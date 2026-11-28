//! A chat's people from its header (the chat header's "View and add participants"), on the fake core in a window of
//! its own: a 1:1 chat's "Start a group chat" (the group chat of those people opened, or a new one its first message
//! makes) and a group chat's "Add people" with its history choices. Synthetic accounts and chats only.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

pub mod common;
#[path = "common/parts.rs"]
mod parts;

use std::sync::Arc;

use common::{LAB_A, LAB_B, Rig};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use teams_core::{Action, ChatHistory, ChatListing};
use teams_proto::FoundPerson;
use teams_store::{Conversation, ConversationKind, Member, Message, OutboxKind, OutboxState};
use teams_ui::ViewTrace;
use teams_ui::chat_people::{ChatPeople, HistoryChoice, NOT_ADDED, PeopleOf, Picked};
use teams_ui::new_chat::NewChat;
use teams_ui::settings::Settings;
use teams_ui::shell::{Main, Shell};

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
/// Someone the directory finds, in no chat of a's.
const LAB_D: &str = "8:orgid:00000000-0000-4000-8000-00000000000d";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const AC: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000c@unq.gbl.spaces";
/// The a–b–c chat everyone wrote in.
const TALK: &str = "19:0000000000000000000000000000talk@thread.v2";
/// A newer a–b–c chat holding only a call: the one found for a, b and c.
const CALL: &str = "19:0000000000000000000000000000call@thread.v2";
/// The chat a first message makes.
const MADE: &str = "19:new-synthetic-group@thread.v2";
/// 2026-01-01T12:00:00Z, ms.
const NOON: i64 = 1_767_268_800_000;
const MINUTE: i64 = 60_000;

fn chat(id: &str, kind: ConversationKind, last: i64) -> Conversation {
    Conversation {
        id: id.to_owned(),
        kind,
        topic: None,
        version: 1,
        last_activity: Some(last),
    }
}

fn message(conversation: &str, id: &str, sender: &str, kind: &str, content: &str, at: i64) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: kind.to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: at,
        compose_time: Some(at),
        edit_time: None,
        delete_time: None,
    }
}

/// a's store: the a–b and a–c 1:1 chats, the a–b–c chat with text and the call-only a–b–c chat after it; the
/// directory finds d.
fn seeded() -> Rig {
    let rig = Rig::start();
    let core = &rig.core;
    core.conversations.lock().unwrap().extend([
        chat(AB, ConversationKind::OneToOne, NOON + 5 * MINUTE),
        chat(TALK, ConversationKind::Group, NOON + 4 * MINUTE),
        chat(AC, ConversationKind::OneToOne, NOON + 2 * MINUTE),
        chat(CALL, ConversationKind::Group, NOON + MINUTE),
    ]);
    let mut messages = core.messages.lock().unwrap();
    for (id, at, sender, kind, content) in [
        (AB, "1", LAB_B, "RichText/Html", "<p>hi a</p>"),
        (AC, "2", LAB_C, "RichText/Html", "<p>hi</p>"),
        (TALK, "3", LAB_C, "RichText/Html", "<p>all three</p>"),
        (CALL, "4", LAB_A, "Event/Call", "<ended/>"),
    ] {
        messages.insert(id.to_owned(), vec![message(id, at, sender, kind, content, NOON)]);
    }
    drop(messages);
    let mut stored = core.members.lock().unwrap();
    stored.insert(AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]);
    stored.insert(AC.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_C, None)]);
    for group in [TALK, CALL] {
        stored.insert(
            group.to_owned(),
            [LAB_A, LAB_B, LAB_C].iter().map(|mri| Member::new(mri, None)).collect(),
        );
    }
    drop(stored);
    *core.listing.lock().unwrap() = ChatListing {
        favorites: None,
        empty: vec![CALL.to_owned()],
    };
    let mut names = core.names.lock().unwrap();
    names.insert(LAB_A.to_owned(), "Lab Ay".to_owned());
    names.insert(LAB_B.to_owned(), "Lab Bee".to_owned());
    names.insert(LAB_C.to_owned(), "Lab Sea".to_owned());
    drop(names);
    core.service_answer.lock().unwrap().people = vec![FoundPerson {
        mri: Some(LAB_D.to_owned()),
        name: "Dee Synthetic".to_owned(),
        email: Some("dee@lab.example.test".to_owned()),
        upn: None,
        addresses: Vec::new(),
        job_title: Some("Analyst".to_owned()),
        department: None,
    }];
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

