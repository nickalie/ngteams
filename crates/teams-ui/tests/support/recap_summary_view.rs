//! The recap's AI summary pane on GPUI's test context, made by a fake `claude` (synthetic transcripts and summaries
//! only): offered, made, played from, copied, previewed and sent to the meeting's chat, shown again without asking
//! the tool; and why it can't be made.

#[path = "fake_tool.rs"]
#[expect(dead_code, reason = "the shared fake CLI helper serves several test harnesses")]
mod fake_tool;

use std::path::{Path, PathBuf};
use std::time::Duration;

use teams_ui::local_ai::{Backend, Tools};

use super::*;

/// A made-up summary of the fixtures' transcript, as a JSON string the fake tool's answer carries.
fn fake_summary() -> String {
    let summary = serde_json::json!({
        "purpose": "Review the made-up video player.",
        "takeaways": ["The video plays and the transcript follows it."],
        "topics": [{"title": "Video player", "points": [{"text": "Bo checked playback.", "at": "0:15"}]}],
        "next_steps": [{"owner": "Ada Lab", "text": "Look at search next.", "at": "0:20"}],
        "questions": []
    });
    serde_json::to_string(&summary.to_string()).unwrap()
}

fn script(dir: &Path, body: &str) -> PathBuf {
    fake_tool::write(dir, "claude", body)
}

/// A fake Claude Code, beside its log of starts: answers every message with the made-up summary.
fn fake_claude(dir: &Path) -> PathBuf {
    let body = format!(
        "echo start >> \"$(dirname \"$0\")/starts.log\"\nwhile IFS= read -r line; do\n  echo \
         '{{\"type\":\"result\",\"is_error\":false,\"result\":{}}}'\ndone\n",
        fake_summary()
    );
    script(dir, &body)
}

fn installed(rig: &Rig, tools: Tools) {
    rig.cx.update(|cx| cx.set_global(tools));
}

/// Draws until `selector` is drawn (the tool answers on a thread of its own), failing after a while.
fn wait_for(cx: &mut VisualTestContext, selector: &'static str) {
    for _ in 0..1000 {
        draw(cx);
        if cx.debug_bounds(selector).is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("{selector} never drawn");
}

fn drawn(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.debug_bounds(selector).is_some()
}

/// The pane's state selector for `words`.
fn state(words: &str) -> &'static str {
    Box::leak(format!("recap-summary-state {words}").into_boxed_str())
}

/// The messages the view sent: their chat and HTML.
fn sent(rig: &Rig) -> Vec<(String, String)> {
    locked(&rig.core.actions)
        .iter()
        .filter_map(|action| match action {
            teams_core::Action::Send { conversation, text } => Some((conversation.clone(), text.html().to_owned())),
            _ => None,
        })
        .collect()
}

#[test]
fn the_ai_summary_is_made_on_this_computer_played_from_copied_and_sent_after_a_preview() {
    let rig = rig(false);
    let dir = tempfile::tempdir().unwrap();
    installed(&rig, Tools::with(vec![(Backend::Claude, fake_claude(dir.path()))]));
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    click(cx, "chat-tab-recap");
    // the transcript shows first, the AI summary offered beside it with where the transcript would go
    assert!(drawn(cx, "recap-pane-transcript shown"));
    click(cx, "recap-pane-summary");
    assert!(drawn(cx, "recap-summary-offer"));
    assert!(drawn(cx, "recap-summary-privacy"));
    click(cx, "recap-summary-generate");
    wait_for(cx, "recap-summary-takeaways");
    for selector in [
        "recap-summary-made",
        "recap-summary-notice",
        "recap-summary-purpose",
        "recap-summary-topics",
        "recap-summary-next-steps",
    ] {
        assert!(drawn(cx, selector), "{selector} drawn");
    }
    assert!(!drawn(cx, "recap-summary-questions"), "none raised");
    no_time_beside_the_first_point(cx);
    // a point clicked: the recording plays from its moment
    click(cx, "recap-summary-at-0");
    assert!(drawn(cx, "video-player-recap-0"));
    copied_and_sent(&rig, cx);
    // another chat, then back: the summary shows at once, the tool not asked again
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_chat(fixtures::TRANSCRIPT_ONLY.to_owned(), window, cx);
        });
    });
    draw(cx);
    reopen(&shell, cx);
    click(cx, "chat-tab-recap");
    assert!(drawn(cx, "recap-pane-summary shown"));
    assert!(drawn(cx, "recap-summary-takeaways"));
    let starts = std::fs::read_to_string(dir.path().join("starts.log")).unwrap();
    assert_eq!(starts.lines().count(), 1);
    // read-only: nothing offers to send
    *locked(&rig.core.read_only) = true;
    cx.update(|window, _| window.refresh());
    draw(cx);
    assert!(!drawn(cx, "recap-summary-send"));
    assert!(drawn(cx, "recap-summary-copy"));
}

/// No time shows at the first point with a moment: its row is the whole width, only its bullet beside its words.
fn no_time_beside_the_first_point(cx: &mut VisualTestContext) {
    let row = cx.debug_bounds("recap-summary-at-0").unwrap();
    let words = cx.debug_bounds("recap-summary-at-0-words").unwrap();
    assert!(row.size.width > gpui::px(200.0), "{row:?}");
    assert_eq!(words.right(), row.right(), "nothing after the words: {row:?} {words:?}");
    assert!(
        words.left() - row.left() <= gpui::px(24.0),
        "only the bullet before the words: {row:?} {words:?}"
    );
}

