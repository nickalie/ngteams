//! The chat list's sections and rows as Teams shows them, on GPUI's test context over the fake engine (`common`): the
//! favourites above the other chats in their order (the account's own notes as "<name> (You)" with its presence), not
//! again under "Chats", the "Favorites" section folding like "Chats" and shown at the top when the favourites come
//! after the list was first drawn (the first run); a chat the service calls empty left out while
//! nothing of it is stored; a meeting's chat by its stored thread type; a bot never named or watched by its MRI; a
//! group without a topic by its members' first names; the section's first rows and See more, a chat that gets a message
//! or is opened from below them. Every id and name is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;

use std::sync::Arc;

use common::{FakeCore, LAB_A, LAB_B, Rig, Written};
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use teams_core::{Availability, ChatListing, Event};
use teams_store::{ChannelKind, Conversation, ConversationKind, Member, Message, Team, TeamChannel};
use teams_ui::ViewTrace;
use teams_ui::chat_list::{BOT, ChatList, ChatRow, ECHO, NOTES, RowKind, UNKNOWN_USER, arrange, joined, unnamed};
use teams_ui::chat_window::{ChatWindow, FIRST_PAGE, NEXT_PAGE, SeeMore};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{Main, Shell, ShellParts};
use teams_ui::sidebar::{Section, Sidebar};

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const LAB_D: &str = "8:orgid:00000000-0000-4000-8000-00000000000d";
const BOT_MRI: &str = "28:00000000-0000-4000-8000-0000000000bb";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const AC: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000c@unq.gbl.spaces";
const A_BOT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-0000000000bb@unq.gbl.spaces";
const GROUP: &str = "19:00000000000000000000000000000group@thread.v2";
const MEETING: &str = "19:meeting_00000000000000000000000000000000@thread.v2";
const QUIET: &str = "19:meeting_0000000000000000000000000000quiet@thread.v2";
const TEAM: &str = "19:00000000000000000000000000000team@thread.tacv2";
const CHANNEL: &str = "19:000000000000000000000000000alerts@thread.tacv2";

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

fn meeting() -> ConversationKind {
    ConversationKind::Other("thread_type:meeting".to_owned())
}

#[tokio::test]
async fn a_list_page_preview_shows_until_stored_history_replaces_it() {
    let core = FakeCore::default();
    let conversation = chat(AB, ConversationKind::OneToOne, Some("Synthetic"), 1_000);
    core.previews
        .lock()
        .unwrap()
        .insert(AB.to_owned(), message(AB, "hint", LAB_B, "<b>List preview</b>", 1_000));
    let row = teams_ui::chat_list::build_row(&core, &conversation).await;
    assert_eq!(row.preview.as_ref(), "List preview");
    assert!(core.messages.lock().unwrap().is_empty());
    core.messages.lock().unwrap().insert(
        AB.to_owned(),
        vec![message(AB, "synced", LAB_A, "Synced message", 1_001)],
    );
    let row = teams_ui::chat_list::build_row(&core, &conversation).await;
    assert_eq!(row.preview.as_ref(), "You: Synced message");
}

