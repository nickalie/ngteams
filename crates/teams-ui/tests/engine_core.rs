//! The app's [`Core`]: the engine's own request side (`EngineRequests`), as the views hold it. Over an engine on the
//! mock it answers the views from the engine's store; once the engine is shut down, every request a view can make
//! answers `Stopped` and nothing more reaches the service.
//!
//! Every id and token is the mock's made-up one.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts};
use teams_core::{
    Action, CallAction, CallId, ChannelDraft, EngineError, EventAttendee, EventWrite, Outbox, OwnStatus, Reply,
    SearchScope, TranscriptWrite, Transient,
};
use teams_mock::{AB_CHAT, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B};
use teams_proto::{
    AnalyticsPeriod, Author, ChannelLayout, ChannelRead, ChannelSectionAction, Client, Credentials, HttpTransport, Mri,
    NewChannel, NewChannelKind, OwnTeamView, PastedFormat, PastedPicture, TeamPicture, TeamWrite, WorkflowRequest,
};
use teams_store::{Store, TranscriptLine};
use teams_ui::Core;
use tokio::sync::broadcast;

include!("support/recording_parts.rs");

/// How long a reply is waited for.
const WAIT: Duration = Duration::from_secs(30);

/// The account's mock token; it never runs out in a test.
struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
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

impl teams_trouter::TrouterCredentials for Tokens {
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

impl Tokens {
    fn access(&self) -> AccessToken {
        AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }
    }
}

/// The mock on a runtime of its own, and a's engine on it (no push channel) over a store in `_dir`.
struct Lab {
    runtime: tokio::runtime::Runtime,
    mock: ChatServiceMock,
    handle: EngineHandle,
    _dir: tempfile::TempDir,
}

impl Lab {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let mock = runtime.block_on(ChatServiceMock::start()).unwrap();
        let tokens = Arc::new(Tokens {
            token: mock.token(LAB_A).unwrap(),
            renewals: broadcast::channel(4).0,
        });
        let origin = mock.origin();
        let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let handle = engine::start(Parts {
            calls: None,
            store: Store::open(&dir.path().join("lab.sqlite")).unwrap(),
            client: Arc::new(Client::new(http())),
            outbox: Outbox::new(Arc::new(http()), Author::new(Mri::new(LAB_A), "Lab account")),
            push: None,
            options: EngineOptions {
                tick: Duration::from_millis(50),
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })
        .unwrap();
        Self {
            runtime,
            mock,
            handle,
            _dir: dir,
        }
    }
}

/// The answer, waited for on this (executor-free) thread; fails after [`WAIT`].
fn answer<T: Send + 'static>(reply: Reply<T>) -> Result<T, EngineError> {
    let (sent, got) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sent.send(reply.blocking_wait());
    });
    got.recv_timeout(WAIT).unwrap()
}

#[track_caller]
fn stopped<T: Send + 'static>(reply: Reply<T>) {
    assert!(
        matches!(answer(reply), Err(EngineError::Stopped)),
        "a stopped engine answers Stopped"
    );
}

#[test]
fn the_app_s_core_answers_the_views_from_the_engine_s_store() {
    let lab = Lab::start();
    let core: Arc<dyn Core> = Arc::new(lab.handle.requests());
    assert_eq!(core.me(), LAB_A);
    assert!(!core.read_only(), "an engine started to send isn't read-only");

    // a new store: nothing kept yet
    assert_eq!(answer(core.conversations(10)).unwrap(), Vec::new());
    assert_eq!(answer(core.unread_count(AB_CHAT.into())).unwrap(), 0);
    assert_eq!(answer(core.message(AB_CHAT.into(), "1".into())).unwrap(), None);
    assert_eq!(answer(core.reply_roots(AB_CHAT.into())).unwrap(), Vec::new());
    assert_eq!(answer(core.outbox_entry(1)).unwrap(), None);
    assert_eq!(answer(core.recent_reactions(5)).unwrap(), Vec::<String>::new());
    assert_eq!(answer(core.frequent_reactions(5)).unwrap(), Vec::<String>::new());
    assert_eq!(
        answer(core.group_chat_with(vec![LAB_A.into(), LAB_B.into()])).unwrap(),
        None
    );
    assert!(answer(core.stored_calendar(0, i64::MAX)).unwrap().is_empty());
    assert!(
        answer(core.sent_originals(AB_CHAT.into(), vec!["key".into()]))
            .unwrap()
            .is_empty()
    );
    assert!(
        answer(core.translations(AB_CHAT.into(), "de".into(), vec!["1".into()]))
            .unwrap()
            .is_empty()
    );
    answer(core.keep_translations(Vec::new())).unwrap();
    assert!(answer(core.glossary(AB_CHAT.into(), "de".into())).unwrap().is_empty());
    answer(core.keep_glossary(Vec::new())).unwrap();
    answer(core.conversation(AB_CHAT.into())).unwrap();
    lab.handle.shutdown();
}

