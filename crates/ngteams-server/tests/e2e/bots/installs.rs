//! The app catalogue and installs, driven by the real engine of an own-server account: the store and the apps'
//! definitions on the account's middle tier, an app's icon on the server's own origin, an app added for the account,
//! to a group chat and to a team, and what its bot gets of each (`installationUpdate`, `conversationUpdate`, a
//! channel mention and its reply in the post's thread). Apps, bots and names are synthetic.

use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::Method;
use serde_json::json;
use teams_core::engine::Action;
use teams_proto::chat::{MessageText, Node, RichText};
use teams_proto::{AppCapability, AppDefinition, Mri, TeamWrite};
use teams_store::OutboxState;

use super::connector::{loopback, personal_chat};
use super::endpoint::{Connector, ORIGIN, TestBot, encode};
use super::{Registry, good_package, png};
use crate::support::{Backend, backends};
use crate::teams::Seat;

/// The packaged app and its bot (the manifest's ids).
const APP: &str = "6b1c2f4e-3a8d-4c51-9e27-1f0a2b3c4d5e";
const BOT: &str = "7c2d3e5f-4b9e-4d62-8f38-2a1b3c4d5e6f";
const FORM_BOT: &str = "Helper (synthetic)";

/// A server with an administrator, the packaged app (its bot at a test endpoint) and a bot from the form, and Ada with
/// her engine.
struct Shop {
    registry: Registry,
    ada: Seat,
    bot: TestBot,
    secret: String,
    form: String,
}

impl Shop {
    async fn open(backend: Backend) -> Self {
        let registry = Registry::open_with(backend, loopback).await;
        let (status, answer) = registry
            .upload(
                &format!("{}/package", super::APPS),
                Method::POST,
                good_package(APP, BOT, "1.2.0"),
            )
            .await;
        assert_eq!(status, 201, "{answer}");
        let secret = answer["secret"].as_str().unwrap().to_owned();
        let bot = TestBot::start().await;
        let (status, answer) = registry
            .call(Method::PATCH, &super::app_path(APP), Some(json!({"endpoint": bot.url})))
            .await;
        assert_eq!(status, 200, "{answer}");
        let form = registry
            .register(FORM_BOT, "https://helper.example.test/api/messages")
            .await;
        let form = form["id"].as_str().unwrap().to_owned();
        let ada = Seat::new(&registry.server, "ada@own.test", "Ada Lovelace").await;
        Self {
            registry,
            ada,
            bot,
            secret,
            form,
        }
    }

    /// The packaged bot's connector, signed with its own token.
    async fn connector(&self, service_url: &str) -> Connector {
        Connector::signed(&self.registry.server, service_url, BOT, &self.secret).await
    }