fn main(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Main {
    cx.update(|_, cx| shell.read(cx).main().clone())
}

fn open(shell: &Entity<Shell>, cx: &mut VisualTestContext, id: &str) {
    shell.update_in(cx, |shell, window, cx| shell.open_chat(id.to_owned(), window, cx));
    draw(cx);
}

fn people(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<ChatPeople> {
    cx.update(|_, cx| shell.read(cx).people().cloned())
        .expect("the people flyout is open")
}

/// Types `query` in the picker's box and takes the first suggestion.
fn take(cx: &mut VisualTestContext, query: &str) {
    cx.simulate_input(query);
    // the directory is asked a moment after the typing stops
    cx.executor().advance_clock(std::time::Duration::from_millis(300));
    draw(cx);
    cx.simulate_keystrokes("enter");
    draw(cx);
}

fn picked(view: &Entity<ChatPeople>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.update(|_, cx| {
        view.read(cx)
            .picked()
            .iter()
            .map(|picked| match picked {
                Picked::Person(person) => person.mri.clone(),
                Picked::Chat(row) => row.id.clone(),
            })
            .collect()
    })
}

/// A 1:1 chat's picker, just opened: no history choices, nothing offered with nothing typed and Create off, and b, in
/// the chat already, never offered.
fn picker_offers_only_newcomers(view: &Entity<ChatPeople>, cx: &mut VisualTestContext) {
    assert!(cx.debug_bounds("chat-people-picker").is_some());
    assert!(
        cx.debug_bounds("chat-people-history").is_none(),
        "no history to share in a new chat"
    );
    assert!(cx.update(|_, cx| view.read(cx).suggestions().is_empty()));
    assert!(!cx.update(|_, cx| view.read(cx).can_confirm(cx)));
    cx.simulate_input("Bee");
    draw(cx);
    let offered = cx.update(|_, cx| view.read(cx).suggestions().len());
    assert!(
        cx.update(|_, cx| view
            .read(cx)
            .suggestions()
            .iter()
            .all(|s| !matches!(s, Picked::Person(p) if p.mri == LAB_B))),
        "{offered}"
    );
    view.update_in(cx, |view, window, cx| view.set_query("", window, cx));
}

#[test]
fn a_one_to_one_chat_s_people_list_both_and_offer_to_start_a_group_chat() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, AB);
    click(cx, "chat-header-people");
    let view = people(&shell, cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).of()), PeopleOf::OneToOne);
    let members: Vec<String> = cx.update(|_, cx| view.read(cx).members().iter().map(|p| p.mri.clone()).collect());
    assert_eq!(members, [LAB_A, LAB_B], "the account first");
    assert!(cx.debug_bounds("chat-people-member-1").is_some());
    assert!(
        cx.debug_bounds("chat-people-add").is_none(),
        "a 1:1 chat starts a group instead"
    );
    click(cx, "chat-people-start-group");
    picker_offers_only_newcomers(&view, cx);
    take(cx, "Sea");
    assert_eq!(picked(&view, cx), [LAB_C]);
    assert!(cx.debug_bounds("chat-people-chip-0").is_some());
    // Backspace in the empty box takes the chip off; c again
    cx.simulate_keystrokes("backspace");
    draw(cx);
    assert!(picked(&view, cx).is_empty());
    take(cx, "Sea");
    // Create: a, b and c have an unnamed group chat, the call-only one listed last: it opens, nothing is written
    click(cx, "chat-people-confirm");
    assert_eq!(main(&shell, cx), Main::Chat(CALL.to_owned()));
    assert!(
        cx.update(|_, cx| shell.read(cx).people().is_none()),
        "the flyout closed"
    );
    assert!(rig.core.actions.lock().unwrap().is_empty(), "nothing written");
}

/// The new group chat of d and b not made yet: d, then the 1:1 chat's partner; its header in place of To:, its row
/// named after them, and its own people flyout.
fn draft_of_d_and_b(new_chat: &Entity<NewChat>, cx: &mut VisualTestContext) {
    assert!(cx.update(|_, cx| new_chat.read(cx).drafting()));
    let people: Vec<String> = cx.update(|_, cx| new_chat.read(cx).people().into_iter().map(str::to_owned).collect());
    assert_eq!(people, [LAB_D, LAB_B], "those added, then the 1:1 chat's partner");
    assert!(cx.debug_bounds("new-chat-draft-header").is_some());
    assert!(cx.debug_bounds("new-chat-to").is_none(), "no To: line");
    assert!(cx.debug_bounds("chat-row-new-group").is_some());
    let row = cx.update(|_, cx| new_chat.read(cx).draft_row()).unwrap();
    assert_eq!(row.name.as_ref(), "Dee and Lab");
    assert_eq!(row.members, 3);
    click(cx, "chat-header-people");
    let flyout = cx.update(|_, cx| new_chat.read(cx).people_flyout().cloned()).unwrap();
    assert_eq!(cx.update(|_, cx| flyout.read(cx).of()), PeopleOf::Draft);
    assert_eq!(cx.update(|_, cx| flyout.read(cx).members().len()), 3);
    click(cx, "chat-header-people");
}

