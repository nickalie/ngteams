//! The Activity pane in the shell: the rail's Activity and its count, the items as Teams draws them, opening one (read,
//! its chat at its message), the item's and the header's menus, the chips and the search, and the read-only mode.
#![cfg(test)]

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;

use std::sync::Arc;

use common::{LAB_B, Rig};
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use serde_json::json;
use teams_core::Action;
use teams_core::activity::{ActivityFeed, ActivityFilter, FEED};
use teams_store::Message;
use teams_ui::ViewTrace;
use teams_ui::activity_view::{ActivityView, activity_time, meeting_span, reaction_emoji};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{ListPlace, Main, Shell, ShellParts};

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const GROUP: &str = "19:made-up-group@thread.v2";

fn item(id: i64, kind: &str, sub: &str, (from, name): (&str, &str), message: u64, read: bool) -> Message {
    let activity = json!({
        "activityType": kind, "activitySubtype": sub, "activityId": id, "sourceThreadId": GROUP,
        "sourceMessageId": message, "sourceUserId": from, "sourceUserImDisplayName": name,
        "messagePreview": "a made-up preview", "sourceThreadType": "chat",
    });
    Message {
        conversation_id: FEED.to_owned(),
        id: id.to_string(),
        version: id,
        client_message_id: None,
        sender: None,
        sender_name: None,
        message_type: "Text".to_owned(),
        content: String::new(),
        properties: Some(json!({"activity": activity, "isread": read}).to_string()),
        arrival_time: id,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

fn feed() -> ActivityFeed {
    let messages = [
        item(1000, "mentionInChat", "person", (LAB_B, "Lab B"), 500, true),
        item(2000, "reactionInChat", "heart", (LAB_C, "Lab C"), 600, false),
        item(3000, "call", "missedCall", (LAB_B, "Lab B"), 1, false),
    ];
    ActivityFeed::new(&messages, Some(1500), None)
}

fn shell_window(rig: &Rig) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig
            .cx
            .update(|cx| cx.new(|_| SettingsModel::in_memory(Settings::default()))),
        trace: Arc::new(ViewTrace::off()),
    };
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.run_until_parked();
    (shell, cx)
}

fn rig() -> Rig {
    let rig = Rig::start();
    rig.cx.update(teams_ui::window::init);
    *rig.core.activity.lock().unwrap() = feed();
    rig
}

fn drawn(cx: &mut VisualTestContext, selector: &str) -> bool {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .is_some()
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    assert!(drawn(cx, selector), "{selector} is drawn");
    let found = cx
        .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .expect("drawn");
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
}

fn view(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<ActivityView> {
    cx.update(|_, cx| shell.read(cx).activity_view().clone())
}

fn writes(rig: &Rig) -> Vec<Action> {
    rig.core.actions.lock().unwrap().clone()
}

#[test]
fn the_rail_counts_new_activity_until_the_pane_is_shown() {
    let rig = rig();
    let (shell, cx) = shell_window(&rig);
    // two unread items are newer than the horizon (1500): the mention at 1000 is read anyway
    assert!(drawn(cx, "rail-activity-count"));
    let pane = view(&shell, cx);
    assert_eq!(cx.update(|_, cx| pane.read(cx).unseen()), 2);
    click(cx, "rail-activity");
    assert_eq!(cx.update(|_, cx| shell.read(cx).list_place()), ListPlace::Activity);
    assert!(drawn(cx, "activity-pane"));
    assert!(!drawn(cx, "rail-activity-count"));
    assert!(writes(&rig).contains(&Action::ActivitySeen));
    // every item is listed, the unread ones with their dot
    for id in ["1000", "2000", "3000"] {
        assert!(drawn(cx, &format!("activity-item-{id}")), "item {id}");
    }
    assert!(drawn(cx, "activity-unread-dot"));
    // the rail's Chat brings the chats back
    click(cx, "rail-chats");
    assert_eq!(cx.update(|_, cx| shell.read(cx).list_place()), ListPlace::Chats);
}

#[test]
fn opening_an_item_marks_it_read_and_opens_its_chat_at_its_message() {
    let rig = rig();
    let (shell, cx) = shell_window(&rig);
    click(cx, "rail-activity");
    click(cx, "activity-item-2000");
    assert!(writes(&rig).contains(&Action::ActivityRead {
        item: "2000".to_owned(),
        read: true,
    }));
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(GROUP.to_owned())
    );
    let view = view(&shell, cx);
    let read = cx.update(|_, cx| {
        let view = view.read(cx);
        let item = view.feed().item("2000").expect("item").clone();
        view.is_read(&item)
    });
    assert!(read, "shown read at once");
}

