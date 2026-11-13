//! What a [`Core`] that implements only the requests every core must answers for the rest (a view's fake without
//! that service): each of the trait's own answers, as its documentation gives it. Nothing is asked of an engine.

#![cfg(test)]

use std::path::PathBuf;

use futures_util::FutureExt as _;
use teams_core::sync::{CatchUpReport, Synced};
use teams_core::{
    Action, Availability, CallAction, CallId, ChannelDraft, ChannelProblem, ChatListing, EngineError, EventAttendee,
    EventWrite, FileStream, OwnStatus, Person, PictureBytes, Reply, SearchScope, Strip, TranscriptWrite, Transient,
    TransientSent,
};
use teams_proto::{
    AnalyticsPeriod, ChannelLayout, ChannelRead, ChannelSectionAction, NewChannel, NewChannelKind, OwnTeamView,
    PastedFormat, PastedPicture, TagsRead, TeamPicture, TeamWrite, WorkflowFailure, WorkflowRequest,
};
use teams_store::{
    Conversation, Member, Message, MessagePage, OutboxEntry, PageCursor, Reaction, ReadHorizon, SearchHit, Team,
};
use teams_ui::{Core, StoppedCore};

include!("support/recording_parts.rs");

const ME: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const CHAT: &str = "19:made-up@thread.v2";
const LINK: &str = "https://madeup-my.sharepoint.com/:v:/g/personal/lab_a/ITEM1";

/// A core with only the required requests, each answered `Stopped` (by a [`StoppedCore`]): every other one is the
/// trait's own.
struct Bare(StoppedCore);

impl Core for Bare {
    fn me(&self) -> &str {
        self.0.me()
    }
    fn write(&self, action: Action) -> Reply<OutboxEntry> {
        self.0.write(action)
    }
    fn resend(&self, id: i64) -> Reply<OutboxEntry> {
        self.0.resend(id)
    }
    fn transient(&self, transient: Transient) -> Reply<TransientSent> {
        self.0.transient(transient)
    }
    fn own_availability(&self, status: OwnStatus) -> Reply<TransientSent> {
        self.0.own_availability(status)
    }
    fn sync(&self) -> Reply<CatchUpReport> {
        self.0.sync()
    }
    fn load_older(&self, conversation: String) -> Reply<Option<Synced>> {
        self.0.load_older(conversation)
    }
    fn conversations(&self, limit: usize) -> Reply<Vec<Conversation>> {
        self.0.conversations(limit)
    }
    fn messages(&self, conversation: String, before: Option<PageCursor>, limit: usize) -> Reply<MessagePage> {
        self.0.messages(conversation, before, limit)
    }
    fn message(&self, conversation: String, id: String) -> Reply<Option<Message>> {
        self.0.message(conversation, id)
    }
    fn reactions(&self, conversation: String, id: String) -> Reply<Vec<Reaction>> {
        self.0.reactions(conversation, id)
    }
    fn members(&self, conversation: String) -> Reply<Vec<Member>> {
        self.0.members(conversation)
    }
    fn recent_reactions(&self, limit: usize) -> Reply<Vec<String>> {
        self.0.recent_reactions(limit)
    }
    fn frequent_reactions(&self, limit: usize) -> Reply<Vec<String>> {
        self.0.frequent_reactions(limit)
    }
    fn unread_count(&self, conversation: String) -> Reply<u64> {
        self.0.unread_count(conversation)
    }
    fn person(&self, mri: String) -> Reply<Person> {
        self.0.person(mri)
    }
    fn presence(&self, mri: String) -> Reply<Availability> {
        self.0.presence(mri)
    }
    fn reaction_animation(&self, id: String) -> Reply<Option<Strip>> {
        self.0.reaction_animation(id)
    }
    fn message_picture(&self, src: String) -> Reply<Option<PictureBytes>> {
        self.0.message_picture(src)
    }
    fn file_thumbnail(&self, share_url: String) -> Reply<Option<PictureBytes>> {
        self.0.file_thumbnail(share_url)
    }
    fn file_stream(&self, share_url: String, offset: u64, len: u64) -> Reply<FileStream> {
        self.0.file_stream(share_url, offset, len)
    }
    fn file_size(&self, share_url: String) -> Reply<u64> {
        self.0.file_size(share_url)
    }
    fn read_horizon(&self, conversation: String) -> Reply<Option<ReadHorizon>> {
        self.0.read_horizon(conversation)
    }
    fn history_complete(&self, conversation: String) -> Reply<Option<bool>> {
        self.0.history_complete(conversation)
    }
    fn outbox_entry(&self, id: i64) -> Reply<Option<OutboxEntry>> {
        self.0.outbox_entry(id)
    }
    fn outbox_unsettled(&self, conversation: String) -> Reply<Vec<OutboxEntry>> {
        self.0.outbox_unsettled(conversation)
    }
    fn left_by_earlier_runs(&self) -> Reply<Vec<OutboxEntry>> {
        self.0.left_by_earlier_runs()
    }
    fn search(&self, query: String, within: Vec<String>, limit: usize) -> Reply<Vec<SearchHit>> {
        self.0.search(query, within, limit)
    }
    fn teams(&self) -> Reply<Option<Vec<Team>>> {
        self.0.teams()
    }
    fn chat_listing(&self) -> Reply<ChatListing> {
        self.0.chat_listing()
    }
    fn sync_conversation(&self, conversation: String) -> Reply<Synced> {
        self.0.sync_conversation(conversation)
    }
    fn reply_roots(&self, conversation: String) -> Reply<Vec<(String, String)>> {
        self.0.reply_roots(conversation)
    }
    fn call(&self, action: CallAction) -> Reply<CallId> {
        self.0.call(action)
    }
}

