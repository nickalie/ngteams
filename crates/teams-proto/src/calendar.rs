//! The account's calendar, read as the web client reads it for its calendar and its meeting cards: the middle tier's
//! calendar view, `GET <middle tier>/v2.0/me/calendars/default/calendarView?StartDate=<ISO>&EndDate=<ISO>&shouldDecryptData=true`
//! ([`CalendarView`]), the window's ends in UTC with milliseconds (`2026-10-26T00:00:00.000Z`), with the account's
//! token for `https://api.spaces.skype.com` (`docs/protocol/calendar.md`).
//!
//! Its answer is `{"type": "Microsoft.SkypeSpaces.MiddleTier.Models.CalendarEvent", "value": [<event>…]}`, every event
//! in the window, recurring ones as their occurrences; an event's fields are the web client's own select list
//! (`objectId`, `subject`, `startTime`, `endTime`, `isAllDayEvent`, `eventType`, `isCancelled`, `isOnlineMeeting`,
//! `skypeTeamsMeetingUrl`, `skypeTeamsData`, `organizerName`, `myResponseType`, `showAs`, …). Only an empty answer was
//! captured (the lab accounts' calendars hold nothing), so an event is read leniently: a field in a form it doesn't
//! take is left out, an event without a readable id, start or end is skipped and counted, never guessed.

use crate::meeting::MeetingCode;
use crate::write::MeetingInfo;
use percent_encoding::percent_decode_str;
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::{Month, OffsetDateTime};

use crate::endpoint::{Endpoint, Service};
use crate::read::Read;
use crate::recurrence::Recurrence;

/// The calendar view's path under the middle-tier base.
pub const CALENDAR_VIEW_PATH: [&str; 5] = ["v2.0", "me", "calendars", "default", "calendarView"];

/// The widest window one read asks for: 62 days (a month view's six weeks fit with room to spare).
pub const MAX_CALENDAR_WINDOW_MS: i64 = 62 * 24 * 3_600_000;

/// `GET <middle tier>/v2.0/me/calendars/default/calendarView?StartDate&EndDate&shouldDecryptData=true`: the account's
/// events between two instants. Its answer is read with [`CalendarView::read_answer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalendarView {
    start: String,
    end: String,
}

impl CalendarView {
    /// The read of the events between `start_ms` and `end_ms` (ms since the epoch); `None` when the window is empty,
    /// reversed, wider than [`MAX_CALENDAR_WINDOW_MS`] or out of range.
    #[must_use]
    pub fn between(start_ms: i64, end_ms: i64) -> Option<Self> {
        if end_ms <= start_ms || end_ms.checked_sub(start_ms)? > MAX_CALENDAR_WINDOW_MS {
            return None;
        }
        Some(Self {
            start: iso_utc(start_ms)?,
            end: iso_utc(end_ms)?,
        })
    }

    /// The events the answer `body` gives, in its order, a recurring series' master left out (its occurrences come
    /// as events of their own).
    ///
    /// # Errors
    /// [`CalendarAnswerError::Malformed`] unless the body is a JSON object with a `value` array.
    pub fn read_answer(body: &[u8]) -> Result<CalendarAnswer, CalendarAnswerError> {
        Self::read_answer_in(&crate::Cloud::Microsoft, body)
    }

    /// Reads with the account's service origin.
    ///
    /// # Errors
    /// Returns the same parse errors as the Microsoft reader.
    pub fn read_answer_in(cloud: &crate::Cloud, body: &[u8]) -> Result<CalendarAnswer, CalendarAnswerError> {
        let answer: Value = serde_json::from_slice(body).map_err(|_| CalendarAnswerError::Malformed)?;
        let entries = answer
            .get("value")
            .and_then(Value::as_array)
            .ok_or(CalendarAnswerError::Malformed)?;
        let mut read = CalendarAnswer::default();
        for entry in entries {
            match CalendarEvent::of(cloud, entry) {
                Some(event) if event.kind == EventKind::RecurringMaster => {}
                Some(event) => {
                    read.events.push(event);
                    read.entries.push(entry.to_string());
                }
                None => read.skipped += 1,
            }
        }
        Ok(read)
    }
}

impl Read for CalendarView {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(CALENDAR_VIEW_PATH)
            .with_query("StartDate", self.start.as_str())
            .with_query("EndDate", self.end.as_str())
            .with_query("shouldDecryptData", "true")
    }
}

