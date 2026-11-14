//! The shell, the chat list, the chat switcher and the settings view on GPUI's test context, over the fake engine
//! (`common`): the three panes and the list pane's resizing; the chats by last activity with their names, previews,
//! times and unread counts from the store, channels and own streams left out, kept up to date by the engine's events
//! (a catch-up's too); the connection state, and the list header's ("Read-only" in the read-only mode); Ctrl+K's
//! switcher, fuzzy, Enter opening, Escape closing; the selected chat kept in the settings and opened again; the
//! settings view changing the settings (the log's level among them); "Copy diagnostics" from the settings view and
//! from the palette, said to be unsafe to paste when it holds a `debug` line. Every id and name is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;

use std::sync::Arc;

use common::{FakeCore, LAB_A, LAB_B, Rig, Written};
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext, px};
use teams_core::sync::CatchUpReport;
use teams_core::{Cause, Event, MessageChange, Source};
use teams_log::diagnostics::{Source as DiagnosticsSource, WARNING};
use teams_media::devices::catalog::{DeviceDirectory as _, DeviceInfo, DeviceKind, DeviceList, FixedDirectory};
use teams_store::{Applied, Conversation, ConversationKind, Member, Message};
use teams_trouter::{Down, Stop};
use teams_ui::chat_list::{ChatList, DELETED, RowKind, UNKNOWN_USER, listed, one_to_one_partner, preview};
use teams_ui::clock::{self, HourCycle};
use teams_ui::local_ai::{AiChoice, Backend, Catalog, Listed, Model, ToolChoice, Tools};
use teams_ui::settings::{
    Copied, FirstRunState, LogLevel, ManualStatus, Notifications, Problem, Settings, SettingsModel, TimeFormat,
    attach_devices,
};
use teams_ui::settings_view::{Page, SettingsView};
use teams_ui::shell::{LIST_MAX, LIST_MIN, LIST_WIDTH, Main, Shell, ShellParts};
use teams_ui::switcher::{fuzzy_score, matches};
use teams_ui::view::{Shown, status_text};
use teams_ui::{Root, ViewTrace};

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const AC: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000c@unq.gbl.spaces";
const TRIO: &str = "19:0000000000000000000000000000trio@thread.v2";
const UNNAMED: &str = "19:00000000000000000000000000unnamed@thread.v2";
const CHANNEL: &str = "19:channel0001@thread.tacv2";
const NOTIFICATIONS: &str = "48:notifications";

fn chat(id: &str, kind: ConversationKind, topic: Option<&str>, last: i64) -> Conversation {
    Conversation {
        id: id.to_owned(),
        kind,
        topic: topic.map(str::to_owned),
        version: 1,
        last_activity: Some(last),
    }
}

fn message(conversation: &str, id: &str, sender: &str, content: &str, arrival: i64) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: "RichText/Html".to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: arrival,
        compose_time: Some(arrival),
        edit_time: None,
        delete_time: None,
    }
}

fn members(mris: &[&str]) -> Vec<Member> {
    mris.iter().map(|mri| Member::new(mri, None)).collect()
}

/// a's store: the a–b chat (b named, two unread, its newest text after a thread event), the a–c chat (nothing of c
/// stored but its id, a's own message last), the trio by its topic, an unnamed group (only b named), a team's channel
/// and a's notifications stream.
fn seed(core: &FakeCore) {
    core.conversations.lock().unwrap().extend([
        chat(AB, ConversationKind::OneToOne, None, 3_000),
        chat(AC, ConversationKind::OneToOne, None, 2_000),
        chat(TRIO, ConversationKind::Group, Some("Lab trio"), 4_000),
        chat(UNNAMED, ConversationKind::Group, Some("  "), 1_000),
        chat(CHANNEL, ConversationKind::Channel, Some("General"), 5_000),
        chat(
            NOTIFICATIONS,
            ConversationKind::Other("notifications".to_owned()),
            None,
            6_000,
        ),
    ]);
    let mut event = message(AB, "3001", LAB_B, "<addmember/>", 3_001);
    event.message_type = "ThreadActivity/AddMember".to_owned();
    let mut gone = message(UNNAMED, "999", LAB_B, "", 999);
    gone.delete_time = Some(1_000);
    let mut messages = core.messages.lock().unwrap();
    messages.insert(
        AB.to_owned(),
        vec![
            event,
            message(AB, "3000", LAB_B, "<p>hello <b>there</b></p><p>second line</p>", 3_000),
        ],
    );
    messages.insert(AC.to_owned(), vec![message(AC, "2000", LAB_A, "<p>mine</p>", 2_000)]);
    messages.insert(UNNAMED.to_owned(), vec![gone]);
    drop(messages);
    let mut stored = core.members.lock().unwrap();
    stored.insert(AB.to_owned(), members(&[LAB_A, LAB_B]));
    stored.insert(TRIO.to_owned(), members(&[LAB_A, LAB_B, LAB_C]));
    stored.insert(UNNAMED.to_owned(), members(&[LAB_A, LAB_B, LAB_C]));
    drop(stored);
    core.unread.lock().unwrap().insert(AB.to_owned(), 2);
    core.names
        .lock()
        .unwrap()
        .insert(LAB_B.to_owned(), "Lab Bee".to_owned());
}

fn trace() -> (Arc<ViewTrace>, Written) {
    let written = Written::default();
    (Arc::new(ViewTrace::new(written.clone())), written)
}

/// The rig with a's store seeded, and the kit set up.
fn seeded() -> Rig {
    let rig = Rig::start();
    seed(&rig.core);
    rig.cx.update(teams_ui::window::init);
    rig
}

fn parts(rig: &Rig, settings: Settings, trace: Arc<ViewTrace>) -> ShellParts {
    parts_with(rig, SettingsModel::in_memory(settings), trace)
}

fn parts_with(rig: &Rig, settings: SettingsModel, trace: Arc<ViewTrace>) -> ShellParts {
    // the views' request side, as the app hands it: the bridge's
    ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig.cx.update(|cx| cx.new(|_| settings)),
        trace,
    }
}

/// The shell in a window of its own.
fn shell_window(
    rig: &Rig,
    settings: Settings,
    trace: Arc<ViewTrace>,
) -> (Entity<Shell>, &'static mut VisualTestContext) {
    shell_window_with(rig, SettingsModel::in_memory(settings), trace)
}

/// The shell in a window of its own, over the settings model `settings`.
fn shell_window_with(
    rig: &Rig,
    settings: SettingsModel,
    trace: Arc<ViewTrace>,
) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let parts = parts_with(rig, settings, trace);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
    (shell, cx)
}

fn list_of(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<ChatList> {
    cx.update(|_, cx| shell.read(cx).list().clone())
}

fn ids(list: &Entity<ChatList>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.update(|_, cx| list.read(cx).rows().iter().map(|row| row.id.clone()).collect())
}

/// Draws the window again and gives the bounds of a selector.
fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<gpui::Bounds<gpui::Pixels>> {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    let found = bounds(cx, selector).unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
}

#[test]
fn the_list_shows_the_chats_by_last_activity_with_names_previews_and_unread_counts() {
    let rig = seeded();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Settings::default(), trace);
    let list = list_of(&shell, cx);

    let rows = cx.update(|_, cx| list.read(cx).rows().to_vec());
    let shown: Vec<(&str, RowKind, &str, bool, &str, u64)> = rows
        .iter()
        .map(|row| {
            (
                row.id.as_str(),
                row.kind,
                row.name.as_ref(),
                row.named,
                row.preview.as_ref(),
                row.unread,
            )
        })
        .collect();
    // a group without a topic by its members' first names (c, unnamed, as "Unknown user", never by MRI)
    let unnamed = format!("Lab and {UNKNOWN_USER}");
    assert_eq!(
        shown,
        [
            (TRIO, RowKind::Group, "Lab trio", true, "", 0),
            (AB, RowKind::OneToOne, "Lab Bee", true, "hello there", 2),
            (AC, RowKind::OneToOne, UNKNOWN_USER, false, "You: mine", 0),
            (UNNAMED, RowKind::Group, unnamed.as_str(), false, DELETED, 0),
        ]
    );
    assert!(rows.iter().all(|row| row.last_activity.is_some()));

    let lines = written.lines();
    let line = lines
        .iter()
        .rev()
        .find(|line| line["view"] == "chat-list")
        .expect("traced");
    assert_eq!(line["conversations"], serde_json::json!([TRIO, AB, AC, UNNAMED]));
    assert_eq!(line["unread"], serde_json::json!([0, 2, 0, 0]));
    assert_eq!(
        line["kinds"],
        serde_json::json!(["group", "one-to-one", "one-to-one", "group"])
    );
    let all = written.text();
    for text in ["Lab Bee", "Lab trio", "hello", "mine"] {
        assert!(!all.contains(text), "{text} reached the trace");
    }
}

