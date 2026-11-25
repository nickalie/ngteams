//! An own server's group call with video in the app's own call window, as a user meets it, in the UI sandbox
//! (`harness = false`: GPUI runs on the process's main thread).
//!
//! The parent runs the real `ngteams-server` binary (TLS with a made-up certificate authority, its users made with
//! `user add`, its group calls' media on its own media node: `serve`, `--role all`) beside the run's coturn, and the
//! clients of two made-up people, b and c, each brought up as the app brings an own server's account up
//! (`ngteams::signin::add_own`, `ngteams::engine::start`), their cameras the file backend's test pattern. In the
//! sandbox this test runs again as a's app on the same server (signed in with its password, started as the app
//! starts it, its camera the test pattern too), serving its control socket. b calls the a-b-c chat, which has members
//! with no client; a answers over the control socket (`ngteamsctl answer`), c from its client; every camera goes on.
//!
//! - The gallery: b's and c's video tiles presented beside a's own; shots in Teams Light and Dark, the call window
//!   1280 and 480 px wide.
//! - b shares its screen at 1080p, as a desktop shares it: the share presented on a's stage; the same four shots.
//!   a's app log (info, as the app installs it) tells the screen's source request and its first frame at 1920x1080.
//! - b's Stop sharing: the share leaves a's stage.
//! - Everyone hangs up; a's app quits.
//!
//! Shots under `target/ui-shots/own-group-video-*.png` and `$OWN_GROUP_VIDEO_EVIDENCE`, if set.
//!
//! Without weston 15, niri or `dbus-daemon`, without a coturn (`NGTS_TEST_COTURN`, `NGTS_TEST_COTURN_SECRET`) or
//! without the server binary (`NGTEAMS_QA_SERVER_BIN`, else `ngteams-server` beside the test's target directory) it
//! says so and passes.

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, thread};

use gpui::{App, AsyncApp, WindowingModes};
use ngteams::accounts_file::{self, PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::calling::PersonalCalls;
use ngteams::descriptors::adopt_trace;
use ngteams::setup::Setup;
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_core::account::{AccountCalls, AccountEngine, call_video, web_client_info};
use teams_core::engine::{Action, Event};
use teams_core::{CallAction, CallFloor, CallId, CallMedia, CallStatus, Relay, VideoOutput};
use teams_ctl::protocol::{CallRow, Request, Response, Toggle};
use teams_ctl::{Client, Timeouts};
use teams_media::devices::file::{AudioInput, FileDevices, FrameCounter, Pacing, PatternCamera, SCREEN as FILE_SCREEN};
use teams_media::devices::{AudioSink, AudioSource, DeviceError, Devices, VideoSink, VideoSource};
use teams_media::processing::ProcessingConfig;
use teams_media::session::SessionConfig;
use teams_media::tone::Tone;
use teams_proto::Mri;
use teams_store::OutboxState;
use teams_ui::settings::Settings;
use teams_ui::{SettingsFile, VideoFeed};
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "../own_server_binary.rs"]
#[expect(
    dead_code,
    reason = "the own server's tests share this support; this one starts it with tables"
)]
mod own_server_binary;
#[path = "../window.rs"]
#[expect(
    dead_code,
    reason = "the GUI tests share this support; this one uses only its runner"
)]
mod support;
use own_server_binary::{BinaryServer, PASSWORD};

const CHILD_ENV: &str = "NGTEAMS_OWN_GROUP_VIDEO_TEST";
const TRACE_FD_ENV: &str = "NGTEAMS_OWN_GROUP_VIDEO_TRACE_FD";
const ORIGIN_ENV: &str = "NGTEAMS_OWN_GROUP_VIDEO_ORIGIN";
const CA_ENV: &str = "NGTEAMS_OWN_GROUP_VIDEO_CA";
const DIR_ENV: &str = "NGTEAMS_OWN_GROUP_VIDEO_DIR";
const CTL_ENV: &str = "NGTEAMS_OWN_GROUP_VIDEO_CTL";
/// Where the shots are copied besides `target/ui-shots/`, if set (the QA run's evidence).
const EVIDENCE_ENV: &str = "OWN_GROUP_VIDEO_EVIDENCE";
const COTURN_ENV: &str = "NGTS_TEST_COTURN";
const COTURN_SECRET_ENV: &str = "NGTS_TEST_COTURN_SECRET";
const QUIT_TRIGGER: &str = "quit";
/// The file in the sandbox dir the parent writes a theme's key to (`teams_ui::theme`).
const THEME_TRIGGER: &str = "theme";

