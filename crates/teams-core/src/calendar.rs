//! The account's calendar: read when a view shows a window of it (the calendar's day, week or month), and written
//! when the scheduling form saves an event or its card cancels or deletes one (`docs/protocol/calendar.md`).
//!
//! - **Reads.** The middle tier's calendar view ([`CalendarView`]), one read per ask, answered with the window's
//!   events as soon as they come; the engine then keeps them in the store, replacing what it held of that window
//!   ([`Store::replace_calendar_window`]), so a later ask, in this run or a later one, is answered from the store at
//!   once ([`stored`]) while the view reads the window again, as Teams' own client shows its cached calendar while
//!   it syncs. A window that is empty, reversed or wider than [`teams_proto::MAX_CALENDAR_WINDOW_MS`] is answered with
//!   no events and nothing read. A read that failed, or an answer that isn't in the shape the web client reads, is
//!   [`EngineError::CalendarUnread`] and changes nothing stored. One event is read whole
//!   ([`EventRead`]) before the form opens on it (a series' master too, when the form changes a whole series). The
//!   form's Scheduler reads the attendees' busy times and the suggested times ([`MeetingAvailability`]): two reads sent as a
//!   `PUT`, which reach nobody.
//! - **Writes** ([`EventWrite`]): an event created, changed, cancelled or deleted, each a sealed `teams-proto` write
//!   through the outbox's calendar kind ([`Outbox::try_calendar`]): no row, nothing sent again by a later run; one more attempt only after a 401 or a short
//!   throttle ([`CallRetries`]), never after an unknown outcome (an invitation sent twice is worse than a failed save).
//!   A write that wasn't answered with a 2xx is [`EngineError::EventNotSaved`], saying why. Once a write is over, and
//!   before its asker hears of it, the engine settles what the store kept ([`settle_write`]): every kept copy of an
//!   event it touched ([`Touched`]: the event, a series' occurrences by their master, a channel's meeting by its
//!   team's id) is forgotten in every window, and a new event is kept, so no window drawn from the store afterwards
//!   shows what the write changed as it was; a window read that began before is answered but not kept (it may hold
//!   the event as it was). An outcome that isn't known settles the same way: the write may have been applied. A channel's meeting is
//!   created and cancelled on its team's group calendar ([`EventWrite::CreateInChannel`],
//!   [`EventWrite::CancelInChannel`]: `docs/protocol/calling.md`, "A channel's meeting").
//! - **A channel's running meeting** ([`find_channel_meetup`]): CSA's `activeMeetups`, else the channel's newest posts.

use std::sync::{Arc, Mutex, PoisonError};

use chrono::{Local, NaiveDate, TimeZone as _};
use serde_json::{Map, Value};
use teams_proto::Mri;
use teams_proto::chat::ChatReads;
use teams_proto::{
    Author, CalendarEvent, CalendarView, CancelChannelEvent, CancelEvent, ChannelEventRead, ChannelMeetup, Client,
    CreateChannelEvent, CreateEvent, CreateOnlineMeeting, Credentials, CsaTeams, DeleteEvent, EventAttendee,
    EventDraft, EventRead, EventTime, FreeBusyAsk, FullEvent, HttpError, HttpTransport, MeetNow, MeetNowMeeting,
    MeetingChannel, MeetingDetails, MeetingThread, MeetingTimes, Organizer, PersonSchedule, Request, RespondEvent,
    RsvpAnswer, ScheduledMeeting, Schedules, SetMeetingTitle, ShortProfileLookup, TimeZoneSettings, Transport,
    UpdateChannelEvent, UpdateEvent, ZonedTime,
};
use teams_store::{Store, StoreError, StoredEvent};
use tokio::task::JoinSet;

use crate::engine::{EngineError, Responder};
use crate::outbox::{CallRetries, CallTried, Outbox};

/// What the user asks of an event of the account's calendar.
#[derive(Clone, Debug, PartialEq)]
pub enum EventWrite {
    /// A new event: its invitation goes to every attendee.
    Create(EventDraft),
    /// A new meeting in a channel (its header's Schedule a meeting): on the team's group calendar, posted to the
    /// channel, reaching everyone on the team.
    CreateInChannel {
        draft: EventDraft,
        channel: MeetingChannel,
        /// "Send personal invites": in the personal calendars of the channel's members too.
        personal_invites: bool,
    },
    /// The event `id`, read whole as `event` ([`FullEvent::raw`]), changed to `draft`: its update goes to every
    /// attendee.
    Update {
        id: String,
        event: Map<String, Value>,
        draft: EventDraft,
    },
    /// A channel's meeting, read whole from its team's calendar as `event` ([`FullEvent::raw`]), changed to `draft` by
    /// its organizer: its update goes to everyone on the team, and to the channel's post.
    UpdateInChannel {
        event: Map<String, Value>,
        draft: EventDraft,
        channel: MeetingChannel,
    },
    /// The organizer's meeting `id` cancelled (its chat named when it has one), `message` sent with the cancellation.
    Cancel {
        id: String,
        chat: Option<String>,
        message: Option<String>,
    },
    /// The event `id` deleted from the account's calendar.
    Delete { id: String },
    /// The account's answer to the invitation of the event `id` (an occurrence, or a series' master for the whole
    /// series): sent to the organizer when `send`, with `comment` as its note.
    Respond {
        id: String,
        answer: RsvpAnswer,
        send: bool,
        comment: Option<String>,
    },
    /// A channel's meeting (`meeting`, as its post `post` in `channel` names it) cancelled by its organizer, `message`
    /// sent with the cancellation: everyone on the team told.
    CancelInChannel {
        meeting: Box<ScheduledMeeting>,
        channel: MeetingChannel,
        post: String,
        message: Option<String>,
    },
}

