//! One ngteams process holding two Microsoft accounts (on `teams-mock`) and an own-server account, each with its own
//! engine, as the app's accounts window runs them: the Microsoft accounts' calls ask Teams' relay credentials of their
//! own service with their own tokens and allocate only on Teams' relay host; the own account's ask its server and
//! go through its coturn. No token or relay credential crosses between them.
//!
//! A Microsoft account's call looks Teams' relay host up, so this runs only inside the QA network ([`QaNet`]), where
//! nothing leaves the machine: the lookup is answered by nobody and the call fails at its media's setup there.

use std::future::{self, Future};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{CallAction, CallMedia, CallParts, Identity, Outbox, Relay};
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::processing::ProcessingConfig;
use teams_media::relay::TEAMS_RELAY_HOST;
use teams_media::session::SessionConfig;
use teams_mock::{AB_CHAT, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, MIDDLE_TIER_BASE, TENANT, TrouterMock};
use teams_proto::{Author, Client, ClientInfo, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use tokio::sync::broadcast;

use super::coturn_log::{COTURN_LOG, CoturnLog};
use super::qa_net::QaNet;
use super::support::{A_TONE, B_TONE, Rig, TALK, Turn, WAIT};
use super::{connect, hang_up, heard_each_other, pair};
use crate::support::Backend;

/// A Microsoft account's tokens, the mock's made-up ones.
struct Tokens {
    ic3: String,
    skypetoken: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.ic3.clone())))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
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

/// A Microsoft account's engine on the mock, its calls' media as a shipped client's: Teams' relay, file devices.
struct Microsoft {
    mri: &'static str,
    ic3: String,
    skypetoken: String,
    engine: EngineHandle,
    _dir: tempfile::TempDir,
}

/// A Microsoft account's calls' media as a shipped client's: Teams' relay, the file devices.
fn media(dir: &tempfile::TempDir) -> CallMedia {
    CallMedia {
        devices: Arc::new(FileDevices {
            input: AudioInput::Tone(A_TONE),
            recording: Some(dir.path().join("heard.wav")),
            camera: PatternCamera::default(),
            pacing: Pacing::RealTime,
        }),
        relay: Relay::Teams,
        session: SessionConfig {
            processing: ProcessingConfig::OFF,
            ..Default::default()
        },
        video: None,
    }
}

fn identity(mri: &str) -> Identity {
    Identity {
        mri: Mri::new(mri),
        display_name: Some("Lab".into()),
        endpoint_id: String::new(),
        language_id: Some("en-us".into()),
        tenant_id: TENANT.into(),
        client: ClientInfo {
            endpoint_capabilities: Some(73_463),
            client_endpoint_capabilities: Some(114_259_690),
            platform_name: Some("SyntheticNative/1".into()),
        },
        region: Some("amer".into()),
        partition: Some("amer01".into()),
    }
}

impl Microsoft {
    fn start(mock: &ChatServiceMock, trouter: &TrouterMock, mri: &'static str) -> Self {
        let tokens = Arc::new(Tokens {
            ic3: mock.token(mri).unwrap(),
            skypetoken: mock.skypetoken(mri).unwrap(),
            renewals: broadcast::channel(4).0,
        });
        let tracker = Arc::new(teams_call::Tracker::new(Mri::new(mri)));
        let origin = mock.origin();
        let http = || {
            HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin)
                .unwrap()
                .with_middle_tier(MIDDLE_TIER_BASE)
                .unwrap()
                .for_account(Mri::new(mri))
                .with_call_state(tracker.clone())
        };
        let transport = Arc::new(http());
        let new_trouter = || {
            Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), trouter.origin())
                .unwrap()
                .with_options(Options {
                    backoff_base: Duration::from_millis(50),
                    backoff_cap: Duration::from_millis(200),
                    backoff_jitter_percent: 0,
                    ..Options::default()
                })
        };
        let dir = tempfile::tempdir().unwrap();
        let calls = CallParts::new(
            new_trouter().for_calls(CallStack::new(Arc::clone(&tracker))),
            identity(mri),
            media(&dir),
        );
        let outbox = Outbox::new(Arc::clone(&transport), Author::new(Mri::new(mri), "Lab"))
            .with_calls(Arc::clone(&tracker), teams_core::CallFloor::new());
        let engine = engine::start(Parts {
            calls: Some(calls),
            store: Store::open(&dir.path().join("store.sqlite")).unwrap(),
            client: Arc::new(Client::new(http())),
            outbox,
            push: Some(Push::new(new_trouter())),
            options: EngineOptions {
                tick: Duration::from_millis(50),
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })
        .unwrap();
        Self {
            mri,
            ic3: tokens.ic3.clone(),
            skypetoken: tokens.skypetoken.clone(),
            engine,
            _dir: dir,
        }
    }

    /// Presses Call in `chat`: what the client answered.
    async fn call(&self, chat: &str) -> Result<teams_core::CallId, String> {
        let pressed = self.engine.requests().call(CallAction::Call { chat: chat.to_owned() });
        tokio::time::timeout(WAIT, pressed)
            .await
            .expect("the press was answered")
            .map_err(|error| error.to_string())
    }

    async fn shut_down(self) {
        tokio::task::spawn_blocking(move || self.engine.shutdown())
            .await
            .unwrap();
    }
}