const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(60);
/// How long a change takes to settle on the screen before a shot.
const SETTLE: Duration = Duration::from_millis(1500);
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
const SCREEN: (u32, u32) = (1920, 1080);
/// The call window's widths the shots are taken at, and its height.
const WIDTHS: [i32; 2] = [1280, 480];
const HEIGHT: i32 = 800;

/// The people with a client: user name, display name, camera, tone.
const A: (&str, &str) = ("ada@own.example.test", "Ada Ł. Lovelace");
const B: (&str, &str) = ("bert@own.example.test", "Bert 🎺 Bach");
const C: (&str, &str) = ("cora@own.example.test", "Cora 'Cee' Nuñez");
const A_CAMERA: PatternCamera = PatternCamera {
    width: 640,
    height: 360,
    fps: 15,
};
const B_CAMERA: PatternCamera = PatternCamera {
    width: 480,
    height: 360,
    fps: 15,
};
const C_CAMERA: PatternCamera = PatternCamera {
    width: 360,
    height: 360,
    fps: 15,
};
/// b's screen: a desktop's 1080p.
const FULL_HD: PatternCamera = PatternCamera {
    width: 1920,
    height: 1080,
    fps: 15,
};
/// What a's app log must tell of b's share, at info.
const SHARE_LOG: [&str; 2] = [": screen ", "the first frame of a shared screen came: 1920x1080"];
const fn tone(hz: f32) -> Tone {
    Tone { hz, amplitude: 0.3 }
}

/// The file devices, their screen the test pattern of `screen`'s shape.
struct Screened {
    files: FileDevices,
    screen: PatternCamera,
}

impl Devices for Screened {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        self.files.microphone()
    }

    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        self.files.speaker()
    }

    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        self.files.camera()
    }

    fn screen(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        FileDevices {
            camera: self.screen,
            ..self.files.clone()
        }
        .camera()
    }
}
/// Made-up members of the chat with no client of their own.
const QUIET: [&str; 4] = [
    "Zoë O'Brien-Ångström",
    "李 小龍",
    "Dr. 🦀 Ferris",
    "<b>Bold</b> & \"Quoted\"",
];

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("own group video window test: {error}");
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

/// An own server's account signed in with its password and started as the app starts it, with its calls on `media`.
fn own_account(
    dir: &Path,
    (origin, ca_file): (&str, &str),
    upn: &str,
    media: CallMedia,
) -> SandboxResult<(PersonalAccount, AccountEngine<MemorySecretStore>)> {
    let paths = PersonalPaths::new(dir.join("config"), dir.join("data"));
    let setup = Setup::new(paths)?;
    let server = accounts_file::OwnServer::parse(origin, ca_file)?;
    let store = MemorySecretStore::new();
    let runtime = ngteams::accounts::runtime()?;
    let account = runtime.block_on(ngteams::signin::add_own(
        &setup,
        upn,
        &server,
        &Secret::new(PASSWORD),
        &store,
    ))?;
    let calls = AccountCalls {
        media,
        client: web_client_info(),
        language_id: Some("en-us".into()),
    };
    let engine = ngteams::engine::start(&setup.paths, &account, CallFloor::new(), store, true, Some(calls))?;
    Ok((account, engine))
}