#[test]
fn people_with_no_group_chat_get_a_new_one_its_first_message_makes_partner_last() {
    let rig = seeded();
    let mut made = common::outbox_row(41, OutboxKind::CreateChat, OutboxState::Sent);
    made.message_id = Some(MADE.to_owned());
    rig.core.write_answers.lock().unwrap().push_back(made);
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, AB);
    click(cx, "chat-header-people");
    click(cx, "chat-people-start-group");
    take(cx, "Dee");
    let view = people(&shell, cx);
    assert_eq!(picked(&view, cx), [LAB_D], "found by the directory");
    click(cx, "chat-people-confirm");
    // no chat of a, b and d: a new group chat not made yet, in New message's place, its row named after them
    assert_eq!(main(&shell, cx), Main::NewChat);
    let new_chat = cx.update(|_, cx| shell.read(cx).new_chat().cloned()).unwrap();
    draft_of_d_and_b(&new_chat, cx);
    assert!(rig.core.actions.lock().unwrap().is_empty(), "nothing written yet");
    // the first message makes the chat, then goes to it
    let compose = cx.update(|_, cx| new_chat.read(cx).compose(cx));
    compose.update_in(cx, |compose, window, cx| compose.focus(window, cx));
    cx.simulate_input("hello both");
    cx.simulate_keystrokes("enter");
    draw(cx);
    let actions = rig.core.actions.lock().unwrap().clone();
    let [Action::CreateChat { others, topic }, Action::Send { conversation, .. }] = actions.as_slice() else {
        panic!("the chat made, then the message sent: {actions:?}");
    };
    let others: Vec<&str> = others.iter().map(teams_proto::Mri::as_str).collect();
    assert_eq!(others, [LAB_D, LAB_B]);
    assert_eq!(*topic, None);
    assert_eq!(conversation, MADE);
    assert_eq!(main(&shell, cx), Main::Chat(MADE.to_owned()));
    assert!(cx.debug_bounds("chat-row-new-group").is_none());
}

/// a–b's "Start a group chat" with d, "half a thought" typed in its box, then the a–c chat opened: the new group chat
/// not made yet.
fn group_draft_left(rig: &Rig) -> (Entity<Shell>, Entity<NewChat>, &'static mut VisualTestContext) {
    let (shell, cx) = shell_window(rig);
    open(&shell, cx, AB);
    click(cx, "chat-header-people");
    click(cx, "chat-people-start-group");
    take(cx, "Dee");
    click(cx, "chat-people-confirm");
    let new_chat = cx.update(|_, cx| shell.read(cx).new_chat().cloned()).unwrap();
    let compose = cx.update(|_, cx| new_chat.read(cx).compose(cx));
    compose.update_in(cx, |compose, window, cx| compose.focus(window, cx));
    cx.simulate_input("half a thought");
    draw(cx);
    open(&shell, cx, AC);
    (shell, new_chat, cx)
}

#[test]
fn a_new_group_chat_left_for_another_chat_keeps_its_row_and_draft_and_its_first_message_makes_it() {
    let rig = seeded();
    let mut made = common::outbox_row(41, OutboxKind::CreateChat, OutboxState::Sent);
    made.message_id = Some(MADE.to_owned());
    rig.core.write_answers.lock().unwrap().push_back(made);
    let (shell, new_chat, cx) = group_draft_left(&rig);
    assert_eq!(main(&shell, cx), Main::Chat(AC.to_owned()));
    // as Teams keeps it: its row in the chats, named after its people
    assert!(
        cx.debug_bounds("chat-row-draft-0").is_some(),
        "the new chat's row stays"
    );
    let row = cx.update(|_, cx| new_chat.read(cx).kept_row(cx)).expect("a row");
    assert_eq!(row.name.as_ref(), "Dee and Lab");
    assert!(rig.core.actions.lock().unwrap().is_empty(), "nothing written");
    click(cx, "chat-row-draft-0");
    assert_eq!(main(&shell, cx), Main::NewChat);
    let shown = cx.update(|_, cx| shell.read(cx).new_chat().cloned()).unwrap();
    assert_eq!(shown.entity_id(), new_chat.entity_id(), "the same new chat");
    assert!(
        cx.debug_bounds("chat-row-draft-0").is_none(),
        "its row is the one selected now"
    );
    draft_of_d_and_b(&shown, cx);
    let compose = cx.update(|_, cx| shown.read(cx).compose(cx));
    assert_eq!(cx.update(|_, cx| compose.read(cx).value(cx)).trim(), "half a thought");
    // its first message makes the chat with the draft's text
    compose.update_in(cx, |compose, window, cx| compose.focus(window, cx));
    cx.simulate_keystrokes("enter");
    draw(cx);
    let actions = rig.core.actions.lock().unwrap().clone();
    let [Action::CreateChat { .. }, Action::Send { conversation, text }] = actions.as_slice() else {
        panic!("the chat made, then the message sent: {actions:?}");
    };
    assert_eq!(conversation, MADE);
    assert_eq!(text.html(), "<p>half a thought</p>");
    assert_eq!(main(&shell, cx), Main::Chat(MADE.to_owned()));
    assert!(cx.debug_bounds("chat-row-new-group").is_none());
    assert!(cx.debug_bounds("chat-row-draft-0").is_none());
}

