//! QA of task 810 (round 1): Request to join in a meeting, as a user meets it, on the mock. The mock holds each add
//! (`ChatServiceMock::hold_adds`) until the check tells its outcome: the invitee answers, declines (`603`) or does not
//! answer. Every name, id and text is made up.
//!
//! - **The states** (`request_states`): opening People and the pointer on the invited rows send nothing. Request to
//!   join on b sends one `add` in the meeting's shape; b shows "Calling…" under "In this meeting". b declines:
//!   "Declined" with Call again, then b goes back to "Others invited". Request to join again; nobody answers: "No
//!   response"; Call again sends one more `add`; b answers and is in the meeting. Shots of each state in Teams Light
//!   and Teams Dark.
//! - **The keyboard** (`request_keyboard`): Tab to an invited row, then Enter, sends one `add` for that row's person;
//!   the keyboard stays in People.
//! - **Refusals** (`request_refusals`): the account's own row offers no Request to join (Enter and a click there send
//!   nothing); an `add` that the service refuses shows "No response", not a "Calling…" that stays.
//! - **The invitee's side** (`invitee_ring`): a group call's ring that names the meeting's chat but no `meetingInfo`
//!   does not ring; a meeting's Request to join (`meetingInfo`, `conversationType` `scheduledMeeting`) rings with the
//!   meeting's title. Accept opens the meeting's pre-join and sends nothing; Cancel (Escape) lets the ring ring on;
//!   Accept again, then Join now (Enter) joins the meeting. Shots of the ring and the pre-join in each theme.
use super::{
    AB_CHAT, Duration, Instant, LAB_A, LAB_B, MEETING, MeetingCall, POLL, Rect, RgbImage, SETTLE, SUBJECT,
    SandboxResult, TENANT, TIMED_OUT, User, Value, WAIT, ensure, i32_of, join_the_meeting, json, people_focus, pixels,
    say, tab_to, thread, u32_of, wait_until,
};
use teams_mock::AddOutcome;

/// The account's own object id: the made-up meeting's organizer.
pub(super) fn organizer() -> &'static str {
    LAB_A.rsplit(':').next().unwrap_or_default()
}

/// The bodies of the `add`s a sent on the mock's call `call`, in order.
fn add_bodies(user: &User<'_>, call: &str) -> Vec<Value> {
    user.mock
        .calls()
        .into_iter()
        .filter(|known| known.id == call)
        .flat_map(|known| known.add_bodies)
        .collect()
}

/// The `add` `body` asks `invitee` into the meeting in the captured shape: the meeting's chat with no message, and the
/// meeting's organizer and tenant.
fn meeting_shaped(body: &Value, invitee: &str) -> SandboxResult {
    let info = &body["participantInvitationData"]["invitationData"]["meetingInfo"];
    ensure(body["participants"]["to"][0]["id"] == invitee, || {
        format!("the add names {} , not {invitee}", body["participants"]["to"][0]["id"])
    })?;
    ensure(
        body["groupChat"] == json!({"threadId": MEETING, "messageId": null}),
        || format!("the add's groupChat is {}", body["groupChat"]),
    )?;
    ensure(info["organizerId"] == organizer() && info["tenantId"] == TENANT, || {
        format!("the add's meetingInfo is {info}")
    })
}

/// What People's state says of `mri`, asked into the call: `None` when not there.
fn asked_of(state: &Value, mri: &str) -> Option<Value> {
    state["call"]["asked"]
        .as_array()?
        .iter()
        .find(|asked| asked["mri"] == mri)
        .cloned()
}

fn passed_has(state: &Value, mri: &str) -> bool {
    state["call"]["passed"]
        .as_array()
        .is_some_and(|passed| passed.iter().any(|one| one == mri))
}

/// The row of People whose avatar is `avatar`, right of the avatar.
fn row_of(user: &User<'_>, avatar: Rect) -> Rect {
    let pane = user.pane();
    Rect {
        x: pane.x + 80,
        y: avatar.y - 8,
        w: pane.w - 84,
        h: avatar.h + 16,
    }
}

