//! Bot invokes through the engine (`teams_core::bots`) against the mock chat service and its scripted bot: each answer
//! type comes back, a Microsoft account's body carries its Teams service token and an own-server account's none, an
//! invoke goes out once, and read-only mode sends nothing. No tenant is reached.

#![cfg(test)]

use std::future::{self, Future};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::{SPACES_RESOURCE, TokenError};
use teams_core::Outbox;
use teams_core::bots::BotAsk;
use teams_core::engine::{self, EngineError, EngineHandle, EngineOptions, Parts};
use teams_mock::{ABC_CHAT, BotReply, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, bot_answer};
use teams_proto::bot::{CardInvoke, CommandContext, InvokeAnswer, InvokeKind};
use teams_proto::{Author, Client, Cloud, Credentials, HttpTransport, Mri};
use teams_store::Store;

const BOT: &str = "28:00000000-0000-4000-8000-0000000000b0";
const APP: &str = "00000000-0000-4000-8000-0000000000a0";
/// The Teams service token the test's credentials hand out: made up, and never the chat service's bearer.
const SPACES_TOKEN: &str = "synthetic-spaces-token";

/// The mock's chat token for every audience but the Teams service's, which gets [`SPACES_TOKEN`]; the audiences asked
/// are kept.
struct Tokens {
    chat: String,
    asked: Mutex<Vec<String>>,
}

impl Credentials for Tokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(audience.to_owned());
        let token = if audience == SPACES_RESOURCE {
            SPACES_TOKEN.to_owned()
        } else {
            self.chat.clone()
        };
        future::ready(Ok(Secret::new(token)))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

struct Engine {
    handle: EngineHandle,
    tokens: Arc<Tokens>,
    _dir: tempfile::TempDir,
}

fn engine(mock: &ChatServiceMock, cloud: &Cloud, read_only: bool) -> Engine {
    let tokens = Arc::new(Tokens {
        chat: mock.token(LAB_A).unwrap(),
        asked: Mutex::new(Vec::new()),
    });
    let base = cloud.service_url(CHAT_BASE, "/api/chatsvc/amer").unwrap();
    let origin = mock.origin();
    let http = || {
        let transport =
            HttpTransport::for_cloud_with_mock_origin(Arc::clone(&tokens), base.as_str(), cloud.clone(), &origin)
                .unwrap()
                .for_account(Mri::new(LAB_A));
        let transport = if cloud.is_microsoft() {
            transport.with_middle_tier(teams_mock::MIDDLE_TIER_BASE).unwrap()
        } else {
            transport
        };
        if read_only { transport.read_only() } else { transport }
    };
    let dir = tempfile::tempdir().unwrap();
    let handle = engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("store.sqlite")).unwrap(),
        client: Arc::new(Client::new(http())),
        outbox: Outbox::new(Arc::new(http()), Author::new(Mri::new(LAB_A), "Lab A")),
        push: None,
        options: EngineOptions {
            tick: Duration::from_millis(50),
            ..EngineOptions::default()
        },
        known_names: vec![(LAB_A.to_owned(), "Lab A".to_owned())],
    })
    .unwrap();
    Engine {
        handle,
        tokens,
        _dir: dir,
    }
}

fn ask(conversation: Option<&str>, kind: InvokeKind) -> BotAsk {
    BotAsk {
        bot: Mri::new(BOT),
        app_id: APP.to_owned(),
        conversation: conversation.map(str::to_owned),
        kind,
    }
}

fn card(data: Value) -> CardInvoke {
    CardInvoke {
        message: Some("1700000000003".to_owned()),
        data,
        ..CardInvoke::default()
    }
}

fn script(mock: &ChatServiceMock) {
    let answer = |name: &str| BotReply::answer(bot_answer(name).unwrap());
    mock.script_bot(
        BOT,
        "Synthetic Bot",
        &[
            ("composeExtension/fetchCommands", answer("commands")),
            ("composeExtension/fetchTask", answer("dialog")),
            (
                "composeExtension/submitAction",
                BotReply::done().posting("RichText/Html", "<p>Synthetic reminder set</p>"),
            ),
            ("composeExtension/query", answer("results")),
            ("composeExtension/selectItem", answer("message")),
            ("adaptiveCard/action", answer("card")),
            ("task/fetch", answer("dialog")),
            ("task/submit", answer("taskMessage")),
            ("message", answer("error")),
        ],
    );
}