/// Why a calendar answer wasn't taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CalendarAnswerError {
    #[error("the calendar's answer isn't in the shape the web client reads")]
    Malformed,
}

/// What a calendar read gave.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CalendarAnswer {
    pub events: Vec<CalendarEvent>,
    /// Each of `events` as the answer gave it (its JSON object, as text), in the same order: what the native client
    /// keeps of an event between runs, read again with [`CalendarEvent::stored`].
    pub entries: Vec<String>,
    /// Events left out: no string `objectId`, or a `startTime` or `endTime` that doesn't read.
    pub skipped: usize,
}

/// When an event starts or ends, as the answer gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventTime {
    /// An instant, the answer naming its offset: ms since the epoch.
    Instant(i64),
    /// A date and time with no offset, which the web client reads as the viewer's local time (a JavaScript `Date`
    /// parses such a string so): the view places it in its own time zone.
    Floating(FloatingTime),
}

/// A date and time without an offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FloatingTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// An event's kind, its `eventType`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    Single,
    /// One occurrence of a recurring series.
    Occurrence,
    /// An occurrence of a recurring series changed on its own.
    Exception,
    /// A recurring series itself.
    RecurringMaster,
    /// A kind no capture or bundle showed, as it came (or empty when there was none).
    Other(String),
}

impl EventKind {
    /// Whether the event belongs to a recurring series (Teams marks it with its recurring icon).
    #[must_use]
    pub fn recurring(&self) -> bool {
        matches!(self, Self::Occurrence | Self::Exception | Self::RecurringMaster)
    }
}

/// The account's answer to an invitation, its `myResponseType`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Response {
    /// The account organizes the meeting.
    Organizer,
    Accepted,
    Tentative,
    Declined,
    NotResponded,
    /// `None`: no answer is asked for (an appointment of the account's own).
    NoResponse,
    Other(String),
}

/// How the event shows the account's time, its `showAs`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ShowAs {
    Free,
    Tentative,
    Busy,
    /// Out of office.
    Away,
    WorkingElsewhere,
    Other(String),
}

/// Someone an event invites.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Attendee {
    pub name: Option<String>,
    pub address: Option<String>,
    /// Their answer (`status.response`), when the answer carries it: an event read whole gives it to its organizer.
    pub response: Option<Response>,
    /// Invited as optional (`type` `Optional`).
    pub optional: bool,
}

/// One event of the account's calendar.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a flag of its own the event's answer carries (`isAllDayEvent`, `isCancelled`, \
              `isOnlineMeeting`, `isOrganizer`, `isPrivate`)"
)]
pub struct CalendarEvent {
    /// Its `objectId`.
    pub id: String,
    /// Its subject (empty when it has none).
    pub subject: String,
    pub start: EventTime,
    pub end: EventTime,
    pub all_day: bool,
    pub kind: EventKind,
    pub cancelled: bool,
    /// A Teams meeting (`isOnlineMeeting`, or a join link).
    pub online: bool,
    /// The meeting's join link (`skypeTeamsMeetingUrl`, else `joinOnlineMeetingUrl`), an `https` link only.
    pub join_url: Option<String>,
    /// The meeting's chat (`19:meeting_…@thread.v2`), from `skypeTeamsData`'s `cid` or the join link's path.
    pub meeting_chat: Option<String>,
    /// The meeting's organizer and tenant, as its join link's `context` names them: with its chat, what joining it
    /// takes, started or not.
    pub meeting: Option<MeetingInfo>,
    /// The meeting's id and passcode, as a short join link carries them (`shortOnlineMeetingJoinUrl`, or the join link
    /// itself): what "Join with an ID" finds the meeting by.
    pub meeting_code: Option<MeetingCode>,
    pub location: Option<String>,
    pub organizer_name: Option<String>,
    pub organizer_address: Option<String>,
    pub is_organizer: bool,
    pub response: Response,
    pub show_as: ShowAs,
    pub private: bool,
    /// The start of its body as text (`bodyPreview`), when the answer has one.
    pub preview: Option<String>,
    /// The meeting's dial-in conference id (`onlineMeetingConferenceId`).
    pub conference_id: Option<String>,
    pub attendees: Vec<Attendee>,
    /// The series an occurrence or exception belongs to (`seriesMasterId`): the event a change of the whole series
    /// reads and writes.
    pub series_master_id: Option<String>,
    /// The channel's meeting it is, when it was scheduled in a channel (its `skypeTeamsData` names a team's thread).
    pub channel_meeting: Option<ChannelEventRef>,
}

