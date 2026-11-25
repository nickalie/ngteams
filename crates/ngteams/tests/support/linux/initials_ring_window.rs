//! Avatars of odd names in each view, and the ringing banner over an open chat, as a user meets them, in the UI
//! sandbox (`harness = false`: GPUI runs on the process's main thread). QA of task 373.
//!
//! Two cases, each in a sandbox of its own, the app as account a:
//!
//! - `mock`: a Microsoft account on the mock chat service, its Trouter and its calling service (made-up people of odd
//!   names post in a group chat with no topic and fill a's activity feed; b rings a in their 1:1 chat and in the
//!   a-b-c chat; made-up people of odd names join the group call).
//! - `own`: an own-server account on the real `ngteams-server` binary (`serve`, SQLite, the run's coturn), with
//!   synthetic users of odd names: their 1:1 chats with a, a group chat with a topic and one without, a message of
//!   each; b (a quoted nickname) rings a in their 1:1 chat and in the group chat; c (Chinese letters only) and d (a
//!   guest's bracket) answer the group call.
//!
//! In each case the parent walks the views from inside the app (the chats, the chat's people, a compact and a full
//! profile card, the activity) and screenshots each in Teams Light and Dark. For each ring the window shows a chat; the
//! parent screenshots the window as fast as the compositor lets it from the ring's start for [`BURST`]: no shot may
//! show the band under the title bar empty (the fault of QA 297's Q5), and one must show the banner. The answered group
//! call's window is shot in Teams Light and Dark. Shots under `target/ui-shots/initials-ring-*.png` (and
//! `$INITIALS_RING_EVIDENCE`, if set). `INITIALS_RING_CASES=mock` (or `own`) runs one case alone.
//!
//! The `own` case needs `NGTS_TEST_COTURN`, `NGTS_TEST_COTURN_SECRET` and the server binary beside the test's target
//! directory. Without them, or without weston 15, niri or `dbus-daemon`, a case says so and passes, unless
//! `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowingModes};
use image::RgbImage;
use ngteams::accounts::Started;
use ngteams::app::{self, Launch};
use ngteams::calling::PersonalCalls;
use ngteams::descriptors::adopt_trace;
use teams_auth::secrets::MemorySecretStore;
use teams_ctl::protocol::{CallRow, Request, Response};
use teams_ctl::{Client as CtlClient, Timeouts};
use teams_proto::Mri;
use teams_ui::chat_view::Item;
use teams_ui::profile_card::{CardOf, CardTab, Form};
use teams_ui::settings::Settings;
use teams_ui::shell::Shell;
use teams_ui::view::Root;
use teams_ui::{SettingsFile, VideoFeed};
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "initials_ring_mock.rs"]
mod mock;
#[path = "initials_ring_own.rs"]
mod own;
#[path = "../window.rs"]
mod support;

const CHILD_ENV: &str = "NGTEAMS_INITIALS_RING_TEST";
const TRACE_FD_ENV: &str = "NGTEAMS_INITIALS_RING_TRACE_FD";
const DIR_ENV: &str = "NGTEAMS_INITIALS_RING_DIR";
const CTL_ENV: &str = "NGTEAMS_INITIALS_RING_CTL";
const CHAT_ENV: &str = "NGTEAMS_INITIALS_RING_CHAT";
/// Where the screenshots are copied besides `target/ui-shots/`, if set (the QA run's evidence).
const EVIDENCE_ENV: &str = "INITIALS_RING_EVIDENCE";
/// The cases to run, comma-separated (`mock`, `own`); both when unset.
const CASES_ENV: &str = "INITIALS_RING_CASES";
/// The file in the sandbox dir the parent writes a step into for the child to take.
const STEP_TRIGGER: &str = "step";
/// The file in the sandbox dir the parent writes to have the child quit.
const QUIT_TRIGGER: &str = "quit";
const STARTUP: Duration = Duration::from_secs(150);
const WAIT: Duration = Duration::from_secs(45);
/// How long the window gets to draw what a step changed.
const SETTLE: Duration = Duration::from_millis(1200);
/// How long the shots after a ring's start go on.
const BURST: Duration = Duration::from_secs(4);
/// niri's layout: each window takes the whole screen, and no hot corner opens the overview.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
const THEMES: [&str; 2] = [teams_ui::theme::LIGHT_KEY, teams_ui::theme::DARK_KEY];

