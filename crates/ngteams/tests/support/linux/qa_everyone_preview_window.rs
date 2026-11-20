//! A mention of everyone in the reader's words, as a user sees it in the app's window in the UI sandbox
//! (`harness = false`: GPUI runs on the process's main thread). It is the QA test of task 715 ("I see Everyone in
//! Russian in the chats list").
//!
//! The parent runs the mock (`teams-mock`) with made-up group chats and runs a's app as a child in the sandbox, with
//! its store and settings in one directory, so a later run is a restart of the same app. Other people's clients post
//! as Teams writes a mention of everyone in the sender's language: a mention span whose text is "Все", "Alle" or
//! "Todos", its `properties.mentions` entry `mentionType` `everyone`. The parent presses the sandbox's keys, reads the
//! views from the child, reads the notifications on the sandbox's bus, and reads the chat list from screenshots (OCR,
//! when `tesseract` is there).
//!
//! - **The chat list**: b's message, a's own message from a's other client ("You: …"), a person's and a channel's
//!   mention (their own names), a message of several paragraphs, line breaks and a list (one line), a chat read in
//!   English. Teams Light and Dark, a narrow window.
//! - **Desktop notifications** of those messages, in the translated chat too.
//! - **The message view** still shows "Everyone".
//! - **A reply's quote** in the box, in a chat as written and in the translated chat.
//! - **Search** finds the messages by "Everyone", and the snippets show it.
//! - **An old message**: its index row as an older build wrote it ("Все …"), the store put back to schema 28, then the
//!   app started again: search finds it by "Everyone".
//!
//! Every name, id and text is made up. Screenshots are kept under `target/ui-shots/qa715-*.png`; `QA715_EVIDENCE`
//! names a directory that gets a copy of each, the OCR text and the results. Without weston 15, niri or
//! `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, future, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
#[path = "../window.rs"]
mod support;
use serde_json::{Value, json};
use support::{Alias, display_name};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_mock::{
    AB_CHAT, AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE, PRESENCE_BASE,
    PROVISIONING_BASE, TEAM_CHANNEL, TrouterMock, USER_PROFILE_BASE,
};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::chat_view::{Body, ChatView, Item};
use teams_ui::compose::Mode;
use teams_ui::rich_text::Block;
use teams_ui::shell::{Main, Shell};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::VirtualInput;
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

const CHILD_ENV: &str = "QA715_CHILD";
const ORIGIN_ENV: &str = "QA715_ORIGIN";
const TROUTER_ENV: &str = "QA715_TROUTER";
const TOKEN_ENV: &str = "QA715_TOKEN";
const DIR_ENV: &str = "QA715_DIR";
const TOOLS_ENV: &str = "QA715_TOOLS";
const EVIDENCE_ENV: &str = "QA715_EVIDENCE";
/// The file in the app's directory the parent asks the child through (`<n> <verb>`; the answer is `re <n> …`).
const ASK: &str = "qa715-ask";
/// The file in the app's directory the child reads the theme to show from (`light` or `dark`).
const THEME_FILE: &str = "theme";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(40);
const POLL: Duration = Duration::from_millis(100);
const SETTLE: Duration = Duration::from_millis(900);
/// The time between a key and the next, so the app sees each.
const STEP: Duration = Duration::from_millis(120);
/// niri's layout: each window takes the whole screen until the test sizes it, and no hot corner.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
const SCREEN: (u32, u32) = (1280, 800);
/// The chat list's column on the screen, rail left out: what the OCR reads.
const LIST_COLUMN: (u32, u32) = (64, 420);

const MENTION_TYPE: &str = "http://schema.skype.com/Mention";

/// The made-up group chats: their topics.
const DESIGN: &str = "Design crew";
const BUILD: &str = "Build room";
const REVIEW: &str = "Review ring";
const NOTES: &str = "Notes desk";
const LONG: &str = "Long notes";
const GERMAN: &str = "Berlin office";

/// What each chat's row must preview once its newest message came.
const DESIGN_ROW: &str = "Everyone Setup is ready, try it";
const BUILD_ROW: &str = "You: Everyone Build is green";
const REVIEW_ROW: &str = "Lab C please review the draft";
const NOTES_ROW: &str = "General has the notes";
const LONG_ROW: &str = "Everyone First line second line one two the end";
const GERMAN_ROW: &str = "Everyone The setup is ready.";
/// The old message's text as an older build indexed it: the sender's label.
const OLD_INDEXED: &str = "Все Quarterly numbers are in";
/// The labels the senders' clients wrote, in their languages: none may show anywhere.
const FOREIGN_LABELS: [&str; 3] = ["Все", "Alle", "Todos"];

/// The made-up translator's dictionary, German to English.
const DICTIONARY: [(&str, &str); 2] = [
    ("Das Setup ist fertig.", "The setup is ready."),
    ("Der Bus kommt um acht.", "The bus comes at eight."),
];

/// A made-up Claude Code translating a chat's batches (`__STATE__` its state directory), as the translate QA's: each
/// message in English from `dictionary.sed`, named German when the dictionary changed it and English when not.
const FAKE_CLAUDE: &str = r#"#!/bin/bash
state="__STATE__"
case "$1" in
  --help)
    printf '%s\n' "Usage: claude [options]" "  --model <model>  Model for the current session: an alias such as 'sonnet' or 'opus'"
    exit 0;;
esac
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$state/received.log"
  echo '{"type":"system","subtype":"init"}'
  body=""
  case "$line" in
    *'<translate>'*)
      inner=$(printf '%s' "$line" | sed 's/.*<translate>\\n//; s/\\n<\/translate>.*//')
      while IFS= read -r m; do
        [ -z "$m" ] && continue
        t=$(printf '%s' "$m" | sed -f "$state/dictionary.sed")
        if [ "$t" != "$m" ]; then lang=de; else lang=en; fi
        t=$(printf '%s' "$t" | sed 's/^\(<m id=\\"[0-9]*\\"\) from=\\"[^\\]*\\"\( re=\\"[^\\]*\\"\)\{0,1\}>/\1 lang=\\"'"$lang"'\\">/')
        body="$body$t\\n"
      done < <(printf '%s\n' "$inner" | sed 's/\\n/\n/g')
      ;;
  esac
  printf '{"type":"result","subtype":"success","is_error":false,"result":"%s"}\n' "$body" | tee -a "$state/answers.log"
