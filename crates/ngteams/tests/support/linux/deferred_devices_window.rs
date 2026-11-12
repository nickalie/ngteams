//! Calls while the app's call devices are still being discovered, and after discovery failed, in the UI sandbox on
//! the mock (`harness = false`: GPUI runs on the process's main thread).
//!
//! Each child is lab account a's or b's app (`ngteams::app::launch`) over an engine on the mock with its calling, its
//! devices the app's deferred devices ([`DeferredDevices`]) over the sandbox's private PipeWire, as `ngteams` opens
//! them. Its discovery waits for the parent: the sandbox file [`GATE`] saying `ready` (PipeWire is connected) or
//! `fail` (discovery fails). Driven by each sandbox's keyboard and notifications, read by the view traces, a probe
//! of the app's models and each sandbox's recordings:
//!
//! - `pending`: a (its USB microphone remembered in its settings) and b open their windows and chat lists while
//!   discovery waits. a calls b: its call window says Preparing…, nothing reaches the mock or b, and a cancels it.
//!   Settings → Calls and devices says "Preparing media…". a calls b again; a's discovery ends and the call rings at b,
//!   whose own discovery still waits; b answers from its notification: its ring stops and its banner says Answering
//!   and Preparing… with Decline alone, and the call connects once b's discovery ends.
//!   b hears a's remembered USB microphone, a hears b. Settings then lists the devices.
//! - `failure`: b calls a while a's discovery waits; a's discovery fails: a's ring goes, the chat has no call button,
//!   Settings says why calls aren't available and what to do in a warning box, a's call key does nothing, and a still receives and sends messages.
//!
//! `NGTEAMS_DEFERRED_SCENARIO=pending` or `failure` runs one. Screenshots go to `target/ui-shots/deferred-*.png`.
//! Without weston 15, niri, `dbus-daemon`, PipeWire or WirePlumber it says so and passes, unless
//! `GPUI_SMOKE_REQUIRE=1`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use std::{fs, future, io, thread};

use gpui::{App, AsyncApp, WindowingModes};
use ngteams::app::{self, Launch};
#[path = "../window.rs"]
mod support;
use ngteams::calling::PersonalCalls;
use ngteams::descriptors::adopt_trace;
use support::Alias;
use support::display_name;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::account::web_client_info;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{CallMedia, CallParts, Identity, Outbox, Relay};
use teams_media::devices::catalog::{DeviceDirectory as _, DeviceKind};
use teams_media::devices::deferred::DeferredDevices;
use teams_media::devices::pipewire::{PipeWireDevices, Reach};
use teams_media::devices::{DeviceError, Devices, SAMPLE_RATE, wav};
use teams_media::processing::ProcessingConfig;
use teams_media::relay::RelayServer;
use teams_media::session::SessionConfig;
use teams_media::tone::{Tone, amplitude_at};
use teams_mock::{AB_CHAT, CHAT_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, LAB_C, TENANT, TrouterMock};
use teams_proto::Mri;
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use teams_ui::Desktop;
use teams_ui::settings::Settings;
use teams_ui::settings_view::Page;
use teams_ui::sounds::SoundBoard;
use teams_ui::view::Root;
use teams_ui::{Ringer, SettingsFile, VideoFeed};
use tokio::sync::broadcast;
use ui_sandbox::input::{Modifier, VirtualInput, keys};
use ui_sandbox::pipewire::nodes::NodeSpec;
use ui_sandbox::pipewire::{DEVICE_PRIORITY, PipeWireOptions};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

const CHILD_ENV: &str = "NGTEAMS_DEFERRED_TEST";
const TRACE_FD_ENV: &str = "NGTEAMS_DEFERRED_TRACE_FD";
const ORIGIN_ENV: &str = "NGTEAMS_DEFERRED_ORIGIN";
const TROUTER_ENV: &str = "NGTEAMS_DEFERRED_TROUTER";
const RELAY_ENV: &str = "NGTEAMS_DEFERRED_RELAY";
const IC3_ENV: &str = "NGTEAMS_DEFERRED_IC3";
const SKYPE_ENV: &str = "NGTEAMS_DEFERRED_SKYPE";
const DIR_ENV: &str = "NGTEAMS_DEFERRED_DIR";
/// The microphone the child's settings remember from an earlier run; unset: none.
const MICROPHONE_ENV: &str = "NGTEAMS_DEFERRED_MICROPHONE";
const SCENARIO_ENV: &str = "NGTEAMS_DEFERRED_SCENARIO";
/// The sandbox file whose `ready` or `fail` ends the child's device discovery.
const GATE: &str = "discovery";
/// The sandbox file the parent writes one ask to ([`ask`]); the child answers `did <ask>`.
const ASK: &str = "ask";
/// The longest the child's discovery waits for its gate.
const GATE_WAIT: Duration = Duration::from_secs(600);
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(60);
/// How long nothing is watched for, where nothing should happen.
const QUIET_FOR: Duration = Duration::from_secs(3);
const READY_FRAMES: usize = 3;
const TALK: Duration = Duration::from_secs(4);
const A_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.5,
};
const B_TONE: Tone = Tone {
    hz: 660.0,
    amplitude: 0.5,
};
/// a's USB microphone, the one its settings remember.
const USB_TONE: Tone = Tone {
    hz: 550.0,
    amplitude: 0.5,
};
const USB_MIC: &str = "sandbox-usb-mic";
const HEARD: f32 = 0.1;
const QUIET: f32 = 0.03;
const B_SAYS: &str = "b still reaches a";
const A_SAYS: &str = "a still sends";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("a") => child(Alias::A),
        Ok("b") => child(Alias::B),
        Ok(other) => Err(format!("unknown account {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("deferred devices window test: {error}");
            ExitCode::FAILURE
        }
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

// --- the parent ---

fn sandbox_options(tone: Tone) -> SandboxOptions {
    SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        pipewire: Some(PipeWireOptions {
            tone,
            ..PipeWireOptions::default()
        }),
        ..SandboxOptions::default()
    }
}

