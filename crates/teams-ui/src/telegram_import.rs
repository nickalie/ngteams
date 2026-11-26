//! The sticker picker's "Import a Telegram sticker pack": a link, and the bot token Telegram's Bot API takes. Teams has
//! no import of other apps' packs, so this panel is the app's own, drawn as the picker's other panels are.
//!
//! - **The link**: `t.me/addstickers/<name>` (or `tg://addstickers?set=<name>`, or the bare name); a link that names
//!   no pack is said to be one before anything is sent.
//! - **The token**: while none is kept, a masked field and a short line on how to get one (`@BotFather`, `/newbot`).
//!   While one is kept, the panel says so, never shows it, and offers Replace and Remove. A token is checked for
//!   `@BotFather`'s form, then kept in the keyring ([`BotToken`], off the UI thread) before the import starts.
//! - **Import** (or Enter in a field) tells the picker the link ([`TelegramImportEvent::Start`]); the picker's import
//!   line shows the progress and what was skipped. **Cancel** (or Escape) goes back to the picker.
//! - **Reach**: the link field has the focus when it opens; Tab and Shift+Tab go through the fields and the buttons in
//!   the order they are drawn, and Enter or Space presses a button. A button that goes away under the keyboard leaves
//!   the focus on what shows in its place.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, px, rgb,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Sizable as _, Size};
use teams_auth::secrets::Secret;
use teams_core::telegram::token_form;

use crate::icons::Glyph;
use crate::stickers::telegram::{BotToken, set_name};
use crate::theme::{self, Palette};

/// The panel's title.
pub const TITLE: &str = "Import a Telegram sticker pack";
/// The link field's hint.
pub const LINK_HINT: &str = "t.me/addstickers/\u{2026}";
/// How to get a token.
pub const TOKEN_HELP: &str = "Telegram gives sticker packs only to a bot. To get a bot token, message @BotFather in \
                              Telegram, send /newbot, and copy the token it gives you.";
/// What the panel says while a token is kept.
pub const TOKEN_SAVED: &str = "A bot token is saved on this computer.";
/// What it says of a link that names no pack.
pub const BAD_LINK: &str = "That isn't a link to a Telegram sticker pack. It looks like t.me/addstickers/\u{2026}";
/// What it says when no token is typed and none is kept.
pub const NO_TOKEN: &str = "Add a bot token first.";
/// What it says of a token not of `@BotFather`'s form.
pub const BAD_TOKEN: &str = "That isn't a bot token. A bot token looks like 123456789:AAH\u{2026}";
/// What it says when the keyring can't keep or forget the token.
pub const NOT_KEPT: &str = "The bot token can't be saved in this computer's keyring.";
/// What it says when the keyring can't be read.
pub const NOT_READ: &str = "The bot token can't be read from this computer's keyring.";

/// A button of the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    Back,
    Replace,
    Remove,
    Start,
    Cancel,
}

impl Press {
    const ALL: [Self; 5] = [Self::Back, Self::Replace, Self::Remove, Self::Start, Self::Cancel];

    /// Its test selector.
    #[must_use]
    pub fn selector(self) -> &'static str {
        match self {
            Self::Back => "telegram-import-back",
            Self::Replace => "telegram-import-replace",
            Self::Remove => "telegram-import-remove",
            Self::Start => "telegram-import-start",
            Self::Cancel => "telegram-import-cancel",
        }
    }
}

/// What the panel tells the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TelegramImportEvent {
    /// Import the pack of this link: its token is kept.
    Start(String),
    /// Back to the picker, with nothing imported.
    Cancel,
}

/// Whether a token is kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenState {
    Checking,
    None,
    Saved,
    /// The keyring can't be read.
    Unreadable,
}

