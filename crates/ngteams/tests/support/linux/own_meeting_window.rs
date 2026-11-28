//! An own server's meetings in the app's windows, as a user meets them, in the UI sandbox (`harness = false`: GPUI
//! runs on the process's main thread). QA of task 298 (meetings on the own server).
//!
//! The parent runs the real `ngteams-server` binary (TLS with a made-up certificate authority, `serve --role all` on
//! SQLite, the run's coturn as its relay, its media node on 127.0.0.1) with made-up accounts a, b and c and more
//! made-up people of odd names. b and c are clients in the parent, each brought up as the app brings an own server's
//! account up (`ngteams::signin::add_own`, `ngteams::engine::start`), a tone each on the file devices. In the sandbox
//! this test runs again as a's app on the same server, serving its control socket and its view trace.
//!
//! - b schedules a meeting with a, c and the made-up people invited, "only the organizer" bypasses the lobby. a opens
//!   the meeting's pre-join from its calendar (shots), presses Enter (Join now) and waits in the lobby (shots). b comes
//!   and admits a; c comes from its card and b admits it. b and c raise their hands (shots of a's meeting). b's Mute
//!   all: a sees "You've been muted" (shots). b's End meeting ends a's call; a's meeting chat shows the start and the
//!   end (shots of the main window).
//! - a schedules a meeting (on a second client of a's own) with b invited, "only invited people" bypass the lobby. a
//!   joins it; c, not invited, joins by its ID and waits: a's call window shows the lobby's card (shots); a clicks
//!   Admit, and c comes in.
//! - b's Meet now in a channel of a team of b's, a and c on it, titled from its pre-join: a's channel shows the post
//!   (shots); the post's Join opens the pre-join (shots), Escape closes it; a joins from the header's Join; everyone
//!   leaves; the post shows the end (shots). Then an untitled Meet now and one titled with 200 characters: the post,
//!   the pre-join and the call (shots).
//!
//! Each shot is taken in Teams Light and Dark, the window 1280 and 480 px wide (the call window's minimum is wider:
//! such a shot is named `-is-<width>`), under `target/ui-shots/own-meeting-*.png` and `$OWN_MEETING_EVIDENCE`, if set.
//!
//! Without weston 15, niri or `dbus-daemon`, without a coturn (`NGTS_TEST_COTURN`, `NGTS_TEST_COTURN_SECRET`) or
//! without the server binary (`NGTEAMS_QA_SERVER_BIN`, else `ngteams-server` beside the test's target directory) it
//! says so and passes.

use std::fmt::Write as _;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, thread};

use gpui::{App, AsyncApp, WindowingModes};
use image::RgbImage;
use ngteams::accounts_file::{self, PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::calling::PersonalCalls;
use ngteams::descriptors::adopt_trace;
use ngteams::setup::Setup;
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_core::account::{AccountCalls, AccountEngine, call_video, web_client_info};
use teams_core::engine::Event;
use teams_core::{CallAction, CallFloor, CallId, CallMedia, CallStatus, EventWrite, Relay, VideoOutput};
use teams_ctl::protocol::{CallRow, OpenTarget, Request, Response};
use teams_ctl::{Client, Timeouts};
use teams_media::devices::VideoSink;
use teams_media::devices::file::{AudioInput, FileDevices, FrameCounter, Pacing, PatternCamera};
use teams_media::processing::ProcessingConfig;
use teams_media::session::SessionConfig;
use teams_media::tone::Tone;
use teams_proto::{
    CalendarEvent, EventAttendee, EventDraft, EventShowAs, Mri, NewTeam, Organizer, TeamRole, TeamWrite, ZonedTime,
};
use teams_ui::settings::Settings;
use teams_ui::{SettingsFile, VideoFeed};
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "../own_server_binary.rs"]
#[expect(
    dead_code,
    reason = "the own server's tests share this support; this one starts it with tables"
)]
mod own_server_binary;
#[path = "call_pixels.rs"]
#[expect(
    dead_code,
    reason = "the call window tests share this support; this one finds one button with it"
)]
mod pixels;
#[path = "../window.rs"]
#[expect(
    dead_code,
    reason = "the GUI tests share this support; this one uses only its runner"
)]
mod support;
use own_server_binary::{BinaryServer, PASSWORD};

