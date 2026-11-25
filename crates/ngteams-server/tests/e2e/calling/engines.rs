//! The client's engine (`teams-core`) calling through the server, with its calls' media (`teams-media`) on the file
//! device backend, relayed through the loopback TURN server the server names and hands out the credentials of (the
//! engines are given the relay a shipped client's are, [`Relay::Teams`]: an own-server account's is its server's): a
//! 1:1 call connects, a tone and video cross both ways, mute and hold show at the far end, and the chat tells the call.
//! The same with each client on another node.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Event, Parts, Push};
use teams_core::{
    CallAction, CallFloor, CallId, CallMedia, CallParts, CallStatus, CallVideo, CallView, End, IncomingCall, Outbox,
    Relay, VideoOutput,
};
use teams_media::devices::file::{AudioInput, FileDevices, FrameCounter, Pacing, PatternCamera, StillSource};
use teams_media::devices::{AudioSink, AudioSource, DeviceError, Devices, VideoFrame, VideoSink, VideoSource, wav};
use teams_media::processing::ProcessingConfig;
use teams_media::session::SessionConfig;
use teams_media::test_turn::{self, TurnServer};
use teams_media::tone::{Tone, level_at};
use teams_media::video::codec::EncoderSettings;
use teams_media::video::convert::MaxSize;
use teams_proto::chat::ChatReads;
use teams_proto::{Author, Client, Cloud, Mri};
use teams_store::{OutboxState, Store};
use teams_trouter::{CallStack, RegistrationReport, Trouter};

use super::client::identity;
use crate::support::{TestServer, backends, redis_url};
use crate::trouter::{Account, ORIGIN, options};

pub(super) const WAIT: Duration = Duration::from_secs(40);
/// How many of the events a wait passed over its timeout shows.
const SKIPPED_SHOWN: usize = 8;
/// How long the media runs once connected, before what was heard is judged.
pub(super) const TALK: Duration = Duration::from_secs(2);
/// The far tone is heard at least this loud in a recording.
pub(super) const HEARD: f32 = 0.25;
/// Frames a side must have decoded of the other's test pattern.
pub(super) const FRAMES: u64 = 20;
pub(super) const A_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.5,
};
pub(super) const B_TONE: Tone = Tone {
    hz: 660.0,
    amplitude: 0.5,
};

/// What an engine's calls showed: the far end's decoded frames (all lines together, and each line apart) and its own
/// camera's.
#[derive(Clone, Default)]
pub(super) struct Shown {
    remote: FrameCounter,
    preview: FrameCounter,
    lines: Arc<Mutex<BTreeMap<usize, Seen>>>,
}

/// What one video line showed: how many frames, the last one's size, and when it came.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Seen {
    pub(super) frames: u64,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) last: Option<std::time::Instant>,
}

/// One line's sink: counted with all the others, and apart.
struct LineSink {
    all: FrameCounter,
    line: usize,
    lines: Arc<Mutex<BTreeMap<usize, Seen>>>,
}

impl VideoSink for LineSink {
    fn show(&mut self, frame: &VideoFrame) -> Result<(), DeviceError> {
        let mut lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        let seen = lines.entry(self.line).or_default();
        seen.frames += 1;
        (seen.width, seen.height) = (frame.width, frame.height);
        seen.last = Some(std::time::Instant::now());
        drop(lines);
        self.all.show(frame)
    }
}

impl VideoOutput for Shown {
    fn remote(&self, line: usize) -> Box<dyn VideoSink> {
        Box::new(LineSink {
            all: self.remote.clone(),
            line,
            lines: Arc::clone(&self.lines),
        })
    }

    fn preview(&self) -> Option<Box<dyn VideoSink>> {
        Some(Box::new(self.preview.clone()))
    }
}

/// The screen an engine shares: the file backend's moving pattern, one that holds still after its first frames, as a
/// desktop's does while nothing on it changes, or a moving 1080p one, a real desktop's size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Screen {
    Moving,
    Still,
    FullHd,
}

/// A 1080p screen, at a screen's rate.
pub(super) const FULL_HD: PatternCamera = PatternCamera {
    width: 1920,
    height: 1080,
    fps: 15,
};

