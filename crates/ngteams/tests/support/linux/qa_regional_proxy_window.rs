//! QA of task 710 (round 1), in the UI sandbox: a meeting hosted in another region, for a Microsoft account on the
//! mock. The real app runs as a child in the sandbox, as lab account a on `teams-mock` (its chat service, its calling
//! service, its Trouter, its group video and its calendar). The parent opens a meeting of a's calendar in the pre-join
//! (the control socket, as a click on the event's Join does), and the user presses Enter (Join now), mutes, raises a
//! hand and hangs up with the call window's keys.
//!
//! - **A meeting in another region**: the mock hands out the call's links on
//!   `api-apac.flightproxy.teams.microsoft.com`. The call connects. The start goes to the main host; the mute
//!   (`updateEndpointState`), the hand (`publishState`), the leave and every other in-call write go to the regional host
//!   and succeed.
//! - **The same meeting on the main host**: it joins as before, every write on the main host.
//! - **A start the service refuses** (HTTP 500): the user sees the join fail. This is the normal error.
//! - **Links on a host that is not a flight proxy's** (look-alikes, another domain, a port): the join fails as the
//!   refused start does, and no request goes to that host (none reaches the mock after the start, and the app's log
//!   names none).
//!
//! The app's log (`info`: no payloads, every request's method, host and path) shows where each request went: the mock
//! takes every host at its own origin. `QA_RP_EVIDENCE` names a directory that gets the screenshots, the results and
//! the log's flight proxy lines. Screenshots also go to `target/ui-shots/qa710-*.png`. Every name, id and text is made
//! up.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::fmt::Write as _;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use std::{fs, future, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
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
use teams_ctl::protocol::{CalendarRow, CallRow, Request, Response};
use teams_ctl::{Client, Timeouts};
use teams_media::devices::Devices;
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::processing::ProcessingConfig;
use teams_media::relay::RelayServer;
use teams_media::session::SessionConfig;
use teams_media::tone::Tone;
use teams_mock::{
    AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE, MockCall,
    PRESENCE_BASE, PROVISIONING_BASE, TENANT, USER_PROFILE_BASE,
};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use teams_ui::calls::MeetingToJoin;
use teams_ui::shell::Shell;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use teams_ui::{SettingsFile, VideoFeed};
use tokio::sync::broadcast;
use ui_sandbox::input::VirtualInput;
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "../window.rs"]
mod support;
use support::{Alias, display_name};

const CHILD_ENV: &str = "QA_RP_CHILD";
const EVIDENCE_ENV: &str = "QA_RP_EVIDENCE";
const TRACE_FD_ENV: &str = "QA_RP_TRACE_FD";
const ORIGIN_ENV: &str = "QA_RP_ORIGIN";
const TROUTER_ENV: &str = "QA_RP_TROUTER";
const RELAY_ENV: &str = "QA_RP_RELAY";
const IC3_ENV: &str = "QA_RP_IC3";
const SKYPE_ENV: &str = "QA_RP_SKYPE";
const DIR_ENV: &str = "QA_RP_DIR";
const CTL_ENV: &str = "QA_RP_CTL";
const LOG_ENV: &str = "QA_RP_LOG";
/// The file in the sandbox dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "qa710-ask";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(100);
const SETTLE: Duration = Duration::from_millis(900);
const STEP: Duration = Duration::from_millis(250);
/// niri's layout: each window takes the whole screen, and no hot corner.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
const SCREEN: (u32, u32) = (1280, 800);
const PEER_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.2,
};

/// The main flight proxy host: every call start goes here.
const MAIN: &str = "api.flightproxy.teams.microsoft.com";
/// The regional flight proxy host a meeting hosted in APAC hands out its links on.
const APAC: &str = "api-apac.flightproxy.teams.microsoft.com";
/// Hosts that are not a flight proxy's, as the start answer names them: a look-alike under another domain, a longer
/// first label, another domain, the regional host with a port.
const LOOK_ALIKES: [(&str, &str); 4] = [
    ("suffix", "api-apac.flightproxy.teams.microsoft.com.evil.example"),
    ("prefix", "evilapi-apac.flightproxy.teams.microsoft.com"),
    ("domain", "api-apac.flightproxy.evil.example"),
    ("port", "api-apac.flightproxy.teams.microsoft.com:8443"),
];

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_regional_proxy_window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn ensure(holds: bool, what: impl FnOnce() -> String) -> SandboxResult {
    if holds { Ok(()) } else { Err(what().into()) }
}

fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn evidence_dir() -> Option<PathBuf> {
    std::env::var_os(EVIDENCE_ENV).map(PathBuf::from)
}

// --- the parent ---

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_regional_proxy_window", &options)? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mock = runtime.block_on(ChatServiceMock::start())?;
    let trouter = mock.start_trouter()?;
    let dir = tempfile::tempdir()?;
    let relay = mock.start_group_video(dir.path(), PEER_TONE)?.to_string();
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    {
        let app = AppUnderTest::start(&sandbox, &mock, &relay, trouter.origin(), &dir.path().join("a"))?;
        let mut user = User::new(&sandbox, &mock, app)?;
        user.run_check(
            "regional_meeting_joins_and_writes_go_to_the_region",
            regional,
            &mut passed,
            &mut failed,
        );
        user.run_check("main_host_meeting_joins_as_before", main_host, &mut passed, &mut failed);
        user.run_check(
            "refused_start_shows_the_normal_error",
            refused_start,
            &mut passed,
            &mut failed,
        );
        for (name, host) in LOOK_ALIKES {
            say(&format!("check look_alike_{name}_is_refused ({host})"));
            match look_alike(&mut user, name, host) {
                Ok(()) => {
                    say(&format!("  pass look_alike_{name}_is_refused"));
                    passed.push(format!("look_alike_{name}_is_refused"));
                }
                Err(error) => {
                    let _ = user.shot(&format!("failed-look-alike-{name}"));
                    say(&format!("  FAIL look_alike_{name}_is_refused: {error}"));
                    failed.push((format!("look_alike_{name}_is_refused"), error.to_string()));
                }
            }
            mock.hand_out_call_links_on(None);
        }
        user.keep_log_lines();
        user.quit()?;
    }
    drop(trouter);
    let mut summary = String::new();
    for name in &passed {
        let _ = writeln!(summary, "pass {name}");
    }
    for (name, why) in &failed {
        let _ = writeln!(summary, "FAIL {name}: {why}");
    }
    say(&summary);
    if let Some(dir) = evidence_dir() {
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(dir.join("results.txt"), &summary);
    }
    say(&format!(
        "qa_regional_proxy_window: {} passed, {} failed",
        passed.len(),
        failed.len()
    ));
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("{} checks failed", failed.len()).into())
    }
}

/// The child app in its sandbox: its process, its trace, its control socket, its log.
struct AppUnderTest {
    running: Option<Running>,
    _trace: TraceReader,
    socket: PathBuf,
    log: PathBuf,
}

impl AppUnderTest {
    fn start(sandbox: &Sandbox, mock: &ChatServiceMock, relay: &str, trouter: &str, own: &Path) -> SandboxResult<Self> {
        fs::create_dir_all(own)?;
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let ic3 = mock.token(LAB_A).ok_or("the mock's token")?;
        let skype = mock.skypetoken(LAB_A).ok_or("the mock's skypetoken")?;
        let socket = sandbox.path("run/ctl-a.sock");
        let log = own.join("log");
        let own_text = own.to_string_lossy().into_owned();
        let socket_text = socket.to_string_lossy().into_owned();
        let log_text = log.to_string_lossy().into_owned();
        let child = sandbox.spawn_with_fds(
            &std::env::current_exe()?,
            &[],
            &[
                (CHILD_ENV, "a"),
                (TRACE_FD_ENV, &fd),
                (ORIGIN_ENV, &mock.origin()),
                (TROUTER_ENV, trouter),
                (RELAY_ENV, relay),
                (IC3_ENV, &ic3),
                (SKYPE_ENV, &skype),
                (DIR_ENV, &own_text),
                (CTL_ENV, &socket_text),
                (LOG_ENV, &log_text),
            ],
            &[writer.as_fd()],
        )?;
        drop(writer);
        let mut running = Running::new(child)?;
        let mut trace = TraceReader::start(reader);
        if let Err(error) = running.line_where(STARTUP, |line| line == "ready") {
            return Err(running.fail(&format!("no ready line: {error:?}{}", sandbox.logs())));
        }
        trace.wait_for("status", "connected")?;
        trace.wait_for("calling", "registered")?;
        Ok(Self {
            running: Some(running),
            _trace: trace,
            socket,
            log,
        })
    }
}

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

    fn wait_for(&mut self, view: &str, change: &str) -> SandboxResult {
        let deadline = Instant::now() + STARTUP;
        while !self
            .seen
            .iter()
            .any(|line| line["view"] == view && line["change"] == change)
        {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .map_err(|_| format!("no trace line {view} {change}"))?;
            self.seen.push(line);
        }
        Ok(())
    }
}

