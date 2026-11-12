//! Catch-up sync: what the account missed while its push channel was down (or before the client ran) is read from
//! the chat service and kept in the store, once, through the same path a Trouter delivery takes.
//!
//! - **The conversation list** ([`CatchUp::catch_up`]): the first catch-up reads the whole list, every later one
//!   follows the list's own sync link (`syncState`), which brings only the conversations that changed since. Each
//!   conversation is upserted by its `version`, its read horizon and (for a chat) its members are kept, and the
//!   messages of every one that is new or changed are synced. What the list says of a conversation is stored whether
//!   or not its messages synced. The list's sync link moves on once every conversation it brought was synced or kept
//!   to be synced again ([`Store::set_unsynced`]): every later catch-up syncs a kept one (its messages, and a chat's
//!   members) whether or not the list brings it, until it syncs, so no conversation holds the list back. A conversation the
//!   service refuses ([`ConversationFailed::is_refused`]) keeps no catch-up due ([`CatchUpReport::caught_up`]); the
//!   account's drafts ([`DRAFTS`]), whose messages the service won't give, are never read.
//!   New conversation rows are stored as each list page arrives, at version 0 until their messages are synced;
//!   list pages and completed conversation syncs publish through [`Meanwhile::progress`] so the first usable list
//!   and messages do not wait for the rest of the account. The list's sync link still waits for every conversation.
//! - **Threads the account can't read** ([`Unreadable`]): the list still brings threads the account left or was removed
//!   from (their reads answer 403 `AclCheckFailed`) and threads deleted for good (404 `ThreadNotFound`). Such a
//!   conversation isn't kept to be synced again: reading it again changes nothing, and if the account is added back the
//!   list brings it with a newer version, which syncs it. One no sync ever read is kept with a sync state that has no
//!   link, so a full listing skips it too (its messages and its members) unless it shows a newer version or a later
//!   last message. A deleted one is forgotten ([`Store::forget_conversation`], told as a change so the views drop it,
//!   its writes still to go told failed and never sent), but the account's own notes, whose 404 means none yet. The
//!   store keeps the version and last activity it had then ([`Store::forgotten_thread`]), so a listing that still
//!   names it neither adds it back nor reads it unless it shows a newer version or a later last message. Each is one
//!   `info` line naming the status and the service's code, never its answer.
//! - **A conversation's messages** ([`CatchUp::sync_messages`]): the first read is the newest page, whose
//!   `backwardLink` is kept for older history ([`CatchUp::load_older`]); every later one follows the conversation's
//!   sync link, which brings what changed since (`docs/protocol/chat.md`, "Sync answers"). A gap is bounded by the
//!   end of the segment the kept sync link came with ([`SyncState::sync_until`], the answer's
//!   `lastCompleteSegmentEndTime`): every message that arrived since is newer than it. A sync answer's
//!   `backwardLink` is followed until a page reaches back to that time (its `lastCompleteSegmentStartTime`, else its
//!   oldest message), or the service ends it: what is behind is history the sync before had, or older, left to
//!   [`CatchUp::load_older`] when the user scrolls. On the way, a page that changes nothing (its messages came as
//!   deliveries, or by an earlier attempt that failed further back) doesn't stop it. Past [`MAX_PAGES`] the link
//!   left is kept as the conversation's gap ([`SyncState::gap_link`]) with the same bound
//!   ([`SyncState::gap_until`]), apart from its older history's `backwardLink`, so the sync link moves on only past
//!   what is stored or kept to be read. The next sync reads on from a kept gap first, and so does
//!   [`CatchUp::load_older`] before older history; a sync answer cut again while a gap is still unread leaves the sync
//!   link where it was ([`CatchUpError::GapPending`]). A sync state written before the bound was kept is bounded by
//!   when it was written, less [`CLOCK_SKEW_MS`] (kept as its bound by the next write), and a gap kept then ends at a
//!   page whose messages the store all had: the messages the sync before it stored. An edit or reaction of a message older than the bound is read only when
//!   the answer's pages up to it carry it (a delivery brings it while the push channel is up). A conversation the
//!   store doesn't know yet is first read as a thread, then gets only its newest page.
//! - **The account's own notes** ([`NOTES`], `48:notes`, its chat with itself, which Teams lists under "Favorites"):
//!   the conversation list never brings it, so a catch-up syncs it after the list when it is one of the favourites CSA
//!   listed or the store already has it (a failure there doesn't hold the list's sync link back). Its messages are
//!   read as any conversation's (`GET …/conversations/48:notes/messages`); it is no thread, so it is never read as one:
//!   the store keeps it as `thread_type:streamofnotes`, with no members, and a delivery naming it adds it so too. An
//!   account that never wrote a note has none: the service answers 404, which is no notes, not a failure.
//! - **One path for what comes in** ([`CatchUp::receive_message`]): a message as read (a sync page, or the resource of
//!   a Trouter delivery) becomes a store row and goes through [`Outbox::receive`], which stores it by its id and
//!   version and matches our own to its outbox row by `clientmessageid`. So a delivery and a sync that bring the same
//!   message, in either order, leave one row at the newest version.
//! - **Control messages are signals** (`Control/…`: typing, its end, any other control): never stored as a message
//!   and never matched to an outbox row, even our own; a sync page's are counted in [`Synced::control`] and nothing
//!   else, a delivery's is dropped ([`CatchUp::receive_message`] gives `None`).
//! - **Versions are never guessed**: a message whose `version` this build can't read ([`Field::Unknown`], or none at
//!   all) isn't stored, since it could be older or newer than the stored row; it is counted in [`Synced::skipped`],
//!   and so is a reaction set with a time it can't read (its reactions stay as they were). A conversation whose
//!   version can't be read keeps its stored row (only a later last activity is taken) and has its messages synced.
//!   Our own message among those not stored is still matched to its outbox row by its id, `clientmessageid`,
//!   sender and conversation ([`Outbox::match_arrival`]). An entry a page holds that can't be read at all but carries
//!   the `clientmessageid` of a message post that had an attempt is recorded on that row before the page is applied
//!   ([`Outbox::unreadable_seen`]), by every read alike: the read moves on, and the row's check then can't tell
//!   whether its message arrived, so the row isn't sent again on its own.
//! - **When**: on start and after every reconnect of the push channel ([`Gaps`], [`CatchUp::catch_up_if_due`]). The
//!   channel is connected first and the catch-up runs after, so nothing falls between the two: what arrives after the
//!   connect comes as a delivery, what came before by the catch-up, and what comes both ways is stored once.
//! - **The outbox's check** ([`ConversationSync`]): before a new attempt whose earlier one may have arrived, the outbox
//!   syncs the conversation through [`CatchUp::sync_messages`] and looks for the row's `clientmessageid`. Whether
//!   that is enough is told by the stored sync state, not by what this one sync read: older pages may be left by it
//!   or by any read before it (a first read is one page, a sync answer is cut at [`MAX_PAGES`], an earlier check fell
//!   short). So the check reads any kept gap to its end and older history until its pages reached a message older
//!   than the row (less [`CLOCK_SKEW_MS`]; [`SyncState::backward_oldest`]), and fails after [`MAX_PAGES`] more, so
//!   the row waits rather than going out twice.
//!
//! - **What the reads changed** ([`CatchUp::take_changes`]): every message a sync page stored new or newer, and every
//!   conversation a read added or changed (its version, its read horizon, its roster, its messages), is kept until
//!   the engine takes it after its read and tells its owner; a message the store had as new or newer is not, so a
//!   delivery the catch-up meets again changes nothing and is told once.
//! - **Meanwhile** ([`Meanwhile`]): while a read waits on the service, the store is handed to its owner's
//!   [`Meanwhile::serve`] (the engine answers its queries from it), and taken back the moment the read is over. A
//!   catch-up of an account with hundreds of conversations and busy channels (a page of 200 channel posts takes
//!   seconds) runs for a while, and a view's read of the store mustn't wait for it. The store is handed over only
//!   between the catch-up's own uses of it, never while it is half way through storing a page.
//!
//! Every read is a `GET` through the native transport (`teams_proto::chat::ChatReads`), and every link is the
//! service's own, followed only on the same chat base. Nothing here writes to the service.
//!
//! M1 Task 17 (`docs/plans/20261020-native-teams-m1-headless-chat-core.md`).