/// The file backend's devices, its screen as [`Screen`] says.
struct TestDevices {
    files: FileDevices,
    screen: Screen,
}

impl Devices for TestDevices {
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
        let screen = self.files.screen()?;
        Ok(match self.screen {
            Screen::Moving => screen,
            Screen::Still => Box::new(StillSource::new(screen, 3)),
            Screen::FullHd => FileDevices {
                camera: FULL_HD,
                ..self.files.clone()
            }
            .camera()?,
        })
    }
}

/// One account's engine, its calls' media a tone in and a recording out.
pub(super) struct Engine {
    pub(super) mri: String,
    pub(super) account: Account,
    pub(super) handle: EngineHandle,
    pub(super) recording: PathBuf,
    video: Shown,
    /// The last view each call told of: [`Engine::shown`] starts from it, since a wait for another event passes over
    /// views.
    views: std::collections::HashMap<CallId, CallView>,
    _store: tempfile::TempDir,
}

impl Engine {
    /// The account's engine: its sockets on `socket`, its requests through `http`, its media through the relay its
    /// server names.
    pub(super) async fn start(
        account: &Account,
        name: &str,
        nodes: (&TestServer, &TestServer),
        (tone, dir): (Tone, &Path),
    ) -> Self {
        Self::start_filmed(account, name, nodes, (tone, dir, PatternCamera::default())).await
    }

    /// [`Engine::start`] with its camera's picture of the shape `camera`.
    pub(super) async fn start_filmed(
        account: &Account,
        name: &str,
        nodes: (&TestServer, &TestServer),
        (tone, dir, camera): (Tone, &Path, PatternCamera),
    ) -> Self {
        let transports = (account.transport(nodes.1), account.transport(nodes.1));
        Self::start_on(account, name, nodes, transports, (tone, dir, camera, Screen::Moving)).await
    }

    /// [`Engine::start`] that shares `screen`.
    pub(super) async fn start_sharing(
        account: &Account,
        name: &str,
        nodes: (&TestServer, &TestServer),
        (tone, dir, screen): (Tone, &Path, Screen),
    ) -> Self {
        let transports = (account.transport(nodes.1), account.transport(nodes.1));
        let film = (tone, dir, PatternCamera::default(), screen);
        Self::start_on(account, name, nodes, transports, film).await
    }

    /// [`Engine::start`] with every service base the account's authz names, as the account's bring-up sets them: the
    /// middle tier and CSA too (the calendar, a channel's meeting), sharing `screen`.
    pub(super) async fn start_served_sharing(
        account: &Account,
        name: &str,
        nodes: (&TestServer, &TestServer),
        (tone, dir, screen): (Tone, &Path, Screen),
    ) -> Self {
        let id = account.mri.trim_start_matches("8:orgid:");
        let transports = (
            crate::teams::transport(&account.session, nodes.1, id).await,
            crate::teams::transport(&account.session, nodes.1, id).await,
        );
        let film = (tone, dir, PatternCamera::default(), screen);
        Self::start_on(account, name, nodes, transports, film).await
    }

