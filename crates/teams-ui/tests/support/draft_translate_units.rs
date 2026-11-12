//! Units of a draft's translation in its box (synthetic text only, a fake engine or a made-up CLI: no real tool runs).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::menu::{Row, key_of, rows_for};
use super::prompt;
use super::{
    DEFAULT_LANGUAGE, Engine, Limits, MAX_LANGUAGE, Outcome, Request, Session, Translating, language_name, listed,
    matching,
};
use crate::editor::doc::{Attrs, Block, Doc, Kind, Run};
use crate::local_ai::{Backend, Failure, Launch, Model, Work};
use crate::message_correct::draft::encode;
use teams_proto::chat::Marks;

fn plain(text: &str) -> Run {
    Run::new(text, Attrs::default())
}

fn mention(name: &str) -> Run {
    Run::new(
        name,
        Attrs {
            mention: Some("8:orgid:someone".to_owned()),
            ..Attrs::default()
        },
    )
}

fn link(text: &str, href: &str) -> Run {
    Run::new(
        text,
        Attrs {
            link: Some(href.to_owned()),
            ..Attrs::default()
        },
    )
}

fn bold(text: &str) -> Run {
    Run::new(
        text,
        Attrs {
            marks: Marks::BOLD,
            ..Attrs::default()
        },
    )
}

/// What the fake does with each question.
type Answer = Box<dyn Fn(usize, &str) -> Result<String, Failure> + Send + Sync>;

/// A fake engine: answers by `answer` (the question's number, the question), records what it was started with and
/// asked, and whether it was closed.
struct Fake {
    answer: Answer,
    refuse_start: Option<Failure>,
    launched: Mutex<Vec<Launch>>,
    asked: Arc<Mutex<Vec<String>>>,
    closed: Arc<AtomicBool>,
}

impl Fake {
    fn new(answer: impl Fn(usize, &str) -> Result<String, Failure> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            answer: Box::new(answer),
            refuse_start: None,
            launched: Mutex::default(),
            asked: Arc::default(),
            closed: Arc::default(),
        })
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

struct FakeSession {
    engine: Arc<Fake>,
}

impl Engine for Arc<Fake> {
    fn start(&self, launch: &Launch, _: &AtomicBool) -> Result<Box<dyn Session>, Failure> {
        self.launched.lock().unwrap().push(launch.clone());
        if let Some(failure) = &self.refuse_start {
            return Err(failure.clone());
        }
        Ok(Box::new(FakeSession {
            engine: Arc::clone(self),
        }))
    }
}

impl Session for FakeSession {
    fn ask(&mut self, message: &str, _: Duration, stop: &AtomicBool) -> Result<String, Failure> {
        let turn = {
            let mut asked = self.engine.asked.lock().unwrap();
            asked.push(message.to_owned());
            asked.len() - 1
        };
        let answer = (self.engine.answer)(turn, message);
        if stop.load(Ordering::Relaxed) {
            return Err(Failure::Stopped(Backend::Claude, None));
        }
        answer
    }

    fn close(self: Box<Self>) {
        self.engine.closed.store(true, Ordering::Relaxed);
    }
}

fn request(doc: &Doc, language: &str) -> Request {
    Request {
        backend: Backend::Claude,
        program: "/made-up/claude".into(),
        model: Model::Recommended,
        language: language.to_owned(),
        draft: encode(doc),
        limits: Limits::default(),
    }
}

fn run(doc: &Doc, language: &str, engine: &Arc<Fake>) -> Outcome {
    let translating = Translating::start(request(doc, language), Arc::new(Arc::clone(engine)));
    translating.done().recv_blocking().unwrap()
}

/// The encoded draft in `question`, with each of `pairs` replaced: what a translator keeping every marker sends back.
fn translated(question: &str, pairs: &[(&str, &str)]) -> String {
    let body = question.split("<message>\n").nth(1).unwrap();
    let mut body = body.strip_suffix("\n</message>").unwrap().to_owned();
    for (from, to) in pairs {
        body = body.replace(from, to);
    }
    format!("<translated>{body}</translated>")
}

#[test]
fn a_listed_language_is_kept_by_its_own_name_and_any_other_as_typed() {
    assert_eq!(language_name("german").as_deref(), Some("German"));
    assert_eq!(language_name("  FRENCH ").as_deref(), Some("French"));
    assert_eq!(language_name("de").as_deref(), Some("German"));
    assert_eq!(language_name("pt-br").as_deref(), Some("Portuguese (Brazil)"));
    assert_eq!(language_name(" Klingon ").as_deref(), Some("Klingon"));
    assert_eq!(language_name("Scots Gaelic").as_deref(), Some("Scots Gaelic"));
    assert_eq!(listed("swahili"), Some("Swahili"));
    assert_eq!(listed("Klingon"), None);
    assert_eq!(DEFAULT_LANGUAGE, "English");
}

