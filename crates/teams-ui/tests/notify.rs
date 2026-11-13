//! The notifications on GPUI's test context, over the fake engine (`common`) and a fake notification server that
//! records what it is asked: which messages notify (someone else's, delivered, or brought by a catch-up or a replay
//! after the push channel was last heard before it went down and unread; never the account's own, one the store
//! already had, history, an edit; the "notifications" setting; a chat shown in a focused window, asked again before
//! its notifications show; channels and own streams), a mention marked, more than three of one chat collapsed into
//! one (still marked when an earlier one mentioned the account), activating one opening its chat, a chat read (its
//! horizon moved, or a catch-up changed it) closing its notifications, a chat opened while its messages were looked
//! at showing none of them.
//! The same server on the sandbox's real private bus is `tests/desktop_bus.rs`. Every id and name is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;
#[expect(dead_code, reason = "this test uses part of the fake tool")]
#[path = "support/fake_translator.rs"]
mod fake_translator;
#[path = "common/notices.rs"]
mod notices;

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::{LAB_A, LAB_B, Rig, Written};
use gpui::{AppContext as _, Entity};
use notices::{Asked, FakeServer, chat, message};
use teams_core::{CLOCK_SKEW_MS, Event, MessageChange, Source};
use teams_store::{Applied, ConversationKind, Member, Message, ReadHorizon};
use teams_trouter::Down;
use teams_ui::ViewTrace;
use teams_ui::local_ai::{Backend, Tools};
use teams_ui::notify::{GATHER, Notice, NoticeKind, Notifier, NotifierEvent, PictureShape, may_notify};
use teams_ui::settings::{Notifications, Settings, SettingsModel};

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const TRIO: &str = "19:0000000000000000000000000000trio@thread.v2";
const CHANNEL: &str = "19:channel0001@thread.tacv2";
const NOTES: &str = "48:notes";

/// The notifier over a rig, its server, what it opened and the chat a test says is shown in a focused window.
struct Notifying {
    rig: Rig,
    notifier: Entity<Notifier>,
    server: Arc<FakeServer>,
    opened: Arc<Mutex<Vec<String>>>,
    /// Each notification told shown ([`NotifierEvent::Shown`]): its chat and title.
    told: Arc<Mutex<Vec<(String, String)>>>,
    viewing: Arc<Mutex<Option<String>>>,
    settings: Entity<SettingsModel>,
    written: Written,
}

fn now_ms() -> i64 {
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()).unwrap()
}

/// `message` with a mention of `mri` in its properties, as the service stores one.
fn mentioning(mut message: Message, mri: &str) -> Message {
    message.properties = Some(format!(
        "{{\"mentions\":\"[{{\\\"@type\\\":\\\"http://schema.skype.com/Mention\\\",\\\"itemid\\\":0,\\\"mri\\\":\
         \\\"{mri}\\\",\\\"mentionType\\\":\\\"person\\\",\\\"displayName\\\":\\\"Lab A\\\"}}]\"}}"
    ));
    message
}

