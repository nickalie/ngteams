//! The Activity pane in the shell: the rail's Activity and its count, the items as Teams draws them, opening one (read,
//! its chat at its message), the item's and the header's menus, the chips and the search, and the read-only mode.
#![cfg(test)]

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;

use std::sync::Arc;

use common::{LAB_A, LAB_B, Rig};
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use serde_json::json;
use teams_core::Action;
use teams_core::activity::{ActivityFeed, ActivityFilter, FEED};
use teams_store::{Conversation, ConversationKind, Member, Message};
use teams_ui::ViewTrace;
use teams_ui::activity_view::{ActivityView, CHAT_GONE, activity_time, meeting_span};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{ListPlace, Main, Shell, ShellParts};

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const GROUP: &str = "19:made-up-group@thread.v2";
/// The 1:1 chat of a (the account) and b.
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
/// A chat the account left (or deleted): the feed still names it, the store doesn't have it.
const GONE: &str = "19:made-up-gone@thread.v2";

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
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (shell, cx)
}

/// a's store: the group chat without a topic (a, b and c, made-up names) and the 1:1 chat with b.
fn rig() -> Rig {
    let rig = Rig::start();
    rig.cx.update(teams_ui::window::init);
    *rig.core.activity.lock().unwrap() = feed();
    let chat = |id: &str, kind| Conversation {
        id: id.to_owned(),
        kind,
        topic: None,
        version: 1,
        last_activity: Some(1),
    };
    rig.core.conversations.lock().unwrap().extend([
        chat(GROUP, ConversationKind::Group),
        chat(AB, ConversationKind::OneToOne),
    ]);
    let members = |mris: &[&str]| mris.iter().map(|mri| Member::new(mri, None)).collect::<Vec<_>>();
    let mut stored = rig.core.members.lock().unwrap();
    stored.insert(GROUP.to_owned(), members(&[LAB_A, LAB_B, LAB_C]));
    stored.insert(AB.to_owned(), members(&[LAB_A, LAB_B]));
    drop(stored);
    let mut names = rig.core.names.lock().unwrap();
    names.insert(LAB_B.to_owned(), "Nikita Gavrilin".to_owned());
    names.insert(LAB_C.to_owned(), "Dmitry Zolin".to_owned());
    drop(names);
    rig
}

fn in_chat(mut message: Message, chat: &str) -> Message {
    let mut properties: serde_json::Value = serde_json::from_str(message.properties.as_deref().unwrap()).unwrap();
    properties["activity"]["sourceThreadId"] = json!(chat);
    message.properties = Some(properties.to_string());
    message
}

fn place(pane: &Entity<ActivityView>, id: &str, cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| {
        let pane = pane.read(cx);
        pane.place(pane.feed().item(id).expect("the item"), cx)
    })
}

fn read(pane: &Entity<ActivityView>, id: &str, cx: &mut VisualTestContext) -> bool {
    cx.update(|_, cx| {
        let pane = pane.read(cx);
        pane.is_read(pane.feed().item(id).expect("the item"))
    })
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

fn write_horizon(pane: &Entity<ActivityView>, seen: bool, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            if seen {
                pane.set_shown(true, window, cx);
            } else {
                pane.mark_all_read(cx);
            }
        });
    });
    cx.run_until_parked();
}

fn reopen_activity(pane: &Entity<ActivityView>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.set_shown(false, window, cx);
            pane.set_shown(true, window, cx);
        });
    });
    cx.run_until_parked();
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
fn dates_read_as_teams_shows_them() {
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

#[test]
fn explicit_unread_survives_reloads_and_only_new_items_are_read_automatically() {
    let rig = rig();
    let (shell, cx) = shell_window(&rig);
    let pane = view(&shell, cx);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(GROUP.to_owned(), window, cx)));
    assert!(drawn(cx, "rail-activity"));
    cx.update(|_, cx| pane.update(cx, |pane, cx| pane.mark("2000", false, cx)));
    rig.core.actions.lock().unwrap().clear();
    let mut messages = [
        item(2000, "reactionInChat", "heart", (LAB_C, "Lab C"), 600, false),
        item(4000, "mentionInChat", "person", (LAB_B, "Lab B"), 700, false),
    ];
    messages[0].version = 5000; // the explicit unread write returned with a new version
    *rig.core.activity.lock().unwrap() = ActivityFeed::new(&messages, None, None);
    cx.update(|_, cx| pane.update(cx, ActivityView::reload));
    cx.run_until_parked();
    assert_eq!(
        writes(&rig),
        [Action::ActivityRead {
            item: "4000".to_owned(),
            read: true
        }]
    );
    assert!(!cx.update(|_, cx| {
        let pane = pane.read(cx);
        pane.is_read(pane.feed().item("2000").unwrap())
    }));
    // Leaving and opening the conversation again makes that unread item seen.
    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            pane.set_on_screen(None, cx);
            pane.set_on_screen(Some(GROUP), cx);
        });
    });
    assert!(writes(&rig).contains(&Action::ActivityRead {
        item: "2000".to_owned(),
        read: true
    }));
}

