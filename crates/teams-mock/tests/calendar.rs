//! The synthetic calendar peer's routes and Scheduler, sent straight to its loopback HTTP server. These tests
//! exercise the mock's model, including rejected authentication and malformed windows; nothing reaches a tenant.

#![cfg(test)]

use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::{Client, Method, Response};
use serde_json::{Value, json};
use teams_mock::{ChatServiceMock, LAB_A};
use teams_proto::{CalendarView, EventRead, MeetingTimes, Read as _, Schedules};

const BASE: &str = "/api/mt/part/amer-03";
const VIEW: &str = "v2.0/me/calendars/default/calendarView";
const DAY: i64 = 86_400_000;

async fn request(mock: &ChatServiceMock, method: Method, endpoint: &str, body: Value) -> Response {
    Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .request(method, format!("{}{BASE}/{endpoint}", mock.origin()))
        .bearer_auth(mock.token(LAB_A).unwrap())
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(&body).unwrap())
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn calendar_views_full_events_and_series_masters_describe_the_same_synthetic_week() {
    let mock = ChatServiceMock::start().await.unwrap();
    let now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()).unwrap();
    let from = now.div_euclid(DAY) * DAY;
    let view = CalendarView::between(from, from + 7 * DAY).unwrap();
    let query = view
        .endpoint()
        .query()
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    let response = request(&mock, Method::GET, &format!("{VIEW}?{query}"), Value::Null).await;
    assert_eq!(response.status(), 200);
    let body = response.bytes().await.unwrap();
    let events = CalendarView::read_answer(&body).unwrap().events;
    assert_eq!(events.len(), 15);
    assert!(events.iter().any(|event| event.cancelled));
    assert!(events.iter().any(|event| event.all_day));
    assert!(events.iter().any(|event| event.online));
    let event = &events[0];
    let response = request(
        &mock,
        Method::GET,
        &format!("v2.0/me/calendars/events/{}", event.id),
        Value::Null,
    )
    .await;
    let full = EventRead::new(&event.id)
        .unwrap()
        .read_answer(&response.bytes().await.unwrap())
        .unwrap();
    assert_eq!(full.event.id, event.id);
    assert_eq!(full.body_html.as_deref(), Some("<div>Made-up notes</div>"));
    assert_eq!(full.event.reminder, teams_proto::EventReminder::Off);
    let series = events.iter().find_map(|event| event.series_master_id.as_ref()).unwrap();
    let response = request(
        &mock,
        Method::GET,
        &format!("v2.0/me/calendars/events/{series}"),
        Value::Null,
    )
    .await;
    let master = EventRead::new(series)
        .unwrap()
        .read_answer(&response.bytes().await.unwrap())
        .unwrap();
    assert_eq!(master.event.kind, teams_proto::EventKind::RecurringMaster);
    assert!(master.recurrence.is_some());
    assert!(master.event.series_master_id.is_none());
}

#[tokio::test]
async fn calendar_routes_require_the_mock_token_a_readable_window_and_a_known_event() {
    let mock = ChatServiceMock::start().await.unwrap();
    let unauthenticated = Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("{}{BASE}/{VIEW}", mock.origin()))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), 401);
    for endpoint in [
        VIEW.to_owned(),
        format!("{VIEW}?StartDate=not-a-time&EndDate=not-a-time&shouldDecryptData=true"),
        format!("{VIEW}?StartDate=2026-11-02T00:00:00Z&EndDate=2026-11-01T00:00:00Z&shouldDecryptData=true"),
        "v2.0/me/calendars/events/no-such-event".to_owned(),
        "v2.0/me/calendars/events/AAMkMadeUpSeriesUnknown".to_owned(),
    ] {
        assert_eq!(
            request(&mock, Method::GET, &endpoint, Value::Null).await.status(),
            404,
            "{endpoint}"
        );
    }
}

