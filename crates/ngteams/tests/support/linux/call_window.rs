//! Calls in the app's window (M3 Task 17), in the UI sandbox, on the mock (`harness = false`: GPUI runs on the
//! process's main thread).
//!
//! The parent runs the mock chat service with its calling service, its Trouter and its media (a loopback TURN server,
//! `teams-mock`'s `media`), and two UI sandboxes, each with its private PipeWire playing a tone of its own into its
//! microphone (a's 440 Hz, b's 660 Hz) and recording its speaker. In each it runs this test again as lab account a's
//! or b's app (`ngteams::app::launch` over an engine on the mock with its calling: its calling socket on the mock's
//! Trouter, its calls' media on the sandbox's PipeWire relayed through the mock's TURN server, unprocessed so a steady
//! tone crosses as it is), with only the view trace's pipe
//! passed through. Then, driven by each sandbox's keyboard and b's notification's actions and read by the traces, the
//! recordings:
//!
//! - a calls b (Ctrl+Shift+C in the a–b chat a's settings open); b's banner, ringtone and notification come, and the
//!   ringtone is in b's recording; b answers from the notification's Answer; both connect, and each one's tone is in
//!   the other's recording, clean in its last 2 s of 20 ms windows (a median of 30 dB over noise, gaps and clicks, and
//!   at most one window in twenty under 20 dB); while the call runs, a's microphone moves (M3 Task 20): to a USB
//!   microphone a's settings choose (the sandbox adds the node; the choice goes through the settings model, as the
//!   settings view makes it), back to the system's when the settings follow it again, to a headset plugged in above the
//!   system's (following the system), and back once the headset is unplugged, b hearing each one's tone alone in turn
//!   and the call never ending; a turns its camera on (Ctrl+Shift+O: the sandbox's fake camera, H.264 the bundled
//!   source's), and b's window presents a's picture through GPUI's external surface and a's its self view (each counted
//!   by the trace's `video-tile presented`); a hangs up (Ctrl+Shift+H) and b's call ends with it, b's window staying on
//!   who ended it (with Call back) until it closes by itself;
//! - a calls b again; b declines from the notification's Decline; a's call is taken by b's voicemail (as captured) and
//!   a's call window and in-call strip say so; a hangs up;
//! - a places the test call from the settings' Calls page ("Make a test call"): the same call window, the echo's cell
//!   (its roster's entry), its camera turned on and the self view presented; a hangs up, and the test call's results
//!   show every check passed, in its window;
//! - each call has its own window (the call keys pressed in it), a's beside its main window (a's screen holds two
//!   windows side by side), each screenshotted under `target/ui-shots/ngteams-call-*.png`;
//! - both quit cleanly.
//!
//! Without weston 15, niri, `dbus-daemon`, PipeWire or WirePlumber it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

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
use teams_media::devices::pipewire::{PipeWireDevices, Reach};
use teams_media::devices::{Devices, SAMPLE_RATE, wav};
use teams_media::processing::ProcessingConfig;
use teams_media::relay::RelayServer;
use teams_media::session::SessionConfig;
use teams_media::tone::{Tone, amplitude_at, purity};
use teams_mock::{AB_CHAT, CHAT_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, LAB_C, TENANT};
use teams_proto::Mri;
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use teams_ui::Desktop;
use teams_ui::ringtone::RINGTONE;
use teams_ui::settings::Settings;
use teams_ui::settings_view::{Page, SettingsView};
use teams_ui::sounds::SoundBoard;
use teams_ui::view::Root;
use teams_ui::{Ringer, SettingsFile, VideoFeed};
use tokio::sync::broadcast;
use ui_sandbox::input::{Modifier, VirtualInput};
use ui_sandbox::pipewire::nodes::NodeSpec;
use ui_sandbox::pipewire::{DEVICE_PRIORITY, PipeWireOptions};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

