//! The calendar's navigation, refresh and event-card actions against a synthetic engine on GPUI's test platform.

#![cfg(test)]

pub mod common;
#[path = "calendar/fixtures.rs"]
mod fixtures;

use std::sync::Arc;

use chrono::{NaiveDate, TimeZone as _};
use common::{Rig, Written};
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use teams_core::{EngineError, EventResponse, EventWrite, EventWritten};
use teams_ui::ViewTrace;
use teams_ui::calendar::{Mode, Shown};
use teams_ui::calendar_view::{CalendarView, CalendarViewEvent};

fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, day).unwrap()
}

fn event(id: &str, hour: u8) -> teams_core::CalendarEvent {
    fixtures::full(id, hour).event
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    draw(cx);
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    draw(cx);
}

fn window(rig: &Rig) -> (Entity<CalendarView>, &'static mut VisualTestContext, Written) {
    rig.cx.update(teams_ui::window::init);
    let written = Written::default();
    let trace = Arc::new(ViewTrace::new(written.clone()));
    let core = rig.core.clone();
    let calls = rig.models.calls.clone();
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (root, cx) = app.add_window_view(|window, cx| {
        let view = cx.new(|cx| CalendarView::new(core, calls, trace, cx));
        teams_ui::window::KitRoot::new(view, window, cx)
    });
    let view = cx.update(|_, cx| root.read(cx).view().clone().downcast::<CalendarView>().unwrap());
    cx.simulate_resize(size(px(1300.), px(850.)));
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.go_to(date(27), cx);
            view.set_shown(true, window, cx);
        });
    });
    draw(cx);
    (view, cx, written)
}

#[test]
fn the_calendar_filters_declined_events_sorts_reads_and_navigates_by_keyboard() {
    let rig = Rig::start();
    let mut declined = event("declined", 11);
    declined.response = EventResponse::Declined;
    rig.core.calendar.lock().unwrap().events = vec![event("late", 14), declined, event("early", 9)];
    let (view, cx, written) = window(&rig);
    cx.update(|_, cx| {
        let view = view.read(cx);
        let events = view.events().unwrap();
        assert_eq!(
            events.iter().map(|shown| shown.event.id.as_str()).collect::<Vec<_>>(),
            ["early", "late"]
        );
        assert!(view.shown());
        assert_eq!(view.mode(), Mode::WorkWeek);
    });
    assert!(cx.debug_bounds("calendar-grid").is_some());
    cx.simulate_keystrokes("ctrl-alt-4");
    draw(cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).mode()), Mode::Month);
    assert!(cx.debug_bounds("calendar-month").is_some());
    cx.simulate_keystrokes("right");
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).anchor()),
        NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()
    );
    cx.simulate_keystrokes("left");
    draw(cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).anchor()), date(1));
    click(cx, "calendar-pane-toggle");
    assert!(cx.debug_bounds("calendar-pane").is_none());
    click(cx, "calendar-pane-toggle");
    assert!(cx.debug_bounds("calendar-pane").is_some());
    assert!(!written.text().contains("Synthetic event"), "trace carries no subjects");
}

#[test]
fn the_calendar_toolbar_keeps_its_actions_inside_a_narrow_window() {
    let rig = Rig::start();
    let (view, cx, _) = window(&rig);
    // The 1024px desktop leaves 948px for the calendar after the app rail.
    for width in [948.0, 1228.0, 948.0] {
        cx.simulate_resize(size(px(width), px(720.0)));
        for mode in [Mode::WorkWeek, Mode::Day, Mode::Month] {
            cx.update(|_, cx| view.update(cx, |view, cx| view.set_mode(mode, cx)));
            draw(cx);
            let toolbar = cx.debug_bounds("calendar-toolbar").unwrap();
            assert!(toolbar.right() <= px(width), "the toolbar fits its parent");
            for selector in [
                "calendar-pane-toggle",
                "calendar-today",
                "calendar-previous",
                "calendar-next",
                "calendar-dates",
                "calendar-mode",
                "calendar-filter",
                "calendar-more",
                "calendar-meet-now",
                "calendar-meet-now-more",
                "calendar-new",
                "calendar-new-more",
            ] {
                let bounds = cx.debug_bounds(selector).unwrap();
                assert!(bounds.size.width > px(0.0), "{selector} remains reachable");
                assert!(bounds.left() >= toolbar.left(), "{selector} stays after the pane");
                assert!(bounds.right() <= toolbar.right(), "{selector} stays inside the toolbar");
            }
        }
    }
    click(cx, "calendar-pane-toggle");
    let new = cx.debug_bounds("calendar-new-more").unwrap();
    assert!(new.right() <= px(948.0), "the actions also fit after hiding the pane");
    click(cx, "calendar-pane-toggle");
    click(cx, "calendar-meet-now");
    assert!(cx.debug_bounds("calendar-meet-now-name").is_some());
    assert!(rig.core.calendar.lock().unwrap().writes.is_empty());
}

#[test]
fn a_failed_read_retries_and_a_stopped_refresh_keeps_the_previous_events() {
    let rig = Rig::start();
    rig.core
        .calendar
        .lock()
        .unwrap()
        .answers
        .push_back(Err(EngineError::CalendarUnread));
    rig.core
        .calendar
        .lock()
        .unwrap()
        .answers
        .push_back(Err(EngineError::CalendarUnread));
    let (view, cx, _) = window(&rig);
    assert!(cx.debug_bounds("calendar-failed").is_some());
    rig.core.calendar.lock().unwrap().events = vec![event("recovered", 9)];
    click(cx, "calendar-retry");
    assert!(cx.debug_bounds("calendar-failed").is_none());
    assert_eq!(cx.update(|_, cx| view.read(cx).events().unwrap().len()), 1);
    rig.core
        .calendar
        .lock()
        .unwrap()
        .answers
        .push_back(Err(EngineError::Stopped));
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.set_shown(false, window, cx);
            view.set_shown(true, window, cx);
        });
    });
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).events().unwrap()[0].event.id.clone()),
        "recovered"
    );
}

fn shown_ids(view: &Entity<CalendarView>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.update(|_, cx| {
        view.read(cx)
            .events()
            .map(|events| events.iter().map(|shown| shown.event.id.clone()).collect())
            .unwrap_or_default()
    })
}

