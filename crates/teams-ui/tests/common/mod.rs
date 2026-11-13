//! Stand-ins for the engine the view tests run on: a fake event source the test feeds, and a fake core that records
//! every request and answers from its own state. Every id here is made up.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use gpui::{AppContext as _, Entity, TestAppContext};
use teams_core::sync::{CatchUpReport, Synced};
use teams_core::{
    Action, Answer, Availability, CallAction, CallId, ChatListing, EngineError, Event, NameFrom, OwnStatus, Person,
    PictureBytes, Reply, SearchAnswer, SearchScope, Strip, Transient, TransientSent,
};
use teams_store::{
    CallTranscript, Conversation, GlossaryTerm, Member, Message, MessagePage, OutboxEntry, OutboxKind, OutboxState,
    PageCursor, Reaction, ReadHorizon, SearchHit, SentOriginal, StoredTranslation, Team, TranscriptLine,
};
use teams_ui::{Bridge, Core, EventSource, Models};
use tokio::sync::mpsc;

pub mod calendar;

pub const LAB_A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
pub const LAB_B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";

/// The engine's owning side, fed by the test; counts its shutdowns.
pub struct FakeEvents {
    pub events: mpsc::UnboundedReceiver<Event>,
    pub shutdowns: Arc<AtomicUsize>,
}

impl EventSource for FakeEvents {
    fn next_event(&mut self) -> impl Future<Output = Option<Event>> {
        self.events.recv()
    }

    fn shutdown(self) {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
    }
}

/// The engine's request side: records each request by name, answers the store's reads from its own state (as account
/// a) and everything else `Stopped`; with `stopped` set, every request answers `Stopped`.
pub type WorkflowHandler = fn(&teams_proto::WorkflowRequest) -> teams_proto::WorkflowResponse;

