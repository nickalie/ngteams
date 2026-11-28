//! A user of an own server talks to a bot in the app's window in the UI sandbox (`harness = false`: GPUI runs on the
//! process's main thread). It is the QA test of task 229.
//!
//! The parent runs the real `ngteams-server` binary (TLS, SQLite) with `[bots]` set to reach a bot on loopback, and a
//! test bot: an HTTPS endpoint on 127.0.0.1 whose certificate a made-up certificate authority signed. The server
//! process trusts only that authority (`SSL_CERT_FILE`, `SSL_CERT_DIR`). The operator registers the signed bot "Echo
//! (synthetic)" with the server's command line; the bot takes its token with the client-credentials grant and uses
//! the connector routes under its `serviceUrl`, as an SDK does.
//!
//! The bot starts its 1:1 chat with the user (`POST v3/conversations`, with a welcome). The user's app runs as a child
//! in the sandbox with a control socket; the parent uses it as a user does, with the sandbox's pointer and keyboard:
//! it clicks the bot's chat in the list, types a message and sends it with Enter. The bot gets the `message`, answers
//! in plain text, in Markdown (bold, a link, a script that must not show) and with an Adaptive Card; the window shows
//! all three under the bot's name. The user edits the message (the bot gets `messageUpdate`), the bot types (the
//! window's typing line) and deletes one of its messages (the window takes it away).
//!
//! Every name, id and text is made up. Screenshots are kept under `target/ui-shots/qa-bot-connector-*.png`, in Teams
//! Light and Dark. Without weston 15, niri, `dbus-daemon` or the built server binary it says so and passes, unless
//! `GPUI_SMOKE_REQUIRE=1`.

use std::ffi::OsStr;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::net::SocketAddr;
use std::os::fd::{AsFd as _, AsRawFd as _, RawFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, thread};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
use ngteams::descriptors::adopt_trace;
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};
use reqwest::Method;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::{Value, json};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_core::CallFloor;
use teams_proto::chat::Marks;
use teams_ui::chat_view::{Body, ChatView, MessageItem};
use teams_ui::rich_text::Block;
use teams_ui::shell::Shell;
use teams_ui::tray::{MENU_PATH, MENU_QUIT};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "../own_server_binary.rs"]
#[expect(dead_code, reason = "the shared binary harness has helpers other QA tests use")]
mod own_server_binary;

use own_server_binary::BinaryServer;
#[path = "../window.rs"]
#[expect(dead_code, reason = "the shared window harness has helpers other tests use")]
mod support;

const CHILD_ENV: &str = "NGTEAMS_QA_BOT_CONNECTOR_TEST";
const TRACE_FD_ENV: &str = "NGTEAMS_QA_BOT_CONNECTOR_TRACE_FD";
const ORIGIN_ENV: &str = "NGTEAMS_QA_BOT_CONNECTOR_ORIGIN";
const CA_ENV: &str = "NGTEAMS_QA_BOT_CONNECTOR_CA";
const DIR_ENV: &str = "NGTEAMS_QA_BOT_CONNECTOR_DIR";
/// The file in the app's directory the parent asks the child about its views through.
const ASK: &str = "qa-bot-connector-ask";
/// The file in the app's directory the child reads the theme to show from (`light` or `dark`).
const THEME_FILE: &str = "theme";
const STARTUP: Duration = Duration::from_secs(90);
const WAIT: Duration = Duration::from_secs(30);
/// How long an activity may take to reach the bot (a lost wake-up waits for the outbox's two-second poll).
const DELIVERY: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(100);
/// How long the screen gets to settle before a shot.
const SETTLE: Duration = Duration::from_millis(900);

/// Where the pointer clicks to put the caret in the open chat's message box.
const COMPOSE: (u32, u32) = (830, 757);
/// The first row under the "Chats" header with Favorites shown, the height of a row, and where a row is clicked.
const FIRST_CHAT_ROW: u32 = 256;
const ROW: u32 = 64;
const ROW_X: u32 = 250;

const USER_UPN: &str = "dana@own.example.test";
const USER_NAME: &str = "Dana Made-Up";
const BOT_NAME: &str = "Echo (synthetic)";

/// What the bot and the user write.
const WELCOME: &str = "Hello, I am Echo (synthetic). Write to me and I answer.";
const HELLO: &str = "Hello Echo, this is a made-up question";
const EDITED: &str = " (edited)";
const PLAIN: &str = "Echo in plain text: 2 < 3 & 4 > 1";
const MARKDOWN: &str =
    "**Bold echo** with [a made-up link](https://example.test/echo) <script>alert('made-up')</script>and the end";
const BOLD: &str = "Bold echo";
const LINK: &str = "https://example.test/echo";
const CARD_TEXT: &str = "Echo card (synthetic)";
const CARD_ACTION: &str = "Open the made-up page";
const CARD_URL: &str = "https://example.test/echo-card";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_bot_connector_window: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Notes a step on stderr with the time since the process started.
fn step(what: &str) {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    eprintln!("[{:>7.2}s] {what}", start.elapsed().as_secs_f64());
}

/// What the walk saw wrong that does not stop it: each is said at the end, and the test fails.
static FINDINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn note(finding: String) {
    step(&format!("FINDING: {finding}"));
    FINDINGS.lock().unwrap_or_else(PoisonError::into_inner).push(finding);
}

