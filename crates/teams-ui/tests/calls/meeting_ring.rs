//! A meeting's Request to join on the invitee's side: Accept opens the meeting's pre-join and sends nothing, Join now
//! answers the ring into the meeting, Cancel lets the ring ring on, and a ring that ends while the pre-join shows
//! leaves Join now to join the meeting by its link. Every name and id is made up.

use std::sync::Arc;

use gpui::VisualTestContext;
use teams_core::{CallAction, CallActionError, CallStatus, EngineError, Event, IncomingCall, RemoteEnd};
use teams_store::ConversationKind;

use super::{
    AB, CALLER_NAME, CallKind, FakeRinger, FakeServer, MEETING_CHAT, Rig, actions, alert, call, call_windows, calling,
    calls_of, chat, click, drawn, invitation, join_meeting_action, lines, meeting_to_join, no_name_in, prejoin_window,
    registered, shell_window, status, trace,
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
fn a_meeting_ring_that_ends_while_its_pre_join_shows_leaves_join_now_to_join_the_meeting_by_its_link() {
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
    assert!(drawn(window, "prejoin-join"));
    click(window, "prejoin-join");
    assert_eq!(actions(&rig), [join_meeting_action()]);
    status(&rig, 1, CallStatus::Joining);
    let active = rig.cx.read(|cx| calls_of(&rig).read(cx).call().cloned()).unwrap();
    assert_eq!((active.kind, active.title.as_deref()), (CallKind::Meeting, Some(TITLE)));
}

#[test]
fn a_refused_answer_leaves_join_now_to_answer_again_and_a_ring_that_ends_meanwhile_to_join_by_the_link() {
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