#[derive(Default)]
pub struct FakeCore {
    pub workflow_handler: Mutex<Option<WorkflowHandler>>,
    pub workflow_requests: Mutex<Vec<teams_proto::WorkflowRequest>>,
    pub hold_workflow_catalog: Mutex<bool>,
    pub held_workflow_catalog: Mutex<Vec<Answer<teams_proto::WorkflowResponse>>>,
    pub uploads: Mutex<Vec<std::path::PathBuf>>,
    pub hold_uploads: Mutex<bool>,
    pub held_uploads: Mutex<Vec<Answer<teams_proto::UploadedFile>>>,
    pub upload_answers: Mutex<VecDeque<Result<teams_proto::UploadedFile, EngineError>>>,
    /// Every picture sent, with its conversation, in order; each answered from `picture_answers`, else an object of
    /// its own (`0-made-up-d1-<n>`).
    pub picture_uploads: Mutex<Vec<(String, teams_proto::PastedPicture)>>,
    pub picture_answers: Mutex<VecDeque<Result<String, EngineError>>>,
    pub me: AccountMri,
    pub stopped: Mutex<bool>,
    pub asked: Mutex<Vec<&'static str>>,
    pub conversations: Mutex<Vec<Conversation>>,
    /// Per conversation, the newest first.
    pub messages: Mutex<HashMap<String, Vec<Message>>>,
    pub members: Mutex<HashMap<String, Vec<Member>>>,
    pub unread: Mutex<HashMap<String, u64>>,
    /// Display names by MRI, as their messages carried them.
    pub names: Mutex<HashMap<String, String>>,
    /// People as their profiles have them, by MRI: answered before `names`.
    pub people: Mutex<HashMap<String, Person>>,
    /// People whose answer is held back: made when asked, kept in `held` for the test to send.
    pub hold_people: Mutex<HashSet<String>>,
    /// The answers held back, as they were when asked.
    pub held: Mutex<Vec<(Answer<Person>, Person)>>,
    /// Reactions by message id.
    pub reactions: Mutex<HashMap<String, Vec<Reaction>>>,
    /// The account's recent reaction keys, the newest first.
    pub recent: Mutex<Vec<String>>,
    /// The account's most used reaction keys, the most used first.
    pub frequent: Mutex<Vec<String>>,
    /// Reactions' animation strips by id; any other is answered `None`.
    pub animations: Mutex<HashMap<String, Strip>>,
    /// Reactions whose animation's read fails on the way (answered [`EngineError::AnimationUnread`]).
    pub unread_animations: Mutex<HashSet<String>>,
    /// Messages' pictures by their `src`; any other is answered `None`.
    pub pictures: Mutex<HashMap<String, PictureBytes>>,
    /// The thumbnails `file_thumbnail` answers, by the file's sharing link.
    pub thumbnails: Mutex<HashMap<String, PictureBytes>>,
    /// What `profile_card` answers, by the person's MRI; anyone else's card can't be read (`ProfileUnread`).
    pub profile_cards: Mutex<HashMap<String, teams_core::ProfileCard>>,
    /// Every card asked for, by MRI, in order.
    pub profile_cards_asked: Mutex<Vec<String>>,
    /// What `meeting_contents` answers, by the meeting's chat; any other chat's recap can't be read.
    pub meeting_contents: Mutex<HashMap<String, Vec<teams_proto::MeetingContent>>>,
    /// The `WebVTT` `meeting_transcript` answers, by the transcript's location; any other can't be read.
    pub transcripts: Mutex<HashMap<String, String>>,
    /// The meeting recordings `recording_presentation` streams (the made-up one, `support/recording_parts.rs`), by
    /// their item; any other can't be had.
    pub recordings: Mutex<HashSet<String>>,
    /// What `recording_file` answers, by the recording's item; any other can't be had.
    pub recording_files: Mutex<HashMap<String, teams_proto::RecordingAccess>>,
    /// The bytes `file_stream` streams, by the file's sharing link; any other file can't be had.
    pub file_bytes: Mutex<HashMap<String, Vec<u8>>>,
    /// The most bytes one `file_stream` hands before it ends (a stream that broke off); `None`: all it was asked.
    pub stream_limit: Mutex<Option<usize>>,
    /// With a file's bytes, where its streams stop being answered: one asked from there is held in `held_streams`.
    pub hold_streams_from: Mutex<Option<u64>>,
    pub held_streams: Mutex<Vec<Answer<teams_core::FileStream>>>,
    /// When `recording_expiration` says each meeting recording expires (ms), by its item; any other never does.
    pub expirations: Mutex<HashMap<String, i64>>,
    /// The account's read horizons by conversation.
    pub horizons: Mutex<HashMap<String, ReadHorizon>>,
    /// Per conversation, the service's older pages `load_older` brings into `messages`, the next one first (each the
    /// newest first); once none is left it answers `None`. A conversation not named answers `None` at once.
    pub older: Mutex<HashMap<String, Vec<Vec<Message>>>>,
    pub hold_older: Mutex<bool>,
    pub held_older: Mutex<Vec<Answer<Option<Synced>>>>,
    pub previews: Mutex<HashMap<String, Message>>,
    /// Conversations never synced: `history_complete` answers `None` for them; for any other, whether `older` has
    /// no page left.
    pub unsynced: Mutex<HashSet<String>>,
    /// Per conversation, a catch-up that lands just before `history_complete` answers (between it and a `load_older`
    /// that found nothing): its messages go into `messages` and the conversation is synced, once.
    pub catch_up_before_history_complete: Mutex<HashMap<String, Vec<Message>>>,
    /// The `limit` of each `messages` read, in order.
    pub limits: Mutex<Vec<usize>>,
    /// Every write asked for, in order; each is answered with a pending row of its own.
    pub actions: Mutex<Vec<Action>>,
    /// The activity feed `activity` answers.
    pub activity: Mutex<teams_core::activity::ActivityFeed>,
    /// The feed's read fails (the engine stopped) while set.
    pub activity_unread: Mutex<bool>,
    pub write_answers: Mutex<std::collections::VecDeque<OutboxEntry>>,
    pub write_errors: Mutex<VecDeque<EngineError>>,
    pub outbox_answers: Mutex<std::collections::VecDeque<OutboxEntry>>,
    /// Every transient asked for, in order; each is answered sent.
    pub transients: Mutex<Vec<Transient>>,
    /// Every own availability asked for, in order.
    pub own_statuses: Mutex<Vec<OwnStatus>>,
    /// The answers the next own availabilities get, in order; sent once none is left.
    pub own_answers: Mutex<VecDeque<Result<TransientSent, EngineError>>>,
    /// Every row resent, by id, in order.
    pub resent: Mutex<Vec<i64>>,
    /// The outbox rows that aren't sent, of any conversation, the oldest first.
    pub unsettled: Mutex<Vec<OutboxEntry>>,
    /// While set, `outbox_unsettled` isn't answered (as the engine's loop doesn't while it sends): its answers are kept
    /// in `held_unsettled`, never sent.
    pub hold_unsettled: Mutex<bool>,
    pub held_unsettled: Mutex<Vec<Answer<Vec<OutboxEntry>>>>,
    /// While `hold_unsettled` is set, how many `outbox_unsettled` are still answered before the hold applies.
    pub unsettled_before_hold: Mutex<usize>,
    /// The rows earlier runs left.
    pub earlier: Mutex<Vec<OutboxEntry>>,
    /// Every search asked for, its query, in order.
    pub searches: Mutex<Vec<String>>,
    /// What the service's search answers, whatever is asked.
    pub service_answer: Mutex<SearchAnswer>,
    /// Every search asked of the service, its query and scope, in order.
    pub service_searches: Mutex<Vec<(String, SearchScope)>>,
    /// The account's teams as the store has them; `None` before a read settled them.
    pub teams: Mutex<Option<Vec<Team>>>,
    /// Synthetic channel responses, keyed by sections/thread:<id>/preferences:<id>/email:<id>.
    pub channel_data: Mutex<HashMap<String, teams_proto::ChannelData>>,
    pub channel_data_errors: Mutex<HashSet<String>>,
    pub section_actions: Mutex<Vec<teams_proto::ChannelSectionAction>>,
    /// The channels Create asked for, in order.
    pub channels_asked: Mutex<Vec<teams_core::ChannelDraft>>,
    /// The writes on a team asked for, in order, with what they change.
    pub team_writes: Mutex<Vec<(teams_proto::TeamWrite, Option<teams_core::TeamChange>)>>,
    /// What a team's write answers; refused when `None`.
    pub team_written: Mutex<Option<teams_core::TeamWritten>>,
    /// The account's own views of a team asked for, in order.
    pub team_views: Mutex<Vec<teams_proto::OwnTeamView>>,
    /// A team's people, as its roster answers.
    pub team_roster: Mutex<Vec<teams_core::TeamMember>>,
    pub team_rosters: Mutex<HashMap<String, Vec<teams_core::TeamMember>>>,
    /// A team's settings, as the service keeps them.
    pub team_settings: Mutex<Option<String>>,
    /// Tags, code, pending membership and analytics returned by the fake service.
    pub team_tags: Mutex<teams_proto::TagsRead>,
    pub team_code: Mutex<Option<String>>,
    pub team_requests: Mutex<Vec<teams_core::TeamMember>>,
    pub team_numbers: Mutex<Option<teams_proto::TeamAnalytics>>,
    pub team_analytics_asked: Mutex<Vec<(String, teams_proto::AnalyticsPeriod)>>,
    /// A team's installed apps.
    pub team_apps: Mutex<Vec<teams_proto::InstalledApp>>,
    /// The apps the store offers.
    pub store_apps: Mutex<Vec<teams_proto::StoreApp>>,
    /// Teams' templates, and what one makes.
    pub templates: Mutex<(
        Vec<teams_proto::TeamTemplateCard>,
        Option<teams_proto::TeamTemplateDetails>,
    )>,
    /// The channels whose analytics were asked for, in order.
    pub channel_analytics_asked: Mutex<Vec<String>>,
    /// What a channel's creation answers; a problem without one.
    pub channel_answer: Mutex<Option<Result<teams_core::ChannelMade, teams_core::ChannelProblem>>>,
    /// The people a private channel was given: its team's group, its group and their MRIs.
    pub channel_members_added: Mutex<Vec<(String, String, Vec<String>)>>,
    /// What a private channel's thread answers.
    pub channel_thread_answer: Mutex<Option<String>>,
    /// Who a team's people search finds.
    pub team_found: Mutex<Vec<teams_proto::TeamPerson>>,
    /// Who the organization's people box finds (Add member).
    pub addable: Mutex<Vec<teams_proto::TeamPerson>>,
    /// Every team's numbers, as "See all your teams"' Analytics tab reads them, and each period asked for, in order.
    pub all_analytics: Mutex<Vec<teams_proto::TeamAnalytics>>,
    pub all_analytics_asked: Mutex<Vec<teams_proto::AnalyticsPeriod>>,
    /// How many Microsoft 365 groups the account owns, as "From group" reads it; `None`: the read answers none.
    pub owned_groups: Mutex<Option<usize>>,
    /// The shared channels' invitations waiting.
    pub invites: Mutex<Vec<teams_proto::PendingInvite>>,
    /// The public teams "Join a team" suggests; a search finds those whose name holds its words.
    pub joinable: Mutex<Vec<teams_proto::JoinableTeam>>,
    /// Every suggestion read asked for, its words, in order.
    pub joinable_asked: Mutex<Vec<Option<String>>>,
    /// The favourite chats and the conversations the service calls empty, as `chat_listing` answers them.
    pub listing: Mutex<ChatListing>,
    /// Per channel, the post each stored message belongs to.
    pub roots: Mutex<HashMap<String, Vec<(String, String)>>>,
    /// Every conversation a sync was asked for, in order.
    pub synced: Mutex<Vec<String>>,
    /// Everyone whose presence was asked about, in order.
    pub presence_asked: Mutex<Vec<String>>,
    /// Presence as the engine would answer it, by MRI; unknown for anyone else.
    pub presences: Mutex<HashMap<String, Availability>>,
    /// Every call action asked for, in order.
    pub call_actions: Mutex<Vec<CallAction>>,
    /// The answers the next call actions get, in order; once none is left, the call the action names (a start or a
    /// join: call 1).
    pub call_answers: Mutex<VecDeque<Result<CallId, EngineError>>>,
    /// While set, call actions aren't answered (as the engine's while it sets a call's media up): their answers are
    /// kept in `held_calls` for the test to send.
    pub hold_calls: Mutex<bool>,
    pub held_calls: Mutex<Vec<Answer<CallId>>>,
    /// What `call_health` answers for any call (none: no media).
    pub call_health: Mutex<Option<teams_core::CallHealth>>,
    /// What `call_meeting_options` answers for any call (none: not the organizer's, as the default core answers).
    pub meeting_options: Mutex<Option<teams_proto::MeetingOptions>>,
    /// The app's read-only mode, as `Core::read_only` answers it.
    pub read_only: Mutex<bool>,
    /// The scheduling tests' calendar service; its defaults match Core's empty, unsaved calendar.
    pub calendar: Mutex<calendar::CalendarFake>,
    /// The meetings their chats' threads name, by chat; any other chat names none.
    pub meetings: Mutex<HashMap<String, teams_core::MeetingDetails>>,
    /// The chats' translations kept, as the store keeps them (a newer version's replacing an older one's).
    pub translations: Mutex<Vec<StoredTranslation>>,
    /// The account's own messages as written, kept when they were sent translated.
    pub sent_originals: Mutex<Vec<SentOriginal>>,
    /// Translated chats' glossaries, as the store keeps them (the newest named last).
    pub glossary: Mutex<Vec<GlossaryTerm>>,
    /// The calls' captions saved on this computer, as the store keeps them.
    pub saved_calls: Mutex<SavedCalls>,
}

