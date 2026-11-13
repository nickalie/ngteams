//! The outbox on a real store (a temporary file) and the native HTTP transport on wiremock, reached through the
//! `test-support` mock origin: a command becomes a row, each attempt asks the fence again, the outbox's part of the
//! retry table, our own message matched by its `clientmessageid`, and no resend across runs.
//!
//! The chat service's sync is played by `ServiceSync`, which stores as messages what the mock service received (or was
//! seeded with). Tokens, MRIs and conversation ids here are placeholders, never real ones.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::collections::VecDeque;
use std::future::{self, Future};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Map, Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::{
    CHECK_RETRY_WAIT, Clock, Command, ConversationSync, MAX_ATTEMPTS, MAX_FAILED_CHECKS, MAX_TRANSIENT_WAIT, Outbox,
    OutboxError, Sent, SyncError, Transient, TransientSent, create_chat, enqueue, new_client_message_id,
    outcome_unknown,
};
use teams_proto::Mri;
use teams_proto::chat::MessageText;
use teams_proto::{
    Author, Credentials, DEFAULT_RETRY_WAIT, Failure, HttpError, HttpTransport, Read, Request as ProtoRequest,
    Response, Transport,
};
use teams_store::{
    Applied, AttemptOutcome, Conversation, ConversationKind, Message, OutboxEntry, OutboxKind, OutboxState, Store,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ME: &str = "8:orgid:lab-a";
const PEER: &str = "8:orgid:lab-b";
const OUTSIDER: &str = "8:orgid:outsider";
const CHAT: &str = "19:lab-chat@thread.v2";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const MESSAGES: &str = "/api/chatsvc/amer/v1/users/ME/conversations/19:lab-chat@thread.v2/messages";
const LOCATION: &str =
    "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/19:lab-chat@thread.v2/messages/1001";
const START: i64 = 1_760_000_000_000;

// --- fakes ---

/// Hands out placeholder tokens; each rejection moves on to the next.
#[derive(Default)]
struct FakeTokens {
    generation: AtomicUsize,
    rejected: AtomicUsize,
}

impl FakeTokens {
    fn current(&self) -> Secret {
        Secret::new(format!(
            "placeholder-token-generation-{}",
            self.generation.load(Ordering::SeqCst)
        ))
    }
}

impl Credentials for FakeTokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(self.current()))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.rejected.fetch_add(1, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
        future::ready(Ok(self.current()))
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
    inner: HttpTransport<FakeTokens>,
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

/// The chat service's sync as the outbox's check sees it: each call stores, as our messages, the `clientmessageid`s
/// the service was seeded with and, when `finds` (the service stored what it was sent, its answer lost or not), those
/// of every post the mock service received.
struct ServiceSync {
    server: Arc<MockServer>,
    finds: AtomicBool,
    fails: AtomicBool,
    seeded: Mutex<Vec<String>>,
    /// How many posts the service had received at each call.
    calls: Mutex<Vec<usize>>,
}

impl ServiceSync {
    fn calls(&self) -> Vec<usize> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn seed(&self, client_message_id: &str) {
        self.seeded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(client_message_id.to_owned());
    }
}

impl ConversationSync for ServiceSync {
    async fn sync_conversation(&self, store: &mut Store, conversation: &str) -> Result<(), SyncError> {
        let posted = posted_ids(&self.server).await;
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(posted.len());
        if self.fails.load(Ordering::SeqCst) {
            return Err("the sync failed".into());
        }
        let mut found = self.seeded.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if self.finds.load(Ordering::SeqCst) {
            found.extend(posted);
        }
        for (n, client_message_id) in found.iter().enumerate() {
            let id = format!("{}", 2000 + n);
            store.upsert_message(&message(conversation, &id, 1, client_message_id, ME), None)?;
        }
        Ok(())
    }
}

/// The `clientmessageid` of every message post the mock service received, in order.
async fn posted_ids(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.method.as_str() == "POST" && request.url.path() == MESSAGES)
        .map(|request| {
            let body: Value = request.body_json().unwrap();
            body["clientmessageid"].as_str().unwrap_or_default().to_owned()
        })
        .collect()
}

fn message(conversation: &str, id: &str, version: i64, client_message_id: &str, sender: &str) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: id.to_owned(),
        version,
        client_message_id: Some(client_message_id.to_owned()),
        sender: Some(sender.to_owned()),
        sender_name: Some("Synthetic".to_owned()),
        message_type: "RichText/Html".to_owned(),
        content: "<p>hello</p>".to_owned(),
        properties: None,
        arrival_time: START,
        compose_time: Some(START),
        edit_time: None,
        delete_time: None,
    }
}

// --- setup ---

struct Setup {
    server: Arc<MockServer>,
    _dir: tempfile::TempDir,
    store_path: PathBuf,
    outbox: Outbox<Flaky>,
    sync: ServiceSync,
    tokens: Arc<FakeTokens>,
    faults: Arc<Mutex<VecDeque<Fault>>>,
    clock: Arc<AtomicI64>,
}

impl Setup {
    fn open_store(&self) -> Store {
        let mut store = Store::open(&self.store_path).unwrap();
        let chat = Conversation {
            id: CHAT.to_owned(),
            kind: ConversationKind::OneToOne,
            topic: None,
            version: 1,
            last_activity: None,
        };
        store.upsert_conversation(&chat).unwrap();
        store
    }

    fn fault(&self, fault: Fault) {
        self.faults
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(fault);
    }

    fn now(&self) -> i64 {
        self.clock.load(Ordering::SeqCst)
    }

    fn advance(&self, by: Duration) {
        self.clock
            .fetch_add(i64::try_from(by.as_millis()).unwrap(), Ordering::SeqCst);
    }

    async fn posted(&self) -> Vec<String> {
        posted_ids(&self.server).await
    }

    async fn answer(&self, status: u16) {
        Mock::given(method("POST"))
            .and(path(MESSAGES))
            .respond_with(ResponseTemplate::new(status))
            .mount(&self.server)
            .await;
    }

    /// Answers the next post with `status` (with `Retry-After` when given), the ones after it as mounted later.
    async fn answer_once(&self, status: u16, retry_after: Option<&str>) {
        let mut answer = ResponseTemplate::new(status);
        if let Some(wait) = retry_after {
            answer = answer.insert_header("Retry-After", wait);
        }
        Mock::given(method("POST"))
            .and(path(MESSAGES))
            .respond_with(answer)
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&self.server)
            .await;
    }

    async fn created(&self) {
        Mock::given(method("POST"))
            .and(path(MESSAGES))
            .respond_with(
                ResponseTemplate::new(201)
                    .insert_header("Location", LOCATION)
                    .set_body_json(json!({"OriginalArrivalTime": START})),
            )
            .mount(&self.server)
            .await;
    }
}

async fn setup() -> Setup {
    let server = Arc::new(MockServer::start().await);
    let dir = tempfile::tempdir().unwrap();
    let store_path = dir.path().join("store").join("a.sqlite");
    let tokens = Arc::new(FakeTokens::default());

    let faults = Arc::new(Mutex::new(VecDeque::new()));
    let transport = Flaky {
        inner: HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap(),
        faults: Arc::clone(&faults),
    };

    let clock = Arc::new(AtomicI64::new(START));
    let read_clock = Arc::clone(&clock);
    let clock_fn: Clock = Arc::new(move || read_clock.load(Ordering::SeqCst));

    let outbox = Outbox::new(Arc::new(transport), Author::new(Mri::new(ME), "Lab A")).with_clock(clock_fn);
    let sync = ServiceSync {
        server: Arc::clone(&server),
        finds: AtomicBool::new(false),
        fails: AtomicBool::new(false),
        seeded: Mutex::new(Vec::new()),
        calls: Mutex::new(Vec::new()),
    };
    Setup {
        server,
        _dir: dir,
        store_path,
        outbox,
        sync,
        tokens,
        faults,
        clock,
    }
}

