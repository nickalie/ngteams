//! The title bar's search on GPUI's test context, over the fake engine (`common`, whose store search plays the store's
//! on the messages it holds, and whose service search answers what a test gives it): Ctrl+E puts the keyboard in the
//! box, typing suggests the chats, channels and people the lists know at once (and the service's once it answers),
//! Down and Enter open one; Enter shows the results across the panes, the messages of the chats the list shows (a
//! channel's left out) beside the service's, the newest first, with the chat, the sender ("You" for the account) and
//! the matched text; a query too short asks the store nothing; a message opened shows its chat at the message, read
//! back from beyond the newest page, selected and scrolled to; Ctrl+Alt+G goes right to a chat by its name; Escape
//! closes the suggestions and leaves the results; Back and Forward go through the places shown. The store's own quoting
//! of FTS syntax is `teams-store`'s test. Every id, name and text is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

pub mod common;
// the shell's parts over the rig
#[expect(dead_code, reason = "this test uses part of the fake tool")]
#[path = "support/fake_translator.rs"]
mod fake_translator;
#[path = "common/parts.rs"]
mod parts;

use std::sync::Arc;

use common::{LAB_A, LAB_B, Rig, Written};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point, px, size};
use teams_core::{Action, Event, SearchScope};
use teams_proto::{Found, FoundChannel, FoundChat, FoundFile, FoundMessage, FoundPerson, SearchKind, highlighted};
use teams_store::search::{MATCH_END, MATCH_START};
use teams_store::{ChannelKind, Conversation, ConversationKind, Member, Message, Team, TeamChannel};
use teams_ui::ViewTrace;
use teams_ui::chat_view::{ChatView, MessageAction, PAGE, READ_DEBOUNCE};
use teams_ui::compose::Mode;
use teams_ui::global_search::{Mode as SearchMode, SUGGEST_DELAY, Suggestion};
use teams_ui::local_ai::{Backend, Tools};
use teams_ui::search::{MAX_HITS, MIN_QUERY, YOU, marked_terms, searchable, snippet_line, split_snippet};
use teams_ui::settings::Settings;
use teams_ui::shell::{Main, Shell};

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const TRIO: &str = "19:0000000000000000000000000000trio@thread.v2";
const CHANNEL: &str = "19:channel0001@thread.tacv2";
/// 2026-01-01T12:00:00Z, ms.
const NOON: i64 = 1_767_268_800_000;
const MINUTE: i64 = 60_000;
/// How many messages the a–b chat holds: more than two of the view's pages.
const MANY: usize = 2 * PAGE + 20;
/// The a–b chat's old message the tests look for: far beyond the newest page.
const OLD: &str = "7";

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

/// a's store: the a–b chat of [`MANY`] messages from b a minute apart up to `NOON`, the old one ([`OLD`]) about the
/// zeppelin plan, and a's own about the zeppelin newer than all; the trio, c's message about the zeppelin newest; a
/// team's channel with one too, which the list doesn't show.
fn seeded() -> Rig {
    let rig = Rig::start();
    let core = &rig.core;
    core.conversations.lock().unwrap().extend([
        chat(AB, ConversationKind::OneToOne, None, NOON + MINUTE),
        chat(TRIO, ConversationKind::Group, Some("Lab trio"), NOON + 2 * MINUTE),
        chat(CHANNEL, ConversationKind::Channel, Some("General"), NOON + 3 * MINUTE),
    ]);
    let mut ab: Vec<Message> = (1..=MANY)
        .map(|n| {
            let back = i64::try_from(MANY - n).unwrap();
            let text = if n.to_string() == OLD {
                "<p>the zeppelin plan</p>".to_owned()
            } else {
                format!("<p>number {n}</p>")
            };
            message(AB, &n.to_string(), LAB_B, &text, NOON - back * MINUTE)
        })
        .collect();
    ab.push(message(AB, "9001", LAB_A, "<p>zeppelin again</p>", NOON + MINUTE));
    let mut messages = core.messages.lock().unwrap();
    messages.insert(AB.to_owned(), ab);
    messages.insert(
        TRIO.to_owned(),
        vec![message(
            TRIO,
            "9002",
            LAB_C,
            "<p>a zeppelin in the trio</p>",
            NOON + 2 * MINUTE,
        )],
    );
    messages.insert(
        CHANNEL.to_owned(),
        vec![message(
            CHANNEL,
            "9003",
            LAB_B,
            "<p>zeppelin channel post</p>",
            NOON + 3 * MINUTE,
        )],
    );
    drop(messages);
    let mut stored = core.members.lock().unwrap();
    stored.insert(AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]);
    stored.insert(
        TRIO.to_owned(),
        [LAB_A, LAB_B, LAB_C].iter().map(|mri| Member::new(mri, None)).collect(),
    );
    drop(stored);
    let mut names = core.names.lock().unwrap();
    names.insert(LAB_B.to_owned(), "Lab Bee".to_owned());
    names.insert(LAB_C.to_owned(), "Lab Sea".to_owned());
    drop(names);
    rig.cx.update(teams_ui::window::init);
    rig
}

fn trace() -> (Arc<ViewTrace>, Written) {
    let written = Written::default();
    (Arc::new(ViewTrace::new(written.clone())), written)
}

/// Draws the window and lets what it started run.
fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    cx.run_until_parked();
}

/// The shell in a 1000×700 window of its own.
fn shell_window(rig: &Rig, trace: Arc<ViewTrace>) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let parts = parts::parts(rig, Settings::default(), trace);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.simulate_resize(size(px(1000.0), px(700.0)));
    draw(cx);
    (shell, cx)
}

