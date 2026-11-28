//! QA of task 1029: a person reads the cards of a channel and a chat with `ngteamsctl`, against the running app. Each
//! `ngteamsctl` run is a process of its own: this test binary started again, which runs `teams_ctl::cli` as the
//! `ngteamsctl` binary does (its arguments, its environment, its stdout, its stderr and its exit code). The app runs
//! in this process, lab account a's engine on the mock chat service and its Trouter (`teams-mock`, loopback only).
//!
//! - **messages**: a workflow's alert Adaptive Card (text blocks, facts, a table, buttons with links), a hero card, a
//!   thumbnail card and a card with only a picture in the team's channel; a connector card, a link's app card and a
//!   plain text message in a's chat with b. As the table (the `(card)` mark) and as `--json` (`card: true`).
//! - **search**: the chat's cards by their text, also after an upgrade from a store of schema 29.
//! - **wait message**: a new card that the push channel delivers, by its text.
//!
//! Every id, name, link and text is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]
#![cfg(target_os = "linux")]

#[expect(dead_code, reason = "the shared harness holds more than this test uses")]
mod ctl_support;

use std::future::{self, Future};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use clap::Parser as _;
use ctl_support::{pump_until, pump_until_done, quit};
use gpui::TestAppContext;
use ngteams::accounts::{Runner, Started};
use ngteams::accounts_file::{PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::setup::Setup;
use serde_json::{Value, json};
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::{AccessToken, Renewal, Session};
use teams_auth::token::{EntraConfig, TokenClient, TokenError};
use teams_core::account::AccountEngine;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{CallFloor, Outbox};
use teams_ctl::cli::{self, Cli};
use teams_ctl::path::Env;
use teams_mock::{AB_CHAT, CHANNEL, CHAT_BASE, ChatServiceMock, LAB_A, MESSAGING_APP, TENANT, TrouterMock};
use teams_proto::chat::card::{ADAPTIVE, CARD_TYPE, CONNECTOR, HERO, THUMBNAIL};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::bus::BusError;
use teams_ui::{SettingsFile, ViewTrace};
use tokio::sync::broadcast;

/// The JSON array of arguments that makes this binary run as `ngteamsctl`.
const CHILD_ARGS: &str = "CTL_CARD_TEXT_CLI_ARGS";
/// The files the child writes its stdout and its stderr in.
const CHILD_OUT: &str = "CTL_CARD_TEXT_CLI_OUT";
const CHILD_ERR: &str = "CTL_CARD_TEXT_CLI_ERR";
/// The name of the test that is the child.
const CHILD_TEST: &str = "ngteamsctl_child";

const UPN: &str = "bot-a@lab.example.test";
const WORKFLOW: (&str, &str) = ("28:00000000-0000-4000-8000-00000000f10e", "Workflows");
const VIDEO_APP: &str = "00000000-0000-4000-8000-0000000a99c0";

const ALERT_TEXT: &str = "[FIRING:2] SyntheticQueueStuck\nTwo queues stopped.\nseverity: warning\n\
     team: synthetic-ops\nqueue | depth\norders-q | 1200\nSilence (https://example.invalid/silence)\n\
     Runbook (https://example.invalid/runbook)";
const HERO_TEXT: &str = "Build 7 passed\nmain\nAll 12 checks passed.\nOpen the build (https://example.invalid/build/7)";
const THUMBNAIL_TEXT: &str = "Deploy started\nto staging\nLogs (https://example.invalid/logs)";
const CONNECTOR_TEXT: &str = "Probe failed\nThe probe of api failed.\nuptime-bot\n\
     endpoint: https://api.example.invalid/health\nStatus page (https://status.example.invalid/api)";
const VIDEO_TEXT: &str = "Synthetic video title\nSynthetic channel\nOpen";

/// When this binary runs as `ngteamsctl` (see the module docs), it does what the `ngteamsctl` binary does.
#[test]
#[ignore = "run by ngteamsctl() as the ngteamsctl binary"]
fn ngteamsctl_child() {
    let args: Vec<String> = serde_json::from_str(&std::env::var(CHILD_ARGS).unwrap()).unwrap();
    let mut out = std::fs::File::create(std::env::var(CHILD_OUT).unwrap()).unwrap();
    let mut err = std::fs::File::create(std::env::var(CHILD_ERR).unwrap()).unwrap();
    let code = match Cli::try_parse_from(std::iter::once("ngteamsctl".to_owned()).chain(args)) {
        Ok(parsed) => cli::run(&parsed, &Env::from_process(), &mut out, &mut err) as i32,
        Err(error) => {
            use std::io::Write as _;
            let _ = write!(err, "{error}");
            error.exit_code()
        }
    };
    std::process::exit(code);
}

/// What an `ngteamsctl` run printed and how it exited.
#[derive(Debug)]
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    fn json(&self) -> Value {
        assert_eq!(self.code, 0, "{}", self.stderr);
        let line: Value = serde_json::from_str(&self.stdout).unwrap_or_else(|error| panic!("{error}: {self:?}"));
        line["ok"].clone()
    }
}