/// A call's media on the file devices: `tone` in, nothing recorded, the test patterns of `camera`'s and `screen`'s
/// shapes.
fn media(tone: Tone, (camera, screen): (PatternCamera, PatternCamera), output: Arc<dyn VideoOutput>) -> CallMedia {
    CallMedia {
        devices: Arc::new(Screened {
            files: FileDevices {
                input: AudioInput::Tone(tone),
                recording: None,
                camera,
                pacing: Pacing::RealTime,
            },
            screen,
        }),
        // as a shipped client's: an own server's account's media goes through the relay its server names
        relay: Relay::Teams,
        session: SessionConfig {
            processing: ProcessingConfig::OFF,
            ..Default::default()
        },
        video: Some(call_video(output)),
    }
}

// --- the parent ---

/// What b's and c's clients show: counted, never drawn.
#[derive(Clone, Default)]
struct Counted(FrameCounter);

impl VideoOutput for Counted {
    fn remote(&self, _line: usize) -> Box<dyn VideoSink> {
        Box::new(self.0.clone())
    }

    fn preview(&self) -> Option<Box<dyn VideoSink>> {
        None
    }
}

/// b's or c's client, in the parent.
struct Peer {
    name: &'static str,
    mri: String,
    engine: AccountEngine<MemorySecretStore>,
    shown: Counted,
}

impl Peer {
    fn start(
        dir: &Path,
        server: (&str, &str),
        (upn, name): (&str, &'static str),
        look: (Tone, PatternCamera, PatternCamera),
    ) -> SandboxResult<Self> {
        let shown = Counted::default();
        let (account, engine) = own_account(
            dir,
            server,
            upn,
            media(look.0, (look.1, look.2), Arc::new(shown.clone())),
        )?;
        let mri = account.mri().ok_or("no MRI")?.as_str().to_owned();
        Ok(Self {
            name,
            mri,
            engine,
            shown,
        })
    }

    /// Presses `action`, again while the call can't take it yet, as a user's second press would.
    fn act(&self, runtime: &tokio::runtime::Runtime, action: &CallAction) -> SandboxResult<CallId> {
        let deadline = Instant::now() + WAIT;
        loop {
            match runtime.block_on(self.engine.handle.requests().call(action.clone())) {
                Ok(call) => return Ok(call),
                Err(error) if Instant::now() < deadline => {
                    say(&format!("{}: {action:?} not taken yet: {error}", self.name));
                    thread::sleep(Duration::from_millis(300));
                }
                Err(error) => return Err(format!("{}: {action:?}: {error}", self.name).into()),
            }
        }
    }

    /// The next event `pick` takes, within [`WAIT`].
    fn next<T>(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        what: &str,
        pick: impl Fn(&Event) -> Option<T>,
    ) -> SandboxResult<T> {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = runtime
                .block_on(async { tokio::time::timeout(left, self.engine.handle.next_event()).await })
                .map_err(|_| format!("{}: no {what} in time", self.name))?
                .ok_or_else(|| format!("{}: the engine stopped before {what}", self.name))?;
            if let Some(found) = pick(&event) {
                return Ok(found);
            }
        }
    }

    fn connected(&mut self, runtime: &tokio::runtime::Runtime, call: CallId) -> SandboxResult {
        self.next(runtime, "connected", |event| match event {
            Event::Call { call: of, status } if *of == call => match status {
                CallStatus::Connected => Some(Ok(())),
                CallStatus::Ended(end) => Some(Err(format!("the call ended: {end:?}"))),
                _ => None,
            },
            _ => None,
        })?
        .map_err(Into::into)
    }
}

fn coturn() -> Option<(String, String)> {
    Some((std::env::var(COTURN_ENV).ok()?, std::env::var(COTURN_SECRET_ENV).ok()?))
}

