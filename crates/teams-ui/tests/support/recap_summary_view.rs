//! The recap's AI summary pane on GPUI's test context, made by a fake `claude` (synthetic transcripts and summaries
//! only): offered, made, played from, copied, previewed and sent to the meeting's chat, shown again without asking
//! the tool; and why it can't be made.

#[path = "fake_tool.rs"]
#[expect(dead_code, reason = "the shared fake CLI helper serves several test harnesses")]
mod fake_tool;

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;
use teams_ui::local_ai::{Backend, Tools};

/// A point longer than the pane is wide.
const LONG_POINT: &str = "Ada said that the player must keep the place of the transcript when the window changes its size, \
                          and Bo said that the search must find the words in each part of the recording, also in the \
                          parts that the speaker said very fast or with a lot of noise behind the voice.";

/// An owner list nearly as wide as the pane.
const LONG_OWNER: &str = "Ada Lab, Bo Lab, Cy Lab, Dee Lab";

/// An owner list wider than the pane.
const WIDER_OWNER: &str = "Ada Lab, Bo Lab, Cy Lab, Dee Lab, Eve Lab, Fay Lab, Gus Lab, Hal Lab, Ivy Lab, Jo Lab, Kai Lab, \
                           Lee Lab, Max Lab, Ned Lab, Oli Lab, Pat Lab, Quin Lab, Rex Lab, Sam Lab, Tia Lab";

/// A window so narrow that the summary's words are narrower than an owned text's basis.
const NARROW_WINDOW: f32 = 520.0;

/// A made-up summary of the fixtures' transcript, as a JSON string the fake tool's answer carries.
fn fake_summary() -> String {
    let summary = serde_json::json!({
        "purpose": "Review the made-up video player.",
        "takeaways": ["The video plays and the transcript follows it."],
        "topics": [{"title": "Video player", "points": [
            {"text": "Bo checked playback.", "at": "0:15"},
            {"text": LONG_POINT, "at": "0:18"}
        ]}],
        "next_steps": [
            {"owner": "Ada Lab", "text": LONG_POINT, "at": "0:20"},
            {"owner": LONG_OWNER, "text": "Check the search.", "at": "0:21"},
            {"owner": WIDER_OWNER, "text": "Check the noise.", "at": "0:22"}
        ],
        "questions": []
    });
    serde_json::to_string(&summary.to_string()).unwrap()
}

