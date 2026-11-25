//! QA of task 371 (round 1), in the UI sandbox: b's app (the callee) is rung, and the caller cancels the ring before,
//! at and just after the moment it starts to ring. After each cancel the parent reads b's view trace: the ring's banner,
//! ringtone and notification go away, no call window opens, and no in-call bar says "Call failed". A ring whose
//! `progress` the service really refuses still shows "Call failed". Screenshots of b after a cancelled ring, in Teams
//! Light and Teams Dark, go to `target/ui-shots/qa371-*.png`.
//!
//! - **Microsoft account** (`microsoft`): b on the mock (its chat service, Trouter, calling service and TURN server),
//!   built as `call_window`'s children are. The caller is one the mock plays (`ChatServiceMock::ring`); the mock ends
//!   the ring as the caller's cancel does (`ChatServiceMock::end_ring`, 487). The cancel comes: while b's `attach`
//!   waits for its answer (so the end reaches b before its `progress` can go), at that answer (a few ms either side),
//!   after b's `progress`, and after b's banner showed. The real failure: the mock answers b's `progress` with 500.
//! - **Own server's account** (`own`): the real `ngteams-server` binary over TLS (`support/own_server_binary.rs`) with
//!   the run's coturn, accounts a and b the operator added. b signs in to it in the window, as the app's own-server
//!   sign-in does, its calls on the sandbox's PipeWire. a is the app's engine for an own-server account
//!   (`ngteams::engine::start`) in this process, its calls on file devices. a calls b in their chat and hangs up: as
//!   soon as a's call rings, as soon as b's banner shows, and 100 ms and 1 s after. Then b's Activity and the chat,
//!   in both themes (the missed calls and the chat's "Call missed" lines).
//!
//! `QA_RC_CLOUD` (`microsoft` or `own`; unset: both). `QA_RC_LOG_DIR`: b's app log (info) goes there. The own
//! server's part needs `NGTS_TEST_COTURN` and `NGTS_TEST_COTURN_SECRET` (as the server's binary call tests) and the
//! built binary (`NGTEAMS_QA_SERVER_BIN`, else `target/debug/ngteams-server`); without them it says so and passes.
//! Without weston 15, niri, `dbus-daemon`, PipeWire or WirePlumber it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use std::{fs, future, io, thread};

use gpui::{App, AsyncApp, Entity, WindowHandle, WindowingModes};
use ngteams::accounts::{self, Runner};
use ngteams::accounts_file::{OwnServer, PersonalAccount};
use ngteams::app::{self, Launch};
use ngteams::calling::{CallDevices, PersonalCalls};
use ngteams::descriptors::adopt_trace;
use ngteams::secrets::AppSecrets;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::account::{account_calls, web_client_info};
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Event, Parts, Push};
use teams_core::{CallAction, CallId, CallMedia, CallParts, CallStatus, End, Identity, Outbox, Relay};
use teams_media::devices::Devices;
use teams_media::devices::Reach;
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::devices::pipewire::PipeWireDevices;
use teams_media::processing::ProcessingConfig;
use teams_media::relay::RelayServer;
use teams_media::session::SessionConfig;
use teams_media::tone::Tone;
use teams_mock::{AB_CHAT, CHAT_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, TENANT};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{CallStack, Options, RegistrationReport, Trouter, TrouterCredentials};
use teams_ui::settings::Settings;
use teams_ui::shell::Shell;
use teams_ui::sounds::SoundBoard;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use teams_ui::{Ringer, SettingsFile, VideoFeed};
use tokio::sync::broadcast;
use ui_sandbox::pipewire::PipeWireOptions;
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

#[path = "../own_server_binary.rs"]
#[expect(dead_code, reason = "the user and setup-code helpers are the accounts window's")]
mod own_server_binary;
#[path = "../window.rs"]
mod support;

use support::Alias;

