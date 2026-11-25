//! QA of task 579, rounds 2 and 3, in the UI sandbox: a call rings while the AI notice waits. b's app runs on the mock with
//! calls, its settings as the installed app makes them (the first run confirmed, no answer to the notice yet), so the
//! notice waits over the window. A caller the mock plays (a) rings b. The parent reads b's view trace and asks b's
//! window for its state through a file in the sandbox dir.
//!
//! - **While the notice waits**: the ring shows (banner, ringtone, desktop notification); Ctrl+Shift+D declines it;
//!   Ctrl+Shift+S answers the next ring and Ctrl+Shift+H hangs it up; the desktop notification's Answer answers a ring;
//!   a click on the banner's Answer is tried as a user tries it; Ctrl+Shift+C starts no call; the notice still waits
//!   after each call and still takes Tab and Enter.
//! - **After the answer**: a click on the banner's Answer answers the ring.
//! - **Round 3** ([`round3`]): the banner's Decline clicked over the notice, a call in progress behind the notice, Tab
//!   after the calls, and the call bar after the notice (a ring, a call in progress, another dialog open).
//!
//! `QA579_RING_ONLY` (a comma list of check names) runs only those.
//!
//! Screenshots go to `target/ui-shots/qa579-ring-*.png` and to `QA579_EVIDENCE` if set. Every name is made up.
//! Without weston 15, niri, `dbus-daemon`, PipeWire or WirePlumber it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use std::{fs, future, io, thread};

use gpui::{App, AsyncApp, Entity, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
use ngteams::calling::PersonalCalls;
use ngteams::descriptors::adopt_trace;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::account::web_client_info;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{CallMedia, CallParts, Identity, Outbox, Relay};
use teams_media::devices::Devices;
use teams_media::devices::Reach;
use teams_media::devices::pipewire::PipeWireDevices;
use teams_media::processing::ProcessingConfig;
use teams_media::relay::RelayServer;
use teams_media::session::SessionConfig;
use teams_media::tone::Tone;
use teams_mock::{AB_CHAT, CHAT_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, TENANT};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use teams_ui::shell::Shell;
use teams_ui::sounds::SoundBoard;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use teams_ui::{Ringer, SettingsFile, VideoFeed};
use tokio::sync::broadcast;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::pipewire::PipeWireOptions;
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

#[path = "qa_ai_notice_ring_round3.rs"]
mod round3;
#[path = "../window.rs"]
mod support;

use support::Alias;

const CHILD_ENV: &str = "QA579_RING_CHILD";
const EVIDENCE_ENV: &str = "QA579_EVIDENCE";
const ONLY_ENV: &str = "QA579_RING_ONLY";
const TRACE_FD_ENV: &str = "QA579_RING_TRACE_FD";
const DIR_ENV: &str = "QA579_RING_DIR";
const ORIGIN_ENV: &str = "QA579_RING_ORIGIN";
const TROUTER_ENV: &str = "QA579_RING_TROUTER";
const RELAY_ENV: &str = "QA579_RING_RELAY";
const IC3_ENV: &str = "QA579_RING_IC3";
const SKYPE_ENV: &str = "QA579_RING_SKYPE";
/// The file in the sandbox dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "qa579-ring-ask";
const STARTUP: Duration = Duration::from_secs(180);
const WAIT: Duration = Duration::from_secs(60);
const SETTLE: Duration = Duration::from_secs(2);
const STEP: Duration = Duration::from_millis(300);
const B_TONE: Tone = Tone {
    hz: 660.0,
    amplitude: 0.5,
};
/// b's settings: the first run confirmed, the chat with a selected, Teams Light, no answer to the AI notice.
const B_SETTINGS: &str = "first_run = \"confirmed\"\nfollow_system_theme = false\ntheme = \"teams-light\"\n";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_ai_notice_ring_window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn env(name: &str) -> SandboxResult<String> {
    Ok(std::env::var(name).map_err(|_| format!("{name} isn't set"))?)
}

fn ensure(holds: bool, what: impl FnOnce() -> String) -> SandboxResult {
    if holds { Ok(()) } else { Err(what().into()) }
}

// --- the parent ---

fn options() -> SandboxOptions {
    SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        pipewire: Some(PipeWireOptions {
            tone: B_TONE,
            ..PipeWireOptions::default()
        }),
        ..SandboxOptions::default()
    }
}