fn parent() -> SandboxResult {
    let scenario = std::env::var(SCENARIO_ENV).unwrap_or_default();
    if scenario.is_empty() || scenario == "pending" {
        let Some(boxes) = sandboxes()? else {
            return Ok(());
        };
        run(&boxes, pending)?;
        say("deferred devices: pending: ok");
    }
    if scenario.is_empty() || scenario == "failure" {
        let Some(boxes) = sandboxes()? else {
            return Ok(());
        };
        run(&boxes, failure)?;
        say("deferred devices: failure: ok");
    }
    Ok(())
}

fn sandboxes() -> SandboxResult<Option<(Sandbox, Sandbox)>> {
    let Some(a_box) = sandbox_or_skip("ngteams deferred devices", &sandbox_options(A_TONE))? else {
        return Ok(None);
    };
    let Some(b_box) = sandbox_or_skip("ngteams deferred devices", &sandbox_options(B_TONE))? else {
        return Ok(None);
    };
    Ok(Some((a_box, b_box)))
}

/// The mock and its services for one scenario.
struct World {
    mock: ChatServiceMock,
    lab: MockLab,
    _trouter: TrouterMock,
    _dir: tempfile::TempDir,
    _runtime: tokio::runtime::Runtime,
}

impl World {
    fn start() -> SandboxResult<Self> {
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
        let lab = MockLab {
            origin: mock.origin(),
            trouter: trouter.origin().to_owned(),
            relay: relay.to_string(),
            dir: dir.path().to_owned(),
        };
        Ok(Self {
            mock,
            lab,
            _trouter: trouter,
            _dir: dir,
            _runtime: runtime,
        })
    }
}

type Scenario = fn(&World, &mut Apps, (&Sandbox, &Sandbox)) -> SandboxResult;

/// The two apps of a scenario, started by it.
#[derive(Default)]
struct Apps {
    a: Option<AppUnderTest>,
    b: Option<AppUnderTest>,
}

impl Apps {
    fn a(&mut self) -> SandboxResult<&mut AppUnderTest> {
        Ok(self.a.as_mut().ok_or("a isn't running")?)
    }

    fn b(&mut self) -> SandboxResult<&mut AppUnderTest> {
        Ok(self.b.as_mut().ok_or("b isn't running")?)
    }
}

fn run((a_box, b_box): &(Sandbox, Sandbox), scenario: Scenario) -> SandboxResult {
    let world = World::start()?;
    let mut apps = Apps::default();
    let outcome = scenario(&world, &mut apps, (a_box, b_box));
    if let Err(error) = outcome {
        let a = apps
            .a
            .as_mut()
            .map_or_else(String::new, |app| app.running.fail(&"killed").to_string());
        let b = apps
            .b
            .as_mut()
            .map_or_else(String::new, |app| app.running.fail(&"killed").to_string());
        return Err(format!(
            "{error}\n--- a: {a}\n--- b: {b}\n--- a's sandbox{}\n--- b's sandbox{}",
            a_box.logs(),
            b_box.logs()
        )
        .into());
    }
    if let Some(a) = apps.a.take() {
        a.quit(a_box)?;
    }
    if let Some(b) = apps.b.take() {
        b.quit(b_box)?;
    }
    Ok(())
}

/// Where the children reach the mock.
struct MockLab {
    origin: String,
    trouter: String,
    relay: String,
    dir: PathBuf,
}

/// A child app in its sandbox: its process, its trace.
struct AppUnderTest {
    alias: Alias,
    running: Running,
    trace: TraceReader,
    asked: usize,
}

/// What the app's models say ([`probe`]).
#[derive(Debug, PartialEq, Eq)]
struct Probe {
    button: String,
    media: String,
    devices: usize,
    microphone: String,
}

