//! Delivery: a channel post or reply that @mentions a webhook reaches its service once, signed with HMAC-SHA256 of
//! the exact body, and the answer (text or a card) becomes the webhook's reply in the post's thread, which the
//! team's engines sync. A failure or a timeout becomes Teams' apology in the thread.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use teams_proto::chat::MessageText;
use teams_proto::reply_chain_id;
use teams_store::Message;

use super::receiver::{Receiver, Reply};
use super::{Crew, key_of, loopback};
use crate::chatsvc::activity::{RawPost, send};
use crate::support::{TestServer, backends, redis_url};
use crate::teams::Seat;

const HOOK: &str = "Echo hook (synthetic)";
/// What Teams posts as the webhook when its service fails or doesn't answer in time.
const APOLOGY: &str = "Sorry, there was a problem encountered with your request";

/// A crew with a webhook at a receiver that trusts its token.
async fn hooked(crew: &Crew) -> (Receiver, String) {
    let receiver = Receiver::start().await;
    let made = crew.create(HOOK, &receiver.url).await;
    receiver.trust(key_of(made["securityToken"].as_str().unwrap()));
    (receiver, made["id"].as_str().unwrap().to_owned())
}

/// `seat`'s post in `conversation` that mentions the webhook `hook` (`name`), as Teams' composer sends it, through
/// the account's own client: the native composer doesn't mention apps yet (client task 412). Its id.
async fn mention(seat: &Seat, conversation: &str, hook: &str, name: &str, words: &str) -> String {
    let post = RawPost {
        chat: conversation.into(),
        body: mention_body(hook, name, words),
    };
    let answer = seat.client.write(post).await.unwrap();
    assert_eq!(answer.status(), 201);
    let body: Value = serde_json::from_slice(answer.body()).unwrap();
    body["OriginalArrivalTime"].to_string().trim_matches('"').to_owned()
}

fn mention_body(hook: &str, name: &str, words: &str) -> Value {
    let mentions = json!([{"@type": "http://schema.skype.com/Mention", "itemid": "0", "mri": format!("28:{hook}"),
        "mentionType": "person", "displayName": name}]);
    json!({
        "messagetype": "RichText/Html", "contenttype": "Text",
        "clientmessageid": uuid::Uuid::new_v4().as_u128().to_string(),
        "content": format!("<p><span itemtype=\"http://schema.skype.com/Mention\" itemscope itemid=\"0\">{name}</span> {words}</p>"),
        "properties": {"mentions": mentions.to_string()},
    })
}

struct RawEdit {
    post: RawPost,
    id: String,
}

impl teams_proto::Request for RawEdit {
    fn service(&self) -> teams_proto::Service {
        teams_proto::Service::ChatService
    }
    fn method(&self) -> teams_proto::Method {
        teams_proto::Method::Put
    }
    fn endpoint(&self) -> teams_proto::Endpoint {
        teams_proto::Endpoint::new([
            "v1",
            "users",
            "ME",
            "conversations",
            &self.post.chat,
            "messages",
            &self.id,
        ])
    }
    fn body(&self) -> Option<Value> {
        Some(self.post.body.clone())
    }
    fn is_write(&self) -> bool {
        true
    }
}

/// `seat`'s synced messages of `channel` in the thread of `post`, oldest first, but the post.
async fn thread(seat: &Seat, channel: &str, post: &str) -> Vec<Message> {
    seat.engine.sync_conversation(channel).await.unwrap();
    let roots = seat.engine.reply_roots(channel).await.unwrap();
    let mut replies = Vec::new();
    for (id, root) in roots {
        if root == post && id != post {
            replies.push(seat.engine.message(channel, &id).await.unwrap().unwrap());
        }
    }
    replies.sort_by_key(|message| (message.arrival_time, message.id.clone()));
    replies
}

