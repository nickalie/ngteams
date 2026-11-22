//! A message translated from its "More options" menu, as Teams offers it: the menu's "Translation" row opens its list
//! beside the menu, with what the message offers ([`chat_translate::Offer`]) and "Settings". "Translate" shows the
//! message in the "Translate to" language, by the chat's translator
//! ([`crate::chat_translate::ChatTranslation::translate_message`]); "See original message (German)" shows it as
//! written again. In a chat read in a language, the list switches a translated message's two forms. "Settings" opens
//! the settings' page that holds the "Translate to" language. The selected message's key is T.

use gpui::{
    AnyElement, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _, Window, div,
    px,
};
use gpui_component::kbd::Kbd;
use gpui_component::{Sizable as _, Size};

use super::{BAR_BUTTON, CONTEXT, ChatView, ChatViewEvent, MessageItem, SideList, TranslateSelected, measured};
use crate::chat_translate;
use crate::icons::{Glyph, icon_button_with_key, selected_by};
use crate::theme::Palette;
use crate::workflow_run::menu;

impl ChatView {
    /// The more-options menu's "Translation" row for `message`, with its chevron, when the message offers one: it opens
    /// and closes its list beside the menu.
    pub(super) fn translation_entry(
        &self,
        message: &str,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.offer_of(message)?;
        let open = self.side_open(SideList::Translation, message);
        let view = cx.entity().downgrade();
        let shown = message.to_owned();
        Some(menu::opener_row(
            "action-translation",
            (Glyph::LANGUAGES.icon().size(px(14.0)).into_any_element(), "Translation"),
            (open, self.menu_keys.opener(SideList::Translation)),
            palette,
            move |event, _, cx| {
                let keyed = event.is_keyboard();
                let _ = view.update(cx, |this, cx| {
                    this.toggle_side_list(SideList::Translation, &shown, cx);
                    if keyed {
                        this.side_list_keyed();
                    }
                });
            },
        ))
    }

    /// Translate on the hover bar, when the user shows it there and the message offers it: does what the message
    /// offers ("Translate", or "See original message"), as that row of "Translation" does.
    pub(super) fn translate_button(item: &MessageItem, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let offer = item.translate?;
        let message = item.id.clone();
        let id = ElementId::Name(format!("action-translate-{message}").into());
        let button = icon_button_with_key(id, Glyph::LANGUAGES, offer.label().to_string(), &TranslateSelected)
            .with_size(Size::Size(px(BAR_BUTTON)))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.translate(&message, cx);
            }));
        Some(selected_by("action-translate", button))
    }

    /// The "Translation" list beside the menu, while it is open: what the message offers, with its key, then
    /// "Settings".
    pub(super) fn translation_menu(
        &self,
        message: &str,
        palette: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.side_open(SideList::Translation, message) {
            return None;
        }
        let offer = self.offer_of(message)?;
        let key = Kbd::binding_for_action(&TranslateSelected, Some(CONTEXT), window).map(Kbd::outline);
        let view = cx.entity().downgrade();
        let chosen = message.to_owned();
        let translate = menu::row(
            "action-translate",
            div().flex_none().size(px(14.0)).into_any_element(),
            offer.label().to_string(),
            key.map(IntoElement::into_any_element),
            palette,
            move |_, _, cx| {
                let _ = view.update(cx, |this, cx| this.translate(&chosen, cx));
            },
        );
        let view = cx.entity().downgrade();
        let settings = menu::row(
            "action-translation-settings",
            Glyph::SETTINGS.icon().size(px(14.0)).into_any_element(),
            "Settings".to_owned(),
            None,
            palette,
            move |_, _, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.close_options();
                    cx.emit(ChatViewEvent::TranslationSettings);
                    cx.notify();
                });
            },
        );
        Some(
            menu::panel("message-translation-menu", palette)
                .track_focus(&self.menu_keys.side)
                .relative()
                .child(measured(&self.menu_keys.side_at))
                .child(translate)
                .child(menu::separator(palette))
                .child(settings)
                .into_any_element(),
        )
    }

    /// Shows the open menu's "Translation" list, as its row does.
    pub fn open_translation_list(&mut self, cx: &mut Context<Self>) {
        if self.options.is_some() {
            self.side_list = self.options.clone().map(|message| (SideList::Translation, message));
            cx.notify();
        }
    }

    /// What `message` offers for its translation, as the view last drew it.
    fn offer_of(&self, message: &str) -> Option<chat_translate::Offer> {
        self.message_items().find(|item| item.id == message)?.translate
    }

    /// Carries out what `message` offers for its translation: translates it, or shows its other form.
    pub fn translate(&mut self, message: &str, cx: &mut Context<Self>) {
        let Some(offer) = self.offer_of(message) else {
            return;
        };
        self.close_options();
        self.translation
            .update(cx, |translation, cx| translation.offered(offer, message, cx));
        cx.notify();
    }

    /// T on the selected message.
    pub(super) fn translate_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(message) = self.selected.clone() {
            self.translate(&message, cx);
        }
    }

    fn close_options(&mut self) {
        self.options = None;
        self.side_list = None;
    }

    /// Whether the menu of `message` shows `list` beside it.
    pub(super) fn side_open(&self, list: SideList, message: &str) -> bool {
        self.side_list
            .as_ref()
            .is_some_and(|(open, shown)| *open == list && shown == message)
    }

    /// Opens `list` beside the menu of `message`, or closes the list open there.
    pub(super) fn toggle_side_list(&mut self, list: SideList, message: &str, cx: &mut Context<Self>) {
        self.side_list = if self.side_open(list, message) {
            None
        } else {
            Some((list, message.to_owned()))
        };
        cx.notify();
    }
}
