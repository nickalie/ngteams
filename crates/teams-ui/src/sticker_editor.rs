//! Teams' "Edit sticker", in the compose box's picker: a click on one of Teams' stickers that takes a caption (its
//! tile has a pencil mark) opens it in the place of the sticker grid, as Teams' web client does.
//!
//! - **The picture** is the sticker's picture with no words (read from Teams' sticker CDN with no credential), with the
//!   caption drawn on it as it is typed ([`crate::stickers::caption`]), at most 250 px wide and 207 px high.
//! - **The caption field** of a sticker ("Caption", up to 150 characters, four lines) starts with the sticker's own
//!   words. A meme has two one-line fields, "Top Caption" and "Bottom caption" (25 characters each), which start empty;
//!   the picture shows their hints while both are.
//! - **Done** (offered only while a field holds words) draws the picture at its full size and tells the picker
//!   ([`StickerEditorEvent::Done`]): the box puts it in as a sticker picture, sent to the media service at once.
//!   **Cancel**, the back arrow or Escape goes back to the stickers. A picture that can't be read says so.
//! - **Reach**: the first field has the focus when it opens; Tab and Shift+Tab go through the back arrow, the fields,
//!   Done (while it is offered) and Cancel (Teams' order); Enter or Space presses a button.

use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, RenderImage, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, img, px, rgb,
};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::{Sizable as _, Size};
use teams_proto::PastedPicture;

use crate::bridge::Core;
use crate::icons::Glyph;
use crate::stickers::caption::{self, BOTTOM_HINT, CAPTION_HINT, Caption, MAX_CAPTION, MAX_MEME_LINE, TOP_HINT};
use crate::stickers::teams;
use crate::telegram_import::outer_ring;
use crate::theme::{self, Palette};

/// The editor's name, as Teams' screen reader hears it.
pub const TITLE: &str = "Edit sticker";
/// What it says of a picture it can't read, in Teams' words.
pub const LOAD_FAILED: &str = "Failed to load image";
/// The largest the picture is drawn, px.
const PICTURE_WIDTH: f32 = 250.0;
const PICTURE_HEIGHT: f32 = 207.0;

/// A button of the editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    Back,
    Done,
    Cancel,
}

impl Press {
    const ALL: [Self; 3] = [Self::Back, Self::Done, Self::Cancel];

    /// Its test selector.
    #[must_use]
    pub fn selector(self) -> &'static str {
        match self {
            Self::Back => "sticker-editor-back",
            Self::Done => "sticker-editor-done",
            Self::Cancel => "sticker-editor-cancel",
        }
    }
}

/// A sticker with its caption drawn in, ready for the box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captioned {
    /// The id of Teams' sticker it was drawn on.
    pub sticker: String,
    /// The picture, a PNG of the template's size.
    pub picture: PastedPicture,
    /// The size it is shown at in a message, CSS px.
    pub shown: (u32, u32),
    /// The words a message names it by.
    pub label: String,
}

/// What the editor tells the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StickerEditorEvent {
    Done(Captioned),
    Cancel,
}

/// One of Teams' stickers that takes a caption, as the editor opens it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub id: String,
    pub name: String,
    pub caption: Caption,
    /// The address of its picture with no words.
    pub template: String,
}

impl Draft {
    /// Teams' sticker `id`, when it takes a caption.
    #[must_use]
    pub fn of(id: &str) -> Option<Self> {
        let catalog = teams::catalog();
        let sticker = catalog.sticker(id)?;
        Some(Self {
            id: sticker.id.clone(),
            name: sticker.name.clone(),
            caption: sticker.caption.clone()?,
            template: catalog.template(sticker)?,
        })
    }
}

/// The fields of a caption: one block, or a meme's two lines.
enum Fields {
    Block(Entity<TextareaState>),
    Meme(Entity<InputState>, Entity<InputState>),
}

/// How the template's read stands.
enum Template {
    Reading,
    Ready(Arc<PastedPicture>),
    Failed,
}

