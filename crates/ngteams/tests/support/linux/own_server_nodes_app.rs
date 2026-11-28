//! One app instance of the two-node test: the child (`ngteams` as its first run starts it) and the parent's handle on
//! it (its own UI sandbox, its keyboard, its control socket, its view trace, its screenshots).
//!
//! **The child** opens the first sign-in screen as `ngteams` opens it with no account (`account_prompt`, with the
//! choice of an own server), its own-server side the app's own ([`OwnPrompt`], [`SignIns::app`]). Once the parent's
//! typing signs an account in, the child starts it as `ngteams` starts an account after that sign-in (its engine with
//! push and calling, [`ngteams::engine::start`]; `ngteams` itself runs again for it, which a test can't), its calls on
//! the file devices (a tone, the test pattern), and launches the app's window on it ([`app::launch`]), serving the
//! control socket; the sign-in screen closes. It says `signed-in <mri>`, then `ready`.
//!
//! Asked through files in its sandbox dir, it also reads what its window reads for a message's picture and a sent
//! file ([`PROBE`]: the bytes of the picture, the file's whole download), sets the theme, and quits.

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};
use std::{fs, thread};

use gpui::{App, AppContext as _, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::accounts::{Runner, Started};
use ngteams::accounts_file::{PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::descriptors::adopt_trace;
use ngteams::own_prompt::{OwnPrompt, SignIns};
use ngteams::secrets::AppSecrets;
use ngteams::setup::Setup;
use teams_core::CallFloor;
use teams_core::engine::EngineRequests;
use teams_ctl::protocol::{CallRow, Request, Response};
use teams_ctl::{Client as CtlClient, Timeouts};
use teams_media::devices::Devices;
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::tone::Tone;
use teams_ui::account_prompt::{self, AccountPrompt, OwnOffer, SignInKind};
use teams_ui::settings::Settings;
use teams_ui::window::{KitRoot, PERSONAL_APP_ID};
use teams_ui::{SettingsFile, SettingsModel, ViewTrace};
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

use super::own_server_binary::PASSWORD;
use super::own_server_nodes::Nodes;

pub const CHILD_ENV: &str = "NGTEAMS_OWN_NODES_TEST";
const TRACE_FD_ENV: &str = "NGTEAMS_OWN_NODES_TRACE_FD";
const DIR_ENV: &str = "NGTEAMS_OWN_NODES_DIR";
const CTL_ENV: &str = "NGTEAMS_OWN_NODES_CTL";
const ORIGIN_ENV: &str = "NGTEAMS_OWN_NODES_ORIGIN";
const TONE_ENV: &str = "NGTEAMS_OWN_NODES_TONE";
/// Where the child keeps the app's own debug log (every request and response, credentials redacted).
const LOG_ENV: &str = "NGTEAMS_OWN_NODES_LOG";
/// The file in the sandbox dir the parent writes to have the child quit.
const QUIT_TRIGGER: &str = "quit";
/// The file in the sandbox dir the parent writes a theme's key into.
const THEME_TRIGGER: &str = "theme";
/// The file in the sandbox dir the parent writes to have the window asking for an account said now.
const PROMPT_ASK: &str = "say-prompt";
/// The file in the sandbox dir the parent writes `picture <chat> <id>` or `file <chat> <id>` into: the child reads
/// it as its window does and says `probe …` of what it got.
pub const PROBE: &str = "probe";
/// From the child's start to its sign-in screen, and from a sign-in to the window ready.
const STARTUP: Duration = Duration::from_secs(180);
/// A sign-in (the password hash is slow in a debug build on a loaded host).
const SIGN_IN: Duration = Duration::from_secs(180);
pub const WAIT: Duration = Duration::from_secs(60);
const READY_FRAMES: usize = 3;

pub fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn env(name: &str) -> SandboxResult<String> {
    Ok(std::env::var(name).map_err(|_| format!("{name} isn't set"))?)
}

pub fn ensure(holds: bool, what: impl FnOnce() -> String) -> SandboxResult {
    if holds { Ok(()) } else { Err(what().into()) }
}

/// Where the screenshots are kept: `target/ui-shots/`, never committed.
pub fn shots_dir() -> SandboxResult<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

// --- the parent's side ---

/// The view trace's lines as they come, each with when the parent read it.
pub struct TraceReader {
    lines: mpsc::Receiver<(Instant, serde_json::Value)>,
    pub seen: Vec<(Instant, serde_json::Value)>,
}

impl TraceReader {
    fn start(reader: io::PipeReader) -> Self {
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let value = serde_json::from_str(&line).unwrap_or(serde_json::Value::String(line));
                let _ = sender.send((Instant::now(), value));
            }
        });
        Self {
            lines,
            seen: Vec::new(),
        }
    }

    /// Where the trace is now: [`TraceReader::after`] reads from here.
    pub fn mark(&mut self) -> usize {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
        self.seen.len()
    }

    /// The first line since `mark` that `found` takes, within `within`: when it came.
    pub fn after(
        &mut self,
        mark: usize,
        within: Duration,
        what: &str,
        found: impl Fn(&serde_json::Value) -> bool,
    ) -> SandboxResult<Instant> {
        let deadline = Instant::now() + within;
        let mut from = mark;
        loop {
            if let Some((at, _)) = self
                .seen
                .get(from..)
                .and_then(|lines| lines.iter().find(|(_, line)| found(line)))
            {
                return Ok(*at);
            }
            from = self.seen.len();
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self.lines.recv_timeout(left).map_err(|_| {
                let tail: Vec<String> = self
                    .seen
                    .iter()
                    .rev()
                    .take(25)
                    .map(|(_, line)| line.to_string())
                    .collect();
                format!("no trace line for {what}; the last lines, newest first: {tail:#?}")
            })?;
            self.seen.push(line);
        }
    }

    /// Waits until the lines seen so far satisfy `done`, within `within`.
    pub fn until(
        &mut self,
        within: Duration,
        what: &str,
        done: impl Fn(&[(Instant, serde_json::Value)]) -> bool,
    ) -> SandboxResult {
        let deadline = Instant::now() + within;
        self.mark();
        while !done(&self.seen) {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self.lines.recv_timeout(left).map_err(|_| {
                let tail: Vec<String> = self
                    .seen
                    .iter()
                    .rev()
                    .take(25)
                    .map(|(_, line)| line.to_string())
                    .collect();
                format!("no trace for {what}; the last lines, newest first: {tail:#?}")
            })?;
            self.seen.push(line);
        }
        Ok(())
    }

    /// A line `found` takes, seen already or coming within `within`.
    pub fn any(&mut self, within: Duration, what: &str, found: impl Fn(&serde_json::Value) -> bool) -> SandboxResult {
        self.after(0, within, what, found).map(|_| ())
    }
}

