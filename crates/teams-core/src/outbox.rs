//! The outbox: every write the user asks for goes out from here, and from nowhere else.
//!
//! - **A command becomes a row** ([`enqueue`]): a [`Command`] is stored as the row's payload under a fresh
//!   `clientmessageid` ([`new_client_message_id`]), which the row keeps across every attempt.
//! - **Each attempt sends its typed request** ([`Outbox::send`]); transport failures and HTTP statuses remain distinct.
//! - **The outbox's part of the retry table** (the plan's Technical Details): a 2xx is sent; a 401 is tried once more
//!   at once (the transport has had the session refresh the refused token), once in the row's budget: a later 401
//!   fails the row, whatever came between; a 429 or 503 waits its `Retry-After`
//!   ([`DEFAULT_RETRY_WAIT`] without one) as the row's `not_before`; any other 4xx (and any other answer) fails the
//!   row, never retried. A network error or a timeout, and a 5xx other than 503, leave it unknown whether the message
//!   arrived: before the next attempt the conversation is synced ([`ConversationSync`]) and searched for the row's
//!   `clientmessageid`; found, the row is sent and nothing goes out again; not found, a new attempt goes out with the
//!   same `clientmessageid` (whether the service dedupes by it is unverified, so the check comes first). Every
//!   attempt after the first starts with that check, whatever ended the one before. A check that can't sync sends
//!   nothing: the row waits [`CHECK_RETRY_WAIT`], doubled for each check of its that failed before. A row is given up
//!   (failed) after [`MAX_ATTEMPTS`] attempts or [`MAX_FAILED_CHECKS`] checks that couldn't sync, both counted since
//!   it was made or last resent.
//! - **An entry no read can take that carries a row's `clientmessageid`** ([`Outbox::unreadable_seen`]): any read
//!   (an ordinary catch-up as much as a check) that meets one for a message post of its conversation that had an
//!   attempt records it on the row and moves on, so the conversation's sync is never held up by it. Its message may
//!   have arrived, and once the read moved past the entry no later sync tells, so every check of the row fails
//!   ([`UnreadableSeen`]) as one that couldn't sync does, until it is given up at [`MAX_FAILED_CHECKS`]: it isn't sent
//!   again on its own, only by an explicit resend ([`OutboxEntry::may_have_arrived`] says so to its owner first). Our
//!   message coming back readable after all still settles it.
//! - **Writes on one thing go out in the order they were asked for** ([`Lane`]): a row waits, sending nothing, while
//!   an older pending row of this run acts on the same message (an edit, a delete, a reaction or taking it back) or,
//!   for a read receipt, on the same conversation's read horizon; so a take-back sent while its reaction waits out a
//!   `Retry-After` can't be undone by the reaction going out after it, nor a newer edit by an older one. Rows on
//!   anything else go out meanwhile; a message post and a new chat wait for nothing.
//! - **Our own message coming back** ([`Outbox::receive`], for a Trouter delivery and a sync alike) is stored once
//!   (by its id and version) and matched by its `clientmessageid` to its row, which becomes sent with the message's
//!   id; it may come back before the post's answer does, and the answer then changes nothing. A row a message coming
//!   in changed is also kept for [`Outbox::take_arrived`], so that the engine tells its owner of it whichever read
//!   (a sync page, a delivery, the check of another row) brought the message.
//! - **No resend across runs**: a run sends only its own rows ([`teams_store::Store::outbox_due`]); rows an earlier
//!   process left pending or in flight are listed by [`teams_store::Store::outbox_left_by_earlier_runs`] and go out
//!   again only after an explicit [`teams_store::Store::resend`], which keeps their `clientmessageid` and starts their
//!   budget over. An adopted row that had an attempt (in any run) begins with the sync check above, so a message an
//!   earlier attempt got through isn't sent twice.
//!
//! - **Transient writes** ([`Transient`], [`Outbox::send_transient`]): typing goes out from here too, never as a row: nothing of it is stored, checked for, retried after an unknown outcome or
//!   sent by a later run; a throttled one waits in memory, once and briefly ([`MAX_TRANSIENT_WAIT`]), the newest of
//!   the same takes its place, and a message post to its conversation drops it ([`Outbox::drop_waiting`]).
//! - **Call writes** ([`Outbox::try_call`], M3 Task 14): a call's writes go out from here as a kind of their own, no
//!   row and no transient: each attempt tracked around the actual send, its answer (any status, with its body) back to the call
//!   machine, another attempt only after a 401 or a short throttle ([`CallRetries`]), never after an unknown outcome.
//!
//! The commands are the chat writes of `teams-proto` (M1 Task 12): a message post, an edit, a soft delete, a reaction
//! and taking it back, a read receipt, a new chat and people added to a group chat; the engine (M1 Task 18) builds them from what the user asks
//! for. Only a message post can be looked for by its `clientmessageid`, so only its row is checked and tried again
//! after an attempt whose outcome is unknown; any other such row is failed rather than risk doing it twice. The sync
//! the check relies on is the [`ConversationSync`] seam, which M1 Task 17's catch-up sync fills.

mod call;
mod transient;

pub use call::{CallRetries, CallTried};
pub use transient::{MAX_TRANSIENT_WAIT, Transient, TransientSent};

use std::collections::BTreeMap;
use std::error::Error;
use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::CallFloor;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use teams_auth::token::TokenError;
use teams_call::Tracker;
use teams_proto::Mri;
use teams_proto::chat::MessageText;
use teams_proto::{
    ACTIVITY_FEED, AddChatMembers, Author, CreateChat, DEFAULT_RETRY_WAIT, DeleteMessage, EditMessage, HttpError,
    MarkFeedRead, Original, ReadReceipt, RemoveReaction, Request, Response, SendMessage, SetActivityRead, SetReaction,
    SharedHistory, Transport,
};
use teams_store::{
    Applied, AttemptOutcome, CheckFailed, Message, NewOutboxEntry, OutboxEntry, OutboxKind, OutboxState, Reaction,
    Store, StoreError,
};

/// The most attempts a row gets since it was made or last resent ([`Store::resend`]), throttled ones included; after
/// the last it is failed.
pub const MAX_ATTEMPTS: u32 = 5;

/// The most checks before a new attempt that can fail to sync since a row was made or last resent; at the last the row
/// is failed, since whether its earlier attempt arrived can't be told.
pub const MAX_FAILED_CHECKS: u32 = 5;

/// How long a row waits after its first check that couldn't sync; each later one doubles it.
pub const CHECK_RETRY_WAIT: Duration = Duration::from_secs(30);

/// How many fresh `clientmessageid`s a command is tried with when one is taken already.
const CLIENT_MESSAGE_ID_TRIES: usize = 3;

/// What a row's `last_error` says when an attempt may have arrived.
const OUTCOME_UNKNOWN: &str = "whether it arrived is unknown";