#[test]
fn the_app_s_core_keeps_a_call_s_captions_in_the_engine_s_store() {
    let lab = Lab::start();
    let core: Arc<dyn Core> = Arc::new(lab.handle.requests());
    // a call's captions kept on this computer: opened, then gone with its lines
    let id = answer(core.transcript(TranscriptWrite::Open {
        conversation: AB_CHAT.into(),
        title: Some("Made-up call".into()),
        at: 1_000,
        rejoin_ms: 0,
    }))
    .unwrap();
    assert!(id > 0, "the store answers the new transcript's row");
    assert!(answer(core.transcript_lines(id)).unwrap().is_empty());
    assert!(
        answer(core.call_transcripts(AB_CHAT.into())).unwrap().is_empty(),
        "a transcript without a line isn't listed"
    );
    let line = TranscriptLine {
        utterance: "u1".into(),
        speaker: Some(LAB_B.into()),
        speaker_name: Some("Lab Bee".into()),
        started_at: 1_100,
        ended_at: 1_200,
        text: "a made-up caption".into(),
    };
    answer(core.transcript(TranscriptWrite::Lines {
        id,
        lines: vec![line.clone()],
    }))
    .unwrap();
    assert_eq!(answer(core.transcript_lines(id)).unwrap(), vec![line]);
    assert_eq!(answer(core.call_transcripts(AB_CHAT.into())).unwrap().len(), 1);
    answer(core.transcript(TranscriptWrite::Delete { id })).unwrap();
    assert!(answer(core.call_transcripts(AB_CHAT.into())).unwrap().is_empty());

    lab.handle.shutdown();
}

#[test]
fn once_the_engine_is_shut_down_every_request_answers_stopped_and_nothing_goes_out() {
    let lab = Lab::start();
    let core: Arc<dyn Core> = Arc::new(lab.handle.requests());
    let Lab {
        runtime,
        mock,
        handle,
        _dir,
    } = lab;
    handle.shutdown();
    let before = runtime.block_on(mock.received_requests()).len();
    let lab_after = |runtime: &tokio::runtime::Runtime| runtime.block_on(mock.received_requests()).len();

    ask_chat(core.as_ref());
    ask_files_and_recaps(core.as_ref());
    ask_calendar_and_calls(core.as_ref());
    ask_teams(core.as_ref());
    ask_team_admin(core.as_ref());

    assert_eq!(
        lab_after(&runtime),
        before,
        "nothing reached the service after the shutdown"
    );
}

fn ask_chat(core: &dyn Core) {
    let chat = || AB_CHAT.to_owned();
    stopped(core.write(Action::Delete {
        conversation: chat(),
        message_id: "1".into(),
    }));
    stopped(core.resend(1));
    stopped(core.transient(Transient::Typing { conversation: chat() }));
    stopped(core.own_availability(OwnStatus::Available));
    stopped(core.sync());
    stopped(core.load_older(chat()));
    stopped(core.conversations(10));
    stopped(core.messages(chat(), None, 10));
    stopped(core.message(chat(), "1".into()));
    stopped(core.reactions(chat(), "1".into()));
    stopped(core.members(chat()));
    stopped(core.group_chat_with(vec![LAB_B.into()]));
    stopped(core.call_runs(chat()));
    stopped(core.unread_count(chat()));
    stopped(core.person(LAB_B.into()));
    stopped(core.presence(LAB_B.into()));
    stopped(core.reaction_animation("like".into()));
    stopped(core.recent_reactions(5));
    stopped(core.frequent_reactions(5));
    stopped(core.read_horizon(chat()));
    stopped(core.history_complete(chat()));
    stopped(core.outbox_entry(1));
    stopped(core.outbox_unsettled(chat()));
    stopped(core.left_by_earlier_runs());
    stopped(core.search("made-up".into(), vec![chat()], 10));
    stopped(core.search_service("made-up".into(), SearchScope::All));
    stopped(core.invitable("made".into()));
    stopped(core.chat_listing());
    stopped(core.sync_conversation(chat()));
    stopped(core.reply_roots(chat()));
    stopped(core.translations(chat(), "de".into(), vec!["1".into()]));
    stopped(core.keep_translations(Vec::new()));
    stopped(core.sent_originals(chat(), vec!["key".into()]));
    stopped(core.glossary(chat(), "de".into()));
    stopped(core.keep_glossary(Vec::new()));
    stopped(core.conversation(chat()));
    stopped(core.workflow_request(WorkflowRequest::Catalog));
}