/// Which account the child is (`a` or `b`); unset in the parent.
const CHILD_ENV: &str = "NGTEAMS_CALL_TEST";
const TRACE_FD_ENV: &str = "NGTEAMS_CALL_TRACE_FD";
/// The mock's origin, its Trouter's, and its TURN server's UDP address.
const ORIGIN_ENV: &str = "NGTEAMS_CALL_ORIGIN";
const TROUTER_ENV: &str = "NGTEAMS_CALL_TROUTER";
const RELAY_ENV: &str = "NGTEAMS_CALL_RELAY";
/// The account's mock tokens (the mock's made-up ones).
const IC3_ENV: &str = "NGTEAMS_CALL_IC3";
const SKYPE_ENV: &str = "NGTEAMS_CALL_SKYPE";
/// The child's own directory: its store and settings.
const DIR_ENV: &str = "NGTEAMS_CALL_DIR";
/// The file in the sandbox dir the parent writes to have the child quit.
const QUIT_TRIGGER: &str = "quit";
/// The file in the sandbox dir the parent writes to have the child's settings choose a microphone: the device's
/// name, or nothing to follow the system.
const MICROPHONE_TRIGGER: &str = "microphone";
/// The file in the sandbox dir the parent writes to have the child's settings show their Calls page
/// ([`SETTINGS_CALLS`]) or press its "Make a test call" ([`SETTINGS_TEST_CALL`]), as the settings view's button does:
/// the sandbox's virtual pointer has no way to find the button, and the button's own click is the view test's.
const SETTINGS_TRIGGER: &str = "settings";
const SETTINGS_CALLS: &str = "calls";
const SETTINGS_TEST_CALL: &str = "test-call";
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(120);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(60);
/// How long the call is held once both connected, before what each heard is read.
const TALK: Duration = Duration::from_secs(4);
/// How long the ringtone is let ring before b answers.
const RING: Duration = Duration::from_secs(3);
/// Each sandbox's microphone tone.
const A_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.5,
};
const B_TONE: Tone = Tone {
    hz: 660.0,
    amplitude: 0.5,
};
/// A tone heard at least this loud in a recording, in its loudest 400 ms.
const HEARD: f32 = 0.1;
/// The frames a tile must have presented (traced at the first and every 30th).
const PRESENTED: u64 = 30;
/// The tones of the microphones plugged into a's sandbox while the call runs: one the settings choose, one plugged in
/// above the system's.
const USB_TONE: Tone = Tone {
    hz: 550.0,
    amplitude: 0.5,
};
const HEADSET_TONE: Tone = Tone {
    hz: 990.0,
    amplitude: 0.5,
};
/// A tone quieter than this in a recording isn't there.
const QUIET: f32 = 0.03;
/// The windows a heard tone is judged in: 20 ms.
const JUDGED_MS: usize = 20;
/// A window is disturbed (a gap, a click, noise) when what the tone does not explain is within this of the tone.
const DISTURBED_DB: f32 = 20.0;
/// The median of a clean tone after Opus, over the windows: noise that buries the tone reads near 0 dB.
const CLEAN_DB: f32 = 30.0;
/// The share of windows that may be disturbed: one in twenty.
const DISTURBED_SHARE: f32 = 0.05;

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
            eprintln!("call window test: {error}");
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

/// niri's layout for a: its main window and its call's window side by side, each half the screen, as a desktop shows
/// two windows.
const SIDE_BY_SIDE: &str = "layout {\n    gaps 0\n    default-column-width { proportion 0.5; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\n";
/// a's screen: two windows of 1024 px side by side.
const A_SCREEN: (u32, u32) = (2048, 800);

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

/// a's sandbox: [`sandbox_options`], its windows side by side on a screen wide enough for two.
fn side_by_side_options(tone: Tone) -> SandboxOptions {
    SandboxOptions {
        width: A_SCREEN.0,
        height: A_SCREEN.1,
        niri_extra: SIDE_BY_SIDE,
        ..sandbox_options(tone)
    }
}

