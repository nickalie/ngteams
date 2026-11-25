//! The control socket's call commands (`ctl::calls`), through the real protocol and server, over lab account a's
//! engine with its calling on the mock (`teams-mock`'s calling service, Trouter and media: a loopback TURN server and a
//! peer behind each party the mock plays), its calls' media on file devices (no PipeWire, no portal). The mock rings a
//! as b (it plays b, no client of b's runs); a answers, declines, calls the a–b chat and hangs up only by a command,
//! each asked through the calls' model as the call buttons ask, so the model's refusals (busy, no call, not ringing,
//! no video) come back as the command's error; one mute goes to a's engine directly, as an organizer's Mute all comes,
//! to overtake an unmute. The view trace counts the asks: a switch already as asked asks nothing. A meeting of a's calendar (the mock's middle tier) is joined only where its media setup fails, so nothing
//! of it goes to the calling service. The app without calling answers `no_calling`. Every id is made up or the mock's
//! own.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared harness holds more than this test uses")]
mod ctl_support;

use std::future::{self, Future};
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use ctl_support::{ask, connect, pumped, quit};
use gpui::{Entity, TestAppContext};
use ngteams::accounts::{Runner, Started};
use ngteams::accounts_file::{PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::calling::PersonalCalls;
use ngteams::setup::Setup;
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::{AccessToken, Renewal, Session};
use teams_auth::token::{EntraConfig, TokenClient, TokenError};
use teams_core::account::AccountEngine;
use teams_core::engine::{self, EngineOptions, Parts, Push};
use teams_core::{CallAction, CallFloor, CallMedia, CallParts, CallStatus, Identity, Outbox, Relay};
use teams_ctl::ClientError;
use teams_ctl::protocol::{CalendarRow, CallRow, CtlError, ErrorCode, Request, Response, Toggle, local_today};
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::devices::{AudioSink, AudioSource, DeviceError, Devices, VideoSource};
use teams_media::processing::ProcessingConfig;
use teams_media::relay::RelayServer;
use teams_media::session::SessionConfig;
use teams_media::tone::Tone;
use teams_mock::{AB_CHAT, CHAT_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, MIDDLE_TIER_BASE, TENANT, TrouterMock};
use teams_proto::{Author, Client, ClientInfo, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use teams_ui::bus::BusError;
use teams_ui::{Calls, Desktop, SettingsFile, VideoFeed, ViewTrace};
use tokio::sync::broadcast;

const UPN: &str = "bot-a@lab.example.test";
const WAIT: Duration = Duration::from_secs(30);
/// The tone the mock's peers play.
const PEER_TONE: Tone = Tone {
    hz: 880.0,
    amplitude: 0.5,
};
/// How long a's camera takes to open: a command asked meanwhile finds the camera's change still on its way.
const CAMERA_OPENS: Duration = Duration::from_millis(400);
/// How long a's microphone takes to open once it was missing: a mute asked meanwhile overtakes the unmute reopening it.
const MICROPHONE_OPENS: Duration = Duration::from_millis(400);

/// File devices whose camera opens [`CAMERA_OPENS`] late, as a real one takes a moment; with `microphone_missing`,
/// their microphone can't open at first, and opens [`MICROPHONE_OPENS`] late afterwards (plugged in meanwhile).
struct LabDevices {
    files: FileDevices,
    microphone_missing: Option<AtomicBool>,
}

impl Devices for LabDevices {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        if let Some(missing) = &self.microphone_missing {
            if missing.swap(false, Ordering::SeqCst) {
                return Err(DeviceError::Refused("no microphone is plugged in".into()));
            }
            std::thread::sleep(MICROPHONE_OPENS);
        }
        self.files.microphone()
    }

    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        self.files.speaker()
    }

    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        std::thread::sleep(CAMERA_OPENS);
        self.files.camera()
    }
}

/// a's tokens, as the mock takes them.
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

/// The view trace's lines, kept for the test to read.
#[derive(Clone, Default)]
struct Traced(Arc<Mutex<Vec<u8>>>);

