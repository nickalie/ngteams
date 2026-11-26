//! A meeting's Request to join on the invitee's side: Accept opens the meeting's pre-join and sends nothing, Join now
//! answers the ring into the meeting, Cancel lets the ring ring on, and a ring that ends while the pre-join shows
//! leaves Join now to join the meeting by the organizer and tenant the ring named. Every name and id is made up.

use std::sync::Arc;

use gpui::{AppContext as _, VisualTestContext};
use teams_core::{CallAction, CallActionError, CallStatus, EngineError, Event, IncomingCall, RemoteEnd};
use teams_store::ConversationKind;

use super::{
    AB, CALLER_NAME, CallKind, FakeRinger, FakeServer, LAB_A, LAB_B, MEETING_CHAT, Rig, TRIO, actions, alert, call,
    call_windows, calling, calls_of, chat, click, drawn, invitation, join_meeting_action, lines, meeting_to_join,
    no_name_in, prejoin_window, registered, shell_window, status, trace,
};

/// The made-up meeting's title, as its chat's row names it.
const TITLE: &str = "Synthetic meeting";

/// The meeting's Request to join, numbered `number`: its chat and its organizer and tenant named.
fn meeting_ring(number: u64) -> IncomingCall {
    IncomingCall {
        meeting: meeting_to_join().meeting().cloned(),
        ..invitation(number, Some(CALLER_NAME), Some(MEETING_CHAT))
    }
}

fn listed_meeting(rig: &Rig) {
    rig.core
        .conversations
        .lock()
        .unwrap()
        .push(chat(MEETING_CHAT, ConversationKind::Meeting, Some(TITLE)));
}

fn said(
    shell: &gpui::Entity<teams_ui::shell::Shell>,
    cx: &mut VisualTestContext,
) -> Option<(String, Option<&'static str>)> {
    super::draw(cx);
    cx.update(|_, cx| shell.read(cx).call_bar().read(cx).said(cx))
        .map(|(text, answer)| (text.to_string(), answer))
}

fn ring(rig: &Rig) -> Option<u64> {
    rig.cx
        .read(|cx| calls_of(rig).read(cx).ringing().map(|ringing| ringing.call.get()))
}

fn prejoin(rig: &Rig) -> Option<teams_ui::calls::Prejoin> {
    rig.cx.read(|cx| calls_of(rig).read(cx).prejoin().cloned())
}

#[test]
fn accept_on_a_meetings_request_to_join_opens_its_pre_join_and_join_now_answers_into_the_meeting() {
    let rig = calling();
    listed_meeting(&rig);
    let (trace, written) = trace();
    let ringer = Arc::new(FakeRinger::default());
    let server = Arc::new(FakeServer::default());
    let _alert = alert(&rig, &trace, &ringer, &server);
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    rig.send(Event::IncomingCall(meeting_ring(3)));
    rig.cx.run_until_parked();
    assert_eq!(
        said(&shell, cx),
        Some((format!("{CALLER_NAME} wants you to join {TITLE}"), Some("Accept")))
    );
    assert_eq!(ringer.started(), 1);

    click(cx, "call-answer");
    let window = accepted(&rig, &windows, (&ringer, &server), (&shell, cx));
    assert_eq!(lines(&written, "call", "prejoin").len(), 1);
    joined_now(&rig, window);
    in_the_meeting(&rig, &windows);
    no_name_in(&written);
}

