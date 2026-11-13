//! The engine, as native clients of the lab accounts run it against the mock chat service and its Trouter
//! (`teams-mock`, reached through the `test-support` mock origins), each engine on its own thread and runtime: a sends
//! and b receives through its push channel, never answering a delivery; an edit, a reaction (and taking it back) and
//! a delete arrive; a read receipt; the lab's group of three under the fence's live read (and any group chat the lab
//! didn't record denied); a new chat, under a stand-in for the group member read (the live read denies a group chat
//! the native client made, which is checked too); the catch-up after a dropped socket, each message stored
//! once; a write throttled once that goes out from the engine's tick; a write whose check can't sync, answered as the
//! row that waits; a post whose answer was lost and whose check couldn't sync, settled later by a catch-up or by its
//! own echo over the push channel and told once either way; such a post whose copy no read can take, met first by a
//! check from the tick and told last as the row stands, waiting or given up; a delivery whose conversation can't be
//! read, caught up from the tick; and an engine without a push channel that syncs on demand and refuses what the
//! store says can't be done.
//!
//! Every write goes through each account's own fence over the lab roster, the 1:1 chats' members read live from the
//! mock. Every id and token here is the mock's made-up one, never a real one.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fmt::Write as _;
use std::future::{self, Future};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU16, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{
    self, Action, Cause, EngineError, EngineHandle, EngineOptions, Event, MessageChange, Parts, Push, Source,
};
use teams_core::search::{Backfill, Step};
use teams_core::{ChatHistory, Clock, MAX_FAILED_CHECKS, Outbox, Transient, TransientSent};
use teams_mock::{
    AB_CHAT, ABC_CHAT, CHAT_BASE, ChatServiceMock, EventKind, LAB_A, LAB_B, LAB_C, OUTSIDER, TrouterMock,
};
use teams_proto::chat::MessageText;
use teams_proto::{
    Author, Client, Credentials, HttpError, HttpTransport, Read, Request as ProtoRequest, Response, Transport,
};
use teams_proto::{Mri, is_group_chat_id};
use teams_store::{
    Applied, Conversation, ConversationKind, Message, OutboxEntry, OutboxKind, OutboxState, SearchHit, Store,
    StoreError,
};
use teams_trouter::{Options, RegistrationReport, Trouter, TrouterCredentials};
use tokio::sync::{broadcast, watch};

/// How long a test waits for an event before it fails.
const WAIT: Duration = Duration::from_secs(30);

/// a's message of `messages.json` that is still standing, with a `clientmessageid`.
const A_EDITED: &str = "1760000010000";
/// a's message of `messages.json` that was deleted.
const A_DELETED: &str = "1760000060000";

// --- fakes ---

/// One account's mock token, for the transport and the push channel alike; it never runs out in a test.
struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Tokens {
    fn of(mock: &ChatServiceMock, mri: &str) -> Arc<Self> {
        Arc::new(Self {
            token: mock.token(mri).unwrap(),
            renewals: broadcast::channel(4).0,
        })
    }

    fn access(&self) -> AccessToken {
        AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }
    }
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
        future::ready(Ok(self.access()))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

/// The reads' token, which fails while `failing` is set: every read of the engine's client (the catch-up's, the
/// outbox's check, a thread read) fails before it goes out, as an unreachable service would fail it. While `held` is
/// set, every such read waits before it goes out, as a slow service would hold it.
struct Reads {
    tokens: Arc<Tokens>,
    failing: Arc<AtomicBool>,
    held: Arc<watch::Sender<bool>>,
}

impl Reads {
    fn token(&self) -> Result<Secret, TokenError> {
        if self.failing.load(Ordering::SeqCst) {
            return Err(TokenError::UnexpectedResponse { status: 503 });
        }
        Ok(Secret::new(self.tokens.token.clone()))
    }
}

impl Reads {
    /// The token once the reads aren't held.
    fn released(&self) -> impl Future<Output = Result<Secret, TokenError>> + Send + use<> {
        let mut held = self.held.subscribe();
        let token = self.token();
        async move {
            let _ = held.wait_for(|held| !held).await;
            token
        }
    }
}

impl Credentials for Reads {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.released()
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.released()
    }
}

/// The outbox's HTTP transport, whose next write can be answered before it reaches the service with the status
/// `next` holds (0: none): a 429 with a short `Retry-After`, any other with no body; or, while `lose` is set, reaches
/// the service and has its answer lost (a 500 in its place), once.
struct Interposed {
    inner: Arc<HttpTransport<Tokens>>,
    next: Arc<AtomicU16>,
    lose: Arc<AtomicBool>,
    held: Arc<watch::Sender<bool>>,
}

impl Transport for Interposed {
    type Error = HttpError;

    fn read<R: Read>(&self, request: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        self.inner.read(request)
    }

    fn send<R: ProtoRequest>(&self, request: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        self.inner.send(request)
    }

    async fn deliver<R: teams_proto::Request + Send>(&self, write: R) -> Result<Response, HttpError> {
        let mut held = self.held.subscribe();
        let _ = held.wait_for(|held| !held).await;
        if self.lose.swap(false, Ordering::SeqCst) {
            self.inner.deliver(write).await?;
            return Ok(Response::new(500, Vec::new()));
        }
        match self.next.swap(0, Ordering::SeqCst) {
            0 => self.inner.deliver(write).await,
            429 => Ok(Response::new(429, Vec::new()).with_retry_after(Duration::from_millis(300))),
            status => Ok(Response::new(status, Vec::new())),
        }
    }
}

// --- setup ---

struct Lab {
    mock: ChatServiceMock,
    trouter: TrouterMock,
}

impl Lab {
    async fn start() -> Self {
        let mock = ChatServiceMock::start().await.unwrap();
        let trouter = mock.start_trouter().unwrap();
        Self { mock, trouter }
    }

    /// The posts the mock got to `conversation`'s messages.
    async fn posts_to(&self, conversation: &str) -> usize {
        let path = format!("/api/chatsvc/amer/v1/users/ME/conversations/{conversation}/messages");
        self.mock
            .received_requests()
            .await
            .iter()
            .filter(|request| request.method.as_str() == "POST" && decoded(request.url.path()) == path)
            .count()
    }

    /// The reads of `conversation`'s member list the mock got (the fence's live member read).
    async fn member_reads_of(&self, conversation: &str) -> usize {
        let path = format!("/api/chatsvc/amer/v1/threads/{conversation}/members");
        self.mock
            .received_requests()
            .await
            .iter()
            .filter(|request| request.method.as_str() == "GET" && decoded(request.url.path()) == path)
            .count()
    }
}

fn decoded(path: &str) -> String {
    path.replace("%3A", ":").replace("%40", "@")
}

/// How a native client is set up.
#[derive(Clone, Copy)]
struct How {
    push: bool,
    /// The first write is throttled ([`Interposed`]).
    throttle_first_write: bool,
    /// The push channel's reconnect wait.
    backoff: Duration,
}

impl Default for How {
    fn default() -> Self {
        Self {
            push: true,
            throttle_first_write: false,
            backoff: Duration::from_millis(50),
        }
    }
}

/// A lab account's native client: its engine, its `fence.log`, every event it took so far, and the switches of its
/// fakes.
struct Native {
    handle: EngineHandle,
    me: &'static str,
    seen: String,
    /// The engine's reads fail while it is set ([`Reads`]).
    reads_fail: Arc<AtomicBool>,
    /// The engine's reads wait while it is set ([`Reads`]).
    reads_held: Arc<watch::Sender<bool>>,
    /// The status the next write is answered with before it reaches the service ([`Interposed`]).
    next_write: Arc<AtomicU16>,
    /// The next write reaches the service and its answer is lost ([`Interposed`]).
    lose_answer: Arc<AtomicBool>,
    /// The receipt transport waits while set. Local reads must still complete.
    writes_held: Arc<watch::Sender<bool>>,
    /// How far ahead of the wall clock the outbox's clock runs, in milliseconds (a row's backoff without waiting).
    ahead: Arc<AtomicI64>,
    /// An [`Event::Failed`] is taken like any other event instead of failing the test.
    failures_expected: bool,
    /// Where its store is, kept while it runs; another engine on the account opens the store there ([`try_native_in`]).
    _dir: Arc<tempfile::TempDir>,
}

fn trouter_options(backoff: Duration) -> Options {
    Options {
        ping_interval: Duration::from_secs(2),
        pong_timeout: Duration::from_secs(5),
        connect_timeout: Duration::from_secs(5),
        hello_timeout: Duration::from_secs(5),
        backoff_base: backoff,
        backoff_cap: backoff,
        backoff_jitter_percent: 0,
        ..Options::default()
    }
}

/// `me`'s native client against the lab's mocks, set up as `how` says, on a store of its own.
fn native(lab: &Lab, me: &'static str, how: How) -> Native {
    try_native_in(lab, me, how, Arc::new(tempfile::tempdir().unwrap())).unwrap()
}

/// The engine's pace in these tests: a fast tick, a short wait before a failed catch-up is tried again, and small
/// batches of the search backfill.
fn fast_options() -> EngineOptions {
    EngineOptions {
        tick: Duration::from_millis(50),
        catch_up_retry: Duration::from_millis(500),
        index_batch: 3,
        ..EngineOptions::default()
    }
}

/// `me`'s native client as [`native`] makes it, on the store in `dir` (another engine's, perhaps), with a `fence.log`
/// of its own; the engine's refusal when it can't start.
fn try_native_in(lab: &Lab, me: &'static str, how: How, dir: Arc<tempfile::TempDir>) -> Result<Native, EngineError> {
    let tokens = Tokens::of(&lab.mock, me);

    let origin = lab.mock.origin();
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin).unwrap();
    let transport = Arc::new(http());
    // the fence's live member read, against the mock: any chat the mock knows, group chats included

    let author = Author::new(Mri::new(me), "Lab account");
    let reads_fail = Arc::new(AtomicBool::new(false));
    let reads_held = Arc::new(watch::Sender::new(false));
    let reads = Arc::new(Reads {
        tokens: Arc::clone(&tokens),
        failing: Arc::clone(&reads_fail),
        held: Arc::clone(&reads_held),
    });
    let client = Arc::new(Client::new(
        HttpTransport::with_mock_origin(reads, CHAT_BASE, &origin)
            .unwrap()
            .with_middle_tier(teams_mock::MIDDLE_TIER_BASE)
            .unwrap(),
    ));
    let push = how.push.then(|| {
        let trouter = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), lab.trouter.origin())
            .unwrap()
            .with_options(trouter_options(how.backoff));
        Push::new(trouter)
    });
    let store = Store::open(&dir.path().join("store").join("lab.sqlite")).unwrap();
    let next_write = Arc::new(AtomicU16::new(if how.throttle_first_write { 429 } else { 0 }));
    let lose_answer = Arc::new(AtomicBool::new(false));
    let writes_held = Arc::new(watch::Sender::new(false));
    let interposed = Interposed {
        inner: transport,
        next: Arc::clone(&next_write),
        lose: Arc::clone(&lose_answer),
        held: Arc::clone(&writes_held),
    };
    let ahead = Arc::new(AtomicI64::new(0));
    let outbox = Outbox::new(Arc::new(interposed), author).with_clock(clock_ahead(&ahead));
    let handle = engine::start(Parts {
        calls: None,
        store,
        client,
        outbox,
        push,
        options: fast_options(),
        known_names: Vec::new(),
    })?;
    Ok(Native {
        handle,
        me,
        seen: String::new(),
        reads_fail,
        reads_held,
        next_write,
        lose_answer,
        writes_held,
        ahead,
        failures_expected: false,
        _dir: dir,
    })
}

impl Native {
    /// The next event `pick` takes, skipping others; fails after [`WAIT`] with every event seen.
    async fn event<T>(&mut self, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let Ok(event) = tokio::time::timeout_at(deadline, self.handle.next_event()).await else {
                panic!("{}: the event didn't come in time; seen:\n{}", self.me, self.seen);
            };
            let event = event.expect("the engine stopped");
            let _ = writeln!(self.seen, "{event:?}");
            if let Event::Failed { during, error } = &event
                && !self.failures_expected
            {
                panic!("{}: failed {during}: {error}", self.me);
            }
            if let Some(picked) = pick(&event) {
                return picked;
            }
        }
    }

    /// Waits until the push channel is registered and the catch-up after its connect has run.
    async fn ready(&mut self) {
        let (mut registered, mut caught_up) = (false, false);
        while !(registered && caught_up) {
            self.event(|event| match event {
                Event::Registration(RegistrationReport::Registered) => {
                    registered = true;
                    Some(())
                }
                Event::Registration(RegistrationReport::Failed(why)) => panic!("registration failed: {why}"),
                Event::CaughtUp { cause, report } => {
                    assert!(report.failed.is_empty(), "{report:?}");
                    assert_eq!(*cause, Cause::Connected);
                    caught_up = true;
                    Some(())
                }
                Event::CatchUpFailed { error, .. } => panic!("the catch-up failed: {error}"),
                _ => None,
            })
            .await;
        }
    }

    /// The next message event about `id`: what changed, what the store did, and the row as stored.
    async fn message_event(&mut self, conversation: &str, id: &str) -> (MessageChange, Applied, Message) {
        self.event(|event| match event {
            Event::Message {
                conversation: got,
                id: got_id,
                change,
                applied,
                message,
                ..
            } if got == conversation && got_id == id => Some((*change, *applied, *message.clone().unwrap())),
            _ => None,
        })
        .await
    }

    async fn write(&self, action: Action) -> OutboxEntry {
        self.handle.write(action).await.unwrap()
    }

    /// Sends `text` to `conversation`; the row, sent.
    async fn send(&self, conversation: &str, text: &str) -> OutboxEntry {
        let row = self.write(send(conversation, text)).await;
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        assert!(row.message_id.is_some(), "{row:?}");
        row
    }

    /// Sends `text` to `conversation`: the post reaches the service, its answer is lost, and the reads fail, so its
    /// check can't sync. The row as it waits, told as such; the reads work again afterwards.
    async fn send_unanswered(&mut self, conversation: &str, text: &str) -> OutboxEntry {
        self.lose_answer.store(true, Ordering::SeqCst);
        self.reads_fail.store(true, Ordering::SeqCst);
        let row = self.write(send(conversation, text)).await;
        assert_eq!(row.state, OutboxState::Pending, "{row:?}");
        assert_eq!((row.attempts, row.failed_checks), (1, 1), "{row:?}");
        let told = self
            .event(|event| match event {
                Event::Outbox(entry) if entry.id == row.id => Some(entry.clone()),
                _ => None,
            })
            .await;
        assert_eq!(told, row);
        self.reads_fail.store(false, Ordering::SeqCst);
        row
    }

    /// Moves the outbox's clock past row `id`'s backoff, so the tick checks it again; the row as that check left it,
    /// told with `failed_checks` checks that couldn't tell whether it arrived.
    async fn checked_from_the_tick(&mut self, id: i64, failed_checks: u32) -> OutboxEntry {
        self.ahead.fetch_add(3_600_000, Ordering::SeqCst);
        self.event(|event| match event {
            Event::Outbox(entry) if entry.id == id && entry.failed_checks == failed_checks => Some(entry.clone()),
            _ => None,
        })
        .await
    }

    /// The events waiting now, taken: the outbox row `id` as each of them told it. A request answered first makes
    /// sure the engine is done with what came before it.
    async fn told_rows(&mut self, id: i64) -> Vec<OutboxEntry> {
        self.handle.conversations(1).await.unwrap();
        let mut rows = Vec::new();
        while let Some(event) = self.handle.try_next_event() {
            let _ = writeln!(self.seen, "{event:?}");
            if let Event::Outbox(row) = event
                && row.id == id
            {
                rows.push(row);
            }
        }
        rows
    }

    /// Every event waiting now, taken; a request answered first makes sure the engine is done with what came before.
    async fn drain(&mut self) -> Vec<Event> {
        self.handle.conversations(1).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = self.handle.try_next_event() {
            let _ = writeln!(self.seen, "{event:?}");
            events.push(event);
        }
        events
    }

    async fn stored(&self, conversation: &str, id: &str) -> Message {
        self.handle.message(conversation, id).await.unwrap().unwrap()
    }

    /// How many times the store holds message `id` of `conversation`.
    async fn copies(&self, conversation: &str, id: &str) -> usize {
        let page = self.handle.messages(conversation, None, 1_000).await.unwrap();
        page.messages.iter().filter(|message| message.id == id).count()
    }

    fn shutdown(self) {
        self.handle.shutdown();
    }
}

fn send(conversation: &str, text: &str) -> Action {
    Action::Send {
        conversation: conversation.to_owned(),
        text: MessageText::plain(text).unwrap(),
    }
}

/// The outbox's clock: the wall clock, `ahead` milliseconds on.
fn clock_ahead(ahead: &Arc<AtomicI64>) -> Clock {
    let ahead = Arc::clone(ahead);
    Arc::new(move || now_ms() + ahead.load(Ordering::SeqCst))
}

/// The wall clock in milliseconds since the Unix epoch.
fn now_ms() -> i64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    i64::try_from(since.as_millis()).unwrap()
}

fn id_of(row: &OutboxEntry) -> String {
    row.message_id.clone().unwrap()
}

/// Waits until none of `account`'s sockets on the Trouter mock is open.
async fn sockets_closed(trouter: &TrouterMock, account: &str) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let open = trouter
            .sockets()
            .iter()
            .any(|socket| socket.account.as_deref() == Some(account) && socket.open);
        if !open {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "{account}'s socket stayed open");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// --- tests ---

#[tokio::test]
async fn a_message_from_a_reaches_b_over_its_push_channel_and_no_delivery_is_answered() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    let mut b = native(&lab, LAB_B, How::default());
    a.ready().await;
    b.ready().await;

    let row = a.send(AB_CHAT, "hello b, from the engine of a").await;
    let id = id_of(&row);
    let (change, applied, message) = b.message_event(AB_CHAT, &id).await;
    assert_eq!((change, applied), (MessageChange::New, Applied::Inserted));
    assert!(
        message.content.contains("hello b, from the engine of a"),
        "{}",
        message.content
    );
    assert_eq!(message.sender.as_deref(), Some(LAB_A));
    assert_eq!(
        message.client_message_id.as_deref(),
        Some(row.client_message_id.as_str())
    );
    assert_eq!(b.copies(AB_CHAT, &id).await, 1);

    // a's own message is in a's store too (the sync after the write), with the row's clientmessageid
    let mine = a.stored(AB_CHAT, &id).await;
    assert_eq!(mine.client_message_id.as_deref(), Some(row.client_message_id.as_str()));
    // a's fence decided the one write, and allowed it

    // b's socket sent nothing that answers a delivery, and no frame was written to fence.log (a frame isn't a
    // contacting write)

    let sent: Vec<String> = lab
        .trouter
        .sockets()
        .into_iter()
        .filter(|socket| socket.account.as_deref() == Some(LAB_B))
        .flat_map(|socket| socket.frames)
        .collect();
    assert!(!sent.is_empty(), "b's socket sent nothing");
    assert!(
        sent.iter()
            .all(|frame| !frame.starts_with("3:") && !frame.starts_with("6:")),
        "{sent:?}"
    );

    // stopping b's engine ends its push channel
    b.shutdown();
    sockets_closed(&lab.trouter, LAB_B).await;
    a.shutdown();
}

