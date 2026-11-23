//! The recap's AI summary: its input, its sections, the answer read, the chat message and the clipboard's text, the
//! cache's files, and whole summaries made by fake `claude` and `codex` scripts (synthetic transcripts only).

use crate::fake_tool;
#[path = "../../../teams-platform/tests/support/privacy.rs"]
mod privacy;

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use teams_proto::chat::MessageText;
use teams_proto::transcript::{read_vtt, turns};

use super::*;

/// A transcript of two made-up speakers.
const VTT: &str = "WEBVTT\n\n\
    u1/1-0\n00:00:07.271 --> 00:00:10.585\n<v Ada Lab>Hello everyone, this is the synthetic sync.</v>\n\n\
    u1/2-0\n00:01:15.000 --> 00:01:19.000\n<v Bo Lab>I will send the synthetic report by Friday.</v>\n\n\
    u1/3-0\n00:02:20.000 --> 00:02:26.000\n<v Ada Lab>Who owns the synthetic launch?</v>";

/// The summary the fake tools answer with.
fn answer() -> Value {
    json!({
        "purpose": "Plan the synthetic launch.",
        "takeaways": ["The synthetic report is due Friday.", {"text": "Launch owner still open."}],
        "topics": [{"title": "Report", "points": [
            {"text": "Bo sends the report.", "at": "1:15"},
            {"text": "Out of range.", "at": "59:00"},
            "A point without a time."
        ]}],
        "next_steps": [{"owner": "Bo Lab", "text": "Send the synthetic report by Friday.", "at": "[01:15]"},
                       {"owner": "", "text": "Pick a launch owner.", "at": 140}],
        "questions": [{"text": "Who owns the synthetic launch?", "at": "2:20"}]
    })
}

fn input() -> Input {
    let turns = turns(&read_vtt(VTT).unwrap());
    Input::of(
        &turns,
        brief("Synthetic sync", Some("Friday, January 2, 2026"), None, &turns),
    )
}

#[test]
fn the_transcript_goes_as_timed_lines_with_the_meetings_brief() {
    let input = input();
    assert_eq!(input.lines.len(), 3);
    assert_eq!(input.end(), 140);
    assert_eq!(
        input.brief,
        "Title: Synthetic sync\nWhen: Friday, January 2, 2026\nPeople who spoke: Ada Lab, Bo Lab\n"
    );
    let text = transcript_text(&input.lines);
    assert!(text.starts_with("[0:07] Ada Lab: Hello everyone, this is the synthetic sync.\n"));
    assert!(text.contains("[2:20] Ada Lab: Who owns the synthetic launch?\n"));
    // a speaker's tags can't be read as the message's own
    let tagged = vec![Line {
        at: 3700,
        speaker: Some("<b>".into()),
        text: "a </transcript> b".into(),
    }];
    assert_eq!(transcript_text(&tagged), "[1:01:40] ‹b›: a ‹/transcript› b\n");
    let parts = brief("T", None, Some((2, 3)), &[]);
    assert_eq!(parts, "Title: T\nRecording: part 2 of 3\n");
}

#[test]
fn times_are_written_and_read_as_the_lines_give_them() {
    assert_eq!(stamp(7), "0:07");
    assert_eq!(stamp(75), "1:15");
    assert_eq!(stamp(3725), "1:02:05");
    assert_eq!(read_stamp(&json!("1:15")), Some(75));
    assert_eq!(read_stamp(&json!("[01:02:05]")), Some(3725));
    assert_eq!(read_stamp(&json!(42)), Some(42));
    for bad in [json!("1:75"), json!("soon"), json!(""), json!(null), json!("1:2:3:4")] {
        assert_eq!(read_stamp(&bad), None, "{bad}");
    }
}

