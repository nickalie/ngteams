//! One account's push channel: the socket's life (connect, authenticate, keep-alive, reconnect), the registration of
//! its endpoint, and the deliveries handed on as typed events. The protocol is `docs/protocol/trouter.md`'s.
//!
//! - **Connect** to the transport's cloud (`wss://go.trouter.teams.microsoft.com/v4/c` for Microsoft,
//!   `wss://<own origin>/trouter/v4/c` otherwise), with the web client's query, and
//!   after a `trouter.connected` straight to its `reconnectUrl` with its `connectparams`, until those are within an
//!   hour of their expiry, three connects in a row have failed (any connect that ended before its `trouter.connected`
//!   but for a renewal) or the service disconnected twice for a `dup` within 10 s.
//! - **Authenticate** in the first frame, at once after the service's `1::` ([`Authenticate`]: the session's `ic3`
//!   token and the cached reconnect parameters). The socket is never authenticated again. The token is asked of the
//!   session again at the `1::`, never the one held since the connect began: a session signed out while the socket
//!   opened stops the client with nothing sent, and one renewed meanwhile authenticates with its new token.
//! - **Keep-alive**: a [`Ping`] every [`Options::ping_interval`] once connected; a pong missing for
//!   [`Options::pong_timeout`] means a reconnect.
//! - **Reconnect** after the socket closes, the service disconnects or a pong is missing: at once for a renewal, a
//!   missing pong, an `unauthorized` disconnect (with a new token) or a `dup` (but never twice within 10 s), else
//!   after a backoff of `base · 2^n` (± [`Options::backoff_jitter_percent`], capped at [`Options::backoff_cap`]),
//!   whose count starts again once a connection stayed up [`Options::stable_after`]. The token service's
//!   `Retry-After` is only a floor: the wait is the longer of it and the backoff. A `skypetoken-deprecated`
//!   disconnect stops the client.
//! - **Token renewal**: the socket is reconnected with a new token before the old one runs out
//!   ([`Options::renew_lead`] before its expiry, checked against the clock every [`Options::renew_check`], so a jump
//!   of the wall clock counts too), and as soon as the session announces another `ic3` token than the one held.
//! - **Registration**: on every `trouter.connected`, the stack's registration of the endpoint's push URL
//!   ([`crate::registration`]).
//! - **Deliveries** on the messaging stack's socket become [`PushEvent::Delivery`] ([`crate::events`]) and are **never
//!   answered**; others' presence at the endpoint's `unifiedPresenceService` path becomes [`PushEvent::Presence`]
//!   ([`crate::presence`]), never answered either; an incoming call's (a `callNotification`, whatever its path) and one
//!   at a path not the endpoint's messaging or presence one are [`PushEvent::Ignored`] and never answered either. A message-loss report becomes
//!   [`PushEvent::MessageLoss`]: catch up, then [`MessageLoss::caught_up`] acknowledges it.
//! - **The calling stack's socket** ([`Trouter::for_calls`]): its own connect (`tc` naming `SkypeSpaces`, `dom`), its
//!   own authentication's headers, the `SkypeSpacesWeb` registration; every delivery at its endpoint read by the call
//!   tracker and handed on as [`PushEvent::Call`] (an invitation the tracker doesn't offer is [`PushEvent::Ignored`]),
//!   answered only by a typed response on its originating connection ([`crate::calls`]); a message-loss report
//!   acknowledged at once (nothing to catch up). A reconnect sends back the cached parameters, so the endpoint and
//!   every call link under its push URL stay as they were, and registers again; nothing is sent to a call
//!   (`docs/protocol/calling.md`, "A call held past a dropped socket").
//!
//! Every frame goes out through the connection's [`FrameSink`]. The socket logs incoming and outgoing frames through
//! the credential-redacting diagnostic wire logger at `debug`.

use std::fmt::Write as _;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt as _;
use futures_util::stream::{FuturesUnordered, SplitStream};
use serde_json::Value;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Clock, REFRESH_MARGIN, Renewal, SystemClock};
use teams_auth::token::TokenError;
use teams_call::Observed;
use teams_proto::{
    Authenticate, CallingRegistration, Cloud, ConnectParams, Disconnect, DroppedIndicator, EndpointRegistrar,
    EndpointRegistration, FrameError, FrameSink, IC3_AUDIENCE, MessageLossAck, OwnFrame, PRESENCE_PUSH_PATH, Ping,
    RegisteredEndpoint, RespondError, Sent,
};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::{self, Message};

use crate::calls::{CallDelivery, CallResponder, CallResponse, CallStack, RespondFailure, ResponseRequest};
use crate::credentials::TrouterCredentials;
use crate::events::Delivery;
use crate::frames::{self, Connected, Event, Incoming, RawDelivery};
use crate::presence::PresenceDelivery;
use crate::registration::{Outcome, Registrar, Registration, RegistrationReport, RegistrationTimes};
use crate::socket::{self, ConnectBase, ConnectError, Opener, SocketWriter, Stack, Stream, WriteFailed};

/// The least time between two reconnects without a wait: a second one sooner waits a backoff, so a service that keeps
/// disconnecting at once (`dup`, say) isn't met with a tight loop.
const IMMEDIATE_SPACING: Duration = Duration::from_secs(10);

