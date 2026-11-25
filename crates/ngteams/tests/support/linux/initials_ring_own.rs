//! The `own` case: an own-server account on the real `ngteams-server` binary, with synthetic users of odd names.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{fs, thread};

use ngteams::accounts::Started;
use ngteams::accounts_file::PersonalAccount;
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore as _};
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, Scope, TokenClient};
use teams_core::account::{AccountEngine, CALL_LANGUAGE, web_client_info};
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Event, Parts, Push};
use teams_core::{CallAction, CallFloor, CallId, CallMedia, CallParts, CallStatus, Identity, Outbox, Relay};
use teams_media::devices::Devices;
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::processing::ProcessingConfig;
use teams_media::session::SessionConfig;
use teams_media::tone::Tone;
use teams_proto::chat::MessageText;
use teams_proto::{Author, Client as ChatClient, Cloud, HttpTransport, Mri};
use teams_store::{OutboxState, Store};
use teams_trouter::{CallStack, Options, RegistrationReport, Trouter};
use teams_ui::VideoFeed;
use ui_sandbox::SandboxResult;

use super::{AppUnderTest, ODD, Rig, SETTLE, THEMES, env, say};

const ORIGIN_ENV: &str = "NGTEAMS_INITIALS_RING_OWN_ORIGIN";
const ENDPOINT_ENV: &str = "NGTEAMS_INITIALS_RING_OWN_ENDPOINT";
const REFRESH_ENV: &str = "NGTEAMS_INITIALS_RING_OWN_REFRESH";
const COTURN_ENV: &str = "NGTS_TEST_COTURN";
const COTURN_SECRET_ENV: &str = "NGTS_TEST_COTURN_SECRET";
const WAIT: Duration = Duration::from_secs(45);
/// The client id the native client signs in as.
const CLIENT_ID: &str = "1fec8e78-bce4-4aaf-ab1b-5451cc387264";
/// Every synthetic account's password.
const PASSWORD: &str = "correct horse battery";
/// a (the app), b (the caller), c and d (they answer the group call): synthetic.
const UPN: [&str; 4] = ["ada@own.test", "cora@own.test", "wang@own.test", "ann@own.test"];
const NAMES: [&str; 4] = ["Ada Ł. Lovelace", "Cora 'Cee' Nuñez", "王小明", "Ann Lee (Guest)"];
const TOPIC: &str = "Q4 ✨ «planning» & co";

pub(super) fn parent() -> SandboxResult {
    let (Ok(relay), Ok(secret)) = (std::env::var(COTURN_ENV), std::env::var(COTURN_SECRET_ENV)) else {
        say("skipped: no coturn named (NGTS_TEST_COTURN, NGTS_TEST_COTURN_SECRET)");
        return Ok(());
    };
    let Some(binary) = server_binary()? else {
        say("skipped: no ngteams-server beside the target directory (cargo build -p ngteams-server)");
        return Ok(());
    };
    let Some(sandbox) = super::sandbox("ngteams initials ring (own server)")? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    let logs = super::evidence_dirs().pop().ok_or("no evidence dir")?;
    fs::create_dir_all(&logs)?;
    let mut server = Server::new(binary, &relay, &secret, &logs)?;
    server.cli(&["migrate"], "")?;
    let users = Users::add(&server)?;
    runtime.block_on(server.serve())?;
    let outcome = runtime.block_on(World::make(&server, &users)).and_then(|mut world| {
        let dir = tempfile::tempdir()?;
        let extra = [
            (ORIGIN_ENV, server.origin.as_str()),
            (ENDPOINT_ENV, server.endpoint.as_str()),
            (REFRESH_ENV, world.a_refresh.as_str()),
        ];
        let waits = [("calling", "registered")];
        let mut app = AppUnderTest::start(&sandbox, ("own", &extra), dir.path(), &world.topic_group, &waits)?;
        thread::sleep(Duration::from_secs(3));
        let outcome = Rig::new(&sandbox, &mut app, "own").and_then(|mut rig| scenario(&mut rig, &runtime, &mut world));
        let outcome = outcome.map_err(|error| {
            format!(
                "{error}\n--- the app: {}\n--- the sandbox{}",
                app.running.fail(&"killed"),
                sandbox.logs()
            )
        });
        outcome?;
        app.quit(&sandbox)
    });
    server.stop()?;
    outcome?;
    say("own case: ok");
    Ok(())
}

