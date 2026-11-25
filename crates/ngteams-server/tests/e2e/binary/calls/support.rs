use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;
use teams_auth::secrets::MemorySecretStore;
use teams_auth::session::Session;
use teams_call::{Identity, RemoteEnd, Tracker};
use teams_core::account::{
    AMS_GTM, CALL_LANGUAGE, CSA_GTM, MIDDLE_TIER_GTM, PRESENCE_GTM, PROVISIONING_GTM, USER_PROFILE_GTM, web_client_info,
};
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Event, Parts, Push, Source};
use teams_core::{
    CallAction, CallFloor, CallId, CallMedia, CallParts, CallStatus, CallVideo, CallView, End, IncomingCall, Outbox,
    Relay, VideoOutput,
};
use teams_media::devices::file::{AudioInput, FileDevices, FrameCounter, Pacing, PatternCamera};
use teams_media::devices::{DeviceError, VideoFrame, VideoSink, wav};
use teams_media::processing::ProcessingConfig;
use teams_media::session::SessionConfig;
use teams_media::stun::{Attribute, Class, IntegrityKey, Message, Method, TRANSPORT_UDP, TransactionId};
use teams_media::tone::{Tone, level_at};
use teams_media::video::codec::EncoderSettings;
use teams_media::video::convert::MaxSize;
use teams_proto::chat::{ChatEvent, chat_event};
use teams_proto::{Author, Client, Cloud, HttpTransport, Mri};
use teams_store::{OutboxState, Store};
use teams_trouter::{CallStack, Options, RegistrationReport, RegistrationTimes, Trouter};
use tempfile::TempDir;

use super::super::support::{Deployment, Node, http_client};
use crate::support::{Backend, PASSWORD, redis_url, take_turn};

type Transport = HttpTransport<Session<MemorySecretStore>>;

/// The coturn the calls' media goes through (`host:port`) and its `static-auth-secret`: `docs/server.md`.
/// Where the nodes' logs go, when set: one file a start.
pub const LOG_DIR: &str = "NGTS_QA_LOG_DIR";
pub const COTURN: &str = "NGTS_TEST_COTURN";
pub const COTURN_SECRET: &str = "NGTS_TEST_COTURN_SECRET";

pub const WAIT: Duration = Duration::from_secs(60);
/// How long the media runs before what was heard is judged: long enough that a client's one-second hiccup on a
/// loaded host (a camera starting) leaves the far tone above [`HEARD`].
pub const TALK: Duration = Duration::from_secs(4);
/// The far tone is heard at least this loud in a recording.
pub const HEARD: f32 = 0.25;
/// Frames a side must have decoded of the other's test pattern.
pub const FRAMES: u64 = 20;
pub const A_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.5,
};
pub const B_TONE: Tone = Tone {
    hz: 660.0,
    amplitude: 0.5,
};
pub const C_TONE: Tone = Tone {
    hz: 880.0,
    amplitude: 0.5,
};

/// The relay's address and secret, when the run names a coturn.
pub fn coturn() -> Option<(SocketAddr, String)> {
    let address = std::env::var(COTURN).ok()?.parse().ok()?;
    Some((address, std::env::var(COTURN_SECRET).ok()?))
}

/// What the nodes' relay is.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    /// The run's coturn, the nodes holding its secret.
    Coturn,
    /// None configured.
    Missing,
}

/// The real server with its calls: one node on SQLite, or two nodes on one Postgres and one Redis; accounts a, b
/// and c the operator added.
pub struct Rig {
    pub origin: String,
    pub database: String,
    pub first: Node,
    pub second: Option<Node>,
    pub relay: SocketAddr,
    pub secret: String,
    logs: Option<PathBuf>,
    starts: u32,
    _deployment: Deployment,
    /// The rig's turn while the run's records are shared (`crate::support::take_turn`).
    _turn: Option<tokio::sync::MutexGuard<'static, ()>>,
}