/// The strings of a trace line's list `key`.
pub fn strings(line: &serde_json::Value, key: &str) -> Vec<String> {
    line[key]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// One app instance in a sandbox of its own.
pub struct Instance {
    pub name: &'static str,
    pub upn: &'static str,
    /// The API node the front sends its connections to first.
    pub node: usize,
    pub sandbox: Sandbox,
    pub running: Running,
    pub trace: TraceReader,
    socket: PathBuf,
    ctl: Option<CtlClient>,
    pub mri: String,
    /// What its screenshots' names start with.
    pub shots: String,
    _dir: tempfile::TempDir,
}

/// Who an instance is: its name (the front counts its connections under it), its user, its node, its tone.
pub struct Who {
    pub name: &'static str,
    pub upn: &'static str,
    pub node: usize,
    pub tone: f32,
}

impl Instance {
    /// A sandbox of its own (`None` when the run has none: skipped), the child started in it, pinned to its node,
    /// its sign-in screen up.
    pub fn start(who: &Who, nodes: &Nodes) -> SandboxResult<Option<Self>> {
        Self::start_mode(who, nodes, "app")
    }

    /// [`Instance::start`] with the child in `mode`: `app` (its first sign-in screen up) or `mixed` (the mixed
    /// scene's window up, its control socket served).
    pub fn start_mode(who: &Who, nodes: &Nodes, mode: &str) -> SandboxResult<Option<Self>> {
        Self::start_at(who, &nodes.origin, Some(nodes), mode)
    }

    /// [`Instance::start_mode`] on the server at `origin`: pinned to its node when `nodes` deploys it, else as a
    /// deployment's own front (its reverse proxy) sends it.
    pub fn start_at(who: &Who, origin: &str, nodes: Option<&Nodes>, mode: &str) -> SandboxResult<Option<Self>> {
        let options = SandboxOptions {
            niri_extra: NIRI_FULL_WINDOW,
            ..SandboxOptions::default()
        };
        let Some(sandbox) = sandbox_or_skip(&format!("ngteams own server nodes ({})", who.name), &options)? else {
            return Ok(None);
        };
        let dir = tempfile::tempdir()?;
        let (reader, writer) = io::pipe()?;
        let socket = sandbox.path("ctl.sock");
        let (fd, dir_text, socket_text) = (
            writer.as_raw_fd().to_string(),
            dir.path().to_string_lossy().into_owned(),
            socket.to_string_lossy().into_owned(),
        );
        let tone = who.tone.to_string();
        let log = shots_dir()?.join(format!("own-server-nodes-{}-app-log", who.name));
        let _ = fs::remove_dir_all(&log);
        let log = log.to_string_lossy().into_owned();
        let child = sandbox.spawn_with_fds(
            &std::env::current_exe()?,
            &[],
            &[
                (CHILD_ENV, mode),
                (TRACE_FD_ENV, &fd),
                (DIR_ENV, &dir_text),
                (CTL_ENV, &socket_text),
                (ORIGIN_ENV, origin),
                (TONE_ENV, &tone),
                (LOG_ENV, &log),
            ],
            &[writer.as_fd()],
        )?;
        drop(writer);
        let mut running = Running::new(child)?;
        if let Some(nodes) = nodes {
            nodes.pin(running.pid(), who.node, who.name);
        }
        let ready = if mode == "app" { "prompt-ready" } else { "ready" };
        running
            .line_where(STARTUP, |line| line == ready)
            .map_err(|error| running.fail(&format!("{}: not {ready}: {error:?}", who.name)))?;
        Ok(Some(Self {
            name: who.name,
            upn: who.upn,
            node: who.node,
            sandbox,
            running,
            trace: TraceReader::start(reader),
            socket,
            ctl: None,
            mri: String::new(),
            shots: "own-server-nodes".to_owned(),
            _dir: dir,
        }))
    }

    /// What the sign-in screen holds now (`prompt-now …`).
    fn prompt_now(&self) -> SandboxResult<String> {
        thread::sleep(Duration::from_millis(400));
        fs::write(self.sandbox.path(PROMPT_ASK), "")?;
        self.running
            .line_where(WAIT, |line| line.starts_with("prompt-now "))
            .map_err(|error| format!("{}: the sign-in screen wasn't said: {error:?}", self.name).into())
    }

    /// Waits until the sign-in screen has the keyboard.
    fn prompt_active(&self) -> SandboxResult<String> {
        let start = Instant::now();
        loop {
            let now = self.prompt_now()?;
            if now.contains("active=true ") {
                return Ok(now);
            }
            ensure(start.elapsed() < WAIT, || {
                format!("{}: the sign-in screen isn't active: {now}", self.name)
            })?;
        }
    }

    /// Signs in as a user does: "Own server" chosen with the keyboard on the first sign-in screen, the server's
    /// address, the user name, the password and the certificate authority's file typed, Enter. Then the app's window
    /// on the account, connected and registered for calls; the control socket.
    pub fn sign_in(&mut self, origin: &str, ca_file: &Path) -> SandboxResult {
        self.type_own_sign_in(origin, ca_file)?;
        let signed = self
            .running
            .line_where(SIGN_IN, |line| {
                line.starts_with("signed-in ") || (line.starts_with("prompt ") && !line.contains("problem=-"))
            })
            .map_err(|error| format!("{}: not signed in: {error:?}", self.name))?;
        let mri = signed
            .strip_prefix("signed-in ")
            .ok_or_else(|| format!("{}: {signed}", self.name))?;
        self.mri = mri.to_owned();
        self.running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| self.running.fail(&format!("{}: no window: {error:?}", self.name)))?;
        self.trace.any(STARTUP, "connected", |line| {
            line["view"] == "status" && line["change"] == "connected"
        })?;
        self.trace.any(STARTUP, "calling registered", |line| {
            line["view"] == "calling" && line["change"] == "registered"
        })?;
        self.connect_ctl()
    }

    /// On the sign-in screen that has the keyboard: "Own server" chosen with the keyboard (Tab, Tab from the
    /// account's field to the choice; Right; Space on to its address), the fields typed, checked, and Enter.
    pub fn type_own_sign_in(&mut self, origin: &str, ca_file: &Path) -> SandboxResult {
        let mut input = self.sandbox.input()?;
        let asked = self.prompt_active()?;
        ensure(asked.contains("kind=microsoft "), || {
            format!("{}: the screen first: {asked}", self.name)
        })?;
        self.shot("prompt")?;
        input.key(keys::TAB)?;
        input.key(keys::TAB)?;
        input.key(keys::RIGHT)?;
        input.key(keys::SPACE)?;
        type_fields(&mut input, origin, self.upn, ca_file)?;
        let typed = self.prompt_now()?;
        ensure(
            typed.contains("kind=own ") && typed.contains(&format!(" server={origin} user={} ", self.upn)),
            || format!("{}: the fields typed: {typed}", self.name),
        )?;
        self.shot("own-server-typed")?;
        input.key(keys::ENTER)
    }

    /// The control socket, once the child serves it.
    pub fn connect_ctl(&mut self) -> SandboxResult {
        let socket = self.socket.clone();
        self.ctl = Some(
            ui_sandbox::wait_until(WAIT, || CtlClient::connect(&socket, Timeouts::default()).ok())
                .ok_or_else(|| format!("{}: no control socket", self.name))?,
        );
        Ok(())
    }

    /// The control socket's answer to `request` for the account `account` (as `ngteamsctl --account` names it).
    pub fn ctl_as(&mut self, account: &str, request: Request) -> SandboxResult<Response> {
        let name = self.name;
        let ctl = self.ctl.as_mut().ok_or_else(|| format!("{name}: no control socket"))?;
        Ok(ctl
            .request(Some(account), request)
            .map_err(|error| format!("{name}: {error}"))?)
    }

    /// The control socket's answer to `request`.
    pub fn ctl(&mut self, request: Request) -> SandboxResult<Response> {
        let name = self.name;
        let ctl = self.ctl.as_mut().ok_or_else(|| format!("{name}: not signed in"))?;
        Ok(ctl.request(None, request).map_err(|error| format!("{name}: {error}"))?)
    }

    /// The calls as the control socket lists them.
    pub fn calls(&mut self) -> SandboxResult<Vec<CallRow>> {
        match self.ctl(Request::Calls)? {
            Response::Calls { calls } => Ok(calls),
            other => Err(format!("{}: calls: {other:?}", self.name).into()),
        }
    }

    /// Waits for a call `wanted` takes.
    pub fn wait_call(&mut self, what: &str, wanted: impl Fn(&CallRow) -> bool) -> SandboxResult<CallRow> {
        let deadline = Instant::now() + WAIT;
        loop {
            let calls = self.calls()?;
            if let Some(row) = calls.iter().find(|row| wanted(row)) {
                return Ok(row.clone());
            }
            ensure(Instant::now() < deadline, || {
                format!("{}: {what}: the calls are {calls:?}", self.name)
            })?;
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Writes `line` into the child's [`PROBE`] file and waits for its `probe …` answer.
    pub fn probe(&self, line: &str) -> SandboxResult<String> {
        let staged = self.sandbox.path("probe.staged");
        fs::write(&staged, line)?;
        fs::rename(&staged, self.sandbox.path(PROBE))?;
        self.running
            .line_where(WAIT, |said| said.starts_with("probe "))
            .map_err(|error| format!("{}: no answer to {line}: {error:?}", self.name).into())
    }

    /// Sets the theme (`teams_ui::theme::LIGHT_KEY` or `DARK_KEY`).
    pub fn theme(&self, key: &str) -> SandboxResult {
        fs::write(self.sandbox.path(THEME_TRIGGER), key)?;
        thread::sleep(Duration::from_millis(1500));
        Ok(())
    }

    /// A screenshot of the focused window as `<shots>-<name>-<what>.png`.
    pub fn shot(&self, what: &str) -> SandboxResult<PathBuf> {
        thread::sleep(Duration::from_millis(800));
        let file = format!("{}-{}-{what}.png", self.shots, self.name);
        let png = self.sandbox.screenshot(&file, true)?;
        let kept = shots_dir()?.join(&file);
        fs::copy(&png, &kept)?;
        let _ = fs::remove_file(png);
        say(&format!("shot {}", kept.display()));
        Ok(kept)
    }

    /// The window niri focuses now, by its id and title.
    pub fn focused_window(&self) -> SandboxResult<(u64, String)> {
        let text = self.sandbox.niri_msg(&["-j", "windows"])?;
        let windows: serde_json::Value = serde_json::from_str(&text)?;
        let focused = windows
            .as_array()
            .and_then(|list| list.iter().find(|window| window["is_focused"] == true))
            .ok_or_else(|| format!("{}: no focused window: {text}", self.name))?;
        Ok((
            focused["id"].as_u64().unwrap_or_default(),
            focused["title"].as_str().unwrap_or_default().to_owned(),
        ))
    }

    /// The main window brought to the front (niri's first window).
    pub fn focus_main(&self) -> SandboxResult {
        let text = self.sandbox.niri_msg(&["-j", "windows"])?;
        let windows: serde_json::Value = serde_json::from_str(&text)?;
        let first = windows
            .as_array()
            .and_then(|list| list.iter().filter_map(|window| window["id"].as_u64()).min())
            .ok_or_else(|| format!("{}: no windows: {text}", self.name))?;
        self.sandbox
            .niri_msg(&["action", "focus-window", "--id", &first.to_string()])?;
        Ok(())
    }

    /// The child quits; its exit.
    pub fn quit(mut self) -> SandboxResult {
        fs::write(self.sandbox.path(QUIT_TRIGGER), "")?;
        drop(self.ctl.take());
        self.running
            .line_where(WAIT, |line| line == "quit")
            .map_err(|error| self.running.fail(&format!("{} didn't quit: {error:?}", self.name)))?;
        let (status, stderr) = self.running.finish(WAIT)?;
        ensure(status.success(), || format!("exited {status}: {stderr}"))
    }
}

/// The own server's fields typed in Tab's order: address, user name, password, certificate authority's file.
fn type_fields(input: &mut VirtualInput, origin: &str, upn: &str, ca_file: &Path) -> SandboxResult {
    input.type_text(origin)?;
    input.key(keys::TAB)?;
    input.type_text(upn)?;
    input.key(keys::TAB)?;
    input.type_text(PASSWORD)?;
    input.key(keys::TAB)?;
    input.type_text(&ca_file.to_string_lossy())
}

// --- the child ---

/// What a test adds to the child's app once its window is up: it gets the app's desktop.
pub type Extra = fn(Entity<teams_ui::Desktop>, &mut App);

/// The child in `mode` (`app`, or `mixed`: [`super::mixed`]).
pub fn child(mode: &str) -> SandboxResult {
    child_with(mode, None)
}

/// [`child`], with `extra` run on the app once it launched.
pub fn child_with(mode: &str, extra: Option<Extra>) -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let log = teams_log::Config {
        dir: PathBuf::from(env(LOG_ENV)?),
        setting: teams_log::Level::Debug,
        env: None,
        rotation: teams_log::file::Rotation::default(),
    };
    teams_log::install(&log).map_err(|error| format!("the app's log: {error}"))?;
    let trace = Arc::new(ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?));
    let dir = PathBuf::from(env(DIR_ENV)?);
    if mode == "mixed" {
        return super::mixed::child(trace, &dir, PathBuf::from(env(CTL_ENV)?), env(ORIGIN_ENV)?);
    }
    let paths = PersonalPaths::new(dir.join("ngteams-config"), dir.join("ngteams-data"));
    // the sandbox has no Secret Service: this process's own store
    let setup = Setup::new(paths)?.with_secrets(AppSecrets::in_memory());
    let settings = SettingsFile::new(dir.join("settings.toml"));
    settings
        .save(&Settings::default())
        .map_err(|problem| format!("{problem:?}"))?;
    let start = Start {
        setup,
        trace,
        settings,
        ctl: PathBuf::from(env(CTL_ENV)?),
        origin: env(ORIGIN_ENV)?,
        tone: env(TONE_ENV)?.parse()?,
        failed: Arc::new(Mutex::new(None)),
        extra,
    };
    let failed = Arc::clone(&start.failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let title = app::TITLE.into();
        match account_prompt::open_offering(cx, PERSONAL_APP_ID, title, None, &OwnOffer::Choice) {
            Ok((window, prompt)) => ask(start, window, &prompt, cx),
            Err(error) => {
                say(&format!("the sign-in screen can't be opened: {error:#}"));
                cx.quit();
            }
        }
    });
    if let Some(error) = failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
        return Err(error.into());
    }
    say("quit");
    Ok(())
}

