//! Files dragged from the file manager onto a chat, as the platform hands them to the window (`FileDropEvent`): Teams'
//! "Share in this chat" over the conversation while they are over it, gone when they leave; several files dropped on
//! the message box staged beside the draft; a folder told on the drop while the files with it go in; a channel's box
//! and a box that edits a message take no files and say why, the draft kept. Every file and name is made up.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{Entity, ExternalPaths, FileDropEvent, Pixels, Point, VisualTestContext, size};
use teams_ui::ViewTrace;
use teams_ui::chat_view::{ChatView, Place};
use teams_ui::compose::{Compose, Loaded};
use teams_ui::editor::doc::{Attrs, Block, Doc, Kind, Run};
use teams_ui::settings::Settings;

use super::{chat_window, draw, parts, rig_with};

fn made_up_files(dir: &Path, names: &[&str]) -> Vec<PathBuf> {
    names
        .iter()
        .map(|name| {
            let path = dir.join(name);
            std::fs::write(&path, b"A synthetic attachment, not a real document.\n").unwrap();
            path
        })
        .collect()
}

fn entered(cx: &mut VisualTestContext, at: Point<Pixels>, paths: &[PathBuf]) {
    cx.simulate_event(FileDropEvent::Entered {
        position: at,
        paths: ExternalPaths(paths.iter().cloned().collect()),
    });
    draw(cx);
}

fn dropped(cx: &mut VisualTestContext, at: Point<Pixels>) {
    cx.simulate_event(FileDropEvent::Submit { position: at });
    draw(cx);
}

fn compose(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Entity<Compose> {
    cx.update(|_, cx| view.read(cx).compose().clone())
}

fn staged(compose: &Entity<Compose>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.update(|_, cx| {
        compose
            .read(cx)
            .attachments()
            .iter()
            .map(|file| file.name.clone())
            .collect()
    })
}

fn problem(compose: &Entity<Compose>, cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|_, cx| compose.read(cx).problem().map(ToString::to_string))
}

fn draft(compose: &Entity<Compose>, cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| compose.read(cx).editor().read(cx).doc().plain_text())
}

fn type_draft(compose: &Entity<Compose>, cx: &mut VisualTestContext, text: &str) {
    cx.update(|_, cx| {
        let editor = compose.read(cx).editor().clone();
        editor.update(cx, |editor, cx| {
            editor.set_doc(
                Doc::new(vec![Block::new(
                    Kind::Paragraph,
                    0,
                    vec![Run::new(text, Attrs::default())],
                )]),
                cx,
            );
        });
    });
    draw(cx);
}

#[test]
fn a_file_over_the_chat_shows_share_in_this_chat_until_it_leaves() {
    let rig = rig_with(Vec::new());
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    let dir = tempfile::tempdir().unwrap();
    let files = made_up_files(dir.path(), &["made-up notes.txt"]);
    let chat = cx.debug_bounds("chat-view").unwrap();

    entered(cx, chat.center(), &files);

    assert!(cx.update(|_, cx| view.read(cx).file_drop_active()));
    let highlight = cx
        .debug_bounds("chat-file-drop-highlight")
        .expect("Teams' drop target over the chat");
    assert_eq!(highlight, chat, "it covers the whole conversation, the message box too");

    cx.simulate_event(FileDropEvent::Exited);
    draw(cx);

    assert!(!cx.update(|_, cx| view.read(cx).file_drop_active()));
    assert!(cx.debug_bounds("chat-file-drop-highlight").is_none());
    assert!(staged(&compose(&view, cx), cx).is_empty(), "leaving attaches nothing");
}