impl Rig {
    /// `None` (the test is skipped, and says so) when the run names no coturn.
    pub async fn start(backend: Backend, flow: &str, ring_timeout: u64, turn: Turn) -> Option<Self> {
        let Some((relay, secret)) = coturn() else {
            eprintln!("skipped: {COTURN} and {COTURN_SECRET} name no coturn");
            return None;
        };
        let table =
            (turn == Turn::Coturn).then(|| format!("secret = {secret:?}\nurls = [\"turn:{relay}?transport=udp\"]\n"));
        Self::start_with(backend, flow, ring_timeout, relay, table.as_deref()).await
    }

    /// As [`Self::start`], the nodes' `[calling.turn]` being `turn` as written (none: no table), the run's relay the
    /// coturn at `relay`.
    pub async fn start_with(
        backend: Backend,
        flow: &str,
        ring_timeout: u64,
        relay: SocketAddr,
        turn: Option<&str>,
    ) -> Option<Self> {
        let Some((_, secret)) = coturn() else {
            eprintln!("skipped: {COTURN} and {COTURN_SECRET} name no coturn");
            return None;
        };
        let turn_taken = take_turn().await;
        eprintln!("real-binary calls, {flow}: {backend:?}");
        let redis = (backend == Backend::Postgres).then(redis_url).flatten();
        assert!(
            backend != Backend::Postgres || redis.is_some(),
            "Postgres QA requires Redis"
        );
        let mut deployment = Deployment::new(backend, false, redis).await;
        deployment.origin = deployment.origin.replacen("http:", "https:", 1);
        let mut config = format!("[calling]\nring_timeout = {ring_timeout}\n");
        if let Some(turn) = turn {
            let _ = write!(config, "[calling.turn]\n{turn}");
        }
        let first = deployment.node("a");
        append(&first.config, &config);
        first.cli(&["migrate"], "");
        for (upn, name) in [("a@own.test", "Ada"), ("b@own.test", "Bert"), ("c@own.test", "Cora")] {
            first.cli(&["user", "add", upn, "--name", name], &format!("{PASSWORD}\n"));
        }
        let second = (backend == Backend::Postgres).then(|| deployment.node("b"));
        if let Some(node) = &second {
            append(&node.config, &config);
        }
        let mut rig = Self {
            origin: deployment.origin.clone(),
            database: deployment.database.clone(),
            first,
            second,
            relay,
            secret,
            logs: std::env::var_os(LOG_DIR).map(|dir| Path::new(&dir).join(format!("calls-{flow}-{backend:?}"))),
            starts: 0,
            _deployment: deployment,
            _turn: turn_taken,
        };
        rig.start_node(true).await;
        if rig.second.is_some() {
            rig.start_node(false).await;
        }
        Some(rig)
    }

    /// Starts the first node, or the second.
    pub async fn start_node(&mut self, first: bool) {
        self.starts += 1;
        let (node, name) = if first {
            (&mut self.first, "a")
        } else {
            (self.second.as_mut().unwrap(), "b")
        };
        let Some(dir) = &self.logs else {
            return node.start().await;
        };
        std::fs::create_dir_all(dir).unwrap();
        node.start_logged(&dir.join(format!("node-{name}-start-{}.log", self.starts)))
            .await;
    }

    /// Stops the first node, or the second: a restart of the service on that machine begins.
    pub async fn stop_node(&mut self, first: bool) {
        if first {
            self.first.stop().await;
        } else {
            self.second.as_mut().unwrap().stop().await;
        }
    }

    /// The node the second of two clients talks to: the other node when there are two.
    pub fn other(&self) -> &Node {
        self.second.as_ref().unwrap_or(&self.first)
    }

    pub fn two_nodes(&self) -> bool {
        self.second.is_some()
    }

    /// A running client of `upn` at `node`, its microphone a tone.
    pub async fn phone(&self, node: &Node, upn: &str, name: &str, tone: Tone) -> Phone {
        let mut phone = Phone::signed_in(node, &self.origin, (upn, name), tone).await;
        phone.start();
        phone.online(0).await;
        phone
    }

    /// The calls the server holds that aren't over, read from its database.
    pub async fn open_calls(&self) -> Vec<ngteams_server::calling::OpenCall> {
        let db = ngteams_server::db::Db::connect(&self.database, 1).await.unwrap();
        ngteams_server::calling::open_calls(&db).await.unwrap()
    }