/// What the child starts the account's app with once it is signed in.
#[derive(Clone)]
struct Start {
    setup: Setup,
    trace: Arc<ViewTrace>,
    settings: SettingsFile,
    ctl: PathBuf,
    origin: String,
    tone: f32,
    failed: Arc<Mutex<Option<String>>>,
    extra: Option<Extra>,
}

/// The sign-in screen's own-server side: the app's own; a sign-in that goes through launches the app on it.
fn ask(start: Start, window: WindowHandle<KitRoot>, prompt: &Entity<AccountPrompt>, cx: &mut App) {
    let launched = Rc::new(std::cell::Cell::new(false));
    let own = OwnPrompt {
        setup: start.setup.clone(),
        sign_ins: SignIns::app(),
        again: None,
        signed: Rc::new(move |account, prompt, cx| {
            if launched.replace(true) {
                return;
            }
            if let Some(unserved) = ngteams::accounts::unserved(&start.setup, &account) {
                prompt.update(cx, |prompt, cx| prompt.failed(unserved, cx));
                return;
            }
            launch_on(&start, account, window, cx);
        }),
    };
    cx.subscribe(prompt, move |prompt, event, cx| {
        own.handle(event, prompt, cx);
    })
    .detach();
    say_prompt(cx);
    quit_when_asked(cx);
    let _ = window.update(cx, |_, window, _| {
        after_frames(window, READY_FRAMES, |_, _| say("prompt-ready"));
    });
}