#[tokio::test]
async fn text_edits_preserve_original_attachments_without_leaking_them_to_other_messages() {
    let lab = Lab::start().await;
    let files = json!([
        {
            "@type": "http://schema.skype.com/File",
            "fileName": "preserved.pdf",
            "fileInfo": {"shareUrl": "https://contoso.sharepoint.com/preserved.pdf", "unknown": [1, null, "x"]},
            "unknown": {"keep": true}
        },
        {"futureAttachmentShape": {"keep": true}}
    ])
    .to_string();
    let cards = json!([{"futureCard": {"keep": ["opaque", null]}}]).to_string();
    assert!(lab.mock.change_message(AB_CHAT, "1111111111111111111", |message| {
        message["properties"]["files"] = json!(files);
        message["properties"]["cards"] = json!(cards);
    }));
    let account = native(&lab, LAB_A, QUIET);
    account.handle.sync().await.unwrap();
    let edit = account
        .write(Action::Edit {
            conversation: AB_CHAT.to_owned(),
            message_id: A_EDITED.to_owned(),
            text: MessageText::plain("edited text with original attachments").unwrap(),
        })
        .await;
    assert_eq!(edit.state, OutboxState::Sent, "{edit:?}");
    let requests = lab.mock.received_requests().await;
    let put = requests
        .iter()
        .rev()
        .find(|request| request.method.as_str() == "PUT")
        .unwrap();
    let body: Value = put.body_json().unwrap();
    assert_eq!(body["properties"]["files"], files);
    assert_eq!(body["properties"]["cards"], cards);

    account.handle.sync().await.unwrap();
    let stored = account.stored(AB_CHAT, A_EDITED).await;
    let properties: Value = serde_json::from_str(stored.properties.as_deref().unwrap()).unwrap();
    assert_eq!(properties["files"], files);
    assert_eq!(properties["cards"], cards);
    let rendered = teams_proto::chat::shared_files(properties.get("files"));
    assert_eq!(rendered.files.len(), 1);
    assert_eq!(rendered.files[0].name, "preserved.pdf");
    assert_eq!(
        rendered.unread,
        Some(json!([{"futureAttachmentShape": {"keep": true}}]))
    );
    assert!(stored.content.contains("edited text with original attachments"));

    let fresh = account.send(AB_CHAT, "fresh text without attachments").await;
    account.handle.sync().await.unwrap();
    let fresh_edit = account
        .write(Action::Edit {
            conversation: AB_CHAT.to_owned(),
            message_id: id_of(&fresh),
            text: MessageText::plain("another edit without attachments").unwrap(),
        })
        .await;
    assert_eq!(fresh_edit.state, OutboxState::Sent, "{fresh_edit:?}");
    let requests = lab.mock.received_requests().await;
    for method in ["POST", "PUT"] {
        let request = requests
            .iter()
            .rev()
            .find(|request| request.method.as_str() == method)
            .unwrap();
        let body: Value = request.body_json().unwrap();
        assert_eq!(body["properties"]["files"], "[]", "{method}");
        assert_eq!(body["properties"]["cards"], "[]", "{method}");
    }
    account.shutdown();
}

#[tokio::test]
async fn edits_refuse_unreadable_outer_properties_before_queuing() {
    let lab = Lab::start().await;
    for properties in ["{", "null", "[]", "\"not an object\""] {
        let account = native_with_unreadable_properties(&lab, properties);
        let error = account
            .handle
            .write(Action::Edit {
                conversation: AB_CHAT.to_owned(),
                message_id: A_EDITED.to_owned(),
                text: MessageText::plain("do not erase unknown metadata").unwrap(),
            })
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                EngineError::Unknown {
                    missing: "attachment properties",
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(account.handle.outbox_unsettled(AB_CHAT).await.unwrap().is_empty());
        account.shutdown();
    }
    assert!(lab.mock.received_requests().await.is_empty());
}

/// An earlier store with unreadable outer properties; only the engine opens it once started.
fn native_with_unreadable_properties(lab: &Lab, properties: &str) -> Native {
    let dir = Arc::new(tempfile::tempdir().unwrap());
    {
        let mut store = Store::open(&dir.path().join("store").join("lab.sqlite")).unwrap();
        store
            .upsert_conversation(&Conversation {
                id: AB_CHAT.to_owned(),
                kind: ConversationKind::OneToOne,
                topic: None,
                version: 1,
                last_activity: None,
            })
            .unwrap();
        store
            .upsert_message(
                &Message {
                    conversation_id: AB_CHAT.to_owned(),
                    id: A_EDITED.to_owned(),
                    version: 1,
                    client_message_id: Some("1111111111111111111".to_owned()),
                    sender: Some(LAB_A.to_owned()),
                    sender_name: None,
                    message_type: "RichText/Html".to_owned(),
                    content: "<p>original text</p>".to_owned(),
                    properties: Some(properties.to_owned()),
                    arrival_time: 1_760_000_000_000,
                    compose_time: Some(1_760_000_000_000),
                    edit_time: None,
                    delete_time: None,
                },
                None,
            )
            .unwrap();
    }
    try_native_in(lab, LAB_A, QUIET, dir).unwrap()
}

#[tokio::test]
async fn an_edit_a_reaction_its_take_back_and_a_delete_arrive_on_b() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    let mut b = native(&lab, LAB_B, How::default());
    a.ready().await;
    b.ready().await;
    let id = id_of(&a.send(AB_CHAT, "a first draft").await);
    b.message_event(AB_CHAT, &id).await;

    let edit = a
        .write(Action::Edit {
            conversation: AB_CHAT.to_owned(),
            message_id: id.clone(),
            text: MessageText::plain("the second draft").unwrap(),
        })
        .await;
    assert_eq!(
        (edit.kind, edit.state),
        (OutboxKind::Edit, OutboxState::Sent),
        "{edit:?}"
    );
    let (change, applied, message) = b.message_event(AB_CHAT, &id).await;
    assert_eq!((change, applied), (MessageChange::Edit, Applied::Updated));
    assert!(message.content.contains("the second draft"), "{}", message.content);
    assert!(message.edit_time.is_some());
    assert!(a.stored(AB_CHAT, &id).await.content.contains("the second draft"));

    let react = Action::React {
        conversation: AB_CHAT.to_owned(),
        message_id: id.clone(),
        key: "like".to_owned(),
    };
    assert_eq!(a.write(react).await.state, OutboxState::Sent);
    let (change, applied, _) = b.message_event(AB_CHAT, &id).await;
    assert_eq!((change, applied), (MessageChange::Reaction, Applied::Updated));
    let reactions = b.handle.reactions(AB_CHAT, &id).await.unwrap();
    assert_eq!(
        reactions
            .iter()
            .map(|reaction| (reaction.key.as_str(), reaction.user.as_str()))
            .collect::<Vec<_>>(),
        [("like", LAB_A)]
    );

    let unreact = Action::Unreact {
        conversation: AB_CHAT.to_owned(),
        message_id: id.clone(),
        key: "like".to_owned(),
    };
    assert_eq!(a.write(unreact).await.kind, OutboxKind::RemoveReaction);
    let (change, _, _) = b.message_event(AB_CHAT, &id).await;
    assert_eq!(change, MessageChange::Reaction);
    assert!(b.handle.reactions(AB_CHAT, &id).await.unwrap().is_empty());

    let delete = Action::Delete {
        conversation: AB_CHAT.to_owned(),
        message_id: id.clone(),
    };
    assert_eq!(a.write(delete).await.state, OutboxState::Sent);
    let (change, applied, message) = b.message_event(AB_CHAT, &id).await;
    assert_eq!((change, applied), (MessageChange::Delete, Applied::Updated));
    assert!(
        message.delete_time.is_some() && message.content.is_empty(),
        "{message:?}"
    );
    assert_eq!(b.copies(AB_CHAT, &id).await, 1);

    // every write was a fresh decision of a's fence, each allowed

    a.shutdown();
    b.shutdown();
}

#[tokio::test]
async fn a_read_receipt_moves_b_s_horizon_as_the_service_sees_it() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    let mut b = native(&lab, LAB_B, How::default());
    a.ready().await;
    b.ready().await;
    let sent = a.send(AB_CHAT, "read me").await;
    let id = id_of(&sent);
    b.message_event(AB_CHAT, &id).await;

    let read = b
        .write(Action::Read {
            conversation: AB_CHAT.to_owned(),
            message_id: id.clone(),
        })
        .await;
    assert_eq!(
        (read.kind, read.state),
        (OutboxKind::ReadReceipt, OutboxState::Sent),
        "{read:?}"
    );
    // a is told nothing over the push channel (no capture showed it), but the service has b's horizon
    let told = lab.mock.take_events(LAB_A);
    let receipt = told
        .iter()
        .find(|event| event.kind == EventKind::ReadReceipt && event.from == LAB_B)
        .unwrap();
    let horizon = receipt.resource["consumptionhorizon"].as_str().unwrap();
    assert!(horizon.starts_with(&format!("{id};")), "{horizon}");
    assert!(horizon.ends_with(&format!(";{}", sent.client_message_id)), "{horizon}");

    a.shutdown();
    b.shutdown();
}

/// The server response is deliberately held. The badge's queries must see local progress before it arrives,
/// while a later incoming message remains unread and stale deliveries cannot undo what was actually seen.
#[tokio::test]
async fn attachment_sharing_keeps_store_queries_and_seen_receipts_responsive() {
    let lab = Lab::start().await;
    let b = native(&lab, LAB_B, QUIET);
    let id = lab.mock.post_as(LAB_A, ABC_CHAT, "<p>seen while sharing</p>").unwrap();
    b.handle.sync().await.unwrap();
    let site = teams_proto::DriveSite::from_web_url("https://madeup-my.sharepoint.com/personal/lab_b/").unwrap();
    let file = teams_proto::UploadedFile::from_upload(
        &site,
        &json!({
            "id":"SyntheticFile","name":"demo.txt","size":3,
            "webUrl":"https://madeup-my.sharepoint.com/personal/lab_b/Documents/demo.txt",
            "parentReference":{"driveId":"synthetic-drive"},
            "sharepointIds":{"listItemUniqueId":"00000000-0000-4000-8000-000000000001"}
        })
        .to_string()
        .into_bytes(),
    )
    .unwrap();
    b.reads_held.send_replace(true);
    let sending = b.handle.write(Action::Send {
        conversation: ABC_CHAT.to_owned(),
        text: MessageText::plain("attached").unwrap().with_files(vec![file]),
    });
    let receipt = b.handle.write(Action::Read {
        conversation: ABC_CHAT.to_owned(),
        message_id: id,
    });
    let count = tokio::time::timeout(Duration::from_secs(2), b.handle.unread_count(ABC_CHAT)).await;
    b.reads_held.send_replace(false);
    assert!(sending.await.is_err(), "the mock has no file-profile service");
    receipt.await.unwrap();
    b.shutdown();
    assert_eq!(count.expect("file sharing must serve local requests").unwrap(), 0);
}

/// New message for some people opens the stored group chat of exactly them (the account among them); a 1:1 chat is
/// no group chat, and a roster with anyone else isn't theirs.
#[tokio::test]
async fn new_message_finds_the_stored_group_chat_of_exactly_its_people() {
    let lab = Lab::start().await;
    let b = native(&lab, LAB_B, QUIET);
    b.handle.sync().await.unwrap();
    let people = |mris: &[&str]| mris.iter().map(|mri| (*mri).to_owned()).collect::<Vec<_>>();
    let found = b.handle.group_chat_with(people(&[LAB_C, LAB_A, LAB_B])).await.unwrap();
    let pair = b.handle.group_chat_with(people(&[LAB_A, LAB_B])).await.unwrap();
    let nobody = b.handle.group_chat_with(Vec::new()).await.unwrap();
    let call_runs = b.handle.call_runs(ABC_CHAT).await.unwrap();
    let call_ended = b.handle.call_ended(ABC_CHAT, "1").await.unwrap();
    b.shutdown();
    assert!(!call_runs, "no call started in it");
    assert!(!call_ended, "no call start stored in it");
    assert_eq!(found.as_deref(), Some(ABC_CHAT));
    assert_eq!(pair, None, "the a-b chat is a 1:1 chat");
    assert_eq!(nobody, None);
}

#[tokio::test]
async fn a_seen_message_clears_unread_before_the_receipt_server_answers_and_keeps_newer_messages_unread() {
    let lab = Lab::start().await;
    let mut b = native(&lab, LAB_B, How::default());
    b.ready().await;
    let first = lab.mock.post_as(LAB_A, ABC_CHAT, "<p>seen</p>").unwrap();
    b.message_event(ABC_CHAT, &first).await;
    assert!(b.handle.unread_count(ABC_CHAT).await.unwrap() > 0);
    b.writes_held.send_replace(true);
    let receipt = tokio::spawn(b.handle.write(Action::Read {
        conversation: ABC_CHAT.to_owned(),
        message_id: first.clone(),
    }));
    b.event(|event| matches!(event, Event::ReadHorizon { conversation } if conversation == ABC_CHAT).then_some(()))
        .await;
    let unread = tokio::time::timeout(Duration::from_secs(5), b.handle.unread_count(ABC_CHAT)).await;
    assert_eq!(unread.expect("local count held by receipt transport").unwrap(), 0);
    assert!(!receipt.is_finished(), "the server still has not answered");
    let local = b.handle.read_horizon(ABC_CHAT).await.unwrap().unwrap();
    assert_eq!(local.last_read_arrival, b.stored(ABC_CHAT, &first).await.arrival_time);
    // This delivery waits in the push queue while the first receipt's transport is held.
    let newer = lab.mock.post_as(LAB_A, ABC_CHAT, "<p>not yet seen</p>").unwrap();
    b.writes_held.send_replace(false);
    assert_eq!(receipt.await.unwrap().unwrap().state, OutboxState::Sent);
    b.message_event(ABC_CHAT, &newer).await;
    assert_eq!(b.handle.unread_count(ABC_CHAT).await.unwrap(), 1);
    b.drain().await;
    let stale = format!("0;{};synthetic", local.updated - 1);
    lab.trouter
        .deliver_chat(LAB_B, &conversation_delivery(ABC_CHAT, Some(&stale)));
    b.event(|event| matches!(event, Event::ReadHorizon { conversation } if conversation == ABC_CHAT).then_some(()))
        .await;
    assert_eq!(
        b.handle.unread_count(ABC_CHAT).await.unwrap(),
        1,
        "old unread horizon cannot restore a badge"
    );
    // An explicit mark-unread on another client later is authoritative, even when it moves backwards.
    let marked_unread = format!("0;{};synthetic", local.updated + 10_000);
    lab.trouter
        .deliver_chat(LAB_B, &conversation_delivery(ABC_CHAT, Some(&marked_unread)));
    b.event(|event| matches!(event, Event::ReadHorizon { conversation } if conversation == ABC_CHAT).then_some(()))
        .await;
    assert!(b.handle.unread_count(ABC_CHAT).await.unwrap() > 1);
    b.shutdown();
}

#[tokio::test]
async fn a_newer_local_read_can_advance_while_an_older_receipt_waits_on_the_server() {
    let lab = Lab::start().await;
    let b = native(&lab, LAB_B, QUIET);
    let first = lab.mock.post_as(LAB_A, ABC_CHAT, "<p>first seen</p>").unwrap();
    let second = lab.mock.post_as(LAB_A, ABC_CHAT, "<p>seen later</p>").unwrap();
    b.handle.sync().await.unwrap();
    b.writes_held.send_replace(true);
    let earlier = tokio::spawn(b.handle.write(Action::Read {
        conversation: ABC_CHAT.to_owned(),
        message_id: first,
    }));
    let unread = tokio::time::timeout(Duration::from_secs(5), b.handle.unread_count(ABC_CHAT)).await;
    assert_eq!(
        unread.expect("partial count held by older receipt").unwrap(),
        1,
        "only the exact target was read"
    );
    let later = tokio::spawn(b.handle.write(Action::Read {
        conversation: ABC_CHAT.to_owned(),
        message_id: second.clone(),
    }));
    let unread = tokio::time::timeout(Duration::from_secs(5), b.handle.unread_count(ABC_CHAT)).await;
    assert_eq!(unread.expect("newer local read held by older receipt").unwrap(), 0);
    assert!(!earlier.is_finished() && !later.is_finished());
    b.writes_held.send_replace(false);
    let earlier = earlier.await.unwrap().unwrap();
    let later = later.await.unwrap().unwrap();
    assert_eq!(earlier.state, OutboxState::Sent);
    assert_eq!(later.state, OutboxState::Sent);
    assert_eq!(b.handle.unread_count(ABC_CHAT).await.unwrap(), 0);
    assert_eq!(
        b.handle
            .read_horizon(ABC_CHAT)
            .await
            .unwrap()
            .unwrap()
            .last_read_arrival,
        b.stored(ABC_CHAT, &second).await.arrival_time
    );
    let events = lab.mock.take_events(LAB_A);
    let targets: Vec<_> = events
        .iter()
        .filter(|event| event.kind == EventKind::ReadReceipt)
        .map(|event| {
            event.resource["consumptionhorizon"]
                .as_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
        })
        .collect();
    assert_eq!(targets.len(), 2, "both receipts still sync in order");
    assert_eq!(targets[1], second);
    b.shutdown();
}

#[tokio::test]
async fn seen_messages_update_locally_during_a_slow_catch_up_and_stay_read_when_the_receipt_fails() {
    let lab = Lab::start().await;
    let b = native(&lab, LAB_B, QUIET);
    lab.mock
        .post_as(LAB_A, ABC_CHAT, "<p>seen before navigating away</p>")
        .unwrap();
    b.handle.sync().await.unwrap();
    let newest = b.handle.messages(ABC_CHAT, None, 1).await.unwrap().messages.remove(0);
    b.reads_held.send_replace(true);
    let sync = b.handle.sync();
    b.next_write.store(403, Ordering::SeqCst);
    let receipt = tokio::spawn(b.handle.write(Action::Read {
        conversation: ABC_CHAT.to_owned(),
        message_id: newest.id.clone(),
    }));
    let unread = tokio::time::timeout(Duration::from_secs(5), b.handle.unread_count(ABC_CHAT)).await;
    assert_eq!(unread.expect("local receipt held by catch-up").unwrap(), 0);
    assert!(!receipt.is_finished());
    b.reads_held.send_replace(false);
    sync.await.unwrap();
    assert_eq!(receipt.await.unwrap().unwrap().state, OutboxState::Failed);
    assert_eq!(
        b.handle.unread_count(ABC_CHAT).await.unwrap(),
        0,
        "seen remains seen after server failure"
    );
    b.handle.sync().await.unwrap();
    assert_eq!(
        b.handle.unread_count(ABC_CHAT).await.unwrap(),
        0,
        "a stale catch-up cannot restore unread"
    );
    b.shutdown();
}

