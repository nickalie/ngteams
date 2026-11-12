//! The picture viewer: a picture of a message opened in a window of its own at its full size, as Teams' image viewer
//! shows one (Teams draws it over the app; this app gives it a window, as it gives a call one).
//!
//! - **It opens** on a click on a picture a message draws as its own (`rich_text`'s picture blocks: one pasted into
//!   the message or a forward's, a GIF from the GIF picker; not a link preview's, a quote's or a file's card, which
//!   open what they open). The view the message is in hands it every picture of its messages in the order it shows
//!   them ([`Viewing`]), so Previous and Next go through the chat's pictures. One viewer window at most: a click on
//!   another picture shows it in the open window and brings that to the front.
//! - **The picture** is the bytes the message's picture is drawn from, asked of the account's engine again
//!   ([`PictureEngine::message_picture`]: the engine keeps them for the run and on disk, so nothing is read twice; the
//!   message names the picture's original, `imgo`, or its full-size view), decoded here off the foreground at up to
//!   [`MAX_VIEWER_SIDE`] on its longer side, never by a request of the view's own.
//! - **Fitted** to the stage (the window under its header, less a margin) and never drawn larger than its own size
//!   (one of its pixels to one of the window's), its aspect ratio kept, centred. Zoom in and out by the header's
//!   buttons, `Ctrl+=`/`Ctrl+-` (`+`/`-` too), `Ctrl+` the wheel (about the pointer); `Ctrl+0` fits it again and the
//!   fit button toggles between fitted and its own size (100%), as does a double-click. Larger than the stage, it is
//!   panned by dragging it or by the wheel, never past its edges.
//! - **The header**: the sender's avatar and name and the message's time in full, as Teams' viewer heads a picture;
//!   on the right "n of m" when there are several, the zoom, Download and Close. Previous and Next sit over the
//!   stage's sides (and on Left and Right) while there is one before or after.
//! - **Download** asks where to save it (the desktop's file chooser through its portal, starting in the user's
//!   downloads folder, named after the message's time) and writes its bytes there as they came; without a file
//!   chooser it is saved straight into the downloads folder under a name no file there has. What happened is said in
//!   a note over the stage. No "Copy image": GPUI's Wayland clipboard offers other apps text alone, so a copied picture
//!   would reach none of them.
//! - **Escape** or the window's own close closes it.

use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use chrono::Local;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, FocusHandle, Global, InteractiveElement as _, IntoElement,
    KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _, Pixels, Point, Render, RenderImage,
    ScrollWheelEvent, SharedString, Styled as _, Task, TitlebarOptions, WeakEntity, Window, WindowBounds, WindowHandle,
    WindowOptions, actions, div, img, point, prelude::FluentBuilder as _, px, rgb, size,
};
use gpui_component::Disableable as _;
use gpui_component::Size as AvatarSize;
use gpui_component::button::Button;
use teams_core::PictureBytes;
use teams_proto::PastedFormat;

use crate::clock;
use crate::icons::{Glyph, icon_button_with_key, selected_by};
use crate::inline::{PictureSource, decode_within};
use crate::people::{self, Face};
use crate::theme::{self, Palette};
use crate::window::KitRoot;

actions!(
    picture_viewer,
    [
        CloseViewer,
        ZoomIn,
        ZoomOut,
        ZoomFit,
        PreviousPicture,
        NextPicture,
        DownloadPicture
    ]
);

/// The viewer's key context.
const CONTEXT: &str = "PictureViewer";

/// The longest side a picture is decoded at for the viewer: a larger one is scaled down to it (and drawn at its own
/// size from that), within what every GPU's textures hold.
pub const MAX_VIEWER_SIDE: u32 = 8192;

/// The header's height.
pub const HEADER_HEIGHT: f32 = 56.0;

/// The margin a fitted picture keeps from the stage's edges.
pub const MARGIN: f32 = 24.0;

/// The zoom steps the buttons and keys go through (as a browser's).
const ZOOM_STEPS: [f32; 18] = [
    0.1, 0.2, 0.25, 0.33, 0.5, 0.67, 0.75, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0,
];

/// The least and most a picture is zoomed to.
const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 8.0;

/// How much one notch of the wheel (a line) zooms, with Ctrl held.
const WHEEL_ZOOM: f32 = 1.2;

/// How far a line of the wheel pans.
const WHEEL_LINE: f32 = 40.0;

/// The window's size when it opens, and the smallest it gets.
const DEFAULT_SIZE: (f32, f32) = (1000.0, 700.0);
const MIN_SIZE: (f32, f32) = (420.0, 320.0);

/// How long a note over the stage stays.
const NOTE_FOR: Duration = Duration::from_secs(4);

/// The side of Previous and Next over the stage.
const NAV_SIDE: f32 = 40.0;