fn bare() -> Bare {
    Bare(StoppedCore::new(ME))
}

/// A trait's own reply is answered at once.
fn now<T>(reply: Reply<T>) -> Result<T, EngineError> {
    reply.now_or_never().expect("the trait's own answer is ready at once")
}

/// Answered with nothing in it.
fn empty<T>(reply: Reply<Vec<T>>) -> bool {
    now(reply).is_ok_and(|found| found.is_empty())
}

/// Answered none.
fn none<T>(reply: Reply<Option<T>>) -> bool {
    now(reply).is_ok_and(|found| found.is_none())
}

/// Refused with the error `debug` writes.
fn refused<T>(reply: Reply<T>, debug: &str) -> bool {
    now(reply).is_err_and(|error| format!("{error:?}") == debug)
}

/// Every check holds, each named by what it asks.
#[track_caller]
fn all(checks: &[(&str, bool)]) {
    for (asked, held) in checks {
        assert!(held, "{asked}");
    }
}

fn attendee() -> EventAttendee {
    EventAttendee {
        address: "lab-b@example.invalid".into(),
        name: None,
        mri: None,
        required: true,
    }
}

#[test]
fn files_recordings_and_workflows_are_unavailable_without_their_service() {
    let core = bare();
    let drive = || "drive".to_owned();
    let picture = PastedPicture {
        format: PastedFormat::Png,
        bytes: vec![1, 2, 3],
    };
    let segment = made_up_presentation().tracks[0].init().clone();
    let workflow = now(core.workflow_request(WorkflowRequest::Catalog));
    all(&[
        ("me", core.me() == ME),
        ("a core sends unless it says otherwise", !core.read_only()),
        (
            "upload_file",
            refused(core.upload_file(PathBuf::from("/made-up.txt")), "FileUnavailable"),
        ),
        (
            "upload_picture",
            refused(core.upload_picture(CHAT.into(), picture), "FileUnavailable"),
        ),
        (
            "workflow_request",
            workflow.ok() == Some(Err(WorkflowFailure::Connection)),
        ),
        (
            "meeting_contents",
            refused(core.meeting_contents(CHAT.into()), "RecapUnread"),
        ),
        (
            "meeting_transcript",
            refused(core.meeting_transcript(LINK.into()), "RecapUnread"),
        ),
        ("transcript_language", none(core.transcript_language(LINK.into()))),
        (
            "recording_presentation",
            refused(
                core.recording_presentation(LINK.into(), drive(), "ITEM1".into()),
                "FileUnavailable",
            ),
        ),
        (
            "recording_segment",
            refused(core.recording_segment(segment), "FileUnavailable"),
        ),
        (
            "recording_file",
            refused(
                core.recording_file(LINK.into(), drive(), "ITEM1".into()),
                "FileUnavailable",
            ),
        ),
        (
            "recording_expiration",
            none(core.recording_expiration(LINK.into(), drive(), "ITEM1".into())),
        ),
    ]);
}

