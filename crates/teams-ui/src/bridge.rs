//! The bridge between GPUI and the engine.
//!
//! The engine runs on a thread and a runtime of its own. Its owner, the [`EngineHandle`], has the one event receiver
//! and the shutdown, so it can't be shared: the [`Bridge`] owns it (as an [`EventSource`]), and one task on GPUI's
//! foreground drains its events into the [`Models`]. Views get only the request side, behind [`Core`] (in the app the
//! engine's [`EngineRequests`]; in a view test a fake), and await each query's [`Reply`] on GPUI's executor.
//!
//! [`Bridge::shutdown`] stops the drain, takes the owner back from it and shuts the engine down on GPUI's background
//! executor, never the foreground ([`EventSource::shutdown`] blocks until the engine's thread is joined). Every call,
//! the first and any later one, answers with a task that ends only once that join has: a caller that quits after it
//! never quits before the engine is joined. The bridge keeps the shutdown going whether or not a caller keeps its
//! task. From then on every request answers `Stopped`: nothing more goes out. [`Bridge::sign_out`] does the same once
//! the account's session can't renew its tokens, and shows the account signed out.
//!
//! A **tap** ([`Bridge::set_tap`]) sees each event just before the models apply it, the app's control socket's events
//! (`ngteams::ctl::events`): the models stay the views' only state, and the tap only looks.
//!
//! The diagnostic log (M2 Task 24a) has the shutdown, the sign-out and an engine that stopped on its own; a view's
//! failed request is logged by its kind where its [`Reply`] is read (`teams-core`), and the engine's own failures as
//! it tells them.

use std::cell::RefCell;
use std::pin::pin;
use std::rc::Rc;
use std::sync::Arc;

use futures_util::FutureExt as _;
use futures_util::future::{Either, Shared, select};
use gpui::{App, AppContext as _, AsyncApp, Context, SharedString, Task};
use teams_core::sync::{CatchUpReport, Synced};
use teams_core::{
    Action, Availability, CalendarEvent, CallAction, CallHealth, CallId, ChannelDraft, ChannelMade, ChatListing,
    EngineHandle, EngineRequests, Event, EventAttendee, EventWrite, EventWritten, FileStream, FullEvent, Invitable,
    MeetingAvailability, MeetingDetails, OwnStatus, Person, PictureBytes, Reply, SearchAnswer, SearchScope, Strip,
    TeamChange, TeamMember, TeamWritten, TranscriptWrite, Transient, TransientSent,
};
use teams_proto::{
    AnalyticsPeriod, InstalledApp, JoinableTeam, ManagedChannel, OwnTeamView, PendingInvite, StoreApp, TagsRead,
    TeamAnalytics, TeamPerson, TeamPicture, TeamTemplateCard, TeamTemplateDetails, TeamWrite,
};
use teams_store::{
    CallTranscript, Conversation, GlossaryTerm, Member, Message, MessagePage, OutboxEntry, PageCursor, Reaction,
    ReadHorizon, SearchHit, SentOriginal, StoredTranslation, Team, TranscriptLine,
};
use tokio::sync::oneshot;

use crate::models::Models;

