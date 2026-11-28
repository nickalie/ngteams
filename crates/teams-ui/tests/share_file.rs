//! "Share this file" (`teams_ui::share_file`), on the fake core in a shell window: Share on a file card opens it with
//! Share off; the recipients found as Teams finds them (people, chats, channels, "We couldn't find any matches."),
//! picked as chips; Share sends the file to each recipient as Teams does (one post each, the file's entry again, the
//! message as its text), making a person's 1:1 chat first when there is none; a recipient not reached stays picked;
//! Copy link and its "Link created", Share outside Teams, the format bar and emoji; the keys. Synthetic accounts, chats and files only (`tests/fixtures/chatsvc/files.json`).

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

pub mod common;
#[path = "common/parts.rs"]
mod parts;

use std::sync::Arc;

use common::{LAB_A, LAB_B, Rig};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use serde_json::Value;
use teams_core::Action;
use teams_proto::FoundPerson;
use teams_proto::chat::{LinkPermission, MessageText};
use teams_store::{
    ChannelKind, Conversation, ConversationKind, Member, Message, OutboxKind, OutboxState, Team, TeamChannel,
};
use teams_ui::ViewTrace;
use teams_ui::settings::Settings;
use teams_ui::share_file::{NO_MATCH, ShareFileDialog, Target};
use teams_ui::shell::Shell;

const FIXTURE: &str = include_str!("../../../tests/fixtures/chatsvc/files.json");
const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const LAB_D: &str = "8:orgid:00000000-0000-4000-8000-00000000000d";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const AC: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000c@unq.gbl.spaces";
const TALK: &str = "19:0000000000000000000000000000talk@thread.v2";
const TEAM: &str = "19:team0001@thread.tacv2";
const CHANNEL: &str = "19:channel0002@thread.tacv2";
/// The 1:1 chat the share makes with d.
const MADE: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000d@unq.gbl.spaces";
/// The message with the files, in the a–b chat.
const ID: &str = "301";
/// 2026-01-01T12:00:00Z, ms.
const NOON: i64 = 1_767_268_800_000;

fn chat(id: &str, kind: ConversationKind, topic: Option<&str>, last: i64) -> Conversation {
    Conversation {
        id: id.to_owned(),
        kind,
        topic: topic.map(str::to_owned),
        version: 1,
        last_activity: Some(last),
    }
}

fn message(conversation: &str, id: &str, sender: &str, content: &str, at: i64) -> Message {
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
        arrival_time: at,
        compose_time: Some(at),
        edit_time: None,
        delete_time: None,
    }
}

fn channel(id: &str, name: &str, general: bool) -> TeamChannel {
    TeamChannel {
        id: id.to_owned(),
        name: name.to_owned(),
        general,
        kind: ChannelKind::Standard,
        about: teams_store::ChannelAbout::default(),
        shown: true,
    }
}

/// a's store: the a–b chat with b's message of two files, the a–c chat, the a–b–c "Lab trio" chat, and a team with
/// a "Made-up Releases" channel; the file's link lets anyone edit it.
fn seeded() -> Rig {
    let rig = Rig::start();
    let core = &rig.core;
    core.conversations.lock().unwrap().extend([
        chat(AB, ConversationKind::OneToOne, None, NOON + 3),
        chat(AC, ConversationKind::OneToOne, None, NOON + 2),
        chat(TALK, ConversationKind::Group, Some("Lab trio"), NOON + 1),
    ]);
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let read_back = &fixture["read_back"];
    let mut files = message(AB, ID, LAB_B, read_back["content"].as_str().unwrap(), NOON);
    files.properties = Some(read_back["properties"].to_string());
    let mut messages = core.messages.lock().unwrap();
    messages.insert(AB.to_owned(), vec![files]);
    messages.insert(AC.to_owned(), vec![message(AC, "2", LAB_C, "<p>hi</p>", NOON)]);
    messages.insert(TALK.to_owned(), vec![message(TALK, "3", LAB_C, "<p>all</p>", NOON)]);
    drop(messages);
    let mut stored = core.members.lock().unwrap();
    stored.insert(AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]);
    stored.insert(AC.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_C, None)]);
    stored.insert(
        TALK.to_owned(),
        [LAB_A, LAB_B, LAB_C].iter().map(|mri| Member::new(mri, None)).collect(),
    );
    drop(stored);
    let mut names = core.names.lock().unwrap();
    names.insert(LAB_B.to_owned(), "Lab Bee".to_owned());
    names.insert(LAB_C.to_owned(), "Lab Sea".to_owned());
    drop(names);
    *core.teams.lock().unwrap() = Some(vec![Team {
        id: TEAM.to_owned(),
        name: "Made-up Team".to_owned(),
        archived: false,
        group_id: None,
        mailbox: None,
        owner: false,
        can_add_channel: false,
        can_add_private_channel: false,
        about: teams_store::TeamAbout::default(),
        channels: vec![
            channel(TEAM, "General", true),
            channel(CHANNEL, "Made-up Releases", false),
        ],
    }]);
    *core.link_permission.lock().unwrap() = Some(LinkPermission {
        scope: "anonymous".to_owned(),
        description: Some("Anyone with the link can edit".to_owned()),
    });
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
    cx.simulate_resize(size(px(1200.0), px(860.0)));
    draw(cx);
    (shell, cx)
}

fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<gpui::Bounds<gpui::Pixels>> {
    draw(cx);
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    let found = bounds(cx, selector).unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(found.center(), Modifiers::none());
    draw(cx);
}

fn shown_dialog(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Option<Entity<ShareFileDialog>> {
    cx.update(|_, cx| shell.read(cx).share_dialog().cloned())
}

/// The shell with the a–b chat open and "Share this file" opened from its first card's Share.
fn opened() -> (
    Rig,
    Entity<Shell>,
    Entity<ShareFileDialog>,
    &'static mut VisualTestContext,
) {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(AB.to_owned(), window, cx)));
    draw(cx);
    click(cx, &format!("file-share-{ID}-0"));
    let dialog = shown_dialog(&shell, cx).expect("Share opened the dialog");
    (rig, shell, dialog, cx)
}

/// Types `query` in the recipients' box and takes the first suggestion with Enter.
fn pick(cx: &mut VisualTestContext, query: &str) {
    cx.simulate_input(query);
    draw(cx);
    cx.simulate_keystrokes("enter");
    draw(cx);
}

fn picked(dialog: &Entity<ShareFileDialog>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.update(|_, cx| {
        dialog
            .read(cx)
            .picked()
            .iter()
            .map(|target| target.key().to_owned())
            .collect()
    })
}

/// The sends written, as (conversation, text).
fn sends(rig: &Rig) -> Vec<(String, MessageText)> {
    rig.core
        .actions
        .lock()
        .unwrap()
        .iter()
        .filter_map(|action| match action {
            Action::Send { conversation, text } => Some((conversation.clone(), text.clone())),
            _ => None,
        })
        .collect()
}

fn sent(id: i64, kind: OutboxKind, state: OutboxState, made: Option<&str>) -> teams_store::OutboxEntry {
    let mut row = common::outbox_row(id, kind, state);
    row.message_id = made.map(str::to_owned);
    row
}

#[test]
fn share_on_a_file_card_opens_share_this_file_with_share_off() {
    let (rig, _shell, dialog, cx) = opened();
    for selector in [
        "share-file-dialog",
        "share-file-title",
        "share-file-to",
        "share-file-preview",
        "share-file-card",
        "share-file-copy-link",
        "share-file-share",
    ] {
        assert!(bounds(cx, selector).is_some(), "{selector}");
    }
    assert!(
        !cx.update(|_, cx| dialog.read(cx).can_share()),
        "Share stays off with no recipient"
    );
    assert_eq!(
        cx.update(|_, cx| dialog.read(cx).permission().cloned())
            .and_then(|permission| permission.description)
            .as_deref(),
        Some("Anyone with the link can edit")
    );
    assert!(
        bounds(cx, "share-file-permission").is_some(),
        "the card says what the link lets people do"
    );
    // a click on Share with nobody picked sends nothing
    click(cx, "share-file-share");
    assert!(sends(&rig).is_empty());
}

#[test]
fn recipients_are_found_as_teams_finds_them() {
    let (_rig, shell, dialog, cx) = opened();
    cx.simulate_input("Lab");
    draw(cx);
    let found: Vec<String> = cx.update(|_, cx| {
        dialog
            .read(cx)
            .suggestions()
            .iter()
            .map(|target| target.key().to_owned())
            .collect()
    });
    assert_eq!(found, [LAB_B, LAB_C, TALK], "the 1:1 partners, then the chats");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(shown_dialog(&shell, cx).is_some(), "the first Escape clears the box");
    cx.simulate_input("Releases");
    draw(cx);
    let found = cx.update(|_, cx| dialog.read(cx).suggestions().to_vec());
    let [Target::Channel { id, team, .. }] = &found[..] else {
        panic!("the channel is found: {found:?}");
    };
    assert_eq!((id.as_str(), team.as_ref()), (CHANNEL, "Made-up Team"));
    cx.simulate_keystrokes("escape");
    cx.simulate_input("zzzz");
    cx.executor().advance_clock(std::time::Duration::from_millis(400));
    draw(cx);
    assert!(bounds(cx, "share-file-no-match").is_some(), "{NO_MATCH}");
}

