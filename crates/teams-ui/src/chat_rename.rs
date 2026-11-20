//! A group chat's name, as browser Teams sets it from the chat header (seen 2026-11-20 with lab accounts): a group
//! chat's header shows a pencil after its name ("Name group chat"), and the pencil opens a small popup under the name.
//! A 1:1 chat and a meeting's chat have no pencil, and the chat list's and the header's "More chat options" offer no
//! rename.
//!
//! - **The popup**: "Group name" over a box that holds the chat's name (empty while it has none: "Type a group name"),
//!   all of it selected, the keyboard in it; Cancel and Save under it, Save lit even while the box is empty. 400 px
//!   wide, as Teams draws it.
//! - **The limit**: the box takes 250 characters. A 251st is refused and "Your group name can't be longer than 250
//!   characters." shows under the box until the next change it takes.
//! - **Save** (or Enter) sends the name through the outbox ([`Action::RenameChat`]): trimmed, and an empty one takes
//!   the name off, so the chat is called by its people again. The popup closes once the outbox sent it; the engine
//!   then calls the chat by its new name everywhere (the list, the header, the search, a call's window). A name that is
//!   the same as before closes it with nothing sent. A rename that fails keeps the popup and says so.
//! - **Cancel**, Escape or a press outside closes it, with nothing sent, but not while a save is on its way. The pencil
//!   again or another chat closes it then too: the save continues, and a failure shows as a toast
//!   ([`ChatRenameEvent::NotSaved`]). The read-only mode shows no pencil.
//!
//! Traced: `chat-rename opened`, `saved` (the chat), never the name.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, AppContext as _, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Task,
    Window, div, px, rgb,
};
use gpui_component::Disableable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use teams_core::Action;
use teams_proto::CHAT_TOPIC_MOST;
use teams_store::OutboxState;

use crate::bridge::Core;
use crate::chat_people::Placed;
use crate::icons::{Glyph, selected_by};
use crate::sending::settled_row;
use crate::theme;
use crate::trace::{TraceLine, ViewTrace};

/// The header pencil's tooltip and accessible name, as Teams words it.
pub const NAME_TIP: &str = "Name group chat";
/// The popup's label over its box.
pub const NAME_LABEL: &str = "Group name";
/// The box's hint while it is empty.
pub const NAME_HINT: &str = "Type a group name";
/// What the popup says when the box refused a character past the limit.
pub const TOO_LONG: &str = "Your group name can't be longer than 250 characters.";
/// What it says when the name couldn't be saved.
pub const NOT_SAVED: &str = "We couldn't save the group name. Check your connection and try again.";

/// The popup's width, px, as Teams draws it.
const WIDTH: f32 = 400.0;

/// What the popup tells the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatRenameEvent {
    /// Saved, cancelled or dismissed.
    Close,
    /// The save failed ([`NOT_SAVED`]): the popup says so, and the shell says so when it closed the popup before.
    NotSaved,
}

/// The "Name group chat" popup of one group chat (see the module docs).
pub struct ChatRename {
    core: Arc<dyn Core>,
    trace: Arc<ViewTrace>,
    chat: String,
    /// The chat's name when the popup opened: empty while it had none.
    was: String,
    input: Entity<InputState>,
    /// The box refused a character past the limit since its last change.
    refused: Rc<Cell<bool>>,
    problem: Option<SharedString>,
    saving: Option<Task<()>>,
    focus: FocusHandle,
    /// Where the header's pencil is drawn: a press on it toggles the popup, never closes it as a press outside.
    button: Placed,
    /// Where the popup itself is drawn.
    popup: Placed,
    _input: Subscription,
}

impl EventEmitter<ChatRenameEvent> for ChatRename {}

/// What the popup is made with.
pub struct ChatRenameParts {
    pub core: Arc<dyn Core>,
    pub trace: Arc<ViewTrace>,
    /// Where the header's pencil is drawn.
    pub button: Placed,
}

/// The box's check of a change: one past [`CHAT_TOPIC_MOST`] characters is refused, and `refused` and the view `view`
/// are told.
fn within_limit(refused: Rc<Cell<bool>>, view: gpui::EntityId) -> impl Fn(&str, &mut gpui::App) -> bool + 'static {
    move |text, cx| {
        let fits = text.chars().count() <= CHAT_TOPIC_MOST;
        if !fits {
            refused.set(true);
            cx.notify(view);
        }
        fits
    }
}