/// The engine's request side, as the views see it: writes and queries, each answered by a [`Reply`]. No view builds a
/// request of its own; every write goes out through the engine's outbox.
pub trait Core: Send + Sync + 'static {
    /// A local file uploaded privately; access is granted only on Send through `write`.
    fn upload_file(&self, path: std::path::PathBuf) -> Reply<teams_proto::UploadedFile> {
        let _ = path;
        Reply::ready(Err(teams_core::EngineError::FileUnavailable))
    }
    /// A picture pasted into a message in `conversation`, sent to the media service readable by that conversation
    /// alone; the reply is its object's id, which the message's `<img>` names.
    fn upload_picture(&self, conversation: String, picture: teams_proto::PastedPicture) -> Reply<String> {
        let _ = (conversation, picture);
        Reply::ready(Err(teams_core::EngineError::FileUnavailable))
    }
    /// Native Power Automate requests for this account. A fake without workflow support returns a connection error.
    fn workflow_request(&self, request: teams_proto::WorkflowRequest) -> Reply<teams_proto::WorkflowResponse> {
        let _ = request;
        Reply::ready(Ok(Err(teams_proto::WorkflowFailure::Connection)))
    }
    /// The account's MRI: who the engine writes as.
    fn me(&self) -> &str;
    /// A write through the outbox; the reply is the row as its first send left it.
    fn write(&self, action: Action) -> Reply<OutboxEntry>;
    /// The explicit resend of an outbox row, by its id.
    fn resend(&self, id: i64) -> Reply<OutboxEntry>;
    /// A transient write (typing), as no row.
    fn transient(&self, transient: Transient) -> Reply<TransientSent>;
    /// The account's own availability, a transient write from the endpoint the push channel registered (M2 Tasks
    /// 19–20): `EngineError::NoEndpoint` before a registration went through and once the channel stopped. When to send
    /// which is the app's policy ([`crate::own_presence`]).
    fn own_availability(&self, status: OwnStatus) -> Reply<TransientSent>;
    /// A catch-up now.
    fn sync(&self) -> Reply<CatchUpReport>;
    /// The next older page of a conversation, from the service.
    fn load_older(&self, conversation: String) -> Reply<Option<Synced>>;
    /// Up to `limit` stored conversations, the latest activity first.
    fn conversations(&self, limit: usize) -> Reply<Vec<Conversation>>;
    /// Up to `limit` stored messages of a conversation older than `before`, the newest first.
    fn messages(&self, conversation: String, before: Option<PageCursor>, limit: usize) -> Reply<MessagePage>;
    /// A list-page hint for the row before the chat's history arrives.
    fn conversation_preview(&self, conversation: String) -> Reply<Option<Message>> {
        let _ = conversation;
        Reply::ready(Ok(None))
    }
    /// One stored message.
    fn message(&self, conversation: String, id: String) -> Reply<Option<Message>>;
    /// A stored message's reactions.
    fn reactions(&self, conversation: String, id: String) -> Reply<Vec<Reaction>>;
    /// A chat's stored members.
    fn members(&self, conversation: String) -> Reply<Vec<Member>>;
    /// The stored group chat of exactly `members` (the account among them), the newest of several: what New message
    /// opens for those people. A fake core has none.
    fn group_chat_with(&self, members: Vec<String>) -> Reply<Option<String>> {
        let _ = members;
        Reply::ready(Ok(None))
    }
    /// The reaction keys the account set most recently, the newest first; at most `limit`.
    fn recent_reactions(&self, limit: usize) -> Reply<Vec<String>>;
    /// The reaction keys the account uses most, the most used first; at most `limit`.
    fn frequent_reactions(&self, limit: usize) -> Reply<Vec<String>>;
    /// How many stored messages of a conversation the account hasn't read.
    fn unread_count(&self, conversation: String) -> Reply<u64>;
    /// Someone's name and picture as the engine has them; a lookup starts in the background when its stored ones
    /// are missing or old, and its change comes as `Event::Profile`.
    fn person(&self, mri: String) -> Reply<Person>;
    /// Someone's presence as last read; from then on the engine reads it (for those this build may read), and each
    /// change comes as `Event::Presence`. The engine watches at most `teams_core::MAX_WATCHED` people: past it the one
    /// asked about longest ago is dropped and told unknown, so a view asks again whenever it shows its people anew.
    fn presence(&self, mri: String) -> Reply<Availability>;
    /// A reaction's animation as Teams' CDN has it (its strip of frames), read once a run; `None` when there is none.
    fn reaction_animation(&self, id: String) -> Reply<Option<Strip>>;
    /// The picture a message's `<img>` names by its `src` (a view of an AMS object, or a GIF on Giphy's media host),
    /// read by the engine once a run; `None` when there is none, or `src` names neither.
    fn message_picture(&self, src: String) -> Reply<Option<PictureBytes>>;
    /// The thumbnail of a file a message carries, by its sharing link (`teams_proto::chat::SharedFile::share_url`),
    /// read by the engine once a run; `None` when there is none.
    fn file_thumbnail(&self, share_url: String) -> Reply<Option<PictureBytes>>;
    /// A stream of `len` bytes of a file a message carries from `offset`, by its sharing link, read by the engine at
    /// the download link Graph handed out, its bytes handed on as they arrive: what a video's card plays it by.
    fn file_stream(&self, share_url: String, offset: u64, len: u64) -> Reply<FileStream>;
    /// The size of a file a message carries, by its sharing link: asked for when a video's card is drawn, so the
    /// engine knows its download link before Play is pressed.
    fn file_size(&self, share_url: String) -> Reply<u64>;
    /// What the meeting whose chat is `thread` left (its recordings and transcripts), as its recap lists it. A fake
    /// without recaps answers that it can't be read.
    fn meeting_contents(&self, thread: String) -> Reply<Vec<teams_proto::MeetingContent>> {
        let _ = thread;
        Reply::ready(Err(teams_core::EngineError::RecapUnread))
    }
    /// The `WebVTT` of the meeting transcript at `location` (a transcript content's).
    fn meeting_transcript(&self, location: String) -> Reply<String> {
        let _ = location;
        Reply::ready(Err(teams_core::EngineError::RecapUnread))
    }
    /// The language (a BCP 47 tag, `en-US`) of the meeting transcript at `location`, as its item's transcripts give it:
    /// what the recap player's captions menu names. A fake without transcripts knows none.
    fn transcript_language(&self, location: String) -> Reply<Option<String>> {
        let _ = location;
        Reply::ready(Ok(None))
    }
    /// The DASH presentation of the meeting recording that is item `item` of drive `drive` on the personal site of
    /// its sharing link `link`, as Teams' Recap player streams it: what a recap plays a recording by, its download
    /// blocked or not. A fake without recordings answers that it can't be had.
    fn recording_presentation(
        &self,
        link: String,
        drive: String,
        item: String,
    ) -> Reply<Arc<teams_proto::Presentation>> {
        let _ = (link, drive, item);
        Reply::ready(Err(teams_core::EngineError::FileUnavailable))
    }
    /// A part of a meeting recording's presentation, at its address.
    fn recording_segment(&self, part: teams_proto::MediaSegmentLink) -> Reply<Vec<u8>> {
        let _ = part;
        Reply::ready(Err(teams_core::EngineError::FileUnavailable))
    }
    /// What the meeting recording that is item `item` of drive `drive` on the personal site of its sharing link `link`
    /// is to the account: whether it may download it (the recap offers Download only then), its file name and size. A
    /// fake without recordings answers that it can't be had.
    fn recording_file(&self, link: String, drive: String, item: String) -> Reply<teams_proto::RecordingAccess> {
        let _ = (link, drive, item);
        Reply::ready(Err(teams_core::EngineError::FileUnavailable))
    }
    /// When the meeting recording that is item `item` of drive `drive` on the personal site of its sharing link
    /// `link` expires, ms since the epoch, as its item says: what a meeting chat's recap card counts its days by.
    /// `None` when it says none; a fake without recordings knows none.
    fn recording_expiration(&self, link: String, drive: String, item: String) -> Reply<Option<i64>> {
        let _ = (link, drive, item);
        Reply::ready(Ok(None))
    }
    /// The account's own read horizon in a conversation.
    fn read_horizon(&self, conversation: String) -> Reply<Option<ReadHorizon>>;
    /// The account's activity feed as stored ([`teams_core::activity`]); an empty one where the engine has none.
    fn activity(&self) -> Reply<teams_core::activity::ActivityFeed> {
        Reply::ready(Ok(teams_core::activity::ActivityFeed::default()))
    }
    /// The kept translations into `language` of the messages `ids` of `conversation` (a chat translated on this
    /// computer: [`crate::chat_translate`]), every version kept. A fake keeps none.
    fn translations(&self, conversation: String, language: String, ids: Vec<String>) -> Reply<Vec<StoredTranslation>> {
        let _ = (conversation, language, ids);
        Reply::ready(Ok(Vec::new()))
    }
    /// Keeps a chat's translations in the store: no service write. A fake keeps nothing.
    fn keep_translations(&self, translations: Vec<StoredTranslation>) -> Reply<()> {
        let _ = translations;
        Reply::ready(Ok(()))
    }
    /// The account's own messages of `conversation` as written when they were sent translated, by their keys. A fake
    /// keeps none.
    fn sent_originals(&self, conversation: String, keys: Vec<String>) -> Reply<Vec<SentOriginal>> {
        let _ = (conversation, keys);
        Reply::ready(Ok(Vec::new()))
    }
    /// Keeps the account's own message as written, sent translated: no service write. A fake keeps nothing.
    fn keep_sent_original(&self, original: SentOriginal) -> Reply<()> {
        let _ = original;
        Reply::ready(Ok(()))
    }
    /// The stored conversation `id` (a translated chat's topic), when the store holds it. A fake holds none.
    fn conversation(&self, id: String) -> Reply<Option<Conversation>> {
        let _ = id;
        Reply::ready(Ok(None))
    }
    /// The glossary of `conversation` translated into `language`, the newest named first. A fake keeps none.
    fn glossary(&self, conversation: String, language: String) -> Reply<Vec<GlossaryTerm>> {
        let _ = (conversation, language);
        Reply::ready(Ok(Vec::new()))
    }
    /// Keeps terms of translated chats' glossaries in the store: no service write. A fake keeps nothing.
    fn keep_glossary(&self, terms: Vec<GlossaryTerm>) -> Reply<()> {
        let _ = terms;
        Reply::ready(Ok(()))
    }
    /// Changes the calls' captions saved on this computer ([`crate::call_transcript`]): only the store changes, nothing
    /// goes to Teams. Answers the transcript's id; a fake keeps nothing and answers 0.
    fn transcript(&self, write: TranscriptWrite) -> Reply<i64> {
        let _ = write;
        Reply::ready(Ok(0))
    }
    /// The saved transcripts of the calls in `conversation`, the earliest first. A fake keeps none.
    fn call_transcripts(&self, conversation: String) -> Reply<Vec<CallTranscript>> {
        let _ = conversation;
        Reply::ready(Ok(Vec::new()))
    }
    /// A saved transcript's lines in the order they were said. A fake keeps none.
    fn transcript_lines(&self, id: i64) -> Reply<Vec<TranscriptLine>> {
        let _ = id;
        Reply::ready(Ok(Vec::new()))
    }
    /// Whether the store holds a conversation's whole history; `None` when it was never synced.
    fn history_complete(&self, conversation: String) -> Reply<Option<bool>>;
    /// An outbox row as it now stands.
    fn outbox_entry(&self, id: i64) -> Reply<Option<OutboxEntry>>;
    /// A conversation's outbox rows that aren't sent.
    fn outbox_unsettled(&self, conversation: String) -> Reply<Vec<OutboxEntry>>;
    /// The rows runs that have ended left pending or in flight.
    fn left_by_earlier_runs(&self) -> Reply<Vec<OutboxEntry>>;
    /// Up to `limit` stored messages of the conversations of `within` whose text holds every term of `query`, the
    /// newest first: the store's own index, never the service's search. The other conversations' messages are left
    /// out before the hits are counted.
    fn search(&self, query: String, within: Vec<String>, limit: usize) -> Reply<Vec<SearchHit>>;
    /// The account's teams with their channels, from the store (`None` before a read settled them); a read starts in
    /// the background when they are missing or old, and a change comes as `Event::Teams` (M2 Task 16).
    fn teams(&self) -> Reply<Option<Vec<Team>>>;
    /// What the chat list shows besides the stored conversations: the favourites (chats and channels) in Teams' order
    /// (`None` before a read gave them) and the conversations the chat service calls empty; a read of the teams starts
    /// in the background when they are old, and a change of the favourites comes as `Event::Teams`.
    fn chat_listing(&self) -> Reply<ChatListing>;
    /// Syncs one conversation's messages from the service now: how a channel's posts are read.
    fn sync_conversation(&self, conversation: String) -> Reply<Synced>;
    /// The post each stored message of a channel belongs to, as (message id, post id).
    fn reply_roots(&self, conversation: String) -> Reply<Vec<(String, String)>>;
    /// A call action (M3 Task 17): a call from a chat, the test call, a join, the answer or the decline of an
    /// invitation the engine offered, a hang-up, muting; answered with the call it acted on, or why it was refused
    /// (nothing sent). Only the user's action asks for one ([`crate::calls`]).
    fn call(&self, action: CallAction) -> Reply<CallId>;
    /// The running call's health now (the call window's Call health), read from its media's own statistics: nothing
    /// is sent, and it isn't a call action. `None` when the call has no media running; a fake core's never has.
    fn call_health(&self, call: CallId) -> Reply<Option<CallHealth>> {
        let _ = call;
        Reply::ready(Ok(None))
    }
    /// The running call's meeting options as the service has them (its organizer's Controls: Enable lobby, Allow all
    /// participants to …), read now; it isn't a call action. `None` unless the account organizes the call; a fake
    /// core's never has them.
    fn call_meeting_options(&self, call: CallId) -> Reply<Option<teams_proto::MeetingOptions>> {
        let _ = call;
        Reply::ready(Ok(None))
    }
    /// Whether the engine sends nothing (the personal build's read-only mode, `ngteams --read-only`): the views offer
    /// no write (the boxes, a message's actions, the calls, the status say "Read-only"), and one asked anyway is
    /// refused below them, recorded in the diagnostic log. `false` by default.
    fn read_only(&self) -> bool {
        false
    }
    /// The account's calendar events between `start_ms` and `end_ms` (ms since the epoch), read from the service now
    /// (`teams_core::calendar`). None by default (a fake core's calendar is empty).
    fn calendar(&self, start_ms: i64, end_ms: i64) -> Reply<Vec<CalendarEvent>> {
        let _ = (start_ms, end_ms);
        Reply::ready(Ok(Vec::new()))
    }
    /// The account's calendar events between `start_ms` and `end_ms` as the store kept them from earlier reads
    /// (`teams_core::calendar`): what the calendar draws at once while [`Core::calendar`] reads them. None by default.
    fn stored_calendar(&self, start_ms: i64, end_ms: i64) -> Reply<Vec<CalendarEvent>> {
        let _ = (start_ms, end_ms);
        Reply::ready(Ok(Vec::new()))
    }
    /// One event of the account's calendar, read whole now: what the scheduling form opens on to change it
    /// (`teams_core::calendar`). A fake core has none.
    fn calendar_event(&self, id: String) -> Reply<FullEvent> {
        let _ = id;
        Reply::ready(Err(teams_core::EngineError::CalendarUnread))
    }
    /// Creates, changes, cancels or deletes an event of the account's calendar, through the outbox
    /// (`teams_core::calendar`); only the user's Save, Cancel meeting or Delete asks for one. A fake core saves
    /// nothing.
    fn write_event(&self, write: EventWrite) -> Reply<EventWritten> {
        Reply::ready(Err(teams_core::EngineError::EventNotSaved {
            kind: write.kind(),
            why: "nothing saves here".to_owned(),
        }))
    }
    /// Makes a team's channel (`teams_core::channels`); only the "Create a channel" dialog's Create asks for one. A fake
    /// core makes none.
    fn create_channel(&self, _draft: ChannelDraft) -> Reply<ChannelMade> {
        Reply::ready(Err(teams_core::EngineError::ChannelNotCreated {
            why: teams_core::ChannelProblem::Other,
        }))
    }
    /// Adds people to a private channel by its group: answered with those who weren't added. A fake core adds nobody.
    fn add_channel_members(
        &self,
        _team_group: String,
        _channel_group: String,
        members: Vec<String>,
    ) -> Reply<Vec<String>> {
        Reply::ready(Ok(members))
    }
    /// A private channel's thread, once it has one. A fake core has none.
    fn channel_thread(
        &self,
        _team: String,
        _team_group: String,
        _channel_group: String,
        _name: String,
    ) -> Reply<Option<String>> {
        Reply::ready(Ok(None))
    }
    /// The people on a team `keyword` finds: a private channel's member step. A fake core finds nobody.
    fn team_people(&self, _team: String, _keyword: String) -> Reply<Vec<TeamPerson>> {
        Reply::ready(Ok(Vec::new()))
    }
    /// A write managing a team (`teams_core::team_admin`); only the team pages' own buttons ask for one. A fake core changes
    /// nothing.
    fn team_write(&self, _write: TeamWrite, _change: Option<TeamChange>) -> Reply<TeamWritten> {
        Reply::ready(Err(teams_core::EngineError::TeamNotChanged { code: None }))
    }
    /// The account's own view of a team changed. A fake core changes nothing.
    fn own_team_view(&self, _requests: Vec<OwnTeamView>, _change: Option<TeamChange>) -> Reply<()> {
        Reply::ready(Err(teams_core::EngineError::TeamNotChanged { code: None }))
    }
    /// The meeting whose chat is `chat`, as its chat's thread names it. A fake core knows none.
    fn meeting(&self, _chat: String) -> Reply<Option<MeetingDetails>> {
        Reply::ready(Ok(None))
    }
    /// The meeting that runs in the channel `channel` now. A fake core knows none.
    fn channel_meetup(&self, _channel: String) -> Reply<Option<teams_core::ChannelMeetup>> {
        Reply::ready(Ok(None))
    }
    /// A channel's meeting `ical_uid`, read whole from the group calendar of its team's `mailbox`: what its details open
    /// on to change it. A fake core has none.
    fn channel_event(&self, _mailbox: String, _ical_uid: String) -> Reply<FullEvent> {
        Reply::ready(Err(teams_core::EngineError::CalendarUnread))
    }
    /// Titles the meeting the account's Meet now in the channel `channel` starts, once the service posted it. A fake core
    /// titles nothing.
    fn title_meet_now(&self, _channel: String, _title: String) -> Reply<()> {
        Reply::ready(Ok(()))
    }
    /// A team's people. A fake core knows nobody.
    fn team_roster(&self, _team: String) -> Reply<Vec<TeamMember>> {
        Reply::ready(Ok(Vec::new()))
    }
    /// A team's settings as the service keeps them. A fake core has none.
    fn team_settings(&self, _team: String) -> Reply<Option<String>> {
        Reply::ready(Ok(None))
    }
    /// A team's tags. A fake core has none.
    fn team_tags(&self, _team: String) -> Reply<TagsRead> {
        Reply::ready(Ok(TagsRead::default()))
    }
    /// A team's code. A fake core has none.
    fn team_code(&self, _team: String) -> Reply<Option<String>> {
        Reply::ready(Ok(None))
    }
    /// A team's channels as Manage team lists them. A fake core has none.
    /// Fresh data for a channel's management dialogs.
    fn channel_section(&self, _action: teams_proto::ChannelSectionAction) -> Reply<()> {
        Reply::ready(Err(teams_core::EngineError::TeamNotChanged { code: None }))
    }

    fn channel_data(&self, _query: teams_proto::ChannelRead) -> Reply<teams_proto::ChannelData> {
        Reply::ready(Err(teams_core::EngineError::TeamNotChanged { code: None }))
    }

    fn team_channels(&self, _group: String) -> Reply<Vec<ManagedChannel>> {
        Reply::ready(Ok(Vec::new()))
    }
    /// The invitations waiting. A fake core has none.
    fn pending_invites(&self) -> Reply<Vec<PendingInvite>> {
        Reply::ready(Ok(Vec::new()))
    }
    /// How a team did. A fake core doesn't know.
    fn team_analytics(&self, _group: String, _period: AnalyticsPeriod) -> Reply<Option<TeamAnalytics>> {
        Reply::ready(Ok(None))
    }
    /// How the account's teams did. A fake core doesn't know.
    fn all_teams_analytics(&self, _period: AnalyticsPeriod) -> Reply<Vec<TeamAnalytics>> {
        Reply::ready(Ok(Vec::new()))
    }
    /// The teams one can join. A fake core has none.
    fn joinable_teams(&self, _keywords: Option<String>) -> Reply<Vec<JoinableTeam>> {
        Reply::ready(Ok(Vec::new()))
    }
    /// The people `keyword` finds, to add to a team. A fake core finds nobody.
    fn addable_people(&self, _keyword: String) -> Reply<Vec<TeamPerson>> {
        Reply::ready(Ok(Vec::new()))
    }
    /// The people asking to join a team. A fake core knows nobody.
    fn team_join_requests(&self, _group: String) -> Reply<Vec<TeamMember>> {
        Reply::ready(Ok(Vec::new()))
    }
    /// A team's picture. A fake core has none.
    fn team_picture(&self, _team: TeamPicture) -> Reply<Option<PictureBytes>> {
        Reply::ready(Ok(None))
    }
    /// The apps installed on a team. A fake core has none.
    fn team_apps(&self, _team: String) -> Reply<Vec<InstalledApp>> {
        Reply::ready(Ok(Vec::new()))
    }
    /// The account's app notification preferences, read now. A fake core can't read them.
    fn app_notification_settings(&self) -> Reply<Option<teams_proto::UserGraphSettings>> {
        Reply::ready(Ok(None))
    }
    /// The account's cached app definition, shared with Activity notification writes. A fake core has none.
    fn app_definition(&self, _id: String) -> Reply<Option<teams_proto::AppDefinition>> {
        Reply::ready(Ok(None))
    }
    /// An app's tile, the picture its definition's `largeImageUrl` names on Microsoft's CDN, read by the engine once a
    /// run and cached; `None` when there is none or `url` isn't an app tile's. A fake core has none.
    fn app_picture(&self, _url: String) -> Reply<Option<PictureBytes>> {
        Reply::ready(Ok(None))
    }
    /// The apps the store offers to add to a team. A fake core offers none.
    fn app_store(&self) -> Reply<Vec<StoreApp>> {
        Reply::ready(Ok(Vec::new()))
    }
    /// How a team's channel did. A fake core doesn't know.
    fn channel_analytics(
        &self,
        _group: String,
        _channel: String,
        _period: AnalyticsPeriod,
    ) -> Reply<Option<TeamAnalytics>> {
        Reply::ready(Ok(None))
    }
    /// Teams' team templates. A fake core has none.
    fn team_templates(&self) -> Reply<Vec<TeamTemplateCard>> {
        Reply::ready(Ok(Vec::new()))
    }
    /// One template's channels and apps. A fake core knows none.
    fn team_template(&self, _id: String) -> Reply<Option<TeamTemplateDetails>> {
        Reply::ready(Ok(None))
    }
    /// How many Microsoft 365 groups the account could make a team of. A fake core knows none.
    fn owned_groups(&self) -> Reply<Option<usize>> {
        Reply::ready(Ok(Some(0)))
    }
    /// The picture Teams makes for a team from its name. A fake core has none.
    fn default_team_picture(&self, _name: String) -> Reply<Option<Vec<u8>>> {
        Reply::ready(Ok(None))
    }
    /// People the account knows whose name holds `query`, with their addresses: the attendee picker's suggestions. A
    /// fake core knows nobody.
    fn invitable(&self, query: String) -> Reply<Vec<Invitable>> {
        let _ = query;
        Reply::ready(Ok(Vec::new()))
    }
    /// What the service's search (Substrate) finds for `query` in `scope`, read now: the title bar's suggestions and
    /// results ([`crate::global_search`]). A fake core finds nothing.
    fn search_service(&self, query: String, scope: SearchScope) -> Reply<SearchAnswer> {
        let _ = (query, scope);
        Reply::ready(Ok(SearchAnswer::default()))
    }
    /// The busy times of `attendees` and the account from `start_ms`, and the starts all of them are free for
    /// `meeting_minutes`, read now: the scheduling form's Scheduler (`teams_core::calendar`). A fake core knows of no
    /// busy time.
    fn availability(
        &self,
        attendees: Vec<EventAttendee>,
        start_ms: i64,
        meeting_minutes: u32,
    ) -> Reply<MeetingAvailability> {
        let _ = (attendees, start_ms, meeting_minutes);
        Reply::ready(Ok(MeetingAvailability::default()))
    }
    /// The busy times of `people` (other people's calendars the account added: Add calendar) over `days` days from
    /// `start_ms`, read now (`teams_core::calendar`). A fake core knows of no busy time.
    fn people_calendars(
        &self,
        people: Vec<EventAttendee>,
        start_ms: i64,
        days: u32,
    ) -> Reply<Vec<teams_proto::PersonSchedule>> {
        let _ = (people, start_ms, days);
        Reply::ready(Ok(Vec::new()))
    }
    /// Makes a meeting of the account's own to start now, named `subject` (the calendar's Meet now): its chat and
    /// links. Only the user's Start meeting or Get a link to share asks for one. A fake core makes none.
    fn meet_now(&self, subject: String) -> Reply<teams_proto::MeetNowMeeting> {
        let _ = subject;
        Reply::ready(Err(teams_core::EngineError::MeetingUnread))
    }
    /// The profile card of the person `mri` names, read now (`teams_core::profile_card`): what Teams' card shows of them
    /// (names and addresses, presence, schedule, organization, the people they work with), each section `None` when its
    /// read failed. A fake core has none ([`teams_core::EngineError::ProfileUnread`]).
    fn profile_card(&self, mri: String) -> Reply<teams_core::ProfileCard> {
        let _ = mri;
        Reply::ready(Err(teams_core::EngineError::ProfileUnread))
    }
}