/// Sets the viewer's keys up, once per app.
pub fn init(cx: &mut App) {
    if cx.has_global::<Bound>() {
        return;
    }
    cx.set_global(Bound);
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("escape", CloseViewer, context),
        KeyBinding::new("secondary-=", ZoomIn, context),
        KeyBinding::new("secondary-+", ZoomIn, context),
        KeyBinding::new("=", ZoomIn, context),
        KeyBinding::new("+", ZoomIn, context),
        KeyBinding::new("secondary--", ZoomOut, context),
        KeyBinding::new("-", ZoomOut, context),
        KeyBinding::new("secondary-0", ZoomFit, context),
        KeyBinding::new("left", PreviousPicture, context),
        KeyBinding::new("right", NextPicture, context),
        KeyBinding::new("secondary-s", DownloadPicture, context),
    ]);
}

/// The viewer's keys are bound.
struct Bound;

impl Global for Bound {}

/// The app id the viewer's window opens with: the main window's ([`set_app_id`]).
struct AppId(String);

impl Global for AppId {}

/// Opens the viewer's windows with app id `app_id` (the main window's) from now on.
pub fn set_app_id(app_id: &str, cx: &mut App) {
    cx.set_global(AppId(app_id.to_owned()));
}

/// What a click on a message's picture calls with the picture's `src`: the view the message is in opening the viewer
/// on it ([`open`]).
#[derive(Clone)]
pub struct OpenPicture(Rc<OpenFn>);

/// What [`OpenPicture`] calls.
type OpenFn = dyn Fn(&str, &mut Window, &mut App);

impl OpenPicture {
    #[must_use]
    pub fn new(open: impl Fn(&str, &mut Window, &mut App) + 'static) -> Self {
        Self(Rc::new(open))
    }

    /// Opens the picture `src` names.
    pub fn open(&self, src: &str, window: &mut Window, cx: &mut App) {
        (self.0)(src, window, cx);
    }
}

impl fmt::Debug for OpenPicture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OpenPicture")
    }
}

/// One picture the viewer goes through: what names it, and the message it is in.
#[derive(Clone, Debug)]
pub struct Viewed {
    /// The message's `src` for it.
    pub src: String,
    /// The message's id.
    pub message: String,
    /// Its sender's name, and what their avatar shows.
    pub sender: SharedString,
    pub face: Face,
    /// The message's arrival time, ms.
    pub time: i64,
}

/// What the viewer shows: the pictures of a view's messages, in its order, the one at `at` first; each asked of
/// `source`'s engine.
#[derive(Clone, Debug)]
pub struct Viewing {
    pub source: PictureSource,
    pub pictures: Vec<Viewed>,
    pub at: usize,
}

/// The viewer's window, while it is open.
struct Open {
    window: WindowHandle<KitRoot>,
    viewer: WeakEntity<PictureViewer>,
}

impl Global for Open {}

/// Shows `viewing` in the viewer, once the update under way is over: in its open window, brought to the front, else
/// in a new one (centred, the keyboard on it). Nothing when it holds no picture.
pub fn open(viewing: Viewing, cx: &mut App) {
    if viewing.pictures.is_empty() {
        return;
    }
    // not inside the click's window's update: the viewer's window is another
    cx.defer(move |cx| open_now(viewing, cx));
}

fn open_now(viewing: Viewing, cx: &mut App) {
    if let Some((window, viewer)) = alive(cx) {
        viewer.update(cx, |viewer, cx| viewer.show(viewing, cx));
        let _ = window.update(cx, |_, window, cx| {
            window.activate_window();
            let focus = viewer.read(cx).focus.clone();
            focus.focus(window, cx);
        });
        return;
    }
    let options = options(cx);
    let mut made = None;
    let opened = cx.open_window(options, |window, cx| {
        let viewer = cx.new(|cx| PictureViewer::new(viewing, cx));
        let focus = viewer.read(cx).focus.clone();
        focus.focus(window, cx);
        made = Some(viewer.downgrade());
        cx.new(|cx| KitRoot::new(viewer, window, cx))
    });
    if let (Ok(window), Some(viewer)) = (opened, made) {
        cx.set_global(Open { window, viewer });
    }
}

/// The viewer, while its window is open.
#[must_use]
pub fn shown(cx: &App) -> Option<gpui::Entity<PictureViewer>> {
    alive(cx).map(|(_, viewer)| viewer)
}

/// The viewer's window, while it is open.
#[must_use]
pub fn window(cx: &App) -> Option<WindowHandle<KitRoot>> {
    alive(cx).map(|(window, _)| window)
}