#[test]
fn the_stored_events_show_at_once_and_the_read_applies_on_top() {
    let rig = Rig::start();
    let mut moved = event("moved", 9);
    moved.subject = "Before the move".to_owned();
    let mut occurrence = series_meeting(13);
    occurrence.id = "occurrence".to_owned();
    rig.core.calendar.lock().unwrap().stored = vec![event("gone", 11), moved.clone(), occurrence.clone()];
    rig.core.calendar.lock().unwrap().hold = true;
    let (view, cx, written) = window(&rig);
    assert_eq!(
        shown_ids(&view, cx),
        ["moved", "gone", "occurrence"],
        "before any answer"
    );
    assert!(cx.update(|_, cx| view.read(cx).showing_stored()));
    {
        let calendar = rig.core.calendar.lock().unwrap();
        assert_eq!(
            calendar.stored_reads[0], calendar.reads[0],
            "the store asked for the same days the read is"
        );
    }
    assert!(written.text().contains(r#""change":"stored""#));
    draw(cx);
    assert!(cx.debug_bounds("calendar-failed").is_none());

    // the answer: one deleted, one moved and renamed, the occurrence cancelled, one added
    let mut moved_now = event("moved", 15);
    moved_now.subject = "After the move".to_owned();
    let mut cancelled = occurrence;
    cancelled.cancelled = true;
    let answer = rig.core.calendar.lock().unwrap().held.remove(0);
    answer.send(Ok(vec![moved_now, cancelled, event("added", 8)]));
    draw(cx);
    assert_eq!(shown_ids(&view, cx), ["added", "occurrence", "moved"]);
    cx.update(|_, cx| {
        let view = view.read(cx);
        assert!(!view.showing_stored());
        let events = view.events().unwrap();
        assert_eq!(events[2].event.subject, "After the move");
        assert!(events[1].event.cancelled);
    });
}

#[test]
fn a_failed_read_keeps_the_stored_events_and_a_read_span_doesnt_ask_the_store_again() {
    let rig = Rig::start();
    rig.core.calendar.lock().unwrap().stored = vec![event("kept", 9)];
    // going to the day and showing the calendar read it twice
    for _ in 0..2 {
        rig.core
            .calendar
            .lock()
            .unwrap()
            .answers
            .push_back(Err(EngineError::CalendarUnread));
    }
    let (view, cx, _) = window(&rig);
    assert_eq!(shown_ids(&view, cx), ["kept"]);
    assert!(
        cx.debug_bounds("calendar-failed").is_none(),
        "the kept events are shown instead"
    );
    rig.core.calendar.lock().unwrap().events = vec![event("fresh", 10)];
    cx.update(|_, cx| view.update(cx, |view, cx| view.step(true, cx)));
    draw(cx);
    cx.update(|_, cx| view.update(cx, |view, cx| view.step(false, cx)));
    draw(cx);
    let stored_reads = rig.core.calendar.lock().unwrap().stored_reads.len();
    // back on the first week: still the store's, so it is read again, and the read replaces them
    assert_eq!(shown_ids(&view, cx), ["fresh"]);
    cx.update(|_, cx| view.update(cx, |view, cx| view.step(true, cx)));
    draw(cx);
    assert_eq!(
        rig.core.calendar.lock().unwrap().stored_reads.len(),
        stored_reads,
        "a span read in this run isn't drawn from the store again"
    );
}

#[test]
fn a_refresh_requested_during_a_read_runs_after_the_first_answer() {
    let rig = Rig::start();
    rig.core.calendar.lock().unwrap().hold = true;
    let (view, cx, _) = window(&rig);
    assert_eq!(
        rig.core.calendar.lock().unwrap().reads.len(),
        1,
        "same-span reads coalesce"
    );
    let answer = rig.core.calendar.lock().unwrap().held.remove(0);
    rig.core.calendar.lock().unwrap().hold = false;
    rig.core.calendar.lock().unwrap().events = vec![event("latest", 10)];
    answer.send(Ok(vec![event("stale", 9)]));
    draw(cx);
    assert_eq!(
        rig.core.calendar.lock().unwrap().reads.len(),
        2,
        "showing requested a fresh read"
    );
    assert_eq!(
        cx.update(|_, cx| view.read(cx).events().unwrap()[0].event.id.clone()),
        "latest"
    );
}

#[test]
fn canceling_an_organizers_meeting_requires_confirmation_and_refreshes_after_success() {
    let rig = Rig::start();
    let mut meeting = event("meeting", 9);
    meeting.attendees.push(teams_proto::Attendee {
        name: Some("Synthetic attendee".to_owned()),
        address: Some("guest@lab.example.test".to_owned()),
        ..teams_proto::Attendee::default()
    });
    let (view, cx, _) = window(&rig);
    let shown = Shown::of(std::rc::Rc::new(meeting)).unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown.clone(), cx)));
    draw(cx);
    click(cx, "calendar-card-remove");
    assert!(rig.core.calendar.lock().unwrap().writes.is_empty());
    click(cx, "calendar-card-keep");
    assert!(cx.debug_bounds("calendar-card-confirm").is_none());
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.close_card(cx);
            view.open_card(shown, cx);
        });
    });
    rig.core
        .calendar
        .lock()
        .unwrap()
        .write_answers
        .push_back(Ok(EventWritten { created: None }));
    click(cx, "calendar-card-remove");
    click(cx, "calendar-card-remove-confirm");
    assert!(
        matches!(rig.core.calendar.lock().unwrap().writes.as_slice(), [EventWrite::Cancel { id, .. }] if id == "meeting")
    );
    assert!(cx.update(|_, cx| view.read(cx).card().is_none()));
}

/// The account's teams: one holding the made-up channel, with its mailbox.
fn channel_team() -> teams_store::Team {
    teams_store::Team {
        id: fixtures::CHANNEL.to_owned(),
        name: "Lab team".to_owned(),
        archived: false,
        group_id: None,
        mailbox: Some(fixtures::TEAM_MAILBOX.to_owned()),
        owner: true,
        can_add_channel: true,
        can_add_private_channel: true,
        about: teams_store::TeamAbout::default(),
        channels: vec![teams_store::TeamChannel {
            id: fixtures::CHANNEL.to_owned(),
            name: "General".to_owned(),
            general: true,
            kind: teams_store::ChannelKind::Standard,
            about: teams_store::ChannelAbout::default(),
            shown: true,
        }],
    }
}

/// The account's own channel meeting as its calendar holds it: an attendee's copy (`isOrganizer` `false`).
fn own_channel_meeting() -> teams_core::CalendarEvent {
    let mut event = fixtures::channel_full(common::LAB_A).event;
    event.is_organizer = false;
    event.response = EventResponse::Accepted;
    event
}

#[test]
fn the_card_of_a_channel_meeting_the_account_scheduled_edits_it_from_the_team_s_calendar() {
    let rig = Rig::start();
    *rig.core.teams.lock().unwrap() = Some(vec![channel_team()]);
    rig.core.calendar.lock().unwrap().full.insert(
        fixtures::CHANNEL_EVENT.to_owned(),
        fixtures::channel_full(common::LAB_A),
    );
    let (view, cx, _) = window(&rig);
    let shown = Shown::of(std::rc::Rc::new(own_channel_meeting())).unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown, cx)));
    draw(cx);
    assert!(cx.debug_bounds("calendar-card-edit").is_some(), "its organizer's Edit");
    assert!(cx.debug_bounds("calendar-card-remove").is_some(), "and Cancel meeting");
    click(cx, "calendar-card-edit");
    // the card read its own copy for whom it invites; Edit reads the team's
    assert_eq!(
        rig.core.calendar.lock().unwrap().full_reads.last(),
        Some(&format!("{}/{}", fixtures::TEAM_MAILBOX, fixtures::CHANNEL_EVENT))
    );
    let form = cx
        .update(|_, cx| teams_ui::event_form::shown(cx))
        .expect("the form opens");
    cx.update(|_, cx| {
        let form = form.read(cx);
        assert!(form.changes());
        assert_eq!(
            form.place().map(|place| place.channel.mailbox.as_str()),
            Some(fixtures::TEAM_MAILBOX)
        );
    });
    cx.update(|_, cx| teams_ui::event_form::close(cx));
    draw(cx);
}

