//! The mock's Trouter (M1 Task 16): a WebSocket server on loopback speaking the push channel as M1 Task 14's
//! captures recorded it (`docs/protocol/trouter.md`, the synthetic `tests/fixtures/trouter/` fixtures), and the
//! registrar's registration of an endpoint, on the chat service mock's own origin.
//!
//! - **Connect**: `GET /v4/c?…` (the query kept for the test to read); the mock answers `1::` and waits
//!   [`TrouterMock::set_auth_timeout`] for `user.authenticate` as the first frame. Late, it closes with
//!   `0:::{"reason":"timeout"}` (seen live); with headers other than the messaging stack's three, or a token it didn't
//!   hand out, with `0:::{"reason":"unauthorized"}` (the bundle's reason).
//! - **`trouter.connected`**, shaped like `session.json`'s: the endpoint id (the one the `connectparams`' `sr` names
//!   when the account had it before, else a new one), push URLs and the `reconnectUrl` on a made-up regional host
//!   (`mock-zz-01`), and `connectparams`. A connect of an endpoint seen before also gets the fixture's
//!   `trouter.message_loss` report (as on reconnects live).
//! - **Keep-alives**: `5:<n>+::{"name":"ping"}` is answered `6:::<n>+["pong"]` (unless [`TrouterMock::set_pongs`]
//!   turned that off), any other acknowledged event `6:::<n>+[]`; `0::` closes the socket.
//! - **Deliveries**: once a registration names the socket's endpoint, the chat service mock's queued events for the
//!   account are taken and delivered as `3:::{…}` frames shaped like `deliveries.json`'s: a message as `NewMessage`,
//!   typing as `NewMessage` of `Control/Typing` (not stored), an edit, a delete or a reaction as `MessageUpdate`, the
//!   message as a read gives it. Read receipts and chat creation stay queued
//!   ([`crate::ChatServiceMock::take_events`]): no capture showed what another member is told of them. Every delivered
//!   event but typing is **stored**: each later connect of the account replays them (`"mstore":true`, the same `id`),
//!   as the service replays unanswered deliveries; the client never answers, so they never leave the store.
//! - **Registrar**: `POST /registrar/prod/V2/registrations` with the account's token, `X-MS-Migration: True` and
//!   either stack's registration (`registration.json`'s shape: `TeamsCDLWebWorker` or `SkypeSpacesWeb`) is answered
//!   `202` with no body and recorded; another body is a `400`, a missing credential a `401`. Chat events go only to an
//!   endpoint the messaging stack registered.
//! - **The calling stack's socket** (a connect whose `tc` names `SkypeSpaces`): its `user.authenticate` carries only
//!   `Authorization` and `X-MS-Migration` (the calling stack's two headers), else it is `unauthorized`. Once the
//!   calling stack registered its endpoint, a test delivers to it an incoming call's invitation
//!   ([`TrouterMock::deliver_call_invitation`], `tests/fixtures/calling/chat/deliveries.json`'s 1:1 shape at the
//!   endpoint's root; [`TrouterMock::call_invitation`] and [`TrouterMock::group_call_invitation`] build either shape,
//!   changed or addressed to someone else, for [`TrouterMock::deliver_call`]) or any call delivery at a path under its push URL ([`TrouterMock::deliver_call`]; one under
//!   `callAgent/` gzip'd in base64 with `X-Microsoft-Skype-Content-Encoding: gzip`, as the call controller sends it),
//!   each with the `trouter-request`, chain and message ids its response must echo. The client's responses (`3:::`) are recorded
//!   with its other frames.
//!
//! Test hooks: every socket's record ([`TrouterMock::sockets`]: the account, the endpoint, the query, the stack, every
//! frame the client sent), dropping an account's sockets, a service disconnect with a reason, an incoming call's
//! delivery, others' presence as UPS delivers it ([`TrouterMock::deliver_presence`]), sockets closed before their
//! `trouter.connected`, a late `1::`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Map, Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request as Handshake, Response as HandshakeAnswer,
};
use wiremock::{Request, ResponseTemplate};

use crate::world::{State, iso_time};
use crate::{ChatEvent, EventKind};

const SESSION: &str = include_str!("../../../tests/fixtures/trouter/session.json");
const DELIVERIES: &str = include_str!("../../../tests/fixtures/trouter/deliveries.json");
const CALL_DELIVERIES: &str = include_str!("../../../tests/fixtures/calling/chat/deliveries.json");

/// The made-up regional name of the mock's Trouter hosts.
pub const REGIONAL_NAME: &str = "mock-zz-01";

/// The messaging stack's app on the registrar.
pub const MESSAGING_APP: &str = "TeamsCDLWebWorker";

/// The calling stack's app on the registrar.
pub const CALLING_APP: &str = "SkypeSpacesWeb";

/// An endpoint registered with the registrar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    /// The account the registration's token is.
    pub account: String,
    /// The stack that registered it: [`MESSAGING_APP`] or [`CALLING_APP`].
    pub app_id: String,
    /// The `registrationId` (the client's `epid`).
    pub registration_id: String,
    /// The push URL it names.
    pub path: String,
    pub ttl: u64,
}

/// What the mock knows of one socket.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SocketRecord {
    /// The connect's query, as it came.
    pub query: String,
    /// Whether the connect named the calling stack's client (`tc`'s `ua` `SkypeSpaces`).
    pub calling: bool,
    /// The account its `user.authenticate` named, once it did.
    pub account: Option<String>,
    /// The endpoint id the mock handed over.
    pub endpoint: Option<String>,
    /// Whether its `user.authenticate` carried `connectparams` (a reconnect).
    pub reconnect: bool,
    /// Every text frame the client sent on it, in order.
    pub frames: Vec<String>,
    /// Still open.
    pub open: bool,
}

