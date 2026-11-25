//! A channel's running meeting, as Teams shows it: its post is a card with Join while the meeting runs and its ended
//! line after; the post's Join and the channel header's Join open the pre-join titled with the meeting's own title
//! (the post's `meetingtitle`), not the channel's name, and the join goes into the meeting of that post. Every id and
//! name is made up.

use std::sync::Arc;

use teams_store::{Applied, ChannelKind, Message, Team, TeamChannel};
use teams_ui::calls::MeetingToJoin;

use teams_core::{CallAction, Event, MessageChange, Source};

use super::{AB, LAB_B, Rig, actions, call_windows, calling, calls_of, click, drawn, prejoin_window, registered};
use super::{shell_window, trace};

const TEAM: &str = "19:MadeUpTeam0001@thread.tacv2";
const CHANNEL: &str = "19:MadeUpChannel0002@thread.tacv2";
const POST: &str = "1760000300000";
const END: &str = "1760000400000";
const CALL_ID: &str = "00000000-0000-4000-8000-0000000c0001";
/// The post of a second meeting that runs in the channel at the same time, and its call.
const OTHER_POST: &str = "1760000500000";
const OTHER_CALL_ID: &str = "00000000-0000-4000-8000-0000000c0002";
const ORGANIZER: &str = "00000000-0000-4000-8000-00000000000b";
const TENANT: &str = "00000000-0000-4000-8000-0000000000f1";
const TITLE: &str = "Sprint review «Q4» & demo";
const LINK: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/MadeUpMeetup?i=10-0-0-2&amp;e=1";

fn teams() -> Vec<Team> {
    let channel = |id: &str, name: &str, general: bool| TeamChannel {
        id: id.to_owned(),
        name: name.to_owned(),
        general,
        kind: ChannelKind::Standard,
        about: teams_store::ChannelAbout::default(),
        shown: true,
    };
    vec![Team {
        id: TEAM.to_owned(),
        name: "Made-up Team".to_owned(),
        archived: false,
        group_id: None,
        mailbox: None,
        owner: false,
        can_add_channel: false,
        can_add_private_channel: false,
        about: teams_store::TeamAbout::default(),
        channels: vec![channel(TEAM, "General", true), channel(CHANNEL, "Launch v2", false)],
    }]
}

/// A channel meeting's call event (`callStarted` or `callEnded`), in the shape the service posts it (seen 2026-11-03),
/// its post's `properties.meeting` titled `title`.
fn call_event(id: &str, event: &str, title: Option<&str>) -> Message {
    call_event_of(id, CALL_ID, event, title)
}

/// [`call_event`] of the call `call`.
fn call_event_of(id: &str, call: &str, event: &str, title: Option<&str>) -> Message {
    let arrival: i64 = id.parse().unwrap();
    let meeting = serde_json::json!({
        "@type": "http://schema.skype.com/ScheduledMeetingCreated", "meetingtitle": title,
        "organizerId": ORGANIZER, "tenantId": TENANT,
    });
    Message {
        conversation_id: CHANNEL.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(LAB_B.to_owned()),
        sender_name: Some("Lab Bee".to_owned()),
        message_type: "Event/Call".to_owned(),
        content: format!(
            "<partlist alt =\"\"></partlist><callEventType>{event}</callEventType>\
             <conversationUrl>{LINK}</conversationUrl><callId>{call}</callId>"
        ),
        properties: Some(serde_json::json!({ "meeting": meeting.to_string() }).to_string()),
        arrival_time: arrival,
        compose_time: Some(arrival),
        edit_time: None,
        delete_time: None,
    }
}

/// The channel holds `messages` (the newest first), each a post of its own or in [`POST`]'s chain.
fn channel_holds(rig: &Rig, messages: Vec<Message>) {
    *rig.core.teams.lock().unwrap() = Some(teams());
    let roots = messages
        .iter()
        .map(|message| (message.id.clone(), POST.to_owned()))
        .collect();
    rig.core.messages.lock().unwrap().insert(CHANNEL.to_owned(), messages);
    rig.core.roots.lock().unwrap().insert(CHANNEL.to_owned(), roots);
}

/// The pre-join of the meeting of [`POST`], titled `title`.
fn channel_meeting(title: &str) -> MeetingToJoin {
    channel_meeting_of(POST, title)
}

/// The pre-join of the meeting of the post `post`, titled `title`.
fn channel_meeting_of(post: &str, title: &str) -> MeetingToJoin {
    MeetingToJoin::Chat {
        chat: CHANNEL.to_owned(),
        meeting: teams_core::MeetingInfo {
            organizer_id: ORGANIZER.to_owned(),
            tenant_id: TENANT.to_owned(),
        },
        title: title.to_owned(),
        post: Some(post.to_owned()),
    }
}

fn prejoin_shown(rig: &Rig) -> Option<MeetingToJoin> {
    rig.cx
        .read(|cx| calls_of(rig).read(cx).prejoin().map(|prejoin| prejoin.to_join.clone()))
}