/// Synthetic people of odd names, and the initials Teams shows for each (none: the blank face).
const ODD: [(&str, &str); 14] = [
    ("Jean-Luc O'Brien", "JO"),
    ("🎉 Party Planner 🎉", "PP"),
    ("Иван Петров", "ИП"),
    ("محمد علي", ""),
    ("Cher", "C"),
    ("Mary Ann van Dyke", "MD"),
    ("Ann :( Lee", "AL"),
    ("+1 425 555 0100", ""),
    ("Ζωή Αλεξίου", "ΖΑ"),
    ("élodie dupont", "ÉD"),
    ("Bob <3 Alice", "BA"),
    ("김 민준", ""),
    ("Zoë [External] Ångström", "ZÅ"),
    ("(Guest)", ""),
];

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("mock") => mock::child(),
        Ok(_) => own::child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("initials ring window test: {error}");
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

fn parent() -> SandboxResult {
    let cases = std::env::var(CASES_ENV).unwrap_or_else(|_| "mock,own".to_owned());
    for case in cases.split(',') {
        say(&format!("case {case}"));
        match case {
            "mock" => mock::parent()?,
            "own" => own::parent()?,
            other => return Err(format!("no case {other:?}").into()),
        }
    }
    say("ngteams initials ring window: ok");
    Ok(())
}

/// A sandbox of the default 1280×800 screen, or `None` (said) where none can run.
fn sandbox(test: &str) -> SandboxResult<Option<Sandbox>> {
    let options = SandboxOptions {
        niri_extra: NIRI_CONFIG,
        ..SandboxOptions::default()
    };
    sandbox_or_skip(test, &options)
}

fn evidence_dirs() -> Vec<PathBuf> {
    let shots = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    let mut dirs = vec![shots];
    dirs.extend(std::env::var_os(EVIDENCE_ENV).map(PathBuf::from));
    dirs
}

/// Keeps `png` as `initials-ring-<name>.png` in each evidence dir.
fn keep(png: &Path, name: &str) -> SandboxResult {
    for dir in evidence_dirs() {
        fs::create_dir_all(&dir)?;
        fs::copy(png, dir.join(format!("initials-ring-{name}.png")))?;
    }
    Ok(())
}

// --- the parent: the app under test, its steps and the screenshots ---

struct AppUnderTest {
    running: Running,
    trace: TraceReader,
    socket: PathBuf,
}

impl AppUnderTest {
    /// This test again in `sandbox` as the app of `case` (`mock`, `own`), with `extra` in its environment, its data in
    /// `dir`, the chat `chat` open at its start; once it said `ready` and its trace told `waits`.
    fn start(
        sandbox: &Sandbox,
        (case, extra): (&str, &[(&str, &str)]),
        dir: &Path,
        chat: &str,
        waits: &[(&str, &str)],
    ) -> SandboxResult<Self> {
        let own = dir.join("a");
        fs::create_dir_all(&own)?;
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let socket = sandbox.path("run/ctl.sock");
        let own_text = own.to_string_lossy().into_owned();
        let socket_text = socket.to_string_lossy().into_owned();
        let mut environment = vec![
            (CHILD_ENV, case),
            (TRACE_FD_ENV, fd.as_str()),
            (DIR_ENV, own_text.as_str()),
            (CTL_ENV, socket_text.as_str()),
            (CHAT_ENV, chat),
        ];
        environment.extend_from_slice(extra);
        let child = sandbox.spawn_with_fds(&std::env::current_exe()?, &[], &environment, &[writer.as_fd()])?;
        drop(writer);
        let mut app = Self {
            running: Running::new(child)?,
            trace: TraceReader::start(reader),
            socket,
        };
        app.running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| app.running.fail(&format!("no ready line: {error:?}")))?;
        for (view, change) in waits {
            app.trace.wait_for(view, change)?;
        }
        Ok(app)
    }

    fn quit(mut self, sandbox: &Sandbox) -> SandboxResult {
        fs::write(sandbox.path(QUIT_TRIGGER), "")?;
        self.running
            .line_where(WAIT, |line| line == "quit")
            .map_err(|error| self.running.fail(&format!("didn't quit: {error:?}")))?;
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("exited {status}: {stderr}").into());
        }
        Ok(())
    }
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

    fn catch_up(&mut self) -> &[serde_json::Value] {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
        &self.seen
    }

    fn wait_for(&mut self, view: &str, change: &str) -> SandboxResult {
        let deadline = Instant::now() + STARTUP;
        while !self
            .catch_up()
            .iter()
            .any(|line| line["view"] == view && line["change"] == change)
        {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .map_err(|_| format!("no trace line for {view} {change}"))?;
            self.seen.push(line);
        }
        Ok(())
    }
}