#[tokio::test]
async fn a_message_to_the_lab_s_group_reaches_both_others() {
    let lab = Lab::start().await;
    let how = How::default();
    let mut a = native(&lab, LAB_A, how);
    let mut b = native(&lab, LAB_B, how);
    let mut c = native(&lab, LAB_C, how);
    for native in [&mut a, &mut b, &mut c] {
        native.ready().await;
    }

    let from_a = id_of(&a.send(ABC_CHAT, "hello b and c").await);
    for native in [&mut b, &mut c] {
        let (change, applied, message) = native.message_event(ABC_CHAT, &from_a).await;
        assert_eq!((change, applied), (MessageChange::New, Applied::Inserted));
        assert_eq!(message.sender.as_deref(), Some(LAB_A));
    }
    let from_c = id_of(&c.send(ABC_CHAT, "hello a and b").await);
    for native in [&mut a, &mut b] {
        let (_, applied, message) = native.message_event(ABC_CHAT, &from_c).await;
        assert_eq!(applied, Applied::Inserted);
        assert_eq!(message.sender.as_deref(), Some(LAB_C));
    }

    for native in [a, b, c] {
        native.shutdown();
    }
}

#[tokio::test]
async fn a_group_chat_made_natively_takes_its_first_message_under_the_live_read() {
    // the live read judges any group chat by its member list, a chat the native client just created included: no lab
    // record (`chats.toml`) plays a part
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    a.ready().await;
    let created = a
        .write(Action::CreateChat {
            others: vec![Mri::new(LAB_B), Mri::new(LAB_C)],
            topic: None,
        })
        .await;
    assert_eq!(
        (created.kind, created.state),
        (OutboxKind::CreateChat, OutboxState::Sent),
        "{created:?}"
    );
    let chat = id_of(&created);
    assert!(is_group_chat_id(&chat) && chat != ABC_CHAT, "{chat}");

    let row = a.send(&chat, "the first message").await;
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");

    assert_eq!(lab.posts_to(&chat).await, 1);
    assert_eq!(
        lab.member_reads_of(&chat).await,
        0,
        "sending needs no membership policy read"
    );
    a.shutdown();
}

#[tokio::test]
async fn a_new_group_chat_is_created_and_its_first_message_brings_it_to_b_and_c() {
    let lab = Lab::start().await;
    let how = How::default();
    let mut a = native(&lab, LAB_A, how);
    let mut b = native(&lab, LAB_B, how);
    let mut c = native(&lab, LAB_C, how);
    for native in [&mut a, &mut b, &mut c] {
        native.ready().await;
    }
    let refused = a
        .handle
        .write(Action::CreateChat {
            others: Vec::new(),
            topic: None,
        })
        .await;
    assert!(matches!(refused, Err(EngineError::NotAChat)), "{refused:?}");
    let refused = a
        .handle
        .write(Action::CreateChat {
            others: vec![Mri::new(LAB_A)],
            topic: None,
        })
        .await;
    assert!(matches!(refused, Err(EngineError::NotAChat)), "{refused:?}");

    let created = a
        .write(Action::CreateChat {
            others: vec![Mri::new(LAB_B), Mri::new(LAB_C)],
            topic: None,
        })
        .await;
    assert_eq!(
        (created.kind, created.state),
        (OutboxKind::CreateChat, OutboxState::Sent),
        "{created:?}"
    );
    let chat = id_of(&created);
    assert!(is_group_chat_id(&chat), "{chat}");
    // the new chat is in a's store with its members (synced after the write)
    let mut members: Vec<String> = a
        .handle
        .members(chat.as_str())
        .await
        .unwrap()
        .into_iter()
        .map(|member| member.mri)
        .collect();
    members.sort();
    assert_eq!(members, [LAB_A, LAB_B, LAB_C]);

    let first = id_of(&a.send(&chat, "the new chat's first message").await);
    for native in [&mut b, &mut c] {
        let (_, applied, _) = native.message_event(&chat, &first).await;
        assert_eq!(applied, Applied::Inserted);
        let conversations = native.handle.conversations(100).await.unwrap();
        assert!(conversations.iter().any(|conversation| conversation.id == chat));
        assert_eq!(native.copies(&chat, &first).await, 1);
    }
    for native in [a, b, c] {
        native.shutdown();
    }
}

#[tokio::test]
async fn people_added_to_a_group_chat_join_its_members_with_the_history_chosen() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    a.ready().await;
    let created = a
        .write(Action::CreateChat {
            others: vec![Mri::new(LAB_B), Mri::new(LAB_C)],
            topic: None,
        })
        .await;
    let chat = id_of(&created);
    // only a stored group chat takes people, and only people not in it yet
    let add = |conversation: &str, members: &[&str]| Action::AddMembers {
        conversation: conversation.to_owned(),
        members: members.iter().map(|mri| Mri::new(*mri)).collect(),
        history: ChatHistory::Days(3),
    };
    let refused = a.handle.write(add(AB_CHAT, &[LAB_C])).await;
    assert!(matches!(refused, Err(EngineError::NotAGroupChat { .. })), "{refused:?}");
    for members in [&[][..], &[LAB_B], &[LAB_A], &[OUTSIDER, OUTSIDER]] {
        let refused = a.handle.write(add(&chat, members)).await;
        assert!(
            matches!(refused, Err(EngineError::NotAddable { ref conversation }) if *conversation == chat),
            "{members:?}: {refused:?}"
        );
    }

    let before = now_ms();
    let added = a.write(add(&chat, &[OUTSIDER])).await;
    assert_eq!(
        (added.kind, added.state),
        (OutboxKind::AddMembers, OutboxState::Sent),
        "{added:?}"
    );
    // the history's start is worked out once, when it is asked for: three days back
    let command: Value = serde_json::from_str(&added.payload).unwrap();
    let since = command["share_history_time"].as_i64().unwrap();
    let three_days = 3 * 86_400_000;
    assert!(
        (before - three_days..=now_ms() - three_days).contains(&since),
        "{since}"
    );
    // the chat's members as the service now has them, read after the write
    let mut members: Vec<String> = a
        .handle
        .members(chat.as_str())
        .await
        .unwrap()
        .into_iter()
        .map(|member| member.mri)
        .collect();
    members.sort();
    let mut expected = vec![LAB_A, LAB_B, LAB_C, OUTSIDER];
    expected.sort_unstable();
    assert_eq!(members, expected);
    a.shutdown();
}

#[tokio::test]
async fn a_named_group_chat_is_created_with_its_name_and_the_store_lists_it_so() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    a.ready().await;

    let created = a
        .write(Action::CreateChat {
            others: vec![Mri::new(LAB_B), Mri::new(LAB_C)],
            topic: Some(" Lab trio ".to_owned()),
        })
        .await;
    assert_eq!(created.state, OutboxState::Sent, "{created:?}");
    // the row keeps the name as it went out: trimmed
    assert!(
        created.payload.contains("\"topic\":\"Lab trio\""),
        "{}",
        created.payload
    );
    let chat = id_of(&created);
    let listed = a.handle.conversations(100).await.unwrap();
    let stored = listed.iter().find(|conversation| conversation.id == chat);
    assert_eq!(stored.and_then(|chat| chat.topic.as_deref()), Some("Lab trio"));
    a.shutdown();
}

#[tokio::test]
async fn a_dropped_socket_is_caught_up_after_the_reconnect_each_message_stored_once() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    let mut b = native(
        &lab,
        LAB_B,
        How {
            backoff: Duration::from_secs(2),
            ..How::default()
        },
    );
    a.ready().await;
    b.ready().await;

    lab.trouter.drop_sockets(LAB_B);
    let heard = b
        .event(|event| match event {
            Event::Disconnected { heard, .. } => Some(*heard),
            _ => None,
        })
        .await;
    // when the socket was last heard, handed on as the push channel told it
    assert!(heard.is_some_and(|heard| heard <= std::time::SystemTime::now()));
    let mut missed = Vec::new();
    for n in 0..3 {
        missed.push(id_of(&a.send(AB_CHAT, &format!("sent while b was away, {n}")).await));
    }

    // the reconnect is caught up (for the connect itself, before any delivery after it is taken); then come the
    // message-loss report's catch-up, the deliveries the service kept for b, and the replays of earlier ones
    b.event(|event| matches!(event, Event::Connected { .. }).then_some(()))
        .await;
    // what the catch-up stored is told as it is stored, marked as brought by it, before it ends
    let mut told_caught_up = 0;
    let mut caught_up = b
        .event(|event| match event {
            Event::CaughtUp { cause, report } => {
                assert_eq!(*cause, Cause::Connected, "{report:?}");
                Some(report.messages.inserted)
            }
            Event::Message {
                id, source, applied, ..
            } if missed.contains(id) => {
                assert_eq!(*source, Source::CatchUp, "a delivery came before the catch-up");
                assert_eq!(*applied, Applied::Inserted);
                told_caught_up += 1;
                None
            }
            _ => None,
        })
        .await;
    assert_eq!(told_caught_up, caught_up, "each message the catch-up stored is told");
    let mut delivered = 0;
    let mut seen: Vec<String> = Vec::new();
    while seen.len() < missed.len() {
        b.event(|event| match event {
            Event::CaughtUp { report, .. } => {
                assert!(report.failed.is_empty(), "{report:?}");
                caught_up += report.messages.inserted;
                Some(())
            }
            // one a later catch-up stored is counted by its report
            Event::Message {
                id, applied, source, ..
            } if missed.contains(id) && *source != Source::CatchUp => {
                if !seen.contains(id) {
                    seen.push(id.clone());
                }
                if *applied == Applied::Inserted {
                    delivered += 1;
                }
                Some(())
            }
            _ => None,
        })
        .await;
    }
    assert_eq!(caught_up + delivered, missed.len(), "every missed message stored once");
    for id in &missed {
        assert_eq!(b.copies(AB_CHAT, id).await, 1);
    }
    a.shutdown();
    b.shutdown();
}

#[tokio::test]
async fn a_throttled_write_goes_out_from_the_tick() {
    let lab = Lab::start().await;
    let mut a = native(
        &lab,
        LAB_A,
        How {
            throttle_first_write: true,
            ..How::default()
        },
    );
    let mut b = native(&lab, LAB_B, How::default());
    a.ready().await;
    b.ready().await;

    let row = a.write(send(AB_CHAT, "after a short wait")).await;
    assert_eq!(row.state, OutboxState::Pending, "{row:?}");
    assert!(row.not_before.is_some());
    let sent = a
        .event(|event| match event {
            Event::Outbox(entry) if entry.id == row.id && entry.state == OutboxState::Sent => Some(entry.clone()),
            _ => None,
        })
        .await;
    assert_eq!(sent.attempts, 2);
    assert_eq!(sent.client_message_id, row.client_message_id);
    let (_, applied, _) = b.message_event(AB_CHAT, &id_of(&sent)).await;
    assert_eq!(applied, Applied::Inserted);
    // the throttled attempt never reached the service; each attempt was a decision of its own
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);

    a.shutdown();
    b.shutdown();
}

#[tokio::test]
async fn without_a_push_channel_the_engine_syncs_on_demand_and_refuses_what_the_store_rules_out() {
    let lab = Lab::start().await;
    let b = native(
        &lab,
        LAB_B,
        How {
            push: false,
            ..How::default()
        },
    );
    assert!(b.handle.conversations(100).await.unwrap().is_empty());
    let report = b.handle.sync().await.unwrap();
    assert!(report.failed.is_empty() && report.conversations > 0, "{report:?}");
    let conversations = b.handle.conversations(100).await.unwrap();
    assert!(conversations.iter().any(|conversation| conversation.id == AB_CHAT));
    assert_eq!(b.stored(AB_CHAT, A_EDITED).await.sender.as_deref(), Some(LAB_A));

    let refusals = [
        Action::Edit {
            conversation: AB_CHAT.to_owned(),
            message_id: A_EDITED.to_owned(),
            text: MessageText::plain("not b's to edit").unwrap(),
        },
        Action::Delete {
            conversation: AB_CHAT.to_owned(),
            message_id: A_EDITED.to_owned(),
        },
        Action::React {
            conversation: AB_CHAT.to_owned(),
            message_id: A_DELETED.to_owned(),
            key: "like".to_owned(),
        },
        Action::Read {
            conversation: AB_CHAT.to_owned(),
            message_id: "1".to_owned(),
        },
    ];
    let mut refused = Vec::new();
    for action in refusals {
        refused.push(b.handle.write(action).await.unwrap_err());
    }
    assert!(
        matches!(refused[0], EngineError::NotOwn { action: "edited", .. }),
        "{:?}",
        refused[0]
    );
    assert!(
        matches!(refused[1], EngineError::NotOwn { action: "deleted", .. }),
        "{:?}",
        refused[1]
    );
    assert!(matches!(refused[2], EngineError::Deleted { .. }), "{:?}", refused[2]);
    assert!(
        matches!(refused[3], EngineError::UnknownMessage { .. }),
        "{:?}",
        refused[3]
    );
    // nothing of those reached the fence or the service

    let writes = lab
        .mock
        .received_requests()
        .await
        .iter()
        .filter(|request| request.method.as_str() != "GET")
        .count();
    assert_eq!(writes, 0);

    let read = b
        .write(Action::Read {
            conversation: AB_CHAT.to_owned(),
            message_id: A_EDITED.to_owned(),
        })
        .await;
    assert_eq!(read.state, OutboxState::Sent, "{read:?}");
    let id = id_of(&b.send(AB_CHAT, "sent without a push channel").await);
    assert!(
        b.stored(AB_CHAT, &id)
            .await
            .content
            .contains("sent without a push channel")
    );
    b.handle.load_older(AB_CHAT).await.unwrap();
    b.shutdown();
}

#[tokio::test]
async fn whether_the_whole_history_is_stored_tells_a_chat_never_synced_from_its_beginning() {
    let lab = Lab::start().await;
    let b = native(&lab, LAB_B, QUIET);
    // never synced: nothing older can be read, and that isn't the chat's beginning
    assert!(b.handle.load_older(AB_CHAT).await.unwrap().is_none());
    assert_eq!(b.handle.history_complete(AB_CHAT).await.unwrap(), None);

    b.handle.sync().await.unwrap();
    let mut pages = 0;
    while b.handle.load_older(AB_CHAT).await.unwrap().is_some() {
        pages += 1;
        assert!(pages < 20, "older pages never ran out");
    }
    assert_eq!(b.handle.history_complete(AB_CHAT).await.unwrap(), Some(true));
    b.shutdown();
}

#[tokio::test]
async fn store_queries_and_history_requests_are_served_while_catch_up_waits() {
    let lab = Lab::start().await;
    let b = native(&lab, LAB_B, QUIET);
    b.handle.sync().await.unwrap();
    let stored = b.handle.messages(AB_CHAT, None, 100).await.unwrap().messages;
    assert!(!stored.is_empty());

    // the service holds the catch-up's first read, as a long catch-up holds the engine
    b.reads_held.send_replace(true);
    let sync = b.handle.sync();
    let older = b.handle.load_older(AB_CHAT);
    // a view opening a chat meanwhile: its reads of the store are answered while the catch-up waits
    let read = tokio::time::timeout(Duration::from_secs(5), b.handle.messages(AB_CHAT, None, 100)).await;
    let read = read.expect("the store wasn't read while the catch-up waited").unwrap();
    assert_eq!(read.messages, stored);
    assert!(b.handle.message(AB_CHAT, A_EDITED).await.unwrap().is_some());
    // and so are the store reads that start a read of their own: a sender's name, the chat list's extras, the teams
    let person = tokio::time::timeout(Duration::from_secs(5), b.handle.person(LAB_A)).await;
    let person = person
        .expect("a person wasn't answered while the catch-up waited")
        .unwrap();
    assert_eq!(person.mri, LAB_A);
    assert!(person.name.is_some(), "{person:?}");
    let listing = tokio::time::timeout(Duration::from_secs(5), b.handle.chat_listing()).await;
    listing
        .expect("the chat listing wasn't answered while the catch-up waited")
        .unwrap();
    let teams = tokio::time::timeout(Duration::from_secs(5), b.handle.teams()).await;
    teams
        .expect("the teams weren't answered while the catch-up waited")
        .unwrap();
    // a receipt for a message the store doesn't hold is refused at once, while the catch-up still waits
    let unknown = b.handle.write(Action::Read {
        conversation: AB_CHAT.to_owned(),
        message_id: "1000000000000".to_owned(),
    });
    let unknown = tokio::time::timeout(Duration::from_secs(5), unknown).await;
    let unknown = unknown.expect("a receipt wasn't answered while the catch-up waited");
    assert!(
        matches!(unknown, Err(EngineError::UnknownMessage { .. })),
        "{unknown:?}"
    );
    let older = tokio::time::timeout(Duration::from_secs(5), older)
        .await
        .expect("older history waited for catch-up")
        .unwrap();
    assert!(older.is_none(), "the initial sync already stored the whole history");

    b.reads_held.send_replace(false);
    let report = sync.await.unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    b.shutdown();
}

#[tokio::test]
async fn a_write_whose_check_cant_sync_is_answered_as_the_row_that_waits() {
    let lab = Lab::start().await;
    let mut a = native(
        &lab,
        LAB_A,
        How {
            push: false,
            ..How::default()
        },
    );
    a.handle.sync().await.unwrap();

    // the post's answer is a 500, so whether it arrived is unknown, and the chat can't be read to look for it
    a.next_write.store(500, Ordering::SeqCst);
    a.reads_fail.store(true, Ordering::SeqCst);
    let row = a.handle.write(send(AB_CHAT, "did it arrive?")).await;

    // the reply is the row as it waits for its next check, not an error a caller would send the message again on
    let row = row.unwrap();
    assert_eq!(row.state, OutboxState::Pending, "{row:?}");
    assert_eq!((row.attempts, row.failed_checks), (1, 1), "{row:?}");
    assert!(row.not_before.is_some(), "{row:?}");
    let told = a
        .event(|event| match event {
            Event::Outbox(entry) => Some(entry.clone()),
            _ => None,
        })
        .await;
    assert_eq!(told, row);
    assert_eq!(lab.posts_to(AB_CHAT).await, 0);
    a.shutdown();
}

/// A post whose answer was lost has `failed_before` checks that couldn't sync; then the service's copy of it turns
/// into an entry no read can take, and the next check, from the tick, meets it first: the row records it and the check
/// fails. The last [`Event::Outbox`] of the row is the row as the store now has it, whether the check left it waiting
/// or gave it up (`state`), never the copy the read kept before the check changed the row.
async fn an_unreadable_entry_a_check_meets_first(failed_before: u32, state: OutboxState) {
    let lab = Lab::start().await;
    let mut a = native(
        &lab,
        LAB_A,
        How {
            push: false,
            ..How::default()
        },
    );
    a.handle.sync().await.unwrap();
    let row = a
        .send_unanswered(AB_CHAT, "did it arrive? its copy can't be read")
        .await;

    // more checks that can't sync, each from the tick once the row's backoff is over
    a.reads_fail.store(true, Ordering::SeqCst);
    for failed in 2..=failed_before {
        a.checked_from_the_tick(row.id, failed).await;
    }
    a.reads_fail.store(false, Ordering::SeqCst);
    let unreadable = lab.mock.change_message(AB_CHAT, &row.client_message_id, |message| {
        message["properties"] = json!("unreadable");
    });
    assert!(unreadable);

    let told = a.checked_from_the_tick(row.id, failed_before + 1).await;
    let stored = told_last_as_it_stands(&mut a, told).await;
    assert_eq!(stored.state, state, "{stored:?}");
    assert!(stored.may_have_arrived(), "{stored:?}");
    assert_eq!(lab.posts_to(AB_CHAT).await, 1, "never sent again on its own");
    a.shutdown();
}