/// Everything the user does, in order.
fn scenario(rig: &mut Rig<'_>, runtime: &tokio::runtime::Runtime, world: &mut World) -> SandboxResult {
    let (card_mri, card_name) = (world.odd[0].account.mri.clone(), ODD[0].0);
    let first_one_to_one = world.odd_chats[0].clone();
    let chats = [
        ("plain-group", world.plain_group.as_str()),
        ("one-to-one-odd", first_one_to_one.as_str()),
        ("one-to-one-b", world.b_chat.as_str()),
        ("topic-group", world.topic_group.as_str()),
    ];
    rig.views(&chats, (&card_mri, card_name))?;
    // b rings a in their 1:1 chat while the group chat with no topic shows; a declines
    for theme in THEMES {
        rig.theme(theme)?;
        rig.step(&format!("chat {}", world.plain_group))?;
        let mark = world.b.log.len();
        let chat = world.b_chat.clone();
        let call = rig.ring_burst(&format!("one-to-one-ringing-{theme}"), || {
            runtime.block_on(world.b.act(CallAction::Call { chat }))
        })?;
        rig.decline()?;
        runtime.block_on(world.b.end(mark, call))?;
    }
    group_rings(rig, runtime, world)
}

/// b rings the group chat with a topic while it shows: a declines the first ring, b ends it; a, c and d answer the
/// second, its window shot.
fn group_rings(rig: &mut Rig<'_>, runtime: &tokio::runtime::Runtime, world: &mut World) -> SandboxResult {
    let group = world.topic_group.clone();
    rig.theme(THEMES[0])?;
    rig.step(&format!("chat {group}"))?;
    let marks = (world.b.log.len(), world.c.log.len(), world.d.log.len());
    let chat = group.clone();
    let call = rig.ring_burst("group-ringing-light", || {
        runtime.block_on(world.b.act(CallAction::Call { chat }))
    })?;
    rig.decline()?;
    runtime.block_on(world.b.act(CallAction::HangUp { call }))?;
    runtime.block_on(world.b.end(marks.0, call))?;
    runtime.block_on(world.c.rang(marks.1))?;
    runtime.block_on(world.d.rang(marks.2))?;
    rig.theme(THEMES[1])?;
    let marks = (world.c.log.len(), world.d.log.len());
    let chat = group.clone();
    let call = rig.ring_burst("group-ringing-dark", || {
        runtime.block_on(world.b.act(CallAction::Call { chat }))
    })?;
    rig.answer()?;
    let mut answered = Vec::new();
    for (peer, mark) in [(&mut world.c, marks.0), (&mut world.d, marks.1)] {
        let rung = runtime.block_on(peer.incoming(mark))?;
        let at = peer.log.len();
        let joined = runtime.block_on(peer.act(CallAction::Answer { call: rung }))?;
        runtime.block_on(peer.connected(at, joined))?;
        answered.push(joined);
    }
    thread::sleep(SETTLE * 3);
    rig.call_window("group-call")?;
    rig.hang_up()?;
    let peers = [
        (&mut world.c, answered[0]),
        (&mut world.d, answered[1]),
        (&mut world.b, call),
    ];
    for (peer, call) in peers {
        let mark = peer.log.len();
        runtime.block_on(peer.act(CallAction::HangUp { call }))?;
        runtime.block_on(peer.end(mark, call))?;
    }
    Ok(())
}

// --- the server ---

/// The real server binary on loopback: `serve` as a process of its own, SQLite, the run's coturn.
struct Server {
    binary: PathBuf,
    config: PathBuf,
    /// `http://127.0.0.1:<port>`: where the clients' requests go.
    endpoint: String,
    /// `https://127.0.0.1:<port>`: the deployment's public origin, as the accounts name it.
    origin: String,
    log: PathBuf,
    serving: Option<Child>,
    _dir: tempfile::TempDir,
}

