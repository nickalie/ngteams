//! A row of controls that Tab passes as one stop, as Teams' toolbars are (its message box's tools, its format bar):
//! Left and Right move inside the row and stop at its ends, Home and End go to its ends, and Tab and Shift+Tab leave it.
//! The row keeps the control that the keys were last on, and the keys come back to it: Tab from the place before the row
//! ([`Row::before`]) and Shift+Tab from the place after it ([`Row::after`]) land there, not on the row's first or last.
//!
//! Every control in the row stays a stop of the window's Tab order, so the row moves the focus with the window's own
//! steps and finds its ends by its focus handle, which holds every control in the row and is no stop itself. The row's
//! keys act only while a control of the row itself has the keys: in a menu that a control of it opened, the menu's own
//! keys stay.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{App, Div, FocusHandle, InteractiveElement as _, KeyBinding, MouseButton, Window, actions};

actions!(
    roving_row,
    [Next, Previous, First, Last, Out, Back, EnterAhead, EnterBehind]
);

/// The key context of a row.
const ROW: &str = "RovingRow";

/// The key context of the place before a row: Tab from it enters the row.
const BEFORE: &str = "RovingRowBefore";

/// The key context of the place after a row: Shift+Tab from it enters the row.
const AFTER: &str = "RovingRowAfter";

/// The key context of the kit's popover, which holds a menu button and its menu: on the button, the keys are on a
/// control of the row; in its open menu, a context of the menu comes after it.
const POPOVER: &str = "Popover";

/// The most steps a move takes: more than a window has stops between a row's ends and the next stop.
const MOST_STEPS: usize = 256;

/// Binds the rows' keys; once, at start.
pub fn bind(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("right", Next, Some(ROW)),
        KeyBinding::new("left", Previous, Some(ROW)),
        KeyBinding::new("home", First, Some(ROW)),
        KeyBinding::new("end", Last, Some(ROW)),
        KeyBinding::new("tab", Out, Some(ROW)),
        KeyBinding::new("shift-tab", Back, Some(ROW)),
        KeyBinding::new("tab", EnterAhead, Some(BEFORE)),
        KeyBinding::new("shift-tab", EnterBehind, Some(AFTER)),
    ]);
}

/// A row of controls that Tab passes as one stop (see the module docs).
#[derive(Clone)]
pub struct Row(Rc<State>);

struct State {
    handle: FocusHandle,
    /// The control the keys were last on, counted from the row's first.
    at: Cell<usize>,
    /// The control that had the keys when the window last drew.
    drawn_on: RefCell<Option<FocusHandle>>,
}

impl Row {
    /// A row; its handle is no stop of Tab's order.
    pub fn new(cx: &mut App) -> Self {
        Self(Rc::new(State {
            handle: cx.focus_handle().tab_stop(false),
            at: Cell::new(0),
            drawn_on: RefCell::new(None),
        }))
    }

    /// The control the keys were last on, counted from the row's first.
    #[must_use]
    pub fn at(&self) -> usize {
        self.0.at.get()
    }

    /// As the window draws: notes the control of the row that has the keys, if one has them.
    pub fn note(&self, window: &Window, cx: &App) {
        let on = self
            .keys_are_on_a_control(window, cx)
            .then(|| window.focused(cx))
            .flatten();
        self.0.drawn_on.replace(on);
    }

    /// The control that had the keys when the window last drew: a menu that opens from it takes the keys first.
    #[must_use]
    pub fn drawn_on(&self) -> Option<FocusHandle> {
        self.0.drawn_on.borrow().clone()
    }

    /// Whether a control of the row has the keys.
    #[must_use]
    pub fn has_focus(&self, window: &Window, cx: &App) -> bool {
        self.0.handle.contains_focused(window, cx)
    }

