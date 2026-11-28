//! The control socket's calendar commands (`ctl::calendar`), through the real protocol and server, over lab account a's
//! engine on the mock (`teams-mock`'s middle tier: the same made-up working week every week, its Teams meetings with
//! their join links, an all-day event that is no meeting). The app shows calls, but a's engine has no calling: a
//! meeting's pre-join opens and nothing goes out; its Join now asks the engine, which refuses it as no calling, so no
//! call or meeting is ever joined. The view trace counts the calls' asks. Every id is made up or the mock's own.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared harness holds more than this test uses")]
mod ctl_support;

use std::future::{self, Future};
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

use ctl_support::{ask, quit};
use gpui::{Entity, TestAppContext};
use ngteams::accounts::{Runner, Started};
use ngteams::accounts_file::{PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::calling::PersonalCalls;
use ngteams::setup::Setup;
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, TokenClient, TokenError};
use teams_core::account::AccountEngine;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts};
use teams_core::{CallFloor, Outbox};
use teams_ctl::ClientError;
use teams_ctl::protocol::{
    AttendeeRow, CalendarRow, CtlError, ErrorCode, EventAnswer, Request, Response, calendar_window, local_today,
};
use teams_mock::{CHAT_BASE, ChatServiceMock, ECHO, LAB_A, MIDDLE_TIER_BASE, TENANT};
use teams_proto::{Author, Client, Credentials, HttpTransport, MAX_CALENDAR_WINDOW_MS, Mri};
use teams_store::Store;
use teams_ui::bus::BusError;
use teams_ui::{Calls, SettingsFile, ViewTrace};

const UPN: &str = "bot-a@lab.example.test";
/// The organizer the mock's join links name (their `context`'s `Oid`).
const ORGANIZER: &str = "00000000-0000-4000-8000-00000000000b";

/// a's token, as the mock hands it out.
struct Tokens(String);

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

/// The view trace's lines, kept for the test to read.
#[derive(Clone, Default)]
struct Traced(Arc<Mutex<Vec<u8>>>);

impl Write for Traced {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Traced {
    /// The chats the calls were asked `kind` in (`join-meeting`, …), in order.
    fn asked(&self, kind: &str) -> Vec<String> {
        let text = String::from_utf8(self.0.lock().unwrap().clone()).unwrap();
        text.lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|line| line["view"] == "call" && line["change"] == "asked" && line["kinds"][0] == kind)
            .map(|line| line["conversation"].as_str().unwrap_or_default().to_owned())
            .collect()
    }
}

/// The mock, and the app of lab account a on it serving its control socket.
struct Lab {
    socket: PathBuf,
    traced: Traced,
    cx: TestAppContext,
    _mock: ChatServiceMock,
    _dir: tempfile::TempDir,
    _runtime: tokio::runtime::Runtime,
}

impl Lab {
    /// a's app on the mock, showing calls when `calls`.
    fn start(calls: bool) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let mock = runtime.block_on(ChatServiceMock::start()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let handle = {
            let _entered = runtime.enter();
            a_engine(&mock, &dir)
        };
        let socket = dir.path().join("run/ngteams/ctl.sock");
        let traced = Traced::default();
        let cx = TestAppContext::single();
        let calls = calls.then(|| PersonalCalls {
            ringer: None,
            sounds: None,
            echo: Mri::new(ECHO),
            video: None,
            camera: None,
            devices: None,
            noise: teams_media::processing::NoiseControl::default(),
        });
        let launch = Launch {
            first: Started {
                calls,
                ..started(handle)
            },
            others: Vec::new(),
            settings: app::settings(SettingsFile::new(dir.path().join("settings.toml")), None),
            bus: Err(BusError::NotGiven),
            sign_in_again: Rc::new(|_| Rc::new(|_| {})),
            runner: Runner {
                setup: Setup::new(PersonalPaths::new(dir.path().join("config"), dir.path().join("data"))).unwrap(),
                floor: CallFloor::new(),
                devices: None,
            },
            read_only: false,
            trace: Arc::new(ViewTrace::new(traced.clone())),
            ctl: Some(socket.clone()),
        };
        let Ok(_launched) = cx.update(|cx| app::launch(cx, launch)) else {
            panic!("the window opens");
        };
        cx.run_until_parked();
        Self {
            socket,
            traced,
            cx,
            _mock: mock,
            _dir: dir,
            _runtime: runtime,
        }
    }

    fn ask(&self, request: Request) -> Result<Response, ClientError> {
        ask(&self.cx, &self.socket, None, request)
    }