#[test]
fn a_chat_opened_directly_has_its_unread_items_marked_read() {
    let rig = rig();
    let (shell, cx) = shell_window(&rig);
    let pane = view(&shell, cx);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(GROUP.to_owned(), window, cx)));
    assert!(drawn(cx, "rail-activity"));
    assert_eq!(
        cx.update(|_, cx| pane.read(cx).on_screen().map(str::to_owned)),
        Some(GROUP.to_owned())
    );
    // the unread items of that chat, each once; the read mention needs nothing
    let marked: Vec<Action> = writes(&rig)
        .into_iter()
        .filter(|action| matches!(action, Action::ActivityRead { .. }))
        .collect();
    assert_eq!(
        marked,
        [
            Action::ActivityRead {
                item: "3000".to_owned(),
                read: true
            },
            Action::ActivityRead {
                item: "2000".to_owned(),
                read: true
            },
        ]
    );
    let items = cx.update(|_, cx| pane.read(cx).feed().items.clone());
    assert!(items.iter().all(|item| cx.update(|_, cx| pane.read(cx).is_read(item))));
    // drawn again, nothing more is asked
    assert!(drawn(cx, "rail-activity"));
    assert_eq!(
        writes(&rig)
            .iter()
            .filter(|action| matches!(action, Action::ActivityRead { .. }))
            .count(),
        2
    );
}

#[test]
fn the_item_menu_marks_unread_and_removes() {
    let rig = rig();
    let (shell, cx) = shell_window(&rig);
    click(cx, "rail-activity");
    let view = view(&shell, cx);
    cx.update(|_, cx| view.update(cx, |view, cx| view.show_item_menu(Some("1000".to_owned()), cx)));
    click(cx, "activity-item-mark");
    assert!(writes(&rig).contains(&Action::ActivityRead {
        item: "1000".to_owned(),
        read: false,
    }));
    cx.update(|_, cx| view.update(cx, |view, cx| view.show_item_menu(Some("1000".to_owned()), cx)));
    click(cx, "activity-item-remove");
    assert!(writes(&rig).contains(&Action::Delete {
        conversation: FEED.to_owned(),
        message_id: "1000".to_owned(),
    }));
    assert!(!drawn(cx, "activity-item-1000"), "a removed item goes at once");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).toast().cloned()),
        Some("Removed".into())
    );
    // its second group: the notification settings
    cx.update(|_, cx| view.update(cx, |view, cx| view.show_item_menu(Some("2000".to_owned()), cx)));
    click(cx, "activity-item-settings");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Settings);
    assert_eq!(cx.update(|_, cx| view.read(cx).menus().1.map(str::to_owned)), None);
}

#[test]
fn mark_all_as_read_from_the_header_menu() {
    let rig = rig();
    let (shell, cx) = shell_window(&rig);
    click(cx, "rail-activity");
    click(cx, "activity-more");
    click(cx, "activity-mark-all-read");
    assert!(writes(&rig).contains(&Action::ActivityAllRead));
    let view = view(&shell, cx);
    let unread = cx.update(|_, cx| {
        let view = view.read(cx);
        view.feed().items.iter().filter(|item| !view.is_read(item)).count()
    });
    assert_eq!(unread, 0);
}