impl EventWrite {
    /// What it is, for the log and the errors.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Create(_) | Self::CreateInChannel { .. } => "creation",
            Self::Update { .. } | Self::UpdateInChannel { .. } => "change",
            Self::Cancel { .. } | Self::CancelInChannel { .. } => "cancellation",
            Self::Delete { .. } => "delete",
            Self::Respond { .. } => "answer",
        }
    }
}

/// Someone the attendee picker suggests: a name the account knows and the address their profile gives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invitable {
    pub mri: String,
    pub name: String,
    pub address: String,
}

/// What the scheduling form's Scheduler shows ([`Calendar::availability`]): each person's busy times (the account's
/// own among them) and the starts every attendee is free for the meeting's length.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MeetingAvailability {
    pub people: Vec<PersonSchedule>,
    /// Suggested starts, ms since the epoch, in the service's order.
    pub suggestions: Vec<i64>,
}

/// What a write gave: the new event, as the service answered its creation (`None` for any other write, or an answer
/// that didn't read).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventWritten {
    pub created: Option<CalendarEvent>,
}

/// The events of the account's calendar a write leaves the kept copies of out of date ([`EventWrite::touched`]): what
/// the store forgets once it is over ([`settle_write`]), and what the calendar drops from every span it holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Touched {
    /// Events by id: each one itself and, a series' master, every occurrence of its series (`seriesMasterId`).
    ids: Vec<String>,
    /// Channels' meetings by their id on their team's calendar (`iCalUID`): the account's own copy of each.
    ical_uids: Vec<String>,
}

impl Touched {
    /// Whether the write touched `event`.
    #[must_use]
    pub fn touches(&self, event: &CalendarEvent) -> bool {
        self.ids
            .iter()
            .any(|id| *id == event.id || event.series_master_id.as_deref() == Some(id.as_str()))
            || event.channel_meeting.as_ref().is_some_and(|meeting| {
                self.ical_uids
                    .iter()
                    .any(|uid| uid.eq_ignore_ascii_case(&meeting.ical_uid))
            })
    }

    /// Whether it touched nothing kept (a creation: nothing kept was the event's).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty() && self.ical_uids.is_empty()
    }

    fn ids(ids: impl IntoIterator<Item = String>) -> Self {
        Self {
            ids: ids.into_iter().filter(|id| !id.is_empty()).collect(),
            ical_uids: Vec::new(),
        }
    }

    fn ical_uids(uids: impl IntoIterator<Item = String>) -> Self {
        Self {
            ids: Vec::new(),
            ical_uids: uids.into_iter().filter(|uid| !uid.is_empty()).collect(),
        }
    }
}

impl EventWrite {
    /// The events whose kept copies it leaves out of date once it is applied ([`Touched`]): the event a change, a
    /// cancellation, a delete or an answer names (a series' master: the whole series), a channel's meeting changed
    /// or cancelled; none for a creation.
    #[must_use]
    pub fn touched(&self) -> Touched {
        match self {
            Self::Create(_) | Self::CreateInChannel { .. } => Touched::default(),
            Self::Update { id, .. } | Self::Cancel { id, .. } | Self::Delete { id } | Self::Respond { id, .. } => {
                Touched::ids([id.clone()])
            }
            Self::UpdateInChannel { event, .. } => {
                Touched::ical_uids(event.get("iCalUID").and_then(Value::as_str).map(str::to_owned))
            }
            Self::CancelInChannel { meeting, .. } => Touched::ical_uids([meeting.ical_uid.clone()]),
        }
    }
}

/// A window's read that came, for the engine to keep ([`Store::replace_calendar_window`]); its asker was answered
/// already.
pub(crate) struct WindowRead {
    pub(crate) start_ms: i64,
    pub(crate) end_ms: i64,
    pub(crate) events: Vec<StoredEvent>,
    /// How many writes were over ([`Calendar::writes_over`]) when it began: one that began before a write was over
    /// may hold what the write changed as it was, so it isn't kept.
    pub(crate) since: u64,
}

/// A write that is over, for the engine to settle what the store kept ([`settle_write`]) before answering its asker.
pub(crate) struct WriteOver {
    pub(crate) answer: Result<EventWritten, EngineError>,
    pub(crate) touched: Touched,
    /// The new event to keep, as its creation's answer gave it.
    pub(crate) kept: Option<StoredEvent>,
    pub(crate) reply: Responder<EventWritten>,
}

/// A window read or a write of the calendar that is over ([`Calendar::next`]; `None`: it failed, or its task was lost).
pub(crate) enum CalendarDone {
    Read(Option<WindowRead>),
    Written(Option<Box<WriteOver>>),
}

pub(crate) struct Calendar<C, T> {
    client: Arc<Client<HttpTransport<C>>>,
    outbox: Arc<Outbox<T>>,
    /// The window reads under way, each giving what to keep once it is over.
    reads: JoinSet<Option<WindowRead>>,
    /// The writes under way, each settled by the engine once it is over.
    writes: JoinSet<Box<WriteOver>>,
    /// How many writes were over: a window read that began before the last one isn't kept.
    writes_over: u64,
    /// The account as an event's organizer, its address and its time zone's name once read ([`Whom`]).
    whom: Arc<Mutex<Option<Whom>>>,
}

/// What an event the account writes says of the account: its address (its profile's) and its time zone as Exchange
/// names it, each kept once read; one that didn't read is asked again at the next write.
#[derive(Clone, Debug, Default)]
struct Whom {
    address: Option<String>,
    time_zone: Option<String>,
}

