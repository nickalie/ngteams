//! Bot chats and bot mentions, as a user sees and uses them in the app's window in the UI sandbox (`harness = false`:
//! GPUI runs on the process's main thread). It is the QA test of task 225.
//!
//! The parent runs the mock (`teams-mock`) with made-up apps and their bots: a personal bot the account added, a bot
//! whose id is its app's id, a notification-only bot, a bot with an odd and very long name, and the fixtures' apps. It
//! runs a's app as a child in the sandbox with a control socket and uses it as a user does: it clicks and types with
//! the sandbox's pointer and keyboard. It reads what the app sent from the mock, and the views' rows from the child.
//!
//! - **A bot's 1:1 chat**: named by its app in the list and in its header, its app's tile, no call, no presence; the
//!   bot's messages and card; the user's message to the bot and the bot's answer; the bot's typing line; About opens
//!   the app's details; an empty bot chat shows the app's intro; the search box finds the bot's chat by the app's name.
//! - **Bot mentions** in a group chat and in a team's channel: the picker offers the bots of the apps added there
//!   after the members and before Everyone, and not the notification-only bot nor a bot the chat does not have; the
//!   mention picked is sent as a bot's mention with its app's id; a bot with an odd and very long name; a received
//!   message that mentions a bot the chat does not have.
//! - **With `NGTEAMS_BOTS` unset** (a second run): no bot in the picker and no About; the bot chats keep their names.
//! - **An own server's account** (a third run, on the real `ngteams-server` binary when it is built): its one
//!   registered app is not added to the group chat, so the picker offers no bot and nothing shows broken.
//!
//! Every name, id and text is made up. Screenshots are kept under `target/ui-shots/qa-bot-chats-*.png`, in Teams Light
//! and Dark. Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, RawFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, future, thread};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
#[path = "../own_server_binary.rs"]
#[expect(
    dead_code,
    reason = "the own server's tests share this support; this one starts it plainly"
)]
mod own_server_binary;
#[path = "../window.rs"]
mod support;
use ngteams::descriptors::adopt_trace;
use serde_json::{Value, json};
use support::{Alias, display_name};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{CallFloor, Capabilities, Outbox};
use teams_ctl::protocol::{OpenTarget, Request};
use teams_ctl::{Client, Timeouts};
use teams_mock::{
    AMS_BASE, AppPlace, CHANNEL, CHAT_BASE, CSA_BASE, ChatEvent, ChatServiceMock, EventKind, LAB_A, LAB_B,
    MIDDLE_TIER_BASE, PRESENCE_BASE, TrouterMock, USER_PROFILE_BASE,
};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::chat_view::{Body, ChatView, MessageItem};
use teams_ui::global_search::Suggestion;
use teams_ui::rich_text::Block;
use teams_ui::shell::Shell;
use teams_ui::tray::{MENU_PATH, MENU_QUIT};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::{Modifier, VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

const CHILD_ENV: &str = "NGTEAMS_QA_BOT_CHATS_TEST";
const TRACE_FD_ENV: &str = "NGTEAMS_QA_BOT_CHATS_TRACE_FD";
const ORIGIN_ENV: &str = "NGTEAMS_QA_BOT_CHATS_ORIGIN";
const TROUTER_ENV: &str = "NGTEAMS_QA_BOT_CHATS_TROUTER";
const TOKEN_ENV: &str = "NGTEAMS_QA_BOT_CHATS_TOKEN";
const DIR_ENV: &str = "NGTEAMS_QA_BOT_CHATS_DIR";
/// `flag-off` (no `NGTEAMS_BOTS`) or `own` (an own server's account); else the ordinary app with the bots shown.
const MODE_ENV: &str = "NGTEAMS_QA_BOT_CHATS_MODE";
const CA_ENV: &str = "NGTEAMS_QA_BOT_CHATS_CA";
/// The file in the app's directory the parent asks the child about its views through.
const ASK: &str = "qa-bot-chats-ask";
/// The file in the app's directory the child reads the theme to show from (`light` or `dark`).
const THEME_FILE: &str = "theme";
const STARTUP: Duration = Duration::from_secs(90);
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(100);
/// How long the screen gets to settle before a shot.
const SETTLE: Duration = Duration::from_millis(900);

/// Where the pointer clicks to put the caret in the open chat's message box.
const COMPOSE: (u32, u32) = (830, 757);

/// The fixtures' apps (`teams-mock`'s `team_apps.rs`) this walk uses, with their bots.
const HELPER_APP: &str = "00000000-0000-4000-8000-00000000a004";
const HELPER_BOT: &str = "28:00000000-0000-4000-8000-00000000b004";
const HELPER: &str = "Made-up Helper";
const UPDATES: &str = "Made-up Updates";
const POLLS_APP: &str = "00000000-0000-4000-8000-00000000a005";
const POLLS_BOT: &str = "28:00000000-0000-4000-8000-00000000b005";
const POLLS: &str = "Made-up Polls";
const APPROVALS_APP: &str = "00000000-0000-4000-8000-00000000a006";
const APPROVALS_BOT: &str = "28:00000000-0000-4000-8000-00000000b006";
const APPROVALS: &str = "Made-up Approvals";

/// A personal bot whose id is its app's id; nobody added its app, and nobody wrote in its chat.
const DESK_APP: &str = "00000000-0000-4000-8000-0000000000d1";
const DESK_BOT: &str = "28:00000000-0000-4000-8000-0000000000d1";
const DESK: &str = "Made-up Desk";
/// A notification-only bot, added to the group chat and to the team.
const ALERTS_APP: &str = "00000000-0000-4000-8000-0000000000d2";
const ALERTS: &str = "Made-up Alerts";
/// A bot with an odd and very long name, for a 1:1 chat and the group chat.
const ODD_APP: &str = "00000000-0000-4000-8000-0000000000d3";
const ODD_BOT: &str = "28:00000000-0000-4000-8000-0000000000e3";
const ODD: &str = "Made-up \"Q&A\" <Desk> & the very long named quarterly planning and retrospective assistant";

/// What the user and the bots write.
const HELLO: &str = "Hello helper, what can you do";
const ANSWER: &str = "I can answer made-up questions";
const GROUP_TOPIC: &str = "Made-up bot group";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_bot_chats_window: {error}");
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
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams bot chats", &options)? else {
        return Ok(());
    };
    let only = std::env::var("QA_BOT_CHATS_ONLY").unwrap_or_default();
    for mode in ["", "flag-off", "own"] {
        if !only.is_empty() && only != mode && !(only == "bots" && mode.is_empty()) {
            continue;
        }
        let dir = tempfile::tempdir()?;
        let outcome = match mode {
            "" => {
                let service = Service::start(&runtime)?;
                walk(&sandbox, &runtime, &service, dir.path())
            }
            "flag-off" => {
                let service = Service::start(&runtime)?;
                flag_off(&sandbox, &runtime, &service, dir.path())
            }
            _ => own_server(&sandbox, dir.path()),
        };
        if let Err(error) = outcome {
            return Err(format!("{error}{}", sandbox.logs()).into());
        }
    }
    let findings = std::mem::take(&mut *FINDINGS.lock().unwrap_or_else(PoisonError::into_inner));
    if !findings.is_empty() {
        return Err(format!("the walk ended with findings:\n- {}", findings.join("\n- ")).into());
    }
    say("ngteams bot chats: ok");
    Ok(())
}

