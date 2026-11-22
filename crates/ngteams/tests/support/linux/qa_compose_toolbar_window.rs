//! QA of tasks 584 and 700, in the UI sandbox: the message box's toolbar with no AI button (the AI tools are rows of
//! "+" until the user shows them), and the buttons that the user can show or hide. The real app runs as a child in the sandbox, as lab account a on `teams-mock` (its chat service, its
//! calling service, its Trouter and its group video). The parent drives it with the sandbox's virtual pointer and
//! keyboard. It asks the child what the boxes hold through a file in the sandbox dir (`harness = false`: GPUI runs on
//! the process's main thread). The app tells no positions: a button is clicked where a user sees it on the screen.
//!
//! - **The bar and the AI rows of "+"**: Format, Emoji, Attach, Loop, "+" and Send. The AI tools' rows of "+"
//!   (Correct, Translate, the language) by the pointer and the keys, with a made-up `claude` on the app's `PATH`;
//!   Ctrl+Shift+G and Ctrl+Shift+Y; the language stays saved. Correct and Translate shown on the bar from the list, and
//!   Translate's chevron opens the language menu.
//! - **The customize list**: a right click on the toolbar, and "Customize toolbar" in "+". Each click hides or shows a
//!   button (the AI tools too); Send and "+" do not hide. A hidden button's action is in "+" and works from there.
//! - **Settings** > Appearance > "Message box toolbar": its switches and the box's list agree, both ways.
//! - **Every box**: a channel post, a thread reply, New message, edit mode, the call's Chat pane, a narrow window, a
//!   large font, Teams Dark.
//! - **"Use AI features"** off and on again.
//! - **A restart**: a second run of the app over the same settings file keeps the hidden buttons.
//!
//! `QA584_ONLY` (a comma list of check names) runs only those. `QA584_EVIDENCE` names a directory that gets the
//! screenshots and the results too. Screenshots also go to `target/ui-shots/qa584-*.png`. Every name, id and text is
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
use teams_ui::compose::{Compose, MenuKind, Mode};
use teams_ui::settings::ComposeTool;
use teams_ui::settings_view::Page;
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

const CHILD_ENV: &str = "QA584_CHILD";
const ONLY_ENV: &str = "QA584_ONLY";
const EVIDENCE_ENV: &str = "QA584_EVIDENCE";
const TRACE_FD_ENV: &str = "QA584_TRACE_FD";
const ORIGIN_ENV: &str = "QA584_ORIGIN";
const TROUTER_ENV: &str = "QA584_TROUTER";
const RELAY_ENV: &str = "QA584_RELAY";
const IC3_ENV: &str = "QA584_IC3";
const SKYPE_ENV: &str = "QA584_SKYPE";
const DIR_ENV: &str = "QA584_DIR";
const CTL_ENV: &str = "QA584_CTL";
/// The file in the sandbox dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "qa584-ask";
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
/// The mock's team channel's post with replies under it.
const CHANNEL_POST: &str = "1760000300000";

/// A made-up Claude Code (`__STATE__` its state directory) for Correct and Translate. Translate (a question with a
/// `<language>`) gives the draft back with "hello" in German, or with `[<language>] ` inside its first marker for
/// another language;
/// Correct gives it back with "teh" as "the". A chat's batch (`<translate>`) gives each message back with
/// `[<language>] ` before it, the language that its instructions read the chat in. It waits while `hold` exists.
const FAKE_CLAUDE: &str = r#"#!/bin/bash
state="__STATE__"
case "$1" in
  --help)
    printf '%s\n' "Usage: claude [options]" "  --model <model>  Model for the current session: an alias such as 'sonnet' or 'opus'"
    exit 0;;
esac
reading=$(printf '%s\n' "$@" | sed -n 's/.*who reads and writes it in \([A-Za-z]*\)\..*/\1/p')
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$state/received.log"
  while [ -e "$state/hold" ]; do sleep 0.02; done
  t=$(printf '%s' "$line" | sed 's/.*<message>\\n\(.*\)\\n<\/message>.*/\1/')
  case "$line" in
    *'<translate>'*)
      ans=""
      inner=$(printf '%s' "$line" | sed 's/.*<translate>\\n//; s/\\n<\/translate>.*//')
      while IFS= read -r m; do
        [ -z "$m" ] && continue
        m=$(printf '%s' "$m" | sed 's/^\(<m id=\\"[0-9]*\\"\)[^>]*>\(<x[0-9]*>\)\{0,1\}/\1 lang=\\"en\\">\2['"$reading"'] /')
        ans="$ans$m\\n"
      done < <(printf '%s\n' "$inner" | sed 's/\\n/\n/g');;
    *"<language>"*)
      lang=$(printf '%s' "$line" | sed 's/.*<language>\([^<]*\)<\/language>.*/\1/')
      if [ "$lang" = German ]; then t=$(printf '%s' "$t" | sed 's/hello/hallo/g'); else t=$(printf '%s' "$t" | sed 's/^\(<x[0-9]*>\)\{0,1\}/&['"$lang"'] /'); fi
      ans="<translated>$t</translated>";;
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
            eprintln!("qa_compose_toolbar_window: {error}");
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

/// The first run of the app: a new settings file, the AI features on.
const FIRST: [Check; 23] = [
    ("bar_and_ai_in_plus", bar_and_ai_in_plus),
    ("ai_rows_act", ai_rows_act),
    ("ai_keys_and_language", ai_keys_and_language),
    ("ai_tools_on_the_bar", ai_tools_on_the_bar),
    ("right_click_list", right_click_list),
    ("list_down_selects_format", list_down_selects_format),
    ("send_and_plus_stay", send_and_plus_stay),
    ("hidden_in_plus", hidden_in_plus),
    ("customize_from_plus", customize_from_plus),
    ("settings_both_ways", settings_both_ways),
    ("every_box", every_box),
    ("call_chat_box", call_chat_box),
    ("narrow_font_dark", narrow_font_dark),
    ("ai_off_and_on", ai_off_and_on),
    ("font_key_with_menu_open", font_key_with_menu_open),
    qa700::FIRST[0],
    qa700::FIRST[1],
    qa700::FIRST[2],
    qa700::FIRST[3],
    qa700::FIRST[4],
    qa700::FIRST[5],
    qa700::FIRST[6],
    ("hide_for_restart", hide_for_restart),
];