#[test]
fn a_running_channel_meetings_post_joins_it_by_its_pre_join_titled_as_the_meeting() {
    let rig = calling();
    channel_holds(&rig, vec![call_event(POST, "callStarted", Some(TITLE))]);
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    click(cx, "channel-row-1");
    assert!(
        drawn(cx, &format!("channel-meeting-call-{POST}")),
        "Teams' meeting card"
    );
    click(cx, &format!("channel-meeting-join-{POST}"));
    let _window = prejoin_window(&rig, &windows);
    assert_eq!(prejoin_shown(&rig), Some(channel_meeting(TITLE)));
    assert!(actions(&rig).is_empty(), "nothing goes out while it shows");
}

#[test]
fn an_ended_channel_meetings_post_offers_no_join() {
    let rig = calling();
    channel_holds(
        &rig,
        vec![
            call_event(END, "callEnded", None),
            call_event(POST, "callStarted", Some(TITLE)),
        ],
    );
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let _windows = call_windows(&rig, &shell, &trace);
    registered(&rig);
    click(cx, "channel-row-1");
    assert!(drawn(cx, &format!("event-{POST}")), "its ended line");
    assert!(!drawn(cx, &format!("channel-meeting-call-{POST}")));
    assert!(!drawn(cx, &format!("channel-meeting-join-{POST}")));
}

#[test]
fn the_channel_headers_join_titles_the_pre_join_with_the_meetings_own_title() {
    let rig = calling();
    channel_holds(&rig, vec![call_event(POST, "callStarted", Some(TITLE))]);
    rig.core
        .channel_meetups
        .lock()
        .unwrap()
        .insert(CHANNEL.to_owned(), meetup());
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    click(cx, "channel-row-1");
    click(cx, "channel-join-meeting");
    let window = prejoin_window(&rig, &windows);
    assert_eq!(prejoin_shown(&rig), Some(channel_meeting(TITLE)));
    click(window, "prejoin-join");
    assert_eq!(
        actions(&rig),
        vec![CallAction::JoinChannel {
            channel: CHANNEL.to_owned(),
            post: Some(POST.to_owned()),
        }],
        "the header's Join goes into the meeting that it read"
    );
}

/// What the engine reads for the channel's running meeting: the meeting of [`POST`].
fn meetup() -> teams_core::ChannelMeetup {
    teams_core::ChannelMeetup {
        message_id: POST.to_owned(),
        conversation: teams_proto::CallLink::parse(&LINK.replace("&amp;", "&")).unwrap(),
        meeting: teams_core::MeetingInfo {
            organizer_id: ORGANIZER.to_owned(),
            tenant_id: TENANT.to_owned(),
        },
    }
}

#[test]
fn the_channel_headers_join_goes_once_the_meetings_post_tells_its_end() {
    let rig = calling();
    channel_holds(&rig, vec![call_event(POST, "callStarted", Some(TITLE))]);
    rig.core
        .channel_meetups
        .lock()
        .unwrap()
        .insert(CHANNEL.to_owned(), meetup());
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let _windows = call_windows(&rig, &shell, &trace);
    registered(&rig);
    click(cx, "channel-row-1");
    assert!(drawn(cx, "channel-join-meeting"), "Join while the meeting runs");
    rig.core.channel_meetups.lock().unwrap().clear();
    meeting_ends(&rig);
    assert!(drawn(cx, &format!("event-{POST}")), "its ended line");
    assert!(
        !drawn(cx, "channel-join-meeting"),
        "no Join once the post tells the end, before the next timed read"
    );
}

/// The delivery of the post [`END`] that tells the meeting of [`POST`] ended, with the channel's posts then.
fn meeting_ends(rig: &Rig) {
    let ended = call_event(END, "callEnded", None);
    channel_holds(rig, vec![ended.clone(), call_event(POST, "callStarted", Some(TITLE))]);
    rig.send(Event::Message {
        conversation: CHANNEL.to_owned(),
        id: END.to_owned(),
        change: MessageChange::New,
        applied: Applied::Inserted,
        message: Some(Box::new(ended)),
        source: Source::Delivery,
    });
    rig.cx.run_until_parked();
}

#[test]
fn an_older_meeting_read_that_ends_last_does_not_put_the_headers_join_back() {
    let rig = calling();
    channel_holds(&rig, vec![call_event(POST, "callStarted", Some(TITLE))]);
    *rig.core.hold_channel_meetups.lock().unwrap() = true;
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let _windows = call_windows(&rig, &shell, &trace);
    registered(&rig);
    click(cx, "channel-row-1");
    meeting_ends(&rig);
    let mut held = std::mem::take(&mut *rig.core.held_channel_meetups.lock().unwrap());
    assert_eq!(held.len(), 2, "the open's read and the read after the end");
    held.remove(1).send(Ok(None));
    held.remove(0).send(Ok(Some(meetup())));
    rig.cx.run_until_parked();
    assert!(
        !drawn(cx, "channel-join-meeting"),
        "the read from before the end is older than the one applied"
    );
}