#[test]
fn the_card_of_a_channel_meeting_the_account_scheduled_cancels_it_on_the_team_s_calendar() {
    let rig = Rig::start();
    *rig.core.teams.lock().unwrap() = Some(vec![channel_team()]);
    rig.core
        .calendar
        .lock()
        .unwrap()
        .write_answers
        .push_back(Ok(EventWritten { created: None }));
    let (view, cx, _) = window(&rig);
    let shown = Shown::of(std::rc::Rc::new(own_channel_meeting())).unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown, cx)));
    click(cx, "calendar-card-remove");
    click(cx, "calendar-card-remove-confirm");
    let calendar = rig.core.calendar.lock().unwrap();
    let [
        EventWrite::CancelInChannel {
            meeting, channel, post, ..
        },
    ] = calendar.writes.as_slice()
    else {
        panic!("one cancellation in the channel: {:?}", calendar.writes);
    };
    assert_eq!(meeting.ical_uid, fixtures::CHANNEL_EVENT);
    assert_eq!(channel.mailbox, fixtures::TEAM_MAILBOX);
    assert_eq!(post, "1793680050510");
}

#[test]
fn the_read_only_card_offers_no_edit_or_remove_and_escape_closes_it() {
    let rig = Rig::start();
    *rig.core.read_only.lock().unwrap() = true;
    let (view, cx, _) = window(&rig);
    let shown = Shown::of(std::rc::Rc::new(event("read-only", 9))).unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown, cx)));
    draw(cx);
    assert!(cx.debug_bounds("calendar-card").is_some());
    assert!(cx.debug_bounds("calendar-card-edit").is_none());
    assert!(cx.debug_bounds("calendar-card-remove").is_none());
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(cx.update(|_, cx| view.read(cx).card().is_none()));
    assert!(rig.core.calendar.lock().unwrap().writes.is_empty());
}

/// A Teams meeting nobody started, of someone else's series, as the calendar view gives one.
fn series_meeting(hour: u8) -> teams_core::CalendarEvent {
    let mut meeting = event("series-occurrence", hour);
    meeting.is_organizer = false;
    meeting.online = true;
    meeting.kind = teams_core::EventKind::Occurrence;
    meeting.join_url = Some("https://teams.microsoft.com/l/meetup-join/made-up".to_owned());
    meeting.meeting_chat = Some("19:meeting_MadeUpMeeting0001@thread.v2".to_owned());
    meeting.meeting = Some(teams_core::MeetingInfo {
        organizer_id: "00000000-0000-4000-8000-00000000000b".to_owned(),
        tenant_id: "00000000-0000-4000-8000-0000000000f1".to_owned(),
    });
    meeting
}

fn calling_rig() -> Rig {
    Rig::start_with(|cx| teams_ui::Models::with_calls(None, cx))
}

#[test]
fn a_meetings_card_offers_join_any_time_and_join_goes_to_its_pre_join() {
    let rig = calling_rig();
    let (view, cx, _) = window(&rig);
    let told = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events = Arc::clone(&told);
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &CalendarViewEvent, _| {
            if let CalendarViewEvent::Join(to_join) = event {
                events.lock().unwrap().push(to_join.clone());
            }
        })
        .detach();
    });
    let shown = Shown::of(std::rc::Rc::new(series_meeting(9))).unwrap();
    let title = shown.title();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown, cx)));
    draw(cx);
    assert!(
        cx.debug_bounds("calendar-card-join-more").is_some(),
        "its menu's chevron"
    );
    assert!(cx.debug_bounds("calendar-card-chat").is_some());
    assert!(
        cx.debug_bounds("calendar-card-copy-link").is_none(),
        "copying is in Join's menu, as in Teams"
    );
    assert!(
        cx.debug_bounds("calendar-card-not-started").is_none(),
        "no note of waiting"
    );
    click(cx, "calendar-card-join");
    let told = told.lock().unwrap();
    assert_eq!(told.len(), 1);
    assert_eq!(told[0].chat(), Some("19:meeting_MadeUpMeeting0001@thread.v2"));
    assert_eq!(told[0].title(), title);
    assert!(cx.update(|_, cx| view.read(cx).card().is_none()), "the card gives way");
}

#[test]
fn joins_menu_offers_join_and_copy_join_link() {
    let rig = calling_rig();
    let (view, cx, _) = window(&rig);
    let shown = Shown::of(std::rc::Rc::new(series_meeting(9))).unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown, cx)));
    click(cx, "calendar-card-join-more");
    assert!(cx.debug_bounds("calendar-card-join-now").is_some());
    click(cx, "calendar-card-copy-link");
    assert_eq!(
        cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
            .as_deref(),
        Some("https://teams.microsoft.com/l/meetup-join/made-up")
    );
}

#[test]
fn the_read_only_card_offers_no_join_but_still_copies_its_link() {
    let rig = calling_rig();
    *rig.core.read_only.lock().unwrap() = true;
    let (view, cx, _) = window(&rig);
    let shown = Shown::of(std::rc::Rc::new(series_meeting(9))).unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown, cx)));
    draw(cx);
    assert!(cx.debug_bounds("calendar-card-join").is_none());
    assert!(cx.debug_bounds("calendar-card-not-started").is_none());
    assert!(cx.debug_bounds("calendar-card-chat").is_some());
    assert!(cx.debug_bounds("calendar-card-copy-link").is_some());
}

#[test]
fn calendar_controls_toggle_visibility_choose_dates_and_open_their_panels() {
    let rig = Rig::start();
    rig.core.calendar.lock().unwrap().events = vec![event("visible", 9)];
    let (view, cx, _) = window(&rig);
    click(cx, "calendar-mini-next");
    click(cx, "calendar-mini-previous");
    click(cx, "calendar-mini-fold");
    click(cx, "calendar-fold");
    click(cx, "calendar-show-all");
    click(cx, "calendar-show-all");
    click(cx, "calendar-mine-fold");
    assert!(cx.debug_bounds("calendar-own").is_none());
    click(cx, "calendar-mine-fold");
    assert!(cx.debug_bounds("calendar-own").is_some());
    click(cx, "calendar-own");
    assert!(cx.debug_bounds("calendar-event-visible").is_none());
    click(cx, "calendar-own");
    click(cx, "calendar-dates");
    assert!(cx.debug_bounds("calendar-picker").is_some());
    click(cx, "calendar-picker-next");
    click(cx, "calendar-picker-previous");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(cx.debug_bounds("calendar-picker").is_none());
    click(cx, "calendar-add");
    assert!(cx.debug_bounds("calendar-add-dialog").is_some());
    click(cx, "calendar-add-close");
    assert!(cx.debug_bounds("calendar-add-dialog").is_none());
    // Join with an ID, in Meet now's menu: its panel, an ID that isn't one refused there, closed again
    click(cx, "calendar-meet-now-more");
    click(cx, "calendar-join-id");
    assert!(cx.debug_bounds("calendar-join-id-panel").is_some());
    click(cx, "calendar-join-id-join");
    assert!(cx.debug_bounds("calendar-join-id-problem").is_some());
    click(cx, "calendar-join-id-close");
    assert!(cx.debug_bounds("calendar-join-id-panel").is_none());
    // Day, in the view menu's Day submenu: one day
    click(cx, "calendar-mode");
    cx.simulate_keystrokes("down right");
    if cx.debug_bounds("calendar-days-1").is_none() {
        cx.simulate_keystrokes("left");
    }
    click(cx, "calendar-days-1");
    assert_eq!(cx.update(|_, cx| view.read(cx).mode()), Mode::Day);
    click(cx, "calendar-next");
    assert_eq!(cx.update(|_, cx| view.read(cx).anchor()), date(28));
    click(cx, "calendar-previous");
    assert_eq!(cx.update(|_, cx| view.read(cx).anchor()), date(27));
}