/// The walk of the ordinary app, the bots shown.
fn walk(sandbox: &Sandbox, runtime: &tokio::runtime::Runtime, service: &Service, dir: &Path) -> SandboxResult {
    let mut run = AppRun::launch(sandbox, &service.env(), dir, "")?;
    let outcome = (|| -> SandboxResult {
        let mut ctx = Walk {
            run: &mut run,
            sandbox,
            runtime,
            service,
        };
        ctx.list()?;
        ctx.bot_chat()?;
        ctx.typing()?;
        ctx.about()?;
        ctx.intro()?;
        ctx.odd_chat()?;
        ctx.search()?;
        ctx.group_picker()?;
        ctx.odd_mention()?;
        ctx.missing_bot()?;
        ctx.channel_picker()
    })();
    if let Err(error) = outcome {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit(sandbox)
}

/// What one step of the walk works with.
struct Walk<'a> {
    run: &'a mut AppRun,
    sandbox: &'a Sandbox,
    runtime: &'a tokio::runtime::Runtime,
    service: &'a Service,
}

impl Walk<'_> {
    fn shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        self.run.shot(self.sandbox, &format!("qa-bot-chats-{name}-light.png"))
    }

    /// The shots of `name` in Teams Light, then Teams Dark, then back to Light.
    fn shots(&mut self, name: &str) -> SandboxResult {
        self.shot(name)?;
        self.run.theme(true)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, &format!("qa-bot-chats-{name}-dark.png"))?;
        self.run.theme(false)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The messages a sent to `chat` so far, as the service got them.
    fn sent(&self, chat: &str) -> Vec<Value> {
        sent_to(self.runtime, &self.service.mock, chat)
    }

    /// Types `text` in the open chat's box after a click in it.
    fn type_in_box(&mut self, text: &str) -> SandboxResult {
        self.run.click(COMPOSE.0, COMPOSE.1)?;
        self.run.input.type_text(text)?;
        thread::sleep(Duration::from_millis(400));
        Ok(())
    }

    /// Enter in the box, then waits for one more message sent to `chat` than `before`; that message.
    fn send(&mut self, chat: &str, before: usize, what: &str) -> SandboxResult<Value> {
        self.run.input.key(keys::ENTER)?;
        let found = wait_until(WAIT, || {
            let all = self.sent(chat);
            (all.len() > before).then(|| all[before].clone())
        });
        found.ok_or_else(|| format!("{what}: nothing reached the chat service").into())
    }

    /// Empties the open box (Ctrl+A, Delete).
    fn clear_box(&mut self) -> SandboxResult {
        let a = self.run.letter('a')?;
        self.run.input.key_with(&[Modifier::Ctrl], a)?;
        self.run.input.key(keys::DELETE)?;
        thread::sleep(Duration::from_millis(300));
        Ok(())
    }
}

// --- the chat list, the bot's chat, its typing line, About, the intro ---