/// The calls' captions saved on this computer, as the store keeps them ([`teams_store::Store::open_call_transcript`]
/// and the others): each transcript with its lines by utterance.
#[derive(Default)]
pub struct SavedCalls {
    pub transcripts: Vec<CallTranscript>,
    pub lines: HashMap<i64, Vec<TranscriptLine>>,
}

impl SavedCalls {
    /// Carries `write` out as the store does; the transcript's id.
    pub fn write(&mut self, write: teams_core::TranscriptWrite) -> i64 {
        use teams_core::TranscriptWrite;
        match write {
            TranscriptWrite::Open {
                conversation,
                title,
                at,
                rejoin_ms,
            } => {
                let latest = self
                    .transcripts
                    .iter_mut()
                    .filter(|kept| kept.conversation_id == conversation && kept.ended_at >= at - rejoin_ms)
                    .max_by_key(|kept| (kept.ended_at, kept.id));
                if let Some(kept) = latest {
                    kept.ended_at = kept.ended_at.max(at);
                    if kept.title.is_none() {
                        kept.title = title;
                    }
                    return kept.id;
                }
                let id = self.transcripts.iter().map(|kept| kept.id).max().unwrap_or(0) + 1;
                self.transcripts.push(CallTranscript {
                    id,
                    conversation_id: conversation,
                    title,
                    started_at: at,
                    ended_at: at,
                    lines: 0,
                });
                id
            }
            TranscriptWrite::Lines { id, lines } => {
                let Some(kept) = self.transcripts.iter_mut().find(|kept| kept.id == id) else {
                    return id;
                };
                let held = self.lines.entry(id).or_default();
                for line in lines {
                    kept.ended_at = kept.ended_at.max(line.ended_at);
                    held.retain(|old| old.utterance != line.utterance);
                    held.push(line);
                }
                held.sort_by_key(|line| (line.started_at, line.ended_at));
                kept.lines = i64::try_from(held.len()).unwrap_or(i64::MAX);
                id
            }
            TranscriptWrite::End { id, at } => {
                if let Some(kept) = self.transcripts.iter_mut().find(|kept| kept.id == id) {
                    kept.ended_at = kept.ended_at.max(at);
                }
                id
            }
            TranscriptWrite::Delete { id } => {
                self.transcripts.retain(|kept| kept.id != id);
                self.lines.remove(&id);
                id
            }
        }
    }
}

/// An explicit actor, with account A as the fixture's default. An explicit override keeps its exact value.
pub struct AccountMri(String);

impl Default for AccountMri {
    fn default() -> Self {
        Self(LAB_A.to_owned())
    }
}

impl From<String> for AccountMri {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl std::ops::Deref for AccountMri {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// The store's page of `messages` (the newest first) older than `before`: at most `limit`, with the cursor for the
/// next when more are older.
#[must_use]
pub fn page_of(messages: &[Message], before: Option<&PageCursor>, limit: usize) -> MessagePage {
    let mut sorted: Vec<&Message> = messages.iter().collect();
    sorted.sort_by(|one, other| (other.arrival_time, &other.id).cmp(&(one.arrival_time, &one.id)));
    let older: Vec<&Message> = sorted
        .into_iter()
        .filter(|message| {
            before.is_none_or(|cursor| (message.arrival_time, &message.id) < (cursor.arrival_time, &cursor.id))
        })
        .collect();
    let page: Vec<Message> = older.iter().take(limit).map(|message| (*message).clone()).collect();
    let next = (older.len() > limit)
        .then(|| page.last())
        .flatten()
        .map(|last| PageCursor {
            arrival_time: last.arrival_time,
            id: last.id.clone(),
        });
    MessagePage { messages: page, next }
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A made-up outbox row `id` of `kind` in `state`, with no conversation and an empty payload.
#[must_use]
pub fn outbox_row(id: i64, kind: OutboxKind, state: OutboxState) -> OutboxEntry {
    OutboxEntry {
        id,
        run_id: 1,
        client_message_id: format!("{:019}", 1_000_000_000_000_000_000_i64 + id),
        conversation_id: None,
        kind,
        payload: "{}".to_owned(),
        state,
        attempts: 0,
        attempts_at_resend: 0,
        failed_checks: 0,
        not_before: None,
        last_error: None,
        message_id: None,
        created_at: 1_767_225_600_000,
        updated_at: 1_767_225_600_000,
        unreadable_seen_at: None,
    }
}

/// The conversation an action writes to.
fn conversation_of(action: &Action) -> Option<String> {
    match action {
        Action::Send { conversation, .. }
        | Action::Edit { conversation, .. }
        | Action::Delete { conversation, .. }
        | Action::React { conversation, .. }
        | Action::Unreact { conversation, .. }
        | Action::Read { conversation, .. }
        | Action::AddMembers { conversation, .. } => Some(conversation.clone()),
        Action::ActivityRead { .. } | Action::ActivitySeen | Action::ActivityAllRead => {
            Some(teams_core::activity::FEED.to_owned())
        }
        Action::CreateChat { .. } => None,
    }
}

impl FakeCore {
    fn ask<T>(&self, name: &'static str) -> Reply<T> {
        locked(&self.asked).push(name);
        Reply::ready(Err(EngineError::Stopped))
    }

    /// Records `name` and answers `answer`, unless the core is stopped.
    fn answer<T>(&self, name: &'static str, answer: impl FnOnce() -> T) -> Reply<T> {
        locked(&self.asked).push(name);
        if *locked(&self.stopped) {
            return Reply::ready(Err(EngineError::Stopped));
        }
        Reply::ready(Ok(answer()))
    }

    pub fn asked(&self) -> Vec<&'static str> {
        locked(&self.asked).clone()
    }

    fn person_now(&self, mri: String) -> Person {
        if let Some(person) = locked(&self.people).get(&mri) {
            return person.clone();
        }
        let name = locked(&self.names).get(&mri).cloned();
        Person {
            from: name.as_ref().map(|_| NameFrom::Message),
            name,
            picture: None,
            looking: false,
            mri,
        }
    }
}

impl Core for FakeCore {
    fn workflow_request(&self, request: teams_proto::WorkflowRequest) -> Reply<teams_proto::WorkflowResponse> {
        locked(&self.workflow_requests).push(request.clone());
        if matches!(request, teams_proto::WorkflowRequest::Catalog) && *locked(&self.hold_workflow_catalog) {
            let (answer, reply) = Reply::later();
            locked(&self.held_workflow_catalog).push(answer);
            return reply;
        }
        let answer = locked(&self.workflow_handler).map_or_else(
            || Err(teams_proto::WorkflowFailure::Connection),
            |handler| handler(&request),
        );
        self.answer("workflow", || answer)
    }