/// A channel's meeting as an event names it (`docs/protocol/calling.md`, "Schedule a meeting"): its `skypeTeamsData`'s
/// `cid` the channel, `uid` who scheduled it and `mid` its post there, and its `iCalUID`, by which its team's calendar
/// holds it. Its organizer's own calendar holds it as an attendee's copy (`isOrganizer` `false`, the team its
/// organizer), so who scheduled it is `uid`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ChannelEventRef {
    /// The channel's thread.
    pub channel: String,
    /// Who scheduled it, by MRI.
    pub organizer: Option<String>,
    /// Its post in the channel.
    pub post: Option<String>,
    /// Its id on the team's calendar.
    pub ical_uid: String,
}

impl ChannelEventRef {
    /// The channel's meeting `entry` (an event as the calendar reads it) is, if it is one.
    fn of(entry: &Value) -> Option<Self> {
        let data: Value = serde_json::from_str(entry.get("skypeTeamsData")?.as_str()?).ok()?;
        let channel = data.get("cid")?.as_str().filter(|cid| crate::is_team_thread_id(cid))?;
        let ical_uid = entry
            .get("iCalUID")?
            .as_str()
            .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit()))?;
        let post = match data.get("mid") {
            Some(Value::Number(number)) => Some(number.to_string()),
            Some(Value::String(text)) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => {
                Some(text.clone())
            }
            _ => None,
        };
        Some(Self {
            channel: channel.to_owned(),
            organizer: data
                .get("uid")
                .and_then(Value::as_str)
                .filter(|uid| !uid.is_empty())
                .map(str::to_owned),
            post,
            ical_uid: ical_uid.to_owned(),
        })
    }

    /// Whether the account `mri` scheduled it.
    #[must_use]
    pub fn organized_by(&self, mri: &str) -> bool {
        self.organizer
            .as_deref()
            .is_some_and(|organizer| organizer.eq_ignore_ascii_case(mri))
    }
}

impl CalendarEvent {
    /// The event a creation's answer gives (`{"value": [<event>]}` as captured, `{"value": <event>}`, or the event
    /// itself), a series' master too; `None` when it doesn't read.
    #[must_use]
    pub fn created(body: &[u8]) -> Option<Self> {
        Self::created_in(&crate::Cloud::Microsoft, body)
    }

    /// Reads with the account's service origin.
    #[must_use]
    pub fn created_in(cloud: &crate::Cloud, body: &[u8]) -> Option<Self> {
        Self::of(cloud, &created_entry(body)?)
    }

    /// The new event a creation's answer gives ([`Self::created`]) and its entry as text, as the calendar view's
    /// answer keeps its events ([`CalendarAnswer::entries`]): what the store keeps of it. `None` when it doesn't read,
    /// or is a series' master (the calendar view gives a series as its occurrences).
    #[must_use]
    pub fn created_kept(body: &[u8]) -> Option<(Self, String)> {
        Self::created_kept_in(&crate::Cloud::Microsoft, body)
    }

    /// Reads with the account's service origin.
    #[must_use]
    pub fn created_kept_in(cloud: &crate::Cloud, body: &[u8]) -> Option<(Self, String)> {
        let entry = created_entry(body)?;
        let event = Self::of(cloud, &entry).filter(|event| event.kind != EventKind::RecurringMaster)?;
        Some((event, entry.to_string()))
    }

    /// The event a kept answer's entry gives ([`CalendarAnswer::entries`]), read as the answer was; `None` when it
    /// doesn't read, or is a series' master.
    #[must_use]
    pub fn stored(entry: &str) -> Option<Self> {
        Self::stored_in(&crate::Cloud::Microsoft, entry)
    }

    /// Reads with the account's service origin.
    #[must_use]
    pub fn stored_in(cloud: &crate::Cloud, entry: &str) -> Option<Self> {
        let entry: Value = serde_json::from_str(entry).ok()?;
        Self::of(cloud, &entry).filter(|event| event.kind != EventKind::RecurringMaster)
    }

