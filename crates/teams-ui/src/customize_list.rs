//! The list that shows or hides the buttons of a bar: the message box's toolbar ([`crate::compose`]) and a message's
//! hover bar ([`crate::chat_view`]). Under its title, each button has a row with a check while it shows, its icon and
//! its name. A click or Enter shows or hides one, and the list stays open. A button that always shows has a row drawn
//! off, with its check, that takes nothing.
//!
//! It is a list of its own, drawn as the kit's menus: the kit's menu selects its first item on the first Down and on
//! Down from its last row, and the first item here is the list's title. Down selects the first row that can be taken,
//! Up the last, and from the end each key goes round. Escape, Tab or Shift+Tab closes the list, as Tab leaves a menu.

use gpui::{
    AnyElement, App, Context, FocusHandle, InteractiveElement as _, IntoElement as _, KeyBinding, ParentElement as _,
    Pixels, Point, SharedString, StatefulInteractiveElement as _, Styled as _, Window, actions, anchored, deferred,
    div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::{ActiveTheme as _, ThemeStyled as _};

use crate::icons::Glyph;
use crate::theme::{self, Palette};

/// The list's key context.
pub(crate) const CONTEXT: &str = "CustomizeList";

actions!(
    compose,
    [
        /// Selects the list's row above, from the first the last.
        CustomizeUp,
        /// Selects the list's row below, from the last the first.
        CustomizeDown,
        /// Shows or hides the button of the list's selected row.
        CustomizeToggle,
        /// Closes the list, the keyboard back where it was.
        CustomizeClose
    ]
);

struct Bound;
impl gpui::Global for Bound {}

/// Binds the list's keys, one time.
pub(crate) fn bind_keys(cx: &mut App) {
    if cx.has_global::<Bound>() {
        return;
    }
    cx.set_global(Bound);
    cx.bind_keys([
        KeyBinding::new("up", CustomizeUp, Some(CONTEXT)),
        KeyBinding::new("down", CustomizeDown, Some(CONTEXT)),
        KeyBinding::new("enter", CustomizeToggle, Some(CONTEXT)),
        KeyBinding::new("escape", CustomizeClose, Some(CONTEXT)),
        // before the root's Tab: the keys must not leave a list that stays drawn
        KeyBinding::new("tab", CustomizeClose, Some(CONTEXT)),
        KeyBinding::new("shift-tab", CustomizeClose, Some(CONTEXT)),
    ]);
}

/// The list while it is open: where it opened, and its selected row.
pub(crate) struct CustomizeList {
    focus: FocusHandle,
    at: Point<Pixels>,
    anchor: gpui::Anchor,
    selected: Option<usize>,
    /// The control the keys were on before the menu that opened the list: they go back to it when the list closes.
    back_to: Option<FocusHandle>,
}

impl CustomizeList {
    /// A list that opens at `at`, by its corner `anchor`, the keys in it.
    pub(crate) fn open<V>(at: Point<Pixels>, anchor: gpui::Anchor, window: &mut Window, cx: &mut Context<V>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self {
            focus,
            at,
            anchor,
            selected: None,
            back_to: None,
        }
    }

    /// The list, the keys back on `control` when it closes.
    #[must_use]
    pub(crate) fn back_to(self, control: Option<FocusHandle>) -> Self {
        Self {
            back_to: control,
            ..self
        }
    }

    /// The control the keys go back to when the list closes, if one was given.
    pub(crate) fn return_control(&self) -> Option<&FocusHandle> {
        self.back_to.as_ref()
    }

    /// Whether the keys are in the list.
    pub(crate) fn has_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    /// Selects row `ix`.
    pub(crate) fn select(&mut self, ix: usize) {
        self.selected = Some(ix);
    }

    fn step(&mut self, takes: &[bool], down: bool) {
        let from = self.selected.filter(|ix| *ix < takes.len());
        self.selected = step(takes, from, down);
    }

    /// The pointer over a row selects it, as in the kit's menus; off it, the row stays selected only after a key.
    fn hover(&mut self, ix: usize, hovered: bool, window: &Window) {
        if hovered {
            self.selected = Some(ix);
        } else if self.selected == Some(ix) && !window.last_input_was_keyboard() {
            self.selected = None;
        }
    }
}

/// A view that has a customize list: what each row does.
pub(crate) trait Customizable: Sized + 'static {
    /// The open list.
    fn customize_list(&mut self) -> Option<&mut CustomizeList>;
    /// Which rows can be taken, in the list's order.
    fn customize_takes(&self, cx: &App) -> Vec<bool>;
    /// Shows or hides the button of row `ix`. The list stays open.
    fn customize_toggle(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>);
    /// Closes the list.
    fn customize_close(&mut self, window: &mut Window, cx: &mut Context<Self>);
}

/// A row of the list.
pub(crate) struct Line {
    /// The row's test selector: `<prefix>-<key>`, its check `<prefix>-<key>-check`.
    pub key: SharedString,
    pub icon: AnyElement,
    pub label: SharedString,
    /// Whether its button shows.
    pub checked: bool,
    /// Whether the row shows or hides its button: false for a button that always shows.
    pub takes: bool,
}

