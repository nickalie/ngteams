//! A group chat's name from its header (the chat header's "Name group chat"), on the fake core in a window of its own:
//! the pencil and its popup, Save and Enter, the 250-character limit, an empty name, Cancel and Escape, a save that
//! fails (also after the popup closed), the chats that show no pencil, and the new name in the header and the list
//! once the engine tells it.
//! Synthetic accounts and chats only.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

pub mod common;
#[path = "common/parts.rs"]
mod parts;

use std::sync::Arc;

use common::{LAB_A, LAB_B, Rig};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use teams_core::Action;
use teams_store::{Conversation, ConversationKind, Member, Message, OutboxKind, OutboxState};
use teams_ui::ViewTrace;
use teams_ui::chat_rename::{ChatRename, NOT_SAVED};
use teams_ui::settings::Settings;
use teams_ui::shell::Shell;

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
/// The a–b–c chat named "Lab trio 39".
const NAMED: &str = "19:000000000000000000000000000named@thread.v2";
/// An a–b–c chat with no name: called by its people.
const UNNAMED: &str = "19:0000000000000000000000000unnamed@thread.v2";
/// A meeting's chat of a and b.
const MEETING: &str = "19:meeting_synthetic-rename@thread.v2";
/// 2026-01-01T12:00:00Z, ms.
const NOON: i64 = 1_767_268_800_000;
const MINUTE: i64 = 60_000;

fn chat(id: &str, kind: ConversationKind, topic: Option<&str>, last: i64) -> Conversation {
    Conversation {
        id: id.to_owned(),
        kind,
        topic: topic.map(str::to_owned),
        version: 1,
        last_activity: Some(last),
    }
}

fn message(conversation: &str, id: &str, sender: &str) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: "RichText/Html".to_owned(),
        content: "<p>hi</p>".to_owned(),
        properties: None,
        arrival_time: NOON,
        compose_time: Some(NOON),
        edit_time: None,
        delete_time: None,
    }
}

/// a's store: the named and the unnamed a–b–c chats, the a–b 1:1 chat and a meeting's chat.
fn seeded() -> Rig {
    let rig = Rig::start();
    let core = &rig.core;
    core.conversations.lock().unwrap().extend([
        chat(NAMED, ConversationKind::Group, Some("Lab trio 39"), NOON + 4 * MINUTE),
        chat(UNNAMED, ConversationKind::Group, None, NOON + 3 * MINUTE),
        chat(AB, ConversationKind::OneToOne, None, NOON + 2 * MINUTE),
        chat(MEETING, ConversationKind::Meeting, Some("Planning"), NOON + MINUTE),
    ]);
    let mut messages = core.messages.lock().unwrap();
    for (id, at) in [(NAMED, "1"), (UNNAMED, "2"), (AB, "3"), (MEETING, "4")] {
        messages.insert(id.to_owned(), vec![message(id, at, LAB_B)]);
    }
    drop(messages);
    let mut stored = core.members.lock().unwrap();
    for group in [NAMED, UNNAMED] {
        stored.insert(
            group.to_owned(),
            [LAB_A, LAB_B, LAB_C].iter().map(|mri| Member::new(mri, None)).collect(),
        );
    }
    for pair in [AB, MEETING] {
        stored.insert(
            pair.to_owned(),
            vec![Member::new(LAB_A, None), Member::new(LAB_B, None)],
        );
    }
    drop(stored);
    let mut names = core.names.lock().unwrap();
    names.insert(LAB_A.to_owned(), "Lab Ay".to_owned());
    names.insert(LAB_B.to_owned(), "Lab Bee".to_owned());
    names.insert(LAB_C.to_owned(), "Lab Sea".to_owned());
    drop(names);
    rig.cx.update(teams_ui::window::init);
    rig
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    cx.run_until_parked();
}

fn shell_window(rig: &Rig) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let parts = parts::parts(rig, Settings::default(), Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    draw(cx);
    (shell, cx)
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let drawn = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(drawn.center(), Modifiers::none());
    draw(cx);
}

fn open(shell: &Entity<Shell>, cx: &mut VisualTestContext, id: &str) {
    shell.update_in(cx, |shell, window, cx| shell.open_chat(id.to_owned(), window, cx));
    draw(cx);
}

fn popup(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Option<Entity<ChatRename>> {
    cx.update(|_, cx| shell.read(cx).rename().cloned())
}

fn value(view: &Entity<ChatRename>, cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| view.read(cx).value(cx))
}

fn renames(rig: &Rig) -> Vec<(String, String)> {
    let actions = rig.core.actions.lock().unwrap().clone();
    actions
        .into_iter()
        .filter_map(|action| match action {
            Action::RenameChat { conversation, topic } => Some((conversation, topic)),
            _ => None,
        })
        .collect()
}

/// The name the list's row of `id` shows.
fn row_name(shell: &Entity<Shell>, cx: &mut VisualTestContext, id: &str) -> String {
    cx.update(|_, cx| {
        let list = shell.read(cx).list().read(cx);
        list.row(id).map(|row| row.name.to_string()).unwrap_or_default()
    })
}