/// The "Edit sticker" editor (see the module docs).
pub struct StickerEditor {
    sticker: String,
    name: String,
    caption: Caption,
    fields: Fields,
    template: Template,
    /// The picture as last drawn, and its size.
    preview: Option<(Arc<RenderImage>, (u32, u32))>,
    /// The number of the last drawing asked for: an older one that ends later is dropped.
    drawing: u64,
    /// Done was pressed, and the full picture is being drawn.
    finishing: bool,
    focus: FocusHandle,
    buttons: Vec<(Press, FocusHandle)>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<StickerEditorEvent> for StickerEditor {}

impl Focusable for StickerEditor {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl StickerEditor {
    /// The editor of `draft`, its picture read through `core`, its first field focused.
    pub fn new(core: &Arc<dyn Core>, draft: Draft, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Draft {
            id,
            name,
            caption,
            template,
        } = draft;
        let (fields, mut subscriptions) = Self::fields(&caption, window, cx);
        let this = cx.weak_entity();
        // the interceptor sees every keystroke of the app: a key is the editor's only while the focus is in it
        subscriptions.push(cx.intercept_keystrokes(move |event, window, cx| {
            let handled = this.update(cx, |this, cx| {
                this.holds_focus(window, cx) && this.key(&event.keystroke, window, cx)
            });
            if matches!(handled, Ok(true)) {
                cx.stop_propagation();
            }
        }));
        let reply = core.message_picture(template);
        cx.spawn(async move |this, cx| {
            let bytes = reply.await.ok().flatten();
            let _ = this.update(cx, |this, cx| this.read(bytes, cx));
        })
        .detach();
        let editor = Self {
            sticker: id,
            name,
            caption,
            fields,
            template: Template::Reading,
            preview: None,
            drawing: 0,
            finishing: false,
            focus: cx.focus_handle(),
            buttons: Press::ALL
                .iter()
                .map(|press| (*press, cx.focus_handle().tab_stop(true)))
                .collect(),
            _subscriptions: subscriptions,
        };
        editor.focus_first(window, cx);
        editor
    }