impl Core for EngineRequests {
    fn activity(&self) -> Reply<teams_core::activity::ActivityFeed> {
        EngineRequests::activity(self)
    }

    fn workflow_request(&self, request: teams_proto::WorkflowRequest) -> Reply<teams_proto::WorkflowResponse> {
        self.workflow_request(request)
    }

    fn upload_file(&self, path: std::path::PathBuf) -> Reply<teams_proto::UploadedFile> {
        EngineRequests::upload_file(self, path)
    }

    fn upload_picture(&self, conversation: String, picture: teams_proto::PastedPicture) -> Reply<String> {
        EngineRequests::upload_picture(self, conversation, picture)
    }
    fn me(&self) -> &str {
        EngineRequests::me(self)
    }

    fn calendar(&self, start_ms: i64, end_ms: i64) -> Reply<Vec<CalendarEvent>> {
        EngineRequests::calendar(self, start_ms, end_ms)
    }

    fn stored_calendar(&self, start_ms: i64, end_ms: i64) -> Reply<Vec<CalendarEvent>> {
        EngineRequests::stored_calendar(self, start_ms, end_ms)
    }

    fn translations(&self, conversation: String, language: String, ids: Vec<String>) -> Reply<Vec<StoredTranslation>> {
        EngineRequests::translations(self, conversation, language, ids)
    }