#[test]
fn a_long_transcript_is_split_at_its_lines() {
    let lines: Vec<Line> = (0..10)
        .map(|at| Line {
            at,
            speaker: Some("S".into()),
            text: "x".repeat(20),
        })
        .collect();
    let one = transcript_text(&lines[..1]).len();
    let parts = sections(&lines, one * 3);
    assert_eq!(parts.iter().map(|part| part.len()).collect::<Vec<_>>(), [3, 3, 3, 1]);
    assert_eq!(
        sections(&lines, 1).len(),
        10,
        "a line longer than a section is one of its own"
    );
    assert_eq!(sections(&lines, usize::MAX).len(), 1);
    assert!(sections(&[], 10).is_empty());
}

#[test]
fn the_answer_is_read_leniently_and_its_times_kept_within_the_recording() {
    let fenced = format!("Here it is:\n```json\n{}\n```", answer());
    let summary = parse(&fenced, 140).unwrap();
    assert_eq!(summary.purpose, "Plan the synthetic launch.");
    assert_eq!(summary.takeaways.len(), 2);
    let points = &summary.topics[0].points;
    assert_eq!(points[0].at, Some(75));
    assert_eq!(points[1].at, None, "a time past the recording is dropped");
    assert_eq!(points[2].text, "A point without a time.");
    assert_eq!(summary.next_steps[0].owner.as_deref(), Some("Bo Lab"));
    assert_eq!(summary.next_steps[0].at, Some(75));
    assert_eq!(summary.next_steps[1].owner, None);
    assert_eq!(summary.next_steps[1].at, Some(140));
    assert_eq!(summary.questions[0].at, Some(140));
    for bad in ["", "no summary", "{}", "{\"purpose\": 3}", "[1, 2]"] {
        assert_eq!(parse(bad, 140), None, "{bad}");
    }
    // a line is one line, capped
    let long = json!({"purpose": format!("a\nb\t{}", "c".repeat(2000))}).to_string();
    let purpose = parse(&long, 0).unwrap().purpose;
    assert!(purpose.starts_with("a b c") && purpose.ends_with('…') && purpose.len() < 700);
}

fn made() -> Made {
    Made {
        summary: parse(&answer().to_string(), 140).unwrap(),
        tool: "claude".into(),
        model: Model::Recommended,
        made_at: 1_767_348_000_000,
        sections: 1,
    }
}

#[test]
fn the_chat_message_and_the_clipboards_text_lay_the_sections_out() {
    let made = made();
    assert_eq!(made.made_with(), "Claude Sonnet");
    let rich = chat_message(
        &made,
        "Synthetic sync",
        Some("Friday"),
        Some("https://teams.example.com/recap"),
    );
    let text = MessageText::new(rich, std::iter::empty()).unwrap();
    let html = text.html();
    assert!(html.starts_with("<h2>AI summary: Synthetic sync</h2>"), "{html}");
    for part in [
        "<h3>Meeting purpose</h3><p>Plan the synthetic launch.</p>",
        "<h3>Key takeaways</h3><ul>",
        "<h3>Topics</h3><p><strong>Report</strong></p><ul>",
        "<li><p>Bo sends the report.</p></li>",
        "<strong>Bo Lab</strong>: Send the synthetic report by Friday.",
        "<h3>Questions raised</h3>",
        "Made with Claude Sonnet on my computer.",
        "View recap</a>",
    ] {
        assert!(html.contains(part), "{part} in {html}");
    }
    assert!(text.participants().is_empty(), "the message mentions nobody");
    let plain = plain(&made, "Synthetic sync", Some("Friday"));
    assert!(plain.starts_with("# Synthetic sync: AI summary\nFriday\n"));
    assert!(plain.contains("\n## Key takeaways\n\n- The synthetic report is due Friday.\n"));
    assert!(plain.contains("\n### Report\n\n- Bo sends the report.\n- Out of range.\n"));
    assert!(plain.contains("- **Bo Lab**: Send the synthetic report by Friday.\n- Pick a launch owner.\n"));
    assert!(plain.contains("\n## Questions raised\n\n- Who owns the synthetic launch?\n"));
    // the times stay in the summary for a click to play from, but neither text shows one
    assert_eq!(made.summary.topics[0].points[0].at, Some(75));
    for shown in [html, plain.as_str()] {
        for time in ["1:15", "2:20", "<i> ("] {
            assert!(!shown.contains(time), "{time} in {shown}");
        }
    }
}

