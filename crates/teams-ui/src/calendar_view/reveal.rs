//! A meeting shown from elsewhere (an Activity meeting item, which Teams opens at the meeting's details): the calendar
//! moves to the meeting's day, scrolls to its start and opens its card once the day's events name it: by the event's
//! id, else by its time, else an occurrence of the series the id names. Only a read answered after the ask settles
//! that the day has no such event (it may be a new invitation the calendar hadn't read); the day stays shown then.

use chrono::{Datelike as _, NaiveDateTime, TimeZone as _};
use gpui::Context;

use super::{CalendarView, Loaded, Toggle};
use crate::calendar::Shown;

/// A meeting to show: its event's id and its start and end (ms), as far as they are known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reveal {
    pub event: Option<String>,
    pub time: Option<(i64, i64)>,
}

impl CalendarView {
    /// Shows `reveal`'s meeting (see the module docs).
    pub fn reveal(&mut self, reveal: Reveal, cx: &mut Context<Self>) {
        self.close_card(cx);
        if let Some(start) = reveal.time.and_then(|(start, _)| local(start)) {
            self.anchor = start.date();
            self.mini_month = start.date().with_day(1).unwrap_or(start.date());
            self.picker = None;
            self.focus_at = Some(start);
            self.toggles.set(Toggle::ScrollDue, true);
        }
        self.reveal = Some(reveal);
        self.reveal_found(false, cx);
        if self.reveal.is_some() {
            self.read(true, cx);
        }
        cx.notify();
    }

    /// The meeting being revealed, until its card opens or the day's read shows it isn't there.
    #[must_use]
    pub fn revealing(&self) -> Option<&Reveal> {
        self.reveal.as_ref()
    }

    /// Opens the card of the meeting being revealed when the events shown name it; `settled` (a read answered since
    /// the ask, none pending) gives up when they don't.
    pub(super) fn reveal_found(&mut self, settled: bool, cx: &mut Context<Self>) {
        let Some(reveal) = self.reveal.as_ref() else {
            return;
        };
        let found = match self.loaded.get(&self.span()) {
            Some(Loaded::Events(events) | Loaded::Stored(events)) => find(events, reveal),
            Some(Loaded::Failed) | None => None,
        };
        if let Some(shown) = found {
            self.reveal = None;
            self.open_card(shown, cx);
        } else if settled {
            self.reveal = None;
        }
    }
}

/// The event of `events` that `reveal` names.
fn find(events: &[Shown], reveal: &Reveal) -> Option<Shown> {
    let own = || events.iter().filter(|shown| shown.owner.is_none());
    let id = reveal.event.as_deref();
    if let Some(id) = id
        && let Some(shown) = own().find(|shown| shown.event.id == id)
    {
        return Some(shown.clone());
    }
    if let Some((start, end)) = reveal.time
        && let (Some(start), Some(end)) = (local(start), local(end))
    {
        let at_time: Vec<&Shown> = own().filter(|shown| shown.start == start && shown.end == end).collect();
        let meeting = at_time.iter().find(|shown| shown.event.online).or(at_time.first());
        if let Some(shown) = meeting {
            return Some((*shown).clone());
        }
    }
    id.and_then(|id| own().find(|shown| shown.event.series_master_id.as_deref() == Some(id)))
        .cloned()
}

/// `ms` in local time.
fn local(ms: i64) -> Option<NaiveDateTime> {
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|time| time.naive_local())
}