/// a's store: the a–b and a–c chats, a 1:1 with a bot, a group without a topic, a meeting's chat and a meeting nobody
/// wrote in; a's notes and the a–c chat its favourites (in that order), the quiet meeting and the bot's chat empty.
fn seed(core: &FakeCore) {
    core.conversations.lock().unwrap().extend([
        chat(AB, ConversationKind::OneToOne, None, 6_000),
        chat(AC, ConversationKind::OneToOne, None, 5_000),
        chat(A_BOT, ConversationKind::OneToOne, None, 4_000),
        chat(GROUP, ConversationKind::Group, None, 3_000),
        chat(MEETING, meeting(), Some("Weekly sync"), 2_000),
        chat(QUIET, meeting(), Some("Test"), 1_000),
    ]);
    let mut messages = core.messages.lock().unwrap();
    messages.insert(AB.to_owned(), vec![message(AB, "6000", LAB_B, "<p>hi</p>", 6_000)]);
    messages.insert(AC.to_owned(), vec![message(AC, "5000", LAB_C, "<p>yo</p>", 5_000)]);
    messages.insert(
        GROUP.to_owned(),
        vec![message(GROUP, "3000", LAB_D, "<p>all</p>", 3_000)],
    );
    messages.insert(
        MEETING.to_owned(),
        vec![message(MEETING, "2000", LAB_B, "<p>notes</p>", 2_000)],
    );
    let mut call = message(A_BOT, "4000", LAB_A, "<partlist/>", 4_000);
    call.message_type = "Event/Call".to_owned();
    messages.insert(A_BOT.to_owned(), vec![call]);
    drop(messages);
    let mut stored = core.members.lock().unwrap();
    stored.insert(AB.to_owned(), members(&[LAB_A, LAB_B]));
    stored.insert(AC.to_owned(), members(&[LAB_A, LAB_C]));
    stored.insert(A_BOT.to_owned(), members(&[LAB_A, BOT_MRI]));
    stored.insert(GROUP.to_owned(), members(&[LAB_A, LAB_B, LAB_D]));
    stored.insert(MEETING.to_owned(), members(&[LAB_A, LAB_B]));
    drop(stored);
    let mut names = core.names.lock().unwrap();
    names.insert(LAB_A.to_owned(), "Ann Able".to_owned());
    names.insert(LAB_B.to_owned(), "Sean Bee".to_owned());
    names.insert(LAB_C.to_owned(), "Cleo Sea".to_owned());
    names.insert(LAB_D.to_owned(), "Evgenia Dee".to_owned());
    drop(names);
    *core.listing.lock().unwrap() = ChatListing {
        favorites: Some(vec![
            NOTES.to_owned(),
            AC.to_owned(),
            "19:not-stored@thread.v2".to_owned(),
        ]),
        empty: vec![QUIET.to_owned(), A_BOT.to_owned()],
    };
}

fn seeded() -> Rig {
    let rig = Rig::start();
    seed(&rig.core);
    rig.cx.update(teams_ui::window::init);
    rig
}

fn shell_window(rig: &Rig) -> (Entity<Shell>, &'static mut VisualTestContext) {
    shell_window_traced(rig, Arc::new(ViewTrace::off()))
}

fn shell_window_traced(rig: &Rig, trace: Arc<ViewTrace>) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig
            .cx
            .update(|cx| cx.new(|_| SettingsModel::in_memory(Settings::default()))),
        trace,
    };
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

fn shown(rows: &[ChatRow]) -> Vec<(&str, RowKind, &str, bool)> {
    rows.iter()
        .map(|row| (row.id.as_str(), row.kind, row.name.as_ref(), row.named))
        .collect()
}

fn drawn(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.debug_bounds(selector).is_some()
}

#[test]
fn the_favourites_come_first_in_their_order_and_not_again_under_chats() {
    let rig = seeded();
    let written = Written::default();
    let (shell, cx) = shell_window_traced(&rig, Arc::new(ViewTrace::new(written.clone())));
    let list = list_of(&shell, cx);
    let (favorites, rows) = cx.update(|_, cx| (list.read(cx).favorites().to_vec(), list.read(cx).rows().to_vec()));

    // a's notes by a's own name, then the a–c chat; a favourite the store doesn't hold left out
    assert_eq!(
        shown(&favorites),
        [
            (NOTES, RowKind::Notes, "Ann Able (You)", true),
            (AC, RowKind::OneToOne, "Cleo Sea", true),
        ]
    );
    assert_eq!(
        favorites[0].partner.as_deref(),
        Some(LAB_A),
        "a's own presence on its notes"
    );
    // the quiet meeting and the bot's chat (only a call event stored) left out as the service calls them empty
    assert_eq!(
        shown(&rows),
        [
            (AB, RowKind::OneToOne, "Sean Bee", true),
            (GROUP, RowKind::Group, "Evgenia and Sean", true),
            (MEETING, RowKind::Meeting, "Weekly sync", true),
        ]
    );
    assert_eq!(rows[1].faces.len(), 2, "a group's avatar is split between two members");
    assert!(
        rows[2].faces.is_empty() && rows[2].partner.is_none(),
        "a meeting shows its calendar"
    );
    // the switcher and the header find a favourite too
    assert!(cx.update(|_, cx| list.read(cx).row(NOTES).is_some()));
    rig.send(Event::Presence {
        mri: LAB_A.to_owned(),
        availability: Availability::Available,
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| list.read(cx).presence_of(&favorites[0], cx)),
        Some(Availability::Available)
    );
    assert!(rig.core.asked().contains(&"chat_listing"));
    // traced by kind, the favourites first, never a name
    let lines = written.lines();
    let line = lines
        .iter()
        .rev()
        .find(|line| line["view"] == "chat-list")
        .expect("traced");
    assert_eq!(
        line["kinds"],
        serde_json::json!(["notes", "one-to-one", "one-to-one", "group", "meeting"])
    );
    let all = written.text();
    for name in ["Ann Able", "Cleo", "Evgenia", "Weekly"] {
        assert!(!all.contains(name), "{name} reached the trace");
    }
}