#[test]
fn an_inactive_window_does_not_read_new_activity_until_activation() {
    let rig = rig();
    let (shell, cx) = shell_window(&rig);
    let pane = view(&shell, cx);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(GROUP.to_owned(), window, cx)));
    assert!(drawn(cx, "rail-activity"));
    cx.deactivate_window();
    cx.run_until_parked();
    rig.core.actions.lock().unwrap().clear();
    *rig.core.activity.lock().unwrap() = ActivityFeed::new(
        &[item(4000, "mentionInChat", "person", (LAB_B, "Lab B"), 700, false)],
        None,
        None,
    );
    cx.update(|_, cx| pane.update(cx, ActivityView::reload));
    cx.run_until_parked();
    assert!(writes(&rig).is_empty());
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    assert_eq!(
        writes(&rig),
        [Action::ActivityRead {
            item: "4000".to_owned(),
            read: true
        }]
    );
}

#[test]
fn refused_and_terminal_failed_writes_restore_the_stored_item() {
    use common::outbox_row;
    use teams_store::{OutboxKind, OutboxState};
    let rig = rig();
    let (shell, cx) = shell_window(&rig);
    let pane = view(&shell, cx);
    *rig.core.stopped.lock().unwrap() = true;
    cx.update(|_, cx| pane.update(cx, |pane, cx| pane.mark("2000", true, cx)));
    cx.run_until_parked();
    assert!(!cx.update(|_, cx| {
        let pane = pane.read(cx);
        pane.is_read(pane.feed().item("2000").unwrap())
    }));
    *rig.core.stopped.lock().unwrap() = false;
    rig.core
        .write_answers
        .lock()
        .unwrap()
        .push_back(outbox_row(500, OutboxKind::Delete, OutboxState::Pending));
    cx.update(|_, cx| pane.update(cx, |pane, cx| pane.remove("2000", cx)));
    cx.run_until_parked();
    assert!(!cx.update(|_, cx| pane.read(cx).shown_items(cx).iter().any(|item| item.id == "2000")));
    rig.cx.update(|cx| {
        rig.models.apply(
            teams_core::Event::Outbox(outbox_row(500, OutboxKind::Delete, OutboxState::Failed)),
            cx,
        );
    });
    cx.run_until_parked();
    assert!(cx.update(|_, cx| pane.read(cx).shown_items(cx).iter().any(|item| item.id == "2000")));
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).toast().cloned()),
        Some("Couldn't update activity. Try again.".into())
    );
}

#[test]
fn opening_a_conversation_does_not_supersede_a_pending_removal() {
    use common::outbox_row;
    use teams_store::{OutboxKind, OutboxState};
    let rig = rig();
    let (shell, cx) = shell_window(&rig);
    let pane = view(&shell, cx);
    rig.core
        .write_answers
        .lock()
        .unwrap()
        .push_back(outbox_row(500, OutboxKind::Delete, OutboxState::Pending));
    cx.update(|_, cx| pane.update(cx, |pane, cx| pane.remove("2000", cx)));
    cx.run_until_parked();
    cx.update(|_, cx| pane.update(cx, |pane, cx| pane.set_on_screen(Some(GROUP), cx)));
    cx.run_until_parked();
    assert!(
        !writes(&rig).contains(&Action::ActivityRead {
            item: "2000".to_owned(),
            read: true,
        }),
        "the removed item is not automatically marked read"
    );
    rig.cx.update(|cx| {
        rig.models.apply(
            teams_core::Event::Outbox(outbox_row(500, OutboxKind::Delete, OutboxState::Failed)),
            cx,
        );
    });
    cx.run_until_parked();
    assert!(cx.update(|_, cx| pane.read(cx).shown_items(cx).iter().any(|item| item.id == "2000")));
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).toast().cloned()),
        Some("Couldn't update activity. Try again.".into())
    );
}