fn hello() -> Command {
    Command::Send {
        conversation: CHAT.to_owned(),
        text: MessageText::plain("hello").unwrap(),
    }
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap()
}

async fn send(setup: &Setup, store: &mut Store, row: &OutboxEntry) -> OutboxEntry {
    setup.outbox.send(store, &setup.sync, row.id).await.unwrap()
}

// --- a command becomes a row ---

#[test]
fn a_command_becomes_a_pending_row_with_a_fresh_client_message_id() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let setup = runtime.block_on(setup());
    let mut store = setup.open_store();

    let first = enqueue(&mut store, &hello()).unwrap();
    let second = enqueue(&mut store, &hello()).unwrap();

    for row in [&first, &second] {
        assert_eq!(row.state, OutboxState::Pending);
        assert_eq!(row.kind, OutboxKind::Send);
        assert_eq!(row.conversation_id.as_deref(), Some(CHAT));
        assert_eq!(row.run_id, store.run_id());
        assert_eq!(row.attempts, 0);
        assert_eq!(row.client_message_id.len(), 19, "{}", row.client_message_id);
        assert!(row.client_message_id.bytes().all(|byte| byte.is_ascii_digit()));
        assert_eq!(serde_json::from_str::<Command>(&row.payload).unwrap(), hello());
    }
    assert_ne!(
        first.client_message_id, second.client_message_id,
        "fresh for each command"
    );
}

#[test]
fn client_message_ids_are_random_19_digit_numbers() {
    let ids: Vec<String> = (0..64).map(|_| new_client_message_id().unwrap()).collect();
    for id in &ids {
        assert_eq!(id.len(), 19, "{id}");
        assert!(!id.starts_with('0'));
    }
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), ids.len());
}

/// Every chat write but a post, as a command on [`CHAT`]'s message 1001 (a new chat with the peer for the last).
fn other_writes() -> Vec<(Command, OutboxKind)> {
    let on_message = |key: &str| (CHAT.to_owned(), "1001".to_owned(), key.to_owned());
    let (conversation, message_id, key) = on_message("like");
    vec![
        (
            Command::Edit {
                conversation: conversation.clone(),
                message_id: message_id.clone(),
                text: MessageText::plain("hello again").unwrap(),
                original: Sent {
                    client_message_id: "1111111111111111111".to_owned(),
                    compose_time_ms: START - 2_000,
                    arrival_time_ms: START - 1_000,
                    attachment_properties: Some(Map::new()),
                },
            },
            OutboxKind::Edit,
        ),
        (
            Command::Delete {
                conversation: conversation.clone(),
                message_id: message_id.clone(),
            },
            OutboxKind::Delete,
        ),
        (
            Command::React {
                conversation: conversation.clone(),
                message_id: message_id.clone(),
                key: key.clone(),
                on: true,
            },
            OutboxKind::AddReaction,
        ),
        (
            Command::React {
                conversation: conversation.clone(),
                message_id: message_id.clone(),
                key,
                on: false,
            },
            OutboxKind::RemoveReaction,
        ),
        (
            Command::ReadReceipt {
                conversation,
                message_id,
                client_message_id: "1111111111111111111".to_owned(),
            },
            OutboxKind::ReadReceipt,
        ),
        (
            Command::CreateChat {
                others: vec![PEER.to_owned()],
                topic: None,
            },
            OutboxKind::CreateChat,
        ),
        (
            Command::CreateChat {
                others: vec![PEER.to_owned(), OUTSIDER.to_owned()],
                topic: Some("Lab trio".to_owned()),
            },
            OutboxKind::CreateChat,
        ),
    ]
}

#[test]
fn a_new_chat_row_of_a_build_before_names_reads_as_unnamed() {
    let row = serde_json::json!({"command": "create_chat", "others": [PEER]});
    assert_eq!(
        serde_json::from_value::<Command>(row).unwrap(),
        Command::CreateChat {
            others: vec![PEER.to_owned()],
            topic: None
        }
    );
    // an unnamed one is stored as before
    let unnamed = Command::CreateChat {
        others: vec![PEER.to_owned()],
        topic: None,
    };
    assert!(!serde_json::to_string(&unnamed).unwrap().contains("topic"));
}

#[test]
fn every_chat_write_becomes_a_row_of_its_kind() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let setup = runtime.block_on(setup());
    let mut store = setup.open_store();

    for (command, kind) in other_writes() {
        let row = enqueue(&mut store, &command).unwrap();
        assert_eq!(row.kind, kind, "{command:?}");
        let conversation = (kind != OutboxKind::CreateChat).then_some(CHAT);
        assert_eq!(row.conversation_id.as_deref(), conversation, "{command:?}");
        assert_eq!(serde_json::from_str::<Command>(&row.payload).unwrap(), command);
    }
}

#[test]
fn a_new_chat_is_one_to_one_for_one_other_and_a_group_for_more() {
    let me = Mri::new(ME);
    let one = create_chat(&me, &[PEER.to_owned()], None).unwrap();
    assert_eq!(one.members(), [Mri::new(ME), Mri::new(PEER)]);
    let group = create_chat(&me, &[PEER.to_owned(), OUTSIDER.to_owned()], None).unwrap();
    assert_eq!(group.members().len(), 3);
    for others in [vec![], vec![ME.to_owned()], vec![PEER.to_owned(), PEER.to_owned()]] {
        assert!(create_chat(&me, &others, None).is_none(), "{others:?}");
    }
    // a group takes its name; a 1:1 chat none
    let named = create_chat(&me, &[PEER.to_owned(), OUTSIDER.to_owned()], Some("Lab trio")).unwrap();
    assert_eq!(named.topic(), Some("Lab trio"));
    let one = create_chat(&me, &[PEER.to_owned()], Some("Pair")).unwrap();
    assert_eq!(one.topic(), None);
}

#[tokio::test]
async fn an_edit_sends_the_message_again_as_it_was_sent_and_keeps_its_id() {
    let setup = setup().await;
    Mock::given(method("PUT"))
        .and(path(format!("{MESSAGES}/1001")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"OriginalArrivalTime": START + 5})))
        .mount(&setup.server)
        .await;
    let mut store = setup.open_store();
    let (edit, _) = other_writes().remove(0);
    let row = enqueue(&mut store, &edit).unwrap();

    let sent = send(&setup, &mut store, &row).await;

    assert_eq!(sent.state, OutboxState::Sent, "{sent:?}");
    assert_eq!(sent.message_id.as_deref(), Some("1001"), "the edited message's own id");
    let requests = setup.server.received_requests().await.unwrap();
    let body: Value = requests.last().unwrap().body_json().unwrap();
    assert_eq!(body["clientmessageid"], "1111111111111111111");
    assert!(body["content"].as_str().unwrap().contains("hello again"));
    assert_eq!(
        body["properties"]["edittime"],
        json!(row.created_at),
        "edited when it was asked for"
    );
}

