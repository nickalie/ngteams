//! QA of task 795 (the recap's AI summary without times, written in Simplified Technical English), as a user does it
//! in the meeting chat's Recap: a stored summary with moments, an old summary file, the keyboard, Regenerate with a
//! stub `claude`, Copy and Send to chat. Every id, name and word is made up; no real AI tool runs.

use std::path::Path;

use super::summary::{drawn, installed, script, sent, wait_for};
use super::*;
use teams_ui::local_ai::{Backend, Tools};
use teams_ui::recap_summary::{Made, Point, STE_RULES, Step, Summary, Topic};
use teams_ui::settings::SettingsFile;

/// The moments of the stored summary, in seconds, and the time text each one had before this change.
const MOMENTS: [(u64, &str); 3] = [(15, "0:15"), (20, "0:20"), (24, "0:24")];

/// A stored summary of the transcribed meeting: one point, one next step and one question with a moment each, and a
/// point without one.
fn stored() -> Made {
    Made {
        summary: Summary {
            purpose: "Check the made-up video player.".to_owned(),
            takeaways: vec!["The video plays and the transcript follows it.".to_owned()],
            topics: vec![Topic {
                title: "Video player".to_owned(),
                points: vec![
                    Point {
                        text: "Bo said that the video plays.".to_owned(),
                        at: Some(MOMENTS[0].0),
                    },
                    Point {
                        text: "The team likes the player.".to_owned(),
                        at: None,
                    },
                ],
            }],
            next_steps: vec![Step {
                owner: Some("Ada Lab".to_owned()),
                text: "Examine the search.".to_owned(),
                at: Some(MOMENTS[1].0),
            }],
            questions: vec![Point {
                text: "Does the search find each video?".to_owned(),
                at: Some(MOMENTS[2].0),
            }],
        },
        tool: Backend::Claude.key().to_owned(),
        model: teams_ui::local_ai::Model::default(),
        made_at: fixtures::START,
        sections: 1,
    }
}

/// The recap of the transcribed meeting (no recording), its transcript answered.
fn transcribed_rig() -> Rig {
    let rig = rig(false);
    locked(&rig.core.transcripts).insert(fixtures::transcript_location("ITEM9"), fixtures::VTT.to_owned());
    rig
}

/// The summary pane of `chat`'s Recap.
fn summary_pane(cx: &mut VisualTestContext) {
    click(cx, "chat-tab-recap");
    show_summary(cx);
    assert!(drawn(cx, "recap-summary-made"), "the stored summary shows");
}

/// The summary pane shown, if the transcript is shown.
fn show_summary(cx: &mut VisualTestContext) {
    if drawn(cx, "recap-pane-summary") {
        click(cx, "recap-pane-summary");
    }
    assert!(drawn(cx, "recap-pane-summary shown"));
}

/// Each point with a moment is a row of the full width with only its bullet before its words, which start with the
/// point's own text or owner: no time column and no time chip.
fn no_time_column(cx: &mut VisualTestContext, moments: usize) {
    for index in 0..moments {
        let selector =
            |part: &str| -> &'static str { Box::leak(format!("recap-summary-at-{index}{part}").into_boxed_str()) };
        let row = cx
            .debug_bounds(selector(""))
            .unwrap_or_else(|| panic!("point {index} is a row"));
        let words = cx.debug_bounds(selector("-words")).unwrap();
        assert!(
            words.left() - row.left() <= px(24.0),
            "point {index}: only the bullet before the words: {row:?} {words:?}"
        );
        assert_eq!(words.right(), row.right(), "point {index}: nothing after the words");
    }
    assert!(!drawn(
        cx,
        Box::leak(format!("recap-summary-at-{moments}").into_boxed_str())
    ));
}

fn spoken(cx: &mut VisualTestContext, turn: usize) -> bool {
    drawn(
        cx,
        Box::leak(format!("recap-paragraph-{turn}-0 spoken").into_boxed_str()),
    )
}