impl ChatRename {
    /// The popup naming `chat`, now called `topic` (empty while it has no name).
    pub fn new(parts: ChatRenameParts, chat: String, topic: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ChatRenameParts { core, trace, button } = parts;
        let refused = Rc::new(Cell::new(false));
        let check = within_limit(Rc::clone(&refused), cx.entity_id());
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(NAME_HINT)
                .default_value(topic.to_owned())
                .validate(check)
        });
        let subscription = cx.subscribe_in(&input, window, |this: &mut Self, _, event: &InputEvent, window, cx| {
            this.on_input(event, window, cx);
        });
        trace.record(&TraceLine::new("chat-rename", "opened").conversation(&chat));
        Self {
            core,
            trace,
            chat,
            was: topic.to_owned(),
            input,
            refused,
            problem: None,
            saving: None,
            focus: cx.focus_handle(),
            button,
            popup: Placed::default(),
            _input: subscription,
        }
    }

    /// The chat it names.
    #[must_use]
    pub fn chat(&self) -> &str {
        &self.chat
    }

    /// What the box holds now.
    #[must_use]
    pub fn value(&self, cx: &gpui::App) -> String {
        self.input.read(cx).value().to_string()
    }

    /// The box's state, for typing into it.
    #[must_use]
    pub fn input(&self) -> &Entity<InputState> {
        &self.input
    }

    /// Whether the box refused a character past the limit since its last change.
    #[must_use]
    pub fn too_long(&self) -> bool {
        self.refused.get()
    }

    /// Whether a save is on its way.
    #[must_use]
    pub fn saving(&self) -> bool {
        self.saving.is_some()
    }

    /// What went wrong with the last save, if anything did.
    #[must_use]
    pub fn problem(&self) -> Option<&SharedString> {
        self.problem.as_ref()
    }

    /// The keyboard to the box, its text selected, as Teams opens it.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
    }

    fn on_input(&mut self, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Change => {
                self.refused.set(false);
                self.problem = None;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => self.save(window, cx),
            InputEvent::Focus | InputEvent::Blur => {}
        }
    }

    /// Save (see the module docs).
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving.is_some() || self.core.read_only() {
            return;
        }
        let topic = self.value(cx).trim().to_owned();
        if topic == self.was.trim() {
            cx.emit(ChatRenameEvent::Close);
            return;
        }
        let core = Arc::clone(&self.core);
        let action = Action::RenameChat {
            conversation: self.chat.clone(),
            topic,
        };
        self.problem = None;
        self.saving = Some(cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let row = settled_row(core.as_ref(), core.write(action), cx.background_executor()).await;
            let saved = row.is_some_and(|row| row.state == OutboxState::Sent);
            let _ = this.update(cx, |this, cx| this.saved(saved, cx));
        }));
        cx.notify();
    }

    fn saved(&mut self, saved: bool, cx: &mut Context<Self>) {
        self.saving = None;
        if saved {
            self.trace
                .record(&TraceLine::new("chat-rename", "saved").conversation(&self.chat));
            cx.emit(ChatRenameEvent::Close);
        } else {
            self.problem = Some(NOT_SAVED.into());
            cx.emit(ChatRenameEvent::NotSaved);
        }
        cx.notify();
    }

    /// Cancel: closed with nothing sent.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.saving.is_none() {
            cx.emit(ChatRenameEvent::Close);
        }
    }

    // --- drawing ---

    fn notice(&self) -> Option<AnyElement> {
        let words = if self.too_long() {
            Some(SharedString::from(TOO_LONG))
        } else {
            self.problem.clone()
        }?;
        Some(
            div()
                .debug_selector(|| "chat-rename-problem".to_owned())
                .flex()
                .items_center()
                .gap_1()
                .text_xs()
                .text_color(rgb(theme::DANGER))
                .child(Glyph::CIRCLE_ALERT.icon().size(px(12.0)))
                .child(words)
                .into_any_element(),
        )
    }

    fn buttons(&self, cx: &mut Context<Self>) -> gpui::Div {
        let busy = self.saving.is_some();
        let cancel = selected_by(
            "chat-rename-cancel",
            Button::new("chat-rename-cancel")
                .label("Cancel")
                .outline()
                .w(px(96.0))
                .disabled(busy)
                .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
        );
        let save = selected_by(
            "chat-rename-save",
            Button::new("chat-rename-save")
                .label("Save")
                .primary()
                .w(px(96.0))
                .disabled(busy)
                .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
        );
        div().pt_5().flex().justify_end().gap_4().child(cancel).child(save)
    }
}

impl Focusable for ChatRename {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ChatRename {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let notice = self.notice();
        let popup = div()
            .id("chat-rename")
            .debug_selector(|| "chat-rename".to_owned())
            .track_focus(&self.focus)
            .key_context("ChatRename")
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel(cx);
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down_out(cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                let inside = [&this.popup, &this.button]
                    .iter()
                    .any(|placed| placed.holds(event.position));
                if !inside {
                    this.cancel(cx);
                }
            }))
            .occlude()
            .w(px(WIDTH))
            .p_4()
            .flex()
            .flex_col()
            .gap_1()
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.foreground))
            .border_1()
            .border_color(rgb(palette.border))
            .rounded_lg()
            .shadow_lg()
            .child(div().text_sm().child(NAME_LABEL))
            .child(
                div()
                    .debug_selector(|| "chat-rename-box".to_owned())
                    .child(Input::new(&self.input).aria_label(NAME_LABEL)),
            )
            .children(notice)
            .child(self.buttons(cx));
        self.popup.keep(popup)
    }
}
