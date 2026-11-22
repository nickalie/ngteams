//! Message extensions and apps' dialogs with the bots shown (`NGTEAMS_BOTS=1`), as a user sees and uses them in the
//! app's window in the UI sandbox (`harness = false`: GPUI runs on the process's main thread). It is the QA test of
//! task 227.
//!
//! The parent runs the mock (`teams-mock`) with the made-up apps of `tests/fixtures/bots/extension-apps.json`, more
//! made-up search apps (one with a very long name), and scripts their bots. It runs a's app as a child in the sandbox
//! with a control socket, and uses it as a user does: it clicks and types with the sandbox's pointer and keyboard, and
//! finds the flyout, the dialog, its fields and its buttons on the screen. It reads what the bots got from the mock,
//! what the app sent to the chat, and the views' state from the child.
//!
//! - **Actions and apps** (the compose box's "+"): the account's apps that work in messages, many of them, a long
//!   name, the search for apps, no match, Up, Down, Enter and Escape, "Get more apps".
//! - **A search app**: its results at once, a search with words, a result chosen into the box (one asks the app with
//!   `selectItem`), the card sent with a message and alone, the card's close mark, a search with no results, a late
//!   answer (a search's and a `selectItem`'s) after its pane is gone and a different app's pane is open.
//! - **An action app**: its card dialog; a required field; Submit and a next dialog, an error, the app's message, a
//!   card into the box; Escape, Cancel and Close; a dialog that is a web page and its link.
//! - **A message's "More actions"**: the apps' message actions, a store offer in italics that opens its details, a
//!   message action's dialog.
//! - **A card's `task/fetch` button**: the app's dialog in the app.
//! - **A narrow window**: the flyout and a dialog.
//! - **More runs**: an account with no apps; the read-only mode; `NGTEAMS_BOTS` unset; an own server's account on the
//!   real `ngteams-server` binary (it names no `appsService` yet, so nothing of this shows).
//!
//! Every name, id and text is made up. Screenshots are kept under `target/ui-shots/qa-me-*.png`, in Teams Light and
//! Dark. `QA_ME_ONLY` (a run's name: `walk`, `no-apps`, `read-only`, `flag-off`, `own`) runs only that run. Without
//! weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

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
    AMS_BASE, AppPlace, BotReply, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, MIDDLE_TIER_BASE, PRESENCE_BASE,
    TrouterMock, USER_PROFILE_BASE,
};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::app_dialog::AppDialog;
use teams_ui::apps_page::details::AppDetailsDialog;
use teams_ui::chat_view::{Body, ChatView, MessageItem};
use teams_ui::rich_text::Block;
use teams_ui::shell::Shell;
use teams_ui::tray::{MENU_PATH, MENU_QUIT};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::{Modifier, VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

const CHILD_ENV: &str = "NGTEAMS_QA_ME_TEST";
/// From the centre of the menu's "Customize" row to the centre of the row over it, px: half a row, a 2 px gap, the
/// hairline (1 px with 4 px over and under), a 2 px gap and half a row.
const CUSTOMIZE_FOOT: u32 = 16 + 2 + 9 + 2 + 16;
const TRACE_FD_ENV: &str = "NGTEAMS_QA_ME_TRACE_FD";
const ORIGIN_ENV: &str = "NGTEAMS_QA_ME_ORIGIN";
const TROUTER_ENV: &str = "NGTEAMS_QA_ME_TROUTER";
const TOKEN_ENV: &str = "NGTEAMS_QA_ME_TOKEN";
const DIR_ENV: &str = "NGTEAMS_QA_ME_DIR";
const CA_ENV: &str = "NGTEAMS_QA_ME_CA";
/// `read-only`, `flag-off` (`NGTEAMS_BOTS` unset) or `own` (an own server's account); else the ordinary app.
const MODE_ENV: &str = "NGTEAMS_QA_ME_MODE";
/// The file in the app's directory the parent asks the child about its views through.
const ASK: &str = "qa-me-ask";
/// The file in the app's directory the child reads the theme to show from (`light` or `dark`).
const THEME_FILE: &str = "theme";
const STARTUP: Duration = Duration::from_secs(90);
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(100);
/// How long the screen gets to settle before a shot.
const SETTLE: Duration = Duration::from_millis(900);
/// How long a click that must send nothing gets to send it anyway.
const QUIET: Duration = Duration::from_millis(1500);

/// Teams Light's surface, its borders, a field's fill, Teams' red and its brand.
const SURFACE: [u8; 3] = [255, 255, 255];
const BORDER: [u8; 3] = [224, 224, 224];
const FILL: [u8; 3] = [243, 244, 250];
const OFF_FILL: [u8; 3] = [243, 243, 243];
const DANGER: [u8; 3] = [196, 49, 75];
const BRAND: [u8; 3] = [91, 95, 199];
/// The lit row of a list (`palette.hover`).
const HOVER: [u8; 3] = [240, 240, 240];
/// Where the chat's pane starts on the 1280-wide screen: the rail and the chat list are left of it.
const PANE_X: u32 = 455;
/// The compose box's "+" (Actions and apps) on the 1280-wide screen, a one-line box.
const PLUS: (u32, u32) = (1194, 749);

/// The fixture's apps (`teams_mock::extension_apps_answer`) and their bots.
const FINDER_APP: &str = "00000000-0000-4000-8000-00000000e001";
const FINDER_BOT: &str = "28:00000000-0000-4000-8000-00000000eb01";
const FINDER: &str = "Made-up Finder";
const PRAISE: &str = "Made-up Praise";
const TASKS_BOT: &str = "28:00000000-0000-4000-8000-00000000eb03";
const TICKETS: &str = "Made-up Tickets";
/// The made-up search app with a very long name, and its bot (its searches find nothing).
const LONG_APP: &str = "00000000-0000-4000-8000-00000000f0ff";
const LONG_BOT_ID: &str = "00000000-0000-4000-8000-00000000fbff";
const LONG_NAME: &str = "Made-up Quarterly Planning, Retrospective and Very Long Named Search Assistant for Everyone";
/// How many more made-up search apps the account has.
const EXTRAS: u32 = 8;

const ADAPTIVE: &str = "application/vnd.microsoft.card.adaptive";
const THUMBNAIL: &str = "application/vnd.microsoft.card.thumbnail";
const TOPIC: &str = "Made-up extensions chat";
const ASKED: &str = "Can someone file the made-up order 1042 for Friday?";
const NAME_TYPED: &str = "Made-up order 1045";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_message_extensions_window: {error}");
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
    let Some(sandbox) = sandbox_or_skip("ngteams message extensions", &options)? else {
        return Ok(());
    };
    let only = std::env::var("QA_ME_ONLY").unwrap_or_default();
    for mode in ["walk", "no-apps", "read-only", "flag-off", "own"] {
        if !only.is_empty() && only != mode {
            continue;
        }
        step(&format!("--- run {mode} ---"));
        let dir = tempfile::tempdir()?;
        let outcome = match mode {
            "walk" => {
                let service = Service::start(&runtime, true)?;
                walk(&sandbox, &runtime, &service, dir.path())
            }
            "no-apps" => {
                let service = Service::start(&runtime, false)?;
                no_apps(&sandbox, &runtime, &service, dir.path())
            }
            "read-only" | "flag-off" => {
                let service = Service::start(&runtime, true)?;
                hidden(&sandbox, &runtime, &service, dir.path(), mode)
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
    say("ngteams message extensions: ok");
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
        ctx.run.open(&service.chat)?;
        ctx.run.until_chat("the chat's messages", |state| {
            state["items"].as_array().is_some_and(|items| items.len() >= 2)
        })?;
        thread::sleep(SETTLE);
        ctx.shots("chat")?;
        ctx.more_actions()?;
        ctx.task_fetch()?;
        ctx.flyout()?;
        ctx.search()?;
        ctx.card_sends()?;
        ctx.no_results()?;
        ctx.late_answers()?;
        ctx.action_dialog()?;
        ctx.dialog_ends()?;
        ctx.web_dialog()?;
        ctx.narrow()
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
        self.run.shot(self.sandbox, &format!("qa-me-{name}-light.png"))
    }

    /// The shots of `name` in Teams Light, then Teams Dark, then back to Light.
    fn shots(&mut self, name: &str) -> SandboxResult<PathBuf> {
        let light = self.shot(name)?;
        self.run.theme(true)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, &format!("qa-me-{name}-dark.png"))?;
        self.run.theme(false)?;
        thread::sleep(SETTLE);
        Ok(light)
    }

    /// A shot of `name` once `region` stops changing (a caret's blink aside), so that keys still on their way show.
    fn steady_shot(&mut self, name: &str, region: Rect) -> SandboxResult<PathBuf> {
        let until = Instant::now() + WAIT;
        let mut last = load(&self.shot(name)?)?;
        loop {
            thread::sleep(SETTLE);
            let path = self.shot(name)?;
            let next = load(&path)?;
            let steady = changed_box(&last, &next, region).is_none_or(|changed| changed.width() <= 3);
            if steady || Instant::now() > until {
                return Ok(path);
            }
            last = next;
        }
    }

    /// The invokes the mock got so far (each request to `v1/agents/<bot>/invoke`, the refused ones too).
    fn invokes(&self) -> Vec<Value> {
        invokes_of(self.runtime, &self.service.mock)
    }

    /// Waits for one more invoke than `before`; that invoke.
    fn next_invoke(&self, before: usize, what: &str) -> SandboxResult<Value> {
        let found = wait_until(WAIT, || {
            let all = self.invokes();
            (all.len() > before).then(|| all[before].clone())
        });
        found.ok_or_else(|| format!("{what}: no invoke reached the app").into())
    }

    /// Checks that no invoke came after `before` within [`QUIET`].
    fn no_invoke(&self, before: usize, what: &str) {
        thread::sleep(QUIET);
        let after = self.invokes().len();
        expect(after == before, || {
            format!("{what}: {} invokes reached an app, expected none", after - before)
        });
    }

    /// The messages a sent to the chat so far, as the service got them.
    fn sent(&self) -> Vec<Value> {
        sent_to(self.runtime, &self.service.mock, &self.service.chat)
    }

    /// The chat's state as the child tells it.
    fn state(&mut self) -> SandboxResult<Value> {
        self.run.state_in("chat")
    }

    fn key(&mut self, key: u32) -> SandboxResult {
        self.run.input.key(key)?;
        thread::sleep(Duration::from_millis(250));
        Ok(())
    }

    fn type_text(&mut self, text: &str) -> SandboxResult {
        self.run.input.type_text(text)?;
        thread::sleep(Duration::from_millis(300));
        Ok(())
    }

    /// Empties the field that has the keyboard (Ctrl+A, Delete).
    fn clear_field(&mut self) -> SandboxResult {
        let a = self.run.letter('a')?;
        self.run.input.key_with(&[Modifier::Ctrl], a)?;
        self.key(keys::DELETE)
    }
}

// --- Actions and apps ---

/// A flyout's first list row's centre is this far under the flyout's top.
const FIRST_ROW: u32 = 62;

impl Walk<'_> {
    /// A click on the box's "+": once the account's apps are read, the flyout's bounds on the screen.
    fn open_flyout(&mut self, name: &str) -> SandboxResult<Rect> {
        let before = load(&self.shot(&format!("{name}-closed"))?)?;
        self.run.click(PLUS.0, PLUS.1)?;
        let opened = self.run.until_chat("the flyout's apps read", |state| {
            state["compose"]["flyout"]["loaded"] == true
        });
        if let Err(error) = opened {
            return Err(format!(
                "a click on the box's + ({}, {}) opens no flyout: {error}",
                PLUS.0, PLUS.1
            )
            .into());
        }
        thread::sleep(SETTLE);
        let after = load(&self.shot(name)?)?;
        let region = Rect {
            x0: 0,
            y0: 0,
            x1: after.width(),
            y1: PLUS.1.saturating_sub(16),
        };
        changed_box(&before, &after, region).ok_or_else(|| format!("no flyout on the screen ({name})").into())
    }

    /// The names the open flyout lists.
    fn listed(&mut self) -> SandboxResult<Vec<String>> {
        Ok(strings(&self.state()?["compose"]["flyout"]["listed"]))
    }

    /// Opens the flyout, types `words` in its search and opens the first app with Enter; the flyout.
    fn open_app(&mut self, words: &str, name: &str) -> SandboxResult<Rect> {
        let flyout = self.open_flyout(name)?;
        self.type_text(words)?;
        self.key(keys::ENTER)?;
        Ok(flyout)
    }

    fn flyout(&mut self) -> SandboxResult {
        let flyout = self.open_flyout("flyout")?;
        step(&format!("the flyout: {flyout:?}"));
        let first_lit = lit_in(&load(&self.shots("flyout")?)?, flyout);
        let listed = self.listed()?;
        let mut wanted = vec![FINDER.to_owned(), PRAISE.to_owned()];
        wanted.extend((1..=EXTRAS).map(extra_name));
        wanted.push(LONG_NAME.to_owned());
        expect(listed == wanted, || {
            format!("the flyout lists {listed:?}, expected {wanted:?} (no message-only app, no store offer)")
        });
        // the keyboard goes past the box's own rows (the AI tools) and down the many apps: the lit row stays in view
        let actions = self.state()?["compose"]["flyout"]["actions"]
            .as_u64()
            .ok_or("the flyout tells no action rows")?;
        let downs = usize::try_from(actions)? + wanted.len() - 2;
        for _ in 0..downs {
            self.key(keys::DOWN)?;
        }
        let down_lit = lit_in(&load(&self.shots("flyout-down")?)?, flyout);
        expect(first_lit < 2000 || down_lit > 500, || {
            format!(
                "Down {downs} times: the lit row is out of the list's view, the list does not scroll to it ({first_lit} \
                 lit px on the first row, {down_lit} after; {})",
                shots_dir()
                    .map(|dir| dir.join("qa-me-flyout-down-light.png").display().to_string())
                    .unwrap_or_default()
            )
        });
        self.key(keys::ENTER)?;
        let state = self.run.until_chat("the app the keyboard chose", |state| {
            state["compose"]["flyout"]["pane"].is_array()
        })?;
        let chosen = state["compose"]["flyout"]["pane"][0]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        expect(chosen == wanted[wanted.len() - 2], || {
            format!(
                "Down {downs} times and Enter opens {chosen}, expected {}",
                wanted[wanted.len() - 2]
            )
        });
        self.shot("flyout-extra")?;
        self.key(keys::ESC)?;
        let state = self.state()?;
        expect(
            state["compose"]["flyout"]["loaded"] == true && state["compose"]["flyout"]["pane"].is_null(),
            || {
                format!(
                    "Escape in an app's pane does not go back to the list: {}",
                    state["compose"]
                )
            },
        );
        self.key(keys::ESC)?;
        let state = self.state()?;
        expect(state["compose"]["flyout"].is_null(), || {
            format!("Escape in the list does not close the flyout: {}", state["compose"])
        });
        self.filter()?;
        self.more_apps(flyout)?;
        step("Actions and apps: the apps, the keys, the search, Get more apps");
        Ok(())
    }

    /// The search for apps: one app, no app, the long name.
    fn filter(&mut self) -> SandboxResult {
        self.open_flyout("flyout-again")?;
        self.type_text("finder")?;
        let listed = self.listed()?;
        expect(listed == [FINDER], || format!("\"finder\" lists {listed:?}"));
        self.shot("flyout-filter")?;
        self.clear_field()?;
        self.type_text("zzz")?;
        let listed = self.listed()?;
        expect(listed.is_empty(), || format!("\"zzz\" lists {listed:?}"));
        self.shots("flyout-no-match")?;
        self.clear_field()?;
        self.type_text("retrospective")?;
        let listed = self.listed()?;
        expect(listed == [LONG_NAME], || format!("\"retrospective\" lists {listed:?}"));
        self.shots("flyout-long")?;
        self.key(keys::ESC)?;
        let state = self.state()?;
        expect(state["compose"]["flyout"].is_null(), || {
            "Escape with words in the search does not close the flyout".to_owned()
        });
        Ok(())
    }

    /// "Get more apps" opens the store.
    fn more_apps(&mut self, flyout: Rect) -> SandboxResult {
        self.open_flyout("flyout-more")?;
        self.run
            .click(flyout.x1.saturating_sub(70), flyout.y1.saturating_sub(30))?;
        thread::sleep(SETTLE);
        let state = self.state()?;
        expect(state["main"].as_str().is_some_and(|main| main.contains("Apps")), || {
            format!(
                "Get more apps shows {} (the flyout {})",
                state["main"], state["compose"]["flyout"]
            )
        });
        self.shot("store")?;
        self.run.open(&self.service.chat)?;
        thread::sleep(SETTLE);
        Ok(())
    }
}

