//! Compose and the writes from the UI (M2 Task 13) on GPUI's test context, over the fake engine (`common`), which
//! records every write and transient it is asked for: the typed text as a message's text (lines as paragraphs, a URL
//! as text, as the capture's link inserted with its text; mentions by MRI only), Enter and Shift-Enter, a refused
//! send's draft given back, the mention picker over the chat's members (closed once the cursor leaves its `@`), a
//! reply with its quote, an edit (Up in an empty box edits the last), a delete confirmed first (on the message
//! selected), the quick reactions and "more reactions" (a chip
//! of a key Teams doesn't offer only shows), typing (its throttle, kept across a chat view
//! built again, the focus, the setting, never while editing), read receipts (the chat shown, the window active, the
//! list at the newest, the setting, one per burst), and the outbox rows shown with their states and "send again" (a
//! queued post at once, a sent one until its message is held or stored, a write a newer one made out of date never).
//! The same writes on two accounts' engines on the mock are `tests/compose_mock.rs`. Every id, name and text is made
//! up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;
// the shell's and a chat view's parts over the rig
#[path = "common/parts.rs"]
mod parts;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{LAB_A, LAB_B, Rig, Written, outbox_row};
use gpui::{
    Entity, Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, VisualTestContext, point, px, size,
};
use teams_core::{Action, Command, Event, MessageChange, Source, Transient};
use teams_proto::Mri;
use teams_proto::chat::{ComposeError, MessageText, Node, RichText};
use teams_store::{Applied, Member, Message, OutboxEntry, OutboxState, Reaction, ReadHorizon};
use teams_ui::ViewTrace;
use teams_ui::bridge::Core;
use teams_ui::chat_view::{
    ChatView, DEFAULT_QUICK_REACTIONS, MessageAction, PAGE, QUICK_COUNT, READ_DEBOUNCE, quick_reactions,
};
use teams_ui::compose::{
    Candidate, Compose, EVERYONE_MAX_MEMBERS, Mode, PREVIEW_MAX, Quoted, READ_ONLY_APP, READ_ONLY_NOTES, TYPING_EVERY,
    TypingThrottle, load, matching, mention_query, message_of, offered, plain_doc, quotes_of,
};
use teams_ui::editor::CantLoad;
use teams_ui::editor::doc::{Attrs, Block, Doc, Kind, Run};
use teams_ui::reaction_picker::{RECENT, Section};
use teams_ui::sending::{Again, RowState};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::Shell;

const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
/// A group chat (made up).
const GROUP: &str = "19:labgroup0001@thread.v2";
/// Someone outside the lab (made up).
const OUTSIDER: &str = "8:orgid:00000000-0000-4000-8000-0000000000ff";
/// 2026-01-01T12:00:00Z, ms.
const NOON: i64 = 1_767_268_800_000;
const MINUTE: i64 = 60_000;

#[test]
fn dropped_files_preserve_the_draft_deduplicate_and_send_with_it() {
    let rig = rig_with(vec![message(NOON, LAB_B, "<p>hello</p>")]);
    let (view, cx, _) = chat_window(&rig, Settings::default());
    let compose = compose(&view, cx);
    let dir = tempfile::tempdir().unwrap();
    let paths = [dir.path().join("one.txt"), dir.path().join("two.pdf")];
    for path in &paths {
        std::fs::write(path, b"synthetic file").unwrap();
    }
    type_text(cx, "Keep this draft");
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.attach_files(&paths, window, cx)));
    draw(cx);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.attach_files(&paths, window, cx)));
    draw(cx);
    assert_eq!(typed(&view, cx), "Keep this draft");
    assert_eq!(rig.core.uploads.lock().unwrap().len(), 2);
    assert_eq!(cx.update(|_, cx| compose.read(cx).attachments().len()), 2);
    click(cx, "compose-attachment-remove-0");
    assert_eq!(typed(&view, cx), "Keep this draft");
    keys(cx, "enter");
    let actions = actions(&rig);
    let Action::Send { text, .. } = &actions[0] else {
        panic!("file message")
    };
    assert_eq!(text.html(), "<p>Keep this draft</p>");
    assert_eq!(text.files().len(), 1);
    assert_eq!(text.files()[0].name(), "two.pdf");
    assert!(cx.update(|_, cx| compose.read(cx).attachments().is_empty()));
}

#[test]
fn switching_chats_preserves_text_files_and_upload_completion() {
    let rig = rig_with(Vec::new());
    *rig.core.hold_uploads.lock().unwrap() = true;
    let settings = Settings {
        selected_chat: Some(AB.to_owned()),
        ..Settings::default()
    };
    let parts = parts::parts(&rig, settings, Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    draw(cx);
    let first = cx.update(|_, cx| shell.read(cx).chat().unwrap().read(cx).compose().clone());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("draft.txt");
    std::fs::write(&file, b"test").unwrap();
    type_text(cx, "Draft with a file");
    cx.update(|window, cx| first.update(cx, |compose, cx| compose.attach_files(&[file], window, cx)));
    draw(cx);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(GROUP.to_owned(), window, cx)));
    draw(cx);
    rig.core
        .held_uploads
        .lock()
        .unwrap()
        .remove(0)
        .send(Ok(common::uploaded_file("renamed draft.txt")));
    draw(cx);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(AB.to_owned(), window, cx)));
    draw(cx);
    let restored = cx.update(|_, cx| shell.read(cx).chat().unwrap().read(cx).compose().clone());
    assert_eq!(first, restored);
    assert_eq!(
        cx.update(|_, cx| restored.read(cx).attachments()[0].name.clone()),
        "renamed draft.txt"
    );
    keys(cx, "enter");
    let sent = actions(&rig);
    let Action::Send { text, conversation } = &sent[0] else {
        panic!("message")
    };
    assert_eq!(conversation, AB);
    assert_eq!(text.html(), "<p>Draft with a file</p>");
    assert_eq!(text.files()[0].name(), "renamed draft.txt");
}

#[test]
fn a_refused_file_send_survives_navigation_and_keeps_newer_text() {
    for newer in ["", "Newer draft"] {
        let rig = rig_with(Vec::new());
        let settings = Settings {
            selected_chat: Some(AB.to_owned()),
            ..Settings::default()
        };
        let parts = parts::parts(&rig, settings, Arc::new(ViewTrace::off()));
        let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
        let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
        draw(cx);
        let compose = cx.update(|_, cx| shell.read(cx).chat().unwrap().read(cx).compose().clone());
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("draft.txt");
        std::fs::write(&file, b"test").unwrap();
        type_text(cx, "Unsent draft");
        cx.update(|window, cx| compose.update(cx, |compose, cx| compose.attach_files(&[file], window, cx)));
        draw(cx);
        *rig.core.stopped.lock().unwrap() = true;
        cx.update(|window, cx| {
            compose.update(cx, |compose, cx| {
                compose.send(window, cx);
                compose
                    .editor()
                    .update(cx, |editor, cx| editor.set_doc(typed_doc(newer, &[]), cx));
            });
            shell.update(cx, |shell, cx| shell.open_chat(GROUP.to_owned(), window, cx));
        });
        draw(cx);
        cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(AB.to_owned(), window, cx)));
        draw(cx);
        let restored = cx.update(|_, cx| shell.read(cx).chat().unwrap().read(cx).compose().clone());
        assert_eq!(restored, compose);
        let value = cx.update(|_, cx| restored.read(cx).value(cx).to_string());
        assert!(value.contains("Unsent draft"), "{value}");
        assert!(value.contains(newer), "{value}");
        assert_eq!(cx.update(|_, cx| restored.read(cx).attachments().len()), 1);
    }
}

#[test]
fn a_file_only_message_is_sendable_after_upload_and_never_while_uploading() {
    let rig = rig_with(Vec::new());
    *rig.core.hold_uploads.lock().unwrap() = true;
    let (view, cx, _) = chat_window(&rig, Settings::default());
    let compose = compose(&view, cx);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("only.txt");
    std::fs::write(&file, b"test").unwrap();
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.attach_files(&[file], window, cx)));
    draw(cx);
    keys(cx, "enter");
    assert!(actions(&rig).is_empty());
    rig.core
        .held_uploads
        .lock()
        .unwrap()
        .remove(0)
        .send(Ok(common::uploaded_file("only.txt")));
    draw(cx);
    keys(cx, "enter");
    let actions = actions(&rig);
    let Action::Send { text, .. } = &actions[0] else {
        panic!("file message")
    };
    assert!(text.html().is_empty());
    assert_eq!(text.files().len(), 1);
}

#[test]
fn removing_an_upload_and_reattaching_ignores_the_old_completion() {
    let rig = rig_with(Vec::new());
    *rig.core.hold_uploads.lock().unwrap() = true;
    let (view, cx, _) = chat_window(&rig, Settings::default());
    let compose = compose(&view, cx);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("again.txt");
    std::fs::write(&file, b"test").unwrap();
    cx.update(|window, cx| {
        compose.update(cx, |compose, cx| {
            compose.attach_files(std::slice::from_ref(&file), window, cx);
        });
    });
    draw(cx);
    click(cx, "compose-attachment-remove-0");
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.attach_files(&[file], window, cx)));
    draw(cx);
    rig.core
        .held_uploads
        .lock()
        .unwrap()
        .remove(0)
        .send(Ok(common::uploaded_file("old.txt")));
    draw(cx);
    assert!(cx.update(|_, cx| matches!(
        compose.read(cx).attachments()[0].state,
        teams_ui::compose::attachments::AttachmentState::Uploading
    )));
    rig.core
        .held_uploads
        .lock()
        .unwrap()
        .remove(0)
        .send(Ok(common::uploaded_file("again.txt")));
    draw(cx);
    assert!(cx.update(|_, cx| matches!(
        compose.read(cx).attachments()[0].state,
        teams_ui::compose::attachments::AttachmentState::Ready(_)
    )));
}

#[test]
fn failed_uploads_can_be_retried_and_a_refused_send_restores_files_and_text() {
    let rig = rig_with(Vec::new());
    rig.core
        .upload_answers
        .lock()
        .unwrap()
        .push_back(Err(teams_core::EngineError::FileUnavailable));
    let (view, cx, _) = chat_window(&rig, Settings::default());
    let compose = compose(&view, cx);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("retry.txt");
    std::fs::write(&file, b"test").unwrap();
    type_text(cx, "preserve me");
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.attach_files(&[file], window, cx)));
    draw(cx);
    assert!(cx.update(|_, cx| matches!(
        compose.read(cx).attachments()[0].state,
        teams_ui::compose::attachments::AttachmentState::Failed(_)
    )));
    click(cx, "compose-attachment-retry-0");
    assert_eq!(rig.core.uploads.lock().unwrap().len(), 2);
    *rig.core.stopped.lock().unwrap() = true;
    keys(cx, "enter");
    assert_eq!(typed(&view, cx), "preserve me");
    assert_eq!(cx.update(|_, cx| compose.read(cx).attachments().len()), 1);
}

#[test]
fn folders_and_read_only_chats_never_upload_and_discard_clears_files() {
    let rig = rig_with(Vec::new());
    let (view, cx, _) = chat_window(&rig, Settings::default());
    let compose = compose(&view, cx);
    let dir = tempfile::tempdir().unwrap();
    cx.update(|window, cx| {
        compose.update(cx, |compose, cx| {
            compose.attach_files(&[dir.path().to_owned()], window, cx);
        });
    });
    draw(cx);
    assert!(rig.core.uploads.lock().unwrap().is_empty());
    let file = dir.path().join("discard.txt");
    std::fs::write(&file, b"test").unwrap();
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.attach_files(&[file], window, cx)));
    draw(cx);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.discard(window, cx)));
    draw(cx);
    assert!(cx.update(|_, cx| compose.read(cx).attachments().is_empty()));
    let read_only = rig_with(Vec::new());
    *read_only.core.read_only.lock().unwrap() = true;
    let (view, cx, _) = chat_window(&read_only, Settings::default());
    let editor = cx.update(|_, cx| view.read(cx).compose().clone());
    let file = dir.path().join("read-only.txt");
    std::fs::write(&file, b"test").unwrap();
    cx.update(|window, cx| editor.update(cx, |editor, cx| editor.attach_files(&[file], window, cx)));
    draw(cx);
    assert!(read_only.core.uploads.lock().unwrap().is_empty());
}

// --- the text typed as a message's text ---

/// A mention in typed text: where its name stands (bytes) and whom it names.
struct DraftMention {
    range: std::ops::Range<usize>,
    mri: String,
}