/// How long a stretch of time each part of the session takes (the web client's, `docs/protocol/trouter.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Between pings once connected: 40 s (the connect's `timeout`).
    pub ping_interval: Duration,
    /// How long a ping's pong may take: 5 s.
    pub pong_timeout: Duration,
    /// How long opening a socket may take: 20 s.
    pub connect_timeout: Duration,
    /// How long the service's `1::` and its `trouter.connected` may take after the socket opened: 20 s.
    pub hello_timeout: Duration,
    /// The first reconnect wait: 1 s.
    pub backoff_base: Duration,
    /// The longest reconnect wait: 300 s (the messaging stack's).
    pub backoff_cap: Duration,
    /// How far a wait may be moved either way, in percent: 20.
    pub backoff_jitter_percent: u32,
    /// How long a connection must stay up for the backoff to start again from its base: 60 s.
    pub stable_after: Duration,
    /// How long before the token's expiry the socket is reconnected with a new one: the session's refresh margin.
    pub renew_lead: Duration,
    /// How often the token's expiry is checked against the clock: 30 s.
    pub renew_check: Duration,
    pub registration: RegistrationTimes,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_secs(socket::PING_SECONDS),
            pong_timeout: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(20),
            hello_timeout: Duration::from_secs(20),
            backoff_base: Duration::from_secs(1),
            backoff_cap: Duration::from_secs(300),
            backoff_jitter_percent: 20,
            stable_after: Duration::from_secs(60),
            renew_lead: REFRESH_MARGIN,
            renew_check: Duration::from_secs(30),
            registration: RegistrationTimes::default(),
        }
    }
}

/// What the push channel tells its owner.
#[derive(Debug)]
pub enum PushEvent {
    /// A socket is up and its endpoint known (`trouter.connected`): pushes may have been missed before it, so catch
    /// up. `new_endpoint` when the service handed over another endpoint id than the last connection's; `push_url` the
    /// endpoint's push URL as the event gave it (`surl`), where the calling stack's callbacks go.
    Connected {
        endpoint: String,
        new_endpoint: bool,
        push_url: String,
    },
    /// A registration of the endpoint went through: the endpoint it registered (the client's `epid`), the account's own
    /// registered endpoint of this run, the one id its own availability may name (M2 Task 19). Told just before the
    /// registration's [`PushEvent::Registration`] report.
    Registered(RegisteredEndpoint),
    /// How a registration of the endpoint went.
    Registration(RegistrationReport),
    /// The service reported pushes the endpoint missed.
    MessageLoss(MessageLoss),
    /// A chat delivery. It is never answered.
    Delivery(Delivery),
    /// Others' presence, delivered at the endpoint's `unifiedPresenceService` path once it subscribed to it
    /// (`teams_proto::PresenceSubscription`, [`crate::presence`]). It is never answered.
    Presence(PresenceDelivery),
    /// A calling delivery and the tracker's observation, with typed replies bound to its connection.
    Call(CallDelivery),
    /// A delivery that isn't a chat event of this endpoint, or on the calling stack's socket an invitation the tracker
    /// doesn't offer. It is never answered.
    Ignored(Ignored),
    /// The socket is down; the client reconnects. `heard`: when (by the client's clock) the socket last read a frame
    /// once connected (a delivery, a pong, any frame), the last moment the channel was known to work; a socket that
    /// went down silently (a suspend, a half-open connection) is noticed only later, by a late pong. `None` when
    /// the socket never got its `trouter.connected`.
    Disconnected { down: Down, heard: Option<SystemTime> },
    /// The client stopped; nothing more comes.
    Stopped(Stop),
}

/// A message-loss report (`trouter.message_loss`): catch up (a `messaging` tag means the chats missed pushes), then
/// [`Self::caught_up`] acknowledges it on the socket it came on. Dropped without that, it stays unacknowledged.
#[derive(Debug)]
pub struct MessageLoss {
    pub indicators: Vec<DroppedIndicator>,
    ack: Option<oneshot::Sender<()>>,
}

impl MessageLoss {
    /// Whether the chats missed pushes (a `messaging` tag).
    #[must_use]
    pub fn messaging(&self) -> bool {
        self.indicators.iter().any(|indicator| indicator.tag == "messaging")
    }

    /// Acknowledges the report (`trouter.processed_message_loss`) once caught up; nothing when its socket is gone.
    pub fn caught_up(mut self) {
        if let Some(ack) = self.ack.take() {
            let _ = ack.send(());
        }
    }
}

/// A delivery the client doesn't act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ignored {
    /// An incoming call on the messaging socket, or an invitation the call tracker doesn't offer.
    IncomingCall,
    /// At a path that isn't the endpoint's messaging one (on the calling stack's socket: not its endpoint's root or call
    /// agent).
    NotOurs { path: String },
    /// A presence or calling delivery whose body could not be decoded.
    Undecodable { path: String },
}

/// Why a socket went down.
#[derive(Debug)]
pub enum Down {
    /// It couldn't be opened.
    ConnectFailed(ConnectError),
    /// No token for it (the session's error, which carries no token).
    Credentials(String),
    /// The socket closed or failed.
    Closed,
    /// The service disconnected, with its reason when it gave one.
    ServiceDisconnect(Option<String>),
    /// The service's `1::` or `trouter.connected` didn't come in time.
    HelloTimeout,
    /// A ping's pong didn't come in time.
    PongTimeout,
    /// A frame couldn't be sent.
    SendFailed(WriteFailed),
    /// Reconnected for a new token.
    Renewal,
}

/// Why the client stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    /// Its owner asked ([`TrouterHandle::stop`], or the handle dropped).
    Requested,
    /// The account isn't signed in.
    SignedOut,
    /// The service disconnected with `skypetoken-deprecated`.
    Deprecated,
}

/// Why a client couldn't be built.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("the TLS client can't be set up: {0}")]
    Tls(#[source] tokio_rustls::rustls::Error),
    #[error("not a loopback ws origin")]
    MockOrigin,
    #[error("the system random source failed")]
    Random,
}