use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use teams_proto::Mri;
use teams_proto::chat::{self, ChatReadError, ChatReads, Field, MAX_PAGE_SIZE, MessagePage};
use teams_proto::{Client, Credentials, HttpError, HttpTransport, Transport};
use teams_store::{
    AppReadHorizon, Applied, Conversation, ConversationKind, Member, Message, Reaction, ReadHorizon, Store, StoreError,
    SyncState,
};

use crate::outbox::{Arrival, Clock, ConversationSync, Outbox, OutboxError, Received, SyncError, now_ms};

/// The most pages one read follows: the conversation list's `forwardLink`s, a sync answer's `backwardLink`s, or the
/// older pages the outbox's check reads.
pub const MAX_PAGES: usize = 50;

/// How far the service's clock (a message's arrival time) may be behind ours (an outbox row's `created_at`): the
/// outbox's check reads back to a message older than its row by this much. Five minutes, Kerberos's default tolerance.
pub const CLOCK_SKEW_MS: i64 = 5 * 60 * 1000;

/// The account's own notes: its chat with itself, a `48:` stream (`threadType` `streamofnotes`) that Teams lists under
/// "Favorites" as "<name> (You)". Read like any conversation's messages; nothing is written there.
pub const NOTES: &str = "48:notes";

/// The account's drafts: a `48:` stream the conversation list brings, whose messages the chat service won't give
/// (`GET …/conversations/48:drafts/messages` answers 400, "Invalid threadId"). Kept as the list gives it, never read.
pub const DRAFTS: &str = "48:drafts";

/// The store's kind for [`NOTES`]: what the conversation list's own kind for it would be ([`store_kind`]).
const NOTES_KIND: &str = "thread_type:streamofnotes";

/// What a catch-up can fail with.
#[derive(Debug, thiserror::Error)]
pub enum CatchUpError {
    #[error(transparent)]
    Read(#[from] ChatReadError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Outbox(#[from] OutboxError),
    /// The conversation list ran past [`MAX_PAGES`] pages: its sync link wasn't moved on.
    #[error("the conversation list ran past {MAX_PAGES} pages, so its sync link wasn't moved on")]
    TooManyPages,
    /// The outbox's check paged back [`MAX_PAGES`] pages without reaching a message older than its row (or the end
    /// of a gap): whether the row's message arrived can't be told.
    #[error("the outbox's check read {MAX_PAGES} older pages without reaching its row's time")]
    CheckFellShort,
    /// A sync answer ran past [`MAX_PAGES`] while the rest of an earlier one was still unread: its sync link wasn't
    /// moved on (the pages it read are stored). The next sync reads on from the earlier one first.
    #[error("a sync answer ran past {MAX_PAGES} pages while an earlier one's rest was still unread")]
    GapPending,
}

/// Which of a conversation's kept links an older page is read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Chain {
    /// The rest of a sync answer cut at [`MAX_PAGES`] ([`SyncState::gap_link`]).
    Gap,
    /// Older history ([`SyncState::backward_link`]).
    History,
}

impl Chain {
    /// The chain the next older page comes from: a gap before older history.
    fn next(state: &SyncState) -> Self {
        if state.gap_link.is_some() {
            Self::Gap
        } else {
            Self::History
        }
    }

    /// The chain the outbox's check still has to read for a message that arrived no earlier than `stop`, decided by
    /// what is stored, not by the read just made: any gap (a sync answer's order is not relied on), then older
    /// history until its pages reached a message older than `stop`. `None`: everything since `stop` is stored.
    fn unread_since(state: &SyncState, stop: i64) -> Option<Self> {
        if state.gap_link.is_some() {
            return Some(Self::Gap);
        }
        let reached = state.backward_oldest.is_some_and(|oldest| oldest < stop);
        (state.backward_link.is_some() && !reached).then_some(Self::History)
    }
}

/// What a sync did with the messages it read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Synced {
    /// Messages the store didn't have.
    pub inserted: usize,
    /// Messages the store had at an older version.
    pub updated: usize,
    /// Messages the store had at the same or a newer version: nothing changed.
    pub unchanged: usize,
    /// Entries not stored: not a message's shape, of another conversation, without a version or an arrival time this
    /// build can read. A message stored whose reaction set had a time it can't read counts here too.
    pub skipped: usize,
    /// Control signals the pages carried (`Control/…`: typing, its end): dropped, neither stored nor counted as
    /// messages.
    pub control: usize,
    /// Pages read.
    pub pages: usize,
}

impl Synced {
    /// Messages that were new or newer.
    #[must_use]
    pub fn changed(&self) -> usize {
        self.inserted + self.updated
    }

    fn count(&mut self, applied: Applied) {
        match applied {
            Applied::Inserted => self.inserted += 1,
            Applied::Updated => self.updated += 1,
            Applied::Ignored => self.unchanged += 1,
        }
    }

    fn add(&mut self, other: Self) {
        self.inserted += other.inserted;
        self.updated += other.updated;
        self.unchanged += other.unchanged;
        self.skipped += other.skipped;
        self.control += other.control;
        self.pages += other.pages;
    }
}

/// What one [`CatchUp::catch_up`] did.
#[derive(Debug, Default)]
pub struct CatchUpReport {
    /// Conversations the list brought: every one on the first catch-up, those changed since afterwards.
    pub conversations: usize,
    /// List entries that aren't a conversation's shape.
    pub skipped: usize,
    /// What the conversations' message syncs did, together.
    pub messages: Synced,
    /// Conversations that couldn't be synced. Each is kept in the store to be synced again by every later catch-up
    /// ([`Store::unsynced_conversations`]), so the list's sync link moves on past it; the account's own notes
    /// ([`NOTES`]), which the list never brings, are synced again by it all the same.
    pub failed: Vec<ConversationFailed>,
    /// Conversations the account can't read (not a member, or deleted): not synced, and not kept to be.
    pub unreadable: Vec<Unreadable>,
}

impl CatchUpReport {
    /// Whether nothing failed that trying again soon may change: every conversation was synced, or refused by the
    /// service ([`ConversationFailed::is_refused`]). A refused one (a 400, or the account's notes refused) is still
    /// synced again by every later catch-up, but keeps none due. One the account can't read (a listed or kept
    /// conversation answered 403, 404 or 410) isn't here but in [`CatchUpReport::unreadable`], and isn't synced again.
    #[must_use]
    pub fn caught_up(&self) -> bool {
        self.failed.iter().all(ConversationFailed::is_refused)
    }
}

/// A conversation a catch-up couldn't sync.
#[derive(Debug)]
pub struct ConversationFailed {
    pub conversation: String,
    pub error: CatchUpError,
}

impl ConversationFailed {
    /// Whether the service refused the conversation's read in a way the same read soon after won't change: 400, 403,
    /// 404 (without a `Location`) or 410. A timeout, a 401, a throttle, a 5xx, a sync answer left for later
    /// ([`CatchUpError::GapPending`]) or a store's failure is not.
    #[must_use]
    pub fn is_refused(&self) -> bool {
        matches!(
            &self.error,
            CatchUpError::Read(ChatReadError::Http(HttpError::Status(answer)))
                if matches!(answer.status(), 400 | 403 | 404 | 410)
        )
    }
}

/// A conversation the account can't read, as the service answered its sync: 403 (the account isn't on the thread's
/// roster: it left, or was removed), 404 without a `Location` (the thread isn't there) or 410. It isn't kept to be
/// synced again ([`Store::set_synced`]): the list brings it again, at a newer version, if the account is added back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreadable {
    pub conversation: String,
    /// The status the service answered.
    pub status: u16,
    /// The service's code for it (`AclCheckFailed`, `ThreadNotFound`, …) when its answer named one.
    pub code: Option<String>,
    /// The thread is gone (a 404 naming `ThreadNotFound`: hard-deleted), so the store forgot it
    /// ([`Store::forget_conversation`]).
    pub forgotten: bool,
}

