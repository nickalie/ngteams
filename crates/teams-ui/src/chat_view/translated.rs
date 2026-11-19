//! Another person's text message translated on its own ([`crate::chat_translate`], a message at a time): "Translate"
//! on its hover bar and in its "More options", "See original message" once it shows translated ([`Offer`]). With no
//! language picked, it opens the chat header's language menu, whose own open state drives its popover (a post's
//! thread uses its channel's); it is offered only where that menu is drawn. It writes nothing to Teams, so it isn't
//! a [`super::MessageAction`] and is offered in the app's read-only mode too.

use gpui::{
    AnyElement, App, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, rgb,
};
use gpui_component::{Sizable as _, Size};

use super::{BAR_BUTTON, ChatView, MessageItem};
use crate::chat_translate::Offer;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::Palette;

impl ChatView {
    /// What `item` offers for its translation now: with no language picked, only where the language menu is drawn.
    fn translation_offer(&self, item: &MessageItem, cx: &App) -> Option<Offer> {
        item.translate
            .filter(|offer| *offer != Offer::PickLanguage || self.translation.read(cx).picker().is_some())
    }

    /// Translate (or See original message) on `item`'s hover bar, before "More options".
    pub(super) fn translate_button(&self, item: &MessageItem, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let offer = self.translation_offer(item, cx)?;
        let message = item.id.clone();
        let id = ElementId::Name(format!("action-translate-{message}").into());
        let button = icon_button(id, Glyph::LANGUAGES, offer.label())
            .with_size(Size::Size(px(BAR_BUTTON)))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.translate_message(&message, window, cx);
            }));
        Some(selected_by("action-translate", button))
    }

    /// Translate (or See original message) in the more-options menu of `message`, after copy and delete.
    pub(super) fn translate_entry(
        &self,
        message: &str,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let item = self.message_items().find(|item| item.id == message)?;
        let offer = self.translation_offer(item, cx)?;
        let message = message.to_owned();
        Some(
            div()
                .id("menu-translate")
                .debug_selector(|| "menu-translate".to_owned())
                .w_full()
                .h(px(32.0))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded_md()
                .cursor_pointer()
                .text_sm()
                .text_color(rgb(palette.foreground))
                .hover(move |row| row.bg(rgb(palette.hover)))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.options = None;
                    this.translate_message(&message, window, cx);
                }))
                .child(Glyph::LANGUAGES.icon().size(px(14.0)))
                .child(div().flex_1().child(offer.label()))
                .into_any_element(),
        )
    }

    /// Translate, or See original message, on `message` ([`crate::chat_translate::ChatTranslation::ask`]); with no
    /// language picked, the chat header's language menu opens.
    pub fn translate_message(&mut self, message: &str, window: &mut Window, cx: &mut Context<Self>) {
        let translation = self.translation.clone();
        if translation.read(cx).language().is_some() {
            translation.update(cx, |translation, cx| translation.ask(message, cx));
            return;
        }
        if let Some(picker) = translation.read(cx).picker().cloned() {
            picker.update(cx, |picker, cx| picker.set_open(true, window, cx));
        }
    }
}
