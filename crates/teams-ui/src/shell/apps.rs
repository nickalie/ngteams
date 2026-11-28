//! The rail's Store in the shell, offered while the account's cloud serves apps (an own server's `appsService`,
//! [`teams_core::Capabilities`]): it takes both panes, as browser Teams' store does; an app's details,
//! the place pickers and a chat's apps open over the window as dialogs.
use super::*;
use crate::apps_page::chat_apps::ChatAppsDialog;
use crate::apps_page::details::{AppDetailsDialog, DetailsContext};
use crate::apps_page::place::{AddToPlaceDialog, Place};
use crate::apps_page::{AppsPage, AppsPageEvent, AppsView};

/// The rail's Store item: Lucide's square-plus (ISC), as Teams' own plus in a square.
pub(super) const STORE: RailItem = RailItem {
    icon: include_bytes!("../../assets/icons/square-plus.svg"),
    label: "Store",
};

/// Whether the account offers the Store and its dialogs: its cloud serves apps.
pub(super) fn apps_offered(core: &dyn Core) -> bool {
    core.capabilities().apps
}

impl Shell {
    /// The rail's Store: the store across both panes, read again.
    pub fn open_apps(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !apps_offered(self.core.as_ref()) {
            return;
        }
        let page = if let Some(page) = &self.apps {
            page.update(cx, AppsPage::refresh);
            page.clone()
        } else {
            let core = Arc::clone(&self.core);
            let page = cx.new(|cx| AppsPage::new(core, window, cx));
            let events = cx.subscribe_in(
                &page,
                window,
                |this: &mut Self, _, event: &AppsPageEvent, window, cx| match event {
                    AppsPageEvent::Details(store) => this.open_app_details(store.as_ref().clone(), window, cx),
                },
            );
            self.apps = Some(page.clone());
            self.apps_events = Some(events);
            page
        };
        page.update(cx, |page, cx| page.show(AppsView::Store, window, cx));
        if self.main != Main::Apps {
            self.trace.record(&TraceLine::new("apps", "opened"));
            self.hide_calendar(window, cx);
            self.leave_workflows(cx);
            let before = std::mem::replace(&mut self.main, Main::Apps);
            if !matches!(before, Main::Settings | Main::Calendar | Main::Search | Main::Workflows) {
                self.before_settings = before;
            }
            self.put_chat_away(cx);
            self.close_switcher(window, cx);
            self.close_search(cx);
        }
        cx.notify();
    }

    /// An app's dialog over the window ([`crate::app_dialog`]), when the account offers apps and writes.
    pub fn open_app_dialog(
        &mut self,
        seed: crate::app_dialog::DialogSeed,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !apps_offered(self.core.as_ref()) || self.core.read_only() {
            return;
        }
        let core = Arc::clone(&self.core);
        let dialog = cx.new(|cx| crate::app_dialog::AppDialog::new(core, seed, window, cx));
        self.show_team_dialog(&dialog, window, cx);
    }

    /// An app dialog's result card goes into the box that writes to `conversation`: the chat's, or in a channel the
    /// posts' box or the post's thread's ([`ChannelView::add_app_card`]); when no box shown writes there, a toast says
    /// that the card was not kept.
    pub(super) fn app_card_to_compose(
        &mut self,
        conversation: &str,
        card: teams_proto::chat::AppCard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let taken = match &self.main {
            Main::Chat(chat) if chat == conversation => match &self.chat {
                Some(view) => {
                    view.update(cx, |view, cx| view.add_app_card(card, window, cx));
                    true
                }
                None => false,
            },
            Main::Channel(_) => self
                .channel
                .as_ref()
                .is_some_and(|(view, _)| view.update(cx, |view, cx| view.add_app_card(conversation, card, window, cx))),
            _ => false,
        };
        if !taken {
            self.show_toast(crate::app_dialog::NO_BOX, cx);
        }
    }

    /// The store, once it was opened.
    #[must_use]
    pub fn apps(&self) -> Option<&Entity<AppsPage>> {
        self.apps.as_ref()
    }

    /// An app was added or removed: the store reads again and the toast says `words`.
    pub(super) fn apps_changed(&mut self, words: &'static str, cx: &mut Context<Self>) {
        if let Some(page) = &self.apps {
            page.update(cx, AppsPage::refresh);
        }
        self.teams.update(cx, TeamsList::reload);
        self.show_toast(words, cx);
    }