    /// A calling endpoint `phone`'s client registered, as its callbacks name it, read from the server's database.
    pub async fn calling_endpoint(&self, phone: &Phone) -> String {
        let db = ngteams_server::db::Db::connect(&self.database, 1).await.unwrap();
        let stack = ngteams_server::trouter::Stack::Calling;
        let registered = ngteams_server::registrar::registered(&db, phone.oid(), stack.app_id())
            .await
            .unwrap();
        let epid = registered.first().expect("a registered calling endpoint");
        ngteams_server::trouter::endpoint_id(phone.oid(), epid, stack)
    }

    /// Waits until the server holds no call that isn't over.
    pub async fn no_open_calls(&self) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let open = self.open_calls().await;
            if open.is_empty() {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "calls left open on the server: {open:?}"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub async fn stop(mut self) {
        if let Some(node) = &mut self.second {
            node.stop().await;
        }
        self.first.stop().await;
    }
}

fn append(config: &Path, tables: &str) {
    let mut file = std::fs::OpenOptions::new().append(true).open(config).unwrap();
    writeln!(file, "{tables}").unwrap();
}

/// What a client's calls showed: the far end's decoded frames (all lines together, and each line apart) and its own
/// camera's.
#[derive(Clone, Default)]
pub struct Shown {
    pub remote: FrameCounter,
    pub preview: FrameCounter,
    lines: Arc<Mutex<BTreeMap<usize, Seen>>>,
}

/// What one video line showed: how many frames, and the last one's size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub frames: u64,
    pub width: u32,
    pub height: u32,
}

impl Shown {
    /// What line `line` showed so far.
    pub fn line(&self, line: usize) -> Seen {
        let lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        lines.get(&line).copied().unwrap_or_default()
    }

    /// What each line showed so far, by line.
    pub fn lines(&self) -> BTreeMap<usize, Seen> {
        self.lines.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
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

/// One of a chat's call lines, as Teams words it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// "Call started".
    Started,
    /// "Call ended `<length>`" with two people in it; "Call missed" at the callee with the caller alone.
    Ended { seconds: Option<u64>, people: Vec<String> },
}

impl Line {
    fn read(message_type: &str, content: &str) -> Option<Self> {
        match chat_event(message_type, content)? {
            ChatEvent::CallStarted(_) => Some(Self::Started),
            ChatEvent::CallEnded(end) => Some(Self::Ended {
                seconds: end.duration,
                people: end.participants,
            }),
            _ => None,
        }
    }

    /// Whether it is the end of a call both of `people` were in.
    pub fn ended_with(&self, people: [&str; 2]) -> bool {
        matches!(self, Self::Ended { seconds: Some(_), people: in_it } if in_it.len() == 2
            && people.iter().all(|one| in_it.iter().any(|other| other == one)))
    }