/// The account's engine started as `ngteams` starts it, its calls on the file devices, and the app's window on it;
/// the sign-in screen closes.
fn launch_on(start: &Start, account: PersonalAccount, prompt: WindowHandle<KitRoot>, cx: &mut App) {
    let devices: Arc<dyn Devices + Send + Sync> = Arc::new(FileDevices {
        input: AudioInput::Tone(Tone {
            hz: start.tone,
            amplitude: 0.3,
        }),
        recording: None,
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    });
    let (engine_calls, calls) = ngteams::calling::on_devices(devices, true);
    let floor = CallFloor::new();
    let secrets = start.setup.secrets();
    let engine = match ngteams::engine::start(
        &start.setup.paths,
        &account,
        floor.clone(),
        secrets,
        true,
        Some(engine_calls),
    ) {
        Ok(engine) => engine,
        Err(error) => return fail(start, format!("the account didn't start: {error}"), cx),
    };
    let requests = engine.handle.requests();
    let mri = account.mri().map(|mri| mri.as_str().to_owned()).unwrap_or_default();
    let first = Started {
        account,
        engine,
        calls: Some(calls),
    };
    let launched = app::launch(
        cx,
        Launch {
            first,
            others: Vec::new(),
            trace: Arc::clone(&start.trace),
            ctl: Some(start.ctl.clone()),
            settings: SettingsModel::load(start.settings.clone()),
            bus: teams_ui::bus::address_from_env(),
            sign_in_again: Rc::new(|_| Rc::new(|_| {})),
            runner: Runner {
                setup: start.setup.clone(),
                floor,
                devices: None,
            },
            read_only: false,
        },
    );
    match launched {
        Ok(launched) => {
            say(&format!("signed-in {mri}"));
            if let Some(extra) = start.extra {
                extra(launched.desktop.clone(), cx);
            }
            theme_when_asked(launched.desktop, cx);
            probe_when_asked(requests, start.origin.clone(), cx);
            let _ = prompt.update(cx, |_, window, _| window.remove_window());
            let _ = launched.window.update(cx, |_, window, _| {
                after_frames(window, READY_FRAMES, |_, _| say("ready"));
            });
        }
        Err(failed) => {
            *start.failed.lock().unwrap_or_else(PoisonError::into_inner) = Some(failed.error);
            app::quit_after(failed.stopped, cx);
        }
    }
}