#[test]
fn a_new_group_chat_s_first_message_given_back_while_it_was_left_has_no_row_and_ctrl_n_shows_it_typing() {
    let rig = seeded();
    rig.core.write_answers.lock().unwrap().push_back(common::outbox_row(
        41,
        OutboxKind::CreateChat,
        OutboxState::Pending,
    ));
    rig.core.outbox_answers.lock().unwrap().push_back(common::outbox_row(
        41,
        OutboxKind::CreateChat,
        OutboxState::Failed,
    ));
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, AB);
    click(cx, "chat-header-people");
    click(cx, "chat-people-start-group");
    take(cx, "Dee");
    click(cx, "chat-people-confirm");
    let new_chat = cx.update(|_, cx| shell.read(cx).new_chat().cloned()).unwrap();
    let compose = cx.update(|_, cx| new_chat.read(cx).compose(cx));
    compose.update_in(cx, |compose, window, cx| compose.focus(window, cx));
    cx.simulate_input("hello both");
    cx.simulate_keystrokes("enter");
    draw(cx);
    open(&shell, cx, AC);
    cx.executor().advance_clock(std::time::Duration::from_secs(1));
    draw(cx);
    assert!(cx.update(|_, cx| new_chat.read(cx).keeps_going()), "given back");
    assert!(
        cx.debug_bounds("chat-row-draft-0").is_none(),
        "a first message given back has no row"
    );
    // New message shows it again, its box (it has no To: line) taking the keyboard
    cx.simulate_keystrokes("ctrl-n");
    draw(cx);
    let shown = cx.update(|_, cx| shell.read(cx).new_chat().cloned()).unwrap();
    assert_eq!(shown.entity_id(), new_chat.entity_id());
    assert_eq!(cx.update(|_, cx| compose.read(cx).value(cx)).trim(), "hello both");
    assert!(cx.update(|window, cx| compose.read(cx).focused(window, cx)));
}

#[test]
fn a_new_group_chat_s_discard_takes_its_row_and_draft_away() {
    let rig = seeded();
    let (shell, new_chat, cx) = group_draft_left(&rig);
    let row = cx.debug_bounds("chat-row-draft-0").expect("the new chat's row");
    cx.simulate_mouse_move(row.center(), None, Modifiers::none());
    draw(cx);
    click(cx, "chat-row-draft-0-more");
    click(cx, "chat-row-draft-discard");
    assert!(cx.debug_bounds("chat-row-draft-0").is_none(), "the row is gone");
    assert_eq!(main(&shell, cx), Main::Chat(AC.to_owned()), "the chat shown stays");
    assert!(cx.update(|_, cx| shell.read(cx).kept_new_chats(cx).is_empty()));
    assert!(!cx.update(|_, cx| shell.read(cx).has_unsent(cx)), "no draft left");
    // New message opens a new one, its box empty
    cx.simulate_keystrokes("ctrl-n");
    draw(cx);
    let fresh = cx.update(|_, cx| shell.read(cx).new_chat().cloned()).unwrap();
    assert_ne!(fresh.entity_id(), new_chat.entity_id());
    assert!(!cx.update(|_, cx| fresh.read(cx).drafting()));
    assert!(rig.core.actions.lock().unwrap().is_empty(), "nothing written");
}

#[test]
fn a_kept_new_group_chat_s_row_answers_the_keyboard_and_back_returns_to_it() {
    let rig = seeded();
    let (shell, new_chat, cx) = group_draft_left(&rig);
    let sidebar = cx.update(|_, cx| shell.read(cx).sidebar().clone());
    let shown = |shell: &Entity<Shell>, cx: &mut VisualTestContext| {
        cx.update(|_, cx| shell.read(cx).new_chat().map(Entity::entity_id))
    };
    // the row under the chats' header: Enter opens it
    cx.update(|window, cx| sidebar.update(cx, |sidebar, cx| sidebar.focus_list(window, cx)));
    cx.simulate_keystrokes("home down enter");
    draw(cx);
    assert_eq!(shown(&shell, cx), Some(new_chat.entity_id()));
    // another chat, then Back: the new chat as it was left
    open(&shell, cx, AC);
    shell.update_in(cx, Shell::go_back);
    draw(cx);
    assert_eq!(main(&shell, cx), Main::NewChat);
    assert_eq!(shown(&shell, cx), Some(new_chat.entity_id()));
    // Delete on its row discards it
    open(&shell, cx, AC);
    cx.update(|window, cx| sidebar.update(cx, |sidebar, cx| sidebar.focus_list(window, cx)));
    cx.simulate_keystrokes("home down delete");
    draw(cx);
    assert!(cx.debug_bounds("chat-row-draft-0").is_none());
    assert!(cx.update(|_, cx| shell.read(cx).kept_new_chats(cx).is_empty()));
    assert_eq!(main(&shell, cx), Main::Chat(AC.to_owned()));
}

/// The kept new group chat is shown, its box has the keyboard, and what the user types goes into its draft.
fn shown_with_box_focused(shell: &Entity<Shell>, new_chat: &Entity<NewChat>, cx: &mut VisualTestContext, way: &str) {
    assert_eq!(main(shell, cx), Main::NewChat, "{way}");
    let shown = cx.update(|_, cx| shell.read(cx).new_chat().map(Entity::entity_id));
    assert_eq!(shown, Some(new_chat.entity_id()), "{way}");
    let compose = cx.update(|_, cx| new_chat.read(cx).compose(cx));
    assert!(
        cx.update(|window, cx| compose.read(cx).focused(window, cx)),
        "{way}: its box has the keyboard"
    );
    let before = cx.update(|_, cx| compose.read(cx).value(cx)).trim().to_owned();
    cx.simulate_input("+");
    draw(cx);
    let after = cx.update(|_, cx| compose.read(cx).value(cx)).trim().to_owned();
    assert_eq!(after, format!("{before}+"), "{way}: typed text goes to its draft");
}