#[test]
fn all_day_recurring_and_canceled_events_remain_navigable_in_week_and_month_views() {
    let rig = Rig::start();
    let mut all_day = event("all-day", 0);
    all_day.all_day = true;
    let mut tentative = event("tentative", 9);
    tentative.is_organizer = false;
    tentative.organizer_name = Some("Synthetic organizer".to_owned());
    tentative.response = EventResponse::Tentative;
    tentative.kind = teams_proto::EventKind::Occurrence;
    let mut canceled = event("canceled", 10);
    canceled.cancelled = true;
    let mut short = event("short", 12);
    short.is_organizer = false;
    short.organizer_name = Some("Another synthetic organizer".to_owned());
    let teams_proto::EventTime::Floating(end) = &mut short.end else {
        unreachable!()
    };
    end.hour = 12;
    end.minute = 15;
    rig.core.calendar.lock().unwrap().events = vec![all_day, tentative, canceled, short];
    let (view, cx, _) = window(&rig);
    assert!(cx.debug_bounds("calendar-all-day").is_some());
    click(cx, "calendar-event-all-day");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).card().unwrap().event.id.clone()),
        "all-day"
    );
    cx.update(|_, cx| view.update(cx, CalendarView::close_card));
    click(cx, "calendar-day-2026-10-27");
    assert_eq!(cx.update(|_, cx| view.read(cx).mode()), Mode::Day);
    click(cx, "calendar-event-tentative");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).card().unwrap().event.id.clone()),
        "tentative"
    );
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.close_card(cx);
            view.set_mode(Mode::Month, cx);
        });
    });
    draw(cx);
    assert!(cx.debug_bounds("calendar-event-canceled").is_some());
    click(cx, "calendar-event-canceled");
    assert!(cx.update(|_, cx| view.read(cx).card().unwrap().event.cancelled));
    assert!(rig.core.calendar.lock().unwrap().writes.is_empty());
}

#[test]
fn double_clicking_an_empty_half_hour_opens_an_unsaved_draft_at_that_time() {
    let rig = Rig::start();
    let (_, cx, _) = window(&rig);
    let at = cx.debug_bounds("calendar-slot-2026-10-27-20").unwrap().center();
    cx.simulate_event(gpui::MouseDownEvent {
        position: at,
        button: gpui::MouseButton::Left,
        modifiers: Modifiers::none(),
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_event(gpui::MouseUpEvent {
        position: at,
        button: gpui::MouseButton::Left,
        modifiers: Modifiers::none(),
        click_count: 2,
    });
    draw(cx);
    cx.update(|_, cx| {
        let form = teams_ui::event_form::shown(cx).unwrap();
        assert_eq!(form.read(cx).slot().start, date(27).and_hms_opt(10, 0, 0).unwrap());
        teams_ui::event_form::close(cx);
    });
    assert!(rig.core.calendar.lock().unwrap().writes.is_empty());
}

#[test]
fn a_drag_across_half_hours_selects_them_and_new_schedules_the_selection() {
    let rig = Rig::start();
    rig.core.calendar.lock().unwrap().events = vec![event("busy", 14)];
    let (view, cx, _) = window(&rig);
    // a click on an event opens its card and selects nothing behind it
    click(cx, "calendar-event-busy");
    assert!(cx.update(|_, cx| view.read(cx).card().is_some()));
    assert!(cx.update(|_, cx| view.read(cx).selection().is_none()));
    cx.update(|_, cx| view.update(cx, CalendarView::close_card));
    draw(cx);
    let from = cx.debug_bounds("calendar-slot-2026-10-28-20").unwrap().center();
    let to = cx.debug_bounds("calendar-slot-2026-10-28-23").unwrap().center();
    cx.simulate_event(gpui::MouseDownEvent {
        position: from,
        button: gpui::MouseButton::Left,
        modifiers: Modifiers::none(),
        click_count: 1,
        first_mouse: false,
    });
    cx.simulate_event(gpui::MouseMoveEvent {
        position: to,
        pressed_button: Some(gpui::MouseButton::Left),
        modifiers: Modifiers::none(),
    });
    cx.simulate_event(gpui::MouseUpEvent {
        position: to,
        button: gpui::MouseButton::Left,
        modifiers: Modifiers::none(),
        click_count: 1,
    });
    draw(cx);
    assert!(
        cx.debug_bounds("calendar-selection").is_some(),
        "the selection is drawn"
    );
    let slot = cx.update(|_, cx| view.read(cx).next_slot());
    assert_eq!(slot.start, date(28).and_hms_opt(10, 0, 0).unwrap());
    assert_eq!(slot.end, date(28).and_hms_opt(12, 0, 0).unwrap());
    // moving without the button pressed changes nothing
    cx.simulate_event(gpui::MouseMoveEvent {
        position: from,
        pressed_button: None,
        modifiers: Modifiers::none(),
    });
    click(cx, "calendar-new");
    cx.update(|_, cx| {
        let form = teams_ui::event_form::shown(cx).unwrap();
        assert_eq!(form.read(cx).slot().start, date(28).and_hms_opt(10, 0, 0).unwrap());
        assert_eq!(form.read(cx).slot().end, date(28).and_hms_opt(12, 0, 0).unwrap());
        teams_ui::event_form::close(cx);
    });
    // Escape drops the selection; New then takes the next half hour
    cx.simulate_keystrokes("escape");
    assert!(cx.update(|_, cx| view.read(cx).selection().is_none()));
    assert!(rig.core.calendar.lock().unwrap().writes.is_empty());
}

#[test]
fn the_card_reads_its_event_whole_for_whom_it_invites_and_cancels_the_organizer_s_meeting() {
    let rig = Rig::start();
    // the calendar view names no attendees; the event read whole does, with their answers
    let body = serde_json::json!({
        "objectId": "own-meeting",
        "subject": "Synthetic meeting",
        "startTime": "2026-10-27T09:00:00",
        "endTime": "2026-10-27T10:00:00",
        "isOrganizer": true,
        "myResponseType": "Organizer",
        "attendees": [
            {"name": "Synthetic attendee", "address": "guest@lab.example.test",
             "status": {"response": "Accepted"}},
            {"name": "Synthetic optional", "address": "optional@lab.example.test", "type": "Optional",
             "status": {"response": "None"}}
        ]
    });
    let full = teams_proto::EventRead::new("own-meeting")
        .unwrap()
        .read_answer(&serde_json::to_vec(&body).unwrap())
        .unwrap();
    rig.core
        .calendar
        .lock()
        .unwrap()
        .full
        .insert("own-meeting".to_owned(), full);
    let (view, cx, _) = window(&rig);
    let shown = Shown::of(std::rc::Rc::new(event("own-meeting", 9))).unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown, cx)));
    draw(cx);
    assert!(cx.debug_bounds("calendar-card-attendees").is_some());
    let answers = cx.update(|_, cx| {
        view.read(cx).card().map(|shown| {
            shown
                .event
                .attendees
                .iter()
                .map(|attendee| attendee.response.clone())
                .collect::<Vec<_>>()
        })
    });
    assert_eq!(
        answers,
        Some(vec![Some(EventResponse::Accepted), Some(EventResponse::NoResponse)])
    );
    answered(&rig);
    click(cx, "calendar-card-remove");
    click(cx, "calendar-card-remove-confirm");
    assert!(matches!(
        rig.core.calendar.lock().unwrap().writes.as_slice(),
        [EventWrite::Cancel { id, message: None, .. }] if id == "own-meeting"
    ));
}