    fn keep_translations(&self, translations: Vec<StoredTranslation>) -> Reply<()> {
        EngineRequests::keep_translations(self, translations)
    }

    fn sent_originals(&self, conversation: String, keys: Vec<String>) -> Reply<Vec<SentOriginal>> {
        EngineRequests::sent_originals(self, conversation, keys)
    }

    fn keep_sent_original(&self, original: SentOriginal) -> Reply<()> {
        EngineRequests::keep_sent_original(self, original)
    }

    fn conversation(&self, id: String) -> Reply<Option<Conversation>> {
        EngineRequests::conversation(self, id)
    }

    fn glossary(&self, conversation: String, language: String) -> Reply<Vec<GlossaryTerm>> {
        EngineRequests::glossary(self, conversation, language)
    }

    fn keep_glossary(&self, terms: Vec<GlossaryTerm>) -> Reply<()> {
        EngineRequests::keep_glossary(self, terms)
    }

    fn transcript(&self, write: TranscriptWrite) -> Reply<i64> {
        EngineRequests::transcript(self, write)
    }

    fn call_transcripts(&self, conversation: String) -> Reply<Vec<CallTranscript>> {
        EngineRequests::call_transcripts(self, conversation)
    }

    fn transcript_lines(&self, id: i64) -> Reply<Vec<TranscriptLine>> {
        EngineRequests::transcript_lines(self, id)
    }

