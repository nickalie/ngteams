//! A calendar meeting's reminder: before a Teams meeting on the account's calendar starts, a notice with Join, Snooze
//! and Dismiss, on the desktop and as a card in the window (`docs/protocol/calendar.md`, "Reminders").
//!
//! **Which meetings.** An online meeting of the account's calendar, neither cancelled nor declined nor all day, whose
//! reminder is set: its `isReminderSet` and `reminderMinutesBeforeStart`. The calendar view's answer may lack them; the
//! event is then read whole ([`Core::calendar_event`]), and again each [`REFRESH`] while it starts within
//! [`REREAD_AHEAD_MS`] (or its reminder, when longer) so that a reminder changed during the run is seen; a meeting that
//! ended is not read. Until the first read answers the reminder waits; one that fails leaves Outlook's default,
//! [`DEFAULT_MINUTES`], and a failed read again keeps the last reminder.
//!
//! **When.** From its reminder time until the meeting starts the notice says how long until it starts ("Starts in 15
//! minutes"); at the start it is shown again ("Starting now"), until the meeting ends. A run that starts after the
//! reminder time but before the start shows it at once; one that starts after the start shows the start's notice only
//! within [`START_LATE_MS`]. Each meeting (its event and its start) is reminded once per stage: a meeting that moves is
//! a new one, its old notice closed; a cancelled, declined or deleted one closes its notice. Snooze holds it back for
//! the time chosen (the desktop notice's Snooze for [`DESKTOP_SNOOZE_MINUTES`]), also past [`START_LATE_MS`]; Dismiss
//! ends its reminders.
//!
//! **The calendar.** Every [`TICK`] the stored calendar is looked at, and every [`REFRESH`] (and at the start) the
//! service's calendar is read, which keeps what it read in the store: so the reminders work when the calendar view was
//! never opened. The time is GPUI's wall clock, the system's in the app and the test clock in a test.
//!
//! None shows while the "Meeting reminders" switch is off ([`Settings::meeting_reminders`]), notifications are off, or
//! the account's status is Do not disturb; the card shows still while the account's in that meeting.
//!
//! What it does is traced (`meeting-reminder shown`/`closed`/`joined`/`snoozed`/`dismissed`, with the meeting's chat),
//! never a title, and logged at debug level by the event's id only.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, AppContext as _, AsyncApp, Context, Entity, EventEmitter};
use teams_core::{CalendarEvent, EventReminder, EventResponse};

use crate::bridge::Core;
use crate::calls::Calls;
use crate::meeting_alert::{MeetingJoin, NO_TITLE};
use crate::notify::{
    DEFAULT_ACTION, DISMISS_ACTION, JOIN_ACTION, Notice, NoticeKind, NotificationServer, SNOOZE_ACTION,
};
use crate::settings::{ManualStatus, Notifications, Settings, SettingsModel};
use crate::sounds::SoundEvent;
use crate::trace::{TraceLine, ViewTrace};

/// The reminder of a meeting whose event doesn't say, in minutes before its start (Outlook's default).
pub const DEFAULT_MINUTES: u32 = 15;
/// How often the stored calendar is looked at.
pub const TICK: Duration = Duration::from_secs(15);
/// How often the service's calendar is read.
pub const REFRESH: Duration = Duration::from_mins(5);
/// How far ahead the calendar is read, in ms: a reminder set further ahead shows once the meeting is this near.
pub const AHEAD_MS: i64 = 24 * HOUR_MS;
/// How long after its start a meeting not reminded yet still shows its start's notice, in ms.
pub const START_LATE_MS: i64 = 10 * MINUTE_MS;
/// How near its start a meeting whose reminder was read whole has that read asked again each [`REFRESH`], in ms
/// (further when its reminder is longer).
pub const REREAD_AHEAD_MS: i64 = 2 * HOUR_MS;
/// How long the desktop notice's Snooze holds a reminder back, in minutes.
pub const DESKTOP_SNOOZE_MINUTES: u32 = 5;
/// The card's Snooze choices, in minutes; `0` is "until it starts".
pub const SNOOZE_CHOICES: [u32; 5] = [1, 5, 10, 15, 0];

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MINUTE_MS;