fn parent() -> SandboxResult {
    let Some(a_box) = sandbox_or_skip("ngteams calls", &side_by_side_options(A_TONE))? else {
        return Ok(());
    };
    let Some(b_box) = sandbox_or_skip("ngteams calls", &sandbox_options(B_TONE))? else {
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
    let lab = MockLab {
        origin: mock.origin(),
        trouter: trouter.origin().to_owned(),
        relay: relay.to_string(),
        dir: dir.path().to_owned(),
    };
    let mut a = AppUnderTest::start(&a_box, &lab, &mock, Alias::A)?;
    let mut b = AppUnderTest::start(&b_box, &lab, &mock, Alias::B)?;
    let outcome = calls(&mut a, &mut b, &a_box, &b_box);
    // a failure kills both apps first (their stderr is read to its end), then says what each wrote
    let outcome = outcome.map_err(|error| {
        format!(
            "{error}\n--- a: {}\n--- b: {}\n--- a's sandbox{}\n--- b's sandbox{}",
            a.running.fail(&"killed"),
            b.running.fail(&"killed"),
            a_box.logs(),
            b_box.logs()
        )
    });
    outcome?;
    // a's starts, follow-ups and leaves; b's answer and decline: each permitted, nothing denied
    a.quit(&a_box, &["call", "call_follow_up"])?;
    b.quit(&b_box, &["call_answer", "call_decline", "call_ringing"])?;
    drop(trouter);
    say("ngteams call window: ok");
    Ok(())
}

/// Where the children reach the mock.
struct MockLab {
    origin: String,
    trouter: String,
    relay: String,
    dir: PathBuf,
}

/// A child app in its sandbox: its process, its trace, its own dir.
struct AppUnderTest {
    alias: Alias,
    running: Running,
    trace: TraceReader,
}

impl AppUnderTest {
    fn start(sandbox: &Sandbox, lab: &MockLab, mock: &ChatServiceMock, alias: Alias) -> SandboxResult<Self> {
        let mri = mri_of(alias);
        let dir = lab.dir.join(alias.to_string());
        fs::create_dir_all(&dir)?;
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let ic3 = mock.token(mri).ok_or("the mock's token")?;
        let skype = mock.skypetoken(mri).ok_or("the mock's skypetoken")?;
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
            ],
            &[writer.as_fd()],
        )?;
        drop(writer);
        let running = Running::new(child)?;
        let mut app = Self {
            alias,
            running,
            trace: TraceReader::start(reader),
        };
        app.running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| app.running.fail(&format!("{alias}: no ready line: {error:?}")))?;
        app.trace.wait_for("status", "connected")?;
        app.trace.wait_for("calling", "registered")?;
        Ok(app)
    }

    /// Waits for an `in-call shown` line whose kinds hold every one of `kinds`; the line.
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

    /// Waits until the main window has the keyboard: its last `window activated` or `deactivated` (or the call
    /// window closing, after which the compositor hands the keyboard back) is an activation.
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

    /// Waits until the trace told `count` `call-window <change>` lines in all, and fails when it told more.
    fn call_windows(&mut self, change: &str, count: usize) -> SandboxResult {
        let is = |line: &serde_json::Value| line["view"] == "call-window" && line["change"] == change;
        let what = format!("{}: call-window {change} #{count}", self.alias);
        // only up to the line waited for: a later line stays for the next wait
        while self.trace.seen.iter().filter(|line| is(line)).count() < count {
            self.trace.next_where(&what, is)?;
        }
        let told = self.trace.seen.iter().filter(|line| is(line)).count();
        if told > count {
            return Err(format!("{what}: told {told}").into());
        }
        Ok(())
    }

    /// Asks the app to quit and waits for it to exit cleanly.
    fn quit(mut self, sandbox: &Sandbox, _kinds: &[&str]) -> SandboxResult {
        fs::write(sandbox.path(QUIT_TRIGGER), "")?;
        self.running
            .line_where(WAIT, |line| line == "quit")
            .map_err(|error| self.running.fail(&format!("{}: didn't quit: {error:?}", self.alias)))?;
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("{}: exited {status}: {stderr}", self.alias).into());
        }
        if let Some(odd) = self.trace.seen.iter().find(|line| !line.is_object()) {
            return Err(format!("{}: a trace line isn't JSON: {odd}", self.alias).into());
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

    /// Takes every line that has come so far into `seen`.
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

/// Ctrl+Shift+`key` on the sandbox's keyboard.
fn ctrl_shift(input: &mut VirtualInput, key: char) -> SandboxResult {
    let (code, _) = input.keymap().key_for(key).ok_or("not on the US keymap")?;
    input.key_with(&[Modifier::Ctrl, Modifier::Shift], code)
}

/// The loudest `hz` gets in any 400 ms of the `stretch` before the end of the recording at `path`.
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

/// Judges the last 2 s of the recording at `path` in 20 ms windows: over the windows, the tone at `hz` is a median
/// of at least [`CLEAN_DB`] over the rest (a gap, a click, noise), and at most [`DISTURBED_SHARE`] of the windows are
/// disturbed (under [`DISTURBED_DB`]).
fn heard_clean(path: &Path, hz: f32, who: &str) -> SandboxResult {
    let samples = wav::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let window = usize::try_from(SAMPLE_RATE / 1_000)? * JUDGED_MS;
    let tail = &samples[samples.len().saturating_sub(window * 100)..];
    let mut decibels: Vec<f32> = tail.chunks_exact(window).map(|chunk| purity(chunk, hz).1).collect();
    if decibels.len() < 50 {
        return Err(format!("{who}'s recording holds {} samples", samples.len()).into());
    }
    decibels.sort_by(f32::total_cmp);
    let median = decibels[decibels.len() / 2];
    let disturbed = decibels.iter().filter(|db| **db < DISTURBED_DB).count();
    #[expect(clippy::cast_precision_loss, reason = "a count of windows")]
    let share = disturbed as f32 / decibels.len() as f32;
    say(&format!(
        "{who} heard {hz} Hz at a median of {median:.1} dB over the rest, {disturbed} of {} windows disturbed",
        decibels.len()
    ));
    if median < CLEAN_DB || share > DISTURBED_SHARE {
        return Err(format!(
            "{who} heard {hz} Hz buried: median {median:.1} dB, {disturbed} of {} windows disturbed",
            decibels.len()
        )
        .into());
    }
    Ok(())
}

fn recording(sandbox: &Sandbox) -> SandboxResult<PathBuf> {
    Ok(sandbox.pipewire().ok_or("no PipeWire")?.recording())
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
    // the frames after the trace's line
    thread::sleep(Duration::from_millis(500));
    let png = sandbox.screenshot(name, window)?;
    fs::copy(&png, dir.join(name))?;
    Ok(())
}

/// The id of the notification of an incoming call on `sandbox`'s bus that wasn't there before (`seen` of them).
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

fn calls(a: &mut AppUnderTest, b: &mut AppUnderTest, a_box: &Sandbox, b_box: &Sandbox) -> SandboxResult {
    let mut a_keys = a_box.input()?;

    // a calls b from the a–b chat its settings opened
    a.trace.wait_for("call-button", "shown")?;
    ctrl_shift(&mut a_keys, 'c')?;
    a.in_call(&["chat", "ringing"])?;
    // a's call has its own window, beside the main one
    a.call_windows("opened", 1)?;
    let banner = b.trace.wait_for("call-banner", "shown")?;
    if banner["people"] != serde_json::json!([LAB_A]) {
        return Err(format!("b's banner names another caller: {banner}").into());
    }
    b.trace.wait_for("ringtone", "started")?;
    b.trace.wait_for("call-notification", "shown")?;
    let ringing = call_notification(b_box, 0)?;
    shot(b_box, "ngteams-call-incoming.png")?;
    thread::sleep(RING);
    let ringtone = loudest(&recording(b_box)?, RINGTONE[0].hz, RING)?;
    say(&format!("b's ringtone at {ringtone:.3}"));
    if ringtone < HEARD {
        return Err(format!("b's ringtone isn't in b's recording ({ringtone:.3})").into());
    }

    // b answers from the notification: both connect, each hears the other
    b_box.bus().invoke_action(ringing, "answer")?;
    b.trace.wait_for("call-notification", "answered")?;
    b.trace.wait_for("ringtone", "stopped")?;
    b.in_call(&["incoming", "connected"])?;
    b.call_windows("opened", 1)?;
    a.in_call(&["chat", "connected"])?;
    // a's call in its own window, beside its main window's chats and in-call strip
    shot(a_box, "ngteams-call-connected.png")?;
    shot_of(a_box, "ngteams-call-side-by-side.png", false)?;
    thread::sleep(TALK);
    let a_heard = loudest(&recording(a_box)?, B_TONE.hz, Duration::from_secs(2))?;
    let b_heard = loudest(&recording(b_box)?, A_TONE.hz, Duration::from_secs(2))?;
    say(&format!("a heard b at {a_heard:.3}, b heard a at {b_heard:.3}"));
    if a_heard < HEARD || b_heard < HEARD {
        return Err(format!("the tones didn't cross: a heard b at {a_heard:.3}, b heard a at {b_heard:.3}").into());
    }
    // each hears the other's tone clean, not buried in noise or cut
    heard_clean(&recording(a_box)?, B_TONE.hz, "a")?;
    heard_clean(&recording(b_box)?, A_TONE.hz, "b")?;

    microphones_mid_call(a, a_box, b_box)?;

    // a turns its camera on (off when the call started): b's window presents a's picture, a's its self view
    ctrl_shift(&mut a_keys, 'o')?;
    a.in_call(&["connected", "camera-on"])?;
    let presented = |tile: &'static str| {
        move |line: &serde_json::Value| {
            line["view"] == "video-tile"
                && line["change"] == "presented"
                && strings(line, "kinds") == [tile]
                && line["frames"].as_u64().is_some_and(|frames| frames >= PRESENTED)
        }
    };
    let remote = b
        .trace
        .next_where("b: the remote tile presented", presented("remote"))?;
    let own = a.trace.next_where("a: the self view presented", presented("self"))?;
    say(&format!(
        "b presented {} of a's frames, a {} of its own",
        remote["frames"], own["frames"]
    ));
    shot(b_box, "ngteams-call-video.png")?;
    shot(a_box, "ngteams-call-self-view.png")?;
    shot_of(a_box, "ngteams-call-self-view-side-by-side.png", false)?;

    // a hangs up: b's call ends with it
    ctrl_shift(&mut a_keys, 'h')?;
    a.in_call(&["ended", "hung-up"])?;
    b.in_call(&["ended", "remote"])?;
    // a's window closes with its own hang-up; b's says who ended the call, with Call back, then closes by itself
    a.call_windows("closed", 1)?;
    let ended = b.trace.wait_for("call-window", "ended")?;
    if strings(&ended, "kinds") != ["ended-by-other", "call-back"] {
        return Err(format!("b's call window tells another end: {ended}").into());
    }
    shot(b_box, "ngteams-call-ended.png")?;
    b.call_windows("closed", 1)?;

    declined_call(a, b, &mut a_keys, (a_box, b_box))?;
    test_call(a, &mut a_keys, a_box)
}

