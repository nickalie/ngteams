//! A meeting shown from elsewhere (an Activity meeting item, which Teams opens at the meeting's details): the calendar
//! moves to the meeting's day (a work week that leaves the day out widens to the whole week), scrolls to its start and
//! opens its card once the day's events name it: by the event's id, else by its time, else an occurrence of the series
//! the id names (the one on the meeting's day first). Events read before the ask (the store's, an earlier visit's) open
//! it only by its id: a new invitation may not be among them while another meeting is at its time. Only a read
//! answered after the ask settles that the day has no such event; the day stays shown then.

use chrono::{Datelike as _, NaiveDateTime, TimeZone as _};
use gpui::Context;

use super::{CalendarView, Loaded, Toggle};
use crate::calendar::{Mode, Shown};
use crate::trace::TraceLine;

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
            if !self.span().contains(start.date()) {
                // a work week's weekend day
                self.mode = Mode::Week;
                self.trace
                    .record(&TraceLine::new("calendar", "shown").kind(self.mode.key()));
            }
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

    /// Opens the card of the meeting being revealed when the events shown name it; until `settled` (a read answered
    /// since the ask, none pending) only by its id. Settled, it gives up when they don't.
    pub(super) fn reveal_found(&mut self, settled: bool, cx: &mut Context<Self>) {
        let Some(reveal) = self.reveal.as_ref() else {
            return;
        };
        let found = match self.loaded.get(&self.span()) {
            Some(Loaded::Events(events) | Loaded::Stored(events)) => find(events, reveal, settled),
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

/// The event of `events` that `reveal` names: by its id only, unless `fallbacks` (its time, its series) may be used.
fn find(events: &[Shown], reveal: &Reveal, fallbacks: bool) -> Option<Shown> {
    let own = || events.iter().filter(|shown| shown.owner.is_none());
    let id = reveal.event.as_deref();
    if let Some(id) = id
        && let Some(shown) = own().find(|shown| shown.event.id == id)
    {
        return Some(shown.clone());
    }
    if !fallbacks {
        return None;
    }
    let time = reveal.time.and_then(|(start, end)| local(start).zip(local(end)));
    if let Some((start, end)) = time {
        let at_time: Vec<&Shown> = own().filter(|shown| shown.start == start && shown.end == end).collect();
        let meeting = at_time.iter().find(|shown| shown.event.online).or(at_time.first());
        if let Some(shown) = meeting {
            return Some((*shown).clone());
        }
    }
    let id = id?;
    let series: Vec<&Shown> = own()
        .filter(|shown| shown.event.series_master_id.as_deref() == Some(id))
        .collect();
    let on_day = time.and_then(|(start, _)| series.iter().find(|shown| shown.start.date() == start.date()));
    on_day.or(series.first()).map(|shown| (*shown).clone())
}

/// `ms` in local time.
fn local(ms: i64) -> Option<NaiveDateTime> {
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|time| time.naive_local())
}
