//! The calendar commands, through the addressed account's [`Core`] as the calendar reads, and its meetings joined as
//! the calendar's Join and the pre-join's Join now join them.
//!
//! - `calendar`: the account's events between two local dates, both included (today without them), read from the
//!   service now ([`Core::calendar`]), by their start. A range wider than one read takes
//!   ([`MAX_CALENDAR_WINDOW_MS`]) is read that much at a time, an event across two reads listed once.
//! - `calendar show <id>`: one event, read whole now ([`Core::calendar_event`]), with what its card in the calendar
//!   shows ([`shown`]): the organizer and the account's answer, who it invites (the organizer not among them, optional
//!   ones marked), its join link, its text as plain text from the sanitized model the scheduling form loads and, on
//!   the organizer's copy alone, how each answered. How a series repeats is the form's summary of it, an
//!   occurrence's from its series read whole too, in what is left of the answer's time; "Part of a series" when that
//!   read fails or is late.
//! - `join meeting <id>`: the event read whole, then its meeting's pre-join opened in the call window as the
//!   calendar's Join opens it ([`calls::show_prejoin`]); nothing goes out. With `now`, the pre-join's Join now asked
//!   at once ([`calls::join_now_then`]: the microphone on, the camera off, the devices the settings chose), answered
//!   as the call commands answer a join ([`super::calls`]): once the engine took it, the meeting's call as told;
//!   otherwise why it went nowhere (a media setup that failed after the call was told included).
//!
//! **A meeting the app joins** is an event that names its chat and its organizer (its join link's `context`), or only
//! its chat, whose thread then names the organizer ([`Core::meeting`]). Any other event (no Teams meeting, or a short
//! `/meet/` link alone, which the calendar opens in the browser) is [`ErrorCode::NoMeeting`], its link told.
//! **An event's row** says `joinable` for such an event in an app that shows calls and isn't read-only.
//!
//! **The account**: the one `--account` names, otherwise the one the window shows (an event is its account's own).
//! A join while a call is up, rings or is being set up there or on another account is refused ([`ErrorCode::Busy`]),
//! as the calendar's Join refuses it; the read-only mode refuses it ([`ErrorCode::RefusedReadOnly`]); an app that
//! shows no calls answers [`ErrorCode::NoCalling`]. A join answers within [`CALL_ANSWER`], a calendar read within
//! [`CALENDAR_ANSWER`].

use std::collections::HashSet;

use gpui::{App, AsyncApp, WeakEntity};
use teams_core::{CalendarEvent, EventResponse, FullEvent};
use teams_ctl::protocol::{
    AttendeeRow, CALENDAR_ANSWER, CALL_ANSWER, CalendarRow, CtlError, ErrorCode, EventAnswer, Request, Response,
    calendar_window, local_today,
};
use teams_proto::MAX_CALENDAR_WINDOW_MS;
use teams_proto::chat::parse_html;
use teams_ui::Core;
use teams_ui::calendar::{event_title, invited};
use teams_ui::calls::{self as ui_calls, MeetingToJoin};
use teams_ui::event_form::repeat::Repeat;
use tokio::sync::oneshot;

use super::address::{self, engine_error};
use super::calls::{self, Fallback, Settle, Target};
use super::dispatch::alive;
use super::writes::within;
use crate::app::Running;

/// Whether `request` is a calendar command (see the module docs).
#[must_use]
pub fn serves(request: &Request) -> bool {
    matches!(
        request,
        Request::Calendar { .. } | Request::CalendarEvent { .. } | Request::JoinMeeting { .. }
    )
}