/// Now on GPUI's wall clock, in ms since the epoch: the system's, or a test's clock, which its timers follow.
#[must_use]
pub fn wall_ms(cx: &App) -> i64 {
    cx.background_executor()
        .scheduler_executor()
        .scheduler()
        .clock()
        .utc_now()
        .timestamp_millis()
}

/// Where a reminded meeting is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Before its start: "Starts in N minutes".
    Before,
    /// Its start: "Starting now".
    Start,
}

/// One meeting reminded: its event and its start (ms), so a meeting that moves is a new one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Occasion {
    pub event: String,
    pub start: i64,
}

/// Whether `event` is a meeting that is reminded: online, neither cancelled nor declined nor all day, and not a
/// series' master (its occurrences come on their own).
#[must_use]
pub fn remindable(event: &CalendarEvent) -> bool {
    event.online
        && !event.cancelled
        && !event.all_day
        && event.response != EventResponse::Declined
        && event.kind != teams_core::EventKind::RecurringMaster
}

/// The minutes before its start `reminder` goes off: `None` when it is off; an unknown one is [`DEFAULT_MINUTES`].
#[must_use]
pub fn minutes_of(reminder: EventReminder) -> Option<u32> {
    match reminder {
        EventReminder::Before(minutes) => Some(minutes),
        EventReminder::Off => None,
        EventReminder::Unknown => Some(DEFAULT_MINUTES),
    }
}

/// The stage a meeting from `start` to `end` (ms) reminded `minutes` before is at `now`; `None` before its reminder
/// time and once it ended. `shown` is whether it was reminded already: a meeting not reminded yet shows its start's
/// notice only within [`START_LATE_MS`] of its start.
#[must_use]
pub fn stage_at(start: i64, end: i64, minutes: u32, now: i64, shown: bool) -> Option<Stage> {
    let remind_at = start - i64::from(minutes) * MINUTE_MS;
    if now < remind_at || now >= end.max(start + MINUTE_MS) {
        None
    } else if now < start {
        Some(Stage::Before)
    } else if shown || now < start + START_LATE_MS {
        Some(Stage::Start)
    } else {
        None
    }
}

/// What the notice says of when the meeting starts: "Starts in 15 minutes", "Starts in 1 minute", "Starting now".
#[must_use]
pub fn when_text(start: i64, now: i64) -> String {
    let minutes = (start - now + MINUTE_MS - 1).div_euclid(MINUTE_MS);
    match minutes {
        ..=0 => "Starting now".to_owned(),
        1 => "Starts in 1 minute".to_owned(),
        _ => format!("Starts in {minutes} minutes"),
    }
}

/// A Snooze choice's words: "1 minute", "5 minutes", "Until it starts".
#[must_use]
pub fn snooze_label(minutes: u32) -> String {
    match minutes {
        0 => "Until it starts".to_owned(),
        1 => "1 minute".to_owned(),
        _ => format!("{minutes} minutes"),
    }
}

/// Whether the settings let a reminder show for `me` now.
#[must_use]
pub fn reminders_on(settings: &Settings, me: &str) -> bool {
    settings.meeting_reminders
        && settings.notifications != Notifications::Off
        && settings.manual_status_of(me) != ManualStatus::DoNotDisturb
}

/// A reminder the window shows as a card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderCard {
    pub occasion: Occasion,
    pub title: String,
    pub stage: Stage,
    /// The meeting's start and end, ms.
    pub start: i64,
    pub end: i64,
    /// Its time as the clock shows it ("10:00 – 10:30").
    pub times: Option<String>,
    /// Whether its Join goes anywhere: the meeting's chat or its link is known, and the build shows calls.
    pub joinable: bool,
    /// The id of its desktop notice once the server answered: until then its notice's actions find no reminder.
    pub notice: Option<u32>,
}

/// What the reminders ask of the desktop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReminderEvent {
    /// Join: the meeting's pre-join, as the calendar's Join opens it.
    Join(MeetingJoin),
    /// The notice's body: the meeting's chat, else the calendar.
    Open { chat: Option<String> },
}

