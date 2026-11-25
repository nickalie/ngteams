//! A still shared screen in the app's own call window, as a person who joins late meets it, in the UI sandbox
//! (`harness = false`: GPUI runs on the process's main thread). QA of task 577 (a share after a join or a new share).
//!
//! The parent runs the real `ngteams-server` binary (TLS with a made-up certificate authority, its users made with
//! `user add`, the media of its group calls and meetings on its own media node: `serve`, `--role all`) beside the
//! run's coturn. It also runs the clients of two made-up people, b and c, each brought up as the app brings an own
//! server's account up (`ngteams::signin::add_own`, `ngteams::engine::start`). Their screens hold still: the file
//! backend's test pattern stops after its first frames (`StillSource`), as a desktop where nothing moves. In the
//! sandbox this test runs again as a's app on the same server, serving its control socket and its view trace.
//!
//! - The a-b-c chat's call: b calls, a declines over the control socket, c answers. b shares its still screen. a joins
//!   late over the control socket (`Join`): a's stage presents b's share. The test logs the time from a's join to the
//!   first frame on a's stage and fails above [`SHOWN_BOUND`].
//! - When b's still share settles, b's and c's Call health is read two times, as the pane reads it: b's sent rate
//!   counts the headers and padding on the wire (at least [`STILL_SENT_KBPS`]), and b sends at most [`STILL_FPS`].
//! - b stops: the share leaves a's stage. b shares the same screen again: a's stage presents it. b stops and shares a
//!   still window (another size): a's stage presents it. b stops and c shares its still screen: a's stage presents it.
//! - b's meeting with a and c invited (invited people bypass the lobby): b and c join, b shares its still screen. a
//!   joins late from its calendar (`JoinMeeting`, now): a's stage presents b's share, timed as above.
//!
//! "Presents" means that the app's trace tells the share tile shown or presented, and then at least 30 frames more
//! presented. A still screen sends no frames of its own, so only the sender's repeats of its last picture keep the
//! share on the stage (a tile with no frame for 2 s goes).
//!
//! One shot of a's call window for each step (Teams Light, 1280 px wide) under `target/ui-shots/qa-share-still-*.png`
//! and `$QA_SHARE_STILL_EVIDENCE`, if set.
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
use teams_core::{CallAction, CallFloor, CallHealth, CallId, CallMedia, CallStatus, EventWrite, Relay, VideoOutput};
use teams_ctl::protocol::{CallRow, Request, Response};
use teams_ctl::{Client, Timeouts};
use teams_media::devices::file::{AudioInput, FileDevices, FrameCounter, Pacing, PatternCamera, StillSource};
use teams_media::devices::{AudioSink, AudioSource, DeviceError, Devices, VideoSink, VideoSource};
use teams_media::processing::ProcessingConfig;
use teams_media::session::{SCREEN_IDLE_REPEAT, SCREEN_SETTLE, START_KEYFRAMES_FOR, SessionConfig};
use teams_media::tone::Tone;
use teams_proto::{CalendarEvent, EventAttendee, EventDraft, EventShowAs, Mri, Organizer, ZonedTime};
use teams_store::OutboxState;
use teams_ui::settings::Settings;
use teams_ui::video::TRACE_EVERY;
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

const CHILD_ENV: &str = "NGTEAMS_QA_SHARE_STILL_TEST";
const TRACE_FD_ENV: &str = "NGTEAMS_QA_SHARE_STILL_TRACE_FD";
const ORIGIN_ENV: &str = "NGTEAMS_QA_SHARE_STILL_ORIGIN";
const CA_ENV: &str = "NGTEAMS_QA_SHARE_STILL_CA";
const DIR_ENV: &str = "NGTEAMS_QA_SHARE_STILL_DIR";
const CTL_ENV: &str = "NGTEAMS_QA_SHARE_STILL_CTL";
/// Where the shots are copied besides `target/ui-shots/`, if set (the QA run's evidence).
const EVIDENCE_ENV: &str = "QA_SHARE_STILL_EVIDENCE";
const COTURN_ENV: &str = "NGTS_TEST_COTURN";
const COTURN_SECRET_ENV: &str = "NGTS_TEST_COTURN_SECRET";
const QUIT_TRIGGER: &str = "quit";
/// The file in the sandbox dir the parent writes a theme's key to (`teams_ui::theme`).
const THEME_TRIGGER: &str = "theme";