#[test]
fn the_suggestions_are_over_the_message_preview() {
    let (_rig, _shell, dialog, cx) = opened();
    cx.simulate_input("Lab");
    draw(cx);
    let preview = bounds(cx, "share-file-preview").expect("the preview is drawn");
    let last = bounds(cx, "share-file-suggestion-2").expect("the third suggestion is drawn");
    assert!(
        preview.contains(&last.center()),
        "the list reaches over the preview: {last:?} in {preview:?}"
    );
    // the row takes the press, not the message's box under it: the keyboard stays in the recipients' box
    click(cx, "share-file-suggestion-2");
    assert_eq!(picked(&dialog, cx), [TALK]);
    cx.simulate_input("Sea");
    draw(cx);
    let found: Vec<String> = cx.update(|_, cx| {
        dialog
            .read(cx)
            .suggestions()
            .iter()
            .map(|target| target.key().to_owned())
            .collect()
    });
    assert!(
        found.iter().any(|key| key == LAB_C),
        "the next name typed goes to the recipients' box: {found:?}"
    );
}

#[test]
fn two_recipients_and_a_message_get_the_file_as_teams_shares_it() {
    let (rig, shell, dialog, cx) = opened();
    pick(cx, "Bee");
    pick(cx, "Releases");
    assert_eq!(picked(&dialog, cx), [LAB_B, CHANNEL]);
    assert!(bounds(cx, "share-file-chip-1").is_some());
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.set_message("Made-up note", cx)));
    rig.core.write_answers.lock().unwrap().extend([
        sent(1, OutboxKind::Send, OutboxState::Sent, None),
        sent(2, OutboxKind::Send, OutboxState::Sent, None),
    ]);
    click(cx, "share-file-share");
    let sends = sends(&rig);
    let conversations: Vec<&str> = sends.iter().map(|(conversation, _)| conversation.as_str()).collect();
    assert_eq!(conversations, [AB, CHANNEL], "one post to each recipient");
    for (_, text) in &sends {
        assert_eq!(text.html(), "<p>Made-up note</p>");
        let [entry] = text.shared() else {
            panic!("the file's entry once: {text:?}");
        };
        assert_eq!(entry["fileName"], "02-made-up-rehearsal-cut-v3.mp4");
        assert_eq!(entry["state"], Value::Null);
        assert_eq!(entry["permissionScope"], "anonymous");
        assert!(text.files().is_empty(), "nothing uploaded");
    }
    assert!(shown_dialog(&shell, cx).is_none(), "the dialog closed");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).toast().cloned()).as_deref(),
        Some("You've shared 02-made-up-rehearsal-cut-v3.mp4")
    );
}

#[test]
fn a_person_with_no_chat_yet_gets_one_first() {
    let (rig, _shell, dialog, cx) = opened();
    rig.core.service_answer.lock().unwrap().people = vec![FoundPerson {
        mri: Some(LAB_D.to_owned()),
        name: "Lab Dee".to_owned(),
        email: Some("dee@lab.example.com".to_owned()),
        upn: None,
        addresses: Vec::new(),
        job_title: None,
        department: None,
    }];
    cx.simulate_input("Dee");
    cx.executor().advance_clock(std::time::Duration::from_millis(400));
    draw(cx);
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(picked(&dialog, cx), [LAB_D]);
    rig.core.write_answers.lock().unwrap().extend([
        sent(1, OutboxKind::CreateChat, OutboxState::Sent, Some(MADE)),
        sent(2, OutboxKind::Send, OutboxState::Sent, None),
    ]);
    cx.simulate_keystrokes("tab enter");
    draw(cx);
    let actions = rig.core.actions.lock().unwrap().clone();
    let [
        Action::CreateChat { others, topic: None },
        Action::Send { conversation, text },
    ] = &actions[..]
    else {
        panic!("the chat made, then the file sent: {actions:?}");
    };
    assert_eq!(others.iter().map(teams_proto::Mri::as_str).collect::<Vec<_>>(), [LAB_D]);
    assert_eq!(conversation, MADE);
    assert_eq!(text.html(), "", "no message: no text");
}

#[test]
fn a_recipient_not_reached_stays_picked_and_the_dialog_says_so() {
    let (rig, shell, dialog, cx) = opened();
    pick(cx, "Bee");
    pick(cx, "Sea");
    rig.core.write_answers.lock().unwrap().extend([
        sent(1, OutboxKind::Send, OutboxState::Sent, None),
        sent(2, OutboxKind::Send, OutboxState::Failed, None),
    ]);
    click(cx, "share-file-share");
    assert!(shown_dialog(&shell, cx).is_some(), "the dialog stays");
    assert_eq!(picked(&dialog, cx), [LAB_C], "only the one missed stays picked");
    assert_eq!(
        cx.update(|_, cx| dialog.read(cx).problem().cloned()).as_deref(),
        Some("We couldn't share the file with Lab Sea. Try again.")
    );
    assert!(bounds(cx, "share-file-problem").is_some());
}