impl Unreadable {
    /// What a conversation's failed sync says of it, when it is one the account can't read; `None` for any other
    /// failure.
    fn of(conversation: &str, error: &CatchUpError) -> Option<Self> {
        let CatchUpError::Read(read @ ChatReadError::Http(HttpError::Status(answer))) = error else {
            return None;
        };
        matches!(answer.status(), 403 | 404 | 410).then(|| Self {
            conversation: conversation.to_owned(),
            status: answer.status(),
            code: read.service_code(),
            // the notes' 404 is no notes yet, never a thread gone
            forgotten: read.is_thread_gone() && conversation != NOTES,
        })
    }
}

/// What the reads of the service changed in the store since the last [`CatchUp::take_changes`]: every read alike (a
/// catch-up, a conversation's sync, older history, the outbox's check), never a delivery's message, which its
/// receiver tells of itself ([`CatchUp::receive_message`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Messages a sync page stored new ([`Applied::Inserted`]) or newer ([`Applied::Updated`]), in the order stored;
    /// one the store had as new or newer isn't here.
    pub messages: Vec<StoredMessage>,
    /// Conversations a read changed (added, a newer version, a moved read horizon, a newer roster, a message stored),
    /// each once, in the order first changed.
    pub conversations: Vec<String>,
}

impl Changes {
    /// Nothing changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty() && self.conversations.is_empty()
    }

    fn conversation(&mut self, conversation: &str) {
        if !self.conversations.iter().any(|known| known == conversation) {
            self.conversations.push(conversation.to_owned());
        }
    }
}

/// A message a sync page stored, as [`Changes`] lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredMessage {
    pub conversation: String,
    pub id: String,
    /// [`Applied::Inserted`] or [`Applied::Updated`].
    pub applied: Applied,
}

/// When a catch-up is due: before the first one, and whenever the push channel went down since the last one that ran
/// to the end while it was up.
#[derive(Debug)]
pub struct Gaps {
    due: bool,
    up: bool,
}

impl Default for Gaps {
    fn default() -> Self {
        Self::new()
    }
}

impl Gaps {
    /// A start: a catch-up is due, the channel isn't up yet.
    #[must_use]
    pub fn new() -> Self {
        Self { due: true, up: false }
    }

    /// The push channel is connected (the first time, or again).
    pub fn connected(&mut self) {
        self.up = true;
    }

    /// The push channel went down: what it would have brought is missed until the next catch-up.
    pub fn disconnected(&mut self) {
        self.up = false;
        self.due = true;
    }

    /// Something the push channel would have brought was missed while it stayed up (the service reported a message
    /// loss), or the owner asks for a catch-up: one is due.
    pub fn missed(&mut self) {
        self.due = true;
    }

    /// Whether a catch-up is due.
    #[must_use]
    pub fn is_due(&self) -> bool {
        self.due
    }

    /// A catch-up ran to the end with nothing failed: none is due until the channel drops again, unless it was down
    /// all along (then what it missed meanwhile keeps one due for when it connects).
    fn caught_up(&mut self) {
        self.due = !self.up;
    }
}

/// What the owner of a catch-up does with its store while a read waits on the service (see the module docs).
pub trait Meanwhile: Send + Sync {
    /// Uses `store` until the read is over, when the future is dropped wherever it waits: only at an `.await` that
    /// loses nothing when dropped there. A future that ends gives the store back early; the read is waited for then.
    fn serve<'a>(&'a self, store: &'a mut Store) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

    /// Publishes a completed part of a catch-up before the rest is read. An owner that takes the changes removes
    /// them from `changes`, so the final publication does not tell them twice. The default keeps them for the owner.
    fn progress(&self, _store: &Store, _changes: &mut Changes) {}
}

/// The catch-up sync of one account (see the module docs): reads through `client`, stores what comes in through
/// `outbox`'s [`Outbox::receive`].
pub struct CatchUp<C, T> {
    client: Arc<Client<HttpTransport<C>>>,
    outbox: Arc<Outbox<T>>,
    /// The account: its read horizon is the list's `consumptionhorizon`.
    me: Mri,
    page_size: u32,
    clock: Clock,
    /// What the reads changed, until taken ([`CatchUp::take_changes`]).
    changes: Mutex<Changes>,
    /// What the store is used for while a read waits ([`CatchUp::with_meanwhile`]).
    meanwhile: Option<Arc<dyn Meanwhile>>,
}

impl<C, T> std::fmt::Debug for CatchUp<C, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatchUp")
            .field("me", &self.me)
            .field("page_size", &self.page_size)
            .finish_non_exhaustive()
    }
}

impl<C: Credentials, T: Transport<Error = HttpError>> CatchUp<C, T> {
    /// The catch-up of the account `me`, reading through `client` and storing through `outbox`.
    #[must_use]
    pub fn new(client: Arc<Client<HttpTransport<C>>>, outbox: Arc<Outbox<T>>, me: Mri) -> Self {
        Self {
            client,
            outbox,
            me,
            page_size: MAX_PAGE_SIZE,
            clock: Arc::new(now_ms),
            changes: Mutex::new(Changes::default()),
            meanwhile: None,
        }
    }

    /// Hands the store to `meanwhile` while each read waits on the service (see the module docs).
    #[must_use]
    pub fn with_meanwhile(mut self, meanwhile: Arc<dyn Meanwhile>) -> Self {
        self.meanwhile = Some(meanwhile);
        self
    }

    /// Waits for `read` of the service, the store meanwhile handed to [`Meanwhile::serve`] (see the module docs).
    async fn waiting<F: Future>(&self, store: &mut Store, read: F) -> F::Output {
        let mut read = pin!(read);
        if let Some(meanwhile) = &self.meanwhile {
            tokio::select! {
                biased;
                output = &mut read => return output,
                () = meanwhile.serve(store) => {}
            }
        }
        read.await
    }

    fn publish_progress(&self, store: &Store) {
        if let Some(meanwhile) = &self.meanwhile {
            meanwhile.progress(store, &mut self.changes.lock().unwrap_or_else(PoisonError::into_inner));
        }
    }