    /// The event `entry` describes; `None` without a string `objectId` or a readable `startTime` and `endTime`.
    fn of(cloud: &crate::Cloud, entry: &Value) -> Option<Self> {
        let text = |key: &str| {
            entry
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let flag = |key: &str| entry.get(key).and_then(Value::as_bool).unwrap_or(false);
        let id = text("objectId")?;
        let start = event_time(entry.get("startTime")?.as_str()?)?;
        let end = event_time(entry.get("endTime")?.as_str()?)?;
        let join_url = text("skypeTeamsMeetingUrl")
            .or_else(|| text("joinOnlineMeetingUrl"))
            .filter(|url| crate::meeting::meeting_link_in(cloud, url));
        let meeting_chat = text("skypeTeamsData")
            .and_then(|data| teams_data_chat(&data))
            .or_else(|| join_url.as_deref().and_then(|url| join_url_chat_in(cloud, url)));
        Some(Self {
            id,
            subject: text("subject").unwrap_or_default(),
            start,
            end,
            all_day: flag("isAllDayEvent"),
            kind: event_kind(text("eventType").as_deref()),
            cancelled: flag("isCancelled"),
            online: flag("isOnlineMeeting") || join_url.is_some(),
            meeting: join_url.as_deref().and_then(|url| join_url_meeting_in(cloud, url)),
            meeting_code: [
                "shortOnlineMeetingJoinUrl",
                "joinOnlineMeetingUrl",
                "skypeTeamsMeetingUrl",
            ]
            .iter()
            .find_map(|key| text(key).and_then(|url| MeetingCode::of_link_in(cloud, &url))),
            join_url,
            meeting_chat,
            location: text("location").or_else(|| location_name(entry.get("location"))),
            organizer_name: text("organizerName"),
            organizer_address: text("organizerAddress"),
            is_organizer: flag("isOrganizer"),
            response: response(text("myResponseType").as_deref()),
            show_as: show_as(text("showAs").as_deref()),
            private: flag("isPrivate"),
            preview: text("bodyPreview"),
            conference_id: text("onlineMeetingConferenceId"),
            attendees: attendees(entry.get("attendees")),
            series_master_id: text("seriesMasterId"),
            channel_meeting: ChannelEventRef::of(entry),
        })
    }
}

/// The event a creation's answer gives: `{"value": [<event>]}` as captured, `{"value": <event>}`, or the event itself.
fn created_entry(body: &[u8]) -> Option<Value> {
    let answer: Value = serde_json::from_slice(body).ok()?;
    match answer.get("value") {
        Some(Value::Array(events)) => events.first().cloned(),
        Some(event @ Value::Object(_)) => Some(event.clone()),
        _ => Some(answer),
    }
}

/// `ms` as the window's ends are written: UTC, milliseconds, `Z` (the free/busy reads' too).
pub(crate) fn iso_utc(ms: i64) -> Option<String> {
    let time = OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()?;
    Some(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        time.year(),
        u8::from(time.month()),
        time.day(),
        time.hour(),
        time.minute(),
        time.second(),
        time.millisecond()
    ))
}

/// An event's start or end: an RFC 3339 instant, or an ISO date and time without an offset (any fraction digits).
fn event_time(text: &str) -> Option<EventTime> {
    let text = text.trim();
    if let Ok(time) = OffsetDateTime::parse(text, &Rfc3339) {
        return i64::try_from(time.unix_timestamp_nanos() / 1_000_000)
            .ok()
            .map(EventTime::Instant);
    }
    floating(text).map(EventTime::Floating)
}

/// `YYYY-MM-DDTHH:MM[:SS[.fraction]]` with no offset, the fraction dropped.
fn floating(text: &str) -> Option<FloatingTime> {
    let (date, clock) = text.split_once(['T', ' '])?;
    let mut date = date.split('-');
    let year = date.next()?.parse().ok()?;
    let month: u8 = date.next()?.parse().ok()?;
    let day: u8 = date.next()?.parse().ok()?;
    let max_day = Month::try_from(month).ok()?.length(year);
    let clock = clock.split_once('.').map_or(clock, |(whole, fraction)| {
        if fraction.bytes().all(|byte| byte.is_ascii_digit()) {
            whole
        } else {
            ""
        }
    });
    let mut clock = clock.split(':');
    let hour: u8 = clock.next()?.parse().ok()?;
    let minute: u8 = clock.next()?.parse().ok()?;
    let second: u8 = clock.next().map_or(Some(0), |second| second.parse().ok())?;
    let valid = date.next().is_none()
        && clock.next().is_none()
        && (1..=max_day).contains(&day)
        && hour < 24
        && minute < 60
        && second < 61;
    valid.then_some(FloatingTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
    })
}

