//! QA of task 420 (round 1), in the UI sandbox: a channel meeting as a user meets it, for a Microsoft account on the
//! mock. The real app runs as a child in the sandbox, as lab account a on `teams-mock` (its chat service, its calling
//! service, its Trouter and its group video). The parent drives it with the sandbox's virtual pointer and keyboard and
//! asks the child what the window shows through a file in the sandbox dir. A button is clicked where a user sees it.
//!
//! b's meeting runs on the mock's calling in the team's standard channel ("A made-up channel"), with nobody rung. Its
//! post is put in the channel as the service puts it (`Event/Call` `callStarted`, its `properties.meeting` with the
//! organizer, the tenant and the title), and the service's end turns the post itself into `callEnded`.
//!
//! - **A titled meeting** (an odd title of 200 characters: emoji, quotes, «», `&`, `<b>`): the post's card shows the
//!   title, "Started <time>" and Join (shots at 1280 and 480 px, Light and Dark). The post's Join opens the pre-join
//!   with the title, for the post; Escape. The header's Join opens the same pre-join. Join now: the call has the title.
//!   In the call, b posts in the channel: the call window shows the chat bubble at its stage's top, gone after about 4 s
//!   (QA of task 666).
//! - **The end**: the post becomes "<title> ended" with no Join, the header has no Join.
//! - **An untitled meeting**: the card, the pre-join and the call say `Meeting in "A made-up channel"`.
//! - **The read-only mode**: a second app, read-only, shows the running meeting's card with no Join.
//!
//! The mock's calling has no lobby: the lobby's toolbar is checked on the own server (`own_meeting_window`).
//!
//! `QA_CM_EVIDENCE` names a directory that gets the screenshots and the results too. Screenshots also go to
//! `target/ui-shots/qa420-*.png`. Every name, id and text is made up.
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
    AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, ECHO, EventKind, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE,
    PRESENCE_BASE, PROVISIONING_BASE, TEAM_CHANNEL, TENANT, TIMED_OUT, USER_PROFILE_BASE,
};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use teams_ui::calls::MeetingToJoin;
use teams_ui::chat_view::Body;
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

const CHILD_ENV: &str = "QA_CM_CHILD";
const EVIDENCE_ENV: &str = "QA_CM_EVIDENCE";
const TRACE_FD_ENV: &str = "QA_CM_TRACE_FD";
const ORIGIN_ENV: &str = "QA_CM_ORIGIN";
const TROUTER_ENV: &str = "QA_CM_TROUTER";
const RELAY_ENV: &str = "QA_CM_RELAY";
const IC3_ENV: &str = "QA_CM_IC3";
const SKYPE_ENV: &str = "QA_CM_SKYPE";
const DIR_ENV: &str = "QA_CM_DIR";
const CTL_ENV: &str = "QA_CM_CTL";
/// The child's mode: `normal` or `read-only`.
const MODE_ENV: &str = "QA_CM_MODE";
/// The file in the sandbox dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "qa420-ask";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(100);
const SETTLE: Duration = Duration::from_millis(900);
const STEP: Duration = Duration::from_millis(250);
/// niri's layout: each window takes the whole screen until the test sizes it, and no hot corner.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
const SCREEN: (u32, u32) = (1280, 800);
const PEER_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.2,
};
/// The untitled meeting's title, as Teams names one.
const UNTITLED: &str = "Meeting in \"A made-up channel\"";

/// The odd title: emoji, quotes, «», `&`, markup, padded to 200 characters.
fn odd_title() -> String {
    let start = "Sprint ✨ \"review\" «Q4» & <b>demo</b> — 🚀 'launch' ";
    let mut title = start.to_owned();
    let mut n = 0;
    while title.chars().count() < 200 {
        n += 1;
        let _ = write!(title, "part{n} ");
    }
    title.chars().take(199).chain(std::iter::once('!')).collect()
}

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_channel_meeting_window: {error}");
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

// --- the parent ---

type Check = (&'static str, fn(&mut User<'_>) -> SandboxResult);

