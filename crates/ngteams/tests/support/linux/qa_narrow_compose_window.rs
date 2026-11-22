//! QA of task 429 (round 1), in the UI sandbox: the message box stays usable in a narrow call chat. The real app runs
//! as a child in the sandbox, as lab account a on `teams-mock` (its chat service, its calling service, its Trouter and
//! its group video). The parent drives it with the sandbox's virtual pointer and keyboard and asks the child what the
//! boxes hold through a file in the sandbox dir (`harness = false`: GPUI runs on the process's main thread). The app
//! tells no positions: a button is clicked where a user sees it on the screen.
//!
//! - **A group call** (b rings a in the a–b–c chat, a answers) and **a meeting** (a joins a made-up meeting from its
//!   meeting chat): the Chat pane open, its box is stacked, More options holds the tools that do not fit, Send stays
//!   on the row. Type, paste, send, a long message, its edit.
//! - **More options**: each row does what its button does (Correct and Translate, which the user shows on the toolbar,
//!   with a made-up `claude` on the app's `PATH`, the language row), the keys work with the buttons in the menu, the check and "Stop …" show, Up, Down,
//!   Enter and Escape, the keyboard back in the text.
//! - **A narrow window**: the main window resized from wide to narrow and back with a draft: the box changes both ways
//!   and keeps the draft and the focus. A thread's pane and a channel's box. At a normal width nothing changes.
//! - **Odd cases**: the largest font (more tools in the menu), a reply over the box, an attachment chip, Teams Dark,
//!   the keyboard only.
//! - **The box's edge** (QA of task 581): no glow or shadow around a box, its border grey, the brand line just inside
//!   its bottom border only while it has the focus, in the chat (not focused, typed, more lines, the format bar, a
//!   reply, a file, an edit, other font sizes), a channel post and its thread (the line moves with the focus), New
//!   message, the call's Chat pane and a narrow window, in Teams Light and Dark. The tour's pixel check reads each
//!   shot.
//!
//! `QA_NARROW_ONLY` (a comma list of check names) runs only those. `QA_NARROW_EVIDENCE` names a directory that gets the
//! screenshots and the results too. Screenshots also go to `target/ui-shots/qa429-*.png`. Every name, id and text is
//! made up.
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
use teams_ctl::protocol::{CallRow, Request, Response};
use teams_ctl::{Client, Timeouts};
use teams_media::devices::Devices;
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::processing::ProcessingConfig;
use teams_media::relay::RelayServer;
use teams_media::session::SessionConfig;
use teams_media::tone::Tone;
use teams_mock::{
    AB_CHAT, ABC_CHAT, AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE,
    PRESENCE_BASE, PROVISIONING_BASE, TEAM_CHANNEL, TENANT, USER_PROFILE_BASE,
};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use teams_ui::compose::{Compose, Mode};
use teams_ui::shell::{Main, Shell};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use teams_ui::{SettingsFile, VideoFeed};
use tokio::sync::broadcast;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "ui_tour_compose_edge.rs"]
#[expect(
    dead_code,
    reason = "the tour's own half of the edge check, its steps and its child's line, is not used here"
)]
mod compose_edge;
#[path = "../window.rs"]
mod support;
use support::{Alias, display_name};

const CHILD_ENV: &str = "QA_NARROW_CHILD";
const ONLY_ENV: &str = "QA_NARROW_ONLY";
const EVIDENCE_ENV: &str = "QA_NARROW_EVIDENCE";
const TRACE_FD_ENV: &str = "QA_NARROW_TRACE_FD";
const ORIGIN_ENV: &str = "QA_NARROW_ORIGIN";
const TROUTER_ENV: &str = "QA_NARROW_TROUTER";
const RELAY_ENV: &str = "QA_NARROW_RELAY";
const IC3_ENV: &str = "QA_NARROW_IC3";
const SKYPE_ENV: &str = "QA_NARROW_SKYPE";
const DIR_ENV: &str = "QA_NARROW_DIR";
const CTL_ENV: &str = "QA_NARROW_CTL";
/// The file in the sandbox dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "qa429-ask";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(60);
const SETTLE: Duration = Duration::from_millis(700);
/// The time between a key or click and the next, so the app sees each.
const STEP: Duration = Duration::from_millis(250);
/// niri's layout: each window takes the whole screen until the test sizes it, and no hot corner.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
/// The screen.
const SCREEN: (u32, u32) = (1280, 800);
const PEER_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.2,
};
/// The made-up meeting's chat and subject.
const MEETING_CHAT: &str = "19:meeting_MadeUpNarrowReview@thread.v2";
const SUBJECT: &str = "Made-up narrow box review";
/// The mock's team channel's post with replies under it.
const CHANNEL_POST: &str = "1760000300000";

/// A made-up Claude Code (`__STATE__` its state directory) for Correct and Translate. Translate (a question with a
/// `<language>`) gives the draft back with "hello" in German; Correct gives it back with "teh" as "the". It waits
/// while `hold` exists.
const FAKE_CLAUDE: &str = r#"#!/bin/bash
state="__STATE__"
case "$1" in
  --help)
    printf '%s\n' "Usage: claude [options]" "  --model <model>  Model for the current session: an alias such as 'sonnet' or 'opus'"
    exit 0;;
esac
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$state/received.log"
  while [ -e "$state/hold" ]; do sleep 0.02; done
  t=$(printf '%s' "$line" | sed 's/.*<message>\\n\(.*\)\\n<\/message>.*/\1/')
  case "$line" in
    *"<language>"*) t=$(printf '%s' "$t" | sed 's/hello/hallo/g'); ans="<translated>$t</translated>";;
    *) t=$(printf '%s' "$t" | sed 's/teh/the/g'); ans="<corrected>$t</corrected>";;
  esac
  printf '{"type":"result","subtype":"success","is_error":false,"result":"%s"}\n' "$ans"
done
"#;

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_narrow_compose_window: {error}");
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

fn wanted(name: &str) -> bool {
    std::env::var(ONLY_ENV)
        .ok()
        .is_none_or(|list| list.split(',').map(str::trim).any(|one| one == name))
}

// --- the parent ---

type Check = (&'static str, fn(&mut User<'_>) -> SandboxResult);

const CHECKS: [Check; 17] = [
    ("group_call_chat_pane_by_click", group_call_chat_pane_by_click),
    ("type_paste_send", type_paste_send),
    ("long_message_and_edit", long_message_and_edit),
    ("row_tools_act", row_tools_act),
    ("more_menu_by_keys", more_menu_by_keys),
    ("keys_with_tools_in_menu", keys_with_tools_in_menu),
    ("odd_reply_attachment_font", odd_reply_attachment_font),
    ("keyboard_only", keyboard_only),
    ("send_click_keyboard", send_click_keyboard),
    ("meeting_chat_pane", meeting_chat_pane),
    ("main_window_resize", main_window_resize),
    ("channel_and_thread", channel_and_thread),
    ("edge_chat_box", edge_chat_box),
    ("edge_focus_moves", edge_focus_moves),
    ("edge_new_chat", edge_new_chat),
    ("edge_narrow_window", edge_narrow_window),
    ("edge_call_chat", edge_call_chat),
];

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_narrow_compose_window", &options)? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mock = runtime.block_on(ChatServiceMock::start())?;
    if !mock.add_meeting_chat(MEETING_CHAT, SUBJECT, &[LAB_B, LAB_A, LAB_C]) {
        return Err("no meeting chat on the mock".into());
    }
    let trouter = mock.start_trouter()?;
    let dir = tempfile::tempdir()?;
    let relay = mock.start_group_video(dir.path(), PEER_TONE)?;
    let tool = FakeTool::install(&sandbox)?;
    let app = AppUnderTest::start(&sandbox, &mock, &relay.to_string(), trouter.origin(), dir.path(), &tool)?;
    let mut user = User::new(&sandbox, &mock, &runtime, app, &tool)?;
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    for (name, check) in CHECKS {
        if wanted(name) {
            user.run_check(name, check, &mut passed, &mut failed);
        }
    }
    let quit = user.quit();
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
        let _ = fs::write(PathBuf::from(dir).join("results.txt"), &summary);
    }
    say(&format!(
        "qa_narrow_compose_window: {} passed, {} failed",
        passed.len(),
        failed.len()
    ));
    quit?;
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("{} checks failed", failed.len()).into())
    }
}