/// A command a test gives an account's open sockets.
#[derive(Clone, Debug)]
enum Command {
    /// Close the TCP connection without a word.
    Drop,
    /// `0:::{"reason":…}`, then close.
    Disconnect(String),
    /// A delivery of this body at this path under the endpoint's push URL.
    Deliver { path: String, body: String },
    /// A call delivery to a calling stack's socket whose endpoint the calling stack registered: its id, its path under
    /// the push URL, its body.
    DeliverCall { id: i64, path: String, body: String },
    /// A presence delivery from UPS to a socket that doesn't take calls: its id, its body, whether it is a stored
    /// replay.
    DeliverPresence { id: i64, body: String, stored: bool },
}

/// One delivery the account's endpoints get again on every connect.
#[derive(Clone, Debug)]
struct Stored {
    id: u64,
    frame_body: Value,
    headers: Map<String, Value>,
}

/// The Trouter mock's own state, beside the chat world's.
#[derive(Default)]
struct Sockets {
    records: Vec<SocketRecord>,
    commands: Vec<(usize, String, mpsc::UnboundedSender<Command>)>,
    /// Endpoint ids handed out, per account.
    endpoints: BTreeMap<String, Vec<String>>,
    stored: BTreeMap<String, Vec<Stored>>,
    next_endpoint: u64,
    next_delivery: u64,
    no_pongs: bool,
    auth_timeout: Option<Duration>,
    /// How many of the next authenticated sockets are closed without a `trouter.connected`.
    withheld: usize,
    /// How long the next socket waits before its `1::`.
    hello_delay: Option<Duration>,
}

struct Shared {
    world: Arc<Mutex<State>>,
    sockets: Mutex<Sockets>,
    templates: Templates,
}

impl Shared {
    fn push_prefix(&self) -> &'static str {
        if self.world().cloud.is_microsoft() {
            ""
        } else {
            "/trouter"
        }
    }

    fn sockets(&self) -> MutexGuard<'_, Sockets> {
        self.sockets.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn world(&self) -> MutexGuard<'_, State> {
        self.world.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The frames the mock's answers are shaped from.
struct Templates {
    connected: Value,
    message_loss: String,
    headers: Map<String, Value>,
}

impl Templates {
    fn load() -> std::io::Result<Self> {
        let invalid =
            |what: &str| std::io::Error::other(format!("a Trouter fixture isn't as the mock expects: {what}"));
        let session: Value = serde_json::from_str(SESSION).map_err(|_| invalid("session.json"))?;
        let frame = |name: &str| {
            session["frames"]
                .as_array()
                .and_then(|frames| frames.iter().find(|frame| frame["name"] == name))
                .and_then(|frame| frame["frame"].as_str())
                .map(str::to_owned)
        };
        let connected = frame("the endpoint and its push URLs")
            .and_then(|text| serde_json::from_str::<Value>(text.split_once("::")?.1).ok())
            .ok_or_else(|| invalid("session.json: trouter.connected"))?;
        let message_loss = frame("the service's report of what the endpoint missed while away (sent more than once)")
            .ok_or_else(|| invalid("session.json: trouter.message_loss"))?;
        let deliveries: Value = serde_json::from_str(DELIVERIES).map_err(|_| invalid("deliveries.json"))?;
        let headers = deliveries["deliveries"][1]["frame"]
            .as_str()
            .and_then(|text| text.strip_prefix("3:::"))
            .and_then(|json| serde_json::from_str::<Value>(json).ok())
            .and_then(|delivery| delivery["headers"].as_object().cloned())
            .ok_or_else(|| invalid("deliveries.json: a message's headers"))?;
        Ok(Self {
            connected,
            message_loss,
            headers,
        })
    }
}