/// Checks that `told`, the [`Event::Outbox`] of a row just taken, is the last one told of it and the row as the store
/// has it; the row.
async fn told_last_as_it_stands(a: &mut Native, told: OutboxEntry) -> OutboxEntry {
    let later = a.told_rows(told.id).await;
    let last = later.last().unwrap_or(&told);
    let stored = a.handle.outbox_entry(told.id).await.unwrap().unwrap();
    assert_eq!(
        last, &stored,
        "the last event is the row as it stands; told after it: {later:?}"
    );
    assert!(later.is_empty(), "told once: {later:?}");
    stored
}

#[tokio::test]
async fn an_unreadable_entry_a_check_meets_first_leaves_the_row_waiting_and_told_as_it_stands() {
    an_unreadable_entry_a_check_meets_first(1, OutboxState::Pending).await;
}

#[tokio::test]
async fn an_unreadable_entry_a_check_meets_first_at_its_last_check_fails_the_row_told_as_it_stands() {
    an_unreadable_entry_a_check_meets_first(MAX_FAILED_CHECKS - 1, OutboxState::Failed).await;
}

/// A post whose answer was lost, and whose check couldn't sync; then the service holds two entries of it, one no read
/// can take and the readable one, and the next check, from the tick, meets both on one page: the unreadable one first
/// (the row records it, still pending), then the readable one (the row is sent). The last [`Event::Outbox`] of the row
/// is the row as sent, never the pending copy the same read kept before it.
#[tokio::test]
async fn a_check_meeting_an_unreadable_and_a_readable_entry_of_its_post_tells_the_row_last_as_sent() {
    let lab = Lab::start().await;
    let mut a = native(
        &lab,
        LAB_A,
        How {
            push: false,
            ..How::default()
        },
    );
    a.handle.sync().await.unwrap();
    let row = a
        .send_unanswered(AB_CHAT, "did it arrive? one of its entries can't be read")
        .await;
    let copied = lab.mock.copy_message(AB_CHAT, &row.client_message_id, |message| {
        message["id"] = json!("9999999999999");
        message["properties"] = json!("unreadable");
    });
    assert!(copied);

    // the check, from the tick once the row's backoff is over
    a.ahead.fetch_add(3_600_000, Ordering::SeqCst);
    let told = a
        .event(|event| match event {
            Event::Outbox(entry) if entry.id == row.id => Some(entry.clone()),
            _ => None,
        })
        .await;
    let stored = told_last_as_it_stands(&mut a, told).await;
    assert_eq!(stored.state, OutboxState::Sent, "{stored:?}");
    assert!(stored.message_id.is_some(), "{stored:?}");
    assert_eq!(lab.posts_to(AB_CHAT).await, 1, "never sent again");
    a.shutdown();
}

/// The service's echo of `from`'s message to `to`'s endpoints, which the mock queues for the chat's other members
/// alone: the one queued for `other`, queued for `to` too.
fn echo(lab: &Lab, from: &str, other: &str, to: &str) {
    echo_changed(lab, from, other, to, |_| {});
}

/// [`echo`], its message changed by `change` first.
fn echo_changed(lab: &Lab, from: &str, other: &str, to: &str, change: impl FnOnce(&mut Value)) {
    let mut event = lab
        .mock
        .take_events(other)
        .into_iter()
        .find(|event| event.kind == EventKind::Message && event.from == from)
        .unwrap();
    change(&mut event.resource);
    lab.mock.queue_event(to, event);
}

#[tokio::test]
async fn a_post_whose_answer_was_lost_is_settled_by_a_later_catch_up_and_told_once() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    a.ready().await;
    let row = a.send_unanswered(AB_CHAT, "did it arrive? ask the catch-up").await;
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);

    // the catch-up finds the message: its row is sent, and told so once
    let report = a.handle.sync().await.unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    let told = a.told_rows(row.id).await;
    assert_eq!(told.len(), 1, "{told:?}\n{}", a.seen);
    let sent = &told[0];
    assert_eq!(sent.state, OutboxState::Sent, "{sent:?}");
    assert_eq!(sent.client_message_id, row.client_message_id);
    let id = id_of(sent);
    assert_eq!(a.handle.outbox_entry(row.id).await.unwrap().as_ref(), Some(sent));
    assert_eq!(
        a.stored(AB_CHAT, &id).await.client_message_id.as_deref(),
        Some(row.client_message_id.as_str())
    );

    // its echo over the push channel changes nothing, and tells nothing more; nor does another catch-up
    echo(&lab, LAB_A, LAB_B, LAB_A);
    let (change, applied, _) = a.message_event(AB_CHAT, &id).await;
    assert_eq!((change, applied), (MessageChange::New, Applied::Ignored));
    a.handle.sync().await.unwrap();
    let told = a.told_rows(row.id).await;
    assert!(told.is_empty(), "{told:?}");
    assert_eq!(a.copies(AB_CHAT, &id).await, 1);
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);

    a.shutdown();
}

#[tokio::test]
async fn a_post_whose_answer_was_lost_is_settled_by_its_echo_over_the_push_channel_and_told_once() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    a.ready().await;
    let row = a.send_unanswered(AB_CHAT, "did it arrive? ask the push channel").await;

    // the echo settles the row, told before the message itself
    echo(&lab, LAB_A, LAB_B, LAB_A);
    let sent = a
        .event(|event| match event {
            Event::Outbox(entry) if entry.id == row.id => Some(entry.clone()),
            Event::Message { .. } => panic!("the message was told before its row"),
            _ => None,
        })
        .await;
    assert_eq!(sent.state, OutboxState::Sent, "{sent:?}");
    let id = id_of(&sent);
    let (change, applied, _) = a.message_event(AB_CHAT, &id).await;
    assert_eq!((change, applied), (MessageChange::New, Applied::Inserted));

    // a catch-up after it finds the row sent already, and tells nothing more
    a.handle.sync().await.unwrap();
    let told = a.told_rows(row.id).await;
    assert!(told.is_empty(), "{told:?}");
    assert_eq!(a.handle.outbox_entry(row.id).await.unwrap(), Some(sent));
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);
    a.shutdown();
}

#[tokio::test]
async fn a_post_whose_answer_was_lost_is_settled_by_an_echo_it_cant_store_and_told_once() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    a.ready().await;
    let row = a
        .send_unanswered(AB_CHAT, "did it arrive? its echo has no version")
        .await;

    // an echo whose version can't be read isn't stored, but it is a's own: its row is sent, and told so
    echo_changed(&lab, LAB_A, LAB_B, LAB_A, |message| {
        message.as_object_mut().unwrap().remove("version");
    });
    let sent = a
        .event(|event| match event {
            Event::Outbox(entry) if entry.id == row.id => Some(entry.clone()),
            _ => None,
        })
        .await;
    assert_eq!(sent.state, OutboxState::Sent, "{sent:?}");
    assert!(a.handle.message(AB_CHAT, id_of(&sent)).await.unwrap().is_none());
    let told = a.told_rows(row.id).await;
    assert!(told.is_empty(), "{told:?}");
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);
    a.shutdown();
}

#[tokio::test]
async fn a_post_whose_answer_was_lost_is_settled_by_the_sync_after_another_write_and_told_once() {
    let lab = Lab::start().await;
    let mut a = native(
        &lab,
        LAB_A,
        How {
            push: false,
            ..How::default()
        },
    );
    a.handle.sync().await.unwrap();
    let row = a.send_unanswered(AB_CHAT, "did it arrive? ask the next write").await;

    // the next post goes through, and the sync after it brings the first one back too
    let next = a.send(AB_CHAT, "the next one").await;
    let told = a.told_rows(row.id).await;
    assert_eq!(told.len(), 1, "{told:?}\n{}", a.seen);
    assert_eq!(told[0].state, OutboxState::Sent, "{:?}", told[0]);
    assert_ne!(told[0].message_id, next.message_id);
    assert_eq!(a.copies(AB_CHAT, &id_of(&told[0])).await, 1);
    assert_eq!(lab.posts_to(AB_CHAT).await, 2);
    a.shutdown();
}

#[tokio::test]
async fn a_delivery_whose_conversation_cant_be_read_is_caught_up_from_the_tick() {
    let lab = Lab::start().await;
    let how = How::default();
    let mut a = native(&lab, LAB_A, how);
    let mut b = native(&lab, LAB_B, how);
    a.ready().await;
    b.ready().await;

    // b can't read the new chat its first message names, so the delivery doesn't reach b's store
    b.reads_fail.store(true, Ordering::SeqCst);
    b.failures_expected = true;
    let chat = id_of(
        &a.write(Action::CreateChat {
            others: vec![Mri::new(LAB_B), Mri::new(LAB_C)],
            topic: None,
        })
        .await,
    );
    let first = id_of(&a.send(&chat, "the first message, missed by b").await);
    b.event(|event| match event {
        Event::Failed { during, .. } => (*during == "adding a delivery's conversation").then_some(()),
        Event::Message { id, .. } if *id == first => panic!("the message reached b's store"),
        _ => None,
    })
    .await;
    assert!(b.handle.message(chat.as_str(), first.as_str()).await.unwrap().is_none());

    // once b can read again, the tick's catch-up brings the chat and its message while the socket stays up
    b.reads_fail.store(false, Ordering::SeqCst);
    b.event(|event| match event {
        Event::CaughtUp {
            cause: Cause::Retry,
            report,
        } => report.failed.is_empty().then_some(()),
        Event::Disconnected { .. } => panic!("b's socket dropped"),
        _ => None,
    })
    .await;
    assert_eq!(b.copies(&chat, &first).await, 1);
    a.shutdown();
    b.shutdown();
}

// --- the request side, the account's lock, the queries the UI reads ---

/// A native client without a push channel.
const QUIET: How = How {
    push: false,
    throttle_first_write: false,
    backoff: Duration::from_millis(50),
};

#[tokio::test]
async fn the_request_side_is_shared_from_any_thread_and_answers_stopped_once_the_engine_is_shut_down() {
    let lab = Lab::start().await;
    let mut b = native(&lab, LAB_B, QUIET);
    let requests = b.handle.requests();

    // from a thread that runs no executor, blocking; from a task, awaiting
    let from_thread = {
        let requests = requests.clone();
        std::thread::spawn(move || requests.sync().blocking_wait())
            .join()
            .unwrap()
            .unwrap()
    };
    assert!(
        from_thread.failed.is_empty() && from_thread.conversations > 0,
        "{from_thread:?}"
    );
    let from_task = tokio::spawn({
        let requests = requests.clone();
        async move { requests.conversations(100).await }
    })
    .await
    .unwrap()
    .unwrap();
    assert!(from_task.iter().any(|conversation| conversation.id == AB_CHAT));

    // a write made through a shared handle is told on the owner's events
    let read = Action::Read {
        conversation: AB_CHAT.to_owned(),
        message_id: A_EDITED.to_owned(),
    };
    let row = requests.write(read).await.unwrap();
    let told = b
        .event(|event| match event {
            Event::Outbox(entry) if entry.id == row.id => Some(entry.clone()),
            _ => None,
        })
        .await;
    assert_eq!(told, row);

    b.shutdown();
    assert!(matches!(requests.conversations(1).await, Err(EngineError::Stopped)));
}

/// The events about outbox row `id` among `events`: `queued` for [`Event::Queued`], `outbox` for [`Event::Outbox`],
/// each with the row's state.
fn row_events(events: &[Event], id: i64) -> Vec<(&'static str, OutboxState)> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Queued(row) if row.id == id => Some(("queued", row.state)),
            Event::Outbox(row) if row.id == id => Some(("outbox", row.state)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_write_s_row_is_told_queued_before_its_first_attempt_and_so_is_a_resend() {
    let lab = Lab::start().await;
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let mut first = try_native_in(&lab, LAB_A, QUIET, Arc::clone(&dir)).unwrap_or_else(|error| panic!("{error}"));
    first.handle.sync().await.unwrap();
    first.drain().await;

    // a view shows the row at once, as it waits for its first attempt, then as the attempt left it
    let row = first.send(AB_CHAT, "shown at once").await;
    let events = first.drain().await;
    assert_eq!(
        row_events(&events, row.id),
        [("queued", OutboxState::Pending), ("outbox", OutboxState::Sent)]
    );
    let queued = events
        .iter()
        .find_map(|event| match event {
            Event::Queued(queued) if queued.id == row.id => Some(queued.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        (queued.attempts, queued.client_message_id.as_str()),
        (0, row.client_message_id.as_str())
    );

    // a write the store rules out makes no row, so none is told
    let refused = first
        .handle
        .write(Action::Delete {
            conversation: AB_CHAT.to_owned(),
            message_id: A_DELETED.to_owned(),
        })
        .await;
    assert!(matches!(refused, Err(EngineError::Deleted { .. })), "{refused:?}");
    assert!(
        !first
            .drain()
            .await
            .iter()
            .any(|event| matches!(event, Event::Queued(_))),
        "no row, nothing queued"
    );

    // a row an earlier run left is told queued again as its explicit resend adopts it
    let left = first.send_unanswered(AB_CHAT, "left behind").await;
    first.shutdown();
    let mut second = native_once_free(&lab, LAB_A, &dir).await;
    second.drain().await;
    let resent = second.handle.resend(left.id).await.unwrap();
    let events = second.drain().await;
    assert_eq!(
        row_events(&events, left.id).first(),
        Some(&("queued", OutboxState::Pending)),
        "{events:?}"
    );
    assert_eq!(resent.state, OutboxState::Sent);
    second.shutdown();
}

/// An engine for `me` on the store in `dir`, once the one before it has let go of the account's lock (its thread ends
/// after its handle is dropped, which isn't waited for).
async fn native_once_free(lab: &Lab, me: &'static str, dir: &Arc<tempfile::TempDir>) -> Native {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        match try_native_in(lab, me, QUIET, Arc::clone(dir)) {
            Ok(native) => return native,
            Err(EngineError::AlreadyRunning { .. }) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => panic!("the engine didn't start: {error}"),
        }
    }
}

#[tokio::test]
async fn dropping_the_owner_stops_the_engine_whatever_request_handles_are_left() {
    let lab = Lab::start().await;
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let b = try_native_in(&lab, LAB_B, QUIET, Arc::clone(&dir)).unwrap_or_else(|error| panic!("{error}"));
    let requests = b.handle.requests();
    drop(b);
    assert!(matches!(requests.conversations(1).await, Err(EngineError::Stopped)));
    // and the account's lock went with its thread
    native_once_free(&lab, LAB_B, &dir).await.shutdown();
}

#[tokio::test]
async fn a_second_engine_on_the_account_is_refused_at_once_and_starts_once_the_first_is_shut_down() {
    let lab = Lab::start().await;
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let mut first =
        try_native_in(&lab, LAB_A, How::default(), Arc::clone(&dir)).unwrap_or_else(|error| panic!("{error}"));
    first.ready().await;

    let started = std::time::Instant::now();
    let Err(error) = try_native_in(&lab, LAB_A, How::default(), Arc::clone(&dir)) else {
        panic!("a second engine started on the account");
    };
    assert!(started.elapsed() < Duration::from_secs(2), "a refusal never waits");
    assert!(
        matches!(&error, EngineError::AlreadyRunning { lock } if lock.ends_with("lab.sqlite-runs/engine.lock")),
        "{error}"
    );
    assert!(
        error.to_string().contains("another engine runs on this account"),
        "{error}"
    );
    // the refused one never started its push channel; the first one runs on
    let sockets = lab
        .trouter
        .sockets()
        .into_iter()
        .filter(|socket| socket.account.as_deref() == Some(LAB_A))
        .count();
    assert_eq!(sockets, 1);
    assert!(!first.handle.conversations(1).await.unwrap().is_empty());

    first.shutdown();
    let second = try_native_in(&lab, LAB_A, QUIET, Arc::clone(&dir)).unwrap_or_else(|error| panic!("{error}"));
    assert!(
        !second.handle.conversations(1).await.unwrap().is_empty(),
        "the same store"
    );
    second.shutdown();
}

#[tokio::test]
async fn the_unread_count_and_the_read_horizon_follow_the_store() {
    let lab = Lab::start().await;
    let a = native(&lab, LAB_A, QUIET);
    let b = native(&lab, LAB_B, QUIET);
    b.handle.sync().await.unwrap();
    let before = b.handle.unread_count(AB_CHAT).await.unwrap();

    let id = id_of(&a.send(AB_CHAT, "count me").await);
    b.handle.sync().await.unwrap();
    assert_eq!(b.handle.unread_count(AB_CHAT).await.unwrap(), before + 1);

    let read = b
        .write(Action::Read {
            conversation: AB_CHAT.to_owned(),
            message_id: id.clone(),
        })
        .await;
    assert_eq!(read.state, OutboxState::Sent, "{read:?}");
    b.handle.sync().await.unwrap();
    let horizon = b.handle.read_horizon(AB_CHAT).await.unwrap().unwrap();
    assert_eq!(horizon.user, LAB_B);
    assert_eq!(horizon.last_read_arrival, b.stored(AB_CHAT, &id).await.arrival_time);
    // what is unread now is what arrived after it (the fixture's messages carry later arrival times than the mock's
    // new ones), from a, standing
    let page = b.handle.messages(AB_CHAT, None, 100).await.unwrap();
    let after = page
        .messages
        .iter()
        .filter(|message| {
            message.arrival_time > horizon.last_read_arrival
                && message.sender.as_deref() == Some(LAB_A)
                && message.delete_time.is_none()
        })
        .count();
    assert_eq!(
        b.handle.unread_count(AB_CHAT).await.unwrap(),
        u64::try_from(after).unwrap()
    );
    assert!(b.handle.read_horizon("19:unknown@thread.v2").await.unwrap().is_none());
    a.shutdown();
    b.shutdown();
}

#[tokio::test]
async fn the_requests_name_the_account_and_a_person_s_name_comes_from_the_store() {
    let lab = Lab::start().await;
    let b = native(&lab, LAB_B, QUIET);
    assert_eq!(b.handle.me(), LAB_B);
    assert_eq!(b.handle.requests().me(), LAB_B, "every clone names the account");
    assert_eq!(b.handle.display_name(LAB_A).await.unwrap(), None, "nothing synced yet");

    b.handle.sync().await.unwrap();
    // the a–b chat's messages are a's, each carrying a's name
    assert_eq!(b.handle.display_name(LAB_A).await.unwrap().as_deref(), Some("Lab A"));
    assert_eq!(b.handle.display_name(LAB_C).await.unwrap(), None);
    b.shutdown();
}

#[tokio::test]
async fn a_row_an_earlier_run_left_goes_out_only_by_an_explicit_resend_which_looks_for_it_first() {
    let lab = Lab::start().await;
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let mut first = try_native_in(&lab, LAB_A, QUIET, Arc::clone(&dir)).unwrap_or_else(|error| panic!("{error}"));
    let row = first.send_unanswered(AB_CHAT, "its answer was lost").await;
    assert_eq!(
        first.handle.outbox_unsettled(AB_CHAT).await.unwrap(),
        std::slice::from_ref(&row)
    );
    assert!(
        first.handle.left_by_earlier_runs().await.unwrap().is_empty(),
        "its own run's"
    );
    first.shutdown();
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);

    let mut second = native_once_free(&lab, LAB_A, &dir).await;
    let left = second.handle.left_by_earlier_runs().await.unwrap();
    assert_eq!(left.iter().map(|left| left.id).collect::<Vec<_>>(), [row.id]);
    assert_eq!(second.handle.outbox_unsettled(AB_CHAT).await.unwrap().len(), 1);
    // it doesn't go out on its own, however many ticks pass
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);

    // the explicit resend: the row's earlier attempt arrived, which its check finds, so nothing goes out again
    let resent = second.handle.resend(row.id).await.unwrap();
    assert_eq!(resent.state, OutboxState::Sent, "{resent:?}");
    assert_eq!(resent.client_message_id, row.client_message_id);
    assert_ne!(resent.run_id, row.run_id, "adopted into this run");
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);
    assert!(second.handle.outbox_unsettled(AB_CHAT).await.unwrap().is_empty());
    assert!(second.handle.left_by_earlier_runs().await.unwrap().is_empty());
    let told = second
        .event(|event| match event {
            Event::Outbox(entry) if entry.id == row.id => Some(entry.clone()),
            _ => None,
        })
        .await;
    assert_eq!(told.state, OutboxState::Sent);
    // a sent row isn't sent again
    let refused = second.handle.resend(row.id).await;
    assert!(
        matches!(refused, Err(EngineError::Store(StoreError::OutboxNotSendable { .. }))),
        "{refused:?}"
    );

    second.shutdown();
}

