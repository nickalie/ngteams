//! An own-server group call in the app's own call window, as a user meets it, in the UI sandbox (`harness = false`:
//! GPUI runs on the process's main thread). QA of task 297 (the media node).
//!
//! The parent runs the real `ngteams-server` binary on loopback (`serve`, SQLite, the run's coturn as its relay, its
//! media node on 127.0.0.1) with synthetic accounts a, b and c and more synthetic people of odd names; b and c are
//! clients of their own in the parent (the client's engine, a tone each on the file devices). In one UI sandbox it
//! runs this test again as a's app (`ngteams::app::launch` over a's engine on the server, signed in with a's own
//! refresh token, its calls' media through the server's relay), serving its control socket and its view trace.
//!
//! - b calls the a-b-c chat; a answers over the control socket and c from its client: the call window shows the
//!   three, b (its tone playing) as the speaker, c muted; a mutes too. Shots at 1280×800 and 480×800, Teams Light
//!   and Dark, under `target/ui-shots/own-group-call-*.png` (and `$OWN_GROUP_CALL_EVIDENCE`, if set).
//! - Everyone leaves; the chat shows the call's end (a shot of the main window).
//! - The server restarts as `serve --role api` without Redis (no media node): a calls the chat, and the call fails
//!   at its start; a shot of what the window shows.
//!
//! Without weston 15, niri or `dbus-daemon`, `NGTS_TEST_COTURN` and `NGTS_TEST_COTURN_SECRET`, or the server binary
//! beside the test's target directory, it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, thread};

use gpui::{App, AsyncApp, WindowingModes};
use ngteams::accounts::{Runner, Started};
use ngteams::accounts_file::{PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::calling::PersonalCalls;
use ngteams::descriptors::adopt_trace;
use ngteams::setup::Setup;
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore as _};
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, Scope, TokenClient};
use teams_core::account::{AccountEngine, CALL_LANGUAGE, web_client_info};
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Event, Parts, Push};
use teams_core::{CallAction, CallFloor, CallId, CallMedia, CallParts, CallStatus, Identity, Outbox, Relay};
use teams_ctl::protocol::{CallRow, Request, Toggle};
use teams_ctl::{Client as CtlClient, Timeouts};
use teams_media::devices::Devices;
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::processing::ProcessingConfig;
use teams_media::session::SessionConfig;
use teams_media::tone::Tone;
use teams_proto::{Author, Client as ChatClient, Cloud, HttpTransport, Mri};
use teams_store::{OutboxState, Store};
use teams_trouter::{CallStack, Options, RegistrationReport, Trouter};
use teams_ui::settings::Settings;
use teams_ui::{SettingsFile, VideoFeed};
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

const CHILD_ENV: &str = "NGTEAMS_OWN_GROUP_CALL_TEST";
const TRACE_FD_ENV: &str = "NGTEAMS_OWN_GROUP_CALL_TRACE_FD";
const ORIGIN_ENV: &str = "NGTEAMS_OWN_GROUP_CALL_ORIGIN";
const ENDPOINT_ENV: &str = "NGTEAMS_OWN_GROUP_CALL_ENDPOINT";
const REFRESH_ENV: &str = "NGTEAMS_OWN_GROUP_CALL_REFRESH";
const CHAT_ENV: &str = "NGTEAMS_OWN_GROUP_CALL_CHAT";
const DIR_ENV: &str = "NGTEAMS_OWN_GROUP_CALL_DIR";
const CTL_ENV: &str = "NGTEAMS_OWN_GROUP_CALL_CTL";
/// Where the screenshots are copied besides `target/ui-shots/`, if set (the QA run's evidence).
const EVIDENCE_ENV: &str = "OWN_GROUP_CALL_EVIDENCE";
const COTURN_ENV: &str = "NGTS_TEST_COTURN";
const COTURN_SECRET_ENV: &str = "NGTS_TEST_COTURN_SECRET";
/// The file in the sandbox dir the parent writes to have the child quit.
const QUIT_TRIGGER: &str = "quit";
/// The file in the sandbox dir the parent writes a theme's key into to have the settings take it.
const THEME_TRIGGER: &str = "theme";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(45);
const SETTLE: Duration = Duration::from_millis(1500);
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
const SCREEN: (u32, u32) = (1920, 1080);
/// The client id the native client signs in as.
const CLIENT_ID: &str = "1fec8e78-bce4-4aaf-ab1b-5451cc387264";
/// Every synthetic account's password.
const PASSWORD: &str = "correct horse battery";
const UPN: [&str; 3] = ["ada@own.test", "bert@own.test", "cora@own.test"];
const NAMES: [&str; 3] = ["Ada Ł. Lovelace", "Bert 🎺 Bach", "Cora 'Cee' Nuñez"];
/// Synthetic members of the chat with no client: odd names.
const ODD: [&str; 4] = [
    "Zoë O'Brien-Ångström",
    "李 小龍",
    "Dr. 🦀 Ferris",
    "<b>Bold</b> & \"Quoted\"",
];
const B_TONE: Tone = Tone {
    hz: 660.0,
    amplitude: 0.5,
};

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("own group call window test: {error}");
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