/// a calls again; b declines from the notification; b's voicemail takes a's call, and a sees it; a hangs up.
fn declined_call(
    a: &mut AppUnderTest,
    b: &mut AppUnderTest,
    a_keys: &mut VirtualInput,
    (a_box, b_box): (&Sandbox, &Sandbox),
) -> SandboxResult {
    a.trace.next_where("a: call-button shown call", |line| {
        line["view"] == "call-button" && strings(line, "kinds") == ["call"]
    })?;
    // the keys go to a's main window once it has the keyboard again: some time after its call's window closed
    a.main_window_active()?;
    ctrl_shift(a_keys, 'c')?;
    a.in_call(&["chat", "ringing"])?;
    a.call_windows("opened", 2)?;
    b.trace.next_where("b: call-notification shown again", |line| {
        line["view"] == "call-notification" && line["change"] == "shown"
    })?;
    let second = call_notification(b_box, 1)?;
    b_box.bus().invoke_action(second, "decline")?;
    b.trace.next_where("b: call-notification declined", |line| {
        line["view"] == "call-notification" && line["change"] == "declined"
    })?;
    b.in_call(&["incoming", "ended", "declined"])?;
    // a decline opens no window
    b.call_windows("opened", 1)?;
    a.in_call(&["voicemail"])?;
    shot(a_box, "ngteams-call-declined.png")?;
    ctrl_shift(a_keys, 'h')?;
    a.in_call(&["ended", "hung-up"])?;
    a.call_windows("closed", 2)
}