// --- a search app and its cards ---

impl Walk<'_> {
    fn search(&mut self) -> SandboxResult {
        let before = self.invokes().len();
        self.open_app("finder", "search-flyout")?;
        let invoke = self.next_invoke(before, "the Finder's search at once")?;
        let body = &invoke["body"];
        expect(
            body["name"] == "composeExtension/query"
                && body["value"]["commandId"] == "findRecords"
                && invoke["path"]
                    .as_str()
                    .is_some_and(|path| decoded(path).contains(FINDER_BOT)),
            || format!("the Finder's search at once sent {invoke}"),
        );
        let state = self.run.until_chat("the Finder's results", |state| {
            state["compose"]["flyout"]["pane"][2]
                .as_array()
                .is_some_and(|found| found.len() == 3)
        })?;
        let pane = &state["compose"]["flyout"]["pane"];
        expect(
            strings(&pane[1]) == ["Find records", "New record"]
                && strings(&pane[2]) == ["Record 1042", "Record 1043", "Record 1044"],
            || format!("the Finder's pane shows {pane}"),
        );
        self.shots("search")?;
        let before = self.invokes().len();
        self.type_text("1042")?;
        thread::sleep(Duration::from_millis(1500));
        let typed: Vec<Value> = self.invokes()[before..].to_vec();
        expect(typed.len() == 1, || {
            format!(
                "typing \"1042\" sent {} searches, expected 1 after the words rest",
                typed.len()
            )
        });
        if let Some(query) = typed.last() {
            expect(query["body"]["value"].to_string().contains("1042"), || {
                format!("the search for \"1042\" sent {}", query["body"])
            });
        }
        self.shot("search-typed")?;
        step("a search app: its results at once, a search with words");
        Ok(())
    }

    /// A result into the box and sent with words; one sent alone; one taken out with its close mark.
    fn card_sends(&mut self) -> SandboxResult {
        // the pane of the search above is open: the third result asks the app for its card
        let before = self.invokes().len();
        self.key(keys::DOWN)?;
        self.key(keys::DOWN)?;
        self.key(keys::ENTER)?;
        let invoke = self.next_invoke(before, "the chosen result's selectItem")?;
        expect(
            invoke["body"]["name"] == "composeExtension/selectItem"
                && invoke["body"]["value"]["itemId"] == "made-up-1044",
            || format!("the chosen result sent {}", invoke["body"]),
        );
        let state = self.run.until_chat("the card in the box", |state| {
            state["compose"]["cards"]
                .as_array()
                .is_some_and(|cards| cards.len() == 1)
        })?;
        expect(state["compose"]["flyout"].is_null(), || {
            "the flyout stays open after a result".to_owned()
        });
        self.shots("card-in-box")?;
        // the box keeps the keyboard once the card is in it
        let sent = self.sent().len();
        self.type_text("Here is the record")?;
        self.key(keys::ENTER)?;
        let message =
            wait_until(WAIT, || self.sent().get(sent).cloned()).ok_or("the message with the card never went")?;
        let cards = sent_cards(&message);
        let content = message["content"].as_str().unwrap_or_default();
        expect(
            cards.len() == 1
                && cards[0]["appId"] == FINDER_APP
                && cards[0]["content"].to_string().contains("Synthetic result, chosen")
                && content.contains("Here is the record")
                && content.contains("InputExtension"),
            || format!("the message with the card sent {message}"),
        );
        self.run.until_chat("the box emptied", |state| {
            state["compose"]["cards"].as_array().is_some_and(Vec::is_empty) && state["compose"]["value"] == ""
        })?;
        // the sent message can come into the list a moment after the box empties
        let shown = self.run.until_chat("the sent message's card", |state| {
            state["items"].as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["own"] == true && item["app_cards"].as_u64() == Some(1))
            })
        });
        expect(shown.is_ok(), || {
            format!(
                "the sent message shows no app card: {}",
                shown.as_ref().err().map(ToString::to_string).unwrap_or_default()
            )
        });
        self.shots("card-sent")?;
        self.card_alone()?;
        self.card_removed()
    }

    /// A result with no `invoke` tap goes into the box as it is, and Enter sends the card alone.
    fn card_alone(&mut self) -> SandboxResult {
        let before = self.invokes().len();
        self.open_app("finder", "alone-flyout")?;
        self.run.until_chat("the results", |state| {
            state["compose"]["flyout"]["pane"][2]
                .as_array()
                .is_some_and(|found| found.len() == 3)
        })?;
        let searched = self.invokes().len();
        self.key(keys::ENTER)?;
        self.run.until_chat("the card in the box", |state| {
            state["compose"]["cards"]
                .as_array()
                .is_some_and(|cards| cards.len() == 1)
        })?;
        expect(self.invokes().len() == searched, || {
            format!(
                "a result with no invoke tap asked the app: {:?}",
                &self.invokes()[before..]
            )
        });
        let sent = self.sent().len();
        self.key(keys::ENTER)?;
        let message = wait_until(WAIT, || self.sent().get(sent).cloned()).ok_or("the card alone never went")?;
        let cards = sent_cards(&message);
        expect(
            cards.len() == 1 && cards[0]["content"].to_string().contains("Record 1042"),
            || format!("the card alone sent {message}"),
        );
        thread::sleep(SETTLE);
        self.shot("card-alone-sent")?;
        Ok(())
    }

    /// The card's close mark takes it out of the box, and nothing is sent.
    fn card_removed(&mut self) -> SandboxResult {
        self.open_app("finder", "remove-flyout")?;
        self.run.until_chat("the results", |state| {
            state["compose"]["flyout"]["pane"][2]
                .as_array()
                .is_some_and(|found| found.len() == 3)
        })?;
        self.key(keys::DOWN)?;
        self.key(keys::ENTER)?;
        self.run.until_chat("the card in the box", |state| {
            state["compose"]["cards"]
                .as_array()
                .is_some_and(|cards| cards.len() == 1)
        })?;
        let shot = self.shot("card-to-remove")?;
        let Some(card) = cards_in(&load(&shot)?).into_iter().max_by_key(|card| card.y0) else {
            note(format!("no card found in the box to take out ({})", shot.display()));
            return Ok(());
        };
        let sent = self.sent().len();
        self.run.click(card.x1.saturating_sub(24), card.y0 + 14)?;
        thread::sleep(SETTLE);
        let state = self.state()?;
        let shot = self.shot("card-removed")?;
        expect(state["compose"]["cards"].as_array().is_some_and(Vec::is_empty), || {
            format!(
                "a click on the card's close mark leaves {} ({})",
                state["compose"]["cards"],
                shot.display()
            )
        });
        thread::sleep(QUIET);
        expect(self.sent().len() == sent, || {
            "taking the card out sent a message".to_owned()
        });
        step("cards: sent with words, alone, and taken out");
        Ok(())
    }

    /// A search that finds nothing says so, the app's long name in its pane.
    fn no_results(&mut self) -> SandboxResult {
        let before = self.invokes().len();
        self.open_app("retrospective", "long-flyout")?;
        self.next_invoke(before, "the long app's search at once")?;
        thread::sleep(SETTLE);
        let state = self.state()?;
        let pane = &state["compose"]["flyout"]["pane"];
        expect(pane[0] == LONG_NAME && strings(&pane[2]).is_empty(), || {
            format!("the long app's pane shows {pane}")
        });
        self.shots("no-results")?;
        self.key(keys::ESC)?;
        self.key(keys::ESC)?;
        step("a search with no results");
        Ok(())
    }

    /// The Finder's next invoke is answered with `body` only after [`LATE`].
    fn hold_finder_answer(&self, body: &Value) {
        self.runtime.block_on(
            self.service
                .mock
                .answer_next_after_with("POST", FINDER_INVOKE_END, 200, body, LATE),
        );
    }

    /// From the open Finder pane back to the list, then into the long app's pane; the time its pane shows.
    fn leave_for_long_app(&mut self, what: &str) -> SandboxResult<Instant> {
        self.key(keys::ESC)?;
        self.clear_field()?;
        self.type_text("retrospective")?;
        self.key(keys::ENTER)?;
        self.run.until_chat(&format!("{what}: the long app's pane"), |state| {
            state["compose"]["flyout"]["pane"][0] == LONG_NAME
        })?;
        Ok(Instant::now())
    }

    /// Waits until the held answer that went at `sent` came back, and a moment more.
    fn after_held_answer(sent: Instant, opened: Instant, what: &str) {
        expect(opened < sent + LATE, || {
            format!(
                "{what}: the long app's pane opened {:?} after the held ask, not before its answer came ({LATE:?})",
                opened.duration_since(sent)
            )
        });
        thread::sleep((sent + LATE + QUIET).saturating_duration_since(Instant::now()));
    }

    /// An app's answer that comes after its pane is gone (Escape, then a different app) changes nothing in the pane
    /// that is open now: a late search fills no results, a late `selectItem` puts no card into the box.
    fn late_answers(&mut self) -> SandboxResult {
        self.hold_finder_answer(&results(&[("Late record 9001", None)]));
        let before = self.invokes().len();
        self.open_app("finder", "late-search-flyout")?;
        self.next_invoke(before, "the Finder's held search")?;
        let sent = Instant::now();
        let opened = self.leave_for_long_app("a late search")?;
        Self::after_held_answer(sent, opened, "a late search");
        let state = self.state()?;
        let pane = &state["compose"]["flyout"]["pane"];
        let shot = self.shots("late-search")?;
        expect(pane[0] == LONG_NAME && strings(&pane[2]).is_empty(), || {
            format!(
                "the Finder's late search answer fills the long app's pane: {pane} ({})",
                shot.display()
            )
        });
        self.key(keys::ESC)?;
        self.key(keys::ESC)?;

        self.open_app("finder", "late-select-flyout")?;
        self.run.until_chat("the Finder's results", |state| {
            state["compose"]["flyout"]["pane"][2]
                .as_array()
                .is_some_and(|found| found.len() == 3)
        })?;
        let selected = teams_mock::bot_answer("selected").ok_or("no fixture answer selected")?;
        self.hold_finder_answer(&selected);
        let before = self.invokes().len();
        self.key(keys::DOWN)?;
        self.key(keys::DOWN)?;
        self.key(keys::ENTER)?;
        let invoke = self.next_invoke(before, "the held selectItem")?;
        let sent = Instant::now();
        expect(invoke["body"]["name"] == "composeExtension/selectItem", || {
            format!("the third result sent {}", invoke["body"])
        });
        let opened = self.leave_for_long_app("a late selectItem")?;
        Self::after_held_answer(sent, opened, "a late selectItem");
        let state = self.state()?;
        let shot = self.shots("late-select")?;
        expect(
            state["compose"]["cards"].as_array().is_some_and(Vec::is_empty)
                && state["compose"]["flyout"]["pane"][0] == LONG_NAME,
            || {
                format!(
                    "the Finder's late selectItem answer changes the box or the pane: cards {}, pane {} ({})",
                    state["compose"]["cards"],
                    state["compose"]["flyout"]["pane"],
                    shot.display()
                )
            },
        );
        self.key(keys::ESC)?;
        self.key(keys::ESC)?;
        step("late answers: an app's answer after its pane is gone changes nothing");
        Ok(())
    }
}