/// The server binary beside this test's target directory (`cargo build -p ngteams-server` leaves it there).
fn server_binary() -> SandboxResult<Option<PathBuf>> {
    let exe = std::env::current_exe()?;
    let beside = exe
        .parent()
        .and_then(Path::parent)
        .map(|profile| profile.join("ngteams-server"));
    Ok(beside.filter(|path| path.is_file()))
}

fn free_port() -> SandboxResult<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn http() -> SandboxResult<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(12))
        .build()?)
}

impl Server {
    fn new(binary: PathBuf, relay: &str, secret: &str, logs: &Path) -> SandboxResult<Self> {
        let dir = tempfile::tempdir()?;
        let port = free_port()?;
        let config = dir.path().join("server.toml");
        let origin = format!("https://127.0.0.1:{port}");
        fs::write(
            &config,
            format!(
                "listen = \"127.0.0.1:{port}\"\npublic_origin = \"{origin}\"\ndatabase_url = \"sqlite://{}\"\n\
                 [calling.turn]\nsecret = {secret:?}\nurls = [\"turn:{relay}?transport=udp\"]\n\
                 [mixer]\nudp_listen = \"127.0.0.1:0\"\n",
                dir.path().join("server.sqlite").display()
            ),
        )?;
        Ok(Self {
            binary,
            config,
            endpoint: format!("http://127.0.0.1:{port}"),
            origin,
            log: logs.join("initials-ring-own-server.log"),
            serving: None,
            _dir: dir,
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .arg("--config")
            .arg(&self.config)
            .env_remove("NGTEAMS_SERVER_ROLE")
            .env("NGTEAMS_SERVER_LOG", "info");
        command
    }

    /// A command line run; its standard output.
    fn cli(&self, args: &[&str], input: &str) -> SandboxResult<String> {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child.stdin.take().ok_or("no stdin")?.write_all(input.as_bytes())?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("{args:?} failed: {stderr}").into());
        }
        Ok(String::from_utf8(output.stdout)?)
    }

    /// `user add`: the account's MRI.
    fn add_user(&self, upn: &str, name: &str) -> SandboxResult<String> {
        let line = self.cli(&["user", "add", upn, "--name", name], &format!("{PASSWORD}\n"))?;
        let id = line
            .split_once('(')
            .and_then(|(_, rest)| rest.split_once(')'))
            .map(|(id, _)| id.to_owned())
            .ok_or_else(|| format!("no id in {line:?}"))?;
        Ok(format!("8:orgid:{id}"))
    }

    /// `serve`, until it answers ready.
    async fn serve(&mut self) -> SandboxResult {
        let log = fs::OpenOptions::new().create(true).append(true).open(&self.log)?;
        let mut command = self.command();
        command.arg("serve");
        self.serving = Some(command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(log).spawn()?);
        let client = http()?;
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            let ready = client
                .get(format!("{}/health/ready", self.endpoint))
                .send()
                .await
                .is_ok_and(|answer| answer.status() == 204);
            if ready {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err("the server never answered ready".into())
    }

    fn stop(&mut self) -> SandboxResult {
        if let Some(mut serving) = self.serving.take() {
            let status = Command::new("kill")
                .args(["-TERM", &serving.id().to_string()])
                .status()?;
            if !status.success() {
                serving.kill()?;
            }
            serving.wait()?;
        }
        Ok(())
    }

    /// The account's refresh token from the password grant, as the sign-in screen's first step gets it.
    async fn refresh_token(&self, upn: &str) -> SandboxResult<String> {
        let form: String = [
            ("client_id", CLIENT_ID),
            ("grant_type", "password"),
            ("username", upn),
            ("password", PASSWORD),
            ("scope", Scope::spaces().as_str()),
        ]
        .iter()
        .map(|(key, value)| format!("{key}={}", encoded(value)))
        .collect::<Vec<_>>()
        .join("&");
        let body = http()?
            .post(format!("{}/oauth2/v2.0/token", self.endpoint))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(form)
            .send()
            .await?
            .bytes()
            .await?;
        let answer: serde_json::Value = serde_json::from_slice(&body)?;
        Ok(answer["refresh_token"].as_str().ok_or("no refresh token")?.to_owned())
    }
}

/// `value` as a form's value: every byte but the unreserved ones percent-encoded.
fn encoded(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(mut serving) = self.serving.take() {
            let _ = serving.kill();
            let _ = serving.wait();
        }
    }
}

