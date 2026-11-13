//! A started meeting's banner on GPUI's test context, over the fake engine (`common`) and a fake notification server
//! that records what it is asked: someone else's Meet now in a group chat, and a meeting's chat's or a channel's
//! meeting the calendar holds about now, show one with Join; the account's own start, a group call (it rings), a 1:1
//! call, history, a meeting not on the calendar, a muted channel, the switch off, notifications off and Do not disturb
//! show none; its Join and its body ask the desktop to go in or to open the chat; the call's end (or an end naming no
//! call) closes it, however late it comes, even while the banner is being shown and when the store held it before a
//! restart, while another call's end leaves it; the chat's header offers Join as its stored call events say, after a
//! restart and whatever order a catch-up brings them in. Every id and name is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;
#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
#[path = "common/notices.rs"]
mod notices;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};
use std::time::{SystemTime, UNIX_EPOCH};

use common::{LAB_A, LAB_B, Rig, Written};
use futures_util::future::BoxFuture;
use gpui::{AppContext as _, Entity};
use notices::{FakeServer, chat};
use teams_core::{
    CalendarEvent, ChannelEventRef, Event, EventKind, EventResponse, EventTime, MeetingInfo, MessageChange, ShowAs,
    Source,
};
use teams_store::{Applied, ConversationKind, Member, Message};
use teams_ui::ViewTrace;
use teams_ui::meeting_alert::{MeetingAlert, MeetingAlertEvent, MeetingJoin};
use teams_ui::models::Models;
use teams_ui::notify::{DEFAULT_ACTION, JOIN_ACTION, Notice, NoticeKind, NotificationServer};
use teams_ui::settings::{ManualStatus, Notifications, Settings, SettingsModel};

const TRIO: &str = "19:0000000000000000000000000000trio@thread.v2";
const MEETING: &str = "19:meeting_MadeUpWeeklySync01@thread.v2";
const CHANNEL: &str = "19:0123456789abcdef0123456789abcdef@thread.tacv2";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const LINK: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/SyntheticHuddle?i=10-0-0-2&amp;e=1";

struct Alerting {
    rig: Rig,
    alert: Entity<MeetingAlert>,
    server: Arc<FakeServer>,
    asked: Arc<Mutex<Vec<MeetingAlertEvent>>>,
    written: Written,
}

fn now_ms() -> i64 {
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()).unwrap()
}

fn call_event(conversation: &str, id: &str, sender: &str, content: String) -> Message {
    let now = now_ms();
    Message {
        conversation_id: conversation.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: "Event/Call".to_owned(),
        content,
        properties: None,
        arrival_time: now,
        compose_time: Some(now),
        edit_time: None,
        delete_time: None,
    }
}

/// A group chat's meeting's start, `starter` named as Teams names the starter of a Meet now.
fn huddle(starter: &str) -> String {
    format!(
        "<partlist type=\"started\" alt=\"\"><part identity=\"{starter}\"><name>{starter}</name></part></partlist>\
         <conversationType>\"huddleGroupCall\"</conversationType><callEventType>callStarted</callEventType>\
         <conversationUrl>{LINK}</conversationUrl><callId>huddle-1</callId>"
    )
}

fn start() -> String {
    format!(
        "<partlist alt =\"\"></partlist><callEventType>callStarted</callEventType>\
         <conversationUrl>{LINK}</conversationUrl><callId>call-1</callId>"
    )
}

fn end() -> String {
    "<ended/><partlist alt=\"\" count=\"0\"></partlist><callEventType>callEnded</callEventType><callId>huddle-1</callId>"
        .to_owned()
}

/// What holds the fake server's banners back until it opens.
#[derive(Default)]
struct Gate {
    open: AtomicBool,
    waiting: Mutex<Vec<Waker>>,
}

impl Gate {
    fn open(&self) {
        self.open.store(true, Ordering::SeqCst);
        for waker in self.waiting.lock().unwrap().drain(..) {
            waker.wake();
        }
    }
}

/// The fake server, its banners shown only once the gate opens.
struct Gated(Arc<FakeServer>, Arc<Gate>);