fn event_kind(kind: Option<&str>) -> EventKind {
    match kind {
        Some("Single") => EventKind::Single,
        Some("Occurrence") => EventKind::Occurrence,
        Some("Exception") => EventKind::Exception,
        Some("RecurringMaster") => EventKind::RecurringMaster,
        other => EventKind::Other(other.unwrap_or_default().to_owned()),
    }
}

fn response(response: Option<&str>) -> Response {
    match response {
        Some("Organizer") => Response::Organizer,
        Some("Accepted") => Response::Accepted,
        Some("Tentative" | "TentativelyAccepted") => Response::Tentative,
        Some("Declined") => Response::Declined,
        Some("NotResponded") => Response::NotResponded,
        Some("None") => Response::NoResponse,
        other => Response::Other(other.unwrap_or_default().to_owned()),
    }
}

fn show_as(show_as: Option<&str>) -> ShowAs {
    match show_as {
        Some("Free") => ShowAs::Free,
        Some("Tentative") => ShowAs::Tentative,
        Some("Busy") => ShowAs::Busy,
        Some("OOF" | "Oof") => ShowAs::Away,
        Some("WorkingElsewhere") => ShowAs::WorkingElsewhere,
        other => ShowAs::Other(other.unwrap_or_default().to_owned()),
    }
}

/// A location given as an object rather than a string: its `displayName`.
fn location_name(location: Option<&Value>) -> Option<String> {
    location?
        .get("displayName")?
        .as_str()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

/// The attendees `attendees` lists, each with a name or an address (a plain one, or an `emailAddress` object's).
fn attendees(attendees: Option<&Value>) -> Vec<Attendee> {
    let Some(attendees) = attendees.and_then(Value::as_array) else {
        return Vec::new();
    };
    attendees
        .iter()
        .filter_map(|attendee| {
            let inner = attendee.get("emailAddress").unwrap_or(attendee);
            let text = |keys: &[&str]| {
                keys.iter()
                    .find_map(|key| inner.get(*key).and_then(Value::as_str))
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned)
            };
            let name = text(&["name", "displayName"]);
            let address = text(&["address", "smtpAddress", "email"]);
            let answer = attendee
                .get("status")
                .and_then(|status| status.get("response"))
                .and_then(Value::as_str)
                .map(|answer| response(Some(answer)));
            let optional = attendee.get("type").and_then(Value::as_str) == Some("Optional");
            (name.is_some() || address.is_some()).then_some(Attendee {
                name,
                address,
                response: answer,
                optional,
            })
        })
        .collect()
}

/// The meeting chat `skypeTeamsData` (a JSON object as a string) names by its `cid`.
fn teams_data_chat(data: &str) -> Option<String> {
    let data: Value = serde_json::from_str(data).ok()?;
    data.get("cid")
        .and_then(Value::as_str)
        .filter(|cid| is_meeting_thread(cid))
        .map(str::to_owned)
}

/// The meeting chat a join link names: `https://teams.microsoft.com/l/meetup-join/<thread, percent-encoded>/…`.
#[must_use]
pub fn join_url_chat(url: &str) -> Option<String> {
    join_url_chat_in(&crate::Cloud::Microsoft, url)
}

/// Reads a join URL within this account's cloud.
#[must_use]
pub fn join_url_chat_in(cloud: &crate::Cloud, url: &str) -> Option<String> {
    if !cloud.is_microsoft() && !crate::meeting::meeting_link_in(cloud, url) {
        return None;
    }
    let parsed;
    let rest = if cloud.is_microsoft() {
        url.split_once("/l/meetup-join/")?.1
    } else {
        parsed = url::Url::parse(url).ok()?;
        parsed.path().strip_prefix("/l/meetup-join/")?
    };
    let segment = rest.split(['/', '?', '#']).next()?;
    let thread = percent_decode_str(segment).decode_utf8().ok()?;
    is_meeting_thread(&thread).then(|| thread.into_owned())
}