/// The made-up tool as the parent sees it: where it is, and the file that holds its answers.
struct FakeTool {
    state: PathBuf,
    home: PathBuf,
    /// The child's `PATH`: the tool's dir, then the sandbox's own, without any directory holding a real `claude` or
    /// `codex`.
    path: String,
}

impl FakeTool {
    fn install(sandbox: &Sandbox) -> SandboxResult<Self> {
        use std::os::unix::fs::PermissionsExt as _;
        let (bin, state, home, commands) = (
            sandbox.path("ai-bin"),
            sandbox.path("ai-state"),
            sandbox.path("ai-home"),
            sandbox.path("ai-commands"),
        );
        for dir in [&bin, &state, &home, &commands] {
            fs::create_dir_all(dir)?;
        }
        let inherited = sandbox
            .client_env()
            .into_iter()
            .find(|(name, _)| *name == "PATH")
            .map(|(_, path)| path)
            .unwrap_or_default();
        let kept: Vec<PathBuf> = std::env::split_paths(&inherited)
            .filter(|dir| !["claude", "codex"].iter().any(|tool| dir.join(tool).exists()))
            .collect();
        for command in ["sed", "sleep"] {
            let found = std::env::split_paths(&inherited)
                .map(|dir| dir.join(command))
                .find(|candidate| candidate.is_file())
                .ok_or_else(|| format!("no {command} on the PATH"))?;
            std::os::unix::fs::symlink(found, commands.join(command))?;
        }
        let program = bin.join("claude");
        fs::write(&program, FAKE_CLAUDE.replace("__STATE__", &state.display().to_string()))?;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755))?;
        let path = std::env::join_paths([bin, commands].into_iter().chain(kept))?
            .into_string()
            .map_err(|_| "a PATH that isn't UTF-8")?;
        Ok(Self { state, home, path })
    }

    /// The tool waits before it answers while `on`.
    fn hold(&self, on: bool) -> SandboxResult {
        let file = self.state.join("hold");
        if on {
            fs::write(file, "")?;
        } else if file.exists() {
            fs::remove_file(file)?;
        }
        Ok(())
    }
}

/// The child app in its sandbox: its process, its trace, its control socket.
struct AppUnderTest {
    running: Option<Running>,
    /// Kept, so the child can write its trace to the end.
    _trace: TraceReader,
    socket: PathBuf,
}

impl AppUnderTest {
    fn start(
        sandbox: &Sandbox,
        mock: &ChatServiceMock,
        relay: &str,
        trouter: &str,
        dir: &Path,
        tool: &FakeTool,
    ) -> SandboxResult<Self> {
        let own = dir.join("a");
        fs::create_dir_all(&own)?;
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let ic3 = mock.token(LAB_A).ok_or("the mock's token")?;
        let skype = mock.skypetoken(LAB_A).ok_or("the mock's skypetoken")?;
        let socket = sandbox.path("run/ctl.sock");
        let own_text = own.to_string_lossy().into_owned();
        let socket_text = socket.to_string_lossy().into_owned();
        let home = tool.home.display().to_string();
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
                ("PATH", &tool.path),
                ("CLAUDE_CONFIG_DIR", &home),
                ("CODEX_HOME", &home),
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
        let deadline = Instant::now() + WAIT;
        while !self
            .catch_up()
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

/// The user at the sandbox: the pointer, the keyboard, the screenshots, the control socket, and the child it asks
/// about the window.
struct User<'a> {
    sandbox: &'a Sandbox,
    mock: &'a ChatServiceMock,
    runtime: &'a tokio::runtime::Runtime,
    app: AppUnderTest,
    tool: &'a FakeTool,
    input: VirtualInput,
    ctl: Client,
    asked: u32,
    /// niri's ids of the main window and the call window.
    main_window: Option<u64>,
    call_window: Option<u64>,
    /// The group call on the mock, while a is in it.
    call: Option<String>,
    marks: u32,
}

impl<'a> User<'a> {
    fn new(
        sandbox: &'a Sandbox,
        mock: &'a ChatServiceMock,
        runtime: &'a tokio::runtime::Runtime,
        app: AppUnderTest,
        tool: &'a FakeTool,
    ) -> SandboxResult<Self> {
        let socket = app.socket.clone();
        let ctl = wait_until(WAIT, || Client::connect(&socket, Timeouts::default()).ok())
            .ok_or_else(|| format!("no control socket at {}", socket.display()))?;
        let mut user = Self {
            sandbox,
            mock,
            runtime,
            app,
            tool,
            input: sandbox.input()?,
            ctl,
            asked: 0,
            main_window: None,
            call_window: None,
            call: None,
            marks: 0,
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

    fn type_text(&mut self, text: &str) -> SandboxResult {
        self.input.type_text(text)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn click(&mut self, (x, y): (i32, i32)) -> SandboxResult {
        self.input.click(u32::try_from(x)?, u32::try_from(y)?, keys::BTN_LEFT)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn park(&mut self) -> SandboxResult {
        // the window's left margin: no button, tooltip or tile there
        self.input.move_to(3, SCREEN.1 / 2)
    }

    /// A unique made-up word for a message, found again in what the mock was sent.
    fn mark(&mut self) -> String {
        self.marks += 1;
        format!("zq{}x", 100 + self.marks)
    }

    /// The bodies of the requests the mock got with `method` that hold `text`.
    fn sent_with(&self, method: &str, text: &str) -> Vec<String> {
        self.runtime
            .block_on(self.mock.received_requests())
            .iter()
            .filter(|request| request.method.as_str() == method)
            .map(|request| String::from_utf8_lossy(&request.body).into_owned())
            .filter(|body| body.contains(text))
            .collect()
    }

    fn wait_sent(&self, method: &str, text: &str) -> SandboxResult<String> {
        wait_until(WAIT, || self.sent_with(method, text).into_iter().next())
            .ok_or_else(|| format!("the mock got no {method} with {text:?}").into())
    }

    /// A screenshot as `qa429-<name>.png`, under `target/ui-shots/` and the evidence dir.
    fn shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        let file = format!("qa429-{name}.png");
        let png = self.sandbox.screenshot(&file, false)?;
        let kept = shots_dir()?.join(&file);
        fs::copy(&png, &kept)?;
        if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
            let dir = PathBuf::from(dir).join("shots");
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(&file))?;
        }
        Ok(kept)
    }

    fn settled_shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        self.park()?;
        thread::sleep(SETTLE);
        self.shot(name)
    }