/// The team the favourite channel below belongs to.
fn ops_crew() -> Team {
    Team {
        id: TEAM.to_owned(),
        name: "ops crew".to_owned(),
        archived: false,
        group_id: None,
        mailbox: None,
        owner: false,
        can_add_channel: false,
        can_add_private_channel: false,
        about: teams_store::TeamAbout::default(),
        channels: vec![
            TeamChannel {
                id: TEAM.to_owned(),
                name: "General".to_owned(),
                general: true,
                kind: ChannelKind::Standard,
                about: teams_store::ChannelAbout::default(),
                shown: true,
            },
            TeamChannel {
                id: CHANNEL.to_owned(),
                name: "Prod alerts".to_owned(),
                general: false,
                kind: ChannelKind::Standard,
                about: teams_store::ChannelAbout::default(),
                shown: true,
            },
        ],
    }
}

fn exercise_channel_menus(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
    // Both copies are in view, so opening either must paint only its own popup.
    let chats = cx.debug_bounds("section-chats").unwrap();
    cx.simulate_click(chats.center(), Modifiers::none());
    cx.run_until_parked();
    for (button, panel) in [
        ("favorite-channel-more-1", "channel-menu-panel-favorite-1"),
        ("tree-channel-more-1", "channel-menu-panel-tree-1"),
    ] {
        assert!(drawn(cx, button), "{button} is reachable");
        let more = cx.debug_bounds(button).unwrap();
        cx.simulate_click(more.center(), Modifiers::none());
        cx.run_until_parked();
        assert!(
            drawn(cx, "channel-workflows-1"),
            "the same channel management is offered"
        );
        assert!(cx.debug_bounds(panel).is_some(), "the clicked row anchors the menu");
        let panels = ["channel-menu-panel-favorite-1", "channel-menu-panel-tree-1"]
            .into_iter()
            .filter(|selector| cx.debug_bounds(selector).is_some())
            .count();
        assert_eq!(panels, 1, "the same channel elsewhere must not render another popup");
        assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Nothing);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
    }
}

#[test]
fn a_favourite_channel_is_listed_by_its_name_and_team_and_opens_as_a_channel() {
    let rig = seeded();
    *rig.core.teams.lock().unwrap() = Some(vec![ops_crew()]);
    // the favourites as an account with channels among them has them: a channel between the notes and a chat, and one
    // of no stored team left out
    rig.core.listing.lock().unwrap().favorites = Some(vec![
        NOTES.to_owned(),
        CHANNEL.to_owned(),
        AC.to_owned(),
        "19:not-a-team-s@thread.tacv2".to_owned(),
    ]);
    rig.core.messages.lock().unwrap().insert(
        CHANNEL.to_owned(),
        vec![message(CHANNEL, "7000", LAB_B, "<p>disk full</p>", 7_000)],
    );
    rig.core.unread.lock().unwrap().insert(CHANNEL.to_owned(), 3);
    let written = Written::default();
    let (shell, cx) = shell_window_traced(&rig, Arc::new(ViewTrace::new(written.clone())));
    let list = list_of(&shell, cx);
    let (favorites, rows) = cx.update(|_, cx| (list.read(cx).favorites().to_vec(), list.read(cx).rows().to_vec()));
    assert_eq!(
        shown(&favorites),
        [
            (NOTES, RowKind::Notes, "Ann Able (You)", true),
            (CHANNEL, RowKind::Channel, "Prod alerts", true),
            (AC, RowKind::OneToOne, "Cleo Sea", true),
        ]
    );
    let channel = &favorites[1];
    assert_eq!(channel.team.as_deref(), Some("ops crew"));
    assert_eq!(
        (channel.preview.as_ref(), channel.last_activity, channel.unread),
        ("disk full", Some(7_000), 3)
    );
    assert!(channel.partner.is_none(), "a channel shows no presence");
    assert!(!rows.iter().any(|row| row.id == CHANNEL));
    assert!(drawn(cx, "avatar-team"), "its team's mark for its avatar");
    exercise_channel_menus(&shell, cx);
    let lines = written.lines();
    let line = lines
        .iter()
        .rev()
        .find(|line| line["view"] == "chat-list")
        .expect("traced");
    assert_eq!(line["kinds"][1], "channel");
    assert!(!written.text().contains("Prod alerts") && !written.text().contains("ops crew"));

    // clicked: opened read-only as a channel, its row still highlighted
    let found = cx.debug_bounds("favorite-row-1").unwrap();
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Channel(CHANNEL.to_owned())
    );
    assert_eq!(
        cx.update(|_, cx| list.read(cx).selected().map(str::to_owned)),
        Some(CHANNEL.to_owned())
    );
    // a chat opened after it takes the highlight
    let found = cx.debug_bounds("favorite-row-2").unwrap();
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| list.read(cx).selected().map(str::to_owned)),
        Some(AC.to_owned())
    );
}