impl Notifying {
    fn start(settings: Settings) -> Self {
        let rig = Rig::start();
        rig.core.conversations.lock().unwrap().extend([
            chat(AB, ConversationKind::OneToOne, None),
            chat(TRIO, ConversationKind::Group, Some("Lab trio")),
            chat(CHANNEL, ConversationKind::Channel, Some("General")),
        ]);
        rig.core.members.lock().unwrap().extend([
            (AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]),
            (
                TRIO.to_owned(),
                vec![
                    Member::new(LAB_A, None),
                    Member::new(LAB_B, None),
                    Member::new(LAB_C, None),
                ],
            ),
        ]);
        rig.core.names.lock().unwrap().extend([
            (LAB_B.to_owned(), "Bee".to_owned()),
            (LAB_C.to_owned(), "Cee".to_owned()),
        ]);
        let written = Written::default();
        let trace = Arc::new(ViewTrace::new(written.clone()));
        let server = Arc::new(FakeServer::default());
        let viewing = Arc::new(Mutex::new(None::<String>));
        let opened = Arc::new(Mutex::new(Vec::new()));
        let core = rig.cx.read(|cx| rig.bridge.read(cx).core());
        let settings = rig.cx.update(|cx| cx.new(|_| SettingsModel::in_memory(settings)));
        let models = rig.models.clone();
        let (shown, given, recorded) = (Arc::clone(&viewing), Arc::clone(&server), Arc::clone(&opened));
        let told = Arc::new(Mutex::new(Vec::new()));
        let telling = Arc::clone(&told);
        let notifier = rig.cx.update(|cx| {
            let notifier = cx.new(|cx| {
                let mut notifier = Notifier::new(core, &models, settings.clone(), trace, cx);
                notifier.set_server(given);
                notifier.set_viewing(Box::new(move |_| shown.lock().unwrap().clone()));
                notifier
            });
            cx.subscribe(&notifier, move |_, event, _| match event {
                NotifierEvent::Open(id) => recorded.lock().unwrap().push(id.clone()),
                NotifierEvent::OpenChannel(id) => recorded.lock().unwrap().push(format!("channel {id}")),
                NotifierEvent::OpenActivity(item) => recorded.lock().unwrap().push(format!("activity {item}")),
                NotifierEvent::Shown { conversation, title } => {
                    telling.lock().unwrap().push((conversation.clone(), title.clone()));
                }
            })
            .detach();
            notifier
        });
        Self {
            rig,
            notifier,
            server,
            opened,
            told,
            viewing,
            settings,
            written,
        }
    }

    /// `message` newly stored in its conversation, brought by `source`.
    fn arrive(&self, message: Message, source: Source) {
        self.rig.send(Event::Message {
            conversation: message.conversation_id.clone(),
            id: message.id.clone(),
            change: MessageChange::New,
            applied: Applied::Inserted,
            message: Some(Box::new(message)),
            source,
        });
    }

    /// Lets the gathering run its course.
    fn settle(&self) {
        self.rig.cx.executor().advance_clock(GATHER);
        self.rig.cx.run_until_parked();
    }

    fn shown_lines(&self) -> Vec<(String, String)> {
        self.written
            .lines()
            .into_iter()
            .filter(|line| line["view"] == "notification" && line["change"] == "shown")
            .map(|line| {
                (
                    line["conversation"].as_str().unwrap().to_owned(),
                    line["kinds"][0].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }
}

#[test]
fn a_reply_notifies_by_its_own_text_instead_of_its_quote() {
    for quoted in [
        "Earlier message",
        "This message was blocked due to organization policy.",
    ] {
        let n = Notifying::start(Settings::default());
        let reply = format!(
            "<blockquote itemtype=\"http://schema.skype.com/Reply\" itemid=\"42\"><p>{quoted}</p></blockquote>\
             <p>My <b>reply</b></p><p>Second line</p>"
        );
        n.arrive(message(AB, "reply", LAB_B, &reply, 5_000), Source::Delivery);
        n.settle();
        assert_eq!(n.server.shown()[0].body, "My reply");
    }
}

#[test]
fn a_translated_reply_notifies_by_its_own_translated_text() {
    let mut settings = Settings::default();
    settings.set_chat_language(LAB_A, AB, Some("de"));
    let n = Notifying::start(settings);
    let reply = message(
        AB,
        "reply",
        LAB_B,
        "<blockquote><p>Earlier message</p></blockquote><p>My reply</p>",
        5_000,
    );
    let translated =
        teams_proto::chat::parse_html("<blockquote><p>Frühere Nachricht</p></blockquote><p>Meine Antwort</p>");
    n.rig
        .core
        .translations
        .lock()
        .unwrap()
        .push(teams_store::StoredTranslation {
            conversation_id: AB.to_owned(),
            message_id: reply.id.clone(),
            version: reply.version,
            language: "de".to_owned(),
            kind: teams_store::TranslationKind::Translated,
            content: serde_json::to_string(&translated).unwrap(),
            source_language: Some("en".to_owned()),
        });
    n.arrive(reply, Source::Delivery);
    n.settle();
    assert_eq!(n.server.shown()[0].body, "Meine Antwort");
}

#[test]
fn a_withheld_message_still_notifies_by_its_policy_notice() {
    let n = Notifying::start(Settings::default());
    let mut withheld = message(AB, "withheld", LAB_B, "", 5_000);
    withheld.properties = Some(r#"{"policyViolation":{"state":1,"violationType":"dlp"}}"#.to_owned());
    n.arrive(withheld, Source::Delivery);
    n.settle();
    assert_eq!(
        n.server.shown()[0].body,
        "This message was blocked due to organization policy."
    );
}

#[test]
fn a_delivered_message_from_someone_else_notifies_with_its_name_and_first_line() {
    let n = Notifying::start(Settings::default());
    n.arrive(
        message(AB, "1", LAB_B, "<p>lunch at <b>noon</b>?</p><p>second line</p>", 5_000),
        Source::Delivery,
    );
    n.settle();
    assert_eq!(
        n.server.shown(),
        [Notice {
            conversation: AB.to_owned(),
            kind: NoticeKind::Message,
            summary: "Bee".to_owned(),
            body: "lunch at noon?".to_owned(),
            picture: None,
        }]
    );
    // a group chat names its sender and the chat
    n.arrive(message(TRIO, "2", LAB_C, "<p>hi all</p>", 6_000), Source::Delivery);
    n.settle();
    assert_eq!(n.server.shown()[1].summary, "Cee in Lab trio");
    // each one shown is told, with its chat and title
    assert_eq!(
        *n.told.lock().unwrap(),
        [
            (AB.to_owned(), "Bee".to_owned()),
            (TRIO.to_owned(), "Cee in Lab trio".to_owned())
        ]
    );
    // the trace says which chat and what kind, never a name or text
    assert_eq!(
        n.shown_lines(),
        [
            (AB.to_owned(), "message".to_owned()),
            (TRIO.to_owned(), "message".to_owned())
        ]
    );
    let text = n.written.text();
    assert!(!text.contains("Bee") && !text.contains("lunch"), "{text}");
    // a notification writes nothing: the engine was only read, and runs on
    let asked = n.rig.core.asked();
    assert!(
        asked
            .iter()
            .all(|asked| !matches!(*asked, "write" | "transient" | "resend" | "sync")),
        "{asked:?}"
    );
    assert_eq!(n.rig.shutdowns(), 0);
}

#[test]
fn the_account_s_own_a_replay_from_before_the_start_one_already_stored_history_and_an_edit_never_notify() {
    let n = Notifying::start(Settings::default());
    n.arrive(message(AB, "1", LAB_A, "<p>mine</p>", 5_000), Source::Delivery);
    n.arrive(message(AB, "2", LAB_B, "<p>again</p>", 5_001), Source::Replay);
    n.arrive(message(AB, "3", LAB_B, "<p>older</p>", 5_002), Source::History);
    for (change, applied) in [
        (MessageChange::New, Applied::Ignored),
        (MessageChange::New, Applied::Updated),
        (MessageChange::Edit, Applied::Updated),
        (MessageChange::Reaction, Applied::Updated),
    ] {
        n.rig.send(Event::Message {
            conversation: AB.to_owned(),
            id: "4".to_owned(),
            change,
            applied,
            message: Some(Box::new(message(AB, "4", LAB_B, "<p>known</p>", 5_003))),
            source: Source::Delivery,
        });
    }
    let mut deleted = message(AB, "5", LAB_B, "", 5_004);
    deleted.delete_time = Some(5_005);
    n.arrive(deleted, Source::Delivery);
    let mut event = message(AB, "6", LAB_B, "<addmember/>", 5_006);
    event.message_type = "ThreadActivity/AddMember".to_owned();
    n.arrive(event, Source::Delivery);
    n.settle();
    assert_eq!(n.server.asked(), []);
    assert!(!may_notify(
        LAB_A,
        AB,
        &message(AB, "1", LAB_A, "x", 1),
        Source::Delivery
    ));
    assert!(may_notify(LAB_A, AB, &message(AB, "1", LAB_B, "x", 1), Source::CatchUp));
}

/// A meeting's chat, for the system messages Teams never notifies for.
const MEETING: &str = "19:meeting_bWFkZS11cA@thread.v2";

/// `kind` with `content` from `sender` in the meeting's chat.
fn system(id: &str, sender: &str, kind: &str, content: &str, at: i64) -> Message {
    let mut event = message(MEETING, id, sender, content, at);
    event.message_type = kind.to_owned();
    event
}

/// A call's start and end, a recording started and ready, a transcript, the thread's events (one of a kind no capture
/// showed) and a text the thread itself sent, in the meeting's chat.
fn system_arrivals() -> Vec<Message> {
    let recording = |status: &str, code: &str| {
        format!(
            "<URIObject type=\"Video.2/CallRecording.1\"><RecordingStatus status=\"{status}\" code=\"{code}\" />\
             <Title>Weekly sync</Title><a href=\"https://example.test/play\">Play</a>\
             <RecordingContent contentTypes=\"Recording+Transcript\" duration=\"0:16:50\"></RecordingContent></URIObject>"
        )
    };
    let ended = format!(
        "<ended/><partlist alt=\"\" count=\"1\"><part identity=\"{LAB_B}\"><name>{LAB_B}</name>\
         <duration>1930</duration></part></partlist>"
    );
    let joined = format!(r#"{{"eventtime":1,"initiator":"{LAB_B}","members":[{{"id":"{LAB_C}","friendlyname":""}}]}}"#);
    vec![
        system("1", LAB_B, "Event/Call", "<partlist alt =\"\"></partlist>", 7_000),
        system(
            "2",
            LAB_B,
            "RichText/Media_CallRecording",
            &recording("Initial", "0"),
            7_001,
        ),
        system("3", LAB_B, "Event/Call", &ended, 7_002),
        system(
            "4",
            LAB_B,
            "RichText/Media_CallRecording",
            &recording("Success", "200"),
            7_003,
        ),
        system(
            "5",
            LAB_B,
            "RichText/Media_CallTranscript",
            r#"{"callId":"c","isDeleted":false}"#,
            7_004,
        ),
        system("6", MEETING, "ThreadActivity/MemberJoined", &joined, 7_005),
        system(
            "7",
            MEETING,
            "ThreadActivity/TopicUpdate",
            "<topicupdate><value>x</value></topicupdate>",
            7_006,
        ),
        system("8", MEETING, "ThreadActivity/SomethingNew", "<somethingnew/>", 7_007),
        // a message of a type Teams notifies, sent by the thread itself: never named by its id
        system("9", MEETING, "RichText/Html", "<p>from the thread</p>", 7_008),
    ]
}

#[test]
fn a_call_a_recording_a_transcript_and_a_thread_s_event_never_notify_as_in_teams() {
    let n = Notifying::start(Settings::default());
    let arrivals = system_arrivals();
    for event in &arrivals {
        assert!(
            !may_notify(LAB_A, MEETING, event, Source::Delivery),
            "{} notifies",
            event.message_type
        );
    }
    for event in arrivals {
        n.arrive(event, Source::Delivery);
    }
    n.settle();
    assert_eq!(n.server.asked(), []);
    // what Teams notifies for: text and a card, from a person
    for kind in ["Text", "RichText/Html", "RichText/Media_Card"] {
        assert!(
            may_notify(
                LAB_A,
                MEETING,
                &system("10", LAB_B, kind, "<p>hi</p>", 7_009),
                Source::Delivery
            ),
            "{kind} doesn't notify"
        );
    }
    n.arrive(
        system("11", LAB_B, "RichText/Html", "<p>notes after the call</p>", 7_010),
        Source::Delivery,
    );
    n.settle();
    let shown = n.server.shown();
    assert_eq!(shown.len(), 1, "{shown:?}");
    assert_eq!(shown[0].body, "notes after the call");
    assert!(
        shown
            .iter()
            .all(|notice| !notice.body.contains("Play") && !notice.summary.contains("19:")),
        "{shown:?}"
    );
}

#[test]
fn a_mention_is_marked_and_mentions_only_shows_only_mentions() {
    let n = Notifying::start(Settings {
        notifications: Notifications::Mentions,
        ..Settings::default()
    });
    n.arrive(message(TRIO, "1", LAB_B, "<p>no mention</p>", 5_000), Source::Delivery);
    n.arrive(
        mentioning(message(TRIO, "2", LAB_B, "<p>see this</p>", 5_001), LAB_A),
        Source::Delivery,
    );
    // a mention of someone else isn't one of the account
    n.arrive(
        mentioning(message(TRIO, "3", LAB_B, "<p>for c</p>", 5_002), LAB_C),
        Source::Delivery,
    );
    n.arrive(
        mentioning(message(AB, "4", LAB_B, "<p>you</p>", 5_003), LAB_A),
        Source::Delivery,
    );
    n.settle();
    let shown = n.server.shown();
    assert_eq!(shown.len(), 2, "{shown:?}");
    assert_eq!(
        (shown[0].kind, shown[0].summary.as_str(), shown[0].body.as_str()),
        (NoticeKind::Mention, "Bee mentioned you in Lab trio", "see this")
    );
    assert_eq!(
        (shown[1].kind, shown[1].summary.as_str()),
        (NoticeKind::Mention, "Bee mentioned you")
    );
    assert_eq!(n.shown_lines()[0].1, "mention");
}

#[test]
fn notifications_off_shows_none_and_the_setting_is_read_as_it_now_stands() {
    let n = Notifying::start(Settings {
        notifications: Notifications::Off,
        ..Settings::default()
    });
    n.arrive(
        mentioning(message(AB, "1", LAB_B, "<p>you</p>", 5_000), LAB_A),
        Source::Delivery,
    );
    n.settle();
    assert_eq!(n.server.asked(), []);
    n.rig.cx.update(|cx| {
        n.settings.update(cx, |settings, cx| {
            settings.change(|settings| settings.notifications = Notifications::All, cx);
        });
    });
    n.arrive(message(AB, "2", LAB_B, "<p>now</p>", 5_001), Source::Delivery);
    n.settle();
    assert_eq!(n.server.shown().len(), 1);
}

#[test]
fn the_chat_shown_in_a_focused_window_never_notifies() {
    let n = Notifying::start(Settings::default());
    *n.viewing.lock().unwrap() = Some(AB.to_owned());
    n.arrive(message(AB, "1", LAB_B, "<p>seen</p>", 5_000), Source::Delivery);
    n.arrive(message(TRIO, "2", LAB_C, "<p>elsewhere</p>", 5_001), Source::Delivery);
    n.settle();
    let shown = n.server.shown();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].conversation, TRIO);
    *n.viewing.lock().unwrap() = None;
    n.arrive(message(AB, "3", LAB_B, "<p>away now</p>", 5_002), Source::Delivery);
    n.settle();
    assert_eq!(n.server.shown().len(), 2);
}

#[test]
fn channels_notify_only_mentions_and_own_streams_never() {
    let n = Notifying::start(Settings::default());
    n.arrive(
        message(CHANNEL, "1", LAB_B, "<p>to the team</p>", 5_000),
        Source::Delivery,
    );
    n.arrive(message(NOTES, "2", LAB_B, "<p>a note</p>", 5_001), Source::Delivery);
    n.arrive(
        mentioning(message(CHANNEL, "3", LAB_B, "<p>you there</p>", 5_002), LAB_A),
        Source::Delivery,
    );
    n.settle();
    let shown = n.server.shown();
    assert_eq!(shown.len(), 1, "{shown:?}");
    assert_eq!(
        (shown[0].conversation.as_str(), shown[0].kind),
        (CHANNEL, NoticeKind::Mention)
    );
    // activated, it opens the channel read-only, never as a chat
    let id = n.rig.cx.read(|cx| n.notifier.read(cx).shown())[0].0;
    n.rig
        .cx
        .update(|cx| n.notifier.update(cx, |notifier, cx| notifier.activated(id, cx)));
    n.rig.cx.run_until_parked();
    assert_eq!(*n.opened.lock().unwrap(), [format!("channel {CHANNEL}")]);
}

/// `millis` milliseconds after the epoch.
fn at(millis: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(u64::try_from(millis).unwrap())
}

#[test]
fn a_catch_up_s_message_notifies_only_if_it_came_after_the_push_channel_was_last_heard_and_is_unread() {
    let n = Notifying::start(Settings::default());
    let started = n.rig.cx.read(|cx| n.notifier.read(cx).cutoff());
    // before the first connect: since the start
    n.arrive(
        message(AB, "1", LAB_B, "<p>while the app was off</p>", started - 60_000),
        Source::CatchUp,
    );
    n.settle();
    assert_eq!(n.server.asked(), []);
    n.rig.send(Event::Connected {
        endpoint: "endpoint-1".to_owned(),
    });
    // a suspend: the socket was last heard twenty minutes ago, its missing pong noticed only now
    let heard = now_ms() - 20 * 60_000;
    n.rig.send(Event::Disconnected {
        down: Down::PongTimeout,
        heard: Some(at(heard)),
    });
    let cutoff = n.rig.cx.read(|cx| n.notifier.read(cx).cutoff());
    assert_eq!(
        cutoff,
        heard - CLOCK_SKEW_MS,
        "the cutoff is when it was last heard, not when the drop was told"
    );
    // a socket that never connected moves nothing
    n.rig.send(Event::Disconnected {
        down: Down::Closed,
        heard: None,
    });
    assert_eq!(n.rig.cx.read(|cx| n.notifier.read(cx).cutoff()), cutoff);
    n.rig.core.horizons.lock().unwrap().insert(
        TRIO.to_owned(),
        ReadHorizon {
            conversation_id: TRIO.to_owned(),
            user: LAB_A.to_owned(),
            last_read_arrival: heard + 180_000,
            updated: heard + 180_000,
            raw: None,
        },
    );
    n.arrive(
        message(AB, "2", LAB_B, "<p>before the drop</p>", cutoff - 1),
        Source::CatchUp,
    );
    // after it was last heard, before the drop was noticed
    n.arrive(
        message(AB, "3", LAB_B, "<p>missed in the silence</p>", heard + 60_000),
        Source::CatchUp,
    );
    // read on another device: at or before the horizon
    n.arrive(
        message(TRIO, "4", LAB_C, "<p>read elsewhere</p>", heard + 120_000),
        Source::CatchUp,
    );
    n.arrive(
        message(TRIO, "5", LAB_C, "<p>not read</p>", heard + 240_000),
        Source::CatchUp,
    );
    // pushed again on the reconnect, never stored before: missed as well
    n.arrive(
        message(AB, "6", LAB_B, "<p>replayed</p>", heard + 300_000),
        Source::Replay,
    );
    n.settle();
    let bodies: Vec<String> = n.server.shown().into_iter().map(|notice| notice.body).collect();
    assert_eq!(bodies, ["missed in the silence", "replayed", "not read"]);
}

#[test]
fn more_than_three_in_one_chat_collapse_into_one() {
    let n = Notifying::start(Settings::default());
    // a catch-up of messages that arrived before the app started notifies nothing, even five at once
    for id in 1..=5 {
        n.arrive(
            message(TRIO, &id.to_string(), LAB_C, "<p>burst</p>", 5_000 + id),
            Source::CatchUp,
        );
    }
    n.settle();
    let asked = n.server.asked();
    assert!(asked.is_empty(), "arrived before the start: {asked:?}");

    // five at once that arrived after the start, unread: straight into one
    let n = Notifying::start(Settings::default());
    let base = now_ms() + 60_000;
    for id in 1..=5 {
        n.arrive(
            message(TRIO, &id.to_string(), LAB_C, "<p>burst</p>", base + id),
            Source::CatchUp,
        );
    }
    n.settle();
    let shown = n.server.shown();
    assert_eq!(shown.len(), 1, "{shown:?}");
    assert_eq!(
        (shown[0].kind, shown[0].summary.as_str(), shown[0].body.as_str()),
        (NoticeKind::Collapsed, "Lab trio", "5 new messages")
    );

    // two, then two more: the two singles give way to one of four, which a fifth replaces
    let n = Notifying::start(Settings::default());
    for id in 1..=2 {
        n.arrive(
            message(AB, &id.to_string(), LAB_B, "<p>one</p>", 5_000),
            Source::Delivery,
        );
    }
    n.settle();
    assert_eq!(n.server.shown().len(), 2);
    for id in 3..=4 {
        n.arrive(
            message(AB, &id.to_string(), LAB_B, "<p>more</p>", 5_000),
            Source::Delivery,
        );
    }
    n.settle();
    assert_eq!(n.server.closed(), [1, 2]);
    let collapsed = n.server.asked().last().cloned().unwrap();
    let Asked::Show {
        notice,
        replaces: 0,
        id,
    } = collapsed
    else {
        panic!("{collapsed:?}");
    };
    assert_eq!(
        (notice.kind, notice.body.as_str()),
        (NoticeKind::Collapsed, "4 new messages")
    );
    // the collapsed one is told too, titled by its chat
    assert_eq!(
        n.told.lock().unwrap().last(),
        Some(&(AB.to_owned(), notice.summary.clone()))
    );
    n.arrive(
        mentioning(message(AB, "5", LAB_B, "<p>and you</p>", 5_000), LAB_A),
        Source::Delivery,
    );
    n.settle();
    let replaced = n.server.asked().last().cloned().unwrap();
    assert_eq!(
        replaced,
        Asked::Show {
            notice: Notice {
                conversation: AB.to_owned(),
                kind: NoticeKind::Collapsed,
                summary: "Bee".to_owned(),
                body: "5 new messages, one or more mentioning you".to_owned(),
                picture: None,
            },
            replaces: id,
            id,
        }
    );
    assert_eq!(n.rig.cx.read(|cx| n.notifier.read(cx).shown()), [(id, AB.to_owned())]);
}

#[test]
fn a_mention_shown_before_still_marks_the_notification_its_chat_collapses_into() {
    let n = Notifying::start(Settings::default());
    n.arrive(
        mentioning(message(AB, "1", LAB_B, "<p>you there?</p>", 5_000), LAB_A),
        Source::Delivery,
    );
    n.settle();
    assert_eq!(n.server.shown()[0].kind, NoticeKind::Mention);
    for id in 2..=4 {
        n.arrive(
            message(AB, &id.to_string(), LAB_B, "<p>more</p>", 5_000),
            Source::Delivery,
        );
    }
    n.settle();
    let collapsed = n.server.shown().last().cloned().unwrap();
    assert_eq!(
        (collapsed.kind, collapsed.body.as_str()),
        (NoticeKind::Collapsed, "4 new messages, one or more mentioning you")
    );
    // the next replaces it, still marked
    n.arrive(message(AB, "5", LAB_B, "<p>and more</p>", 5_000), Source::Delivery);
    n.settle();
    assert_eq!(
        n.server.shown().last().unwrap().body,
        "5 new messages, one or more mentioning you"
    );
    // once read, the chat starts again: no mark left over
    n.rig.core.unread.lock().unwrap().insert(AB.to_owned(), 0);
    n.rig.send(Event::ReadHorizon {
        conversation: AB.to_owned(),
    });
    for id in 6..=9 {
        n.arrive(
            message(AB, &id.to_string(), LAB_B, "<p>later</p>", 5_000),
            Source::Delivery,
        );
    }
    n.settle();
    assert_eq!(n.server.shown().last().unwrap().body, "4 new messages");
}

#[test]
fn a_chat_shown_or_opened_while_its_messages_are_looked_at_shows_none_of_them() {
    let n = Notifying::start(Settings::default());
    n.rig.core.hold_people.lock().unwrap().insert(LAB_B.to_owned());
    // every name asked answered, one ask after another (the chat's name, then the sender's)
    let release = |n: &Notifying| {
        let mut answered = 0;
        loop {
            let held: Vec<_> = n.rig.core.held.lock().unwrap().drain(..).collect();
            if held.is_empty() {
                break;
            }
            for (answer, person) in held {
                answer.send(Ok(person));
                answered += 1;
            }
            n.rig.cx.run_until_parked();
        }
        assert!(answered > 0, "nothing held");
    };
    // the names are asked; the chat is focused before the answers come
    n.arrive(message(AB, "1", LAB_B, "<p>one</p>", 5_000), Source::Delivery);
    n.settle();
    *n.viewing.lock().unwrap() = Some(AB.to_owned());
    release(&n);
    assert_eq!(n.server.asked(), []);

    // one shown, then another waits for its sender's name while the first is activated: the chat was opened after
    // the second came, so it shows nothing
    *n.viewing.lock().unwrap() = None;
    n.arrive(message(AB, "2", LAB_B, "<p>two</p>", 5_001), Source::Delivery);
    n.settle();
    release(&n);
    let first = n.rig.cx.read(|cx| n.notifier.read(cx).shown())[0].0;
    n.arrive(message(AB, "3", LAB_B, "<p>three</p>", 5_002), Source::Delivery);
    n.settle();
    n.rig
        .cx
        .update(|cx| n.notifier.update(cx, |notifier, cx| notifier.activated(first, cx)));
    release(&n);
    let bodies: Vec<String> = n.server.shown().into_iter().map(|notice| notice.body).collect();
    assert_eq!(bodies, ["two"]);
    // a later one shows again
    n.arrive(message(AB, "4", LAB_B, "<p>four</p>", 5_003), Source::Delivery);
    n.settle();
    release(&n);
    assert_eq!(n.server.shown().last().unwrap().body, "four");
}

#[test]
fn activating_one_opens_its_chat_and_closes_the_chat_s_others() {
    let n = Notifying::start(Settings::default());
    n.arrive(message(AB, "1", LAB_B, "<p>one</p>", 5_000), Source::Delivery);
    n.arrive(message(AB, "2", LAB_B, "<p>two</p>", 5_001), Source::Delivery);
    n.arrive(message(TRIO, "3", LAB_C, "<p>three</p>", 5_002), Source::Delivery);
    n.settle();
    assert_eq!(n.server.shown().len(), 3);
    n.rig
        .cx
        .update(|cx| n.notifier.update(cx, |notifier, cx| notifier.activated(2, cx)));
    n.rig.cx.run_until_parked();
    assert_eq!(*n.opened.lock().unwrap(), [AB]);
    assert_eq!(n.server.closed(), [1, 2]);
    assert_eq!(n.rig.cx.read(|cx| n.notifier.read(cx).shown()), [(3, TRIO.to_owned())]);
    // an id it never showed opens nothing
    n.rig
        .cx
        .update(|cx| n.notifier.update(cx, |notifier, cx| notifier.activated(99, cx)));
    assert_eq!(n.opened.lock().unwrap().len(), 1);
    let opened: Vec<_> = n
        .written
        .lines()
        .into_iter()
        .filter(|line| line["view"] == "notification" && line["change"] == "opened")
        .collect();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0]["conversation"], AB);
    // the chat starts its count again: one more is shown on its own
    n.arrive(message(AB, "4", LAB_B, "<p>four</p>", 5_003), Source::Delivery);
    n.settle();
    assert_eq!(n.server.shown().last().unwrap().kind, NoticeKind::Message);
}