/// The viewer's window and the viewer in it, while the window is open (the app still lists it).
fn alive(cx: &App) -> Option<(WindowHandle<KitRoot>, gpui::Entity<PictureViewer>)> {
    let open = cx.try_global::<Open>()?;
    let id = open.window.window_id();
    if !cx.windows().iter().any(|window| window.window_id() == id) {
        return None;
    }
    Some((open.window, open.viewer.upgrade()?))
}

/// Closes the viewer's window, if it is open.
pub fn close(cx: &mut App) {
    let Some((window, _)) = alive(cx) else { return };
    cx.defer(move |cx| {
        // a window gone already has nothing to take away
        let _ = window.update(cx, |_, window, _| window.remove_window());
    });
}

/// How the window opens: centred, titled "Image", with the main window's app id.
fn options(cx: &App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(DEFAULT_SIZE.0), px(DEFAULT_SIZE.1)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Image".into()),
            ..TitlebarOptions::default()
        }),
        app_id: cx.try_global::<AppId>().map(|id| id.0.clone()),
        window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
        ..WindowOptions::default()
    }
}

/// How the picture shown stands.
enum Loaded {
    Reading,
    /// Decoded: its frames, its own size and its bytes as they came (what Download saves).
    Shown(Arc<RenderImage>, (u32, u32), PictureBytes),
    /// None to show: the engine has none, or it doesn't decode.
    Missing,
}

/// How the picture sits on the stage: its zoom (`None`: fitted) and how far its centre is moved from the stage's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub zoom: Option<f32>,
    pub pan: Point<Pixels>,
}

impl Default for Placement {
    fn default() -> Self {
        Self {
            zoom: None,
            pan: point(px(0.0), px(0.0)),
        }
    }
}

/// A picture's side as the stage measures it.
#[expect(clippy::cast_precision_loss, reason = "a picture's side is at most 100 000 pixels")]
fn side(pixels: u32) -> f32 {
    pixels.max(1) as f32
}

impl Placement {
    /// The scale a picture of `natural` pixels is fitted at on `stage`: within the stage less its margin, never above
    /// its own size.
    #[must_use]
    pub fn fit(natural: (u32, u32), stage: gpui::Size<Pixels>) -> f32 {
        let room_width = (f32::from(stage.width) - 2.0 * MARGIN).max(1.0);
        let room_height = (f32::from(stage.height) - 2.0 * MARGIN).max(1.0);
        (room_width / side(natural.0))
            .min(room_height / side(natural.1))
            .min(1.0)
    }

    /// The scale it draws a picture of `natural` pixels at on `stage`.
    #[must_use]
    pub fn scale(&self, natural: (u32, u32), stage: gpui::Size<Pixels>) -> f32 {
        self.zoom.unwrap_or_else(|| Self::fit(natural, stage))
    }

    /// The picture's bounds on `stage` (from the stage's top left corner).
    #[must_use]
    pub fn bounds(&self, natural: (u32, u32), stage: gpui::Size<Pixels>) -> Bounds<Pixels> {
        let scale = self.scale(natural, stage);
        let drawn = size(px(side(natural.0) * scale), px(side(natural.1) * scale));
        let pan = clamped(self.pan, drawn, stage);
        let origin = point(
            (stage.width - drawn.width) / 2.0 + pan.x,
            (stage.height - drawn.height) / 2.0 + pan.y,
        );
        Bounds::new(origin, drawn)
    }

    /// Whether the picture is larger than `stage` at its scale: it can be panned.
    #[must_use]
    pub fn pans(&self, natural: (u32, u32), stage: gpui::Size<Pixels>) -> bool {
        let drawn = self.bounds(natural, stage).size;
        drawn.width > stage.width || drawn.height > stage.height
    }

    /// It zoomed to `zoom` (within [`MIN_ZOOM`] and [`MAX_ZOOM`]), the picture's point at `anchor` (from the stage's
    /// centre) staying where it is.
    #[must_use]
    pub fn zoomed(self, zoom: f32, anchor: Point<Pixels>, natural: (u32, u32), stage: gpui::Size<Pixels>) -> Self {
        let old = self.scale(natural, stage);
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let pan = clamped(self.pan, self.bounds(natural, stage).size, stage);
        let ratio = zoom / old;
        let pan = point(
            anchor.x - (anchor.x - pan.x) * ratio,
            anchor.y - (anchor.y - pan.y) * ratio,
        );
        let zoomed = Self { zoom: Some(zoom), pan };
        let drawn = zoomed.bounds(natural, stage).size;
        Self {
            zoom: Some(zoom),
            pan: clamped(pan, drawn, stage),
        }
    }

    /// It moved by `by`, never past the picture's edges.
    #[must_use]
    pub fn panned(self, by: Point<Pixels>, natural: (u32, u32), stage: gpui::Size<Pixels>) -> Self {
        let drawn = self.bounds(natural, stage).size;
        Self {
            zoom: self.zoom,
            pan: clamped(self.pan + by, drawn, stage),
        }
    }
}