#[test]
fn the_favorites_section_folds_like_chats_and_is_shown_only_with_favourites() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    let sidebar = cx.update(|_, cx| shell.read(cx).sidebar().clone());
    for selector in [
        "section-favorites",
        "favorite-row-0",
        "favorite-row-1",
        "section-chats",
        "chat-row-0",
    ] {
        assert!(drawn(cx, selector), "{selector}");
    }
    let found = cx.debug_bounds("section-favorites").unwrap();
    cx.simulate_click(found.center(), Modifiers::none());
    assert!(!cx.update(|_, cx| sidebar.read(cx).is_open(Section::Favorites)));
    assert!(!drawn(cx, "favorite-row-0"));
    assert!(drawn(cx, "chat-row-0"), "only the favourites folded");

    // no favourites: no section
    let rig = seeded();
    rig.core.listing.lock().unwrap().favorites = Some(Vec::new());
    let (_, cx) = shell_window(&rig);
    assert!(!drawn(cx, "section-favorites"));
    assert!(drawn(cx, "section-chats"));
}

#[test]
fn favourites_read_after_the_list_was_drawn_show_above_its_chats() {
    // the first run: the window opens before anything is stored, so the list's first layout doesn't fill the pane
    let rig = seeded();
    let stored = std::mem::take(&mut *rig.core.conversations.lock().unwrap());
    rig.core.listing.lock().unwrap().favorites = None;
    let (shell, cx) = shell_window(&rig);
    assert!(drawn(cx, "section-chats"));
    assert!(!drawn(cx, "section-favorites"));
    // the catch-up stores more chats than the pane holds
    let mut many = stored;
    many.extend((0..40).map(|at| {
        let id = format!("19:{at:032}@thread.v2");
        chat(&id, ConversationKind::Group, Some("Topic"), 500 - at)
    }));
    let ids: Vec<String> = many.iter().map(|conversation| conversation.id.clone()).collect();
    *rig.core.conversations.lock().unwrap() = many;
    for conversation in ids {
        rig.send(Event::ConversationChanged { conversation });
    }
    assert!(drawn(cx, "section-chats"));
    // then the teams' read gives the favourites, only the notes (so every row under "Chats" stays where it was):
    // shown at the top, not scrolled past
    rig.core.listing.lock().unwrap().favorites = Some(vec![NOTES.to_owned()]);
    rig.send(Event::Teams);
    let list = list_of(&shell, cx);
    assert_eq!(cx.update(|_, cx| list.read(cx).favorites().len()), 1);
    for selector in ["section-favorites", "favorite-row-0", "section-chats"] {
        assert!(drawn(cx, selector), "{selector}");
    }
    // in view at the list's top, right under the header
    let favorites = cx.debug_bounds("section-favorites").unwrap();
    let sidebar = cx.debug_bounds("sidebar").unwrap();
    let header = gpui::px(48.0);
    assert!(
        favorites.top() >= sidebar.top() + header && favorites.top() < sidebar.top() + header + gpui::px(4.0),
        "{favorites:?} at the list's top in {sidebar:?}"
    );
}