#[tokio::test]
async fn a_catch_up_tells_each_stored_message_once_and_publishes_conversation_changes() {
    let lab = Lab::start().await;
    let a = native(&lab, LAB_A, QUIET);
    let mut b = native(&lab, LAB_B, QUIET);

    let report = b.handle.sync().await.unwrap();
    let told = b.drain().await;
    let messages: Vec<_> = told
        .iter()
        .filter_map(|event| match event {
            Event::Message {
                change,
                applied,
                message,
                source,
                ..
            } => Some((*change, *applied, message.is_some(), *source)),
            _ => None,
        })
        .collect();
    assert_eq!(messages.len(), report.messages.changed(), "{messages:?}");
    assert!(
        messages
            .iter()
            .all(|told| *told == (MessageChange::New, Applied::Inserted, true, Source::CatchUp)),
        "{messages:?}"
    );
    let changed = conversations_changed(&told);
    assert!(changed.contains(&AB_CHAT.to_owned()), "{changed:?}");

    // nothing changed: nothing told
    b.handle.sync().await.unwrap();
    let told = b.drain().await;
    assert!(
        told.iter()
            .all(|event| !matches!(event, Event::Message { .. } | Event::ConversationChanged { .. })),
        "{told:?}"
    );

    // a's new message: told once, as new, with the conversation it changed
    let id = id_of(&a.send(AB_CHAT, "brought by a catch-up").await);
    b.handle.sync().await.unwrap();
    let told = b.drain().await;
    let messages: Vec<_> = told
        .iter()
        .filter_map(|event| match event {
            Event::Message {
                id,
                change,
                applied,
                message,
                source,
                ..
            } => Some((
                id.clone(),
                *change,
                *applied,
                *source,
                message.as_ref()?.content.clone(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(messages.len(), 1, "{messages:?}");
    let (got, change, applied, source, content) = &messages[0];
    assert_eq!(
        (got, *change, *applied, *source),
        (&id, MessageChange::New, Applied::Inserted, Source::CatchUp)
    );
    assert!(content.contains("brought by a catch-up"), "{content}");
    assert_eq!(conversations_changed(&told), [AB_CHAT]);
    a.shutdown();
    b.shutdown();
}

/// The conversations `events` told changed, in order.
fn conversations_changed(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::ConversationChanged { conversation } => Some(conversation.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_message_a_delivery_brought_is_not_told_again_by_a_catch_up() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    let mut b = native(&lab, LAB_B, How::default());
    a.ready().await;
    b.ready().await;
    let id = id_of(&a.send(AB_CHAT, "delivered, then caught up").await);
    let (applied, source) = b
        .event(|event| match event {
            Event::Message {
                id: got,
                applied,
                source,
                ..
            } if *got == id => Some((*applied, *source)),
            _ => None,
        })
        .await;
    assert_eq!((applied, source), (Applied::Inserted, Source::Delivery));

    b.handle.sync().await.unwrap();
    let told = b.drain().await;
    assert!(
        !told
            .iter()
            .any(|event| matches!(event, Event::Message { id: got, .. } if *got == id)),
        "{told:?}"
    );
    a.shutdown();
    b.shutdown();
}

// --- transient writes ---

fn typing(conversation: &str) -> Transient {
    Transient::Typing {
        conversation: conversation.to_owned(),
    }
}

#[tokio::test]
async fn typing_reaches_b_under_a_fresh_decision_each_time_and_nothing_of_it_is_stored_or_survives_a_restart() {
    let lab = Lab::start().await;
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let mut a = try_native_in(&lab, LAB_A, How::default(), Arc::clone(&dir)).unwrap_or_else(|error| panic!("{error}"));
    let mut b = native(&lab, LAB_B, How::default());
    a.ready().await;
    b.ready().await;

    for _ in 0..2 {
        assert_eq!(a.handle.transient(typing(AB_CHAT)).await.unwrap(), TransientSent::Sent);
        let from = b
            .event(|event| match event {
                Event::Typing {
                    conversation,
                    from,
                    typing: true,
                } if conversation == AB_CHAT => Some(from.clone()),
                _ => None,
            })
            .await;
        assert_eq!(from.as_deref(), Some(LAB_A));
    }

    assert_eq!(lab.posts_to(AB_CHAT).await, 2);

    // no row: none is the conversation's, none was ever made, and none is left for a later run
    assert!(a.handle.outbox_unsettled(AB_CHAT).await.unwrap().is_empty());
    assert!(a.handle.outbox_entry(1).await.unwrap().is_none());
    a.shutdown();
    let again = native_once_free(&lab, LAB_A, &dir).await;
    assert!(again.handle.left_by_earlier_runs().await.unwrap().is_empty());
    assert!(again.handle.outbox_entry(1).await.unwrap().is_none());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lab.posts_to(AB_CHAT).await, 2, "nothing sent again after the restart");
    again.shutdown();
    b.shutdown();
}

#[tokio::test]
async fn typing_whose_outcome_is_unknown_is_dropped_and_never_sent_again() {
    let lab = Lab::start().await;
    let a = native(&lab, LAB_A, QUIET);
    a.handle.sync().await.unwrap();

    a.lose_answer.store(true, Ordering::SeqCst);
    let sent = a.handle.transient(typing(AB_CHAT)).await.unwrap();
    assert!(
        matches!(&sent, TransientSent::Dropped { reason } if reason.contains("whether it arrived is unknown")),
        "{sent:?}"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lab.posts_to(AB_CHAT).await, 1, "no check, no second attempt");

    // a 401 gets one more attempt at once, as a row's does; any other refusal drops it
    a.next_write.store(401, Ordering::SeqCst);
    assert_eq!(a.handle.transient(typing(AB_CHAT)).await.unwrap(), TransientSent::Sent);
    assert_eq!(lab.posts_to(AB_CHAT).await, 2);
    a.next_write.store(400, Ordering::SeqCst);
    let sent = a.handle.transient(typing(AB_CHAT)).await.unwrap();
    assert_eq!(
        sent,
        TransientSent::Dropped {
            reason: "HTTP 400".to_owned()
        }
    );

    assert!(a.handle.outbox_entry(1).await.unwrap().is_none());
    a.shutdown();
}

#[tokio::test]
async fn throttled_typing_waits_and_only_the_latest_goes_out_from_the_tick() {
    let lab = Lab::start().await;
    let mut a = native(
        &lab,
        LAB_A,
        How {
            throttle_first_write: true,
            ..QUIET
        },
    );
    a.handle.sync().await.unwrap();

    let first = a.handle.transient(typing(AB_CHAT)).await.unwrap();
    let TransientSent::Waiting { not_before } = first else {
        panic!("{first:?}");
    };
    // asked again while it waits: it takes the first one's place, and waits as long
    let second = a.handle.transient(typing(AB_CHAT)).await.unwrap();
    assert_eq!(second, TransientSent::Waiting { not_before });
    let (transient, sent) = a
        .event(|event| match event {
            Event::Transient(transient, sent) => Some((transient.clone(), sent.clone())),
            _ => None,
        })
        .await;
    assert_eq!((transient, sent), (typing(AB_CHAT), TransientSent::Sent));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let told = a.drain().await;
    assert!(
        !told.iter().any(|event| matches!(event, Event::Transient(..))),
        "one went out from the tick: {told:?}"
    );
    // the throttled attempt never reached the service; the replaced one never went out
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);

    a.shutdown();
}

#[tokio::test]
async fn a_message_post_drops_the_typing_waiting_in_its_conversation() {
    let lab = Lab::start().await;
    let mut a = native(
        &lab,
        LAB_A,
        How {
            throttle_first_write: true,
            ..QUIET
        },
    );
    a.handle.sync().await.unwrap();

    let waiting = a.handle.transient(typing(AB_CHAT)).await.unwrap();
    assert!(matches!(waiting, TransientSent::Waiting { .. }), "{waiting:?}");
    let row = a.handle.write(send(AB_CHAT, "typed")).await.unwrap();
    assert_eq!(row.state, OutboxState::Sent);
    let (transient, sent) = a
        .event(|event| match event {
            Event::Transient(transient, sent) => Some((transient.clone(), sent.clone())),
            _ => None,
        })
        .await;
    assert_eq!(transient, typing(AB_CHAT));
    assert!(matches!(sent, TransientSent::Dropped { .. }), "{sent:?}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let told = a.drain().await;
    assert!(
        !told.iter().any(|event| matches!(event, Event::Transient(..))),
        "no typing after the message: {told:?}"
    );
    // the message alone reached the service: the throttled typing never did, and nothing followed
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);

    a.shutdown();
}

#[tokio::test]
async fn typing_goes_out_without_a_member_read_and_is_refused_where_the_store_knows_no_chat() {
    // typing reaches nobody: the fence permits it on a chat without reading its members, logged
    let lab = Lab::start().await;
    let a = native(&lab, LAB_A, QUIET);
    a.handle.sync().await.unwrap();
    let sent = a.handle.transient(typing(ABC_CHAT)).await.unwrap();
    assert!(matches!(sent, TransientSent::Sent), "{sent:?}");

    assert_eq!(lab.posts_to(ABC_CHAT).await, 1);
    assert_eq!(lab.member_reads_of(ABC_CHAT).await, 0, "no member read");

    let refused = a.handle.transient(typing("19:unknown@thread.v2")).await;
    assert!(
        matches!(refused, Err(EngineError::Store(StoreError::UnknownConversation(_)))),
        "{refused:?}"
    );

    a.shutdown();
}

// --- local search ---

/// The hits' conversations and message ids, in order.
fn hit_ids(hits: &[SearchHit]) -> Vec<(String, String)> {
    hits.iter()
        .map(|hit| (hit.conversation_id.clone(), hit.message_id.clone()))
        .collect()
}

#[tokio::test]
async fn a_message_is_found_once_stored_and_its_edit_and_delete_change_what_finds_it() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    let mut b = native(&lab, LAB_B, How::default());
    a.ready().await;
    b.ready().await;
    let id = id_of(&a.send(AB_CHAT, "the quarterly zeppelin report").await);
    b.message_event(AB_CHAT, &id).await;
    let expected = vec![(AB_CHAT.to_owned(), id.clone())];
    assert_eq!(
        hit_ids(&b.handle.search("zeppelin", 10).await.unwrap()),
        expected,
        "b's delivery"
    );
    assert_eq!(
        hit_ids(&a.handle.search("QUARTERLY zep", 10).await.unwrap()),
        expected,
        "a's own, stored by the sync after its write"
    );
    assert_eq!(
        hit_ids(
            &b.handle
                .search_in("zeppelin", vec![AB_CHAT.to_owned()], 10)
                .await
                .unwrap()
        ),
        expected,
        "a search in the chat"
    );
    assert!(
        b.handle
            .search_in("zeppelin", vec![ABC_CHAT.to_owned()], 10)
            .await
            .unwrap()
            .is_empty(),
        "a search in other chats only"
    );
    let hit = b.handle.search("zeppelin", 10).await.unwrap().remove(0);
    assert_eq!(hit.sender.as_deref(), Some(LAB_A));
    assert!(hit.snippet.contains("\u{e000}zeppelin\u{e001}"), "{hit:?}");

    a.write(Action::Edit {
        conversation: AB_CHAT.to_owned(),
        message_id: id.clone(),
        text: MessageText::plain("the yearly airship report").unwrap(),
    })
    .await;
    let (change, _, _) = b.message_event(AB_CHAT, &id).await;
    assert_eq!(change, MessageChange::Edit);
    assert!(
        b.handle.search("zeppelin", 10).await.unwrap().is_empty(),
        "the edit replaced the text"
    );
    assert_eq!(hit_ids(&b.handle.search("airship", 10).await.unwrap()), expected);

    a.write(Action::Delete {
        conversation: AB_CHAT.to_owned(),
        message_id: id.clone(),
    })
    .await;
    let (change, _, _) = b.message_event(AB_CHAT, &id).await;
    assert_eq!(change, MessageChange::Delete);
    assert!(
        b.handle.search("airship", 10).await.unwrap().is_empty(),
        "the delete removed it"
    );
    assert!(a.handle.search("airship", 10).await.unwrap().is_empty());
    // a query of FTS syntax is only text, and never fails
    assert!(b.handle.search("NEAR(\"report\" -*:", 10).await.is_ok());
    a.shutdown();
    b.shutdown();
}

#[tokio::test]
async fn the_engine_indexes_what_was_stored_before_from_its_start_going_on_where_a_stopped_backfill_was() {
    let lab = Lab::start().await;
    let dir = Arc::new(tempfile::tempdir().unwrap());
    {
        // the store as an earlier build left it: messages stored without their text, of which a backfill that stopped
        // after its first batch indexed three
        let mut store = Store::open(&dir.path().join("store").join("lab.sqlite")).unwrap();
        store
            .upsert_conversation(&Conversation {
                id: AB_CHAT.to_owned(),
                kind: ConversationKind::OneToOne,
                topic: None,
                version: 1,
                last_activity: None,
            })
            .unwrap();
        for at in 0..7_i64 {
            let message = Message {
                conversation_id: AB_CHAT.to_owned(),
                id: (1_760_100_000_000 + at).to_string(),
                version: 1,
                client_message_id: None,
                sender: Some(LAB_A.to_owned()),
                sender_name: None,
                message_type: "RichText/Html".to_owned(),
                content: format!("<p>an old memo {at}</p>"),
                properties: None,
                arrival_time: 1_760_100_000_000 + at,
                compose_time: None,
                edit_time: None,
                delete_time: None,
            };
            store.upsert_message(&message, None).unwrap();
        }
        assert_eq!(Backfill::new(3).step(&mut store).unwrap(), Step::Indexed(3));
    }
    let mut b = try_native_in(&lab, LAB_B, QUIET, Arc::clone(&dir)).unwrap_or_else(|error| panic!("{error}"));
    let indexed = b
        .event(|event| match event {
            Event::Indexed { messages } => Some(*messages),
            _ => None,
        })
        .await;
    assert_eq!(
        indexed, 4,
        "the four the stopped backfill hadn't reached, in batches of three"
    );
    let hits = b.handle.search("memo", 50).await.unwrap();
    assert_eq!(hits.len(), 7);
    assert!(
        hits.windows(2).all(|pair| pair[0].arrival_time > pair[1].arrival_time),
        "the newest first"
    );
    b.shutdown();
}

/// The item reads of the shared file whose sharing token is `token` the mock got.
async fn item_reads(lab: &Lab, token: &str) -> usize {
    let path = format!("/v1.0/shares/{token}/driveItem");
    lab.mock
        .received_requests()
        .await
        .iter()
        .filter(|request| request.method.as_str() == "GET" && request.url.path() == path)
        .count()
}

#[tokio::test]
async fn a_shared_file_is_read_by_ranges_its_link_read_once_and_again_once_the_host_refuses_it() {
    let lab = Lab::start().await;
    let a = native(
        &lab,
        LAB_A,
        How {
            push: false,
            ..How::default()
        },
    );
    let share = "https://contoso-my.sharepoint.com/:v:/g/personal/made_up_user_contoso_test/MadeUpShareLinkEngine01";
    let token = teams_proto::FileDownload::of(share).unwrap().share().to_owned();
    let bytes: Vec<u8> = (0..=255_u8).cycle().take(5_000).collect();
    let download = "/personal/made_up_user_contoso_test/_layouts/15/download.aspx";
    let item = json!({
        "id": "made-up",
        "size": bytes.len(),
        "@microsoft.graph.downloadUrl": format!("https://contoso-my.sharepoint.com{download}?UniqueId=made-up&tempauth=made-up"),
    });
    lab.mock
        .serve_bytes(
            &format!("/v1.0/shares/{token}/driveItem"),
            "application/json",
            item.to_string().into_bytes(),
        )
        .await;
    lab.mock.serve_ranged(download, "video/mp4", bytes.clone()).await;

    // two parts asked at once: one read of the item for both
    let (first, second) = tokio::join!(a.handle.file_range(share, 0, 100), a.handle.file_range(share, 100, 100));
    let (first, second) = (first.unwrap(), second.unwrap());
    assert_eq!((first.bytes.as_slice(), first.total), (&bytes[..100], 5_000));
    assert_eq!(second.bytes, bytes[100..200]);
    assert_eq!(item_reads(&lab, &token).await, 1);

    // the host refuses the link (it expired): the item is read again once, and the part comes
    lab.mock.answer_next("GET", "download.aspx", 403, &json!({})).await;
    let last = a.handle.file_range(share, 4_900, 200).await.unwrap();
    assert_eq!(last.bytes, bytes[4_900..]);
    assert_eq!(item_reads(&lab, &token).await, 2);

    // a server error fails only that read; a link that names no SharePoint file can't be had
    lab.mock.answer_next("GET", "download.aspx", 502, &json!({})).await;
    assert!(matches!(
        a.handle.file_range(share, 0, 10).await,
        Err(EngineError::FileUnread)
    ));
    assert_eq!(a.handle.file_range(share, 0, 10).await.unwrap().bytes, bytes[..10]);
    assert!(matches!(
        a.handle.file_range("https://example.com/not-a-share", 0, 10).await,
        Err(EngineError::FileUnavailable)
    ));

    // a stream: its size told by its answer, its bytes as they came, fewer where the file ends
    let mut stream = a.handle.file_stream(share, 1_000, 10_000).await.unwrap();
    assert_eq!((stream.offset, stream.len, stream.total), (1_000, 4_000, 5_000));
    let mut streamed = Vec::new();
    while let Some(piece) = stream.next().await {
        streamed.extend(piece.unwrap());
    }
    assert_eq!(streamed, bytes[1_000..]);
    // the size, from the item whose link is kept
    assert_eq!(a.handle.file_size(share).await.unwrap(), 5_000);
    assert!(matches!(
        a.handle.file_stream("https://example.com/not-a-share", 0, 10).await,
        Err(EngineError::FileUnavailable)
    ));
    a.shutdown();
}

#[tokio::test]
async fn service_search_keeps_scopes_separate_and_reports_unreadable_answers_and_failed_reads() {
    use teams_core::SearchScope;
    use teams_proto::SearchKind;

    let lab = Lab::start().await;
    let a = native(
        &lab,
        LAB_A,
        How {
            push: false,
            ..How::default()
        },
    );
    assert!(!a.handle.search_service("  ", SearchScope::All).await.unwrap().more);
    for (scope, endpoint, response) in [
        (SearchScope::Suggestions, "suggestions", json!({"Groups": []})),
        (SearchScope::All, "query", json!({"EntitySets": []})),
        (
            SearchScope::Only(SearchKind::People),
            "query",
            json!({"EntitySets": []}),
        ),
    ] {
        lab.mock.answer_next("POST", endpoint, 200, &response).await;
        assert!(!a.handle.search_service("synthetic query", scope).await.unwrap().more);
    }
    for (status, response) in [(200, json!({"not": "search results"})), (403, Value::Null)] {
        lab.mock.answer_next("POST", "query", status, &response).await;
        assert!(matches!(
            a.handle.search_service("synthetic query", SearchScope::All).await,
            Err(EngineError::SearchFailed)
        ));
    }
    a.reads_fail.store(true, Ordering::SeqCst);
    assert!(matches!(
        a.handle.search_service("synthetic query", SearchScope::All).await,
        Err(EngineError::SearchFailed)
    ));
    a.shutdown();
}

#[tokio::test]
async fn reaction_history_and_animation_queries_are_answered_without_exposing_write_capabilities() {
    let lab = Lab::start().await;
    let a = native(&lab, LAB_A, QUIET);
    a.handle.sync().await.unwrap();
    assert!(!a.handle.read_only());
    assert!(a.handle.recent_reactions(0).await.unwrap().is_empty());
    assert!(a.handle.frequent_reactions(0).await.unwrap().is_empty());
    let recent = a.handle.recent_reactions(20).await.unwrap();
    let frequent = a.handle.frequent_reactions(20).await.unwrap();
    assert!(recent.iter().all(|key| frequent.contains(key)));
    assert!(
        a.handle
            .reaction_animation("../not-a-reaction")
            .await
            .unwrap()
            .is_none()
    );
    let animation = "/evergreen-assets/personal-expressions/v2/assets/emoticons/wow/default/50_anim_f.png";
    lab.mock
        .serve_bytes(animation, "image/png", b"synthetic animation strip".to_vec())
        .await;
    let (one, two) = tokio::join!(a.handle.reaction_animation("wow"), a.handle.reaction_animation("wow"));
    assert_eq!(one.unwrap().unwrap().as_ref(), b"synthetic animation strip");
    assert_eq!(two.unwrap().unwrap().as_ref(), b"synthetic animation strip");
    assert!(a.handle.reaction_animation("missing").await.unwrap().is_none());
    assert!(a.handle.reaction_animation("missing").await.unwrap().is_none());
    let requests = lab.mock.received_requests().await;
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.path() == animation)
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.path().contains("/emoticons/missing/"))
            .count(),
        1
    );
    a.shutdown();
}