done
"#;

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_everyone_preview_window: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Notes a step on stderr with the time since the process started.
fn step(what: &str) {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    eprintln!("[{:>7.2}s] {what}", start.elapsed().as_secs_f64());
}

/// What the walk saw wrong that does not stop it: each is said at the end, and the test fails.
static FINDINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// What the walk checked and found right, for the results file.
static PASSED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn note(finding: String) {
    step(&format!("FINDING: {finding}"));
    FINDINGS.lock().unwrap_or_else(PoisonError::into_inner).push(finding);
}

/// Notes `finding` unless `ok`; else counts `what` as passed.
fn check(what: &str, ok: bool, finding: impl FnOnce() -> String) {
    if ok {
        step(&format!("pass: {what}"));
        PASSED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(what.to_owned());
    } else {
        note(format!("{what}: {}", finding()));
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn evidence_dir() -> Option<PathBuf> {
    std::env::var_os(EVIDENCE_ENV).map(PathBuf::from)
}

/// Where the screenshots are kept: `target/ui-shots/`, never committed.
fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Whether `text` shows a label of a sender's language.
fn foreign(text: &str) -> bool {
    FOREIGN_LABELS.iter().any(|label| text.contains(label))
}

// --- the parent ---

fn parent() -> SandboxResult {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams everyone preview", &options)? else {
        return Ok(());
    };
    let tool = FakeTool::install(&sandbox)?;
    let service = Service::start(&runtime)?;
    let dir = tempfile::tempdir()?;
    if let Err(error) = runs(&sandbox, &runtime, &service, &tool, dir.path()) {
        return Err(format!("{error}{}", sandbox.logs()).into());
    }
    let passed = std::mem::take(&mut *PASSED.lock().unwrap_or_else(PoisonError::into_inner));
    let findings = std::mem::take(&mut *FINDINGS.lock().unwrap_or_else(PoisonError::into_inner));
    let mut summary = String::new();
    for what in &passed {
        let _ = writeln!(summary, "pass {what}");
    }
    for what in &findings {
        let _ = writeln!(summary, "FAIL {what}");
    }
    let _ = writeln!(summary, "{} passed, {} failed", passed.len(), findings.len());
    say(&summary);
    if let Some(dir) = evidence_dir() {
        fs::write(dir.join("results.txt"), &summary)?;
    }
    if !findings.is_empty() {
        return Err(format!("the walk ended with findings:\n- {}", findings.join("\n- ")).into());
    }
    say("ngteams everyone preview: ok");
    Ok(())
}

/// The walk, the store put back to how an older build left its index, and a start of the upgraded app on it.
fn runs(
    sandbox: &Sandbox,
    runtime: &tokio::runtime::Runtime,
    service: &Service,
    tool: &FakeTool,
    dir: &Path,
) -> SandboxResult {
    step("--- run walk ---");
    let mut run = AppRun::launch(sandbox, &service.env(), dir, tool)?;
    let outcome = Walk {
        run: &mut run,
        sandbox,
        runtime,
        service,
    }
    .all();
    if let Err(error) = outcome {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit()?;

    step("--- the index as an older build wrote it ---");
    old_index(dir, service)?;

    step("--- run upgrade ---");
    let mut run = AppRun::launch(sandbox, &service.env(), dir, tool)?;
    let outcome = Walk {
        run: &mut run,
        sandbox,
        runtime,
        service,
    }
    .after_upgrade();
    if let Err(error) = outcome {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit()
}

/// Gives the old message's index row the text an older build gave it (the sender's label), and puts the store back to
/// schema 28, as that build left it.
fn old_index(dir: &Path, service: &Service) -> SandboxResult {
    let store = support::store_file(dir, Alias::A);
    let db = rusqlite::Connection::open(&store)?;
    let (row, text): (i64, String) = db.query_row(
        "SELECT e.id, s.text FROM search_entries e JOIN message_search s ON s.rowid = e.id \
         WHERE e.conversation_id = ?1 AND e.message_id = ?2",
        (&service.chats.review, &service.old),
        |found| Ok((found.get(0)?, found.get(1)?)),
    )?;
    check(
        "this build indexes a new message in the reader's words",
        text.contains("Everyone") && !foreign(&text),
        || format!("the old message is indexed as {text:?}"),
    );
    db.execute(
        "UPDATE message_search SET text = ?1 WHERE rowid = ?2",
        (OLD_INDEXED, row),
    )?;
    db.execute_batch("PRAGMA user_version = 28;")?;
    let found: i64 = db.query_row(
        "SELECT count(*) FROM message_search WHERE message_search MATCH 'Everyone' AND rowid = ?1",
        [row],
        |found| found.get(0),
    )?;
    if found != 0 {
        return Err("the old index row is still found by Everyone".into());
    }
    Ok(())
}

// --- the service ---

/// The made-up group chats' ids.
#[derive(Clone, Debug, Default)]
struct Chats {
    design: String,
    build: String,
    review: String,
    notes: String,
    long: String,
    german: String,
}

struct Service {
    mock: ChatServiceMock,
    trouter: TrouterMock,
    chats: Chats,
    /// b's old message in the review chat, which mentions everyone.
    old: String,
    /// The German chat's first message, which the translator knows.
    bus: String,
}

impl Service {
    /// The mock with a, b and c's made-up group chats, each with a first message of c's; b's old message, which
    /// mentions everyone, in the review chat; a's own message from a's other client in the build chat.
    fn start(runtime: &tokio::runtime::Runtime) -> SandboxResult<Self> {
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        let group = |topic: &str| -> SandboxResult<String> {
            let id = mock
                .add_group(topic, &[LAB_C, LAB_A, LAB_B])
                .ok_or_else(|| format!("no group {topic}"))?;
            mock.post_as(LAB_C, &id, "<p>Hello from c</p>")
                .ok_or_else(|| format!("c's post to {topic} refused"))?;
            Ok(id)
        };
        let chats = Chats {
            design: group(DESIGN)?,
            build: group(BUILD)?,
            review: group(REVIEW)?,
            notes: group(NOTES)?,
            long: group(LONG)?,
            german: group(GERMAN)?,
        };
        let mut service = Self {
            mock,
            trouter,
            chats,
            old: String::new(),
            bus: String::new(),
        };
        service.bus = service.post(
            runtime,
            LAB_C,
            &service.chats.german.clone(),
            "<p>Der Bus kommt um acht.</p>",
            &[],
        )?;
        let review = service.chats.review.clone();
        service.old = service.post(
            runtime,
            LAB_B,
            &review,
            &paragraphs(&mention_span("Все"), &["Quarterly numbers are in"]),
            &[everyone(&review, "Все")],
        )?;
        // a's own message from a's other client, while a's app is closed: the mock tells no sender of its own post,
        // so the app reads it at its start
        let build = service.chats.build.clone();
        service.post(
            runtime,
            LAB_A,
            &build,
            &paragraphs(&mention_span("Alle"), &["Build is green"]),
            &[everyone(&build, "Alle")],
        )?;
        let _ = service.mock.take_events(LAB_A);
        Ok(service)
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            (ORIGIN_ENV, self.mock.origin()),
            (TROUTER_ENV, self.trouter.origin().to_owned()),
            (TOKEN_ENV, self.mock.token(LAB_A).unwrap_or_default()),
        ]
    }

    /// `from`'s client posts `html` to `chat` with `mentions` (their `properties.mentions` entries), as Teams posts a
    /// message; its id.
    fn post(
        &self,
        runtime: &tokio::runtime::Runtime,
        from: &str,
        chat: &str,
        html: &str,
        mentions: &[Value],
    ) -> SandboxResult<String> {
        let url = format!(
            "{}/api/chatsvc/amer/v1/users/ME/conversations/{chat}/messages",
            self.mock.origin()
        );
        let token = self.mock.token(from).ok_or("no token")?;
        let body = json!({
            "messagetype": "RichText/Html",
            "contenttype": "Text",
            "content": html,
            "clientmessageid": now_ms().to_string(),
            "imdisplayname": if from == LAB_A { "Lab A" } else if from == LAB_B { "Lab B" } else { "Lab C" },
            "properties": {"mentions": Value::Array(mentions.to_vec()).to_string(), "files": "[]"},
        });
        let (status, location) = runtime.block_on(async {
            let answer = reqwest::Client::new()
                .post(url)
                .bearer_auth(token)
                .header("X-MS-Migration", "True")
                .header("BehaviorOverride", "redirectAs404")
                .header("content-type", "application/json")
                .body(body.to_string())
                .send()
                .await?;
            let location = answer
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            Ok::<_, reqwest::Error>((answer.status().as_u16(), location))
        })?;
        if !(200..300).contains(&status) {
            return Err(format!("the post to {chat} answered {status}").into());
        }
        location
            .and_then(|location| location.rsplit('/').next().map(str::to_owned))
            .ok_or_else(|| "the post named no message".into())
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis())
}

/// A mention span as Teams writes it, item 0, showing `label`.
fn mention_span(label: &str) -> String {
    format!(r#"<span itemscope="" itemtype="{MENTION_TYPE}" itemid="0">{label}</span>"#)
}

/// The mention's paragraph, an empty one under it as Teams leaves one after a mention of everyone, then `rest`, a
/// paragraph each.
fn paragraphs(mention: &str, rest: &[&str]) -> String {
    let mut html = format!("<p>{mention}</p><p>&nbsp;</p>");
    for paragraph in rest {
        let _ = write!(html, "<p>{paragraph}</p>");
    }
    html
}

/// The `properties.mentions` entry of a mention of everyone in `chat`, labelled `label`.
fn everyone(chat: &str, label: &str) -> Value {
    json!({"@type": MENTION_TYPE, "itemid": "0", "mri": chat, "mentionType": "everyone", "displayName": label})
}

// --- the made-up tool ---

/// The made-up translator as the parent sees it.
struct FakeTool {
    bin: PathBuf,
    home: PathBuf,
    /// The child's `PATH`: `bin`, then the sandbox's own, without any directory holding a real `claude` or `codex`.
    path: String,
}

impl FakeTool {
    fn install(sandbox: &Sandbox) -> SandboxResult<Self> {
        use std::os::unix::fs::PermissionsExt as _;
        let (bin, state, home, commands) = (
            sandbox.path("ai-bin"),
            sandbox.path("ai-state"),
            sandbox.path("ai-home"),
            sandbox.path("ai-commands"),
        );
        for dir in [&bin, &state, &home, &commands] {
            fs::create_dir_all(dir)?;
        }
        let inherited = sandbox
            .client_env()
            .into_iter()
            .find(|(name, _)| *name == "PATH")
            .map(|(_, path)| path)
            .unwrap_or_default();
        let kept: Vec<PathBuf> = std::env::split_paths(&inherited)
            .filter(|dir| !["claude", "codex"].iter().any(|tool| dir.join(tool).exists()))
            .collect();
        for command in ["sed", "tee"] {
            let found = std::env::split_paths(&inherited)
                .map(|dir| dir.join(command))
                .find(|candidate| candidate.is_file())
                .ok_or_else(|| format!("no {command} on the PATH"))?;
            std::os::unix::fs::symlink(found, commands.join(command))?;
        }
        let path = std::env::join_paths([bin.clone(), commands].into_iter().chain(kept))?
            .into_string()
            .map_err(|_| "a PATH that isn't UTF-8")?;
        let dictionary: String = DICTIONARY
            .iter()
            .map(|(german, english)| ["s/", german, "/", english, "/g\n"].concat())
            .collect();
        fs::write(state.join("dictionary.sed"), dictionary)?;
        let program = bin.join("claude");
        fs::write(&program, FAKE_CLAUDE.replace("__STATE__", &state.display().to_string()))?;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755))?;
        Ok(Self { bin, home, path })
    }
}

// --- the app in the sandbox ---

/// The app running in the sandbox: its process, its directory and the user's keys.
struct AppRun {
    running: Running,
    dir: PathBuf,
    input: VirtualInput,
    asked: u32,
}

impl AppRun {
    /// Starts the app with `env` and its store and settings in `dir`; once its window is drawn.
    fn launch(sandbox: &Sandbox, env: &[(&'static str, String)], dir: &Path, tool: &FakeTool) -> SandboxResult<Self> {
        let dir_text = dir.to_string_lossy().into_owned();
        let bin = tool.bin.to_string_lossy().into_owned();
        let home = tool.home.to_string_lossy().into_owned();
        let mut vars: Vec<(&str, &str)> = vec![
            (CHILD_ENV, "app"),
            (DIR_ENV, &dir_text),
            (TOOLS_ENV, &bin),
            ("PATH", &tool.path),
            ("CLAUDE_CONFIG_DIR", &home),
            ("CODEX_HOME", &home),
        ];
        vars.extend(env.iter().map(|(name, value)| (*name, value.as_str())));
        let child = sandbox.spawn(&std::env::current_exe()?, &[], &vars)?;
        let running = Running::new(child)?;
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| format!("no ready line: {error:?}"))?;
        let mut run = Self {
            running,
            dir: dir.to_owned(),
            input: sandbox.input()?,
            asked: 0,
        };
        run.park()?;
        Ok(run)
    }

    /// Asks the child `verb`; its answer.
    fn question(&mut self, verb: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = self.dir.join(format!("{ASK}.staged"));
        fs::write(&staged, format!("{} {verb}", self.asked))?;
        fs::rename(&staged, self.dir.join(ASK))?;
        let prefix = format!("re {} ", self.asked);
        let line = self
            .running
            .line_where(WAIT, |line| line.starts_with(&prefix))
            .map_err(|error| format!("no answer to {verb}: {error:?}"))?;
        Ok(line[prefix.len()..].to_owned())
    }

    fn ok(&mut self, verb: &str) -> SandboxResult {
        let answer = self.question(verb)?;
        if answer == "ok" {
            Ok(())
        } else {
            Err(format!("{verb}: {answer}").into())
        }
    }

    fn json(&mut self, verb: &str) -> SandboxResult<Value> {
        let answer = self.question(verb)?;
        serde_json::from_str(&answer).map_err(|error| format!("{verb} {answer}: {error}").into())
    }

    /// Waits up to [`WAIT`] for an answer to `verb` that `wanted` takes, named `what` in the error.
    fn until(&mut self, verb: &str, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let start = Instant::now();
        loop {
            let state = self.json(verb)?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {state}").into());
            }
            thread::sleep(POLL);
        }
    }

    /// Shows the app in Teams Light or Dark (`dark`), as the user picks it in Settings; once it is applied.
    fn theme(&mut self, dark: bool) -> SandboxResult {
        let key = if dark { "dark" } else { "light" };
        fs::write(self.dir.join(THEME_FILE), key)?;
        let applied = format!("theme {key}");
        self.running
            .line_where(WAIT, |line| line == applied)
            .map_err(|error| format!("{applied} never applied: {error:?}"))?;
        Ok(())
    }

    fn park(&mut self) -> SandboxResult {
        self.input.move_to(640, 12)
    }

    fn press(&mut self, spec: &str) -> SandboxResult {
        let (modifiers, key) =
            ui_sandbox::input::parse_key(spec, self.input.keymap()).ok_or_else(|| format!("no key {spec}"))?;
        self.input.key_with(&modifiers, key)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn type_text(&mut self, text: &str) -> SandboxResult {
        self.input.type_text(text)?;
        thread::sleep(STEP);
        Ok(())
    }

    /// A screenshot as `qa715-<name>.png`, the pointer at rest, under `target/ui-shots/` and the evidence dir.
    fn shot(&mut self, sandbox: &Sandbox, name: &str) -> SandboxResult<PathBuf> {
        self.park()?;
        thread::sleep(SETTLE);
        let file = format!("qa715-{name}.png");
        let png = sandbox.screenshot(&file, false)?;
        let kept = shots_dir()?.join(&file);
        fs::copy(&png, &kept)?;
        if let Some(dir) = evidence_dir() {
            let dir = dir.join("shots");
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(&file))?;
        }
        Ok(kept)
    }

    /// The error, with the last screen and the app's stderr.
    fn fail(&mut self, sandbox: &Sandbox, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
        let shot = self
            .shot(sandbox, "failed-run")
            .map_or_else(|_| String::new(), |path| path.display().to_string());
        self.running.fail(&format!("{error} (last screen: {shot})"))
    }

    /// Quits the app; once it ended.
    fn quit(mut self) -> SandboxResult {
        self.asked += 1;
        let staged = self.dir.join(format!("{ASK}.staged"));
        fs::write(&staged, format!("{} quit", self.asked))?;
        fs::rename(&staged, self.dir.join(ASK))?;
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("the app exited {status}: {stderr}").into());
        }
        Ok(())
    }
}

