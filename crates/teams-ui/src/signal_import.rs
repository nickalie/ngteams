//! The sticker picker's "Import a Signal sticker pack": the pack's link. Teams has no import of other apps' packs, so
//! this panel is the app's own, drawn as the picker's other panels are.
//!
//! - **The link**: `signal.art/addstickers/#pack_id=…&pack_key=…` (or `sgnl://addstickers/?pack_id=…&pack_key=…`); a
//!   link that names no pack is said to be one before anything is sent. Signal's CDN needs no account.
//! - **Import** (or Enter in the field) tells the picker the link ([`SignalImportEvent::Start`]); the picker's import
//!   line shows the progress and what was skipped. **Cancel**, the back arrow or Escape goes back to the picker.
//! - **Reach**: the link field has the focus when it opens; Tab and Shift+Tab go through the back arrow, the field
//!   and the buttons, and Enter or Space presses a button.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Window, div, px, rgb,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Sizable as _, Size};

use crate::icons::Glyph;
use crate::stickers::signal::is_pack_link;
use crate::telegram_import::outer_ring;
use crate::theme::{self, Palette};

/// The panel's title.
pub const TITLE: &str = "Import a Signal sticker pack";
/// The link field's hint.
pub const LINK_HINT: &str = "signal.art/addstickers/#pack_id=\u{2026}";
/// Where a link comes from.
pub const HELP: &str = "Paste the link of a sticker pack from signal.art or from a Signal chat. The pack comes from \
                        Signal's servers, with no Signal account.";
/// What it says of a link that names no pack.
pub const BAD_LINK: &str = "That isn't a link to a Signal sticker pack. It looks like signal.art/addstickers/#pack_id=\u{2026}&pack_key=\u{2026}";

/// A button of the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    Back,
    Start,
    Cancel,
}

impl Press {
    const ALL: [Self; 3] = [Self::Back, Self::Start, Self::Cancel];

    /// Its test selector.
    #[must_use]
    pub fn selector(self) -> &'static str {
        match self {
            Self::Back => "signal-import-back",
            Self::Start => "signal-import-start",
            Self::Cancel => "signal-import-cancel",
        }
    }
}

/// What the panel tells the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignalImportEvent {
    /// Import the pack of this link.
    Start(String),
    /// Back to the picker, with nothing imported.
    Cancel,
}