    /// The 1:1 chat with the bot of `store` that a person chats with, when the chat list has it.
    fn bot_chat(&self, store: &teams_proto::StoreApp, cx: &App) -> Option<String> {
        let bot = store.definition.personal_bot()?;
        self.list.read(cx).one_to_one_with(bot.as_str()).map(str::to_owned)
    }

    /// An app's details over the window.
    pub fn open_app_details(&mut self, store: teams_proto::StoreApp, window: &mut Window, cx: &mut Context<Self>) {
        self.open_app_details_from(store, None, window, cx);
    }

    /// [`Self::open_app_details`], from `chat` (the app's bot's chat, whose About opened it): the app is the account's,
    /// and Open goes back to that chat.
    pub(super) fn open_app_details_from(
        &mut self,
        store: teams_proto::StoreApp,
        chat: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !apps_offered(self.core.as_ref()) {
            return;
        }
        let known = self.apps.as_ref().map_or_else(DetailsContext::default, |page| {
            let page = page.read(cx);
            DetailsContext {
                rating: page.rating(&store.app.id),
                added: page.added(&store.app.id),
                bot_chat: None,
                picture: page.picture_of(&store.app.id),
            }
        });
        let known = match chat {
            Some(chat) => DetailsContext {
                added: true,
                bot_chat: Some(chat),
                ..known
            },
            None => DetailsContext {
                bot_chat: self.bot_chat(&store, cx),
                ..known
            },
        };
        let core = Arc::clone(&self.core);
        let dialog = cx.new(|cx| AppDetailsDialog::new(core, store, known, window, cx));
        self.show_team_dialog(&dialog, window, cx);
    }

    /// "Add to a chat" (`team` false) or "Add to a team" of `store`: the picker of the account's chats or teams.
    pub(super) fn open_add_app_to(
        &mut self,
        store: teams_proto::StoreApp,
        team: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !apps_offered(self.core.as_ref()) {
            return;
        }
        let places: Vec<Place> = if team {
            self.teams
                .read(cx)
                .teams()
                .unwrap_or_default()
                .iter()
                .filter(|team| !team.archived)
                .map(|team| Place {
                    id: team.id.clone(),
                    name: team.name.clone(),
                })
                .collect()
        } else {
            self.list
                .read(cx)
                .rows()
                .iter()
                .filter(|row| {
                    matches!(
                        row.kind,
                        crate::chat_list::RowKind::OneToOne
                            | crate::chat_list::RowKind::Group
                            | crate::chat_list::RowKind::Meeting
                    )
                })
                .map(|row| Place {
                    id: row.id.clone(),
                    name: row.name.to_string(),
                })
                .collect()
        };
        let core = Arc::clone(&self.core);
        let dialog = cx.new(|cx| AddToPlaceDialog::new(core, store, team, places, window, cx));
        self.show_team_dialog(&dialog, window, cx);
    }

    /// The apps of chat `chat` over the window.
    pub fn open_chat_apps(&mut self, chat: String, window: &mut Window, cx: &mut Context<Self>) {
        if !apps_offered(self.core.as_ref()) {
            return;
        }
        let core = Arc::clone(&self.core);
        let dialog = cx.new(|cx| ChatAppsDialog::new(core, chat, window, cx));
        self.show_team_dialog(&dialog, window, cx);
    }

    /// The rail's Store item, while the account offers the Store.
    pub(super) fn store_rail(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        apps_offered(self.core.as_ref()).then(|| {
            Self::rail_button(
                "rail-apps",
                STORE,
                self.main == Main::Apps,
                palette,
                cx,
                Self::open_apps,
            )
        })
    }

    /// A chat header's "Apps in this chat", while the account offers the Store.
    pub(super) fn chat_apps_button(&self, chat: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !apps_offered(self.core.as_ref()) {
            return None;
        }
        let chat = chat.to_owned();
        Some(
            crate::icons::selected_by(
                "chat-apps",
                crate::icons::icon_button("chat-apps", crate::icons::Glyph::PLUS, "Apps in this chat")
                    .on_click(cx.listener(move |this, _, window, cx| this.open_chat_apps(chat.clone(), window, cx))),
            )
            .into_any_element(),
        )
    }
}