impl<C, T> Calendar<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>, outbox: Arc<Outbox<T>>) -> Self {
        Self {
            client,
            outbox,
            reads: JoinSet::new(),
            writes: JoinSet::new(),
            writes_over: 0,
            whom: Arc::new(Mutex::new(None)),
        }
    }

    /// Whether a window's read or a write runs.
    pub(crate) fn busy(&self) -> bool {
        !self.reads.is_empty() || !self.writes.is_empty()
    }

    /// The next window read or write that is over; never while none runs ([`Self::busy`]).
    pub(crate) async fn next(&mut self) -> CalendarDone {
        tokio::select! {
            read = self.reads.join_next(), if !self.reads.is_empty() => {
                CalendarDone::Read(read.and_then(Result::ok).flatten())
            }
            written = self.writes.join_next(), if !self.writes.is_empty() => {
                CalendarDone::Written(written.and_then(Result::ok))
            }
            else => std::future::pending().await,
        }
    }

    /// A write is over: a window read that began before it isn't kept ([`WindowRead::since`]).
    pub(crate) fn write_over(&mut self) {
        self.writes_over += 1;
    }

    /// How many writes were over.
    pub(crate) fn writes_over(&self) -> u64 {
        self.writes_over
    }

    /// Answers `reply` with the account's events between `start_ms` and `end_ms` as soon as the read is over; what it
    /// read comes from [`Self::next`] for the engine to keep.
    pub(crate) fn view(&mut self, start_ms: i64, end_ms: i64, reply: Responder<Vec<CalendarEvent>>) {
        let Some(view) = CalendarView::between(start_ms, end_ms) else {
            let _ = reply.send(Ok(Vec::new()));
            return;
        };
        let client = Arc::clone(&self.client);
        let since = self.writes_over;
        // on the engine's `LocalSet`: the read waits on nothing of the engine's loop
        self.reads.spawn_local(async move {
            match read(&client, &view).await {
                Ok(answer) => {
                    let events = kept(&answer.events, answer.entries);
                    let _ = reply.send(Ok(answer.events));
                    Some(WindowRead {
                        start_ms,
                        end_ms,
                        events,
                        since,
                    })
                }
                Err(error) => {
                    let _ = reply.send(Err(error));
                    None
                }
            }
        });
    }

    /// Answers `reply` with the event `id`, read whole.
    pub(crate) fn event(&self, id: &str, reply: Responder<FullEvent>) {
        let Some(read) = EventRead::new(id) else {
            let _ = reply.send(Err(EngineError::CalendarUnread));
            return;
        };
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let answer = match client.read(&read).await {
                Ok(answer) => read
                    .read_answer_in(client.transport().cloud(), answer.body())
                    .map_err(|error| {
                        log::info!("the event's answer wasn't taken: {error}");
                        EngineError::CalendarUnread
                    }),
                Err(error) => {
                    log::info!("the event couldn't be read: {error}");
                    Err(EngineError::CalendarUnread)
                }
            };
            let _ = reply.send(answer);
        });
    }

    /// Answers `reply` with a channel's meeting `ical_uid`, read whole from the group calendar of its team's `mailbox`
    /// ([`ChannelEventRead`]): what its details open on to change it.
    pub(crate) fn channel_event(&self, mailbox: &str, ical_uid: &str, reply: Responder<FullEvent>) {
        let Some(read) = ChannelEventRead::new(mailbox, ical_uid) else {
            let _ = reply.send(Err(EngineError::CalendarUnread));
            return;
        };
        let client = Arc::clone(&self.client);
        let whom = Arc::clone(&self.whom);
        let me = self.outbox.me().clone();
        tokio::task::spawn_local(async move {
            let answer = match client.read(&read).await {
                Ok(answer) => read
                    .read_answer_in(client.transport().cloud(), answer.body())
                    .map_err(|error| {
                        log::info!("the channel meeting's answer wasn't taken: {error}");
                        EngineError::CalendarUnread
                    }),
                Err(error) => {
                    log::info!("the channel's meeting couldn't be read: {error}");
                    Err(EngineError::CalendarUnread)
                }
            };
            let answer = match answer {
                Ok(full) => Ok(as_scheduler(full, &known(&client, &whom, &me).await, &me)),
                Err(error) => Err(error),
            };
            let _ = reply.send(answer);
        });
    }

    /// Titles the meeting the account's Meet now in `channel` starts ([`SetMeetingTitle`], as the web client titles it
    /// once the service posted it): the channel's newest posts are looked at until one starts a meeting the account
    /// organizes, posted no earlier than a minute before the ask, for up to [`TITLE_WAIT`]; `reply` is answered once it
    /// was sent, [`EngineError::MeetingUnread`] when no such post came.
    pub(crate) fn title_meet_now(&self, channel: String, title: String, reply: Responder<()>) {
        let client = Arc::clone(&self.client);
        let outbox = Arc::clone(&self.outbox);
        tokio::task::spawn_local(async move {
            let me = outbox
                .me()
                .mri()
                .as_str()
                .trim_start_matches("8:orgid:")
                .to_ascii_lowercase();
            let since = chrono::Utc::now().timestamp_millis() - 60_000;
            let deadline = tokio::time::Instant::now() + TITLE_WAIT;
            let answer = loop {
                let posted = ChatReads::new(&client)
                    .messages(&channel, POSTS_LOOKED_AT)
                    .await
                    .ok()
                    .and_then(|page| ChannelMeetup::of_posts_in(client.transport().cloud(), &page.messages))
                    .filter(|meetup| meetup.meeting.organizer_id.eq_ignore_ascii_case(&me))
                    .filter(|meetup| meetup.message_id.parse::<i64>().is_ok_and(|id| id >= since));
                if let Some(meetup) = posted {
                    break match SetMeetingTitle::new(&channel, &meetup.message_id, &title) {
                        Some(write) => send(&outbox, &write, "title").await.map(|_| ()),
                        None => Err(not_saved("title", "the title or the post doesn't read")),
                    };
                }
                if tokio::time::Instant::now() >= deadline {
                    log::info!("no post of the channel's new meeting came to title");
                    break Err(EngineError::MeetingUnread);
                }
                tokio::time::sleep(TITLE_POLL).await;
            };
            let _ = reply.send(answer);
        });
    }

    /// Answers `reply` with the meeting the chat `chat`'s thread names ([`MeetingThread`]): `None` when it names
    /// none, [`EngineError::MeetingUnread`] when it couldn't be read.
    pub(crate) fn meeting(&self, chat: String, reply: Responder<Option<MeetingDetails>>) {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let read = MeetingThread { chat };
            let answer = match client.read(&read).await {
                Ok(answer) => Ok(MeetingThread::read_answer_in(client.transport().cloud(), answer.body())),
                Err(error) => {
                    log::info!("the meeting's chat couldn't be read: {error}");
                    Err(EngineError::MeetingUnread)
                }
            };
            let _ = reply.send(answer);
        });
    }

    /// Answers `reply` with the meeting that runs in the channel `channel` now ([`find_channel_meetup`]): `None` when
    /// none runs, [`EngineError::MeetingUnread`] when it couldn't be read.
    pub(crate) fn channel_meetup(&self, channel: String, reply: Responder<Option<ChannelMeetup>>) {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let answer = find_channel_meetup(&client, &channel).await.map_err(|error| {
                log::info!("{error}");
                EngineError::MeetingUnread
            });
            let _ = reply.send(answer);
        });
    }

    /// Answers `reply` with the addresses of `named` (stored people as (MRI, name)), from their profiles read now
    /// ([`ShortProfileLookup`], a read by `POST`); one without an address is left out.
    pub(crate) fn invitable(&self, named: Vec<(String, String)>, reply: Responder<Vec<Invitable>>) {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let lookup = ShortProfileLookup::new(named.iter().map(|(mri, _)| Mri::new(mri.as_str())));
            let profiles = match lookup {
                Some(lookup) if client.transport().has_middle_tier() => match client.lookup(lookup.clone()).await {
                    Ok(answer) => lookup.read_answer(answer.body()).unwrap_or_default(),
                    Err(error) => {
                        log::info!("the attendee picker's profiles couldn't be read: {error}");
                        Vec::new()
                    }
                },
                _ => Vec::new(),
            };
            let found = named
                .into_iter()
                .filter_map(|(mri, name)| {
                    let profile = profiles.iter().find(|profile| profile.mri.as_str() == mri)?;
                    let address = profile
                        .email
                        .clone()
                        .or_else(|| profile.user_principal_name.clone())
                        .filter(|address| address.contains('@'))?;
                    Some(Invitable { mri, name, address })
                })
                .collect();
            let _ = reply.send(Ok(found));
        });
    }

    /// Answers `reply` with the busy times of `attendees` and the account from `start_ms` (the local midnight of the
    /// day the form shows) and the starts all of them are free for `meeting_minutes`: the scheduling form's two
    /// free/busy reads ([`Schedules`], [`MeetingTimes`]), plain requests that reach nobody, sent together. One that
    /// failed leaves its part empty; both failing is [`EngineError::CalendarUnread`].
    pub(crate) fn availability(
        &self,
        attendees: Vec<EventAttendee>,
        start_ms: i64,
        meeting_minutes: u32,
        reply: Responder<MeetingAvailability>,
    ) {
        let outbox = Arc::clone(&self.outbox);
        let client = Arc::clone(&self.client);
        let whom = Arc::clone(&self.whom);
        tokio::task::spawn_local(async move {
            let me = outbox.me().clone();
            let known = known(&client, &whom, &me).await;
            let ask = FreeBusyAsk {
                attendees,
                organizer: organizer(&me, &known),
                start_ms,
                meeting_minutes: meeting_minutes.max(1),
            };
            let schedules = Schedules(ask.clone());
            let times = MeetingTimes(ask);
            let (schedules, times) = tokio::join!(client.send(&schedules), client.send(&times));
            let people = match schedules {
                Ok(answer) => Schedules::read_answer(answer.body()),
                Err(error) => {
                    log::info!("the attendees' busy times couldn't be read: {error}");
                    None
                }
            };
            let suggestions = match times {
                Ok(answer) => MeetingTimes::read_answer(answer.body()),
                Err(error) => {
                    log::info!("the suggested times couldn't be read: {error}");
                    None
                }
            };
            let answer = if people.is_none() && suggestions.is_none() {
                Err(EngineError::CalendarUnread)
            } else {
                Ok(MeetingAvailability {
                    people: people.unwrap_or_default(),
                    suggestions: suggestions.unwrap_or_default(),
                })
            };
            let _ = reply.send(answer);
        });
    }

    /// Answers `reply` with the busy times of `people` (other people's calendars added to the account's: their
    /// addresses and names) over `days` days from `start_ms`: the free/busy read the Scheduler sends ([`Schedules`]),
    /// one per [`teams_proto::AVAILABILITY_DAYS`] days as the web client asks, sent together and their answers joined
    /// by person. Plain reads that reach nobody; all of them failing is [`EngineError::CalendarUnread`].
    pub(crate) fn people_calendars(
        &self,
        people: Vec<EventAttendee>,
        start_ms: i64,
        days: u32,
        reply: Responder<Vec<PersonSchedule>>,
    ) {
        let outbox = Arc::clone(&self.outbox);
        let client = Arc::clone(&self.client);
        let whom = Arc::clone(&self.whom);
        tokio::task::spawn_local(async move {
            if people.is_empty() {
                let _ = reply.send(Ok(Vec::new()));
                return;
            }
            let me = outbox.me().clone();
            let known = known(&client, &whom, &me).await;
            let organizer = organizer(&me, &known);
            let chunk = teams_proto::AVAILABILITY_DAYS;
            let mut reads = JoinSet::new();
            for index in 0..days.clamp(1, 42).div_ceil(chunk) {
                let read = Schedules(FreeBusyAsk {
                    attendees: people.clone(),
                    organizer: organizer.clone(),
                    start_ms: start_ms + i64::from(index * chunk) * 86_400_000,
                    meeting_minutes: 30,
                });
                reads.spawn_local(schedules(Arc::clone(&client), read));
            }
            let mut joined: Vec<PersonSchedule> = Vec::new();
            let mut any = false;
            while let Some(done) = reads.join_next().await {
                let Ok(Some(read)) = done else { continue };
                any = true;
                join_schedules(&mut joined, read);
            }
            for schedule in &mut joined {
                schedule.busy.sort_by_key(|busy| busy.start_ms);
                schedule.busy.dedup();
            }
            let _ = reply.send(if any {
                Ok(joined)
            } else {
                Err(EngineError::CalendarUnread)
            });
        });
    }

    /// Makes a meeting of the account's own to start now, named `subject` (the calendar's Meet now, [`MeetNow`]) and
    /// answers `reply` with its chat and links: one attempt through the outbox (refused in the read-only mode), its
    /// failure [`EngineError::EventNotSaved`].
    pub(crate) fn meet_now(&self, subject: String, reply: Responder<MeetNowMeeting>) {
        let cloud = self.client.transport().cloud().clone();
        let outbox = Arc::clone(&self.outbox);
        tokio::task::spawn_local(async move {
            let request = MeetNow { subject };
            let answer = match send(&outbox, &request, "meet now").await {
                Ok(body) => MeetNow::read_answer_in(&cloud, &body)
                    .ok_or_else(|| not_saved("meet now", "the meeting doesn't read")),
                Err(error) => Err(error),
            };
            let _ = reply.send(answer);
        });
    }

    /// Sends `write` (see the module docs); once it is over, the engine settles the store and answers `reply` with what
    /// it gave ([`WriteOver`]).
    pub(crate) fn write(&mut self, write: EventWrite, reply: Responder<EventWritten>) {
        let outbox = Arc::clone(&self.outbox);
        let client = Arc::clone(&self.client);
        let whom = Arc::clone(&self.whom);
        self.writes.spawn_local(async move {
            let kind = write.kind();
            let touched = write.touched();
            let creates = matches!(write, EventWrite::Create(_) | EventWrite::CreateInChannel { .. });
            // a channel's meeting is created on its team's calendar: the account's own copy has another id
            let keeps = matches!(write, EventWrite::Create(_));
            let write = completed(&client, &whom, outbox.me(), write).await;
            let answer = match write {
                EventWrite::Create(draft) => {
                    let meeting = if draft.online {
                        online_meeting(&client).await
                    } else {
                        None
                    };
                    if draft.online && meeting.is_none() {
                        Err(not_saved(kind, "the Teams meeting couldn't be made"))
                    } else {
                        send(&outbox, &CreateEvent::new(draft, meeting), kind).await
                    }
                }
                // the event's online meeting is the service's: the web client's creation carries none
                EventWrite::CreateInChannel {
                    draft,
                    channel,
                    personal_invites,
                } => match CreateChannelEvent::new(draft, channel) {
                    Some(create) => send(&outbox, &create.with_personal_invites(personal_invites), kind).await,
                    None => Err(not_saved(kind, "the channel's team has no mailbox")),
                },
                EventWrite::Update { id, event, draft } => match UpdateEvent::new(id, event, draft) {
                    Some(update) => send(&outbox, &update, kind).await,
                    None => Err(not_saved(kind, "the event read isn't the one changed")),
                },
                EventWrite::UpdateInChannel { event, draft, channel } => {
                    match UpdateChannelEvent::new(channel, event, draft) {
                        Some(update) => send(&outbox, &update, kind).await,
                        None => Err(not_saved(kind, "the meeting read isn't the channel's")),
                    }
                }
                EventWrite::Cancel { id, chat, message } => match CancelEvent::new(id, chat, message) {
                    Some(cancel) => send(&outbox, &cancel, kind).await,
                    None => Err(not_saved(kind, "no event")),
                },
                EventWrite::CancelInChannel {
                    meeting,
                    channel,
                    post,
                    message,
                } => match CancelChannelEvent::new(&meeting, channel, post, message) {
                    Some(cancel) => send(&outbox, &cancel, kind).await,
                    None => Err(not_saved(kind, "the meeting's channel or post doesn't read")),
                },
                EventWrite::Delete { id } => match DeleteEvent::new(id) {
                    Some(delete) => send(&outbox, &delete, kind).await,
                    None => Err(not_saved(kind, "no event")),
                },
                EventWrite::Respond {
                    id,
                    answer,
                    send: tell,
                    comment,
                } => match RespondEvent::new(id, answer, tell, comment) {
                    Some(respond) => send(&outbox, &respond, kind).await,
                    None => Err(not_saved(kind, "no event")),
                },
            };
            Box::new(WriteOver::of(
                client.transport().cloud(),
                answer,
                creates,
                keeps,
                touched,
                reply,
            ))
        });
    }
}