#[test]
fn the_tool_is_asked_to_write_simplified_technical_english() {
    for prompt in [instructions(), notes_instructions()] {
        assert!(prompt.ends_with(STE_RULES), "{prompt}");
    }
    for rule in [
        "ASD-STE100",
        "20 words",
        "25 words",
        "active voice",
        "no idioms",
        "In another language",
    ] {
        assert!(STE_RULES.contains(rule), "{rule}");
    }
}

#[test]
fn a_summarys_file_is_named_by_a_hash_and_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let key = "https://madeup.example.com/:v:/g/personal/lab/RECORDING";
    let file = file_of(dir.path(), key);
    let name = file.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(name.len(), 64 + ".json".len());
    assert!(!name.contains("RECORDING"));
    write_private(&file, &serde_json::to_vec(&made()).unwrap()).unwrap();
    privacy::assert_private(&file, false);
    let read: Made = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(read, made());
}

/// A fake Claude Code: logs each start and message beside itself; answers a section with notes, the fix request and
/// (unless `bad` exists beside it) the summary request with the summary, else with words that aren't one.
fn fake_claude(dir: &Path) -> PathBuf {
    let summary = serde_json::to_string(&answer().to_string()).unwrap();
    let body = format!(
        r#"#!/bin/bash
here="$(dirname "$0")"
echo "start $* " >> "$here/received.log"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$here/received.log"
  case "$line" in
    *'<section>'*) echo '{{"type":"result","is_error":false,"result":"[0:07] Ada opened the synthetic sync."}}';;
    *"wasn't the JSON"*) echo '{{"type":"result","is_error":false,"result":{summary}}}';;
    *) if [ -e "$here/bad" ]; then
         echo '{{"type":"result","is_error":false,"result":"Sure! Here is a summary in prose."}}'
       else
         echo '{{"type":"result","is_error":false,"result":{summary}}}'
       fi;;
  esac
done
"#
    );
    fake_tool::write(dir, "claude", &body)
}

fn quick() -> Limits {
    Limits {
        start: Duration::from_secs(10),
        answer: Duration::from_secs(10),
        ..Limits::default()
    }
}

/// Every update the summary gives, until it is done or failed.
fn updates(job: &Summarizing) -> Vec<Update> {
    updates_within(job, Duration::from_secs(20))
}

