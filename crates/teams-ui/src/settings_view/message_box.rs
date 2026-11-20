//! The Appearance page's "Message box toolbar" group: a switch for each toolbar button that can hide, the same list as
//! the box's customize list ([`crate::compose::SHOW_IN_TOOLBAR`]). A hidden button's action stays in the box's "+".
//! The AI tools (Correct, Translate) are off until the user turns them on.

use gpui::{AnyElement, App, IntoElement as _, SharedString};
use gpui_component::switch::Switch;

use super::{SettingsView, group, row};
use crate::icons::selected_by;
use crate::settings::{ComposeTool, Settings};
use crate::theme::Palette;

/// What each button's row says it does.
fn description(tool: ComposeTool) -> &'static str {
    match tool {
        ComposeTool::Format => "Show the formatting options: bold, lists, headings and more.",
        ComposeTool::Emoji => "Put an emoji in your message.",
        ComposeTool::Attach => "Send files from this computer in a chat.",
        ComposeTool::Loop => "Loop components, which this app does not have yet.",
        ComposeTool::Correct => "Correct the spelling and style of your message with your AI tool.",
        ComposeTool::Translate => "Translate your message with your AI tool, and pick its language.",
    }
}

impl SettingsView {
    /// The "Message box toolbar" group: a row for each button that can show or hide (`compose-tool-<key>`), the AI
    /// tools' only while the AI features are on. A button that does not show has its action in "+".
    pub(super) fn message_box(&self, settings: &Settings, palette: Palette, cx: &App) -> AnyElement {
        let uses_ai = self.model.read(cx).uses_ai();
        let rows = ComposeTool::ALL
            .into_iter()
            .filter(|tool| !tool.uses_ai() || uses_ai)
            .map(|tool| {
                row(
                    tool.label(),
                    Some(description(tool)),
                    self.tool_switch(tool, settings.shows_compose_tool(tool)),
                    palette,
                )
            })
            .collect();
        group(Some("Message box toolbar"), rows, palette)
    }

    fn tool_switch(&self, tool: ComposeTool, shown: bool) -> AnyElement {
        let model = self.model.clone();
        let id = SharedString::from(format!("compose-tool-{}", tool.key()));
        selected_by(
            id.clone(),
            Switch::new(id).checked(shown).on_click(move |shown: &bool, _, cx| {
                let shown = *shown;
                model.update(cx, |model, cx| {
                    model.change(|settings| settings.show_compose_tool(tool, shown), cx);
                });
            }),
        )
        .into_any_element()
    }
}