#[test]
fn close_escape_and_copy_cannot_stop_a_share_before_all_recipients_finish() {
    let (rig, shell, dialog, cx) = opened();
    pick(cx, "Bee");
    pick(cx, "Sea");
    rig.core.write_answers.lock().unwrap().extend([
        sent(1, OutboxKind::Send, OutboxState::Pending, None),
        sent(2, OutboxKind::Send, OutboxState::Sent, None),
    ]);
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.set_message("Original note", cx)));
    click(cx, "share-file-share");
    assert_eq!(sends(&rig).len(), 1);
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.set_message("Changed note", cx)));
    click(cx, "share-file-format");
    click(cx, "share-file-emoji");
    assert_eq!(cx.update(|_, cx| dialog.read(cx).message_text(cx)), "Original note");
    assert!(!cx.update(|_, cx| dialog.read(cx).formatting()));
    assert!(!cx.update(|_, cx| dialog.read(cx).emoji_open()));
    click(cx, "share-file-close");
    cx.simulate_keystrokes("escape");
    cx.update(|_, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.close(cx);
            dialog.copy_link(cx);
        });
    });
    assert!(shown_dialog(&shell, cx).is_some());
    assert!(!cx.update(|_, cx| dialog.read(cx).link_created()));
    rig.core
        .outbox_answers
        .lock()
        .unwrap()
        .push_back(sent(1, OutboxKind::Send, OutboxState::Sent, None));
    cx.executor().advance_clock(std::time::Duration::from_millis(600));
    draw(cx);
    assert_eq!(sends(&rig).len(), 2);
    assert!(shown_dialog(&shell, cx).is_none());
    assert!(cx.update(|_, cx| shell.read(cx).toast().is_some()));
}

#[test]
fn an_uncertain_share_warns_then_resends_the_same_row() {
    let (rig, shell, dialog, cx) = opened();
    pick(cx, "Bee");
    let mut failed = sent(77, OutboxKind::Send, OutboxState::Failed, None);
    failed.last_error = Some("whether it arrived is unknown".to_owned());
    rig.core.write_answers.lock().unwrap().push_back(failed.clone());
    click(cx, "share-file-share");
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.remove(0, cx)));
    pick(cx, "Bee");
    rig.core.outbox_answers.lock().unwrap().push_back(failed.clone());
    click(cx, "share-file-share");
    assert_eq!(
        cx.update(|_, cx| dialog.read(cx).problem().cloned()).as_deref(),
        Some(teams_ui::chat_view::MAY_HAVE_ARRIVED)
    );
    assert!(rig.core.resent.lock().unwrap().is_empty());
    rig.core.outbox_answers.lock().unwrap().extend([failed.clone(), failed]);
    click(cx, "share-file-share");
    assert_eq!(*rig.core.resent.lock().unwrap(), [77]);
    assert_eq!(sends(&rig).len(), 1);
    rig.core
        .outbox_answers
        .lock()
        .unwrap()
        .push_back(sent(77, OutboxKind::Send, OutboxState::Sent, None));
    cx.executor().advance_clock(std::time::Duration::from_millis(600));
    draw(cx);
    assert!(shown_dialog(&shell, cx).is_none());
}

#[test]
fn a_failed_share_resends_its_row_without_a_new_command() {
    let (rig, shell, _dialog, cx) = opened();
    pick(cx, "Bee");
    rig.core
        .write_answers
        .lock()
        .unwrap()
        .push_back(sent(88, OutboxKind::Send, OutboxState::Failed, None));
    click(cx, "share-file-share");
    rig.core.outbox_answers.lock().unwrap().extend([
        sent(88, OutboxKind::Send, OutboxState::Failed, None),
        sent(88, OutboxKind::Send, OutboxState::Failed, None),
    ]);
    click(cx, "share-file-share");
    assert_eq!(*rig.core.resent.lock().unwrap(), [88]);
    assert_eq!(sends(&rig).len(), 1);
    rig.core
        .outbox_answers
        .lock()
        .unwrap()
        .push_back(sent(88, OutboxKind::Send, OutboxState::Sent, None));
    cx.executor().advance_clock(std::time::Duration::from_millis(600));
    draw(cx);
    assert!(shown_dialog(&shell, cx).is_none());
}