    /// Whether it is the end of a call nobody but `caller` was in: "Call missed" for the callee.
    pub fn missed_from(&self, caller: &str) -> bool {
        matches!(self, Self::Ended { seconds: Some(_), people } if people.len() == 1 && people[0] == caller)
    }
}

/// One running ngteams client of an account, as the app's account bring-up builds it (`teams_core::account`): its
/// session, its engine on its store, its messaging and calling push channels, its calls' media on the file devices
/// (a tone in, a recording out, a pattern for a camera) through the relay the server names.
pub struct Phone {
    pub upn: String,
    pub mri: String,
    pub log: Vec<Event>,
    pub recording: PathBuf,
    pub video: Shown,
    /// The camera's picture: its shape tells this client's video apart at the others.
    pub camera: PatternCamera,
    name: String,
    tone: Tone,
    /// A WAV its microphone plays once in place of [`Self::tone`] ([`Self::speak`]).
    speech: Option<PathBuf>,
    engine: Option<EngineHandle>,
    session: Arc<Session<MemorySecretStore>>,
    origin: String,
    http: String,
    /// Where its push sockets connect, when not straight to its node (`ws://…`): a proxy in front of the node.
    socket: Option<String>,
    /// Its engine's `catch_up_retry`: 2 s unless set ([`Self::catch_up_as_the_app`]).
    catch_up_retry: Duration,
    /// Its requests also reach the calendar, CSA, provisioning and the profiles ([`Self::served`]).
    served: bool,
    /// Its requests also reach pictures (AMS) and presence, as the app's do ([`Self::whole`]).
    whole: bool,
    dir: TempDir,
}

fn options() -> Options {
    Options {
        backoff_base: Duration::from_millis(300),
        backoff_cap: Duration::from_secs(2),
        backoff_jitter_percent: 0,
        registration: RegistrationTimes::default(),
        ..Options::default()
    }
}

impl Phone {
    /// Signed in at `node`, its client not yet started.
    pub async fn signed_in(node: &Node, origin: &str, (upn, name): (&str, &str), tone: Tone) -> Self {
        let session = Arc::new(node.sign_in(upn, PASSWORD).await);
        let skype = session.skypetoken().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        Self {
            upn: upn.into(),
            mri: format!("8:{}", skype.skype_id.as_deref().unwrap()),
            log: Vec::new(),
            recording: dir.path().join("heard.wav"),
            video: Shown::default(),
            camera: PatternCamera::default(),
            name: name.into(),
            tone,
            speech: None,
            engine: None,
            session,
            origin: origin.into(),
            http: node.endpoint.clone(),
            socket: None,
            catch_up_retry: Duration::from_secs(2),
            served: false,
            whole: false,
            dir,
        }
    }

    /// Its microphone plays the WAV at `wav` (48 kHz) once, then silence, from its next start on.
    pub fn speak(&mut self, wav: PathBuf) {
        self.speech = Some(wav);
    }

    /// Its engine waits as long as the app's for a catch-up a delivery made due, from its next start on.
    pub fn catch_up_as_the_app(&mut self) {
        self.catch_up_retry = EngineOptions::default().catch_up_retry;
    }

    /// Its requests reach every service the app's reach (the calendar and meetings, CSA, teams), at the bases authz
    /// names, from its next start on.
    pub fn served(&mut self) {
        self.served = true;
    }

    /// As [`Self::served`], and its requests reach pictures (AMS) and presence too: every service the app's account
    /// reaches on an own server, from its next start on.
    pub fn whole(&mut self) {
        self.served = true;
        self.whole = true;
    }

    /// Its push sockets connect to `socket` (`ws://127.0.0.1:<port>`) from its next start on.
    pub fn connect_through(&mut self, socket: &str) {
        self.socket = Some(socket.to_owned());
    }

    /// Its HTTP requests go to `http` (`http://127.0.0.1:<port>`, a proxy in front of its node) from its next start
    /// on; its push sockets stay on the node unless [`Self::connect_through`] says otherwise.
    pub fn http_through(&mut self, http: &str) {
        if self.socket.is_none() {
            self.socket = Some(self.http.replacen("http://", "ws://", 1));
        }
        http.clone_into(&mut self.http);
    }

    /// The account's object id.
    pub fn oid(&self) -> &str {
        self.mri.strip_prefix("8:orgid:").unwrap()
    }

    /// The account's skypetoken, for a request no client sends.
    pub async fn skypetoken(&self) -> String {
        self.session.skypetoken().await.unwrap().token.expose().to_owned()
    }

    /// The account's `ic3` token, for a check of where it went.
    pub async fn ic3(&self) -> String {
        let token = teams_trouter::TrouterCredentials::ic3(&*self.session).await.unwrap();
        token.token.expose().to_owned()
    }

    fn transport(&self, tracker: &Arc<Tracker>) -> Transport {
        let chats = HttpTransport::for_cloud_with_mock_origin(
            Arc::clone(&self.session),
            &format!("{}/api/chatsvc/own", self.origin),
            Cloud::own(&self.origin).unwrap(),
            &self.http,
        )
        .unwrap()
        .for_account(Mri::new(self.mri.clone()))
        .with_call_state(tracker.clone());
        if !self.served {
            return chats;
        }
        let base = |gtm: &str| {
            self.session
                .service_base(gtm)
                .unwrap_or_else(|| panic!("authz names no {gtm}"))
        };
        let chats = if self.whole {
            chats
                .with_ams(&base(AMS_GTM))
                .unwrap()
                .with_presence(&base(PRESENCE_GTM))
                .unwrap()
        } else {
            chats
        };
        chats
            .with_middle_tier(&base(MIDDLE_TIER_GTM))
            .unwrap()
            .with_csa(&base(CSA_GTM))
            .unwrap()
            .with_provisioning(&base(PROVISIONING_GTM))
            .unwrap()
            .with_user_profile(&base(USER_PROFILE_GTM))
            .unwrap()
    }

