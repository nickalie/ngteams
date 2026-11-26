//! The sign-in window's view: the page's frames drawn and the window's input handed to the engine (the element of
//! ngterm's HTML overlay, cut down to what a sign-in page needs), the page's questions answered by its [`Page`].
//!
//! - Frames come as damaged rectangles of shared memory ([`teams_wpe_client::FrameContent::Shm`]): they are copied into the
//!   page's whole picture, drawn through GPUI's external surface, and the engine is told the frame was shown once the
//!   renderer is done with it (whatever its outcome), so it renders the next one.
//! - The content's size and the window's scale go to the engine whenever they change; it renders at their product.
//! - Pointer presses, releases, motion and the wheel go to the engine in the content's coordinates; keys while the view
//!   holds the keyboard (it takes it when the window opens and on every press). A paste key hands the desktop's text
//!   to the engine first; a copy in the page puts its text on the desktop's clipboard. A `<select>` popup closes
//!   unchanged (the sign-in pages offer none to choose from), the context menu isn't offered.
//! - The answer ends the run once: the engine is shut down, the window closed, and the sign-in told.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ExternalSurface, FocusHandle, FrameCompletion,
    InteractiveElement as _, IntoElement, KeyDownEvent, KeyUpEvent, Keystroke, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, PixelFrame, PixelRect, PixelSurface, Pixels, Point, Render, ScrollEvent,
    Styled as _, Window, canvas, div, rgb,
};
use teams_auth::signin::SignInAnswer;
use teams_wpe_client::{
    Event, Frame, FrameContent, Geometry, Host, Input, Key, LoadErrorKind, LoadEvent, Modifiers, Pointer, PointerKind,
    PolicyKind, PolicyRequest, Scroll, ViewId,
};

use crate::input;
use crate::window::{Done, Outcome, PUMP, Page, WEB_PROCESS_ENDED};

/// The page's picture so far: every pixel, premultiplied BGRA.
struct Picture {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// The sign-in window's root view.
pub(crate) struct SignInView {
    /// The page and who gets the answer, until it is given.
    run: Option<(Page, Done)>,
    focus: FocusHandle,
    surface: Arc<PixelSurface>,
    picture: Picture,
    serial: u64,
    frame: Option<Arc<PixelFrame>>,
    /// The content's bounds as last laid out.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The geometry last sent to the engine (width, height, scale bits).
    sent: Rc<Cell<Option<(u32, u32, u64)>>>,
    cursor: CursorStyle,
    /// The pointer buttons held, as the engine's bits.
    buttons: Modifiers,
    /// Why the page shows nothing (a load that failed).
    problem: Option<String>,
}

impl SignInView {
    /// Hands each of the engine's events to the view, until the engine ends (the run then fails, unless answered).
    fn follow_events(events: async_channel::Receiver<Event>, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |view, cx| {
            while let Ok(event) = events.recv().await {
                let followed = view.update_in(cx, |view, window, cx| view.event(event, window, cx));
                if followed.is_err() {
                    return;
                }
            }
            let _ = view.update_in(cx, |view, window, cx| {
                view.finish(SignInAnswer::Failed("the page's engine ended".into()), window, cx);
            });
        })
        .detach();
    }