/// Text typed as the editor's document: each line a paragraph, `mentions` at their ranges.
fn typed_doc(text: &str, mentions: &[DraftMention]) -> Doc {
    let mut blocks = Vec::new();
    let mut start = 0;
    for line in text.split('\n') {
        let end = start + line.len();
        let mut runs = Vec::new();
        let mut at = start;
        for mention in mentions {
            if mention.range.start < at || mention.range.end > end {
                continue;
            }
            runs.push(Run::new(&text[at..mention.range.start], Attrs::default()));
            let attrs = Attrs {
                mention: Some(mention.mri.clone()),
                ..Attrs::default()
            };
            runs.push(Run::new(&text[mention.range.clone()], attrs));
            at = mention.range.end;
        }
        runs.push(Run::new(&text[at..end], Attrs::default()));
        blocks.push(Block::new(Kind::Paragraph, 0, runs));
        start = end + 1;
    }
    Doc::new(blocks)
}

/// The message typed text makes.
fn message_text(text: &str, mentions: &[DraftMention], quotes: &[Node]) -> Result<Option<MessageText>, ComposeError> {
    message_of(&typed_doc(text, mentions), quotes)
}

fn html(text: &str, mentions: &[DraftMention], quotes: &[Node]) -> String {
    message_text(text, mentions, quotes)
        .unwrap()
        .expect("a message")
        .html()
        .to_owned()
}

#[test]
fn each_line_is_a_paragraph_and_blank_lines_at_either_end_are_dropped() {
    assert_eq!(html("one\ntwo", &[], &[]), "<p>one</p><p>two</p>");
    assert_eq!(
        html("\n  \none\n\ntwo\n \n", &[], &[]),
        "<p>one</p><p>&nbsp;</p><p>two</p>",
        "an empty line inside stays, as the web client writes one"
    );
    assert_eq!(html("a < b & \"c\"", &[], &[]), "<p>a &lt; b &amp; &quot;c&quot;</p>");
    assert!(message_text("", &[], &[]).unwrap().is_none());
    assert!(
        message_text(" \n\t\n ", &[], &[]).unwrap().is_none(),
        "whitespace sends nothing"
    );
}

#[test]
fn plain_text_is_a_paragraph_per_line_and_reads_no_format() {
    let doc = plain_doc("one\r\ntwo\n\n**three** @b <i>\n");
    let lines: Vec<String> = doc.blocks.iter().map(Block::text).collect();
    assert_eq!(lines, ["one", "two", "", "**three** @b <i>"]);
    assert!(doc.blocks.iter().all(|block| block.kind == Kind::Paragraph));
    assert!(
        doc.blocks
            .iter()
            .flat_map(|block| &block.runs)
            .all(|run| run.attrs == Attrs::default()),
        "no format, no mention"
    );
    let html = message_of(&doc, &[]).unwrap().unwrap().html().to_owned();
    assert_eq!(html, "<p>one</p><p>two</p><p>&nbsp;</p><p>**three** @b &lt;i&gt;</p>");
    assert_eq!(plain_doc(""), Doc::default());
    assert!(message_of(&plain_doc("\n \n"), &[]).unwrap().is_none());
    assert_eq!(
        message_of(&plain_doc(&"long ".repeat(40_000)), &[]),
        Err(ComposeError::TooLong)
    );
}

#[test]
fn an_edit_keeps_a_reply_s_or_a_forward_s_quote_only() {
    let quote = |message_id: Option<&str>, forward: bool| {
        Node::Quote(teams_proto::chat::Quote {
            message_id: message_id.map(str::to_owned),
            forward,
            ..teams_proto::chat::Quote::default()
        })
    };
    let text = RichText {
        nodes: vec![
            quote(Some("1"), false),
            quote(None, false),
            quote(None, true),
            Node::Text {
                text: "hi".to_owned(),
                marks: teams_proto::chat::Marks::NONE,
            },
        ],
        truncated: false,
    };
    assert_eq!(quotes_of(&text), [quote(Some("1"), false), quote(None, true)]);
}

/// `lines.json` (synthetic-from-capture): the chat-shapes capture's message of three lines, the last holding a link.
fn lines_fixture() -> serde_json::Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/chatsvc/lines.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// What `lines.json`'s message was typed as.
const LINES_TYPED: &str =
    "a made-up first line\na second line\na link https://example.com/made-up/path on the third line";

#[test]
fn a_typed_url_stays_text_as_the_capture_s_link_inserted_with_its_text() {
    // the captured form with `links` "[]": the anchor form needs a `HyperLink` entry this box doesn't build
    let fixture = lines_fixture();
    assert_eq!(
        html(LINES_TYPED, &[], &[]),
        fixture["unlinked_content"].as_str().unwrap()
    );
    for typed in [
        "see https://example.test/a?b=1. ok",
        "(http://example.test) mailto:lab@example.test",
        "https:// and javascript:alert(1)",
    ] {
        let built = html(typed, &[], &[]);
        assert!(!built.contains("<a"), "no anchor: {built}");
        assert_eq!(built, format!("<p>{typed}</p>"));
    }
}

#[test]
fn a_mention_is_sent_by_the_mri_its_token_names_never_by_its_text() {
    let text = "hi Lab Bee and Lab Bee";
    let mentions = [DraftMention {
        range: 3..10,
        mri: LAB_B.to_owned(),
    }];
    let built = message_text(text, &mentions, &[]).unwrap().unwrap();
    assert!(built.html().starts_with("<p>hi <readonly"), "{}", built.html());
    assert!(
        built.html().ends_with(" and Lab Bee</p>"),
        "the name typed again is text: {}",
        built.html()
    );
    let participants: Vec<&str> = built.participants().iter().map(Mri::as_str).collect();
    assert_eq!(participants, [LAB_B]);

    // two mentions, numbered in order, on two lines
    let two = [
        DraftMention {
            range: 0..1,
            mri: LAB_B.to_owned(),
        },
        DraftMention {
            range: 2..3,
            mri: OUTSIDER.to_owned(),
        },
    ];
    let built = message_text("b\nx", &two, &[]).unwrap().unwrap();
    assert!(built.html().contains("itemid=\"0\">b</span>") && built.html().contains("itemid=\"1\">x</span>"));
    assert_eq!(
        built.participants().len(),
        2,
        "an outsider is a participant the fence judges"
    );
    // only work accounts can be mentioned
    let bot = [DraftMention {
        range: 0..1,
        mri: "28:00000000-0000-4000-8000-000000000001".to_owned(),
    }];
    assert!(matches!(message_text("x", &bot, &[]), Err(ComposeError::NotAPerson(_))));
}

/// A stored message of the a–b chat (made up): `id` is its arrival time.
fn message(id: i64, sender: &str, content: &str) -> Message {
    Message {
        conversation_id: AB.to_owned(),
        id: id.to_string(),
        version: 1,
        client_message_id: Some(format!("{}", 9_000_000_000_000_000_000_i64 - id)),
        sender: Some(sender.to_owned()),
        sender_name: (sender == LAB_B).then(|| "Lab Bee".to_owned()),
        message_type: "RichText/Html".to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: id,
        compose_time: Some(id),
        edit_time: None,
        delete_time: None,
    }
}

#[test]
fn a_reply_quotes_the_message_its_sender_its_time_and_a_one_line_preview() {
    let quoted = message(NOON, LAB_B, "<p>first line</p><p>second   line</p>");
    let quote = Quoted::of(&quoted, "Lab Bee").unwrap();
    assert_eq!(
        quote.time,
        NOON.to_string(),
        "the quoted id, as the captured reply named it"
    );
    assert_eq!(quote.preview, "first line second line");
    let sent = html("my answer", &[], &[quote.node()]);
    assert_eq!(
        sent,
        format!(
            "<blockquote itemscope itemtype=\"http://schema.skype.com/Reply\" itemid=\"{NOON}\"><strong itemprop=\"mri\" \
             itemid=\"{LAB_B}\">Lab Bee</strong><span itemprop=\"time\" itemid=\"{NOON}\"></span><p \
             itemprop=\"preview\">first line second line</p></blockquote><p>my answer</p>"
        )
    );
    let long = message(NOON, LAB_B, &"word ".repeat(200));
    let preview = Quoted::of(&long, "Lab Bee").unwrap().preview;
    assert_eq!(preview.chars().count(), PREVIEW_MAX + 1, "cut, with an ellipsis");
    assert!(preview.ends_with('…'));
    let mut nobody = message(NOON, LAB_B, "<p>x</p>");
    nobody.sender = None;
    assert!(Quoted::of(&nobody, "x").is_none());
    // a reply's quote is left out; a forward's content is the message's own, quoted
    let replied = message(
        NOON,
        LAB_B,
        "<blockquote itemtype=\"http://schema.skype.com/Reply\" itemid=\"1\"><p itemprop=\"preview\">old</p>\
         </blockquote><p>new</p>",
    );
    assert_eq!(Quoted::of(&replied, "Lab Bee").unwrap().preview, "new");
    let forwarded = message(
        NOON,
        LAB_B,
        "<blockquote itemtype=\"http://schema.skype.com/Forward\">\r\n<p>forwarded line</p>\r\n</blockquote>",
    );
    assert_eq!(Quoted::of(&forwarded, "Lab Bee").unwrap().preview, "forwarded line");
}

#[test]
fn an_edit_loads_the_text_back_its_mentions_by_mri_and_keeps_its_quote() {
    let text = RichText {
        nodes: vec![
            Node::Quote(teams_proto::chat::Quote::default()),
            Node::Paragraph(vec![
                Node::Text {
                    text: "hi ".to_owned(),
                    marks: teams_proto::chat::Marks::BOLD,
                },
                Node::Mention {
                    item_id: "0".to_owned(),
                    text: "Lab Bee".to_owned(),
                },
                Node::Mention {
                    item_id: "7".to_owned(),
                    text: "Nobody".to_owned(),
                },
            ]),
            Node::Paragraph(vec![
                Node::Link {
                    href: "https://example.test".to_owned(),
                    children: vec![Node::Text {
                        text: "a link".to_owned(),
                        marks: teams_proto::chat::Marks::NONE,
                    }],
                },
                Node::Break,
                Node::Text {
                    text: "end".to_owned(),
                    marks: teams_proto::chat::Marks::NONE,
                },
            ]),
        ],
        truncated: false,
    };
    let mentioned = HashMap::from([("0".to_owned(), LAB_B.to_owned())]);
    let loaded = load(&text, &mentioned).unwrap();
    assert_eq!(loaded.doc.plain_text(), "hi Lab BeeNobody\na link\nend");
    let mentions: Vec<(usize, &str)> = loaded
        .doc
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(index, block)| block.runs.iter().map(move |run| (index, run)))
        .filter_map(|(index, run)| Some((index, run.attrs.mention.as_deref()?)))
        .collect();
    assert_eq!(mentions, [(0, LAB_B)], "a mention naming nobody known stays text");
    let mentions = loaded.doc.blocks[0].mention_ranges();
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0], 3..10);
    assert!(
        loaded.doc.blocks[0].runs[0]
            .attrs
            .marks
            .contains(teams_proto::chat::Marks::BOLD),
        "its formats load with it"
    );
    assert_eq!(
        loaded.doc.blocks[1].runs[0].attrs.link.as_deref(),
        Some("https://example.test"),
        "and its links"
    );
    assert!(loaded.quotes.is_empty(), "a quote of nothing is the text's own");
    let reply = RichText {
        nodes: vec![Node::Quote(teams_proto::chat::Quote {
            message_id: Some("1".to_owned()),
            ..teams_proto::chat::Quote::default()
        })],
        truncated: false,
    };
    assert_eq!(
        load(&reply, &mentioned).unwrap().quotes.len(),
        1,
        "a reply's quote is kept aside"
    );
}

