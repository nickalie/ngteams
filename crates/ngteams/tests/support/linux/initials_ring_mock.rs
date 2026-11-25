//! The `mock` case: a Microsoft account on the mock chat service, its Trouter and its calling service.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use std::{fs, future, thread};

use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::account::web_client_info;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{CallMedia, CallParts, Identity, Outbox, Relay};
use teams_media::devices::Devices;
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::processing::ProcessingConfig;
use teams_media::relay::RelayServer;
use teams_media::session::SessionConfig;
use teams_media::tone::Tone;
use teams_mock::{ABC_CHAT, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, TENANT};
use teams_proto::Mri;
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use teams_ui::VideoFeed;
use tokio::sync::broadcast;
use ui_sandbox::SandboxResult;

use super::support::{self, Alias};
use super::{AppUnderTest, ODD, Rig, SETTLE, THEMES, env, say};

const ORIGIN_ENV: &str = "NGTEAMS_INITIALS_RING_ORIGIN";
const TROUTER_ENV: &str = "NGTEAMS_INITIALS_RING_TROUTER";
const RELAY_ENV: &str = "NGTEAMS_INITIALS_RING_RELAY";
const IC3_ENV: &str = "NGTEAMS_INITIALS_RING_IC3";
const SKYPE_ENV: &str = "NGTEAMS_INITIALS_RING_SKYPE";
/// The mock's peers' tone.
const PEER_TONE: Tone = Tone {
    hz: 880.0,
    amplitude: 0.5,
};

/// The made-up person `index` of [`ODD`]: no account of the mock's.
fn made_up_mri(index: usize) -> String {
    format!("8:orgid:00000000-0000-4000-8000-{:012x}", 0x0373_0000 + index)
}

pub(super) fn parent() -> SandboxResult {
    let Some(sandbox) = super::sandbox("ngteams initials ring (mock)")? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mock = runtime.block_on(ChatServiceMock::start())?;
    let trouter = mock.start_trouter()?;
    let dir = tempfile::tempdir()?;
    let relay = mock.start_group_video(dir.path(), PEER_TONE)?.to_string();
    let group = seed(&mock)?;
    let ic3 = mock.token(LAB_A).ok_or("the mock's token")?;
    let skype = mock.skypetoken(LAB_A).ok_or("the mock's skypetoken")?;
    let origin = mock.origin();
    let extra = [
        (ORIGIN_ENV, origin.as_str()),
        (TROUTER_ENV, trouter.origin()),
        (RELAY_ENV, relay.as_str()),
        (IC3_ENV, ic3.as_str()),
        (SKYPE_ENV, skype.as_str()),
    ];
    let waits = [("status", "connected"), ("calling", "registered")];
    let mut app = AppUnderTest::start(&sandbox, ("mock", &extra), dir.path(), &group, &waits)?;
    // the first sync and the profiles' reads settle
    thread::sleep(Duration::from_secs(3));
    let outcome = Rig::new(&sandbox, &mut app, "mock").and_then(|mut rig| scenario(&mut rig, &mock, &group));
    let outcome = outcome.map_err(|error| {
        format!(
            "{error}\n--- the app: {}\n--- the sandbox{}",
            app.running.fail(&"killed"),
            sandbox.logs()
        )
    });
    outcome?;
    app.quit(&sandbox)?;
    drop(trouter);
    say("mock case: ok");
    Ok(())
}

/// A group chat of a and the made-up people of [`ODD`], no topic, a message from each; mentions of a by some of them
/// in a's activity feed. The chat's id.
fn seed(mock: &ChatServiceMock) -> SandboxResult<String> {
    let people: Vec<String> = (0..ODD.len()).map(made_up_mri).collect();
    let mut members = vec![LAB_A];
    members.extend(people.iter().map(String::as_str));
    let group = mock.add_group("", &members).ok_or("no group chat made")?;
    for ((name, _), mri) in ODD.iter().zip(&people) {
        let text = format!(
            "<p>A made-up note from {}</p>",
            mri.rsplit('-').next().unwrap_or_default()
        );
        mock.add_message(&group, (mri, name), "RichText/Html", &text)
            .ok_or("a message refused")?;
    }
    for (index, ((name, _), mri)) in ODD.iter().zip(&people).enumerate() {
        let activity = serde_json::json!({
            "activityType": "mentionInChat", "activitySubtype": "person", "activityId": 373_000 + index,
            "sourceThreadId": group, "sourceMessageId": 1, "sourceUserId": mri,
            "sourceUserImDisplayName": name, "messagePreview": "Lab A a made-up question",
            "sourceThreadType": "chat",
        });
        mock.add_activity(LAB_A, &activity, index % 2 == 0).ok_or("no feed")?;
    }
    Ok(group)
}

