//! The customize list: a right click on the toolbar, or "Customize toolbar", lists the buttons that can show or hide,
//! each with its icon and a check while it shows: Teams' own, then the AI tools (Correct, Translate), which are in "+"
//! until the user shows them. A click or Enter shows or hides one, and the list stays open. The choice is a setting
//! ([`crate::settings::ComposeTool`]) of every box. Send and "+" are listed, and cannot hide.
//!
//! It is a list of its own, drawn as the kit's menus: the kit's menu selects its first item on the first Down and on
//! Down from its last row, and the first item here is the list's title. Down selects the first row that can be taken,
//! Up the last, and from the end each key goes round, as in the box's other menus.

use gpui::{
    AnyElement, App, Bounds, Context, FocusHandle, InteractiveElement as _, IntoElement as _, KeyBinding,
    ParentElement as _, Pixels, Point, StatefulInteractiveElement as _, Styled as _, Window, actions, anchored,
    deferred, div, point, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::{ActiveTheme as _, ThemeStyled as _};

use super::Compose;
use super::apps::ACTIONS_AND_APPS;
use super::menus::Item;
use crate::icons::Glyph;
use crate::settings::ComposeTool;
use crate::theme::{self, Palette};

/// The customize list's title.
pub const SHOW_IN_TOOLBAR: &str = "Show in the toolbar";

/// The list's key context.
const CONTEXT: &str = "ComposeCustomize";

actions!(
    compose,
    [
        /// Selects the list's row above, from the first the last.
        CustomizeUp,
        /// Selects the list's row below, from the last the first.
        CustomizeDown,
        /// Shows or hides the button of the list's selected row.
        CustomizeToggle,
        /// Closes the list, the keyboard back in the text.
        CustomizeClose
    ]
);

/// Binds the list's keys.
pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", CustomizeUp, Some(CONTEXT)),
        KeyBinding::new("down", CustomizeDown, Some(CONTEXT)),
        KeyBinding::new("enter", CustomizeToggle, Some(CONTEXT)),
        KeyBinding::new("escape", CustomizeClose, Some(CONTEXT)),
    ]);
}

/// The customize list while it is open: where it opened, and its selected row.
pub(super) struct Customize {
    focus: FocusHandle,
    at: Point<Pixels>,
    anchor: gpui::Anchor,
    selected: Option<usize>,
}

/// A row of the list: a button that can hide (`tool`), or "+" or Send, which always show.
#[derive(Clone, Copy)]
struct Line {
    tool: Option<ComposeTool>,
    glyph: Glyph,
    key: &'static str,
    label: &'static str,
    checked: bool,
}

/// The row after `from` that can be taken, going round; `down` false: the row before. No row: the first, or the last.
fn step(lines: &[Line], from: Option<usize>, down: bool) -> Option<usize> {
    let takes: Vec<usize> = (0..lines.len()).filter(|ix| lines[*ix].tool.is_some()).collect();
    let next = match (from, down) {
        (Some(from), true) => takes.iter().find(|ix| **ix > from),
        (Some(from), false) => takes.iter().rev().find(|ix| **ix < from),
        (None, _) => None,
    };
    next.or(if down { takes.first() } else { takes.last() }).copied()
}

impl Compose {
    /// Opens the customize list by a right click at `at` in the window: its foot over the toolbar's buttons, as from
    /// "Customize toolbar", so that the bar shows while the user changes it.
    pub fn open_customize(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let foot = self.customize_at.get().map_or(at.y, |bounds| bounds.top());
        self.open_customize_with(point(at.x, foot), gpui::Anchor::BottomLeft, window, cx);
    }

