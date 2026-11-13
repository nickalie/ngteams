//! A meeting that started, as a user meets it (#125). The starts and ends are posted on the mock as the service posts
//! them (`Event/Call`, made up); a meeting on the calendar is the mock calendar's next answer (a made-up meeting from 5
//! minutes ago to 25 minutes from now), as the service answers a calendar read.
//!
//! - b's Meet now in the a–b–c group chat: a's banner, titled with the chat, "Lab B started a meeting", with Join and
//!   Open chat; Join asks the calls to join the chat's running meeting (the test engine has no calling, so the
//!   refusal's banner then shows and is closed; `large_call_window`'s `meet-now` step joins on the mock's calling) and
//!   the banner closes; Open chat opens the chat; the meeting's end closes a banner still up; while it runs the chat's
//!   header offers Join, after its end not.
//! - A meeting on the calendar started by b, in its meeting chat and in a channel: a banner titled with its subject,
//!   "Lab B started the meeting"; Join opens the meeting's pre-join, Open chat the chat or the channel.
//! - No banner for a's own Meet now, b's group call or 1:1 call, a channel's Meet now, a meeting chat's meeting not on
//!   the calendar, with Do not disturb, notifications Off, a call's Mute notifications, a muted channel, or Settings →
//!   Notifications → "Meeting started notification" off (its page shot in Teams Light and Dark).
//! - A catch-up after the push channel came back, or asked for, shows no banner again.
//! - Across restarts of the app on one data dir ([`restart_runs`]): an old Meet now running brings no banner, the
//!   header offers Join after a restart, and no longer once it ended, also after the next restart.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;
use std::{fs, io};

use gpui::{App, Entity};
use serde_json::{Value, json};
use teams_mock::{AB_CHAT, ABC_CHAT, CHANNEL, ChatServiceMock, LAB_A, LAB_B, LAB_C};
use teams_proto::Transport as _;
use teams_ui::shell::Shell;
use ui_sandbox::{Sandbox, SandboxResult, wait_until};

use super::{BANNER_CLOSE, CTL_SOCKET, Check, Probe, Qa, ensure, say};

/// How long a banner may take to come, and how long none coming counts as none.
const BANNER_WAIT: Duration = Duration::from_secs(5);
/// The made-up conversation the meeting runs at, on the calling service's host.
const LINK: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/SyntheticHuddle?i=10-0-0-2&amp;e=1";
/// The made-up meeting on a's calendar, its chat, and another meeting's chat the calendar doesn't hold.
const MEETING_CHAT: &str = "19:meeting_MadeUpStartedMeeting@thread.v2";
const UNSCHEDULED_CHAT: &str = "19:meeting_MadeUpUnscheduled@thread.v2";
const SUBJECT: &str = "Made-up planning review";
const CHANNEL_SUBJECT: &str = "Made-up channel review";
/// The channel meeting's post, as its calendar event names it.
const CHANNEL_POST: &str = "1760000000999";
/// The child mode of the restart runs, and the data dir they share.
pub(super) const RESTART_MODE: &str = "meeting-restart";
const DATA_ENV: &str = "NGTEAMS_ACTIVITY_MEETING_DATA";
/// How many of the calendar's reads the made-up meetings answer.
const CALENDAR_READS: usize = 30;
/// The notifications the sandbox showed before the restart run now running started.
static BEFORE_RUN: AtomicUsize = AtomicUsize::new(0);
/// How many calendar answers the child mounted on the mock so far.
static MOUNTED: AtomicU64 = AtomicU64::new(0);
/// The call of the Meet now running when the restart runs start.
const RUNNING_CALL: &str = "50";