fn http() -> SandboxResult<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(12))
        .build()?)
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
            log: logs.join("server.log"),
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
            return Err(format!("{args:?} failed").into());
        }
        Ok(String::from_utf8(output.stdout)?)
    }

    /// `user add`: the account's object id.
    fn add_user(&self, upn: &str, name: &str) -> SandboxResult<String> {
        let line = self.cli(&["user", "add", upn, "--name", name], &format!("{PASSWORD}\n"))?;
        let id = line
            .split_once('(')
            .and_then(|(_, rest)| rest.split_once(')'))
            .map(|(id, _)| id.to_owned())
            .ok_or_else(|| format!("no id in {line:?}"))?;
        Ok(id)
    }

    /// `serve` with `role` (the default when `None`), until it answers ready.
    async fn serve(&mut self, role: Option<&str>) -> SandboxResult {
        let log = fs::OpenOptions::new().create(true).append(true).open(&self.log)?;
        let mut command = self.command();
        command.arg("serve");
        if let Some(role) = role {
            command.args(["--role", role]);
        }
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

// --- an account's client, as the app builds it ---

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
        let cloud = Cloud::own(&self.origin)?;
        let tracker = Arc::new(teams_call::Tracker::for_cloud(mri.clone(), cloud));
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
        let identity = Identity {
            mri: mri.clone(),
            display_name: Some(name.to_owned()),
            endpoint_id: String::new(),
            language_id: Some(CALL_LANGUAGE.to_owned()),
            tenant_id: String::new(),
            client: web_client_info(),
            region: self.session.region().name,
            partition: self.session.region().partition,
        };
        let media = CallMedia {
            devices,
            relay: Relay::Teams,
            session: SessionConfig {
                processing: ProcessingConfig::OFF,
                ..Default::default()
            },
            video,
        };
        let calls = CallParts::new(trouter()?.for_calls(stack), identity, media);
        let outbox = Outbox::new(Arc::clone(&transport), Author::new(mri, name)).with_calls(tracker, floor);
        let reads = Arc::new(teams_call::Tracker::for_cloud(
            Mri::new(self.mri.clone()),
            Cloud::own(&self.origin)?,
        ));
        fs::create_dir_all(dir)?;
        Ok(engine::start(Parts {
            calls: Some(calls),
            store: Store::open(&dir.join("account.sqlite"))?,
            client: Arc::new(ChatClient::new(self.transport(&reads)?)),
            outbox,
            push: Some(Push::new(trouter()?)),
            options: EngineOptions {
                tick: Duration::from_millis(50),
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })?)
    }
}

/// A client the parent runs for b or c: its engine, its events so far.
struct Peer {
    account: Account,
    engine: EngineHandle,
    log: Vec<Event>,
    _dir: tempfile::TempDir,
}