/// The synthetic users' UPNs, by role.
struct Users {
    /// b, c and d.
    callers: Vec<String>,
    /// The people of [`ODD`].
    odd: Vec<String>,
}

impl Users {
    fn add(server: &Server) -> SandboxResult<Self> {
        let mut callers = Vec::new();
        for (upn, name) in UPN.iter().zip(NAMES) {
            server.add_user(upn, name)?;
            callers.push((*upn).to_owned());
        }
        callers.remove(0);
        let mut odd = Vec::new();
        for (index, (name, _)) in ODD.iter().enumerate() {
            let upn = format!("odd.{index}@own.test");
            server.add_user(&upn, name)?;
            odd.push(upn);
        }
        match server.add_user("blank@own.test", "") {
            Ok(_) => odd.push("blank@own.test".to_owned()),
            Err(error) => say(&format!("an empty name refused: {error}")),
        }
        Ok(Self { callers, odd })
    }
}

// --- the accounts' clients ---

type OwnTransport = HttpTransport<Session<MemorySecretStore>>;

/// A signed-in own-server account: its session on its refresh token, its MRI.
struct Account {
    upn: String,
    mri: String,
    session: Arc<Session<MemorySecretStore>>,
    origin: String,
    endpoint: String,
}

impl Account {
    async fn sign_in(upn: &str, refresh: &str, origin: &str, endpoint: &str) -> SandboxResult<Self> {
        let store = MemorySecretStore::new();
        store.put(&refresh_token_key(upn), &Secret::new(refresh)).await?;
        let session = Session::new(
            upn,
            store,
            TokenClient::new(EntraConfig::teams_native().with_authority(endpoint))?,
            AuthzClient::with_endpoint(&format!("{endpoint}/api/authsvc/v1.0/authz"))?,
            Region::default(),
        );
        let skype = session.skypetoken().await?;
        let id = skype.skype_id.ok_or("no skype id")?;
        Ok(Self {
            upn: upn.to_owned(),
            mri: format!("8:{id}"),
            session: Arc::new(session),
            origin: origin.to_owned(),
            endpoint: endpoint.to_owned(),
        })
    }

    async fn of(server: &Server, upn: &str) -> SandboxResult<Self> {
        let refresh = server.refresh_token(upn).await?;
        Self::sign_in(upn, &refresh, &server.origin, &server.endpoint).await
    }

    fn transport(&self, tracker: &Arc<teams_call::Tracker>) -> SandboxResult<OwnTransport> {
        Ok(HttpTransport::for_cloud_with_mock_origin(
            Arc::clone(&self.session),
            &format!("{}/api/chatsvc/own", self.origin),
            Cloud::own(&self.origin)?,
            &self.endpoint,
        )?
        .for_account(Mri::new(self.mri.clone()))
        .with_call_state(tracker.clone()))
    }

    fn tracker(&self) -> SandboxResult<Arc<teams_call::Tracker>> {
        Ok(Arc::new(teams_call::Tracker::for_cloud(
            Mri::new(self.mri.clone()),
            Cloud::own(&self.origin)?,
        )))
    }

    /// The account's engine on its store in `dir`, writes only: no push channel, no calls.
    fn writer(&self, name: &str, dir: &Path) -> SandboxResult<EngineHandle> {
        let tracker = self.tracker()?;
        let transport = Arc::new(self.transport(&tracker)?);
        fs::create_dir_all(dir)?;
        Ok(engine::start(Parts {
            calls: None,
            store: Store::open(&dir.join("account.sqlite"))?,
            client: Arc::new(ChatClient::new(self.transport(&self.tracker()?)?)),
            outbox: Outbox::new(transport, Author::new(Mri::new(self.mri.clone()), name)),
            push: None,
            options: EngineOptions::default(),
            known_names: Vec::new(),
        })?)
    }