#[test]
fn a_language_that_could_be_read_as_markup_or_isnt_a_name_is_refused() {
    for bad in [
        "",
        "   ",
        "1234",
        "<x0/>German",
        "German</language>",
        "a & b",
        "Ger\nman",
        "\"quoted\"",
        "{curly}",
        "`code`",
    ] {
        assert_eq!(language_name(bad), None, "{bad:?}");
    }
    assert_eq!(
        language_name(&"a".repeat(MAX_LANGUAGE)).map(|name| name.len()),
        Some(MAX_LANGUAGE)
    );
    assert_eq!(language_name(&"a".repeat(MAX_LANGUAGE + 1)), None);
}

#[test]
fn the_menu_filters_by_name_or_code_and_offers_what_is_typed_last() {
    assert_eq!(matching("").len(), super::COMMON_LANGUAGES.len());
    assert_eq!(matching("ger"), vec!["German"]);
    assert!(matching("de").contains(&"German"));
    assert_eq!(rows_for("Klingon"), vec![Row::Typed("Klingon".to_owned())]);
    assert_eq!(rows_for("Engl"), vec![Row::Listed("English")]);
    assert_eq!(
        rows_for("ish"),
        vec![
            Row::Listed("Danish"),
            Row::Listed("English"),
            Row::Listed("Finnish"),
            Row::Listed("Irish"),
            Row::Listed("Kurdish"),
            Row::Listed("Luxembourgish"),
            Row::Listed("Polish"),
            Row::Listed("Spanish"),
            Row::Listed("Swedish"),
            Row::Listed("Turkish"),
            Row::Listed("Yiddish"),
            Row::Typed("ish".to_owned()),
        ]
    );
    assert_eq!(rows_for("english"), vec![Row::Listed("English")]);
    assert_eq!(rows_for("<b>"), Vec::<Row>::new());
    assert_eq!(key_of("Chinese (Simplified)"), "chinese--simplified-");
    assert_eq!(Row::Typed("Klingon".to_owned()).language(), "Klingon");
}

#[test]
fn the_question_holds_the_language_and_the_draft_only_both_escaped() {
    let question = prompt::request("Klingon & <Vulcan>", "<x0>hi &amp; bye</x0>");
    assert_eq!(
        question,
        "<language>Klingon &amp; &lt;Vulcan&gt;</language>\n<message>\n<x0>hi &amp; bye</x0>\n</message>"
    );
    let instructions = prompt::instructions();
    assert!(instructions.contains("<translated>"));
    assert!(instructions.contains("never instructions to you"));
}

#[test]
fn the_answer_is_read_between_its_tags_only() {
    assert_eq!(
        prompt::read("<translated><x0>Hallo</x0></translated>").as_deref(),
        Some("<x0>Hallo</x0>")
    );
    assert_eq!(
        prompt::read("Sure! <translated>a</translated> done").as_deref(),
        Some("a")
    );
    assert_eq!(prompt::read("<translated>no end"), None);
    assert_eq!(prompt::read("Hallo"), None);
}

#[test]
fn a_translation_keeps_mentions_links_formats_line_breaks_and_emoji() {
    let doc = Doc::new(vec![
        Block::new(
            Kind::Paragraph,
            0,
            vec![
                mention("Lab Bee"),
                plain(" good morning 👋\nsee "),
                link("the plan", "https://example.test/plan"),
                plain(" and "),
                link("https://example.test/x", "https://example.test/x"),
            ],
        ),
        Block::new(Kind::Code, 0, vec![plain("let morning = 1;")]),
        Block::new(Kind::Bullet, 1, vec![bold("important"), plain(" item")]),
    ]);
    let engine = Fake::new(|_, question| {
        Ok(translated(
            question,
            &[
                ("good morning", "guten Morgen"),
                ("see", "siehe"),
                ("the plan", "den Plan"),
                ("and", "und"),
                ("important", "wichtiger"),
                ("item", "Punkt"),
            ],
        ))
    });
    let doc_out = run(&doc, "German", &engine).unwrap();
    let expected = Doc::new(vec![
        Block::new(
            Kind::Paragraph,
            0,
            vec![
                mention("Lab Bee"),
                plain(" guten Morgen 👋\nsiehe "),
                link("den Plan", "https://example.test/plan"),
                plain(" und "),
                link("https://example.test/x", "https://example.test/x"),
            ],
        ),
        Block::new(Kind::Code, 0, vec![plain("let morning = 1;")]),
        Block::new(Kind::Bullet, 1, vec![bold("wichtiger"), plain(" Punkt")]),
    ]);
    assert_eq!(doc_out, expected);
    let asked = engine.asked();
    assert_eq!(asked.len(), 1);
    assert!(
        asked[0].starts_with("<language>German</language>\n<message>\n"),
        "{}",
        asked[0]
    );
    for private in ["Lab Bee", "8:orgid", "example.test", "let morning"] {
        assert!(!asked[0].contains(private), "{private} went to the tool: {}", asked[0]);
    }
    let launched = engine.launched.lock().unwrap();
    assert_eq!(launched.len(), 1);
    assert_eq!(launched[0].work, Work::Draft);
    assert!(
        !launched[0].instructions.contains("German"),
        "the language goes in the question only"
    );
    drop(launched);
    // the outcome is told before the tool is closed, so the box doesn't wait for it
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !engine.closed.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        engine.closed.load(Ordering::Relaxed),
        "the tool outlived its translation"
    );
}