/// `ngteamsctl <args>` against `socket`, while the test thread steps the app.
fn ngteamsctl(cx: &TestAppContext, socket: &Path, args: &[&str]) -> Run {
    let logs = tempfile::tempdir().unwrap();
    let (out, err) = (logs.path().join("out"), logs.path().join("err"));
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", CHILD_TEST, "--ignored", "--test-threads=1", "--quiet"])
        .current_dir(logs.path())
        .env_clear()
        .envs(std::env::var_os("LLVM_PROFILE_FILE").map(|value| ("LLVM_PROFILE_FILE", value)))
        .env(CHILD_ARGS, serde_json::to_string(args).unwrap())
        .env(CHILD_OUT, &out)
        .env(CHILD_ERR, &err)
        .env("NGTEAMS_CTL_SOCKET", socket)
        .env("HOME", logs.path())
        .env("XDG_RUNTIME_DIR", logs.path())
        .env("TZ", "UTC");
    let thread = std::thread::spawn(move || command.output().unwrap());
    pump_until_done(cx, &thread);
    let output = thread.join().unwrap();
    assert!(
        output.status.code() != Some(101),
        "ngteamsctl {args:?} panicked: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Run {
        code: output.status.code().unwrap_or(-1),
        stdout: std::fs::read_to_string(&out).unwrap_or_default(),
        stderr: std::fs::read_to_string(&err).unwrap_or_default(),
    }
}

/// The mock token of account a, for its transport and its push channel alike.
struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        self.ic3()
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
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
        &json!([{"contentType": ADAPTIVE, "content": {"type": "AdaptiveCard", "version": "1.5",
            "body": [
                {"type": "TextBlock", "text": "**[FIRING:2] SyntheticQueueStuck**", "weight": "Bolder", "size": "Large"},
                {"type": "TextBlock", "text": "Two queues stopped.", "wrap": true},
                {"type": "FactSet", "facts": [
                    {"title": "severity", "value": "warning"},
                    {"title": "team", "value": "synthetic-ops"},
                ]},
                {"type": "Table", "columns": [{"width": 1}, {"width": 1}], "rows": [
                    {"type": "TableRow", "cells": [
                        {"type": "TableCell", "items": [{"type": "TextBlock", "text": "queue"}]},
                        {"type": "TableCell", "items": [{"type": "TextBlock", "text": "depth"}]},
                    ]},
                    {"type": "TableRow", "cells": [
                        {"type": "TableCell", "items": [{"type": "TextBlock", "text": "orders-q"}]},
                        {"type": "TableCell", "items": [{"type": "TextBlock", "text": "1200"}]},
                    ]},
                ]},
                {"type": "ActionSet", "actions": [
                    {"type": "Action.OpenUrl", "title": "Silence", "url": "https://example.invalid/silence"},
                ]},
            ],
            "actions": [{"type": "Action.OpenUrl", "title": "Runbook", "url": "https://example.invalid/runbook"}],
        }}]),
    )
}