impl Write for Traced {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Traced {
    /// How many times the calls were asked `kind` (`mute`, `camera`, …).
    fn asked(&self, kind: &str) -> usize {
        let text = String::from_utf8(self.0.lock().unwrap().clone()).unwrap();
        text.lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|line| line["view"] == "call" && line["change"] == "asked" && line["kinds"][0] == kind)
            .count()
    }
}

/// The mock with its calling on, and the app of lab account a on it serving its control socket.
struct Lab {
    mock: ChatServiceMock,
    desktop: Entity<Desktop>,
    socket: PathBuf,
    traced: Traced,
    cx: TestAppContext,
    _trouter: TrouterMock,
    _dir: tempfile::TempDir,
    runtime: tokio::runtime::Runtime,
}

impl Lab {
    /// a's app on the mock, its calling endpoint registered; its calls carry video when `video`.
    fn start(video: bool) -> Self {
        Self::with_microphone(video, false)
    }

    /// [`Self::start`], a's microphone missing at first when `microphone_missing` ([`LabDevices`]).
    fn with_microphone(video: bool, microphone_missing: bool) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let mock = runtime.block_on(ChatServiceMock::start()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let feed = video.then(VideoFeed::new);
        let (trouter, handle) = {
            let _entered = runtime.enter();
            let trouter = mock.start_trouter().unwrap();
            let relay = mock.start_media(dir.path(), PEER_TONE).unwrap();
            let handle = a_engine(&mock, &trouter, relay, feed.clone(), microphone_missing, dir.path());
            (trouter, handle)
        };
        // a–b stored, as a catch-up stores it: a call names its chat
        handle.requests().sync().blocking_wait().unwrap();
        let socket = dir.path().join("run/ngteams/ctl.sock");
        let traced = Traced::default();
        let cx = TestAppContext::single();
        let calls = PersonalCalls {
            ringer: None,
            sounds: None,
            echo: Mri::new(ECHO),
            video: feed,
            camera: None,
            devices: None,
            noise: teams_media::processing::NoiseControl::default(),
        };
        let launch = Launch {
            first: Started {
                calls: Some(calls),
                ..started(handle)
            },
            others: Vec::new(),
            settings: app::settings(SettingsFile::new(dir.path().join("settings.toml")), None),
            bus: Err(BusError::NotGiven),
            sign_in_again: Rc::new(|_| Rc::new(|_| {})),
            runner: Runner {
                setup: Setup::new(PersonalPaths::new(dir.path().join("config"), dir.path().join("data"))).unwrap(),
                floor: CallFloor::new(),
                devices: None,
            },
            read_only: false,
            trace: Arc::new(ViewTrace::new(traced.clone())),
            ctl: Some(socket.clone()),
        };
        let Ok(launched) = cx.update(|cx| app::launch(cx, launch)) else {
            panic!("the window opens");
        };
        let lab = Self {
            mock,
            desktop: launched.desktop,
            socket,
            traced,
            cx,
            _trouter: trouter,
            _dir: dir,
            runtime,
        };
        lab.settle(Calls::registered);
        lab
    }

    fn ask(&self, request: Request) -> Result<Response, ClientError> {
        ask(&self.cx, &self.socket, None, request)
    }

    /// The call row `request` answers with.
    fn row(&self, request: Request) -> CallRow {
        let asked = format!("{request:?}");
        match self.ask(request) {
            Ok(Response::Call(row)) => row,
            other => panic!("{asked}: {other:?}"),
        }
    }

    /// The error `request` answers with.
    fn refused(&self, request: Request) -> CtlError {
        let asked = format!("{request:?}");
        match self.ask(request) {
            Err(ClientError::Server(error)) => error,
            other => panic!("{asked}: {other:?}"),
        }
    }

    fn listed(&self) -> Vec<CallRow> {
        match self.ask(Request::Calls) {
            Ok(Response::Calls { calls }) => calls,
            other => panic!("calls: {other:?}"),
        }
    }

    fn calls(&self) -> Entity<Calls> {
        self.cx.read(|cx| app::bridges(cx)[0].read(cx).models().calls.clone())
    }