pub(super) const CHECKS: [Check; 19] = [
    (
        "meet_now_in_a_group_chat_shows_a_banner_with_join",
        someone_else_s_meet_now,
    ),
    ("meet_now_banner_open_chat_opens_the_chat", open_chat),
    ("own_meet_now_shows_no_banner", own_meet_now),
    ("meeting_s_end_closes_its_banner", end_closes),
    ("chat_header_offers_join_while_the_meet_now_runs_only", header_join),
    ("group_audio_call_shows_no_meeting_banner", group_call),
    ("one_to_one_call_shows_no_meeting_banner", one_to_one_call),
    ("channel_meet_now_shows_no_banner", channel_meet_now),
    ("meeting_not_on_the_calendar_shows_no_banner", unscheduled_meeting),
    (
        "meeting_on_the_calendar_shows_a_banner_join_opens_its_prejoin",
        scheduled_meeting,
    ),
    (
        "channel_meeting_on_the_calendar_shows_a_banner_body_opens_the_channel",
        channel_meeting,
    ),
    ("muted_channel_shows_no_banner", muted_channel),
    ("do_not_disturb_shows_no_banner", do_not_disturb),
    ("notifications_off_shows_no_banner", notifications_off),
    ("call_s_mute_notifications_shows_no_banner", muted_in_call),
    ("catch_up_after_a_reconnect_shows_no_banner_again", reconnect),
    ("meeting_started_switch_off_shows_no_banner", switch_off),
    ("meeting_started_setting_light_and_dark", setting_shots),
    ("meeting_s_end_after_join_leaves_nothing_running", tidy),
];

/// The mock's own setup: the meeting chats (b organizes them, a and c attend).
pub(super) fn seed(mock: &ChatServiceMock) -> SandboxResult {
    for (chat, topic) in [(MEETING_CHAT, SUBJECT), (UNSCHEDULED_CHAT, "Made-up unscheduled sync")] {
        if !mock.add_meeting_chat(chat, topic, &[LAB_B, LAB_A, LAB_C]) {
            return Err("no meeting chat made".into());
        }
    }
    Ok(())
}

/// b's Meet now in the group chat, started before the app's first run (the mock's history, older than any banner).
pub(super) fn seed_running(mock: &ChatServiceMock) -> SandboxResult {
    mock.add_message(
        ABC_CHAT,
        (LAB_B, "Lab B"),
        "Event/Call",
        &start(LAB_B, RUNNING_CALL, true),
    )
    .ok_or("no a-b-c chat")?;
    Ok(())
}

/// The data dir a restart run keeps, or `None` for another mode.
pub(super) fn kept_dir(mode: &str) -> SandboxResult<Option<PathBuf>> {
    if mode != RESTART_MODE {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(
        std::env::var_os(DATA_ENV).ok_or("no meeting data dir")?,
    )))
}

/// The mock's answers to the questions of these checks.
pub(super) fn mock_reply(verb: &str, rest: &str, probe: &Probe) -> Option<Value> {
    let mock = &*probe.mock;
    let mut words = rest.split(' ');
    let mut next = || words.next().unwrap_or("1").to_owned();
    Some(match verb {
        // `push-meet-now <a|b> <n>`: a's or b's Meet now in the group chat
        "push-meet-now" => {
            let who = if next() == "a" {
                (LAB_A, "Lab A")
            } else {
                (LAB_B, "Lab B")
            };
            let posted = posted(mock, ABC_CHAT, who, &start(who.0, &next(), true));
            if who.0 == LAB_A {
                echo_own_post(mock, &posted);
            }
            posted
        }
        "push-meet-now-end" => posted(mock, ABC_CHAT, (LAB_B, "Lab B"), &end(&next(), true)),
        // `push-start <place> <n>`, `push-end <place> <n>`: b's call in the place
        "push-start" | "push-end" => {
            let (place, call) = (next(), next());
            let (chat, huddle) = match place.as_str() {
                "group" => (ABC_CHAT, false),
                "one" => (AB_CHAT, false),
                "channel" => (CHANNEL, false),
                "meeting" => (MEETING_CHAT, false),
                "unscheduled" => (UNSCHEDULED_CHAT, false),
                _ => return Some(json!({"error": format!("no place {place}")})),
            };
            let content = if verb == "push-start" {
                start(LAB_B, &call, huddle)
            } else {
                end(&call, huddle)
            };
            posted(mock, chat, (LAB_B, "Lab B"), &content)
        }
        // `calendar`: the calendar's next reads hold the meeting chat's and the channel's meetings, now
        "calendar" => {
            let events = json!([meeting_event(), channel_event()]);
            let answer = json!({"type": "Microsoft.SkypeSpaces.MiddleTier.Models.CalendarEvent", "value": events});
            // mounted off the window's thread: a mount waits for the mock's held requests to end
            let held = Arc::clone(&probe.mock);
            probe.runtime.spawn(async move {
                for _ in 0..CALENDAR_READS {
                    held.answer_next("GET", "calendarView", 200, &answer).await;
                }
                MOUNTED.fetch_add(1, Ordering::SeqCst);
            });
            json!({"calendar": MOUNTED.load(Ordering::SeqCst)})
        }
        "calendar-mounted" => json!({"calendar": MOUNTED.load(Ordering::SeqCst)}),
        // `mute-channel <on|off>`: a's own channel notifications off, as Channel notifications → Off writes them
        "mute-channel" => {
            let value = if next() == "on" { "false" } else { "true" };
            let write = teams_proto::OwnTeamView::ChannelProperty {
                channel: CHANNEL.to_owned(),
                property: teams_proto::ChannelOwnProperty::Alerts,
                value: value.to_owned(),
            };
            if let Some(Err(error)) = probe.block(probe.transport.deliver(write)) {
                return Some(json!({"error": error.to_string()}));
            }
            // what the service holds now, as the banner's check reads it
            let read = teams_proto::ChannelRead::Preferences {
                channel: CHANNEL.to_owned(),
            };
            let held = probe
                .block(probe.transport.read(&read))
                .and_then(Result::ok)
                .and_then(|answer| serde_json::from_slice::<Value>(answer.body()).ok());
            json!({"alerts": held.map(|held| held["properties"]["alerts"].clone())})
        }
        // the push channel drops; the app connects again (and catches up)
        "drop-push" => {
            let before = probe.trouter.sockets().len();
            probe.trouter.drop_sockets(LAB_A);
            json!({"sockets": before})
        }
        "sockets" => json!({"sockets": probe.trouter.sockets().len()}),
        _ => return None,
    })
}