fn chat_view(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<ChatView> {
    cx.update(|_, cx| shell.read(cx).chat().cloned())
        .expect("a chat is open")
}

/// The chat is open at `message`: it is held, selected and the list no longer follows the newest.
fn opened_at(shell: &Entity<Shell>, cx: &mut VisualTestContext, conversation: &str, message: &str) {
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(conversation.to_owned())
    );
    let view = chat_view(shell, cx);
    cx.update(|_, cx| {
        let view = view.read(cx);
        assert_eq!(view.conversation(), conversation);
        assert!(view.message_items().any(|item| item.id == message), "{message} is held");
        assert_eq!(view.selected(), Some(message), "and selected");
        assert!(!view.list_state().is_following_tail(), "the list stays at it");
    });
}

fn changes(written: &Written, view: &str) -> Vec<serde_json::Value> {
    written
        .lines()
        .into_iter()
        .filter(|line| line["view"] == view)
        .collect()
}

/// The messages the results show: (conversation, message, chat, sender, snippet).
fn result_hits(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Vec<(String, String, String, String, String)> {
    cx.update(|_, cx| {
        let search = shell.read(cx).search().clone();
        search
            .read(cx)
            .message_hits(cx)
            .iter()
            .map(|hit| {
                (
                    hit.target.conversation.clone(),
                    hit.target.message.clone(),
                    hit.chat.to_string(),
                    hit.sender.to_string(),
                    hit.snippet.to_string(),
                )
            })
            .collect()
    })
}

/// Searches for `query` from the title bar: Ctrl+E, the query typed, Enter.
fn search_for(cx: &mut VisualTestContext, query: &str) {
    cx.simulate_keystrokes("ctrl-e");
    draw(cx);
    cx.simulate_input(query);
    draw(cx);
    cx.simulate_keystrokes("enter");
    draw(cx);
}

/// Clicks the element drawn with `selector`.
fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let drawn = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(drawn.center(), Modifiers::none());
    draw(cx);
}

fn suggestions_open(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> bool {
    cx.update(|_, cx| shell.read(cx).search().read(cx).is_open())
}

fn box_has_focus(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| shell.read(cx).search().read(cx).holds_focus(window, cx))
}

#[test]
fn ctrl_e_then_enter_shows_the_listed_chats_messages_the_newest_first_and_a_message_opens_its_chat_at_it() {
    let rig = seeded();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, trace);
    cx.simulate_keystrokes("ctrl-e");
    draw(cx);
    assert!(
        box_has_focus(&shell, cx),
        "Ctrl+E puts the keyboard in the title bar's search"
    );
    assert!(suggestions_open(&shell, cx));
    cx.simulate_input("zeppelin");
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Nothing,
        "typing only suggests"
    );
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Search);
    assert!(
        cx.debug_bounds("search-results").is_some(),
        "the results take the panes' place"
    );
    let hits = result_hits(&shell, cx);
    let shown: Vec<(&str, &str, &str, &str)> = hits
        .iter()
        .map(|(conversation, message, chat, sender, _)| {
            (conversation.as_str(), message.as_str(), chat.as_str(), sender.as_str())
        })
        .collect();
    assert_eq!(
        shown,
        [
            (TRIO, "9002", "Lab trio", "Lab Sea"),
            (AB, "9001", "Lab Bee", YOU),
            (AB, OLD, "Lab Bee", "Lab Bee"),
        ],
        "the newest first; the channel's post left out"
    );
    // the matched term is marked, the marks taken out of the text
    let (_, _, _, _, snippet) = &hits[2];
    assert!(snippet.contains("zeppelin"), "{snippet}");
    assert!(!snippet.contains(MATCH_START) && !snippet.contains(MATCH_END));
    let asked = rig.core.service_searches.lock().unwrap().clone();
    assert_eq!(
        asked.last(),
        Some(&("zeppelin".to_owned(), SearchScope::All)),
        "and the service asked"
    );

    click(cx, "search-message-7");
    opened_at(&shell, cx, AB, OLD);
    let view = chat_view(&shell, cx);
    assert!(
        cx.update(|_, cx| view.read(cx).held()) >= MANY - 6,
        "read back to the message, beyond the newest page"
    );
    shown_and_revealed_traced(&written);
}

/// The trace of the search above: the results shown (by their ids), the opened message revealed, and no query or
/// text.
fn shown_and_revealed_traced(written: &Written) {
    let search = changes(written, "search");
    let shown_line = search
        .iter()
        .rev()
        .find(|line| line["change"] == "shown")
        .expect("the results traced");
    assert_eq!(shown_line["messages"], serde_json::json!(["9002", "9001", OLD]));
    assert!(search.iter().any(|line| line["change"] == "opened"));
    assert!(search.iter().any(|line| line["change"] == "results"));
    assert!(!written.text().contains("zeppelin"), "no query or text in the trace");
    let revealed: Vec<serde_json::Value> = changes(written, "chat-view")
        .into_iter()
        .filter(|line| line["change"] == "revealed")
        .collect();
    assert_eq!(revealed.len(), 1);
    assert_eq!(revealed[0]["conversation"], AB);
    assert_eq!(revealed[0]["messages"], serde_json::json!([OLD]));
}

#[test]
fn hits_of_chats_the_list_doesn_t_show_never_take_the_listed_chats_places() {
    let rig = seeded();
    // a busy channel and the account's own stream: far more newer matches than the search shows
    let stream = "48:notifications";
    let mut messages = rig.core.messages.lock().unwrap();
    for (conversation, base) in [(CHANNEL, 10_000), (stream, 20_000)] {
        let posts = messages.entry(conversation.to_owned()).or_default();
        for n in 0..i64::try_from(4 * MAX_HITS).unwrap() {
            let id = (base + n).to_string();
            posts.push(message(
                conversation,
                &id,
                LAB_B,
                "<p>zeppelin again and again</p>",
                NOON + (10 + n) * MINUTE,
            ));
        }
    }
    drop(messages);
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    search_for(cx, "zeppelin");
    let shown: Vec<(String, String)> = result_hits(&shell, cx)
        .into_iter()
        .map(|(conversation, message, ..)| (conversation, message))
        .collect();
    assert_eq!(
        shown,
        [
            (TRIO.to_owned(), "9002".to_owned()),
            (AB.to_owned(), "9001".to_owned()),
            (AB.to_owned(), OLD.to_owned()),
        ],
        "the listed chats' hits, however many newer ones the others hold"
    );
}