#[test]
fn an_edit_sends_back_what_it_loaded_and_refuses_what_it_can_t_keep() {
    let mentioned = HashMap::from([("0".to_owned(), LAB_B.to_owned())]);
    let image = RichText {
        nodes: vec![Node::Image(teams_proto::chat::Image {
            src: "https://example.test/i.png".to_owned(),
            alt: None,
            item_type: None,
            ..teams_proto::chat::Image::default()
        })],
        truncated: false,
    };
    assert_eq!(load(&image, &mentioned), Err(CantLoad::Image));
    let partial = RichText {
        nodes: Vec::new(),
        truncated: true,
    };
    assert_eq!(load(&partial, &mentioned), Err(CantLoad::Truncated));
    // a block of code loads as one, its lines as they were, and what loads goes back as it was: a heading past the
    // format bar's three, a numbered list from 1, a mention
    let kept = teams_proto::chat::parse_html(
        "<p>see</p><pre class=\"language-plaintext\"><code>{<br>&nbsp; \"a\": 1<br>}</code></pre>\
         <h5>Five</h5><ol><li>one</li><li>two</li></ol>\
         <p>hi <span itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Lab Bee</span></p>",
    );
    let loaded = load(&kept, &mentioned).unwrap();
    assert_eq!(loaded.doc.blocks[1].text(), "{\n  \"a\": 1\n}");
    let sent = message_of(&loaded.doc, &loaded.quotes).unwrap().expect("a message");
    assert_eq!(
        teams_proto::chat::parse_html(sent.html()).nodes,
        kept.nodes,
        "{}",
        sent.html()
    );
    // what would go back changed though the edit didn't touch it can't be edited here
    for changed in [
        "<p>see</p><pre class=\"language-json\"><code>{<br>&nbsp; \"a\": 1<br>}</code></pre>",
        "<ol start=\"3\"><li>three</li></ol>",
    ] {
        let text = teams_proto::chat::parse_html(changed);
        assert_eq!(load(&text, &mentioned), Err(CantLoad::Formatting), "{changed}");
    }
}

#[test]
fn an_at_that_starts_a_word_opens_the_picker_and_its_query_is_what_follows() {
    assert_eq!(mention_query("hi @Lab B", 9, &[]), Some((3, "Lab B")));
    assert_eq!(mention_query("@", 1, &[]), Some((0, "")));
    assert_eq!(mention_query("mail@lab", 8, &[]), None, "inside a word");
    assert_eq!(mention_query("@x\ny", 4, &[]), None, "on another line");
    assert_eq!(mention_query("@ x", 3, &[]), None, "a space right after the @");
    assert_eq!(
        mention_query("hi @Lab Bee x", 13, std::slice::from_ref(&(4..11))),
        None,
        "a mention placed since"
    );
    assert_eq!(
        mention_query(&format!("@{}", "y".repeat(50)), 51, &[]),
        None,
        "too far back"
    );
    assert_eq!(mention_query("hi @Lab", 3, &[]), None, "the cursor before it");
}

#[test]
fn the_picker_offers_the_names_holding_the_query() {
    let candidates = [
        Candidate {
            mri: LAB_B.to_owned(),
            name: "Lab Bee".into(),
        },
        Candidate {
            mri: "8:orgid:00000000-0000-4000-8000-00000000000c".to_owned(),
            name: "Lab Sea".into(),
        },
    ];
    assert_eq!(matching(&candidates, "").len(), 2);
    assert_eq!(matching(&candidates, "bE")[0].mri, LAB_B);
    assert_eq!(matching(&candidates, "lab s").len(), 1);
    assert!(matching(&candidates, "zz").is_empty());
}

#[test]
fn a_group_chat_s_picker_offers_everyone_after_its_members_and_a_one_to_one_chat_s_never() {
    let members = [Candidate {
        mri: LAB_B.to_owned(),
        name: "Lab Bee".into(),
    }];
    let names = |offered: Vec<Candidate>| offered.into_iter().map(|c| c.name.to_string()).collect::<Vec<_>>();
    assert_eq!(names(offered(GROUP, &members, "")), ["Lab Bee", "Everyone"]);
    let everyone = &offered(GROUP, &members, "EVE")[0];
    assert_eq!(offered(GROUP, &members, "EVE").len(), 1);
    assert!(everyone.is_everyone());
    assert_eq!(everyone.mri, GROUP, "its token names the chat");
    assert_eq!(names(offered(GROUP, &members, "bee")), ["Lab Bee"]);
    assert_eq!(
        names(offered(AB, &members, "")),
        ["Lab Bee"],
        "a 1:1 chat has no everyone"
    );
    assert_eq!(
        names(offered("19:meeting_lab0001@thread.v2", &members, "")),
        ["Lab Bee"],
        "nor a meeting chat"
    );
    let crowd: Vec<Candidate> = (0..EVERYONE_MAX_MEMBERS)
        .map(|n| Candidate {
            mri: format!("8:orgid:00000000-0000-4000-8000-{n:012x}"),
            name: format!("Member {n}").into(),
        })
        .collect();
    assert!(
        !offered(GROUP, &crowd, "").iter().any(Candidate::is_everyone),
        "a chat of over 1,000 members gets no everyone"
    );
}

#[test]
fn everyone_picked_is_a_mention_of_the_chat_adding_nobody() {
    let text = "@Everyone hi";
    let mentions = [DraftMention {
        range: 0..9,
        mri: GROUP.to_owned(),
    }];
    let built = message_text(text, &mentions, &[]).unwrap().expect("a message");
    assert!(built.participants().is_empty(), "it adds nobody to the target");
    assert!(built.everyone().contains(GROUP));
    assert!(built.mentions_everyone_only_in(GROUP));
    assert!(!built.mentions_everyone_only_in(AB));
}

#[test]
fn typing_goes_out_at_most_once_per_six_seconds_the_first_at_once() {
    let mut throttle = TypingThrottle::default();
    let start = Instant::now();
    assert!(throttle.take(start));
    assert!(!throttle.take(start + Duration::from_secs(5)));
    assert!(throttle.take(start + TYPING_EVERY));
    assert!(!throttle.take(start + TYPING_EVERY + Duration::from_secs(1)));
}

// --- the box in a chat view ---

/// The rig with the a–b chat holding `messages`, its members a and b, b named, the kit set up.
fn rig_with(messages: Vec<Message>) -> Rig {
    let rig = Rig::start();
    rig.core.messages.lock().unwrap().insert(AB.to_owned(), messages);
    rig.core
        .members
        .lock()
        .unwrap()
        .insert(AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]);
    rig.core
        .names
        .lock()
        .unwrap()
        .insert(LAB_B.to_owned(), "Lab Bee".to_owned());
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

/// The chat view of the a–b chat with `settings`, in an 800×600 window of its own, its trace written to the second
/// value; the window active, the compose box focused.
fn chat_window(rig: &Rig, settings: Settings) -> (Entity<ChatView>, &'static mut VisualTestContext, Written) {
    let written = Written::default();
    let parts = parts::parts(rig, settings, Arc::new(ViewTrace::new(written.clone())));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(800.0), px(600.0)));
    cx.update(|window, _| window.activate_window());
    draw(cx);
    focus_compose(&view, cx);
    (view, cx, written)
}

fn compose(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Entity<Compose> {
    cx.update(|_, cx| view.read(cx).compose().clone())
}

fn focus_compose(view: &Entity<ChatView>, cx: &mut VisualTestContext) {
    let compose = compose(view, cx);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    draw(cx);
}

fn focus_list(view: &Entity<ChatView>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| view.read(cx).focus_handle().clone().focus(window, cx));
    draw(cx);
}