/// One meeting being reminded: the event as last read, the stage shown, the stage sent to the desktop and its notice, a
/// snooze, a dismissal.
struct Kept {
    event: CalendarEvent,
    end: i64,
    shown: Option<Stage>,
    /// The stage whose desktop notice went to the server: one shown before the server came is sent once it comes.
    sent: Option<Stage>,
    notice: Option<u32>,
    snoozed_until: Option<i64>,
    dismissed: bool,
}

impl Kept {
    /// Whether it was reminded: shown, or snoozed since (a snooze past [`START_LATE_MS`] must still remind again).
    fn reminded(&self) -> bool {
        self.shown.is_some() || self.snoozed_until.is_some()
    }
}

/// The meeting reminders of one account (see the module docs). A GPUI entity the desktop holds.
pub struct MeetingReminders {
    core: Arc<dyn Core>,
    settings: Entity<SettingsModel>,
    calls: Entity<Calls>,
    trace: Arc<ViewTrace>,
    server: Option<Arc<dyn NotificationServer>>,
    kept: HashMap<Occasion, Kept>,
    /// The reminders read from the events read whole, by event id, with when each was read (wall clock, ms).
    reminders: HashMap<String, (EventReminder, i64)>,
    /// The events being read whole.
    reading: HashSet<String>,
    /// When the service's calendar was last read, on the wall clock (ms).
    refreshed: Option<i64>,
    /// The calendar as last read: applied again once an event read whole answers.
    last: Vec<CalendarEvent>,
}

impl EventEmitter<ReminderEvent> for MeetingReminders {}