impl Walk<'_> {
    /// The list names each bot's chat by its app, with no call and no presence; a person's chat stays a person's.
    fn list(&mut self) -> SandboxResult {
        let service = self.service;
        let rows = match self.run.until_list("the bot chats named", |rows| {
            row(rows, &service.helper_chat).is_some_and(|row| row["name"] == HELPER)
        }) {
            Ok(rows) => rows,
            Err(error) => {
                note(format!("the bot chats are not named by their apps: {error}"));
                for line in app_requests(self.runtime, &service.mock) {
                    step(&format!("app read: {line}"));
                }
                self.run.state_in("list")?["rows"].clone()
            }
        };
        for (chat, name, bot) in [
            (&service.helper_chat, HELPER, HELPER_BOT),
            (&service.odd_chat, ODD, ODD_BOT),
        ] {
            let Some(found) = row(&rows, chat) else {
                note(format!("the list has no row for the chat with {name}"));
                continue;
            };
            expect(found["name"] == name, || {
                format!(
                    "the chat with {bot} is named {} in the list, expected {name}",
                    found["name"]
                )
            });
            expect(found["bot"] == bot, || {
                format!(
                    "the chat with {name}: its row's bot is {}, expected {bot}",
                    found["bot"]
                )
            });
            expect(found["partner"].is_null(), || {
                format!("the chat with {name} shows a presence for {}", found["partner"])
            });
            expect(found["can_call"] == false, || {
                format!("the chat with {name} offers a call")
            });
        }
        let ab = row(&rows, teams_mock::AB_CHAT).ok_or("no row for the chat with b")?;
        expect(ab["bot"].is_null() && ab["partner"] == LAB_B, || {
            format!("the 1:1 chat with b is no longer b's: {ab}")
        });
        self.shots("list")?;
        step("the list names the bot chats by their apps");
        Ok(())
    }

    /// The bot's chat: its header, the bot's text and card, the user's message, the bot's answer.
    fn bot_chat(&mut self) -> SandboxResult {
        let service = self.service;
        let chat = service.helper_chat.clone();
        self.run.open(&chat)?;
        self.run.until_chat("the bot's messages", |state| {
            state["items"].as_array().is_some_and(|items| items.len() >= 2)
        })?;
        // the engine names the bot's app after the first frame: wait for it, then judge the last state
        let state = match self.run.until_chat("the bot's app", |state| {
            state["bot"] == HELPER_BOT && misnamed(state).is_empty()
        }) {
            Ok(state) => state,
            Err(_) => self.run.state_in("chat")?,
        };
        expect(state["bot"] == HELPER_BOT, || {
            format!("the open chat's bot is {}, expected {HELPER_BOT}", state["bot"])
        });
        let misnamed = misnamed(&state);
        expect(misnamed.is_empty(), || {
            format!("the bot's messages are named {misnamed:?} over them, expected the app's name {HELPER}")
        });
        expect(
            state["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["kind"] == "cards")),
            || "the bot's card does not show as a card".to_owned(),
        );
        self.shots("chat")?;
        let before = self.sent(&chat).len();
        self.type_in_box(HELLO)?;
        let sent = self.send(&chat, before, "the message to the bot")?;
        expect(content(&sent).contains(HELLO), || {
            format!("the message to the bot sent {sent}")
        });
        service
            .mock
            .add_message(
                &chat,
                (HELPER_BOT, HELPER),
                "RichText/Html",
                &format!("<p>{ANSWER}</p>"),
            )
            .ok_or("the bot's answer was not posted")?;
        self.run.until_chat("the bot's answer", |state| {
            state["items"].as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["text"].as_str().is_some_and(|text| text.contains(ANSWER)))
            })
        })?;
        thread::sleep(SETTLE);
        self.shots("chat-answered")?;
        step("the bot's chat: its messages and card, a message to it and its answer");
        Ok(())
    }

    /// The bot types: the line names its app.
    fn typing(&mut self) -> SandboxResult {
        let service = self.service;
        let typing = |on: bool| ChatEvent {
            conversation: service.helper_chat.clone(),
            from: HELPER_BOT.to_owned(),
            kind: EventKind::Typing,
            resource: json!({"messagetype": if on { "Control/Typing" } else { "Control/ClearTyping" }}),
        };
        service.mock.queue_event(LAB_A, typing(true));
        let state = self
            .run
            .until_chat("the bot's typing line", |state| state["typing"].is_string());
        match state {
            Ok(state) => {
                let wanted = format!("{HELPER} is typing…");
                expect(state["typing"] == wanted.as_str(), || {
                    format!("the bot's typing line says {}, expected {wanted}", state["typing"])
                });
                self.shots("typing")?;
                step("the bot's typing line names its app");
            }
            Err(error) => note(format!("no typing line when the bot types: {error}")),
        }
        service.mock.queue_event(LAB_A, typing(false));
        Ok(())
    }

    /// About in the header opens the app's details; Open goes back to the chat.
    fn about(&mut self) -> SandboxResult {
        let before = self.shot("before-about")?;
        let Some((x, y)) = about_tab(&load(&before)?) else {
            note(format!("no About tab in the bot chat's header ({})", before.display()));
            return Ok(());
        };
        self.run.click(x, y)?;
        let state = self
            .run
            .until_chat("the app's details", |state| state["dialog"] == true);
        if let Err(error) = state {
            note(format!("About opens no dialog: {error}"));
            return Ok(());
        }
        thread::sleep(SETTLE);
        self.shots("about")?;
        self.run.input.key(keys::ESC)?;
        let closed = self
            .run
            .until_chat("the details closed", |state| state["dialog"] == false);
        expect(closed.is_ok(), || "Escape does not close the app's details".to_owned());
        step("About opens the app's details");
        Ok(())
    }

    /// The chat with the bot whose app nobody added: found by its id as the app's, and its intro.
    fn intro(&mut self) -> SandboxResult {
        let chat = self.service.desk_chat.clone();
        self.run.open(&chat)?;
        let state = self
            .run
            .until_chat("the empty bot chat", |state| state["bot"] == DESK_BOT)?;
        expect(state["items"].as_array().is_some_and(Vec::is_empty), || {
            format!("the empty bot chat shows {}", state["items"])
        });
        thread::sleep(SETTLE);
        self.shots("intro")?;
        step("an empty bot chat shows its app's intro");
        Ok(())
    }

    /// The chat with the bot with an odd and very long name.
    fn odd_chat(&mut self) -> SandboxResult {
        let chat = self.service.odd_chat.clone();
        self.run.open(&chat)?;
        let state = self.run.until_chat("the odd bot's message", |state| {
            state["items"].as_array().is_some_and(|items| !items.is_empty())
        })?;
        expect(state["bot"] == ODD_BOT, || {
            format!("the odd bot's chat: its bot is {}", state["bot"])
        });
        thread::sleep(SETTLE);
        self.shots("odd-chat")?;
        Ok(())
    }

    /// The search box finds the bot's chat by its app's name, and Enter opens it.
    fn search(&mut self) -> SandboxResult {
        let e = self.run.letter('e')?;
        self.run.input.key_with(&[Modifier::Ctrl], e)?;
        thread::sleep(Duration::from_millis(400));
        self.run.input.type_text("Helper")?;
        let state = self.run.until_search("the bot's chat suggested", |state| {
            state["suggestions"]
                .as_array()
                .is_some_and(|all| all.iter().any(|one| one["name"] == HELPER))
        });
        match state {
            Ok(state) => {
                let first = &state["suggestions"][0];
                expect(
                    first["id"] == self.service.helper_chat.as_str()
                        || first["chat"] == self.service.helper_chat.as_str(),
                    || format!("the search for Helper suggests first {first}, expected the bot's chat"),
                );
                thread::sleep(SETTLE);
                self.shots("search")?;
            }
            Err(error) => {
                self.shot("search")?;
                note(format!(
                    "the search for Helper does not suggest the bot's chat: {error}"
                ));
            }
        }
        self.run.input.key(keys::ESC)?;
        thread::sleep(Duration::from_millis(400));
        self.run.input.key(keys::ESC)?;
        thread::sleep(Duration::from_millis(400));
        step("searched for the bot's app");
        Ok(())
    }
}

/// `chat`'s row in `rows`.
fn row<'a>(rows: &'a Value, chat: &str) -> Option<&'a Value> {
    rows.as_array()?.iter().find(|row| row["id"] == chat)
}

// --- the mentions ---