/// The window's answers to the questions of these checks.
pub(super) fn window_reply(verb: &str, rest: &str, shell: &Entity<Shell>, cx: &mut App) -> Option<Value> {
    let calls = shell.read(cx).models().calls.clone();
    let model = shell.read(cx).settings().clone();
    Some(match verb {
        // whether the group chat's header offers Join, and the pre-join open
        "meeting-calls" => {
            let calls = calls.read(cx);
            json!({
                "joinable": calls.joinable(ABC_CHAT),
                "prejoin": calls.prejoin().and_then(|prejoin| prejoin.to_join.chat().map(str::to_owned)),
                "prejoin_meeting": calls.prejoin().is_some_and(|prejoin| prejoin.to_join.meeting().is_some()),
            })
        }
        "cancel-prejoin" => {
            calls.update(cx, teams_ui::Calls::cancel_prejoin);
            json!({"cancelled": true})
        }
        // `notifications <off|all>`, as Settings → Notifications sets them
        "notifications" => {
            let off = rest == "off";
            model.update(cx, |model, cx| {
                model.change(
                    |settings| {
                        settings.notifications = if off {
                            teams_ui::settings::Notifications::Off
                        } else {
                            teams_ui::settings::Notifications::All
                        };
                    },
                    cx,
                );
            });
            json!({"off": off})
        }
        // `mute-in-call <on|off>`, as the call window's More → Settings → Mute notifications sets it
        "mute-in-call" => {
            let on = rest == "on";
            teams_ui::notify::mute_in_call(on, cx);
            json!({"muted": on})
        }
        _ => return None,
    })
}

fn posted(mock: &ChatServiceMock, chat: &str, from: (&str, &str), content: &str) -> Value {
    mock.add_message(chat, from, "Event/Call", content)
        .map_or_else(|| json!({"error": format!("no chat {chat}")}), |id| json!({"id": id}))
}

fn echo_own_post(mock: &ChatServiceMock, posted: &Value) {
    for event in mock.take_events(LAB_C) {
        if event.conversation == ABC_CHAT && event.resource["id"] == posted["id"] {
            mock.queue_event(LAB_A, event.clone());
        }
        mock.queue_event(LAB_C, event);
    }
}

/// A call's start by `starter`: a group chat's meeting (Meet now) when `huddle`, else a call.
fn start(starter: &str, call: &str, huddle: bool) -> String {
    let kind = if huddle {
        "<conversationType>\"huddleGroupCall\"</conversationType>"
    } else {
        ""
    };
    format!(
        "<partlist type=\"started\" alt=\"\"><part identity=\"{starter}\"><name>{starter}</name></part></partlist>\
         {kind}<callEventType>callStarted</callEventType>\
         <conversationUrl>{LINK}</conversationUrl><callId>made-up-meet-now-{call}</callId>"
    )
}

