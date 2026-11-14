//! The push channel against `teams-mock`'s Trouter and chat service, reached through the `test-support` mock origins:
//! connect, authenticate as the first frame, register the messaging endpoint, keep-alives, deliveries that are never
//! answered (an incoming call's and others' presence included), a dropped socket and the regional reconnect with its replay, when a socket
//! that went down was last heard (not when a missing pong was noticed), the service's
//! disconnects, a regional host given up, token renewal (a fake clock, the real session over a mock Entra, before the
//! service's `1::`), a session signed out before the service's `1::`, a throttled token service, the stop (also while
//! a socket opens), no token in any log record, and that every frame the socket sent is one of the session's own
//! frames.
//!
//! Every id and token here is the mocks' made-up one.

use std::future::{self, Future};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore as _};
use teams_auth::session::{AccessToken, Clock, Renewal, Session};
use teams_auth::token::{EntraConfig, TokenClient, TokenError};
use teams_mock::{AB_CHAT, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, REGIONAL_NAME, SocketRecord, TrouterMock};
use teams_proto::Mri;
use teams_proto::{Credentials, HttpTransport, IC3_AUDIENCE, TROUTER_HOST};
use teams_trouter::{
    BuildError, Change, ChatEvent, Down, Ignored, Options, PushEvent, RegistrationReport, Stop, Trouter,
    TrouterCredentials, TrouterHandle,
};
use tokio::sync::broadcast;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// How long a test waits for an event before it fails.
const WAIT: Duration = Duration::from_secs(15);

/// A clock the test moves by hand.
struct FakeClock(Mutex<SystemTime>);

#[cfg(test)]
impl FakeClock {
    fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(SystemTime::now())))
    }

    fn advance(&self, by: Duration) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) += by;
    }
}

#[cfg(test)]
impl Clock for FakeClock {
    fn now(&self) -> SystemTime {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// b's tokens: the mock's, then the ones the test queues for a replacement.
struct FakeTokens {
    clock: Arc<FakeClock>,
    current: Mutex<AccessToken>,
    queued: Mutex<Vec<String>>,
    replaced: AtomicUsize,
    renewals: broadcast::Sender<Renewal>,
    /// Every `ic3` the client asked for.
    asked: AtomicUsize,
    /// Each ask is answered as the token service's throttling with `Retry-After: 0`.
    throttled: AtomicBool,
    /// Each ask is answered `NotSignedIn`, as a signed-out session answers.
    signed_out: AtomicBool,
}

#[cfg(test)]
impl FakeTokens {
    fn new(token: &str, clock: Arc<FakeClock>) -> Arc<Self> {
        let current = Mutex::new(Self::token(token, clock.now()));
        Arc::new(Self {
            clock,
            current,
            queued: Mutex::default(),
            replaced: AtomicUsize::new(0),
            renewals: broadcast::channel(16).0,
            asked: AtomicUsize::new(0),
            throttled: AtomicBool::new(false),
            signed_out: AtomicBool::new(false),
        })
    }

    fn token(token: &str, now: SystemTime) -> AccessToken {
        AccessToken {
            token: Secret::new(token),
            expires_at: now + Duration::from_secs(7_000),
            scopes: Vec::new(),
        }
    }

    fn queue(&self, token: &str) {
        self.queued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(token.to_owned());
    }

    fn current(&self) -> AccessToken {
        self.current.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Another part of the client renewed the token: `token` now, expiring at `expires_at`, and announced.
    fn renew(&self, token: &str, expires_at: SystemTime) {
        let mut renewed = Self::token(token, self.clock.now());
        renewed.expires_at = expires_at;
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = renewed;
        let _ = self.renewals.send(Renewal::AccessToken {
            resource: IC3_AUDIENCE.to_owned(),
            expires_at,
        });
    }

    fn replace(&self, old: &Secret) -> Result<AccessToken, TokenError> {
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        if current.token.expose() == old.expose() {
            let mut queued = self.queued.lock().unwrap_or_else(PoisonError::into_inner);
            if queued.is_empty() {
                return Err(TokenError::NotSignedIn {
                    account: "lab-b".to_owned(),
                });
            }
            *current = Self::token(&queued.remove(0), self.clock.now());
            self.replaced.fetch_add(1, Ordering::SeqCst);
        }
        Ok(current.clone())
    }
}

#[cfg(test)]
impl TrouterCredentials for FakeTokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        self.asked.fetch_add(1, Ordering::SeqCst);
        if self.signed_out.load(Ordering::SeqCst) {
            return future::ready(Err(TokenError::NotSignedIn {
                account: "lab-b".to_owned(),
            }));
        }
        if self.throttled.load(Ordering::SeqCst) {
            return future::ready(Err(TokenError::Throttled {
                retry_after: Some(Duration::ZERO),
                error: None,
            }));
        }
        future::ready(Ok(self.current()))
    }

    fn ic3_after(&self, old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(self.replace(old))
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

#[cfg(test)]
impl Credentials for FakeTokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(self.current().token))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(self.replace(rejected).map(|token| token.token))
    }
}

/// Short waits, so the tests run in seconds: pings every 300 ms, a pong late after 1.5 s (a loaded machine is slow),
/// backoffs of 50–200 ms with no jitter, the token's expiry checked every 50 ms.
#[cfg(test)]
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

struct Lab {
    mock: ChatServiceMock,
    trouter: TrouterMock,
    clock: Arc<FakeClock>,
    tokens: Arc<FakeTokens>,
    http: reqwest::Client,
}

#[cfg(test)]
impl Lab {
    async fn start() -> Self {
        let mock = ChatServiceMock::start().await.unwrap();
        let trouter = mock.start_trouter().unwrap();
        let clock = FakeClock::new();
        let tokens = FakeTokens::new(&mock.token(LAB_B).unwrap(), Arc::clone(&clock));
        Self {
            mock,
            trouter,
            clock,
            tokens,
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
        }
    }

    /// b's messaging push channel on the mock transport.
    fn client(&self) -> Trouter<HttpTransport<FakeTokens>, FakeTokens> {
        let transport =
            HttpTransport::with_mock_origin(Arc::clone(&self.tokens), CHAT_BASE, &self.mock.origin()).unwrap();
        Trouter::with_mock_origin(Arc::new(transport), Arc::clone(&self.tokens), self.trouter.origin())
            .unwrap()
            .with_clock(self.clock.clone())
            .with_options(options())
    }