    /// The fields `caption` takes, and the subscriptions that redraw the picture as they change.
    fn fields(caption: &Caption, window: &mut Window, cx: &mut Context<Self>) -> (Fields, Vec<Subscription>) {
        if caption.is_meme() {
            let line = |hint: &'static str, window: &mut Window, cx: &mut Context<Self>| {
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(hint)
                        .validate(|text, _| text.chars().count() <= MAX_MEME_LINE)
                })
            };
            let (top, bottom) = (line(TOP_HINT, window, cx), line(BOTTOM_HINT, window, cx));
            let subscriptions = vec![Self::watch(&top, window, cx), Self::watch(&bottom, window, cx)];
            return (Fields::Meme(top, bottom), subscriptions);
        }
        let first = caption.first_text().concat();
        let block = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(CAPTION_HINT)
                .soft_wrap(true)
                .auto_grow(4, 4)
                .default_value(first)
        });
        let subscriptions = vec![
            cx.subscribe_in(&block, window, |this, block, event: &InputEvent, window, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                // the text box takes no limit of its own: what goes past Teams' 150 characters is cut off
                let text = block.read(cx).value();
                if text.chars().count() > MAX_CAPTION {
                    let kept: String = text.chars().take(MAX_CAPTION).collect();
                    block.update(cx, |block, cx| block.set_value(kept, window, cx));
                }
                this.redraw(cx);
            }),
        ];
        (Fields::Block(block), subscriptions)
    }

    /// Redraws the picture as a meme's line `field` changes.
    fn watch(field: &Entity<InputState>, window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe_in(field, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.redraw(cx);
            }
        })
    }

    fn focus_first(&self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.fields {
            Fields::Block(block) => block.update(cx, |block, cx| block.focus(window, cx)),
            Fields::Meme(top, _) => top.update(cx, |top, cx| top.focus(window, cx)),
        }
    }

    /// The sticker's id.
    #[must_use]
    pub fn sticker(&self) -> &str {
        &self.sticker
    }

    /// What the fields hold, in order.
    #[must_use]
    pub fn typed(&self, cx: &gpui::App) -> Vec<String> {
        match &self.fields {
            Fields::Block(block) => vec![block.read(cx).value().to_string()],
            Fields::Meme(top, bottom) => vec![top.read(cx).value().to_string(), bottom.read(cx).value().to_string()],
        }
    }

    /// Puts `texts` in the fields, in order (as typed).
    pub fn set_typed(&mut self, texts: &[&str], window: &mut Window, cx: &mut Context<Self>) {
        let text = |at: usize| texts.get(at).copied().unwrap_or_default().to_owned();
        match &self.fields {
            Fields::Block(block) => block.update(cx, |block, cx| block.set_value(text(0), window, cx)),
            Fields::Meme(top, bottom) => {
                top.update(cx, |top, cx| top.set_value(text(0), window, cx));
                bottom.update(cx, |bottom, cx| bottom.set_value(text(1), window, cx));
            }
        }
        self.redraw(cx);
    }

    /// Whether Done is offered: a field holds words, the picture is read, and Done isn't already drawing it.
    #[must_use]
    pub fn can_finish(&self, cx: &gpui::App) -> bool {
        matches!(self.template, Template::Ready(_))
            && !self.finishing
            && self.typed(cx).iter().any(|text| !text.trim().is_empty())
    }

    /// Whether the picture couldn't be read.
    #[must_use]
    pub fn failed(&self) -> bool {
        matches!(self.template, Template::Failed)
    }

    /// The picture as last drawn: its size, once it is.
    #[must_use]
    pub fn drawn(&self) -> Option<(u32, u32)> {
        self.preview.as_ref().map(|(_, size)| *size)
    }

    /// Whether the focus is in the editor: in what it draws, or in a field it hides (a picture that can't be read
    /// leaves only the back arrow, and Escape must still go back).
    pub fn holds_focus(&self, window: &Window, cx: &gpui::App) -> bool {
        self.focus.contains_focused(window, cx) || self.stops(cx).iter().any(|stop| stop.is_focused(window))
    }

    /// The template's bytes came (`None`: they can't be read).
    fn read(&mut self, bytes: Option<Arc<PastedPicture>>, cx: &mut Context<Self>) {
        self.template = match bytes {
            Some(bytes) if image::load_from_memory(&bytes.bytes).is_ok() => Template::Ready(bytes),
            _ => Template::Failed,
        };
        self.redraw(cx);
    }

    /// Draws the picture with the fields' words again, on a background thread.
    fn redraw(&mut self, cx: &mut Context<Self>) {
        let Template::Ready(bytes) = &self.template else {
            cx.notify();
            return;
        };
        self.drawing += 1;
        let (number, bytes, caption, typed) = (self.drawing, Arc::clone(bytes), self.caption.clone(), self.typed(cx));
        let drawn = cx.background_spawn(async move {
            let mut fonts = caption::shared()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            caption::draw(&bytes.bytes, &caption, &typed, &mut fonts)
        });
        cx.spawn(async move |this, cx| {
            let Some(mut image) = drawn.await else {
                return;
            };
            let size = image.dimensions();
            crate::inline::to_bgra(&mut image);
            let image = Arc::new(RenderImage::new(vec![image::Frame::new(image)]));
            let _ = this.update(cx, |this, cx| {
                if this.drawing == number {
                    this.preview = Some((image, size));
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Done: the picture drawn at its full size as a PNG, then the picker told.
    pub fn finish(&mut self, cx: &mut Context<Self>) {
        if !self.can_finish(cx) {
            return;
        }
        let Template::Ready(bytes) = &self.template else {
            return;
        };
        self.finishing = true;
        cx.notify();
        let typed = self.typed(cx);
        let label = self.caption.label(&self.name, &typed);
        let (bytes, caption, sticker) = (Arc::clone(bytes), self.caption.clone(), self.sticker.clone());
        let drawn = cx.background_spawn(async move {
            let picture = caption::render(&bytes.bytes, &caption, &typed)?;
            let size = image::load_from_memory(&picture.bytes).ok()?;
            Some((picture, caption::shown((size.width(), size.height()))))
        });
        cx.spawn(async move |this, cx| {
            let drawn = drawn.await;
            let _ = this.update(cx, |this, cx| {
                this.finishing = false;
                match drawn {
                    Some((picture, shown)) => cx.emit(StickerEditorEvent::Done(Captioned {
                        sticker,
                        picture,
                        shown,
                        label,
                    })),
                    None => this.template = Template::Failed,
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Acts on `press`, as its click or Enter does.
    pub fn press(&mut self, press: Press, cx: &mut Context<Self>) {
        match press {
            Press::Back | Press::Cancel => cx.emit(StickerEditorEvent::Cancel),
            Press::Done => self.finish(cx),
        }
    }

    /// The button whose focus the keyboard is on.
    fn focused_press(&self, window: &Window) -> Option<Press> {
        self.buttons
            .iter()
            .find(|(_, focus)| focus.is_focused(window))
            .map(|(press, _)| *press)
    }

    fn focus_of(&self, press: Press) -> FocusHandle {
        self.buttons
            .iter()
            .find(|(held, _)| *held == press)
            .map_or_else(|| self.focus.clone(), |(_, focus)| focus.clone())
    }

    /// The editor's stops in Tab's order: the back arrow, the fields, Done, Cancel.
    fn stops(&self, cx: &gpui::App) -> Vec<FocusHandle> {
        let mut stops = vec![self.focus_of(Press::Back)];
        match &self.fields {
            Fields::Block(block) => stops.push(block.focus_handle(cx)),
            Fields::Meme(top, bottom) => stops.extend([top.focus_handle(cx), bottom.focus_handle(cx)]),
        }
        // a Done that isn't offered takes no focus, as a disabled button
        if self.can_finish(cx) {
            stops.push(self.focus_of(Press::Done));
        }
        stops.push(self.focus_of(Press::Cancel));
        stops
    }

    /// Moves the focus to the next stop (or the one before), round from the last to the first.
    fn step_focus(&self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let stops = self.stops(cx);
        let at = stops.iter().position(|stop| stop.contains_focused(window, cx));
        let count = stops.len();
        let next = match (at, forward) {
            (Some(at), true) => (at + 1) % count,
            (Some(at), false) => (at + count - 1) % count,
            (None, _) => 0,
        };
        window.focus(&stops[next], cx);
        cx.notify();
    }

    /// Handles `keystroke` while the focus is in the editor; whether it took it.
    fn key(&mut self, keystroke: &gpui::Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let modifiers = keystroke.modifiers;
        match keystroke.key.as_str() {
            "escape" if modifiers.number_of_modifiers() == 0 => cx.emit(StickerEditorEvent::Cancel),
            "tab" if modifiers.number_of_modifiers() == u8::from(modifiers.shift) => {
                self.step_focus(!modifiers.shift, window, cx);
            }
            "enter" | "space" if modifiers.number_of_modifiers() == 0 => match self.focused_press(window) {
                Some(press) => self.press(press, cx),
                None => return false,
            },
            _ => return false,
        }
        true
    }
}

impl StickerEditor {
    fn back(&self, palette: Palette, window: &Window, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let focus = self.focus_of(Press::Back);
        let ring = if focus.is_focused(window) {
            rgb(palette.brand).into()
        } else {
            gpui::transparent_black()
        };
        div()
            .id(Press::Back.selector())
            .debug_selector(|| Press::Back.selector().to_owned())
            .track_focus(&focus)
            .flex_none()
            .p_1()
            .rounded_md()
            .border_2()
            .border_color(ring)
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(palette.hover)))
            .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Back").build(window, cx))
            .on_click(cx.listener(|this, _, _, cx| this.press(Press::Back, cx)))
            .child(
                Glyph::ARROW_LEFT
                    .icon()
                    .size(px(16.0))
                    .text_color(rgb(palette.foreground)),
            )
    }

    /// Cancel or Done, as wide as each other: Done in the brand's colour, dimmed while it isn't offered.
    fn button(&self, press: Press, window: &Window, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let palette = theme::current(cx);
        let primary = press == Press::Done;
        let offered = !primary || self.can_finish(cx);
        let (background, foreground, border) = if primary {
            (palette.brand, palette.on_brand, palette.brand)
        } else {
            (palette.surface, palette.foreground, palette.border)
        };
        let focus = self.focus_of(press);
        let focused = focus.is_focused(window);
        let text = if primary { "Done" } else { "Cancel" };
        div()
            .id(press.selector())
            .debug_selector(move || press.selector().to_owned())
            .when(offered, |button| button.track_focus(&focus))
            .relative()
            .flex_1()
            .flex()
            .justify_center()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(if focused && !primary { palette.brand } else { border }))
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .bg(rgb(background))
            .text_color(rgb(foreground))
            .when(!offered, |button| button.opacity(0.4))
            .when(offered, |button| {
                button
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.press(press, cx)))
            })
            .child(text)
            .when(primary && focused, |button| {
                button.child(outer_ring(press.selector(), palette))
            })
    }

    /// The picture as drawn, fitted in the picture's box; a blank box while it is read, Teams' words when it can't be.
    fn picture(&self, palette: Palette) -> AnyElement {
        let frame = div()
            .debug_selector(|| "sticker-editor-picture".to_owned())
            .flex()
            .justify_center()
            .items_center()
            .h(px(PICTURE_HEIGHT));
        if self.failed() {
            return frame
                .text_sm()
                .text_color(rgb(theme::DANGER))
                .child(LOAD_FAILED)
                .into_any_element();
        }
        let Some((image, (width, height))) = &self.preview else {
            return frame.into_any_element();
        };
        #[expect(clippy::cast_precision_loss, reason = "a sticker's picture is a few hundred pixels")]
        let (width, height) = (*width as f32, *height as f32);
        let scale = (PICTURE_WIDTH / width.max(1.0)).min(PICTURE_HEIGHT / height.max(1.0));
        frame
            .child(
                img(Arc::clone(image))
                    .w(px(width * scale))
                    .h(px(height * scale))
                    .flex_none()
                    .border_1()
                    .border_color(rgb(palette.border)),
            )
            .into_any_element()
    }

    fn inputs(&self) -> AnyElement {
        match &self.fields {
            Fields::Block(block) => div()
                .debug_selector(|| "sticker-editor-caption".to_owned())
                .child(Textarea::new(block))
                .into_any_element(),
            Fields::Meme(top, bottom) => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .debug_selector(|| "sticker-editor-top".to_owned())
                        .child(Input::new(top).with_size(Size::Small)),
                )
                .child(
                    div()
                        .debug_selector(|| "sticker-editor-bottom".to_owned())
                        .child(Input::new(bottom).with_size(Size::Small)),
                )
                .into_any_element(),
        }
    }
}

impl Render for StickerEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let header = div()
            .flex()
            .items_center()
            .gap_1()
            .child(self.back(palette, window, cx))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.foreground))
                    .truncate()
                    .child(TITLE),
            );
        let buttons = div()
            .flex()
            .gap_2()
            .child(self.button(Press::Cancel, window, cx))
            .child(self.button(Press::Done, window, cx));
        div()
            .id("sticker-editor")
            .debug_selector(|| "sticker-editor".to_owned())
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .gap_3()
            .child(header)
            .child(self.picture(palette))
            .when(!self.failed(), |editor| editor.child(self.inputs()).child(buttons))
    }
}
