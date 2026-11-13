//! A group chat's Meet now, joined from its banner on the mock's calling service (#125): b's meeting in the a–b–c chat
//! runs on the mock with nobody rung, its start posted in the chat as the service posts it (`Event/Call`, made up, at
//! the call's own conversation); a's banner's Join joins that call (the mock gets a's `join` at its conversation), the
//! call window opens connected and the banner closes; b ends it. Then b's group call and b's 1:1 call ring a as before,
//! their starts posted in their chats, with no meeting banner beside the ring.

use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{fs, thread};

use teams_ctl::protocol::{OpenTarget, Request};
use teams_mock::{AB_CHAT, ABC_CHAT, LAB_A, LAB_B, TIMED_OUT};
use ui_sandbox::bus::Notification;
use ui_sandbox::{SandboxResult, wait_until};

use super::{EVIDENCE_ENV, Rig, SCREEN, SETTLE, WAIT, i32_of, say};

/// How long no meeting banner beside a ring counts as none.
const QUIET: Duration = Duration::from_secs(4);

/// The steps (see the module docs).
pub(super) fn meet_now(rig: &mut Rig<'_>) -> SandboxResult {
    joined_from_the_banner(rig)?;
    rings_without_a_banner(rig, true)?;
    rings_without_a_banner(rig, false)
}

/// b's Meet now: a's banner, its Join joins the mock's call, the banner closes; b ends it.
fn joined_from_the_banner(rig: &mut Rig<'_>) -> SandboxResult {
    // Notifications connect before the tray registers.
    wait_until(WAIT, || (!rig.sandbox.bus().tray_items().is_empty()).then_some(()))
        .ok_or("the desktop notification connection isn't ready")?;
    let seen = rig.sandbox.bus().notifications().len();
    let call = rig.mock.ring_group(LAB_B, ABC_CHAT, &[]);
    // nobody rung: the meeting runs with b in it, to be joined
    if !rig.mock.end_ring(&call, 0, TIMED_OUT) {
        return Err("no Meet now on the mock".into());
    }
    let conversation = conversation_of(rig, &call)?;
    rig.ctl.request(
        None,
        Request::Open {
            target: OpenTarget::Chat {
                chat: ABC_CHAT.to_owned(),
            },
        },
    )?;
    let joins_shown = buttons(rig.app.trace.catch_up(), "join");
    post(rig, ABC_CHAT, &start(&conversation, &call, true))?;
    let banner =
        wait_until(WAIT, || meeting_banners(rig, seen).first().cloned()).ok_or("no banner for b's Meet now")?;
    rig.app.trace.wait_until("the chat's header offering Join", |seen| {
        buttons(seen, "join") > joins_shown
    })?;
    screen(rig, "large-call-meet-now-header-join")?;
    say(&format!(
        "meet-now: {:?}: {:?} {:?}",
        banner.summary, banner.body, banner.actions
    ));
    let opened = |seen: &[serde_json::Value]| {
        seen.iter()
            .filter(|line| line["view"] == "call-window" && line["change"] == "opened")
            .count()
    };
    let before = opened(rig.app.trace.catch_up());
    let joins = joins_of_a(rig, &conversation);
    rig.window_id = None;
    rig.sandbox.bus().invoke_action(banner.id, "join")?;
    wait_until(WAIT, || (joins_of_a(rig, &conversation) > joins).then_some(()))
        .ok_or("the banner's Join reached no join of the Meet now on the mock")?;
    rig.wait_call("connected in the Meet now", |row| row.state == "connected")?;
    rig.app
        .trace
        .wait_until("the call window opened", |seen| opened(seen) > before)?;
    rig.wait_window(|size| size.0 == i32_of(SCREEN.0))?;
    rig.park()?;
    thread::sleep(SETTLE);
    rig.save("large-call-meet-now-joined")?;
    wait_until(WAIT, || {
        rig.sandbox
            .bus()
            .closed_notifications()
            .contains(&banner.id)
            .then_some(())
    })
    .ok_or("the banner stayed after its Join")?;
    if !rig.mock.hang_up_call(&call) {
        return Err("the Meet now didn't end".into());
    }
    let calls_shown = buttons(rig.app.trace.catch_up(), "call");
    post(rig, ABC_CHAT, &end(&conversation, &call, true))?;
    no_call(rig)?;
    rig.app
        .trace
        .wait_until("the chat's header offering Call again", |seen| {
            buttons(seen, "call") > calls_shown
        })?;
    screen(rig, "large-call-meet-now-header-ended")
}