    fn calendar_event(&self, id: String) -> Reply<FullEvent> {
        EngineRequests::calendar_event(self, id)
    }

    fn write_event(&self, write: EventWrite) -> Reply<EventWritten> {
        EngineRequests::write_event(self, write)
    }

    fn create_channel(&self, draft: ChannelDraft) -> Reply<ChannelMade> {
        EngineRequests::create_channel(self, draft)
    }

    fn add_channel_members(
        &self,
        team_group: String,
        channel_group: String,
        members: Vec<String>,
    ) -> Reply<Vec<String>> {
        EngineRequests::add_channel_members(self, team_group, channel_group, members)
    }

    fn channel_thread(
        &self,
        team: String,
        team_group: String,
        channel_group: String,
        name: String,
    ) -> Reply<Option<String>> {
        EngineRequests::channel_thread(self, team, team_group, channel_group, name)
    }

    fn team_people(&self, team: String, keyword: String) -> Reply<Vec<TeamPerson>> {
        EngineRequests::team_people(self, team, keyword)
    }

    fn team_write(&self, write: TeamWrite, change: Option<TeamChange>) -> Reply<TeamWritten> {
        EngineRequests::team_write(self, write, change)
    }

    fn own_team_view(&self, requests: Vec<OwnTeamView>, change: Option<TeamChange>) -> Reply<()> {
        EngineRequests::own_team_view(self, requests, change)
    }

    fn team_roster(&self, team: String) -> Reply<Vec<TeamMember>> {
        EngineRequests::team_roster(self, team)
    }

    fn meeting(&self, chat: String) -> Reply<Option<MeetingDetails>> {
        EngineRequests::meeting(self, chat)
    }

    fn channel_meetup(&self, channel: String) -> Reply<Option<teams_core::ChannelMeetup>> {
        EngineRequests::channel_meetup(self, channel)
    }

    fn channel_event(&self, mailbox: String, ical_uid: String) -> Reply<FullEvent> {
        EngineRequests::channel_event(self, mailbox, ical_uid)
    }

    fn title_meet_now(&self, channel: String, title: String) -> Reply<()> {
        EngineRequests::title_meet_now(self, channel, title)
    }

    fn team_settings(&self, team: String) -> Reply<Option<String>> {
        EngineRequests::team_settings(self, team)
    }

    fn team_tags(&self, team: String) -> Reply<TagsRead> {
        EngineRequests::team_tags(self, team)
    }

    fn team_code(&self, team: String) -> Reply<Option<String>> {
        EngineRequests::team_code(self, team)
    }

    fn channel_section(&self, action: teams_proto::ChannelSectionAction) -> Reply<()> {
        EngineRequests::channel_section(self, action)
    }

    fn channel_data(&self, query: teams_proto::ChannelRead) -> Reply<teams_proto::ChannelData> {
        EngineRequests::channel_data(self, query)
    }

    fn team_channels(&self, group: String) -> Reply<Vec<ManagedChannel>> {
        EngineRequests::team_channels(self, group)
    }