    async fn start_on(
        account: &Account,
        name: &str,
        (socket, http): (&TestServer, &TestServer),
        (transport, reads): (crate::trouter::Transport, crate::trouter::Transport),
        (tone, dir, camera, screen): (Tone, &Path, PatternCamera, Screen),
    ) -> Self {
        let cloud = Cloud::own(ORIGIN).unwrap();
        let mri = Mri::new(account.mri.clone());
        let tracker = Arc::new(teams_call::Tracker::for_cloud(mri.clone(), cloud));
        let transport = Arc::new(transport.with_call_state(tracker.clone()));
        let trouter = || {
            Trouter::with_mock_origin(
                Arc::clone(&transport),
                Arc::clone(&account.session),
                &socket.origin.replace("http://", "ws://"),
            )
            .unwrap()
            .with_options(options())
        };
        let recording = dir.join(format!("{name}.wav"));
        let video = Shown::default();
        let media = CallMedia {
            devices: Arc::new(TestDevices {
                files: FileDevices {
                    input: AudioInput::Tone(tone),
                    recording: Some(recording.clone()),
                    camera,
                    pacing: Pacing::RealTime,
                },
                screen,
            }),
            relay: Relay::Teams,
            session: SessionConfig {
                processing: ProcessingConfig::OFF,
                ..Default::default()
            },
            video: Some(CallVideo {
                encoder: EncoderSettings::default(),
                max_size: MaxSize::HD,
                output: Arc::new(video.clone()),
            }),
        };
        let tenant = ngteams_server::db::meta::tenant(&http.app.db).await.unwrap();
        let calling = trouter().for_calls(CallStack::new(Arc::clone(&tracker)));
        let calls = CallParts::new(calling, identity(&account.mri, name, &tenant, ""), media);
        let store = tempfile::tempdir().unwrap();
        let outbox = Outbox::new(Arc::clone(&transport), Author::new(mri, name)).with_calls(tracker, CallFloor::new());
        let handle = engine::start(Parts {
            calls: Some(calls),
            store: Store::open(&store.path().join("store.sqlite")).unwrap(),
            client: Arc::new(Client::new(reads)),
            outbox,
            push: Some(Push::new(trouter())),
            options: EngineOptions {
                tick: Duration::from_millis(50),
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })
        .unwrap();
        let mut engine = Self {
            mri: account.mri.clone(),
            account: Account {
                mri: account.mri.clone(),
                session: Arc::clone(&account.session),
            },
            handle,
            recording,
            video,
            views: std::collections::HashMap::new(),
            _store: store,
        };
        engine.ready().await;
        engine
    }

    /// Waits until both its sockets are registered.
    async fn ready(&mut self) {
        let (mut messaging, mut calling) = (false, false);
        while !(messaging && calling) {
            let which = self
                .next(|event| match event {
                    Event::Registration(RegistrationReport::Registered) => Some(true),
                    Event::CallingRegistration(RegistrationReport::Registered) => Some(false),
                    _ => None,
                })
                .await;
            messaging |= which;
            calling |= !which;
        }
    }

    pub(super) async fn next<T>(&mut self, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + WAIT;
        let mut skipped = std::collections::VecDeque::new();
        loop {
            let event = tokio::time::timeout_at(deadline, self.handle.next_event())
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "{}: the event didn't come in time; the last others: {skipped:#?}",
                        self.mri
                    )
                })
                .expect("the engine runs");
            if let Event::CallParticipants { call, view } = &event {
                self.views.insert(*call, view.clone());
            }
            if let Some(picked) = pick(&event) {
                return picked;
            }
            if skipped.len() == SKIPPED_SHOWN {
                skipped.pop_front();
            }
            skipped.push_back(event);
        }
    }

    /// Asks `action`, again while the call can't take it yet (a renegotiation still settling), as a user's second
    /// press would.
    pub(super) async fn call(&self, action: CallAction) -> CallId {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            match self.handle.requests().call(action.clone()).await {
                Ok(call) => return call,
                Err(error) if tokio::time::Instant::now() < deadline => {
                    eprintln!("{}: {action:?} not taken yet: {error}", self.mri);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(error) => panic!("{action:?}: {error}"),
            }
        }
    }

    pub(super) async fn status(&mut self, call: CallId, wanted: &CallStatus) {
        self.next(|event| match event {
            Event::Call { call: of, status } if *of == call && status == wanted => Some(()),
            Event::Call {
                call: of,
                status: CallStatus::Ended(end),
            } if *of == call => panic!("call {} ended ({end:?}) before {wanted:?}", call.get()),
            _ => None,
        })
        .await;
    }

    pub(super) async fn ended(&mut self, call: CallId) -> End {
        self.next(|event| match event {
            Event::Call {
                call: of,
                status: CallStatus::Ended(end),
            } if *of == call => Some(end.clone()),
            _ => None,
        })
        .await
    }

    pub(super) async fn incoming(&mut self) -> IncomingCall {
        self.next(|event| match event {
            Event::IncomingCall(incoming) => Some(incoming.clone()),
            _ => None,
        })
        .await
    }

    /// The next view of `call` that is `wanted`, among the views that no earlier wait read. A view in the queue can be
    /// older than the action before this wait, so `wanted` must be false for the views before that action.
    pub(super) async fn view(&mut self, call: CallId, wanted: impl Fn(&CallView) -> bool) -> CallView {
        self.next(|event| match event {
            Event::CallParticipants { call: of, view } if *of == call && wanted(view) => Some(view.clone()),
            _ => None,
        })
        .await
    }

    /// The view of `call` once it is `wanted`: the last view told, or the next that is. The last view told can be
    /// older than the views still in the queue, so use this only when no view after it can make `wanted` false
    /// again (cameras that turned on and stay on).
    pub(super) async fn shown(&mut self, call: CallId, wanted: impl Fn(&CallView) -> bool) -> CallView {
        if let Some(last) = self.views.get(&call).filter(|last| wanted(last)) {
            return last.clone();
        }
        self.view(call, wanted).await
    }

    /// The next word of whether the far end holds `call`.
    async fn held_by_far_end(&mut self, call: CallId) -> bool {
        self.next(|event| match event {
            Event::CallHeldByFarEnd { call: of, held } if *of == call => Some(*held),
            _ => None,
        })
        .await
    }

    /// What its video line `line` showed so far.
    pub(super) fn seen(&self, line: usize) -> Seen {
        let lines = self.video.lines.lock().unwrap_or_else(PoisonError::into_inner);
        lines.get(&line).copied().unwrap_or_default()
    }

    pub(super) async fn shut_down(self) {
        tokio::task::spawn_blocking(move || self.handle.shutdown())
            .await
            .unwrap();
    }
}