fn typed(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> String {
    let compose = compose(view, cx);
    cx.update(|_, cx| compose.read(cx).value(cx).to_string())
}

fn mode(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Mode {
    let compose = compose(view, cx);
    cx.update(|_, cx| compose.read(cx).mode().clone())
}

fn actions(rig: &Rig) -> Vec<Action> {
    std::mem::take(&mut *rig.core.actions.lock().unwrap())
}

/// The HTML of a message post or an edit.
fn text_of(action: &Action) -> &MessageText {
    match action {
        Action::Send { text, .. } | Action::Edit { text, .. } => text,
        other => panic!("not a message: {other:?}"),
    }
}

fn type_text(cx: &mut VisualTestContext, text: &str) {
    cx.simulate_input(text);
    draw(cx);
}

fn keys(cx: &mut VisualTestContext, keystrokes: &str) {
    cx.simulate_keystrokes(keystrokes);
    draw(cx);
}

fn act(view: &Entity<ChatView>, cx: &mut VisualTestContext, action: MessageAction, message: &str) {
    cx.update(|window, cx| view.update(cx, |view, cx| view.act(action, message, window, cx)));
    draw(cx);
}

/// The trace's changes of `view`, the compose box's `changed` (each edit of what it holds) left out.
fn trace_changes(written: &Written, view: &str) -> Vec<String> {
    written
        .lines()
        .into_iter()
        .filter(|line| line["view"] == view && line["change"] != "changed")
        .map(|line| line["change"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn each_change_of_what_the_box_holds_is_traced_without_its_text() {
    let rig = rig_with(vec![message(NOON, LAB_B, "<p>hi a</p>")]);
    let (_view, cx, written) = chat_window(&rig, Settings::default());
    type_text(cx, "s");
    type_text(cx, "ecret");
    keys(cx, "backspace");
    let changed: Vec<serde_json::Value> = written
        .lines()
        .into_iter()
        .filter(|line| line["view"] == "compose" && line["change"] == "changed")
        .collect();
    // one per character typed, one for the backspace
    assert_eq!(changed.len(), 7, "{changed:?}");
    assert!(changed.iter().all(|line| line["conversation"] == AB));
    assert!(!written.text().contains("secret") && !written.text().contains("secre"));
    // the box's making is traced once, naming its chat
    let opened: Vec<serde_json::Value> = written
        .lines()
        .into_iter()
        .filter(|line| line["view"] == "compose" && line["change"] == "opened")
        .collect();
    assert_eq!(opened.len(), 1, "{opened:?}");
    assert_eq!(opened[0]["conversation"], AB);
}

#[test]
fn enter_sends_and_shift_enter_breaks_the_line() {
    let rig = rig_with(vec![message(NOON, LAB_B, "<p>hi a</p>")]);
    let (view, cx, written) = chat_window(&rig, Settings::default());

    keys(cx, "enter");
    assert!(actions(&rig).is_empty(), "an empty box sends nothing");
    type_text(cx, "one");
    keys(cx, "shift-enter");
    type_text(cx, "two https://example.test/a.");
    assert_eq!(typed(&view, cx), "one\ntwo https://example.test/a.");
    keys(cx, "enter");
    let sent = actions(&rig);
    assert_eq!(sent.len(), 1, "{sent:?}");
    let Action::Send { conversation, text } = &sent[0] else {
        panic!("a post: {sent:?}")
    };
    assert_eq!(conversation, AB);
    assert_eq!(text.html(), "<p>one</p><p>two https://example.test/a.</p>");
    assert_eq!(typed(&view, cx), "", "the box is emptied once the write is asked for");
    assert_eq!(trace_changes(&written, "compose"), ["opened", "sent"]);
    assert!(!written.text().contains("example.test"), "no text reaches the trace");
}

#[test]
fn a_mention_is_completed_after_at_from_the_chat_s_members_by_mri() {
    let rig = rig_with(vec![message(NOON, LAB_B, "<p>hi a</p>")]);
    let (view, cx, _) = chat_window(&rig, Settings::default());
    let compose = compose(&view, cx);
    let picker = |cx: &mut VisualTestContext| cx.update(|_, cx| compose.read(cx).picker().cloned());

    type_text(cx, "hi @");
    let open = picker(cx).expect("the picker opens");
    assert_eq!(
        open.matches
            .iter()
            .map(|candidate| candidate.mri.as_str())
            .collect::<Vec<_>>(),
        [LAB_B],
        "the chat's members, the account left out"
    );
    assert_eq!(open.matches[0].name.as_ref(), "Lab Bee");
    assert!(cx.debug_bounds("mention-picker").is_some());
    keys(cx, "escape");
    assert!(picker(cx).is_none(), "Escape closes it");
    assert_eq!(typed(&view, cx), "hi @");

    type_text(cx, "be");
    assert!(picker(cx).is_some(), "typing on opens it again");
    type_text(cx, "zz");
    assert!(picker(cx).is_none(), "nobody matches");
    keys(cx, "backspace backspace");
    keys(cx, "enter");
    assert!(actions(&rig).is_empty(), "Enter chose from the picker, it didn't send");
    assert_eq!(typed(&view, cx), "hi Lab Bee ");
    type_text(cx, "!");
    keys(cx, "enter");
    let sent = actions(&rig);
    let text = text_of(&sent[0]);
    let participants: Vec<&str> = text.participants().iter().map(Mri::as_str).collect();
    assert_eq!(participants, [LAB_B]);
    assert!(text.html().contains("itemid=\"0\">Lab Bee</span>"), "{}", text.html());

    // Tab chooses too; a name typed without the picker is only text
    type_text(cx, "Lab Bee @");
    keys(cx, "tab");
    keys(cx, "enter");
    let text = text_of(&actions(&rig)[0]).clone();
    assert_eq!(text.participants().len(), 1);
    assert!(text.html().starts_with("<p>Lab Bee <readonly"), "{}", text.html());
}

#[test]
fn a_reply_carries_its_quote_and_escape_cancels_it() {
    let theirs = message(NOON, LAB_B, "<p>question?</p>");
    let rig = rig_with(vec![theirs.clone()]);
    let (view, cx, written) = chat_window(&rig, Settings::default());

    act(&view, cx, MessageAction::Reply, &theirs.id);
    assert!(matches!(mode(&view, cx), Mode::Reply(_)));
    assert!(cx.debug_bounds("compose-mode").is_some());
    keys(cx, "escape");
    assert_eq!(mode(&view, cx), Mode::New, "Escape cancels the reply");

    act(&view, cx, MessageAction::Reply, &theirs.id);
    type_text(cx, "answer");
    keys(cx, "enter");
    let sent = actions(&rig);
    let html = text_of(&sent[0]).html().to_owned();
    assert!(
        html.starts_with(&format!(
            "<blockquote itemscope itemtype=\"http://schema.skype.com/Reply\" itemid=\"{NOON}\">"
        )),
        "{html}"
    );
    assert!(html.contains(">question?</p></blockquote><p>answer</p>"), "{html}");
    assert_eq!(mode(&view, cx), Mode::New);
    assert_eq!(
        trace_changes(&written, "compose"),
        ["opened", "replying", "cancelled", "replying", "sent"]
    );
}

/// Whether the message list has the focus, and whether the compose box has it.
fn focused(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> (bool, bool) {
    let compose = compose(view, cx);
    cx.update(|window, cx| {
        (
            view.read(cx).focus_handle().is_focused(window),
            compose.read(cx).focused(window, cx),
        )
    })
}

fn selected(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|_, cx| view.read(cx).selected().map(str::to_owned))
}

#[test]
fn escape_moves_the_focus_from_an_empty_box_to_the_list_and_back_so_r_replies_to_a_message() {
    let theirs = message(NOON, LAB_B, "<p>question?</p>");
    let rig = rig_with(vec![theirs.clone()]);
    let (view, cx, written) = chat_window(&rig, Settings::default());
    assert_eq!(focused(&view, cx), (false, true));

    // Escape with nothing to cancel in the box: the list takes the focus; Up selects the newest message, r replies
    keys(cx, "escape");
    assert_eq!(focused(&view, cx), (true, false));
    keys(cx, "up");
    assert_eq!(selected(&view, cx), Some(theirs.id.clone()));
    keys(cx, "r");
    assert!(matches!(mode(&view, cx), Mode::Reply(_)));
    assert_eq!(focused(&view, cx), (false, true), "the reply is typed in the box");
    // Escape cancels the reply first, and only then moves the focus
    keys(cx, "escape");
    assert_eq!(mode(&view, cx), Mode::New);
    assert_eq!(focused(&view, cx), (false, true));
    keys(cx, "escape");
    assert_eq!(focused(&view, cx), (true, false));
    assert_eq!(trace_changes(&written, "compose"), ["opened", "replying", "cancelled"]);
}

#[test]
fn a_click_on_a_message_leaves_the_focus_in_the_box() {
    let theirs = message(NOON, LAB_B, "<p>question?</p>");
    let rig = rig_with(vec![theirs.clone()]);
    let (view, cx, _) = chat_window(&rig, Settings::default());
    assert_eq!(focused(&view, cx), (false, true));
    let selector = format!("bubble-{}", theirs.id);
    let bubble = cx.debug_bounds(Box::leak(selector.into_boxed_str())).unwrap();
    cx.simulate_click(bubble.center(), Modifiers::none());
    assert_eq!(focused(&view, cx), (false, true), "the box keeps the focus");
    assert_eq!(selected(&view, cx), None);
    type_text(cx, "typed");
    assert_eq!(typed(&view, cx), "typed");
}

#[test]
fn the_keyboard_alone_reacts_and_escape_goes_back_to_the_box_keeping_its_draft() {
    let theirs = message(NOON, LAB_B, "<p>question?</p>");
    let rig = rig_with(vec![theirs.clone()]);
    let (view, cx, _) = chat_window(&rig, Settings::default());
    keys(cx, "escape");
    keys(cx, "down");
    keys(cx, "a");
    keys(cx, "1");
    assert_eq!(
        actions(&rig),
        [Action::React {
            conversation: AB.to_owned(),
            message_id: theirs.id.clone(),
            key: "like".to_owned(),
        }]
    );
    // Escape in the list: the selection first, then the focus back to the box
    keys(cx, "escape");
    assert_eq!(selected(&view, cx), None);
    assert_eq!(focused(&view, cx), (true, false));
    keys(cx, "escape");
    assert_eq!(focused(&view, cx), (false, true));
    type_text(cx, "typed");
    // a box holding text hands Escape on too: the list takes the focus and the draft stays
    keys(cx, "escape");
    assert_eq!(focused(&view, cx), (true, false));
    assert_eq!(typed(&view, cx), "typed");
}

#[test]
fn up_in_an_empty_box_edits_the_account_s_last_message() {
    let mut mine = message(NOON + MINUTE, LAB_A, "");
    "<p>hello <span itemscope itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Lab Bee</span></p>"
        .clone_into(&mut mine.content);
    mine.properties = Some(format!(
        "{{\"mentions\":[{{\"@type\":\"http://schema.skype.com/Mention\",\"itemid\":0,\"mri\":\"{LAB_B}\",\
         \"mentionType\":\"person\",\"displayName\":\"Lab Bee\"}}]}}"
    ));
    let rig = rig_with(vec![
        message(NOON - MINUTE, LAB_A, "<p>older</p>"),
        message(NOON, LAB_B, "<p>theirs</p>"),
        mine.clone(),
    ]);
    let (view, cx, _) = chat_window(&rig, Settings::default());

    type_text(cx, "x");
    keys(cx, "up");
    assert_eq!(mode(&view, cx), Mode::New, "only in an empty box");
    keys(cx, "backspace up");
    assert!(
        matches!(mode(&view, cx), Mode::Edit { ref message_id, .. } if *message_id == mine.id),
        "{:?}",
        mode(&view, cx)
    );
    assert_eq!(typed(&view, cx), "hello Lab Bee");
    keys(cx, "escape");
    assert_eq!(
        (mode(&view, cx), typed(&view, cx)),
        (Mode::New, String::new()),
        "Escape cancels it"
    );

    keys(cx, "up");
    type_text(cx, " again");
    keys(cx, "enter");
    let sent = actions(&rig);
    let Action::Edit {
        conversation,
        message_id,
        text,
    } = &sent[0]
    else {
        panic!("an edit: {sent:?}")
    };
    assert_eq!((conversation.as_str(), message_id.as_str()), (AB, mine.id.as_str()));
    let participants: Vec<&str> = text.participants().iter().map(Mri::as_str).collect();
    assert_eq!(participants, [LAB_B], "the mention kept by its MRI");
    assert!(text.html().ends_with(" again</p>"), "{}", text.html());
}

#[test]
fn a_delete_asks_first_and_escape_keeps_the_message() {
    let mine = message(NOON, LAB_A, "<p>oops</p>");
    let rig = rig_with(vec![mine.clone()]);
    let (view, cx, _) = chat_window(&rig, Settings::default());

    act(&view, cx, MessageAction::Delete, &mine.id);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).deleting().map(str::to_owned)),
        Some(mine.id.clone())
    );
    assert!(cx.debug_bounds("confirmation").is_some());
    keys(cx, "escape");
    assert!(actions(&rig).is_empty(), "cancelled, nothing written");
    assert!(cx.debug_bounds("confirmation").is_none());

    act(&view, cx, MessageAction::Delete, &mine.id);
    keys(cx, "enter");
    assert_eq!(
        actions(&rig),
        [Action::Delete {
            conversation: AB.to_owned(),
            message_id: mine.id.clone(),
        }]
    );
    // and by the button
    act(&view, cx, MessageAction::Delete, &mine.id);
    let confirm = cx.debug_bounds("confirm-delete").unwrap();
    cx.simulate_click(confirm.center(), Modifiers::none());
    draw(cx);
    assert_eq!(actions(&rig).len(), 1);
}

#[test]
fn the_quick_reactions_are_the_account_s_most_used_then_teams_own() {
    let frequent = |keys: &[&str]| keys.iter().map(|key| (*key).to_owned()).collect::<Vec<_>>();
    assert_eq!(quick_reactions(&[]), ["like", "heart", "laugh", "surprised"]);
    assert_eq!(
        quick_reactions(&frequent(&["1f440_eyes", "heart"])),
        ["1f440_eyes", "heart", "like", "laugh"],
        "the most used first, Teams' own after them, none twice"
    );
    assert_eq!(
        quick_reactions(&frequent(&["custom-key", "yes-tone2", "fire", "cool", "angry", "sad"])),
        ["yes-tone2", "fire", "cool", "angry"],
        "a key Teams doesn't offer left out, a skin tone kept, four at most"
    );

    // the view asks the engine, and its keys pick them in that order
    let theirs = message(NOON, LAB_B, "<p>nice</p>");
    let rig = rig_with(vec![theirs.clone()]);
    *rig.core.frequent.lock().unwrap() = frequent(&["1f440_eyes"]);
    let (view, cx, _) = chat_window(&rig, Settings::default());
    assert_eq!(
        cx.update(|_, cx| view.read(cx).quick().to_vec()),
        ["1f440_eyes", "like", "heart", "laugh"]
    );
    act(&view, cx, MessageAction::React, &theirs.id);
    assert!(cx.debug_bounds("pick-1f440_eyes").is_some());
    keys(cx, "1");
    assert_eq!(
        actions(&rig),
        [Action::React {
            conversation: AB.to_owned(),
            message_id: theirs.id.clone(),
            key: "1f440_eyes".to_owned(),
        }]
    );
}

#[test]
fn the_quick_reactions_set_the_account_s_own_and_a_chip_takes_it_back() {
    let theirs = message(NOON, LAB_B, "<p>nice</p>");
    let rig = rig_with(vec![theirs.clone()]);
    rig.core.reactions.lock().unwrap().insert(
        theirs.id.clone(),
        vec![Reaction {
            key: "like".to_owned(),
            user: LAB_A.to_owned(),
            time: NOON,
        }],
    );
    let (view, cx, _) = chat_window(&rig, Settings::default());

    act(&view, cx, MessageAction::React, &theirs.id);
    assert_eq!(
        DEFAULT_QUICK_REACTIONS[..QUICK_COUNT],
        ["like", "heart", "laugh", "surprised"],
        "M2 Task 10's capture"
    );
    for pick in ["pick-like", "pick-heart", "pick-laugh", "pick-surprised"] {
        assert!(cx.debug_bounds(pick).is_some(), "{pick} offered");
    }
    keys(cx, "2");
    assert_eq!(
        actions(&rig),
        [Action::React {
            conversation: AB.to_owned(),
            message_id: theirs.id.clone(),
            key: "heart".to_owned(),
        }]
    );
    assert_eq!(cx.update(|_, cx| view.read(cx).reacting().map(str::to_owned)), None);
    // the account's own like, chosen again, is taken back
    act(&view, cx, MessageAction::React, &theirs.id);
    keys(cx, "1");
    let chip = cx.debug_bounds("reaction-like").unwrap();
    cx.simulate_click(chip.center(), Modifiers::none());
    draw(cx);
    let unreact = Action::Unreact {
        conversation: AB.to_owned(),
        message_id: theirs.id.clone(),
        key: "like".to_owned(),
    };
    assert_eq!(actions(&rig), [unreact.clone(), unreact]);
}

fn typing_sent(rig: &Rig) -> usize {
    std::mem::take(&mut *rig.core.transients.lock().unwrap())
        .into_iter()
        .inspect(|transient| {
            assert_eq!(
                transient,
                &Transient::Typing {
                    conversation: AB.to_owned()
                }
            );
        })
        .count()
}

#[test]
fn typing_goes_out_while_the_box_has_text_and_focus_no_more_often_than_the_capture_shows() {
    let rig = rig_with(vec![
        message(NOON, LAB_B, "<p>hi</p>"),
        message(NOON + 1, LAB_A, "<p>mine</p>"),
    ]);
    // read receipts off: any write here would be typing's
    let settings = Settings {
        send_read_receipts: false,
        ..Settings::default()
    };
    let (view, cx, _) = chat_window(&rig, settings);

    type_text(cx, " ");
    assert_eq!(typing_sent(&rig), 0, "whitespace isn't text");
    type_text(cx, "h");
    type_text(cx, "e");
    assert_eq!(typing_sent(&rig), 1, "the first at once, the next held back");
    cx.executor()
        .advance_clock(TYPING_EVERY.saturating_sub(Duration::from_secs(1)));
    type_text(cx, "l");
    assert_eq!(typing_sent(&rig), 0);
    cx.executor().advance_clock(Duration::from_secs(1));
    type_text(cx, "l");
    assert_eq!(typing_sent(&rig), 1, "{TYPING_EVERY:?} after the last");

    // without the focus, nothing
    cx.executor().advance_clock(TYPING_EVERY);
    let compose = compose(&view, cx);
    focus_list(&view, cx);
    cx.update(|window, cx| {
        let _ = window;
        compose.update(cx, |compose, cx| {
            compose.editor().update(cx, |editor, cx| editor.insert("o", cx));
        });
    });
    draw(cx);
    let compose_focused = cx.update(|window, cx| compose.read(cx).focused(window, cx));
    assert!(!compose_focused);
    assert_eq!(typing_sent(&rig), 0, "not while another view has the focus");

    // never while editing
    focus_compose(&view, cx);
    keys(cx, "ctrl-a backspace up");
    assert!(matches!(mode(&view, cx), Mode::Edit { .. }));
    cx.executor().advance_clock(TYPING_EVERY);
    type_text(cx, "x");
    assert_eq!(typing_sent(&rig), 0, "not while editing");
    keys(cx, "escape");
    assert!(
        rig.core.asked().iter().all(|asked| *asked != "write"),
        "typing is never a write"
    );
}

#[test]
fn typing_stays_off_with_its_setting_off() {
    let rig = rig_with(vec![message(NOON, LAB_B, "<p>hi</p>")]);
    let (_view, cx, _) = chat_window(
        &rig,
        Settings {
            send_typing: false,
            ..Settings::default()
        },
    );
    type_text(cx, "hello");
    assert_eq!(typing_sent(&rig), 0);
}

#[test]
fn typing_waits_for_the_first_run_to_be_confirmed() {
    let rig = rig_with(vec![message(NOON, LAB_B, "<p>hi</p>")]);
    let parts = parts::parts(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let settings = parts.settings.clone();
    rig.cx.update(|cx| {
        settings.update(cx, |model, _| {
            *model = std::mem::replace(model, SettingsModel::in_memory(Settings::default())).requiring_first_run();
        });
    });
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(800.0), px(600.0)));
    cx.update(|window, _| window.activate_window());
    draw(cx);
    focus_compose(&view, cx);
    type_text(cx, "hello");
    assert_eq!(
        typing_sent(&rig),
        0,
        "nothing on its own before the first run is confirmed"
    );
    cx.update(|_, cx| settings.update(cx, SettingsModel::confirm_first_run));
    cx.executor().advance_clock(TYPING_EVERY);
    type_text(cx, "!");
    assert_eq!(typing_sent(&rig), 1);
}

fn receipts(rig: &Rig) -> Vec<String> {
    actions(rig)
        .into_iter()
        .filter_map(|action| match action {
            Action::Read {
                conversation,
                message_id,
            } if conversation == AB => Some(message_id),
            _ => None,
        })
        .collect()
}

fn deliver(rig: &Rig, message: &Message) -> Event {
    rig.core
        .messages
        .lock()
        .unwrap()
        .get_mut(AB)
        .unwrap()
        .push(message.clone());
    Event::Message {
        conversation: AB.to_owned(),
        id: message.id.clone(),
        change: MessageChange::New,
        applied: Applied::Inserted,
        message: Some(Box::new(message.clone())),
        source: Source::Delivery,
    }
}

#[test]
fn a_read_receipt_follows_the_newest_message_shown_in_the_active_window_once_per_burst() {
    let read = message(NOON, LAB_B, "<p>read already</p>");
    let unread = message(NOON + MINUTE, LAB_B, "<p>new</p>");
    let rig = rig_with(vec![read.clone(), unread.clone()]);
    rig.core.horizons.lock().unwrap().insert(
        AB.to_owned(),
        ReadHorizon {
            conversation_id: AB.to_owned(),
            user: LAB_A.to_owned(),
            last_read_arrival: read.arrival_time,
            updated: NOON,
            raw: None,
        },
    );
    let (view, cx, _) = chat_window(&rig, Settings::default());
    assert!(receipts(&rig).is_empty(), "not before its wait is over");
    debounced(cx);
    assert_eq!(
        receipts(&rig),
        std::slice::from_ref(&unread.id),
        "up to the newest message"
    );
    assert_eq!(cx.update(|_, cx| view.read(cx).receipts()), 1);
    debounced(cx);
    assert!(receipts(&rig).is_empty(), "once");

    // the account's own message moves nothing; two from b within the wait give one receipt, for the newer
    let mine = message(NOON + 2 * MINUTE, LAB_A, "<p>mine</p>");
    rig.send(deliver(&rig, &mine));
    debounced(cx);
    assert!(receipts(&rig).is_empty());
    rig.send(deliver(&rig, &message(NOON + 3 * MINUTE, LAB_B, "<p>one</p>")));
    let second = message(NOON + 4 * MINUTE, LAB_B, "<p>two</p>");
    rig.send(deliver(&rig, &second));
    debounced(cx);
    assert_eq!(receipts(&rig), std::slice::from_ref(&second.id));

    // in a window that isn't active, none; once it is, the receipt goes
    cx.deactivate_window();
    let third = message(NOON + 5 * MINUTE, LAB_B, "<p>three</p>");
    rig.send(deliver(&rig, &third));
    debounced(cx);
    debounced(cx);
    assert!(receipts(&rig).is_empty(), "not while the window isn't the user's");
    cx.update(|window, _| window.activate_window());
    draw(cx);
    debounced(cx);
    assert_eq!(receipts(&rig), std::slice::from_ref(&third.id));
}

/// Lets a read receipt's wait run out.
fn debounced(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(READ_DEBOUNCE);
    draw(cx);
}

#[test]
fn read_receipts_stay_off_with_their_setting_off_or_scrolled_up() {
    let rig = rig_with(vec![message(NOON, LAB_B, "<p>new</p>")]);
    let (_view, cx, _) = chat_window(
        &rig,
        Settings {
            send_read_receipts: false,
            ..Settings::default()
        },
    );
    debounced(cx);
    assert!(receipts(&rig).is_empty());

    let many: Vec<Message> = (0..120).map(|n| message(NOON + n, LAB_B, "<p>line</p>")).collect();
    let rig = rig_with(many);
    let (view, cx, _) = chat_window(&rig, Settings::default());
    debounced(cx);
    assert_eq!(receipts(&rig).len(), 1);
    // scrolled up from the newest: a new message isn't read
    cx.update(|_, cx| view.read(cx).list_state().pause_following_tail());
    cx.update(|_, cx| view.read(cx).list_state().scroll_to_reveal_item(0));
    draw(cx);
    rig.send(deliver(&rig, &message(NOON + 500, LAB_B, "<p>later</p>")));
    debounced(cx);
    assert!(receipts(&rig).is_empty());
    // back at the newest, it is
    cx.update(|_, cx| view.update(cx, ChatView::jump_to_newest));
    draw(cx);
    debounced(cx);
    assert_eq!(receipts(&rig), [(NOON + 500).to_string()]);
}

/// A row of the a–b chat whose payload is the command it carries.
fn row(id: i64, command: &Command, state: OutboxState) -> OutboxEntry {
    let mut row = outbox_row(id, command.kind(), state);
    row.conversation_id = Some(AB.to_owned());
    row.payload = serde_json::to_string(command).unwrap();
    row
}

fn post(text: &str) -> Command {
    Command::Send {
        conversation: AB.to_owned(),
        text: MessageText::plain(text).unwrap(),
    }
}

fn pending(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Vec<(i64, RowState, Again)> {
    cx.update(|_, cx| {
        view.read(cx)
            .pending_items()
            .map(|pending| (pending.note.row, pending.note.state, pending.note.again))
            .collect()
    })
}

/// Clicks what `selector` names, and lets the view draw.
fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    draw(cx);
}

/// The a–b chat's rows that aren't sent, one of each state (rows 1–6 posts, row 7 an edit of `theirs`), and the one
/// an earlier session left.
fn rows_of_every_state(theirs: &Message) -> (Vec<OutboxEntry>, OutboxEntry) {
    let mut waiting = row(2, &post("waiting"), OutboxState::Pending);
    waiting.not_before = Some(i64::MAX);
    let mut maybe = row(5, &post("maybe"), OutboxState::Failed);
    maybe.unreadable_seen_at = Some(NOON);
    let earlier = row(6, &post("left"), OutboxState::Sending);
    let edit = Command::Edit {
        conversation: AB.to_owned(),
        message_id: theirs.id.clone(),
        text: MessageText::plain("x").unwrap(),
        original: teams_core::Sent {
            client_message_id: "1".to_owned(),
            compose_time_ms: NOON,
            arrival_time_ms: NOON,
            attachment_properties: Some(serde_json::Map::new()),
        },
    };
    let rows = vec![
        row(1, &post("sending"), OutboxState::Pending),
        waiting,
        row(3, &post("denied"), OutboxState::Denied),
        row(4, &post("failed"), OutboxState::Failed),
        maybe,
        earlier.clone(),
        row(7, &edit, OutboxState::Failed),
    ];
    (rows, earlier)
}

#[test]
fn each_post_on_its_way_shows_its_state_and_a_failed_or_earlier_one_offers_send_again() {
    let theirs = message(NOON, LAB_B, "<p>hi</p>");
    let rig = rig_with(vec![theirs.clone()]);
    let (rows, earlier) = rows_of_every_state(&theirs);
    *rig.core.unsettled.lock().unwrap() = rows;
    *rig.core.earlier.lock().unwrap() = vec![earlier];
    let (view, cx, written) = chat_window(&rig, Settings::default());

    assert_eq!(
        pending(&view, cx),
        [
            (1, RowState::Sending, Again::No),
            (2, RowState::Waiting, Again::No),
            (3, RowState::Denied, Again::No),
            (4, RowState::Failed, Again::Yes),
            (5, RowState::MayHaveArrived, Again::AfterWarning),
            (6, RowState::EarlierSession, Again::Yes),
        ]
    );
    assert_eq!(
        notes_on(&view, cx, &theirs.id),
        [(7, RowState::Failed, Again::AfterWarning)],
        "an edit on its message"
    );
    assert!(cx.debug_bounds("pending-3").is_some() && cx.debug_bounds("send-again-3").is_none());
    let line = written
        .lines()
        .into_iter()
        .rfind(|line| line["view"] == "outbox")
        .unwrap();
    assert_eq!(
        line["kinds"],
        serde_json::json!([
            "failed",
            "sending",
            "waiting",
            "denied",
            "failed",
            "may-have-arrived",
            "earlier-session"
        ]),
        "the message's note, then the posts on their way: {line}"
    );
    assert_eq!(line["messages"], serde_json::json!(["7", "1", "2", "3", "4", "5", "6"]));
    assert_eq!(rig.shutdowns(), 0, "a view never stops the engine");

    // send again: at once on a failed row, after the warning on one that may have arrived
    click(cx, "send-again-4");
    assert_eq!(*rig.core.resent.lock().unwrap(), [4]);
    click(cx, "send-again-5");
    assert_eq!(cx.update(|_, cx| view.read(cx).resending()), Some(5));
    assert_eq!(*rig.core.resent.lock().unwrap(), [4], "warned first");
    click(cx, "resend-anyway");
    assert_eq!(*rig.core.resent.lock().unwrap(), [4, 5]);
    click(cx, "send-again-6");
    assert_eq!(*rig.core.resent.lock().unwrap(), [4, 5, 6]);
    assert!(
        actions(&rig).is_empty(),
        "send again is the explicit resend, never a new write"
    );
}

#[test]
fn a_post_shows_at_once_as_it_is_queued_then_sent_until_its_message_is_held() {
    let rig = rig_with(vec![message(NOON, LAB_B, "<p>hi</p>")]);
    let (view, cx, _) = chat_window(&rig, Settings::default());
    let command = post("on its way");
    let queued = row(9, &command, OutboxState::Pending);
    rig.send(Event::Queued(queued.clone()));
    draw(cx);
    assert_eq!(pending(&view, cx), [(9, RowState::Sending, Again::No)]);
    assert!(cx.debug_bounds("pending-9").is_some());

    let mut sent = queued.clone();
    sent.state = OutboxState::Sent;
    rig.send(Event::Outbox(sent.clone()));
    draw(cx);
    assert_eq!(
        pending(&view, cx),
        [(9, RowState::Sent, Again::No)],
        "sent, its message not held yet"
    );

    let mut mine = message(NOON + MINUTE, LAB_A, "<p>on its way</p>");
    mine.client_message_id = Some(sent.client_message_id.clone());
    rig.send(deliver(&rig, &mine));
    draw(cx);
    assert!(pending(&view, cx).is_empty(), "the message itself shows now");
    let shown = cx.update(|_, cx| view.read(cx).message_items().any(|item| item.id == mine.id));
    assert!(shown);
}

/// The notes on message `id`: each row, its state, whether "send again" is offered.
fn notes_on(view: &Entity<ChatView>, cx: &mut VisualTestContext, id: &str) -> Vec<(i64, RowState, Again)> {
    cx.update(|_, cx| {
        view.read(cx)
            .message_items()
            .find(|item| item.id == id)
            .map(|item| {
                item.outbox
                    .iter()
                    .map(|note| (note.row, note.state, note.again))
                    .collect()
            })
            .unwrap_or_default()
    })
}

fn no_receipts() -> Settings {
    Settings {
        send_read_receipts: false,
        ..Settings::default()
    }
}

#[test]
fn a_send_the_engine_refuses_gives_the_box_its_draft_back() {
    let theirs = message(NOON, LAB_B, "<p>question?</p>");
    let rig = rig_with(vec![theirs.clone()]);
    let (view, cx, _) = chat_window(&rig, no_receipts());
    act(&view, cx, MessageAction::Reply, &theirs.id);
    type_text(cx, "hi @");
    keys(cx, "enter");
    type_text(cx, "keep me");
    // the engine refuses before it makes a row (here: stopped)
    *rig.core.stopped.lock().unwrap() = true;
    keys(cx, "enter");
    assert_eq!(actions(&rig).len(), 1, "asked for once");
    assert_eq!(typed(&view, cx), "hi Lab Bee keep me", "the text is back");
    assert!(matches!(mode(&view, cx), Mode::Reply(_)), "and the reply");
    let compose = compose(&view, cx);
    let (tokens, problem) = cx.update(|_, cx| {
        let compose = compose.read(cx);
        let doc = compose.editor().read(cx).doc();
        let tokens = doc
            .blocks
            .iter()
            .flat_map(teams_ui::editor::doc::Block::mention_ranges)
            .count();
        (tokens, compose.problem().cloned())
    });
    assert_eq!(tokens, 1, "the mention, still a token");
    assert!(problem.is_some_and(|problem| problem.starts_with("Not sent")));
}

#[test]
fn a_chip_of_any_reaction_teams_offers_sets_or_takes_back_and_one_of_another_key_only_shows() {
    let theirs = message(NOON, LAB_B, "<p>nice</p>");
    let rig = rig_with(vec![theirs.clone()]);
    rig.core.reactions.lock().unwrap().insert(
        theirs.id.clone(),
        vec![
            Reaction {
                key: "1f440_eyes".to_owned(),
                user: LAB_B.to_owned(),
                time: NOON,
            },
            Reaction {
                key: "yes-tone2".to_owned(),
                user: LAB_A.to_owned(),
                time: NOON + 1,
            },
            Reaction {
                key: "custom-key".to_owned(),
                user: LAB_B.to_owned(),
                time: NOON + 2,
            },
        ],
    );
    let (view, cx, _) = chat_window(&rig, no_receipts());
    click(cx, "reaction-1f440_eyes");
    click(cx, "reaction-yes-tone2");
    click(cx, "reaction-custom-key");
    cx.update(|_, cx| view.update(cx, |view, cx| view.toggle_reaction(&theirs.id, "custom-key", cx)));
    let (conversation, message_id) = (AB.to_owned(), theirs.id.clone());
    assert_eq!(
        actions(&rig),
        [
            Action::React {
                conversation: conversation.clone(),
                message_id: message_id.clone(),
                key: "1f440_eyes".to_owned(),
            },
            Action::Unreact {
                conversation,
                message_id,
                key: "yes-tone2".to_owned(),
            },
        ],
        "a key Teams doesn't offer is neither set nor taken back"
    );
}

#[test]
fn a_click_into_the_compose_box_closes_more_reactions_and_keeps_the_focus_there() {
    let theirs = message(NOON, LAB_B, "<p>nice</p>");
    let rig = rig_with(vec![theirs.clone()]);
    let (view, cx, _) = chat_window(&rig, no_receipts());
    act(&view, cx, MessageAction::React, &theirs.id);
    keys(cx, "5");
    let picker = cx.debug_bounds("reaction-picker-more").expect("more reactions open");
    // a spot of the box's text area: its toolbar's buttons act on their own
    let text = cx.debug_bounds("rich-editor").unwrap();
    let inside = [
        text.center(),
        point(text.right() - px(4.0), text.center().y),
        point(text.left() + px(4.0), text.center().y),
    ]
    .into_iter()
    .find(|spot| !picker.contains(spot))
    .expect("a spot of the text area outside the picker");

    cx.simulate_click(inside, Modifiers::none());
    assert!(cx.update(|_, cx| view.read(cx).more_reactions().is_none()), "closed");
    assert_eq!(focused(&view, cx), (false, true), "the compose box keeps the focus");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).reacting().map(str::to_owned)),
        None,
        "the quick reactions closed with it"
    );
    type_text(cx, "1");
    assert!(actions(&rig).is_empty(), "1 typed in the compose box reacts to nothing");
}

