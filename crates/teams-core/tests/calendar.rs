//! Calendar operations through a running engine, with synthetic middle-tier answers on loopback. Writes preserve
//! event fields and organizer identity; failed or uncertain outcomes are never blindly resent.

#![cfg(test)]

use std::future::{self, Future};
use std::sync::Arc;

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts};
use teams_core::{EngineError, EventDraft, EventShowAs, EventWrite, Organizer, Outbox, ZonedTime};
use teams_mock::{CHAT_BASE, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE};
use teams_proto::Mri;
use teams_proto::{Author, Client, Credentials, EventAttendee, HttpTransport};
use teams_store::{NameSource, PictureState, Profile, Store};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const PREFIX: &str = "/api/mt/part/amer-03";
const PROFILES: &str = "/api/mt/part/amer-03/beta/users/fetchShortProfile";
const ZONE: &str = "/api/mt/part/amer-03/beta/me/calendarEvents/timeZoneSettingsWithOffset";
const EVENTS: &str = "/api/mt/part/amer-03/v2.0/me/calendars/default/events";
const EVENT: &str = "/api/mt/part/amer-03/v2.0/me/calendars/events/MadeUpEvent";
const ONLINE: &str = "/api/mt/part/amer-03/beta/me/calendarEvents/privateMeeting/schedulingService/create";
const SCHEDULES: &str = "/api/mt/part/amer-03/v2.0/me/calendars/schedules";
const TIMES: &str = "/api/mt/part/amer-03/v2.0/me/calendars/findMeetingTimes";
const START: i64 = 1_793_491_200_000;
const MAIL_A: &str = "lab-a@lab.example.com";
const MAIL_B: &str = "lab-b@lab.example.com";

struct Tokens;

impl Credentials for Tokens {
    fn bearer(&self, _: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new("synthetic-calendar-token")))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

struct Rig {
    server: MockServer,
    handle: EngineHandle,
    _dir: tempfile::TempDir,
}

async fn running_engine(middle_tier: bool) -> Rig {
    running_engine_on(middle_tier, tempfile::tempdir().unwrap()).await
}

/// An engine on the store in `dir` (`calendar.sqlite`, made when missing).
async fn running_engine_on(middle_tier: bool, dir: tempfile::TempDir) -> Rig {
    let server = MockServer::start().await;
    let tokens = Arc::new(Tokens);
    let transport = || {
        let transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
        if middle_tier {
            transport.with_middle_tier(MIDDLE_TIER_BASE).unwrap()
        } else {
            transport
        }
    };
    let writes = Arc::new(transport());

    let outbox = Outbox::new(writes, Author::new(Mri::new(LAB_A), "Lab A"));
    let mut store = Store::open(&dir.path().join("calendar.sqlite")).unwrap();
    for (mri, name) in [(LAB_B, "Lab B"), (LAB_C, "Lab C")] {
        store
            .put_profile(&Profile {
                mri: mri.to_owned(),
                name: Some(name.to_owned()),
                name_source: Some(NameSource::ShortProfile),
                picture: PictureState::None,
                read_at: START,
            })
            .unwrap();
    }
    let handle = engine::start(Parts {
        calls: None,
        store,
        client: Arc::new(Client::new(transport())),
        outbox,
        push: None,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap();
    Rig {
        server,
        handle,
        _dir: dir,
    }
}

async fn answer(server: &MockServer, verb: &str, endpoint: &str, status: u16, body: Value) {
    Mock::given(method(verb))
        .and(path(endpoint))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

async fn next_answer(server: &MockServer, verb: &str, endpoint: &str, status: u16, body: Value) {
    Mock::given(method(verb))
        .and(path(endpoint))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .with_priority(1)
        .up_to_n_times(1)
        .mount(server)
        .await;
}

async fn identity(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path(PROFILES))
        .respond_with(|request: &Request| {
            let asked: Vec<String> = serde_json::from_slice(&request.body).unwrap();
            let profiles = [
                json!({"mri": LAB_A, "displayName": "Lab A", "email": MAIL_A}),
                json!({"mri": LAB_B, "displayName": "Remote B", "userPrincipalName": MAIL_B}),
                json!({"mri": LAB_C, "displayName": "Remote C", "email": "not-an-address"}),
            ];
            let found: Vec<_> = profiles
                .into_iter()
                .filter(|profile| asked.iter().any(|mri| profile["mri"] == *mri))
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({
                "type": "Microsoft.SkypeSpaces.MiddleTier.Models.IUserIdentity", "value": found,
            }))
        })
        .mount(server)
        .await;
    answer(server, "GET", ZONE, 200, json!({"timeZone": "Utc"})).await;
}

fn attendee(address: &str) -> EventAttendee {
    EventAttendee {
        address: address.to_owned(),
        name: Some("Synthetic attendee".to_owned()),
        mri: None,
        required: true,
    }
}

fn draft() -> EventDraft {
    EventDraft {
        subject: "Synthetic planning".to_owned(),
        start: ZonedTime {
            ms: START,
            offset_minutes: 0,
        },
        end: ZonedTime {
            ms: START + 1_800_000,
            offset_minutes: 0,
        },
        time_zone: None,
        zone: None,
        all_day: false,
        organizer: Organizer {
            mri: String::new(),
            address: None,
            name: None,
        },
        attendees: vec![attendee(MAIL_B)],
        location: "Made-up room".to_owned(),
        body_html: "<p>Made-up notes</p>".to_owned(),
        online: false,
        show_as: EventShowAs::Busy,
        reminder_minutes: Some(15),
        private: false,
        categories: Vec::new(),
        recurrence: None,
    }
}

fn event() -> Value {
    json!({
        "objectId": "MadeUpEvent",
        "subject": "Synthetic planning",
        "startTime": "2026-11-01T16:00:00Z",
        "endTime": "2026-11-01T16:30:00Z",
        "eventType": "Single",
        "organizerAddress": MAIL_A,
        "attendees": [{"address": MAIL_A}, {"address": MAIL_B}],
        "bodyContent": "<p>Made-up notes</p>",
        "untouchedField": "kept",
    })
}

async fn requests(server: &MockServer, verb: &str, endpoint: &str) -> Vec<Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.method.as_str() == verb && request.url.path() == endpoint)
        .collect()
}

