//! The buttons of a message's hover bar, as the user chooses them ([`BarItem`], kept for each account in the settings).
//! A right click on the bar, or "Customize" at the foot of "More options", opens the list of the buttons that can
//! show or hide: the quick reactions on the bar now, each by the reaction itself, "React", Edit, Reply, Copy text,
//! Delete, Mark as unread and Translate. "More options" is listed, and cannot hide. It is the message box's list
//! ([`crate::customize_list`]).
//!
//! A button that does not show has its row in "More options", and its key still acts. A button that shows but does not
//! apply to a message (Edit on someone else's message, Reply in a thread) is not on that message's bar.

use gpui::{AnyElement, App, Bounds, Context, IntoElement as _, Pixels, Point, SharedString, Styled as _, Window, px};

use super::{ChatView, Entry, MessageAction, menu_entry_parts};
use crate::customize_list::{self, Customizable, CustomizeList, Line, Shown};
use crate::icons::Glyph;
use crate::keyboard;
use crate::reactions;
use crate::settings::BarItem;
use crate::theme::Palette;

/// The list's title.
pub const SHOW_IN_BAR: &str = "Show in the message bar";
/// The row at the foot of "More options" that opens the list.
pub const CUSTOMIZE: &str = "Customize";

/// The list while it is open: on which message's bar, and where the focus goes back to.
pub(super) struct BarCustomize {
    pub(super) list: CustomizeList,
    /// The rows as the list opened: a click or a key resolves its row against what the list shows.
    rows: Vec<Option<BarItem>>,
    /// The message whose bar stays shown while the list is open.
    pub(super) message: String,
    back: keyboard::Return,
}

/// The rows of the list: the quick reactions in their order now, the other buttons that can hide, and "More options"
/// (`None`).
fn rows(quick: &[String]) -> Vec<Option<BarItem>> {
    quick
        .iter()
        .filter_map(|key| BarItem::reaction(key))
        .chain(BarItem::ACTIONS)
        .map(Some)
        .chain([None])
        .collect()
}

/// The action a button does, for its icon, its name and its key; `None`: a quick reaction or Translate.
fn action_of(item: BarItem) -> Option<MessageAction> {
    match item {
        BarItem::React => Some(MessageAction::React),
        BarItem::Edit => Some(MessageAction::Edit),
        BarItem::Reply => Some(MessageAction::Reply),
        BarItem::Copy => Some(MessageAction::Copy),
        BarItem::Delete => Some(MessageAction::Delete),
        BarItem::MarkUnread => Some(MessageAction::MarkUnread),
        BarItem::Reaction(..) | BarItem::Translate => None,
    }
}

/// The action `item` does on a message that offers `actions`, when it applies there: Reply is "Reply in thread" on a
/// channel's post. `None` also for a quick reaction or Translate.
pub(super) fn bar_action(item: BarItem, actions: &[MessageAction]) -> Option<MessageAction> {
    let action = match item {
        BarItem::Reply => [MessageAction::Reply, MessageAction::ReplyInThread]
            .into_iter()
            .find(|action| actions.contains(action)),
        other => action_of(other),
    };
    action.filter(|action| actions.contains(action))
}

/// A button's icon and name, as its row in the list and in "More options" shows it.
fn bar_item_face(item: BarItem) -> (AnyElement, SharedString) {
    match item {
        BarItem::Reaction(..) => {
            let key = item.reaction_key().unwrap_or_default();
            (
                reactions::emoji(&key, px(14.0)),
                reactions::description(&key).to_owned().into(),
            )
        }
        BarItem::Translate => (
            Glyph::LANGUAGES.icon().size(px(14.0)).into_any_element(),
            "Translate".into(),
        ),
        item => {
            let action = action_of(item).unwrap_or(MessageAction::React);
            (
                action.glyph().icon().size(px(14.0)).into_any_element(),
                action.label().into(),
            )
        }
    }
}

impl ChatView {
    /// Whether the account's hover bar shows `item`.
    pub(super) fn bar_shows(&self, item: BarItem, cx: &App) -> bool {
        self.settings.read(cx).settings().shows_bar_item(self.core.me(), item)
    }

    /// Opens the list by a right click at `at` on `message`'s bar; "More options" closes.
    pub fn open_bar_customize(
        &mut self,
        message: &str,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_bar_customize_with(message, at, gpui::Anchor::TopLeft, window, cx);
    }

    /// "Customize" in `message`'s "More options": the menu closes, and the list opens where it was.
    pub fn customize_bar(&mut self, message: &str, window: &mut Window, cx: &mut Context<Self>) {
        let at = self
            .options_at
            .get()
            .map_or_else(Point::default, |bounds: Bounds<Pixels>| bounds.bottom_right());
        self.open_bar_customize_with(message, at, gpui::Anchor::TopRight, window, cx);
    }