    /// A typed client of the account, for a read the tests make themselves.
    pub fn client(&self) -> Client<Transport> {
        let cloud = Cloud::own(&self.origin).unwrap();
        Client::new(self.transport(&Arc::new(Tracker::for_cloud(Mri::new(self.mri.clone()), cloud))))
    }

    fn media(&self) -> CallMedia {
        CallMedia {
            devices: Arc::new(FileDevices {
                input: self.speech.clone().map_or(AudioInput::Tone(self.tone), AudioInput::Wav),
                recording: Some(self.recording.clone()),
                camera: self.camera,
                pacing: Pacing::RealTime,
            }),
            // as a shipped client's: an own-server account's media goes through the relay its server names
            relay: Relay::Teams,
            session: SessionConfig {
                processing: ProcessingConfig::OFF,
                ..Default::default()
            },
            video: Some(CallVideo {
                encoder: EncoderSettings::default(),
                max_size: MaxSize::HD,
                output: Arc::new(self.video.clone()),
            }),
        }
    }

    /// Starts the client on its store: a new engine with new push channels, as a start of ngteams makes them.
    pub fn start(&mut self) {
        assert!(self.engine.is_none());
        let cloud = Cloud::own(&self.origin).unwrap();
        let mri = Mri::new(self.mri.clone());
        let tracker = Arc::new(Tracker::for_cloud(mri.clone(), cloud));
        let transport = Arc::new(self.transport(&tracker));
        let socket = self
            .socket
            .clone()
            .unwrap_or_else(|| self.http.replacen("http://", "ws://", 1));
        let trouter = || {
            Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&self.session), &socket)
                .unwrap()
                .with_options(options())
        };
        let floor = CallFloor::new();
        let (observed, on_floor) = (Arc::clone(&tracker), floor.clone());
        let stack =
            CallStack::new(Arc::clone(&tracker)).observing(move |delivery| on_floor.observe(&observed, delivery));
        let identity = Identity {
            mri: mri.clone(),
            display_name: Some(self.name.clone()),
            endpoint_id: String::new(),
            language_id: Some(CALL_LANGUAGE.to_owned()),
            tenant_id: String::new(),
            client: web_client_info(),
            region: self.session.region().name,
            partition: self.session.region().partition,
        };
        let calls = CallParts::new(trouter().for_calls(stack), identity, self.media());
        let outbox =
            Outbox::new(Arc::clone(&transport), Author::new(mri, self.name.clone())).with_calls(tracker, floor);
        let handle = engine::start(Parts {
            calls: Some(calls),
            store: Store::open(&self.dir.path().join("account.sqlite")).unwrap(),
            client: Arc::new(Client::new(self.transport(&Arc::new(Tracker::for_cloud(
                Mri::new(self.mri.clone()),
                Cloud::own(&self.origin).unwrap(),
            ))))),
            outbox,
            push: Some(Push::new(trouter())),
            options: EngineOptions {
                tick: Duration::from_millis(50),
                catch_up_retry: self.catch_up_retry,
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })
        .unwrap();
        self.engine = Some(handle);
    }

    /// Closes the client, as a quit does it.
    pub async fn close(&mut self) {
        if let Some(engine) = self.engine.take() {
            tokio::task::spawn_blocking(move || engine.shutdown()).await.unwrap();
        }
    }

    /// Quits the client and starts it again on its store: connected, registered and caught up.
    pub async fn restart(&mut self) {
        self.close().await;
        let mark = self.mark();
        self.start();
        self.online(mark).await;
        self.since(mark, "catch-up", |event| matches!(event, Event::CaughtUp { .. }))
            .await;
    }