/// The parent's hold on the app: its steps, its control socket, its windows.
struct Rig<'a> {
    sandbox: &'a Sandbox,
    app: &'a mut AppUnderTest,
    ctl: CtlClient,
    case: &'static str,
    /// niri's id of the main window.
    main: u64,
}

impl<'a> Rig<'a> {
    fn new(sandbox: &'a Sandbox, app: &'a mut AppUnderTest, case: &'static str) -> SandboxResult<Self> {
        let socket = app.socket.clone();
        let ctl = wait_until(WAIT, || CtlClient::connect(&socket, Timeouts::default()).ok())
            .ok_or_else(|| format!("no control socket at {}", socket.display()))?;
        let main = newest_window(sandbox)?;
        Ok(Self {
            sandbox,
            app,
            ctl,
            case,
            main,
        })
    }

    /// Has the child take `command` (see [`run_step`]) and waits until the window drew it.
    fn step(&mut self, command: &str) -> SandboxResult {
        let staged = self.sandbox.path("step.staged");
        fs::write(&staged, command)?;
        fs::rename(&staged, self.sandbox.path(STEP_TRIGGER))?;
        let (did, failed) = (format!("did {command}"), format!("failed {command}"));
        let line = self
            .app
            .running
            .line_where(WAIT, |line| line == did || line.starts_with(&failed))
            .map_err(|error| format!("step {command}: {error:?}"))?;
        if line.starts_with(&failed) {
            return Err(line.into());
        }
        thread::sleep(SETTLE);
        Ok(())
    }

    fn theme(&mut self, theme: &str) -> SandboxResult {
        self.step(&format!("theme {theme}"))
    }

    fn focus(&self, id: u64) -> SandboxResult {
        self.sandbox
            .niri_msg(&["action", "focus-window", "--id", &id.to_string()])?;
        thread::sleep(Duration::from_millis(300));
        Ok(())
    }

    /// A screenshot of the focused window, kept as `<case>-<name>`.
    fn save(&self, name: &str) -> SandboxResult<RgbImage> {
        let file = format!("{}-{name}", self.case);
        let png = self.sandbox.screenshot(&format!("{file}.png"), true)?;
        keep(&png, &file)?;
        let shot = image::open(&png)?.to_rgb8();
        let _ = fs::remove_file(png);
        Ok(shot)
    }

    /// The main window, shot as `<case>-<name>`.
    fn shot(&self, name: &str) -> SandboxResult<RgbImage> {
        self.focus(self.main)?;
        self.save(name)
    }