fn parent() -> SandboxResult {
    let Some(sandbox) = sandbox_or_skip("qa_ai_notice_ring_window", &options())? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mock = runtime.block_on(ChatServiceMock::start())?;
    let trouter = mock.start_trouter()?;
    let dir = tempfile::tempdir()?;
    let relay = mock.start_media(
        dir.path(),
        Tone {
            hz: 880.0,
            amplitude: 0.5,
        },
    )?;
    let b_dir = dir.path().join("b");
    fs::create_dir_all(&b_dir)?;
    fs::write(
        b_dir.join("settings.toml"),
        format!("{B_SETTINGS}selected_chat = \"{AB_CHAT}\"\n"),
    )?;
    let env = [
        (CHILD_ENV, "b".to_owned()),
        (ORIGIN_ENV, mock.origin()),
        (TROUTER_ENV, trouter.origin().to_owned()),
        (RELAY_ENV, relay.to_string()),
        (IC3_ENV, mock.token(LAB_B).ok_or("the mock's token")?),
        (SKYPE_ENV, mock.skypetoken(LAB_B).ok_or("the mock's skypetoken")?),
        (DIR_ENV, b_dir.to_string_lossy().into_owned()),
    ];
    let mut b = Callee::start(&sandbox, &env)?;
    let mut results = Results::default();
    for (name, check) in CHECKS {
        if !wanted(name) {
            continue;
        }
        say(&format!("check {name}"));
        let outcome = check(&mut b, &mock);
        if outcome.is_err() {
            let _ = b.shot(&format!("ring-failed-{name}"));
            say(&format!("  state: {:?}", b.state().ok()));
        }
        results.note(name, outcome);
    }
    let summary = results.summary();
    say(&summary);
    if let Some(dir) = evidence_dir() {
        let _ = fs::write(dir.join("ring-results.txt"), &summary);
        let _ = fs::copy(b_dir.join("settings.toml"), dir.join("ring-settings-after.toml"));
    }
    let quit = b.quit();
    drop(trouter);
    quit?;
    ensure(results.failed == 0, || format!("{} checks failed", results.failed))
}

fn wanted(name: &str) -> bool {
    std::env::var(ONLY_ENV).map_or(true, |only| only.split(',').any(|one| one.trim() == name))
}

type Check = fn(&mut Callee<'_>, &ChatServiceMock) -> SandboxResult;

const CHECKS: [(&str, Check); 14] = [
    ("ring_shows_while_the_notice_waits", ring_shows_while_the_notice_waits),
    ("decline_key_while_the_notice_waits", decline_key_while_the_notice_waits),
    (
        "answer_and_hang_up_keys_while_the_notice_waits",
        answer_and_hang_up_keys,
    ),
    ("notification_answers_while_the_notice_waits", notification_answers),
    (
        "banner_click_while_the_notice_waits",
        banner_click_while_the_notice_waits,
    ),
    (
        "r3_banner_decline_click_while_the_notice_waits",
        round3::banner_decline_click_while_the_notice_waits,
    ),
    (
        "r3_call_in_progress_stays_behind_the_notice",
        round3::call_in_progress_stays_behind_the_notice,
    ),
    ("no_new_call_from_behind_the_notice", no_new_call_from_behind_the_notice),
    (
        "r3_tab_stays_in_the_notice_after_calls",
        round3::tab_stays_in_the_notice_after_calls,
    ),
    (
        "notice_still_takes_tab_and_enter_after_calls",
        notice_still_takes_tab_and_enter,
    ),
    (
        "banner_click_answers_after_the_notice",
        banner_click_answers_after_the_notice,
    ),
    ("r3_banner_after_the_notice", round3::banner_after_the_notice),
    (
        "r3_in_call_strip_after_the_notice",
        round3::in_call_strip_after_the_notice,
    ),
    (
        "r3_ring_with_another_dialog_open",
        round3::ring_with_another_dialog_open,
    ),
];

#[derive(Default)]
struct Results {
    lines: String,
    passed: usize,
    failed: usize,
}

impl Results {
    fn note(&mut self, name: &str, outcome: SandboxResult) {
        match outcome {
            Ok(()) => {
                say(&format!("  pass {name}"));
                let _ = writeln!(self.lines, "pass {name}");
                self.passed += 1;
            }
            Err(error) => {
                say(&format!("  FAIL {name}: {error}"));
                let _ = writeln!(self.lines, "FAIL {name}: {error}");
                self.failed += 1;
            }
        }
    }

    fn summary(&self) -> String {
        format!(
            "{}qa_ai_notice_ring_window: {} passed, {} failed",
            self.lines, self.passed, self.failed
        )
    }
}

fn evidence_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os(EVIDENCE_ENV)?);
    fs::create_dir_all(&dir).ok().map(|()| dir)
}