    /// The error `request` answers with.
    fn refused(&self, request: Request) -> CtlError {
        let asked = format!("{request:?}");
        match self.ask(request) {
            Err(ClientError::Server(error)) => error,
            other => panic!("{asked}: {other:?}"),
        }
    }

    /// The events of the days `from` to `to` (local dates).
    fn listed(&self, from: Option<&str>, to: Option<&str>) -> Vec<CalendarRow> {
        let request = Request::Calendar {
            from: from.map(str::to_owned),
            to: to.map(str::to_owned),
        };
        match self.ask(request) {
            Ok(Response::Calendar { events }) => events,
            other => panic!("calendar {from:?}..{to:?}: {other:?}"),
        }
    }

    /// This week's events (seven days from today: the made-up week once whole).
    fn week(&self) -> Vec<CalendarRow> {
        let (from, to) = week();
        self.listed(Some(&from), Some(&to))
    }

    /// The first event of this week called `subject`.
    fn event(&self, subject: &str) -> CalendarRow {
        self.week()
            .into_iter()
            .find(|event| event.subject == subject)
            .unwrap_or_else(|| panic!("no {subject} this week"))
    }

    fn calls(&self) -> Entity<Calls> {
        self.cx.read(|cx| app::bridges(cx)[0].read(cx).models().calls.clone())
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        quit(&self.cx);
    }
}

/// Today and six days on, as `calendar --from --to` spells them.
fn week() -> (String, String) {
    let today = local_today();
    let last = (0..6).fold(today, |day, _| day.succ_opt().unwrap());
    (
        today.format("%Y-%m-%d").to_string(),
        last.format("%Y-%m-%d").to_string(),
    )
}

/// a's engine on the mock without a push channel or calling, its calendar read from the mock's middle tier.
fn a_engine(mock: &ChatServiceMock, dir: &tempfile::TempDir) -> EngineHandle {
    let tokens = Arc::new(Tokens(mock.token(LAB_A).unwrap()));
    let origin = mock.origin();
    let http = || {
        HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin)
            .unwrap()
            .with_middle_tier(MIDDLE_TIER_BASE)
            .unwrap()
    };
    engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("a.sqlite")).unwrap(),
        client: Arc::new(Client::new(http())),
        outbox: Outbox::new(Arc::new(http()), Author::new(Mri::new(LAB_A), "bot-a")),
        push: None,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap()
}

/// a's engine as the app starts an account: its session only for the sign-out watch, reaching nothing (an empty
/// in-memory store, a loopback port nothing listens on).
fn started(handle: EngineHandle) -> Started<MemorySecretStore> {
    let mut account = PersonalAccount::new(UPN, LAB_A.strip_prefix("8:orgid:").unwrap(), TENANT).unwrap();
    account.name = Some("bot-a".to_owned());
    let session = Session::new(
        UPN,
        MemorySecretStore::new(),
        TokenClient::new(EntraConfig::teams_native().with_authority("http://127.0.0.1:9/")).unwrap(),
        AuthzClient::with_endpoint("http://127.0.0.1:9/authz").unwrap(),
        Region::default(),
    );
    Started {
        account,
        engine: AccountEngine {
            handle,
            session: Arc::new(session),
        },
        calls: None,
    }
}

fn join_meeting(event: &str, now: bool) -> Request {
    Request::JoinMeeting {
        event: event.to_owned(),
        now,
    }
}

#[test]
fn a_range_lists_its_events_by_their_start_each_inside_it() {
    let lab = Lab::start(true);
    let (from, to) = week();
    let (start, end) = calendar_window(Some(&from), Some(&to), local_today()).unwrap();
    let events = lab.week();
    // seven days hold the made-up week once
    assert_eq!(events.len(), 15, "{events:#?}");
    assert!(
        events.iter().all(|event| (start..end).contains(&event.start)),
        "{events:#?}"
    );
    assert!(events.is_sorted_by_key(|event| event.start), "{events:#?}");
    let sync = lab.event("Product all hands");
    assert!(sync.online && sync.joinable && !sync.cancelled, "{sync:?}");
    assert_eq!(sync.organizer.as_deref(), Some("Riley Placeholder"));
    assert_eq!(sync.location.as_deref(), Some("Microsoft Teams Meeting"));
    let release = lab.event("Release day");
    assert!(release.all_day && !release.online && !release.joinable, "{release:?}");
    assert_eq!(release.location, None, "an empty location is none");
    assert!(
        events
            .iter()
            .any(|event| event.cancelled && event.subject.starts_with("Canceled: ")),
        "{events:#?}"
    );
    // today alone: what of the week starts before tomorrow
    let (today_start, today_end) = calendar_window(None, None, local_today()).unwrap();
    let today = lab.listed(None, None);
    let expected: Vec<&CalendarRow> = events
        .iter()
        .filter(|event| (today_start..today_end).contains(&event.start))
        .collect();
    assert_eq!(today.iter().collect::<Vec<_>>(), expected);
    // a day that doesn't read, or days the wrong way round
    for (from, to) in [(Some("2026-13-01"), None), (Some(to.as_str()), Some(from.as_str()))] {
        let request = Request::Calendar {
            from: from.map(str::to_owned),
            to: to.map(str::to_owned),
        };
        assert_eq!(lab.refused(request).code, ErrorCode::BadRequest, "{from:?}..{to:?}");
    }
}