fn hero() -> String {
    card(
        &json!([{"contentType": HERO, "content": {"title": "Build 7 passed", "subtitle": "main",
        "text": "All 12 checks passed.", "images": [{"url": "https://example.invalid/build.png"}],
        "buttons": [{"type": "openUrl", "title": "Open the build", "value": "https://example.invalid/build/7"}]}}]),
    )
}

fn thumbnail() -> String {
    card(
        &json!([{"contentType": THUMBNAIL, "content": {"title": "Deploy started", "text": "to staging",
        "buttons": [{"type": "openUrl", "title": "Logs", "value": "https://example.invalid/logs"}]}}]),
    )
}

fn picture() -> String {
    card(
        &json!([{"contentType": ADAPTIVE, "content": {"type": "AdaptiveCard", "version": "1.4",
        "body": [{"type": "Image", "url": "https://example.invalid/graph.png", "altText": "a graph"}]}}]),
    )
}

fn connector(title: &str) -> String {
    card(
        &json!([{"contentType": CONNECTOR, "content": {"@type": "MessageCard", "themeColor": "FF0000",
        "title": title, "summary": title, "text": "The probe of **api** failed.",
        "sections": [{"activityTitle": "uptime-bot",
            "facts": [{"name": "endpoint", "value": "https://api.example.invalid/health"}]}],
        "potentialAction": [{"@type": "OpenUri", "name": "Status page",
            "targets": [{"os": "default", "uri": "https://status.example.invalid/api"}]}]}}]),
    )
}

/// The `properties.cards` of a link's app card, a JSON string of a list as the chat service gives it.
fn video_cards() -> Value {
    let open = json!({"type": "Action.Submit", "title": "Open", "data": {"msteams": {"type": "invoke"}}});
    let cards = json!([{"appId": VIDEO_APP, "appName": "Synthetic Video", "contentType": ADAPTIVE,
    "content": {"type": "AdaptiveCard", "version": "1.6", "body": [
        {"type": "Media", "sources": [{"mimeType": "video/mp4", "url": "https://video.example.invalid/v/1"}]},
        {"type": "TextBlock", "text": "Synthetic video title", "weight": "bolder", "size": "large"},
        {"type": "TextBlock", "text": "Synthetic channel", "isSubtle": true},
        {"type": "ActionSet", "actions": [open]},
    ]}}]);
    json!(cards.to_string())
}

/// The ids of the messages the mock holds.
struct Posted {
    alert: String,
    hero: String,
    thumbnail: String,
    picture: String,
    connector: String,
    video: String,
    plain: String,
}

fn post(mock: &ChatServiceMock) -> Posted {
    let card_in = |chat, content: &str| mock.add_message(chat, WORKFLOW, CARD_TYPE, content).unwrap();
    Posted {
        alert: card_in(CHANNEL, &alert()),
        hero: card_in(CHANNEL, &hero()),
        thumbnail: card_in(CHANNEL, &thumbnail()),
        picture: card_in(CHANNEL, &picture()),
        connector: card_in(AB_CHAT, &connector("Probe failed")),
        video: mock
            .add_message_with(
                AB_CHAT,
                WORKFLOW,
                "RichText/Html",
                "<p>watch <a href=\"https://video.example.invalid/v/1\">https://video.example.invalid/v/1</a></p>",
                |message| message["properties"]["cards"] = video_cards(),
            )
            .unwrap(),
        plain: mock
            .add_message(AB_CHAT, WORKFLOW, "RichText/Html", "<p>plain words, no card</p>")
            .unwrap(),
    }
}

