//! The catch-up sync on a real store (a temporary file) against the mock chat service (`teams-mock`, reached through
//! the `test-support` mock origin): the first catch-up fills the store; one after a gap brings what was missed, once;
//! Trouter deliveries (here the mock's queued events, the resources a delivery carries) and the sync agree; a
//! reconnect triggers a catch-up; the outbox's check after a lost answer finds the message by its `clientmessageid`.
//! A few shapes the mock doesn't serve (versions that can't be read, a sync answer with a `backwardLink`) are played
//! on plain wiremock.
//!
//! The other lab accounts write straight to the mock with plain HTTP, as their own clients would. Every id and token
//! here is the mock's made-up one, or a placeholder, never a real one.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::collections::VecDeque;
use std::future::{self, Future};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::{
    CLOCK_SKEW_MS, CatchUp, CatchUpError, CatchUpReport, Clock, Command, DRAFTS, Gaps, MAX_FAILED_CHECKS, MAX_PAGES,
    NOTES, Outbox, OutboxError, StoredMessage, UnreadableSeen, enqueue, outbox::ConversationSync, outcome_unknown,
};
use teams_mock::{
    AB_CHAT, ABC_CHAT, AC_CHAT, AX_CHAT, CHANNEL, ChatEvent, ChatServiceMock, EventKind, LAB_A, LAB_B, LAB_C,
    NOTIFICATIONS, TEAM_CHANNEL,
};
use teams_proto::Mri;
use teams_proto::chat::{self, MessageText};
use teams_proto::{
    Author, Client, Credentials, Failure, HttpError, HttpTransport, Read, Request as ProtoRequest, Response, Transport,
};
use teams_store::{
    Applied, AttemptOutcome, Conversation, ConversationKind, Member, Message, OutboxEntry, OutboxState, Store,
    SyncState,
};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const CHAT_BASE: &str = teams_mock::CHAT_BASE;
const START: i64 = 1_760_100_000_000;

// --- fakes ---

/// The mock's token for one account, the same after a rejection.
struct MockToken(String);

impl Credentials for MockToken {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }
}

/// What the next write's delivery does instead of an ordinary one.
#[derive(Clone, Copy, Debug)]
enum Fault {
    /// The post reaches the service, but its answer is lost: a timeout.
    LoseAnswer,
    /// The post times out before it reaches the service.
    DropRequest,
}

/// The HTTP transport with scripted faults on its writes.
struct Flaky {
    inner: HttpTransport<MockToken>,
    faults: Arc<Mutex<VecDeque<Fault>>>,
}

impl Transport for Flaky {
    type Error = HttpError;

    fn read<R: Read>(&self, request: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        self.inner.read(request)
    }

    fn send<R: ProtoRequest>(&self, request: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        self.inner.send(request)
    }

    async fn deliver<R: teams_proto::Request + Send>(&self, write: R) -> Result<Response, HttpError> {
        let fault = self.faults.lock().unwrap_or_else(PoisonError::into_inner).pop_front();
        match fault {
            None => self.inner.deliver(write).await,
            Some(Fault::LoseAnswer) => {
                self.inner.deliver(write).await?;
                Err(HttpError::Failed(Failure::Timeout))
            }
            Some(Fault::DropRequest) => Err(HttpError::Failed(Failure::Timeout)),
        }
    }
}

// --- setup ---

fn transport(origin: &str, token: &str) -> HttpTransport<MockToken> {
    HttpTransport::with_mock_origin(Arc::new(MockToken(token.to_owned())), CHAT_BASE, origin).unwrap()
}

/// The outbox of the account `me` (the one place that builds one).
fn outbox(transport: Flaky, me: &str) -> Outbox<Flaky> {
    Outbox::new(Arc::new(transport), Author::new(Mri::new(me), "Lab account"))
}

/// A message post as a command (the one place that builds one).
fn send(conversation: &str, text: &str) -> Command {
    Command::Send {
        conversation: conversation.to_owned(),
        text: MessageText::plain(text).unwrap(),
    }
}

/// One native client's account: its store, its outbox and its catch-up, against one origin.
struct Account {
    store: Store,
    outbox: Arc<Outbox<Flaky>>,
    catch_up: CatchUp<MockToken, Flaky>,
    faults: Arc<Mutex<VecDeque<Fault>>>,
    _dir: tempfile::TempDir,
}

impl Account {
    fn on(origin: &str, me: &str, token: &str) -> Self {
        Self::with_outbox_clock(origin, me, token, Arc::new(now_ms))
    }

    /// An account whose outbox reads `clock` (a backoff passed without waiting for it).
    fn with_outbox_clock(origin: &str, me: &str, token: &str, clock: Clock) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("store").join("lab.sqlite")).unwrap();
        let faults = Arc::new(Mutex::new(VecDeque::new()));
        let flaky = Flaky {
            inner: transport(origin, token),
            faults: Arc::clone(&faults),
        };
        let outbox = Arc::new(outbox(flaky, me).with_clock(clock));
        let client = Arc::new(Client::new(transport(origin, token)));
        let clock = Arc::new(AtomicI64::new(START));
        let catch_up = CatchUp::new(client, Arc::clone(&outbox), Mri::new(me))
            .with_clock(Arc::new(move || clock.fetch_add(1, Ordering::SeqCst)));
        Self {
            store,
            outbox,
            catch_up,
            faults,
            _dir: dir,
        }
    }

    fn of(mock: &ChatServiceMock, me: &str) -> Self {
        Self::on(&mock.origin(), me, &mock.token(me).unwrap())
    }

    fn fault(&self, fault: Fault) {
        self.faults
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(fault);
    }

    /// Every stored message of a conversation, the newest first.
    fn messages(&self, conversation: &str) -> Vec<Message> {
        self.store.messages_before(conversation, None, 1000).unwrap().messages
    }

    fn message(&self, conversation: &str, id: &str) -> Message {
        self.store.message(conversation, id).unwrap().unwrap()
    }

    /// Stores the resource of each event, as a Trouter delivery of it would; returns what the store did with each.
    fn deliver(&mut self, events: &[ChatEvent]) -> Vec<Applied> {
        events
            .iter()
            .map(|event| {
                let message: chat::Message = serde_json::from_value(event.resource.clone()).unwrap();
                let received = self
                    .catch_up
                    .receive_message(&mut self.store, &event.conversation, &message)
                    .unwrap();
                received.unwrap().applied
            })
            .collect()
    }
}

/// Another lab account's own client, writing straight to the mock with the web client's headers.
struct Writer {
    http: reqwest::Client,
    origin: String,
    token: String,
}

impl Writer {
    fn of(mock: &ChatServiceMock, who: &str) -> Self {
        Self {
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            origin: mock.origin(),
            token: mock.token(who).unwrap(),
        }
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: &Value) -> reqwest::Response {
        let answer = self
            .http
            .request(method, format!("{}/api/chatsvc/amer/v1/{path}", self.origin))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("X-MS-Migration", "True")
            .header("BehaviorOverride", "redirectAs404")
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        assert!(answer.status().is_success(), "{path}: {}", answer.status());
        answer
    }

    /// Posts a message to `chat`; returns its id.
    async fn post(&self, chat: &str, text: &str) -> String {
        let body = json!({
            "content": format!("<p>{text}</p>"),
            "messagetype": "RichText/Html",
            "contenttype": "Text",
            "clientmessageid": format!("9{}", text.len()),
            "imdisplayname": "Lab",
            "properties": {"mentions": "[]", "files": "[]"},
        });
        let answer = self
            .send(
                reqwest::Method::POST,
                &format!("{}/messages", conversation(chat)),
                &body,
            )
            .await;
        let posted: Value = serde_json::from_str(&answer.text().await.unwrap()).unwrap();
        posted["OriginalArrivalTime"].to_string()
    }

    async fn edit(&self, chat: &str, id: &str, text: &str) {
        let body = json!({"content": format!("<p>{text}</p>"), "messagetype": "RichText/Html"});
        self.send(
            reqwest::Method::PUT,
            &format!("{}/messages/{id}", conversation(chat)),
            &body,
        )
        .await;
    }

    async fn react(&self, chat: &str, id: &str, key: &str) {
        let path = format!("{}/messages/{id}/properties?name=emotions", conversation(chat));
        let body = json!({"emotions": {"key": key, "value": 1_760_000_055_000_u64}});
        self.send(reqwest::Method::PUT, &path, &body).await;
    }
}

/// A conversation's path, its id one segment.
fn conversation(chat: &str) -> String {
    format!(
        "users/ME/conversations/{}",
        chat.replace(':', "%3A").replace('@', "%40")
    )
}

/// The path (after the chat base's `v1`) and the `syncState` of every request the mock got since `from`.
async fn reads_since(mock: &ChatServiceMock, from: usize) -> Vec<String> {
    mock.received_requests()
        .await
        .iter()
        .skip(from)
        .map(|request| {
            let path = request
                .url
                .path()
                .trim_start_matches("/api/chatsvc/amer/v1/")
                .to_owned();
            let sync = request
                .url
                .query_pairs()
                .find(|(name, _)| name == "syncState")
                .map(|(_, value)| format!(" since {value}"))
                .unwrap_or_default();
            format!("{} {path}{sync}", request.method)
        })
        .collect()
}

fn encoded(chat: &str) -> String {
    chat.replace(':', "%3A").replace('@', "%40")
}

// --- the first catch-up ---

#[tokio::test]
async fn the_first_catch_up_fills_the_store() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut a = Account::of(&mock, LAB_A);
    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!((report.conversations, report.skipped), (6, 0));

    let kind = |id: &str| a.store.conversation(id).unwrap().unwrap().kind;
    assert_eq!(kind(AB_CHAT), ConversationKind::OneToOne);
    assert_eq!(kind(AC_CHAT), ConversationKind::OneToOne);
    assert_eq!(kind(AX_CHAT), ConversationKind::OneToOne);
    assert_eq!(kind(ABC_CHAT), ConversationKind::Group);
    assert_eq!(kind(CHANNEL), ConversationKind::Channel);
    assert_eq!(kind(NOTIFICATIONS), ConversationKind::Other("notifications".to_owned()));

    // the a–b chat's messages, each as the service has it now
    let messages = a.messages(AB_CHAT);
    assert_eq!(messages.len(), 5);
    assert_eq!(
        messages.len() + a.messages(NOTIFICATIONS).len(),
        report.messages.inserted
    );
    let deleted = &messages[0];
    assert!(deleted.delete_time.is_some() && deleted.content.is_empty());
    assert_eq!(messages[2].edit_time, Some(1_760_000_020_000));
    assert_eq!(messages[4].message_type, "ThreadActivity/AddMember");
    assert!(messages[1].client_message_id.is_some() && messages[1].sender.is_some());
    let page = a.store.conversations_by_activity(10).unwrap();
    assert_eq!(page[0].id, AB_CHAT, "the most recent activity first");

    // members of the chats, none of the read-only conversations
    let mris = |id: &str| -> Vec<String> { a.store.members(id).unwrap().into_iter().map(|m| m.mri).collect() };
    assert_eq!(mris(AB_CHAT), [LAB_A, LAB_B]);
    assert_eq!(mris(ABC_CHAT), [LAB_A, LAB_B, LAB_C]);
    assert!(mris(CHANNEL).is_empty() && mris(NOTIFICATIONS).is_empty());

    // a's own read horizon, and where every sync stands
    let horizon = a.store.read_horizon(AB_CHAT, LAB_A).unwrap().unwrap();
    assert!(horizon.raw.is_some() && horizon.last_read_arrival > 0);
    for id in [AB_CHAT, AC_CHAT, AX_CHAT, ABC_CHAT, CHANNEL, NOTIFICATIONS] {
        let state = a.store.sync_state(id).unwrap().unwrap();
        assert!(state.sync_link.is_some() && state.history_complete, "{id}: {state:?}");
    }
    assert!(a.store.conversation_list_sync_link().unwrap().is_some());
    // empty as the list says (the mock calls a conversation empty while it holds no message)
    let empty = a.store.empty_conversations().unwrap();
    for id in [AB_CHAT, AC_CHAT, AX_CHAT, ABC_CHAT, CHANNEL, NOTIFICATIONS] {
        assert_eq!(empty.iter().any(|e| e == id), a.messages(id).is_empty(), "{id}");
    }

    // nothing changed: the next catch-up reads the list's sync link and nothing else
    let before = mock.received_requests().await.len();
    let again = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert_eq!((again.conversations, again.messages.changed()), (0, 0));
    let reads = reads_since(&mock, before).await;
    assert_eq!(reads.len(), 2, "{reads:?}");
    assert_eq!(reads[0], "GET users/ME/properties", "the mark-all-read horizon");
    assert!(reads[1].starts_with(&format!("GET users/{LAB_A}/conversations since mock-since-")));
}

#[tokio::test]
async fn older_history_is_read_a_page_at_a_time_from_the_kept_backward_link() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut b = Account::of(&mock, LAB_B);
    b.catch_up = b.catch_up.with_page_size(2);
    let first = b.catch_up.sync_messages(&mut b.store, AB_CHAT).await.unwrap();
    assert_eq!((first.inserted, first.pages), (2, 1));
    let state = b.store.sync_state(AB_CHAT).unwrap().unwrap();
    assert!(!state.history_complete && state.backward_link.is_some());

    let mut loaded = 0;
    while let Some(older) = b.catch_up.load_older(&mut b.store, AB_CHAT).await.unwrap() {
        loaded += older.inserted;
    }
    assert_eq!(loaded, 3);
    assert_eq!(b.messages(AB_CHAT).len(), 5);
    let done = b.store.sync_state(AB_CHAT).unwrap().unwrap();
    assert!(done.history_complete && done.backward_link.is_none());
    assert_eq!(done.sync_link, state.sync_link, "paging back keeps the sync link");
    assert!(b.catch_up.load_older(&mut b.store, ABC_CHAT).await.unwrap().is_none());
}

