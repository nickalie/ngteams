//! The text of a card in the control socket's reads (`ctl::reads`), through the real protocol and server, over lab
//! account a's engine on the mock chat service (`teams-mock`, loopback only): a monitoring alert's Adaptive Card that
//! a workflow posts in the team's channel, and a webhook's connector card in a's chat with b. `messages` gives the
//! text on each card, and `search` finds the chat's card by that text. A card with no text, and a text message with a
//! link's app card, are read directly. Every id, name and text is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared harness holds more than this test uses")]
mod ctl_support;

use std::future::{self, Future};
use std::rc::Rc;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ctl_support::{ask, quit};
use gpui::TestAppContext;
use ngteams::accounts::{Runner, Started};
use ngteams::accounts_file::{PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::ctl::reads::{ReadText, text_of};
use ngteams::setup::Setup;
use serde_json::{Value, json};
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, TokenClient, TokenError};
use teams_core::account::AccountEngine;
use teams_core::engine::{self, EngineOptions, Parts};
use teams_core::{CallFloor, Outbox};
use teams_ctl::protocol::{MessageRow, Request, Response};
use teams_mock::{AB_CHAT, CHANNEL, CHAT_BASE, ChatServiceMock, LAB_A, TENANT};
use teams_proto::chat::card::{ADAPTIVE, CARD_TYPE, CONNECTOR};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::{Message, Store};
use teams_ui::bus::BusError;
use teams_ui::{SettingsFile, ViewTrace};

const UPN: &str = "bot-a@lab.example.test";
const WORKFLOW: (&str, &str) = ("28:00000000-0000-4000-8000-00000000f10e", "Workflows");

struct Tokens(String);

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

/// A card message's `content` with `attachments`, as the chat service gives it.
fn card(attachments: &Value) -> String {
    let activity = json!({"type": "message/card", "attachments": attachments});
    let b64 = STANDARD.encode(activity.to_string());
    format!(
        "<URIObject type=\"SWIFT.1\">Card - access it on <a href=\"https://go.skype.com/cards.unsupported\">\
         https://go.skype.com/cards.unsupported</a>. <Swift b64=\"{b64}\"></Swift></URIObject>"
    )
}

fn alert() -> String {
    card(
        &json!([{"contentType": ADAPTIVE, "content": {"type": "AdaptiveCard", "version": "1.4",
            "body": [
                {"type": "TextBlock", "text": "**[FIRING:1] SyntheticDiskFull**", "weight": "Bolder"},
                {"type": "ColumnSet", "columns": [{"type": "Column", "items": [
                    {"type": "FactSet", "facts": [
                        {"title": "severity", "value": "critical"},
                        {"title": "instance", "value": "db-1.example.invalid:9100"},
                    ]},
                ]}]},
            ],
            "actions": [{"type": "Action.OpenUrl", "title": "Runbook", "url": "https://example.invalid/runbook"}],
        }}]),
    )
}

fn connector() -> String {
    card(
        &json!([{"contentType": CONNECTOR, "content": {"@type": "MessageCard", "title": "Probe down",
        "sections": [{"facts": [{"name": "endpoint", "value": "api"}]}]}}]),
    )
}

fn started(handle: engine::EngineHandle) -> Started<MemorySecretStore> {
    let mut account = PersonalAccount::new(UPN, LAB_A.strip_prefix("8:orgid:").unwrap(), TENANT).unwrap();
    account.name = Some("bot-a".to_owned());
    let session = Session::new(
        UPN,
        MemorySecretStore::new(),
        TokenClient::new(EntraConfig::teams_native().with_authority("http://127.0.0.1:9/")).unwrap(),
        AuthzClient::with_endpoint("http://127.0.0.1:9/authz").unwrap(),
        Region::default(),
    );
    Started {
        account,
        engine: AccountEngine {
            handle,
            session: Arc::new(session),
        },
        calls: None,
    }
}

fn rows(response: Response) -> Vec<MessageRow> {
    match response {
        Response::Messages { messages, .. } | Response::Search { messages } => messages,
        other => panic!("not messages: {other:?}"),
    }
}