#[test]
fn the_filter_hides_the_kinds_unticked_and_clear_filters_shows_them_again() {
    let rig = Rig::start();
    let mut series = invitation("series", 9);
    series.kind = teams_core::EventKind::Occurrence;
    series.online = true;
    rig.core.calendar.lock().unwrap().events = vec![event("own", 11), series];
    let (view, cx, _) = window(&rig);
    assert!(cx.debug_bounds("calendar-event-series").is_some());
    click(cx, "calendar-filter");
    click(cx, "calendar-filter-recurring");
    assert!(cx.update(|_, cx| view.read(cx).hidden().hides(teams_ui::calendar_view::Shows::Recurring)));
    assert!(
        cx.debug_bounds("calendar-event-series").is_none(),
        "a series' occurrence is hidden"
    );
    assert!(cx.debug_bounds("calendar-event-own").is_some(), "a single event stays");
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.flip_shown(teams_ui::calendar_view::Shows::Organized, cx);
        });
    });
    draw(cx);
    assert!(
        cx.debug_bounds("calendar-event-own").is_none(),
        "the account's own is hidden too"
    );
    click(cx, "calendar-filter");
    click(cx, "calendar-filter-clear");
    assert!(!cx.update(|_, cx| view.read(cx).hidden().any()));
    assert!(cx.debug_bounds("calendar-event-series").is_some());
    assert!(cx.debug_bounds("calendar-event-own").is_some());
}

/// Someone else's meeting the account hasn't answered, as the calendar view gives one.
fn invitation(id: &str, hour: u8) -> teams_core::CalendarEvent {
    let mut meeting = event(id, hour);
    meeting.is_organizer = false;
    meeting.response = EventResponse::NotResponded;
    meeting.organizer_name = Some("Synthetic organizer".to_owned());
    meeting
}

fn answered(rig: &Rig) {
    rig.core
        .calendar
        .lock()
        .unwrap()
        .write_answers
        .push_back(Ok(EventWritten { created: None }));
}

#[test]
fn an_invitation_s_card_accepts_it_with_a_message_and_shows_the_answer() {
    let rig = Rig::start();
    let (view, cx, _) = window(&rig);
    let shown = Shown::of(std::rc::Rc::new(invitation("invited", 9))).unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown, cx)));
    draw(cx);
    assert!(
        cx.debug_bounds("calendar-card-rsvp-scope").is_none(),
        "a single event offers no series"
    );
    answered(&rig);
    click(cx, "calendar-card-rsvp-note");
    cx.simulate_input("See you there");
    // typed into the message, a "t" is a letter, not Today
    assert_eq!(cx.update(|_, cx| view.read(cx).anchor()), date(27));
    click(cx, "calendar-card-rsvp-accept");
    assert!(matches!(
        rig.core.calendar.lock().unwrap().writes.as_slice(),
        [EventWrite::Respond { id, answer: teams_core::RsvpAnswer::Accept, send: true, comment: Some(note) }]
            if id == "invited" && note == "See you there"
    ));
    let response = cx.update(|_, cx| view.read(cx).card().map(|shown| shown.event.response.clone()));
    assert_eq!(
        response,
        Some(EventResponse::Accepted),
        "the card stays open on the answer"
    );
}

#[test]
fn an_occurrence_is_answered_for_its_series_or_declined_without_telling_the_organizer() {
    let rig = Rig::start();
    let (view, cx, _) = window(&rig);
    let mut occurrence = invitation("occurrence", 10);
    occurrence.kind = teams_core::EventKind::Occurrence;
    occurrence.series_master_id = Some("master".to_owned());
    let shown = Shown::of(std::rc::Rc::new(occurrence)).unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown, cx)));
    answered(&rig);
    click(cx, "calendar-card-rsvp-scope");
    click(cx, "calendar-rsvp-scope-series");
    click(cx, "calendar-card-rsvp-tentative");
    assert!(matches!(
        rig.core.calendar.lock().unwrap().writes.as_slice(),
        [EventWrite::Respond { id, answer: teams_core::RsvpAnswer::Tentative, send: true, comment: None }]
            if id == "master"
    ));

    // "Email organizer" unticked: this occurrence declined, nothing sent to the organizer, and the card closes
    answered(&rig);
    click(cx, "calendar-card-rsvp-scope");
    click(cx, "calendar-rsvp-scope-event");
    click(cx, "calendar-card-rsvp-email");
    click(cx, "calendar-card-rsvp-decline");
    assert!(matches!(
        rig.core.calendar.lock().unwrap().writes.last(),
        Some(EventWrite::Respond { id, answer: teams_core::RsvpAnswer::Decline, send: false, comment: None })
            if id == "occurrence"
    ));
    assert!(cx.update(|_, cx| view.read(cx).card().is_none()));
}

#[test]
fn an_organizer_cancels_a_whole_series_with_a_message_and_an_appointment_offers_no_answer() {
    let rig = Rig::start();
    let (view, cx, _) = window(&rig);
    let appointment = Shown::of(std::rc::Rc::new(event("appointment", 8))).unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(appointment, cx)));
    draw(cx);
    assert!(
        cx.debug_bounds("calendar-card-rsvp").is_none(),
        "the account's own event asks no answer"
    );

    let mut occurrence = event("own-occurrence", 11);
    occurrence.kind = teams_core::EventKind::Occurrence;
    occurrence.series_master_id = Some("own-master".to_owned());
    occurrence.meeting_chat = Some("19:meeting_MadeUpSeries@thread.v2".to_owned());
    occurrence.attendees.push(teams_proto::Attendee {
        name: Some("Synthetic attendee".to_owned()),
        address: Some("guest@lab.example.test".to_owned()),
        ..teams_proto::Attendee::default()
    });
    let shown = Shown::of(std::rc::Rc::new(occurrence)).unwrap();
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.close_card(cx);
            view.open_card(shown, cx);
        });
    });
    answered(&rig);
    click(cx, "calendar-card-remove");
    click(cx, "calendar-card-remove-series");
    click(cx, "calendar-card-cancel-note");
    cx.simulate_input("Moving to next month");
    click(cx, "calendar-card-remove-confirm");
    assert!(matches!(
        rig.core.calendar.lock().unwrap().writes.as_slice(),
        [EventWrite::Cancel { id, message: Some(message), .. }] if id == "own-master" && message == "Moving to next month"
    ));
}

#[test]
fn day_view_shows_one_to_seven_days_and_steps_by_them() {
    let rig = Rig::start();
    let (view, cx, _) = window(&rig);
    cx.update(|_, cx| view.update(cx, |view, cx| view.set_days(3, cx)));
    draw(cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).mode()), Mode::Day);
    let span = cx.update(|_, cx| view.read(cx).span());
    assert_eq!((span.first, span.days), (date(27), 3));
    assert_eq!(cx.update(|_, cx| view.read(cx).title()), "October 27–29, 2026");
    assert!(cx.debug_bounds("calendar-day-2026-10-29").is_some());
    assert!(cx.debug_bounds("calendar-day-2026-10-30").is_none());
    click(cx, "calendar-next");
    assert_eq!(cx.update(|_, cx| view.read(cx).anchor()), date(30));
    click(cx, "calendar-previous");
    assert_eq!(cx.update(|_, cx| view.read(cx).anchor()), date(27));
    // out of range asks are held to a week, and one day is the day's own title
    cx.update(|_, cx| view.update(cx, |view, cx| view.set_days(9, cx)));
    assert_eq!(cx.update(|_, cx| view.read(cx).days()), 7);
    cx.update(|_, cx| view.update(cx, |view, cx| view.set_days(1, cx)));
    assert_eq!(cx.update(|_, cx| view.read(cx).title()), "Tuesday, October 27, 2026");
    // from the work week, a day count shows Day view
    cx.update(|_, cx| view.update(cx, |view, cx| view.set_mode(Mode::WorkWeek, cx)));
    cx.update(|_, cx| view.update(cx, |view, cx| view.set_days(2, cx)));
    assert_eq!(cx.update(|_, cx| view.read(cx).mode()), Mode::Day);
    assert_eq!(cx.update(|_, cx| view.read(cx).span().days), 2);
}