/// `request`, a calendar command, answered for `account` (see the module docs).
///
/// # Errors
/// The account isn't one, a date doesn't read, the engine's read failed or took longer than [`CALENDAR_ANSWER`], the
/// event isn't a meeting the app joins, or the join was refused or took longer than [`CALL_ANSWER`].
pub async fn answer(
    running: &WeakEntity<Running>,
    account: Option<&str>,
    request: &Request,
    cx: &mut AsyncApp,
) -> Result<Response, CtlError> {
    if let Request::JoinMeeting { event, now } = request {
        let target = cx.update(|cx| calls::target(&alive(running, cx)?, account, Fallback::Shown, cx))?;
        let executor = cx.background_executor().clone();
        let late = format!(
            "the meeting wasn't joined in {}s: `ngteamsctl calls` shows where it stands",
            CALL_ANSWER.as_secs()
        );
        return within(CALL_ANSWER, &late, join(&target, event, *now, cx), &executor).await;
    }
    let (addressed, joinable) = cx.update(|cx| {
        let running = alive(running, cx)?;
        let running = running.read(cx);
        let addressed = address::account(running, account, cx)?;
        let (_, _, bridge) = address::known(running, Some(&addressed.mri), cx)?;
        let shows = bridge.read(cx).models().calls.read(cx).shows();
        let joinable = shows && !addressed.core.read_only();
        Ok::<_, CtlError>((addressed, joinable))
    })?;
    let core = &*addressed.core;
    let executor = cx.background_executor().clone();
    let late = format!("the calendar wasn't read in {}s", CALENDAR_ANSWER.as_secs());
    match request {
        Request::Calendar { from, to } => {
            let (start, end) = calendar_window(from.as_deref(), to.as_deref(), local_today())?;
            let mut events = within(CALENDAR_ANSWER, &late, read_range(core, start, end), &executor).await?;
            events.sort_by_key(|event| (teams_core::calendar::instant(event.start), event.id.clone()));
            Ok(Response::Calendar {
                events: events.iter().map(|event| row(event, joinable)).collect(),
            })
        }
        Request::CalendarEvent { event } => {
            let started = std::time::Instant::now();
            let read = async {
                core.calendar_event(event.clone())
                    .await
                    .map_err(|error| unread(event, &error))
            };
            let full = within(CALENDAR_ANSWER, &late, read, &executor).await?;
            let left = CALENDAR_ANSWER.saturating_sub(started.elapsed());
            let series = async { Ok(recurrence(core, &full).await) };
            let repeats = within(left, &late, series, &executor).await.ok().flatten();
            Ok(Response::CalendarEvent(Box::new(shown(&full, repeats, joinable))))
        }
        _ => Err(CtlError::new(ErrorCode::BadRequest, "not a calendar command")),
    }
}

/// The account's events between `start_ms` and `end_ms`, read at most [`MAX_CALENDAR_WINDOW_MS`] at a time (the
/// engine reads nothing wider) and merged in the reads' order: an event across two reads once.
async fn read_range(core: &dyn Core, start_ms: i64, end_ms: i64) -> Result<Vec<CalendarEvent>, CtlError> {
    let mut events = Vec::new();
    let mut listed = HashSet::new();
    let mut from = start_ms;
    while from < end_ms {
        let to = from.saturating_add(MAX_CALENDAR_WINDOW_MS).min(end_ms);
        let read = core.calendar(from, to).await.map_err(|error| engine_error(&error))?;
        events.extend(read.into_iter().filter(|event| listed.insert(event.id.clone())));
        from = to;
    }
    Ok(events)
}

/// `event`'s row: `joinable` when the app joins meetings (it shows calls, it isn't read-only) and the event names its
/// meeting's chat.
#[must_use]
pub fn row(event: &CalendarEvent, joinable: bool) -> CalendarRow {
    let start = teams_core::calendar::instant(event.start);
    let filled = |text: &Option<String>| text.clone().filter(|text| !text.trim().is_empty());
    CalendarRow {
        id: event.id.clone(),
        subject: event.subject.clone(),
        start,
        end: teams_core::calendar::instant(event.end).max(start),
        all_day: event.all_day,
        cancelled: event.cancelled,
        online: event.online,
        joinable: joinable && event.meeting_chat.is_some(),
        location: filled(&event.location),
        organizer: filled(&event.organizer_name).or_else(|| filled(&event.organizer_address)),
        ..CalendarRow::default()
    }
}

/// How `full`'s series repeats (see the module docs): its own recurrence, or an occurrence's series' read whole.
async fn recurrence(core: &dyn Core, full: &FullEvent) -> Option<String> {
    if let Some(recurrence) = &full.recurrence {
        return Repeat::summary_of(recurrence);
    }
    let master = full.event.series_master_id.clone()?;
    // the event itself was read: a series that isn't still shows it, as "Part of a series"
    let series = core.calendar_event(master).await.ok()?;
    series.recurrence.as_ref().and_then(Repeat::summary_of)
}

/// `full`'s row with what its card shows (see the module docs); `recurrence` how its series repeats, when read.
#[must_use]
pub fn shown(full: &FullEvent, recurrence: Option<String>, joinable: bool) -> CalendarRow {
    let event = &full.event;
    let filled = |text: &Option<String>| text.clone().filter(|text| !text.trim().is_empty());
    let attendees = invited(event)
        .map(|attendee| AttendeeRow {
            name: filled(&attendee.name),
            address: filled(&attendee.address),
            optional: attendee.optional
                || attendee
                    .address
                    .as_deref()
                    .is_some_and(|address| full.optional.contains(&address.to_lowercase())),
            response: attendee
                .response
                .as_ref()
                .filter(|_| event.is_organizer)
                .and_then(event_answer),
        })
        .collect();
    let body = full
        .body_html
        .as_deref()
        .map(|html| parse_html(html).plain_text().trim().to_owned())
        .filter(|text| !text.is_empty())
        .or_else(|| filled(&event.preview).map(|text| text.trim().to_owned()));
    CalendarRow {
        organizer_address: filled(&event.organizer_address),
        response: if event.is_organizer {
            Some(EventAnswer::Organizer)
        } else {
            event_answer(&event.response).filter(|_| event.response != EventResponse::NoResponse)
        },
        attendees,
        join_url: event.join_url.clone(),
        series: event.series_master_id.clone(),
        recurrence: recurrence.or_else(|| event.kind.recurring().then(|| "Part of a series".to_owned())),
        body,
        ..row(event, joinable)
    }
}