    /// What the reads changed in the store since the last call ([`Changes`]): the engine takes it after every read it
    /// makes and tells its owner. Kept until taken, whether or not the read ran to its end.
    pub fn take_changes(&self) -> Changes {
        std::mem::take(&mut *self.changes.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn changed(&self, record: impl FnOnce(&mut Changes)) {
        record(&mut self.changes.lock().unwrap_or_else(PoisonError::into_inner));
    }

    /// A conversation changed by a read, when `applied` changed anything.
    fn conversation_changed(&self, conversation: &str, applied: Applied) {
        if applied != Applied::Ignored {
            self.changed(|changes| changes.conversation(conversation));
        }
    }

    /// Another size for a conversation's first page and its older pages ([`MAX_PAGE_SIZE`] at most).
    #[must_use]
    pub fn with_page_size(mut self, page_size: u32) -> Self {
        self.page_size = page_size.clamp(1, MAX_PAGE_SIZE);
        self
    }

    /// Another clock for the times a sync state is written at.
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Catches up if one is due ([`Gaps`]); `None` when none is. A catch-up that fails, or leaves a conversation
    /// unsynced for a reason other than the service's refusal ([`CatchUpReport::caught_up`]), keeps one due.
    ///
    /// # Errors
    ///
    /// As [`CatchUp::catch_up`].
    pub async fn catch_up_if_due(
        &self,
        store: &mut Store,
        gaps: &mut Gaps,
    ) -> Result<Option<CatchUpReport>, CatchUpError> {
        if !gaps.is_due() {
            return Ok(None);
        }
        let report = self.catch_up(store).await?;
        if report.caught_up() {
            gaps.caught_up();
        }
        Ok(Some(report))
    }

    /// Reads the conversation list (the whole one the first time, what changed since the last catch-up after that)
    /// and syncs every conversation it brought (see the module docs).
    ///
    /// # Errors
    ///
    /// [`CatchUpError::Read`] when the list can't be read, [`CatchUpError::TooManyPages`] when it doesn't end,
    /// [`CatchUpError::Store`] when the store fails to keep its sync link. A conversation that can't be synced is in
    /// [`CatchUpReport::failed`] instead.
    pub async fn catch_up(&self, store: &mut Store) -> Result<CatchUpReport, CatchUpError> {
        self.read_app_horizon(store).await?;
        let kept = store.conversation_list_sync_link()?;
        let first = kept.is_none();
        let (sync_link, conversations, mut report) = self.read_list(store, kept).await?;
        // past every conversation it brought: each one is synced, or kept to be synced again
        if self.sync_listed(store, &conversations, &mut report).await? {
            store.set_conversation_list_sync_link(sync_link.as_deref(), (self.clock)())?;
        }
        self.catch_up_notes(store, &conversations, &mut report).await?;
        log_report(&report, first);
        Ok(report)
    }

    /// The account's mark-all-read horizon. A read that fails keeps the one stored and stops no catch-up.
    async fn read_app_horizon(&self, store: &mut Store) -> Result<(), CatchUpError> {
        let reads = ChatReads::new(&self.client);
        match self.waiting(store, reads.app_consumption_horizon()).await {
            Ok(Some(horizon)) => self.receive_app_horizon(store, horizon)?,
            Ok(None) => {}
            Err(error) => log::debug!("sync: the account's own properties weren't read: {error}"),
        }
        Ok(())
    }

    /// Keeps the account's mark-all-read horizon, and tells of the conversations whose unread count it can change.
    ///
    /// # Errors
    ///
    /// [`CatchUpError::Store`] when the store fails.
    pub fn receive_app_horizon(
        &self,
        store: &mut Store,
        horizon: chat::AppConsumptionHorizon,
    ) -> Result<(), CatchUpError> {
        let changed = store.set_app_read_horizon(AppReadHorizon {
            chat: horizon.chat,
            channel: horizon.channel,
        })?;
        for conversation in &changed {
            self.changed(|changes| changes.conversation(conversation));
        }
        Ok(())
    }

    /// The conversation list from `kept`, its sync link (the whole list without one), every page of it: the list's
    /// next sync link, the conversations, and a report counting them.
    async fn read_list(
        &self,
        store: &mut Store,
        kept: Option<String>,
    ) -> Result<(Option<String>, Vec<chat::Conversation>, CatchUpReport), CatchUpError> {
        let reads = ChatReads::new(&self.client);
        let mut list = match kept.as_deref() {
            Some(link) => self.waiting(store, reads.conversations_from(link)).await?,
            None => self.waiting(store, reads.conversations()).await?,
        };
        let sync_link = list.sync_state().map(str::to_owned).or(kept);
        let mut conversations = std::mem::take(&mut list.conversations);
        let mut report = CatchUpReport {
            skipped: list.unreadable.len(),
            ..CatchUpReport::default()
        };
        self.stage_listed(store, &conversations)?;
        let mut pages = 1;
        while let Some(next) = list.forward_link().map(str::to_owned) {
            if pages == MAX_PAGES {
                return Err(CatchUpError::TooManyPages);
            }
            list = self.waiting(store, reads.conversations_from(&next)).await?;
            pages += 1;
            self.stage_listed(store, &list.conversations)?;
            conversations.append(&mut list.conversations);
            report.skipped += list.unreadable.len();
        }
        report.conversations = conversations.len();
        Ok((sync_link, conversations, report))
    }

    fn stage_listed(&self, store: &mut Store, conversations: &[chat::Conversation]) -> Result<(), CatchUpError> {
        for listed in conversations {
            if store.conversation(&listed.id)?.is_none() && !still_forgotten(store, listed, known_i64(&listed.version))?
            {
                self.store_listing(store, listed, None, Some(0))?;
            }
        }
        self.publish_progress(store);
        Ok(())
    }

    /// Syncs every conversation the list brought, then every one an earlier catch-up kept to be synced again that it
    /// didn't bring ([`CatchUp::resync`]); whether the store kept what became of each (when it didn't, the list's sync
    /// link stays).
    async fn sync_listed(
        &self,
        store: &mut Store,
        conversations: &[chat::Conversation],
        report: &mut CatchUpReport,
    ) -> Result<bool, CatchUpError> {
        // the conversations an earlier catch-up couldn't sync, taken before this one keeps any
        let unsynced = store.unsynced_conversations()?;
        let mut kept = true;
        for conversation in conversations {
            let result = self.catch_up_conversation(store, conversation).await;
            kept &= self.settle(store, &conversation.id, result, report);
            self.publish_progress(store);
        }
        for id in unsynced {
            if conversations.iter().any(|listed| listed.id == id) {
                continue;
            }
            let result = self.resync(store, &id).await;
            kept &= self.settle(store, &id, result, report);
            self.publish_progress(store);
        }
        Ok(kept)
    }

    /// A conversation kept to be synced again that the list didn't bring: its messages, and a chat's members read
    /// again as a thread. What the list said of it was stored when it brought it ([`CatchUp::catch_up_conversation`]).
    async fn resync(&self, store: &mut Store, id: &str) -> Result<Synced, CatchUpError> {
        let synced = self.sync_messages(store, id).await?;
        let chat = store
            .conversation(id)?
            .is_some_and(|stored| matches!(stored.kind, ConversationKind::OneToOne | ConversationKind::Group));
        if chat {
            let reads = ChatReads::new(&self.client);
            let thread = self.waiting(store, reads.thread(id)).await?;
            self.store_members(store, &thread)?;
        }
        Ok(synced)
    }

    /// Syncs one conversation's messages from the service (see the module docs): the newest page the first time, what
    /// changed since after that. A conversation the store doesn't know is read as a thread first.
    ///
    /// # Errors
    ///
    /// [`CatchUpError::Read`] when a read fails, [`CatchUpError::Store`] or [`CatchUpError::Outbox`] when the store
    /// does; the sync state then stays as it was (a kept gap may have been read on). [`CatchUpError::GapPending`]
    /// when the answer is cut at [`MAX_PAGES`] while an earlier gap is still unread: the sync link stays. The account's
    /// own notes ([`NOTES`]) answered 404 ("Conversation was not found": nothing was ever written there) are none yet,
    /// not an error.
    pub async fn sync_messages(&self, store: &mut Store, conversation: &str) -> Result<Synced, CatchUpError> {
        if conversation == DRAFTS {
            return Ok(Synced::default());
        }
        match self.sync_stored(store, conversation).await {
            Err(error) if conversation == NOTES && not_found(&error) => {
                log::debug!("sync: the account's notes aren't there yet (404)");
                Ok(Synced::default())
            }
            synced => synced,
        }
    }

    /// [`CatchUp::sync_messages`], whatever the service answers.
    async fn sync_stored(&self, store: &mut Store, conversation: &str) -> Result<Synced, CatchUpError> {
        if store.conversation(conversation)?.is_none() {
            self.add_conversation(store, conversation).await?;
        }
        let reads = ChatReads::new(&self.client);
        let kept = store.sync_state(conversation)?;
        let Some((kept, link)) = kept.and_then(|state| state.sync_link.clone().map(|link| (state, link))) else {
            return self.first_read(store, &reads, conversation).await;
        };
        // how far back this answer is read: to the end of the segment the kept sync link came with
        let until = sync_bound(&kept);
        let mut synced = Synced::default();
        let kept = self.read_gap(store, kept, &mut synced).await?;
        let page = self.waiting(store, reads.messages_from(conversation, &link)).await?;
        synced.add(self.apply_page(store, conversation, &page)?);
        let rest = page.backward_link().filter(|_| !reached(&page, until));
        let (left, walked) = self
            .follow_back(store, conversation, rest, MAX_PAGES - 1, Bound::Until(until))
            .await?;
        synced.add(walked);
        if left.is_some() && kept.gap_link.is_some() {
            // one gap link at a time: the earlier one's progress is kept, this answer is read again next time
            return Err(CatchUpError::GapPending);
        }
        let (gap_link, gap_until) = match left {
            Some(left) => (Some(left), Some(until)),
            None => (kept.gap_link.clone(), kept.gap_until),
        };
        let state = SyncState {
            sync_link: page.sync_state().map(str::to_owned).or(Some(link)),
            sync_until: segment_end(&page).or(Some(until)),
            history_complete: kept.backward_link.is_none() && gap_link.is_none(),
            gap_link,
            gap_until,
            updated: (self.clock)(),
            ..kept
        };
        store.set_sync_state(&state)?;
        Ok(synced)
    }

    /// Reads on from the rest of an earlier sync answer, if one is kept, for up to [`MAX_PAGES`] pages; returns the
    /// sync state with the gap as far as it is now read (stored).
    async fn read_gap(
        &self,
        store: &mut Store,
        kept: SyncState,
        synced: &mut Synced,
    ) -> Result<SyncState, CatchUpError> {
        if kept.gap_link.is_none() {
            return Ok(kept);
        }
        let conversation = kept.conversation_id.clone();
        let bound = gap_bound(&kept);
        let (gap_link, walked) = self
            .follow_back(store, &conversation, kept.gap_link.as_deref(), MAX_PAGES, bound)
            .await?;
        synced.add(walked);
        let state = SyncState {
            history_complete: kept.backward_link.is_none() && gap_link.is_none(),
            sync_until: Some(sync_bound(&kept)),
            gap_until: gap_link.as_ref().and(bound.until()),
            gap_link,
            updated: (self.clock)(),
            ..kept
        };
        store.set_sync_state(&state)?;
        Ok(state)
    }

    /// Reads the next older page of a conversation from the links its sync kept, the rest of a cut sync answer before
    /// older history; `None` when there is none to read (the whole history is stored, or the conversation was never
    /// synced).
    ///
    /// # Errors
    ///
    /// As [`CatchUp::sync_messages`].
    pub async fn load_older(&self, store: &mut Store, conversation: &str) -> Result<Option<Synced>, CatchUpError> {
        let Some(kept) = store.sync_state(conversation)? else {
            return Ok(None);
        };
        let chain = Chain::next(&kept);
        self.older_page(store, kept, chain).await
    }

    /// Reads the next page of one of a conversation's kept chains; `None` when that chain has no link.
    async fn older_page(
        &self,
        store: &mut Store,
        kept: SyncState,
        chain: Chain,
    ) -> Result<Option<Synced>, CatchUpError> {
        let link = match chain {
            Chain::Gap => kept.gap_link.as_deref(),
            Chain::History => kept.backward_link.as_deref(),
        };
        let Some(link) = link else {
            return Ok(None);
        };
        let conversation = kept.conversation_id.as_str();
        let reads = ChatReads::new(&self.client);
        let page = self.waiting(store, reads.messages_from(conversation, link)).await?;
        let synced = self.apply_page(store, conversation, &page)?;
        let older = page.backward_link().map(str::to_owned);
        // a state of an older schema keeps the bound its next sync answer falls back on, not the time of this write
        let sync_until = Some(sync_bound(&kept));
        let state = match chain {
            Chain::Gap => {
                // the gap ends where its page reached what the sync before it had
                let bound = gap_bound(&kept);
                let older = older.filter(|_| !bound.ends(&page, &synced));
                SyncState {
                    history_complete: kept.backward_link.is_none() && older.is_none(),
                    sync_until,
                    gap_until: older.as_ref().and(bound.until()),
                    gap_link: older,
                    updated: (self.clock)(),
                    ..kept
                }
            }
            Chain::History => SyncState {
                history_complete: kept.gap_link.is_none() && older.is_none(),
                // a page without a time it can read is older than the one before it all the same
                backward_oldest: oldest_arrival(&page).or(kept.backward_oldest),
                backward_link: older,
                sync_until,
                updated: (self.clock)(),
                ..kept
            },
        };
        store.set_sync_state(&state)?;
        Ok(Some(synced))
    }

    /// The outbox's check (see the module docs): syncs `conversation`, then reads older pages until the stored state
    /// says everything since `since` less [`CLOCK_SKEW_MS`] is stored ([`Chain::unread_since`]), whatever this sync
    /// itself read: a gap or a history read an earlier sync, check or start left unread is read here.
    async fn sync_since(&self, store: &mut Store, conversation: &str, since: i64) -> Result<(), CatchUpError> {
        let stop = since.saturating_sub(CLOCK_SKEW_MS);
        self.sync_messages(store, conversation).await?;
        let mut pages = 0;
        loop {
            // a sync that succeeded kept a state: none can't be told complete
            let state = store.sync_state(conversation)?.ok_or(CatchUpError::CheckFellShort)?;
            let Some(chain) = Chain::unread_since(&state, stop) else {
                return Ok(());
            };
            if pages == MAX_PAGES {
                return Err(CatchUpError::CheckFellShort);
            }
            self.older_page(store, state, chain).await?;
            pages += 1;
        }
    }

    /// Stores a message that came in (a sync page's entry, or a Trouter delivery's resource) in `conversation`,
    /// through [`Outbox::receive`]; `None` when it isn't stored (see [`Synced::skipped`]). One of ours that isn't
    /// stored, because its version or its arrival time can't be read, is still matched to its outbox row
    /// ([`Outbox::match_arrival`]). A control message (`Control/…`) is a signal: `None`, never stored and never
    /// matched to an outbox row ([`Synced::control`]).
    ///
    /// # Errors
    ///
    /// [`CatchUpError::Outbox`] when the store fails, an unknown conversation included (a delivery for one is
    /// followed by [`CatchUp::sync_messages`], which reads it).
    pub fn receive_message(
        &self,
        store: &mut Store,
        conversation: &str,
        message: &chat::Message,
    ) -> Result<Option<Received>, CatchUpError> {
        if message.message_type.is_control() {
            return Ok(None);
        }
        let Some(row) = message_row(conversation, message) else {
            if let Some(arrival) = arrival(conversation, message) {
                self.outbox.match_arrival(store, &arrival)?;
            }
            return Ok(None);
        };
        let reactions = reaction_set(message);
        let received = self.outbox.receive(store, &row, reactions.as_deref())?;
        // a channel's message names its post (M2 Task 16): kept beside it, whatever version the store took
        if let Some(root) = message.reply_root(conversation) {
            store.set_reply_root(conversation, &message.id, &root)?;
        }
        Ok(Some(received))
    }

    /// The first read of a conversation's messages: its newest page, with the page's `backwardLink` kept for older
    /// history.
    async fn first_read(
        &self,
        store: &mut Store,
        reads: &ChatReads<'_, C>,
        conversation: &str,
    ) -> Result<Synced, CatchUpError> {
        let (kept_gap, kept_gap_until) = store
            .sync_state(conversation)?
            .map_or((None, None), |state| (state.gap_link, state.gap_until));
        let page = self
            .waiting(store, reads.messages(conversation, self.page_size))
            .await?;
        let synced = self.apply_page(store, conversation, &page)?;
        let backward_link = page.backward_link().map(str::to_owned);
        let state = SyncState {
            conversation_id: conversation.to_owned(),
            sync_link: page.sync_state().map(str::to_owned),
            history_complete: backward_link.is_none() && kept_gap.is_none(),
            backward_link,
            backward_oldest: oldest_arrival(&page),
            // a page without a time it can read bounds nothing: the next sync answer is read to its end
            sync_until: Some(segment_end(&page).unwrap_or(0)),
            gap_link: kept_gap,
            gap_until: kept_gap_until,
            updated: (self.clock)(),
        };
        store.set_sync_state(&state)?;
        Ok(synced)
    }

    /// Follows a chain of `backwardLink`s from `from`, for up to `budget` pages, until a page ends it by `bound`
    /// ([`Bound::ends`]: what is behind it is history the sync before had, or older) or the chain ends; under a time
    /// bound whatever each page brings on the way (a page that changes nothing, its messages delivered already, may
    /// still have messages the store never had behind it). Returns the link left unfollowed, if any, and what the
    /// pages read did.
    async fn follow_back(
        &self,
        store: &mut Store,
        conversation: &str,
        from: Option<&str>,
        budget: usize,
        bound: Bound,
    ) -> Result<(Option<String>, Synced), CatchUpError> {
        let reads = ChatReads::new(&self.client);
        let mut synced = Synced::default();
        let mut link = from.map(str::to_owned);
        let mut read = 0;
        while let Some(next) = link {
            if read == budget {
                return Ok((Some(next), synced));
            }
            let page = self.waiting(store, reads.messages_from(conversation, &next)).await?;
            let applied = self.apply_page(store, conversation, &page)?;
            synced.add(applied);
            read += 1;
            link = page
                .backward_link()
                .filter(|_| !bound.ends(&page, &applied))
                .map(str::to_owned);
        }
        Ok((None, synced))
    }

    /// Stores a page's messages, the oldest first, once every entry it can't read that carries an outbox row's
    /// `clientmessageid` is recorded on that row ([`CatchUp::note_unreadable`]).
    fn apply_page(&self, store: &mut Store, conversation: &str, page: &MessagePage) -> Result<Synced, CatchUpError> {
        self.note_unreadable(store, conversation, page)?;
        let mut synced = Synced {
            skipped: page.unreadable.len(),
            pages: 1,
            ..Synced::default()
        };
        for message in page.messages.iter().rev() {
            if message.message_type.is_control() {
                synced.control += 1;
                continue;
            }
            match self.receive_message(store, conversation, message)? {
                Some(received) => {
                    synced.count(received.applied);
                    self.message_stored(conversation, &message.id, received.applied);
                    if has_unreadable_reactions(message) {
                        synced.skipped += 1;
                    }
                }
                None => synced.skipped += 1,
            }
        }
        Ok(synced)
    }

    /// A sync page's message as the store took it: kept for [`CatchUp::take_changes`] when it was new or newer.
    fn message_stored(&self, conversation: &str, id: &str, applied: Applied) {
        if applied == Applied::Ignored {
            return;
        }
        self.changed(|changes| {
            changes.messages.push(StoredMessage {
                conversation: conversation.to_owned(),
                id: id.to_owned(),
                applied,
            });
            changes.conversation(conversation);
        });
    }

    /// Every entry of a page it can't read at all that carries a `clientmessageid` is handed to the outbox
    /// ([`Outbox::unreadable_seen`]), before the page is applied or any link moves past it: the message post of
    /// `conversation` with that id, if it had an attempt, may have arrived, and its row records it, so its check
    /// doesn't send it again on its own however far the reads move on.
    fn note_unreadable(&self, store: &mut Store, conversation: &str, page: &MessagePage) -> Result<(), CatchUpError> {
        for entry in &page.unreadable {
            if let Some(client_message_id) = entry.get("clientmessageid").and_then(Value::as_str) {
                self.outbox.unreadable_seen(store, conversation, client_message_id)?;
            }
        }
        Ok(())
    }

    /// One conversation of the list: kept, its messages synced when it is new or changed, then what the list says of
    /// it (its row, whether it is empty, its read horizon, its members) whether or not they synced. Messages that
    /// failed keep the conversation to be synced again before its new version is stored, so neither a later catch-up
    /// nor a later listing of it skips them; but not when the account can't read it ([`Unreadable`]). One known to be
    /// unreadable ([`CatchUp::mark_unreadable`]) has neither its messages nor its members read until the list shows it
    /// changed; one forgotten as deleted isn't even added back until then ([`still_forgotten`]).
    async fn catch_up_conversation(
        &self,
        store: &mut Store,
        listed: &chat::Conversation,
    ) -> Result<Synced, CatchUpError> {
        let stored = store.conversation(&listed.id)?;
        let version = known_i64(&listed.version);
        if stored.is_none() && still_forgotten(store, listed, version)? {
            log::debug!(
                "catch-up: conversation {} skipped: forgotten as deleted, and the list shows it unchanged",
                listed.id
            );
            return Ok(Synced::default());
        }
        if stored.is_none() {
            let added = store.upsert_conversation(&conversation_row(listed, 0))?;
            self.conversation_changed(&listed.id, added);
        }
        let state = store.sync_state(&listed.id)?;
        let changed = listing_changed(listed, stored.as_ref(), version, state.as_ref());
        let left = !changed && state.as_ref().is_some_and(marked_unreadable);
        let messages = if changed || state.is_none() {
            self.sync_messages(store, &listed.id).await
        } else {
            Ok(Synced::default())
        };
        if let Err(error) = &messages
            && Unreadable::of(&listed.id, error).is_none()
        {
            store.set_unsynced(&listed.id)?;
        }
        self.store_listing(store, listed, stored.as_ref(), version)?;
        let members = if left {
            Ok(())
        } else {
            self.refresh_members(store, listed).await
        };
        let synced = messages?;
        members?;
        Ok(synced)
    }

    /// What the list says of a conversation it brought: its row at the listed version, whether it is empty, the
    /// account's read horizon.
    fn store_listing(
        &self,
        store: &mut Store,
        listed: &chat::Conversation,
        stored: Option<&Conversation>,
        version: Option<i64>,
    ) -> Result<(), CatchUpError> {
        // an unreadable version keeps the stored row: as new as itself, only a later last activity is taken
        let kept = stored.map_or(0, |stored| stored.version);
        let row = conversation_row(listed, version.unwrap_or(kept));
        let applied = store.upsert_conversation(&row)?;
        self.conversation_changed(&listed.id, applied);
        // the store takes a later last activity whatever the version (`Ignored` then), and that is a change too
        if let Some(stored) = stored
            && advances(row.last_activity, stored.last_activity)
        {
            self.changed(|changes| changes.conversation(&listed.id));
        }
        // whether the service calls it empty (the chat list leaves such a chat out), as the list says it now
        if store.set_conversation_empty(&listed.id, listed.is_empty_conversation())? {
            self.changed(|changes| changes.conversation(&listed.id));
        }
        self.store_horizon(store, listed)
    }

    /// The account's own notes ([`NOTES`]), which no conversation list brings (when `listed` does, they were synced
    /// with it): synced when CSA listed them among the favourites or the store has them already. Once the list's sync link is kept, so a failure here (in
    /// [`CatchUpReport::failed`], and the catch-up is tried again) never holds the list back.
    async fn catch_up_notes(
        &self,
        store: &mut Store,
        listed: &[chat::Conversation],
        report: &mut CatchUpReport,
    ) -> Result<(), CatchUpError> {
        if listed.iter().any(|conversation| conversation.id == NOTES) {
            return Ok(());
        }
        let favourite = store
            .favorites()?
            .is_some_and(|favorites| favorites.iter().any(|id| id == NOTES));
        if !favourite && store.conversation(NOTES)?.is_none() {
            return Ok(());
        }
        match self.sync_messages(store, NOTES).await {
            Ok(synced) => report.messages.add(synced),
            Err(error) => report.failed.push(ConversationFailed {
                conversation: NOTES.to_owned(),
                error,
            }),
        }
        Ok(())
    }

    /// Keeps what became of one conversation's sync in a catch-up: in `report`, and in the store as synced or to be
    /// synced again by every later catch-up; one the account can't read as neither ([`CatchUp::leave_unreadable`]).
    /// Whether the store kept it: when it couldn't, the list's sync link stays.
    fn settle(
        &self,
        store: &mut Store,
        conversation: &str,
        result: Result<Synced, CatchUpError>,
        report: &mut CatchUpReport,
    ) -> bool {
        let error = match result {
            Ok(synced) => {
                report.messages.add(synced);
                // one that stays kept is only synced once more
                if let Err(error) = store.set_synced(conversation) {
                    log::warn!("catch-up: conversation {conversation} synced, but still kept to be synced: {error}");
                }
                return true;
            }
            Err(error) => error,
        };
        if let Some(unreadable) = Unreadable::of(conversation, &error) {
            return self.leave_unreadable(store, unreadable, report);
        }
        let kept = store.set_unsynced(conversation);
        if let Err(kept) = &kept {
            log::warn!("catch-up: conversation {conversation} can't be kept to be synced again: {kept}");
        }
        report.failed.push(ConversationFailed {
            conversation: conversation.to_owned(),
            error,
        });
        kept.is_ok()
    }

    /// A conversation the account can't read ([`Unreadable`]): no longer kept to be synced again, or forgotten when its
    /// thread is gone (told as a change, so a view showing it drops it; its writes still to go are told failed,
    /// [`Outbox::forgotten`]). One `info` line tells which, with the status and the service's code, never its answer.
    /// Whether the store kept that.
    fn leave_unreadable(&self, store: &mut Store, unreadable: Unreadable, report: &mut CatchUpReport) -> bool {
        let id = unreadable.conversation.as_str();
        let (status, code) = (unreadable.status, unreadable.code.as_deref().unwrap_or("no code"));
        let left = if unreadable.forgotten {
            log::info!("catch-up: conversation {id} forgotten: the service answered HTTP {status} ({code}), deleted");
            store.forget_conversation(id).map(|forgot| {
                if forgot.stored {
                    self.changed(|changes| changes.conversation(id));
                }
                self.outbox.forgotten(forgot.unsent);
            })
        } else {
            log::info!(
                "catch-up: conversation {id} skipped: the service answered HTTP {status} ({code}), not readable"
            );
            store.set_synced(id).and_then(|_| self.mark_unreadable(store, id))
        };
        if let Err(error) = &left {
            log::warn!("catch-up: conversation {id} can't be left unsynced: {error}");
        }
        report.unreadable.push(unreadable);
        left.is_ok()
    }

    /// A conversation the account can't read that no sync ever read gets a sync state with no link and no bound (every
    /// read keeps a bound: [`marked_unreadable`]), so a later listing reads it again only when it shows it changed
    /// ([`listing_changed`]), not on every full listing; its next read is a first one. A state kept from when it could
    /// be read stays as it is (a listing skips that one unless it is newer already).
    fn mark_unreadable(&self, store: &mut Store, id: &str) -> Result<(), StoreError> {
        if store.sync_state(id)?.is_some() {
            return Ok(());
        }
        store.set_sync_state(&SyncState {
            conversation_id: id.to_owned(),
            sync_link: None,
            backward_link: None,
            backward_oldest: None,
            sync_until: None,
            gap_link: None,
            gap_until: None,
            history_complete: false,
            updated: (self.clock)(),
        })
    }

    /// A conversation the store doesn't know, read as a thread: kept at version 0 (the list's version replaces it),
    /// with its members. The engine adds one this way when a delivery names it, before it stores the delivery. The
    /// account's own notes ([`NOTES`]) are no thread: kept as they are, with nothing read.
    pub(crate) async fn add_conversation(&self, store: &mut Store, id: &str) -> Result<(), CatchUpError> {
        if id == NOTES {
            let row = Conversation {
                id: NOTES.to_owned(),
                kind: ConversationKind::Other(NOTES_KIND.to_owned()),
                topic: None,
                version: 0,
                last_activity: None,
            };
            let added = store.upsert_conversation(&row)?;
            self.conversation_changed(id, added);
            return Ok(());
        }
        let reads = ChatReads::new(&self.client);
        let thread = self.waiting(store, reads.thread(id)).await?;
        let row = Conversation {
            id: id.to_owned(),
            kind: store_kind(&thread.kind()),
            topic: thread.topic().map(str::to_owned),
            version: 0,
            last_activity: None,
        };
        let added = store.upsert_conversation(&row)?;
        self.conversation_changed(id, added);
        if !thread.kind().is_read_only() {
            self.store_members(store, &thread)?;
        }
        Ok(())
    }

    /// The account's own read horizon from the list, when both its parts can be read.
    fn store_horizon(&self, store: &mut Store, listed: &chat::Conversation) -> Result<(), CatchUpError> {
        if let Some(horizon) = listed.consumption_horizon()
            && let Some(applied) = self.receive_horizon(store, &listed.id, &horizon)?
        {
            self.conversation_changed(&listed.id, applied);
        }
        Ok(())
    }

    /// A thread's members by its `rosterVersion`, when that can be read.
    fn store_members(&self, store: &mut Store, thread: &chat::Thread) -> Result<(), CatchUpError> {
        let Some(roster) = known_i64(&thread.roster_version) else {
            return Ok(());
        };
        let members: Vec<Member> = thread
            .members
            .iter()
            .map(|member| Member::new(&member.id, member.role.as_deref()))
            .collect();
        let applied = store.replace_members(&thread.id, roster, &members)?;
        self.conversation_changed(&thread.id, applied);
        Ok(())
    }

    /// Stores the account's own read horizon in `conversation` (from the conversation list, or a Trouter delivery),
    /// by the time it was set; `None` when its message or its time can't be read.
    ///
    /// # Errors
    ///
    /// [`CatchUpError::Store`] when the store fails, an unknown conversation included.
    pub fn receive_horizon(
        &self,
        store: &mut Store,
        conversation: &str,
        horizon: &chat::ConsumptionHorizon,
    ) -> Result<Option<Applied>, CatchUpError> {
        let (Some(last_read), Some(updated)) = (horizon.last_read_arrival, horizon.updated) else {
            return Ok(None);
        };
        let (Ok(last_read_arrival), Ok(updated)) = (i64::try_from(last_read), i64::try_from(updated)) else {
            return Ok(None);
        };
        let applied = store.set_read_horizon(&ReadHorizon {
            conversation_id: conversation.to_owned(),
            user: self.me.as_str().to_owned(),
            last_read_arrival,
            updated,
            raw: Some(horizon.raw.clone()),
        })?;
        Ok(Some(applied))
    }

    /// A chat's members read again as a thread, now: after the account added people to it.
    ///
    /// # Errors
    ///
    /// [`CatchUpError::Read`] when the read fails, [`CatchUpError::Store`] when the store does.
    pub(crate) async fn sync_members(&self, store: &mut Store, id: &str) -> Result<(), CatchUpError> {
        let reads = ChatReads::new(&self.client);
        let thread = self.waiting(store, reads.thread(id)).await?;
        self.store_members(store, &thread)
    }

    /// A chat's members, read as a thread when the list's `rosterVersion` is newer than the stored one, or there is
    /// none stored, or the list's can't be read (it may be newer). A list without one keeps the stored members.
    /// Channels and the other conversations that are no chat keep none.
    async fn refresh_members(&self, store: &mut Store, listed: &chat::Conversation) -> Result<(), CatchUpError> {
        if listed.is_read_only() {
            return Ok(());
        }
        let newer = match (store.roster_version(&listed.id)?, listed.roster_version()) {
            (None, _) => true,
            (Some(_), Field::Absent) => false,
            (Some(stored), listed) => known_i64(&listed).is_none_or(|listed| listed > stored),
        };
        if newer {
            let reads = ChatReads::new(&self.client);
            let thread = self.waiting(store, reads.thread(&listed.id)).await?;
            self.store_members(store, &thread)?;
        }
        Ok(())
    }
}

impl<C: Credentials, T: Transport<Error = HttpError> + Send> ConversationSync for CatchUp<C, T> {
    fn serve<'a>(&'a self, store: &'a mut Store) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        match &self.meanwhile {
            Some(meanwhile) => meanwhile.serve(store),
            None => Box::pin(std::future::pending()),
        }
    }