impl Walk<'_> {
    /// The group chat's picker: the members, the bots of its apps but the notification-only one, then Everyone; Polls
    /// picked and sent as a bot's mention.
    fn group_picker(&mut self) -> SandboxResult {
        let service = self.service;
        let chat = service.group.clone();
        self.run.open(&chat)?;
        self.run.until_chat("the group's messages", |state| {
            state["items"].as_array().is_some_and(|items| !items.is_empty())
        })?;
        thread::sleep(SETTLE);
        self.shots("group-received")?;
        self.type_in_box("Who runs this @")?;
        let picker = self.run.until_chat("the picker's bots", |state| {
            state["picker"]["loading"] == false
                && state["picker"]["names"]
                    .as_array()
                    .is_some_and(|names| names.iter().any(|name| name == POLLS))
        });
        let offered = match picker {
            Ok(state) => names(&state),
            Err(error) => {
                self.shot("group-picker")?;
                note(format!("the group chat's picker offers no bot: {error}"));
                self.clear_box()?;
                return Ok(());
            }
        };
        step(&format!("the group chat's picker offers {offered:?}"));
        let wanted = vec![
            "Lab B".to_owned(),
            ODD.to_owned(),
            POLLS.to_owned(),
            "Everyone".to_owned(),
        ];
        expect(offered == wanted, || {
            format!("the group chat's picker offers {offered:?}, expected {wanted:?}")
        });
        expect(!offered.iter().any(|name| name == ALERTS), || {
            "the picker offers the notification-only bot".to_owned()
        });
        expect(!offered.iter().any(|name| name == APPROVALS), || {
            "the picker offers a bot the chat does not have".to_owned()
        });
        self.shots("group-picker")?;
        self.run.input.type_text("pol")?;
        let narrowed = self.run.until_chat("the picker narrowed", |state| {
            state["picker"]["names"]
                .as_array()
                .is_some_and(|names| names.len() == 1)
        })?;
        expect(names(&narrowed) == [POLLS], || {
            format!("@pol offers {:?}, expected only {POLLS}", names(&narrowed))
        });
        self.run.input.key(keys::ENTER)?;
        thread::sleep(Duration::from_millis(400));
        self.run.input.type_text(" today")?;
        thread::sleep(SETTLE);
        self.shot("group-mention-in-box")?;
        let before = self.sent(&chat).len();
        let sent = self.send(&chat, before, "the message with the bot's mention")?;
        expect_bot_mention(&sent, POLLS_BOT, POLLS_APP, POLLS);
        self.run.until_chat("the sent mention shown", |state| {
            state["items"].as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["own"] == true && item["text"].as_str().is_some_and(|text| text.contains("today")))
            })
        })?;
        thread::sleep(SETTLE);
        self.shots("group-mention-sent")?;
        step("the group chat's bot mention is sent with its app");
        Ok(())
    }

    /// The bot with the odd and very long name, picked by part of its name, sent and shown.
    fn odd_mention(&mut self) -> SandboxResult {
        let chat = self.service.group.clone();
        self.type_in_box("Ask @Q&A")?;
        let picker = self.run.until_chat("the odd bot offered", |state| {
            state["picker"]["names"]
                .as_array()
                .is_some_and(|names| names.iter().any(|name| name == ODD))
        });
        if let Err(error) = picker {
            self.shot("odd-picker")?;
            note(format!("@Q&A does not offer the bot named {ODD}: {error}"));
            self.clear_box()?;
            return Ok(());
        }
        self.shots("odd-picker")?;
        self.run.input.key(keys::ENTER)?;
        thread::sleep(Duration::from_millis(400));
        self.run.input.type_text(" please")?;
        let before = self.sent(&chat).len();
        let sent = self.send(&chat, before, "the message with the odd bot's mention")?;
        expect_bot_mention(&sent, ODD_BOT, ODD_APP, ODD);
        let html = content(&sent);
        expect(!html.contains("<Desk>") && html.contains("&lt;Desk&gt;"), || {
            format!("the odd bot's name is not escaped in the message's HTML: {html}")
        });
        self.run.until_chat("the odd mention shown", |state| {
            state["items"].as_array().is_some_and(|items| {
                items.iter().any(|item| {
                    item["own"] == true && item["text"].as_str().is_some_and(|text| text.contains("please"))
                })
            })
        })?;
        thread::sleep(SETTLE);
        self.shots("odd-mention-sent")?;
        step("the odd bot's mention is sent and shown");
        Ok(())
    }

    /// A bot the chat does not have: the picker offers nothing for it, and the text goes as text.
    fn missing_bot(&mut self) -> SandboxResult {
        let chat = self.service.group.clone();
        self.type_in_box("@Approv")?;
        thread::sleep(SETTLE * 2);
        let state = self.run.state_in("chat")?;
        let offered = names(&state);
        expect(!offered.iter().any(|name| name == APPROVALS), || {
            format!("@Approv offers {offered:?} in a chat without the Approvals app")
        });
        self.shot("missing-picker")?;
        self.run.input.type_text("als is not here")?;
        let before = self.sent(&chat).len();
        let sent = self.send(&chat, before, "the text that names a bot the chat does not have")?;
        let mentions = mentions_of(&sent);
        expect(mentions.is_empty(), || {
            format!("a text that names a bot the chat does not have is sent with mentions {mentions:?}")
        });
        step("a bot the chat does not have is not offered");
        Ok(())
    }

    /// The team's channel: its picker offers the team's bots but the notification-only one.
    fn channel_picker(&mut self) -> SandboxResult {
        self.run.open_channel(CHANNEL)?;
        thread::sleep(SETTLE * 2);
        self.shot("channel")?;
        let before = self.run.state_in("channel")?;
        step(&format!("the channel: {}", before["conversation"]));
        self.type_in_box("Status please @")?;
        let picker = self.run.until_in("channel", "the channel picker's bots", |state| {
            state["picker"]["loading"] == false
                && state["picker"]["names"]
                    .as_array()
                    .is_some_and(|names| names.iter().any(|name| name == POLLS))
        });
        let state = match picker {
            Ok(state) => state,
            Err(error) => {
                self.shot("channel-picker")?;
                note(format!("the channel's picker offers no bot: {error}"));
                return Ok(());
            }
        };
        let offered = names(&state);
        step(&format!("the channel's picker offers {offered:?}"));
        for wanted in [POLLS, UPDATES] {
            expect(offered.iter().any(|name| name == wanted), || {
                format!("the channel's picker offers {offered:?}, without {wanted}")
            });
        }
        expect(!offered.iter().any(|name| name == ALERTS), || {
            "the channel's picker offers the notification-only bot".to_owned()
        });
        self.shots("channel-picker")?;
        let channel_before = self.sent(CHANNEL).len();
        self.run.input.type_text("upd")?;
        thread::sleep(Duration::from_millis(600));
        self.run.input.key(keys::ENTER)?;
        thread::sleep(Duration::from_millis(400));
        let sent = self.send(CHANNEL, channel_before, "the channel post with the bot's mention")?;
        expect_bot_mention(
            &sent,
            "28:00000000-0000-4000-8000-00000000b002",
            "00000000-0000-4000-8000-00000000a002",
            UPDATES,
        );
        thread::sleep(SETTLE);
        self.shots("channel-mention-sent")?;
        step("the channel's bot mention is sent with its app");
        Ok(())
    }
}