fn end(call: &str, huddle: bool) -> String {
    let kind = if huddle {
        "<conversationType>\"huddleGroupCall\"</conversationType>"
    } else {
        ""
    };
    format!(
        "<ended/><partlist alt=\"\" count=\"0\"></partlist>{kind}\
         <callEventType>callEnded</callEventType><conversationUrl>{LINK}</conversationUrl>\
         <callId>made-up-meet-now-{call}</callId>"
    )
}

/// The calendar's form of a time: `2026-11-13T10:00:00.0000000Z`.
fn iso(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%S.0000000Z").to_string()
}

/// A made-up Teams meeting from 5 minutes ago to 25 minutes from now, its join link on `thread` (a meeting's chat or a
/// channel, at `post`) organized by b.
fn event_now(id: &str, subject: &str, (thread, post): (&str, &str)) -> Value {
    let now = chrono::Utc::now();
    let link = format!(
        "https://teams.microsoft.com/l/meetup-join/{}/{post}?context=%7b%22Tid%22%3a%22{}%22%2c%22Oid%22%3a%22{}%22%7d",
        thread.replace(':', "%3a").replace('@', "%40"),
        teams_mock::TENANT,
        LAB_B.rsplit(':').next().unwrap_or_default(),
    );
    json!({
        "objectId": id, "subject": subject,
        "startTime": iso(now - chrono::Duration::minutes(5)), "endTime": iso(now + chrono::Duration::minutes(25)),
        "isAllDayEvent": false, "eventType": "Single", "isCancelled": false, "isOnlineMeeting": true,
        "isOrganizer": false, "myResponseType": "Accepted", "showAs": "Busy",
        "organizerName": "Lab B", "organizerAddress": "lab-b@lab.example.com",
        "location": "Microsoft Teams Meeting", "isPrivate": false,
        "attendees": [{"name": "Lab B", "address": "lab-b@lab.example.com"}, {"name": "Lab A", "address": "a@example.com"}],
        "skypeTeamsMeetingUrl": link,
    })
}

fn meeting_event() -> Value {
    let mut event = event_now("AAMkMadeUpStartedMeeting", SUBJECT, (MEETING_CHAT, "0"));
    event["skypeTeamsData"] = json!(json!({"cid": MEETING_CHAT, "type": 0}).to_string());
    event
}

fn channel_event() -> Value {
    let mut event = event_now("AAMkMadeUpChannelMeeting", CHANNEL_SUBJECT, (CHANNEL, CHANNEL_POST));
    event["skypeTeamsData"] = json!(json!({"cid": CHANNEL, "uid": LAB_B, "mid": CHANNEL_POST}).to_string());
    event["iCalUID"] = json!("040000008200e00074c5b7101a82e008000000000a0d000000000000000000000000001");
    event
}

/// The meeting banners shown since `seen` (those saying someone started one).
fn meeting_banners(qa: &Qa<'_>, seen: usize) -> Vec<ui_sandbox::bus::Notification> {
    qa.sandbox
        .bus()
        .notifications()
        .into_iter()
        .skip(seen)
        .filter(|banner| banner.body.contains(" started "))
        .collect()
}

/// Waits for the first meeting banner since `seen`.
fn banner_after(qa: &Qa<'_>, seen: usize, what: &str) -> SandboxResult<ui_sandbox::bus::Notification> {
    wait_until(BANNER_WAIT, || meeting_banners(qa, seen).first().cloned())
        .ok_or_else(|| format!("no banner for {what}").into())
}

/// Asks `question` (a start), then waits [`BANNER_WAIT`]: no meeting banner came.
fn none_for(qa: &Qa<'_>, question: &str, what: &str) -> SandboxResult {
    let seen = qa.sandbox.bus().notifications().len();
    let posted = qa.ask(question)?;
    ensure(posted.get("error").is_none(), || format!("{question}: {posted}"))?;
    thread::sleep(BANNER_WAIT);
    let banners = meeting_banners(qa, seen);
    ensure(banners.is_empty(), || format!("{what}: {banners:?}"))
}

fn offers(banner: &ui_sandbox::bus::Notification, id: &str, label: &str) -> bool {
    banner
        .actions
        .chunks(2)
        .any(|pair| pair.first().map(String::as_str) == Some(id) && pair.get(1).map(String::as_str) == Some(label))
}