/// Notes `finding` unless `ok`.
fn expect(ok: bool, finding: impl FnOnce() -> String) {
    if !ok {
        note(finding());
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

// --- the parent ---

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams bot connector", &options)? else {
        return Ok(());
    };
    let Some(binary) = own_server_binary::binary()? else {
        if ui_sandbox::required() {
            return Err("no ngteams-server binary: cargo build -p ngteams-server first".into());
        }
        say("skipped: no ngteams-server binary (cargo build -p ngteams-server)");
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let dir = tempfile::tempdir()?;
    let bot = TestBot::start(&runtime, dir.path())?;
    let outcome = world(&sandbox, &runtime, &binary, &bot, dir.path());
    bot.stop();
    drop(runtime);
    if let Err(error) = outcome {
        return Err(format!("{error}{}", sandbox.logs()).into());
    }
    let findings = std::mem::take(&mut *FINDINGS.lock().unwrap_or_else(PoisonError::into_inner));
    if !findings.is_empty() {
        return Err(format!("the walk ended with findings:\n- {}", findings.join("\n- ")).into());
    }
    say("ngteams bot connector: ok");
    Ok(())
}

/// The server, its user and bot, the app; then the walk.
fn world(
    sandbox: &Sandbox,
    runtime: &tokio::runtime::Runtime,
    binary: &Path,
    bot: &TestBot,
    dir: &Path,
) -> SandboxResult {
    let server = start_server(binary, &dir.join("server"), &bot.ca_file)?;
    let user = server.add_user(USER_UPN, USER_NAME)?;
    let (bot_id, secret) = add_bot(&server, BOT_NAME, &bot.url)?;
    step(&format!(
        "own server {}; user {user}; bot 28:{bot_id} at {}",
        server.origin, bot.url
    ));
    let connector = Connector::sign_in(runtime, &server, &bot_id, &secret)?;
    let app_dir = dir.join("app");
    fs::create_dir_all(&app_dir)?;
    let env = vec![
        (ORIGIN_ENV, server.origin.clone()),
        (CA_ENV, server.ca_file.to_string_lossy().into_owned()),
    ];
    let mut run = AppRun::launch(sandbox, &env, &app_dir)?;
    let outcome = (|| -> SandboxResult {
        let mut walk = Walk {
            run: &mut run,
            sandbox,
            runtime,
            bot,
            connector: &connector,
            user: &user,
            bot_id: &bot_id,
            origin: &server.origin,
            server: &server,
            chat: String::new(),
            hello: String::new(),
            answers: Vec::new(),
        };
        walk.start_chat()?;
        walk.chat_in_list()?;
        walk.open_by_click()?;
        walk.send_hello()?;
        walk.answer()?;
        walk.see_answers()?;
        walk.read_while_shown()?;
        walk.edit()?;
        walk.bot_types()?;
        walk.bot_deletes()
    })();
    if let Err(error) = outcome {
        let _ = fs::write(dir.join("own-server.log"), server.log());
        step(&format!("the server's log:\n{}", tail(&server.log(), 60)));
        return Err(run.fail(sandbox, &*error));
    }
    run.quit(sandbox)
}

/// The last `lines` lines of `text`.
fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// What one step of the walk works with.
struct Walk<'a> {
    run: &'a mut AppRun,
    sandbox: &'a Sandbox,
    runtime: &'a tokio::runtime::Runtime,
    bot: &'a TestBot,
    connector: &'a Connector,
    /// The user's object id.
    user: &'a str,
    bot_id: &'a str,
    origin: &'a str,
    server: &'a BinaryServer,
    /// The bot chat's id, once the bot started it.
    chat: String,
    /// The id of the user's message to the bot.
    hello: String,
    /// The ids of the bot's plain, Markdown and card answers.
    answers: Vec<String>,
}

impl Walk<'_> {
    /// The shots of `name` in Teams Light, then Teams Dark, then back to Light.
    fn shots(&mut self, name: &str) -> SandboxResult {
        self.run
            .shot(self.sandbox, &format!("qa-bot-connector-{name}-light.png"))?;
        self.run.theme(true)?;
        thread::sleep(SETTLE);
        self.run
            .shot(self.sandbox, &format!("qa-bot-connector-{name}-dark.png"))?;
        self.run.theme(false)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn server_log(&self) -> String {
        self.server.log()
    }

    /// A call to the connector under the bot's `serviceUrl`.
    fn call(&self, method: Method, path: &str, body: Option<Value>) -> SandboxResult<(u16, Value)> {
        self.runtime
            .block_on(self.connector.call(method, path, body))
            .map_err(|error| format!("{path}: {error}").into())
    }

    /// The bot sends `activity` to its chat; the new message's id.
    fn bot_sends(&self, what: &str, activity: Value) -> SandboxResult<String> {
        let path = format!("v3/conversations/{}/activities", encode(&self.chat));
        let (status, answer) = self.call(Method::POST, &path, Some(activity))?;
        if status != 200 {
            return Err(format!("the bot's {what}: {status} {answer}").into());
        }
        Ok(answer["id"].as_str().unwrap_or_default().to_owned())
    }

    /// The first activity of `kind` the bot got after `from` others that `wanted` takes, within `wait`.
    fn activity(&self, from: usize, kind: &str, wait: Duration, wanted: impl Fn(&Value) -> bool) -> Option<Received> {
        wait_until(wait, || {
            self.bot
                .received()
                .into_iter()
                .skip(from)
                .find(|got| got.activity["type"] == kind && wanted(&got.activity))
        })
    }
}

// --- the bot starts its chat, the user opens it ---