/// Everything the user does, in order.
fn scenario(rig: &mut Rig<'_>, mock: &ChatServiceMock, group: &str) -> SandboxResult {
    let card = made_up_mri(0);
    rig.views(&[("abc-chat", ABC_CHAT), ("odd-group", group)], (&card, ODD[0].0))?;
    // b rings a in their 1:1 chat while the odd group chat shows
    for theme in THEMES {
        rig.theme(theme)?;
        rig.step(&format!("chat {group}"))?;
        rig.ring_burst(&format!("one-to-one-ringing-{theme}"), || Ok(mock.ring(LAB_B, LAB_A)))?;
        rig.decline()?;
    }
    // b rings a in the a-b-c chat while it shows; the second ring answered, made-up people of odd names join
    rig.theme(THEMES[0])?;
    rig.step(&format!("chat {ABC_CHAT}"))?;
    rig.ring_burst("group-ringing-light", || Ok(mock.ring_group(LAB_B, ABC_CHAT, &[LAB_A])))?;
    rig.decline()?;
    rig.theme(THEMES[1])?;
    let call = rig.ring_burst("group-ringing-dark", || Ok(mock.ring_group(LAB_B, ABC_CHAT, &[LAB_A])))?;
    rig.answer()?;
    let people: Vec<(String, &str)> = ODD
        .iter()
        .enumerate()
        .map(|(index, (name, _))| (made_up_mri(index), *name))
        .collect();
    let people: Vec<(&str, &str)> = people.iter().map(|(mri, name)| (mri.as_str(), *name)).collect();
    if !mock.join_made_up(&call, &people) {
        return Err("no group call to join".into());
    }
    thread::sleep(SETTLE * 2);
    rig.call_window("group-call")?;
    rig.hang_up()
}

// --- the child ---

/// The account's mock tokens; they never run out in a test.
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

/// a's engine on the mock, its calls' media on file devices, relayed through the mock's TURN server.
fn engine(feed: VideoFeed, dir: &Path) -> SandboxResult<EngineHandle> {
    let tokens = Arc::new(Tokens {
        ic3: env(IC3_ENV)?,
        skypetoken: env(SKYPE_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let tracker = Arc::new(teams_call::Tracker::new(Mri::new(LAB_A)));
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
    let name = support::display_name("bot-a@lab.example.test").to_owned();
    let identity = Identity {
        mri: Mri::new(LAB_A),
        display_name: Some(name.clone()),
        endpoint_id: String::new(),
        language_id: Some("en-us".into()),
        tenant_id: TENANT.into(),
        client: web_client_info(),
        region: Some("amer".into()),
        partition: Some("amer01".into()),
    };
    let devices: Arc<dyn Devices + Send + Sync> = Arc::new(FileDevices {
        input: AudioInput::Silence,
        recording: None,
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    });
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
    Ok(engine::start(Parts {
        calls: Some(CallParts::new(calling, identity, media)),
        store: Store::open(&support::store_file(dir, Alias::A))?,
        client: Arc::new(ChatClient::new(http()?)),
        outbox: Outbox::new(transport, Author::new(Mri::new(LAB_A), name))
            .with_calls(Arc::clone(&tracker), teams_core::CallFloor::new()),
        push: Some(push),
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })?)
}

pub(super) fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let dir = PathBuf::from(env(super::DIR_ENV)?);
    fs::create_dir_all(&dir)?;
    let feed = VideoFeed::new();
    let engine = engine(feed.clone(), &dir)?;
    super::run_app(support::started(Alias::A, engine)?, feed, teams_mock::ECHO)
}