    /// `element` as the row: it holds the row's controls and takes their keys.
    #[must_use]
    pub fn wrap(&self, element: Div) -> Div {
        let row = |act: fn(&Row, &mut Window, &mut App)| {
            let this = self.clone();
            move |window: &mut Window, cx: &mut App| {
                if this.keys_are_on_a_control(window, cx) {
                    act(&this, window, cx);
                } else {
                    cx.propagate();
                }
            }
        };
        let (next, previous, first, last, out, back) = (
            row(|row, window, cx| row.step_inside(true, window, cx)),
            row(|row, window, cx| row.step_inside(false, window, cx)),
            row(|row, window, cx| row.to_end(false, window, cx)),
            row(|row, window, cx| row.to_end(true, window, cx)),
            row(|row, window, cx| row.leave(true, window, cx)),
            row(|row, window, cx| row.leave(false, window, cx)),
        );
        element
            .key_context(ROW)
            .track_focus(&self.0.handle)
            // a press between its controls leaves the keys where they were
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_action(move |_: &Next, window, cx| next(window, cx))
            .on_action(move |_: &Previous, window, cx| previous(window, cx))
            .on_action(move |_: &First, window, cx| first(window, cx))
            .on_action(move |_: &Last, window, cx| last(window, cx))
            .on_action(move |_: &Out, window, cx| out(window, cx))
            .on_action(move |_: &Back, window, cx| back(window, cx))
    }

    /// `element` as the place before the row: Tab from it lands on the control the keys were last on, when the next
    /// stop is in the row.
    #[must_use]
    pub fn before(&self, element: Div) -> Div {
        let this = self.clone();
        element
            .key_context(BEFORE)
            .on_action(move |_: &EnterAhead, window, cx| this.enter(true, window, cx))
    }

    /// `element` as the place after the row: Shift+Tab from it lands on the control the keys were last on, when the
    /// stop before is in the row.
    #[must_use]
    pub fn after(&self, element: Div) -> Div {
        let this = self.clone();
        element
            .key_context(AFTER)
            .on_action(move |_: &EnterBehind, window, cx| this.enter(false, window, cx))
    }

    /// Whether the keys are on a control of the row itself, not in something of its own (a menu) inside it.
    fn keys_are_on_a_control(&self, window: &Window, cx: &App) -> bool {
        if !self.has_focus(window, cx) || self.0.handle.is_focused(window) {
            return false;
        }
        let contexts = window.context_stack();
        contexts
            .iter()
            .rposition(|context| context.contains(ROW))
            .is_some_and(|row| contexts[row + 1..].iter().all(|context| context.contains(POPOVER)))
    }

    /// One step of the window's Tab order.
    fn step(forward: bool, window: &mut Window, cx: &mut App) {
        if forward {
            window.focus_next(cx);
        } else {
            window.focus_prev(cx);
        }
    }

    /// Left or Right: the next control in the row, if there is one.
    fn step_inside(&self, forward: bool, window: &mut Window, cx: &mut App) {
        Self::step(forward, window, cx);
        if !self.has_focus(window, cx) {
            Self::step(!forward, window, cx);
            return;
        }
        let at = self.at();
        self.0.at.set(if forward {
            at.saturating_add(1)
        } else {
            at.saturating_sub(1)
        });
    }

    /// Home or End: the first or the last control of the row.
    fn to_end(&self, last: bool, window: &mut Window, cx: &mut App) {
        let mut steps = 0;
        while steps < MOST_STEPS {
            Self::step(last, window, cx);
            if !self.has_focus(window, cx) {
                break;
            }
            steps += 1;
        }
        Self::step(!last, window, cx);
        self.0.at.set(if last { self.at() + steps } else { 0 });
    }

    /// Tab or Shift+Tab: the first stop past the row's end.
    fn leave(&self, forward: bool, window: &mut Window, cx: &mut App) {
        for _ in 0..MOST_STEPS {
            Self::step(forward, window, cx);
            if !self.has_focus(window, cx) {
                return;
            }
        }
    }

    /// Tab from before the row or Shift+Tab from after it: one step, and inside the row, on to the control the keys
    /// were last on. The row may have fewer controls than then: it stops at its last.
    fn enter(&self, forward: bool, window: &mut Window, cx: &mut App) {
        Self::step(forward, window, cx);
        if !self.has_focus(window, cx) {
            return;
        }
        if !forward {
            // from its last control back to its first
            for _ in 0..MOST_STEPS {
                Self::step(false, window, cx);
                if !self.has_focus(window, cx) {
                    break;
                }
            }
            Self::step(true, window, cx);
        }
        let mut at = 0;
        while at < self.at() {
            Self::step(true, window, cx);
            if !self.has_focus(window, cx) {
                Self::step(false, window, cx);
                break;
            }
            at += 1;
        }
        self.0.at.set(at);
    }
}