/// How long the mock holds back an app's answer in [`Walk::late_answers`].
const LATE: Duration = Duration::from_secs(6);
/// The end of the path of an invoke of the Finder's bot.
const FINDER_INVOKE_END: &str = "00000000eb01/invoke";

// --- an action app's dialog ---

/// The flyout's left edge: it opens up and left from the "+", 340 px wide.
const FLYOUT_X0: u32 = 870;
/// The flyout's "New record" tab, from the flyout's left edge and its top.
const NEW_RECORD_TAB: (u32, u32) = (128, 54);

impl Walk<'_> {
    /// Opens the Finder's "New record" dialog from the flyout; once its card shows, the dialog's bounds.
    fn open_new_record(&mut self, name: &str) -> SandboxResult<Rect> {
        self.open_app("finder", &format!("{name}-flyout"))?;
        self.run.until_chat("the Finder's pane", |state| {
            state["compose"]["flyout"]["pane"].is_array()
        })?;
        thread::sleep(SETTLE);
        let pane = load(&self.shot(&format!("{name}-pane"))?)?;
        let top = flyout_top(&pane).ok_or("no flyout's top on the screen")?;
        self.run.click(FLYOUT_X0 + NEW_RECORD_TAB.0, top + NEW_RECORD_TAB.1)?;
        self.until_dialog("the New record dialog", |dialog| dialog["stage"] == "card")?;
        self.dialog_rect(&format!("{name}-dialog"))
    }

    /// Waits for the dialog that `wanted` takes.
    fn until_dialog(&mut self, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let state = self.run.until_chat(what, |state| wanted(&state["dialog"]))?;
        Ok(state["dialog"].clone())
    }

    /// The open dialog's bounds: the Teams Light surface over the dimmed window.
    fn dialog_rect(&mut self, name: &str) -> SandboxResult<Rect> {
        thread::sleep(SETTLE);
        let shot = self.shot(name)?;
        surface_box(&load(&shot)?).ok_or_else(|| format!("no dialog on the screen ({})", shot.display()).into())
    }

    fn action_dialog(&mut self) -> SandboxResult {
        let before = self.invokes().len();
        let dialog = self.open_new_record("new-record")?;
        step(&format!("the dialog: {dialog:?}"));
        // the Finder's search runs at once as its pane opens; the dialog's fetch comes after it
        let invoke = self.next_invoke(before + 1, "the New record dialog's fetch")?;
        expect(
            invoke["body"]["name"] == "composeExtension/fetchTask"
                && invoke["body"]["value"]["commandId"] == "newRecord"
                && invoke["body"]["value"]["commandContext"] == "compose",
            || format!("the New record dialog's fetch sent {}", invoke["body"]),
        );
        self.shots("dialog")?;
        let shot = load(&self.shot("dialog-fields")?)?;
        let fields = boxes_in(&shot, dialog);
        step(&format!("the dialog's boxes: {fields:?}"));
        let [name, .., create, cancel] = fields.as_slice() else {
            return Err(format!("the dialog shows {} fields and buttons, expected 4", fields.len()).into());
        };
        let (name, create, cancel) = (*name, *create, *cancel);
        // Create with the required name empty
        let red = danger_in(&shot, dialog);
        let before = self.invokes().len();
        self.run.click(create.centre().0, create.centre().1)?;
        self.no_invoke(before, "Create with the required name empty");
        let required_shot = self.shot("dialog-required")?;
        let required = load(&required_shot)?;
        let after = danger_in(&required, dialog);
        expect(after > red + 20, || {
            format!(
                "Create with the required name empty shows no error ({red} -> {after} red px, {})",
                required_shot.display()
            )
        });
        self.run.click(name.centre().0, name.centre().1)?;
        self.type_text(NAME_TYPED)?;
        self.steady_shot("dialog-typed", name)?;
        let shot = load(&self.shots("dialog-typed")?)?;
        let typed = changed_box(&required, &shot, name).is_some();
        let mut dialog = dialog;
        if !typed {
            let state = self.state()?;
            note(format!(
                "typing \"{NAME_TYPED}\" in the dialog's Name field shows nothing in it, also one key at a time (the \
                 box behind holds {:?}; {})",
                state["compose"]["value"],
                shots_dir()?.join("qa-me-dialog-typed-light.png").display()
            ));
            // a second try: a click in the field, a pause, then one key at a time
            self.run.click(name.centre().0, name.centre().1)?;
            thread::sleep(SETTLE);
            for letter in "abc".chars() {
                let key = self.run.letter(letter)?;
                self.key(key)?;
            }
            self.shot("dialog-typed-again")?;
            // the next steps go on with a dialog whose name is not required
            self.key(keys::ESC)?;
            self.service.mock.script_bot(
                FINDER_BOT,
                FINDER,
                &[("composeExtension/fetchTask", BotReply::answer(record_dialog(false)))],
            );
            dialog = self.open_new_record("optional")?;
        }
        let shot = load(&self.shot("dialog-to-create")?)?;
        let create = boxes_in(&shot, dialog).iter().rev().nth(1).copied().unwrap_or(create);
        let before = self.invokes().len();
        self.run.click(create.centre().0, create.centre().1)?;
        let invoke = self.next_invoke(before, "Create")?;
        expect(
            invoke["body"]["name"] == "composeExtension/submitAction"
                && invoke["body"]["value"]["commandId"] == "newRecord"
                && (!typed || invoke["body"]["value"]["data"]["name"] == NAME_TYPED)
                && invoke["body"]["value"]["data"]["step"] == "create",
            || format!("Create sent {}", invoke["body"]),
        );
        thread::sleep(SETTLE);
        let next = self.state()?["dialog"].clone();
        expect(next["stage"] == "card" && next["error"].is_null(), || {
            format!("the next step shows {next}")
        });
        self.shots("dialog-next")?;
        let _ = cancel;
        self.answers()
    }

    /// The next step's Send: an error under the card, then the app's message and its Close.
    fn answers(&mut self) -> SandboxResult {
        let service = self.service;
        service.mock.script_bot(
            FINDER_BOT,
            FINDER,
            &[("composeExtension/submitAction", fixture("error")?)],
        );
        let dialog = self.dialog_rect("next-dialog")?;
        let send = last_button(&load(&self.shot("next-send")?)?, dialog)?;
        let before = self.invokes().len();
        self.run.click(send.centre().0, send.centre().1)?;
        self.next_invoke(before, "Send of the next step")?;
        let shown = self.until_dialog("the app's error", |dialog| !dialog["error"].is_null())?;
        expect(
            shown["error"] == "This request was already answered." && shown["stage"] == "card",
            || format!("the app's error shows as {shown}"),
        );
        self.shots("dialog-error")?;
        service.mock.script_bot(
            FINDER_BOT,
            FINDER,
            &[("composeExtension/submitAction", fixture("taskMessage")?)],
        );
        let dialog = self.dialog_rect("error-dialog")?;
        let send = last_button(&load(&self.shot("error-send")?)?, dialog)?;
        self.run.click(send.centre().0, send.centre().1)?;
        let shown = self.until_dialog("the app's message", |dialog| dialog["stage"] == "note")?;
        expect(shown["error"].is_null(), || {
            format!(
                "the app's message after an error still shows the old error {} under it ({})",
                shown["error"],
                shots_dir()
                    .map(|dir| dir.join("qa-me-dialog-note-light.png").display().to_string())
                    .unwrap_or_default()
            )
        });
        self.shots("dialog-note")?;
        let dialog = self.dialog_rect("note-dialog")?;
        let close = last_button(&load(&self.shot("note-close")?)?, dialog)?;
        self.run.click(close.centre().0, close.centre().1)?;
        thread::sleep(SETTLE);
        let state = self.state()?;
        expect(state["dialog"].is_null(), || {
            format!("Close under the app's message leaves {}", state["dialog"])
        });
        step("an action app: required field, next step, error, message");
        Ok(())
    }

    /// A result card into the box, then Escape, Cancel and the head's Close.
    fn dialog_ends(&mut self) -> SandboxResult {
        let service = self.service;
        service
            .mock
            .script_bot(FINDER_BOT, FINDER, &[("composeExtension/submitAction", made_result())]);
        let dialog = self.open_new_record("result")?;
        let shot = load(&self.shot("result-fields")?)?;
        let fields = boxes_in(&shot, dialog);
        let (Some(name), Some(create)) = (fields.first().copied(), fields.iter().rev().nth(1).copied()) else {
            return Err(format!("the dialog shows {} boxes", fields.len()).into());
        };
        self.run.click(name.centre().0, name.centre().1)?;
        self.type_text(NAME_TYPED)?;
        self.run.click(create.centre().0, create.centre().1)?;
        let state = self.run.until_chat("the dialog's card in the box", |state| {
            state["dialog"].is_null()
                && state["compose"]["cards"]
                    .as_array()
                    .is_some_and(|cards| cards.len() == 1)
        });
        match state {
            Ok(state) => expect(
                state["compose"]["cards"][0].to_string().contains("Record 1045 made"),
                || format!("the dialog's card in the box is {}", state["compose"]["cards"]),
            ),
            Err(error) => note(format!("a result of Create does not go into the box: {error}")),
        }
        self.shots("dialog-card-in-box")?;
        let sent = self.sent().len();
        self.key(keys::ENTER)?;
        let message = wait_until(WAIT, || self.sent().get(sent).cloned());
        expect(
            message.as_ref().is_some_and(|message| sent_cards(message).len() == 1),
            || format!("the dialog's card was not sent: {message:?}"),
        );
        self.dialog_closes()
    }

    /// Escape, the app's Cancel and the head's Close end the dialog.
    fn dialog_closes(&mut self) -> SandboxResult {
        let service = self.service;
        // Escape
        let before = self.invokes().len();
        self.open_new_record("escape")?;
        let fetched = self.invokes().len();
        self.key(keys::ESC)?;
        let state = self.state()?;
        expect(state["dialog"].is_null(), || {
            format!("Escape leaves the dialog open: {}", state["dialog"])
        });
        self.no_invoke(fetched, "Escape in the dialog");
        expect(fetched == before + 2, || {
            format!(
                "opening the dialog (its search at once, its fetch) sent {} invokes",
                fetched - before
            )
        });
        // Cancel: the app's own button, with no inputs checked
        service.mock.script_bot(
            FINDER_BOT,
            FINDER,
            &[
                ("composeExtension/submitAction", BotReply::done()),
                ("composeExtension/fetchTask", BotReply::answer(record_dialog(true))),
            ],
        );
        let dialog = self.open_new_record("cancel")?;
        let shot = load(&self.shot("cancel-fields")?)?;
        let cancel = boxes_in(&shot, dialog)
            .last()
            .copied()
            .ok_or("no Cancel in the dialog")?;
        let before = self.invokes().len();
        self.run.click(cancel.centre().0, cancel.centre().1)?;
        let invoke = wait_until(WAIT, || self.invokes().get(before).cloned());
        thread::sleep(SETTLE);
        let state = self.state()?;
        let shot = self.shot("cancel-after")?;
        expect(
            invoke
                .as_ref()
                .is_some_and(|invoke| invoke["body"]["value"]["data"]["step"] == "cancel")
                && state["dialog"].is_null(),
            || {
                format!(
                    "Cancel (no inputs checked) with the name empty: invoke {invoke:?}, the dialog {} ({})",
                    state["dialog"],
                    shot.display()
                )
            },
        );
        if !state["dialog"].is_null() {
            self.key(keys::ESC)?;
        }
        // the head's Close
        let dialog = self.open_new_record("close")?;
        let before = self.invokes().len();
        self.run.click(dialog.x1.saturating_sub(38), dialog.y0 + 40)?;
        thread::sleep(SETTLE);
        let state = self.state()?;
        expect(state["dialog"].is_null(), || {
            format!("the head's Close leaves {}", state["dialog"])
        });
        self.no_invoke(before, "the head's Close");
        step("a dialog's card into the box; Escape, Cancel and Close");
        Ok(())
    }

    /// An app whose dialog is a web page: the stand-in and its link.
    fn web_dialog(&mut self) -> SandboxResult {
        let before = self.invokes().len();
        self.open_flyout("praise-flyout")?;
        self.type_text("praise")?;
        thread::sleep(SETTLE);
        let filtered = load(&self.shot("praise-filtered")?)?;
        let top = flyout_top(&filtered).ok_or("no flyout's top on the screen")?;
        // a click on its row, as a user does
        self.run.click(FLYOUT_X0 + 120, top + FIRST_ROW)?;
        self.until_dialog("the web page's stand-in", |dialog| dialog["stage"] == "web")?;
        self.no_invoke(before, "an app whose dialog is a static web page");
        self.shots("web")?;
        let dialog = self.dialog_rect("web-dialog")?;
        let shot = load(&self.shot("web-link")?)?;
        let Some(link) = brand_button(&shot, dialog) else {
            note("the web page's stand-in shows no Open in Teams on the web".to_owned());
            return self.key(keys::ESC);
        };
        let opened = self.run.click_opens(self.sandbox, link.centre().0, link.centre().1)?;
        expect(opened.starts_with("https://teams.microsoft.com/"), || {
            format!("Open in Teams on the web opened {opened}")
        });
        thread::sleep(SETTLE);
        let state = self.state()?;
        expect(state["dialog"].is_null(), || {
            "the stand-in stays open after its link".to_owned()
        });
        step("a web page's dialog: the stand-in and its link");
        Ok(())
    }
}

