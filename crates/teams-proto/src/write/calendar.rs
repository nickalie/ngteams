//! Typed calendar event and online-meeting writes in the captured middle-tier protocol shapes.

use serde_json::{Map, Value, json};
use time::{OffsetDateTime, UtcOffset};

use super::Prepared;
use super::Wire;
use crate::endpoint::{Endpoint, Method, Service};
use crate::recurrence::Recurrence;
use crate::request::Request;

/// Someone an event invites: their address, their name, and whether they are required.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EventAttendee {
    pub address: String,
    pub name: Option<String>,
    /// Their MRI, when the picker knew it.
    pub mri: Option<String>,
    /// Required (`true`) or optional.
    pub required: bool,
}

impl EventAttendee {
    /// Its entry in an event's `attendees` (and in the free/busy reads' list of people).
    pub(crate) fn entry(&self) -> Value {
        let mut entry = json!({
            "address": self.address,
            "name": self.name.clone().unwrap_or_else(|| self.address.clone()),
            "role": "Admin",
            "type": if self.required { "Required" } else { "Optional" },
        });
        if let Some(mri) = &self.mri {
            entry["mri"] = json!(mri);
        }
        entry
    }
}

/// How the event shows the account's time (`showAs`), as the form's Show as offers it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum EventShowAs {
    Free,
    WorkingElsewhere,
    Tentative,
    #[default]
    Busy,
    /// Out of office.
    Away,
}

impl EventShowAs {
    /// Every choice, in the order the form lists them.
    pub const ALL: [Self; 5] = [
        Self::Free,
        Self::WorkingElsewhere,
        Self::Tentative,
        Self::Busy,
        Self::Away,
    ];

    /// Its `showAs` value.
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            Self::Free => "Free",
            Self::WorkingElsewhere => "WorkingElsewhere",
            Self::Tentative => "Tentative",
            Self::Busy => "Busy",
            Self::Away => "OOF",
        }
    }

    /// The choice a `showAs` value names; `None` for one the form doesn't offer.
    #[must_use]
    pub fn of(wire: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|show| show.wire() == wire)
    }
}

/// An instant with the offset of the time zone it is shown in: how the event's times are written
/// (`2026-10-31T01:30:00+03:00`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ZonedTime {
    /// Ms since the epoch.
    pub ms: i64,
    /// Minutes east of UTC.
    pub offset_minutes: i32,
}

impl ZonedTime {
    /// `2026-10-31T01:30:00+03:00`; `None` out of range.
    #[must_use]
    pub fn iso(self) -> Option<String> {
        let offset = UtcOffset::from_whole_seconds(self.offset_minutes.checked_mul(60)?).ok()?;
        let time = OffsetDateTime::from_unix_timestamp_nanos(i128::from(self.ms) * 1_000_000)
            .ok()?
            .to_offset(offset);
        let sign = if self.offset_minutes < 0 { '-' } else { '+' };
        let minutes = self.offset_minutes.unsigned_abs();
        Some(format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
            time.year(),
            u8::from(time.month()),
            time.day(),
            time.hour(),
            time.minute(),
            time.second(),
            minutes / 60,
            minutes % 60
        ))
    }
}

/// The account as the organizer of what it creates: its MRI, and its address and name when known.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Organizer {
    pub mri: String,
    pub address: Option<String>,
    pub name: Option<String>,
}

/// An event as the scheduling form makes it: what a new event's body, or a change's, carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventDraft {
    pub subject: String,
    pub start: ZonedTime,
    pub end: ZonedTime,
    /// The account's time zone as Exchange names it (`TurkeySt`), when known; else UTC's.
    pub time_zone: Option<String>,
    /// The zone its start is given in, by its IANA name (`Europe/London`), when the form chose one other than the
    /// desktop's (its start and end time zones): the engine reads its Exchange name for `time_zone` instead of the
    /// desktop's.
    pub zone: Option<String>,
    pub all_day: bool,
    pub organizer: Organizer,
    pub attendees: Vec<EventAttendee>,
    pub location: String,
    /// Its text, as HTML.
    pub body_html: String,
    /// A Teams meeting.
    pub online: bool,
    pub show_as: EventShowAs,
    /// Minutes before its start the reminder goes off; `None` for no reminder.
    pub reminder_minutes: Option<u32>,
    pub private: bool,
    /// Its categories, by name.
    pub categories: Vec<String>,
    /// How it repeats: a series (its creation makes the series' master), else a single event. A change of an
    /// occurrence leaves it `None` and keeps what the event read has.
    pub recurrence: Option<Recurrence>,
}

/// The label the web client gives a Teams meeting's join.
const JOIN_LABEL: &str = "Join Microsoft Teams Online Meeting";

impl EventDraft {
    /// The event's fields as the body's event carries them.
    fn fields(&self) -> Option<Map<String, Value>> {
        let mut organizer = Map::new();
        if let Some(address) = &self.organizer.address {
            organizer.insert("address".to_owned(), json!(address));
            organizer.insert("mri".to_owned(), json!(self.organizer.mri));
        }
        if let Some(name) = &self.organizer.name {
            organizer.insert("name".to_owned(), json!(name));
        }
        organizer.insert("role".to_owned(), json!("Admin"));
        organizer.insert("type".to_owned(), json!("Organizer"));
        let mut attendees = vec![Value::Object(organizer)];
        attendees.extend(self.attendees.iter().map(EventAttendee::entry));
        let mut fields = json!({
            "attendees": attendees,
            "doNotForward": false,
            "endTime": self.end.iso()?,
            "eventTimeZone": self.time_zone.clone().unwrap_or_else(|| "Utc".to_owned()),
            // what the form's creation names it: a series' master when it repeats (a change keeps the event's own)
            "eventType": if self.recurrence.is_some() { "RecurringMaster" } else { "Single" },
            "isAllDayEvent": self.all_day,
            "isAppointment": self.attendees.is_empty(),
            "isOrganizer": true,
            "isPrivate": self.private,
            "isReminderSet": self.reminder_minutes.is_some(),
            "isResponseRequested": true,
            "location": self.location,
            "primarySpokenLanguage": "en-us",
            "reminderMinutesBeforeStart": self.reminder_minutes.unwrap_or(0),
            "showAs": self.show_as.wire(),
            "startTime": self.start.iso()?,
            "subject": self.subject,
            "utcOffset": if self.time_zone.is_some() { self.start.offset_minutes } else { 0 },
        });
        let object = fields.as_object_mut()?;
        if !self.categories.is_empty() {
            object.insert("categories".to_owned(), json!(self.categories));
        }
        if !self.body_html.trim().is_empty() {
            object.insert("bodyContentType".to_owned(), json!("html"));
            object.insert("bodyContent".to_owned(), json!(self.body_html));
        }
        if let Some(address) = &self.organizer.address {
            object.insert("organizerAddress".to_owned(), json!(address));
        }
        if let Some(name) = &self.organizer.name {
            object.insert("organizerName".to_owned(), json!(name));
        }
        if let Some(recurrence) = &self.recurrence {
            object.insert("eventRecurrencePattern".to_owned(), recurrence.pattern.wire());
            object.insert("eventRecurrenceRange".to_owned(), recurrence.range_wire()?);
        }
        if self.online {
            object.insert("joinMeetingLabel".to_owned(), json!(JOIN_LABEL));
            object.insert("meetingTemplateId".to_owned(), json!("default"));
            let data = json!({"uid": self.organizer.mri, "type": 0, "private": true});
            object.insert("skypeTeamsData".to_owned(), json!(data.to_string()));
        }
        Some(fields.as_object()?.clone())
    }