// --- the walk ---

struct Walk<'a> {
    run: &'a mut AppRun,
    sandbox: &'a Sandbox,
    runtime: &'a tokio::runtime::Runtime,
    service: &'a Service,
}

/// One row of the chat list as the child reads it.
fn row<'v>(rows: &'v Value, chat: &str) -> Option<&'v Value> {
    rows.as_array()?.iter().find(|row| row["id"] == chat)
}

fn preview_of(rows: &Value, chat: &str) -> String {
    row(rows, chat)
        .and_then(|row| row["preview"].as_str())
        .unwrap_or_default()
        .to_owned()
}

impl Walk<'_> {
    fn all(&mut self) -> SandboxResult {
        self.translated_chat()?;
        self.run.ok(&format!("open {AB_CHAT}"))?;
        thread::sleep(SETTLE);
        self.post_the_messages()?;
        self.chat_list()?;
        self.notifications()?;
        self.looks()?;
        self.message_view()?;
        self.reply_quote(&self.service.chats.design.clone(), DESIGN_ROW, "reply-design")?;
        self.reply_quote(&self.service.chats.german.clone(), GERMAN_ROW, "reply-german")?;
        let wanted = [
            (DESIGN, "Setup is ready"),
            (BUILD, "Build is green"),
            (LONG, "First line"),
            (GERMAN, "setup is ready"),
            (REVIEW, "Quarterly"),
        ];
        self.search("Everyone", &wanted, "search-everyone")
    }

    /// The German chat read in English, as the header's picker sets it, before b writes there.
    fn translated_chat(&mut self) -> SandboxResult {
        let german = self.service.chats.german.clone();
        self.run.ok(&format!("open {german}"))?;
        self.run.until("chat", "the German chat open", |state| {
            state["conversation"] == german.as_str()
        })?;
        self.run.ok("language en")?;
        let bus = self.service.bus.clone();
        self.run.until("chat", "the bus message in English", |state| {
            item_text(state, &bus).is_some_and(|text| text.contains("The bus comes at eight."))
        })?;
        Ok(())
    }

    /// The other people's clients post while a's app shows the a–b chat.
    fn post_the_messages(&mut self) -> SandboxResult {
        let chats = self.service.chats.clone();
        let post = |from: &str, chat: &str, html: String, mentions: Vec<Value>| {
            self.service
                .post(self.runtime, from, chat, &html, &mentions)
                .map(|_| ())
        };
        post(
            LAB_B,
            &chats.design,
            paragraphs(&mention_span("Все"), &["Setup is ready, try it"]),
            vec![everyone(&chats.design, "Все")],
        )?;
        post(
            LAB_B,
            &chats.review,
            format!("<p>{} please review the draft</p>", mention_span("Lab C")),
            vec![
                json!({"@type": MENTION_TYPE, "itemid": "0", "mri": LAB_C, "mentionType": "person", "displayName": "Lab C"}),
            ],
        )?;
        post(
            LAB_B,
            &chats.notes,
            format!("<p>{} has the notes</p>", mention_span("General")),
            vec![
                json!({"@type": MENTION_TYPE, "itemid": "0", "mri": TEAM_CHANNEL, "mentionType": "channel", "displayName": "General"}),
            ],
        )?;
        post(
            LAB_B,
            &chats.long,
            format!(
                "<p>{}</p><p>First line<br>second line</p><ul><li>one</li><li>two</li></ul><p>the end</p>",
                mention_span("Todos")
            ),
            vec![everyone(&chats.long, "Todos")],
        )?;
        post(
            LAB_B,
            &chats.german,
            paragraphs(&mention_span("Alle"), &["Das Setup ist fertig."]),
            vec![everyone(&chats.german, "Alle")],
        )
    }

    /// Each chat's row previews its newest message on one line, a mention of everyone as "Everyone".
    fn chat_list(&mut self) -> SandboxResult {
        let chats = self.service.chats.clone();
        let wanted = [
            ("b's message, Все", chats.design.clone(), DESIGN_ROW),
            ("a's own message, Alle", chats.build.clone(), BUILD_ROW),
            ("a person's mention", chats.review.clone(), REVIEW_ROW),
            ("a channel's mention", chats.notes.clone(), NOTES_ROW),
            ("paragraphs, a line break, a list", chats.long.clone(), LONG_ROW),
            ("a chat read in English", chats.german.clone(), GERMAN_ROW),
        ];
        let rows = self
            .run
            .until("rows", "every row previews its newest message as wanted", |rows| {
                wanted
                    .iter()
                    .all(|(_, chat, expected)| preview_of(rows, chat) == *expected)
            })
            .or_else(|_| self.run.json("rows"))?;
        save_text("rows.json", &rows.to_string())?;
        for (what, chat, expected) in &wanted {
            let preview = preview_of(&rows, chat);
            check(
                &format!("the row of {what} reads {expected:?}"),
                preview == *expected,
                || format!("it reads {preview:?}"),
            );
        }
        let all: Vec<String> = rows
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| row["preview"].as_str().map(str::to_owned))
            .collect();
        check(
            "no row shows a sender's own label",
            !all.iter().any(|text| foreign(text)),
            || format!("rows: {all:?}"),
        );
        Ok(())
    }

    /// The desktop notifications of the messages others wrote say "Everyone"; a's own message notifies nothing.
    fn notifications(&mut self) -> SandboxResult {
        let wanted = [DESIGN_ROW, REVIEW_ROW, NOTES_ROW, LONG_ROW, GERMAN_ROW];
        let bus = self.sandbox.bus();
        let all_came = wait_until(WAIT, || {
            let bodies: Vec<String> = bus.notifications().into_iter().map(|shown| shown.body).collect();
            wanted
                .iter()
                .all(|body| bodies.iter().any(|shown| shown == body))
                .then_some(())
        });
        let notifications = bus.notifications();
        let listing: Vec<String> = notifications
            .iter()
            .map(|shown| format!("{} | {}", shown.summary, shown.body))
            .collect();
        save_text("notifications.txt", &listing.join("\n"))?;
        for body in wanted {
            check(
                &format!("a notification says {body:?}"),
                notifications.iter().any(|shown| shown.body == body),
                || format!("notifications: {listing:?}"),
            );
        }
        if all_came.is_none() {
            step("not every notification came in time");
        }
        check(
            "no notification shows a sender's own label",
            !notifications
                .iter()
                .any(|shown| foreign(&shown.body) || foreign(&shown.summary)),
            || format!("notifications: {listing:?}"),
        );
        check(
            "a's own message from its other client notifies nothing",
            !notifications.iter().any(|shown| shown.body.contains("Build is green")),
            || format!("notifications: {listing:?}"),
        );
        Ok(())
    }

    /// The list in Teams Light and Dark, wide and narrow; the wide ones read by OCR.
    fn looks(&mut self) -> SandboxResult {
        for (dark, theme) in [(false, "light"), (true, "dark")] {
            self.run.theme(dark)?;
            let png = self.run.shot(self.sandbox, &format!("list-{theme}"))?;
            ocr_list(&png, dark, theme)?;
            for width in [760, 520] {
                self.resize(width)?;
                self.run.shot(self.sandbox, &format!("list-{theme}-{width}"))?;
            }
            self.resize(SCREEN.0)?;
        }
        self.run.theme(false)
    }

    fn windows(&self) -> SandboxResult<Vec<Value>> {
        let text = self.sandbox.niri_msg(&["-j", "windows"])?;
        let windows: Value = serde_json::from_str(&text)?;
        Ok(windows.as_array().cloned().unwrap_or_default())
    }

    fn focused_width(&self) -> SandboxResult<Option<f64>> {
        Ok(self
            .windows()?
            .iter()
            .find(|window| window["is_focused"] == true)
            .and_then(|window| window["layout"]["window_size"][0].as_f64()))
    }

    /// The compositor sizes the window to `width` wide, as a user's drag does.
    fn resize(&mut self, width: u32) -> SandboxResult {
        if self
            .focused_width()?
            .is_some_and(|now| (now - f64::from(width)).abs() < 0.5)
        {
            return Ok(());
        }
        self.sandbox
            .niri_msg(&["action", "set-column-width", &width.to_string()])?;
        wait_until(WAIT, || {
            self.focused_width()
                .ok()
                .flatten()
                .filter(|now| (now - f64::from(width)).abs() < 0.5)
        })
        .ok_or_else(|| format!("the window never took {width} px"))?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// b's message in the chat still shows "Everyone" over its text.
    fn message_view(&mut self) -> SandboxResult {
        let design = self.service.chats.design.clone();
        self.run.ok(&format!("open {design}"))?;
        let state = self.run.until("chat", "b's message in the design chat", |state| {
            last_text(state).is_some_and(|text| text.contains("Setup is ready"))
        })?;
        let text = last_text(&state).unwrap_or_default();
        check(
            "the message view shows \"Everyone\" over b's text",
            text.starts_with("Everyone") && text.contains("Setup is ready, try it") && !foreign(&text),
            || format!("it shows {text:?}"),
        );
        self.run.shot(self.sandbox, "message-view-light")?;
        self.run.theme(true)?;
        self.run.shot(self.sandbox, "message-view-dark")?;
        self.run.theme(false)
    }

    /// The newest message of `chat` selected with the keys (Escape from the box, then Up) and replied to (R): the box's
    /// quote reads `shown`.
    fn reply_quote(&mut self, chat: &str, shown: &str, name: &str) -> SandboxResult {
        self.run.ok(&format!("open {chat}"))?;
        let state = self.run.until("chat", "the chat open", |state| {
            state["conversation"] == chat && last_id(state).is_some()
        })?;
        let id = last_id(&state).unwrap_or_default();
        self.run.ok("focus-box")?;
        self.run.press("escape")?;
        let mut selected = false;
        for _ in 0..8 {
            if self.run.json("chat")?["selected"] == id.as_str() {
                selected = true;
                break;
            }
            self.run.press("up")?;
        }
        if !selected {
            return Err(format!("{name}: the newest message was never selected").into());
        }
        self.run.type_text("r")?;
        let state = self.run.until("chat", "the reply's quote in the box", |state| {
            state["box"]["mode"] == "reply"
        })?;
        let quote = state["box"]["shown"]
            .as_str()
            .or_else(|| state["box"]["preview"].as_str())
            .unwrap_or_default()
            .to_owned();
        let wanted = shown.strip_prefix("You: ").unwrap_or(shown);
        check(
            &format!("{name}: the box's quote reads {wanted:?}"),
            quote == wanted,
            || format!("it reads {quote:?} (sent as {})", state["box"]["preview"]),
        );
        self.run.shot(self.sandbox, name)?;
        self.run.press("escape")?;
        Ok(())
    }

    /// Search (Ctrl+E, the words, Enter, then its Messages tab) finds `wanted` (a chat's name and a word of the
    /// message's text): each hit's snippet says "Everyone", no hit shows a sender's label.
    fn search(&mut self, words: &str, wanted: &[(&str, &str)], name: &str) -> SandboxResult {
        self.run.press("ctrl+e")?;
        self.run.type_text(words)?;
        self.run.press("enter")?;
        self.run.ok("tab-messages")?;
        let results = self
            .run
            .until("search", "the search's results", |results| {
                wanted.iter().all(|(chat, _)| hit(results, chat).is_some())
            })
            .or_else(|_| self.run.json("search"))?;
        save_text(&format!("{name}.json"), &results.to_string())?;
        for (chat, word) in wanted {
            let snippet = hit(&results, chat)
                .and_then(|hit| hit["snippet"].as_str())
                .unwrap_or_default()
                .to_owned();
            check(
                &format!("search {words:?} finds {chat}'s message, its snippet in the reader's words"),
                snippet.contains("Everyone") && snippet.contains(word) && !foreign(&snippet),
                || format!("its snippet is {snippet:?}; results {results}"),
            );
        }
        let snippets: Vec<&str> = results["hits"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|hit| hit["snippet"].as_str())
            .collect();
        check(
            &format!("search {words:?}: no snippet shows a sender's label"),
            !snippets.iter().any(|snippet| foreign(snippet)),
            || format!("snippets: {snippets:?}"),
        );
        self.run.shot(self.sandbox, name)?;
        self.run.press("escape")?;
        self.run.ok(&format!("open {AB_CHAT}"))?;
        Ok(())
    }

    /// After the upgrade: the old message, indexed by an older build as "Все …", is found by "Everyone" again.
    fn after_upgrade(&mut self) -> SandboxResult {
        self.run.ok(&format!("open {AB_CHAT}"))?;
        thread::sleep(SETTLE);
        self.search(
            "Quarterly",
            &[(REVIEW, "Quarterly numbers are in")],
            "search-old-quarterly",
        )?;
        self.search(
            "Everyone Quarterly",
            &[(REVIEW, "Quarterly numbers are in")],
            "search-old-everyone",
        )
    }
}