/// The organizer and tenant a join link's `context` names: `…?context={"Tid":"<tenant>","Oid":"<object id>"}`,
/// percent-encoded. `None` for a link without them (a short `/meet/<code>` one).
#[must_use]
pub fn join_url_meeting(url: &str) -> Option<MeetingInfo> {
    join_url_meeting_in(&crate::Cloud::Microsoft, url)
}

/// Reads a join URL within this account's cloud.
#[must_use]
pub fn join_url_meeting_in(cloud: &crate::Cloud, url: &str) -> Option<MeetingInfo> {
    if !cloud.is_microsoft() && !crate::meeting::meeting_link_in(cloud, url) {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    let (_, context) = parsed.query_pairs().find(|(key, _)| key == "context")?;
    let context: Value = serde_json::from_str(&context).ok()?;
    let id = |key: &str| {
        context
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_ascii_lowercase)
    };
    Some(MeetingInfo {
        organizer_id: id("Oid")?,
        tenant_id: id("Tid")?,
    })
}

/// A thread id a meeting's chat can have: `19:<id>@thread.<v2|skype|tacv2>`, no spaces or slashes.
fn is_meeting_thread(id: &str) -> bool {
    id.starts_with("19:")
        && ["@thread.v2", "@thread.skype", "@thread.tacv2"]
            .iter()
            .any(|suffix| id.ends_with(suffix))
        && !id.contains([' ', '/', '?', '#'])
}

/// `GET v2.0/me/calendars/events/<id>?shouldDecryptData=true`: one event, as the web client reads it before it opens
/// the scheduling form on it. Its answer is the event itself ([`EventRead::read_answer`]): what the form fills in from,
/// and what a change sends back with the form's fields over it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventRead {
    id: String,
}

impl EventRead {
    /// The read of the event `id`; `None` for an empty id.
    #[must_use]
    pub fn new(id: &str) -> Option<Self> {
        (!id.trim().is_empty()).then(|| Self { id: id.to_owned() })
    }

    /// The event the answer `body` is.
    ///
    /// # Errors
    /// [`CalendarAnswerError::Malformed`] unless it is an event object whose id is the one read (the answer may also
    /// wrap it in `value`).
    pub fn read_answer(&self, body: &[u8]) -> Result<FullEvent, CalendarAnswerError> {
        self.read_answer_in(&crate::Cloud::Microsoft, body)
    }

    /// Reads an event with meeting links restricted to the account's cloud.
    ///
    /// # Errors
    /// Returns an error when the event is malformed or has the wrong id.
    pub fn read_answer_in(&self, cloud: &crate::Cloud, body: &[u8]) -> Result<FullEvent, CalendarAnswerError> {
        let answer: Value = serde_json::from_slice(body).map_err(|_| CalendarAnswerError::Malformed)?;
        let raw = match answer.get("value") {
            Some(Value::Object(inner)) => inner.clone(),
            _ => answer.as_object().cloned().ok_or(CalendarAnswerError::Malformed)?,
        };
        let full = full_event(cloud, raw)?;
        if full.event.id != self.id {
            return Err(CalendarAnswerError::Malformed);
        }
        Ok(full)
    }
}

/// An event's answer, read whole ([`FullEvent`]).
fn full_event(cloud: &crate::Cloud, raw: serde_json::Map<String, Value>) -> Result<FullEvent, CalendarAnswerError> {
    let event = CalendarEvent::of(cloud, &Value::Object(raw.clone())).ok_or(CalendarAnswerError::Malformed)?;
    let body_html = raw
        .get("bodyContent")
        .or_else(|| raw.get("body").and_then(|body| body.get("content")))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let reminder_minutes = raw
        .get("isReminderSet")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        .then(|| raw.get("reminderMinutesBeforeStart").and_then(Value::as_u64))
        .flatten()
        .and_then(|minutes| u32::try_from(minutes).ok());
    let categories = raw
        .get("categories")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    let optional = raw
        .get("attendees")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|attendee| attendee.get("type").and_then(Value::as_str) == Some("Optional"))
                .filter_map(|attendee| {
                    let inner = attendee.get("emailAddress").unwrap_or(attendee);
                    inner.get("address").and_then(Value::as_str).map(str::to_lowercase)
                })
                .collect()
        })
        .unwrap_or_default();
    let recurrence = Recurrence::read(&raw);
    Ok(FullEvent {
        event,
        raw,
        body_html,
        reminder_minutes,
        categories,
        optional,
        recurrence,
    })
}

