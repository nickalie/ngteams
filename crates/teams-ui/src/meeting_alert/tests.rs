//! Which call events give a banner and which don't, on made-up ids.

use super::*;
use chrono::NaiveDate;
use teams_core::{ChannelEventRef, EventKind, EventResponse, EventTime, FloatingTime, ShowAs};

const ME: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const BEE: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const GROUP: &str = "19:00000000000000000000000000000001@thread.v2";
const MEETING: &str = "19:meeting_MadeUpMeeting01@thread.v2";
const CHANNEL: &str = "19:0123456789abcdef0123456789abcdef@thread.tacv2";
const ONE_TO_ONE: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

fn message(conversation: &str, sender: &str, content: &str) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: "1700000000001".to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: "Event/Call".to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: 1_700_000_000_001,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

fn huddle_start(starter: &str) -> String {
    format!(
        "<partlist type=\"started\" alt=\"\"><part identity=\"{starter}\"><name>{starter}</name></part></partlist>\
         <conversationType>\"huddleGroupCall\"</conversationType><callEventType>callStarted</callEventType>"
    )
}

const START: &str = "<partlist alt =\"\"></partlist><callEventType>callStarted</callEventType>";
const END: &str = "<ended/><partlist alt=\"\" count=\"0\"></partlist><callEventType>callEnded</callEventType>";

#[test]
fn a_call_s_start_tells_where_and_who_and_an_end_tells_it_ended() {
    assert_eq!(
        call_news(GROUP, &message(GROUP, BEE, &huddle_start(BEE))),
        Some(CallNews::Started {
            place: StartedIn::GroupChat { huddle: true },
            starter: Some(BEE.to_owned()),
        })
    );
    // a start naming nobody: its sender started it
    assert_eq!(
        call_news(MEETING, &message(MEETING, BEE, START)),
        Some(CallNews::Started {
            place: StartedIn::MeetingChat,
            starter: Some(BEE.to_owned()),
        })
    );
    assert!(matches!(
        call_news(CHANNEL, &message(CHANNEL, BEE, START)),
        Some(CallNews::Started {
            place: StartedIn::Channel,
            ..
        })
    ));
    assert!(matches!(
        call_news(GROUP, &message(GROUP, BEE, START)),
        Some(CallNews::Started {
            place: StartedIn::GroupChat { huddle: false },
            ..
        })
    ));
    assert_eq!(call_news(GROUP, &message(GROUP, BEE, END)), Some(CallNews::Ended));
    // a 1:1 call rings; a text message is no call's
    assert_eq!(call_news(ONE_TO_ONE, &message(ONE_TO_ONE, BEE, START)), None);
    let mut text = message(GROUP, BEE, "<p>hi</p>");
    text.message_type = "RichText/Html".to_owned();
    assert_eq!(call_news(GROUP, &text), None);
}

#[test]
fn only_someone_else_s_meeting_shows_a_banner_and_a_group_call_rings_instead() {
    assert!(may_show(ME, StartedIn::GroupChat { huddle: true }, Some(BEE)));
    assert!(may_show(ME, StartedIn::MeetingChat, Some(BEE)));
    assert!(may_show(ME, StartedIn::Channel, None));
    assert!(
        !may_show(ME, StartedIn::GroupChat { huddle: true }, Some(ME)),
        "own start"
    );
    assert!(!may_show(ME, StartedIn::MeetingChat, Some(ME)), "own start");
    assert!(
        !may_show(ME, StartedIn::GroupChat { huddle: false }, Some(BEE)),
        "rings"
    );
}

#[test]
fn a_start_is_news_when_delivered_or_caught_up_soon_never_from_history() {
    let now = 1_700_000_600_000;
    assert!(fresh(Source::Delivery, 0, now));
    assert!(fresh(Source::CatchUp, now - FRESH_MS, now));
    assert!(fresh(Source::Replay, now - 1_000, now));
    assert!(!fresh(Source::CatchUp, now - FRESH_MS - 1, now));
    assert!(!fresh(Source::History, now, now));
}

#[test]
fn the_switch_notifications_do_not_disturb_and_a_call_s_mute_hold_banners_back() {
    let on = Settings::default();
    assert!(on.meeting_started, "on by default, as Teams' is");
    assert!(banners_on(&on, ME, false));
    assert!(!banners_on(&on, ME, true), "a call's Mute notifications");
    let off = Settings {
        meeting_started: false,
        ..Settings::default()
    };
    assert!(!banners_on(&off, ME, false));
    let silent = Settings {
        notifications: Notifications::Off,
        ..Settings::default()
    };
    assert!(!banners_on(&silent, ME, false));
    let mentions = Settings {
        notifications: Notifications::Mentions,
        ..Settings::default()
    };
    assert!(banners_on(&mentions, ME, false), "a meeting's start is no message");
    let dnd = Settings {
        manual_status: ManualStatus::DoNotDisturb,
        ..Settings::default()
    };
    assert!(!banners_on(&dnd, ME, false));
}

fn at(hour: u8, minute: u8) -> EventTime {
    EventTime::Floating(FloatingTime {
        year: 2026,
        month: 11,
        day: 13,
        hour,
        minute,
        second: 0,
    })
}

fn local(hour: u32, minute: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 11, 13)
        .unwrap()
        .and_hms_opt(hour, minute, 0)
        .unwrap()
}

fn scheduled(chat: Option<&str>, channel: Option<&str>) -> CalendarEvent {
    CalendarEvent {
        id: "made-up".to_owned(),
        subject: "Weekly sync".to_owned(),
        start: at(10, 0),
        end: at(10, 30),
        all_day: false,
        kind: EventKind::Single,
        cancelled: false,
        online: true,
        join_url: None,
        meeting_chat: chat.map(str::to_owned),
        meeting: None,
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
            ical_uid: "made-up".to_owned(),
        }),
    }
}

#[test]
fn a_meeting_shows_its_banner_only_while_the_calendar_holds_it_about_now() {
    let events = [scheduled(Some(MEETING), None), scheduled(None, Some(CHANNEL))];
    assert!(scheduled_now(&events, MEETING, local(10, 2)).is_some());
    assert!(scheduled_now(&events, CHANNEL, local(10, 2)).is_some());
    // started early, or running late
    assert!(scheduled_now(&events, MEETING, local(9, 45)).is_some());
    assert!(scheduled_now(&events, MEETING, local(10, 45)).is_some());
    assert!(scheduled_now(&events, MEETING, local(9, 44)).is_none());
    assert!(scheduled_now(&events, MEETING, local(10, 46)).is_none());
    // a meeting not on the calendar (a channel's ad-hoc Meet now), or one cancelled
    assert!(scheduled_now(&events, "19:meeting_Other@thread.v2", local(10, 2)).is_none());
    let mut cancelled = scheduled(Some(MEETING), None);
    cancelled.cancelled = true;
    assert!(scheduled_now(&[cancelled], MEETING, local(10, 2)).is_none());
}