#[test]
fn channels_and_the_account_s_own_streams_aren_t_chats() {
    assert!(!listed(&chat(CHANNEL, ConversationKind::Channel, None, 1)));
    assert!(!listed(&chat(
        NOTIFICATIONS,
        ConversationKind::Other("x".to_owned()),
        None,
        1
    )));
    assert!(!listed(&chat("48:notes", ConversationKind::Group, None, 1)));
    assert!(listed(&chat(AB, ConversationKind::OneToOne, None, 1)));
    assert!(listed(&chat(TRIO, ConversationKind::Meeting, None, 1)));
    assert_eq!(one_to_one_partner(AB, LAB_A).as_deref(), Some(LAB_B));
    assert_eq!(one_to_one_partner(AB, LAB_B).as_deref(), Some(LAB_A));
    assert_eq!(one_to_one_partner(AB, LAB_C), None, "not a's own chat");
    assert_eq!(one_to_one_partner(TRIO, LAB_A), None);
}

#[test]
fn a_row_s_time_is_the_time_of_day_today_then_the_weekday_then_the_day() {
    use chrono::TimeZone as _;
    let zone = chrono::FixedOffset::east_opt(3 * 3600).expect("offset");
    // Wednesday 21 October 2026, 15:00 local
    let now = zone.with_ymd_and_hms(2026, 10, 21, 15, 0, 0).single().expect("now");
    let at = |y, mo, d, h, mi| {
        zone.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("time")
            .timestamp_millis()
    };
    let format_time =
        |ms, now: &chrono::DateTime<chrono::FixedOffset>| clock::list_time(ms, now, HourCycle::TwentyFour);
    assert_eq!(format_time(at(2026, 10, 21, 9, 5), &now), "09:05");
    assert_eq!(format_time(at(2026, 10, 19, 9, 5), &now), "Mon");
    assert_eq!(format_time(at(2026, 10, 1, 9, 5), &now), "1 Oct");
    assert_eq!(format_time(at(2025, 12, 31, 9, 5), &now), "31 Dec 2025");
}

#[test]
fn the_list_follows_the_engine_s_events_and_builds_again_only_what_changed() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let list = list_of(&shell, cx);
    let members_asked = |rig: &Rig| rig.core.asked().iter().filter(|name| **name == "members").count();
    let before = members_asked(&rig);
    assert_eq!(before, 4, "one row built per chat");

    // a new message in the a–c chat: it moves to the top with its unread count; nothing else is built again
    {
        let mut stored = rig.core.conversations.lock().unwrap();
        stored.iter_mut().find(|chat| chat.id == AC).expect("ac").last_activity = Some(9_000);
    }
    rig.core
        .messages
        .lock()
        .unwrap()
        .get_mut(AC)
        .expect("ac")
        .insert(0, message(AC, "9000", LAB_C, "<p>new from c</p>", 9_000));
    rig.core.unread.lock().unwrap().insert(AC.to_owned(), 1);
    rig.send(Event::Message {
        conversation: AC.to_owned(),
        id: "9000".to_owned(),
        change: MessageChange::New,
        applied: Applied::Inserted,
        message: None,
        source: Source::Delivery,
    });
    cx.run_until_parked();
    assert_eq!(ids(&list, cx), [AC, TRIO, AB, UNNAMED]);
    let ac = cx.update(|_, cx| list.read(cx).row(AC).cloned()).expect("row");
    assert_eq!((ac.preview.as_ref(), ac.unread), ("new from c", 1));
    assert_eq!(members_asked(&rig), before + 1);

    // a catch-up may bring names from any chat: every row is built again
    rig.core
        .names
        .lock()
        .unwrap()
        .insert(LAB_C.to_owned(), "Lab Sea".to_owned());
    rig.send(Event::CaughtUp {
        cause: Cause::Connected,
        report: CatchUpReport::default(),
    });
    cx.run_until_parked();
    assert_eq!(members_asked(&rig), before + 1 + 4);
    let ac = cx.update(|_, cx| list.read(cx).row(AC).cloned()).expect("row");
    assert_eq!((ac.name.as_ref(), ac.named), ("Lab Sea", true));
}

#[test]
fn a_name_delivered_in_one_chat_renames_every_row_named_by_that_person() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let list = list_of(&shell, cx);
    let members_asked = |rig: &Rig| rig.core.asked().iter().filter(|name| **name == "members").count();
    let before = members_asked(&rig);
    let ac = cx.update(|_, cx| list.read(cx).row(AC).cloned()).expect("row");
    assert_eq!((ac.name.as_ref(), ac.named), (UNKNOWN_USER, false));

    // c's first named message comes live in the trio: the store now names c
    rig.core
        .names
        .lock()
        .unwrap()
        .insert(LAB_C.to_owned(), "Lab Sea".to_owned());
    let mut named = message(TRIO, "9100", LAB_C, "<p>hi all</p>", 9_100);
    named.sender_name = Some("Lab Sea".to_owned());
    rig.send(Event::Message {
        conversation: TRIO.to_owned(),
        id: "9100".to_owned(),
        change: MessageChange::New,
        applied: Applied::Inserted,
        message: Some(Box::new(named)),
        source: Source::Delivery,
    });
    cx.run_until_parked();

    // the trio (its own change), and the two rows named by c; the a–b row, named by b alone, is kept
    assert_eq!(members_asked(&rig), before + 3);
    let ac = cx.update(|_, cx| list.read(cx).row(AC).cloned()).expect("row");
    assert_eq!((ac.name.as_ref(), ac.named), ("Lab Sea", true));
    let unnamed = cx.update(|_, cx| list.read(cx).row(UNNAMED).cloned()).expect("row");
    assert_eq!((unnamed.name.as_ref(), unnamed.named), ("Lab and Lab", true));
}

#[test]
fn a_text_message_s_preview_is_its_text_as_it_came() {
    let mut plain = message(AB, "1", LAB_B, "Use <name> here &amp; there\nsecond line", 1);
    plain.message_type = "Text".to_owned();
    assert_eq!(preview(&[plain], LAB_A).as_ref(), "Use <name> here &amp; there");
    let rich = message(AB, "2", LAB_B, "<p>a &amp; <b>b</b></p>", 2);
    assert_eq!(preview(&[rich], LAB_A).as_ref(), "a & b");
}

#[test]
fn a_chat_whose_id_the_trace_refuses_leaves_the_trace_whole_rows_in_step() {
    const ODD: &str = "19:odd.thread@thread.v2";
    let rig = seeded();
    rig.core
        .conversations
        .lock()
        .unwrap()
        .push(chat(ODD, ConversationKind::Meeting, Some("Odd"), 3_500));
    rig.core.unread.lock().unwrap().insert(ODD.to_owned(), 5);
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Settings::default(), trace);
    let list = list_of(&shell, cx);
    assert_eq!(ids(&list, cx), [TRIO, ODD, AB, AC, UNNAMED], "the list shows it");

    let lines = written.lines();
    let line = lines
        .iter()
        .rev()
        .find(|line| line["view"] == "chat-list")
        .expect("traced");
    assert_eq!(line["conversations"], serde_json::json!([TRIO, AB, AC, UNNAMED]));
    assert_eq!(
        line["kinds"],
        serde_json::json!(["group", "one-to-one", "one-to-one", "group"])
    );
    assert_eq!(line["unread"], serde_json::json!([0, 2, 0, 0]));
    assert_eq!(line["refused"], 1);
}