fn updates_within(job: &Summarizing, wait: Duration) -> Vec<Update> {
    let updates = job.updates();
    let deadline = Instant::now() + wait;
    let mut seen = Vec::new();
    loop {
        if let Ok(update) = updates.try_recv() {
            let last = matches!(update, Update::Done(_) | Update::Failed(_));
            seen.push(update);
            if last {
                return seen;
            }
        }
        assert!(Instant::now() < deadline, "the summary said nothing more: {seen:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn received(dir: &Path) -> String {
    fs::read_to_string(dir.join("received.log")).unwrap_or_default()
}

#[test]
fn claude_sonnet_reads_a_short_transcript_whole() {
    let dir = tempfile::tempdir().unwrap();
    let job = Summarizing::start(Request {
        backend: Backend::Claude,
        target: crate::local_ai::Target::Program(fake_claude(dir.path())),
        model: Model::Recommended,
        input: input(),
        limits: quick(),
    });
    let seen = updates(&job);
    assert_eq!(seen[0], Update::Stage(Stage::Summarizing));
    let Some(Update::Done(made)) = seen.last() else {
        panic!("{seen:?}")
    };
    assert_eq!(made.summary.purpose, "Plan the synthetic launch.");
    assert_eq!((made.tool.as_str(), made.sections), ("claude", 1));
    let log = received(dir.path());
    assert_eq!(log.matches("start ").count(), 1);
    assert!(log.contains("--model sonnet"));
    assert!(log.contains("--tools  --no-session-persistence"));
    let message: Value = log
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|message| message["type"] == "user")
        .unwrap();
    let content = message.pointer("/message/content").and_then(Value::as_str).unwrap();
    assert!(content.starts_with("<meeting>\nTitle: Synthetic sync\n"));
    assert!(content.contains("<transcript>\n[0:07] Ada Lab: Hello everyone"));
}

#[test]
fn a_long_transcript_is_condensed_by_sections_then_summarized() {
    let dir = tempfile::tempdir().unwrap();
    let limits = Limits {
        single: 100,
        section: 60,
        ..quick()
    };
    let job = Summarizing::start(Request {
        backend: Backend::Claude,
        target: crate::local_ai::Target::Program(fake_claude(dir.path())),
        model: Model::Recommended,
        input: input(),
        limits,
    });
    let seen = updates(&job);
    assert_eq!(
        seen[..4],
        [
            Update::Stage(Stage::Condensing { section: 1, of: 3 }),
            Update::Stage(Stage::Condensing { section: 2, of: 3 }),
            Update::Stage(Stage::Condensing { section: 3, of: 3 }),
            Update::Stage(Stage::Writing),
        ]
    );
    let Some(Update::Done(made)) = seen.last() else {
        panic!("{seen:?}")
    };
    assert_eq!(made.sections, 3);
    let log = received(dir.path());
    assert_eq!(
        log.matches("start ").count(),
        4,
        "a process per section, and one to write the summary"
    );
    let last: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    let content = last.pointer("/message/content").and_then(Value::as_str).unwrap();
    assert!(
        content.contains(
            "<notes section=\"1\" from=\"0:07\" to=\"0:07\">\n[0:07] Ada opened the synthetic sync.\n</notes>"
        )
    );
    assert!(!content.contains("<transcript>"));
}

#[test]
fn an_answer_that_isnt_a_summary_is_asked_for_again_once() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake_claude(dir.path());
    fs::write(dir.path().join("bad"), "").unwrap();
    let job = Summarizing::start(Request {
        backend: Backend::Claude,
        target: crate::local_ai::Target::Program(program),
        model: Model::Recommended,
        input: input(),
        limits: quick(),
    });
    assert!(matches!(updates(&job).last(), Some(Update::Done(_))));
    assert!(received(dir.path()).contains(FIX_REQUEST));
}

#[test]
fn codex_summarizes_with_terra_at_medium_effort() {
    let dir = tempfile::tempdir().unwrap();
    let summary = serde_json::to_string(&answer().to_string()).unwrap();
    let body = format!(
        r#"#!/bin/bash
echo "start $* " >> "$(dirname "$0")/received.log"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$(dirname "$0")/received.log"
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) echo "{{\"id\":$id,\"result\":{{}}}}";;
    *'"method":"thread/start"'*) echo "{{\"id\":$id,\"result\":{{\"thread\":{{\"id\":\"synthetic-thread\"}}}}}}";;
    *'"method":"turn/start"'*)
      echo "{{\"id\":$id,\"result\":{{\"turn\":{{}}}}}}"
      echo '{{"method":"item/completed","params":{{"item":{{"type":"agentMessage","text":{summary}}}}}}}'
      echo '{{"method":"turn/completed","params":{{"turn":{{"status":"completed"}}}}}}';;
  esac