/// `pan` kept so that a picture of `drawn` size never leaves an edge of `stage` bare it could cover (none along a side
/// it is smaller on).
fn clamped(pan: Point<Pixels>, drawn: gpui::Size<Pixels>, stage: gpui::Size<Pixels>) -> Point<Pixels> {
    let limit = |drawn: Pixels, stage: Pixels| ((drawn - stage) / 2.0).max(px(0.0));
    let (x, y) = (limit(drawn.width, stage.width), limit(drawn.height, stage.height));
    point(pan.x.clamp(-x, x), pan.y.clamp(-y, y))
}

/// The next zoom step past `scale`: up when `up`, else down; `scale` itself past the last.
fn step(scale: f32, up: bool) -> f32 {
    if up {
        ZOOM_STEPS
            .iter()
            .copied()
            .find(|step| *step > scale * 1.001)
            .unwrap_or(MAX_ZOOM.min(scale * 1.25))
    } else {
        ZOOM_STEPS
            .iter()
            .rev()
            .copied()
            .find(|step| *step < scale * 0.999)
            .unwrap_or(MIN_ZOOM.max(scale / 1.25))
    }
}

/// The viewer (see the module docs): the root view of its window.
pub struct PictureViewer {
    viewing: Viewing,
    loaded: Loaded,
    placement: Placement,
    /// While the picture is dragged: where the pointer was pressed, and the placement then.
    drag: Option<(Point<Pixels>, Placement)>,
    /// What the last Download did, while it is said.
    note: Option<(SharedString, bool)>,
    /// Counts the pictures shown, so a read of one shown before lands nowhere.
    generation: u64,
    focus: FocusHandle,
    /// The picture being read, dropped (and so stopped) when another is shown.
    reading: Option<Task<()>>,
    /// What takes the note away again.
    note_timer: Option<Task<()>>,
}

impl PictureViewer {
    fn new(viewing: Viewing, cx: &mut Context<Self>) -> Self {
        let mut viewer = Self {
            viewing,
            loaded: Loaded::Reading,
            placement: Placement::default(),
            drag: None,
            note: None,
            generation: 0,
            focus: cx.focus_handle(),
            reading: None,
            note_timer: None,
        };
        viewer.read(cx);
        viewer
    }

    /// Shows `viewing` instead.
    pub fn show(&mut self, viewing: Viewing, cx: &mut Context<Self>) {
        self.viewing = viewing;
        self.go_to(self.viewing.at, cx);
    }

    /// The picture shown: its index among the pictures, and how many there are.
    #[must_use]
    pub fn at(&self) -> (usize, usize) {
        (self.viewing.at, self.viewing.pictures.len())
    }

    /// The picture shown's own size, once it was read.
    #[must_use]
    pub fn natural(&self) -> Option<(u32, u32)> {
        match &self.loaded {
            Loaded::Shown(_, natural, _) => Some(*natural),
            Loaded::Reading | Loaded::Missing => None,
        }
    }

    /// How the picture sits on the stage.
    #[must_use]
    pub fn placement(&self) -> Placement {
        self.placement
    }

    fn current(&self) -> Option<&Viewed> {
        self.viewing.pictures.get(self.viewing.at)
    }

    /// Shows the picture at `at`, fitted, read anew.
    fn go_to(&mut self, at: usize, cx: &mut Context<Self>) {
        if at >= self.viewing.pictures.len() {
            return;
        }
        self.viewing.at = at;
        self.placement = Placement::default();
        self.drag = None;
        if let Loaded::Shown(image, ..) = std::mem::replace(&mut self.loaded, Loaded::Reading) {
            // its frames leave every window's atlas: the next picture takes their place
            cx.drop_image(image, None);
        }
        self.read(cx);
        cx.notify();
    }