fn parent() -> SandboxResult {
    let Some((relay, secret)) = coturn() else {
        say("skipped: no coturn named (NGTS_TEST_COTURN, NGTS_TEST_COTURN_SECRET)");
        return Ok(());
    };
    let Some(binary) = own_server_binary::binary()? else {
        say("skipped: no ngteams-server binary (NGTEAMS_QA_SERVER_BIN)");
        return Ok(());
    };
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams own group video", &options)? else {
        return Ok(());
    };
    let tables = format!(
        "[calling]\nring_timeout = 45\n[calling.turn]\nsecret = {secret:?}\nurls = [\"turn:{relay}?transport=udp\"]\n\
         [mixer]\nudp_listen = \"127.0.0.1:0\"\n"
    );
    let server = BinaryServer::start_with(&binary, &tables)?;
    let mut oids = Vec::new();
    for (upn, name) in [A, B, C] {
        oids.push(server.add_user(upn, name)?);
    }
    let mut quiet = Vec::new();
    for (index, name) in QUIET.iter().enumerate() {
        quiet.push(format!(
            "8:orgid:{}",
            server.add_user(&format!("quiet.{index}@own.example.test"), name)?
        ));
    }
    let ca = server.ca_file.to_string_lossy().into_owned();
    let at = (server.origin.as_str(), ca.as_str());
    let dir = tempfile::tempdir()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mut b = Peer::start(&dir.path().join("b"), at, B, (tone(660.0), B_CAMERA, FULL_HD))?;
    let mut c = Peer::start(&dir.path().join("c"), at, C, (tone(880.0), C_CAMERA, FILE_SCREEN))?;
    let mut app = AppUnderTest::start(&sandbox, at, dir.path())?;
    let a_mri = format!("8:orgid:{}", oids[0]);
    let outcome = Rig::new(&sandbox, &mut app, &runtime).and_then(|mut rig| {
        let result = scenario(&mut rig, (&mut b, &mut c), &a_mri, &quiet);
        rig.done();
        result
    });
    let outcome = outcome.map_err(|error| {
        format!(
            "{error}\n--- the app: {}\n--- the server's log: {}\n--- the sandbox{}",
            app.running.fail(&"killed"),
            server.log(),
            sandbox.logs()
        )
    });
    for peer in [b, c] {
        peer.engine.handle.shutdown();
    }
    outcome?;
    app.quit(&sandbox)?;
    share_logged(&dir.path().join("a").join(APP_LOG))?;
    say("ngteams own group video window: ok");
    Ok(())
}

/// b calls the chat; a answers in the app, c in its client; every camera on; the gallery's and the share's shots.
fn scenario(rig: &mut Rig<'_>, (b, c): (&mut Peer, &mut Peer), a_mri: &str, quiet: &[String]) -> SandboxResult {
    let runtime = rig.runtime;
    let (b_mri, c_mri) = (b.mri.clone(), c.mri.clone());
    let mut others = vec![Mri::new(a_mri), Mri::new(c_mri.as_str())];
    others.extend(quiet.iter().map(Mri::new));
    let row = runtime.block_on(b.engine.handle.write(Action::CreateChat {
        others,
        topic: Some("Q4 ✨ «planning» & co".into()),
    }))?;
    if row.state != OutboxState::Sent {
        return Err(format!("the chat wasn't made: {row:?}").into());
    }
    let chat = row.message_id.ok_or("the chat's id")?;
    runtime.block_on(c.engine.handle.sync())?;
    thread::sleep(Duration::from_secs(2));
    let call = b.act(runtime, &CallAction::Call { chat: chat.clone() })?;
    b.connected(runtime, call)?;
    rig.answer()?;
    let rung = c.next(runtime, "c's ring", |event| match event {
        Event::IncomingCall(incoming) => Some(incoming.call),
        _ => None,
    })?;
    let c_call = c.act(runtime, &CallAction::Answer { call: rung })?;
    c.connected(runtime, c_call)?;
    rig.ctl.request(None, Request::Camera { op: Toggle::On })?;
    b.act(runtime, &CallAction::Camera { call, on: true })?;
    c.act(runtime, &CallAction::Camera { call: c_call, on: true })?;
    // a's window presents b's and c's video on two lines of its own, and its own camera
    rig.app
        .trace
        .wait_until("two video tiles and the self view presented", |seen| {
            let lines: std::collections::BTreeSet<String> = seen
                .iter()
                .filter(|line| line["view"] == "video-tile" && line["change"] == "presented")
                .flat_map(|line| strings(line, "kinds"))
                .collect();
            lines.iter().filter(|kind| kind.starts_with("line-")).count() >= 2 && lines.contains("self")
        })?;
    say(&format!(
        "a's window presents b's and c's video; b's client decoded {} frames, c's {}",
        b.shown.0.counts().frames,
        c.shown.0.counts().frames
    ));
    rig.shots("gallery")?;
    // b shares its screen: a's stage presents it
    let shared_before = rig.app.trace.presented("share");
    b.act(runtime, &CallAction::Share { call, on: true })?;
    rig.app.trace.wait_until("b's screen presented", |seen| {
        presented_in(seen, "share") >= shared_before + 30
    })?;
    rig.shots("share")?;
    // Stop sharing: the share leaves a's stage
    b.act(runtime, &CallAction::Share { call, on: false })?;
    // the stage presents no more of the share (it draws no share tile, so nothing says it hidden)
    let deadline = Instant::now() + WAIT;
    loop {
        let before = rig.app.trace.presented("share");
        thread::sleep(Duration::from_secs(3));
        if rig.app.trace.presented("share") == before {
            break;
        }
        if Instant::now() > deadline {
            return Err("b's screen is still presented after Stop sharing".into());
        }
    }
    rig.theme(teams_ui::theme::LIGHT_KEY)?;
    rig.resize(WIDTHS[0])?;
    rig.save("after-stop-sharing-light-1280")?;
    // everyone hangs up
    rig.ctl.request(None, Request::HangUp)?;
    c.act(runtime, &CallAction::HangUp { call: c_call })?;
    let _ = b.act(runtime, &CallAction::HangUp { call });
    say(&format!("b {b_mri} and c {c_mri} hung up"));
    Ok(())
}