/// A join: where the app's log and the mock's calls stood before it.
struct Joined {
    log_mark: usize,
    calls_before: usize,
}

/// The user at the sandbox: the keyboard, the screenshots, the control socket, and the child it asks about the window.
struct User<'a> {
    sandbox: &'a Sandbox,
    mock: &'a ChatServiceMock,
    app: AppUnderTest,
    input: VirtualInput,
    ctl: Client,
    asked: u32,
    main_window: Option<u64>,
    meeting: CalendarRow,
    /// What the window shows of a join that the service refused: the normal error.
    normal_failure: Option<Value>,
}

impl<'a> User<'a> {
    fn new(sandbox: &'a Sandbox, mock: &'a ChatServiceMock, app: AppUnderTest) -> SandboxResult<Self> {
        let socket = app.socket.clone();
        let mut ctl = wait_until(WAIT, || Client::connect(&socket, Timeouts::default()).ok())
            .ok_or_else(|| format!("no control socket at {}", socket.display()))?;
        let meeting = calendar_meeting(&mut ctl)?;
        say(&format!("  the meeting: {} ({})", meeting.subject, meeting.id));
        let mut user = Self {
            sandbox,
            mock,
            app,
            input: sandbox.input()?,
            ctl,
            asked: 0,
            main_window: None,
            meeting,
            normal_failure: None,
        };
        user.main_window = user.focused_window()?;
        user.park()?;
        thread::sleep(SETTLE);
        Ok(user)
    }

    fn run_check(
        &mut self,
        name: &'static str,
        check: fn(&mut User<'_>) -> SandboxResult,
        passed: &mut Vec<String>,
        failed: &mut Vec<(String, String)>,
    ) {
        say(&format!("check {name}"));
        match check(self) {
            Ok(()) => {
                say(&format!("  pass {name}"));
                passed.push(name.to_owned());
            }
            Err(error) => {
                let error = error.to_string();
                let state = self.state().map(|state| state.to_string()).unwrap_or_default();
                let _ = self.shot(&format!("failed-{name}"));
                say(&format!("  FAIL {name}: {error}\n  state: {state}"));
                failed.push((name.to_owned(), error));
            }
        }
        self.mock.hand_out_call_links_on(None);
    }

    fn running(&self) -> SandboxResult<&Running> {
        self.app.running.as_ref().ok_or_else(|| "the app has quit".into())
    }

    fn ask(&mut self, verb: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = self.sandbox.path(&format!("{ASK}.staged"));
        fs::write(&staged, format!("{} {verb}", self.asked))?;
        fs::rename(&staged, self.sandbox.path(ASK))?;
        let prefix = format!("re {} ", self.asked);
        let line = self
            .running()?
            .line_where(WAIT, |line| line.starts_with(&prefix))
            .map_err(|error| format!("no answer to {verb}: {error:?}"))?;
        Ok(line[prefix.len()..].to_owned())
    }

    fn state(&mut self) -> SandboxResult<Value> {
        Ok(serde_json::from_str(&self.ask("state")?)?)
    }

    fn until(&mut self, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let start = Instant::now();
        loop {
            let state = self.state()?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {state}").into());
            }
            thread::sleep(POLL);
        }
    }