    /// Each view that draws a person's avatar, in Teams Light and Dark: each of `chats` (`(name, id)`: the chat list,
    /// the chat's header and its messages' faces), the people of the last, a compact card beside the face of its last
    /// message from someone else, the full card of `card` (`(mri, name)`), the activity.
    fn views(&mut self, chats: &[(&str, &str)], card: (&str, &str)) -> SandboxResult {
        for theme in THEMES {
            self.theme(theme)?;
            for (name, chat) in chats {
                self.step(&format!("chat {chat}"))?;
                self.shot(&format!("{name}-{theme}"))?;
            }
            self.step("people")?;
            self.shot(&format!("people-{theme}"))?;
            self.step("people")?;
            self.step("compact")?;
            self.shot(&format!("compact-card-{theme}"))?;
            self.step("close-card")?;
            self.step(&format!("card {}|{}", card.0, card.1))?;
            self.shot(&format!("card-{theme}"))?;
            self.step("close-card")?;
            self.step("activity")?;
            self.shot(&format!("activity-{theme}"))?;
        }
        Ok(())
    }

    /// The main window shot as fast as the compositor lets it, from `ring`'s start for [`BURST`]: no shot may show the
    /// band under the title bar empty, and one must show the banner (the band differs from the shot before the ring).
    fn ring_burst<T>(&mut self, name: &str, ring: impl FnOnce() -> SandboxResult<T>) -> SandboxResult<T> {
        let before = self.shot(&format!("{name}-before"))?;
        band_drawn(&before).map_err(|error| format!("{name}, before the ring: {error}"))?;
        let started = Instant::now();
        let started_call = ring()?;
        let mut first = None;
        let mut index = 0;
        while started.elapsed() < BURST {
            let shot = self.save(&format!("{name}-{index:02}"))?;
            let at = started.elapsed();
            band_drawn(&shot).map_err(|error| format!("{name}, shot {index} at {at:?}: {error}"))?;
            if first.is_none() && band_changed(&before, &shot) {
                first = Some((index, at));
            }
            index += 1;
        }
        say(&format!("{name}: {index} shots; the first with the banner: {first:?}"));
        if first.is_none() {
            return Err(format!("{name}: no shot shows the banner").into());
        }
        self.wait_call("ringing", |row| row.ringing)?;
        Ok(started_call)
    }

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