/// The time texts of the stored summary's moments, also as the old view wrote them.
fn assert_no_time(shown: &str) {
    for (_, time) in MOMENTS {
        assert!(!shown.contains(time), "{time} in {shown}");
    }
    assert!(!shown.contains("(0:"), "a time in {shown}");
}

#[test]
fn qa_a_stored_summary_shows_no_time_and_a_click_on_a_point_plays_its_moment() {
    let rig = transcribed_rig();
    let (_shell, cx) = shell_window(&rig, fixtures::TRANSCRIPT_ONLY);
    cx.update(|_, cx| {
        teams_ui::recap_summary::keep(&fixtures::transcript_location("ITEM9"), &stored(), None, cx);
    });
    summary_pane(cx);
    no_time_column(cx, 3);
    // the point at 0:15 (Bo's paragraph), the next step at 0:20 (Ada's last), each clicked
    for (point, turn) in [(0, 1), (1, 2)] {
        show_summary(cx);
        click(cx, Box::leak(format!("recap-summary-at-{point}").into_boxed_str()));
        assert!(
            drawn(cx, "recap-pane-transcript shown"),
            "point {point}: the transcript shows"
        );
        assert!(spoken(cx, turn), "point {point}: the paragraph of its moment is marked");
    }
}

#[test]
fn qa_copy_and_send_to_chat_of_a_stored_summary_have_no_time() {
    let rig = transcribed_rig();
    let (_shell, cx) = shell_window(&rig, fixtures::TRANSCRIPT_ONLY);
    cx.update(|_, cx| {
        teams_ui::recap_summary::keep(&fixtures::transcript_location("ITEM9"), &stored(), None, cx);
    });
    summary_pane(cx);
    click(cx, "recap-summary-copy");
    let copied = cx
        .update(|_, cx| cx.read_from_clipboard())
        .and_then(|item| item.text())
        .unwrap();
    assert!(copied.contains("- Bo said that the video plays.\n"), "{copied}");
    assert!(copied.contains("- **Ada Lab**: Examine the search.\n"), "{copied}");
    assert!(copied.contains("- Does the search find each video?"), "{copied}");
    assert_no_time(&copied);
    click(cx, "recap-summary-send");
    click(cx, "recap-summary-send-confirm");
    let sent = sent(&rig);
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].1.contains("<strong>Ada Lab</strong>: Examine the search."),
        "{}",
        sent[0].1
    );
    assert_no_time(&sent[0].1);
}

/// A summary file as an earlier build wrote it (before this change, and before a summary kept its model).
const OLD_FILE: &str = r#"{"summary":{"purpose":"Check the made-up video player.","takeaways":["The video plays and the transcript follows it."],"topics":[{"title":"Video player","points":[{"text":"Bo said that the video plays.","at":15},{"text":"The team likes the player.","at":null}]}],"next_steps":[{"owner":"Ada Lab","text":"Examine the search.","at":20}],"questions":[{"text":"Does the search find each video?","at":24}]},"tool":"claude","made_at":1767348000000,"sections":1}"#;

#[test]
fn qa_an_old_summary_file_opens_without_times() {
    let rig = transcribed_rig();
    let dir = tempfile::tempdir().unwrap();
    let summaries = dir.path().join(teams_ui::recap_summary::SUMMARY_DIR);
    std::fs::create_dir_all(&summaries).unwrap();
    let file = teams_ui::recap_summary::file_of(&summaries, &fixtures::transcript_location("ITEM9"));
    assert_eq!(
        serde_json::from_str::<Made>(OLD_FILE).unwrap(),
        stored(),
        "the old file reads as the summary"
    );
    std::fs::write(&file, OLD_FILE).unwrap();
    let mut parts = parts(&rig, fixtures::TRANSCRIPT_ONLY);
    let settings_file = SettingsFile::new(dir.path().join("settings.json"));
    parts.settings = rig.cx.update(|cx| cx.new(|_| SettingsModel::load(settings_file)));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.simulate_resize(size(px(1600.0), px(1000.0)));
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_chat(fixtures::TRANSCRIPT_ONLY.to_owned(), window, cx);
        });
    });
    draw(cx);
    summary_pane(cx);
    no_time_column(cx, 3);
    click(cx, "recap-summary-copy");
    let copied = cx
        .update(|_, cx| cx.read_from_clipboard())
        .and_then(|item| item.text())
        .unwrap();
    assert!(copied.contains("- Bo said that the video plays.\n"), "{copied}");
    assert_no_time(&copied);
}