/// Whether an unsent row's last attempt may have arrived: one begun by an earlier run and never finished, one whose
/// answer was lost, or one given up after a read met an entry it couldn't read that carries its `clientmessageid`
/// ([`OutboxEntry::may_have_arrived`]). Such a row is not to be done again as a new command either.
#[must_use]
pub fn outcome_unknown(row: &OutboxEntry) -> bool {
    match row.state {
        OutboxState::Sending => true,
        OutboxState::Failed => {
            row.may_have_arrived()
                || row
                    .last_error
                    .as_deref()
                    .is_some_and(|error| error.contains(OUTCOME_UNKNOWN))
        }
        OutboxState::Pending | OutboxState::Sent | OutboxState::Denied => false,
    }
}

/// What the user asked for, as the outbox stores it (the row's payload, JSON). Each attempt builds its sealed request
/// from it, with the row's `created_at` as the time the request carries (a post's compose time, an edit's time, a
/// reaction's or a read receipt's), so every attempt sends the same request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    /// Posts a message to an existing chat: its text, from which the request builds its HTML, its mentions and its
    /// target.
    Send { conversation: String, text: MessageText },
    /// Edits one of the account's messages: its new text, and what the edit sends again of the message as it was
    /// sent.
    Edit {
        conversation: String,
        message_id: String,
        text: MessageText,
        original: Sent,
    },
    /// Soft-deletes one of the account's messages.
    Delete { conversation: String, message_id: String },
    /// Sets (`on`) or takes back the account's reaction `key` on a message.
    React {
        conversation: String,
        message_id: String,
        key: String,
        on: bool,
    },
    /// Moves the account's read horizon to a message (sent with `client_message_id`).
    ReadReceipt {
        conversation: String,
        message_id: String,
        client_message_id: String,
    },
    /// Creates a chat of the account and `others` (MRIs): their 1:1 chat for one, a new group chat for two or more,
    /// named `topic` when it has one (absent from the rows of builds before names).
    CreateChat {
        others: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        topic: Option<String>,
    },
    /// Adds `members` (MRIs) to the group chat `conversation`, sharing its history from `share_history_time` as
    /// [`SharedHistory`] reads it (worked out when the user asked, so every attempt sends the same).
    AddMembers {
        conversation: String,
        members: Vec<String>,
        share_history_time: i64,
    },
    /// Marks an item of the account's activity feed ([`ACTIVITY_FEED`]) read or unread.
    ActivityRead { item: String, read: bool },
    /// Marks the whole activity feed read as of the row's time ("Mark all as read").
    FeedRead,
    /// Turns off the app's activity types, merged with a fresh own-properties read on each attempt.
    ActivityTurnOffApp { app: String, activity_types: Vec<String> },
    /// Restores the app preference saved for a confirmed Turn off.
    ActivityRestoreApp {
        app: String,
        preference: teams_proto::AppNotificationPreference,
    },
}

/// What commands go out in the order they were asked for on: a later one of this run waits while an earlier one on
/// the same lane is pending, since sent out of order the earlier would undo the later (a reaction coming back after
/// its take-back, an older edit over a newer one, the read horizon moving back).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane<'a> {
    /// The complete account app notification setting.
    ActivitySettings,
    /// A message's edits, its delete, and the account's reactions on it.
    Message { conversation: &'a str, message_id: &'a str },
    /// The account's read horizon in a conversation.
    ReadHorizon { conversation: &'a str },
}

/// What an edit sends again of the message it changes, as the store had it when the edit was asked for: its
/// `clientmessageid`, times in milliseconds since the Unix epoch, and raw attachment properties.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sent {
    pub client_message_id: String,
    pub compose_time_ms: i64,
    pub arrival_time_ms: i64,
    /// Original raw `files`/`cards`, captured from the store rather than new upload staging. `None` denotes an
    /// older queued edit that must be upgraded before delivery; `Some({})` means neither property was present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_properties: Option<Map<String, Value>>,
}

/// The attachment metadata that a text edit must keep, without parsing or rebuilding any entries. Invalid outer
/// properties cannot establish that attachments are absent; unknown inner values can still be preserved.
pub(crate) fn attachment_properties(message: &Message) -> Result<Map<String, Value>, serde_json::Error> {
    let properties: Map<String, Value> = message
        .properties
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_default();
    Ok(properties
        .into_iter()
        .filter(|(key, _)| matches!(key.as_str(), "files" | "cards"))
        .collect())
}

impl Command {
    pub(crate) fn for_cloud(self, cloud: &teams_proto::Cloud) -> Result<Self, teams_proto::chat::ComposeError> {
        Ok(match self {
            Self::Send { conversation, text } => Self::Send {
                conversation,
                text: text.for_cloud(cloud)?,
            },
            Self::Edit {
                conversation,
                message_id,
                text,
                original,
            } => Self::Edit {
                conversation,
                message_id,
                text: text.for_cloud(cloud)?,
                original,
            },
            other => other,
        })
    }

    /// The row's kind.
    #[must_use]
    pub fn kind(&self) -> OutboxKind {
        match self {
            Self::Send { .. } => OutboxKind::Send,
            Self::Edit { .. } => OutboxKind::Edit,
            Self::Delete { .. } => OutboxKind::Delete,
            Self::React { on: true, .. } => OutboxKind::AddReaction,
            Self::React { on: false, .. } => OutboxKind::RemoveReaction,
            Self::ReadReceipt { .. } => OutboxKind::ReadReceipt,
            Self::CreateChat { .. } => OutboxKind::CreateChat,
            Self::AddMembers { .. } => OutboxKind::AddMembers,
            Self::ActivityRead { .. } => OutboxKind::ActivityRead,
            Self::FeedRead => OutboxKind::FeedRead,
            Self::ActivityTurnOffApp { .. } => OutboxKind::ActivityTurnOffApp,
            Self::ActivityRestoreApp { .. } => OutboxKind::ActivityRestoreApp,
        }
    }

    /// The conversation it writes to; none for a chat that doesn't exist yet.
    #[must_use]
    pub fn conversation(&self) -> Option<&str> {
        match self {
            Self::Send { conversation, .. }
            | Self::Edit { conversation, .. }
            | Self::Delete { conversation, .. }
            | Self::React { conversation, .. }
            | Self::ReadReceipt { conversation, .. }
            | Self::AddMembers { conversation, .. } => Some(conversation),
            Self::ActivityRead { .. } | Self::FeedRead => Some(ACTIVITY_FEED),
            Self::CreateChat { .. } | Self::ActivityTurnOffApp { .. } | Self::ActivityRestoreApp { .. } => None,
        }
    }

