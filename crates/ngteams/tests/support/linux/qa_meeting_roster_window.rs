//! QA of task 798 (rounds 1 and 2), in the UI sandbox: who a scheduled meeting shows as in it, and who as only invited, as
//! a user meets it. The real app runs as a child in the sandbox, as lab account a on `teams-mock` (its chat service,
//! its calling service, its Trouter and its group video). The parent drives it with the sandbox's pointer and keyboard,
//! reads the call from the child (a file in the sandbox dir), the writes from the mock, and the People pane from
//! screenshots: each row there has a round avatar, so the avatars in the pane's left column count the rows, and a gap
//! a heading tall splits its sections. Every name, id and text is made up.
//!
//! - **Alone** (`meeting_alone`): a meeting of a and ten invitees (b and nine made-up people). Its call runs on the
//!   mock with nobody in it; a joins it from the meeting chat's header Join and the pre-join's Join now. The app knows
//!   the meeting chat only from its chat list (no `ThreadUpdate` comes), and has its invitees from it. The call has
//!   no other participant and the invitees do not stand in for them; the window's title is the meeting's subject; People shows
//!   one row in its first section (a) and ten in its second ("Others invited (10)"). Shots of the stage and of People.
//! - **b joins, then leaves** (`b_joins_then_leaves`): b's tile and row come (two rows, then nine), then go (one, ten).
//! - **Not people** (`not_people`): the account's own entry under its MRI in upper case with two endpoints and the
//!   camera on, and a nameless app (`28:`) that only listens: neither is a participant, People stays 1 + 10. An
//!   invitee who joins after them shows that the app applied both entries.
//! - **The lobby** (`lobby`): an invitee waits in the lobby: People shows the lobby's row, a, and nine invited.
//! - **Keyboard** (`keyboard`): Tab reaches each heading and row of People in the drawn order, Shift+Tab goes back the
//!   same way (the child names the row the keyboard is on); Enter folds "Others invited", Space unfolds it, and the
//!   other way round.
//! - **Themes** (`themes`): People and the stage alone in Teams Light and Teams Dark, and People with the keyboard
//!   on the "Others invited" heading and on its first row (the focus ring) in each.
//! - **Request to join in a meeting** (`invited_request`, task 810): the pointer on an invited row shows Request to
//!   join, in Teams Light and Teams Dark (a shot of each); its click sends exactly one `add`, for that invitee, naming
//!   the meeting's chat, and the invitee moves to "In this meeting" (Calling…, then No response: the mock has nothing
//!   of theirs to ring).
//! - **No regression**: a 1:1 call to b and a group call to b and c, before an answer, name the chat's people
//!   (`one_to_one_ringing`, `group_ringing`), and People lists b and c under a; in a's group call that b joined after the ring, "People added to the
//!   chat" shows c, and Request to join, on hover, sends exactly one `add`, for c (`group_request_to_join`).
//!
//! - **Request to join, task 810 QA round 1** (`request_states`, `request_keyboard`, `request_refusals`,
//!   `invitee_ring`): see `qa_meeting_request.rs`.
//!
//! `QA798_EVIDENCE` names a directory that gets the screenshots and the results too. `QA798_ONLY` runs only the named
//! checks (comma-separated; the meeting checks need `meeting_alone` first). Screenshots also go to
//! `target/ui-shots/qa798-*.png`. Without weston 15, niri or `dbus-daemon` it says so and passes, unless
//! `GPUI_SMOKE_REQUIRE=1`.

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
use image::RgbImage;
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
use teams_ctl::protocol::{CallRow, OpenTarget, Request, Response};
use teams_ctl::{Client, Timeouts};
use teams_media::devices::Devices;
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::processing::ProcessingConfig;
use teams_media::relay::RelayServer;
use teams_media::session::SessionConfig;
use teams_media::tone::Tone;
use teams_mock::{
    AB_CHAT, ABC_CHAT, AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE,
    PRESENCE_BASE, PROVISIONING_BASE, TENANT, TIMED_OUT, USER_PROFILE_BASE,
};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use teams_ui::call_stage::CallStage;
use teams_ui::shell::{Main, Shell};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use teams_ui::{SettingsFile, VideoFeed};
use tokio::sync::broadcast;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "../window.rs"]
mod support;
use support::{Alias, display_name};
#[path = "call_pixels.rs"]
#[expect(
    dead_code,
    reason = "the call window's pixel finders are shared; this harness uses some of them"
)]
mod pixels;
use pixels::Rect;
#[path = "qa_meeting_request.rs"]
mod request;

const CHILD_ENV: &str = "QA798_CHILD";
const EVIDENCE_ENV: &str = "QA798_EVIDENCE";
const ONLY_ENV: &str = "QA798_ONLY";
const TRACE_FD_ENV: &str = "QA798_TRACE_FD";
const ORIGIN_ENV: &str = "QA798_ORIGIN";
const TROUTER_ENV: &str = "QA798_TROUTER";
const RELAY_ENV: &str = "QA798_RELAY";
const IC3_ENV: &str = "QA798_IC3";
const SKYPE_ENV: &str = "QA798_SKYPE";
const DIR_ENV: &str = "QA798_DIR";
const CTL_ENV: &str = "QA798_CTL";
/// The file in the sandbox dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "qa798-ask";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(100);
const SETTLE: Duration = Duration::from_millis(900);
const STEP: Duration = Duration::from_millis(250);
/// niri's layout: each window takes the whole screen, and no hot corner.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
/// Tall enough for People to show a, ten invitees and both headings without a scroll.
const SCREEN: (u32, u32) = (1280, 960);
const PEER_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.2,
};
/// The call window's toolbar lies within this many px of its top.
const TOOLBAR_BAND: i32 = 58;
/// The People pane's width (`call_stage::PANE_WIDTH`) and the height of its title row.
const PANE: (i32, i32) = (320, 48);
/// The made-up scheduled meeting's chat, organized by a.
const MEETING: &str = "19:meeting_MadeUpRosterCheck798@thread.v2";
const SUBJECT: &str = "Made-up roster check";
/// The nine made-up invitees besides b.
const INVITEES: [(&str, &str); 9] = [
    ("8:orgid:00000000-0000-4000-8000-000000798001", "Ada Made"),
    ("8:orgid:00000000-0000-4000-8000-000000798002", "Bo Made"),
    ("8:orgid:00000000-0000-4000-8000-000000798003", "Cy Made"),
    ("8:orgid:00000000-0000-4000-8000-000000798004", "Di Made"),
    ("8:orgid:00000000-0000-4000-8000-000000798005", "Ed Made"),
    ("8:orgid:00000000-0000-4000-8000-000000798006", "Fay Made"),
    ("8:orgid:00000000-0000-4000-8000-000000798007", "Gus Made"),
    ("8:orgid:00000000-0000-4000-8000-000000798008", "Hal Made"),
    ("8:orgid:00000000-0000-4000-8000-000000798009", "Ivy Made"),
];
/// A made-up recorder: an app with no name that only listens.
const RECORDER: &str = "28:00000000-0000-4000-8000-000000798ec0";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_meeting_roster_window: {error}");
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