/// The value of each header of the mock's received requests, with the request's path.
async fn headers_seen(mock: &ChatServiceMock) -> Vec<(String, String)> {
    mock.received_requests()
        .await
        .iter()
        .flat_map(|request| {
            let path = request.url.path().to_owned();
            request
                .headers
                .iter()
                .map(move |(_, value)| (path.clone(), value.to_str().unwrap_or_default().to_owned()))
        })
        .collect()
}

/// The mock (Microsoft's services) heard each Microsoft account ask for relay credentials with its own token, and
/// never got an own-server token, in any header.
async fn tokens_stayed_apart(mock: &ChatServiceMock, microsoft: &[Microsoft], own_tokens: &[String]) {
    let requests = mock.received_requests().await;
    for account in microsoft {
        let asked = requests.iter().filter(|request| {
            request.url.path() == "/trap/tokens"
                && request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    == Some(format!("Bearer {}", account.ic3).as_str())
        });
        assert!(
            asked.count() >= 1,
            "{} never asked Teams for relay credentials",
            account.mri
        );
    }
    let token_reads = requests
        .iter()
        .filter(|request| request.url.path() == "/trap/tokens")
        .count();
    eprintln!("the mock answered {token_reads} relay credential reads");
    for (path, value) in headers_seen(mock).await {
        for token in own_tokens {
            assert!(
                !value.contains(token.as_str()),
                "an own-server token reached Microsoft's {path}"
            );
        }
    }
    for account in microsoft {
        assert!(!own_tokens.contains(&account.skypetoken));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn microsoft_accounts_beside_an_own_account_keep_teams_relay_and_their_own_tokens() {
    let Some(net) = QaNet::here() else {
        eprintln!("skipped: a Microsoft account's call looks Teams' relay up, which only the QA network keeps in");
        return;
    };
    let Some(rig) = Rig::start(Backend::Sqlite, "accounts", 30, Turn::Coturn).await else {
        return;
    };
    let before = net.seen();
    let log = CoturnLog::from_now(COTURN_LOG);
    let mock = ChatServiceMock::start().await.unwrap();
    let trouter = mock.start_trouter().unwrap();
    let microsoft = [
        Microsoft::start(&mock, &trouter, LAB_A),
        Microsoft::start(&mock, &trouter, LAB_B),
    ];
    let (mut a, mut b, chat) = pair(&rig).await;
    let own_tokens = [a.skypetoken().await, b.skypetoken().await, a.ic3().await, b.ic3().await];

    // each Microsoft account presses Call: its media asks Teams' relay credentials of the mock and looks Teams' relay
    // host up (nobody answers that in the QA network, so the press is refused at the media's setup)
    for account in &microsoft {
        let pressed = account.call(AB_CHAT).await;
        eprintln!("{}: {pressed:?}", account.mri);
        let error = pressed.expect_err("Teams' relay is out of reach in the QA network");
        assert!(error.contains("media couldn't be set up"), "{error}");
    }
    // meanwhile the own account's call goes through its server's coturn
    let talk = connect(&mut a, &mut b, &chat).await;
    tokio::time::sleep(TALK).await;
    heard_each_other(&a, A_TONE, &b, B_TONE).await;
    hang_up(&mut a, talk.call, &mut b, talk.answered).await;

    tokens_stayed_apart(&mock, &microsoft, &own_tokens).await;
    // the own server's coturn served only the own accounts: no Microsoft account's credential reached it
    if let Some(log) = &log {
        let users = log.users();
        eprintln!("coturn users: {users:?}");
        assert!(
            users
                .iter()
                .all(|user| user.ends_with(a.oid()) || user.ends_with(b.oid())),
            "{users:?}"
        );
        assert!(log.allocations(a.oid()) >= 1 && log.allocations(b.oid()) >= 1);
    }
    // only the Microsoft accounts looked a relay up, and only Teams' host; nothing left loopback
    let after = net.seen().since(&before);
    eprintln!("the network: {after:?}");
    assert!(after.outside.is_empty() && after.direct.is_empty(), "{after:?}");
    assert!(
        after.names.iter().any(|name| name.starts_with(TEAMS_RELAY_HOST)),
        "no Microsoft account looked Teams' relay up: {after:?}"
    );
    assert!(
        after.names.iter().all(|name| name.starts_with(TEAMS_RELAY_HOST)),
        "a name other than Teams' relay was looked up: {after:?}"
    );
    a.close().await;
    b.close().await;
    for account in microsoft {
        account.shut_down().await;
    }
    rig.stop().await;
}