#[test]
fn each_way_back_to_a_kept_new_group_chat_gives_its_box_the_keyboard() {
    let rig = seeded();
    let (shell, new_chat, cx) = group_draft_left(&rig);
    let sidebar = cx.update(|_, cx| shell.read(cx).sidebar().clone());
    click(cx, "chat-row-draft-0");
    shown_with_box_focused(&shell, &new_chat, cx, "its row's click");
    open(&shell, cx, AC);
    cx.update(|window, cx| sidebar.update(cx, |sidebar, cx| sidebar.focus_list(window, cx)));
    cx.simulate_keystrokes("home down enter");
    draw(cx);
    shown_with_box_focused(&shell, &new_chat, cx, "Enter on its row");
    open(&shell, cx, AC);
    shell.update_in(cx, Shell::go_back);
    draw(cx);
    shown_with_box_focused(&shell, &new_chat, cx, "the Back arrow");
    // the keys of the history go on from its box
    cx.simulate_keystrokes("alt-right");
    draw(cx);
    assert_eq!(main(&shell, cx), Main::Chat(AC.to_owned()), "Alt+Right from its box");
    cx.simulate_keystrokes("alt-left");
    draw(cx);
    shown_with_box_focused(&shell, &new_chat, cx, "Alt+Left");
    cx.simulate_keystrokes("alt-right");
    draw(cx);
    cx.simulate_keystrokes("alt-left");
    draw(cx);
    cx.simulate_keystrokes("alt-left");
    draw(cx);
    assert!(matches!(main(&shell, cx), Main::Chat(_)), "Alt+Left from its box");
    cx.simulate_keystrokes("alt-right");
    draw(cx);
    shown_with_box_focused(&shell, &new_chat, cx, "Alt+Right");
    shell.update_in(cx, Shell::open_settings);
    draw(cx);
    shell.update_in(cx, Shell::go_back);
    draw(cx);
    shown_with_box_focused(&shell, &new_chat, cx, "Back from Settings");
    assert_eq!(
        cx.update(|_, cx| new_chat.read(cx).compose(cx).read(cx).value(cx))
            .trim(),
        "half a thought++++++"
    );
}

#[test]
fn a_group_chat_s_add_people_sends_them_with_the_history_chosen() {
    let rig = seeded();
    let added = common::outbox_row(7, OutboxKind::AddMembers, OutboxState::Sent);
    rig.core.write_answers.lock().unwrap().push_back(added);
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, TALK);
    // the button counts the chat's people
    let button = cx.debug_bounds("chat-header-people").expect("the people button");
    cx.simulate_click(button.center(), Modifiers::none());
    draw(cx);
    let view = people(&shell, cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).members().len()), 3);
    assert!(cx.debug_bounds("chat-people-start-group").is_none());
    click(cx, "chat-people-add");
    assert!(cx.debug_bounds("chat-people-history").is_some());
    assert_eq!(cx.update(|_, cx| view.read(cx).history()), HistoryChoice::None);
    // everyone in the chat already: nobody offered
    cx.simulate_input("Sea");
    draw(cx);
    assert!(cx.update(|_, cx| {
        view.read(cx)
            .suggestions()
            .iter()
            .all(|s| !matches!(s, Picked::Person(_)))
    }));
    view.update_in(cx, |view, window, cx| view.set_query("", window, cx));
    take(cx, "Dee");
    // the past days: digits only, and none or 0 turns Add off
    click(cx, "chat-people-history-days");
    assert_eq!(cx.update(|_, cx| view.read(cx).history()), HistoryChoice::Days);
    for (typed, lit) in [("0", false), ("", false), ("x7", true)] {
        view.update_in(cx, |view, window, cx| view.set_days(typed, window, cx));
        draw(cx);
        assert_eq!(cx.update(|_, cx| view.read(cx).can_confirm(cx)), lit, "{typed:?}");
    }
    click(cx, "chat-people-confirm");
    let actions = rig.core.actions.lock().unwrap().clone();
    let [
        Action::AddMembers {
            conversation,
            members,
            history,
        },
    ] = actions.as_slice()
    else {
        panic!("one add: {actions:?}");
    };
    assert_eq!(conversation, TALK);
    let members: Vec<&str> = members.iter().map(teams_proto::Mri::as_str).collect();
    assert_eq!(members, [LAB_D]);
    assert_eq!(*history, ChatHistory::Days(7));
    // went out: the flyout closes, the chat stays
    assert!(cx.update(|_, cx| shell.read(cx).people().is_none()));
    assert_eq!(main(&shell, cx), Main::Chat(TALK.to_owned()));
}