    /// What it has to go out in order on with the other commands of this run ([`Lane`]); none for a message post, a
    /// new chat and people added.
    #[must_use]
    pub fn lane(&self) -> Option<Lane<'_>> {
        match self {
            Self::Edit {
                conversation,
                message_id,
                ..
            }
            | Self::Delete {
                conversation,
                message_id,
            }
            | Self::React {
                conversation,
                message_id,
                ..
            } => Some(Lane::Message {
                conversation,
                message_id,
            }),
            Self::ReadReceipt { conversation, .. } => Some(Lane::ReadHorizon { conversation }),
            Self::ActivityRead { item, .. } => Some(Lane::Message {
                conversation: ACTIVITY_FEED,
                message_id: item,
            }),
            Self::ActivityTurnOffApp { .. } | Self::ActivityRestoreApp { .. } => Some(Lane::ActivitySettings),
            Self::FeedRead => Some(Lane::ReadHorizon {
                conversation: ACTIVITY_FEED,
            }),
            Self::Send { .. } | Self::CreateChat { .. } | Self::AddMembers { .. } => None,
        }
    }

    /// The existing message it acts on: an edit's, a delete's, a reaction's, a read receipt's.
    #[must_use]
    pub fn message_id(&self) -> Option<&str> {
        match self {
            Self::Edit { message_id, .. }
            | Self::Delete { message_id, .. }
            | Self::React { message_id, .. }
            | Self::ReadReceipt { message_id, .. } => Some(message_id),
            Self::ActivityRead { item, .. } => Some(item),
            Self::Send { .. }
            | Self::CreateChat { .. }
            | Self::AddMembers { .. }
            | Self::FeedRead
            | Self::ActivityTurnOffApp { .. }
            | Self::ActivityRestoreApp { .. } => None,
        }
    }
}

/// The request a new chat of `me` and `others` is: their 1:1 chat for one other, a group chat for more, named `topic`
/// when it is a group ([`CreateChat::named`]); `None` when `others` don't make a chat (none, `me` among them, one
/// given twice).
#[must_use]
pub fn create_chat(me: &Mri, others: &[String], topic: Option<&str>) -> Option<CreateChat> {
    let request = match others {
        [] => None,
        [other] => CreateChat::one_to_one(me.clone(), Mri::new(other)),
        _ => CreateChat::group(me.clone(), others.iter().map(Mri::new)),
    }?;
    Some(match topic {
        Some(topic) => request.named(topic),
        None => request,
    })
}

/// Why a [`ConversationSync`] couldn't sync.
pub type SyncError = Box<dyn Error + Send + Sync>;

/// Syncs one conversation from the service, for the outbox's check after an attempt whose outcome is unknown. M1 Task
/// 17's catch-up sync implements it.
pub trait ConversationSync: Sync {
    /// Serves local queries and seen-message updates while transport waits. The engine's catch-up delegates to its
    /// inbox; a standalone outbox has nothing to serve and leaves this pending.
    fn serve<'a>(&'a self, _store: &'a mut Store) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(std::future::pending())
    }

    /// Reads `conversation`'s messages from the service now, never from the store alone, and stores what changed
    /// (through [`Outbox::receive`], or [`Store::upsert_message`]), so that a message we sent that reached the service
    /// is in `store` once it returns.
    fn sync_conversation<'a>(
        &'a self,
        store: &'a mut Store,
        conversation: &'a str,
    ) -> impl Future<Output = Result<(), SyncError>> + Send + 'a;

    /// As [`ConversationSync::sync_conversation`], for a message we sent no earlier than `since` (milliseconds since
    /// the Unix epoch: the row's `created_at`); the outbox's check calls this one. A sync that reads the newest
    /// messages a page at a time reads back until it reaches one older than `since`, and fails when it can't, so a
    /// message we sent is never missed for being more than a page back. By default it is
    /// [`ConversationSync::sync_conversation`], for a sync that reads everything at once.
    fn sync_conversation_since<'a>(
        &'a self,
        store: &'a mut Store,
        conversation: &'a str,
        _since: i64,
    ) -> impl Future<Output = Result<(), SyncError>> + Send + 'a {
        self.sync_conversation(store, conversation)
    }
}

/// The time in milliseconds since the Unix epoch, as the outbox reads it for `not_before`.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// What the outbox can fail with.
#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The check before a new attempt couldn't tell whether the last one arrived: its conversation couldn't be synced
    /// (a [`SyncError`] of the sync), or a read met an entry it couldn't read carrying the row's `clientmessageid`
    /// ([`UnreadableSeen`]). Nothing was sent, and the row waits (or, at [`MAX_FAILED_CHECKS`], is failed).
    #[error("the check couldn't tell whether outbox row {id} arrived, so it wasn't sent again: {source}")]
    Sync {
        id: i64,
        #[source]
        source: SyncError,
    },
    /// A pending row of another run: only an explicit [`Store::resend`] makes it this run's.
    #[error("outbox row {id} belongs to run {run_id}, not this one: only an explicit resend sends it")]
    OtherRun { id: i64, run_id: i64 },
    #[error("no random source for a clientmessageid: {0}")]
    Random(getrandom::Error),
    #[error("the command can't be stored: {0}")]
    Payload(#[source] serde_json::Error),
}

/// A check before a new attempt couldn't tell whether the row's message arrived because a read met an entry it couldn't
/// read at all carrying the row's `clientmessageid` ([`Outbox::unreadable_seen`]): the row isn't sent again on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a read met an entry it couldn't read carrying its clientmessageid, so it may have arrived")]
pub struct UnreadableSeen;

/// What one [`Outbox::send_due`] did.
#[derive(Debug, Default)]
pub struct SentDue {
    /// Every due row as it ended up, oldest first; one whose check couldn't tell whether it arrived is here too,
    /// waiting or failed.
    pub rows: Vec<OutboxEntry>,
    /// The rows whose check before a new attempt couldn't tell whether they arrived ([`OutboxError::Sync`]: a sync
    /// that failed, or [`UnreadableSeen`]), so nothing was sent for them.
    pub unsynced: Vec<Unsynced>,
}

/// An [`Outbox::send_due`] the store failed during: the rows done before it, and the error.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct DueFailed {
    /// The rows sent (or left waiting, or failed) before the store failed, as in [`SentDue`].
    pub done: SentDue,
    #[source]
    pub error: OutboxError,
}

/// A row whose check before a new attempt couldn't tell whether it arrived: why, as [`OutboxError::Sync`] gives it (a
/// [`SyncError`] of the sync, or [`UnreadableSeen`]).
#[derive(Debug)]
pub struct Unsynced {
    pub id: i64,
    pub source: SyncError,
}

/// A message stored by [`Outbox::receive`]: what the store did with it, and the outbox row it was matched to (ours).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Received {
    pub applied: Applied,
    pub outbox: Option<OutboxEntry>,
}

/// What a message that came in is matched to its outbox row by ([`Outbox::match_arrival`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arrival<'a> {
    /// The conversation it came in.
    pub conversation: &'a str,
    /// The service's id for it.
    pub id: &'a str,
    pub client_message_id: Option<&'a str>,
    /// Its sender's MRI.
    pub sender: Option<&'a str>,
}

/// A fresh `clientmessageid`: a random 19-digit number, as the web client makes them.
///
/// # Errors
///
/// [`OutboxError::Random`] when the system random source fails.
pub fn new_client_message_id() -> Result<String, OutboxError> {
    const LOWEST: u64 = 1_000_000_000_000_000_000;
    let random = getrandom::u64().map_err(OutboxError::Random)?;
    Ok((LOWEST + random % (9 * LOWEST)).to_string())
}