/// The pointer on the row of `avatar`: the button that comes on its right (Request to join), if one comes.
fn hover_button(user: &mut User<'_>, avatar: Rect, shot: Option<&str>) -> SandboxResult<Option<Rect>> {
    let row = row_of(user, avatar);
    let calm = user.screen()?;
    user.hover((user.pane().x + 120, avatar.center().1))?;
    let hovered = user.screen_as_is()?;
    if let Some(name) = shot {
        user.shot(name)?;
    }
    Ok(pixels::changes(&calm, &hovered, row)
        .into_iter()
        .map(|blob| blob.bounds)
        .reduce(Rect::union)
        .filter(|bounds| bounds.w >= 40 && bounds.h >= 12))
}

/// The right-most mark in the row of `avatar` (an icon button at the row's end), on the screen as it is now.
fn row_end_mark(user: &mut User<'_>, avatar: Rect) -> SandboxResult<Option<Rect>> {
    let shot = user.screen()?;
    let row = row_of(user, avatar);
    let ground = shot.get_pixel(u32_of(row.right() - 2), u32_of(row.y + 1)).0;
    let marked = |x: i32, y: i32| {
        shot.get_pixel_checked(u32_of(x), u32_of(y)).is_some_and(|pixel| {
            let distance: i32 = pixel
                .0
                .iter()
                .zip(ground)
                .map(|(one, two)| (i32::from(*one) - i32::from(two)).abs())
                .sum();
            distance > 60
        })
    };
    let end = Rect {
        x: row.right() - 56,
        w: 54,
        ..row
    };
    Ok(pixels::blobs(end, marked)
        .into_iter()
        .map(|blob| blob.bounds)
        .filter(|bounds| bounds.w >= 6 && bounds.h >= 6)
        .max_by_key(|bounds| bounds.right()))
}

/// A shot of the focused window in Teams Light and Teams Dark, named `<name>-<theme>`; the theme ends light.
fn both_themes(user: &mut User<'_>, name: &str) -> SandboxResult {
    for theme in ["light", "dark"] {
        user.set_theme(theme)?;
        user.focus_other()?;
        user.park()?;
        thread::sleep(SETTLE);
        user.shot(&format!("{name}-{theme}"))?;
    }
    user.set_theme("light")
}

/// A shot in Teams Light and Teams Dark, named `<name>-<theme>`, with the focus and the pointer where they are.
fn themes_in_place(user: &mut User<'_>, name: &str) -> SandboxResult {
    let focus = people_focus(user)?;
    for theme in ["light", "dark"] {
        user.set_theme(theme)?;
        user.shot(&format!("{name}-{theme}"))?;
    }
    user.set_theme("light")?;
    let still = people_focus(user)?;
    ensure(still == focus, || {
        format!("the theme change moved the keyboard from {focus:?} to {still:?}")
    })
}

/// Waits until the state of `mri` in People holds `wanted`.
fn until_asked(user: &mut User<'_>, what: &str, mri: &str, wanted: impl Fn(Option<Value>) -> bool) -> SandboxResult {
    user.until(what, |state| wanted(asked_of(state, mri))).map(drop)
}

/// Waits until `count` more `add`s than `before` went out, then a little more: the count then.
fn adds_after(user: &mut User<'_>, before: usize, count: usize) -> SandboxResult<usize> {
    wait_until(WAIT, || (user.sent("add") >= before + count).then_some(()))
        .ok_or_else(|| format!("fewer than {count} adds went out"))?;
    thread::sleep(SETTLE * 2);
    Ok(user.sent("add") - before)
}

/// a is in the meeting, People open, every invited row under "Others invited".
fn in_the_meeting(user: &mut User<'_>) -> SandboxResult<MeetingCall> {
    if user.meeting.is_none() {
        join_the_meeting(user)?;
    }
    user.focus_other()?;
    user.open_people()?;
    user.meeting()
}

pub(super) fn request_states(user: &mut User<'_>) -> SandboxResult {
    user.mock.hold_adds(true);
    let outcome = states(user);
    user.mock.hold_adds(false);
    outcome
}

/// The call requests a sent on the mock so far.
fn requests_of_a(user: &User<'_>) -> Vec<String> {
    user.mock
        .calls()
        .iter()
        .flat_map(|call| call.requests.iter())
        .filter(|(_, by)| by == LAB_A)
        .map(|(op, _)| op.clone())
        .collect()
}

