//! The AI tools' buttons, while the user shows them on the toolbar ([`crate::settings::ComposeTool`]): Correct, and
//! Translate with a chevron beside it that opens the language menu. Until the user shows them, their actions are rows
//! of "+" ([`super::menus`]). Their keys (Ctrl+Shift+G, Ctrl+Shift+Y) work in both cases.

use gpui::{AnyElement, Context, Entity, IntoElement as _, ParentElement as _, Styled as _, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, Selectable as _, Sizable as _, Size};

use super::{Compose, CorrectDraft, LANGUAGE_CHEVRON, TOOL, TranslateDraft};
use crate::draft_translate::menu::{self, LanguageMenu};
use crate::icons::{Glyph, icon_button_with_key, selected_by};

impl Compose {
    /// Correct: lit while a correction runs or waits for the user, off while there is nothing to correct.
    pub(super) fn correct_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let (tip, lit, can) = self.correct_state(cx);
        selected_by(
            "compose-correct",
            icon_button_with_key("compose-correct", Glyph::SPELL_CHECK, tip, &CorrectDraft)
                .with_size(Size::Size(px(TOOL)))
                .selected(lit)
                .disabled(!lit && !can)
                .on_click(cx.listener(|this, _, window, cx| this.correct_draft(window, cx))),
        )
        .into_any_element()
    }

    /// Translate, lit while it runs, and the chevron beside it that opens the language menu.
    pub(super) fn translate_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let (tip, busy, can) = self.translate_state(cx);
        let button = selected_by(
            "compose-translate",
            icon_button_with_key("compose-translate", Glyph::LANGUAGES, tip, &TranslateDraft)
                .with_size(Size::Size(px(TOOL)))
                .selected(busy)
                .disabled(!busy && !can)
                .on_click(cx.listener(|this, _, window, cx| this.translate_draft(window, cx))),
        );
        div()
            .flex()
            .items_center()
            .child(button)
            .children(self.language_menu().map(|menu| self.language_chevron(&menu, cx)))
            .into_any_element()
    }

    /// The chevron by Translate (`compose-language`): it opens the language menu over it.
    fn language_chevron(&self, menu: &Entity<LanguageMenu>, cx: &mut Context<Self>) -> AnyElement {
        let language = &self.settings.read(cx).settings().draft_translate_to;
        let tip = format!("Translation language: {language}");
        let face = Button::new("compose-language")
            .ghost()
            .icon(Glyph::CHEVRON_DOWN.icon().size(px(12.0)))
            .with_size(Size::XSmall)
            .w(px(LANGUAGE_CHEVRON))
            .h(px(TOOL))
            .accessibility_label(tip.clone())
            .tooltip(tip);
        selected_by(
            "compose-language",
            menu::popover(menu, "compose-language-popover", gpui::Anchor::BottomRight, face, cx),
        )
        .into_any_element()
    }
}