#[test]
fn a_stopped_engine_leaves_the_rows_shown() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let list = list_of(&shell, cx);
    let loads = cx.update(|_, cx| list.read(cx).loads());
    *rig.core.stopped.lock().unwrap() = true;
    rig.send(Event::ConversationChanged {
        conversation: AB.to_owned(),
    });
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| list.read(cx).loads()), loads + 1, "it asked again");
    assert_eq!(ids(&list, cx), [TRIO, AB, AC, UNNAMED]);
}

#[test]
fn the_engine_shut_down_leaves_the_rows_shown_and_the_list_offline() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let list = list_of(&shell, cx);
    rig.cx
        .update(|cx| rig.bridge.update(cx, teams_ui::Bridge::shutdown).detach());
    // offline as the shutdown begins, before the engine's thread is joined (one held up stays shown offline)
    assert_eq!(rig.shutdowns(), 0);
    assert_eq!(
        rig.cx.read(|cx| status_text(rig.models.status.read(cx))).as_ref(),
        "Offline"
    );
    rig.cx.run_until_parked();
    assert_eq!(rig.shutdowns(), 1);
    assert_eq!(ids(&list, cx), [TRIO, AB, AC, UNNAMED]);
    assert_eq!(
        rig.cx.read(|cx| status_text(rig.models.status.read(cx))).as_ref(),
        "Offline"
    );
}

#[test]
fn the_connection_state_follows_the_push_channel() {
    let rig = Rig::start();
    let text = |rig: &Rig| rig.cx.read(|cx| status_text(rig.models.status.read(cx)).to_string());
    assert_eq!(text(&rig), "Connecting…");
    rig.send(Event::Connected {
        endpoint: "00000000-0000-4000-8000-0000000000e1".to_owned(),
    });
    assert_eq!(text(&rig), "Connected");
    rig.send(Event::Disconnected {
        down: Down::Closed,
        heard: None,
    });
    assert_eq!(text(&rig), "Reconnecting…");
    rig.send(Event::PushStopped(Stop::Requested));
    assert_eq!(text(&rig), "Offline");
    assert_eq!(rig.cx.read(|cx| Shown::of(rig.models.status.read(cx))), Shown::Offline);
}

#[test]
fn the_list_header_shows_the_connection_only_while_it_is_not_up() {
    let rig = seeded();
    let (_, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    assert!(bounds(cx, "connection").is_some(), "connecting");
    assert!(bounds(cx, "read-only").is_none());
    rig.send(Event::Connected {
        endpoint: "00000000-0000-4000-8000-0000000000e1".to_owned(),
    });
    assert!(bounds(cx, "connection").is_none(), "a healthy connection says nothing");
    rig.send(Event::Disconnected {
        down: Down::Closed,
        heard: None,
    });
    assert!(bounds(cx, "connection").is_some(), "reconnecting");
    rig.send(Event::PushStopped(Stop::Requested));
    assert!(bounds(cx, "connection").is_some(), "offline");
    assert!(bounds(cx, "read-only").is_none());
}

#[test]
fn in_the_read_only_mode_the_list_header_says_read_only_never_connecting() {
    let rig = seeded();
    *rig.core.read_only.lock().unwrap() = true;
    let (_, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    // no push channel ever comes up: the status stays "starting", which isn't shown as a connection on its way
    assert_eq!(
        rig.cx.read(|cx| Shown::of(rig.models.status.read(cx))),
        Shown::Connecting
    );
    assert!(bounds(cx, "read-only").is_some(), "the quiet read-only state");
    assert!(bounds(cx, "connection").is_none(), "no connecting pill");
    // the engine stopped: that is still told
    rig.cx
        .update(|cx| rig.bridge.update(cx, teams_ui::Bridge::shutdown).detach());
    rig.cx.run_until_parked();
    assert!(bounds(cx, "connection").is_some(), "offline");
    assert!(bounds(cx, "read-only").is_none());
}

#[test]
fn the_shell_has_three_panes_and_the_list_pane_resizes_within_its_range() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let rail = bounds(cx, "rail").expect("the rail");
    let list = bounds(cx, "list-pane").expect("the list pane");
    let main = bounds(cx, "conversation-pane").expect("the conversation pane");
    assert!(
        rail.right() <= list.left() && list.right() <= main.left(),
        "{rail:?} {list:?} {main:?}"
    );
    assert_eq!(list.size.width, px(LIST_WIDTH));
    assert!(main.size.width > px(0.0));

    let panes = cx.update(|_, cx| shell.read(cx).panes().clone());
    for (asked, got) in [(400.0, 400.0), (100.0, LIST_MIN), (2_000.0, LIST_MAX)] {
        cx.update(|window, cx| panes.update(cx, |panes, cx| panes.resize_panel(0, px(asked), window, cx)));
        let list = bounds(cx, "list-pane").expect("the list pane");
        assert_eq!(list.size.width, px(got), "asked {asked}");
        let main = bounds(cx, "conversation-pane").expect("the conversation pane");
        assert!(list.right() <= main.left());
    }
}

#[test]
fn the_sidebar_shows_the_chats_and_the_teams_and_the_rail_s_chat_leaves_the_settings() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    for selector in ["section-chats", "chat-row-0", "section-teams", "teams-note"] {
        assert!(bounds(cx, selector).is_some(), "{selector} is drawn");
    }
    assert!(bounds(cx, "rail-teams").is_none(), "no rail of the teams' own");
    // the chats folded: the teams stay
    click(cx, "section-chats");
    assert!(bounds(cx, "chat-row-0").is_none());
    assert!(bounds(cx, "section-teams").is_some());
    click(cx, "section-chats");
    assert!(bounds(cx, "chat-row-0").is_some());

    // the settings hide the chat; the rail's Chat shows it again
    click(cx, "chat-row-1");
    click(cx, "rail-settings");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Settings);
    assert!(bounds(cx, "settings").is_some());
    click(cx, "rail-chats");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(AB.to_owned())
    );
}

#[test]
fn a_chat_clicked_opens_and_is_kept_as_the_selected_chat() {
    let rig = seeded();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Settings::default(), trace);
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Nothing);
    click(cx, "chat-row-1");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(AB.to_owned())
    );
    let list = list_of(&shell, cx);
    assert_eq!(
        cx.update(|_, cx| list.read(cx).selected().map(str::to_owned)),
        Some(AB.to_owned())
    );
    assert_eq!(settings_of(&shell, cx).selected_chat.as_deref(), Some(AB));
    let opened = written
        .lines()
        .into_iter()
        .find(|line| line["view"] == "chat" && line["change"] == "opened")
        .expect("traced");
    assert_eq!(opened["conversation"], AB);
    assert!(bounds(cx, "chat-header").is_some());
}

#[test]
fn the_selected_chat_is_opened_again_at_the_start() {
    let rig = seeded();
    let settings = Settings {
        selected_chat: Some(AC.to_owned()),
        ..Settings::default()
    };
    let (shell, cx) = shell_window(&rig, settings, Arc::new(ViewTrace::off()));
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(AC.to_owned())
    );
    let list = list_of(&shell, cx);
    assert_eq!(
        cx.update(|_, cx| list.read(cx).selected().map(str::to_owned)),
        Some(AC.to_owned())
    );
}