    /// The same screen in Teams Light and Teams Dark; Teams Light after.
    fn shots_in_both_themes(&mut self, name: &str) -> SandboxResult {
        self.settled_shot(&format!("{name}-light"))?;
        self.ask("theme-dark")?;
        self.settled_shot(&format!("{name}-dark"))?;
        self.ask("theme-light")?;
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

    /// The size niri gives window `id`.
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

    fn focus_window(&mut self, id: Option<u64>) -> SandboxResult {
        let id = id.ok_or("no such window")?;
        self.sandbox
            .niri_msg(&["action", "focus-window", "--id", &id.to_string()])?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The compositor sizes the focused window to `width` wide, as a user's drag does; the window takes the screen's
    /// left edge.
    fn resize_focused(&mut self, width: i32) -> SandboxResult {
        let id = self.focused_window()?.ok_or("no window has the focus")?;
        self.sandbox
            .niri_msg(&["action", "set-column-width", &width.to_string()])?;
        wait_until(WAIT, || {
            self.window_size(id).ok().filter(|size| size.0 == width).map(|_| ())
        })
        .ok_or_else(|| format!("the window never took {width} px"))?;
        thread::sleep(SETTLE);
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
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// The call window's id, once niri shows a window other than the main window with the focus.
    fn wait_call_window(&mut self) -> SandboxResult {
        let main = self.main_window;
        let id = wait_until(WAIT, || {
            self.windows().ok()?.iter().find_map(|window| {
                let id = window["id"].as_u64()?;
                (Some(id) != main && window["is_focused"] == true).then_some(id)
            })
        })
        .ok_or("no call window")?;
        self.call_window = Some(id);
        wait_until(WAIT, || {
            self.window_size(id)
                .ok()
                .filter(|size| size.0 == i32::try_from(SCREEN.0).unwrap_or(0))
        })
        .ok_or("the call window never filled the screen")?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// b rings a in the a–b–c chat's group call; a answers (as `ngteamsctl answer` does). Nothing when a is in it.
    fn group_call(&mut self) -> SandboxResult {
        if self.call.is_some() && self.calls()?.iter().any(|row| row.state == "connected") {
            return self.focus_window(self.call_window);
        }
        self.call = Some(self.mock.ring_group(LAB_B, ABC_CHAT, &[LAB_A]));
        self.wait_call("ringing", |row| row.ringing)?;
        self.ctl.request(None, Request::Answer)?;
        self.wait_call("connected", |row| row.state == "connected")?;
        self.wait_call_window()?;
        self.park()
    }

    /// a leaves the call it is in.
    fn hang_up(&mut self) -> SandboxResult {
        if self.calls()?.is_empty() {
            return Ok(());
        }
        self.ctl.request(None, Request::HangUp)?;
        let deadline = Instant::now() + WAIT;
        while self.calls()?.iter().any(|row| row.state == "connected") {
            if Instant::now() > deadline {
                return Err("the call never ended".into());
            }
            thread::sleep(Duration::from_millis(100));
        }
        self.call = None;
        self.call_window = None;
        thread::sleep(SETTLE);
        Ok(())
    }
}

// --- the checks ---

// --- where things are in the call window, 1280x800, the Chat pane open (its box stacked) ---

/// The call toolbar's Chat, and a meeting's (its toolbar also has Controls).
const CALL_CHAT: (i32, i32) = (708, 27);
const MEETING_CHAT_BUTTON: (i32, i32) = (630, 27);
/// The text field of the Chat pane's box, and the row under it.
const CALL_TEXT: (i32, i32) = (1090, 714);
const CALL_ROW_Y: i32 = 749;
const CALL_FORMAT: (i32, i32) = (951, CALL_ROW_Y);
const CALL_EMOJI: (i32, i32) = (982, CALL_ROW_Y);
const CALL_LOOP: (i32, i32) = (1044, CALL_ROW_Y);
const CALL_APPS: (i32, i32) = (1075, CALL_ROW_Y);
const CALL_MORE: (i32, i32) = (1106, CALL_ROW_Y);
const CALL_SEND: (i32, i32) = (1232, CALL_ROW_Y);

/// A long made-up message, more than the pane's width many times.
const LONG: &str = "This is a long made-up message for the narrow chat pane of the call. It goes on for many lines so \
                    that the text field must wrap it and grow, and the row of tools and Send must stay under it where \
                    the user can still reach them with the pointer and the keys.";

fn call_box(state: &Value) -> &Value {
    &state["call"]["box"]
}

fn text_of(boxed: &Value) -> String {
    boxed["text"].as_str().unwrap_or_default().trim().to_owned()
}

impl User<'_> {
    /// The group call up, its Chat pane open (by its toolbar button), the box empty, Teams Light.
    fn call_chat(&mut self) -> SandboxResult<Value> {
        self.group_call()?;
        let state = self.state()?;
        if state["call"]["chat_shown"] != true {
            self.click(CALL_CHAT)?;
            self.until("the call's Chat pane", |state| state["call"]["chat_shown"] == true)?;
        }
        self.ask("theme-light")?;
        self.ask("call-clear")?;
        self.click(CALL_TEXT)?;
        self.until("the call box empty and focused", |state| {
            text_of(call_box(state)).is_empty() && call_box(state)["focused"] == true
        })
    }

    /// Opens More options by its button and waits for it.
    fn open_more(&mut self) -> SandboxResult<Value> {
        self.click(CALL_MORE)?;
        self.until("More options open", |state| call_box(state)["more_open"] == true)
    }
}

fn group_call_chat_pane_by_click(user: &mut User<'_>) -> SandboxResult {
    user.group_call()?;
    user.settled_shot("call-no-pane")?;
    user.click(CALL_CHAT)?;
    let state = user.until("the call's Chat pane", |state| state["call"]["chat_shown"] == true)?;
    let boxed = call_box(&state).clone();
    say(&format!("  the box as the pane opens: {boxed}"));
    ensure(boxed["stacked"] == true, || format!("the box is not stacked: {boxed}"))?;
    ensure(boxed["more_open"] == false, || format!("More options open: {boxed}"))?;
    user.shots_in_both_themes("call-chat-empty")
}

fn type_paste_send(user: &mut User<'_>) -> SandboxResult {
    user.call_chat()?;
    let mark = user.mark();
    user.type_text(&format!("hello team {mark}"))?;
    user.press("ctrl+a")?;
    user.press("ctrl+c")?;
    user.press("ctrl+end")?;
    user.type_text(" and ")?;
    user.press("ctrl+v")?;
    let state = user.until("the paste", |state| {
        text_of(call_box(state)).matches(mark.as_str()).count() == 2
    })?;
    ensure(call_box(&state)["stacked"] == true, || format!("not stacked: {state}"))?;
    user.shots_in_both_themes("call-chat-typed-pasted")?;
    user.click(CALL_SEND)?;
    let body = user.wait_sent("POST", &mark)?;
    say(&format!("  sent by Send: {}", short_body(&body)));
    let state = user.until("the box empty after Send", |state| text_of(call_box(state)).is_empty())?;
    say(&format!(
        "  the text has the keyboard after a click on Send: {}",
        call_box(&state)["focused"]
    ));
    user.click(CALL_TEXT)?;
    let second = user.mark();
    user.type_text(&format!("sent by Enter {second}"))?;
    user.press("enter")?;
    user.wait_sent("POST", &second)?;
    user.until("the box empty after Enter", |state| text_of(call_box(state)).is_empty())?;
    user.settled_shot("call-chat-sent")?;
    Ok(())
}

fn long_message_and_edit(user: &mut User<'_>) -> SandboxResult {
    user.call_chat()?;
    let mark = user.mark();
    user.type_text(&format!("{LONG} {mark}"))?;
    let state = user.until("the long draft", |state| text_of(call_box(state)).ends_with(&mark))?;
    ensure(call_box(&state)["stacked"] == true, || format!("not stacked: {state}"))?;
    user.shots_in_both_themes("call-chat-long")?;
    user.press("enter")?;
    user.wait_sent("POST", &mark)?;
    user.until("the box empty", |state| text_of(call_box(state)).is_empty())?;
    thread::sleep(SETTLE);
    // Up in the empty box edits the last own message
    user.press("up")?;
    let state = user.until("the edit", |state| call_box(state)["mode"] == "edit")?;
    ensure(text_of(call_box(&state)).ends_with(&mark), || {
        format!("the edit holds another text: {state}")
    })?;
    user.shots_in_both_themes("call-chat-edit")?;
    let edited = user.mark();
    user.press("ctrl+end")?;
    user.type_text(&format!(" edited {edited}"))?;
    user.press("enter")?;
    user.wait_sent("PUT", &edited)?;
    let state = user.until("the box back to new", |state| call_box(state)["mode"] == "new")?;
    ensure(text_of(call_box(&state)).is_empty(), || {
        format!("the box after the edit: {state}")
    })
}

fn row_tools_act(user: &mut User<'_>) -> SandboxResult {
    user.call_chat()?;
    user.type_text("row tools")?;
    // Format
    user.click(CALL_FORMAT)?;
    let state = user.until("formatting shown", |state| call_box(state)["expanded"] == true)?;
    ensure(call_box(&state)["stacked"] == true, || format!("not stacked: {state}"))?;
    // the format bar fits the narrow box: the tools that do not fit are in its own More options
    let state = user.state()?;
    let in_more = &call_box(&state)["format_in_more"];
    ensure(
        in_more
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool == "format-code")),
        || format!("the format bar keeps every tool in a narrow box: {state}"),
    )?;
    user.shots_in_both_themes("call-chat-format-open")?;
    user.ask("call-format-more")?;
    user.shots_in_both_themes("call-chat-format-more")?;
    user.ask("call-format-more")?;
    user.click_after_layout_change("format")?;
    user.until("formatting hidden", |state| call_box(state)["expanded"] == false)?;
    // Emoji
    user.click(CALL_EMOJI)?;
    user.until("the emoji picker", |state| call_box(state)["emoji_open"] == true)?;
    user.settled_shot("call-chat-emoji-open")?;
    user.press("escape")?;
    let state = user.until("the emoji picker closed", |state| {
        call_box(state)["emoji_open"] == false
    })?;
    ensure(call_box(&state)["focused"] == true, || {
        format!("the box lost the keyboard after the picker: {state}")
    })?;
    // Loop and Actions and apps are shown off: a click does nothing
    user.click(CALL_LOOP)?;
    user.click(CALL_APPS)?;
    let state = user.state()?;
    ensure(text_of(call_box(&state)) == "row tools", || {
        format!("the draft changed: {state}")
    })?;
    ensure(call_box(&state)["more_open"] == false, || {
        format!("a menu opened: {state}")
    })
}

impl User<'_> {
    /// Format's second click: the expanded box puts its row at the foot of a taller box, so the button is found again
    /// by the key that does the same.
    fn click_after_layout_change(&mut self, _what: &str) -> SandboxResult {
        self.press("ctrl+shift+x")
    }
}

fn more_menu_by_keys(user: &mut User<'_>) -> SandboxResult {
    user.call_chat()?;
    user.type_text("hello teh team")?;
    let state = user.open_more()?;
    ensure(call_box(&state)["focused"] == false, || {
        format!("the text keeps the keyboard with the menu open: {state}")
    })?;
    user.shots_in_both_themes("call-chat-more-open")?;
    // Escape: closed, the keyboard back in the text
    user.press("escape")?;
    let state = user.until("More options closed", |state| call_box(state)["more_open"] == false)?;
    ensure(call_box(&state)["focused"] == true, || {
        format!("Escape did not give the keyboard back: {state}")
    })?;
    user.type_text(" ok")?;
    user.until("the text after Escape", |state| {
        text_of(call_box(state)) == "hello teh team ok"
    })?;
    user.press("backspace")?;
    user.press("backspace")?;
    user.press("backspace")?;
    // Down, Enter: the first row, Correct
    user.open_more()?;
    user.tool.hold(true)?;
    user.press("down")?;
    user.settled_shot("call-chat-more-first-row")?;
    user.press("enter")?;
    let state = user.until("Correct by its row", |state| call_box(state)["correction"] == "Running")?;
    ensure(call_box(&state)["more_open"] == false, || {
        format!("the menu stays open: {state}")
    })?;
    // the menu now says Stop correcting, its row checked
    user.open_more()?;
    user.settled_shot("call-chat-more-stop-correcting")?;
    user.press("escape")?;
    user.tool.hold(false)?;
    let state = user.until("the correction", |state| call_box(state)["correction"] == "Proposed")?;
    ensure(call_box(&state)["focused"] == true, || {
        format!("the text has no keyboard: {state}")
    })?;
    user.settled_shot("call-chat-correction-proposed")?;
    // Correct again (its row) gives the keyboard to the proposal; Enter takes it
    user.open_more()?;
    user.shots_in_both_themes("call-chat-more-correction-shown")?;
    user.press("down")?;
    user.press("enter")?;
    user.press("enter")?;
    user.until("the correction taken", |state| {
        text_of(call_box(state)) == "hello the team"
    })?;
    // Up from no row: the last row that can be taken, the language
    user.open_more()?;
    user.press("up")?;
    user.settled_shot("call-chat-more-up-last")?;
    user.press("enter")?;
    let state = user.until("the language menu", |state| call_box(state)["language_open"] == true)?;
    ensure(call_box(&state)["more_open"] == false, || {
        format!("More still open: {state}")
    })?;
    user.shots_in_both_themes("call-chat-language-menu")?;
    user.type_text("German")?;
    user.press("enter")?;
    user.until("German chosen", |state| state["language"] == "German")?;
    user.press("escape")?;
    // Down, Down, Enter: Translate, now to German
    user.open_more()?;
    user.settled_shot("call-chat-more-german")?;
    user.press("down")?;
    user.press("down")?;
    user.press("enter")?;
    let state = user.until("the translation", |state| text_of(call_box(state)) == "hallo the team")?;
    ensure(call_box(&state)["focused"] == true, || {
        format!("the box has no keyboard: {state}")
    })
}

fn keys_with_tools_in_menu(user: &mut User<'_>) -> SandboxResult {
    user.call_chat()?;
    user.ask("language English")?;
    user.type_text("hello teh keys")?;
    user.tool.hold(true)?;
    user.press("ctrl+shift+g")?;
    user.until("Correct by Ctrl+Shift+G", |state| {
        call_box(state)["correction"] == "Running"
    })?;
    user.press("ctrl+shift+g")?;
    user.until("the correction stopped by its key", |state| {
        call_box(state)["correction"] != "Running"
    })?;
    user.press("ctrl+shift+y")?;
    user.until("Translate by Ctrl+Shift+Y", |state| {
        call_box(state)["translation"] == "Running"
    })?;
    user.open_more()?;
    user.settled_shot("call-chat-more-stop-translating")?;
    user.press("escape")?;
    user.press("ctrl+shift+y")?;
    user.until("the translation stopped by its key", |state| {
        call_box(state)["translation"] != "Running"
    })?;
    user.tool.hold(false)?;
    let state = user.state()?;
    ensure(text_of(call_box(&state)) == "hello teh keys", || {
        format!("the draft after the stops: {state}")
    })?;
    user.press("ctrl+shift+x")?;
    user.until("Format by its key", |state| call_box(state)["expanded"] == true)?;
    user.press("ctrl+shift+x")?;
    user.until("Format off by its key", |state| call_box(state)["expanded"] == false)?;
    user.press("ctrl+.")?;
    user.until("Emoji by its key", |state| call_box(state)["emoji_open"] == true)?;
    user.press("ctrl+.")?;
    let state = user.until("Emoji closed by its key", |state| {
        call_box(state)["emoji_open"] == false
    })?;
    ensure(call_box(&state)["focused"] == true, || {
        format!("the box has no keyboard: {state}")
    })
}

fn odd_reply_attachment_font(user: &mut User<'_>) -> SandboxResult {
    user.call_chat()?;
    // a reply over the box
    let reply = user.ask("call-reply-last")?;
    ensure(reply == "ok", || format!("no reply: {reply}"))?;
    let state = user.until("the reply", |state| call_box(state)["mode"] == "reply")?;
    ensure(call_box(&state)["stacked"] == true, || format!("not stacked: {state}"))?;
    let mark = user.mark();
    user.type_text(&format!("a reply {mark}"))?;
    user.shots_in_both_themes("call-chat-reply")?;
    user.click(CALL_SEND)?;
    user.wait_sent("POST", &mark)?;
    user.until("the box back to new", |state| call_box(state)["mode"] == "new")?;
    // a file pasted: its chip over the row
    let file = user.sandbox.path("made-up-notes.txt");
    fs::write(&file, "made-up notes\n")?;
    user.sandbox.set_clipboard(vec![(
        "text/uri-list".to_owned(),
        format!("file://{}\r\n", file.display()).into_bytes(),
    )])?;
    user.click(CALL_TEXT)?;
    user.press("ctrl+v")?;
    let state = user.until("the file's chip", |state| call_box(state)["attachments"] == 1)?;
    say(&format!("  the box with a file: {}", call_box(&state)));
    user.shots_in_both_themes("call-chat-attachment")?;
    user.ask("call-clear")?;
    // the largest font: the row keeps fewer tools
    user.ask("font 24")?;
    thread::sleep(SETTLE);
    user.ask("call-focus")?;
    user.type_text("big font")?;
    user.shots_in_both_themes("call-chat-font-24")?;
    user.ask("call-open-more")?;
    user.settled_shot("call-chat-font-24-more")?;
    user.press("escape")?;
    user.ask("font 14")?;
    Ok(())
}

fn keyboard_only(user: &mut User<'_>) -> SandboxResult {
    user.call_chat()?;
    // from the text, Tab n times, then Enter: what each stop of the keyboard does
    let mut seen = Vec::new();
    for tabs in 1..=8 {
        user.ask("call-reset")?;
        user.type_text("by keys")?;
        for _ in 0..tabs {
            user.press("tab")?;
        }
        user.settled_shot(&format!("call-chat-tab-{tabs}"))?;
        user.press("enter")?;
        let state = user.state()?;
        let boxed = call_box(&state);
        // the language is in More options: no stop of its own, so Enter on a stop never opens its menu
        ensure(boxed["language_open"] == false, || {
            format!("stop {tabs} is a hidden one: Enter opened the language menu: {boxed}")
        })?;
        // the stops are Format, Emoji, Attach, "+", More options, then Send
        if tabs == 6 {
            ensure(text_of(boxed).is_empty(), || {
                format!("Enter on stop 6 did not send: {boxed}")
            })?;
        }
        seen.push(format!(
            "tab x{tabs} then enter: text {:?} more {} language {} emoji {} expanded {} focused {}",
            text_of(boxed),
            boxed["more_open"],
            boxed["language_open"],
            boxed["emoji_open"],
            boxed["expanded"],
            boxed["focused"]
        ));
        if boxed["more_open"] == true {
            user.settled_shot("call-chat-more-by-tab")?;
            user.press("down")?;
            user.press("enter")?;
            let state = user.state()?;
            seen.push(format!(
                "  its first row: correction {}",
                call_box(&state)["correction"]
            ));
        }
        user.press("escape")?;
    }
    user.ask("call-reset")?;
    say(&format!("  keyboard only: {}", seen.join("; ")));
    Ok(())
}

/// A click on Send: the text keeps the keyboard, in the narrow call box and in a wide box, as in Teams.
fn send_click_keyboard(user: &mut User<'_>) -> SandboxResult {
    user.call_chat()?;
    let mark = user.mark();
    user.type_text(&format!("narrow send {mark}"))?;
    user.click(CALL_SEND)?;
    user.wait_sent("POST", &mark)?;
    let state = user.until("the call box empty", |state| text_of(call_box(state)).is_empty())?;
    let narrow = call_box(&state)["focused"].clone();
    user.hang_up()?;
    user.focus_window(user.main_window)?;
    user.resize_focused(1280)?;
    user.ask(&format!("open {AB_CHAT}"))?;
    user.ask("clear")?;
    user.click((700, 750))?;
    let wide_mark = user.mark();
    user.type_text(&format!("wide send {wide_mark}"))?;
    user.click(MAIN_SEND)?;
    user.wait_sent("POST", &wide_mark)?;
    let state = user.until("the main box empty", |state| text_of(main_box(state)).is_empty())?;
    let wide = main_box(&state)["focused"].clone();
    say(&format!(
        "  the text keeps the keyboard after a click on Send: narrow {narrow}, wide {wide}"
    ));
    ensure(narrow == true && wide == true, || {
        format!("the text lost the keyboard after a click on Send: narrow {narrow}, wide {wide}")
    })
}

fn meeting_chat_pane(user: &mut User<'_>) -> SandboxResult {
    user.hang_up()?;
    user.focus_window(user.main_window)?;
    user.ask("join-meeting")?;
    user.until("the pre-join", |state| state["prejoin"] == true)?;
    user.settled_shot("meeting-prejoin")?;
    user.press("enter")?;
    user.wait_call("connected in the meeting", |row| row.state == "connected")?;
    user.wait_call_window()?;
    user.call = Some("meeting".to_owned());
    user.park()?;
    user.settled_shot("meeting-no-pane")?;
    user.click(MEETING_CHAT_BUTTON)?;
    let state = user.until("the meeting's Chat pane", |state| state["call"]["chat_shown"] == true)?;
    ensure(call_box(&state)["stacked"] == true, || format!("not stacked: {state}"))?;
    user.click(CALL_TEXT)?;
    let mark = user.mark();
    user.type_text(&format!("meeting note {mark}"))?;
    user.shots_in_both_themes("meeting-chat-typed")?;
    user.open_more()?;
    user.shots_in_both_themes("meeting-chat-more")?;
    user.press("escape")?;
    user.click(CALL_SEND)?;
    user.wait_sent("POST", &mark)?;
    user.until("the box empty", |state| text_of(call_box(state)).is_empty())?;
    user.settled_shot("meeting-chat-sent")?;
    user.hang_up()
}

/// Send in the main window's chat box, the window 1280 px wide.
const MAIN_SEND: (i32, i32) = (1236, 749);

fn main_box(state: &Value) -> &Value {
    &state["chat"]
}

fn main_window_resize(user: &mut User<'_>) -> SandboxResult {
    user.hang_up()?;
    user.focus_window(user.main_window)?;
    user.resize_focused(1280)?;
    user.ask(&format!("open {AB_CHAT}"))?;
    user.ask("clear")?;
    user.click((700, 750))?;
    user.type_text("a draft across sizes")?;
    let state = user.until("the draft", |state| text_of(main_box(state)) == "a draft across sizes")?;
    ensure(main_box(&state)["stacked"] == false, || {
        format!("stacked at 1280: {state}")
    })?;
    ensure(main_box(&state)["focused"] == true, || format!("no keyboard: {state}"))?;
    user.shots_in_both_themes("main-1280")?;
    let mut seen = Vec::new();
    for width in [1000, 800, 700, 640, 560, 800, 1280] {
        user.resize_focused(width)?;
        let state = user.state()?;
        let boxed = main_box(&state);
        seen.push(format!(
            "{width}: stacked {} narrow {} text {:?} focused {}",
            boxed["stacked"],
            boxed["narrow"],
            text_of(boxed),
            boxed["focused"]
        ));
        ensure(text_of(boxed) == "a draft across sizes", || {
            format!("the draft at {width}: {state}")
        })?;
        ensure(boxed["focused"] == true, || format!("the keyboard at {width}: {state}"))?;
        user.settled_shot(&format!("main-{width}"))?;
        if width == 560 {
            very_narrow(user)?;
        }
    }
    say(&format!("  sizes: {}", seen.join("; ")));
    let state = user.state()?;
    ensure(main_box(&state)["stacked"] == false, || {
        format!("stacked again at 1280: {state}")
    })?;
    user.type_text(" ok")?;
    user.until("the keyboard after the sizes", |state| {
        text_of(main_box(state)) == "a draft across sizes ok"
    })?;
    user.ask("clear")?;
    Ok(())
}

/// The narrowest window at the largest font: the narrowest box the app draws; its More options open.
fn very_narrow(user: &mut User<'_>) -> SandboxResult {
    user.ask("font 24")?;
    thread::sleep(SETTLE);
    user.shots_in_both_themes("main-560-font-24")?;
    user.ask("open-more")?;
    let state = user.until("More options in the main box", |state| {
        main_box(state)["more_open"] == true
    })?;
    say(&format!("  560 px, font 24: {}", main_box(&state)));
    user.settled_shot("main-560-font-24-more")?;
    user.press("escape")?;
    let state = user.until("More closed", |state| main_box(state)["more_open"] == false)?;
    ensure(main_box(&state)["focused"] == true, || {
        format!("no keyboard after Escape: {state}")
    })?;
    user.ask("font 14")?;
    thread::sleep(SETTLE);
    Ok(())
}

fn channel_and_thread(user: &mut User<'_>) -> SandboxResult {
    user.focus_window(user.main_window)?;
    user.resize_focused(1280)?;
    user.ask("open-channel")?;
    let state = user.until("the channel", |state| !state["post"].is_null())?;
    ensure(state["post"]["stacked"] == false, || {
        format!("the post box stacked at 1280: {state}")
    })?;
    user.ask("open-thread")?;
    let state = user.until("the thread", |state| !state["thread"].is_null())?;
    say(&format!("  at 1280: post {} thread {}", state["post"], state["thread"]));
    user.shots_in_both_themes("channel-thread-1280")?;
    let mut seen = Vec::new();
    for width in [1000, 800, 640, 1280] {
        user.resize_focused(width)?;
        let state = user.state()?;
        seen.push(format!(
            "{width}: post stacked {} thread stacked {} thread narrow {}",
            state["post"]["stacked"], state["thread"]["stacked"], state["thread"]["narrow"]
        ));
        user.settled_shot(&format!("channel-thread-{width}"))?;
    }
    user.ask("close-thread")?;
    for width in [800, 1280] {
        user.resize_focused(width)?;
        let state = user.state()?;
        seen.push(format!("{width} no thread: post stacked {}", state["post"]["stacked"]));
        user.settled_shot(&format!("channel-{width}"))?;
    }
    say(&format!("  channel: {}", seen.join("; ")));
    Ok(())
}

// --- task 581: the box's edge ---

/// The text of the main window's chat box at 1280 px.
const CHAT_TEXT: (i32, i32) = (700, 750);

/// The box under `key` in the state: `call` for the call's Chat pane, else the main window's (`chat`, `post`,
/// `thread`, `new_chat`).
fn box_at<'a>(state: &'a Value, key: &str) -> &'a Value {
    if key == "call" { call_box(state) } else { &state[key] }
}

