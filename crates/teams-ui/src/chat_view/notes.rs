//! The account's own notes ([`NOTES`], "<name> (You)") while they hold nothing: Teams shows "This is your space" and
//! what the chat is for where the messages go, in place of the history's top row.

use gpui::{AnyElement, InteractiveElement as _, IntoElement as _, ParentElement as _, Styled as _, div, px, rgb};
use teams_core::NOTES;

use super::{ChatView, History};
use crate::icons::Glyph;
use crate::theme::{self, Palette};

/// The title of the empty notes, as Teams words it.
pub const NOTES_INTRO_TITLE: &str = "This is your space";

/// What Teams says under [`NOTES_INTRO_TITLE`].
pub const NOTES_INTRO_TEXT: &str = "This chat is just for you...with you. Use it for drafts, send files to yourself, or \
                                    get to know chat features a little better.";

impl ChatView {
    /// Whether the view shows the empty notes' intro: the notes, read to their beginning, with nothing in them.
    pub(super) fn notes_intro_shown(&self) -> bool {
        self.conversation == NOTES && self.history == History::Complete && self.is_empty()
    }

    /// The empty notes' intro ([`NOTES_INTRO_TITLE`]), over the place of the messages.
    pub(super) fn notes_intro(&self, palette: Palette) -> Option<AnyElement> {
        if !self.notes_intro_shown() {
            return None;
        }
        let mark = div()
            .size(px(80.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(theme::brand_gradient(palette, 135.0))
            .text_color(rgb(palette.on_brand))
            .child(Glyph::MESSAGE.icon().size(px(34.0)));
        let words = div()
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.foreground))
                    .child(NOTES_INTRO_TITLE),
            )
            .child(
                div()
                    .max_w(px(420.0))
                    .text_sm()
                    .text_center()
                    .text_color(rgb(palette.foreground))
                    .child(NOTES_INTRO_TEXT),
            );
        Some(
            div()
                .debug_selector(|| "chat-notes-intro".to_owned())
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_4()
                .px_6()
                .child(mark)
                .child(words)
                .into_any_element(),
        )
    }
}
