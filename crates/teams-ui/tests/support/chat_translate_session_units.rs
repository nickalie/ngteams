//! A chat's translating session against a fake `claude` (synthetic messages only): a batch and an outgoing message
//! answered in order, the process renewed once worn, and a failure told with its job, the next job starting the tool
//! anew.

use crate::fake_tool;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::*;

/// A fake Claude Code: logs its start and each question beside itself; answers a batch's first message, an outgoing
/// message, and anything holding "die" by exiting.
const FAKE: &str = r#"#!/bin/bash
log="$(dirname "$0")/received.log"
echo "start $$" >> "$log"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  case "$line" in
    *die*) echo 'Synthetic crash' >&2; exit 3;;
    *'<translate>'*) printf '{"type":"result","is_error":false,"result":"<m id=\\"1\\" lang=\\"en\\">DE one</m>"}\n';;
    *'<outgoing'*) printf '{"type":"result","is_error":false,"result":"<out lang=\\"en\\">EN two</out>"}\n';;
    *) printf '{"type":"result","is_error":false,"result":"nonsense"}\n';;
  esac
done
"#;

fn fake(dir: &Path) -> PathBuf {
    fake_tool::write(dir, "claude", FAKE)
}

fn next(session: &Session) -> Outcome {
    let outcomes = session.outcomes();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(outcome) = outcomes.try_recv() {
            return outcome;
        }
        assert!(Instant::now() < deadline, "the session said nothing");
        thread::sleep(Duration::from_millis(10));
    }
}

fn batch(id: u64, text: &str) -> Job {
    Job::Batch {
        id,
        brief: Arc::new(Brief::default()),
        context: vec![Said::new("Lab B", "Earlier")],
        items: vec![Item {
            n: 1,
            key: String::new(),
            from: "Lab B".to_owned(),
            text: text.to_owned(),
            re: None,
        }],
    }
}

#[test]
fn jobs_are_answered_in_order_and_a_worn_process_is_renewed() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(Spec {
        backend: Backend::Claude,
        program: fake(dir.path()),
        model: Model::Recommended,
        language: "German",
        limits: Limits {
            turns: 2,
            chars: 1_000_000,
            first: Duration::from_secs(10),
            turn: Duration::from_secs(10),
        },
    });
    session.send(batch(1, "one"));
    match next(&session) {
        Outcome::Batch {
            id: 1,
            answers: Ok(answers),
            ..
        } => {
            assert_eq!(
                answers.get(&1),
                Some(&Answer::Translated {
                    text: "DE one".to_owned(),
                    source: Some("en".to_owned()),
                })
            );
        }
        other => panic!("{other:?}"),
    }
    session.send(Job::Outgoing {
        id: 2,
        brief: Arc::new(Brief::default()),
        context: Vec::new(),
        re: None,
        to: Some("en".to_owned()),
        text: "zwei".to_owned(),
    });
    match next(&session) {
        Outcome::Outgoing { id: 2, answer } => {
            assert_eq!(answer, Ok((Some("EN two".to_owned()), Some("en".to_owned()))));
        }
        other => panic!("{other:?}"),
    }
    // the third question goes to a new process
    session.send(Job::Outgoing {
        id: 3,
        brief: Arc::new(Brief::default()),
        context: Vec::new(),
        re: None,
        to: None,
        text: "x".to_owned(),
    });
    match next(&session) {
        Outcome::Outgoing { id: 3, answer: Ok(_) } => {}
        other => panic!("{other:?}"),
    }
    let log = fs::read_to_string(dir.path().join("received.log")).unwrap();
    assert_eq!(log.matches("start ").count(), 2, "{log}");
    assert!(log.contains("<context>") && log.contains("Earlier"));
    drop(session);
}