#[test]
fn more_reactions_finds_and_picks_any_reaction_teams_offers() {
    let theirs = message(NOON, LAB_B, "<p>nice</p>");
    let rig = rig_with(vec![theirs.clone()]);
    let (view, cx, _) = chat_window(&rig, no_receipts());
    let more = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).more_reactions().cloned());

    // key 5 opens it, its search field focused: what is typed there is a search, never the list's keys
    act(&view, cx, MessageAction::React, &theirs.id);
    assert!(cx.debug_bounds("pick-more").is_some());
    keys(cx, "5");
    assert!(cx.debug_bounds("reaction-picker-more").is_some());
    type_text(cx, "1");
    assert!(actions(&rig).is_empty(), "1 typed in the search picks nothing");
    let picker = more(cx).expect("more reactions open");
    let shown = cx.update(|_, cx| picker.read(cx).shown());
    assert!(shown.iter().all(|id| id.contains('1')), "1 is searched for");
    keys(cx, "backspace");
    type_text(cx, "eyes");
    let shown = cx.update(|_, cx| picker.read(cx).shown());
    assert_eq!(shown.first(), Some(&"1f440_eyes"), "the reaction named so first");
    assert!(shown.contains(&"hearteyes"));
    keys(cx, "enter");
    assert_eq!(
        actions(&rig),
        [Action::React {
            conversation: AB.to_owned(),
            message_id: theirs.id.clone(),
            key: "1f440_eyes".to_owned(),
        }]
    );
    assert!(more(cx).is_none(), "a pick closes it");
    assert_eq!(cx.update(|_, cx| view.read(cx).reacting().map(str::to_owned)), None);

    act(&view, cx, MessageAction::React, &theirs.id);
    keys(cx, "5");
    keys(cx, "escape");
    assert!(more(cx).is_none());
    assert!(actions(&rig).is_empty());
}