/// a's test call, from the settings' Calls page ("Make a test call", as Teams has it): a call window of its own as a
/// chat's call has, the settings staying in the main window, Echo its other participant (by its roster's entry), its
/// camera turned on and its self view presented; once a hangs up, its results (the network, the microphone, the
/// speaker and the camera checked).
fn test_call(a: &mut AppUnderTest, keys: &mut VirtualInput, a_box: &Sandbox) -> SandboxResult {
    ask_settings(a_box, SETTINGS_CALLS)?;
    a.trace.next_where("a: settings opened", |line| {
        line["view"] == "settings" && line["change"] == "opened"
    })?;
    shot(a_box, "ngteams-call-settings.png")?;
    ask_settings(a_box, SETTINGS_TEST_CALL)?;
    a.in_call(&["test-call", "connected"])?;
    a.call_windows("opened", 3)?;
    ctrl_shift(keys, 'o')?;
    a.in_call(&["test-call", "camera-on"])?;
    a.trace.next_where("a: the test call's self view presented", |line| {
        line["view"] == "video-tile"
            && line["change"] == "presented"
            && strings(line, "kinds") == ["self"]
            && line["frames"].as_u64().is_some_and(|frames| frames >= PRESENTED)
    })?;
    let echo = |line: &serde_json::Value| {
        line["view"] == "call-participant" && line["change"] == "shown" && line["people"] == serde_json::json!([ECHO])
    };
    if !a.trace.catch_up().iter().any(echo) {
        a.trace.next_where("a: Echo's cell", echo)?;
    }
    shot(a_box, "ngteams-call-test-call.png")?;
    ctrl_shift(keys, 'h')?;
    a.in_call(&["test-call", "ended", "hung-up"])?;
    let wanted = ["connected", "mic-heard", "speaker-played", "camera-on"];
    let results = |line: &serde_json::Value| line["view"] == "test-call" && line["change"] == "results";
    let line = match a.trace.catch_up().iter().find(|line| results(line)).cloned() {
        Some(line) => line,
        None => a.trace.next_where("a: the test call's results", results)?,
    };
    if strings(&line, "kinds") != wanted {
        return Err(format!("a: the test call's results {line}, not {wanted:?}").into());
    }
    // the trace's line comes as the model changes, before the window draws it: a moment for the frame
    thread::sleep(Duration::from_secs(2));
    // its window stays on its results, as Teams' test call's does
    a.call_windows("closed", 2)?;
    shot(a_box, "ngteams-call-test-call-results.png")?;
    Ok(())
}