/// One account's push channel (see the module docs), built and then [`Self::spawn`]ed.
pub struct Trouter<T, K> {
    transport: Arc<T>,
    credentials: Arc<K>,
    clock: Arc<dyn Clock>,
    opener: Opener,
    cloud: teams_proto::Cloud,
    /// The endpoint's id on every connect (`epid`) and its registration's `registrationId`.
    epid: String,
    options: Options,
    calls: Option<CallStack>,
}

impl<T, K> std::fmt::Debug for Trouter<T, K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Trouter")
            .field("epid", &self.epid)
            .field("options", &self.options)
            .field("calling", &self.calls.is_some())
            .finish_non_exhaustive()
    }
}

impl<T, K> Trouter<T, K> {
    fn stack(&self) -> Stack {
        if self.calls.is_some() {
            Stack::Calling
        } else {
            Stack::Messaging
        }
    }
}

impl<T, K> Trouter<T, K>
where
    T: EndpointRegistrar + Send + 'static,
    K: TrouterCredentials + 'static,
{
    /// The push channel of the account whose registrations go through `transport` and whose tokens `credentials`
    /// gives, with a fresh endpoint id. The transport determines the cloud; credentials must belong to that account.
    ///
    /// # Errors
    /// [`BuildError::Tls`] when the system's certificate verifier can't be set up, [`BuildError::Random`] when no
    /// endpoint id can be made.
    pub fn new(transport: Arc<T>, credentials: Arc<K>) -> Result<Self, BuildError> {
        let cloud = transport.cloud().clone();
        let opener = Opener::new(socket::tls_config().map_err(BuildError::Tls)?).for_cloud(cloud.clone());
        Ok(Self {
            transport,
            cloud,
            credentials,
            clock: Arc::new(SystemClock),
            opener,
            epid: guid().ok_or(BuildError::Random)?,
            options: Options::default(),
            calls: None,
        })
    }

    /// Uses the calling stack's socket, registration and delivery tracking instead of the messaging stack.
    #[must_use]
    pub fn for_calls(mut self, calls: CallStack) -> Self {
        self.calls = Some(calls);
        self
    }

    /// As [`Self::new`], but every socket goes to `origin` (`ws://127.0.0.1:<port>`, a local mock) instead, with the
    /// path and query it was built and checked with. Tests only (`test-support`).
    ///
    /// # Errors
    /// [`BuildError::MockOrigin`] unless `origin` is `ws` on a loopback host with a port and nothing after it;
    /// otherwise as [`Self::new`].
    #[cfg(feature = "test-support")]
    pub fn with_mock_origin(transport: Arc<T>, credentials: Arc<K>, origin: &str) -> Result<Self, BuildError> {
        let url = url::Url::parse(origin).map_err(|_| BuildError::MockOrigin)?;
        let loopback = matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"));
        let bare = url.path() == "/" && url.query().is_none() && url.fragment().is_none() && url.username().is_empty();
        if url.scheme() != "ws" || !loopback || url.port().is_none() || !bare || url.password().is_some() {
            return Err(BuildError::MockOrigin);
        }
        let mut trouter = Self::new(transport, credentials)?;
        trouter.opener = trouter.opener.with_mock_origin(url);
        Ok(trouter)
    }

    /// Every URL this client asks its mock origin to open, as built and checked: the host a connect chose, which the
    /// mock's handshake can't show. Tests only (`test-support`).
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn opened_urls(&self) -> socket::OpenedUrls {
        self.opener.opened()
    }

    /// Another clock (a fake one in tests); the session's should be the same.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    #[must_use]
    pub fn with_options(mut self, options: Options) -> Self {
        self.options = options;
        self
    }

    /// The endpoint id the client connects and registers with (`epid`).
    #[must_use]
    pub fn endpoint_id(&self) -> &str {
        &self.epid
    }

    /// Runs the push channel on the current tokio runtime until it is stopped.
    #[must_use]
    pub fn spawn(self) -> TrouterHandle {
        let (events, received) = mpsc::unbounded_channel();
        let (stop, stopped) = oneshot::channel();
        let renewals = self.credentials.renewals();
        let run = Run {
            trouter: self,
            events,
            stop: stopped,
            renewals,
            renewals_open: true,
            cache: None,
            failures: 0,
            level: 0,
            connects: 0,
            need: Need::Current,
            last_endpoint: None,
            last_immediate: None,
            last_dup: None,
            heard: None,
        };
        TrouterHandle {
            events: received,
            stop: Some(stop),
            task: tokio::spawn(run.run()),
        }
    }
}

/// The running push channel: its events, and the way to stop it. Dropping it stops the client too.
#[derive(Debug)]
pub struct TrouterHandle {
    events: mpsc::UnboundedReceiver<PushEvent>,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl TrouterHandle {
    /// The next event; `None` once the client has stopped and every event was taken.
    pub async fn next(&mut self) -> Option<PushEvent> {
        self.events.recv().await
    }

    /// Stops the client (it sends its disconnect, `0::`, on an authenticated socket; a stop while a token is fetched
    /// or a socket opened takes effect at once) and waits for it to end. Events not yet taken are dropped.
    pub async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = (&mut self.task).await;
    }
}

/// Which token the next connect takes.
#[derive(Clone)]
enum Need {
    /// The session's current one.
    Current,
    /// A new one in place of this one (refused, or about to run out).
    Replace(Secret),
}

/// What the next connect starts from: a regional host and the parameters to send back.
struct Cached {
    base: ConnectBase,
    params: ConnectParams,
}

/// How long to wait before the next connect.
enum Wait {
    None,
    Backoff,
    For(Duration),
}

/// How one connection ended.
enum End {
    Stop(Stop),
    Again(Down, Wait),
}