/// The list's column read by OCR: b's row says "Everyone Setup is ready", never "Все" (which OCR reads as "Bce").
fn ocr_list(png: &Path, dark: bool, theme: &str) -> SandboxResult {
    let Some(text) = ocr(png, LIST_COLUMN, dark)? else {
        step("no tesseract: the OCR check is skipped");
        return Ok(());
    };
    save_text(&format!("ocr-list-{theme}.txt"), &text)?;
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    check(
        &format!("OCR of the {theme} list reads \"Everyone Setup is ready\""),
        flat.contains("Everyone Setup is ready"),
        || format!("OCR: {flat:?}"),
    );
    check(
        &format!("OCR of the {theme} list reads no \"Bce\" or \"Alle\""),
        !flat.contains("Bce") && !flat.contains("Alle") && !foreign(&flat),
        || format!("OCR: {flat:?}"),
    );
    Ok(())
}

fn hit<'v>(results: &'v Value, chat: &str) -> Option<&'v Value> {
    results["hits"].as_array()?.iter().find(|hit| hit["chat"] == chat)
}

fn item_text(state: &Value, id: &str) -> Option<String> {
    state["items"]
        .as_array()?
        .iter()
        .find(|item| item["id"] == id)
        .and_then(|item| item["text"].as_str())
        .map(str::to_owned)
}