/// The picker's names in `state`.
fn names(state: &Value) -> Vec<String> {
    state["picker"]["names"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// A sent message's HTML.
fn content(sent: &Value) -> String {
    sent["content"].as_str().unwrap_or_default().to_owned()
}

/// A sent message's mentions (`properties.mentions`, a JSON text in a list).
fn mentions_of(sent: &Value) -> Vec<Value> {
    let mentions = &sent["properties"]["mentions"];
    let parsed = mentions
        .as_str()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .unwrap_or_else(|| mentions.clone());
    parsed.as_array().cloned().unwrap_or_default()
}

/// The names over the messages of others in the chat's `state` that are not the bot's app's name.
fn misnamed(state: &Value) -> Vec<&Value> {
    state["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["own"] == false && item["sender_name"] != HELPER)
        .map(|item| &item["sender_name"])
        .collect()
}

/// Notes what is wrong with `sent` as a message that mentions the bot `bot` of the app `app` named `name`.
fn expect_bot_mention(sent: &Value, bot: &str, app: &str, name: &str) {
    let mentions = mentions_of(sent);
    let found = mentions.iter().find(|mention| mention["mri"] == bot);
    let Some(found) = found else {
        note(format!("the message names no mention of {bot}: {sent}"));
        return;
    };
    expect(
        found["mentionType"] == "bot" && found["appId"] == app && found["displayName"] == name,
        || format!("the mention of {name} is {found}, expected a bot's mention with the app {app}"),
    );
    let html = content(sent);
    expect(html.contains("http://schema.skype.com/Mention"), || {
        format!("the message's HTML has no mention's span: {html}")
    });
}

// --- the bots hidden ---

/// The app without `NGTEAMS_BOTS`: the bot chats keep their names, no bot in the picker, no About.
fn flag_off(sandbox: &Sandbox, runtime: &tokio::runtime::Runtime, service: &Service, dir: &Path) -> SandboxResult {
    let mut run = AppRun::launch(sandbox, &service.env(), dir, "flag-off")?;
    let outcome = (|| -> SandboxResult {
        let mut ctx = Walk {
            run: &mut run,
            sandbox,
            runtime,
            service,
        };
        let rows = ctx.run.until_list("the bot chats named", |rows| {
            row(rows, &service.helper_chat).is_some_and(|row| row["name"] == HELPER)
        });
        expect(rows.is_ok(), || {
            "with the bots hidden, the bot's chat is not named by its app".to_owned()
        });
        ctx.run.open(&service.helper_chat)?;
        ctx.run.until_chat("the bot's messages", |state| {
            state["items"].as_array().is_some_and(|items| !items.is_empty())
        })?;
        thread::sleep(SETTLE);
        let shot = ctx.shot("off-chat")?;
        ctx.shots("off-chat")?;
        if let Some((x, y)) = about_tab(&load(&shot)?) {
            ctx.run.click(x, y)?;
            thread::sleep(SETTLE);
            let state = ctx.run.state_in("chat")?;
            expect(state["dialog"] == false, || {
                "with the bots hidden, a click on the header's About opens the app's details".to_owned()
            });
        }
        ctx.run.open(&service.group)?;
        thread::sleep(SETTLE);
        ctx.type_in_box("Who runs this @")?;
        let state = ctx
            .run
            .until_chat("the picker", |state| state["picker"]["loading"] == false)?;
        let offered = names(&state);
        expect(offered == ["Lab B", "Everyone"], || {
            format!("with the bots hidden, the group chat's picker offers {offered:?}, expected [Lab B, Everyone]")
        });
        ctx.shots("off-picker")?;
        ctx.clear_box()?;
        step("with the bots hidden: no bot in the picker, no About");
        Ok(())
    })();
    if let Err(error) = outcome {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit(sandbox)
}

// --- an own server's account ---

const OWN_UPN: &str = "dana@own.example.test";
const OWN_B: &str = "erin@own.example.test";
const OWN_C: &str = "finn@own.example.test";

/// An own server's account on the real binary: its picker offers no bot, and its chats show as before.
fn own_server(sandbox: &Sandbox, dir: &Path) -> SandboxResult {
    let Some(binary) = own_server_binary::binary()? else {
        say("skipped own-server checks: no ngteams-server binary (cargo build -p ngteams-server)");
        return Ok(());
    };
    let server = own_server_binary::BinaryServer::start(&binary)?;
    server.add_user(OWN_UPN, "Dana Made-Up")?;
    server.add_user(OWN_B, "Erin Made-Up")?;
    server.add_user(OWN_C, "Finn Made-Up")?;
    let added = server.add_app(
        "Made-up Notes (synthetic)",
        "https://bots.example.test/api/messages",
        "A made-up app an admin registered",
    )?;
    step(&format!("own server: {}", added.lines().next().unwrap_or_default()));
    let env = vec![
        (ORIGIN_ENV, server.origin.clone()),
        (CA_ENV, server.ca_file.to_string_lossy().into_owned()),
    ];
    let mut run = AppRun::launch(sandbox, &env, dir, "own")?;
    let outcome = (|| -> SandboxResult {
        thread::sleep(SETTLE * 2);
        run.shot(sandbox, "qa-bot-chats-own-start-light.png")?;
        run.new_chat(&[OWN_B, OWN_C])?;
        let rows = run.until_list("the group chat made", |rows| {
            rows.as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["kind"] == "group"))
        })?;
        let group = rows
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["kind"] == "group"))
            .and_then(|row| row["id"].as_str())
            .ok_or("no group row")?
            .to_owned();
        run.open(&group)?;
        thread::sleep(SETTLE);
        run.click(COMPOSE.0, COMPOSE.1)?;
        run.input.type_text("Who runs this @")?;
        let state = run.until_chat("the own picker", |state| state["picker"]["loading"] == false)?;
        let offered = names(&state);
        step(&format!("the own server's group picker offers {offered:?}"));
        expect(
            offered
                .iter()
                .all(|name| name.contains("Made-Up") || name == "Everyone"),
            || format!("an own server's group picker offers {offered:?}: more than its people and Everyone"),
        );
        expect(offered.iter().any(|name| name == "Everyone"), || {
            format!("an own server's group picker offers {offered:?}, without Everyone")
        });
        run.shot(sandbox, "qa-bot-chats-own-picker-light.png")?;
        run.theme(true)?;
        thread::sleep(SETTLE);
        run.shot(sandbox, "qa-bot-chats-own-picker-dark.png")?;
        run.theme(false)?;
        let rows = run.state_in("list")?;
        for found in rows["rows"].as_array().into_iter().flatten() {
            expect(found["bot"].is_null(), || {
                format!("an own server's chat shows as a bot's: {found}")
            });
        }
        step("an own server's account: no bot offered, nothing broken");
        Ok(())
    })();
    if let Err(error) = outcome {
        let _ = fs::write(dir.join("own-server.log"), server.log());
        return Err(run.fail(sandbox, &*error));
    }
    run.quit(sandbox)
}

// --- the screen ---

fn load(shot: &Path) -> SandboxResult<image::RgbImage> {
    Ok(image::open(shot)?.to_rgb8())
}