/// `target/ui-shots/` and the evidence dir, if set.
fn evidence_dirs() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    let mut dirs = vec![dir];
    dirs.extend(std::env::var_os(EVIDENCE_ENV).map(PathBuf::from));
    dirs
}

/// a's app log, in its dir under the parent's `a`.
const APP_LOG: &str = "logs";

/// a's app log tells each of [`SHARE_LOG`]: the lines go to the evidence too.
fn share_logged(dir: &Path) -> SandboxResult {
    let mut text = String::new();
    for entry in fs::read_dir(dir)? {
        text.push_str(&fs::read_to_string(entry?.path())?);
    }
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("source request") || line.contains("shared screen"))
        .collect();
    let told = lines.join("\n");
    for dir in evidence_dirs() {
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(dir.join("own-group-video-share-log.txt"), &told);
    }
    say(&format!("a's app log of the share:\n{told}"));
    for wanted in SHARE_LOG {
        if !lines.iter().any(|line| line.contains(wanted)) {
            return Err(format!("a's app log doesn't tell {wanted:?}: {told}").into());
        }
    }
    Ok(())
}

fn strings(line: &serde_json::Value, key: &str) -> Vec<String> {
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

/// The most frames of `kind` the trace told presented so far.
fn presented_in(seen: &[serde_json::Value], kind: &str) -> u64 {
    seen.iter()
        .filter(|line| {
            line["view"] == "video-tile" && line["change"] == "presented" && strings(line, "kinds") == [kind]
        })
        .filter_map(|line| line["frames"].as_u64())
        .max()
        .unwrap_or_default()
}

struct Rig<'a> {
    sandbox: &'a Sandbox,
    app: &'a mut AppUnderTest,
    runtime: &'a tokio::runtime::Runtime,
    ctl: Client,
    /// niri's id of the call window.
    window_id: Option<u64>,
    shots: usize,
}