fn person(address: &str, name: &str) -> teams_core::Invitable {
    teams_core::Invitable {
        mri: String::new(),
        name: name.to_owned(),
        address: address.to_owned(),
    }
}

fn busy_at(address: &str, day: u32, hour: u32) -> teams_proto::PersonSchedule {
    let start = chrono::Local
        .from_local_datetime(&date(day).and_hms_opt(hour, 0, 0).unwrap())
        .unwrap()
        .timestamp_millis();
    teams_proto::PersonSchedule {
        address: address.to_owned(),
        busy: vec![teams_proto::BusyTime {
            status: teams_proto::FreeBusy::Busy,
            start_ms: start,
            end_ms: start + 3_600_000,
            subject: None,
        }],
    }
}

#[test]
fn add_calendar_finds_a_person_adds_their_calendar_and_shows_their_busy_times() {
    let rig = Rig::start();
    {
        let mut calendar = rig.core.calendar.lock().unwrap();
        calendar.people = vec![person("avery@example.com", "Avery Example")];
        calendar.schedules = vec![busy_at("avery@example.com", 28, 10)];
    }
    let (view, cx, written) = window(&rig);
    click(cx, "calendar-add");
    cx.simulate_input("Ave");
    cx.executor().advance_clock(std::time::Duration::from_millis(400));
    draw(cx);
    click(cx, "calendar-add-found-0");
    assert!(cx.debug_bounds("calendar-add-picked").is_some());
    // the person picked takes the box's place, as Teams' does
    assert!(cx.debug_bounds("calendar-add-query").is_none());
    click(cx, "calendar-add-add");
    assert!(cx.debug_bounds("calendar-add-dialog").is_none());
    let people = cx.update(|_, cx| view.read(cx).people().to_vec());
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].address, "avery@example.com");
    assert!(people[0].ticked);
    // their busy time is read for the days shown and drawn, opening no card
    let reads = rig.core.calendar.lock().unwrap().schedule_reads.clone();
    assert_eq!(reads.last().unwrap().0, ["avery@example.com"]);
    assert_eq!(reads.last().unwrap().2, 5);
    draw(cx);
    assert!(cx.debug_bounds("calendar-people").is_some());
    let busy = format!(
        "calendar-event-person-avery@example.com-{}",
        rig.core.calendar.lock().unwrap().schedules[0].busy[0].start_ms
    );
    let busy: &'static str = Box::leak(busy.into_boxed_str());
    click(cx, busy);
    assert!(cx.update(|_, cx| view.read(cx).card().is_none()));
    split_close_and_add_again(&view, cx);
    let trace = written.text();
    assert!(trace.contains("person-added") && trace.contains("person-removed"));
    assert!(rig.core.calendar.lock().unwrap().writes.is_empty());
}

#[test]
fn meet_now_starts_a_meeting_of_its_own_or_gives_its_link() {
    let rig = Rig::start();
    rig.core.calendar.lock().unwrap().meet_now = Some(teams_proto::MeetNowMeeting {
        thread: "19:meeting_made-up@thread.v2".to_owned(),
        join: "https://teams.microsoft.com/l/meetup-join/made-up".to_owned(),
        share: "https://teams.microsoft.com/meet/1?p=made-up".to_owned(),
    });
    let (view, cx, _) = window(&rig);
    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = std::rc::Rc::clone(&events);
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &CalendarViewEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
    });
    // Get a link to share: the meeting made, its link shown with Share via email
    click(cx, "calendar-meet-now");
    assert!(cx.debug_bounds("calendar-meet-now-dialog").is_some());
    click(cx, "calendar-meet-now-share");
    assert!(cx.debug_bounds("calendar-meet-now-link").is_some());
    assert!(cx.debug_bounds("calendar-meet-now-email").is_some());
    click(cx, "calendar-meet-now-copy");
    assert_eq!(
        cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
            .as_deref(),
        Some("https://teams.microsoft.com/meet/1?p=made-up")
    );
    // Start meeting then joins the meeting made, without making another
    click(cx, "calendar-meet-now-start");
    assert!(cx.debug_bounds("calendar-meet-now-dialog").is_none());
    assert_eq!(rig.core.calendar.lock().unwrap().meet_now_asked.len(), 1);
    assert!(
        matches!(&events.borrow()[..], [CalendarViewEvent::JoinChat { chat, link: Some(_), .. }]
        if chat == "19:meeting_made-up@thread.v2")
    );
    // Start meeting from the dialog: made and joined at once
    click(cx, "calendar-meet-now");
    click(cx, "calendar-meet-now-start");
    assert_eq!(rig.core.calendar.lock().unwrap().meet_now_asked.len(), 2);
    assert_eq!(events.borrow().len(), 2);
    assert_eq!(
        teams_ui::calendar_view::mail_link("Team sync", "https://x/y?p=1"),
        "mailto:?subject=Team%20sync&body=Join%20the%20meeting%3A%20https%3A%2F%2Fx%2Fy%3Fp%3D1"
    );
}

#[test]
fn a_clicked_event_s_card_opens_beside_it() {
    let rig = Rig::start();
    rig.core.calendar.lock().unwrap().events = vec![event("busy", 14)];
    let (view, cx, _) = window(&rig);
    let block = cx.debug_bounds("calendar-event-busy").unwrap();
    click(cx, "calendar-event-busy");
    let card = cx.debug_bounds("calendar-card").unwrap();
    // beside the point clicked, not centred over the grid
    assert!(card.left() > block.center().x || card.right() < block.center().x);
    assert!(cx.update(|_, cx| view.read(cx).card().is_some()));
}

#[test]
fn with_a_peek_open_another_event_opens_its_peek_and_a_double_click_edits_the_account_s_own() {
    let rig = Rig::start();
    rig.core.calendar.lock().unwrap().events = vec![event("early", 11), event("busy", 14)];
    rig.core
        .calendar
        .lock()
        .unwrap()
        .full
        .insert("busy".to_owned(), fixtures::full("busy", 14));
    let (view, cx, _) = window(&rig);
    let carded =
        |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).card().map(|shown| shown.event.id.clone()));
    // one click on another event while a peek is open shows that event's peek, as Teams does (the earlier event lies
    // above the later one's peek)
    click(cx, "calendar-event-busy");
    assert_eq!(carded(cx).as_deref(), Some("busy"));
    click(cx, "calendar-event-early");
    assert_eq!(
        carded(cx).as_deref(),
        Some("early"),
        "the other event's peek, in one click"
    );
    cx.update(|_, cx| view.update(cx, CalendarView::close_card));
    draw(cx);
    // a double click on the account's own event: the first click's peek doesn't swallow the second
    let at = cx.debug_bounds("calendar-event-busy").unwrap().center();
    for count in 1..=2 {
        cx.simulate_event(gpui::MouseDownEvent {
            position: at,
            button: gpui::MouseButton::Left,
            modifiers: Modifiers::none(),
            click_count: count,
            first_mouse: false,
        });
        cx.simulate_event(gpui::MouseUpEvent {
            position: at,
            button: gpui::MouseButton::Left,
            modifiers: Modifiers::none(),
            click_count: count,
        });
        draw(cx);
    }
    assert!(carded(cx).is_none(), "the peek gives way to the form");
    cx.update(|_, cx| {
        assert!(teams_ui::event_form::shown(cx).is_some(), "the event opens to edit");
        teams_ui::event_form::close(cx);
    });
}