// --- a message's More actions, a card's task/fetch ---

impl Walk<'_> {
    /// Hovers the message whose bubble is `bubble` and clicks its hover bar's More options; the menu's bounds. With
    /// `fixture_apps`, the menu's read of the account's apps gets the fixture's whole answer (its store offer too).
    fn more_options(
        &mut self,
        bubble: Rect,
        name: &str,
        fixture_apps: Option<&Service>,
    ) -> SandboxResult<Option<Rect>> {
        let still = load(&self.shot(&format!("{name}-still"))?)?;
        self.run.input.move_to(bubble.x0 + 40, bubble.y0 + 12)?;
        thread::sleep(SETTLE);
        let png = self.sandbox.screenshot("qa-me-hover.png", false)?;
        fs::copy(&png, shots_dir()?.join(format!("qa-me-{name}-hover-light.png")))?;
        let hover = load(&png)?;
        let region = Rect {
            x0: PANE_X,
            y0: bubble.y0.saturating_sub(70),
            x1: 1280,
            y1: bubble.y0 + 20,
        };
        let Some(bar) = changed_box(&still, &hover, region) else {
            note(format!("no hover bar over the message ({})", png.display()));
            return Ok(None);
        };
        if let Some(service) = fixture_apps {
            self.runtime.block_on(service.mock.answer_next(
                "POST",
                "users/apps/aggregatedEntitlements",
                200,
                &teams_mock::extension_apps_answer(),
            ));
        }
        self.run.click(bar.x1.saturating_sub(14), bar.y0 + bar.height() / 2)?;
        thread::sleep(SETTLE);
        let menu = load(&self.shot(&format!("{name}-menu"))?)?;
        let whole = Rect {
            x0: 0,
            y0: 0,
            x1: menu.width(),
            y1: menu.height(),
        };
        Ok(changed_box(&hover, &menu, whole).filter(|menu| menu.height() > 40))
    }

    /// Opens b's message's More options, then its More actions; the list's bounds.
    fn open_more_actions(&mut self, name: &str) -> SandboxResult<Option<Rect>> {
        let shot = load(&self.shot(&format!("{name}-chat"))?)?;
        let Some(bubble) = first_bubble(&shot) else {
            note(format!("no message bubble to open More options on ({name})"));
            return Ok(None);
        };
        let service = self.service;
        let Some(menu) = self.more_options(bubble, name, Some(service))? else {
            note(format!("no More options menu on b's message ({name})"));
            return Ok(None);
        };
        let before = load(&self.shot(&format!("{name}-menu-again"))?)?;
        // More actions is the menu's last action, as in Teams, over its "Customize"
        self.run
            .click(menu.x0 + 60, menu.y1.saturating_sub(18 + CUSTOMIZE_FOOT))?;
        thread::sleep(SETTLE);
        let after = load(&self.shot(&format!("{name}-list"))?)?;
        let whole = Rect {
            x0: 0,
            y0: 0,
            x1: after.width(),
            y1: after.height(),
        };
        Ok(changed_box(&before, &after, whole))
    }

    fn more_actions(&mut self) -> SandboxResult {
        let Some(list) = self.open_more_actions("more-actions")? else {
            return Ok(());
        };
        step(&format!("More actions: {list:?}"));
        let state = self.run.until_chat("the apps' message actions", |state| {
            state["actions"].as_array().is_some_and(|actions| !actions.is_empty())
        })?;
        let wanted = json!([
            {"app": FINDER, "title": "New record", "installed": true},
            {"app": "Made-up Tasks", "title": "Create task", "installed": true},
            {"app": TICKETS, "title": "File a ticket", "installed": false},
        ]);
        expect(state["actions"] == wanted, || {
            format!("More actions lists {}, expected {wanted}", state["actions"])
        });
        self.shots("more-actions")?;
        let rows = list_rows(&load(&self.shot("more-actions-rows")?)?, list);
        step(&format!("More actions' rows: {rows:?}"));
        self.key(keys::ESC)?;
        self.key(keys::ESC)?;
        thread::sleep(SETTLE);
        self.message_action()?;
        self.store_offer()
    }

    /// Clicks row `at` of More actions, counted from its top: the list is left of the menu, its rows' text in its
    /// first 250 px.
    fn click_action(&mut self, list: Rect, at: usize, name: &str) -> SandboxResult {
        let shot = load(&self.shot(&format!("{name}-rows"))?)?;
        let left = Rect {
            x1: (list.x0 + 250).min(list.x1),
            ..list
        };
        let rows = list_rows(&shot, left);
        let Some(row) = rows.get(at).copied() else {
            return Err(format!("More actions shows {} rows: {rows:?}", rows.len()).into());
        };
        self.run.click(list.x0 + 60, row)
    }

    /// Tasks' message action: its dialog is asked with the message, and its answer is a web page.
    fn message_action(&mut self) -> SandboxResult {
        let Some(list) = self.open_more_actions("task-action")? else {
            return Ok(());
        };
        self.run.until_chat("the apps' message actions", |state| {
            state["actions"].as_array().is_some_and(|actions| actions.len() == 3)
        })?;
        let before = self.invokes().len();
        // New record, Create task, File a ticket, Create workflows
        self.click_action(list, 1, "task-action")?;
        let invoke = self.next_invoke(before, "Create task")?;
        let value = &invoke["body"]["value"];
        expect(
            invoke["body"]["name"] == "composeExtension/fetchTask"
                && value["commandId"] == "taskFromMessage"
                && value["commandContext"] == "message"
                && value["messagePayload"].to_string().contains("1042")
                && invoke["path"]
                    .as_str()
                    .is_some_and(|path| decoded(path).contains(TASKS_BOT)),
            || format!("Create task sent {invoke}"),
        );
        self.until_dialog("Create task's web page", |dialog| dialog["stage"] == "web")?;
        self.shots("message-action-web")?;
        self.key(keys::ESC)?;
        step("a message action: its dialog on the message");
        Ok(())
    }

    /// The store's offer, in italics, opens the app's details.
    fn store_offer(&mut self) -> SandboxResult {
        let Some(list) = self.open_more_actions("offer")? else {
            return Ok(());
        };
        self.run.until_chat("the apps' message actions", |state| {
            state["actions"].as_array().is_some_and(|actions| actions.len() == 3)
        })?;
        let before = self.invokes().len();
        self.click_action(list, 2, "offer")?;
        let shown = self.until_dialog("the offer's details", |dialog| dialog["kind"] == "details");
        if let Err(error) = shown {
            note(format!(
                "File a ticket (not added) does not open the app's details: {error}"
            ));
        }
        self.no_invoke(before, "a store offer in More actions");
        self.shots("offer-details")?;
        self.key(keys::ESC)?;
        thread::sleep(SETTLE);
        let state = self.state()?;
        if !state["dialog"].is_null() {
            note(format!("Escape leaves the app's details open: {}", state["dialog"]));
            self.run.request(Request::Open {
                target: OpenTarget::Chat {
                    chat: self.service.chat.clone(),
                },
            })?;
        }
        step("a store offer opens its details");
        Ok(())
    }

    /// The Finder's card's Review (`task/fetch`): the app's dialog in the app.
    fn task_fetch(&mut self) -> SandboxResult {
        let shot = load(&self.shot("task-card")?)?;
        let Some(card) = cards_in(&shot).into_iter().max_by_key(|card| card.y0) else {
            note("no Finder card on the screen".to_owned());
            return Ok(());
        };
        let Some(review) = boxes_in(&shot, card).last().copied() else {
            note(format!("no Review button on the Finder's card {card:?}"));
            return Ok(());
        };
        let before = self.invokes().len();
        self.run.click(review.centre().0, review.centre().1)?;
        let invoke = self.next_invoke(before, "Review")?;
        expect(
            invoke["body"]["name"] == "task/fetch" && invoke["body"]["value"]["data"]["record"] == 1042,
            || format!("Review sent {}", invoke["body"]),
        );
        if let Err(error) = self.until_dialog("the Review dialog", |dialog| dialog["stage"] == "card") {
            note(format!("Review (task/fetch) opens no dialog in the app: {error}"));
        }
        thread::sleep(SETTLE);
        self.shots("task-fetch")?;
        self.key(keys::ESC)?;
        step("a card's task/fetch: the dialog in the app");
        Ok(())
    }
}

