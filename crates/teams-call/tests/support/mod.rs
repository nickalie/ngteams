//! A native calling client on the mock, as the engine runs one: one account's tracker, its
//! HTTP transport and calling socket through the `test-support` mock origins, and a [`Calls`] machine fed with the
//! socket's call deliveries, its writes tracked and sent in order, its responses sent through each
//! delivery's own responder, its events handed to the test.
//!
//! Every id, token and MRI here is the mocks' made-up one.

use std::collections::HashMap;
use std::future::{self, Future};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_call::{
    CallEvent, CallId, Calls, DeliveryKey, DeliveryReply, Effect, Identity, Outcome, Placement, WriteId, WriteSink,
};
use teams_mock::{CHAT_BASE, ChatServiceMock, TENANT};
use teams_proto::Mri;
use teams_proto::{ClientInfo, Credentials, HttpTransport, MediaContent, Transport};
use teams_trouter::{CallResponder, CallResponse, CallStack, Options, PushEvent, RegistrationReport, Trouter};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

/// How long a test waits for an event before it fails.
pub const WAIT: Duration = Duration::from_secs(15);

/// An offer of the account's (its content type is all the signaling reads).
pub fn offer() -> MediaContent {
    MediaContent {
        client_location: Some("ZZ".into()),
        ..MediaContent::new("v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\n", "application/sdp-ngc-1.0")
    }
}

/// An answer of the account's.
pub fn answer() -> MediaContent {
    MediaContent::new("v=0\r\no=- 3 4 IN IP4 127.0.0.1\r\ns=-\r\n", "application/sdp-ngc-1.0")
}

/// The account's tokens: the mock's `ic3` token and skypetoken.
struct Tokens {
    ic3: String,
    skypetoken: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Tokens {
    fn access(&self) -> AccessToken {
        AccessToken {
            token: Secret::new(self.ic3.clone()),
            expires_at: SystemTime::now() + Duration::from_secs(7_000),
            scopes: Vec::new(),
        }
    }
}

impl teams_trouter::TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
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

/// What the test asks a client to do.
enum Command {
    Place(Placement),
    Join(String),
    Answer(CallId),
    Decline(CallId),
    HangUp(CallId),
    Tick(Instant),
}

/// One native client of the account `mri`, on the mock.
pub struct Client {
    pub mri: &'static str,
    commands: mpsc::UnboundedSender<Command>,
    events: mpsc::UnboundedReceiver<CallEvent>,
    refusals: mpsc::UnboundedReceiver<String>,
    task: JoinHandle<()>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.task.abort();
    }
}

type Sending = Pin<Box<dyn Future<Output = Outcome> + Send>>;

/// The engine's sink: each write tracked and sent through the transport, in order.
struct Sink {
    tracker: Arc<teams_call::Tracker>,
    transport: Arc<HttpTransport<Tokens>>,
    queued: Vec<(WriteId, Sending)>,
}

impl WriteSink for Sink {
    fn submit<R: teams_proto::Request + Clone + Send + 'static>(&mut self, id: WriteId, write: R) {
        let (tracker, transport) = (Arc::clone(&self.tracker), Arc::clone(&self.transport));
        self.queued.push((
            id,
            Box::pin(async move {
                let attempt = tracker.before(&write);
                match transport.deliver(write).await {
                    Ok(answer) => {
                        tracker.answered(attempt, &answer);
                        Outcome::Answered {
                            status: answer.status(),
                            body: answer.body().to_vec(),
                        }
                    }
                    Err(_) => Outcome::Failed,
                }
            }),
        ));
    }
}

/// Short waits, so the tests run in seconds.
fn options() -> Options {
    Options {
        ping_interval: Duration::from_millis(300),
        pong_timeout: Duration::from_millis(1_500),
        connect_timeout: Duration::from_secs(5),
        hello_timeout: Duration::from_secs(5),
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(200),
        backoff_jitter_percent: 0,
        renew_check: Duration::from_millis(50),
        ..Options::default()
    }
}