#[test]
fn a_failure_is_told_with_its_job_and_the_next_job_starts_the_tool_anew() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(Spec {
        backend: Backend::Claude,
        program: fake(dir.path()),
        model: Model::Recommended,
        language: "German",
        limits: Limits {
            turns: 100,
            chars: 1_000_000,
            first: Duration::from_secs(10),
            turn: Duration::from_secs(10),
        },
    });
    session.send(batch(1, "please die"));
    match next(&session) {
        Outcome::Batch {
            id: 1,
            answers: Err(Failure::Stopped(Backend::Claude, Some(why))),
            ..
        } => {
            assert!(why.contains("Synthetic crash"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    session.send(Job::Outgoing {
        id: 2,
        brief: Arc::new(Brief::default()),
        context: Vec::new(),
        re: None,
        to: Some("en".to_owned()),
        text: "nonsense please".to_owned(),
    });
    match next(&session) {
        Outcome::Outgoing { id: 2, answer } => assert_eq!(answer.map(|(text, _)| text), Ok(Some("EN two".to_owned()))),
        other => panic!("{other:?}"),
    }
    // a tool that isn't there fails its job
    let missing = Session::start(Spec {
        backend: Backend::Claude,
        program: dir.path().join("no-such-claude"),
        model: Model::Recommended,
        language: "German",
        limits: Limits::default(),
    });
    missing.send(batch(5, "one"));
    match next(&missing) {
        Outcome::Batch {
            id: 5,
            answers: Err(Failure::NotInstalled(Some(Backend::Claude))),
            ..
        } => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(
        not_a_translation(Backend::Codex).message(),
        "The Codex CLI stopped: its answer wasn't a translation."
    );
}

/// A fake Claude Code that streams a batch's answer (`--include-partial-messages`), logging its arguments: the first
/// message's answer cut mid-way, a delta closing it and starting the second, then the rest; a new message starting
/// over what came before.
const STREAMING_CLAUDE: &str = r#"#!/bin/bash
printf 'args %s\n' "$*" >> "$(dirname "$0")/received.log"
delta() { printf '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"%s"}}}\n' "$1"; }
while IFS= read -r line; do
  echo '{"type":"stream_event","event":{"type":"message_start"}}'
  delta '<m id=\"1\" lang=\"en\">DE o'
  delta 'ne</m>\n<m id=\"2\" lang=\"en\">DE t'
  echo '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{}"}}}'
  delta 'wo</m><m id=\"3\" lang=\"de\" same/>'
  printf '{"type":"result","is_error":false,"result":"%s"}\n' '<m id=\"1\" lang=\"en\">DE one</m>\n<m id=\"2\" lang=\"en\">DE two</m><m id=\"3\" lang=\"de\" same/>'
done
"#;

/// A fake Codex app server that streams a turn's agent message as deltas.
const STREAMING_CODEX: &str = r#"#!/bin/bash
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  delta() { printf '{"method":"item/agentMessage/delta","params":{"itemId":"%s","delta":"%s"}}\n' "$1" "$2"; }
  case "$line" in
    *'"method":"initialize"'*) echo "{\"id\":$id,\"result\":{}}";;
    *'"method":"thread/start"'*) echo "{\"id\":$id,\"result\":{\"thread\":{\"id\":\"synthetic-thread\"}}}";;
    *'"method":"turn/start"'*)
      echo "{\"id\":$id,\"result\":{\"turn\":{}}}"
      delta b '<m id=\"1\" lang=\"en\">DE one</m>'
      delta b '<m id=\"2\" lang=\"en\">DE two</m>'
      echo '{"method":"item/completed","params":{"item":{"type":"agentMessage","text":"<m id=\"1\" lang=\"en\">DE one</m><m id=\"2\" lang=\"en\">DE two</m>"}}}'
      echo '{"method":"turn/completed","params":{"turn":{"status":"completed"}}}';;
  esac
done
"#;

fn three_items() -> Job {
    Job::Batch {
        id: 7,
        brief: Arc::new(Brief::default()),
        context: Vec::new(),
        items: (1..=3)
            .map(|n| Item {
                n,
                key: format!("key {n}"),
                from: "Lab B".to_owned(),
                text: format!("message {n}"),
                re: None,
            })
            .collect(),
    }
}

/// The session's outcomes until the batch's whole answer, its partial answers' numbers in the order they came.
fn partials_then_batch(session: &Session) -> (Vec<Vec<usize>>, HashMap<usize, Answer>) {
    let mut partials = Vec::new();
    loop {
        match next(session) {
            Outcome::Partial { id: 7, answers } => {
                let mut numbers: Vec<usize> = answers.keys().copied().collect();
                numbers.sort_unstable();
                partials.push(numbers);
            }
            Outcome::Batch { id: 7, answers, .. } => return (partials, answers.unwrap()),
            other => panic!("{other:?}"),
        }
    }
}

fn quick() -> Limits {
    Limits {
        turns: 100,
        chars: 1_000_000,
        first: Duration::from_secs(10),
        turn: Duration::from_secs(10),
    }
}

#[test]
fn claude_s_batch_answers_come_a_message_at_a_time_as_it_writes_them() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(Spec {
        backend: Backend::Claude,
        program: fake_tool::write(dir.path(), "claude", STREAMING_CLAUDE),
        model: Model::Recommended,
        language: "German",
        limits: quick(),
    });
    session.send(three_items());
    let (partials, whole) = partials_then_batch(&session);
    // the first came with the delta that closed it, though that delta ends mid-way through the second; the second
    // only once it was closed, the third with it
    assert_eq!(partials, [vec![1], vec![2, 3]]);
    assert_eq!(whole.len(), 3);
    assert_eq!(
        whole.get(&3),
        Some(&Answer::Same {
            source: Some("de".to_owned())
        })
    );
    let log = fs::read_to_string(dir.path().join("received.log")).unwrap();
    assert!(log.contains("--include-partial-messages"), "{log}");
}