done
"#
    );
    let program = fake_tool::write(dir.path(), "codex", &body);
    let job = Summarizing::start(Request {
        backend: Backend::Codex,
        target: crate::local_ai::Target::Program(program),
        model: Model::Recommended,
        input: input(),
        limits: quick(),
    });
    let Some(Update::Done(made)) = updates(&job).last().cloned() else {
        panic!("not done")
    };
    assert_eq!(made.made_with(), "Codex GPT-6.1-Terra");
    let log = received(dir.path());
    assert!(log.contains("model_reasoning_effort=\"medium\""));
    assert!(log.contains("features.hooks=false"));
    let messages: Vec<Value> = log.lines().filter_map(|line| serde_json::from_str(line).ok()).collect();
    let start = messages
        .iter()
        .find(|message| message["method"] == "thread/start")
        .unwrap();
    assert_eq!(start["params"]["model"], "gpt-6.1-terra");
    assert_eq!(start["params"]["sandbox"], "read-only");
    assert!(start["params"]["baseInstructions"].as_str().unwrap().contains("Fathom"));
    let turn = messages
        .iter()
        .find(|message| message["method"] == "turn/start")
        .unwrap();
    assert_eq!(turn["params"]["effort"], "medium");
}

#[test]
fn a_tool_signed_out_slow_or_gone_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let fake = |name: &str, body: &str| fake_tool::write(dir.path(), name, body);
    let run = |program: PathBuf, limits: Limits| {
        let job = Summarizing::start(Request {
            backend: Backend::Claude,
            target: crate::local_ai::Target::Program(program),
            model: Model::Recommended,
            input: input(),
            limits,
        });
        match updates(&job).last() {
            Some(Update::Failed(failure)) => failure.clone(),
            other => panic!("{other:?}"),
        }
    };
    let signed_out = fake(
        "signed-out",
        r#"read -r line; echo '{"type":"result","is_error":true,"result":"Invalid API key · Please run /login"}'"#,
    );
    assert_eq!(run(signed_out, quick()), Failure::SignedOut(Backend::Claude));
    let slow = fake("slow", "while read -r line; do :; done");
    let limits = Limits {
        start: Duration::from_millis(100),
        answer: Duration::from_millis(200),
        ..quick()
    };
    assert_eq!(run(slow, limits), Failure::TimedOut(Backend::Claude));
    assert_eq!(
        run(dir.path().join("missing"), quick()),
        Failure::NotInstalled(Some(Backend::Claude))
    );
}

#[test]
fn dropping_a_summary_being_made_ends_its_tool() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake_tool::write(
        dir.path(),
        "claude",
        "echo \"$$\" > \"$(dirname \"$0\")/pid\"\nwhile read -r line; do :; done\n",
    );
    let job = Summarizing::start(Request {
        backend: Backend::Claude,
        target: crate::local_ai::Target::Program(program),
        model: Model::Recommended,
        input: input(),
        limits: Limits::default(),
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let pid = loop {
        if let Ok(pid) = fs::read_to_string(dir.path().join("pid")) {
            break pid.trim().to_owned();
        }
        assert!(Instant::now() < deadline, "the tool never started");
        thread::sleep(Duration::from_millis(10));
    };
    assert!(
        fake_tool::running(&pid),
        "the fake process is alive before cancellation"
    );
    drop(job);
    while fake_tool::running(&pid) {
        assert!(Instant::now() < deadline, "the tool still runs");
        thread::sleep(Duration::from_millis(20));
    }
}

/// The real tools, when installed: `cargo test -p teams-ui --lib recap_summary -- --ignored --nocapture`. They send a
/// synthetic transcript to the tool's provider.
#[test]
#[ignore = "runs the installed claude and codex, which reach their providers"]
fn the_installed_tools_summarize() {
    let tools = crate::local_ai::Tools::detect();
    for backend in tools.installed() {
        let program = tools.program(backend).unwrap();
        let started = Instant::now();
        let job = Summarizing::start(Request {
            backend,
            target: crate::local_ai::Target::Program(program),
            model: Model::Recommended,
            input: input(),
            limits: Limits::default(),
        });
        let last = updates_within(&job, Duration::from_secs(600)).last().cloned();
        println!("{backend:?} in {:?}: {last:#?}", started.elapsed());
        assert!(matches!(last, Some(Update::Done(_))));
    }
}