#[tokio::test]
async fn original_attachment_values_survive_a_reopen_and_explicit_resend() {
    let setup = setup().await;
    Mock::given(method("PUT"))
        .and(path(format!("{MESSAGES}/1001")))
        .respond_with(ResponseTemplate::new(200))
        .mount(&setup.server)
        .await;
    let entries = json!([{"unknown": {"nested": [1, null]}}, {"futureEntry": true}]);
    for value in [
        json!(entries.to_string()),
        entries,
        Value::Null,
        json!({"opaque": "unread"}),
        json!(17),
    ] {
        let properties = serde_json::from_value(json!({"files": value, "cards": value})).unwrap();
        let row = {
            let mut earlier = setup.open_store();
            let mut command = edit("changed text");
            let Command::Edit { original, .. } = &mut command else {
                unreachable!()
            };
            original.attachment_properties = Some(properties);
            enqueue(&mut earlier, &command).unwrap()
        };
        let before = setup.server.received_requests().await.unwrap().len();
        let mut store = setup.open_store();
        assert!(
            setup
                .outbox
                .send_due(&mut store, &setup.sync)
                .await
                .unwrap()
                .rows
                .is_empty()
        );
        assert_eq!(setup.server.received_requests().await.unwrap().len(), before);
        store.resend(row.id).unwrap();
        let sent = send(&setup, &mut store, &row).await;
        assert_eq!(sent.state, OutboxState::Sent, "{sent:?}");
        assert_eq!(sent.payload, row.payload, "known snapshots are not rehydrated");
        let requests = setup.server.received_requests().await.unwrap();
        let body: Value = requests.last().unwrap().body_json().unwrap();
        assert_eq!(body["properties"]["files"], value);
        assert_eq!(body["properties"]["cards"], value);
    }
}

#[tokio::test]
async fn a_legacy_edit_is_upgraded_once_and_keeps_its_snapshot_after_a_failed_attempt_and_resend() {
    let setup = setup().await;
    Mock::given(method("PUT"))
        .and(path(format!("{MESSAGES}/1001")))
        .respond_with(ResponseTemplate::new(200))
        .mount(&setup.server)
        .await;
    let attachments = json!({
        "files": "[{\"futureFile\":{\"keep\":[null,7]}}]",
        "cards": null
    });
    let row = {
        let mut earlier = setup.open_store();
        let mut original = message(CHAT, "1001", 1, "1111111111111111111", ME);
        original.properties = Some(
            json!({
                "files": attachments["files"], "cards": attachments["cards"],
                "mentions": "stale mentions", "qtdMsgs": "stale quotes", "emotions": "stale reactions"
            })
            .to_string(),
        );
        earlier.upsert_message(&original, None).unwrap();
        let mut command = edit("edited by a legacy queued command");
        let Command::Edit { original, .. } = &mut command else {
            unreachable!()
        };
        original.attachment_properties = None;
        enqueue(&mut earlier, &command).unwrap()
    };
    assert!(!row.payload.contains("attachment_properties"));
    let mut store = setup.open_store();
    assert_no_due_rows(&setup, &mut store).await;
    store.resend(row.id).unwrap();
    setup.fault(Fault::LoseAnswer);
    let failed = send(&setup, &mut store, &row).await;
    assert_eq!(failed.state, OutboxState::Failed, "unknown edit outcome is not retried");
    assert_eq!(failed.attempts, 1);
    let payload: Value = serde_json::from_str(&failed.payload).unwrap();
    assert_eq!(payload["original"]["attachment_properties"], attachments);

    let mut changed = message(CHAT, "1001", 2, "1111111111111111111", ME);
    changed.properties = Some(json!({"files": "[]", "cards": "[]"}).to_string());
    store.upsert_message(&changed, None).unwrap();
    assert_no_due_rows(&setup, &mut store).await;
    assert_eq!(setup.server.received_requests().await.unwrap().len(), 1);
    drop(store);
    let mut store = setup.open_store();
    assert_no_due_rows(&setup, &mut store).await;
    store.resend(row.id).unwrap();
    let sent = send(&setup, &mut store, &row).await;
    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(sent.payload, failed.payload);
    assert_eq!(sent.client_message_id, row.client_message_id);
    assert_eq!(sent.created_at, row.created_at);
    assert!(setup.sync.calls().is_empty(), "edits never become findable posts");
    let requests = setup.server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests {
        let body: Value = request.body_json().unwrap();
        assert_eq!(body["properties"]["files"], attachments["files"]);
        assert_eq!(body["properties"]["cards"], attachments["cards"]);
        assert_eq!(body["properties"]["mentions"], "[]");
        assert!(body["properties"].get("qtdMsgs").is_none());
        assert!(body["properties"].get("emotions").is_none());
    }
}

async fn assert_no_due_rows(setup: &Setup, store: &mut Store) {
    assert!(setup.outbox.send_due(store, &setup.sync).await.unwrap().rows.is_empty());
}

#[tokio::test]
async fn a_legacy_edit_with_no_attachment_properties_gets_a_known_empty_snapshot() {
    let setup = setup().await;
    Mock::given(method("PUT"))
        .and(path(format!("{MESSAGES}/1001")))
        .respond_with(ResponseTemplate::new(200))
        .mount(&setup.server)
        .await;
    for properties in [None, Some("{}".to_owned())] {
        let mut store = setup.open_store();
        let version = if properties.is_none() { 1 } else { 2 };
        let mut message = message(CHAT, "1001", version, "1111111111111111111", ME);
        message.properties = properties;
        store.upsert_message(&message, None).unwrap();
        let mut command = edit("legacy text-only edit");
        let Command::Edit { original, .. } = &mut command else {
            unreachable!()
        };
        original.attachment_properties = None;
        let row = enqueue(&mut store, &command).unwrap();
        let sent = send(&setup, &mut store, &row).await;
        assert_eq!(sent.state, OutboxState::Sent, "{sent:?}");
        let payload: Value = serde_json::from_str(&sent.payload).unwrap();
        assert_eq!(payload["original"]["attachment_properties"], json!({}));
        let requests = setup.server.received_requests().await.unwrap();
        let body: Value = requests.last().unwrap().body_json().unwrap();
        assert_eq!(body["properties"]["files"], "[]");
        assert_eq!(body["properties"]["cards"], "[]");
    }
}

#[tokio::test]
async fn an_unrecoverable_legacy_edit_fails_locally_without_erasing_attachments() {
    for case in [
        "missing",
        "foreign",
        "wrong client id",
        "deleted",
        "invalid json",
        "nonobject",
    ] {
        let setup = setup().await;
        let mut store = setup.open_store();
        let mut original = message(CHAT, "1001", 1, "1111111111111111111", ME);
        match case {
            "foreign" => original.sender = Some(PEER.to_owned()),
            "wrong client id" => original.client_message_id = Some("another message".to_owned()),
            "deleted" => original.delete_time = Some(START),
            "invalid json" => original.properties = Some("{".to_owned()),
            "nonobject" => original.properties = Some("null".to_owned()),
            "missing" => {}
            _ => unreachable!(),
        }
        if case != "missing" {
            store.upsert_message(&original, None).unwrap();
        }
        let mut command = edit("legacy edit");
        let Command::Edit { original, .. } = &mut command else {
            unreachable!()
        };
        original.attachment_properties = None;
        let row = enqueue(&mut store, &command).unwrap();
        let failed = send(&setup, &mut store, &row).await;
        assert_eq!(failed.state, OutboxState::Failed, "{case}: {failed:?}");
        assert!(
            failed.last_error.as_deref().unwrap().contains("nothing was sent"),
            "{case}: {failed:?}"
        );
        assert!(!outcome_unknown(&failed), "{case}: {failed:?}");
        assert_eq!(failed.payload, row.payload);
        assert!(
            setup
                .outbox
                .send_due(&mut store, &setup.sync)
                .await
                .unwrap()
                .rows
                .is_empty()
        );
        assert!(setup.server.received_requests().await.unwrap().is_empty(), "{case}");
    }
}