/// Which child this is (`microsoft` or `own`); unset in the parent.
const CHILD_ENV: &str = "QA_RC_CHILD";
/// Which clouds the parent runs (`microsoft`, `own`); unset: both.
const CLOUD_ENV: &str = "QA_RC_CLOUD";
const TRACE_FD_ENV: &str = "QA_RC_TRACE_FD";
/// The child's own directory: its store and settings.
const DIR_ENV: &str = "QA_RC_DIR";
/// The chat the child's window opens.
const CHAT_ENV: &str = "QA_RC_CHAT";
/// Where the child's app log (info) goes, if set.
const LOG_ENV: &str = "QA_RC_LOG_DIR";
/// The mock's origin, its Trouter's, its TURN server's UDP address and b's made-up tokens.
const ORIGIN_ENV: &str = "QA_RC_ORIGIN";
const TROUTER_ENV: &str = "QA_RC_TROUTER";
const RELAY_ENV: &str = "QA_RC_RELAY";
const IC3_ENV: &str = "QA_RC_IC3";
const SKYPE_ENV: &str = "QA_RC_SKYPE";
/// The own server's origin, its CA file and the file with b's password.
const OWN_ORIGIN_ENV: &str = "QA_RC_OWN_ORIGIN";
const OWN_CA_ENV: &str = "QA_RC_OWN_CA";
const OWN_PASSWORD_ENV: &str = "QA_RC_OWN_PASSWORD";
/// The coturn of the run (as the server's binary call tests name it).
const COTURN_ENV: &str = "NGTS_TEST_COTURN";
const COTURN_SECRET_ENV: &str = "NGTS_TEST_COTURN_SECRET";
/// The file in the sandbox dir the parent writes to ask the child something: `light`, `dark`, `activity`, `chats`,
/// `quit`.
const ASK: &str = "qa-ask";
const STARTUP: Duration = Duration::from_secs(180);
const WAIT: Duration = Duration::from_secs(60);
/// How long the window is given, after a cancel, to show what it shows of it.
const SETTLE: Duration = Duration::from_secs(2);
/// How long the mock holds the answer of b's `attach`.
const ATTACH_HELD: Duration = Duration::from_millis(800);
/// The mock's own phrase of a caller's cancel (not captured).
const CANCELLED: &str = "MockCallerCancelled";
const B_TONE: Tone = Tone {
    hz: 660.0,
    amplitude: 0.5,
};
const A_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.5,
};
const ROUNDS: usize = 2;

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("microsoft") => child_microsoft(),
        Ok("own") => child_own(),
        Ok(other) => Err(format!("unknown child {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa ring cancel window: {error}");
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
    let which = std::env::var(CLOUD_ENV).unwrap_or_default();
    if which.is_empty() || which == "microsoft" {
        microsoft()?;
    }
    if which.is_empty() || which == "own" {
        own()?;
    }
    say("qa ring cancel window: ok");
    Ok(())
}

/// The child's view trace, line by line as it comes.
struct TraceReader {
    lines: mpsc::Receiver<serde_json::Value>,
    seen: Vec<serde_json::Value>,
}

impl TraceReader {
    fn start(reader: io::PipeReader) -> Self {
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let value = serde_json::from_str(&line).unwrap_or(serde_json::Value::String(line));
                let _ = sender.send(value);
            }
        });
        Self {
            lines,
            seen: Vec::new(),
        }
    }

    /// Takes every line that has come so far into `seen`; how many there are.
    fn mark(&mut self) -> usize {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
        self.seen.len()
    }

    /// The first line since `mark` that `found` takes, waiting for it if none came yet.
    fn since(
        &mut self,
        mark: usize,
        what: &str,
        found: impl Fn(&serde_json::Value) -> bool,
    ) -> SandboxResult<serde_json::Value> {
        self.mark();
        if let Some(line) = self.seen[mark..].iter().find(|line| found(line)) {
            return Ok(line.clone());
        }
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .map_err(|_| format!("no trace line {what}; since the mark {:?}", &self.seen[mark..]))?;
            let hit = found(&line);
            self.seen.push(line.clone());
            if hit {
                return Ok(line);
            }
        }
    }
}

fn is(line: &serde_json::Value, view: &str, change: &str) -> bool {
    line["view"] == view && line["change"] == change
}