impl NotificationServer for Gated {
    fn show(&self, notice: &Notice, replaces: u32) -> BoxFuture<'static, Result<u32, String>> {
        let shown = self.0.show(notice, replaces);
        let gate = Arc::clone(&self.1);
        Box::pin(async move {
            std::future::poll_fn(|cx| {
                if gate.open.load(Ordering::SeqCst) {
                    Poll::Ready(())
                } else {
                    gate.waiting.lock().unwrap().push(cx.waker().clone());
                    Poll::Pending
                }
            })
            .await;
            shown.await
        })
    }

    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        self.0.close(id)
    }
}

/// The account's calendar event of a meeting running now, in `chat` or `channel`.
fn running_meeting(chat: Option<&str>, channel: Option<&str>) -> CalendarEvent {
    let now = now_ms();
    CalendarEvent {
        id: "made-up-event".to_owned(),
        subject: "Weekly sync".to_owned(),
        start: EventTime::Instant(now - 5 * 60_000),
        end: EventTime::Instant(now + 25 * 60_000),
        all_day: false,
        kind: EventKind::Single,
        cancelled: false,
        online: true,
        join_url: None,
        meeting_chat: chat.map(str::to_owned),
        meeting: Some(MeetingInfo {
            organizer_id: "00000000-0000-4000-8000-00000000000b".to_owned(),
            tenant_id: "00000000-0000-4000-8000-0000000000d0".to_owned(),
        }),
        meeting_code: None,
        location: None,
        organizer_name: None,
        organizer_address: None,
        is_organizer: false,
        response: EventResponse::Accepted,
        show_as: ShowAs::Busy,
        private: false,
        preview: None,
        conference_id: None,
        attendees: Vec::new(),
        series_master_id: None,
        channel_meeting: channel.map(|channel| ChannelEventRef {
            channel: channel.to_owned(),
            organizer: None,
            post: Some("scheduled-post".to_owned()),
            ical_uid: "made-up-uid".to_owned(),
        }),
    }
}

impl Alerting {
    fn start(settings: Settings) -> Self {
        Self::start_over(settings, None, Vec::new())
    }

    /// The alert over a store that holds `stored` already, its banners held back by `gate`.
    fn start_over(settings: Settings, gate: Option<Arc<Gate>>, stored: Vec<Message>) -> Self {
        let rig = Rig::start_with(|cx| Models::with_calls(None, cx));
        rig.core.conversations.lock().unwrap().extend([
            chat(AB, ConversationKind::OneToOne, None),
            chat(TRIO, ConversationKind::Group, Some("Lab trio")),
            chat(MEETING, ConversationKind::Group, Some("Weekly sync chat")),
            chat(CHANNEL, ConversationKind::Channel, Some("General")),
        ]);
        rig.core.members.lock().unwrap().insert(
            TRIO.to_owned(),
            vec![Member::new(LAB_A, None), Member::new(LAB_B, None)],
        );
        rig.core
            .names
            .lock()
            .unwrap()
            .insert(LAB_B.to_owned(), "Bee".to_owned());
        for message in stored {
            store(&rig, message);
        }
        let written = Written::default();
        let trace = Arc::new(ViewTrace::new(written.clone()));
        let server = Arc::new(FakeServer::default());
        let core = rig.cx.read(|cx| rig.bridge.read(cx).core());
        let settings = rig.cx.update(|cx| cx.new(|_| SettingsModel::in_memory(settings)));
        let models = rig.models.clone();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let given: Arc<dyn NotificationServer> = match gate {
            Some(gate) => Arc::new(Gated(Arc::clone(&server), gate)),
            None => server.clone(),
        };
        let recorded = Arc::clone(&asked);
        let alert = rig.cx.update(|cx| {
            let alert = cx.new(|cx| {
                let mut alert = MeetingAlert::new(core, &models, settings, trace, cx);
                alert.set_server(given);
                alert
            });
            cx.subscribe(&alert, move |_, event, _| recorded.lock().unwrap().push(event.clone()))
                .detach();
            alert
        });
        rig.cx.run_until_parked();
        Self {
            rig,
            alert,
            server,
            asked,
            written,
        }
    }