#[tokio::test]
async fn a_write_other_than_a_post_whose_outcome_is_unknown_is_failed_not_tried_again() {
    let setup = setup().await;
    let mut store = setup.open_store();
    for (command, _) in other_writes() {
        setup.fault(Fault::DropRequest);
        let row = enqueue(&mut store, &command).unwrap();

        let ended = send(&setup, &mut store, &row).await;

        assert_eq!(ended.state, OutboxState::Failed, "{command:?}: {ended:?}");
        assert_eq!(ended.attempts, 1, "{command:?}");
        assert!(
            ended.last_error.as_deref().unwrap().contains("isn't sent again"),
            "{ended:?}"
        );
        assert!(outcome_unknown(&ended), "{ended:?}");
    }
    assert!(setup.sync.calls().is_empty(), "only a post can be looked for");
}

// --- each attempt asks the fence ---

#[tokio::test]
async fn a_permitted_send_posts_the_rows_client_message_id_and_takes_the_message_id_from_the_answer() {
    let setup = setup().await;
    setup.created().await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let sent = send(&setup, &mut store, &row).await;

    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(sent.message_id.as_deref(), Some("1001"), "from Location");
    assert_eq!(sent.attempts, 1);
    assert_eq!(setup.posted().await, std::slice::from_ref(&row.client_message_id));

    assert!(setup.sync.calls().is_empty(), "a first attempt needs no check");
}

#[tokio::test]
async fn without_a_location_the_message_id_is_the_arrival_time() {
    let setup = setup().await;
    Mock::given(method("POST"))
        .and(path(MESSAGES))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"OriginalArrivalTime": START})))
        .mount(&setup.server)
        .await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let sent = send(&setup, &mut store, &row).await;

    assert_eq!(sent.message_id, Some(START.to_string()));
}

#[tokio::test]
async fn an_explicit_401_response_is_retried_with_the_same_row() {
    let setup = setup().await;
    setup.answer_once(401, None).await;
    setup.created().await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let sent = send(&setup, &mut store, &row).await;

    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(sent.attempts, 2);
}

#[tokio::test]
async fn a_message_goes_out_from_the_account_with_its_text_composed_when_it_was_asked_for() {
    let setup = setup().await;
    setup.created().await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();
    setup.advance(Duration::from_secs(5));

    send(&setup, &mut store, &row).await;

    let requests = setup.server.received_requests().await.unwrap();
    let body: Value = requests[0].body_json().unwrap();
    assert_eq!(body["content"], "<p>hello</p>");
    assert_eq!((&body["from"], &body["imdisplayname"]), (&json!(ME), &json!("Lab A")));
    let composed = time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(row.created_at) * 1_000_000).unwrap();
    let iso = body["composetime"].as_str().unwrap();
    assert_eq!(
        time::OffsetDateTime::parse(iso, &time::format_description::well_known::Rfc3339).unwrap(),
        composed,
        "the row's time, not the attempt's"
    );
    assert_eq!(body["clientmessageid"], json!(row.client_message_id));
}

// --- the retry table ---

#[tokio::test]
async fn a_401_is_tried_once_more_with_the_refreshed_token_and_the_same_client_message_id() {
    let setup = setup().await;
    setup.answer_once(401, None).await;
    setup.created().await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let sent = send(&setup, &mut store, &row).await;

    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(
        setup.tokens.rejected.load(Ordering::SeqCst),
        1,
        "the session refreshed the refused token"
    );
    let received = setup.server.received_requests().await.unwrap();
    let tokens: Vec<_> = received
        .iter()
        .map(|request| {
            request
                .headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        tokens,
        [
            "Bearer placeholder-token-generation-0",
            "Bearer placeholder-token-generation-1"
        ]
    );
    let cmid = row.client_message_id.clone();
    assert_eq!(setup.posted().await, [cmid.clone(), cmid]);
}

#[tokio::test]
async fn a_second_401_fails_the_row() {
    let setup = setup().await;
    setup.answer(401).await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let failed = send(&setup, &mut store, &row).await;

    assert_eq!(failed.state, OutboxState::Failed);
    assert_eq!(failed.last_error.as_deref(), Some("HTTP 401"));
    assert_eq!(setup.posted().await.len(), 2, "one new attempt, no more");
}

#[tokio::test]
async fn a_check_that_cant_sync_between_two_401s_doesnt_give_the_row_another_attempt() {
    let setup = setup().await;
    setup.answer(401).await;
    setup.sync.fails.store(true, Ordering::SeqCst);
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let error = setup.outbox.send(&mut store, &setup.sync, row.id).await.unwrap_err();
    assert!(matches!(error, OutboxError::Sync { .. }), "{error:?}");
    assert_eq!(
        setup.posted().await.len(),
        1,
        "the check before the new attempt couldn't sync"
    );

    setup.sync.fails.store(false, Ordering::SeqCst);
    setup.advance(CHECK_RETRY_WAIT);
    let failed = send(&setup, &mut store, &row).await;

    assert_eq!(failed.state, OutboxState::Failed);
    assert_eq!(failed.last_error.as_deref(), Some("HTTP 401"));
    assert_eq!(setup.posted().await.len(), 2, "one new attempt, no more");
}

#[tokio::test]
async fn a_401_after_another_answer_still_fails_a_row_that_had_its_one_more_attempt() {
    let setup = setup().await;
    for (priority, status) in [(1, 401), (2, 500)] {
        Mock::given(method("POST"))
            .and(path(MESSAGES))
            .respond_with(ResponseTemplate::new(status))
            .up_to_n_times(1)
            .with_priority(priority)
            .mount(&setup.server)
            .await;
    }
    setup.answer(401).await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let failed = send(&setup, &mut store, &row).await;

    assert_eq!(failed.state, OutboxState::Failed);
    assert_eq!(failed.last_error.as_deref(), Some("HTTP 401"));
    assert_eq!(setup.posted().await.len(), 3, "401, 500, 401");
}

#[tokio::test]
async fn a_resend_gives_a_row_given_up_after_a_401_its_one_more_attempt_again() {
    let setup = setup().await;
    setup.answer_once(401, None).await;
    setup.created().await;
    setup.sync.fails.store(true, Ordering::SeqCst);
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();
    // the 401's one more attempt waits for a check that never syncs, until the row is given up
    for _ in 0..MAX_FAILED_CHECKS {
        let error = setup.outbox.send(&mut store, &setup.sync, row.id).await.unwrap_err();
        assert!(matches!(error, OutboxError::Sync { .. }), "{error:?}");
        setup.advance(Duration::from_secs(3_600));
    }
    assert_eq!(store.outbox_entry(row.id).unwrap().unwrap().state, OutboxState::Failed);

    setup.sync.fails.store(false, Ordering::SeqCst);
    store.resend(row.id).unwrap();
    setup.answer_once(401, None).await;
    let sent = send(&setup, &mut store, &row).await;

    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(setup.posted().await.len(), 3, "401, then after the resend 401 and 201");
}

#[tokio::test]
async fn a_429_waits_its_retry_after_before_a_new_attempt_with_the_same_client_message_id() {
    let setup = setup().await;
    setup.answer_once(429, Some("7")).await;
    setup.created().await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let waiting = send(&setup, &mut store, &row).await;
    assert_eq!(waiting.state, OutboxState::Pending);
    assert_eq!(waiting.not_before, Some(setup.now() + 7_000));
    assert_eq!(waiting.last_error.as_deref(), Some("HTTP 429"));

    // not due yet: nothing goes out
    assert_eq!(send(&setup, &mut store, &row).await, waiting);
    setup.advance(Duration::from_millis(6_999));
    assert!(
        setup
            .outbox
            .send_due(&mut store, &setup.sync)
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    assert_eq!(setup.posted().await.len(), 1);

    setup.advance(Duration::from_millis(1));
    let sent = setup.outbox.send_due(&mut store, &setup.sync).await.unwrap().rows;

    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].state, OutboxState::Sent);
    let cmid = row.client_message_id.clone();
    assert_eq!(setup.posted().await, [cmid.clone(), cmid]);
}