    fn pending_invites(&self) -> Reply<Vec<PendingInvite>> {
        EngineRequests::pending_invites(self)
    }

    fn team_analytics(&self, group: String, period: AnalyticsPeriod) -> Reply<Option<TeamAnalytics>> {
        EngineRequests::team_analytics(self, group, period)
    }

    fn all_teams_analytics(&self, period: AnalyticsPeriod) -> Reply<Vec<TeamAnalytics>> {
        EngineRequests::all_teams_analytics(self, period)
    }

    fn joinable_teams(&self, keywords: Option<String>) -> Reply<Vec<JoinableTeam>> {
        EngineRequests::joinable_teams(self, keywords)
    }

    fn addable_people(&self, keyword: String) -> Reply<Vec<TeamPerson>> {
        EngineRequests::addable_people(self, keyword)
    }

    fn team_join_requests(&self, group: String) -> Reply<Vec<TeamMember>> {
        EngineRequests::team_join_requests(self, group)
    }

    fn team_picture(&self, team: TeamPicture) -> Reply<Option<PictureBytes>> {
        EngineRequests::team_picture(self, team)
    }

    fn team_apps(&self, team: String) -> Reply<Vec<InstalledApp>> {
        EngineRequests::team_apps(self, team)
    }

    fn app_notification_settings(&self) -> Reply<Option<teams_proto::UserGraphSettings>> {
        EngineRequests::app_notification_settings(self)
    }

    fn app_definition(&self, id: String) -> Reply<Option<teams_proto::AppDefinition>> {
        EngineRequests::app_definition(self, id)
    }

    fn app_store(&self) -> Reply<Vec<StoreApp>> {
        EngineRequests::app_store(self)
    }

    fn channel_analytics(
        &self,
        group: String,
        channel: String,
        period: AnalyticsPeriod,
    ) -> Reply<Option<TeamAnalytics>> {
        EngineRequests::channel_analytics(self, group, channel, period)
    }

    fn team_templates(&self) -> Reply<Vec<TeamTemplateCard>> {
        EngineRequests::team_templates(self)
    }

    fn team_template(&self, id: String) -> Reply<Option<TeamTemplateDetails>> {
        EngineRequests::team_template(self, id)
    }

    fn owned_groups(&self) -> Reply<Option<usize>> {
        EngineRequests::owned_groups(self)
    }

    fn default_team_picture(&self, name: String) -> Reply<Option<Vec<u8>>> {
        EngineRequests::default_team_picture(self, name)
    }

    fn invitable(&self, query: String) -> Reply<Vec<Invitable>> {
        EngineRequests::invitable(self, query)
    }

    fn search_service(&self, query: String, scope: SearchScope) -> Reply<SearchAnswer> {
        EngineRequests::search_service(self, query, scope)
    }

    fn availability(
        &self,
        attendees: Vec<EventAttendee>,
        start_ms: i64,
        meeting_minutes: u32,
    ) -> Reply<MeetingAvailability> {
        EngineRequests::availability(self, attendees, start_ms, meeting_minutes)
    }

    fn people_calendars(
        &self,
        people: Vec<EventAttendee>,
        start_ms: i64,
        days: u32,
    ) -> Reply<Vec<teams_proto::PersonSchedule>> {
        EngineRequests::people_calendars(self, people, start_ms, days)
    }

    fn meet_now(&self, subject: String) -> Reply<teams_proto::MeetNowMeeting> {
        EngineRequests::meet_now(self, subject)
    }

    fn read_only(&self) -> bool {
        EngineRequests::read_only(self)
    }

    fn write(&self, action: Action) -> Reply<OutboxEntry> {
        EngineRequests::write(self, action)
    }

    fn resend(&self, id: i64) -> Reply<OutboxEntry> {
        EngineRequests::resend(self, id)
    }

    fn transient(&self, transient: Transient) -> Reply<TransientSent> {
        EngineRequests::transient(self, transient)
    }

    fn own_availability(&self, status: OwnStatus) -> Reply<TransientSent> {
        EngineRequests::own_availability(self, status)
    }

    fn sync(&self) -> Reply<CatchUpReport> {
        EngineRequests::sync(self)
    }

    fn load_older(&self, conversation: String) -> Reply<Option<Synced>> {
        EngineRequests::load_older(self, conversation)
    }

    fn conversations(&self, limit: usize) -> Reply<Vec<Conversation>> {
        EngineRequests::conversations(self, limit)
    }

    fn messages(&self, conversation: String, before: Option<PageCursor>, limit: usize) -> Reply<MessagePage> {
        EngineRequests::messages(self, conversation, before, limit)
    }

    fn conversation_preview(&self, conversation: String) -> Reply<Option<Message>> {
        EngineRequests::conversation_preview(self, conversation)
    }

    fn message(&self, conversation: String, id: String) -> Reply<Option<Message>> {
        EngineRequests::message(self, conversation, id)
    }

    fn reactions(&self, conversation: String, id: String) -> Reply<Vec<Reaction>> {
        EngineRequests::reactions(self, conversation, id)
    }

    fn members(&self, conversation: String) -> Reply<Vec<Member>> {
        EngineRequests::members(self, conversation)
    }

    fn group_chat_with(&self, members: Vec<String>) -> Reply<Option<String>> {
        EngineRequests::group_chat_with(self, members)
    }

    fn unread_count(&self, conversation: String) -> Reply<u64> {
        EngineRequests::unread_count(self, conversation)
    }

    fn person(&self, mri: String) -> Reply<Person> {
        EngineRequests::person(self, mri)
    }

    fn presence(&self, mri: String) -> Reply<Availability> {
        EngineRequests::presence(self, mri)
    }

    fn reaction_animation(&self, id: String) -> Reply<Option<Strip>> {
        EngineRequests::reaction_animation(self, id)
    }

    fn message_picture(&self, src: String) -> Reply<Option<PictureBytes>> {
        EngineRequests::message_picture(self, src)
    }

    fn app_picture(&self, url: String) -> Reply<Option<PictureBytes>> {
        EngineRequests::app_picture(self, url)
    }

    fn file_thumbnail(&self, share_url: String) -> Reply<Option<PictureBytes>> {
        EngineRequests::file_thumbnail(self, share_url)
    }

    fn file_stream(&self, share_url: String, offset: u64, len: u64) -> Reply<FileStream> {
        EngineRequests::file_stream(self, share_url, offset, len)
    }

    fn file_size(&self, share_url: String) -> Reply<u64> {
        EngineRequests::file_size(self, share_url)
    }

    fn meeting_contents(&self, thread: String) -> Reply<Vec<teams_proto::MeetingContent>> {
        EngineRequests::meeting_contents(self, thread)
    }

    fn profile_card(&self, mri: String) -> Reply<teams_core::ProfileCard> {
        EngineRequests::profile_card(self, mri)
    }

    fn meeting_transcript(&self, location: String) -> Reply<String> {
        EngineRequests::meeting_transcript(self, location)
    }

    fn transcript_language(&self, location: String) -> Reply<Option<String>> {
        EngineRequests::transcript_language(self, location)
    }