    fn arrive(&self, message: Message, source: Source) {
        self.arrive_together(vec![message], source);
    }

    /// `messages` stored and told in this order, before anything they set off runs.
    fn arrive_together(&self, messages: Vec<Message>, source: Source) {
        for message in messages {
            store(&self.rig, message.clone());
            self.rig
                .events
                .send(Event::Message {
                    conversation: message.conversation_id.clone(),
                    id: message.id.clone(),
                    change: MessageChange::New,
                    applied: Applied::Inserted,
                    message: Some(Box::new(message)),
                    source,
                })
                .unwrap();
        }
        self.rig.cx.run_until_parked();
    }

    fn activate(&self, id: u32, action: &str) -> bool {
        let taken = self
            .rig
            .cx
            .update(|cx| self.alert.update(cx, |alert, cx| alert.activated(id, action, cx)));
        self.rig.cx.run_until_parked();
        taken
    }

    fn joinable(&self, chat: &str) -> bool {
        self.rig.cx.read(|cx| self.rig.models.calls.read(cx).joinable(chat))
    }

    fn calendar(&self, event: CalendarEvent) {
        self.rig.core.calendar.lock().unwrap().events.push(event);
    }
}

fn store(rig: &Rig, message: Message) {
    rig.core
        .messages
        .lock()
        .unwrap()
        .entry(message.conversation_id.clone())
        .or_default()
        .push(message);
}

#[test]
fn someone_else_s_meet_now_in_a_group_chat_shows_a_banner_whose_join_joins_it() {
    let a = Alerting::start(Settings::default());
    a.arrive(call_event(TRIO, "1", LAB_B, huddle(LAB_B)), Source::Delivery);
    assert_eq!(
        a.server.shown(),
        [Notice {
            conversation: TRIO.to_owned(),
            kind: NoticeKind::Meeting,
            summary: "Lab trio".to_owned(),
            body: "Bee started a meeting".to_owned(),
            picture: None,
        }]
    );
    // the chat's header offers Join while it runs: the engine joins where its start says
    assert!(a.joinable(TRIO));
    assert!(!a.activate(99, JOIN_ACTION), "not its banner");
    assert!(a.activate(1, JOIN_ACTION));
    assert_eq!(
        *a.asked.lock().unwrap(),
        [MeetingAlertEvent::Join(MeetingJoin::GroupChat {
            chat: TRIO.to_owned()
        })]
    );
    assert_eq!(a.server.closed(), [1]);
    // the trace says which chat and what kind, never a name or a title
    let text = a.written.text();
    assert!(
        text.contains("meeting-notification") && text.contains("group-chat"),
        "{text}"
    );
    assert!(!text.contains("Bee") && !text.contains("Lab trio"), "{text}");
    // shown once: the same start again shows nothing
    a.arrive(call_event(TRIO, "1", LAB_B, huddle(LAB_B)), Source::Replay);
    assert_eq!(a.server.shown().len(), 1);
}

