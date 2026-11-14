//! Synthetic own-origin calling and meeting metadata; no network or account data.
#![cfg(test)]

use serde_json::{Value, json};
use teams_proto::{
    CalendarEvent, CalendarView, ChannelEventRead, ChannelMeetup, Cloud, CreateOnlineMeeting, CsaTeams, EventRead,
    MeetNow, MeetingCode, MeetingJoinInfo, MeetingThread, join_url_chat_in, join_url_meeting_in, running_call_in,
};

const ORIGIN: &str = "https://calls.example:8443";
const THREAD: &str = "19:meeting_SyntheticOwn@thread.v2";
const ORGANIZER: &str = "00000000-0000-4000-8000-00000000000a";
const TENANT: &str = "00000000-0000-4000-8000-00000000000f";

fn join_url(origin: &str) -> String {
    format!(
        "{origin}/l/meetup-join/19%3Ameeting_SyntheticOwn%40thread.v2/0?context=%7B%22Oid%22%3A%22{ORGANIZER}%22%2C%22Tid%22%3A%22{TENANT}%22%7D"
    )
}

fn event(origin: &str) -> Value {
    json!({
        "objectId": "own-event", "iCalUID": "aabbccdd00112233", "startTime": "2026-11-14T09:00:00Z",
        "endTime": "2026-11-14T10:00:00Z", "skypeTeamsMeetingUrl": join_url(origin),
        "shortOnlineMeetingJoinUrl": format!("{origin}/meet/123456789012?p=MadeUp"),
    })
}

#[test]
fn own_calendar_meetings_are_read_from_live_and_kept_answers() {
    let cloud = Cloud::own(ORIGIN).unwrap();
    let raw = event(ORIGIN);
    let body = raw.to_string().into_bytes();
    let event = CalendarEvent::created_in(&cloud, &body).unwrap();
    assert_eq!(event.meeting_chat.as_deref(), Some(THREAD));
    assert_eq!(event.meeting.as_ref().unwrap().organizer_id, ORGANIZER);
    assert!(event.meeting_code.is_some());
    let (kept, entry) = CalendarEvent::created_kept_in(&cloud, &body).unwrap();
    assert_eq!(CalendarEvent::stored_in(&cloud, &entry), Some(kept));
    let read = CalendarView::read_answer_in(&cloud, &json!({"value": [raw]}).to_string().into_bytes()).unwrap();
    assert_eq!(read.events.as_slice(), std::slice::from_ref(&event));
    assert_eq!(
        EventRead::new("own-event")
            .unwrap()
            .read_answer_in(&cloud, &body)
            .unwrap()
            .event,
        event
    );
    assert_eq!(
        ChannelEventRead::new("team@example.invalid", "aabbccdd00112233")
            .unwrap()
            .read_answer_in(&cloud, &body)
            .unwrap()
            .event,
        event
    );
    let foreign = CalendarEvent::created_in(
        &cloud,
        &self::event("https://teams.microsoft.com").to_string().into_bytes(),
    )
    .unwrap();
    assert!(
        foreign.join_url.is_none()
            && foreign.meeting.is_none()
            && foreign.meeting_code.is_none()
            && foreign.meeting_chat.is_none()
    );
}

#[test]
fn own_join_links_and_meeting_creation_answers_refuse_other_origins() {
    let cloud = Cloud::own(ORIGIN).unwrap();
    assert_eq!(join_url_chat_in(&cloud, &join_url(ORIGIN)).as_deref(), Some(THREAD));
    assert_eq!(
        join_url_meeting_in(&cloud, &join_url(ORIGIN)).unwrap().tenant_id,
        TENANT
    );
    for origin in [ORIGIN, "https://teams.microsoft.com", "https://calls.example:9443"] {
        let expected = origin == ORIGIN;
        let short = format!("{origin}/meet/123456789012?p=MadeUp");
        assert_eq!(MeetingCode::of_link_in(&cloud, &short).is_some(), expected);
        assert_eq!(join_url_chat_in(&cloud, &join_url(origin)).is_some(), expected);
        assert_eq!(join_url_meeting_in(&cloud, &join_url(origin)).is_some(), expected);
        let meeting = json!({"organizerId": ORGANIZER, "tenantId": TENANT, "meetingJoinUrl": short});
        let thread = json!({"properties": {"meeting": meeting.to_string()}}).to_string();
        assert_eq!(
            MeetingThread::read_answer_in(&cloud, thread.as_bytes())
                .unwrap()
                .join_url
                .is_some(),
            expected
        );
        let made = json!({"value": {"groupContext": {"threadId": THREAD}, "links": {"join": join_url(origin), "shortJoinUrl": short}}}).to_string();
        assert_eq!(
            CreateOnlineMeeting::read_answer_in(&cloud, made.as_bytes()).is_some(),
            expected
        );
        assert_eq!(MeetNow::read_answer_in(&cloud, made.as_bytes()).is_some(), expected);
        let html = format!("<a id=\"meet_invite_block.action.join_link\" href=\"{short}\">Join</a>");
        let update = json!({"meetingDetails": {"invitation": html}});
        assert_eq!(MeetingJoinInfo::read_in(&cloud, &update).is_some(), expected);
    }
}

#[test]
fn own_channel_and_group_call_posts_keep_exact_conversation_links() {
    let cloud = Cloud::own(ORIGIN).unwrap();
    let controller = format!("{ORIGIN}/flightproxy/api/v2/ep/conversation.example/conv/SyntheticOwn?i=1&e=2");
    let content = format!(
        "<callEventType>callStarted</callEventType><callId>own</callId><conversationUrl>{}</conversationUrl>",
        controller.replace('&', "&amp;")
    );
    let post: teams_proto::chat::Message =
        serde_json::from_value(json!({"id": "1", "messagetype": "Event/Call", "content": content,
        "properties": {"meeting": json!({"organizerId": ORGANIZER, "tenantId": TENANT}).to_string()}}))
        .unwrap();
    assert_eq!(
        ChannelMeetup::of_posts_in(&cloud, &[post])
            .unwrap()
            .conversation
            .as_str(),
        controller
    );
    assert_eq!(
        running_call_in(&cloud, [("Event/Call", content.as_str())])
            .unwrap()
            .as_str(),
        controller
    );
    let csa = json!({"teams": [{"channels": [{"id": THREAD, "activeMeetups": [{"status": "Active", "messageId": "1", "conversationUrl": controller, "organizerId": ORGANIZER, "tenantId": TENANT}]}]}]}).to_string();
    assert_eq!(
        CsaTeams::meetup_for_cloud(&cloud, csa.as_bytes(), THREAD)
            .unwrap()
            .conversation
            .as_str(),
        controller
    );
    let foreign = content.replace(ORIGIN, "https://other.example");
    assert!(running_call_in(&cloud, [("Event/Call", foreign.as_str())]).is_none());
    assert!(
        CsaTeams::meetup_for_cloud(
            &cloud,
            csa.replace(ORIGIN, "https://teams.microsoft.com").as_bytes(),
            THREAD
        )
        .is_none()
    );
}