#[tokio::test]
async fn a_microsoft_account_gets_each_answer_and_sends_its_teams_token_only_in_the_body() {
    let mock = ChatServiceMock::start().await.unwrap();
    script(&mock);
    let a = engine(&mock, &Cloud::Microsoft, false);
    let invoke = |conversation: Option<&str>, kind| a.handle.bot_invoke(ask(conversation, kind));

    let commands = invoke(
        None,
        InvokeKind::FetchCommands {
            context: CommandContext::Message,
        },
    )
    .await
    .unwrap();
    assert!(matches!(commands, InvokeAnswer::Commands(listed) if listed.len() == 2));
    let dialog = InvokeKind::FetchTask {
        command: "synthetic-command-1".to_owned(),
        context: CommandContext::Compose,
        message: None,
    };
    assert!(matches!(
        invoke(Some(ABC_CHAT), dialog).await.unwrap(),
        InvokeAnswer::Dialog(dialog) if dialog.card.is_some()
    ));
    let submit = InvokeKind::SubmitAction {
        command: "synthetic-command-1".to_owned(),
        context: CommandContext::Compose,
        data: serde_json::Map::new(),
        message: None,
    };
    assert_eq!(invoke(Some(ABC_CHAT), submit).await.unwrap(), InvokeAnswer::Done);
    let query = InvokeKind::Query {
        command: "synthetic-search".to_owned(),
        parameters: vec![("searchText".to_owned(), "lab".to_owned())],
        skip: 0,
        count: 25,
    };
    assert!(matches!(
        invoke(Some(ABC_CHAT), query).await.unwrap(),
        InvokeAnswer::Results { attachments, .. } if attachments.len() == 1
    ));
    let select = InvokeKind::SelectItem {
        value: json!({"itemId": "synthetic-item-3"}),
    };
    assert_eq!(
        invoke(Some(ABC_CHAT), select).await.unwrap(),
        InvokeAnswer::Message("Nothing found.".to_owned())
    );
    let execute = InvokeKind::Execute(card(json!({"request": 1})));
    assert!(matches!(
        invoke(Some(ABC_CHAT), execute).await.unwrap(),
        InvokeAnswer::Card(_)
    ));
    let fetch = InvokeKind::TaskFetch(card(json!({"msteams": {"type": "task/fetch"}})));
    assert!(matches!(
        invoke(Some(ABC_CHAT), fetch).await.unwrap(),
        InvokeAnswer::Dialog(_)
    ));
    let task_submit = InvokeKind::TaskSubmit(card(json!({"form": 1})));
    assert_eq!(
        invoke(Some(ABC_CHAT), task_submit).await.unwrap(),
        InvokeAnswer::Message("Thanks, saved.".to_owned())
    );
    let plain = InvokeKind::Submit(card(json!({"choice": "yes"})));
    assert!(matches!(
        invoke(Some(ABC_CHAT), plain).await.unwrap(),
        InvokeAnswer::Error { code: Some(code), .. } if code == "BadRequest"
    ));

    sent_once_with_the_teams_token_in_the_body(&mock, &a.tokens).await;
    a.handle.shutdown();
}