#[test]
fn kept_things_are_none_and_keeping_them_keeps_nothing() {
    let core = bare();
    let open = TranscriptWrite::Open {
        conversation: CHAT.into(),
        title: None,
        at: 1,
        rejoin_ms: 0,
    };
    let call = CallId::for_tests(1);
    all(&[
        (
            "translations",
            empty(core.translations(CHAT.into(), "de".into(), vec!["1".into()])),
        ),
        ("keep_translations", now(core.keep_translations(Vec::new())).is_ok()),
        ("conversation", none(core.conversation(CHAT.into()))),
        ("glossary", empty(core.glossary(CHAT.into(), "de".into()))),
        ("keep_glossary", now(core.keep_glossary(Vec::new())).is_ok()),
        (
            "sent_originals",
            empty(core.sent_originals(CHAT.into(), vec!["key".into()])),
        ),
        (
            "a fake keeps no transcript and answers 0",
            now(core.transcript(open)).ok() == Some(0),
        ),
        ("call_transcripts", empty(core.call_transcripts(CHAT.into()))),
        ("transcript_lines", empty(core.transcript_lines(1))),
        ("group_chat_with", none(core.group_chat_with(vec![ME.into()]))),
        ("call_runs", now(core.call_runs(CHAT.into())).ok() == Some(false)),
        (
            "call_ended",
            now(core.call_ended(CHAT.into(), "1".into())).ok() == Some(false),
        ),
        ("call_health", none(core.call_health(call))),
        ("call_meeting_options", none(core.call_meeting_options(call))),
    ]);
}

#[test]
fn the_calendar_is_empty_and_nothing_is_scheduled() {
    let core = bare();
    let found = now(core.search_service("lab".into(), SearchScope::All)).unwrap();
    let free = now(core.availability(vec![attendee()], 0, 30)).unwrap();
    let unsaved = now(core.write_event(EventWrite::Delete { id: "event".into() }));
    all(&[
        ("calendar", empty(core.calendar(0, 1))),
        ("stored_calendar", empty(core.stored_calendar(0, 1))),
        (
            "calendar_event",
            refused(core.calendar_event("event".into()), "CalendarUnread"),
        ),
        (
            "write_event",
            unsaved.is_err_and(|error| format!("{error:?}").contains("nothing saves here")),
        ),
        ("meeting", none(core.meeting(CHAT.into()))),
        ("channel_meetup", none(core.channel_meetup(CHAT.into()))),
        (
            "channel_event",
            refused(
                core.channel_event("team@example.invalid".into(), "uid".into()),
                "CalendarUnread",
            ),
        ),
        (
            "title_meet_now",
            now(core.title_meet_now(CHAT.into(), "Made-up".into())).is_ok(),
        ),
        ("invitable", empty(core.invitable("lab".into()))),
        (
            "search_service",
            format!("{found:?}") == format!("{:?}", teams_core::SearchAnswer::default()),
        ),
        (
            "availability",
            format!("{free:?}") == format!("{:?}", teams_core::MeetingAvailability::default()),
        ),
        ("people_calendars", empty(core.people_calendars(vec![attendee()], 0, 1))),
        ("meet_now", refused(core.meet_now("Made-up".into()), "MeetingUnread")),
        ("profile_card", refused(core.profile_card(ME.into()), "ProfileUnread")),
    ]);
}