impl AppUnderTest {
    fn start(sandbox: &Sandbox, world: &World, alias: Alias, microphone: Option<&str>) -> SandboxResult<Self> {
        let lab = &world.lab;
        let mri = mri_of(alias);
        let dir = lab.dir.join(alias.to_string());
        fs::create_dir_all(&dir)?;
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let ic3 = world.mock.token(mri).ok_or("the mock's token")?;
        let skype = world.mock.skypetoken(mri).ok_or("the mock's skypetoken")?;
        let account = alias.to_string();
        let own = dir.to_string_lossy().into_owned();
        let child = sandbox.spawn_with_fds(
            &std::env::current_exe()?,
            &[],
            &[
                (CHILD_ENV, account.as_str()),
                (TRACE_FD_ENV, &fd),
                (ORIGIN_ENV, &lab.origin),
                (TROUTER_ENV, &lab.trouter),
                (RELAY_ENV, &lab.relay),
                (IC3_ENV, &ic3),
                (SKYPE_ENV, &skype),
                (DIR_ENV, &own),
                (MICROPHONE_ENV, microphone.unwrap_or_default()),
            ],
            &[writer.as_fd()],
        )?;
        drop(writer);
        let running = Running::new(child)?;
        let mut app = Self {
            alias,
            running,
            trace: TraceReader::start(reader),
            asked: 0,
        };
        let ready = app
            .running
            .line_where(STARTUP, |line| line.starts_with("ready "))
            .map_err(|error| app.running.fail(&format!("{alias}: no ready line: {error:?}")))?;
        say(&format!("{alias}: window presented: {ready}"));
        app.trace.wait_for("status", "connected")?;
        app.trace.next_where(&format!("{alias}: the a–b chat listed"), |line| {
            line["view"] == "chat-list" && line.to_string().contains(AB_CHAT)
        })?;
        Ok(app)
    }

    /// Asks the app `what` ([`answer_asks`]) and waits for its answer: the line after `did`.
    fn ask(&mut self, sandbox: &Sandbox, what: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = sandbox.path("ask.staged");
        fs::write(&staged, format!("{} {what}", self.asked))?;
        fs::rename(&staged, sandbox.path(ASK))?;
        let did = format!("did {} {what}", self.asked);
        let line = self
            .running
            .line_where(WAIT, |line| line.starts_with(&did))
            .map_err(|error| self.running.fail(&format!("{}: no {did}: {error:?}", self.alias)))?;
        Ok(line.trim_start_matches(&did).trim().to_owned())
    }

    fn probe(&mut self, sandbox: &Sandbox) -> SandboxResult<Probe> {
        let line = self.ask(sandbox, "probe")?;
        let field = |name: &str| {
            line.split(' ')
                .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
                .unwrap_or_default()
                .to_owned()
        };
        let probe = Probe {
            button: field("button"),
            media: field("media"),
            devices: field("devices").parse().unwrap_or_default(),
            microphone: field("microphone"),
        };
        say(&format!("{}: {probe:?}", self.alias));
        Ok(probe)
    }

    fn in_call(&mut self, kinds: &[&str]) -> SandboxResult<serde_json::Value> {
        let what = format!("{}: in-call shown {kinds:?}", self.alias);
        self.trace.next_where(&what, |line| {
            line["view"] == "in-call"
                && line["change"] == "shown"
                && kinds
                    .iter()
                    .all(|kind| strings(line, "kinds").iter().any(|shown| shown == kind))
        })
    }

    fn main_window_active(&mut self) -> SandboxResult {
        let activation = |line: &serde_json::Value| {
            line["view"] == "window" && (line["change"] == "activated" || line["change"] == "deactivated")
        };
        let what = format!("{}: window activated", self.alias);
        loop {
            let last = self
                .trace
                .catch_up()
                .iter()
                .rev()
                .find(|line| activation(line) || (line["view"] == "call-window" && line["change"] == "closed"))
                .cloned();
            if last.is_some_and(|line| line["view"] == "window" && line["change"] == "activated") {
                return Ok(());
            }
            self.trace
                .next_where(&what, |line| line["view"] == "window" && line["change"] == "activated")?;
        }
    }

    fn call_windows(&mut self, change: &str, count: usize) -> SandboxResult {
        let is = |line: &serde_json::Value| line["view"] == "call-window" && line["change"] == change;
        let what = format!("{}: call-window {change} #{count}", self.alias);
        while self.trace.seen.iter().filter(|line| is(line)).count() < count {
            self.trace.next_where(&what, is)?;
        }
        let told = self.trace.seen.iter().filter(|line| is(line)).count();
        if told > count {
            return Err(format!("{what}: told {told}").into());
        }
        Ok(())
    }

    /// The trace lines that came in the next [`QUIET_FOR`].
    fn lines_while_quiet(&mut self) -> Vec<serde_json::Value> {
        let before = self.trace.catch_up().len();
        thread::sleep(QUIET_FOR);
        self.trace.catch_up()[before..].to_vec()
    }