impl WriteOver {
    /// What a write answered with `answer` (its answer's body) gives: the event it `creates`, and the one it `keeps`
    /// in the store (a creation's on the account's own calendar), for the engine to settle and answer `reply` with.
    fn of(
        cloud: &teams_proto::Cloud,
        answer: Result<Vec<u8>, EngineError>,
        creates: bool,
        keeps: bool,
        touched: Touched,
        reply: Responder<EventWritten>,
    ) -> Self {
        let new_event = answer
            .as_ref()
            .ok()
            .filter(|_| keeps)
            .and_then(|body| CalendarEvent::created_kept_in(cloud, body))
            .and_then(|(event, entry)| kept(&[event], vec![entry]).pop());
        Self {
            answer: answer.map(|body| EventWritten {
                created: creates.then(|| CalendarEvent::created_in(cloud, &body)).flatten(),
            }),
            touched,
            kept: new_event,
            reply,
        }
    }
}

/// Settles what `store` kept of the calendar once a write is over (see the module docs): every kept event it
/// `touched` forgotten, whatever window it was kept by, and the new event it made (`kept`) kept.
///
/// # Errors
/// [`StoreError::Sqlite`] when the store couldn't be read or written.
pub(crate) fn settle_write(store: &mut Store, touched: &Touched, kept: Option<&StoredEvent>) -> Result<(), StoreError> {
    if !touched.is_empty() {
        let gone: Vec<String> = store
            .calendar_events(i64::MIN, i64::MAX)?
            .into_iter()
            .filter(|stored| {
                touched.ids.contains(&stored.id)
                    || CalendarEvent::stored(&stored.entry).is_some_and(|event| touched.touches(&event))
            })
            .map(|stored| stored.id)
            .collect();
        if !gone.is_empty() {
            store.forget_calendar_events(&gone)?;
        }
    }
    if let Some(kept) = kept {
        store.keep_calendar_events(std::slice::from_ref(kept))?;
    }
    Ok(())
}