pub(super) fn script(dir: &Path, body: &str) -> PathBuf {
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

pub(super) fn installed(rig: &Rig, tools: Tools) {
    rig.cx.update(|cx| cx.set_global(tools));
}

/// Draws until `selector` is drawn (the tool answers on a thread of its own), failing after a while.
pub(super) fn wait_for(cx: &mut VisualTestContext, selector: &'static str) {
    for _ in 0..1000 {
        draw(cx);
        if cx.debug_bounds(selector).is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("{selector} never drawn");
}

pub(super) fn drawn(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.debug_bounds(selector).is_some()
}

/// The pane's state selector for `words`.
fn state(words: &str) -> &'static str {
    Box::leak(format!("recap-summary-state {words}").into_boxed_str())
}

/// The messages the view sent: their chat and HTML.
pub(super) fn sent(rig: &Rig) -> Vec<(String, String)> {
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

/// No time shows at the first point with a moment: its row is the whole width, only its bullet beside its words, and
/// its text starts its words and is as wide as its own words drawn.
fn no_time_beside_the_first_point(cx: &mut VisualTestContext) {
    let row = cx.debug_bounds("recap-summary-at-0").unwrap();
    let words = cx.debug_bounds("recap-summary-at-0-words").unwrap();
    let text = cx.debug_bounds("recap-summary-at-0-text").unwrap();
    let drawn = cx.update(|window, _| {
        let words = "Bo checked playback.";
        let run = gpui::TextRun {
            len: words.len(),
            font: gpui::font(teams_ui::theme::SYSTEM_FONT),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window
            .text_system()
            .shape_line(words.into(), gpui::px(14.0), &[run], None)
            .width
    });
    assert!(
        (text.size.width - drawn).abs() < gpui::px(1.0),
        "only its words: {text:?} {drawn:?}"
    );
    assert_eq!(text.left(), words.left(), "nothing before the text: {words:?} {text:?}");
    assert!(row.size.width > gpui::px(200.0), "{row:?}");
    assert_eq!(words.right(), row.right(), "nothing after the words: {row:?} {words:?}");
    assert!(
        words.left() - row.left() <= gpui::px(24.0),
        "only the bullet before the words: {row:?} {words:?}"
    );
    a_long_point_wraps(cx);
}

/// A point longer than the pane wraps in its row: its text stays in its words, on more than one line.
fn a_long_point_wraps(cx: &mut VisualTestContext) {
    let row = cx.debug_bounds("recap-summary-at-1").unwrap();
    let words = cx.debug_bounds("recap-summary-at-1-words").unwrap();
    let text = cx.debug_bounds("recap-summary-at-1-text").unwrap();
    assert!(
        text.right() <= words.right(),
        "the text in its words: {words:?} {text:?}"
    );
    assert!(words.right() <= row.right(), "the words in its row: {row:?} {words:?}");
    assert!(text.size.height > gpui::px(30.0), "more than one line: {text:?}");
    a_long_next_step_wraps_beside_its_owner(cx);
}

/// A long next step stays on its owner's line: its text starts beside the owner and wraps in the rest of its words.
fn a_long_next_step_wraps_beside_its_owner(cx: &mut VisualTestContext) {
    let words = cx.debug_bounds("recap-summary-at-2-words").unwrap();
    let owner = cx.debug_bounds("recap-summary-at-2-owner").unwrap();
    let text = cx.debug_bounds("recap-summary-at-2-text").unwrap();
    assert_eq!(
        text.top(),
        owner.top(),
        "the text on the owner's line: {owner:?} {text:?}"
    );
    assert!(
        text.left() > owner.right(),
        "the text after the owner: {owner:?} {text:?}"
    );
    assert!(
        text.right() <= words.right(),
        "the text in its words: {words:?} {text:?}"
    );
    assert!(text.size.height > gpui::px(30.0), "more than one line: {text:?}");
    a_wide_owner_puts_its_text_below(cx);
}

/// An owner that leaves only a narrow strip beside it puts its text on the next line, in the full width.
fn a_wide_owner_puts_its_text_below(cx: &mut VisualTestContext) {
    let words = cx.debug_bounds("recap-summary-at-3-words").unwrap();
    let owner = cx.debug_bounds("recap-summary-at-3-owner").unwrap();
    let text = cx.debug_bounds("recap-summary-at-3-text").unwrap();
    assert!(
        owner.right() <= words.right() && words.right() - owner.right() < gpui::px(160.0),
        "the owner leaves a narrow strip: {words:?} {owner:?}"
    );
    assert!(
        text.top() >= owner.bottom(),
        "the text below the owner: {owner:?} {text:?}"
    );
    assert_eq!(
        text.left(),
        words.left(),
        "the text in the full width: {words:?} {text:?}"
    );
    an_owner_wider_than_the_pane_wraps(cx);
}

/// An owner wider than its words wraps in them, on more than one line.
fn an_owner_wider_than_the_pane_wraps(cx: &mut VisualTestContext) {
    let words = cx.debug_bounds("recap-summary-at-4-words").unwrap();
    let owner = cx.debug_bounds("recap-summary-at-4-owner").unwrap();
    assert!(
        owner.right() <= words.right(),
        "the owner in its words: {words:?} {owner:?}"
    );
    assert!(owner.size.height > gpui::px(30.0), "more than one line: {owner:?}");
    a_narrow_pane_shrinks_the_text(cx);
}

/// In a pane so narrow that its words are narrower than the text's basis, an owned text shrinks to stay in its words.
fn a_narrow_pane_shrinks_the_text(cx: &mut VisualTestContext) {
    cx.simulate_resize(gpui::size(gpui::px(NARROW_WINDOW), gpui::px(1000.0)));
    draw(cx);
    let words = cx.debug_bounds("recap-summary-at-2-words").unwrap();
    let text = cx.debug_bounds("recap-summary-at-2-text").unwrap();
    assert!(
        words.size.width < gpui::px(160.0),
        "words narrower than the basis: {words:?}"
    );
    assert!(
        text.right() <= words.right(),
        "the text in its words: {words:?} {text:?}"
    );
    cx.simulate_resize(gpui::size(gpui::px(1600.0), gpui::px(1000.0)));
    draw(cx);
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
    assert!(copied.contains(&format!("- **Ada Lab**: {LONG_POINT}\n")), "{copied}");
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
    assert!(sent[0].1.contains(&format!("<strong>Ada Lab</strong>: {LONG_POINT}")));
    assert_no_time(&copied);
    assert_no_time(&sent[0].1);
    assert!(!drawn(cx, "recap-summary-preview"));
    assert!(drawn(cx, "recap-note"));
}

fn assert_no_time(shown: &str) {
    for at in ["0:15", "0:18", "0:20", "0:21", "0:22"] {
        assert!(!shown.contains(at), "{at} in {shown}");
    }
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

/// The keyboard reaches each point with a moment in the drawn order after the pane's pills and buttons: Tab moves to
/// the next one, Shift+Tab back, and Enter or Space plays its moment as a click does (the transcript shows at the line
/// spoken then). Each point's moment is in a line of its own, so each press shows which point it played.
#[test]
fn tab_and_shift_tab_move_through_the_points_and_enter_or_space_plays_one() {
    let first = (2..=10)
        .find(|&tabs| pressed(tabs, "enter") == Some(LINES[0]))
        .expect("Tab reaches the first point, and Enter plays it from 0:15");
    assert_eq!(
        pressed(first + 1, "space"),
        Some(LINES[1]),
        "the next Tab reaches the next point, 0:20"
    );
    assert_eq!(
        pressed(first + 2, "enter"),
        Some(LINES[2]),
        "the next Tab reaches the question, 0:04"
    );
    assert_eq!(
        pressed(first + 2, "shift-tab enter"),
        Some(LINES[1]),
        "Shift+Tab from the last point goes back to the one before it"
    );
    assert_eq!(
        pressed(first + 1, "shift-tab space"),
        Some(LINES[0]),
        "Shift+Tab goes back to the first point"
    );
}

/// The transcript lines the stored summary's points play, in the points' order: 0:15, 0:20, then the question's 0:04.
const LINES: [&str; 3] = [
    "recap-paragraph-1-0 spoken",
    "recap-paragraph-2-0 spoken",
    "recap-paragraph-0-0 spoken",
];

/// In a new window on the stored summary, its question moved to 0:04: its pill clicked (the focus on it), `tabs` Tab
/// presses, then `then`; the transcript line played, if the transcript shows.
fn pressed(tabs: usize, then: &str) -> Option<&'static str> {
    let (_shell, cx) = keyed_summary(None);
    tabbed(cx, tabs, then)
}

/// `tabs` Tab presses, then `then`; the transcript line played, if the transcript shows.
fn tabbed(cx: &mut VisualTestContext, tabs: usize, then: &str) -> Option<&'static str> {
    for _ in 0..tabs {
        press(cx, "tab");
    }
    press(cx, then);
    if !drawn(cx, "recap-pane-transcript shown") {
        return None;
    }
    LINES.into_iter().find(|line| drawn(cx, line))
}

/// The AI summary and Transcript pills are a tab list: only the shown pane's pill is a tab stop, so Tab goes from it
/// into the pane, and the arrows show the other pane and move the focus to its pill.
#[test]
fn the_pills_take_one_tab_stop_and_the_arrows_switch_the_pane() {
    let (_shell, cx) = keyed_summary(None);
    press(cx, "right");
    assert!(
        drawn(cx, "recap-pane-transcript shown"),
        "Right on the AI summary pill shows the transcript"
    );
    press(cx, "left");
    assert!(
        drawn(cx, "recap-pane-summary shown"),
        "Left goes back to the AI summary"
    );
    press(cx, "end");
    assert!(drawn(cx, "recap-pane-transcript shown"), "End goes to the last pill");
    press(cx, "home tab enter");
    assert!(
        !drawn(cx, "recap-pane-transcript shown"),
        "Tab from the AI summary pill goes into the pane, past the Transcript pill"
    );
}

/// Enter on a point of a recap without a recording shows the transcript, and the focus goes to the Transcript pill (the
/// point is drawn no more): Left from there goes back to the summary.
#[test]
fn a_point_played_without_a_recording_leaves_the_focus_on_the_transcript_pill() {
    let first = (2..=10)
        .find(|&tabs| pressed(tabs, "enter") == Some(LINES[0]))
        .expect("Tab reaches the first point");
    let (_shell, cx) = keyed_summary(None);
    assert_eq!(tabbed(cx, first, "enter"), Some(LINES[0]));
    press(cx, "left");
    assert!(
        drawn(cx, "recap-pane-summary shown"),
        "the Transcript pill has the focus, so Left shows the summary"
    );
}

/// In a window too short for the whole summary, a point that Tab reaches below the pane's fold scrolls into view, so
/// its focus fill shows.
#[test]
fn a_point_tab_reaches_below_the_fold_scrolls_into_view() {
    let first = (2..=10)
        .find(|&tabs| pressed(tabs, "enter") == Some(LINES[0]))
        .expect("Tab reaches the first point");
    let (_shell, cx) = keyed_summary(Some(SHORT_WINDOW));
    let pane = cx.debug_bounds("recap-summary").expect("the summary shows");
    let last = cx.debug_bounds("recap-summary-at-2").expect("the question is laid out");
    assert!(
        last.bottom() > pane.bottom(),
        "the question starts below the fold: {last:?} in {pane:?}"
    );
    for _ in 0..first + 2 {
        press(cx, "tab");
    }
    let last = cx.debug_bounds("recap-summary-at-2").expect("the question is laid out");
    assert!(
        last.top() >= pane.top() && last.bottom() <= pane.bottom(),
        "the focused question is in view: {last:?} in {pane:?}"
    );
    press(cx, "enter");
    assert!(drawn(cx, LINES[2]), "Enter plays the point in view");
}

/// A window height that shows only the start of the summary.
const SHORT_WINDOW: f32 = 420.0;

/// A window on the stored summary, its question moved to 0:04, its pill clicked (the focus on it); `height` makes the
/// window shorter.
fn keyed_summary(height: Option<f32>) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let rig = super::summary_qa::transcribed_rig();
    let dir = tempfile::tempdir().unwrap();
    // Enter on Regenerate starts this stub, never a real tool.
    installed(
        &rig,
        Tools::with(vec![(Backend::Claude, script(dir.path(), "exit 1\n"))]),
    );
    let (shell, cx) = kit_window(&rig, fixtures::TRANSCRIPT_ONLY);
    if let Some(height) = height {
        cx.simulate_resize(gpui::size(gpui::px(1600.0), gpui::px(height)));
        draw(cx);
    }
    let mut made = super::summary_qa::stored();
    made.summary.questions[0].at = Some(4);
    cx.update(|_, cx| {
        teams_ui::recap_summary::keep(&fixtures::transcript_location("ITEM9"), &made, None, cx);
    });
    super::summary_qa::summary_pane(cx);
    click(cx, "recap-pane-summary shown");
    (shell, cx)
}