/// The level of `tone` in the last `stretch` of the recording at `path`, leaving out its last 200 ms.
pub(super) fn heard(path: &Path, tone: Tone, stretch: Duration) -> f32 {
    let end = recorded(path).saturating_sub(in_samples(Duration::from_millis(200)));
    heard_before(path, tone, stretch, end)
}

/// The samples of the recording at `path` that a read gets now. The writer updates the header each second, so this
/// count is at most 1 s behind the recording.
pub(super) fn recorded(path: &Path) -> usize {
    wav::read(path).unwrap().len()
}

/// The level of `tone` in the `stretch` of the recording at `path` that ends at sample `end`.
pub(super) fn heard_before(path: &Path, tone: Tone, stretch: Duration, end: usize) -> f32 {
    let samples = wav::read(path).unwrap();
    let end = end.min(samples.len());
    let start = end.saturating_sub(in_samples(stretch));
    assert!(
        end - start >= 24_000,
        "{} holds {} samples",
        path.display(),
        samples.len()
    );
    level_at(&samples[start..end], tone.hz)
}

/// `span` in samples at 48 kHz.
fn in_samples(span: Duration) -> usize {
    usize::try_from(span.as_millis()).unwrap() * 48
}

pub(super) async fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !done() {
        assert!(tokio::time::Instant::now() < deadline, "{what} didn't happen in time");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The TURN relay the server names, and the credentials it hands out: the loopback test server at `relay` and its one
/// user.
pub(super) fn relayed(relay: std::net::SocketAddr) -> impl FnOnce(&mut ngteams_server::config::Config) {
    move |config| {
        config.public_origin = url::Url::parse(ORIGIN).unwrap();
        config.calling.turn.urls = vec![format!("turn:{relay}?transport=udp")];
        config.calling.turn.username = Some(test_turn::USER.to_owned());
        config.calling.turn.password = Some(test_turn::PASSWORD.to_owned());
    }
}

/// The loopback relay carried the call: both engines allocated on it and it relayed their media.
fn carried(turn: &TurnServer) {
    let allocations = turn.counters.allocations.load(std::sync::atomic::Ordering::Relaxed);
    let relayed = turn.counters.rtp_relayed.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        allocations >= 2 && relayed > 0,
        "allocations {allocations}, RTP relayed {relayed}"
    );
}

/// Two engines in a call: a's and b's, each with its own id of the call.
struct Talk {
    a: Engine,
    b: Engine,
    call: CallId,
    answered: CallId,
    chat: String,
}