async fn editable_calendar(server: &MockServer) {
    answer(server, "GET", EVENT, 200, event()).await;
    answer(server, "PUT", EVENT, 204, Value::Null).await;
    answer(server, "PUT", &format!("{EVENT}/cancel"), 204, Value::Null).await;
    answer(server, "PUT", &format!("{EVENT}/respond"), 204, Value::Null).await;
    answer(server, "DELETE", EVENT, 204, Value::Null).await;
    Mock::given(method("POST"))
        .and(path(EVENTS))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let mut created = body["calendarEvent"].clone();
            created["objectId"] = json!("MadeUpEvent");
            ResponseTemplate::new(201).set_body_json(json!({"value": [created]}))
        })
        .mount(server)
        .await;
}

#[tokio::test]
async fn calendar_writes_keep_the_organizer_and_fields_read_before_an_edit() {
    let rig = running_engine(true).await;
    identity(&rig.server).await;
    editable_calendar(&rig.server).await;
    let created = rig.handle.write_event(EventWrite::Create(draft())).await.unwrap();
    assert_eq!(created.created.unwrap().id, "MadeUpEvent");
    let full = rig.handle.calendar_event("MadeUpEvent").await.unwrap();
    assert_eq!(full.body_html.as_deref(), Some("<p>Made-up notes</p>"));
    let mut changed = draft();
    changed.subject = "Changed planning".to_owned();
    assert!(
        rig.handle
            .write_event(EventWrite::Update {
                id: "MadeUpEvent".to_owned(),
                event: full.raw,
                draft: changed,
            })
            .await
            .unwrap()
            .created
            .is_none()
    );
    rig.handle
        .write_event(EventWrite::Cancel {
            id: "MadeUpEvent".to_owned(),
            chat: None,
            message: Some("Made-up cancellation".to_owned()),
        })
        .await
        .unwrap();
    rig.handle
        .write_event(EventWrite::Respond {
            id: "MadeUpEvent".to_owned(),
            answer: teams_core::RsvpAnswer::Tentative,
            send: true,
            comment: Some("Made-up note".to_owned()),
        })
        .await
        .unwrap();
    rig.handle
        .write_event(EventWrite::Delete {
            id: "MadeUpEvent".to_owned(),
        })
        .await
        .unwrap();

    let posted: Value = serde_json::from_slice(&requests(&rig.server, "POST", EVENTS).await[0].body).unwrap();
    assert_eq!(posted["calendarEvent"]["organizerAddress"], MAIL_A);
    assert_eq!(posted["calendarEvent"]["organizerName"], "Lab A");
    assert_eq!(posted["calendarEvent"]["eventTimeZone"], "Utc");
    let answered: Value =
        serde_json::from_slice(&requests(&rig.server, "PUT", &format!("{EVENT}/respond")).await[0].body).unwrap();
    assert_eq!(
        answered,
        json!({"responseType": "TentativelyAccept", "sendResponse": true, "comment": "Made-up note"})
    );
    let updated: Value = serde_json::from_slice(&requests(&rig.server, "PUT", EVENT).await[0].body).unwrap();
    assert_eq!(updated["updatedCalendarEvent"]["subject"], "Changed planning");
    assert_eq!(updated["updatedCalendarEvent"]["untouchedField"], "kept");
    assert_eq!(
        requests(&rig.server, "POST", PROFILES).await.len(),
        1,
        "the organizer's address is cached"
    );
    assert_eq!(
        requests(&rig.server, "GET", ZONE).await.len(),
        1,
        "the organizer's zone is cached"
    );
    assert_eq!(
        requests(&rig.server, "GET", EVENT).await.len(),
        1,
        "the explicitly requested full event read"
    );
    rig.handle.shutdown();
}