/// A place in the box's one line of text near its left end: beside the tools of a box on one line, or just over the
/// tools' row of a stacked box, under any row over its text (a thread's "Send to").
#[expect(
    clippy::cast_possible_truncation,
    reason = "a window's logical px are small whole numbers"
)]
fn in_text(boxed: &Value) -> SandboxResult<(i32, i32)> {
    let at = |index: usize| boxed["bounds"][index].as_f64().map(|value| value.round() as i32);
    let (Some(x), Some(y), Some(height)) = (at(0), at(1), at(3)) else {
        return Err(format!("the box was not drawn: {boxed}").into());
    };
    let above_foot = if boxed["stacked"] == true { 64 } else { 27 };
    Ok((x + 40, y + height - above_foot))
}

impl User<'_> {
    /// A shot of the screen as it stands; the edge check on each box of `keys` in it.
    fn edge_shot(&mut self, name: &str, keys: &[&str]) -> SandboxResult {
        self.park()?;
        let edges = |state: &Value| {
            keys.iter()
                .map(|key| box_at(state, key)["edge"].clone())
                .collect::<Vec<_>>()
        };
        // a loaded compositor can show a frame late: the boxes stand still from before the shot to after it
        let mut tries = 0;
        let (state, png) = loop {
            thread::sleep(SETTLE);
            let before = self.state()?;
            thread::sleep(SETTLE);
            let png = self.shot(name)?;
            let after = self.state()?;
            tries += 1;
            if edges(&before) == edges(&after) || tries == 3 {
                break (after, png);
            }
        };
        for key in keys {
            let said = box_at(&state, key)["edge"]
                .as_str()
                .ok_or_else(|| format!("{name}: no {key} box: {state}"))?;
            say(&format!("  {name} {key}: {said}"));
            compose_edge::check(&png, said).map_err(|error| format!("{name}, the {key} box: {error}"))?;
        }
        Ok(())
    }

    /// [`Self::edge_shot`] in Teams Light and Teams Dark; Teams Light after.
    fn edge_shots(&mut self, name: &str, keys: &[&str]) -> SandboxResult {
        self.edge_shot(&format!("{name}-light"), keys)?;
        self.ask("theme-dark")?;
        let dark = self.edge_shot(&format!("{name}-dark"), keys);
        self.ask("theme-light")?;
        thread::sleep(SETTLE);
        dark
    }

    /// The chat's box emptied, then clicked: "clear" also empties a channel kept from before, which takes the focus.
    fn clear_chat_box(&mut self) -> SandboxResult {
        self.ask("clear")?;
        self.click(CHAT_TEXT)?;
        self.until("the chat box empty and focused", |state| {
            text_of(main_box(state)).is_empty() && main_box(state)["focused"] == true
        })?;
        Ok(())
    }

    /// The main window 1280 px wide, Teams Light at 14 px, the a–b chat open and its box empty and focused.
    fn main_chat(&mut self) -> SandboxResult<Value> {
        self.hang_up()?;
        self.focus_window(self.main_window)?;
        self.resize_focused(1280)?;
        self.ask("theme-light")?;
        self.ask("font 14")?;
        self.ask(&format!("open {AB_CHAT}"))?;
        self.ask("clear")?;
        self.click(CHAT_TEXT)?;
        self.until("the chat box empty and focused", |state| {
            text_of(main_box(state)).is_empty() && main_box(state)["focused"] == true
        })
    }
}