#[tokio::test]
async fn a_503_without_retry_after_waits_the_default() {
    let setup = setup().await;
    setup.answer_once(503, None).await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let waiting = send(&setup, &mut store, &row).await;

    assert_eq!(waiting.state, OutboxState::Pending);
    let default = i64::try_from(DEFAULT_RETRY_WAIT.as_millis()).unwrap();
    assert_eq!(waiting.not_before, Some(setup.now() + default));
    assert_eq!(setup.posted().await.len(), 1);
}

#[tokio::test]
async fn a_throttled_row_is_given_up_after_its_last_attempt() {
    let setup = setup().await;
    setup.answer(429).await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let mut last = send(&setup, &mut store, &row).await;
    while last.state == OutboxState::Pending {
        setup.advance(Duration::from_secs(60));
        last = send(&setup, &mut store, &row).await;
    }

    assert_eq!(last.state, OutboxState::Failed);
    assert_eq!(last.attempts, MAX_ATTEMPTS);
    assert_eq!(setup.posted().await.len(), usize::try_from(MAX_ATTEMPTS).unwrap());
}

#[tokio::test]
async fn any_other_4xx_fails_the_row_without_a_retry() {
    for status in [400, 403, 404, 409, 413] {
        let setup = setup().await;
        setup.answer(status).await;
        let mut store = setup.open_store();
        let row = enqueue(&mut store, &hello()).unwrap();

        let failed = send(&setup, &mut store, &row).await;

        assert_eq!(failed.state, OutboxState::Failed, "HTTP {status}");
        assert_eq!(failed.last_error, Some(format!("HTTP {status}")));
        assert_eq!(failed.attempts, 1);
        assert_eq!(setup.posted().await.len(), 1, "HTTP {status}: no retry");
        assert!(store.outbox_due(i64::MAX).unwrap().is_empty());
    }
}

#[tokio::test]
async fn a_timeout_whose_message_arrived_is_found_by_its_client_message_id_and_not_sent_again() {
    let setup = setup().await;
    setup.created().await;
    setup.fault(Fault::LoseAnswer);
    setup.sync.finds.store(true, Ordering::SeqCst);
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let sent = send(&setup, &mut store, &row).await;

    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(
        sent.message_id.as_deref(),
        Some("2000"),
        "the id the sync found it under"
    );
    assert_eq!(sent.attempts, 1);
    assert_eq!(
        setup.posted().await,
        std::slice::from_ref(&row.client_message_id),
        "sent once"
    );
    assert_eq!(setup.sync.calls(), [1], "the chat was synced after the timeout");
}

#[tokio::test]
async fn a_timeout_whose_message_didnt_arrive_is_sent_again_with_the_same_client_message_id() {
    let setup = setup().await;
    setup.created().await;
    setup.fault(Fault::DropRequest);
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let sent = send(&setup, &mut store, &row).await;

    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(sent.message_id.as_deref(), Some("1001"));
    assert_eq!(sent.attempts, 2);
    assert_eq!(
        setup.sync.calls(),
        [0],
        "the sync came first, before anything reached the service"
    );
    assert_eq!(setup.posted().await, std::slice::from_ref(&row.client_message_id));
}

#[tokio::test]
async fn a_5xx_other_than_503_is_checked_like_a_timeout() {
    let setup = setup().await;
    setup.answer_once(500, None).await;
    setup.created().await;
    // the service stored the message although it answered 500
    setup.sync.finds.store(true, Ordering::SeqCst);
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let sent = send(&setup, &mut store, &row).await;

    assert_eq!(
        sent.state,
        OutboxState::Sent,
        "the service had stored it: found by the sync"
    );
    assert_eq!(setup.posted().await.len(), 1, "not sent again");
    assert_eq!(setup.sync.calls(), [1]);
}

#[tokio::test]
async fn an_unknown_outcome_is_given_up_after_the_last_attempt() {
    let setup = setup().await;
    setup.answer(502).await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let failed = send(&setup, &mut store, &row).await;

    assert_eq!(failed.state, OutboxState::Failed);
    assert_eq!(failed.attempts, MAX_ATTEMPTS);
    assert!(
        failed
            .last_error
            .as_deref()
            .unwrap()
            .contains("whether it arrived is unknown")
    );
    assert_eq!(setup.posted().await.len(), usize::try_from(MAX_ATTEMPTS).unwrap());
    assert_eq!(
        setup.sync.calls().len(),
        usize::try_from(MAX_ATTEMPTS - 1).unwrap(),
        "a check before each new one"
    );
}

#[tokio::test]
async fn a_check_that_cant_sync_sends_nothing_and_leaves_the_row_waiting() {
    let setup = setup().await;
    setup.created().await;
    setup.fault(Fault::DropRequest);
    setup.sync.fails.store(true, Ordering::SeqCst);
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let error = setup.outbox.send(&mut store, &setup.sync, row.id).await.unwrap_err();

    assert!(
        matches!(error, OutboxError::Sync { id, .. } if id == row.id),
        "{error:?}"
    );
    let waiting = store.outbox_entry(row.id).unwrap().unwrap();
    assert_eq!(waiting.state, OutboxState::Pending);
    assert_eq!(waiting.attempts, 1);
    assert_eq!(waiting.failed_checks, 1);
    assert_eq!(waiting.not_before, Some(setup.now() + millis(CHECK_RETRY_WAIT)));
    assert!(setup.posted().await.is_empty());

    // not retried at once: it waits its backoff
    setup.sync.fails.store(false, Ordering::SeqCst);
    assert_eq!(send(&setup, &mut store, &row).await, waiting);
    assert_eq!(setup.sync.calls(), [0]);

    // once it is due and the sync works again, the check runs first and the row goes out
    setup.advance(CHECK_RETRY_WAIT);
    let sent = send(&setup, &mut store, &row).await;
    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(setup.sync.calls(), [0, 0]);
    assert_eq!(setup.posted().await.len(), 1);
}

#[tokio::test]
async fn a_row_whose_check_cant_sync_doesnt_hold_up_the_rows_after_it() {
    let setup = setup().await;
    setup.created().await;
    // the first row's post times out, and its chat can't be synced to look for it
    setup.fault(Fault::DropRequest);
    setup.sync.fails.store(true, Ordering::SeqCst);
    let mut store = setup.open_store();
    let stuck = enqueue(&mut store, &hello()).unwrap();
    let next = enqueue(&mut store, &hello()).unwrap();

    let done = setup.outbox.send_due(&mut store, &setup.sync).await.unwrap();

    assert_eq!(done.rows.len(), 2);
    assert_eq!((done.rows[0].id, done.rows[0].state), (stuck.id, OutboxState::Pending));
    assert_eq!((done.rows[1].id, done.rows[1].state), (next.id, OutboxState::Sent));
    let unsynced: Vec<i64> = done.unsynced.iter().map(|unsynced| unsynced.id).collect();
    assert_eq!(unsynced, [stuck.id]);
    assert_eq!(setup.posted().await, std::slice::from_ref(&next.client_message_id));

    // the stuck row isn't due again at once, so the next call has nothing to do
    let again = setup.outbox.send_due(&mut store, &setup.sync).await.unwrap();
    assert!(again.rows.is_empty() && again.unsynced.is_empty(), "{again:?}");
    assert_eq!(setup.sync.calls().len(), 1);
}