/// Where the header's About tab is: the Chat tab is underlined in the brand colour at the header's bottom (y 86..96);
/// About is the next tab right of it, on the header's middle line.
fn about_tab(shot: &image::RgbImage) -> Option<(u32, u32)> {
    let brand = |[r, g, b]: [u8; 3]| (i32::from(b) - i32::from(r)) > 50 && (i32::from(b) - i32::from(g)) > 30;
    for y in 86..96 {
        let xs: Vec<u32> = (460..1100).filter(|&x| brand(shot.get_pixel(x, y).0)).collect();
        if let (Some(&first), Some(&last)) = (xs.first(), xs.last())
            && last - first > 12
            && last - first < 120
        {
            return Some((last + 40, 71));
        }
    }
    None
}

// --- the world on the mock ---

struct Service {
    mock: ChatServiceMock,
    trouter: TrouterMock,
    helper_chat: String,
    desk_chat: String,
    odd_chat: String,
    group: String,
}

impl Service {
    fn start(runtime: &tokio::runtime::Runtime) -> SandboxResult<Self> {
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        for definition in made_up_apps() {
            mock.add_app(definition);
        }
        mock.install_app(AppPlace::Personal, HELPER_APP);
        mock.install_app(AppPlace::Personal, ODD_APP);
        let helper_chat = mock.add_bot_chat(HELPER_BOT).ok_or("no chat with the helper")?;
        mock.add_message(
            &helper_chat,
            (HELPER_BOT, "Helper bot"),
            "RichText/Html",
            "<p>Hi! I am a made-up helper. Ask me anything made up.</p>",
        )
        .ok_or("the helper's text was not posted")?;
        post_card(&mock, &helper_chat, HELPER_BOT, &adaptive(&help_card()))?;
        let desk_chat = mock.add_bot_chat(DESK_BOT).ok_or("no chat with the desk")?;
        let odd_chat = mock.add_bot_chat(ODD_BOT).ok_or("no chat with the odd bot")?;
        mock.add_message(
            &odd_chat,
            (ODD_BOT, "Odd bot"),
            "RichText/Html",
            "<p>A made-up odd hello.</p>",
        )
        .ok_or("the odd bot's text was not posted")?;
        let group = mock.add_group(GROUP_TOPIC, &[LAB_A, LAB_B]).ok_or("no group made")?;
        for app in [POLLS_APP, ALERTS_APP, ODD_APP] {
            mock.install_app(AppPlace::Chat(group.clone()), app);
        }
        for app in [POLLS_APP, ALERTS_APP] {
            mock.install_app(AppPlace::Team(CHANNEL.to_owned()), app);
        }
        received_mention(&mock, &group)?;
        let _ = mock.take_events(LAB_A);
        Ok(Self {
            mock,
            trouter,
            helper_chat,
            desk_chat,
            odd_chat,
            group,
        })
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            (ORIGIN_ENV, self.mock.origin()),
            (TROUTER_ENV, self.trouter.origin().to_owned()),
            (TOKEN_ENV, self.mock.token(LAB_A).unwrap_or_default()),
        ]
    }
}

/// The made-up apps beside the fixtures': a personal bot with its app's id, a notification-only bot, and a bot with an
/// odd and very long name.
fn made_up_apps() -> Vec<Value> {
    let bot_id = |mri: &str| mri.trim_start_matches("28:").to_owned();
    vec![
        json!({ "id": DESK_APP, "name": DESK, "developerName": "Made-up Maker", "shortDescription": "A made-up help desk",
                "longDescription": "A made-up help desk to chat with.", "version": "1.0.0", "accentColor": "#038387",
                "bots": [{ "id": bot_id(DESK_BOT), "scopes": ["personal"] }] }),
        json!({ "id": ALERTS_APP, "name": ALERTS, "developerName": "Made-up Maker", "shortDescription": "Made-up alerts",
                "version": "1.0.0", "accentColor": "#c4314b",
                "bots": [{ "id": "00000000-0000-4000-8000-0000000000e2", "scopes": ["groupChat", "team"],
                           "isNotificationOnly": true }] }),
        json!({ "id": ODD_APP, "name": ODD, "developerName": "Made-up Maker & Sons <Ltd>",
                "shortDescription": "A made-up assistant with an odd and very long name",
                "version": "0.9.1", "accentColor": "#8764b8",
                "bots": [{ "id": bot_id(ODD_BOT), "scopes": ["personal", "groupChat"] }] }),
    ]
}

/// b's message in the group that mentions the Approvals bot (an app the chat does not have) and the Polls bot.
fn received_mention(mock: &ChatServiceMock, group: &str) -> SandboxResult {
    let span = |id: u32, name: &str| {
        format!("<span itemscope=\"\" itemtype=\"http://schema.skype.com/Mention\" itemid=\"{id}\">{name}</span>")
    };
    let html = format!(
        "<p>Ask {} or {} about the made-up plan.</p>",
        span(0, APPROVALS),
        span(1, POLLS)
    );
    let id = mock
        .add_message(group, (LAB_B, "b"), "RichText/Html", &html)
        .ok_or("b's mention was not posted")?;
    let client_id = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
    let mentions = json!([
        {"@type": "http://schema.skype.com/Mention", "itemid": "0", "mri": APPROVALS_BOT, "mentionType": "bot",
         "displayName": APPROVALS, "appId": APPROVALS_APP},
        {"@type": "http://schema.skype.com/Mention", "itemid": "1", "mri": POLLS_BOT, "mentionType": "bot",
         "displayName": POLLS, "appId": POLLS_APP},
    ]);
    let changed = mock.change_message(group, &client_id, |message| {
        message["properties"]["mentions"] = json!(mentions.to_string());
    });
    if !changed {
        return Err("b's mention was not changed".into());
    }
    Ok(())
}

/// `bot` posts a card message carrying `activity` to `chat`, as the chat service delivers a bot's card; its id.
fn post_card(mock: &ChatServiceMock, chat: &str, bot: &str, activity: &Value) -> SandboxResult<String> {
    let b64 = STANDARD.encode(serde_json::to_vec(activity)?);
    let content = format!(
        "<URIObject type=\"SWIFT.1\">Card - access it on <a href=\"https://go.skype.com/cards.unsupported\">\
         https://go.skype.com/cards.unsupported</a>. <Title>Card</Title><Swift b64=\"{b64}\"></Swift></URIObject>"
    );
    mock.add_message(chat, (bot, "Helper bot"), "RichText/Media_Card", &content)
        .ok_or_else(|| format!("the card was not posted to {chat}").into())
}

fn adaptive(card: &Value) -> Value {
    json!({"type": "message/card",
           "attachments": [{"contentType": "application/vnd.microsoft.card.adaptive", "content": card}]})
}

