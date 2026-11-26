//! QA of task 831 (round 1): Accept on a meeting's Request to join opens the meeting's pre-join, as a user meets it.
//! b, whom the mock plays, asks a into the made-up meeting (a ring with `meetingInfo`).
//!
//! - **The ring after the call** (`ring_gone_after_leave`): after `invitee_ring` answers the ring and leaves the
//!   meeting, the ring does not ring again.
//! - **Keyboard** (`ring_keyboard`): Ctrl+Shift+S opens the pre-join with the meeting's title; nothing goes out, the
//!   ringtone and the notification stop, and the banner hides. Ctrl+Shift+S again only raises the pre-join. The
//!   microphone off (Ctrl+Shift+M) applies in the call that Join now (Enter) answers (the camera's switch acts only
//!   in a build with video, which this one is not).
//!   When the service ends the call, Rejoin (Ctrl+Shift+J) goes into the meeting again by its join, not the ring's
//!   accept (a Leave by the user closes the window, with no Rejoin, by design).
//! - **Close and Decline** (`ring_close_decline`): a close of the pre-join's window sends nothing, and the ring rings
//!   on with its banner; Decline (Ctrl+Shift+D) declines the ring.
//! - **The ring ends** (`ring_ends_in_prejoin`): the ring times out, or b gives up, while the pre-join shows; the
//!   pre-join stays, and Join now joins the meeting.
//! - **A refused answer** (`ring_refused_answer`): the service refuses Join now's `accept`; the pre-join stays, and
//!   Join now goes again.
//! - **No regression** (`ring_answers_at_once`): a 1:1 ring, a group ring, and a ring with `meetingInfo` in a group
//!   chat that is no meeting's are answered at once by Accept, with no pre-join.
use super::{
    ABC_CHAT, Duration, Instant, LAB_A, LAB_B, MEETING, POLL, SETTLE, SUBJECT, SandboxResult, TENANT, TIMED_OUT, User,
    Value, WAIT, ensure, json, request, say, thread, wait_until,
};

const CANCELLED: &str = "MockCallerCancelled";

/// b asks a into the meeting: the ring's id on the mock, once a's window rings.
fn meeting_ring(user: &mut User<'_>) -> SandboxResult<String> {
    user.hang_up()?;
    user.meeting = None;
    user.focus_main()?;
    let info = json!({"organizerId": request::organizer(), "tenantId": TENANT});
    let id = user.mock.ring_meeting(LAB_B, MEETING, &[LAB_A], info);
    user.until("the meeting's ring", |state| {
        !state["ringing"].is_null() && state["alerting"] == true
    })?;
    thread::sleep(SETTLE);
    Ok(id)
}

/// The mock ends the ring `ring` and its call, an open pre-join is cancelled and a's call ends: the check's `outcome`
/// stays, or the first error of the cleanup when the check passed.
fn finish(user: &mut User<'_>, ring: &str, outcome: SandboxResult) -> SandboxResult {
    if let Err(error) = &outcome {
        say(&format!("  the check failed: {error}"));
    }
    let _ = user.mock.end_ring(ring, 0, TIMED_OUT);
    let _ = user.mock.hang_up_call(ring);
    let cleaned = (|| -> SandboxResult {
        let state = user.until("the ring ended", |state| state["ringing"].is_null())?;
        if !state["prejoin"].is_null() {
            user.focus_other()?;
            user.press("escape")?;
            user.until("the pre-join cancelled", |state| state["prejoin"].is_null())?;
        }
        if user.calls()?.iter().any(|row| row.state == "connected")
            && let Err(error) = user.hang_up()
        {
            // the mock's end of the call can come first: then there is nothing left to hang up
            wait_until(WAIT, || {
                user.calls()
                    .ok()
                    .filter(|calls| calls.iter().all(|row| row.state != "connected"))
                    .map(drop)
            })
            .ok_or(error)?;
        }
        Ok(())
    })();
    outcome.and(cleaned)
}