#[test]
fn a_failed_chat_creation_resends_its_row_before_the_file_post() {
    let (rig, shell, dialog, cx) = opened();
    pick(cx, "Bee");
    rig.core.service_answer.lock().unwrap().people = vec![FoundPerson {
        mri: Some(LAB_D.to_owned()),
        name: "Lab Dee".to_owned(),
        email: None,
        upn: None,
        addresses: Vec::new(),
        job_title: None,
        department: None,
    }];
    cx.simulate_input("Dee");
    cx.executor().advance_clock(std::time::Duration::from_millis(400));
    draw(cx);
    cx.simulate_keystrokes("enter");
    draw(cx);
    rig.core.write_answers.lock().unwrap().extend([
        sent(98, OutboxKind::Send, OutboxState::Sent, None),
        sent(99, OutboxKind::CreateChat, OutboxState::Failed, None),
        sent(100, OutboxKind::Send, OutboxState::Sent, None),
    ]);
    click(cx, "share-file-share");
    assert_eq!(picked(&dialog, cx), [LAB_D]);
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.set_message("Changed note", cx)));
    assert_eq!(cx.update(|_, cx| dialog.read(cx).message_text(cx)), "Changed note");
    click(cx, "share-file-format");
    assert!(cx.update(|_, cx| dialog.read(cx).formatting()));
    click(cx, "share-file-format");
    let failed = sent(99, OutboxKind::CreateChat, OutboxState::Failed, None);
    rig.core.outbox_answers.lock().unwrap().push_back(failed.clone());
    click(cx, "share-file-share");
    assert_eq!(
        cx.update(|_, cx| dialog.read(cx).problem().cloned()).as_deref(),
        Some("The chat may already exist. Try to create it again?")
    );
    assert!(
        rig.core.resent.lock().unwrap().is_empty(),
        "chat creation needs confirmation"
    );
    rig.core.outbox_answers.lock().unwrap().extend([failed.clone(), failed]);
    click(cx, "share-file-share");
    assert_eq!(*rig.core.resent.lock().unwrap(), [99]);
    rig.core
        .outbox_answers
        .lock()
        .unwrap()
        .push_back(sent(99, OutboxKind::CreateChat, OutboxState::Sent, Some(MADE)));
    cx.executor().advance_clock(std::time::Duration::from_millis(600));
    draw(cx);
    assert_eq!(rig.core.actions.lock().unwrap().len(), 3);
    assert_eq!(sends(&rig)[1].0, MADE);
    assert_eq!(sends(&rig)[1].1.html(), "<p>Changed note</p>");
    assert!(shown_dialog(&shell, cx).is_none());
}

fn mixed_retries(rig: &Rig, cx: &mut VisualTestContext) -> (teams_store::OutboxEntry, teams_store::OutboxEntry) {
    pick(cx, "Bee");
    rig.core.service_answer.lock().unwrap().people = vec![FoundPerson {
        mri: Some(LAB_D.to_owned()),
        name: "Lab Dee".to_owned(),
        email: None,
        upn: None,
        addresses: Vec::new(),
        job_title: None,
        department: None,
    }];
    cx.simulate_input("Dee");
    cx.executor().advance_clock(std::time::Duration::from_millis(400));
    draw(cx);
    cx.simulate_keystrokes("enter");
    draw(cx);
    let mut post = sent(77, OutboxKind::Send, OutboxState::Failed, None);
    post.last_error = Some("whether it arrived is unknown".to_owned());
    let chat = sent(99, OutboxKind::CreateChat, OutboxState::Failed, None);
    rig.core
        .write_answers
        .lock()
        .unwrap()
        .extend([post.clone(), chat.clone()]);
    click(cx, "share-file-share");
    (post, chat)
}

#[test]
fn mixed_retries_show_both_cautions_before_any_resend() {
    let (rig, _shell, dialog, cx) = opened();
    let (post, chat) = mixed_retries(&rig, cx);
    rig.core.outbox_answers.lock().unwrap().extend([post, chat]);
    click(cx, "share-file-share");
    let question = cx.update(|_, cx| dialog.read(cx).problem().cloned()).unwrap();
    assert!(question.contains(teams_ui::chat_view::MAY_HAVE_ARRIVED));
    assert!(question.contains("The chat may already exist. Try to create it again?"));
    assert!(rig.core.resent.lock().unwrap().is_empty());
    assert_eq!(rig.core.actions.lock().unwrap().len(), 2);
}

#[test]
fn a_new_recipient_clears_confirmation_and_gets_both_cautions_before_retry() {
    let (rig, _shell, dialog, cx) = opened();
    let (post, chat) = mixed_retries(&rig, cx);
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.remove(0, cx)));
    rig.core.outbox_answers.lock().unwrap().push_back(chat.clone());
    click(cx, "share-file-share");
    assert_eq!(
        cx.update(|_, cx| dialog.read(cx).problem().cloned()).as_deref(),
        Some("The chat may already exist. Try to create it again?")
    );
    cx.update(|window, cx| dialog.update(cx, |dialog, cx| dialog.focus(window, cx)));
    pick(cx, "Bee");
    assert_eq!(picked(&dialog, cx), [LAB_D, LAB_B]);
    assert!(cx.update(|_, cx| dialog.read(cx).problem().is_none()));
    rig.core.outbox_answers.lock().unwrap().extend([chat, post]);
    click(cx, "share-file-share");
    let question = cx.update(|_, cx| dialog.read(cx).problem().cloned()).unwrap();
    assert!(question.contains(teams_ui::chat_view::MAY_HAVE_ARRIVED));
    assert!(question.contains("The chat may already exist. Try to create it again?"));
    assert!(rig.core.resent.lock().unwrap().is_empty());
    assert_eq!(rig.core.actions.lock().unwrap().len(), 2);
}