    fn quit(&mut self) -> SandboxResult {
        let _ = self.hang_up_by_ctl();
        self.ask("quit")?;
        let running = self.app.running.take().ok_or("the app has quit")?;
        let (status, stderr) = running.finish(WAIT)?;
        ensure(status.success(), || format!("the app exited {status}: {stderr}"))
    }

    fn press(&mut self, spec: &str) -> SandboxResult {
        let (modifiers, key) =
            ui_sandbox::input::parse_key(spec, self.input.keymap()).ok_or_else(|| format!("no key {spec}"))?;
        self.input.key_with(&modifiers, key)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn park(&mut self) -> SandboxResult {
        // the window's left margin: no button, tooltip or tile there
        self.input.move_to(3, SCREEN.1 / 2)
    }

    /// A screenshot of the focused window as `qa710-<name>.png`, under `target/ui-shots/` and the evidence dir.
    fn shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        self.park()?;
        thread::sleep(SETTLE);
        let file = format!("qa710-{name}.png");
        let png = self.sandbox.screenshot(&file, true)?;
        let kept = shots_dir()?.join(&file);
        fs::copy(&png, &kept)?;
        if let Some(dir) = evidence_dir() {
            let dir = dir.join("shots");
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(&file))?;
        }
        say(&format!("  shot qa710-{name}.png"));
        Ok(kept)
    }

    // --- windows ---

    fn windows(&self) -> SandboxResult<Vec<Value>> {
        let text = self.sandbox.niri_msg(&["-j", "windows"])?;
        let windows: Value = serde_json::from_str(&text)?;
        Ok(windows.as_array().cloned().unwrap_or_default())
    }

    fn focused_window(&self) -> SandboxResult<Option<u64>> {
        Ok(self
            .windows()?
            .iter()
            .find(|window| window["is_focused"] == true)
            .and_then(|window| window["id"].as_u64()))
    }

    /// Waits for a window other than the main window to have the focus (the pre-join's or the call's).
    fn wait_other_window(&mut self) -> SandboxResult {
        let main = self.main_window;
        wait_until(WAIT, || {
            self.windows().ok()?.iter().find_map(|window| {
                let id = window["id"].as_u64()?;
                (Some(id) != main && window["is_focused"] == true).then_some(id)
            })
        })
        .ok_or("no window besides the main window")?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// Waits until only the main window is left (the call window closed by itself).
    fn wait_only_main(&mut self) -> SandboxResult {
        let main = self.main_window;
        wait_until(Duration::from_secs(60), || {
            let windows = self.windows().ok()?;
            windows.iter().all(|window| window["id"].as_u64() == main).then_some(())
        })
        .ok_or("the call window stayed open")?;
        Ok(())
    }

    // --- calls ---

    fn calls(&mut self) -> SandboxResult<Vec<CallRow>> {
        match self.ctl.request(None, Request::Calls)? {
            Response::Calls { calls } => Ok(calls),
            other => Err(format!("calls: {other:?}").into()),
        }
    }

    fn wait_call(&mut self, what: &str, wanted: impl Fn(&CallRow) -> bool) -> SandboxResult<CallRow> {
        let deadline = Instant::now() + WAIT;
        loop {
            let calls = self.calls()?;
            if let Some(row) = calls.iter().find(|row| wanted(row)) {
                return Ok(row.clone());
            }
            if Instant::now() > deadline {
                return Err(format!("{what}: the calls are {calls:?}").into());
            }
            thread::sleep(POLL);
        }
    }

    fn hang_up_by_ctl(&mut self) -> SandboxResult {
        if self.calls()?.is_empty() {
            return Ok(());
        }
        self.ctl.request(None, Request::HangUp)?;
        Ok(())
    }

    /// The meeting's pre-join, as the event's Join opens it, then Join now (Enter) in it.
    fn join(&mut self, name: &str) -> SandboxResult<Joined> {
        self.wait_only_main()?;
        let joined = Joined {
            log_mark: self.log_lines().len(),
            calls_before: self.mock.calls().len(),
        };
        self.ctl.request(
            None,
            Request::JoinMeeting {
                event: self.meeting.id.clone(),
                now: false,
            },
        )?;
        let state = self.until("the pre-join", |state| !state["prejoin"].is_null())?;
        say(&format!("  the pre-join: {}", state["prejoin"]));
        self.wait_other_window()?;
        self.shot(&format!("{name}-prejoin"))?;
        self.press("enter")?;
        Ok(joined)
    }

    /// The mock's call that a's join of `joined` started.
    fn started_call(&self, joined: &Joined) -> SandboxResult<MockCall> {
        let found = wait_until(WAIT, || {
            self.mock
                .calls()
                .into_iter()
                .skip(joined.calls_before)
                .find(|call| call.requests.iter().any(|(op, by)| op == "start" && by == LAB_A))
        });
        Ok(found.ok_or("no start of a's on the mock")?)
    }

    fn requests_of(&self, id: &str) -> Vec<(String, String)> {
        self.mock
            .calls()
            .into_iter()
            .find(|call| call.id == id)
            .map(|call| call.requests)
            .unwrap_or_default()
    }

    fn count_of(&self, id: &str, op: &str) -> usize {
        self.requests_of(id)
            .iter()
            .filter(|(seen, by)| seen == op && by == LAB_A)
            .count()
    }

    /// Waits for a's `op` on call `id` to reach the mock more than `before` times.
    fn wait_op(&self, id: &str, op: &str, before: usize) -> SandboxResult {
        wait_until(WAIT, || (self.count_of(id, op) > before).then_some(()))
            .ok_or_else(|| format!("a's {op} never reached the mock: {:?}", self.requests_of(id)))?;
        Ok(())
    }

    // --- the app's log ---

    fn log_lines(&self) -> Vec<String> {
        let file = self.app.log.join(teams_log::FILE_NAME);
        let text = fs::read_to_string(&file).unwrap_or_default();
        text.lines().map(str::to_owned).collect()
    }

    /// The flight proxy's request lines the app logged after `mark` (`<method> <host><path> -> HTTP <code> …`).
    fn proxy_lines(&self, mark: usize) -> Vec<String> {
        self.log_lines()
            .into_iter()
            .skip(mark)
            .filter(|line| line.contains("flightproxy") || line.contains("/api/v2/ep"))
            .collect()
    }

    /// Waits for the app's log to show a `<host>…/<op>` request after `mark` answered with a 2xx.
    fn wait_logged(&self, mark: usize, host: &str, op: &str) -> SandboxResult<String> {
        let found = wait_until(WAIT, || {
            self.proxy_lines(mark).into_iter().find(|line| {
                line.contains(&format!(" {host}/api/v2/ep/")) && line.contains(&format!("/{op} -> HTTP 2"))
            })
        });
        Ok(found.ok_or_else(|| {
            format!(
                "the app's log has no {op} on {host} with a 2xx: {:?}",
                self.proxy_lines(mark)
            )
        })?)
    }

    /// Copies the log's flight proxy lines (no payloads) to the evidence dir.
    fn keep_log_lines(&self) {
        if let Some(dir) = evidence_dir() {
            let lines = self.proxy_lines(0);
            let _ = fs::create_dir_all(&dir);
            let _ = fs::write(dir.join("flightproxy-requests.log"), lines.join("\n") + "\n");
        }
    }
}