    fn upload_file(&self, path: std::path::PathBuf) -> Reply<teams_proto::UploadedFile> {
        locked(&self.uploads).push(path.clone());
        if *locked(&self.hold_uploads) {
            let (answer, reply) = Reply::later();
            locked(&self.held_uploads).push(answer);
            return reply;
        }
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("made-up.txt");
        Reply::ready(
            locked(&self.upload_answers)
                .pop_front()
                .unwrap_or_else(|| Ok(uploaded_file(name))),
        )
    }
    fn upload_picture(&self, conversation: String, picture: teams_proto::PastedPicture) -> Reply<String> {
        let mut uploads = locked(&self.picture_uploads);
        uploads.push((conversation, picture));
        let made = format!("0-made-up-d1-{}", uploads.len());
        drop(uploads);
        Reply::ready(locked(&self.picture_answers).pop_front().unwrap_or(Ok(made)))
    }
    fn meeting(&self, chat: String) -> Reply<Option<teams_core::MeetingDetails>> {
        let found = locked(&self.meetings).get(&chat).cloned();
        self.answer("meeting", || found)
    }

    fn calendar(&self, start_ms: i64, end_ms: i64) -> Reply<Vec<teams_core::CalendarEvent>> {
        locked(&self.calendar).read(start_ms, end_ms)
    }

    fn stored_calendar(&self, start_ms: i64, end_ms: i64) -> Reply<Vec<teams_core::CalendarEvent>> {
        locked(&self.calendar).stored(start_ms, end_ms)
    }

    fn calendar_event(&self, id: String) -> Reply<teams_core::FullEvent> {
        let mut calendar = locked(&self.calendar);
        calendar.full_reads.push(id.clone());
        Reply::ready(calendar.full.get(&id).cloned().ok_or(EngineError::CalendarUnread))
    }

    fn write_event(&self, write: teams_core::EventWrite) -> Reply<teams_core::EventWritten> {
        locked(&self.calendar).write(write)
    }

    fn channel_event(&self, mailbox: String, ical_uid: String) -> Reply<teams_core::FullEvent> {
        let mut calendar = locked(&self.calendar);
        calendar.full_reads.push(format!("{mailbox}/{ical_uid}"));
        Reply::ready(calendar.full.get(&ical_uid).cloned().ok_or(EngineError::CalendarUnread))
    }

    fn title_meet_now(&self, channel: String, title: String) -> Reply<()> {
        locked(&self.calendar).titles.push((channel, title));
        Reply::ready(Ok(()))
    }

    fn create_channel(&self, draft: teams_core::ChannelDraft) -> Reply<teams_core::ChannelMade> {
        locked(&self.channels_asked).push(draft);
        Reply::ready(match locked(&self.channel_answer).clone() {
            Some(Ok(made)) => Ok(made),
            Some(Err(why)) => Err(EngineError::ChannelNotCreated { why }),
            None => Err(EngineError::ChannelNotCreated {
                why: teams_core::ChannelProblem::Other,
            }),
        })
    }

    fn channel_data(&self, query: teams_proto::ChannelRead) -> Reply<teams_proto::ChannelData> {
        use serde_json::json;
        let (key, fallback) = match query {
            teams_proto::ChannelRead::Sections => (
                "sections".to_owned(),
                json!({"isPartialResponse":false,"folderHierarchyVersion":1,"conversationFolderOrder":["favorites"],"conversationFolders":[{"id":"favorites","name":"Favorites","folderType":"Favorites","conversationFolderItems":[]}]}),
            ),
            teams_proto::ChannelRead::Thread { channel } => (
                format!("thread:{channel}"),
                json!({"id":channel,"properties":{"channelSettings":json!({"postPermissions":0,"allowReplies":0,"allowPinPosts":0,"allowBotsPost":1,"allowConnectorsPost":1}).to_string(),"spaceAdminSettings":json!({"userSettings":{"generalChannelSetting":0}}).to_string()}}),
            ),
            teams_proto::ChannelRead::Preferences { channel } => {
                (format!("preferences:{channel}"), json!({"id":channel,"properties":{}}))
            }
            teams_proto::ChannelRead::Email { channel } => (
                format!("email:{channel}"),
                json!({"emailAddressDetails":{"emailId":"synthetic-channel@example.invalid"},"allowedSenders":{"allowedSenderType":"members","allowedDomains":[]}}),
            ),
        };
        if locked(&self.channel_data_errors).contains(&key) {
            return Reply::ready(Err(EngineError::TeamNotChanged { code: None }));
        }
        self.answer("channel_data", || {
            locked(&self.channel_data)
                .get(&key)
                .cloned()
                .unwrap_or(teams_proto::ChannelData(fallback))
        })
    }

    fn channel_section(&self, action: teams_proto::ChannelSectionAction) -> Reply<()> {
        locked(&self.section_actions).push(action);
        self.answer("channel_section", || ())
    }

    fn team_write(
        &self,
        write: teams_proto::TeamWrite,
        change: Option<teams_core::TeamChange>,
    ) -> Reply<teams_core::TeamWritten> {
        locked(&self.team_writes).push((write, change));
        Reply::ready(
            locked(&self.team_written)
                .clone()
                .ok_or(EngineError::TeamNotChanged { code: None }),
        )
    }

    fn all_teams_analytics(&self, period: teams_proto::AnalyticsPeriod) -> Reply<Vec<teams_proto::TeamAnalytics>> {
        locked(&self.all_analytics_asked).push(period);
        Reply::ready(Ok(locked(&self.all_analytics).clone()))
    }

    fn owned_groups(&self) -> Reply<Option<usize>> {
        Reply::ready(Ok(*locked(&self.owned_groups)))
    }

    fn pending_invites(&self) -> Reply<Vec<teams_proto::PendingInvite>> {
        Reply::ready(Ok(locked(&self.invites).clone()))
    }

    fn joinable_teams(&self, keywords: Option<String>) -> Reply<Vec<teams_proto::JoinableTeam>> {
        locked(&self.joinable_asked).push(keywords.clone());
        let words = keywords.unwrap_or_default().to_lowercase();
        let found = locked(&self.joinable)
            .iter()
            .filter(|team| team.name.to_lowercase().contains(&words))
            .cloned()
            .collect();
        Reply::ready(Ok(found))
    }