#[test]
fn codex_s_batch_answers_come_from_its_message_deltas() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(Spec {
        backend: Backend::Codex,
        program: fake_tool::write(dir.path(), "codex", STREAMING_CODEX),
        model: Model::Recommended,
        language: "German",
        limits: quick(),
    });
    session.send(three_items());
    let (partials, whole) = partials_then_batch(&session);
    assert_eq!(partials, [vec![1], vec![2]]);
    assert_eq!(whole.len(), 2);
}

#[test]
fn only_closed_answers_are_fresh_and_each_once_unless_answered_otherwise() {
    let mut told = HashMap::new();
    assert!(fresh_answers("<m id=\"1\" lang=\"en\">DE o", &mut told).is_empty());
    assert!(fresh_answers("<m id=\"1\" lang=\"en", &mut told).is_empty());
    // a delta that closes the first and starts the second: the first is fresh at once
    let first = fresh_answers(
        "<m id=\"1\" lang=\"en\">DE one</m>\n<m id=\"2\" lang=\"en\">DE t",
        &mut told,
    );
    assert_eq!(first.keys().copied().collect::<Vec<_>>(), [1]);
    // still open after the first: nothing new
    assert!(fresh_answers("<m id=\"1\" lang=\"en\">DE one</m>\n<m id=\"2\">D", &mut told).is_empty());
    let second = fresh_answers(
        "<m id=\"1\" lang=\"en\">DE one</m>\n<m id=\"2\" lang=\"en\" same/>\n",
        &mut told,
    );
    assert_eq!(second.keys().copied().collect::<Vec<_>>(), [2]);
    // a later agent message answering the first otherwise: told again, the corrected one
    let corrected = fresh_answers("<m id=\"1\" lang=\"en\">DE uno</m>", &mut told);
    assert_eq!(
        corrected,
        HashMap::from([(
            1,
            Answer::Translated {
                text: "DE uno".to_owned(),
                source: Some("en".to_owned()),
            }
        )])
    );
    assert!(fresh_answers("<m id=\"1\" lang=\"en\">DE uno</m>", &mut told).is_empty());
}

