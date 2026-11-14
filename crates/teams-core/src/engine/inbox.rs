//! The engine's requests as they come in, shared by its loop and its catch-up. The loop takes them one at a time, in
//! the order they were made ([`Inbox::next`]). While a read of the catch-up waits on the service (a catch-up, a
//! conversation's sync, older history, the outbox's check), the inbox answers what the views read from the store as it
//! stands, and keeps every other request, in order, for the loop to take once the read is over ([`Meanwhile`]), except
//! the priority reads below, which take the foreground lane while the catch-up waits: an account whose catch-up runs
//! for minutes still opens its chats, channels and lists at once.
//!
//! - **Answered meanwhile**: every store query and cached person, team and chat listing. Person lookups wait for
//!   the main loop. The teams scheduler lives here so its one read can start, settle and publish during catch-up.
//! - **Priority reads**: opened conversations and older history use one foreground lane beside the pending
//!   background read. Requests remain FIFO within that lane, except one for a conversation being read waits until
//!   its cursor is stored. Duplicate message syncs share the successful result; canceled queued reads are skipped.
//!   Teams reads use the same lane, so requesting many chats and lists cannot create a burst of parallel reads.
//! - **Read receipts**: queued locally at once, advancing the shared read horizon for the messages seen; the
//!   transport waits its turn with the other writes. Queries and read receipts also run while an outbox attempt waits.
//! - **Call actions**: handed to the calling at once ([`Inbox::hand_calls_to`]), as the loop hands them: the calling
//!   runs on its own, so a Call pressed while a catch-up runs for minutes rings now, not once that is over (the
//!   header's Call went, and nothing happened until then). A group chat's call reads whom it calls from the store.
//!   Once a shutdown is kept, none goes any more: each is answered [`EngineError::Stopped`]. A call's health (no
//!   action: it reads the call's media) goes to the calling at once the same way.
//! - **Kept**: everything else (another write, account sync, presence, the shutdown).
//! - **Catch-up publication**: stored changes are published as each list page and conversation finishes
//!   ([`Meanwhile::progress`]), and any remainder at the end ([`Inbox::brought`]), so views can query what is ready
//!   while the rest loads. Noticed team threads are kept for the loop's next tick.
//! - Such an answer comes before a request made earlier that is kept (a write's row isn't in it yet); what that request
//!   changes is told by its events, as always.

use std::collections::{HashMap, VecDeque};
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{MutexGuard, OnceLock, PoisonError};

use teams_proto::Credentials;
use teams_store::{Applied, OutboxEntry, ReadHorizon, Store, StoreError};
use tokio::sync::{Mutex, mpsc, oneshot};

use super::{Action, EngineError, Event, MessageChange, Query, Request, Responder, Source};
use crate::calling::CallActions;
use crate::outbox::{Command, enqueue};
use crate::sync::{Changes, Meanwhile, PriorityRead};
use crate::teams::{Teams, TeamsRead, TeamsReadError};
use crate::{profiles, teams};

/// See the module docs.
pub(super) struct Inbox<C> {
    /// The account's MRI, whose read horizon and unread counts the queries answer with.
    me: String,
    cloud: teams_proto::Cloud,
    /// The names the binary knows ([`Parts::known_names`](super::Parts::known_names)), for a person answered meanwhile.
    known: HashMap<String, String>,
    requests: Mutex<Requests>,
    teams: std::sync::Mutex<Teams<C>>,
    events: mpsc::UnboundedSender<Event>,
    read_only: bool,
    /// The calling a call action goes to meanwhile, once it runs.
    calls: OnceLock<CallActions>,
    /// A shutdown was kept: the engine stops once the read is over, and starts no call meanwhile.
    stopping: AtomicBool,
    /// A read of the teams changed the favourites: the account's own notes may be due a sync.
    teams_changed: AtomicBool,
    /// Team threads the reads and deliveries brought since the last tick.
    noticed: std::sync::Mutex<Vec<String>>,
}

/// The requests not taken yet: those kept while a read waited first, then the channel's.
struct Requests {
    received: mpsc::UnboundedReceiver<Request>,
    kept: VecDeque<Request>,
}