#[test]
fn a_range_over_the_widest_read_lists_every_event_of_it_once() {
    let lab = Lab::start(true);
    let week = lab.week();
    // thirteen weeks: wider than one read of the service (62 days), the made-up week thirteen times
    let today = local_today();
    let last = (0..90).fold(today, |day, _| day.succ_opt().unwrap());
    let (from, to) = (
        today.format("%Y-%m-%d").to_string(),
        last.format("%Y-%m-%d").to_string(),
    );
    let (start, end) = calendar_window(Some(&from), Some(&to), today).unwrap();
    assert!(end - start > MAX_CALENDAR_WINDOW_MS, "{start}..{end}");
    let events = lab.listed(Some(&from), Some(&to));
    assert_eq!(events.len(), 13 * week.len(), "{events:#?}");
    assert!(
        events.iter().all(|event| (start..end).contains(&event.start)),
        "{events:#?}"
    );
    assert!(events.is_sorted_by_key(|event| event.start), "{events:#?}");
    let ids: std::collections::HashSet<&str> = events.iter().map(|event| event.id.as_str()).collect();
    assert_eq!(ids.len(), events.len(), "each event once");
    assert_eq!(events[..week.len()], week[..], "the range's first week is this one");
}

#[test]
fn an_event_s_details_are_read_whole_and_an_unknown_one_is_the_engine_s_error() {
    let lab = Lab::start(true);
    let listed = lab.event("Product all hands");
    let Ok(Response::CalendarEvent(shown)) = lab.ask(Request::CalendarEvent {
        event: listed.id.clone(),
    }) else {
        panic!("the event wasn't read");
    };
    assert_eq!(
        CalendarRow {
            id: shown.id.clone(),
            subject: shown.subject.clone(),
            start: shown.start,
            end: shown.end,
            all_day: shown.all_day,
            cancelled: shown.cancelled,
            online: shown.online,
            joinable: shown.joinable,
            location: shown.location.clone(),
            organizer: shown.organizer.clone(),
            ..CalendarRow::default()
        },
        listed,
        "the list's row, and more"
    );
    // someone else's meeting: the account's answer, and no answer of the others (only the organizer's copy has them)
    assert_eq!(shown.response, Some(EventAnswer::Accepted));
    assert_eq!(shown.organizer_address.as_deref(), Some("someone@example.com"));
    assert_eq!((shown.series.as_deref(), shown.recurrence.as_deref()), (None, None));
    assert!(
        shown.attendees.iter().all(|attendee| attendee.response.is_none()),
        "{shown:#?}"
    );
    assert_eq!(shown.attendees.len(), 4, "{shown:#?}");
    let unknown = lab.refused(Request::CalendarEvent {
        event: "AAMkNoSuchEvent".to_owned(),
    });
    assert_eq!(unknown.code, ErrorCode::UnknownEvent, "{unknown:?}");
    assert_eq!(
        unknown.message,
        "no event AAMkNoSuchEvent in the calendar: `ngteamsctl calendar` lists them"
    );
}