    /// GPUI stepped (its clock left as it is: no call is given up on) until a's calls show `done`.
    fn settle(&self, mut done: impl FnMut(&Calls) -> bool) {
        let calls = self.calls();
        let start = Instant::now();
        loop {
            self.cx.run_until_parked();
            if self.cx.read(|cx| done(calls.read(cx))) {
                return;
            }
            assert!(start.elapsed() < WAIT, "a's calls didn't settle in time");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The operations a sent on the mock's newest call, in order.
    fn sent(&self) -> Vec<String> {
        let calls = self.mock.calls();
        calls.last().map_or_else(Vec::new, |call| {
            call.requests
                .iter()
                .filter(|(_, account)| account == LAB_A)
                .map(|(op, _)| op.clone())
                .collect()
        })
    }

    /// Waits until a sent `op` on the mock's newest call (the call is told as its setup begins, before it goes out).
    fn sent_at_last(&self, op: &str) {
        let start = Instant::now();
        while !self.sent().iter().any(|sent| sent == op) {
            assert!(start.elapsed() < WAIT, "{op} not sent: {:?}", self.sent());
            self.cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The mock rings a as b: the call's id on the mock, once it rings in a's calls.
    fn ring(&self) -> String {
        let id = self.mock.ring(LAB_B, LAB_A);
        self.settle(|calls| calls.ringing().is_some());
        id
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        quit(&self.cx);
    }
}

/// a's engine on the mock with its push channel and its calling, its calls' media on file devices relayed through the
/// mock's TURN server at `relay`, their video on `feed` when given, their microphone missing at first when
/// `microphone_missing`.
fn a_engine(
    mock: &ChatServiceMock,
    trouter: &TrouterMock,
    relay: std::net::SocketAddr,
    feed: Option<VideoFeed>,
    microphone_missing: bool,
    dir: &std::path::Path,
) -> engine::EngineHandle {
    let tracker = Arc::new(teams_call::Tracker::new(Mri::new(LAB_A)));
    let tokens = Arc::new(Tokens {
        ic3: mock.token(LAB_A).unwrap(),
        skypetoken: mock.skypetoken(LAB_A).unwrap(),
        renewals: broadcast::channel(4).0,
    });
    let origin = mock.origin();
    // the calendar read from the mock's middle tier: a meeting's join from its event
    let http = || {
        HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin)
            .unwrap()
            .with_middle_tier(MIDDLE_TIER_BASE)
            .unwrap()
    };
    let transport = Arc::new(http());
    let options = Options {
        ping_interval: Duration::from_millis(300),
        pong_timeout: Duration::from_secs(5),
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(200),
        backoff_jitter_percent: 0,
        ..Options::default()
    };
    let trouter = || {
        Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), trouter.origin())
            .unwrap()
            .with_options(options)
    };
    let calling = trouter().for_calls(CallStack::new(Arc::clone(&tracker)));
    let push = Push::new(trouter());
    let media = CallMedia {
        devices: Arc::new(LabDevices {
            files: FileDevices {
                input: AudioInput::Silence,
                recording: None,
                camera: PatternCamera::default(),
                pacing: Pacing::RealTime,
            },
            microphone_missing: microphone_missing.then(|| AtomicBool::new(true)),
        }),
        relay: Relay::Fixed(vec![RelayServer::udp(relay)]),
        session: SessionConfig {
            processing: ProcessingConfig::OFF,
            ..Default::default()
        },
        video: feed.map(|feed| teams_core::account::call_video(Arc::new(feed))),
    };
    let identity = Identity {
        mri: Mri::new(LAB_A),
        display_name: Some("bot-a".into()),
        endpoint_id: String::new(),
        language_id: Some(teams_core::account::CALL_LANGUAGE.into()),
        tenant_id: TENANT.into(),
        client: ClientInfo {
            endpoint_capabilities: Some(teams_core::account::WEB_ENDPOINT_CAPABILITIES),
            client_endpoint_capabilities: Some(teams_core::account::WEB_CLIENT_ENDPOINT_CAPABILITIES),
            platform_name: Some("SyntheticNative/1".into()),
        },
        region: Some("amer".into()),
        partition: Some("amer01".into()),
    };
    engine::start(Parts {
        calls: Some(CallParts::new(calling, identity, media)),
        store: Store::open(&dir.join("a.sqlite")).unwrap(),
        client: Arc::new(Client::new(http())),
        outbox: Outbox::new(transport, Author::new(Mri::new(LAB_A), "bot-a"))
            .with_calls(Arc::clone(&tracker), CallFloor::new()),
        push: Some(push),
        options: EngineOptions {
            tick: Duration::from_millis(50),
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
    })
    .unwrap()
}

/// a's engine as the app starts an account: its session only for the sign-out watch, reaching nothing (an empty
/// in-memory store, a loopback port nothing listens on).
fn started(handle: engine::EngineHandle) -> Started<MemorySecretStore> {
    let mut account = PersonalAccount::new(UPN, LAB_A.strip_prefix("8:orgid:").unwrap(), TENANT).unwrap();
    account.name = Some("bot-a".to_owned());
    let session = Session::new(
        UPN,
        MemorySecretStore::new(),
        TokenClient::new(EntraConfig::teams_native().with_authority("http://127.0.0.1:9/")).unwrap(),
        AuthzClient::with_endpoint("http://127.0.0.1:9/authz").unwrap(),
        Region::default(),
    );
    Started {
        account,
        engine: AccountEngine {
            handle,
            session: Arc::new(session),
        },
        calls: None,
    }
}

fn mute(op: Toggle) -> Request {
    Request::Mute { op }
}

fn camera(op: Toggle) -> Request {
    Request::Camera { op }
}

fn connected(calls: &Calls) -> bool {
    calls.call().is_some_and(|call| call.status == CallStatus::Connected)
}

#[test]
fn nothing_up_or_ringing_refuses_each_command_as_its_button_would() {
    let lab = Lab::start(false);
    assert!(lab.listed().is_empty());
    for (request, code) in [
        (Request::Answer, ErrorCode::NotRinging),
        (Request::Decline, ErrorCode::NotRinging),
        (Request::HangUp, ErrorCode::NoCall),
        (mute(Toggle::On), ErrorCode::NoCall),
        (mute(Toggle::Toggle), ErrorCode::NoCall),
        // this app's calls carry no video: the camera's button isn't there, call or not
        (camera(Toggle::Off), ErrorCode::NoVideo),
        // no running call is known in a–b
        (
            Request::Join {
                chat: AB_CHAT.to_owned(),
            },
            ErrorCode::NoCall,
        ),
        (
            Request::Call {
                chat: "no such chat".to_owned(),
            },
            ErrorCode::UnknownChat,
        ),
    ] {
        let asked = format!("{request:?}");
        assert_eq!(lab.refused(request).code, code, "{asked}");
    }
    assert!(lab.mock.calls().is_empty(), "nothing went to the calling service");
}

#[test]
fn an_invitation_is_answered_muted_and_its_camera_turned_on_and_off_then_hung_up() {
    let lab = Lab::start(true);
    let id = lab.ring();
    let rows = lab.listed();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let ringing = &rows[0];
    assert!(ringing.ringing && ringing.state == "ringing", "{ringing:?}");
    assert_eq!(
        (ringing.account.as_str(), ringing.caller.as_deref()),
        (UPN, Some(LAB_B))
    );
    // nothing answers it but the command
    assert!(!lab.sent().contains(&"accept".to_owned()), "{:?}", lab.sent());

    let answered = lab.row(Request::Answer);
    assert_eq!(answered.id, ringing.id, "the invitation's call");
    assert!(!answered.ringing && answered.state != "ended", "{answered:?}");
    assert!(lab.sent().contains(&"accept".to_owned()), "{:?}", lab.sent());
    lab.settle(connected);
    // while it runs, nothing else rings or starts: a start is busy
    let busy = lab.refused(Request::Call {
        chat: AB_CHAT.to_owned(),
    });
    assert_eq!(busy.code, ErrorCode::Busy, "{busy:?}");

    switches_ask_only_when_they_differ(&lab);
    overlapping_switches_end_as_the_later_one_says(&lab);

    let left = lab.row(Request::HangUp);
    assert_eq!((left.id.as_str(), left.state.as_str()), (answered.id.as_str(), "ended"));
    assert!(lab.listed().is_empty());
    assert_eq!(lab.sent().last().map(String::as_str), Some("leave"));
    let _ = lab.mock.close_peer(&id, LAB_A);
}

/// `answer` on a meeting's Request to join goes through the meeting's pre-join, as Accept then Join now: it answers
/// the ring, and the call is the meeting's.
#[test]
fn answer_on_a_meetings_request_to_join_answers_it_through_its_pre_join() {
    let lab = Lab::start(true);
    let chat = "19:meeting_MadeUpCtlRing0001@thread.v2";
    let organizer = LAB_A.trim_start_matches("8:orgid:");
    let info = serde_json::json!({"organizerId": organizer, "tenantId": TENANT});
    let id = lab.mock.ring_meeting(LAB_B, chat, &[LAB_A], info);
    lab.settle(Calls::ring_opens_prejoin);
    let answered = lab.row(Request::Answer);
    assert!(!answered.ringing && answered.state != "ended", "{answered:?}");
    assert!(lab.sent().contains(&"accept".to_owned()), "{:?}", lab.sent());
    lab.settle(|calls| {
        calls
            .call()
            .is_some_and(|call| call.kind == teams_ui::calls::CallKind::Meeting)
    });
    assert!(lab.cx.read(|cx| lab.calls().read(cx).prejoin().is_none()));
    lab.row(Request::HangUp);
    let _ = lab.mock.close_peer(&id, LAB_A);
}

/// The microphone and the camera of the call up switched by `mute` and `camera`, each asked only when it isn't as the
/// command says already; only the command turns the camera on.
fn switches_ask_only_when_they_differ(lab: &Lab) {
    let switched = |request: Request| {
        let row = lab.row(request);
        (row.muted, row.camera)
    };
    assert_eq!(switched(mute(Toggle::On)), (true, false));
    assert_eq!(switched(mute(Toggle::On)), (true, false));
    assert_eq!(switched(mute(Toggle::Off)), (false, false));
    assert_eq!(switched(mute(Toggle::Off)), (false, false));
    assert_eq!(switched(mute(Toggle::Toggle)), (true, false));
    assert_eq!(switched(mute(Toggle::Toggle)), (false, false));
    assert_eq!(lab.traced.asked("mute"), 4, "on, off and two toggles");

    // the camera likewise: only the command turns it on
    assert_eq!(switched(camera(Toggle::Off)), (false, false));
    assert_eq!(lab.traced.asked("camera"), 0, "off already");
    assert_eq!(switched(camera(Toggle::On)), (false, true));
    assert_eq!(switched(camera(Toggle::On)), (false, true));
    assert_eq!(switched(camera(Toggle::Off)), (false, false));
    assert_eq!(lab.traced.asked("camera"), 2, "on and off");
}

/// A `camera on` (a `mute on`) asked, then an `off` from another connection while the `on` is on its way (the camera
/// opening, [`CAMERA_OPENS`]): the `off` waits for the `on` to be answered and turns it off again, rather than finding
/// the switch still off and asking nothing.
fn overlapping_switches_end_as_the_later_one_says(lab: &Lab) {
    for (kind, switch) in [("camera", camera as fn(Toggle) -> Request), ("mute", mute)] {
        let before = lab.traced.asked(kind);
        let (socket, traced) = (lab.socket.clone(), lab.traced.clone());
        let (on, off) = pumped(&lab.cx, move || {
            let first = socket.clone();
            let on = std::thread::spawn(move || connect(&first)?.request(None, switch(Toggle::On)));
            // the `off` sent once the `on` was asked of the calls, while the engine sets it
            let start = Instant::now();
            while traced.asked(kind) == before {
                assert!(start.elapsed() < WAIT, "{kind} on not asked");
                std::thread::sleep(Duration::from_millis(1));
            }
            let off = connect(&socket).and_then(|mut client| client.request(None, switch(Toggle::Off)));
            (on.join().unwrap(), off)
        });
        let switched = |answer: Result<Response, ClientError>| match answer {
            Ok(Response::Call(row)) => (row.muted, row.camera),
            other => panic!("{kind}: {other:?}"),
        };
        let (on, off) = (switched(on), switched(off));
        let calls = lab.calls();
        let shown = lab.cx.read(|cx| {
            let call = calls.read(cx).call().unwrap();
            (call.muted, call.camera)
        });
        assert_eq!(off, (false, false), "{kind} off answered as it is (on answered {on:?})");
        assert_eq!(shown, (false, false), "{kind} stays off");
        assert_eq!(lab.traced.asked(kind), before + 2, "{kind} on, then off");
    }
}

/// A `mute off` with a's microphone missing reopens it first ([`MICROPHONE_OPENS`]); a mute asked meanwhile (as an
/// organizer's Mute all asks it, not through the calls) takes its place: the command says so, rather than an earlier
/// refusal or one that says nothing, and the call stays muted with nothing told in its bar.
#[test]
fn an_unmute_a_newer_mute_overtook_while_the_microphone_reopened_says_so() {
    let lab = Lab::with_microphone(false, true);
    let id = lab.ring();
    lab.row(Request::Answer);
    lab.settle(connected);
    lab.settle(|calls| {
        calls
            .call()
            .is_some_and(|call| call.muted && call.check.microphone_lost)
    });
    let calls = lab.calls();
    let call = lab.cx.read(|cx| calls.read(cx).call().unwrap().id);
    let core = lab.cx.read(|cx| app::bridges(cx)[0].read(cx).core());

    let unmute = {
        let socket = lab.socket.clone();
        std::thread::spawn(move || connect(&socket)?.request(None, mute(Toggle::Off)))
    };
    let start = Instant::now();
    while lab.traced.asked("mute") == 0 {
        assert!(start.elapsed() < WAIT, "the unmute wasn't asked");
        lab.cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(1));
    }
    // asked of the engine after the unmute, while its microphone reopens
    let muted = lab.runtime.block_on(core.call(CallAction::Mute { call, muted: true }));
    assert_eq!(muted.unwrap(), call);
    ctl_support::pump_until_done(&lab.cx, &unmute);
    let refused = match unmute.join().unwrap() {
        Err(ClientError::Server(error)) => error,
        other => panic!("mute off: {other:?}"),
    };
    assert_eq!(refused.code, ErrorCode::Engine, "{refused:?}");
    assert!(refused.message.contains("newer mute"), "{}", refused.message);

    let rows = lab.listed();
    assert!(rows.len() == 1 && rows[0].muted, "stays muted: {rows:?}");
    assert!(
        lab.cx.read(|cx| calls.read(cx).refused().is_none()),
        "nothing went wrong to tell"
    );
    assert_eq!(lab.traced.asked("mute"), 1);
    lab.row(Request::HangUp);
    let _ = lab.mock.close_peer(&id, LAB_A);
}

#[test]
fn an_invitation_is_declined_by_the_command_and_a_camera_without_video_is_refused() {
    let lab = Lab::start(false);
    let _first = lab.ring();
    match lab.ask(Request::Decline) {
        Ok(Response::Call(row)) => assert_eq!(row.state, "ended", "{row:?}"),
        Ok(Response::Done) => {}
        other => panic!("decline: {other:?}"),
    }
    assert!(lab.listed().is_empty());
    assert!(!lab.sent().contains(&"accept".to_owned()), "{:?}", lab.sent());
    assert_eq!(lab.refused(Request::Answer).code, ErrorCode::NotRinging);

    // the next one answered: this app's calls carry no video
    let id = lab.ring();
    lab.row(Request::Answer);
    lab.settle(connected);
    assert_eq!(lab.refused(camera(Toggle::On)).code, ErrorCode::NoVideo);
    assert!(!lab.row(camera(Toggle::Off)).camera, "off already: nothing asked");
    lab.row(Request::HangUp);
    let _ = lab.mock.close_peer(&id, LAB_A);
}

#[test]
fn a_call_from_a_chat_rings_until_hung_up_and_one_whose_window_closed_is_left() {
    let lab = Lab::start(false);
    let call = || Request::Call {
        chat: AB_CHAT.to_owned(),
    };
    let placed = lab.row(call());
    assert_eq!(
        (placed.chat.as_deref(), placed.ringing, placed.account.as_str()),
        (Some(AB_CHAT), false, UPN)
    );
    lab.sent_at_last("start");
    assert_eq!(lab.refused(call()).code, ErrorCode::Busy);
    assert_eq!(lab.refused(Request::Answer).code, ErrorCode::NotRinging);
    let rows = lab.listed();
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        [placed.id.as_str()]
    );
    let left = lab.row(Request::HangUp);
    assert_eq!((left.id.as_str(), left.state.as_str()), (placed.id.as_str(), "ended"));
    assert!(lab.listed().is_empty());