#[test]
fn a_query_too_short_asks_the_store_nothing_and_a_longer_one_is_asked_as_entered() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    search_for(cx, "z");
    assert!(
        rig.core.searches.lock().unwrap().is_empty(),
        "one character asks the store nothing"
    );
    assert!(result_hits(&shell, cx).is_empty());
    cx.simulate_keystrokes("ctrl-e");
    cx.simulate_input("e");
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(*rig.core.searches.lock().unwrap(), ["ze"]);
    // FTS syntax goes to the engine as typed: the store quotes it
    cx.simulate_keystrokes("ctrl-e");
    cx.simulate_input(" (NEAR \"-*:");
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(
        rig.core.searches.lock().unwrap().last().map(String::as_str),
        Some("ze (NEAR \"-*:")
    );
    assert!(searchable("ab") && !searchable(" a ") && !searchable(""));
    assert_eq!(MIN_QUERY, 2);
}

#[test]
fn the_suggestions_list_the_people_and_chats_the_lists_know_at_once_and_down_enter_opens_one() {
    let rig = seeded();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, trace);
    cx.simulate_keystrokes("ctrl-e");
    draw(cx);
    // nothing typed: the recent chats
    let recent = cx.update(|_, cx| shell.read(cx).search().read(cx).suggestions().to_vec());
    assert!(
        recent
            .iter()
            .all(|suggestion| matches!(suggestion, Suggestion::Chat { .. }))
            && !recent.is_empty()
    );
    cx.simulate_input("trio");
    draw(cx);
    let suggested = cx.update(|_, cx| shell.read(cx).search().read(cx).suggestions().to_vec());
    assert!(
        matches!(&suggested[0], Suggestion::Chat { id, .. } if id == TRIO),
        "the trio's chat first: {suggested:?}"
    );
    assert_eq!(suggested.last(), Some(&Suggestion::Results), "then all the results");
    assert!(
        cx.debug_bounds("title-search-suggestions").is_some(),
        "drawn under the box"
    );
    cx.simulate_keystrokes("down enter");
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(TRIO.to_owned())
    );
    assert!(!suggestions_open(&shell, cx), "closed once one opened");
    let suggested_lines: Vec<serde_json::Value> = changes(&written, "search")
        .into_iter()
        .filter(|line| line["change"] == "suggested")
        .collect();
    assert!(!suggested_lines.is_empty());
    assert!(!written.text().contains("trio\""), "no query in the trace");
}

#[test]
fn the_service_s_people_files_and_messages_show_in_the_suggestions_and_the_results() {
    let rig = seeded();
    *rig.core.service_answer.lock().unwrap() = Found {
        people: vec![FoundPerson {
            mri: Some(LAB_B.to_owned()),
            name: "Lab Bee".to_owned(),
            email: Some("bee@lab.example.com".to_owned()),
            upn: None,
            addresses: Vec::new(),
            job_title: Some("Tester".to_owned()),
            department: None,
        }],
        files: vec![FoundFile {
            name: "Zeppelin plan.docx".to_owned(),
            url: "https://contoso.example.com/plan.docx".to_owned(),
            kind: Some("docx".to_owned()),
            location: Some("Lab site".to_owned()),
            modified_by: Some("Lab Sea".to_owned()),
            modified_ms: Some(NOON),
            snippet: "the zeppelin plan".to_owned(),
            matched: std::iter::once(4..12).collect(),
        }],
        messages: vec![FoundMessage {
            conversation: AB.to_owned(),
            id: "5".to_owned(),
            parent: Some("5".to_owned()),
            sent_ms: NOON - 200 * MINUTE,
            sender_mri: Some(LAB_B.to_owned()),
            sender_name: Some("Lab Bee".to_owned()),
            to: None,
            snippet: "an older zeppelin".to_owned(),
            matched: std::iter::once(9..17).collect(),
        }],
        ..Found::default()
    };
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    cx.simulate_keystrokes("ctrl-e");
    cx.simulate_input("bee");
    draw(cx);
    cx.executor().advance_clock(SUGGEST_DELAY);
    draw(cx);
    let suggested = cx.update(|_, cx| shell.read(cx).search().read(cx).suggestions().to_vec());
    let person = suggested
        .iter()
        .find(|suggestion| matches!(suggestion, Suggestion::Person { .. }))
        .expect("a person suggested");
    assert!(
        matches!(person, Suggestion::Person { chat: Some(chat), detail, .. } if chat == AB && detail == "Tester"),
        "b, with their job, opening the a–b chat: {person:?}"
    );
    assert!(
        suggested
            .iter()
            .any(|suggestion| matches!(suggestion, Suggestion::File { name, .. } if name == "Zeppelin plan.docx"))
    );
    let asked = rig.core.service_searches.lock().unwrap().clone();
    assert!(asked.contains(&("bee".to_owned(), SearchScope::Suggestions)));

    cx.simulate_keystrokes("enter");
    draw(cx);
    let search = cx.update(|_, cx| shell.read(cx).search().clone());
    let (people, files) = cx.update(|_, cx| (search.read(cx).people(), search.read(cx).files()));
    assert_eq!(people.len(), 1);
    assert_eq!(files.len(), 1);
    // the service's message is shown beside the store's, once
    let messages: Vec<String> = result_hits(&shell, cx).into_iter().map(|hit| hit.1).collect();
    assert!(messages.contains(&"5".to_owned()), "{messages:?}");
    // a tab of its own asks the service for that kind
    click(cx, "search-tab-people");
    let asked = rig.core.service_searches.lock().unwrap().clone();
    assert_eq!(
        asked.last(),
        Some(&("bee".to_owned(), SearchScope::Only(SearchKind::People)))
    );
    assert!(cx.debug_bounds("search-person-0").is_some());
    click(cx, "search-person-0");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(AB.to_owned())
    );
}

