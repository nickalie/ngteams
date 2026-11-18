//! Bot invokes (`docs/protocol/bots.md`): the engine of an own-server account asks a bot through its chat service
//! (`v1/agents/28:<bot>/invoke`), the server posts the invoke activity to the bot at once and hands its answer back.
//! The bot is the loopback test bot; apps, bots, cards and messages are synthetic.

use std::time::{Duration, Instant};

use reqwest::Method;
use serde_json::{Map, Value, json};
use teams_core::bots::BotAsk;
use teams_proto::Mri;
use teams_proto::bot::{CardInvoke, CommandContext, InvokeAnswer, InvokeKind, MessagePayload, PayloadSender};

use super::connector::{Setup, personal_chat};
use super::endpoint::{Connector, ORIGIN, Received, Scripted, TestBot};
use crate::support::backends;

/// An Adaptive Card with an input, a plain submit, an `Action.Execute` and a dialog button.
pub fn poll_card(title: &str) -> Value {
    json!({"type": "AdaptiveCard", "version": "1.5",
        "body": [{"type": "TextBlock", "text": title},
                 {"type": "Input.Text", "id": "answer", "label": "Your answer"}],
        "actions": [
            {"type": "Action.Submit", "title": "Send", "data": {"poll": 7}},
            {"type": "Action.Execute", "title": "Vote", "verb": "vote", "data": {"poll": 7}},
            {"type": "Action.Submit", "title": "Details", "data": {"msteams": {"type": "task/fetch"}, "poll": 7}}]})
}

fn adaptive(card: &Value) -> Value {
    json!({"contentType": "application/vnd.microsoft.card.adaptive", "content": card})
}

/// What the test bot answers each invoke with, by its name.
fn answers(activity: &Value) -> Scripted {
    let name = activity["name"]
        .as_str()
        .unwrap_or_else(|| activity["type"].as_str().unwrap_or_default());
    let result = json!({"type": "result", "attachmentLayout": "list", "attachments": [{
        "contentType": "application/vnd.microsoft.card.thumbnail", "content": {"title": "Poll 7"},
        "preview": {"contentType": "application/vnd.microsoft.card.thumbnail",
                    "content": {"title": "Poll 7", "tap": {"type": "invoke", "value": {"poll": 7}}}}}]});
    match name {
        "composeExtension/fetchCommands" => Scripted::json(json!({"composeExtension": {"type": "command",
            "commands": [{"id": "newPoll", "type": "action", "title": "New poll", "fetchTask": true}]}})),
        "composeExtension/query" | "composeExtension/selectItem" => {
            Scripted::json(json!({ "composeExtension": result }))
        }
        "composeExtension/fetchTask" | "task/fetch" => Scripted::json(json!({"task": {"type": "continue",
            "value": {"title": "New poll", "width": 600, "height": "medium", "card": adaptive(&poll_card("Ask"))}}})),
        "composeExtension/submitAction" | "message" => Scripted::empty(200),
        "task/submit" => Scripted::json(json!({"task": {"type": "message", "value": "Poll saved"}})),
        "adaptiveCard/action" => match activity["value"]["action"]["verb"].as_str() {
            Some("vote") => Scripted::json(json!({"statusCode": 200,
                "type": "application/vnd.microsoft.card.adaptive", "value": poll_card("Voted")})),
            _ => Scripted::json(json!({"statusCode": 400, "type": "application/vnd.microsoft.error",
                "value": {"code": "BadVerb", "message": "No such verb"}})),
        },
        _ => Scripted::empty(501),
    }
}

impl Setup {
    /// The bot's card in Ada's chat with it, posted through the connector after her first message; its id.
    pub async fn card_in_chat(&self) -> (String, Connector) {
        let chat = self.chat();
        self.send(&chat, "start").await;
        let got = self.bot.next_of("message").await;
        let connector = self.connector(got.activity["serviceUrl"].as_str().unwrap()).await;
        let (status, answer) = connector
            .send(
                &chat,
                json!({"type": "message", "attachments": [adaptive(&poll_card("Lunch?"))]}),
            )
            .await;
        assert_eq!(status, 200, "{answer}");
        (answer["id"].as_str().unwrap().to_owned(), connector)
    }