#[tokio::test]
async fn a_conversation_the_store_does_not_know_is_read_as_a_thread_first() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut c = Account::of(&mock, LAB_C);
    let synced = c.catch_up.sync_messages(&mut c.store, ABC_CHAT).await.unwrap();
    assert_eq!(synced.changed(), 0);
    let stored = c.store.conversation(ABC_CHAT).unwrap().unwrap();
    assert_eq!((stored.kind, stored.version), (ConversationKind::Group, 0));
    assert_eq!(
        c.store.members(ABC_CHAT).unwrap(),
        [LAB_A, LAB_B, LAB_C].map(|mri| Member::new(mri, Some("Admin")))
    );
    // the list's version replaces the thread's placeholder
    c.catch_up.catch_up(&mut c.store).await.unwrap();
    assert!(c.store.conversation(ABC_CHAT).unwrap().unwrap().version > 0);
}

// --- a gap ---

#[tokio::test]
async fn a_catch_up_after_a_gap_brings_what_was_missed_exactly_once() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut a = Account::of(&mock, LAB_A);
    a.catch_up.catch_up(&mut a.store).await.unwrap();
    let edited_before = a.message(AB_CHAT, "1760000010000");

    // a's push channel is down: b and c write meanwhile
    let b = Writer::of(&mock, LAB_B);
    let c = Writer::of(&mock, LAB_C);
    let first = b.post(AB_CHAT, "one").await;
    b.post(AB_CHAT, "two two").await;
    c.post(ABC_CHAT, "to the group").await;
    b.edit(AB_CHAT, &first, "one, edited").await;
    b.react(AB_CHAT, &edited_before.id, "heart").await;

    let before = mock.received_requests().await.len();
    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(report.conversations, 2, "the two chats that changed");
    assert_eq!((report.messages.inserted, report.messages.updated), (3, 1));
    let reads = reads_since(&mock, before).await;
    assert_eq!(
        reads.len(),
        4,
        "the properties, the list and the two chats' sync links: {reads:?}"
    );
    assert!(
        reads[2..]
            .iter()
            .all(|read| read.contains("/messages since mock-since-")),
        "{reads:?}"
    );

    let messages = a.messages(AB_CHAT);
    assert_eq!(messages.len(), 7, "each message once");
    assert_eq!(a.message(AB_CHAT, &first).content, "<p>one, edited</p>");
    assert_eq!(
        a.store
            .reactions(AB_CHAT, &edited_before.id)
            .unwrap()
            .iter()
            .map(|r| (r.key.as_str(), r.user.as_str()))
            .collect::<Vec<_>>(),
        [("heart", LAB_B)]
    );
    assert_eq!(a.messages(ABC_CHAT).len(), 1);
    assert_eq!(a.store.unread_count(ABC_CHAT, LAB_A).unwrap(), 1);

    // caught up: the next one brings nothing
    let again = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert_eq!((again.conversations, again.messages.changed()), (0, 0));
    assert_eq!(a.messages(AB_CHAT).len(), 7);
}

#[tokio::test]
async fn a_conversation_that_fails_to_sync_is_synced_again_by_every_later_catch_up() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut a = Account::of(&mock, LAB_A);
    a.catch_up.catch_up(&mut a.store).await.unwrap();
    let list_link = a.store.conversation_list_sync_link().unwrap();
    Writer::of(&mock, LAB_B).post(AB_CHAT, "missed").await;

    // the chat's sync link is one the service doesn't take (400): its sync fails, the chat is kept to be synced
    // again, and the list's link moves on past it
    let mut state = a.store.sync_state(AB_CHAT).unwrap().unwrap();
    let good = state.sync_link.replace(format!(
        "{CHAT_BASE}/v1/users/ME/conversations/{}/messages?syncState=not-the-mocks",
        encoded(AB_CHAT)
    ));
    a.store.set_sync_state(&state).unwrap();
    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].conversation, AB_CHAT);
    assert!(
        matches!(report.failed[0].error, CatchUpError::Read(_)),
        "{:?}",
        report.failed[0].error
    );
    assert!(
        report.failed[0].is_refused() && report.caught_up(),
        "a refusal keeps no catch-up due"
    );
    assert_ne!(a.store.conversation_list_sync_link().unwrap(), list_link);
    assert_eq!(a.store.unsynced_conversations().unwrap(), [AB_CHAT]);
    assert_eq!(a.messages(AB_CHAT).len(), 5);

    // the next catch-up syncs it again although the list doesn't bring it; still refused, it stays kept
    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert_eq!(report.conversations, 0);
    assert_eq!(report.failed.len(), 1);
    assert_eq!(a.store.unsynced_conversations().unwrap(), [AB_CHAT]);

    // with the link back, the next catch-up brings the message and keeps it no longer
    state.sync_link = good;
    a.store.set_sync_state(&state).unwrap();
    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert!(report.failed.is_empty());
    assert_eq!(report.messages.inserted, 1);
    assert_eq!(a.messages(AB_CHAT).len(), 6);
    assert!(a.store.unsynced_conversations().unwrap().is_empty());
}

#[tokio::test]
async fn a_conversation_whose_sync_fails_keeps_one_due_and_is_synced_again_with_its_members() {
    let server = MockServer::start().await;
    let mut renamed = listed(CHAT, 2);
    renamed["threadProperties"]["topic"] = json!("Synthetic renamed");
    mount_list(&server, vec![renamed], "l1").await;
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path_regex("/messages$"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let mut a = plain_account(&server);
    let mut gaps = Gaps::new();
    gaps.connected();
    let report = a
        .catch_up
        .catch_up_if_due(&mut a.store, &mut gaps)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.failed.len(), 1);
    assert!(!report.caught_up() && gaps.is_due(), "a 503 may pass: one stays due");
    assert_eq!(a.store.conversation_list_sync_link().unwrap(), Some(list_link("l1")));
    assert_eq!(a.store.unsynced_conversations().unwrap(), [CHAT]);
    // what the list said of it is kept all the same
    let row = a.store.conversation(CHAT).unwrap().unwrap();
    assert_eq!((row.version, row.topic.as_deref()), (2, Some("Synthetic renamed")));
    assert_eq!(
        a.store.roster_version(CHAT).unwrap(),
        None,
        "its thread read failed too"
    );

    // the service is back; the list brings nothing more, the kept chat is synced, its members read as a thread
    server.reset().await;
    mount_list(&server, vec![], "l2").await;
    mount_pages(
        &server,
        vec![("", page(vec![entry(1001, json!("1001"))], Some("s1"), None))],
    )
    .await;
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(format!("/api/chatsvc/amer/v1/threads/{CHAT}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": CHAT,
            "rosterVersion": 7,
            "members": [{"id": LAB_A, "role": "Admin"}, {"id": LAB_B, "role": "User"}],
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    let report = a
        .catch_up
        .catch_up_if_due(&mut a.store, &mut gaps)
        .await
        .unwrap()
        .unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    assert!(!gaps.is_due());
    assert!(a.store.unsynced_conversations().unwrap().is_empty());
    assert_eq!(stored_ids(&a), ["1001"]);
    assert_eq!(a.store.roster_version(CHAT).unwrap(), Some(7));
}

#[tokio::test]
async fn the_drafts_are_never_read_and_hold_nothing() {
    let server = MockServer::start().await;
    mount_list(&server, vec![listed(DRAFTS, 2)], "l1").await;
    let mut a = plain_account(&server);
    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    assert_eq!(a.store.conversation_list_sync_link().unwrap(), Some(list_link("l1")));
    let reads: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    assert!(reads.iter().all(|path| !path.contains("48:drafts")), "{reads:?}");
}

// --- deliveries and the sync ---

#[tokio::test]
async fn trouter_deliveries_and_the_sync_agree() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut b = Account::of(&mock, LAB_B);
    b.catch_up.catch_up(&mut b.store).await.unwrap();

    let a = Writer::of(&mock, LAB_A);
    let id = a.post(AB_CHAT, "hi b").await;
    a.edit(AB_CHAT, &id, "hi b, edited").await;
    a.react(AB_CHAT, &id, "like").await;
    let events = mock.take_events(LAB_B);
    let kinds: Vec<EventKind> = events.iter().map(|event| event.kind).collect();
    assert_eq!(kinds, [EventKind::Message, EventKind::Edit, EventKind::Reaction]);

    // delivered first, synced after: each version once, the sync changes nothing
    assert_eq!(
        b.deliver(&events),
        [Applied::Inserted, Applied::Updated, Applied::Updated]
    );
    let report = b.catch_up.catch_up(&mut b.store).await.unwrap();
    assert_eq!((report.messages.changed(), report.messages.unchanged), (0, 1));
    assert_eq!(b.messages(AB_CHAT).len(), 6);
    let stored = b.message(AB_CHAT, &id);
    assert_eq!(stored.content, "<p>hi b, edited</p>");
    assert_eq!(b.store.reactions(AB_CHAT, &id).unwrap().len(), 1);

    // a late delivery of an older version changes nothing
    assert_eq!(b.deliver(&events[..2]), [Applied::Ignored, Applied::Ignored]);
    assert_eq!(b.message(AB_CHAT, &id), stored);

    // synced first, delivered after: the same rows
    let second = a.post(AB_CHAT, "again").await;
    let events = mock.take_events(LAB_B);
    let report = b.catch_up.catch_up(&mut b.store).await.unwrap();
    assert_eq!(report.messages.inserted, 1);
    assert_eq!(b.deliver(&events), [Applied::Ignored]);
    assert_eq!(b.messages(AB_CHAT).len(), 7);
    assert_eq!(b.message(AB_CHAT, &second).content, "<p>again</p>");
}

fn stored(conversation: &str, id: &str, applied: Applied) -> StoredMessage {
    StoredMessage {
        conversation: conversation.to_owned(),
        id: id.to_owned(),
        applied,
    }
}

#[tokio::test]
async fn the_reads_tell_what_they_changed_once_and_never_what_the_store_had() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut b = Account::of(&mock, LAB_B);
    let report = b.catch_up.catch_up(&mut b.store).await.unwrap();
    let changes = b.catch_up.take_changes();
    assert_eq!(changes.messages.len(), report.messages.changed(), "{changes:?}");
    assert!(
        changes
            .messages
            .iter()
            .all(|message| message.applied == Applied::Inserted)
    );
    let mut conversations = changes.conversations.clone();
    conversations.sort();
    conversations.dedup();
    assert_eq!(
        conversations.len(),
        changes.conversations.len(),
        "each conversation once"
    );
    assert_eq!(
        conversations.len(),
        report.conversations,
        "every one the first catch-up added"
    );
    assert!(b.catch_up.take_changes().is_empty(), "taken once");

    // nothing changed: nothing to tell
    b.catch_up.catch_up(&mut b.store).await.unwrap();
    assert!(b.catch_up.take_changes().is_empty());

    // a new message, then it edited: each change told, its conversation with it
    let a = Writer::of(&mock, LAB_A);
    let id = a.post(AB_CHAT, "told once").await;
    b.catch_up.catch_up(&mut b.store).await.unwrap();
    let changes = b.catch_up.take_changes();
    assert_eq!(changes.messages, [stored(AB_CHAT, &id, Applied::Inserted)]);
    assert_eq!(changes.conversations, [AB_CHAT]);
    a.edit(AB_CHAT, &id, "told once, edited").await;
    b.catch_up.catch_up(&mut b.store).await.unwrap();
    assert_eq!(
        b.catch_up.take_changes().messages,
        [stored(AB_CHAT, &id, Applied::Updated)]
    );

    // delivered first: the delivery is its receiver's to tell, and the catch-up that meets it again tells nothing of it
    drop(mock.take_events(LAB_B));
    let delivered = a.post(AB_CHAT, "delivered first").await;
    b.deliver(&mock.take_events(LAB_B));
    assert!(b.catch_up.take_changes().is_empty(), "a delivery is no read's change");
    b.catch_up.catch_up(&mut b.store).await.unwrap();
    let changes = b.catch_up.take_changes();
    assert!(
        changes.messages.iter().all(|message| message.id != delivered),
        "{changes:?}"
    );
}

#[tokio::test]
async fn older_history_tells_each_message_it_stored() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut b = Account::of(&mock, LAB_B);
    b.catch_up = b.catch_up.with_page_size(2);
    b.catch_up.sync_messages(&mut b.store, AB_CHAT).await.unwrap();
    assert_eq!(b.catch_up.take_changes().messages.len(), 2);
    let older = b.catch_up.load_older(&mut b.store, AB_CHAT).await.unwrap().unwrap();
    let changes = b.catch_up.take_changes();
    assert_eq!(changes.messages.len(), older.inserted);
    assert!(older.inserted > 0);
    assert_eq!(changes.conversations, [AB_CHAT]);
}

// --- when ---

#[tokio::test]
async fn a_reconnect_triggers_a_catch_up() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut a = Account::of(&mock, LAB_A);
    let mut gaps = Gaps::new();

    // on start, before the channel is up: a catch-up, and another stays due for when it connects
    let started = a.catch_up.catch_up_if_due(&mut a.store, &mut gaps).await.unwrap();
    assert!(started.is_some() && gaps.is_due());
    gaps.connected();
    assert!(
        a.catch_up
            .catch_up_if_due(&mut a.store, &mut gaps)
            .await
            .unwrap()
            .is_some()
    );
    assert!(!gaps.is_due());

    // up, and caught up: none is due, nothing is read
    let before = mock.received_requests().await.len();
    assert!(
        a.catch_up
            .catch_up_if_due(&mut a.store, &mut gaps)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(mock.received_requests().await.len(), before);

    // the channel drops, b writes meanwhile, the channel is back: the reconnect brings b's message
    gaps.disconnected();
    let id = Writer::of(&mock, LAB_B).post(AB_CHAT, "while you were away").await;
    gaps.connected();
    let report = a
        .catch_up
        .catch_up_if_due(&mut a.store, &mut gaps)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.messages.inserted, 1);
    assert_eq!(a.message(AB_CHAT, &id).content, "<p>while you were away</p>");
    assert!(
        a.catch_up
            .catch_up_if_due(&mut a.store, &mut gaps)
            .await
            .unwrap()
            .is_none()
    );

    // a catch-up that fails keeps one due
    gaps.disconnected();
    gaps.connected();
    assert!(mock.set_token(LAB_A, "mock-ic3-token-replaced"));
    assert!(a.catch_up.catch_up_if_due(&mut a.store, &mut gaps).await.is_err());
    assert!(gaps.is_due());
}