const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(60);
/// How long a change takes to settle on the screen before a shot.
const SETTLE: Duration = Duration::from_millis(1500);
/// The longest time from a's join to the first frame of the share on a's stage. The fix aims at 2 s; a loaded host
/// gets more.
const SHOWN_BOUND: Duration = Duration::from_secs(10);
/// What the fix aims at: the test logs whether a join met it.
const SHOWN_AIM: Duration = Duration::from_secs(2);
/// How long the shared screen holds still before a joins (the source stops after [`STILL_FRAMES`] frames).
const STILL_FOR: Duration = Duration::from_secs(3);
/// What [`gone_for`] adds to the time of [`TRACE_EVERY`] settled repeats: a late frame, a loaded host.
const GONE_MARGIN: Duration = Duration::from_secs(10);
/// The frames a still source gives before it holds still.
const STILL_FRAMES: u32 = 3;
/// The time between the two reads of Call health: the pane's rates are counted between two reads.
const HEALTH_SPAN: Duration = Duration::from_secs(5);
/// The least sent rate Call health may show for a still share: its RTP headers and padding go on the wire (task 577
/// QA, Q3: the pane showed 0.3 kbit/s of payload while about 117 kbit/s went out).
const STILL_SENT_KBPS: f64 = 5.0;
/// The most frames each second Call health may show for a still share that settled (one each [`SCREEN_IDLE_REPEAT`]).
const STILL_FPS: f64 = 2.5;
const SHARE: &str = "share";
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
const SCREEN: (u32, u32) = (1920, 1080);
/// The call window's size in the shots.
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
const MINUTE: i64 = 60_000;
/// The client id the native client signs in as (the middle tier's token for the options page).
const MIDDLE_TIER: &str = "https://api.spaces.skype.com";

/// The people with a client: user name, display name.
const A: (&str, &str) = ("ada@own.example.test", "Ada Ł. Lovelace");
const B: (&str, &str) = ("bert@own.example.test", "Bert 🎺 Bach");
const C: (&str, &str) = ("cora@own.example.test", "Cora 'Cee' Nuñez");
const TOPIC: &str = "Q4 ✨ «planning» & co";
const SUBJECT: &str = "Design review 🖥️ «still» & co";
/// The still window b shares in the third share: another shape than the screen's 1280×720.
const WINDOW: PatternCamera = PatternCamera {
    width: 960,
    height: 720,
    fps: 15,
};
const fn tone(hz: f32) -> Tone {
    Tone { hz, amplitude: 0.3 }
}

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa share still own window test: {error}");
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

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::UNIX_EPOCH
            .elapsed()
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(0)
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

/// A call's media on `devices`, nothing recorded.
fn media(devices: Arc<dyn Devices + Send + Sync>, output: Arc<dyn VideoOutput>) -> CallMedia {
    CallMedia {
        devices,
        // as a shipped client's: an own server's account's media goes through the relay its server names
        relay: Relay::Teams,
        session: SessionConfig {
            processing: ProcessingConfig::OFF,
            ..Default::default()
        },
        video: Some(call_video(output)),
    }
}

fn file_devices(tone: Tone, camera: PatternCamera) -> FileDevices {
    FileDevices {
        input: AudioInput::Tone(tone),
        recording: None,
        camera,
        pacing: Pacing::RealTime,
    }
}

/// A meeting `organizer` schedules now (it began five minutes ago, for an hour), with `invited` (MRI, name).
fn meeting(subject: &str, organizer: &str, invited: &[(String, String)]) -> EventDraft {
    let start = now_ms() - 5 * MINUTE;
    let at = |ms| ZonedTime { ms, offset_minutes: 0 };
    EventDraft {
        subject: subject.into(),
        start: at(start),
        end: at(start + 60 * MINUTE),
        time_zone: None,
        zone: Some("UTC".into()),
        all_day: false,
        organizer: Organizer {
            mri: organizer.to_owned(),
            address: None,
            name: None,
        },
        attendees: invited
            .iter()
            .map(|(mri, name)| EventAttendee {
                address: format!("{}@own.example.test", mri.trim_start_matches("8:orgid:")),
                name: Some(name.clone()),
                mri: Some(mri.clone()),
                required: true,
            })
            .collect(),
        location: String::new(),
        body_html: String::new(),
        online: true,
        show_as: EventShowAs::Busy,
        reminder_minutes: Some(15),
        private: false,
        categories: Vec::new(),
        recurrence: None,
    }
}

// --- the parent ---

/// What a person picks to share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shared {
    /// The whole screen: the test pattern at the file backend's screen size.
    Screen,
    /// One window: the test pattern at [`WINDOW`]'s size.
    Window,
}

/// The file backend's devices, with a shared screen or window that holds still after its first frames.
struct StillDevices {
    files: FileDevices,
    window: FileDevices,
    pick: Arc<Mutex<Shared>>,
}