#[test]
fn ctrl_k_s_switcher_matches_fuzzily_and_enter_opens_the_best_match() {
    let rig = seeded();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Settings::default(), trace);
    cx.simulate_keystrokes("ctrl-k");
    cx.run_until_parked();
    let switcher = cx.update(|_, cx| shell.read(cx).switcher().cloned()).expect("open");
    assert_eq!(
        cx.update(|_, cx| switcher.read(cx).matches().len()),
        4,
        "every chat at first"
    );

    cx.simulate_input("lbe");
    cx.run_until_parked();
    let names: Vec<String> = cx.update(|_, cx| {
        switcher
            .read(cx)
            .matches()
            .iter()
            .map(|(_, name)| name.to_string())
            .collect()
    });
    assert_eq!(names.first().map(String::as_str), Some("Lab Bee"), "{names:?}");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(AB.to_owned())
    );
    assert!(
        cx.update(|_, cx| shell.read(cx).switcher().is_none()),
        "closed once a chat opened"
    );
    let changes: Vec<String> = written
        .lines()
        .iter()
        .filter(|line| line["view"] == "switcher" || line["view"] == "chat")
        .map(|line| format!("{} {}", line["view"], line["change"]))
        .collect();
    assert_eq!(
        changes,
        [r#""switcher" "opened""#, r#""chat" "opened""#, r#""switcher" "closed""#]
    );
}

#[test]
fn escape_closes_the_switcher_whatever_was_typed_and_ctrl_k_toggles_it() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("trio");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| shell.read(cx).switcher().is_some()));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| shell.read(cx).switcher().is_none()));
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Nothing);

    cx.simulate_keystrokes("ctrl-k");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| shell.read(cx).switcher().is_some()));
    cx.simulate_keystrokes("ctrl-k");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| shell.read(cx).switcher().is_none()));
}

#[test]
fn a_fuzzy_match_takes_the_query_s_characters_in_order_and_ranks_word_starts_and_runs() {
    assert_eq!(fuzzy_score("", "anything"), Some(0));
    assert!(fuzzy_score("lt", "Lab trio").is_some());
    assert!(
        fuzzy_score("LAB TRIO", "lab trio").is_some(),
        "any case, spaces left out"
    );
    assert_eq!(fuzzy_score("oil", "Lab trio"), None, "out of order");
    assert_eq!(fuzzy_score("x", "Lab trio"), None);
    assert!(
        fuzzy_score("tri", "Lab trio") > fuzzy_score("tri", "a t r i"),
        "a run counts more"
    );
    assert!(
        fuzzy_score("b", "Lab Bee") > fuzzy_score("b", "Lab"),
        "a word's start counts more"
    );

    let rows = vec![
        ("1".to_owned(), "Abacus".into()),
        ("2".to_owned(), "Bob".into()),
        ("3".to_owned(), "Rob B".into()),
        ("4".to_owned(), "Zed".into()),
    ];
    let found: Vec<String> = matches("b", &rows).into_iter().map(|(id, _)| id).collect();
    // word starts first (Bob, then Rob B in the list's order), then the rest in the list's order
    assert_eq!(found, ["2", "3", "1"]);
}

#[test]
fn the_settings_view_changes_the_settings() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    cx.update(|_, cx| shell.update(cx, Shell::show_settings));
    // each page from the settings' own column; a picker's row once its menu is open
    for selector in [
        "settings-page-privacy",
        "send-typing",
        "send-read-receipts",
        "settings-page-general",
        "send-presence",
        "idle",
        "idle-15",
        "status",
        "status-do-not-disturb",
        "time-format-12-hour",
        "settings-page-notifications",
        "notifications-mentions",
        "settings-page-diagnostics",
        "log-level-debug",
    ] {
        click(cx, selector);
    }
    let settings = settings_of(&shell, cx);
    assert_eq!(
        settings,
        Settings {
            send_typing: false,
            send_read_receipts: false,
            send_presence: false,
            notifications: Notifications::Mentions,
            idle_minutes: 15,
            manual_status: ManualStatus::DoNotDisturb,
            log_level: LogLevel::Debug,
            time_format: TimeFormat::Twelve,
            ..Settings::default()
        }
    );
}

#[test]
fn the_title_bar_menus_open_each_place_and_change_zoom_without_losing_the_chat() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    click(cx, "chat-row-1");
    let chat = cx.update(|_, cx| shell.read(cx).main().clone());
    click(cx, "title-launcher");
    cx.simulate_keystrokes("down down down enter");
    let _ = bounds(cx, "title-launcher");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Calendar);
    click(cx, "title-launcher");
    cx.simulate_keystrokes("down down down down enter");
    let _ = bounds(cx, "title-launcher");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Settings);
    click(cx, "title-launcher");
    cx.simulate_keystrokes("down down enter");
    let _ = bounds(cx, "title-launcher");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), chat);
    let initial = settings_of(&shell, cx).appearance.font_size;
    for (keys, expected) in [
        ("down down enter", initial + 1),
        ("down down down enter", initial),
        ("down down down down enter", teams_ui::theme::FONT_SIZE),
    ] {
        click(cx, "title-more");
        cx.simulate_keystrokes(keys);
        let _ = bounds(cx, "title-more");
        assert_eq!(settings_of(&shell, cx).appearance.font_size, expected);
        assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), chat);
    }
    click(cx, "title-more");
    cx.simulate_keystrokes("down enter");
    let _ = bounds(cx, "title-more");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Settings);
    click(cx, "title-back");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), chat);
    click(cx, "title-forward");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Settings);
}

#[test]
fn the_calls_settings_keep_video_effects_without_starting_a_camera_and_drop_the_picker_when_hidden() {
    let rig = seeded();
    let model = SettingsModel::in_memory(Settings::default()).with_video_switch();
    let (shell, cx) = shell_window_with(&rig, model, Arc::new(ViewTrace::off()));
    click(cx, "rail-settings");
    click(cx, "settings-page-calls");
    assert!(bounds(cx, "video-effects").is_some());
    let settings = cx.update(|_, cx| shell.read(cx).settings_view().clone());
    assert!(cx.update(|_, cx| settings.read(cx).effects_picker().is_some()));
    click(cx, "video-calls");
    assert!(!settings_of(&shell, cx).video_calls);
    click(cx, "settings-page-privacy");
    assert!(cx.update(|_, cx| settings.read(cx).effects_picker().is_none()));
    click(cx, "settings-page-calls");
    assert!(cx.update(|_, cx| settings.read(cx).effects_picker().is_some()));
    click(cx, "rail-chats");
    assert!(cx.update(|_, cx| settings.read(cx).effects_picker().is_none()));
    assert!(rig.core.actions.lock().unwrap().is_empty());
}

#[test]
fn the_font_picker_can_replace_a_missing_family_with_the_system_font() {
    let rig = seeded();
    let mut settings = Settings::default();
    settings.appearance.font_family = Some("Synthetic missing family".to_owned());
    let (shell, cx) = shell_window(&rig, settings, Arc::new(ViewTrace::off()));
    cx.simulate_resize(gpui::size(px(1400.), px(1400.)));
    click(cx, "rail-settings");
    click(cx, "settings-page-appearance");
    click(cx, "font-family");
    cx.simulate_keystrokes("up enter");
    let _ = bounds(cx, "font-family");
    assert_eq!(settings_of(&shell, cx).appearance.font_family, None);
}