#[test]
fn a_meeting_s_banner_shows_its_starter_s_stored_picture() {
    let a = Alerting::start(Settings::default());
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(120, 120, image::Rgba([40, 160, 90, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    a.rig.core.people.lock().unwrap().insert(
        LAB_B.to_owned(),
        teams_core::Person {
            mri: LAB_B.to_owned(),
            name: Some("Bee".to_owned()),
            from: Some(teams_core::NameFrom::Graph),
            picture: Some(teams_store::StoredPicture {
                media_type: "image/png".to_owned(),
                bytes: png,
            }),
            looking: false,
        },
    );
    a.arrive(call_event(TRIO, "1", LAB_B, huddle(LAB_B)), Source::Delivery);
    let shown = a.server.shown();
    assert_eq!(shown[0].body, "Bee started a meeting");
    let picture = shown[0].picture.as_ref().expect("the starter's picture");
    assert_eq!(
        (picture.width(), picture.height(), picture.shape()),
        (96, 96, teams_ui::notify::PictureShape::Circle)
    );
}

#[test]
fn the_call_s_end_closes_its_banner_and_the_chat_s_join() {
    let a = Alerting::start(Settings::default());
    a.arrive(call_event(TRIO, "1", LAB_B, huddle(LAB_B)), Source::Delivery);
    assert_eq!(a.server.shown().len(), 1);
    a.arrive(call_event(TRIO, "2", LAB_B, end()), Source::Delivery);
    assert_eq!(a.server.closed(), [1]);
    assert!(!a.joinable(TRIO));
    assert!(!a.activate(1, JOIN_ACTION), "gone with its call");
}

#[test]
fn the_account_s_own_start_a_group_call_a_one_to_one_call_and_history_show_none() {
    let a = Alerting::start(Settings::default());
    a.arrive(call_event(TRIO, "1", LAB_A, huddle(LAB_A)), Source::Delivery);
    // a group call rings its members instead; its chat's header offers its Join all the same
    a.arrive(call_event(TRIO, "2", LAB_B, start()), Source::Delivery);
    assert!(a.joinable(TRIO));
    a.arrive(call_event(AB, "3", LAB_B, start()), Source::Delivery);
    a.arrive(call_event(TRIO, "4", LAB_B, huddle(LAB_B)), Source::History);
    let mut stale = call_event(TRIO, "5", LAB_B, huddle(LAB_B));
    stale.arrival_time -= 10 * 60_000;
    a.arrive(stale, Source::CatchUp);
    assert_eq!(a.server.shown(), []);
}

#[test]
fn the_switch_notifications_off_and_do_not_disturb_show_none() {
    for settings in [
        Settings {
            meeting_started: false,
            ..Settings::default()
        },
        Settings {
            notifications: Notifications::Off,
            ..Settings::default()
        },
        Settings {
            manual_status: ManualStatus::DoNotDisturb,
            ..Settings::default()
        },
    ] {
        let a = Alerting::start(settings);
        a.arrive(call_event(TRIO, "1", LAB_B, huddle(LAB_B)), Source::Delivery);
        assert_eq!(a.server.shown(), []);
        assert!(a.joinable(TRIO), "the header's Join doesn't hang on the banner");
    }
}

#[test]
fn a_meeting_s_chat_shows_a_banner_only_while_the_calendar_holds_its_meeting() {
    let a = Alerting::start(Settings::default());
    a.arrive(call_event(MEETING, "1", LAB_B, start()), Source::Delivery);
    assert_eq!(a.server.shown(), [], "not on the calendar");
    a.calendar(running_meeting(Some(MEETING), None));
    a.arrive(call_event(MEETING, "2", LAB_B, start()), Source::Delivery);
    assert_eq!(
        a.server.shown(),
        [Notice {
            conversation: MEETING.to_owned(),
            kind: NoticeKind::Meeting,
            summary: "Weekly sync".to_owned(),
            body: "Bee started the meeting".to_owned(),
            picture: None,
        }]
    );
    assert!(a.activate(1, DEFAULT_ACTION));
    assert_eq!(
        *a.asked.lock().unwrap(),
        [MeetingAlertEvent::Open {
            conversation: MEETING.to_owned(),
            channel: false,
        }]
    );
    a.arrive(call_event(MEETING, "3", LAB_B, start()), Source::Delivery);
    assert!(a.activate(2, JOIN_ACTION));
    assert_eq!(
        a.asked.lock().unwrap()[1],
        MeetingAlertEvent::Join(MeetingJoin::MeetingChat {
            chat: MEETING.to_owned(),
            title: "Weekly sync".to_owned(),
        })
    );
}

#[test]
fn a_channel_s_scheduled_meeting_shows_a_banner_unless_the_channel_is_muted() {
    let a = Alerting::start(Settings::default());
    a.calendar(running_meeting(None, Some(CHANNEL)));
    a.rig
        .core
        .roots
        .lock()
        .unwrap()
        .insert(CHANNEL.to_owned(), vec![("1".to_owned(), "scheduled-post".to_owned())]);
    a.arrive(call_event(CHANNEL, "1", LAB_B, start()), Source::Delivery);
    assert_eq!(a.server.shown().len(), 1);
    assert_eq!(a.server.shown()[0].body, "Bee started the meeting");
    assert!(a.activate(1, JOIN_ACTION));
    assert!(matches!(
        &a.asked.lock().unwrap()[0],
        MeetingAlertEvent::Join(MeetingJoin::Channel { channel, meeting: Some(_), .. }) if channel == CHANNEL
    ));

    let muted = Alerting::start(Settings::default());
    muted.calendar(running_meeting(None, Some(CHANNEL)));
    muted
        .rig
        .core
        .roots
        .lock()
        .unwrap()
        .insert(CHANNEL.to_owned(), vec![("1".to_owned(), "scheduled-post".to_owned())]);
    muted.rig.core.channel_data.lock().unwrap().insert(
        format!("preferences:{CHANNEL}"),
        teams_proto::ChannelData(serde_json::json!({"id": CHANNEL, "properties": {"alerts": "false"}})),
    );
    muted.arrive(call_event(CHANNEL, "1", LAB_B, start()), Source::Delivery);
    assert_eq!(muted.server.shown(), []);
}

#[test]
fn an_end_a_catch_up_brings_late_still_closes_the_banner_and_the_chat_s_join() {
    let a = Alerting::start(Settings::default());
    // delivered live; the push channel then went down for longer than a banner stays news
    let mut start = call_event(TRIO, "1", LAB_B, huddle(LAB_B));
    start.arrival_time -= 12 * 60_000;
    a.arrive(start, Source::Delivery);
    assert!(a.joinable(TRIO));
    let mut late = call_event(TRIO, "2", LAB_B, end());
    late.arrival_time -= 10 * 60_000;
    a.arrive(late, Source::CatchUp);
    assert_eq!(a.server.closed(), [1]);
    assert!(!a.joinable(TRIO));
}

#[test]
fn a_call_that_ends_while_its_banner_is_shown_has_it_withdrawn() {
    let gate = Arc::new(Gate::default());
    let a = Alerting::start_over(Settings::default(), Some(Arc::clone(&gate)), Vec::new());
    a.arrive(call_event(TRIO, "1", LAB_B, huddle(LAB_B)), Source::Delivery);
    assert_eq!(a.server.shown().len(), 1, "being shown");
    a.arrive(call_event(TRIO, "2", LAB_B, end()), Source::Delivery);
    gate.open();
    a.rig.cx.run_until_parked();
    assert_eq!(a.server.closed(), [1]);
    assert!(!a.activate(1, JOIN_ACTION), "never kept");
    assert!(!a.joinable(TRIO));
}

#[test]
fn a_start_and_its_end_told_together_show_no_banner_whichever_comes_first() {
    let a = Alerting::start(Settings::default());
    // the end told first, as a catch-up's page can
    let start = call_event(TRIO, "1", LAB_B, huddle(LAB_B));
    let mut end = call_event(TRIO, "2", LAB_B, end());
    end.arrival_time += 1;
    a.arrive_together(vec![end.clone(), start.clone()], Source::CatchUp);
    assert_eq!(a.server.shown(), []);
    assert!(!a.joinable(TRIO));

    // the start first: the end comes while the banner's reads run
    let b = Alerting::start(Settings::default());
    b.arrive_together(vec![start, end], Source::Delivery);
    assert_eq!(b.server.shown(), []);
    assert!(!b.joinable(TRIO));
}

#[test]
fn the_chat_s_header_offers_join_for_a_meet_now_that_ran_before_a_restart_or_came_late() {
    let a = Alerting::start_over(
        Settings::default(),
        None,
        vec![call_event(TRIO, "1", LAB_B, huddle(LAB_B))],
    );
    assert!(a.joinable(TRIO), "read from the store as it starts");
    assert_eq!(a.server.shown(), [], "no banner for what the store held");

    let b = Alerting::start(Settings::default());
    let mut late = call_event(TRIO, "1", LAB_B, huddle(LAB_B));
    late.arrival_time -= 10 * 60_000;
    b.arrive(late, Source::CatchUp);
    assert!(b.joinable(TRIO), "a late start still runs");
    assert_eq!(b.server.shown(), [], "too late for a banner");
}

/// An end of the chat's call `call`; `None`: one naming no call, as the oldest shape's does.
fn end_of(call: Option<&str>) -> String {
    let call = call.map(|call| format!("<callId>{call}</callId>")).unwrap_or_default();
    format!("<ended/><partlist alt=\"\" count=\"0\"></partlist><callEventType>callEnded</callEventType>{call}")
}

#[test]
fn an_earlier_call_s_late_end_leaves_the_running_call_s_banner_and_an_end_naming_no_call_closes_it() {
    let a = Alerting::start(Settings::default());
    a.arrive(call_event(TRIO, "1", LAB_B, huddle(LAB_B)), Source::Delivery);
    assert_eq!(a.server.shown().len(), 1);
    // an earlier Meet now's end, missed while the push channel was down, comes with the catch-up
    let mut earlier = call_event(TRIO, "0", LAB_B, end_of(Some("huddle-0")));
    earlier.arrival_time -= 10 * 60_000;
    a.arrive(earlier, Source::CatchUp);
    assert!(a.server.closed().is_empty(), "another call's end");
    assert!(a.joinable(TRIO));
    a.arrive(call_event(TRIO, "2", LAB_B, end_of(None)), Source::Delivery);
    assert_eq!(a.server.closed(), [1]);
    assert!(!a.joinable(TRIO));
}

#[test]
fn an_end_naming_no_call_while_a_banner_is_shown_has_it_withdrawn() {
    let gate = Arc::new(Gate::default());
    let a = Alerting::start_over(Settings::default(), Some(Arc::clone(&gate)), Vec::new());
    a.arrive(call_event(TRIO, "1", LAB_B, huddle(LAB_B)), Source::Delivery);
    assert_eq!(a.server.shown().len(), 1, "being shown");
    a.arrive(call_event(TRIO, "2", LAB_B, end_of(None)), Source::Delivery);
    gate.open();
    a.rig.cx.run_until_parked();
    assert_eq!(a.server.closed(), [1]);
    assert!(!a.activate(1, JOIN_ACTION), "never kept");
}

#[test]
fn a_start_whose_end_the_store_held_before_a_restart_shows_no_banner() {
    // the end came first, then the app restarted before the start did
    let start = call_event(TRIO, "1", LAB_B, huddle(LAB_B));
    let mut end = call_event(TRIO, "2", LAB_B, end());
    end.arrival_time += 1;
    let a = Alerting::start_over(Settings::default(), None, vec![end]);
    a.arrive(start, Source::CatchUp);
    assert_eq!(a.server.shown(), []);
    assert!(!a.joinable(TRIO));
}

#[test]
fn a_channel_meet_now_does_not_borrow_a_scheduled_meetings_banner() {
    let a = Alerting::start(Settings::default());
    a.calendar(running_meeting(Some(CHANNEL), Some(CHANNEL)));
    a.rig
        .core
        .roots
        .lock()
        .unwrap()
        .insert(CHANNEL.to_owned(), vec![("1".to_owned(), "meet-now-post".to_owned())]);
    a.arrive(call_event(CHANNEL, "1", LAB_B, start()), Source::Delivery);
    assert_eq!(a.server.shown(), [], "another post's call");
    a.arrive(call_event(CHANNEL, "2", LAB_B, start()), Source::Delivery);
    assert_eq!(a.server.shown(), [], "a start with no recorded reply root");
    a.rig
        .core
        .roots
        .lock()
        .unwrap()
        .insert(CHANNEL.to_owned(), vec![("3".to_owned(), "scheduled-post".to_owned())]);
    a.arrive(call_event(CHANNEL, "3", LAB_B, start()), Source::Delivery);
    assert_eq!(a.server.shown().len(), 1, "the scheduled post's own call");
    assert_eq!(a.server.shown()[0].summary, "Weekly sync");
}