// --- a narrow window ---

impl Walk<'_> {
    fn windows(&self) -> SandboxResult<Vec<Value>> {
        let text = self.sandbox.niri_msg(&["-j", "windows"])?;
        let windows: Value = serde_json::from_str(&text)?;
        Ok(windows.as_array().cloned().unwrap_or_default())
    }

    /// The compositor sizes the focused window to `width` wide, as a user's drag does.
    fn resize(&mut self, width: i32) -> SandboxResult {
        let id = self
            .windows()?
            .iter()
            .find(|window| window["is_focused"] == true)
            .and_then(|window| window["id"].as_u64())
            .ok_or("no window has the focus")?;
        self.sandbox
            .niri_msg(&["action", "set-column-width", &width.to_string()])?;
        wait_until(WAIT, || {
            let windows = self.windows().ok()?;
            let window = windows.iter().find(|window| window["id"].as_u64() == Some(id))?;
            (window["layout"]["window_size"][0].as_f64() == Some(f64::from(width))).then_some(())
        })
        .ok_or_else(|| format!("the window never took {width} px"))?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// A narrow window: the flyout and a dialog fit it.
    fn narrow(&mut self) -> SandboxResult {
        const NARROW: u32 = 640;
        /// The box's "+" in the 640 px window: the box stacks its tools under its text.
        const NARROW_PLUS: (u32, u32) = (458, 749);
        self.resize(i32::try_from(NARROW)?)?;
        self.shots("narrow-chat")?;
        let closed = load(&self.shot("narrow-closed")?)?;
        self.run.click(NARROW_PLUS.0, NARROW_PLUS.1)?;
        match self.run.until_chat("the narrow flyout", |state| {
            state["compose"]["flyout"]["loaded"] == true
        }) {
            Ok(_) => {
                thread::sleep(SETTLE);
                let open = load(&self.shot("narrow-flyout")?)?;
                let whole = Rect {
                    x0: 0,
                    y0: 0,
                    x1: open.width(),
                    y1: NARROW_PLUS.1 - 16,
                };
                let flyout = changed_box(&closed, &open, whole);
                expect(flyout.is_some_and(|flyout| flyout.x1 <= NARROW), || {
                    format!("in a {NARROW} px window the flyout is {flyout:?}")
                });
                self.shots("narrow-flyout")?;
                self.key(keys::ESC)?;
            }
            Err(error) => note(format!("in a {NARROW} px window the + opens no flyout: {error}")),
        }
        let shot = load(&self.shot("narrow-card")?)?;
        let pane = Rect {
            x0: 300,
            y0: 96,
            x1: NARROW,
            y1: 680,
        };
        let review = boxes_in(&shot, pane)
            .into_iter()
            .find(|found| (40..150).contains(&found.width()));
        if let Some(review) = review {
            self.run.click(review.centre().0, review.centre().1)?;
            if self
                .until_dialog("the narrow dialog", |dialog| dialog["stage"] == "card")
                .is_ok()
            {
                let dialog = self.dialog_rect("narrow-dialog")?;
                expect(dialog.x1 <= NARROW, || {
                    format!("in a {NARROW} px window the dialog is {dialog:?}")
                });
                self.shots("narrow-dialog")?;
                self.key(keys::ESC)?;
            } else {
                note("in a narrow window, Review opens no dialog".to_owned());
            }
        } else {
            note("in a narrow window, no Review button found".to_owned());
        }
        self.resize(1280)?;
        step("a narrow window");
        Ok(())
    }
}