    /// Sends as a, with the web client's chat headers.
    async fn as_a(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Value {
        let url = format!("{}/api/chatsvc/amer/v1/{path}", self.mock.origin());
        let mut request = self
            .http
            .request(method, url)
            .header("Authorization", format!("Bearer {}", self.mock.token(LAB_A).unwrap()))
            .header("X-MS-Migration", "True")
            .header("BehaviorOverride", "redirectAs404");
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .body(body.to_string());
        }
        let answer = request.send().await.unwrap();
        assert!(answer.status().is_success(), "{}", answer.status());
        serde_json::from_str(&answer.text().await.unwrap()).unwrap_or(Value::Null)
    }

    /// a's message to b; its id.
    async fn post(&self, content: &str, client_id: &str) -> String {
        let body = text_message(content, client_id);
        let answer = self.as_a(reqwest::Method::POST, &messages_path(), Some(body)).await;
        answer["OriginalArrivalTime"].to_string()
    }
}

#[cfg(test)]
fn messages_path() -> String {
    format!(
        "users/ME/conversations/{}/messages",
        AB_CHAT.replace(':', "%3A").replace('@', "%40")
    )
}

#[cfg(test)]
fn text_message(content: &str, client_id: &str) -> Value {
    json!({
        "content": content,
        "messagetype": "RichText/Html",
        "contenttype": "Text",
        "clientmessageid": client_id,
        "imdisplayname": "Lab A",
        "amsreferences": [],
        "properties": {"mentions": "[]", "files": "[]", "formatVariant": "TEAMS"},
    })
}

/// The next event `pick` takes, skipping others; fails after [`WAIT`].
#[cfg(test)]
async fn next<T>(push: &mut TrouterHandle, mut pick: impl FnMut(PushEvent) -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let event = tokio::time::timeout_at(deadline, push.next())
            .await
            .expect("the event didn't come in time")
            .expect("the client stopped");
        if let Some(picked) = pick(event) {
            return picked;
        }
    }
}

#[cfg(test)]
async fn connected(push: &mut TrouterHandle) -> String {
    next(push, |event| match event {
        PushEvent::Connected { endpoint, .. } => Some(endpoint),
        _ => None,
    })
    .await
}

#[cfg(test)]
async fn registered(push: &mut TrouterHandle) {
    next(push, |event| match event {
        PushEvent::Registration(RegistrationReport::Registered) => Some(()),
        PushEvent::Registration(RegistrationReport::Failed(why)) => panic!("registration failed: {why}"),
        _ => None,
    })
    .await;
}

#[cfg(test)]
async fn chat_event(push: &mut TrouterHandle) -> (bool, ChatEvent) {
    next(push, |event| match event {
        PushEvent::Delivery(delivery) => Some((delivery.stored, delivery.event)),
        _ => None,
    })
    .await
}

#[cfg(test)]
async fn down(push: &mut TrouterHandle) -> Down {
    next(push, |event| match event {
        PushEvent::Disconnected { down, .. } => Some(down),
        _ => None,
    })
    .await
}

/// Waits until `check` holds of the mock's sockets.
#[cfg(test)]
async fn sockets_until(trouter: &TrouterMock, check: impl Fn(&[SocketRecord]) -> bool) -> Vec<SocketRecord> {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let sockets = trouter.sockets();
        if check(&sockets) {
            return sockets;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the mock's sockets: {sockets:#?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The own frame a frame the socket sent is, by its kind; `None` for anything else.
#[cfg(test)]
fn own_kind(frame: &str) -> Option<&'static str> {
    if frame == "0::" {
        return Some("trouter_disconnect");
    }
    if frame.starts_with(
        r#"5:::{"name":"user.authenticate","args":[{"headers":{"X-Ms-Test-User":"False","Authorization":"Bearer "#,
    ) {
        return Some("trouter_authenticate");
    }
    let (id, event) = frame.strip_prefix("5:")?.split_once("+::")?;
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if event == r#"{"name":"ping"}"# {
        Some("trouter_ping")
    } else if event.starts_with(r#"{"name":"trouter.processed_message_loss","args":[{"droppedIndicators":["#) {
        Some("trouter_message_loss_ack")
    } else {
        None
    }
}

/// Once every socket has closed: every frame every socket sent is one of the session's own frames, none answers a
/// delivery.
#[cfg(test)]
async fn sent_only_own_frames(lab: &Lab) {
    let sockets = sockets_until(&lab.trouter, |sockets| sockets.iter().all(|socket| !socket.open)).await;
    let sent: Vec<String> = sockets.into_iter().flat_map(|socket| socket.frames).collect();
    assert!(!sent.is_empty(), "the socket sent nothing");
    for frame in &sent {
        assert!(!frame.starts_with("3:"), "a delivery was answered: {frame}");
        assert!(own_kind(frame).is_some(), "not an own frame: {frame}");
    }
}

#[tokio::test]
async fn a_channel_delivery_keeps_the_post_its_message_belongs_to() {
    let lab = Lab::start().await;
    let mut push = lab.client().spawn();
    connected(&mut push).await;
    registered(&mut push).await;
    let channel = teams_mock::TEAM_CHANNEL;
    let id = lab
        .mock
        .add_message(
            &format!("{channel};messageid=1760000000999"),
            (LAB_A, "Lab A"),
            "Event/Call",
            "<callEventType>callStarted</callEventType>",
        )
        .unwrap();
    let (_, event) = chat_event(&mut push).await;
    push.stop().await;
    let ChatEvent::Message { conversation, message } = event else {
        panic!("not a message: {event:?}");
    };
    assert_eq!(conversation, channel);
    assert_eq!(message.id, id);
    assert_eq!(message.reply_root(channel).as_deref(), Some("1760000000999"));
}