impl Client {
    /// `mri`'s client, its calling socket up and registered.
    pub async fn start(mock: &ChatServiceMock, trouter_origin: &str, mri: &'static str) -> Self {
        Self::start_in(mock, trouter_origin, mri, teams_proto::Cloud::Microsoft).await
    }

    pub async fn start_in(
        mock: &ChatServiceMock,
        trouter_origin: &str,
        mri: &'static str,
        cloud: teams_proto::Cloud,
    ) -> Self {
        let tracker = Arc::new(teams_call::Tracker::for_cloud(Mri::new(mri), cloud.clone()));
        let tokens = Arc::new(Tokens {
            ic3: mock.token(mri).unwrap(),
            skypetoken: mock.skypetoken(mri).unwrap(),
            renewals: broadcast::channel(4).0,
        });
        let chat_base = cloud.service_url(CHAT_BASE, "/api/chatsvc/amer").unwrap();
        let transport = HttpTransport::for_cloud_with_mock_origin(
            Arc::clone(&tokens),
            chat_base.as_str(),
            cloud.clone(),
            &mock.origin(),
        )
        .unwrap();
        let transport = Arc::new(if cloud.is_microsoft() {
            transport
        } else {
            transport.for_account(Mri::new(mri)).with_call_state(tracker.clone())
        });
        let trouter = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), trouter_origin)
            .unwrap()
            .with_options(options())
            .for_calls(CallStack::new(Arc::clone(&tracker)));
        let identity = Identity {
            mri: Mri::new(mri),
            display_name: Some(format!(
                "Lab {}",
                mri.chars().last().unwrap_or('?').to_ascii_uppercase()
            )),
            endpoint_id: trouter.endpoint_id().to_owned(),
            language_id: Some("en-us".into()),
            tenant_id: TENANT.into(),
            client: ClientInfo {
                endpoint_capabilities: Some(73_463),
                client_endpoint_capabilities: Some(114_259_690),
                platform_name: Some("SyntheticNative/1".into()),
            },
            region: Some("amer".into()),
            partition: Some("amer01".into()),
        };
        let (commands, commanded) = mpsc::unbounded_channel();
        let (told, events) = mpsc::unbounded_channel();
        let (refused, refusals) = mpsc::unbounded_channel();
        let (ready, up) = tokio::sync::oneshot::channel();
        let driver = Driver {
            calls: Calls::for_cloud(identity, cloud),
            sink: Sink {
                tracker,
                transport,
                queued: Vec::new(),
            },
            responders: HashMap::new(),
            told,
            refused,
        };
        let task = tokio::spawn(driver.run(trouter.spawn(), commanded, ready));
        tokio::time::timeout(WAIT, up)
            .await
            .expect("the calling socket didn't come up")
            .expect("the client stopped");
        Self {
            mri,
            commands,
            events,
            refusals,
            task,
        }
    }

    /// Why the next action was refused; fails after [`WAIT`].
    pub async fn refusal(&mut self) -> String {
        tokio::time::timeout(WAIT, self.refusals.recv())
            .await
            .expect("no action was refused")
            .expect("the client stopped")
    }

    fn command(&self, command: Command) {
        self.commands.send(command).unwrap();
    }

    pub fn place(&self, placement: Placement) {
        self.command(Command::Place(placement));
    }

    pub fn join(&self, chat: &str) {
        self.command(Command::Join(chat.to_owned()));
    }

    pub fn answer(&self, call: CallId) {
        self.command(Command::Answer(call));
    }

    pub fn decline(&self, call: CallId) {
        self.command(Command::Decline(call));
    }

    pub fn hang_up(&self, call: CallId) {
        self.command(Command::HangUp(call));
    }

    pub fn tick(&self, now: Instant) {
        self.command(Command::Tick(now));
    }

    /// The next event `pick` takes, skipping others; fails after [`WAIT`].
    pub async fn next<T>(&mut self, mut pick: impl FnMut(CallEvent) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let event = tokio::time::timeout_at(deadline, self.events.recv())
                .await
                .unwrap_or_else(|_| panic!("{}: the event didn't come in time", self.mri))
                .expect("the client stopped");
            if let Some(picked) = pick(event) {
                return picked;
            }
        }
    }

    /// Every event within `quiet`, for a test that something didn't happen.
    pub async fn events_within(&mut self, quiet: Duration) -> Vec<CallEvent> {
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + quiet;
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, self.events.recv()).await {
            seen.push(event);
        }
        seen
    }
}