#[test]
fn a_chat_read_to_its_end_closes_its_notifications() {
    let n = Notifying::start(Settings::default());
    n.arrive(message(AB, "1", LAB_B, "<p>one</p>", 5_000), Source::Delivery);
    n.settle();
    n.rig.core.unread.lock().unwrap().insert(AB.to_owned(), 1);
    n.rig.send(Event::ReadHorizon {
        conversation: AB.to_owned(),
    });
    assert_eq!(n.server.closed(), [] as [u32; 0]);
    n.rig.core.unread.lock().unwrap().insert(AB.to_owned(), 0);
    n.rig.send(Event::ReadHorizon {
        conversation: AB.to_owned(),
    });
    assert_eq!(n.server.closed(), [1]);
    assert_eq!(n.rig.cx.read(|cx| n.notifier.read(cx).shown()), []);
}

#[test]
fn a_chat_a_catch_up_finds_read_closes_its_notifications() {
    let n = Notifying::start(Settings::default());
    n.arrive(message(AB, "1", LAB_B, "<p>one</p>", 5_000), Source::Delivery);
    n.settle();
    // read on another device while the push channel was down: the catch-up stores the horizon and tells the chat
    // changed
    n.rig.core.unread.lock().unwrap().insert(AB.to_owned(), 0);
    n.rig.send(Event::ConversationChanged {
        conversation: AB.to_owned(),
    });
    assert_eq!(n.server.closed(), [1]);
    assert_eq!(n.rig.cx.read(|cx| n.notifier.read(cx).shown()), []);
}