impl Walk<'_> {
    /// The bot's `POST v3/conversations` with the user, with its welcome.
    fn start_chat(&mut self) -> SandboxResult {
        let create = json!({
            "bot": {"id": format!("28:{}", self.bot_id), "name": BOT_NAME},
            "members": [{"id": format!("29:{}", self.user)}],
            "isGroup": false,
            "activity": {"type": "message", "text": WELCOME},
        });
        let (status, answer) = self.call(Method::POST, "v3/conversations", Some(create))?;
        if status != 201 {
            return Err(format!("the bot's POST v3/conversations: {status} {answer}").into());
        }
        answer["id"]
            .as_str()
            .ok_or("the new conversation has no id")?
            .clone_into(&mut self.chat);
        expect(answer["activityId"].is_string(), || {
            format!("the new conversation's answer has no activityId: {answer}")
        });
        let service = format!("{}/bot/", self.origin);
        expect(answer["serviceUrl"] == service.as_str(), || {
            format!(
                "the new conversation's serviceUrl is {}, expected {service}",
                answer["serviceUrl"]
            )
        });
        step(&format!("the bot started the chat {}", self.chat));
        Ok(())
    }

    /// The chat appears in the user's list, named by the bot.
    fn chat_in_list(&mut self) -> SandboxResult {
        let chat = self.chat.clone();
        let rows = self
            .run
            .until_list("the bot chat in the list", |state| row(state, &chat).is_some())?;
        let found = row(&rows, &chat).cloned().unwrap_or_default();
        step(&format!("the list's row of the bot chat: {found}"));
        expect(found["name"] == BOT_NAME, || {
            format!(
                "the bot chat is named {} in the list, expected {BOT_NAME}",
                found["name"]
            )
        });
        expect(found["can_call"] == false, || "the bot chat offers a call".to_owned());
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, "qa-bot-connector-list-light.png")?;
        Ok(())
    }

    /// A click on the chat's row opens it with the bot's welcome.
    fn open_by_click(&mut self) -> SandboxResult {
        let list = self.run.state_in("list")?;
        let before = self.run.state_in("chat")?;
        step(&format!("before the click the open chat is {}", before["conversation"]));
        let at = list["shown"]
            .as_array()
            .and_then(|shown| shown.iter().position(|id| *id == self.chat.as_str()))
            .ok_or_else(|| format!("the bot chat is not among the rows shown: {}", list["shown"]))?;
        let favorites = u32::try_from(list["favorites"].as_u64().unwrap_or(0))?;
        let first = if favorites == 0 {
            FIRST_CHAT_ROW - ROW - 32
        } else {
            FIRST_CHAT_ROW + (favorites - 1) * ROW
        };
        self.run.click(ROW_X, first + u32::try_from(at)? * ROW)?;
        let chat = self.chat.clone();
        let state = self.run.until_chat("the bot chat opened with its welcome", |state| {
            state["conversation"] == chat.as_str() && item_with(state, WELCOME).is_some()
        })?;
        let welcome = item_with(&state, WELCOME).cloned().unwrap_or_default();
        expect_bot_item(&welcome, "the welcome");
        thread::sleep(SETTLE);
        step("the user opened the bot chat with a click; the welcome shows");
        Ok(())
    }

    /// The user types a message and sends it with Enter; the bot gets it.
    fn send_hello(&mut self) -> SandboxResult {
        let from = self.bot.received().len();
        self.run.click(COMPOSE.0, COMPOSE.1)?;
        self.run.input.type_text(HELLO)?;
        thread::sleep(Duration::from_millis(400));
        self.run.input.key(keys::ENTER)?;
        let got = self
            .activity(from, "message", DELIVERY, |activity| activity["text"] == HELLO)
            .ok_or_else(|| {
                format!(
                    "no message activity with {HELLO:?} reached the bot: {:?}",
                    self.kinds(from)
                )
            })?;
        self.check_message(&got);
        got.activity["id"]
            .as_str()
            .unwrap_or_default()
            .clone_into(&mut self.hello);
        let typed = self.activity(from, "typing", Duration::from_secs(1), |_| true);
        expect(typed.is_some(), || {
            format!(
                "no typing activity reached the bot while the user typed: {:?}",
                self.kinds(from)
            )
        });
        step("the bot got the user's message");
        Ok(())
    }

    /// The kinds of the activities the bot got after `from` others.
    fn kinds(&self, from: usize) -> Vec<String> {
        self.bot
            .received()
            .into_iter()
            .skip(from)
            .map(|got| got.activity["type"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    /// Notes what is wrong with the user's `message` activity as the bot got it.
    fn check_message(&self, got: &Received) {
        let activity = &got.activity;
        let person = format!("29:{}", self.user);
        let bot = format!("28:{}", self.bot_id);
        let service = format!("{}/bot/", self.origin);
        for (field, value, wanted) in [
            ("from.id", &activity["from"]["id"], person.as_str()),
            ("from.name", &activity["from"]["name"], USER_NAME),
            ("recipient.id", &activity["recipient"]["id"], bot.as_str()),
            ("conversation.id", &activity["conversation"]["id"], self.chat.as_str()),
            (
                "conversation.conversationType",
                &activity["conversation"]["conversationType"],
                "personal",
            ),
            ("channelId", &activity["channelId"], "msteams"),
            ("serviceUrl", &activity["serviceUrl"], service.as_str()),
        ] {
            expect(*value == wanted, || {
                format!("the bot's message activity has {field} {value}, expected {wanted}")
            });
        }
        let Some(token) = got
            .authorization
            .as_deref()
            .and_then(|value| value.strip_prefix("Bearer "))
        else {
            note("the bot's message activity carries no Bearer token".to_owned());
            return;
        };
        let claims = claims(token);
        expect(claims["aud"] == self.bot_id, || {
            format!(
                "the activity's token is for {}, expected the bot {}",
                claims["aud"], self.bot_id
            )
        });
        let issuer = format!("{}/bot", self.origin);
        expect(claims["iss"] == issuer.as_str(), || {
            format!("the activity's token is from {}, expected {issuer}", claims["iss"])
        });
        step(&format!(
            "the activity's token: aud {} iss {} exp {}",
            claims["aud"], claims["iss"], claims["exp"]
        ));
    }
}

// --- the bot answers ---

impl Walk<'_> {
    /// The bot's three answers: plain text (a reply to the message), Markdown, an Adaptive Card.
    fn answer(&mut self) -> SandboxResult {
        let path = format!("v3/conversations/{}/activities/{}", encode(&self.chat), self.hello);
        let plain = json!({"type": "message", "textFormat": "plain", "text": PLAIN, "replyToId": self.hello});
        let (status, answer) = self.call(Method::POST, &path, Some(plain))?;
        if status != 200 {
            return Err(format!("the bot's plain reply: {status} {answer}").into());
        }
        self.answers.push(answer["id"].as_str().unwrap_or_default().to_owned());
        let markdown = self.bot_sends("Markdown", json!({"type": "message", "text": MARKDOWN}))?;
        self.answers.push(markdown);
        let card = json!({
            "type": "message",
            "attachments": [{
                "contentType": "application/vnd.microsoft.card.adaptive",
                "content": {
                    "type": "AdaptiveCard",
                    "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                    "version": "1.4",
                    "body": [{"type": "TextBlock", "text": CARD_TEXT, "wrap": true, "weight": "Bolder"}],
                    "actions": [{"type": "Action.OpenUrl", "title": CARD_ACTION, "url": CARD_URL}],
                },
            }],
        });
        let card = self.bot_sends("card", card)?;
        self.answers.push(card);
        step(&format!("the bot answered: {:?}", self.answers));
        Ok(())
    }

    /// The window shows the three answers under the bot's name.
    fn see_answers(&mut self) -> SandboxResult {
        let state = match self.run.until_chat("the bot's three answers", |state| {
            item_with(state, "Echo in plain text").is_some()
                && item_with(state, "Bold echo").is_some()
                && items(state).any(|item| item["kind"] == "cards")
        }) {
            Ok(state) => state,
            Err(error) => {
                note(format!("the window does not show all three answers: {error}"));
                self.run.state_in("chat")?
            }
        };
        if let Some(plain) = item_with(&state, "Echo in plain text") {
            expect_bot_item(plain, "the plain answer");
            expect(plain["text"] == PLAIN, || {
                format!("the plain answer shows {}, expected {PLAIN:?}", plain["text"])
            });
        }
        if let Some(markdown) = item_with(&state, "Bold echo") {
            check_markdown(markdown);
        }
        match items(&state).find(|item| item["kind"] == "cards") {
            Some(card) => check_card(card),
            None => note("the bot's card does not show as a card".to_owned()),
        }
        let own = item_with(&state, HELLO).cloned().unwrap_or_default();
        expect(own["own"] == true, || {
            format!("the user's message is not shown as theirs: {own}")
        });
        thread::sleep(SETTLE);
        self.shots("chat")?;
        step("the window shows the bot's plain, Markdown and card answers");
        Ok(())
    }
}

/// Notes what is wrong with `item` as a message of the bot, named `what`.
fn expect_bot_item(item: &Value, what: &str) {
    expect(item["own"] == false && item["sender_name"] == BOT_NAME, || {
        format!(
            "{what} shows as {}'s (own {}), expected {BOT_NAME}'s",
            item["sender_name"], item["own"]
        )
    });
}

/// Notes what is wrong with the Markdown answer: its bold, its link, no script.
fn check_markdown(item: &Value) {
    expect_bot_item(item, "the Markdown answer");
    let text = item["text"].as_str().unwrap_or_default();
    expect(!text.contains("script") && !text.contains("alert"), || {
        format!("the Markdown answer shows its script: {text:?}")
    });
    expect(text.contains("and the end") && text.contains("a made-up link"), || {
        format!("the Markdown answer lost text: {text:?}")
    });
    expect(!text.contains("**"), || {
        format!("the Markdown answer shows its marks: {text:?}")
    });
    let bold = item["bold"].as_array().cloned().unwrap_or_default();
    expect(bold.iter().any(|piece| piece == BOLD), || {
        format!("the Markdown answer's bold pieces are {bold:?}, expected {BOLD:?}")
    });
    let links = item["links"].as_array().cloned().unwrap_or_default();
    expect(links.iter().any(|href| href == LINK), || {
        format!("the Markdown answer's links are {links:?}, expected {LINK}")
    });
}

/// Notes what is wrong with the card answer: its text block and its Action.OpenUrl.
fn check_card(item: &Value) {
    expect_bot_item(item, "the card");
    let card = item["card"].as_str().unwrap_or_default();
    for wanted in [CARD_TEXT, CARD_ACTION, CARD_URL] {
        expect(card.contains(wanted), || format!("the card has no {wanted:?}: {card}"));
    }
    expect(!item["unshown"].as_bool().unwrap_or(true), || {
        format!("the card holds a part the app can't show: {card}")
    });
}

// --- read, an edit, typing, a delete ---

impl Walk<'_> {
    /// The chat the user reads in the active window is read: its row's unread count goes to 0, as in Teams.
    fn read_while_shown(&mut self) -> SandboxResult {
        let chat = self.chat.clone();
        let read = self.run.until_list("the open chat read", |state| {
            row(state, &chat).is_some_and(|found| found["unread"] == 0)
        });
        if let Err(error) = read {
            let open = self.run.state_in("chat")?;
            step(&format!("the window is active: {}", open["active"]));
            for line in self.server_log().lines().filter(|line| line.contains("properties")) {
                step(&format!("server: {line}"));
            }
            let state = self.run.state_in("list")?;
            let found = row(&state, &chat).cloned().unwrap_or_default();
            note(format!(
                "the bot chat the user has open and wrote in keeps {} unread in the list: {error}",
                found["unread"]
            ));
            self.run.shot(self.sandbox, "qa-bot-connector-unread-light.png")?;
        }
        Ok(())
    }

    /// The user edits the message (Up in the empty box): the bot gets `messageUpdate`.
    fn edit(&mut self) -> SandboxResult {
        let from = self.bot.received().len();
        self.run.click(COMPOSE.0, COMPOSE.1)?;
        self.run.input.key(keys::UP)?;
        thread::sleep(Duration::from_millis(500));
        self.run.input.key(keys::END)?;
        self.run.input.type_text(EDITED)?;
        thread::sleep(Duration::from_millis(400));
        self.run.input.key(keys::ENTER)?;
        let edited = format!("{HELLO}{EDITED}");
        let got = self.activity(from, "messageUpdate", DELIVERY, |_| true);
        match got {
            Some(got) => {
                let activity = &got.activity;
                expect(
                    activity["text"] == edited.as_str() && activity["id"] == self.hello.as_str(),
                    || {
                        format!(
                            "the bot's messageUpdate has text {} and id {}, expected {edited:?} and {}",
                            activity["text"], activity["id"], self.hello
                        )
                    },
                );
                expect(activity["channelData"]["eventType"] == "editMessage", || {
                    format!(
                        "the messageUpdate's eventType is {}",
                        activity["channelData"]["eventType"]
                    )
                });
            }
            None => note(format!("no messageUpdate reached the bot: {:?}", self.kinds(from))),
        }
        let shown = self
            .run
            .until_chat("the edit shown", |state| item_with(state, &edited).is_some());
        expect(shown.is_ok(), || {
            "the window does not show the edited message".to_owned()
        });
        step("the user edited the message; the bot got messageUpdate");
        Ok(())
    }

    /// The bot types: the window shows its typing line.
    fn bot_types(&mut self) -> SandboxResult {
        self.bot_sends("typing", json!({"type": "typing"}))?;
        match self
            .run
            .until_chat("the bot's typing line", |state| state["typing"].is_string())
        {
            Ok(state) => {
                let wanted = format!("{BOT_NAME} is typing…");
                expect(state["typing"] == wanted.as_str(), || {
                    format!("the bot's typing line says {}, expected {wanted}", state["typing"])
                });
                self.run.shot(self.sandbox, "qa-bot-connector-typing-light.png")?;
                step("the bot's typing line shows");
            }
            Err(error) => note(format!("no typing line when the bot types: {error}")),
        }
        Ok(())
    }

    /// The bot deletes its plain answer: the window takes it away.
    fn bot_deletes(&mut self) -> SandboxResult {
        let id = self.answers.first().cloned().unwrap_or_default();
        let path = format!("v3/conversations/{}/activities/{id}", encode(&self.chat));
        let (status, answer) = self.call(Method::DELETE, &path, None)?;
        if status != 200 {
            note(format!("the bot's delete: {status} {answer}"));
            return Ok(());
        }
        let gone = self.run.until_chat("the plain answer removed", |state| {
            item_with(state, "Echo in plain text").is_none()
        });
        match gone {
            Ok(state) => {
                let left = items(&state).find(|item| item["id"] == id.as_str());
                step(&format!("the deleted answer's row: {left:?}"));
            }
            Err(error) => note(format!("the window still shows the deleted answer: {error}")),
        }
        thread::sleep(SETTLE);
        self.shots("deleted")?;
        step("the bot deleted its plain answer; the window took it away");
        Ok(())
    }
}