#[test]
fn team_changes_are_refused() {
    let core = bare();
    let team = || "19:made-up-team@thread.tacv2".to_owned();
    let draft = ChannelDraft {
        channel: NewChannel {
            team: team(),
            group: "group".into(),
            mailbox: None,
            name: "Made-up".into(),
            description: String::new(),
            kind: NewChannelKind::Private,
            layout: ChannelLayout::Posts,
        },
        show_by_default: true,
    };
    let not_changed = "TeamNotChanged { code: None }";
    let settings = TeamWrite::Settings {
        team: team(),
        settings: "{}".into(),
    };
    let expanded = ChannelSectionAction::Expanded {
        section: "s".into(),
        expanded: true,
    };
    let added = now(core.add_channel_members("group".into(), "channel".into(), vec![ME.into()]));
    all(&[
        (
            "create_channel",
            refused(
                core.create_channel(draft),
                &format!("ChannelNotCreated {{ why: {:?} }}", ChannelProblem::Other),
            ),
        ),
        (
            "everyone asked for counts as added",
            added.ok() == Some(vec![ME.to_owned()]),
        ),
        (
            "channel_thread",
            none(core.channel_thread(team(), "g".into(), "c".into(), "n".into())),
        ),
        ("team_write", refused(core.team_write(settings, None), not_changed)),
        (
            "own_team_view",
            refused(
                core.own_team_view(vec![OwnTeamView::ShowAll { team: team() }], None),
                not_changed,
            ),
        ),
        ("channel_section", refused(core.channel_section(expanded), not_changed)),
        (
            "channel_data",
            refused(core.channel_data(ChannelRead::Sections), not_changed),
        ),
    ]);
}

#[test]
fn team_reads_are_empty() {
    let core = bare();
    let team = || "19:made-up-team@thread.tacv2".to_owned();
    all(&[
        ("team_people", empty(core.team_people(team(), "lab".into()))),
        ("team_roster", empty(core.team_roster(team()))),
        ("team_settings", none(core.team_settings(team()))),
        (
            "team_tags",
            now(core.team_tags(team())).ok() == Some(TagsRead::default()),
        ),
        ("team_code", none(core.team_code(team()))),
    ]);
}

#[test]
fn team_directories_and_apps_are_empty() {
    let core = bare();
    let group = || "group".to_owned();
    let picture = TeamPicture {
        own: ME.into(),
        group: group(),
        name: "Made-up".into(),
        etag: "1".into(),
    };
    all(&[
        ("team_channels", empty(core.team_channels(group()))),
        ("pending_invites", empty(core.pending_invites())),
        (
            "team_analytics",
            none(core.team_analytics(group(), AnalyticsPeriod::LastSevenDays)),
        ),
        (
            "all_teams_analytics",
            empty(core.all_teams_analytics(AnalyticsPeriod::LastSevenDays)),
        ),
        ("joinable_teams", empty(core.joinable_teams(Some("lab".into())))),
        ("addable_people", empty(core.addable_people("lab".into()))),
        ("team_join_requests", empty(core.team_join_requests(group()))),
        ("team_picture", none(core.team_picture(picture))),
        ("team_apps", empty(core.team_apps("team".into()))),
        ("app_store", empty(core.app_store())),
        ("app_definition", none(core.app_definition("app".into()))),
        (
            "app_picture",
            none(core.app_picture("https://res.cdn.office.net/teamsappdata/a.png".into())),
        ),
        ("cached_app_definition", none(core.cached_app_definition("app".into()))),
        (
            "cached_app_picture",
            none(core.cached_app_picture("https://res.cdn.office.net/teamsappdata/a.png".into())),
        ),
        (
            "channel_analytics",
            none(core.channel_analytics(group(), "channel".into(), AnalyticsPeriod::LastNinetyDays)),
        ),
        ("team_templates", empty(core.team_templates())),
        ("team_template", none(core.team_template("template".into()))),
        ("a fake owns no group", now(core.owned_groups()).ok() == Some(Some(0))),
        (
            "default_team_picture",
            none(core.default_team_picture("Made-up".into())),
        ),
    ]);
}