/// Asks a's app to show the settings' Calls page ([`SETTINGS_CALLS`]) or to press its "Make a test call"
/// ([`SETTINGS_TEST_CALL`]).
fn ask_settings(a_box: &Sandbox, what: &str) -> SandboxResult {
    let staged = a_box.path("settings.staged");
    fs::write(&staged, what)?;
    fs::rename(&staged, a_box.path(SETTINGS_TRIGGER))?;
    Ok(())
}

/// Moves a's microphone while the call runs: to a USB microphone a's settings choose, back to the system's, to a
/// headset plugged in above the system's (following the system), and back once it is unplugged; b hears each tone
/// alone in turn, and a's call never ends.
fn microphones_mid_call(a: &mut AppUnderTest, a_box: &Sandbox, b_box: &Sandbox) -> SandboxResult {
    let pipewire = a_box.pipewire().ok_or("no PipeWire")?;
    let heard_by_b = recording(b_box)?;
    let usb = pipewire.add_microphone(
        NodeSpec {
            name: "sandbox-usb-mic".to_owned(),
            description: "USB microphone".to_owned(),
            priority: DEVICE_PRIORITY - 1_000,
        },
        USB_TONE,
    )?;
    choose_microphone(a_box, Some(usb.name()))?;
    hears_alone(
        &heard_by_b,
        USB_TONE.hz,
        A_TONE.hz,
        "the USB microphone a's settings chose",
    )?;
    choose_microphone(a_box, None)?;
    hears_alone(
        &heard_by_b,
        A_TONE.hz,
        USB_TONE.hz,
        "a's settings following the system again",
    )?;
    let headset = pipewire.add_microphone(
        NodeSpec {
            name: "sandbox-headset-mic".to_owned(),
            description: "Headset".to_owned(),
            priority: DEVICE_PRIORITY + 1_000,
        },
        HEADSET_TONE,
    )?;
    hears_alone(
        &heard_by_b,
        HEADSET_TONE.hz,
        A_TONE.hz,
        "a headset plugged into a's sandbox",
    )?;
    headset.remove()?;
    hears_alone(&heard_by_b, A_TONE.hz, HEADSET_TONE.hz, "the headset unplugged")?;
    drop(usb);
    if let Some(ended) = a
        .trace
        .catch_up()
        .iter()
        .find(|line| line["view"] == "in-call" && strings(line, "kinds").iter().any(|kind| kind == "ended"))
    {
        return Err(format!("a's call ended while its microphone moved: {ended}").into());
    }
    Ok(())
}