#[test]
fn the_settings_view_chooses_the_theme_and_the_font_size_and_the_zoom_keys_step_it() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    cx.update(|_, cx| shell.update(cx, Shell::show_settings));
    click(cx, "settings-page-appearance");
    // following the system: a theme for each of its modes
    click(cx, "dark-theme-nord");
    assert_eq!(settings_of(&shell, cx).appearance.dark_theme, "nord");
    assert!(bounds(cx, "theme-dracula").is_none(), "no single theme while following");
    click(cx, "follow-system-theme");
    click(cx, "theme-dracula");
    click(cx, "font-larger");
    click(cx, "font-larger");
    click(cx, "font-smaller");
    let appearance = settings_of(&shell, cx).appearance;
    assert!(!appearance.follow_system);
    assert_eq!(appearance.theme, "dracula");
    assert_eq!(appearance.font_size, teams_ui::theme::FONT_SIZE + 1);
    assert!(
        bounds(cx, "light-theme-nord").is_none(),
        "no sides while one theme is chosen"
    );
    assert!(bounds(cx, "font-family").is_some(), "the font picker is drawn");

    cx.simulate_keystrokes("ctrl-=");
    cx.simulate_keystrokes("ctrl-=");
    assert_eq!(
        settings_of(&shell, cx).appearance.font_size,
        teams_ui::theme::FONT_SIZE + 3
    );
    cx.simulate_keystrokes("ctrl--");
    assert_eq!(
        settings_of(&shell, cx).appearance.font_size,
        teams_ui::theme::FONT_SIZE + 2
    );
    cx.simulate_keystrokes("ctrl-0");
    assert_eq!(settings_of(&shell, cx).appearance.font_size, teams_ui::theme::FONT_SIZE);
}

#[test]
fn the_settings_pages_are_reached_by_the_keyboard() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    cx.update(|_, cx| shell.update(cx, Shell::show_settings));
    let view = cx.update(|_, cx| shell.read(cx).settings_view().clone());
    cx.update(|window, cx| view.update(cx, |view, cx| view.focus_pages(window, cx)));
    let page = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).page());
    assert_eq!(page(cx), Page::General);
    cx.simulate_keystrokes("down");
    assert_eq!(page(cx), Page::Appearance);
    // without calls the last page is Diagnostics: Calls isn't listed, so it isn't reached
    cx.simulate_keystrokes("end");
    assert_eq!(page(cx), Page::Diagnostics);
    cx.simulate_keystrokes("up");
    assert_eq!(page(cx), Page::Privacy);
    cx.simulate_keystrokes("home up");
    assert_eq!(page(cx), Page::General, "Up at the first page stays there");
    // a page clicked takes the keyboard too
    click(cx, "settings-page-notifications");
    cx.simulate_keystrokes("down");
    assert_eq!(page(cx), Page::Privacy);
}

#[test]
fn the_font_picker_shows_a_family_chosen_elsewhere_and_the_settings_scroll() {
    let rig = seeded();
    let settings = Settings {
        appearance: teams_ui::theme::Appearance {
            font_size: *teams_ui::theme::FONT_SIZES.end(),
            ..teams_ui::theme::Appearance::default()
        },
        ..Settings::default()
    };
    let (shell, cx) = shell_window(&rig, settings, Arc::new(ViewTrace::off()));
    cx.update(|_, cx| shell.update(cx, Shell::show_settings));
    click(cx, "settings-page-appearance");
    let shown = |cx: &mut VisualTestContext| {
        let _ = bounds(cx, "settings");
        cx.update(|_, cx| shell.read(cx).settings_view().read(cx).shown_font(cx))
    };
    assert_eq!(shown(cx).as_deref(), Some(teams_ui::settings_view::SYSTEM_FONT_LABEL));
    // another account's view (they share the one settings model) chooses a family
    let model = cx.update(|_, cx| shell.read(cx).settings().clone());
    cx.update(|_, cx| {
        model.update(cx, |model, cx| {
            model.change(
                |settings| settings.appearance.font_family = Some("Noto Sans".to_owned()),
                cx,
            );
        });
    });
    assert_eq!(shown(cx).as_deref(), Some("Noto Sans"));

    // in a small window the font's row is below the pane: a scroll brings it up
    cx.simulate_resize(gpui::size(px(800.0), px(400.0)));
    let before = bounds(cx, "font-family").expect("drawn").origin.y;
    let pane = bounds(cx, "settings").expect("drawn");
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: pane.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-400.0))),
        modifiers: Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    let after = bounds(cx, "font-family").expect("drawn").origin.y;
    assert!(after < before, "the settings scrolled: {before:?} to {after:?}");
}

fn device_info(kind: DeviceKind, name: &str, description: &str) -> DeviceInfo {
    DeviceInfo {
        kind,
        name: name.to_owned(),
        description: description.to_owned(),
    }
}

#[test]
fn the_settings_view_lists_the_devices_and_moves_the_calls_to_the_one_chosen() {
    let rig = seeded();
    let directory = FixedDirectory::new(
        DeviceList::new(vec![
            device_info(DeviceKind::Microphone, "built-in-mic", "Built-in microphone"),
            device_info(DeviceKind::Microphone, "usb-mic", "USB headset"),
            device_info(DeviceKind::Speaker, "built-in-speaker", "Speakers"),
        ])
        .with_default(DeviceKind::Microphone, Some("built-in-mic")),
    );
    let settings = Settings {
        camera: Some("unplugged-cam".to_owned()),
        ..Settings::default()
    };
    let (shell, cx) = shell_window(&rig, settings, Arc::new(ViewTrace::off()));
    // without the calls' devices the view has no page for them
    cx.update(|_, cx| shell.update(cx, Shell::show_settings));
    assert!(bounds(cx, "settings-page-calls").is_none(), "no devices without calls");
    let model = cx.update(|_, cx| shell.read(cx).settings().clone());
    cx.update(|_, cx| attach_devices(&model, Arc::new(directory.clone()), cx));
    click(cx, "settings-page-calls");
    for (picker, rows) in [
        (
            "microphone",
            &["microphone-system", "microphone-built-in-mic", "microphone-usb-mic"][..],
        ),
        ("speaker", &["speaker-system", "speaker-built-in-speaker"][..]),
        // chosen and not plugged in: still shown, as not connected
        ("camera", &["camera-system", "camera-unplugged-cam"][..]),
    ] {
        click(cx, picker);
        for drawn in rows {
            assert!(bounds(cx, drawn).is_some(), "{drawn} is drawn");
        }
        cx.simulate_keystrokes("escape");
    }

    click(cx, "microphone");
    click(cx, "microphone-usb-mic");
    assert_eq!(settings_of(&shell, cx).microphone.as_deref(), Some("usb-mic"));
    assert_eq!(
        directory.chosen(DeviceKind::Microphone).as_deref(),
        Some("usb-mic"),
        "the calls move to it"
    );
    click(cx, "camera");
    click(cx, "camera-system");
    assert_eq!(settings_of(&shell, cx).camera, None);
    assert_eq!(
        directory.chosen(DeviceKind::Camera),
        None,
        "the camera follows the system again"
    );

    // a device plugged in while the settings are shown appears in them
    directory.set_devices(DeviceList::new(vec![
        device_info(DeviceKind::Microphone, "usb-mic", "USB headset"),
        device_info(DeviceKind::Camera, "webcam", "Webcam"),
    ]));
    click(cx, "camera");
    assert!(bounds(cx, "camera-webcam").is_some(), "the camera plugged in is listed");
    cx.simulate_keystrokes("escape");
    click(cx, "microphone");
    assert!(
        bounds(cx, "microphone-built-in-mic").is_none(),
        "the one unplugged is gone"
    );
}

/// Both AI tools installed at synthetic paths (nothing here runs them), with a few models each lists.
fn synthetic_ai_tools(cx: &mut VisualTestContext) {
    let listed = |id: &str, name: &str, latest: bool| Listed {
        id: id.to_owned(),
        name: name.to_owned(),
        latest,
    };
    cx.update(|_, cx| {
        cx.set_global(Tools::with(vec![
            (Backend::Claude, "/synthetic/claude".into()),
            (Backend::Codex, "/synthetic/codex".into()),
        ]));
        cx.set_global(Catalog::with(
            vec![
                listed("opus", "Opus", true),
                listed("claude-opus-5-5", "Opus 5.5", false),
            ],
            vec![listed("gpt-6.1-sol", "GPT-6.1-Sol", false)],
        ));
    });
}