#[test]
fn several_files_dropped_on_the_box_go_in_beside_the_draft() {
    let rig = rig_with(Vec::new());
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    let compose = compose(&view, cx);
    type_draft(&compose, cx, "Here are the notes");
    let dir = tempfile::tempdir().unwrap();
    let files = made_up_files(
        dir.path(),
        &["made-up notes.txt", "made-up-report.pdf", "made-up plan.docx"],
    );
    let the_box = cx.debug_bounds("compose-box").unwrap();

    entered(cx, the_box.center(), &files);
    assert!(cx.debug_bounds("chat-file-drop-highlight").is_some());
    dropped(cx, the_box.center());

    assert_eq!(
        staged(&compose, cx),
        ["made-up notes.txt", "made-up-report.pdf", "made-up plan.docx"]
    );
    assert_eq!(draft(&compose, cx), "Here are the notes");
    assert_eq!(problem(&compose, cx), None);
    assert!(cx.debug_bounds("chat-file-drop-highlight").is_none());
}

#[test]
fn a_folder_is_told_on_the_drop_while_the_files_with_it_go_in() {
    let rig = rig_with(Vec::new());
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    let compose = compose(&view, cx);
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("Made-up folder");
    std::fs::create_dir(&folder).unwrap();
    let mut paths = vec![folder];
    paths.extend(made_up_files(dir.path(), &["made-up notes.txt"]));
    let the_box = cx.debug_bounds("compose-box").unwrap();

    entered(cx, the_box.center(), &paths);
    assert!(
        cx.debug_bounds("chat-file-drop-highlight").is_some(),
        "a folder is only known on the drop, as in Teams"
    );
    dropped(cx, the_box.center());

    assert_eq!(staged(&compose, cx), ["made-up notes.txt"]);
    assert_eq!(
        problem(&compose, cx).as_deref(),
        Some("Folders can't be attached. Choose the files inside the folder.")
    );
}

#[test]
fn a_channel_s_box_takes_no_files_and_says_why_on_the_drop() {
    const CHANNEL: &str = "19:made-up-channel@thread.tacv2";
    let rig = rig_with(Vec::new());
    let parts = parts::parts(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let app: &'static mut gpui::TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| {
        let compose = ChatView::channel_box(&parts, CHANNEL, &Place::Channel, "Post in channel".into(), window, cx);
        ChatView::in_place(parts, CHANNEL.to_owned(), Place::Channel, compose, window, cx)
    });
    cx.simulate_resize(size(gpui::px(800.0), gpui::px(600.0)));
    draw(cx);
    let compose = compose(&view, cx);
    type_draft(&compose, cx, "A made-up post");
    let dir = tempfile::tempdir().unwrap();
    let files = made_up_files(dir.path(), &["made-up notes.txt"]);
    let chat = cx.debug_bounds("chat-view").unwrap();

    entered(cx, chat.center(), &files);
    assert!(cx.debug_bounds("chat-file-drop-highlight").is_none());
    dropped(cx, chat.center());

    assert!(staged(&compose, cx).is_empty());
    assert_eq!(
        problem(&compose, cx).as_deref(),
        Some("Files can't be shared in a channel from this app.")
    );
    assert_eq!(draft(&compose, cx), "A made-up post");
}

#[test]
fn a_box_that_edits_a_message_takes_no_files_and_says_why_on_the_drop() {
    let rig = rig_with(Vec::new());
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    let compose = compose(&view, cx);
    let loaded = Loaded {
        doc: Doc::new(vec![Block::paragraph("A made-up message")]),
        quotes: Vec::new(),
    };
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.edit("1".to_owned(), loaded, window, cx)));
    draw(cx);
    let dir = tempfile::tempdir().unwrap();
    let files = made_up_files(dir.path(), &["made-up notes.txt"]);
    let the_box = cx.debug_bounds("compose-box").unwrap();

    entered(cx, the_box.center(), &files);
    assert!(cx.debug_bounds("chat-file-drop-highlight").is_none());
    dropped(cx, the_box.center());

    assert!(staged(&compose, cx).is_empty());
    assert_eq!(
        problem(&compose, cx).as_deref(),
        Some("Files can't be added to a message you edit. Send them in a new message.")
    );
    assert_eq!(draft(&compose, cx), "A made-up message");
}
