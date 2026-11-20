//! How the box's tools fit its own width, as Teams' box does it. The rule depends only on the width the box was last
//! drawn at, so a narrow window, a thread's pane and a call's Chat pane give the same box.
//!
//! - A box at least [`NARROW_BOX`] wide, or one whose text keeps [`MIN_TEXT`] beside all its tools, stays on one line.
//! - A narrower box is stacked, as Teams' narrow box is: the text has the full width on top, and the tools and Send
//!   go on a row under it. The row holds Teams' own tools (Format, Emoji, Attach, Loop, Actions and apps) while they
//!   fit. This app's own AI tools (Correct, Translate) and each Teams tool that does not fit go into "More options",
//!   as the user asked for this width. Send always stays on the row.
//! - A tool that the user does not show ([`crate::settings::ComposeTool`]) is on no row: its action is in "+"
//!   ([`super::menus`]). The AI tools are there until the user shows them.

use gpui::{App, Pixels};

use super::{Compose, LANGUAGE_CHEVRON, NARROW_BOX, TOOL};
use crate::settings::ComposeTool;

/// The least width of the text beside the tools on one line, px; a box that cannot give it is stacked.
pub(super) const MIN_TEXT: f32 = 160.0;

/// The box's spacing at the window's rem size, px. Its gaps and its padding are rem-sized, so they follow the font
/// size the user set; the tool buttons are [`TOOL`] px at every size.
#[derive(Clone, Copy, Debug)]
struct Space {
    /// The gap between the tools (`gap_1`).
    gap: f32,
    /// The border and the padding left and right of a box on one line (`pl_3`, `pr_2`).
    line_chrome: f32,
    /// The border and the padding left and right of a stacked box (`px_3`).
    stacked_chrome: f32,
    /// The gap after a tool's button.
    gap_after: f32,
    /// The divider (`mx_1`) and Send at the end of the row, with their gaps.
    end: f32,
}

impl Space {
    /// The width of `tool`'s button and the gap after it.
    fn slot(self, tool: Tool) -> f32 {
        tool.width() + self.gap_after
    }

    fn at(rem: Pixels) -> Self {
        let rem = f32::from(rem);
        let gap = 0.25 * rem;
        Self {
            gap,
            line_chrome: 2.0 + 0.75 * rem + 0.5 * rem,
            stacked_chrome: 2.0 + 2.0 * 0.75 * rem,
            gap_after: gap,
            end: gap + 1.0 + 2.0 * gap + gap + TOOL,
        }
    }
}

/// A tool of the box, Send apart, in the order the box shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tool {
    Format,
    Emoji,
    Attach,
    Loop,
    /// Actions and apps, the "+": it never hides, as it holds the hidden tools.
    Apps,
    /// Correct the draft with the user's AI tool.
    Correct,
    /// Translate the draft with the user's AI tool; its button has the language's chevron beside it.
    Translate,
}

impl Tool {
    /// A tool of Teams' own box, which a stacked row keeps while it fits.
    fn of_teams(self) -> bool {
        !matches!(self, Self::Correct | Self::Translate)
    }

    /// The width of its button on the row, px.
    fn width(self) -> f32 {
        if self == Self::Translate {
            TOOL + LANGUAGE_CHEVRON
        } else {
            TOOL
        }
    }

    /// The setting that hides it, for a tool that can hide.
    #[must_use]
    pub(crate) fn hides_as(self) -> Option<ComposeTool> {
        match self {
            Self::Format => Some(ComposeTool::Format),
            Self::Emoji => Some(ComposeTool::Emoji),
            Self::Attach => Some(ComposeTool::Attach),
            Self::Loop => Some(ComposeTool::Loop),
            Self::Correct => Some(ComposeTool::Correct),
            Self::Translate => Some(ComposeTool::Translate),
            Self::Apps => None,
        }
    }
}

/// Where each tool of a box goes at its width.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Fit {
    /// The text on top and the tools on a row under it.
    pub(crate) stacked: bool,
    /// The tools on the box's row, in order.
    pub(crate) row: Vec<Tool>,
    /// The tools in "More options", in order.
    pub(crate) more: Vec<Tool>,
    /// The tools the user hid, in order: their actions are in "+".
    pub(crate) hidden: Vec<Tool>,
}

/// Where `tools` go in a box last drawn `width` wide (`None`: not drawn yet, so as a wide box) in a window whose rem size
/// is `rem`.
#[must_use]
pub(crate) fn fit(width: Option<Pixels>, rem: Pixels, tools: &[Tool]) -> Fit {
    let all = || Fit {
        stacked: false,
        row: tools.to_vec(),
        more: Vec::new(),
        hidden: Vec::new(),
    };
    let Some(width) = width.map(f32::from).filter(|width| *width < NARROW_BOX) else {
        return all();
    };
    let space = Space::at(rem);
    let tools_width: f32 = tools.iter().map(|tool| space.slot(*tool)).sum();
    if width - space.line_chrome - tools_width - space.end - space.gap >= MIN_TEXT {
        return all();
    }
    // the row keeps a slot for More options
    let room = width - space.stacked_chrome - space.end - (TOOL + space.gap_after);
    let mut used = 0.0;
    let row: Vec<Tool> = tools
        .iter()
        .copied()
        .take_while(|tool| {
            used += space.slot(*tool);
            tool.of_teams() && used <= room
        })
        .collect();
    let more = tools.iter().copied().filter(|tool| !row.contains(tool)).collect();
    Fit {
        stacked: true,
        row,
        more,
        hidden: Vec::new(),
    }
}