#[test]
fn a_share_that_arrives_after_failure_closes_without_a_warning_or_a_resend() {
    let (rig, shell, _dialog, cx) = opened();
    pick(cx, "Bee");
    let mut failed = sent(77, OutboxKind::Send, OutboxState::Failed, None);
    failed.last_error = Some("whether it arrived is unknown".to_owned());
    rig.core.write_answers.lock().unwrap().push_back(failed);
    click(cx, "share-file-share");
    let arrived = sent(77, OutboxKind::Send, OutboxState::Sent, None);
    rig.core
        .outbox_answers
        .lock()
        .unwrap()
        .extend([arrived.clone(), arrived]);
    click(cx, "share-file-share");
    assert!(shown_dialog(&shell, cx).is_none());
    assert!(rig.core.resent.lock().unwrap().is_empty());
    assert_eq!(sends(&rig).len(), 1);
}

#[test]
fn a_share_that_arrives_before_resend_uses_the_current_row() {
    let (rig, shell, _dialog, cx) = opened();
    pick(cx, "Bee");
    let failed = sent(77, OutboxKind::Send, OutboxState::Failed, None);
    rig.core.write_answers.lock().unwrap().push_back(failed.clone());
    click(cx, "share-file-share");
    rig.core
        .outbox_answers
        .lock()
        .unwrap()
        .extend([failed, sent(77, OutboxKind::Send, OutboxState::Sent, None)]);
    click(cx, "share-file-share");
    assert!(shown_dialog(&shell, cx).is_none());
    assert!(rig.core.resent.lock().unwrap().is_empty());
    assert_eq!(sends(&rig).len(), 1);
}

#[test]
fn a_resend_error_reads_the_row_again_and_accepts_a_share_that_arrived() {
    let (rig, shell, _dialog, cx) = opened();
    pick(cx, "Bee");
    let failed = sent(77, OutboxKind::Send, OutboxState::Failed, None);
    rig.core.write_answers.lock().unwrap().push_back(failed.clone());
    click(cx, "share-file-share");
    rig.core.outbox_answers.lock().unwrap().extend([
        failed.clone(),
        failed,
        sent(77, OutboxKind::Send, OutboxState::Sent, None),
    ]);
    rig.core
        .resend_errors
        .lock()
        .unwrap()
        .push_back(teams_core::EngineError::Stopped);
    click(cx, "share-file-share");
    assert!(shown_dialog(&shell, cx).is_none());
    assert_eq!(*rig.core.resent.lock().unwrap(), [77]);
    assert_eq!(sends(&rig).len(), 1);
}

#[test]
fn an_unread_retry_keeps_its_row_and_does_not_post_a_new_message() {
    let (rig, shell, dialog, cx) = opened();
    pick(cx, "Bee");
    let failed = sent(77, OutboxKind::Send, OutboxState::Failed, None);
    rig.core.write_answers.lock().unwrap().push_back(failed.clone());
    click(cx, "share-file-share");
    click(cx, "share-file-share");
    assert_eq!(
        cx.update(|_, cx| dialog.read(cx).problem().cloned()).as_deref(),
        Some("We could not check the share. Try again.")
    );
    assert!(shown_dialog(&shell, cx).is_some());
    assert!(rig.core.resent.lock().unwrap().is_empty());
    assert_eq!(sends(&rig).len(), 1);
    rig.core.outbox_answers.lock().unwrap().extend([failed.clone(), failed]);
    rig.core
        .resend_errors
        .lock()
        .unwrap()
        .push_back(teams_core::EngineError::Stopped);
    click(cx, "share-file-share");
    assert!(shown_dialog(&shell, cx).is_some());
    let arrived = sent(77, OutboxKind::Send, OutboxState::Sent, None);
    rig.core
        .outbox_answers
        .lock()
        .unwrap()
        .extend([arrived.clone(), arrived]);
    click(cx, "share-file-share");
    assert!(shown_dialog(&shell, cx).is_none());
    assert_eq!(*rig.core.resent.lock().unwrap(), [77]);
    assert_eq!(sends(&rig).len(), 1);
}