/// The engine's word that `id` is now called `topic`, as it tells it once a rename went through or was pushed.
fn renamed(rig: &Rig, cx: &mut VisualTestContext, id: &str, topic: Option<&str>) {
    let mut conversations = rig.core.conversations.lock().unwrap();
    if let Some(stored) = conversations.iter_mut().find(|stored| stored.id == id) {
        stored.topic = topic.map(str::to_owned);
    }
    drop(conversations);
    rig.send(teams_core::Event::ConversationChanged {
        conversation: id.to_owned(),
    });
    draw(cx);
}

#[test]
fn a_group_chat_s_pencil_opens_its_name_and_enter_renames_it() {
    let rig = seeded();
    let sent = common::outbox_row(7, OutboxKind::RenameChat, OutboxState::Sent);
    rig.core.write_answers.lock().unwrap().push_back(sent);
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, NAMED);
    assert!(popup(&shell, cx).is_none());
    click(cx, "chat-header-rename");
    let view = popup(&shell, cx).expect("the popup");
    assert!(cx.debug_bounds("chat-rename").is_some());
    assert!(cx.debug_bounds("chat-rename-problem").is_none());
    assert_eq!(value(&view, cx), "Lab trio 39");
    // the popup hangs under the header, from the chat's name
    let header = cx.debug_bounds("chat-header").expect("the header");
    let drawn = cx.debug_bounds("chat-rename").expect("the popup");
    assert!(
        drawn.top() < header.bottom() && drawn.bottom() > header.bottom(),
        "{drawn:?}"
    );
    assert!((drawn.size.width - px(400.0)).abs() < px(1.0), "{drawn:?}");

    // the name is selected and the keyboard is in the box: typing replaces it, Enter saves
    cx.simulate_input("Lab trio 40");
    draw(cx);
    assert_eq!(value(&view, cx), "Lab trio 40");
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(renames(&rig), [(NAMED.to_owned(), "Lab trio 40".to_owned())]);
    assert!(popup(&shell, cx).is_none(), "closed once it went out");

    // the engine calls the chat by its new name: the list's row and the header say it
    renamed(&rig, cx, NAMED, Some("Lab trio 40"));
    assert_eq!(row_name(&shell, cx, NAMED), "Lab trio 40");
}

#[test]
fn the_box_refuses_a_251st_character_and_says_so_until_the_next_change() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, NAMED);
    click(cx, "chat-header-rename");
    let view = popup(&shell, cx).expect("the popup");
    let longest = "a".repeat(250);
    cx.simulate_input(&longest);
    draw(cx);
    assert_eq!(value(&view, cx).chars().count(), 250);
    assert!(!cx.update(|_, cx| view.read(cx).too_long()));
    cx.simulate_input("x");
    draw(cx);
    assert_eq!(value(&view, cx), longest, "the 251st is refused");
    assert!(cx.update(|_, cx| view.read(cx).too_long()));
    assert!(cx.debug_bounds("chat-rename-problem").is_some());
    cx.simulate_keystrokes("backspace");
    draw(cx);
    assert!(
        cx.debug_bounds("chat-rename-problem").is_none(),
        "gone with the next change"
    );
    // typed one after the other: the first fits, the second is refused
    cx.simulate_input("yz");
    draw(cx);
    assert_eq!(value(&view, cx).chars().count(), 250);
    assert!(value(&view, cx).ends_with('y'));
    assert!(cx.update(|_, cx| view.read(cx).too_long()));
    assert!(renames(&rig).is_empty());
}

#[test]
fn an_unnamed_group_chat_opens_empty_and_an_emptied_name_takes_it_off() {
    let rig = seeded();
    let sent = common::outbox_row(7, OutboxKind::RenameChat, OutboxState::Sent);
    rig.core.write_answers.lock().unwrap().push_back(sent);
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, UNNAMED);
    click(cx, "chat-header-rename");
    let view = popup(&shell, cx).expect("the popup");
    assert_eq!(value(&view, cx), "", "called by its people, it has no name to show");
    // Save with nothing changed sends nothing
    click(cx, "chat-rename-save");
    assert!(popup(&shell, cx).is_none());
    assert!(renames(&rig).is_empty());

    open(&shell, cx, NAMED);
    click(cx, "chat-header-rename");
    cx.simulate_keystrokes("backspace");
    draw(cx);
    click(cx, "chat-rename-save");
    assert_eq!(renames(&rig), [(NAMED.to_owned(), String::new())]);
    renamed(&rig, cx, NAMED, None);
    let by_people = row_name(&shell, cx, UNNAMED);
    assert_eq!(
        row_name(&shell, cx, NAMED),
        by_people,
        "called by its people, as the unnamed chat is"
    );
}

