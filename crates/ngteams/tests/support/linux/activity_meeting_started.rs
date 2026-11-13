//! A meeting that started, as a user meets it (#125): b's Meet now in the a–b–c group chat shows a's banner, titled
//! with the chat and saying who started it, with Join; its Join asks the calls to join the chat's running meeting (the
//! test engine has no calling, so the refusal's banner then shows and is closed); a's own Meet now shows none; the
//! meeting's end closes a banner still up; with Settings → Notifications → "Meeting started notification" off, none
//! shows. The starts are posted on the mock as the service posts them (`Event/Call`, made up).

use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use teams_mock::{ABC_CHAT, ChatServiceMock, LAB_A, LAB_B};
use ui_sandbox::{SandboxResult, wait_until};

use super::{BANNER_CLOSE, Qa, ensure, say};

/// How long a banner may take to come, and how long none coming counts as none.
const BANNER_WAIT: Duration = Duration::from_secs(5);
/// The made-up conversation the meeting runs at, on the calling service's host.
const LINK: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/SyntheticHuddle?i=10-0-0-2&amp;e=1";

pub(super) const CHECKS: [super::Check; 4] = [
    (
        "meet_now_in_a_group_chat_shows_a_banner_with_join",
        someone_else_s_meet_now,
    ),
    ("own_meet_now_shows_no_banner", own_meet_now),
    ("meeting_s_end_closes_its_banner", end_closes),
    ("meeting_started_switch_off_shows_no_banner", switch_off),
];

/// The mock's answers to the questions of these checks: `push-meet-now <a|b> <n>`, `push-meet-now-end <n>`.
pub(super) fn mock_reply(verb: &str, rest: &str, mock: &ChatServiceMock) -> Option<Value> {
    let mut words = rest.split(' ');
    match verb {
        "push-meet-now" => {
            let (who, name) = if words.next() == Some("a") {
                (LAB_A, "Lab A")
            } else {
                (LAB_B, "Lab B")
            };
            let call = words.next().unwrap_or("1");
            Some(posted(mock, (who, name), &start(who, call)))
        }
        "push-meet-now-end" => {
            let call = words.next().unwrap_or("1");
            Some(posted(mock, (LAB_B, "Lab B"), &end(call)))
        }
        _ => None,
    }
}

fn posted(mock: &ChatServiceMock, from: (&str, &str), content: &str) -> Value {
    mock.add_message(ABC_CHAT, from, "Event/Call", content)
        .map_or_else(|| json!({"error": "no a-b-c chat"}), |id| json!({"id": id}))
}

fn start(starter: &str, call: &str) -> String {
    format!(
        "<partlist type=\"started\" alt=\"\"><part identity=\"{starter}\"><name>{starter}</name></part></partlist>\
         <conversationType>\"huddleGroupCall\"</conversationType><callEventType>callStarted</callEventType>\
         <conversationUrl>{LINK}</conversationUrl><callId>made-up-meet-now-{call}</callId>"
    )
}

fn end(call: &str) -> String {
    format!(
        "<ended/><partlist alt=\"\" count=\"0\"></partlist><conversationType>\"huddleGroupCall\"</conversationType>\
         <callEventType>callEnded</callEventType><conversationUrl>{LINK}</conversationUrl>\
         <callId>made-up-meet-now-{call}</callId>"
    )
}

/// b's Meet now: a's banner, "<chat>", "Lab B started a meeting", with Join; Join asks to join the chat's call.
fn someone_else_s_meet_now(qa: &mut Qa<'_>) -> SandboxResult {
    let shown = qa.sandbox.bus().notifications().len();
    qa.trace.drain();
    qa.ask("push-meet-now b 1")?;
    let banner = wait_until(BANNER_WAIT, || qa.sandbox.bus().notifications().get(shown).cloned())
        .ok_or("no banner for b's Meet now in the group chat")?;
    say(&format!(
        "b's Meet now: {:?}: {:?}, actions {:?}",
        banner.summary, banner.body, banner.actions
    ));
    ensure(banner.body == "Lab B started a meeting", || {
        format!("the banner says {:?}", banner.body)
    })?;
    ensure(!banner.summary.is_empty() && banner.summary != "Meeting", || {
        format!("the banner isn't titled with the chat: {:?}", banner.summary)
    })?;
    let offers = |id: &str, label: &str| banner.actions.windows(2).any(|pair| pair[0] == id && pair[1] == label);
    ensure(offers("join", "Join") && offers("default", "Open chat"), || {
        format!("the banner's actions: {:?}", banner.actions)
    })?;
    qa.sandbox.bus().invoke_action(banner.id, "join")?;
    let asked = qa.trace.next_where("a join asked in the group chat", |line| {
        line["view"] == "call" && line["change"] == "asked" && line.to_string().contains(ABC_CHAT)
    })?;
    say(&format!("the banner's Join: {asked}"));
    // the test engine has no calling: the refusal's banner pushes the panes down until it is closed
    thread::sleep(Duration::from_millis(700));
    qa.shot("meeting-started-joined")?;
    qa.click(BANNER_CLOSE)?;
    ensure(qa.sandbox.bus().closed_notifications().contains(&banner.id), || {
        "the banner stayed after its Join".to_owned()
    })
}

/// a's own Meet now (another client of a's started it): no banner.
fn own_meet_now(qa: &mut Qa<'_>) -> SandboxResult {
    let shown = qa.sandbox.bus().notifications().len();
    qa.ask("push-meet-now a 2")?;
    thread::sleep(BANNER_WAIT);
    let banners = qa.sandbox.bus().notifications().len() - shown;
    ensure(banners == 0, || format!("{banners} banners for a's own Meet now"))
}

/// A banner not acted on closes once the meeting ends.
fn end_closes(qa: &mut Qa<'_>) -> SandboxResult {
    let shown = qa.sandbox.bus().notifications().len();
    qa.ask("push-meet-now b 3")?;
    let banner = wait_until(BANNER_WAIT, || qa.sandbox.bus().notifications().get(shown).cloned())
        .ok_or("no banner for b's second Meet now")?;
    qa.shot("meeting-started-banner")?;
    qa.ask("push-meet-now-end 3")?;
    wait_until(BANNER_WAIT, || {
        qa.sandbox
            .bus()
            .closed_notifications()
            .contains(&banner.id)
            .then_some(())
    })
    .ok_or("the banner stayed after the meeting ended")?;
    Ok(())
}

/// Settings → Notifications → "Meeting started notification" off: b's Meet now shows no banner; on again after.
fn switch_off(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ctl_json(&["open", "settings", "notifications"])?;
    qa.shot("meeting-started-setting")?;
    qa.ask("meeting-started off")?;
    let shown = qa.sandbox.bus().notifications().len();
    qa.ask("push-meet-now b 4")?;
    thread::sleep(BANNER_WAIT);
    let banners = qa.sandbox.bus().notifications().len() - shown;
    qa.ask("meeting-started on")?;
    ensure(banners == 0, || format!("{banners} banners with the switch off"))
}