/// The meeting that runs in the channel `channel` now: as CSA's teams read names it under the channel's
/// `activeMeetups` ([`CsaTeams::meetup_in`]), else as the channel's newest posts tell it ([`ChannelMeetup::of_posts`]:
/// CSA names a channel's meeting only to some of the team, seen 2026-11-03, where the web client joins by the post's
/// card). `Ok(None)` when neither names one; an error saying what couldn't be read when neither read.
///
/// # Errors
/// As above.
pub(crate) async fn find_channel_meetup<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    channel: &str,
) -> Result<Option<ChannelMeetup>, String> {
    let csa = match client.read(&CsaTeams).await {
        Ok(answer) => match CsaTeams::meetup_for_cloud(client.transport().cloud(), answer.body(), channel) {
            Some(meetup) => return Ok(Some(meetup)),
            None => Ok(()),
        },
        Err(error) => Err(format!("the channel's meeting couldn't be read: {error}")),
    };
    match ChatReads::new(client).messages(channel, POSTS_LOOKED_AT).await {
        Ok(page) => Ok(ChannelMeetup::of_posts_in(client.transport().cloud(), &page.messages)),
        Err(error) => {
            log::info!("the channel's posts couldn't be read for its meeting: {error}");
            csa.map(|()| None)
        }
    }
}