#[test]
fn ctrl_alt_g_goes_right_to_a_chat_or_a_channel_by_its_name() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    cx.simulate_keystrokes("ctrl-alt-g");
    draw(cx);
    let mode = cx.update(|_, cx| shell.read(cx).search().read(cx).mode());
    assert_eq!(mode, SearchMode::GoTo);
    cx.simulate_input("trio");
    draw(cx);
    let search = cx.update(|_, cx| shell.read(cx).search().clone());
    let (suggested, highlighted) = cx.update(|_, cx| {
        let search = search.read(cx);
        (search.suggestions().to_vec(), search.highlighted())
    });
    assert!(
        suggested
            .iter()
            .all(|suggestion| matches!(suggestion, Suggestion::Chat { .. } | Suggestion::Channel { .. })),
        "chats and channels only: {suggested:?}"
    );
    assert_eq!(highlighted, Some(0), "the first highlighted");
    assert!(
        rig.core.service_searches.lock().unwrap().is_empty(),
        "nothing asked of the service"
    );
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(TRIO.to_owned())
    );
    let mode = cx.update(|_, cx| shell.read(cx).search().read(cx).mode());
    assert_eq!(mode, SearchMode::Search, "the go-to mode ends");
}

#[test]
fn a_hit_in_the_chat_already_open_scrolls_its_view_to_the_message() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(AB.to_owned(), window, cx)));
    draw(cx);
    let before = chat_view(&shell, cx);
    assert!(cx.update(|_, cx| before.read(cx).list_state().is_following_tail()));
    assert_eq!(cx.update(|_, cx| before.read(cx).selected().map(str::to_owned)), None);
    search_for(cx, "zeppelin plan");
    click(cx, "search-message-7");
    opened_at(&shell, cx, AB, OLD);
    assert_eq!(chat_view(&shell, cx), before, "the same view");
}

/// The read receipts asked for, by message id.
fn receipts(rig: &Rig) -> Vec<String> {
    rig.core
        .actions
        .lock()
        .unwrap()
        .iter()
        .filter_map(|action| match action {
            Action::Read { message_id, .. } => Some(message_id.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_hit_s_chat_sends_no_read_receipt_for_the_newest_while_it_reads_back_to_the_hit() {
    let rig = seeded();
    // b's messages carry the `clientmessageid` a receipt names, none of them read: the newest would be due
    for message in rig.core.messages.lock().unwrap().get_mut(AB).unwrap() {
        if message.sender.as_deref() == Some(LAB_B) {
            message.client_message_id = Some(format!("1{}", message.id));
        }
    }
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    cx.update(|window, _| window.activate_window());
    draw(cx);
    search_for(cx, "zeppelin plan");
    // the newest page's load is answered, the load reading back to the hit held (the engine busy elsewhere)
    *rig.core.unsettled_before_hold.lock().unwrap() = 1;
    *rig.core.hold_unsettled.lock().unwrap() = true;
    click(cx, "search-message-7");
    assert_eq!(
        rig.core.held_unsettled.lock().unwrap().len(),
        1,
        "the reveal's load waits"
    );
    for _ in 0..3 {
        cx.executor().advance_clock(READ_DEBOUNCE);
        draw(cx);
    }
    assert!(receipts(&rig).is_empty(), "none before the hit is shown");

    *rig.core.hold_unsettled.lock().unwrap() = false;
    let held: Vec<_> = rig.core.held_unsettled.lock().unwrap().drain(..).collect();
    for answer in held {
        answer.send(Ok(Vec::new()));
    }
    draw(cx);
    opened_at(&shell, cx, AB, OLD);
    cx.executor().advance_clock(READ_DEBOUNCE);
    draw(cx);
    assert!(receipts(&rig).is_empty(), "nor once the list stays at the hit");
    let view = chat_view(&shell, cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).receipts()), 0);
}

#[test]
fn escape_closes_the_suggestions_and_leaves_the_results_for_where_the_user_was() {
    let rig = seeded();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, trace);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(TRIO.to_owned(), window, cx)));
    draw(cx);
    cx.simulate_keystrokes("ctrl-e");
    cx.simulate_input("zeppelin");
    draw(cx);
    assert!(suggestions_open(&shell, cx));
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!suggestions_open(&shell, cx), "Escape closes them whatever was typed");
    assert!(!box_has_focus(&shell, cx), "and gives the keyboard back");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(TRIO.to_owned())
    );

    search_for(cx, "zeppelin");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Search);
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(TRIO.to_owned()),
        "Escape on the results goes back to the chat"
    );
    let named: Vec<String> = changes(&written, "search")
        .iter()
        .map(|line| line["change"].as_str().unwrap().to_owned())
        .filter(|change| change == "opened" || change == "closed")
        .collect();
    assert_eq!(
        named,
        ["opened", "closed", "opened", "closed"],
        "{:?}",
        changes(&written, "search")
    );
}

#[test]
fn an_escape_meant_for_the_compose_box_is_never_the_search_s() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    // an active window: the box hears that it lost the keyboard
    cx.update(|window, _| window.activate_window());
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(AB.to_owned(), window, cx)));
    draw(cx);
    let view = chat_view(&shell, cx);
    cx.simulate_keystrokes("ctrl-e");
    draw(cx);
    assert!(box_has_focus(&shell, cx));
    act(&view, cx, MessageAction::Reply, "9001");
    assert!(matches!(mode(&view, cx), Mode::Reply(_)), "replying");
    assert!(!box_has_focus(&shell, cx), "the compose box has the keyboard");
    assert!(
        !suggestions_open(&shell, cx),
        "the suggestions closed as the box let go"
    );
    escape(cx);
    assert_eq!(mode(&view, cx), Mode::New, "the compose box's Escape cancels the reply");
}