fn closed(qa: &Qa<'_>, id: u32, what: &str) -> SandboxResult {
    wait_until(BANNER_WAIT, || {
        qa.sandbox.bus().closed_notifications().contains(&id).then_some(())
    })
    .ok_or_else(|| format!("the banner stayed {what}").into())
}

/// The calendar's next reads hold the made-up meetings, once the mock took the answer.
fn calendar_now(qa: &Qa<'_>) -> SandboxResult {
    let before = qa.ask("calendar")?["calendar"].as_u64().unwrap_or_default();
    wait_until(Duration::from_secs(40), || {
        let now = qa.ask("calendar-mounted").ok()?["calendar"].as_u64()?;
        if now > before {
            Some(())
        } else {
            thread::sleep(Duration::from_millis(200));
            None
        }
    })
    .ok_or_else(|| "the mock never took the calendar's answer".into())
}

fn meeting_calls(qa: &Qa<'_>) -> SandboxResult<Value> {
    qa.ask("meeting-calls")
}

fn joinable_until(qa: &Qa<'_>, wanted: bool) -> SandboxResult {
    let mut last = Value::Null;
    wait_until(BANNER_WAIT * 2, || match meeting_calls(qa) {
        Ok(calls) if calls["joinable"] == wanted => Some(()),
        Ok(calls) => {
            last = calls;
            thread::sleep(Duration::from_millis(200));
            None
        }
        Err(_) => None,
    })
    .ok_or_else(|| format!("the header's Join isn't {wanted}: {last}").into())
}

/// b's Meet now: a's banner, "<chat>", "Lab B started a meeting", with Join; Join asks to join the chat's call.
fn someone_else_s_meet_now(qa: &mut Qa<'_>) -> SandboxResult {
    let shown = qa.sandbox.bus().notifications().len();
    qa.trace.drain();
    qa.ask("push-meet-now b 1")?;
    let banner = banner_after(qa, shown, "b's Meet now in the group chat")?;
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
    ensure(
        offers(&banner, "join", "Join") && offers(&banner, "default", "Open chat"),
        || format!("the banner's actions: {:?}", banner.actions),
    )?;
    qa.shot("meeting-started-banner-shown")?;
    qa.sandbox.bus().invoke_action(banner.id, "join")?;
    let asked = qa.trace.next_where("a join asked in the group chat", |line| {
        line["view"] == "call" && line["change"] == "asked" && line.to_string().contains(ABC_CHAT)
    })?;
    say(&format!("the banner's Join: {asked}"));
    // the test engine has no calling: the refusal's banner pushes the panes down until it is closed
    thread::sleep(Duration::from_millis(700));
    qa.shot("meeting-started-joined")?;
    qa.click(BANNER_CLOSE)?;
    closed(qa, banner.id, "after its Join")?;
    qa.ask("push-meet-now-end 1")?;
    Ok(())
}

/// The banner's Open chat (its body's click) opens the group chat and closes the banner.
fn open_chat(qa: &mut Qa<'_>) -> SandboxResult {
    qa.chord(&[ui_sandbox::input::Modifier::Ctrl], '1')?;
    let shown = qa.sandbox.bus().notifications().len();
    qa.ask("push-meet-now b 5")?;
    let banner = banner_after(qa, shown, "b's Meet now")?;
    qa.sandbox.bus().invoke_action(banner.id, "default")?;
    qa.state_until("the group chat open", |state| state["chat"] == ABC_CHAT)?;
    qa.shot("meeting-started-open-chat")?;
    closed(qa, banner.id, "after Open chat")?;
    qa.ask("push-meet-now-end 5")?;
    Ok(())
}

/// a's own Meet now (another client of a's started it): no banner.
fn own_meet_now(qa: &mut Qa<'_>) -> SandboxResult {
    joinable_until(qa, false)?;
    let seen = qa.sandbox.bus().notifications().len();
    let posted = qa.ask("push-meet-now a 2")?;
    ensure(posted.get("error").is_none(), || format!("a's own Meet now: {posted}"))?;
    joinable_until(qa, true)?;
    thread::sleep(BANNER_WAIT);
    let banners = meeting_banners(qa, seen);
    qa.ask("push-meet-now-end 2")?;
    joinable_until(qa, false)?;
    ensure(banners.is_empty(), || format!("a's own Meet now: {banners:?}"))
}