    async fn sync_conversation(&self, store: &mut Store, conversation: &str) -> Result<(), SyncError> {
        self.sync_messages(store, conversation)
            .await
            .map(|_| ())
            .map_err(SyncError::from)
    }

    async fn sync_conversation_since(
        &self,
        store: &mut Store,
        conversation: &str,
        since: i64,
    ) -> Result<(), SyncError> {
        self.sync_since(store, conversation, since)
            .await
            .map_err(SyncError::from)
    }
}

/// Logs what a catch-up did.
fn log_report(report: &CatchUpReport, first: bool) {
    log::info!(
        "catch-up: {} conversation(s) listed ({} skipped), {} message(s) new, {} newer, {} skipped (not a member or \
         deleted), {} failed{}",
        report.conversations,
        report.skipped,
        report.messages.inserted,
        report.messages.updated,
        report.unreadable.len(),
        report.failed.len(),
        if first { " (the first)" } else { "" }
    );
    for failed in &report.failed {
        log::warn!(
            "catch-up: conversation {} not synced: {}",
            failed.conversation,
            failed.error
        );
    }
}

/// Whether a read was answered 404 without a `Location` (with one it is a region hop, [`ChatReadError::RegionMoved`]).
fn not_found(error: &CatchUpError) -> bool {
    matches!(error, CatchUpError::Read(ChatReadError::Http(HttpError::Status(answer))) if answer.status() == 404)
}