/// A draft with its times in Honolulu's offset, the zone the form chose.
fn honolulu_draft() -> EventDraft {
    let mut draft = draft();
    draft.attendees.clear();
    draft.zone = Some("Pacific/Honolulu".to_owned());
    draft.start.offset_minutes = -600;
    draft.end.offset_minutes = -600;
    draft
}

#[tokio::test]
async fn a_chosen_zone_is_sent_by_its_exchange_name_or_the_times_move_to_the_desktop_s_zone() {
    let rig = running_engine(true).await;
    identity(&rig.server).await;
    editable_calendar(&rig.server).await;
    Mock::given(method("GET"))
        .and(path(ZONE))
        .and(wiremock::matchers::query_param("timezone", "Pacific/Honolulu"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"timeZone": "HawaiianSt"})))
        .with_priority(1)
        .up_to_n_times(1)
        .mount(&rig.server)
        .await;
    rig.handle
        .write_event(EventWrite::Create(honolulu_draft()))
        .await
        .unwrap();
    // the zone's read fails the second time: the desktop's zone, the times in its offsets
    Mock::given(method("GET"))
        .and(path(ZONE))
        .and(wiremock::matchers::query_param("timezone", "Pacific/Honolulu"))
        .respond_with(ResponseTemplate::new(500))
        .with_priority(1)
        .mount(&rig.server)
        .await;
    rig.handle
        .write_event(EventWrite::Create(honolulu_draft()))
        .await
        .unwrap();

    let posted: Vec<Value> = requests(&rig.server, "POST", EVENTS)
        .await
        .iter()
        .map(|request| serde_json::from_slice::<Value>(&request.body).unwrap()["calendarEvent"].clone())
        .collect();
    assert_eq!(posted[0]["eventTimeZone"], "HawaiianSt");
    assert_eq!(posted[0]["utcOffset"], -600);
    assert_eq!(posted[0]["startTime"], "2026-10-31T14:00:00-10:00");
    assert_eq!(posted[0]["endTime"], "2026-10-31T14:30:00-10:00");
    assert_eq!(posted[1]["eventTimeZone"], "Utc");
    let desktop = |ms: i64| {
        chrono::DateTime::from_timestamp_millis(ms)
            .unwrap()
            .with_timezone(&chrono::Local)
            .to_rfc3339()
    };
    assert_eq!(posted[1]["startTime"], desktop(START));
    assert_eq!(posted[1]["endTime"], desktop(START + 1_800_000));
    rig.handle.shutdown();
}