    fn own_team_view(
        &self,
        requests: Vec<teams_proto::OwnTeamView>,
        _change: Option<teams_core::TeamChange>,
    ) -> Reply<()> {
        locked(&self.team_views).extend(requests);
        Reply::ready(Ok(()))
    }

    fn team_roster(&self, team: String) -> Reply<Vec<teams_core::TeamMember>> {
        Reply::ready(Ok(locked(&self.team_rosters)
            .get(&team)
            .cloned()
            .unwrap_or_else(|| locked(&self.team_roster).clone())))
    }

    fn team_settings(&self, _team: String) -> Reply<Option<String>> {
        Reply::ready(Ok(locked(&self.team_settings).clone()))
    }

    fn team_tags(&self, _team: String) -> Reply<teams_proto::TagsRead> {
        Reply::ready(Ok(locked(&self.team_tags).clone()))
    }

    fn team_code(&self, _team: String) -> Reply<Option<String>> {
        Reply::ready(Ok(locked(&self.team_code).clone()))
    }

    fn team_join_requests(&self, _group: String) -> Reply<Vec<teams_core::TeamMember>> {
        Reply::ready(Ok(locked(&self.team_requests).clone()))
    }

    fn team_analytics(
        &self,
        group: String,
        period: teams_proto::AnalyticsPeriod,
    ) -> Reply<Option<teams_proto::TeamAnalytics>> {
        locked(&self.team_analytics_asked).push((group, period));
        Reply::ready(Ok(locked(&self.team_numbers).clone()))
    }

    fn team_apps(&self, _team: String) -> Reply<Vec<teams_proto::InstalledApp>> {
        Reply::ready(Ok(locked(&self.team_apps).clone()))
    }

    fn app_store(&self) -> Reply<Vec<teams_proto::StoreApp>> {
        Reply::ready(Ok(locked(&self.store_apps).clone()))
    }

    fn team_templates(&self) -> Reply<Vec<teams_proto::TeamTemplateCard>> {
        Reply::ready(Ok(locked(&self.templates).0.clone()))
    }

    fn team_template(&self, _id: String) -> Reply<Option<teams_proto::TeamTemplateDetails>> {
        Reply::ready(Ok(locked(&self.templates).1.clone()))
    }

    fn channel_analytics(
        &self,
        _group: String,
        channel: String,
        _period: teams_proto::AnalyticsPeriod,
    ) -> Reply<Option<teams_proto::TeamAnalytics>> {
        locked(&self.channel_analytics_asked).push(channel);
        Reply::ready(Ok(locked(&self.team_numbers).clone()))
    }

    fn add_channel_members(
        &self,
        team_group: String,
        channel_group: String,
        members: Vec<String>,
    ) -> Reply<Vec<String>> {
        locked(&self.channel_members_added).push((team_group, channel_group, members));
        Reply::ready(Ok(Vec::new()))
    }

    fn channel_thread(
        &self,
        _team: String,
        _team_group: String,
        _channel_group: String,
        _name: String,
    ) -> Reply<Option<String>> {
        Reply::ready(Ok(locked(&self.channel_thread_answer).clone()))
    }

    fn team_people(&self, _team: String, _keyword: String) -> Reply<Vec<teams_proto::TeamPerson>> {
        Reply::ready(Ok(locked(&self.team_found).clone()))
    }

    fn addable_people(&self, _keyword: String) -> Reply<Vec<teams_proto::TeamPerson>> {
        Reply::ready(Ok(locked(&self.addable).clone()))
    }

    fn invitable(&self, query: String) -> Reply<Vec<teams_core::Invitable>> {
        let mut calendar = locked(&self.calendar);
        calendar.people_queries.push(query);
        Reply::ready(Ok(calendar.people.clone()))
    }

    fn availability(
        &self,
        _: Vec<teams_core::EventAttendee>,
        _: i64,
        _: u32,
    ) -> Reply<teams_core::MeetingAvailability> {
        Reply::ready(Ok(locked(&self.calendar).availability.clone()))
    }

    fn people_calendars(
        &self,
        people: Vec<teams_core::EventAttendee>,
        start_ms: i64,
        days: u32,
    ) -> Reply<Vec<teams_proto::PersonSchedule>> {
        let mut calendar = locked(&self.calendar);
        let addresses = people.into_iter().map(|person| person.address).collect();
        calendar.schedule_reads.push((addresses, start_ms, days));
        Reply::ready(Ok(calendar.schedules.clone()))
    }

    fn meet_now(&self, subject: String) -> Reply<teams_proto::MeetNowMeeting> {
        let mut calendar = locked(&self.calendar);
        calendar.meet_now_asked.push(subject);
        Reply::ready(calendar.meet_now.clone().ok_or(teams_core::EngineError::MeetingUnread))
    }

    fn me(&self) -> &str {
        &self.me
    }

    fn read_only(&self) -> bool {
        *locked(&self.read_only)
    }

    fn activity(&self) -> Reply<teams_core::activity::ActivityFeed> {
        if *locked(&self.activity_unread) {
            locked(&self.asked).push("activity");
            return Reply::ready(Err(EngineError::Stopped));
        }
        let feed = locked(&self.activity).clone();
        self.answer("activity", || feed)
    }

    fn write(&self, action: Action) -> Reply<OutboxEntry> {
        let mut actions = locked(&self.actions);
        actions.push(action.clone());
        let id = i64::try_from(actions.len()).unwrap();
        drop(actions);
        if let Some(error) = locked(&self.write_errors).pop_front() {
            return Reply::ready(Err(error));
        }
        self.answer("write", || {
            if let Some(row) = locked(&self.write_answers).pop_front() {
                return row;
            }
            let mut row = outbox_row(id, OutboxKind::Send, OutboxState::Pending);
            row.conversation_id = conversation_of(&action);
            row
        })
    }

    fn resend(&self, id: i64) -> Reply<OutboxEntry> {
        locked(&self.resent).push(id);
        self.answer("resend", || outbox_row(id, OutboxKind::Send, OutboxState::Pending))
    }

    fn transient(&self, transient: Transient) -> Reply<TransientSent> {
        locked(&self.transients).push(transient);
        self.answer("transient", || TransientSent::Sent)
    }

    fn own_availability(&self, status: OwnStatus) -> Reply<TransientSent> {
        locked(&self.own_statuses).push(status);
        locked(&self.asked).push("own_availability");
        if *locked(&self.stopped) {
            return Reply::ready(Err(EngineError::Stopped));
        }
        Reply::ready(locked(&self.own_answers).pop_front().unwrap_or(Ok(TransientSent::Sent)))
    }

    fn sync(&self) -> Reply<CatchUpReport> {
        self.ask("sync")
    }

    fn load_older(&self, conversation: String) -> Reply<Option<Synced>> {
        if *locked(&self.hold_older) {
            let (answer, reply) = Reply::later();
            locked(&self.held_older).push(answer);
            return reply;
        }
        self.answer("load_older", || {
            let mut older = locked(&self.older);
            let page = older
                .get_mut(&conversation)
                .filter(|pages| !pages.is_empty())?
                .remove(0);
            let synced = Synced {
                inserted: page.len(),
                pages: 1,
                ..Synced::default()
            };
            locked(&self.messages).entry(conversation).or_default().extend(page);
            Some(synced)
        })
    }