const CHILD_ENV: &str = "NGTEAMS_OWN_MEETING_TEST";
const TRACE_FD_ENV: &str = "NGTEAMS_OWN_MEETING_TRACE_FD";
const ORIGIN_ENV: &str = "NGTEAMS_OWN_MEETING_ORIGIN";
const CA_ENV: &str = "NGTEAMS_OWN_MEETING_CA";
const DIR_ENV: &str = "NGTEAMS_OWN_MEETING_DIR";
const CTL_ENV: &str = "NGTEAMS_OWN_MEETING_CTL";
/// Where the shots are copied besides `target/ui-shots/`, if set (the QA run's evidence).
const EVIDENCE_ENV: &str = "OWN_MEETING_EVIDENCE";
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
/// The windows' widths the shots are taken at, and their height.
const WIDTHS: [i32; 2] = [1280, 480];
const HEIGHT: i32 = 800;
const MINUTE: i64 = 60_000;
/// The client id the native client signs in as (the middle tier's token for the options page).
const MIDDLE_TIER: &str = "https://api.spaces.skype.com";

/// The people with a client: user name, display name.
const A: (&str, &str) = ("ada@own.example.test", "Ada Ł. Lovelace");
const B: (&str, &str) = ("bert@own.example.test", "Bert 🎺 Bach");
const C: (&str, &str) = ("cora@own.example.test", "Cora 'Cee' Nuñez");
const fn tone(hz: f32) -> Tone {
    Tone { hz, amplitude: 0.3 }
}
/// Made-up people with no client: invited, members of the team.
const QUIET: [&str; 6] = [
    "Zoë O'Brien-Ångström",
    "李 小龍",
    "Dr. 🦀 Ferris",
    "<b>Bold</b> & \"Quoted\"",
    "ابن سينا",
    "Name-That-Is-Very-Long-Indeed-For-A-Person-Who-Has-Many-Middle-Names",
];
const PLANNING: &str = "Q4 ✨ «planning» & co";
const LOBBY: &str = "Lobby 🚪 «check» with a long subject that the pre-join and the call window must cut";
const CHANNEL_TITLE: &str = "Sprint review ✨ «Q4» & demo";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("own meeting window test: {error}");
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

