//! The chat header's "Name group chat" ([`crate::chat_rename`]): a group chat's header shows a pencil after its name,
//! and the pencil opens the popup under the name, or closes it. A 1:1 chat, a meeting's chat and the read-only mode
//! show none. Another chat, a channel or another page opened closes the popup (each page switch puts the chat away). A
//! popup closed while its save is on its way is kept out of view until the save ends, and a failed save shows as a toast.

use super::*;
use crate::chat_list::RowKind;
use crate::chat_people::Placed;
use crate::chat_rename::{ChatRename, ChatRenameEvent, ChatRenameParts, EDGE, NAME_TIP, NOT_SAVED};
use gpui_component::Selectable as _;

/// The popup while it is open, the popups closed while their save was on its way, and where the header's pencil was
/// last drawn.
#[derive(Default)]
pub(super) struct State {
    open: Option<(Entity<ChatRename>, Subscription)>,
    /// Not drawn: each is kept until its save ends, so that a failure is still told ([`ChatRenameEvent::NotSaved`]).
    closed: Vec<(Entity<ChatRename>, Subscription)>,
    button: Placed,
}

impl State {
    /// Closes the popup, if it is open. One whose save is on its way is kept out of view until the save ends.
    pub(super) fn close(&mut self, cx: &App) {
        if let Some(open) = self.open.take()
            && open.0.read(cx).saving()
        {
            self.closed.push(open);
        }
    }

    /// Takes `view` out: whether it was the open popup (not one closed before).
    fn forget(&mut self, view: &Entity<ChatRename>) -> bool {
        if self.open.as_ref().is_some_and(|(open, _)| open == view) {
            self.open = None;
            return true;
        }
        self.closed.retain(|(closed, _)| closed != view);
        false
    }
}

impl Shell {
    /// The header's pencil for chat `id`: a group chat's only, and none in the read-only mode.
    pub(super) fn rename_header_button(&self, id: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        let row = self.list.read(cx).row(id)?;
        if row.kind != RowKind::Group || self.core.read_only() {
            return None;
        }
        let button = icon_button("chat-header-rename", Glyph::PENCIL, NAME_TIP)
            .small()
            .selected(self.rename.open.is_some())
            .on_click(cx.listener(|this, _, window, cx| this.toggle_rename(window, cx)));
        Some(
            self.rename
                .button
                .keep(selected_by("chat-header-rename", button))
                .into_any_element(),
        )
    }

    /// Opens the chat shown's "Name group chat", or closes it.
    pub fn toggle_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename.open.is_some() {
            self.rename.close(cx);
            self.take_focus(window, cx);
            cx.notify();
            return;
        }
        let Main::Chat(id) = &self.main else { return };
        let Some(row) = self.list.read(cx).row(id) else { return };
        if row.kind != RowKind::Group || self.core.read_only() {
            return;
        }
        // a group chat without a name is called by its people: its box starts empty
        let topic = if row.named && row.chat_with.is_none() {
            row.name.to_string()
        } else {
            String::new()
        };
        let parts = ChatRenameParts {
            core: Arc::clone(&self.core),
            trace: Arc::clone(&self.trace),
            button: self.rename.button.clone(),
        };
        let chat = id.clone();
        let view = cx.new(|cx| ChatRename::new(parts, chat, &topic, window, cx));
        let events = cx.subscribe_in(
            &view,
            window,
            |this: &mut Self, view, event: &ChatRenameEvent, window, cx| {
                match event {
                    ChatRenameEvent::Close => {
                        if this.rename.forget(view) {
                            this.take_focus(window, cx);
                        }
                    }
                    // the popup shown says so itself; one closed before, or not shown, is told as a toast
                    ChatRenameEvent::NotSaved => {
                        let shown = this.rename.open.as_ref().is_some_and(|(open, _)| open == view)
                            && matches!(&this.main, Main::Chat(id) if id.as_str() == view.read(cx).chat())
                            && !this.signed_out_shown(cx);
                        if !shown {
                            this.rename.forget(view);
                            this.show_toast(NOT_SAVED, cx);
                        }
                    }
                }
                cx.notify();
            },
        );
        view.update(cx, |view, cx| view.focus(window, cx));
        self.rename.open = Some((view, events));
        cx.notify();
    }

    /// The chat's "Name group chat" popup, while it is open.
    #[must_use]
    pub fn rename(&self) -> Option<&Entity<ChatRename>> {
        self.rename.open.as_ref().map(|(view, _)| view)
    }

    /// The popup over the conversation pane, under the chat's name, while it is the chat shown's; moved left as far as
    /// the window's right edge needs, as Teams keeps it in the window.
    pub(super) fn rename_layer(&self, id: &str, cx: &App) -> Option<AnyElement> {
        let (view, _) = self.rename.open.as_ref()?;
        (view.read(cx).chat() == id).then(|| {
            let popup = gpui::anchored()
                .snap_to_window_with_margin(px(EDGE))
                .child(view.clone());
            gpui::deferred(div().absolute().top(px(42.0)).left(px(4.0)).child(popup))
                .with_priority(1)
                .into_any_element()
        })
    }
}