/// The push channel's state across its connections.
struct Run<T, K> {
    trouter: Trouter<T, K>,
    events: mpsc::UnboundedSender<PushEvent>,
    stop: oneshot::Receiver<()>,
    renewals: broadcast::Receiver<Renewal>,
    renewals_open: bool,
    cache: Option<Cached>,
    /// Connects that failed in a row.
    failures: u32,
    /// The backoff's exponent.
    level: u32,
    connects: u64,
    need: Need,
    last_endpoint: Option<String>,
    /// When the last reconnect without a wait began.
    last_immediate: Option<Instant>,
    /// When the service last disconnected for a `dup`.
    last_dup: Option<Instant>,
    /// When the connection under way last read a frame once connected ([`PushEvent::Disconnected`]).
    heard: Option<SystemTime>,
}

impl<T, K> Run<T, K> {
    fn emit(&self, event: PushEvent) {
        // an owner that dropped its handle has stopped the client already
        let _ = self.events.send(event);
    }
}

impl<T, K> Run<T, K>
where
    T: EndpointRegistrar + Send + 'static,
    K: TrouterCredentials + 'static,
{
    async fn run(mut self) {
        loop {
            let started = Instant::now();
            let (down, wait) = match self.connection().await {
                End::Stop(stop) => {
                    log::info!("push channel stopped: {stop:?}");
                    self.emit(PushEvent::Stopped(stop));
                    return;
                }
                End::Again(down, wait) => (down, wait),
            };
            log::info!("push channel down: {down:?}");
            let heard = self.heard.take();
            self.emit(PushEvent::Disconnected { down, heard });
            if started.elapsed() >= self.trouter.options.stable_after {
                self.level = 0;
            }
            let immediate_allowed = self
                .last_immediate
                .is_none_or(|last| last.elapsed() >= IMMEDIATE_SPACING);
            let delay = match wait {
                Wait::None if immediate_allowed => {
                    self.last_immediate = Some(Instant::now());
                    log::info!("push channel reconnecting at once");
                    continue;
                }
                Wait::None | Wait::Backoff => self.backoff(),
                // the service's wait is a floor: a `Retry-After: 0` (or a past date) still waits the backoff
                Wait::For(delay) => delay.max(self.backoff()),
            };
            log::info!("push channel reconnecting in {} ms", delay.as_millis());
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                _ = &mut self.stop => {
                    self.emit(PushEvent::Stopped(Stop::Requested));
                    return;
                }
            }
        }
    }

    /// The next backoff wait, and the exponent moved on.
    fn backoff(&mut self) -> Duration {
        let options = &self.trouter.options;
        let doubled = options
            .backoff_base
            .saturating_mul(2_u32.saturating_pow(self.level.min(30)));
        let wait = doubled.min(options.backoff_cap);
        self.level = self.level.saturating_add(1);
        let jitter = options.backoff_jitter_percent.min(100);
        let spread = getrandom::u32().unwrap_or_default() % (2 * jitter + 1);
        wait.saturating_mul(100 - jitter + spread) / 100
    }

    fn sink(&self, writer: SocketWriter, connection: &str) -> FrameSink<SocketWriter> {
        let sink = FrameSink::new(writer);
        if let Some(calls) = &self.trouter.calls {
            // Typed replies stay bound to this socket and its canonical current call.
            sink.with_call_responses(connection)
                .with_call_state(Arc::clone(&calls.state))
        } else {
            sink
        }
    }

    /// One connection, from asking for its token to its end. A stop while the token is fetched or the socket opened
    /// ends it at once (nothing was sent).
    async fn connection(&mut self) -> End {
        let fetched = tokio::select! {
            biased;
            _ = &mut self.stop => return End::Stop(Stop::Requested),
            fetched = fetch_token(&*self.trouter.credentials, &self.need) => fetched,
        };
        let token = match self.token(fetched) {
            Ok(token) => token,
            Err(end) => return end,
        };
        let params = self.drop_stale_cache();
        let base = match (&self.cache, ConnectBase::global_in(&self.trouter.cloud)) {
            (Some(cached), _) => cached.base.clone(),
            (None, Some(global)) => global,
            (None, None) => return End::Again(Down::ConnectFailed(ConnectError::NotAllowed), Wait::Backoff),
        };
        self.connects += 1;
        let con_num = format!("{}_{}", millis(self.trouter.clock.now()), self.connects);
        let url = base.connect_url(
            self.trouter.stack(),
            &self.trouter.epid,
            &guid().unwrap_or_default(),
            &con_num,
        );
        let options = self.trouter.options;
        let opened = tokio::select! {
            biased;
            _ = &mut self.stop => return End::Stop(Stop::Requested),
            opened = self.trouter.opener.open(&url, options.connect_timeout) => opened,
        };
        let socket = match opened {
            Ok(socket) => socket,
            Err(error) => {
                self.connect_failed();
                return End::Again(Down::ConnectFailed(error), Wait::Backoff);
            }
        };
        let (writer, read) = socket::split(socket);
        // the connection's key: one per socket, the same for its deliveries and its responses
        let connection = format!("{}/{}", self.trouter.epid, self.connects);
        let sink = self.sink(writer, &connection);
        let mut link = Link::new(sink, read, token, params, &options, self.trouter.clock.now());
        link.connection = connection;
        link.cloud = self.trouter.cloud.clone();
        let end = link.serve(self).await;
        self.served(&link, &end);
        end
    }

    /// What a connection that ended as `end` leaves behind.
    fn served(&mut self, link: &Link, end: &End) {
        if let Some(calls) = &self.trouter.calls {
            calls.state.disconnected(&link.connection);
        }
        // a connect that ended before its `trouter.connected` failed, whatever ended it (the socket closed, the
        // service disconnected, `1::` or `trouter.connected` never came), unless a renewal ended it
        let failed = matches!(end, End::Again(down, _) if !matches!(down, Down::Renewal));
        if failed && link.endpoint.is_none() {
            self.connect_failed();
        }
    }

    /// The token for the next connect from what [`fetch_token`] got; on success the need is back to the current one.
    fn token(&mut self, result: Result<AccessToken, TokenError>) -> Result<AccessToken, End> {
        match result {
            Ok(token) => {
                self.need = Need::Current;
                Ok(token)
            }
            Err(TokenError::NotSignedIn { .. }) => Err(End::Stop(Stop::SignedOut)),
            Err(TokenError::Throttled { retry_after, .. }) => Err(End::Again(
                Down::Credentials("throttled".to_owned()),
                retry_after.map_or(Wait::Backoff, Wait::For),
            )),
            Err(error) => Err(End::Again(Down::Credentials(error.to_string()), Wait::Backoff)),
        }
    }

    /// Drops the cached regional host when its parameters run out within the hour; the parameters to send otherwise.
    fn drop_stale_cache(&mut self) -> Option<ConnectParams> {
        let now = millis(self.trouter.clock.now());
        let fresh = self.cache.as_ref().is_some_and(|cached| {
            cached
                .params
                .se
                .parse::<u64>()
                .is_ok_and(|expiry| expiry > now.saturating_add(3_600_000))
        });
        if !fresh {
            self.cache = None;
        }
        self.cache.as_ref().map(|cached| cached.params.clone())
    }

    /// A connect that failed: three in a row drop the cached regional host.
    fn connect_failed(&mut self) {
        self.failures += 1;
        if self.failures >= 3 {
            self.cache = None;
        }
    }

    /// A `trouter.connected`: the cache, the event, and whether its endpoint is another one.
    fn connected(&mut self, connected: &Connected) {
        self.failures = 0;
        let base = connected
            .reconnect_url
            .as_deref()
            .and_then(|url| ConnectBase::regional_in(&self.trouter.cloud, url));
        self.cache = match (base, connected.params.clone()) {
            (Some(base), Some(params)) => Some(Cached { base, params }),
            _ => None,
        };
        let new_endpoint = self.last_endpoint.as_deref() != Some(connected.id.as_str());
        log::info!(
            "push channel connected (connect {}, endpoint {}{})",
            self.connects,
            connected.id,
            if new_endpoint { ", a new one" } else { "" }
        );
        self.last_endpoint = Some(connected.id.clone());
        self.emit(PushEvent::Connected {
            endpoint: connected.id.clone(),
            new_endpoint,
            push_url: connected.push_url.clone(),
        });
    }
}