    /// Asks the engine for the picture shown and decodes it off the foreground.
    fn read(&mut self, cx: &mut Context<Self>) {
        let Some(src) = self.current().map(|current| current.src.clone()) else {
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        let reply = self.viewing.source.engine().message_picture(src);
        self.reading = Some(cx.spawn(async move |this, cx| {
            let loaded = match reply.await {
                Ok(Some(bytes)) => cx
                    .background_executor()
                    .spawn(async move {
                        let decoded = decode_within(&bytes, MAX_VIEWER_SIDE);
                        decoded.map(|(image, natural)| (Arc::new(image), natural, bytes))
                    })
                    .await
                    .map_or(Loaded::Missing, |(image, natural, bytes)| {
                        Loaded::Shown(image, natural, bytes)
                    }),
                Ok(None) | Err(_) => Loaded::Missing,
            };
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.loaded = loaded;
                    cx.notify();
                }
            });
        }));
    }

    /// The previous picture, when there is one.
    pub fn previous(&mut self, cx: &mut Context<Self>) {
        if let Some(at) = self.viewing.at.checked_sub(1) {
            self.go_to(at, cx);
        }
    }

    /// The next picture, when there is one.
    pub fn next(&mut self, cx: &mut Context<Self>) {
        self.go_to(self.viewing.at + 1, cx);
    }

    /// One zoom step in (`up`) or out, about the stage's centre.
    pub fn zoom_step(&mut self, up: bool, window: &Window, cx: &mut Context<Self>) {
        let Some(natural) = self.natural() else { return };
        let stage = stage(window);
        let to = step(self.placement.scale(natural, stage), up);
        self.placement = self.placement.zoomed(to, point(px(0.0), px(0.0)), natural, stage);
        cx.notify();
    }

    /// Fitted to the stage again.
    pub fn fit(&mut self, cx: &mut Context<Self>) {
        self.placement = Placement::default();
        cx.notify();
    }

    /// Fitted when it isn't, else at its own size (about `anchor`, from the stage's centre).
    fn toggle_fit(&mut self, anchor: Point<Pixels>, window: &Window, cx: &mut Context<Self>) {
        let Some(natural) = self.natural() else { return };
        let stage = stage(window);
        let fitted = Placement::fit(natural, stage);
        let scale = self.placement.scale(natural, stage);
        if self.placement.zoom.is_none() && fitted < 1.0 {
            self.placement = self.placement.zoomed(1.0, anchor, natural, stage);
        } else if (scale - fitted).abs() > f32::EPSILON || self.placement.zoom.is_some() {
            self.placement = Placement::default();
        }
        cx.notify();
    }

    /// The wheel: with Ctrl it zooms about the pointer; else it pans a picture larger than the stage.
    fn wheel(&mut self, event: &ScrollWheelEvent, window: &Window, cx: &mut Context<Self>) {
        let Some(natural) = self.natural() else { return };
        let stage = stage(window);
        let delta = event.delta.pixel_delta(px(WHEEL_LINE));
        if event.modifiers.secondary() {
            let scale = self.placement.scale(natural, stage);
            let to = scale * WHEEL_ZOOM.powf(f32::from(delta.y) / WHEEL_LINE);
            let anchor = from_centre(event.position, window);
            self.placement = self.placement.zoomed(to, anchor, natural, stage);
        } else if self.placement.pans(natural, stage) {
            self.placement = self.placement.panned(delta, natural, stage);
        } else {
            return;
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn press(&mut self, event: &MouseDownEvent, window: &Window, cx: &mut Context<Self>) {
        if event.click_count == 2 {
            self.toggle_fit(from_centre(event.position, window), window, cx);
            return;
        }
        if self
            .natural()
            .is_some_and(|natural| self.placement.pans(natural, stage(window)))
        {
            self.drag = Some((event.position, self.placement));
        }
    }

    fn moved(&mut self, event: &MouseMoveEvent, window: &Window, cx: &mut Context<Self>) {
        let (Some((from, then)), Some(natural)) = (self.drag, self.natural()) else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        self.placement = then.panned(event.position - from, natural, stage(window));
        cx.notify();
    }

    /// Download: asks where to save the picture's bytes (see the module docs) and saves them there.
    pub fn download(&mut self, cx: &mut Context<Self>) {
        let (Loaded::Shown(_, _, bytes), Some(current)) = (&self.loaded, self.current()) else {
            return;
        };
        let bytes = Arc::clone(bytes);
        let name = file_name(current.time, bytes.format);
        let folder = match downloads() {
            Ok(folder) => folder,
            Err(error) => {
                self.say((format!("Couldn't open Downloads: {error}").into(), false), cx);
                return;
            }
        };
        let chosen = cx.prompt_for_new_path(&folder, Some(&name));
        cx.spawn(async move |this, cx| {
            let target = match chosen.await {
                Ok(Ok(Some(path))) => Some(path),
                // cancelled
                Ok(Ok(None)) => None,
                // no file chooser: straight into the downloads folder
                Ok(Err(_)) | Err(_) => Some(free_name(&folder, &name)),
            };
            let Some(target) = target else { return };
            let written = cx
                .background_executor()
                .spawn({
                    let target = target.clone();
                    async move { write_picture(&target, &bytes.bytes) }
                })
                .await;
            let shown = target.file_name().map_or_else(
                || target.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            let note = match written {
                Ok(()) => (SharedString::from(format!("Saved as {shown}")), true),
                Err(error) => (SharedString::from(format!("Couldn't save {shown}: {error}")), false),
            };
            let _ = this.update(cx, |this, cx| this.say(note, cx));
        })
        .detach();
    }

    /// Says `note` over the stage for a while.
    fn say(&mut self, note: (SharedString, bool), cx: &mut Context<Self>) {
        self.note = Some(note);
        self.note_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTE_FOR).await;
            let _ = this.update(cx, |this, cx| {
                this.note = None;
                cx.notify();
            });
        }));
        cx.notify();
    }
}