/// How many of a channel's newest posts are looked at for the meeting running in it.
const POSTS_LOOKED_AT: u32 = 50;

/// How long a Meet now's post is waited for to title it, and how often the channel's posts are looked at meanwhile.
const TITLE_WAIT: std::time::Duration = std::time::Duration::from_secs(120);
const TITLE_POLL: std::time::Duration = std::time::Duration::from_secs(3);

/// `draft` with the account as its organizer and its time zone, as read ([`Whom`]).
async fn complete<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    whom: &Mutex<Option<Whom>>,
    me: &Author,
    mut draft: EventDraft,
) -> EventDraft {
    let known = known(client, whom, me).await;
    draft.organizer = organizer(me, &known);
    draft.time_zone = match &draft.zone {
        // a zone the form chose: as Exchange names it, read now
        Some(zone) => {
            let name = zone_name(client, zone).await;
            if name.is_none() {
                // the desktop's when that read fails: the service reads both times as the zone sent's clock, so they
                // are written in the desktop's offsets rather than the chosen zone's
                draft.start = in_desktop_zone(draft.start);
                draft.end = in_desktop_zone(draft.end);
            }
            name.or(known.time_zone)
        }
        None => known.time_zone,
    };
    draft
}

/// `time`, the same moment, with the desktop's offset then.
fn in_desktop_zone(time: ZonedTime) -> ZonedTime {
    Local
        .timestamp_millis_opt(time.ms)
        .single()
        .map_or(time, |at| ZonedTime {
            ms: time.ms,
            offset_minutes: at.offset().local_minus_utc() / 60,
        })
}

/// `write` with its draft, when it carries one, completed ([`complete`]).
async fn completed<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    whom: &Mutex<Option<Whom>>,
    me: &Author,
    write: EventWrite,
) -> EventWrite {
    match write {
        EventWrite::Create(draft) => EventWrite::Create(complete(client, whom, me, draft).await),
        EventWrite::CreateInChannel {
            draft,
            channel,
            personal_invites,
        } => EventWrite::CreateInChannel {
            draft: complete(client, whom, me, draft).await,
            channel,
            personal_invites,
        },
        EventWrite::Update { id, event, draft } => EventWrite::Update {
            id,
            event,
            draft: complete(client, whom, me, draft).await,
        },
        EventWrite::UpdateInChannel { event, draft, channel } => EventWrite::UpdateInChannel {
            event,
            draft: complete(client, whom, me, draft).await,
            channel,
        },
        other => other,
    }
}