    /// The account's engine on its store in `dir`: its push channels, its calls' media on `devices` through the
    /// relay its server names.
    fn engine(
        &self,
        name: &str,
        devices: Arc<dyn Devices + Send + Sync>,
        video: Option<teams_core::CallVideo>,
        dir: &Path,
    ) -> SandboxResult<EngineHandle> {
        let mri = Mri::new(self.mri.clone());
        let tracker = Arc::new(teams_call::Tracker::for_cloud(mri.clone(), Cloud::own(&self.origin)?));
        let transport = Arc::new(self.transport(&tracker)?);
        let socket = self.endpoint.replacen("http://", "ws://", 1);
        let options = Options {
            backoff_base: Duration::from_millis(300),
            backoff_cap: Duration::from_secs(2),
            backoff_jitter_percent: 0,
            ..Options::default()
        };
        let trouter = || -> SandboxResult<_> {
            Ok(
                Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&self.session), &socket)?
                    .with_options(options),
            )
        };
        let floor = CallFloor::new();
        let (observed, on_floor) = (Arc::clone(&tracker), floor.clone());
        let stack =
            CallStack::new(Arc::clone(&tracker)).observing(move |delivery| on_floor.observe(&observed, delivery));
        let media = CallMedia {
            devices,
            relay: Relay::Teams,
            session: SessionConfig {
                processing: ProcessingConfig::OFF,
                ..Default::default()
            },
            video,
        };
        let calls = CallParts::new(trouter()?.for_calls(stack), self.identity(name), media);
        let outbox = Outbox::new(Arc::clone(&transport), Author::new(mri, name)).with_calls(tracker, floor);
        fs::create_dir_all(dir)?;
        Ok(engine::start(Parts {
            calls: Some(calls),
            store: Store::open(&dir.join("account.sqlite"))?,
            client: Arc::new(ChatClient::new(self.transport(&self.tracker()?)?)),
            outbox,
            push: Some(Push::new(trouter()?)),
            options: EngineOptions {
                tick: Duration::from_millis(50),
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })?)
    }

    fn identity(&self, name: &str) -> Identity {
        Identity {
            mri: Mri::new(self.mri.clone()),
            display_name: Some(name.to_owned()),
            endpoint_id: String::new(),
            language_id: Some(CALL_LANGUAGE.to_owned()),
            tenant_id: String::new(),
            client: web_client_info(),
            region: self.session.region().name,
            partition: self.session.region().partition,
        }
    }
}

/// A client the parent runs for someone: its engine, its events so far.
struct Peer {
    account: Account,
    engine: EngineHandle,
    log: Vec<Event>,
    _dir: tempfile::TempDir,
}

impl Peer {
    /// A client with calls, a tone for its microphone, online.
    async fn calling(account: Account, name: &str, hz: f32) -> SandboxResult<Self> {
        let dir = tempfile::tempdir()?;
        let devices: Arc<dyn Devices + Send + Sync> = Arc::new(FileDevices {
            input: AudioInput::Tone(Tone { hz, amplitude: 0.5 }),
            recording: None,
            camera: PatternCamera::default(),
            pacing: Pacing::RealTime,
        });
        let engine = account.engine(name, devices, None, dir.path())?;
        let mut peer = Self {
            account,
            engine,
            log: Vec::new(),
            _dir: dir,
        };
        peer.online().await?;
        Ok(peer)
    }

    /// A client that only writes.
    fn writing(account: Account, name: &str) -> SandboxResult<Self> {
        let dir = tempfile::tempdir()?;
        let engine = account.writer(name, dir.path())?;
        Ok(Self {
            account,
            engine,
            log: Vec::new(),
            _dir: dir,
        })
    }

    /// Waits for an event since `mark` that `pick` takes; its place in the log.
    async fn since(&mut self, mark: usize, what: &str, pick: impl Fn(&Event) -> bool) -> SandboxResult<usize> {
        let deadline = tokio::time::Instant::now() + WAIT;
        let mut seen = mark;
        loop {
            if let Some(found) = self.log.get(seen..).and_then(|events| events.iter().position(&pick)) {
                return Ok(seen + found);
            }
            seen = self.log.len();
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(left, self.engine.next_event()).await {
                Ok(Some(event)) => self.log.push(event),
                Ok(None) | Err(_) => return Err(format!("{}: no {what} in time", self.account.upn).into()),
            }
        }
    }

