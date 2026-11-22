//! The keyboard in a message's "More options" menu, as Teams' context menu takes it: Shift+F10 or the menu key opens
//! the menu of the selected message, its first row under the keys. Up and Down move through the rows of the menu or of
//! the list beside it, Home and End to the ends, Enter or Space picks. Right on "Translation" or "More actions" opens
//! its list, its first row under the keys; Left or Escape there closes the list and goes back to its row. Escape, Tab
//! or Shift+Tab in the menu closes it. The focus goes back to the message list when the menu closes from the keys, or
//! when a row picked from the keys leaves it nowhere.
//!
//! The kit's root binds Tab and Shift+Tab, and a binding takes a key before any key listener hears it: the menu's own
//! key context ([`CONTEXT`]) binds them to [`LeaveMenu`], which takes precedence over the root's.
//!
//! A menu taller than the window scrolls. The focus moving by the keys does not scroll it, so each of its rows sits in
//! a slot with its own focus handle: the slot that holds the focus is the row the menu scrolls to.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, Context, Div, FocusHandle, InteractiveElement as _, IntoElement as _, KeyBinding,
    KeyDownEvent, ParentElement as _, Pixels, ScrollHandle, Styled as _, Window, actions, div,
};

use super::{ChatView, MessageAction, SideList};
use crate::keyboard;

actions!(teams_ui, [LeaveMenu]);

/// The key context of the menu and the list beside it.
pub(super) const CONTEXT: &str = "MessageMenu";

/// Binds Tab and Shift+Tab in the menu ([`CONTEXT`]): they close it.
pub(super) fn bind(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("tab", LeaveMenu, context),
        KeyBinding::new("shift-tab", LeaveMenu, context),
    ]);
}

/// Where the keys go once the menu or its list is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Due {
    Menu,
    Side,
}

/// The menu's focus handles and where the focus goes back to.
pub(super) struct MenuKeys {
    /// The menu's panel.
    pub(super) menu: FocusHandle,
    /// The list beside it, while one is open.
    pub(super) side: FocusHandle,
    /// Where that list ("More actions" or "Translation") was last drawn: a click in it is not a click outside the
    /// menu.
    pub(super) side_at: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The rows that open a list beside the menu.
    translation: FocusHandle,
    more_actions: FocusHandle,
    /// The menu's scroll, when it is taller than the window.
    pub(super) scroll: ScrollHandle,
    /// The slots of the menu's rows, in their order.
    slots: RefCell<Vec<FocusHandle>>,
    back: keyboard::Return,
    due: Option<Due>,
    /// The keys opened the menu: it gives the focus back when it closes.
    keyed: bool,
}

impl MenuKeys {
    pub(super) fn new(cx: &App) -> Self {
        Self {
            menu: cx.focus_handle(),
            side: cx.focus_handle(),
            side_at: Rc::default(),
            translation: cx.focus_handle(),
            more_actions: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            slots: RefCell::default(),
            back: keyboard::Return::default(),
            due: None,
            keyed: false,
        }
    }

    /// The focus of the row that opens `list`.
    pub(super) fn opener(&self, list: SideList) -> &FocusHandle {
        match list {
            SideList::Translation => &self.translation,
            SideList::MoreActions => &self.more_actions,
        }
    }

    /// Where the focus goes back to from a popup that a row of the menu opens: where the menu's keys go back to when
    /// the keys opened it, else where the focus is now.
    pub(super) fn focus_return(&self, window: &Window, cx: &App) -> keyboard::Return {
        if self.keyed {
            return self.back.clone();
        }
        let back = keyboard::Return::default();
        back.keep(window, cx);
        back
    }

    /// `entries` each in its slot, as the menu holds them.
    pub(super) fn slotted(&self, entries: Vec<AnyElement>, cx: &App) -> Vec<AnyElement> {
        let mut slots = self.slots.borrow_mut();
        while slots.len() < entries.len() {
            slots.push(cx.focus_handle());
        }
        entries
            .into_iter()
            .zip(slots.iter())
            .map(|(entry, slot)| {
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .track_focus(slot)
                    .child(entry)
                    .into_any_element()
            })
            .collect()
    }

    /// Scrolls the menu to the row that holds the focus.
    fn reveal_focused(&self, window: &Window, cx: &App) {
        let slots = self.slots.borrow();
        if let Some(at) = slots.iter().position(|slot| slot.contains_focused(window, cx)) {
            self.scroll.scroll_to_item(at);
        }
    }

    fn opened_by(&self, window: &Window) -> Option<SideList> {
        [SideList::Translation, SideList::MoreActions]
            .into_iter()
            .find(|list| self.opener(*list).is_focused(window))
    }
}