/// b's group call (`group`) or 1:1 call rings a, its start posted in its chat: no meeting banner beside the ring.
fn rings_without_a_banner(rig: &mut Rig<'_>, group: bool) -> SandboxResult {
    let what = if group { "b's group call" } else { "b's 1:1 call" };
    let seen = rig.sandbox.bus().notifications().len();
    let (call, chat) = if group {
        (rig.mock.ring_group(LAB_B, ABC_CHAT, &[LAB_A]), ABC_CHAT)
    } else {
        (rig.mock.ring(LAB_B, LAB_A), AB_CHAT)
    };
    let conversation = conversation_of(rig, &call)?;
    post(rig, chat, &start(&conversation, &call, false))?;
    rig.wait_call(&format!("{what} ringing"), |row| row.ringing)?;
    thread::sleep(QUIET);
    let banners = meeting_banners(rig, seen);
    let rings = rig.sandbox.bus().notifications().len() - seen;
    say(&format!("{what}: {rings} notifications, meeting banners {banners:?}"));
    let _ = rig.mock.end_ring(&call, 0, TIMED_OUT);
    let _ = rig.mock.hang_up_call(&call);
    post(rig, chat, &end(&conversation, &call, false))?;
    no_call(rig)?;
    if !banners.is_empty() {
        return Err(format!("a meeting banner for {what}: {banners:?}").into());
    }
    if rings == 0 {
        return Err(format!("{what} rang with no notification").into());
    }
    Ok(())
}

/// How many times the a–b–c chat's header showed the call button `kind` (`join`, `call`).
fn buttons(seen: &[serde_json::Value], kind: &str) -> usize {
    seen.iter()
        .filter(|line| {
            line["view"] == "call-button"
                && line["change"] == "shown"
                && line["kinds"] == serde_json::json!([kind])
                && line.to_string().contains(ABC_CHAT)
        })
        .count()
}

/// The whole screen as `name.png`, kept with the other shots and in the evidence dir.
fn screen(rig: &Rig<'_>, name: &str) -> SandboxResult {
    thread::sleep(SETTLE);
    let file = format!("{name}.png");
    let png = rig.sandbox.screenshot(&file, false)?;
    let mut dirs = vec![
        Path::new(env!("CARGO_TARGET_TMPDIR"))
            .parent()
            .map_or_else(|| PathBuf::from("target"), Path::to_owned)
            .join("ui-shots"),
    ];
    dirs.extend(std::env::var_os(EVIDENCE_ENV).map(PathBuf::from));
    for dir in dirs {
        fs::create_dir_all(&dir)?;
        fs::copy(&png, dir.join(&file))?;
    }
    Ok(())
}

/// Waits until a is in no call and nothing rings.
fn no_call(rig: &mut Rig<'_>) -> SandboxResult {
    wait_until(WAIT, || {
        let calls = rig.calls().ok()?;
        calls
            .iter()
            .all(|row| !row.ringing && row.state != "connected")
            .then_some(())
    })
    .ok_or_else(|| "a call stayed up".into())
}

fn conversation_of(rig: &Rig<'_>, call: &str) -> SandboxResult<String> {
    rig.mock
        .calls()
        .into_iter()
        .find(|known| known.id == call)
        .map(|known| known.conversation)
        .ok_or_else(|| format!("no call {call} on the mock").into())
}

/// a's joins of the call at `conversation` the mock got so far.
fn joins_of_a(rig: &Rig<'_>, conversation: &str) -> usize {
    rig.mock
        .calls()
        .iter()
        .filter(|call| call.conversation == conversation)
        .flat_map(|call| call.requests.iter())
        .filter(|(op, by)| op == "join" && by == LAB_A)
        .count()
}

/// The meeting banners shown since `seen` (those saying someone started one).
fn meeting_banners(rig: &Rig<'_>, seen: usize) -> Vec<Notification> {
    rig.sandbox
        .bus()
        .notifications()
        .into_iter()
        .skip(seen)
        .filter(|banner| banner.body.contains(" started "))
        .collect()
}

fn post(rig: &Rig<'_>, chat: &str, content: &str) -> SandboxResult {
    rig.mock
        .add_message(chat, (LAB_B, "Lab B"), "Event/Call", content)
        .map(|_| ())
        .ok_or_else(|| format!("no chat {chat}").into())
}

/// Where the mock's call at `conversation` runs, as a call's events name it.
fn link(conversation: &str) -> String {
    format!(
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/{conversation}?i=10-0-0-2&amp;e=1"
    )
}

fn kind(huddle: bool) -> &'static str {
    if huddle {
        "<conversationType>\"huddleGroupCall\"</conversationType>"
    } else {
        ""
    }
}

/// b's start of the call at `conversation`: a group chat's meeting (Meet now) when `huddle`.
fn start(conversation: &str, call: &str, huddle: bool) -> String {
    format!(
        "<partlist type=\"started\" alt=\"\"><part identity=\"{LAB_B}\"><name>{LAB_B}</name></part></partlist>{}\
         <callEventType>callStarted</callEventType><conversationUrl>{}</conversationUrl><callId>{call}</callId>",
        kind(huddle),
        link(conversation)
    )
}

fn end(conversation: &str, call: &str, huddle: bool) -> String {
    format!(
        "<ended/><partlist alt=\"\" count=\"0\"></partlist>{}<callEventType>callEnded</callEventType>\
         <conversationUrl>{}</conversationUrl><callId>{call}</callId>",
        kind(huddle),
        link(conversation)
    )
}