    /// `event` (an event as it was read) with this draft's fields over it: what a change sends. The meeting's own
    /// fields (its join links, its chat) and the event's kind (`eventType`: an occurrence stays one, a series' master
    /// one) stay as they were read, and so does its recurrence unless the draft sets one (a series changed whole). A
    /// body or categories the draft cleared are written out empty, so the event's own don't stay; without a zone of its
    /// own the draft keeps the event's.
    #[must_use]
    pub fn over(&self, event: &Map<String, Value>) -> Option<Map<String, Value>> {
        let mut merged = event.clone();
        let mut fields = self.fields()?;
        for kept in ["skypeTeamsData", "eventType"] {
            if event.contains_key(kept) {
                fields.remove(kept);
            }
        }
        // without the account's zone the event keeps the one it was read with, rather than going to UTC
        if self.time_zone.is_none() && event.contains_key("eventTimeZone") {
            fields.remove("eventTimeZone");
            fields.remove("utcOffset");
        }
        if event.contains_key("categories") && !fields.contains_key("categories") {
            fields.insert("categories".to_owned(), json!([]));
        }
        if event.contains_key("bodyContent") && !fields.contains_key("bodyContent") {
            fields.insert("bodyContentType".to_owned(), json!("html"));
            fields.insert("bodyContent".to_owned(), json!(""));
        }
        merged.extend(fields);
        Some(merged)
    }
}

/// The middle tier's path to the account's events.
const EVENTS: [&str; 4] = ["v2.0", "me", "calendars", "events"];

/// `POST beta/me/calendarEvents/privateMeeting/schedulingService/create` with `{"isStreamEnabled": false,
/// "meetingType": "Scheduled"}`: an online meeting of the account's own (its join link and its chat, the organizer
/// alone in it), made before a Teams meeting's event, which then carries it. It reaches nobody: a plain request. The
/// service answers `201` with `{"value": <the meeting>}` ([`CreateOnlineMeeting::read_answer`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CreateOnlineMeeting;

impl CreateOnlineMeeting {
    /// The meeting the answer `body` gives, as the event's `schedulingServiceEvent` carries it: `None` unless it is an
    /// object with a join link.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<Value> {
        Self::read_answer_in(&crate::Cloud::Microsoft, body)
    }

    /// Reads meeting links within the account's cloud.
    #[must_use]
    pub fn read_answer_in(cloud: &crate::Cloud, body: &[u8]) -> Option<Value> {
        let answer: Value = serde_json::from_slice(body).ok()?;
        let meeting = answer.get("value")?;
        meeting
            .get("links")
            .and_then(|links| links.get("join"))
            .and_then(Value::as_str)
            .is_some_and(|url| cloud.is_microsoft() || crate::meeting::meeting_link_in(cloud, url))
            .then(|| meeting.clone())
    }
}

impl Request for CreateOnlineMeeting {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new([
            "beta",
            "me",
            "calendarEvents",
            "privateMeeting",
            "schedulingService",
            "create",
        ])
    }

    fn body(&self) -> Option<Value> {
        Some(json!({"isStreamEnabled": false, "meetingType": "Scheduled"}))
    }
}

/// `POST beta/me/calendarEvents/privateMeeting/schedulingService/create` with `{"meetingType": "MeetNow",
/// "isStreamEnabled": false, "subject", "unhideChatThread": true}`: a meeting of the account's own to start now (the
/// calendar's Meet now: Start meeting, or Get a link to share), its chat shown to the organizer alone and no event on
/// the calendar. It reaches nobody: a plain request. The service answers `201` with `{"value": {"subject",
/// "groupContext": {"threadId"}, "links": {"join", "shortJoinUrl", "update"}, …}}` ([`MeetNow::read_answer`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetNow {
    /// The meeting's name (the dialog's "Meeting name").
    pub subject: String,
}

/// A meeting Meet now made: its chat, its join link and the short link the dialog shares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetNowMeeting {
    /// Its chat (`groupContext.threadId`), what its pre-join joins by.
    pub thread: String,
    /// Its join link (`links.join`).
    pub join: String,
    /// Its short link (`links.shortJoinUrl`, `https://teams.microsoft.com/meet/<id>?p=<passcode>`), else the join link.
    pub share: String,
}

impl MeetNow {
    /// The meeting the answer `body` gives: `None` without a chat or a join link.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<MeetNowMeeting> {
        Self::read_answer_in(&crate::Cloud::Microsoft, body)
    }

    /// Reads meeting links within the account's cloud.
    #[must_use]
    pub fn read_answer_in(cloud: &crate::Cloud, body: &[u8]) -> Option<MeetNowMeeting> {
        let answer: Value = serde_json::from_slice(body).ok()?;
        let meeting = answer.get("value")?;
        let text = |value: Option<&Value>| {
            value
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let thread = text(meeting.get("groupContext").and_then(|context| context.get("threadId")))?;
        let links = meeting.get("links");
        let join = text(links.and_then(|links| links.get("join")))?;
        let share = text(links.and_then(|links| links.get("shortJoinUrl"))).unwrap_or_else(|| join.clone());
        if !cloud.is_microsoft()
            && (!crate::meeting::meeting_link_in(cloud, &join) || !crate::meeting::meeting_link_in(cloud, &share))
        {
            return None;
        }
        Some(MeetNowMeeting { thread, join, share })
    }
}

impl Request for MeetNow {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        CreateOnlineMeeting.endpoint()
    }

    fn body(&self) -> Option<Value> {
        Some(json!({
            "meetingType": "MeetNow",
            "isStreamEnabled": false,
            "subject": self.subject,
            "unhideChatThread": true,
        }))
    }
}

/// `POST v2.0/me/calendars/default/events?isOnlineMeeting=<bool>&shouldDecryptData=true` with `{"calendarEvent":
/// <event>, "schedulingServiceEvent"?: <online meeting>}`: a new event, its invitation sent to every attendee. The
/// service answers `201` with `{"value": [<event>]}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateEvent {
    draft: EventDraft,
    meeting: Option<Value>,
    transaction: String,
}

impl CreateEvent {
    /// The creation of `draft`, carrying `meeting` ([`CreateOnlineMeeting`]'s) for a Teams meeting.
    #[must_use]
    pub fn new(draft: EventDraft, meeting: Option<Value>) -> Self {
        Self {
            draft,
            meeting,
            transaction: transaction_id(),
        }
    }

    #[must_use]
    pub fn draft(&self) -> &EventDraft {
        &self.draft
    }
}

/// The web client's id of an event it makes (`clientOptimisticEventId`, `TransactionId`): five random 32-bit numbers
/// joined by `-`.
fn transaction_id() -> String {
    let mut parts = Vec::with_capacity(5);
    for _ in 0..5 {
        parts.push(getrandom::u32().unwrap_or_default().to_string());
    }
    parts.join("-")
}