#[test]
fn what_comes_before_a_server_is_given_is_never_shown() {
    let n = Notifying::start(Settings::default());
    let late = n.rig.cx.update(|cx| {
        let core = n.rig.bridge.read(cx).core();
        let trace = Arc::new(ViewTrace::off());
        cx.new(|cx| Notifier::new(core, &n.rig.models, n.settings.clone(), trace, cx))
    });
    n.arrive(message(AB, "1", LAB_B, "<p>one</p>", 5_000), Source::Delivery);
    n.settle();
    assert!(!n.rig.cx.read(|cx| late.read(cx).has_server()));
    let server = Arc::new(FakeServer::default());
    let given = Arc::clone(&server);
    n.rig.cx.update(|cx| late.update(cx, |late, _| late.set_server(given)));
    n.settle();
    assert_eq!(server.asked(), [], "dropped, not held for later");
    n.arrive(message(AB, "2", LAB_B, "<p>two</p>", 5_001), Source::Delivery);
    n.settle();
    let bodies: Vec<String> = server.shown().into_iter().map(|notice| notice.body).collect();
    assert_eq!(bodies, ["two"]);
}

/// The a–b chat read in German, its tool the fake `script`, and b's plain-text "Hello there" stored and delivered.
fn translated_delivery(script: &str, dir: &std::path::Path) -> Notifying {
    let mut settings = Settings::default();
    settings.set_chat_language(LAB_A, AB, Some("de"));
    let n = Notifying::start(settings);
    let program = fake_translator::fake(dir, script);
    n.rig
        .cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, program)])));
    let mut hello = message(AB, "1", LAB_B, "Hello there", 5_000);
    hello.message_type = "Text".to_owned();
    n.rig
        .core
        .messages
        .lock()
        .unwrap()
        .insert(AB.to_owned(), vec![hello.clone()]);
    n.arrive(hello, Source::Delivery);
    n.settle();
    n
}