fn fail(start: &Start, error: String, cx: &mut App) {
    say(&error);
    *start.failed.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
    cx.quit();
}

/// Runs `done` once `frames` more frames of `window` have been presented.
pub fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

/// The sign-in screen, if it is open, and whether it has the keyboard.
fn open_prompt(cx: &mut App) -> Option<(Entity<AccountPrompt>, bool)> {
    let (window, prompt) = cx
        .windows()
        .into_iter()
        .filter_map(|window| window.downcast::<KitRoot>())
        .find_map(|window| {
            let prompt = window.read(cx).ok()?.view().clone().downcast::<AccountPrompt>().ok()?;
            Some((window, prompt))
        })?;
    let active = window.is_active(cx).unwrap_or(false);
    Some((prompt, active))
}

/// What the sign-in screen holds, on one line (never the password).
fn described(cx: &mut App) -> String {
    let Some((prompt, active)) = open_prompt(cx) else {
        return "closed".to_owned();
    };
    let prompt = prompt.read(cx);
    let kind = match prompt.kind() {
        SignInKind::Microsoft => "microsoft",
        SignInKind::OwnServer => "own",
    };
    let (server, user) = prompt.own_fields(cx);
    let problem = prompt.problem().map_or_else(|| "-".to_owned(), ToString::to_string);
    format!(
        "active={active} kind={kind} signing={} server={server} user={user} problem={problem}",
        prompt.is_signing_in()
    )
}

