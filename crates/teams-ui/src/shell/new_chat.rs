//! The shell's New message ([`crate::new_chat`]): the list header's button and Ctrl+N open it in the conversation
//! pane, with its row at the top of the chats; a chat it names (chosen from its suggestions, or one a message was just
//! sent to) opens in its place. A 1:1 chat's "Start a group chat" opens it as a new group chat of its people
//! ([`Shell::open_group_draft`]), its row named after them. The read-only mode opens none: it writes nothing.
//!
//! A new chat that the user leaves for another place is kept, as Teams keeps a chat not made yet
//! ([`NewChat::worth_keeping`]):
//! - a new group chat, or New message with people and a draft, stays as a row in the chats under New message's row,
//!   the latest first ([`NewChat::kept_row`]). The row opens it again as it was left. Its "Discard" deletes it and its
//!   draft. Its first message makes the chat, as from New message.
//! - New message with its first message on its way, or given back, or with a draft for nobody yet, has no row: New
//!   message shows it again. A chat made while it is out of sight isn't opened over the place shown.
//! - a draft typed to the chat that the people already have goes to that chat, as Teams opens that chat with it. It
//!   comes after a draft that the chat holds already.
//!
//! The kept new chats live as long as the window: a restart does not keep them, nor the drafts of other chats.

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement as _, ParentElement as _, Subscription, Window,
};

use std::sync::Arc;

use super::{Main, Shell};
use crate::chat_list::{ChatList, ChatRow};
use crate::compose::Compose;
use crate::models::Models;
use crate::new_chat::{NewChat, NewChatEvent, Recipient};
use crate::sidebar::{Sidebar, SidebarEvent};
use crate::teams_list::TeamsList;
use crate::theme::Palette;
use crate::trace::{TraceLine, ViewTrace};

/// A new chat and what it tells.
type Held = (Entity<NewChat>, Subscription);

/// New message, while it is open.
pub(super) type Open = Option<Held>;

/// The new chats kept out of sight, the latest last (see the module docs).
pub(super) type Kept = Vec<Held>;

impl Shell {
    /// The sidebar over the lists, and the subscription by which its New message opens New message and its kept new
    /// chats' rows open or discard them.
    pub(super) fn make_sidebar(
        list: &Entity<ChatList>,
        teams: &Entity<TeamsList>,
        models: &Models,
        trace: &Arc<ViewTrace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<Sidebar>, Subscription) {
        let (list, teams, models, trace) = (list.clone(), teams.clone(), models.clone(), Arc::clone(trace));
        let sidebar = cx.new(|cx| Sidebar::new(list, teams, models, trace, cx));
        let asked = cx.subscribe_in(&sidebar, window, |this: &mut Self, _, event, window, cx| match *event {
            SidebarEvent::NewMessage => this.open_new_chat(window, cx),
            SidebarEvent::OpenKept(id) => this.open_kept_new_chat(id, window, cx),
            SidebarEvent::DiscardKept(id) => this.discard_kept_new_chat(id, cx),
        });
        (sidebar, asked)
    }