#[tokio::test]
async fn a_conversation_the_service_refuses_keeps_no_catch_up_due_but_is_synced_by_the_next() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut a = Account::of(&mock, LAB_A);
    let mut gaps = Gaps::new();
    gaps.connected();
    a.catch_up.catch_up_if_due(&mut a.store, &mut gaps).await.unwrap();
    assert!(!gaps.is_due());

    // the channel drops, b writes, the channel is back; the chat's sync link is one the service doesn't take
    gaps.disconnected();
    let id = Writer::of(&mock, LAB_B).post(AB_CHAT, "missed").await;
    gaps.connected();
    let mut state = a.store.sync_state(AB_CHAT).unwrap().unwrap();
    let good = state.sync_link.replace(format!(
        "{CHAT_BASE}/v1/users/ME/conversations/{}/messages?syncState=not-the-mocks",
        encoded(AB_CHAT)
    ));
    a.store.set_sync_state(&state).unwrap();
    let report = a
        .catch_up
        .catch_up_if_due(&mut a.store, &mut gaps)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.failed.len(), 1);
    // a 400 won't pass by trying again soon (a 503 would keep one due: see below)
    assert!(!gaps.is_due(), "a refused conversation keeps none due");
    assert_eq!(a.store.unsynced_conversations().unwrap(), [AB_CHAT]);

    // with the link back, the next one (the service reported a loss) brings the message and clears it
    state.sync_link = good;
    a.store.set_sync_state(&state).unwrap();
    gaps.missed();
    let report = a
        .catch_up
        .catch_up_if_due(&mut a.store, &mut gaps)
        .await
        .unwrap()
        .unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(a.message(AB_CHAT, &id).content, "<p>missed</p>");
    assert!(!gaps.is_due());
}

// --- threads the account can't read ---

/// A team thread the account left, one deleted for good and one the service doesn't know; made-up ids, each told apart
/// by its last hex digits.
const LEFT: &str = "19:00000000000000000000000000000a1e@thread.tacv2";
const GONE: &str = "19:000000000000000000000000000090e0@thread.tacv2";
const MISSING: &str = "19:00000000000000000000000000000155@thread.v2";

/// The chat service's error answer, in its nested form, with `code` (its text made up).
fn service_error(status: u16, code: &str) -> Value {
    json!({
        "errorCode": status,
        "message": json!({"errorCode": status, "message": "Synthetic refusal.", "subErrorCode": code}).to_string(),
        "standardizedError": {"errorCode": status, "errorSubCode": 1, "errorSubCodeString": code},
    })
}

/// Every read of the conversation whose id ends in `hex` (its messages, its thread) answered `status` with `code`.
async fn refuse_reads(server: &MockServer, hex: &str, status: u16, code: &str) {
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path_regex(format!("{hex}(@|%40)thread")))
        .respond_with(ResponseTemplate::new(status).set_body_json(service_error(status, code)))
        .with_priority(1)
        .mount(server)
        .await;
}

/// How many reads went to the conversation whose id ends in `hex`.
async fn reads_of(server: &MockServer, hex: &str) -> usize {
    let requests = server.received_requests().await.unwrap();
    requests
        .iter()
        .filter(|request| request.url.path().contains(hex))
        .count()
}

#[tokio::test]
async fn a_thread_the_account_left_is_skipped_once_and_not_kept_to_be_synced() {
    let server = MockServer::start().await;
    mount_list(&server, vec![listed(LEFT, 2)], "l1").await;
    refuse_reads(&server, "0a1e", 403, "AclCheckFailed").await;
    let mut a = Account::on(&server.uri(), LAB_A, "placeholder-token");
    let mut gaps = Gaps::new();
    gaps.connected();
    let report = a
        .catch_up
        .catch_up_if_due(&mut a.store, &mut gaps)
        .await
        .unwrap()
        .unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    assert_eq!(
        report.unreadable,
        [teams_core::Unreadable {
            conversation: LEFT.to_owned(),
            status: 403,
            code: Some("AclCheckFailed".to_owned()),
            forgotten: false,
        }]
    );
    assert!(report.caught_up() && !gaps.is_due());
    assert!(a.store.unsynced_conversations().unwrap().is_empty(), "not kept");
    assert_eq!(a.store.conversation_list_sync_link().unwrap(), Some(list_link("l1")));
    assert!(
        a.store.conversation(LEFT).unwrap().is_some(),
        "what the list said of it stays"
    );
    let read = reads_of(&server, "0a1e").await;
    assert!(read > 0);

    // the list doesn't bring it again: no later catch-up reads it
    server.reset().await;
    mount_list(&server, vec![], "l2").await;
    refuse_reads(&server, "0a1e", 403, "AclCheckFailed").await;
    gaps.missed();
    let report = a
        .catch_up
        .catch_up_if_due(&mut a.store, &mut gaps)
        .await
        .unwrap()
        .unwrap();
    assert!(report.failed.is_empty() && report.unreadable.is_empty(), "{report:?}");
    assert_eq!(reads_of(&server, "0a1e").await, 0);
}

/// A thread whose reads the service answers 400: a refusal that may pass, kept to be synced again.
const REFUSED: &str = "19:00000000000000000000000000000bad@thread.tacv2";

/// The full list again (no sync link kept), bringing `entries`; every read of [`LEFT`] answered 403 and of [`REFUSED`]
/// 400. The report and how many reads went to each.
async fn list_again(server: &MockServer, a: &mut Account, entries: Vec<Value>) -> (CatchUpReport, usize, usize) {
    server.reset().await;
    mount_list(server, entries, "l1").await;
    refuse_reads(server, "0a1e", 403, "AclCheckFailed").await;
    refuse_reads(server, "0bad", 400, "BadRequest").await;
    a.store.set_conversation_list_sync_link(None, 0).unwrap();
    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    (report, reads_of(server, "0a1e").await, reads_of(server, "0bad").await)
}

#[tokio::test]
async fn a_full_listing_reads_a_thread_the_account_left_again_only_once_it_changed() {
    let server = MockServer::start().await;
    let mut a = Account::on(&server.uri(), LAB_A, "placeholder-token");
    let (report, left, refused) = list_again(&server, &mut a, vec![listed(LEFT, 2), listed(REFUSED, 2)]).await;
    assert_eq!(report.unreadable.len(), 1, "{report:?}");
    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert!(left > 0 && refused > 0);

    // listed as it was: the one it left isn't read again, the refused one is
    let (report, left, refused) = list_again(&server, &mut a, vec![listed(LEFT, 2), listed(REFUSED, 2)]).await;
    assert!(report.unreadable.is_empty(), "{report:?}");
    assert_eq!(report.failed.len(), 1, "a 400 is tried again: {report:?}");
    assert_eq!(left, 0, "not read again");
    assert!(refused > 0);
    assert!(
        a.store.unsynced_conversations().unwrap() == [REFUSED],
        "only the refused one is kept"
    );

    // a later last message (the account added back), its version as it was: read again
    let mut added_back = listed(LEFT, 2);
    added_back["lastMessage"] = entry(1001, json!("1001"));
    let (report, left, _) = list_again(&server, &mut a, vec![added_back.clone()]).await;
    assert_eq!(report.unreadable.len(), 1, "{report:?}");
    assert!(left > 0);
    let (_, left, _) = list_again(&server, &mut a, vec![added_back]).await;
    assert_eq!(left, 0, "as it was again");

    // a newer version: read again
    let (report, left, _) = list_again(&server, &mut a, vec![listed(LEFT, 3)]).await;
    assert_eq!(report.unreadable.len(), 1, "{report:?}");
    assert!(left > 0);
}

#[tokio::test]
async fn threads_kept_from_an_earlier_build_are_left_and_a_deleted_one_forgotten() {
    let server = MockServer::start().await;
    mount_list(&server, vec![], "l1").await;
    refuse_reads(&server, "0a1e", 403, "AclCheckFailed").await;
    refuse_reads(&server, "90e0", 404, "ThreadNotFound").await;
    refuse_reads(&server, "0155", 404, "RequestHandlerException").await;
    let mut a = Account::on(&server.uri(), LAB_A, "placeholder-token");
    // what a build before this one kept: each refused conversation to be synced again by every catch-up
    for id in [LEFT, GONE, MISSING] {
        a.store
            .upsert_conversation(&Conversation {
                id: id.to_owned(),
                kind: ConversationKind::Group,
                topic: None,
                version: 1,
                last_activity: None,
            })
            .unwrap();
        a.store.set_unsynced(id).unwrap();
    }
    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    let left: Vec<(&str, u16, Option<&str>, bool)> = report
        .unreadable
        .iter()
        .map(|unreadable| {
            (
                unreadable.conversation.as_str(),
                unreadable.status,
                unreadable.code.as_deref(),
                unreadable.forgotten,
            )
        })
        .collect();
    assert_eq!(
        left,
        [
            (MISSING, 404, Some("RequestHandlerException"), false),
            (LEFT, 403, Some("AclCheckFailed"), false),
            (GONE, 404, Some("ThreadNotFound"), true),
        ]
    );
    assert!(a.store.unsynced_conversations().unwrap().is_empty());
    assert_eq!(
        a.store.conversation(GONE).unwrap(),
        None,
        "a deleted thread is forgotten"
    );
    assert!(a.store.conversation(LEFT).unwrap().is_some());
    assert!(
        a.store.conversation(MISSING).unwrap().is_some(),
        "only a thread named gone is forgotten"
    );
    assert_eq!(
        a.catch_up.take_changes().conversations,
        [GONE],
        "the forgotten one is told, so a view drops it"
    );

    // none of them is read again
    let before: usize = [
        reads_of(&server, "0a1e").await,
        reads_of(&server, "90e0").await,
        reads_of(&server, "0155").await,
    ]
    .iter()
    .sum();
    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert!(report.failed.is_empty() && report.unreadable.is_empty(), "{report:?}");
    let after: usize = [
        reads_of(&server, "0a1e").await,
        reads_of(&server, "90e0").await,
        reads_of(&server, "0155").await,
    ]
    .iter()
    .sum();
    assert_eq!(after, before);
}

/// The full list again (no sync link kept), bringing `entry`, a listing of [`GONE`], whose every read is answered 404
/// `ThreadNotFound`. The report and how many reads went to it.
async fn list_gone_again(server: &MockServer, a: &mut Account, entry: Value) -> (CatchUpReport, usize) {
    server.reset().await;
    mount_list(server, vec![entry], "l1").await;
    refuse_reads(server, "90e0", 404, "ThreadNotFound").await;
    a.store.set_conversation_list_sync_link(None, 0).unwrap();
    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    (report, reads_of(server, "90e0").await)
}

#[tokio::test]
async fn a_full_listing_reads_a_deleted_thread_again_only_once_it_changed() {
    let server = MockServer::start().await;
    let mut a = Account::on(&server.uri(), LAB_A, "placeholder-token");
    let (report, read) = list_gone_again(&server, &mut a, listed(GONE, 2)).await;
    assert!(
        report.unreadable.len() == 1 && report.unreadable[0].forgotten,
        "{report:?}"
    );
    assert!(read > 0);
    assert_eq!(a.store.conversation(GONE).unwrap(), None, "forgotten");
    a.catch_up.take_changes();

    // listed as it was, again and again: neither added back nor read
    for _ in 0..2 {
        let (report, read) = list_gone_again(&server, &mut a, listed(GONE, 2)).await;
        assert!(report.unreadable.is_empty() && report.failed.is_empty(), "{report:?}");
        assert_eq!(read, 0, "not read again");
        assert_eq!(a.store.conversation(GONE).unwrap(), None, "not added back");
        assert!(a.catch_up.take_changes().is_empty(), "no view is told of it");
    }

    // a later last message, its version as it was: read again, and forgotten again at it
    let mut later = listed(GONE, 2);
    later["lastMessage"] = entry(1001, json!("1001"));
    let (report, read) = list_gone_again(&server, &mut a, later.clone()).await;
    assert!(
        report.unreadable.len() == 1 && report.unreadable[0].forgotten,
        "{report:?}"
    );
    assert!(read > 0);
    assert_eq!(a.store.conversation(GONE).unwrap(), None);
    let (_, read) = list_gone_again(&server, &mut a, later).await;
    assert_eq!(read, 0, "as it was again");

    // a newer version: read again
    let (report, read) = list_gone_again(&server, &mut a, listed(GONE, 3)).await;
    assert_eq!(report.unreadable.len(), 1, "{report:?}");
    assert!(read > 0);
    let (_, read) = list_gone_again(&server, &mut a, listed(GONE, 3)).await;
    assert_eq!(read, 0, "as it was again");
}

#[tokio::test]
async fn a_deleted_thread_that_reads_again_is_synced_as_any_other() {
    let server = MockServer::start().await;
    let mut a = Account::on(&server.uri(), LAB_A, "placeholder-token");
    list_gone_again(&server, &mut a, listed(GONE, 2)).await;
    assert_eq!(a.store.conversation(GONE).unwrap(), None);

    // recreated: the list shows it newer and its reads answer
    server.reset().await;
    mount_list(&server, vec![listed(GONE, 3)], "l2").await;
    mount_pages(&server, vec![("", page(vec![], Some("s1"), None))]).await;
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(format!("/api/chatsvc/amer/v1/threads/{GONE}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": GONE, "members": []})))
        .with_priority(1)
        .mount(&server)
        .await;
    a.store.set_conversation_list_sync_link(None, 0).unwrap();
    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert!(report.unreadable.is_empty() && report.failed.is_empty(), "{report:?}");
    assert_eq!(
        a.store.conversation(GONE).unwrap().map(|stored| stored.version),
        Some(3)
    );
    assert_eq!(
        a.store.forgotten_thread(GONE).unwrap(),
        None,
        "stored again: no longer forgotten"
    );
}