/// What the account's events say of it ([`Whom`]): each part read the first time it is asked for, and again while it
/// didn't read (a failed read isn't kept for the run).
async fn known<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    whom: &Mutex<Option<Whom>>,
    me: &Author,
) -> Whom {
    let mut known = whom
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .unwrap_or_default();
    if known.address.is_some() && known.time_zone.is_some() {
        return known;
    }
    if known.address.is_none() {
        known.address = own_address(client, me.mri()).await;
    }
    if known.time_zone.is_none() {
        known.time_zone = time_zone(client).await;
    }
    *whom.lock().unwrap_or_else(PoisonError::into_inner) = Some(known.clone());
    known
}

/// A channel's meeting as its team's calendar gave it, seen by the account: the group's read says `isOrganizer` to
/// whoever reads it (the team organizes it), so it organizes it when it scheduled it (its `skypeTeamsData`'s `uid`),
/// and the account isn't among whom it invites (the service lists who scheduled it as an attendee who accepted).
fn as_scheduler(mut full: FullEvent, known: &Whom, me: &Author) -> FullEvent {
    let scheduled = full
        .event
        .channel_meeting
        .as_ref()
        .is_some_and(|channel| channel.organized_by(me.mri().as_str()));
    full.event.is_organizer = scheduled;
    if let Some(own) = known.address.as_deref() {
        full.event.attendees.retain(|attendee| {
            attendee
                .address
                .as_deref()
                .is_none_or(|address| !address.eq_ignore_ascii_case(own))
        });
    }
    full
}

/// The account as the organizer: its MRI, its address and its name.
fn organizer(me: &Author, known: &Whom) -> Organizer {
    Organizer {
        mri: me.mri().as_str().to_owned(),
        address: known.address.clone(),
        name: Some(me.display_name().to_owned()).filter(|name| !name.trim().is_empty()),
    }
}

/// The account's address, as its profile gives it.
async fn own_address<C: Credentials + 'static>(client: &Client<HttpTransport<C>>, me: &Mri) -> Option<String> {
    if !client.transport().has_middle_tier() {
        return None;
    }
    let lookup = ShortProfileLookup::new([me.clone()])?;
    let answer = client.lookup(lookup.clone()).await.ok()?;
    let profile = lookup
        .read_answer(answer.body())
        .ok()?
        .into_iter()
        .find(|profile| profile.mri == *me)?;
    profile
        .email
        .or(profile.user_principal_name)
        .filter(|address| address.contains('@'))
}

/// The account's time zone as Exchange names it (`TurkeySt`), from the middle tier's reading of the desktop's
/// ([`TimeZoneSettings`]).
async fn time_zone<C: Credentials + 'static>(client: &Client<HttpTransport<C>>) -> Option<String> {
    zone_name(client, &local_zone()?).await
}

/// The busy times one free/busy read gives ([`Schedules`]); `None` when it failed.
async fn schedules<C: Credentials + 'static>(
    client: Arc<Client<HttpTransport<C>>>,
    read: Schedules,
) -> Option<Vec<PersonSchedule>> {
    match client.send(&read).await {
        Ok(answer) => Schedules::read_answer(answer.body()),
        Err(error) => {
            log::info!("other people's calendars couldn't be read: {error}");
            None
        }
    }
}

/// `read`'s busy times added to `joined`'s, each person's to theirs.
fn join_schedules(joined: &mut Vec<PersonSchedule>, read: Vec<PersonSchedule>) {
    for schedule in read {
        match joined
            .iter_mut()
            .find(|known| known.address.eq_ignore_ascii_case(&schedule.address))
        {
            Some(known) => known.busy.extend(schedule.busy),
            None => joined.push(schedule),
        }
    }
}

/// The IANA zone `zone` as Exchange names it, read from the middle tier ([`TimeZoneSettings`]).
async fn zone_name<C: Credentials + 'static>(client: &Client<HttpTransport<C>>, zone: &str) -> Option<String> {
    let read = TimeZoneSettings::new(zone);
    let answer = client.read(&read).await.ok()?;
    TimeZoneSettings::read_answer(answer.body())
}

/// The desktop's IANA zone: honor `TZ`, else use the OS resolver (including Windows' native time-zone mapping).
fn local_zone() -> Option<String> {
    if let Ok(zone) = std::env::var("TZ") {
        let zone = zone.trim_start_matches(':').trim();
        if !zone.is_empty() && !zone.starts_with('/') {
            return Some(zone.to_owned());
        }
    }
    iana_time_zone::get_timezone().ok()
}

/// A Teams meeting's online meeting, made now ([`CreateOnlineMeeting`]): what its event carries.
async fn online_meeting<C: Credentials + 'static>(client: &Client<HttpTransport<C>>) -> Option<Value> {
    let answer = match client.transport().send(&CreateOnlineMeeting).await {
        Ok(answer) => answer,
        Err(error) => {
            log::info!("the Teams meeting couldn't be made: {error}");
            return None;
        }
    };
    CreateOnlineMeeting::read_answer_in(client.transport().cloud(), answer.body())
}