impl Compose {
    /// The tools this box has, shown or hidden: the AI tools only while the AI features are on.
    fn tools(&self, cx: &App) -> Vec<Tool> {
        let mut tools = vec![Tool::Format, Tool::Emoji];
        if self.writes_a_chat() {
            tools.extend([Tool::Attach, Tool::Loop]);
        }
        tools.push(Tool::Apps);
        if crate::local_ai::enabled(cx) {
            tools.extend([Tool::Correct, Tool::Translate]);
        }
        tools
    }

    /// Where this box's tools go at the width it was last drawn at, the ones the user hid apart.
    pub(super) fn fit(&self, cx: &App) -> Fit {
        let settings = self.settings.read(cx).settings();
        let (shown, hidden): (Vec<Tool>, Vec<Tool>) = self
            .tools(cx)
            .into_iter()
            .partition(|tool| tool.hides_as().is_none_or(|hides| settings.shows_compose_tool(hides)));
        let mut fit = fit(self.box_at.get().map(|bounds| bounds.size.width), self.rem, &shown);
        fit.hidden = hidden;
        fit
    }

    /// Whether the box was last drawn stacked: the text on top, its tools on a row under it.
    #[must_use]
    pub fn is_stacked(&self, cx: &App) -> bool {
        self.fit(cx).stacked
    }
}

#[cfg(test)]
mod tests {
    use gpui::px;

    use super::*;

    /// The app's rem size at its default font size.
    fn rem() -> Pixels {
        px(f32::from(crate::theme::FONT_SIZE))
    }

    const CHAT: [Tool; 7] = [
        Tool::Format,
        Tool::Emoji,
        Tool::Attach,
        Tool::Loop,
        Tool::Apps,
        Tool::Correct,
        Tool::Translate,
    ];
    const CHANNEL: [Tool; 5] = [Tool::Format, Tool::Emoji, Tool::Apps, Tool::Correct, Tool::Translate];

    #[test]
    fn a_box_not_drawn_yet_or_wide_keeps_every_tool_on_one_line() {
        for width in [None, Some(px(NARROW_BOX)), Some(px(1200.0))] {
            let fit = fit(width, rem(), &CHAT);
            assert!(!fit.stacked);
            assert_eq!(fit.row, CHAT);
            assert!(fit.more.is_empty() && fit.hidden.is_empty());
        }
    }

    #[test]
    fn a_box_in_a_call_s_chat_pane_stacks_with_teams_tools_on_its_row() {
        // the Chat pane is 360 px, its box 328 px
        let fit = fit(Some(px(328.0)), rem(), &CHAT);
        assert!(fit.stacked);
        assert_eq!(
            fit.row,
            [Tool::Format, Tool::Emoji, Tool::Attach, Tool::Loop, Tool::Apps]
        );
        assert_eq!(fit.more, [Tool::Correct, Tool::Translate]);
    }

    #[test]
    fn a_thread_s_box_with_room_for_its_text_stays_on_one_line() {
        // the thread's pane is 440 px, its box 408 px
        let fit = fit(Some(px(408.0)), rem(), &CHANNEL);
        assert!(!fit.stacked);
        assert_eq!(fit.row, CHANNEL);
    }

    #[test]
    fn a_very_narrow_box_moves_the_last_teams_tools_into_the_menu_too() {
        let fit = fit(Some(px(200.0)), rem(), &CHAT);
        assert!(fit.stacked);
        assert_eq!(fit.row, [Tool::Format, Tool::Emoji, Tool::Attach]);
        assert_eq!(fit.more.first(), Some(&Tool::Loop));
        assert_eq!(fit.row.len() + fit.more.len(), CHAT.len());
    }

    #[test]
    fn a_box_with_no_room_for_one_tool_keeps_more_options_and_send() {
        let fit = fit(Some(px(60.0)), rem(), &CHAT);
        assert!(fit.row.is_empty());
        assert_eq!(fit.more, CHAT);
    }

    #[test]
    fn the_spacing_follows_the_font_size_so_a_larger_one_moves_more_into_the_menu() {
        // at 24 px the box's gaps and padding are wider: the five Teams tools no longer fit beside More and Send
        let at_default = fit(Some(px(264.0)), rem(), &CHAT);
        assert_eq!(at_default.row.len(), 5);
        let at_largest = fit(Some(px(264.0)), px(24.0), &CHAT);
        assert!(at_largest.stacked);
        assert_eq!(at_largest.row, [Tool::Format, Tool::Emoji, Tool::Attach, Tool::Loop]);
        assert_eq!(at_largest.more.first(), Some(&Tool::Apps));
    }

    #[test]
    fn only_the_plus_cannot_hide() {
        let hides: Vec<Option<ComposeTool>> = CHAT.iter().map(|tool| tool.hides_as()).collect();
        assert_eq!(
            hides,
            [
                Some(ComposeTool::Format),
                Some(ComposeTool::Emoji),
                Some(ComposeTool::Attach),
                Some(ComposeTool::Loop),
                None,
                Some(ComposeTool::Correct),
                Some(ComposeTool::Translate),
            ]
        );
    }
}