#[tokio::test]
async fn a_write_waiting_on_a_deleted_thread_is_told_failed_and_never_sent() {
    let server = MockServer::start().await;
    mount_list(&server, vec![listed(GONE, 2)], "l1").await;
    refuse_reads(&server, "90e0", 404, "ThreadNotFound").await;
    let mut a = Account::on(&server.uri(), LAB_A, "placeholder-token");
    a.store
        .upsert_conversation(&Conversation {
            id: GONE.to_owned(),
            kind: ConversationKind::Group,
            topic: None,
            version: 1,
            last_activity: None,
        })
        .unwrap();
    let row = enqueue(&mut a.store, &send(GONE, "never goes")).unwrap();

    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert!(
        report.unreadable.len() == 1 && report.unreadable[0].forgotten,
        "{report:?}"
    );
    assert_eq!(a.store.conversation(GONE).unwrap(), None);
    let told = a.outbox.take_arrived();
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(
        (told[0].id, told[0].state),
        (row.id, OutboxState::Failed),
        "its owner stops showing it as sending"
    );
    assert_eq!(a.store.outbox_entry(row.id).unwrap(), None, "gone with the thread");
    assert!(a.store.outbox_left_by_earlier_runs().unwrap().is_empty());

    let due = a.outbox.send_due(&mut a.store, &a.catch_up).await.unwrap();
    assert!(due.rows.is_empty(), "nothing goes out: {due:?}");
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests.iter().all(|request| request.method.as_str() == "GET"),
        "no write went out"
    );
}

// --- the outbox's check ---

#[tokio::test]
async fn the_outbox_check_finds_a_message_whose_answer_was_lost() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut a = Account::of(&mock, LAB_A);
    a.catch_up.catch_up(&mut a.store).await.unwrap();

    a.fault(Fault::LoseAnswer);
    let row = enqueue(&mut a.store, &send(AB_CHAT, "did it arrive")).unwrap();
    let sent = a.outbox.send(&mut a.store, &a.catch_up, row.id).await.unwrap();
    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(
        posts(&mock).await,
        std::slice::from_ref(&row.client_message_id),
        "posted once"
    );
    // found by the sync, stored once, matched to the row
    let found = a
        .store
        .message_by_client_id(AB_CHAT, &row.client_message_id)
        .unwrap()
        .unwrap();
    assert_eq!(sent.message_id.as_deref(), Some(found.id.as_str()));
    assert_eq!(found.sender.as_deref(), Some(LAB_A));
    assert_eq!(a.messages(AB_CHAT).len(), 6);
}

#[tokio::test]
async fn the_outbox_check_finds_a_thread_reply_whose_answer_was_lost_in_its_channel() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut a = Account::of(&mock, LAB_A);
    let post = mock.post_as(LAB_B, TEAM_CHANNEL, "<p>a post</p>").unwrap();
    a.catch_up.sync_messages(&mut a.store, TEAM_CHANNEL).await.unwrap();

    // a reply posted to the post's chain comes back in the channel's messages, never the chain's
    let chain = teams_proto::reply_chain_id(TEAM_CHANNEL, &post).unwrap();
    a.fault(Fault::LoseAnswer);
    let row = enqueue(&mut a.store, &send(&chain, "did my reply arrive")).unwrap();
    let sent = a.outbox.send(&mut a.store, &a.catch_up, row.id).await.unwrap();
    assert_eq!(sent.state, OutboxState::Sent, "{sent:?}");
    assert_eq!(
        posts(&mock).await,
        std::slice::from_ref(&row.client_message_id),
        "posted once"
    );
    let found = a
        .store
        .message_by_client_id(TEAM_CHANNEL, &row.client_message_id)
        .unwrap()
        .unwrap();
    assert_eq!(sent.message_id.as_deref(), Some(found.id.as_str()));
    let roots = a.store.reply_roots(TEAM_CHANNEL).unwrap();
    assert!(roots.contains(&(found.id.clone(), post.clone())), "{roots:?}");
    assert!(
        a.store
            .message_by_client_id(&chain, &row.client_message_id)
            .unwrap()
            .is_none(),
        "nothing kept under the chain"
    );
}

#[tokio::test]
async fn the_outbox_check_sends_again_with_the_same_id_when_the_message_did_not_arrive() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut a = Account::of(&mock, LAB_A);
    a.catch_up.catch_up(&mut a.store).await.unwrap();

    a.fault(Fault::DropRequest);
    let row = enqueue(&mut a.store, &send(AB_CHAT, "second try")).unwrap();
    let sent = a.outbox.send(&mut a.store, &a.catch_up, row.id).await.unwrap();
    assert_eq!((sent.state, sent.attempts), (OutboxState::Sent, 2));
    assert_eq!(
        posts(&mock).await,
        std::slice::from_ref(&row.client_message_id),
        "the second attempt only"
    );

    // the post's answer and the sync agree: one message
    a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert_eq!(a.messages(AB_CHAT).len(), 6);
    let stored = a
        .store
        .message_by_client_id(AB_CHAT, &row.client_message_id)
        .unwrap()
        .unwrap();
    assert_eq!(sent.message_id.as_deref(), Some(stored.id.as_str()));
}

#[tokio::test]
async fn the_check_reads_the_service_and_a_failed_read_is_an_error() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut a = Account::of(&mock, LAB_A);
    a.catch_up.catch_up(&mut a.store).await.unwrap();
    let before = mock.received_requests().await.len();
    a.catch_up.sync_conversation(&mut a.store, AB_CHAT).await.unwrap();
    assert_eq!(
        reads_since(&mock, before).await.len(),
        1,
        "read from the service, not the store"
    );
    assert!(mock.set_token(LAB_A, "mock-ic3-token-replaced"));
    assert!(a.catch_up.sync_conversation(&mut a.store, AB_CHAT).await.is_err());
}

/// The `clientmessageid` of every message post the mock got, in order.
async fn posts(mock: &ChatServiceMock) -> Vec<String> {
    mock.received_requests()
        .await
        .iter()
        .filter(|request| request.method.as_str() == "POST" && request.url.path().ends_with("/messages"))
        .map(|request| {
            let body: Value = request.body_json().unwrap();
            body["clientmessageid"].as_str().unwrap_or_default().to_owned()
        })
        .collect()
}

// --- shapes the mock doesn't serve, on plain wiremock ---

const CHAT: &str = "19:00000000000000000000000000000def@thread.v2";

/// A plain service whose message reads answer by their `syncState`: none for the first read, else the token.
async fn scripted(pages: Vec<(&str, Value)>) -> MockServer {
    let server = MockServer::start().await;
    mount_pages(&server, pages).await;
    server
}

/// Answers the plain service's message reads by their `syncState`, as [`scripted`] does.
async fn mount_pages(server: &MockServer, pages: Vec<(&str, Value)>) {
    let pages: Vec<(String, Value)> = pages
        .into_iter()
        .map(|(token, page)| (token.to_owned(), page))
        .collect();
    Mock::given(wiremock::matchers::method("GET"))
        .respond_with(move |request: &Request| {
            let token = request
                .url
                .query_pairs()
                .find(|(name, _)| name == "syncState")
                .map(|(_, value)| value.into_owned())
                .unwrap_or_default();
            pages.iter().find(|(expected, _)| *expected == token).map_or_else(
                || ResponseTemplate::new(404),
                |(_, page)| ResponseTemplate::new(200).set_body_json(page),
            )
        })
        .mount(server)
        .await;
}

fn link(token: &str) -> String {
    format!(
        "{CHAT_BASE}/v1/users/ME/conversations/{}/messages?syncState={token}&view=msnp24Equivalent",
        encoded(CHAT)
    )
}

fn entry(id: u64, version: Value) -> Value {
    let mut entry = json!({
        "id": id.to_string(),
        "sequenceId": id - 1000,
        "version": Value::Null,
        "conversationid": CHAT,
        "messagetype": "RichText/Html",
        "content": format!("<p>{id}</p>"),
        "from": format!("{CHAT_BASE}/v1/users/ME/contacts/{LAB_B}"),
        "originalarrivaltime": "2025-10-09T08:43:20.0000000Z",
    });
    entry["version"] = version;
    entry
}

fn page(messages: Vec<Value>, sync: Option<&str>, backward: Option<&str>) -> Value {
    let mut metadata = serde_json::Map::new();
    if let Some(token) = sync {
        metadata.insert("syncState".to_owned(), json!(link(token)));
    }
    if let Some(token) = backward {
        metadata.insert("backwardLink".to_owned(), json!(link(token)));
    }
    json!({"messages": Value::Array(messages), "_metadata": metadata})
}

fn plain_account(server: &MockServer) -> Account {
    let mut account = Account::on(&server.uri(), LAB_A, "placeholder-token");
    account.store.upsert_conversation(&plain_chat()).unwrap();
    account
}

/// The plain service's conversation list link with `token` as its `syncState`.
fn list_link(token: &str) -> String {
    format!("{CHAT_BASE}/v1/users/ME/conversations?view=msnp24Equivalent&syncState={token}&pageSize=200")
}

/// A conversation list entry: a group chat's for a `19:` id, else a stream's (no thread properties).
fn listed(id: &str, version: u64) -> Value {
    if id.starts_with("19:") {
        json!({"id": id, "version": version, "threadProperties": {"threadType": "chat"}})
    } else {
        json!({"id": id, "version": version})
    }
}

/// Answers the plain service's conversation list reads (the first one and its sync link's) with `entries`, handing
/// out the sync link `token`.
async fn mount_list(server: &MockServer, entries: Vec<Value>, token: &str) {
    let list = json!({"conversations": entries, "_metadata": {"syncState": list_link(token)}});
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/chatsvc/amer/v1/users/ME/conversations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list))
        .with_priority(1)
        .mount(server)
        .await;
}

/// The plain service's one chat, as the store keeps it.
fn plain_chat() -> Conversation {
    Conversation {
        id: CHAT.to_owned(),
        kind: ConversationKind::Group,
        topic: None,
        version: 1,
        last_activity: None,
    }
}

/// Milliseconds since the Unix epoch, as the outbox's own clock reads them.
fn now_ms() -> i64 {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    i64::try_from(since.as_millis()).unwrap()
}

#[tokio::test]
async fn a_message_whose_version_cannot_be_read_is_not_stored() {
    let mut other = entry(1004, json!("1004"));
    other["conversationid"] = json!("19:another@thread.v2");
    let mut timeless = entry(1005, json!("1005"));
    timeless["id"] = json!("not-a-time");
    timeless.as_object_mut().unwrap().remove("originalarrivaltime");
    let mut odd_reactions = entry(1006, json!("1006"));
    odd_reactions["properties"] = json!({"emotions": [{"key": "like", "users": [{"mri": LAB_C, "time": "soon"}]}]});
    let server = scripted(vec![(
        "",
        page(
            vec![
                entry(1001, json!("1001")),
                entry(1002, json!("v2")),
                entry(1003, Value::Null),
                {
                    let mut unversioned = entry(1007, Value::Null);
                    unversioned.as_object_mut().unwrap().remove("version");
                    unversioned
                },
                other,
                timeless,
                odd_reactions,
                json!({"no": "id"}),
            ],
            Some("s1"),
            None,
        ),
    )])
    .await;
    let mut a = plain_account(&server);
    let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!((synced.inserted, synced.skipped), (2, 7), "{synced:?}");
    let stored: Vec<String> = a.messages(CHAT).into_iter().map(|message| message.id).collect();
    assert_eq!(stored, ["1006", "1001"]);
    assert!(
        a.store.reactions(CHAT, "1006").unwrap().is_empty(),
        "a set it can't read is left out"
    );
}

#[tokio::test]
async fn an_unreadable_version_never_replaces_a_stored_one() {
    let server = scripted(vec![
        ("", page(vec![entry(1001, json!("5"))], Some("s1"), None)),
        ("s1", page(vec![entry(1001, json!("later, maybe"))], Some("s2"), None)),
    ])
    .await;
    let mut a = plain_account(&server);
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    let stored = a.message(CHAT, "1001");
    let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!((synced.changed(), synced.skipped), (0, 1));
    assert_eq!(a.message(CHAT, "1001"), stored);
}

#[tokio::test]
async fn a_later_last_activity_under_an_unreadable_version_is_told_as_a_change() {
    let server = scripted(vec![
        ("", page(Vec::new(), Some("s1"), None)),
        ("s1", page(Vec::new(), Some("s1"), None)),
    ])
    .await;
    // the list: the plain chat, its version unreadable, its last message later than anything stored
    let listed = json!({
        "id": CHAT,
        "version": "not a number",
        "lastMessage": entry(1001, json!("1001")),
        "threadProperties": {"threadType": "chat"},
    });
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/chatsvc/amer/v1/users/ME/conversations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"conversations": [listed]})))
        .with_priority(1)
        .mount(&server)
        .await;
    let mut a = plain_account(&server);
    a.store
        .replace_members(CHAT, 1, &[Member::new(LAB_A, None), Member::new(LAB_B, None)])
        .unwrap();

    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    let activity = a.store.conversation(CHAT).unwrap().unwrap().last_activity;
    assert!(activity.is_some(), "the later last activity is taken");
    let changes = a.catch_up.take_changes();
    assert!(changes.messages.is_empty(), "{changes:?}");
    assert_eq!(changes.conversations, [CHAT]);

    // the same list again: the stored last activity is as late, nothing to tell
    a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert!(a.catch_up.take_changes().is_empty());
}

#[tokio::test]
async fn a_listed_chats_topic_is_stored_as_its_name() {
    const MEETING: &str = "19:meeting_synthetic@thread.v2";
    const BLANK: &str = "19:synthetic-blank@thread.v2";
    let server = scripted(vec![
        ("", page(Vec::new(), Some("s1"), None)),
        ("s1", page(Vec::new(), Some("s1"), None)),
    ])
    .await;
    let listed = json!({"conversations": [
        {"id": CHAT, "version": 2, "threadProperties": {"threadType": "chat", "topic": "Synthetic group"}},
        {"id": MEETING, "version": 3, "threadProperties": {"threadType": "meeting", "topic": "Synthetic sync"}},
        {"id": BLANK, "version": 4, "threadProperties": {"threadType": "chat", "topic": "  "}},
    ]});
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/chatsvc/amer/v1/users/ME/conversations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(listed))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(format!(
            "/api/chatsvc/amer/v1/threads/{BLANK}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": BLANK, "members": []})))
        .with_priority(1)
        .mount(&server)
        .await;
    let mut a = plain_account(&server);
    a.store
        .replace_members(CHAT, 1, &[Member::new(LAB_A, None), Member::new(LAB_B, None)])
        .unwrap();

    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    let topic = |id: &str| a.store.conversation(id).unwrap().unwrap().topic;
    assert_eq!(topic(CHAT).as_deref(), Some("Synthetic group"));
    assert_eq!(
        topic(MEETING).as_deref(),
        Some("Synthetic sync"),
        "a meeting's chat by its title"
    );
    assert_eq!(topic(BLANK), None, "a blank topic names nothing");
}