/// a's first meeting of the next seven days that the app can join.
fn calendar_meeting(ctl: &mut Client) -> SandboxResult<CalendarRow> {
    let today = chrono::Local::now().date_naive();
    let last = today + chrono::Days::new(6);
    let request = Request::Calendar {
        from: Some(today.format("%Y-%m-%d").to_string()),
        to: Some(last.format("%Y-%m-%d").to_string()),
    };
    let found = wait_until(WAIT, || match ctl.request(None, request.clone()) {
        Ok(Response::Calendar { events }) => events
            .into_iter()
            .find(|event| event.online && event.joinable && !event.cancelled),
        _ => None,
    });
    Ok(found.ok_or("no joinable meeting in a's calendar")?)
}

// --- the checks ---

/// A join that connects on `host`: mute, raise a hand and hang up with the call window's keys; every write after the
/// start goes to `host` and succeeds, the start to the main host.
fn joins_on(user: &mut User<'_>, name: &str, host: &str) -> SandboxResult {
    let joined = user.join(name)?;
    user.wait_call("connected in the meeting", |row| row.state == "connected")?;
    let call = user.started_call(&joined)?;
    user.wait_other_window()?;
    let state = user.until("the call window", |state| state["call"]["status"] == "connected")?;
    say(&format!("  the call: {}", state["call"]));
    user.shot(&format!("{name}-connected"))?;
    let muted_before = user.count_of(&call.id, "updateEndpointState");
    user.press("ctrl+shift+m")?;
    user.wait_call("muted", |row| row.muted)?;
    user.wait_op(&call.id, "updateEndpointState", muted_before)?;
    let hands_before = user.count_of(&call.id, "publishState");
    user.press("ctrl+shift+k")?;
    user.wait_op(&call.id, "publishState", hands_before)?;
    user.shot(&format!("{name}-muted-hand-up"))?;
    let leaves_before = user.count_of(&call.id, "leave");
    user.press("ctrl+shift+h")?;
    user.wait_op(&call.id, "leave", leaves_before)?;
    let state = user.until("the call's end", |state| {
        state["call"].is_null() || state["call"]["status"] == "ended"
    })?;
    say(&format!("  after the leave: {} {}", state["call"], state["ended"]));
    let mark = joined.log_mark;
    for op in ["updateEndpointState", "publishState", "leave"] {
        let line = user.wait_logged(mark, host, op)?;
        say(&format!("  logged: {}", line_tail(&line)));
    }
    let lines = user.proxy_lines(mark);
    let start = format!("POST {MAIN}/api/v2/epconv -> HTTP 200");
    ensure(lines.iter().any(|line| line.contains(&start)), || {
        format!("no start on the main host in the app's log: {lines:?}")
    })?;
    let elsewhere: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("/api/v2/ep/") && !line.contains(&format!(" {host}/api/v2/ep/")))
        .collect();
    ensure(elsewhere.is_empty(), || {
        format!("in-call writes not on {host}: {elsewhere:?}")
    })?;
    let failed: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("/api/v2/ep") && !line.contains("-> HTTP 2"))
        .collect();
    ensure(failed.is_empty(), || format!("in-call writes that failed: {failed:?}"))?;
    say(&format!("  the mock's requests: {:?}", user.requests_of(&call.id)));
    Ok(())
}