fn u32_of(value: i32) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(0)
}

fn i32_of(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

// --- the parent ---

type Check = (&'static str, fn(&mut User<'_>) -> SandboxResult);

const CHECKS: [Check; 14] = [
    ("meeting_alone", meeting_alone),
    ("b_joins_then_leaves", b_joins_then_leaves),
    ("not_people", not_people),
    ("lobby", lobby),
    ("keyboard", keyboard),
    ("themes", themes),
    ("invited_request", invited_request),
    ("request_states", request::request_states),
    ("request_keyboard", request::request_keyboard),
    ("request_refusals", request::request_refusals),
    ("invitee_ring", request::invitee_ring),
    ("one_to_one_ringing", one_to_one_ringing),
    ("group_ringing", group_ringing),
    ("group_request_to_join", group_request_to_join),
];

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_meeting_roster_window", &options)? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mock = runtime.block_on(ChatServiceMock::start())?;
    let mut members = vec![LAB_A, LAB_B];
    members.extend(INVITEES.iter().map(|(mri, _)| *mri));
    ensure(mock.add_meeting_chat(MEETING, SUBJECT, &members), || {
        "no meeting chat on the mock".to_owned()
    })?;
    let trouter = mock.start_trouter()?;
    let dir = tempfile::tempdir()?;
    let relay = mock.start_group_video(dir.path(), PEER_TONE)?.to_string();
    let only: Option<Vec<String>> = std::env::var(ONLY_ENV)
        .ok()
        .map(|names| names.split(',').map(|name| name.trim().to_owned()).collect());
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    {
        let app = AppUnderTest::start(&sandbox, &mock, &relay, trouter.origin(), &dir.path().join("a"))?;
        let mut user = User::new(&sandbox, &mock, app)?;
        for (name, check) in CHECKS {
            if only
                .as_ref()
                .is_some_and(|only| !only.iter().any(|wanted| wanted == name))
            {
                continue;
            }
            user.run_check(name, check, &mut passed, &mut failed);
        }
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
    if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(PathBuf::from(dir).join("results.txt"), &summary);
    }
    say(&format!(
        "qa_meeting_roster_window: {} passed, {} failed",
        passed.len(),
        failed.len()
    ));
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("{} checks failed", failed.len()).into())
    }
}

/// The child app in its sandbox: its process, its trace, its control socket.
struct AppUnderTest {
    running: Option<Running>,
    trace: TraceReader,
    socket: PathBuf,
}

impl AppUnderTest {
    fn start(sandbox: &Sandbox, mock: &ChatServiceMock, relay: &str, trouter: &str, own: &Path) -> SandboxResult<Self> {
        fs::create_dir_all(own)?;
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let ic3 = mock.token(LAB_A).ok_or("the mock's token")?;
        let skype = mock.skypetoken(LAB_A).ok_or("the mock's skypetoken")?;
        let socket = sandbox.path("run/ctl.sock");
        let own_text = own.to_string_lossy().into_owned();
        let socket_text = socket.to_string_lossy().into_owned();
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
            trace,
            socket,
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

    fn catch_up(&mut self) -> &[Value] {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
        &self.seen
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

/// The meeting's call on the mock, once a joined it.
#[derive(Clone, Debug)]
struct MeetingCall {
    call: String,
}

/// The People pane's rows as their avatars show them, top to bottom: the sections' sizes, split where a heading lies
/// between two rows.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Rows {
    sections: Vec<usize>,
    avatars: Vec<Rect>,
}

/// The user at the sandbox: the pointer, the keyboard, the screenshots, the control socket, and the child it asks
/// about the window.
struct User<'a> {
    sandbox: &'a Sandbox,
    mock: &'a ChatServiceMock,
    app: AppUnderTest,
    input: VirtualInput,
    ctl: Client,
    asked: u32,
    main_window: Option<u64>,
    meeting: Option<MeetingCall>,
    /// The toolbar's People, once found.
    people_button: Option<Rect>,
    /// People shows, as the clicks left it.
    people_open: bool,
    theme: &'static str,
}

impl<'a> User<'a> {
    fn new(sandbox: &'a Sandbox, mock: &'a ChatServiceMock, app: AppUnderTest) -> SandboxResult<Self> {
        let socket = app.socket.clone();
        let ctl = wait_until(WAIT, || Client::connect(&socket, Timeouts::default()).ok())
            .ok_or_else(|| format!("no control socket at {}", socket.display()))?;
        let mut user = Self {
            sandbox,
            mock,
            app,
            input: sandbox.input()?,
            ctl,
            asked: 0,
            main_window: None,
            meeting: None,
            people_button: None,
            people_open: false,
            theme: "light",
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
                let _ = self.set_theme("light");
                say(&format!("  FAIL {name}: {error}\n  state: {state}"));
                failed.push((name.to_owned(), error));
            }
        }
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
        let _ = self.hang_up();
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

    fn click(&mut self, (x, y): (i32, i32)) -> SandboxResult {
        let (x, y) = (u32_of(x), u32_of(y));
        self.input.move_to(x, y)?;
        thread::sleep(Duration::from_millis(80));
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn hover(&mut self, (x, y): (i32, i32)) -> SandboxResult {
        self.input.move_to(u32_of(x), u32_of(y))?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn park(&mut self) -> SandboxResult {
        // the window's bottom left corner: no button, tooltip, row or tile there
        self.input.move_to(2, SCREEN.1 - 2)
    }

    /// The focused window as `qa798-<name>.png`, under `target/ui-shots/` and the evidence dir.
    fn shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        let file = format!("qa798-{name}.png");
        let png = self.sandbox.screenshot(&file, true)?;
        let kept = shots_dir()?.join(&file);
        fs::copy(&png, &kept)?;
        if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
            let dir = PathBuf::from(dir).join("shots");
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(&file))?;
        }
        say(&format!("  shot qa798-{name}.png"));
        Ok(kept)
    }