    fn quit(mut self, sandbox: &Sandbox) -> SandboxResult {
        self.asked += 1;
        fs::write(sandbox.path("ask.staged"), format!("{} quit", self.asked))?;
        fs::rename(sandbox.path("ask.staged"), sandbox.path(ASK))?;
        self.running
            .line_where(WAIT, |line| line == "quit")
            .map_err(|error| self.running.fail(&format!("{}: didn't quit: {error:?}", self.alias)))?;
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("{}: exited {status}: {stderr}", self.alias).into());
        }
        Ok(())
    }
}

fn mri_of(alias: Alias) -> &'static str {
    match alias {
        Alias::A => LAB_A,
        Alias::B => LAB_B,
        Alias::C => LAB_C,
    }
}

/// Ends the discovery of the app in `sandbox`: `ready` or `fail`.
fn gate(sandbox: &Sandbox, outcome: &str) -> SandboxResult {
    let staged = sandbox.path("discovery.staged");
    fs::write(&staged, outcome)?;
    fs::rename(&staged, sandbox.path(GATE))?;
    Ok(())
}

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

    fn wait_for(&mut self, view: &str, change: &str) -> SandboxResult<serde_json::Value> {
        let what = format!("{view} {change}");
        if let Some(line) = self
            .seen
            .iter()
            .find(|line| line["view"] == view && line["change"] == change)
        {
            return Ok(line.clone());
        }
        self.next_where(&what, |line| line["view"] == view && line["change"] == change)
    }

    fn next_where(
        &mut self,
        what: &str,
        found: impl Fn(&serde_json::Value) -> bool,
    ) -> SandboxResult<serde_json::Value> {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .map_err(|_| format!("no trace line {what}; seen {:?}", self.seen))?;
            let hit = found(&line);
            self.seen.push(line.clone());
            if hit {
                return Ok(line);
            }
        }
    }
}