/// A banner not acted on closes once the meeting ends.
fn end_closes(qa: &mut Qa<'_>) -> SandboxResult {
    let shown = qa.sandbox.bus().notifications().len();
    qa.ask("push-meet-now b 3")?;
    let banner = banner_after(qa, shown, "b's second Meet now")?;
    qa.shot("meeting-started-banner")?;
    qa.ask("push-meet-now-end 3")?;
    closed(qa, banner.id, "after the meeting ended")
}

/// The group chat's header offers Join while b's Meet now runs, and not once it ended.
fn header_join(qa: &mut Qa<'_>) -> SandboxResult {
    joinable_until(qa, false)?;
    qa.ctl_json(&["open", "chat", ABC_CHAT])?;
    qa.ask("push-meet-now b 6")?;
    joinable_until(qa, true)?;
    qa.shot("meeting-started-header-join")?;
    qa.ask("push-meet-now-end 6")?;
    joinable_until(qa, false)?;
    qa.shot("meeting-started-header-ended")?;
    Ok(())
}

/// b's group call (an "Audio call", no Meet now) in the group chat: no meeting banner (it rings instead,
/// `large_call_window`'s `meet-now` step).
fn group_call(qa: &mut Qa<'_>) -> SandboxResult {
    none_for(qa, "push-start group 7", "b's group call")?;
    qa.ask("push-end group 7")?;
    Ok(())
}

/// b's 1:1 call: no meeting banner.
fn one_to_one_call(qa: &mut Qa<'_>) -> SandboxResult {
    none_for(qa, "push-start one 8", "b's 1:1 call")?;
    qa.ask("push-end one 8")?;
    Ok(())
}

/// b's Meet now in the team's channel, no meeting on the calendar: no banner.
fn channel_meet_now(qa: &mut Qa<'_>) -> SandboxResult {
    none_for(qa, "push-start channel 9", "b's Meet now in the channel")?;
    qa.ask("push-end channel 9")?;
    Ok(())
}

/// A meeting chat's meeting the calendar doesn't hold: no banner.
fn unscheduled_meeting(qa: &mut Qa<'_>) -> SandboxResult {
    none_for(qa, "push-start unscheduled 10", "a meeting not on the calendar")?;
    qa.ask("push-end unscheduled 10")?;
    Ok(())
}

/// The calendar's meeting started by b: its subject, "Lab B started the meeting", Join and Open chat; Join opens its
/// pre-join on its chat.
fn scheduled_meeting(qa: &mut Qa<'_>) -> SandboxResult {
    calendar_now(qa)?;
    let shown = qa.sandbox.bus().notifications().len();
    qa.trace.drain();
    qa.ask("push-start meeting 11")?;
    let banner = banner_after(qa, shown, "b's start of the meeting on the calendar")?;
    say(&format!(
        "the meeting: {:?}: {:?}, actions {:?}",
        banner.summary, banner.body, banner.actions
    ));
    let checked = ensure(
        banner.summary == SUBJECT && banner.body == "Lab B started the meeting",
        || format!("the banner: {:?}: {:?}", banner.summary, banner.body),
    )
    .and_then(|()| {
        ensure(
            offers(&banner, "join", "Join") && offers(&banner, "default", "Open chat"),
            || format!("the banner's actions: {:?}", banner.actions),
        )
    });
    qa.shot("meeting-started-meeting-banner")?;
    qa.sandbox.bus().invoke_action(banner.id, "join")?;
    let mut last = Value::Null;
    let prejoin = wait_until(BANNER_WAIT * 2, || match meeting_calls(qa) {
        Ok(calls) if calls["prejoin"] == MEETING_CHAT => Some(calls),
        Ok(calls) => {
            last = calls;
            thread::sleep(Duration::from_millis(200));
            None
        }
        Err(_) => None,
    });
    qa.shot("meeting-started-meeting-prejoin")?;
    let closed_after = closed(qa, banner.id, "after its Join");
    qa.ask("cancel-prejoin")?;
    qa.ask("push-end meeting 11")?;
    checked?;
    prejoin.ok_or_else(|| format!("Join opened no pre-join of the meeting's chat: {last}"))?;
    closed_after
}