fn kinds(line: &serde_json::Value) -> Vec<String> {
    line["kinds"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// b's app in its sandbox: its process and its trace.
struct Callee {
    running: Running,
    trace: TraceReader,
}

impl Callee {
    fn start(sandbox: &Sandbox, env: &[(&str, String)]) -> SandboxResult<Self> {
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let mut all: Vec<(&str, &str)> = env.iter().map(|(key, value)| (*key, value.as_str())).collect();
        all.push((TRACE_FD_ENV, &fd));
        let log = std::env::var(LOG_ENV).unwrap_or_default();
        if !log.is_empty() {
            all.push((LOG_ENV, &log));
        }
        let child = sandbox.spawn_with_fds(&std::env::current_exe()?, &[], &all, &[writer.as_fd()])?;
        drop(writer);
        let mut callee = Self {
            running: Running::new(child)?,
            trace: TraceReader::start(reader),
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
        Ok(callee)
    }

    /// Asks the child `what` and waits until it did it.
    fn ask(&mut self, sandbox: &Sandbox, what: &str) -> SandboxResult {
        fs::write(sandbox.path(ASK), what)?;
        let done = format!("asked {what}");
        self.running
            .line_where(WAIT, |line| line == done)
            .map_err(|error| self.running.fail(&format!("not {done}: {error:?}")))?;
        // the frames after the change
        thread::sleep(Duration::from_secs(2));
        Ok(())
    }

    fn quit(mut self, sandbox: &Sandbox) -> SandboxResult {
        fs::write(sandbox.path(ASK), "quit")?;
        self.running
            .line_where(WAIT, |line| line == "quit")
            .map_err(|error| self.running.fail(&format!("didn't quit: {error:?}")))?;
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("exited {status}: {stderr}").into());
        }
        Ok(())
    }

    /// What b's window did of a ring the caller cancelled (since `mark`): each banner it showed hidden, each ringtone
    /// stopped, each notification closed; no call window, no in-call bar, nothing that says the call failed. Whether
    /// it rang.
    fn cancelled_ring(&mut self, mark: usize, what: &str) -> SandboxResult<bool> {
        thread::sleep(SETTLE);
        self.trace.mark();
        let lines = self.trace.seen[mark..].to_vec();
        for line in &lines {
            let failed = is(line, "in-call", "shown") && kinds(line).iter().any(|kind| kind == "failed");
            if failed || is(line, "in-call", "shown") || is(line, "call-window", "opened") {
                return Err(format!("{what}: b's window shows {line}; since the mark {lines:?}").into());
            }
        }
        let shown: Vec<serde_json::Value> = lines
            .iter()
            .filter(|line| is(line, "call-banner", "shown"))
            .map(|line| line["call"].clone())
            .collect();
        for call in &shown {
            self.trace
                .since(mark, &format!("{what}: banner {call} hidden"), |line| {
                    is(line, "call-banner", "hidden") && line["call"] == *call
                })?;
        }
        let pairs = [
            (("ringtone", "started"), ("ringtone", "stopped")),
            (("call-notification", "shown"), ("call-notification", "closed")),
        ];
        for ((view, change), (ended_view, ended_change)) in pairs {
            let started: Vec<serde_json::Value> = lines
                .iter()
                .filter(|line| is(line, view, change))
                .map(|line| line["call"].clone())
                .collect();
            for call in &started {
                self.trace
                    .since(mark, &format!("{what}: {ended_view} {ended_change} {call}"), |line| {
                        is(line, ended_view, ended_change) && line["call"] == *call
                    })?;
            }
        }
        // what came while the ends were awaited: still nothing that failed
        self.trace.mark();
        if let Some(line) = self.trace.seen[mark..]
            .iter()
            .find(|line| is(line, "in-call", "shown") || is(line, "call-window", "opened"))
        {
            return Err(format!("{what}: b's window shows {line}").into());
        }
        say(&format!("qa: {what}: rang {}, ended as a cancelled ring", shown.len()));
        Ok(!shown.is_empty())
    }
}

/// Screenshots the focused window of `sandbox` into `name` under `target/ui-shots/`.
fn shot(sandbox: &Sandbox, name: &str) -> SandboxResult {
    shot_of(sandbox, name, true)
}

/// Where the screenshots are kept: `target/ui-shots/`, never committed; the focused window's (`window`), else the
/// screen's.
fn shot_of(sandbox: &Sandbox, name: &str, window: bool) -> SandboxResult {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    thread::sleep(Duration::from_millis(500));
    let png = sandbox.screenshot(name, window)?;
    fs::copy(&png, dir.join(name))?;
    say(&format!("qa: shot {name}"));
    Ok(())
}

/// Shots of b's window in Teams Light and Teams Dark: `qa371-<name>-light.png`, `qa371-<name>-dark.png`.
fn both_themes(b: &mut Callee, sandbox: &Sandbox, name: &str) -> SandboxResult {
    shot(sandbox, &format!("qa371-{name}-light.png"))?;
    b.ask(sandbox, "dark")?;
    shot(sandbox, &format!("qa371-{name}-dark.png"))?;
    b.ask(sandbox, "light")
}

// --- the Microsoft account on the mock ---

/// When the mock (the caller) cancels.
#[derive(Clone, Copy, Debug)]
enum Cut {
    /// While b's `attach` waits for its answer: the end reaches b before its `progress` can go.
    WhileAttachHeld,
    /// This many ms after the held `attach` is answered (negative: before).
    AtAttachAnswer(i64),
    /// As soon as the mock took b's `progress`.
    AfterProgress,
    /// This long after b's banner showed.
    AfterBanner(Duration),
}

const CUTS: [Cut; 8] = [
    Cut::WhileAttachHeld,
    Cut::AtAttachAnswer(-20),
    Cut::AtAttachAnswer(0),
    Cut::AtAttachAnswer(5),
    Cut::AtAttachAnswer(20),
    Cut::AtAttachAnswer(60),
    Cut::AfterProgress,
    Cut::AfterBanner(Duration::from_secs(1)),
];

fn microsoft() -> SandboxResult {
    let Some(sandbox) = sandbox_or_skip("qa ring cancel (Microsoft account)", &options())? else {
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
    let env = [
        (CHILD_ENV, "microsoft".to_owned()),
        (ORIGIN_ENV, mock.origin()),
        (TROUTER_ENV, trouter.origin().to_owned()),
        (RELAY_ENV, relay.to_string()),
        (IC3_ENV, mock.token(LAB_B).ok_or("the mock's token")?),
        (SKYPE_ENV, mock.skypetoken(LAB_B).ok_or("the mock's skypetoken")?),
        (DIR_ENV, b_dir.to_string_lossy().into_owned()),
        (CHAT_ENV, AB_CHAT.to_owned()),
    ];
    let mut b = Callee::start(&sandbox, &env)?;
    let outcome = microsoft_rings(&mock, &mut b, &sandbox);
    let outcome = outcome.map_err(|error| {
        format!(
            "{error}\n--- b: {}\n--- mock calls: {:?}\n--- sandbox{}",
            b.running.fail(&"killed"),
            mock.calls().iter().map(|call| &call.requests).collect::<Vec<_>>(),
            sandbox.logs()
        )
    });
    outcome?;
    b.quit(&sandbox)?;
    drop(trouter);
    say("qa ring cancel window, Microsoft account: ok");
    Ok(())
}

/// When the mock took the call's `segment` request (`attach`, `progress`) from b; waits for it.
fn took(mock: &ChatServiceMock, id: &str, segment: &str) -> SandboxResult<Instant> {
    let deadline = Instant::now() + WAIT;
    loop {
        let taken = mock
            .calls()
            .iter()
            .any(|call| call.id == id && call.requests.iter().any(|(op, by)| op == segment && by == LAB_B));
        if taken {
            return Ok(Instant::now());
        }
        if Instant::now() > deadline {
            return Err(format!("the mock never took b's {segment} of {id}").into());
        }
        thread::sleep(Duration::from_millis(1));
    }
}

/// The mock rings b and cancels at `cut`; b's window ends the ring as cancelled. Whether b rang.
fn ring_and_cut(mock: &ChatServiceMock, b: &mut Callee, cut: Cut, what: &str) -> SandboxResult<bool> {
    let mark = b.trace.mark();
    if matches!(cut, Cut::WhileAttachHeld | Cut::AtAttachAnswer(_)) {
        mock.delay_next_call_op("attach", ATTACH_HELD);
    }
    let id = mock.ring(LAB_A, LAB_B);
    match cut {
        Cut::WhileAttachHeld => {
            took(mock, &id, "attach")?;
        }
        Cut::AtAttachAnswer(offset) => {
            let at = took(mock, &id, "attach")? + ATTACH_HELD;
            let at = if offset < 0 {
                at.checked_sub(Duration::from_millis(offset.unsigned_abs()))
                    .unwrap_or(at)
            } else {
                at + Duration::from_millis(offset.unsigned_abs())
            };
            thread::sleep(at.saturating_duration_since(Instant::now()));
        }
        Cut::AfterProgress => {
            took(mock, &id, "progress")?;
        }
        Cut::AfterBanner(wait) => {
            b.trace
                .since(mark, "call-banner shown", |line| is(line, "call-banner", "shown"))?;
            thread::sleep(wait);
        }
    }
    if !mock.end_ring(&id, 487, CANCELLED) {
        return Err(format!("{what}: the mock knows no call {id}").into());
    }
    b.cancelled_ring(mark, what)
}

fn microsoft_rings(mock: &ChatServiceMock, b: &mut Callee, sandbox: &Sandbox) -> SandboxResult {
    let mut rang = 0;
    let mut tried = 0;
    for round in 0..ROUNDS {
        for cut in CUTS {
            tried += 1;
            if ring_and_cut(mock, b, cut, &format!("microsoft round {round} {cut:?}"))? {
                rang += 1;
            }
        }
    }
    say(&format!(
        "qa: microsoft: {tried} rings cancelled, {rang} showed b's banner, none failed"
    ));

    // b's window as it rings, then after the cancel, in both themes
    let mark = b.trace.mark();
    let id = mock.ring(LAB_A, LAB_B);
    b.trace
        .since(mark, "call-banner shown", |line| is(line, "call-banner", "shown"))?;
    thread::sleep(Duration::from_secs(1));
    shot(sandbox, "qa371-microsoft-ringing-light.png")?;
    if !mock.end_ring(&id, 487, CANCELLED) {
        return Err("the mock knows no call to cancel".into());
    }
    b.cancelled_ring(mark, "microsoft: the shot's ring")?;
    both_themes(b, sandbox, "microsoft-cancelled")?;

    // the service refuses b's progress on a ring that is live: "Call failed"
    let mark = b.trace.mark();
    mock.refuse_next_call_op("progress", 500);
    let id = mock.ring(LAB_A, LAB_B);
    let failed = b.trace.since(mark, "in-call shown failed", |line| {
        is(line, "in-call", "shown") && kinds(line).iter().any(|kind| kind == "failed")
    })?;
    say(&format!("qa: microsoft: progress refused 500: b's in-call {failed}"));
    thread::sleep(SETTLE);
    b.trace.mark();
    for line in &b.trace.seen[mark..] {
        say(&format!("qa: microsoft: the failed ring's trace: {line}"));
    }
    // the whole screen: no call window opened, and the main window says "Call failed"
    shot_of(sandbox, "qa371-microsoft-progress-failed-screen.png", false)?;
    both_themes(b, sandbox, "microsoft-progress-failed")?;
    let _ = mock.end_ring(&id, 487, CANCELLED);
    Ok(())
}

// --- the own server's account on the real binary ---

/// When a cancels.
#[derive(Clone, Copy, Debug)]
enum OwnCut {
    /// As soon as a's call rings (b's invitation may still be on its way).
    CallerRings,
    /// This long after b's banner showed (zero: at once).
    AfterBanner(Duration),
}

const OWN_CUTS: [OwnCut; 4] = [
    OwnCut::CallerRings,
    OwnCut::AfterBanner(Duration::ZERO),
    OwnCut::AfterBanner(Duration::from_millis(100)),
    OwnCut::AfterBanner(Duration::from_secs(1)),
];

/// a: the app's engine for an own server's account, in this process, its calls on file devices.
struct Caller {
    engine: teams_core::account::AccountEngine<AppSecrets>,
    runtime: tokio::runtime::Runtime,
    log: Vec<Event>,
}

impl Caller {
    fn sign_in(server: &own_server_binary::BinaryServer, root: &Path) -> SandboxResult<Self> {
        let setup = support::runner(root)?.setup.with_secrets(AppSecrets::in_memory());
        let own = OwnServer::parse(&server.origin, &server.ca_file.to_string_lossy())?;
        let pending = PersonalAccount::pending_on("a@own.test", &own)?;
        pending.save(&setup.paths)?;
        let signed = accounts::sign_in_own(&setup, &pending, &Secret::new(own_server_binary::PASSWORD))?;
        let devices: Arc<dyn Devices + Send + Sync> = Arc::new(FileDevices {
            input: AudioInput::Tone(A_TONE),
            recording: None,
            camera: PatternCamera::default(),
            pacing: Pacing::RealTime,
        });
        let engine = ngteams::engine::start(
            &setup.paths,
            &signed,
            teams_core::CallFloor::new(),
            setup.secrets(),
            true,
            Some(account_calls(devices, None)),
        )
        .map_err(|error| format!("a's engine: {error}"))?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mut caller = Self {
            engine,
            runtime,
            log: Vec::new(),
        };
        caller.until(0, "a's calling registration", |event| {
            matches!(event, Event::CallingRegistration(RegistrationReport::Registered))
        })?;
        Ok(caller)
    }

    fn handle(&self) -> &EngineHandle {
        &self.engine.handle
    }

    /// The first event since `mark` that `pick` takes, waiting for it.
    fn until(&mut self, mark: usize, what: &str, pick: impl Fn(&Event) -> bool) -> SandboxResult<usize> {
        let deadline = Instant::now() + WAIT;
        let mut seen = mark;
        loop {
            if let Some(found) = self.log[seen..].iter().position(&pick) {
                return Ok(seen + found);
            }
            seen = self.log.len();
            let left = deadline.saturating_duration_since(Instant::now());
            let handle = &mut self.engine.handle;
            let next = self
                .runtime
                .block_on(async { tokio::time::timeout(left, handle.next_event()).await });
            match next {
                Ok(Some(event)) => self.log.push(event),
                _ => return Err(format!("a: no {what}; since the mark {:?}", &self.log[mark..]).into()),
            }
        }
    }

    fn press(&self, action: CallAction) -> SandboxResult<CallId> {
        let handle = self.handle();
        Ok(self
            .runtime
            .block_on(handle.requests().call(action))
            .map_err(|error| format!("a's call action: {error}"))?)
    }

    fn chat_with(&self, other: &str) -> SandboxResult<String> {
        let create = Action::CreateChat {
            others: vec![Mri::new(other)],
            topic: None,
        };
        let row = self
            .runtime
            .block_on(self.handle().write(create))
            .map_err(|error| format!("a's chat: {error}"))?;
        Ok(row.message_id.ok_or("a's chat has no id")?)
    }
}

fn own() -> SandboxResult {
    let (Ok(relay), Ok(secret)) = (std::env::var(COTURN_ENV), std::env::var(COTURN_SECRET_ENV)) else {
        say("qa ring cancel window, own server: skipped: NGTS_TEST_COTURN and its secret name no coturn");
        return Ok(());
    };
    let Some(binary) = own_server_binary::binary()? else {
        say("qa ring cancel window, own server: skipped: no ngteams-server binary");
        return Ok(());
    };
    let Some(sandbox) = sandbox_or_skip("qa ring cancel (own server's account)", &options())? else {
        return Ok(());
    };
    let server = own_server_binary::BinaryServer::start_with(
        &binary,
        &format!(
            "\n[calling]\nring_timeout = 30\n[calling.turn]\nsecret = {secret:?}\nurls = [\"turn:{relay}?transport=udp\"]\n"
        ),
    )?;
    server.add_user("a@own.test", "Ada")?;
    let b_oid = server.add_user("b@own.test", "Bert")?;
    let root = tempfile::tempdir()?;
    let mut a = Caller::sign_in(&server, &root.path().join("a"))?;
    let chat = a.chat_with(&format!("8:orgid:{b_oid}"))?;
    let password = root.path().join("pw");
    fs::write(&password, own_server_binary::PASSWORD)?;
    let b_dir = root.path().join("b");
    fs::create_dir_all(&b_dir)?;
    let env = [
        (CHILD_ENV, "own".to_owned()),
        (OWN_ORIGIN_ENV, server.origin.clone()),
        (OWN_CA_ENV, server.ca_file.to_string_lossy().into_owned()),
        (OWN_PASSWORD_ENV, password.to_string_lossy().into_owned()),
        (DIR_ENV, b_dir.to_string_lossy().into_owned()),
        (CHAT_ENV, chat.clone()),
    ];
    let mut b = Callee::start(&sandbox, &env)?;
    let outcome = own_rings(&mut a, &mut b, &sandbox, &chat);
    let outcome = outcome.map_err(|error| {
        format!(
            "{error}\n--- b: {}\n--- server: {}\n--- sandbox{}",
            b.running.fail(&"killed"),
            server.log(),
            sandbox.logs()
        )
    });
    outcome?;
    b.quit(&sandbox)?;
    a.engine.handle.shutdown();
    if let Ok(dir) = std::env::var(LOG_ENV) {
        let _ = fs::write(Path::new(&dir).join("own-server.log"), server.log());
    }
    say("qa ring cancel window, own server: ok");
    Ok(())
}

/// a calls b and hangs up at `cut`; b's window ends the ring as cancelled. Whether b rang.
fn own_ring(a: &mut Caller, b: &mut Callee, chat: &str, cut: OwnCut, what: &str) -> SandboxResult<bool> {
    let (mark, from) = (b.trace.mark(), a.log.len());
    let call = a.press(CallAction::Call { chat: chat.to_owned() })?;
    match cut {
        OwnCut::CallerRings => {
            a.until(
                from,
                "a's call rings",
                |event| matches!(event, Event::Call { call: of, status: CallStatus::Ringing } if *of == call),
            )?;
        }
        OwnCut::AfterBanner(wait) => {
            b.trace
                .since(mark, "call-banner shown", |line| is(line, "call-banner", "shown"))?;
            thread::sleep(wait);
        }
    }
    a.press(CallAction::HangUp { call })?;
    let at = a.until(
        from,
        "a's call ends",
        |event| matches!(event, Event::Call { call: of, status: CallStatus::Ended(_) } if *of == call),
    )?;
    if !matches!(
        &a.log[at],
        Event::Call {
            status: CallStatus::Ended(End::HungUp),
            ..
        }
    ) {
        return Err(format!("{what}: a's call ended as {:?}", a.log[at]).into());
    }
    b.cancelled_ring(mark, what)
}

fn own_rings(a: &mut Caller, b: &mut Callee, sandbox: &Sandbox, chat: &str) -> SandboxResult {
    let (mut rang, mut tried) = (0, 0);
    for round in 0..ROUNDS {
        for cut in OWN_CUTS {
            tried += 1;
            if own_ring(a, b, chat, cut, &format!("own round {round} {cut:?}"))? {
                rang += 1;
            }
        }
    }
    say(&format!(
        "qa: own: {tried} rings cancelled, {rang} showed b's banner, none failed"
    ));
    let what = "own: the shot's ring";
    let (mark, from) = (b.trace.mark(), a.log.len());
    let call = a.press(CallAction::Call { chat: chat.to_owned() })?;
    b.trace
        .since(mark, "call-banner shown", |line| is(line, "call-banner", "shown"))?;
    thread::sleep(Duration::from_secs(1));
    shot(sandbox, "qa371-own-ringing-light.png")?;
    a.press(CallAction::HangUp { call })?;
    a.until(
        from,
        "a's call ends",
        |event| matches!(event, Event::Call { call: of, status: CallStatus::Ended(_) } if *of == call),
    )?;
    b.cancelled_ring(mark, what)?;
    // the missed calls' lines and items come from the server a moment after the end
    thread::sleep(Duration::from_secs(3));
    both_themes(b, sandbox, "own-cancelled-chat")?;
    b.ask(sandbox, "activity")?;
    both_themes(b, sandbox, "own-cancelled-activity")?;
    b.ask(sandbox, "chats")?;
    Ok(())
}

// --- the children ---

/// Installs b's app log (info: no payloads) in [`LOG_ENV`]'s directory, if it is set.
fn app_log(name: &str) -> SandboxResult {
    let Ok(dir) = std::env::var(LOG_ENV) else {
        return Ok(());
    };
    let config = teams_log::Config {
        dir: Path::new(&dir).join(name),
        setting: teams_log::Level::Info,
        env: None,
        rotation: teams_log::file::Rotation::default(),
    };
    teams_log::install(&config).map_err(|error| format!("the app's log: {error}"))?;
    Ok(())
}

/// b's settings: the chat with a open, Teams Light.
fn settings(dir: &Path) -> SandboxResult<SettingsFile> {
    let file = SettingsFile::new(dir.join("settings.toml"));
    let mut opened = Settings {
        selected_chat: Some(env(CHAT_ENV)?),
        ..Settings::default()
    };
    opened.appearance.follow_system = false;
    opened.appearance.theme = teams_ui::theme::LIGHT_KEY;
    file.save(&opened).map_err(|problem| format!("{problem:?}"))?;
    Ok(file)
}

/// b's mock tokens: its `ic3` token and its skypetoken; they never run out in a test.
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

/// b's engine on the mock, as `call_window`'s children build theirs: its calls' media on `devices`, relayed through
/// the mock's TURN server.
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
        known_names: Vec::new(),
    })?)
}