/// The "Import a Telegram sticker pack" panel (see the module docs).
pub struct TelegramImport {
    tokens: BotToken,
    link: Entity<InputState>,
    token: Entity<InputState>,
    state: TokenState,
    /// The token field shown while one is kept.
    replacing: bool,
    /// A token is being kept or forgotten.
    busy: bool,
    problem: Option<SharedString>,
    focus: FocusHandle,
    /// Each button's focus, a stop of Tab's.
    buttons: Vec<(Press, FocusHandle)>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TelegramImportEvent> for TelegramImport {}

impl Focusable for TelegramImport {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TelegramImport {
    /// The panel, its link field focused; it looks for a kept token in the background.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let link = cx.new(|cx| InputState::new(window, cx).placeholder(LINK_HINT));
        let token = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Bot token"));
        let enter = |this: &mut Self,
                     _: &Entity<InputState>,
                     event: &InputEvent,
                     window: &mut Window,
                     cx: &mut Context<Self>| {
            match event {
                InputEvent::PressEnter { .. } => this.start(window, cx),
                InputEvent::Change => this.clear_problem(cx),
                _ => {}
            }
        };
        let mut subscriptions = vec![
            cx.subscribe_in(&link, window, enter),
            cx.subscribe_in(&token, window, enter),
        ];
        let this = cx.weak_entity();
        // the interceptor sees every keystroke of the app, before a field's own keys: a key is the panel's only while
        // the focus is in it
        subscriptions.push(cx.intercept_keystrokes(move |event, window, cx| {
            let handled = this.update(cx, |this, cx| {
                this.holds_focus(window, cx) && this.key(&event.keystroke, window, cx)
            });
            if matches!(handled, Ok(true)) {
                cx.stop_propagation();
            }
        }));
        link.update(cx, |link, cx| link.focus(window, cx));
        let tokens = BotToken::of(cx);
        let mut this = Self {
            tokens,
            link,
            token,
            state: TokenState::Checking,
            replacing: false,
            busy: false,
            problem: None,
            focus: cx.focus_handle(),
            buttons: Press::ALL
                .iter()
                // no tab index: stops of one index go in the drawn order, and the kit's fields take none
                .map(|press| (*press, cx.focus_handle().tab_stop(true)))
                .collect(),
            _subscriptions: subscriptions,
        };
        this.check_token(cx);
        this
    }

    #[must_use]
    pub fn token_state(&self) -> &TokenState {
        &self.state
    }

    #[must_use]
    pub fn problem(&self) -> Option<&SharedString> {
        self.problem.as_ref()
    }

    /// Whether the token field shows.
    #[must_use]
    pub fn asks_token(&self) -> bool {
        matches!(self.state, TokenState::None | TokenState::Unreadable) || self.replacing
    }

    /// Whether the focus is in the panel.
    pub fn holds_focus(&self, window: &Window, cx: &gpui::App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    /// Whether the token field has the focus.
    pub fn token_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.token.read(cx).focus_handle(cx).is_focused(window)
    }

    /// Whether the link field has the focus.
    pub fn link_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.link.read(cx).focus_handle(cx).is_focused(window)
    }

    /// The text in the link field.
    #[must_use]
    pub fn link_text(&self, cx: &gpui::App) -> SharedString {
        self.link.read(cx).value()
    }

    /// How many characters the token field holds: its mask shows that many, never the token.
    #[must_use]
    pub fn token_chars(&self, cx: &gpui::App) -> usize {
        self.token.read(cx).value().chars().count()
    }

    /// Puts `link` in the link field (as typed or pasted).
    pub fn set_link(&mut self, link: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.link
            .update(cx, |field, cx| field.set_value(link.to_owned(), window, cx));
    }

    /// Puts `token` in the token field (as typed or pasted).
    pub fn set_token(&mut self, token: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.token
            .update(cx, |field, cx| field.set_value(token.to_owned(), window, cx));
    }