#[test]
fn an_own_series_occurrence_shows_its_text_attendees_answers_join_link_and_recurrence() {
    let lab = Lab::start(true);
    let listed = lab.event("Code review");
    let Ok(Response::CalendarEvent(shown)) = lab.ask(Request::CalendarEvent {
        event: listed.id.clone(),
    }) else {
        panic!("the event wasn't read");
    };
    assert_eq!(shown.body.as_deref(), Some("Made-up notes"));
    assert_eq!(shown.response, Some(EventAnswer::Organizer));
    assert_eq!(shown.series.as_deref(), Some("AAMkMadeUpSeriesreview"));
    let recurrence = shown.recurrence.as_deref().unwrap_or_default();
    assert!(
        recurrence.starts_with("Occurs every Wednesday starting "),
        "{recurrence}"
    );
    assert!(
        shown.join_url.as_deref().is_some_and(
            |link| link.starts_with("https://teams.microsoft.com/l/meetup-join/19%3ameeting_MadeUpMeeting")
        ),
        "{shown:#?}"
    );
    let attendee = |name: &str, address: &str, optional, response| AttendeeRow {
        name: Some(name.to_owned()),
        address: Some(address.to_owned()),
        optional,
        response: Some(response),
    };
    assert_eq!(
        shown.attendees,
        [
            attendee("Avery Example", "avery@example.com", false, EventAnswer::Accepted),
            attendee("Lab Account A", "a@example.com", false, EventAnswer::Accepted),
            attendee("Riley Placeholder", "riley@example.com", false, EventAnswer::Tentative),
            attendee("Casey Madeup", "casey@example.com", true, EventAnswer::NotResponded),
        ]
    );
    // the list's rows stay as they were
    let json = serde_json::to_value(&listed).unwrap();
    for detail in [
        "response",
        "attendees",
        "join_url",
        "series",
        "recurrence",
        "body",
        "organizer_address",
    ] {
        assert!(json.get(detail).is_none(), "{detail}: {json}");
    }
}

#[test]
fn join_meeting_opens_its_pre_join_and_sends_nothing() {
    let lab = Lab::start(true);
    let event = lab.event("Product all hands");
    assert_eq!(lab.ask(join_meeting(&event.id, false)).unwrap(), Response::Done);
    let prejoin = lab.cx.read(|cx| lab.calls().read(cx).prejoin().cloned()).unwrap();
    assert!(
        prejoin.to_join.chat().unwrap().starts_with("19:meeting_MadeUpMeeting"),
        "{prejoin:?}"
    );
    assert_eq!(prejoin.to_join.meeting().unwrap().organizer_id, ORGANIZER);
    assert_eq!(prejoin.to_join.title(), "Product all hands");
    assert!(!prejoin.joining && !prejoin.camera && !prejoin.muted, "{prejoin:?}");
    assert!(
        lab.traced.asked("join-meeting").is_empty(),
        "nothing was asked of the engine"
    );
}

#[test]
fn join_meeting_now_asks_the_pre_join_s_join_now_of_the_engine() {
    let lab = Lab::start(true);
    let event = lab.event("Product all hands");
    // a's engine has no calling: the join it was asked is refused there, and no call is set up
    let refused = lab.refused(join_meeting(&event.id, true));
    assert_eq!(refused.code, ErrorCode::NoCalling, "{refused:?}");
    let asked = lab.traced.asked("join-meeting");
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(asked[0].starts_with("19:meeting_MadeUpMeeting"), "{asked:?}");
    let calls = lab.calls();
    lab.cx.read(|cx| {
        let calls = calls.read(cx);
        assert!(calls.call().is_none() && !calls.busy());
        assert_eq!(
            calls.meeting_to_join().map(teams_ui::calls::MeetingToJoin::title),
            Some("Product all hands")
        );
    });
}

#[test]
fn an_event_without_a_meeting_is_not_joined_either_way() {
    let lab = Lab::start(true);
    let release = lab.event("Release day");
    for now in [false, true] {
        let refused = lab.refused(join_meeting(&release.id, now));
        assert_eq!(refused.code, ErrorCode::NoMeeting, "{refused:?}");
        assert!(refused.message.contains("Release day"), "{}", refused.message);
    }
    assert!(lab.cx.read(|cx| lab.calls().read(cx).prejoin().is_none()));
    assert!(lab.traced.asked("join-meeting").is_empty());
}

#[test]
fn an_unknown_or_blank_event_is_not_joined_either_way() {
    let lab = Lab::start(true);
    for now in [false, true] {
        let refused = lab.refused(join_meeting("AAMkNoSuchEvent", now));
        assert_eq!(refused.code, ErrorCode::UnknownEvent, "{refused:?}");
        assert_eq!(
            refused.message,
            "no event AAMkNoSuchEvent in the calendar: `ngteamsctl calendar` lists them"
        );
        assert_eq!(lab.refused(join_meeting(" ", now)).code, ErrorCode::UnknownEvent);
    }
    assert!(lab.cx.read(|cx| lab.calls().read(cx).prejoin().is_none()));
    assert!(lab.traced.asked("join-meeting").is_empty());
}