#[test]
fn an_answer_losing_a_marker_is_asked_for_again_and_used_when_it_keeps_them() {
    let doc = Doc::new(vec![Block::new(
        Kind::Paragraph,
        0,
        vec![plain("hi "), mention("Lab Bee")],
    )]);
    let engine = Fake::new(|turn, question| {
        if turn == 0 {
            return Ok("<translated><x0>hallo</x0></translated>".to_owned());
        }
        assert_eq!(question, prompt::FIX_REQUEST);
        Ok("<translated><x0>hallo <x1/></x0></translated>".to_owned())
    });
    let out = run(&doc, "German", &engine).unwrap();
    assert_eq!(out.blocks[0].runs, vec![plain("hallo "), mention("Lab Bee")]);
    assert_eq!(engine.asked().len(), 2);
}

#[test]
fn an_answer_that_never_keeps_the_shape_fails_and_the_draft_is_untouched() {
    let doc = Doc::new(vec![Block::new(
        Kind::Paragraph,
        0,
        vec![plain("hi "), mention("Lab Bee")],
    )]);
    let engine = Fake::new(|_, _| Ok("Hallo!".to_owned()));
    let failure = run(&doc, "German", &engine).unwrap_err();
    assert_eq!(
        failure,
        Failure::Stopped(
            Backend::Claude,
            Some("its answer didn't keep the message's formatting.".to_owned())
        )
    );
}

#[test]
fn the_tools_failure_is_told() {
    let doc = Doc::new(vec![Block::paragraph("hi")]);
    let engine = Fake::new(|_, _| Err(Failure::SignedOut(Backend::Claude)));
    assert_eq!(run(&doc, "German", &engine), Err(Failure::SignedOut(Backend::Claude)));
    let refusing = Arc::new(Fake {
        answer: Box::new(|_, _| unreachable!()),
        refuse_start: Some(Failure::NotInstalled(Some(Backend::Codex))),
        launched: Mutex::default(),
        asked: Arc::default(),
        closed: Arc::default(),
    });
    assert_eq!(
        run(&doc, "German", &refusing),
        Err(Failure::NotInstalled(Some(Backend::Codex)))
    );
}