/// A stub `claude` that writes its arguments to `args.log` beside it, then answers with a made-up summary.
fn stub_claude(dir: &Path) -> std::path::PathBuf {
    let summary = serde_json::json!({
        "purpose": "Check the made-up video player.",
        "takeaways": ["The video plays."],
        "topics": [{"title": "Player", "points": [{"text": "Bo said that the video plays.", "at": "0:15"}]}],
        "next_steps": [{"owner": "Ada Lab", "text": "Examine the search.", "at": "0:20"}],
        "questions": []
    });
    let answer = serde_json::to_string(&summary.to_string()).unwrap();
    let body = format!(
        "printf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args.log\"\nwhile IFS= read -r line; do\n  echo \
         '{{\"type\":\"result\",\"is_error\":false,\"result\":{answer}}}'\ndone\n"
    );
    script(dir, &body)
}

#[test]
fn qa_regenerate_asks_the_tool_for_simplified_technical_english_and_shows_the_result() {
    let rig = transcribed_rig();
    let dir = tempfile::tempdir().unwrap();
    installed(&rig, Tools::with(vec![(Backend::Claude, stub_claude(dir.path()))]));
    let (_shell, cx) = shell_window(&rig, fixtures::TRANSCRIPT_ONLY);
    cx.update(|_, cx| {
        teams_ui::recap_summary::keep(&fixtures::transcript_location("ITEM9"), &stored(), None, cx);
    });
    summary_pane(cx);
    assert!(drawn(cx, "recap-summary-questions"), "the stored summary has questions");
    click(cx, "recap-summary-regenerate");
    for _ in 0..1000 {
        draw(cx);
        if drawn(cx, "recap-summary-made") && !drawn(cx, "recap-summary-questions") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    wait_for(cx, "recap-summary-made");
    assert!(!drawn(cx, "recap-summary-questions"), "the new summary shows");
    let args = std::fs::read_to_string(dir.path().join("args.log")).unwrap();
    assert!(args.contains(STE_RULES), "the prompt ends with the STE rules: {args}");
    assert!(args.contains("ASD-STE100 Simplified Technical English"));
    assert!(args.contains("Keep names, product terms and quotes as they were spoken."));
    no_time_column(cx, 2);
    click(cx, "recap-summary-at-1");
    assert!(spoken(cx, 2), "the new next step plays from 0:20");
}

/// Tab moves the focus from the summary pane's tab to the points; Enter on a point plays its moment.
#[test]
#[ignore = "QA 795 Q1: a summary point takes no focus and no Enter (the same on main)"]
fn qa_tab_reaches_each_point_and_enter_plays_its_moment() {
    let rig = transcribed_rig();
    let (_shell, cx) = shell_window(&rig, fixtures::TRANSCRIPT_ONLY);
    cx.update(|_, cx| {
        teams_ui::recap_summary::keep(&fixtures::transcript_location("ITEM9"), &stored(), None, cx);
    });
    summary_pane(cx);
    let mut played = Vec::new();
    for tabs in 1..=30 {
        show_summary(cx);
        for _ in 0..tabs {
            cx.simulate_keystrokes("tab");
        }
        cx.simulate_keystrokes("enter");
        draw(cx);
        if drawn(cx, "recap-pane-transcript shown") {
            played.push(tabs);
        }
    }
    assert!(
        played.len() >= 3,
        "Tab and Enter reach the three points with a moment; Enter played after these Tab counts: {played:?}"
    );
}