#[test]
fn an_app_without_calls_lists_no_event_joinable_and_joins_none() {
    let lab = Lab::start(false);
    let events = lab.week();
    assert!(events.iter().any(|event| event.online), "{events:#?}");
    assert!(events.iter().all(|event| !event.joinable), "{events:#?}");
    let event = lab.event("Product all hands");
    assert_eq!(lab.refused(join_meeting(&event.id, false)).code, ErrorCode::NoCalling);
    assert_eq!(lab.refused(join_meeting(&event.id, true)).code, ErrorCode::NoCalling);
}

/// A made-up event of an attendee's copy, read from `entry`'s fields over a plain one.
fn full_event(entry: &serde_json::Value, body_html: Option<&str>, optional: &[&str]) -> teams_core::FullEvent {
    let mut fields = serde_json::json!({
        "objectId": "AAMkMadeUpCopy",
        "subject": "Copy",
        "startTime": "2026-11-02T10:00:00Z",
        "endTime": "2026-11-02T10:30:00Z",
        "organizerName": "Riley Placeholder",
        "organizerAddress": "Riley@example.com",
        "attendees": [
            {"name": "Riley Placeholder", "address": "riley@example.com", "status": {"response": "Organizer"}},
            {"name": "Avery Example", "address": "Avery@example.com", "status": {"response": "Accepted"}},
            {"address": "casey@example.com", "type": "Optional", "status": {"response": "Declined"}},
        ],
    });
    for (key, value) in entry.as_object().unwrap() {
        fields[key] = value.clone();
    }
    teams_core::FullEvent {
        event: teams_core::CalendarEvent::stored(&fields.to_string()).unwrap(),
        raw: serde_json::Map::new(),
        body_html: body_html.map(str::to_owned),
        reminder_minutes: None,
        categories: Vec::new(),
        optional: optional.iter().map(|address| (*address).to_owned()).collect(),
        recurrence: None,
    }
}

#[test]
fn an_attendee_s_copy_shows_no_answers_of_others_and_its_preview_for_an_empty_text() {
    use ngteams::ctl::calendar::shown;
    let copy = full_event(
        &serde_json::json!({"myResponseType": "None", "eventType": "Occurrence", "bodyPreview": " Made-up preview "}),
        Some("<div> </div>"),
        &["avery@example.com"],
    );
    let row = shown(&copy, None, true);
    assert_eq!(row.response, None, "no answer asked");
    assert_eq!(row.body.as_deref(), Some("Made-up preview"));
    assert_eq!(row.recurrence.as_deref(), Some("Part of a series"));
    assert_eq!(row.organizer_address.as_deref(), Some("Riley@example.com"));
    let attendees: Vec<(Option<&str>, bool)> = row
        .attendees
        .iter()
        .map(|attendee| (attendee.address.as_deref(), attendee.optional))
        .collect();
    assert_eq!(
        attendees,
        [(Some("Avery@example.com"), true), (Some("casey@example.com"), true)],
        "the organizer left out, the optional ones by type or by the event's list"
    );
    assert!(row.attendees.iter().all(|attendee| attendee.response.is_none()));
    for (answer, expected) in [
        ("NotResponded", Some(EventAnswer::NotResponded)),
        ("Declined", Some(EventAnswer::Declined)),
        ("Organizer", Some(EventAnswer::Organizer)),
        ("SomethingNew", None),
    ] {
        let copy = full_event(&serde_json::json!({"myResponseType": answer}), None, &[]);
        let row = shown(&copy, Some("Occurs every day".to_owned()), false);
        assert_eq!(row.response, expected, "{answer}");
        assert_eq!(row.body, None);
        assert_eq!(row.recurrence.as_deref(), Some("Occurs every day"));
    }
    let organizer = full_event(
        &serde_json::json!({"isOrganizer": true, "organizerAddress": "", "myResponseType": "Organizer"}),
        Some("<p>Line one</p><p>Line two</p>"),
        &[],
    );
    let row = shown(&organizer, None, false);
    assert_eq!(row.body.as_deref(), Some("Line one\nLine two"));
    assert_eq!(row.recurrence, None);
    let answers: Vec<Option<EventAnswer>> = row.attendees.iter().map(|attendee| attendee.response).collect();
    assert_eq!(
        answers,
        [
            Some(EventAnswer::Organizer),
            Some(EventAnswer::Accepted),
            Some(EventAnswer::Declined)
        ],
        "without the organizer's address, nobody is left out"
    );
}