    async fn online(&mut self) -> SandboxResult {
        self.since(0, "messaging registration", |event| {
            matches!(event, Event::Registration(RegistrationReport::Registered))
        })
        .await?;
        self.since(0, "calling registration", |event| {
            matches!(event, Event::CallingRegistration(RegistrationReport::Registered))
        })
        .await?;
        Ok(())
    }

    /// Presses `action`, again while the call can't take it yet.
    async fn act(&self, action: CallAction) -> SandboxResult<CallId> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            match self.engine.requests().call(action.clone()).await {
                Ok(call) => return Ok(call),
                Err(error) if tokio::time::Instant::now() < deadline => {
                    eprintln!("{}: {action:?} not taken yet: {error}", self.account.upn);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(error) => return Err(format!("{}: {action:?}: {error}", self.account.upn).into()),
            }
        }
    }

    async fn connected(&mut self, mark: usize, call: CallId) -> SandboxResult {
        let at = self
            .since(mark, "connected", |event| {
                matches!(event, Event::Call { call: of, status: CallStatus::Connected | CallStatus::Ended(_) }
                    if *of == call)
            })
            .await?;
        match self.log.get(at) {
            Some(Event::Call {
                status: CallStatus::Connected,
                ..
            }) => Ok(()),
            other => Err(format!("{}: the call ended: {other:?}", self.account.upn).into()),
        }
    }

    /// Waits until `call` ended (since `mark`).
    async fn end(&mut self, mark: usize, call: CallId) -> SandboxResult {
        self.since(
            mark,
            "the call's end",
            |event| matches!(event, Event::Call { call: of, status: CallStatus::Ended(_) } if *of == call),
        )
        .await?;
        Ok(())
    }

    async fn incoming(&mut self, mark: usize) -> SandboxResult<CallId> {
        let at = self
            .since(mark, "an incoming call", |event| {
                matches!(event, Event::IncomingCall(_))
            })
            .await?;
        match self.log.get(at) {
            Some(Event::IncomingCall(incoming)) => Ok(incoming.call),
            _ => Err("no incoming call".into()),
        }
    }

    /// Waits until a ring since `mark` came and ended, unanswered.
    async fn rang(&mut self, mark: usize) -> SandboxResult {
        let at = self
            .since(mark, "an incoming call", |event| {
                matches!(event, Event::IncomingCall(_))
            })
            .await?;
        self.since(at, "the ring's end", |event| matches!(event, Event::RingEnded { .. }))
            .await?;
        Ok(())
    }

    async fn write(&self, action: Action) -> SandboxResult<String> {
        let row = self.engine.write(action).await?;
        if row.state != OutboxState::Sent {
            return Err(format!("{}: not written: {row:?}", self.account.upn).into());
        }
        Ok(row.message_id.unwrap_or_default())
    }

    async fn create(&self, others: &[&str], topic: Option<&str>) -> SandboxResult<String> {
        self.write(Action::CreateChat {
            others: others.iter().map(|mri| Mri::new(*mri)).collect(),
            topic: topic.map(str::to_owned),
        })
        .await
    }

    async fn send(&self, chat: &str, text: &str) -> SandboxResult {
        self.write(Action::Send {
            conversation: chat.to_owned(),
            text: MessageText::plain(text)?,
        })
        .await?;
        Ok(())
    }
}

/// The people around a and their chats.
struct World {
    a_mri: String,
    a_refresh: String,
    b: Peer,
    c: Peer,
    d: Peer,
    odd: Vec<Peer>,
    /// b's group chat with a, c, d and the odd people, [`TOPIC`] its topic.
    topic_group: String,
    /// c's group chat with a, b and the first four odd people, no topic.
    plain_group: String,
    /// b's 1:1 chat with a.
    b_chat: String,
    /// Each odd person's 1:1 chat with a.
    odd_chats: Vec<String>,
}