impl Peer {
    fn start(account: Account, name: &str, tone: Tone) -> SandboxResult<Self> {
        let dir = tempfile::tempdir()?;
        let devices: Arc<dyn Devices + Send + Sync> = Arc::new(FileDevices {
            input: AudioInput::Tone(tone),
            recording: None,
            camera: PatternCamera::default(),
            pacing: Pacing::RealTime,
        });
        let engine = account.engine(name, devices, None, dir.path())?;
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
}

// --- the parent ---

fn evidence_dirs() -> Vec<PathBuf> {
    let shots = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    let mut dirs = vec![shots];
    dirs.extend(std::env::var_os(EVIDENCE_ENV).map(PathBuf::from));
    dirs
}

fn parent() -> SandboxResult {
    let (Ok(relay), Ok(secret)) = (std::env::var(COTURN_ENV), std::env::var(COTURN_SECRET_ENV)) else {
        say("skipped: no coturn named (NGTS_TEST_COTURN, NGTS_TEST_COTURN_SECRET)");
        return Ok(());
    };
    let Some(binary) = server_binary()? else {
        say("skipped: no ngteams-server beside the target directory (cargo build -p ngteams-server)");
        return Ok(());
    };
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams own group call", &options)? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    let logs = evidence_dirs().pop().ok_or("no evidence dir")?;
    fs::create_dir_all(&logs)?;
    let mut server = Server::new(binary, &relay, &secret, &logs)?;
    server.cli(&["migrate"], "")?;
    let mut ids = Vec::new();
    for (upn, name) in UPN.iter().zip(NAMES) {
        ids.push(server.add_user(upn, name)?);
    }
    let a = format!("8:orgid:{}", ids.first().ok_or("no a")?);
    let mut odd = Vec::new();
    for (index, name) in ODD.iter().enumerate() {
        odd.push(format!(
            "8:orgid:{}",
            server.add_user(&format!("odd.{index}@own.test"), name)?
        ));
    }
    runtime.block_on(server.serve(None))?;
    let outcome = runtime.block_on(people(&server)).and_then(|(b, c, a_refresh)| {
        let chat = runtime.block_on(chat_of(&b, &c, &a, &odd))?;
        let dir = tempfile::tempdir()?;
        let mut app = AppUnderTest::start(&sandbox, &server, &a_refresh, &chat, dir.path())?;
        let outcome = scenario(&sandbox, &runtime, &mut server, &mut app, (b, c), &chat);
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
    say("ngteams own group call window: ok");
    Ok(())
}

/// b and c signed in, their clients online; a's refresh token for the app.
async fn people(server: &Server) -> SandboxResult<(Peer, Peer, String)> {
    let mut peers = Vec::new();
    for index in 1..3 {
        let refresh = server.refresh_token(UPN[index]).await?;
        let account = Account::sign_in(UPN[index], &refresh, &server.origin, &server.endpoint).await?;
        let tone = if index == 1 {
            B_TONE
        } else {
            Tone {
                hz: 880.0,
                amplitude: 0.5,
            }
        };
        let mut peer = Peer::start(account, NAMES[index], tone)?;
        peer.online().await?;
        peers.push(peer);
    }
    let c = peers.pop().ok_or("no c")?;
    let b = peers.pop().ok_or("no b")?;
    Ok((b, c, server.refresh_token(UPN[0]).await?))
}

/// The a-b-c chat b makes, with the odd synthetic people in it; c has it.
async fn chat_of(b: &Peer, c: &Peer, a: &str, odd: &[String]) -> SandboxResult<String> {
    let mut members = vec![Mri::new(a), Mri::new(c.account.mri.clone())];
    members.extend(odd.iter().map(Mri::new));
    let row = b
        .engine
        .write(Action::CreateChat {
            others: members,
            topic: Some("Q4 ✨ «planning» & co".to_owned()),
        })
        .await?;
    if row.state != OutboxState::Sent {
        return Err(format!("the chat wasn't made: {row:?}").into());
    }
    let chat = row.message_id.ok_or("no chat id")?;
    c.engine.sync().await?;
    Ok(chat)
}

/// Everything the user does, in order.
fn scenario(
    sandbox: &Sandbox,
    runtime: &tokio::runtime::Runtime,
    server: &mut Server,
    app: &mut AppUnderTest,
    (mut b, mut c): (Peer, Peer),
    chat: &str,
) -> SandboxResult {
    let mut ctl = wait_for(WAIT, || CtlClient::connect(&app.socket, Timeouts::default()).ok())
        .ok_or_else(|| format!("no control socket at {}", app.socket.display()))?;
    // b calls the chat; a answers over the control socket, c from its client
    let (b_mark, c_mark) = (b.log.len(), c.log.len());
    let b_call = runtime.block_on(b.act(CallAction::Call { chat: chat.to_owned() }))?;
    runtime.block_on(b.connected(b_mark, b_call))?;
    wait_call(&mut ctl, "a rings", |row| row.ringing)?;
    shot(sandbox, "own-group-call-ringing-screen", false)?;
    ctl.request(None, Request::Answer)?;
    wait_call(&mut ctl, "a connected", |row| row.state == "connected")?;
    let rung = runtime.block_on(c.incoming(c_mark))?;
    let c_call = runtime.block_on(c.act(CallAction::Answer { call: rung }))?;
    runtime.block_on(c.connected(c_mark, c_call))?;
    // c mutes; b's tone plays: b is the speaker
    runtime.block_on(c.act(CallAction::Mute {
        call: c_call,
        muted: true,
    }))?;
    thread::sleep(SETTLE * 3);
    let mut window = CallWindow::find(sandbox)?;
    for (width, height) in [(1280, 800), (480, 800)] {
        window.resize(width, height)?;
        for theme in [teams_ui::theme::LIGHT_KEY, teams_ui::theme::DARK_KEY] {
            fs::write(sandbox.path(THEME_TRIGGER), theme)?;
            thread::sleep(SETTLE);
            window.save(&format!("own-group-call-three-{width}-{theme}"))?;
        }
    }
    // a mutes
    ctl.request(None, Request::Mute { op: Toggle::On })?;
    wait_call(&mut ctl, "a muted", |row| row.muted)?;
    window.resize(1280, 800)?;
    fs::write(sandbox.path(THEME_TRIGGER), teams_ui::theme::LIGHT_KEY)?;
    thread::sleep(SETTLE);
    window.save("own-group-call-a-muted-1280-light")?;
    // everyone leaves; the chat shows the call's end
    ctl.request(None, Request::HangUp)?;
    for (peer, call) in [(&mut b, b_call), (&mut c, c_call)] {
        let mark = peer.log.len();
        runtime.block_on(peer.act(CallAction::HangUp { call }))?;
        runtime.block_on(peer.since(
            mark,
            "the call's end",
            |event| matches!(event, Event::Call { call: of, status: CallStatus::Ended(_) } if *of == call),
        ))?;
    }
    thread::sleep(SETTLE * 4);
    shot(sandbox, "own-group-call-ended-chat-screen", false)?;
    // no media node: the server serves the public API alone; a's call fails at its start
    server.stop()?;
    runtime.block_on(server.serve(Some("api")))?;
    thread::sleep(Duration::from_secs(5));
    ctl.request(None, Request::Call { chat: chat.to_owned() })?;
    thread::sleep(Duration::from_millis(400));
    shot(sandbox, "own-group-call-no-media-node-screen-early", false)?;
    thread::sleep(SETTLE * 2);
    shot(sandbox, "own-group-call-no-media-node-screen", false)?;
    let calls = calls(&mut ctl)?;
    say(&format!("after the start with no media node, the calls are {calls:?}"));
    drop(b);
    drop(c);
    Ok(())
}

fn calls(ctl: &mut CtlClient) -> SandboxResult<Vec<CallRow>> {
    match ctl.request(None, Request::Calls)? {
        teams_ctl::protocol::Response::Calls { calls } => Ok(calls),
        other => Err(format!("calls: {other:?}").into()),
    }
}

fn wait_call(ctl: &mut CtlClient, what: &str, wanted: impl Fn(&CallRow) -> bool) -> SandboxResult<CallRow> {
    let deadline = Instant::now() + WAIT;
    loop {
        let calls = calls(ctl)?;
        if let Some(row) = calls.iter().find(|row| wanted(row)) {
            return Ok(row.clone());
        }
        if Instant::now() > deadline {
            return Err(format!("{what}: the calls are {calls:?}").into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for<T>(within: Duration, mut done: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + within;
    loop {
        if let Some(value) = done() {
            return Some(value);
        }
        if Instant::now() > deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// A screenshot of the focused window (`window`) or the whole screen, kept as `<name>.png` in the evidence dirs.
fn shot(sandbox: &Sandbox, name: &str, window: bool) -> SandboxResult {
    let png = sandbox.screenshot(&format!("{name}.png"), window)?;
    for dir in evidence_dirs() {
        fs::create_dir_all(&dir)?;
        fs::copy(&png, dir.join(format!("{name}.png")))?;
    }
    let _ = fs::remove_file(png);
    Ok(())
}

/// The call window, by niri's id.
struct CallWindow<'a> {
    sandbox: &'a Sandbox,
    id: u64,
}

impl<'a> CallWindow<'a> {
    /// The window whose title names the call (the newest window).
    fn find(sandbox: &'a Sandbox) -> SandboxResult<Self> {
        let text = sandbox.niri_msg(&["-j", "windows"])?;
        let windows: serde_json::Value = serde_json::from_str(&text)?;
        let id = windows
            .as_array()
            .and_then(|list| list.iter().filter_map(|window| window["id"].as_u64()).max())
            .ok_or_else(|| format!("no windows: {text}"))?;
        say(&format!("the windows: {text}"));
        Ok(Self { sandbox, id })
    }

    fn focus(&self) -> SandboxResult {
        self.sandbox
            .niri_msg(&["action", "focus-window", "--id", &self.id.to_string()])?;
        Ok(())
    }

    /// The compositor sizes it to `width`×`height` (or as near as the window lets it), as a user's drag does.
    fn resize(&mut self, width: i32, height: i32) -> SandboxResult {
        self.focus()?;
        self.sandbox
            .niri_msg(&["action", "set-column-width", &width.to_string()])?;
        self.sandbox
            .niri_msg(&["action", "set-window-height", &height.to_string()])?;
        thread::sleep(SETTLE);
        let text = self.sandbox.niri_msg(&["-j", "windows"])?;
        say(&format!("asked {width}×{height}: {text}"));
        Ok(())
    }

    fn save(&mut self, name: &str) -> SandboxResult {
        self.focus()?;
        thread::sleep(Duration::from_millis(300));
        shot(self.sandbox, name, true)
    }
}

struct AppUnderTest {
    running: Running,
    _trace: TraceReader,
    socket: PathBuf,
}

impl AppUnderTest {
    fn start(sandbox: &Sandbox, server: &Server, refresh: &str, chat: &str, dir: &Path) -> SandboxResult<Self> {
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
                (ORIGIN_ENV, &server.origin),
                (ENDPOINT_ENV, &server.endpoint),
                (REFRESH_ENV, refresh),
                (CHAT_ENV, chat),
                (DIR_ENV, &own_text),
                (CTL_ENV, &socket_text),
            ],
            &[writer.as_fd()],
        )?;
        drop(writer);
        let mut trace = TraceReader::start(reader);
        let mut running = Running::new(child)?;
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
        trace.wait_for("calling", "registered")?;
        Ok(Self {
            running,
            _trace: trace,
            socket,
        })
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

    fn wait_for(&mut self, view: &str, change: &str) -> SandboxResult {
        let deadline = Instant::now() + STARTUP;
        loop {
            while let Ok(line) = self.lines.try_recv() {
                self.seen.push(line);
            }
            if self
                .seen
                .iter()
                .any(|line| line["view"] == view && line["change"] == change)
            {
                return Ok(());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .map_err(|_| format!("no trace line for {view} {change}"))?;
            self.seen.push(line);
        }
    }
}

// --- the child ---

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let first = first_account(&dir)?;
    let runner = Runner {
        setup: Setup::new(PersonalPaths::new(dir.join("ngteams-config"), dir.join("ngteams-data")))?,
        floor: CallFloor::new(),
        devices: None,
    };
    let settings = SettingsFile::new(dir.join("settings.toml"));
    let opened = Settings {
        selected_chat: Some(env(CHAT_ENV)?),
        ..Settings::default()
    };
    settings.save(&opened).map_err(|problem| format!("{problem:?}"))?;
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

/// a's account as the app starts it: signed in with its refresh token, its engine on the server, silence for its
/// microphone.
fn first_account(dir: &Path) -> SandboxResult<Started<MemorySecretStore>> {
    let (origin, endpoint) = (env(ORIGIN_ENV)?, env(ENDPOINT_ENV)?);
    // the session's requests run on this runtime's reactor for the app's whole life
    let runtime = Box::leak(Box::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?,
    ));
    let account = runtime.block_on(Account::sign_in(UPN[0], &env(REFRESH_ENV)?, &origin, &endpoint))?;
    let feed = VideoFeed::new();
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
        calls: Some(PersonalCalls {
            ringer: None,
            sounds: None,
            echo: Mri::new("8:echo123"),
            video: Some(feed),
            camera: None,
            devices: None,
            noise: teams_media::processing::NoiseControl::default(),
        }),
    })
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