#[tokio::test]
async fn the_scheduler_keeps_each_successful_half_and_fails_only_when_both_reads_fail() {
    let rig = running_engine(true).await;
    identity(&rig.server).await;
    answer(&rig.server, "PUT", SCHEDULES, 200, json!([{
        "userPrincipalName": MAIL_B,
        "freeBusyEvents": [{"freeBusyStatus": "Busy", "startTime": "2026-11-01T16:00:00Z", "endTime": "2026-11-01T16:30:00Z"}],
    }])).await;
    answer(
        &rig.server,
        "PUT",
        TIMES,
        200,
        json!({"timeSuggestions": [{"meetingTime": "2026-11-01T17:00:00Z"}]}),
    )
    .await;
    let both = rig.handle.availability(vec![attendee(MAIL_B)], START, 0).await.unwrap();
    assert_eq!(both.people[0].address, MAIL_B);
    assert_eq!(both.people[0].busy.len(), 1);
    assert_eq!(both.suggestions.len(), 1);
    for endpoint in [SCHEDULES, TIMES] {
        let body: Value = serde_json::from_slice(&requests(&rig.server, "PUT", endpoint).await[0].body).unwrap();
        assert_eq!(body["meetingDurationInMinutes"], 1, "a zero-length request is clamped");
        assert_eq!(body["attendees"][1]["address"], MAIL_A);
    }
    next_answer(&rig.server, "PUT", SCHEDULES, 403, Value::Null).await;
    let times_only = rig.handle.availability(Vec::new(), START, 30).await.unwrap();
    assert!(times_only.people.is_empty());
    assert_eq!(times_only.suggestions, both.suggestions);
    next_answer(&rig.server, "PUT", TIMES, 200, json!({"malformed": true})).await;
    let schedules_only = rig.handle.availability(Vec::new(), START, 30).await.unwrap();
    assert_eq!(schedules_only.people, both.people);
    assert!(schedules_only.suggestions.is_empty());
    next_answer(&rig.server, "PUT", SCHEDULES, 200, json!({"malformed": true})).await;
    next_answer(&rig.server, "PUT", TIMES, 403, Value::Null).await;
    assert!(matches!(
        rig.handle.availability(Vec::new(), START, 30).await,
        Err(EngineError::CalendarUnread)
    ));
    rig.handle.shutdown();
}

#[tokio::test]
async fn people_s_calendars_are_read_five_days_a_read_and_joined_by_person() {
    let rig = running_engine(true).await;
    identity(&rig.server).await;
    answer(&rig.server, "PUT", SCHEDULES, 200, json!([{
        "userPrincipalName": MAIL_B,
        "freeBusyEvents": [{"freeBusyStatus": "Busy", "startTime": "2026-11-01T16:00:00Z", "endTime": "2026-11-01T16:30:00Z"}],
    }])).await;
    let read = rig
        .handle
        .people_calendars(vec![attendee(MAIL_B)], START, 7)
        .await
        .unwrap();
    // a week is two reads, five days from the start and then from five days on; the same busy time once
    let asked = requests(&rig.server, "PUT", SCHEDULES).await;
    assert_eq!(asked.len(), 2);
    let mut starts: Vec<String> = asked
        .iter()
        .map(|request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["numberOfDays"], 5);
            assert_eq!(body["attendees"][0]["address"], MAIL_B);
            body["startTime"].as_str().unwrap().to_owned()
        })
        .collect();
    starts.sort();
    assert_ne!(starts[0], starts[1]);
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].address, MAIL_B);
    assert_eq!(read[0].busy.len(), 1);
    // nobody asked about reads nothing
    assert!(
        rig.handle
            .people_calendars(Vec::new(), START, 7)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(requests(&rig.server, "PUT", SCHEDULES).await.len(), 2);
    rig.server.reset().await;
    identity(&rig.server).await;
    answer(&rig.server, "PUT", SCHEDULES, 403, Value::Null).await;
    assert!(matches!(
        rig.handle.people_calendars(vec![attendee(MAIL_B)], START, 3).await,
        Err(EngineError::CalendarUnread)
    ));
    rig.handle.shutdown();
}

