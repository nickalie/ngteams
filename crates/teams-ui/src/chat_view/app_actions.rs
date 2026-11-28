//! Apps' message actions and dialogs in a chat for an account whose cloud serves apps
//! (`docs/protocol/bots.md`, "Message extensions"):
//!
//! - **"More actions"** on a message lists the message extension commands that run on a message
//!   ([`teams_proto::ExtensionApp::commands_in`]), read each time the menu opens, as Teams lists them: each its app's
//!   picture and its title, an app the account hasn't added in italics. They come after the workflows and before
//!   "Create workflows", whose entries stay as they are. Without the Workflows service the menu still offers "More
//!   actions" for them.
//! - **A command chosen** opens its app's dialog on the message ([`crate::app_dialog`]): the message is read from the
//!   store and described to the app as Teams describes it (`messagePayload`). An app the account hasn't added opens
//!   its details, where it is added.
//! - **A card's `task/fetch` button** answered with a dialog opens it ([`crate::bot_card::CardBotEvent::Dialog`]).
use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::{
    AnyElement, AsyncApp, Context, Entity, IntoElement, ParentElement as _, RenderImage, Styled as _, WeakEntity, div,
};
use teams_proto::bot::{MessagePayload, PayloadSender};
use teams_proto::{CommandContext, ExtensionApp, ExtensionCommand};

use super::{ChatView, ChatViewEvent};
use crate::app_dialog::{DialogApp, DialogSeed, Opened};
use crate::bot_card::{CardBot, CardBotEvent, Target};
use crate::theme::Palette;

/// The apps' message actions a chat's menus offer, and their pictures.
#[derive(Default)]
pub(super) struct AppActions {
    /// The account's apps with a message extension, as last read; none before the first menu opened.
    apps: Option<Vec<ExtensionApp>>,
    pictures: BTreeMap<String, Option<Arc<RenderImage>>>,
}

impl ChatView {
    /// Whether a message's menu offers apps' actions: the account's cloud serves apps, and the
    /// app writes.
    fn offers_app_actions(&self) -> bool {
        self.asks_bots() && !self.core.read_only()
    }