#[test]
fn an_add_that_did_not_go_out_says_so_and_keeps_the_people() {
    let rig = seeded();
    let failed = common::outbox_row(7, OutboxKind::AddMembers, OutboxState::Failed);
    rig.core.write_answers.lock().unwrap().push_back(failed);
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, TALK);
    click(cx, "chat-header-people");
    click(cx, "chat-people-add");
    take(cx, "Dee");
    click(cx, "chat-people-history-all");
    click(cx, "chat-people-confirm");
    let view = people(&shell, cx);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).problem().cloned()),
        Some(NOT_ADDED.into())
    );
    assert_eq!(picked(&view, cx), [LAB_D]);
    assert!(cx.debug_bounds("chat-people-problem").is_some());
    let actions = rig.core.actions.lock().unwrap().clone();
    assert!(
        matches!(
            actions.as_slice(),
            [Action::AddMembers {
                history: ChatHistory::All,
                ..
            }]
        ),
        "{actions:?}"
    );
    // Escape closes the picker, then the flyout
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!cx.update(|_, cx| view.read(cx).picking()));
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(cx.update(|_, cx| shell.read(cx).people().is_none()));
}

#[test]
fn the_read_only_mode_lists_the_people_and_offers_no_add() {
    let rig = seeded();
    *rig.core.read_only.lock().unwrap() = true;
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, TALK);
    click(cx, "chat-header-people");
    assert!(cx.debug_bounds("chat-people").is_some());
    assert!(cx.debug_bounds("chat-people-member-2").is_some());
    assert!(cx.debug_bounds("chat-people-add").is_none());
    open(&shell, cx, AB);
    assert!(
        cx.update(|_, cx| shell.read(cx).people().is_none()),
        "another chat closes it"
    );
    click(cx, "chat-header-people");
    assert!(cx.debug_bounds("chat-people-start-group").is_none());
    assert!(rig.core.actions.lock().unwrap().is_empty());
}

/// A 1:1 chat's picker: a suggestion clicked under the picker's own box (the suggestions hang out of it) is taken,
/// and the flyout and its picker stay open, in a window too short for them where they hang; a press away from them
/// still closes the flyout.
#[test]
fn a_suggestion_clicked_below_the_picker_is_taken_and_keeps_the_flyout_open() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, AB);
    click(cx, "chat-header-people");
    click(cx, "chat-people-start-group");
    let view = people(&shell, cx);
    cx.simulate_input("Sea");
    cx.executor().advance_clock(std::time::Duration::from_millis(300));
    draw(cx);
    let offered: Vec<String> = cx.update(|_, cx| {
        view.read(cx)
            .suggestions()
            .iter()
            .filter_map(|s| match s {
                Picked::Person(p) => Some(p.mri.clone()),
                Picked::Chat(_) => None,
            })
            .collect()
    });
    assert_eq!(offered, [LAB_C, LAB_D], "c's 1:1 chat, then the directory's d");
    // a short window: the picker alone would fit, its suggestions not, so it is pulled up with them
    cx.simulate_resize(size(px(1200.0), px(460.0)));
    draw(cx);
    let suggestions = cx.debug_bounds("chat-people-suggestions").expect("the suggestions");
    assert!(suggestions.bottom() <= px(460.0), "in the window: {suggestions:?}");
    let picker = cx.debug_bounds("chat-people-picker").expect("the picker");
    let second = cx
        .debug_bounds("chat-people-suggestion-1")
        .expect("the second suggestion");
    assert!(second.center().y > picker.bottom(), "{second:?} hangs under {picker:?}");
    cx.simulate_click(second.center(), Modifiers::none());
    draw(cx);
    assert!(cx.update(|_, cx| shell.read(cx).people().is_some()), "the flyout stays");
    assert!(cx.update(|_, cx| view.read(cx).picking()), "and its picker");
    assert_eq!(picked(&view, cx), [LAB_D]);
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    draw(cx);
    cx.simulate_click(gpui::point(px(400.0), px(700.0)), Modifiers::none());
    draw(cx);
    assert!(
        cx.update(|_, cx| shell.read(cx).people().is_none()),
        "a press away closes it"
    );
}