fn regional(user: &mut User<'_>) -> SandboxResult {
    user.mock.hand_out_call_links_on(Some(APAC));
    joins_on(user, "regional", APAC)
}

fn main_host(user: &mut User<'_>) -> SandboxResult {
    user.mock.hand_out_call_links_on(None);
    joins_on(user, "main-host", MAIN)
}

/// The window's end of a join that failed.
fn failed_join(user: &mut User<'_>, name: &str) -> SandboxResult<(Joined, Value)> {
    user.wait_only_main()?;
    let before = user.state()?["ended"]["id"].clone();
    let joined = user.join(name)?;
    let state = user.until("the join's failure", |state| {
        !state["ended"].is_null() && state["ended"]["id"] != before && state["call"].is_null()
    })?;
    let ended = state["ended"].clone();
    say(&format!("  the end: {ended}"));
    user.shot(&format!("{name}-failed"))?;
    ensure(ended["end"] == "failed", || format!("the join did not fail: {ended}"))?;
    ensure(user.calls()?.iter().all(|row| row.state != "connected"), || {
        "a call is still connected".to_owned()
    })?;
    Ok((joined, ended))
}

/// The service refuses the start (HTTP 500): the normal error the user sees.
fn refused_start(user: &mut User<'_>) -> SandboxResult {
    user.mock.refuse_next_call_op("epconv", 500);
    let (_, ended) = failed_join(user, "refused-start")?;
    user.normal_failure = Some(ended);
    Ok(())
}