    // again; its call window closed while it rings at b: left as the window's close leaves it, nothing to hang up
    let again = lab.row(call());
    assert_ne!(again.id, placed.id);
    let window = lab
        .cx
        .read(|cx| lab.desktop.read(cx).call_windows().read(cx).window())
        .expect("the call's window");
    lab.cx
        .update(|cx| window.update(cx, |_, window, _| window.remove_window()))
        .unwrap();
    lab.settle(|calls| calls.call().is_none());
    assert!(lab.listed().is_empty());
    assert_eq!(lab.refused(Request::HangUp).code, ErrorCode::NoCall);
}

/// This week's event called `subject`, as `calendar` lists it.
fn this_weeks(lab: &Lab, subject: &str) -> CalendarRow {
    let today = local_today();
    let last = (0..6).fold(today, |day, _| day.succ_opt().unwrap());
    let request = Request::Calendar {
        from: Some(today.format("%Y-%m-%d").to_string()),
        to: Some(last.format("%Y-%m-%d").to_string()),
    };
    let Ok(Response::Calendar { events }) = lab.ask(request) else {
        panic!("the calendar wasn't read");
    };
    events
        .into_iter()
        .find(|event| event.subject == subject)
        .unwrap_or_else(|| panic!("no {subject} this week"))
}