    /// "Customize toolbar": opens the customize list by the button whose menu holds that row.
    pub fn customize_toolbar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let at = self
            .customize_at
            .get()
            .or_else(|| self.box_at.get())
            .map_or_else(Point::default, |bounds: Bounds<Pixels>| bounds.top_right());
        self.open_customize_with(at, gpui::Anchor::BottomRight, window, cx);
    }

    fn open_customize_with(
        &mut self,
        at: Point<Pixels>,
        anchor: gpui::Anchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only.is_some() {
            return;
        }
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.customize = Some(Customize {
            focus,
            at,
            anchor,
            selected: None,
        });
        cx.notify();
    }

    /// Whether the customize list is open.
    #[must_use]
    pub fn customize_open(&self) -> bool {
        self.customize.is_some()
    }

    /// Closes the customize list, the keyboard back in the text.
    pub fn close_customize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.customize.take().is_some() {
            self.focus(window, cx);
            cx.notify();
        }
    }

    /// The list's rows: a row with a check for each button that can hide (the AI tools only while the AI features are
    /// on), then "+" and Send, checked and off, as they always show.
    fn customize_lines(&self, cx: &App) -> Vec<Line> {
        let settings = self.settings.read(cx).settings();
        let tools = ComposeTool::ALL
            .into_iter()
            .filter(|tool| !tool.uses_ai() || crate::local_ai::enabled(cx))
            .map(|tool| Line {
                tool: Some(tool),
                glyph: glyph_of(tool),
                key: tool.key(),
                label: tool.label(),
                checked: settings.shows_compose_tool(tool),
            });
        let fixed =
            [("apps", Glyph::PLUS, ACTIONS_AND_APPS), ("send", Glyph::SEND, "Send")].map(|(key, glyph, label)| Line {
                tool: None,
                glyph,
                key,
                label,
                checked: true,
            });
        tools.chain(fixed).collect()
    }

    fn customize_step(&mut self, down: bool, cx: &mut Context<Self>) {
        let lines = self.customize_lines(cx);
        if let Some(customize) = &mut self.customize {
            let from = customize.selected.filter(|ix| *ix < lines.len());
            customize.selected = step(&lines, from, down);
            cx.notify();
        }
    }

    /// Shows the button of the list's row `ix` again, or hides it; the list stays open, the row selected.
    fn customize_toggle(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(tool) = self.customize_lines(cx).get(ix).and_then(|line| line.tool) else {
            return;
        };
        if let Some(customize) = &mut self.customize {
            customize.selected = Some(ix);
        }
        self.settings.update(cx, |model, cx| {
            let shown = model.settings().shows_compose_tool(tool);
            model.change(|settings| settings.show_compose_tool(tool, !shown), cx);
        });
        cx.notify();
    }

    /// The pointer over a row selects it, as in the kit's menus; off it, the row stays selected only after a key.
    fn customize_hover(&mut self, ix: usize, hovered: bool, window: &Window, cx: &mut Context<Self>) {
        let Some(customize) = &mut self.customize else { return };
        if hovered {
            customize.selected = Some(ix);
        } else if customize.selected == Some(ix) && !window.last_input_was_keyboard() {
            customize.selected = None;
        }
        cx.notify();
    }

    /// The customize list over the window, where it opened (`compose-customize-menu`).
    pub(super) fn customize_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let customize = self.customize.as_ref()?;
        let palette = theme::current(cx);
        let radius = cx.theme().radius.min(px(8.0));
        let rows: Vec<AnyElement> = self
            .customize_lines(cx)
            .into_iter()
            .enumerate()
            .map(|(ix, line)| customize_row(ix, line, customize.selected == Some(ix), palette, radius, cx))
            .collect();
        let title = div()
            .h(px(26.0))
            .px_2()
            .flex()
            .items_center()
            .text_color(rgb(palette.muted_foreground))
            .child(SHOW_IN_TOOLBAR);
        let list = div()
            .id("compose-customize-list")
            .key_context(CONTEXT)
            .track_focus(&customize.focus)
            .on_action(cx.listener(|this, _: &CustomizeUp, _, cx| this.customize_step(false, cx)))
            .on_action(cx.listener(|this, _: &CustomizeDown, _, cx| this.customize_step(true, cx)))
            .on_action(cx.listener(|this, _: &CustomizeToggle, _, cx| {
                if let Some(ix) = this.customize.as_ref().and_then(|customize| customize.selected) {
                    this.customize_toggle(ix, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CustomizeClose, window, cx| this.close_customize(window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_customize(window, cx)))
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
        Some(
            deferred(
                anchored()
                    .anchor(customize.anchor)
                    .position(customize.at)
                    .snap_to_window_with_margin(px(8.0))
                    .child(
                        div()
                            .debug_selector(|| "compose-customize-menu".to_owned())
                            .pb_1()
                            .child(list),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}

/// The icon of `tool`'s button, as its row in "+" shows it.
fn glyph_of(tool: ComposeTool) -> Glyph {
    match tool {
        ComposeTool::Format => Item::Format,
        ComposeTool::Emoji => Item::Emoji,
        ComposeTool::Attach => Item::Attach,
        ComposeTool::Loop => Item::Loop,
        ComposeTool::Correct => Item::Correct,
        ComposeTool::Translate => Item::Translate,
    }
    .glyph()
}

/// A row of the list: a check while its button shows, its icon and its name (`compose-customize-<key>`,
/// `compose-customize-<key>-check`); lit while selected. "+" and Send are drawn off and take nothing.
fn customize_row(
    ix: usize,
    line: Line,
    selected: bool,
    palette: Palette,
    radius: Pixels,
    cx: &mut Context<Compose>,
) -> AnyElement {
    let selector = format!("compose-customize-{}", line.key);
    let check = line.checked.then(|| {
        let check = format!("{selector}-check");
        div()
            .debug_selector(move || check)
            .child(Glyph::CHECK.icon().size(px(14.0)))
    });
    let takes = line.tool.is_some();
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
        .child(line.glyph.icon().size(px(14.0)))
        .child(line.label)
        .when(!takes, |row| row.text_color(rgb(palette.muted_foreground)))
        .when(takes, |row| {
            row.text_color(rgb(palette.foreground))
                .when(selected, |row| row.bg(rgb(palette.hover)))
                .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
                    this.customize_hover(ix, *hovered, window, cx);
                }))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| this.customize_toggle(ix, cx)))
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(tools: usize) -> Vec<Line> {
        let line = |tool: Option<ComposeTool>| Line {
            tool,
            glyph: Glyph::CHECK,
            key: "",
            label: "",
            checked: true,
        };
        let mut lines: Vec<Line> = ComposeTool::ALL[..tools].iter().map(|tool| line(Some(*tool))).collect();
        lines.extend([line(None), line(None)]);
        lines
    }

    #[test]
    fn the_keys_pass_over_the_rows_that_cannot_hide_and_go_round() {
        let five = lines(5);
        assert_eq!(step(&five, None, true), Some(0));
        assert_eq!(step(&five, None, false), Some(4));
        assert_eq!(step(&five, Some(0), true), Some(1));
        assert_eq!(step(&five, Some(4), true), Some(0));
        assert_eq!(step(&five, Some(0), false), Some(4));
        assert_eq!(step(&five, Some(6), true), Some(0));
        assert_eq!(step(&five, Some(6), false), Some(4));
        assert_eq!(step(&lines(0), None, true), None);
    }
}