/// The Calls page, scrolled to its AI pickers.
fn ai_pickers_shown(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
    let model = cx.update(|_, cx| shell.read(cx).settings().clone());
    let directory = FixedDirectory::new(DeviceList::new(vec![device_info(
        DeviceKind::Microphone,
        "built-in-mic",
        "Built-in microphone",
    )]));
    cx.update(|_, cx| {
        attach_devices(&model, Arc::new(directory), cx);
        shell.update(cx, Shell::show_settings);
    });
    click(cx, "settings-page-calls");
    cx.update(|_, cx| shell.read(cx).settings_view().read(cx).scroll_to_end());
}

/// Other's name field: a name it can't take is said so and not used; one it can is set.
fn a_model_typed_is_used_when_the_tool_can_take_it(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
    // another model typed: its name field, used on Enter; one it can't take is said so and not used
    click(cx, "caption-translator-model");
    click(cx, "caption-translator-model-other");
    assert!(
        bounds(cx, "caption-translator-model-name").is_some(),
        "the name field shows"
    );
    cx.simulate_input("-x");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        settings_of(shell, cx).caption_translator.model,
        Model::ToolDefault,
        "a dash can't start a model's name"
    );
    cx.simulate_keystrokes("backspace backspace");
    cx.simulate_input("claude-next");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        settings_of(shell, cx).caption_translator,
        AiChoice {
            tool: ToolChoice::Only(Backend::Claude),
            model: Model::Named("claude-next".to_owned()),
        }
    );
    // a model the tool doesn't list keeps its name field, and is listed as the one set
    assert!(
        bounds(cx, "caption-translator-model-name").is_some(),
        "unlisted: its field stays"
    );
    click(cx, "caption-translator-model");
    assert!(bounds(cx, "caption-translator-model-claude-next").is_some());
    cx.simulate_keystrokes("escape");
}

/// The recaps' summarizer: its own choice, and Automatic offers no model by name.
fn the_recaps_summarizer_is_kept_apart(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
    // the recaps' summarizer has the same control, kept apart; Automatic offers no model by name
    click(cx, "recap-summarizer-codex");
    click(cx, "recap-summarizer-model");
    click(cx, "recap-summarizer-model-gpt-6.1-sol");
    assert_eq!(
        settings_of(shell, cx).recap_summarizer,
        AiChoice {
            tool: ToolChoice::Only(Backend::Codex),
            model: Model::Named("gpt-6.1-sol".to_owned()),
        }
    );
    click(cx, "recap-summarizer-automatic");
    click(cx, "recap-summarizer-model");
    assert!(bounds(cx, "recap-summarizer-model-recommended").is_some());
    assert!(
        bounds(cx, "recap-summarizer-model-other").is_none(),
        "no Other for Automatic"
    );
    assert!(bounds(cx, "recap-summarizer-model-gpt-6.1-sol").is_none());
    cx.simulate_keystrokes("escape");
    assert_eq!(settings_of(shell, cx).recap_summarizer, AiChoice::default());
}

#[test]
fn every_ai_setting_picks_its_tool_then_any_model_the_tool_lists_or_one_typed() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    synthetic_ai_tools(cx);
    ai_pickers_shown(&shell, cx);
    let captions = |cx: &mut VisualTestContext| settings_of(&shell, cx).caption_translator;
    let claude = ToolChoice::Only(Backend::Claude);

    // a tool chosen comes with its recommended model; its models are listed, with its own default and Other
    click(cx, "caption-translator-claude");
    assert_eq!(captions(cx), AiChoice::of(claude));
    click(cx, "caption-translator-model");
    for row in ["recommended", "default", "opus", "claude-opus-5-5", "other"] {
        assert!(
            bounds(cx, &format!("caption-translator-model-{row}")).is_some(),
            "{row} is listed"
        );
    }
    assert!(
        bounds(cx, "caption-translator-model-gpt-6.1-sol").is_none(),
        "not Codex's models"
    );
    click(cx, "caption-translator-model-opus");
    assert_eq!(captions(cx).model, Model::Named("opus".to_owned()));
    click(cx, "caption-translator-model");
    click(cx, "caption-translator-model-default");
    assert_eq!(captions(cx).model, Model::ToolDefault);
    assert!(bounds(cx, "caption-translator-model-name").is_none(), "no name field");

    a_model_typed_is_used_when_the_tool_can_take_it(&shell, cx);

    // another tool: its recommended model again
    click(cx, "caption-translator-codex");
    assert_eq!(captions(cx), AiChoice::of(ToolChoice::Only(Backend::Codex)));
    assert!(bounds(cx, "caption-translator-model-name").is_none());

    the_recaps_summarizer_is_kept_apart(&shell, cx);
    assert_eq!(
        captions(cx),
        AiChoice::of(ToolChoice::Only(Backend::Codex)),
        "each setting its own"
    );
}

#[test]
fn an_ai_setting_draws_without_its_tool_and_shows_a_model_set_that_isn_t_listed() {
    let rig = seeded();
    let settings = Settings {
        caption_translator: AiChoice::of(ToolChoice::Only(Backend::Codex)),
        ..Settings::default()
    };
    let (shell, cx) = shell_window(&rig, settings, Arc::new(ViewTrace::off()));
    cx.update(|_, cx| {
        cx.set_global(Tools::with(vec![(Backend::Claude, "/synthetic/claude".into())]));
        cx.set_global(Catalog::default());
    });
    ai_pickers_shown(&shell, cx);
    let pickers = cx.update(|_, cx| {
        let view = shell.read(cx).settings_view().clone();
        view.update(cx, SettingsView::ai_pickers)
    });
    assert!(bounds(cx, "caption-translator").is_some());
    // nothing installed: the recaps' Automatic says so; Codex chosen and missing says so
    cx.update(|_, cx| cx.set_global(Tools::default()));
    assert!(bounds(cx, "recap-summarizer").is_some());
    assert!(cx.update(|_, cx| !pickers[0].read(cx).is_typing()));
    // a model set that isn't listed (or before the list came) shows its name in its field
    cx.update(|_, cx| {
        cx.set_global(Tools::with(vec![(Backend::Claude, "/synthetic/claude".into())]));
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(
                |settings| {
                    settings.recap_summarizer = AiChoice {
                        tool: ToolChoice::Only(Backend::Claude),
                        model: Model::Named("from-the-file".to_owned()),
                    };
                },
                cx,
            );
        });
    });
    assert!(bounds(cx, "recap-summarizer-model-name").is_some());
    assert_eq!(
        cx.update(|_, cx| pickers[1].read(cx).choice(cx).model),
        Model::Named("from-the-file".to_owned())
    );
}

/// Where "copy diagnostics" reads from: the diagnostic log under `dir` holding `log`.
fn diagnostics_in(dir: &tempfile::TempDir, log: &str) -> DiagnosticsSource {
    let log_file = dir.path().join("teams-app.log");
    std::fs::write(&log_file, log).unwrap();
    DiagnosticsSource {
        app: "ngteams 0.1.0".to_owned(),
        log_file: Some(log_file),
        log_problem: None,
    }
}

fn clipboard(cx: &mut VisualTestContext) -> String {
    cx.read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default()
}

const INFO_LINE: &str = "2026-10-22T08:00:00.000Z INFO  teams_core::engine: action: send to a chat\n";
const DEBUG_LINE: &str =
    "2026-10-22T08:00:01.000Z DEBUG teams_proto::http: response GET https://example.invalid/ -> 200\n";