impl Read for EventRead {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v2.0", "me", "calendars", "events", self.id.as_str()]).with_query("shouldDecryptData", "true")
    }
}

/// `GET v2.0/groups/smtp:<team mailbox>/calendars/events/iCalUId/<iCalUid>?eventStartDateTime=&shouldDecryptData=true`:
/// a channel's meeting as its team's group calendar holds it, as the web client reads it before it opens the meeting's
/// details (its card's Edit, seen 2026-11-03). Its answer is the event itself, `isOrganizer` `true` and `myResponseType`
/// `Organizer` whoever reads it (the group organizes it; `skypeTeamsData`'s `uid` names who scheduled it), and what a
/// change sends back with the form's fields over it ([`crate::UpdateChannelEvent`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelEventRead {
    mailbox: String,
    ical_uid: String,
}

impl ChannelEventRead {
    /// The read of the event `ical_uid` (hex digits) on the group calendar of `mailbox` (one address); `None` for
    /// either in another form.
    #[must_use]
    pub fn new(mailbox: &str, ical_uid: &str) -> Option<Self> {
        let mailbox = mailbox.trim();
        let address = mailbox
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.') && !domain.contains('@'))
            && !mailbox.contains(['/', '?', '#', ' ', ',', ';', '"', '<', '>', '\\', '%']);
        let id = !ical_uid.is_empty() && ical_uid.chars().all(|c| c.is_ascii_hexdigit());
        (address && id).then(|| Self {
            mailbox: mailbox.to_owned(),
            ical_uid: ical_uid.to_owned(),
        })
    }

    /// The event the answer `body` is.
    ///
    /// # Errors
    /// [`CalendarAnswerError::Malformed`] unless it is an event object whose `iCalUID` is the one read.
    pub fn read_answer(&self, body: &[u8]) -> Result<FullEvent, CalendarAnswerError> {
        self.read_answer_in(&crate::Cloud::Microsoft, body)
    }

    /// Reads an event with meeting links restricted to the account's cloud.
    ///
    /// # Errors
    /// Returns an error when the event is malformed or has the wrong id.
    pub fn read_answer_in(&self, cloud: &crate::Cloud, body: &[u8]) -> Result<FullEvent, CalendarAnswerError> {
        let answer: Value = serde_json::from_slice(body).map_err(|_| CalendarAnswerError::Malformed)?;
        let raw = match answer.get("value") {
            Some(Value::Object(inner)) => inner.clone(),
            _ => answer.as_object().cloned().ok_or(CalendarAnswerError::Malformed)?,
        };
        let named = raw.get("iCalUID").and_then(Value::as_str);
        if !named.is_some_and(|named| named.eq_ignore_ascii_case(&self.ical_uid)) {
            return Err(CalendarAnswerError::Malformed);
        }
        full_event(cloud, raw)
    }
}

impl Read for ChannelEventRead {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn endpoint(&self) -> Endpoint {
        let group = format!("smtp:{}", self.mailbox);
        Endpoint::new([
            "v2.0",
            "groups",
            group.as_str(),
            "calendars",
            "events",
            "iCalUId",
            self.ical_uid.as_str(),
        ])
        .with_query("eventStartDateTime", "")
        .with_query("shouldDecryptData", "true")
    }
}

/// One event as [`EventRead`] gave it: read as the calendar reads its events, the whole answer kept, and what the
/// scheduling form shows beyond the calendar's fields.
#[derive(Clone, Debug, PartialEq)]
pub struct FullEvent {
    pub event: CalendarEvent,
    /// The answer as it came: a change sends it back with the form's fields over it.
    pub raw: serde_json::Map<String, Value>,
    /// Its text as HTML (`bodyContent`), when the answer has it.
    pub body_html: Option<String>,
    /// Minutes before its start its reminder goes off, when one is set.
    pub reminder_minutes: Option<u32>,
    pub categories: Vec<String>,
    /// The addresses (lower case) of the attendees it invites as optional.
    pub optional: Vec<String>,
    /// How it repeats, when the answer gives a recurrence that reads (a series' master; an occurrence may carry its
    /// series').
    pub recurrence: Option<Recurrence>,
}

