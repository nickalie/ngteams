//! The keyboard's reach into controls drawn as plain `div`s, the way Teams' own are reached:
//!
//! - [`stop`]: a clickable `div` made a tab stop, lit while the keyboard is on it; Enter and Space click it (GPUI's
//!   keyboard click on a focused element).
//! - [`Handles`]: focus handles kept by key, for a group whose focus the view moves itself: a tab list or a radio group
//!   ([`step`]: Left/Up and Right/Down move and choose, as Teams' tabs and radio cards do; only the chosen one is a tab
//!   stop, so Tab passes the group in one step).
//! - [`menu_keys`]: a popup menu of focusable rows (the rows are [`stop`]s): Up and Down move through them, Home and End
//!   to the first and the last, Enter or Space picks, Escape and Tab close it. The opener keeps where the focus was
//!   ([`Return`]) and gives it back on close, as Teams returns it to what opened the menu.
//! - [`submenu_escape`]: Escape in one of the kit's submenus closes that submenu alone and gives the keyboard back to
//!   the menu it opened from, as Teams' menus do (the kit closes every level at once).
//!
//! Tab and Shift+Tab are the kit's root's ([`gpui_component::Root`]): a dialog traps them with the kit's `focus_trap`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ElementId, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent, KeystrokeEvent, RenderOnce,
    Stateful, Styled as _, Window, rgb,
};

use crate::theme::Palette;

/// How a stop shows that the keyboard is on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ring {
    /// Its background lights, as a hovered row's does.
    Fill,
    /// Its border takes the brand colour: a card or a chip, which has one.
    Edge,
}

/// A clickable `div` the keyboard reaches ([`stop`]).
#[derive(IntoElement)]
pub struct Stop {
    key: ElementId,
    element: Stateful<gpui::Div>,
    palette: Palette,
    ring: Ring,
    handle: Option<FocusHandle>,
    takes_tab: bool,
}

/// `element` as a tab stop keyed by `key` (unique among its siblings), lit while focused from the keyboard.
pub fn stop(key: impl Into<ElementId>, element: Stateful<gpui::Div>, palette: Palette) -> Stop {
    Stop {
        key: key.into(),
        element,
        palette,
        ring: Ring::Fill,
        handle: None,
        takes_tab: true,
    }
}

impl Stop {
    /// Rings its border instead of filling it.
    #[must_use]
    pub fn edge(mut self) -> Self {
        self.ring = Ring::Edge;
        self
    }

    /// Tracks `handle`, which the view moves the focus to itself, instead of one of its own.
    #[must_use]
    pub fn handle(mut self, handle: &FocusHandle) -> Self {
        self.handle = Some(handle.clone());
        self
    }

    /// Whether Tab stops on it (the chosen one of a group does, the others don't).
    #[must_use]
    pub fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.takes_tab = tab_stop;
        self
    }
}

impl RenderOnce for Stop {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            key,
            element,
            palette,
            ring,
            handle,
            takes_tab,
        } = self;
        let handle = handle
            .unwrap_or_else(|| {
                window
                    .use_keyed_state(key, cx, |_, cx| cx.focus_handle())
                    .read(cx)
                    .clone()
            })
            .tab_stop(takes_tab);
        element.track_focus(&handle).map(|element| match ring {
            Ring::Fill => element.focus_visible(move |style| style.bg(rgb(palette.hover))),
            Ring::Edge => element.focus_visible(move |style| style.border_color(rgb(palette.brand))),
        })
    }
}

/// The kit's popup menu's key context.
const POPUP_MENU: &str = "PopupMenu";

/// Makes Escape in a submenu go back to the menu it opened from instead of closing every level; once, at start.
pub fn submenu_escape(cx: &mut App) {
    cx.intercept_keystrokes(escape_in_submenu).detach();
}

