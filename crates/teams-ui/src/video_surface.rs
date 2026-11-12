//! Video painting without exposing platform textures to call tiles or inline players. Linux keeps its persistent
//! external texture and GPU completion (the vendored GPUI's Linux renderer). Windows and macOS use GPUI's BGRA image
//! path (`images.rs`), with one uploaded image per visible surface, evicted on replacement and when the element
//! disappears. They trace submission, not GPU completion: GPUI's image API cannot report the latter. A hidden or failed
//! paint never counts as submitted.

use gpui::{App, Bounds, Pixels, Window};
use std::sync::Arc;

#[cfg(target_os = "linux")]
pub(crate) use gpui::{PixelFrame as Frame, PixelSurface as Surface};
#[cfg(not(target_os = "linux"))]
#[path = "video_surface/images.rs"]
mod images;
#[cfg(not(target_os = "linux"))]
pub(crate) use images::{Frame, Surface};

pub(crate) enum Outcome {
    #[cfg(target_os = "linux")]
    Presented,
    #[cfg(not(target_os = "linux"))]
    Submitted,
}

impl Outcome {
    pub(crate) fn change(&self) -> &'static str {
        match self {
            #[cfg(target_os = "linux")]
            Self::Presented => "presented",
            #[cfg(not(target_os = "linux"))]
            Self::Submitted => "submitted",
        }
    }
}

pub(crate) fn frame(
    surface: &Arc<Surface>,
    serial: u64,
    (width, height): (u32, u32),
    pixels: &Arc<Vec<u8>>,
    complete: impl FnOnce(Outcome) + Send + 'static,
) -> Arc<Frame> {
    #[cfg(target_os = "linux")]
    {
        Arc::new(Frame {
            surface: Arc::clone(surface),
            serial,
            width,
            height,
            pixels: Arc::clone(pixels),
            damage: vec![gpui::PixelRect {
                x: 0,
                y: 0,
                width,
                height,
            }],
            completion: gpui::FrameCompletion::new(move |outcome| {
                if outcome == gpui::FrameOutcome::Presented {
                    complete(Outcome::Presented);
                }
            }),
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        images::frame(surface, serial, (width, height), pixels, complete)
    }
}

pub(crate) fn paint(frame: &Arc<Frame>, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    #[cfg(target_os = "linux")]
    {
        let _ = cx;
        window.paint_external_surface(bounds, gpui::ExternalSurface::Pixels(Arc::clone(frame)));
    }
    #[cfg(not(target_os = "linux"))]
    images::paint(frame, bounds, window, cx);
}

/// Fit a frame inside its bounds without stretching it.
pub(crate) fn fitted(bounds: Bounds<Pixels>, width: u32, height: u32) -> Bounds<Pixels> {
    let side = |pixels: u32| f32::from(u16::try_from(pixels).unwrap_or(u16::MAX)).max(1.0);
    let (box_width, box_height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let scale = (box_width / side(width)).min(box_height / side(height));
    let (drawn_width, drawn_height) = (side(width) * scale, side(height) * scale);
    Bounds {
        origin: gpui::point(
            bounds.origin.x + gpui::px((box_width - drawn_width) / 2.0),
            bounds.origin.y + gpui::px((box_height - drawn_height) / 2.0),
        ),
        size: gpui::size(gpui::px(drawn_width), gpui::px(drawn_height)),
    }
}