/// The summary shown: copied as Markdown, then sent to the meeting's chat only once its preview's Send is clicked.
fn copied_and_sent(rig: &Rig, cx: &mut VisualTestContext) {
    click(cx, "recap-summary-copy");
    let copied = cx
        .update(|_, cx| cx.read_from_clipboard())
        .and_then(|item| item.text())
        .unwrap();
    assert!(copied.contains("## Key takeaways\n\n- The video plays and the transcript follows it."));
    assert!(copied.contains("- Bo checked playback.\n"), "{copied}");
    assert!(copied.contains("- **Ada Lab**: Look at search next.\n"), "{copied}");
    // Send to chat: previewed, sent only on Send
    click(cx, "recap-summary-send");
    assert!(drawn(cx, "recap-summary-preview"));
    assert!(sent(rig).is_empty(), "nothing sent before Send");
    click(cx, "recap-summary-send-cancel");
    assert!(!drawn(cx, "recap-summary-preview"));
    click(cx, "recap-summary-send");
    click(cx, "recap-summary-send-confirm");
    let sent = sent(rig);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, fixtures::MEETING);
    assert!(
        sent[0].1.starts_with("<h2>AI summary: Weekly sync</h2>"),
        "{}",
        sent[0].1
    );
    assert!(sent[0].1.contains("<strong>Ada Lab</strong>: Look at search next."));
    for shown in [copied.as_str(), sent[0].1.as_str()] {
        assert!(!shown.contains("0:15") && !shown.contains("0:20"), "a time in {shown}");
    }
    assert!(!drawn(cx, "recap-summary-preview"));
    assert!(drawn(cx, "recap-note"));
}

#[test]
fn a_recording_without_a_transcript_has_nothing_to_summarize() {
    let rig = rig(false);
    let recorded_only: Vec<_> = fixtures::recorded()
        .into_iter()
        .filter(|content| content.kind == teams_proto::recap::ContentKind::Recording)
        .collect();
    locked(&rig.core.meeting_contents).insert(fixtures::MEETING.to_owned(), recorded_only);
    installed(&rig, Tools::default());
    let (_shell, cx) = shell_window(&rig, fixtures::MEETING);
    click(cx, "chat-tab-recap");
    assert!(drawn(cx, "recap-pane-summary shown"));
    assert!(drawn(cx, state(teams_ui::recap_view::NO_TRANSCRIPT)));
    // its Transcript pane stays, saying there is none, as Teams' Recap does
    click(cx, "recap-pane-transcript");
    assert!(drawn(cx, "recap-state No transcript available"));
}

#[test]
fn the_ai_summary_says_how_to_get_a_tool_and_why_one_failed() {
    let rig = rig(false);
    locked(&rig.core.transcripts).insert(fixtures::transcript_location("ITEM9"), fixtures::VTT.to_owned());
    installed(&rig, Tools::default());
    let (_shell, cx) = shell_window(&rig, fixtures::TRANSCRIPT_ONLY);
    click(cx, "chat-tab-recap");
    click(cx, "recap-pane-summary");
    // no tool installed: how to get one
    assert!(drawn(cx, "recap-summary-find-tools"));
    assert!(!drawn(cx, "recap-summary-generate"));
    // a tool signed out: said, with Try again and Dismiss
    let dir = tempfile::tempdir().unwrap();
    let program = script(
        dir.path(),
        "read -r line\necho '{\"type\":\"result\",\"is_error\":true,\"result\":\"Not logged in · Please run /login\"}'\n",
    );
    installed(&rig, Tools::with(vec![(Backend::Claude, program)]));
    cx.update(|window, _| window.refresh());
    draw(cx);
    click(cx, "recap-summary-generate");
    wait_for(cx, "recap-summary-failure");
    assert!(drawn(cx, state(teams_ui::recap_view::SUMMARY_FAILED)));
    assert!(drawn(cx, "recap-summary-retry"));
    click(cx, "recap-summary-dismiss");
    assert!(drawn(cx, "recap-summary-offer"));
}

#[test]
fn turning_ai_off_stops_a_summary_being_made_also_after_its_recap_closed() {
    let rig = rig(false);
    let dir = tempfile::tempdir().unwrap();
    // a tool that never answers
    let program = script(dir.path(), "read -r line\nsleep 60\n");
    installed(&rig, Tools::with(vec![(Backend::Claude, program)]));
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    click(cx, "chat-tab-recap");
    click(cx, "recap-pane-summary");
    click(cx, "recap-summary-generate");
    assert!(drawn(cx, "recap-summary-running"));
    // another chat closes the recap; then AI goes off and on again
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_chat(fixtures::TRANSCRIPT_ONLY.to_owned(), window, cx);
        });
    });
    draw(cx);
    assert!(cx.update(|_, cx| shell.read(cx).recap_view().is_none()));
    cx.update(|_, cx| teams_ui::local_ai::AiSwitch::publish(false, cx));
    draw(cx);
    cx.update(|_, cx| teams_ui::local_ai::AiSwitch::publish(true, cx));
    reopen(&shell, cx);
    click(cx, "chat-tab-recap");
    // the summary stopped: the transcript shows, and the summary is offered again
    assert!(drawn(cx, "recap-pane-transcript shown"));
    click(cx, "recap-pane-summary");
    assert!(!drawn(cx, "recap-summary-running"));
    assert!(drawn(cx, "recap-summary-generate"));
}