#[tokio::test]
async fn meet_now_makes_a_meeting_of_the_account_s_own_once() {
    let rig = running_engine(true).await;
    answer(&rig.server, "POST", ONLINE, 201, json!({"value": {
        "groupContext": {"threadId": "19:meeting_made-up@thread.v2"},
        "links": {"join": "https://teams.microsoft.com/l/meetup-join/made-up", "shortJoinUrl": "https://teams.microsoft.com/meet/1?p=x"},
    }})).await;
    let made = rig.handle.meet_now("Meeting with Lab").await.unwrap();
    assert_eq!(made.thread, "19:meeting_made-up@thread.v2");
    assert_eq!(made.share, "https://teams.microsoft.com/meet/1?p=x");
    let asked = requests(&rig.server, "POST", ONLINE).await;
    assert_eq!(asked.len(), 1);
    let body: Value = serde_json::from_slice(&asked[0].body).unwrap();
    assert_eq!(body["meetingType"], "MeetNow");
    assert_eq!(body["subject"], "Meeting with Lab");
    next_answer(&rig.server, "POST", ONLINE, 500, Value::Null).await;
    assert!(matches!(
        rig.handle.meet_now("Again").await,
        Err(EngineError::EventNotSaved { .. })
    ));
    rig.handle.shutdown();
}

#[tokio::test]
async fn the_attendee_picker_uses_stored_names_and_only_profiles_with_an_address() {
    let rig = running_engine(true).await;
    identity(&rig.server).await;
    let found = rig.handle.invitable("Lab").await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(
        (&*found[0].mri, &*found[0].name, &*found[0].address),
        (LAB_B, "Lab B", MAIL_B)
    );
    assert!(rig.handle.invitable("no match").await.unwrap().is_empty());
    next_answer(&rig.server, "POST", PROFILES, 403, Value::Null).await;
    assert!(rig.handle.invitable("Lab").await.unwrap().is_empty());
    rig.handle.shutdown();
    let no_middle_tier = running_engine(false).await;
    assert!(no_middle_tier.handle.invitable("Lab").await.unwrap().is_empty());
    no_middle_tier.handle.shutdown();
}

#[tokio::test]
async fn invalid_or_unreadable_calendar_answers_fail_without_guessing_an_event() {
    let rig = running_engine(true).await;
    assert!(rig.handle.calendar(START, START).await.unwrap().is_empty());
    assert!(rig.handle.calendar(START + 1, START).await.unwrap().is_empty());
    assert!(matches!(
        rig.handle.calendar_event(" ").await,
        Err(EngineError::CalendarUnread)
    ));
    assert!(requests(&rig.server, "GET", EVENT).await.is_empty());
    answer(&rig.server, "GET", EVENT, 200, json!({"not": "an event"})).await;
    assert!(matches!(
        rig.handle.calendar_event("MadeUpEvent").await,
        Err(EngineError::CalendarUnread)
    ));
    next_answer(&rig.server, "GET", EVENT, 403, Value::Null).await;
    assert!(matches!(
        rig.handle.calendar_event("MadeUpEvent").await,
        Err(EngineError::CalendarUnread)
    ));
    next_answer(&rig.server, "GET", EVENT, 404, Value::Null).await;
    assert!(matches!(
        rig.handle.calendar_event("MadeUpEvent").await,
        Err(EngineError::NoSuchEvent)
    ));
    let view = format!("{PREFIX}/v2.0/me/calendars/default/calendarView");
    answer(
        &rig.server,
        "GET",
        &view,
        200,
        json!({"value": [event(), {"broken": true}]}),
    )
    .await;
    assert_eq!(rig.handle.calendar(START, START + 86_400_000).await.unwrap().len(), 1);
    next_answer(&rig.server, "GET", &view, 200, json!({"value": "not a list"})).await;
    assert!(matches!(
        rig.handle.calendar(START, START + 86_400_000).await,
        Err(EngineError::CalendarUnread)
    ));
    next_answer(&rig.server, "GET", &view, 403, Value::Null).await;
    assert!(matches!(
        rig.handle.calendar(START, START + 86_400_000).await,
        Err(EngineError::CalendarUnread)
    ));
    rig.handle.shutdown();
}

#[tokio::test]
async fn an_online_event_carries_its_meeting_and_a_failed_meeting_prevents_the_invitation() {
    let rig = running_engine(true).await;
    identity(&rig.server).await;
    let meeting = json!({
        "groupContext": {"threadId": "19:meeting_MadeUp@thread.v2"},
        "links": {"join": "https://teams.microsoft.com/l/meetup-join/19%3ameeting_MadeUp%40thread.v2/0"},
    });
    answer(&rig.server, "POST", ONLINE, 201, json!({"value": meeting})).await;
    answer(&rig.server, "POST", EVENTS, 201, json!({"value": [event()]})).await;
    let mut online = draft();
    online.online = true;
    next_answer(&rig.server, "POST", ONLINE, 403, Value::Null).await;
    assert!(matches!(
        rig.handle.write_event(EventWrite::Create(online.clone())).await,
        Err(EngineError::EventNotSaved { kind: "creation", .. })
    ));
    assert!(requests(&rig.server, "POST", EVENTS).await.is_empty());
    rig.handle.write_event(EventWrite::Create(online)).await.unwrap();
    let request = requests(&rig.server, "POST", EVENTS).await.remove(0);
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["schedulingServiceEvent"], meeting);
    assert!(
        request
            .url
            .query_pairs()
            .any(|(key, value)| key == "isOnlineMeeting" && value == "true")
    );
    rig.handle.shutdown();
}