#[test]
fn the_settings_view_copies_the_diagnostics_and_says_when_they_aren_t_safe_to_paste() {
    let rig = seeded();
    let dir = tempfile::tempdir().unwrap();
    let model = SettingsModel::in_memory(Settings::default()).with_diagnostics(diagnostics_in(&dir, INFO_LINE));
    let (shell, cx) = shell_window_with(&rig, model, Arc::new(ViewTrace::off()));
    cx.update(|_, cx| shell.update(cx, Shell::show_settings));
    click(cx, "settings-page-diagnostics");
    assert!(bounds(cx, "diagnostics-copied").is_none(), "nothing copied yet");
    click(cx, "copy-diagnostics");
    let copied = clipboard(cx);
    assert!(copied.starts_with("ngteams 0.1.0 ("), "{copied}");
    assert!(copied.contains("action: send to a chat"));
    assert!(!copied.contains(WARNING));
    assert!(bounds(cx, "diagnostics-copied").is_some());
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).settings().read(cx).copied()),
        Some(Copied { has_debug: false })
    );

    // a debug line among them: said first in the text, and in the view
    std::fs::write(dir.path().join("teams-app.log"), format!("{INFO_LINE}{DEBUG_LINE}")).unwrap();
    click(cx, "copy-diagnostics");
    let copied = clipboard(cx);
    assert!(copied.starts_with(&format!("WARNING: {WARNING}")), "{copied}");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).settings().read(cx).copied()),
        Some(Copied { has_debug: true })
    );
}

#[test]
fn the_diagnostics_are_collected_off_the_foreground_and_copied_once_ready() {
    let rig = seeded();
    let dir = tempfile::tempdir().unwrap();
    let model = SettingsModel::in_memory(Settings::default()).with_diagnostics(diagnostics_in(&dir, INFO_LINE));
    let (shell, cx) = shell_window_with(&rig, model, Arc::new(ViewTrace::off()));
    cx.write_to_clipboard(gpui::ClipboardItem::new_string("before".to_owned()));
    // the foreground's own turn only asks: the flush and the reads run on a background task
    cx.update(|_, cx| {
        let settings = shell.read(cx).settings().clone();
        settings.update(cx, SettingsModel::copy_diagnostics);
        assert_eq!(
            settings.read(cx).copied(),
            None,
            "nothing copied within the click's turn"
        );
    });
    assert_eq!(clipboard(cx), "before");
    cx.run_until_parked();
    assert!(clipboard(cx).contains("action: send to a chat"), "{}", clipboard(cx));
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).settings().read(cx).copied()),
        Some(Copied { has_debug: false })
    );
}

#[test]
fn the_palette_s_copy_diagnostics_copies_them_and_shows_the_settings() {
    let rig = seeded();
    let dir = tempfile::tempdir().unwrap();
    let model = SettingsModel::in_memory(Settings::default())
        .with_diagnostics(diagnostics_in(&dir, &format!("{INFO_LINE}{DEBUG_LINE}")));
    let (trace, written) = trace();
    let (shell, cx) = shell_window_with(&rig, model, trace);
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("copy diag");
    cx.run_until_parked();
    let switcher = cx.update(|_, cx| shell.read(cx).switcher().cloned()).expect("open");
    assert_eq!(cx.update(|_, cx| switcher.read(cx).commands()), ["Copy diagnostics"]);
    assert!(cx.update(|_, cx| switcher.read(cx).matches().is_empty()));
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let copied = clipboard(cx);
    assert!(copied.starts_with(&format!("WARNING: {WARNING}")), "{copied}");
    assert!(cx.update(|_, cx| shell.read(cx).switcher().is_none()));
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Settings);
    assert!(bounds(cx, "diagnostics-copied").is_some());
    assert!(
        written
            .lines()
            .iter()
            .any(|line| line["view"] == "diagnostics" && line["change"] == "copied")
    );
}

#[test]
fn the_palette_sets_the_account_s_status_as_the_account_menu_does() {
    let rig = seeded();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Settings::default(), trace);
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("set status");
    cx.run_until_parked();
    let switcher = cx.update(|_, cx| shell.read(cx).switcher().cloned()).expect("open");
    assert_eq!(
        cx.update(|_, cx| switcher.read(cx).commands()),
        [
            "Set status: Automatic",
            "Set status: Available",
            "Set status: Busy",
            "Set status: Do not disturb",
            "Set status: Appear away",
        ]
    );
    cx.simulate_input(" busy");
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| switcher.read(cx).commands()), ["Set status: Busy"]);
    assert!(cx.update(|_, cx| switcher.read(cx).matches().is_empty()));
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(settings_of(&shell, cx).manual_status, ManualStatus::Busy);
    assert!(cx.update(|_, cx| shell.read(cx).switcher().is_none()), "closed");
    let chosen: Vec<serde_json::Value> = written
        .lines()
        .into_iter()
        .filter(|line| line["view"] == "status-menu" && line["change"] == "chosen")
        .map(|line| line["kinds"].clone())
        .collect();
    assert_eq!(chosen, [serde_json::json!(["busy"])]);

    // and back to Automatic the same way
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("set status automatic");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(settings_of(&shell, cx).manual_status, ManualStatus::Automatic);
}

#[test]
fn a_log_that_couldn_t_be_opened_is_shown_with_the_settings_problems() {
    let model = SettingsModel::in_memory(Settings::default()).with_diagnostics(DiagnosticsSource {
        app: "teams-app 0.1.0".to_owned(),
        log_file: None,
        log_problem: Some("the diagnostic log can't be opened".to_owned()),
    });
    assert_eq!(
        model.problems(),
        [Problem::LogUnavailable {
            why: "the diagnostic log can't be opened".to_owned()
        }]
    );
    assert!(
        model
            .diagnostics()
            .text
            .contains("log: not written (the diagnostic log can't be opened)")
    );
}