    /// Reads the account's apps with a message extension again, as a message's menu opens.
    pub(super) fn read_app_actions(&mut self, cx: &mut Context<Self>) {
        if !self.offers_app_actions() {
            return;
        }
        let reply = self.core.message_extensions();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let apps = reply.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.held_cards.app_actions.apps = Some(apps);
                this.ask_app_pictures(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// The commands that run on a message, each with its app, in the apps' order.
    #[must_use]
    pub fn message_app_actions(&self) -> Vec<(ExtensionApp, ExtensionCommand)> {
        if !self.offers_app_actions() {
            return Vec::new();
        }
        self.held_cards
            .app_actions
            .apps
            .iter()
            .flatten()
            .flat_map(|app| {
                app.commands_in(CommandContext::Message)
                    .into_iter()
                    .map(move |command| (app.clone(), command))
            })
            .collect()
    }

    fn ask_app_pictures(&mut self, cx: &mut Context<Self>) {
        let wanted: Vec<(String, String)> = self
            .held_cards
            .app_actions
            .apps
            .iter()
            .flatten()
            .filter_map(|app| Some((app.id().to_owned(), app.store.definition.large_image_url()?.to_owned())))
            .collect();
        for (id, url) in wanted {
            if self.held_cards.app_actions.pictures.contains_key(&id) {
                continue;
            }
            self.held_cards.app_actions.pictures.insert(id.clone(), None);
            let reply = self.core.app_picture(url);
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let Ok(Some(bytes)) = reply.await else { return };
                let decoded = cx
                    .background_executor()
                    .spawn(async move { crate::inline::decode_app_tile(&bytes, 32) })
                    .await;
                let Some((image, _)) = decoded else { return };
                let _ = this.update(cx, |this, cx| {
                    this.held_cards.app_actions.pictures.insert(id, Some(Arc::new(image)));
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// The rows of "More actions"' apps' commands for `message`.
    pub(super) fn app_action_rows(&self, message: &str, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.message_app_actions()
            .into_iter()
            .map(|(app, command)| {
                let picture = crate::apps_page::app_picture(
                    &app.store,
                    self.held_cards.app_actions.pictures.get(app.id()).cloned().flatten(),
                    16.0,
                    palette,
                );
                let installed = app.installed;
                let id = format!("message-app-action-{}-{}", app.id(), command.id);
                let (view, message) = (cx.entity().downgrade(), message.to_owned());
                let row = crate::workflow_run::menu::row(
                    id,
                    picture.into_any_element(),
                    command.title.clone(),
                    None,
                    palette,
                    move |_, _, cx| {
                        let _ = view.update(cx, |this, cx| this.run_app_action(&app, &command, &message, cx));
                    },
                );
                // an app the account hasn't added is in italics, as Teams lists it
                if installed {
                    row
                } else {
                    div().italic().child(row).into_any_element()
                }
            })
            .collect()
    }

    /// `command` of `app` chosen on `message`: its dialog opens on the message, or the app's details when the account
    /// hasn't added it.
    pub fn run_app_action(
        &mut self,
        app: &ExtensionApp,
        command: &ExtensionCommand,
        message: &str,
        cx: &mut Context<Self>,
    ) {
        self.options = None;
        self.side_list = None;
        if !app.installed {
            cx.emit(ChatViewEvent::AppDetails(Box::new(app.store.clone())));
            return cx.notify();
        }
        let conversation = self.bot_conversation(message);
        let in_channel = self.place.in_channel();
        let root = in_channel
            .then(|| self.root_of(message).to_owned())
            .filter(|root| root != message);
        let (app, command) = (app.clone(), command.clone());
        let stored_in = teams_proto::stored_conversation(&self.conversation).to_owned();
        let link_in = self.conversation.clone();
        let read = self.core.message(stored_in, message.to_owned());
        let id = message.to_owned();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let Ok(Some(stored)) = read.await else { return };
            let payload = payload_of(&stored, &id, root, &link_in);
            let seed = DialogSeed {
                app: DialogApp::of(&app.store),
                target: Target {
                    bot: command.bot.clone(),
                    app_id: app.id().to_owned(),
                    conversation,
                },
                in_teams: Some(payload.link.clone()),
                opened: Opened::Command {
                    command: Box::new(command),
                    context: CommandContext::Message,
                    message: Some(Box::new(payload)),
                },
            };
            let _ = this.update(cx, |_, cx| cx.emit(ChatViewEvent::AppDialog(Box::new(seed))));
        })
        .detach();
        cx.notify();
    }

    /// Opens the dialog a card's `task/fetch` button of `message` (sent by `sender`) got.
    pub(super) fn follow_card_dialogs(
        &self,
        bot: &Entity<CardBot>,
        message: &str,
        sender: String,
        cx: &mut Context<Self>,
    ) {
        let link = teams_proto::workflow::MessageRun::link_to(&self.conversation, message, None);
        let message = message.to_owned();
        cx.subscribe(bot, move |_, _, event: &CardBotEvent, cx| {
            if let CardBotEvent::Dialog { target, dialog } = event {
                let seed = DialogSeed {
                    app: DialogApp {
                        id: target.app_id.clone(),
                        name: sender.clone(),
                        store: None,
                    },
                    target: target.clone(),
                    opened: Opened::Task {
                        dialog: dialog.clone(),
                        message: Some(message.clone()),
                    },
                    in_teams: Some(link.clone()),
                };
                cx.emit(ChatViewEvent::AppDialog(Box::new(seed)));
            }
        })
        .detach();
    }

    /// An app's card goes into the compose box, as a message extension's result does.
    pub fn add_app_card(
        &mut self,
        card: teams_proto::chat::AppCard,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let compose = self.compose.clone();
        compose.update(cx, |compose, cx| compose.add_app_card(card, window, cx));
    }
}

/// `stored`, the message `id`, as Teams describes it to an app (`messagePayload`); `root` is a channel reply's post.
fn payload_of(stored: &teams_store::Message, id: &str, root: Option<String>, conversation: &str) -> MessagePayload {
    let name = stored.sender_name.clone().unwrap_or_default();
    let sender = stored.sender.clone().unwrap_or_default();
    let from = match sender.strip_prefix("28:") {
        Some(bot) => PayloadSender::App {
            id: bot.to_owned(),
            name,
        },
        None => PayloadSender::User {
            id: teams_proto::object_id(&sender).unwrap_or(&sender).to_owned(),
            name,
        },
    };
    let created = chrono::DateTime::from_timestamp_millis(stored.arrival_time)
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    MessagePayload {
        id: id.to_owned(),
        link: teams_proto::workflow::MessageRun::link_to(conversation, id, root.as_deref()),
        reply_to: root,
        created,
        from,
        html: stored.content.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_is_described_as_teams_describes_it() {
        let stored = teams_store::Message {
            conversation_id: "19:c@thread.v2".to_owned(),
            id: "1".to_owned(),
            version: 1,
            client_message_id: None,
            sender: Some("8:orgid:00000000-0000-4000-8000-00000000000b".to_owned()),
            sender_name: Some("Lab B".to_owned()),
            message_type: "RichText/Html".to_owned(),
            content: "<p>Hi</p>".to_owned(),
            properties: None,
            arrival_time: 1_767_268_800_000,
            compose_time: None,
            edit_time: None,
            delete_time: None,
        };
        let payload = payload_of(&stored, "1", None, "19:c@thread.v2");
        assert_eq!(payload.created, "2026-01-01T12:00:00.000Z");
        assert_eq!(
            payload.from,
            PayloadSender::User {
                id: "00000000-0000-4000-8000-00000000000b".to_owned(),
                name: "Lab B".to_owned()
            }
        );
        assert_eq!(payload.html, "<p>Hi</p>");
        let bot = teams_store::Message {
            sender: Some("28:bot".to_owned()),
            ..stored
        };
        let payload = payload_of(&bot, "2", Some("1".to_owned()), "19:c@thread.tacv2");
        assert!(matches!(payload.from, PayloadSender::App { ref id, .. } if id == "bot"));
        assert_eq!(payload.reply_to.as_deref(), Some("1"));
        assert!(payload.link.contains("parentMessageId=1"));
    }
}