    /// Runs the view's [`SignInView::pump`] every [`PUMP`] while it asks to go on.
    fn pump_on(window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |view, cx| {
            loop {
                cx.background_executor().timer(PUMP).await;
                match view.update_in(cx, Self::pump) {
                    Ok(true) => {}
                    Ok(false) | Err(_) => return,
                }
            }
        })
        .detach();
    }

    pub(crate) fn new(run: Option<(Page, Done)>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        if let Some((page, _)) = &run {
            Self::follow_events(page.events.clone(), window, cx);
            Self::pump_on(window, cx);
        }
        let closing = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = closing.update(cx, |view, cx| {
                if let Some((page, _)) = view.run.as_mut() {
                    let answer = page.closed();
                    view.answer(answer, cx);
                }
            });
            true
        });
        Self {
            run,
            focus,
            surface: PixelSurface::new(),
            picture: Picture {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            },
            serial: 0,
            frame: None,
            bounds: Rc::new(Cell::new(None)),
            sent: Rc::new(Cell::new(None)),
            cursor: CursorStyle::Arrow,
            buttons: Modifiers::empty(),
            problem: None,
        }
    }

    fn engine(&self) -> Option<(Arc<Host>, ViewId)> {
        self.run.as_ref().map(|(page, _)| (Arc::clone(&page.host), page.view))
    }

    /// Reads the guard's events; `false` once the run is over.
    fn pump(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some((page, _)) = self.run.as_mut() else {
            return false;
        };
        let outcome = page.pump();
        self.outcome(outcome, window, cx);
        self.run.is_some()
    }

    fn outcome(&mut self, outcome: Outcome, window: &mut Window, cx: &mut Context<Self>) {
        if let Outcome::Finish(answer) = outcome {
            self.finish(answer, window, cx);
        }
    }

    /// Ends the run with `answer` and closes the window.
    fn finish(&mut self, answer: SignInAnswer, window: &mut Window, cx: &mut Context<Self>) {
        if self.answer(answer, cx) {
            window.remove_window();
        }
    }

    /// Gives `answer` once: the engine is shut down and the sign-in told. `false` when it was given already.
    fn answer(&mut self, answer: SignInAnswer, cx: &mut Context<Self>) -> bool {
        let Some((page, done)) = self.run.take() else {
            return false;
        };
        self.frame = None;
        page.shut_down(cx);
        done(answer, cx);
        true
    }

    fn event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        let Some((page, _)) = self.run.as_mut() else {
            return;
        };
        match event {
            Event::ViewCreated { page_id, .. } => page.created(page_id),
            Event::Policy(PolicyRequest { token, kind, .. }) => {
                let (allow, outcome) = match kind {
                    PolicyKind::Navigation {
                        uri,
                        method,
                        new_window,
                        ..
                    } => page.navigation(&uri, method.as_deref(), new_window),
                    PolicyKind::Response { mime_supported, .. } => (Page::response(mime_supported), Outcome::Continue),
                };
                page.host.decide_policy(token, allow);
                self.outcome(outcome, window, cx);
            }
            Event::Frame(frame) => self.frame(frame, cx),
            Event::Cursor { name, .. } => {
                self.cursor = input::cursor(&name);
                cx.notify();
            }
            Event::Clipboard { text, .. } => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            Event::OptionMenu(menu) => page.host.choose_option(menu.token, None),
            Event::LoadFailed { error, .. } if error.kind == LoadErrorKind::Failed => {
                self.problem = Some(error.message);
                cx.notify();
            }
            Event::Load {
                event: LoadEvent::Committed,
                ..
            } => {
                self.problem = None;
                cx.notify();
            }
            Event::WebProcessTerminated { reason, .. } => {
                self.finish(
                    SignInAnswer::Failed(format!("{WEB_PROCESS_ENDED} ({reason:?})")),
                    window,
                    cx,
                );
            }
            Event::ViewFailed { message, .. } => {
                self.finish(
                    SignInAnswer::Failed(format!("the page can't be made: {message}")),
                    window,
                    cx,
                );
            }
            _ => {}
        }
    }

    /// Copies an SHM frame's damage into the picture and shows it; the engine hears it was shown once the renderer
    /// is done with it.
    fn frame(&mut self, frame: Frame, cx: &mut Context<Self>) {
        let Some((host, _)) = self.engine() else {
            return;
        };
        let (view, buffer) = (frame.view, frame.buffer);
        let FrameContent::Shm(regions) = frame.content else {
            // an SHM engine hands no dmabuf: nothing to draw, the buffer goes back
            host.frame_presented(view, buffer);
            return;
        };
        let resized = (frame.width, frame.height) != (self.picture.width, self.picture.height);
        if resized {
            self.picture = Picture {
                width: frame.width,
                height: frame.height,
                pixels: vec![0; pixel_bytes(frame.width, frame.height)],
            };
        }
        for region in &regions {
            copy_region(&mut self.picture, region);
        }
        self.serial += 1;
        let damage = if resized {
            vec![PixelRect {
                x: 0,
                y: 0,
                width: frame.width,
                height: frame.height,
            }]
        } else {
            regions
                .iter()
                .map(|region| PixelRect {
                    x: region.rect.x,
                    y: region.rect.y,
                    width: region.rect.width,
                    height: region.rect.height,
                })
                .collect()
        };
        self.frame = Some(Arc::new(PixelFrame {
            surface: Arc::clone(&self.surface),
            serial: self.serial,
            width: frame.width,
            height: frame.height,
            pixels: Arc::new(self.picture.pixels.clone()),
            damage,
            completion: FrameCompletion::new(move |_| host.frame_presented(view, buffer)),
        }));
        cx.notify();
    }

    fn pointer(&self, kind: PointerKind, position: Point<Pixels>, modifiers: gpui::Modifiers) {
        let (Some((host, view)), Some(bounds)) = (self.engine(), self.bounds.get()) else {
            return;
        };
        let (x, y) = input::local(position, bounds.origin);
        let pointer = Pointer {
            kind,
            x,
            y,
            modifiers: input::modifiers(modifiers) | self.buttons,
        };
        host.input(view, Input::Pointer(pointer));
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let button = input::button(event.button);
        self.pointer(PointerKind::Down(button), event.position, event.modifiers);
        self.buttons |= input::button_bit(button);
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        let button = input::button(event.button);
        self.buttons -= input::button_bit(button);
        self.pointer(PointerKind::Up(button), event.position, event.modifiers);
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        self.pointer(PointerKind::Move, event.position, event.modifiers);
    }

    fn scroll(&mut self, event: &ScrollEvent, _: &mut Window, _: &mut Context<Self>) {
        let (Some((host, view)), Some(bounds)) = (self.engine(), self.bounds.get()) else {
            return;
        };
        let (x, y) = input::local(event.position, bounds.origin);
        let (delta_x, delta_y, precise) = input::scroll(event.delta);
        host.input(
            view,
            Input::Scroll(Scroll {
                x,
                y,
                delta_x,
                delta_y,
                precise,
                stop: false,
                modifiers: input::modifiers(event.modifiers) | self.buttons,
            }),
        );
    }

    fn key(&self, keystroke: &Keystroke, pressed: bool, cx: &mut Context<Self>) {
        let (Some((host, view)), Some(keyval)) = (self.engine(), input::keysym(keystroke)) else {
            return;
        };
        if pressed && input::pastes(keystroke) {
            let text = cx.read_from_clipboard().and_then(|item| item.text());
            host.set_clipboard(text.unwrap_or_default());
        }
        let key = Key {
            pressed,
            keyval,
            keycode: None,
            modifiers: input::modifiers(keystroke.modifiers),
        };
        host.input(view, Input::Key(key));
    }

    /// The page's frame, its geometry kept in step with the content's bounds and the window's scale.
    fn content(&self) -> impl IntoElement {
        let frame = self.frame.clone();
        let bounds_cell = Rc::clone(&self.bounds);
        let engine = self.engine();
        let sent = Rc::clone(&self.sent);
        canvas(
            move |bounds: Bounds<Pixels>, window: &mut Window, _: &mut App| {
                bounds_cell.set(Some(bounds));
                let Some((host, view)) = engine.as_ref() else {
                    return;
                };
                let geometry = geometry_of(bounds, window.scale_factor());
                let key = (geometry.width, geometry.height, geometry.scale.to_bits());
                if sent.get() != Some(key) {
                    sent.set(Some(key));
                    host.set_geometry(*view, geometry);
                }
            },
            move |bounds: Bounds<Pixels>, (), window: &mut Window, _: &mut App| {
                if let Some(frame) = frame {
                    window.paint_external_surface(bounds, ExternalSurface::Pixels(frame));
                }
            },
        )
        .size_full()
    }
}