    fn recording_presentation(
        &self,
        link: String,
        drive: String,
        item: String,
    ) -> Reply<Arc<teams_proto::Presentation>> {
        EngineRequests::recording_presentation(self, &link, &drive, &item)
    }

    fn recording_segment(&self, part: teams_proto::MediaSegmentLink) -> Reply<Vec<u8>> {
        EngineRequests::recording_segment(self, part)
    }

    fn recording_file(&self, link: String, drive: String, item: String) -> Reply<teams_proto::RecordingAccess> {
        EngineRequests::recording_file(self, &link, &drive, &item)
    }

    fn recording_expiration(&self, link: String, drive: String, item: String) -> Reply<Option<i64>> {
        EngineRequests::recording_expiration(self, &link, &drive, &item)
    }

    fn recent_reactions(&self, limit: usize) -> Reply<Vec<String>> {
        EngineRequests::recent_reactions(self, limit)
    }

    fn frequent_reactions(&self, limit: usize) -> Reply<Vec<String>> {
        EngineRequests::frequent_reactions(self, limit)
    }

    fn read_horizon(&self, conversation: String) -> Reply<Option<ReadHorizon>> {
        EngineRequests::read_horizon(self, conversation)
    }

    fn history_complete(&self, conversation: String) -> Reply<Option<bool>> {
        EngineRequests::history_complete(self, conversation)
    }

    fn outbox_entry(&self, id: i64) -> Reply<Option<OutboxEntry>> {
        EngineRequests::outbox_entry(self, id)
    }

    fn outbox_unsettled(&self, conversation: String) -> Reply<Vec<OutboxEntry>> {
        EngineRequests::outbox_unsettled(self, conversation)
    }

    fn left_by_earlier_runs(&self) -> Reply<Vec<OutboxEntry>> {
        EngineRequests::left_by_earlier_runs(self)
    }

    fn search(&self, query: String, within: Vec<String>, limit: usize) -> Reply<Vec<SearchHit>> {
        EngineRequests::search_in(self, query, within, limit)
    }

    fn teams(&self) -> Reply<Option<Vec<Team>>> {
        EngineRequests::teams(self)
    }

    fn chat_listing(&self) -> Reply<ChatListing> {
        EngineRequests::chat_listing(self)
    }

    fn sync_conversation(&self, conversation: String) -> Reply<Synced> {
        EngineRequests::sync_conversation(self, conversation)
    }

    fn reply_roots(&self, conversation: String) -> Reply<Vec<(String, String)>> {
        EngineRequests::reply_roots(self, conversation)
    }

    fn call(&self, action: CallAction) -> Reply<CallId> {
        EngineRequests::call(self, action)
    }

    fn call_health(&self, call: CallId) -> Reply<Option<CallHealth>> {
        EngineRequests::call_health(self, call)
    }

    fn call_meeting_options(&self, call: CallId) -> Reply<Option<teams_proto::MeetingOptions>> {
        EngineRequests::call_meeting_options(self, call)
    }
}

/// A core whose engine has stopped: every request answers [`EngineError::Stopped`](teams_core::EngineError::Stopped)
/// and nothing goes anywhere. For views shown without an engine (a window's own tests).
#[derive(Clone, Debug)]
pub struct StoppedCore {
    me: String,
}

impl StoppedCore {
    /// A stopped core of the account `me`.
    #[must_use]
    pub fn new(me: impl Into<String>) -> Self {
        Self { me: me.into() }
    }
}

fn stopped<T>() -> Reply<T> {
    Reply::ready(Err(teams_core::EngineError::Stopped))
}

impl Core for StoppedCore {
    fn me(&self) -> &str {
        &self.me
    }

    fn calendar(&self, _: i64, _: i64) -> Reply<Vec<CalendarEvent>> {
        stopped()
    }

    fn stored_calendar(&self, _: i64, _: i64) -> Reply<Vec<CalendarEvent>> {
        stopped()
    }

    fn write(&self, _: Action) -> Reply<OutboxEntry> {
        stopped()
    }

    fn resend(&self, _: i64) -> Reply<OutboxEntry> {
        stopped()
    }

    fn transient(&self, _: Transient) -> Reply<TransientSent> {
        stopped()
    }

    fn own_availability(&self, _: OwnStatus) -> Reply<TransientSent> {
        stopped()
    }

    fn sync(&self) -> Reply<CatchUpReport> {
        stopped()
    }

    fn load_older(&self, _: String) -> Reply<Option<Synced>> {
        stopped()
    }

    fn conversations(&self, _: usize) -> Reply<Vec<Conversation>> {
        stopped()
    }

    fn messages(&self, _: String, _: Option<PageCursor>, _: usize) -> Reply<MessagePage> {
        stopped()
    }

    fn message(&self, _: String, _: String) -> Reply<Option<Message>> {
        stopped()
    }

    fn reactions(&self, _: String, _: String) -> Reply<Vec<Reaction>> {
        stopped()
    }

    fn members(&self, _: String) -> Reply<Vec<Member>> {
        stopped()
    }

    fn unread_count(&self, _: String) -> Reply<u64> {
        stopped()
    }

    fn person(&self, _: String) -> Reply<Person> {
        stopped()
    }

    fn presence(&self, _: String) -> Reply<Availability> {
        stopped()
    }

    fn reaction_animation(&self, _: String) -> Reply<Option<Strip>> {
        stopped()
    }

    fn message_picture(&self, _: String) -> Reply<Option<PictureBytes>> {
        stopped()
    }

    fn file_thumbnail(&self, _: String) -> Reply<Option<PictureBytes>> {
        stopped()
    }

    fn file_stream(&self, _: String, _: u64, _: u64) -> Reply<FileStream> {
        stopped()
    }

    fn file_size(&self, _: String) -> Reply<u64> {
        stopped()
    }

    fn recent_reactions(&self, _: usize) -> Reply<Vec<String>> {
        stopped()
    }

    fn frequent_reactions(&self, _: usize) -> Reply<Vec<String>> {
        stopped()
    }

    fn read_horizon(&self, _: String) -> Reply<Option<ReadHorizon>> {
        stopped()
    }

    fn history_complete(&self, _: String) -> Reply<Option<bool>> {
        stopped()
    }

    fn outbox_entry(&self, _: i64) -> Reply<Option<OutboxEntry>> {
        stopped()
    }

    fn outbox_unsettled(&self, _: String) -> Reply<Vec<OutboxEntry>> {
        stopped()
    }

    fn left_by_earlier_runs(&self) -> Reply<Vec<OutboxEntry>> {
        stopped()
    }

    fn search(&self, _: String, _: Vec<String>, _: usize) -> Reply<Vec<SearchHit>> {
        stopped()
    }

    fn teams(&self) -> Reply<Option<Vec<Team>>> {
        stopped()
    }

    fn chat_listing(&self) -> Reply<ChatListing> {
        stopped()
    }

    fn sync_conversation(&self, _: String) -> Reply<Synced> {
        stopped()
    }

    fn reply_roots(&self, _: String) -> Reply<Vec<(String, String)>> {
        stopped()
    }