fn escape_in_submenu(event: &KeystrokeEvent, window: &mut Window, cx: &mut App) {
    let menus = event
        .context_stack
        .iter()
        .filter(|context| context.contains(POPUP_MENU))
        .count();
    if event.keystroke.key != "escape" || event.keystroke.modifiers.modified() || menus < 2 {
        return;
    }
    let Some(submenu) = window.focused(cx) else { return };
    cx.stop_propagation();
    // the kit's arrow toward the parent goes back; which arrow that is depends on the side the submenu opened on
    for back in ["ui::SelectLeft", "ui::SelectRight"] {
        if !submenu.contains_focused(window, cx) {
            return;
        }
        submenu.focus(window, cx);
        if let Ok(action) = cx.build_action(back, None) {
            submenu.dispatch_action(action.as_ref(), window, cx);
        }
    }
}

/// Focus handles a view keeps by key, made the first time each is asked for.
#[derive(Default)]
pub struct Handles(RefCell<HashMap<String, FocusHandle>>);

impl Handles {
    /// The handle kept for `key`.
    pub fn get(&self, key: &str, cx: &App) -> FocusHandle {
        self.0
            .borrow_mut()
            .entry(key.to_owned())
            .or_insert_with(|| cx.focus_handle())
            .clone()
    }
}

/// Where an arrow key moves in a group of `count` from `at` (Left and Up back, Right and Down on, wrapping; Home and End
/// to the ends); `None` for any other key.
#[must_use]
pub fn step(event: &KeyDownEvent, count: usize, at: usize) -> Option<usize> {
    if count == 0 || event.keystroke.modifiers.modified() {
        return None;
    }
    let last = count - 1;
    Some(match event.keystroke.key.as_str() {
        "left" | "up" => {
            if at == 0 {
                last
            } else {
                at - 1
            }
        }
        "right" | "down" => {
            if at >= last {
                0
            } else {
                at + 1
            }
        }
        "home" => 0,
        "end" => last,
        _ => return None,
    })
}

/// Moves the focus to the next (or previous) tab stop inside `root`, wrapping round inside it. The tab order is the
/// window's, so it goes on until it comes back inside; a root with no stop keeps the focus itself.
pub fn move_within(root: &FocusHandle, forward: bool, window: &mut Window, cx: &mut App) {
    let started = window.focused(cx);
    // at most once round the window's stops
    for _ in 0..1024 {
        if forward {
            window.focus_next(cx);
        } else {
            window.focus_prev(cx);
        }
        if root.contains_focused(window, cx) && !root.is_focused(window) {
            return;
        }
        if window.focused(cx) == started {
            break;
        }
    }
    root.focus(window, cx);
}

/// Where the focus was when a menu opened, given back when it closes.
#[derive(Clone, Default)]
pub struct Return(Rc<RefCell<Option<FocusHandle>>>);

impl Return {
    /// Keeps where the focus is now.
    pub fn keep(&self, window: &Window, cx: &App) {
        *self.0.borrow_mut() = window.focused(cx);
    }

    /// Gives the focus back to where it was kept, if anywhere.
    pub fn give_back(&self, window: &mut Window, cx: &mut App) {
        if let Some(handle) = self.0.borrow_mut().take() {
            handle.focus(window, cx);
        }
    }
}

/// A view's popup's keyboard: the popup's root takes the focus when it opens ([`Self::opened`]), and the focus goes back
/// to where it was when it closes ([`Self::closed`]).
pub struct PopupFocus {
    root: FocusHandle,
    back: Return,
}

impl PopupFocus {
    pub fn new(cx: &App) -> Self {
        Self {
            root: cx.focus_handle(),
            back: Return::default(),
        }
    }

    /// The popup's root, which its rows' keys bubble to.
    #[must_use]
    pub fn root(&self) -> &FocusHandle {
        &self.root
    }

    /// A popup opened: the keyboard goes to it.
    pub fn opened(&self, window: &mut Window, cx: &mut App) {
        if !self.root.contains_focused(window, cx) {
            self.back.keep(window, cx);
        }
        self.root.focus(window, cx);
    }

    /// The popup closed: the keyboard goes back to what opened it.
    pub fn closed(&self, window: &mut Window, cx: &mut App) {
        self.back.give_back(window, cx);
    }
}