/// The nine invokes went out once each, every body with the Teams service token and none of them with it as bearer.
async fn sent_once_with_the_teams_token_in_the_body(mock: &ChatServiceMock, tokens: &Tokens) {
    let taken = mock.bot_invokes();
    assert_eq!(taken.len(), 9);
    for record in &taken {
        assert_eq!(record.caller, LAB_A);
        assert_eq!(record.bot, BOT);
        assert_eq!(record.body["userAadToken"], SPACES_TOKEN, "{}", record.body["name"]);
        assert_eq!(record.body["imdisplayname"], "Lab A");
        assert_eq!(record.body["appId"], APP);
    }
    assert!(taken[0].body.get("conversation").is_none());
    assert_eq!(taken[1].body["conversation"]["id"], ABC_CHAT);
    let requests = mock.received_requests().await;
    let invokes: Vec<_> = requests
        .iter()
        .filter(|request| request.url.path().ends_with("/invoke"))
        .collect();
    assert_eq!(invokes.len(), 9, "each goes out once");
    for request in invokes {
        let bearer = request.headers.get("authorization").unwrap().to_str().unwrap();
        assert!(!bearer.contains(SPACES_TOKEN), "the Teams token goes only in the body");
    }
    assert!(
        tokens
            .asked
            .lock()
            .unwrap()
            .iter()
            .any(|audience| audience == SPACES_RESOURCE)
    );
}

#[tokio::test]
async fn the_bots_post_after_a_submit_reaches_the_conversation() {
    let mock = ChatServiceMock::start().await.unwrap();
    script(&mock);
    let a = engine(&mock, &Cloud::Microsoft, false);
    let submit = InvokeKind::SubmitAction {
        command: "synthetic-command-1".to_owned(),
        context: CommandContext::Message,
        data: serde_json::Map::new(),
        message: None,
    };
    let _ = mock.take_events(LAB_B);
    assert_eq!(
        a.handle.bot_invoke(ask(Some(ABC_CHAT), submit)).await.unwrap(),
        InvokeAnswer::Done
    );
    let events = mock.take_events(LAB_B);
    assert!(
        events
            .iter()
            .any(|event| event.from == BOT && event.resource["imdisplayname"] == "Synthetic Bot"),
        "{events:?}"
    );
    a.handle.shutdown();
}

#[tokio::test]
async fn refusals_come_back_and_nothing_is_sent_again() {
    let mock = ChatServiceMock::start().await.unwrap();
    script(&mock);
    let a = engine(&mock, &Cloud::Microsoft, false);
    let fetch = || InvokeKind::FetchCommands {
        context: CommandContext::Message,
    };
    let unknown = BotAsk {
        bot: Mri::new("28:00000000-0000-4000-8000-0000000000b9"),
        ..ask(None, fetch())
    };
    assert_eq!(a.handle.bot_invoke(unknown).await.unwrap(), InvokeAnswer::Refused(404));
    let outside = "19:00000000-0000-4000-8000-00000000000b_00000000-0000-4000-8000-00000000000c@unq.gbl.spaces";
    assert!(matches!(
        a.handle.bot_invoke(ask(Some(outside), fetch())).await.unwrap(),
        InvokeAnswer::Refused(403 | 404)
    ));
    mock.refuse_next("POST", "/invoke", 503).await;
    assert_eq!(
        a.handle.bot_invoke(ask(None, fetch())).await.unwrap(),
        InvokeAnswer::Refused(503)
    );
    let requests = mock.received_requests().await;
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.path().ends_with("/invoke"))
            .count(),
        3,
        "a 503 isn't sent again"
    );
    let not_a_bot = BotAsk {
        bot: Mri::new(LAB_B),
        ..ask(None, fetch())
    };
    assert!(matches!(
        a.handle.bot_invoke(not_a_bot).await,
        Err(EngineError::BotNotReached { .. })
    ));
    a.handle.shutdown();
}

#[tokio::test]
async fn read_only_sends_no_invoke() {
    let mock = ChatServiceMock::start().await.unwrap();
    script(&mock);
    let a = engine(&mock, &Cloud::Microsoft, true);
    let error = a
        .handle
        .bot_invoke(ask(
            None,
            InvokeKind::FetchCommands {
                context: CommandContext::Message,
            },
        ))
        .await
        .unwrap_err();
    assert!(matches!(error, EngineError::BotNotReached { .. }), "{error}");
    assert_eq!(error.kind(), "bot not reached");
    assert!(mock.bot_invokes().is_empty());
    a.handle.shutdown();
}