fn ask_files_and_recaps(core: &dyn Core) {
    let link = "https://madeup-my.sharepoint.com/:v:/g/personal/lab_a/ITEM1";
    stopped(core.upload_file(std::path::PathBuf::from("/nonexistent/made-up.txt")));
    stopped(core.upload_picture(
        AB_CHAT.into(),
        PastedPicture {
            format: PastedFormat::Png,
            bytes: vec![0x89, b'P', b'N', b'G'],
        },
    ));
    stopped(core.message_picture("https://us-api.asm.skype.com/v1/objects/0-made-up/views/imgo".into()));
    stopped(core.file_thumbnail(link.into()));
    stopped(core.file_stream(link.into(), 0, 10));
    stopped(core.file_size(link.into()));
    stopped(core.meeting_contents(AB_CHAT.into()));
    stopped(core.meeting_transcript(link.into()));
    stopped(core.transcript_language(link.into()));
    stopped(core.recording_presentation(link.into(), "drive".into(), "ITEM1".into()));
    stopped(core.recording_segment(made_up_presentation().tracks[0].init().clone()));
    stopped(core.recording_file(link.into(), "drive".into(), "ITEM1".into()));
    stopped(core.recording_expiration(link.into(), "drive".into(), "ITEM1".into()));
    stopped(core.transcript(TranscriptWrite::Delete { id: 1 }));
    stopped(core.call_transcripts(AB_CHAT.into()));
    stopped(core.transcript_lines(1));
}

fn ask_calendar_and_calls(core: &dyn Core) {
    let attendee = || EventAttendee {
        address: "lab-b@example.invalid".into(),
        name: None,
        mri: Some(LAB_B.into()),
        required: true,
    };
    stopped(core.calendar(0, 1));
    stopped(core.stored_calendar(0, 1));
    stopped(core.calendar_event("event".into()));
    stopped(core.write_event(EventWrite::Delete { id: "event".into() }));
    stopped(core.availability(vec![attendee()], 0, 30));
    stopped(core.people_calendars(vec![attendee()], 0, 1));
    stopped(core.meeting(AB_CHAT.into()));
    stopped(core.meet_now("Made-up meeting".into()));
    let call = CallId::for_tests(1);
    stopped(core.call(CallAction::Mute { call, muted: true }));
    stopped(core.call_health(call));
    stopped(core.call_meeting_options(call));
}

fn ask_teams(core: &dyn Core) {
    let team = || "19:made-up-team@thread.tacv2".to_owned();
    let group = || "00000000-0000-4000-8000-0000000000aa".to_owned();
    let channel = || "19:made-up-channel@thread.tacv2".to_owned();
    stopped(core.teams());
    stopped(core.team_people(team(), "lab".into()));
    stopped(core.team_roster(team()));
    stopped(core.team_settings(team()));
    stopped(core.team_tags(team()));
    stopped(core.team_code(team()));
    stopped(core.team_channels(group()));
    stopped(core.pending_invites());
    stopped(core.team_analytics(group(), AnalyticsPeriod::LastSevenDays));
    stopped(core.all_teams_analytics(AnalyticsPeriod::LastThirtyDays));
    stopped(core.channel_analytics(group(), channel(), AnalyticsPeriod::LastNinetyDays));
    stopped(core.joinable_teams(None));
    stopped(core.addable_people("lab".into()));
    stopped(core.team_join_requests(group()));
    stopped(core.team_picture(TeamPicture {
        own: LAB_A.into(),
        group: group(),
        name: "Made-up team".into(),
        etag: "1".into(),
    }));
    stopped(core.team_apps(team()));
    stopped(core.app_store());
    stopped(core.team_templates());
    stopped(core.team_template("com.microsoft.teams.template.ManageAProject".into()));
    stopped(core.owned_groups());
    stopped(core.default_team_picture("Made-up team".into()));
    stopped(core.channel_meetup(channel()));
    stopped(core.channel_event("team@example.invalid".into(), "uid".into()));
    stopped(core.channel_data(ChannelRead::Sections));
}

fn ask_team_admin(core: &dyn Core) {
    let team = || "19:made-up-team@thread.tacv2".to_owned();
    let group = || "00000000-0000-4000-8000-0000000000aa".to_owned();
    let channel = || "19:made-up-channel@thread.tacv2".to_owned();
    stopped(core.create_channel(ChannelDraft {
        channel: NewChannel {
            team: team(),
            group: group(),
            mailbox: None,
            name: "Made-up channel".into(),
            description: String::new(),
            kind: NewChannelKind::Standard,
            layout: ChannelLayout::Threads,
        },
        show_by_default: false,
    }));
    stopped(core.add_channel_members(group(), group(), vec![LAB_B.into()]));
    stopped(core.channel_thread(team(), group(), group(), "Made-up channel".into()));
    stopped(core.team_write(
        TeamWrite::Settings {
            team: team(),
            settings: "{}".into(),
        },
        None,
    ));
    stopped(core.own_team_view(vec![OwnTeamView::HideAll { team: team() }], None));
    stopped(core.title_meet_now(channel(), "Made-up title".into()));
    stopped(core.channel_section(ChannelSectionAction::Remove { channel: channel() }));
}