/// A fake Claude Code that streams the first message's answer, then holds until a `go` file beside itself exists before
/// it writes the rest. It logs its process id.
const GATED_CLAUDE: &str = r#"#!/bin/bash
dir="$(dirname "$0")"
echo "$$" > "$dir/pid"
delta() { printf '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"%s"}}}\n' "$1"; }
while IFS= read -r line; do
  echo '{"type":"stream_event","event":{"type":"message_start"}}'
  delta '<m id=\"1\" lang=\"en\">DE one</m>'
  for _ in $(seq 400); do [ -e "$dir/go" ] && break; sleep 0.05; done
  delta '<m id=\"2\" lang=\"en\">DE two</m>'
  printf '{"type":"result","is_error":false,"result":"%s"}\n' '<m id=\"1\" lang=\"en\">DE one</m><m id=\"2\" lang=\"en\">DE two</m>'
done
"#;

#[test]
fn a_session_dropped_mid_batch_stops_its_tool_and_tells_nothing_more() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(Spec {
        backend: Backend::Claude,
        program: fake_tool::write(dir.path(), "claude", GATED_CLAUDE),
        model: Model::Recommended,
        language: "German",
        limits: quick(),
    });
    session.send(three_items());
    match next(&session) {
        Outcome::Partial { id: 7, answers } => assert_eq!(answers.keys().copied().collect::<Vec<_>>(), [1]),
        other => panic!("{other:?}"),
    }
    let outcomes = session.outcomes();
    let pid = fs::read_to_string(dir.path().join("pid")).unwrap();
    let pid = pid.trim();
    // dropped, and the tool let go on at once: what it still writes comes too late to be told
    drop(session);
    fs::write(dir.path().join("go"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while fake_tool::running(pid) || !outcomes.is_closed() {
        assert!(
            Instant::now() < deadline,
            "the tool or the session's thread is still running"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let late: Vec<Outcome> = std::iter::from_fn(|| outcomes.try_recv().ok()).collect();
    assert!(late.is_empty(), "{late:?}");
}

fn line(key: &str, text: &str) -> Said {
    Said {
        key: key.to_owned(),
        from: "Lab B".to_owned(),
        text: text.to_owned(),
        label: None,
    }
}

fn synthetic_brief(conversation: &str, terms: &[(&str, &str)]) -> Brief {
    Brief {
        conversation: conversation.to_owned(),
        topic: Some("Synthetic topic".to_owned()),
        you: Some("Lab A".to_owned()),
        members: vec!["Lab B".to_owned()],
        terms: terms
            .iter()
            .map(|(term, rendering)| Term {
                term: (*term).to_owned(),
                rendering: (*rendering).to_owned(),
            })
            .collect(),
    }
}

fn item(n: usize, key: &str) -> Item {
    Item {
        n,
        key: key.to_owned(),
        from: "Lab B".to_owned(),
        text: format!("message {n}"),
        re: None,
    }
}

#[test]
fn a_process_is_told_the_chat_its_terms_and_each_line_once() {
    let brief = synthetic_brief("chat-1", &[("Abnahme", "sign-off")]);
    let context = vec![line("a", "first"), line("b", "second")];
    let mut given = Given::default();
    // a new process: everything
    let preface = given.preface(&brief, &context);
    assert_eq!(preface.chat, Some(&brief));
    assert_eq!(preface.terms.len(), 1);
    assert_eq!(preface.context.len(), 2);
    let learned = [Term {
        term: "Lastenheft".to_owned(),
        rendering: "requirements spec".to_owned(),
    }];
    given.told(&preface, &[item(1, "c")], &learned);
    // then only what is new to it: no chat, no term it holds (nor one it named), no line or message it read
    let with_learned = Brief {
        terms: [brief.terms.clone(), learned.to_vec()].concat(),
        ..brief.clone()
    };
    let more = vec![
        line("a", "first"),
        line("c", "message 1"),
        line("d", "third"),
        line("", "keyless"),
    ];
    let preface = given.preface(&with_learned, &more);
    assert_eq!(preface.chat, None);
    assert!(preface.terms.is_empty());
    let texts: Vec<&str> = preface.context.iter().map(|said| said.text.as_str()).collect();
    assert_eq!(texts, ["third", "keyless"]);
    // a term rendered otherwise is told again; a changed chat is told again, with all its terms
    let changed = synthetic_brief("chat-1", &[("Abnahme", "acceptance")]);
    assert_eq!(given.preface(&changed, &[]).terms.len(), 1);
    let renamed = Brief {
        topic: Some("Renamed".to_owned()),
        ..brief.clone()
    };
    let preface = given.preface(&renamed, &[]);
    assert_eq!(preface.chat, Some(&renamed));
    assert_eq!(preface.terms.len(), 1);
    // another chat forgets the first one's terms once told
    let other = synthetic_brief("chat-2", &[]);
    let preface = given.preface(&other, &[]);
    given.told(&preface, &[], &[]);
    assert_eq!(given.preface(&brief, &[]).terms.len(), 1);
}

#[test]
fn a_running_process_gets_at_most_the_warm_window_but_every_line_replied_to() {
    let brief = synthetic_brief("chat-1", &[]);
    let mut given = Given::default();
    let preface = given.preface(&brief, &[]);
    given.told(&preface, &[], &[]);
    let mut context: Vec<Said> = (0..WARM_LINES + 10)
        .map(|n| line(&format!("k{n}"), &format!("line {n}")))
        .collect();
    context[0].label = Some("q1".to_owned());
    let preface = given.preface(&brief, &context);
    assert_eq!(preface.context.len(), WARM_LINES + 1);
    assert_eq!(preface.context[0].label.as_deref(), Some("q1"));
    assert_eq!(preface.context.last().map(|said| said.text.as_str()), Some("line 24"));
    // a long line counts by its characters
    let long = vec![line("x", &"w".repeat(WARM_CHARS)), line("y", "short")];
    let preface = given.preface(&brief, &long);
    assert_eq!(preface.context.len(), 1);
    // a new process takes the whole window
    assert_eq!(Given::default().preface(&brief, &context).context.len(), context.len());
}

#[test]
fn a_running_process_asked_about_another_chat_takes_its_whole_window() {
    // previews ask one process about every chat in a language
    let mut given = Given::default();
    let first = synthetic_brief("chat-1", &[]);
    let preface = given.preface(&first, &[]);
    given.told(&preface, &[], &[]);
    let context: Vec<Said> = (0..WARM_LINES + 10)
        .map(|n| line(&format!("k{n}"), &format!("line {n}")))
        .collect();
    let other = synthetic_brief("chat-2", &[]);
    let preface = given.preface(&other, &context);
    assert_eq!(preface.context.len(), context.len());
    given.told(&preface, &[], &[]);
    // and once it read that chat, only what is new to it
    let more: Vec<Said> = (100..100 + WARM_LINES + 10)
        .map(|n| line(&format!("k{n}"), &format!("line {n}")))
        .collect();
    assert_eq!(given.preface(&other, &more).context.len(), WARM_LINES);
}

/// A fake Claude Code that logs each question and answers a batch with a term after its message.
const TERMING: &str = r#"#!/bin/bash
log="$(dirname "$0")/received.log"
echo "start $$" >> "$log"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  printf '{"type":"result","is_error":false,"result":"%s"}\n' '<m id=\"1\" lang=\"de\">EN</m><term from=\"Abnahme\" to=\"sign-off\"/>'
done
"#;

#[test]
fn a_batch_s_terms_are_told_and_a_new_process_is_told_the_chat_again() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(Spec {
        backend: Backend::Claude,
        program: fake_tool::write(dir.path(), "claude", TERMING),
        model: Model::Recommended,
        language: "English",
        limits: Limits { turns: 2, ..quick() },
    });
    let brief = Arc::new(synthetic_brief("chat-1", &[("Lastenheft", "requirements spec")]));
    for (id, key) in [(1, "m1"), (2, "m2"), (3, "m3")] {
        session.send(Job::Batch {
            id,
            brief: Arc::clone(&brief),
            context: vec![line("earlier", "Earlier words")],
            items: vec![item(1, key)],
        });
        match next(&session) {
            Outcome::Batch {
                id: answered,
                answers: Ok(answers),
                terms,
                ..
            } => {
                assert_eq!(answered, id);
                assert_eq!(answers.len(), 1);
                assert_eq!(
                    terms,
                    [Term {
                        term: "Abnahme".to_owned(),
                        rendering: "sign-off".to_owned()
                    }]
                );
            }
            other => panic!("{other:?}"),
        }
    }
    let log = fs::read_to_string(dir.path().join("received.log")).unwrap();
    let asked: Vec<&str> = log.lines().filter(|line| !line.starts_with("start ")).collect();
    assert_eq!(asked.len(), 3, "{log}");
    assert!(
        asked[0].contains("<chat>") && asked[0].contains("Title: Synthetic topic"),
        "{log}"
    );
    assert!(asked[0].contains("Lastenheft = requirements spec") && asked[0].contains("Earlier words"));
    // the same process isn't told again
    assert!(!asked[1].contains("<chat>") && !asked[1].contains("<terms>") && !asked[1].contains("Earlier words"));
    // a new one is
    assert_eq!(log.matches("start ").count(), 2, "{log}");
    assert!(
        asked[2].contains("<chat>") && asked[2].contains("Earlier words"),
        "{log}"
    );
    drop(session);
}

fn said(n: usize, text: &str) -> Item {
    Item {
        text: text.to_owned(),
        ..item(n, &format!("key {n}"))
    }
}

fn translated(text: &str, source: &str) -> Answer {
    Answer::Translated {
        text: text.to_owned(),
        source: Some(source.to_owned()),
    }
}

fn same(source: &str) -> Answer {
    Answer::Same {
        source: Some(source.to_owned()),
    }
}

/// `answers` to `items` judged in a chat read in `reading`, nothing handed back before but `echoed`; the keys held.
fn judged_in(
    reading: &str,
    items: &[Item],
    answers: &[(usize, Answer)],
    echoed: &[&str],
) -> (HashMap<usize, Answer>, Vec<String>) {
    let all: HashMap<usize, Answer> = answers.iter().cloned().collect();
    let echoed = echoed.iter().map(|key| (*key).to_owned()).collect();
    let (kept, held) = judge_echoes(all.clone(), &all, items, reading, &echoed);
    (kept, held.into_iter().map(|item| item.key.clone()).collect())
}

/// `answers` to `items` judged in a chat read in German, nothing handed back before but `echoed`; the keys held.
fn judged(items: &[Item], answers: &[(usize, Answer)], echoed: &[&str]) -> (HashMap<usize, Answer>, Vec<String>) {
    judged_in("German", items, answers, echoed)
}

#[test]
fn a_batch_handed_back_as_written_is_asked_again() {
    // the failure a fast model shows: every message as written, with the language it was written in
    let items = [said(1, "Wie weit ist der Ausbau?"), said(2, "Ich frage morgen nach")];
    let (kept, mut held) = judged(
        &items,
        &[
            (1, translated(" Wie weit ist der Ausbau? ", "ru")),
            (2, translated("Ich frage morgen nach", "ru")),
        ],
        &[],
    );
    assert!(kept.is_empty(), "{kept:?}");
    held.sort();
    assert_eq!(held, ["key 1", "key 2"]);
}

#[test]
fn a_message_handed_back_twice_is_taken_as_written() {
    let items = [said(1, "Wie weit ist der Ausbau?")];
    let (kept, held) = judged(&items, &[(1, translated("Wie weit ist der Ausbau?", "ru"))], &["key 1"]);
    assert_eq!(kept, HashMap::from([(1, same("ru"))]));
    assert!(held.is_empty());
}

#[test]
fn an_answer_that_is_rightly_its_message_is_taken() {
    let items = [
        said(1, "OK"),
        said(2, "<x1>Kubernetes</x1>"),
        said(3, "42 / 17:30"),
        said(4, "Bis morgen dann"),
    ];
    let (kept, held) = judged(
        &items,
        &[
            // a single word, a name or a product: nothing else it could be
            (1, translated("OK", "en")),
            (2, translated("<x1>Kubernetes</x1>", "en")),
            // no words at all
            (3, translated("42 / 17:30", "en")),
            // the model named the chat's own language rather than answering `same`
            (4, translated("Bis morgen dann", "de")),
        ],
        &[],
    );
    assert!(held.is_empty(), "{held:?}");
    assert_eq!(
        kept,
        HashMap::from([(1, same("en")), (2, same("en")), (3, same("en")), (4, same("de"))])
    );
}

#[test]
fn a_message_left_as_written_beside_translated_ones_is_meant_so() {
    let items = [
        said(1, "Visual Studio Code"),
        said(2, "see you tomorrow"),
        said(3, "Anna Schmidt"),
    ];
    let (kept, held) = judged(
        &items,
        &[
            (1, translated("Visual Studio Code", "en")),
            (2, translated("bis morgen", "en")),
            (3, Answer::Same { source: None }),
        ],
        &[],
    );
    assert!(held.is_empty(), "{held:?}");
    assert_eq!(
        kept,
        HashMap::from([
            (1, same("en")),
            (2, translated("bis morgen", "en")),
            (3, Answer::Same { source: None })
        ])
    );
}

#[test]
fn words_count_only_words_with_letters_markers_aside() {
    assert_eq!(words("<x3>OK</x3> <x7/>"), 1);
    assert_eq!(words("12:30 — 5 €"), 0);
    assert_eq!(words("Guten <x2>Morgen</x2>!"), 2);
}

#[test]
fn a_batch_of_unspaced_sentences_handed_back_as_written_is_asked_again() {
    // Japanese, Chinese and Thai sentences hold no spaces: they aren't one word each
    let items = [
        said(1, "明日の会議は午後三時からです。"),
        said(2, "明天的会议下午三点开始。"),
        said(3, "พรุ่งนี้ประชุมบ่ายสามโมง"),
    ];
    let (kept, mut held) = judged_in(
        "English",
        &items,
        &[
            (1, translated("明日の会議は午後三時からです。", "ja")),
            (2, translated("明天的会议下午三点开始。", "zh")),
            (3, translated("พรุ่งนี้ประชุมบ่ายสามโมง", "th")),
        ],
        &[],
    );
    assert!(kept.is_empty(), "{kept:?}");
    held.sort();
    assert_eq!(held, ["key 1", "key 2", "key 3"]);
    // asked again and handed back again: taken as written
    let (kept, held) = judged_in(
        "English",
        &items[..1],
        &[(1, translated("明日の会議は午後三時からです。", "ja"))],
        &["key 1"],
    );
    assert_eq!(kept, HashMap::from([(1, same("ja"))]));
    assert!(held.is_empty());
}

#[test]
fn a_single_unspaced_word_handed_back_is_taken() {
    let items = [
        said(1, "会議"),
        said(2, "ありがとう"),
        said(3, "コンピューター"),
        said(4, "<x1>東京</x1>"),
    ];
    let (kept, held) = judged_in(
        "English",
        &items,
        &[
            (1, translated("会議", "ja")),
            (2, translated("ありがとう", "ja")),
            (3, translated("コンピューター", "ja")),
            (4, translated("<x1>東京</x1>", "ja")),
        ],
        &[],
    );
    assert!(held.is_empty(), "{held:?}");
    assert_eq!(
        kept,
        HashMap::from([(1, same("ja")), (2, same("ja")), (3, same("ja")), (4, same("ja"))])
    );
}

#[test]
fn letters_of_unspaced_scripts_count_a_word_per_run_of_them() {
    assert_eq!(words("会議"), 1);
    assert_eq!(words("コンピューター"), 1);
    assert_eq!(words("iPhone用"), 1);
    assert_eq!(words("明日の会議は午後三時からです。"), 2);
    assert_eq!(words("พรุ่งนี้ประชุมบ่ายสามโมง"), 3);
    assert_eq!(words("<x1>会議</x1> 三時"), 2);
    assert_eq!(words("15:00。"), 0);
    // Korean is written with spaces
    assert_eq!(words("내일 회의는 세 시에 시작합니다"), 5);
}