/// The stage's size in `window`: the window under the header.
fn stage(window: &Window) -> gpui::Size<Pixels> {
    let viewport = window.viewport_size();
    size(viewport.width, (viewport.height - px(HEADER_HEIGHT)).max(px(1.0)))
}

/// `position` in `window` from the stage's centre.
fn from_centre(position: Point<Pixels>, window: &Window) -> Point<Pixels> {
    let stage = stage(window);
    point(
        position.x - stage.width / 2.0,
        position.y - px(HEADER_HEIGHT) - stage.height / 2.0,
    )
}

/// The name a picture is saved under: the message's time, as a camera names its pictures, and its format's extension.
#[must_use]
pub fn file_name(time: i64, format: PastedFormat) -> String {
    let extension = match format {
        PastedFormat::Png => "png",
        PastedFormat::Jpeg => "jpg",
        PastedFormat::Gif => "gif",
        PastedFormat::Webp => "webp",
    };
    let at = chrono::DateTime::from_timestamp_millis(time)
        .map(|at| at.with_timezone(&Local).format("%Y-%m-%d %H.%M.%S").to_string())
        .unwrap_or_default();
    format!("Image {at}.{extension}")
}

/// The user's downloads folder, including a redirected Windows known folder.
///
/// # Errors
/// Windows could not resolve the current user's native Downloads folder.
#[cfg(windows)]
pub(crate) fn downloads() -> std::io::Result<PathBuf> {
    dirs::download_dir()
        .filter(|path| path.is_absolute())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "Windows Downloads folder is unavailable"))
}

/// `XDG_DOWNLOAD_DIR` (from the environment, else `user-dirs.dirs`), else `~/Downloads`.
#[cfg(not(windows))]
#[expect(clippy::unnecessary_wraps, reason = "the native Windows folder lookup can fail")]
pub(crate) fn downloads() -> std::io::Result<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if let Some(dir) = std::env::var_os("XDG_DOWNLOAD_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let config = std::env::var_os("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
    Ok(std::fs::read_to_string(config.join("user-dirs.dirs"))
        .ok()
        .and_then(|dirs| user_dir(&dirs, "XDG_DOWNLOAD_DIR", &home))
        .unwrap_or_else(|| home.join("Downloads")))
}

/// The folder `user-dirs.dirs` (its text `dirs`) names by `key`, `$HOME` in it standing for `home`.
#[must_use]
pub fn user_dir(dirs: &str, key: &str, home: &Path) -> Option<PathBuf> {
    let line = dirs
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(key)?.trim_start().strip_prefix('='))?;
    let value = line.trim().trim_matches('"');
    let path = match value.strip_prefix("$HOME") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(value),
    };
    path.is_absolute().then_some(path)
}

/// `name` in `folder`, or with " (n)" before its extension when a file has it.
pub(crate) fn free_name(folder: &Path, name: &str) -> PathBuf {
    let first = folder.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, extension) = name.rsplit_once('.').unwrap_or((name, ""));
    (1..1000)
        .map(|n| folder.join(format!("{stem} ({n}).{extension}")))
        .find(|path| !path.exists())
        .unwrap_or(first)
}

/// Writes `bytes` to `path`, its folder made first.
fn write_picture(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    std::fs::write(path, bytes)
}

impl Render for PictureViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let header = self.header(palette, window, cx);
        let stage = self.stage(palette, window, cx);
        div()
            .debug_selector(|| "picture-viewer".to_owned())
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &CloseViewer, window, _| window.remove_window()))
            .on_action(cx.listener(|this, _: &ZoomIn, window, cx| this.zoom_step(true, window, cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, window, cx| this.zoom_step(false, window, cx)))
            .on_action(cx.listener(|this, _: &ZoomFit, _, cx| this.fit(cx)))
            .on_action(cx.listener(|this, _: &PreviousPicture, _, cx| this.previous(cx)))
            .on_action(cx.listener(|this, _: &NextPicture, _, cx| this.next(cx)))
            .on_action(cx.listener(|this, _: &DownloadPicture, _, cx| this.download(cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background))
            .text_color(rgb(palette.foreground))
            .child(header)
            .child(stage)
    }
}