/// The "Import a Signal sticker pack" panel (see the module docs).
pub struct SignalImport {
    link: Entity<InputState>,
    problem: Option<SharedString>,
    focus: FocusHandle,
    /// Each button's focus, a stop of Tab's.
    buttons: Vec<(Press, FocusHandle)>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SignalImportEvent> for SignalImport {}

impl Focusable for SignalImport {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SignalImport {
    /// The panel, its link field focused.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let link = cx.new(|cx| InputState::new(window, cx).placeholder(LINK_HINT));
        let mut subscriptions = vec![cx.subscribe_in(&link, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.clear_problem(cx);
            }
        })];
        let this = cx.weak_entity();
        // the interceptor sees every keystroke of the app, before the field's own keys: a key is the panel's only
        // while the focus is in it
        subscriptions.push(cx.intercept_keystrokes(move |event, window, cx| {
            let handled = this.update(cx, |this, cx| {
                this.holds_focus(window, cx) && this.key(&event.keystroke, window, cx)
            });
            if matches!(handled, Ok(true)) {
                cx.stop_propagation();
            }
        }));
        link.update(cx, |link, cx| link.focus(window, cx));
        Self {
            link,
            problem: None,
            focus: cx.focus_handle(),
            buttons: Press::ALL
                .iter()
                .map(|press| (*press, cx.focus_handle().tab_stop(true)))
                .collect(),
            _subscriptions: subscriptions,
        }
    }

    #[must_use]
    pub fn problem(&self) -> Option<&SharedString> {
        self.problem.as_ref()
    }

    /// Whether the focus is in the panel.
    pub fn holds_focus(&self, window: &Window, cx: &gpui::App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    /// Puts `link` in the link field (as typed or pasted).
    pub fn set_link(&mut self, link: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.link
            .update(cx, |field, cx| field.set_value(link.to_owned(), window, cx));
    }

    /// Import pressed: the link checked, then the picker told.
    pub fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let link = self.link.read(cx).value().trim().to_owned();
        if is_pack_link(&link) {
            cx.emit(SignalImportEvent::Start(link));
            return;
        }
        self.problem = Some(BAD_LINK.into());
        self.link.update(cx, |field, cx| field.focus(window, cx));
        cx.notify();
    }

    fn clear_problem(&mut self, cx: &mut Context<Self>) {
        if self.problem.take().is_some() {
            cx.notify();
        }
    }

    /// Acts on `press`, as its click or Enter does.
    pub fn press(&mut self, press: Press, window: &mut Window, cx: &mut Context<Self>) {
        match press {
            Press::Back | Press::Cancel => cx.emit(SignalImportEvent::Cancel),
            Press::Start => self.start(window, cx),
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

    /// Handles `keystroke` while the focus is in the panel; whether it took it.
    fn key(&mut self, keystroke: &gpui::Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let modifiers = keystroke.modifiers;
        match keystroke.key.as_str() {
            "escape" if modifiers.number_of_modifiers() == 0 => cx.emit(SignalImportEvent::Cancel),
            "tab" if modifiers.number_of_modifiers() == u8::from(modifiers.shift) => {
                crate::keyboard::move_within(&self.focus, !modifiers.shift, window, cx);
                cx.notify();
            }
            "enter" if modifiers.number_of_modifiers() == 0 && self.link.focus_handle(cx).is_focused(window) => {
                self.start(window, cx);
            }
            "enter" | "space" if modifiers.number_of_modifiers() == 0 => match self.focused_press(window) {
                Some(press) => self.press(press, window, cx),
                None => return false,
            },
            _ => return false,
        }
        true
    }

    /// A button: the brand's colour when `primary`, outlined otherwise, with the keyboard's ring while it has the focus.
    /// A primary button's ring is outside its fill, as the Telegram panel's is.
    fn button(
        &self,
        press: Press,
        text: &'static str,
        primary: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = theme::current(cx);
        let (background, foreground) = if primary {
            (palette.brand, palette.on_brand)
        } else {
            (palette.surface, palette.foreground)
        };
        let focus = self.focus_of(press);
        let focused = focus.is_focused(window);
        let ring = match (primary, focused) {
            (true, _) => gpui::transparent_black(),
            (false, true) => rgb(palette.brand).into(),
            (false, false) => rgb(palette.border).into(),
        };
        div()
            .id(press.selector())
            .debug_selector(move || press.selector().to_owned())
            .track_focus(&focus)
            .relative()
            .flex_none()
            .px_3()
            .py_1()
            .rounded_md()
            .border_2()
            .border_color(ring)
            .text_sm()
            .bg(rgb(background))
            .text_color(rgb(foreground))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| this.press(press, window, cx)))
            .child(text)
            .when(primary && focused, |element| {
                element.child(outer_ring(press.selector(), palette))
            })
    }

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
            .on_click(cx.listener(|this, _, window, cx| this.press(Press::Back, window, cx)))
            .child(
                Glyph::ARROW_LEFT
                    .icon()
                    .size(px(16.0))
                    .text_color(rgb(palette.foreground)),
            )
    }
}

impl Render for SignalImport {
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
                    .child(TITLE),
            );
        let link = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.foreground))
                    .child("Link"),
            )
            .child(
                div()
                    .debug_selector(|| "signal-import-link".to_owned())
                    .child(Input::new(&self.link).with_size(Size::Small)),
            )
            .child(
                div()
                    .debug_selector(|| "signal-import-help".to_owned())
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(HELP),
            );
        let problem = self.problem.clone().map(|problem| {
            div()
                .debug_selector(|| "signal-import-problem".to_owned())
                .text_sm()
                .text_color(rgb(theme::DANGER))
                .child(problem)
        });
        let buttons = div()
            .flex()
            .justify_end()
            .gap_2()
            .child(self.button(Press::Start, "Import", true, window, cx))
            .child(self.button(Press::Cancel, "Cancel", false, window, cx));
        div()
            .id("signal-import")
            .debug_selector(|| "signal-import".to_owned())
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .gap_3()
            .child(header)
            .child(link)
            .children(problem)
            .child(buttons)
    }
}