/// The chat's box in each state a user meets: no focus, empty, typed, more lines, the format bar and its menu, the
/// emoji picker, a reply, a file, an edit, other font sizes.
fn edge_chat_box(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    // a click in the messages leaves the focus in the box, as in Teams; Escape in the empty box gives it to the list
    user.press("escape")?;
    user.until("the box without the focus", |state| main_box(state)["focused"] == false)?;
    user.edge_shots("edge-chat-unfocused", &["chat"])?;
    user.click(CHAT_TEXT)?;
    user.until("the box focused", |state| main_box(state)["focused"] == true)?;
    user.edge_shots("edge-chat-empty", &["chat"])?;
    user.type_text("Lunch at noon?")?;
    user.edge_shots("edge-chat-typed", &["chat"])?;
    user.press("shift+enter")?;
    user.type_text("The usual place, or the new one?")?;
    user.press("shift+enter")?;
    user.type_text("Either is fine.")?;
    user.until("the three lines", |state| {
        text_of(main_box(state)).ends_with("Either is fine.")
    })?;
    user.edge_shots("edge-chat-lines", &["chat"])?;
    user.clear_chat_box()?;
    // the format bar by its key, Send lit by a draft
    user.press("ctrl+shift+x")?;
    user.until("the format bar", |state| main_box(state)["expanded"] == true)?;
    user.type_text("Lunch at noon?")?;
    user.edge_shots("edge-chat-format", &["chat"])?;
    // a menu of the bar and the emoji picker open over the box, not cut by it
    user.ask("format-menu")?;
    user.settled_shot("edge-chat-format-menu")?;
    user.ask("format-menu")?;
    user.press("ctrl+shift+x")?;
    user.until("no format bar", |state| main_box(state)["expanded"] == false)?;
    user.press("ctrl+;")?;
    user.until("the emoji picker", |state| main_box(state)["emoji_open"] == true)?;
    user.settled_shot("edge-chat-emoji")?;
    user.press("escape")?;
    user.until("the emoji picker closed", |state| {
        main_box(state)["emoji_open"] == false
    })?;
    user.clear_chat_box()?;
    edge_reply_file_edit(user)?;
    for size in [10, 15, 18, 24] {
        user.ask(&format!("font {size}"))?;
        thread::sleep(SETTLE);
        user.click(CHAT_TEXT)?;
        user.until("the box focused", |state| main_box(state)["focused"] == true)?;
        user.edge_shots(&format!("edge-chat-font-{size}"), &["chat"])?;
    }
    user.ask("font 14")?;
    user.clear_chat_box()?;
    Ok(())
}