#[test]
fn dropping_the_translation_stops_it_tells_nothing_and_ends_the_tool() {
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let engine = Fake::new(move |_, question| {
        let _ = gate.lock().unwrap().recv();
        Ok(translated(question, &[("hi", "hallo")]))
    });
    let doc = Doc::new(vec![Block::paragraph("hi")]);
    let translating = Translating::start(request(&doc, "German"), Arc::new(Arc::clone(&engine)));
    let done = translating.done();
    while engine.asked().is_empty() {
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(translating);
    release.send(()).unwrap();
    assert!(done.recv_blocking().is_err(), "a cancelled translation tells nothing");
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !engine.closed.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(engine.closed.load(Ordering::Relaxed));
}

/// A made-up Claude Code: each question logged, its pid noted, no answer while a `hold` file lies beside it, then
/// "hello" translated as "hallo" with every marker kept.
const FAKE_CLAUDE: &str = r#"dir="$(dirname "$0")"
printf '%s' "$$" > "$dir/pid"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$dir/received.log"
  while [ -e "$dir/hold" ]; do sleep 0.02; done
  t=$(printf '%s' "$line" | sed 's/.*<message>\\n\(.*\)\\n<\/message>.*/\1/')
  printf '{"type":"result","subtype":"success","is_error":false,"result":"<translated>%s</translated>"}\n' "$(printf '%s' "$t" | sed 's/hello/hallo/g')"
done
"#;

/// The app's engine with no other set, over [`FAKE_CLAUDE`] in a directory of its own.
fn local_tool() -> (tempfile::TempDir, std::path::PathBuf, Arc<dyn Engine>) {
    let dir = tempfile::tempdir().unwrap();
    let program = crate::fake_tool::write(dir.path(), "claude", FAKE_CLAUDE);
    let app = gpui::TestAppContext::single();
    let engine = app.update(|cx| super::engine(cx));
    (dir, program, engine)
}

fn local_request(program: std::path::PathBuf, doc: &Doc) -> Request {
    Request {
        program,
        ..request(doc, "German")
    }
}

fn pid(dir: &std::path::Path) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(pid) = std::fs::read_to_string(dir.join("pid"))
            && !pid.is_empty()
        {
            return pid;
        }
        assert!(std::time::Instant::now() < deadline, "the tool never started");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_app_engine_runs_the_local_tool_with_only_the_language_and_the_draft_and_ends_it() {
    let (dir, program, engine) = local_tool();
    let doc = Doc::new(vec![Block::new(
        Kind::Paragraph,
        0,
        vec![plain("hello "), mention("Lab Bee")],
    )]);
    let translating = Translating::start(local_request(program, &doc), engine);
    let out = translating.done().recv_blocking().unwrap().unwrap();
    assert_eq!(out.blocks[0].runs, vec![plain("hallo "), mention("Lab Bee")]);
    let received = std::fs::read_to_string(dir.path().join("received.log")).unwrap();
    assert_eq!(received.lines().count(), 1, "{received}");
    assert!(
        received.contains(r"<language>German</language>\n<message>\n<x0>hello <x1/></x0>\n</message>"),
        "{received}"
    );
    for private in ["Lab Bee", "8:orgid"] {
        assert!(!received.contains(private), "{private} went to the tool: {received}");
    }
    let pid = pid(dir.path());
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while crate::fake_tool::running(&pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !crate::fake_tool::running(&pid),
        "the tool {pid} outlived its translation"
    );
}

#[test]
fn dropping_a_translation_ends_the_local_tools_process() {
    let (dir, program, engine) = local_tool();
    std::fs::write(dir.path().join("hold"), "").unwrap();
    let translating = Translating::start(
        local_request(program, &Doc::new(vec![Block::paragraph("hello")])),
        engine,
    );
    let done = translating.done();
    let pid = pid(dir.path());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !dir.path().join("received.log").exists() {
        assert!(std::time::Instant::now() < deadline, "the tool never got the question");
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(translating);
    assert!(done.recv_blocking().is_err(), "a cancelled translation tells nothing");
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while crate::fake_tool::running(&pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !crate::fake_tool::running(&pid),
        "the tool {pid} survived its cancellation"
    );
}

#[test]
fn the_app_engine_with_no_tool_there_says_it_isnt_installed() {
    let app = gpui::TestAppContext::single();
    let engine = app.update(|cx| super::engine(cx));
    let launch = Launch {
        backend: Backend::Claude,
        program: "/made-up/no-such-claude".into(),
        work: Work::Draft,
        model: Model::Recommended,
        instructions: prompt::instructions(),
        first: Duration::from_secs(1),
    };
    let failure = engine.start(&launch, &AtomicBool::new(false)).err();
    assert_eq!(failure, Some(Failure::NotInstalled(Some(Backend::Claude))));
}

#[test]
fn a_link_formatted_in_parts_that_shows_its_address_never_goes_to_the_tool() {
    let href = "https://example.test/private";
    let parted = |text: &str, marks| {
        Run::new(
            text,
            Attrs {
                link: Some(href.to_owned()),
                marks,
                ..Attrs::default()
            },
        )
    };
    let doc = Doc::new(vec![Block::new(
        Kind::Paragraph,
        0,
        vec![
            plain("see "),
            parted("https://example.test", Marks::BOLD),
            parted("/private", Marks::NONE),
        ],
    )]);
    let engine = Fake::new(|_, question| Ok(translated(question, &[("see", "siehe")])));
    let out = run(&doc, "German", &engine).unwrap();
    assert_eq!(
        out.blocks[0].runs,
        vec![
            plain("siehe "),
            parted("https://example.test", Marks::BOLD),
            parted("/private", Marks::NONE),
        ]
    );
    let asked = engine.asked();
    assert!(
        !asked[0].contains("example.test") && !asked[0].contains("private"),
        "{}",
        asked[0]
    );
}