/// The running Trouter mock: a WebSocket server on loopback on its own thread. Dropping it stops serving.
pub struct TrouterMock {
    origin: String,
    shared: Arc<Shared>,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for TrouterMock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrouterMock")
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl Drop for TrouterMock {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl TrouterMock {
    /// Starts the mock's Trouter over the chat world `world`, on a free loopback port.
    pub(crate) fn start(world: Arc<Mutex<State>>) -> std::io::Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let origin = format!("ws://{}", listener.local_addr()?);
        let shared = Arc::new(Shared {
            world,
            sockets: Mutex::default(),
            templates: Templates::load()?,
        });
        let (shutdown, stopped) = oneshot::channel();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let serving = Arc::clone(&shared);
        let thread = std::thread::spawn(move || {
            runtime.block_on(async move {
                let Ok(listener) = TcpListener::from_std(listener) else {
                    return;
                };
                tokio::select! {
                    () = accept(listener, serving) => {}
                    _ = stopped => {}
                }
            });
        });
        Ok(Self {
            origin,
            shared,
            shutdown: Some(shutdown),
            thread: Some(thread),
        })
    }

    /// The origin clients connect to (`ws://127.0.0.1:<port>`), for the `test-support` mock origin.
    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Every socket so far, in the order they connected.
    #[must_use]
    pub fn sockets(&self) -> Vec<SocketRecord> {
        self.shared.sockets().records.clone()
    }

    /// The endpoints registered so far, in order.
    #[must_use]
    pub fn registrations(&self) -> Vec<Registration> {
        self.shared.world().registrations.clone()
    }

    /// Removes `account`'s saved chat deliveries so later connections cannot replay them.
    pub fn clear_stored_deliveries(&self, account: &str) {
        self.shared.sockets().stored.remove(account);
    }

    /// Whether pings are answered (they are unless this turned it off).
    pub fn set_pongs(&self, answer: bool) {
        self.shared.sockets().no_pongs = !answer;
    }

    /// How long the mock waits for `user.authenticate` after its `1::` (2 s unless this changed it).
    pub fn set_auth_timeout(&self, timeout: Duration) {
        self.shared.sockets().auth_timeout = Some(timeout);
    }

    /// Closes each of the next `count` sockets right after its `user.authenticate` (recorded and checked as usual),
    /// without a `trouter.connected`: a regional host that takes the socket and never says connected.
    pub fn withhold_connected(&self, count: usize) {
        self.shared.sockets().withheld = count;
    }

    /// Makes the next socket wait `delay` after its handshake before the mock's `1::`.
    pub fn delay_hello_once(&self, delay: Duration) {
        self.shared.sockets().hello_delay = Some(delay);
    }

    /// Drops `account`'s open sockets without a word (the network failing).
    pub fn drop_sockets(&self, account: &str) {
        self.command(account, &Command::Drop);
    }

    /// Disconnects `account`'s open sockets with `reason` (`0:::{"reason":…}`), then closes them.
    pub fn disconnect(&self, account: &str, reason: &str) {
        self.command(account, &Command::Disconnect(reason.to_owned()));
    }

    /// Delivers an incoming call to `account`'s open sockets: a body with a `callNotification` (the bundle's sign of
    /// one) at a call agent's path under the push URL. The client must never answer it.
    pub fn deliver_incoming_call(&self, account: &str) {
        let body = json!({"callNotification": {"from": "8:orgid:mock-caller", "callId": "mock-call-1"}});
        self.command(
            account,
            &Command::Deliver {
                path: "callAgent/mock-agent/1/incomingCall".to_owned(),
                body: body.to_string(),
            },
        );
    }

    /// Delivers to `to`'s calling sockets an incoming 1:1 call from `from`: the fixture's invitation
    /// (`tests/fixtures/calling/chat/deliveries.json`, its `gp` base64 JSON) at the endpoint's root, its parties
    /// `from` and `to` (work accounts' MRIs). The delivery's id, which its response must name.
    #[must_use = "a response names the delivery's id"]
    pub fn deliver_call_invitation(&self, to: &str, from: &str) -> i64 {
        self.deliver_call(to, "", &Self::call_invitation(from, to, |_| {}))
    }

    /// The fixture's 1:1 invitation from `from` to `to` (work accounts' MRIs) as a delivery's body, its decoded `gp`
    /// changed by `edit` before it is encoded (base64 JSON, as on the wire): for [`TrouterMock::deliver_call`] to any
    /// account's calling sockets, an invitation addressed to someone else or in a shape the tracker doesn't read.
    #[must_use]
    pub fn call_invitation(from: &str, to: &str, edit: impl FnOnce(&mut Value)) -> Value {
        invitation(0, from, to, edit)
    }

    /// The fixture's group call invitation (`evt` 109) from `from` to `to` in the group chat `chat` (its
    /// `19:…@thread.v2` id) as a delivery's body, its decoded `gp` changed by `edit` before it is encoded.
    #[must_use]
    pub fn group_call_invitation(from: &str, to: &str, chat: &str, edit: impl FnOnce(&mut Value)) -> Value {
        invitation(1, from, to, |gp| {
            gp["groupChat"]["threadId"] = Value::from(chat);
            edit(gp);
        })
    }

    /// Delivers `body` to `account`'s calling sockets whose endpoint the calling stack registered, at `path` under the
    /// push URL (`""` for its root, `callAgent/…` for a callback), with a `trouter-request`, chain and message id of its
    /// own. The delivery's id.
    #[must_use = "a response names the delivery's id"]
    pub fn deliver_call(&self, account: &str, path: &str, body: &Value) -> i64 {
        let id = {
            let mut sockets = self.shared.sockets();
            sockets.next_delivery += 1;
            i64::try_from(2_000_000 + sockets.next_delivery).unwrap_or(i64::MAX)
        };
        self.command(
            account,
            &Command::DeliverCall {
                id,
                path: path.to_owned(),
                body: body.to_string(),
            },
        );
        id
    }

    /// Delivers `body` to `account`'s open messaging sockets as UPS delivers others' presence once the endpoint
    /// subscribed to it (`tests/fixtures/trouter/presence-deliveries.json`'s shape, at the endpoint's
    /// `unifiedPresenceService` path), a stored replay (`"mstore":true`) when `stored`. The mock's UPS sends none by
    /// itself: a test delivers what the subscription would bring.
    pub fn deliver_presence(&self, account: &str, body: &Value, stored: bool) {
        let id = {
            let mut sockets = self.shared.sockets();
            sockets.next_delivery += 1;
            i64::try_from(3_000_000 + sockets.next_delivery).unwrap_or(i64::MAX)
        };
        self.command(
            account,
            &Command::DeliverPresence {
                id,
                body: body.to_string(),
                stored,
            },
        );
    }

    /// Delivers a synthetic chat `EventMessage` on the account's messaging sockets, as the captured messaging
    /// deliveries do. Calling sockets receive none, and no delivery response is requested or produced.
    pub fn deliver_chat(&self, account: &str, body: &Value) {
        let sockets = self.shared.sockets();
        for (index, owner, commands) in &sockets.commands {
            if owner == account && sockets.records.get(*index).is_some_and(|record| !record.calling) {
                let _ = commands.send(Command::Deliver {
                    path: "messaging".to_owned(),
                    body: body.to_string(),
                });
            }
        }
    }

    fn command(&self, account: &str, command: &Command) {
        let sockets = self.shared.sockets();
        for (_, owner, commands) in &sockets.commands {
            if owner == account {
                let _ = commands.send(command.clone());
            }
        }
    }
}

async fn accept(listener: TcpListener, shared: Arc<Shared>) {
    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(serve(stream, Arc::clone(&shared)));
    }
}