fn conversation_delivery(id: &str, horizon: Option<&str>) -> Value {
    let mut resource = json!({"id": id});
    if let Some(horizon) = horizon {
        resource["properties"] = json!({"consumptionhorizon": horizon});
        resource["propertiesUpdated"] = json!(["consumptionhorizon"]);
    }
    json!({"type": "EventMessage", "resourceType": "ConversationUpdate", "resource": resource})
}

#[tokio::test]
async fn marking_all_read_in_teams_reads_the_chats_from_the_catch_up_and_from_a_delivery() {
    let lab = Lab::start().await;
    let mut b = native(&lab, LAB_B, How::default());
    b.ready().await;
    lab.mock.post_as(LAB_A, AB_CHAT, "<p>unread</p>").unwrap();
    b.handle.sync().await.unwrap();
    assert!(b.handle.unread_count(AB_CHAT).await.unwrap() > 0);

    lab.mock
        .answer_next(
            "GET",
            "users/ME/properties",
            200,
            &json!({"appConsumptionHorizon": "{\"chat\":9999999999999,\"channel\":0}"}),
        )
        .await;
    b.handle.sync().await.unwrap();
    assert_eq!(
        b.handle.unread_count(AB_CHAT).await.unwrap(),
        0,
        "read by the catch-up's read"
    );

    // the catch-up's own change events, so the wait below sees the delivery's
    b.drain().await;
    lab.trouter.deliver_chat(
        LAB_B,
        &json!({
            "type": "EventMessage",
            "resourceType": "CustomUserProperties",
            "resource": {"appConsumptionHorizon": {"chat": 0, "channel": 0}},
        }),
    );
    b.event(|event| {
        matches!(event, Event::ConversationChanged { conversation } if conversation == AB_CHAT).then_some(())
    })
    .await;
    assert!(
        b.handle.unread_count(AB_CHAT).await.unwrap() > 0,
        "taken back by a delivery"
    );
    b.shutdown();
}

#[tokio::test]
async fn a_conversation_delivery_updates_known_chats_and_syncs_new_ones_without_answering_it() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    a.ready().await;
    lab.trouter.deliver_chat(LAB_A, &conversation_delivery(AB_CHAT, None));
    a.event(|event| {
        matches!(event, Event::ConversationChanged { conversation } if conversation == AB_CHAT).then_some(())
    })
    .await;
    let horizon = "9999999999999;9999999999999;synthetic";
    lab.trouter
        .deliver_chat(LAB_A, &conversation_delivery(AB_CHAT, Some(horizon)));
    a.event(|event| matches!(event, Event::ReadHorizon { conversation } if conversation == AB_CHAT).then_some(()))
        .await;
    assert_eq!(
        a.handle.read_horizon(AB_CHAT).await.unwrap().unwrap().raw.as_deref(),
        Some(horizon)
    );
    lab.trouter
        .deliver_chat(LAB_A, &conversation_delivery(teams_mock::TEAM_CHANNEL, None));
    a.event(|event| {
        matches!(event, Event::ConversationChanged { conversation } if conversation == teams_mock::TEAM_CHANNEL)
            .then_some(())
    })
    .await;
    assert!(
        !a.handle
            .messages(teams_mock::TEAM_CHANNEL, None, 50)
            .await
            .unwrap()
            .messages
            .is_empty()
    );
    assert!(
        lab.trouter
            .sockets()
            .iter()
            .flat_map(|socket| &socket.frames)
            .all(|frame| !frame.starts_with("3:::")),
        "chat deliveries are never answered"
    );
    a.shutdown();
}

#[tokio::test]
async fn a_delivery_of_an_unreadable_conversation_reports_failure_and_schedules_recovery() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    a.ready().await;
    a.failures_expected = true;
    let absent = "19:synthetic-missing@thread.v2";
    lab.trouter.deliver_chat(LAB_A, &conversation_delivery(absent, None));
    a.event(|event| {
        matches!(
            event,
            Event::Failed {
                during: "adding a changed conversation",
                ..
            }
        )
        .then_some(())
    })
    .await;
    a.event(|event| {
        matches!(
            event,
            Event::CaughtUp {
                cause: Cause::Retry,
                ..
            }
        )
        .then_some(())
    })
    .await;
    lab.trouter.deliver_chat(
        LAB_A,
        &conversation_delivery(absent, Some("9999999999999;9999999999999;synthetic")),
    );
    a.event(|event| {
        matches!(
            event,
            Event::CaughtUp {
                cause: Cause::Retry,
                ..
            }
        )
        .then_some(())
    })
    .await;
    assert!(a.handle.read_horizon(absent).await.unwrap().is_none());
    a.shutdown();
}