#[tokio::test]
async fn a_client_authenticates_first_registers_and_gets_a_delivery_it_never_answers() {
    let lab = Lab::start().await;
    let client = lab.client();
    let epid = client.endpoint_id().to_owned();
    let mut push = client.spawn();

    let endpoint = connected(&mut push).await;
    registered(&mut push).await;
    let id = lab.post("<p>hello b</p>", "1000000000000000042").await;
    let (stored, event) = chat_event(&mut push).await;
    let ChatEvent::Message { conversation, message } = event else {
        panic!("not a message: {event:?}");
    };
    assert!(!stored);
    assert_eq!((conversation.as_str(), message.id.as_str()), (AB_CHAT, id.as_str()));
    assert_eq!(message.content, "<p>hello b</p>");
    assert_eq!(
        message.client_message_id.as_known().map(String::as_str),
        Some("1000000000000000042")
    );
    assert!(lab.mock.take_events(LAB_B).is_empty(), "delivered, not left queued");

    // the first frame on the socket is the authentication, with b's token and no reconnect parameters
    let sockets = lab.trouter.sockets();
    let [socket] = &sockets[..] else {
        panic!("one socket: {sockets:#?}");
    };
    assert_eq!(socket.account.as_deref(), Some(LAB_B));
    assert!(!socket.reconnect);
    let first: Value = serde_json::from_str(socket.frames[0].strip_prefix("5:::").unwrap()).unwrap();
    assert_eq!(
        first["args"][0]["headers"]["Authorization"],
        format!("Bearer {}", lab.mock.token(LAB_B).unwrap())
    );
    assert!(first["args"][0].get("connectparams").is_none());
    // the connect URL carries the web client's query
    let query: Vec<(String, String)> = url::form_urlencoded::parse(socket.query.as_bytes())
        .into_owned()
        .collect();
    let names: Vec<&str> = query.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["tc", "timeout", "epid", "cor_id", "con_num"]);
    let tc: Value = serde_json::from_str(&query[0].1).unwrap();
    assert_eq!(
        (tc["ua"].as_str(), tc["v"].as_str()),
        (Some("TeamsCDL"), Some(teams_proto::CLIENT_BUILD))
    );
    assert_eq!((query[1].1.as_str(), query[2].1.as_str()), ("40", epid.as_str()));
    // The registration names the messaging stack and the endpoint's push URL.
    let registrations = lab.trouter.registrations();
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].registration_id, epid);
    assert!(registrations[0].path.ends_with(&format!("/v4/f/{endpoint}/")));

    push.stop().await;
    let sockets = sockets_until(&lab.trouter, |sockets| sockets.iter().all(|socket| !socket.open)).await;
    assert_eq!(sockets[0].frames.last().map(String::as_str), Some("0::"));
    sent_only_own_frames(&lab).await;
}

#[tokio::test]
async fn pings_are_answered_and_a_missing_pong_means_a_reconnect() {
    let lab = Lab::start().await;
    let mut push = lab.client().spawn();
    connected(&mut push).await;
    let pinged = sockets_until(&lab.trouter, |sockets| {
        sockets[0]
            .frames
            .iter()
            .filter(|frame| own_kind(frame) == Some("trouter_ping"))
            .count()
            >= 2
    })
    .await;
    assert_eq!(pinged.len(), 1, "an answered ping keeps the socket");

    lab.trouter.set_pongs(false);
    assert!(matches!(down(&mut push).await, Down::PongTimeout));
    connected(&mut push).await;
    assert_eq!(lab.trouter.sockets().len(), 2);
    push.stop().await;
    sent_only_own_frames(&lab).await;
}

#[tokio::test]
async fn a_socket_gone_silent_says_when_it_was_last_heard_not_when_that_was_noticed() {
    let lab = Lab::start().await;
    let started = lab.clock.now();
    let mut push = lab.client().spawn();
    connected(&mut push).await;
    lab.trouter.set_pongs(false);
    // two pings' time: every pong answered before is read, and the next ping waits for one that never comes (a late
    // pong is noticed only after 1.5 s)
    tokio::time::sleep(Duration::from_millis(600)).await;
    let silent_since = lab.clock.now();
    // a suspend: the clock jumps on before the missing pong is noticed
    lab.clock.advance(Duration::from_secs(600));
    let (down, heard) = next(&mut push, |event| match event {
        PushEvent::Disconnected { down, heard } => Some((down, heard)),
        _ => None,
    })
    .await;
    assert!(matches!(down, Down::PongTimeout), "{down:?}");
    let heard = heard.expect("the socket was connected");
    assert!(
        started <= heard && heard <= silent_since,
        "last heard {heard:?}, not between the start {started:?} and the silence {silent_since:?}"
    );
    push.stop().await;
}

#[tokio::test]
async fn a_socket_that_never_connected_says_nothing_was_heard() {
    let lab = Lab::start().await;
    // the service's `1::` and the authentication, then the socket closed without a `trouter.connected`
    lab.trouter.withhold_connected(1);
    let mut push = lab.client().spawn();
    let (down, heard) = next(&mut push, |event| match event {
        PushEvent::Disconnected { down, heard } => Some((down, heard)),
        _ => None,
    })
    .await;
    assert!(matches!(down, Down::Closed), "{down:?}");
    assert_eq!(heard, None);
    // the next one connects, and a drop of it was heard
    connected(&mut push).await;
    lab.trouter.drop_sockets(LAB_B);
    let heard = next(&mut push, |event| match event {
        PushEvent::Disconnected { heard, .. } => Some(heard),
        _ => None,
    })
    .await;
    assert!(heard.is_some());
    push.stop().await;
}

#[tokio::test]
async fn a_dropped_socket_reconnects_regionally_and_the_unanswered_deliveries_come_again_stored() {
    let lab = Lab::start().await;
    let client = lab.client();
    let opened = client.opened_urls();
    let mut push = client.spawn();
    let endpoint = connected(&mut push).await;
    registered(&mut push).await;
    let first = lab.post("<p>before the drop</p>", "1000000000000000101").await;
    assert!(matches!(
        chat_event(&mut push).await,
        (false, ChatEvent::Message { .. })
    ));

    lab.trouter.drop_sockets(LAB_B);
    assert!(matches!(down(&mut push).await, Down::Closed));
    let again = next(&mut push, |event| match event {
        PushEvent::Connected {
            endpoint, new_endpoint, ..
        } => Some((endpoint, new_endpoint)),
        _ => None,
    })
    .await;
    assert_eq!(
        again,
        (endpoint.clone(), false),
        "the same endpoint, from the reconnect parameters"
    );
    let loss = next(&mut push, |event| match event {
        PushEvent::MessageLoss(loss) => Some(loss),
        _ => None,
    })
    .await;
    assert!(loss.messaging());
    let indicators = loss.indicators.clone();
    loss.caught_up();
    // the message delivered before the drop comes again, as a stored replay; a new one comes live
    let (stored, replayed) = chat_event(&mut push).await;
    assert!(stored);
    assert!(matches!(replayed, ChatEvent::Message { ref message, .. } if message.id == first));
    lab.post("<p>after the drop</p>", "1000000000000000102").await;
    let (stored, live) = chat_event(&mut push).await;
    assert!(!stored);
    assert!(matches!(live, ChatEvent::Message { ref message, .. } if message.content == "<p>after the drop</p>"));

    let sockets = sockets_until(&lab.trouter, |sockets| {
        sockets.len() == 2
            && sockets[1]
                .frames
                .iter()
                .any(|frame| own_kind(frame) == Some("trouter_message_loss_ack"))
    })
    .await;
    assert!(sockets[1].reconnect, "the reconnect sent the parameters back");
    // the first connect went to the global host, the reconnect to the regional one the first was given
    let hosts: Vec<_> = opened
        .urls()
        .iter()
        .map(|url| url.host_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        hosts,
        [
            TROUTER_HOST.to_owned(),
            format!("{REGIONAL_NAME}-t.trouter.teams.microsoft.com")
        ],
        "the connects' hosts as built"
    );
    let auth: Value = serde_json::from_str(sockets[1].frames[0].strip_prefix("5:::").unwrap()).unwrap();
    assert_eq!(auth["args"][0]["connectparams"]["sr"], endpoint.as_str());
    let ack = sockets[1]
        .frames
        .iter()
        .find(|frame| own_kind(frame) == Some("trouter_message_loss_ack"))
        .unwrap();
    let ack: Value = serde_json::from_str(ack.split_once("+::").unwrap().1).unwrap();
    assert_eq!(
        ack["args"][0]["droppedIndicators"][0]["tag"],
        indicators[0].tag.as_str()
    );
    push.stop().await;
    sent_only_own_frames(&lab).await;
}