    pub fn engine(&self) -> &EngineHandle {
        self.engine.as_ref().unwrap()
    }

    /// Where the log stands: events from here on are the ones after this moment.
    pub fn mark(&self) -> usize {
        self.log.len()
    }

    /// Takes the client's next event into its log, waiting up to `wait`: whether one came.
    pub async fn pump(&mut self, wait: Duration) -> bool {
        let engine = self.engine.as_mut().unwrap();
        match tokio::time::timeout(wait, engine.next_event()).await {
            Ok(Some(event)) => {
                self.log.push(event);
                true
            }
            Ok(None) | Err(_) => false,
        }
    }

    fn briefly(&self, mark: usize) -> Vec<String> {
        self.log[mark..].iter().map(brief).collect()
    }

    /// Waits for an event since `mark` that `pick` takes; its place in the log.
    pub async fn since(&mut self, mark: usize, what: &str, pick: impl Fn(&Event) -> bool) -> usize {
        let deadline = tokio::time::Instant::now() + WAIT;
        let mut seen = mark;
        loop {
            if let Some(found) = self.log[seen..].iter().position(&pick) {
                return seen + found;
            }
            seen = self.log.len();
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(
                !left.is_zero() && self.pump(left).await,
                "{}: no {what} in time; since the mark: {:?}",
                self.upn,
                self.briefly(mark)
            );
        }
    }

    /// Whether nothing `unwanted` comes for `quiet`, counting what came since `mark`.
    pub async fn quiet(&mut self, mark: usize, quiet: Duration, unwanted: impl Fn(&Event) -> bool) -> bool {
        let until = tokio::time::Instant::now() + quiet;
        loop {
            let left = until.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() || !self.pump(left).await {
                break;
            }
        }
        !self.log[mark..].iter().any(unwanted)
    }

    /// Both push channels connected and registered since `mark`: messages and invitations come from here on.
    pub async fn online(&mut self, mark: usize) {
        self.since(mark, "messaging registration", |event| {
            matches!(event, Event::Registration(RegistrationReport::Registered))
        })
        .await;
        self.since(mark, "calling registration", |event| {
            matches!(event, Event::CallingRegistration(RegistrationReport::Registered))
        })
        .await;
    }

    /// One press of a call control: the call it is about, or why the client refused it.
    pub async fn press(&self, action: CallAction) -> Result<CallId, String> {
        self.engine()
            .requests()
            .call(action)
            .await
            .map_err(|error| error.to_string())
    }

    /// Presses `action`, again while the call can't take it yet (a renegotiation still settling), as a user's second
    /// press would.
    pub async fn act(&self, action: CallAction) -> CallId {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            match self.press(action.clone()).await {
                Ok(call) => return call,
                Err(error) if tokio::time::Instant::now() < deadline => {
                    eprintln!("{}: {action:?} not taken yet: {error}", self.upn);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(error) => panic!("{}: {action:?}: {error}", self.upn),
            }
        }
    }

    /// Waits until `call` is `wanted`; fails when it ends first.
    pub async fn status(&mut self, mark: usize, call: CallId, wanted: &CallStatus) {
        let at = self
            .since(mark, &format!("{wanted:?}"), |event| {
                matches!(event, Event::Call { call: of, status } if *of == call
                    && (status == wanted || matches!(status, CallStatus::Ended(_))))
            })
            .await;
        let Event::Call { status, .. } = &self.log[at] else {
            unreachable!()
        };
        assert_eq!(status, wanted, "{}: call {} ended first", self.upn, call.get());
    }

    /// How `call` ended.
    pub async fn ended(&mut self, mark: usize, call: CallId) -> End {
        let at = self
            .since(
                mark,
                "the call's end",
                |event| matches!(event, Event::Call { call: of, status: CallStatus::Ended(_) } if *of == call),
            )
            .await;
        match &self.log[at] {
            Event::Call {
                status: CallStatus::Ended(end),
                ..
            } => end.clone(),
            _ => unreachable!(),
        }
    }