/// A popup menu's keys on its root `root` (see the module docs); `close` closes it, the focus given back by the caller.
pub fn menu_keys(
    root: FocusHandle,
    close: impl Fn(&mut Window, &mut App) + 'static,
) -> impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static {
    move |event, window, cx| {
        if event.keystroke.modifiers.modified() && event.keystroke.key != "tab" {
            return;
        }
        match event.keystroke.key.as_str() {
            "down" => move_within(&root, true, window, cx),
            "up" => move_within(&root, false, window, cx),
            "home" => {
                root.focus(window, cx);
                move_within(&root, true, window, cx);
            }
            "end" => {
                root.focus(window, cx);
                move_within(&root, false, window, cx);
            }
            "escape" | "tab" => close(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

/// A list of focusable rows' keys on its root `root` (people found under a box): Up and Down move through them, Home
/// and End to the ends; Tab goes on past them as it does anywhere.
pub fn list_keys(root: FocusHandle) -> impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static {
    move |event, window, cx| {
        if event.keystroke.modifiers.modified() {
            return;
        }
        match event.keystroke.key.as_str() {
            "down" => move_within(&root, true, window, cx),
            "up" => move_within(&root, false, window, cx),
            "home" => {
                root.focus(window, cx);
                move_within(&root, true, window, cx);
            }
            "end" => {
                root.focus(window, cx);
                move_within(&root, false, window, cx);
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}

/// Whether the key opens a focused row's context menu: Shift+F10 or the menu key, as in Teams.
#[must_use]
pub fn opens_menu(event: &KeyDownEvent) -> bool {
    let keystroke = &event.keystroke;
    (keystroke.key == "f10" && keystroke.modifiers.shift && !keystroke.modifiers.control && !keystroke.modifiers.alt)
        || (keystroke.key == "menu" && !keystroke.modifiers.modified())
}

/// A list's cursor moved by Up, Down, Home and End over `count` rows, not wrapping; `None` for any other key.
#[must_use]
pub fn list_step(event: &KeyDownEvent, count: usize, at: Option<usize>) -> Option<usize> {
    if count == 0 || event.keystroke.modifiers.modified() {
        return None;
    }
    let last = count - 1;
    Some(match event.keystroke.key.as_str() {
        "down" => at.map_or(0, |at| (at + 1).min(last)),
        "up" => at.map_or(0, |at| at.saturating_sub(1)),
        "home" => 0,
        "end" => last,
        _ => return None,
    })
}

/// Lights `element` while `lit` (a list's cursor row while the list has the keyboard).
pub fn lit<E: gpui::Styled>(element: E, lit: bool, palette: Palette) -> E {
    if lit {
        element.bg(rgb(palette.hover)).border_color(rgb(palette.brand))
    } else {
        element
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Keystroke;

    fn key(spec: &str) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke::parse(spec).unwrap_or_else(|_| Keystroke {
                modifiers: gpui::Modifiers::none(),
                key: spec.to_owned(),
                key_char: None,
            }),
            is_held: false,
            prefer_character_input: false,
        }
    }

    #[test]
    fn a_groups_arrows_wrap_and_home_and_end_go_to_its_ends() {
        assert_eq!(step(&key("right"), 3, 2), Some(0));
        assert_eq!(step(&key("left"), 3, 0), Some(2));
        assert_eq!(step(&key("down"), 3, 1), Some(2));
        assert_eq!(step(&key("end"), 3, 0), Some(2));
        assert_eq!(step(&key("home"), 3, 2), Some(0));
        assert_eq!(step(&key("a"), 3, 0), None);
        assert_eq!(step(&key("shift-right"), 3, 0), None);
        assert_eq!(step(&key("right"), 0, 0), None);
    }

    #[test]
    fn a_lists_cursor_stops_at_its_ends() {
        assert_eq!(list_step(&key("down"), 3, None), Some(0));
        assert_eq!(list_step(&key("down"), 3, Some(2)), Some(2));
        assert_eq!(list_step(&key("up"), 3, Some(0)), Some(0));
        assert_eq!(list_step(&key("end"), 3, Some(0)), Some(2));
        assert_eq!(list_step(&key("enter"), 3, Some(0)), None);
    }

    #[test]
    fn shift_f10_and_the_menu_key_open_a_rows_menu() {
        assert!(opens_menu(&key("shift-f10")));
        assert!(opens_menu(&key("menu")));
        assert!(!opens_menu(&key("f10")));
        assert!(!opens_menu(&key("ctrl-shift-f10")));
    }
}