/// Says `prompt <state>` whenever the sign-in screen changes, and `prompt-now <state>` when the parent asks.
pub fn say_prompt(cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        return;
    };
    let ask = PathBuf::from(dir).join(PROMPT_ASK);
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut last = "closed".to_owned();
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let now = cx.update(described);
            if now != last {
                say(&format!("prompt {now}"));
                last.clone_from(&now);
            }
            if fs::remove_file(&ask).is_ok() {
                say(&format!("prompt-now {now}"));
            }
        }
    })
    .detach();
}

/// Each time the parent writes [`PROBE`]: reads the picture or the file it names as the window does, and says what it
/// got (`probe picture bytes=<n> png=<bool>`, `probe file name=<name> bytes=<n> sum=<n>`, or `probe failed <why>`).
fn probe_when_asked(requests: EngineRequests, origin: String, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        return;
    };
    let trigger = PathBuf::from(dir).join(PROBE);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(line) = fs::read_to_string(&trigger) else {
                continue;
            };
            let _ = fs::remove_file(&trigger);
            let (requests, origin) = (requests.clone(), origin.clone());
            let answer = cx
                .background_spawn(async move { probed(&requests, &origin, &line).await })
                .await;
            say(&format!(
                "probe {}",
                answer.unwrap_or_else(|error| format!("failed {error}"))
            ));
        }
    })
    .detach();
}