/// The rows of a chat's `state`.
fn items(state: &Value) -> impl Iterator<Item = &Value> {
    state["items"].as_array().into_iter().flatten()
}

/// The first row of the chat's `state` whose text holds `text`.
fn item_with<'a>(state: &'a Value, text: &str) -> Option<&'a Value> {
    items(state).find(|item| item["text"].as_str().is_some_and(|shown| shown.contains(text)))
}

/// The list's row of `chat`.
fn row<'a>(state: &'a Value, chat: &str) -> Option<&'a Value> {
    state["rows"].as_array()?.iter().find(|row| row["id"] == chat)
}

/// A JWT's claims, unverified (`Null` when it can't be read).
fn claims(token: &str) -> Value {
    token
        .split('.')
        .nth(1)
        .and_then(|payload| URL_SAFE_NO_PAD.decode(payload).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

/// A conversation id as an SDK puts it in a path.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

// --- the test bot ---

/// One activity as the bot got it, with its `Authorization`.
#[derive(Clone, Debug)]
struct Received {
    activity: Value,
    authorization: Option<String>,
}

type Inbox = Arc<Mutex<Vec<Received>>>;

/// The bot's HTTPS endpoint on 127.0.0.1, with the made-up authority that signed its certificate.
struct TestBot {
    url: String,
    ca_file: PathBuf,
    inbox: Inbox,
    handle: axum_server::Handle<SocketAddr>,
}

impl TestBot {
    /// Writes the authority's PEM to `dir` and serves `/api/messages` on `runtime`.
    fn start(runtime: &tokio::runtime::Runtime, dir: &Path) -> SandboxResult<Self> {
        let (ca_pem, cert, key) = bot_certificates()?;
        let ca_file = dir.join("bot-ca.pem");
        fs::write(&ca_file, ca_pem)?;
        let mut config =
            rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
                .with_safe_default_protocol_versions()?
                .with_no_client_auth()
                .with_single_cert(vec![cert], key)?;
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let url = format!("https://{}/api/messages", listener.local_addr()?);
        let inbox: Inbox = Arc::default();
        let router = Router::new()
            .route("/api/messages", post(receive))
            .with_state(Arc::clone(&inbox));
        let handle = axum_server::Handle::new();
        let _entered = runtime.enter();
        let server = axum_server::from_tcp_rustls(
            listener,
            axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(config)),
        )?
        .handle(handle.clone());
        runtime.spawn(server.serve(router.into_make_service()));
        Ok(Self {
            url,
            ca_file,
            inbox,
            handle,
        })
    }

    /// Every activity it got so far, the oldest first.
    fn received(&self) -> Vec<Received> {
        self.inbox.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn stop(&self) {
        self.handle.shutdown();
    }
}

async fn receive(State(inbox): State<Inbox>, headers: HeaderMap, body: String) -> StatusCode {
    let activity = serde_json::from_str(&body).unwrap_or(Value::Null);
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    step(&format!(
        "the bot got {} (a token: {})",
        activity["type"],
        authorization.is_some()
    ));
    inbox.lock().unwrap_or_else(PoisonError::into_inner).push(Received {
        activity,
        authorization,
    });
    StatusCode::OK
}

/// A made-up authority's PEM, and the bot's certificate for 127.0.0.1 it signed with its key.
fn bot_certificates() -> SandboxResult<(String, CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let ca_key = KeyPair::generate()?;
    let mut ca = CertificateParams::new(Vec::new())?;
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.distinguished_name
        .push(DnType::CommonName, "Made-up bot certificate authority");
    let ca_cert = ca.self_signed(&ca_key)?;
    let key = KeyPair::generate()?;
    let leaf = CertificateParams::new(vec!["127.0.0.1".to_owned()])?;
    let cert = leaf.signed_by(&key, &Issuer::new(ca, ca_key))?;
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    Ok((ca_cert.pem(), cert.der().clone(), key))
}

/// The bot's side of the Bot Connector API: its `serviceUrl` and its own token.
struct Connector {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Connector {
    /// The bot's token from the server's client-credentials grant, as MSAL asks for it.
    fn sign_in(
        runtime: &tokio::runtime::Runtime,
        server: &BinaryServer,
        bot: &str,
        secret: &str,
    ) -> SandboxResult<Self> {
        let roots = reqwest::Certificate::from_pem_bundle(&fs::read(&server.ca_file)?)?;
        let http = reqwest::Client::builder().no_proxy().tls_certs_merge(roots).build()?;
        let form = format!(
            "grant_type=client_credentials&client_id={bot}&client_secret={}&scope={}",
            encode(secret),
            encode("https://api.botframework.com/.default"),
        );
        let url = format!("{}/botframework.com/oauth2/v2.0/token", server.origin);
        let (status, text) = runtime.block_on(async {
            let answer = http
                .post(&url)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(form)
                .send()
                .await?;
            Ok::<_, reqwest::Error>((answer.status().as_u16(), answer.text().await?))
        })?;
        let answer: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let token = answer["access_token"].as_str().filter(|_| status == 200);
        let Some(token) = token else {
            return Err(format!("the bot's token request: {status} {}", answer["error"]).into());
        };
        let claims = claims(token);
        step(&format!("the bot's token: aud {} exp {}", claims["aud"], claims["exp"]));
        Ok(Self {
            http,
            base: format!("{}/bot/", server.origin),
            token: token.to_owned(),
        })
    }

    /// A call under the `serviceUrl`; its status and JSON (null without one).
    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<(u16, Value), reqwest::Error> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(body.to_string());
        }
        let answer = request.send().await?;
        let status = answer.status().as_u16();
        let text = answer.text().await?;
        Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
    }
}

// --- the own server ---

/// The real `ngteams-server` binary on loopback, its `[bots]` reaching loopback and its process trusting only the
/// bot's authority; its log and database stay in `dir`.
fn start_server(binary: &Path, dir: &Path, bot_ca: &Path) -> SandboxResult<BinaryServer> {
    // the server reaches the bot through the system verifier, which reads only these when they are set
    let no_roots = dir.join("no-roots");
    fs::create_dir_all(&no_roots)?;
    BinaryServer::start_in(
        binary,
        Some(dir),
        "\n[bots]\nallowed_networks = [\"127.0.0.0/8\"]\n",
        &[
            ("SSL_CERT_FILE", bot_ca.as_os_str()),
            ("SSL_CERT_DIR", no_roots.as_os_str()),
            ("NGTEAMS_SERVER_LOG", OsStr::new("info,tower_http=debug")),
        ],
    )
}

/// `app add --name <name> --endpoint <url>`, as the operator registers a signed bot; its id and secret.
fn add_bot(server: &BinaryServer, name: &str, endpoint: &str) -> SandboxResult<(String, String)> {
    let said = server.run(
        &[
            "app",
            "add",
            "--name",
            name,
            "--endpoint",
            endpoint,
            "--description",
            "A made-up bot that answers",
        ],
        None,
    )?;
    let id = said
        .lines()
        .find_map(|line| line.strip_prefix("added "))
        .and_then(|rest| rest.split_whitespace().next())
        .ok_or("app add printed no id")?;
    let secret = said
        .lines()
        .find_map(|line| line.strip_prefix("secret "))
        .ok_or("app add printed no secret")?;
    Ok((id.to_owned(), secret.trim().to_owned()))
}

// --- the app in the sandbox ---

/// The app running in the sandbox: its process, its control socket and the user's pointer.
struct AppRun {
    running: Running,
    dir: PathBuf,
    input: VirtualInput,
    /// Where the pointer rests between clicks: on the empty title bar.
    rest: (u32, u32),
    asked: u32,
}

impl AppRun {
    /// Starts the app with `env` and its store and settings in `dir`; once its window is drawn.
    fn launch(sandbox: &Sandbox, env: &[(&'static str, String)], dir: &Path) -> SandboxResult<Self> {
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let dir_text = dir.to_string_lossy().into_owned();
        let mut vars: Vec<(&str, &str)> = vec![(CHILD_ENV, "app"), (TRACE_FD_ENV, &fd), (DIR_ENV, &dir_text)];
        vars.extend(env.iter().map(|(name, value)| (*name, value.as_str())));
        let child = sandbox.spawn_with_fds(&std::env::current_exe()?, &[], &vars, &[writer.as_fd()])?;
        drop(writer);
        // the view trace is drained so that the app never waits on a full pipe
        thread::spawn(move || BufReader::new(reader).lines().map_while(Result::ok).for_each(drop));
        let mut running = Running::new(child)?;
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
        let mut run = Self {
            running,
            dir: dir.to_owned(),
            input: sandbox.input()?,
            rest: (200, 24),
            asked: 0,
        };
        run.input.move_to(run.rest.0, run.rest.1)?;
        Ok(run)
    }

    /// Asks the child `verb`; its answer.
    fn question(&mut self, verb: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = self.dir.join(format!("{ASK}.staged"));
        fs::write(&staged, format!("{} {verb}", self.asked))?;
        fs::rename(&staged, self.dir.join(ASK))?;
        let prefix = format!("re {} ", self.asked);
        let line = self
            .running
            .line_where(WAIT, |line| line.starts_with(&prefix))
            .map_err(|error| format!("no answer to {verb}: {error:?}"))?;
        Ok(line[prefix.len()..].to_owned())
    }

    fn state_in(&mut self, view: &str) -> SandboxResult<Value> {
        let answer = self.question(view)?;
        serde_json::from_str(&answer).map_err(|error| format!("the state {answer}: {error}").into())
    }

    /// Waits up to [`WAIT`] for a state of `view` that `wanted` takes, named `what` in the error.
    fn until_in(&mut self, view: &str, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let start = Instant::now();
        loop {
            let state = self.state_in(view)?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {state}").into());
            }
            thread::sleep(POLL);
        }
    }

    fn until_chat(&mut self, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        self.until_in("chat", what, wanted)
    }

    /// The list's state once `wanted` takes it.
    fn until_list(&mut self, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        self.until_in("list", what, wanted)
    }

    /// Shows the app in Teams Light or Dark (`dark`), as the user picks it in Settings; once it is applied.
    fn theme(&mut self, dark: bool) -> SandboxResult {
        let key = if dark { "dark" } else { "light" };
        fs::write(self.dir.join(THEME_FILE), key)?;
        let applied = format!("theme {key}");
        self.running
            .line_where(WAIT, |line| line == applied)
            .map_err(|error| format!("{applied} never applied: {error:?}"))?;
        Ok(())
    }

    /// A screenshot of the screen, kept as `target/ui-shots/<name>`; the sandbox's copy.
    fn shot(&mut self, sandbox: &Sandbox, name: &str) -> SandboxResult<PathBuf> {
        self.input.move_to(self.rest.0, self.rest.1)?;
        thread::sleep(Duration::from_millis(600));
        let png = sandbox.screenshot(name, false)?;
        let kept = shots_dir()?.join(name);
        fs::copy(&png, &kept)?;
        step(&format!("shot {}", kept.display()));
        Ok(png)
    }

    /// Clicks (`x`, `y`) as a user does.
    fn click(&mut self, x: u32, y: u32) -> SandboxResult {
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(Duration::from_millis(250));
        Ok(())
    }

    /// The error, with the last screen and the app's stderr.
    fn fail(&mut self, sandbox: &Sandbox, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
        let shot = self
            .shot(sandbox, "qa-bot-connector-failed.png")
            .map_or_else(|_| String::new(), |path| path.display().to_string());
        self.running.fail(&format!("{error} (last screen: {shot})"))
    }

    /// Quits through the tray menu, as a user does; once the app ended. Without a tray item the app is stopped.
    fn quit(mut self, sandbox: &Sandbox) -> SandboxResult {
        let name = format!("org.kde.StatusNotifierItem-{}-1", self.running.pid());
        let host = sandbox.bus().connect()?;
        let menu = zbus::blocking::Proxy::new(&host, name.as_str(), MENU_PATH, "com.canonical.dbusmenu")?;
        let quit = || menu.call_method("Event", &(MENU_QUIT, "clicked", zbus::zvariant::Value::from(""), 0_u32));
        if wait_until(Duration::from_secs(10), || quit().ok()).is_none() {
            step("no tray item to quit through: the app is stopped");
            let _ = self.running.fail(&"stopped by the walk");
            return Ok(());
        }
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("the app exited {status}: {stderr}").into());
        }
        Ok(())
    }
}