/// The token for the next connect, as `need` says.
async fn fetch_token<K: TrouterCredentials>(credentials: &K, need: &Need) -> Result<AccessToken, TokenError> {
    match need {
        Need::Current => credentials.ic3().await,
        Need::Replace(old) => credentials.ic3_after(old).await,
    }
}

/// One step of a connection's loop.
enum Step {
    Stop,
    Frame(Option<Result<Message, tungstenite::Error>>),
    Ping,
    PongLate,
    HelloLate,
    RenewCheck,
    Renewal(Result<Renewal, broadcast::error::RecvError>),
    Report(Outcome),
    CaughtUp(Vec<DroppedIndicator>),
    Respond(ResponseRequest),
}

/// A message-loss report waiting for its owner to have caught up: its indicators then, `None` if it never did.
type LossAck = Pin<Box<dyn Future<Output = Option<Vec<DroppedIndicator>>> + Send>>;

/// One open socket: its sink (the only way it sends), its read half, and what it waits for.
struct Link {
    cloud: Cloud,
    sink: FrameSink<SocketWriter>,
    read: SplitStream<Stream>,
    token: AccessToken,
    params: Option<ConnectParams>,
    authenticated: bool,
    endpoint: Option<String>,
    hello_by: Instant,
    pong_by: Option<(u64, Instant)>,
    ping: tokio::time::Interval,
    renew_check: tokio::time::Interval,
    /// Whether the token's expiry is far enough off to renew before it (else only a notification renews it).
    renew: bool,
    renew_lead: Duration,
    pong_timeout: Duration,
    registration: RegistrationTimes,
    registrar: Option<Registrar>,
    reports: mpsc::UnboundedReceiver<Outcome>,
    reports_to: mpsc::UnboundedSender<Outcome>,
    acks: FuturesUnordered<LossAck>,
    /// The connection's key: the endpoint and the connect's number, the same for its deliveries and its responses.
    connection: String,
    /// The calling stack's: what its call deliveries hand on, and the responses they send back.
    responder: CallResponder,
    responses: mpsc::UnboundedReceiver<ResponseRequest>,
}

impl Link {
    fn new(
        sink: FrameSink<SocketWriter>,
        read: SplitStream<Stream>,
        token: AccessToken,
        params: Option<ConnectParams>,
        options: &Options,
        now: SystemTime,
    ) -> Self {
        let (reports_to, reports) = mpsc::unbounded_channel();
        let start = Instant::now();
        let mut ping = tokio::time::interval_at(start + options.ping_interval, options.ping_interval);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut renew_check = tokio::time::interval_at(start + options.renew_check, options.renew_check);
        renew_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let renew = renewable(&token, options.renew_lead, now);
        let (responder, responses) = CallResponder::channel();
        Self {
            cloud: Cloud::Microsoft,
            connection: String::new(),
            responder,
            responses,
            sink,
            read,
            token,
            params,
            authenticated: false,
            endpoint: None,
            hello_by: start + options.hello_timeout,
            pong_by: None,
            ping,
            renew_check,
            renew,
            renew_lead: options.renew_lead,
            pong_timeout: options.pong_timeout,
            registration: options.registration,
            registrar: None,
            reports,
            reports_to,
            acks: FuturesUnordered::new(),
        }
    }