impl PictureViewer {
    /// The header: who sent it and when on the left; where it is among the pictures, the zoom, Download and Close on
    /// the right.
    fn header(&self, palette: Palette, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let who = self.current().map(|current| {
            let time = clock::full_time(current.time, &Local, clock::cycle(cx));
            div()
                .debug_selector(|| "picture-viewer-sender".to_owned())
                .flex()
                .items_center()
                .gap_3()
                .min_w_0()
                .child(
                    people::avatar(
                        current.face.named.then_some(&current.sender),
                        current.face.picture.as_ref(),
                        AvatarSize::Medium,
                    )
                    .size(px(32.0)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .truncate()
                                .child(current.sender.clone()),
                        )
                        .child(div().text_xs().text_color(rgb(palette.muted_foreground)).child(time)),
                )
        });
        div()
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .px_4()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .relative()
            .bg(rgb(palette.surface))
            .child(div().flex_1().min_w_0().children(who))
            .child(self.actions(palette, window, cx))
            // its foot in the brand gradient, where a border would be
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(2.0))
                    .bg(theme::brand_gradient(palette, 90.0)),
            )
            .into_any_element()
    }

    /// The header's right: "n of m", zoom out, the zoom, zoom in, fit or its own size, Download, Close.
    fn actions(&self, palette: Palette, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let (at, of) = self.at();
        let natural = self.natural();
        let stage = stage(window);
        let scale = natural.map(|natural| self.placement.scale(natural, stage));
        let fitted = natural.map(|natural| Placement::fit(natural, stage));
        let counter = (of > 1).then(|| {
            div()
                .debug_selector(|| "picture-viewer-counter".to_owned())
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .mr_2()
                .child(format!("{} of {of}", at + 1))
        });
        let percent = div()
            .debug_selector(|| "picture-viewer-zoom".to_owned())
            .w(px(52.0))
            .text_center()
            .text_sm()
            .child(scale.map_or_else(String::new, |scale| format!("{:.0}%", scale * 100.0)));
        // at its own size, or fitted when it is smaller than the stage: the one that changes something
        let (fit_glyph, fit_tip) = if self.placement.zoom.is_none() {
            (Glyph::EXPAND, "Actual size")
        } else {
            (Glyph::SHRINK, "Fit to window")
        };
        let fit_does = match (self.placement.zoom, fitted) {
            (None, Some(fitted)) => fitted < 1.0,
            (Some(_), Some(_)) => true,
            (_, None) => false,
        };
        let shown = natural.is_some();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .children(counter)
            .child(Self::button(
                ("picture-zoom-out", Glyph::ZOOM_OUT, "Zoom out", &ZoomOut),
                shown,
                cx,
            ))
            .child(percent)
            .child(Self::button(
                ("picture-zoom-in", Glyph::ZOOM_IN, "Zoom in", &ZoomIn),
                shown,
                cx,
            ))
            .child(Self::button(
                ("picture-fit", fit_glyph, fit_tip, &ZoomFit),
                fit_does,
                cx,
            ))
            .child(div().w(px(1.0)).h(px(20.0)).mx_2().bg(rgb(palette.border)))
            .child(Self::button(
                ("picture-download", Glyph::DOWNLOAD, "Download", &DownloadPicture),
                shown,
                cx,
            ))
            .child(Self::button(
                ("picture-close", Glyph::X, "Close", &CloseViewer),
                true,
                cx,
            ))
            .into_any_element()
    }

    /// A header button, `enabled` or not: its click dispatches its action, as its key does.
    fn button(
        (id, glyph, tip, action): (&'static str, Glyph, &'static str, &dyn gpui::Action),
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let boxed = action.boxed_clone();
        let fit = id == "picture-fit";
        let button: Button = icon_button_with_key(id, glyph, tip, action)
            .disabled(!enabled)
            .on_click(cx.listener(move |this, _, window, cx| {
                if fit {
                    // the button toggles; Ctrl+0 only fits
                    this.toggle_fit(point(px(0.0), px(0.0)), window, cx);
                } else {
                    window.dispatch_action(boxed.boxed_clone(), cx);
                }
            }));
        selected_by(id, button).into_any_element()
    }

    /// The stage: the picture (or what stands in for it), Previous and Next over its sides, the note over its foot.
    fn stage(&self, palette: Palette, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let stage = stage(window);
        let picture = match &self.loaded {
            Loaded::Shown(image, natural, _) => {
                let bounds = self.placement.bounds(*natural, stage);
                let pans = self.placement.pans(*natural, stage);
                // an id of its own, for a GIF to play
                let image = img(Arc::clone(image))
                    .id(("picture-viewer-image", self.generation))
                    .debug_selector(|| "picture-viewer-image".to_owned())
                    .absolute()
                    .left(bounds.origin.x)
                    .top(bounds.origin.y)
                    .w(bounds.size.width)
                    .h(bounds.size.height)
                    .when(pans, gpui::Styled::cursor_grab);
                // another picture fades in where it is placed
                crate::motion::fade(image, ("picture-viewer-in", self.generation), cx)
            }
            Loaded::Reading => standing_in(Glyph::LOADING, None, palette),
            Loaded::Missing => standing_in(Glyph::FILE_IMAGE, Some("This image can't be shown."), palette),
        };
        div()
            .id("picture-viewer-stage")
            .debug_selector(|| "picture-viewer-stage".to_owned())
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| this.wheel(event, window, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| this.press(event, window, cx)),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| this.moved(event, window, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.drag = None))
            .child(picture)
            .children(self.navigation(palette, cx))
            .children(self.note.as_ref().map(|(note, fine)| note_card(note, *fine, palette)))
            .into_any_element()
    }

    /// Previous and Next, over the stage's sides, each while there is a picture that way.
    fn navigation(&self, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let (at, of) = self.at();
        let previous = (at > 0).then(|| {
            nav_button(
                ("picture-previous", Glyph::CHEVRON_LEFT, "Previous image"),
                &PreviousPicture,
                palette,
                cx,
            )
            .left_4()
            .into_any_element()
        });
        let next = (at + 1 < of).then(|| {
            nav_button(
                ("picture-next", Glyph::CHEVRON_RIGHT, "Next image"),
                &NextPicture,
                palette,
                cx,
            )
            .right_4()
            .into_any_element()
        });
        previous.into_iter().chain(next).collect()
    }
}