/// The chat a makes with b, once b has it.
async fn chat_of(a: &Engine, b: &Engine) -> String {
    let create = Action::CreateChat {
        others: vec![Mri::new(b.mri.clone())],
        topic: None,
    };
    let row = a.handle.write(create).await.unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    assert!(b.handle.sync().await.unwrap().failed.is_empty());
    row.message_id.unwrap()
}

/// a's engine on `node_a` and b's on `node_b`, both accounts new.
async fn engines(node_a: &TestServer, node_b: &TestServer, dir: &Path) -> (Engine, Engine) {
    // names of this call's own: with Redis, sign-ins are counted per name across every test running now
    let tag = uuid::Uuid::new_v4().simple().to_string();
    let (a_upn, b_upn) = (format!("a-{tag}@own.test"), format!("b-{tag}@own.test"));
    let a_id = node_a.add_user(&a_upn, "Ada").await;
    let b_id = node_a.add_user(&b_upn, "Bert").await;
    let a_account = Account::sign_in(node_a, &a_upn, &a_id).await;
    let b_account = Account::sign_in(node_b, &b_upn, &b_id).await;
    let a = Engine::start(&a_account, "Ada", (node_a, node_a), (A_TONE, dir)).await;
    let b = Engine::start(&b_account, "Bert", (node_b, node_b), (B_TONE, dir)).await;
    (a, b)
}

/// a calls b in their chat and b answers: both are connected and see each other.
async fn connect(node_a: &TestServer, node_b: &TestServer, dir: &Path) -> Talk {
    let (mut a, mut b) = engines(node_a, node_b, dir).await;
    let chat = chat_of(&a, &b).await;
    let call = a.call(CallAction::Call { chat: chat.clone() }).await;
    a.status(call, &CallStatus::Ringing).await;
    let incoming = b.incoming().await;
    assert_eq!(incoming.caller.as_str(), a.mri);
    let answered = b.call(CallAction::Answer { call: incoming.call }).await;
    b.status(answered, &CallStatus::Connected).await;
    a.status(call, &CallStatus::Connected).await;
    let shows = |who: String| move |view: &CallView| view.participants.iter().any(|party| party.mri == who);
    // the roster can come before `Connected`, whose wait passes over it: the last view counts too
    a.shown(call, shows(b.mri.clone())).await;
    b.shown(answered, shows(a.mri.clone())).await;
    Talk {
        a,
        b,
        call,
        answered,
        chat,
    }
}

impl Talk {
    /// a's camera, then b's: each renegotiation is forwarded, answered and acknowledged through the server.
    async fn video_both_ways(&self) {
        let (a, b) = (&self.a, &self.b);
        let on = |call| CallAction::Camera { call, on: true };
        a.call(on(self.call)).await;
        wait_until("a's video at b", || b.video.remote.counts().frames >= FRAMES).await;
        b.call(on(self.answered)).await;
        wait_until("b's video at a", || a.video.remote.counts().frames >= FRAMES).await;
        assert!(a.video.preview.counts().frames >= FRAMES, "a's self view");
    }

    /// b's mute shows on a; a's hold shows on b, and its end.
    async fn mute_and_hold(&mut self) {
        let (call, answered) = (self.call, self.answered);
        let mute = |muted| CallAction::Mute { call: answered, muted };
        self.b.call(mute(true)).await;
        let b = self.b.mri.clone();
        self.a
            .view(call, |view| {
                view.participants.iter().any(|party| party.mri == b && party.muted)
            })
            .await;
        self.b.call(mute(false)).await;
        self.a.call(CallAction::Hold { call, held: true }).await;
        assert!(self.b.held_by_far_end(answered).await, "b sees itself held");
        self.a.call(CallAction::Hold { call, held: false }).await;
        assert!(!self.b.held_by_far_end(answered).await, "b sees the hold end");
    }