#[tokio::test]
async fn a_row_whose_check_keeps_failing_waits_longer_each_time_and_is_given_up() {
    let setup = setup().await;
    setup.created().await;
    setup.fault(Fault::DropRequest);
    setup.sync.fails.store(true, Ordering::SeqCst);
    let mut store = setup.open_store();
    enqueue(&mut store, &hello()).unwrap();

    let mut wait = CHECK_RETRY_WAIT;
    for failed in 1..MAX_FAILED_CHECKS {
        let done = setup.outbox.send_due(&mut store, &setup.sync).await.unwrap();
        assert_eq!(done.unsynced.len(), 1, "check {failed}");
        let waiting = &done.rows[0];
        assert_eq!(waiting.state, OutboxState::Pending, "check {failed}");
        assert_eq!(waiting.failed_checks, failed);
        assert_eq!(waiting.not_before, Some(setup.now() + millis(wait)), "check {failed}");
        setup.advance(wait);
        wait *= 2;
    }
    let done = setup.outbox.send_due(&mut store, &setup.sync).await.unwrap();

    let failed = &done.rows[0];
    assert_eq!(failed.state, OutboxState::Failed);
    assert_eq!(failed.failed_checks, MAX_FAILED_CHECKS);
    assert!(
        failed
            .last_error
            .as_deref()
            .unwrap()
            .contains("given up after 5 checks"),
        "{failed:?}"
    );
    assert_eq!(failed.attempts, 1, "nothing went out after the lost post");
    assert!(setup.posted().await.is_empty());
    assert_eq!(setup.sync.calls().len(), usize::try_from(MAX_FAILED_CHECKS).unwrap());
    assert!(store.outbox_due(i64::MAX).unwrap().is_empty());
}

// --- writes on one thing go out in order ---

fn react(message_id: &str, on: bool) -> Command {
    Command::React {
        conversation: CHAT.to_owned(),
        message_id: message_id.to_owned(),
        key: "like".to_owned(),
        on,
    }
}

fn edit(text: &str) -> Command {
    Command::Edit {
        conversation: CHAT.to_owned(),
        message_id: "1001".to_owned(),
        text: MessageText::plain(text).unwrap(),
        original: Sent {
            client_message_id: "1111111111111111111".to_owned(),
            compose_time_ms: START - 2_000,
            arrival_time_ms: START - 1_000,
            attachment_properties: Some(Map::new()),
        },
    }
}

fn read_up_to(message_id: &str) -> Command {
    Command::ReadReceipt {
        conversation: CHAT.to_owned(),
        message_id: message_id.to_owned(),
        client_message_id: "1111111111111111111".to_owned(),
    }
}

/// `CHAT`'s properties (a read receipt's URL).
const PROPERTIES: &str = "/api/chatsvc/amer/v1/users/ME/conversations/19:lab-chat@thread.v2/properties";

/// A message's properties (a reaction's URL, `?name=emotions`).
fn emotions(message_id: &str) -> String {
    format!("{MESSAGES}/{message_id}/properties")
}

/// Answers `verb` on `at` with 429 (`Retry-After: 7`) once, and with 200 after that.
async fn throttled_once(setup: &Setup, verb: &str, at: &str) {
    Mock::given(method(verb))
        .and(path(at))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "7"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&setup.server)
        .await;
    answered(setup, verb, at).await;
}

/// Answers `verb` on `at` with 200.
async fn answered(setup: &Setup, verb: &str, at: &str) {
    Mock::given(method(verb))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"OriginalArrivalTime": START + 5})))
        .mount(&setup.server)
        .await;
}

/// The requests the service received at `at`, in order: each one's method and body.
async fn requests_at(setup: &Setup, at: &str) -> Vec<(String, Value)> {
    setup
        .server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path() == at)
        .map(|request| {
            let body = request.body_json().unwrap_or(Value::Null);
            (request.method.to_string(), body)
        })
        .collect()
}

#[tokio::test]
async fn a_take_back_waits_for_its_throttled_reaction_and_goes_out_after_it() {
    let setup = setup().await;
    let at = emotions("1001");
    throttled_once(&setup, "PUT", &at).await;
    answered(&setup, "DELETE", &at).await;
    let mut store = setup.open_store();
    let reaction = enqueue(&mut store, &react("1001", true)).unwrap();
    let waiting = send(&setup, &mut store, &reaction).await;
    assert_eq!(waiting.state, OutboxState::Pending, "{waiting:?}");

    // taken back while the reaction waits out its Retry-After: it waits behind it, sending nothing
    let take_back = enqueue(&mut store, &react("1001", false)).unwrap();
    assert_eq!(send(&setup, &mut store, &take_back).await, take_back);
    let done = setup.outbox.send_due(&mut store, &setup.sync).await.unwrap();
    assert!(done.rows.is_empty(), "{done:?}");
    let methods = |requests: Vec<(String, Value)>| requests.into_iter().map(|(verb, _)| verb).collect::<Vec<_>>();
    assert_eq!(methods(requests_at(&setup, &at).await), ["PUT"]);

    setup.advance(Duration::from_secs(7));
    let done = setup.outbox.send_due(&mut store, &setup.sync).await.unwrap();

    let settled: Vec<(i64, OutboxState)> = done.rows.iter().map(|row| (row.id, row.state)).collect();
    assert_eq!(
        settled,
        [(reaction.id, OutboxState::Sent), (take_back.id, OutboxState::Sent)]
    );
    // the take-back went out last, so the reaction stays taken back
    assert_eq!(methods(requests_at(&setup, &at).await), ["PUT", "PUT", "DELETE"]);
}

#[tokio::test]
async fn a_later_edit_waits_for_an_earlier_throttled_one_so_the_later_text_stands() {
    let setup = setup().await;
    let at = format!("{MESSAGES}/1001");
    throttled_once(&setup, "PUT", &at).await;
    let mut store = setup.open_store();
    let first = enqueue(&mut store, &edit("the first edit")).unwrap();
    assert_eq!(send(&setup, &mut store, &first).await.state, OutboxState::Pending);
    let second = enqueue(&mut store, &edit("the second edit")).unwrap();

    let behind = send(&setup, &mut store, &second).await;

    assert_eq!((behind.state, behind.attempts), (OutboxState::Pending, 0), "{behind:?}");
    setup.advance(Duration::from_secs(7));
    let done = setup.outbox.send_due(&mut store, &setup.sync).await.unwrap();
    let settled: Vec<(i64, OutboxState)> = done.rows.iter().map(|row| (row.id, row.state)).collect();
    assert_eq!(settled, [(first.id, OutboxState::Sent), (second.id, OutboxState::Sent)]);
    let texts: Vec<bool> = requests_at(&setup, &at)
        .await
        .iter()
        .map(|(_, body)| body["content"].as_str().unwrap().contains("the second edit"))
        .collect();
    assert_eq!(texts, [false, false, true], "the second edit went out last");
}

#[tokio::test]
async fn a_later_read_receipt_waits_for_an_earlier_throttled_one() {
    let setup = setup().await;
    throttled_once(&setup, "PUT", PROPERTIES).await;
    let mut store = setup.open_store();
    let earlier = enqueue(&mut store, &read_up_to("1001")).unwrap();
    assert_eq!(send(&setup, &mut store, &earlier).await.state, OutboxState::Pending);
    let later = enqueue(&mut store, &read_up_to("1002")).unwrap();

    assert_eq!(send(&setup, &mut store, &later).await, later);
    assert_eq!(requests_at(&setup, PROPERTIES).await.len(), 1);

    setup.advance(Duration::from_secs(7));
    let done = setup.outbox.send_due(&mut store, &setup.sync).await.unwrap();
    let settled: Vec<i64> = done.rows.iter().map(|row| row.id).collect();
    assert_eq!(settled, [earlier.id, later.id]);
    assert_eq!(requests_at(&setup, PROPERTIES).await.len(), 3);
}