#[tokio::test]
async fn invalid_changes_and_uncertain_or_refused_writes_are_never_resent() {
    let rig = running_engine(true).await;
    identity(&rig.server).await;
    for write in [
        EventWrite::Update {
            id: "other".to_owned(),
            event: event().as_object().unwrap().clone(),
            draft: draft(),
        },
        EventWrite::Cancel {
            id: String::new(),
            chat: None,
            message: None,
        },
        EventWrite::Delete { id: String::new() },
    ] {
        let kind = write.kind();
        assert!(
            matches!(rig.handle.write_event(write).await, Err(EngineError::EventNotSaved { kind: got, .. }) if got == kind)
        );
    }
    answer(&rig.server, "POST", EVENTS, 201, json!({"value": [event()]})).await;
    for status in [403, 500] {
        next_answer(&rig.server, "POST", EVENTS, status, Value::Null).await;
        assert!(matches!(
            rig.handle.write_event(EventWrite::Create(draft())).await,
            Err(EngineError::EventNotSaved { kind: "creation", .. })
        ));
    }
    assert_eq!(
        requests(&rig.server, "POST", EVENTS).await.len(),
        2,
        "one attempt per failed write"
    );
    // A 401 says the server did not act, so it permits one retry after credential renewal.
    next_answer(&rig.server, "POST", EVENTS, 401, Value::Null).await;
    rig.handle.write_event(EventWrite::Create(draft())).await.unwrap();
    let sent = requests(&rig.server, "POST", EVENTS).await;
    assert_eq!(sent.len(), 4);
    assert_eq!(sent[2].body, sent[3].body, "a retry keeps the transaction id");
    rig.handle.shutdown();
}

#[tokio::test]
async fn an_engine_without_the_middle_tier_saves_no_event_and_sends_nothing() {
    let rig = running_engine(false).await;
    let refused = rig.handle.write_event(EventWrite::Create(draft())).await;
    assert!(
        matches!(refused, Err(EngineError::EventNotSaved { kind: "creation", .. })),
        "{refused:?}"
    );
    assert!(
        rig.server.received_requests().await.unwrap().is_empty(),
        "nothing went out"
    );
    rig.handle.shutdown();
}