#[tokio::test]
async fn typing_edits_reactions_and_deletes_arrive_as_their_events() {
    let lab = Lab::start().await;
    let mut push = lab.client().spawn();
    connected(&mut push).await;
    registered(&mut push).await;
    let typing = json!({"content": "", "messagetype": "Control/Typing", "contenttype": "Application/Message"});
    lab.as_a(reqwest::Method::POST, &messages_path(), Some(typing)).await;
    let id = lab.post("<p>to change</p>", "1000000000000000201").await;
    let message = format!("{}/{id}", messages_path());
    lab.as_a(
        reqwest::Method::PUT,
        &message,
        Some(text_message("<p>changed</p>", "1000000000000000201")),
    )
    .await;
    let like = json!({"emotions": {"key": "like", "value": 1_760_002_000_000_u64}});
    lab.as_a(
        reqwest::Method::PUT,
        &format!("{message}/properties?name=emotions"),
        Some(like),
    )
    .await;
    lab.as_a(reqwest::Method::DELETE, &format!("{message}?behavior=softDelete"), None)
        .await;

    let (_, event) = chat_event(&mut push).await;
    assert_eq!(
        event,
        ChatEvent::Typing {
            conversation: AB_CHAT.to_owned(),
            from: Some(LAB_A.to_owned())
        }
    );
    assert!(matches!(chat_event(&mut push).await.1, ChatEvent::Message { .. }));
    for (change, content) in [
        (Change::Edit, "<p>changed</p>"),
        (Change::Reaction, "<p>changed</p>"),
        (Change::Delete, ""),
    ] {
        let (_, event) = chat_event(&mut push).await;
        let ChatEvent::MessageUpdate {
            change: got, message, ..
        } = event
        else {
            panic!("not an update: {event:?}");
        };
        assert_eq!(
            (got, message.id.as_str(), message.content.as_str()),
            (change, id.as_str(), content)
        );
    }
    push.stop().await;
    sent_only_own_frames(&lab).await;
}

#[tokio::test]
async fn an_incoming_call_is_ignored_and_never_answered() {
    let lab = Lab::start().await;
    let mut push = lab.client().spawn();
    connected(&mut push).await;
    registered(&mut push).await;
    lab.trouter.deliver_incoming_call(LAB_B);
    let ignored = next(&mut push, |event| match event {
        PushEvent::Ignored(ignored) => Some(ignored),
        _ => None,
    })
    .await;
    assert_eq!(ignored, Ignored::IncomingCall);
    // a ping afterwards shows the socket read on, and nothing answered the call
    let before = lab.trouter.sockets()[0].frames.len();
    sockets_until(&lab.trouter, |sockets| sockets[0].frames.len() > before).await;
    push.stop().await;
    sent_only_own_frames(&lab).await;
}

#[tokio::test]
async fn others_presence_is_handed_on_as_delivered_and_never_answered() {
    let lab = Lab::start().await;
    let mut push = lab.client().spawn();
    connected(&mut push).await;
    registered(&mut push).await;
    let presence = |availability: &str| {
        json!({"presence": [{
            "mri": LAB_A,
            "etag": "A0000000110",
            "presence": {
                "sourceNetwork": "SameEnterprise",
                "calendarData": {"isOutOfOffice": false},
                "capabilities": [],
                "availability": availability,
                "activity": availability,
                "deviceType": "Web",
                "lastActiveTime": "1970-01-01T00:00:03.000Z",
            },
        }]})
    };
    let mut snapshot = presence("Offline");
    snapshot["isSnapshot"] = json!(true);
    lab.trouter.deliver_presence(LAB_B, &snapshot, false);
    let delivered = next(&mut push, |event| match event {
        PushEvent::Presence(delivery) => Some(delivery),
        _ => None,
    })
    .await;
    assert!(delivered.snapshot && !delivered.stored);
    assert_eq!(delivered.presences.len(), 1);
    assert_eq!(delivered.presences[0].mri, Mri::new(LAB_A));
    assert_eq!(delivered.presences[0].availability.as_deref(), Some("Offline"));
    assert_eq!(delivered.presences[0].last_active_ms, Some(3000));
    assert_eq!(delivered.presences[0].out_of_office, Some(false));

    // a change, then a replay of an older one (stored): both handed on, told apart
    lab.trouter.deliver_presence(LAB_B, &presence("Busy"), false);
    lab.trouter.deliver_presence(LAB_B, &presence("Away"), true);
    let mut seen = Vec::new();
    while seen.len() < 2 {
        seen.push(
            next(&mut push, |event| match event {
                PushEvent::Presence(delivery) => Some(delivery),
                _ => None,
            })
            .await,
        );
    }
    assert_eq!(seen[0].presences[0].availability.as_deref(), Some("Busy"));
    assert!(!seen[0].stored && !seen[0].snapshot);
    assert_eq!(seen[1].presences[0].availability.as_deref(), Some("Away"));
    assert!(seen[1].stored);

    // a body that isn't a presence list isn't read, and isn't answered either
    lab.trouter
        .deliver_presence(LAB_B, &json!({"presence": [{"etag": "no mri"}]}), false);
    let ignored = next(&mut push, |event| match event {
        PushEvent::Ignored(ignored) => Some(ignored),
        _ => None,
    })
    .await;
    assert!(matches!(ignored, Ignored::Undecodable { ref path } if path.ends_with("/unifiedPresenceService")));
    push.stop().await;
    sent_only_own_frames(&lab).await;
}