/// A reply with its quote, a pasted file's chip and the edit of a sent message, each in the chat's box.
fn edge_reply_file_edit(user: &mut User<'_>) -> SandboxResult {
    let reply = user.ask("reply-last")?;
    ensure(reply == "ok", || format!("no reply: {reply}"))?;
    user.until("the reply", |state| main_box(state)["mode"] == "reply")?;
    user.type_text("Yes, at noon.")?;
    user.edge_shots("edge-chat-reply", &["chat"])?;
    user.clear_chat_box()?;
    let file = user.sandbox.path("made-up-agenda.txt");
    fs::write(&file, "made-up agenda\n")?;
    user.sandbox.set_clipboard(vec![(
        "text/uri-list".to_owned(),
        format!("file://{}\r\n", file.display()).into_bytes(),
    )])?;
    user.click(CHAT_TEXT)?;
    user.press("ctrl+v")?;
    user.until("the file's chip", |state| main_box(state)["attachments"] == 1)?;
    user.edge_shots("edge-chat-attachment", &["chat"])?;
    user.clear_chat_box()?;
    let mark = user.mark();
    user.type_text(&format!("to edit {mark}"))?;
    user.press("enter")?;
    user.wait_sent("POST", &mark)?;
    user.until("the box empty", |state| text_of(main_box(state)).is_empty())?;
    thread::sleep(SETTLE);
    user.press("up")?;
    user.until("the edit", |state| main_box(state)["mode"] == "edit")?;
    user.edge_shots("edge-chat-edit", &["chat"])?;
    user.press("escape")?;
    user.until("the edit ended", |state| main_box(state)["mode"] == "new")?;
    user.clear_chat_box()?;
    Ok(())
}

/// A channel's post box and its thread's reply box side by side: the line goes with the focus from one to the other.
fn edge_focus_moves(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.ask("open-channel")?;
    user.until("the channel", |state| !state["post"].is_null())?;
    user.ask("open-thread")?;
    user.until("the thread", |state| !state["thread"].is_null())?;
    // the boxes' places once the thread's pane has narrowed the channel
    thread::sleep(SETTLE);
    let state = user.state()?;
    user.click(in_text(&state["post"])?)?;
    let state = user.until("the post box focused", |state| state["post"]["focused"] == true)?;
    ensure(state["thread"]["focused"] == false, || format!("both focused: {state}"))?;
    user.type_text("A made-up post")?;
    user.edge_shots("edge-focus-post", &["post", "thread"])?;
    user.click(in_text(&state["thread"])?)?;
    let state = user.until("the thread box focused", |state| state["thread"]["focused"] == true)?;
    ensure(state["post"]["focused"] == false, || format!("both focused: {state}"))?;
    user.type_text("A made-up reply")?;
    user.edge_shots("edge-focus-thread", &["post", "thread"])?;
    user.press("shift+tab")?;
    user.edge_shot("edge-focus-thread-shift-tab-light", &["post", "thread"])?;
    user.ask("clear")?;
    user.ask("close-thread")?;
    Ok(())
}