/// Puts `command` in the outbox as a pending row of this run, with a fresh `clientmessageid`.
///
/// # Errors
///
/// [`OutboxError::Payload`] when the command can't be serialized, [`OutboxError::Random`] without a random source,
/// [`OutboxError::Store`] when the row can't be written.
pub fn enqueue(store: &mut Store, command: &Command) -> Result<OutboxEntry, OutboxError> {
    let payload = serde_json::to_string(command).map_err(OutboxError::Payload)?;
    let mut tries = 1;
    loop {
        let new = NewOutboxEntry {
            conversation_id: command.conversation().map(str::to_owned),
            kind: command.kind(),
            client_message_id: new_client_message_id()?,
            payload: payload.clone(),
        };
        match store.enqueue(&new) {
            Err(StoreError::DuplicateClientMessageId(_)) if tries < CLIENT_MESSAGE_ID_TRIES => tries += 1,
            stored => return Ok(stored?),
        }
    }
}

/// How one attempt went.
enum Tried {
    /// The service answered.
    Answered(Response),
    Denied(String),
    /// Nothing went out, and nothing will: the row is failed.
    NotSent(String),
    /// Nothing went out yet: try again after `wait`.
    Throttled {
        wait: Duration,
        error: String,
    },
    /// The request may or may not have reached the service.
    Uncertain(String),
}

impl Tried {
    fn summary(&self) -> String {
        match self {
            Self::Answered(answer) => format!("admitted, HTTP {}", answer.status()),
            Self::Denied(denied) => format!("call admission refused: {denied}"),
            Self::NotSent(why) => format!("not sent: {why}"),
            Self::Throttled { wait, .. } => format!("throttled for {} ms", wait.as_millis()),
            Self::Uncertain(why) => format!("outcome unknown: {why}"),
        }
    }
}

/// Sends the outbox's rows over a transport, tracking each actual attempt (see the module docs).
pub struct Outbox<T> {
    transport: Arc<T>,
    tracker: Arc<Tracker>,
    floor: CallFloor,
    /// The account: its messages go out as its, and only its messages are matched to outbox rows.
    me: Author,
    clock: Clock,
    /// The rows a read changed, until they are taken: by a message of ours coming in ([`Outbox::match_arrival`]), or
    /// by an entry it couldn't read carrying a row's `clientmessageid` ([`Outbox::unreadable_seen`]). Each row is kept
    /// once, as the outbox last left it: a later read that changes it replaces its copy ([`Outbox::keep_arrived`]), and
    /// so does a check that changes it afterwards ([`Outbox::check_failed`]).
    arrived: Mutex<Vec<OutboxEntry>>,
    /// The rows that had their one more attempt after a 401, each with its `attempts_at_resend` then: another 401
    /// within the same budget fails the row, whatever came between (other answers, a check that couldn't sync, more
    /// [`Outbox::send`] calls); a resend starts a new budget, and with it a new allowance.
    refreshed: Mutex<BTreeMap<i64, u32>>,
    /// The transients waiting for their `Retry-After`, one of each kind and target ([`Transient::replaces`]); kept
    /// in memory only.
    waiting: Mutex<Vec<transient::Waiting>>,
    activity_undo: Mutex<BTreeMap<i64, (String, teams_proto::AppNotificationPreference)>>,
}

impl<T: Transport<Error = HttpError>> Outbox<T> {
    #[must_use]
    pub fn new(transport: Arc<T>, me: Author) -> Self {
        Self {
            transport,
            tracker: Arc::new(Tracker::new(me.mri().clone())),
            floor: CallFloor::new(),
            me,
            clock: Arc::new(now_ms),
            arrived: Mutex::new(Vec::new()),
            refreshed: Mutex::new(BTreeMap::new()),
            waiting: Mutex::new(Vec::new()),
            activity_undo: Mutex::new(BTreeMap::new()),
        }
    }

    /// Another clock (tests: a `Retry-After` without waiting for it).
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// The account the outbox writes as.
    #[must_use]
    pub fn me(&self) -> &Author {
        &self.me
    }

    /// Issues one Power Automate mutation, never queued or replayed across runs.
    ///
    /// # Errors
    /// Returns the transport's authentication, service, schema or read-only refusal.
    pub async fn try_workflow(&self, request: &teams_proto::WorkflowRequest) -> teams_proto::WorkflowResponse {
        self.transport.workflow(request).await
    }

    pub(crate) fn cloud(&self) -> &teams_proto::Cloud {
        self.transport.cloud()
    }