    /// a hangs up: both calls end, and each recording holds the other's tone.
    async fn hang_up(&mut self) {
        self.a.call(CallAction::HangUp { call: self.call }).await;
        assert_eq!(self.a.ended(self.call).await, End::HungUp);
        assert!(matches!(self.b.ended(self.answered).await, End::Remote(_)));
        tokio::time::sleep(Duration::from_millis(300)).await;
        let a_heard = heard(&self.a.recording, B_TONE, TALK);
        let b_heard = heard(&self.b.recording, A_TONE, TALK);
        eprintln!("a heard b at {a_heard:.3}, b heard a at {b_heard:.3}");
        assert!(a_heard > HEARD && b_heard > HEARD);
    }
}

/// The chat tells the call: its start, and its end with both in it.
async fn chat_tells_the_call(node: &TestServer, reader: &Account, chat: &str) {
    let reader = reader.client(node);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let events = loop {
        let page = ChatReads::new(&reader).messages(chat, 10).await.unwrap();
        let events: Vec<String> = page
            .messages
            .iter()
            .filter(|message| message.message_type.as_str() == "Event/Call")
            .map(|message| message.content.clone())
            .collect();
        // the server writes the end to the chat after it tells the parties the call ended
        if events.len() >= 2 || tokio::time::Instant::now() > deadline {
            break events;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(
        events[0].contains("<ended/>") && events[0].contains("count=\"2\""),
        "{events:?}"
    );
    assert!(events[1].contains("callStarted"), "{events:?}");
}

/// a calls b in their chat, b answers; a tone and video cross both ways; b's mute and a's hold show at the far end;
/// a hangs up and the chat tells the call's length. a's client is on `node_a`, b's on `node_b`.
async fn a_call_with_media(node_a: &TestServer, node_b: &TestServer) {
    let dir = tempfile::tempdir().unwrap();
    let mut talk = connect(node_a, node_b, dir.path()).await;
    tokio::time::sleep(TALK).await;
    talk.video_both_ways().await;
    talk.mute_and_hold().await;
    tokio::time::sleep(TALK).await;
    talk.hang_up().await;
    chat_tells_the_call(node_b, &talk.b.account, &talk.chat).await;
    talk.a.shut_down().await;
    talk.b.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_connects_audio_and_video_through_the_servers_signaling() {
    let turn = TurnServer::start().unwrap();
    let server = TestServer::start_with(crate::support::Backend::Sqlite, relayed(turn.udp)).await;
    a_call_with_media(&server, &server).await;
    carried(&turn);
    assert!(
        ngteams_server::calling::open_calls(&server.app.db)
            .await
            .unwrap()
            .is_empty()
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_connects_audio_and_video_with_each_client_on_another_node() {
    let Some(redis) = redis_url() else { return };
    for backend in backends() {
        let turn = TurnServer::start().unwrap();
        let url = redis.clone();
        let node_a = TestServer::start_with(backend, |config| {
            relayed(turn.udp)(config);
            config.redis_url = Some(url);
        })
        .await;
        let (shared, url) = (node_a.app.config.clone(), redis.clone());
        let node_b = TestServer::start_with(backend, |config| {
            relayed(turn.udp)(config);
            config.database_url = shared.database_url;
            config.redis_url = Some(url);
        })
        .await;
        a_call_with_media(&node_a, &node_b).await;
        carried(&turn);
        node_b.stop().await;
        node_a.stop().await;
    }
}

/// With a coturn running `use-auth-secret` (its address in `NGTS_TEST_COTURN`, its secret in
/// `NGTS_TEST_COTURN_SECRET`; `docs/server.md`), the media is relayed with the credentials the server makes
/// from that secret.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_is_relayed_by_coturn_with_the_credentials_made_from_its_secret() {
    let (Ok(address), Ok(secret)) = (
        std::env::var("NGTS_TEST_COTURN"),
        std::env::var("NGTS_TEST_COTURN_SECRET"),
    ) else {
        return;
    };
    let _turn = crate::support::take_turn().await;
    let server = TestServer::start_with(crate::support::Backend::Sqlite, |config| {
        config.public_origin = url::Url::parse(ORIGIN).unwrap();
        config.calling.turn.secret = Some(secret);
        config.calling.turn.urls = vec![format!("turn:{address}?transport=udp")];
    })
    .await;
    a_call_with_media(&server, &server).await;
    server.stop().await;
}