#[tokio::test]
async fn an_unauthorized_disconnect_reconnects_with_a_new_token() {
    let lab = Lab::start().await;
    let mut push = lab.client().spawn();
    connected(&mut push).await;
    let renewed = "mock-ic3-token-renewed-b-1";
    lab.mock.set_token(LAB_B, renewed);
    lab.tokens.queue(renewed);
    lab.trouter.disconnect(LAB_B, "unauthorized");
    assert!(matches!(down(&mut push).await, Down::ServiceDisconnect(Some(ref reason)) if reason == "unauthorized"));
    connected(&mut push).await;
    registered(&mut push).await;
    let sockets = lab.trouter.sockets();
    let auth: Value = serde_json::from_str(sockets[1].frames[0].strip_prefix("5:::").unwrap()).unwrap();
    assert_eq!(auth["args"][0]["headers"]["Authorization"], format!("Bearer {renewed}"));
    assert_eq!(lab.tokens.replaced.load(Ordering::SeqCst), 1);
    push.stop().await;
    sent_only_own_frames(&lab).await;
}

#[tokio::test]
async fn a_service_that_keeps_disconnecting_at_once_is_met_with_a_backoff() {
    let lab = Lab::start().await;
    let slow = Options {
        backoff_base: Duration::from_millis(700),
        backoff_cap: Duration::from_millis(700),
        ..options()
    };
    let mut push = lab.client().with_options(slow).spawn();
    connected(&mut push).await;
    lab.trouter.disconnect(LAB_B, "dup");
    assert!(matches!(down(&mut push).await, Down::ServiceDisconnect(Some(ref reason)) if reason == "dup"));
    connected(&mut push).await;
    lab.trouter.disconnect(LAB_B, "dup");
    down(&mut push).await;
    let second = tokio::time::Instant::now();
    connected(&mut push).await;
    assert!(
        second.elapsed() >= Duration::from_millis(700),
        "a second reconnect within 10 s waits"
    );
    // the second `dup` within 10 s gave up the regional host: the next connect starts over, without parameters
    let sockets = lab.trouter.sockets();
    assert_eq!(
        sockets.iter().map(|socket| socket.reconnect).collect::<Vec<_>>(),
        [false, true, false]
    );
    push.stop().await;
    sent_only_own_frames(&lab).await;
}

#[tokio::test]
async fn a_token_near_its_expiry_by_the_clock_means_a_reconnect_with_a_new_one_before_it_runs_out() {
    let lab = Lab::start().await;
    let mut push = lab.client().spawn();
    connected(&mut push).await;
    let old = lab.tokens.current();
    let renewed = "mock-ic3-token-renewed-b-2";
    lab.mock.set_token(LAB_B, renewed);
    lab.tokens.queue(renewed);

    // 4 min 59 s left: within the 5 min lead, still good
    lab.clock.advance(Duration::from_secs(7_000 - 299));
    assert!(matches!(down(&mut push).await, Down::Renewal));
    connected(&mut push).await;
    assert!(lab.clock.now() < old.expires_at, "renewed before the old token ran out");
    let sockets = lab.trouter.sockets();
    assert_eq!(sockets[0].frames.last().map(String::as_str), Some("0::"));
    let auth: Value = serde_json::from_str(sockets[1].frames[0].strip_prefix("5:::").unwrap()).unwrap();
    assert_eq!(auth["args"][0]["headers"]["Authorization"], format!("Bearer {renewed}"));
    push.stop().await;
    sent_only_own_frames(&lab).await;
}

#[tokio::test]
async fn a_refused_connect_is_tried_again_after_a_growing_backoff() {
    let lab = Lab::start().await;
    // a port nothing listens on
    let unused = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("ws://{}", unused.local_addr().unwrap());
    drop(unused);
    let transport = HttpTransport::with_mock_origin(Arc::clone(&lab.tokens), CHAT_BASE, &lab.mock.origin()).unwrap();
    let mut push = Trouter::with_mock_origin(Arc::new(transport), Arc::clone(&lab.tokens), &origin)
        .unwrap()
        .with_options(options())
        .spawn();
    let mut times = Vec::new();
    for _ in 0..4 {
        assert!(matches!(down(&mut push).await, Down::ConnectFailed(_)));
        times.push(tokio::time::Instant::now());
    }
    // waits of 50, 100, 200 ms between the attempts (no jitter); the later ones at least as long
    assert!(times[2] - times[1] >= Duration::from_millis(100));
    assert!(times[3] - times[2] >= Duration::from_millis(200));
    push.stop().await;
}

#[test]
fn only_a_loopback_ws_origin_is_a_mock_origin() {
    let tokens = FakeTokens::new("mock-token", FakeClock::new());
    let transport =
        Arc::new(HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, "http://127.0.0.1:9").unwrap());
    for origin in [
        "http://127.0.0.1:9",
        "wss://127.0.0.1:9",
        "ws://example.test:9",
        "ws://127.0.0.1",
        "ws://127.0.0.1:9/v4/c",
        "ws://127.0.0.1:9?x=1",
        "ws://user@127.0.0.1:9",
    ] {
        let built = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), origin);
        assert!(matches!(built, Err(BuildError::MockOrigin)), "{origin}");
    }
    assert!(Trouter::with_mock_origin(transport, tokens, "ws://127.0.0.1:9").is_ok());
}

#[test]
fn the_sockets_write_half_goes_only_to_the_frame_sink() {
    // the only code in the crate that can write to a socket is the frame writer the sink is given
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for file in std::fs::read_dir(&src).unwrap() {
        let path = file.unwrap().path();
        let source = std::fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_str().unwrap();
        for writer in ["SinkExt", "SplitSink", "Message::text", ".feed(", "start_send"] {
            if name != "socket.rs" {
                assert!(!source.contains(writer), "{name} names {writer}");
            }
        }
    }
    let socket = std::fs::read_to_string(src.join("socket.rs")).unwrap();
    assert_eq!(socket.matches("self.0.send(").count(), 1);
    let writer = socket.split("impl FrameWriter for SocketWriter").nth(1).unwrap();
    assert!(writer.contains("self.0.send("), "the one send is the frame writer's");
    assert_eq!(socket.matches(".send(").count(), 1, "socket.rs sends nothing else");
    assert!(socket.contains("pub(crate) struct SocketWriter(SplitSink"));
}