#[tokio::test]
async fn writes_on_anything_else_dont_wait_for_a_throttled_reaction() {
    let setup = setup().await;
    throttled_once(&setup, "PUT", &emotions("1001")).await;
    answered(&setup, "PUT", &emotions("1002")).await;
    answered(&setup, "PUT", PROPERTIES).await;
    setup.created().await;
    let mut store = setup.open_store();
    let reaction = enqueue(&mut store, &react("1001", true)).unwrap();
    assert_eq!(send(&setup, &mut store, &reaction).await.state, OutboxState::Pending);

    for command in [react("1002", true), read_up_to("1001"), hello()] {
        let row = enqueue(&mut store, &command).unwrap();
        let sent = send(&setup, &mut store, &row).await;
        assert_eq!(sent.state, OutboxState::Sent, "{command:?}: {sent:?}");
    }
}

// --- our own message coming back ---

#[tokio::test]
async fn our_message_coming_back_is_matched_to_its_row_and_stored_once() {
    let setup = setup().await;
    setup.created().await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();
    send(&setup, &mut store, &row).await;
    let back = message(CHAT, "1001", 1, &row.client_message_id, ME);

    let by_trouter = setup.outbox.receive(&mut store, &back, None).unwrap();
    let by_sync = setup.outbox.receive(&mut store, &back, None).unwrap();

    assert_eq!(by_trouter.applied, Applied::Inserted);
    assert_eq!(by_sync.applied, Applied::Ignored, "never stored twice");
    for received in [&by_trouter, &by_sync] {
        let matched = received.outbox.as_ref().unwrap();
        assert_eq!(matched.id, row.id);
        assert_eq!(matched.state, OutboxState::Sent);
        assert_eq!(matched.message_id.as_deref(), Some("1001"));
    }
    let page = store.messages_before(CHAT, None, 10).unwrap();
    assert_eq!(page.messages.len(), 1);
    assert_eq!(
        page.messages[0].client_message_id.as_deref(),
        Some(row.client_message_id.as_str())
    );
}

#[tokio::test]
async fn our_message_coming_back_before_the_answer_wins_over_the_answer() {
    let setup = setup().await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();
    // the attempt is in flight when Trouter brings the message
    store.begin_attempt(row.id).unwrap();

    let received = setup
        .outbox
        .receive(&mut store, &message(CHAT, "3003", 1, &row.client_message_id, ME), None)
        .unwrap();
    assert_eq!(received.outbox.as_ref().unwrap().state, OutboxState::Sent);

    let after = store
        .finish_attempt(
            row.id,
            &AttemptOutcome::Retry {
                not_before: START,
                error: "timed out".to_owned(),
            },
        )
        .unwrap();
    assert_eq!(after.state, OutboxState::Sent, "the late answer changes nothing");
    assert_eq!(after.message_id.as_deref(), Some("3003"));
}

#[tokio::test]
async fn a_later_version_of_our_message_keeps_its_row_as_it_is() {
    let setup = setup().await;
    setup.created().await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();
    let sent = send(&setup, &mut store, &row).await;

    let edited = message(CHAT, "1001", 2, &row.client_message_id, ME);
    let received = setup.outbox.receive(&mut store, &edited, None).unwrap();

    assert_eq!(received.applied, Applied::Inserted);
    assert_eq!(received.outbox, Some(sent));
}

#[tokio::test]
async fn a_message_that_isnt_ours_is_stored_but_matched_to_nothing() {
    let setup = setup().await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    // another sender, with our row's clientmessageid
    let theirs = message(CHAT, "4004", 1, &row.client_message_id, PEER);
    let received = setup.outbox.receive(&mut store, &theirs, None).unwrap();

    assert_eq!(received.applied, Applied::Inserted);
    assert_eq!(received.outbox, None);
    assert_eq!(store.outbox_entry(row.id).unwrap().unwrap().state, OutboxState::Pending);
}

#[tokio::test]
async fn our_message_in_another_conversation_is_matched_to_nothing() {
    let setup = setup().await;
    let mut store = setup.open_store();
    let other = Conversation {
        id: "19:other-chat@thread.v2".to_owned(),
        kind: ConversationKind::Group,
        topic: None,
        version: 1,
        last_activity: None,
    };
    store.upsert_conversation(&other).unwrap();
    let row = enqueue(&mut store, &hello()).unwrap();

    let elsewhere = message(&other.id, "5005", 1, &row.client_message_id, ME);
    let received = setup.outbox.receive(&mut store, &elsewhere, None).unwrap();

    assert_eq!(received.outbox, None);
    assert_eq!(store.outbox_entry(row.id).unwrap().unwrap().state, OutboxState::Pending);
}

// --- no resend across runs ---

#[tokio::test]
async fn rows_an_earlier_run_left_pending_are_not_sent_on_start_and_go_out_only_on_an_explicit_resend() {
    let setup = setup().await;
    setup.created().await;
    let row = {
        let mut earlier = setup.open_store();
        enqueue(&mut earlier, &hello()).unwrap()
    };
    let mut store = setup.open_store();

    assert!(
        setup
            .outbox
            .send_due(&mut store, &setup.sync)
            .await
            .unwrap()
            .rows
            .is_empty(),
        "nothing on start"
    );
    let error = setup.outbox.send(&mut store, &setup.sync, row.id).await.unwrap_err();
    assert!(
        matches!(error, OutboxError::OtherRun { id, .. } if id == row.id),
        "{error:?}"
    );
    assert!(setup.posted().await.is_empty());

    let left = store.outbox_left_by_earlier_runs().unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, row.id);

    let adopted = store.resend(row.id).unwrap();
    assert_eq!(adopted.client_message_id, row.client_message_id);
    let sent = setup.outbox.send_due(&mut store, &setup.sync).await.unwrap().rows;

    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].state, OutboxState::Sent);
    assert_eq!(setup.posted().await, std::slice::from_ref(&row.client_message_id));
    assert!(
        setup.sync.calls().is_empty(),
        "it had never been attempted: nothing to look for"
    );
}

#[tokio::test]
async fn a_row_an_earlier_run_left_in_flight_whose_message_arrived_isnt_sent_again_on_resend() {
    let setup = setup().await;
    setup.created().await;
    let row = {
        let mut earlier = setup.open_store();
        let row = enqueue(&mut earlier, &hello()).unwrap();
        // the earlier process got as far as sending and ended before the answer
        earlier.begin_attempt(row.id).unwrap();
        row
    };
    setup.sync.seed(&row.client_message_id);
    let mut store = setup.open_store();

    store.resend(row.id).unwrap();
    let after = send(&setup, &mut store, &row).await;

    assert_eq!(after.state, OutboxState::Sent);
    assert_eq!(after.message_id.as_deref(), Some("2000"));
    assert!(setup.posted().await.is_empty(), "found by the sync: not sent twice");
    assert_eq!(setup.sync.calls(), [0]);
}

#[tokio::test]
async fn a_row_an_earlier_run_left_in_flight_that_didnt_arrive_goes_out_with_its_client_message_id() {
    let setup = setup().await;
    setup.created().await;
    let row = {
        let mut earlier = setup.open_store();
        let row = enqueue(&mut earlier, &hello()).unwrap();
        earlier.begin_attempt(row.id).unwrap();
        row
    };
    let mut store = setup.open_store();

    store.resend(row.id).unwrap();
    let after = send(&setup, &mut store, &row).await;

    assert_eq!(after.state, OutboxState::Sent);
    assert_eq!(after.attempts, 2);
    assert_eq!(after.attempts_since_resend(), 1);
    assert_eq!(setup.sync.calls(), [0], "looked for first");
    assert_eq!(setup.posted().await, std::slice::from_ref(&row.client_message_id));
}