fn child_microsoft() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    app_log("microsoft")?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let pipewire = Arc::new(PipeWireDevices::connect(Reach::Sandbox)?);
    let devices: Arc<dyn Devices + Send + Sync> = pipewire.clone();
    let feed = VideoFeed::new();
    let engine = mock_engine(Arc::clone(&devices), feed.clone(), &dir)?;
    let first = support::started(Alias::B, engine)?;
    let runner = support::runner(&dir)?;
    let settings = settings(&dir)?;
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
    run_app(trace, settings, runner, first)
}

fn child_own() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    app_log("own")?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let setup = support::runner(&dir)?.setup.with_secrets(AppSecrets::in_memory());
    let server = OwnServer::parse(&env(OWN_ORIGIN_ENV)?, &env(OWN_CA_ENV)?)?;
    let password = fs::read_to_string(env(OWN_PASSWORD_ENV)?)?;
    let pending = PersonalAccount::pending_on("b@own.test", &server)?;
    pending.save(&setup.paths)?;
    let signed = accounts::sign_in_own(&setup, &pending, &Secret::new(password.trim()))?;
    let runner = Runner {
        setup,
        floor: teams_core::CallFloor::new(),
        devices: Some(CallDevices::open(Reach::Sandbox)),
    };
    let first = runner.start(&signed).map_err(|failed| failed.message)?;
    let settings = settings(&dir)?;
    run_app(trace, settings, runner, first)
}