/// Where the screenshots are kept: `target/ui-shots/`, never committed.
fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

// --- the child ---

/// The own server's account, signed in with its password: the app's first account.
/// The account, the runner, and the runtime that signed it in (kept while the app runs).
type OwnStart = (
    ngteams::accounts::Started<MemorySecretStore>,
    ngteams::accounts::Runner,
    tokio::runtime::Runtime,
);

fn own_account(dir: &Path) -> SandboxResult<OwnStart> {
    let paths = ngteams::accounts_file::PersonalPaths::new(dir.join("config"), dir.join("data"));
    let setup = ngteams::setup::Setup::new(paths)?;
    let server = ngteams::accounts_file::OwnServer::parse(&std::env::var(ORIGIN_ENV)?, &std::env::var(CA_ENV)?)?;
    let store = MemorySecretStore::new();
    let runtime = ngteams::accounts::runtime()?;
    let account = runtime.block_on(ngteams::signin::add_own(
        &setup,
        USER_UPN,
        &server,
        &Secret::new(own_server_binary::PASSWORD),
        &store,
    ))?;
    let engine = ngteams::engine::start(&setup.paths, &account, CallFloor::new(), store, true, None)?;
    let first = ngteams::accounts::Started {
        account,
        engine,
        calls: None,
    };
    Ok((first, support::runner(dir)?, runtime))
}