/// Entra's token endpoint: each grant a new access token (`synthetic-access-token-<n>`) living `LIFETIME` seconds.
struct Entra(Arc<AtomicUsize>);

#[cfg(test)]
impl Respond for Entra {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let grant = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        ResponseTemplate::new(200).set_body_json(json!({
            "token_type": "Bearer",
            "scope": "https://ic3.teams.office.com/.default openid profile offline_access",
            "expires_in": 7_000,
            "ext_expires_in": 7_000,
            "access_token": format!("synthetic-access-token-{grant}"),
            "refresh_token": format!("synthetic-refresh-token-{grant}"),
        }))
    }
}

/// b's real session over a mock Entra, on the lab's fake clock.
#[cfg(test)]
async fn session(clock: Arc<FakeClock>) -> (MockServer, Arc<Session<MemorySecretStore>>) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/organizations/oauth2/v2.0/token"))
        .respond_with(Entra(Arc::new(AtomicUsize::new(0))))
        .mount(&server)
        .await;
    let store = MemorySecretStore::new();
    store
        .put(
            &refresh_token_key("b@lab.example.test"),
            &Secret::new("synthetic-refresh-token-0"),
        )
        .await
        .unwrap();
    let tokens =
        TokenClient::new(EntraConfig::teams_native().with_authority(format!("{}/organizations/", server.uri())))
            .unwrap();
    let authz = AuthzClient::with_endpoint(&format!("{}/api/authsvc/v1.0/authz", server.uri())).unwrap();
    let session = Session::new("b@lab.example.test", store, tokens, authz, Region::default()).with_clock(clock);
    (server, Arc::new(session))
}

#[cfg(test)]
fn session_client(lab: &Lab, session: &Arc<Session<MemorySecretStore>>) -> TrouterHandle {
    let transport = HttpTransport::with_mock_origin(Arc::clone(session), CHAT_BASE, &lab.mock.origin()).unwrap();
    Trouter::with_mock_origin(Arc::new(transport), Arc::clone(session), lab.trouter.origin())
        .unwrap()
        .with_clock(lab.clock.clone())
        .with_options(options())
        .spawn()
}