/// Runs until the server was asked to show something, the fake tool answering on its own thread meanwhile.
fn until_shown(n: &Notifying) {
    for _ in 0..1500 {
        if !n.server.shown().is_empty() {
            return;
        }
        n.rig.cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("nothing shown");
}

#[test]
fn a_translated_chat_s_message_notifies_in_the_chat_s_language() {
    let dir = tempfile::tempdir().unwrap();
    let n = translated_delivery(fake_translator::FAKE_CLAUDE, dir.path());
    until_shown(&n);
    assert_eq!(n.server.shown()[0].body, "DE Hello there");
    assert_eq!(n.server.shown()[0].summary, "Bee");
    // the trace never carries the text
    assert!(!n.written.text().contains("Hello"));
}

#[test]
fn a_translation_slower_than_the_budget_notifies_as_written_once() {
    let dir = tempfile::tempdir().unwrap();
    let n = translated_delivery(&fake_translator::slow(), dir.path());
    n.rig
        .cx
        .executor()
        .advance_clock(teams_ui::chat_translate::previews::NOTIFY_BUDGET);
    until_shown(&n);
    assert_eq!(n.server.shown()[0].body, "Hello there");
    // the translation comes later: kept for the list, never notified again
    for _ in 0..300 {
        n.rig.cx.run_until_parked();
        if !n.rig.core.translations.lock().unwrap().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!n.rig.core.translations.lock().unwrap().is_empty());
    n.settle();
    assert_eq!(n.server.shown().len(), 1);
}

#[derive(Default)]
struct SoundNotices(Mutex<Vec<teams_ui::sounds::SoundEvent>>);

impl teams_ui::sounds::SoundPlayer for SoundNotices {
    fn apply(&self, _: &Settings, _: Option<std::path::PathBuf>) {}
    fn play(&self, event: teams_ui::sounds::SoundEvent, _: &str, _: bool) {
        self.0.lock().unwrap().push(event);
    }
    fn looped(&self, _: teams_ui::sounds::SoundEvent) -> teams_ui::Ringtone {
        panic!("notifications never loop")
    }
    fn preview(&self, _: teams_ui::sounds::SoundEvent) {
        panic!("notifications never preview");
    }
    fn stop_preview(&self) {}
    fn previewing(&self) -> Option<teams_ui::sounds::SoundEvent> {
        None
    }
    fn watch_previewing(&self, _: Box<dyn Fn() + Send + Sync>) {}
}

#[test]
fn collapsed_mentions_sound_for_this_batch_and_later_plain_messages_use_their_own_event() {
    use teams_ui::settings::attach_sounds;
    use teams_ui::sounds::SoundEvent;
    for chat in [AB, CHANNEL] {
        let n = Notifying::start(Settings::default());
        n.rig.core.channel_data.lock().unwrap().insert(
            format!("preferences:{CHANNEL}"),
            teams_proto::ChannelData(serde_json::json!({"properties":{
                "channelNotificationSettings":serde_json::json!({"allNewPosts":"On","dskNotif":"On"}).to_string()
            }})),
        );
        let sounds = Arc::new(SoundNotices::default());
        n.rig.cx.update(|cx| attach_sounds(&n.settings, sounds.clone(), cx));
        // First exceed the single-notification limit with a batch containing a mention.
        for id in 1..=4 {
            let msg = message(chat, &id.to_string(), LAB_B, "<p>Synthetic message</p>", 5_000 + id);
            n.arrive(if id == 4 { mentioning(msg, LAB_A) } else { msg }, Source::Delivery);
        }
        n.settle();
        assert_eq!(*sounds.0.lock().unwrap(), [SoundEvent::Mention]);
        n.arrive(message(chat, "5", LAB_B, "<p>Plain</p>", 5_005), Source::Delivery);
        n.settle();
        let plain = if chat == CHANNEL {
            SoundEvent::ChannelMessage
        } else {
            SoundEvent::Message
        };
        assert_eq!(*sounds.0.lock().unwrap(), [SoundEvent::Mention, plain]);
        n.arrive(
            mentioning(message(chat, "6", LAB_B, "<p>Mention</p>", 5_006), LAB_A),
            Source::Delivery,
        );
        n.settle();
        assert_eq!(
            *sounds.0.lock().unwrap(),
            [SoundEvent::Mention, plain, SoundEvent::Mention]
        );
    }
}

/// The account's feed.
const FEED: &str = "48:notifications";
/// A made-up app and the activity types its definition declares.
const APP: &str = "00000000-0000-4000-8000-00000000a0aa";
const APP_TYPE: &str = "customNotification";

/// An app's item of the feed: its own title, a preview, `subtype` its activity type.
fn app_item(id: &str, subtype: &str, arrival: i64, read: bool) -> Message {
    let activity = serde_json::json!({
        "activityType": "msGraph", "activitySubtype": subtype, "activityId": id,
        "sourceUserId": "28:app:made-up", "sourceAppDisplayName": "Made-up flows",
        "messagePreview": "the made-up build passed",
        "activityContext": {
            "teamsAppId": APP,
            "templateParameters": "{\"customMessage\":\"Build finished\"}",
        },
    });
    let mut item = message(FEED, id, LAB_A, "", arrival);
    item.message_type = "Text".to_owned();
    item.properties = Some(serde_json::json!({"activity": activity, "isread": read}).to_string());
    item
}

/// App preferences as the account's own properties carry them.
fn app_settings(setting: &serde_json::Value) -> teams_proto::UserGraphSettings {
    let answer = serde_json::json!({"userGraphSettings": setting.to_string()});
    teams_proto::UserGraphSettings::read_answer(answer.to_string().as_bytes()).unwrap()
}

#[test]
fn an_app_item_shows_its_banner_until_its_app_is_turned_off() {
    let n = Notifying::start(Settings::default());
    // no saved choice for the app: Teams' "Show in Activity and banner"
    *n.rig.core.app_settings.lock().unwrap() = Some(teams_proto::UserGraphSettings::default());
    n.arrive(app_item("1", APP_TYPE, 5_000, false), Source::Delivery);
    n.settle();
    assert_eq!(
        n.server.shown(),
        [Notice {
            conversation: FEED.to_owned(),
            kind: NoticeKind::Activity,
            summary: "Build finished".to_owned(),
            body: "the made-up build passed".to_owned(),
            picture: None,
        }]
    );
    assert_eq!(
        *n.told.lock().unwrap(),
        [(FEED.to_owned(), "Build finished".to_owned())]
    );
    assert_eq!(n.shown_lines(), [(FEED.to_owned(), "activity".to_owned())]);
    assert!(
        !n.written.text().contains("Build finished"),
        "the trace never carries the text"
    );
    // activated, it opens the Activity pane at the item
    let Asked::Show { id, .. } = n.server.asked()[0].clone() else {
        panic!("not shown")
    };
    n.rig
        .cx
        .update(|cx| n.notifier.update(cx, |notifier, cx| notifier.activated(id, cx)));
    assert_eq!(*n.opened.lock().unwrap(), ["activity 1".to_owned()]);
    // the app turned off (#27's write): no banner for its later item
    let mut off = teams_proto::UserGraphSettings::default();
    off.turn_off(APP, &["messageNotify".to_owned(), APP_TYPE.to_owned()])
        .unwrap();
    *n.rig.core.app_settings.lock().unwrap() = Some(off);
    n.arrive(app_item("2", APP_TYPE, 6_000, false), Source::Delivery);
    n.settle();
    assert_eq!(n.server.shown().len(), 1);
}

#[test]
fn an_app_item_follows_its_activity_type_s_choice() {
    let n = Notifying::start(Settings::default());
    // "Show in Activity" for one type, banner kept for another, "Off" for a third
    *n.rig.core.app_settings.lock().unwrap() = Some(app_settings(&serde_json::json!({
        APP: {
            "activityGenerationSettings": {"messageNotify": true, "taskDone": false},
            "toastSettings": {"messageNotify": false, "systemDefault": false, "taskDone": false},
            "emailSettings": {},
        },
    })));
    n.arrive(app_item("1", "messageNotify", 5_000, false), Source::Delivery);
    n.arrive(app_item("2", "taskDone", 5_100, false), Source::Delivery);
    n.settle();
    assert_eq!(n.server.shown(), []);
    n.arrive(app_item("3", APP_TYPE, 5_200, false), Source::Delivery);
    n.settle();
    assert_eq!(n.server.shown().len(), 1, "a type with no choice of its own shows one");
}

#[test]
fn an_app_item_shows_none_unread_preferences_mentions_only_or_off() {
    let n = Notifying::start(Settings::default());
    // preferences that can't be read: the app may be turned off, so nothing
    n.arrive(app_item("1", APP_TYPE, 5_000, false), Source::Delivery);
    n.settle();
    assert_eq!(n.server.asked(), []);
    assert!(n.rig.core.asked().contains(&"app_notification_settings"));
    // a person's item of the feed isn't an app's: its message notifies in its chat, the item never
    *n.rig.core.app_settings.lock().unwrap() = Some(teams_proto::UserGraphSettings::default());
    let mut person = app_item("2", "person", 5_100, false);
    person.properties = Some(
        serde_json::json!({"activity": {"activityType": "mentionInChat", "activitySubtype": "person"}}).to_string(),
    );
    n.arrive(person, Source::Delivery);
    // history and the account's settings
    n.arrive(app_item("3", APP_TYPE, 5_200, false), Source::History);
    n.settle();
    assert_eq!(n.server.asked(), []);
    for level in [Notifications::Mentions, Notifications::Off] {
        n.rig.cx.update(|cx| {
            n.settings.update(cx, |settings, cx| {
                settings.change(|settings| settings.notifications = level, cx);
            });
        });
        n.arrive(app_item("4", APP_TYPE, 5_300, false), Source::Delivery);
        n.settle();
        assert_eq!(n.server.asked(), [], "{level:?}");
    }
}

#[test]
fn a_catch_up_s_app_item_notifies_only_unread_and_after_the_cutoff() {
    let n = Notifying::start(Settings::default());
    *n.rig.core.app_settings.lock().unwrap() = Some(teams_proto::UserGraphSettings::default());
    let cutoff = n.rig.cx.read(|cx| n.notifier.read(cx).cutoff());
    n.arrive(app_item("1", APP_TYPE, cutoff - 1_000, false), Source::CatchUp);
    n.arrive(app_item("2", APP_TYPE, cutoff + 1_000, true), Source::Replay);
    n.settle();
    assert_eq!(n.server.asked(), []);
    n.arrive(app_item("3", APP_TYPE, cutoff + 2_000, false), Source::CatchUp);
    n.settle();
    assert_eq!(n.server.shown().len(), 1);
}

/// A made-up picture of `side` a side, as a profile's is stored.
fn stored_picture(side: u32) -> teams_store::StoredPicture {
    teams_store::StoredPicture {
        media_type: "image/png".to_owned(),
        bytes: png(side),
    }
}

fn png(side: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(side, side, image::Rgba([200, 120, 40, 255]))
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    bytes
}

/// `mri` with a stored picture, as their profile has them.
fn pictured(n: &Notifying, mri: &str, name: &str) {
    n.rig.core.people.lock().unwrap().insert(
        mri.to_owned(),
        teams_core::Person {
            mri: mri.to_owned(),
            name: Some(name.to_owned()),
            from: Some(teams_core::NameFrom::Graph),
            picture: Some(stored_picture(160)),
            looking: false,
        },
    );
}

fn shape_of(notice: &Notice) -> Option<(u32, u32, PictureShape)> {
    notice
        .picture
        .as_ref()
        .map(|picture| (picture.width(), picture.height(), picture.shape()))
}

#[test]
fn a_banner_shows_its_sender_s_picture_and_a_collapsed_one_only_in_a_1_1_chat() {
    let n = Notifying::start(Settings::default());
    pictured(&n, LAB_B, "Bee");
    let person = Some((96, 96, PictureShape::Circle));
    n.arrive(message(AB, "1", LAB_B, "<p>one</p>", 5_000), Source::Delivery);
    // Cee has no stored picture: the app's icon alone
    n.arrive(message(TRIO, "2", LAB_C, "<p>two</p>", 5_001), Source::Delivery);
    n.arrive(message(TRIO, "3", LAB_B, "<p>three</p>", 5_002), Source::Delivery);
    n.settle();
    let shown = n.server.shown();
    assert_eq!(shown.len(), 3);
    assert_eq!((shown[0].summary.as_str(), shape_of(&shown[0])), ("Bee", person));
    assert_eq!(
        (shown[1].summary.as_str(), shape_of(&shown[1])),
        ("Cee in Lab trio", None)
    );
    assert_eq!(shape_of(&shown[2]), person);
    let picture = shown[0].picture.as_ref().unwrap();
    assert_eq!(picture.rgba()[..4], [200, 120, 40, 0], "a corner is outside the circle");

    // collapsed: a 1:1 chat's banner is about the other person; a group's about the chat
    for (id, at) in [("4", 6_000), ("5", 6_001), ("6", 6_002)] {
        n.arrive(message(AB, id, LAB_B, "<p>more</p>", at), Source::Delivery);
        n.arrive(
            message(TRIO, &format!("t{id}"), LAB_B, "<p>more</p>", at),
            Source::Delivery,
        );
    }
    n.settle();
    let collapsed: Vec<Notice> = n
        .server
        .shown()
        .into_iter()
        .filter(|notice| notice.kind == NoticeKind::Collapsed)
        .collect();
    assert_eq!(collapsed.len(), 2, "{collapsed:?}");
    assert_eq!(
        (collapsed[0].conversation.as_str(), shape_of(&collapsed[0])),
        (AB, person)
    );
    assert_eq!(
        (collapsed[1].conversation.as_str(), shape_of(&collapsed[1])),
        (TRIO, None)
    );
}

#[test]
fn a_sender_s_picture_that_doesn_t_decode_shows_no_picture() {
    let n = Notifying::start(Settings::default());
    pictured(&n, LAB_B, "Bee");
    if let Some(person) = n.rig.core.people.lock().unwrap().get_mut(LAB_B) {
        person.picture = Some(teams_store::StoredPicture {
            media_type: "image/jpeg".to_owned(),
            bytes: vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 0],
        });
    }
    n.arrive(message(AB, "1", LAB_B, "<p>one</p>", 5_000), Source::Delivery);
    n.settle();
    let shown = n.server.shown();
    assert_eq!((shown[0].summary.as_str(), shown[0].picture.as_ref()), ("Bee", None));
}