#[test]
fn cancel_escape_and_a_press_outside_close_it_with_nothing_sent() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, NAMED);
    click(cx, "chat-header-rename");
    cx.simulate_input("Another name");
    draw(cx);
    click(cx, "chat-rename-cancel");
    assert!(popup(&shell, cx).is_none());

    click(cx, "chat-header-rename");
    let view = popup(&shell, cx).expect("the popup");
    assert_eq!(value(&view, cx), "Lab trio 39", "a new popup holds the name as it is");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(popup(&shell, cx).is_none());

    // the pencil closes it again, and a press on the chat outside it closes it
    click(cx, "chat-header-rename");
    click(cx, "chat-header-rename");
    assert!(popup(&shell, cx).is_none());
    click(cx, "chat-header-rename");
    let pane = cx.debug_bounds("conversation-pane").expect("the pane");
    cx.simulate_click(pane.center(), Modifiers::none());
    draw(cx);
    assert!(popup(&shell, cx).is_none());

    // another chat opened closes it
    click(cx, "chat-header-rename");
    open(&shell, cx, UNNAMED);
    assert!(popup(&shell, cx).is_none());
    assert!(renames(&rig).is_empty());
}

#[test]
fn a_save_that_did_not_go_out_says_so_and_keeps_the_name_typed() {
    let rig = seeded();
    let failed = common::outbox_row(7, OutboxKind::RenameChat, OutboxState::Failed);
    rig.core.write_answers.lock().unwrap().push_back(failed);
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, NAMED);
    click(cx, "chat-header-rename");
    cx.simulate_input("Lab trio 40");
    draw(cx);
    click(cx, "chat-rename-save");
    let view = popup(&shell, cx).expect("still open");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).problem().cloned()),
        Some(NOT_SAVED.into())
    );
    assert!(cx.debug_bounds("chat-rename-problem").is_some());
    assert_eq!(value(&view, cx), "Lab trio 40");
    assert_eq!(row_name(&shell, cx, NAMED), "Lab trio 39");
}

#[test]
fn a_popup_closed_while_its_save_is_on_its_way_tells_a_failure_as_a_toast() {
    let rig = seeded();
    // the first save waits in the outbox, then fails; the second waits, then goes out
    let mut writes = rig.core.write_answers.lock().unwrap();
    writes.push_back(common::outbox_row(7, OutboxKind::RenameChat, OutboxState::Pending));
    writes.push_back(common::outbox_row(8, OutboxKind::RenameChat, OutboxState::Pending));
    drop(writes);
    let mut settled = rig.core.outbox_answers.lock().unwrap();
    settled.push_back(common::outbox_row(7, OutboxKind::RenameChat, OutboxState::Failed));
    settled.push_back(common::outbox_row(8, OutboxKind::RenameChat, OutboxState::Sent));
    drop(settled);
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, NAMED);
    let toast = |cx: &mut VisualTestContext| cx.update(|_, cx| shell.read(cx).toast().cloned());

    // Cancel waits for the save, the pencil closes the popup, and the failure that comes after shows as a toast
    click(cx, "chat-header-rename");
    cx.simulate_input("Lab trio 40");
    draw(cx);
    click(cx, "chat-rename-save");
    let view = popup(&shell, cx).expect("open while it saves");
    assert!(cx.update(|_, cx| view.read(cx).saving()));
    click(cx, "chat-rename-cancel");
    assert!(popup(&shell, cx).is_some(), "Cancel waits for the save");
    click(cx, "chat-header-rename");
    assert!(popup(&shell, cx).is_none());
    assert_eq!(toast(cx), None);
    cx.executor().advance_clock(std::time::Duration::from_secs(1));
    draw(cx);
    assert_eq!(toast(cx), Some(NOT_SAVED.into()));
    cx.executor().advance_clock(std::time::Duration::from_secs(5));
    draw(cx);
    assert_eq!(toast(cx), None);

    // another chat opened closes it too, and a save that goes out after that says nothing
    click(cx, "chat-header-rename");
    cx.simulate_input("Lab trio 41");
    draw(cx);
    click(cx, "chat-rename-save");
    open(&shell, cx, UNNAMED);
    assert!(popup(&shell, cx).is_none());
    cx.executor().advance_clock(std::time::Duration::from_secs(1));
    draw(cx);
    assert_eq!(toast(cx), None);
    assert_eq!(
        renames(&rig),
        vec![
            (NAMED.to_owned(), "Lab trio 40".to_owned()),
            (NAMED.to_owned(), "Lab trio 41".to_owned())
        ]
    );
}

#[test]
fn a_1_1_chat_a_meeting_s_chat_and_the_read_only_mode_show_no_pencil() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    for id in [AB, MEETING] {
        open(&shell, cx, id);
        assert!(cx.debug_bounds("chat-header").is_some());
        assert!(cx.debug_bounds("chat-header-rename").is_none(), "{id}");
    }
    open(&shell, cx, NAMED);
    assert!(cx.debug_bounds("chat-header-rename").is_some());

    let rig = seeded();
    *rig.core.read_only.lock().unwrap() = true;
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, NAMED);
    assert!(cx.debug_bounds("chat-header").is_some());
    assert!(cx.debug_bounds("chat-header-rename").is_none());
    shell.update_in(cx, Shell::toggle_rename);
    assert!(popup(&shell, cx).is_none());
}