/// An answer as the card's tally counts it: no answer asked (`None`) as not answered.
fn event_answer(response: &EventResponse) -> Option<EventAnswer> {
    match response {
        EventResponse::Organizer => Some(EventAnswer::Organizer),
        EventResponse::Accepted => Some(EventAnswer::Accepted),
        EventResponse::Tentative => Some(EventAnswer::Tentative),
        EventResponse::Declined => Some(EventAnswer::Declined),
        EventResponse::NotResponded | EventResponse::NoResponse => Some(EventAnswer::NotResponded),
        EventResponse::Other(_) => None,
    }
}

/// The error of event `id`'s read: the engine's, the event named.
fn unread(id: &str, error: &teams_core::EngineError) -> CtlError {
    let mut error = engine_error(error);
    if error.code == ErrorCode::Engine {
        error.message = format!(
            "event {id} couldn't be read ({}): `ngteamsctl calendar` lists them",
            error.message
        );
    }
    error
}

/// Event `id`'s meeting joined on `target`: its pre-join opened, and with `now` its Join now asked and its call waited
/// for (see the module docs).
async fn join(target: &Target, id: &str, now: bool, cx: &mut AsyncApp) -> Result<Response, CtlError> {
    let core = &*target.account.core;
    if core.read_only() {
        return Err(CtlError::new(
            ErrorCode::RefusedReadOnly,
            "the read-only mode joins no meeting",
        ));
    }
    let full = core
        .calendar_event(id.to_owned())
        .await
        .map_err(|error| unread(id, &error))?;
    let to_join = to_join(core, &full.event).await?;
    let Some(taken) = cx.update(|cx| open(target, to_join, now, cx))? else {
        return Ok(Response::Done);
    };
    // the engine's answer first, as the call commands wait for it: it tells a join's call as its media begins to be
    // set up, and refuses the join afterwards when that setup fails
    if !taken.await.unwrap_or(false) {
        return Err(cx.update(|cx| calls::refusal(target.calls.read(cx).refused())));
    }
    calls::settled(target, Settle::Started, cx).await
}

/// What joining `event`'s meeting takes: its chat and organizer, the organizer read from the chat's thread when the
/// event names only the chat (see the module docs).
async fn to_join(core: &dyn Core, event: &CalendarEvent) -> Result<MeetingToJoin, CtlError> {
    let title = event_title(event);
    let not_one = || {
        let link = event
            .join_url
            .as_ref()
            .map_or_else(String::new, |link| format!(": open its link instead, {link}"));
        CtlError::new(
            ErrorCode::NoMeeting,
            format!("{title} isn't a Teams meeting the app can join{link}"),
        )
    };
    let Some(chat) = event.meeting_chat.clone() else {
        return Err(not_one());
    };
    let meeting = match &event.meeting {
        Some(meeting) => meeting.clone(),
        None => match core.meeting(chat.clone()).await {
            Ok(Some(details)) => details.meeting,
            Ok(None) => return Err(not_one()),
            Err(error) => return Err(engine_error(&error)),
        },
    };
    Ok(MeetingToJoin::Chat {
        chat,
        meeting,
        title,
        post: None,
    })
}

/// `to_join`'s pre-join opened on `target`'s calls as the calendar's Join opens it, and with `now` its Join now asked:
/// then whether the engine took that join, once it answers.
fn open(
    target: &Target,
    to_join: MeetingToJoin,
    now: bool,
    cx: &mut App,
) -> Result<Option<oneshot::Receiver<bool>>, CtlError> {
    let busy_elsewhere = target.accounts.read(cx).busy_elsewhere(&target.account.mri, cx);
    if !ui_calls::show_prejoin(&target.calls, &target.trace, to_join, busy_elsewhere, cx) {
        return Err(calls::refusal(target.calls.read(cx).refused()));
    }
    if !now {
        return Ok(None);
    }
    let (told, taken) = oneshot::channel();
    ui_calls::join_now_then(
        &target.calls,
        &target.account.core,
        &target.trace,
        cx,
        move |taken, _| {
            // the command's connection may be gone meanwhile: nobody to tell
            let _ = told.send(taken);
        },
    );
    Ok(Some(taken))
}