#[cfg(test)]
fn authorization(socket: &SocketRecord) -> String {
    let auth: Value = serde_json::from_str(socket.frames[0].strip_prefix("5:::").unwrap()).unwrap();
    auth["args"][0]["headers"]["Authorization"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn with_the_session_the_socket_is_renewed_before_the_token_runs_out() {
    let lab = Lab::start().await;
    let (_entra, session) = session(lab.clock.clone()).await;
    lab.mock.set_token(LAB_B, "synthetic-access-token-1");
    let mut push = session_client(&lab, &session);
    connected(&mut push).await;
    registered(&mut push).await;
    let first = session.access_token(IC3_AUDIENCE).await.unwrap();

    lab.mock.set_token(LAB_B, "synthetic-access-token-2");
    lab.clock.advance(Duration::from_secs(7_000 - 299));
    assert!(matches!(down(&mut push).await, Down::Renewal));
    connected(&mut push).await;
    registered(&mut push).await;
    assert!(lab.clock.now() < first.expires_at);
    let sockets = lab.trouter.sockets();
    assert_eq!(sockets.len(), 2);
    assert_eq!(authorization(&sockets[0]), "Bearer synthetic-access-token-1");
    assert_eq!(authorization(&sockets[1]), "Bearer synthetic-access-token-2");
    assert_eq!(sockets[0].frames.last().map(String::as_str), Some("0::"));
    push.stop().await;
    sent_only_own_frames(&lab).await;
}

#[tokio::test]
async fn with_the_session_a_renewal_it_announces_reconnects_at_once() {
    let lab = Lab::start().await;
    let (_entra, session) = session(lab.clock.clone()).await;
    lab.mock.set_token(LAB_B, "synthetic-access-token-1");
    let mut push = session_client(&lab, &session);
    connected(&mut push).await;

    // another part of the client (the chat transport, say) had the ic3 token refused and renewed it
    lab.mock.set_token(LAB_B, "synthetic-access-token-2");
    lab.clock.advance(Duration::from_secs(1));
    let held = session.access_token(IC3_AUDIENCE).await.unwrap();
    session.refresh_rejected(IC3_AUDIENCE, &held.token).await.unwrap();
    assert!(matches!(down(&mut push).await, Down::Renewal));
    connected(&mut push).await;
    let sockets = lab.trouter.sockets();
    assert_eq!(authorization(&sockets[1]), "Bearer synthetic-access-token-2");
    push.stop().await;
    sent_only_own_frames(&lab).await;
}

/// A registrar that refuses the first registration (503) and takes every later one (202), keeping what it was sent.
#[derive(Default)]
struct FlakyRegistrar {
    sent: Mutex<Vec<(String, Value)>>,
    disconnect: AtomicBool,
}

#[cfg(test)]
impl teams_proto::Transport for FlakyRegistrar {
    type Error = io::Error;

    fn read<R: teams_proto::Read>(
        &self,
        _request: &R,
    ) -> impl Future<Output = Result<teams_proto::Response, io::Error>> + Send {
        future::ready(Err(io::Error::other("the push channel reads nothing")))
    }

    fn send<R: teams_proto::Request>(
        &self,
        request: &R,
    ) -> impl Future<Output = Result<teams_proto::Response, io::Error>> + Send {
        if self.disconnect.swap(false, Ordering::SeqCst) {
            return future::ready(Err(io::Error::other("synthetic registrar connection lost")));
        }
        let mut sent = self.sent.lock().unwrap_or_else(PoisonError::into_inner);
        sent.push((
            request.endpoint().segments().join("/"),
            request.body().unwrap_or_default(),
        ));
        let status = if sent.len() == 1 { 503 } else { 202 };
        future::ready(Ok(teams_proto::Response::new(status, Vec::new())))
    }

    fn deliver<R: teams_proto::Request + Send>(
        &self,
        _write: R,
    ) -> impl Future<Output = Result<teams_proto::Response, io::Error>> + Send {
        future::ready(Err(io::Error::other("the push channel writes nothing")))
    }
}

#[tokio::test]
async fn calling_registration_recovers_from_transport_and_http_failures_without_issuing_a_presence_endpoint() {
    let lab = Lab::start().await;
    let registrar = Arc::new(FlakyRegistrar {
        disconnect: AtomicBool::new(true),
        ..FlakyRegistrar::default()
    });
    let client = Trouter::with_mock_origin(Arc::clone(&registrar), Arc::clone(&lab.tokens), lab.trouter.origin())
        .unwrap()
        .with_options(Options {
            registration: teams_trouter::RegistrationTimes {
                retry_after: Duration::from_millis(100),
                ..teams_trouter::RegistrationTimes::default()
            },
            ..options()
        })
        .for_calls(teams_trouter::CallStack::new(Arc::new(teams_call::Tracker::new(
            Mri::new(LAB_B),
        ))));
    let epid = client.endpoint_id().to_owned();
    let mut push = client.spawn();
    let endpoint = connected(&mut push).await;
    let mut reports = Vec::new();
    while reports.len() < 3 {
        reports.push(
            next(&mut push, |event| match event {
                PushEvent::Registration(report) => Some(report),
                PushEvent::Registered(endpoint) => panic!("calling cannot issue a presence endpoint: {endpoint:?}"),
                _ => None,
            })
            .await,
        );
    }
    assert!(matches!(&reports[0], RegistrationReport::Failed(why) if why.contains("connection lost")));
    assert!(matches!(&reports[1], RegistrationReport::Failed(why) if why.contains("503")));
    assert_eq!(reports[2], RegistrationReport::Registered);
    push.stop().await;
    let sent = registrar.sent.lock().unwrap();
    assert_eq!(sent.len(), 2, "only the HTTP attempts reached the registrar");
    for (path, body) in sent.iter() {
        assert_eq!(path, "prod/V2/registrations");
        assert_eq!(body["registrationId"], epid);
        assert_eq!(body["clientDescription"]["appId"], "SkypeSpacesWeb");
        assert!(
            body["transports"]["TROUTER"][0]["path"]
                .as_str()
                .unwrap()
                .ends_with(&format!("/v4/f/{endpoint}/"))
        );
    }
}

#[tokio::test]
async fn a_failed_registration_is_tried_again_and_a_good_one_renewed_before_its_ttl_ends() {
    let lab = Lab::start().await;
    let registrar = Arc::new(FlakyRegistrar::default());
    let times = teams_trouter::RegistrationTimes {
        ttl: Duration::from_millis(500),
        renew_before: Duration::from_millis(200),
        retry_after: Duration::from_millis(200),
    };
    let client = Trouter::with_mock_origin(Arc::clone(&registrar), Arc::clone(&lab.tokens), lab.trouter.origin())
        .unwrap()
        .with_options(Options {
            registration: times,
            ..options()
        });
    let epid = client.endpoint_id().to_owned();
    let mut push = client.spawn();
    let endpoint = connected(&mut push).await;
    let mut reports = Vec::new();
    let mut at = Vec::new();
    // the endpoint each registration that went through registered, told just before its report (M2 Task 19)
    let mut endpoints = Vec::new();
    while reports.len() < 3 {
        let event = next(&mut push, |event| match event {
            PushEvent::Registration(report) => Some(Ok(report)),
            PushEvent::Registered(endpoint) => Some(Err(endpoint)),
            _ => None,
        })
        .await;
        match event {
            Ok(report) => {
                reports.push(report);
                at.push(tokio::time::Instant::now());
            }
            Err(endpoint) => endpoints.push((reports.len(), endpoint)),
        }
    }
    assert!(matches!(reports[0], RegistrationReport::Failed(ref why) if why.contains("503")));
    assert_eq!(
        reports[1..],
        [RegistrationReport::Registered, RegistrationReport::Registered]
    );
    let told: Vec<(usize, String)> = endpoints
        .iter()
        .map(|(before, endpoint)| (*before, endpoint.id().to_owned()))
        .collect();
    assert_eq!(
        told,
        [(1, epid.clone()), (2, epid.clone())],
        "the refused registration registered nothing; each taken one its epid, before its report"
    );
    assert!(at[1] - at[0] >= Duration::from_millis(200), "retried after its wait");
    assert!(
        at[2] - at[1] >= Duration::from_millis(300),
        "renewed 200 ms before a 500 ms TTL ends"
    );
    let sent = registrar.sent.lock().unwrap_or_else(PoisonError::into_inner).clone();
    for (endpoint_path, body) in &sent {
        assert_eq!(endpoint_path, "prod/V2/registrations");
        assert_eq!(body["registrationId"], epid.as_str());
        assert_eq!(body["clientDescription"]["appId"], "TeamsCDLWebWorker");
        let path = body["transports"]["TROUTER"][0]["path"].as_str().unwrap();
        assert!(path.ends_with(&format!("/v4/f/{endpoint}/")), "{path}");
    }
    push.stop().await;
    sent_only_own_frames(&lab).await;
}

#[tokio::test]
async fn a_regional_host_that_never_says_connected_is_given_up_after_three_connects_in_a_row() {
    let lab = Lab::start().await;
    let mut push = lab.client().spawn();
    let first = connected(&mut push).await;
    // the regional host takes each socket and its authentication, and closes it without a `trouter.connected`
    lab.trouter.withhold_connected(3);
    lab.trouter.drop_sockets(LAB_B);
    for _ in 0..4 {
        assert!(matches!(down(&mut push).await, Down::Closed));
    }
    let (again, new_endpoint) = next(&mut push, |event| match event {
        PushEvent::Connected {
            endpoint, new_endpoint, ..
        } => Some((endpoint, new_endpoint)),
        _ => None,
    })
    .await;
    assert!(new_endpoint && again != first, "started over at the global host");
    let sockets = lab.trouter.sockets();
    assert_eq!(
        sockets.iter().map(|socket| socket.reconnect).collect::<Vec<_>>(),
        [false, true, true, true, false],
        "three failed reconnects with the parameters, then none"
    );
    push.stop().await;
    sent_only_own_frames(&lab).await;
}

#[tokio::test]
async fn a_throttled_token_answer_with_no_wait_still_waits_a_backoff() {
    let lab = Lab::start().await;
    lab.tokens.throttled.store(true, Ordering::SeqCst);
    let mut push = lab.client().spawn();
    assert!(matches!(down(&mut push).await, Down::Credentials(_)));
    tokio::time::sleep(Duration::from_millis(600)).await;
    // backoffs of 50, 100, 200, 200 ms: about five asks in 600 ms, not one per timer tick
    let asked = lab.tokens.asked.load(Ordering::SeqCst);
    assert!((2..=8).contains(&asked), "{asked} asks");
    push.stop().await;
}

#[tokio::test]
async fn a_renewal_before_the_services_hello_sends_nothing_on_that_socket() {
    let lab = Lab::start().await;
    lab.trouter.delay_hello_once(Duration::from_secs(1));
    let mut push = lab.client().spawn();
    sockets_until(&lab.trouter, |sockets| sockets.len() == 1).await;
    let renewed = "mock-ic3-token-renewed-b-3";
    lab.mock.set_token(LAB_B, renewed);
    // later than the held one, so the renewal is told even by its expiry
    lab.tokens.renew(renewed, lab.clock.now() + Duration::from_secs(7_100));
    assert!(matches!(down(&mut push).await, Down::Renewal));
    connected(&mut push).await;
    push.stop().await;
    sent_only_own_frames(&lab).await;
    // not even a disconnect went out before the first socket's authentication
    let sockets = lab.trouter.sockets();
    assert!(sockets[0].frames.is_empty(), "{:?}", sockets[0].frames);
    assert_eq!(own_kind(&sockets[1].frames[0]), Some("trouter_authenticate"));
    assert_eq!(authorization(&sockets[1]), format!("Bearer {renewed}"));
}

#[tokio::test]
async fn a_session_signed_out_before_the_services_hello_sends_nothing_and_stops_the_client() {
    let lab = Lab::start().await;
    lab.trouter.delay_hello_once(Duration::from_secs(1));
    let mut push = lab.client().spawn();
    sockets_until(&lab.trouter, |sockets| sockets.len() == 1).await;
    // the connect took its token already; the session signs out while the socket waits for the service's `1::`
    lab.tokens.signed_out.store(true, Ordering::SeqCst);
    let stopped = next(&mut push, |event| match event {
        PushEvent::Stopped(stop) => Some(stop),
        _ => None,
    })
    .await;
    assert_eq!(stopped, Stop::SignedOut);
    let sockets = lab.trouter.sockets();
    assert_eq!(sockets.len(), 1, "no reconnect after the sign-out");
    assert!(
        sockets[0].frames.is_empty(),
        "no user.authenticate with the token held from before: {:?}",
        sockets[0].frames
    );
    push.stop().await;
}

#[tokio::test]
async fn a_renewed_token_that_expires_no_later_still_means_a_reconnect() {
    let lab = Lab::start().await;
    let mut push = lab.client().spawn();
    connected(&mut push).await;
    let held = lab.tokens.current();
    let renewed = "mock-ic3-token-renewed-b-4";
    lab.mock.set_token(LAB_B, renewed);
    let earlier = held.expires_at.checked_sub(Duration::from_secs(60)).unwrap();
    lab.tokens.renew(renewed, earlier);
    assert!(matches!(down(&mut push).await, Down::Renewal));
    connected(&mut push).await;
    assert_eq!(authorization(&lab.trouter.sockets()[1]), format!("Bearer {renewed}"));
    push.stop().await;
    sent_only_own_frames(&lab).await;
}

#[tokio::test]
async fn a_stop_while_the_socket_opens_ends_the_client_at_once() {
    let lab = Lab::start().await;
    // a port that takes the TCP connection and never answers the WebSocket handshake
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("ws://{}", silent.local_addr().unwrap());
    let transport = HttpTransport::with_mock_origin(Arc::clone(&lab.tokens), CHAT_BASE, &lab.mock.origin()).unwrap();
    let push = Trouter::with_mock_origin(Arc::new(transport), Arc::clone(&lab.tokens), &origin)
        .unwrap()
        .with_options(Options {
            connect_timeout: Duration::from_secs(60),
            ..options()
        })
        .spawn();
    let (opening, _) = tokio::time::timeout(WAIT, silent.accept()).await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(5), push.stop())
        .await
        .expect("the stop waited for the socket to open");
    drop(opening);
}

/// Every log record of the process, as its target and its text.
struct Captured(Mutex<Vec<String>>);

#[cfg(test)]
impl log::Log for Captured {
    fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        let line = format!("{} {}", record.target(), record.args());
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push(line);
    }

    fn flush(&self) {}
}