#[test]
fn a_bot_is_never_titled_or_watched_by_its_mri() {
    let rig = seeded();
    // the bot's chat with text in it: listed, as the service no longer calls it empty
    rig.core.listing.lock().unwrap().empty.retain(|id| id != A_BOT);
    rig.core.messages.lock().unwrap().insert(
        A_BOT.to_owned(),
        vec![message(A_BOT, "4001", BOT_MRI, "<p>beep</p>", 4_001)],
    );
    let (shell, cx) = shell_window(&rig);
    let list = list_of(&shell, cx);
    let row = cx.update(|_, cx| list.read(cx).row(A_BOT).cloned()).expect("listed");
    assert_eq!(
        (row.name.as_ref(), row.named, row.partner.as_deref()),
        (BOT, false, None)
    );
    assert!(!rig.core.presence_asked.lock().unwrap().iter().any(|mri| mri == BOT_MRI));
    // the meeting nobody wrote in stays out of the list, and is still found (by the search, the switcher, New message)
    assert!(cx.update(|_, cx| !list.read(cx).all_rows().any(|row| row.id == QUIET)));
    assert!(cx.update(|_, cx| list.read(cx).row(QUIET).is_some()));
    assert_eq!(rig.shutdowns(), 0, "the list never stops the engine");
}

#[test]
fn someone_without_a_name_is_named_by_what_they_are() {
    assert_eq!(unnamed(LAB_B), UNKNOWN_USER);
    assert_eq!(unnamed(BOT_MRI), BOT);
    assert_eq!(unnamed(teams_proto::TEAMS_ECHO_BOT_MRI), ECHO);
}

#[test]
fn names_are_joined_as_teams_joins_them() {
    let names = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>();
    assert_eq!(joined(&names(&["Ann"]), 0), "Ann");
    assert_eq!(joined(&names(&["Evgenia", "Sean"]), 0), "Evgenia and Sean");
    assert_eq!(joined(&names(&["Ann", "Bob", "Cleo"]), 0), "Ann, Bob, and Cleo");
    assert_eq!(
        joined(&names(&["Ann", "Bob", "Cleo", "Dee"]), 2),
        "Ann, Bob, Cleo, Dee, +2"
    );
}

#[test]
fn arranging_keeps_a_chat_called_empty_once_it_has_text_or_unread() {
    let row = |id: &str, preview: &str, unread: u64| ChatRow {
        id: id.to_owned(),
        kind: RowKind::Group,
        name: "x".into(),
        named: true,
        picture: None,
        partner: None,
        faces: Vec::new(),
        members: 0,
        team: None,
        preview: preview.to_owned().into(),
        previewed: None,
        last_activity: None,
        unread,
    };
    let listing = ChatListing {
        favorites: None,
        empty: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
    };
    let arranged = arrange(
        vec![row("a", "", 0), row("b", "hi", 0), row("c", "", 1)],
        &listing,
        None,
        &[],
    );
    assert!(arranged.favorites.is_empty());
    let ids: Vec<&str> = arranged.rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, ["b", "c"]);
    // the one left out is still found
    let hidden: Vec<&str> = arranged.hidden.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(hidden, ["a"]);
}

#[test]
fn an_empty_chat_opened_is_listed_after_the_others_in_the_order_they_were_opened() {
    let row = |id: &str, last: i64| ChatRow {
        id: id.to_owned(),
        kind: RowKind::Group,
        name: "x".into(),
        named: true,
        picture: None,
        partner: None,
        faces: Vec::new(),
        members: 0,
        team: None,
        preview: if id.starts_with("text") { "hi" } else { "" }.into(),
        previewed: None,
        last_activity: Some(last),
        unread: 0,
    };
    let listing = ChatListing {
        favorites: None,
        empty: vec!["call-1".to_owned(), "call-2".to_owned(), "call-3".to_owned()],
    };
    // newer than the chat with text, the call-only chats are still listed after it, as Teams lists them once opened
    let rows = vec![row("call-1", 30), row("call-2", 20), row("text", 10), row("call-3", 5)];
    let arranged = arrange(rows, &listing, None, &["call-3".to_owned(), "call-1".to_owned()]);
    let ids: Vec<&str> = arranged.rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, ["text", "call-3", "call-1"]);
    let hidden: Vec<&str> = arranged.hidden.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(hidden, ["call-2"]);
}