    fn call(&self, _: CallAction) -> Reply<CallId> {
        stopped()
    }
}

/// The engine's owning side: its events, one at a time, and its shutdown. The app's is the [`EngineHandle`]; a view
/// test hands in a fake.
pub trait EventSource: Send + 'static {
    /// The next event; `None` once the engine has stopped and every event was taken. Dropping the future before it
    /// is ready loses no event.
    fn next_event(&mut self) -> impl Future<Output = Option<Event>>;

    /// Stops the engine and waits until it has: blocking, so the bridge calls it off GPUI's foreground.
    fn shutdown(self);
}

impl EventSource for EngineHandle {
    fn next_event(&mut self) -> impl Future<Output = Option<Event>> {
        EngineHandle::next_event(self)
    }

    fn shutdown(self) {
        EngineHandle::shutdown(self);
    }
}

/// How the engine is shut down, once the drain hands its owner back.
type Shutdown = Box<dyn FnOnce() + Send>;

/// The drain while it runs: its stop signal, and the task that hands the owner back.
struct Draining {
    stop: oneshot::Sender<()>,
    task: Task<Shutdown>,
}

/// Where the engine's owner is.
enum Held {
    /// With the drain.
    Draining(Draining),
    /// Being shut down: the task ends once the engine's thread is joined. Every [`Bridge::shutdown`] awaits this one.
    Stopping(Shared<Task<()>>),
    /// Shut down, its thread joined.
    Stopped,
}

/// What sees each engine event just before the models apply it ([`Bridge::set_tap`]).
pub type Tap = Rc<dyn Fn(&Event, &mut App)>;

/// The tap the drain tells, shared by the bridge (which sets it) and its drain.
type Tapped = Rc<RefCell<Option<Tap>>>;

/// The engine as the UI holds it (see the module docs). A GPUI entity: [`Bridge::start`] builds it in a
/// `cx.new(...)`.
pub struct Bridge {
    core: Arc<dyn Core>,
    models: Models,
    held: Held,
    tap: Tapped,
}

impl Bridge {
    /// Takes over the engine's owner (`events`) and its request side (`core`), and starts draining its events into
    /// `models`.
    pub fn start<E: EventSource>(events: E, core: Arc<dyn Core>, models: Models, cx: &mut Context<Self>) -> Self {
        let (stop, stopped) = oneshot::channel();
        let drained = models.clone();
        let tap: Tapped = Rc::default();
        let tapped = Rc::clone(&tap);
        let task = cx.spawn(async move |_, cx: &mut AsyncApp| {
            let events = drain(events, stopped, &drained, &tapped, cx).await;
            Box::new(move || events.shutdown()) as Shutdown
        });
        Self {
            core,
            models,
            held: Held::Draining(Draining { stop, task }),
            tap,
        }
    }

    /// From now on `tap` (none: no tap) sees each engine event just before the models apply it, on GPUI's foreground.
    pub fn set_tap(&self, tap: Option<Tap>) {
        *self.tap.borrow_mut() = tap;
    }

    /// `event` handled as the drain handles one the engine told: the tap shown it, then the models apply it. A test
    /// stands in for the engine with it.
    pub fn tell(&mut self, event: Event, cx: &mut Context<Self>) {
        told(&self.tap, &self.models, event, cx);
    }

    /// The request side, for a view to keep.
    #[must_use]
    pub fn core(&self) -> Arc<dyn Core> {
        Arc::clone(&self.core)
    }

    #[must_use]
    pub fn models(&self) -> &Models {
        &self.models
    }

    /// Whether the engine is still held: not shut down, or its thread not joined yet.
    #[must_use]
    pub fn running(&self) -> bool {
        !matches!(self.held, Held::Stopped)
    }

    /// Stops the drain and shuts the engine down off the foreground; the task ends once the engine's thread is
    /// joined. A later call starts nothing new: its task ends with the first one's join, at once when that is done.
    /// Dropping the task doesn't cancel the shutdown.
    pub fn shutdown(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let stopping = match std::mem::replace(&mut self.held, Held::Stopped) {
            Held::Draining(draining) => self.stop(draining, cx),
            Held::Stopping(stopping) => stopping,
            Held::Stopped => return Task::ready(()),
        };
        self.held = Held::Stopping(stopping.clone());
        cx.spawn(async move |_, _: &mut AsyncApp| stopping.await)
    }

    /// The shutdown itself: the drain stopped and its owner taken back, the engine shut down on the background
    /// executor, then the models and the bridge told.
    fn stop(&self, Draining { stop, task }: Draining, cx: &mut Context<Self>) -> Shared<Task<()>> {
        // a drain that already ended (the engine stopped on its own) has dropped its receiver
        let _ = stop.send(());
        let models = self.models.clone();
        log::info!("the engine is shutting down");
        // told at once, not once its thread is joined: a window over an engine that is going (or whose thread is held
        // up) shows it offline, its calls ended and no Call offered
        models.engine_stopped(cx);
        cx.spawn(async move |bridge, cx: &mut AsyncApp| {
            let shutdown = task.await;
            cx.background_spawn(async move { shutdown() }).await;
            log::info!("the engine is shut down, its thread joined");
            cx.update(|cx| models.engine_stopped(cx));
            // a bridge that is gone has nobody left to ask
            let _ = bridge.update(cx, |bridge, _| bridge.held = Held::Stopped);
        })
        .shared()
    }

    /// The account's session can't renew its tokens: shows it signed out with `hint` and shuts the engine down
    /// ([`Bridge::shutdown`]), so nothing more is sent.
    pub fn sign_out(&mut self, hint: SharedString, cx: &mut Context<Self>) -> Task<()> {
        log::warn!("the account is signed out: shown, and the engine shut down");
        self.models.signed_out(hint, cx);
        self.shutdown(cx)
    }
}

/// Applies `events` to `models` until the engine's events end or `stop` comes, and hands the owner back.
async fn drain<E: EventSource>(
    mut events: E,
    mut stop: oneshot::Receiver<()>,
    models: &Models,
    tap: &Tapped,
    cx: &mut AsyncApp,
) -> E {
    loop {
        let next = {
            let event = pin!(events.next_event());
            // the stop first: an engine that tells events without a pause (a catch-up's) is still shut down when asked,
            // not once it falls quiet (it went on taking the user's asks meanwhile)
            match select(&mut stop, event).await {
                // stopped, or the bridge is gone without a shutdown
                Either::Left(_) => None,
                Either::Right((event, _)) => Some(event),
            }
        };
        match next {
            Some(Some(event)) => cx.update(|cx| told(tap, models, event, cx)),
            Some(None) => {
                log::warn!("the engine stopped on its own");
                cx.update(|cx| models.engine_stopped(cx));
                break;
            }
            None => break,
        }
    }
    events
}

/// `event` shown to the tap, if one is set, then applied to `models`. The tap is taken out first: it may set another.
fn told(tap: &Tapped, models: &Models, event: Event, cx: &mut App) {
    let tap = tap.borrow().clone();
    if let Some(tap) = tap {
        tap(&event, cx);
    }
    models.apply(event, cx);
}