impl<C: Credentials + 'static> Inbox<C> {
    pub(super) fn cloud(&self) -> &teams_proto::Cloud {
        &self.cloud
    }

    pub(super) fn new(
        me: String,
        known: HashMap<String, String>,
        received: mpsc::UnboundedReceiver<Request>,
        events: mpsc::UnboundedSender<Event>,
        read_only: bool,
        teams: Teams<C>,
    ) -> Self {
        Self {
            cloud: teams.cloud().clone(),
            me,
            known,
            events,
            read_only,
            teams: std::sync::Mutex::new(teams),
            calls: OnceLock::new(),
            stopping: AtomicBool::new(false),
            teams_changed: AtomicBool::new(false),
            noticed: std::sync::Mutex::default(),
            requests: Mutex::new(Requests {
                received,
                kept: VecDeque::new(),
            }),
        }
    }

    async fn next_priority(&self, store: &mut Store, blocked: &[String]) -> PriorityRead {
        let mut requests = self.requests.lock().await;
        loop {
            let teams_busy = self.teams().busy();
            if let Some(at) = requests.kept.iter().position(|request| {
                !self.stopping.load(Ordering::Relaxed) && priority_allowed(request, blocked, teams_busy)
            }) {
                let request = requests.kept.remove(at);
                if let Some(request) = request
                    && let Some(read) = self.priority_request(store, request)
                {
                    return read;
                }
                continue;
            }
            tokio::select! {
                read = self.next_teams(), if teams_busy => self.store_teams(store, read),
                request = requests.received.recv() => {
                    let Some(request) = request else { return std::future::pending().await };
                    if !self.stopping.load(Ordering::Relaxed) && priority_allowed(&request, blocked, teams_busy) {
                        if let Some(read) = self.priority_request(store, request) {
                            return read;
                        }
                    } else if let Some(kept) = self.meanwhile(store, request) {
                        requests.kept.push_back(kept);
                    }
                }
            }
        }
    }

    pub(super) fn teams(&self) -> MutexGuard<'_, Teams<C>> {
        self.teams.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) async fn next_teams(&self) -> Option<Result<TeamsRead, TeamsReadError>> {
        poll_fn(|cx| self.teams().poll_next(cx)).await
    }

    pub(super) fn take_teams_changed(&self) -> bool {
        self.teams_changed.swap(false, Ordering::Relaxed)
    }

    pub(super) fn store_teams(&self, store: &mut Store, read: Option<Result<TeamsRead, TeamsReadError>>) {
        let Some(read) = read else { return };
        let result = self.teams().store(store, read);
        let event = match result {
            Ok(Ok(true)) => {
                self.teams_changed.store(true, Ordering::Relaxed);
                Event::Teams
            }
            Ok(Ok(false)) => return,
            Ok(Err(error)) => Event::Failed {
                during: teams::READING_TEAMS,
                error: error.into(),
            },
            Err(error) => Event::Failed {
                during: "storing the teams",
                error: error.into(),
            },
        };
        super::emit_to(&self.events, event);
    }

    fn priority_request(&self, store: &mut Store, request: Request) -> Option<PriorityRead> {
        match request {
            Request::SyncConversation(_, reply) if reply.is_closed() => None,
            Request::LoadOlder(_, reply) if reply.is_closed() => None,
            Request::SyncConversation(conversation, reply) => {
                log::info!("action: a sync of {conversation}");
                Some(PriorityRead {
                    conversation,
                    older: false,
                    complete: Box::new(move |result| {
                        let _ = reply.send(result.map(Option::unwrap_or_default).map_err(EngineError::from));
                    }),
                })
            }
            Request::LoadOlder(conversation, reply) => {
                log::info!("action: older history of {conversation}");
                Some(PriorityRead {
                    conversation,
                    older: true,
                    complete: Box::new(move |result| {
                        let _ = reply.send(result.map_err(EngineError::from));
                    }),
                })
            }
            Request::Teams(reply) => {
                let teams = self.teams().teams(store);
                let _ = reply.send(teams.map_err(EngineError::from));
                None
            }
            Request::ChatListing(reply) => {
                let listing = self.teams().chat_listing(store);
                let _ = reply.send(listing.map_err(EngineError::from));
                None
            }
            _ => None,
        }
    }

    pub(super) fn notice(&self, conversation: &str) {
        if teams_proto::is_team_thread_id(conversation) {
            self.noticed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(conversation.to_owned());
        }
    }

    pub(super) fn take_noticed(&self) -> Vec<String> {
        std::mem::take(&mut *self.noticed.lock().unwrap_or_else(PoisonError::into_inner))
    }

    pub(super) fn brought(&self, store: &Store, changes: Changes, source: Source) {
        for stored in changes.messages {
            let message = match store.message(&stored.conversation, &stored.id) {
                Ok(message) => message.map(Box::new),
                Err(error) => {
                    super::emit_to(
                        &self.events,
                        Event::Failed {
                            during: "reading a synced message back",
                            error: error.into(),
                        },
                    );
                    None
                }
            };
            let change = if stored.applied == Applied::Inserted {
                MessageChange::New
            } else {
                MessageChange::Other
            };
            super::emit_to(
                &self.events,
                Event::Message {
                    conversation: stored.conversation,
                    id: stored.id,
                    change,
                    applied: stored.applied,
                    message,
                    source,
                },
            );
        }
        for conversation in changes.conversations {
            self.notice(&conversation);
            super::emit_to(&self.events, Event::ConversationChanged { conversation });
        }
    }

    /// From now on a call action that comes while a read waits goes to `calls` at once (see the module docs).
    pub(super) fn hand_calls_to(&self, calls: CallActions) {
        let _ = self.calls.set(calls);
    }

    /// The next request, in the order they were made; `None` once every request handle is gone and every request
    /// was taken. Loses nothing when dropped while it waits.
    pub(super) async fn next(&self) -> Option<Request> {
        let mut requests = self.requests.lock().await;
        if let Some(kept) = requests.kept.pop_front() {
            return Some(kept);
        }
        requests.received.recv().await
    }

    /// Answers a request from the store alone when it can be (see the module docs); the request to keep, if any.
    fn meanwhile(&self, store: &mut Store, request: Request) -> Option<Request> {
        match request {
            Request::Write(
                Action::Read {
                    conversation,
                    message_id,
                },
                reply,
            ) => match self.queue_read(store, conversation, message_id) {
                Ok(row) => Some(Request::SendQueued(row.id, reply)),
                Err(error) => {
                    let _ = reply.send(Err(error));
                    None
                }
            },
            Request::Query(query) => {
                self.answer(store, query);
                None
            }
            Request::Call(_, _, reply) if self.stopping.load(Ordering::Relaxed) => {
                log::info!("a call action came after the engine's shutdown: refused");
                let _ = reply.send(Err(EngineError::Stopped));
                None
            }
            Request::Call(action, asked, reply) => match self.calls.get() {
                Some(calls) => {
                    super::hand_call(&self.cloud, store, &self.me, calls, (action, asked), reply);
                    None
                }
                None => Some(Request::Call(action, asked, reply)),
            },
            Request::CallHealth(call, reply) => match self.calls.get() {
                Some(calls) => {
                    calls.health(call, reply);
                    None
                }
                None => Some(Request::CallHealth(call, reply)),
            },
            Request::CallMeetingOptions(call, reply) => match self.calls.get() {
                Some(calls) => {
                    calls.meeting_options(call, reply);
                    None
                }
                None => Some(Request::CallMeetingOptions(call, reply)),
            },
            Request::Person(mri, reply) => {
                let person = store.profile(&mri).and_then(|stored| {
                    profiles::stored_person(store, &mri, &self.known, profiles::due(stored.as_ref()))
                });
                let _ = reply.send(person.map_err(EngineError::from));
                Some(Request::Person(mri, answered()))
            }
            Request::Teams(reply) => {
                let _ = reply.send(teams::stored_teams(store).map_err(EngineError::from));
                Some(Request::Teams(answered()))
            }
            Request::ChatListing(reply) => {
                let _ = reply.send(teams::stored_listing(store).map_err(EngineError::from));
                Some(Request::ChatListing(answered()))
            }
            Request::Shutdown => {
                self.stopping.store(true, Ordering::Relaxed);
                Some(Request::Shutdown)
            }
            other => Some(other),
        }
    }

    /// A receipt records what was actually seen, even if its transport later fails. The service's timestamp-ordered
    /// horizon will reconcile it as usual; an older response cannot restore the badge, and messages arriving after
    /// this exact target still count unread. Keep this in the inbox so a slow catch-up cannot hold the local update.
    pub(super) fn queue_read(
        &self,
        store: &mut Store,
        conversation: String,
        message_id: String,
    ) -> Result<OutboxEntry, EngineError> {
        let command = read_command(store, conversation.clone(), message_id.clone())?;
        let row = enqueue(store, &command)?;
        let _ = self.events.send(Event::Queued(row.clone()));
        if !self.read_only {
            let message = store
                .message(&conversation, &message_id)?
                .ok_or_else(|| EngineError::UnknownMessage {
                    conversation: conversation.clone(),
                    message: message_id,
                })?;
            let before = store.read_horizon(&conversation, &self.me)?;
            if before
                .as_ref()
                .is_none_or(|before| before.last_read_arrival < message.arrival_time)
            {
                // Two seen-message bursts can queue in the same millisecond. Local progress must still win the tie.
                let updated = before.as_ref().map_or(row.created_at, |before| {
                    row.created_at.max(before.updated.saturating_add(1))
                });
                let applied = store.set_read_horizon(&ReadHorizon {
                    conversation_id: conversation.clone(),
                    user: self.me.clone(),
                    last_read_arrival: message.arrival_time,
                    updated,
                    raw: None,
                })?;
                if applied != Applied::Ignored {
                    let _ = self.events.send(Event::ReadHorizon { conversation });
                }
            }
        }
        Ok(row)
    }

    /// Answers a query from the store.
    pub(super) fn answer(&self, store: &Store, query: Query) {
        fn give<T>(reply: Responder<T>, answer: Result<T, StoreError>) {
            let _ = reply.send(answer.map_err(EngineError::from));
        }
        let me = self.me.as_str();
        match query {
            Query::Conversations(limit, reply) => give(reply, store.conversations_by_activity(limit)),
            Query::Conversation(id, reply) => give(reply, store.conversation(&id)),
            Query::ConversationPreview(id, reply) => give(reply, store.conversation_preview(&id)),
            Query::Messages {
                conversation,
                before,
                limit,
                reply,
            } => give(reply, store.messages_before(&conversation, before.as_ref(), limit)),
            Query::Message {
                conversation,
                id,
                reply,
            } => give(reply, store.message(&conversation, &id)),
            Query::Reactions {
                conversation,
                id,
                reply,
            } => give(reply, store.reactions(&conversation, &id)),
            Query::Members(conversation, reply) => give(reply, store.members(&conversation)),
            Query::GroupChatWith(members, reply) => give(reply, store.group_chat_with(&members)),
            Query::CallRuns(chat, reply) => give(reply, super::call_runs(&self.cloud, store, &chat)),
            Query::CallEnded(chat, start, reply) => give(reply, super::call_ended(store, &chat, &start)),
            Query::RecentReactions(limit, reply) => give(reply, store.recent_reactions(me, limit)),
            Query::FrequentReactions(limit, reply) => give(reply, store.frequent_reactions(me, limit)),
            Query::UnreadCount(conversation, reply) => give(reply, store.unread_count(&conversation, me)),
            Query::DisplayName(mri, reply) => give(reply, store.display_name(&mri)),
            Query::ReadHorizon(conversation, reply) => give(reply, store.read_horizon(&conversation, me)),
            Query::Activity(reply) => give(reply, crate::activity::ActivityFeed::from_store(store, me)),
            Query::HistoryComplete(conversation, reply) => give(
                reply,
                store
                    .sync_state(&conversation)
                    .map(|state| state.map(|state| state.history_complete)),
            ),
            Query::OutboxEntry(id, reply) => give(reply, store.outbox_entry(id)),
            Query::OutboxUnsettled(conversation, reply) => give(reply, store.outbox_unsettled(&conversation)),
            Query::LeftByEarlierRuns(reply) => give(reply, store.outbox_left_by_earlier_runs()),
            Query::ReplyRoots(conversation, reply) => give(reply, store.reply_roots(&conversation)),
            Query::Translations {
                conversation,
                language,
                ids,
                reply,
            } => give(reply, store.translations(&conversation, &language, &ids)),
            Query::Glossary {
                conversation,
                language,
                reply,
            } => give(reply, store.glossary(&conversation, &language)),
            Query::SentOriginals {
                conversation,
                keys,
                reply,
            } => give(reply, store.sent_originals(&conversation, &keys)),
            Query::CallTranscripts(conversation, reply) => give(reply, store.call_transcripts(&conversation)),
            Query::TranscriptLines(id, reply) => give(reply, store.transcript_lines(id)),
            Query::CalendarEvents {
                start_ms,
                end_ms,
                reply,
            } => give(reply, crate::calendar::stored_in(&self.cloud, store, start_ms, end_ms)),
            Query::Search {
                query,
                within,
                limit,
                reply,
            } => give(
                reply,
                match within {
                    Some(within) => store.search_in(&query, &within, limit),
                    None => store.search(&query, limit),
                },
            ),
        }
    }
}