/// Runs `done` once `frames` more frames of `window` have been presented.
fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

/// The settings as the Appearance page leaves them once theme `key` is picked.
fn show_theme(settings: &mut teams_ui::Settings, key: &'static str) {
    settings.appearance.follow_system = false;
    settings.appearance.theme = key;
}

/// Shows the theme `file` names (`light` or `dark`) once it changes, and says `theme <name>` once it is applied.
fn follow_theme_file(settings: &Entity<teams_ui::SettingsModel>, file: PathBuf, cx: &mut App) {
    let settings = settings.downgrade();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut applied = String::new();
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(wanted) = fs::read_to_string(&file) else {
                continue;
            };
            if wanted == applied {
                continue;
            }
            let key = if wanted == "dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            let changed =
                cx.update(|cx| settings.update(cx, |model, cx| model.change(|chosen| show_theme(chosen, key), cx)));
            if changed.is_err() {
                return;
            }
            cx.background_executor().timer(Duration::from_millis(300)).await;
            say(&format!("theme {wanted}"));
            applied = wanted;
        }
    })
    .detach();
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let fd: RawFd = std::env::var(TRACE_FD_ENV)?.parse()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(fd)?);
    let dir = PathBuf::from(std::env::var(DIR_ENV)?);
    let (first, runner, runtime) = own_account(&dir)?;
    let settings = teams_ui::SettingsFile::new(dir.join("ui").join("a.toml"));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let ctl = dir.join("ctl.sock");
    let theme_file = dir.join(THEME_FILE);
    let ask = dir.join(ASK);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: Vec::new(),
                trace: Arc::new(trace),
                ctl: Some(ctl),
                settings: teams_ui::SettingsModel::load(settings),
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(launched) => {
                let settings = launched.desktop.read(cx).settings().clone();
                follow_theme_file(&settings, theme_file.clone(), cx);
                let window = launched.window;
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, 3, |_, _| say("ready"));
                });
                answer(window, ask.clone(), cx);
            }
            Err(failed) => {
                *failed_in_app.lock().unwrap_or_else(PoisonError::into_inner) = Some(failed.error);
                app::quit_after(failed.stopped, cx);
            }
        }
    });
    if let Some(error) = failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
        return Err(error.into());
    }
    drop(runtime);
    Ok(())
}

