//! What the diagnostic log tests share: two native clients of the lab against the mock chat service and its
//! Trouter, every account with a real-shaped token (a JWT, made up here).

use std::fs;
use std::future::{self, Future};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Event, Parts, Push};
use teams_mock::{AB_CHAT, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, TrouterMock};
use teams_proto::Mri;
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{Options, RegistrationReport, Trouter, TrouterCredentials};
use tokio::sync::broadcast;

/// How long a test waits for an event.
pub const WAIT: Duration = Duration::from_secs(30);

/// A JWT-shaped token, made up: what a real `ic3` token looks like.
pub fn jwt(seed: &str) -> String {
    let body = format!("{seed}LoGtEsTtOkEnAbCdEf0123456789-_").repeat(5);
    format!("eyJ0eXAiOiJKV1QiLCJhbGciOiJSUzI1NiJ9.eyJ{body}.{body}")
}

/// One account's token, for the transport and the push channel alike.
pub struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(AccessToken {
            token: Secret::new(self.token.clone()),
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

/// `me`'s engine against the mocks, knowing a and b by `names` (writing as its own), its store in `dir`.
pub fn native(mock: &ChatServiceMock, trouter: &TrouterMock, me: &str, names: [&str; 2], dir: &Path) -> EngineHandle {
    native_for_cloud(mock, trouter, me, names, dir, &teams_proto::Cloud::Microsoft)
}

pub fn native_for_cloud(
    mock: &ChatServiceMock,
    trouter: &TrouterMock,
    me: &str,
    names: [&str; 2],
    dir: &Path,
    cloud: &teams_proto::Cloud,
) -> EngineHandle {
    let tokens = Arc::new(Tokens {
        token: mock.token(me).unwrap(),
        renewals: broadcast::channel(4).0,
    });
    let origin = mock.origin();
    let base = cloud.service_url(CHAT_BASE, "/api/chatsvc/amer").unwrap();
    let http = || {
        let transport =
            HttpTransport::for_cloud_with_mock_origin(Arc::clone(&tokens), base.as_str(), cloud.clone(), &origin)
                .unwrap()
                .for_account(Mri::new(me));
        if cloud.is_microsoft() {
            transport
        } else {
            transport
                .with_presence(
                    cloud
                        .service_url(teams_mock::PRESENCE_BASE, "/ups/noam")
                        .unwrap()
                        .as_str(),
                )
                .unwrap()
        }
    };
    let transport = Arc::new(http());
    let push = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), trouter.origin())
        .unwrap()
        .with_options(Options {
            backoff_base: Duration::from_millis(50),
            backoff_cap: Duration::from_millis(50),
            backoff_jitter_percent: 0,
            ..Options::default()
        });
    let client = Arc::new(Client::new(http()));
    let outbox = Outbox::new(
        transport,
        Author::new(Mri::new(me), if me == LAB_A { names[0] } else { names[1] }),
    );
    engine::start(Parts {
        calls: None,
        store: Store::open(&dir.join("store.sqlite")).unwrap(),
        client,
        outbox,
        push: Some(Push::new(push)),
        options: EngineOptions {
            tick: Duration::from_millis(50),
            ..EngineOptions::default()
        },
        known_names: vec![
            (LAB_A.to_owned(), names[0].to_owned()),
            (LAB_B.to_owned(), names[1].to_owned()),
        ],
    })
    .unwrap()
}

/// The next event `pick` takes, skipping others.
pub async fn event<T>(handle: &mut EngineHandle, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let event = tokio::time::timeout_at(deadline, handle.next_event())
            .await
            .expect("the event came in time")
            .expect("the engine runs");
        if let Some(picked) = pick(&event) {
            return picked;
        }
    }
}

/// Waits until the push channel is registered and its first catch-up has run.
pub async fn ready(handle: &mut EngineHandle) {
    let (mut registered, mut caught_up) = (false, false);
    while !(registered && caught_up) {
        let seen = event(handle, |event| match event {
            Event::Registration(RegistrationReport::Registered) => Some(true),
            Event::CaughtUp { .. } => Some(false),
            _ => None,
        })
        .await;
        if seen {
            registered = true;
        } else {
            caught_up = true;
        }
    }
}

/// Waits for a message event about `id` in the a–b chat.
pub async fn message(handle: &mut EngineHandle, id: &str) {
    event(handle, |event| match event {
        Event::Message {
            conversation, id: got, ..
        } if conversation == AB_CHAT && got == id => Some(()),
        _ => None,
    })
    .await;
}

/// Every file in `dir`, as text.
pub fn every_file(dir: &Path) -> String {
    let mut text = String::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        text.push_str(&fs::read_to_string(entry.path()).unwrap());
    }
    text
}