/// One socket, from its handshake to its close.
async fn serve(stream: TcpStream, shared: Arc<Shared>) {
    let mut query = String::new();
    let Ok(mut socket) = tokio_tungstenite::accept_hdr_async(stream, QueryOf(&mut query)).await else {
        return;
    };
    let (index, hello_delay) = {
        let mut sockets = shared.sockets();
        let calling = names_calling_stack(&query);
        sockets.records.push(SocketRecord {
            query,
            calling,
            open: true,
            ..SocketRecord::default()
        });
        (sockets.records.len() - 1, sockets.hello_delay.take())
    };
    if let Some(delay) = hello_delay {
        tokio::time::sleep(delay).await;
    }
    if socket.send(Message::text("1::")).await.is_ok()
        && let Some(account) = authenticate(&mut socket, &shared, index).await
    {
        connected(socket, &shared, index, &account).await;
    }
    let mut sockets = shared.sockets();
    if let Some(record) = sockets.records.get_mut(index) {
        record.open = false;
    }
    sockets.commands.retain(|(socket, ..)| *socket != index);
}

type Socket = WebSocketStream<TcpStream>;

/// Whether a connect's query names the calling stack's client: its `tc`'s `ua` is `SkypeSpaces`.
fn names_calling_stack(query: &str) -> bool {
    query
        .split('&')
        .filter_map(|pair| pair.strip_prefix("tc="))
        .filter_map(|tc| percent_encoding::percent_decode_str(tc).decode_utf8().ok())
        .filter_map(|tc| serde_json::from_str::<Value>(&tc).ok())
        .any(|tc| tc["ua"] == "SkypeSpaces")
}

/// Keeps the connect's query, from the handshake.
struct QueryOf<'a>(&'a mut String);

impl Callback for QueryOf<'_> {
    fn on_request(self, request: &Handshake, answer: HandshakeAnswer) -> Result<HandshakeAnswer, ErrorResponse> {
        request.uri().query().unwrap_or_default().clone_into(self.0);
        Ok(answer)
    }
}

/// The next text frame from the client, recorded; `None` once the socket closed.
async fn next_text(socket: &mut Socket, shared: &Shared, index: usize) -> Option<String> {
    while let Some(Ok(message)) = socket.next().await {
        match message {
            Message::Text(text) => {
                let text = text.as_str().to_owned();
                let account = shared.sockets().records.get_mut(index).and_then(|record| {
                    record.frames.push(text.clone());
                    record.account.clone()
                });
                if let Some(account) = account.filter(|_| text.starts_with("3:::")) {
                    crate::calling::observe_response(&mut shared.world(), &account, &text);
                }
                return Some(text);
            }
            Message::Close(_) => return None,
            _ => {}
        }
    }
    None
}