/// Has a's settings choose the microphone `name` (`None`: the system's), as the settings view does.
fn choose_microphone(a_box: &Sandbox, name: Option<&str>) -> SandboxResult {
    let staged = a_box.path("microphone.staged");
    fs::write(&staged, name.unwrap_or_default())?;
    fs::rename(&staged, a_box.path(MICROPHONE_TRIGGER))?;
    Ok(())
}

/// Waits until the recording at `path` holds `wanted` and not `other` at its end: what `moved` moved b's ear to.
fn hears_alone(path: &Path, wanted: f32, other: f32, moved: &str) -> SandboxResult {
    let started = Instant::now();
    let alone = wait_until(WAIT, || {
        let heard = loudest(path, wanted, Duration::from_secs(1)).ok()?;
        let left = loudest(path, other, Duration::from_millis(600)).ok()?;
        (heard >= HEARD && left < QUIET).then_some((heard, left))
    });
    match alone {
        Some((heard, left)) => {
            say(&format!(
                "{moved}: b heard {wanted} Hz at {heard:.3} ({other} Hz at {left:.3}) after {} ms",
                started.elapsed().as_millis()
            ));
            Ok(())
        }
        None => Err(format!(
            "{moved}: b didn't hear {wanted} Hz alone (at {:?}, {other} Hz at {:?})",
            loudest(path, wanted, Duration::from_secs(1)).ok(),
            loudest(path, other, Duration::from_millis(600)).ok()
        )
        .into()),
    }
}

// --- the children ---