impl Render for SignInView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let note = if self.run.is_none() {
            None
        } else if let Some(problem) = &self.problem {
            Some(format!("The sign-in page didn't load: {problem}"))
        } else {
            self.frame.is_none().then(|| "Loading the sign-in page…".to_owned())
        };
        let mut root = div()
            .id("signin-page")
            .size_full()
            .relative()
            .bg(rgb(0x00ff_ffff))
            .track_focus(&self.focus)
            .cursor(self.cursor)
            .on_any_mouse_down(cx.listener(Self::mouse_down))
            .capture_any_mouse_up(cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_scroll(cx.listener(Self::scroll))
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| view.key(&event.keystroke, true, cx)))
            .on_key_up(cx.listener(|view, event: &KeyUpEvent, _, cx| view.key(&event.keystroke, false, cx)))
            .child(self.content());
        if let Some(note) = note {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px_6()
                    .text_sm()
                    .text_color(rgb(0x0061_6161))
                    .child(note),
            );
        }
        root
    }
}

/// The engine's geometry for content laid out at `bounds` in a window of `scale`.
fn geometry_of(bounds: Bounds<Pixels>, scale: f32) -> Geometry {
    let logical = |value: Pixels| {
        let value = f32::from(value).round().max(1.0);
        // a window's size in logical pixels fits a u32
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "rounded, at least 1, and small"
        )]
        let value = value as u32;
        value
    };
    Geometry {
        width: logical(bounds.size.width),
        height: logical(bounds.size.height),
        scale: f64::from(scale),
    }
}

/// The bytes of a `width` x `height` picture.
fn pixel_bytes(width: u32, height: u32) -> usize {
    usize::try_from(u64::from(width) * u64::from(height) * 4).unwrap_or(0)
}

/// Copies `region`'s rows into `picture`, clipped to it.
fn copy_region(picture: &mut Picture, region: &teams_wpe_client::ShmRegion) {
    let rect = region.rect;
    let (Ok(x), Ok(y), Ok(width), Ok(height), Ok(stride)) = (
        usize::try_from(rect.x),
        usize::try_from(rect.y),
        usize::try_from(rect.width),
        usize::try_from(rect.height),
        usize::try_from(picture.width),
    ) else {
        return;
    };
    let row_bytes = width * 4;
    for row in 0..height {
        let from = row * row_bytes;
        let to = ((y + row) * stride + x) * 4;
        let (Some(source), Some(target)) = (
            region.pixels.get(from..from + row_bytes),
            picture.pixels.get_mut(to..to + row_bytes),
        ) else {
            return;
        };
        target.copy_from_slice(source);
    }
}