async fn probed(requests: &EngineRequests, origin: &str, line: &str) -> Result<String, String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let [what, chat, id] = words.as_slice() else {
        return Err(format!("not a probe: {line}"));
    };
    let message = requests
        .message(*chat, *id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or("no such message")?;
    match *what {
        "picture" => {
            let src = message
                .content
                .split("src=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .ok_or("no picture in the message")?
                .replace("&amp;", "&");
            let picture = requests
                .message_picture(src)
                .await
                .map_err(|error| error.to_string())?
                .ok_or("no picture read")?;
            let png = picture.bytes.starts_with(b"\x89PNG");
            Ok(format!("picture bytes={} png={png}", picture.bytes.len()))
        }
        "file" => downloaded(requests, origin, &message).await,
        _ => Err(format!("not a probe: {line}")),
    }
}

/// The message's one file downloaded whole, as the window streams it.
async fn downloaded(requests: &EngineRequests, origin: &str, message: &teams_store::Message) -> Result<String, String> {
    let properties: serde_json::Value =
        serde_json::from_str(message.properties.as_deref().unwrap_or("{}")).map_err(|error| error.to_string())?;
    let cloud = teams_proto::Cloud::own(origin).map_err(|error| error.to_string())?;
    let files = teams_proto::chat::shared_files_in(&cloud, properties.get("files"));
    let file = files
        .files
        .first()
        .ok_or_else(|| format!("no file in the message: {:?}", files.unread))?;
    let share = file.share_url.clone().ok_or("the file has no link")?;
    let size = requests
        .file_size(share.clone())
        .await
        .map_err(|error| error.to_string())?;
    let mut stream = requests
        .file_stream(share, 0, size)
        .await
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    while let Some(piece) = stream.next().await {
        bytes.extend_from_slice(&piece.map_err(|error| error.to_string())?);
    }
    let sum: u64 = bytes.iter().map(|byte| u64::from(*byte)).sum();
    Ok(format!(
        "file name={} bytes={} sum={sum}",
        file.name.replace(' ', "_"),
        bytes.len()
    ))
}

/// Each time the parent writes [`THEME_TRIGGER`]: the settings take the theme it names.
fn theme_when_asked(desktop: Entity<teams_ui::Desktop>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        return;
    };
    let trigger = PathBuf::from(dir).join(THEME_TRIGGER);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(key) = fs::read_to_string(&trigger) else {
                continue;
            };
            let _ = fs::remove_file(&trigger);
            let theme = if key == teams_ui::theme::DARK_KEY {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            cx.update(|cx| fixed_theme(&desktop, theme, cx));
        }
    })
    .detach();
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

/// Once the parent writes [`QUIT_TRIGGER`]: the app quits (its engines shut down first).
pub fn quit_when_asked(cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        return;
    };
    let trigger = PathBuf::from(dir).join(QUIT_TRIGGER);
    cx.spawn(async move |cx: &mut AsyncApp| {
        while !trigger.exists() {
            cx.background_executor().timer(Duration::from_millis(100)).await;
        }
        cx.update(app::quit);
    })
    .detach();
}