    fn wait_no_call(&mut self, what: &str, gone: impl Fn(&CallRow) -> bool) -> SandboxResult {
        let deadline = Instant::now() + WAIT;
        loop {
            let calls = self.calls()?;
            if calls.iter().all(&gone) {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("{what}: the calls are {calls:?}").into());
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Declines the ringing call over the control socket, as `ngteamsctl decline` does.
    fn decline(&mut self) -> SandboxResult {
        self.ctl.request(None, Request::Decline)?;
        self.wait_no_call("the ring declined", |row| !row.ringing)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// Answers the ringing call over the control socket, as `ngteamsctl answer` does; the call window opens.
    fn answer(&mut self) -> SandboxResult {
        self.ctl.request(None, Request::Answer)?;
        self.wait_call("connected", |row| row.state == "connected")?;
        thread::sleep(SETTLE * 2);
        Ok(())
    }

    /// The call window, in Teams Light and Dark, shot as `<name>-<theme>`; the main window focused again.
    fn call_window(&mut self, name: &str) -> SandboxResult {
        let window = newest_window(self.sandbox)?;
        if window == self.main {
            return Err("no call window".into());
        }
        for theme in THEMES {
            self.theme(theme)?;
            self.focus(window)?;
            self.save(&format!("{name}-{theme}"))?;
        }
        self.focus(self.main)
    }

    fn hang_up(&mut self) -> SandboxResult {
        self.ctl.request(None, Request::HangUp)?;
        self.wait_no_call("the call left", |row| row.state != "connected")?;
        thread::sleep(SETTLE);
        Ok(())
    }
}

/// niri's id of the newest window.
fn newest_window(sandbox: &Sandbox) -> SandboxResult<u64> {
    let text = sandbox.niri_msg(&["-j", "windows"])?;
    let windows: serde_json::Value = serde_json::from_str(&text)?;
    let id = windows
        .as_array()
        .and_then(|list| list.iter().filter_map(|window| window["id"].as_u64()).max())
        .ok_or_else(|| format!("no windows: {text}"))?;
    Ok(id)
}

/// The band under the title bar, right of the rail, that the ringing banner takes: rows and columns.
const BAND: (std::ops::Range<u32>, u32) = (54..88, 96);

fn apart(one: image::Rgb<u8>, other: image::Rgb<u8>) -> bool {
    one.0
        .iter()
        .zip(other.0)
        .map(|(a, b)| u32::from(a.abs_diff(b)))
        .sum::<u32>()
        > 48
}

/// Fails when the band is one colour across: drawn but empty (QA 297's Q5). The banner's words and buttons, or the
/// panes' headers, make it more than one.
fn band_drawn(shot: &RgbImage) -> SandboxResult {
    let (rows, left) = BAND;
    let (width, height) = shot.dimensions();
    if width <= left * 2 || height <= rows.end {
        return Err(format!("the shot is {width}×{height}").into());
    }
    let first = *shot.get_pixel(left, rows.start);
    let drawn = rows
        .clone()
        .any(|y| (left..width - left).any(|x| apart(*shot.get_pixel(x, y), first)));
    if drawn {
        Ok(())
    } else {
        Err(format!("an empty band of {first:?} under the title bar").into())
    }
}

/// Whether a tenth of the band's pixels or more differ between the two shots: the banner came in above the panes.
fn band_changed(before: &RgbImage, after: &RgbImage) -> bool {
    let (rows, left) = BAND;
    if before.dimensions() != after.dimensions() {
        return true;
    }
    let width = before.width();
    let mut changed = 0_u32;
    let mut all = 0_u32;
    for y in rows {
        for x in left..width.saturating_sub(left) {
            all += 1;
            changed += u32::from(apart(*before.get_pixel(x, y), *after.get_pixel(x, y)));
        }
    }
    changed * 10 >= all
}

// --- the child: the app, its steps taken as the parent writes them ---

/// The app of `first` with its calls (`video` their feed, `echo` the test call), the chat in [`CHAT_ENV`] open; it
/// takes the parent's steps and quits when asked.
fn run_app(first: Started<MemorySecretStore>, video: VideoFeed, echo: &str) -> SandboxResult {
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let runner = support::runner(&dir)?;
    let settings = SettingsFile::new(dir.join("settings.toml"));
    let opened = Settings {
        selected_chat: Some(env(CHAT_ENV)?),
        ..Settings::default()
    };
    settings.save(&opened).map_err(|problem| format!("{problem:?}"))?;
    let ctl = PathBuf::from(env(CTL_ENV)?);
    let first = Started {
        calls: Some(PersonalCalls {
            ringer: None,
            sounds: None,
            echo: Mri::new(echo),
            video: Some(video),
            camera: None,
            devices: None,
            noise: teams_media::processing::NoiseControl::default(),
        }),
        ..first
    };
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
                steps_when_asked(launched.desktop, cx);
                quit_when_asked(cx);
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

/// Each time the parent writes [`STEP_TRIGGER`]: the step it names, then `did <step>` once the window drew it.
fn steps_when_asked(desktop: Entity<teams_ui::Desktop>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let trigger = PathBuf::from(dir).join(STEP_TRIGGER);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(command) = fs::read_to_string(&trigger) else {
                continue;
            };
            let _ = fs::remove_file(&trigger);
            cx.update(|cx| run_step(&desktop, command.trim(), cx));
        }
    })
    .detach();
}

/// Takes `command`: `theme <key>`, `chat <id>`, `people` (its flyout opened or closed), `compact` (the card beside the
/// face of the open chat's last message from someone else), `card <mri>|<name>` (a full card), `close-card`,
/// `activity`.
fn run_step(desktop: &Entity<teams_ui::Desktop>, command: &str, cx: &mut App) {
    if let Some(theme) = command.strip_prefix("theme ") {
        let theme = if theme == teams_ui::theme::DARK_KEY {
            teams_ui::theme::DARK_KEY
        } else {
            teams_ui::theme::LIGHT_KEY
        };
        fixed_theme(desktop, theme, cx);
    }
    let Some(handle) = desktop.read(cx).window() else {
        say(&format!("failed {command}: no window"));
        return;
    };
    let ran = handle.update(cx, |kit, window, cx| {
        let shell = kit
            .view()
            .clone()
            .downcast::<Root>()
            .ok()
            .and_then(|root| root.read(cx).shell_in(cx).cloned());
        match shell {
            Some(shell) => shell_step(&shell, command.to_owned(), window, cx),
            None => say(&format!("failed {command}: no shell")),
        }
    });
    if ran.is_err() {
        say("failed: the window is gone");
    }
}

fn shell_step(shell: &Entity<Shell>, command: String, window: &mut Window, cx: &mut App) {
    let (verb, rest) = command.split_once(' ').unwrap_or((command.as_str(), ""));
    match verb {
        "theme" => {}
        "chat" => shell.update(cx, |shell, cx| {
            shell.close_card(window, cx);
            shell.show_chats(window, cx);
            shell.open_chat(rest.to_owned(), window, cx);
        }),
        "people" => shell.update(cx, |shell, cx| shell.toggle_people(window, cx)),
        "compact" => return compact_card(shell.clone(), window, 20, command),
        "card" => {
            let (mri, name) = rest.split_once('|').unwrap_or((rest, ""));
            let of = CardOf::new(mri, (!name.is_empty()).then(|| name.to_owned().into()));
            shell.update(cx, |shell, cx| {
                shell.open_card(of, (Form::Full, CardTab::Overview), gpui::Bounds::default(), window, cx);
            });
        }
        "close-card" => shell.update(cx, |shell, cx| shell.close_card(window, cx)),
        "activity" => shell.update(cx, |shell, cx| shell.show_activity(window, cx)),
        _ => return say(&format!("failed {command}: no such step")),
    }
    did(window, command);
}

/// Says `did <command>` once the window drew a few frames after it.
fn did(window: &Window, command: String) {
    after_frames(window, 3, move |_, _| say(&format!("did {command}")));
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

/// Opens the compact card beside the face of the open chat's last message from someone else, once the view drew it:
/// tried again on the next frames, `tries` times.
fn compact_card(shell: Entity<Shell>, window: &mut Window, tries: usize, command: String) {
    after_frames(window, 2, move |window, cx| match face_to_open(&shell, cx) {
        Some((of, face)) => {
            shell.update(cx, |shell, cx| {
                shell.open_card(of, (Form::Compact, CardTab::Overview), face, window, cx);
            });
            did(window, command);
        }
        None if tries > 0 => compact_card(shell, window, tries - 1, command),
        None => say(&format!("failed {command}: no face drawn")),
    });
}

fn face_to_open(shell: &Entity<Shell>, cx: &App) -> Option<(CardOf, gpui::Bounds<gpui::Pixels>)> {
    let chat = shell.read(cx).chat()?.clone();
    let chat = chat.read(cx);
    chat.items().iter().rev().find_map(|item| match item {
        Item::Message(message) if message.header && !message.own => {
            let mri = message.sender.clone()?;
            let face = chat.face_bounds(&message.id)?;
            Some((CardOf::new(mri, Some(message.sender_name.clone())), face))
        }
        _ => None,
    })
}

/// The settings show `theme` whatever the desktop's, as the appearance page's tiles choose one.
fn fixed_theme(desktop: &Entity<teams_ui::Desktop>, theme: &'static str, cx: &mut App) {
    let settings = desktop.read(cx).settings().clone();
    settings.update(cx, |model, cx| {
        model.change(
            |settings| {
                settings.appearance.follow_system = false;
                settings.appearance.theme = theme;
            },
            cx,
        );
    });
}

/// Once the parent writes [`QUIT_TRIGGER`] in the sandbox dir: the app quits.
fn quit_when_asked(cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let trigger = PathBuf::from(dir).join(QUIT_TRIGGER);
    cx.spawn(async move |cx: &mut AsyncApp| {
        while !trigger.exists() {
            cx.background_executor().timer(Duration::from_millis(100)).await;
        }
        cx.update(|cx| {
            app::quit(cx);
        });
    })
    .detach();
}