fn states(user: &mut User<'_>) -> SandboxResult {
    if user.meeting.is_none() {
        join_the_meeting(user)?;
    }
    let meeting = user.meeting()?;
    // the harness finds People by Raise's Ctrl+Shift+K (a hand up and down): before the count
    user.focus_other()?;
    user.people_button()?;
    thread::sleep(SETTLE * 2);
    let before = user.sent("add");
    let requests_before = requests_of_a(user);
    user.focus_other()?;
    user.open_people()?;
    let rows = user.until_rows("before the hover", &[1, 10])?;
    // the pointer over every invited row, then off: nothing goes out by itself
    for avatar in rows.avatars.iter().skip(1).copied().collect::<Vec<_>>() {
        user.hover((user.pane().x + 120, avatar.center().1))?;
    }
    user.park()?;
    thread::sleep(SETTLE * 2);
    let requests_after = requests_of_a(user);
    ensure(user.sent("add") == before && requests_after == requests_before, || {
        format!(
            "People's open and hover sent {:?}",
            &requests_after[requests_before.len().min(requests_after.len())..]
        )
    })?;
    let rows = user.until_rows("before the request", &[1, 10])?;
    let first = *rows.avatars.get(1).ok_or("no invited row")?;
    let button = hover_button(user, first, Some("request-hover"))?.ok_or("no Request to join on b's row")?;
    user.click(button.center())?;
    user.park()?;
    let sent = adds_after(user, before, 1)?;
    ensure(sent == 1, || format!("{sent} adds for one Request to join"))?;
    let bodies = add_bodies(user, &meeting.call);
    say(&format!("  the add: {}", bodies.last().cloned().unwrap_or_default()));
    meeting_shaped(bodies.last().ok_or("no add body")?, LAB_B)?;
    until_asked(user, "b called", LAB_B, |asked| {
        asked.is_some_and(|asked| asked["no_response"] == false)
    })?;
    user.until_rows("b under In this meeting", &[2, 9])?;
    both_themes(user, "request-calling")?;
    declined(user, &meeting)?;
    no_answer_then_joined(user, &meeting)
}

/// b declines: "Declined" with Call again, then b goes back to "Others invited".
fn declined(user: &mut User<'_>, meeting: &MeetingCall) -> SandboxResult {
    // b declines: "Declined" with Call again
    ensure(user.mock.tell_add(&meeting.call, LAB_B, AddOutcome::Declined), || {
        "the mock held no add of b's".to_owned()
    })?;
    until_asked(user, "b declined", LAB_B, |asked| {
        asked.is_some_and(|asked| asked["declined"] == true)
    })?;
    let rows = user.until_rows("Declined under In this meeting", &[2, 9])?;
    let declined_at = Instant::now();
    let again = row_end_mark(user, *rows.avatars.get(1).ok_or("no row for b")?)?;
    say(&format!("  Call again on Declined at {again:?}"));
    both_themes(user, "request-declined")?;
    ensure(again.is_some(), || "no Call again on the Declined row".to_owned())?;
    // Teams moves the row back to "Others invited" a little later, with "No response" under the name
    user.until("b back under Others invited", |state| {
        asked_of(state, LAB_B).is_none() && passed_has(state, LAB_B)
    })?;
    say(&format!("  b went back after {:?}", declined_at.elapsed()));
    user.until_rows("b back under Others invited", &[1, 10])?;
    both_themes(user, "request-passed")
}