    fn conversation_preview(&self, conversation: String) -> Reply<Option<Message>> {
        self.answer("conversation_preview", || {
            locked(&self.previews).get(&conversation).cloned()
        })
    }

    fn conversations(&self, limit: usize) -> Reply<Vec<Conversation>> {
        // the store's order: the latest activity first
        self.answer("conversations", || {
            let mut stored = locked(&self.conversations).clone();
            stored.sort_by_key(|conversation| std::cmp::Reverse(conversation.last_activity));
            stored.truncate(limit);
            stored
        })
    }

    fn messages(&self, conversation: String, before: Option<PageCursor>, limit: usize) -> Reply<MessagePage> {
        locked(&self.limits).push(limit);
        self.answer("messages", || {
            let messages = locked(&self.messages);
            page_of(
                messages.get(&conversation).map_or(&[][..], Vec::as_slice),
                before.as_ref(),
                limit,
            )
        })
    }

    fn message(&self, conversation: String, id: String) -> Reply<Option<Message>> {
        self.answer("message", || {
            locked(&self.messages)
                .get(&conversation)
                .and_then(|messages| messages.iter().find(|message| message.id == id).cloned())
        })
    }

    fn reactions(&self, _conversation: String, id: String) -> Reply<Vec<Reaction>> {
        self.answer("reactions", || {
            locked(&self.reactions).get(&id).cloned().unwrap_or_default()
        })
    }

    fn members(&self, conversation: String) -> Reply<Vec<Member>> {
        self.answer("members", || {
            locked(&self.members).get(&conversation).cloned().unwrap_or_default()
        })
    }

    /// The stored unnamed group chat of exactly `wanted`, the last listed of several (the fake keeps no roster version).
    fn group_chat_with(&self, wanted: Vec<String>) -> Reply<Option<String>> {
        self.answer("group_chat_with", || {
            let mut wanted = wanted.clone();
            wanted.sort();
            wanted.dedup();
            let members = locked(&self.members);
            locked(&self.conversations)
                .iter()
                .filter(|conversation| conversation.kind == teams_store::ConversationKind::Group)
                .filter(|conversation| conversation.topic.as_deref().unwrap_or_default().is_empty())
                .filter(|conversation| {
                    let mut have: Vec<String> = members
                        .get(&conversation.id)
                        .map(|members| members.iter().map(|member| member.mri.clone()).collect())
                        .unwrap_or_default();
                    have.sort();
                    have == wanted
                })
                .map(|conversation| conversation.id.clone())
                .next_back()
        })
    }

    fn unread_count(&self, conversation: String) -> Reply<u64> {
        self.answer("unread_count", || {
            locked(&self.unread).get(&conversation).copied().unwrap_or(0)
        })
    }

    fn person(&self, mri: String) -> Reply<Person> {
        if !*locked(&self.stopped) && locked(&self.hold_people).contains(&mri) {
            locked(&self.asked).push("person");
            let (answer, reply) = Reply::later();
            let person = self.person_now(mri);
            locked(&self.held).push((answer, person));
            return reply;
        }
        self.answer("person", || self.person_now(mri))
    }

    fn recent_reactions(&self, limit: usize) -> Reply<Vec<String>> {
        self.answer("recent_reactions", || {
            locked(&self.recent).iter().take(limit).cloned().collect()
        })
    }

    fn frequent_reactions(&self, limit: usize) -> Reply<Vec<String>> {
        self.answer("frequent_reactions", || {
            locked(&self.frequent).iter().take(limit).cloned().collect()
        })
    }

    fn reaction_animation(&self, id: String) -> Reply<Option<Strip>> {
        if locked(&self.unread_animations).contains(&id) {
            locked(&self.asked).push("reaction_animation");
            return Reply::ready(Err(EngineError::AnimationUnread));
        }
        self.answer("reaction_animation", || locked(&self.animations).get(&id).cloned())
    }

    fn message_picture(&self, src: String) -> Reply<Option<PictureBytes>> {
        self.answer("message_picture", || locked(&self.pictures).get(&src).cloned())
    }

    fn file_thumbnail(&self, share_url: String) -> Reply<Option<PictureBytes>> {
        self.answer("file_thumbnail", || locked(&self.thumbnails).get(&share_url).cloned())
    }

    fn file_stream(&self, share_url: String, offset: u64, len: u64) -> Reply<teams_core::FileStream> {
        locked(&self.asked).push("file_stream");
        // a file without bytes here: a video's card shows that it can't be opened
        let Some(bytes) = locked(&self.file_bytes).get(&share_url).cloned() else {
            return Reply::ready(Err(teams_core::EngineError::FileUnavailable));
        };
        if locked(&self.hold_streams_from).is_some_and(|from| offset >= from) {
            let (answer, reply) = Reply::later();
            locked(&self.held_streams).push(answer);
            return reply;
        }
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(bytes.len());
        let wanted = usize::try_from(len).unwrap_or(usize::MAX);
        let limit = locked(&self.stream_limit).unwrap_or(usize::MAX);
        let end = start.saturating_add(wanted.min(limit)).min(bytes.len());
        Reply::ready(Ok(teams_core::FileStream::of(
            offset,
            &bytes[start..end],
            bytes.len() as u64,
            4096,
        )))
    }

    fn recording_file(&self, _: String, _: String, item: String) -> Reply<teams_proto::RecordingAccess> {
        locked(&self.asked).push("recording_file");
        Reply::ready(
            locked(&self.recording_files)
                .get(&item)
                .cloned()
                .ok_or(teams_core::EngineError::FileUnavailable),
        )
    }

    fn file_size(&self, _: String) -> Reply<u64> {
        Reply::ready(Err(teams_core::EngineError::FileUnavailable))
    }

    fn profile_card(&self, mri: String) -> Reply<teams_core::ProfileCard> {
        locked(&self.asked).push("profile_card");
        let card = locked(&self.profile_cards).get(&mri).cloned();
        locked(&self.profile_cards_asked).push(mri);
        Reply::ready(card.ok_or(EngineError::ProfileUnread))
    }

    fn meeting_contents(&self, thread: String) -> Reply<Vec<teams_proto::MeetingContent>> {
        locked(&self.asked).push("meeting_contents");
        Reply::ready(
            locked(&self.meeting_contents)
                .get(&thread)
                .cloned()
                .ok_or(teams_core::EngineError::RecapUnread),
        )
    }

    fn recording_presentation(&self, _: String, _: String, item: String) -> Reply<Arc<teams_proto::Presentation>> {
        locked(&self.asked).push("recording_presentation");
        if locked(&self.recordings).contains(&item) {
            Reply::ready(Ok(made_up_presentation()))
        } else {
            Reply::ready(Err(teams_core::EngineError::FileUnavailable))
        }
    }

    fn recording_expiration(&self, _: String, _: String, item: String) -> Reply<Option<i64>> {
        locked(&self.asked).push("recording_expiration");
        Reply::ready(Ok(locked(&self.expirations).get(&item).copied()))
    }

    fn recording_segment(&self, part: teams_proto::MediaSegmentLink) -> Reply<Vec<u8>> {
        // every streamed recording is the made-up one
        Reply::ready(made_up_part(&made_up_presentation(), &part).ok_or(teams_core::EngineError::FileUnavailable))
    }