/// Links on `host`, not a flight proxy's: the join fails as a refused start does, and no request goes there.
fn look_alike(user: &mut User<'_>, name: &str, host: &str) -> SandboxResult {
    user.mock.hand_out_call_links_on(Some(host));
    let (joined, ended) = failed_join(user, &format!("look-alike-{name}"))?;
    let call = user.started_call(&joined)?;
    // the acceptance and any write it starts would come within this
    thread::sleep(Duration::from_secs(3));
    let after_start: Vec<(String, String)> = user
        .requests_of(&call.id)
        .into_iter()
        .filter(|(op, by)| by == LAB_A && op != "start")
        .collect();
    ensure(after_start.is_empty(), || {
        format!("requests reached the mock on {host}'s links: {after_start:?}")
    })?;
    let lines = user.proxy_lines(joined.log_mark);
    say(&format!(
        "  the log's flight proxy lines: {:?}",
        lines.iter().map(|line| line_tail(line)).collect::<Vec<_>>()
    ));
    let bare = host.split(':').next().unwrap_or(host);
    let start = format!("POST {MAIN}/api/v2/epconv -> HTTP 200");
    ensure(lines.iter().any(|line| line.contains(&start)), || {
        format!("no start on the main host in the app's log: {lines:?}")
    })?;
    let sent: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains(bare) || line.contains("/api/v2/ep/"))
        .collect();
    ensure(sent.is_empty(), || {
        format!("requests to {host} in the app's log: {sent:?}")
    })?;
    let normal = user.normal_failure.clone().ok_or("no normal error to compare with")?;
    ensure(
        ended["end"] == normal["end"] && ended["text"] == normal["text"] && ended["reason"] == normal["reason"],
        || format!("the end {ended} is not the normal error {normal}"),
    )
}

/// A log line from its method on.
fn line_tail(line: &str) -> &str {
    ["POST ", "PUT ", "GET ", "DELETE "]
        .iter()
        .filter_map(|method| line.find(method))
        .min()
        .map_or(line, |at| &line[at..])
}

// --- the child ---

/// The account's mock tokens; they never run out in a test.
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

fn env(name: &str) -> SandboxResult<String> {
    Ok(std::env::var(name).map_err(|_| format!("{name} isn't set"))?)
}