/// Sends `write` until it is answered or no further attempt may follow ([`CallRetries`]); the answer's body on a 2xx.
async fn send<T, R>(outbox: &Outbox<T>, write: &R, kind: &'static str) -> Result<Vec<u8>, EngineError>
where
    T: Transport<Error = HttpError>,
    R: Request + Clone + Send,
{
    let mut retries = CallRetries::default();
    loop {
        let tried = outbox.try_calendar(write.clone()).await;
        if let Some(wait) = retries.again(&tried) {
            tokio::time::sleep(wait).await;
            continue;
        }
        return match tried {
            CallTried::Answered(answer) if (200..300).contains(&answer.status()) => Ok(answer.body().to_vec()),
            CallTried::Answered(answer) => Err(not_saved(kind, &format!("the service answered {}", answer.status()))),
            CallTried::Denied(reason) => Err(not_saved(kind, &format!("refused: {reason}"))),
            CallTried::NotSent(why) | CallTried::Uncertain(why) => Err(not_saved(kind, &why)),
            CallTried::Throttled(_) => Err(not_saved(kind, "the service is busy")),
        };
    }
}

fn not_saved(kind: &'static str, why: &str) -> EngineError {
    log::info!("the event's {kind} wasn't saved: {why}");
    EngineError::EventNotSaved {
        kind,
        why: why.to_owned(),
    }
}

async fn read<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    view: &CalendarView,
) -> Result<teams_proto::CalendarAnswer, EngineError> {
    let answer = client.read(view).await.map_err(|error| {
        log::info!("the calendar couldn't be read: {error}");
        EngineError::CalendarUnread
    })?;
    let read = CalendarView::read_answer_in(client.transport().cloud(), answer.body()).map_err(|error| {
        log::info!("the calendar's answer wasn't taken: {error}");
        EngineError::CalendarUnread
    })?;
    if read.skipped > 0 {
        log::info!("the calendar's answer had {} events that don't read", read.skipped);
    }
    log::debug!("the calendar read {} events", read.events.len());
    Ok(read)
}

/// The stored events between `start_ms` and `end_ms` (see the module docs), by start: what the calendar shows at once
/// while it reads the window. An empty or reversed window has none; an entry that no longer reads is left out.
///
/// # Errors
///
/// [`StoreError`] when the store can't be read.
#[cfg(test)]
pub(crate) fn stored(store: &Store, start_ms: i64, end_ms: i64) -> Result<Vec<CalendarEvent>, StoreError> {
    stored_in(&teams_proto::Cloud::Microsoft, store, start_ms, end_ms)
}

pub(crate) fn stored_in(
    cloud: &teams_proto::Cloud,
    store: &Store,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<CalendarEvent>, StoreError> {
    if end_ms <= start_ms {
        return Ok(Vec::new());
    }
    Ok(store
        .calendar_events(start_ms, end_ms)?
        .iter()
        .filter_map(|stored| CalendarEvent::stored_in(cloud, &stored.entry))
        .collect())
}

/// `events` as the store keeps them, each with its entry as the answer gave it (`entries`, in the same order).
fn kept(events: &[CalendarEvent], entries: Vec<String>) -> Vec<StoredEvent> {
    events
        .iter()
        .zip(entries)
        .map(|(event, entry)| {
            let start_ms = instant(event.start);
            StoredEvent {
                id: event.id.clone(),
                start_ms,
                end_ms: instant(event.end).max(start_ms),
                entry,
            }
        })
        .collect()
}

/// An event's start or end in ms since the epoch: a time without an offset is the desktop's local time, as the
/// calendar places it (and the service, asked for a window by local midnights, gives it).
#[must_use]
pub fn instant(time: EventTime) -> i64 {
    match time {
        EventTime::Instant(ms) => ms,
        EventTime::Floating(time) => {
            let local =
                NaiveDate::from_ymd_opt(time.year, u32::from(time.month), u32::from(time.day)).and_then(|date| {
                    date.and_hms_opt(
                        u32::from(time.hour),
                        u32::from(time.minute),
                        u32::from(time.second.min(59)),
                    )
                });
            local.map_or(0, |local| {
                Local
                    .from_local_datetime(&local)
                    .earliest()
                    .map_or_else(|| local.and_utc().timestamp_millis(), |time| time.timestamp_millis())
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use teams_proto::FloatingTime;

    use super::*;

    fn floating(month: u8, day: u8, second: u8) -> EventTime {
        EventTime::Floating(FloatingTime {
            year: 2026,
            month,
            day,
            hour: 9,
            minute: 30,
            second,
        })
    }

    #[test]
    fn a_time_without_an_offset_is_the_desktop_s_local_time() {
        assert_eq!(instant(EventTime::Instant(1_793_491_200_000)), 1_793_491_200_000);
        let local = NaiveDate::from_ymd_opt(2026, 11, 2)
            .and_then(|date| date.and_hms_opt(9, 30, 59))
            .map(|time| {
                Local
                    .from_local_datetime(&time)
                    .earliest()
                    .map(|time| time.timestamp_millis())
            });
        // a leap second is read as the minute's last
        assert_eq!(Some(Some(instant(floating(11, 2, 60)))), local);
        assert_eq!(instant(floating(2, 30, 0)), 0, "a day the calendar has none of");
    }

    #[test]
    fn an_empty_or_reversed_window_has_no_stored_events() {
        let dir = tempfile::tempdir().expect("a directory");
        let store = Store::open(&dir.path().join("calendar.sqlite")).expect("a store");
        assert!(stored(&store, 10, 10).expect("read").is_empty());
        assert!(stored(&store, 10, 5).expect("read").is_empty());
        assert!(stored(&store, 0, 10).expect("read").is_empty());
    }

    #[test]
    fn a_write_s_kind_names_what_it_does() {
        let delete = EventWrite::Delete { id: "x".to_owned() };
        assert_eq!(delete.kind(), "delete");
        let cancel = EventWrite::Cancel {
            id: "x".to_owned(),
            chat: None,
            message: None,
        };
        assert_eq!(cancel.kind(), "cancellation");
    }
}