/// Account a's engine on `mock`, its store at `store`, its push channel on `trouter` when given.
fn engine_on(mock: &ChatServiceMock, trouter: Option<&TrouterMock>, store: &Path) -> EngineHandle {
    let tokens = Arc::new(Tokens {
        token: mock.token(LAB_A).unwrap(),
        renewals: broadcast::channel(4).0,
    });
    let origin = mock.origin();
    let http = || {
        HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin)
            .unwrap()
            .for_account(Mri::new(LAB_A))
    };
    let transport = Arc::new(http());
    let push = trouter.map(|trouter| {
        let channel = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), trouter.origin())
            .unwrap()
            .with_options(Options {
                backoff_base: Duration::from_millis(50),
                backoff_cap: Duration::from_millis(50),
                backoff_jitter_percent: 0,
                ..Options::default()
            });
        Push::new(channel)
    });
    let handle = engine::start(Parts {
        calls: None,
        store: Store::open(store).unwrap(),
        client: Arc::new(Client::new(http())),
        outbox: Outbox::new(transport, Author::new(Mri::new(LAB_A), "bot-a")),
        push,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap();
    handle.requests().sync().blocking_wait().unwrap();
    handle
}

fn started(handle: EngineHandle) -> Started<MemorySecretStore> {
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

/// a's app on the mock, its push channel registered, serving its control socket.
struct Lab {
    mock: Arc<ChatServiceMock>,
    socket: PathBuf,
    cx: TestAppContext,
    _trouter: TrouterMock,
}

impl Lab {
    fn start(mock: Arc<ChatServiceMock>, dir: &Path) -> Self {
        let trouter = mock.start_trouter().unwrap();
        let handle = engine_on(&mock, Some(&trouter), &dir.join("a.sqlite"));
        let socket = dir.join("run/ngteams/ctl.sock");
        let cx = TestAppContext::single();
        let launch = Launch {
            first: started(handle),
            others: Vec::new(),
            settings: app::settings(SettingsFile::new(dir.join("settings.toml")), None),
            bus: Err(BusError::NotGiven),
            sign_in_again: Rc::new(|_| Rc::new(|_| {})),
            runner: Runner {
                setup: Setup::new(PersonalPaths::new(dir.join("config"), dir.join("data"))).unwrap(),
                floor: CallFloor::new(),
                devices: None,
            },
            read_only: false,
            trace: Arc::new(ViewTrace::off()),
            ctl: Some(socket.clone()),
        };
        assert!(cx.update(|cx| app::launch(cx, launch)).is_ok(), "the window opens");
        pump_until(&cx, Duration::ZERO, |_| {
            trouter
                .registrations()
                .iter()
                .any(|registration| registration.app_id == MESSAGING_APP)
        });
        Self {
            mock,
            socket,
            cx,
            _trouter: trouter,
        }
    }

    fn ctl(&self, args: &[&str]) -> Run {
        ngteamsctl(&self.cx, &self.socket, args)
    }

    /// `ngteamsctl <args>` while a workflow posts the connector card `title` in a's chat with b, again and again
    /// until the run ends: the run's subscription can start after the first post.
    fn ctl_while_posting(&self, title: &str, args: &[&str]) -> Run {
        let done = Arc::new(AtomicBool::new(false));
        let poster = {
            let (mock, done, content) = (Arc::clone(&self.mock), Arc::clone(&done), connector(title));
            std::thread::spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    let _ = mock.add_message(AB_CHAT, WORKFLOW, CARD_TYPE, &content);
                    std::thread::sleep(Duration::from_millis(300));
                }
            })
        };
        let run = self.ctl(args);
        done.store(true, Ordering::Relaxed);
        poster.join().unwrap();
        run
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        quit(&self.cx);
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

/// The row of message `id` in the `--json` answer of `messages` or `search`.
fn row<'a>(answer: &'a Value, id: &str) -> &'a Value {
    answer["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("no messages: {answer}"))
        .iter()
        .find(|row| row["id"] == id)
        .unwrap_or_else(|| panic!("no message {id}: {answer}"))
}

/// The line of the table that `ngteamsctl messages` printed for message `id`.
fn line<'a>(table: &'a str, id: &str) -> &'a str {
    table
        .lines()
        .find(|line| line.starts_with(id))
        .unwrap_or_else(|| panic!("no line of {id}:\n{table}"))
}