/// The operations a sent on the mock's calls, each with its call's group chat, oldest first.
fn sent_by_a(user: &User<'_>) -> Vec<(String, Option<String>, String)> {
    user.mock
        .calls()
        .into_iter()
        .flat_map(|call| {
            let chat = call.group_chat.clone();
            let id = call.id.clone();
            call.requests
                .into_iter()
                .filter(|(_, by)| by == LAB_A)
                .map(move |(op, _)| (op, chat.clone(), id.clone()))
        })
        .collect()
}

/// a sent nothing since `before`.
fn nothing_sent(user: &User<'_>, before: usize, after: &str) -> SandboxResult {
    let sent = sent_by_a(user);
    ensure(sent.len() == before, || {
        format!("{after} sent {:?}", &sent[before.min(sent.len())..])
    })
}

/// The trace lines of `view` from the `seen`th on.
fn traced(user: &mut User<'_>, seen: usize, view: &str) -> Vec<String> {
    user.app
        .trace
        .catch_up()
        .iter()
        .skip(seen)
        .filter(|line| line["view"] == view)
        .map(|line| line["change"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// The ring's pre-join shows with the meeting's title, the ring rings on but is silent, and no call is up.
fn ring_prejoin_shows(user: &mut User<'_>, what: &str) -> SandboxResult<Value> {
    let state = user.until(what, |state| {
        state["prejoin"]["ring"] == true && state["prejoin"]["title"] == SUBJECT
    })?;
    say(&format!("  {what}: {state}"));
    window_titled(user);
    ensure(
        state["prejoin"]["chat"] == MEETING
            && !state["ringing"].is_null()
            && state["alerting"] == false
            && state["call"].is_null(),
        || format!("{what}: not the ring's pre-join alone: {state}"),
    )?;
    Ok(state)
}

/// How long the pre-join's window takes to show the meeting's title, as the compositor lists it (logged, not checked:
/// `invitee_ring` checks the title).
fn window_titled(user: &mut User<'_>) {
    let start = Instant::now();
    let mut seen: Vec<String> = Vec::new();
    while start.elapsed() < Duration::from_secs(10) {
        let titles: Vec<String> = user
            .windows()
            .unwrap_or_default()
            .iter()
            .filter(|window| window["id"].as_u64() != user.main_window)
            .map(|window| window["title"].as_str().unwrap_or_default().to_owned())
            .collect();
        let line = titles.join(" | ");
        if seen.last() != Some(&line) {
            seen.push(line);
        }
        if titles.iter().any(|title| title == SUBJECT) {
            break;
        }
        thread::sleep(POLL);
    }
    say(&format!(
        "  the pre-join's window titles: {seen:?}, after {:?}",
        start.elapsed()
    ));
}

/// The main window shows no Accept: the ring's banner hides.
fn no_banner(user: &mut User<'_>) -> SandboxResult {
    user.focus_main()?;
    let screen = user.screen()?;
    let answer = request::answer_button(&screen);
    ensure(answer.is_none(), || {
        format!("the main window shows an Accept at {answer:?}")
    })
}

/// The ring's banner shows its Accept again, and the ring alerts.
fn banner_back(user: &mut User<'_>, what: &str) -> SandboxResult {
    let state = user.until(what, |state| {
        state["prejoin"].is_null() && !state["ringing"].is_null() && state["alerting"] == true
    })?;
    ensure(state["call"].is_null(), || format!("{what}: a call is up: {state}"))?;
    user.focus_main()?;
    let screen = user.screen()?;
    ensure(request::answer_button(&screen).is_some(), || {
        format!("{what}: the main window shows no Accept")
    })
}

/// The window besides the main window is focused (the pre-join's): its title.
fn other_focused(user: &mut User<'_>) -> SandboxResult<String> {
    let main = user.main_window;
    let focused = user
        .windows()?
        .into_iter()
        .find(|window| window["is_focused"] == true)
        .ok_or("no window is focused")?;
    ensure(focused["id"].as_u64() != main, || {
        "the main window keeps the focus".to_owned()
    })?;
    Ok(focused["title"].as_str().unwrap_or_default().to_owned())
}

/// Accept by the keyboard opens the pre-join, sends nothing and silences the ring; a second Accept only raises it.
fn keyboard_accept(user: &mut User<'_>, before: usize) -> SandboxResult {
    let notices = user.sandbox.bus().notifications().len();
    let seen = user.app.trace.catch_up().len();
    user.press("ctrl+shift+s")?;
    let state = ring_prejoin_shows(user, "the pre-join after Ctrl+Shift+S")?;
    let opened = state["prejoin"]["opened"].clone();
    thread::sleep(SETTLE);
    let ringtone = traced(user, seen, "ringtone");
    let notification = traced(user, seen, "call-notification");
    say(&format!(
        "  after Accept: ringtone {ringtone:?}, notification {notification:?}"
    ));
    ensure(ringtone.iter().all(|change| change != "started"), || {
        format!("the ringtone started again: {ringtone:?}")
    })?;
    ensure(user.sandbox.bus().notifications().len() == notices, || {
        "Accept brought a new notification".to_owned()
    })?;
    nothing_sent(user, before, "Accept")?;
    no_banner(user)?;
    user.shot("q831-ring-accepted-main")?;
    // the second Accept, from the main window, only raises the pre-join
    user.focus_main()?;
    user.press("ctrl+shift+s")?;
    thread::sleep(SETTLE);
    let raised = other_focused(user)
        .and_then(|title| ensure(title == SUBJECT, || format!("the second Accept raised {title:?}")));
    if let Err(error) = &raised {
        user.shot("q831-second-accept")?;
        say(&format!("  the second Accept: {error}"));
    }
    let again = ring_prejoin_shows(user, "the pre-join after a second Accept")?;
    ensure(again["prejoin"]["opened"] == opened, || {
        format!(
            "the second Accept opened the pre-join anew: {opened} then {}",
            again["prejoin"]["opened"]
        )
    })?;
    nothing_sent(user, before, "the second Accept")?;
    raised
}

/// The microphone off in the pre-join, then Join now (Enter): the meeting's call, muted. The camera's switch is not
/// checked: it acts only in a build with video, and this one has none.
fn join_now_with_choices(user: &mut User<'_>, before: usize) -> SandboxResult {
    user.focus_other()?;
    user.press("ctrl+shift+m")?;
    let chosen = user.until("the pre-join's choices", |state| state["prejoin"]["muted"] == true)?;
    say(&format!("  the pre-join's choices: {}", chosen["prejoin"]));
    nothing_sent(user, before, "the pre-join's switches")?;
    user.park()?;
    thread::sleep(SETTLE);
    user.shot("q831-prejoin-choices")?;
    user.press("enter")?;
    user.wait_call("a in the meeting after Join now", |row| row.state == "connected")?;
    let state = user.until("the call takes the pre-join's choices", |state| {
        state["call"]["muted"] == true
    })?;
    say(&format!("  in the call: {}", state["call"]));
    ensure(
        state["call"]["kind"] == "Meeting" && state["call"]["chat"] == MEETING && state["call"]["title"] == SUBJECT,
        || format!("Join now's call is not the meeting's: {}", state["call"]),
    )?;
    let sent = sent_by_a(user);
    say(&format!("  a sent: {:?}", &sent[before..]));
    ensure(sent[before..].iter().any(|(op, _, _)| op == "accept"), || {
        "Join now sent no accept of the ring".to_owned()
    })?;
    thread::sleep(SETTLE);
    user.park()?;
    user.shot("q831-joined-choices").map(drop)
}

/// The call ends from the service, then Rejoin: the meeting's join, not the ring's accept.
fn rejoin(user: &mut User<'_>, ring: &str) -> SandboxResult {
    // Teams offers Rejoin when the user did not end the call: here the service ends it
    ensure(user.mock.hang_up_call(ring), || "no meeting call to end".to_owned())?;
    wait_until(WAIT, || {
        user.calls()
            .ok()
            .filter(|calls| calls.iter().all(|row| row.state != "connected"))
            .map(drop)
    })
    .ok_or("the call did not end")?;
    thread::sleep(SETTLE);
    user.focus_other()?;
    user.park()?;
    user.shot("q831-left-rejoin")?;
    let before = sent_by_a(user).len();
    user.press("ctrl+shift+j")?;
    user.wait_call("a in the meeting after Rejoin", |row| row.state == "connected")?;
    let state = user.state()?;
    say(&format!("  after Rejoin: {}", state["call"]));
    let sent = sent_by_a(user);
    say(&format!("  Rejoin sent: {:?}", &sent[before..]));
    ensure(
        state["call"]["kind"] == "Meeting" && state["call"]["chat"] == MEETING,
        || format!("Rejoin went into {}", state["call"]),
    )?;
    ensure(
        sent[before..].iter().all(|(op, _, id)| op != "accept" || id != ring),
        || "Rejoin accepted the ring again".to_owned(),
    )
}

pub(super) fn ring_keyboard(user: &mut User<'_>) -> SandboxResult {
    let ring = meeting_ring(user)?;
    let before = sent_by_a(user).len();
    // the rest of the flow runs when only the raise fails, so its outcome comes last
    let accepted = keyboard_accept(user, before);
    let outcome = match accepted {
        Err(error) if !error.to_string().contains("raised") && !error.to_string().contains("keeps the focus") => {
            Err(error)
        }
        accepted => join_now_with_choices(user, before)
            .and_then(|()| rejoin(user, &ring))
            .and(accepted),
    };
    finish(user, &ring, outcome)
}

/// The pre-join's window closes as the compositor closes it.
fn close_prejoin(user: &mut User<'_>) -> SandboxResult {
    let prejoin_window = wait_until(WAIT, || {
        user.windows().ok()?.iter().find_map(|window| {
            (window["title"] == SUBJECT && window["id"].as_u64() != user.main_window).then(|| window["id"].as_u64())?
        })
    });
    let titles: Vec<Value> = user.windows()?.iter().map(|window| window["title"].clone()).collect();
    say(&format!("  the windows: {titles:?}"));
    let id = prejoin_window.ok_or_else(|| format!("no window is the pre-join's: {titles:?}"))?;
    user.sandbox
        .niri_msg(&["action", "focus-window", "--id", &id.to_string()])?;
    thread::sleep(SETTLE);
    user.sandbox.niri_msg(&["action", "close-window"])?;
    thread::sleep(SETTLE);
    Ok(())
}

pub(super) fn ring_close_decline(user: &mut User<'_>) -> SandboxResult {
    let ring = meeting_ring(user)?;
    let before = sent_by_a(user).len();
    let outcome = (|| {
        request::accept(user)?;
        ring_prejoin_shows(user, "the pre-join after Accept")?;
        close_prejoin(user)?;
        banner_back(user, "the ring after a close")?;
        nothing_sent(user, before, "a close")?;
        user.shot("q831-ring-after-close")?;
        user.focus_main()?;
        user.press("ctrl+shift+d")?;
        user.until("the ring declined", |state| {
            state["ringing"].is_null() && state["prejoin"].is_null()
        })?;
        let sent = sent_by_a(user);
        say(&format!("  Decline sent: {:?}", &sent[before..]));
        ensure(
            sent[before..].iter().any(|(op, _, id)| op == "reject" && *id == ring),
            || format!("Decline sent no reject of the ring: {:?}", &sent[before..]),
        )?;
        ensure(user.calls()?.iter().all(|row| row.state != "connected"), || {
            "Decline joined a call".to_owned()
        })
    })();
    finish(user, &ring, outcome)
}

/// Join now after the ring ended: a joins the meeting's call, with no accept of the ended ring.
fn join_after_the_end(user: &mut User<'_>, ring: &str, shot: &str) -> SandboxResult {
    let state = user.until("the pre-join after the ring ended", |state| {
        state["ringing"].is_null() && !state["prejoin"].is_null()
    })?;
    say(&format!("  after the ring ended: {state}"));
    ensure(state["prejoin"]["title"] == SUBJECT && state["call"].is_null(), || {
        format!("the pre-join after the ring ended: {state}")
    })?;
    user.focus_other()?;
    user.park()?;
    thread::sleep(SETTLE);
    user.shot(shot)?;
    let before = sent_by_a(user).len();
    user.press("enter")?;
    user.wait_call("a in the meeting after the ring ended", |row| row.state == "connected")?;
    let call = user.state()?["call"].clone();
    let sent = sent_by_a(user);
    say(&format!("  Join now sent: {:?}; the call {call}", &sent[before..]));
    ensure(call["kind"] == "Meeting" && call["chat"] == MEETING, || {
        format!("Join now after the ring went into {call}")
    })?;
    ensure(
        sent[before..]
            .iter()
            .all(|(op, chat, id)| chat.as_deref() == Some(MEETING) && (op != "accept" || id != ring)),
        || format!("Join now after the ring sent {:?}", &sent[before..]),
    )
}

pub(super) fn ring_ends_in_prejoin(user: &mut User<'_>) -> SandboxResult {
    for (code, phrase, shot) in [
        (0, TIMED_OUT, "q831-prejoin-ring-timed-out"),
        (487, CANCELLED, "q831-prejoin-ring-cancelled"),
    ] {
        let ring = meeting_ring(user)?;
        let outcome = request::accept(user)
            .and_then(|()| ring_prejoin_shows(user, "the pre-join after Accept").map(drop))
            .and_then(|()| ensure(user.mock.end_ring(&ring, code, phrase), || "no ring to end".to_owned()))
            .and_then(|()| join_after_the_end(user, &ring, shot));
        finish(user, &ring, outcome).map_err(|error| format!("{phrase}: {error}"))?;
    }
    Ok(())
}

/// What the app and the mock show after a refused answer: its trace lines from `seen` on, and a's calls on the mock.
fn after_refusal(user: &mut User<'_>, seen: usize) {
    let lines: Vec<String> = user
        .app
        .trace
        .catch_up()
        .iter()
        .skip(seen)
        .filter(|line| {
            line["view"]
                .as_str()
                .is_some_and(|view| view.contains("call") || view.contains("prejoin"))
        })
        .map(Value::to_string)
        .collect();
    say(&format!("  the trace after Join now: {lines:?}"));
    let mock: Vec<String> = user
        .mock
        .calls()
        .iter()
        .rev()
        .take(2)
        .map(|call| format!("{} {:?} {:?} {:?}", call.id, call.group_chat, call.phase, call.requests))
        .collect();
    say(&format!("  the mock's last calls: {mock:?}"));
}

/// The service answers Join now's `accept` `code`: the pre-join stays, and Join now goes again.
fn refused_with(user: &mut User<'_>, code: u16) -> SandboxResult {
    let ring = meeting_ring(user)?;
    let outcome = (|| {
        request::accept(user)?;
        ring_prejoin_shows(user, "the pre-join after Accept")?;
        user.mock.refuse_next_call_op("accept", code);
        user.focus_other()?;
        let seen = user.app.trace.catch_up().len();
        user.press("enter")?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !sent_by_a(user).iter().any(|(op, _, id)| op == "accept" && *id == ring) {
            thread::sleep(POLL);
        }
        thread::sleep(SETTLE * 3);
        after_refusal(user, seen);
        let state = user.until("the pre-join or the call's end after the refused answer", |state| {
            // the refused `accept` ended the ring's call: the pre-join stays, its Join now joins the meeting
            (!state["prejoin"].is_null() && state["prejoin"]["joining"] == false)
                || (state["prejoin"].is_null() && state["call"].is_null())
        })?;
        say(&format!("  after the refusal: {state}"));
        if state["prejoin"].is_null() {
            return rejoin_after_refusal(user, code);
        }
        ensure(user.calls()?.iter().all(|row| row.state != "connected"), || {
            "the refused answer joined".to_owned()
        })?;
        user.focus_other()?;
        user.park()?;
        thread::sleep(SETTLE);
        user.shot(&format!("q831-prejoin-after-refusal-{code}"))?;
        user.press("enter")?;
        user.wait_call("a in the meeting after the second Join now", |row| {
            row.state == "connected"
        })?;
        let call = user.state()?["call"].clone();
        ensure(call["kind"] == "Meeting" && call["chat"] == MEETING, || {
            format!("the second Join now went into {call}")
        })
    })();
    finish(user, &ring, outcome).map_err(|error| format!("{code}: {error}").into())
}

/// The refused answer closed the pre-join and ended the call: the call window's end offers Rejoin, which goes into
/// the meeting. The check fails all the same, as the pre-join did not stay.
fn rejoin_after_refusal(user: &mut User<'_>, code: u16) -> SandboxResult {
    let title = user.focus_other()?;
    user.park()?;
    thread::sleep(SETTLE);
    user.shot(&format!("q831-refused-{code}-end"))?;
    user.press("ctrl+shift+j")?;
    let rejoined = user
        .wait_call("a in the meeting after Rejoin", |row| row.state == "connected")
        .and_then(|_| user.state())
        .map(|state| state["call"].clone());
    say(&format!(
        "  the window after the refusal: {title:?}; Rejoin: {rejoined:?}"
    ));
    if rejoined.is_ok() {
        thread::sleep(SETTLE);
        user.park()?;
        user.shot(&format!("q831-refused-{code}-rejoined"))?;
    }
    Err(
        format!("the refused answer closed the pre-join and ended the call (its window offers Rejoin: {rejoined:?})")
            .into(),
    )
}

pub(super) fn ring_refused_answer(user: &mut User<'_>) -> SandboxResult {
    let forbidden = refused_with(user, 403);
    let failed = refused_with(user, 500);
    forbidden.and(failed)
}

/// Accept on the ring `id` answers it at once: no pre-join, and a's `accept` of it.
fn answered_at_once(user: &mut User<'_>, id: &str, what: &str) -> SandboxResult {
    let outcome = (|| {
        user.until(what, |state| !state["ringing"].is_null())?;
        thread::sleep(SETTLE);
        request::accept(user)?;
        wait_until(WAIT, || {
            sent_by_a(user)
                .iter()
                .any(|(op, _, call)| op == "accept" && call == id)
                .then_some(())
        })
        .ok_or_else(|| format!("{what}: Accept sent no accept: {:?}", sent_by_a(user)))?;
        let state = user.state()?;
        say(&format!("  {what} after Accept: {state}"));
        ensure(state["prejoin"].is_null(), || {
            format!("{what}: Accept opened a pre-join")
        })?;
        user.focus_other()?;
        user.park()?;
        thread::sleep(SETTLE);
        user.shot(&format!("q831-answered-{}", what.replace(' ', "-")))
            .map(drop)
    })();
    finish(user, id, outcome)
}

pub(super) fn ring_answers_at_once(user: &mut User<'_>) -> SandboxResult {
    user.hang_up()?;
    user.focus_main()?;
    let one = user.mock.ring(LAB_B, LAB_A);
    answered_at_once(user, &one, "1-1 ring")?;
    user.focus_main()?;
    let group = user.mock.ring_group(LAB_B, ABC_CHAT, &[LAB_A]);
    answered_at_once(user, &group, "group ring")?;
    user.focus_main()?;
    let info = json!({"organizerId": request::organizer(), "tenantId": TENANT});
    let named = user.mock.ring_meeting(LAB_B, ABC_CHAT, &[LAB_A], info);
    answered_at_once(user, &named, "group ring with meetingInfo")
}

/// After the answered ring's call ends (`invitee_ring`), its ring does not come back; one that does is declined, so
/// the next checks start with no ring.
pub(super) fn ring_gone_after_leave(user: &mut User<'_>) -> SandboxResult {
    let calls = user.calls()?;
    let state = user.state()?;
    say(&format!("  after invitee_ring: calls {calls:?}; state {state}"));
    let mock_calls: Vec<String> = user
        .mock
        .calls()
        .iter()
        .map(|call| format!("{} {:?} {:?} {:?}", call.id, call.group_chat, call.phase, call.requests))
        .collect();
    say(&format!("  the mock's calls: {mock_calls:?}"));
    let back = !state["ringing"].is_null();
    if back {
        user.focus_main()?;
        user.park()?;
        thread::sleep(SETTLE);
        user.shot("q831-ring-back-after-leave")?;
        user.press("ctrl+shift+d")?;
        user.until("the ring declined", |state| state["ringing"].is_null())?;
    }
    ensure(!back, || {
        format!("the answered ring rings again after the call ended: {state}")
    })
}
