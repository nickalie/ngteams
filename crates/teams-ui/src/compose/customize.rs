//! The customize list: a right click on the toolbar, or "Customize toolbar", lists the buttons that can show or hide,
//! each with its icon and a check while it shows: Teams' own, then the AI tools (Correct, Translate), which are in "+"
//! until the user shows them. A click or Enter shows or hides one, and the list stays open. The choice is a setting
//! ([`crate::settings::ComposeTool`]) of every box. Send and "+" are listed, and cannot hide.
//! The list is the app's shared one ([`crate::customize_list`]), as a message's hover bar has it.

use gpui::{AnyElement, App, Bounds, Context, IntoElement as _, Pixels, Point, Styled as _, Window, point, px};

use super::Compose;
use super::apps::ACTIONS_AND_APPS;
use super::menus::Item;
use crate::customize_list::{self, Customizable, CustomizeList, Line, Shown};
pub use crate::customize_list::{CustomizeClose, CustomizeDown, CustomizeToggle, CustomizeUp};
use crate::icons::Glyph;
use crate::settings::ComposeTool;

/// The customize list's title.
pub const SHOW_IN_TOOLBAR: &str = "Show in the toolbar";

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
        self.customize = Some(CustomizeList::open(at, anchor, window, cx));
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

    /// The list's buttons: each that can hide (the AI tools only while the AI features are on), then "+" and Send
    /// (`None`), as they always show.
    fn customize_tools(cx: &App) -> Vec<Option<ComposeTool>> {
        ComposeTool::ALL
            .into_iter()
            .filter(|tool| !tool.uses_ai() || crate::local_ai::enabled(cx))
            .map(Some)
            .chain([None, None])
            .collect()
    }

    /// The customize list over the window, where it opened (`compose-customize-menu`): a row with a check for each
    /// button that can hide, then "+" and Send, checked and off.
    pub(super) fn customize_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let list = self.customize.as_ref()?;
        let settings = self.settings.read(cx).settings();
        let mut fixed = [("apps", Glyph::PLUS, ACTIONS_AND_APPS), ("send", Glyph::SEND, "Send")].into_iter();
        let lines = Self::customize_tools(cx)
            .into_iter()
            .filter_map(|tool| {
                let (key, glyph, label, checked) = if let Some(tool) = tool {
                    (
                        tool.key(),
                        glyph_of(tool),
                        tool.label(),
                        settings.shows_compose_tool(tool),
                    )
                } else {
                    let (key, glyph, label) = fixed.next()?;
                    (key, glyph, label, true)
                };
                Some(Line {
                    key: key.into(),
                    icon: glyph.icon().size(px(14.0)).into_any_element(),
                    label: label.into(),
                    checked,
                    takes: tool.is_some(),
                })
            })
            .collect();
        let shown = Shown {
            title: SHOW_IN_TOOLBAR,
            prefix: "compose-customize",
            lines,
        };
        Some(customize_list::overlay(list, shown, cx))
    }
}

impl Customizable for Compose {
    fn customize_list(&mut self) -> Option<&mut CustomizeList> {
        self.customize.as_mut()
    }

    fn customize_takes(&self, cx: &App) -> Vec<bool> {
        Self::customize_tools(cx).iter().map(Option::is_some).collect()
    }

    /// Shows the button of the list's row `ix` again, or hides it; the list stays open, the row selected.
    fn customize_toggle(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Self>) {
        let Some(tool) = Self::customize_tools(cx).get(ix).copied().flatten() else {
            return;
        };
        self.settings.update(cx, |model, cx| {
            let shown = model.settings().shows_compose_tool(tool);
            model.change(|settings| settings.show_compose_tool(tool, !shown), cx);
        });
        cx.notify();
    }

    fn customize_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_customize(window, cx);
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