#[test]
fn messages_and_search_give_the_text_on_a_card() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let mock = runtime.block_on(ChatServiceMock::start()).unwrap();
    let alert_id = mock.add_message(CHANNEL, WORKFLOW, CARD_TYPE, &alert()).unwrap();
    let probe_id = mock.add_message(AB_CHAT, WORKFLOW, CARD_TYPE, &connector()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let http = || {
        let tokens = Arc::new(Tokens(mock.token(LAB_A).unwrap()));
        HttpTransport::with_mock_origin(tokens, CHAT_BASE, &mock.origin()).unwrap()
    };
    let handle = engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("a.sqlite")).unwrap(),
        client: Arc::new(Client::new(http())),
        outbox: Outbox::new(Arc::new(http()), Author::new(Mri::new(LAB_A), "bot-a")),
        push: None,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap();
    handle.requests().sync().blocking_wait().unwrap();
    let socket = dir.path().join("run/ngteams/ctl.sock");
    let cx = TestAppContext::single();
    let launch = Launch {
        first: started(handle),
        others: Vec::new(),
        settings: app::settings(SettingsFile::new(dir.path().join("settings.toml")), None),
        bus: Err(BusError::NotGiven),
        sign_in_again: Rc::new(|_| Rc::new(|_| {})),
        runner: Runner {
            setup: Setup::new(PersonalPaths::new(dir.path().join("config"), dir.path().join("data"))).unwrap(),
            floor: CallFloor::new(),
            devices: None,
        },
        read_only: false,
        trace: Arc::new(ViewTrace::off()),
        ctl: Some(socket.clone()),
    };
    assert!(cx.update(|cx| app::launch(cx, launch)).is_ok(), "the window opens");
    cx.run_until_parked();
    let read = |request| rows(ask(&cx, &socket, None, request).unwrap());

    let channel = read(Request::Messages {
        chat: CHANNEL.to_owned(),
        limit: None,
        before: None,
    });
    let posted = channel.iter().find(|row| row.id == alert_id).expect("the alert");
    assert_eq!(
        posted.text,
        "[FIRING:1] SyntheticDiskFull\nseverity: critical\ninstance: db-1.example.invalid:9100\n\
         Runbook (https://example.invalid/runbook)"
    );
    assert!(posted.card);

    let chat = read(Request::Messages {
        chat: AB_CHAT.to_owned(),
        limit: None,
        before: None,
    });
    let probe = chat.iter().find(|row| row.id == probe_id).expect("the connector card");
    assert_eq!((probe.text.as_str(), probe.card), ("Probe down\nendpoint: api", true));
    assert!(
        chat.iter().filter(|row| row.id != probe_id).all(|row| !row.card),
        "{chat:?}"
    );

    let found = read(Request::Search {
        query: "endpoint".to_owned(),
        limit: None,
    });
    assert_eq!(
        found.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        [probe_id.as_str()]
    );
    quit(&cx);
}

fn stored(message_type: &str, content: &str, properties: Option<&str>) -> Message {
    Message {
        conversation_id: AB_CHAT.to_owned(),
        id: "1".to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(WORKFLOW.0.to_owned()),
        sender_name: Some(WORKFLOW.1.to_owned()),
        message_type: message_type.to_owned(),
        content: content.to_owned(),
        properties: properties.map(str::to_owned),
        arrival_time: 1,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

#[test]
fn a_card_without_text_says_sent_a_card_and_a_link_s_app_card_follows_the_text() {
    let picture = card(&json!([{"contentType": ADAPTIVE, "content": {"type": "AdaptiveCard",
        "body": [{"type": "Image", "url": "https://example.invalid/graph.png"}]}}]));
    let read = |message_type, content: &str, properties| text_of(&stored(message_type, content, properties));
    let sent_a_card = ReadText {
        text: "Sent a card".to_owned(),
        card: true,
    };
    assert_eq!(read(CARD_TYPE, &picture, None), Some(sent_a_card.clone()));
    assert_eq!(
        read(CARD_TYPE, "<URIObject>no card</URIObject>", None),
        Some(sent_a_card.clone())
    );

    let app_card = json!({"cards": [{"contentType": ADAPTIVE, "content": {"type": "AdaptiveCard",
        "body": [{"type": "TextBlock", "text": "A video title"}]}}]})
    .to_string();
    assert_eq!(
        read("RichText/Html", "<p>see this</p>", Some(&app_card)),
        Some(ReadText {
            text: "see this\nA video title".to_owned(),
            card: true,
        })
    );
    let empty_app_card =
        json!({"cards": [{"contentType": ADAPTIVE, "content": {"type": "AdaptiveCard", "body": []}}]}).to_string();
    assert_eq!(
        read("Text", "plain ", Some(&empty_app_card)),
        Some(ReadText {
            text: "plain".to_owned(),
            card: true,
        })
    );
    assert_eq!(
        read("RichText/Html", "<p>only text</p>", None),
        Some(ReadText {
            text: "only text".to_owned(),
            card: false,
        })
    );
}