    /// The screen as an image, the pointer parked (the call window fills it).
    fn screen(&mut self) -> SandboxResult<RgbImage> {
        self.park()?;
        thread::sleep(SETTLE);
        self.screen_as_is()
    }

    fn screen_as_is(&mut self) -> SandboxResult<RgbImage> {
        let png = self.sandbox.screenshot("qa798-find.png", false)?;
        let image = image::open(&png)?.to_rgb8();
        let _ = fs::remove_file(png);
        Ok(image)
    }

    fn set_theme(&mut self, theme: &'static str) -> SandboxResult {
        self.ask(&format!("theme-{theme}"))?;
        self.theme = theme;
        thread::sleep(SETTLE);
        Ok(())
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

    fn focus_main(&mut self) -> SandboxResult {
        let id = self.main_window.ok_or("no main window")?;
        self.sandbox
            .niri_msg(&["action", "focus-window", "--id", &id.to_string()])?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The window besides the main window (the pre-join's or the call's) takes the focus; its title.
    fn focus_other(&mut self) -> SandboxResult<String> {
        let main = self.main_window;
        let (id, title) = wait_until(WAIT, || {
            self.windows().ok()?.iter().find_map(|window| {
                let id = window["id"].as_u64()?;
                (Some(id) != main).then(|| (id, window["title"].as_str().unwrap_or_default().to_owned()))
            })
        })
        .ok_or("no window besides the main window")?;
        self.sandbox
            .niri_msg(&["action", "focus-window", "--id", &id.to_string()])?;
        thread::sleep(SETTLE);
        Ok(title)
    }

    /// The call window's title, as the compositor lists it.
    fn call_title(&mut self) -> SandboxResult<String> {
        self.focus_other()
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

    fn hang_up(&mut self) -> SandboxResult {
        if self.calls()?.is_empty() {
            return Ok(());
        }
        self.ctl.request(None, Request::HangUp)?;
        wait_until(WAIT, || {
            self.calls()
                .ok()
                .filter(|calls| calls.iter().all(|row| row.state != "connected" && !row.ringing))
                .map(|_| ())
        })
        .ok_or("the call never ended")?;
        self.people_button = None;
        self.people_open = false;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn open_chat(&mut self, chat: &str) -> SandboxResult {
        self.focus_main()?;
        self.ctl.request(
            None,
            Request::Open {
                target: OpenTarget::Chat { chat: chat.to_owned() },
            },
        )?;
        self.until("the chat open", |state| state["main"] == format!("chat:{chat}"))?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The members of `chat` the app has, as the control socket gives them.
    fn members(&mut self, chat: &str) -> SandboxResult<String> {
        let answer = self.ctl.request(None, Request::Members { chat: chat.to_owned() })?;
        Ok(format!("{answer:?}"))
    }

    /// How many requests `op` a sent on the mock's calls.
    fn sent(&self, op: &str) -> usize {
        self.mock
            .calls()
            .iter()
            .flat_map(|call| call.requests.iter())
            .filter(|(sent, by)| sent == op && by == LAB_A)
            .count()
    }

    // --- the meeting's call window ---

    fn meeting(&self) -> SandboxResult<MeetingCall> {
        self.meeting.clone().ok_or_else(|| "a is not in the meeting".into())
    }

    /// The call window's rectangle: it fills the screen.
    #[expect(
        clippy::unused_self,
        reason = "the call window's place is the user's to find, as the screen is"
    )]
    fn window(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: i32_of(SCREEN.0),
            h: i32_of(SCREEN.1),
        }
    }

    /// The People pane's rectangle, under its title row.
    fn pane(&self) -> Rect {
        let window = self.window();
        Rect {
            x: window.right() - PANE.0,
            y: window.y + TOOLBAR_BAND + PANE.1,
            w: PANE.0,
            h: window.h - TOOLBAR_BAND - PANE.1,
        }
    }

    /// The toolbar's People: the button left of Raise, which Ctrl+Shift+K turns on and off.
    fn people_button(&mut self) -> SandboxResult<Rect> {
        if let Some(button) = self.people_button {
            return Ok(button);
        }
        self.focus_other()?;
        let window = self.window();
        let band = Rect {
            h: TOOLBAR_BAND,
            ..window
        };
        let deadline = Instant::now() + WAIT;
        let raise = loop {
            let before = self.screen()?;
            self.press("ctrl+shift+k")?;
            thread::sleep(SETTLE);
            let on = self.screen()?;
            self.press("ctrl+shift+k")?;
            thread::sleep(SETTLE);
            let off = self.screen()?;
            if let Some(button) = pixels::toggled_button(&before, &on, &off, band) {
                break button;
            }
            if Instant::now() > deadline {
                return Err("no Raise in the toolbar changed with Ctrl+Shift+K".into());
            }
        };
        let shot = self.screen()?;
        let groups = pixels::white_groups(
            &shot,
            Rect {
                x: window.x + 4,
                y: window.y + 6,
                w: window.w - 8,
                h: TOOLBAR_BAND - 12,
            },
        );
        let at = groups
            .iter()
            .position(|group| group.x <= raise.center().0 && raise.center().0 < group.right())
            .ok_or_else(|| format!("Raise {raise:?} is in no group of {groups:?}"))?;
        let people = at
            .checked_sub(1)
            .and_then(|index| groups.get(index).copied())
            .ok_or_else(|| format!("nothing left of Raise in {groups:?}"))?;
        say(&format!("  People at {people:?} (Raise at {raise:?})"));
        self.people_button = Some(people);
        Ok(people)
    }

    /// Opens People with its toolbar button (`open`), or closes it, unless it is so: the pane comes or goes.
    fn toggle_people(&mut self, open: bool) -> SandboxResult {
        if self.people_open == open {
            return Ok(());
        }
        let button = self.people_button()?;
        let pane = Rect {
            y: self.window().y + TOOLBAR_BAND,
            h: self.window().h - TOOLBAR_BAND,
            ..self.pane()
        };
        let before = self.screen()?;
        self.click(button.center())?;
        let deadline = Instant::now() + WAIT;
        loop {
            let now = self.screen()?;
            if pixels::changed_pixels(&before, &now, pane) >= 3_000 {
                break;
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "People's click did not {} the pane",
                    if open { "open" } else { "close" }
                )
                .into());
            }
        }
        self.people_open = open;
        Ok(())
    }

    fn open_people(&mut self) -> SandboxResult {
        self.toggle_people(true)
    }