    /// Whether the outbox sends nothing (its transport is read-only: the personal build's `ngteams --read-only`): every
    /// write it is asked for, a row's, a transient's or a call's, is refused before sending and logged as
    /// `refused_read_only`; a row is failed, a transient dropped, a call write not sent.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.transport.is_read_only()
    }

    #[must_use]
    pub fn tracker(&self) -> &Arc<Tracker> {
        &self.tracker
    }

    /// Shares this account's tracking and cross-account call floor with its calling socket.
    #[must_use]
    pub fn with_calls(mut self, tracker: Arc<Tracker>, floor: CallFloor) -> Self {
        floor.join(&tracker);
        self.tracker = tracker;
        self.floor = floor;
        self
    }

    /// The rows a read changed since the last call, oldest first, each as the outbox last left it: a row that became
    /// sent (or had its message id filled in) by [`Outbox::match_arrival`], whichever read brought the message (a sync
    /// page, a delivery, the check before another row's attempt), and a pending row a read met an entry it couldn't
    /// read for ([`Outbox::unreadable_seen`]), which stays pending. Each row is here once: one that several reads
    /// changed is here as the last of them left it, and one whose own check changed it after the read as that check
    /// left it, never as the read did. A row a send returns may be among them too. They are kept
    /// until taken: the engine takes them after every read it makes.
    pub fn take_arrived(&self) -> Vec<OutboxEntry> {
        std::mem::take(&mut *self.arrived.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Sends every row of this run that is due, oldest first, as [`Outbox::send`] does; returns the rows as they
    /// end up. A row whose check can't sync doesn't hold up the rows after it: it is listed in
    /// [`SentDue::unsynced`] and waits (see the module docs). A row that waits behind an older one on its lane
    /// ([`Lane`]) is left out: nothing happened to it.
    ///
    /// # Errors
    ///
    /// [`DueFailed`] when the store fails, with the rows done before it; the rows after it wait for the next call.
    pub async fn send_due<S: ConversationSync>(&self, store: &mut Store, sync: &S) -> Result<SentDue, DueFailed> {
        let mut done = SentDue::default();
        match self.send_due_into(store, sync, &mut done).await {
            Ok(()) => Ok(done),
            Err(error) => Err(DueFailed { done, error }),
        }
    }

    async fn send_due_into<S: ConversationSync>(
        &self,
        store: &mut Store,
        sync: &S,
        done: &mut SentDue,
    ) -> Result<(), OutboxError> {
        let due = store.outbox_due((self.clock)())?;
        done.rows.reserve(due.len());
        for row in due {
            if waits_behind(store, &row)? {
                continue;
            }
            match self.send(store, sync, row.id).await {
                Ok(row) => done.rows.push(row),
                Err(OutboxError::Sync { id, source }) => {
                    done.rows.push(entry(store, id)?);
                    done.unsynced.push(Unsynced { id, source });
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Sends one row of this run, if it is pending and due: attempts until it is sent, denied or failed, or waits
    /// for a `Retry-After` (see the module docs). A row of this run that isn't due, or waits behind an older pending
    /// row on its lane ([`Lane`]), and a row of any run that isn't pending, comes back as it is.
    ///
    /// # Errors
    ///
    /// [`OutboxError::OtherRun`] for a pending row of another run, due or not (nothing is sent),
    /// [`OutboxError::Sync`] when the check before a new attempt can't tell whether the last one arrived (its sync
    /// failed, or [`UnreadableSeen`]; nothing is sent, the row waits or, at [`MAX_FAILED_CHECKS`], is failed),
    /// [`OutboxError::Store`] when the store fails.
    pub async fn send<S: ConversationSync>(
        &self,
        store: &mut Store,
        sync: &S,
        id: i64,
    ) -> Result<OutboxEntry, OutboxError> {
        loop {
            let row = entry(store, id)?;
            if row.state == OutboxState::Pending && row.run_id != store.run_id() {
                return Err(OutboxError::OtherRun { id, run_id: row.run_id });
            }
            if !self.is_due(&row) || waits_behind(store, &row)? {
                return Ok(row);
            }
            if row.attempts > 0
                && let Some(arrived) = self.look_for_arrival(store, sync, &row).await?
            {
                return Ok(arrived);
            }
            let preparation = self.prepare_edit(store, &row)?;
            let row = store.begin_attempt(id)?;
            let mut attempt = pin!(async {
                match preparation {
                    Ok(()) => self.attempt(&row).await,
                    Err(error) => Tried::NotSent(error),
                }
            });
            let tried = tokio::select! {
                biased;
                tried = &mut attempt => tried,
                () = sync.serve(store) => attempt.await,
            };
            let summary = tried.summary();
            let (outcome, again) = self.settle(&row, tried);
            let row = store.finish_attempt(id, &outcome)?;
            log::info!(
                "outbox row {} ({:?}, clientmessageid {}) attempt {}: {summary}; now {:?}",
                row.id,
                row.kind,
                row.client_message_id,
                row.attempts,
                row.state
            );
            if !again || row.state != OutboxState::Pending {
                return Ok(row);
            }
        }
    }

    /// Stores a message that came in (a Trouter delivery or a sync) with its text for search ([`crate::search`]) and,
    /// when it is ours, matches it by its `clientmessageid` to its row, which becomes sent with the message's id. The
    /// same message coming in again (the other way, or a later version) is stored once, and its row is left as it is.
    ///
    /// # Errors
    ///
    /// [`OutboxError::Store`] when the store fails, its conversation unknown included.
    pub fn receive(
        &self,
        store: &mut Store,
        message: &Message,
        reactions: Option<&[Reaction]>,
    ) -> Result<Received, OutboxError> {
        let applied = store.upsert_message_indexed(message, reactions, crate::search::search_text)?;
        let outbox = self.match_own(store, message)?;
        Ok(Received { applied, outbox })
    }

    fn is_mine(&self, message: &Message) -> bool {
        message.sender.as_deref() == Some(self.me.mri().as_str())
    }

    fn is_due(&self, row: &OutboxEntry) -> bool {
        row.state == OutboxState::Pending && row.not_before.is_none_or(|not_before| not_before <= (self.clock)())
    }

    /// Matches a message that came in to its row without storing it, for one that can't be stored (its version or
    /// its arrival time can't be read); [`Outbox::receive`] matches a stored one the same way. When it is ours, the
    /// message post with its `clientmessageid` in its conversation becomes sent with its id, and is kept for
    /// [`Outbox::take_arrived`]; a row already sent with a message id stays as it is.
    ///
    /// # Errors
    ///
    /// [`OutboxError::Store`] when the store fails.
    pub fn match_arrival(&self, store: &mut Store, arrival: &Arrival<'_>) -> Result<Option<OutboxEntry>, OutboxError> {
        let mine = arrival.sender == Some(self.me.mri().as_str());
        let Some(client_message_id) = arrival.client_message_id.filter(|_| mine) else {
            return Ok(None);
        };
        let Some(row) = store.outbox_by_client_message_id(client_message_id)? else {
            return Ok(None);
        };
        if findable(&row) != Some(arrival.conversation) {
            return Ok(None);
        }
        if row.state == OutboxState::Sent && row.message_id.is_some() {
            return Ok(Some(row));
        }
        let arrived = store.outbox_arrived(client_message_id, arrival.id)?;
        if let Some(row) = &arrived {
            self.keep_arrived(row);
        }
        Ok(arrived)
    }

    /// A read met an entry it couldn't read at all, in `conversation`, carrying `client_message_id`: when that is the
    /// `clientmessageid` of a message post of `conversation` (of any run) that had an attempt and isn't known sent, its
    /// message may have arrived, and the row records it ([`Store::outbox_unreadable_seen`]; see the module docs).
    /// Returns the row when this recorded it, which is also kept for [`Outbox::take_arrived`]. Who sent the entry isn't
    /// inferred, and the row isn't matched from it.
    ///
    /// # Errors
    ///
    /// [`OutboxError::Store`] when the store fails.
    pub fn unreadable_seen(
        &self,
        store: &mut Store,
        conversation: &str,
        client_message_id: &str,
    ) -> Result<Option<OutboxEntry>, OutboxError> {
        let Some(row) = store.outbox_by_client_message_id(client_message_id)? else {
            return Ok(None);
        };
        let may_have_arrived = findable(&row) == Some(conversation)
            && row.state != OutboxState::Sent
            && row.attempts > 0
            && row.unreadable_seen_at.is_none();
        if !may_have_arrived {
            return Ok(None);
        }
        let seen = store.outbox_unreadable_seen(row.id, (self.clock)())?;
        self.keep_arrived(&seen);
        Ok(Some(seen))
    }

    /// The rows of a conversation the store forgot that weren't sent ([`Store::forget_conversation`]: its thread is
    /// gone, so a write to it can never succeed). The store no longer has them, so none goes out or can be resent;
    /// each still to go (waiting, or in flight as an earlier run left it) is kept for [`Outbox::take_arrived`] as
    /// failed, so its owner stops showing it as sending. One already denied or failed was told so before.
    pub(crate) fn forgotten(&self, unsent: Vec<OutboxEntry>) {
        let now = (self.clock)();
        for mut row in unsent {
            if !matches!(row.state, OutboxState::Pending | OutboxState::Sending) {
                continue;
            }
            log::info!(
                "outbox row {} ({:?}, clientmessageid {}) failed: its conversation was deleted",
                row.id,
                row.kind,
                row.client_message_id
            );
            row.state = OutboxState::Failed;
            row.not_before = None;
            row.last_error = Some("its conversation was deleted".to_owned());
            row.updated_at = now;
            let mut arrived = self.arrived.lock().unwrap_or_else(PoisonError::into_inner);
            arrived.retain(|queued| queued.id != row.id);
            arrived.push(row);
        }
    }

    /// The row a message of ours belongs to ([`Outbox::match_arrival`]).
    fn match_own(&self, store: &mut Store, message: &Message) -> Result<Option<OutboxEntry>, OutboxError> {
        let arrival = Arrival {
            conversation: &message.conversation_id,
            id: &message.id,
            client_message_id: message.client_message_id.as_deref(),
            sender: message.sender.as_deref(),
        };
        self.match_arrival(store, &arrival)
    }

    /// Before a new attempt: syncs the row's conversation and looks for its `clientmessageid` among our messages.
    /// Found, the row is sent; `None` when it wasn't found (or a row of its kind can't be looked for). A sync that fails,
    /// or a row a read met an unreadable entry for ([`UnreadableSeen`]), leaves the row waiting its backoff, or fails
    /// it ([`Outbox::check_failed`]), and is returned as [`OutboxError::Sync`].
    async fn look_for_arrival<S: ConversationSync>(
        &self,
        store: &mut Store,
        sync: &S,
        row: &OutboxEntry,
    ) -> Result<Option<OutboxEntry>, OutboxError> {
        let Some(conversation) = findable(row) else {
            return Ok(None);
        };
        if let Err(source) = sync.sync_conversation_since(store, conversation, row.created_at).await {
            let error = format!("the conversation couldn't be synced to look for it ({source}); {OUTCOME_UNKNOWN}");
            let after = self.check_failed(store, row, error)?;
            if after.state == OutboxState::Sent {
                // the sync matched it before it failed
                return Ok(Some(after));
            }
            return Err(OutboxError::Sync { id: row.id, source });
        }
        let current = entry(store, row.id)?;
        if current.state != OutboxState::Pending {
            // the sync's own receive matched it
            return Ok(Some(current));
        }
        if let Some(message) = store.message_by_client_id(conversation, &row.client_message_id)?
            && self.is_mine(&message)
        {
            return Ok(store.outbox_arrived(&row.client_message_id, &message.id)?);
        }
        if current.unreadable_seen_at.is_some() {
            self.check_failed(store, &current, UnreadableSeen.to_string())?;
            return Err(OutboxError::Sync {
                id: row.id,
                source: Box::new(UnreadableSeen),
            });
        }
        Ok(None)
    }

    /// A check that couldn't tell whether the row's message arrived, for the reason `error`: the row waits its
    /// backoff, or is failed at its last check. A copy of the row a read kept for [`Outbox::take_arrived`] on the way
    /// (its sync met an entry it couldn't read) is replaced by the row as this leaves it, so that copy is never told
    /// after it.
    fn check_failed(&self, store: &mut Store, row: &OutboxEntry, error: String) -> Result<OutboxEntry, OutboxError> {
        let failed = row.failed_checks.saturating_add(1);
        let outcome = if failed >= MAX_FAILED_CHECKS {
            CheckFailed::GiveUp {
                error: format!("{error}, so it isn't sent again; given up after {failed} checks"),
            }
        } else {
            let doubled = CHECK_RETRY_WAIT.saturating_mul(2_u32.saturating_pow(failed - 1));
            CheckFailed::Wait {
                not_before: (self.clock)().saturating_add(i64::try_from(doubled.as_millis()).unwrap_or(i64::MAX)),
                error,
            }
        };
        let after = store.check_failed(row.id, &outcome)?;
        log::info!(
            "outbox row {} (clientmessageid {}): check {failed} before another attempt failed; now {:?}",
            after.id,
            after.client_message_id,
            after.state
        );
        self.replace_arrived(&after);
        Ok(after)
    }

    /// Keeps `row`, as a read just changed it, for [`Outbox::take_arrived`]: in place of any copy of it an earlier
    /// read kept, moved last as its latest change. One row is kept once, as the outbox last left it, whichever reads
    /// changed it on the way (a sync page that holds an entry of it no read can take and then a readable one): an
    /// earlier copy told after a later one would leave its owner with a row the store no longer has.
    fn keep_arrived(&self, row: &OutboxEntry) {
        log::info!(
            "outbox row {} ({:?}, clientmessageid {}) settled by a read: now {:?}",
            row.id,
            row.kind,
            row.client_message_id,
            row.state
        );
        let mut arrived = self.arrived.lock().unwrap_or_else(PoisonError::into_inner);
        arrived.retain(|queued| queued.id != row.id);
        arrived.push(row.clone());
    }

    /// `row` as it now stands in place of any copy of it kept for [`Outbox::take_arrived`], moved last as its latest
    /// change; nothing is kept for it when none was.
    fn replace_arrived(&self, row: &OutboxEntry) {
        let mut arrived = self.arrived.lock().unwrap_or_else(PoisonError::into_inner);
        let kept = arrived.len();
        arrived.retain(|queued| queued.id != row.id);
        if arrived.len() != kept {
            arrived.push(row.clone());
        }
    }

    /// Legacy edit commands had no attachment snapshot. Recover it once from the authoritative original and
    /// persist it before the attempt, so retries and explicit resends use the same request even after a restart.
    fn prepare_edit(&self, store: &mut Store, row: &OutboxEntry) -> Result<Result<(), String>, OutboxError> {
        if row.kind != OutboxKind::Edit {
            return Ok(Ok(()));
        }
        let Ok(mut command) = serde_json::from_str::<Command>(&row.payload) else {
            // The ordinary attempt reports an unreadable command through its existing terminal failure path.
            return Ok(Ok(()));
        };
        let Command::Edit {
            conversation,
            message_id,
            original,
            ..
        } = &mut command
        else {
            return Ok(Ok(()));
        };
        if original.attachment_properties.is_some() {
            return Ok(Ok(()));
        }
        let Some(message) = store.message(conversation, message_id)? else {
            return Ok(Err(
                "the original message for this queued edit is unavailable; nothing was sent".into(),
            ));
        };
        if !self.is_mine(&message)
            || message.delete_time.is_some()
            || message.client_message_id.as_deref() != Some(original.client_message_id.as_str())
        {
            return Ok(Err(
                "the original message for this queued edit cannot be identified; nothing was sent".into(),
            ));
        }
        let Ok(properties) = attachment_properties(&message) else {
            return Ok(Err(
                "the original attachment properties cannot be read; nothing was sent".into(),
            ));
        };
        original.attachment_properties = Some(properties);
        let payload = serde_json::to_string(&command).map_err(OutboxError::Payload)?;
        store.upgrade_pending_edit(row.id, &payload)?;
        Ok(Ok(()))
    }

    async fn attempt(&self, row: &OutboxEntry) -> Tried {
        let command = match serde_json::from_str::<Command>(&row.payload) {
            Ok(command) => command,
            Err(error) => return Tried::NotSent(format!("the stored command can't be read: {error}")),
        };
        let command = match command.for_cloud(self.transport.cloud()) {
            Ok(command) => command,
            Err(error) => return Tried::NotSent(error.to_string()),
        };
        let at = row.created_at;
        let me = self.me.clone();
        match command {
            Command::Send { conversation, text } => {
                let request = SendMessage::new(conversation, me, text)
                    .composed_at(at)
                    .with_client_message_id(&row.client_message_id);
                self.deliver(request).await
            }
            Command::Edit {
                conversation,
                message_id,
                text,
                original,
            } => {
                let Some(attachments) = original.attachment_properties else {
                    return Tried::NotSent(
                        "the queued edit has no original attachment snapshot; nothing was sent".into(),
                    );
                };
                let original = Original {
                    client_message_id: original.client_message_id,
                    compose_time_ms: original.compose_time_ms,
                    arrival_time_ms: original.arrival_time_ms,
                };
                let request = EditMessage::new(conversation, message_id, me, text, original)
                    .with_original_attachments(attachments)
                    .edited_at(at);
                self.deliver(request).await
            }
            Command::Delete {
                conversation,
                message_id,
            } => self.deliver(DeleteMessage::new(conversation, message_id)).await,
            Command::React {
                conversation,
                message_id,
                key,
                on: true,
            } => {
                self.deliver(SetReaction::new(conversation, message_id, key).at(at))
                    .await
            }
            Command::React {
                conversation,
                message_id,
                key,
                on: false,
            } => self.deliver(RemoveReaction::new(conversation, message_id, key)).await,
            Command::ReadReceipt {
                conversation,
                message_id,
                client_message_id,
            } => {
                let request = ReadReceipt::new(conversation, message_id, client_message_id).at(at);
                self.deliver(request).await
            }
            Command::CreateChat { others, topic } => match create_chat(me.mri(), &others, topic.as_deref()) {
                Some(request) => self.deliver(request).await,
                None => Tried::NotSent("the chat's members don't make a chat".to_owned()),
            },
            command @ (Command::AddMembers { .. }
            | Command::ActivityRead { .. }
            | Command::FeedRead
            | Command::ActivityTurnOffApp { .. }
            | Command::ActivityRestoreApp { .. }) => self.attempt_more(command, at, row.id).await,
        }
    }

    /// [`Outbox::attempt`]'s people added to a chat and the activity feed's writes.
    async fn attempt_more(&self, command: Command, at: i64, row: i64) -> Tried {
        match command {
            Command::AddMembers {
                conversation,
                members,
                share_history_time,
            } => {
                let members = members.iter().map(Mri::new).collect();
                let history = SharedHistory::from_share_history_time(share_history_time);
                match AddChatMembers::new(conversation, members, history) {
                    Some(request) => self.deliver(request).await,
                    None => Tried::NotSent("nobody to add".to_owned()),
                }
            }
            Command::ActivityRead { item, read } => match SetActivityRead::new(&item, read) {
                Some(request) => self.deliver(request).await,
                None => Tried::NotSent("the activity item's id can't be read".to_owned()),
            },
            Command::FeedRead => self.deliver(MarkFeedRead::at(at)).await,
            Command::ActivityTurnOffApp { app, activity_types } => self.turn_off_app(row, &app, &activity_types).await,
            Command::ActivityRestoreApp { app, preference } => {
                let mut settings = match self.app_settings().await {
                    Ok(settings) => settings,
                    Err(error) => return Tried::NotSent(error),
                };
                settings.restore_app(&app, &preference);
                self.deliver(teams_proto::SetUserGraphSettings(settings)).await
            }
            _ => Tried::NotSent("not a command of this kind".to_owned()),
        }
    }

    pub(crate) fn activity_app_undo(&self, row: i64) -> Option<Command> {
        let saved = self.activity_undo.lock().unwrap_or_else(PoisonError::into_inner);
        let (app, preference) = saved.get(&row)?;
        Some(Command::ActivityRestoreApp {
            app: app.clone(),
            preference: preference.clone(),
        })
    }

    async fn turn_off_app(&self, row: i64, app: &str, activity_types: &[String]) -> Tried {
        let mut settings = match self.app_settings().await {
            Ok(settings) => settings,
            Err(error) => return Tried::NotSent(error),
        };
        let original = match settings.app_preference(app) {
            Ok(original) => original,
            Err(error) => return Tried::NotSent(error.to_string()),
        };
        if let Err(error) = settings.turn_off(app, activity_types) {
            return Tried::NotSent(error.to_string());
        }
        self.activity_undo
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(row)
            .or_insert_with(|| (app.to_owned(), original));
        self.deliver(teams_proto::SetUserGraphSettings(settings)).await
    }

    async fn app_settings(&self) -> Result<teams_proto::UserGraphSettings, String> {
        if self.is_read_only() {
            return Err("app notification settings are read-only".to_owned());
        }
        match self.transport.read(&teams_proto::UserGraphSettingsRead).await {
            Ok(answer)
                if answer.status() == 404 && teams_proto::UserGraphSettingsRead::is_absent_answer(answer.body()) =>
            {
                Ok(teams_proto::UserGraphSettings::default())
            }
            Err(HttpError::Status(answer))
                if answer.status() == 404 && teams_proto::UserGraphSettingsRead::is_absent_answer(answer.body()) =>
            {
                Ok(teams_proto::UserGraphSettings::default())
            }
            Ok(answer) if (200..300).contains(&answer.status()) => {
                teams_proto::UserGraphSettings::read_answer(answer.body()).map_err(|error| error.to_string())
            }
            _ => Err("app notification settings could not be read; nothing was changed".to_owned()),
        }
    }

    async fn deliver<R: Request + Send>(&self, request: R) -> Tried {
        // read-only (the personal build's `ngteams --read-only`): refused before sending, logged by the
        // transport as `refused_read_only`; nothing is sent, and nothing tries it again
        if let Some(refused) = self.transport.read_only_refusal(&request) {
            log::info!("read-only: write refused");
            return Tried::NotSent(refused.to_string());
        }
        if !self.floor.allows(&self.tracker, &request) {
            return Tried::Denied("another account is in a call".to_owned());
        }
        let attempt = self.tracker.before(&request);
        let result = self.transport.deliver(request).await;
        if let Ok(answer) = &result {
            self.tracker.answered(attempt, answer);
        }
        let tried = match result {
            Ok(answer) => Tried::Answered(answer),
            Err(error) => from_transport(error),
        };
        if matches!(tried, Tried::NotSent(_) | Tried::Throttled { .. }) {
            self.tracker.not_sent(attempt);
        }
        tried
    }

    /// What an attempt makes of the row, and whether another attempt follows at once.
    fn settle(&self, row: &OutboxEntry, tried: Tried) -> (AttemptOutcome, bool) {
        let unauthorized = matches!(&tried, Tried::Answered(answer) if answer.status() == 401);
        let mut refreshed = self.refreshed.lock().unwrap_or_else(PoisonError::into_inner);
        let budget = row.attempts_at_resend;
        let used = refreshed.get(&row.id) == Some(&budget);
        let settled = self.settle_tried(row, tried, used);
        if !matches!(settled.0, AttemptOutcome::Retry { .. }) {
            // the row leaves pending here; one that leaves it elsewhere keeps an entry a resend makes stale
            refreshed.remove(&row.id);
        } else if unauthorized {
            refreshed.insert(row.id, budget);
        }
        settled
    }

    /// [`Outbox::settle`] for a row that had its one more attempt after a 401 in this budget (`refreshed`) or not.
    fn settle_tried(&self, row: &OutboxEntry, tried: Tried, refreshed: bool) -> (AttemptOutcome, bool) {
        match tried {
            Tried::Answered(answer) => self.settle_answer(row, &answer, refreshed),
            Tried::Denied(denied) => (AttemptOutcome::Denied { reason: denied.clone() }, false),
            Tried::NotSent(error) => (AttemptOutcome::Failed { error }, false),
            Tried::Throttled { wait, error } => (self.retry(row, wait, error), false),
            Tried::Uncertain(error) => self.again_if_findable(row, &error),
        }
    }

    /// `refreshed`: the row had its one more attempt after a 401 in this budget.
    fn settle_answer(&self, row: &OutboxEntry, answer: &Response, refreshed: bool) -> (AttemptOutcome, bool) {
        let status = answer.status();
        match status {
            200..=299 => (
                AttemptOutcome::Sent {
                    message_id: answered_id(row, answer),
                },
                false,
            ),
            401 if !refreshed => {
                let error = "HTTP 401, the token was refreshed".to_owned();
                (self.retry(row, Duration::ZERO, error), true)
            }
            429 | 503 => {
                let wait = answer.retry_after().unwrap_or(DEFAULT_RETRY_WAIT);
                (self.retry(row, wait, format!("HTTP {status}")), false)
            }
            500..=599 => self.again_if_findable(row, &format!("HTTP {status}")),
            _ => (
                AttemptOutcome::Failed {
                    error: format!("HTTP {status}"),
                },
                false,
            ),
        }
    }

    /// An attempt whose message may have arrived: another one (after the sync check) for a row that can be looked
    /// for; any other is failed rather than risk doing it twice.
    fn again_if_findable(&self, row: &OutboxEntry, error: &str) -> (AttemptOutcome, bool) {
        let error = format!("{error}; {OUTCOME_UNKNOWN}");
        if findable(row).is_some() {
            (self.retry(row, Duration::ZERO, error), true)
        } else {
            (
                AttemptOutcome::Failed {
                    error: format!("{error}, so it isn't sent again"),
                },
                false,
            )
        }
    }

    /// Another attempt after `wait`, unless the row has had all of its attempts since it was made or last resent.
    fn retry(&self, row: &OutboxEntry, wait: Duration, error: String) -> AttemptOutcome {
        let attempts = row.attempts_since_resend();
        if attempts >= MAX_ATTEMPTS {
            return AttemptOutcome::Failed {
                error: format!("{error}; given up after {attempts} attempts"),
            };
        }
        let wait = i64::try_from(wait.as_millis()).unwrap_or(i64::MAX);
        AttemptOutcome::Retry {
            not_before: (self.clock)().saturating_add(wait),
            error,
        }
    }
}

/// The conversation a row's message can be looked for in: a message post's.
fn findable(row: &OutboxEntry) -> Option<&str> {
    if row.kind == OutboxKind::Send {
        // a reply posted to a post's chain comes back in its channel (`teams_proto::reply_chain`)
        row.conversation_id.as_deref().map(teams_proto::stored_conversation)
    } else {
        None
    }
}

/// Whether an older pending row of this run is on `row`'s lane ([`Lane`]), so `row` waits for it to settle. A row
/// whose command can't be read is on no lane (its attempt fails it).
fn waits_behind(store: &Store, row: &OutboxEntry) -> Result<bool, OutboxError> {
    let Ok(command) = serde_json::from_str::<Command>(&row.payload) else {
        return Ok(false);
    };
    let Some(lane) = command.lane() else {
        return Ok(false);
    };
    // every pending row of this run, whenever it is due, oldest first
    let pending = store.outbox_due(i64::MAX)?;
    Ok(pending
        .iter()
        .take_while(|older| older.id < row.id)
        .any(|older| serde_json::from_str::<Command>(&older.payload).is_ok_and(|command| command.lane() == Some(lane))))
}

fn entry(store: &Store, id: i64) -> Result<OutboxEntry, OutboxError> {
    store
        .outbox_entry(id)?
        .ok_or(OutboxError::Store(StoreError::UnknownOutboxEntry(id)))
}

/// A transport error as an attempt's outcome: one that may have happened after the request went out is uncertain.
fn from_transport(error: HttpError) -> Tried {
    match error {
        HttpError::Failed(_) | HttpError::BodyTooLarge => Tried::Uncertain(error.to_string()),
        HttpError::Credentials(TokenError::Throttled { retry_after, .. }) => Tried::Throttled {
            wait: retry_after.unwrap_or(DEFAULT_RETRY_WAIT),
            error: "the token service is throttling".to_owned(),
        },
        other => Tried::NotSent(other.to_string()),
    }
}

/// The id a row that went through is kept with: a post's new message, a new chat's thread (from the answer), the
/// message an edit, a delete or a reaction acted on; none for a read receipt and people added.
fn answered_id(row: &OutboxEntry, answer: &Response) -> Option<String> {
    match row.kind {
        OutboxKind::Send | OutboxKind::CreateChat => message_id(answer),
        OutboxKind::ReadReceipt
        | OutboxKind::AddMembers
        | OutboxKind::FeedRead
        | OutboxKind::ActivityTurnOffApp
        | OutboxKind::ActivityRestoreApp => None,
        OutboxKind::Edit
        | OutboxKind::Delete
        | OutboxKind::AddReaction
        | OutboxKind::RemoveReaction
        | OutboxKind::ActivityRead => {
            let command = serde_json::from_str::<Command>(&row.payload).ok()?;
            command.message_id().map(str::to_owned)
        }
    }
}

/// The new message's id from a post's answer: the last segment of its `Location`, or else its `OriginalArrivalTime` (a
/// message's id is its arrival time). A new chat's answer gives its thread id the same way (`Location`
/// `…/v1/threads/<id>`).
fn message_id(answer: &Response) -> Option<String> {
    let located = answer
        .location()
        .and_then(|location| location.split(['?', '#']).next())
        .and_then(|path| path.rsplit('/').next())
        .filter(|id| !id.is_empty());
    if let Some(id) = located {
        return Some(id.to_owned());
    }
    let body: Value = serde_json::from_slice(answer.body()).ok()?;
    match body.get("OriginalArrivalTime")? {
        Value::Number(time) => Some(time.to_string()),
        Value::String(time) if !time.is_empty() => Some(time.clone()),
        _ => None,
    }
}

/// Milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> i64 {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(since_epoch.as_millis()).unwrap_or(i64::MAX)
}