    /// The invitation that rings here since `mark`.
    pub async fn incoming(&mut self, mark: usize) -> IncomingCall {
        let at = self
            .since(mark, "an incoming call", |event| {
                matches!(event, Event::IncomingCall(_))
            })
            .await;
        match &self.log[at] {
            Event::IncomingCall(incoming) => incoming.clone(),
            _ => unreachable!(),
        }
    }

    /// How the ring of `call` ended here without the account's action.
    pub async fn ring_ended(&mut self, mark: usize, call: CallId) -> RemoteEnd {
        let at = self
            .since(
                mark,
                "the ring's end",
                |event| matches!(event, Event::RingEnded { call: of, .. } if *of == call),
            )
            .await;
        match &self.log[at] {
            Event::RingEnded { end, .. } => end.clone(),
            _ => unreachable!(),
        }
    }

    /// The call's people once `wanted` holds of them.
    pub async fn view(&mut self, mark: usize, call: CallId, what: &str, wanted: impl Fn(&CallView) -> bool) {
        self.since(
            mark,
            what,
            |event| matches!(event, Event::CallParticipants { call: of, view } if *of == call && wanted(view)),
        )
        .await;
    }

    /// The first word since `mark` of whether the far end holds `call`.
    pub async fn held_by_far_end(&mut self, mark: usize, call: CallId) -> bool {
        let at = self
            .since(
                mark,
                "the far end's hold",
                |event| matches!(event, Event::CallHeldByFarEnd { call: of, .. } if *of == call),
            )
            .await;
        matches!(&self.log[at], Event::CallHeldByFarEnd { held: true, .. })
    }

    /// The 1:1 chat with `other`, made by this account; the other's client has it once it synced.
    pub async fn chat_with(&self, other_mri: &str) -> String {
        let create = Action::CreateChat {
            others: vec![Mri::new(other_mri)],
            topic: None,
        };
        let row = self.engine().write(create).await.unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        row.message_id.unwrap()
    }

    pub async fn sync(&self) {
        let report = self.engine().sync().await.unwrap();
        assert!(report.failed.is_empty(), "{report:?}");
    }

    /// The chat's call lines as the client's store holds them, the oldest first.
    pub async fn stored_lines(&self, chat: &str) -> Vec<Line> {
        let page = self.engine().messages(chat, None, 200).await.unwrap();
        let mut lines: Vec<Line> = page
            .messages
            .iter()
            .filter_map(|message| Line::read(&message.message_type, &message.content))
            .collect();
        lines.reverse();
        lines
    }

    /// Waits until the push channel brought a call line of `chat` that `pick` takes, since `mark`.
    pub async fn pushed_line(&mut self, mark: usize, chat: &str, what: &str, pick: impl Fn(&Line) -> bool) {
        self.since(mark, what, |event| match event {
            Event::Message {
                conversation,
                message: Some(message),
                source: Source::Delivery | Source::Replay,
                ..
            } if conversation == chat => {
                Line::read(&message.message_type, &message.content).is_some_and(|line| pick(&line))
            }
            _ => false,
        })
        .await;
    }

    /// Waits until the client's store holds exactly these call lines of `chat`.
    pub async fn shows_lines(&mut self, chat: &str, wanted: &[&dyn Fn(&Line) -> bool]) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let lines = self.stored_lines(chat).await;
            if lines.len() == wanted.len() && lines.iter().zip(wanted).all(|(line, wanted)| wanted(line)) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{}: the chat's call lines are {lines:?}",
                self.upn
            );
            self.pump(Duration::from_millis(100)).await;
        }
    }

    /// The level of `tone` in each half second of what this client's speaker played, from the start.
    pub fn levels(&self, tone: Tone) -> Vec<f32> {
        let samples = wav::read(&self.recording).unwrap();
        samples.chunks(24_000).map(|half| level_at(half, tone.hz)).collect()
    }

    /// The level of `tone` in the last `stretch` of what this client's speaker played, leaving out its last 200 ms.
    pub fn heard(&self, tone: Tone, stretch: Duration) -> f32 {
        let samples = wav::read(&self.recording).unwrap();
        let at = |span: Duration| usize::try_from(span.as_millis()).unwrap() * 48;
        let end = samples.len().saturating_sub(at(Duration::from_millis(200)));
        let start = end.saturating_sub(at(stretch));
        assert!(end - start >= 24_000, "{}: {} samples played", self.upn, samples.len());
        level_at(&samples[start..end], tone.hz)
    }
}

