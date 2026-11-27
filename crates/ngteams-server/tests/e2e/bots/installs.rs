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
use teams_proto::{AppCapability, AppDefinition, Mri, TeamWrite, reply_chain_id};
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

/// An app that is off and one for listed people only, which are in nobody else's store; their ids.
async fn hidden_apps(registry: &Registry) -> (String, String) {
    // an app that is off, and one for listed people only, are in nobody else's store
    let off = registry
        .register("Off (synthetic)", "https://off.example.test/api")
        .await;
    let off = off["id"].as_str().unwrap().to_owned();
    let (status, _) = registry
        .call(Method::PATCH, &super::app_path(&off), Some(json!({"enabled": false})))
        .await;
    assert_eq!(status, 200);
    let few = registry
        .register("Few (synthetic)", "https://few.example.test/api")
        .await;
    let few = few["id"].as_str().unwrap().to_owned();
    let (status, _) = registry
        .call(
            Method::PATCH,
            &super::app_path(&few),
            Some(json!({"everyone": false, "users": []})),
        )
        .await;
    assert_eq!(status, 200);

    (off, few)
}

/// What the packaged app's definition says, as the client reads it.
fn polls_defined(definition: &AppDefinition) {
    // the bot and its message extension's bot, as the client reads a Teams definition
    assert_eq!(
        definition.bots(),
        [Mri::new(format!("28:{BOT}")), Mri::new(format!("28:{BOT}"))]
    );
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
}

/// What the bot gets when its app is added to `team`.
async fn told_of_team(bot: &TestBot, team: &str, group: &str) {
    let got = bot.next_of("installationUpdate").await.activity;
    assert_eq!(got["action"], "add", "{got}");
    assert_eq!(got["conversation"]["id"], team);
    assert_eq!(got["conversation"]["conversationType"], "channel");
    assert_eq!(got["channelData"]["team"]["id"], team);
    assert_eq!(got["channelData"]["team"]["name"], "Polling crew");
    assert_eq!(got["channelData"]["team"]["aadGroupId"], group);
    assert_eq!(got["channelData"]["channel"]["id"], team);
    let members = bot.next_of("conversationUpdate").await.activity;
    assert_eq!(members["channelData"]["eventType"], "teamMemberAdded", "{members}");
    assert_eq!(members["membersAdded"][0]["id"], format!("28:{BOT}"));
}

/// What the bot gets when its app is taken off: `conversationUpdate` (it is gone, with `event` as the team's
/// `eventType`) before `installationUpdate`. `next_of` drops other kinds, so a wrong order fails on the second read.
async fn told_of_removal(bot: &TestBot, conversation: &str, event: Option<&str>) {
    let members = bot.next_of("conversationUpdate").await.activity;
    assert_eq!(
        members["membersRemoved"],
        json!([{"id": format!("28:{BOT}")}]),
        "{members}"
    );
    assert!(members["membersAdded"].is_null(), "{members}");
    assert_eq!(members["conversation"]["id"], conversation);
    assert_eq!(members["channelData"]["eventType"].as_str(), event, "{members}");
    let got = bot.next_of("installationUpdate").await.activity;
    assert_eq!(got["action"], "remove", "{got}");
    assert_eq!(got["conversation"]["id"], conversation);
}

/// A mention in a reply under `post` names the post's chain, not the reply, and the bot's answer joins the same
/// thread.
async fn reply_in_thread(bot: &TestBot, bea: &Seat, connector: &Connector, team: &str, post: &str) {
    let chain = reply_chain_id(team, post).unwrap();
    let in_thread = mention(bea, &chain, "count me in").await;
    assert_ne!(in_thread, post);
    let message = bot.next_of("message").await.activity;
    assert_eq!(message["id"], in_thread);
    assert_eq!(
        message["conversation"]["id"],
        format!("{team};messageid={post}"),
        "{message}"
    );
    let (status, answer) = connector
        .send(
            message["conversation"]["id"].as_str().unwrap(),
            Connector::text("counted"),
        )
        .await;
    assert_eq!(status, 200, "{answer}");
    let counted = answer["id"].as_str().unwrap().to_owned();
    bea.engine.sync_conversation(team.to_owned()).await.unwrap();
    let roots = bea.engine.reply_roots(team.to_owned()).await.unwrap();
    assert!(roots.contains(&(counted, post.to_owned())), "{roots:?}");
}