fn last_text(state: &Value) -> Option<String> {
    state["items"].as_array()?.last()?["text"].as_str().map(str::to_owned)
}

fn last_id(state: &Value) -> Option<String> {
    state["items"].as_array()?.last()?["id"].as_str().map(str::to_owned)
}

/// Writes `text` as `name` in the evidence dir, if any.
fn save_text(name: &str, text: &str) -> SandboxResult {
    if let Some(dir) = evidence_dir() {
        fs::create_dir_all(&dir)?;
        fs::write(dir.join(name), text)?;
    }
    Ok(())
}

/// The text `tesseract` reads in the columns `columns` of the screenshot `png` (light text on dark inverted, scaled
/// three times); `None` without `tesseract`.
fn ocr(png: &Path, columns: (u32, u32), dark: bool) -> SandboxResult<Option<String>> {
    let shot = image::open(png)?.to_luma8();
    let width = columns.1.min(shot.width()) - columns.0;
    let mut crop = image::imageops::crop_imm(&shot, columns.0, 0, width, shot.height()).to_image();
    if dark {
        image::imageops::invert(&mut crop);
    }
    let scaled = image::imageops::resize(
        &crop,
        crop.width() * 3,
        crop.height() * 3,
        image::imageops::FilterType::CatmullRom,
    );
    let input = png.with_extension("ocr.png");
    scaled.save(&input)?;
    let output = match std::process::Command::new("tesseract")
        .arg(&input)
        .arg("stdout")
        .args(["--psm", "4", "-l", "eng"])
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let _ = fs::remove_file(&input);
    if !output.status.success() {
        return Err(format!("tesseract: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
}

// --- the child ---

/// One account's mock token, for the transport and the push channel alike.
struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

impl Tokens {
    fn access(&self) -> AccessToken {
        AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: std::time::SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }
    }
}

fn engine_on_the_mock(dir: &Path) -> SandboxResult<EngineHandle> {
    let origin = std::env::var(ORIGIN_ENV)?;
    let tokens = Arc::new(Tokens {
        token: std::env::var(TOKEN_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
    let transport = Arc::new(http()?.with_middle_tier(MIDDLE_TIER_BASE)?);
    let trouter = Trouter::with_mock_origin(
        Arc::clone(&transport),
        Arc::clone(&tokens),
        &std::env::var(TROUTER_ENV)?,
    )?
    .with_options(Options {
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(50),
        backoff_jitter_percent: 0,
        ..Options::default()
    });
    let author = Author::new(Mri::new(LAB_A), display_name("bot-a@lab.example.test"));
    Ok(engine::start(Parts {
        calls: None,
        store: Store::open(&support::store_file(dir, Alias::A))?,
        client: Arc::new(ChatClient::new(
            http()?
                .with_ams(AMS_BASE)?
                .with_presence(PRESENCE_BASE)?
                .with_csa(CSA_BASE)?
                .with_user_profile(USER_PROFILE_BASE)?
                .with_provisioning(PROVISIONING_BASE)?
                .with_middle_tier(MIDDLE_TIER_BASE)?,
        )),
        outbox: Outbox::new(transport, author),
        push: Some(Push::new(trouter)),
        options: EngineOptions::default(),
        known_names: vec![
            (LAB_B.to_owned(), "Lab B".to_owned()),
            (LAB_C.to_owned(), "Lab C".to_owned()),
        ],
    })?)
}

/// The child's `PATH` holds no tool but the made-up one.
fn only_the_made_up_tool() -> SandboxResult {
    let made_up = PathBuf::from(std::env::var_os(TOOLS_ENV).ok_or("no made-up tool's directory")?);
    let found = teams_ui::local_ai::Tools::detect();
    for backend in [teams_ui::local_ai::Backend::Claude, teams_ui::local_ai::Backend::Codex] {
        if let Some(program) = found.program(backend)
            && !program.starts_with(&made_up)
        {
            return Err(format!("a real tool is on the PATH: {}", program.display()).into());
        }
    }
    Ok(())
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    only_the_made_up_tool()?;
    let dir = PathBuf::from(std::env::var(DIR_ENV)?);
    let engine = engine_on_the_mock(&dir)?;
    let first = support::started(Alias::A, engine)?;
    let runner = support::runner(&dir)?;
    let settings = teams_ui::SettingsFile::new(dir.join("ui").join("a.toml"));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let theme_file = dir.join(THEME_FILE);
    let _ = fs::remove_file(&theme_file);
    let ask = dir.join(ASK);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: Vec::new(),
                trace: Arc::new(teams_ui::ViewTrace::off()),
                ctl: None,
                settings: teams_ui::SettingsModel::load(settings),
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(launched) => {
                let settings = launched.desktop.read(cx).settings().clone();
                follow_theme_file(&settings, theme_file.clone(), cx);
                let window = launched.window;
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, 3, |_, _| say("ready"));
                });
                answer(window, ask.clone(), cx);
            }
            Err(failed) => {
                *failed_in_app.lock().unwrap_or_else(PoisonError::into_inner) = Some(failed.error);
                app::quit_after(failed.stopped, cx);
            }
        }
    });
    if let Some(error) = failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
        return Err(error.into());
    }
    Ok(())
}