/// The replies from `hook` in the thread of `post`, waited for up to 10 s.
pub(super) async fn replies_from(seat: &Seat, channel: &str, post: &str, hook: &str, count: usize) -> Vec<Message> {
    let mri = format!("28:{hook}");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let from: Vec<Message> = thread(seat, channel, post)
            .await
            .into_iter()
            .filter(|message| message.sender.as_deref() == Some(mri.as_str()))
            .collect();
        if from.len() >= count || Instant::now() > deadline {
            return from;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The card a `RichText/Media_Card` message carries, from its `Swift`.
fn swift_card(content: &str) -> Value {
    use base64::Engine as _;
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

async fn ignored_posts(crew: &Crew, hook: &str, name: &str, post: &str) {
    send(
        &crew.ada.engine,
        &crew.channel,
        MessageText::plain("no mention").unwrap(),
    )
    .await;
    let edited = crew
        .ada
        .client
        .write(RawEdit {
            post: RawPost {
                chat: crew.channel.clone(),
                body: mention_body(hook, name, "edited"),
            },
            id: post.to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(edited.status(), 200);
    let draft = teams_core::ChannelDraft {
        channel: crate::teams::new_channel(&crew.team, &crew.group, "Secret", teams_proto::NewChannelKind::Private),
        show_by_default: false,
    };
    let made = crew.ada.engine.create_channel(draft).await.unwrap();
    let private = crew
        .ada
        .engine
        .channel_thread(
            crew.team.as_str(),
            crew.group.as_str(),
            made.group.unwrap().as_str(),
            "Secret",
        )
        .await
        .unwrap()
        .unwrap();
    mention(&crew.ada, &private, hook, name, "private").await;
    let (other, other_group) = crew.ada.make_team("Other (synthetic)", false).await;
    crew.ada.add(&other, &other_group, &[&crew.bert]).await;
    mention(&crew.ada, &other, hook, name, "wrong team").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mention_in_a_channel_reaches_the_webhook_signed_and_its_answer_is_the_reply_in_the_thread() {
    for backend in backends() {
        let crew = Crew::open(backend).await;
        let (receiver, hook) = hooked(&crew).await;
        let renamed = "Renamed hook (synthetic)";
        let path = format!("{}/{hook}", super::hooks_path(&crew.team));
        let (status, _) = crew
            .mt(
                "ada@own.test",
                reqwest::Method::PUT,
                &path,
                Some(json!({"name": renamed})),
            )
            .await;
        assert_eq!(status, 200);
        receiver.answer(Reply::json(&json!({"type": "message", "text": "**Pong** for you"})));
        let post = mention(&crew.ada, &crew.channel, &hook, renamed, "status please").await;

        let got = receiver.next().await;
        assert!(got.signed, "{:?}", got.authorization);
        let activity = &got.activity;
        assert_eq!(activity["type"], "message");
        assert_eq!(activity["id"], post.as_str());
        assert_eq!(activity["channelId"], "msteams");
        assert_eq!(activity["text"], format!("<at>{renamed}</at> status please"));
        assert_eq!(activity["textFormat"], "plain");
        assert_eq!(activity["from"]["id"], format!("29:{}", crew.ada.id));
        assert_eq!(activity["from"]["aadObjectId"], crew.ada.id.as_str());
        assert_eq!(activity["from"]["name"], "Ada Lovelace");
        let chain = format!("{};messageid={post}", crew.channel);
        assert_eq!(activity["conversation"]["id"], chain.as_str());
        assert_eq!(activity["conversation"]["conversationType"], "channel");
        assert_eq!(activity["conversation"]["isGroup"], true);
        assert_eq!(activity["channelData"]["teamsChannelId"], crew.channel.as_str());
        assert_eq!(activity["channelData"]["teamsTeamId"], crew.team.as_str());
        assert_eq!(activity["channelData"]["channel"]["id"], crew.channel.as_str());
        assert_eq!(activity["channelData"]["team"]["id"], crew.team.as_str());
        assert!(activity["channelData"]["tenant"]["id"].is_string());
        assert_eq!(activity["attachments"][0]["contentType"], "text/html");
        assert!(activity["recipient"].is_null());
        assert!(!carries_token(activity), "no token goes to a webhook");
        assert!(got.authorization.as_deref().unwrap().starts_with("HMAC "));
        // another key would sign the same body otherwise
        assert_ne!(
            super::receiver::hmac(&[7; 32], &got.body),
            got.authorization.clone().unwrap()
        );

        // the answer is the webhook's reply under the post, as the team's people sync it
        let replies = replies_from(&crew.bert, &crew.channel, &post, &hook, 1).await;
        assert_eq!(replies.len(), 1, "{replies:?}");
        assert_eq!(replies[0].sender_name.as_deref(), Some(renamed));
        assert!(
            replies[0].content.contains("<strong>Pong</strong> for you"),
            "{}",
            replies[0].content
        );

        // a reply in the thread that mentions it: the activity names the same thread, the answer goes under it
        receiver.answer(Reply::json(
            &json!({"type": "message", "text": "second answer", "textFormat": "plain"}),
        ));
        let chain_id = reply_chain_id(&crew.channel, &post).unwrap();
        let reply = mention(&crew.bert, &chain_id, &hook, renamed, "again").await;
        let got = receiver.next().await;
        assert!(got.signed);
        assert_eq!(got.activity["id"], reply.as_str());
        assert_eq!(got.activity["conversation"]["id"], chain.as_str());
        assert_eq!(got.activity["from"]["name"], "Bert Sample");
        let replies = replies_from(&crew.ada, &crew.channel, &post, &hook, 2).await;
        assert_eq!(replies.len(), 2, "{replies:?}");
        assert!(replies[1].content.contains("second answer"));

        ignored_posts(&crew, &hook, renamed, &post).await;
        assert!(!receiver.gets_within(Duration::from_secs(3)).await);
        crew.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_card_answer_is_the_webhooks_card_reply_with_only_its_open_url_actions() {
    for backend in backends() {
        let crew = Crew::open(backend).await;
        let (receiver, hook) = hooked(&crew).await;
        let card = json!({"type": "AdaptiveCard", "version": "1.4", "body": [
            {"type": "TextBlock", "text": "Build 42 passed"},
            {"type": "ActionSet", "actions": [
                {"type": "Action.Submit", "title": "Rerun", "data": {"x": 1}},
                {"type": "Action.OpenUrl", "title": "Logs", "url": "https://ci.example.test/42"}]}],
            "actions": [{"type": "Action.OpenUrl", "title": "Open", "url": "https://ci.example.test/"},
                {"type": "Action.Execute", "title": "Run", "verb": "run"}]});
        receiver.answer(Reply::json(&json!({"type": "message", "attachments": [
            {"contentType": "application/vnd.microsoft.card.adaptive", "contentUrl": null, "content": card}]})));
        let post = mention(&crew.ada, &crew.channel, &hook, HOOK, "build?").await;
        assert!(receiver.next().await.signed);
        let replies = replies_from(&crew.bert, &crew.channel, &post, &hook, 1).await;
        assert_eq!(replies.len(), 1, "{replies:?}");
        assert_eq!(replies[0].message_type, "RichText/Media_Card");
        let carried = swift_card(&replies[0].content);
        let content = &carried["attachments"][0]["content"];
        assert_eq!(content["body"][0]["text"], "Build 42 passed");
        assert_eq!(
            content["body"][1]["actions"],
            json!([{"type": "Action.OpenUrl", "title": "Logs", "url": "https://ci.example.test/42"}])
        );
        assert_eq!(
            content["actions"],
            json!([{"type": "Action.OpenUrl", "title": "Open", "url": "https://ci.example.test/"}])
        );
        crew.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failure_or_a_timeout_shows_as_teams_apology_and_an_empty_answer_shows_nothing() {
    for backend in backends() {
        let crew = Crew::open(backend).await;
        let (receiver, hook) = hooked(&crew).await;
        for (case, reply, apology) in failure_cases() {
            receiver.answer(reply);
            let post = mention(&crew.ada, &crew.channel, &hook, HOOK, case).await;
            let got = receiver.next().await;
            assert!(got.signed, "{case}");
            let started = got.at;
            let replies = replies_from(&crew.bert, &crew.channel, &post, &hook, 1).await;
            if apology {
                assert_eq!(replies.len(), 1, "{case}: {replies:?}");
                assert!(replies[0].content.contains(APOLOGY), "{case}: {}", replies[0].content);
                assert_eq!(replies[0].sender_name.as_deref(), Some(HOOK));
            } else {
                assert!(replies.is_empty(), "{case}: {replies:?}");
            }
            if case == "late" {
                let posted_after = replies[0].arrival_time - got.timestamp;
                eprintln!(
                    "QA existing timeout {backend:?}: apology posted after {posted_after} ms, client sync after {} ms",
                    started.elapsed().as_millis()
                );
                assert!(posted_after < 7000, "the apology comes at about 5 s");
            }
        }
        // the late answer never becomes a reply
        tokio::time::sleep(Duration::from_secs(2)).await;
        crew.ada.engine.sync_conversation(crew.channel.as_str()).await.unwrap();
        let page = crew
            .ada
            .engine
            .messages(&crew.channel, None, 200)
            .await
            .unwrap()
            .messages;
        assert!(page.iter().all(|message| !message.content.contains("too late")));
        crew.stop().await;
    }
}

fn failure_cases() -> [(&'static str, Reply, bool); 5] {
    [
        (
            "refused",
            Reply {
                status: 500,
                body: "{}".into(),
                delay: Duration::ZERO,
            },
            true,
        ),
        (
            "garbled",
            Reply {
                status: 200,
                body: "not json".into(),
                delay: Duration::ZERO,
            },
            true,
        ),
        (
            "unknown attachment",
            Reply::json(&json!({"type": "message", "attachments": [
            {"contentType": "image/png", "contentUrl": "https://x.example.test/a.png"}]})),
            true,
        ),
        (
            "late",
            Reply {
                status: 200,
                body: json!({"text": "too late"}).to_string(),
                delay: Duration::from_secs(7),
            },
            true,
        ),
        (
            "empty",
            Reply {
                status: 200,
                body: String::new(),
                delay: Duration::ZERO,
            },
            false,
        ),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn a_private_or_loopback_address_is_refused_unless_the_server_allows_its_network() {
    for backend in backends() {
        let crew = Crew::open_with(backend, |config| config.bots.allowed_networks.clear()).await;
        let receiver = Receiver::start().await;
        let path = super::hooks_path(&crew.team);
        let literal = json!({"name": "Literal (synthetic)", "callbackUrl": receiver.url});
        let (status, answer) = crew
            .mt("ada@own.test", reqwest::Method::POST, &path, Some(literal))
            .await;
        assert_eq!(status, 400, "{answer}");
        // a name that resolves to loopback is taken, and refused when the server would send
        let by_name = receiver.url.replace("127.0.0.1", "localhost");
        let made = crew.create("By name (synthetic)", &by_name).await;
        let hook = made["id"].as_str().unwrap().to_owned();
        let post = mention(&crew.ada, &crew.channel, &hook, "By name (synthetic)", "hi").await;
        let replies = replies_from(&crew.bert, &crew.channel, &post, &hook, 1).await;
        assert_eq!(replies.len(), 1, "{replies:?}");
        assert!(replies[0].content.contains(APOLOGY));
        assert!(!receiver.gets_within(Duration::from_millis(500)).await);
        crew.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_nodes_send_each_mention_once() {
    for backend in backends() {
        let crew = Crew::open(backend).await;
        let url = crew.registry.server.app.config.database_url.clone();
        let second = TestServer::start_with(backend, |config| {
            loopback(config);
            config.database_url = url;
            config.redis_url = redis_url();
        })
        .await;
        let (receiver, hook) = hooked(&crew).await;
        receiver.answer(Reply::json(&json!({"type": "message", "text": "once"})));
        let mut posts = Vec::new();
        for n in 0..6 {
            posts.push(mention(&crew.ada, &crew.channel, &hook, HOOK, &format!("call {n}")).await);
        }
        let mut seen = Vec::new();
        for _ in 0..6 {
            seen.push(receiver.next().await.activity["id"].as_str().unwrap().to_owned());
        }
        seen.sort();
        let mut expected = posts.clone();
        expected.sort();
        assert_eq!(seen, expected);
        assert!(!receiver.gets_within(Duration::from_secs(3)).await);
        for post in &posts {
            let replies = replies_from(&crew.bert, &crew.channel, post, &hook, 1).await;
            assert_eq!(replies.len(), 1, "{post}: {replies:?}");
        }
        second.stop().await;
        crew.stop().await;
    }
}

async fn refuse_card_inserts(crew: &Crew, backend: crate::support::Backend) {
    use diesel_async::SimpleAsyncConnection as _;
    let sql = match backend {
        crate::support::Backend::Sqlite => {
            "CREATE TRIGGER refuse_card BEFORE INSERT ON chat_messages
            WHEN NEW.body LIKE '%RichText/Media_Card%'
            BEGIN SELECT RAISE(ABORT, 'synthetic card failure'); END;"
        }
        crate::support::Backend::Postgres => {
            "CREATE FUNCTION refuse_card() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN IF NEW.body LIKE '%RichText/Media_Card%' THEN RAISE EXCEPTION 'synthetic card failure'; END IF;
            RETURN NEW; END $$;
            CREATE TRIGGER refuse_card BEFORE INSERT ON chat_messages FOR EACH ROW EXECUTE FUNCTION refuse_card();"
        }
    };
    let result: Result<(), ngteams_server::db::DbError> = async {
        ngteams_server::with_conn!(&crew.registry.server.app.db, |conn| conn.batch_execute(sql).await?);
        Ok(())
    }
    .await;
    result.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_insert_failure_rolls_back_all_replies_and_the_sweep_posts_one_apology() {
    for backend in backends() {
        let crew = Crew::open(backend).await;
        let (receiver, hook) = hooked(&crew).await;
        refuse_card_inserts(&crew, backend).await;
        receiver.answer(Reply::json(&json!({"type": "message", "text": "Text before card",
            "attachments": [{"contentType": "application/vnd.microsoft.card.hero", "content": {"title": "Card"}}]})));
        let post = mention(&crew.ada, &crew.channel, &hook, HOOK, "reply?").await;
        assert!(receiver.next().await.signed);
        let mut replies = replies_from(&crew.bert, &crew.channel, &post, &hook, 1).await;
        if replies.is_empty() {
            replies = replies_from(&crew.bert, &crew.channel, &post, &hook, 1).await;
        }
        assert_eq!(replies.len(), 1, "{replies:?}");
        assert!(replies[0].content.contains(APOLOGY), "{}", replies[0].content);
        assert!(!receiver.gets_within(Duration::from_secs(2)).await);
        let replies = thread(&crew.bert, &crew.channel, &post).await;
        assert_eq!(replies.len(), 1);
        crew.stop().await;
    }
}