/// The channel's meeting on the calendar started by b: its subject, "Lab B started the meeting"; the body opens the
/// channel.
fn channel_meeting(qa: &mut Qa<'_>) -> SandboxResult {
    let shown = qa.sandbox.bus().notifications().len();
    qa.ask("push-start channel 12")?;
    let banner = banner_after(qa, shown, "b's start of the channel's meeting on the calendar")?;
    say(&format!(
        "the channel's meeting: {:?}: {:?}, actions {:?}",
        banner.summary, banner.body, banner.actions
    ));
    qa.shot("meeting-started-channel-banner")?;
    let checked = ensure(
        banner.summary == CHANNEL_SUBJECT
            && banner.body == "Lab B started the meeting"
            && banner.actions == ["default", "", "join", "Join"],
        || {
            format!(
                "the banner: {:?}: {:?} {:?}",
                banner.summary, banner.body, banner.actions
            )
        },
    );
    qa.sandbox.bus().invoke_action(banner.id, "default")?;
    let opened = qa.state_until("the channel open", |state| {
        state["main"] == "Channel" || state["chat"] == CHANNEL
    });
    qa.shot("meeting-started-channel-opened")?;
    qa.ask("push-end channel 12")?;
    checked?;
    opened?;
    closed(qa, banner.id, "after its body's click")
}

/// The channel muted (its notifications Off): its meeting on the calendar shows no banner.
fn muted_channel(qa: &mut Qa<'_>) -> SandboxResult {
    let muted = qa.ask("mute-channel on")?;
    ensure(muted["alerts"] == "false", || {
        format!("the channel wasn't muted: {muted}")
    })?;
    let none = none_for(qa, "push-start channel 13", "the muted channel's meeting");
    qa.ask("push-end channel 13")?;
    qa.ask("mute-channel off")?;
    none
}

/// a's status Do not disturb (`ngteamsctl status set dnd`, as the status menu sets it): no banner.
fn do_not_disturb(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ctl_json(&["status", "set", "dnd"])?;
    let none = none_for(qa, "push-meet-now b 14", "b's Meet now with Do not disturb");
    qa.ctl_json(&["status", "set", "reset"])?;
    qa.ask("push-meet-now-end 14")?;
    none
}

/// Settings → Notifications off: no banner.
fn notifications_off(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ask("notifications off")?;
    let none = none_for(qa, "push-meet-now b 15", "b's Meet now with notifications off");
    qa.ask("notifications all")?;
    qa.ask("push-meet-now-end 15")?;
    none
}

/// A call's Mute notifications on: no banner.
fn muted_in_call(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ask("mute-in-call on")?;
    let none = none_for(
        qa,
        "push-meet-now b 16",
        "b's Meet now with a call's Mute notifications",
    );
    qa.ask("mute-in-call off")?;
    qa.ask("push-meet-now-end 16")?;
    none
}

/// A banner shown; the push channel drops and comes back (a catch-up), then a catch-up asked for (`ngteamsctl sync`):
/// no banner again.
fn reconnect(qa: &mut Qa<'_>) -> SandboxResult {
    let shown = qa.sandbox.bus().notifications().len();
    qa.ask("push-meet-now b 17")?;
    banner_after(qa, shown, "b's Meet now before the reconnect")?;
    let before = qa.ask("drop-push")?["sockets"].as_u64().unwrap_or_default();
    wait_until(Duration::from_secs(20), || {
        let now = qa.ask("sockets").ok()?["sockets"].as_u64()?;
        (now > before).then_some(())
    })
    .ok_or("the push channel didn't connect again")?;
    thread::sleep(BANNER_WAIT);
    qa.ctl_json(&["sync"])?;
    thread::sleep(BANNER_WAIT);
    let banners = meeting_banners(qa, shown);
    qa.ask("push-meet-now-end 17")?;
    ensure(banners.len() == 1, || {
        format!(
            "{} banners for one Meet now across a reconnect: {banners:?}",
            banners.len()
        )
    })
}

/// Settings → Notifications → "Meeting started notification" off: b's Meet now shows no banner; on again after.
fn switch_off(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ctl_json(&["open", "settings", "notifications"])?;
    qa.shot("meeting-started-setting")?;
    qa.ask("meeting-started off")?;
    qa.shot("meeting-started-setting-off")?;
    let none = none_for(qa, "push-meet-now b 4", "b's Meet now with the switch off");
    qa.ask("meeting-started on")?;
    qa.ask("push-meet-now-end 4")?;
    none
}