impl<'a> Rig<'a> {
    fn new(
        sandbox: &'a Sandbox,
        app: &'a mut AppUnderTest,
        runtime: &'a tokio::runtime::Runtime,
    ) -> SandboxResult<Self> {
        let socket = app.socket.clone();
        let ctl = wait_until(WAIT, || Client::connect(&socket, Timeouts::default()).ok())
            .ok_or_else(|| format!("no control socket at {}", socket.display()))?;
        Ok(Self {
            sandbox,
            app,
            runtime,
            ctl,
            window_id: None,
            shots: 0,
        })
    }

    fn done(&mut self) {
        say(&format!("{} shots taken", self.shots));
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

    /// Answers the ringing call over the control socket; the call window opens and is found.
    fn answer(&mut self) -> SandboxResult {
        self.wait_call("ringing", |row| row.ringing)?;
        self.ctl.request(None, Request::Answer)?;
        self.wait_call("connected", |row| row.state == "connected")?;
        self.app.trace.wait_for("call-window", "opened")?;
        thread::sleep(SETTLE);
        let text = self.sandbox.niri_msg(&["-j", "windows"])?;
        let windows: serde_json::Value = serde_json::from_str(&text)?;
        let focused = windows
            .as_array()
            .and_then(|list| list.iter().find(|window| window["is_focused"] == true))
            .ok_or_else(|| format!("no focused window: {text}"))?;
        self.window_id = focused["id"].as_u64();
        say(&format!("the call window: {}", focused["title"]));
        Ok(())
    }

    fn focus(&mut self) -> SandboxResult {
        if let Some(id) = self.window_id {
            self.sandbox
                .niri_msg(&["action", "focus-window", "--id", &id.to_string()])?;
        }
        Ok(())
    }

    /// The call window's size, as niri gives it.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "niri's logical px are whole numbers on a scale of 1"
    )]
    fn size(&mut self) -> SandboxResult<(i32, i32)> {
        let text = self.sandbox.niri_msg(&["-j", "windows"])?;
        let windows: serde_json::Value = serde_json::from_str(&text)?;
        let window = windows
            .as_array()
            .and_then(|list| list.iter().find(|window| window["id"].as_u64() == self.window_id))
            .ok_or_else(|| format!("no call window: {text}"))?;
        let size = &window["layout"]["window_size"];
        let at = |index: usize| size[index].as_f64().map(|value| value.round() as i32);
        Ok((at(0).ok_or("no width")?, at(1).ok_or("no height")?))
    }

    /// The compositor sizes the call window `width` wide, as a user's drag does; the size it took.
    fn resize(&mut self, width: i32) -> SandboxResult<(i32, i32)> {
        self.focus()?;
        self.sandbox
            .niri_msg(&["action", "set-column-width", &width.to_string()])?;
        self.sandbox
            .niri_msg(&["action", "set-window-height", &HEIGHT.to_string()])?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut size = self.size()?;
        while size.0 != width && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
            size = self.size()?;
        }
        thread::sleep(SETTLE);
        say(&format!(
            "asked {width}×{HEIGHT}: the call window is {}×{}",
            size.0, size.1
        ));
        Ok(size)
    }

    fn theme(&mut self, key: &str) -> SandboxResult {
        fs::write(self.sandbox.path(THEME_TRIGGER), key)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The four shots of what a's call window shows now: Light and Dark, each 1280 and 480 px wide.
    fn shots(&mut self, what: &str) -> SandboxResult {
        for (theme, name) in [
            (teams_ui::theme::LIGHT_KEY, "light"),
            (teams_ui::theme::DARK_KEY, "dark"),
        ] {
            self.theme(theme)?;
            for width in WIDTHS {
                let (got, _) = self.resize(width)?;
                self.save(&format!(
                    "{what}-{name}-{width}{}",
                    if got == width {
                        String::new()
                    } else {
                        format!("-is-{got}")
                    }
                ))?;
            }
        }
        Ok(())
    }

    /// Saves the call window alone as `own-group-video-<name>.png` under `target/ui-shots/` and the evidence dir.
    fn save(&mut self, name: &str) -> SandboxResult {
        self.focus()?;
        thread::sleep(Duration::from_millis(300));
        self.shots += 1;
        let png = self.sandbox.screenshot(&format!("shot-{}.png", self.shots), true)?;
        for dir in evidence_dirs() {
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(format!("own-group-video-{name}.png")))?;
        }
        let _ = fs::remove_file(png);
        say(&format!("shot own-group-video-{name}.png"));
        Ok(())
    }
}