#[test]
fn a_click_in_the_suggestions_keeps_the_box_s_keyboard() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    cx.simulate_keystrokes("ctrl-e");
    cx.simulate_input("qqqqqq");
    draw(cx);
    let drawn = cx
        .debug_bounds("title-search-suggestions")
        .expect("the suggestions are drawn");
    // the chips' row, which opens nothing but where it is clicked
    let at = point(drawn.right() - px(8.0), drawn.top() + px(8.0));
    cx.simulate_click(at, Modifiers::none());
    draw(cx);
    assert!(box_has_focus(&shell, cx), "the box keeps the keyboard");
    assert!(suggestions_open(&shell, cx), "and the suggestions stay");
    cx.simulate_input("x");
    draw(cx);
    let query = cx.update(|_, cx| shell.read(cx).search().read(cx).query(cx));
    assert_eq!(query, "qqqqqqx");
}

/// The dialog Ctrl+K (the switcher) opens.
fn switcher_open(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> bool {
    cx.update(|_, cx| shell.read(cx).switcher().is_some())
}

fn mode(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Mode {
    cx.update(|_, cx| view.read(cx).compose().read(cx).mode().clone())
}

fn act(view: &Entity<ChatView>, cx: &mut VisualTestContext, action: MessageAction, message: &str) {
    cx.update(|window, cx| view.update(cx, |view, cx| view.act(action, message, window, cx)));
    draw(cx);
}

fn escape(cx: &mut VisualTestContext) {
    cx.simulate_keystrokes("escape");
    draw(cx);
}

/// With the switcher open and the focus moved out of it, into the compose box, an Escape is the box's; with the focus
/// back in its field it is the switcher's, and Ctrl+K closes it wherever the focus is.
#[test]
fn an_escape_meant_for_the_compose_box_is_never_the_switcher_s() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(AB.to_owned(), window, cx)));
    draw(cx);
    let view = chat_view(&shell, cx);
    let holds = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| shell.read(cx).switcher().unwrap().read(cx).holds_focus(window, cx))
    };
    cx.simulate_keystrokes("ctrl-k");
    draw(cx);
    assert!(switcher_open(&shell, cx));
    assert!(holds(cx), "the switcher takes the focus");
    act(&view, cx, MessageAction::Reply, "9001");
    assert!(!holds(cx), "the box has the focus");
    escape(cx);
    assert_eq!(mode(&view, cx), Mode::New, "the box's Escape cancels the reply");
    assert!(switcher_open(&shell, cx), "the switcher doesn't take the box's Escape");
    act(&view, cx, MessageAction::Edit, "9001");
    escape(cx);
    assert_eq!(mode(&view, cx), Mode::New, "nor the edit's");
    assert!(switcher_open(&shell, cx));
    // the focus back in its field: Escape is the switcher's again
    cx.update(|window, cx| {
        let switcher = shell.read(cx).switcher().cloned().unwrap();
        switcher.update(cx, |switcher, cx| switcher.focus(window, cx));
    });
    draw(cx);
    escape(cx);
    assert!(!switcher_open(&shell, cx), "Escape in its field closes it");
    // and its key closes it wherever the focus is
    cx.simulate_keystrokes("ctrl-k");
    draw(cx);
    act(&view, cx, MessageAction::Reply, "9001");
    cx.simulate_keystrokes("ctrl-k");
    draw(cx);
    assert!(!switcher_open(&shell, cx), "Ctrl+K closes it from the box");
    assert!(matches!(mode(&view, cx), Mode::Reply(_)), "the reply kept");
}

#[test]
fn escape_closes_the_switcher_once_its_list_was_clicked() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    cx.simulate_keystrokes("ctrl-k");
    // a query nothing matches: the list holds only its empty text, which no click chooses
    cx.simulate_input("qqqqqq");
    draw(cx);
    let drawn = cx.debug_bounds("switcher").expect("the switcher is drawn");
    let at = point(drawn.center().x, drawn.bottom() - px(8.0));
    cx.simulate_click(at, Modifiers::none());
    draw(cx);
    assert!(switcher_open(&shell, cx), "a click in it keeps it open");
    cx.simulate_input("x");
    draw(cx);
    let query = cx.update(|_, cx| shell.read(cx).switcher().unwrap().read(cx).query().to_owned());
    assert_eq!(query, "qqqqqq", "the focus left its field");
    escape(cx);
    assert!(!switcher_open(&shell, cx), "Escape closes it");
}

#[test]
fn once_the_engine_stopped_a_query_shows_no_hit() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    // the backfill's end changes nothing shown
    rig.send(Event::Indexed { messages: 3 });
    let shutdown = cx.update(|_, cx| rig.bridge.update(cx, teams_ui::Bridge::shutdown));
    shutdown.detach();
    draw(cx);
    assert_eq!(rig.shutdowns(), 1);
    // the stopped engine answers every request `Stopped`
    *rig.core.stopped.lock().unwrap() = true;
    search_for(cx, "zeppelin");
    assert!(rig.core.asked().contains(&"search"), "asked, and answered stopped");
    assert!(result_hits(&shell, cx).is_empty());
    assert!(cx.debug_bounds("search-results-empty").is_some(), "said so");
}

#[test]
fn back_and_forward_go_through_the_places_shown_and_the_list_pane_hides() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    assert!(cx.debug_bounds("title-bar").is_some(), "the title bar is drawn");
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(AB.to_owned(), window, cx)));
    draw(cx);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(TRIO.to_owned(), window, cx)));
    draw(cx);
    // the rail's Settings: the keyboard on their pages
    click(cx, "rail-settings");
    cx.simulate_keystrokes("alt-left");
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(TRIO.to_owned())
    );
    click(cx, "title-back");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(AB.to_owned())
    );
    cx.simulate_keystrokes("alt-right");
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(TRIO.to_owned())
    );
    // a new place drops what was ahead
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.show_calendar(window, cx)));
    draw(cx);
    assert!(!cx.update(|_, cx| shell.read(cx).history().can_go_forward()));

    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(AB.to_owned(), window, cx)));
    draw(cx);
    assert!(cx.debug_bounds("list-pane").is_some());
    click(cx, "title-list-toggle");
    assert!(cx.debug_bounds("list-pane").is_none(), "the list pane hidden");
    assert!(cx.debug_bounds("conversation-pane").is_some());
    click(cx, "title-list-toggle");
    assert!(cx.debug_bounds("list-pane").is_some(), "and shown again");
}