impl<C: Credentials + 'static> Meanwhile for Inbox<C> {
    fn priority<'a>(
        &'a self,
        store: &'a mut Store,
        blocked: &'a [String],
    ) -> Pin<Box<dyn Future<Output = PriorityRead> + Send + 'a>> {
        Box::pin(self.next_priority(store, blocked))
    }

    fn synced(&self, store: &mut Store, conversation: &str, synced: crate::sync::Synced) {
        let Ok(mut requests) = self.requests.try_lock() else {
            return;
        };
        while let Ok(request) = requests.received.try_recv() {
            if let Some(kept) = self.meanwhile(store, request) {
                requests.kept.push_back(kept);
            }
        }
        let mut at = 0;
        while at < requests.kept.len() {
            if matches!(requests.kept[at], Request::Shutdown) {
                break;
            }
            if matches!(&requests.kept[at], Request::SyncConversation(chat, _) if chat == conversation) {
                if let Some(Request::SyncConversation(_, reply)) = requests.kept.remove(at) {
                    let _ = reply.send(Ok(synced));
                }
            } else {
                at += 1;
            }
        }
    }

    fn history_progress(&self, store: &Store, changes: &mut Changes) {
        self.brought(store, std::mem::take(changes), Source::History);
    }

    fn progress(&self, store: &Store, changes: &mut Changes) {
        self.brought(store, std::mem::take(changes), Source::CatchUp);
    }

    /// Answers what comes while a read waits, and keeps the rest for the loop (see the module docs). Ends at once
    /// when the loop is taking a request (it never is while the catch-up runs, which the loop awaits), and once every
    /// request handle is gone. `recv` loses nothing when dropped, and an answer is given whole before the next wait.
    fn serve<'a>(&'a self, store: &'a mut Store) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let Ok(mut requests) = self.requests.try_lock() else {
                return;
            };
            while let Some(request) = requests.received.recv().await {
                if let Some(kept) = self.meanwhile(store, request) {
                    requests.kept.push_back(kept);
                }
            }
        })
    }
}

fn priority_allowed(request: &Request, blocked: &[String], teams_busy: bool) -> bool {
    match request {
        Request::SyncConversation(conversation, _) | Request::LoadOlder(conversation, _) => {
            !teams_busy && !blocked.contains(conversation)
        }
        Request::Teams(_) | Request::ChatListing(_) => true,
        _ => false,
    }
}

/// The responder of a request whose answer was given already: the loop's answer to it goes nowhere.
fn answered<T>() -> Responder<T> {
    oneshot::channel().0
}

/// Checks the exact stored target before either a local horizon or a receipt can be queued.
pub(super) fn read_command(store: &Store, conversation: String, message_id: String) -> Result<Command, EngineError> {
    let message = store
        .message(&conversation, &message_id)?
        .ok_or_else(|| EngineError::UnknownMessage {
            conversation: conversation.clone(),
            message: message_id.clone(),
        })?;
    let client_message_id = message.client_message_id.ok_or_else(|| EngineError::Unknown {
        message: message_id.clone(),
        missing: "clientmessageid",
        action: "read up to",
    })?;
    Ok(Command::ReadReceipt {
        conversation,
        message_id,
        client_message_id,
    })
}