/// After Accept: the pre-join of the ring's meeting, nothing sent, the ring silent and its banner gone.
fn accepted(
    rig: &Rig,
    windows: &gpui::Entity<teams_ui::call_window::CallWindows>,
    (ringer, server): (&FakeRinger, &FakeServer),
    (shell, cx): (&gpui::Entity<teams_ui::shell::Shell>, &mut VisualTestContext),
) -> &'static mut VisualTestContext {
    assert!(actions(rig).is_empty(), "Accept sends nothing, as Teams' Accept");
    let window = prejoin_window(rig, windows);
    for shown in ["prejoin-title", "prejoin-microphone", "prejoin-cancel", "prejoin-join"] {
        assert!(drawn(window, shown), "{shown}");
    }
    let shown = prejoin(rig).expect("the meeting's pre-join");
    assert_eq!(shown.ring, Some(call(3)));
    assert_eq!(shown.to_join.title(), TITLE, "titled with the meeting's name");
    assert_eq!(shown.to_join.meeting(), meeting_to_join().meeting());
    assert_eq!(ring(rig), Some(3), "the ring rings on while the pre-join shows");
    assert_eq!(ringer.stopped(), 1, "silent while the pre-join shows");
    assert_eq!(server.closed().len(), 1, "its notification goes, as Teams' toast goes");
    assert_eq!(said(shell, cx), None, "the banner gives way to the pre-join");
    window
}

/// The keyboard in the pre-join: the microphone off, then Join now (once, however often Enter is pressed).
fn joined_now(rig: &Rig, window: &mut VisualTestContext) {
    window.simulate_keystrokes("ctrl-shift-m");
    window.run_until_parked();
    assert_eq!(prejoin(rig).map(|prejoin| prejoin.muted), Some(true));
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    assert_eq!(actions(rig), [CallAction::Answer { call: call(3) }]);
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    assert_eq!(actions(rig).len(), 1, "one answer while it goes");
}

/// The answered ring is the meeting's call: its title, the pre-join's microphone, and a Rejoin into the meeting.
fn in_the_meeting(rig: &Rig, windows: &gpui::Entity<teams_ui::call_window::CallWindows>) {
    status(rig, 3, CallStatus::Answering);
    status(rig, 3, CallStatus::Connected);
    rig.cx.run_until_parked();
    assert!(rig.cx.read(|cx| windows.read(cx).prejoin_window().is_none()));
    let active = rig.cx.read(|cx| calls_of(rig).read(cx).call().cloned()).unwrap();
    assert_eq!(
        (active.kind, active.chat.as_deref(), active.title.as_deref()),
        (CallKind::Meeting, Some(MEETING_CHAT), Some(TITLE)),
        "the meeting's call"
    );
    let sent = actions(rig);
    assert!(
        sent.contains(&CallAction::Mute {
            call: call(3),
            muted: true
        }),
        "the pre-join's microphone: {sent:?}"
    );
    // a Rejoin after it is the meeting's own join
    status(rig, 3, CallStatus::Ended(teams_core::End::Remote(RemoteEnd::default())));
    let ended = rig.cx.read(|cx| calls_of(rig).read(cx).ended().cloned()).unwrap();
    let rejoin = rig.cx.read(|cx| calls_of(rig).read(cx).rejoin_of(&ended));
    assert_eq!(rejoin, Some(meeting_to_join_ask()));
}

fn meeting_to_join_ask() -> teams_ui::calls::Ask {
    let to_join = meeting_to_join();
    teams_ui::calls::Ask::JoinMeeting {
        chat: MEETING_CHAT.to_owned(),
        meeting: to_join.meeting().cloned().unwrap(),
        post: None,
    }
}

#[test]
fn cancel_on_a_meeting_rings_pre_join_sends_nothing_and_the_ring_rings_on_until_declined() {
    let rig = calling();
    listed_meeting(&rig);
    let (trace, _) = trace();
    let ringer = Arc::new(FakeRinger::default());
    let server = Arc::new(FakeServer::default());
    let _alert = alert(&rig, &trace, &ringer, &server);
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    rig.send(Event::IncomingCall(meeting_ring(4)));
    rig.cx.run_until_parked();
    click(cx, "call-answer");
    let window = prejoin_window(&rig, &windows);
    // Accept again, from the switcher or the notification: the same pre-join, still nothing sent
    let core: Arc<dyn teams_ui::Core> = rig.core.clone();
    rig.cx
        .update(|cx| teams_ui::calls::ask(&calls_of(&rig), &core, &trace, teams_ui::calls::Ask::Answer, cx));
    rig.cx.run_until_parked();
    assert!(actions(&rig).is_empty());
    assert_eq!(prejoin(&rig).and_then(|prejoin| prejoin.ring), Some(call(4)));
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    rig.cx.run_until_parked();
    assert!(prejoin(&rig).is_none());
    assert!(rig.cx.read(|cx| windows.read(cx).prejoin_window().is_none()));
    assert!(actions(&rig).is_empty(), "Cancel sends nothing");
    assert_eq!(ring(&rig), Some(4), "the ring rings on");
    assert_eq!(ringer.started(), 2, "and aloud again");
    assert_eq!(
        said(&shell, cx),
        Some((format!("{CALLER_NAME} wants you to join {TITLE}"), Some("Accept")))
    );

    // Accept again, then the window closed: the same
    click(cx, "call-answer");
    let _window = prejoin_window(&rig, &windows);
    rig.cx
        .update(|cx| calls_of(&rig).update(cx, teams_ui::Calls::cancel_prejoin));
    rig.cx.run_until_parked();
    assert!(actions(&rig).is_empty());
    click(cx, "call-decline");
    assert_eq!(actions(&rig), [CallAction::Decline { call: call(4) }]);
}