/// `entry` as a control message of `messagetype` (typing and its end come with no content).
fn control(mut entry: Value, messagetype: &str) -> Value {
    entry["messagetype"] = json!(messagetype);
    entry["content"] = json!("");
    entry
}

#[tokio::test]
async fn control_messages_on_a_sync_page_are_signals_never_stored() {
    let server = scripted(vec![(
        "",
        page(
            vec![
                control(entry(1003, json!("1003")), "Control/ClearTyping"),
                entry(1002, json!("1002")),
                control(entry(1001, json!("1001")), "Control/Typing"),
            ],
            Some("s1"),
            None,
        ),
    )])
    .await;
    let mut a = plain_account(&server);
    let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!(
        (synced.inserted, synced.unchanged, synced.skipped, synced.control),
        (1, 0, 0, 2),
        "{synced:?}"
    );
    assert_eq!(stored_ids(&a), ["1002"]);
    assert!(
        a.store.message(CHAT, "1001").unwrap().is_none(),
        "typing isn't a message"
    );
    assert!(a.store.message(CHAT, "1003").unwrap().is_none(), "its end isn't either");
}

/// The `syncState` of every message read the plain service got, in order.
async fn tokens_read(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter_map(|request| {
            request
                .url
                .query_pairs()
                .find(|(name, _)| name == "syncState")
                .map(|(_, value)| value.into_owned())
        })
        .collect()
}

/// Stores entries as Trouter deliveries of them would.
fn deliver_entries(account: &mut Account, entries: &[Value]) {
    for entry in entries {
        let message: chat::Message = serde_json::from_value(entry.clone()).unwrap();
        let received = account
            .catch_up
            .receive_message(&mut account.store, CHAT, &message)
            .unwrap();
        assert_eq!(received.unwrap().applied, Applied::Inserted);
    }
}

fn stored_ids(account: &Account) -> Vec<String> {
    account.messages(CHAT).into_iter().map(|message| message.id).collect()
}

#[tokio::test]
async fn a_sync_answers_backward_link_is_followed_to_its_end() {
    let known = entry(1001, json!("1001"));
    let server = scripted(vec![
        ("", page(vec![known.clone()], Some("s1"), None)),
        // what changed: more than a page, the older part behind a backward link
        (
            "s1",
            page(
                vec![entry(1005, json!("1005")), entry(1004, json!("1004"))],
                Some("s2"),
                Some("b1"),
            ),
        ),
        (
            "b1",
            page(vec![entry(1003, json!("1003")), known.clone()], Some("s2"), Some("b2")),
        ),
        // a page that changes nothing doesn't end the walk: the one behind it has a message the store never had
        ("b2", page(vec![known.clone()], Some("s2"), Some("b3"))),
        ("b3", page(vec![entry(1002, json!("1002"))], Some("s2"), None)),
    ])
    .await;
    let mut a = plain_account(&server);
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!((synced.inserted, synced.unchanged, synced.pages), (4, 2, 4));
    assert_eq!(tokens_read(&server).await, ["s1", "b1", "b2", "b3"]);
    assert_eq!(stored_ids(&a), ["1005", "1004", "1003", "1002", "1001"]);
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert_eq!(state.sync_link, Some(link("s2")));
    assert!(state.history_complete && state.backward_link.is_none());
}

#[tokio::test]
async fn a_sync_answer_whose_newest_page_came_as_deliveries_still_reads_what_is_behind_it() {
    // the socket dropped, 1002 and 1003 were missed; 1004 and 1005 came as deliveries before the catch-up ran
    let newest = vec![entry(1005, json!("1005")), entry(1004, json!("1004"))];
    let server = scripted(vec![
        ("", page(vec![entry(1001, json!("1001"))], Some("s1"), None)),
        ("s1", page(newest.clone(), Some("s2"), Some("b1"))),
        (
            "b1",
            page(
                vec![entry(1003, json!("1003")), entry(1002, json!("1002"))],
                Some("s2"),
                None,
            ),
        ),
    ])
    .await;
    let mut a = plain_account(&server);
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    deliver_entries(&mut a, &newest);
    let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!((synced.inserted, synced.unchanged), (2, 2));
    assert_eq!(stored_ids(&a), ["1005", "1004", "1003", "1002", "1001"]);
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert_eq!(state.sync_link, Some(link("s2")));
    assert!(state.history_complete && state.backward_link.is_none());
}

#[tokio::test]
async fn a_middle_page_that_changes_nothing_does_not_end_a_sync_answers_walk() {
    let delivered = vec![entry(1005, json!("1005")), entry(1004, json!("1004"))];
    let server = scripted(vec![
        ("", page(vec![entry(1001, json!("1001"))], Some("s1"), None)),
        ("s1", page(vec![entry(1006, json!("1006"))], Some("s2"), Some("b1"))),
        ("b1", page(delivered.clone(), Some("s2"), Some("b2"))),
        ("b2", page(vec![entry(1003, json!("1003"))], Some("s2"), None)),
    ])
    .await;
    let mut a = plain_account(&server);
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    deliver_entries(&mut a, &delivered);
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!(stored_ids(&a), ["1006", "1005", "1004", "1003", "1001"]);
}

#[tokio::test]
async fn a_sync_whose_older_page_failed_reads_it_on_the_retry() {
    let server = scripted(vec![
        ("", page(vec![entry(1001, json!("1001"))], Some("s1"), None)),
        ("s1", page(vec![entry(1005, json!("1005"))], Some("s2"), Some("b1"))),
        ("b1", page(vec![entry(1004, json!("1004"))], Some("s2"), None)),
    ])
    .await;
    // the first read of b1 fails
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::query_param("syncState", "b1"))
        .respond_with(ResponseTemplate::new(404))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    let mut a = plain_account(&server);
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert!(a.catch_up.sync_messages(&mut a.store, CHAT).await.is_err());
    // the first page was stored, the sync link stayed: the retry's first page changes nothing, b1 is read all the same
    assert_eq!(stored_ids(&a), ["1005", "1001"]);
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert_eq!(state.sync_link, Some(link("s1")));
    let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!((synced.inserted, synced.unchanged), (1, 1));
    assert_eq!(stored_ids(&a), ["1005", "1004", "1001"]);
    assert_eq!(a.store.sync_state(CHAT).unwrap().unwrap().sync_link, Some(link("s2")));
}

#[tokio::test]
async fn a_backward_walk_past_the_page_cap_keeps_the_rest_as_a_gap() {
    let tokens: Vec<String> = (0..=MAX_PAGES).map(|n| format!("w{n}")).collect();
    let mut pages = vec![
        ("", page(vec![], Some("s1"), None)),
        (
            "s1",
            page(vec![entry(5000, json!("5000"))], Some("s2"), Some(&tokens[0])),
        ),
    ];
    for (n, token) in tokens.iter().enumerate() {
        let id = 4000 + u64::try_from(n).unwrap();
        let older = tokens.get(n + 1).map_or("end", String::as_str);
        pages.push((
            token,
            page(vec![entry(id, json!(id.to_string()))], Some("s2"), Some(older)),
        ));
    }
    let server = scripted(pages).await;
    let mut a = plain_account(&server);
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!(synced.pages, MAX_PAGES);
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert_eq!(state.sync_link, Some(link("s2")));
    assert_eq!(state.gap_link, Some(link(&tokens[MAX_PAGES - 1])));
    assert_eq!(state.backward_link, None, "the first read had no older history");
    assert!(!state.history_complete);
    // the rest comes first when older pages are read
    let older = a.catch_up.load_older(&mut a.store, CHAT).await.unwrap().unwrap();
    assert_eq!(older.inserted, 1);
}

// --- a sync answer read back only to where the sync before it ended, in the captured shape ---

/// A made-up time (ms) the bounded walks' messages arrive after.
const BUSY: u64 = 1_790_000_000_000;

/// A message that arrived `offset` ms after [`BUSY`]: its id is its arrival time, and its version.
fn post(offset: u64) -> Value {
    entry_at(BUSY + offset)
}

/// A page as the service gives a sync answer's: `_metadata` with its complete segment, from the oldest message's id
/// on the page to `end` (the newest version the conversation is complete to).
fn segment_page(messages: Vec<Value>, sync: Option<&str>, backward: Option<&str>, end: u64) -> Value {
    let start = messages
        .iter()
        .filter_map(|message| message["id"].as_str()?.parse::<u64>().ok())
        .min()
        .unwrap_or(end);
    let mut page = page(messages, sync, backward);
    page["_metadata"]["lastCompleteSegmentStartTime"] = json!(start);
    page["_metadata"]["lastCompleteSegmentEndTime"] = json!(end);
    page
}

/// A busy channel's history behind `first`: `count` pages of posts from before [`BUSY`], each with a link to the
/// next, the last one's leading to `"history-end"`.
fn deep_history(first: &str, count: usize, end: u64) -> Vec<(String, Value)> {
    let tokens: Vec<String> = std::iter::once(first.to_owned())
        .chain((1..count).map(|n| format!("{first}{n}")))
        .collect();
    tokens
        .iter()
        .enumerate()
        .map(|(n, token)| {
            let newest = BUSY - 1_000 * (u64::try_from(n).unwrap() + 1);
            let older = tokens.get(n + 1).map_or("history-end", String::as_str);
            let entries = vec![entry_at(newest), entry_at(newest - 1)];
            (token.clone(), segment_page(entries, Some("s2"), Some(older), end))
        })
        .collect()
}

#[tokio::test]
async fn a_busy_channels_sync_answer_is_read_back_only_to_where_the_last_sync_ended() {
    // the first read: the newest page, complete to +300, with older history behind h1
    let mut pages = vec![
        (
            String::new(),
            segment_page(vec![post(300), post(200)], Some("s1"), Some("h1"), BUSY + 300),
        ),
        (
            "h1".to_owned(),
            segment_page(vec![post(150)], Some("s1"), None, BUSY + 300),
        ),
        // what changed since: more than a page, its last page reaching back past +300, and then the channel's whole
        // history behind it, as a busy channel's answer pages on
        (
            "s1".to_owned(),
            segment_page(vec![post(500), post(400)], Some("s2"), Some("w0"), BUSY + 500),
        ),
        (
            "w0".to_owned(),
            segment_page(vec![post(350), post(300), post(200)], Some("s2"), Some("d"), BUSY + 500),
        ),
        ("s2".to_owned(), segment_page(vec![], Some("s3"), None, BUSY + 500)),
    ];
    pages.extend(deep_history("d", MAX_PAGES + 5, BUSY + 500));
    let server = scripted(
        pages
            .iter()
            .map(|(token, page)| (token.as_str(), page.clone()))
            .collect(),
    )
    .await;
    let mut a = plain_account(&server);
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!(
        a.store.sync_state(CHAT).unwrap().unwrap().sync_until,
        Some(i64::try_from(BUSY + 300).unwrap())
    );

    let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!((synced.inserted, synced.pages), (3, 2), "s1 and w0 only");
    assert_eq!(
        tokens_read(&server).await,
        ["s1", "w0"],
        "the history behind w0 isn't read"
    );
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert_eq!(state.sync_link, Some(link("s2")), "the sync link moved on");
    assert_eq!(state.sync_until, Some(i64::try_from(BUSY + 500).unwrap()));
    assert_eq!((state.gap_link, state.gap_until), (None, None));
    assert_eq!(
        state.backward_link,
        Some(link("h1")),
        "older history stays the first read's"
    );

    // the next sync reads its one page; older history is read as the user scrolls
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!(tokens_read(&server).await, ["s1", "w0", "s2"]);
    let older = a.catch_up.load_older(&mut a.store, CHAT).await.unwrap().unwrap();
    assert_eq!(older.inserted, 1);
    assert!(a.store.message(CHAT, &(BUSY + 150).to_string()).unwrap().is_some());
    assert!(a.store.sync_state(CHAT).unwrap().unwrap().history_complete);
}

#[tokio::test]
async fn a_gap_longer_than_a_page_is_still_read_through_to_where_the_last_sync_ended() {
    // the socket dropped after +300; +400 to +900 were missed, +800 and +900 came as deliveries before the catch-up
    let delivered = vec![post(900), post(800)];
    let server = scripted(vec![
        ("", segment_page(vec![post(300)], Some("s1"), None, BUSY + 300)),
        (
            "s1",
            segment_page(delivered.clone(), Some("s2"), Some("g1"), BUSY + 900),
        ),
        (
            "g1",
            segment_page(vec![post(700), post(600)], Some("s2"), Some("g2"), BUSY + 900),
        ),
        (
            "g2",
            segment_page(vec![post(500), post(400)], Some("s2"), Some("g3"), BUSY + 900),
        ),
        (
            "g3",
            segment_page(
                vec![post(350), post(300), post(250)],
                Some("s2"),
                Some("g4"),
                BUSY + 900,
            ),
        ),
        ("g4", segment_page(vec![post(100)], Some("s2"), None, BUSY + 900)),
    ])
    .await;
    let mut a = plain_account(&server);
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    deliver_entries(&mut a, &delivered);
    let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    // the delivered page changes nothing and doesn't end the walk; g3 reaches +300 and ends it
    assert_eq!(tokens_read(&server).await, ["s1", "g1", "g2", "g3"]);
    assert_eq!((synced.inserted, synced.unchanged), (6, 3));
    let expected: Vec<String> = [900, 800, 700, 600, 500, 400, 350, 300, 250]
        .map(|offset| (BUSY + offset).to_string())
        .into();
    assert_eq!(stored_ids(&a), expected);
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert_eq!(state.sync_link, Some(link("s2")));
    assert_eq!(state.gap_link, None);
}