#[test]
fn the_wheel_beside_a_peek_scrolls_the_hours_and_closes_it_the_wheel_on_it_keeps_it() {
    let rig = Rig::start();
    rig.core.calendar.lock().unwrap().events = vec![event("busy", 14)];
    let (view, cx, _) = window(&rig);
    let wheel = |cx: &mut VisualTestContext, at: gpui::Point<gpui::Pixels>| {
        cx.simulate_event(gpui::ScrollEvent {
            position: at,
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-200.0))),
            modifiers: Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
            source: gpui::ScrollSource::Pointer,
        });
        draw(cx);
    };
    click(cx, "calendar-event-busy");
    let card = cx.debug_bounds("calendar-card").unwrap();
    wheel(cx, card.center());
    assert!(
        cx.update(|_, cx| view.read(cx).card().is_some()),
        "the wheel on the peek keeps it"
    );
    let before = cx.debug_bounds("calendar-event-busy").unwrap().origin.y;
    let hours = cx.debug_bounds("calendar-hours").unwrap();
    wheel(cx, gpui::point(hours.origin.x + px(4.0), hours.center().y));
    assert!(
        cx.update(|_, cx| view.read(cx).card().is_none()),
        "the wheel beside the peek closes it"
    );
    let after = cx.debug_bounds("calendar-event-busy").unwrap().origin.y;
    assert!(after < before, "the hours scrolled under it: {before:?} to {after:?}");
}

/// Split view shows the account's calendar and the person's apart; closing theirs unticks it; adding them again says
/// so; Remove takes them off the list.
fn split_close_and_add_again(view: &Entity<CalendarView>, cx: &mut VisualTestContext) {
    // Split view: the account's calendar and theirs in columns of their own
    cx.update(|_, cx| view.update(cx, |view, cx| view.set_split(true, cx)));
    draw(cx);
    assert!(cx.debug_bounds("calendar-split-0").is_some());
    assert!(cx.debug_bounds("calendar-split-1").is_some());
    // closing their columns unticks their calendar
    click(cx, "calendar-split-close-1");
    assert!(!cx.update(|_, cx| view.read(cx).people()[0].ticked));
    assert!(cx.debug_bounds("calendar-split-1").is_none());
    // adding them again says so
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_add_calendar(window, cx)));
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.pick_person(person("AVERY@example.com", "Avery"), window, cx);
        });
    });
    click(cx, "calendar-add-add");
    assert!(cx.debug_bounds("calendar-add-problem").is_some());
    cx.update(|_, cx| view.update(cx, CalendarView::close_add_calendar));
    // Remove takes it off the list
    cx.update(|_, cx| view.update(cx, |view, cx| view.remove_person("avery@example.com", cx)));
    assert!(cx.update(|_, cx| view.read(cx).people().is_empty()));
}

/// An occurrence of the account's own made-up weekly series `own-master`, titled `subject`.
fn own_occurrence(id: &str, subject: &str) -> teams_core::CalendarEvent {
    let mut occurrence = event(id, 11);
    occurrence.subject = subject.to_owned();
    occurrence.kind = teams_core::EventKind::Occurrence;
    occurrence.series_master_id = Some("own-master".to_owned());
    occurrence
}

/// The series renamed, as the form's save sends it.
fn renamed_series() -> EventWrite {
    let at = teams_core::ZonedTime {
        ms: 1_793_091_600_000,
        offset_minutes: 0,
    };
    EventWrite::Update {
        id: "own-master".to_owned(),
        event: serde_json::Map::new(),
        draft: teams_core::EventDraft {
            subject: "Renamed sync".to_owned(),
            start: at,
            end: at,
            time_zone: None,
            zone: None,
            all_day: false,
            organizer: teams_core::Organizer {
                mri: String::new(),
                address: None,
                name: None,
            },
            attendees: Vec::new(),
            location: String::new(),
            body_html: String::new(),
            online: false,
            show_as: teams_core::EventShowAs::Busy,
            reminder_minutes: None,
            private: false,
            categories: Vec::new(),
            recurrence: None,
        },
    }
}

/// Whether any of `subjects` is the series'.
fn any_series(subjects: &[String]) -> bool {
    subjects.iter().any(|subject| subject.ends_with(" sync"))
}

/// The subjects the view shows now, by start.
fn shown_subjects(view: &Entity<CalendarView>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.update(|_, cx| {
        view.read(cx)
            .events()
            .map(|events| events.iter().map(|shown| shown.event.subject.clone()).collect())
            .unwrap_or_default()
    })
}

/// The read the view asked last, held, answered with `events`.
fn answer_held(rig: &Rig, cx: &mut VisualTestContext, events: Vec<teams_core::CalendarEvent>) {
    let held = rig.core.calendar.lock().unwrap().held.pop().expect("a read held");
    held.send(Ok(events));
    draw(cx);
}

/// Two work weeks read and held by the view, each with an occurrence of the account's series and the first with a
/// single event; the store kept them too. The view is back on the first week.
fn two_weeks_held(
    rig: &Rig,
) -> (
    Entity<CalendarView>,
    &'static mut VisualTestContext,
    teams_core::CalendarEvent,
) {
    let single = event("single", 9);
    rig.core.calendar.lock().unwrap().events = vec![own_occurrence("occ-1", "Weekly sync"), single.clone()];
    let (view, cx, _) = window(rig);
    rig.core.calendar.lock().unwrap().events = vec![own_occurrence("occ-2", "Weekly sync")];
    cx.update(|_, cx| view.update(cx, |view, cx| view.step(true, cx)));
    draw(cx);
    assert_eq!(shown_subjects(&view, cx), ["Weekly sync"]);
    cx.update(|_, cx| view.update(cx, |view, cx| view.step(false, cx)));
    draw(cx);
    rig.core.calendar.lock().unwrap().stored = vec![
        own_occurrence("occ-1", "Weekly sync"),
        single.clone(),
        own_occurrence("occ-2", "Weekly sync"),
    ];
    rig.core.calendar.lock().unwrap().hold = true;
    (view, cx, single)
}

/// `write` saved by the form (answered, the fake store settled) and the view told what it touched.
fn form_saved(rig: &Rig, view: &Entity<CalendarView>, cx: &mut VisualTestContext, write: EventWrite) {
    answered(rig);
    let touched = write.touched();
    drop(rig.core.calendar.lock().unwrap().write(write));
    cx.update(|_, cx| view.update(cx, |view, cx| view.written(&touched, cx)));
    draw(cx);
}