    /// Ada's invoke of `bot` in `conversation`.
    pub async fn invoke_bot(&self, bot: &str, conversation: Option<&str>, kind: InvokeKind) -> InvokeAnswer {
        self.engine
            .bot_invoke(BotAsk {
                bot: Mri::new(format!("28:{bot}")),
                app_id: bot.to_owned(),
                conversation: conversation.map(str::to_owned),
                kind,
            })
            .await
            .unwrap()
    }

    pub async fn invoke(&self, conversation: Option<&str>, kind: InvokeKind) -> InvokeAnswer {
        self.invoke_bot(&self.bot_id, conversation, kind).await
    }

    /// Ada's raw invoke, as any client could post it; the status and body.
    pub async fn raw_invoke(&self, bot: &str, body: &Value) -> (u16, String) {
        let session = self.registry.server.session("ada@own.test").await;
        let token = teams_trouter::TrouterCredentials::ic3(&session).await.unwrap();
        let response = reqwest::Client::new()
            .post(
                self.registry
                    .server
                    .url(&format!("/api/chatsvc/own/v1/agents/28:{bot}/invoke")),
            )
            .bearer_auth(token.token.expose())
            .json(body)
            .send()
            .await
            .unwrap();
        (response.status().as_u16(), response.text().await.unwrap())
    }
}

fn card_invoke(message: &str, verb: Option<&str>, data: Value) -> CardInvoke {
    CardInvoke {
        message: Some(message.to_owned()),
        id: None,
        verb: verb.map(str::to_owned),
        data,
        automatic: false,
        text: None,
    }
}

/// Asserts each `(pointer, expected)` of `value`.
fn assert_fields(value: &Value, fields: &[(&str, Value)]) {
    for (pointer, expected) in fields {
        assert_eq!(value.pointer(pointer), Some(expected), "{pointer} in {value}");
    }
}

/// Checks what every invoke activity carries: who asks whom, where, and where the bot answers.
fn assert_addressed(got: &Received, setup: &Setup, conversation: &str) {
    let activity = &got.activity;
    assert_fields(
        activity,
        &[
            ("/channelId", json!("msteams")),
            ("/serviceUrl", json!(format!("{ORIGIN}/bot/"))),
            ("/from/id", json!(format!("29:{}", setup.ada))),
            ("/from/aadObjectId", json!(setup.ada)),
            ("/from/name", json!("Ada Lovelace")),
            ("/recipient/id", json!(format!("28:{}", setup.bot_id))),
            ("/conversation/id", json!(conversation)),
            ("/conversation/conversationType", json!("personal")),
            ("/channelData/tenant/id", activity["conversation"]["tenantId"].clone()),
        ],
    );
    let filled = ["/conversation/tenantId", "/id", "/timestamp"].iter().all(|pointer| {
        activity
            .pointer(pointer)
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty())
    });
    assert!(filled, "{activity}");
    assert!(activity.get("userAadToken").is_none(), "{activity}");
    let claims = got.token_claims().unwrap();
    assert_fields(
        &claims,
        &[
            ("/aud", json!(setup.bot_id)),
            ("/serviceurl", activity["serviceUrl"].clone()),
        ],
    );
}

fn carries_token(value: &Value) -> bool {
    match value {
        Value::String(text) => text.split(['.', ' ', '"']).any(|part| part.starts_with("eyJ")),
        Value::Array(items) => items.iter().any(carries_token),
        Value::Object(map) => map.values().any(carries_token),
        _ => false,
    }
}