#[test]
fn a_meeting_joined_now_whose_media_setup_fails_after_its_call_was_told_is_refused() {
    let lab = Lab::start(false);
    let event = this_weeks(&lab, "Product all hands");
    assert!(event.joinable, "{event:?}");
    // the relay's credentials refused a second after asked: the engine has told the join's call by then
    lab.runtime.block_on(
        lab.mock
            .refuse_next_after("GET", "/trap/tokens", 500, Duration::from_secs(1)),
    );
    let refused = lab.refused(Request::JoinMeeting {
        event: event.id,
        now: true,
    });
    assert_eq!(refused.code, ErrorCode::Engine, "{refused:?}");
    assert!(refused.message.contains("relay"), "{}", refused.message);
    assert_eq!(lab.traced.asked("join-meeting"), 1);
    lab.settle(|calls| calls.call().is_none() && !calls.busy());
    assert!(lab.listed().is_empty());
    assert!(lab.mock.calls().is_empty(), "nothing went to the calling service");
}

#[test]
fn an_app_without_calling_answers_no_calling() {
    let cx = TestAppContext::single();
    let dir = tempfile::tempdir().unwrap();
    let socket = ctl_support::socket_in(&dir);
    ctl_support::launched(&cx, dir.path(), &socket);
    for request in [
        Request::Call {
            chat: AB_CHAT.to_owned(),
        },
        Request::Answer,
        Request::HangUp,
        mute(Toggle::On),
        camera(Toggle::Toggle),
    ] {
        let asked = format!("{request:?}");
        match ask(&cx, &socket, None, request) {
            Err(ClientError::Server(error)) => assert_eq!(error.code, ErrorCode::NoCalling, "{asked}"),
            other => panic!("{asked}: {other:?}"),
        }
    }
    assert!(matches!(ask(&cx, &socket, None, Request::Calls), Ok(Response::Calls { calls }) if calls.is_empty()));
    quit(&cx);
}