/// A conversation of the list as the store keeps it, at `version`.
fn conversation_row(listed: &chat::Conversation, version: i64) -> Conversation {
    Conversation {
        id: listed.id.clone(),
        kind: store_kind(&listed.kind()),
        topic: listed.topic().map(str::to_owned),
        version,
        last_activity: listed.last_message().and_then(|message| message.arrival_time_ms()),
    }
}

/// Whether the list shows a stored conversation changed since it was stored: a newer version (one it can't read is a
/// change). One the account couldn't read ([`marked_unreadable`]) has changed at a later last message too, whatever its
/// version says, so the account added back to a thread it left reads it again.
fn listing_changed(
    listed: &chat::Conversation,
    stored: Option<&Conversation>,
    version: Option<i64>,
    state: Option<&SyncState>,
) -> bool {
    let (Some(stored), Some(version)) = (stored, version) else {
        return true;
    };
    let unreadable = state.is_some_and(marked_unreadable);
    let last_activity = listed.last_message().and_then(|message| message.arrival_time_ms());
    version > stored.version || (unreadable && advances(last_activity, stored.last_activity))
}

/// Whether `listed` is a thread the store forgot as deleted ([`Store::forget_conversation`]) that the list shows
/// unchanged since: no newer version and no later last message (the rule [`listing_changed`] keeps for one the account
/// couldn't read). A version it can't read may be newer: such a listing reads the thread again.
fn still_forgotten(store: &Store, listed: &chat::Conversation, version: Option<i64>) -> Result<bool, StoreError> {
    let Some(forgotten) = store.forgotten_thread(&listed.id)? else {
        return Ok(false);
    };
    let last_activity = listed.last_message().and_then(|message| message.arrival_time_ms());
    let newer = version.is_none_or(|version| version > forgotten.version);
    Ok(!newer && !advances(last_activity, forgotten.last_activity))
}