// --- the runs that show nothing of this ---

/// An account with no app that works in messages: the flyout says so, with Get more apps.
fn no_apps(sandbox: &Sandbox, runtime: &tokio::runtime::Runtime, service: &Service, dir: &Path) -> SandboxResult {
    let mut run = AppRun::launch(sandbox, &service.env(), dir, "")?;
    let outcome = (|| -> SandboxResult {
        let mut ctx = Walk {
            run: &mut run,
            sandbox,
            runtime,
            service,
        };
        ctx.run.open(&service.chat)?;
        thread::sleep(SETTLE);
        ctx.open_flyout("no-apps")?;
        let listed = ctx.listed()?;
        expect(listed.is_empty(), || {
            format!("an account with no apps lists {listed:?}")
        });
        ctx.shots("no-apps")?;
        ctx.key(keys::ESC)?;
        let Some(_) = ctx.open_more_actions_plain("no-apps-more")? else {
            return Ok(());
        };
        let state = ctx.state()?;
        expect(state["actions"].as_array().is_some_and(Vec::is_empty), || {
            format!("an account with no apps: More actions lists {}", state["actions"])
        });
        ctx.shots("no-apps-more")?;
        step("no apps: the flyout says so");
        Ok(())
    })();
    if let Err(error) = outcome {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit(sandbox)
}

impl Walk<'_> {
    /// [`Self::open_more_actions`] with the world's own apps.
    fn open_more_actions_plain(&mut self, name: &str) -> SandboxResult<Option<Rect>> {
        let shot = load(&self.shot(&format!("{name}-chat"))?)?;
        let Some(bubble) = first_bubble(&shot) else {
            note(format!("no message bubble to open More options on ({name})"));
            return Ok(None);
        };
        let Some(menu) = self.more_options(bubble, name, None)? else {
            return Ok(None);
        };
        self.shot(&format!("{name}-menu-open"))?;
        self.run
            .click(menu.x0 + 60, menu.y1.saturating_sub(18 + CUSTOMIZE_FOOT))?;
        thread::sleep(SETTLE);
        self.shot(&format!("{name}-list"))?;
        Ok(Some(menu))
    }
}

/// The read-only mode, or `NGTEAMS_BOTS` unset: the "+" opens nothing, More actions lists no app, a card's
/// `task/fetch` opens no dialog in the app.
fn hidden(
    sandbox: &Sandbox,
    runtime: &tokio::runtime::Runtime,
    service: &Service,
    dir: &Path,
    mode: &str,
) -> SandboxResult {
    let mut run = AppRun::launch(sandbox, &service.env(), dir, mode)?;
    let outcome = (|| -> SandboxResult {
        let mut ctx = Walk {
            run: &mut run,
            sandbox,
            runtime,
            service,
        };
        ctx.run.open(&service.chat)?;
        ctx.run.until_chat("the chat's messages", |state| {
            state["items"].as_array().is_some_and(|items| items.len() >= 2)
        })?;
        thread::sleep(SETTLE);
        ctx.shots(&format!("{mode}-chat"))?;
        let before = ctx.invokes().len();
        ctx.run.click(PLUS.0, PLUS.1)?;
        thread::sleep(SETTLE);
        let state = ctx.state()?;
        expect(state["compose"]["flyout"].is_null(), || {
            format!("{mode}: a click where the + is opens {}", state["compose"]["flyout"])
        });
        ctx.shot(&format!("{mode}-plus"))?;
        ctx.key(keys::ESC)?;
        if ctx.open_more_actions_plain(&format!("{mode}-more"))?.is_some() {
            thread::sleep(SETTLE);
            let state = ctx.state()?;
            expect(state["actions"].as_array().is_some_and(Vec::is_empty), || {
                format!("{mode}: More actions lists {}", state["actions"])
            });
            ctx.shots(&format!("{mode}-more"))?;
            ctx.key(keys::ESC)?;
            ctx.key(keys::ESC)?;
        }
        let shot = load(&ctx.shot(&format!("{mode}-card"))?)?;
        if let Some(review) = cards_in(&shot)
            .into_iter()
            .max_by_key(|card| card.y0)
            .and_then(|card| boxes_in(&shot, card).last().copied())
        {
            let opened_before = sandbox.opened()?.len();
            ctx.run.click(review.centre().0, review.centre().1)?;
            thread::sleep(SETTLE * 2);
            let state = ctx.state()?;
            expect(state["dialog"].is_null(), || {
                format!("{mode}: Review opens a dialog in the app: {}", state["dialog"])
            });
            let opened = sandbox.opened()?.len() > opened_before;
            step(&format!("{mode}: Review opened a link: {opened}"));
            ctx.shot(&format!("{mode}-review"))?;
        }
        thread::sleep(QUIET);
        let after = ctx.invokes().len();
        expect(after == before || mode == "flag-off", || {
            format!("{mode}: {} invokes reached an app", after - before)
        });
        step(&format!("{mode}: nothing of the apps shows"));
        Ok(())
    })();
    if let Err(error) = outcome {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit(sandbox)
}

const OWN_UPN: &str = "dana@own.example.test";
const OWN_B: &str = "erin@own.example.test";

/// An own server's account on the real binary: its server names no `appsService` yet, so the "+" opens nothing.
fn own_server(sandbox: &Sandbox, dir: &Path) -> SandboxResult {
    let Some(binary) = own_server_binary::binary()? else {
        say("skipped own-server checks: no ngteams-server binary (cargo build -p ngteams-server)");
        return Ok(());
    };
    let server = own_server_binary::BinaryServer::start(&binary)?;
    server.add_user(OWN_UPN, "Dana Made-Up")?;
    server.add_user(OWN_B, "Erin Made-Up")?;
    let env = vec![
        (ORIGIN_ENV, server.origin.clone()),
        (CA_ENV, server.ca_file.to_string_lossy().into_owned()),
    ];
    let mut run = AppRun::launch(sandbox, &env, dir, "own")?;
    let outcome = (|| -> SandboxResult {
        thread::sleep(SETTLE * 2);
        run.request(Request::NewChat {
            people: vec![OWN_B.to_owned()],
            wait: true,
        })?;
        let rows = run.until_in("list", "the chat made", |state| {
            state["rows"].as_array().is_some_and(|rows| !rows.is_empty())
        })?;
        let chat = rows["rows"][0]["id"].as_str().ok_or("no chat row")?.to_owned();
        run.open(&chat)?;
        thread::sleep(SETTLE);
        run.shot(sandbox, "qa-me-own-chat-light.png")?;
        run.click(PLUS.0, PLUS.1)?;
        thread::sleep(SETTLE);
        let state = run.state_in("chat")?;
        expect(state["compose"]["flyout"].is_null(), || {
            format!(
                "an own server's account (no appsService): the + opens {}",
                state["compose"]["flyout"]
            )
        });
        run.shot(sandbox, "qa-me-own-plus-light.png")?;
        run.theme(true)?;
        thread::sleep(SETTLE);
        run.shot(sandbox, "qa-me-own-plus-dark.png")?;
        run.theme(false)?;
        step("an own server's account: nothing of the apps shows");
        Ok(())
    })();
    if let Err(error) = outcome {
        let _ = fs::write(dir.join("own-server.log"), server.log());
        return Err(run.fail(sandbox, &*error));
    }
    run.quit(sandbox)
}

// --- the screen ---

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Rect {
    const fn width(&self) -> u32 {
        self.x1 - self.x0
    }

    const fn height(&self) -> u32 {
        self.y1 - self.y0
    }

    const fn centre(&self) -> (u32, u32) {
        (self.x0 + self.width() / 2, self.y0 + self.height() / 2)
    }

    const fn holds(&self, other: &Self) -> bool {
        other.x0 + 10 >= self.x0 && other.x1 <= self.x1 + 10 && other.y0 >= self.y0 && other.y1 <= self.y1
    }
}

fn load(shot: &Path) -> SandboxResult<image::RgbImage> {
    Ok(image::open(shot)?.to_rgb8())
}

fn near(pixel: [u8; 3], wanted: [u8; 3], within: u8) -> bool {
    pixel.iter().zip(wanted).all(|(got, want)| got.abs_diff(want) <= within)
}

fn at(shot: &image::RgbImage, x: u32, y: u32) -> [u8; 3] {
    if x < shot.width() && y < shot.height() {
        shot.get_pixel(x, y).0
    } else {
        [0, 0, 0]
    }
}

/// The cards in the chat's pane: a top border, a run of [`BORDER`] 120 to 470 px long over [`SURFACE`], down to the
/// bottom border; none inside another.
fn cards_in(shot: &image::RgbImage) -> Vec<Rect> {
    let mut found = Vec::new();
    for y in 0..shot.height().saturating_sub(4) {
        for (x0, x1) in runs(shot, y, PANE_X, shot.width(), |pixel| near(pixel, BORDER, 3)) {
            let mid = x0 + (x1 - x0) / 2;
            if !(120..=470).contains(&(x1 - x0)) || at(shot, mid, y + 2) != SURFACE {
                continue;
            }
            if let Some(bottom) = (y + 2..shot.height()).find(|&below| near(at(shot, x0 + 4, below), BORDER, 3)) {
                found.push(Rect {
                    x0: x0.saturating_sub(8),
                    y0: y,
                    x1: x1 + 8,
                    y1: bottom + 1,
                });
            }
        }
    }
    let outer: Vec<Rect> = found
        .iter()
        .copied()
        .filter(|card| !found.iter().any(|other| other != card && other.holds(card)))
        .collect();
    outer
}

/// The topmost white bubble in the chat's messages, under the header: its top row of [`SURFACE`] 120 to 700 px wide,
/// one line high.
fn first_bubble(shot: &image::RgbImage) -> Option<Rect> {
    (100..PLUS.1.saturating_sub(40)).find_map(|y| {
        runs(shot, y, PANE_X, shot.width(), |pixel| pixel == SURFACE)
            .into_iter()
            .find(|(x0, x1)| (120..=700).contains(&(x1 - x0)))
            .map(|(x0, x1)| Rect {
                x0,
                y0: y,
                x1,
                y1: y + 36,
            })
    })
}

/// The open flyout's top edge: up its left padding from the "+" until its surface ends.
fn flyout_top(shot: &image::RgbImage) -> Option<u32> {
    (60..PLUS.1.saturating_sub(24))
        .rev()
        .find(|&y| at(shot, FLYOUT_X0 + 4, y) != SURFACE)
}

/// The runs of row `y` from `from` to `to` whose pixels `taken` takes.
fn runs(shot: &image::RgbImage, y: u32, from: u32, to: u32, taken: impl Fn([u8; 3]) -> bool) -> Vec<(u32, u32)> {
    let mut found = Vec::new();
    let mut x = from;
    let to = to.min(shot.width());
    while x < to {
        if !taken(at(shot, x, y)) {
            x += 1;
            continue;
        }
        let start = x;
        while x < to && taken(at(shot, x, y)) {
            x += 1;
        }
        found.push((start, x));
    }
    found
}

/// A field's edge: its [`BORDER`], or the [`BRAND`] ring of the focused one.
fn edge(pixel: [u8; 3]) -> bool {
    near(pixel, BORDER, 4) || near(pixel, BRAND, 4)
}

/// The fields and buttons in `region`: boxes with an [`edge`] and a [`FILL`] (or an [`OFF_FILL`]) inside, in order
/// down the screen, then across.
fn boxes_in(shot: &image::RgbImage, region: Rect) -> Vec<Rect> {
    let mut found: Vec<Rect> = Vec::new();
    for y in region.y0..region.y1.min(shot.height()).saturating_sub(3) {
        for (x0, x1) in runs(shot, y, region.x0, region.x1, edge) {
            let mid = x0 + (x1 - x0) / 2;
            let inside = at(shot, mid, y + 2);
            if x1 - x0 < 30 || !(near(inside, FILL, 3) || near(inside, OFF_FILL, 2)) {
                continue;
            }
            let known = |it: &Rect| it.x0 <= mid && mid < it.x1 && it.y0 <= y && y < it.y1;
            if found.iter().any(known) {
                continue;
            }
            let Some(bottom) = (y + 2..region.y1.min(shot.height())).find(|&below| edge(at(shot, mid, below))) else {
                continue;
            };
            found.push(Rect {
                x0,
                y0: y,
                x1,
                y1: bottom + 1,
            });
        }
    }
    found.sort_by_key(|found| (found.y0, found.x0));
    found
}

/// The dialog's last box: its last button.
fn last_button(shot: &image::RgbImage, dialog: Rect) -> SandboxResult<Rect> {
    boxes_in(shot, dialog)
        .last()
        .copied()
        .ok_or_else(|| format!("no button in the dialog {dialog:?}").into())
}

/// The primary button in `dialog`: rows of [`BRAND`] over 100 px wide.
fn brand_button(shot: &image::RgbImage, dialog: Rect) -> Option<Rect> {
    let rows: Vec<(u32, u32, u32)> = (dialog.y0..dialog.y1)
        .filter_map(|y| {
            runs(shot, y, dialog.x0, dialog.x1, |pixel| near(pixel, BRAND, 12))
                .into_iter()
                .find(|(x0, x1)| x1 - x0 > 100)
                .map(|(x0, x1)| (y, x0, x1))
        })
        .collect();
    let (first, last) = (rows.first()?, rows.last()?);
    Some(Rect {
        x0: first.1,
        y0: first.0,
        x1: first.2,
        y1: last.0 + 1,
    })
}

/// The bounds of the Teams Light surface: an open dialog over the dimmed window.
fn surface_box(shot: &image::RgbImage) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    for y in 0..shot.height() {
        for (x0, x1) in runs(shot, y, 0, shot.width(), |pixel| pixel == SURFACE) {
            if x1 - x0 < 100 {
                continue;
            }
            bounds = Some(bounds.map_or(
                Rect {
                    x0,
                    y0: y,
                    x1,
                    y1: y + 1,
                },
                |known| Rect {
                    x0: known.x0.min(x0),
                    y0: known.y0,
                    x1: known.x1.max(x1),
                    y1: y + 1,
                },
            ));
        }
    }
    bounds
}