#[tokio::test]
async fn bounded_shutdown_releases_a_held_read_and_the_blocking_event_api_receives_queued_events() {
    let lab = Lab::start().await;
    let a = native(&lab, LAB_A, QUIET);
    a.reads_held.send_replace(true);
    let request = a.handle.sync();
    let released = Arc::new(AtomicBool::new(false));
    let marked = Arc::clone(&released);
    a.handle.shutdown_within(Duration::ZERO, || {
        marked.store(true, Ordering::SeqCst);
        a.reads_held.send_replace(false);
    });
    assert!(released.load(Ordering::SeqCst));
    assert!(request.await.is_ok());
    let Native {
        mut handle,
        _dir: directory,
        ..
    } = native(&lab, LAB_A, QUIET);
    tokio::task::spawn_blocking(move || {
        let _keep = directory;
        assert!(matches!(handle.blocking_next_event(), Some(Event::Indexed { .. })));
        handle.shutdown_within(WAIT, || panic!("an idle engine must stop within its normal grace"));
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn incomplete_original_identity_refuses_edits_and_read_receipts_before_an_outbox_row_exists() {
    for (field, missing) in [("clientmessageid", "clientmessageid"), ("composetime", "compose time")] {
        let lab = Lab::start().await;
        assert!(lab.mock.change_message(AB_CHAT, "1111111111111111111", |message| {
            message.as_object_mut().unwrap().remove(field);
        }));
        let account = native(&lab, LAB_A, QUIET);
        account.handle.sync().await.unwrap();
        let error = account
            .handle
            .write(Action::Edit {
                conversation: AB_CHAT.into(),
                message_id: A_EDITED.into(),
                text: MessageText::plain("synthetic edit refused before enqueue").unwrap(),
            })
            .await
            .unwrap_err();
        assert!(
            matches!(error,EngineError::Unknown {missing: actual,action:"edited",..} if actual == missing),
            "{error:?}"
        );
        if field == "clientmessageid" {
            let error = account
                .handle
                .write(Action::Read {
                    conversation: AB_CHAT.into(),
                    message_id: A_EDITED.into(),
                })
                .await
                .unwrap_err();
            assert!(matches!(
                error,
                EngineError::Unknown {
                    missing: "clientmessageid",
                    action: "read up to",
                    ..
                }
            ));
        }
        let error = account
            .handle
            .write(Action::CreateChat {
                others: vec![],
                topic: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(error, EngineError::NotAChat));

        assert!(account.handle.outbox_entry(1).await.unwrap().is_none());
        account.shutdown();
    }
}

/// A control message `kind` of `from` in `chat`, delivered as the messaging socket delivers a typing one.
fn control_delivery(chat: &str, from: &str, kind: &str) -> Value {
    let base = "https://notifications.skype.net/v1/users/ME";
    json!({
        "time": "2023-11-14T22:13:21.0000000Z",
        "type": "EventMessage",
        "resourceLink": format!("{base}/conversations/{chat}/messages/1700000000950"),
        "resourceType": "NewMessage",
        "resource": {
            "clientmessageid": "1700000000900",
            "content": "",
            "from": format!("{base}/contacts/{from}"),
            "id": "1700000000950",
            "messagetype": kind,
            "originalarrivaltime": "2023-11-14T22:13:21.0000000Z",
            "version": "1700000000950",
            "type": "Message",
            "conversationLink": format!("{base}/conversations/{chat}"),
            "to": chat,
            "contenttype": "Application/Message",
            "threadtype": "chat",
        },
    })
}

#[tokio::test]
async fn typing_that_stops_is_told_stopped_and_nothing_of_it_is_stored() {
    let lab = Lab::start().await;
    let mut b = native(&lab, LAB_B, How::default());
    b.ready().await;
    lab.trouter
        .deliver_chat(LAB_B, &control_delivery(AB_CHAT, LAB_A, "Control/ClearTyping"));
    let from = b
        .event(|event| match event {
            Event::Typing {
                conversation,
                from,
                typing: false,
            } if conversation == AB_CHAT => Some(from.clone()),
            _ => None,
        })
        .await;
    assert_eq!(from.as_deref(), Some(LAB_A));
    assert!(b.handle.message(AB_CHAT, "1700000000950").await.unwrap().is_none());
    b.shutdown();
}

#[tokio::test]
async fn a_read_horizon_of_a_conversation_the_store_doesn_t_know_comes_with_its_catch_up() {
    let lab = Lab::start().await;
    let mut b = native(&lab, LAB_B, How::default());
    b.ready().await;
    let unknown = "19:00000000000000000000000000000bee@thread.v2";
    lab.trouter.deliver_chat(
        LAB_B,
        &conversation_delivery(unknown, Some("9999999999999;9999999999999;synthetic")),
    );
    // a catch-up is made due rather than the horizon kept for a conversation that isn't there
    b.event(|event| matches!(event, Event::CaughtUp { .. }).then_some(()))
        .await;
    assert!(b.handle.read_horizon(unknown).await.unwrap().is_none());
    b.shutdown();
}

/// The body of the last `PUT` to the feed whose query holds `query`.
async fn put_body(lab: &Lab, query: &str) -> Value {
    let feed = format!("/conversations/{}/", teams_mock::NOTIFICATIONS);
    let request = lab
        .mock
        .received_requests()
        .await
        .into_iter()
        .rev()
        .find(|request| {
            request.method.as_str() == "PUT"
                && decoded(request.url.path()).contains(&feed)
                && request.url.query().is_some_and(|got| got.contains(query))
        })
        .unwrap_or_else(|| panic!("no PUT {query}"));
    serde_json::from_slice(&request.body).unwrap()
}

/// One made-up feed item: a mention of a by b in the a–b chat, about message `message`.
fn mention_of_a(message: u64) -> Value {
    json!({
        "activityType": "mentionInChat", "activitySubtype": "person", "activityId": 500 + message,
        "sourceThreadId": AB_CHAT, "sourceMessageId": message, "sourceUserId": LAB_B,
        "sourceUserImDisplayName": "Lab B", "messagePreview": "made up",
    })
}

/// a's native client after its catch-up stored two made-up feed items: (older, newer).
async fn feed_of_a(lab: &Lab) -> (Native, String, String) {
    let older = lab.mock.add_activity(LAB_A, &mention_of_a(11), false).unwrap();
    let newer = lab.mock.add_activity(LAB_A, &mention_of_a(12), false).unwrap();
    let mut a = native(lab, LAB_A, How::default());
    a.ready().await;
    (a, older, newer)
}

/// Seen (`docs/protocol/chat.md`, "The activity feed"): the feed's horizon put at its newest item, the store's horizon
/// advanced after the send, the rail's count gone; seen again with nothing newer is refused. All read: the bookmark
/// stored after confirmation, every item read.
#[tokio::test]
async fn the_feed_seen_and_all_read_put_its_horizon_and_its_bookmark() {
    let lab = Lab::start().await;
    let (a, older, newer) = feed_of_a(&lab).await;
    let feed = a.handle.activity().await.unwrap();
    let newest = feed.items.first().unwrap().clone();
    assert_eq!(newest.id, newer);
    assert!(feed.item(&older).is_some(), "{feed:?}");
    assert!(feed.unseen() >= 2, "{feed:?}");

    let seen = a.write(Action::ActivitySeen).await;
    assert_eq!(
        (seen.kind, seen.state),
        (OutboxKind::ReadReceipt, OutboxState::Sent),
        "{seen:?}"
    );
    let feed = a.handle.activity().await.unwrap();
    assert_eq!((feed.seen_ms, feed.unseen()), (Some(newest.at_ms), 0), "{feed:?}");
    let put = put_body(&lab, "name=consumptionhorizon").await;
    let horizon = put["consumptionhorizon"].as_str().unwrap();
    assert!(horizon.starts_with(&format!("{};", newest.at_ms)), "{horizon}");
    assert!(
        horizon.ends_with(&format!(";{}", newest.activity_id.clone().unwrap())),
        "{horizon}"
    );
    assert!(matches!(
        a.handle.write(Action::ActivitySeen).await,
        Err(EngineError::NothingNewInActivity)
    ));

    let all = a.write(Action::ActivityAllRead).await;
    assert_eq!(
        (all.kind, all.state),
        (OutboxKind::FeedRead, OutboxState::Sent),
        "{all:?}"
    );
    let feed = a.handle.activity().await.unwrap();
    assert_eq!(feed.bookmark_ms, Some(all.created_at), "{feed:?}");
    assert!(feed.items.iter().all(|item| feed.is_read(item)), "{feed:?}");
    let body = put_body(&lab, "name=consumptionHorizonBookmark").await;
    assert_eq!(
        body["consumptionHorizonBookmark"].as_str(),
        Some(format!("0;{};0", all.created_at).as_str())
    );
    a.shutdown();
}

/// An item marked read, then unread: its `isread` put each time and its changed version synced back; an item the
/// feed doesn't hold is refused. Removed: soft-deleted, it leaves the feed once its delete comes back.
#[tokio::test]
async fn a_feed_item_marked_read_unread_or_removed_syncs_back_from_the_service() {
    let lab = Lab::start().await;
    let (mut a, older, newer) = feed_of_a(&lab).await;
    for read in [true, false] {
        let row = a
            .write(Action::ActivityRead {
                item: older.clone(),
                read,
            })
            .await;
        assert_eq!(
            (row.kind, row.state),
            (OutboxKind::ActivityRead, OutboxState::Sent),
            "{row:?}"
        );
        assert_eq!(put_body(&lab, "name=isread").await, json!({"isread": read.to_string()}));
        a.message_event(teams_mock::NOTIFICATIONS, &older).await;
        let feed = a.handle.activity().await.unwrap();
        let item = feed.item(&older).unwrap();
        assert_eq!(feed.is_read(item), read, "{item:?}");
    }
    let unknown = Action::ActivityRead {
        item: "1".to_owned(),
        read: true,
    };
    assert!(a.handle.write(unknown).await.is_err());

    let removed = a
        .write(Action::Delete {
            conversation: teams_mock::NOTIFICATIONS.to_owned(),
            message_id: newer.clone(),
        })
        .await;
    assert_eq!(removed.state, OutboxState::Sent, "{removed:?}");
    a.message_event(teams_mock::NOTIFICATIONS, &newer).await;
    let feed = a.handle.activity().await.unwrap();
    assert!(feed.item(&newer).is_none() && feed.item(&older).is_some(), "{feed:?}");
    a.shutdown();
}

#[tokio::test]
async fn the_feed_bookmark_syncs_into_a_fresh_store_and_from_a_conversation_delivery() {
    let lab = Lab::start().await;
    let (a, older, _) = feed_of_a(&lab).await;
    let all = a.handle.write(Action::ActivityAllRead).await.unwrap();
    assert_eq!(all.state, OutboxState::Sent);
    a.shutdown();

    let mut fresh = native(&lab, LAB_A, How::default());
    fresh.ready().await;
    let feed = fresh.handle.activity().await.unwrap();
    assert_eq!(feed.bookmark_ms, Some(all.created_at));
    assert!(
        feed.is_read(feed.item(&older).unwrap()),
        "bookmark came from the listing"
    );

    fresh.drain().await;
    let bookmark = all.created_at + 1000;
    lab.trouter.deliver_chat(
        LAB_A,
        &json!({
            "type": "EventMessage", "resourceType": "ConversationUpdate",
            "resource": {"id": teams_mock::NOTIFICATIONS, "properties": {
                "consumptionHorizonBookmark": format!("0;{bookmark};0")
            }, "propertiesUpdated": ["consumptionHorizonBookmark"]}
        }),
    );
    fresh
        .event(|event| {
            matches!(event, Event::ConversationChanged { conversation }
        if conversation == teams_mock::NOTIFICATIONS)
            .then_some(())
        })
        .await;
    assert_eq!(fresh.handle.activity().await.unwrap().bookmark_ms, Some(bookmark));
    fresh.shutdown();
}

#[tokio::test]
async fn failed_feed_horizons_leave_the_store_unchanged_and_resends_apply_only_after_confirmation() {
    let lab = Lab::start().await;
    let (mut a, _, _) = feed_of_a(&lab).await;
    for action in [Action::ActivitySeen, Action::ActivityAllRead] {
        let before = a.handle.activity().await.unwrap();
        a.next_write.store(429, Ordering::SeqCst);
        let pending = a.write(action).await;
        assert_eq!(pending.state, OutboxState::Pending);
        a.next_write.store(403, Ordering::SeqCst);
        let feed = a.handle.activity().await.unwrap();
        assert_eq!((feed.seen_ms, feed.bookmark_ms), (before.seen_ms, before.bookmark_ms));
        a.event(|event| match event {
            Event::Outbox(row) if row.id == pending.id && row.state == OutboxState::Failed => Some(()),
            _ => None,
        })
        .await;
        let feed = a.handle.activity().await.unwrap();
        assert_eq!((feed.seen_ms, feed.bookmark_ms), (before.seen_ms, before.bookmark_ms));
        // A full service sync cannot legitimize the failed local advance either.
        a.handle.sync().await.unwrap();
        let feed = a.handle.activity().await.unwrap();
        assert_eq!((feed.seen_ms, feed.bookmark_ms), (before.seen_ms, before.bookmark_ms));
        a.next_write.store(429, Ordering::SeqCst);
        let resent = a.handle.resend(pending.id).await.unwrap();
        assert_eq!(resent.state, OutboxState::Pending);
        let sent = a
            .event(|event| match event {
                Event::Outbox(row) if row.id == pending.id && row.state == OutboxState::Sent => Some(row.clone()),
                _ => None,
            })
            .await;
        let feed = a.handle.activity().await.unwrap();
        if sent.kind == OutboxKind::FeedRead {
            assert_eq!(feed.bookmark_ms, Some(sent.created_at));
            assert!(feed.items.iter().all(|item| feed.is_read(item)));
        } else {
            assert_eq!(feed.seen_ms, feed.items.first().map(|item| item.at_ms));
            assert_eq!(feed.unseen(), 0);
        }
    }
    a.shutdown();
}

/// Synthetic app item: app id is separate from its bot or attributed person's identity.
fn app_activity(app: &str) -> serde_json::Value {
    serde_json::json!({"activityType": "msGraph", "activitySubtype": "customNotification", "activityId": 1234,
        "sourceUserId": "28:made-up-bot", "sourceAppDisplayName": "Made-up Updates", "sourceThreadId": "", "sourceMessageId": 1,
        "activityContext": {"teamsAppId": app, "templateParameters": "{\"customMessage\":\"Made-up update\"}"}})
}

#[tokio::test]
async fn activity_turn_off_app_merges_current_preferences_and_reuses_its_definition() {
    let lab = Lab::start().await;
    let app = "00000000-0000-4000-8000-00000000a002";
    let id = lab.mock.add_activity(LAB_A, &app_activity(app), false).unwrap();
    let mut a = native(&lab, LAB_A, How::default());
    a.ready().await;
    let item = a.handle.activity().await.unwrap().item(&id).unwrap().clone();
    assert_eq!(item.teams_app_id.as_deref(), Some(app));
    // a banner's ask reads nothing: unknown until a read brought the definition
    assert_eq!(a.handle.cached_app_definition(app.to_owned()).await.unwrap(), None);
    let definition = a.handle.app_definition(app.to_owned()).await.unwrap().unwrap();
    assert_eq!(
        a.handle.cached_app_definition(app.to_owned()).await.unwrap(),
        Some(definition.clone())
    );
    assert_eq!(
        definition.activity_types().unwrap(),
        ["messageNotify", "customNotification"]
    );
    assert_eq!(
        a.handle.app_definition(app.to_owned()).await.unwrap().unwrap(),
        definition
    );
    let first = a.write(Action::ActivityTurnOffApp { item: id.clone() }).await;
    assert_eq!(
        first.state,
        OutboxState::Sent,
        "an account with no own properties can turn off an app"
    );
    let http = HttpTransport::with_mock_origin(Tokens::of(&lab.mock, LAB_A), CHAT_BASE, &lab.mock.origin()).unwrap();
    let original =
        serde_json::json!({"other-app": {"unknown": "keep"}, app: {"emailSettings": {"digest": true}, "future": 42}});
    let settings = teams_proto::UserGraphSettings::read_answer(
        serde_json::json!({"userGraphSettings": original.to_string()})
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    assert_eq!(
        http.deliver(teams_proto::SetUserGraphSettings(settings))
            .await
            .unwrap()
            .status(),
        200
    );
    let row = a.write(Action::ActivityTurnOffApp { item: id.clone() }).await;
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    assert_eq!(row.kind, OutboxKind::ActivityTurnOffApp);
    let answer = http.read(&teams_proto::UserGraphSettingsRead).await.unwrap();
    let settings = teams_proto::UserGraphSettings::read_answer(answer.body()).unwrap();
    let merged: serde_json::Value = serde_json::from_str(&settings.json_text()).unwrap();
    assert_eq!(merged["other-app"], original["other-app"]);
    assert_eq!(merged[app]["emailSettings"], original[app]["emailSettings"]);
    assert_eq!(merged[app]["future"], 42);
    for category in ["activityGenerationSettings", "toastSettings"] {
        assert_eq!(
            merged[app][category],
            serde_json::json!({"messageNotify": false, "customNotification": false, "systemDefault": false})
        );
    }
    assert!(
        a.handle.activity().await.unwrap().item(&id).is_some(),
        "existing feed items remain"
    );
    let requests = lab.mock.received_requests().await;
    let definitions: Vec<_> = requests
        .iter()
        .filter(|request| request.url.path().ends_with("/batchedDefinitions"))
        .collect();
    assert_eq!(definitions.len(), 1, "the icon reader and write share the cache");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&definitions[0].body).unwrap(),
        serde_json::json!([app])
    );
    let b = HttpTransport::with_mock_origin(Tokens::of(&lab.mock, LAB_B), CHAT_BASE, &lab.mock.origin()).unwrap();
    assert!(matches!(b.read(&teams_proto::UserGraphSettingsRead).await,
        Err(HttpError::Status(answer)) if answer.status() == 404));
    a.shutdown();
}

#[tokio::test]
async fn activity_turn_off_app_refuses_an_unknown_definition_or_non_app_item() {
    let lab = Lab::start().await;
    let id = lab
        .mock
        .add_activity(LAB_A, &app_activity("made-up-unknown-app"), false)
        .unwrap();
    let (a, plain, _) = feed_of_a(&lab).await;
    for item in [id, plain] {
        assert!(matches!(
            a.handle.write(Action::ActivityTurnOffApp { item }).await,
            Err(EngineError::ActivityAppUnavailable)
        ));
    }
    assert!(!lab.mock.received_requests().await.iter().any(|request| {
        request.method.as_str() == "PUT"
            && request
                .url
                .query_pairs()
                .any(|(key, value)| key == "name" && value == "userGraphSettings")
    }));
    a.shutdown();
}

#[tokio::test]
async fn activity_undo_turn_off_restores_the_app_without_reverting_other_apps() {
    let lab = Lab::start().await;
    let app = "00000000-0000-4000-8000-00000000a002";
    let id = lab.mock.add_activity(LAB_A, &app_activity(app), false).unwrap();
    let mut a = native(&lab, LAB_A, How::default());
    a.ready().await;
    let http = HttpTransport::with_mock_origin(Tokens::of(&lab.mock, LAB_A), CHAT_BASE, &lab.mock.origin()).unwrap();
    let original = json!({app: {"activityGenerationSettings": {"customNotification": true}, "toastSettings": {"customNotification": false}, "emailSettings": {"digest": true}}});
    let preferences = teams_proto::UserGraphSettings::read_answer(
        json!({"userGraphSettings": original.to_string()})
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    http.deliver(teams_proto::SetUserGraphSettings(preferences))
        .await
        .unwrap();
    let row = a.write(Action::ActivityTurnOffApp { item: id }).await;
    assert_eq!(row.state, OutboxState::Sent);
    let answer = http.read(&teams_proto::UserGraphSettingsRead).await.unwrap();
    let mut preferences = teams_proto::UserGraphSettings::read_answer(answer.body()).unwrap();
    preferences
        .turn_off("another-app", &["another-type".to_owned()])
        .unwrap();
    http.deliver(teams_proto::SetUserGraphSettings(preferences))
        .await
        .unwrap();
    let undo = a.write(Action::ActivityUndoTurnOffApp { row: row.id }).await;
    assert_eq!(undo.state, OutboxState::Sent);
    assert_eq!(undo.kind, OutboxKind::ActivityRestoreApp);
    let answer = http.read(&teams_proto::UserGraphSettingsRead).await.unwrap();
    let restored = teams_proto::UserGraphSettings::read_answer(answer.body()).unwrap();
    let restored: Value = serde_json::from_str(&restored.json_text()).unwrap();
    assert_eq!(restored[app], original[app]);
    assert_eq!(
        restored["another-app"]["activityGenerationSettings"]["another-type"],
        false
    );
    assert!(matches!(
        a.handle.write(Action::ActivityUndoTurnOffApp { row: undo.id }).await,
        Err(EngineError::ActivityAppUnavailable)
    ));
    a.shutdown();
}

#[tokio::test]
async fn activity_turn_off_keeps_queries_responsive_while_reading_the_definition() {
    let lab = Lab::start().await;
    let id = lab
        .mock
        .add_activity(LAB_A, &app_activity("00000000-0000-4000-8000-00000000a002"), false)
        .unwrap();
    let mut a = native(&lab, LAB_A, How::default());
    a.ready().await;
    lab.mock
        .answer_next_after("POST", "batchedDefinitions", Duration::from_secs(3))
        .await;
    let write = a.handle.write(Action::ActivityTurnOffApp { item: id.clone() });
    tokio::time::timeout(WAIT, async {
        loop {
            if lab
                .mock
                .received_requests()
                .await
                .iter()
                .any(|request| request.url.path().ends_with("/batchedDefinitions"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let feed = tokio::time::timeout(Duration::from_secs(1), a.handle.activity())
        .await
        .expect("local activity reads stay available during the definition request")
        .unwrap();
    assert!(feed.item(&id).is_some());
    assert_eq!(write.await.unwrap().state, OutboxState::Sent);
    a.shutdown();
}

/// The synthetic shapes of a message a policy blocked (`tests/fixtures/chatsvc/policy-blocked.json`), with the mock's
/// ids filled in.
fn blocked_shapes() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/chatsvc/policy-blocked.json"
    );
    let text = std::fs::read_to_string(path)
        .unwrap()
        .replace("<region>", "amer")
        .replace("<guid:a>", LAB_A.trim_start_matches("8:orgid:"))
        .replace("<guid:b>", LAB_B.trim_start_matches("8:orgid:"));
    serde_json::from_str(&text).unwrap()
}

/// The policy's update of message `id` (sent as `client_id`), at a version later than any the mock gave.
fn blocked_update(shapes: &Value, id: &str, client_id: &str) -> Value {
    let mut blocked = shapes["blocked"].clone();
    let version = (now_ms() + 60_000).to_string();
    blocked["id"] = json!(id);
    blocked["clientmessageid"] = json!(client_id);
    blocked["version"] = json!(version);
    blocked["properties"]["edittime"] = json!(version);
    json!({"type": "EventMessage", "resourceType": "MessageUpdate", "resource": blocked})
}

/// The service's copy of the blocked message, and its stream's own updates before it.
fn deliver_copy(lab: &Lab, shapes: &Value) {
    let mut stream = shapes["stream"].clone();
    stream["properties"]["consumptionhorizon"] = json!("1760000100900;1760000101000;7100000000000000002");
    stream["properties"]["consumptionHorizonBookmark"] = json!("synthetic-bookmark");
    lab.trouter.deliver_chat(
        LAB_A,
        &json!({"type": "EventMessage", "resourceType": "ThreadUpdate", "resource": {"id": stream["id"]}}),
    );
    lab.trouter.deliver_chat(
        LAB_A,
        &json!({"type": "EventMessage", "resourceType": "ConversationUpdate", "resource": stream}),
    );
    lab.trouter.deliver_chat(
        LAB_A,
        &json!({"type": "EventMessage", "resourceType": "NewMessage", "resource": shapes["copy"]}),
    );
}

/// Whether the engine read the copies' stream as a thread or a conversation's message list.
async fn read_the_stream(lab: &Lab) -> bool {
    lab.mock.received_requests().await.iter().any(|request| {
        let path = decoded(request.url.path());
        path.ends_with("/threads/48:generic") || path.ends_with("/conversations/48:generic/messages")
    })
}

fn files_of(message: &Message) -> usize {
    let properties: Value = serde_json::from_str(message.properties.as_deref().unwrap_or("{}")).unwrap();
    teams_proto::chat::shared_files(properties.get("files")).files.len()
}

#[tokio::test]
async fn a_blocked_message_keeps_its_text_and_files_from_the_copy_that_came_first_and_is_not_edited() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    a.ready().await;
    let row = a.send(AB_CHAT, "made up text as sent").await;
    let id = id_of(&row);
    let shapes = blocked_shapes();
    a.message_event(AB_CHAT, &id).await;

    deliver_copy(&lab, &shapes);
    lab.trouter
        .deliver_chat(LAB_A, &blocked_update(&shapes, &id, &row.client_message_id));
    let (_, applied, message) = a.message_event(AB_CHAT, &id).await;
    assert_eq!(applied, Applied::Updated);
    assert_eq!(message.content, shapes["copy"]["content"].as_str().unwrap());
    assert_eq!(files_of(&message), 1, "{:?}", message.properties);
    assert!(message.properties.as_deref().unwrap().contains("policyViolation"));
    assert_eq!(message.edit_time, None, "the verdict's update is no edit");

    // the later update (the link scan's data added) keeps it as it was put back
    let mut later = blocked_update(&shapes, &id, &row.client_message_id);
    later["resource"]["version"] = json!((now_ms() + 120_000).to_string());
    later["resource"]["properties"]["atp"] = shapes["blocked_atp"]["properties"]["atp"].clone();
    lab.trouter.deliver_chat(LAB_A, &later);
    let (_, applied, message) = a.message_event(AB_CHAT, &id).await;
    assert_eq!(applied, Applied::Updated);
    assert_eq!(message.content, shapes["copy"]["content"].as_str().unwrap());
    assert_eq!((files_of(&message), message.edit_time), (1, None));
    assert!(!read_the_stream(&lab).await, "the stream is never read as a chat");
    assert!(a.handle.conversation(GENERIC.to_owned()).await.unwrap().is_none());
    a.shutdown();
}

const GENERIC: &str = teams_proto::chat::GENERIC_STREAM;

#[tokio::test]
async fn a_blocked_message_keeps_its_text_as_sent_and_takes_a_copy_that_comes_after() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    a.ready().await;
    let shapes = blocked_shapes();

    // sent as written: the update before any copy keeps what the store had
    let row = a.send(AB_CHAT, "made up text kept").await;
    let id = id_of(&row);
    a.message_event(AB_CHAT, &id).await;
    lab.trouter
        .deliver_chat(LAB_A, &blocked_update(&shapes, &id, &row.client_message_id));
    let (_, _, message) = a.message_event(AB_CHAT, &id).await;
    assert!(message.content.contains("made up text kept"), "{}", message.content);
    assert_eq!(message.edit_time, None);

    // sent from another client: the update comes first without any text, its copy after puts it back
    let other = lab.mock.post_as(LAB_A, AB_CHAT, "<p>from another client</p>").unwrap();
    lab.trouter
        .deliver_chat(LAB_A, &blocked_update(&shapes, &other, "7100000000000000009"));
    let (_, _, message) = a.message_event(AB_CHAT, &other).await;
    assert_eq!((message.content.as_str(), files_of(&message)), ("", 0));
    deliver_copy(&lab, &shapes);
    let (_, _, message) = a.message_event(AB_CHAT, &other).await;
    assert_eq!(message.content, shapes["copy"]["content"].as_str().unwrap());
    assert_eq!(files_of(&message), 1);
    assert_eq!(a.stored(AB_CHAT, &other).await, message, "stored as told");
    let message = a.stored(AB_CHAT, &id).await;
    assert_eq!(
        message.content,
        shapes["copy"]["content"].as_str().unwrap(),
        "the copy replaces stale fallback text"
    );
    assert_eq!(files_of(&message), 1);
    assert!(
        a.handle
            .search("made up line", 10)
            .await
            .unwrap()
            .iter()
            .any(|hit| hit.message_id == other)
    );
    assert!(!read_the_stream(&lab).await);
    a.shutdown();
}

#[tokio::test]
async fn someone_else_s_blocked_message_keeps_no_text_and_takes_no_copy() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    let mut b = native(&lab, LAB_B, How::default());
    a.ready().await;
    b.ready().await;
    let row = a.send(AB_CHAT, "made up text b must not keep").await;
    let id = id_of(&row);
    b.message_event(AB_CHAT, &id).await;
    let shapes = blocked_shapes();
    lab.trouter
        .deliver_chat(LAB_B, &blocked_update(&shapes, &id, &row.client_message_id));
    let (_, applied, message) = b.message_event(AB_CHAT, &id).await;
    assert_eq!(applied, Applied::Updated);
    assert_eq!((message.content.as_str(), message.edit_time), ("", None));
    // even a copy of b's own naming it puts nothing back into a's message
    let mut copy = shapes["copy"].clone();
    copy["from"] = json!(format!("{CHAT_BASE}/v1/users/ME/contacts/{LAB_B}"));
    lab.trouter.deliver_chat(
        LAB_B,
        &json!({"type": "EventMessage", "resourceType": "NewMessage", "resource": copy}),
    );
    b.drain().await;
    assert_eq!(b.stored(AB_CHAT, &id).await.content, "");
    a.shutdown();
    b.shutdown();
}

#[tokio::test]
async fn an_offline_blocked_message_recovers_its_copy_from_history_and_keeps_it_across_restart() {
    let lab = Lab::start().await;
    let shapes = blocked_shapes();
    let id = shapes["blocked"]["id"].as_str().unwrap();
    let copy_id = shapes["copy"]["id"].as_str().unwrap();
    lab.mock
        .answer_next(
            "GET",
            &format!("/conversations/{AB_CHAT}/messages"),
            200,
            &json!({"messages": [shapes["blocked"]]}),
        )
        .await;
    lab.mock
        .answer_next(
            "GET",
            &format!("/conversations/48:generic/messages/{copy_id}"),
            200,
            &shapes["copy"],
        )
        .await;
    let how = How {
        push: false,
        ..How::default()
    };
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let a = try_native_in(&lab, LAB_A, how, Arc::clone(&dir)).unwrap();
    assert!(a.handle.sync().await.unwrap().failed.is_empty());
    let restored = a.stored(AB_CHAT, id).await;
    assert_eq!(restored.content, shapes["copy"]["content"].as_str().unwrap());
    assert_eq!(files_of(&restored), 1);
    assert!(
        a.handle
            .search("made up line", 10)
            .await
            .unwrap()
            .iter()
            .any(|hit| hit.message_id == id)
    );
    assert!(!read_the_stream(&lab).await);
    a.shutdown();
    let a = try_native_in(&lab, LAB_A, how, dir).unwrap();
    assert_eq!(a.stored(AB_CHAT, id).await, restored);
    lab.mock
        .answer_next(
            "GET",
            &format!("/conversations/{AB_CHAT}/messages"),
            200,
            &json!({"messages": [shapes["blocked"]]}),
        )
        .await;
    assert!(a.handle.sync().await.unwrap().failed.is_empty());
    assert_eq!(a.stored(AB_CHAT, id).await, restored);
    let reads = lab.mock.received_requests().await;
    assert_eq!(
        reads
            .iter()
            .filter(|request| decoded(request.url.path())
                .ends_with(&format!("/conversations/48:generic/messages/{copy_id}")))
            .count(),
        1
    );
    a.shutdown();
}

async fn answer_blocked_history(lab: &Lab, shapes: &Value) {
    let mut newer = shapes["sent"].clone();
    newer["id"] = json!("1760000102000");
    lab.mock
        .answer_next(
            "GET",
            &format!("/conversations/{AB_CHAT}/messages"),
            200,
            &json!({"messages": [newer, shapes["blocked"]], "_metadata": {
                "syncState": blocked_sync_link()
            }}),
        )
        .await;
}

fn blocked_sync_link() -> String {
    format!("{CHAT_BASE}/v1/users/ME/conversations/{AB_CHAT}/messages?syncState=blocked-recovery")
}

async fn answer_empty_blocked_sync(lab: &Lab) {
    lab.mock
        .answer_next(
            "GET",
            &format!("/conversations/{AB_CHAT}/messages"),
            200,
            &json!({"messages": [], "_metadata": {"syncState": blocked_sync_link()}}),
        )
        .await;
}

async fn copy_reads(lab: &Lab, shapes: &Value) -> usize {
    let suffix = format!(
        "/conversations/48:generic/messages/{}",
        shapes["copy"]["id"].as_str().unwrap()
    );
    lab.mock
        .received_requests()
        .await
        .iter()
        .filter(|request| decoded(request.url.path()).ends_with(&suffix))
        .count()
}

async fn restored_copy_event(a: &mut Native, shapes: &Value) {
    a.event(|event| match event {
        Event::Message {
            id,
            message: Some(message),
            ..
        } if id == shapes["blocked"]["id"].as_str().unwrap() && message.content == shapes["copy"]["content"] => {
            Some(())
        }
        _ => None,
    })
    .await;
}

async fn delivery_finished(lab: &Lab, a: &mut Native) {
    lab.trouter
        .deliver_chat(LAB_A, &control_delivery(AB_CHAT, LAB_B, "Control/Typing"));
    a.event(|event| matches!(event, Event::Typing { .. }).then_some(()))
        .await;
}

async fn native_with_sent_history(lab: &Lab, shapes: &Value) -> Native {
    lab.mock
        .answer_next(
            "GET",
            &format!("/conversations/{AB_CHAT}/messages"),
            200,
            &json!({"messages": [shapes["sent"]]}),
        )
        .await;
    let a = native(
        lab,
        LAB_A,
        How {
            push: false,
            ..How::default()
        },
    );
    assert!(a.handle.sync().await.unwrap().failed.is_empty());
    lab.mock
        .post_as(LAB_A, AB_CHAT, "<p>made up newer message</p>")
        .unwrap();
    a
}

#[tokio::test]
async fn a_failed_blocked_copy_read_keeps_history_readable_and_retries_later() {
    for status in [403, 410, 400, 500, 200] {
        let lab = Lab::start().await;
        let shapes = blocked_shapes();
        let id = shapes["blocked"]["id"].as_str().unwrap();
        let copy_id = shapes["copy"]["id"].as_str().unwrap();
        let mut a = native_with_sent_history(&lab, &shapes).await;
        answer_blocked_history(&lab, &shapes).await;
        lab.mock
            .answer_next("GET", &format!("/messages/{copy_id}"), status, &json!({}))
            .await;
        let report = a.handle.sync().await.unwrap();
        assert!(report.failed.is_empty(), "{status}: {report:?}");
        assert!(report.unreadable.is_empty(), "{status}: {report:?}");
        let fallback = a.stored(AB_CHAT, id).await;
        assert_eq!(fallback.content, shapes["sent"]["content"]);
        assert_eq!(files_of(&fallback), 1);
        assert_eq!(
            a.stored(AB_CHAT, "1760000102000").await.content,
            shapes["sent"]["content"]
        );
        assert_eq!(copy_reads(&lab, &shapes).await, 1);

        answer_empty_blocked_sync(&lab).await;
        lab.mock
            .answer_next("GET", &format!("/messages/{copy_id}"), 200, &shapes["copy"])
            .await;
        let synced = a.handle.sync_conversation(AB_CHAT).await.unwrap();
        assert_eq!(synced.changed(), 0);
        // Identical recovered content emits no new message event; wait for the retry itself.
        tokio::time::timeout(WAIT, async {
            while copy_reads(&lab, &shapes).await < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(a.stored(AB_CHAT, id).await.content, shapes["copy"]["content"]);
        assert_eq!(files_of(&a.stored(AB_CHAT, id).await), 1);
        assert_eq!(copy_reads(&lab, &shapes).await, 2);
        let requests = lab.mock.received_requests().await;
        let last = requests
            .iter()
            .rev()
            .find(|request| decoded(request.url.path()).ends_with(&format!("/conversations/{AB_CHAT}/messages")))
            .unwrap();
        assert!(
            last.url
                .query_pairs()
                .any(|(name, value)| name == "syncState" && value == "blocked-recovery")
        );
        assert!(
            a.drain()
                .await
                .iter()
                .all(|event| !matches!(event, Event::Failed { .. }))
        );
        a.shutdown();
    }
}

#[tokio::test]
async fn failed_blocked_copy_reads_retry_on_empty_catch_up_and_after_restart() {
    let lab = Lab::start().await;
    let shapes = blocked_shapes();
    let id = shapes["blocked"]["id"].as_str().unwrap();
    let copy_id = shapes["copy"]["id"].as_str().unwrap();
    answer_blocked_history(&lab, &shapes).await;
    lab.mock
        .answer_next("GET", &format!("/messages/{copy_id}"), 403, &json!({}))
        .await;
    let how = How {
        push: false,
        ..How::default()
    };
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let a = try_native_in(&lab, LAB_A, how, Arc::clone(&dir)).unwrap();
    let report = a.handle.sync().await.unwrap();
    assert!(report.failed.is_empty());
    assert_eq!(a.stored(AB_CHAT, id).await.content, "");
    assert_eq!(copy_reads(&lab, &shapes).await, 1);

    lab.mock
        .answer_next("GET", "/conversations", 200, &json!({"conversations": []}))
        .await;
    lab.mock
        .answer_next("GET", &format!("/messages/{copy_id}"), 403, &json!({}))
        .await;
    let report = a.handle.sync().await.unwrap();
    assert!(report.failed.is_empty());
    assert_eq!(report.conversations, 0);
    assert_eq!(a.stored(AB_CHAT, id).await.content, "");
    a.shutdown();
    assert_eq!(copy_reads(&lab, &shapes).await, 2);

    lab.mock
        .answer_next("GET", "/conversations", 200, &json!({"conversations": []}))
        .await;
    lab.mock
        .answer_next("GET", &format!("/messages/{copy_id}"), 200, &shapes["copy"])
        .await;
    let mut a = try_native_in(&lab, LAB_A, how, dir).unwrap();
    let report = a.handle.sync().await.unwrap();
    assert!(report.failed.is_empty());
    assert_eq!(report.conversations, 0);
    restored_copy_event(&mut a, &shapes).await;
    let restored = a.stored(AB_CHAT, id).await;
    assert_eq!(restored.content, shapes["copy"]["content"]);
    assert_eq!(files_of(&restored), 1);
    assert_eq!(copy_reads(&lab, &shapes).await, 3);
    assert!(
        a.handle
            .search("made up line", 10)
            .await
            .unwrap()
            .iter()
            .any(|hit| hit.message_id == id)
    );
    a.shutdown();
}

#[tokio::test]
async fn a_failed_blocked_copy_read_on_delivery_keeps_the_fallback_without_failing_catch_up() {
    for status in [403, 500] {
        let lab = Lab::start().await;
        let mut a = native(&lab, LAB_A, How::default());
        a.ready().await;
        let row = a.send(AB_CHAT, "made up fallback").await;
        let id = id_of(&row);
        a.message_event(AB_CHAT, &id).await;
        let shapes = blocked_shapes();
        let copy_id = shapes["copy"]["id"].as_str().unwrap();
        lab.mock
            .answer_next("GET", &format!("/messages/{copy_id}"), status, &json!({}))
            .await;
        lab.trouter
            .deliver_chat(LAB_A, &blocked_update(&shapes, &id, &row.client_message_id));
        let (_, _, message) = a.message_event(AB_CHAT, &id).await;
        assert!(message.content.contains("made up fallback"));
        assert_eq!(message.edit_time, None);
        delivery_finished(&lab, &mut a).await;
        assert!(
            a.drain()
                .await
                .iter()
                .all(|event| !matches!(event, Event::Failed { .. }))
        );
        assert_eq!(copy_reads(&lab, &shapes).await, 1);
        deliver_copy(&lab, &shapes);
        let (_, _, message) = a.message_event(AB_CHAT, &id).await;
        assert_eq!(message.content, shapes["copy"]["content"]);
        a.shutdown();
    }
}

#[tokio::test]
async fn missing_or_rejected_blocked_copies_are_read_once_per_session_and_push_can_still_restore_them() {
    for status in [404, 200] {
        let lab = Lab::start().await;
        let mut a = native(&lab, LAB_A, How::default());
        a.ready().await;
        let shapes = blocked_shapes();
        let id = shapes["blocked"]["id"].as_str().unwrap();
        let copy_id = shapes["copy"]["id"].as_str().unwrap();
        let mut rejected = shapes["copy"].clone();
        rejected["from"] = json!(format!("{CHAT_BASE}/v1/users/ME/contacts/{LAB_B}"));
        lab.mock
            .answer_next("GET", &format!("/messages/{copy_id}"), status, &rejected)
            .await;
        for _ in 0..2 {
            answer_blocked_history(&lab, &shapes).await;
            a.handle.sync_conversation(AB_CHAT).await.unwrap();
            assert_eq!(a.stored(AB_CHAT, id).await.content, "");
        }
        lab.trouter
            .deliver_chat(LAB_A, &blocked_update(&shapes, id, "7100000000000000001"));
        a.message_event(AB_CHAT, id).await;
        delivery_finished(&lab, &mut a).await;
        assert!(
            a.drain()
                .await
                .iter()
                .all(|event| !matches!(event, Event::Failed { .. }))
        );
        assert_eq!(copy_reads(&lab, &shapes).await, 1);
        deliver_copy(&lab, &shapes);
        let (_, _, message) = a.message_event(AB_CHAT, id).await;
        assert_eq!(message.content, shapes["copy"]["content"]);
        assert_eq!(files_of(&message), 1);
        a.shutdown();
    }
}

fn native_with_stored_missing_copy(lab: &Lab, how: How) -> Native {
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let shapes = blocked_shapes();
    {
        let mut store = Store::open(&dir.path().join("store").join("lab.sqlite")).unwrap();
        for id in [AB_CHAT, teams_core::NOTES] {
            store
                .upsert_conversation(&Conversation {
                    id: id.to_owned(),
                    kind: ConversationKind::OneToOne,
                    topic: None,
                    version: 0,
                    last_activity: None,
                })
                .unwrap();
        }
        store
            .upsert_message(
                &Message {
                    conversation_id: AB_CHAT.to_owned(),
                    id: shapes["blocked"]["id"].as_str().unwrap().to_owned(),
                    version: 1,
                    client_message_id: None,
                    sender: Some(LAB_A.to_owned()),
                    sender_name: None,
                    message_type: "RichText/Html".to_owned(),
                    content: String::new(),
                    properties: Some(shapes["blocked"]["properties"].to_string()),
                    arrival_time: 1_760_000_100_000,
                    compose_time: None,
                    edit_time: None,
                    delete_time: None,
                },
                None,
            )
            .unwrap();
    }
    try_native_in(lab, LAB_A, how, dir).unwrap()
}

async fn delay_stored_copy(lab: &Lab) {
    let shapes = blocked_shapes();
    let copy_id = shapes["copy"]["id"].as_str().unwrap();
    lab.mock
        .refuse_next_after("GET", &format!("/messages/{copy_id}"), 403, Duration::from_secs(8))
        .await;
}

#[tokio::test]
async fn chat_sync_and_older_history_run_beside_deferred_copy_recovery() {
    let lab = Lab::start().await;
    delay_stored_copy(&lab).await;
    let messages = format!("{CHAT_BASE}/v1/users/ME/conversations/{ABC_CHAT}/messages");
    lab.mock
        .answer_next(
            "GET",
            &format!("/conversations/{ABC_CHAT}/messages"),
            200,
            &json!({"messages": [], "_metadata": {
                "syncState": format!("{messages}?syncState=made-up-sync"),
                "backwardLink": format!("{messages}?syncState=made-up-older"),
            }}),
        )
        .await;
    let mut a = native_with_stored_missing_copy(&lab, How::default());
    a.event(|event| matches!(event, Event::CaughtUp { .. }).then_some(()))
        .await;
    tokio::time::timeout(Duration::from_secs(4), async {
        while copy_reads(&lab, &blocked_shapes()).await == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("deferred copy recovery never started");
    lab.mock
        .answer_next(
            "GET",
            &format!("/conversations/{ABC_CHAT}/messages"),
            200,
            &json!({"messages": []}),
        )
        .await;
    tokio::time::timeout(Duration::from_secs(4), async {
        a.handle.sync_conversation(ABC_CHAT).await.unwrap();
        lab.mock
            .answer_next(
                "GET",
                &format!("/conversations/{ABC_CHAT}/messages"),
                200,
                &json!({"messages": []}),
            )
            .await;
        assert!(a.handle.load_older(ABC_CHAT).await.unwrap().is_some());
        assert_eq!(a.handle.history_complete(ABC_CHAT).await.unwrap(), Some(true));
    })
    .await
    .expect("foreground sync or history waited for deferred copy recovery");
    a.shutdown();
    assert_eq!(copy_reads(&lab, &blocked_shapes()).await, 1);
}

#[tokio::test]
async fn standalone_sync_replies_and_publishes_before_slow_stored_copy_retries() {
    for status in [200, 403] {
        let lab = Lab::start().await;
        let mut a = native_with_stored_missing_copy(&lab, QUIET);
        delay_stored_copy(&lab).await;
        let mut message = blocked_shapes()["sent"].clone();
        message["id"] = json!("1760000103000");
        lab.mock
            .answer_next(
                "GET",
                &format!("/conversations/{AB_CHAT}/messages"),
                status,
                &json!({"messages": [message]}),
            )
            .await;
        tokio::time::timeout(Duration::from_secs(4), async {
            let result = a.handle.sync_conversation(AB_CHAT).await;
            assert_eq!(result.is_ok(), status == 200, "{result:?}");
            if status == 200 {
                a.message_event(AB_CHAT, "1760000103000").await;
            }
        })
        .await
        .expect("normal sync completion waited for the copy retry");
        a.shutdown();
        assert_eq!(copy_reads(&lab, &blocked_shapes()).await, 1);
    }
}

#[tokio::test]
async fn a_write_replies_and_publishes_its_message_and_outbox_before_slow_stored_copy_retries() {
    let lab = Lab::start().await;
    let mut a = native_with_stored_missing_copy(&lab, QUIET);
    delay_stored_copy(&lab).await;
    tokio::time::timeout(Duration::from_secs(4), async {
        let row = a.send(AB_CHAT, "made up prompt send").await;
        let mut message_seen = false;
        let mut outbox_seen = false;
        a.event(|event| {
            match event {
                Event::Message { id, .. } if Some(id) == row.message_id.as_ref() => message_seen = true,
                Event::Outbox(entry) if entry.id == row.id => outbox_seen = true,
                _ => {}
            }
            (message_seen && outbox_seen).then_some(())
        })
        .await;
    })
    .await
    .expect("write completion waited for the copy retry");
    a.shutdown();
    assert_eq!(copy_reads(&lab, &blocked_shapes()).await, 1);
}

#[tokio::test]
async fn catch_up_publishes_notes_and_completion_before_slow_stored_copy_retries() {
    let lab = Lab::start().await;
    delay_stored_copy(&lab).await;
    let mut note = blocked_shapes()["sent"].clone();
    note["conversationid"] = json!(teams_core::NOTES);
    note["conversationLink"] = json!(format!("{CHAT_BASE}/v1/users/ME/conversations/{}", teams_core::NOTES));
    lab.mock
        .answer_next(
            "GET",
            &format!("/conversations/{}/messages", teams_core::NOTES),
            200,
            &json!({"messages": [note]}),
        )
        .await;
    let mut a = native_with_stored_missing_copy(&lab, How::default());
    tokio::time::timeout(Duration::from_secs(4), async {
        let mut notes_seen = false;
        let mut completion_seen = false;
        a.event(|event| {
            match event {
                Event::Message { conversation, .. } if conversation == teams_core::NOTES => notes_seen = true,
                Event::CaughtUp { report, .. } => {
                    assert!(report.failed.is_empty(), "{report:?}");
                    completion_seen = true;
                }
                _ => {}
            }
            (notes_seen && completion_seen).then_some(())
        })
        .await;
    })
    .await
    .expect("notes or catch-up completion waited for the copy retry");
    a.shutdown();
    assert_eq!(copy_reads(&lab, &blocked_shapes()).await, 1);
}