/// The settings the shell keeps.
fn settings_of(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Settings {
    cx.update(|_, cx| shell.read(cx).settings().read(cx).settings().clone())
}

#[test]
fn the_root_shows_the_shell_until_the_account_is_signed_out() {
    let rig = seeded();
    let parts = parts(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let models = rig.models.clone();
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (_, cx) = app.add_window_view(|window, cx| {
        let shell = cx.new(|cx| Shell::new(parts, window, cx));
        Root::new(models, Arc::new(ViewTrace::off()), cx).with_shell(shell)
    });
    assert!(bounds(cx, "rail").is_some());
    rig.cx
        .update(|cx| rig.models.signed_out("run `proto-lab auth a`".into(), cx));
    assert!(bounds(cx, "rail").is_none(), "only the sign-in hint is left");
}

#[test]
fn the_first_run_screen_shows_over_everything_until_it_is_confirmed() {
    let rig = seeded();
    // the lab app doesn't ask
    let (_, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    assert!(bounds(cx, "first-run").is_none());
    // the personal build asks: the three switches at their defaults, and nothing sent on its own before "Confirm"
    let model = SettingsModel::in_memory(Settings::default()).requiring_first_run();
    let (shell, cx) = shell_window_with(&rig, model, Arc::new(ViewTrace::off()));
    assert!(bounds(cx, "first-run").is_some());
    for switch in [
        "first-run-send-typing",
        "first-run-send-read-receipts",
        "first-run-send-presence",
    ] {
        assert!(bounds(cx, switch).is_some(), "{switch}");
    }
    // and what a call sends without a click, which has no switch
    assert!(bounds(cx, "first-run-calls").is_some());
    assert!(teams_ui::first_run::CALLS_NOTE.contains("rings here"));
    assert!(cx.update(|_, cx| shell.read(cx).settings().read(cx).awaiting_first_run()));
    click(cx, "first-run-send-typing");
    click(cx, "first-run-confirm");
    assert!(bounds(cx, "first-run").is_none(), "gone once confirmed");
    let settings = settings_of(&shell, cx);
    assert_eq!(settings.first_run, FirstRunState::Confirmed);
    assert!(!settings.send_typing && settings.send_read_receipts && settings.send_presence);
    let model = cx.update(|_, cx| shell.read(cx).settings().clone());
    assert!(cx.update(|_, cx| {
        let model = model.read(cx);
        !model.sends_typing() && model.sends_read_receipts() && model.sends_presence()
    }));
}

/// A settings model kept in a file of its own, so packs can be imported.
fn filed_settings() -> (tempfile::TempDir, SettingsModel) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = teams_ui::SettingsFile::new(dir.path().join("settings.toml"));
    (dir, SettingsModel::load(file))
}

#[test]
fn every_sound_can_be_turned_off_changed_and_made_quieter() {
    use teams_ui::sounds::{SoundEvent, SoundSource};
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    cx.update(|_, cx| shell.update(cx, Shell::show_settings));
    click(cx, "settings-page-notifications");
    for event in SoundEvent::ALL {
        assert!(bounds(cx, event.picker()).is_some(), "{event:?} has its row");
    }
    click(cx, "sound-on-message");
    let sounds = |cx: &mut VisualTestContext| settings_of(&shell, cx).sounds;
    assert!(!sounds(cx).event(SoundEvent::Message).on);
    click(cx, "sound-volume-mention-down");
    assert_eq!(sounds(cx).event(SoundEvent::Mention).volume, 90);
    click(cx, "sound-volume-down");
    assert_eq!(sounds(cx).volume, 90);
    click(cx, "sound-message");
    click(cx, "sound-message-chimes");
    assert_eq!(
        sounds(cx).event(SoundEvent::Message).sound,
        Some(SoundSource::Builtin("chimes"))
    );
    click(cx, "sound-pack");
    click(cx, "sound-pack-retro");
    assert_eq!(sounds(cx).pack, SoundSource::Builtin("retro"));
    click(cx, "sound-pack");
    click(cx, "sound-pack-big-beat");
    assert_eq!(sounds(cx).pack, SoundSource::Builtin("big-beat"));
    click(cx, "sounds-while-busy");
    assert!(!sounds(cx).while_busy);
    assert!(
        bounds(cx, "sound-pack-add-folder").is_none(),
        "settings in memory import nothing"
    );
}

#[test]
fn an_imported_pack_is_chosen_listed_and_removed() {
    use teams_ui::sounds::{SoundEvent, SoundSource};
    let rig = seeded();
    let (_dir, model) = filed_settings();
    let (shell, cx) = shell_window_with(&rig, model, Arc::new(ViewTrace::off()));
    cx.update(|_, cx| shell.update(cx, Shell::show_settings));
    click(cx, "settings-page-notifications");
    assert!(bounds(cx, "sound-pack-add-folder").is_some());
    let pack = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(
        pack.path().join("pack.toml"),
        "name = \"Lab pack\"\n[sounds]\nmessage = \"a.wav\"\n",
    )
    .expect("written");
    let import = |cx: &mut VisualTestContext| {
        let path = pack.path().to_owned();
        click(cx, "sound-pack-add-folder");
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(|options| {
            assert!(options.directories && !options.files && !options.multiple);
            Some(vec![path])
        });
        cx.run_until_parked();
    };
    import(cx);
    assert!(
        bounds(cx, "sound-pack-problem").is_some(),
        "a pack without its sound says so"
    );
    // a quarter second of 48 kHz silence
    let samples = vec![0.0_f32; 12_000];
    let mut writer = teams_media::devices::wav::WavWriter::create(&pack.path().join("a.wav")).expect("made");
    writer.write(&samples).expect("written");
    writer.finish().expect("finished");
    import(cx);
    assert!(bounds(cx, "sound-pack-problem").is_none());
    let source = SoundSource::Imported("lab-pack".to_owned());
    assert_eq!(settings_of(&shell, cx).sounds.pack, source, "chosen as it is added");
    assert!(bounds(cx, "sound-pack-remove-lab-pack").is_some());
    click(cx, SoundEvent::Message.picker());
    assert!(
        bounds(cx, "sound-message-lab-pack").is_some(),
        "it offers the sound it has"
    );
    cx.simulate_keystrokes("escape");
    click(cx, SoundEvent::Mention.picker());
    assert!(bounds(cx, "sound-mention-lab-pack").is_none(), "not one it hasn't");
    cx.simulate_keystrokes("escape");
    click(cx, "sound-pack-remove-lab-pack");
    cx.run_until_parked();
    assert_eq!(settings_of(&shell, cx).sounds.pack, SoundSource::default());
    assert!(bounds(cx, "sound-pack-remove-lab-pack").is_none());
}

#[test]
fn the_own_file_in_the_sound_picker_can_be_chosen_with_the_keyboard() {
    let rig = seeded();
    let (_dir, model) = filed_settings();
    let (shell, cx) = shell_window_with(&rig, model, Arc::new(ViewTrace::off()));
    cx.update(|_, cx| shell.update(cx, Shell::show_settings));
    click(cx, "settings-page-notifications");
    click(cx, "sound-message");
    let first = bounds(cx, "sound-message-pack").expect("the first sound is drawn");
    // A menu opened above its trigger can select a sound under the pointer.
    cx.simulate_mouse_move(first.center(), None, Modifiers::none());
    cx.simulate_keystrokes("up enter");
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|options| {
        assert!(options.files && !options.directories && !options.multiple);
        None
    });
    cx.run_until_parked();
}

#[test]
fn replacing_an_own_file_refreshes_the_playing_sound_without_changing_the_source() {
    use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
    use teams_media::devices::wav::{self, WavWriter};
    use teams_ui::settings::attach_sounds;
    use teams_ui::sounds::{SoundBoard, SoundEvent, SoundPlayer};
    let rig = seeded();
    let (dir, model) = filed_settings();
    let (shell, cx) = shell_window_with(&rig, model, Arc::new(ViewTrace::off()));
    let recording = dir.path().join("out.wav");
    let player = Arc::new(SoundBoard::new(Arc::new(FileDevices {
        input: AudioInput::Silence,
        recording: Some(recording.clone()),
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    })));
    cx.update(|_, cx| {
        let settings = shell.read(cx).settings().clone();
        attach_sounds(&settings, player.clone(), cx);
    });
    let (sender, heard) = std::sync::mpsc::channel();
    player.watch_previewing(Box::new(move || {
        sender.send(()).unwrap();
    }));
    cx.update(|_, cx| shell.update(cx, Shell::show_settings));
    click(cx, "settings-page-notifications");
    for value in [0.1_f32, 0.25] {
        let file = dir.path().join("own.wav");
        let mut writer = WavWriter::create(&file).unwrap();
        writer.write(&vec![value; 12_000]).unwrap();
        writer.finish().unwrap();
        click(cx, "sound-message");
        let menu_row = bounds(cx, "sound-message-pack").expect("drawn");
        let before = bounds(cx, "sound-message-own-file").expect("drawn").origin.y;
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: menu_row.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-400.0))),
            modifiers: Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        let after = bounds(cx, "sound-message-own-file").expect("drawn").origin.y;
        assert!(after < before, "the menu scrolled: {before:?} to {after:?}");
        click(cx, "sound-message-own-file");
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(|options| {
            assert!(options.files && !options.directories && !options.multiple);
            Some(vec![file])
        });
        cx.run_until_parked();
        assert!(bounds(cx, "sound-pack-problem").is_none());
        player.preview(SoundEvent::Message);
        heard.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        heard.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        let samples = wav::read(&recording).unwrap();
        assert!(!samples.is_empty());
        assert!((samples[0] - value).abs() < 0.001, "the imported replacement plays");
    }
}

#[test]
fn another_import_cannot_replace_the_pending_chooser_and_cancel_allows_retry() {
    use teams_ui::settings_view::sounds::Adding;
    use teams_ui::sounds::SoundEvent;
    let rig = seeded();
    let (_dir, model) = filed_settings();
    let (shell, cx) = shell_window_with(&rig, model, Arc::new(ViewTrace::off()));
    cx.update(|_, cx| shell.update(cx, Shell::show_settings));
    click(cx, "settings-page-notifications");
    click(cx, "sound-pack-add-folder");
    cx.update(|_, cx| {
        shell
            .read(cx)
            .settings_view()
            .clone()
            .update(cx, |view, cx| view.add(Adding::File(SoundEvent::Message), cx));
    });
    cx.simulate_path_prompt_response(|options| {
        assert!(options.directories && !options.files, "the original chooser remains");
        None
    });
    cx.run_until_parked();
    click(cx, "sound-pack-add-archive");
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|options| {
        assert!(options.files && !options.directories);
        None
    });
    cx.run_until_parked();
}