/// Request to join on b again; nobody answers: "No response"; Call again; b answers.
fn no_answer_then_joined(user: &mut User<'_>, meeting: &MeetingCall) -> SandboxResult {
    let rows = user.until_rows("b under Others invited", &[1, 10])?;
    // Request to join again; nobody answers: "No response" with Call again
    let before = user.sent("add");
    let row = *rows.avatars.get(1).ok_or("no invited row")?;
    let button = hover_button(user, row, None)?.ok_or("no Request to join on b's row after the decline")?;
    user.click(button.center())?;
    user.park()?;
    ensure(adds_after(user, before, 1)? == 1, || {
        "not one add for the second request".to_owned()
    })?;
    until_asked(user, "b called again", LAB_B, |asked| {
        asked.is_some_and(|asked| asked["no_response"] == false)
    })?;
    ensure(user.mock.tell_add(&meeting.call, LAB_B, AddOutcome::NoAnswer), || {
        "the mock held no second add of b's".to_owned()
    })?;
    until_asked(user, "b did not answer", LAB_B, |asked| {
        asked.is_some_and(|asked| asked["no_response"] == true && asked["declined"] == false)
    })?;
    let rows = user.until_rows("No response under In this meeting", &[2, 9])?;
    both_themes(user, "request-no-response")?;
    let again = row_end_mark(user, *rows.avatars.get(1).ok_or("no row for b")?)?
        .ok_or("no Call again on the No response row")?;
    let before = user.sent("add");
    user.click(again.center())?;
    user.park()?;
    let sent = adds_after(user, before, 1)?;
    ensure(sent == 1, || format!("{sent} adds for one Call again"))?;
    meeting_shaped(add_bodies(user, &meeting.call).last().ok_or("no add body")?, LAB_B)?;
    until_asked(user, "b called by Call again", LAB_B, |asked| {
        asked.is_some_and(|asked| asked["no_response"] == false)
    })?;
    // b answers: a joined person
    ensure(user.mock.tell_add(&meeting.call, LAB_B, AddOutcome::Joined), || {
        "the mock held no add of b's for Call again".to_owned()
    })?;
    user.until("b in the meeting", |state| {
        User::participants(state).iter().any(|mri| mri == LAB_B) && asked_of(state, LAB_B).is_none()
    })?;
    user.until_rows("b joined", &[2, 9])?;
    both_themes(user, "request-joined")?;
    let _ = user.mock.leave_made_up(&meeting.call, LAB_B);
    user.until_rows("b left", &[1, 10])?;
    Ok(())
}

pub(super) fn request_keyboard(user: &mut User<'_>) -> SandboxResult {
    user.mock.hold_adds(true);
    let outcome = keyboard_request(user);
    user.mock.hold_adds(false);
    outcome
}

fn keyboard_request(user: &mut User<'_>) -> SandboxResult {
    let meeting = in_the_meeting(user)?;
    user.until_rows("before the keyboard", &[1, 10])?;
    user.park()?;
    tab_to(user, "call-roster-added-1")?;
    thread::sleep(SETTLE);
    themes_in_place(user, "request-keyboard-on-row")?;
    let before = user.sent("add");
    // Enter on the row itself, then Tab to its Request to join and Enter
    user.press("enter")?;
    thread::sleep(SETTLE * 2);
    let on_row = user.sent("add") - before;
    say(&format!("  Enter on the row sent {on_row} adds"));
    if on_row == 0 {
        user.press("tab")?;
        thread::sleep(SETTLE);
        user.shot("request-keyboard-on-button")?;
        user.press("enter")?;
    }
    let sent = adds_after(user, before, 1)?;
    ensure(sent == 1, || format!("{sent} adds for one Enter"))?;
    let body = add_bodies(user, &meeting.call).last().cloned().ok_or("no add body")?;
    let invitee = body["participants"]["to"][0]["id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    say(&format!("  Enter asked {invitee} in"));
    ensure(invitee != LAB_A && !invitee.is_empty(), || {
        format!("Enter asked {invitee:?}")
    })?;
    meeting_shaped(&body, &invitee)?;
    let outcome = no_second_request(user, &meeting);
    let _ = user.mock.tell_add(&meeting.call, &invitee, AddOutcome::NoAnswer);
    user.press("escape")?;
    outcome
}

/// A second Enter where the keyboard stays after the move must not ask the next person in.
fn no_second_request(user: &mut User<'_>, meeting: &MeetingCall) -> SandboxResult {
    user.until_rows("the invitee under In this meeting", &[2, 9])?;
    say(&format!(
        "  the keyboard is on {:?} after the move",
        people_focus(user)?
    ));
    thread::sleep(SETTLE);
    themes_in_place(user, "request-keyboard-after")?;
    let before = user.sent("add");
    let bodies = add_bodies(user, &meeting.call).len();
    user.press("enter")?;
    thread::sleep(SETTLE * 3);
    let again: Vec<String> = add_bodies(user, &meeting.call)
        .iter()
        .skip(bodies)
        .filter_map(|body| body["participants"]["to"][0]["id"].as_str().map(str::to_owned))
        .collect();
    let second = user.sent("add") - before;
    say(&format!("  a second Enter sent {second} adds"));
    let shot = user.shot("request-keyboard-second-enter");
    for other in &again {
        let _ = user.mock.tell_add(&meeting.call, other, AddOutcome::NoAnswer);
    }
    shot?;
    ensure(second == 0, || {
        format!(
            "after the request the keyboard sat on another person's Request to join: a second Enter asked {again:?}"
        )
    })
}

pub(super) fn request_refusals(user: &mut User<'_>) -> SandboxResult {
    let meeting = in_the_meeting(user)?;
    let rows = user.rows()?;
    ensure(rows.sections.len() == 2, || format!("People shows {:?}", rows.sections))?;
    let own = *rows.avatars.first().ok_or("no row of the account's")?;
    let before = user.sent("add");
    let shown = hover_button(user, own, Some("request-own-row-hover"))?;
    say(&format!("  the pointer on the own row brought {shown:?}"));
    user.park()?;
    tab_to(user, "call-roster-self")?;
    user.press("enter")?;
    thread::sleep(SETTLE * 2);
    user.press("escape")?;
    ensure(user.sent("add") == before, || {
        "the account's own row sent an add".to_owned()
    })?;
    // the service refuses the add: the row says so, no "Calling…" stays
    user.mock.refuse_next_call_op("add", 403);
    let rows = user.rows()?;
    let other = *rows.avatars.last().ok_or("no invited row")?;
    let button = hover_button(user, other, None)?.ok_or("no Request to join on the invited row")?;
    user.click(button.center())?;
    user.park()?;
    adds_after(user, before, 1)?;
    let body = add_bodies(user, &meeting.call).last().cloned().ok_or("no add body")?;
    let invitee = body["participants"]["to"][0]["id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    until_asked(user, "the refused add told", &invitee, |asked| {
        asked.is_some_and(|asked| asked["no_response"] == true)
    })?;
    thread::sleep(SETTLE);
    user.shot("request-refused")?;
    user.hang_up()?;
    user.meeting = None;
    Ok(())
}