/// Runs `done` once `frames` more frames of `window` have been presented.
fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

/// The settings as the Appearance page leaves them once theme `key` is picked.
fn show_theme(settings: &mut teams_ui::Settings, key: &'static str) {
    settings.appearance.follow_system = false;
    settings.appearance.theme = key;
}

/// Shows the theme `file` names (`light` or `dark`) once it changes, and says `theme <name>` once it is applied.
fn follow_theme_file(settings: &Entity<teams_ui::SettingsModel>, file: PathBuf, cx: &mut App) {
    let settings = settings.downgrade();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut applied = String::new();
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(wanted) = fs::read_to_string(&file) else {
                continue;
            };
            if wanted == applied {
                continue;
            }
            let key = if wanted == "dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            let changed =
                cx.update(|cx| settings.update(cx, |model, cx| model.change(|chosen| show_theme(chosen, key), cx)));
            if changed.is_err() {
                return;
            }
            cx.background_executor().timer(Duration::from_millis(300)).await;
            say(&format!("theme {wanted}"));
            applied = wanted;
        }
    })
    .detach();
}

/// Answers the parent's questions as they come, until it asks to quit.
fn answer(window: WindowHandle<KitRoot>, ask: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(10)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let (seq, verb) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            if verb == "quit" {
                say(&format!("re {seq} ok"));
                cx.update(|cx| cx.quit());
                return;
            }
            let reply = window
                .update(cx, |kit, window, cx| {
                    let shell = kit
                        .view()
                        .clone()
                        .downcast::<Root>()
                        .ok()?
                        .read(cx)
                        .shell_in(cx)
                        .cloned()?;
                    Some(on_shell(&shell, verb, window, cx))
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| "no shell".to_owned());
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn chat_view(shell: &Entity<Shell>, cx: &App) -> Option<Entity<ChatView>> {
    let shell = shell.read(cx);
    shell.chat().filter(|_| matches!(shell.main(), Main::Chat(_))).cloned()
}

fn on_shell(shell: &Entity<Shell>, verb: &str, window: &mut Window, cx: &mut App) -> String {
    let (verb, rest) = verb.split_once(' ').unwrap_or((verb, ""));
    match verb {
        "open" => {
            shell.update(cx, |shell, cx| shell.open_chat(rest.to_owned(), window, cx));
            "ok".to_owned()
        }
        "rows" => rows_of(shell, cx).to_string(),
        "chat" => chat_state(shell, window, cx).to_string(),
        "search" => search_state(shell, cx).to_string(),
        // the results page's Messages tab, as its click picks it
        "tab-messages" => {
            let search = shell.read(cx).search().clone();
            search.update(cx, |search, cx| {
                search.set_tab(teams_ui::global_search::Tab::Messages, cx)
            });
            "ok".to_owned()
        }
        "focus-box" => chat_view(shell, cx).map_or_else(
            || "no chat".to_owned(),
            |chat| {
                let compose = chat.read(cx).compose().clone();
                compose.update(cx, |compose, cx| compose.focus(window, cx));
                "ok".to_owned()
            },
        ),
        // the whole chat read in a language, as the header's picker sets it
        "language" => chat_view(shell, cx).map_or_else(
            || "no chat".to_owned(),
            |chat| {
                let translation = chat.read(cx).translation().clone();
                translation.update(cx, |translation, cx| translation.set_language(Some("en"), cx));
                "ok".to_owned()
            },
        ),
        other => format!("unknown {other}"),
    }
}

/// Each chat list row: its chat, name and the preview it shows.
fn rows_of(shell: &Entity<Shell>, cx: &mut App) -> Value {
    let list = shell.read(cx).list().clone();
    let rows: Vec<teams_ui::chat_list::ChatRow> = list.read(cx).all_rows().cloned().collect();
    let shown: Vec<Value> = rows
        .iter()
        .map(|row| {
            let preview = list.update(cx, |list, cx| list.preview_shown(row, cx));
            json!({"id": row.id, "name": row.name.as_ref(), "preview": preview.as_ref()})
        })
        .collect();
    Value::Array(shown)
}

/// The open chat: its messages' words as shown, the one selected, and the box's mode and quote.
fn chat_state(shell: &Entity<Shell>, window: &Window, cx: &App) -> Value {
    let Some(chat) = chat_view(shell, cx) else {
        return json!({"items": []});
    };
    let chat = chat.read(cx);
    let items: Vec<Value> = chat
        .items()
        .iter()
        .filter_map(|item| {
            let Item::Message(message) = item else {
                return None;
            };
            let text = match &message.body {
                Body::Text(rendered) => rendered
                    .0
                    .blocks
                    .iter()
                    .flat_map(Block::plain_lines)
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            Some(json!({"id": message.id, "text": text}))
        })
        .collect();
    let compose = chat.compose().read(cx);
    let boxed = match compose.mode() {
        Mode::New => json!({"mode": "new"}),
        Mode::Reply(quoted) => json!({"mode": "reply", "preview": quoted.preview, "shown": quoted.shown}),
        Mode::Edit { .. } => json!({"mode": "edit"}),
    };
    json!({
        "conversation": chat.conversation(),
        "selected": chat.selected(),
        "focused": compose.focused(window, cx),
        "items": items,
        "box": boxed,
    })
}

/// The search's message hits: their chat's name and snippet.
fn search_state(shell: &Entity<Shell>, cx: &App) -> Value {
    let search = shell.read(cx).search().read(cx);
    let hits: Vec<Value> = search
        .message_hits(cx)
        .iter()
        .map(|hit| {
            json!({
                "chat": hit.chat.as_ref(),
                "sender": hit.sender.as_ref(),
                "snippet": hit.snippet.as_ref(),
                "conversation": hit.target.conversation,
                "message": hit.target.message,
            })
        })
        .collect();
    json!({"query": search.query(cx), "failed": search.failed(), "hits": hits})
}