/// `GET beta/me/calendarEvents/timeZoneSettingsWithOffset?timezone=<IANA name>`: the desktop's time zone as Exchange
/// names it, which the web client reads before it writes an event (`eventTimeZone`). Its answer is `{"timeZone",
/// "utcOffsetInMinutes", "local": {"timeZone", …}, "remote": {…}}`; the event carries `local`'s name
/// ([`TimeZoneSettings::read_answer`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeZoneSettings {
    zone: String,
}

impl TimeZoneSettings {
    /// The read for the IANA zone `zone` (`Europe/Istanbul`).
    #[must_use]
    pub fn new(zone: &str) -> Self {
        Self { zone: zone.to_owned() }
    }

    /// The zone's Exchange name: `local.timeZone`, else the top-level `timeZone`.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<String> {
        let answer: Value = serde_json::from_slice(body).ok()?;
        answer
            .get("local")
            .and_then(|local| local.get("timeZone"))
            .or_else(|| answer.get("timeZone"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    }
}

impl Read for TimeZoneSettings {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["beta", "me", "calendarEvents", "timeZoneSettingsWithOffset"])
            .with_query("timezone", self.zone.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_written_in_utc_with_milliseconds() {
        let view = CalendarView::between(1_792_962_000_000, 1_793_221_200_000).unwrap();
        let endpoint = view.endpoint();
        assert_eq!(endpoint.segments(), CALENDAR_VIEW_PATH);
        let query: Vec<(&str, &str)> = endpoint
            .query()
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        assert_eq!(
            query,
            [
                ("StartDate", "2026-10-25T21:00:00.000Z"),
                ("EndDate", "2026-10-28T21:00:00.000Z"),
                ("shouldDecryptData", "true"),
            ]
        );
        assert!(CalendarView::between(10, 10).is_none());
        assert!(CalendarView::between(0, MAX_CALENDAR_WINDOW_MS + 1).is_none());
    }

    #[test]
    fn times_read_with_or_without_an_offset() {
        assert_eq!(
            event_time("2026-10-26T14:00:00.0000000Z"),
            Some(EventTime::Instant(1_793_023_200_000))
        );
        assert_eq!(
            event_time("2026-10-26T14:00:00+02:00"),
            Some(EventTime::Instant(1_793_016_000_000))
        );
        assert_eq!(
            event_time("2026-10-26T14:30:00.0000000"),
            Some(EventTime::Floating(FloatingTime {
                year: 2026,
                month: 10,
                day: 26,
                hour: 14,
                minute: 30,
                second: 0
            }))
        );
        assert_eq!(event_time("tomorrow"), None);
        assert_eq!(event_time("2026-13-26T14:30:00"), None);
    }

    #[test]
    fn a_join_link_names_its_organizer_and_tenant() {
        let url = "https://teams.microsoft.com/l/meetup-join/19%3ameeting_MadeUpId0001%40thread.v2/0?context=%7b%22Tid%22%3a%22AAAAAAAA-0000-4000-8000-0000000000f1%22%2c%22Oid%22%3a%2200000000-0000-4000-8000-00000000000b%22%7d";
        assert_eq!(
            join_url_meeting(url),
            Some(MeetingInfo {
                organizer_id: "00000000-0000-4000-8000-00000000000b".to_owned(),
                tenant_id: "aaaaaaaa-0000-4000-8000-0000000000f1".to_owned(),
            })
        );
        for without in [
            "https://teams.microsoft.com/meet/1234567890?p=MadeUp",
            "https://teams.microsoft.com/l/meetup-join/19%3ameeting_MadeUpId0001%40thread.v2/0",
            "https://teams.microsoft.com/l/meetup-join/x/0?context=%7b%22Tid%22%3a%22t%22%7d",
            "https://teams.microsoft.com/l/meetup-join/x/0?context=notjson",
            "not a link",
        ] {
            assert_eq!(join_url_meeting(without), None, "{without}");
        }
    }

    #[test]
    fn a_join_link_names_its_meeting_chat() {
        let url = "https://teams.microsoft.com/l/meetup-join/19%3ameeting_MadeUpId0001%40thread.v2/0?context=%7b%7d";
        assert_eq!(join_url_chat(url).as_deref(), Some("19:meeting_MadeUpId0001@thread.v2"));
        assert_eq!(
            join_url_chat("https://teams.microsoft.com/l/chat/19%3aabc%40thread.v2/0"),
            None
        );
    }
}