    /// The People pane's rows, read from their avatars: round faces about 32 px across in the pane's left column.
    fn rows(&mut self) -> SandboxResult<Rows> {
        let shot = self.screen()?;
        Ok(rows_in(&shot, self.pane(), self.theme == "dark"))
    }

    /// Waits until People shows `sections` (sizes, top to bottom).
    fn until_rows(&mut self, what: &str, sections: &[usize]) -> SandboxResult<Rows> {
        let deadline = Instant::now() + WAIT;
        loop {
            let rows = self.rows()?;
            if rows.sections == sections {
                say(&format!("  People: {:?}", rows.sections));
                return Ok(rows);
            }
            if Instant::now() > deadline {
                return Err(format!("{what}: People shows {:?}, not {sections:?}", rows.sections).into());
            }
            thread::sleep(POLL);
        }
    }

    fn participants(state: &Value) -> Vec<String> {
        state["call"]["participants"]
            .as_array()
            .map(|all| {
                all.iter()
                    .filter_map(|participant| participant["mri"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Waits until the call's participants are `mris`.
    fn until_participants(&mut self, what: &str, mris: &[&str]) -> SandboxResult<Value> {
        self.until(what, |state| Self::participants(state) == mris)
    }
}

/// The rows of a People pane in `pane` of `shot`, by their avatars: blobs off the pane's ground 20 to 44 px across in
/// the pane's left 54 px (a name starts 60 px in), none cut by that column's edge (a button's outline is).
fn rows_in(shot: &RgbImage, pane: Rect, dark: bool) -> Rows {
    let column = Rect { w: 54, ..pane };
    // the ground of the same line at the pane's left edge: a row the keyboard is on is filled
    let ground = |y: i32| {
        shot.get_pixel_checked(u32_of(pane.x + 6), u32_of(y))
            .map(|pixel| pixel.0)
    };
    let off_ground = |x: i32, y: i32| {
        let Some(ground) = ground(y) else { return false };
        shot.get_pixel_checked(u32_of(x), u32_of(y)).is_some_and(|pixel| {
            let distance: i32 = pixel
                .0
                .iter()
                .zip(ground)
                .map(|(one, two)| (i32::from(*one) - i32::from(two)).abs())
                .sum();
            distance > if dark { 18 } else { 14 }
        })
    };
    let mut avatars: Vec<Rect> = pixels::blobs(column, off_ground)
        .into_iter()
        .map(|blob| blob.bounds)
        .filter(|bounds| {
            (20..=44).contains(&bounds.w)
                && (20..=44).contains(&bounds.h)
                && bounds.x > column.x + 2
                && bounds.right() < column.right() - 1
        })
        .collect();
    avatars.sort_by_key(|avatar| avatar.y);
    let mut sections = Vec::new();
    let mut last: Option<Rect> = None;
    for avatar in &avatars {
        match (last, sections.last_mut()) {
            // rows are 40 to 48 px apart; a heading between two adds 28 px or more
            (Some(previous), Some(count)) if avatar.y - previous.y <= 60 => *count += 1,
            _ => sections.push(1),
        }
        last = Some(*avatar);
    }
    Rows { sections, avatars }
}

// --- the roster entries the mock tells whole ---

/// An endpoint in the call, with its main audio and video going `audio` and `video`.
fn in_call(audio: &str, video: &str) -> Value {
    json!({"call": {
        "mediaStreams": [
            {"type": "audio", "label": "main-audio", "direction": audio, "serverMuted": false},
            {"type": "video", "label": "main-video", "direction": video, "serverMuted": false},
        ],
        "serverMuteVersion": 0,
    }})
}

fn whole(mri: &str, name: Option<&str>, endpoints: &Value) -> Value {
    let mut details = json!({"id": mri});
    if let Some(name) = name {
        details["displayName"] = json!(name);
    }
    json!({"state": "active", "details": details, "endpoints": endpoints})
}

// --- the meeting ---

/// The meeting's call starts on the mock (nobody in it, as when its organizer's earlier call emptied), its start
/// posted in the chat as the service posts it; a opens the chat, and its header's Join and the pre-join's Join now
/// take a in.
fn join_the_meeting(user: &mut User<'_>) -> SandboxResult<MeetingCall> {
    let call = user.mock.ring_group(LAB_B, MEETING, &[]);
    ensure(user.mock.end_ring(&call, 0, TIMED_OUT), || {
        "no meeting call on the mock".to_owned()
    })?;
    let conversation = user
        .mock
        .calls()
        .into_iter()
        .find(|known| known.id == call)
        .map(|known| known.conversation)
        .ok_or("no call on the mock")?;
    let start = format!(
        "<partlist type=\"started\" alt=\"\"><part identity=\"{LAB_B}\"><name>{LAB_B}</name></part></partlist>\
         <callEventType>callStarted</callEventType><conversationUrl>{}</conversationUrl><callId>{call}</callId>",
        link(&conversation)
    );
    user.mock
        .add_message(MEETING, (LAB_B, "Lab B"), "Event/Call", &start)
        .ok_or("no meeting chat on the mock")?;
    user.open_chat(MEETING)?;
    meeting_people_from_the_list(user)?;
    wait_until(WAIT, || {
        user.app
            .trace
            .catch_up()
            .iter()
            .any(|line| line["view"] == "call-button" && line.to_string().contains(MEETING))
            .then_some(())
    })
    .ok_or("the meeting chat's header shows no call button")?;
    thread::sleep(SETTLE);
    user.shot("meeting-chat-header")?;
    let screen = user.screen()?;
    let join = header_join(&screen).ok_or("no Join in the meeting chat's header")?;
    say(&format!("  the header's Join at {join:?}"));
    user.click(join)?;
    user.until("the pre-join", |state| !state["prejoin"].is_null())?;
    user.focus_other()?;
    user.shot("meeting-prejoin")?;
    user.press("enter")?;
    user.wait_call("connected in the meeting", |row| row.state == "connected")?;
    // a's join reached the meeting's call, or a's start made it, in the meeting chat
    let call = wait_until(WAIT, || {
        user.mock
            .calls()
            .into_iter()
            .rev()
            .find(|known| {
                known.group_chat.as_deref() == Some(MEETING) && known.requests.iter().any(|(_, by)| by == LAB_A)
            })
            .map(|known| {
                say(&format!("  a's call on the mock: {} {:?}", known.id, known.requests));
                known.id
            })
    })
    .ok_or("a's join did not reach a call in the meeting chat")?;
    say(&format!("  the members the app has: {}", user.members(MEETING)?));
    let meeting = MeetingCall { call };
    user.meeting = Some(meeting.clone());
    Ok(meeting)
}

/// The app has the meeting chat's people from its chat list's sync, with no `ThreadUpdate` for it.
fn meeting_people_from_the_list(user: &mut User<'_>) -> SandboxResult {
    wait_until(WAIT, || {
        user.members(MEETING)
            .ok()
            .filter(|members| members.matches("mri").count() >= 11)
    })
    .ok_or("the app has not the meeting chat's people from its chat list")?;
    say(&format!(
        "  the members the app has from the list: {}",
        user.members(MEETING)?
    ));
    Ok(())
}

/// Where the mock's call at `conversation` runs, as a call's post names it.
fn link(conversation: &str) -> String {
    format!(
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/{conversation}?i=10-0-0-2&amp;e=1"
    )
}

/// A pixel of the brand fill (a button's).
fn brand(pixel: image::Rgb<u8>) -> bool {
    let [r, g, b] = pixel.0.map(i32::from);
    b >= 150 && b - r >= 60 && b - g >= 60
}

/// The chat header's Join: the brand-filled button at the right of the header.
fn header_join(image: &RgbImage) -> Option<(i32, i32)> {
    let within = Rect {
        x: i32_of(image.width().saturating_sub(300)),
        y: 54,
        w: 290,
        h: 36,
    };
    pixels::blobs(within, |x, y| {
        image
            .get_pixel_checked(u32_of(x), u32_of(y))
            .is_some_and(|pixel| brand(*pixel))
    })
    .into_iter()
    .map(|blob| blob.bounds)
    .filter(|bounds| bounds.w >= 40 && bounds.h >= 20)
    .max_by_key(|bounds| bounds.right())
    .map(Rect::center)
}

/// The meeting's title is its subject: the chat's name, not the roster's or the invitees' names.
fn subject_title(text: &str) -> SandboxResult {
    ensure(text == SUBJECT, || {
        format!("the title is {text:?}, not the meeting's subject")
    })
}

// --- the checks ---

/// a joins the meeting alone: no participant, no stand-in; the title is the subject; People: a, then ten invited.
fn meeting_alone(user: &mut User<'_>) -> SandboxResult {
    join_the_meeting(user)?;
    let state = user.until("the call's state", |state| state["call"]["status"] == "connected")?;
    let call = state["call"].clone();
    say(&format!("  the call: {call}"));
    ensure(call["meeting"] == true, || format!("not a meeting's call: {call}"))?;
    ensure(call["stand_in"] == false, || {
        format!("the invitees stand in for the roster: {call}")
    })?;
    ensure(User::participants(&state).is_empty(), || {
        format!("someone is in the call besides a: {call}")
    })?;
    let title = user.call_title()?;
    say(&format!("  the call window's title: {title:?}"));
    subject_title(&title)?;
    thread::sleep(SETTLE);
    user.park()?;
    user.shot("alone-stage-light")?;
    let shown = user
        .app
        .trace
        .catch_up()
        .iter()
        .filter(|line| line["view"] == "call-participant" && line["change"] == "shown")
        .count();
    ensure(shown == 0, || {
        format!("{shown} participant tiles traced while a is alone")
    })?;
    user.open_people()?;
    user.until_rows("a alone", &[1, 10])?;
    user.shot("alone-people-light")?;
    user.focus_main()?;
    user.shot("alone-main-window-light")?;
    user.focus_other()?;
    Ok(())
}

/// b joins: b's tile and row, nine invited; b leaves: one and ten again, the tile gone.
fn b_joins_then_leaves(user: &mut User<'_>) -> SandboxResult {
    let meeting = user.meeting()?;
    user.open_people()?;
    ensure(user.mock.join_made_up(&meeting.call, &[(LAB_B, "Lab B")]), || {
        "b did not join on the mock".to_owned()
    })?;
    user.until_participants("b in the call", &[LAB_B])?;
    user.until_rows("b joined", &[2, 9])?;
    user.shot("b-joined-people")?;
    let traced = user.app.trace.catch_up().iter().any(|line| {
        line["view"] == "call-participant" && line["change"] == "shown" && line.to_string().contains(LAB_B)
    });
    ensure(traced, || "no tile for b on the stage".to_owned())?;
    let title = user.call_title()?;
    say(&format!("  the title with b: {title:?}"));
    ensure(user.mock.leave_made_up(&meeting.call, LAB_B), || {
        "b did not leave on the mock".to_owned()
    })?;
    user.until_participants("b gone", &[])?;
    user.until_rows("b left", &[1, 10])?;
    user.shot("b-left-people")?;
    let title = user.call_title()?;
    say(&format!("  the title after b left: {title:?}"));
    subject_title(&title)
}

/// The account's own entry in upper case with two endpoints and the camera on, and a nameless listening app: nobody
/// new in the call or in People.
fn not_people(user: &mut User<'_>) -> SandboxResult {
    let meeting = user.meeting()?;
    user.open_people()?;
    let upper = LAB_A.to_uppercase().replace("8:ORGID:", "8:orgid:");
    let own = whole(
        &upper,
        Some("bot-a"),
        &json!({
            "00000000-0000-4000-8000-0000007980a1": in_call("sendrecv", "sendrecv"),
            "00000000-0000-4000-8000-0000007980a2": in_call("sendrecv", "recvonly"),
        }),
    );
    ensure(user.mock.tell_roster_entry(&meeting.call, &upper, own), || {
        "the own entry was not told".to_owned()
    })?;
    let recorder = whole(
        RECORDER,
        None,
        &json!({"00000000-0000-4000-8000-000000798ec1": in_call("recvonly", "recvonly")}),
    );
    ensure(user.mock.tell_roster_entry(&meeting.call, RECORDER, recorder), || {
        "the recorder's entry was not told".to_owned()
    })?;
    // the roster comes in order: once the marker is in, the app has applied the two entries before it
    let (marker, name) = INVITEES[1];
    ensure(user.mock.join_made_up(&meeting.call, &[(marker, name)]), || {
        "the marker did not join on the mock".to_owned()
    })?;
    let state = user.until_participants("only the marker in the call", &[marker])?;
    say(&format!("  the call: {}", state["call"]));
    user.until_rows("the marker in, not the own entry or the recorder", &[2, 9])?;
    user.shot("not-people")?;
    ensure(user.mock.leave_made_up(&meeting.call, marker), || {
        "the marker did not leave on the mock".to_owned()
    })?;
    user.until_participants("the marker gone", &[])?;
    user.until_rows("after the marker left", &[1, 10])?;
    let title = user.call_title()?;
    subject_title(&title)?;
    ensure(!title.contains("bot-a") && !title.contains("Unknown"), || {
        format!("the title names a or someone unknown: {title:?}")
    })
}

/// An invitee waits in the lobby: its row in the lobby's section, a, nine invited; let go: one and ten.
fn lobby(user: &mut User<'_>) -> SandboxResult {
    let meeting = user.meeting()?;
    user.open_people()?;
    let (waiter, name) = INVITEES[0];
    let waiting = whole(
        waiter,
        Some(name),
        &json!({"00000000-0000-4000-8000-0000007981b1": {"lobby": {"mediaStreams": []}}}),
    );
    ensure(user.mock.tell_roster_entry(&meeting.call, waiter, waiting), || {
        "the lobby's entry was not told".to_owned()
    })?;
    user.until("the lobby", |state| {
        state["call"]["lobby"].as_array().is_some_and(|lobby| lobby.len() == 1)
    })?;
    user.until_rows("one in the lobby", &[1, 1, 9])?;
    user.shot("lobby-people")?;
    let gone = json!({"state": "inactive", "details": {"id": waiter, "displayName": name}, "endpoints": {}});
    ensure(user.mock.tell_roster_entry(&meeting.call, waiter, gone), || {
        "the lobby's end was not told".to_owned()
    })?;
    user.until("the lobby empty", |state| {
        state["call"]["lobby"].as_array().is_some_and(Vec::is_empty)
    })?;
    user.until_rows("the lobby empty", &[1, 10])?;
    Ok(())
}

/// Request to join on the first invited row, in Teams Dark and Teams Light: it shows on hover and sends one `add`.
fn invited_request(user: &mut User<'_>) -> SandboxResult {
    let meeting = user.meeting()?;
    let mut button = None;
    for theme in ["dark", "light"] {
        user.set_theme(theme)?;
        user.focus_other()?;
        user.open_people()?;
        let rows = user.until_rows(&format!("before the hover in {theme}"), &[1, 10])?;
        let avatar = *rows.avatars.get(1).ok_or("no invited row")?;
        let pane = user.pane();
        let row = Rect {
            x: pane.x + 80,
            y: avatar.y - 8,
            w: pane.w - 84,
            h: avatar.h + 16,
        };
        let calm = user.screen()?;
        user.hover((pane.x + 120, avatar.center().1))?;
        let hovered = user.screen_as_is()?;
        user.shot(&format!("invited-hover-{theme}"))?;
        // the button's label and outline: everything that came on the row's right
        let shown = pixels::changes(&calm, &hovered, row)
            .into_iter()
            .map(|blob| blob.bounds)
            .reduce(Rect::union)
            .filter(|bounds| bounds.w >= 40 && bounds.h >= 12)
            .ok_or_else(|| format!("no Request to join on the first invited row in {theme}"))?;
        say(&format!("  Request to join at {shown:?} in {theme}"));
        button = Some(shown);
    }
    let button = button.ok_or("no Request to join")?;
    let adds = user.sent("add");
    let added_before = added_to(user, &meeting.call).len();
    user.click(button.center())?;
    user.park()?;
    wait_until(WAIT, || (user.sent("add") > adds).then_some(())).ok_or("Request to join sent no add")?;
    user.until_rows("the invitee called", &[2, 9])?;
    thread::sleep(SETTLE * 3);
    user.shot("invited-requested")?;
    let sent = user.sent("add") - adds;
    let added = added_to(user, &meeting.call);
    say(&format!("  adds sent: {sent}; added: {:?}", &added[added_before..]));
    // the first invited row is the meeting chat's first member besides a (the mock lists them as added): b
    let outcome = ensure(sent == 1, || format!("{sent} adds for one Request to join")).and_then(|()| {
        ensure(
            matches!(&added[added_before..], [(who, chat)] if who == LAB_B && chat == MEETING),
            || format!("added {added:?}"),
        )
    });
    user.hang_up()?;
    user.meeting = None;
    outcome
}

/// Who the mock's call `call` had added, with the chat each `add` named.
fn added_to(user: &User<'_>, call: &str) -> Vec<(String, String)> {
    user.mock
        .calls()
        .into_iter()
        .filter(|known| known.id == call)
        .flat_map(|known| known.added)
        .collect()
}

/// The People pane's headings and rows, as Tab reaches them while a is alone: the account, then ten invited.
fn people_order() -> Vec<String> {
    let mut order = [
        "call-roster-section-in",
        "call-roster-self",
        "call-roster-section-others",
    ]
    .map(str::to_owned)
    .to_vec();
    order.extend((0..=INVITEES.len()).map(|index| format!("call-roster-added-{index}")));
    order
}

/// The heading or row of People the keyboard is on, as the child names it.
fn people_focus(user: &mut User<'_>) -> SandboxResult<Option<String>> {
    let answer = user.ask("people-focus")?;
    Ok((answer != "none").then_some(answer))
}

/// The focus once it moved off `before`, or `before` if it stays there for a while: a read right after a press may
/// come before the app has handled it.
fn people_focus_after(user: &mut User<'_>, before: Option<String>) -> SandboxResult<Option<String>> {
    let deadline = Instant::now() + STEP * 4;
    loop {
        let now = people_focus(user)?;
        if now != before {
            return Ok(now);
        }
        if Instant::now() > deadline {
            return Ok(before);
        }
        thread::sleep(POLL);
    }
}

/// The People headings and rows `key` reaches, in order, until `count` of them (a shot of each).
fn people_stops(user: &mut User<'_>, key: &str, count: usize) -> SandboxResult<Vec<String>> {
    let mut reached: Vec<String> = Vec::new();
    let mut seen = people_focus(user)?;
    for step in 1..=40 {
        user.press(key)?;
        seen = people_focus_after(user, seen)?;
        if let Some(stop) = seen.clone()
            && reached.last() != Some(&stop)
        {
            say(&format!("  {key} {step}: {stop}"));
            reached.push(stop);
            if reached.len() <= 4 {
                user.shot(&format!("keyboard-{}-{}", key.replace('+', "-"), reached.len()))?;
            }
        }
        if reached.len() == count {
            break;
        }
    }
    Ok(reached)
}

/// Tab reaches each heading and row of People in the drawn order, Shift+Tab goes back the same way; Enter on a
/// heading folds its section and Space unfolds it.
fn keyboard(user: &mut User<'_>) -> SandboxResult {
    user.open_people()?;
    user.focus_other()?;
    user.park()?;
    let order = people_order();
    let forward = people_stops(user, "tab", order.len())?;
    ensure(forward == order, || format!("Tab reached {forward:?}, not {order:?}"))?;
    let mut back = vec![people_focus(user)?.ok_or("the keyboard left People")?];
    back.extend(people_stops(user, "shift+tab", order.len() - 1)?);
    let mut reversed = order.clone();
    reversed.reverse();
    ensure(back == reversed, || {
        format!("Shift+Tab reached {back:?}, not {reversed:?}")
    })?;
    // from the first heading, Tab to "Others invited": Enter folds it, Space unfolds it
    let to_others = people_stops(user, "tab", 2)?;
    ensure(
        to_others.last().map(String::as_str) == Some("call-roster-section-others"),
        || format!("Tab from the first heading reached {to_others:?}"),
    )?;
    user.press("enter")?;
    user.until_rows("Others invited folded", &[1])?;
    user.shot("keyboard-folded")?;
    user.press("space")?;
    user.until_rows("Others invited unfolded", &[1, 10])?;
    // the other way round: Space folds it, Enter unfolds it
    user.press("space")?;
    user.until_rows("Others invited folded by Space", &[1])?;
    user.press("enter")?;
    user.until_rows("Others invited unfolded by Enter", &[1, 10])?;
    user.press("escape")?;
    Ok(())
}

/// Presses Tab until the keyboard is on the People heading or row `stop`.
fn tab_to(user: &mut User<'_>, stop: &str) -> SandboxResult {
    let mut seen = Vec::new();
    for _ in 0..40 {
        user.press("tab")?;
        let focus = people_focus(user)?;
        if focus.as_deref() == Some(stop) {
            return Ok(());
        }
        if seen.last() != Some(&focus) {
            seen.push(focus);
        }
    }
    Err(format!("Tab did not reach {stop}; it reached {seen:?}").into())
}

/// People and the stage alone in Teams Light and Teams Dark.
fn themes(user: &mut User<'_>) -> SandboxResult {
    for theme in ["light", "dark"] {
        user.set_theme(theme)?;
        user.focus_other()?;
        user.open_people()?;
        user.until_rows(&format!("People in {theme}"), &[1, 10])?;
        user.shot(&format!("theme-people-{theme}"))?;
        tab_to(user, "call-roster-section-others")?;
        thread::sleep(SETTLE);
        user.shot(&format!("theme-focus-heading-{theme}"))?;
        tab_to(user, "call-roster-added-0")?;
        thread::sleep(SETTLE);
        user.shot(&format!("theme-focus-row-{theme}"))?;
        user.press("escape")?;
        user.toggle_people(false)?;
        user.park()?;
        thread::sleep(SETTLE);
        user.shot(&format!("theme-stage-alone-{theme}"))?;
    }
    user.set_theme("light")
}

/// a calls b in their 1:1 chat; before b answers, the call names b.
fn one_to_one_ringing(user: &mut User<'_>) -> SandboxResult {
    user.hang_up()?;
    user.open_chat(AB_CHAT)?;
    user.ctl.request(
        None,
        Request::Call {
            chat: AB_CHAT.to_owned(),
        },
    )?;
    let state = user.until("the 1:1 call", |state| !state["call"].is_null())?;
    let call = state["call"].clone();
    say(&format!("  the call: {call}"));
    let title = user.call_title()?;
    say(&format!("  the call window's title: {title:?}"));
    thread::sleep(SETTLE);
    user.park()?;
    user.shot("one-to-one-ringing")?;
    let outcome = ensure(call["stand_in"] == true && call["meeting"] == false, || {
        format!("the chat's people do not stand in: {call}")
    })
    .and_then(|()| {
        ensure(title.contains("bot-b") || title.contains("Lab B"), || {
            format!("the title names not b: {title:?}")
        })
    });
    user.hang_up()?;
    outcome
}

/// a calls the a–b–c chat; while b and c ring, the call names them and People lists a, b and c.
fn group_ringing(user: &mut User<'_>) -> SandboxResult {
    user.open_chat(ABC_CHAT)?;
    user.ctl.request(
        None,
        Request::Call {
            chat: ABC_CHAT.to_owned(),
        },
    )?;
    let state = user.until("the group call", |state| !state["call"].is_null())?;
    let call = state["call"].clone();
    say(&format!("  the call: {call}"));
    let title = user.call_title()?;
    say(&format!("  the call window's title: {title:?}"));
    thread::sleep(SETTLE);
    user.park()?;
    user.shot("group-ringing")?;
    let outcome = ensure(call["stand_in"] == true, || {
        format!("the chat's people do not stand in: {call}")
    })
    .and_then(|()| {
        // a group call's title names each person by the first word of the name, as Teams does
        ensure(title == "Lab and Lab", || {
            format!("the title names not b and c: {title:?}")
        })
    });
    let listed = outcome
        .and_then(|()| user.open_people())
        .and_then(|()| user.until_rows("a, b and c while b and c ring", &[3]).map(drop));
    if listed.is_ok() {
        user.shot("group-ringing-people")?;
    }
    user.hang_up()?;
    listed
}

/// a calls the a–b–c chat; the ring times out and b joins the call: "People added to the chat" lists c, and its
/// Request to join, on hover, sends one `add`, for c. (A callee's own Request to join is refused on the mock: the
/// mock's answer to a callee carries no `add` link.)
fn group_request_to_join(user: &mut User<'_>) -> SandboxResult {
    user.hang_up()?;
    user.open_chat(ABC_CHAT)?;
    user.ctl.request(
        None,
        Request::Call {
            chat: ABC_CHAT.to_owned(),
        },
    )?;
    user.wait_call("a's group call up", |row| row.state == "connected")?;
    let group = wait_until(WAIT, || {
        user.mock
            .calls()
            .into_iter()
            .rev()
            .find(|known| known.caller == LAB_A && known.group_chat.as_deref() == Some(ABC_CHAT))
            .map(|known| known.id)
    })
    .ok_or("no group call of a's on the mock")?;
    ensure(user.mock.end_ring(&group, 0, TIMED_OUT), || {
        "the ring did not end".to_owned()
    })?;
    ensure(user.mock.join_made_up(&group, &[(LAB_B, "Lab B")]), || {
        "b is not in the group call".to_owned()
    })?;
    user.until_participants("b in the call", &[LAB_B])?;
    user.focus_other()?;
    user.open_people()?;
    let rows = user.until_rows("a, b, then c added", &[2, 1])?;
    user.shot("group-added-people")?;
    let avatar = *rows.avatars.last().ok_or("no row for c")?;
    let pane = user.pane();
    let row = Rect {
        x: pane.x + 80,
        y: avatar.y - 8,
        w: pane.w - 84,
        h: avatar.h + 16,
    };
    let calm = user.screen()?;
    user.hover((pane.x + 120, avatar.center().1))?;
    let hovered = user.screen_as_is()?;
    user.shot("group-added-hover")?;
    // the button's label and outline: everything that came on the row's right
    let button = pixels::changes(&calm, &hovered, row)
        .into_iter()
        .map(|blob| blob.bounds)
        .reduce(Rect::union)
        .filter(|bounds| bounds.w >= 40 && bounds.h >= 12)
        .ok_or("no Request to join on c's row")?;
    say(&format!("  Request to join at {button:?}"));
    let adds = user.sent("add");
    user.click(button.center())?;
    user.park()?;
    wait_until(WAIT, || (user.sent("add") > adds).then_some(())).ok_or("Request to join sent no add")?;
    thread::sleep(SETTLE * 3);
    user.shot("group-added-requested")?;
    let sent = user.sent("add") - adds;
    let added: Vec<(String, String)> = user
        .mock
        .calls()
        .into_iter()
        .filter(|known| known.id == group)
        .flat_map(|known| known.added)
        .collect();
    say(&format!("  adds sent: {sent}; added: {added:?}"));
    let outcome = ensure(sent == 1, || format!("{sent} adds for one Request to join")).and_then(|()| {
        ensure(added == [(LAB_C.to_owned(), ABC_CHAT.to_owned())], || {
            format!("added {added:?}")
        })
    });
    user.hang_up()?;
    let _ = user.mock.hang_up_call(&group);
    outcome
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

/// a's engine on the mock (its chats, teams and calling), its calls' media on file devices, relayed through the
/// mock's TURN server; read-only (its transports send nothing) when `read_only`.
fn engine(feed: VideoFeed, dir: &Path, read_only: bool) -> SandboxResult<EngineHandle> {
    let tokens = Arc::new(Tokens {
        ic3: env(IC3_ENV)?,
        skypetoken: env(SKYPE_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let tracker = Arc::new(teams_call::Tracker::new(Mri::new(LAB_A)));
    let origin = env(ORIGIN_ENV)?;
    let trouter_origin = env(TROUTER_ENV)?;
    let http = || -> SandboxResult<HttpTransport<Tokens>> {
        let transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin)?;
        Ok(if read_only { transport.read_only() } else { transport })
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
    let calls = if read_only {
        None
    } else {
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
        Some(CallParts::new(calling, identity, media))
    };
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
        known_names: [(LAB_B, "Lab B"), (LAB_C, "Lab C")]
            .into_iter()
            .chain(INVITEES)
            .map(|(mri, name)| (mri.to_owned(), name.to_owned()))
            .collect(),
    })?)
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let read_only = false;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let feed = VideoFeed::new();
    let engine = engine(feed.clone(), &dir, read_only)?;
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
        let calls = (!read_only).then(|| PersonalCalls {
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
                read_only,
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
            let reply = cx.update(|cx| {
                if verb == "people-focus" {
                    people_focus_in(cx)
                } else {
                    on_app(window, verb, cx)
                }
            });
            say(&format!("re {seq} {}", reply.replace('\n', " ")));
        }
    })
    .detach();
}

/// The People pane's heading or row the keyboard is on in the call window (`none` when on none).
fn people_focus_in(cx: &mut App) -> String {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<KitRoot>())
        .find_map(|window| {
            window
                .update(cx, |kit, window, cx| {
                    let stage = kit.view().clone().downcast::<CallStage>().ok()?;
                    stage.read(cx).people_focus(window)
                })
                .ok()
                .flatten()
        })
        .unwrap_or_else(|| "none".to_owned())
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
            Some(on_shell(&shell, verb, cx))
        })
        .ok()
        .flatten()
        .unwrap_or_else(|| "no shell".to_owned())
}

fn on_shell(shell: &Entity<Shell>, verb: &str, cx: &mut App) -> String {
    match verb {
        "state" => state_of(shell, cx).to_string(),
        "theme-dark" | "theme-light" => {
            let key = if verb == "theme-dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
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
            "ok".to_owned()
        }
        other => format!("unknown {other}"),
    }
}

/// What the window shows of the call: the main view, the pre-join, and the call (its kind, whether it is a meeting's,
/// whether its chat's people stand in for its roster, who is in it, who waits in its lobby, who was added).
fn state_of(shell: &Entity<Shell>, cx: &App) -> Value {
    let shell_ref = shell.read(cx);
    let main = match shell_ref.main() {
        Main::Chat(id) => format!("chat:{id}"),
        Main::Channel(id) => format!("channel:{id}"),
        other => format!("{other:?}"),
    };
    let calls = shell_ref.models().calls.read(cx);
    let prejoin = calls.prejoin().map(|prejoin| {
        json!({"chat": prejoin.to_join.chat(), "title": prejoin.to_join.title(), "ring": prejoin.ring.is_some()})
    });
    let call = calls.call().map(|call| {
        let participants: Vec<Value> = call
            .view
            .participants
            .iter()
            .map(|participant| json!({"mri": participant.mri, "name": participant.name, "video": participant.video_on}))
            .collect();
        let lobby: Vec<&str> = call.lobby.iter().map(|person| person.mri.as_str()).collect();
        let invited: Vec<&str> = call.invited.iter().map(|invited| invited.mri.as_str()).collect();
        let asked: Vec<Value> = call
            .invited
            .iter()
            .map(|invited| {
                json!({"mri": invited.mri.as_str(), "no_response": invited.no_response, "declined": invited.declined.is_some()})
            })
            .collect();
        let passed: Vec<&str> = call.passed.iter().map(Mri::as_str).collect();
        json!({
            "title": call.title,
            "status": teams_ui::calls::status_name(&call.status),
            "chat": call.chat,
            "kind": format!("{:?}", call.kind),
            "meeting": call.is_meeting(),
            "stand_in": call.people_stand_in(),
            "participants": participants,
            "lobby": lobby,
            "invited": invited,
            "asked": asked,
            "passed": passed,
        })
    });
    let ringing = calls
        .ringing()
        .map(|ringing| json!({"caller": ringing.caller.as_str(), "chat": ringing.group_chat}));
    json!({
        "main": main,
        "ringing": ringing,
        "prejoin": prejoin,
        "call": call,
    })
}