#[test]
fn a_meeting_ring_that_ends_while_its_pre_join_shows_is_not_missed_and_join_now_joins_its_meeting() {
    let rig = calling();
    listed_meeting(&rig);
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    rig.send(Event::IncomingCall(meeting_ring(5)));
    rig.cx.run_until_parked();
    click(cx, "call-answer");
    let window = prejoin_window(&rig, &windows);
    rig.send(Event::RingEnded {
        call: call(5),
        end: RemoteEnd::default(),
    });
    rig.cx.run_until_parked();
    let kept = prejoin(&rig).expect("the pre-join stays");
    assert_eq!((kept.ring, kept.joining), (None, false));
    assert_eq!(
        rig.cx.read(|cx| calls_of(&rig).read(cx).missed(LAB_A)),
        None,
        "an accepted ring is not missed"
    );
    assert!(drawn(window, "prejoin-join"));
    click(window, "prejoin-join");
    assert_eq!(actions(&rig), [join_meeting_action()]);
    status(&rig, 1, CallStatus::Joining);
    let active = rig.cx.read(|cx| calls_of(&rig).read(cx).call().cloned()).unwrap();
    assert_eq!((active.kind, active.title.as_deref()), (CallKind::Meeting, Some(TITLE)));
}

#[test]
fn a_refused_answer_leaves_join_now_to_answer_again_and_a_ring_that_ends_meanwhile_to_join_its_meeting() {
    let rig = calling();
    listed_meeting(&rig);
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    rig.core
        .call_answers
        .lock()
        .unwrap()
        .push_back(Err(EngineError::Call(CallActionError::Busy)));
    rig.send(Event::IncomingCall(meeting_ring(6)));
    rig.cx.run_until_parked();
    click(cx, "call-answer");
    let window = prejoin_window(&rig, &windows);
    click(window, "prejoin-join");
    assert_eq!(actions(&rig), [CallAction::Answer { call: call(6) }]);
    let refused = prejoin(&rig).expect("the pre-join stays after a refused answer");
    assert_eq!((refused.ring, refused.joining), (Some(call(6)), false));

    // answered again; the ring ends while the answer waits
    *rig.core.hold_calls.lock().unwrap() = true;
    click(window, "prejoin-join");
    assert_eq!(actions(&rig).len(), 2, "answered again");
    assert!(prejoin(&rig).is_some_and(|prejoin| prejoin.joining));
    rig.send(Event::RingEnded {
        call: call(6),
        end: RemoteEnd::default(),
    });
    rig.cx.run_until_parked();
    let held = std::mem::take(&mut *rig.core.held_calls.lock().unwrap());
    for answer in held {
        answer.send(Err(EngineError::Call(CallActionError::NoMedia)));
    }
    rig.cx.run_until_parked();
    let after = prejoin(&rig).expect("the pre-join stays");
    assert_eq!((after.ring, after.joining), (None, false), "Join now can go again");
    *rig.core.hold_calls.lock().unwrap() = false;
    click(window, "prejoin-join");
    assert_eq!(actions(&rig).last(), Some(&join_meeting_action()));
}