#[test]
fn failed_horizon_writes_restore_the_feed_and_do_not_retry_on_reload() {
    use common::outbox_row;
    use teams_store::{OutboxKind, OutboxState};
    for seen in [false, true] {
        for state in [None, Some(OutboxState::Denied), Some(OutboxState::Failed)] {
            let rig = rig();
            let (shell, cx) = shell_window(&rig);
            let pane = view(&shell, cx);
            if let Some(state) = state {
                let kind = if seen {
                    OutboxKind::ReadReceipt
                } else {
                    OutboxKind::FeedRead
                };
                rig.core
                    .write_answers
                    .lock()
                    .unwrap()
                    .push_back(outbox_row(500, kind, state));
            } else {
                *rig.core.stopped.lock().unwrap() = true;
            }
            write_horizon(&pane, seen, cx);
            assert_eq!(
                cx.update(|_, cx| pane.read(cx).feed().clone()),
                feed(),
                "{seen} {state:?}"
            );
            assert_eq!(
                cx.update(|_, cx| shell.read(cx).toast().cloned()),
                Some("Couldn't update activity. Try again.".into())
            );
            cx.update(|_, cx| pane.update(cx, ActivityView::reload));
            cx.run_until_parked();
            assert_eq!(
                writes(&rig).len(),
                1,
                "a reload must not retry a failed automatic seen mark"
            );
            if seen {
                reopen_activity(&pane, cx);
                assert_eq!(writes(&rig).len(), 2, "opening the pane again retries the seen mark");
            }
        }
    }
}

#[test]
fn pending_horizons_survive_reload_and_terminal_failures_restore_the_feed() {
    use common::outbox_row;
    use teams_store::{OutboxKind, OutboxState};
    for seen in [false, true] {
        let rig = rig();
        let (shell, cx) = shell_window(&rig);
        let pane = view(&shell, cx);
        let kind = if seen {
            OutboxKind::ReadReceipt
        } else {
            OutboxKind::FeedRead
        };
        rig.core
            .write_answers
            .lock()
            .unwrap()
            .push_back(outbox_row(500, kind, OutboxState::Pending));
        write_horizon(&pane, seen, cx);
        cx.update(|_, cx| pane.update(cx, ActivityView::reload));
        cx.run_until_parked();
        cx.update(|_, cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.feed().seen_ms, Some(if seen { 3000 } else { 1500 }));
            assert_eq!(pane.feed().bookmark_ms.is_some(), !seen);
        });
        rig.cx.update(|cx| {
            rig.models.apply(
                teams_core::Event::Outbox(outbox_row(500, kind, OutboxState::Failed)),
                cx,
            );
        });
        cx.run_until_parked();
        assert_eq!(cx.update(|_, cx| pane.read(cx).feed().clone()), feed());
        assert_eq!(writes(&rig).len(), 1);
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).toast().cloned()),
            Some("Couldn't update activity. Try again.".into())
        );
    }
}

#[test]
fn a_seen_horizon_already_advanced_elsewhere_reloads_without_a_failure_toast() {
    let rig = rig();
    let (shell, cx) = shell_window(&rig);
    let pane = view(&shell, cx);
    rig.core.activity.lock().unwrap().seen_ms = Some(3000);
    rig.core
        .write_errors
        .lock()
        .unwrap()
        .push_back(teams_core::EngineError::NothingNewInActivity);
    write_horizon(&pane, true, cx);
    assert_eq!(cx.update(|_, cx| pane.read(cx).feed().seen_ms), Some(3000));
    assert_eq!(cx.update(|_, cx| shell.read(cx).toast().cloned()), None);
    assert_eq!(writes(&rig), [Action::ActivitySeen]);
}