/// The Answer of the banner: the largest solid green button of the window.
pub(super) fn answer_button(image: &RgbImage) -> Option<(i32, i32)> {
    let within = Rect {
        x: 0,
        y: 0,
        w: i32_of(image.width()),
        h: i32_of(image.height()),
    };
    let [r, g, b] = [0x23, 0x7b, 0x4b];
    pixels::blobs(within, |x, y| {
        image.get_pixel_checked(u32_of(x), u32_of(y)).is_some_and(|pixel| {
            let [pr, pg, pb] = pixel.0.map(i32::from);
            (pr - r).abs() + (pg - g).abs() + (pb - b).abs() < 40
        })
    })
    .into_iter()
    .map(|blob| blob.bounds)
    .filter(|bounds| bounds.w >= 24 && bounds.h >= 16)
    .max_by_key(|bounds| bounds.area())
    .map(Rect::center)
}

/// The desktop notification of a meeting's Request to join uses Teams' words.
fn meeting_notified(user: &mut User<'_>, seen: usize) -> SandboxResult {
    let asks = format!("wants you to join {SUBJECT}");
    // the notification waits for the meeting's title from the chat list
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let notices: Vec<String> = user
            .sandbox
            .bus()
            .notifications()
            .into_iter()
            .skip(seen)
            .map(|notice| format!("{} / {}", notice.summary, notice.body))
            .collect();
        let told = notices.iter().any(|notice| notice.ends_with(&format!(" / {asks}")));
        if told || Instant::now() > deadline {
            say(&format!("  the ring's notifications: {notices:?}"));
            return ensure(told, || format!("no notification says \"{asks}\": {notices:?}"));
        }
        thread::sleep(POLL);
    }
}

/// The call window of the answered Request to join has the meeting's title.
fn meeting_titled(user: &mut User<'_>) -> SandboxResult {
    let titles: Vec<String> = user
        .windows()?
        .iter()
        .map(|window| window["title"].as_str().unwrap_or_default().to_owned())
        .collect();
    say(&format!("  the windows: {titles:?}"));
    ensure(titles.iter().any(|title| title == SUBJECT), || {
        format!("no call window has the meeting's title: {titles:?}")
    })
}