    fn check_token(&mut self, cx: &mut Context<Self>) {
        let tokens = self.tokens.clone();
        let read = cx.background_spawn(async move { tokens.get().map(|token| token.is_some()) });
        cx.spawn(async move |this, cx| {
            let kept = read.await;
            let _ = this.update(cx, |this, cx| {
                this.state = match kept {
                    Ok(true) => TokenState::Saved,
                    Ok(false) => TokenState::None,
                    Err(_) => TokenState::Unreadable,
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// Import pressed: the link checked, the typed token checked and kept, then the picker told.
    pub fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.state == TokenState::Checking {
            return;
        }
        let link = self.link.read(cx).value().trim().to_owned();
        if set_name(&link).is_none() {
            self.problem = Some(BAD_LINK.into());
            self.link.update(cx, |field, cx| field.focus(window, cx));
            cx.notify();
            return;
        }
        let typed = self.token.read(cx).value().trim().to_owned();
        if !self.asks_token() || (typed.is_empty() && self.state == TokenState::Saved) {
            cx.emit(TelegramImportEvent::Start(link));
            return;
        }
        if typed.is_empty() || !token_form(&typed) {
            self.problem = Some(if typed.is_empty() { NO_TOKEN } else { BAD_TOKEN }.into());
            self.token.update(cx, |field, cx| field.focus(window, cx));
            cx.notify();
            return;
        }
        self.token.update(cx, |field, cx| field.set_value("", window, cx));
        self.keep(Some(Secret::new(typed)), Some(link), window, cx);
    }

    /// Keeps `token` in the keyring (or forgets the kept one), off the UI thread; then tells the picker `link`.
    fn keep(&mut self, token: Option<Secret>, link: Option<String>, window: &Window, cx: &mut Context<Self>) {
        self.busy = true;
        self.problem = None;
        cx.notify();
        let tokens = self.tokens.clone();
        let saved = token.is_some();
        let work = cx.background_spawn(async move {
            match token {
                Some(token) => tokens.put(&token),
                None => tokens.delete(),
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let done = work.await;
            let _ = this.update_in(cx, |this, window, cx| this.kept(done.is_ok(), saved, link, window, cx));
        })
        .detach();
    }

    /// The keyring kept (or forgot) the token when `done`; then the picker is told `link`.
    fn kept(&mut self, done: bool, saved: bool, link: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.busy = false;
        if done {
            self.state = if saved { TokenState::Saved } else { TokenState::None };
            self.replacing = false;
            // Replace and Remove go with the kept token: the keyboard stays in the panel, on the field drawn there now
            if !saved && matches!(self.focused_press(window), Some(Press::Replace | Press::Remove)) {
                self.token.update(cx, |field, cx| field.focus(window, cx));
            }
            if let Some(link) = link {
                cx.emit(TelegramImportEvent::Start(link));
            }
        } else {
            self.problem = Some(NOT_KEPT.into());
        }
        cx.notify();
    }

    fn clear_problem(&mut self, cx: &mut Context<Self>) {
        if self.problem.take().is_some() {
            cx.notify();
        }
    }

    /// Replace pressed: the token field shows, focused.
    pub fn replace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.replacing = !self.replacing;
        self.problem = None;
        if self.replacing {
            self.token.update(cx, |field, cx| field.focus(window, cx));
        } else if self.token_focused(window, cx) {
            self.focus_of(Press::Replace).focus(window, cx);
        }
        cx.notify();
    }

    /// Remove pressed: the kept token forgotten.
    pub fn remove(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.busy {
            self.keep(None, None, window, cx);
        }
    }

    fn cancel(cx: &mut Context<Self>) {
        cx.emit(TelegramImportEvent::Cancel);
    }

    /// Acts on `press`, as its click or Enter does.
    pub fn press(&mut self, press: Press, window: &mut Window, cx: &mut Context<Self>) {
        if !self.enabled(press) {
            return;
        }
        match press {
            Press::Back | Press::Cancel => Self::cancel(cx),
            Press::Replace => self.replace(window, cx),
            Press::Remove => self.remove(window, cx),
            Press::Start => self.start(window, cx),
        }
    }

    fn enabled(&self, press: Press) -> bool {
        match press {
            Press::Back | Press::Cancel => true,
            Press::Replace | Press::Remove => !self.busy && self.state == TokenState::Saved,
            Press::Start => !self.busy && self.state != TokenState::Checking,
        }
    }

    /// The button whose focus the keyboard is on.
    #[must_use]
    pub fn focused_press(&self, window: &Window) -> Option<Press> {
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
            "escape" if modifiers.number_of_modifiers() == 0 => Self::cancel(cx),
            "tab" if modifiers.number_of_modifiers() == u8::from(modifiers.shift) => {
                crate::keyboard::move_within(&self.focus, !modifiers.shift, window, cx);
                cx.notify();
            }
            "enter" | "space" if modifiers.number_of_modifiers() == 0 => match self.focused_press(window) {
                Some(press) => self.press(press, window, cx),
                None => return false,
            },
            _ => return false,
        }
        true
    }

    fn token_part(&mut self, palette: Palette, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let mut part = div().flex().flex_col().gap_1().child(label("Bot token", palette));
        match self.state {
            TokenState::Checking => {
                part = part.child(note("Looking for a saved bot token\u{2026}", palette));
            }
            TokenState::Saved => {
                let replace = self.button(
                    Press::Replace,
                    if self.replacing { "Keep it" } else { "Replace" },
                    false,
                    window,
                    cx,
                );
                let remove = self.button(Press::Remove, "Remove", false, window, cx);
                part = part
                    .child(
                        div()
                            .debug_selector(|| "telegram-import-token-saved".to_owned())
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .text_color(rgb(palette.foreground))
                            .child(
                                Glyph::LOCK
                                    .icon()
                                    .size(px(14.0))
                                    .text_color(rgb(palette.muted_foreground)),
                            )
                            .child(div().flex_1().child(TOKEN_SAVED)),
                    )
                    .child(div().flex().gap_2().child(replace).child(remove));
            }
            TokenState::None | TokenState::Unreadable => {}
        }
        if self.state == TokenState::Unreadable {
            part = part.child(note(NOT_READ, palette).text_color(rgb(theme::DANGER)));
        }
        if self.asks_token() {
            part = part
                .child(
                    div()
                        .debug_selector(|| "telegram-import-token".to_owned())
                        .child(Input::new(&self.token).with_size(Size::Small)),
                )
                .child(
                    div()
                        .debug_selector(|| "telegram-import-help".to_owned())
                        .child(note(TOKEN_HELP, palette)),
                );
        }
        part.into_any_element()
    }

    fn buttons(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .justify_end()
            .gap_2()
            .child(self.button(Press::Start, "Import", true, window, cx))
            .child(self.button(Press::Cancel, "Cancel", false, window, cx))
            .into_any_element()
    }

    /// A button: the brand's colour when `primary`, outlined otherwise, with the keyboard's ring while it has the focus.
    /// A brand ring on the brand's fill can't be seen, so a primary button's ring is outside it, past a gap, as Teams
    /// draws it.
    fn button(
        &self,
        press: Press,
        text: &'static str,
        primary: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = theme::current(cx);
        let enabled = self.enabled(press);
        let (background, foreground) = match (primary, enabled) {
            (true, true) => (palette.brand, palette.on_brand),
            (true, false) => (palette.border, palette.muted_foreground),
            (false, true) => (palette.surface, palette.foreground),
            (false, false) => (palette.surface, palette.muted_foreground),
        };
        let focus = self.focus_of(press);
        let focused = focus.is_focused(window);
        let ring = match (primary, focused) {
            (true, _) => gpui::transparent_black(),
            (false, true) => rgb(palette.brand).into(),
            (false, false) => rgb(palette.border).into(),
        };
        let element = div()
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
            .child(text)
            .when(primary && focused, |element| {
                element.child(outer_ring(press.selector(), palette))
            });
        if enabled {
            element
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| this.press(press, window, cx)))
        } else {
            element
        }
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

/// The ring of a focused primary button: outside its edge, past a gap of the panel's surface.
pub(crate) fn outer_ring(selector: &'static str, palette: Palette) -> gpui::Div {
    // from the padding box: past the button's 2px border, a 2px gap, then the 2px ring
    let out = px(-6.0);
    div()
        .debug_selector(move || format!("{selector}-ring"))
        .absolute()
        .top(out)
        .left(out)
        .right(out)
        .bottom(out)
        .rounded(px(10.0))
        .border_2()
        .border_color(rgb(palette.brand))
}

fn label(text: &'static str, palette: Palette) -> gpui::Div {
    div()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(palette.foreground))
        .child(text)
}

fn note(text: &'static str, palette: Palette) -> gpui::Div {
    div().text_xs().text_color(rgb(palette.muted_foreground)).child(text)
}

impl Render for TelegramImport {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let back = self.back(palette, window, cx);
        let header = div().flex().items_center().gap_1().child(back).child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(palette.foreground))
                .child(TITLE),
        );
        let link = div().flex().flex_col().gap_1().child(label("Link", palette)).child(
            div()
                .debug_selector(|| "telegram-import-link".to_owned())
                .child(Input::new(&self.link).with_size(Size::Small)),
        );
        let token = self.token_part(palette, window, cx);
        let problem = self.problem.clone().map(|problem| {
            div()
                .debug_selector(|| "telegram-import-problem".to_owned())
                .text_sm()
                .text_color(rgb(theme::DANGER))
                .child(problem)
        });
        let buttons = self.buttons(window, cx);
        div()
            .id("telegram-import")
            .debug_selector(|| "telegram-import".to_owned())
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .gap_3()
            .child(header)
            .child(link)
            .child(token)
            .children(problem)
            .child(buttons)
    }
}