impl ChatView {
    /// Shift+F10 or the menu key on the selected message: its menu opens with the keys in it.
    pub(super) fn open_selected_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(message) = self.selected.clone() else {
            return;
        };
        let offered = self
            .message_items()
            .any(|item| item.id == message && item.actions().contains(&MessageAction::Copy));
        if !offered {
            return;
        }
        if self.options.as_deref() != Some(message.as_str()) {
            self.toggle_options(&message, cx);
        }
        self.side_list = None;
        self.menu_keys.back.keep(window, cx);
        self.menu_keys.keyed = true;
        self.menu_keys.due = Some(Due::Menu);
        cx.notify();
    }

    /// A key in the menu or in the list beside it.
    fn menu_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keys = &self.menu_keys;
        if event.keystroke.modifiers.modified() {
            return;
        }
        let in_side = self.side_list.is_some() && keys.side.contains_focused(window, cx);
        let root = if in_side { keys.side.clone() } else { keys.menu.clone() };
        match event.keystroke.key.as_str() {
            "down" | "up" => {
                keyboard::move_within(&root, event.keystroke.key == "down", window, cx);
                self.reveal_menu_row(in_side, window, cx);
            }
            "home" | "end" => {
                root.focus(window, cx);
                keyboard::move_within(&root, event.keystroke.key == "home", window, cx);
                self.reveal_menu_row(in_side, window, cx);
            }
            "right" if !in_side => {
                let Some(list) = keys.opened_by(window) else { return };
                let Some(message) = self.options.clone() else { return };
                self.side_list = Some((list, message));
                self.menu_keys.due = Some(Due::Side);
                cx.notify();
            }
            "left" | "escape" if in_side => {
                if let Some((list, _)) = self.side_list.take() {
                    self.menu_keys.opener(list).clone().focus(window, cx);
                }
                cx.notify();
            }
            "escape" => self.close_menu_by_keys(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The row the keys moved to in the menu (not `in_side`, the list beside it) scrolls into view.
    fn reveal_menu_row(&self, in_side: bool, window: &Window, cx: &mut Context<Self>) {
        if !in_side {
            self.menu_keys.reveal_focused(window, cx);
            cx.notify();
        }
    }

    /// The menu's keys on `holder`, which holds the menu and the list beside it.
    pub(super) fn menu_keys_on(holder: Div, cx: &mut Context<Self>) -> Div {
        holder
            .key_context(CONTEXT)
            .on_key_down(cx.listener(Self::menu_key))
            .on_action(cx.listener(Self::leave_menu))
    }

    /// Tab or Shift+Tab in the menu or in the list beside it ([`LeaveMenu`]).
    fn leave_menu(&mut self, _: &LeaveMenu, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu_by_keys(window, cx);
    }

    /// Closes the menu and its list, and gives the focus back to where it was when the menu opened.
    fn close_menu_by_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.options = None;
        self.side_list = None;
        self.menu_keys.due = None;
        self.menu_keys.keyed = false;
        self.menu_keys.back.give_back(window, cx);
        cx.notify();
    }

    /// A list opener's row picked from the keys: its list's first row takes them once it is drawn.
    pub(super) fn side_list_keyed(&mut self) {
        if self.side_list.is_some() {
            self.menu_keys.due = Some(Due::Side);
        }
    }

    /// Moves the keys to where they are due once the menu (`drawn`) or its list is drawn (after the frame in which it
    /// is); when the menu is gone and the keys opened it, gives the focus back to the message list if nothing else took it.
    pub(super) fn settle_menu_keys(&mut self, drawn: bool, window: &Window, cx: &mut App) {
        let keys = &mut self.menu_keys;
        if self.options.is_none() {
            keys.due = None;
            if std::mem::take(&mut keys.keyed) {
                let (back, menu, side) = (keys.back.clone(), keys.menu.clone(), keys.side.clone());
                window.defer(cx, move |window, cx| {
                    give_back_if_lost(&back, [&menu, &side], window, cx);
                });
            }
            return;
        }
        if !drawn {
            return;
        }
        let root = match keys.due {
            Some(Due::Menu) => keys.menu.clone(),
            Some(Due::Side) if self.side_list.is_some() => keys.side.clone(),
            _ => return,
        };
        keys.due = None;
        window.defer(cx, move |window, cx| {
            root.focus(window, cx);
            keyboard::move_within(&root, true, window, cx);
        });
    }
}

/// Gives the focus back to `back` when what a row picked from the keys did left it on nothing, or on one of the
/// menu's `roots`.
fn give_back_if_lost(back: &keyboard::Return, roots: [&FocusHandle; 2], window: &mut Window, cx: &mut App) {
    let lost = window.focused(cx).is_none_or(|focused| roots.contains(&&focused));
    if lost {
        back.give_back(window, cx);
    }
}