static CAPTURED: Captured = Captured(Mutex::new(Vec::new()));

#[tokio::test]
async fn no_log_record_carries_the_token_even_with_a_logger_at_trace() {
    // tungstenite traces each frame it sends, the authentication's bearer token included: the cap compiles that out
    assert!(log::STATIC_MAX_LEVEL <= log::LevelFilter::Debug);
    log::set_logger(&CAPTURED).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let lab = Lab::start().await;
    let mut push = lab.client().spawn();
    connected(&mut push).await;
    registered(&mut push).await;
    lab.post("<p>logged?</p>", "1000000000000000301").await;
    chat_event(&mut push).await;
    push.stop().await;
    sent_only_own_frames(&lab).await;
    let token = lab.mock.token(LAB_B).unwrap();
    let records = CAPTURED.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(!records.is_empty(), "the logger was live");
    for record in &records {
        let target = record.split_once(' ').map_or("", |(target, _)| target);
        assert!(!record.contains(&token), "a record of {target} carries the token");
    }
}

#[tokio::test]
async fn own_cloud_authenticates_locally_but_refuses_microsoft_push_and_reconnect_urls() {
    let lab = Lab::start().await;
    let cloud = teams_proto::Cloud::own("https://chat.example:8443").unwrap();
    let transport = HttpTransport::for_cloud_with_mock_origin(
        Arc::clone(&lab.tokens),
        "https://chat.example:8443/api/chatsvc/amer",
        cloud.clone(),
        &lab.mock.origin(),
    )
    .unwrap();
    let client = Trouter::with_mock_origin(Arc::new(transport), Arc::clone(&lab.tokens), lab.trouter.origin())
        .unwrap()
        .for_cloud(cloud)
        .with_options(options());
    let opened = client.opened_urls();
    let mut push = client.spawn();
    for attempt in 0..2 {
        if attempt != 0 {
            lab.trouter.drop_sockets(LAB_B);
        }
        connected(&mut push).await;
        next(&mut push, |event| match event {
            PushEvent::Registration(RegistrationReport::Failed(_)) => Some(()),
            _ => None,
        })
        .await;
    }
    push.stop().await;
    assert!(lab.trouter.registrations().is_empty());
    let urls = opened.urls();
    assert_eq!(urls.len(), 2);
    for url in urls {
        assert_eq!(url.host_str(), Some("chat.example"));
        assert_eq!(url.port(), Some(8443));
        assert_eq!(url.path(), "/trouter/v4/c");
    }
    assert_eq!(lab.trouter.sockets().len(), 2);
}