#[test]
fn more_reactions_scrolls_through_recent_and_every_category_its_tabs_following() {
    let theirs = message(NOON, LAB_B, "<p>nice</p>");
    let rig = rig_with(vec![theirs.clone()]);
    let (view, cx, _) = chat_window(&rig, no_receipts());
    let more = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).more_reactions().cloned());

    // Recent first: the account's own recent reactions (one with a skin tone as its reaction), then Teams' defaults
    *rig.core.recent.lock().unwrap() = vec!["1f44b_wavinghand".to_owned(), "yes-tone2".to_owned()];
    act(&view, cx, MessageAction::React, &theirs.id);
    click(cx, "pick-more");
    let picker = more(cx).expect("more reactions open");
    let recent = cx.update(|_, cx| picker.read(cx).section(Section::Recent));
    assert_eq!(recent[..3], ["1f44b_wavinghand", "yes", "surprised"]);
    assert_eq!(recent.len(), RECENT);
    assert_eq!(cx.update(|_, cx| picker.read(cx).active()), Some(Section::Recent));
    // the whole catalog in one list: scrolled down, the tab follows; a tab clicked scrolls to its section
    let list = cx.debug_bounds("emoji-1f44b_wavinghand").unwrap();
    cx.simulate_mouse_move(list.center(), None, Modifiers::none());
    cx.simulate_event(ScrollWheelEvent {
        position: list.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-1_500.0))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    draw(cx);
    assert!(matches!(
        cx.update(|_, cx| picker.read(cx).active()),
        Some(Section::Category(_))
    ));
    click(cx, "reaction-category-1");
    assert_eq!(cx.update(|_, cx| picker.read(cx).active()), Some(Section::Category(1)));
    let first = cx.update(|_, cx| picker.read(cx).section(Section::Category(1))[0]);
    let selector: &'static str = format!("emoji-{first}").leak();
    click(cx, selector);
    assert_eq!(
        actions(&rig),
        [Action::React {
            conversation: AB.to_owned(),
            message_id: theirs.id.clone(),
            key: first.to_owned(),
        }]
    );
}

#[test]
fn the_reaction_under_the_pointer_plays_its_animation_once_read() {
    let theirs = message(NOON, LAB_B, "<p>nice</p>");
    let rig = rig_with(vec![theirs.clone()]);
    let mut strip = image::RgbaImage::new(2, 6);
    strip.put_pixel(0, 0, image::Rgba([1, 2, 3, 255]));
    let mut png = Vec::new();
    strip
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    rig.core
        .animations
        .lock()
        .unwrap()
        .insert("1f440_eyes".to_owned(), png.into());
    let (view, cx, _) = chat_window(&rig, no_receipts());
    act(&view, cx, MessageAction::React, &theirs.id);
    keys(cx, "5");
    type_text(cx, "eyes");
    let picker = cx.update(|_, cx| view.read(cx).more_reactions().cloned()).unwrap();
    assert!(
        !rig.core.asked().contains(&"reaction_animation"),
        "nothing read before a hover"
    );

    let cell = cx.debug_bounds("emoji-1f440_eyes").unwrap();
    cx.simulate_mouse_move(cell.center(), None, Modifiers::none());
    draw(cx);
    assert_eq!(cx.update(|_, cx| picker.read(cx).hovered()), Some("1f440_eyes"));
    assert!(rig.core.asked().contains(&"reaction_animation"));
    let core: Arc<dyn Core> = rig.core.clone();
    let played = cx.update(|_, cx| teams_ui::reactions::animation("1f440_eyes", &core, cx));
    assert_eq!(played.map(|frames| frames.frame_count()), Some(3), "its three frames");
    let asked = rig
        .core
        .asked()
        .iter()
        .filter(|name| **name == "reaction_animation")
        .count();
    assert_eq!(asked, 1, "read once");
}

#[test]
fn an_animation_whose_read_failed_on_the_way_is_asked_again() {
    let rig = rig_with(vec![]);
    rig.core
        .unread_animations
        .lock()
        .unwrap()
        .insert("1f440_eyes".to_owned());
    let (_view, cx, _) = chat_window(&rig, no_receipts());
    let core: Arc<dyn Core> = rig.core.clone();
    let asked = || {
        rig.core
            .asked()
            .iter()
            .filter(|name| **name == "reaction_animation")
            .count()
    };
    assert!(
        cx.update(|_, cx| teams_ui::reactions::animation("1f440_eyes", &core, cx))
            .is_none()
    );
    cx.run_until_parked();
    assert_eq!(asked(), 1);

    // the read fails again: asked again, not kept as none
    assert!(
        cx.update(|_, cx| teams_ui::reactions::animation("1f440_eyes", &core, cx))
            .is_none()
    );
    cx.run_until_parked();
    assert_eq!(asked(), 2, "a failed read isn't kept as none");
}

#[test]
fn typing_s_throttle_holds_for_a_chat_whose_view_is_built_again() {
    let rig = rig_with(vec![message(NOON, LAB_B, "<p>hi</p>")]);
    let (_view, cx, _) = chat_window(&rig, no_receipts());
    type_text(cx, "h");
    assert_eq!(typing_sent(&rig), 1);
    // the chat opened again (another chat opened between): a new view of it, within the throttle
    let (_again, again, _) = chat_window(&rig, no_receipts());
    type_text(again, "i");
    assert_eq!(typing_sent(&rig), 0, "the chat's throttle holds");
    again.executor().advance_clock(TYPING_EVERY);
    type_text(again, "!");
    assert_eq!(typing_sent(&rig), 1);
}