#[test]
fn the_service_s_marked_text_becomes_plain_text_and_the_ranges_of_its_matches() {
    let (text, matched) = highlighted("Acme <c0>Lab</c0>s &amp; the <c0>lab</c0><ddd/> <a href=\"x\">link</a>");
    assert_eq!(text, "Acme Labs & the lab… link");
    let parts: Vec<&str> = matched.iter().map(|range| &text[range.clone()]).collect();
    assert_eq!(parts, ["Lab", "lab"]);
}

fn team_in_search(rig: &Rig) {
    *rig.core.teams.lock().unwrap() = Some(vec![Team {
        id: CHANNEL.to_owned(),
        name: "Lab rocket".to_owned(),
        archived: false,
        group_id: None,
        mailbox: None,
        owner: false,
        can_add_channel: false,
        can_add_private_channel: false,
        about: teams_store::TeamAbout::default(),
        channels: vec![TeamChannel {
            id: CHANNEL.to_owned(),
            name: "Release notes".to_owned(),
            general: true,
            kind: ChannelKind::Standard,
            about: teams_store::ChannelAbout::default(),
            shown: true,
        }],
    }]);
}

#[test]
fn go_to_finds_a_local_channel_by_its_team_and_channel_names_without_contacting_anyone() {
    let rig = seeded();
    team_in_search(&rig);
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, trace);
    cx.simulate_keystrokes("ctrl-alt-g");
    cx.simulate_input("Release");
    draw(cx);
    let suggestions = cx.update(|_, cx| shell.read(cx).search().read(cx).suggestions().to_vec());
    assert!(suggestions.iter().any(|suggestion| matches!(suggestion,
        Suggestion::Channel { id, name, team } if id == CHANNEL && name == "Release notes" && team == "Lab rocket")));
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Channel(CHANNEL.to_owned())
    );
    assert!(rig.core.synced.lock().unwrap().contains(&CHANNEL.to_owned()));
    assert!(rig.core.actions.lock().unwrap().is_empty());
    assert!(!written.text().contains("Release notes"));
    assert!(!written.text().contains("Lab rocket"));
}

#[test]
fn group_and_channel_results_merge_local_and_service_entries_and_open_the_selected_place() {
    let rig = seeded();
    team_in_search(&rig);
    let remote_chat = "19:synthetic-service-chat@thread.v2";
    let remote_channel = "19:synthetic-service-channel@thread.tacv2";
    *rig.core.service_answer.lock().unwrap() = Found {
        chats: vec![
            FoundChat {
                id: TRIO.to_owned(),
                name: "Service name".to_owned(),
            },
            FoundChat {
                id: remote_chat.to_owned(),
                name: "Lab remote chat".to_owned(),
            },
        ],
        channels: vec![
            FoundChannel {
                id: CHANNEL.to_owned(),
                name: "Service channel name".to_owned(),
                team: None,
            },
            FoundChannel {
                id: remote_channel.to_owned(),
                name: "Lab remote channel".to_owned(),
                team: Some("Remote team".to_owned()),
            },
        ],
        ..Found::default()
    };
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    search_for(cx, "Lab");
    let search = cx.update(|_, cx| shell.read(cx).search().clone());
    cx.update(|_, cx| {
        let search = search.read(cx);
        assert_eq!(search.results().unwrap().query(), "Lab");
        assert_eq!(search.results().unwrap().tab(), teams_ui::global_search::Tab::All);
        assert_eq!(search.group_chats(cx).len(), 2);
        assert_eq!(search.channels(cx).len(), 2);
    });
    click(cx, "search-see-all-group-chats");
    assert_eq!(
        rig.core.service_searches.lock().unwrap().last(),
        Some(&("Lab".to_owned(), SearchScope::Only(SearchKind::Chats)))
    );
    click(cx, "search-chat-1");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(remote_chat.to_owned())
    );
    cx.simulate_keystrokes("ctrl-e ctrl-a");
    cx.simulate_input("Lab");
    draw(cx);
    cx.simulate_keystrokes("enter");
    draw(cx);
    click(cx, "search-tab-channels");
    let channels = cx.update(|_, cx| search.read(cx).channels(cx));
    assert!(matches!(&channels[0], Suggestion::Channel { name, .. } if name == "Release notes"));
    click(cx, "search-channel-1");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Channel(remote_channel.to_owned())
    );
    assert!(rig.core.actions.lock().unwrap().is_empty());
}

fn new_person(rig: &Rig) {
    rig.core.service_answer.lock().unwrap().people = vec![FoundPerson {
        mri: Some(LAB_C.to_owned()),
        name: "Synthetic analyst".to_owned(),
        email: Some("analyst@lab.example.test".to_owned()),
        upn: None,
        addresses: Vec::new(),
        job_title: None,
        department: None,
    }];
}

fn created_chat(state: teams_store::OutboxState) -> teams_store::OutboxEntry {
    let mut row = common::outbox_row(17, teams_store::OutboxKind::CreateChat, state);
    row.message_id = Some("19:new-synthetic-chat@thread.v2".to_owned());
    row
}