fn run_app<S: teams_auth::secrets::SecretStore + 'static>(
    trace: teams_ui::ViewTrace,
    settings: SettingsFile,
    runner: Runner,
    first: ngteams::accounts::Started<S>,
) -> SandboxResult {
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
                settings: teams_ui::SettingsModel::load(settings),
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(launched) => {
                answer_asks(launched.window, cx);
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
    say("quit");
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

/// Each time the parent writes [`ASK`] in the sandbox dir: the theme, the view or a quit it asks for.
fn answer_asks(window: WindowHandle<KitRoot>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let ask = PathBuf::from(dir).join(ASK);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(asked) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            if asked == "quit" {
                cx.update(app::quit);
                return;
            }
            let Some(shell) = shell_of(window, cx) else {
                say("no shell");
                continue;
            };
            let _ = window.update(cx, |_, window, cx| match asked.as_str() {
                "light" => fixed_theme(&shell, teams_ui::theme::LIGHT_KEY, cx),
                "dark" => fixed_theme(&shell, teams_ui::theme::DARK_KEY, cx),
                "activity" => shell.update(cx, |shell, cx| shell.show_activity(window, cx)),
                "chats" => shell.update(cx, |shell, cx| shell.show_chats(window, cx)),
                _ => say("an unknown ask"),
            });
            say(&format!("asked {asked}"));
        }
    })
    .detach();
}

/// The theme `key`, whatever the system says.
fn fixed_theme(shell: &Entity<Shell>, key: &'static str, cx: &mut App) {
    let model = shell.read(cx).settings().clone();
    model.update(cx, |model, cx| {
        model.change(
            |settings| {
                settings.appearance.follow_system = false;
                settings.appearance.theme = key;
            },
            cx,
        );
    });
}