impl World {
    async fn make(server: &Server, users: &Users) -> SandboxResult<Self> {
        let a = Account::of(server, UPN[0]).await?;
        let mut callers = Vec::new();
        for ((upn, name), hz) in users.callers.iter().zip(&NAMES[1..]).zip([660.0, 880.0, 990.0]) {
            callers.push(Peer::calling(Account::of(server, upn).await?, name, hz).await?);
        }
        let mut odd = Vec::new();
        for (index, upn) in users.odd.iter().enumerate() {
            let name = ODD.get(index).map_or("", |(name, _)| *name);
            odd.push(Peer::writing(Account::of(server, upn).await?, name)?);
        }
        let d = callers.pop().ok_or("no d")?;
        let c = callers.pop().ok_or("no c")?;
        let b = callers.pop().ok_or("no b")?;
        let mut world = Self {
            a_mri: a.mri.clone(),
            a_refresh: server.refresh_token(UPN[0]).await?,
            b,
            c,
            d,
            odd,
            topic_group: String::new(),
            plain_group: String::new(),
            b_chat: String::new(),
            odd_chats: Vec::new(),
        };
        world.chats().await?;
        Ok(world)
    }

    /// The chats, a message from each person in each.
    async fn chats(&mut self) -> SandboxResult {
        let a = self.a_mri.clone();
        let odd: Vec<String> = self.odd.iter().map(|peer| peer.account.mri.clone()).collect();
        let mut members = vec![a.as_str(), &self.c.account.mri, &self.d.account.mri];
        members.extend(odd.iter().map(String::as_str));
        self.topic_group = self.b.create(&members, Some(TOPIC)).await?;
        let mut members = vec![a.as_str(), &self.b.account.mri];
        members.extend(odd.iter().take(4).map(String::as_str));
        self.plain_group = self.c.create(&members, None).await?;
        for (index, peer) in self.odd.iter().enumerate() {
            peer.send(&self.topic_group, &format!("A made-up note {index}")).await?;
            if index < 4 {
                peer.send(&self.plain_group, &format!("Another made-up note {index}"))
                    .await?;
            }
            let chat = peer.create(&[&a], None).await?;
            peer.send(&chat, &format!("Hello, a made-up hello {index}")).await?;
            self.odd_chats.push(chat);
        }
        for peer in [&self.b, &self.c, &self.d] {
            peer.send(&self.topic_group, "A made-up note").await?;
        }
        self.c.send(&self.plain_group, "A made-up note").await?;
        self.b_chat = self.b.create(&[&a], None).await?;
        self.b.send(&self.b_chat, "A made-up hello").await?;
        Ok(())
    }
}

// --- the child ---

/// a's account as the app starts it: signed in with its refresh token, its engine on the server, silence for its
/// microphone.
fn first_account(dir: &Path, feed: &VideoFeed) -> SandboxResult<Started<MemorySecretStore>> {
    let (origin, endpoint) = (env(ORIGIN_ENV)?, env(ENDPOINT_ENV)?);
    // the session's requests run on this runtime's reactor for the app's whole life
    let runtime = Box::leak(Box::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?,
    ));
    let account = runtime.block_on(Account::sign_in(UPN[0], &env(REFRESH_ENV)?, &origin, &endpoint))?;
    let devices: Arc<dyn Devices + Send + Sync> = Arc::new(FileDevices {
        input: AudioInput::Silence,
        recording: None,
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    });
    let video = Some(teams_core::account::call_video(Arc::new(feed.clone())));
    let handle = {
        let _entered = runtime.enter();
        account.engine(NAMES[0], devices, video, &dir.join("store"))?
    };
    let mut personal = PersonalAccount::new(
        UPN[0],
        account.mri.trim_start_matches("8:orgid:"),
        "00000000-0000-4000-8000-000000000001",
    )?;
    personal.name = Some(NAMES[0].to_owned());
    personal.server = Some(origin);
    Ok(Started {
        account: personal,
        engine: AccountEngine {
            handle,
            session: Arc::clone(&account.session),
        },
        calls: None,
    })
}

pub(super) fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let dir = PathBuf::from(env(super::DIR_ENV)?);
    let feed = VideoFeed::new();
    let first = first_account(&dir, &feed)?;
    super::run_app(first, feed, "8:echo123")
}
