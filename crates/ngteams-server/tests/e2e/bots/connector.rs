//! The Bot Connector and personal bot chats, driven by the real engine of an own-server account and a test bot on
//! loopback: the first message to a bot makes the chat, the bot gets it as an activity and answers through the
//! connector, and the account syncs the answer. Names, apps and messages are synthetic.

use std::time::Duration;

use base64::Engine as _;
use reqwest::Method;
use serde_json::{Value, json};
use teams_core::engine::{Action, EngineHandle};
use teams_core::{Transient, TransientSent};
use teams_proto::chat::MessageText;
use teams_store::{Message, OutboxState};
use tempfile::TempDir;

use super::Registry;
use super::endpoint::{Connector, ORIGIN, TestBot};
use crate::chatsvc::engine_with_session;
use crate::support::{Backend, backends};
use std::sync::Arc;
use teams_auth::secrets::MemorySecretStore;
use teams_auth::session::Session;

pub const BOT_NAME: &str = "Echo (synthetic)";

/// A server with an administrator, the account Ada with her engine, and a registered bot at a test endpoint.
pub struct Setup {
    pub registry: Registry,
    pub ada: String,
    pub engine: EngineHandle,
    /// The session whose tokens Ada's engine sends.
    pub session: Arc<Session<MemorySecretStore>>,
    pub bot: TestBot,
    pub bot_id: String,
    pub secret: String,
    _dir: TempDir,
}

/// The server's settings for a bot on loopback over plain HTTP.
pub fn loopback(config: &mut ngteams_server::config::Config) {
    config.public_origin = url::Url::parse(ORIGIN).unwrap();
    config.bots.allow_http_endpoints = true;
    config.bots.allowed_networks = vec!["127.0.0.0/8".parse().unwrap()];
}

impl Setup {
    pub async fn open(backend: Backend) -> Self {
        Self::open_with(backend, |_| {}).await
    }

    pub async fn open_with(backend: Backend, configure: impl FnOnce(&mut ngteams_server::config::Config)) -> Self {
        Self::open_changed(backend, configure, |_| {}).await
    }

    pub async fn open_changed(
        backend: Backend,
        configure: impl FnOnce(&mut ngteams_server::config::Config),
        change: impl FnOnce(&mut ngteams_server::app::App),
    ) -> Self {
        let server = crate::support::TestServer::start_changed(
            backend,
            |config| {
                config.setup.require_code = false;
                loopback(config);
                configure(config);
            },
            change,
        )
        .await;
        let registry = Registry::over(server).await;
        let ada = registry.server.add_user("ada@own.test", "Ada Lovelace").await;
        let dir = tempfile::tempdir().unwrap();
        let (engine, session) = engine_with_session(&registry.server, "ada@own.test", &ada, dir.path()).await;
        let bot = TestBot::start().await;
        let answer = registry.register(BOT_NAME, &bot.url).await;
        let bot_id = answer["id"].as_str().unwrap().to_owned();
        let secret = answer["secret"].as_str().unwrap().to_owned();
        Self {
            registry,
            ada,
            engine,
            session,
            bot,
            bot_id,
            secret,
            _dir: dir,
        }
    }

    /// The 1:1 chat of Ada and the bot, as Teams names it.
    pub fn chat(&self) -> String {
        personal_chat(&self.ada, &self.bot_id)
    }