/// One account's mock tokens: its `ic3` token and its skypetoken; they never run out in a test.
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

/// The account's engine on the mock, with its calls' media on `devices`, relayed through the mock TURN server.
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
            ..Default::default()
        },
    };
    let push = Push::new(trouter()?);
    let handle = engine::start(Parts {
        calls: Some(CallParts::new(calling, identity, media)),
        store: Store::open(&support::store_file(dir, Alias::A))?,
        client: Arc::new(Client::new(http()?)),
        outbox: Outbox::new(transport, Author::new(Mri::new(mri), name))
            .with_calls(Arc::clone(&tracker), teams_core::CallFloor::new()),
        push: Some(push),
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })?;
    Ok(handle)
}

fn child(alias: Alias) -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let pipewire = Arc::new(PipeWireDevices::connect(Reach::Sandbox)?);
    let devices: Arc<dyn Devices + Send + Sync> = pipewire.clone();
    let feed = VideoFeed::new();
    let engine = engine(alias, Arc::clone(&devices), feed.clone(), &dir)?;
    let first = support::started(alias, engine)?;
    let runner = support::runner(&dir)?;
    let settings = SettingsFile::new(dir.join("settings.toml"));
    if alias == Alias::A {
        let opened = Settings {
            selected_chat: Some(AB_CHAT.to_owned()),
            ..Settings::default()
        };
        settings.save(&opened).map_err(|problem| format!("{problem:?}"))?;
    }
    let ringer: Arc<dyn Ringer> = Arc::new(SoundBoard::new(devices));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first: ngteams::accounts::Started {
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
                choose_when_asked(launched.desktop.clone(), cx);
                settings_when_asked(launched.desktop, cx);
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

/// Each time the parent writes [`MICROPHONE_TRIGGER`] in the sandbox dir: the settings choose the microphone it names
/// (none: the system's), through the settings model as the settings view does.
fn choose_when_asked(desktop: gpui::Entity<Desktop>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let trigger = PathBuf::from(dir).join(MICROPHONE_TRIGGER);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(name) = fs::read_to_string(&trigger) else {
                continue;
            };
            let _ = fs::remove_file(&trigger);
            let name = (!name.is_empty()).then_some(name);
            cx.update(|cx| {
                let settings = desktop.read(cx).settings().clone();
                settings.update(cx, |model, cx| model.change(|settings| settings.microphone = name, cx));
            });
        }
    })
    .detach();
}

/// Each time the parent writes [`SETTINGS_TRIGGER`] in the sandbox dir: the shell shows the settings' Calls page, or
/// its settings view's "Make a test call" is pressed.
fn settings_when_asked(desktop: gpui::Entity<Desktop>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let trigger = PathBuf::from(dir).join(SETTINGS_TRIGGER);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(asked) = fs::read_to_string(&trigger) else {
                continue;
            };
            let _ = fs::remove_file(&trigger);
            let Some(window) = cx.update(|cx| desktop.read(cx).window()) else {
                say("no window");
                continue;
            };
            let _ = window.update(cx, |kit, window, cx| {
                let Some(shell) = kit
                    .view()
                    .clone()
                    .downcast::<Root>()
                    .ok()
                    .and_then(|root| root.read(cx).shell_in(cx).cloned())
                else {
                    return;
                };
                let view = shell.read(cx).settings_view().clone();
                match asked.as_str() {
                    // as the rail's Settings opens them: the keyboard on their pages, so the call keys still work
                    SETTINGS_CALLS => {
                        shell.update(cx, |shell, cx| shell.open_settings_page(Page::Calls, window, cx));
                    }
                    SETTINGS_TEST_CALL => view.update(cx, SettingsView::make_test_call),
                    _ => say("an unknown settings ask"),
                }
            });
        }
    })
    .detach();
}

/// Once the parent writes [`QUIT_TRIGGER`] in the sandbox dir: the engine shut down, then the app quits.
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