#[tokio::test]
async fn a_row_given_up_and_resent_gets_its_attempts_again() {
    let setup = setup().await;
    setup.answer(429).await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();
    let mut last = send(&setup, &mut store, &row).await;
    while last.state == OutboxState::Pending {
        setup.advance(Duration::from_secs(60));
        last = send(&setup, &mut store, &row).await;
    }
    assert_eq!((last.state, last.attempts), (OutboxState::Failed, MAX_ATTEMPTS));

    let resent = store.resend(row.id).unwrap();
    assert_eq!((resent.attempts, resent.attempts_since_resend()), (MAX_ATTEMPTS, 0));
    let waiting = send(&setup, &mut store, &row).await;

    assert_eq!(
        waiting.state,
        OutboxState::Pending,
        "a 429 on the resend waits: {waiting:?}"
    );
    assert_eq!(waiting.not_before, Some(setup.now() + millis(DEFAULT_RETRY_WAIT)));
    assert_eq!(waiting.attempts_since_resend(), 1);
    assert_eq!(
        setup.sync.calls().len(),
        usize::try_from(MAX_ATTEMPTS).unwrap(),
        "the resent row, attempted before, was looked for first"
    );
    assert_eq!(setup.posted().await.len(), usize::try_from(MAX_ATTEMPTS + 1).unwrap());
}

#[tokio::test]
async fn another_runs_pending_row_is_refused_even_while_it_waits_for_its_retry_after() {
    let setup = setup().await;
    let row = {
        let mut earlier = setup.open_store();
        let row = enqueue(&mut earlier, &hello()).unwrap();
        earlier.begin_attempt(row.id).unwrap();
        let retry = AttemptOutcome::Retry {
            not_before: START + 60_000,
            error: "HTTP 429".to_owned(),
        };
        earlier.finish_attempt(row.id, &retry).unwrap();
        row
    };
    let mut store = setup.open_store();

    let error = setup.outbox.send(&mut store, &setup.sync, row.id).await.unwrap_err();

    assert!(
        matches!(error, OutboxError::OtherRun { id, .. } if id == row.id),
        "{error:?}"
    );
}

// --- transient writes wait once, and briefly ---

fn typing() -> Transient {
    Transient::Typing {
        conversation: CHAT.to_owned(),
    }
}

#[tokio::test]
async fn a_typing_throttled_for_longer_than_it_is_worth_is_dropped_at_once() {
    let setup = setup().await;
    setup.answer(201).await;
    let longest = MAX_TRANSIENT_WAIT.as_secs();
    setup.answer_once(429, Some(&(longest + 1).to_string())).await;

    let sent = setup.outbox.send_transient(typing()).await;

    assert!(
        matches!(&sent, TransientSent::Dropped { reason } if reason.contains("longer than")),
        "{sent:?}"
    );
    setup.advance(Duration::from_secs(longest + 2));
    assert!(setup.outbox.send_due_transients().await.is_empty(), "nothing waits");
    assert_eq!(setup.posted().await.len(), 1, "only the throttled attempt");
}

#[tokio::test]
async fn a_typing_throttled_again_after_its_wait_is_dropped_not_kept_waiting() {
    let setup = setup().await;
    setup.answer(201).await;
    setup.answer_once(429, Some("1")).await;
    setup.answer_once(503, Some("1")).await;

    let sent = setup.outbox.send_transient(typing()).await;
    assert_eq!(
        sent,
        TransientSent::Waiting {
            not_before: setup.now() + 1000
        }
    );
    assert!(setup.outbox.send_due_transients().await.is_empty(), "not yet due");
    setup.advance(Duration::from_secs(1));

    let due = setup.outbox.send_due_transients().await;
    assert_eq!(
        due,
        [(
            typing(),
            TransientSent::Dropped {
                reason: "throttled again after waiting once".to_owned()
            }
        )]
    );
    setup.advance(Duration::from_secs(5));
    assert!(setup.outbox.send_due_transients().await.is_empty(), "nothing waits");
    assert_eq!(setup.posted().await.len(), 2);
}

#[tokio::test]
async fn a_waiting_typing_is_dropped_when_its_conversation_is_named() {
    let setup = setup().await;
    setup.answer(201).await;
    setup.answer_once(429, Some("1")).await;
    assert!(matches!(
        setup.outbox.send_transient(typing()).await,
        TransientSent::Waiting { .. }
    ));

    assert!(setup.outbox.drop_waiting("19:another@thread.v2").is_empty());
    let dropped = setup.outbox.drop_waiting(CHAT);
    assert!(
        matches!(dropped.as_slice(), [(transient, TransientSent::Dropped { .. })] if *transient == typing()),
        "{dropped:?}"
    );
    setup.advance(Duration::from_secs(2));
    assert!(setup.outbox.send_due_transients().await.is_empty());
    assert_eq!(setup.posted().await.len(), 1, "only the throttled attempt");
}

// --- the engine's runtime ---

#[test]
fn sending_is_a_future_the_engine_can_spawn() {
    fn spawnable<F: Future + Send>(_: &F) {}
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let setup = runtime.block_on(setup());
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();
    let sending = setup.outbox.send(&mut store, &setup.sync, row.id);
    spawnable(&sending);
    drop(sending);
}

#[tokio::test]
async fn activity_turn_off_app_never_repeats_an_unknown_write() {
    let setup = setup().await;
    let mut store = setup.open_store();
    let properties = "/api/chatsvc/amer/v1/users/ME/properties";
    Mock::given(method("GET"))
        .and(path(properties))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&setup.server)
        .await;
    Mock::given(method("PUT"))
        .and(path(properties))
        .respond_with(ResponseTemplate::new(200))
        .mount(&setup.server)
        .await;
    setup.fault(Fault::LoseAnswer);
    let command = Command::ActivityTurnOffApp {
        app: "made-up-app".into(),
        activity_types: vec!["customNotification".into()],
    };
    let queued = enqueue(&mut store, &command).unwrap();
    let row = setup.outbox.send(&mut store, &setup.sync, queued.id).await.unwrap();
    assert_eq!(row.state, OutboxState::Failed);
    setup.outbox.send_due(&mut store, &setup.sync).await.unwrap();
    let requests = setup.server.received_requests().await.unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method.as_str() == "PUT")
            .count(),
        1
    );
    assert!(setup.sync.calls().is_empty());
}

#[tokio::test]
async fn activity_turn_off_app_does_not_overwrite_unreadable_preferences() {
    for answer in [
        ResponseTemplate::new(403),
        ResponseTemplate::new(404),
        ResponseTemplate::new(404)
            .set_body_json(json!({"errorCode": 404, "standardizedError": {"errorCode": 404, "errorSubCode": 2}})),
        ResponseTemplate::new(200).set_body_json(json!({"userGraphSettings": "[]"})),
    ] {
        let setup = setup().await;
        let mut store = setup.open_store();
        Mock::given(method("GET"))
            .and(path("/api/chatsvc/amer/v1/users/ME/properties"))
            .respond_with(answer)
            .mount(&setup.server)
            .await;
        let row = enqueue(
            &mut store,
            &Command::ActivityTurnOffApp {
                app: "made-up-app".into(),
                activity_types: vec![],
            },
        )
        .unwrap();
        let row = setup.outbox.send(&mut store, &setup.sync, row.id).await.unwrap();
        assert_eq!(row.state, OutboxState::Failed);
        assert!(
            setup
                .server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|request| request.method.as_str() == "GET")
        );
    }
}