/// The client's loop: the machine fed and its effects carried out.
struct Driver {
    calls: Calls,
    sink: Sink,
    responders: HashMap<DeliveryKey, CallResponder>,
    told: mpsc::UnboundedSender<CallEvent>,
    refused: mpsc::UnboundedSender<String>,
}

impl Driver {
    async fn run(
        mut self,
        mut push: teams_trouter::TrouterHandle,
        mut commanded: mpsc::UnboundedReceiver<Command>,
        ready: tokio::sync::oneshot::Sender<()>,
    ) {
        let mut ready = Some(ready);
        loop {
            tokio::select! {
                event = push.next() => {
                    let Some(event) = event else { return };
                    self.pushed(event, &mut ready);
                }
                command = commanded.recv() => {
                    let Some(command) = command else { return };
                    self.act(command);
                }
            }
            self.carry_out().await;
        }
    }

    fn pushed(&mut self, event: PushEvent, ready: &mut Option<tokio::sync::oneshot::Sender<()>>) {
        match event {
            PushEvent::Connected { push_url, .. } => self.calls.set_push_url(&push_url).unwrap(),
            PushEvent::Registration(RegistrationReport::Registered) => {
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                }
            }
            PushEvent::Call(delivery) => {
                let key = DeliveryKey {
                    connection: delivery.delivery.connection.clone(),
                    id: delivery.delivery.id,
                };
                self.responders.insert(key, delivery.responder());
                self.calls
                    .on_delivery(&delivery.delivery, delivery.observed, Instant::now());
            }
            _ => {}
        }
    }

    fn act(&mut self, command: Command) {
        let now = Instant::now();
        let result = match command {
            Command::Place(placement) => self.calls.place(&placement, offer(), now).map(drop),
            Command::Join(chat) => self.calls.join(&chat, offer(), now).map(drop),
            Command::Answer(call) => self.calls.answer(call, answer()),
            Command::Decline(call) => self.calls.decline(call),
            Command::HangUp(call) => self.calls.hang_up(call, now),
            Command::Tick(at) => {
                self.calls.tick(at);
                Ok(())
            }
        };
        if let Err(error) = result {
            let _ = self.refused.send(error.to_string());
        }
    }

    /// Every effect, in order: a write sent and its outcome fed back before the next.
    async fn carry_out(&mut self) {
        while let Some(effect) = self.calls.poll() {
            match effect {
                Effect::Send { id, write } => {
                    write.submit_to(id, &mut self.sink);
                    for (id, sending) in std::mem::take(&mut self.sink.queued) {
                        let outcome = sending.await;
                        self.calls.on_outcome(id, &outcome, Instant::now());
                    }
                }
                Effect::Respond { delivery, reply } => {
                    let response = match reply {
                        DeliveryReply::Receipt(receipt) => CallResponse::from(receipt),
                        DeliveryReply::Acknowledgement(acknowledgement) => CallResponse::from(acknowledgement),
                    };
                    let sent = match self.responders.get(&delivery) {
                        Some(responder) => responder.respond(response).await.is_ok(),
                        None => false,
                    };
                    if !sent {
                        self.calls.on_response_failed(&delivery);
                    }
                }
                Effect::Event(event) => {
                    let _ = self.told.send(event);
                }
                Effect::LetGo { attach } => self.sink.tracker.let_go(attach.as_str()),
            }
        }
    }
}