/// a's engine on the mock (its chats, calendar and calling), its calls' media on file devices, relayed through the
/// mock's TURN server.
fn engine(feed: VideoFeed, dir: &Path) -> SandboxResult<EngineHandle> {
    let tokens = Arc::new(Tokens {
        ic3: env(IC3_ENV)?,
        skypetoken: env(SKYPE_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let tracker = Arc::new(teams_call::Tracker::new(Mri::new(LAB_A)));
    let origin = env(ORIGIN_ENV)?;
    let trouter_origin = env(TROUTER_ENV)?;
    let http = || -> SandboxResult<HttpTransport<Tokens>> {
        Ok(HttpTransport::with_mock_origin(
            Arc::clone(&tokens),
            CHAT_BASE,
            &origin,
        )?)
    };
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
    let name = display_name("bot-a@lab.example.test").to_owned();
    let calling = trouter()?.for_calls(CallStack::new(Arc::clone(&tracker)));
    let identity = Identity {
        mri: Mri::new(LAB_A),
        display_name: Some(name.clone()),
        endpoint_id: String::new(),
        language_id: Some("en-us".into()),
        tenant_id: TENANT.into(),
        client: web_client_info(),
        region: Some("amer".into()),
        partition: Some("amer01".into()),
    };
    let devices: Arc<dyn Devices + Send + Sync> = Arc::new(FileDevices {
        input: AudioInput::Silence,
        recording: None,
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    });
    let media = CallMedia {
        devices,
        video: Some(teams_core::account::call_video(Arc::new(feed))),
        relay: Relay::Fixed(vec![RelayServer::udp(env(RELAY_ENV)?.parse()?)]),
        session: SessionConfig {
            processing: ProcessingConfig::OFF,
            ..Default::default()
        },
    };
    let calls = Some(CallParts::new(calling, identity, media));
    let push = Push::new(trouter()?);
    let client = http()?
        .with_ams(AMS_BASE)?
        .with_presence(PRESENCE_BASE)?
        .with_csa(CSA_BASE)?
        .with_user_profile(USER_PROFILE_BASE)?
        .with_provisioning(PROVISIONING_BASE)?
        .with_middle_tier(MIDDLE_TIER_BASE)?;
    Ok(engine::start(Parts {
        calls,
        store: Store::open(&support::store_file(dir, Alias::A))?,
        client: Arc::new(ChatClient::new(client)),
        outbox: Outbox::new(transport, Author::new(Mri::new(LAB_A), name))
            .with_calls(Arc::clone(&tracker), teams_core::CallFloor::new()),
        push: Some(push),
        options: EngineOptions::default(),
        known_names: vec![
            (LAB_B.to_owned(), "Lab B".to_owned()),
            (LAB_C.to_owned(), "Lab C".to_owned()),
        ],
    })?)
}

/// The app's log at `info` (no payloads: each request's method, host and path) in [`LOG_ENV`]'s directory.
fn app_log() -> SandboxResult {
    let config = teams_log::Config {
        dir: PathBuf::from(env(LOG_ENV)?),
        setting: teams_log::Level::Info,
        env: None,
        rotation: teams_log::file::Rotation::default(),
    };
    teams_log::install(&config).map_err(|error| format!("the app's log: {error}"))?;
    Ok(())
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    app_log()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let feed = VideoFeed::new();
    let engine = engine(feed.clone(), &dir)?;
    let first = support::started(Alias::A, engine)?;
    let runner = support::runner(&dir)?;
    let settings = SettingsFile::new(dir.join("settings.toml"));
    settings
        .save(&teams_ui::settings::Settings::default())
        .map_err(|problem| format!("{problem:?}"))?;
    let ctl = PathBuf::from(env(CTL_ENV)?);
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let calls = Some(PersonalCalls {
            ringer: None,
            sounds: None,
            echo: Mri::new(ECHO),
            video: Some(feed),
            camera: None,
            devices: None,
            noise: teams_media::processing::NoiseControl::default(),
        });
        let launched = app::launch(
            cx,
            Launch {
                first: ngteams::accounts::Started { calls, ..first },
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
                let window = launched.window;
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, 3, |_, _| say("ready"));
                });
                answer(window, cx);
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

fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
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
            cx.background_executor().timer(Duration::from_millis(10)).await;
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
            let reply = cx.update(|cx| on_app(window, verb, cx));
            say(&format!("re {seq} {}", reply.replace('\n', " ")));
        }
    })
    .detach();
}

fn on_app(window: WindowHandle<KitRoot>, verb: &str, cx: &mut App) -> String {
    window
        .update(cx, |kit, _, cx| {
            let shell = kit
                .view()
                .clone()
                .downcast::<Root>()
                .ok()?
                .read(cx)
                .shell_in(cx)
                .cloned()?;
            Some(match verb {
                "state" => state_of(&shell, cx).to_string(),
                other => format!("unknown {other}"),
            })
        })
        .ok()
        .flatten()
        .unwrap_or_else(|| "no shell".to_owned())
}

/// What the window shows of the meeting: the pre-join, the call and how the last call ended (its words and why).
fn state_of(shell: &Entity<Shell>, cx: &App) -> Value {
    let calls = shell.read(cx).models().calls.read(cx);
    let prejoin = calls.prejoin().map(|prejoin| match &prejoin.to_join {
        MeetingToJoin::Chat { chat, title, .. } => json!({"chat": chat, "title": title}),
        MeetingToJoin::Code(_) => json!({"code": true}),
    });
    let call = calls.call().map(|call| {
        json!({
            "title": call.title,
            "status": teams_ui::calls::status_name(&call.status),
            "text": teams_ui::calls::status_text(call),
            "chat": call.chat,
        })
    });
    let ended = calls.ended().map(|ended| {
        json!({
            "id": ended.id.get(),
            "end": teams_ui::calls::end_name(&ended.end),
            "text": teams_ui::calls::end_text(&ended.end, ended.voicemail),
            "reason": format!("{:?}", ended.reason(LAB_A)),
            "detail": format!("{:?}", ended.end),
        })
    });
    json!({
        "prejoin": prejoin,
        "call": call,
        "ended": ended,
    })
}