#[test]
fn chips_and_the_search_narrow_the_items() {
    let rig = rig();
    let (shell, cx) = shell_window(&rig);
    click(cx, "rail-activity");
    let view = view(&shell, cx);
    let shown = |cx: &mut VisualTestContext| -> Vec<String> {
        cx.update(|_, cx| {
            view.read(cx)
                .shown_items(cx)
                .iter()
                .map(|item| item.id.clone())
                .collect()
        })
    };
    click(cx, "activity-chip-unread");
    assert_eq!(shown(cx), ["3000", "2000"]);
    click(cx, "activity-chip-mentions");
    assert_eq!(shown(cx), ["1000"]);
    click(cx, "activity-chip-mentions");
    assert_eq!(cx.update(|_, cx| view.read(cx).filter()), ActivityFilter::All);
    click(cx, "activity-search");
    assert!(drawn(cx, "activity-filter-box"));
    cx.update(|window, cx| view.update(cx, |view, cx| view.search_for("lab c", window, cx)));
    assert_eq!(shown(cx), ["2000"]);
    cx.update(|window, cx| view.update(cx, |view, cx| view.search_for("nobody here", window, cx)));
    assert!(drawn(cx, "activity-empty"));
    cx.update(|window, cx| view.update(cx, |view, cx| view.search_for("", window, cx)));
    assert_eq!(shown(cx).len(), 3);
}

#[test]
fn the_read_only_mode_marks_and_removes_nothing() {
    let rig = rig();
    *rig.core.read_only.lock().unwrap() = true;
    let (shell, cx) = shell_window(&rig);
    click(cx, "rail-activity");
    let view = view(&shell, cx);
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.mark("2000", true, cx);
            view.remove("2000", cx);
            view.mark_all_read(cx);
        });
    });
    cx.run_until_parked();
    assert!(writes(&rig).is_empty(), "{:?}", writes(&rig));
}

#[test]
fn the_keyboard_walks_the_items_and_opens_one() {
    let rig = rig();
    let (shell, cx) = shell_window(&rig);
    click(cx, "rail-activity");
    cx.simulate_keystrokes("down down enter");
    assert!(writes(&rig).contains(&Action::ActivityRead {
        item: "2000".to_owned(),
        read: true,
    }));
    let _ = shell;
}

#[test]
fn dates_and_emoji_read_as_teams_shows_them() {
    use chrono::TimeZone as _;
    let now = chrono::Utc.with_ymd_and_hms(2026, 11, 12, 15, 0, 0).unwrap();
    let today = now.timestamp_millis() - 3_600_000;
    let earlier = chrono::Utc
        .with_ymd_and_hms(2026, 11, 11, 9, 0, 0)
        .unwrap()
        .timestamp_millis();
    let last_year = chrono::Utc
        .with_ymd_and_hms(2025, 3, 4, 9, 0, 0)
        .unwrap()
        .timestamp_millis();
    let cycle = teams_ui::clock::HourCycle::Twelve;
    assert_eq!(activity_time(earlier, &now, cycle), "11/11");
    assert_eq!(activity_time(last_year, &now, cycle), "3/4/25");
    assert!(activity_time(today, &now, cycle).contains("2:00"));
    assert_eq!(reaction_emoji("heart"), "❤️");
    assert_eq!(reaction_emoji("made-up"), "👍");
}

#[test]
fn a_meeting_s_time_reads_as_its_item_s_third_line() {
    use chrono::TimeZone as _;
    let at = |day, hour, minute| {
        chrono::Utc
            .with_ymd_and_hms(2026, 11, day, hour, minute, 0)
            .unwrap()
            .timestamp_millis()
    };
    let (twelve, twenty_four) = (
        teams_ui::clock::HourCycle::Twelve,
        teams_ui::clock::HourCycle::TwentyFour,
    );
    assert_eq!(
        meeting_span(at(11, 16, 30), at(11, 17, 0), &chrono::Utc, twelve),
        "Nov 11, 4:30 PM - 5:00 PM"
    );
    assert_eq!(
        meeting_span(at(11, 16, 30), at(11, 17, 0), &chrono::Utc, twenty_four),
        "Nov 11, 16:30 - 17:00"
    );
    assert_eq!(
        meeting_span(at(11, 23, 0), at(12, 1, 0), &chrono::Utc, twelve),
        "Nov 11, 11:00 PM - Nov 12, 1:00 AM"
    );
    assert_eq!(meeting_span(i64::MAX, 0, &chrono::Utc, twelve), "");
}