/// The Notifications page with its Meetings box, in Teams Light and Dark.
fn setting_shots(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ctl_json(&["open", "settings", "notifications"])?;
    qa.shot("meeting-started-setting-light")?;
    qa.ask("theme dark")?;
    let dark = qa.shot("meeting-started-setting-dark");
    qa.ask("theme light")?;
    dark.map(|_| ())
}

/// Every call these checks started has ended: the header offers no Join, the window back on the chats.
fn tidy(qa: &mut Qa<'_>) -> SandboxResult {
    joinable_until(qa, false)?;
    qa.chord(&[ui_sandbox::input::Modifier::Ctrl], '1')?;
    Ok(())
}

/// The flows that passed, and those that failed with why.
type Tally = (Vec<&'static str>, Vec<(&'static str, String)>);

/// The app three times on one data dir, b's Meet now running from before the first (see the module docs).
pub(super) fn restart_runs(sandbox: &Sandbox) -> SandboxResult<Tally> {
    let data = tempfile::tempdir()?;
    let dir = data.path().to_string_lossy().into_owned();
    let runs: [&[Check]; 3] = [
        &[("old_running_meet_now_shows_no_banner_at_first_start", first_start)],
        &[(
            "running_meet_now_offers_join_after_a_restart_and_not_after_its_end",
            after_restart,
        )],
        &[("ended_meet_now_offers_no_join_after_the_next_restart", after_end)],
    ];
    let (mut passed, mut failed) = (Vec::new(), Vec::new());
    for checks in runs {
        remove_socket(sandbox)?;
        BEFORE_RUN.store(sandbox.bus().notifications().len(), Ordering::SeqCst);
        let mut qa = Qa::start_in(sandbox, RESTART_MODE, &[(DATA_ENV, &dir)])?;
        for (name, check) in checks {
            qa.check(name, *check);
        }
        passed.append(&mut qa.passed.clone());
        failed.append(&mut qa.failed.clone());
        qa.quit()?;
    }
    Ok((passed, failed))
}

fn remove_socket(sandbox: &Sandbox) -> io::Result<()> {
    match fs::remove_file(sandbox.path(CTL_SOCKET)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// The first start, b's Meet now running in the mock's history: no banner; the header's Join is said, not checked
/// (the store didn't hold the start when the app started).
fn first_start(qa: &mut Qa<'_>) -> SandboxResult {
    let seen = qa.sandbox.bus().notifications().len();
    thread::sleep(BANNER_WAIT);
    let banners = meeting_banners(qa, BEFORE_RUN.load(Ordering::SeqCst));
    say(&format!(
        "first start: the header's Join {}",
        meeting_calls(qa)?["joinable"]
    ));
    ensure(banners.is_empty(), || {
        format!("banners for an old Meet now ({seen} notifications): {banners:?}")
    })
}

/// The app started again: no banner, the header offers Join; b ends it: no Join.
fn after_restart(qa: &mut Qa<'_>) -> SandboxResult {
    thread::sleep(BANNER_WAIT);
    let banners = meeting_banners(qa, BEFORE_RUN.load(Ordering::SeqCst));
    ensure(banners.is_empty(), || format!("banners after the restart: {banners:?}"))?;
    joinable_until(qa, true)?;
    qa.ctl_json(&["open", "chat", ABC_CHAT])?;
    qa.shot("meeting-started-restart-join")?;
    qa.ask(&format!("push-meet-now-end {RUNNING_CALL}"))?;
    joinable_until(qa, false)
}

/// Started again after the end: no banner, no Join.
fn after_end(qa: &mut Qa<'_>) -> SandboxResult {
    thread::sleep(BANNER_WAIT);
    let banners = meeting_banners(qa, BEFORE_RUN.load(Ordering::SeqCst));
    ensure(banners.is_empty(), || format!("banners after the restart: {banners:?}"))?;
    thread::sleep(Duration::from_secs(2));
    let calls = meeting_calls(qa)?;
    qa.ctl_json(&["open", "chat", ABC_CHAT])?;
    qa.shot("meeting-started-restart-ended")?;
    ensure(calls["joinable"] == false, || {
        format!("the header offers Join after the end: {calls}")
    })
}