impl Prepared for CreateEvent {
    fn wire(&self) -> Option<Wire> {
        let mut event = self.draft.fields()?;
        event.insert("clientOptimisticEventId".to_owned(), json!(self.transaction));
        event.insert("TransactionId".to_owned(), json!(self.transaction));
        let mut body = json!({ "calendarEvent": Value::Object(event) });
        if let Some(meeting) = self.meeting.as_ref().filter(|_| self.draft.online) {
            body["schedulingServiceEvent"] = meeting.clone();
        }
        Some(Wire {
            raw: None,
            service: Service::MiddleTier,
            method: Method::Post,
            endpoint: Endpoint::new(["v2.0", "me", "calendars", "default", "events"])
                .with_query(
                    "isOnlineMeeting",
                    bool_text(self.draft.online && self.meeting.is_some()),
                )
                .with_query("shouldDecryptData", "true"),
            headers: Vec::new(),
            body: Some(body),
        })
    }
}

/// The channel a channel's meeting is posted to and the team it reaches: the channel's thread and the team's mailbox
/// (CSA's `smtpAddress`), whose group calendar holds the event.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MeetingChannel {
    /// The channel's thread (`19:…@thread.tacv2`).
    pub channel: String,
    /// The team's mail address.
    pub mailbox: String,
}

impl MeetingChannel {
    /// The channel with its mailbox trimmed; `None` for a channel that isn't a team's thread or a mailbox that isn't
    /// one address (nothing that would read as another path segment or recipient).
    #[must_use]
    pub fn checked(self) -> Option<Self> {
        let address = self.mailbox.trim();
        let mailbox_reads = address
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.') && !domain.contains('@'))
            && !address.contains(['/', '?', '#', ' ', ',', ';', '"', '<', '>', '\\', '%']);
        (crate::is_team_thread_id(&self.channel) && mailbox_reads).then(|| Self {
            mailbox: address.to_owned(),
            channel: self.channel,
        })
    }
}

/// What a channel's meeting says when it has no text of its own (`emptyBodyPlaceholder`), as the web client's says it.
const EMPTY_BODY: &str = "Scheduled a meeting";

/// `POST v2.0/groups/smtp:<team mailbox>/calendars/default/events?isOnlineMeeting=true&shouldDecryptData=true` with the
/// event's keys at the top level: a channel's meeting (its header's Schedule a meeting, `docs/protocol/calling.md`), a
/// Teams meeting on the team's group calendar, its attendees the organizer, the team's mailbox and whoever the form
/// invites, `skypeTeamsData` naming the channel, and with "Send personal invites" `inviteAllMembers: true`, as the web
/// client sent it (2026-11-03). The service posts it to the channel. A [`WriteKind::Meeting`] on [`Scope::TeamCalendar`], each attendee but the team by address. The service
/// answers `201` with `{"value": [<event>]}`, as a creation in the account's own calendar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateChannelEvent {
    draft: EventDraft,
    channel: MeetingChannel,
    /// "Send personal invites": the meeting goes to the personal calendars of the channel's members too
    /// (`inviteAllMembers`).
    personal_invites: bool,
    transaction: String,
}

impl CreateChannelEvent {
    /// The creation of `draft` (a Teams meeting whatever its switch says) in `channel`; `None` for a channel that isn't
    /// a team's thread or a mailbox that isn't an address.
    #[must_use]
    pub fn new(draft: EventDraft, channel: MeetingChannel) -> Option<Self> {
        Some(Self {
            draft,
            channel: channel.checked()?,
            personal_invites: false,
            transaction: transaction_id(),
        })
    }

    /// The same creation, its invitations sent to the personal calendars of the channel's members too ("Send personal
    /// invites", `inviteAllMembers: true`) when `personal_invites`.
    #[must_use]
    pub fn with_personal_invites(mut self, personal_invites: bool) -> Self {
        self.personal_invites = personal_invites;
        self
    }

    #[must_use]
    pub fn draft(&self) -> &EventDraft {
        &self.draft
    }

    #[must_use]
    pub fn channel(&self) -> &MeetingChannel {
        &self.channel
    }

    /// The draft as sent: a Teams meeting, the team's mailbox not among the form's attendees (it is listed once, as the
    /// team).
    fn sent(&self) -> EventDraft {
        let mut draft = self.draft.clone();
        draft.online = true;
        draft
            .attendees
            .retain(|attendee| !attendee.address.trim().eq_ignore_ascii_case(&self.channel.mailbox));
        draft
    }
}

impl Prepared for CreateChannelEvent {
    fn wire(&self) -> Option<Wire> {
        let draft = self.sent();
        let mut event = draft.fields()?;
        event.remove("isAppointment");
        if let Some(Value::Array(attendees)) = event.get_mut("attendees") {
            attendees.push(json!({"address": self.channel.mailbox, "role": "User", "type": "Required"}));
        }
        let data = json!({"cid": self.channel.channel, "uid": draft.organizer.mri, "type": 0});
        event.insert("skypeTeamsData".to_owned(), json!(data.to_string()));
        event.insert("mailboxAddress".to_owned(), json!(self.channel.mailbox));
        event.insert("sensitivityLabelId".to_owned(), json!(""));
        if !event.contains_key("bodyContent") {
            event.insert("emptyBodyPlaceholder".to_owned(), json!(EMPTY_BODY));
        }
        if self.personal_invites {
            event.insert("inviteAllMembers".to_owned(), json!(true));
        }
        event.insert("clientOptimisticEventId".to_owned(), json!(self.transaction));
        let group = format!("smtp:{}", self.channel.mailbox);
        Some(Wire {
            raw: None,
            service: Service::MiddleTier,
            method: Method::Post,
            endpoint: Endpoint::new(["v2.0", "groups", group.as_str(), "calendars", "default", "events"])
                .with_query("isOnlineMeeting", "true")
                .with_query("shouldDecryptData", "true"),
            headers: Vec::new(),
            body: Some(Value::Object(event)),
        })
    }
}

/// What a channel meeting's cancellation posts to the channel, `{0}` its title (`cancellationMessageFormat`), as the web
/// client's says it.
const CANCELLED_FORMAT: &str = "The meeting \"{0}\" is cancelled";

/// `PUT v2.0/groups/smtp:<team mailbox>/calendars/events/<iCalUid>/cancel` with `{"conversationId": <channel>,
/// "messageId": <post>, "rootMessageId": <post>, "cancellationMessageFormat", "fetchEventContract": {"eventType",
/// "eventTimeZone": "UTC", "startTime"}, "cancellationMessage"?: <note>}`: a channel's meeting cancelled by its
/// organizer, as the web client's Cancel meeting sent it (2026-11-03, held back): everyone on the team told, the
/// channel's post marked. A [`WriteKind::Meeting`] on [`Scope::TeamCalendar`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelChannelEvent {
    channel: MeetingChannel,
    ical_uid: String,
    post: String,
    event_type: String,
    start: String,
    message: Option<String>,
}