fn shots_dir() -> io::Result<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// b's view trace, line by line as it comes.
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

    fn mark(&mut self) -> usize {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
        self.seen.len()
    }

    /// The first line since `mark` that `found` takes, waiting [`WAIT`] at most for it.
    fn since(&mut self, mark: usize, what: &str, found: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        self.within(mark, what, WAIT, found)
    }

    fn within(
        &mut self,
        mark: usize,
        what: &str,
        wait: Duration,
        found: impl Fn(&Value) -> bool,
    ) -> SandboxResult<Value> {
        self.mark();
        if let Some(line) = self.seen[mark..].iter().find(|line| found(line)) {
            return Ok(line.clone());
        }
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .map_err(|_| format!("no trace line {what} in {wait:?}"))?;
            let hit = found(&line);
            self.seen.push(line.clone());
            if hit {
                return Ok(line);
            }
        }
    }
}

fn is(line: &Value, view: &str, change: &str) -> bool {
    line["view"] == view && line["change"] == change
}

fn has_kind(line: &Value, kind: &str) -> bool {
    line["kinds"]
        .as_array()
        .is_some_and(|kinds| kinds.iter().any(|shown| shown == kind))
}

fn in_call_with(kind: &'static str) -> impl Fn(&Value) -> bool {
    move |line| is(line, "in-call", "shown") && has_kind(line, kind)
}

/// b's app in its sandbox: its process, its trace, the user's keyboard and pointer.
struct Callee<'a> {
    sandbox: &'a Sandbox,
    running: Running,
    trace: TraceReader,
    input: VirtualInput,
    asked: u32,
}