impl Devices for StillDevices {
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
        let source = match *self.pick.lock().unwrap_or_else(PoisonError::into_inner) {
            Shared::Screen => self.files.screen()?,
            Shared::Window => self.window.camera()?,
        };
        Ok(Box::new(StillSource::new(source, STILL_FRAMES)))
    }
}

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
    upn: &'static str,
    mri: String,
    engine: AccountEngine<MemorySecretStore>,
    /// What the next share takes.
    pick: Arc<Mutex<Shared>>,
}

impl Peer {
    fn start(
        dir: &Path,
        server: (&str, &str),
        (upn, name): (&'static str, &'static str),
        hz: f32,
    ) -> SandboxResult<Self> {
        let pick = Arc::new(Mutex::new(Shared::Screen));
        let devices = StillDevices {
            files: file_devices(tone(hz), PatternCamera::default()),
            window: file_devices(tone(hz), WINDOW),
            pick: Arc::clone(&pick),
        };
        let (account, engine) = own_account(dir, server, upn, media(Arc::new(devices), Arc::new(Counted::default())))?;
        let mri = account.mri().ok_or("no MRI")?.as_str().to_owned();
        Ok(Self {
            name,
            upn,
            mri,
            engine,
            pick,
        })
    }

    fn picks(&self, shared: Shared) {
        *self.pick.lock().unwrap_or_else(PoisonError::into_inner) = shared;
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

    /// Share or Stop sharing in `call`.
    fn share(&self, runtime: &tokio::runtime::Runtime, call: CallId, on: bool) -> SandboxResult {
        self.act(runtime, &CallAction::Share { call, on })?;
        say(&format!("{}: sharing {on}", self.name));
        Ok(())
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

    /// Until `call` comes to `wanted` (an end first fails).
    fn status(&mut self, runtime: &tokio::runtime::Runtime, call: CallId, wanted: &CallStatus) -> SandboxResult {
        let what = format!("{wanted:?}");
        self.next(runtime, &what, |event| match event {
            Event::Call { call: of, status } if *of == call && status == wanted => Some(Ok(())),
            Event::Call {
                call: of,
                status: CallStatus::Ended(end),
            } if *of == call => Some(Err(format!("the call ended: {end:?}"))),
            _ => None,
        })?
        .map_err(Into::into)
    }

    /// The Call health of `call`, as the pane reads it (its rates count from this client's last read).
    fn health(&self, runtime: &tokio::runtime::Runtime, call: CallId) -> SandboxResult<CallHealth> {
        runtime
            .block_on(self.engine.handle.requests().call_health(call))?
            .ok_or_else(|| format!("{}: no Call health in the call", self.name).into())
    }

    /// Until this client shows another's share in `call`.
    fn sees_share(&mut self, runtime: &tokio::runtime::Runtime, call: CallId) -> SandboxResult {
        self.next(runtime, "a share shown", |event| match event {
            Event::CallRemoteShare { call: of, on: true } if *of == call => Some(()),
            _ => None,
        })
    }

    /// The meeting chat's header's Join: the meeting read from its chat.
    fn join_from_chat(&self, runtime: &tokio::runtime::Runtime, chat: &str) -> SandboxResult<CallAction> {
        let details = runtime
            .block_on(self.engine.handle.meeting(chat.to_owned()))?
            .ok_or("the chat names no meeting")?;
        Ok(CallAction::JoinMeeting {
            chat: chat.to_owned(),
            meeting: details.meeting,
        })
    }

    /// The organizer saves "Who can bypass the lobby" as `value` on the meeting's options page (the native client opens
    /// that page in a browser).
    fn bypass(
        &self,
        runtime: &tokio::runtime::Runtime,
        (origin, ca_file): (&str, &str),
        event: &CalendarEvent,
        value: &str,
    ) -> SandboxResult {
        let info = event.meeting.as_ref().ok_or("no meeting")?;
        let chat = event.meeting_chat.as_ref().ok_or("no meeting chat")?;
        let oid = self.mri.trim_start_matches("8:orgid:");
        let url = format!(
            "{origin}/api/mt/part/own-01/beta/meetings/v1/options/{}/{oid}/{chat}/0/0",
            info.tenant_id
        );
        let roots = reqwest::Certificate::from_pem_bundle(&fs::read(ca_file)?)?;
        let http = reqwest::Client::builder().no_proxy().tls_certs_merge(roots).build()?;
        let body = serde_json::json!({"options": [{"name": "AutoAdmittedUsers", "currentValue": value}]});
        let status = runtime.block_on(async {
            let token = self.engine.session.access_token(MIDDLE_TIER).await?;
            let answer = http
                .post(&url)
                .bearer_auth(token.token.expose())
                .header("content-type", "application/json")
                .body(body.to_string())
                .send()
                .await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(answer.status().as_u16())
        });
        let status = status.map_err(|error| error.to_string())?;
        if status != 200 {
            return Err(format!("{}: the options page saved nothing: {status}", self.upn).into());
        }
        Ok(())
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
    let Some(sandbox) = sandbox_or_skip("ngteams qa share still", &options)? else {
        return Ok(());
    };
    let tables = format!(
        "[calling]\nring_timeout = 45\n[calling.turn]\nsecret = {secret:?}\nurls = [\"turn:{relay}?transport=udp\"]\n\
         [mixer]\nudp_listen = \"127.0.0.1:0\"\n"
    );
    let server = BinaryServer::start_with(&binary, &tables)?;
    for (upn, name) in [A, B, C] {
        server.add_user(upn, name)?;
    }
    let ca = server.ca_file.to_string_lossy().into_owned();
    let at = (server.origin.as_str(), ca.as_str());
    let dir = tempfile::tempdir()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mut b = Peer::start(&dir.path().join("b"), at, B, 660.0)?;
    let mut c = Peer::start(&dir.path().join("c"), at, C, 880.0)?;
    let mut app = AppUnderTest::start(&sandbox, at, dir.path())?;
    let outcome = Rig::new(&sandbox, &mut app, &runtime).and_then(|mut rig| {
        let result = scenario(&mut rig, (&mut b, &mut c), at);
        rig.done();
        result
    });
    let log = server.log();
    for dir in evidence_dirs() {
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(dir.join("qa-share-still-server.log"), &log);
    }
    let outcome = outcome.map_err(|error| {
        format!(
            "{error}\n--- the app: {}\n--- the sandbox{}",
            app.running.fail(&"killed"),
            sandbox.logs()
        )
    });
    for peer in [b, c] {
        peer.engine.handle.shutdown();
    }
    outcome?;
    app.quit(&sandbox)?;
    say("ngteams qa share still own window: ok");
    Ok(())
}

fn scenario(rig: &mut Rig<'_>, (b, c): (&mut Peer, &mut Peer), server: (&str, &str)) -> SandboxResult {
    rig.theme(teams_ui::theme::LIGHT_KEY)?;
    let a_mri = rig.own_mri()?;
    let calls = late_join_call(rig, (b, c), &a_mri)?;
    still_share_health(rig.runtime, (b, c), calls)?;
    shares_again(rig, (b, c), calls)?;
    late_join_meeting(rig, (b, c), &a_mri, server)?;
    for line in &rig.times {
        say(line);
    }
    Ok(())
}

/// b calls the a-b-c chat; a declines, c answers; b shares its still screen; a joins late and sees it. b's and c's
/// calls.
fn late_join_call(rig: &mut Rig<'_>, (b, c): (&mut Peer, &mut Peer), a_mri: &str) -> SandboxResult<(CallId, CallId)> {
    let runtime = rig.runtime;
    let row = runtime.block_on(b.engine.handle.write(Action::CreateChat {
        others: vec![Mri::new(a_mri), Mri::new(c.mri.as_str())],
        topic: Some(TOPIC.into()),
    }))?;
    if row.state != OutboxState::Sent {
        return Err(format!("the chat wasn't made: {row:?}").into());
    }
    let chat = row.message_id.ok_or("the chat's id")?;
    runtime.block_on(c.engine.handle.sync())?;
    thread::sleep(Duration::from_secs(2));
    let b_call = b.act(runtime, &CallAction::Call { chat: chat.clone() })?;
    b.status(runtime, b_call, &CallStatus::Connected)?;
    rig.wait_call("a's ring", |row| row.ringing)?;
    rig.ctl.request(None, Request::Decline)?;
    say("a declined b's ring");
    let rung = c.next(runtime, "c's ring", |event| match event {
        Event::IncomingCall(incoming) => Some(incoming.call),
        _ => None,
    })?;
    let c_call = c.act(runtime, &CallAction::Answer { call: rung })?;
    c.status(runtime, c_call, &CallStatus::Connected)?;
    b.share(runtime, b_call, true)?;
    c.sees_share(runtime, c_call)?;
    thread::sleep(STILL_FOR);
    rig.ctl.request(None, Request::Sync)?;
    rig.join_late(
        "call-late-join",
        "b's still screen after a's late join",
        Request::Join { chat },
    )?;
    Ok((b_call, c_call))
}

/// The Call health of b's still share after it settled, read as the pane reads it: b's sent rate counts what goes on
/// the wire (headers and padding too), and b's frame rate settles to a repeat each [`SCREEN_IDLE_REPEAT`].
fn still_share_health(
    runtime: &tokio::runtime::Runtime,
    (b, c): (&mut Peer, &mut Peer),
    (b_call, c_call): (CallId, CallId),
) -> SandboxResult {
    thread::sleep(START_KEYFRAMES_FOR + SCREEN_SETTLE);
    b.health(runtime, b_call)?;
    c.health(runtime, c_call)?;
    thread::sleep(HEALTH_SPAN);
    let sent = b
        .health(runtime, b_call)?
        .sharing
        .sent
        .ok_or("b's Call health: no sent screen share")?;
    let received = c.health(runtime, c_call)?.sharing.received;
    say(&format!(
        "Call health of b's still share: b sent {:?} kbit/s at {:?} fps, {:?}; c received {:?} kbit/s at {:?} fps",
        sent.bitrate_kbps,
        sent.frame_rate,
        sent.resolution,
        received.as_ref().and_then(|stream| stream.bitrate_kbps),
        received.as_ref().and_then(|stream| stream.frame_rate),
    ));
    let kbps = sent.bitrate_kbps.ok_or("b's Call health: no sent rate")?;
    if kbps < STILL_SENT_KBPS {
        return Err(format!("b's Call health shows {kbps} kbit/s sent for a still share").into());
    }
    let fps = sent.frame_rate.ok_or("b's Call health: no sent frame rate")?;
    if fps > STILL_FPS {
        return Err(format!("b's still share did not settle: {fps} fps sent").into());
    }
    Ok(())
}

/// b stops and shares the same screen again, then a still window; then c shares: a's stage presents each share.
fn shares_again(
    rig: &mut Rig<'_>,
    (b, c): (&mut Peer, &mut Peer),
    (b_call, c_call): (CallId, CallId),
) -> SandboxResult {
    let runtime = rig.runtime;
    b.share(runtime, b_call, false)?;
    rig.share_gone("b stopped sharing")?;
    rig.save("stopped")?;
    rig.shown_after("shared-again", "b's same still screen shared again", || {
        b.share(runtime, b_call, true)
    })?;
    b.share(runtime, b_call, false)?;
    rig.share_gone("b stopped sharing its screen")?;
    b.picks(Shared::Window);
    rig.shown_after("window", "b's still window", || b.share(runtime, b_call, true))?;
    b.share(runtime, b_call, false)?;
    rig.share_gone("b stopped sharing its window")?;
    b.picks(Shared::Screen);
    rig.shown_after("c-shares", "c's still screen after b's share", || {
        c.share(runtime, c_call, true)
    })?;
    c.share(runtime, c_call, false)?;
    rig.share_gone("c stopped sharing")?;
    rig.ctl.request(None, Request::HangUp)?;
    c.act(runtime, &CallAction::HangUp { call: c_call })?;
    let _ = b.act(runtime, &CallAction::HangUp { call: b_call });
    rig.wait_no_call("the call over")
}

/// b's meeting with a and c invited: b and c join, b shares its still screen, a joins late from its calendar.
fn late_join_meeting(
    rig: &mut Rig<'_>,
    (b, c): (&mut Peer, &mut Peer),
    a_mri: &str,
    server: (&str, &str),
) -> SandboxResult {
    let runtime = rig.runtime;
    let invited = [(a_mri.to_owned(), A.1.to_owned()), (c.mri.clone(), C.1.to_owned())];
    let draft = meeting(SUBJECT, &b.mri, &invited);
    let written = runtime.block_on(b.engine.handle.write_event(EventWrite::Create(draft)))?;
    let event = written.created.ok_or("b's meeting wasn't made")?;
    let chat = event.meeting_chat.clone().ok_or("no meeting chat")?;
    b.bypass(runtime, server, &event, "InvitedUsers")?;
    runtime.block_on(c.engine.handle.sync())?;
    let join = b.join_from_chat(runtime, &chat)?;
    let b_call = b.act(runtime, &join)?;
    b.status(runtime, b_call, &CallStatus::Connected)?;
    let join = c.join_from_chat(runtime, &chat)?;
    let c_call = c.act(runtime, &join)?;
    c.status(runtime, c_call, &CallStatus::Connected)?;
    b.share(runtime, b_call, true)?;
    c.sees_share(runtime, c_call)?;
    thread::sleep(STILL_FOR);
    let id = rig.event_id(SUBJECT)?;
    rig.join_late(
        "meeting-late-join",
        "b's still screen after a's late join to the meeting",
        Request::JoinMeeting { event: id, now: true },
    )?;
    b.share(runtime, b_call, false)?;
    rig.ctl.request(None, Request::HangUp)?;
    c.act(runtime, &CallAction::HangUp { call: c_call })?;
    let _ = b.act(runtime, &CallAction::HangUp { call: b_call });
    rig.wait_no_call("the meeting left")
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

fn is_share_tile(line: &serde_json::Value) -> bool {
    line["view"] == "video-tile" && strings(line, "kinds") == [SHARE]
}

fn is_share_presented(line: &serde_json::Value) -> bool {
    is_share_tile(line) && line["change"] == "presented"
}

/// The most frames of the share tile of `call` the trace told presented so far. Each call window counts from 1.
fn presented_in(seen: &[Seen], call: Option<u64>) -> u64 {
    seen.iter()
        .filter(|seen| is_share_presented(&seen.line) && seen.line["call"].as_u64() == call)
        .filter_map(|seen| seen.line["frames"].as_u64())
        .max()
        .unwrap_or_default()
}

/// The first sign of a share in `seen`: the share tile shown or presented.
fn first_share_sign(seen: &[Seen]) -> Option<&Seen> {
    seen.iter().find(|seen| {
        is_share_tile(&seen.line) && (seen.line["change"] == "shown" || seen.line["change"] == "presented")
    })
}

/// The share tile's lines in `seen`, each with its time from `first` in ms ("presented 30 +6012").
fn share_timeline(seen: &[Seen], first: Instant) -> String {
    let lines: Vec<String> = seen
        .iter()
        .filter(|seen| is_share_tile(&seen.line))
        .map(|seen| {
            let at = i128::try_from(seen.at.saturating_duration_since(first).as_millis()).unwrap_or(i128::MAX)
                - i128::try_from(first.saturating_duration_since(seen.at).as_millis()).unwrap_or(i128::MAX);
            let frames = seen.line["frames"]
                .as_u64()
                .map(|frames| format!(" {frames}"))
                .unwrap_or_default();
            format!("{}{frames} {at:+}", seen.line["change"].as_str().unwrap_or("?"))
        })
        .collect();
    lines.join(", ")
}

/// How long no traced share frame may come to a's stage before the share counts as gone. A still screen that settled
/// goes again only each [`SCREEN_IDLE_REPEAT`] and the trace tells each [`TRACE_EVERY`]th frame, so a share that stays
/// crosses a traced count in this time, and two in twice this time.
fn gone_for() -> Duration {
    SCREEN_IDLE_REPEAT.saturating_mul(u32::try_from(TRACE_EVERY).unwrap_or(u32::MAX)) + GONE_MARGIN
}

fn evidence_dirs() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    let mut dirs = vec![dir];
    dirs.extend(std::env::var_os(EVIDENCE_ENV).map(PathBuf::from));
    dirs
}

struct Rig<'a> {
    sandbox: &'a Sandbox,
    app: &'a mut AppUnderTest,
    runtime: &'a tokio::runtime::Runtime,
    ctl: Client,
    /// niri's id of a's main window.
    main_id: Option<u64>,
    /// niri's id of the call window.
    window_id: Option<u64>,
    shots: usize,
    /// The times the run measured, said again at its end.
    times: Vec<String>,
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
        let mut rig = Self {
            sandbox,
            app,
            runtime,
            ctl,
            main_id: None,
            window_id: None,
            shots: 0,
            times: Vec::new(),
        };
        rig.main_id = rig.windows()?.iter().filter_map(|window| window["id"].as_u64()).min();
        Ok(rig)
    }

    fn done(&mut self) {
        say(&format!("{} shots taken", self.shots));
    }

    /// a's MRI, as the app's accounts tell it.
    fn own_mri(&mut self) -> SandboxResult<String> {
        match self.ctl.request(None, Request::Accounts)? {
            Response::Accounts { accounts } => Ok(accounts.first().ok_or("no account")?.mri.clone()),
            other => Err(format!("accounts: {other:?}").into()),
        }
    }

    /// The id of the calendar's event `subject`, once a's calendar has it.
    fn event_id(&mut self, subject: &str) -> SandboxResult<String> {
        let deadline = Instant::now() + WAIT;
        loop {
            let answer = self.ctl.request(None, Request::Calendar { from: None, to: None })?;
            if let Response::Calendar { events } = answer
                && let Some(row) = events.iter().find(|row| row.subject == subject)
            {
                say(&format!("a's calendar: {subject:?} joinable {}", row.joinable));
                return Ok(row.id.clone());
            }
            if Instant::now() > deadline {
                return Err(format!("{subject:?} isn't in a's calendar").into());
            }
            thread::sleep(Duration::from_millis(500));
        }
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

    fn wait_no_call(&mut self, what: &str) -> SandboxResult {
        let deadline = Instant::now() + WAIT;
        loop {
            let calls = self.calls()?;
            if calls.iter().all(|row| row.state == "ended") {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("{what}: the calls are {calls:?}").into());
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// a joins late with `request`: a's stage presents the share; the time from the join to its first frame; a shot.
    fn join_late(&mut self, name: &str, what: &str, request: Request) -> SandboxResult {
        let mark = self.app.trace.mark();
        let pressed = Instant::now();
        self.ctl.request(None, request)?;
        self.wait_call("a in the call", |row| row.state == "connected")?;
        let connected = Instant::now();
        self.app.trace.wait_after(mark, "call-window opened", |line| {
            line["view"] == "call-window" && line["change"] == "opened"
        })?;
        let first = self.presents_share(what, mark)?;
        let after_press = first.saturating_duration_since(pressed);
        let line = format!(
            "{what}: the first frame on a's stage {after_press:?} after a pressed Join ({:?} after the app told the \
             call connected); aim {SHOWN_AIM:?}: {}",
            first.saturating_duration_since(connected),
            if after_press <= SHOWN_AIM { "met" } else { "missed" }
        );
        say(&line);
        self.times.push(line);
        if after_press > SHOWN_BOUND {
            return Err(format!("{what}: shown {after_press:?} after the join, above {SHOWN_BOUND:?}").into());
        }
        self.find_window()?;
        self.resize()?;
        self.save(name)
    }

    /// `press` starts a share: a's stage presents it; the time from the press to its first sign; a shot.
    fn shown_after(&mut self, name: &str, what: &str, press: impl FnOnce() -> SandboxResult) -> SandboxResult {
        let mark = self.app.trace.mark();
        let pressed = Instant::now();
        press()?;
        let first = self.presents_share(what, mark)?;
        let line = format!(
            "{what}: the first sign on a's stage {:?} after the press",
            first.saturating_duration_since(pressed)
        );
        say(&line);
        self.times.push(line);
        self.save(name)
    }

    /// Until a's stage presents the share: a sign of it after trace line `mark` (the share before it is gone), then at
    /// least [`TRACE_EVERY`] frames more than its call window had presented before `mark`. When the first sign came.
    fn presents_share(&mut self, what: &str, mark: usize) -> SandboxResult<Instant> {
        self.app
            .trace
            .wait_until(what, |seen| first_share_sign(&seen[mark..]).is_some())?;
        let seen = self.app.trace.catch_up();
        let sign = first_share_sign(&seen[mark..]).ok_or("no sign of the share")?;
        let (first, call) = (sign.at, sign.line["call"].as_u64());
        // past two traced counts: at least TRACE_EVERY frames presented since `mark`
        let wanted = presented_in(&seen[..mark], call) + 2 * TRACE_EVERY;
        self.app
            .trace
            .wait_within(gone_for() * 2, &format!("{what}: {wanted} frames presented"), |seen| {
                presented_in(seen, call) >= wanted
            })?;
        say(&format!(
            "{what}: {wanted} frames of the share presented in call {call:?} {:?} after its first sign; the share \
             tile's lines: {}",
            first.elapsed(),
            share_timeline(&self.app.trace.catch_up()[mark..], first)
        ));
        Ok(first)
    }

    /// Until no traced share frame comes to a's stage for [`gone_for`] (the stage draws no share tile then, so nothing
    /// says it hidden). A frame still on its way at the stop can cross a count in the first time, not in a second one.
    fn share_gone(&mut self, what: &str) -> SandboxResult {
        let gone_for = gone_for();
        for _ in 0..2 {
            let before = self.app.trace.presented();
            thread::sleep(gone_for);
            if self.app.trace.presented() == before {
                say(&format!("{what}: the share left a's stage"));
                return Ok(());
            }
        }
        Err(format!("{what}: the share is still presented").into())
    }

    fn windows(&self) -> SandboxResult<Vec<serde_json::Value>> {
        let text = self.sandbox.niri_msg(&["-j", "windows"])?;
        let windows: serde_json::Value = serde_json::from_str(&text)?;
        Ok(windows.as_array().cloned().unwrap_or_default())
    }

    /// The newest window beside the main one: the call window.
    fn find_window(&mut self) -> SandboxResult {
        let windows = self.windows()?;
        let newest = windows
            .iter()
            .filter_map(|window| window["id"].as_u64())
            .filter(|id| Some(*id) != self.main_id)
            .max()
            .ok_or("no window beside the main one")?;
        let title = windows
            .iter()
            .find(|window| window["id"].as_u64() == Some(newest))
            .map(|window| window["title"].to_string());
        say(&format!("the call window {newest}: {title:?}"));
        self.window_id = Some(newest);
        Ok(())
    }

    fn focus(&mut self) -> SandboxResult {
        if let Some(id) = self.window_id {
            self.sandbox
                .niri_msg(&["action", "focus-window", "--id", &id.to_string()])?;
        }
        Ok(())
    }

    /// The compositor sizes the call window [`WIDTH`] × [`HEIGHT`], as a user's drag does.
    fn resize(&mut self) -> SandboxResult {
        self.focus()?;
        self.sandbox
            .niri_msg(&["action", "set-column-width", &WIDTH.to_string()])?;
        self.sandbox
            .niri_msg(&["action", "set-window-height", &HEIGHT.to_string()])?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn theme(&mut self, key: &str) -> SandboxResult {
        fs::write(self.sandbox.path(THEME_TRIGGER), key)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// Saves the call window alone as `qa-share-still-<name>.png` under `target/ui-shots/` and the evidence dir.
    fn save(&mut self, name: &str) -> SandboxResult {
        self.focus()?;
        thread::sleep(Duration::from_millis(300));
        self.shots += 1;
        let png = self.sandbox.screenshot(&format!("shot-{}.png", self.shots), true)?;
        for dir in evidence_dirs() {
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(format!("qa-share-still-{name}.png")))?;
        }
        let _ = fs::remove_file(png);
        say(&format!("shot qa-share-still-{name}.png"));
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
        app.trace.wait_after(0, "status connected", |line| {
            line["view"] == "status" && line["change"] == "connected"
        })?;
        app.trace.wait_after(0, "calling registered", |line| {
            line["view"] == "calling" && line["change"] == "registered"
        })?;
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

/// A trace line and when the parent read it.
struct Seen {
    at: Instant,
    line: serde_json::Value,
}

/// The child's view trace, line by line as it comes, each line with the time it came.
struct TraceReader {
    lines: mpsc::Receiver<Seen>,
    seen: Vec<Seen>,
}

impl TraceReader {
    fn start(reader: io::PipeReader) -> Self {
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for text in BufReader::new(reader).lines().map_while(Result::ok) {
                let line = serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
                let _ = sender.send(Seen {
                    at: Instant::now(),
                    line,
                });
            }
        });
        Self {
            lines,
            seen: Vec::new(),
        }
    }

    fn catch_up(&mut self) -> &[Seen] {
        while let Ok(seen) = self.lines.try_recv() {
            self.seen.push(seen);
        }
        &self.seen
    }

    /// Where the lines that come from now on start.
    fn mark(&mut self) -> usize {
        self.catch_up().len()
    }

    /// How many lines told share frames presented so far: it grows while a share is on the stage.
    fn presented(&mut self) -> usize {
        self.catch_up()
            .iter()
            .filter(|seen| is_share_presented(&seen.line))
            .count()
    }

    /// Until a line from `mark` on is one `wanted` takes.
    fn wait_after(&mut self, mark: usize, what: &str, wanted: impl Fn(&serde_json::Value) -> bool) -> SandboxResult {
        self.wait_until(what, |seen| seen[mark..].iter().any(|seen| wanted(&seen.line)))
    }

    fn wait_until(&mut self, what: &str, done: impl Fn(&[Seen]) -> bool) -> SandboxResult {
        self.wait_within(WAIT, what, done)
    }

    fn wait_within(&mut self, bound: Duration, what: &str, done: impl Fn(&[Seen]) -> bool) -> SandboxResult {
        let deadline = Instant::now() + bound;
        while !done(self.catch_up()) {
            let left = deadline.saturating_duration_since(Instant::now());
            let seen = self.lines.recv_timeout(left).map_err(|_| {
                let tail: Vec<String> = self
                    .seen
                    .iter()
                    .rev()
                    .take(40)
                    .map(|seen| seen.line.to_string())
                    .collect();
                format!("no trace line for {what}; the last lines, newest first: {tail:#?}")
            })?;
            self.seen.push(seen);
        }
        Ok(())
    }
}

// --- the child ---

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let (origin, ca_file) = (env(ORIGIN_ENV)?, env(CA_ENV)?);
    let feed = VideoFeed::new();
    let devices = file_devices(tone(440.0), PatternCamera::default());
    let (account, engine) = own_account(
        &dir.join("account"),
        (&origin, &ca_file),
        A.0,
        media(Arc::new(devices), Arc::new(feed.clone())),
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