#[test]
fn an_item_read_on_arrival_in_the_chat_on_screen_is_never_counted() {
    let rig = rig();
    let (shell, cx) = shell_window(&rig);
    let pane = view(&shell, cx);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(GROUP.to_owned(), window, cx)));
    assert!(drawn(cx, "rail-activity"));
    assert_eq!(cx.update(|_, cx| pane.read(cx).unseen()), 0);
    // a mention arrives for the chat on screen: read at once, its change not back yet (the feed still says unread)
    let mut messages = vec![item(4000, "mentionInChat", "person", (LAB_B, "Lab B"), 700, false)];
    messages.extend([
        item(1000, "mentionInChat", "person", (LAB_B, "Lab B"), 500, true),
        item(2000, "reactionInChat", "heart", (LAB_C, "Lab C"), 600, true),
        item(3000, "call", "missedCall", (LAB_B, "Lab B"), 1, true),
    ]);
    *rig.core.activity.lock().unwrap() = ActivityFeed::new(&messages, Some(1500), None);
    cx.update(|_, cx| pane.update(cx, ActivityView::reload));
    cx.run_until_parked();
    assert!(writes(&rig).contains(&Action::ActivityRead {
        item: "4000".to_owned(),
        read: true
    }));
    assert!(read(&pane, "4000", cx));
    assert_eq!(cx.update(|_, cx| pane.read(cx).unseen()), 0);
    assert!(!drawn(cx, "rail-activity-count"));
    // a removed item isn't counted either
    *rig.core.activity.lock().unwrap() = ActivityFeed::new(
        &[in_chat(
            item(5000, "mentionInChat", "person", (LAB_B, "Lab B"), 800, false),
            GONE,
        )],
        Some(1500),
        None,
    );
    cx.update(|_, cx| pane.update(cx, ActivityView::reload));
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| pane.read(cx).unseen()), 1);
    cx.update(|_, cx| pane.update(cx, |pane, cx| pane.remove("5000", cx)));
    assert_eq!(cx.update(|_, cx| pane.read(cx).unseen()), 0);
}

#[test]
fn an_item_whose_chat_is_gone_is_read_and_says_so_without_opening_a_chat() {
    let rig = rig();
    let mut messages = vec![in_chat(
        item(4000, "mentionInChat", "person", (LAB_B, "Nikita Gavrilin"), 700, false),
        GONE,
    )];
    messages.extend([item(2000, "reactionInChat", "heart", (LAB_C, "Lab C"), 600, false)]);
    *rig.core.activity.lock().unwrap() = ActivityFeed::new(&messages, None, None);
    let (shell, cx) = shell_window(&rig);
    let pane = view(&shell, cx);
    click(cx, "rail-activity");
    // its third line: the person behind it, as a chat with them
    assert_eq!(place(&pane, "4000", cx), "Chat with Nikita");
    click(cx, "activity-item-4000");
    assert!(writes(&rig).contains(&Action::ActivityRead {
        item: "4000".to_owned(),
        read: true
    }));
    assert!(read(&pane, "4000", cx));
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Nothing);
    assert!(
        cx.update(|_, cx| shell.read(cx).chat().is_none()),
        "no chat view to write in"
    );
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).toast().cloned()),
        Some(CHAT_GONE.into())
    );
    // a chat that is there still opens
    click(cx, "activity-item-2000");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(GROUP.to_owned())
    );
}

#[test]
fn a_group_chat_without_a_topic_is_named_by_its_members_first_names() {
    let rig = rig();
    let (shell, cx) = shell_window(&rig);
    let pane = view(&shell, cx);
    click(cx, "rail-activity");
    // as Teams' Activity says it, not the chat list's "Dmitry and Nikita"
    assert_eq!(place(&pane, "2000", cx), "Chat with Dmitry, Nikita");
    assert_eq!(place(&pane, "3000", cx), "Teams call");
    // the search finds it by those words
    cx.update(|window, cx| pane.update(cx, |pane, cx| pane.search_for("chat with dmitry", window, cx)));
    let shown: Vec<String> = cx.update(|_, cx| {
        pane.read(cx)
            .shown_items(cx)
            .iter()
            .map(|item| item.id.clone())
            .collect()
    });
    assert_eq!(shown, ["2000", "1000"]);
}

#[test]
fn a_missed_call_s_chat_and_call_read_it() {
    let rig = rig();
    let missed = [item(3000, "call", "missedCall", (LAB_B, "Lab B"), 1, false)];
    *rig.core.activity.lock().unwrap() = ActivityFeed::new(&missed, None, None);
    let (shell, cx) = shell_window(&rig);
    let pane = view(&shell, cx);
    click(cx, "rail-activity");
    click(cx, "activity-chat-3000");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(AB.to_owned())
    );
    assert!(writes(&rig).contains(&Action::ActivityRead {
        item: "3000".to_owned(),
        read: true
    }));
    assert!(read(&pane, "3000", cx));
    // unread again, then Call: read again
    cx.update(|_, cx| pane.update(cx, |pane, cx| pane.mark("3000", false, cx)));
    assert!(!read(&pane, "3000", cx));
    click(cx, "rail-activity");
    rig.core.actions.lock().unwrap().clear();
    click(cx, "activity-call-3000");
    assert!(writes(&rig).contains(&Action::ActivityRead {
        item: "3000".to_owned(),
        read: true
    }));
    assert!(read(&pane, "3000", cx));
}