#[test]
fn clicking_a_person_without_a_listed_chat_opens_the_created_chat_from_suggestions_and_results() {
    for route in ["suggestion", "people", "keyboard"] {
        let rig = seeded();
        new_person(&rig);
        rig.core
            .write_answers
            .lock()
            .unwrap()
            .push_back(created_chat(teams_store::OutboxState::Sent));
        let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
        cx.simulate_keystrokes("ctrl-e");
        cx.simulate_input("analyst");
        draw(cx);
        cx.executor().advance_clock(SUGGEST_DELAY);
        draw(cx);
        assert!(
            rig.core.actions.lock().unwrap().is_empty(),
            "searching never creates a chat"
        );
        if route == "people" {
            click(cx, "title-search-chip-people");
            click(cx, "search-person-0");
        } else if route == "keyboard" {
            cx.simulate_keystrokes("down enter");
            draw(cx);
        } else {
            click(cx, "title-search-person-0");
        }
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).main().clone()),
            Main::Chat("19:new-synthetic-chat@thread.v2".to_owned())
        );
        assert!(
            matches!(rig.core.actions.lock().unwrap().as_slice(), [Action::CreateChat { others, .. }] if others.iter().map(teams_proto::Mri::as_str).collect::<Vec<_>>() == [LAB_C])
        );
        assert!(!suggestions_open(&shell, cx));
    }
}

#[test]
fn a_pending_chat_creation_is_watched_without_duplicate_writes_and_opens_when_sent() {
    let rig = seeded();
    new_person(&rig);
    rig.core
        .write_answers
        .lock()
        .unwrap()
        .push_back(created_chat(teams_store::OutboxState::Pending));
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    search_for(cx, "analyst");
    click(cx, "search-person-0");
    click(cx, "search-person-0");
    assert_eq!(rig.core.actions.lock().unwrap().len(), 1);
    // A temporarily unreadable row is still pending: another click must not enqueue a replacement.
    cx.executor().advance_clock(std::time::Duration::from_millis(500));
    draw(cx);
    click(cx, "search-person-0");
    assert_eq!(rig.core.actions.lock().unwrap().len(), 1);
    rig.core
        .outbox_answers
        .lock()
        .unwrap()
        .push_back(created_chat(teams_store::OutboxState::Sent));
    cx.executor().advance_clock(std::time::Duration::from_millis(500));
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat("19:new-synthetic-chat@thread.v2".to_owned())
    );
}

#[test]
fn a_person_without_a_chat_reports_denial_missing_identity_and_read_only_without_navigating() {
    for case in ["denied", "missing", "read-only"] {
        let rig = seeded();
        new_person(&rig);
        if case == "missing" {
            rig.core.service_answer.lock().unwrap().people[0].mri = None;
        }
        if case == "read-only" {
            *rig.core.read_only.lock().unwrap() = true;
        }
        rig.core
            .write_answers
            .lock()
            .unwrap()
            .push_back(created_chat(teams_store::OutboxState::Denied));
        let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
        search_for(cx, "analyst");
        click(cx, "search-person-0");
        assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Search);
        assert!(cx.debug_bounds("toast").is_some());
        let expected = match case {
            "denied" => "We couldn't open the chat. Check your connection and try again.",
            "missing" => "This account has no chat address.",
            _ => "A new chat can't be opened in read-only mode.",
        };
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).toast().map(ToString::to_string)),
            Some(expected.to_owned())
        );
        assert_eq!(rig.core.actions.lock().unwrap().len(), usize::from(case == "denied"));
    }
}

#[test]
fn service_only_channel_and_group_suggestions_open_their_destinations_on_click() {
    for channel in [false, true] {
        let rig = seeded();
        let remote = if channel {
            "19:remote-channel@thread.tacv2"
        } else {
            "19:remote-chat@thread.v2"
        };
        if channel {
            rig.core.service_answer.lock().unwrap().channels.push(FoundChannel {
                id: remote.to_owned(),
                name: "Remote".to_owned(),
                team: None,
            });
        } else {
            rig.core.service_answer.lock().unwrap().chats.push(FoundChat {
                id: remote.to_owned(),
                name: "Remote".to_owned(),
            });
        }
        let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
        cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(AB.to_owned(), window, cx)));
        draw(cx);
        cx.simulate_keystrokes("ctrl-e");
        cx.simulate_input("Remote");
        draw(cx);
        cx.executor().advance_clock(SUGGEST_DELAY);
        draw(cx);
        let selector = if channel {
            "title-search-channel-0"
        } else {
            "title-search-chat-0"
        };
        let row = cx.debug_bounds(selector).expect("the suggestion is drawn");
        // Even the blank right edge owns its click instead of focusing the conversation underneath.
        cx.simulate_click(point(row.right() - px(8.0), row.center().y), Modifiers::none());
        draw(cx);
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).main().clone()),
            if channel {
                Main::Channel(remote.to_owned())
            } else {
                Main::Chat(remote.to_owned())
            }
        );
        assert!(rig.core.actions.lock().unwrap().is_empty());
    }
}

#[test]
fn a_chat_creation_that_finishes_after_navigation_does_not_take_the_user_back() {
    let rig = seeded();
    new_person(&rig);
    rig.core
        .write_answers
        .lock()
        .unwrap()
        .push_back(created_chat(teams_store::OutboxState::Pending));
    rig.core
        .outbox_answers
        .lock()
        .unwrap()
        .push_back(created_chat(teams_store::OutboxState::Sent));
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    search_for(cx, "analyst");
    click(cx, "search-person-0");
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(AB.to_owned(), window, cx)));
    draw(cx);
    cx.executor().advance_clock(std::time::Duration::from_millis(500));
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(AB.to_owned())
    );
    assert_eq!(rig.core.actions.lock().unwrap().len(), 1);
}