/// The bot's team and its channels, through the connector.
async fn team_routes(connector: &Connector, team: &str, group: &str, later: &str) {
    // the bot's team, and a channel made after the add
    let (status, details) = connector
        .call(Method::GET, &format!("v3/teams/{}", encode(team)), None)
        .await;
    assert_eq!(status, 200, "{details}");
    assert_eq!(
        (details["name"].as_str(), details["aadGroupId"].as_str()),
        (Some("Polling crew"), Some(group))
    );
    let (status, listed) = connector
        .call(Method::GET, &format!("v3/teams/{}/conversations", encode(team)), None)
        .await;
    assert_eq!(status, 200, "{listed}");
    let names: Vec<(&str, Option<&str>)> = listed["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| (entry["id"].as_str().unwrap(), entry["name"].as_str()))
        .collect();
    assert!(
        names.contains(&(team, None)) && names.contains(&(later, Some("Results"))),
        "{names:?}"
    );
}

/// A personal-only app, which no chat or team takes, uploaded; its definition and the packaged app's.
async fn lone_app(shop: &Shop) -> (AppDefinition, AppDefinition) {
    // a personal-only app, which no chat or team takes
    let mut lone = super::manifest(
        "8d3e4f60-5c0f-4e73-9a49-3b2c4d5e6f70",
        "9e4f5061-6d10-4f84-8b5a-4c3d5e6f7081",
        "1.0.0",
    );
    lone["bots"][0]["scopes"] = json!(["personal"]);
    lone["name"]["short"] = json!("Lone (synthetic)");
    let package = super::package(&lone, &png(192, 192), &png(32, 32), &[]);
    let (status, answer) = shop
        .registry
        .upload(&format!("{}/package", super::APPS), Method::POST, package)
        .await;
    assert_eq!(status, 201, "{answer}");
    let listing = shop.ada.engine.app_listing().await.unwrap();
    let lone = listing
        .app("8d3e4f60-5c0f-4e73-9a49-3b2c4d5e6f70")
        .unwrap()
        .definition
        .clone();
    let polls = listing.app(APP).unwrap().definition.clone();
    (lone, polls)
}

/// Icons: an enabled app's only, its two files only.
async fn icons(shop: &Shop, lone: &AppDefinition) {
    let server = &shop.registry.server;
    // icons: an enabled app's only, its two files only
    let icon = lone.large_image_url().unwrap().to_owned();
    let at = |path: &str| server.url(&path.replacen(ORIGIN, "", 1));
    let get = |url: String| async move { reqwest::get(url).await.unwrap().status().as_u16() };
    assert_eq!(get(at(&icon)).await, 200);
    assert_eq!(get(at(&icon.replace("color.png", "outline.png"))).await, 200);
    assert_eq!(get(at(&icon.replace("color.png", "other.png"))).await, 404);
    assert_eq!(
        get(server.url(&format!("/teamsappdata/apps/{}/color.png", shop.form))).await,
        404
    );
    let (status, _) = shop
        .registry
        .call(
            Method::PATCH,
            &super::app_path("8d3e4f60-5c0f-4e73-9a49-3b2c4d5e6f70"),
            Some(json!({"enabled": false})),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(get(at(&icon)).await, 404);
}

/// How `seat`'s add of `app` to `team`, then its removal of the packaged app, ended: `accepted` or the refusal.
async fn team_answers(seat: &Seat, team: &str, app: &AppDefinition) -> (String, String) {
    let refused = |answer: Result<teams_core::TeamWritten, teams_core::EngineError>| match answer {
        Ok(_) => "accepted".to_owned(),
        refused => crate::teams::refusal(refused).unwrap_or_default(),
    };
    let add = TeamWrite::AddApp {
        team: team.to_owned(),
        app: app.clone(),
    };
    let added = refused(seat.engine.team_write(add, None).await);
    let remove = TeamWrite::RemoveApp {
        team: team.to_owned(),
        app: APP.into(),
    };
    (added, refused(seat.engine.team_write(remove, None).await))
}

/// Team adds and removals the account may not make: on a team it isn't on, by a member while the team's members may
/// not add or remove apps (a closed custom-upload box alone doesn't stop them), and an app whose bot serves no team.
async fn team_refusals(
    shop: &Shop,
    (bea, outsider): (&Seat, &Seat),
    team: &str,
    group_id: &str,
    (polls, lone): (&AppDefinition, &AppDefinition),
) {
    let (added, removed) = team_answers(outsider, team, polls).await;
    assert!(
        added.contains("NotFound") && removed.contains("NotFound"),
        "{added} {removed}"
    );
    assert!(outsider.engine.team_apps(team.to_owned()).await.unwrap().is_empty());
    shop.ada.add(team, group_id, &[bea]).await;
    members_may(&shop.ada, team, false).await;
    let (added, removed) = team_answers(bea, team, polls).await;
    assert!(
        added.contains("Forbidden") && removed.contains("Forbidden"),
        "{added} {removed}"
    );
    members_may(&shop.ada, team, true).await;
    let (added, removed) = team_answers(bea, team, polls).await;
    assert_eq!((added.as_str(), removed.as_str()), ("accepted", "accepted"));
    let (added, _) = team_answers(&shop.ada, team, lone).await;
    assert!(added.contains("BadRequest"), "{added}");
}

/// `owner` lets the members of `team` add and remove apps (`open`) or not; custom uploads stay closed either way.
async fn members_may(owner: &Seat, team: &str, open: bool) {
    let read = owner.engine.team_settings(team).await.unwrap().unwrap();
    let mut settings: serde_json::Value = serde_json::from_str(&read).unwrap();
    settings["userSettings"]["installApp"] = json!(open);
    settings["userSettings"]["uninstallApp"] = json!(open);
    settings["userSettings"]["uploadCustomApp"] = json!(false);
    let write = TeamWrite::Settings {
        team: team.to_owned(),
        settings: settings.to_string(),
    };
    owner.engine.team_write(write, None).await.unwrap();
}

/// `seat` adds the packaged app to `team`.
async fn add_to_team(seat: &Seat, team: &str) {
    let definition = polls(seat).await;
    let written = seat
        .engine
        .team_write(
            TeamWrite::AddApp {
                team: team.to_owned(),
                app: definition,
            },
            None,
        )
        .await;
    assert!(written.is_ok(), "{written:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_engine_lists_the_store_reads_definitions_and_loads_an_apps_icon() {
    for backend in backends() {
        let shop = Shop::open(backend).await;
        let server = &shop.registry.server;
        let (off, few) = hidden_apps(&shop.registry).await;

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
        polls_defined(definition);

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
        let connector = shop.connector(message["serviceUrl"].as_str().unwrap()).await;

        let removed = shop
            .ada
            .engine
            .team_write(TeamWrite::RemovePersonalApp { app: APP.into() }, None)
            .await;
        assert!(removed.is_ok(), "{removed:?}");
        told_of_removal(&shop.bot, &chat, None).await;
        assert!(shop.ada.engine.user_apps().await.unwrap().is_empty());
        // removal stops delivery both ways: Ada keeps the chat, the bot hears and posts nothing there
        say(&shop.ada, &chat, "still there?").await;
        assert!(!reaches(&shop.bot).await, "a removed bot got a message in its 1:1 chat");
        let (status, _) = connector.send(&chat, Connector::text("still here")).await;
        assert_eq!(status, 403);
        // added again, it is back in the chat
        let definition = polls(&shop.ada).await;
        let written = shop
            .ada
            .engine
            .team_write(TeamWrite::AddPersonalApp { app: definition }, None)
            .await;
        assert!(written.is_ok(), "{written:?}");
        say(&shop.ada, &chat, "welcome back").await;
        assert_eq!(shop.bot.next_of("message").await.activity["text"], "welcome back");
        shop.stop();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_added_to_a_group_chat_seats_its_bot_which_gets_only_mentions_until_it_is_removed() {
    for backend in backends() {
        let shop = Shop::open(backend).await;
        let bea = Seat::new(&shop.registry.server, "bea@own.test", "Bea Lovelace").await;
        let cora = shop.registry.server.add_user("cora@own.test", "Cora Lovelace").await;
        let made = shop
            .ada
            .engine
            .write(Action::CreateChat {
                others: vec![Mri::new(&bea.mri), Mri::new(format!("8:orgid:{cora}"))],
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
        told_of_removal(&shop.bot, &chat, None).await;
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
        add_to_team(&shop.ada, &team).await;
        told_of_team(&shop.bot, &team, &group).await;

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
        reply_in_thread(&shop.bot, &bea, &connector, &team, &post).await;

        let later = shop.ada.make_channel(&team, &group, "Results", true).await;
        team_routes(&connector, &team, &group, &later).await;
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
        told_of_removal(&shop.bot, &team, Some("teamMemberRemoved")).await;
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

#[tokio::test(flavor = "multi_thread")]
async fn adds_the_account_may_not_make_are_refused_and_an_icon_is_only_an_enabled_apps() {
    for backend in backends() {
        let shop = Shop::open(backend).await;
        let server = &shop.registry.server;
        let (lone, polls) = lone_app(&shop).await;

        let bea = Seat::new(server, "bea@own.test", "Bea Lovelace").await;
        let cora = Seat::new(server, "cora@own.test", "Cora Lovelace").await;
        let one = shop
            .ada
            .engine
            .write(Action::CreateChat {
                others: vec![Mri::new(&bea.mri)],
                topic: None,
            })
            .await
            .unwrap()
            .message_id
            .unwrap();
        let group = shop
            .ada
            .engine
            .write(Action::CreateChat {
                others: vec![Mri::new(&bea.mri), Mri::new(&cora.mri)],
                topic: Some("Refusals".into()),
            })
            .await
            .unwrap()
            .message_id
            .unwrap();
        let refused = |answer: Result<teams_core::TeamWritten, teams_core::EngineError>| {
            crate::teams::refusal(answer).unwrap_or_else(|| "accepted".to_owned())
        };
        let add_chat = |chat: &str, app: &AppDefinition| TeamWrite::AddChatApp {
            chat: chat.to_owned(),
            app: app.clone(),
        };
        // a 1:1 chat, an app whose bot serves no group chat
        let answer = shop.ada.engine.team_write(add_chat(&one, &polls), None).await;
        assert!(refused(answer).contains("BadRequest"));
        let answer = shop.ada.engine.team_write(add_chat(&group, &lone), None).await;
        assert!(refused(answer).contains("BadRequest"));
        // a chat the account isn't in
        let (team, group_id) = shop.ada.make_team("Closed crew", false).await;
        let outsider = Seat::new(server, "dan@own.test", "Dan Lovelace").await;
        let answer = outsider.engine.team_write(add_chat(&group, &polls), None).await;
        assert!(refused(answer).contains("Forbidden"));
        assert!(outsider.engine.chat_apps(group.clone()).await.unwrap().is_empty());
        // nor take an app off it
        let remove_chat = TeamWrite::RemoveChatApp {
            chat: group.clone(),
            app: APP.into(),
        };
        let answer = outsider.engine.team_write(remove_chat, None).await;
        assert!(refused(answer).contains("Forbidden"));
        team_refusals(&shop, (&bea, &outsider), &team, &group_id, (&polls, &lone)).await;
        let add_team = |app: &AppDefinition| TeamWrite::AddApp {
            team: team.clone(),
            app: app.clone(),
        };
        // an app the account may not use
        let (status, _) = shop
            .registry
            .call(
                Method::PATCH,
                &super::app_path(APP),
                Some(json!({"everyone": false, "users": []})),
            )
            .await;
        assert_eq!(status, 200);
        let answer = bea.engine.team_write(add_team(&polls), None).await;
        assert!(refused(answer).contains("NotFound"));
        let answer = bea
            .engine
            .team_write(TeamWrite::AddPersonalApp { app: polls.clone() }, None)
            .await;
        assert!(refused(answer).contains("NotFound"));
        assert!(bea.engine.app_definition(APP.to_owned()).await.unwrap().is_none());

        icons(&shop, &lone).await;
        for seat in [bea, cora, outsider] {
            seat.stop();
        }
        shop.stop();
    }
}