#[test]
fn opening_a_channel_with_a_running_meeting_reads_its_meeting_once() {
    let rig = calling();
    channel_holds(&rig, vec![call_event(POST, "callStarted", Some(TITLE))]);
    rig.core
        .channel_meetups
        .lock()
        .unwrap()
        .insert(CHANNEL.to_owned(), meetup());
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let _windows = call_windows(&rig, &shell, &trace);
    registered(&rig);
    click(cx, "channel-row-1");
    assert!(drawn(cx, "channel-join-meeting"));
    assert_eq!(*rig.core.channel_meetup_reads.lock().unwrap(), 1);
}

#[test]
fn opening_the_shown_channel_again_keeps_one_timed_meeting_read() {
    let rig = calling();
    channel_holds(&rig, vec![call_event(POST, "callStarted", Some(TITLE))]);
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let _windows = call_windows(&rig, &shell, &trace);
    registered(&rig);
    click(cx, "channel-row-1");
    click(cx, "channel-row-1");
    assert_eq!(*rig.core.channel_meetup_reads.lock().unwrap(), 2);
    rig.cx.executor().advance_clock(teams_ui::shell::CHANNEL_MEETUP_REFRESH);
    rig.cx.run_until_parked();
    assert_eq!(
        *rig.core.channel_meetup_reads.lock().unwrap(),
        3,
        "one timed read: the first open's loop stopped"
    );
}

#[test]
fn an_untitled_channel_meeting_is_titled_by_its_channel_as_teams_names_it() {
    let rig = calling();
    channel_holds(&rig, vec![call_event(POST, "callStarted", None)]);
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    click(cx, "channel-row-1");
    click(cx, &format!("channel-meeting-join-{POST}"));
    let _window = prejoin_window(&rig, &windows);
    assert_eq!(prejoin_shown(&rig), Some(channel_meeting("Meeting in \"Launch v2\"")));
}

#[test]
fn of_two_meetings_that_run_in_a_channel_each_posts_join_goes_into_its_own() {
    let rig = calling();
    channel_holds(
        &rig,
        vec![
            call_event_of(OTHER_POST, OTHER_CALL_ID, "callStarted", Some("Design sync")),
            call_event(POST, "callStarted", Some(TITLE)),
        ],
    );
    // each meeting's call is a post of its own
    rig.core.roots.lock().unwrap().insert(
        CHANNEL.to_owned(),
        [
            (OTHER_POST.to_owned(), OTHER_POST.to_owned()),
            (POST.to_owned(), POST.to_owned()),
        ]
        .into_iter()
        .collect(),
    );
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    click(cx, "channel-row-1");
    assert!(
        drawn(cx, &format!("channel-meeting-join-{OTHER_POST}")),
        "the newer meeting's Join"
    );
    click(cx, &format!("channel-meeting-join-{POST}"));
    let window = prejoin_window(&rig, &windows);
    assert_eq!(prejoin_shown(&rig), Some(channel_meeting_of(POST, TITLE)));
    click(window, "prejoin-join");
    assert_eq!(
        actions(&rig),
        vec![CallAction::JoinChannel {
            channel: CHANNEL.to_owned(),
            post: Some(POST.to_owned()),
        }],
        "the older meeting's post names it, not the newer meeting of the channel"
    );
}

/// Where the card of [`POST`] draws itself, its title and its Join, in a window `width` px wide.
fn card_at(width: f32) -> [gpui::Bounds<gpui::Pixels>; 3] {
    let rig = calling();
    channel_holds(&rig, vec![call_event(POST, "callStarted", None)]);
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let _windows = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(800.0)));
    click(cx, "channel-row-1");
    assert!(drawn(cx, &format!("channel-meeting-call-{POST}")));
    ["call", "title", "join"]
        .map(|part| format!("channel-meeting-{part}-{POST}"))
        .map(|selector| cx.debug_bounds(Box::leak(selector.into_boxed_str())).expect("drawn"))
}

/// The width that keeps 20 characters or more of a meeting's title readable, px.
const READABLE_TITLE: f32 = 160.0;
/// The card's padding on each side, px.
const CARD_PADDING: f32 = 16.0;

#[test]
fn a_meeting_cards_join_goes_below_the_title_before_the_title_is_cut_to_a_few_letters() {
    for width in (480_u16..=1280).step_by(40) {
        let [card, title, join] = card_at(f32::from(width));
        if join.top() < title.bottom() {
            assert!(
                title.size.width >= gpui::px(READABLE_TITLE),
                "at {width} px Join is beside a title of 20 characters or more: {title:?} {join:?}"
            );
        } else {
            assert!(
                title.right() >= card.right() - gpui::px(CARD_PADDING + 1.0),
                "at {width} px Join is below and the title takes its whole line: {card:?} {title:?}"
            );
        }
        if width >= 1280 {
            assert!(
                join.left() >= title.right(),
                "a wide window keeps Join on the title's right"
            );
        }
    }
}