/// The id of the group chat numbered `number`, the first the latest.
fn numbered(number: usize) -> String {
    format!("19:{number:032x}@thread.v2")
}

/// A list of `count` group chats, each with a message from the account: nothing unread.
fn many_chats(count: usize) -> Rig {
    let rig = Rig::start();
    rig.cx.update(teams_ui::window::init);
    for number in 0..count {
        let id = numbered(number);
        let last = i64::try_from(1_000_000 - number).unwrap();
        rig.core
            .conversations
            .lock()
            .unwrap()
            .push(chat(&id, ConversationKind::Group, Some("Project"), last));
        rig.core
            .messages
            .lock()
            .unwrap()
            .insert(id.clone(), vec![message(&id, "1", LAB_A, "<p>done</p>", last)]);
    }
    rig
}

/// The chats the section shows, by their number.
fn shown_numbers(list: &Entity<ChatList>, cx: &mut VisualTestContext) -> Vec<usize> {
    cx.update(|_, cx| {
        let list = list.read(cx);
        list.shown_rows()
            .into_iter()
            .map(|at| {
                let id = &list.rows()[at].id;
                (0..1_000).find(|number| numbered(*number) == *id).unwrap()
            })
            .collect()
    })
}

/// The chat numbered `number` gets a message: the latest of all, told as the engine tells it.
fn new_message(rig: &Rig, number: usize, cx: &mut VisualTestContext) {
    let id = numbered(number);
    {
        let mut conversations = rig.core.conversations.lock().unwrap();
        let at = conversations.iter().position(|chat| chat.id == id).unwrap();
        let mut moved = conversations.remove(at);
        moved.last_activity = Some(2_000_000);
        moved.version += 1;
        conversations.insert(0, moved);
    }
    rig.send(Event::ConversationChanged { conversation: id });
    cx.run_until_parked();
}

/// See more, clicked where it is drawn.
fn click_see_more(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
    let sidebar = cx.update(|_, cx| shell.read(cx).sidebar().clone());
    cx.update(|_, cx| sidebar.update(cx, Sidebar::reveal_see_more));
    assert!(drawn(cx, "chats-see-more"), "See more is listed");
    let found = cx.debug_bounds("chats-see-more").unwrap();
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
}

#[test]
fn the_section_shows_its_first_rows_and_see_more_brings_the_next_until_none_is_left() {
    let total = FIRST_PAGE + NEXT_PAGE + 3;
    let rig = many_chats(total);
    let (shell, cx) = shell_window(&rig);
    let list = list_of(&shell, cx);
    assert_eq!(
        cx.update(|_, cx| list.read(cx).rows().len()),
        total,
        "the list holds them all"
    );
    assert_eq!(shown_numbers(&list, cx), (0..FIRST_PAGE).collect::<Vec<_>>());
    assert!(drawn(cx, "chat-row-0"));
    assert_eq!(
        cx.update(|_, cx| list.read(cx).see_more_offered()),
        Some(SeeMore {
            hidden: total - FIRST_PAGE,
            unread: false
        })
    );

    click_see_more(&shell, cx);
    assert_eq!(
        shown_numbers(&list, cx),
        (0..FIRST_PAGE + NEXT_PAGE).collect::<Vec<_>>()
    );

    // what is left comes with the next one, and See more goes: there is no "See less"
    click_see_more(&shell, cx);
    assert_eq!(shown_numbers(&list, cx), (0..total).collect::<Vec<_>>());
    assert_eq!(cx.update(|_, cx| list.read(cx).see_more_offered()), None);
    assert!(!drawn(cx, "chats-see-more"));
}