#[test]
fn a_meeting_ring_that_names_no_meeting_and_a_group_ring_are_answered_at_once() {
    let rig = calling();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    // a group chat's ring, even one that names a meeting, is not a meeting's Request to join
    rig.send(Event::IncomingCall(IncomingCall {
        meeting: meeting_to_join().meeting().cloned(),
        ..invitation(8, Some(CALLER_NAME), Some(TRIO))
    }));
    rig.cx.run_until_parked();
    assert!(!rig.cx.read(|cx| calls_of(&rig).read(cx).ring_opens_prejoin()));
    click(cx, "call-answer");
    assert_eq!(actions(&rig), [CallAction::Answer { call: call(8) }]);
    assert!(rig.cx.read(|cx| windows.read(cx).prejoin_window().is_none()));
    drop((shell, windows));

    let rig = calling();
    let (trace, _) = super::trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    rig.send(Event::IncomingCall(invitation(
        7,
        Some(CALLER_NAME),
        Some(MEETING_CHAT),
    )));
    rig.cx.run_until_parked();
    assert!(!rig.cx.read(|cx| calls_of(&rig).read(cx).ring_opens_prejoin()));
    click(cx, "call-answer");
    assert_eq!(actions(&rig), [CallAction::Answer { call: call(7) }]);
    assert!(rig.cx.read(|cx| windows.read(cx).prejoin_window().is_none()));
}