impl MeetingReminders {
    /// The reminders over the account's calendar; they look at it every [`TICK`] from now on.
    pub fn new(
        core: Arc<dyn Core>,
        settings: Entity<SettingsModel>,
        calls: Entity<Calls>,
        trace: Arc<ViewTrace>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            loop {
                if this.update(cx, Self::look).is_err() {
                    return;
                }
                cx.background_executor().timer(TICK).await;
            }
        })
        .detach();
        Self {
            core,
            settings,
            calls,
            trace,
            server: None,
            kept: HashMap::new(),
            reminders: HashMap::new(),
            reading: HashSet::new(),
            refreshed: None,
            last: Vec::new(),
        }
    }

    /// From now on, desktop notices go to `server`; a reminder the window shows already (one due as the app started,
    /// before the server came) goes there now.
    pub fn set_server(&mut self, server: Arc<dyn NotificationServer>, cx: &mut Context<Self>) {
        self.server = Some(server);
        let now = wall_ms(cx);
        let unsent: Vec<(Occasion, Stage)> = self
            .kept
            .iter()
            .filter(|(_, kept)| !kept.dismissed && kept.snoozed_until.is_none() && kept.sent != kept.shown)
            .filter_map(|(occasion, kept)| Some((occasion.clone(), kept.shown?)))
            .collect();
        for (occasion, stage) in unsent {
            log::debug!(
                "meeting reminder: {} sent to the desktop late ({stage:?})",
                occasion.event
            );
            self.send(&occasion, stage, now, cx);
        }
    }

    /// Reads the service's calendar now (the calendar changed, or a test can't wait for [`REFRESH`]).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refreshed = None;
        self.look(cx);
    }

    /// The reminders the window shows as cards now, the soonest first.
    #[must_use]
    pub fn cards(&self, cx: &App) -> Vec<ReminderCard> {
        let joins = self.calls.read(cx).shows() && !self.core.read_only();
        let mut cards: Vec<ReminderCard> = self
            .kept
            .iter()
            .filter_map(|(occasion, kept)| {
                let stage = kept.shown.filter(|_| !kept.dismissed && kept.snoozed_until.is_none())?;
                Some(ReminderCard {
                    occasion: occasion.clone(),
                    title: title_of(&kept.event),
                    stage,
                    start: occasion.start,
                    end: kept.end,
                    times: time_range(&kept.event, crate::clock::cycle(cx).time_of_day()),
                    joinable: joins && (kept.event.meeting_chat.is_some() || kept.event.join_url.is_some()),
                    notice: kept.notice,
                })
            })
            .collect();
        cards.sort_by(|a, b| (a.start, &a.occasion.event).cmp(&(b.start, &b.occasion.event)));
        cards
    }

    /// Desktop notice `id` had `action` invoked: whether it is one of these reminders'.
    pub fn activated(&mut self, id: u32, action: &str, cx: &mut Context<Self>) -> bool {
        let Some(occasion) = self
            .kept
            .iter()
            .find_map(|(occasion, kept)| (kept.notice == Some(id)).then(|| occasion.clone()))
        else {
            return false;
        };
        match action {
            JOIN_ACTION => self.join(&occasion, cx),
            SNOOZE_ACTION => self.snooze(&occasion, DESKTOP_SNOOZE_MINUTES, cx),
            DISMISS_ACTION => self.dismiss(&occasion, cx),
            DEFAULT_ACTION => {
                let chat = self
                    .kept
                    .get(&occasion)
                    .and_then(|kept| kept.event.meeting_chat.clone());
                cx.emit(ReminderEvent::Open { chat });
            }
            _ => {}
        }
        true
    }

    /// Joins the meeting of `occasion`: its reminders end.
    pub fn join(&mut self, occasion: &Occasion, cx: &mut Context<Self>) {
        let Some(kept) = self.kept.get_mut(occasion) else {
            return;
        };
        kept.dismissed = true;
        let join = MeetingJoin::Calendar {
            chat: kept.event.meeting_chat.clone(),
            meeting: kept.event.meeting.clone(),
            title: title_of(&kept.event),
            link: kept.event.join_url.clone(),
        };
        self.record("joined", occasion);
        self.close_notice(occasion, cx);
        cx.emit(ReminderEvent::Join(join));
        cx.notify();
    }

    /// Holds the reminder of `occasion` back for `minutes` (`0`: until the meeting starts).
    pub fn snooze(&mut self, occasion: &Occasion, minutes: u32, cx: &mut Context<Self>) {
        let now = wall_ms(cx);
        let Some(kept) = self.kept.get_mut(occasion) else {
            return;
        };
        let until = if minutes == 0 {
            occasion.start.max(now + MINUTE_MS)
        } else {
            now + i64::from(minutes) * MINUTE_MS
        };
        kept.snoozed_until = Some(until);
        kept.shown = None;
        kept.sent = None;
        log::debug!("meeting reminder: {} snoozed for {minutes} min", occasion.event);
        self.record("snoozed", occasion);
        self.close_notice(occasion, cx);
        cx.notify();
    }

    /// Ends the reminders of `occasion`.
    pub fn dismiss(&mut self, occasion: &Occasion, cx: &mut Context<Self>) {
        let Some(kept) = self.kept.get_mut(occasion) else {
            return;
        };
        kept.dismissed = true;
        log::debug!("meeting reminder: {} dismissed", occasion.event);
        self.record("dismissed", occasion);
        self.close_notice(occasion, cx);
        cx.notify();
    }

    /// One look at the calendar: the stored one, and the service's when a read is due.
    fn look(&mut self, cx: &mut Context<Self>) {
        let now = wall_ms(cx);
        let window = (now - 2 * HOUR_MS, now + AHEAD_MS);
        let refresh = self.refreshed.is_none_or(|at| now - at >= refresh_ms());
        let core = Arc::clone(&self.core);
        if refresh {
            self.refreshed = Some(now);
        }
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let events = if refresh {
                match core.calendar(window.0, window.1).await {
                    Ok(events) => Some(events),
                    Err(error) => {
                        log::debug!("meeting reminder: the calendar read failed: {error}");
                        core.stored_calendar(window.0, window.1).await.ok()
                    }
                }
            } else {
                core.stored_calendar(window.0, window.1).await.ok()
            };
            if let Some(events) = events {
                let _ = this.update(cx, |this, cx| this.apply(&events, cx));
            }
        })
        .detach();
    }

    /// Applies the calendar `events` as read now: each meeting due shows its stage, and those no longer due close.
    fn apply(&mut self, events: &[CalendarEvent], cx: &mut Context<Self>) {
        let now = wall_ms(cx);
        let on = reminders_on(self.settings.read(cx).settings(), self.core.me());
        let mut due = HashSet::new();
        for event in events.iter().filter(|event| remindable(event)) {
            let occasion = Occasion {
                event: event.id.clone(),
                start: teams_core::calendar::instant(event.start),
            };
            let end = teams_core::calendar::instant(event.end);
            let Some(minutes) = self.reminder_of(event, now, cx).and_then(minutes_of) else {
                continue;
            };
            let shown = self.kept.get(&occasion).is_some_and(Kept::reminded);
            let Some(stage) = stage_at(occasion.start, end, minutes, now, shown) else {
                continue;
            };
            due.insert(occasion.clone());
            let kept = self.kept.entry(occasion.clone()).or_insert_with(|| Kept {
                event: event.clone(),
                end,
                shown: None,
                sent: None,
                notice: None,
                snoozed_until: None,
                dismissed: false,
            });
            kept.event = event.clone();
            kept.end = end;
            if on {
                self.remind(&occasion, stage, now, cx);
            }
        }
        let gone: Vec<Occasion> = self
            .kept
            .keys()
            .filter(|occasion| !due.contains(*occasion))
            .cloned()
            .collect();
        for occasion in gone {
            if self.kept.get(&occasion).is_some_and(|kept| kept.shown.is_some()) {
                log::debug!(
                    "meeting reminder: {} no longer due (moved, cancelled, declined or over)",
                    occasion.event
                );
                self.record("closed", &occasion);
            }
            self.close_notice(&occasion, cx);
            self.kept.remove(&occasion);
        }
        self.last = events.to_vec();
        cx.notify();
    }

    /// The reminder of `event`: its own, else the one its read whole gave; `None` until that read (asked for an event
    /// within [`AHEAD_MS`]) first answers. A read older than [`REFRESH`] is asked again, its reminder kept meanwhile, so
    /// a reminder changed during the run is seen: only for a meeting that starts within [`REREAD_AHEAD_MS`] (or within
    /// its reminder, when that is longer). No read is asked for a meeting that ended.
    fn reminder_of(&mut self, event: &CalendarEvent, now: i64, cx: &mut Context<Self>) -> Option<EventReminder> {
        if event.reminder != EventReminder::Unknown {
            return Some(event.reminder);
        }
        let read = self.reminders.get(&event.id).copied();
        let start = teams_core::calendar::instant(event.start);
        let over = now >= teams_core::calendar::instant(event.end).max(start + MINUTE_MS);
        let ask = match read {
            None => start - now <= AHEAD_MS,
            Some((reminder, at)) => {
                let reminder_ms = minutes_of(reminder).map_or(0, |minutes| i64::from(minutes) * MINUTE_MS);
                now - at >= refresh_ms() && start - now <= REREAD_AHEAD_MS.max(reminder_ms)
            }
        };
        if !over && ask && self.reading.insert(event.id.clone()) {
            let (core, id) = (Arc::clone(&self.core), event.id.clone());
            cx.spawn(async move |this, cx: &mut AsyncApp| {
                let read = core.calendar_event(id.clone()).await;
                let _ = this.update(cx, |this, cx| {
                    this.reading.remove(&id);
                    let before = this.reminders.get(&id).map(|(reminder, _)| *reminder);
                    let reminder = read.map_or(before.unwrap_or(EventReminder::Unknown), |full| full.event.reminder);
                    log::debug!("meeting reminder: {id} read whole, its reminder {reminder:?}");
                    this.reminders.insert(id, (reminder, wall_ms(cx)));
                    let last = std::mem::take(&mut this.last);
                    this.apply(&last, cx);
                });
            })
            .detach();
        }
        read.map(|(reminder, _)| reminder)
    }

    /// Shows `occasion` at `stage` unless it is dismissed, snoozed, or shown at that stage already.
    fn remind(&mut self, occasion: &Occasion, stage: Stage, now: i64, cx: &mut Context<Self>) {
        let Some(kept) = self.kept.get_mut(occasion) else {
            return;
        };
        if kept.dismissed || kept.shown == Some(stage) {
            return;
        }
        if let Some(until) = kept.snoozed_until {
            if now < until {
                return;
            }
            kept.snoozed_until = None;
        }
        kept.shown = Some(stage);
        log::debug!("meeting reminder: {} shown ({stage:?})", occasion.event);
        self.record("shown", occasion);
        if let Some(player) = self.settings.read(cx).sound_player() {
            player.play(
                SoundEvent::Message,
                self.core.me(),
                self.calls.read(cx).call().is_some(),
            );
        }
        self.send(occasion, stage, now, cx);
    }

    /// Sends the desktop notice of `occasion` at `stage`, in place of its last one; without a server yet it waits for
    /// [`MeetingReminders::set_server`].
    fn send(&mut self, occasion: &Occasion, stage: Stage, now: i64, cx: &mut Context<Self>) {
        let Some(server) = self.server.clone() else { return };
        let Some(kept) = self.kept.get_mut(occasion) else {
            return;
        };
        kept.sent = Some(stage);
        let notice = Notice {
            conversation: kept.event.meeting_chat.clone().unwrap_or_default(),
            kind: NoticeKind::Reminder,
            summary: title_of(&kept.event),
            body: notice_body(&kept.event, occasion.start, now, crate::clock::cycle(cx).time_of_day()),
            picture: None,
        };
        let replaces = kept.notice.unwrap_or(0);
        let occasion = occasion.clone();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let Ok(id) = server.show(&notice, replaces).await else {
                log::debug!("meeting reminder: the notification server refused one");
                return;
            };
            // a Dismiss, Join, Snooze or the next stage while the server answered: this notice is no longer the one
            let kept = this.update(cx, |this, _| {
                let kept = this
                    .kept
                    .get_mut(&occasion)
                    .filter(|kept| kept.shown == Some(stage) && !kept.dismissed && kept.snoozed_until.is_none());
                kept.map(|kept| kept.notice = Some(id)).is_some()
            });
            if !kept.unwrap_or(false) {
                server.close(id).await;
            }
        })
        .detach();
    }

    fn close_notice(&mut self, occasion: &Occasion, cx: &App) {
        let Some(id) = self.kept.get_mut(occasion).and_then(|kept| kept.notice.take()) else {
            return;
        };
        if let Some(server) = self.server.clone() {
            cx.background_spawn(server.close(id)).detach();
        }
    }

    fn record(&self, what: &'static str, occasion: &Occasion) {
        let chat = self
            .kept
            .get(occasion)
            .and_then(|kept| kept.event.meeting_chat.clone())
            .unwrap_or_default();
        self.trace
            .record(&TraceLine::new("meeting-reminder", what).conversation(&chat));
    }
}

/// [`REFRESH`] in ms.
fn refresh_ms() -> i64 {
    i64::try_from(REFRESH.as_millis()).unwrap_or(i64::MAX)
}

/// The meeting's title: its subject, else [`NO_TITLE`].
fn title_of(event: &CalendarEvent) -> String {
    let subject = event.subject.trim();
    if subject.is_empty() {
        NO_TITLE.to_owned()
    } else {
        subject.to_owned()
    }
}

/// The desktop notice's text: when it starts, and its time in the clock's `format` ("Starts in 15 minutes\n10:00 –
/// 10:30").
#[must_use]
pub fn notice_body(event: &CalendarEvent, start: i64, now: i64, format: &str) -> String {
    let times = time_range(event, format);
    match times {
        Some(times) => format!("{}\n{times}", when_text(start, now)),
        None => when_text(start, now),
    }
}

/// The meeting's time in the clock's `format`: "10:00 – 10:30".
#[must_use]
pub fn time_range(event: &CalendarEvent, format: &str) -> Option<String> {
    crate::calendar::local(event.start)
        .zip(crate::calendar::local(event.end))
        .map(|(start, end)| format!("{} – {}", start.format(format), end.format(format)))
}

#[cfg(test)]
mod tests;