/// Answers the parent's questions as they come.
fn answer(window: WindowHandle<KitRoot>, ask: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(10)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let (seq, verb) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            let reply = window
                .update(cx, |kit, window, cx| {
                    let shell = kit
                        .view()
                        .clone()
                        .downcast::<Root>()
                        .ok()?
                        .read(cx)
                        .shell_in(cx)
                        .cloned()?;
                    Some(on_shell(&shell, verb, window.is_window_active(), cx))
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| "no shell".to_owned());
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn on_shell(shell: &Entity<Shell>, verb: &str, active: bool, cx: &App) -> String {
    match verb {
        "chat" => shell.read(cx).chat().map_or_else(
            || json!({"conversation": null, "items": [], "active": active}),
            |view| state_of(view.read(cx), active, cx),
        ),
        "list" => list_of(shell, cx),
        other => json!(format!("unknown {other}")),
    }
    .to_string()
}

/// The list's rows (each chat's id, name, bot, preview and whether it offers a call), the ids of the rows shown in
/// their drawn order, and how many favourites are over them.
fn list_of(shell: &Entity<Shell>, cx: &App) -> Value {
    let list = shell.read(cx).list().read(cx);
    let rows: Vec<Value> = list
        .rows()
        .iter()
        .map(|row| {
            json!({
                "id": row.id,
                "name": row.name.as_ref(),
                "kind": format!("{:?}", row.kind).to_lowercase(),
                "bot": row.bot.as_ref().map(|bot| bot.mri.clone()),
                "preview": row.preview.as_ref(),
                "can_call": list.can_call(&row.id),
                "unread": row.unread,
            })
        })
        .collect();
    let shown: Vec<&str> = list
        .shown_rows()
        .into_iter()
        .filter_map(|index| list.rows().get(index))
        .map(|row| row.id.as_str())
        .collect();
    json!({ "rows": rows, "shown": shown, "favorites": list.favorites().len() })
}

/// The view's rows, its bot, its typing line, and whether its window is the active one.
fn state_of(view: &ChatView, active: bool, cx: &App) -> Value {
    let items: Vec<Value> = view.message_items().map(item_of).collect();
    json!({
        "conversation": view.conversation(),
        "bot": view.chat_bot(),
        "items": items,
        "typing": view.typing_text(cx),
        "active": active,
    })
}

fn item_of(item: &MessageItem) -> Value {
    let (kind, blocks, card, unshown) = match &item.body {
        Body::Text(rendered) => ("text", rendered.0.blocks.as_slice(), String::new(), false),
        Body::Cards(cards) => ("cards", &[][..], format!("{:?}", cards.0), !cards.0.drawn()),
        Body::Deleted => ("deleted", &[][..], String::new(), false),
        _ => ("other", &[][..], String::new(), false),
    };
    let text = blocks
        .iter()
        .flat_map(Block::plain_lines)
        .collect::<Vec<_>>()
        .join("\n");
    let paragraphs = blocks.iter().filter_map(|block| match block {
        Block::Text(text) => Some(text),
        _ => None,
    });
    let mut bold = Vec::new();
    let mut links = Vec::new();
    for paragraph in paragraphs {
        bold.extend(
            paragraph
                .pieces
                .iter()
                .filter(|(_, style)| style.marks.contains(Marks::BOLD))
                .filter_map(|(range, _)| paragraph.text.get(range.clone())),
        );
        links.extend(paragraph.links.iter().map(|link| link.href.as_str()));
    }
    json!({
        "id": item.id, "own": item.own, "kind": kind, "text": text, "bold": bold, "links": links,
        "card": card, "unshown": unshown, "edited": item.edited,
        "sender": item.sender, "sender_name": item.sender_name.as_ref(),
    })
}