/// The ids the store answers for `START`'s day once the engine kept what it read (the read's answer comes first):
/// asked until they are `expected`, for a bounded while.
async fn stored_settles(rig: &Rig, expected: &[&str]) {
    let mut ids = Vec::new();
    for _ in 0..100 {
        let stored = rig.handle.stored_calendar(START, START + 86_400_000).await.unwrap();
        ids = stored.into_iter().map(|event| event.id).collect::<Vec<_>>();
        if ids == expected {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("stored {ids:?}, not {expected:?}");
}

#[tokio::test]
async fn a_window_read_is_kept_and_the_next_read_replaces_it() {
    let rig = running_engine(true).await;
    let view = format!("{PREFIX}/v2.0/me/calendars/default/calendarView");
    assert!(
        rig.handle
            .stored_calendar(START, START + 86_400_000)
            .await
            .unwrap()
            .is_empty()
    );
    let mut occurrence = event();
    occurrence["objectId"] = json!("MadeUpOccurrence");
    occurrence["eventType"] = json!("Occurrence");
    occurrence["startTime"] = json!("2026-11-01T18:00:00Z");
    occurrence["endTime"] = json!("2026-11-01T19:00:00Z");
    let mut master = occurrence.clone();
    master["objectId"] = json!("MadeUpMaster");
    master["eventType"] = json!("RecurringMaster");
    next_answer(
        &rig.server,
        "GET",
        &view,
        200,
        json!({"value": [event(), occurrence, master]}),
    )
    .await;
    assert_eq!(rig.handle.calendar(START, START + 86_400_000).await.unwrap().len(), 2);
    stored_settles(&rig, &["MadeUpEvent", "MadeUpOccurrence"]).await;
    let stored = rig.handle.stored_calendar(START, START + 86_400_000).await.unwrap();
    assert_eq!(stored[0].subject, "Synthetic planning", "read back as it came");
    assert!(
        rig.handle
            .stored_calendar(START + 86_400_000, START + 2 * 86_400_000)
            .await
            .unwrap()
            .is_empty(),
        "nothing on the next day"
    );
    assert!(
        rig.handle.stored_calendar(START, START).await.unwrap().is_empty(),
        "an empty window"
    );

    // a failed read changes nothing kept
    next_answer(&rig.server, "GET", &view, 403, Value::Null).await;
    assert!(rig.handle.calendar(START, START + 86_400_000).await.is_err());
    stored_settles(&rig, &["MadeUpEvent", "MadeUpOccurrence"]).await;

    // the next read: the event gone, the occurrence cancelled
    let mut cancelled = event();
    cancelled["objectId"] = json!("MadeUpOccurrence");
    cancelled["eventType"] = json!("Occurrence");
    cancelled["isCancelled"] = json!(true);
    next_answer(&rig.server, "GET", &view, 200, json!({"value": [cancelled]})).await;
    assert_eq!(rig.handle.calendar(START, START + 86_400_000).await.unwrap().len(), 1);
    stored_settles(&rig, &["MadeUpOccurrence"]).await;
    let stored = rig.handle.stored_calendar(START, START + 86_400_000).await.unwrap();
    assert!(stored[0].cancelled);
    rig.handle.shutdown();
}

#[tokio::test]
async fn a_kept_window_is_answered_by_a_later_engine_on_the_same_store() {
    // the store's own reopen is `teams-store`'s test; here the kept rows read back through a new engine's query
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calendar.sqlite");
    let entry = event().to_string();
    {
        let mut store = Store::open(&path).unwrap();
        let start = START + 16 * 3_600_000;
        store
            .replace_calendar_window(
                START,
                START + 86_400_000,
                &[teams_store::StoredEvent {
                    id: "MadeUpEvent".to_owned(),
                    start_ms: start,
                    end_ms: start + 1_800_000,
                    entry,
                }],
            )
            .unwrap();
    }
    let rig = running_engine_on(true, dir).await;
    let stored = rig.handle.stored_calendar(START, START + 86_400_000).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, "MadeUpEvent");
    rig.handle.shutdown();
}

const WEEK: i64 = 7 * 86_400_000;
const MASTER: &str = "/api/mt/part/amer-03/v2.0/me/calendars/events/MadeUpMaster";
const VIEW: &str = "/api/mt/part/amer-03/v2.0/me/calendars/default/calendarView";

/// An occurrence of the made-up weekly series `MadeUpMaster`, on the Monday `week` weeks after `START`'s week.
fn occurrence(week: i64, subject: &str) -> Value {
    json!({
        "objectId": format!("MadeUpOccurrence{week}"),
        "subject": subject,
        "startTime": format!("2026-11-{:02}T09:00:00Z", 2 + 7 * week),
        "endTime": format!("2026-11-{:02}T09:30:00Z", 2 + 7 * week),
        "eventType": "Occurrence",
        "seriesMasterId": "MadeUpMaster",
        "organizerAddress": MAIL_A,
        "isOrganizer": true,
        "myResponseType": "Organizer",
    })
}

fn series_master(subject: &str) -> Value {
    let mut master = occurrence(0, subject);
    master["objectId"] = json!("MadeUpMaster");
    master["eventType"] = json!("RecurringMaster");
    master.as_object_mut().unwrap().remove("seriesMasterId");
    master
}

/// The ids the store answers for `start..start + WEEK` now, with no wait: what a view drawn from it right after a
/// confirmed write shows.
async fn week_now(rig: &Rig, start: i64) -> Vec<String> {
    let stored = rig.handle.stored_calendar(start, start + WEEK).await.unwrap();
    stored.into_iter().map(|event| event.id).collect()
}

/// The week from `start` read, answered with `events`, and kept as `expected` (asked for a bounded while: the read's
/// answer comes before the engine keeps it).
async fn read_week(rig: &Rig, start: i64, events: Vec<Value>, expected: &[&str]) {
    next_answer(&rig.server, "GET", VIEW, 200, json!({"value": events})).await;
    rig.handle.calendar(start, start + WEEK).await.unwrap();
    let mut ids = Vec::new();
    for _ in 0..100 {
        ids = week_now(rig, start).await;
        if ids == expected {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("stored {ids:?}, not {expected:?}");
}

#[tokio::test]
async fn a_confirmed_write_leaves_no_kept_copy_of_what_it_changed_in_any_week() {
    let rig = running_engine(true).await;
    identity(&rig.server).await;
    editable_calendar(&rig.server).await;
    answer(&rig.server, "PUT", MASTER, 204, Value::Null).await;
    answer(&rig.server, "DELETE", MASTER, 204, Value::Null).await;

    // two weeks read and kept: the series' occurrence in each, the single event in the first
    let first = ["MadeUpEvent", "MadeUpOccurrence0"];
    read_week(&rig, START, vec![event(), occurrence(0, "Weekly sync")], &first).await;
    read_week(
        &rig,
        START + WEEK,
        vec![occurrence(1, "Weekly sync")],
        &["MadeUpOccurrence1"],
    )
    .await;

    // the series renamed: each week forgets its occurrence the moment the change is confirmed
    let mut renamed = draft();
    renamed.subject = "Renamed sync".to_owned();
    renamed.attendees.clear();
    rig.handle
        .write_event(EventWrite::Update {
            id: "MadeUpMaster".to_owned(),
            event: series_master("Weekly sync").as_object().unwrap().clone(),
            draft: renamed,
        })
        .await
        .unwrap();
    assert_eq!(week_now(&rig, START).await, ["MadeUpEvent"]);
    assert!(week_now(&rig, START + WEEK).await.is_empty());

    // read again as renamed, then the whole series deleted, with a read of the second week under way that began
    // before the delete was confirmed and answers with the series as it was
    read_week(&rig, START, vec![event(), occurrence(0, "Renamed sync")], &first).await;
    read_week(
        &rig,
        START + WEEK,
        vec![occurrence(1, "Renamed sync")],
        &["MadeUpOccurrence1"],
    )
    .await;
    Mock::given(method("GET"))
        .and(path(VIEW))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"value": [occurrence(1, "Renamed sync")]}))
                .set_delay(std::time::Duration::from_millis(300)),
        )
        .with_priority(1)
        .up_to_n_times(1)
        .mount(&rig.server)
        .await;
    let under_way = rig.handle.calendar(START + WEEK, START + 2 * WEEK);
    rig.handle
        .write_event(EventWrite::Delete {
            id: "MadeUpMaster".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(week_now(&rig, START).await, ["MadeUpEvent"]);
    assert!(week_now(&rig, START + WEEK).await.is_empty());
    assert_eq!(under_way.await.unwrap().len(), 1, "its asker is answered");
    for _ in 0..20 {
        assert!(
            week_now(&rig, START + WEEK).await.is_empty(),
            "a read that began before the delete isn't kept"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    the_single_event_is_forgotten_once_changed_or_deleted_and_a_new_one_kept(&rig).await;
    rig.handle.shutdown();
}

/// The single event of `START`'s week, kept: changed, read again, deleted; then a new one made.
async fn the_single_event_is_forgotten_once_changed_or_deleted_and_a_new_one_kept(rig: &Rig) {
    rig.handle
        .write_event(EventWrite::Update {
            id: "MadeUpEvent".to_owned(),
            event: event().as_object().unwrap().clone(),
            draft: draft(),
        })
        .await
        .unwrap();
    assert!(week_now(rig, START).await.is_empty());
    read_week(rig, START, vec![event()], &["MadeUpEvent"]).await;
    rig.handle
        .write_event(EventWrite::Delete {
            id: "MadeUpEvent".to_owned(),
        })
        .await
        .unwrap();
    assert!(week_now(rig, START).await.is_empty());

    // a new event is kept at once in the week it falls in
    rig.handle.write_event(EventWrite::Create(draft())).await.unwrap();
    assert_eq!(week_now(rig, START).await, ["MadeUpEvent"]);
    assert!(week_now(rig, START + WEEK).await.is_empty());
}
