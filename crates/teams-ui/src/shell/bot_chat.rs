//! A bot's chat's header, as browser Teams draws one (the Workflows bot's, lab shots of 2026-11-14): the app's icon on
//! its tile and the app's name, then its tabs, Chat underlined and About, which opens the app's details
//! ([`crate::apps_page::details`]) while the Store is offered (a cloud that serves apps). An app's
//! other tabs (Workflows' Home and Templates) are its web pages, which the app doesn't host. No call, no people.

use super::apps::apps_offered;
use super::*;

/// The tile's side in the header, px.
const TILE: f32 = 28.0;

impl Shell {
    /// The header of chat `id` when it is a 1:1 chat with a bot ([`crate::chat_list::ChatRow::bot`]); none for any
    /// other chat.
    pub(super) fn bot_header(&self, id: &str, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let row = self.list.read(cx).row(id)?;
        let bot = row.bot.as_ref()?;
        // the list's rule: the Workflows bot keeps its own icon
        let lead = crate::chat_list::chat_avatar(row, palette, Size::Size(px(TILE)), HEADER_AVATAR);
        let about = (bot.app.is_some() && apps_offered(self.core.as_ref())).then(|| {
            let chat = id.to_owned();
            bot_tab("chat-bot-tab-about", "About", false, palette)
                .on_click(cx.listener(move |this, _, window, cx| this.open_bot_about(&chat, window, cx)))
        });
        let tabs = div()
            .debug_selector(|| "chat-bot-tabs".to_owned())
            .h_full()
            .flex()
            .items_center()
            .gap_4()
            .child(bot_tab("chat-bot-tab-chat", "Chat", true, palette))
            .children(about);
        Some(pane_header_led(Some(lead), row.name.clone(), palette).child(tabs))
    }

    /// A bot chat's About: its app's details over the window, Open going back to the chat.
    pub fn open_bot_about(&mut self, chat: &str, window: &mut Window, cx: &mut Context<Self>) {
        let app = self
            .list
            .read(cx)
            .row(chat)
            .and_then(|row| row.bot.as_ref())
            .and_then(|bot| bot.app.as_ref())
            .map(|app| app.app.clone());
        if let Some(store) = app {
            self.open_app_details_from(store, Some(chat.to_owned()), window, cx);
        }
    }
}

/// One of a bot chat's tabs, the shown one semibold over the brand's line.
fn bot_tab(id: &'static str, label: &'static str, chosen: bool, palette: Palette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || format!("{id}{}", if chosen { " shown" } else { "" }))
        .h(px(48.0))
        .px_1()
        .relative()
        .flex()
        .items_center()
        .text_color(rgb(if chosen {
            palette.foreground
        } else {
            palette.muted_foreground
        }))
        .when(chosen, |tab| tab.font_weight(gpui::FontWeight::SEMIBOLD))
        .when(!chosen, |tab| {
            tab.cursor_pointer()
                .hover(move |tab| tab.text_color(rgb(palette.foreground)))
        })
        .child(label)
        .child(
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .h(px(2.0))
                .rounded_full()
                .when(chosen, |line| line.bg(theme::brand_gradient(palette, 90.0))),
        )
}