    async fn serve<T, K>(&mut self, run: &mut Run<T, K>) -> End
    where
        T: EndpointRegistrar + Send + 'static,
        K: TrouterCredentials + 'static,
    {
        loop {
            let step = self.next_step(run).await;
            if let Some(end) = self.take(step, run).await {
                return end;
            }
        }
    }

    async fn next_step<T, K>(&mut self, run: &mut Run<T, K>) -> Step {
        let connected = self.endpoint.is_some();
        let pong_by = self.pong_by.map_or_else(far_future, |(_, by)| by);
        tokio::select! {
            biased;
            _ = &mut run.stop => Step::Stop,
            frame = self.read.next() => Step::Frame(frame),
            () = tokio::time::sleep_until(self.hello_by), if !connected => Step::HelloLate,
            () = tokio::time::sleep_until(pong_by), if self.pong_by.is_some() => Step::PongLate,
            _ = self.ping.tick(), if connected && self.pong_by.is_none() => Step::Ping,
            _ = self.renew_check.tick(), if self.renew => Step::RenewCheck,
            renewal = run.renewals.recv(), if run.renewals_open => Step::Renewal(renewal),
            Some(report) = self.reports.recv() => Step::Report(report),
            Some(Some(indicators)) = self.acks.next(), if !self.acks.is_empty() => Step::CaughtUp(indicators),
            Some(request) = self.responses.recv() => Step::Respond(request),
        }
    }

    async fn take<T, K>(&mut self, step: Step, run: &mut Run<T, K>) -> Option<End>
    where
        T: EndpointRegistrar + Send + 'static,
        K: TrouterCredentials + 'static,
    {
        match step {
            Step::Stop => {
                // a socket not yet authenticated sends nothing, not even its disconnect
                if self.authenticated {
                    let _ = self.send(&Disconnect::new()).await;
                }
                Some(End::Stop(Stop::Requested))
            }
            Step::Frame(frame) => {
                let end = self.on_frame(frame, run).await;
                if end.is_none() && self.endpoint.is_some() {
                    run.heard = Some(run.trouter.clock.now());
                }
                end
            }
            Step::HelloLate => Some(End::Again(Down::HelloTimeout, Wait::Backoff)),
            Step::PongLate => Some(End::Again(Down::PongTimeout, Wait::None)),
            Step::Ping => self.ping().await,
            Step::RenewCheck => self.renew_check(run).await,
            Step::Renewal(renewal) => self.on_renewal(renewal, run).await,
            Step::Report((report, endpoint)) => {
                if let Some(endpoint) = endpoint {
                    run.emit(PushEvent::Registered(endpoint));
                }
                run.emit(PushEvent::Registration(report));
                None
            }
            Step::CaughtUp(indicators) => {
                let ack = MessageLossAck::new(indicators)?;
                self.send(&ack).await.err()
            }
            Step::Respond(request) => self.respond(request).await,
        }
    }

    /// Writes a typed reply on this delivery's socket.
    async fn respond(&mut self, request: ResponseRequest) -> Option<End> {
        let sent = match request.response {
            CallResponse::Acknowledgement(response) => self.sink.respond(response).await,
            CallResponse::Receipt(response) => self.sink.respond(response).await,
        };
        let (outcome, end) = match sent {
            Ok(_) => (Ok(()), None),
            Err(RespondError::NoCallResponses | RespondError::NotCurrentCall) => {
                (Err(RespondFailure::NotPending), None)
            }
            Err(RespondError::OtherConnection) => (Err(RespondFailure::OtherConnection), None),
            Err(RespondError::Frame(FrameError::Write(failed))) => (
                Err(RespondFailure::Failed),
                Some(End::Again(Down::SendFailed(failed), Wait::Backoff)),
            ),
        };
        let _ = request.done.send(outcome);
        end
    }

    /// Sends one own frame through the sink; one the socket couldn't send ends the connection.
    async fn send<F: OwnFrame + Sync>(&mut self, frame: &F) -> Result<Sent, End> {
        self.sink.send(frame).await.map_err(|error| match error {
            FrameError::Write(failed) => End::Again(Down::SendFailed(failed), Wait::Backoff),
        })
    }

    async fn ping(&mut self) -> Option<End> {
        match self.send(&Ping::new()).await {
            Ok(sent) => {
                self.pong_by = sent.ack_id().map(|id| (id, Instant::now() + self.pong_timeout));
                None
            }
            Err(end) => Some(end),
        }
    }

    async fn on_frame<T, K>(
        &mut self,
        frame: Option<Result<Message, tungstenite::Error>>,
        run: &mut Run<T, K>,
    ) -> Option<End>
    where
        T: EndpointRegistrar + Send + 'static,
        K: TrouterCredentials + 'static,
    {
        match frame {
            Some(Ok(Message::Text(text))) => {
                // the frame in full at `debug`: its record redacts every credential field
                log::debug!(
                    target: teams_log::wire::FRAME_TARGET,
                    "{}",
                    teams_log::wire::Frame {
                        direction: teams_log::wire::Direction::Received,
                        text: text.as_str(),
                    }
                );
                self.on_text(text.as_str(), run).await
            }
            Some(Ok(Message::Close(_)) | Err(_)) | None => Some(End::Again(Down::Closed, Wait::Backoff)),
            Some(Ok(_)) => None,
        }
    }