    fn meeting_transcript(&self, location: String) -> Reply<String> {
        locked(&self.asked).push("meeting_transcript");
        Reply::ready(
            locked(&self.transcripts)
                .get(&location)
                .cloned()
                .ok_or(teams_core::EngineError::RecapUnread),
        )
    }

    fn transcript_language(&self, location: String) -> Reply<Option<String>> {
        locked(&self.asked).push("transcript_language");
        // every made-up transcript is in American English
        Reply::ready(Ok(locked(&self.transcripts)
            .contains_key(&location)
            .then(|| "en-US".to_owned())))
    }

    fn presence(&self, mri: String) -> Reply<Availability> {
        locked(&self.presence_asked).push(mri.clone());
        self.answer("presence", || {
            locked(&self.presences)
                .get(&mri)
                .cloned()
                .unwrap_or(Availability::Unknown)
        })
    }

    fn read_horizon(&self, conversation: String) -> Reply<Option<ReadHorizon>> {
        self.answer("read_horizon", || locked(&self.horizons).get(&conversation).cloned())
    }

    fn translations(&self, conversation: String, language: String, ids: Vec<String>) -> Reply<Vec<StoredTranslation>> {
        self.answer("translations", || {
            locked(&self.translations)
                .iter()
                .filter(|kept| {
                    kept.conversation_id == conversation && kept.language == language && ids.contains(&kept.message_id)
                })
                .cloned()
                .collect()
        })
    }

    fn keep_translations(&self, translations: Vec<StoredTranslation>) -> Reply<()> {
        self.answer("keep_translations", || {
            let mut kept = locked(&self.translations);
            for row in translations {
                kept.retain(|old| {
                    !(old.conversation_id == row.conversation_id
                        && old.message_id == row.message_id
                        && old.language == row.language)
                });
                kept.push(row);
            }
        })
    }

    fn sent_originals(&self, conversation: String, keys: Vec<String>) -> Reply<Vec<SentOriginal>> {
        self.answer("sent_originals", || {
            locked(&self.sent_originals)
                .iter()
                .filter(|kept| kept.conversation_id == conversation && keys.contains(&kept.key))
                .cloned()
                .collect()
        })
    }

    fn keep_sent_original(&self, original: SentOriginal) -> Reply<()> {
        self.answer("keep_sent_original", || {
            let mut kept = locked(&self.sent_originals);
            kept.retain(|old| !(old.conversation_id == original.conversation_id && old.key == original.key));
            kept.push(original);
        })
    }

    fn conversation(&self, id: String) -> Reply<Option<Conversation>> {
        self.answer("conversation", || {
            locked(&self.conversations)
                .iter()
                .find(|conversation| conversation.id == id)
                .cloned()
        })
    }

    fn glossary(&self, conversation: String, language: String) -> Reply<Vec<GlossaryTerm>> {
        self.answer("glossary", || {
            locked(&self.glossary)
                .iter()
                .rev()
                .filter(|kept| kept.conversation_id == conversation && kept.language == language)
                .cloned()
                .collect()
        })
    }

    fn keep_glossary(&self, terms: Vec<GlossaryTerm>) -> Reply<()> {
        self.answer("keep_glossary", || {
            let mut kept = locked(&self.glossary);
            for term in terms {
                kept.retain(|old| {
                    !(old.conversation_id == term.conversation_id
                        && old.language == term.language
                        && old.term == term.term)
                });
                kept.push(term);
            }
        })
    }

    fn transcript(&self, write: teams_core::TranscriptWrite) -> Reply<i64> {
        self.answer("transcript", || locked(&self.saved_calls).write(write))
    }

    fn call_transcripts(&self, conversation: String) -> Reply<Vec<CallTranscript>> {
        self.answer("call_transcripts", || {
            let mut listed: Vec<CallTranscript> = locked(&self.saved_calls)
                .transcripts
                .iter()
                .filter(|kept| kept.conversation_id == conversation && kept.lines > 0)
                .cloned()
                .collect();
            listed.sort_by_key(|kept| (kept.started_at, kept.id));
            listed
        })
    }

    fn transcript_lines(&self, id: i64) -> Reply<Vec<TranscriptLine>> {
        self.answer("transcript_lines", || {
            locked(&self.saved_calls).lines.get(&id).cloned().unwrap_or_default()
        })
    }

    fn history_complete(&self, conversation: String) -> Reply<Option<bool>> {
        self.answer("history_complete", || {
            if let Some(caught_up) = locked(&self.catch_up_before_history_complete).remove(&conversation) {
                locked(&self.messages)
                    .entry(conversation.clone())
                    .or_default()
                    .extend(caught_up);
                locked(&self.unsynced).remove(&conversation);
            }
            if locked(&self.unsynced).contains(&conversation) {
                return None;
            }
            Some(locked(&self.older).get(&conversation).is_none_or(Vec::is_empty))
        })
    }

    fn outbox_entry(&self, _id: i64) -> Reply<Option<OutboxEntry>> {
        match locked(&self.outbox_answers).pop_front() {
            Some(row) => self.answer("outbox_entry", || Some(row)),
            None => self.ask("outbox_entry"),
        }
    }

    fn outbox_unsettled(&self, conversation: String) -> Reply<Vec<OutboxEntry>> {
        if !*locked(&self.stopped) && *locked(&self.hold_unsettled) && !counted_down(&self.unsettled_before_hold) {
            locked(&self.asked).push("outbox_unsettled");
            let (answer, reply) = Reply::later();
            locked(&self.held_unsettled).push(answer);
            return reply;
        }
        self.answer("outbox_unsettled", || {
            locked(&self.unsettled)
                .iter()
                .filter(|row| row.conversation_id.as_deref() == Some(conversation.as_str()))
                .cloned()
                .collect()
        })
    }

    fn left_by_earlier_runs(&self) -> Reply<Vec<OutboxEntry>> {
        self.answer("left_by_earlier_runs", || locked(&self.earlier).clone())
    }

    fn search_service(&self, query: String, scope: SearchScope) -> Reply<SearchAnswer> {
        locked(&self.service_searches).push((query, scope));
        let answer = locked(&self.service_answer).clone();
        self.answer("search_service", || answer)
    }

    /// The store's search, played on the messages held: a message of a conversation of `within`, not deleted, whose
    /// content holds every term (any case), the newest first, the first term marked in its snippet (the content as it
    /// is).
    fn search(&self, query: String, within: Vec<String>, limit: usize) -> Reply<Vec<SearchHit>> {
        locked(&self.searches).push(query.clone());
        self.answer("search", || {
            let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
            let mut hits: Vec<SearchHit> = locked(&self.messages)
                .values()
                .flatten()
                .filter(|message| within.contains(&message.conversation_id))
                .filter(|message| message.delete_time.is_none())
                .filter(|message| {
                    let content = message.content.to_lowercase();
                    !terms.is_empty() && terms.iter().all(|term| content.contains(term.as_str()))
                })
                .map(|message| SearchHit {
                    conversation_id: message.conversation_id.clone(),
                    message_id: message.id.clone(),
                    arrival_time: message.arrival_time,
                    sender: message.sender.clone(),
                    sender_name: message.sender_name.clone(),
                    snippet: marked(&message.content, &terms[0]),
                })
                .collect();
            hits.sort_by(|one, other| {
                (other.arrival_time, &other.message_id).cmp(&(one.arrival_time, &one.message_id))
            });
            hits.truncate(limit);
            hits
        })
    }