impl Drop for Phone {
    fn drop(&mut self) {
        if let Some(engine) = self.engine.take() {
            std::thread::spawn(move || engine.shutdown());
        }
    }
}

/// An event's kind and what it is about, without its payload.
pub fn brief(event: &Event) -> String {
    match event {
        Event::Call { call, status } => format!("Call({} {status:?})", call.get()),
        Event::IncomingCall(incoming) => format!("IncomingCall({})", incoming.call.get()),
        Event::RingEnded { call, end } => format!("RingEnded({} {:?} {:?})", call.get(), end.code, end.phrase),
        Event::CallHeldByFarEnd { held, .. } => format!("CallHeldByFarEnd({held})"),
        Event::CallHeld { held, .. } => format!("CallHeld({held})"),
        Event::Failed { during, error } => format!("Failed({during}: {error})"),
        Event::Message { id, change, source, .. } => format!("Message({id} {change:?} {source:?})"),
        Event::Registration(report) => format!("Registration({report:?})"),
        Event::CallingRegistration(report) => format!("CallingRegistration({report:?})"),
        other => {
            let text = format!("{other:?}");
            text.split(['(', '{', ' ']).next().unwrap_or_default().to_owned()
        }
    }
}

pub async fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !done() {
        assert!(tokio::time::Instant::now() < deadline, "{what} didn't happen in time");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A calling request as `token`'s account, one no client sends; its status and JSON.
pub async fn raw(node: &Node, token: &str, method: reqwest::Method, path: &str, body: &Value) -> (u16, Value) {
    let response = http_client()
        .request(method, format!("{}{path}", node.endpoint))
        .header("x-skypetoken", token)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

/// What a TURN relay said to an allocation asked with a credential.
#[derive(Debug, PartialEq, Eq)]
pub enum Allocation {
    /// It relays for the credential.
    Made,
    /// It refused, with this code.
    Refused(u16),
}

fn exchange(socket: &UdpSocket, request: &Message, key: Option<&IntegrityKey>) -> Message {
    let mut answer = [0_u8; 1500];
    for _ in 0..5 {
        socket.send(&request.encode(key, true)).unwrap();
        if let Ok(read) = socket.recv(&mut answer) {
            let message = Message::decode(&answer[..read]).unwrap();
            if message.transaction_id == request.transaction_id {
                return message;
            }
        }
    }
    panic!("the relay answered nothing");
}

/// Asks the TURN relay at `relay` for an allocation as `username` with `password`, as a client's ICE agent does:
/// unauthenticated first, then with the realm and nonce the relay named.
pub fn allocate(relay: SocketAddr, username: &str, password: &str) -> Allocation {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.connect(relay).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let ask = || {
        Message::new(Class::Request, Method::Allocate, TransactionId::random().unwrap())
            .with(Attribute::RequestedTransport(TRANSPORT_UDP))
    };
    let challenge = exchange(&socket, &ask(), None);
    assert_eq!(challenge.error_code().map(|(code, _)| code), Some(401), "{challenge:?}");
    let realm = challenge
        .find(|attribute| match attribute {
            Attribute::Realm(realm) => Some(realm.clone()),
            _ => None,
        })
        .unwrap();
    let nonce = challenge
        .find(|attribute| match attribute {
            Attribute::Nonce(nonce) => Some(nonce.clone()),
            _ => None,
        })
        .unwrap();
    let request = ask()
        .with(Attribute::Username(username.to_owned()))
        .with(Attribute::Realm(realm.clone()))
        .with(Attribute::Nonce(nonce));
    let key = IntegrityKey::long_term(username, &realm, password);
    let answer = exchange(&socket, &request, Some(&key));
    match answer.error_code() {
        None if answer.class == Class::Success => Allocation::Made,
        Some((code, _)) => Allocation::Refused(code),
        None => panic!("{answer:?}"),
    }
}