pub(super) fn invitee_ring(user: &mut User<'_>) -> SandboxResult {
    user.hang_up()?;
    user.meeting = None;
    user.open_chat(AB_CHAT)?;
    // a group call's ring in the meeting's chat with no meetingInfo: no meeting's ring
    let plain = user.mock.ring_group(LAB_B, MEETING, &[LAB_A]);
    thread::sleep(Duration::from_secs(5));
    let rang = user.calls()?.into_iter().any(|row| row.ringing);
    let _ = user.mock.end_ring(&plain, 0, TIMED_OUT);
    ensure(!rang, || {
        "a ring in the meeting's chat without meetingInfo rang".to_owned()
    })?;
    // the meeting's Request to join
    let seen = user.sandbox.bus().notifications().len();
    let info = json!({"organizerId": organizer(), "tenantId": TENANT});
    let asked = user.mock.ring_meeting(LAB_B, MEETING, &[LAB_A], info);
    let row = user.wait_call("the meeting's ring", |row| row.ringing)?;
    say(&format!("  the ring: {row:?}"));
    ensure(row.chat.as_deref() == Some(MEETING), || {
        format!("the ring is in {:?}, not the meeting's chat", row.chat)
    })?;
    meeting_notified(user, seen)?;
    for theme in ["light", "dark"] {
        user.set_theme(theme)?;
        user.focus_main()?;
        user.park()?;
        thread::sleep(SETTLE);
        user.shot(&format!("invitee-ring-{theme}"))?;
    }
    through_the_prejoin(user)?;
    let joined = user.wait_call("a in the meeting after Join now", |row| row.state == "connected");
    thread::sleep(SETTLE);
    let _ = user.shot("invitee-answered");
    if joined.is_ok() {
        user.set_theme("dark")?;
        user.focus_other()?;
        user.park()?;
        thread::sleep(SETTLE);
        let _ = user.shot("invitee-answered-dark");
        user.set_theme("light")?;
    }
    let in_meeting = user.state()?;
    say(&format!("  after Join now: {}", in_meeting["call"]));
    let titled = meeting_titled(user);
    user.hang_up()?;
    let _ = user.mock.hang_up_call(&asked);
    joined?;
    ensure(in_meeting["call"]["chat"] == MEETING, || {
        format!("Join now joined {}, not the meeting", in_meeting["call"]["chat"])
    })?;
    titled?;
    ensure(in_meeting["call"]["kind"] == "Meeting", || {
        format!("Join now's call is no meeting's: {}", in_meeting["call"])
    })
}

/// Accept opens the meeting's pre-join with its title, Cancel lets the ring ring on, and Accept again then Join now
/// (Enter) answers; shots of the pre-join in each theme.
fn through_the_prejoin(user: &mut User<'_>) -> SandboxResult {
    // Accept opens the meeting's pre-join, as Teams' Accept: nothing goes out, and the ring rings on
    accept(user)?;
    let shown = user.until("the meeting's pre-join with its title", |state| {
        state["prejoin"]["title"] == SUBJECT
    })?;
    say(&format!("  after Accept: {shown}"));
    ensure(
        shown["prejoin"]["chat"] == MEETING
            && shown["prejoin"]["ring"] == true
            && !shown["ringing"].is_null()
            && shown["call"].is_null(),
        || format!("Accept did not open the ring's pre-join alone: {shown}"),
    )?;
    let title = user.focus_other()?;
    ensure(title == SUBJECT, || {
        format!("the pre-join's window is titled {title:?}")
    })?;
    for theme in ["light", "dark"] {
        user.set_theme(theme)?;
        user.focus_other()?;
        user.park()?;
        thread::sleep(SETTLE);
        user.shot(&format!("invitee-prejoin-{theme}"))?;
    }
    user.set_theme("light")?;
    // Cancel (Escape): the pre-join goes, and the ring rings on with its banner
    user.focus_other()?;
    user.press("escape")?;
    let cancelled = user.until("the ring after Cancel", |state| {
        state["prejoin"].is_null() && !state["ringing"].is_null()
    })?;
    ensure(cancelled["call"].is_null(), || format!("Cancel answered: {cancelled}"))?;
    // Accept again, then Join now with the keyboard
    accept(user)?;
    user.until("the pre-join again", |state| state["prejoin"]["ring"] == true)?;
    user.focus_other()?;
    user.press("enter")?;
    Ok(())
}

/// Accept on the ring's banner in the main window, as the user clicks it.
pub(super) fn accept(user: &mut User<'_>) -> SandboxResult {
    user.focus_main()?;
    let screen = user.screen()?;
    let answer = answer_button(&screen).ok_or("no Accept on the ring")?;
    say(&format!("  Accept at {answer:?}"));
    user.click(answer)?;
    user.park()
}