/// Previous or Next: a round button over the stage, half way down.
fn nav_button(
    (id, glyph, tip): (&'static str, Glyph, &'static str),
    action: &dyn gpui::Action,
    palette: Palette,
    cx: &mut Context<PictureViewer>,
) -> gpui::Div {
    let boxed = action.boxed_clone();
    let button = icon_button_with_key(id, glyph, tip, action).on_click(cx.listener(move |_, _, window, cx| {
        window.dispatch_action(boxed.boxed_clone(), cx);
    }));
    div()
        .debug_selector(move || id.to_owned())
        .absolute()
        .top(gpui::relative(0.5))
        .mt(px(-NAV_SIDE / 2.0))
        .size(px(NAV_SIDE))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(rgb(palette.surface))
        .border_1()
        .border_color(rgb(palette.border))
        .shadow_md()
        // a press on it isn't one on the picture
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(button)
}

/// What stands in for the picture while it is read, or when there is none: its icon in a tinted circle, and a line.
fn standing_in(glyph: Glyph, line: Option<&'static str>, palette: Palette) -> AnyElement {
    div()
        .debug_selector(|| "picture-viewer-standing-in".to_owned())
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .text_color(rgb(palette.muted_foreground))
        .child(
            div()
                .size(px(56.0))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgb(palette.hover))
                .child(glyph.icon().size(px(24.0))),
        )
        .children(line.map(|line| div().text_sm().child(line)))
        .into_any_element()
}

/// The note over the stage's foot: what Download did.
fn note_card(note: &SharedString, fine: bool, palette: Palette) -> AnyElement {
    let (glyph, colour) = if fine {
        (Glyph::CIRCLE_CHECK, theme::SUCCESS)
    } else {
        (Glyph::CIRCLE_ALERT, theme::DANGER)
    };
    div()
        .absolute()
        .bottom_4()
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            div()
                .debug_selector(|| "picture-viewer-note".to_owned())
                .max_w(px(560.0))
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded_lg()
                .bg(rgb(palette.surface))
                .border_1()
                .border_color(rgb(palette.border))
                .shadow_lg()
                .text_sm()
                .child(div().text_color(rgb(colour)).child(glyph.icon().size(px(16.0))))
                .child(div().truncate().child(note.clone())),
        )
        .into_any_element()
}

#[cfg(all(test, windows))]
mod native_downloads {
    /// Windows resolves its known Downloads folder even without HOME and with an unrelated XDG override.
    #[cfg(windows)]
    #[test]
    fn windows_downloads_uses_the_shell_folder_without_unix_homes() {
        use windows::Win32::System::Com::CoTaskMemFree;
        use windows::Win32::UI::Shell::{FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

        const CHILD: &str = "NGTEAMS_TEST_NATIVE_DOWNLOADS";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "picture_viewer::native_downloads::windows_downloads_uses_the_shell_folder_without_unix_homes",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env_remove("HOME")
                .env("XDG_DOWNLOAD_DIR", "relative-unix-override")
                .output()
                .unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return;
        }
        // SAFETY: this reads the current user's shell configuration and returns an owned COM allocation.
        let native = unsafe { SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None) }.unwrap();
        // SAFETY: SHGetKnownFolderPath returned a live NUL-terminated string.
        let expected = std::path::PathBuf::from(unsafe { native.to_string() }.unwrap());
        // SAFETY: release the API's allocation exactly once, after copying it.
        unsafe { CoTaskMemFree(Some(native.0.cast())) };
        let folder = super::downloads().unwrap();
        assert!(folder.is_absolute());
        assert_eq!(folder, expected);
    }
}