#[test]
fn an_app_item_s_banner_shows_the_app_s_cached_tile_and_never_waits_for_one() {
    const TILE: &str = "https://res.cdn.office.net/teamsappdata/evergreen-assets/apps/made-up_largeImage.png?v=1";
    let n = Notifying::start(Settings::default());
    *n.rig.core.app_settings.lock().unwrap() = Some(teams_proto::UserGraphSettings::default());
    let definition = serde_json::json!([{ "id": APP, "name": "Made-up flows", "largeImageUrl": TILE }]);
    let definition = teams_proto::AppDefinitions::read_answer(definition.to_string().as_bytes())
        .pop()
        .unwrap()
        .definition;
    n.rig
        .core
        .app_definitions
        .lock()
        .unwrap()
        .insert(APP.to_owned(), definition);
    n.rig.core.app_pictures.lock().unwrap().insert(
        TILE.to_owned(),
        Arc::new(teams_proto::PastedPicture {
            format: teams_proto::PastedFormat::Png,
            bytes: png(192),
        }),
    );

    // the engine has neither the definition nor the tile yet: the banner shows without, and both are read for later
    *n.rig.core.apps_uncached.lock().unwrap() = true;
    n.arrive(app_item("1", APP_TYPE, 5_000, false), Source::Delivery);
    n.settle();
    assert_eq!(n.server.shown()[0].picture, None);
    let asked = n.rig.core.asked();
    let at = |name: &str| {
        asked
            .iter()
            .position(|asked| *asked == name)
            .unwrap_or_else(|| panic!("{name} never asked: {asked:?}"))
    };
    assert!(at("cached_app_definition") < at("app_definition"), "{asked:?}");
    assert!(at("app_definition") < at("app_picture"), "{asked:?}");

    // once the engine has them: the app's tile, square
    *n.rig.core.apps_uncached.lock().unwrap() = false;
    n.arrive(app_item("2", APP_TYPE, 6_000, false), Source::Delivery);
    n.settle();
    let shown = n.server.shown();
    assert_eq!(shape_of(&shown[1]), Some((96, 96, PictureShape::Square)));
    assert_eq!(shown[1].picture.as_ref().unwrap().rgba()[..4], [200, 120, 40, 255]);
}