#[test]
fn folding_the_section_keeps_what_see_more_brought() {
    let rig = many_chats(FIRST_PAGE + 2);
    let (shell, cx) = shell_window(&rig);
    let list = list_of(&shell, cx);
    click_see_more(&shell, cx);
    cx.update(|_, cx| {
        let sidebar = shell.read(cx).sidebar().clone();
        sidebar.update(cx, |sidebar, cx| {
            sidebar.toggle(Section::Chats, cx);
            sidebar.toggle(Section::Chats, cx);
        });
    });
    assert_eq!(shown_numbers(&list, cx), (0..FIRST_PAGE + 2).collect::<Vec<_>>());
}

#[test]
fn a_list_no_longer_than_the_first_rows_offers_no_see_more() {
    let rig = many_chats(FIRST_PAGE);
    let (shell, cx) = shell_window(&rig);
    let list = list_of(&shell, cx);
    assert_eq!(shown_numbers(&list, cx).len(), FIRST_PAGE);
    assert_eq!(cx.update(|_, cx| list.read(cx).see_more_offered()), None);
    assert!(!drawn(cx, "chats-see-more"));
}

#[test]
fn a_chat_that_gets_a_message_below_the_rows_comes_in_on_top_and_none_drops_out() {
    let rig = many_chats(FIRST_PAGE + 10);
    let (shell, cx) = shell_window(&rig);
    let list = list_of(&shell, cx);

    new_message(&rig, FIRST_PAGE + 5, cx);
    let mut expected = vec![FIRST_PAGE + 5];
    expected.extend(0..FIRST_PAGE);
    assert_eq!(shown_numbers(&list, cx), expected);

    // a chat already shown only moves up
    new_message(&rig, FIRST_PAGE - 1, cx);
    let shown = shown_numbers(&list, cx);
    assert_eq!(shown.len(), FIRST_PAGE + 1);
    assert_eq!(shown[0], FIRST_PAGE - 1);
}

#[test]
fn unread_chats_stay_in_their_place_and_see_more_says_when_one_is_below_it() {
    let late = FIRST_PAGE + 2;
    let rig = many_chats(FIRST_PAGE + 3);
    rig.core.unread.lock().unwrap().insert(numbered(late), 2);
    let (shell, cx) = shell_window(&rig);
    let list = list_of(&shell, cx);
    // the order is the latest message's, whatever is unread, as Teams' `MostRecent`
    assert_eq!(shown_numbers(&list, cx), (0..FIRST_PAGE).collect::<Vec<_>>());
    assert_eq!(
        cx.update(|_, cx| list.read(cx).see_more_offered()),
        Some(SeeMore {
            hidden: 3,
            unread: true
        })
    );
    assert!(drawn(cx, "chat-row-0"));
    let sidebar = cx.update(|_, cx| shell.read(cx).sidebar().clone());
    cx.update(|_, cx| sidebar.update(cx, Sidebar::reveal_see_more));
    assert!(drawn(cx, "chats-see-more"), "drawn bold");
}

#[test]
fn a_chat_opened_from_below_the_rows_is_listed_after_them_and_see_more_keeps_going() {
    let below = FIRST_PAGE + 8;
    let rig = many_chats(FIRST_PAGE + NEXT_PAGE + 1);
    let (shell, cx) = shell_window(&rig);
    let list = list_of(&shell, cx);
    let id = numbered(below);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(id.clone(), window, cx)));
    cx.run_until_parked();

    let mut expected: Vec<usize> = (0..FIRST_PAGE).collect();
    expected.push(below);
    assert_eq!(shown_numbers(&list, cx), expected);
    assert_eq!(
        cx.update(|_, cx| list.read(cx).see_more_offered())
            .map(|more| more.hidden),
        Some(NEXT_PAGE),
        "the chat opened isn't counted as left"
    );
    // scrolled to, and highlighted there; its selector is its place in the list
    let selector: &'static str = Box::leak(format!("chat-row-{below}").into_boxed_str());
    assert!(drawn(cx, selector), "it is on screen");

    // it stays when another chat is opened, as Teams keeps the row it added
    let first = numbered(0);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(first.clone(), window, cx)));
    cx.run_until_parked();
    assert_eq!(shown_numbers(&list, cx), expected);

    // See more reaches it in its own place
    cx.update(|_, cx| list.update(cx, ChatList::see_more));
    cx.run_until_parked();
    assert_eq!(
        shown_numbers(&list, cx),
        (0..FIRST_PAGE + NEXT_PAGE).collect::<Vec<_>>()
    );
}