#[tokio::test]
async fn a_gap_kept_at_the_page_cap_is_read_on_only_to_where_the_sync_before_it_ended() {
    let walk: Vec<String> = (0..=MAX_PAGES).map(|n| format!("w{n}")).collect();
    let mut pages = vec![
        (
            String::new(),
            segment_page(vec![post(100)], Some("s1"), None, BUSY + 100),
        ),
        (
            "s1".to_owned(),
            segment_page(vec![post(100_000)], Some("s2"), Some(&walk[0]), BUSY + 100_000),
        ),
        ("s2".to_owned(), segment_page(vec![], Some("s3"), None, BUSY + 100_000)),
    ];
    for (n, token) in walk.iter().enumerate() {
        let offset = 90_000 - 10 * u64::try_from(n).unwrap();
        let (entries, older) = match walk.get(n + 1) {
            Some(older) => (vec![post(offset)], older.as_str()),
            // the walk's last page reaches back past +100, with the channel's history behind it
            None => (vec![post(150), post(100), post(50)], "d"),
        };
        pages.push((
            token.clone(),
            segment_page(entries, Some("s2"), Some(older), BUSY + 100_000),
        ));
    }
    pages.extend(deep_history("d", 3, BUSY + 100_000));
    let server = scripted(
        pages
            .iter()
            .map(|(token, page)| (token.as_str(), page.clone()))
            .collect(),
    )
    .await;
    let mut a = plain_account(&server);
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert_eq!(state.gap_link, Some(link(&walk[MAX_PAGES - 1])));
    assert_eq!(
        state.gap_until,
        Some(i64::try_from(BUSY + 100).unwrap()),
        "the bound it was cut under"
    );
    assert_eq!(state.sync_until, Some(i64::try_from(BUSY + 100_000).unwrap()));

    // the next sync reads the gap's last two pages, then the new answer: nothing of the history behind
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    let read = tokens_read(&server).await;
    assert!(read.iter().all(|token| !token.starts_with('d')), "{read:?}");
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert_eq!((state.gap_link, state.gap_until), (None, None));
    assert_eq!(state.sync_link, Some(link("s3")));
    assert!(a.store.message(CHAT, &(BUSY + 150).to_string()).unwrap().is_some());
}

#[tokio::test]
async fn a_sync_state_kept_before_its_bound_is_read_back_to_when_it_was_written() {
    let server = scripted(vec![
        (
            "s1",
            segment_page(vec![post(500), post(400)], Some("s2"), Some("w0"), BUSY + 500),
        ),
        (
            "w0",
            segment_page(vec![post(300), post(200)], Some("s2"), Some("d"), BUSY + 500),
        ),
        // the gap a sync of an older build kept: messages missed, then what the sync before it stored, then history
        (
            "g0",
            segment_page(vec![post(280), post(270)], Some("s1"), Some("g1"), BUSY + 500),
        ),
        (
            "g1",
            segment_page(vec![post(240), post(230)], Some("s1"), Some("d"), BUSY + 500),
        ),
        ("d", segment_page(vec![post(10)], Some("s2"), Some("d1"), BUSY + 500)),
    ])
    .await;
    // the sync answer's first read fails, after the gap was read
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::query_param("syncState", "s1"))
        .respond_with(ResponseTemplate::new(404))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    let mut a = plain_account(&server);
    deliver_entries(&mut a, &[post(240), post(230)]);
    // written at +300 by our clock, before this build kept how far back its answers go
    let written = i64::try_from(BUSY + 300).unwrap() + CLOCK_SKEW_MS;
    a.store
        .set_sync_state(&SyncState {
            conversation_id: CHAT.to_owned(),
            sync_link: Some(link("s1")),
            backward_link: None,
            backward_oldest: None,
            sync_until: None,
            gap_link: Some(link("g0")),
            gap_until: None,
            history_complete: false,
            updated: written,
        })
        .unwrap();
    // the gap is read until a page the store had all of, the answer's read fails
    assert!(a.catch_up.sync_messages(&mut a.store, CHAT).await.is_err());
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert_eq!(state.gap_link, None);
    assert!(a.store.message(CHAT, &(BUSY + 270).to_string()).unwrap().is_some());
    // the bound it fell back on is kept, not the time of the gap's write
    assert_eq!(state.sync_until, Some(i64::try_from(BUSY + 300).unwrap()));

    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!(tokens_read(&server).await, ["g0", "g1", "s1", "s1", "w0"]);
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert_eq!((state.gap_link, state.sync_link), (None, Some(link("s2"))));
    assert_eq!(state.sync_until, Some(i64::try_from(BUSY + 500).unwrap()));
}

// --- the outbox's check, on plain wiremock ---

/// When the check's row was made, in the tests' own made-up time (later than every other time here).
const MADE: i64 = 1_800_000_000_000;

/// The id of a message that arrived `offset` ms after the row was made: its id is its arrival time.
fn at(offset: i64) -> u64 {
    u64::try_from(MADE + offset).unwrap()
}

/// An entry without an `originalarrivaltime`, so its id (`at(…)`) is the time it arrived.
fn entry_at(id: u64) -> Value {
    let mut entry = entry(id, json!(id.to_string()));
    entry.as_object_mut().unwrap().remove("originalarrivaltime");
    entry
}

#[tokio::test]
async fn the_check_of_a_conversation_never_synced_pages_back_past_the_rows_time() {
    let server = scripted(vec![
        // the newest page: one message after the row, one a little before it by the service's clock (within the skew)
        (
            "",
            page(vec![entry_at(at(2000)), entry_at(at(-1000))], Some("s1"), Some("c1")),
        ),
        // the message looked for, its arrival before the row's `created_at` by a service clock behind ours
        ("c1", page(vec![entry_at(at(-2000))], Some("s1"), Some("c2"))),
        // older than the row by more than the skew: the check stops here
        (
            "c2",
            page(vec![entry_at(at(-CLOCK_SKEW_MS - 1))], Some("s1"), Some("c3")),
        ),
    ])
    .await;
    let mut a = plain_account(&server);
    a.catch_up
        .sync_conversation_since(&mut a.store, CHAT, MADE)
        .await
        .unwrap();
    let expected: Vec<String> = [2000, -1000, -2000, -CLOCK_SKEW_MS - 1]
        .map(|offset| at(offset).to_string())
        .into();
    assert_eq!(stored_ids(&a), expected);
    assert_eq!(tokens_read(&server).await, ["c1", "c2"], "c3 isn't read");
    // the history read is kept: older pages go on from c3
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert_eq!(state.backward_link, Some(link("c3")));

    // a plain sync of the same conversation reads only the newest page
    let server = scripted(vec![("", page(vec![entry_at(at(2000))], Some("s1"), Some("c1")))]).await;
    let mut a = plain_account(&server);
    a.catch_up.sync_conversation(&mut a.store, CHAT).await.unwrap();
    assert!(tokens_read(&server).await.is_empty());
}

#[tokio::test]
async fn a_check_that_cannot_page_back_far_enough_fails() {
    let tokens: Vec<String> = (0..=MAX_PAGES).map(|n| format!("c{n}")).collect();
    let mut pages = vec![("", page(vec![entry_at(at(1000))], Some("s1"), Some(&tokens[0])))];
    for (n, token) in tokens.iter().enumerate() {
        let older = tokens.get(n + 1).map_or("end", String::as_str);
        let id = at(-i64::try_from(n).unwrap());
        pages.push((token, page(vec![entry_at(id)], Some("s1"), Some(older))));
    }
    let server = scripted(pages).await;
    let mut a = plain_account(&server);
    let error = a
        .catch_up
        .sync_conversation_since(&mut a.store, CHAT, MADE)
        .await
        .unwrap_err();
    assert!(
        matches!(error.downcast_ref::<CatchUpError>(), Some(CatchUpError::CheckFellShort)),
        "{error:?}"
    );
    assert_eq!(tokens_read(&server).await.len(), MAX_PAGES);
}

#[tokio::test]
async fn a_check_whose_sync_answer_was_cut_at_the_page_cap_pages_back_from_the_link_it_kept() {
    let tokens: Vec<String> = (0..=MAX_PAGES).map(|n| format!("w{n}")).collect();
    let mut pages = vec![
        ("", page(vec![], Some("s1"), None)),
        ("s1", page(vec![entry_at(at(1000))], Some("s2"), Some(&tokens[0]))),
    ];
    let looked_for = at(999 - i64::try_from(MAX_PAGES).unwrap());
    for (n, token) in tokens.iter().enumerate() {
        // the answer ends at its last page: a gap is read to its end, whatever times its pages have
        let older = tokens.get(n + 1).map(String::as_str);
        let id = at(999 - i64::try_from(n).unwrap());
        let entries = if n == MAX_PAGES {
            // the message looked for, behind the cap, then one older than the row
            vec![entry_at(id), entry_at(at(-CLOCK_SKEW_MS - 1))]
        } else {
            vec![entry_at(id)]
        };
        pages.push((token, page(entries, Some("s2"), older)));
    }
    let server = scripted(pages).await;
    let mut a = plain_account(&server);
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    a.catch_up
        .sync_conversation_since(&mut a.store, CHAT, MADE)
        .await
        .unwrap();
    assert!(a.store.message(CHAT, &looked_for.to_string()).unwrap().is_some());
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert_eq!((state.gap_link, state.backward_link), (None, None));
    assert!(state.history_complete);
}

#[tokio::test]
async fn a_check_reads_on_from_the_history_a_first_read_left_unread() {
    let server = scripted(vec![
        // the start's first read: one page, newer than the row; the row's message behind its backward link
        ("", page(vec![entry_at(at(3000))], Some("s1"), Some("h1"))),
        (
            "h1",
            page(
                vec![entry_at(at(1000)), entry_at(at(-CLOCK_SKEW_MS - 1))],
                Some("s1"),
                None,
            ),
        ),
        // nothing changed since: this sync alone reads everything it was given
        ("s1", page(vec![], Some("s2"), None)),
    ])
    .await;
    let mut a = plain_account(&server);
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    a.catch_up
        .sync_conversation_since(&mut a.store, CHAT, MADE)
        .await
        .unwrap();
    assert_eq!(tokens_read(&server).await, ["s1", "h1"]);
    assert!(a.store.message(CHAT, &at(1000).to_string()).unwrap().is_some());
}

#[tokio::test]
async fn a_check_after_one_that_fell_short_does_not_send_the_row_again() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"OriginalArrivalTime": 1_800_000_000_001_u64})))
        .mount(&server)
        .await;
    let clock = Arc::new(AtomicI64::new(now_ms()));
    let mut a = Account::with_outbox_clock(&server.uri(), LAB_A, "placeholder-token", {
        let clock = Arc::clone(&clock);
        Arc::new(move || clock.load(Ordering::SeqCst))
    });
    a.store.upsert_conversation(&plain_chat()).unwrap();
    let row = enqueue(&mut a.store, &send(CHAT, "behind fifty pages")).unwrap();
    let made = row.created_at;
    let arrived = |offset: i64| entry_at(u64::try_from(made + offset).unwrap());
    let mut ours = arrived(1000);
    ours["clientmessageid"] = json!(row.client_message_id);
    ours["from"] = json!(format!("{CHAT_BASE}/v1/users/ME/contacts/{LAB_A}"));
    let tokens: Vec<String> = (0..=MAX_PAGES).map(|n| format!("c{n}")).collect();
    let mut pages = vec![
        ("", page(vec![arrived(60_000)], Some("s1"), Some(&tokens[0]))),
        ("s1", page(vec![], Some("s2"), None)),
    ];
    for (n, token) in tokens.iter().enumerate() {
        let entries = if n == MAX_PAGES {
            vec![ours.clone(), arrived(-CLOCK_SKEW_MS - 1)]
        } else {
            vec![arrived(50_000 - i64::try_from(n).unwrap())]
        };
        pages.push((token, page(entries, Some("s1"), tokens.get(n + 1).map(String::as_str))));
    }
    mount_pages(&server, pages).await;

    // the post reaches the service, its answer is lost; the check reads MAX_PAGES older pages, all newer than the row
    a.fault(Fault::LoseAnswer);
    let error = a.outbox.send(&mut a.store, &a.catch_up, row.id).await.unwrap_err();
    assert!(matches!(error, OutboxError::Sync { .. }), "{error:?}");
    // after the backoff, the next check's sync brings nothing new: the history the first one left is read on
    clock.fetch_add(3_600_000, Ordering::SeqCst);
    let sent = a.outbox.send(&mut a.store, &a.catch_up, row.id).await.unwrap();
    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(post_count(&server).await, 1, "posted once");
}

#[tokio::test]
async fn a_sync_cut_at_the_page_cap_keeps_the_older_history_apart() {
    let tokens: Vec<String> = (0..MAX_PAGES).map(|n| format!("w{n}")).collect();
    let mut pages = vec![
        ("", page(vec![entry(3000, json!("3000"))], Some("s1"), Some("h1"))),
        ("h1", page(vec![entry(2000, json!("2000"))], Some("s1"), None)),
        (
            "s1",
            page(vec![entry(5000, json!("5000"))], Some("s2"), Some(&tokens[0])),
        ),
    ];
    for (n, token) in tokens.iter().enumerate() {
        let id = 4000 + u64::try_from(n).unwrap();
        pages.push((
            token,
            page(
                vec![entry(id, json!(id.to_string()))],
                Some("s2"),
                tokens.get(n + 1).map(String::as_str),
            ),
        ));
    }
    let server = scripted(pages).await;
    let mut a = plain_account(&server);
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    // the gap's rest is read first, and reading it to its end doesn't end the older history
    let gap = a.catch_up.load_older(&mut a.store, CHAT).await.unwrap().unwrap();
    assert_eq!(gap.inserted, 1);
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert!(!state.history_complete);
    assert_eq!(state.backward_link, Some(link("h1")));
    let history = a.catch_up.load_older(&mut a.store, CHAT).await.unwrap().unwrap();
    assert_eq!(history.inserted, 1);
    assert!(a.store.message(CHAT, "2000").unwrap().is_some());
    assert!(a.store.sync_state(CHAT).unwrap().unwrap().history_complete);
    assert!(a.catch_up.load_older(&mut a.store, CHAT).await.unwrap().is_none());
}