#[test]
fn a_file_post_retry_keeps_the_original_message_for_all_recipients() {
    let (rig, shell, dialog, cx) = opened();
    pick(cx, "Bee");
    click(cx, "share-file-message");
    cx.simulate_input("Original note");
    let failed = sent(77, OutboxKind::Send, OutboxState::Failed, None);
    rig.core.write_answers.lock().unwrap().push_back(failed.clone());
    click(cx, "share-file-share");
    click(cx, "share-file-message");
    cx.simulate_keystrokes("ctrl-a");
    cx.simulate_input("Changed note");
    cx.simulate_keystrokes("ctrl-z");
    click(cx, "share-file-format");
    click(cx, "share-file-emoji");
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.set_message("Another note", cx)));
    assert_eq!(cx.update(|_, cx| dialog.read(cx).message_text(cx)), "Original note");
    assert!(!cx.update(|_, cx| dialog.read(cx).formatting()));
    assert!(!cx.update(|_, cx| dialog.read(cx).emoji_open()));
    cx.update(|window, cx| dialog.update(cx, |dialog, cx| dialog.focus(window, cx)));
    pick(cx, "Sea");
    assert_eq!(picked(&dialog, cx), [LAB_B, LAB_C]);
    rig.core.outbox_answers.lock().unwrap().extend([
        failed.clone(),
        failed,
        sent(77, OutboxKind::Send, OutboxState::Sent, None),
    ]);
    rig.core
        .write_answers
        .lock()
        .unwrap()
        .push_back(sent(78, OutboxKind::Send, OutboxState::Sent, None));
    click(cx, "share-file-share");
    cx.executor().advance_clock(std::time::Duration::from_millis(600));
    draw(cx);
    assert!(shown_dialog(&shell, cx).is_none());
    assert_eq!(*rig.core.resent.lock().unwrap(), [77]);
    let posts = sends(&rig);
    assert_eq!(posts.len(), 2);
    assert!(posts.iter().all(|(_, text)| text.html() == "<p>Original note</p>"));
}

#[test]
fn restricted_and_unknown_microsoft_permissions_send_nothing() {
    for scope in [Some("users"), Some("existingAccess"), Some("organization"), None] {
        let rig = seeded();
        *rig.core.link_permission.lock().unwrap() = scope.map(|scope| LinkPermission {
            scope: scope.to_owned(),
            description: None,
        });
        let (shell, cx) = shell_window(&rig);
        cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(AB.to_owned(), window, cx)));
        draw(cx);
        click(cx, &format!("file-share-{ID}-0"));
        pick(cx, "Bee");
        click(cx, "share-file-share");
        let dialog = shown_dialog(&shell, cx).unwrap();
        assert!(cx.update(|_, cx| dialog.read(cx).problem().is_some()));
        assert!(sends(&rig).is_empty());
        assert!(cx.update(|_, cx| shell.read(cx).toast().is_none()));
    }
}

/// What the clipboard holds.
fn copied(cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
}

/// The file's OneDrive page, where SharePoint shares it, as the fixture's first file has it.
const ONEDRIVE_PAGE: &str = "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test/_layouts/15/onedrive.aspx?id=%2Fpersonal%2Fmade_up_user_contoso_test%2FDocuments%2FMicrosoft%20Teams%20Chat%20Files%2F02-made-up-rehearsal-cut-v3.mp4&parent=%2Fpersonal%2Fmade_up_user_contoso_test%2FDocuments%2FMicrosoft%20Teams%20Chat%20Files";

#[test]
fn copy_link_copies_the_link_and_shows_link_created() {
    let (_rig, shell, dialog, cx) = opened();
    let link = cx.update(|_, cx| dialog.read(cx).file().share_url.clone()).unwrap();
    click(cx, "share-file-copy-link");
    assert_eq!(copied(cx), Some(link.clone()));
    assert!(cx.update(|_, cx| dialog.read(cx).link_created()));
    for selector in [
        "share-file-link-created",
        "share-file-link-field",
        "share-file-link-copy",
        "share-file-link-permission",
        "share-file-link-settings",
    ] {
        assert!(bounds(cx, selector).is_some(), "{selector}");
    }
    assert!(bounds(cx, "share-file-title").is_none(), "in the dialog's place");
    // Copy copies it again
    cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(String::new())));
    click(cx, "share-file-link-copy");
    assert_eq!(copied(cx), Some(link));
    // Settings opens SharePoint's page of the file, where its links are set
    click(cx, "share-file-link-settings");
    assert_eq!(cx.opened_url().as_deref(), Some(ONEDRIVE_PAGE));
    click(cx, "share-file-link-close");
    assert!(shown_dialog(&shell, cx).is_none(), "Close closes it");
}

#[test]
fn more_actions_shares_outside_teams_on_the_files_onedrive_page() {
    let (_rig, _shell, dialog, cx) = opened();
    click(cx, "share-file-more");
    assert!(cx.update(|_, cx| dialog.read(cx).more_open()));
    click(cx, "share-file-outside");
    assert_eq!(cx.opened_url().as_deref(), Some(ONEDRIVE_PAGE));
    assert!(!cx.update(|_, cx| dialog.read(cx).more_open()), "the menu closed");
    // Escape closes the menu first, the dialog stays
    click(cx, "share-file-more");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!cx.update(|_, cx| dialog.read(cx).more_open()));
    assert!(bounds(cx, "share-file-dialog").is_some());
}