#[test]
fn a_favourite_opened_is_not_listed_again_under_chats() {
    let rig = many_chats(FIRST_PAGE + 5);
    let favourite = numbered(FIRST_PAGE + 2);
    rig.core.listing.lock().unwrap().favorites = Some(vec![favourite.clone()]);
    let (shell, cx) = shell_window(&rig);
    let list = list_of(&shell, cx);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(favourite.clone(), window, cx)));
    cx.run_until_parked();
    assert_eq!(shown_numbers(&list, cx), (0..FIRST_PAGE).collect::<Vec<_>>());
    assert!(drawn(cx, "favorite-row-0"));
}

#[test]
fn the_palette_sees_more_chats_as_the_link_does() {
    let rig = many_chats(FIRST_PAGE + 1);
    let (shell, cx) = shell_window(&rig);
    let list = list_of(&shell, cx);
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("see more");
    cx.run_until_parked();
    let switcher = cx.update(|_, cx| shell.read(cx).switcher().cloned()).expect("open");
    assert_eq!(cx.update(|_, cx| switcher.read(cx).commands()), ["See more chats"]);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(shown_numbers(&list, cx).len(), FIRST_PAGE + 1);
    assert!(cx.update(|_, cx| shell.read(cx).switcher().is_none()), "closed");
}

/// A row of nothing but its id.
fn plain_row(id: &str) -> ChatRow {
    ChatRow {
        id: id.to_owned(),
        kind: RowKind::Group,
        name: "Project".into(),
        named: true,
        picture: None,
        partner: None,
        faces: Vec::new(),
        members: 0,
        team: None,
        preview: "done".into(),
        previewed: None,
        last_activity: None,
        unread: 0,
    }
}

#[test]
fn a_catch_up_adds_no_rows_for_what_it_moved_up() {
    let rows: Vec<ChatRow> = (0..40).map(|number| plain_row(&numbered(number))).collect();
    let mut window = ChatWindow::default();
    window.settle(&rows, None);
    assert_eq!(window.shown(&rows).len(), FIRST_PAGE);

    // while running, what moved up adds to the rows shown
    let mut moved = rows.clone();
    moved.rotate_right(3);
    window.settle(&moved, None);
    assert_eq!(window.shown(&moved).len(), FIRST_PAGE + 3);

    // a catch-up shows what was asked for, and See more counts from there
    window.caught_up();
    window.settle(&moved, None);
    assert_eq!(window.shown(&moved).len(), FIRST_PAGE);
    window.see_more(&moved, None);
    assert_eq!(window.shown(&moved).len(), FIRST_PAGE + NEXT_PAGE.min(20));
}

#[test]
fn a_shorter_list_shows_what_there_is_and_forgets_a_chat_gone() {
    let rows: Vec<ChatRow> = (0..30).map(|number| plain_row(&numbered(number))).collect();
    let mut window = ChatWindow::default();
    window.settle(&rows, Some(&numbered(25)));
    assert_eq!(window.shown(&rows).last(), Some(&25));
    // the chat opened leaves the list: nothing listed for it, and it isn't asked for again
    let shorter: Vec<ChatRow> = rows.iter().take(25).cloned().collect();
    window.settle(&shorter, None);
    assert_eq!(window.shown(&shorter).len(), FIRST_PAGE);
    assert_eq!(window.see_more_offered(&shorter).map(|more| more.hidden), Some(5));
    let few: Vec<ChatRow> = rows.iter().take(3).cloned().collect();
    window.settle(&few, None);
    assert_eq!(window.shown(&few), [0, 1, 2]);
    assert_eq!(window.see_more_offered(&few), None);
}

#[test]
fn a_long_list_is_held_whole() {
    // past [`teams_ui::chat_list::LIST_LIMIT`]: the tray's and the notifications' limit, not the list's
    let total = teams_ui::chat_list::LIST_LIMIT + 3;
    let rig = many_chats(total);
    let (shell, cx) = shell_window(&rig);
    let list = list_of(&shell, cx);
    assert_eq!(cx.update(|_, cx| list.read(cx).rows().len()), total);
    assert_eq!(shown_numbers(&list, cx).len(), FIRST_PAGE);
}