    /// Ada's message to `conversation`; its id.
    pub async fn send(&self, conversation: &str, text: &str) -> String {
        let row = self
            .engine
            .write(Action::Send {
                conversation: conversation.to_owned(),
                text: MessageText::plain(text).unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        row.message_id.unwrap()
    }

    /// The bot's connector, signed with its own token.
    pub async fn connector(&self, service_url: &str) -> Connector {
        Connector::signed(&self.registry.server, service_url, &self.bot_id, &self.secret).await
    }

    /// Ada's messages of the chat after a sync, oldest first.
    pub async fn synced(&self, conversation: &str) -> Vec<Message> {
        let report = self.engine.sync().await.unwrap();
        assert!(report.failed.is_empty(), "{report:?}");
        let mut page = self.engine.messages(conversation, None, 100).await.unwrap().messages;
        page.reverse();
        page
    }

    pub async fn stop(self) {
        self.engine.shutdown();
        self.registry.server.stop().await;
    }
}

pub fn personal_chat(user: &str, bot: &str) -> String {
    let mut ids = [user, bot];
    ids.sort_unstable();
    format!("19:{}_{}@unq.gbl.spaces", ids[0], ids[1])
}

/// The card a `RichText/Media_Card` message carries, from its `Swift`.
fn swift_card(content: &str) -> Value {
    let start = content.find("b64=\"").unwrap() + 5;
    let end = start + content[start..].find('"').unwrap();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&content[start..end])
        .unwrap();
    serde_json::from_slice(&decoded).unwrap()
}

/// Whether any string in `value` is a JWT.
fn carries_token(value: &Value) -> bool {
    match value {
        Value::String(text) => text.split(['.', ' ', '"']).any(|part| part.starts_with("eyJ")),
        Value::Array(items) => items.iter().any(carries_token),
        Value::Object(map) => map.values().any(carries_token),
        _ => false,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_user_writes_to_a_bot_and_syncs_its_text_markdown_and_card_replies() {
    for backend in backends() {
        let setup = Setup::open(backend).await;
        let chat = setup.chat();
        let sent = setup.send(&chat, "hello bot").await;

        let got = setup.bot.next_of("message").await;
        let activity = &got.activity;
        assert_eq!(activity["text"], "hello bot", "{activity}");
        assert_eq!(activity["id"], sent);
        assert_eq!(activity["channelId"], "msteams");
        assert_eq!(activity["serviceUrl"], format!("{ORIGIN}/bot/"));
        assert_eq!(activity["from"]["id"], format!("29:{}", setup.ada));
        assert_eq!(activity["from"]["aadObjectId"], setup.ada);
        assert_eq!(activity["from"]["name"], "Ada Lovelace");
        assert_eq!(activity["recipient"]["id"], format!("28:{}", setup.bot_id));
        assert_eq!(activity["recipient"]["name"], BOT_NAME);
        assert_eq!(activity["conversation"]["id"], chat);
        assert_eq!(activity["conversation"]["conversationType"], "personal");
        assert_eq!(activity["conversation"]["isGroup"], false);
        let tenant = ngteams_server::db::meta::tenant(&setup.registry.server.app.db)
            .await
            .unwrap();
        assert_eq!(activity["conversation"]["tenantId"], tenant);
        assert_eq!(activity["channelData"]["tenant"]["id"], tenant);
        assert!(!carries_token(activity), "{activity}");
        let claims = got.token_claims().unwrap();
        assert_eq!(claims["aud"], setup.bot_id);
        assert_eq!(claims["serviceurl"], activity["serviceUrl"]);
        assert_eq!(claims["iss"], format!("{ORIGIN}/bot"));

        let connector = setup.connector(activity["serviceUrl"].as_str().unwrap()).await;
        let (status, answer) = connector.reply(&chat, &sent, Connector::text("pong")).await;
        assert_eq!(status, 200, "{answer}");
        let text_id = answer["id"].as_str().unwrap().to_owned();
        let markdown = json!({"type": "message", "textFormat": "markdown",
            "text": "**bold** and [a link](https://docs.example.test/a)\n\n<script>alert(1)</script>"});
        let (status, _) = connector.send(&chat, markdown).await;
        assert_eq!(status, 200);
        let card = json!({"type": "AdaptiveCard", "version": "1.5",
            "body": [{"type": "TextBlock", "text": "Synthetic card"}],
            "actions": [{"type": "Action.OpenUrl", "title": "Open", "url": "https://docs.example.test/card"}]});
        let with_card = json!({"type": "message", "attachments": [
            {"contentType": "application/vnd.microsoft.card.adaptive", "content": card}]});
        let (status, _) = connector.send(&chat, with_card).await;
        assert_eq!(status, 200);

        let messages = setup.synced(&chat).await;
        let from_bot: Vec<&Message> = messages
            .iter()
            .filter(|message| message.sender.as_deref() == Some(&format!("28:{}", setup.bot_id)))
            .collect();
        assert_eq!(from_bot.len(), 3, "{messages:#?}");
        assert_eq!(from_bot[0].id, text_id);
        assert!(from_bot[0].content.contains("pong"));
        assert_eq!(from_bot[0].sender_name.as_deref(), Some(BOT_NAME));
        assert!(
            from_bot[1].content.contains("<strong>bold</strong>"),
            "{}",
            from_bot[1].content
        );
        assert!(from_bot[1].content.contains("href=\"https://docs.example.test/a\""));
        assert!(!from_bot[1].content.contains("<script"));
        assert_eq!(from_bot[2].message_type, "RichText/Media_Card");
        let carried = swift_card(&from_bot[2].content);
        assert_eq!(carried["attachments"][0]["content"], card);
        let parsed = teams_proto::chat::card::card_message(&from_bot[2].content).unwrap();
        assert!(parsed.drawn(), "{parsed:?}");
        assert!(format!("{parsed:?}").contains("OpenUrl(\"https://docs.example.test/card\")"));
        setup.stop().await;
    }
}

/// QA 229 Q2: the bot's answers carry a `clientmessageid`, as a Microsoft bot's do, so the account's read receipt
/// names the newest of them and the chat is no longer unread.
#[tokio::test(flavor = "multi_thread")]
async fn a_user_reads_the_bots_answers_and_the_chat_is_read() {
    for backend in backends() {
        let setup = Setup::open(backend).await;
        let chat = setup.chat();
        let sent = setup.send(&chat, "anything new?").await;
        let got = setup.bot.next_of("message").await;
        let connector = setup.connector(got.activity["serviceUrl"].as_str().unwrap()).await;
        for text in ["one", "two"] {
            let (status, answer) = connector.reply(&chat, &sent, Connector::text(text)).await;
            assert_eq!(status, 200, "{answer}");
        }

        let messages = setup.synced(&chat).await;
        let newest = messages.last().unwrap();
        assert_eq!(newest.sender.as_deref(), Some(format!("28:{}", setup.bot_id).as_str()));
        let ids: Vec<&str> = messages
            .iter()
            .filter(|message| message.sender == newest.sender)
            .map(|message| message.client_message_id.as_deref().unwrap_or_default())
            .collect();
        assert!(
            ids.iter()
                .all(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())),
            "{ids:?}"
        );
        assert_ne!(ids[0], ids[1]);
        assert_eq!(setup.engine.unread_count(&chat).await.unwrap(), 2);

        let row = setup
            .engine
            .write(Action::Read {
                conversation: chat.clone(),
                message_id: newest.id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        setup.synced(&chat).await;
        assert_eq!(setup.engine.unread_count(&chat).await.unwrap(), 0);
        let horizon = setup.engine.read_horizon(&chat).await.unwrap().unwrap();
        assert_eq!(horizon.last_read_arrival.to_string(), newest.id);
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_deletes_and_typing_go_both_ways() {
    for backend in backends() {
        let setup = Setup::open(backend).await;
        let chat = setup.chat();
        let sent = setup.send(&chat, "first words").await;
        let first = setup.bot.next_of("message").await;

        let row = setup
            .engine
            .write(Action::Edit {
                conversation: chat.clone(),
                message_id: sent.clone(),
                text: MessageText::plain("better words").unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        let edited = setup.bot.next_of("messageUpdate").await;
        assert_eq!(edited.activity["id"], sent);
        assert_eq!(edited.activity["text"], "better words");
        assert_eq!(edited.activity["channelData"]["eventType"], "editMessage");

        let row = setup
            .engine
            .write(Action::Delete {
                conversation: chat.clone(),
                message_id: sent.clone(),
            })
            .await
            .unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        let deleted = setup.bot.next_of("messageDelete").await;
        assert_eq!(deleted.activity["id"], sent);
        assert_eq!(deleted.activity["channelData"]["eventType"], "softDeleteMessage");

        let typed = setup
            .engine
            .transient(Transient::Typing {
                conversation: chat.clone(),
            })
            .await
            .unwrap();
        assert!(matches!(typed, TransientSent::Sent), "{typed:?}");
        let typing = setup.bot.next_of("typing").await;
        assert_eq!(typing.activity["from"]["id"], format!("29:{}", setup.ada));

        bot_side(&setup, &chat, &sent, first.activity["serviceUrl"].as_str().unwrap()).await;
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_writes_first_to_a_user_who_may_use_it_and_reads_the_roster() {
    for backend in backends() {
        let setup = Setup::open(backend).await;
        let server = &setup.registry.server;
        let service_url = format!("{ORIGIN}/bot/");
        let connector = setup.connector(&service_url).await;
        let tenant = ngteams_server::db::meta::tenant(&server.app.db).await.unwrap();
        let create = json!({
            "bot": {"id": format!("28:{}", setup.bot_id)},
            "members": [{"id": format!("29:{}", setup.ada)}],
            "channelData": {"tenant": {"id": tenant}},
            "isGroup": false,
            "activity": {"type": "message", "text": "A proactive hello"},
        });
        let (status, answer) = connector.call(Method::POST, "v3/conversations", Some(create)).await;
        assert_eq!(status, 201, "{answer}");
        let chat = setup.chat();
        assert_eq!(answer["id"], chat);
        assert!(answer["activityId"].as_str().is_some());
        let messages = setup.synced(&chat).await;
        assert!(
            messages
                .iter()
                .any(|message| message.content.contains("A proactive hello")),
            "{messages:#?}"
        );
        // a second create finds the same chat
        let again = json!({"bot": {"id": format!("28:{}", setup.bot_id)},
            "members": [{"id": format!("8:orgid:{}", setup.ada)}], "channelData": {"tenant": {"id": tenant}}});
        let (status, answer) = connector.call(Method::POST, "v3/conversations", Some(again)).await;
        assert!(status == 200 || status == 201, "{answer}");
        assert_eq!(answer["id"], chat);

        let path = format!("v3/conversations/{}/members", super::endpoint::encode(&chat));
        let (status, members) = connector.call(Method::GET, &path, None).await;
        assert_eq!(status, 200, "{members}");
        let members = members.as_array().unwrap();
        assert_eq!(members.len(), 1, "{members:?}");
        assert_eq!(members[0]["id"], format!("29:{}", setup.ada));
        assert_eq!(members[0]["aadObjectId"], setup.ada);
        assert_eq!(members[0]["name"], "Ada Lovelace");
        assert_eq!(members[0]["userPrincipalName"], "ada@own.test");
        assert_eq!(members[0]["tenantId"], tenant);
        let path = format!(
            "v3/conversations/{}/members/29:{}",
            super::endpoint::encode(&chat),
            setup.ada
        );
        let (status, one) = connector.call(Method::GET, &path, None).await;
        assert_eq!(status, 200, "{one}");
        assert_eq!(one["email"], "ada@own.test");
        let path = format!("v3/conversations/{}/pagedmembers", super::endpoint::encode(&chat));
        let (status, paged) = connector.call(Method::GET, &path, None).await;
        assert_eq!(status, 200, "{paged}");
        assert_eq!(paged["members"].as_array().unwrap().len(), 1);

        refusals(&setup, &connector, &tenant, &chat).await;
        setup.stop().await;
    }
}

/// The bot's side: typing, then its own message edited and deleted; another's it can't change.
async fn bot_side(setup: &Setup, chat: &str, sent: &str, service_url: &str) {
    let connector = setup.connector(service_url).await;
    bot_types(setup, &connector, chat).await;
    bot_changes_its_own(setup, &connector, chat, sent).await;
}

async fn bot_types(setup: &Setup, connector: &Connector, chat: &str) {
    let mut pushed = setup
        .registry
        .server
        .app
        .fanout
        .subscribe(&format!("push:8:orgid:{}", setup.ada))
        .await
        .unwrap();
    let (status, answer) = connector.send(chat, json!({"type": "typing"})).await;
    assert_eq!(status, 200, "{answer}");
    let event = tokio::time::timeout(Duration::from_secs(5), pushed.recv())
        .await
        .unwrap()
        .unwrap();
    let event: Value = serde_json::from_slice(&event).unwrap();
    assert_eq!(event["resource"]["messagetype"], "Control/Typing");
    assert!(
        event["resource"]["from"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/28:{}", setup.bot_id))
    );
}

async fn bot_changes_its_own(setup: &Setup, connector: &Connector, chat: &str, sent: &str) {
    let (_, answer) = connector.send(chat, Connector::text("draft answer")).await;
    let id = answer["id"].as_str().unwrap().to_owned();
    let (status, answer) = connector
        .update(chat, &id, json!({"type": "message", "id": id, "text": "final answer"}))
        .await;
    assert_eq!(status, 200, "{answer}");
    let messages = setup.synced(chat).await;
    let mine = messages.iter().find(|message| message.id == id).unwrap();
    assert!(mine.content.contains("final answer"), "{mine:?}");
    assert!(mine.edit_time.is_some());
    // a bot changes only its own messages
    let (status, _) = connector.update(chat, sent, Connector::text("not mine")).await;
    assert_eq!(status, 403);
    let (status, _) = connector.delete(chat, &id).await;
    assert_eq!(status, 200);
    let messages = setup.synced(chat).await;
    let mine = messages.iter().find(|message| message.id == id).unwrap();
    assert!(mine.delete_time.is_some(), "{mine:?}");
}

/// A person who may not use the app gets no chat, a chat the bot isn't in is closed to it, and no token or a
/// person's opens nothing.
async fn refusals(setup: &Setup, connector: &Connector, tenant: &str, chat: &str) {
    let cora = setup.registry.server.add_user("cora@own.test", "Cora Synthetic").await;
    let (_, record) = setup
        .registry
        .call(
            Method::PATCH,
            &super::app_path(&setup.bot_id),
            Some(json!({"everyone": false, "users": [setup.ada]})),
        )
        .await;
    assert_eq!(record["everyone"], false, "{record}");
    let to_cora = json!({"bot": {"id": format!("28:{}", setup.bot_id)},
        "members": [{"id": format!("29:{cora}")}], "channelData": {"tenant": {"id": tenant}}});
    let (status, _) = connector.call(Method::POST, "v3/conversations", Some(to_cora)).await;
    assert_eq!(status, 403);
    let theirs = personal_chat(&setup.ada, &cora);
    let (status, _) = connector.send(&theirs, Connector::text("intrusion")).await;
    assert!(status == 403 || status == 404, "{status}");
    // no token, or a person's token, opens nothing
    let anonymous = Connector {
        base: connector.base.clone(),
        token: None,
    };
    let (status, _) = anonymous.send(chat, Connector::text("anonymous")).await;
    assert_eq!(status, 401);
    let person = Connector {
        base: connector.base.clone(),
        token: Some(setup.registry.token().await),
    };
    let (status, _) = person.send(chat, Connector::text("impostor")).await;
    assert_eq!(status, 401);
}