/// What the list shows over the window: its title, its rows, and its selectors (`<prefix>-list`, `<prefix>-menu`).
pub(crate) struct Shown {
    pub title: &'static str,
    pub prefix: &'static str,
    pub lines: Vec<Line>,
}

/// The row after `from` that can be taken, going round; `down` false: the row before. No row: the first, or the last.
pub(crate) fn step(takes: &[bool], from: Option<usize>, down: bool) -> Option<usize> {
    let rows: Vec<usize> = (0..takes.len()).filter(|ix| takes[*ix]).collect();
    let next = match (from, down) {
        (Some(from), true) => rows.iter().find(|ix| **ix > from),
        (Some(from), false) => rows.iter().rev().find(|ix| **ix < from),
        (None, _) => None,
    };
    next.or(if down { rows.first() } else { rows.last() }).copied()
}

fn on_step<V: Customizable>(this: &mut V, down: bool, cx: &mut Context<V>) {
    let takes = this.customize_takes(cx);
    if let Some(list) = this.customize_list() {
        list.step(&takes, down);
        cx.notify();
    }
}

/// The list over the window, where it opened.
pub(crate) fn overlay<V: Customizable>(list: &CustomizeList, shown: Shown, cx: &mut Context<V>) -> AnyElement {
    let palette = theme::current(cx);
    let Shown { title, prefix, lines } = shown;
    let rows: Vec<AnyElement> = lines
        .into_iter()
        .enumerate()
        .map(|(ix, line)| row(prefix, ix, line, list.selected == Some(ix), palette, cx))
        .collect();
    let title = div()
        .h(px(26.0))
        .px_2()
        .flex()
        .items_center()
        .text_color(rgb(palette.muted_foreground))
        .child(title);
    let panel = div()
        .id(SharedString::from(format!("{prefix}-list")))
        .key_context(CONTEXT)
        .track_focus(&list.focus)
        .on_action(cx.listener(|this, _: &CustomizeUp, _, cx| on_step(this, false, cx)))
        .on_action(cx.listener(|this, _: &CustomizeDown, _, cx| on_step(this, true, cx)))
        .on_action(cx.listener(|this: &mut V, _: &CustomizeToggle, window, cx| {
            if let Some(ix) = this.customize_list().and_then(|list| list.selected) {
                this.customize_toggle(ix, window, cx);
            }
        }))
        .on_action(cx.listener(|this: &mut V, _: &CustomizeClose, window, cx| this.customize_close(window, cx)))
        .on_mouse_down_out(cx.listener(|this: &mut V, _, window, cx| this.customize_close(window, cx)))
        .popover_style(cx)
        .occlude()
        .min_w(px(220.0))
        .p_1()
        .flex()
        .flex_col()
        .gap_0p5()
        .text_sm()
        .child(title)
        .children(rows);
    let menu = format!("{prefix}-menu");
    deferred(
        anchored()
            .anchor(list.anchor)
            .position(list.at)
            .snap_to_window_with_margin(px(8.0))
            .child(div().debug_selector(move || menu).pb_1().child(panel)),
    )
    .with_priority(1)
    .into_any_element()
}

/// A row of the list: a check while its button shows, its icon and its name; lit while selected. A row that takes
/// nothing is drawn off.
fn row<V: Customizable>(
    prefix: &str,
    ix: usize,
    line: Line,
    selected: bool,
    palette: Palette,
    cx: &mut Context<V>,
) -> AnyElement {
    let radius = cx.theme().radius.min(px(8.0));
    let selector = format!("{prefix}-{}", line.key);
    let check = line.checked.then(|| {
        let check = format!("{selector}-check");
        div()
            .debug_selector(move || check)
            .child(Glyph::CHECK.icon().size(px(14.0)))
    });
    let takes = line.takes;
    div()
        .id(ix)
        .debug_selector(move || selector)
        .h(px(26.0))
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .rounded(radius)
        .child(div().flex_none().w(px(14.0)).children(check))
        .child(
            div()
                .flex_none()
                .size(px(14.0))
                .flex()
                .items_center()
                .justify_center()
                .child(line.icon),
        )
        .child(line.label)
        .when(!takes, |row| row.text_color(rgb(palette.muted_foreground)))
        .when(takes, |row| {
            row.text_color(rgb(palette.foreground))
                .when(selected, |row| row.bg(rgb(palette.hover)))
                .on_hover(cx.listener(move |this: &mut V, hovered: &bool, window, cx| {
                    if let Some(list) = this.customize_list() {
                        list.hover(ix, *hovered, window);
                        cx.notify();
                    }
                }))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this: &mut V, _, window, cx| {
                    if let Some(list) = this.customize_list() {
                        list.select(ix);
                    }
                    this.customize_toggle(ix, window, cx);
                }))
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_pass_over_the_rows_that_cannot_hide_and_go_round() {
        let five = [true, true, true, true, true, false, false];
        assert_eq!(step(&five, None, true), Some(0));
        assert_eq!(step(&five, None, false), Some(4));
        assert_eq!(step(&five, Some(0), true), Some(1));
        assert_eq!(step(&five, Some(4), true), Some(0));
        assert_eq!(step(&five, Some(0), false), Some(4));
        assert_eq!(step(&five, Some(6), true), Some(0));
        assert_eq!(step(&five, Some(6), false), Some(4));
        assert_eq!(step(&[false, false], None, true), None);
    }
}