/// New message: the box without the focus while To: has it, then focused with a draft.
fn edge_new_chat(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.press("ctrl+n")?;
    let state = user.until("New message", |state| {
        state["main"] == "NewChat" && !state["new_chat"].is_null()
    })?;
    ensure(state["new_chat"]["focused"] == false, || {
        format!("the box has the focus, not To: {state}")
    })?;
    user.edge_shots("edge-new-chat-to", &["new_chat"])?;
    user.click(in_text(&state["new_chat"])?)?;
    user.until("the new chat's box focused", |state| {
        state["new_chat"]["focused"] == true
    })?;
    user.type_text("Lunch on Friday?")?;
    user.edge_shots("edge-new-chat-typed", &["new_chat"])?;
    user.press("ctrl+a")?;
    user.press("backspace")?;
    user.until("the new chat's box empty", |state| {
        text_of(&state["new_chat"]).is_empty()
    })?;
    user.ask(&format!("open {AB_CHAT}"))?;
    Ok(())
}

/// The main window narrowed with a draft: the stacked box keeps its edge, also at the largest font.
fn edge_narrow_window(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.type_text("A draft in a narrow window")?;
    for width in [800, 640, 560] {
        user.resize_focused(width)?;
        user.edge_shot(&format!("edge-main-{width}-light"), &["chat"])?;
    }
    user.ask("font 24")?;
    thread::sleep(SETTLE);
    user.edge_shots("edge-main-560-font-24", &["chat"])?;
    user.ask("font 14")?;
    user.resize_focused(1280)?;
    user.ask("clear")?;
    Ok(())
}

/// The call's Chat pane: typed, without the focus, a reply, the largest font.
fn edge_call_chat(user: &mut User<'_>) -> SandboxResult {
    user.call_chat()?;
    user.type_text("A note in the call")?;
    user.edge_shots("edge-call-typed", &["call"])?;
    // the call's stage, left of the pane
    user.click((400, 400))?;
    user.until("the call box without the focus", |state| {
        call_box(state)["focused"] == false
    })?;
    user.edge_shots("edge-call-unfocused", &["call"])?;
    user.ask("call-clear")?;
    let mark = user.mark();
    user.type_text(&format!("to reply to {mark}"))?;
    user.press("enter")?;
    user.wait_sent("POST", &mark)?;
    user.until("the call box empty", |state| text_of(call_box(state)).is_empty())?;
    thread::sleep(SETTLE);
    let reply = user.ask("call-reply-last")?;
    ensure(reply == "ok", || format!("no reply: {reply}"))?;
    user.until("the reply", |state| call_box(state)["mode"] == "reply")?;
    user.type_text("A reply in the call")?;
    user.edge_shots("edge-call-reply", &["call"])?;
    user.ask("call-clear")?;
    user.ask("font 24")?;
    thread::sleep(SETTLE);
    user.ask("call-focus")?;
    user.type_text("big font")?;
    user.edge_shots("edge-call-font-24", &["call"])?;
    user.ask("font 14")?;
    user.ask("call-clear")?;
    user.hang_up()
}

fn short_body(body: &str) -> String {
    let content = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value["content"].as_str().map(str::to_owned))
        .unwrap_or_else(|| body.to_owned());
    content.chars().take(160).collect()
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
    let name = display_name("bot-a@lab.example.test").to_owned();
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
        },
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
        calls: Some(CallParts::new(calling, identity, media)),
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
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let feed = VideoFeed::new();
    let engine = engine(feed.clone(), &dir)?;
    let first = support::started(Alias::A, engine)?;
    let runner = support::runner(&dir)?;
    let settings = SettingsFile::new(dir.join("settings.toml"));
    // the user shows the AI tools on the toolbar: a narrow box puts them in More options
    let opened = teams_ui::settings::Settings {
        selected_chat: Some(AB_CHAT.to_owned()),
        compose_shown: vec![
            teams_ui::settings::ComposeTool::Correct,
            teams_ui::settings::ComposeTool::Translate,
        ],
        ..teams_ui::settings::Settings::default()
    };
    settings.save(&opened).map_err(|problem| format!("{problem:?}"))?;
    let ctl = PathBuf::from(env(CTL_ENV)?);
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first: ngteams::accounts::Started {
                    calls: Some(PersonalCalls {
                        ringer: None,
                        sounds: None,
                        echo: Mri::new(ECHO),
                        video: Some(feed),
                        camera: None,
                        devices: None,
                    }),
                    ..first
                },
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
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn on_app(window: WindowHandle<KitRoot>, verb: &str, cx: &mut App) -> String {
    if let Some(call_verb) = verb.strip_prefix("call-") {
        return on_call(call_verb, cx);
    }
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
            Some(on_shell(&shell, verb, window, cx))
        })
        .ok()
        .flatten()
        .unwrap_or_else(|| "no shell".to_owned());
    if verb != "state" {
        return reply.replace('\n', " ");
    }
    // the call window's box, read outside the main window's update
    let Ok(mut state) = serde_json::from_str::<Value>(&reply) else {
        return reply;
    };
    state["call"] = call_state(cx);
    state.to_string()
}

/// The call window's stage and its window, if one is open.
fn call_stage(cx: &App) -> Option<(WindowHandle<KitRoot>, Entity<teams_ui::call_stage::CallStage>)> {
    cx.windows().into_iter().find_map(|window| {
        let kit = window.downcast::<KitRoot>()?;
        let stage = kit
            .read(cx)
            .ok()?
            .view()
            .clone()
            .downcast::<teams_ui::call_stage::CallStage>()
            .ok()?;
        Some((kit, stage))
    })
}

fn call_state(cx: &mut App) -> Value {
    let Some((kit, stage)) = call_stage(cx) else {
        return json!({"open": false});
    };
    kit.update(cx, |_, window, cx| {
        let stage = stage.read(cx);
        let chat = stage.chat_view().cloned();
        json!({
            "open": true,
            "chat_shown": stage.chat_shown(),
            "box": chat.map(|chat| box_state(chat.read(cx).compose(), window, cx)),
        })
    })
    .unwrap_or_else(|_| json!({"open": false}))
}

fn on_call(verb: &str, cx: &mut App) -> String {
    let Some((kit, stage)) = call_stage(cx) else {
        return "no call window".to_owned();
    };
    kit.update(cx, |_, window, cx| match verb {
        "show-chat" => {
            stage.update(cx, |stage, cx| stage.show_chat(true, window, cx));
            "ok".to_owned()
        }
        "open-more" => {
            let Some(chat) = stage.read(cx).chat_view().cloned() else {
                return "no chat".to_owned();
            };
            let compose = chat.read(cx).compose().clone();
            compose.update(cx, |compose, cx| compose.open_more(window, cx));
            "ok".to_owned()
        }
        "reset" => {
            let Some(chat) = stage.read(cx).chat_view().cloned() else {
                return "no chat".to_owned();
            };
            let compose = chat.read(cx).compose().clone();
            compose.update(cx, |compose, cx| {
                compose.close_more(window, cx);
                if compose.emoji_picker().is_some() {
                    compose.toggle_emoji(window, cx);
                }
                if compose.is_expanded() {
                    compose.toggle_expanded(window, cx);
                }
                compose.discard(window, cx);
                compose.focus(window, cx);
            });
            "ok".to_owned()
        }
        "clear" | "focus" => {
            let Some(chat) = stage.read(cx).chat_view().cloned() else {
                return "no chat".to_owned();
            };
            let compose = chat.read(cx).compose().clone();
            compose.update(cx, |compose, cx| {
                if verb == "clear" {
                    compose.discard(window, cx);
                }
                compose.focus(window, cx);
            });
            "ok".to_owned()
        }
        "format-more" => {
            let Some(chat) = stage.read(cx).chat_view().cloned() else {
                return "no chat".to_owned();
            };
            let bar = chat.read(cx).compose().read(cx).format_bar().clone();
            bar.update(cx, |bar, cx| {
                bar.toggle(teams_ui::editor::toolbar::Menu::More, window, cx);
            });
            "ok".to_owned()
        }
        "reply-last" => {
            let Some(chat) = stage.read(cx).chat_view().cloned() else {
                return "no chat".to_owned();
            };
            reply_to_last(&chat, window, cx)
        }
        other => format!("unknown call-{other}"),
    })
    .unwrap_or_else(|_| "no call window".to_owned())
}