/// The message ids in the `--json` answer of `search`, in order.
fn ids(found: &Value) -> Vec<&str> {
    found["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("no messages: {found}"))
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect()
}

fn one_line(text: &str) -> String {
    text.replace('\n', " ↵ ")
}

#[test]
fn a_person_reads_the_cards_of_a_channel_as_json_and_as_a_table() {
    let runtime = runtime();
    let mock = Arc::new(runtime.block_on(ChatServiceMock::start()).unwrap());
    let posted = post(&mock);
    let dir = tempfile::tempdir().unwrap();
    let lab = Lab::start(mock, dir.path());

    let answer = lab.ctl(&["--json", "messages", CHANNEL]).json();
    for (id, text) in [
        (&posted.alert, ALERT_TEXT),
        (&posted.hero, HERO_TEXT),
        (&posted.thumbnail, THUMBNAIL_TEXT),
        (&posted.picture, "Sent a card"),
    ] {
        let row = row(&answer, id);
        assert_eq!(
            (row["text"].as_str(), &row["card"]),
            (Some(text), &json!(true)),
            "{row}"
        );
        assert_eq!(row["sender_name"], "Workflows", "{row}");
    }

    let table = lab.ctl(&["messages", CHANNEL]);
    assert_eq!(table.code, 0, "{table:?}");
    for (id, text) in [
        (&posted.alert, ALERT_TEXT),
        (&posted.hero, HERO_TEXT),
        (&posted.thumbnail, THUMBNAIL_TEXT),
        (&posted.picture, "Sent a card"),
    ] {
        let line = line(&table.stdout, id);
        assert!(line.contains(" Workflows "), "{line}");
        assert!(line.ends_with(&format!("(card) {}", one_line(text))), "{line}");
    }
}

#[test]
fn a_person_reads_a_chat_s_cards_and_its_text_message_is_as_before() {
    let runtime = runtime();
    let mock = Arc::new(runtime.block_on(ChatServiceMock::start()).unwrap());
    let posted = post(&mock);
    let dir = tempfile::tempdir().unwrap();
    let lab = Lab::start(mock, dir.path());

    let answer = lab.ctl(&["--json", "messages", AB_CHAT]).json();
    let connector = row(&answer, &posted.connector);
    assert_eq!(
        (connector["text"].as_str(), &connector["card"]),
        (Some(CONNECTOR_TEXT), &json!(true))
    );
    let video = row(&answer, &posted.video);
    assert_eq!(video["card"], json!(true), "{video}");
    let video_text = video["text"].as_str().unwrap();
    assert!(video_text.starts_with("watch "), "{video}");
    assert!(video_text.ends_with(&format!("\n{VIDEO_TEXT}")), "{video}");
    let plain = row(&answer, &posted.plain);
    assert_eq!(plain["text"], "plain words, no card", "{plain}");
    assert!(plain.get("card").is_none(), "a text message has no card field: {plain}");

    let table = lab.ctl(&["messages", AB_CHAT]);
    assert_eq!(table.code, 0, "{table:?}");
    assert!(line(&table.stdout, &posted.connector).ends_with(&format!("(card) {}", one_line(CONNECTOR_TEXT))));
    assert!(line(&table.stdout, &posted.video).ends_with(&format!(" ↵ {}", one_line(VIDEO_TEXT))));
    assert!(line(&table.stdout, &posted.video).contains("(card) watch "));
    let plain = line(&table.stdout, &posted.plain);
    assert!(
        plain.ends_with(" plain words, no card") && !plain.contains("(card)"),
        "{plain}"
    );
}

#[test]
fn a_person_finds_a_chat_s_card_by_its_text() {
    let runtime = runtime();
    let mock = Arc::new(runtime.block_on(ChatServiceMock::start()).unwrap());
    let posted = post(&mock);
    let dir = tempfile::tempdir().unwrap();
    let lab = Lab::start(mock, dir.path());

    for (query, id) in [
        ("api.example.invalid", &posted.connector),
        ("uptime-bot", &posted.connector),
        ("Synthetic video title", &posted.video),
    ] {
        let found = lab.ctl(&["--json", "search", query]).json();
        assert_eq!(ids(&found), [id.as_str()], "{query}: {found}");
        assert_eq!(row(&found, id)["card"], json!(true));
    }
    let table = lab.ctl(&["search", "uptime-bot"]);
    assert!(table.stdout.contains("(card) Probe failed ↵ "), "{table:?}");
}

#[test]
fn search_finds_a_card_that_the_previous_release_indexed_by_its_fallback() {
    let runtime = runtime();
    let mock = Arc::new(runtime.block_on(ChatServiceMock::start()).unwrap());
    let posted = post(&mock);
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("a.sqlite");
    engine_on(&mock, None, &store).shutdown();

    // the store as schema 29 left it: the cards indexed by the fallback text, a link's app card by its text alone
    let db = rusqlite::Connection::open(&store).unwrap();
    for (id, old) in [
        (
            &posted.connector,
            "Card - access it on https://go.skype.com/cards.unsupported.",
        ),
        (&posted.video, "watch https://video.example.invalid/v/1"),
    ] {
        let changed = db
            .execute(
                "UPDATE message_search SET text = ?1 WHERE rowid = \
                 (SELECT id FROM search_entries WHERE conversation_id = ?2 AND message_id = ?3)",
                (old, AB_CHAT, id),
            )
            .unwrap();
        assert_eq!(changed, 1, "{id} was indexed");
    }
    db.execute_batch("PRAGMA user_version = 29;").unwrap();
    let old_hits: i64 = db
        .query_row(
            "SELECT count(*) FROM message_search JOIN search_entries AS entry ON entry.id = message_search.rowid \
             WHERE message_search MATCH '\"uptime\"* OR \"Synthetic\"*' AND entry.conversation_id = ?1",
            [AB_CHAT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_hits, 0, "the old index does not hold the card text");
    drop(db);

    let lab = Lab::start(mock, dir.path());
    for (query, id) in [("uptime-bot", &posted.connector), ("Synthetic channel", &posted.video)] {
        let mut found = Value::Null;
        for _ in 0..50 {
            found = lab.ctl(&["--json", "search", query]).json();
            if !found["messages"].as_array().unwrap().is_empty() {
                break;
            }
            pump_until(&lab.cx, Duration::from_millis(100), |_| true);
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(ids(&found), [id.as_str()], "{query}: {found}");
    }
}

#[test]
fn wait_message_gives_the_text_of_a_new_card() {
    let runtime = runtime();
    let mock = Arc::new(runtime.block_on(ChatServiceMock::start()).unwrap());
    let dir = tempfile::tempdir().unwrap();
    let lab = Lab::start(mock, dir.path());
    let text = CONNECTOR_TEXT.replace("Probe failed", "Queue drained");

    let run = lab.ctl_while_posting(
        "Queue drained",
        &[
            "--json",
            "wait",
            "message",
            "--chat",
            AB_CHAT,
            "--match",
            "Queue drained",
            "--timeout",
            "20",
        ],
    );
    assert_eq!(run.code, 0, "{run:?}");
    let event: Value = serde_json::from_str(run.stdout.trim()).unwrap_or_else(|error| panic!("{error}: {run:?}"));
    assert_eq!(event["event"], "message", "{event}");
    assert_eq!(event["text"], text, "{event}");
    assert_eq!(event["sender_name"], "Workflows", "{event}");

    let run = lab.ctl_while_posting(
        "Queue drained",
        &[
            "wait",
            "message",
            "--chat",
            AB_CHAT,
            "--match",
            "Queue drained",
            "--timeout",
            "20",
        ],
    );
    assert_eq!(run.code, 0, "{run:?}");
    assert!(
        run.stdout.contains(&format!("Workflows: {}", one_line(&text))),
        "{run:?}"
    );
}