#[test]
fn a_queued_post_shows_while_the_engine_is_busy_with_its_send() {
    let rig = rig_with(vec![message(NOON, LAB_B, "<p>hi</p>")]);
    let (view, cx, _) = chat_window(&rig, no_receipts());
    // the engine's loop sends, so it answers no store read meanwhile
    *rig.core.hold_unsettled.lock().unwrap() = true;
    rig.send(Event::Queued(row(9, &post("on its way"), OutboxState::Pending)));
    draw(cx);
    assert_eq!(pending(&view, cx), [(9, RowState::Sending, Again::No)]);
    assert!(cx.debug_bounds("pending-9").is_some());
}

#[test]
fn the_mention_picker_closes_once_the_cursor_leaves_its_at() {
    let rig = rig_with(vec![message(NOON, LAB_B, "<p>hi a</p>")]);
    let (view, cx, _) = chat_window(&rig, no_receipts());
    let compose = compose(&view, cx);
    type_text(cx, "hi @");
    assert!(cx.update(|_, cx| compose.read(cx).picker().is_some()));
    keys(cx, "home");
    keys(cx, "enter");
    let sent = actions(&rig);
    assert_eq!(sent.len(), 1, "Enter sent, it chose nobody: {sent:?}");
    let text = text_of(&sent[0]);
    assert_eq!(text.html(), "<p>hi @</p>");
    assert!(text.participants().is_empty());
}

#[test]
fn a_chat_hidden_behind_the_settings_sends_no_read_receipt() {
    let read = message(NOON, LAB_B, "<p>read already</p>");
    let rig = rig_with(vec![read.clone()]);
    rig.core.horizons.lock().unwrap().insert(
        AB.to_owned(),
        ReadHorizon {
            conversation_id: AB.to_owned(),
            user: LAB_A.to_owned(),
            last_read_arrival: read.arrival_time,
            updated: NOON,
            raw: None,
        },
    );
    let settings = Settings {
        selected_chat: Some(AB.to_owned()),
        ..Settings::default()
    };
    let parts = parts::parts(&rig, settings, Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.simulate_resize(size(px(1000.0), px(700.0)));
    cx.update(|window, _| window.activate_window());
    draw(cx);
    debounced(cx);
    assert!(receipts(&rig).is_empty(), "nothing unread");

    // b's message comes in, then the settings hide the chat before the receipt's wait is over
    let first = message(NOON + MINUTE, LAB_B, "<p>one</p>");
    rig.send(deliver(&rig, &first));
    cx.update(|_, cx| shell.update(cx, Shell::show_settings));
    draw(cx);
    debounced(cx);
    // and another while it is hidden
    let second = message(NOON + 2 * MINUTE, LAB_B, "<p>two</p>");
    rig.send(deliver(&rig, &second));
    debounced(cx);
    debounced(cx);
    assert!(receipts(&rig).is_empty(), "no receipt for a chat not shown");

    // shown again, it goes, for the newest
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(AB.to_owned(), window, cx)));
    draw(cx);
    debounced(cx);
    assert_eq!(receipts(&rig), std::slice::from_ref(&second.id));
}

#[test]
fn a_sent_post_whose_message_is_older_than_the_pages_held_isn_t_shown_pending() {
    let mut mine = message(NOON, LAB_A, "<p>mine</p>");
    let mut sent = row(9, &post("mine"), OutboxState::Sent);
    sent.message_id = Some(mine.id.clone());
    mine.client_message_id = Some(sent.client_message_id.clone());
    let mut messages = vec![mine.clone()];
    let later = i64::try_from(4 * PAGE).unwrap();
    messages.extend((1..=later).map(|n| message(NOON + n * MINUTE, LAB_B, "<p>later</p>")));
    let rig = rig_with(messages);
    // told sent this session, its message since followed by more than the pages a view holds
    rig.send(Event::Outbox(sent));
    let (view, cx, _) = chat_window(&rig, no_receipts());
    let held = cx.update(|_, cx| view.read(cx).message_items().any(|item| item.id == mine.id));
    assert!(!held, "the view holds only the newest pages");
    assert!(pending(&view, cx).is_empty(), "its message is in the store");
    assert!(cx.debug_bounds("pending-9").is_none());
}

/// An edit of `message` to `text` (made up).
fn edit_of(message: &Message, text: &str) -> Command {
    Command::Edit {
        conversation: AB.to_owned(),
        message_id: message.id.clone(),
        text: MessageText::plain(text).unwrap(),
        original: teams_core::Sent {
            client_message_id: "1".to_owned(),
            compose_time_ms: NOON,
            arrival_time_ms: NOON,
            attachment_properties: Some(serde_json::Map::new()),
        },
    }
}

fn reaction_on(message: &Message, key: &str, on: bool) -> Command {
    Command::React {
        conversation: AB.to_owned(),
        message_id: message.id.clone(),
        key: key.to_owned(),
        on,
    }
}

#[test]
fn a_write_a_newer_one_on_the_same_thing_made_out_of_date_isn_t_offered_again() {
    let mine = message(NOON, LAB_A, "<p>mine</p>");
    let theirs = message(NOON + MINUTE, LAB_B, "<p>theirs</p>");
    let rig = rig_with(vec![mine.clone(), theirs.clone()]);
    // edit 1 failed, edit 2 sent; heart set failed, then taken back; like set failed, nothing newer on it
    *rig.core.unsettled.lock().unwrap() = vec![
        row(1, &edit_of(&mine, "one"), OutboxState::Failed),
        row(3, &reaction_on(&theirs, "heart", true), OutboxState::Failed),
        row(5, &reaction_on(&theirs, "like", true), OutboxState::Failed),
    ];
    rig.send(Event::Outbox(row(2, &edit_of(&mine, "two"), OutboxState::Sent)));
    rig.send(Event::Outbox(row(
        4,
        &reaction_on(&theirs, "heart", false),
        OutboxState::Sent,
    )));
    let (view, cx, _) = chat_window(&rig, no_receipts());

    assert!(notes_on(&view, cx, &mine.id).is_empty(), "edit 1 is out of date");
    assert_eq!(
        notes_on(&view, cx, &theirs.id),
        [(5, RowState::Failed, Again::AfterWarning)],
        "the heart set before its take-back is out of date; the like isn't"
    );
    assert!(cx.debug_bounds("send-again-1").is_none() && cx.debug_bounds("send-again-3").is_none());
    for stale in [1, 3] {
        cx.update(|_, cx| view.update(cx, |view, cx| view.send_again(stale, cx)));
        draw(cx);
    }
    assert!(rig.core.resent.lock().unwrap().is_empty(), "never sent again");

    // the one still current warns first (a later change may have gone out in a session the view doesn't know)
    click(cx, "send-again-5");
    assert_eq!(cx.update(|_, cx| view.read(cx).resending()), Some(5));
    assert!(rig.core.resent.lock().unwrap().is_empty());
    assert!(cx.debug_bounds("confirmation").is_some());
    click(cx, "resend-anyway");
    assert_eq!(*rig.core.resent.lock().unwrap(), [5]);
}

#[test]
fn moving_the_selection_closes_the_delete_s_confirmation_and_the_quick_reactions() {
    let older = message(NOON, LAB_A, "<p>older</p>");
    let newer = message(NOON + MINUTE, LAB_A, "<p>newer</p>");
    let rig = rig_with(vec![older.clone(), newer.clone()]);
    let (view, cx, _) = chat_window(&rig, no_receipts());
    let state = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            let view = view.read(cx);
            (
                view.selected().map(str::to_owned),
                view.deleting().map(str::to_owned),
                view.reacting().map(str::to_owned),
            )
        })
    };

    act(&view, cx, MessageAction::Delete, &newer.id);
    assert_eq!(
        state(cx),
        (Some(newer.id.clone()), Some(newer.id.clone()), None),
        "asked on the message selected"
    );
    keys(cx, "up");
    assert_eq!(
        state(cx),
        (Some(older.id.clone()), None, None),
        "the selection moved: the question closed"
    );
    assert!(cx.debug_bounds("confirmation").is_none());
    keys(cx, "enter");
    assert!(actions(&rig).is_empty(), "Enter deletes nothing");

    act(&view, cx, MessageAction::React, &newer.id);
    keys(cx, "up");
    keys(cx, "1");
    assert!(
        actions(&rig).is_empty(),
        "the quick reactions closed with the selection"
    );

    // asked on the message now selected, Enter deletes that one
    act(&view, cx, MessageAction::Delete, &older.id);
    keys(cx, "enter");
    assert_eq!(
        actions(&rig),
        [Action::Delete {
            conversation: AB.to_owned(),
            message_id: older.id.clone(),
        }]
    );
}

// --- the account's own notes: read-only ---

const NOTES: &str = "48:notes";

#[test]
fn the_notes_show_their_messages_synced_on_opening_and_take_no_write() {
    let rig = rig_with(Vec::new());
    let mut note = message(NOON, LAB_A, "<p>buy milk</p>");
    note.conversation_id = NOTES.to_owned();
    rig.core
        .messages
        .lock()
        .unwrap()
        .insert(NOTES.to_owned(), vec![note.clone()]);
    let parts = parts::parts(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, NOTES.to_owned(), window, cx));
    cx.simulate_resize(size(px(800.0), px(600.0)));
    cx.update(|window, _| window.activate_window());
    draw(cx);

    // read from the service as it opens, as no conversation list brings them
    assert_eq!(*rig.core.synced.lock().unwrap(), [NOTES]);
    let shown: Vec<String> = cx.update(|_, cx| view.read(cx).message_items().map(|item| item.id.clone()).collect());
    assert_eq!(shown, [note.id.clone()]);
    let offered = cx.update(|_, cx| {
        view.read(cx)
            .message_items()
            .map(teams_ui::chat_view::MessageItem::actions)
            .collect::<Vec<_>>()
    });
    assert_eq!(offered, [Vec::<MessageAction>::new()], "a note offers no action");

    // the box takes no text and sends nothing: no reply, no edit, no reaction, no typing
    let compose = compose(&view, cx);
    assert!(cx.update(|_, cx| compose.read(cx).is_read_only()));
    assert!(cx.debug_bounds("compose-read-only").is_some(), "{READ_ONLY_NOTES}");
    focus_compose(&view, cx);
    type_text(cx, "x");
    keys(cx, "enter");
    act(&view, cx, MessageAction::Reply, &note.id);
    act(&view, cx, MessageAction::Edit, &note.id);
    cx.update(|_, cx| view.update(cx, |view, cx| view.toggle_reaction(&note.id, "like", cx)));
    draw(cx);
    assert!(actions(&rig).is_empty());
    assert_eq!(mode(&view, cx), Mode::New);
    assert!(rig.core.transients.lock().unwrap().is_empty());
}

// --- the app's read-only mode ---

#[test]
fn in_the_read_only_mode_a_chat_offers_only_copying_and_sends_nothing() {
    let theirs = message(NOON, LAB_B, "<p>hello</p>");
    let mine = message(NOON + MINUTE, LAB_A, "<p>mine</p>");
    let rig = rig_with(vec![mine.clone(), theirs.clone()]);
    *rig.core.read_only.lock().unwrap() = true;
    let settings = Settings {
        send_typing: true,
        send_read_receipts: true,
        ..Settings::default()
    };
    let (view, cx, _) = chat_window(&rig, settings);

    let offered = cx.update(|_, cx| {
        view.read(cx)
            .message_items()
            .map(teams_ui::chat_view::MessageItem::actions)
            .collect::<Vec<_>>()
    });
    assert!(
        offered.iter().all(|actions| actions == &[MessageAction::Copy]),
        "copying only: {offered:?}"
    );
    // the box says so, takes no text and sends nothing: no post, reply, edit, reaction, typing or read receipt
    let compose = compose(&view, cx);
    assert!(cx.update(|_, cx| compose.read(cx).is_read_only()));
    assert!(cx.debug_bounds("compose-read-only").is_some(), "{READ_ONLY_APP}");
    focus_compose(&view, cx);
    type_text(cx, "x");
    keys(cx, "enter");
    act(&view, cx, MessageAction::Reply, &theirs.id);
    act(&view, cx, MessageAction::Edit, &mine.id);
    act(&view, cx, MessageAction::Delete, &mine.id);
    cx.update(|_, cx| view.update(cx, |view, cx| view.toggle_reaction(&theirs.id, "like", cx)));
    cx.executor().advance_clock(READ_DEBOUNCE * 2);
    draw(cx);
    assert!(actions(&rig).is_empty(), "no write, no read receipt");
    assert_eq!(mode(&view, cx), Mode::New);
    assert!(rig.core.transients.lock().unwrap().is_empty(), "no typing");
    assert!(cx.debug_bounds("compose-emoji").is_none(), "no emoji to put anywhere");
}