impl CancelChannelEvent {
    /// The cancellation of the meeting `meeting` posted as `post` in `channel`, `message` sent with it; `None` for a
    /// channel or mailbox [`MeetingChannel::checked`] refuses, or a post id that isn't digits.
    #[must_use]
    pub fn new(
        meeting: &crate::ScheduledMeeting,
        channel: MeetingChannel,
        post: String,
        message: Option<String>,
    ) -> Option<Self> {
        let channel = channel.checked()?;
        let start = OffsetDateTime::from_unix_timestamp_nanos(i128::from(meeting.start_ms) * 1_000_000).ok()?;
        let start = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.000Z",
            start.year(),
            u8::from(start.month()),
            start.day(),
            start.hour(),
            start.minute(),
            start.second()
        );
        let digits = !post.is_empty() && post.chars().all(|c| c.is_ascii_digit());
        digits.then(|| Self {
            channel,
            ical_uid: meeting.ical_uid.clone(),
            post,
            event_type: meeting.event_type.clone(),
            start,
            message: message.filter(|message| !message.trim().is_empty()),
        })
    }
}

impl Prepared for CancelChannelEvent {
    fn wire(&self) -> Option<Wire> {
        let mut body = json!({
            "conversationId": self.channel.channel,
            "messageId": self.post,
            "rootMessageId": self.post,
            "cancellationMessageFormat": CANCELLED_FORMAT,
            "fetchEventContract": {"eventType": self.event_type, "eventTimeZone": "UTC", "startTime": self.start},
        });
        if let Some(message) = &self.message {
            body["cancellationMessage"] = json!(message);
        }
        let group = format!("smtp:{}", self.channel.mailbox);
        Some(Wire {
            raw: None,
            service: Service::MiddleTier,
            method: Method::Put,
            endpoint: Endpoint::new([
                "v2.0",
                "groups",
                group.as_str(),
                "calendars",
                "events",
                self.ical_uid.as_str(),
                "cancel",
            ]),
            headers: Vec::new(),
            body: Some(body),
        })
    }
}

/// What a channel meeting's change posts to the channel (`updateMessageFormat`), as the web client's says it.
const UPDATED_FORMAT: &str = "The meeting has been updated";

/// The keys of a channel's meeting, as its group calendar's read gives them ([`crate::ChannelEventRead`]), that its
/// change sends back as they came unless the form sets them, as the web client's Send update did (2026-11-03, held
/// back): the rest of the read (its last change, its preview, the conflicts, the meeting's locations) stays out.
const CHANNEL_EVENT_KEPT: [&str; 22] = [
    "objectId",
    "iCalUID",
    "cleanGlobalObjectId",
    "eventType",
    "isOnlineMeeting",
    "isOrganizer",
    "myResponseType",
    "categories",
    "isResponseRequested",
    "isReminderSet",
    "reminderMinutesBeforeStart",
    "showAs",
    "bodyContentType",
    "bodyContent",
    "doNotForward",
    "inviteAllMembers",
    "meetingTemplateId",
    "schedulingServiceUpdateUrl",
    "shortOnlineMeetingJoinUrl",
    "skypeTeamsData",
    "skypeTeamsMeetingUrl",
    "location",
];

/// `PUT v2.0/groups/smtp:<team mailbox>/calendars/events/<iCalUid>?isOnlineMeeting=<bool>` with
/// `{"updateMessageFormat", "updatedCalendarEvent": <the event as read, the form's fields over it>,
/// "fetchEventContract": {"eventType", "startTime": <its start as read, UTC>, "eventTimeZone": "UTC"}}`: a channel's
/// meeting changed by its organizer, its details' Send update, as the web client sent it (2026-11-03, held back);
/// everyone on the team told, the channel's post too. Its attendees are the organizer, the team's mailbox and whoever
/// else the form invites. A [`WriteKind::Meeting`] on [`Scope::TeamCalendar`], each attendee but the team by address,
/// as the creation's ([`CreateChannelEvent`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateChannelEvent {
    channel: MeetingChannel,
    ical_uid: String,
    event: Map<String, Value>,
    draft: EventDraft,
    transaction: String,
}

impl UpdateChannelEvent {
    /// The change of the channel's meeting `event` (read whole from its team's calendar) to `draft`; `None` for a
    /// channel or mailbox [`MeetingChannel::checked`] refuses, or an event read without its `iCalUID` or that names
    /// another channel.
    #[must_use]
    pub fn new(channel: MeetingChannel, event: Map<String, Value>, draft: EventDraft) -> Option<Self> {
        let channel = channel.checked()?;
        let ical_uid = event
            .get("iCalUID")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit()))?
            .to_owned();
        let named = event
            .get("skypeTeamsData")
            .and_then(Value::as_str)
            .and_then(|data| serde_json::from_str::<Value>(data).ok())
            .and_then(|data| data.get("cid").and_then(Value::as_str).map(str::to_owned));
        (named.as_deref() == Some(channel.channel.as_str())).then(|| Self {
            channel,
            ical_uid,
            event,
            draft,
            transaction: transaction_id(),
        })
    }

    /// The draft as sent: a Teams meeting, neither the team's mailbox nor the organizer among the form's attendees
    /// (each is listed once, as itself).
    fn sent(&self) -> EventDraft {
        let mut draft = self.draft.clone();
        draft.online = true;
        let organizer = draft.organizer.address.clone().unwrap_or_default();
        draft.attendees.retain(|attendee| {
            let address = attendee.address.trim();
            !address.eq_ignore_ascii_case(&self.channel.mailbox) && !address.eq_ignore_ascii_case(organizer.trim())
        });
        draft
    }

    /// How the service finds the event: its kind and its start as read, in UTC.
    fn contract(&self) -> Option<Value> {
        let start = self.event.get("startTime").and_then(Value::as_str)?;
        let start = OffsetDateTime::parse(start.trim(), &time::format_description::well_known::Rfc3339).ok()?;
        let start = i64::try_from(start.unix_timestamp_nanos() / 1_000_000).ok()?;
        let kind = self.event.get("eventType").and_then(Value::as_str).unwrap_or("Single");
        Some(json!({
            "eventType": kind,
            "startTime": crate::calendar::iso_utc(start)?,
            "eventTimeZone": "UTC",
        }))
    }
}

impl Prepared for UpdateChannelEvent {
    fn wire(&self) -> Option<Wire> {
        let kept: Map<String, Value> = self
            .event
            .iter()
            .filter(|(key, _)| CHANNEL_EVENT_KEPT.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let mut event = self.sent().over(&kept)?;
        for dropped in ["isAppointment", "primarySpokenLanguage", "joinMeetingLabel"] {
            event.remove(dropped);
        }
        if let Some(Value::Array(attendees)) = event.get_mut("attendees") {
            attendees.push(json!({"address": self.channel.mailbox, "role": "User", "type": "Required"}));
        }
        event.insert("mailboxAddress".to_owned(), json!(self.channel.mailbox));
        event.insert("clientOptimisticEventId".to_owned(), json!(self.transaction));
        let group = format!("smtp:{}", self.channel.mailbox);
        Some(Wire {
            raw: None,
            service: Service::MiddleTier,
            method: Method::Put,
            endpoint: Endpoint::new([
                "v2.0",
                "groups",
                group.as_str(),
                "calendars",
                "events",
                self.ical_uid.as_str(),
            ])
            .with_query("isOnlineMeeting", "true"),
            headers: Vec::new(),
            body: Some(json!({
                "updateMessageFormat": UPDATED_FORMAT,
                "updatedCalendarEvent": Value::Object(event),
                "fetchEventContract": self.contract()?,
            })),
        })
    }
}

/// `PUT v2.0/me/calendars/events/<id>?isOnlineMeeting=<bool>` with `{"updatedCalendarEvent": <event>}`: the event
/// changed, its update sent to every attendee. The event is the one read with the draft over it
/// ([`EventDraft::over`]); the service answers `204`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateEvent {
    id: String,
    draft: EventDraft,
    event: Map<String, Value>,
}