/// Whether `state` is the mark of a conversation the account couldn't read and no sync ever read
/// ([`CatchUp::mark_unreadable`]): no sync link and no bound, which every read keeps.
fn marked_unreadable(state: &SyncState) -> bool {
    state.sync_link.is_none() && state.sync_until.is_none()
}

/// Whether a listed last activity moves the stored one on: the store keeps the later of the two.
fn advances(listed: Option<i64>, stored: Option<i64>) -> bool {
    listed.is_some_and(|listed| stored.is_none_or(|stored| listed > stored))
}

/// The store's kind for the model's: a kind it doesn't name is kept as text the store's own names can't be.
fn store_kind(kind: &chat::ConversationKind) -> ConversationKind {
    match kind {
        chat::ConversationKind::OneToOne => ConversationKind::OneToOne,
        chat::ConversationKind::Group => ConversationKind::Group,
        chat::ConversationKind::Channel => ConversationKind::Channel,
        chat::ConversationKind::Notifications => ConversationKind::Other("notifications".to_owned()),
        chat::ConversationKind::Unknown(thread_type) => ConversationKind::Other(format!("thread_type:{thread_type}")),
    }
}

/// A message as read, as the store keeps it in `conversation`; `None` when it can't be: it names another
/// conversation, or its version or its arrival time can't be read (the arrival time falls back to the id, which is
/// that time).
fn message_row(conversation: &str, message: &chat::Message) -> Option<Message> {
    if message.conversation_id.as_deref().is_some_and(|id| id != conversation) {
        return None;
    }
    let version = known_i64(&message.version)?;
    let arrival_time = arrival_time(message)?;
    let properties = (!message.properties.is_empty()).then(|| Value::Object(message.properties.clone()).to_string());
    Some(Message {
        conversation_id: conversation.to_owned(),
        id: message.id.clone(),
        version,
        client_message_id: message.client_message_id.as_known().cloned(),
        sender: message.sender().map(str::to_owned),
        sender_name: message.display_name.clone(),
        message_type: message.message_type.as_str().to_owned(),
        content: message.content.clone(),
        properties,
        arrival_time,
        compose_time: message.compose_time_ms(),
        edit_time: known_i64(&message.edit_time()),
        delete_time: known_i64(&message.delete_time()),
    })
}