#[test]
fn no_week_or_view_shows_a_series_as_it_was_after_its_edit_or_its_delete() {
    let rig = Rig::start();
    let (view, cx, single) = two_weeks_held(&rig);
    let single_title = std::slice::from_ref(&single.subject);
    let with_series = [single.subject.clone(), "Renamed sync".to_owned()];

    // the series renamed: until the read answers, the week shows no occurrence as it was
    form_saved(&rig, &view, cx, renamed_series());
    assert_eq!(shown_subjects(&view, cx), single_title);
    answer_held(&rig, cx, vec![own_occurrence("occ-1", "Renamed sync"), single.clone()]);
    assert_eq!(shown_subjects(&view, cx), with_series);
    // the next week, held before the change: drawn from the store, which forgot the series, while it is read again
    cx.update(|_, cx| view.update(cx, |view, cx| view.step(true, cx)));
    draw(cx);
    assert!(
        !any_series(&shown_subjects(&view, cx)),
        "no copy from before the change"
    );
    answer_held(&rig, cx, vec![own_occurrence("occ-2", "Renamed sync")]);
    assert_eq!(shown_subjects(&view, cx), ["Renamed sync"]);
    rig.core.calendar.lock().unwrap().stored = vec![
        own_occurrence("occ-1", "Renamed sync"),
        single.clone(),
        own_occurrence("occ-2", "Renamed sync"),
    ];
    cx.update(|_, cx| view.update(cx, |view, cx| view.step(false, cx)));
    draw(cx);
    assert_eq!(shown_subjects(&view, cx), with_series);

    // the whole series deleted from its card, with a read under way that began before: neither this week, Week view
    // nor the next week shows an occurrence while their reads are held
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.set_shown(false, window, cx);
            view.set_shown(true, window, cx);
        });
    });
    let shown = Shown::of(std::rc::Rc::new(own_occurrence("occ-1", "Renamed sync"))).unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown, cx)));
    draw(cx);
    answered(&rig);
    click(cx, "calendar-card-remove");
    click(cx, "calendar-card-remove-series");
    click(cx, "calendar-card-remove-confirm");
    assert!(matches!(
        rig.core.calendar.lock().unwrap().writes.as_slice(),
        [_, EventWrite::Delete { id }] if id == "own-master"
    ));
    assert_eq!(shown_subjects(&view, cx), single_title);
    // the read from before the delete answers with the series as it was: dropped
    let before = rig.core.calendar.lock().unwrap().held.remove(0);
    before.send(Ok(vec![own_occurrence("occ-1", "Renamed sync"), single.clone()]));
    draw(cx);
    assert_eq!(shown_subjects(&view, cx), single_title);
    cx.update(|_, cx| view.update(cx, |view, cx| view.set_mode(Mode::Week, cx)));
    draw(cx);
    assert_eq!(shown_subjects(&view, cx), single_title, "drawn from the store");
    cx.update(|_, cx| view.update(cx, |view, cx| view.step(true, cx)));
    draw(cx);
    assert!(!any_series(&shown_subjects(&view, cx)));
}

#[test]
fn no_view_shows_a_single_event_as_it_was_after_its_edit_or_its_delete() {
    let rig = Rig::start();
    let (view, cx, single) = two_weeks_held(&rig);
    let renamed_single = |subject: &str| {
        let mut renamed = single.clone();
        renamed.subject = subject.to_owned();
        renamed
    };

    // renamed: until the read answers, the old title isn't shown, the series' occurrence is
    let EventWrite::Update { event, draft, .. } = renamed_series() else {
        unreachable!("an update")
    };
    let rename = EventWrite::Update {
        id: "single".to_owned(),
        event,
        draft,
    };
    form_saved(&rig, &view, cx, rename);
    assert_eq!(shown_subjects(&view, cx), ["Weekly sync"]);
    answer_held(
        &rig,
        cx,
        vec![own_occurrence("occ-1", "Weekly sync"), renamed_single("Renamed")],
    );
    assert_eq!(shown_subjects(&view, cx), ["Renamed", "Weekly sync"]);
    rig.core.calendar.lock().unwrap().stored = vec![own_occurrence("occ-1", "Weekly sync"), renamed_single("Renamed")];

    // deleted from its card: gone at once, and Week and Month view, drawn from the store, don't show it
    let shown = Shown::of(std::rc::Rc::new(renamed_single("Renamed"))).unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown, cx)));
    draw(cx);
    answered(&rig);
    click(cx, "calendar-card-remove");
    click(cx, "calendar-card-remove-confirm");
    assert!(matches!(
        rig.core.calendar.lock().unwrap().writes.as_slice(),
        [_, EventWrite::Delete { id }] if id == "single"
    ));
    assert_eq!(shown_subjects(&view, cx), ["Weekly sync"]);
    for mode in [Mode::Week, Mode::Month] {
        cx.update(|_, cx| view.update(cx, |view, cx| view.set_mode(mode, cx)));
        draw(cx);
        assert_eq!(shown_subjects(&view, cx), ["Weekly sync"], "{mode:?}");
    }
}

#[test]
fn a_meeting_meet_now_couldn_t_make_says_so_and_the_dialog_stays() {
    let rig = Rig::start();
    let (_, cx, _) = window(&rig);
    // a meeting that couldn't be made says so, and the dialog stays
    rig.core.calendar.lock().unwrap().meet_now = None;
    click(cx, "calendar-meet-now");
    click(cx, "calendar-meet-now-start");
    assert!(cx.debug_bounds("calendar-meet-now-problem").is_some());
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(cx.debug_bounds("calendar-meet-now-dialog").is_none());
    assert!(rig.core.calendar.lock().unwrap().writes.is_empty());
}

fn kept(account: &str, address: &str, name: &str) -> teams_ui::settings::PersonCalendar {
    teams_ui::settings::PersonCalendar {
        account: account.to_owned(),
        address: address.to_owned(),
        name: name.to_owned(),
    }
}

#[test]
fn a_person_s_calendar_shows_alone_or_goes_and_the_account_s_list_is_kept_in_the_settings() {
    use teams_ui::settings::{Settings, SettingsModel};
    let rig = Rig::start();
    let me = teams_ui::Core::me(&*rig.core).to_owned();
    let (view, cx, _written) = window(&rig);
    let other = kept("8:orgid:other-account", "quinn@example.com", "Quinn");
    let settings = cx.update(|_, cx| {
        cx.new(|_| {
            SettingsModel::in_memory(Settings {
                people_calendars: vec![kept(&me, "avery@example.com", "Avery"), other.clone()],
                ..Settings::default()
            })
        })
    });
    cx.update(|_, cx| view.update(cx, |view, cx| view.set_settings(settings.clone(), cx)));
    let addresses = |cx: &mut VisualTestContext| -> Vec<(String, bool)> {
        cx.update(|_, cx| {
            view.read(cx)
                .people()
                .iter()
                .map(|person| (person.address.clone(), person.ticked))
                .collect()
        })
    };
    assert_eq!(
        addresses(cx),
        [("avery@example.com".to_owned(), true)],
        "only the account's own"
    );
    let added = cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.add_person(&person("riley@example.com", "Riley"), cx)
        })
    });
    assert!(added);
    let again = cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.add_person(&person("RILEY@example.com", "Riley"), cx)
        })
    });
    assert!(!again, "a calendar listed already isn't added twice");
    let stored = cx.update(|_, cx| settings.read(cx).settings().people_calendars.clone());
    assert_eq!(
        stored,
        [
            other.clone(),
            kept(&me, "avery@example.com", "Avery"),
            kept(&me, "riley@example.com", "Riley")
        ]
    );

    // Riley's "…": Show this only
    draw(cx);
    click(cx, "calendar-person-more-1");
    click(cx, "calendar-person-only-1");
    assert_eq!(
        addresses(cx),
        [
            ("avery@example.com".to_owned(), false),
            ("riley@example.com".to_owned(), true)
        ]
    );
    // Avery's "…": Remove, kept so in the settings, the other account's entry as it was
    click(cx, "calendar-person-more-0");
    click(cx, "calendar-person-remove-0");
    assert_eq!(addresses(cx), [("riley@example.com".to_owned(), true)]);
    let stored = cx.update(|_, cx| settings.read(cx).settings().people_calendars.clone());
    assert_eq!(stored, [other, kept(&me, "riley@example.com", "Riley")]);
}