fn payload(id: &str, setup: &Setup) -> MessagePayload {
    MessagePayload {
        id: id.to_owned(),
        reply_to: None,
        created: "2026-11-17T10:00:00.000Z".to_owned(),
        from: PayloadSender::App {
            id: setup.bot_id.clone(),
            name: "Echo (synthetic)".to_owned(),
        },
        html: "<p>card</p>".to_owned(),
        link: "https://own.example.test/l/message/x".to_owned(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn each_invoke_reaches_the_bot_as_its_activity_and_each_answer_comes_back() {
    for backend in backends() {
        let setup = Setup::open(backend).await;
        let (card, _) = setup.card_in_chat().await;
        setup.bot.script(answers);
        let chat = setup.chat();
        commands_and_search(&setup, &chat).await;
        action_command(&setup, &card, &chat).await;
        execute(&setup, &card, &chat).await;
        card_dialog_and_chosen_result(&setup, &card, &chat).await;
        plain_submit(&setup, &card, &chat).await;
        setup.stop().await;
    }
}

/// The app's commands and a search.
async fn commands_and_search(setup: &Setup, chat: &str) {
    let fetch = InvokeKind::FetchCommands {
        context: CommandContext::Compose,
    };
    let InvokeAnswer::Commands(commands) = setup.invoke(Some(chat), fetch).await else {
        panic!("no commands")
    };
    assert_eq!(commands[0].id, "newPoll");
    let got = setup.bot.next_of("invoke").await;
    assert_addressed(&got, setup, chat);
    assert_fields(
        &got.activity,
        &[
            ("/name", json!("composeExtension/fetchCommands")),
            ("/value", json!({"commandContext": "Compose"})),
        ],
    );
    let query = InvokeKind::Query {
        command: "searchPolls".to_owned(),
        parameters: vec![("q".to_owned(), "lunch".to_owned())],
        skip: 0,
        count: 25,
    };
    let results = setup.invoke(Some(chat), query).await;
    let listed = matches!(&results, InvokeAnswer::Results { grid: false, attachments } if attachments.len() == 1);
    assert!(listed, "{results:?}");
    let got = setup.bot.next_of("invoke").await;
    assert_fields(
        &got.activity,
        &[
            ("/name", json!("composeExtension/query")),
            ("/value/commandId", json!("searchPolls")),
            ("/value/parameters", json!([{"name": "q", "value": "lunch"}])),
            ("/value/queryOptions", json!({"skip": 0, "count": 25})),
        ],
    );
}

/// An action command's dialog on a message, and its submit.
async fn action_command(setup: &Setup, card: &str, chat: &str) {
    let fetch = InvokeKind::FetchTask {
        command: "newPoll".to_owned(),
        context: CommandContext::Message,
        message: Some(payload(card, setup)),
    };
    let InvokeAnswer::Dialog(dialog) = setup.invoke(Some(chat), fetch).await else {
        panic!("no dialog")
    };
    assert_eq!(dialog.title.as_deref(), Some("New poll"));
    assert!(dialog.card.is_some());
    let got = setup.bot.next_of("invoke").await;
    assert_fields(
        &got.activity,
        &[
            ("/name", json!("composeExtension/fetchTask")),
            ("/value/messagePayload/id", json!(card)),
        ],
    );
    let mut data = Map::new();
    data.insert("answer".to_owned(), json!("pizza"));
    let submit = InvokeKind::SubmitAction {
        command: "newPoll".to_owned(),
        context: CommandContext::Compose,
        data,
        message: None,
    };
    assert_eq!(setup.invoke(Some(chat), submit).await, InvokeAnswer::Done);
    let got = setup.bot.next_of("invoke").await;
    assert_fields(
        &got.activity,
        &[
            ("/name", json!("composeExtension/submitAction")),
            ("/value/data/answer", json!("pizza")),
        ],
    );
}

/// A card's `Action.Execute`: the bot's new card, then the bot's error.
async fn execute(setup: &Setup, card: &str, chat: &str) {
    let vote = InvokeKind::Execute(card_invoke(card, Some("vote"), json!({"poll": 7})));
    let InvokeAnswer::Card(voted) = setup.invoke(Some(chat), vote).await else {
        panic!("no card")
    };
    assert_eq!(voted["body"][0]["text"], "Voted");
    let got = setup.bot.next_of("invoke").await;
    assert_fields(
        &got.activity,
        &[
            ("/name", json!("adaptiveCard/action")),
            ("/replyToId", json!(card)),
            ("/value/action/verb", json!("vote")),
            ("/value/action/data", json!({"poll": 7})),
            ("/value/trigger", json!("manual")),
        ],
    );
    let wrong = InvokeKind::Execute(card_invoke(card, Some("nope"), json!({})));
    let error = InvokeAnswer::Error {
        code: Some("BadVerb".to_owned()),
        message: "No such verb".to_owned(),
    };
    assert_eq!(setup.invoke(Some(chat), wrong).await, error);
    setup.bot.next_of("invoke").await;
}

/// A card button's dialog and its submit, and a search result chosen.
async fn card_dialog_and_chosen_result(setup: &Setup, card: &str, chat: &str) {
    let fetch = InvokeKind::TaskFetch(card_invoke(card, None, json!({"poll": 7})));
    let fetched = setup.invoke(Some(chat), fetch).await;
    assert!(matches!(fetched, InvokeAnswer::Dialog(_)), "{fetched:?}");
    let got = setup.bot.next_of("invoke").await;
    assert_fields(
        &got.activity,
        &[("/name", json!("task/fetch")), ("/value/data", json!({"poll": 7}))],
    );
    let submit = InvokeKind::TaskSubmit(card_invoke(card, None, json!({"answer": "soup"})));
    let saved = setup.invoke(Some(chat), submit).await;
    assert_eq!(saved, InvokeAnswer::Message("Poll saved".to_owned()));
    assert_eq!(setup.bot.next_of("invoke").await.activity["name"], "task/submit");
    let select = InvokeKind::SelectItem {
        value: json!({"poll": 7}),
    };
    let picked = setup.invoke(Some(chat), select).await;
    assert!(matches!(picked, InvokeAnswer::Results { .. }), "{picked:?}");
    let got = setup.bot.next_of("invoke").await;
    assert_fields(
        &got.activity,
        &[
            ("/name", json!("composeExtension/selectItem")),
            ("/value", json!({"poll": 7})),
        ],
    );
}

/// A plain Submit: the bot gets a message carrying the card's data and answers.
async fn plain_submit(setup: &Setup, card: &str, chat: &str) {
    let mut submit = card_invoke(card, None, json!({"poll": 7, "answer": "salad"}));
    submit.text = Some("salad please".to_owned());
    let submitted = setup.invoke(Some(chat), InvokeKind::Submit(submit)).await;
    assert_eq!(submitted, InvokeAnswer::Done);
    let got = setup.bot.next_of("message").await;
    assert_addressed(&got, setup, chat);
    assert_fields(
        &got.activity,
        &[
            ("/value", json!({"poll": 7, "answer": "salad"})),
            ("/text", json!("salad please")),
            ("/replyToId", json!(card)),
        ],
    );
    assert!(got.activity.get("name").is_none(), "{}", got.activity);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_invoke_or_a_bot_error_comes_back_as_the_bots_status() {
    for backend in backends() {
        let setup = Setup::open(backend).await;
        let chat = setup.chat();
        setup.send(&chat, "hi").await;
        setup.bot.next_of("message").await;
        setup.bot.script(|_| Scripted::empty(501));
        let refused = setup
            .invoke(
                Some(&chat),
                InvokeKind::FetchCommands {
                    context: CommandContext::Message,
                },
            )
            .await;
        assert_eq!(refused, InvokeAnswer::Refused(501));
        // a bot's 401 is the bot's, never a sign that the account's own token was refused
        setup.bot.script(|_| Scripted::empty(401));
        let refused = setup
            .invoke(
                Some(&chat),
                InvokeKind::FetchCommands {
                    context: CommandContext::Message,
                },
            )
            .await;
        assert_eq!(refused, InvokeAnswer::Refused(502));
        setup.bot.script(|_| Scripted::empty(200));
        let fine = setup
            .invoke(
                Some(&chat),
                InvokeKind::FetchCommands {
                    context: CommandContext::Message,
                },
            )
            .await;
        assert_eq!(fine, InvokeAnswer::Done);
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_bot_gives_the_client_an_error_in_time() {
    for backend in backends() {
        let setup = Setup::open_with(backend, |config| config.bots.send_timeout = 1).await;
        let chat = setup.chat();
        setup.send(&chat, "hi").await;
        setup.bot.next_of("message").await;
        setup.bot.script(|_| {
            Scripted::json(json!({"task": {"type": "message", "value": "late"}})).after(Duration::from_secs(6))
        });
        let started = Instant::now();
        let answer = setup
            .invoke(Some(&chat), InvokeKind::TaskFetch(CardInvoke::default()))
            .await;
        assert_eq!(answer, InvokeAnswer::Refused(504));
        assert!(started.elapsed() < Duration::from_secs(4), "{:?}", started.elapsed());
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_not_in_the_conversation_or_not_for_the_user_is_refused_and_gets_nothing() {
    for backend in backends() {
        let setup = Setup::open(backend).await;
        let chat = setup.chat();
        setup.send(&chat, "hi").await;
        setup.bot.next_of("message").await;
        setup.bot.script(answers);
        let other = TestBot::start().await;
        other.script(answers);
        let answer = setup.registry.register("Other (synthetic)", &other.url).await;
        let other_id = answer["id"].as_str().unwrap().to_owned();
        let fetch = || InvokeKind::FetchCommands {
            context: CommandContext::Message,
        };

        // another bot in Ada's chat with Echo
        let refused = setup.invoke_bot(&other_id, Some(&chat), fetch()).await;
        assert_eq!(refused, InvokeAnswer::Refused(403));
        // Echo in Bob's chat with it, which Ada isn't in
        let bob = setup
            .registry
            .server
            .add_user("bob@own.test", "Bob Stone (synthetic)")
            .await;
        let bobs = personal_chat(&bob, &setup.bot_id);
        assert_eq!(setup.invoke(Some(&bobs), fetch()).await, InvokeAnswer::Refused(403));
        // a bot nobody registered
        let nobody = "00000000-0000-4000-8000-00000000dead";
        assert_eq!(
            setup.invoke_bot(nobody, Some(&chat), fetch()).await,
            InvokeAnswer::Refused(404)
        );
        // Echo's app no longer for Ada: refused, even in her chat with it
        let path = super::app_path(&setup.bot_id);
        let (status, changed) = setup
            .registry
            .call(Method::PATCH, &path, Some(json!({"everyone": false, "users": [bob]})))
            .await;
        assert_eq!(status, 200, "{changed}");
        assert_eq!(setup.invoke(Some(&chat), fetch()).await, InvokeAnswer::Refused(403));
        assert!(!setup.bot.gets_within("invoke", Duration::from_millis(300)).await);
        assert!(!other.gets_within("invoke", Duration::from_millis(300)).await);
        assert_eq!(other.hits(), 0);
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn no_user_token_reaches_the_bot_even_when_a_client_sends_one() {
    for backend in backends() {
        let setup = Setup::open(backend).await;
        let chat = setup.chat();
        setup.send(&chat, "hi").await;
        setup.bot.next_of("message").await;
        setup.bot.script(answers);
        // a synthetic JWT-shaped value: a Microsoft client's field must not pass through
        let synthetic = "eyJhbGciOiJub25lIn0.eyJhdWQiOiJzeW50aGV0aWMifQ.";
        let body = json!({"name": "composeExtension/fetchCommands", "appId": setup.bot_id,
            "value": {"commandContext": "Message"}, "imdisplayname": "Ada Lovelace",
            "userAadToken": synthetic, "conversation": {"id": chat}});
        let (status, answer) = setup.raw_invoke(&setup.bot_id, &body).await;
        assert_eq!(status, 200, "{answer}");
        let answer: Value = serde_json::from_str(&answer).unwrap();
        assert_eq!(answer["responseType"], "composeExtension");
        assert_eq!(answer["composeExtension"]["type"], "command");
        let got = setup.bot.next_of("invoke").await;
        assert!(!carries_token(&got.activity), "{}", got.activity);
        assert!(!got.activity.to_string().contains(synthetic));
        let session = setup.registry.server.session("ada@own.test").await;
        let ic3 = teams_trouter::TrouterCredentials::ic3(&session).await.unwrap();
        let bearer = got.authorization.as_deref().unwrap();
        assert!(!bearer.contains(ic3.token.expose()));
        // the server's own activity token, for the bot only
        let claims = got.token_claims().unwrap();
        assert_eq!(claims["aud"], setup.bot_id);
        assert!(claims.get("oid").is_none(), "{claims}");
        // a body that names another app than the bot's, and one that isn't an invoke
        let wrong = json!({"name": "composeExtension/fetchCommands", "appId": "someone-else",
            "value": {}, "conversation": {"id": chat}});
        assert_eq!(setup.raw_invoke(&setup.bot_id, &wrong).await.0, 404);
        assert_eq!(setup.raw_invoke(&setup.bot_id, &json!([1])).await.0, 400);
        let nameless = json!({"appId": setup.bot_id, "value": {}, "conversation": {"id": chat}});
        assert_eq!(setup.raw_invoke(&setup.bot_id, &nameless).await.0, 400);
        let odd_post = json!({"name": "task/fetch", "appId": setup.bot_id, "value": {},
            "conversation": {"id": format!("{chat};messageid=x\"y")}});
        assert_eq!(setup.raw_invoke(&setup.bot_id, &odd_post).await.0, 400);
        setup.stop().await;
    }
}

/// The `body[0]` text of the card a card message carries.
fn card_title(content: &str) -> Value {
    use base64::Engine as _;
    let start = content.find("b64=\"").unwrap() + 5;
    let end = start + content[start..].find('"').unwrap();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&content[start..end])
        .unwrap();
    let carried: Value = serde_json::from_slice(&decoded).unwrap();
    carried["attachments"][0]["content"]["body"][0]["text"].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_submit_reaches_the_bot_as_a_message_and_its_reply_syncs() {
    for backend in backends() {
        let setup = Setup::open(backend).await;
        let (card, connector) = setup.card_in_chat().await;
        let chat = setup.chat();
        let submitted = setup
            .invoke(
                Some(&chat),
                InvokeKind::Submit(card_invoke(&card, None, json!({"poll": 7, "answer": "soup"}))),
            )
            .await;
        assert_eq!(submitted, InvokeAnswer::Done);
        let got = setup.bot.next_of("message").await;
        assert_eq!(got.activity["value"]["answer"], "soup");
        let reply_to = got.activity["replyToId"].as_str().unwrap();
        let (status, answer) = connector.reply(&chat, reply_to, Connector::text("Soup it is")).await;
        assert_eq!(status, 200, "{answer}");
        let messages = setup.synced(&chat).await;
        assert!(messages.last().unwrap().content.contains("Soup it is"), "{messages:#?}");
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_execute_gets_the_new_card_and_the_bots_update_replaces_it_in_the_chat() {
    for backend in backends() {
        let setup = Setup::open(backend).await;
        let (card, connector) = setup.card_in_chat().await;
        let chat = setup.chat();
        assert_eq!(card_title(&setup.synced(&chat).await.last().unwrap().content), "Lunch?");
        setup.bot.script(answers);
        let voted = setup
            .invoke(
                Some(&chat),
                InvokeKind::Execute(card_invoke(&card, Some("vote"), json!({"poll": 7}))),
            )
            .await;
        let InvokeAnswer::Card(voted) = voted else {
            panic!("{voted:?}")
        };
        let got = setup.bot.next_of("invoke").await;
        let replaced = json!({"type": "message", "attachments": [adaptive(&voted)]});
        let (status, answer) = connector
            .update(&chat, got.activity["replyToId"].as_str().unwrap(), replaced)
            .await;
        assert_eq!(status, 200, "{answer}");
        let messages = setup.synced(&chat).await;
        let updated = messages.iter().find(|message| message.id == card).unwrap();
        assert_eq!(card_title(&updated.content), "Voted");
        assert!(updated.edit_time.is_some(), "{updated:?}");
        let parsed = teams_proto::chat::card::card_message_with_inputs(&updated.content).unwrap();
        assert!(parsed.drawn(), "{parsed:?}");
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_search_command_lists_results_and_a_chosen_one_comes_back_as_its_card() {
    for backend in backends() {
        let setup = Setup::open(backend).await;
        let chat = setup.chat();
        setup.send(&chat, "hi").await;
        setup.bot.next_of("message").await;
        setup.bot.script(answers);
        // the commands come without a conversation too, as from a compose box with none open
        let commands = setup
            .invoke(
                None,
                InvokeKind::FetchCommands {
                    context: CommandContext::Compose,
                },
            )
            .await;
        assert!(matches!(commands, InvokeAnswer::Commands(_)), "{commands:?}");
        let got = setup.bot.next_of("invoke").await;
        assert_eq!(got.activity["conversation"]["id"], chat);
        let query = InvokeKind::Query {
            command: "searchPolls".to_owned(),
            parameters: vec![("q".to_owned(), "poll".to_owned())],
            skip: 0,
            count: 10,
        };
        let InvokeAnswer::Results { attachments, .. } = setup.invoke(Some(&chat), query).await else {
            panic!("no results")
        };
        let tap = attachments[0]["preview"]["content"]["tap"]["value"].clone();
        setup.bot.next_of("invoke").await;
        let chosen = setup
            .invoke(Some(&chat), InvokeKind::SelectItem { value: tap.clone() })
            .await;
        let InvokeAnswer::Results { attachments, .. } = chosen else {
            panic!("{chosen:?}")
        };
        assert_eq!(attachments[0]["content"]["title"], "Poll 7");
        assert_eq!(setup.bot.next_of("invoke").await.activity["value"], tap);
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_card_dialog_opens_and_its_submit_is_answered_and_posted() {
    for backend in backends() {
        let setup = Setup::open(backend).await;
        let (card, connector) = setup.card_in_chat().await;
        let chat = setup.chat();
        setup.bot.script(answers);
        let InvokeAnswer::Dialog(dialog) = setup
            .invoke(
                Some(&chat),
                InvokeKind::TaskFetch(card_invoke(&card, None, json!({"poll": 7}))),
            )
            .await
        else {
            panic!("no dialog")
        };
        assert_eq!(dialog.card.unwrap()["body"][0]["text"], "Ask");
        setup.bot.next_of("invoke").await;
        let saved = setup
            .invoke(
                Some(&chat),
                InvokeKind::TaskSubmit(card_invoke(&card, None, json!({"answer": "tea"}))),
            )
            .await;
        assert_eq!(saved, InvokeAnswer::Message("Poll saved".to_owned()));
        let got = setup.bot.next_of("invoke").await;
        assert_eq!(got.activity["value"]["data"]["answer"], "tea");
        let (status, _) = connector
            .send(
                &chat,
                json!({"type": "message", "attachments": [adaptive(&poll_card("Tea poll"))]}),
            )
            .await;
        assert_eq!(status, 200);
        assert_eq!(
            card_title(&setup.synced(&chat).await.last().unwrap().content),
            "Tea poll"
        );
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reactions_to_the_bots_message_reach_it_and_others_dont() {
    for backend in backends() {
        let setup = Setup::open(backend).await;
        let (card, _) = setup.card_in_chat().await;
        let chat = setup.chat();
        setup.synced(&chat).await;
        let react = |key: &str, message: &str, remove: bool| {
            let (conversation, message_id, key) = (chat.clone(), message.to_owned(), key.to_owned());
            if remove {
                teams_core::engine::Action::Unreact {
                    conversation,
                    message_id,
                    key,
                }
            } else {
                teams_core::engine::Action::React {
                    conversation,
                    message_id,
                    key,
                }
            }
        };
        let row = setup.engine.write(react("like", &card, false)).await.unwrap();
        assert_eq!(row.state, teams_store::OutboxState::Sent, "{row:?}");
        let got = setup.bot.next_of("messageReaction").await;
        assert_eq!(got.activity["replyToId"], card);
        assert_eq!(got.activity["reactionsAdded"], json!([{"type": "like"}]));
        assert!(got.activity.get("reactionsRemoved").is_none());
        assert_eq!(got.activity["from"]["id"], format!("29:{}", setup.ada));
        assert_eq!(got.activity["conversation"]["id"], chat);
        assert_eq!(got.activity["serviceUrl"], format!("{ORIGIN}/bot/"));

        let row = setup.engine.write(react("like", &card, true)).await.unwrap();
        assert_eq!(row.state, teams_store::OutboxState::Sent, "{row:?}");
        let got = setup.bot.next_of("messageReaction").await;
        assert_eq!(got.activity["reactionsRemoved"], json!([{"type": "like"}]));

        // Ada's own message: her reaction to it is nothing to the bot
        let own = setup.send(&chat, "my own words").await;
        setup.bot.next_of("message").await;
        let row = setup.engine.write(react("heart", &own, false)).await.unwrap();
        assert_eq!(row.state, teams_store::OutboxState::Sent, "{row:?}");
        assert!(!setup.bot.gets_within("messageReaction", Duration::from_secs(3)).await);
        setup.stop().await;
    }
}