#[test]
fn an_own_servers_file_has_no_share_outside_teams() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    let link = "https://chat.example:8443/sp/personal/0000000000004000800000000000000A/Documents/Microsoft%20Teams%20Chat%20Files/plan.pdf";
    let entry = serde_json::json!({"@type": "http://schema.skype.com/File", "fileName": "plan.pdf", "fileType": "pdf",
        "objectUrl": link, "fileInfo": {"shareUrl": link, "fileUrl": link}});
    let file = teams_proto::chat::shared_files_any(Some(&serde_json::json!([entry])))
        .files
        .remove(0);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_share_file(file, window, cx)));
    draw(cx);
    assert!(bounds(cx, "share-file-dialog").is_some());
    assert!(
        bounds(cx, "share-file-more").is_none(),
        "no page outside the app to share it on"
    );
    click(cx, "share-file-copy-link");
    assert!(bounds(cx, "share-file-link-created").is_some());
    assert!(
        bounds(cx, "share-file-link-settings").is_none(),
        "no page to set its link on"
    );
}

#[test]
fn an_own_servers_channel_file_shows_an_access_error_and_sends_nothing() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    let link = "https://chat.example:8443/sp/sites/00000000-0000-4000-8000-00000000000a/Shared%20Documents/Made-up%20Releases/plan.pdf";
    let entry = serde_json::json!({"@type": "http://schema.skype.com/File", "fileName": "plan.pdf", "fileType": "pdf",
        "objectUrl": link, "fileInfo": {"shareUrl": link, "fileUrl": link}});
    let file = teams_proto::chat::shared_files_any(Some(&serde_json::json!([entry])))
        .files
        .remove(0);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_share_file(file, window, cx)));
    draw(cx);
    pick(cx, "Bee");
    click(cx, "share-file-share");
    let dialog = shown_dialog(&shell, cx).unwrap();
    assert!(cx.update(|_, cx| dialog.read(cx).problem().is_some()));
    assert!(sends(&rig).is_empty());
    assert!(cx.update(|_, cx| shell.read(cx).toast().is_none()));
}

#[test]
fn the_message_is_formatted_as_the_compose_box_formats_it_and_sent_as_its_rich_text() {
    let (rig, _shell, dialog, cx) = opened();
    pick(cx, "Bee");
    click(cx, "share-file-format");
    assert!(cx.update(|_, cx| dialog.read(cx).formatting()));
    assert!(
        bounds(cx, "share-file-format-bar").is_some(),
        "the compose box's format bar"
    );
    click(cx, "share-file-format");
    assert!(bounds(cx, "share-file-format-bar").is_none());
    // Format left the keyboard in the message
    cx.simulate_input("see ");
    cx.simulate_keystrokes("ctrl-b");
    cx.simulate_input("this");
    draw(cx);
    rig.core
        .write_answers
        .lock()
        .unwrap()
        .push_back(sent(1, OutboxKind::Send, OutboxState::Sent, None));
    cx.simulate_keystrokes("enter");
    draw(cx);
    let sends = sends(&rig);
    let [(_, text)] = &sends[..] else {
        panic!("one send: {sends:?}");
    };
    assert_eq!(text.html(), "<p>see <strong>this</strong></p>");
    assert_eq!(text.shared().len(), 1);
}

#[test]
fn the_emoji_button_puts_the_emoji_picked_in_the_message() {
    let (_rig, _shell, dialog, cx) = opened();
    click(cx, "share-file-emoji");
    assert!(cx.update(|_, cx| dialog.read(cx).emoji_open()));
    click(cx, "emoji-1f440_eyes");
    assert!(!cx.update(|_, cx| dialog.read(cx).emoji_open()), "a pick closes it");
    assert_eq!(cx.update(|_, cx| dialog.read(cx).message_text(cx)), "\u{1f440}");
    // Escape closes an open picker, the dialog stays
    click(cx, "share-file-emoji");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!cx.update(|_, cx| dialog.read(cx).emoji_open()));
    assert!(bounds(cx, "share-file-dialog").is_some());
}

#[test]
fn keys_pick_take_off_close_and_share() {
    let (rig, shell, dialog, cx) = opened();
    pick(cx, "Bee");
    pick(cx, "Sea");
    cx.simulate_keystrokes("backspace");
    draw(cx);
    assert_eq!(
        picked(&dialog, cx),
        [LAB_B],
        "Backspace in the empty box takes the last one off"
    );
    // Down moves in the suggestions: the second is taken
    cx.simulate_input("Lab");
    draw(cx);
    cx.simulate_keystrokes("down down enter");
    draw(cx);
    assert_eq!(picked(&dialog, cx), [LAB_B, TALK]);
    // Enter in the message shares
    rig.core.write_answers.lock().unwrap().extend([
        sent(1, OutboxKind::Send, OutboxState::Sent, None),
        sent(2, OutboxKind::Send, OutboxState::Sent, None),
    ]);
    cx.simulate_keystrokes("tab");
    cx.simulate_input("hello");
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(sends(&rig).len(), 2);
    assert!(shown_dialog(&shell, cx).is_none());
    // Escape closes it, nothing sent
    click(cx, &format!("file-share-{ID}-0"));
    assert!(shown_dialog(&shell, cx).is_some());
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(shown_dialog(&shell, cx).is_none(), "Escape closed it");
    assert_eq!(sends(&rig).len(), 2);
}