    fn stop(self) {
        self.ada.stop();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_engine_lists_the_store_reads_definitions_and_loads_an_apps_icon() {
    for backend in backends() {
        let shop = Shop::open(backend).await;
        let server = &shop.registry.server;
        // an app that is off, and one for listed people only, are in nobody else's store
        let off = shop
            .registry
            .register("Off (synthetic)", "https://off.example.test/api")
            .await;
        let off = off["id"].as_str().unwrap().to_owned();
        let (status, _) = shop
            .registry
            .call(Method::PATCH, &super::app_path(&off), Some(json!({"enabled": false})))
            .await;
        assert_eq!(status, 200);
        let few = shop
            .registry
            .register("Few (synthetic)", "https://few.example.test/api")
            .await;
        let few = few["id"].as_str().unwrap().to_owned();
        let (status, _) = shop
            .registry
            .call(
                Method::PATCH,
                &super::app_path(&few),
                Some(json!({"everyone": false, "users": []})),
            )
            .await;
        assert_eq!(status, 200);

        let session = server.session("ada@own.test").await;
        session.skypetoken().await.unwrap();
        assert!(
            session.service_base("appsService").is_some(),
            "authz names no appsService"
        );

        let listing = shop.ada.engine.app_listing().await.unwrap();
        assert_eq!(listing.sections.org, [shop.form.clone(), APP.to_owned()], "{listing:?}");
        assert!(!listing.sections.ids().contains(&off) && !listing.sections.ids().contains(&few));
        let polls = listing.app(APP).unwrap();
        assert_eq!(polls.app.name, "Polls (synthetic)");
        assert_eq!(polls.app.developer, "Synthetic Tools");
        assert_eq!(polls.app.description, "Quick polls");
        assert_eq!(polls.app.accent, Some(0x004F_6BED));
        assert!(polls.has_bot);
        let definition = &polls.definition;
        assert_eq!(definition.bots(), [Mri::new(format!("28:{BOT}"))]);
        assert_eq!(definition.version(), Some("1.2.0"));
        assert_eq!(
            definition.long_description(),
            Some("Runs quick polls in chats and channels. Synthetic.")
        );
        assert_eq!(definition.privacy_url(), Some("https://tools.example.test/privacy"));
        assert_eq!(definition.terms_url(), Some("https://tools.example.test/terms"));
        assert_eq!(definition.developer_url(), Some("https://tools.example.test"));
        assert_eq!(
            definition.capabilities(),
            [AppCapability::Bot, AppCapability::Messaging]
        );
        let places = definition.places();
        assert!(places.personal && places.chat && places.team, "{places:?}");
        assert_eq!(definition.mentionable_bot(true), Some(Mri::new(format!("28:{BOT}"))));
        let commands = definition.extension_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(
            (commands[0].id.as_str(), commands[0].title.as_str()),
            ("searchPolls", "Search polls")
        );

        let icon = definition.large_image_url().unwrap().to_owned();
        assert!(icon.starts_with(&format!("{ORIGIN}/teamsappdata/")), "{icon}");
        let picture = shop.ada.engine.app_picture(icon).await.unwrap().unwrap();
        assert_eq!(picture.bytes, png(192, 192));

        let helper = listing.app(&shop.form).unwrap();
        assert_eq!(helper.app.name, FORM_BOT);
        assert_eq!(helper.definition.large_image_url(), None);
        assert_eq!(
            helper.definition.personal_bot(),
            Some(Mri::new(format!("28:{}", shop.form)))
        );

        // Manage team's "Add an app" reads the store's categories, the bot index the definitions by id
        let store = shop.ada.engine.app_store().await.unwrap();
        let ids: Vec<&str> = store.iter().map(|app| app.app.id.as_str()).collect();
        assert!(ids.contains(&APP) && ids.contains(&shop.form.as_str()), "{ids:?}");
        let read = shop
            .ada
            .engine
            .app_definition(shop.form.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read.name(), Some(FORM_BOT));
        let off_read = shop.ada.engine.app_definition(off.clone()).await.unwrap();
        assert!(off_read.is_none(), "{off_read:?}");
        let bot_app = shop.ada.engine.bot_app(format!("28:{BOT}")).await.unwrap().unwrap();
        assert_eq!(bot_app.app.id, APP);
        shop.stop();
    }
}

/// Ada's definition of the packaged app, as the store gave it.
async fn polls(seat: &Seat) -> AppDefinition {
    let listing = seat.engine.app_listing().await.unwrap();
    listing.app(APP).unwrap().definition.clone()
}

/// `seat`'s message to `conversation` that mentions the packaged app's bot, composed as the client composes it.
async fn mention(seat: &Seat, conversation: &str, words: &str) -> String {
    let bot = Mri::new(format!("28:{BOT}"));
    let text = RichText {
        nodes: vec![Node::Paragraph(vec![
            Node::Mention {
                item_id: "0".into(),
                text: "Polls (synthetic)".into(),
            },
            Node::Text {
                text: format!(" {words}"),
                marks: teams_proto::chat::Marks::NONE,
            },
        ])],
        truncated: false,
    };
    let apps = BTreeMap::from([(bot.clone(), APP.to_owned())]);
    let text = MessageText::new_with_bots(text, [("0".to_owned(), bot)], &apps).unwrap();
    send(seat, conversation, text).await
}

async fn say(seat: &Seat, conversation: &str, words: &str) -> String {
    send(seat, conversation, MessageText::plain(words).unwrap()).await
}

async fn send(seat: &Seat, conversation: &str, text: MessageText) -> String {
    let row = seat
        .engine
        .write(Action::Send {
            conversation: conversation.to_owned(),
            text,
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    row.message_id.unwrap()
}

/// Whether a message activity reaches the bot within a few seconds.
async fn reaches(bot: &TestBot) -> bool {
    bot.gets_within("message", Duration::from_secs(3)).await
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_added_for_the_account_tells_its_bot_in_their_chat_and_lists_as_theirs() {
    for backend in backends() {
        let shop = Shop::open(backend).await;
        let definition = polls(&shop.ada).await;
        let written = shop
            .ada
            .engine
            .team_write(TeamWrite::AddPersonalApp { app: definition }, None)
            .await;
        assert!(written.is_ok(), "{written:?}");
        let chat = personal_chat(&shop.ada.id, BOT);
        let got = shop.bot.next_of("installationUpdate").await.activity;
        assert_eq!(got["action"], "add", "{got}");
        assert_eq!(got["conversation"]["id"], chat);
        assert_eq!(got["conversation"]["conversationType"], "personal");
        assert_eq!(got["from"]["id"], format!("29:{}", shop.ada.id));
        assert_eq!(got["recipient"]["id"], format!("28:{BOT}"));
        let members = shop.bot.next_of("conversationUpdate").await.activity;
        assert_eq!(
            members["membersAdded"],
            json!([{"id": format!("28:{BOT}")}]),
            "{members}"
        );
        let mine = shop.ada.engine.user_apps().await.unwrap();
        assert_eq!(mine.iter().map(|app| app.app.id.as_str()).collect::<Vec<_>>(), [APP]);
        // the bot's chat is there, and the bot gets what is said in it
        say(&shop.ada, &chat, "hello polls").await;
        let message = shop.bot.next_of("message").await.activity;
        assert_eq!(message["text"], "hello polls");

        let removed = shop
            .ada
            .engine
            .team_write(TeamWrite::RemovePersonalApp { app: APP.into() }, None)
            .await;
        assert!(removed.is_ok(), "{removed:?}");
        let got = shop.bot.next_of("installationUpdate").await.activity;
        assert_eq!(got["action"], "remove", "{got}");
        assert!(shop.ada.engine.user_apps().await.unwrap().is_empty());
        shop.stop();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_added_to_a_group_chat_seats_its_bot_which_gets_only_mentions_until_it_is_removed() {
    for backend in backends() {
        let shop = Shop::open(backend).await;
        let bea = Seat::new(&shop.registry.server, "bea@own.test", "Bea Lovelace").await;
        let made = shop
            .ada
            .engine
            .write(Action::CreateChat {
                others: vec![Mri::new(&bea.mri)],
                topic: Some("Polls room".into()),
            })
            .await
            .unwrap();
        let chat = made.message_id.unwrap();
        say(&shop.ada, &chat, "before the app").await;
        let definition = polls(&shop.ada).await;
        let written = shop
            .ada
            .engine
            .team_write(
                TeamWrite::AddChatApp {
                    chat: chat.clone(),
                    app: definition,
                },
                None,
            )
            .await;
        assert!(written.is_ok(), "{written:?}");
        let got = shop.bot.next_of("installationUpdate").await.activity;
        assert_eq!(got["action"], "add", "{got}");
        assert_eq!(got["conversation"]["id"], chat);
        assert_eq!(got["conversation"]["conversationType"], "groupChat");
        assert_eq!(got["conversation"]["isGroup"], true);
        let members = shop.bot.next_of("conversationUpdate").await.activity;
        assert_eq!(members["membersAdded"][0]["id"], format!("28:{BOT}"), "{members}");

        let added = bea.engine.chat_apps(chat.clone()).await.unwrap();
        assert_eq!(added.iter().map(|app| app.app.id.as_str()).collect::<Vec<_>>(), [APP]);
        let mentionable = bea.engine.mentionable_bots(chat.clone(), false).await.unwrap();
        assert_eq!(mentionable.len(), 1);

        say(&bea, &chat, "not for the bot").await;
        assert!(!reaches(&shop.bot).await, "a message without a mention reached the bot");
        let sent = mention(&bea, &chat, "start a poll").await;
        let message = shop.bot.next_of("message").await.activity;
        assert_eq!(message["id"], sent);
        assert_eq!(message["conversation"]["conversationType"], "groupChat");
        assert_eq!(message["text"], "<at>Polls (synthetic)</at> start a poll", "{message}");
        let entity = &message["entities"][1];
        assert_eq!(entity["type"], "mention", "{message}");
        assert_eq!(entity["mentioned"]["id"], format!("28:{BOT}"));
        assert_eq!(entity["text"], "<at>Polls (synthetic)</at>");
        assert_eq!(message["from"]["id"], format!("29:{}", bea.id));
        let connector = shop.connector(message["serviceUrl"].as_str().unwrap()).await;
        let (status, answer) = connector.reply(&chat, &sent, Connector::text("poll started")).await;
        assert_eq!(status, 200, "{answer}");

        let removed = bea
            .engine
            .team_write(
                TeamWrite::RemoveChatApp {
                    chat: chat.clone(),
                    app: APP.into(),
                },
                None,
            )
            .await;
        assert!(removed.is_ok(), "{removed:?}");
        let got = shop.bot.next_of("installationUpdate").await.activity;
        assert_eq!(got["action"], "remove", "{got}");
        assert!(bea.engine.chat_apps(chat.clone()).await.unwrap().is_empty());
        mention(&shop.ada, &chat, "anyone there").await;
        assert!(!reaches(&shop.bot).await, "a removed bot got a mention");
        let (status, _) = connector.send(&chat, Connector::text("still here")).await;
        assert_eq!(status, 403);
        bea.stop();
        shop.stop();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_added_to_a_team_hears_mentions_in_its_channels_and_its_reply_joins_the_posts_thread() {
    for backend in backends() {
        let shop = Shop::open(backend).await;
        let bea = Seat::new(&shop.registry.server, "bea@own.test", "Bea Lovelace").await;
        let (team, group) = shop.ada.make_team("Polling crew", false).await;
        shop.ada.add(&team, &group, &[&bea]).await;
        let definition = polls(&shop.ada).await;
        let written = shop
            .ada
            .engine
            .team_write(
                TeamWrite::AddApp {
                    team: team.clone(),
                    app: definition,
                },
                None,
            )
            .await;
        assert!(written.is_ok(), "{written:?}");
        let got = shop.bot.next_of("installationUpdate").await.activity;
        assert_eq!(got["action"], "add", "{got}");
        assert_eq!(got["conversation"]["id"], team);
        assert_eq!(got["conversation"]["conversationType"], "channel");
        assert_eq!(got["channelData"]["team"]["id"], team);
        assert_eq!(got["channelData"]["team"]["name"], "Polling crew");
        assert_eq!(got["channelData"]["team"]["aadGroupId"], group);
        assert_eq!(got["channelData"]["channel"]["id"], team);
        let members = shop.bot.next_of("conversationUpdate").await.activity;
        assert_eq!(members["channelData"]["eventType"], "teamMemberAdded", "{members}");
        assert_eq!(members["membersAdded"][0]["id"], format!("28:{BOT}"));

        let apps = bea.engine.team_apps(team.clone()).await.unwrap();
        assert_eq!(apps.iter().map(|app| app.id.as_str()).collect::<Vec<_>>(), [APP]);
        let mentionable = bea.engine.mentionable_bots(team.clone(), true).await.unwrap();
        assert_eq!(mentionable.len(), 1);

        say(&bea, &team, "a post for people").await;
        assert!(!reaches(&shop.bot).await, "a post without a mention reached the bot");
        let post = mention(&bea, &team, "how many are coming?").await;
        let message = shop.bot.next_of("message").await.activity;
        assert_eq!(message["id"], post);
        assert_eq!(
            message["conversation"]["id"],
            format!("{team};messageid={post}"),
            "{message}"
        );
        assert_eq!(message["conversation"]["conversationType"], "channel");
        assert_eq!(message["channelData"]["channel"]["id"], team);
        assert_eq!(message["channelData"]["teamsTeamId"], team);
        assert_eq!(message["text"], "<at>Polls (synthetic)</at> how many are coming?");

        let service_url = message["serviceUrl"].as_str().unwrap();
        let connector = shop.connector(service_url).await;
        let conversation = message["conversation"]["id"].as_str().unwrap();
        let (status, answer) = connector.send(conversation, Connector::text("Poll: **coming?**")).await;
        assert_eq!(status, 200, "{answer}");
        let reply = answer["id"].as_str().unwrap().to_owned();
        let roots = bea.engine.reply_roots(team.clone()).await.unwrap();
        bea.engine.sync_conversation(team.clone()).await.unwrap();
        let roots_after = bea.engine.reply_roots(team.clone()).await.unwrap();
        assert!(
            roots_after.contains(&(reply.clone(), post.clone())),
            "{roots:?} then {roots_after:?}"
        );
        let synced = bea.engine.message(team.clone(), reply).await.unwrap().unwrap();
        assert_eq!(synced.sender.as_deref(), Some(format!("28:{BOT}").as_str()));

        // the bot's team, and a channel made after the add
        let (status, details) = connector
            .call(Method::GET, &format!("v3/teams/{}", encode(&team)), None)
            .await;
        assert_eq!(status, 200, "{details}");
        assert_eq!(
            (details["name"].as_str(), details["aadGroupId"].as_str()),
            (Some("Polling crew"), Some(group.as_str()))
        );
        let later = shop.ada.make_channel(&team, &group, "Results", true).await;
        let (status, listed) = connector
            .call(Method::GET, &format!("v3/teams/{}/conversations", encode(&team)), None)
            .await;
        assert_eq!(status, 200, "{listed}");
        let names: Vec<(&str, Option<&str>)> = listed["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| (entry["id"].as_str().unwrap(), entry["name"].as_str()))
            .collect();
        assert!(
            names.contains(&(team.as_str(), None)) && names.contains(&(later.as_str(), Some("Results"))),
            "{names:?}"
        );
        let in_later = mention(&bea, &later, "results please").await;
        let message = shop.bot.next_of("message").await.activity;
        assert_eq!(message["conversation"]["id"], format!("{later};messageid={in_later}"));

        let removed = shop
            .ada
            .engine
            .team_write(
                TeamWrite::RemoveApp {
                    team: team.clone(),
                    app: APP.into(),
                },
                None,
            )
            .await;
        assert!(removed.is_ok(), "{removed:?}");
        let got = shop.bot.next_of("installationUpdate").await.activity;
        assert_eq!(got["action"], "remove", "{got}");
        assert!(bea.engine.team_apps(team.clone()).await.unwrap().is_empty());
        mention(&bea, &team, "still polling?").await;
        mention(&bea, &later, "anyone?").await;
        assert!(!reaches(&shop.bot).await, "a removed bot got a mention");
        let (status, _) = connector
            .send(&format!("{team};messageid={post}"), Connector::text("late"))
            .await;
        assert_eq!(status, 403);
        bea.stop();
        shop.stop();
    }
}