#[tokio::test]
async fn a_second_cut_sync_answer_waits_for_the_first_gap_to_be_read() {
    let gap: Vec<String> = (0..2 * MAX_PAGES + 10).map(|n| format!("w{n}")).collect();
    let next: Vec<String> = (0..MAX_PAGES).map(|n| format!("x{n}")).collect();
    let mut pages = vec![
        ("", page(vec![entry(1001, json!("1001"))], Some("s1"), None)),
        ("s1", page(vec![entry(5000, json!("5000"))], Some("s2"), Some(&gap[0]))),
        ("s2", page(vec![entry(6000, json!("6000"))], Some("s3"), Some(&next[0]))),
    ];
    for (chain, first) in [(&gap, 10_000), (&next, 20_000)] {
        for (n, token) in chain.iter().enumerate() {
            let id = first + u64::try_from(n).unwrap();
            let older = chain.get(n + 1).map(String::as_str);
            pages.push((token, page(vec![entry(id, json!(id.to_string()))], Some("s3"), older)));
        }
    }
    let server = scripted(pages).await;
    let mut a = plain_account(&server);
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    // the first gap is read on, but not to its end: the second answer, cut too, isn't moved past
    let error = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap_err();
    assert!(matches!(error, CatchUpError::GapPending), "{error:?}");
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert_eq!(state.sync_link, Some(link("s2")));
    assert_eq!(state.gap_link, Some(link(&gap[2 * MAX_PAGES - 1])));
    // the next sync reads the first gap to its end, then keeps the second's rest
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert_eq!(state.sync_link, Some(link("s3")));
    assert_eq!(state.gap_link, Some(link(&next[MAX_PAGES - 1])));
    assert!(a.catch_up.load_older(&mut a.store, CHAT).await.unwrap().is_some());
    assert!(a.store.sync_state(CHAT).unwrap().unwrap().history_complete);
    assert_eq!(stored_ids(&a).len(), 3 + gap.len() + next.len());
}

// --- our own message that comes back but isn't stored, and entries a check can't read ---

/// An account of a's on a plain service that takes every message post, its outbox reading `clock`, the plain chat
/// stored.
async fn posting_account(clock: &Arc<AtomicI64>) -> (MockServer, Account) {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"OriginalArrivalTime": 1_800_000_000_001_u64})))
        .mount(&server)
        .await;
    let mut a = Account::with_outbox_clock(&server.uri(), LAB_A, "placeholder-token", {
        let clock = Arc::clone(clock);
        Arc::new(move || clock.load(Ordering::SeqCst))
    });
    a.store.upsert_conversation(&plain_chat()).unwrap();
    (server, a)
}

/// Our own message for `row`, as a read gives it back `offset` ms after the row was made (its id that time).
fn own_entry(row: &OutboxEntry, offset: i64) -> Value {
    let mut entry = entry_at(u64::try_from(row.created_at + offset).unwrap());
    entry["clientmessageid"] = json!(row.client_message_id);
    entry["from"] = json!(format!("{CHAT_BASE}/v1/users/ME/contacts/{LAB_A}"));
    entry
}

/// Sends `row` with its first post's answer lost while the service can't be read, so its check fails and the row
/// waits its backoff (one post, the row pending).
async fn lose_the_answer_and_the_check(a: &mut Account, row: &OutboxEntry) {
    a.fault(Fault::LoseAnswer);
    let error = a.outbox.send(&mut a.store, &a.catch_up, row.id).await.unwrap_err();
    assert!(matches!(error, OutboxError::Sync { .. }), "{error:?}");
}

#[tokio::test]
async fn our_message_whose_version_cannot_be_read_still_settles_its_row() {
    let clock = Arc::new(AtomicI64::new(now_ms()));
    let (server, mut a) = posting_account(&clock).await;
    let row = enqueue(&mut a.store, &send(CHAT, "unversioned")).unwrap();
    let mut ours = own_entry(&row, 1000);
    ours["version"] = Value::Null;
    let id = ours["id"].as_str().unwrap().to_owned();
    mount_pages(&server, vec![("", page(vec![ours], Some("s1"), None))]).await;

    a.fault(Fault::LoseAnswer);
    let sent = a.outbox.send(&mut a.store, &a.catch_up, row.id).await.unwrap();
    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(sent.message_id.as_deref(), Some(id.as_str()));
    assert_eq!(post_count(&server).await, 1, "posted once");
    assert!(a.store.message(CHAT, &id).unwrap().is_none(), "matched, not stored");
}

#[tokio::test]
async fn our_message_whose_arrival_time_cannot_be_read_still_settles_its_row() {
    let clock = Arc::new(AtomicI64::new(now_ms()));
    let (server, mut a) = posting_account(&clock).await;
    let row = enqueue(&mut a.store, &send(CHAT, "timeless")).unwrap();
    // no `originalarrivaltime`, and an id that isn't a time either
    let mut ours = own_entry(&row, 1000);
    ours["id"] = json!("not-a-time");
    mount_pages(&server, vec![("", page(vec![ours], Some("s1"), None))]).await;

    a.fault(Fault::LoseAnswer);
    let sent = a.outbox.send(&mut a.store, &a.catch_up, row.id).await.unwrap();
    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(sent.message_id.as_deref(), Some("not-a-time"));
    assert_eq!(post_count(&server).await, 1, "posted once");
    assert!(
        a.store.message(CHAT, "not-a-time").unwrap().is_none(),
        "matched, not stored"
    );
}

#[tokio::test]
async fn our_unversioned_message_seen_by_a_catch_up_is_not_sent_again_by_a_later_check() {
    let clock = Arc::new(AtomicI64::new(now_ms()));
    let (server, mut a) = posting_account(&clock).await;
    let row = enqueue(&mut a.store, &send(CHAT, "seen first by a catch-up")).unwrap();
    lose_the_answer_and_the_check(&mut a, &row).await;

    let mut ours = own_entry(&row, 1000);
    ours["version"] = json!("v2");
    mount_pages(
        &server,
        vec![
            ("", page(vec![ours], Some("s1"), None)),
            // what the later check reads: nothing new
            ("s1", page(vec![], Some("s2"), None)),
        ],
    )
    .await;
    let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!((synced.inserted, synced.skipped), (0, 1), "{synced:?}");

    clock.fetch_add(3_600_000, Ordering::SeqCst);
    let sent = a.outbox.send(&mut a.store, &a.catch_up, row.id).await.unwrap();
    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(post_count(&server).await, 1, "posted once");
}

#[tokio::test]
async fn an_unstored_message_from_someone_else_or_another_conversation_matches_nothing() {
    let clock = Arc::new(AtomicI64::new(now_ms()));
    let (server, mut a) = posting_account(&clock).await;
    let row = enqueue(&mut a.store, &send(CHAT, "not theirs")).unwrap();
    lose_the_answer_and_the_check(&mut a, &row).await;

    let mut theirs = own_entry(&row, 1000);
    theirs["version"] = Value::Null;
    theirs["from"] = json!(format!("{CHAT_BASE}/v1/users/ME/contacts/{LAB_B}"));
    let mut elsewhere = own_entry(&row, 2000);
    elsewhere["version"] = Value::Null;
    elsewhere["conversationid"] = json!("19:another@thread.v2");
    let entries = vec![elsewhere, theirs];
    mount_pages(
        &server,
        vec![
            ("", page(entries.clone(), Some("s1"), None)),
            ("s1", page(entries, Some("s2"), None)),
        ],
    )
    .await;
    // an ordinary catch-up, then the check's own sync
    let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!(synced.skipped, 2, "{synced:?}");
    a.catch_up
        .sync_conversation_since(&mut a.store, CHAT, row.created_at)
        .await
        .unwrap();

    let after = a.store.outbox_entry(row.id).unwrap().unwrap();
    assert_eq!((after.state, after.message_id), (OutboxState::Pending, None));
    assert_eq!(post_count(&server).await, 1);
}

#[tokio::test]
async fn our_control_message_with_a_rows_id_never_settles_it() {
    let clock = Arc::new(AtomicI64::new(now_ms()));
    let (server, mut a) = posting_account(&clock).await;
    let row = enqueue(&mut a.store, &send(CHAT, "typed, then sent")).unwrap();
    lose_the_answer_and_the_check(&mut a, &row).await;

    let typing = control(own_entry(&row, 1000), "Control/Typing");
    let mut unversioned = control(own_entry(&row, 2000), "Control/ClearTyping");
    unversioned["version"] = Value::Null;
    mount_pages(
        &server,
        vec![("", page(vec![unversioned.clone(), typing.clone()], Some("s1"), None))],
    )
    .await;
    let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!(
        (synced.changed(), synced.skipped, synced.control),
        (0, 0, 2),
        "{synced:?}"
    );
    // as Trouter deliveries
    for entry in [typing, unversioned] {
        let message: chat::Message = serde_json::from_value(entry).unwrap();
        let received = a.catch_up.receive_message(&mut a.store, CHAT, &message).unwrap();
        assert!(received.is_none());
    }

    assert!(stored_ids(&a).is_empty());
    let after = a.store.outbox_entry(row.id).unwrap().unwrap();
    assert_eq!((after.state, after.message_id), (OutboxState::Pending, None));
    assert_eq!(post_count(&server).await, 1);
}

#[tokio::test]
async fn an_entry_a_check_cannot_read_that_carries_the_rows_id_fails_the_check() {
    let clock = Arc::new(AtomicI64::new(now_ms()));
    let (server, mut a) = posting_account(&clock).await;
    let row = enqueue(&mut a.store, &send(CHAT, "unreadable")).unwrap();
    mount_pages(
        &server,
        vec![
            ("", page(vec![], Some("s1"), None)),
            ("s1", page(vec![unreadable(&row, 1000)], Some("s2"), None)),
        ],
    )
    .await;
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();

    a.fault(Fault::LoseAnswer);
    let error = a.outbox.send(&mut a.store, &a.catch_up, row.id).await.unwrap_err();
    assert_unreadable_seen(&error);
    assert_eq!(post_count(&server).await, 1, "not sent again");
    let after = a.store.outbox_entry(row.id).unwrap().unwrap();
    assert_eq!((after.state, after.failed_checks), (OutboxState::Pending, 1));
    assert!(after.may_have_arrived(), "{after:?}");
    assert_eq!(
        a.outbox.take_arrived(),
        std::slice::from_ref(&after),
        "the copy the catch-up kept is the row as the check left it"
    );
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert_eq!(
        state.sync_link,
        Some(link("s2")),
        "the check moves past the page: the row keeps what it saw"
    );
}

#[tokio::test]
async fn an_unreadable_entry_an_ordinary_catch_up_moves_past_keeps_later_checks_from_sending_the_row_again() {
    let clock = Arc::new(AtomicI64::new(now_ms()));
    let (server, mut a) = posting_account(&clock).await;
    let row = enqueue(&mut a.store, &send(CHAT, "unreadable, then caught up")).unwrap();
    lose_the_answer_and_the_check(&mut a, &row).await;
    a.outbox.take_arrived();
    mount_pages(
        &server,
        vec![
            ("", page(vec![unreadable(&row, 1000)], Some("s1"), None)),
            // what every later check reads: nothing new
            ("s1", page(vec![], Some("s1"), None)),
        ],
    )
    .await;

    let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!((synced.changed(), synced.skipped), (0, 1), "{synced:?}");
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert_eq!(state.sync_link, Some(link("s1")), "the catch-up isn't held up by it");
    let seen = a.store.outbox_entry(row.id).unwrap().unwrap();
    assert_eq!(seen.state, OutboxState::Pending);
    assert!(seen.may_have_arrived(), "{seen:?}");
    assert_eq!(
        a.outbox.take_arrived(),
        std::slice::from_ref(&seen),
        "the row's change is told"
    );

    // every later check fails, until the row is given up; nothing goes out again on its own
    for _ in seen.failed_checks..MAX_FAILED_CHECKS {
        clock.fetch_add(3_600_000, Ordering::SeqCst);
        let error = a.outbox.send(&mut a.store, &a.catch_up, row.id).await.unwrap_err();
        assert_unreadable_seen(&error);
    }
    let given_up = a.store.outbox_entry(row.id).unwrap().unwrap();
    assert_eq!(
        (given_up.state, given_up.failed_checks),
        (OutboxState::Failed, MAX_FAILED_CHECKS)
    );
    assert!(given_up.may_have_arrived(), "{given_up:?}");
    assert!(outcome_unknown(&given_up), "{given_up:?}");
    assert!(
        given_up.last_error.as_deref().unwrap().contains("may have arrived"),
        "{given_up:?}"
    );
    assert_eq!(post_count(&server).await, 1, "never sent again on its own");

    // only an explicit resend sends it again, with the same clientmessageid
    a.store.resend(row.id).unwrap();
    let sent = a.outbox.send(&mut a.store, &a.catch_up, row.id).await.unwrap();
    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(post_count(&server).await, 2);
}

#[tokio::test]
async fn an_unreadable_entry_marks_only_a_post_of_its_conversation_that_had_an_attempt() {
    const OTHER: &str = "19:00000000000000000000000000000abc@thread.v2";
    let clock = Arc::new(AtomicI64::new(now_ms()));
    let (server, mut a) = posting_account(&clock).await;
    let unsent = enqueue(&mut a.store, &send(CHAT, "never attempted")).unwrap();
    let elsewhere = enqueue(&mut a.store, &send(OTHER, "posted to another chat")).unwrap();
    a.store.begin_attempt(elsewhere.id).unwrap();
    let lost = AttemptOutcome::Retry {
        not_before: 0,
        error: "timed out".to_owned(),
    };
    a.store.finish_attempt(elsewhere.id, &lost).unwrap();
    let entries = vec![unreadable(&unsent, 1000), unreadable(&elsewhere, 2000)];
    mount_pages(&server, vec![("", page(entries, Some("s1"), None))]).await;

    let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();

    assert_eq!(synced.skipped, 2, "{synced:?}");
    for row in [unsent, elsewhere] {
        let after = a.store.outbox_entry(row.id).unwrap().unwrap();
        assert_eq!(after.unreadable_seen_at, None, "{after:?}");
    }
    assert!(a.outbox.take_arrived().is_empty());
}

/// An entry no read takes for a message (`properties` must be an object) that carries `row`'s clientmessageid, as our
/// own message would come back `offset` ms after the row was made.
fn unreadable(row: &OutboxEntry, offset: i64) -> Value {
    let mut raw = own_entry(row, offset);
    raw["properties"] = json!("unreadable");
    raw
}

/// The error of a check that read, or a read before it had read, an entry it can't read carrying its row's
/// clientmessageid.
fn assert_unreadable_seen(error: &OutboxError) {
    let OutboxError::Sync { source, .. } = error else {
        panic!("{error:?}");
    };
    assert!(source.downcast_ref::<UnreadableSeen>().is_some(), "{error:?}");
}