/// a's app in the sandbox: its process, its trace, its control socket.
struct AppUnderTest {
    running: Running,
    trace: TraceReader,
    socket: PathBuf,
}

impl AppUnderTest {
    fn start(sandbox: &Sandbox, (origin, ca_file): (&str, &str), dir: &Path) -> SandboxResult<Self> {
        let own = dir.join("a");
        fs::create_dir_all(&own)?;
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let socket = sandbox.path("run/ctl.sock");
        let own_text = own.to_string_lossy().into_owned();
        let socket_text = socket.to_string_lossy().into_owned();
        let child = sandbox.spawn_with_fds(
            &std::env::current_exe()?,
            &[],
            &[
                (CHILD_ENV, "a"),
                (TRACE_FD_ENV, &fd),
                (ORIGIN_ENV, origin),
                (CA_ENV, ca_file),
                (DIR_ENV, &own_text),
                (CTL_ENV, &socket_text),
            ],
            &[writer.as_fd()],
        )?;
        drop(writer);
        let mut app = Self {
            running: Running::new(child)?,
            trace: TraceReader::start(reader),
            socket,
        };
        app.running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| app.running.fail(&format!("no ready line: {error:?}")))?;
        app.trace.wait_for("status", "connected")?;
        app.trace.wait_for("calling", "registered")?;
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

    fn presented(&mut self, kind: &str) -> u64 {
        presented_in(self.catch_up(), kind)
    }

    fn wait_for(&mut self, view: &str, change: &str) -> SandboxResult {
        self.wait_until(&format!("{view} {change}"), |seen| {
            seen.iter().any(|line| line["view"] == view && line["change"] == change)
        })
    }

    fn wait_until(&mut self, what: &str, done: impl Fn(&[serde_json::Value]) -> bool) -> SandboxResult {
        let deadline = Instant::now() + WAIT;
        while !done(self.catch_up()) {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self.lines.recv_timeout(left).map_err(|_| {
                let tail: Vec<String> = self.seen.iter().rev().take(40).map(ToString::to_string).collect();
                format!("no trace line for {what}; the last lines, newest first: {tail:#?}")
            })?;
            self.seen.push(line);
        }
        Ok(())
    }
}

// --- the child ---

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let _diagnostics = app::install_log(dir.join(APP_LOG), teams_ui::settings::LogLevel::Info);
    let (origin, ca_file) = (env(ORIGIN_ENV)?, env(CA_ENV)?);
    let feed = VideoFeed::new();
    let (account, engine) = own_account(
        &dir.join("account"),
        (&origin, &ca_file),
        A.0,
        media(tone(440.0), (A_CAMERA, FILE_SCREEN), Arc::new(feed.clone())),
    )?;
    let first = ngteams::accounts::Started {
        account,
        engine,
        calls: Some(PersonalCalls {
            ringer: None,
            sounds: None,
            echo: Mri::new(teams_mock::ECHO),
            video: Some(feed),
            camera: None,
            devices: None,
            noise: teams_media::processing::NoiseControl::default(),
        }),
    };
    let runner = support::runner(&dir)?;
    let settings = SettingsFile::new(dir.join("settings.toml"));
    settings
        .save(&Settings::default())
        .map_err(|problem| format!("{problem:?}"))?;
    let ctl = PathBuf::from(env(CTL_ENV)?);
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
                theme_when_asked(launched.desktop, cx);
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

/// Each time the parent writes [`THEME_TRIGGER`]: the settings take the theme it names.
fn theme_when_asked(desktop: gpui::Entity<teams_ui::Desktop>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
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
fn fixed_theme(desktop: &gpui::Entity<teams_ui::Desktop>, theme: &'static str, cx: &mut App) {
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