#[test]
fn a_message_in_a_service_only_channel_opens_the_channel_at_its_thread() {
    let rig = seeded();
    let channel = "19:remote-channel@thread.tacv2";
    rig.core.service_answer.lock().unwrap().messages.push(FoundMessage {
        id: "9003".to_owned(),
        conversation: channel.to_owned(),
        sent_ms: NOON,
        sender_mri: Some(LAB_B.to_owned()),
        sender_name: Some("Lab Bee".to_owned()),
        to: Some("Remote channel".to_owned()),
        snippet: "Remote post".to_owned(),
        matched: Vec::new(),
        parent: Some("9003".to_owned()),
    });
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    search_for(cx, "Remote");
    click(cx, "search-message-9003");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Channel(channel.to_owned())
    );
    assert!(rig.core.synced.lock().unwrap().contains(&channel.to_owned()));
    assert!(rig.core.actions.lock().unwrap().is_empty());
}

#[test]
fn suggested_files_and_the_files_chip_open_only_the_mock_browser_url_and_clear_keeps_focus() {
    let rig = seeded();
    let file = |url: &str| FoundFile {
        name: "Synthetic manual.pdf".to_owned(),
        url: url.to_owned(),
        kind: Some("pdf".to_owned()),
        location: None,
        modified_by: None,
        modified_ms: None,
        snippet: String::new(),
        matched: Vec::new(),
    };
    rig.core.service_answer.lock().unwrap().files = vec![file("https://files.example.test/first.pdf")];
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    let search = cx.update(|_, cx| shell.read(cx).search().clone());
    cx.update(|window, cx| search.update(cx, |search, cx| search.set_query("manual", window, cx)));
    draw(cx);
    cx.executor().advance_clock(SUGGEST_DELAY);
    draw(cx);
    let at = cx.update(|_, cx| {
        search
            .read(cx)
            .suggestions()
            .iter()
            .position(|suggestion| matches!(suggestion, Suggestion::File { .. }))
            .unwrap()
    });
    cx.update(|window, cx| search.update(cx, |search, cx| search.choose(at, window, cx)));
    draw(cx);
    assert_eq!(cx.opened_url().as_deref(), Some("https://files.example.test/first.pdf"));
    rig.core.service_answer.lock().unwrap().files = vec![file("https://files.example.test/second.pdf")];
    cx.simulate_keystrokes("ctrl-e");
    cx.simulate_input("manual again");
    draw(cx);
    click(cx, "title-search-chip-files");
    click(cx, "search-file-0");
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://files.example.test/second.pdf")
    );
    cx.simulate_keystrokes("ctrl-e");
    draw(cx);
    click(cx, "title-search-clear");
    assert_eq!(cx.update(|_, cx| search.read(cx).query(cx)), "");
    assert!(box_has_focus(&shell, cx));
    assert!(rig.core.actions.lock().unwrap().is_empty());
}

#[test]
fn a_snippet_s_marks_become_the_ranges_of_its_matches() {
    let snippet = format!("…the {MATCH_START}zep{MATCH_END}pelin and {MATCH_START}plan{MATCH_END}…");
    let (text, matched) = split_snippet(&snippet);
    assert_eq!(text, "…the zeppelin and plan…");
    let parts: Vec<&str> = matched.iter().map(|range| &text[range.clone()]).collect();
    assert_eq!(parts, ["zep", "plan"]);
    // an end without a start, or an empty match, marks nothing
    let (text, matched) = split_snippet(&format!("a{MATCH_END}b{MATCH_START}{MATCH_END}c"));
    assert_eq!(text, "abc");
    assert!(matched.is_empty());
}

#[test]
fn a_translated_chat_s_found_message_shows_in_its_language() {
    let rig = seeded();
    {
        let mut messages = rig.core.messages.lock().unwrap();
        let old = messages
            .get_mut(AB)
            .unwrap()
            .iter_mut()
            .find(|message| message.id == OLD)
            .unwrap();
        old.message_type = "Text".to_owned();
        old.content = "the zeppelin plan".to_owned();
    }
    let dir = tempfile::tempdir().unwrap();
    let program = fake_translator::fake(dir.path(), fake_translator::FAKE_CLAUDE);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, program)])));
    let mut settings = Settings::default();
    settings.set_chat_language(LAB_A, AB, Some("de"));
    let (trace, written) = trace();
    let parts = parts::parts(&rig, settings, trace);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.simulate_resize(size(px(1000.0), px(700.0)));
    draw(cx);
    search_for(cx, "zeppelin");
    let snippet_of = |hits: &[(String, String, String, String, String)], id: &str| {
        hits.iter()
            .find(|hit| hit.1 == id)
            .map(|hit| hit.4.clone())
            .unwrap_or_default()
    };
    let mut hits = result_hits(&shell, cx);
    for _ in 0..1500 {
        if snippet_of(&hits, OLD).starts_with("DE") {
            break;
        }
        cx.executor().advance_clock(teams_ui::chat_translate::previews::GATHER);
        draw(cx);
        std::thread::sleep(std::time::Duration::from_millis(10));
        hits = result_hits(&shell, cx);
    }
    assert_eq!(snippet_of(&hits, OLD), "DE the zeppelin plan");
    // the account's own shows as written; a chat not translated as found
    assert!(snippet_of(&hits, "9001").contains("zeppelin again"));
    assert!(snippet_of(&hits, "9002").contains("a zeppelin in the trio"));
    let text = written.text();
    assert!(!text.contains("DE the"), "nothing translated is traced");
}

#[test]
fn a_translated_card_marks_the_query_s_words_where_they_are() {
    assert_eq!(marked_terms("Der Zeppelin-Plan, zeppelin", "zeppelin"), [4..12, 19..27]);
    assert_eq!(
        marked_terms("nothing here", "zeppelin plan"),
        Vec::<std::ops::Range<usize>>::new()
    );
    // a text whose lowercase changes its length isn't marked
    assert!(marked_terms("İstanbul zeppelin", "zeppelin").is_empty());
    assert_eq!(snippet_line("  two\n lines  "), "two lines");
    let long = "word ".repeat(100);
    let cut = snippet_line(&long);
    assert_eq!(cut.chars().count(), teams_ui::search::SNIPPET_CHARS);
    assert!(cut.ends_with('…'));
}