/// Waits for `user.authenticate` as the first frame and checks it: the account whose token it carries, with its
/// endpoint id; `None` (the socket told why and closed) otherwise.
async fn authenticate(socket: &mut Socket, shared: &Shared, index: usize) -> Option<String> {
    let timeout = shared.sockets().auth_timeout.unwrap_or(Duration::from_secs(2));
    let first = tokio::time::timeout(timeout, next_text(socket, shared, index)).await;
    let Ok(Some(first)) = first else {
        let _ = socket.send(Message::text(r#"0:::{"reason":"timeout"}"#)).await;
        return None;
    };
    let calling = shared.sockets().records.get(index).is_some_and(|record| record.calling);
    let Some((account, endpoint, reconnect)) = authenticated(&first, shared, calling) else {
        let _ = socket.send(Message::text(r#"0:::{"reason":"unauthorized"}"#)).await;
        return None;
    };
    if let Some(record) = shared.sockets().records.get_mut(index) {
        record.account = Some(account.clone());
        record.endpoint = Some(endpoint.clone());
        record.reconnect = reconnect;
    }
    let withheld = {
        let mut sockets = shared.sockets();
        let withheld = sockets.withheld > 0;
        sockets.withheld = sockets.withheld.saturating_sub(1);
        withheld
    };
    if withheld {
        let _ = socket.close(None).await;
        return None;
    }
    let seen_before = send_connected(socket, shared, &account, &endpoint).await?;
    if seen_before {
        let report = shared.templates.message_loss.clone();
        socket.send(Message::text(report)).await.ok()?;
    }
    Some(account)
}

/// The account, the endpoint id and whether the frame carried `connectparams`, if `frame` is the socket's stack's
/// `user.authenticate` (the messaging stack's three headers, or on a `calling` socket the calling stack's two) with a
/// token the mock handed out.
fn authenticated(frame: &str, shared: &Shared, calling: bool) -> Option<(String, String, bool)> {
    let event: Value = serde_json::from_str(frame.strip_prefix("5:::")?).ok()?;
    let argument = event["args"].as_array().filter(|args| args.len() == 1)?.first()?;
    let headers = argument["headers"].as_object()?;
    let token = headers.get("Authorization")?.as_str()?.strip_prefix("Bearer ")?;
    let stack_headers = if calling {
        headers.len() == 2
    } else {
        headers.len() == 3 && headers.get("X-Ms-Test-User") == Some(&json!("False"))
    };
    let shape =
        event["name"] == "user.authenticate" && stack_headers && headers.get("X-MS-Migration") == Some(&json!("True"));
    if !shape {
        return None;
    }
    let account = shared.world().account_by_token(token)?.mri.clone();
    let sr = argument["connectparams"]["sr"].as_str();
    let mut sockets = shared.sockets();
    let known = sockets.endpoints.entry(account.clone()).or_default().clone();
    let endpoint = if let Some(sr) = sr.filter(|sr| known.iter().any(|known| known == sr)) {
        sr.to_owned()
    } else {
        sockets.next_endpoint += 1;
        let endpoint = format!("MockEndpoint{:06}", sockets.next_endpoint);
        sockets
            .endpoints
            .entry(account.clone())
            .or_default()
            .push(endpoint.clone());
        endpoint
    };
    Some((account, endpoint, !argument["connectparams"].is_null()))
}

/// Sends `trouter.connected` for `endpoint`: whether `account` had connected with it before.
async fn send_connected(socket: &mut Socket, shared: &Shared, account: &str, endpoint: &str) -> Option<bool> {
    let seen_before = shared
        .sockets()
        .records
        .iter()
        .filter(|record| record.account.as_deref() == Some(account) && record.endpoint.as_deref() == Some(endpoint))
        .count()
        > 1;
    let mut event = shared.templates.connected.clone();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let host = |kind: &str| format!("{REGIONAL_NAME}-{kind}.trouter.teams.microsoft.com");
    let info = &mut event["args"][0];
    info["id"] = json!(endpoint);
    info["ccid"] = json!(endpoint.trim_start_matches("Mock"));
    info["url"] = json!(format!("https://{}:8443/v4/f/{endpoint}/", host("f")));
    info["surl"] = json!(format!("https://{}:3443/v4/f/{endpoint}/", host("f")));
    info["curlb"] = json!(format!("https://{}:443", host("t")));
    info["healthUrl"] = json!(format!("https://{}:443/v4/h", host("t")));
    info["reconnectUrl"] = json!(format!("wss://{}:443/v4/c", host("t")));
    info["socketio"] = json!(format!("https://{}:443/", host("t")));
    if let Some(origin) = shared.world().cloud.origin() {
        let base = origin.as_str().trim_end_matches('/');
        info["url"] = json!(format!("{base}/trouter/v4/f/{endpoint}/"));
        info["surl"] = info["url"].clone();
        info["curlb"] = json!(base);
        info["healthUrl"] = json!(format!("{base}/trouter/v4/h"));
        info["reconnectUrl"] = json!(format!("wss://{}/trouter/v4/c", &base[8..]));
        info["socketio"] = json!(format!("{base}/trouter/"));
    }
    info["connectparams"]["sr"] = json!(endpoint);
    info["connectparams"]["sp"] = json!(REGIONAL_NAME);
    info["connectparams"]["st"] = json!((now * 1000).to_string());
    info["connectparams"]["se"] = json!(((now + 7 * 86_400) * 1000).to_string());
    socket
        .send(Message::text(format!("5:1::{event}")))
        .await
        .ok()
        .map(|()| seen_before)
}

/// An authenticated socket: keep-alives answered, deliveries sent once the endpoint is registered, commands carried
/// out, until the client or a command closes it.
async fn connected(mut socket: Socket, shared: &Shared, index: usize, account: &str) {
    let (commands, mut commanded) = mpsc::unbounded_channel();
    shared.sockets().commands.push((index, account.to_owned(), commands));
    let mut changes = shared.world().changes.subscribe();
    let (endpoint, calling) = shared
        .sockets()
        .records
        .get(index)
        .map(|record| (record.endpoint.clone().unwrap_or_default(), record.calling))
        .unwrap_or_default();
    let mut replayed = false;
    loop {
        if !replayed && registered(shared, account, &endpoint, MESSAGING_APP) {
            replayed = true;
            if replay(&mut socket, shared, account, &endpoint).await.is_none() {
                return;
            }
        }
        if replayed && deliver_queued(&mut socket, shared, account, &endpoint).await.is_none() {
            return;
        }
        if calling
            && registered(shared, account, &endpoint, CALLING_APP)
            && deliver_calls(&mut socket, shared, &endpoint).await.is_none()
        {
            return;
        }
        tokio::select! {
            text = next_text(&mut socket, shared, index) => {
                let Some(text) = text else { return };
                if answer(&mut socket, shared, &text).await.is_none() {
                    return;
                }
            }
            command = commanded.recv() => {
                let Some(command) = command else { return };
                let to_calls = calling && registered(shared, account, &endpoint, CALLING_APP);
                if carry_out(&mut socket, &endpoint, to_calls, command, shared.push_prefix()).await.is_none() {
                    return;
                }
            }
            woke = changes.changed() => {
                if woke.is_err() {
                    return;
                }
            }
        }
    }
}

/// Answers a client frame: a ping's pong, any other acknowledged event's empty acknowledgement; `None` when the
/// client disconnected (`0::`) or the socket failed.
async fn answer(socket: &mut Socket, shared: &Shared, text: &str) -> Option<()> {
    if text == "0::" {
        return None;
    }
    let Some((id, event)) = text.strip_prefix("5:").and_then(|rest| rest.split_once("+::")) else {
        return Some(());
    };
    let ping = event == r#"{"name":"ping"}"#;
    if ping && shared.sockets().no_pongs {
        return Some(());
    }
    let ack = if ping {
        format!(r#"6:::{id}+["pong"]"#)
    } else {
        format!("6:::{id}+[]")
    };
    socket.send(Message::text(ack)).await.ok()
}

/// Carries out `command` on a socket of `endpoint`; a call delivery only when the socket takes calls (`to_calls`: a
/// calling stack's socket whose endpoint the calling stack registered).
async fn carry_out(socket: &mut Socket, endpoint: &str, to_calls: bool, command: Command, prefix: &str) -> Option<()> {
    match command {
        Command::Drop => None,
        Command::Disconnect(reason) => {
            let _ = socket
                .send(Message::text(format!("0:::{}", json!({"reason": reason}))))
                .await;
            None
        }
        Command::Deliver { path, body } => {
            let delivery = json!({
                "id": 9_000_001,
                "method": "POST",
                "url": format!("{prefix}/v4/f/{endpoint}/{path}"),
                "headers": {"Content-Type": "application/json"},
                "body": body,
            });
            socket.send(Message::text(format!("3:::{delivery}"))).await.ok()
        }
        Command::DeliverPresence { .. } if to_calls => Some(()),
        Command::DeliverPresence { id, body, stored } => {
            let mut request = json!({"id": format!("00000000-0000-4000-8003-{:012x}", id.unsigned_abs()), "src": "192.0.2.1", "port": 3443});
            if stored {
                request["mstore"] = json!(true);
            }
            let delivery = json!({
                "id": id,
                "method": "POST",
                "url": format!("{prefix}/v4/f/{endpoint}/unifiedPresenceService"),
                "headers": {
                    "Host": format!("{REGIONAL_NAME}-t.trouter.teams.microsoft.com"),
                    "User-Agent": "Microsoft.Skype.Presence.App/1.0",
                    "Content-Type": "application/json; charset=utf-8",
                    "X-Microsoft-Skype-Message-ID": "0",
                    "X-Trouter-Delivery-Control": "async; ttl=600",
                    "trouter-request": request.to_string(),
                    "Content-Length": body.len().to_string(),
                },
                "body": body,
            });
            socket.send(Message::text(format!("3:::{delivery}"))).await.ok()
        }
        Command::DeliverCall { .. } if !to_calls => Some(()),
        Command::DeliverCall { id, path, body } => {
            let guid = |kind: u64| format!("00000000-0000-4000-{kind:04x}-{:012x}", id.unsigned_abs());
            let request = json!({"id": guid(0x8002), "src": "192.0.2.1", "port": 3443});
            let mut headers = json!({
                "Host": format!("{REGIONAL_NAME}-t.trouter.teams.microsoft.com"),
                "User-Agent": "CallController/0.0.0.0",
                "Content-Type": "application/json; charset=utf-8",
                "trouter-request": request.to_string(),
                "X-Microsoft-Skype-Chain-ID": guid(0x8004),
                "X-Microsoft-Skype-Message-ID": guid(0x8001),
            });
            // the call controller's callbacks come gzip'd in base64, as captured; an invitation at the root as JSON
            let body = if path.starts_with("callAgent/") {
                headers["X-Microsoft-Skype-Content-Encoding"] = json!("gzip");
                gzip_base64(&body)
            } else {
                body
            };
            headers["Content-Length"] = json!(body.len().to_string());
            let delivery = json!({
                "id": id,
                "method": "POST",
                "url": format!("{prefix}/v4/f/{endpoint}/{path}"),
                "headers": headers,
                "body": body,
            });
            socket.send(Message::text(format!("3:::{delivery}"))).await.ok()
        }
    }
}

/// Takes the calling service's deliveries waiting for `endpoint` (`calling.rs`) and delivers them, in order.
async fn deliver_calls(socket: &mut Socket, shared: &Shared, endpoint: &str) -> Option<()> {
    let waiting: Vec<crate::calling::Outgoing> = {
        let mut world = shared.world();
        let (mine, others) = std::mem::take(&mut world.calling.outgoing)
            .into_iter()
            .partition(|delivery| delivery.endpoint == endpoint);
        world.calling.outgoing = others;
        mine
    };
    for delivery in waiting {
        let id = {
            let mut sockets = shared.sockets();
            sockets.next_delivery += 1;
            i64::try_from(2_000_000 + sockets.next_delivery).unwrap_or(i64::MAX)
        };
        let command = Command::DeliverCall {
            id,
            path: delivery.path,
            body: delivery.body.to_string(),
        };
        carry_out(socket, endpoint, true, command, shared.push_prefix()).await?;
    }
    Some(())
}

/// Whether a registration of `account` by the stack `app_id` names `endpoint`'s push URL.
fn registered(shared: &Shared, account: &str, endpoint: &str, app_id: &str) -> bool {
    let suffix = format!("/v4/f/{endpoint}/");
    shared.world().registrations.iter().any(|registration| {
        registration.account == account && registration.app_id == app_id && registration.path.ends_with(&suffix)
    })
}

/// The fixture's invitation `index` (0: a 1:1 call's, 1: a group call's) from `from` to `to` (work accounts' MRIs), its
/// `gp` changed by `edit`, then base64 JSON as on the wire, every other placeholder a made-up GUID.
pub(crate) fn invitation(index: usize, from: &str, to: &str, edit: impl FnOnce(&mut Value)) -> Value {
    use base64::Engine as _;
    let fixture: Value = serde_json::from_str(CALL_DELIVERIES).unwrap_or_default();
    let body = &fixture["invitations"][index]["body"];
    let object_id = |mri: &str| mri.rsplit(':').next().unwrap_or_default().to_owned();
    let gp = body["gp"]
        .to_string()
        .replace("<guid:a>", &object_id(from))
        .replace("<guid:b>", &object_id(to));
    let mut gp: Value = serde_json::from_str(&fill_guids(&gp)).unwrap_or_default();
    edit(&mut gp);
    let gp = gp.to_string();
    json!({
        "evt": body["evt"],
        "gp": base64::engine::general_purpose::STANDARD.encode(gp),
    })
}

/// `text` gzip'd, in base64: a call controller's delivery body.
fn gzip_base64(text: &str) -> String {
    use base64::Engine as _;
    use std::io::Write as _;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let compressed = encoder
        .write_all(text.as_bytes())
        .and_then(|()| encoder.finish())
        .unwrap_or_default();
    base64::engine::general_purpose::STANDARD.encode(compressed)
}

/// `text` with every `<guid:…>` placeholder left a made-up GUID, one per name.
pub(crate) fn fill_guids(text: &str) -> String {
    use std::fmt::Write as _;
    let mut names: Vec<String> = Vec::new();
    let mut filled = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<guid:") {
        let Some(end) = rest[start..].find('>') else {
            break;
        };
        let name = rest[start..=start + end].to_owned();
        let number = names.iter().position(|known| *known == name).unwrap_or_else(|| {
            names.push(name);
            names.len() - 1
        });
        filled.push_str(&rest[..start]);
        let _ = write!(filled, "00000000-0000-4000-8000-{:012x}", 0x0c00 + number);
        rest = &rest[start + end + 1..];
    }
    filled.push_str(rest);
    filled
}

/// Replays `account`'s stored deliveries, as stored ones.
async fn replay(socket: &mut Socket, shared: &Shared, account: &str, endpoint: &str) -> Option<()> {
    let stored = shared.sockets().stored.get(account).cloned().unwrap_or_default();
    for delivery in stored {
        let mut headers = delivery.headers.clone();
        let request = json!({"id": format!("00000000-0000-4000-8000-{:012x}", delivery.id), "src": "192.0.2.1",
            "port": 3443, "mstore": true, "flow": "messaging", "seq": delivery.id});
        headers.insert("trouter-request".to_owned(), json!(request.to_string()));
        headers.insert("Trouter-Timeout".to_owned(), json!("7500"));
        headers.insert("Connection".to_owned(), json!("keep-alive"));
        send_delivery(
            socket,
            endpoint,
            delivery.id,
            headers,
            &delivery.frame_body,
            shared.push_prefix(),
        )
        .await?;
    }
    Some(())
}

/// Takes `account`'s queued events that have a captured wire form and delivers them.
async fn deliver_queued(socket: &mut Socket, shared: &Shared, account: &str, endpoint: &str) -> Option<()> {
    let events = {
        let mut world = shared.world();
        let queued = world.events.remove(account).unwrap_or_default();
        let (deliverable, kept): (Vec<ChatEvent>, Vec<ChatEvent>) = queued
            .into_iter()
            .partition(|event| resource_type(event.kind).is_some());
        if !kept.is_empty() {
            world.events.insert(account.to_owned(), kept);
        }
        deliverable
    };
    // every event is numbered and stored before the first send, so a socket that fails halfway loses none: the
    // unsent ones come with the next connect's replay
    let own_base = {
        let world = shared.world();
        (!world.cloud.is_microsoft()).then(|| format!("{}/v1/users/ME", world.chat_base()))
    };
    let numbered: Vec<(u64, Value, bool)> = {
        let mut sockets = shared.sockets();
        events
            .iter()
            .map(|event| {
                let typing = event.kind == EventKind::Typing;
                sockets.next_delivery += 1;
                let id = 1_000_000 + sockets.next_delivery;
                let body = event_message(event, id, own_base.as_deref());
                if !typing {
                    sockets.stored.entry(account.to_owned()).or_default().push(Stored {
                        id,
                        frame_body: body.clone(),
                        headers: live_headers(shared, id, false),
                    });
                }
                (id, body, typing)
            })
            .collect()
    };
    for (id, body, typing) in numbered {
        send_delivery(
            socket,
            endpoint,
            id,
            live_headers(shared, id, typing),
            &body,
            shared.push_prefix(),
        )
        .await?;
    }
    Some(())
}

/// The headers of a live delivery: `deliveries.json`'s, with this delivery's ids (typing is never stored).
fn live_headers(shared: &Shared, id: u64, typing: bool) -> Map<String, Value> {
    let mut headers = shared.templates.headers.clone();
    let guid = |kind: u64| format!("00000000-0000-4000-{kind:04x}-{id:012x}");
    headers.insert("X-Microsoft-Skype-Message-ID".to_owned(), json!(guid(0x8001)));
    headers.insert("__RequestTunnelId__".to_owned(), json!(guid(0x8003)));
    let request = json!({"id": guid(0x8002), "src": "192.0.2.1", "port": 3443});
    headers.insert("trouter-request".to_owned(), json!(request.to_string()));
    if typing {
        headers.insert(
            "X-Trouter-Delivery-Control".to_owned(),
            json!("async; ttl=900; flow=messaging; prio=normal; resilience=nostorenoloss"),
        );
    }
    headers
}

async fn send_delivery(
    socket: &mut Socket,
    endpoint: &str,
    id: u64,
    mut headers: Map<String, Value>,
    body: &Value,
    prefix: &str,
) -> Option<()> {
    let body = body.to_string();
    headers.insert("Content-Length".to_owned(), json!(body.len().to_string()));
    let delivery = json!({
        "id": id,
        "method": "POST",
        "url": format!("{prefix}/v4/f/{endpoint}/messaging"),
        "headers": headers,
        "body": body,
    });
    socket.send(Message::text(format!("3:::{delivery}"))).await.ok()
}

/// The `resourceType` an event is delivered as; `None` for one no capture showed delivered to another member.
fn resource_type(kind: EventKind) -> Option<&'static str> {
    match kind {
        EventKind::Message | EventKind::Typing => Some("NewMessage"),
        EventKind::Edit | EventKind::Delete | EventKind::Reaction => Some("MessageUpdate"),
        EventKind::ConversationUpdate => Some("ConversationUpdate"),
        EventKind::ReadReceipt | EventKind::ChatCreated => None,
    }
}

/// The delivery's body (an `EventMessage`), `deliveries.json`'s shape around the event's message.
fn event_message(event: &ChatEvent, id: u64, own_base: Option<&str>) -> Value {
    const NOTIFICATIONS: &str = "https://notifications.skype.net/v1/users/ME";
    let base = own_base.unwrap_or(NOTIFICATIONS);
    let chat = &event.conversation;
    let conversation_link = format!("{base}/conversations/{chat}");
    if event.kind == EventKind::ConversationUpdate {
        return json!({
            "time": iso_time(now_ms(), 7),
            "type": "EventMessage",
            "resourceLink": conversation_link,
            "resourceType": "ConversationUpdate",
            "resource": event.resource,
            "isactive": true,
        });
    }
    let post = event.resource["conversationLink"]
        .as_str()
        .and_then(|link| link.rsplit_once(";messageid="))
        .map_or_else(String::new, |(_, post)| format!(";messageid={post}"));
    let conversation_link = format!("{conversation_link}{post}");
    let mut resource = if event.kind == EventKind::Typing {
        json!({
            "clientmessageid": id.to_string(), "content": "", "id": id.to_string(), "version": id.to_string(),
            "messagetype": event.resource["messagetype"], "contenttype": "Application/Message",
            "from": format!("{base}/contacts/{}", event.from), "imdisplayname": "",
            "properties": {"importance": "", "subject": ""}, "type": "Message",
        })
    } else {
        event.resource.clone()
    };
    if let Value::Object(fields) = &mut resource {
        fields.insert("conversationLink".to_owned(), json!(conversation_link));
        fields.insert("to".to_owned(), json!(chat));
        fields.insert("threadtype".to_owned(), json!("chat"));
    }
    let message_id = resource["id"].as_str().unwrap_or_default().to_owned();
    json!({
        "time": iso_time(now_ms(), 7),
        "type": "EventMessage",
        "resourceLink": format!("{conversation_link}/messages/{message_id}"),
        "resourceType": resource_type(event.kind).unwrap_or_default(),
        "resource": resource,
        "isactive": true,
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or_default())
}

/// The registrar: `POST /registrar/prod/V2/registrations` (see the module docs); `None` for any other path.
pub(crate) fn registrar(state: &mut State, request: &Request) -> Option<ResponseTemplate> {
    if request.url.path() != "/registrar/prod/V2/registrations" {
        return None;
    }
    if request.method.as_str() != "POST" {
        return Some(ResponseTemplate::new(404));
    }
    let header = |name: &str| request.headers.get(name).and_then(|value| value.to_str().ok());
    let account = header("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(|token| state.account_by_token(token))
        .map(|account| account.mri.clone())
        .filter(|_| header("x-ms-migration") == Some("True"));
    let Some(account) = account else {
        return Some(ResponseTemplate::new(401));
    };
    let Some(registration) = stack_registration(&request.body, account) else {
        return Some(ResponseTemplate::new(400));
    };
    state.registrations.push(registration);
    state.changed();
    Some(ResponseTemplate::new(202))
}

/// Either stack's registration of `registration.json`'s shape, by `account`.
fn stack_registration(body: &[u8], account: String) -> Option<Registration> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let description = &body["clientDescription"];
    let app_id = description["appId"].as_str()?;
    let template = description["templateKey"].as_str()?;
    let transport = body["transports"]["TROUTER"]
        .as_array()
        .filter(|list| list.len() == 1)?
        .first()?;
    let path = transport["path"].as_str()?;
    let known = [MESSAGING_APP, CALLING_APP].contains(&app_id)
        && template
            .strip_prefix(app_id)
            .and_then(|rest| rest.strip_prefix('_'))
            .is_some_and(|version| !version.is_empty())
        && path.starts_with("https://")
        && path.contains("/v4/f/");
    known.then(|| Registration {
        account,
        app_id: app_id.to_owned(),
        registration_id: body["registrationId"].as_str().unwrap_or_default().to_owned(),
        path: path.to_owned(),
        ttl: transport["ttl"].as_u64().unwrap_or_default(),
    })
}