impl UpdateEvent {
    /// The change of the event `id`, read as `event`, to `draft`; `None` when `event` names another id.
    #[must_use]
    pub fn new(id: String, event: Map<String, Value>, draft: EventDraft) -> Option<Self> {
        match event.get("objectId") {
            None | Some(Value::Null) => {}
            Some(Value::String(object_id)) if *object_id == id => {}
            Some(_) => return None,
        }
        (!id.is_empty()).then_some(Self { id, draft, event })
    }

    /// Whether the event as read was a Teams meeting: a change keeps it one.
    fn online(&self) -> bool {
        self.event
            .get("isOnlineMeeting")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || self.draft.online
    }
}

impl Prepared for UpdateEvent {
    fn wire(&self) -> Option<Wire> {
        let event = self.draft.over(&self.event)?;
        Some(Wire {
            raw: None,
            service: Service::MiddleTier,
            method: Method::Put,
            endpoint: Endpoint::new(EVENTS.into_iter().chain([self.id.as_str()]))
                .with_query("isOnlineMeeting", bool_text(self.online())),
            headers: Vec::new(),
            body: Some(json!({ "updatedCalendarEvent": Value::Object(event) })),
        })
    }
}

/// `PUT v2.0/me/calendars/events/<id>/cancel` with `{"conversationId"?: <meeting chat>, "cancellationMessage"?:
/// <text>}`: an organizer's meeting cancelled, its attendees told; the service answers `204`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelEvent {
    id: String,
    chat: Option<String>,
    message: Option<String>,
}

impl CancelEvent {
    /// The cancellation of the event `id`, naming its meeting chat when it has one, with `message` for its attendees.
    #[must_use]
    pub fn new(id: String, chat: Option<String>, message: Option<String>) -> Option<Self> {
        (!id.is_empty()).then_some(Self { id, chat, message })
    }
}

impl Prepared for CancelEvent {
    fn wire(&self) -> Option<Wire> {
        let mut body = Map::new();
        if let Some(chat) = &self.chat {
            body.insert("conversationId".to_owned(), Value::String(chat.clone()));
        }
        if let Some(message) = self.message.as_ref().filter(|message| !message.trim().is_empty()) {
            body.insert("cancellationMessage".to_owned(), Value::String(message.clone()));
        }
        Some(Wire {
            raw: None,
            service: Service::MiddleTier,
            method: Method::Put,
            endpoint: Endpoint::new(EVENTS.into_iter().chain([self.id.as_str(), "cancel"])),
            headers: Vec::new(),
            body: Some(Value::Object(body)),
        })
    }
}

/// `DELETE v2.0/me/calendars/events/<id>`: the event deleted from the account's calendar (an appointment, or a meeting
/// the account doesn't organize); the service answers `204`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteEvent {
    id: String,
}

impl DeleteEvent {
    #[must_use]
    pub fn new(id: String) -> Option<Self> {
        (!id.is_empty()).then_some(Self { id })
    }
}

impl Prepared for DeleteEvent {
    fn wire(&self) -> Option<Wire> {
        Some(Wire {
            raw: None,
            service: Service::MiddleTier,
            method: Method::Delete,
            endpoint: Endpoint::new(EVENTS.into_iter().chain([self.id.as_str()])),
            headers: Vec::new(),
            body: None,
        })
    }
}

/// An attendee's answer to a meeting's invitation, as the web client's RSVP sends it (`responseType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RsvpAnswer {
    /// Accept (`Accept`).
    Accept,
    /// Tentative (`TentativelyAccept`).
    Tentative,
    /// Decline (`Decline`): the event leaves the account's calendar.
    Decline,
}

impl RsvpAnswer {
    /// Its `responseType`.
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            Self::Accept => "Accept",
            Self::Tentative => "TentativelyAccept",
            Self::Decline => "Decline",
        }
    }
}

/// `PUT v2.0/me/calendars/events/<id>/respond` with `{"responseType", "sendResponse", "comment"?}`: the account's
/// answer to a meeting's invitation, for the occurrence `id` names or, named by its series' master, the whole series;
/// sent to the organizer when `send`, with `comment` as its note. The service answers `204`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RespondEvent {
    id: String,
    answer: RsvpAnswer,
    send: bool,
    comment: Option<String>,
}

impl RespondEvent {
    /// The answer `answer` to the event `id`'s invitation; `None` without an id.
    #[must_use]
    pub fn new(id: String, answer: RsvpAnswer, send: bool, comment: Option<String>) -> Option<Self> {
        (!id.is_empty()).then_some(Self {
            id,
            answer,
            send,
            comment,
        })
    }
}

impl Prepared for RespondEvent {
    fn wire(&self) -> Option<Wire> {
        let mut body = Map::new();
        body.insert("responseType".to_owned(), json!(self.answer.wire()));
        body.insert("sendResponse".to_owned(), json!(self.send));
        // the note goes only with an answer that is sent
        if let Some(comment) = self
            .comment
            .as_ref()
            .filter(|comment| self.send && !comment.trim().is_empty())
        {
            body.insert("comment".to_owned(), json!(comment));
        }
        Some(Wire {
            raw: None,
            service: Service::MiddleTier,
            method: Method::Put,
            endpoint: Endpoint::new(EVENTS.into_iter().chain([self.id.as_str(), "respond"])),
            headers: Vec::new(),
            body: Some(Value::Object(body)),
        })
    }
}