/// The centres of the text lines of a menu list: rows of dark text right of the rows' pictures.
fn list_rows(shot: &image::RgbImage, list: Rect) -> Vec<u32> {
    let dark = |y: u32| {
        (list.x0 + 36..list.x1.saturating_sub(8))
            .filter(|&x| at(shot, x, y).iter().map(|&value| u32::from(value)).sum::<u32>() < 450)
            .count()
            > 2
    };
    let mut lines: Vec<(u32, u32)> = Vec::new();
    for y in list.y0..list.y1 {
        if !dark(y) {
            continue;
        }
        match lines.last_mut() {
            Some(line) if line.1 + 3 >= y => line.1 = y,
            _ => lines.push((y, y)),
        }
    }
    lines.iter().map(|(top, bottom)| (top + bottom) / 2).collect()
}

/// The pixels of Teams Light's lit row ([`HOVER`]) in `region`.
fn lit_in(shot: &image::RgbImage, region: Rect) -> usize {
    (region.y0..region.y1.min(shot.height()))
        .flat_map(|y| (region.x0..region.x1.min(shot.width())).map(move |x| (x, y)))
        .filter(|&(x, y)| at(shot, x, y) == HOVER)
        .count()
}

fn danger_in(shot: &image::RgbImage, region: Rect) -> usize {
    (region.y0..region.y1.min(shot.height()))
        .flat_map(|y| (region.x0..region.x1.min(shot.width())).map(move |x| (x, y)))
        .filter(|&(x, y)| near(at(shot, x, y), DANGER, 40))
        .count()
}

/// The bounds of the pixels that changed in `region`.
fn changed_box(before: &image::RgbImage, after: &image::RgbImage, region: Rect) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    for y in region.y0..region.y1.min(before.height()) {
        for x in region.x0..region.x1.min(before.width()) {
            if near(at(before, x, y), at(after, x, y), 8) {
                continue;
            }
            bounds = Some(bounds.map_or(
                Rect {
                    x0: x,
                    y0: y,
                    x1: x + 1,
                    y1: y + 1,
                },
                |known| Rect {
                    x0: known.x0.min(x),
                    y0: known.y0.min(y),
                    x1: known.x1.max(x + 1),
                    y1: known.y1.max(y + 1),
                },
            ));
        }
    }
    bounds
}

// --- the service ---

struct Service {
    mock: ChatServiceMock,
    trouter: TrouterMock,
    chat: String,
}

impl Service {
    /// The mock with a's chat with b (b's question, and the Finder's card with a `task/fetch` button), and the apps
    /// when `with_apps`.
    fn start(runtime: &tokio::runtime::Runtime, with_apps: bool) -> SandboxResult<Self> {
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        if with_apps {
            for (definition, state) in teams_mock::extension_apps() {
                let id = definition["id"].as_str().unwrap_or_default().to_owned();
                mock.add_app(definition);
                if state != "Featured" {
                    mock.install_app(AppPlace::Personal, &id);
                }
            }
            for n in 1..=EXTRAS {
                let (app, bot) = extra_ids(n);
                mock.add_app(search_app(&app, &bot, &extra_name(n)));
                mock.install_app(AppPlace::Personal, &app);
                mock.script_bot(
                    &format!("28:{bot}"),
                    &extra_name(n),
                    &[(
                        "composeExtension/query",
                        BotReply::answer(results(&[(&format!("Extra {n} thing"), None)])),
                    )],
                );
            }
            mock.add_app(search_app(LONG_APP, LONG_BOT_ID, LONG_NAME));
            mock.install_app(AppPlace::Personal, LONG_APP);
        }
        script(&mock)?;
        let chat = mock.add_group(TOPIC, &[LAB_A, LAB_B]).ok_or("no group made")?;
        mock.post_as(LAB_B, &chat, &format!("<p>{ASKED}</p>"))
            .ok_or("b's question refused")?;
        post_card(&mock, &chat, FINDER_BOT, &review_card())?;
        let _ = mock.take_events(LAB_A);
        Ok(Self { mock, trouter, chat })
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            (ORIGIN_ENV, self.mock.origin()),
            (TROUTER_ENV, self.trouter.origin().to_owned()),
            (TOKEN_ENV, self.mock.token(LAB_A).unwrap_or_default()),
        ]
    }
}

/// What the made-up bots answer.
fn script(mock: &ChatServiceMock) -> SandboxResult {
    mock.script_bot(
        FINDER_BOT,
        FINDER,
        &[
            (
                "composeExtension/query",
                BotReply::answer(results(&[
                    ("Record 1042", None),
                    ("Record 1043", None),
                    ("Record 1044", Some("made-up-1044")),
                ])),
            ),
            ("composeExtension/selectItem", fixture("selected")?),
            ("composeExtension/fetchTask", BotReply::answer(record_dialog(true))),
            ("composeExtension/submitAction", fixture("submitDialog")?),
            ("task/fetch", fixture("dialog")?),
            ("task/submit", BotReply::done()),
        ],
    );
    mock.script_bot(
        &format!("28:{LONG_BOT_ID}"),
        LONG_NAME,
        &[("composeExtension/query", BotReply::answer(results(&[])))],
    );
    mock.script_bot(
        TASKS_BOT,
        "Made-up Tasks",
        &[
            ("composeExtension/fetchTask", fixture("webDialog")?),
            ("composeExtension/fetchCommands", fixture("noCommands")?),
        ],
    );
    Ok(())
}

fn fixture(name: &str) -> SandboxResult<BotReply> {
    teams_mock::bot_answer(name)
        .map(BotReply::answer)
        .ok_or_else(|| format!("no fixture answer {name}").into())
}

fn extra_name(n: u32) -> String {
    format!("Made-up Extra {n}")
}

