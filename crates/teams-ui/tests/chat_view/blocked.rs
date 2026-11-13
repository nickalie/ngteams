//! Messages a tenant's policy blocked, on GPUI's test context over the fake engine, in the synthetic shapes of
//! `tests/fixtures/chatsvc/policy-blocked.json`: the account's own keeps its text and file (put back by the engine)
//! under Teams' notice and its link, with no "Edited"; someone else's shows the notice alone, in its bubble.

use gpui::{Modifiers, VisualTestContext, px};
use serde_json::{Value, json};
use teams_store::Message;

use super::common::{LAB_A, LAB_B};
use super::{AB, NOON, chat_window, draw, items, message, rig_with, trace};

const FIXTURE: &str = include_str!("../../../../tests/fixtures/chatsvc/policy-blocked.json");
const MINE: &str = "401";
const THEIRS: &str = "402";

fn verdict(kind: &str) -> Value {
    json!({"verdictDetails": 0, "dlpAction": 2, "state": 1, "violationType": kind, "genericStreamItemId": 900})
}

/// a's own blocked message as the engine stores it once its copy's text and file are back, its edit time set as
/// a store from before kept it; b's as a reader stores it.
fn blocked() -> Vec<Message> {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let copy = &fixture["copy"];
    let mut mine = message(MINE, LAB_A, copy["content"].as_str().unwrap(), NOON - 60_000);
    mine.properties =
        Some(json!({"files": copy["properties"]["files"], "policyViolation": verdict("wft")}).to_string());
    mine.edit_time = Some(NOON - 59_000);
    let mut theirs = message(THEIRS, LAB_B, "", NOON);
    theirs.properties = Some(json!({"mentions": "[]", "policyViolation": verdict("dlp")}).to_string());
    vec![mine, theirs]
}

fn drawn(cx: &mut VisualTestContext, selector: &str) -> bool {
    draw(cx);
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .is_some()
}

fn assert_notice_inset(cx: &mut VisualTestContext, id: &str) {
    draw(cx);
    let bubble = cx
        .debug_bounds(Box::leak(format!("bubble-{id}").into_boxed_str()))
        .unwrap();
    let notice = cx
        .debug_bounds(Box::leak(format!("policy-notice-{id}").into_boxed_str()))
        .unwrap();
    assert!(
        notice.left() - bubble.left() >= px(15.0),
        "the bubble surface insets the notice: {bubble:?} {notice:?}"
    );
}

#[test]
fn the_own_blocked_message_keeps_its_text_and_file_under_the_notice_and_is_not_edited() {
    let rig = rig_with(blocked());
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    assert!(drawn(cx, &format!("policy-notice-{MINE}")));
    assert!(drawn(cx, &format!("policy-learn-{MINE}")));
    assert!(drawn(cx, &format!("file-cards-{MINE}")));
    assert!(!drawn(cx, "edited"), "the verdict's update is no edit");
    // someone else's: the notice alone, in a bubble, its link Teams' "What's this?"
    assert!(drawn(cx, &format!("policy-notice-{THEIRS}")));
    assert_notice_inset(cx, THEIRS);
    let link = cx
        .debug_bounds(Box::leak(format!("policy-learn-{THEIRS}").into_boxed_str()))
        .unwrap();
    cx.simulate_click(link.center(), Modifiers::none());
    draw(cx);
    assert_eq!(cx.opened_url().as_deref(), Some(teams_ui::policy_notice::POLICY_PAGE));
}

#[test]
fn an_own_blocked_message_with_only_a_file_keeps_the_notice_inside_a_bubble() {
    let mut messages = blocked();
    messages.truncate(1);
    messages[0].content.clear();
    let rig = rig_with(messages);
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    assert!(drawn(cx, &format!("file-cards-{MINE}")));
    assert_notice_inset(cx, MINE);
}

#[test]
fn a_late_copy_refreshes_the_rendered_text_at_the_same_message_version() {
    let mut messages = blocked();
    messages.truncate(1);
    messages[0].content.clear();
    let rig = rig_with(messages);
    let (written_trace, _) = trace();
    let (view, cx) = chat_window(&rig, written_trace);
    let before = items(&view, cx).remove(0);
    let teams_ui::chat_view::Body::Text(before) = before.body else {
        panic!("a text message")
    };
    assert!(before.0.blocks.is_empty());
    rig.core.messages.lock().unwrap().get_mut(AB).unwrap()[0].content = "<p>Restored text</p>".to_owned();
    rig.send(teams_core::Event::ConversationChanged {
        conversation: AB.to_owned(),
    });
    draw(cx);
    let after = items(&view, cx).remove(0);
    let teams_ui::chat_view::Body::Text(after) = after.body else {
        panic!("a text message")
    };
    assert!(matches!(&after.0.blocks[0], teams_ui::rich_text::Block::Text(text) if text.text == "Restored text"));
}