/// A call's media on the file devices: `tone` in, nothing recorded, the test pattern for a camera.
fn media(tone: Tone, output: Arc<dyn VideoOutput>) -> CallMedia {
    CallMedia {
        devices: Arc::new(FileDevices {
            input: AudioInput::Tone(tone),
            recording: None,
            camera: PatternCamera::default(),
            pacing: Pacing::RealTime,
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

/// A client in the parent: b's, c's, or a second one of a's.
struct Peer {
    name: &'static str,
    upn: &'static str,
    mri: String,
    engine: AccountEngine<MemorySecretStore>,
    /// Every event its engine told so far.
    log: Vec<Event>,
}

impl Peer {
    fn start(
        dir: &Path,
        server: (&str, &str),
        (upn, name): (&'static str, &'static str),
        hz: f32,
    ) -> SandboxResult<Self> {
        let (account, engine) = own_account(dir, server, upn, media(tone(hz), Arc::new(Counted::default())))?;
        let mri = account.mri().ok_or("no MRI")?.as_str().to_owned();
        Ok(Self {
            name,
            upn,
            mri,
            engine,
            log: Vec::new(),
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
            let found = pick(&event);
            self.log.push(event);
            if let Some(found) = found {
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

    /// Until `call`'s lobby shows `mri` waiting.
    /// The lobby's tellings come with the join itself: the ones already told count too.
    fn sees_waiting(&mut self, runtime: &tokio::runtime::Runtime, call: CallId, mri: &str) -> SandboxResult {
        let waits = |event: &Event| match event {
            Event::Lobby { call: of, waiting } if *of == call && waiting.iter().any(|one| one.mri.as_str() == mri) => {
                Some(())
            }
            _ => None,
        };
        if self.log.iter().any(|event| waits(event).is_some()) {
            return Ok(());
        }
        self.next(runtime, "someone in the lobby", waits)
    }

    fn admit(&self, runtime: &tokio::runtime::Runtime, call: CallId, mri: &str) -> SandboxResult {
        self.act(
            runtime,
            &CallAction::Lobby {
                call,
                waiting: Mri::new(mri),
                admit: true,
            },
        )?;
        Ok(())
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
    let Some(sandbox) = sandbox_or_skip("ngteams own meeting", &options)? else {
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
    let mut quiet = Vec::new();
    for (index, name) in QUIET.iter().enumerate() {
        let oid = server.add_user(&format!("quiet.{index}@own.example.test"), name)?;
        quiet.push((format!("8:orgid:{oid}"), (*name).to_owned()));
    }
    let ca = server.ca_file.to_string_lossy().into_owned();
    let at = (server.origin.as_str(), ca.as_str());
    let dir = tempfile::tempdir()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mut people = People {
        bert: Peer::start(&dir.path().join("b"), at, B, 660.0)?,
        cora: Peer::start(&dir.path().join("c"), at, C, 880.0)?,
        quiet,
        server: at,
        dir: dir.path().to_owned(),
    };
    let mut app = AppUnderTest::start(&sandbox, at, dir.path())?;
    let outcome = Rig::new(&sandbox, &mut app, &runtime).and_then(|mut rig| {
        let result = scenario(&mut rig, &mut people);
        rig.done();
        result
    });
    let log = server.log();
    for dir in evidence_dirs() {
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(dir.join("own-meeting-server.log"), &log);
    }
    let outcome = outcome.map_err(|error| {
        format!(
            "{error}\n--- the app: {}\n--- the sandbox{}",
            app.running.fail(&"killed"),
            sandbox.logs()
        )
    });
    for peer in [people.bert, people.cora] {
        peer.engine.handle.shutdown();
    }
    outcome?;
    app.quit(&sandbox)?;
    say("ngteams own meeting window: ok");
    Ok(())
}

/// b and c, the made-up people with no client, and where the server is.
struct People<'a> {
    bert: Peer,
    cora: Peer,
    quiet: Vec<(String, String)>,
    server: (&'a str, &'a str),
    dir: PathBuf,
}

fn scenario(rig: &mut Rig<'_>, people: &mut People<'_>) -> SandboxResult {
    let a_mri = rig.own_mri()?;
    let chat = scheduled_by_b(rig, people, &a_mri)?;
    meeting_ends(rig, people, &chat)?;
    organizer_admits(rig, people)?;
    channel_meet_now(rig, people, &a_mri)
}

/// b's meeting (only the organizer bypasses the lobby): a's pre-join, a in the lobby, b lets a in, c comes and b lets
/// it in; b and c raise their hands; b's Mute all. The meeting's chat, and b's and c's calls.
fn scheduled_by_b(rig: &mut Rig<'_>, people: &mut People<'_>, a_mri: &str) -> SandboxResult<String> {
    let runtime = rig.runtime;
    let mut invited = vec![
        (a_mri.to_owned(), A.1.to_owned()),
        (people.cora.mri.clone(), C.1.to_owned()),
    ];
    invited.extend(people.quiet.iter().cloned());
    let draft = meeting(PLANNING, &people.bert.mri, &invited);
    let written = runtime.block_on(people.bert.engine.handle.write_event(EventWrite::Create(draft)))?;
    let event = written.created.ok_or("b's meeting wasn't made")?;
    let chat = event.meeting_chat.clone().ok_or("no meeting chat")?;
    people.bert.bypass(runtime, people.server, &event, "OrganizerOnly")?;
    runtime.block_on(people.cora.engine.handle.sync())?;
    // a opens the meeting's pre-join from its calendar
    let id = rig.event_id(PLANNING)?;
    rig.ctl.request(None, Request::JoinMeeting { event: id, now: false })?;
    rig.app.trace.wait_for("prejoin", "shown")?;
    thread::sleep(SETTLE);
    rig.find_window()?;
    rig.shots("prejoin")?;
    // Join now (Enter): nobody admits a yet, so a waits in the lobby
    rig.focus()?;
    rig.input.key(keys::ENTER)?;
    rig.wait_call("a in the lobby", |row| row.state == "lobby")?;
    thread::sleep(SETTLE);
    rig.find_window()?;
    rig.shots("lobby-guest")?;
    // b comes from the chat and lets a in; c comes from its card, waits, and b lets it in
    let join = people.bert.join_from_chat(runtime, &chat)?;
    let b_call = people.bert.act(runtime, &join)?;
    people.bert.status(runtime, b_call, &CallStatus::Connected)?;
    people.bert.sees_waiting(runtime, b_call, a_mri)?;
    people.bert.admit(runtime, b_call, a_mri)?;
    rig.wait_call("a in the meeting", |row| row.state == "connected")?;
    let join = people.cora.join_from_chat(runtime, &chat)?;
    let c_call = people.cora.act(runtime, &join)?;
    people.cora.status(runtime, c_call, &CallStatus::Lobby)?;
    people.bert.sees_waiting(runtime, b_call, &people.cora.mri)?;
    people.bert.admit(runtime, b_call, &people.cora.mri)?;
    people.cora.status(runtime, c_call, &CallStatus::Connected)?;
    for (peer, call) in [(&people.bert, b_call), (&people.cora, c_call)] {
        peer.act(runtime, &CallAction::RaiseHand { call, raised: true })?;
    }
    thread::sleep(SETTLE * 2);
    rig.find_window()?;
    rig.shots("meeting-hands")?;
    rig.calls_of_b_and_c = Some((b_call, c_call));
    Ok(chat)
}

/// b's Mute all: a sees "You've been muted"; b's End meeting: a's call ends; a's meeting chat shows its start and end.
fn meeting_ends(rig: &mut Rig<'_>, people: &mut People<'_>, chat: &str) -> SandboxResult {
    let runtime = rig.runtime;
    let (b_call, c_call) = rig.calls_of_b_and_c.take().ok_or("no meeting")?;
    people.bert.act(runtime, &CallAction::MuteAll { call: b_call })?;
    rig.wait_call("a muted by the organizer", |row| row.muted)?;
    thread::sleep(SETTLE);
    rig.theme(teams_ui::theme::LIGHT_KEY)?;
    rig.resize(WIDTHS[0])?;
    rig.save("muted-notice-first-light-1280")?;
    rig.shots("muted-notice")?;
    people.bert.act(runtime, &CallAction::EndMeeting { call: b_call })?;
    let end = people.cora.next(runtime, "c's end", |event| match event {
        Event::Call {
            call,
            status: CallStatus::Ended(end),
        } if *call == c_call => Some(end.clone()),
        _ => None,
    })?;
    say(&format!("c: the meeting ended: {end:?}"));
    rig.wait_no_call("a's call over")?;
    rig.ctl.request(
        None,
        Request::Open {
            target: OpenTarget::Chat { chat: chat.to_owned() },
        },
    )?;
    thread::sleep(SETTLE * 3);
    rig.main_window();
    // what a's app holds of the meeting chat, as its control socket reads it (the newest first)
    let held = rig.ctl.request(
        None,
        Request::Messages {
            chat: chat.to_owned(),
            limit: Some(20),
            before: None,
        },
    )?;
    if let Response::Messages { messages, .. } = held {
        let texts: Vec<&str> = messages.iter().map(|row| row.text.as_str()).collect();
        say(&format!("a's meeting chat holds, the newest first: {texts:?}"));
    }
    rig.shots("meeting-chat-ended")
}

/// a's own meeting (scheduled on a second client of a's), only invited people bypass the lobby: a joins; c (not
/// invited) joins by ID and waits; a's call window shows the lobby's card; a clicks Admit and c comes in.
fn organizer_admits(rig: &mut Rig<'_>, people: &mut People<'_>) -> SandboxResult {
    let runtime = rig.runtime;
    let second = Peer::start(&people.dir.join("a2"), people.server, A, 440.0)?;
    let invited = [(people.bert.mri.clone(), B.1.to_owned())];
    let draft = meeting(LOBBY, &second.mri, &invited);
    let written = runtime.block_on(second.engine.handle.write_event(EventWrite::Create(draft)))?;
    let event = written.created.ok_or("a's meeting wasn't made")?;
    second.bypass(runtime, people.server, &event, "InvitedUsers")?;
    second.engine.handle.shutdown();
    let id = rig.event_id(LOBBY)?;
    rig.ctl.request(None, Request::JoinMeeting { event: id, now: true })?;
    rig.wait_call("a in its meeting", |row| row.state == "connected")?;
    let code = event.meeting_code.clone().ok_or("no meeting ID")?;
    let c_call = people.cora.act(runtime, &CallAction::JoinMeetingCode(code))?;
    people.cora.status(runtime, c_call, &CallStatus::Lobby)?;
    thread::sleep(SETTLE * 2);
    rig.find_window()?;
    rig.shots("lobby-organizer")?;
    rig.theme(teams_ui::theme::LIGHT_KEY)?;
    rig.resize(WIDTHS[0])?;
    rig.click_admit()?;
    people.cora.status(runtime, c_call, &CallStatus::Connected)?;
    say("a's Admit let c in");
    thread::sleep(SETTLE);
    rig.save("lobby-organizer-admitted-light-1280")?;
    rig.ctl.request(None, Request::HangUp)?;
    people.cora.act(runtime, &CallAction::HangUp { call: c_call })?;
    rig.wait_no_call("a's meeting left")?;
    Ok(())
}

/// b's team with a, c and the made-up people on it, and a channel of it: the channel's thread.
fn team_channel(rig: &Rig<'_>, people: &People<'_>, a_mri: &str) -> SandboxResult<String> {
    let runtime = rig.runtime;
    let handle = &people.bert.engine.handle;
    let create = TeamWrite::Create(NewTeam {
        name: "Rocket 🚀 «R&D»".into(),
        description: "Rocket's people & friends".into(),
        first_channel: "General".into(),
        public: false,
    });
    let (team, group) = runtime
        .block_on(handle.team_write(create, None))?
        .created
        .ok_or("no team")?;
    let group = group.ok_or("no team group")?;
    let mut members = vec![
        (Mri::new(a_mri), TeamRole::Member),
        (Mri::new(people.cora.mri.as_str()), TeamRole::Member),
    ];
    members.extend(
        people
            .quiet
            .iter()
            .map(|(mri, _)| (Mri::new(mri.as_str()), TeamRole::Member)),
    );
    let add = TeamWrite::AddMembers {
        team: team.clone(),
        group: group.clone(),
        members,
    };
    runtime.block_on(handle.team_write(add, None))?;
    let made = runtime.block_on(handle.create_channel(teams_core::ChannelDraft {
        channel: teams_proto::NewChannel {
            team,
            group,
            mailbox: None,
            name: "Launch ✨ v2".into(),
            description: "About the launch".into(),
            kind: teams_proto::NewChannelKind::Standard,
            layout: teams_proto::ChannelLayout::Threads,
        },
        show_by_default: true,
    }))?;
    Ok(made.thread.ok_or("no channel")?)
}

/// b's Meet now in the channel, titled from its pre-join: a's channel shows the post; a joins from the channel; all
/// leave; the post shows the end.
fn channel_meet_now(rig: &mut Rig<'_>, people: &mut People<'_>, a_mri: &str) -> SandboxResult {
    let runtime = rig.runtime;
    let channel = team_channel(rig, people, a_mri)?;
    runtime.block_on(people.cora.engine.handle.sync())?;
    rig.ctl.request(None, Request::Sync)?;
    let meet_now = CallAction::MeetNow {
        channel: channel.clone(),
    };
    let b_call = people.bert.act(runtime, &meet_now)?;
    people.bert.status(runtime, b_call, &CallStatus::Connected)?;
    runtime.block_on(
        people
            .bert
            .engine
            .handle
            .requests()
            .title_meet_now(channel.clone(), CHANNEL_TITLE),
    )?;
    rig.open_channel(&channel)?;
    thread::sleep(SETTLE * 3);
    rig.main_window();
    rig.shots("channel-post")?;
    // the post's Join opens the pre-join of the post's meeting; Escape closes it
    rig.resize(WIDTHS[0])?;
    rig.click_post_join()?;
    thread::sleep(SETTLE * 2);
    rig.find_window()?;
    rig.shots("channel-prejoin-from-post")?;
    rig.focus()?;
    rig.input.key(keys::ESC)?;
    thread::sleep(SETTLE * 2);
    rig.main_window();
    // a joins from the channel: its header's Join while the meeting runs
    rig.resize(WIDTHS[0])?;
    rig.click_header_join()?;
    thread::sleep(SETTLE * 2);
    rig.find_window()?;
    rig.shots("channel-prejoin")?;
    rig.focus()?;
    rig.input.key(keys::ENTER)?;
    rig.wait_call("a in the channel's meeting", |row| row.state == "connected")?;
    thread::sleep(SETTLE * 2);
    rig.find_window()?;
    rig.resize(WIDTHS[0])?;
    rig.save("channel-meeting-light-1280")?;
    rig.ctl.request(None, Request::HangUp)?;
    rig.wait_no_call("a left the channel's meeting")?;
    rig.main_window();
    people.bert.act(runtime, &CallAction::HangUp { call: b_call })?;
    thread::sleep(SETTLE * 3);
    rig.open_channel(&channel)?;
    thread::sleep(SETTLE * 2);
    rig.main_window();
    rig.shots("channel-post-ended")?;
    let long = long_title();
    say(&format!("the long title ({} characters): {long}", long.chars().count()));
    for (name, title) in [("channel-untitled", None), ("channel-long", Some(long.as_str()))] {
        more_meet_now(rig, people, &channel, name, title)?;
    }
    Ok(())
}

/// A title of 200 characters: emoji, quotes, «», `&` and markup.
fn long_title() -> String {
    let mut title = "Sprint ✨ \"review\" «Q4» & <b>demo</b> — 🚀 'launch' ".to_owned();
    let mut n = 0;
    while title.chars().count() < 200 {
        n += 1;
        let _ = write!(title, "part{n} ");
    }
    title.chars().take(199).chain(std::iter::once('!')).collect()
}

/// b's Meet now in the channel, titled `title` or untitled: a's post (shots), a's pre-join from the header's Join and
/// its call (shots `<name>-…`); all leave.
fn more_meet_now(
    rig: &mut Rig<'_>,
    people: &mut People<'_>,
    channel: &str,
    name: &str,
    title: Option<&str>,
) -> SandboxResult {
    let runtime = rig.runtime;
    let meet_now = CallAction::MeetNow {
        channel: channel.to_owned(),
    };
    let b_call = people.bert.act(runtime, &meet_now)?;
    people.bert.status(runtime, b_call, &CallStatus::Connected)?;
    if let Some(title) = title {
        runtime.block_on(
            people
                .bert
                .engine
                .handle
                .requests()
                .title_meet_now(channel.to_owned(), title),
        )?;
    }
    rig.open_channel(channel)?;
    thread::sleep(SETTLE * 3);
    rig.main_window();
    rig.shots(&format!("{name}-post"))?;
    rig.resize(WIDTHS[0])?;
    rig.click_header_join()?;
    thread::sleep(SETTLE * 2);
    rig.find_window()?;
    rig.resize(WIDTHS[0])?;
    rig.save(&format!("{name}-prejoin-light-1280"))?;
    rig.focus()?;
    rig.input.key(keys::ENTER)?;
    rig.wait_call("a in the channel's meeting", |row| row.state == "connected")?;
    thread::sleep(SETTLE * 2);
    rig.find_window()?;
    rig.resize(WIDTHS[0])?;
    rig.save(&format!("{name}-meeting-light-1280"))?;
    rig.ctl.request(None, Request::HangUp)?;
    rig.wait_no_call("a left the channel's meeting")?;
    rig.main_window();
    people.bert.act(runtime, &CallAction::HangUp { call: b_call })?;
    thread::sleep(SETTLE * 3);
    rig.open_channel(channel)?;
    thread::sleep(SETTLE * 2);
    rig.main_window();
    rig.resize(WIDTHS[0])?;
    rig.save(&format!("{name}-post-ended-light-1280"))
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
    input: VirtualInput,
    /// niri's id of the window the shots are of.
    window_id: Option<u64>,
    /// niri's id of the main window.
    main_id: Option<u64>,
    calls_of_b_and_c: Option<(CallId, CallId)>,
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
        let mut rig = Self {
            sandbox,
            app,
            runtime,
            ctl,
            input: sandbox.input()?,
            window_id: None,
            main_id: None,
            calls_of_b_and_c: None,
            shots: 0,
        };
        rig.main_id = rig.windows()?.iter().filter_map(|window| window["id"].as_u64()).min();
        Ok(rig)
    }

    fn done(&mut self) {
        say(&format!("{} shots taken", self.shots));
    }

    /// a's MRI, as the app's status tells it.
    fn own_mri(&mut self) -> SandboxResult<String> {
        match self.ctl.request(None, Request::Accounts)? {
            Response::Accounts { accounts } => {
                let first = accounts.first().ok_or("no account")?;
                Ok(first.mri.clone())
            }
            other => Err(format!("accounts: {other:?}").into()),
        }
    }

    /// Opens the channel `channel` in a's main window, once a's app knows it (a new team reaches it after a sync).
    fn open_channel(&mut self, channel: &str) -> SandboxResult {
        let deadline = Instant::now() + WAIT;
        loop {
            let open = Request::Open {
                target: OpenTarget::Channel {
                    channel: channel.to_owned(),
                },
            };
            match self.ctl.request(None, open) {
                Ok(_) => return Ok(()),
                Err(error) if Instant::now() < deadline => {
                    say(&format!("a's app cannot open the channel yet: {error}"));
                    self.ctl.request(None, Request::Sync)?;
                    thread::sleep(Duration::from_secs(1));
                }
                Err(error) => return Err(error.into()),
            }
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

    fn windows(&self) -> SandboxResult<Vec<serde_json::Value>> {
        let text = self.sandbox.niri_msg(&["-j", "windows"])?;
        let windows: serde_json::Value = serde_json::from_str(&text)?;
        Ok(windows.as_array().cloned().unwrap_or_default())
    }

    /// The newest window that isn't the main one (the pre-join's or the call's): the shots are of it.
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
        say(&format!("the window {newest}: {title:?}"));
        self.window_id = Some(newest);
        Ok(())
    }

    /// The shots are of the main window.
    fn main_window(&mut self) {
        self.window_id = self.main_id;
    }

    fn focus(&mut self) -> SandboxResult {
        if let Some(id) = self.window_id {
            self.sandbox
                .niri_msg(&["action", "focus-window", "--id", &id.to_string()])?;
        }
        thread::sleep(Duration::from_millis(300));
        Ok(())
    }

    /// The window's size, as niri gives it.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "niri's logical px are whole numbers on a scale of 1"
    )]
    fn size(&mut self) -> SandboxResult<(i32, i32)> {
        let windows = self.windows()?;
        let window = windows
            .iter()
            .find(|window| window["id"].as_u64() == self.window_id)
            .ok_or("the window is gone")?;
        let size = &window["layout"]["window_size"];
        let at = |index: usize| size[index].as_f64().map(|value| value.round() as i32);
        Ok((at(0).ok_or("no width")?, at(1).ok_or("no height")?))
    }

    /// The compositor sizes the window `width` wide, as a user's drag does; the size it took.
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
        Ok(size)
    }

    fn theme(&mut self, key: &str) -> SandboxResult {
        fs::write(self.sandbox.path(THEME_TRIGGER), key)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The four shots of the window: Light and Dark, each 1280 and 480 px wide.
    fn shots(&mut self, what: &str) -> SandboxResult {
        for (theme, name) in [
            (teams_ui::theme::LIGHT_KEY, "light"),
            (teams_ui::theme::DARK_KEY, "dark"),
        ] {
            self.theme(theme)?;
            for width in WIDTHS {
                let (got, _) = self.resize(width)?;
                let narrow = if got == width {
                    String::new()
                } else {
                    format!("-is-{got}")
                };
                self.save(&format!("{what}-{name}-{width}{narrow}"))?;
            }
        }
        self.theme(teams_ui::theme::LIGHT_KEY)
    }

    /// Saves the window alone as `own-meeting-<name>.png` under `target/ui-shots/` and the evidence dir.
    fn save(&mut self, name: &str) -> SandboxResult {
        self.focus()?;
        self.shots += 1;
        let png = self.sandbox.screenshot(&format!("shot-{}.png", self.shots), true)?;
        for dir in evidence_dirs() {
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(format!("own-meeting-{name}.png")))?;
        }
        let _ = fs::remove_file(png);
        say(&format!("shot own-meeting-{name}.png"));
        Ok(())
    }

    /// Clicks the lobby card's Admit, found as a user's eye finds it: the brand-filled button at the card, in the top
    /// of the call window (Teams Light).
    fn click_admit(&mut self) -> SandboxResult {
        self.click_brand("Admit", admit_button)
    }

    /// Clicks the channel header's Join (Teams Light, the main window 1280 px wide).
    fn click_header_join(&mut self) -> SandboxResult {
        self.click_brand("Join", header_join)
    }

    /// Clicks the channel post's Join (Teams Light, the main window 1280 px wide).
    fn click_post_join(&mut self) -> SandboxResult {
        self.click_brand("the post's Join", post_join)
    }

    /// Clicks the brand-filled button `find` finds in the window's shot, where the window is on the screen.
    fn click_brand(&mut self, what: &str, find: fn(&RgbImage) -> Option<(i32, i32)>) -> SandboxResult {
        self.focus()?;
        self.shots += 1;
        let png = self.sandbox.screenshot(&format!("alone-{}.png", self.shots), true)?;
        let alone = image::open(&png)?.to_rgb8();
        let _ = fs::remove_file(png);
        let png = self.sandbox.screenshot(&format!("screen-{}.png", self.shots), false)?;
        let screen = image::open(&png)?.to_rgb8();
        let _ = fs::remove_file(png);
        let frame = pixels::find(&screen, &alone).ok_or("the call window isn't on the screen")?;
        let (x, y) = find(&alone).ok_or_else(|| format!("no {what} button in the window"))?;
        let (x, y) = (frame.x + x, frame.y + y);
        say(&format!("{what} at {x},{y} (the window at {},{})", frame.x, frame.y));
        let (x, y) = (u32::try_from(x)?, u32::try_from(y)?);
        self.input.move_to(x, y)?;
        thread::sleep(Duration::from_millis(80));
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(SETTLE);
        Ok(())
    }
}

/// The middle of the brand-filled button nearest the top of `window`'s stage, within the lobby card's width around its
/// middle (below the title bar and its toolbar, drawn in the brand's gradient).
fn admit_button(window: &RgbImage) -> Option<(i32, i32)> {
    let middle = window.width() / 2;
    let columns = (middle.saturating_sub(170), (middle + 170).min(window.width()));
    brand_button(window, columns, (60, 400), 50)
}

/// The middle of the channel header's Join: the brand-filled button at the right of the header, under the title bar.
fn header_join(window: &RgbImage) -> Option<(i32, i32)> {
    brand_button(
        window,
        (window.width().saturating_sub(200), window.width()),
        (52, 100),
        30,
    )
}

/// The middle of the channel post's Join: a brand-filled button wider than the card's tile, in the posts (below the
/// header, above the message box).
fn post_join(window: &RgbImage) -> Option<(i32, i32)> {
    brand_button(
        window,
        (380, window.width()),
        (110, window.height().saturating_sub(70)),
        44,
    )
}

/// The middle of the first brand-filled button within `columns` and `rows` of `window`: rows whose rightmost run of
/// brand pixels (gaps of 12 px or less, for a button's icon and label) is at least `wide` px wide and at least half
/// brand, at least 16 such rows together. A smaller brand tile beside the button on the same rows is not part of the run.
fn brand_button(
    window: &RgbImage,
    (left, right): (u32, u32),
    (top, bottom): (u32, u32),
    wide: usize,
) -> Option<(i32, i32)> {
    let brand = |pixel: &image::Rgb<u8>| {
        let [r, g, b] = pixel.0.map(i32::from);
        b >= 150 && b - r >= 60 && b - g >= 60
    };
    let mut run: Vec<(u32, u32, u32)> = Vec::new();
    for y in top..window.height().min(bottom) {
        let mut segments: Vec<(u32, u32, usize)> = Vec::new();
        for x in (left..right.min(window.width())).filter(|x| brand(window.get_pixel(*x, y))) {
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
    let (top, first, last) = run[0];
    let bottom = run[run.len() - 1].0;
    Some((
        i32::try_from(first.midpoint(last)).ok()?,
        i32::try_from(top.midpoint(bottom)).ok()?,
    ))
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

    fn wait_for(&mut self, view: &str, change: &str) -> SandboxResult {
        let deadline = Instant::now() + WAIT;
        let found =
            |seen: &[serde_json::Value]| seen.iter().any(|line| line["view"] == view && line["change"] == change);
        while !found(self.catch_up()) {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self.lines.recv_timeout(left).map_err(|_| {
                let tail: Vec<String> = self.seen.iter().rev().take(40).map(ToString::to_string).collect();
                format!("no trace line for {view} {change}; the last lines, newest first: {tail:#?}")
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
    let (origin, ca_file) = (env(ORIGIN_ENV)?, env(CA_ENV)?);
    let feed = VideoFeed::new();
    let (account, engine) = own_account(
        &dir.join("account"),
        (&origin, &ca_file),
        A.0,
        media(tone(440.0), Arc::new(feed.clone())),
    )?;
    let first = ngteams::accounts::Started {
        account,
        engine,
        calls: Some(PersonalCalls {
            ringer: None,
            sounds: None,
            echo: Mri::new(teams_mock::ECHO),
            video: Some(feed),
            camera: Some(Arc::new(FileDevices {
                input: AudioInput::Silence,
                recording: None,
                camera: PatternCamera::default(),
                pacing: Pacing::RealTime,
            })),
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