/// What a message that can't be stored is matched to an outbox row by: `None` when it names another conversation, or
/// has no id.
fn arrival<'a>(conversation: &'a str, message: &'a chat::Message) -> Option<Arrival<'a>> {
    if message.conversation_id.as_deref().is_some_and(|id| id != conversation) || message.id.is_empty() {
        return None;
    }
    Some(Arrival {
        conversation,
        id: &message.id,
        client_message_id: message.client_message_id.as_known().map(String::as_str),
        sender: message.sender(),
    })
}

/// A message's whole reaction set (`properties.emotions`), when it is there and every time in it can be read; `None`
/// leaves the stored reactions as they are.
fn reaction_set(message: &chat::Message) -> Option<Vec<Reaction>> {
    let Field::Known(keys) = message.reactions() else {
        return None;
    };
    let mut set = Vec::new();
    for reaction in &keys {
        for user in &reaction.users {
            set.push(Reaction {
                key: reaction.key.clone(),
                user: user.mri.clone(),
                time: known_i64(&user.time)?,
            });
        }
    }
    Some(set)
}

/// Whether a message has a reaction set that isn't stored: one in a form this build can't read.
fn has_unreadable_reactions(message: &chat::Message) -> bool {
    !matches!(message.reactions(), Field::Absent) && reaction_set(message).is_none()
}

/// A message's arrival time: its `originalarrivaltime`, or else its id, which is that time.
fn arrival_time(message: &chat::Message) -> Option<i64> {
    message.arrival_time_ms().or_else(|| message.id.parse().ok())
}

/// The oldest arrival time among a page's messages.
fn oldest_arrival(page: &MessagePage) -> Option<i64> {
    page.messages.iter().filter_map(arrival_time).min()
}

/// How far back a page of a sync answer reaches (ms): the start of the complete segment the service reports with it
/// (`lastCompleteSegmentStartTime`: the oldest message's id on the page, or the start of the segment it merged into),
/// else the oldest arrival time on the page.
fn reach(page: &MessagePage) -> Option<i64> {
    known_i64(&page.last_complete_segment_start()).or_else(|| oldest_arrival(page))
}

/// Whether a page of a sync answer reached back to `until`, the end of the segment the sync before it was complete to:
/// a page holds its messages newest first by arrival, so every message that arrived since is on it or on a page read
/// before it, and what its `backwardLink` leads to is history the sync before had, or older.
fn reached(page: &MessagePage, until: i64) -> bool {
    reach(page).is_some_and(|start| start <= until)
}

/// The time a sync answer's page says the conversation is complete to (ms): its `lastCompleteSegmentEndTime`, else the
/// newest version on the page; none for an empty page without the time.
fn segment_end(page: &MessagePage) -> Option<i64> {
    known_i64(&page.last_complete_segment_end()).or_else(|| {
        page.messages
            .iter()
            .filter_map(|message| known_i64(&message.version))
            .max()
    })
}

/// How far back a sync answer of `state`'s sync link is read: its kept `sync_until`, or for a state written before it
/// was kept, when the state was written (less [`CLOCK_SKEW_MS`]), which its sync answer was read before.
fn sync_bound(state: &SyncState) -> i64 {
    state.sync_until.unwrap_or_else(|| written_bound(state))
}

/// Where `state`'s gap ends: at its kept `gap_until`; a gap kept before that was, at a page whose messages were all
/// stored already ([`Bound::Stored`]).
fn gap_bound(state: &SyncState) -> Bound {
    state.gap_until.map_or(Bound::Stored, Bound::Until)
}

/// Where a chain of a sync answer's pages ends (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bound {
    /// At a page that reached back to this time ([`reached`]).
    Until(i64),
    /// A gap kept before its bound was: at a page whose messages the store all had, as the messages the sync before
    /// it stored are. Where the gap was cut its pages are messages missed, newer than those.
    Stored,
}

impl Bound {
    /// Whether `page`, stored as `applied` tells, ends the chain: what its `backwardLink` leads to isn't read.
    fn ends(self, page: &MessagePage, applied: &Synced) -> bool {
        match self {
            Self::Until(until) => reached(page, until),
            Self::Stored => !page.messages.is_empty() && applied.changed() == 0 && applied.unchanged > 0,
        }
    }

    /// The time, when it is one.
    fn until(self) -> Option<i64> {
        match self {
            Self::Until(until) => Some(until),
            Self::Stored => None,
        }
    }
}

fn written_bound(state: &SyncState) -> i64 {
    state.updated.saturating_sub(CLOCK_SKEW_MS)
}

fn known_i64(field: &Field<u64>) -> Option<i64> {
    field.as_known().and_then(|value| i64::try_from(*value).ok())
}