#[test]
fn join_now_in_the_same_turn_as_accept_titles_the_meetings_call_once_its_name_is_read() {
    let rig = calling();
    listed_meeting(&rig);
    let (trace, _) = trace();
    let (shell, _cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (_windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    rig.send(Event::IncomingCall(meeting_ring(9)));
    let core: Arc<dyn teams_ui::Core> = rig.core.clone();
    // as `ngteamsctl answer` does: Accept, then Join now, and its call told before the meeting's name is read
    rig.cx.update(|cx| {
        teams_ui::calls::accept_meeting_ring(&calls_of(&rig), &core, &trace, cx);
        teams_ui::calls::join_now(&calls_of(&rig), &core, &trace, cx);
        calls_of(&rig).update(cx, |calls, cx| {
            calls.apply(
                &Event::Call {
                    call: call(9),
                    status: CallStatus::Joining,
                },
                cx,
            );
        });
        let told = calls_of(&rig).read(cx).call().cloned().expect("the answered call");
        assert_eq!(
            told.title.as_deref(),
            Some(teams_ui::meeting_alert::NO_TITLE),
            "its name not read yet"
        );
    });
    rig.cx.run_until_parked();
    assert_eq!(actions(&rig), [CallAction::Answer { call: call(9) }]);
    status(&rig, 9, CallStatus::Connected);
    let active = rig.cx.read(|cx| calls_of(&rig).read(cx).call().cloned()).unwrap();
    assert_eq!((active.kind, active.title.as_deref()), (CallKind::Meeting, Some(TITLE)));
    let meeting = rig.cx.read(|cx| calls_of(&rig).read(cx).meeting_to_join().cloned());
    assert_eq!(
        meeting.map(|to_join| to_join.title().to_owned()).as_deref(),
        Some(TITLE)
    );
}

#[test]
fn an_accepted_ring_shows_its_pre_join_over_another_accounts_calendar_pre_join() {
    let rig = calling();
    listed_meeting(&rig);
    let (trace, _) = trace();
    let (shell, _cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    let (other, calls) = second_account(&rig, &shell, &windows, &trace);
    let core: Arc<dyn teams_ui::Core> = other.clone();
    rig.cx.update(|cx| {
        calls.update(cx, |calls, cx| calls.apply(&Event::IncomingCall(meeting_ring(10)), cx));
        teams_ui::calls::ask(&calls, &core, &trace, teams_ui::calls::Ask::Answer, cx);
    });
    rig.cx.run_until_parked();
    assert!(rig.cx.read(|cx| calls.read(cx).ring_in_prejoin()));
    let window = prejoin_window(&rig, &windows);
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    assert_eq!(
        *other.call_actions.lock().unwrap(),
        [CallAction::Answer { call: call(10) }],
        "Join now on the window shown answers the accepted ring"
    );
    assert!(actions(&rig).is_empty(), "the other account's meeting is not joined");
}

/// Account a's calendar pre-join open and a second account `LAB_B` followed by the same windows: its core and calls.
fn second_account(
    rig: &Rig,
    shell: &gpui::Entity<teams_ui::shell::Shell>,
    windows: &gpui::Entity<teams_ui::call_window::CallWindows>,
    trace: &Arc<teams_ui::ViewTrace>,
) -> (Arc<super::common::FakeCore>, gpui::Entity<teams_ui::calls::Calls>) {
    rig.cx
        .update(|cx| calls_of(rig).update(cx, |calls, cx| calls.open_prejoin(meeting_to_join(), cx)));
    let _first = prejoin_window(rig, windows);
    let other = Arc::new(super::common::FakeCore {
        me: LAB_B.to_owned().into(),
        ..super::common::FakeCore::default()
    });
    let core: Arc<dyn teams_ui::Core> = other.clone();
    let models = rig.cx.update(|cx| teams_ui::Models::with_calls(None, cx));
    let parts = teams_ui::shell::ShellParts {
        core,
        models: models.clone(),
        settings: rig.cx.read(|cx| shell.read(cx).settings().clone()),
        trace: Arc::clone(trace),
    };
    rig.cx
        .update(|cx| windows.update(cx, |windows, cx| windows.add(parts, cx)));
    (other, models.calls)
}

#[test]
fn an_accepted_rings_pre_join_stays_over_another_accounts_calendar_pre_join_after_its_ring_ends() {
    let rig = calling();
    listed_meeting(&rig);
    let (trace, _) = trace();
    let (shell, _cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    let (other, calls) = second_account(&rig, &shell, &windows, &trace);
    let core: Arc<dyn teams_ui::Core> = other.clone();
    rig.cx.update(|cx| {
        calls.update(cx, |calls, cx| calls.apply(&Event::IncomingCall(meeting_ring(10)), cx));
        teams_ui::calls::ask(&calls, &core, &trace, teams_ui::calls::Ask::Answer, cx);
    });
    rig.cx.run_until_parked();
    rig.cx.update(|cx| {
        calls.update(cx, |calls, cx| {
            calls.apply(
                &Event::RingEnded {
                    call: call(10),
                    end: RemoteEnd::default(),
                },
                cx,
            );
        });
    });
    rig.cx.run_until_parked();
    assert!(rig.cx.read(|cx| !calls.read(cx).ring_in_prejoin()));
    let window = prejoin_window(&rig, &windows);
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    assert_eq!(
        *other.call_actions.lock().unwrap(),
        [join_meeting_action()],
        "Join now on the window shown joins the ended ring's meeting"
    );
    assert!(actions(&rig).is_empty(), "the other account's meeting is not joined");
}

#[test]
fn a_calendar_pre_join_opened_last_shows_over_another_accounts_open_one() {
    let rig = calling();
    let (trace, _) = trace();
    let (shell, _cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    let (other, calls) = second_account(&rig, &shell, &windows, &trace);
    rig.cx.update(|cx| {
        assert!(teams_ui::calls::show_prejoin(
            &calls,
            &trace,
            meeting_to_join(),
            false,
            cx
        ));
    });
    let window = prejoin_window(&rig, &windows);
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    assert_eq!(
        *other.call_actions.lock().unwrap(),
        [join_meeting_action()],
        "Join now on the window shown joins the pre-join opened last"
    );
    assert!(
        actions(&rig).is_empty(),
        "the account whose pre-join opened first joins nothing"
    );
}

#[test]
fn a_pre_join_that_replaces_its_accounts_own_shows_over_another_accounts_opened_before_it() {
    let rig = calling();
    let (trace, _) = trace();
    let (shell, _cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    let (other, calls) = second_account(&rig, &shell, &windows, &trace);
    rig.cx.update(|cx| {
        assert!(teams_ui::calls::show_prejoin(
            &calls,
            &trace,
            meeting_to_join(),
            false,
            cx
        ));
    });
    rig.cx.run_until_parked();
    rig.cx.update(|cx| {
        assert!(teams_ui::calls::show_prejoin(
            &calls_of(&rig),
            &trace,
            meeting_to_join(),
            false,
            cx
        ));
    });
    let window = prejoin_window(&rig, &windows);
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    assert_eq!(
        actions(&rig),
        [join_meeting_action()],
        "Join now on the window shown joins the pre-join opened last"
    );
    assert!(
        other.call_actions.lock().unwrap().is_empty(),
        "the account whose pre-join opened before joins nothing"
    );
}

#[test]
fn an_accepted_rings_pre_join_stays_over_a_calendar_pre_join_another_account_opens_after_it() {
    let rig = calling();
    listed_meeting(&rig);
    let (trace, _) = trace();
    let (shell, _cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    let (other, calls) = second_account(&rig, &shell, &windows, &trace);
    let core: Arc<dyn teams_ui::Core> = other.clone();
    rig.cx.update(|cx| {
        calls.update(cx, |calls, cx| calls.apply(&Event::IncomingCall(meeting_ring(10)), cx));
        teams_ui::calls::ask(&calls, &core, &trace, teams_ui::calls::Ask::Answer, cx);
    });
    rig.cx.run_until_parked();
    rig.cx.update(|cx| {
        assert!(teams_ui::calls::show_prejoin(
            &calls_of(&rig),
            &trace,
            meeting_to_join(),
            false,
            cx
        ));
    });
    rig.cx.run_until_parked();
    assert!(rig.cx.read(|cx| calls.read(cx).ring_in_prejoin()));
    let window = prejoin_window(&rig, &windows);
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    assert_eq!(
        *other.call_actions.lock().unwrap(),
        [CallAction::Answer { call: call(10) }],
        "Join now on the window shown answers the accepted ring, silent while its pre-join shows"
    );
    assert!(
        actions(&rig).is_empty(),
        "the calendar pre-join opened after it joins nothing"
    );
}

#[test]
fn a_ring_that_ends_before_its_meetings_name_is_read_leaves_its_pre_join_titled_with_the_name() {
    let rig = calling();
    listed_meeting(&rig);
    let (trace, _) = trace();
    let (shell, _cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (_windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    rig.send(Event::IncomingCall(meeting_ring(6)));
    let core: Arc<dyn teams_ui::Core> = rig.core.clone();
    rig.cx.update(|cx| {
        teams_ui::calls::accept_meeting_ring(&calls_of(&rig), &core, &trace, cx);
        calls_of(&rig).update(cx, |calls, cx| {
            calls.apply(
                &Event::RingEnded {
                    call: call(6),
                    end: RemoteEnd::default(),
                },
                cx,
            );
        });
    });
    rig.cx.run_until_parked();
    let kept = prejoin(&rig).expect("the pre-join stays");
    assert_eq!((kept.ring, kept.to_join.title()), (None, TITLE));
}

#[test]
fn the_pre_joins_window_takes_the_meetings_name_as_it_is_read_with_no_draw_between() {
    let mut rig = calling();
    listed_meeting(&rig);
    let (trace, _) = trace();
    let (shell, _cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    rig.send(Event::IncomingCall(meeting_ring(8)));
    rig.cx.run_until_parked();
    // a title that only a draw sets stays unset
    rig.cx.skip_drawing();
    let core: Arc<dyn teams_ui::Core> = rig.core.clone();
    rig.cx
        .update(|cx| teams_ui::calls::accept_meeting_ring(&calls_of(&rig), &core, &trace, cx));
    rig.cx.run_until_parked();
    let window = rig
        .cx
        .read(|cx| windows.read(cx).prejoin_window())
        .expect("the pre-join's window is open");
    let title = rig
        .cx
        .update(|cx| cx.update_window(window.into(), |_, window, _| window.window_title()))
        .unwrap();
    assert_eq!(title, TITLE, "titled as the name is read, not at the next draw");
}

#[test]
fn a_service_refused_answer_keeps_the_pre_join_with_why_and_its_join_now_joins_the_meeting() {
    let rig = calling();
    listed_meeting(&rig);
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    rig.send(Event::IncomingCall(meeting_ring(9)));
    rig.cx.run_until_parked();
    click(cx, "call-answer");
    let window = prejoin_window(&rig, &windows);
    click(window, "prejoin-join");
    assert_eq!(actions(&rig), [CallAction::Answer { call: call(9) }]);
    status(&rig, 9, CallStatus::Answering);
    rig.cx.run_until_parked();
    let shown = |rig: &Rig| {
        rig.cx.read(|cx| {
            let windows = windows.read(cx);
            (windows.prejoin_window().is_some(), windows.window().is_some())
        })
    };
    assert_eq!(shown(&rig), (true, false), "the pre-join shows while its answer goes");
    assert!(prejoin(&rig).is_some_and(|prejoin| prejoin.joining));

    // the service refuses the `accept`: the engine leaves the call, and it ends failed
    status(&rig, 9, CallStatus::Leaving);
    status(&rig, 9, CallStatus::Ended(teams_core::End::Failed("accept")));
    rig.cx.run_until_parked();
    assert_eq!(shown(&rig), (true, false), "the pre-join stays, no call window");
    let kept = prejoin(&rig).expect("the pre-join stays");
    assert_eq!((kept.ring, kept.joining), (None, false), "Join now can go again");
    let calls = rig.cx.read(|cx| {
        let calls = calls_of(&rig).read(cx);
        (
            calls.call().is_none(),
            calls.ended().is_none(),
            calls.refused().map(|refused| refused.text.clone()),
        )
    });
    assert_eq!(
        calls,
        (true, true, Some("the meeting couldn't be joined: try again".to_owned()))
    );
    assert!(drawn(window, "prejoin-refused"));
    assert_eq!(actions(&rig).len(), 1, "nothing goes again by itself");

    // only the user's new Join now goes: the ring is over, so it joins the meeting
    click(window, "prejoin-join");
    assert_eq!(actions(&rig).last(), Some(&join_meeting_action()));
    assert_eq!(actions(&rig).len(), 2);
}

#[test]
fn an_answer_from_the_pre_join_that_is_taken_gives_way_to_the_call_window() {
    let rig = calling();
    listed_meeting(&rig);
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    rig.send(Event::IncomingCall(meeting_ring(10)));
    rig.cx.run_until_parked();
    click(cx, "call-answer");
    let window = prejoin_window(&rig, &windows);
    click(window, "prejoin-join");
    status(&rig, 10, CallStatus::Answering);
    rig.send(Event::RingEnded {
        call: call(10),
        end: RemoteEnd::default(),
    });
    rig.cx.run_until_parked();
    assert_eq!(
        prejoin(&rig).and_then(|prejoin| prejoin.ring),
        Some(call(10)),
        "the answered ring's end leaves the answer to its call"
    );
    status(&rig, 10, CallStatus::Connecting);
    rig.cx.run_until_parked();
    assert!(prejoin(&rig).is_none());
    let (prejoin_open, call_open) = rig.cx.read(|cx| {
        let windows = windows.read(cx);
        (windows.prejoin_window().is_some(), windows.window().is_some())
    });
    assert_eq!((prejoin_open, call_open), (false, true));
}

#[test]
fn the_pre_joins_switches_are_locked_while_its_answer_goes_and_the_call_takes_what_join_now_saw() {
    let rig = calling();
    listed_meeting(&rig);
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    rig.send(Event::IncomingCall(meeting_ring(11)));
    rig.cx.run_until_parked();
    click(cx, "call-answer");
    let window = prejoin_window(&rig, &windows);
    let seen = |rig: &Rig| prejoin(rig).map(|prejoin| (prejoin.camera, prejoin.muted, prejoin.audio));
    let at_join = seen(&rig);
    assert_eq!(at_join, Some((false, false, teams_ui::calls::PrejoinAudio::Computer)));
    click(window, "prejoin-join");
    status(&rig, 11, CallStatus::Answering);
    rig.cx.run_until_parked();

    click(window, "prejoin-microphone");
    rig.cx.update(|cx| {
        calls_of(&rig).update(cx, |calls, cx| {
            calls.set_prejoin_camera(true, cx);
            calls.set_prejoin_muted(true, cx);
            calls.set_prejoin_no_audio(true, cx);
        });
    });
    assert_eq!(seen(&rig), at_join, "the switches hold what Join now saw");

    status(&rig, 11, CallStatus::Connecting);
    status(&rig, 11, CallStatus::Connected);
    rig.cx.run_until_parked();
    assert_eq!(
        actions(&rig),
        [CallAction::Answer { call: call(11) }],
        "no camera or mute goes"
    );
}

/// A camera that is never there: the pre-join's preview starts and shows its failure, enough to see it run.
struct NoCamera;

impl teams_media::devices::Devices for NoCamera {
    fn microphone(&self) -> Result<Box<dyn teams_media::devices::AudioSource>, teams_media::devices::DeviceError> {
        Err(teams_media::devices::DeviceError::Closed)
    }
    fn speaker(&self) -> Result<Box<dyn teams_media::devices::AudioSink>, teams_media::devices::DeviceError> {
        Err(teams_media::devices::DeviceError::Closed)
    }
    fn camera(&self) -> Result<Box<dyn teams_media::devices::VideoSource>, teams_media::devices::DeviceError> {
        Err(teams_media::devices::DeviceError::Closed)
    }
}

#[test]
fn while_its_answer_goes_the_pre_join_takes_no_choice_and_its_preview_comes_back_if_the_answer_fails() {
    let rig = calling();
    listed_meeting(&rig);
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    cx.update(|_, cx| {
        let settings = shell.read(cx).settings().clone();
        let preview = teams_ui::effects::CameraPreview::new(Arc::new(NoCamera));
        teams_ui::settings::attach_camera_preview(&settings, preview, cx);
    });
    registered(&rig);
    rig.send(Event::IncomingCall(meeting_ring(12)));
    rig.cx.run_until_parked();
    click(cx, "call-answer");
    let window = prejoin_window(&rig, &windows);
    let root = rig.cx.read(|cx| windows.read(cx).prejoin_window()).unwrap();
    let view = rig.cx.update(|cx| {
        root.read(cx)
            .unwrap()
            .view()
            .clone()
            .downcast::<teams_ui::prejoin::PrejoinView>()
            .unwrap()
    });
    let previewing = |rig: &Rig| rig.cx.read(|cx| view.read(cx).previewing());
    click(window, "prejoin-camera");
    assert!(previewing(&rig));
    let seen = |rig: &Rig| prejoin(rig).map(|prejoin| (prejoin.camera, prejoin.muted, prejoin.audio));
    let at_join = seen(&rig);
    assert_eq!(at_join, Some((true, false, teams_ui::calls::PrejoinAudio::Computer)));
    click(window, "prejoin-join");
    status(&rig, 12, CallStatus::Answering);
    rig.cx.run_until_parked();
    assert!(!previewing(&rig), "the call takes the camera");

    window.simulate_keystrokes("ctrl-shift-o ctrl-shift-m");
    click(window, "prejoin-audio-none");
    click(window, "prejoin-effects");
    assert_eq!(seen(&rig), at_join, "the choices hold what Join now saw");
    assert!(!previewing(&rig), "no preview starts while the call takes the camera");
    assert!(
        !drawn(window, "prejoin-effects-pane"),
        "the filters' pane opens no preview of its own"
    );

    status(&rig, 12, CallStatus::Leaving);
    status(&rig, 12, CallStatus::Ended(teams_core::End::Failed("accept")));
    rig.cx.run_until_parked();
    assert!(prejoin(&rig).is_some_and(|prejoin| !prejoin.joining && prejoin.camera));
    assert!(
        previewing(&rig),
        "the camera's switch is still on, so its preview comes back"
    );
}