fn help_card() -> Value {
    json!({
        "type": "AdaptiveCard", "version": "1.5",
        "body": [
            {"type": "TextBlock", "text": "What I can do", "weight": "Bolder", "size": "Medium"},
            {"type": "TextBlock", "text": "Answer made-up questions, and keep made-up notes.", "wrap": true},
            {"type": "FactSet", "facts": [{"title": "Version", "value": "1.0.0"}, {"title": "Maker", "value": "Made-up Maker"}]},
        ],
        "actions": [{"type": "Action.OpenUrl", "title": "Learn more", "url": "https://example.com/made-up-help"}],
    })
}

/// The messages a sent to `chat`, as the service got them (each request's JSON body), in order.
fn sent_to(runtime: &tokio::runtime::Runtime, mock: &ChatServiceMock, chat: &str) -> Vec<Value> {
    let end = format!("/conversations/{chat}/messages");
    runtime
        .block_on(mock.received_requests())
        .iter()
        .filter(|request| request.method.as_str() == "POST" && decoded(request.url.path()).ends_with(&end))
        .filter_map(|request| serde_json::from_slice::<Value>(&request.body).ok())
        .filter(|body| body["messagetype"] != "Control/Typing" && body["messagetype"] != "Control/ClearTyping")
        .collect()
}

/// The app reads the mock got: each one's method, path, body and the mock's status.
fn app_requests(runtime: &tokio::runtime::Runtime, mock: &ChatServiceMock) -> Vec<String> {
    runtime
        .block_on(mock.received_requests())
        .iter()
        .filter(|request| request.url.path().contains("apps"))
        .map(|request| {
            format!(
                "{} {} {}",
                request.method,
                request.url.path(),
                String::from_utf8_lossy(&request.body)
            )
        })
        .collect()
}

/// `path` with the characters a chat's id has percent-encoded (`:`, `@`, `;`, `=`) decoded.
fn decoded(path: &str) -> String {
    [("%3A", ":"), ("%40", "@"), ("%3B", ";"), ("%3D", "=")]
        .iter()
        .fold(path.to_owned(), |path, (encoded, plain)| {
            path.replace(encoded, plain).replace(&encoded.to_lowercase(), plain)
        })
}

// --- the app in the sandbox ---

/// The child's view trace, line by line as it comes.
struct TraceReader {
    lines: mpsc::Receiver<Value>,
    seen: Vec<Value>,
}

impl TraceReader {
    fn start(reader: io::PipeReader) -> Self {
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let value = serde_json::from_str(&line).unwrap_or(Value::String(line));
                let _ = sender.send(value);
            }
        });
        Self {
            lines,
            seen: Vec::new(),
        }
    }

    /// Waits for a line, not seen before, that `found` takes, named `what` in the error.
    fn next_where(&mut self, what: &str, found: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .map_err(|_| format!("no trace line {what}; the last seen {:?}", self.seen.last()))?;
            self.seen.push(line.clone());
            if found(&line) {
                return Ok(line);
            }
        }
    }
}

/// The app running in the sandbox: its process, its view trace, its control socket and the user's pointer.
struct AppRun {
    running: Running,
    trace: TraceReader,
    ctl: PathBuf,
    dir: PathBuf,
    input: VirtualInput,
    /// Where the pointer rests between clicks: on the empty title bar.
    rest: (u32, u32),
    asked: u32,
}