    fn teams(&self) -> Reply<Option<Vec<Team>>> {
        self.answer("teams", || locked(&self.teams).clone())
    }

    fn chat_listing(&self) -> Reply<ChatListing> {
        self.answer("chat_listing", || locked(&self.listing).clone())
    }

    fn sync_conversation(&self, conversation: String) -> Reply<Synced> {
        locked(&self.synced).push(conversation);
        self.answer("sync_conversation", Synced::default)
    }

    fn reply_roots(&self, conversation: String) -> Reply<Vec<(String, String)>> {
        self.answer("reply_roots", || {
            locked(&self.roots).get(&conversation).cloned().unwrap_or_default()
        })
    }

    fn call_health(&self, _: CallId) -> Reply<Option<teams_core::CallHealth>> {
        self.answer("call_health", || locked(&self.call_health).clone())
    }

    fn call_meeting_options(&self, _: CallId) -> Reply<Option<teams_proto::MeetingOptions>> {
        // not listed among `asked`: the default core's answer isn't either
        Reply::ready(Ok(*locked(&self.meeting_options)))
    }

    fn call(&self, action: CallAction) -> Reply<CallId> {
        locked(&self.call_actions).push(action.clone());
        locked(&self.asked).push("call");
        if *locked(&self.stopped) {
            return Reply::ready(Err(EngineError::Stopped));
        }
        if *locked(&self.hold_calls) {
            let (answer, reply) = Reply::later();
            locked(&self.held_calls).push(answer);
            return reply;
        }
        let named = match action {
            CallAction::Captions { call, .. }
            | CallAction::CaptionLanguage { call, .. }
            | CallAction::Record { call, .. }
            | CallAction::StopRecording { call }
            | CallAction::Transcribe { call, .. }
            | CallAction::StopTranscription { call }
            | CallAction::Answer { call }
            | CallAction::Decline { call }
            | CallAction::HangUp { call }
            | CallAction::Mute { call, .. }
            | CallAction::Camera { call, .. }
            | CallAction::Lobby { call, .. }
            | CallAction::RaiseHand { call, .. }
            | CallAction::React { call, .. }
            | CallAction::EndMeeting { call }
            | CallAction::Hold { call, .. }
            | CallAction::Timer { call, .. }
            | CallAction::LockMeeting { call, .. }
            | CallAction::Share { call, .. }
            | CallAction::Add { call, .. }
            | CallAction::Transfer { call, .. }
            | CallAction::MuteAll { call }
            | CallAction::MeetingOption { call, .. }
            | CallAction::Renegotiate { call } => call,
            CallAction::Call { .. }
            | CallAction::TestCall { .. }
            | CallAction::Join { .. }
            | CallAction::MeetNow { .. }
            | CallAction::JoinChannel { .. }
            | CallAction::JoinMeeting { .. }
            | CallAction::JoinMeetingCode(_) => CallId::for_tests(1),
        };
        Reply::ready(locked(&self.call_answers).pop_front().unwrap_or(Ok(named)))
    }
}

/// Synthetic upload metadata for view tests; this never contacts a service.
///
/// # Panics
/// If the synthetic fixture no longer matches the supported upload answer.
#[must_use]
pub fn uploaded_file(name: &str) -> teams_proto::UploadedFile {
    let site =
        teams_proto::DriveSite::from_web_url("https://madeup-my.sharepoint.com/personal/a_example_test/Documents")
            .expect("synthetic site");
    let body = serde_json::json!({"id":"MadeUpItem","name":name,"size":54,
        "webUrl":format!("{}Documents/Microsoft%20Teams%20Chat%20Files/{name}",site.base()),
        "parentReference":{"driveId":"made-up-drive"},
        "sharepointIds":{"listItemUniqueId":"00000000-0000-4000-8000-00000000000f"}});
    teams_proto::UploadedFile::from_upload(&site, body.to_string().as_bytes()).expect("synthetic upload")
}

/// Whether `count` was above zero, taking one off it.
fn counted_down(count: &Mutex<usize>) -> bool {
    let mut count = locked(count);
    let was = *count > 0;
    *count = count.saturating_sub(1);
    was
}

/// `content` with its first match of `term` (lower case; ASCII content) between the store's match marks.
fn marked(content: &str, term: &str) -> String {
    let Some(at) = content.to_lowercase().find(term) else {
        return content.to_owned();
    };
    let end = at + term.len();
    format!(
        "{}{}{}{}{}",
        &content[..at],
        teams_store::search::MATCH_START,
        &content[at..end],
        teams_store::search::MATCH_END,
        &content[end..]
    )
}

/// A bridge over fakes, as a test holds it.
pub struct Rig {
    pub cx: TestAppContext,
    pub events: mpsc::UnboundedSender<Event>,
    pub shutdowns: Arc<AtomicUsize>,
    pub core: Arc<FakeCore>,
    pub models: Models,
    pub bridge: Entity<Bridge>,
}

impl Rig {
    pub fn start() -> Self {
        Self::start_with(Models::new)
    }

    /// The rig over the models `make` makes (a build that shows calls: `Models::with_calls`).
    pub fn start_with(make: impl FnOnce(&mut gpui::App) -> Models) -> Self {
        let cx = TestAppContext::single();
        let (events, received) = mpsc::unbounded_channel();
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let source = FakeEvents {
            events: received,
            shutdowns: Arc::clone(&shutdowns),
        };
        let core = Arc::new(FakeCore::default());
        let models = cx.update(make);
        let shared: Arc<dyn Core> = core.clone();
        let drained = models.clone();
        let bridge = cx.update(|cx| cx.new(|cx| Bridge::start(source, shared, drained, cx)));
        Self {
            cx,
            events,
            shutdowns,
            core,
            models,
            bridge,
        }
    }

    /// Hands the bridge an event and lets the drain apply it.
    ///
    /// # Panics
    /// The bridge has already stopped receiving events.
    pub fn send(&self, event: Event) {
        self.events.send(event).expect("the drain is listening");
        self.cx.run_until_parked();
    }

    #[must_use]
    pub fn shutdowns(&self) -> usize {
        self.shutdowns.load(Ordering::SeqCst)
    }
}

/// What a trace wrote, shared with the test.
#[derive(Clone, Default)]
pub struct Written(pub Arc<Mutex<Vec<u8>>>);

impl Written {
    /// The trace as UTF-8.
    ///
    /// # Panics
    /// A writer handed the trace bytes that aren't UTF-8.
    pub fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()).expect("UTF-8")
    }

    #[must_use]
    /// The trace's JSON records.
    ///
    /// # Panics
    /// The trace isn't UTF-8 or one of its lines isn't a JSON record.
    pub fn lines(&self) -> Vec<serde_json::Value> {
        self.text()
            .lines()
            .map(|line| serde_json::from_str(line).expect("a JSON line"))
            .collect()
    }
}

impl Write for Written {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

include!("../support/recording_parts.rs");