/// The last message of the chat with words, quoted in its box as its Reply does.
fn reply_to_last(chat: &Entity<teams_ui::chat_view::ChatView>, window: &mut Window, cx: &mut App) -> String {
    let last = chat
        .read(cx)
        .message_items()
        .filter(|item| matches!(item.body, teams_ui::chat_view::Body::Text(_)))
        .last()
        .map(|item| item.id.clone());
    let Some(id) = last else {
        return "no message".to_owned();
    };
    chat.update(cx, |chat, cx| {
        chat.act(teams_ui::chat_view::MessageAction::Reply, &id, window, cx);
    });
    "ok".to_owned()
}

fn on_shell(shell: &Entity<Shell>, verb: &str, window: &mut Window, cx: &mut App) -> String {
    match verb {
        "state" => state_of(shell, window, cx).to_string(),
        "theme-dark" | "theme-light" => {
            let key = if verb == "theme-dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            change_settings(shell, cx, |settings| {
                settings.appearance.follow_system = false;
                settings.appearance.theme = key;
            });
            "ok".to_owned()
        }
        "show-chat" => on_call("show-chat", cx),
        "open-more" => {
            let Some(chat) = shell.read(cx).chat().cloned() else {
                return "no chat".to_owned();
            };
            let compose = chat.read(cx).compose().clone();
            compose.update(cx, |compose, cx| compose.open_more(window, cx));
            "ok".to_owned()
        }
        "open-channel" => {
            let list = shell.read(cx).list().clone();
            list.update(cx, |list, cx| list.open(TEAM_CHANNEL.to_owned(), cx));
            "ok".to_owned()
        }
        "open-thread" => {
            let Some(channel) = shell.read(cx).channel().cloned() else {
                return "no channel".to_owned();
            };
            channel.update(cx, |channel, cx| {
                channel.open_thread(CHANNEL_POST.to_owned(), window, cx);
            });
            "ok".to_owned()
        }
        "reply-last" | "format-menu" => on_chat(shell, verb, window, cx),
        "close-thread" => {
            if let Some(channel) = shell.read(cx).channel().cloned() {
                channel.update(cx, teams_ui::ChannelView::close_thread);
            }
            "ok".to_owned()
        }
        "join-meeting" => {
            shell.update(cx, |shell, cx| {
                shell.join_meeting_chat(MEETING_CHAT.to_owned(), None, None, cx);
            });
            "ok".to_owned()
        }
        "clear" => {
            for compose in boxes(shell, cx) {
                compose.update(cx, |compose, cx| compose.discard(window, cx));
            }
            "ok".to_owned()
        }
        other => {
            if let Some(id) = other.strip_prefix("open ") {
                shell.update(cx, |shell, cx| shell.open_chat(id.to_owned(), window, cx));
                "ok".to_owned()
            } else if let Some(language) = other.strip_prefix("language ") {
                let language = language.to_owned();
                change_settings(shell, cx, |settings| settings.draft_translate_to = language);
                "ok".to_owned()
            } else if let Some(size) = other.strip_prefix("font ") {
                let Ok(size) = size.parse::<u16>() else {
                    return format!("bad size {size}");
                };
                change_settings(shell, cx, |settings| settings.appearance.font_size = size);
                "ok".to_owned()
            } else {
                format!("unknown {other}")
            }
        }
    }
}

/// The main window's chat: its last message quoted in a reply, or the format bar's colour menu opened or closed.
fn on_chat(shell: &Entity<Shell>, verb: &str, window: &mut Window, cx: &mut App) -> String {
    let Some(chat) = shell.read(cx).chat().cloned() else {
        return "no chat".to_owned();
    };
    if verb == "reply-last" {
        return reply_to_last(&chat, window, cx);
    }
    let bar = chat.read(cx).compose().read(cx).format_bar().clone();
    bar.update(cx, |bar, cx| {
        bar.toggle(teams_ui::editor::toolbar::Menu::FontColour, window, cx);
    });
    "ok".to_owned()
}

fn change_settings(shell: &Entity<Shell>, cx: &mut App, change: impl FnOnce(&mut teams_ui::settings::Settings)) {
    let model = shell.read(cx).settings().clone();
    model.update(cx, |model, cx| model.change(change, cx));
}

/// Every box the main window shows now.
fn boxes(shell: &Entity<Shell>, cx: &App) -> Vec<Entity<Compose>> {
    let shell_ref = shell.read(cx);
    let mut all = Vec::new();
    if let Some(chat) = shell_ref.chat() {
        all.push(chat.read(cx).compose().clone());
    }
    if let Some(channel) = shell_ref.channel() {
        let channel = channel.read(cx);
        all.push(channel.posts().read(cx).compose().clone());
        if let Some((_, thread)) = channel.thread() {
            all.push(thread.read(cx).compose().clone());
        }
    }
    all
}

fn state_of(shell: &Entity<Shell>, window: &Window, cx: &App) -> Value {
    let shell_ref = shell.read(cx);
    let main = match shell_ref.main() {
        Main::Chat(id) => format!("chat:{id}"),
        Main::Channel(id) => format!("channel:{id}"),
        other => format!("{other:?}"),
    };
    let chat = shell_ref
        .chat()
        .filter(|_| matches!(shell_ref.main(), Main::Chat(_)))
        .map(|chat| box_state(chat.read(cx).compose(), window, cx));
    let (post, thread) = shell_ref
        .channel()
        .filter(|_| matches!(shell_ref.main(), Main::Channel(_)))
        .map_or((None, None), |channel| {
            let channel = channel.read(cx);
            (
                Some(box_state(channel.posts().read(cx).compose(), window, cx)),
                channel
                    .thread()
                    .map(|(_, thread)| box_state(thread.read(cx).compose(), window, cx)),
            )
        });
    let new_chat = shell_ref
        .new_chat()
        .filter(|_| *shell_ref.main() == Main::NewChat)
        .map(|view| box_state(&view.read(cx).compose(cx), window, cx));
    let calls = shell_ref.models().calls.read(cx);
    json!({
        "main": main,
        "chat": chat,
        "new_chat": new_chat,
        "post": post,
        "thread": thread,
        "prejoin": calls.prejoin().is_some(),
        "language": shell_ref.settings().read(cx).settings().draft_translate_to,
    })
}

/// One box: its text, mode, focus, its layout and its menus.
fn box_state(compose: &Entity<Compose>, window: &Window, cx: &App) -> Value {
    let compose = compose.read(cx);
    let mode = match compose.mode() {
        Mode::New => "new",
        Mode::Reply(_) => "reply",
        Mode::Edit { .. } => "edit",
    };
    json!({
        "text": compose.value(cx).to_string(),
        "mode": mode,
        "focused": compose.focused(window, cx),
        "stacked": compose.is_stacked(cx),
        "narrow": compose.is_narrow(),
        "more_open": compose.more_open(),
        "expanded": compose.is_expanded(),
        "format_in_more": compose.format_bar().read(cx).in_more_options(),
        "emoji_open": compose.emoji_picker().is_some(),
        "language_open": compose.language_menu().is_some_and(|menu| menu.read(cx).is_open()),
        "correction": format!("{:?}", compose.correction_status()),
        "translation": format!("{:?}", compose.draft_translation_status()),
        "attachments": compose.attachments().len(),
        "problem": compose.problem().map(ToString::to_string),
        "bounds": compose.box_bounds().map(|bounds| {
            [bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height].map(f32::from)
        }),
        "edge": edge_line(compose, window, cx),
    })
}

/// The box as the tour's edge check reads it: where it is inside its border, the scale, the rem, its focus, the brand.
fn edge_line(compose: &Compose, window: &Window, cx: &App) -> String {
    let Some(bounds) = compose.box_bounds() else {
        return format!("{}none", compose_edge::PREFIX);
    };
    format!(
        "{}{} {} {} {} {} {} {} {:06x}",
        compose_edge::PREFIX,
        f32::from(bounds.origin.x),
        f32::from(bounds.origin.y),
        f32::from(bounds.size.width),
        f32::from(bounds.size.height),
        window.scale_factor(),
        f32::from(window.rem_size()),
        compose.focused(window, cx),
        teams_ui::theme::current(cx).brand,
    )
}
