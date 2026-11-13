//! A started meeting's banner on GPUI's test context, over the fake engine (`common`) and a fake notification server
//! that records what it is asked: someone else's Meet now in a group chat, and a meeting's chat's or a channel's
//! meeting the calendar holds about now, show one with Join; the account's own start, a group call (it rings), a 1:1
//! call, history, a meeting not on the calendar, a muted channel, the switch off, notifications off and Do not disturb
//! show none; its Join and its body ask the desktop to go in or to open the chat; the call's end closes it. Every id
//! and name is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;
#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
#[path = "common/notices.rs"]
mod notices;

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use common::{LAB_A, LAB_B, Rig, Written};
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
use teams_ui::notify::{DEFAULT_ACTION, JOIN_ACTION, Notice, NoticeKind};
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
            post: None,
            ical_uid: "made-up-uid".to_owned(),
        }),
    }
}

impl Alerting {
    fn start(settings: Settings) -> Self {
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
        let written = Written::default();
        let trace = Arc::new(ViewTrace::new(written.clone()));
        let server = Arc::new(FakeServer::default());
        let core = rig.cx.read(|cx| rig.bridge.read(cx).core());
        let settings = rig.cx.update(|cx| cx.new(|_| SettingsModel::in_memory(settings)));
        let models = rig.models.clone();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (given, recorded) = (Arc::clone(&server), Arc::clone(&asked));
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
        Self {
            rig,
            alert,
            server,
            asked,
            written,
        }
    }

    fn arrive(&self, message: Message, source: Source) {
        self.rig.send(Event::Message {
            conversation: message.conversation_id.clone(),
            id: message.id.clone(),
            change: MessageChange::New,
            applied: Applied::Inserted,
            message: Some(Box::new(message)),
            source,
        });
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
    muted.rig.core.channel_data.lock().unwrap().insert(
        format!("preferences:{CHANNEL}"),
        teams_proto::ChannelData(serde_json::json!({"id": CHANNEL, "properties": {"alerts": "false"}})),
    );
    muted.arrive(call_event(CHANNEL, "1", LAB_B, start()), Source::Delivery);
    assert_eq!(muted.server.shown(), []);
}