    /// Opens New message in the conversation pane, its To: box taking the keyboard; nothing in the read-only mode. New
    /// message kept without a row (see the module docs) is shown again.
    pub fn open_new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.read_only() {
            return;
        }
        if self.main == Main::NewChat
            && let Some((view, _)) = &self.new_chat
            && !view.read(cx).drafting()
            && view.read(cx).kept_row(cx).is_none()
        {
            view.update(cx, |view, cx| view.focus(window, cx));
            return;
        }
        let view = self.show_new_chat(
            |kept, cx| {
                let at = |rowless: &dyn Fn(&NewChat) -> bool| kept.iter().rposition(|(view, _)| rowless(view.read(cx)));
                at(&NewChat::keeps_going).or_else(|| at(&|view| view.kept_row(cx).is_none()))
            },
            window,
            cx,
        );
        view.update(cx, |view, cx| view.focus(window, cx));
        self.finish_opening_new_chat(window, cx);
    }

    /// Back, Forward or the chats' pane again at New message: the new chat last left, its box taking the keyboard,
    /// else New message.
    pub(super) fn resume_new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.main == Main::NewChat || self.kept_new.is_empty() || self.core.read_only() {
            self.open_new_chat(window, cx);
            return;
        }
        let view = self.show_new_chat(|kept, _| kept.len().checked_sub(1), window, cx);
        Self::focus_new_chat_box(&view, window, cx);
        self.finish_opening_new_chat(window, cx);
    }

    /// A new group chat of `people` not made yet, from a 1:1 chat's "Start a group chat" ([`NewChat::start_draft`]),
    /// in New message's place; nothing opens in the read-only mode.
    pub fn open_group_draft(&mut self, people: Vec<Recipient>, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.read_only() {
            return;
        }
        let view = self.show_new_chat(|_, _| None, window, cx);
        view.update(cx, |view, cx| view.start_draft(people, window, cx));
        self.finish_opening_new_chat(window, cx);
    }

    /// New message to `person` alone (a profile card's Chat, or a chat's people, with someone the account has no 1:1
    /// chat with): its To: line holding them, its box taking the keyboard ([`NewChat::start_with`]); nothing opens in
    /// the read-only mode.
    pub fn open_new_chat_with(&mut self, person: Recipient, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.read_only() {
            return;
        }
        let view = self.show_new_chat(|_, _| None, window, cx);
        view.update(cx, |view, cx| view.start_with(person, window, cx));
        self.finish_opening_new_chat(window, cx);
    }

    /// Opens the kept new chat `id` (its entity's) again, as it was left.
    pub fn open_kept_new_chat(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let is = |(view, _): &Held| view.entity_id().as_u64() == id;
        if !self.kept_new.iter().chain(&self.new_chat).any(is) {
            return;
        }
        let view = self.show_new_chat(|kept, _| kept.iter().position(is), window, cx);
        Self::focus_new_chat_box(&view, window, cx);
        self.finish_opening_new_chat(window, cx);
    }

    /// A kept new chat shown again gives its box the keyboard: a focus left on the box of the place it replaced
    /// would reach no handler, not even Back's or Forward's.
    fn focus_new_chat_box(view: &Entity<NewChat>, window: &mut Window, cx: &mut Context<Self>) {
        let compose = view.read(cx).compose(cx);
        compose.update(cx, |compose, cx| compose.focus(window, cx));
    }

    /// Deletes the kept new chat `id` (its entity's) and its draft: its row's "Discard".
    pub fn discard_kept_new_chat(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.main != Main::NewChat {
            self.set_new_chat_aside(cx);
        }
        let before = self.kept_new.len();
        self.kept_new.retain(|(view, _)| view.entity_id().as_u64() != id);
        if self.kept_new.len() != before {
            self.trace.record(&TraceLine::new("new-chat", "discarded"));
            self.sync_new_message_row(cx);
            cx.notify();
        }
    }

    /// The new chats kept out of sight, the latest last: with the one just left, which the next drawing sets aside.
    #[must_use]
    pub fn kept_new_chats(&self, cx: &App) -> Vec<Entity<NewChat>> {
        let left = self
            .new_chat
            .iter()
            .filter(|(view, _)| self.main != Main::NewChat && view.read(cx).worth_keeping(cx));
        self.kept_new.iter().chain(left).map(|(view, _)| view.clone()).collect()
    }

    /// Puts the new chat shown aside (see the module docs), then puts in the conversation pane's place the kept one
    /// that `pick` chooses, else a new one.
    fn show_new_chat(
        &mut self,
        pick: impl FnOnce(&Kept, &App) -> Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<NewChat> {
        self.hide_calendar(window, cx);
        self.put_chat_away(cx);
        self.keep_draft(cx);
        self.people.open = None;
        self.set_new_chat_aside(cx);
        let held = if let Some(at) = pick(&self.kept_new, cx) {
            self.kept_new.remove(at)
        } else {
            self.make_new_chat(window, cx)
        };
        let view = held.0.clone();
        self.new_chat = Some(held);
        view
    }

    fn make_new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Held {
        let (parts, list) = (self.parts(), self.list.clone());
        let view = cx.new(|cx| NewChat::new(parts, list, window, cx));
        let events = cx.subscribe_in(&view, window, |this: &mut Self, view, event, window, cx| match event {
            // a chat made while its new chat is out of sight isn't opened over the place shown
            NewChatEvent::Open(id) if this.new_chat().is_some_and(|shown| shown == view) => {
                this.open_chat(id.clone(), window, cx);
            }
            NewChatEvent::Open(_) => cx.notify(),
            NewChatEvent::DraftChanged => this.sync_new_message_row(cx),
        });
        (view, events)
    }

    /// Keeps the new chat shown, if it is worth keeping, or gives the draft it typed to a chat the people have to
    /// that chat ([`Self::hand_found_draft`]).
    pub(super) fn set_new_chat_aside(&mut self, cx: &mut App) {
        if let Some(held) = self.new_chat.take() {
            self.keep_new_chat(held, cx);
        }
    }

    fn keep_new_chat(&mut self, (view, events): Held, cx: &mut App) {
        if view.read(cx).worth_keeping(cx) {
            self.kept_new.push((view, events));
        } else if let Some((chat, compose)) = view.read(cx).found_draft(cx) {
            self.hand_found_draft(chat, &compose, cx);
        }
    }

    /// The draft typed to `chat` in New message and the draft of that chat become one draft in one box. The box of
    /// the chat is the box of its view while that view is shown or holds a draft, else its kept draft. The full draft
    /// (text, pictures, files, reply, and the answers on their way) moves there with [`Compose::hand_draft_to`], after
    /// the draft of the chat, so no draft is lost. With no draft in the chat, `compose` becomes the kept draft of the
    /// chat, and the chat opens with it.
    fn hand_found_draft(&mut self, chat: String, compose: &Entity<Compose>, cx: &mut App) {
        let shown = self.main == Main::Chat(chat.clone());
        let own = self
            .chat
            .as_ref()
            .map(|view| view.read(cx))
            .filter(|view| view.conversation() == chat)
            .map(|view| view.compose().clone())
            .filter(|own| shown || own.read(cx).has_draft(cx))
            .or_else(|| {
                self.drafts
                    .get(&chat)
                    .filter(|kept| kept.read(cx).has_draft(cx))
                    .cloned()
            });
        match own {
            Some(own) if own != *compose => compose.update(cx, |compose, cx| compose.hand_draft_to(&own, cx)),
            _ => {
                self.drafts.insert(chat, compose.clone());
            }
        }
    }

    fn finish_opening_new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.list.update(cx, |list, cx| list.select(None, cx));
        self.teams.update(cx, |teams, cx| teams.select(None, cx));
        self.main = Main::NewChat;
        self.close_switcher(window, cx);
        self.close_search(cx);
        self.sync_new_message_row(cx);
        cx.notify();
    }

    /// New message, while it is the place shown.
    #[must_use]
    pub fn new_chat(&self) -> Option<&Entity<NewChat>> {
        self.new_chat
            .as_ref()
            .filter(|_| self.main == Main::NewChat)
            .map(|(view, _)| view)
    }

    /// The conversation pane while New message is the place shown.
    pub(super) fn new_chat_pane(&self, pane: gpui::Div, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        match &self.new_chat {
            Some((view, _)) => pane.child(view.clone()).into_any_element(),
            None => pane.child(super::no_chat(palette, cx)).into_any_element(),
        }
    }

    /// The sidebar's rows of the new chats follow them: New message's row while it is shown (a new group chat's or a
    /// new chat's named after it), and a row for each one kept. A new chat left is set aside; a kept one that is no
    /// longer worth keeping (its first message sent) goes.
    pub(super) fn sync_new_message_row(&mut self, cx: &mut Context<Self>) {
        let open = self.main == Main::NewChat;
        if !open {
            self.set_new_chat_aside(cx);
        }
        for held in std::mem::take(&mut self.kept_new) {
            self.keep_new_chat(held, cx);
        }
        if self.sidebar.read(cx).new_message_open() != open {
            self.sidebar.update(cx, |sidebar, cx| sidebar.set_new_message(open, cx));
        }
        let shown = self.new_chat.as_ref().filter(|_| open).and_then(|(view, _)| {
            let view = view.read(cx);
            view.draft_row().or_else(|| view.kept_row(cx))
        });
        if self.sidebar.read(cx).new_message_draft() != shown.as_ref() {
            self.sidebar
                .update(cx, |sidebar, cx| sidebar.set_new_message_draft(shown, cx));
        }
        let kept: Vec<(u64, ChatRow)> = self
            .kept_new
            .iter()
            .rev()
            .filter_map(|(view, _)| Some((view.entity_id().as_u64(), view.read(cx).kept_row(cx)?)))
            .collect();
        if self.sidebar.read(cx).kept_new_chats() != kept.as_slice() {
            self.sidebar
                .update(cx, |sidebar, cx| sidebar.set_kept_new_chats(kept, cx));
        }
    }
}