/// The app's and its bot's ids of extra app `n`.
fn extra_ids(n: u32) -> (String, String) {
    (
        format!("00000000-0000-4000-8000-00000000f0{n:02x}"),
        format!("00000000-0000-4000-8000-00000000fb{n:02x}"),
    )
}

/// A made-up app with one search command, run at once.
fn search_app(id: &str, bot: &str, name: &str) -> Value {
    json!({
        "id": id, "name": name, "developerName": "Made-up Apps",
        "shortDescription": "Search made-up things and share them in a chat.", "accentColor": "#8764B8",
        "bots": [{"id": bot, "scopes": ["Personal", "Team", "GroupChat"]}],
        "inputExtensions": [{
            "botId": bot,
            "commands": [{"id": "find", "title": "Find", "initialRun": true,
                "parameters": [{"name": "words", "title": "Words", "description": "Search made-up things"}]}],
            "type": "botBased"
        }]
    })
}

/// A search's results: each a card with its preview, the preview's tap an `invoke` of the item when given.
fn results(items: &[(&str, Option<&str>)]) -> Value {
    let attachments: Vec<Value> = items
        .iter()
        .map(|(title, item)| {
            let mut preview = json!({"title": title, "text": "A made-up order for Friday"});
            if let Some(item) = item {
                preview["tap"] = json!({"type": "invoke", "value": {"itemId": item}});
            }
            json!({
                "contentType": ADAPTIVE,
                "content": {"type": "AdaptiveCard", "version": "1.5", "body": [
                    {"type": "TextBlock", "text": title, "weight": "bolder"},
                    {"type": "TextBlock", "text": "A made-up order for Friday", "wrap": true}
                ]},
                "preview": {"contentType": THUMBNAIL, "content": preview}
            })
        })
        .collect();
    json!({"responseType": "composeExtension", "composeExtension": {
        "type": "result", "attachmentLayout": "list", "attachments": attachments}})
}

/// The New record dialog: a name (`required` or not), a note, Create and a Cancel that checks no input.
fn record_dialog(required: bool) -> Value {
    json!({"responseType": "task", "task": {"type": "continue", "value": {
        "title": "New record", "width": "medium",
        "card": {"contentType": ADAPTIVE, "content": {
            "type": "AdaptiveCard", "version": "1.5",
            "body": [
                {"type": "TextBlock", "text": "New record", "weight": "bolder", "size": "medium"},
                {"type": "Input.Text", "id": "name", "label": "Name", "isRequired": required,
                 "errorMessage": "Enter a name"},
                {"type": "Input.Text", "id": "note", "label": "Note", "isMultiline": true,
                 "placeholder": "Anything else?"}
            ],
            "actions": [
                {"type": "Action.Submit", "title": "Create", "data": {"step": "create"}},
                {"type": "Action.Submit", "title": "Cancel", "associatedInputs": "none", "data": {"step": "cancel"}}
            ]
        }}
    }}})
}

/// A submit answered with a result: the card goes into the box.
fn made_result() -> BotReply {
    BotReply::answer(json!({"responseType": "composeExtension", "composeExtension": {
    "type": "result", "attachmentLayout": "list", "attachments": [{
        "contentType": ADAPTIVE,
        "content": {"type": "AdaptiveCard", "version": "1.5", "body": [
            {"type": "TextBlock", "text": "Record 1045 made", "weight": "bolder"}
        ]}
    }]}}))
}

/// The Finder's card about record 1042, its Review a `task/fetch` button.
fn review_card() -> Value {
    json!({"type": "message/card", "attachments": [{"contentType": ADAPTIVE, "content": {
        "type": "AdaptiveCard", "version": "1.5",
        "body": [
            {"type": "TextBlock", "text": "Record 1042", "weight": "bolder", "size": "medium"},
            {"type": "TextBlock", "text": "A made-up order for Friday, waiting for a reviewer.", "wrap": true}
        ],
        "actions": [{"type": "Action.Submit", "title": "Review", "data": {"msteams": {"type": "task/fetch"}, "record": 1042}}]
    }}]})
}

/// `bot` posts a card message carrying `activity` to `chat`, as the chat service delivers a bot's card; its id.
fn post_card(mock: &ChatServiceMock, chat: &str, bot: &str, activity: &Value) -> SandboxResult<String> {
    let b64 = STANDARD.encode(serde_json::to_vec(activity)?);
    let content = format!(
        "<URIObject type=\"SWIFT.1\">Card - access it on <a href=\"https://go.skype.com/cards.unsupported\">\
         https://go.skype.com/cards.unsupported</a>. <Title>Card</Title><Swift b64=\"{b64}\"></Swift></URIObject>"
    );
    mock.add_message(chat, (bot, FINDER), "RichText/Media_Card", &content)
        .ok_or_else(|| format!("the card was not posted to {chat}").into())
}

/// Every request to a bot's invoke the mock got: its path, whether the mock took it, and its body.
fn invokes_of(runtime: &tokio::runtime::Runtime, mock: &ChatServiceMock) -> Vec<Value> {
    let recorded = mock.bot_invokes();
    runtime
        .block_on(mock.received_requests())
        .iter()
        .filter(|request| request.url.path().contains("/agents/") && request.url.path().ends_with("/invoke"))
        .map(|request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
            let kept = recorded.iter().any(|record| record.body == body);
            json!({"path": request.url.path(), "taken": kept, "body": body})
        })
        .collect()
}

/// The messages sent to `chat`, typing aside.
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

/// The apps' cards a sent message carries (`properties.cards`, a JSON text).
fn sent_cards(message: &Value) -> Vec<Value> {
    message["properties"]["cards"]
        .as_str()
        .and_then(|cards| serde_json::from_str::<Vec<Value>>(cards).ok())
        .unwrap_or_default()
}

fn strings(list: &Value) -> Vec<String> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_owned))
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
        let mut run = Self {
            running,
            trace,
            ctl,
            dir: dir.to_owned(),
            input: sandbox.input()?,
            rest: (200, 24),
            asked: 0,
        };
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

    /// Clicks (`x`, `y`) as a user does; the link the app asked the opener for.
    fn click_opens(&mut self, sandbox: &Sandbox, x: u32, y: u32) -> SandboxResult<String> {
        let before = sandbox.opened()?.len();
        self.input.click(x, y, keys::BTN_LEFT)?;
        let opened = wait_until(WAIT, || sandbox.opened().ok()?.get(before).cloned())
            .ok_or_else(|| format!("a click at ({x}, {y}) opened nothing"))?;
        self.input.move_to(self.rest.0, self.rest.1)?;
        opened
            .args
            .last()
            .cloned()
            .ok_or_else(|| format!("the opener got no address: {opened:?}").into())
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
            .shot(sandbox, "qa-me-failed.png")
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

/// a's engine on the parent's mock, its store in `dir`; read-only when `read_only`.
fn engine_on_the_mock(dir: &Path, read_only: bool) -> SandboxResult<EngineHandle> {
    let origin = std::env::var(ORIGIN_ENV)?;
    let tokens = Arc::new(Tokens {
        token: std::env::var(TOKEN_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let http = || -> SandboxResult<HttpTransport<Tokens>> {
        let transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin)?;
        Ok(if read_only { transport.read_only() } else { transport })
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
    let read_only = mode == "read-only";
    let (first, runner, runtime) = if mode == "own" {
        let (first, runner, runtime) = own_account(&dir)?;
        (first, runner, Some(runtime))
    } else {
        let engine = engine_on_the_mock(&dir, read_only)?;
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
                read_only,
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
    match verb {
        "chat" => chat_state(shell.read(cx), cx),
        "list" => {
            let rows: Vec<Value> = shell
                .read(cx)
                .list()
                .read(cx)
                .rows()
                .iter()
                .map(|row| json!({"id": row.id, "name": row.name.as_ref()}))
                .collect();
            json!({ "rows": rows })
        }
        other => json!(format!("unknown {other}")),
    }
    .to_string()
}

/// The open dialog: an app's (its stage and its error), an app's details, or another.
fn dialog_state(shell: &Shell, cx: &App) -> Value {
    let Some(dialog) = shell.team_dialog() else {
        return Value::Null;
    };
    if let Ok(app) = dialog.clone().downcast::<AppDialog>() {
        let app = app.read(cx);
        return json!({"kind": "app", "stage": app.stage_name(), "error": app.error()});
    }
    if dialog.clone().downcast::<AppDetailsDialog>().is_ok() {
        return json!({"kind": "details"});
    }
    json!({"kind": "other"})
}

/// The chat's rows, its box (its words, its apps' cards, the flyout), the apps' message actions and the dialog.
fn chat_state(shell: &Shell, cx: &App) -> Value {
    let dialog = dialog_state(shell, cx);
    let main = format!("{:?}", shell.main());
    let toast = shell.toast().map(ToString::to_string);
    let Some(view) = shell.chat() else {
        return json!({"items": [], "compose": {}, "actions": [], "dialog": dialog, "main": main, "toast": toast});
    };
    let view: &ChatView = view.read(cx);
    let compose = view.compose().read(cx);
    let flyout = compose.apps_flyout().map(|flyout| {
        let flyout = flyout.read(cx);
        let listed: Vec<String> = flyout.listed(cx).iter().map(|app| app.name().to_owned()).collect();
        json!({
            "loaded": flyout.loaded(),
            "listed": listed,
            "actions": flyout.actions().len(),
            "pane": flyout.pane().map(|(name, tabs, found)| json!([name, tabs, found])),
        })
    });
    let cards: Vec<Value> = compose
        .app_cards()
        .iter()
        .map(|card| json!({"app": card.app_name, "content": card.content}))
        .collect();
    let actions: Vec<Value> = view
        .message_app_actions()
        .iter()
        .map(|(app, command)| json!({"app": app.name(), "title": command.title, "installed": app.installed}))
        .collect();
    let items: Vec<Value> = view.message_items().map(item_of).collect();
    json!({
        "conversation": view.conversation(),
        "items": items,
        "options": view.options(),
        "compose": {
            "value": compose.value(cx).to_string(),
            "cards": cards,
            "flyout": flyout,
            "read_only": compose.is_read_only(),
        },
        "actions": actions,
        "dialog": dialog,
        "main": main,
        "toast": toast,
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
    let app_cards = item.app_cards.as_ref().map_or(0, |cards| cards.0.cards.len());
    json!({"id": item.id, "own": item.own, "kind": kind, "text": text, "app_cards": app_cards})
}