/// The second run of the app over the same settings file.
const RESTART: [Check; 3] = [
    qa700::RESTART_FIRST,
    ("kept_after_restart", kept_after_restart),
    qa700::RESTART_LAST,
];

#[path = "qa_compose_toolbar_qa700.rs"]
mod qa700;

#[derive(Default)]
struct Tally {
    passed: Vec<String>,
    failed: Vec<(String, String)>,
}

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_compose_toolbar_window", &options)? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mock = runtime.block_on(ChatServiceMock::start())?;
    let trouter = mock.start_trouter()?;
    let dir = tempfile::tempdir()?;
    let relay = mock.start_group_video(dir.path(), PEER_TONE)?;
    let tool = FakeTool::install(&sandbox)?;
    let mut tally = Tally::default();
    let mut quits = Vec::new();
    for (run, checks) in [
        ("first", &FIRST[..]),
        ("restart", &RESTART[..]),
        ("old-file", &qa700::OLD_FILE[..]),
    ] {
        if !checks.iter().any(|(name, _)| wanted(name)) {
            continue;
        }
        if run == "old-file" {
            qa700::write_old_file(&dir.path().join("a/settings.toml"))?;
        }
        say(&format!("run {run}"));
        let app = AppUnderTest::start(&sandbox, &mock, &relay.to_string(), trouter.origin(), dir.path(), &tool)?;
        let mut user = User::new(&sandbox, &mock, &runtime, app, &tool)?;
        for (name, check) in checks {
            if wanted(name) {
                user.run_check(name, *check, &mut tally.passed, &mut tally.failed);
            }
        }
        let _ = user.hang_up();
        quits.push(user.quit());
        if let Some(evidence) = std::env::var_os(EVIDENCE_ENV) {
            let _ = fs::copy(
                dir.path().join("a/settings.toml"),
                PathBuf::from(&evidence).join(format!("settings-after-{run}.toml")),
            );
            let _ = fs::copy(
                tool.state.join("received.log"),
                PathBuf::from(evidence).join(format!("ai-tool-received-{run}.log")),
            );
        }
    }
    drop(trouter);
    let mut summary = String::new();
    for name in &tally.passed {
        let _ = writeln!(summary, "pass {name}");
    }
    for (name, why) in &tally.failed {
        let _ = writeln!(summary, "FAIL {name}: {why}");
    }
    say(&summary);
    if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
        let _ = fs::write(PathBuf::from(dir).join("results.txt"), &summary);
    }
    say(&format!(
        "qa_compose_toolbar_window: {} passed, {} failed",
        tally.passed.len(),
        tally.failed.len()
    ));
    for quit in quits {
        quit?;
    }
    ensure(tally.failed.is_empty(), || {
        format!("{} checks failed", tally.failed.len())
    })
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
    /// The settings file, the same for both runs.
    settings: PathBuf,
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
            settings: own.join("settings.toml"),
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

    /// A screenshot as `qa584-<name>.png`, under `target/ui-shots/` and the evidence dir.
    fn shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        let file = format!("qa584-{name}.png");
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

// --- where things are, 1280x800 ---

/// The call toolbar's Chat.
const CALL_CHAT: (i32, i32) = (708, 27);
/// The text field of the call's Chat pane, and the row of its stacked box, its tools from the left 31 px apart.
const CALL_TEXT: (i32, i32) = (1090, 714);
const CALL_ROW_Y: i32 = 749;
/// A place on the stacked row with no button: between the last tool and the divider.
const CALL_ROW_GAP: (i32, i32) = (1170, CALL_ROW_Y);
/// The text of the main window's chat box at 1280 px; its tools sit on the text's line, before the divider and Send.
const MAIN_TEXT: (i32, i32) = (700, 750);
const MAIN_ROW_Y: i32 = 749;
const MAIN_SEND: (i32, i32) = (1236, MAIN_ROW_Y);
/// The rightmost tool of the main box ("+" while no AI tool shows) at 1280 px.
const MAIN_LAST_TOOL_X: i32 = 1194;
const TOOL_STEP: i32 = 31;

/// The main box's tool `from_right` places left of the rightmost one, at 1280 px.
fn main_tool(from_right: i32) -> (i32, i32) {
    (MAIN_LAST_TOOL_X - TOOL_STEP * from_right, MAIN_ROW_Y)
}

/// The switches of "Message box toolbar" on the Appearance page, scrolled to its foot.
const SETTINGS_FORMAT: (i32, i32) = (1135, 411);
const SETTINGS_EMOJI: (i32, i32) = (1135, 475);

fn main_box(state: &Value) -> &Value {
    &state["chat"]
}

