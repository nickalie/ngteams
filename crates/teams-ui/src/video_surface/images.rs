//! GPUI's Windows (DirectX) and macOS (Metal) renderers upload BGRA images to their sprite atlas. Element-owned state
//! holds the latest image and a release observer evicts its atlas allocation even when the tile/player disappears
//! without another paint. Repeated draws of one serial reuse the same image, rather than allocating on every layout or
//! repaint.

use super::Outcome;
use gpui::{App, AppContext as _, Bounds, Corners, ElementId, Entity, Pixels, RenderImage, Window};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

pub(crate) struct Surface(u64);
impl Surface {
    pub(crate) fn new() -> Arc<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Arc::new(Self(NEXT.fetch_add(1, Ordering::Relaxed)))
    }
}

type Completion = Box<dyn FnOnce(Outcome) + Send>;
pub(crate) struct Frame {
    surface: Arc<Surface>,
    image: Option<Arc<RenderImage>>,
    complete: Mutex<Option<Completion>>,
}

impl Frame {
    pub(crate) fn image(&self) -> Option<&Arc<RenderImage>> {
        self.image.as_ref()
    }
}

pub(super) fn frame(
    surface: &Arc<Surface>,
    _serial: u64,
    (width, height): (u32, u32),
    pixels: &Arc<Vec<u8>>,
    complete: impl FnOnce(Outcome) + Send + 'static,
) -> Arc<Frame> {
    // RenderImage takes BGRA bytes despite image::RgbaImage's type name; these frames are already opaque BGRA.
    let image = image::RgbaImage::from_raw(width, height, pixels.as_ref().clone())
        .map(|image| Arc::new(RenderImage::new(vec![image::Frame::new(image)])));
    Arc::new(Frame {
        surface: Arc::clone(surface),
        image,
        complete: Mutex::new(Some(Box::new(complete))),
    })
}

struct Upload(Option<Arc<RenderImage>>);
impl Upload {
    fn replace(&mut self, image: &Arc<RenderImage>, window: &mut Window, cx: &mut App) {
        if self.0.as_ref().is_none_or(|previous| previous.id != image.id)
            && let Some(previous) = self.0.replace(Arc::clone(image))
        {
            cx.drop_image(previous, Some(window));
        }
    }

    fn new(cx: &mut App) -> Entity<Self> {
        let entity = cx.new(|_| Self(None));
        cx.observe_release(&entity, |upload, cx| {
            if let Some(image) = upload.0.take() {
                cx.drop_image(image, None);
            }
        })
        .detach();
        entity
    }
}

pub(super) fn paint(frame: &Frame, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    let Some(image) = frame.image() else { return };
    let visible = window.content_mask().bounds.intersect(&bounds);
    if visible.size.width <= gpui::px(0.) || visible.size.height <= gpui::px(0.) {
        return;
    }
    let id = ElementId::NamedInteger("video-surface".into(), frame.surface.0);
    window.with_global_id(id, |id, window| {
        window.with_element_state::<Entity<Upload>, _>(id, |upload, window| {
            let upload = upload.unwrap_or_else(|| Upload::new(cx));
            upload.update(cx, |upload, cx| upload.replace(image, window, cx));
            ((), upload)
        });
    });
    if window
        .paint_image(bounds, bounds, Corners::default(), Arc::clone(image), 0, false)
        .is_ok()
        && let Some(complete) = frame.complete.lock().unwrap_or_else(PoisonError::into_inner).take()
    {
        complete(Outcome::Submitted);
    }
}