/// A group chat of 21 people, its flyout scrolled to its end: Add's picker hangs from the action where it is drawn
/// and stays in a short window.
#[test]
fn a_large_group_chat_s_picker_hangs_from_its_action_inside_the_window() {
    const BIG: &str = "19:00000000000000000000000000000big@thread.v2";
    let rig = seeded();
    rig.core
        .conversations
        .lock()
        .unwrap()
        .push(chat(BIG, ConversationKind::Group, NOON + 6 * MINUTE));
    rig.core.messages.lock().unwrap().insert(
        BIG.to_owned(),
        vec![message(BIG, "5", LAB_B, "RichText/Html", "<p>many</p>", NOON)],
    );
    let mut members = vec![Member::new(LAB_A, None)];
    members.extend((1..=20).map(|n| Member::new(&format!("8:orgid:00000000-0000-4000-8000-0000000001{n:02}"), None)));
    rig.core.members.lock().unwrap().insert(BIG.to_owned(), members);
    let (shell, cx) = shell_window(&rig);
    cx.simulate_resize(size(px(1200.0), px(600.0)));
    draw(cx);
    open(&shell, cx, BIG);
    click(cx, "chat-header-people");
    let view = people(&shell, cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).members().len()), 21);
    let flyout = cx.debug_bounds("chat-people").expect("the flyout");
    cx.simulate_event(gpui::ScrollEvent {
        position: flyout.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-3000.0))),
        touch_phase: gpui::TouchPhase::Moved,
        ..gpui::ScrollEvent::default()
    });
    draw(cx);
    let add = cx.debug_bounds("chat-people-add").expect("Add people");
    assert!(
        add.bottom() <= flyout.bottom(),
        "scrolled into view: {add:?} in {flyout:?}"
    );
    click(cx, "chat-people-add");
    let picker = cx.debug_bounds("chat-people-picker").expect("the picker");
    assert!(
        picker.top() >= px(0.0) && picker.bottom() <= px(600.0),
        "in the window: {picker:?}"
    );
    assert!(
        picker.top() <= add.bottom(),
        "pulled up to stay in it: {picker:?} under {add:?}"
    );
    assert!(
        picker.right() > flyout.left() && picker.left() < flyout.left(),
        "over the flyout's left: {picker:?}, {flyout:?}"
    );
    // a tall window: it hangs right under the action
    cx.simulate_resize(size(px(1200.0), px(1000.0)));
    draw(cx);
    let add = cx.debug_bounds("chat-people-add").expect("Add people");
    let picker = cx.debug_bounds("chat-people-picker").expect("the picker");
    assert!(
        (picker.top() - add.bottom()).abs() <= px(8.0),
        "under the action: {picker:?}, {add:?}"
    );
    // its controls take clicks there
    click(cx, "chat-people-history-all");
    assert_eq!(cx.update(|_, cx| view.read(cx).history()), HistoryChoice::All);
}

/// A 1:1 chat the store lists no members for: its people are the account and the partner its id names, and a group
/// chat started from it still ends with that partner.
#[test]
fn a_one_to_one_chat_without_stored_members_still_starts_the_group_with_its_partner() {
    let rig = seeded();
    rig.core.members.lock().unwrap().remove(AC);
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, AC);
    click(cx, "chat-header-people");
    let view = people(&shell, cx);
    let members: Vec<String> = cx.update(|_, cx| view.read(cx).members().iter().map(|p| p.mri.clone()).collect());
    assert_eq!(members, [LAB_A, LAB_C], "the partner the chat names");
    click(cx, "chat-people-start-group");
    take(cx, "Dee");
    assert_eq!(picked(&view, cx), [LAB_D]);
    click(cx, "chat-people-confirm");
    assert_eq!(main(&shell, cx), Main::NewChat);
    let new_chat = cx.update(|_, cx| shell.read(cx).new_chat().cloned()).unwrap();
    let people: Vec<String> = cx.update(|_, cx| new_chat.read(cx).people().into_iter().map(str::to_owned).collect());
    assert_eq!(
        people,
        [LAB_D, LAB_C],
        "a new group chat of a, d and c, never a's 1:1 chat with d"
    );
    let row = cx.update(|_, cx| new_chat.read(cx).draft_row()).unwrap();
    assert_eq!(row.name.as_ref(), "Dee and Lab");
}

/// A new group chat's people while its first message makes the chat: its people flyout closes and doesn't open
/// again, and nobody comes off; the chat is made with the people shown.
#[test]
fn a_new_group_chat_s_people_are_locked_while_its_first_message_makes_it() {
    let rig = seeded();
    // the chat's creation stays under way until the test settles it
    rig.core.write_answers.lock().unwrap().push_back(common::outbox_row(
        41,
        OutboxKind::CreateChat,
        OutboxState::Pending,
    ));
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, AB);
    click(cx, "chat-header-people");
    click(cx, "chat-people-start-group");
    take(cx, "Dee");
    click(cx, "chat-people-confirm");
    let new_chat = cx.update(|_, cx| shell.read(cx).new_chat().cloned()).unwrap();
    click(cx, "chat-header-people");
    assert!(cx.update(|_, cx| new_chat.read(cx).people_flyout().is_some()));
    let compose = cx.update(|_, cx| new_chat.read(cx).compose(cx));
    compose.update_in(cx, |compose, window, cx| compose.focus(window, cx));
    cx.simulate_input("hello both");
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert!(cx.update(|_, cx| new_chat.read(cx).is_starting()));
    assert!(
        cx.update(|_, cx| new_chat.read(cx).people_flyout().is_none()),
        "the flyout closed"
    );
    click(cx, "chat-header-people");
    new_chat.update_in(cx, |new_chat, window, cx| {
        if new_chat.people_flyout().is_none() {
            new_chat.toggle_people(window, cx);
        }
    });
    draw(cx);
    assert!(
        cx.update(|_, cx| new_chat.read(cx).people_flyout().is_none()),
        "no people to add to meanwhile"
    );
    new_chat.update_in(cx, |new_chat, window, cx| new_chat.remove(0, window, cx));
    let people = |cx: &mut VisualTestContext| -> Vec<String> {
        cx.update(|_, cx| new_chat.read(cx).people().into_iter().map(str::to_owned).collect())
    };
    assert_eq!(people(cx), [LAB_D, LAB_B], "nobody comes off");
    // made: the message goes to it
    let mut made = common::outbox_row(41, OutboxKind::CreateChat, OutboxState::Sent);
    made.message_id = Some(MADE.to_owned());
    rig.core.outbox_answers.lock().unwrap().push_back(made);
    cx.executor().advance_clock(std::time::Duration::from_secs(1));
    draw(cx);
    let actions = rig.core.actions.lock().unwrap().clone();
    let [Action::CreateChat { others, .. }, Action::Send { conversation, .. }] = actions.as_slice() else {
        panic!("the chat made, then the message sent: {actions:?}");
    };
    let others: Vec<&str> = others.iter().map(teams_proto::Mri::as_str).collect();
    assert_eq!(others, [LAB_D, LAB_B]);
    assert_eq!(conversation, MADE);
}