    fn open_bar_customize_with(
        &mut self,
        message: &str,
        at: Point<Pixels>,
        anchor: gpui::Anchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.core.read_only() {
            return;
        }
        let back = if let Some(open) = self.bar_customize.take() {
            open.back
        } else {
            self.customize_return(window, cx)
        };
        // a right click on the open menu's own button is not a click outside it: the menu closes here
        self.options = None;
        self.side_list = None;
        self.bar_customize = Some(BarCustomize {
            list: CustomizeList::open(at, anchor, window, cx),
            rows: rows(&self.quick),
            message: message.to_owned(),
            back,
        });
        cx.notify();
    }

    /// Where the keys go back to when the list closes.
    fn customize_return(&self, window: &mut Window, cx: &mut Context<Self>) -> keyboard::Return {
        // the right click dismisses "more reactions": the keys must not go back to its search field
        if self
            .more_reactions()
            .is_some_and(|picker| picker.read(cx).holds_focus(window, cx))
        {
            self.focus.handle.focus(window, cx);
        }
        self.menu_keys.focus_return(&self.focus.handle, window, cx)
    }

    /// Shows `item` on the account's hover bar, or hides it in "More options", as the list does.
    pub fn show_bar_item(&mut self, item: BarItem, shown: bool, cx: &mut Context<Self>) {
        let account = self.core.me().to_owned();
        self.settings.update(cx, |model, cx| {
            model.change(|settings| settings.show_bar_item(&account, item, shown), cx);
        });
        cx.notify();
    }

    /// Whether the list is open.
    #[must_use]
    pub fn bar_customize_open(&self) -> bool {
        self.bar_customize.is_some()
    }

    /// Closes the list, the focus back where it was.
    pub fn close_bar_customize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(open) = self.bar_customize.take() {
            open.back.give_back(window, cx);
            cx.notify();
        }
    }

    /// The list over the window, where it opened (`message-customize-menu`): a row with a check for each button that
    /// can hide (the quick reactions in their order when it opened), then "More options", checked and off.
    pub(super) fn bar_customize_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.bar_customize.as_ref()?;
        let lines = open
            .rows
            .iter()
            .copied()
            .map(|item| {
                let (key, icon, label): (SharedString, AnyElement, SharedString) = match item {
                    Some(item) => {
                        let (icon, label) = bar_item_face(item);
                        (item.key().into_owned().into(), icon, label)
                    }
                    None => (
                        "more".into(),
                        Glyph::MORE.icon().size(px(14.0)).into_any_element(),
                        "More options".into(),
                    ),
                };
                Line {
                    key,
                    icon,
                    label,
                    checked: item.is_none_or(|item| self.bar_shows(item, cx)),
                    takes: item.is_some(),
                }
            })
            .collect();
        let shown = Shown {
            title: SHOW_IN_BAR,
            prefix: "message-customize",
            lines,
        };
        Some(customize_list::overlay(&open.list, shown, cx))
    }

    /// The rows of `message`'s "More options" for its quick reactions that the bar does not show: each sets the
    /// account's own, or takes it back.
    pub(super) fn hidden_reaction_entries(
        &self,
        message: &str,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        self.quick
            .iter()
            .filter_map(|key| {
                let item = BarItem::reaction(key)?;
                if self.bar_shows(item, cx) {
                    return None;
                }
                let key = key.clone();
                let (icon, label) = bar_item_face(item);
                let message = message.to_owned();
                let id = SharedString::from(format!("menu-pick-{key}"));
                Some(menu_entry_parts(
                    Entry::plain(id, (icon, label), palette),
                    palette,
                    cx,
                    move |this, _, cx| {
                        this.options = None;
                        this.toggle_reaction(&message, &key, cx);
                    },
                ))
            })
            .collect()
    }

    /// "Customize" at the foot of `message`'s "More options".
    pub(super) fn customize_entry(
        &self,
        message: &str,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.core.read_only() {
            return None;
        }
        let message = message.to_owned();
        let face = (
            Glyph::SETTINGS.icon().size(px(14.0)).into_any_element(),
            CUSTOMIZE.into(),
        );
        Some(menu_entry_parts(
            Entry::plain("action-customize".into(), face, palette),
            palette,
            cx,
            move |this, window, cx| this.customize_bar(&message, window, cx),
        ))
    }
}

impl Customizable for ChatView {
    fn customize_list(&mut self) -> Option<&mut CustomizeList> {
        self.bar_customize.as_mut().map(|open| &mut open.list)
    }

    fn customize_takes(&self, _: &App) -> Vec<bool> {
        self.bar_customize
            .as_ref()
            .map(|open| open.rows.iter().map(Option::is_some).collect())
            .unwrap_or_default()
    }

    fn customize_toggle(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self
            .bar_customize
            .as_ref()
            .and_then(|open| open.rows.get(ix).copied().flatten())
        else {
            return;
        };
        let shown = self.bar_shows(item, cx);
        self.show_bar_item(item, !shown, cx);
    }

    fn customize_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_bar_customize(window, cx);
    }
}