impl AppRun {
    /// Starts the app with `env` and its store and settings in `dir`, in `mode`; once its window is drawn.
    fn launch(sandbox: &Sandbox, env: &[(&'static str, String)], dir: &Path, mode: &str) -> SandboxResult<Self> {
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let dir_text = dir.to_string_lossy().into_owned();
        let mut vars: Vec<(&str, &str)> = vec![
            (CHILD_ENV, "app"),
            (TRACE_FD_ENV, &fd),
            (DIR_ENV, &dir_text),
            (MODE_ENV, mode),
        ];
        vars.extend(env.iter().map(|(name, value)| (*name, value.as_str())));
        if mode != "flag-off" {
            vars.push((teams_ui::bots_flag::BOTS_ENV, "1"));
        }
        let child = sandbox.spawn_with_fds(&std::env::current_exe()?, &[], &vars, &[writer.as_fd()])?;
        drop(writer);
        let mut running = Running::new(child)?;
        let trace = TraceReader::start(reader);
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
        let ctl = dir.join("ctl.sock");
        wait_until(WAIT, || Client::connect(&ctl, Timeouts::default()).ok()).ok_or("no control socket")?;
        let run = Self {
            running,
            trace,
            ctl,
            dir: dir.to_owned(),
            input: sandbox.input()?,
            rest: (200, 24),
            asked: 0,
        };
        let mut run = run;
        run.input.move_to(run.rest.0, run.rest.1)?;
        Ok(run)
    }

    fn request(&self, request: Request) -> SandboxResult {
        let mut client = Client::connect(&self.ctl, Timeouts::default())?;
        client.request(None, request)?;
        Ok(())
    }

    /// `ngteamsctl open chat <chat>`, then the trace's `chat opened`.
    fn open(&mut self, chat: &str) -> SandboxResult {
        self.request(Request::Open {
            target: OpenTarget::Chat { chat: chat.to_owned() },
        })?;
        self.trace.next_where(&format!("chat {chat} opened"), |line| {
            line["view"] == "chat" && line["change"] == "opened" && line["conversation"] == chat
        })?;
        Ok(())
    }

    /// `ngteamsctl open channel <channel>`.
    fn open_channel(&mut self, channel: &str) -> SandboxResult {
        self.request(Request::Open {
            target: OpenTarget::Channel {
                channel: channel.to_owned(),
            },
        })
    }

    /// `ngteamsctl new-chat <people>`.
    fn new_chat(&mut self, people: &[&str]) -> SandboxResult {
        self.request(Request::NewChat {
            people: people.iter().map(|&person| person.to_owned()).collect(),
            wait: true,
        })
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

    fn until_search(&mut self, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        self.until_in("search", what, wanted)
    }

    /// The list's rows once `wanted` takes them.
    fn until_list(&mut self, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let state = self.until_in("list", what, |state| wanted(&state["rows"]))?;
        Ok(state["rows"].clone())
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
        fs::copy(&png, shots_dir()?.join(name))?;
        Ok(png)
    }

    /// Clicks (`x`, `y`) as a user does.
    fn click(&mut self, x: u32, y: u32) -> SandboxResult {
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(Duration::from_millis(250));
        Ok(())
    }

    /// The key of letter `letter`.
    fn letter(&self, letter: char) -> SandboxResult<u32> {
        self.input
            .keymap()
            .key_for(letter)
            .map(|(key, _)| key)
            .ok_or_else(|| format!("no key for {letter}").into())
    }

    /// The error, with the last screen and the app's stderr.
    fn fail(&mut self, sandbox: &Sandbox, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
        let shot = self
            .shot(sandbox, "qa-bot-chats-failed.png")
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

/// One account's mock token, for the transport and the push channel alike.
struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

impl Tokens {
    fn access(&self) -> AccessToken {
        AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: std::time::SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }
    }
}

/// a's engine on the parent's mock, its store in `dir`.
fn engine_on_the_mock(dir: &Path) -> SandboxResult<EngineHandle> {
    let origin = std::env::var(ORIGIN_ENV)?;
    let tokens = Arc::new(Tokens {
        token: std::env::var(TOKEN_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let http = || -> SandboxResult<HttpTransport<Tokens>> {
        Ok(HttpTransport::with_mock_origin(
            Arc::clone(&tokens),
            CHAT_BASE,
            &origin,
        )?)
    };
    let transport = Arc::new(http()?.with_middle_tier(MIDDLE_TIER_BASE)?);
    let trouter = Trouter::with_mock_origin(
        Arc::clone(&transport),
        Arc::clone(&tokens),
        &std::env::var(TROUTER_ENV)?,
    )?
    .with_options(Options {
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(50),
        backoff_jitter_percent: 0,
        ..Options::default()
    });
    let author = Author::new(Mri::new(LAB_A), display_name("bot-a@lab.example.test"));
    Ok(engine::start(Parts {
        calls: None,
        store: Store::open(&support::store_file(dir, Alias::A))?,
        client: Arc::new(ChatClient::new(
            http()?
                .with_ams(AMS_BASE)?
                .with_presence(PRESENCE_BASE)?
                .with_csa(CSA_BASE)?
                .with_user_profile(USER_PROFILE_BASE)?
                .with_middle_tier(MIDDLE_TIER_BASE)?,
        )),
        outbox: Outbox::new(transport, author),
        push: Some(Push::new(trouter)),
        options: EngineOptions {
            capabilities: Capabilities::ALL,
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
    })?)
}

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
        OWN_UPN,
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
    let mode = std::env::var(MODE_ENV).unwrap_or_default();
    let (first, runner, runtime) = if mode == "own" {
        let (first, runner, runtime) = own_account(&dir)?;
        (first, runner, Some(runtime))
    } else {
        let engine = engine_on_the_mock(&dir)?;
        (support::started(Alias::A, engine)?, support::runner(&dir)?, None)
    };
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
                .update(cx, |kit, _, cx| {
                    let shell = kit
                        .view()
                        .clone()
                        .downcast::<Root>()
                        .ok()?
                        .read(cx)
                        .shell_in(cx)
                        .cloned()?;
                    Some(on_shell(&shell, verb, cx))
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| "no shell".to_owned());
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn on_shell(shell: &Entity<Shell>, verb: &str, cx: &App) -> String {
    let dialog = shell.read(cx).team_dialog().is_some();
    match verb {
        "chat" => shell.read(cx).chat().map_or_else(
            || json!({"items": [], "picker": null, "dialog": dialog}),
            |view| state_of(view.read(cx), dialog, cx),
        ),
        "channel" => shell.read(cx).channel().map_or_else(
            || json!({"items": [], "picker": null, "dialog": dialog}),
            |channel| state_of(channel.read(cx).posts().read(cx), dialog, cx),
        ),
        "list" => list_of(shell, cx),
        "search" => search_of(shell, cx),
        other => json!(format!("unknown {other}")),
    }
    .to_string()
}

/// The list's rows: each chat's id, name, bot, presence and whether it offers a call.
fn list_of(shell: &Entity<Shell>, cx: &App) -> Value {
    let list = shell.read(cx).list().read(cx);
    let rows: Vec<Value> = list
        .rows()
        .iter()
        .map(|row| {
            let bot = row.bot.as_ref();
            json!({
                "id": row.id,
                "name": row.name.as_ref(),
                "kind": format!("{:?}", row.kind).to_lowercase(),
                "bot": bot.map(|bot| bot.mri.clone()),
                "app": bot.and_then(|bot| bot.app.as_ref()).map(|app| app.app.app.id.clone()),
                "partner": row.partner,
                "can_call": list.can_call(&row.id),
            })
        })
        .collect();
    json!({ "rows": rows })
}

/// The search box's suggestions.
fn search_of(shell: &Entity<Shell>, cx: &App) -> Value {
    let search = shell.read(cx).search().read(cx);
    let suggestions: Vec<Value> = search
        .suggestions()
        .iter()
        .map(|suggestion| match suggestion {
            Suggestion::Chat { id, name, .. } => json!({"kind": "chat", "id": id, "name": name.as_ref()}),
            Suggestion::Channel { id, name, .. } => json!({"kind": "channel", "id": id, "name": name.as_ref()}),
            Suggestion::Person { mri, name, chat, .. } => {
                json!({"kind": "person", "id": mri, "chat": chat, "name": name.as_ref()})
            }
            _ => json!({"kind": "other"}),
        })
        .collect();
    json!({ "open": search.is_open(), "query": search.query(cx), "suggestions": suggestions })
}

/// The view's rows, its bot, its typing line and its box's picker.
fn state_of(view: &ChatView, dialog: bool, cx: &App) -> Value {
    let items: Vec<Value> = view.message_items().map(item_of).collect();
    let picker = view.compose().read(cx).picker().map(|picker| {
        let names: Vec<&str> = picker.matches.iter().map(|candidate| candidate.name.as_ref()).collect();
        let bots: Vec<Option<String>> = picker
            .matches
            .iter()
            .map(|candidate| candidate.app.as_ref().map(|app| app.app.app.id.clone()))
            .collect();
        json!({ "names": names, "apps": bots, "loading": picker.loading })
    });
    json!({
        "conversation": view.conversation(),
        "bot": view.chat_bot(),
        "items": items,
        "typing": view.typing_text(cx),
        "picker": picker,
        "dialog": dialog,
    })
}

fn item_of(item: &MessageItem) -> Value {
    let (kind, text) = match &item.body {
        Body::Text(rendered) => (
            "text",
            rendered
                .0
                .blocks
                .iter()
                .flat_map(Block::plain_lines)
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        Body::Cards(_) => ("cards", String::new()),
        _ => ("other", String::new()),
    };
    json!({
        "id": item.id, "own": item.own, "kind": kind, "text": text,
        "sender": item.sender, "sender_name": item.sender_name.as_ref(),
    })
}