/// The people the open flyout shows, after the account.
fn shown_people(view: &Entity<ChatPeople>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.update(|_, cx| {
        let members = view.read(cx).members();
        members.iter().skip(1).map(|person| person.mri.clone()).collect()
    })
}

/// A group chat's open flyout follows its people as they change while it is open (someone added on another device,
/// someone gone), as Teams' does, without being closed and opened again; a change of another chat leaves it be.
#[test]
fn an_open_group_chat_s_people_follow_a_change_of_its_people_live() {
    let rig = seeded();
    rig.core
        .names
        .lock()
        .unwrap()
        .insert(LAB_D.to_owned(), "Dee Synthetic".to_owned());
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, TALK);
    click(cx, "chat-header-people");
    let view = people(&shell, cx);
    assert_eq!(shown_people(&view, cx), [LAB_B, LAB_C]);
    assert!(cx.debug_bounds("chat-people-member-3").is_none());

    // d added: the store has the four, and the chat is told changed
    let set = |members: &[&str]| {
        let people = members.iter().map(|mri| Member::new(mri, None)).collect();
        rig.core.members.lock().unwrap().insert(TALK.to_owned(), people);
    };
    set(&[LAB_A, LAB_B, LAB_C, LAB_D]);
    rig.send(teams_core::Event::ConversationChanged {
        conversation: TALK.to_owned(),
    });
    draw(cx);
    assert_eq!(shown_people(&view, cx), [LAB_B, LAB_C, LAB_D]);
    assert!(cx.debug_bounds("chat-people-member-3").is_some());

    // c gone; a change of another chat alone doesn't read it again
    set(&[LAB_A, LAB_B, LAB_D]);
    rig.send(teams_core::Event::ConversationChanged {
        conversation: AB.to_owned(),
    });
    draw(cx);
    assert_eq!(shown_people(&view, cx), [LAB_B, LAB_C, LAB_D]);
    rig.send(teams_core::Event::ConversationChanged {
        conversation: TALK.to_owned(),
    });
    draw(cx);
    assert_eq!(shown_people(&view, cx), [LAB_B, LAB_D]);
    assert!(cx.update(|_, cx| shell.read(cx).people().is_some()), "still open");

    // a read again asks the presence of a newcomer, and not again of those it showed already (d, whom only the
    // flyout shows, was asked once over the two reads that showed d)
    let asked = rig.core.presence_asked.lock().unwrap().clone();
    assert_eq!(asked.iter().filter(|asked| *asked == LAB_D).count(), 1, "{asked:?}");
}

/// While an open flyout reads its people again, it keeps the people it showed and Add lit for those picked.
#[test]
fn a_read_again_keeps_the_people_shown_and_add_lit_until_it_ends() {
    let rig = seeded();
    rig.core
        .names
        .lock()
        .unwrap()
        .insert(LAB_D.to_owned(), "Dee Synthetic".to_owned());
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, TALK);
    click(cx, "chat-header-people");
    let view = people(&shell, cx);
    click(cx, "chat-people-add");
    take(cx, "Dee");
    assert_eq!(picked(&view, cx), [LAB_D]);
    let lit = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).can_confirm(cx));
    assert!(lit(cx));

    // c gone, and b's answer held: the read again doesn't end yet
    let people: Vec<Member> = [LAB_A, LAB_B].iter().map(|mri| Member::new(mri, None)).collect();
    rig.core.members.lock().unwrap().insert(TALK.to_owned(), people);
    rig.core.hold_people.lock().unwrap().insert(LAB_B.to_owned());
    rig.send(teams_core::Event::ConversationChanged {
        conversation: TALK.to_owned(),
    });
    draw(cx);
    let held: Vec<_> = rig.core.held.lock().unwrap().drain(..).collect();
    assert!(!held.is_empty(), "the read again waits for b");
    assert_eq!(shown_people(&view, cx), [LAB_B, LAB_C], "the people shown stay");
    assert!(lit(cx), "Add stays lit");

    rig.core.hold_people.lock().unwrap().clear();
    for (answer, person) in held {
        answer.send(Ok(person));
    }
    draw(cx);
    assert_eq!(shown_people(&view, cx), [LAB_B]);
    assert!(lit(cx));
}