/// How many message posts the plain service got.
async fn post_count(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.method.as_str() == "POST")
        .count()
}

// --- the account's own notes ---

/// A plain service whose conversation list is empty and whose notes hold one note of the account's own, synced on.
async fn notes_service() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/chatsvc/amer/v1/users/ME/conversations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"conversations": []})))
        .mount(&server)
        .await;
    let mut note = entry(1_760_000_001_000, json!(1_760_000_001_000_u64));
    note["conversationid"] = json!(NOTES);
    note["from"] = json!(format!("{CHAT_BASE}/v1/users/ME/contacts/{LAB_A}"));
    let sync = format!("{CHAT_BASE}/v1/users/ME/conversations/48%3Anotes/messages?syncState=n1&view=msnp24Equivalent");
    let page = json!({"messages": [note], "_metadata": {"syncState": sync}});
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path_regex(
            "/conversations/48(:|%3A)notes/messages$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(page))
        .mount(&server)
        .await;
    server
}

/// The paths (after the chat base's `v1`) the plain service was asked for.
async fn paths_read(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| {
            let path = request.url.path().trim_start_matches("/api/chatsvc/amer/v1/");
            format!("{} {}", request.method, path.replace("%3A", ":"))
        })
        .collect()
}

#[tokio::test]
async fn a_catch_up_syncs_the_favourite_notes_the_list_never_brings() {
    let server = notes_service().await;
    let mut a = Account::on(&server.uri(), LAB_A, "placeholder-token");
    a.store.replace_favorites(&[NOTES.to_owned()], START).unwrap();

    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    assert_eq!(report.messages.inserted, 1);
    let stored = a.store.conversation(NOTES).unwrap().unwrap();
    assert_eq!(
        stored.kind,
        ConversationKind::Other("thread_type:streamofnotes".to_owned())
    );
    let notes = a.messages(NOTES);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].sender.as_deref(), Some(LAB_A), "a note is the account's own");
    assert_eq!(
        paths_read(&server).await,
        [
            "GET users/ME/properties",
            "GET users/ME/conversations",
            "GET users/ME/conversations/48:notes/messages"
        ],
        "only GETs, and the notes are never read as a thread"
    );

    // the next catch-up follows the notes' own sync link
    a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert_eq!(tokens_read(&server).await, ["n1"]);
}

#[tokio::test]
async fn notes_neither_favourite_nor_stored_are_not_read() {
    let server = notes_service().await;
    let mut a = Account::on(&server.uri(), LAB_A, "placeholder-token");
    a.store.replace_favorites(&[], START).unwrap();

    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    assert!(a.store.conversation(NOTES).unwrap().is_none());
    assert_eq!(
        paths_read(&server).await,
        ["GET users/ME/properties", "GET users/ME/conversations"]
    );
}

/// A plain service whose conversation list is empty, with a sync link, and whose notes answer `status`.
async fn notes_answering(status: u16) -> MockServer {
    let server = MockServer::start().await;
    let sync = format!("{CHAT_BASE}/v1/users/ME/conversations?syncState=l1&view=msnp24Equivalent");
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/chatsvc/amer/v1/users/ME/conversations"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"conversations": [], "_metadata": {"syncState": sync}})),
        )
        .mount(&server)
        .await;
    // the chat service's answer for notes never written (a 404's shape, its text made up)
    let missing = json!({"errorCode": status, "message": "synthetic: not found"});
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path_regex(
            "/conversations/48(:|%3A)notes/messages$",
        ))
        .respond_with(ResponseTemplate::new(status).set_body_json(missing))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn notes_that_fail_to_sync_never_hold_the_lists_sync_link_back() {
    let server = notes_answering(500).await;
    let mut a = Account::on(&server.uri(), LAB_A, "placeholder-token");
    a.store.replace_favorites(&[NOTES.to_owned()], START).unwrap();

    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert_eq!(report.failed[0].conversation, NOTES);
    assert!(a.store.conversation_list_sync_link().unwrap().is_some());
}

#[tokio::test]
async fn notes_never_written_are_none_not_a_failure() {
    let server = notes_answering(404).await;
    let mut a = Account::on(&server.uri(), LAB_A, "placeholder-token");
    a.store.replace_favorites(&[NOTES.to_owned()], START).unwrap();

    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    assert!(a.messages(NOTES).is_empty());
    assert!(
        a.store.sync_state(NOTES).unwrap().is_none(),
        "read again from the newest page next time"
    );
}

/// Answers the plain service's conversation list a page at a time: the first read (no `syncState` of
/// `page-<n>`) its first page, each page's `forwardLink` naming the next, the last of `pages` naming none but a sync
/// link. Every conversation's own reads are refused (`404`): only the list matters here.
async fn mount_list_pages(server: &MockServer, pages: Vec<Vec<Value>>) {
    let last = pages.len() - 1;
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/chatsvc/amer/v1/users/ME/conversations"))
        .respond_with(move |request: &Request| {
            let index = request
                .url
                .query_pairs()
                .find(|(name, _)| name == "syncState")
                .and_then(|(_, value)| value.strip_prefix("page-").and_then(|n| n.parse::<usize>().ok()))
                .unwrap_or(0)
                .min(last);
            let metadata = if index == last {
                json!({"syncState": list_link("l2")})
            } else {
                json!({"forwardLink": list_link(&format!("page-{}", index + 1))})
            };
            ResponseTemplate::new(200).set_body_json(json!({"conversations": pages[index], "_metadata": metadata}))
        })
        .with_priority(1)
        .mount(server)
        .await;
}

async fn list_reads(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path().ends_with("/users/ME/conversations"))
        .count()
}

#[tokio::test]
async fn a_conversation_list_of_several_pages_is_read_to_its_last() {
    let server = MockServer::start().await;
    let mut a = Account::on(&server.uri(), LAB_A, "placeholder-token");
    mount_list_pages(
        &server,
        vec![
            vec![listed("48:made-up-first", 1)],
            vec![listed("48:made-up-second", 1), json!({"not": "a conversation"})],
            vec![listed("48:made-up-third", 1)],
        ],
    )
    .await;
    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert_eq!(report.conversations, 3, "{report:?}");
    assert_eq!(report.skipped, 1, "the entry of another shape: {report:?}");
    assert_eq!(list_reads(&server).await, 3);
}

#[tokio::test]
async fn a_conversation_list_that_never_ends_stops_at_the_page_limit() {
    let server = MockServer::start().await;
    let mut a = Account::on(&server.uri(), LAB_A, "placeholder-token");
    let pages = (0..=MAX_PAGES)
        .map(|n| vec![listed(&format!("48:made-up-{n}"), 1)])
        .collect();
    mount_list_pages(&server, pages).await;
    let error = a.catch_up.catch_up(&mut a.store).await.unwrap_err();
    assert!(matches!(error, CatchUpError::TooManyPages), "{error:?}");
    assert_eq!(list_reads(&server).await, MAX_PAGES);
}

#[tokio::test]
async fn a_catch_up_s_debug_names_its_account_and_page_size_but_nothing_of_its_transport() {
    let server = MockServer::start().await;
    let a = Account::on(&server.uri(), LAB_A, "placeholder-token");
    let shown = format!("{:?}", a.catch_up);
    assert!(shown.starts_with("CatchUp"), "{shown}");
    assert!(shown.contains(LAB_A), "{shown}");
    assert!(shown.contains("page_size"), "{shown}");
    assert!(!shown.contains("placeholder-token"), "{shown}");
    assert!(!shown.contains(&server.uri()), "{shown}");
}

fn message_missing_copy(version: u64) -> Value {
    let mut blocked = entry(1500, json!(version));
    blocked["from"] = json!(format!("{CHAT_BASE}/v1/users/ME/contacts/{LAB_A}"));
    blocked["content"] = json!("");
    blocked["properties"] = json!({"policyViolation": {"state": 1, "genericStreamItemId": 900}});
    blocked
}

fn keep_message_missing_copy(account: &mut Account) {
    let message = serde_json::from_value(message_missing_copy(1501)).unwrap();
    account
        .catch_up
        .receive_message(&mut account.store, CHAT, &message)
        .unwrap();
}

#[tokio::test]
async fn replayed_blocked_messages_leave_queued_copies_for_after_sync_completion() {
    for catch_up in [false, true] {
        for version in [1501, 1502] {
            let server = scripted(vec![(
                "",
                page(
                    vec![message_missing_copy(version), entry(2000, json!(2000))],
                    Some("s1"),
                    None,
                ),
            )])
            .await;
            mount_list(&server, vec![listed(CHAT, 2)], "l1").await;
            Mock::given(wiremock::matchers::path_regex("/threads/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": CHAT})))
                .with_priority(1)
                .mount(&server)
                .await;
            mount_copy_refusal(&server).await;
            let mut a = plain_account(&server);
            keep_message_missing_copy(&mut a);
            if catch_up {
                let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
                assert!(report.failed.is_empty(), "{report:?}");
            } else {
                let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
                assert_eq!(synced.unchanged, usize::from(version == 1501));
                assert_eq!(synced.updated, usize::from(version == 1502));
            }
            assert!(a.store.message(CHAT, "2000").unwrap().is_some());
            assert!(a.store.sync_state(CHAT).unwrap().unwrap().sync_link.is_some());
            assert!(
                !paths_read(&server)
                    .await
                    .iter()
                    .any(|path| path.ends_with("/messages/900"))
            );
            a.catch_up.retry_blocked_copies(&mut a.store).await;
            assert_copy_after(&server, "/messages").await;
        }
    }
}

async fn mount_copy_refusal(server: &MockServer) {
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path_regex(
            "/conversations/48(:|%3A)generic/messages/900$",
        ))
        .respond_with(ResponseTemplate::new(403))
        .with_priority(1)
        .mount(server)
        .await;
}

async fn assert_copy_after(server: &MockServer, earlier_suffix: &str) {
    let paths = paths_read(server).await;
    let earlier = paths.iter().position(|path| path.ends_with(earlier_suffix)).unwrap();
    let copies: Vec<_> = paths
        .iter()
        .enumerate()
        .filter(|(_, path)| path.ends_with("/messages/900"))
        .collect();
    assert_eq!(copies.len(), 1, "{paths:?}");
    assert!(earlier < copies[0].0, "{paths:?}");
}

struct MessageProgress(Arc<AtomicBool>);

impl teams_core::sync::Meanwhile for MessageProgress {
    fn serve<'a>(&'a self, _store: &'a mut Store) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(future::pending())
    }

    fn progress(&self, store: &Store, _changes: &mut teams_core::sync::Changes) {
        if store.message(CHAT, "2000").unwrap().is_some() {
            self.0.store(true, Ordering::SeqCst);
        }
    }
}

#[tokio::test]
async fn missing_blocked_copies_wait_for_catch_up_messages_and_progress() {
    let server = scripted(vec![("", page(vec![entry(2000, json!(2000))], Some("s1"), None))]).await;
    mount_list(&server, vec![listed(CHAT, 2)], "l1").await;
    Mock::given(wiremock::matchers::path_regex("/threads/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": CHAT})))
        .with_priority(1)
        .mount(&server)
        .await;
    let mut a = plain_account(&server);
    keep_message_missing_copy(&mut a);
    let published = Arc::new(AtomicBool::new(false));
    let published_before_copy = Arc::new(AtomicBool::new(false));
    a.catch_up = a
        .catch_up
        .with_meanwhile(Arc::new(MessageProgress(Arc::clone(&published))));
    Mock::given(wiremock::matchers::path_regex(
        "/conversations/48(:|%3A)generic/messages/900$",
    ))
    .respond_with({
        let published_before_copy = Arc::clone(&published_before_copy);
        let published = Arc::clone(&published);
        move |_: &Request| {
            published_before_copy.store(published.load(Ordering::SeqCst), Ordering::SeqCst);
            ResponseTemplate::new(403)
        }
    })
    .with_priority(1)
    .mount(&server)
    .await;
    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    assert!(published.load(Ordering::SeqCst));
    assert!(
        !paths_read(&server)
            .await
            .iter()
            .any(|path| path.ends_with("/messages/900"))
    );
    a.catch_up.retry_blocked_copies(&mut a.store).await;
    assert!(published_before_copy.load(Ordering::SeqCst));
    assert_copy_after(&server, "/conversations").await;
    assert_copy_after(&server, "/messages").await;
}

#[tokio::test]
async fn missing_blocked_copies_follow_successful_and_failed_chat_reads() {
    for status in [200, 403] {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::method("GET"))
            .respond_with(ResponseTemplate::new(status).set_body_json(page(vec![], Some("s1"), None)))
            .mount(&server)
            .await;
        mount_copy_refusal(&server).await;
        let mut a = plain_account(&server);
        keep_message_missing_copy(&mut a);
        let result = a.catch_up.sync_messages(&mut a.store, CHAT).await;
        assert_eq!(result.is_ok(), status == 200, "{result:?}");
        assert!(
            !paths_read(&server)
                .await
                .iter()
                .any(|path| path.ends_with("/messages/900"))
        );
        a.catch_up.retry_blocked_copies(&mut a.store).await;
        assert_copy_after(&server, "/messages").await;
    }
}

#[tokio::test]
async fn missing_blocked_copies_are_retried_even_when_the_list_read_fails() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let mut a = plain_account(&server);
    keep_message_missing_copy(&mut a);
    assert!(a.catch_up.catch_up(&mut a.store).await.is_err());
    assert!(
        !paths_read(&server)
            .await
            .iter()
            .any(|path| path.ends_with("/messages/900"))
    );
    a.catch_up.retry_blocked_copies(&mut a.store).await;
    assert_copy_after(&server, "/conversations").await;
}

#[tokio::test]
async fn a_blocked_copy_store_error_does_not_change_the_normal_sync_result() {
    let server = scripted(vec![("", page(vec![], Some("s1"), None))]).await;
    mount_list(&server, vec![], "l1").await;
    let mut a = plain_account(&server);
    rusqlite::Connection::open(a.store.path())
        .unwrap()
        .execute("DROP TABLE blocked_copies", [])
        .unwrap();
    a.catch_up.catch_up(&mut a.store).await.unwrap();
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    a.catch_up.retry_blocked_copies(&mut a.store).await;
}