impl<'a> Callee<'a> {
    fn start(sandbox: &'a Sandbox, env: &[(&str, String)]) -> SandboxResult<Self> {
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let mut all: Vec<(&str, &str)> = env.iter().map(|(key, value)| (*key, value.as_str())).collect();
        all.push((TRACE_FD_ENV, &fd));
        let child = sandbox.spawn_with_fds(&std::env::current_exe()?, &[], &all, &[writer.as_fd()])?;
        drop(writer);
        let mut callee = Self {
            sandbox,
            running: Running::new(child)?,
            trace: TraceReader::start(reader),
            input: sandbox.input()?,
            asked: 0,
        };
        callee
            .running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| callee.running.fail(&format!("no ready line: {error:?}")))?;
        callee
            .trace
            .since(0, "status connected", |line| is(line, "status", "connected"))?;
        callee
            .trace
            .since(0, "calling registered", |line| is(line, "calling", "registered"))?;
        callee.park()?;
        thread::sleep(SETTLE);
        Ok(callee)
    }

    fn ask(&mut self, verb: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = self.sandbox.path(&format!("{ASK}.staged"));
        fs::write(&staged, format!("{} {verb}", self.asked))?;
        fs::rename(&staged, self.sandbox.path(ASK))?;
        let prefix = format!("re {} ", self.asked);
        let line = self
            .running
            .line_where(WAIT, |line| line.starts_with(&prefix))
            .map_err(|error| format!("no answer to {verb}: {error:?}"))?;
        Ok(line[prefix.len()..].to_owned())
    }

    fn state(&mut self) -> SandboxResult<Value> {
        Ok(serde_json::from_str(&self.ask("state")?)?)
    }

    fn notice_waits(&mut self, what: &str) -> SandboxResult {
        let state = self.state()?;
        ensure(state["notice"] == true && state["ai_features"].is_null(), || {
            format!("{what}: the notice does not wait: {state}")
        })
    }

    fn press(&mut self, spec: &str) -> SandboxResult {
        let (modifiers, key) =
            ui_sandbox::input::parse_key(spec, self.input.keymap()).ok_or_else(|| format!("no key {spec}"))?;
        self.input.key_with(&modifiers, key)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn park(&mut self) -> SandboxResult {
        self.input.move_to(300, 12)
    }

    /// The whole screen (the main window and any call window) as `qa579-<name>.png`.
    fn shot(&self, name: &str) -> SandboxResult {
        let file = format!("qa579-{name}.png");
        let png = self.sandbox.screenshot(&file, false)?;
        fs::copy(&png, shots_dir()?.join(&file))?;
        if let Some(dir) = evidence_dir() {
            fs::copy(&png, dir.join(&file))?;
        }
        Ok(())
    }

    fn look(&mut self) -> SandboxResult<image::RgbImage> {
        self.asked += 1;
        let png = self.sandbox.screenshot(&format!("look-{}.png", self.asked), false)?;
        let shot = image::open(&png)?.to_rgb8();
        fs::remove_file(png)?;
        Ok(shot)
    }

    /// The incoming call's desktop notification after `seen` others.
    fn notification(&self, seen: usize) -> SandboxResult<u32> {
        let deadline = Instant::now() + WAIT;
        loop {
            let found = self
                .sandbox
                .bus()
                .notifications()
                .into_iter()
                .filter(|notification| notification.actions.iter().any(|action| action == "answer"))
                .nth(seen);
            if let Some(found) = found {
                return Ok(found.id);
            }
            if Instant::now() > deadline {
                return Err("no incoming call's notification".into());
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn notifications_seen(&self) -> usize {
        self.sandbox
            .bus()
            .notifications()
            .iter()
            .filter(|notification| notification.actions.iter().any(|action| action == "answer"))
            .count()
    }

    fn quit(mut self) -> SandboxResult {
        let _ = self.ask("quit");
        let (status, stderr) = self.running.finish(WAIT)?;
        ensure(status.success(), || format!("b's app exited {status}: {stderr}"))
    }
}

/// The mock rings b; waits for b's banner. The call's id and the trace's mark before it.
fn ring(b: &mut Callee<'_>, mock: &ChatServiceMock) -> SandboxResult<(String, usize)> {
    let mark = b.trace.mark();
    let id = mock.ring(LAB_A, LAB_B);
    b.trace
        .since(mark, "call-banner shown", |line| is(line, "call-banner", "shown"))?;
    thread::sleep(SETTLE);
    Ok((id, mark))
}

/// Ends a call that is still on, whatever it is.
fn end_left(b: &mut Callee<'_>, mock: &ChatServiceMock, id: &str) {
    let _ = mock.hang_up_call(id);
    let _ = mock.end_ring(id, 487, "MockCallerCancelled");
    b.trace.mark();
    thread::sleep(SETTLE);
}

/// Where the call banner's buttons are at 1280 × 800: right of the banner's strip below the title bar.
const BANNER_BUTTONS: (u32, u32, u32, u32) = (980, 50, 1270, 94);

/// The centre of the banner's green Answer button on the screen, if any (the presence dots are green too: only the
/// banner's strip is searched).
fn answer_button(shot: &image::RgbImage) -> Option<(u32, u32)> {
    let (left, top, right, bottom) = BANNER_BUTTONS;
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in top..bottom.min(shot.height()) {
        for x in left..right.min(shot.width()) {
            let [r, g, b] = shot.get_pixel(x, y).0;
            if u16::from(g) > u16::from(r) + 40 && u16::from(g) > u16::from(b) + 20 {
                found = Some(found.map_or((x, y, x, y), |(x0, y0, x1, y1)| {
                    (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
                }));
            }
        }
    }
    found
        .filter(|(x0, y0, x1, y1)| x1 - x0 > 40 && y1 - y0 > 16)
        .map(|(x0, y0, x1, y1)| (x0.midpoint(x1), y0.midpoint(y1)))
}

fn ring_shows_while_the_notice_waits(b: &mut Callee<'_>, mock: &ChatServiceMock) -> SandboxResult {
    b.notice_waits("before the ring")?;
    let (id, mark) = ring(b, mock)?;
    b.trace
        .since(mark, "ringtone started", |line| is(line, "ringtone", "started"))?;
    let _ = b.notification(0)?;
    b.shot("ring-under-notice-light")?;
    b.ask("theme-dark")?;
    thread::sleep(SETTLE);
    b.shot("ring-under-notice-dark")?;
    b.ask("theme-light")?;
    end_left(b, mock, &id);
    b.notice_waits("after the ring")
}

fn decline_key_while_the_notice_waits(b: &mut Callee<'_>, mock: &ChatServiceMock) -> SandboxResult {
    let (id, mark) = ring(b, mock)?;
    b.press("ctrl+shift+d")?;
    let outcome = b
        .trace
        .since(mark, "the banner hidden", |line| is(line, "call-banner", "hidden"))
        .map(|_| ());
    let rejected = mock
        .calls()
        .iter()
        .any(|call| call.id == id && call.requests.iter().any(|(op, by)| op == "reject" && by == LAB_B));
    say(&format!(
        "  Ctrl+Shift+D: banner hidden {}, b's reject sent {rejected}",
        outcome.is_ok()
    ));
    end_left(b, mock, &id);
    outcome?;
    ensure(rejected, || "Ctrl+Shift+D sent no reject".to_owned())?;
    b.notice_waits("after the decline")
}

fn answer_and_hang_up_keys(b: &mut Callee<'_>, mock: &ChatServiceMock) -> SandboxResult {
    let (id, mark) = ring(b, mock)?;
    b.press("ctrl+shift+s")?;
    let answered = b.trace.since(mark, "in-call connected", in_call_with("connected"));
    if let Err(error) = answered {
        end_left(b, mock, &id);
        return Err(format!("Ctrl+Shift+S did not answer: {error}").into());
    }
    thread::sleep(SETTLE);
    b.shot("ring-answered-by-key")?;
    b.notice_waits("during the call")?;
    let mark = b.trace.mark();
    b.press("ctrl+shift+h")?;
    let ended = b.trace.since(mark, "in-call ended", in_call_with("ended"));
    end_left(b, mock, &id);
    ended.map_err(|error| format!("Ctrl+Shift+H did not hang up: {error}"))?;
    b.trace.within(mark, "the call window closed", WAIT, |line| {
        is(line, "call-window", "closed")
    })?;
    thread::sleep(SETTLE);
    b.shot("ring-hung-up-by-key")?;
    b.notice_waits("after the hang-up")
}

fn notification_answers(b: &mut Callee<'_>, mock: &ChatServiceMock) -> SandboxResult {
    let seen = b.notifications_seen();
    let (id, mark) = ring(b, mock)?;
    let notification = b.notification(seen)?;
    b.sandbox.bus().invoke_action(notification, "answer")?;
    let answered = b.trace.since(mark, "in-call connected", in_call_with("connected"));
    thread::sleep(SETTLE);
    b.shot("ring-answered-from-notification")?;
    let mark = b.trace.mark();
    b.press("ctrl+shift+h")?;
    let _ = b.trace.since(mark, "in-call ended", in_call_with("ended"));
    end_left(b, mock, &id);
    let _ = b.trace.within(mark, "the call window closed", WAIT, |line| {
        is(line, "call-window", "closed")
    });
    answered.map_err(|error| format!("the notification's Answer did not answer: {error}"))?;
    b.notice_waits("after the notification's call")
}

/// What a user tries first: a click on the banner's green Answer. It is said, and it fails the check if the click
/// neither answers nor shows the user why.
fn banner_click_while_the_notice_waits(b: &mut Callee<'_>, mock: &ChatServiceMock) -> SandboxResult {
    let (id, mark) = ring(b, mock)?;
    let shot = b.look()?;
    let at = answer_button(&shot);
    say(&format!("  the banner's Answer at {at:?}"));
    let Some((x, y)) = at else {
        end_left(b, mock, &id);
        return Err("no green Answer button on the screen while the notice waits".into());
    };
    b.input.click(x, y, keys::BTN_LEFT)?;
    b.park()?;
    let answered = b.trace.within(
        mark,
        "in-call connected",
        Duration::from_secs(8),
        in_call_with("connected"),
    );
    b.shot("ring-banner-click-under-notice")?;
    let state = b.state()?;
    say(&format!(
        "  a click on the banner's Answer: answered {}, notice {}",
        answered.is_ok(),
        state["notice"]
    ));
    if answered.is_ok() {
        let mark = b.trace.mark();
        b.press("ctrl+shift+h")?;
        let _ = b.trace.since(mark, "in-call ended", in_call_with("ended"));
    } else {
        b.press("ctrl+shift+d")?;
    }
    end_left(b, mock, &id);
    ensure(answered.is_ok(), || {
        "a click on the banner's Answer while the notice waits does not answer the call".to_owned()
    })
}

fn no_new_call_from_behind_the_notice(b: &mut Callee<'_>, mock: &ChatServiceMock) -> SandboxResult {
    let before = mock.calls().len();
    let mark = b.trace.mark();
    b.press("ctrl+shift+c")?;
    thread::sleep(Duration::from_secs(4));
    let after = mock.calls().len();
    b.trace.mark();
    let started = b.trace.seen[mark..]
        .iter()
        .find(|line| is(line, "in-call", "shown") || is(line, "call-window", "opened"))
        .cloned();
    ensure(after == before && started.is_none(), || {
        format!("Ctrl+Shift+C started a call: {before} -> {after} calls, {started:?}")
    })?;
    b.notice_waits("after Ctrl+Shift+C")
}

fn notice_still_takes_tab_and_enter(b: &mut Callee<'_>, _mock: &ChatServiceMock) -> SandboxResult {
    b.park()?;
    b.press("Tab")?;
    b.press("Return")?;
    thread::sleep(SETTLE);
    let state = b.state()?;
    b.shot("ring-notice-answered-after-calls")?;
    ensure(state["notice"] == false && state["ai_features"] == false, || {
        format!("after the calls, Tab and Enter did not answer \"Turn AI features off\": {state}")
    })
}

fn banner_click_answers_after_the_notice(b: &mut Callee<'_>, mock: &ChatServiceMock) -> SandboxResult {
    let (id, mark) = ring(b, mock)?;
    b.shot("ring-after-notice")?;
    let shot = b.look()?;
    let Some((x, y)) = answer_button(&shot) else {
        end_left(b, mock, &id);
        return Err("no green Answer button on the screen".into());
    };
    b.input.click(x, y, keys::BTN_LEFT)?;
    b.park()?;
    let answered = b.trace.since(mark, "in-call connected", in_call_with("connected"));
    thread::sleep(SETTLE);
    b.shot("ring-banner-click-after-notice")?;
    let mark = b.trace.mark();
    b.press("ctrl+shift+h")?;
    let _ = b.trace.since(mark, "in-call ended", in_call_with("ended"));
    end_left(b, mock, &id);
    answered
        .map(|_| ())
        .map_err(|error| format!("a click on the banner's Answer: {error}").into())
}

// --- the child: b's app ---

struct Tokens {
    ic3: String,
    skypetoken: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.ic3.clone())))
    }

    fn bearer_rejected(&self, _audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.ic3.clone())))
    }

    fn skypetoken(&self) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        future::ready(Ok(Some(Secret::new(self.skypetoken.clone()))))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(AccessToken {
            token: Secret::new(self.ic3.clone()),
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

/// b's engine on the mock, as `call_window`'s children build theirs.
fn mock_engine(devices: Arc<dyn Devices + Send + Sync>, feed: VideoFeed, dir: &Path) -> SandboxResult<EngineHandle> {
    let tokens = Arc::new(Tokens {
        ic3: env(IC3_ENV)?,
        skypetoken: env(SKYPE_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let tracker = Arc::new(teams_call::Tracker::new(Mri::new(LAB_B)));
    let origin = env(ORIGIN_ENV)?;
    let trouter_origin = env(TROUTER_ENV)?;
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
    let transport = Arc::new(http()?);
    let options = Options {
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(200),
        backoff_jitter_percent: 0,
        ..Options::default()
    };
    let trouter = || -> SandboxResult<_> {
        Ok(
            Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), &trouter_origin)?
                .with_options(options),
        )
    };
    let calling = trouter()?.for_calls(CallStack::new(Arc::clone(&tracker)));
    let name = support::display_name("bot-b@lab.example.test").to_owned();
    let identity = Identity {
        mri: Mri::new(LAB_B),
        display_name: Some(name.clone()),
        endpoint_id: String::new(),
        language_id: Some("en-us".into()),
        tenant_id: TENANT.into(),
        client: web_client_info(),
        region: Some("amer".into()),
        partition: Some("amer01".into()),
    };
    let media = CallMedia {
        devices,
        video: Some(teams_core::account::call_video(Arc::new(feed))),
        relay: Relay::Fixed(vec![RelayServer::udp(env(RELAY_ENV)?.parse()?)]),
        session: SessionConfig {
            processing: ProcessingConfig::OFF,
            ..Default::default()
        },
    };
    let push = Push::new(trouter()?);
    Ok(engine::start(Parts {
        calls: Some(CallParts::new(calling, identity, media)),
        store: Store::open(&support::store_file(dir, Alias::B))?,
        client: Arc::new(Client::new(http()?)),
        outbox: Outbox::new(transport, Author::new(Mri::new(LAB_B), name))
            .with_calls(Arc::clone(&tracker), teams_core::CallFloor::new()),
        push: Some(push),
        options: EngineOptions::default(),
        known_names: vec![(LAB_A.to_owned(), "Lab A".to_owned())],
    })?)
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let pipewire = Arc::new(PipeWireDevices::connect(Reach::Sandbox)?);
    let devices: Arc<dyn Devices + Send + Sync> = pipewire.clone();
    let feed = VideoFeed::new();
    let engine = mock_engine(Arc::clone(&devices), feed.clone(), &dir)?;
    let first = support::started(Alias::B, engine)?;
    let runner = support::runner(&dir)?;
    let ringer: Arc<dyn Ringer> = Arc::new(SoundBoard::new(devices));
    let first = ngteams::accounts::Started {
        calls: Some(PersonalCalls {
            ringer: Some(ringer),
            sounds: None,
            echo: Mri::new(ECHO),
            video: Some(feed),
            camera: Some(pipewire.clone()),
            devices: Some(pipewire),
            noise: teams_media::processing::NoiseControl::default(),
        }),
        ..first
    };
    // as the installed app makes them: the AI notice waits until the user answers it
    let settings = app::settings(SettingsFile::new(dir.join("settings.toml")), None);
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: Vec::new(),
                trace: Arc::new(trace),
                ctl: None,
                settings,
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(launched) => {
                answer(launched.window, cx);
                say("ready");
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
    Ok(())
}

fn shell_of(window: WindowHandle<KitRoot>, cx: &mut AsyncApp) -> Option<Entity<Shell>> {
    window
        .update(cx, |kit, _, cx| {
            let root = kit.view().clone().downcast::<Root>().ok()?;
            root.read(cx).shell_in(cx).cloned()
        })
        .ok()
        .flatten()
}

/// Answers the parent's questions as they come, until it asks to quit.
fn answer(window: WindowHandle<KitRoot>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let ask = PathBuf::from(dir).join(ASK);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(20)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let (seq, verb) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            if verb == "quit" {
                say(&format!("re {seq} ok"));
                cx.update(app::quit);
                return;
            }
            let reply = shell_of(window, cx)
                .and_then(|shell| {
                    window
                        .update(cx, |_, window, cx| on_shell(&shell, verb, window, cx))
                        .ok()
                })
                .unwrap_or_else(|| "no shell".to_owned());
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn on_shell(shell: &Entity<Shell>, verb: &str, window: &mut gpui::Window, cx: &mut App) -> String {
    let model = shell.read(cx).settings().clone();
    match verb {
        "state" => {
            let held = model.read(cx);
            json!({
                "notice": held.awaiting_ai_notice(),
                "ai_features": held.settings().ai_features,
                "main": format!("{:?}", shell.read(cx).main()),
            })
            .to_string()
        }
        "theme-dark" | "theme-light" => {
            let key = if verb == "theme-dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            model.update(cx, |model, cx| {
                model.change(
                    |settings| {
                        settings.appearance.follow_system = false;
                        settings.appearance.theme = key;
                    },
                    cx,
                );
            });
            "ok".to_owned()
        }
        // what the Teams page's "Create team" does
        "open-create-team" => {
            shell.update(cx, |shell, cx| shell.open_create_team(window, cx));
            "ok".to_owned()
        }
        _ => format!("unknown {verb}"),
    }
}