fn strings(line: &serde_json::Value, key: &str) -> Vec<String> {
    line[key]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn ctrl_shift(input: &mut VirtualInput, key: char) -> SandboxResult {
    let (code, _) = input.keymap().key_for(key).ok_or("not on the US keymap")?;
    input.key_with(&[Modifier::Ctrl, Modifier::Shift], code)
}

fn loudest(path: &Path, hz: f32, stretch: Duration) -> SandboxResult<f32> {
    let samples = wav::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let per_ms = usize::try_from(SAMPLE_RATE / 1_000)?;
    let window = 400 * per_ms;
    let wanted = usize::try_from(stretch.as_millis())? * per_ms;
    let tail = &samples[samples.len().saturating_sub(wanted)..];
    if tail.len() < window {
        return Err(format!("{} holds {} samples", path.display(), samples.len()).into());
    }
    Ok((0..=tail.len() - window)
        .step_by(window / 4)
        .map(|at| amplitude_at(&tail[at..at + window], hz))
        .fold(0.0, f32::max))
}

fn recording(sandbox: &Sandbox) -> SandboxResult<PathBuf> {
    Ok(sandbox.pipewire().ok_or("no PipeWire")?.recording())
}

/// Waits until the recording at `path` holds `wanted` and not `other` at its end.
fn hears_alone(path: &Path, wanted: f32, other: f32, what: &str) -> SandboxResult {
    let alone = wait_until(WAIT, || {
        let heard = loudest(path, wanted, Duration::from_secs(1)).ok()?;
        let left = loudest(path, other, Duration::from_millis(600)).ok()?;
        (heard >= HEARD && left < QUIET).then_some((heard, left))
    });
    match alone {
        Some((heard, left)) => {
            say(&format!("{what}: {wanted} Hz at {heard:.3} ({other} Hz at {left:.3})"));
            Ok(())
        }
        None => Err(format!(
            "{what}: {wanted} Hz isn't heard alone (at {:?}, {other} Hz at {:?})",
            loudest(path, wanted, Duration::from_secs(1)).ok(),
            loudest(path, other, Duration::from_millis(600)).ok()
        )
        .into()),
    }
}

/// Screenshots the focused window (`window`) or the screen of `sandbox` into `target/ui-shots/<name>`.
fn shot_of(sandbox: &Sandbox, name: &str, window: bool) -> SandboxResult {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    thread::sleep(Duration::from_millis(700));
    let png = sandbox.screenshot(name, window)?;
    fs::copy(&png, dir.join(name))?;
    Ok(())
}

/// Screenshots `name` in Teams Light, then `name-dark` in Teams Dark, and turns Light back on.
fn shots_light_and_dark(app: &mut AppUnderTest, sandbox: &Sandbox, name: &str, window: bool) -> SandboxResult {
    shot_of(sandbox, &format!("{name}.png"), window)?;
    app.ask(sandbox, "dark")?;
    shot_of(sandbox, &format!("{name}-dark.png"), window)?;
    app.ask(sandbox, "light")?;
    Ok(())
}

fn call_notification(sandbox: &Sandbox, seen: usize) -> SandboxResult<u32> {
    wait_until(WAIT, || {
        sandbox
            .bus()
            .notifications()
            .into_iter()
            .filter(|notification| notification.actions.iter().any(|action| action == "answer"))
            .nth(seen)
    })
    .map(|notification| notification.id)
    .ok_or_else(|| format!("no incoming call's notification: {:?}", sandbox.bus().notifications()).into())
}

fn requests_of(world: &World, account: &str) -> Vec<String> {
    world
        .mock
        .calls()
        .iter()
        .flat_map(|call| call.requests.iter())
        .filter(|(_, from)| from == account)
        .map(|(op, _)| op.clone())
        .collect()
}

fn expect_probe(probe: &Probe, media: &str, what: &str) -> SandboxResult {
    if probe.media != media {
        return Err(format!("{what}: media {}, not {media}: {probe:?}", probe.media).into());
    }
    Ok(())
}

// --- the pending scenario ---

fn pending(world: &World, apps: &mut Apps, (a_box, b_box): (&Sandbox, &Sandbox)) -> SandboxResult {
    let pipewire = a_box.pipewire().ok_or("no PipeWire")?;
    let usb = pipewire.add_microphone(
        NodeSpec {
            name: USB_MIC.to_owned(),
            description: "USB microphone".to_owned(),
            priority: DEVICE_PRIORITY - 1_000,
        },
        USB_TONE,
    )?;
    apps.a = Some(AppUnderTest::start(a_box, world, Alias::A, Some(USB_MIC))?);
    apps.b = Some(AppUnderTest::start(b_box, world, Alias::B, None)?);
    let a = apps.a()?;
    // the window and the chat list came while discovery waits for its gate
    expect_probe(&a.probe(a_box)?, "Preparing", "a's window before discovery ended")?;
    shot_of(a_box, "deferred-window-pending.png", true)?;
    expect_probe(
        &apps.b()?.probe(b_box)?,
        "Preparing",
        "b's window before discovery ended",
    )?;

    cancelled_while_preparing(world, apps, (a_box, b_box))?;
    settings_while_preparing(apps.a()?, a_box)?;
    answered_across_discovery(world, apps, (a_box, b_box))?;

    let a = apps.a()?;
    let ready = a.probe(a_box)?;
    expect_probe(&ready, "Ready", "a after discovery")?;
    if ready.microphone != USB_MIC || ready.devices == 0 {
        return Err(format!("a's remembered microphone or devices after discovery: {ready:?}").into());
    }
    a.ask(a_box, "calls")?;
    shot_of(a_box, "deferred-settings-ready.png", true)?;
    drop(usb);
    Ok(())
}

/// a calls b during discovery: Preparing…, nothing sent; a cancels it.
fn cancelled_while_preparing(world: &World, apps: &mut Apps, (a_box, _): (&Sandbox, &Sandbox)) -> SandboxResult {
    let mut a_keys = a_box.input()?;
    let a = apps.a()?;
    let button = a.trace.wait_for("call-button", "shown")?;
    say(&format!("a's call button during discovery: {button}"));
    ctrl_shift(&mut a_keys, 'c')?;
    a.in_call(&["chat", "preparing"])?;
    a.call_windows("opened", 1)?;
    shots_light_and_dark(a, a_box, "deferred-call-preparing", true)?;
    shot_of(a_box, "deferred-call-preparing-screen.png", false)?;
    let sent = requests_of(world, LAB_A);
    if !sent.is_empty() {
        return Err(format!("a's preparing call sent {sent:?}").into());
    }
    ctrl_shift(&mut a_keys, 'h')?;
    a.call_windows("closed", 1)?;
    let after = a.lines_while_quiet();
    if let Some(odd) = after
        .iter()
        .find(|line| line["view"] == "in-call" && strings(line, "kinds").iter().any(|kind| kind == "ringing"))
    {
        return Err(format!("a's cancelled call went on: {odd}").into());
    }
    let sent = requests_of(world, LAB_A);
    if !sent.is_empty() {
        return Err(format!("a's cancelled call sent {sent:?}").into());
    }
    let b = apps.b()?;
    if let Some(banner) = b
        .trace
        .catch_up()
        .iter()
        .find(|line| line["view"] == "call-banner" && line["change"] == "shown")
    {
        return Err(format!("b was rung by a's cancelled call: {banner}").into());
    }
    say("a's call during discovery: Preparing…, cancelled, nothing sent");
    Ok(())
}

/// Settings → Calls and devices during discovery: "Preparing media…".
fn settings_while_preparing(a: &mut AppUnderTest, a_box: &Sandbox) -> SandboxResult {
    a.main_window_active()?;
    a.ask(a_box, "calls")?;
    a.trace.wait_for("settings", "opened")?;
    shots_light_and_dark(a, a_box, "deferred-settings-preparing", true)?;
    let probe = a.probe(a_box)?;
    expect_probe(&probe, "Preparing", "a's settings")?;
    if probe.devices != 0 {
        return Err(format!("a's settings list devices during discovery: {probe:?}").into());
    }
    a.ask(a_box, "chat")?;
    Ok(())
}

/// a calls b during discovery; a's discovery ends and b is rung while its own still runs; b answers, and the call
/// connects once b's discovery ends, on a's remembered microphone.
fn answered_across_discovery(world: &World, apps: &mut Apps, (a_box, b_box): (&Sandbox, &Sandbox)) -> SandboxResult {
    let mut a_keys = a_box.input()?;
    let a = apps.a()?;
    a.main_window_active()?;
    ctrl_shift(&mut a_keys, 'c')?;
    a.in_call(&["chat", "preparing"])?;
    a.call_windows("opened", 2)?;
    gate(a_box, "ready")?;
    a.in_call(&["chat", "ringing"])?;
    say("a's waiting call went on once a's discovery ended");

    let b = apps.b()?;
    let banner = b.trace.wait_for("call-banner", "shown")?;
    if banner["people"] != serde_json::json!([LAB_A]) {
        return Err(format!("b's banner names another caller: {banner}").into());
    }
    b.trace.wait_for("call-notification", "shown")?;
    expect_probe(&b.probe(b_box)?, "Preparing", "b rung during its discovery")?;
    shot_of(b_box, "deferred-incoming-pending.png", true)?;
    let ringing = call_notification(b_box, 0)?;
    b_box.bus().invoke_action(ringing, "answer")?;
    b.trace.wait_for("call-notification", "answered")?;
    // the answer waits for b's discovery: the banner says so at once, and the ring stops
    b.trace.wait_for("call-banner", "answering")?;
    b.trace.wait_for("call-notification", "closed")?;
    let waiting = b.lines_while_quiet();
    say(&format!("b's lines while its answer waits: {waiting:?}"));
    if let Some(odd) = waiting
        .iter()
        .find(|line| line["view"] == "call-banner" && line["change"] == "hidden")
    {
        return Err(format!("b's banner went before its answer could go: {odd}").into());
    }
    shots_light_and_dark(b, b_box, "deferred-answer-pending", true)?;
    let sent = requests_of(world, LAB_B);
    if sent.iter().any(|op| op == "accept") {
        return Err(format!("b accepted before its discovery ended: {sent:?}").into());
    }
    gate(b_box, "ready")?;
    b.in_call(&["incoming", "connected"])?;
    apps.a()?.in_call(&["chat", "connected"])?;
    shot_of(b_box, "deferred-answered.png", true)?;
    thread::sleep(TALK);
    hears_alone(
        &recording(b_box)?,
        USB_TONE.hz,
        A_TONE.hz,
        "b hears a's remembered USB microphone",
    )?;
    let a_heard = loudest(&recording(a_box)?, B_TONE.hz, Duration::from_secs(2))?;
    say(&format!("a heard b at {a_heard:.3}"));
    if a_heard < HEARD {
        return Err(format!("a doesn't hear b ({a_heard:.3})").into());
    }
    ctrl_shift(&mut a_keys, 'h')?;
    apps.a()?.in_call(&["ended", "hung-up"])?;
    apps.b()?.in_call(&["ended", "remote"])?;
    let a = apps.a()?;
    a.call_windows("closed", 2)?;
    a.main_window_active()?;
    Ok(())
}

// --- the failure scenario ---

fn failure(world: &World, apps: &mut Apps, (a_box, b_box): (&Sandbox, &Sandbox)) -> SandboxResult {
    gate(b_box, "ready")?;
    apps.a = Some(AppUnderTest::start(a_box, world, Alias::A, None)?);
    apps.b = Some(AppUnderTest::start(b_box, world, Alias::B, None)?);
    expect_probe(&apps.a()?.probe(a_box)?, "Preparing", "a before its discovery failed")?;

    // b rings a while a's discovery runs; then a's discovery fails
    let mut b_keys = b_box.input()?;
    let b = apps.b()?;
    b.trace.wait_for("calling", "registered")?;
    b.trace.wait_for("call-button", "shown")?;
    ctrl_shift(&mut b_keys, 'c')?;
    b.in_call(&["chat", "ringing"])?;
    let a = apps.a()?;
    let banner = a.trace.wait_for("call-banner", "shown")?;
    say(&format!("a rung during its discovery: {banner}"));
    let notification = a.trace.wait_for("call-notification", "shown");
    say(&format!("a's call notification during discovery: {notification:?}"));
    shot_of(a_box, "deferred-incoming-before-failure.png", true)?;
    gate(a_box, "fail")?;
    a.trace.wait_for("call-banner", "hidden")?;
    let failed = a.probe(a_box)?;
    expect_probe(&failed, "Unavailable", "a after its discovery failed")?;
    if failed.button != "none" {
        return Err(format!("a's chat still offers a call: {failed:?}").into());
    }
    let closed = a_box.bus().closed_notifications();
    say(&format!("a's notifications closed after the failure: {closed:?}"));
    shots_light_and_dark(a, a_box, "deferred-failed-window", true)?;
    ctrl_shift(&mut b_keys, 'h')?;
    apps.b()?.in_call(&["ended"])?;

    let a = apps.a()?;
    a.ask(a_box, "calls")?;
    a.trace.wait_for("settings", "opened")?;
    shots_light_and_dark(a, a_box, "deferred-settings-unavailable", true)?;
    a.ask(a_box, "chat")?;
    a.main_window_active()?;
    no_call_after_failure(world, apps, a_box)?;
    chats_after_failure(world, apps, (a_box, b_box))
}

/// a's call key does nothing once discovery failed.
fn no_call_after_failure(world: &World, apps: &mut Apps, a_box: &Sandbox) -> SandboxResult {
    let mut a_keys = a_box.input()?;
    // a's ring replies to b's earlier call (attach, progress) went before; nothing new may go
    let before = requests_of(world, LAB_A);
    let a = apps.a()?;
    ctrl_shift(&mut a_keys, 'c')?;
    let after = a.lines_while_quiet();
    if let Some(odd) = after
        .iter()
        .find(|line| line["view"] == "in-call" || (line["view"] == "call-window" && line["change"] == "opened"))
    {
        return Err(format!("a's call key opened a call after discovery failed: {odd}").into());
    }
    let sent = requests_of(world, LAB_A);
    let placed = world.mock.calls().iter().filter(|call| call.caller == LAB_A).count();
    if sent != before || placed != 0 {
        return Err(
            format!("a's call key after discovery failed sent {sent:?} (before {before:?}), {placed} calls").into(),
        );
    }
    say(&format!("a's call key after the failure: {after:?}"));
    Ok(())
}

/// a still receives b's message and sends its own, which b sees.
fn chats_after_failure(world: &World, apps: &mut Apps, (a_box, _): (&Sandbox, &Sandbox)) -> SandboxResult {
    let posted = world
        .mock
        .post_as(LAB_B, AB_CHAT, &format!("<p>{B_SAYS}</p>"))
        .ok_or("the mock refused b's message")?;
    let a = apps.a()?;
    a.trace.next_where("a shows b's new message", |line| {
        line["view"] == "chat-view" && line["conversation"] == AB_CHAT && strings(line, "messages").contains(&posted)
    })?;
    let b = apps.b()?;
    let before = b
        .trace
        .catch_up()
        .iter()
        .rev()
        .find(|line| line["view"] == "chat-view" && line["conversation"] == AB_CHAT)
        .map_or(0, |line| strings(line, "messages").len());
    let mut a_keys = a_box.input()?;
    a_keys.type_text(A_SAYS)?;
    a_keys.key(keys::ENTER)?;
    let a = apps.a()?;
    a.trace.next_where("a's message sent", |line| {
        line["view"] == "compose" && line["change"] == "sent" && line["conversation"] == AB_CHAT
    })?;
    shot_of(a_box, "deferred-failed-chat.png", true)?;
    let b = apps.b()?;
    b.trace.next_where("b shows a's message", |line| {
        line["view"] == "chat-view" && line["conversation"] == AB_CHAT && strings(line, "messages").len() > before
    })?;
    say("a received and sent a message after its discovery failed");
    Ok(())
}

// --- the children ---

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

fn engine(
    alias: Alias,
    devices: Arc<dyn Devices + Send + Sync>,
    feed: VideoFeed,
    dir: &Path,
) -> SandboxResult<EngineHandle> {
    let mri = mri_of(alias);
    let tokens = Arc::new(Tokens {
        ic3: env(IC3_ENV)?,
        skypetoken: env(SKYPE_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let tracker = Arc::new(teams_call::Tracker::new(Mri::new(mri)));
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
    let name = display_name(&format!("bot-{alias}@lab.example.test")).to_owned();
    let identity = Identity {
        mri: Mri::new(mri),
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
        },
    };
    let push = Push::new(trouter()?);
    let handle = engine::start(Parts {
        calls: Some(CallParts::new(calling, identity, media)),
        store: Store::open(&support::store_file(dir, alias))?,
        client: Arc::new(Client::new(http()?)),
        outbox: Outbox::new(transport, Author::new(Mri::new(mri), name))
            .with_calls(Arc::clone(&tracker), teams_core::CallFloor::new()),
        push: Some(push),
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })?;
    Ok(handle)
}

/// The app's deferred devices over the sandbox's PipeWire, discovery waiting for the parent's [`GATE`].
fn deferred_devices(sandbox: &Path) -> DeferredDevices {
    let gate = sandbox.join(GATE);
    DeferredDevices::connect(move || {
        let deadline = Instant::now() + GATE_WAIT;
        loop {
            if let Ok(outcome) = fs::read_to_string(&gate) {
                return match outcome.trim() {
                    "ready" => PipeWireDevices::connect(Reach::Sandbox),
                    _ => Err(DeviceError::Refused("synthetic discovery failure".to_owned())),
                };
            }
            if Instant::now() > deadline {
                return Err(DeviceError::Refused("the discovery gate never opened".to_owned()));
            }
            thread::sleep(Duration::from_millis(50));
        }
    })
}

fn child(alias: Alias) -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let started = Instant::now();
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let sandbox = PathBuf::from(std::env::var_os(ui_sandbox::SANDBOX_ENV).ok_or("no sandbox dir")?);
    let deferred = Arc::new(deferred_devices(&sandbox));
    let devices: Arc<dyn Devices + Send + Sync> = deferred.clone();
    let feed = VideoFeed::new();
    let engine = engine(alias, Arc::clone(&devices), feed.clone(), &dir)?;
    let first = support::started(alias, engine)?;
    let runner = support::runner(&dir)?;
    let settings = SettingsFile::new(dir.join("settings.toml"));
    let microphone = std::env::var(MICROPHONE_ENV).ok().filter(|name| !name.is_empty());
    let opened = Settings {
        selected_chat: Some(AB_CHAT.to_owned()),
        microphone,
        ..Settings::default()
    };
    settings.save(&opened).map_err(|problem| format!("{problem:?}"))?;
    let sounds = Arc::new(SoundBoard::new(Arc::clone(&devices)));
    let ringer: Arc<dyn Ringer> = sounds.clone();
    let calls = PersonalCalls {
        ringer: Some(ringer),
        sounds: Some(sounds),
        echo: Mri::new(ECHO),
        video: Some(feed),
        camera: Some(devices),
        devices: Some(deferred.clone()),
    };
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first: ngteams::accounts::Started {
                    calls: Some(calls),
                    ..first
                },
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
                let _ = launched.window.update(cx, |_, window, _| {
                    after_frames(window, READY_FRAMES, move |_, _| {
                        say(&format!("ready {} ms", started.elapsed().as_millis()));
                    });
                });
                answer_asks(launched.desktop, deferred, sandbox.join(ASK), cx);
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

fn after_frames(window: &gpui::Window, frames: usize, done: impl FnOnce(&mut gpui::Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

/// Each ask the parent writes to `trigger` (`<n> <ask>`), answered with `did <n> <ask>` and what it read: `calls`
/// (Settings → Calls and devices, as the rail's Settings opens it), `chat` (the a–b chat), `dark` or `light` (the
/// theme), `probe` (the chat's call button, the media's status, the devices listed and the microphone chosen), `quit`.
fn answer_asks(desktop: gpui::Entity<Desktop>, deferred: Arc<DeferredDevices>, trigger: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(asked) = fs::read_to_string(&trigger) else {
                continue;
            };
            let _ = fs::remove_file(&trigger);
            let what = asked.split_once(' ').map_or("", |(_, what)| what);
            if what == "quit" {
                cx.update(app::quit);
                return;
            }
            let answer = cx.update(|cx| answer(&desktop, &deferred, what, cx));
            say(&format!("did {asked} {answer}"));
        }
    })
    .detach();
}

fn answer(desktop: &gpui::Entity<Desktop>, deferred: &DeferredDevices, what: &str, cx: &mut App) -> String {
    let settings = desktop.read(cx).settings().clone();
    match what {
        "dark" | "light" => {
            let key = if what == "dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            settings.update(cx, |model, cx| {
                model.change(
                    |settings| {
                        settings.appearance.follow_system = false;
                        settings.appearance.theme = key;
                    },
                    cx,
                );
            });
            return String::new();
        }
        "probe" | "calls" | "chat" => {}
        _ => return "unknown".to_owned(),
    }
    let Some(window) = desktop.read(cx).window() else {
        return "no-window".to_owned();
    };
    let mut answer = String::new();
    let _ = window.update(cx, |kit, window, cx| {
        let Some(shell) = kit
            .view()
            .clone()
            .downcast::<Root>()
            .ok()
            .and_then(|root| root.read(cx).shell_in(cx).cloned())
        else {
            "no-shell".clone_into(&mut answer);
            return;
        };
        match what {
            "calls" => shell.update(cx, |shell, cx| shell.open_settings_page(Page::Calls, window, cx)),
            "chat" => shell.update(cx, |shell, cx| shell.open_chat(AB_CHAT.to_owned(), window, cx)),
            _ => {
                let model = settings.read(cx);
                answer = format!(
                    "button={} media={:?} devices={} microphone={}",
                    shell.read(cx).call_button(cx),
                    model.media_status(),
                    model.devices().map_or(0, |list| list.all().len()),
                    deferred.chosen(DeviceKind::Microphone).unwrap_or_default(),
                );
            }
        }
    });
    answer
}