fn schedule_ask() -> Value {
    json!({
        "startTime": "2026-11-02T00:00:00.000Z", "numberOfDays": 5,
        "meetingDurationInMinutes": 30,
        "attendees": [
            {"address": "b@synthetic.example", "type": "Required"},
            {"address": "c@synthetic.example", "type": "Optional"},
            {"address": "a@synthetic.example", "type": "Organizer"},
            {"type": "Required"},
        ],
    })
}

#[tokio::test]
async fn the_scheduler_returns_each_person_s_busy_times_and_avoids_attendee_conflicts() {
    let mock = ChatServiceMock::start().await.unwrap();
    let response = request(&mock, Method::PUT, "v2.0/me/calendars/schedules", schedule_ask()).await;
    assert_eq!(response.status(), 200);
    let schedules = Schedules::read_answer(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(schedules.len(), 3, "an attendee without an address is ignored");
    assert_eq!(
        schedules[0].busy.len(),
        10,
        "the first attendee has a busy hour and tentative half hour per day"
    );
    assert_eq!(schedules[1].busy.len(), 5);
    assert!(
        !schedules[2].busy.is_empty(),
        "the organizer's real synthetic events are used"
    );
    let response = request(&mock, Method::PUT, "v2.0/me/calendars/findMeetingTimes", schedule_ask()).await;
    let suggestions = MeetingTimes::read_answer(&response.bytes().await.unwrap()).unwrap();
    assert!(!suggestions.is_empty());
    for at in suggestions {
        assert!(
            schedules[..2]
                .iter()
                .flat_map(|person| &person.busy)
                .all(|busy| at + 1_800_000 <= busy.start_ms || at >= busy.end_ms)
        );
    }
    let response = request(&mock, Method::PUT, "v2.0/me/calendars/schedules", json!({})).await;
    assert!(
        Schedules::read_answer(&response.bytes().await.unwrap())
            .unwrap()
            .is_empty()
    );
    let response = request(&mock, Method::PUT, "v2.0/me/calendars/findMeetingTimes", json!({})).await;
    assert_eq!(
        MeetingTimes::read_answer(&response.bytes().await.unwrap())
            .unwrap()
            .len(),
        18,
        "the default is one working day"
    );
}

#[tokio::test]
async fn the_mock_answers_the_existing_scheduling_form_endpoints_without_storing_an_event() {
    let mock = ChatServiceMock::start().await.unwrap();
    let event = json!({"subject": "Made-up appointment", "startTime": "2026-11-02T08:00:00Z", "endTime": "2026-11-02T08:30:00Z"});
    let response = request(
        &mock,
        Method::POST,
        "v2.0/me/calendars/default/events",
        json!({"calendarEvent": event}),
    )
    .await;
    assert_eq!(response.status(), 201);
    let created: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(created["value"][0]["objectId"], "AAMkMadeUpCreated");
    assert_eq!(created["value"][0]["subject"], "Made-up appointment");
    for (method, endpoint) in [
        (Method::PUT, "v2.0/me/calendars/events/AAMkMadeUpCreated"),
        (Method::PUT, "v2.0/me/calendars/events/AAMkMadeUpCreated/cancel"),
        (Method::DELETE, "v2.0/me/calendars/events/AAMkMadeUpCreated"),
    ] {
        assert_eq!(request(&mock, method, endpoint, json!({})).await.status(), 204);
    }
    let response = request(
        &mock,
        Method::POST,
        "beta/me/calendarEvents/privateMeeting/schedulingService/create",
        json!({"isStreamEnabled": false, "meetingType": "Scheduled"}),
    )
    .await;
    let meeting: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert!(
        meeting["value"]["links"]["join"]
            .as_str()
            .unwrap()
            .starts_with("https://teams.microsoft.com/l/meetup-join/")
    );
    let response = request(
        &mock,
        Method::GET,
        "beta/me/calendarEvents/timeZoneSettingsWithOffset",
        Value::Null,
    )
    .await;
    let zone: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(zone["local"]["timeZone"], "UTC");
}