fn emoji_open(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> bool {
    let compose = compose(view, cx);
    cx.update(|_, cx| compose.read(cx).emoji_picker().is_some())
}

#[test]
fn the_emoji_button_puts_the_emoji_picked_at_the_cursor_and_sends_nothing_by_itself() {
    let rig = rig_with(vec![message(NOON, LAB_B, "<p>hi a</p>")]);
    let (view, cx, written) = chat_window(&rig, no_receipts());
    type_text(cx, "see you");
    keys(cx, "left left left");

    click(cx, "compose-emoji");
    assert!(emoji_open(&view, cx), "the button opens the picker");
    let picker = cx.debug_bounds("compose-emoji-picker").expect("the picker is drawn");
    let button = cx.debug_bounds("compose-emoji").unwrap();
    assert!(picker.bottom() <= button.top(), "it opens upwards, over the box");
    // its search field has the focus: what is typed there is a search, never the box's text
    type_text(cx, "eyes");
    assert_eq!(typed(&view, cx), "see you");
    keys(cx, "enter");
    assert!(!emoji_open(&view, cx), "a pick closes it");
    assert_eq!(typed(&view, cx), "see \u{1f440}you", "the emoji at the cursor");
    assert_eq!(focused(&view, cx), (false, true), "the box has the focus back");
    assert!(actions(&rig).is_empty(), "picking an emoji writes nothing");
    assert!(!written.text().contains('\u{1f440}'), "no text reaches the trace");

    keys(cx, "enter");
    let sent = actions(&rig);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(text_of(&sent[0]).html(), "<p>see \u{1f440}you</p>");
}

#[test]
fn the_emoji_picker_opens_from_the_keyboard_and_its_key_or_escape_closes_it() {
    // the account's own message, which Up in an empty box would edit
    let rig = rig_with(vec![message(NOON, LAB_A, "<p>mine</p>")]);
    let (view, cx, _) = chat_window(&rig, no_receipts());

    keys(cx, "ctrl-.");
    assert!(emoji_open(&view, cx), "Ctrl+. opens it");
    // Up in its search field, the box empty, edits nothing
    keys(cx, "up");
    assert_eq!(mode(&view, cx), Mode::New, "Up in the search edits no message");
    keys(cx, "ctrl-.");
    assert!(!emoji_open(&view, cx), "Ctrl+. closes it again");
    assert_eq!(focused(&view, cx), (false, true), "the box has the focus back");

    // Enter in its search field, the box holding text, sends nothing
    type_text(cx, "hi");
    keys(cx, "ctrl-;");
    assert!(emoji_open(&view, cx), "Ctrl+; opens it too");
    keys(cx, "enter");
    keys(cx, "escape");
    assert!(!emoji_open(&view, cx), "Escape closes it");
    assert_eq!(focused(&view, cx), (false, true), "the box has the focus back");
    assert_eq!(mode(&view, cx), Mode::New);
    assert_eq!(typed(&view, cx), "hi");
    assert!(actions(&rig).is_empty(), "nothing sent");
}

#[test]
fn the_emoji_button_closes_its_picker_and_an_escape_meant_for_the_box_isn_t_the_picker_s() {
    let rig = rig_with(vec![message(NOON, LAB_A, "<p>mine</p>")]);
    let (view, cx, _) = chat_window(&rig, no_receipts());

    click(cx, "compose-emoji");
    assert!(emoji_open(&view, cx));
    click(cx, "compose-emoji");
    assert!(!emoji_open(&view, cx), "its button closes it again");
    assert_eq!(focused(&view, cx), (false, true));
    assert!(actions(&rig).is_empty());

    // an Escape while the focus is elsewhere is not the picker's: it goes where the focus is
    click(cx, "compose-emoji");
    assert!(emoji_open(&view, cx));
    let mine = rig_message_id(&view, cx);
    act(&view, cx, MessageAction::Reply, &mine);
    assert!(matches!(mode(&view, cx), Mode::Reply(_)), "replying, the box focused");
    keys(cx, "escape");
    assert_eq!(mode(&view, cx), Mode::New, "the box's Escape cancels the reply");
}

/// The id of the chat's last message.
fn rig_message_id(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| view.read(cx).message_items().last().map(|item| item.id.clone()))
        .expect("a message")
}

#[test]
fn send_and_emoji_sit_inside_the_box_and_its_accent_line_follows_the_focus() {
    let rig = rig_with(vec![message(NOON, LAB_B, "<p>hi a</p>")]);
    let (view, cx, _) = chat_window(&rig, no_receipts());
    let inside = |outer: gpui::Bounds<gpui::Pixels>, inner: gpui::Bounds<gpui::Pixels>| {
        outer.left() <= inner.left()
            && inner.right() <= outer.right()
            && outer.top() <= inner.top()
            && inner.bottom() <= outer.bottom()
    };
    let the_box = cx.debug_bounds("compose-box").expect("the box is drawn");
    assert!(the_box.size.height >= px(teams_ui::compose::BOX_HEIGHT), "{the_box:?}");
    for selector in ["compose-emoji", "compose-send"] {
        let button = cx.debug_bounds(selector).unwrap();
        assert!(inside(the_box, button), "{selector} {button:?} in {the_box:?}");
    }
    let emoji = cx.debug_bounds("compose-emoji").unwrap();
    let send = cx.debug_bounds("compose-send").unwrap();
    assert!(emoji.right() < send.left(), "send last, on the right");

    assert!(
        cx.debug_bounds("compose-accent").is_some(),
        "the accent line while the box has the focus"
    );
    focus_list(&view, cx);
    assert!(cx.debug_bounds("compose-accent").is_none(), "none without it");
}

/// A made-up picture, `width` by `height`, as PNG bytes.
fn png(width: u32, height: u32) -> Vec<u8> {
    let picture = image::RgbaImage::from_pixel(width, height, image::Rgba([40, 120, 200, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

/// Puts `entries` on the test platform's clipboard.
fn clipboard(cx: &mut VisualTestContext, entries: Vec<gpui::ClipboardEntry>) {
    cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem { entries }));
}

fn image_entry(bytes: Vec<u8>) -> gpui::ClipboardEntry {
    gpui::ClipboardEntry::Image(gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes))
}

fn string_entry(text: &str) -> gpui::ClipboardEntry {
    gpui::ClipboardEntry::String(gpui::ClipboardString::new(text.to_owned()))
}

fn picture_uploads(rig: &Rig) -> Vec<(String, teams_proto::PastedFormat)> {
    rig.core
        .picture_uploads
        .lock()
        .unwrap()
        .iter()
        .map(|(conversation, picture)| (conversation.clone(), picture.format))
        .collect()
}

#[test]
fn a_pasted_picture_shows_in_the_box_is_sent_at_once_and_the_message_names_it() {
    let rig = rig_with(Vec::new());
    let (view, cx, _) = chat_window(&rig, Settings::default());
    type_text(cx, "Look");
    keys(cx, "shift-enter");
    clipboard(cx, vec![image_entry(png(64, 40))]);
    keys(cx, "ctrl-v");
    assert_eq!(picture_uploads(&rig), [(AB.to_owned(), teams_proto::PastedFormat::Png)]);
    assert!(cx.debug_bounds("editor-picture-1").is_some(), "its thumbnail");
    assert!(
        cx.debug_bounds("editor-picture-remove-1").is_some(),
        "its Remove button"
    );
    assert_eq!(
        typed(&view, cx),
        "Look\n\n",
        "a picture adds no text: its line, and the cursor on the next"
    );
    keys(cx, "enter");
    let actions = actions(&rig);
    let text = text_of(&actions[0]);
    assert_eq!(text.pictures(), ["0-made-up-d1-1"]);
    // shown at its pixels' size over the screen's scale (the test window's is 2), as a browser gives a pasted picture
    assert_eq!(
        text.html(),
        "<p>Look</p><p><img src=\"https://us-api.asm.skype.com/v1/objects/0-made-up-d1-1/views/imgo\" \
         itemtype=\"http://schema.skype.com/AMSImage\" width=\"32\" height=\"20\" alt=\"image\" \
         id=\"x_0-made-up-d1-1\" itemid=\"0-made-up-d1-1\"></p>"
    );
    assert!(cx.debug_bounds("editor-picture-1").is_none(), "the box is emptied");
}

#[test]
fn a_picture_removed_from_the_box_is_not_sent_and_undo_brings_it_back() {
    let rig = rig_with(Vec::new());
    let (_view, cx, _) = chat_window(&rig, Settings::default());
    type_text(cx, "Only text");
    clipboard(cx, vec![image_entry(png(30, 30))]);
    keys(cx, "ctrl-v");
    click(cx, "editor-picture-remove-1");
    assert!(cx.debug_bounds("editor-picture-1").is_none());
    keys(cx, "ctrl-z");
    assert!(cx.debug_bounds("editor-picture-1").is_some(), "undo");
    click(cx, "editor-picture-remove-1");
    keys(cx, "enter");
    let actions = actions(&rig);
    assert_eq!(text_of(&actions[0]).html(), "<p>Only text</p>");
    assert!(text_of(&actions[0]).pictures().is_empty());
}

#[test]
fn text_of_its_own_beside_a_picture_and_a_plain_paste_paste_text_and_send_nothing() {
    let rig = rig_with(Vec::new());
    let (view, cx, _) = chat_window(&rig, Settings::default());
    // a spreadsheet's cells offer their picture beside their text: the text is pasted
    clipboard(cx, vec![string_entry("Cell one\tCell two"), image_entry(png(20, 10))]);
    keys(cx, "ctrl-v");
    assert_eq!(typed(&view, cx), "Cell one\tCell two");
    // Ctrl+Shift+V pastes text alone
    clipboard(cx, vec![string_entry(" and more"), image_entry(png(20, 10))]);
    keys(cx, "ctrl-shift-v");
    assert_eq!(typed(&view, cx), "Cell one\tCell two and more");
    assert!(picture_uploads(&rig).is_empty());
    // a browser's "Copy image" with the picture's address as its text: the picture
    clipboard(
        cx,
        vec![
            string_entry("https://made-up.example/picture.png"),
            image_entry(png(20, 10)),
        ],
    );
    keys(cx, "ctrl-v");
    assert_eq!(picture_uploads(&rig).len(), 1);
    // text alone pastes as before
    clipboard(cx, vec![string_entry("!")]);
    keys(cx, "ctrl-v");
    assert_eq!(
        typed(&view, cx),
        "Cell one\tCell two and more\n\n!",
        "after the picture"
    );
}

#[test]
fn a_picture_that_failed_to_send_holds_the_message_until_retried() {
    let rig = rig_with(Vec::new());
    let (_view, cx, _) = chat_window(&rig, Settings::default());
    rig.core
        .picture_answers
        .lock()
        .unwrap()
        .push_back(Err(teams_core::EngineError::Attachment {
            why: "The picture service is busy. Try again.".to_owned(),
        }));
    clipboard(cx, vec![image_entry(png(16, 16))]);
    keys(cx, "ctrl-v");
    assert!(cx.debug_bounds("editor-picture-retry-1").is_some(), "Retry shown");
    keys(cx, "enter");
    assert!(actions(&rig).is_empty(), "nothing sent while it isn't");
    click(cx, "editor-picture-retry-1");
    assert_eq!(picture_uploads(&rig).len(), 2, "sent again");
    assert!(cx.debug_bounds("editor-picture-retry-1").is_none());
    keys(cx, "enter");
    let actions = actions(&rig);
    assert_eq!(text_of(&actions[0]).pictures(), ["0-made-up-d1-2"]);
}

#[test]
fn copied_files_paste_their_pictures_inline_and_attach_the_rest() {
    let rig = rig_with(Vec::new());
    let (_view, cx, _) = chat_window(&rig, Settings::default());
    let dir = tempfile::tempdir().unwrap();
    let picture = dir.path().join("screenshot.png");
    let notes = dir.path().join("notes.txt");
    std::fs::write(&picture, png(48, 48)).unwrap();
    std::fs::write(&notes, b"synthetic file").unwrap();
    clipboard(
        cx,
        vec![
            string_entry(&format!("{}\n{}", picture.display(), notes.display())),
            gpui::ClipboardEntry::ExternalPaths(gpui::ExternalPaths(vec![picture, notes.clone()].into())),
        ],
    );
    keys(cx, "ctrl-v");
    assert_eq!(picture_uploads(&rig).len(), 1, "the picture inline");
    assert_eq!(*rig.core.uploads.lock().unwrap(), [notes], "the other file attached");
    assert!(cx.debug_bounds("editor-picture-1").is_some());
}