    async fn on_text<T, K>(&mut self, text: &str, run: &mut Run<T, K>) -> Option<End>
    where
        T: EndpointRegistrar + Send + 'static,
        K: TrouterCredentials + 'static,
    {
        match frames::decode(text) {
            Ok(Incoming::Connect) if !self.authenticated => self.authenticate(run).await,
            Ok(Incoming::Event(event)) => {
                self.on_event(&event, run);
                None
            }
            Ok(Incoming::Ack { id, .. }) => {
                if self.pong_by.is_some_and(|(pending, _)| pending == id) {
                    self.pong_by = None;
                }
                None
            }
            Ok(Incoming::Delivery(raw)) => {
                let event = match &run.trouter.calls {
                    Some(calls) => self.on_call_delivery(&raw, calls),
                    None => self.on_delivery(&raw),
                };
                run.emit(event);
                None
            }
            Ok(Incoming::Disconnect { reason }) => Some(self.disconnected(reason, run)),
            Ok(other) => {
                log::debug!("trouter: a packet left alone: {other:?}");
                None
            }
            Err(error) => {
                log::debug!("trouter: an unreadable packet: {error}");
                None
            }
        }
    }

    /// The socket's first frame, with the token the session holds now (asked again: the connect's may be stale by
    /// the `1::`). A session that has none any more (signed out) stops the client and any other failure ends the
    /// connection, nothing sent either way; a stop while it is asked ends it at once.
    async fn authenticate<T, K>(&mut self, run: &mut Run<T, K>) -> Option<End>
    where
        T: EndpointRegistrar + Send + 'static,
        K: TrouterCredentials + 'static,
    {
        let asked = tokio::select! {
            biased;
            _ = &mut run.stop => return Some(End::Stop(Stop::Requested)),
            asked = run.trouter.credentials.ic3() => asked,
        };
        let token = match run.token(asked) {
            Ok(token) => token,
            Err(end) => return Some(end),
        };
        if token.token.expose() != self.token.token.expose() {
            self.renew = renewable(&token, self.renew_lead, run.trouter.clock.now());
            self.token = token;
        }
        self.authenticated = true;
        let authenticate = match run.trouter.stack() {
            Stack::Messaging => Authenticate::new(&self.token.token, self.params.clone()),
            Stack::Calling => Authenticate::calling(&self.token.token, self.params.clone()),
        };
        self.send(&authenticate).await.err()
    }

    fn on_event<T, K>(&mut self, event: &Event, run: &mut Run<T, K>)
    where
        T: EndpointRegistrar + Send + 'static,
        K: TrouterCredentials + 'static,
    {
        if let Some(connected) = Connected::from_event(event) {
            if let Some(calls) = &run.trouter.calls
                && !calls.state.connected(&self.connection, &connected.push_url)
            {
                log::warn!("calling endpoint did not provide a valid callback root");
            }
            run.connected(&connected);
            self.register(&connected, run);
            self.endpoint = Some(connected.id);
        } else if let Some(indicators) = frames::message_loss(event).filter(|found| !found.is_empty()) {
            if run.trouter.calls.is_some() {
                // the calling stack keeps no chat state to catch up: acknowledged at once, as its sockets did
                self.acks.push(Box::pin(std::future::ready(Some(indicators))));
                return;
            }
            let (ack, acked) = oneshot::channel();
            let handed_back = indicators.clone();
            self.acks
                .push(Box::pin(async move { acked.await.ok().map(|()| handed_back) }));
            run.emit(PushEvent::MessageLoss(MessageLoss {
                indicators,
                ack: Some(ack),
            }));
        } else {
            log::debug!("trouter: the service's event {:?} left alone", event.name);
        }
    }

    /// Registers the endpoint's push URL (the stack's registration), replacing the last loop.
    fn register<T, K>(&mut self, connected: &Connected, run: &Run<T, K>)
    where
        T: EndpointRegistrar + Send + 'static,
    {
        self.registrar = None;
        let (epid, push_url) = (&run.trouter.epid, &connected.push_url);
        let registration = match run.trouter.stack() {
            Stack::Messaging => {
                EndpointRegistration::messaging_in(&run.trouter.cloud, epid, push_url).map(Registration::Messaging)
            }
            Stack::Calling => {
                CallingRegistration::new_in(&run.trouter.cloud, epid, push_url).map(Registration::Calling)
            }
        };
        match registration {
            Some(registration) => {
                self.registrar = Some(Registrar::start(
                    Arc::clone(&run.trouter.transport),
                    registration,
                    self.registration,
                    self.reports_to.clone(),
                ));
            }
            None => run.emit(PushEvent::Registration(RegistrationReport::Failed(
                "the push URL isn't a Trouter push URL".to_owned(),
            ))),
        }
    }

    /// What a delivery is to the client: never answered, whatever it is.
    fn on_delivery(&self, raw: &RawDelivery) -> PushEvent {
        let incoming_call =
            serde_json::from_str::<Value>(&raw.body).is_ok_and(|body| body.get("callNotification").is_some());
        if incoming_call {
            return PushEvent::Ignored(Ignored::IncomingCall);
        }
        let prefix = if self.cloud.is_microsoft() { "" } else { "/trouter" };
        let at = |path: &str| {
            self.endpoint
                .as_ref()
                .is_some_and(|endpoint| raw.url == format!("{prefix}/v4/f/{endpoint}/{path}"))
        };
        let ours = at("messaging");
        // the delivery's number and kind of path only, never its body or headers
        log::debug!(
            "trouter: delivery {} stored={} ours={ours} body_bytes={}",
            raw.id,
            raw.is_stored(),
            raw.body.len()
        );
        if ours {
            PushEvent::Delivery(Delivery::from_raw(raw))
        } else if at(PRESENCE_PUSH_PATH) {
            PresenceDelivery::from_raw(raw).map_or_else(
                || PushEvent::Ignored(Ignored::Undecodable { path: raw.url.clone() }),
                PushEvent::Presence,
            )
        } else {
            PushEvent::Ignored(Ignored::NotOurs { path: raw.url.clone() })
        }
    }