const CHECKS: [Check; 4] = [
    ("titled_post_card_join_and_prejoin", titled_meeting),
    ("ended_post_has_no_join", ended_meeting),
    ("untitled_meeting_named_after_the_channel", untitled_meeting),
    ("narrow_card_wraps_join", narrow_card),
];

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_channel_meeting_window", &options)? else {
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
    let mut meetings = Meetings::default();
    {
        let app = AppUnderTest::start(&sandbox, &mock, &relay, trouter.origin(), &dir.path().join("a"), false)?;
        let mut user = User::new(&sandbox, &mock, app, &mut meetings)?;
        for (name, check) in CHECKS {
            user.run_check(name, check, &mut passed, &mut failed);
        }
        user.quit()?;
    }
    {
        let app = AppUnderTest::start(&sandbox, &mock, &relay, trouter.origin(), &dir.path().join("ro"), true)?;
        let mut user = User::new(&sandbox, &mock, app, &mut meetings)?;
        user.run_check("read_only_card_has_no_join", read_only, &mut passed, &mut failed);
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
        "qa_channel_meeting_window: {} passed, {} failed",
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
    _trace: TraceReader,
    socket: PathBuf,
}

impl AppUnderTest {
    fn start(
        sandbox: &Sandbox,
        mock: &ChatServiceMock,
        relay: &str,
        trouter: &str,
        own: &Path,
        read_only: bool,
    ) -> SandboxResult<Self> {
        fs::create_dir_all(own)?;
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let ic3 = mock.token(LAB_A).ok_or("the mock's token")?;
        let skype = mock.skypetoken(LAB_A).ok_or("the mock's skypetoken")?;
        let mode = if read_only { "read-only" } else { "normal" };
        let socket = sandbox.path(&format!("run/ctl-{mode}.sock"));
        let own_text = own.to_string_lossy().into_owned();
        let socket_text = socket.to_string_lossy().into_owned();
        let child = sandbox.spawn_with_fds(
            &std::env::current_exe()?,
            &[],
            &[
                (CHILD_ENV, "a"),
                (MODE_ENV, mode),
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
        if !read_only {
            trace.wait_for("calling", "registered")?;
        }
        Ok(Self {
            running: Some(running),
            _trace: trace,
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

/// A meeting on the mock: its call, the conversation it runs at, and its post in the channel.
#[derive(Clone, Debug)]
struct Meeting {
    call: String,
    conversation: String,
    post: String,
    title: String,
}

/// The meetings the checks started, for the checks after them.
#[derive(Default)]
struct Meetings {
    titled: Option<Meeting>,
    running: Option<Meeting>,
}

/// The user at the sandbox: the pointer, the keyboard, the screenshots, the control socket, and the child it asks
/// about the window.
struct User<'a> {
    sandbox: &'a Sandbox,
    mock: &'a ChatServiceMock,
    app: AppUnderTest,
    meetings: &'a mut Meetings,
    input: VirtualInput,
    ctl: Client,
    asked: u32,
    main_window: Option<u64>,
}

impl<'a> User<'a> {
    fn new(
        sandbox: &'a Sandbox,
        mock: &'a ChatServiceMock,
        app: AppUnderTest,
        meetings: &'a mut Meetings,
    ) -> SandboxResult<Self> {
        let socket = app.socket.clone();
        let ctl = wait_until(WAIT, || Client::connect(&socket, Timeouts::default()).ok())
            .ok_or_else(|| format!("no control socket at {}", socket.display()))?;
        let mut user = Self {
            sandbox,
            mock,
            app,
            meetings,
            input: sandbox.input()?,
            ctl,
            asked: 0,
            main_window: None,
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
                let _ = self.shot(&format!("failed-{name}"), false);
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
        let (x, y) = (u32::try_from(x)?, u32::try_from(y)?);
        self.input.move_to(x, y)?;
        thread::sleep(Duration::from_millis(80));
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn park(&mut self) -> SandboxResult {
        // the window's left margin: no button, tooltip or tile there
        self.input.move_to(3, SCREEN.1 / 2)
    }

    /// A screenshot as `qa420-<name>.png` (the focused window alone, or the screen), under `target/ui-shots/` and the
    /// evidence dir.
    fn shot(&mut self, name: &str, window: bool) -> SandboxResult<PathBuf> {
        let file = format!("qa420-{name}.png");
        let png = self.sandbox.screenshot(&file, window)?;
        let kept = shots_dir()?.join(&file);
        fs::copy(&png, &kept)?;
        if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
            let dir = PathBuf::from(dir).join("shots");
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(&file))?;
        }
        say(&format!("  shot qa420-{name}.png"));
        Ok(kept)
    }

    /// The focused window in Teams Light and Teams Dark, at `widths` (the window's own width when empty); Teams Light
    /// and the full width after.
    fn shots(&mut self, name: &str, widths: &[i32]) -> SandboxResult {
        for (verb, theme) in [("theme-light", "light"), ("theme-dark", "dark")] {
            self.ask(verb)?;
            if widths.is_empty() {
                self.park()?;
                thread::sleep(SETTLE);
                self.shot(&format!("{name}-{theme}"), true)?;
            }
            for width in widths {
                self.resize_focused(*width)?;
                self.park()?;
                thread::sleep(SETTLE);
                self.shot(&format!("{name}-{theme}-{width}"), true)?;
            }
        }
        self.ask("theme-light")?;
        if !widths.is_empty() {
            self.resize_focused(i32::try_from(SCREEN.0)?)?;
        }
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

    #[expect(
        clippy::cast_possible_truncation,
        reason = "niri's logical px are whole numbers on a scale of 1"
    )]
    fn window_size(&self, id: u64) -> SandboxResult<(i32, i32)> {
        let windows = self.windows()?;
        let window = windows
            .iter()
            .find(|window| window["id"].as_u64() == Some(id))
            .ok_or_else(|| format!("no window {id}"))?;
        let size = &window["layout"]["window_size"];
        let at = |index: usize| size[index].as_f64().map(|value| value.round() as i32);
        Ok((at(0).ok_or("no width")?, at(1).ok_or("no height")?))
    }

    fn focus_main(&mut self) -> SandboxResult {
        let id = self.main_window.ok_or("no main window")?;
        self.sandbox
            .niri_msg(&["action", "focus-window", "--id", &id.to_string()])?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The compositor sizes the focused window to `width` wide, as a user's drag does (no narrower than the window's
    /// own minimum).
    fn resize_focused(&mut self, width: i32) -> SandboxResult {
        let id = self.focused_window()?.ok_or("no window has the focus")?;
        self.sandbox
            .niri_msg(&["action", "set-column-width", &width.to_string()])?;
        let _ = wait_until(Duration::from_secs(5), || {
            self.window_size(id).ok().filter(|size| size.0 == width).map(|_| ())
        });
        thread::sleep(SETTLE);
        Ok(())
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

    /// The screen as an image (the main window fills it at 1280 px).
    fn screen(&mut self) -> SandboxResult<RgbImage> {
        self.park()?;
        thread::sleep(SETTLE);
        let png = self.sandbox.screenshot("qa420-find.png", false)?;
        let image = image::open(&png)?.to_rgb8();
        let _ = fs::remove_file(png);
        Ok(image)
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
                .filter(|calls| calls.iter().all(|row| row.state != "connected"))
                .map(|_| ())
        })
        .ok_or("the call never ended")?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn open_channel(&mut self) -> SandboxResult {
        self.focus_main()?;
        self.ctl.request(
            None,
            Request::Open {
                target: OpenTarget::Channel {
                    channel: TEAM_CHANNEL.to_owned(),
                },
            },
        )?;
        self.until("the channel open", |state| {
            state["main"] == format!("channel:{TEAM_CHANNEL}")
        })?;
        Ok(())
    }

    // --- the mock's meetings ---

    /// b's meeting in the channel on the mock's calling (nobody rung), and its post as the service puts it: titled
    /// `title`, or untitled.
    fn start_meeting(&mut self, title: Option<&str>) -> SandboxResult<Meeting> {
        let call = self.mock.ring_group(LAB_B, TEAM_CHANNEL, &[]);
        ensure(self.mock.end_ring(&call, 0, TIMED_OUT), || {
            "no meeting on the mock".to_owned()
        })?;
        let conversation = self
            .mock
            .calls()
            .into_iter()
            .find(|known| known.id == call)
            .map(|known| known.conversation)
            .ok_or("no call on the mock")?;
        let content = format!(
            "<partlist type=\"started\" alt=\"\"><part identity=\"{LAB_B}\"><name>{LAB_B}</name></part></partlist>\
             <callEventType>callStarted</callEventType><conversationUrl>{}</conversationUrl><callId>{call}</callId>",
            link(&conversation)
        );
        let mut meeting = json!({
            "organizerId": LAB_B.trim_start_matches("8:orgid:"),
            "tenantId": TENANT,
        });
        if let Some(title) = title {
            meeting["meetingtitle"] = json!(title);
        }
        let meeting = meeting.to_string();
        let post = self.post_changed(None, &content, |message| {
            message["properties"]["meeting"] = json!(meeting);
        })?;
        Ok(Meeting {
            call,
            conversation,
            post,
            title: title.map_or_else(|| UNTITLED.to_owned(), str::to_owned),
        })
    }

    /// The meeting ends on the mock, and the service turns its post into the end.
    fn end_meeting(&mut self, meeting: &Meeting) -> SandboxResult {
        let _ = self.mock.hang_up_call(&meeting.call);
        let content = format!(
            "<ended/><partlist alt=\"\" count=\"0\"></partlist><callEventType>callEnded</callEventType>\
             <conversationUrl>{}</conversationUrl><callId>{}</callId>",
            link(&meeting.conversation),
            meeting.call
        );
        self.post_changed(Some(&meeting.post), &content, |_| {})?;
        Ok(())
    }

    /// Puts a new post (`post` none) or changes the post `post` to `content` and `change`, and tells a of it as an
    /// edit of the post: the mock's own post goes out with no properties.
    fn post_changed(
        &mut self,
        post: Option<&str>,
        content: &str,
        change: impl Fn(&mut Value),
    ) -> SandboxResult<String> {
        let id = match post {
            Some(id) => id.to_owned(),
            None => self
                .mock
                .add_message(TEAM_CHANNEL, (LAB_B, "Lab B"), "Event/Call", content)
                .ok_or("no channel on the mock")?,
        };
        let client_id = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
        let mut changed = Value::Null;
        let found = self.mock.change_message(TEAM_CHANNEL, &client_id, |message| {
            message["content"] = json!(content);
            change(message);
            let version = message["version"]
                .as_str()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
                + 1;
            message["version"] = json!(version.to_string());
            changed = message.clone();
        });
        ensure(found, || format!("no post {id} on the mock"))?;
        let mut kept = Vec::new();
        for event in self.mock.take_events(LAB_A) {
            if event.resource["id"] != id.as_str() {
                kept.push(event);
            }
        }
        for event in kept {
            self.mock.queue_event(LAB_A, event);
        }
        self.mock.queue_event(
            LAB_A,
            teams_mock::ChatEvent {
                conversation: TEAM_CHANNEL.to_owned(),
                from: LAB_B.to_owned(),
                kind: EventKind::Message,
                resource: changed,
            },
        );
        Ok(id)
    }
}

/// Where the mock's call at `conversation` runs, as a call's post names it.
fn link(conversation: &str) -> String {
    format!(
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/{conversation}?i=10-0-0-2&amp;e=1"
    )
}

// --- finding a button on the screen ---

/// A pixel of the brand fill (a button's or a meeting tile's).
fn brand(pixel: image::Rgb<u8>) -> bool {
    let [r, g, b] = pixel.0.map(i32::from);
    b >= 150 && b - r >= 60 && b - g >= 60
}

/// The middle of the first brand-filled button within `columns` and `rows` of `image` ([`brand_box`]).
fn brand_button(image: &RgbImage, columns: (u32, u32), rows: (u32, u32), wide: usize) -> Option<(i32, i32)> {
    let (first, last, top, bottom) = brand_box(image, columns, rows, wide)?;
    Some((
        i32::try_from(first.midpoint(last)).ok()?,
        i32::try_from(top.midpoint(bottom)).ok()?,
    ))
}

/// The box (left, right, top, bottom) of the first brand-filled button within `columns` and `rows` of `image`: rows
/// whose rightmost run of brand pixels (gaps of 12 px or less, for the button's icon and label) is at least `wide` px
/// wide and at least half brand, at least 16 such rows together. A smaller brand tile beside the button on the same
/// rows is not part of the run.
fn brand_box(
    image: &RgbImage,
    (left, right): (u32, u32),
    (top, bottom): (u32, u32),
    wide: usize,
) -> Option<(u32, u32, u32, u32)> {
    let mut run: Vec<(u32, u32, u32)> = Vec::new();
    for y in top..image.height().min(bottom) {
        let xs: Vec<u32> = (left..right.min(image.width()))
            .filter(|x| brand(*image.get_pixel(*x, y)))
            .collect();
        let mut segments: Vec<(u32, u32, usize)> = Vec::new();
        for x in xs {
            match segments.last_mut() {
                Some((_, end, count)) if x - *end <= 12 => {
                    *end = x;
                    *count += 1;
                }
                _ => segments.push((x, x, 1)),
            }
        }
        let wide_enough = |(first, last, count): &&(u32, u32, usize)| {
            usize::try_from(*last - *first + 1).is_ok_and(|span| span >= wide) && *count * 2 >= wide
        };
        if let Some((first, last, _)) = segments.iter().rev().find(wide_enough) {
            run.push((y, *first, *last));
        } else if run.len() >= 16 {
            break;
        } else {
            run.clear();
        }
    }
    if run.len() < 16 {
        return None;
    }
    let first = run.iter().map(|row| row.1).min()?;
    let last = run.iter().map(|row| row.2).max()?;
    Some((first, last, run[0].0, run[run.len() - 1].0))
}

/// The channel header's Join: the brand-filled button at the right of the header.
fn header_join(image: &RgbImage) -> Option<(i32, i32)> {
    brand_button(image, (image.width().saturating_sub(260), image.width()), (40, 104), 30)
}

/// The post's Join: a brand-filled button wider than the card's tile, in the posts (below the header, above the box).
fn post_join(image: &RgbImage) -> Option<(i32, i32)> {
    brand_button(
        image,
        (380, image.width()),
        (110, image.height().saturating_sub(70)),
        44,
    )
}

// --- the checks ---

fn post_of<'v>(state: &'v Value, post: &str) -> Option<&'v Value> {
    state["posts"].as_array()?.iter().find(|item| item["id"] == post)
}

/// The odd-titled meeting: its card, its Join, the header's Join, and the call.
fn titled_meeting(user: &mut User<'_>) -> SandboxResult {
    let title = odd_title();
    say(&format!("  the title ({} characters): {title}", title.chars().count()));
    user.open_channel()?;
    let meeting = user.start_meeting(Some(&title))?;
    user.meetings.titled = Some(meeting.clone());
    let state = user.until("the post's card", |state| {
        post_of(state, &meeting.post).is_some_and(|item| item["kind"] == "meeting-call")
    })?;
    let card = post_of(&state, &meeting.post).cloned().unwrap_or_default();
    say(&format!("  the card: {card}"));
    ensure(card["title"] == title.as_str(), || format!("the card's title: {card}"))?;
    user.shots("titled-post", &[1280, 480])?;
    // the post's Join, as a user clicks it
    let screen = user.screen()?;
    let join = post_join(&screen).ok_or("no Join on the post")?;
    say(&format!("  the post's Join at {join:?}"));
    user.click(join)?;
    let state = user.until("the pre-join from the post", |state| !state["prejoin"].is_null())?;
    let prejoin = state["prejoin"].clone();
    say(&format!("  the pre-join from the post: {prejoin}"));
    user.wait_other_window()?;
    user.shots("titled-prejoin-from-post", &[])?;
    ensure(prejoin["title"] == title.as_str(), || {
        format!("the pre-join's title: {prejoin}")
    })?;
    ensure(
        prejoin["chat"] == TEAM_CHANNEL && prejoin["post"] == meeting.post.as_str(),
        || format!("the pre-join is for another meeting: {prejoin}"),
    )?;
    user.press("escape")?;
    user.until("the pre-join closed", |state| state["prejoin"].is_null())?;
    // the header's Join opens the same pre-join
    user.focus_main()?;
    let screen = user.screen()?;
    let join = header_join(&screen).ok_or("no Join in the channel's header")?;
    say(&format!("  the header's Join at {join:?}"));
    user.click(join)?;
    let state = user.until("the pre-join from the header", |state| !state["prejoin"].is_null())?;
    let from_header = state["prejoin"].clone();
    say(&format!("  the pre-join from the header: {from_header}"));
    user.wait_other_window()?;
    ensure(from_header == prejoin, || {
        format!("the header's pre-join {from_header} is not the post's {prejoin}")
    })?;
    // Join now: the call has the title
    user.press("enter")?;
    user.wait_call("connected in the meeting", |row| row.state == "connected")?;
    let state = user.until("the call's title", |state| !state["call"].is_null())?;
    let call = state["call"].clone();
    say(&format!("  the call: {call}"));
    let joined = user
        .mock
        .calls()
        .iter()
        .filter(|known| known.conversation == meeting.conversation)
        .flat_map(|known| known.requests.iter())
        .any(|(op, by)| op == "join" && by == LAB_A);
    user.wait_other_window()?;
    user.shots("titled-call", &[])?;
    channel_bubble(user)?;
    ensure(joined, || {
        "a's join did not reach the post's meeting on the mock".to_owned()
    })?;
    ensure(call["title"] == title.as_str(), || format!("the call's title: {call}"))?;
    user.hang_up()
}

/// QA of task 666: b posts in the channel while a is in its meeting: the call window shows the chat bubble at the
/// stage's top (about 320×70) and puts it away after about 4 s.
fn channel_bubble(user: &mut User<'_>) -> SandboxResult {
    let calm = user.screen()?;
    user.mock
        .post_as(LAB_B, TEAM_CHANNEL, "<p>The agenda is in the channel</p>")
        .ok_or("b's post to the channel was refused")?;
    thread::sleep(Duration::from_millis(500));
    let shot = user.screen()?;
    let bubble = changed_band(&calm, &shot);
    user.shot("qa666-channel-meeting-bubble", false)?;
    say(&format!(
        "  QA 666: b's channel post in the meeting: the call window's top changed at {bubble:?}"
    ));
    let (x, y, w, h) = bubble.ok_or("b's post in the channel showed no chat bubble in the meeting's call window")?;
    ensure((290..=350).contains(&w) && (55..=95).contains(&h), || {
        format!("the change at the call window's top is {w}×{h} at ({x}, {y}): not a chat bubble")
    })?;
    thread::sleep(Duration::from_secs(5));
    let later = user.screen()?;
    ensure(changed_band(&calm, &later).is_none(), || {
        "the chat bubble stayed after 5 s".to_owned()
    })
}

/// Where `after` differs from `before` under the call window's toolbar (rows 56 to 200, where the chat bubble
/// shows): the bounds `(x, y, w, h)` of the changed px, if any.
fn changed_band(before: &RgbImage, after: &RgbImage) -> Option<(u32, u32, u32, u32)> {
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for y in 56..200.min(before.height()) {
        for x in 0..before.width() {
            let (a, b) = (before.get_pixel(x, y).0, after.get_pixel(x, y).0);
            let distance: u32 = a.iter().zip(b).map(|(one, two)| u32::from(one.abs_diff(two))).sum();
            if distance > 48 {
                bounds = Some(bounds.map_or((x, y, x, y), |(x0, y0, x1, y1)| {
                    (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
                }));
            }
        }
    }
    bounds.map(|(x0, y0, x1, y1)| (x0, y0, x1 - x0 + 1, y1 - y0 + 1))
}

/// The titled meeting ends: its post is a line "<title> ended" with no Join; the header has no Join.
fn ended_meeting(user: &mut User<'_>) -> SandboxResult {
    let meeting = user.meetings.titled.clone().ok_or("no titled meeting")?;
    user.open_channel()?;
    user.end_meeting(&meeting)?;
    let state = user.until("the post's end", |state| {
        post_of(state, &meeting.post).is_some_and(|item| item["kind"] != "meeting-call")
    })?;
    let line = post_of(&state, &meeting.post).cloned().unwrap_or_default();
    say(&format!("  the post: {line}"));
    thread::sleep(SETTLE * 2);
    user.shots("titled-post-ended", &[1280, 480])?;
    let screen = user.screen()?;
    let post = post_join(&screen);
    let header = header_join(&screen);
    // the line's words as the child's debug text quotes them
    let ended = format!("{:?}", format!("{} ended", meeting.title));
    ensure(line["text"].as_str().is_some_and(|text| text.contains(&ended)), || {
        format!("the post's line: {line}")
    })?;
    ensure(post.is_none(), || format!("a Join on the screen's posts at {post:?}"))?;
    ensure(header.is_none(), || format!("a Join in the header at {header:?}"))
}

/// An untitled meeting: the card, the pre-join and the call say `Meeting in "<channel>"`.
fn untitled_meeting(user: &mut User<'_>) -> SandboxResult {
    user.open_channel()?;
    let meeting = user.start_meeting(None)?;
    user.meetings.running = Some(meeting.clone());
    let state = user.until("the untitled post's card", |state| {
        post_of(state, &meeting.post).is_some_and(|item| item["kind"] == "meeting-call")
    })?;
    let card = post_of(&state, &meeting.post).cloned().unwrap_or_default();
    say(&format!("  the card: {card}"));
    user.shots("untitled-post", &[1280])?;
    ensure(card["title"] == UNTITLED, || format!("the card's title: {card}"))?;
    let screen = user.screen()?;
    let join = header_join(&screen).ok_or("no Join in the channel's header")?;
    user.click(join)?;
    let state = user.until("the pre-join", |state| !state["prejoin"].is_null())?;
    let prejoin = state["prejoin"].clone();
    say(&format!("  the pre-join: {prejoin}"));
    user.wait_other_window()?;
    user.shots("untitled-prejoin", &[])?;
    ensure(prejoin["title"] == UNTITLED, || {
        format!("the pre-join's title: {prejoin}")
    })?;
    user.press("enter")?;
    user.wait_call("connected in the meeting", |row| row.state == "connected")?;
    let state = user.until("the call", |state| !state["call"].is_null())?;
    let call = state["call"].clone();
    say(&format!("  the call: {call}"));
    user.wait_other_window()?;
    user.shots("untitled-call", &[])?;
    ensure(call["title"] == UNTITLED, || format!("the call's title: {call}"))?;
    user.hang_up()
}

/// The width that keeps 20 characters or more of a meeting's title readable, px.
const READABLE_TITLE: u32 = 160;
/// The gap between the card's tile, its text and its Join, px.
const CARD_GAP: u32 = 12;

/// The running meeting's card in a narrow window: at 640 and 560 px, Join is beside the title only when the title
/// keeps [`READABLE_TITLE`], else Join is below the tile and the title has the whole line (shots at 640, 560 and 480 px).
fn narrow_card(user: &mut User<'_>) -> SandboxResult {
    let meeting = user.meetings.running.clone().ok_or("no running meeting")?;
    user.open_channel()?;
    user.until("the card", |state| {
        post_of(state, &meeting.post).is_some_and(|item| item["kind"] == "meeting-call")
    })?;
    for width in [640, 560] {
        user.resize_focused(width)?;
        user.park()?;
        thread::sleep(SETTLE);
        let screen = user.screen()?;
        // the posts start right of the chat list
        let (first, last, top, bottom) = brand_box(
            &screen,
            (300, screen.width()),
            (110, screen.height().saturating_sub(70)),
            44,
        )
        .ok_or_else(|| format!("no Join on the post at {width} px"))?;
        let middle = top.midpoint(bottom);
        let tile_beside = (300..first).rev().find(|x| brand(*screen.get_pixel(*x, middle)));
        say(&format!(
            "  {width} px: Join at x {first}..{last}, y {top}..{bottom}; the tile's right on its row: {tile_beside:?}"
        ));
        if let Some(tile) = tile_beside {
            let text = first.saturating_sub(tile + 1 + 2 * CARD_GAP);
            ensure(text >= READABLE_TITLE, || {
                format!("at {width} px Join is beside a title of {text} px, less than 20 characters")
            })?;
        } else {
            let tile_above = (top.saturating_sub(64)..top).any(|y| brand(*screen.get_pixel(first + 16, y)));
            ensure(tile_above, || {
                format!("at {width} px Join is not beside the tile and the tile is not above it")
            })?;
        }
    }
    user.resize_focused(i32::try_from(SCREEN.0)?)?;
    user.shots("untitled-post-narrow", &[640, 560, 480])
}

/// The read-only app: the running meeting's card shows no Join.
fn read_only(user: &mut User<'_>) -> SandboxResult {
    let meeting = user.meetings.running.clone().ok_or("no running meeting")?;
    user.open_channel()?;
    let state = user.until("the card", |state| {
        post_of(state, &meeting.post).is_some_and(|item| item["kind"] == "meeting-call")
    })?;
    say(&format!(
        "  the card: {}",
        post_of(&state, &meeting.post).cloned().unwrap_or_default()
    ));
    thread::sleep(SETTLE * 2);
    user.shots("read-only-post", &[1280, 480])?;
    let screen = user.screen()?;
    let post = post_join(&screen);
    let header = header_join(&screen);
    say(&format!("  the header's Join in the read-only mode: {header:?}"));
    let ended = user.end_meeting(&meeting);
    ensure(post.is_none(), || format!("a Join on the read-only card at {post:?}"))?;
    ended
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
        known_names: vec![
            (LAB_B.to_owned(), "Lab B".to_owned()),
            (LAB_C.to_owned(), "Lab C".to_owned()),
        ],
    })?)
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let read_only = env(MODE_ENV)? == "read-only";
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

/// What the window shows of the channel's meetings: the main view, the channel's posts (their kind, a card's title, a
/// line's words), the pre-join and the call.
fn state_of(shell: &Entity<Shell>, cx: &App) -> Value {
    let shell_ref = shell.read(cx);
    let main = match shell_ref.main() {
        Main::Chat(id) => format!("chat:{id}"),
        Main::Channel(id) => format!("channel:{id}"),
        other => format!("{other:?}"),
    };
    let posts: Vec<Value> = shell_ref
        .channel()
        .filter(|_| matches!(shell_ref.main(), Main::Channel(_)))
        .map(|channel| {
            channel
                .read(cx)
                .posts()
                .read(cx)
                .message_items()
                .filter(|item| item.message_type == "Event/Call")
                .map(|item| match &item.body {
                    Body::MeetingCall(call) => json!({
                        "id": item.id,
                        "kind": item.body.kind(),
                        "title": call.title.to_string(),
                        "organizer": call.meeting.organizer_id,
                    }),
                    other => json!({
                        "id": item.id,
                        "kind": other.kind(),
                        "text": format!("{other:?}"),
                    }),
                })
                .collect()
        })
        .unwrap_or_default();
    let calls = shell_ref.models().calls.read(cx);
    let prejoin = calls.prejoin().map(|prejoin| match &prejoin.to_join {
        MeetingToJoin::Chat { chat, title, post, .. } => json!({"chat": chat, "title": title, "post": post}),
        MeetingToJoin::Code(_) => json!({"code": true}),
    });
    let call = calls.call().map(|call| {
        json!({
            "title": call.title,
            "status": teams_ui::calls::status_name(&call.status),
            "chat": call.chat,
        })
    });
    json!({
        "main": main,
        "posts": posts,
        "prejoin": prejoin,
        "call": call,
    })
}