fn hidden(state: &Value) -> Vec<String> {
    state["hidden"]
        .as_array()
        .map(|names| names.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

fn hidden_is(state: &Value, names: &[&str]) -> bool {
    hidden(state) == names
}

/// The AI tools that the toolbar shows.
fn shown(state: &Value) -> Vec<String> {
    state["shown"]
        .as_array()
        .map(|names| names.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

fn shown_is(state: &Value, names: &[&str]) -> bool {
    shown(state) == names
}

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
}

impl User<'_> {
    /// The main window at 1280 px, the a–b chat open, its box empty with the keyboard, Teams Light, font 14.
    fn main_chat(&mut self) -> SandboxResult<Value> {
        self.focus_window(self.main_window)?;
        self.resize_focused(1280)?;
        self.ask("theme-light")?;
        self.ask("font 14")?;
        self.ask(&format!("open {AB_CHAT}"))?;
        self.ask("clear")?;
        self.until("the chat", |state| !main_box(state).is_null())?;
        self.press("escape")?;
        self.click(MAIN_TEXT)?;
        self.until("the chat box empty and focused", |state| {
            text_of(main_box(state)).is_empty() && main_box(state)["focused"] == true
        })
    }

    /// Teams' buttons in `names` hidden, the others shown, by the setting (as the customize list sets it), and waits
    /// for it.
    fn hide(&mut self, names: &[&str]) -> SandboxResult<Value> {
        self.ask(&format!("hide {}", names.join(",")))?;
        self.until("the hidden buttons", |state| hidden_is(state, names))
    }

    /// The AI tools in `names` shown on the bar, the others in "+", by the setting, and waits for it.
    fn show_ai(&mut self, names: &[&str]) -> SandboxResult<Value> {
        self.ask(&format!("show-ai {}", names.join(",")))?;
        self.until("the shown AI tools", |state| shown_is(state, names))
    }

    /// Opens the menu of the main box's tool `from_right` by a click; waits for the menu `kind`.
    fn click_menu(&mut self, from_right: i32, kind: &str) -> SandboxResult<Value> {
        self.click(main_tool(from_right))?;
        self.until(&format!("the {kind} menu"), |state| main_box(state)["menu"] == kind)
    }

    fn right_click(&mut self, (x, y): (i32, i32)) -> SandboxResult {
        self.input
            .click(u32::try_from(x)?, u32::try_from(y)?, keys::BTN_RIGHT)?;
        thread::sleep(STEP);
        Ok(())
    }

    /// The settings file as the app wrote it.
    fn settings_text(&self) -> String {
        fs::read_to_string(&self.app.settings).unwrap_or_default()
    }

    /// Takes the customize list's row `up` places from its foot by the keys: Up from no row selects the last row that
    /// can be taken (Translate, or Loop components while AI is off), and each Up the row over it.
    fn take_row_by_up(&mut self, up: usize) -> SandboxResult {
        // the pointer off the list: a row under it is selected by its hover
        self.park()?;
        thread::sleep(STEP);
        for _ in 0..up {
            self.press("up")?;
        }
        self.press("enter")
    }

    /// A screenshot in Teams Light and Teams Dark of what `open` opens; it is closed before each theme change, as no
    /// user changes the theme with a menu open.
    fn reopened_shots(&mut self, name: &str, key: &str, open: fn(&mut User<'_>) -> SandboxResult) -> SandboxResult {
        for theme in ["light", "dark"] {
            self.ask(&format!("theme-{theme}"))?;
            thread::sleep(SETTLE);
            open(self)?;
            self.settled_shot(&format!("{name}-{theme}"))?;
            self.close_menus(key)?;
        }
        self.ask("theme-light")?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// Escape until no menu of the main box is open, the keyboard back in its text.
    fn close_menus(&mut self, key: &str) -> SandboxResult<Value> {
        for _ in 0..3 {
            let state = self.state()?;
            let boxed = &state[key];
            if boxed["menu"].is_null() && boxed["customize"] == false && boxed["language_open"] == false {
                return Ok(state);
            }
            self.press("escape")?;
        }
        self.until("the menus closed", |state| {
            state[key]["menu"].is_null() && state[key]["customize"] == false
        })
    }
}

/// The bar: Format, Emoji, Attach, Loop, "+" and Send, no AI button; "+" holds the AI tools' rows; Escape.
fn bar_and_ai_in_plus(user: &mut User<'_>) -> SandboxResult {
    let state = user.main_chat()?;
    ensure(
        hidden(&state).is_empty() && shown(&state).is_empty() && state["ai"] == true,
        || format!("not the first run's settings: {state}"),
    )?;
    ensure(main_box(&state)["stacked"] == false, || {
        format!("stacked at 1280: {state}")
    })?;
    user.type_text("a draft for the bar")?;
    user.shots_in_both_themes("bar")?;
    // the tooltip of "+", the rightmost tool
    user.input
        .move_to(u32::try_from(main_tool(0).0)?, u32::try_from(MAIN_ROW_Y)?)?;
    thread::sleep(Duration::from_millis(1500));
    user.shot("plus-tooltip")?;
    user.park()?;
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        let state = user.click_menu(0, "Plus")?;
        ensure(main_box(&state)["focused"] == false, || {
            format!("the text keeps the keyboard with \"+\" open: {state}")
        })?;
        user.settled_shot(&format!("plus-ai-rows-{theme}"))?;
        user.press("escape")?;
        let state = user.until("\"+\" closed", |state| main_box(state)["menu"].is_null())?;
        ensure(main_box(&state)["focused"] == true, || {
            format!("Escape did not give the keyboard back: {state}")
        })?;
    }
    user.ask("theme-light")?;
    // a second click on the button closes its menu
    user.click_menu(0, "Plus")?;
    user.click(main_tool(0))?;
    user.until("\"+\" closed by its button", |state| main_box(state)["menu"].is_null())?;
    user.type_text(" ok")?;
    user.until("the keyboard after the menu", |state| {
        text_of(main_box(state)) == "a draft for the bar ok"
    })?;
    user.ask("clear")?;
    Ok(())
}

/// The AI rows of "+" by the keys: Correct, its "Stop correcting", the proposal taken, the language, Translate.
fn ai_rows_act(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.ask("language English")?;
    user.type_text("hello teh team")?;
    user.click_menu(0, "Plus")?;
    user.tool.hold(true)?;
    user.press("down")?;
    user.settled_shot("plus-first-row")?;
    user.press("enter")?;
    let state = user.until("Correct by its row", |state| main_box(state)["correction"] == "Running")?;
    ensure(main_box(&state)["menu"].is_null(), || {
        format!("the menu stays open: {state}")
    })?;
    user.click_menu(0, "Plus")?;
    user.settled_shot("plus-stop-correcting")?;
    user.press("escape")?;
    user.tool.hold(false)?;
    user.until("the correction", |state| main_box(state)["correction"] == "Proposed")?;
    user.settled_shot("ai-correction-proposed")?;
    user.click_menu(0, "Plus")?;
    user.press("down")?;
    user.press("enter")?;
    user.press("enter")?;
    user.until("the correction taken", |state| {
        text_of(main_box(state)) == "hello the team"
    })?;
    // Up from no row: Customize toolbar; Up again: the language row
    user.click_menu(0, "Plus")?;
    user.press("up")?;
    user.press("up")?;
    user.settled_shot("plus-language-row")?;
    user.press("enter")?;
    let state = user.until("the language menu", |state| main_box(state)["language_open"] == true)?;
    ensure(main_box(&state)["menu"].is_null(), || {
        format!("\"+\" stays open: {state}")
    })?;
    user.shots_in_both_themes("ai-language-menu")?;
    user.type_text("German")?;
    user.press("enter")?;
    user.until("German chosen", |state| state["language"] == "German")?;
    let state = user.state()?;
    ensure(state["reading"] == "English", || {
        format!("the language messages are read in changed: {state}")
    })?;
    user.press("escape")?;
    user.close_menus("chat")?;
    user.click_menu(0, "Plus")?;
    user.settled_shot("plus-german")?;
    user.press("down")?;
    user.press("down")?;
    user.press("enter")?;
    let state = user.until("the translation", |state| text_of(main_box(state)) == "hallo the team")?;
    ensure(main_box(&state)["focused"] == true, || {
        format!("the box has no keyboard: {state}")
    })?;
    user.ask("clear")?;
    Ok(())
}

/// Ctrl+Shift+G and Ctrl+Shift+Y as before; the language is in the settings file.
fn ai_keys_and_language(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    let text = user.settings_text();
    ensure(text.contains("draft_translate_to = \"German\""), || {
        format!("the language is not saved: {text}")
    })?;
    ensure(text.contains("message_translate_to = \"English\""), || {
        format!("the language messages are read in changed: {text}")
    })?;
    user.type_text("hello teh keys")?;
    user.tool.hold(true)?;
    user.press("ctrl+shift+g")?;
    user.until("Correct by Ctrl+Shift+G", |state| {
        main_box(state)["correction"] == "Running"
    })?;
    user.press("ctrl+shift+g")?;
    user.until("the correction stopped by its key", |state| {
        main_box(state)["correction"] != "Running"
    })?;
    user.press("ctrl+shift+y")?;
    user.until("Translate by Ctrl+Shift+Y", |state| {
        main_box(state)["translation"] == "Running"
    })?;
    user.click_menu(0, "Plus")?;
    user.settled_shot("plus-stop-translating")?;
    user.press("escape")?;
    user.press("ctrl+shift+y")?;
    user.until("the translation stopped by its key", |state| {
        main_box(state)["translation"] != "Running"
    })?;
    user.tool.hold(false)?;
    user.press("ctrl+shift+y")?;
    user.until("the translation by its key", |state| {
        text_of(main_box(state)) == "hallo teh keys"
    })?;
    user.ask("clear")?;
    Ok(())
}

/// Correct and Translate shown from the list: their buttons sit after "+", Translate with its chevron, which opens the
/// language menu; "+" has no AI rows then. Back into "+" from the list.
fn ai_tools_on_the_bar(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.type_text("hello teh bar")?;
    open_list_on_attach(user)?;
    user.settled_shot("customize-ai-in-plus")?;
    user.click((LIST_X, LIST_CORRECT_Y))?;
    user.until("Correct shown by its row", |state| shown_is(state, &["correct"]))?;
    user.click((LIST_X, LIST_TRANSLATE_Y))?;
    user.until("Translate shown by its row", |state| {
        shown_is(state, &["correct", "translate"])
    })?;
    user.settled_shot("customize-ai-shown")?;
    user.close_menus("chat")?;
    user.shots_in_both_themes("bar-ai-shown")?;
    // the chevron is the rightmost tool now: it opens the language menu
    user.click(MAIN_CHEVRON)?;
    user.until("the language menu by the chevron", |state| {
        main_box(state)["language_open"] == true
    })?;
    user.shots_in_both_themes("bar-ai-language")?;
    user.press("escape")?;
    user.close_menus("chat")?;
    // Correct's button, then Translate's
    user.click(MAIN_CORRECT)?;
    user.until("Correct by its button", |state| {
        main_box(state)["correction"] == "Proposed"
    })?;
    // Translate drops the proposal and translates the draft as written
    user.click(MAIN_TRANSLATE)?;
    user.until("Translate by its button", |state| {
        text_of(main_box(state)) == "hallo teh bar"
    })?;
    // "+" holds no AI row now: Down from no row is Customize toolbar
    user.click((MAIN_TRANSLATE.0 - 2 * TOOL_STEP, MAIN_ROW_Y))?;
    user.until("\"+\" with the AI tools shown", |state| {
        main_box(state)["menu"] == "Plus"
    })?;
    user.settled_shot("plus-ai-shown")?;
    user.press("down")?;
    user.press("enter")?;
    user.until("the list from \"+\"", |state| main_box(state)["customize"] == true)?;
    user.close_menus("chat")?;
    user.show_ai(&[])?;
    user.ask("clear")?;
    Ok(())
}

/// The main box's Translate, its chevron and Correct while both show at 1280 px: the chevron is the rightmost tool, 16
/// px wide; Translate is just left of it, Correct a tool's step further.
const MAIN_CHEVRON: (i32, i32) = (MAIN_LAST_TOOL_X + 6, MAIN_ROW_Y);
const MAIN_TRANSLATE: (i32, i32) = (MAIN_LAST_TOOL_X - 16, MAIN_ROW_Y);
const MAIN_CORRECT: (i32, i32) = (MAIN_LAST_TOOL_X - 16 - TOOL_STEP, MAIN_ROW_Y);

/// Where the customize list's rows are after a right click on the main box's Attach at 1280 px: the list opens over the
/// toolbar's buttons and to the right of the pointer, moved left to stay in the window.
const LIST_X: i32 = 1110;
const LIST_EMOJI_Y: i32 = 548;
const LIST_CORRECT_Y: i32 = 632;
const LIST_TRANSLATE_Y: i32 = 660;
const LIST_PLUS_Y: i32 = 688;
const LIST_SEND_Y: i32 = 716;

fn open_list_on_attach(user: &mut User<'_>) -> SandboxResult {
    user.right_click(main_tool(2))?;
    user.until("the list by a right click", |state| {
        main_box(state)["customize"] == true
    })?;
    Ok(())
}

/// A right click on the toolbar opens the list; a click and the keys hide and show a button; the list stays open;
/// Send and "+" do nothing; Escape.
fn right_click_list(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.type_text("a draft kept")?;
    user.reopened_shots("customize", "chat", open_list_on_attach)?;
    open_list_on_attach(user)?;
    let state = user.state()?;
    ensure(hidden(&state).is_empty(), || {
        format!("a right click hid a button: {state}")
    })?;
    // the pointer: Emoji's row hides it, the list stays open where it was
    user.click((LIST_X, LIST_EMOJI_Y))?;
    let state = user.until("Emoji hidden by a click on its row", |state| {
        hidden_is(state, &["emoji"])
    })?;
    ensure(main_box(&state)["customize"] == true, || {
        format!("the list closed: {state}")
    })?;
    user.settled_shot("customize-emoji-clicked")?;
    // "+" and Send: a click changes nothing
    user.click((LIST_X, LIST_PLUS_Y))?;
    user.click((LIST_X, LIST_SEND_Y))?;
    thread::sleep(SETTLE);
    let state = user.state()?;
    ensure(hidden_is(&state, &["emoji"]), || {
        format!("a click on + or Send hid a button: {state}")
    })?;
    ensure(text_of(main_box(&state)) == "a draft kept", || {
        format!("Send sent the draft: {state}")
    })?;
    user.settled_shot("customize-plus-send-clicked")?;
    user.close_menus("chat")?;
    open_list_on_attach(user)?;
    user.click((LIST_X, LIST_EMOJI_Y))?;
    user.until("Emoji shown again by a click", |state| hidden(state).is_empty())?;
    // the keys: Format's row, six rows over the foot's Translate
    user.take_row_by_up(6)?;
    let state = user.until("Format hidden by its row", |state| hidden_is(state, &["format"]))?;
    ensure(main_box(&state)["customize"] == true, || {
        format!("the list closed: {state}")
    })?;
    user.settled_shot("customize-format-off")?;
    user.take_row_by_up(6)?;
    user.until("Format shown again by its row", |state| hidden(state).is_empty())?;
    user.press("escape")?;
    let state = user.until("the list closed", |state| main_box(state)["customize"] == false)?;
    ensure(main_box(&state)["focused"] == true, || {
        format!("Escape did not give the keyboard back: {state}")
    })?;
    ensure(text_of(main_box(&state)) == "a draft kept", || {
        format!("the draft changed: {state}")
    })?;
    // a right click on Send opens the list too, and sends nothing
    user.right_click(MAIN_SEND)?;
    user.until("the list by a right click on Send", |state| {
        main_box(state)["customize"] == true
    })?;
    user.press("escape")?;
    let state = user.until("the list closed", |state| main_box(state)["customize"] == false)?;
    ensure(text_of(main_box(&state)) == "a draft kept", || {
        format!("a right click on Send sent or changed the draft: {state}")
    })?;
    user.hide(&[])?;
    user.ask("clear")?;
    Ok(())
}

/// The keys in the customize list as in any menu: Down from no row selects the first row, Format, and Enter takes it;
/// Down from the last row that can be taken goes round to Format.
fn list_down_selects_format(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.hide(&[])?;
    open_list_on_attach(user)?;
    user.park()?;
    user.press("down")?;
    user.settled_shot("customize-first-down")?;
    user.press("enter")?;
    let first = user.until("Format hidden by Down, Enter", |state| hidden_is(state, &["format"]));
    user.hide(&[])?;
    user.close_menus("chat")?;
    open_list_on_attach(user)?;
    user.park()?;
    // Up selects Translate, the last row; Down from it goes round
    user.press("up")?;
    user.press("down")?;
    user.settled_shot("customize-down-from-last")?;
    user.press("enter")?;
    let round = user.until("Format hidden by Up, Down, Enter", |state| {
        hidden_is(state, &["format"])
    });
    user.hide(&[])?;
    user.close_menus("chat")?;
    first?;
    round.map(|_| ())
}

/// Send and "+" cannot hide: the keys pass over their rows; Up from no row is Translate, the last that can be taken.
fn send_and_plus_stay(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.right_click(main_tool(1))?;
    user.until("the list", |state| main_box(state)["customize"] == true)?;
    user.press("up")?;
    user.settled_shot("customize-up-last")?;
    user.press("enter")?;
    let state = user.until("a button shown by the last row", |state| !shown(state).is_empty())?;
    ensure(shown_is(&state, &["translate"]) && hidden(&state).is_empty(), || {
        format!("the last row that can be taken is not Translate: {state}")
    })?;
    user.settled_shot("customize-translate-on")?;
    user.press("escape")?;
    user.close_menus("chat")?;
    user.show_ai(&[])?;
    let state = user.hide(&["format", "emoji", "attach", "loop"])?;
    ensure(main_box(&state)["stacked"] == false, || format!("stacked: {state}"))?;
    user.shots_in_both_themes("all-hidden")?;
    // "+" is now the rightmost tool
    user.click_menu(0, "Plus")?;
    user.shots_in_both_themes("all-hidden-plus")?;
    user.press("escape")?;
    user.close_menus("chat")?;
    user.hide(&[])?;
    Ok(())
}

/// A hidden button's action is in "+", with its icon and key, and works from there; its key works too.
fn hidden_in_plus(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.hide(&["format", "emoji"])?;
    user.shots_in_both_themes("hidden-format-emoji")?;
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        user.click_menu(0, "Plus")?;
        user.settled_shot(&format!("plus-format-emoji-{theme}"))?;
        user.press("escape")?;
        user.close_menus("chat")?;
    }
    user.ask("theme-light")?;
    // Down, Enter: Format's row shows the formatting options
    user.click_menu(0, "Plus")?;
    user.press("down")?;
    user.press("enter")?;
    user.until("formatting by its row in +", |state| {
        main_box(state)["expanded"] == true
    })?;
    user.settled_shot("plus-format-taken")?;
    user.press("ctrl+shift+x")?;
    user.until("formatting off by its key", |state| {
        main_box(state)["expanded"] == false
    })?;
    // Down, Down, Enter: Emoji's row opens the picker
    user.close_menus("chat")?;
    user.click_menu(0, "Plus")?;
    user.press("down")?;
    user.press("down")?;
    user.press("enter")?;
    user.until("the emoji picker by its row in +", |state| {
        main_box(state)["emoji_open"] == true
    })?;
    user.settled_shot("plus-emoji-taken")?;
    user.press("escape")?;
    let state = user.until("the picker closed", |state| main_box(state)["emoji_open"] == false)?;
    ensure(main_box(&state)["focused"] == true, || {
        format!("no keyboard after the picker: {state}")
    })?;
    user.press("ctrl+.")?;
    user.until("Emoji by its key while hidden", |state| {
        main_box(state)["emoji_open"] == true
    })?;
    user.press("ctrl+.")?;
    user.until("Emoji closed by its key", |state| {
        main_box(state)["emoji_open"] == false
    })?;
    ai_rows_in_plus(user)
}

/// The AI tools' three rows in "+" under Teams' own hidden ones work.
fn ai_rows_in_plus(user: &mut User<'_>) -> SandboxResult {
    user.hide(&["format", "emoji"])?;
    user.type_text("hello teh plus")?;
    user.click_menu(0, "Plus")?;
    user.shots_in_both_themes("plus-ai-hidden")?;
    user.press("down")?;
    user.press("down")?;
    user.press("down")?;
    user.press("enter")?;
    user.until("Correct by its row in +", |state| {
        main_box(state)["correction"] == "Proposed"
    })?;
    user.click_menu(0, "Plus")?;
    user.press("down")?;
    user.press("down")?;
    user.press("down")?;
    user.press("enter")?;
    user.press("enter")?;
    user.until("the correction taken", |state| {
        text_of(main_box(state)) == "hello the plus"
    })?;
    user.click_menu(0, "Plus")?;
    for _ in 0..5 {
        user.press("down")?;
    }
    user.press("enter")?;
    user.until("the language menu by its row in +", |state| {
        main_box(state)["language_open"] == true
    })?;
    user.settled_shot("plus-language-menu")?;
    user.press("escape")?;
    user.close_menus("chat")?;
    user.press("ctrl+shift+y")?;
    user.until("Translate by its key while hidden", |state| {
        text_of(main_box(state)) == "hallo the plus"
    })?;
    user.ask("clear")?;
    user.hide(&[])?;
    Ok(())
}

/// "Customize toolbar" in "+" opens the list by "+"; with nothing of Teams hidden, "+" holds the AI rows and that row.
fn customize_from_plus(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.reopened_shots("plus-nothing-hidden", "chat", |user| {
        user.click_menu(0, "Plus").map(|_| ())
    })?;
    user.reopened_shots("customize-from-plus", "chat", |user| {
        user.click_menu(0, "Plus")?;
        user.press("up")?;
        user.press("enter")?;
        let state = user.until("the list from +", |state| main_box(state)["customize"] == true)?;
        ensure(main_box(&state)["menu"].is_null(), || format!("+ stays open: {state}"))
    })?;
    // Customize toolbar is the last row of "+": Up selects it
    user.click_menu(0, "Plus")?;
    user.press("up")?;
    user.press("enter")?;
    user.until("the list from + by Up", |state| main_box(state)["customize"] == true)?;
    user.take_row_by_up(4)?;
    user.until("Attach hidden from the list of +", |state| {
        hidden_is(state, &["attach"])
    })?;
    user.press("escape")?;
    let state = user.until("the list closed", |state| main_box(state)["customize"] == false)?;
    ensure(main_box(&state)["focused"] == true, || {
        format!("no keyboard after the list: {state}")
    })?;
    user.click_menu(0, "Plus")?;
    user.settled_shot("plus-attach-hidden")?;
    user.press("escape")?;
    user.close_menus("chat")?;
    user.hide(&[])?;
    Ok(())
}

/// Settings > Appearance > "Message box toolbar" shows what the box's list set, and the other way.
fn settings_both_ways(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.hide(&["emoji", "loop"])?;
    user.ask("settings appearance")?;
    user.until("the settings", |state| state["main"] == "Settings")?;
    user.shots_in_both_themes("settings-appearance")?;
    user.input.scroll(800, 450, 10)?;
    thread::sleep(SETTLE);
    user.shots_in_both_themes("settings-appearance-scrolled")?;
    // a click on a switch: Emoji shown again, Format hidden
    user.click(SETTINGS_EMOJI)?;
    user.until("Emoji shown by its switch", |state| hidden_is(state, &["loop"]))?;
    user.click(SETTINGS_FORMAT)?;
    user.until("Format hidden by its switch", |state| {
        hidden_is(state, &["format", "loop"])
    })?;
    user.settled_shot("settings-format-off-by-click")?;
    // the box's list shows the same
    user.main_chat()?;
    user.settled_shot("settings-then-bar")?;
    open_list_on_attach(user)?;
    user.settled_shot("settings-then-list")?;
    user.close_menus("chat")?;
    user.hide(&[])?;
    Ok(())
}

/// Every box takes the choice: a channel post, a thread reply, New message, edit mode; "+" in each holds Format.
fn every_box(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.hide(&["format"])?;
    // edit mode: a message sent, then Up in the empty box
    let mark = user.mark();
    user.type_text(&format!("edit me {mark}"))?;
    user.press("enter")?;
    user.wait_sent("POST", &mark)?;
    user.until("the box empty", |state| text_of(main_box(state)).is_empty())?;
    thread::sleep(SETTLE);
    user.press("up")?;
    user.until("the edit", |state| main_box(state)["mode"] == "edit")?;
    user.shots_in_both_themes("edit-mode")?;
    user.click_menu(0, "Plus")?;
    user.settled_shot("edit-mode-plus")?;
    user.press("escape")?;
    user.close_menus("chat")?;
    user.press("escape")?;
    user.until("the edit ended", |state| main_box(state)["mode"] == "new")?;
    // a channel's post and a thread's reply
    user.ask("open-channel")?;
    user.until("the channel", |state| !state["post"].is_null())?;
    user.ask("open-thread")?;
    user.until("the thread", |state| !state["thread"].is_null())?;
    user.shots_in_both_themes("channel-thread")?;
    for key in ["post", "thread"] {
        user.ask(&format!("box-menu {key} plus"))?;
        user.until(&format!("+ of the {key} box"), |state| state[key]["menu"] == "Plus")?;
        user.settled_shot(&format!("{key}-plus"))?;
        user.press("escape")?;
        user.close_menus(key)?;
    }
    user.ask("close-thread")?;
    // New message
    user.ask(&format!("open {AB_CHAT}"))?;
    user.press("ctrl+n")?;
    user.until("New message", |state| {
        state["main"] == "NewChat" && !state["new_chat"].is_null()
    })?;
    user.shots_in_both_themes("new-message")?;
    user.ask("box-menu new_chat plus")?;
    user.until("+ of New message", |state| state["new_chat"]["menu"] == "Plus")?;
    user.settled_shot("new-message-plus")?;
    user.press("escape")?;
    user.ask(&format!("open {AB_CHAT}"))?;
    user.hide(&[])?;
    Ok(())
}

/// The call's Chat pane: its stacked box takes the choice; a right click on its row opens the list; "+" holds Format.
fn call_chat_box(user: &mut User<'_>) -> SandboxResult {
    user.ask("hide format")?;
    let state = user.call_chat()?;
    ensure(hidden_is(&state, &["format"]), || format!("the choice: {state}"))?;
    ensure(call_box(&state)["stacked"] == true, || format!("not stacked: {state}"))?;
    user.shots_in_both_themes("call-chat-format-hidden")?;
    user.right_click(CALL_ROW_GAP)?;
    user.until("the list in the call", |state| call_box(state)["customize"] == true)?;
    user.settled_shot("call-chat-customize")?;
    user.take_row_by_up(6)?;
    user.until("Format shown again from the call", |state| hidden(state).is_empty())?;
    user.settled_shot("call-chat-format-shown")?;
    user.press("escape")?;
    user.ask("hide format,emoji")?;
    user.ask("call-menu-plus")?;
    user.until("+ in the call", |state| call_box(state)["menu"] == "Plus")?;
    user.settled_shot("call-chat-plus")?;
    user.press("escape")?;
    user.ask("call-menu-more")?;
    let state = user.until("More in the call", |state| call_box(state)["menu"] == "More");
    if state.is_ok() {
        user.settled_shot("call-chat-more")?;
        user.press("escape")?;
    }
    user.ask("font 20")?;
    thread::sleep(SETTLE);
    user.shots_in_both_themes("call-chat-font-20")?;
    user.ask("call-menu-plus")?;
    user.until("+ in the call at font 20", |state| call_box(state)["menu"] == "Plus")?;
    user.settled_shot("call-chat-font-20-plus")?;
    user.press("escape")?;
    user.ask("font 14")?;
    user.hang_up()?;
    user.hide(&[])?;
    Ok(())
}

/// A 520 px window: the box stacks; with "+" in More options, More holds the hidden actions and "Customize toolbar".
fn narrow_font_dark(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.hide(&["format", "emoji"])?;
    user.type_text("a narrow draft")?;
    for (width, font) in [(520, 14), (520, 20), (800, 20), (520, 24)] {
        user.resize_focused(width)?;
        user.ask(&format!("font {font}"))?;
        thread::sleep(SETTLE);
        let name = format!("narrow-{width}-font-{font}");
        user.shots_in_both_themes(&name)?;
        let state = user.state()?;
        say(&format!("  {name}: {}", main_box(&state)));
        // More options when the row has it (then it holds "+"'s rows), else "+"
        user.ask("box-menu chat more")?;
        thread::sleep(SETTLE);
        if user.state()?["chat"]["menu"].is_null() {
            user.ask("box-menu chat plus")?;
        }
        let state = user.until(&format!("a menu at {width}"), |state| {
            !main_box(state)["menu"].is_null()
        })?;
        say(&format!("  {name}: {} open", main_box(&state)["menu"]));
        user.settled_shot(&format!("{name}-menu"))?;
        user.close_menus("chat")?;
    }
    user.ask("font 14")?;
    user.resize_focused(1280)?;
    user.ask("clear")?;
    user.hide(&[])?;
    Ok(())
}

/// "Use AI features" off: no AI tools on the bar, no AI rows in "+", the list or Settings; on again: all back; an AI
/// tool shown while off shows on the bar after on.
fn ai_off_and_on(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.hide(&["format"])?;
    user.ask("ai off")?;
    let state = user.until("AI off", |state| state["ai"] == false)?;
    ensure(hidden_is(&state, &["format"]), || {
        format!("the choice changed: {state}")
    })?;
    user.shots_in_both_themes("ai-off-bar")?;
    // "+" is now the rightmost tool
    user.click_menu(0, "Plus")?;
    user.settled_shot("ai-off-plus")?;
    user.press("escape")?;
    user.close_menus("chat")?;
    user.right_click(main_tool(1))?;
    user.until("the list", |state| main_box(state)["customize"] == true)?;
    user.settled_shot("ai-off-customize")?;
    user.press("up")?;
    user.press("enter")?;
    let state = user.until("the last row taken", |state| hidden(state).len() == 2)?;
    ensure(hidden_is(&state, &["format", "loop"]), || {
        format!("with AI off, the last row is not Loop: {state}")
    })?;
    user.press("escape")?;
    user.close_menus("chat")?;
    user.ask("settings appearance")?;
    user.until("the settings", |state| state["main"] == "Settings")?;
    user.input.scroll(800, 450, 10)?;
    thread::sleep(SETTLE);
    user.settled_shot("ai-off-settings")?;
    // Correct shown while off shows after on; Translate's rows are back in "+"
    user.ask("show-ai correct")?;
    user.ask("ai on")?;
    user.until("AI on", |state| state["ai"] == true && shown_is(state, &["correct"]))?;
    user.settled_shot("ai-on-settings")?;
    user.main_chat()?;
    user.settled_shot("ai-on-correct-bar")?;
    user.click_menu(1, "Plus")?;
    user.settled_shot("ai-on-translate-plus")?;
    user.press("escape")?;
    user.close_menus("chat")?;
    user.show_ai(&[])?;
    user.hide(&[])?;
    user.settled_shot("ai-on-bar")?;
    Ok(())
}

/// Ctrl+= with a menu open: the font grows, and Escape still closes the menu and gives the keyboard back.
fn font_key_with_menu_open(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    let mut faults = Vec::new();
    for (what, open) in [("\"+\"", 0), ("the customize list", 1)] {
        if open == 0 {
            user.click_menu(0, "Plus")?;
        } else {
            open_list_on_attach(user)?;
        }
        user.press("ctrl+=")?;
        thread::sleep(SETTLE);
        user.settled_shot(&format!("font-key-{}", if open == 0 { "plus" } else { "list" }))?;
        user.press("escape")?;
        thread::sleep(SETTLE);
        let state = user.state()?;
        let boxed = main_box(&state);
        if !(boxed["menu"].is_null() && boxed["customize"] == false && boxed["focused"] == true) {
            faults.push(format!("{what}: after Ctrl+= and Escape: {boxed}"));
        }
        user.close_menus("chat")?;
        user.ask("font 14")?;
        user.click(MAIN_TEXT)?;
    }
    ensure(faults.is_empty(), || faults.join("; "))
}

/// Hides Emoji and Loop by the list, for the restart.
fn hide_for_restart(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.right_click(main_tool(1))?;
    user.until("the list", |state| main_box(state)["customize"] == true)?;
    user.take_row_by_up(5)?;
    user.until("Emoji hidden", |state| hidden_is(state, &["emoji"]))?;
    // Emoji's row stays selected: Loop is two rows below it
    for key in ["down", "down", "enter"] {
        user.press(key)?;
    }
    user.until("Loop hidden", |state| hidden_is(state, &["emoji", "loop"]))?;
    user.press("escape")?;
    user.close_menus("chat")?;
    user.settled_shot("before-restart")?;
    let text = user.settings_text();
    ensure(text.contains("compose_hidden"), || format!("not in the file: {text}"))
}

/// The second run: Emoji and Loop still hidden, the language still German; "+" holds Emoji.
fn kept_after_restart(user: &mut User<'_>) -> SandboxResult {
    let state = user.main_chat()?;
    ensure(hidden_is(&state, &["emoji", "loop"]), || format!("not kept: {state}"))?;
    ensure(state["language"] == "German", || format!("the language: {state}"))?;
    user.shots_in_both_themes("after-restart")?;
    user.click_menu(0, "Plus")?;
    user.settled_shot("after-restart-plus")?;
    user.press("down")?;
    user.press("enter")?;
    user.until("Emoji by its row after the restart", |state| {
        main_box(state)["emoji_open"] == true
    })?;
    user.press("escape")?;
    user.close_menus("chat")?;
    Ok(())
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
    // a restart keeps the file the first run left; the first run's file has the AI features on, as answered
    let mut opened = settings.load().settings;
    opened.selected_chat = Some(AB_CHAT.to_owned());
    opened.ai_features.get_or_insert(true);
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
        "menu-plus" | "menu-more" | "menu-ai" => {
            let Some(chat) = stage.read(cx).chat_view().cloned() else {
                return "no chat".to_owned();
            };
            let Some(kind) = menu_kind(&verb["menu-".len()..]) else {
                return "no menu".to_owned();
            };
            let compose = chat.read(cx).compose().clone();
            compose.update(cx, |compose, cx| compose.open_menu(kind, window, cx));
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
        "settings appearance" => {
            shell.update(cx, |shell, cx| shell.open_settings_page(Page::Appearance, window, cx));
            "ok".to_owned()
        }
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
        "clear" => {
            for compose in boxes(shell, cx) {
                compose.update(cx, |compose, cx| compose.discard(window, cx));
            }
            "ok".to_owned()
        }
        other => on_shell_with(shell, other, window, cx),
    }
}

/// A verb with an argument: a chat to open, the buttons to hide, the AI switch, a box's menu, the language, the font.
fn on_shell_with(shell: &Entity<Shell>, other: &str, window: &mut Window, cx: &mut App) -> String {
    if let Some(id) = other.strip_prefix("open ") {
        shell.update(cx, |shell, cx| shell.open_chat(id.to_owned(), window, cx));
        "ok".to_owned()
    } else if let Some(names) = other.strip_prefix("hide") {
        let hidden = tools_named(names);
        change_settings(shell, cx, |settings| {
            for tool in ComposeTool::ALL.into_iter().filter(|tool| !tool.uses_ai()) {
                settings.show_compose_tool(tool, !hidden.contains(&tool));
            }
        });
        "ok".to_owned()
    } else if let Some(names) = other.strip_prefix("show-ai") {
        let shown = tools_named(names);
        change_settings(shell, cx, |settings| {
            for tool in ComposeTool::ALL.into_iter().filter(|tool| tool.uses_ai()) {
                settings.show_compose_tool(tool, shown.contains(&tool));
            }
        });
        "ok".to_owned()
    } else if let Some(on) = other.strip_prefix("ai ") {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| model.set_ai_features(on == "on", cx));
        "ok".to_owned()
    } else if let Some(rest) = other.strip_prefix("box-menu ") {
        box_menu(shell, rest, window, cx)
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
    } else if let Some(reply) = qa700::on_verb(shell, other, window, cx) {
        reply
    } else {
        format!("unknown {other}")
    }
}

/// The buttons named in a comma list.
fn tools_named(names: &str) -> Vec<ComposeTool> {
    names
        .trim()
        .split(',')
        .filter_map(|name| ComposeTool::ALL.into_iter().find(|tool| tool.key() == name.trim()))
        .collect()
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

/// The kind of a menu of tools by its name in a verb.
fn menu_kind(name: &str) -> Option<MenuKind> {
    match name {
        "plus" => Some(MenuKind::Plus),
        "more" => Some(MenuKind::More),
        _ => None,
    }
}

/// `<box> <menu>`: opens the menu of tools of the main window's box (`chat`, `post`, `thread`, `new_chat`), as a
/// click on its button does, for a box whose buttons the parent does not place.
fn box_menu(shell: &Entity<Shell>, rest: &str, window: &mut Window, cx: &mut App) -> String {
    let Some((key, kind)) = rest.split_once(' ') else {
        return format!("bad box-menu {rest}");
    };
    let Some(kind) = menu_kind(kind) else {
        return format!("no menu {kind}");
    };
    let shell_ref = shell.read(cx);
    let compose = match key {
        "chat" => shell_ref.chat().map(|chat| chat.read(cx).compose().clone()),
        "post" => shell_ref
            .channel()
            .map(|channel| channel.read(cx).posts().read(cx).compose().clone()),
        "thread" => shell_ref.channel().and_then(|channel| {
            channel
                .read(cx)
                .thread()
                .map(|(_, thread)| thread.read(cx).compose().clone())
        }),
        "new_chat" => shell_ref.new_chat().map(|view| view.read(cx).compose(cx)),
        _ => None,
    };
    let Some(compose) = compose else {
        return format!("no box {key}");
    };
    compose.update(cx, |compose, cx| compose.open_menu(kind, window, cx));
    "ok".to_owned()
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
    let (rows, selected) = qa700::chat_rows(shell_ref, cx);
    json!({
        "main": main,
        "rows": rows,
        "selected": selected,
        "chat": chat,
        "new_chat": new_chat,
        "post": post,
        "thread": thread,
        "prejoin": calls.prejoin().is_some(),
        "language": shell_ref.settings().read(cx).settings().draft_translate_to,
        "reading": shell_ref.settings().read(cx).settings().message_translate_to,
        "shown": shell_ref
            .settings()
            .read(cx)
            .settings()
            .compose_shown
            .iter()
            .map(|tool| tool.key())
            .collect::<Vec<_>>(),
        "hidden": shell_ref
            .settings()
            .read(cx)
            .settings()
            .compose_hidden
            .iter()
            .map(|tool| tool.key())
            .collect::<Vec<_>>(),
        "ai": shell_ref.settings().read(cx).uses_ai(),
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
        "menu": compose.menu_open().map(|kind| format!("{kind:?}")),
        "row_tools": compose.row_tools(cx),
        "plus_rows": compose.menu_selectors(MenuKind::Plus, cx),
        "more_rows": compose.menu_selectors(MenuKind::More, cx),
        "customize": compose.customize_open(),
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
    })
}