#[tokio::test]
async fn an_own_server_account_sends_no_token_in_the_body() {
    let cloud = Cloud::own("https://chat.example:8443").unwrap();
    let mock = ChatServiceMock::start_for_cloud(cloud.clone()).await.unwrap();
    script(&mock);
    let a = engine(&mock, &cloud, false);
    let execute = InvokeKind::Execute(card(json!({"request": 1})));
    assert!(matches!(
        a.handle.bot_invoke(ask(Some(ABC_CHAT), execute)).await.unwrap(),
        InvokeAnswer::Card(_)
    ));
    let taken = mock.bot_invokes();
    assert_eq!(taken.len(), 1);
    assert!(taken[0].body.get("userAadToken").is_none());
    assert_eq!(taken[0].body["messageId"], "1700000000003");
    assert!(
        !a.tokens
            .asked
            .lock()
            .unwrap()
            .iter()
            .any(|audience| audience == SPACES_RESOURCE),
        "no Microsoft token is even asked for"
    );
    a.handle.shutdown();
}

#[tokio::test]
async fn the_accounts_apps_with_message_extensions_come_with_their_commands() {
    let mock = ChatServiceMock::start().await.unwrap();
    for (definition, state) in teams_mock::extension_apps() {
        let id = definition["id"].as_str().unwrap().to_owned();
        mock.add_app(definition);
        if state != "Featured" {
            mock.install_app(teams_mock::AppPlace::Personal, &id);
        }
    }
    let a = engine(&mock, &Cloud::Microsoft, false);

    let apps = a.handle.message_extensions().await.unwrap();

    let names: Vec<_> = apps.iter().map(|app| (app.name().to_owned(), app.installed)).collect();
    assert_eq!(
        names,
        [
            ("Made-up Finder".to_owned(), true),
            ("Made-up Praise".to_owned(), true),
            ("Made-up Tasks".to_owned(), true)
        ]
    );
    let compose = apps[0].commands_in(CommandContext::Compose);
    assert_eq!(compose.len(), 2);
    assert!(!compose[0].action && compose[0].initial_run);
    assert_eq!(compose[0].bot.as_str(), "28:00000000-0000-4000-8000-00000000eb01");
    assert!(apps[2].commands_in(CommandContext::Compose).is_empty());
    assert_eq!(apps[2].commands_in(CommandContext::Message).len(), 1);
    assert!(apps[2].fetches_commands().is_some());
    // the read brings the bots' apps along, as every read of the account's apps does
    let found = a
        .handle
        .bot_app("28:00000000-0000-4000-8000-00000000eb01")
        .await
        .unwrap();
    assert_eq!(found.map(|app| app.app.name), Some("Made-up Finder".to_owned()));
}

/// A sign-in card's code (`signin/verifyState`) against the mock: one attempt with the Teams token in the body as any
/// invoke; a code the bot refuses comes back as its status, one it takes as done, with the bot's message after.
#[tokio::test]
async fn a_sign_in_code_goes_once_and_the_bots_answer_comes_back() {
    let mock = ChatServiceMock::start().await.unwrap();
    mock.script_bot(
        BOT,
        "Synthetic Bot",
        &[(
            "signin/verifyState",
            BotReply::done().posting("RichText/Html", "<p>Signed in (synthetic)</p>"),
        )],
    );
    let a = engine(&mock, &Cloud::Microsoft, false);
    let verify = || InvokeKind::VerifyState {
        code: "402917".to_owned(),
    };
    assert_eq!(
        a.handle.bot_invoke(ask(Some(ABC_CHAT), verify())).await.unwrap(),
        InvokeAnswer::Done
    );
    let taken = mock.bot_invokes();
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].body["name"], "signin/verifyState");
    assert_eq!(taken[0].body["value"], json!({"state": "402917"}));
    assert_eq!(taken[0].body["conversation"]["id"], ABC_CHAT);
    assert!(taken[0].body["userAadToken"].is_string(), "the Teams token, as every invoke");
    mock.refuse_next("POST", "/invoke", 404).await;
    assert_eq!(
        a.handle.bot_invoke(ask(Some(ABC_CHAT), verify())).await.unwrap(),
        InvokeAnswer::Refused(404)
    );
    a.handle.shutdown();
}