    fn on_call_delivery(&self, raw: &RawDelivery, calls: &CallStack) -> PushEvent {
        let prefix = if self.cloud.is_microsoft() { "" } else { "/trouter" };
        let root = self
            .endpoint
            .as_ref()
            .map(|endpoint| format!("{prefix}/v4/f/{endpoint}/"));
        let ours = root.as_deref().is_some_and(|root| {
            raw.url == root
                || raw
                    .url
                    .strip_prefix(root)
                    .is_some_and(|rest| rest.starts_with("callAgent/"))
        });
        // the delivery's number and kind of path only, never its body or headers
        log::debug!(
            "trouter: call delivery {} ours={ours} body_bytes={}",
            raw.id,
            raw.body.len()
        );
        if !ours {
            return PushEvent::Ignored(Ignored::NotOurs { path: raw.url.clone() });
        }
        let Some(delivery) = crate::calls::read_delivery(&self.connection, raw) else {
            log::info!("trouter: a call delivery whose body doesn't decode, ignored");
            return PushEvent::Ignored(Ignored::Undecodable { path: raw.url.clone() });
        };
        match calls.observed(&delivery) {
            Observed::Ignored => {
                log::info!(
                    "trouter: an incoming call not offered by the call tracker, ignored: {}",
                    teams_call::track::unread_invitation(&delivery)
                );
                PushEvent::Ignored(Ignored::IncomingCall)
            }
            observed => PushEvent::Call(CallDelivery::new(delivery, observed, self.responder.clone())),
        }
    }

    /// The service's disconnect, by its reason.
    fn disconnected<T, K>(&self, reason: Option<String>, run: &mut Run<T, K>) -> End {
        match reason.as_deref() {
            Some("unauthorized") => {
                run.need = Need::Replace(self.token.token.clone());
                End::Again(Down::ServiceDisconnect(reason), Wait::None)
            }
            Some("dup") => {
                // a second `dup` within the spacing: the cached regional host is given up, as the web client does
                let now = Instant::now();
                if run.last_dup.is_some_and(|last| now - last < IMMEDIATE_SPACING) {
                    run.cache = None;
                }
                run.last_dup = Some(now);
                End::Again(Down::ServiceDisconnect(reason), Wait::None)
            }
            Some("skypetoken-deprecated") => End::Stop(Stop::Deprecated),
            _ => End::Again(Down::ServiceDisconnect(reason), Wait::Backoff),
        }
    }

    /// Reconnects with a new token once the held one is within the lead of its expiry.
    async fn renew_check<T, K>(&mut self, run: &mut Run<T, K>) -> Option<End>
    where
        T: EndpointRegistrar + Send + 'static,
        K: TrouterCredentials + 'static,
    {
        let due = self
            .token
            .expires_at
            .checked_sub(self.renew_lead)
            .is_none_or(|due| run.trouter.clock.now() >= due);
        if !due {
            return None;
        }
        run.need = Need::Replace(self.token.token.clone());
        Some(self.reconnect_for_renewal().await)
    }

    /// Reconnects when the session announces another `ic3` token than the one held: the session's token is compared
    /// with the held one (never printed), since a replacement may expire no later than the token it replaced.
    async fn on_renewal<T, K>(
        &mut self,
        renewal: Result<Renewal, broadcast::error::RecvError>,
        run: &mut Run<T, K>,
    ) -> Option<End>
    where
        T: EndpointRegistrar + Send + 'static,
        K: TrouterCredentials + 'static,
    {
        let ask = match renewal {
            Ok(Renewal::AccessToken { resource, .. }) => resource == IC3_AUDIENCE,
            Ok(Renewal::Skypetoken { .. }) => false,
            // missed some: the session says whether its token is another one now
            Err(broadcast::error::RecvError::Lagged(_)) => true,
            Err(broadcast::error::RecvError::Closed) => {
                run.renewals_open = false;
                false
            }
        };
        let newer = ask
            && run
                .trouter
                .credentials
                .ic3()
                .await
                .is_ok_and(|current| current.token.expose() != self.token.token.expose());
        if !newer {
            return None;
        }
        run.need = Need::Current;
        Some(self.reconnect_for_renewal().await)
    }

    /// Reconnects for a new token: the disconnect (`0::`) only on an authenticated socket, so `user.authenticate`
    /// stays a socket's first frame.
    async fn reconnect_for_renewal(&mut self) -> End {
        if !self.authenticated {
            return End::Again(Down::Renewal, Wait::None);
        }
        match self.send(&Disconnect::new()).await {
            Err(End::Stop(stop)) => End::Stop(stop),
            _ => End::Again(Down::Renewal, Wait::None),
        }
    }
}

/// Whether `token`'s expiry is far enough off at `now` to renew it `lead` before it runs out.
fn renewable(token: &AccessToken, lead: Duration, now: SystemTime) -> bool {
    token.expires_at.checked_sub(lead).is_some_and(|due| due > now)
}

/// An instant no wait reaches (a guarded `select!` branch still needs one).
fn far_future() -> Instant {
    Instant::now() + Duration::from_hours(24 * 365)
}

fn millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX))
}

/// A random GUID (version 4), lowercase with dashes.
fn guid() -> Option<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes.iter().fold(String::with_capacity(32), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}