fn bool_text(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

#[cfg(test)]
mod tests {

    use super::*;

    fn draft() -> EventDraft {
        EventDraft {
            subject: "Planning".to_owned(),
            start: ZonedTime {
                ms: 1_793_372_400_000,
                offset_minutes: 180,
            },
            end: ZonedTime {
                ms: 1_793_374_200_000,
                offset_minutes: 180,
            },
            time_zone: Some("TurkeySt".to_owned()),
            zone: None,
            all_day: false,
            organizer: Organizer {
                mri: "8:orgid:00000000-0000-4000-8000-00000000000a".to_owned(),
                address: Some("bot-a@lab.example.com".to_owned()),
                name: Some("Bot A".to_owned()),
            },
            attendees: vec![EventAttendee {
                address: "bot-b@lab.example.com".to_owned(),
                name: Some("Bot B".to_owned()),
                mri: None,
                required: true,
            }],
            location: String::new(),
            body_html: String::new(),
            online: true,
            show_as: EventShowAs::Busy,
            reminder_minutes: Some(15),
            private: false,
            categories: Vec::new(),
            recurrence: None,
        }
    }

    fn object(value: &Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn each_event_constructor_builds_a_request() {
        let meeting = json!({"links": {"join": "https://teams.microsoft.com/l/meetup-join/x"}});
        let create = CreateEvent::new(draft(), Some(meeting));

        let event = object(&json!({"objectId": "AAMkEvent", "subject": "Old", "iCalUId": "kept"}));
        let update = UpdateEvent::new("AAMkEvent".to_owned(), event, draft()).expect("an update");

        let cancel = CancelEvent::new("AAMkEvent".to_owned(), Some("19:meeting_x@thread.v2".to_owned()), None)
            .expect("a cancellation");

        let delete = DeleteEvent::new("AAMkEvent".to_owned()).expect("a delete");
        assert!(
            create.wire().is_some() && update.wire().is_some() && cancel.wire().is_some() && delete.wire().is_some()
        );
    }

    #[test]
    fn an_answer_to_an_invitation_is_sent_as_the_web_client_sends_it() {
        let accept = RespondEvent::new(
            "AAMkEvent".to_owned(),
            RsvpAnswer::Accept,
            true,
            Some("See you there".to_owned()),
        )
        .expect("an answer");
        let wire = accept.wire().expect("a wire form");
        assert_eq!(wire.method(), Method::Put);
        assert_eq!(
            wire.endpoint().segments(),
            ["v2.0", "me", "calendars", "events", "AAMkEvent", "respond"]
        );
        assert_eq!(
            wire.body(),
            Some(&json!({"responseType": "Accept", "sendResponse": true, "comment": "See you there"}))
        );

        // an answer not sent carries no note; a blank note is left out
        let quiet = RespondEvent::new(
            "AAMkEvent".to_owned(),
            RsvpAnswer::Tentative,
            false,
            Some("unsent".to_owned()),
        )
        .expect("an answer");
        assert_eq!(
            quiet.wire().and_then(|wire| wire.body().cloned()),
            Some(json!({"responseType": "TentativelyAccept", "sendResponse": false}))
        );
        let blank = RespondEvent::new("AAMkEvent".to_owned(), RsvpAnswer::Decline, true, Some("  ".to_owned()))
            .expect("an answer");
        assert_eq!(
            blank.wire().and_then(|wire| wire.body().cloned()),
            Some(json!({"responseType": "Decline", "sendResponse": true}))
        );
        assert!(RespondEvent::new(String::new(), RsvpAnswer::Accept, true, None).is_none());
    }

    fn channel() -> MeetingChannel {
        MeetingChannel {
            channel: "19:0123456789abcdef0123456789abcdef@thread.tacv2".to_owned(),
            mailbox: "lab-team@lab.example.com".to_owned(),
        }
    }

    #[test]
    fn a_channel_s_meeting_is_written_to_its_team_s_calendar_as_the_web_client_writes_it() {
        let mut only_organizer = draft();
        only_organizer.attendees.clear();
        only_organizer.online = false;
        let create = CreateChannelEvent::new(only_organizer, channel()).expect("a creation");

        let wire = create.wire().expect("a wire form");
        assert_eq!(
            wire.endpoint().segments(),
            [
                "v2.0",
                "groups",
                "smtp:lab-team@lab.example.com",
                "calendars",
                "default",
                "events"
            ]
        );
        assert_eq!(
            wire.endpoint().query()[0],
            ("isOnlineMeeting".to_owned(), "true".to_owned())
        );
        let event = wire.body().expect("a body");
        assert!(event.get("calendarEvent").is_none() && event.get("isAppointment").is_none());
        assert_eq!(event["attendees"][0]["type"], "Organizer");
        assert_eq!(
            event["attendees"][1],
            json!({"address": "lab-team@lab.example.com", "role": "User", "type": "Required"})
        );
        assert_eq!(event["mailboxAddress"], "lab-team@lab.example.com");
        assert_eq!(event["emptyBodyPlaceholder"], "Scheduled a meeting");
        assert_eq!(event["joinMeetingLabel"], JOIN_LABEL);
        let data: Value = serde_json::from_str(event["skypeTeamsData"].as_str().expect("text")).expect("json");
        assert_eq!(
            data,
            json!({"cid": channel().channel, "uid": "8:orgid:00000000-0000-4000-8000-00000000000a", "type": 0})
        );
    }

    #[test]
    fn a_channel_s_meeting_adds_whom_it_invites_and_its_personal_invites() {
        // whoever else the form invites is added, the team's mailbox never twice
        let mut more = draft();
        more.attendees.push(EventAttendee {
            address: "LAB-TEAM@lab.example.com".to_owned(),
            name: None,
            mri: None,
            required: true,
        });
        let create = CreateChannelEvent::new(more, channel()).expect("a creation");

        let body = create.wire().expect("a wire form").body().cloned().expect("a body");
        assert_eq!(body["attendees"].as_array().map(Vec::len), Some(3));
        assert!(create.wire().expect("wire").body().expect("body")["attendees"].is_array());
        assert!(body.get("inviteAllMembers").is_none());
        // Send personal invites
        let personal = CreateChannelEvent::new(draft(), channel())
            .expect("a creation")
            .with_personal_invites(true);

        assert_eq!(
            personal.wire().expect("a wire form").body().expect("a body")["inviteAllMembers"],
            true
        );
    }

    #[test]
    fn a_channel_s_meeting_needs_a_channel_and_a_mailbox() {
        let mut chat = channel();
        chat.channel = "19:meeting_x@thread.v2".to_owned();
        assert!(CreateChannelEvent::new(draft(), chat).is_none());
        for mailbox in ["", "team", "a/b@lab.example.com", "x@y"] {
            let mut odd = channel();
            odd.mailbox = mailbox.to_owned();
            assert!(CreateChannelEvent::new(draft(), odd).is_none(), "{mailbox}");
        }
    }

    #[test]
    fn a_channel_s_meeting_is_cancelled_on_its_team_s_calendar_as_the_web_client_cancels_it() {
        let post = json!({"meeting": json!({
            "@type": crate::SCHEDULED_MEETING_TYPE,
            "meetingtitle": "lab channel meeting",
            "scheduledmeetinginfo": {"startTime": "2026-11-03T02:00:00+00:00", "endTime": "2026-11-03T02:30:00+00:00",
                "iCalUid": "040000008200e00074c5b7101a82e008", "eventType": "Single"},
        }).to_string()});
        let meeting = crate::ScheduledMeeting::of_properties(&post.to_string()).expect("a meeting");
        let cancel = CancelChannelEvent::new(&meeting, channel(), "1793680050510".to_owned(), Some("note".to_owned()))
            .expect("a cancellation");

        let wire = cancel.wire().expect("a wire form");
        assert_eq!(
            wire.endpoint().segments(),
            [
                "v2.0",
                "groups",
                "smtp:lab-team@lab.example.com",
                "calendars",
                "events",
                "040000008200e00074c5b7101a82e008",
                "cancel"
            ]
        );
        let body = wire.body().expect("a body");
        assert_eq!(body["messageId"], "1793680050510");
        assert_eq!(body["rootMessageId"], "1793680050510");
        assert_eq!(body["fetchEventContract"]["startTime"], "2026-11-03T02:00:00.000Z");
        assert_eq!(body["cancellationMessage"], "note");
        // a post that isn't one, or no note
        assert!(CancelChannelEvent::new(&meeting, channel(), "x".to_owned(), None).is_none());
        let quiet = CancelChannelEvent::new(&meeting, channel(), "1".to_owned(), Some(" ".to_owned())).expect("one");
        assert!(
            quiet
                .wire()
                .expect("a wire form")
                .body()
                .expect("a body")
                .get("cancellationMessage")
                .is_none()
        );
    }

    /// A channel's meeting as its team's calendar gave it (made up, in the shape seen 2026-11-03).
    fn channel_event() -> Map<String, Value> {
        let data = json!({"cid": channel().channel, "rid": 1_793_680_050_510_u64, "mid": 1_793_680_050_510_u64,
            "uid": "8:orgid:00000000-0000-4000-8000-00000000000a", "private": false, "type": 0, "ct": null});
        object(&json!({
            "objectId": "AQMkMadeUp",
            "iCalUID": "040000008200e00074c5b7101a82e008",
            "cleanGlobalObjectId": "040000008200e00074c5b7101a82e008",
            "startTime": "2026-11-03T02:00:00+00:00",
            "endTime": "2026-11-03T02:30:00+00:00",
            "lastModifiedTime": "2026-11-03T04:28:37+00:00",
            "eventTimeZone": "TurkeySt",
            "eventType": "Single",
            "subject": "lab channel meeting",
            "location": "Microsoft Teams Meeting",
            "meetingLocations": [{"displayName": "Microsoft Teams Meeting"}],
            "isOnlineMeeting": true,
            "isOrganizer": true,
            "myResponseType": "Organizer",
            "organizerAddress": "lab-team@lab.example.com",
            "attendees": [
                {"type": "Organizer", "address": "lab-team@lab.example.com"},
                {"status": {"response": "Accepted"}, "address": "bot-a@lab.example.com"},
            ],
            "bodyContent": "<p>join block</p>",
            "bodyPreview": "join block",
            "conflictingMeetings": [],
            "skypeTeamsData": data.to_string(),
            "skypeTeamsDataObject": {"cid": channel().channel},
            "inviteAllMembers": false,
        }))
    }

    #[test]
    fn a_channel_s_meeting_is_changed_on_its_team_s_calendar_as_the_web_client_changes_it() {
        let mut changed = draft();
        changed.attendees.push(EventAttendee {
            address: "lab-team@lab.example.com".to_owned(),
            name: None,
            mri: None,
            required: true,
        });
        changed.subject = "lab channel meeting edited".to_owned();
        let update = UpdateChannelEvent::new(channel(), channel_event(), changed).expect("a change");

        let wire = update.wire().expect("a wire form");
        assert_eq!(
            wire.endpoint().segments(),
            [
                "v2.0",
                "groups",
                "smtp:lab-team@lab.example.com",
                "calendars",
                "events",
                "040000008200e00074c5b7101a82e008"
            ]
        );
        assert_eq!(
            wire.endpoint().query(),
            [("isOnlineMeeting".to_owned(), "true".to_owned())]
        );
        let body = wire.body().expect("a body");
        assert_eq!(body["updateMessageFormat"], "The meeting has been updated");
        assert_eq!(
            body["fetchEventContract"],
            json!({"eventType": "Single", "startTime": "2026-11-03T02:00:00.000Z", "eventTimeZone": "UTC"})
        );
        let event = &body["updatedCalendarEvent"];
        assert_eq!(event["subject"], "lab channel meeting edited");
        assert_eq!(event["mailboxAddress"], "lab-team@lab.example.com");
        assert_eq!(event["iCalUID"], "040000008200e00074c5b7101a82e008");
        assert_eq!(event["skypeTeamsData"], channel_event()["skypeTeamsData"]);
        // the read's own parts stay out; the organizer, whom it invites and the team, once each
        for read_only in [
            "lastModifiedTime",
            "meetingLocations",
            "bodyPreview",
            "conflictingMeetings",
            "skypeTeamsDataObject",
        ] {
            assert!(event.get(read_only).is_none(), "{read_only}");
        }
        let addresses: Vec<&str> = event["attendees"]
            .as_array()
            .expect("attendees")
            .iter()
            .filter_map(|attendee| attendee["address"].as_str())
            .collect();
        assert_eq!(
            addresses,
            [
                "bot-a@lab.example.com",
                "bot-b@lab.example.com",
                "lab-team@lab.example.com"
            ]
        );
        // an event read of another channel, or without its calendar id, isn't changed
        let mut other = channel_event();
        other.insert("skypeTeamsData".to_owned(), json!(r#"{"cid":"19:other@thread.tacv2"}"#));
        assert!(UpdateChannelEvent::new(channel(), other, draft()).is_none());
        let mut unnamed = channel_event();
        unnamed.remove("iCalUID");
        assert!(UpdateChannelEvent::new(channel(), unnamed, draft()).is_none());
    }

    #[test]
    fn a_meet_now_s_post_is_titled_as_the_web_client_titles_it() {
        let title = crate::SetMeetingTitle::new(&channel().channel, "1793684261870", " lab subject ").expect("a title");
        // a chat service write: on a regional chat service host

        let wire = title.wire().expect("a wire form");
        assert_eq!(wire.endpoint().query(), [("name".to_owned(), "meeting".to_owned())]);
        assert_eq!(wire.body(), Some(&json!({"meeting": {"meetingtitle": "lab subject"}})));
        // a chat's post, a post that isn't one, or no title
        assert!(crate::SetMeetingTitle::new("19:meeting_x@thread.v2", "1", "t").is_none());
        assert!(crate::SetMeetingTitle::new(&channel().channel, "x", "t").is_none());
        assert!(crate::SetMeetingTitle::new(&channel().channel, "1", " ").is_none());
    }

    #[test]
    fn a_calendar_event_names_the_channel_meeting_it_is() {
        let event = crate::CalendarEvent::created(&serde_json::to_vec(&Value::Object(channel_event())).expect("bytes"))
            .expect("an event");
        let channel_meeting = event.channel_meeting.clone().expect("a channel's meeting");
        assert_eq!(channel_meeting.channel, channel().channel);
        assert_eq!(channel_meeting.post.as_deref(), Some("1793680050510"));
        assert!(channel_meeting.organized_by("8:orgid:00000000-0000-4000-8000-00000000000A"));
        let (meeting, post) = crate::ScheduledMeeting::of_event(&event).expect("its meeting");
        assert_eq!(post, "1793680050510");
        assert_eq!(meeting.ical_uid, "040000008200e00074c5b7101a82e008");
        assert_eq!(meeting.start_text, "2026-11-03T02:00:00.000Z");
        let read = crate::ChannelEventRead::new("lab-team@lab.example.com", "040000008200e00074c5b7101a82e008")
            .expect("a read");
        let body = serde_json::to_vec(&Value::Object(channel_event())).expect("bytes");
        assert!(read.read_answer(&body).is_ok());
        let other = crate::ChannelEventRead::new("lab-team@lab.example.com", "0400ff").expect("a read");
        assert!(other.read_answer(&body).is_err(), "another event's answer");
        assert!(crate::ChannelEventRead::new("not a mailbox", "0400").is_none());
        // a Teams meeting of a chat is no channel's
        let mut chat = channel_event();
        chat.insert(
            "skypeTeamsData".to_owned(),
            json!(r#"{"cid":"19:meeting_x@thread.v2"}"#),
        );
        let chat =
            crate::CalendarEvent::created(&serde_json::to_vec(&Value::Object(chat)).expect("bytes")).expect("one");
        assert!(chat.channel_meeting.is_none());
    }

    #[test]
    fn a_new_event_is_written_as_the_web_client_writes_it() {
        let create = CreateEvent::new(draft(), None);
        let wire = create.wire().expect("a wire form");
        let event = &wire.body().expect("a body")["calendarEvent"];
        assert_eq!(event["startTime"], "2026-10-30T18:00:00+03:00");
        assert_eq!(event["eventTimeZone"], "TurkeySt");
        assert_eq!(event["utcOffset"], 180);
        assert_eq!(event["attendees"][0]["type"], "Organizer");
        assert_eq!(event["attendees"][1]["type"], "Required");
        assert_eq!(event["isAppointment"], false);
        assert_eq!(event["clientOptimisticEventId"], event["TransactionId"]);
        // a Teams meeting without its online meeting goes as an event alone
        assert_eq!(
            wire.endpoint().query()[0],
            ("isOnlineMeeting".to_owned(), "false".to_owned())
        );
    }

    #[test]
    fn a_series_is_created_as_its_master_with_its_recurrence() {
        use crate::recurrence::{Pattern, Weekday};
        let mut series = draft();
        series.recurrence = Some(Recurrence {
            pattern: Pattern::Weekly {
                interval: 1,
                days: vec![Weekday::Friday],
            },
            start: ZonedTime {
                ms: 1_793_307_600_000,
                offset_minutes: 180,
            },
            end: None,
        });
        let create = CreateEvent::new(series.clone(), None);
        let wire = create.wire().expect("a wire form");
        let event = &wire.body().expect("a body")["calendarEvent"];
        assert_eq!(event["eventType"], "RecurringMaster");
        assert_eq!(event["eventRecurrencePattern"]["patternType"], 2);
        assert_eq!(event["eventRecurrenceRange"]["startDate"], "2026-10-30T00:00:00+03:00");

        // a change of one occurrence keeps its kind and the series' recurrence as read
        let occurrence = object(&json!({"objectId": "AAMkOne", "eventType": "Occurrence",
            "eventRecurrencePattern": {"patternType": 1, "daily": {"interval": 1}}}));
        let merged = draft().over(&occurrence).expect("merged");
        assert_eq!(merged["eventType"], "Occurrence");
        assert_eq!(merged["eventRecurrencePattern"]["patternType"], 1);
        let master = object(&json!({"objectId": "AAMkSeries", "eventType": "RecurringMaster"}));
        let merged = series.over(&master).expect("merged");
        assert_eq!(merged["eventType"], "RecurringMaster");
        assert_eq!(merged["eventRecurrencePattern"]["weekly"]["daysOfTheWeek"], json!([5]));
    }

    #[test]
    fn a_change_keeps_what_the_form_doesn_t_edit() {
        let event = object(&json!({"objectId": "AAMkEvent", "subject": "Old", "iCalUId": "kept",
            "skypeTeamsData": "{\"cid\":\"19:meeting_x@thread.v2\"}"}));
        let merged = draft().over(&event).expect("merged");
        assert_eq!(merged["iCalUId"], "kept");
        assert_eq!(merged["subject"], "Planning");
        assert_eq!(merged["skypeTeamsData"], "{\"cid\":\"19:meeting_x@thread.v2\"}");
        let other = object(&json!({"objectId": "Another"}));
        assert!(UpdateEvent::new("AAMkEvent".to_owned(), other, draft()).is_none());
    }

    #[test]
    fn a_change_clears_a_body_and_categories_the_form_emptied() {
        let event = object(&json!({"objectId": "AAMkEvent", "bodyContentType": "html",
            "bodyContent": "<p>Old notes</p>", "categories": ["Red category"]}));
        let merged = draft().over(&event).expect("merged");
        assert_eq!(merged["bodyContent"], "");
        assert_eq!(merged["categories"], json!([]));
        // kept as the form has them when it has them
        let mut filled = draft();
        filled.body_html = "<p>New notes</p>".to_owned();
        filled.categories = vec!["Blue category".to_owned()];
        let merged = filled.over(&event).expect("merged");
        assert_eq!(merged["bodyContent"], "<p>New notes</p>");
        assert_eq!(merged["categories"], json!(["Blue category"]));
        // an event without them gets none
        let bare = object(&json!({"objectId": "AAMkEvent"}));
        let merged = draft().over(&bare).expect("merged");
        assert!(!merged.contains_key("bodyContent"));
        assert!(!merged.contains_key("categories"));
    }

    #[test]
    fn a_change_without_the_account_s_zone_keeps_the_event_s() {
        let event = object(&json!({"objectId": "AAMkEvent", "eventTimeZone": "TurkeySt", "utcOffset": 180}));
        let mut unzoned = draft();
        unzoned.time_zone = None;
        let merged = unzoned.over(&event).expect("merged");
        assert_eq!(merged["eventTimeZone"], "TurkeySt");
        assert_eq!(merged["utcOffset"], 180);
        let merged = draft().over(&event).expect("merged");
        assert_eq!(merged["eventTimeZone"], "TurkeySt");
    }

    #[test]
    fn the_online_meeting_s_answer_is_its_value() {
        let body = br#"{"value": {"links": {"join": "https://teams.microsoft.com/l/meetup-join/x"}, "etag": "e"}}"#;
        assert_eq!(
            CreateOnlineMeeting::read_answer(body).map(|meeting| meeting["etag"].clone()),
            Some(json!("e"))
        );
        assert!(CreateOnlineMeeting::read_answer(br#"{"value": {}}"#).is_none());
    }

    #[test]
    fn meet_now_makes_a_meeting_of_its_own_and_reads_its_chat_and_links() {
        let meet = MeetNow {
            subject: "Meeting with A".to_owned(),
        };
        assert_eq!(meet.method(), Method::Post);
        assert_eq!(meet.endpoint(), CreateOnlineMeeting.endpoint());
        assert_eq!(
            meet.body(),
            Some(
                json!({"meetingType": "MeetNow", "isStreamEnabled": false, "subject": "Meeting with A",
                "unhideChatThread": true})
            )
        );
        let body =
            json!({"value": {"subject": "Meeting with A", "groupContext": {"threadId": "19:meeting_x@thread.v2"},
            "links": {"join": "https://teams.microsoft.com/l/meetup-join/x", "shortJoinUrl":
            "https://teams.microsoft.com/meet/1?p=x", "update": "https://example.com/u"}}})
            .to_string();
        let made = MeetNow::read_answer(body.as_bytes()).unwrap();
        assert_eq!(made.thread, "19:meeting_x@thread.v2");
        assert_eq!(made.join, "https://teams.microsoft.com/l/meetup-join/x");
        assert_eq!(made.share, "https://teams.microsoft.com/meet/1?p=x");
        let no_short =
            json!({"value": {"groupContext": {"threadId": "19:t"}, "links": {"join": "https://j"}}}).to_string();
        assert_eq!(MeetNow::read_answer(no_short.as_bytes()).unwrap().share, "https://j");
        assert!(MeetNow::read_answer(br#"{"value": {"links": {"join": "https://j"}}}"#).is_none());
        assert!(MeetNow::read_answer(b"no").is_none());
    }
}
