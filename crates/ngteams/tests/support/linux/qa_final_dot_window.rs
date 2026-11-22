//! QA of task 406 (round 1), in the UI sandbox: the AI correction keeps a missing final dot missing. The real app runs
//! as a child in the sandbox, as lab account a on `teams-mock`. The parent drives it with the sandbox's virtual keyboard
//! and asks the child what the box holds through a file in the sandbox dir (`harness = false`: GPUI runs on the
//! process's main thread).
//!
//! The correcting tool is a made-up `claude` ([`FAKE_CLAUDE`]) first on the app's `PATH`, which holds no directory with
//! a real `claude` or `codex`. It answers as a model does: it keeps every marker, fixes a few made-up typos and, as
//! the parent's rules file says before each case, adds a final dot where the draft has none (at the end of a block,
//! before a line break, after a mention or a link, inside or after bold and a named link, a CJK full stop), or after a
//! typed "?" or "!".
//!
//! - **Plain drafts** by Ctrl+Shift+G: no final dot kept none; a typed ".", "?" or "!" stays; an answer that only adds
//!   the dot shows "No corrections needed."; a real fix plus a dot keeps the fix only.
//! - **Paragraphs and line breaks**: Shift+Enter paragraphs in the box, Shift+Enter line breaks in the expanded box.
//! - **Rich drafts**: a's own messages edited (a link showing its address, a mention, bold, a named link, line breaks,
//!   a typed dot after a link); one edit saved, its PUT read on the mock.
//! - **Other languages and ends**: Cyrillic, CJK, an emoji, ":)", "...", pasted where the keymap has no key. A CJK
//!   full stop "。" that the model adds is no change either (`cjk_full_stop_added`).
//! - **More options**: Correct shown on the bar, a narrow window, Correct by its row in More options, the proposal taken
//!   by the row again.
//! - **Send corrected** (Ctrl+Enter): the POST on the mock has no added dot.
//!
//! `QA_FINAL_DOT_ONLY` (a comma list of check names) runs only those. `QA_FINAL_DOT_EVIDENCE` names a directory that
//! gets the screenshots, the tool's logs and the results too. Screenshots also go to `target/ui-shots/qa406-*.png`.
//! Every name, id and text is made up.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

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
    PROVISIONING_BASE, TrouterMock, USER_PROFILE_BASE,
};
use teams_proto::chat::Marks;
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::compose::{Compose, Mode};
use teams_ui::shell::{Main, Shell};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::VirtualInput;
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

const CHILD_ENV: &str = "QA_FINAL_DOT_CHILD";
const ONLY_ENV: &str = "QA_FINAL_DOT_ONLY";
const EVIDENCE_ENV: &str = "QA_FINAL_DOT_EVIDENCE";
const SETTINGS_ENV: &str = "QA_FINAL_DOT_SETTINGS";
const TOOLS_ENV: &str = "QA_FINAL_DOT_TOOLS";
/// The file in the sandbox dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "qa406-ask";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(60);
const SETTLE: Duration = Duration::from_millis(800);
/// The time between a key and the next, so the app sees each.
const STEP: Duration = Duration::from_millis(120);
/// niri's layout: each window takes the whole screen until the test sizes it, and no hot corner.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
const SCREEN: (u32, u32) = (1280, 800);

/// A made-up Claude Code (`__STATE__` its state directory). It answers each question with the message as it came, run
/// through `rules.sed` (`sed -E`, on the JSON-escaped line), and logs what it was asked and what it answered.
const FAKE_CLAUDE: &str = r#"#!/bin/bash
state="__STATE__"
case "$1" in
  --help)
    printf '%s\n' "Usage: claude [options]" "  --model <model>  Model for the current session: an alias such as 'sonnet' or 'opus'"
    exit 0;;
esac
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$state/received.log"
  t=$(printf '%s' "$line" | sed 's/.*<message>\\n\(.*\)\\n<\/message>.*/\1/')
  a=$(printf '%s' "$t" | sed -E -f "$state/rules.sed")
  printf '{"type":"result","subtype":"success","is_error":false,"result":"<corrected>%s</corrected>"}\n' "$a" | tee -a "$state/answers.log"
done
"#;

// --- the rules of the made-up model (sed -E on the JSON-escaped message: a block ends at `\n` or the end) ---

/// The made-up typos a model fixes.
const FIX: &str = "s/teh/the/g\ns/tomorow/tomorrow/g\ns/превет/привет/g\n";
/// A final dot at the end of each block, before its closing markers (inside bold or a named link at the end).
const DOT_END: &str = "s#([^.?!>])((</x[0-9]+>)+)(\\\\n|$)#\\1.\\2\\4#g\n";
/// A final dot after an atom (a mention, a link that shows its address) at the end of a block.
const DOT_ATOM: &str = "s#(/>)((</x[0-9]+>)+)(\\\\n|$)#\\1.\\2\\4#g\n";
/// A final dot after the styled run at the end of a block, not inside it.
const DOT_AFTER_STYLED: &str = "s#([^.?!>])(</x[0-9]+>)(</x[0-9]+>)(\\\\n|$)#\\1\\2.\\3\\4#g\n";
/// A final dot before a line break (an atom between two words).
const DOT_BREAK: &str = "s#([a-z])(<x[0-9]+/>)([a-z])#\\1.\\2\\3#g\n";
/// A dot after a typed "?" or "!" at the end of a block.
const DOT_AFTER_MARK: &str = "s#([?!])((</x[0-9]+>)+)(\\\\n|$)#\\1.\\2\\4#g\n";
/// A CJK full stop after "见" at the end of a block.
const CJK_STOP: &str = "s#(见)((</x[0-9]+>)+)(\\\\n|$)#\\1。\\2\\4#g\n";

/// a's own messages to edit: a name, its HTML as the store loads it.
const OWN: [(&str, &str); 6] = [
    (
        "link",
        "<p>teh notes are at <a href=\"https://example.com/notes\">https://example.com/notes</a></p>",
    ),
    (
        "mention",
        "<p>please send teh notes to <span itemscope=\"\" itemtype=\"http://schema.skype.com/Mention\" \
         itemid=\"0\">Lab B</span></p>",
    ),
    ("bold", "<p>send teh notes <b>today</b></p>"),
    (
        "named",
        "<p>teh notes are in <a href=\"https://example.com/docs\">the docs</a></p>",
    ),
    ("breaks", "<p>hi teh team<br>see teh notes<br>thanks</p>"),
    (
        "typed-dot",
        "<p>see teh notes at <a href=\"https://example.com/notes\">https://example.com/notes</a>.</p>",
    ),
];

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_final_dot_window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn ensure(holds: bool, what: impl FnOnce() -> String) -> SandboxResult {
    if holds { Ok(()) } else { Err(what().into()) }
}

fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn evidence_dir() -> Option<PathBuf> {
    std::env::var_os(EVIDENCE_ENV).map(PathBuf::from)
}

fn wanted(name: &str) -> bool {
    std::env::var(ONLY_ENV)
        .ok()
        .is_none_or(|list| list.split(',').map(str::trim).any(|one| one == name))
}

// --- the parent ---

type Check = (&'static str, fn(&mut User<'_>) -> SandboxResult);

const CHECKS: [Check; 12] = [
    ("only_a_dot_is_no_change", only_a_dot_is_no_change),
    ("fix_kept_dot_dropped", fix_kept_dot_dropped),
    ("typed_marks_stay", typed_marks_stay),
    ("paragraphs", paragraphs),
    ("line_breaks_expanded", line_breaks_expanded),
    ("rich_ends_in_an_edit", rich_ends_in_an_edit),
    ("edit_saved_without_dot", edit_saved_without_dot),
    ("other_languages", other_languages),
    ("cjk_full_stop_added", cjk_full_stop_added),
    ("emoji_smiley_ellipsis", emoji_smiley_ellipsis),
    ("send_corrected", send_corrected),
    ("more_options_row", more_options_row),
];

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_final_dot_window", &options)? else {
        return Ok(());
    };
    let tool = FakeTool::install(&sandbox)?;
    let settings = sandbox.path("settings.toml");
    let child = sandbox.spawn(
        &std::env::current_exe()?,
        &[],
        &[
            (CHILD_ENV, "app"),
            (SETTINGS_ENV, &settings.display().to_string()),
            (TOOLS_ENV, &tool.bin.display().to_string()),
            ("PATH", &tool.path),
            ("CLAUDE_CONFIG_DIR", &tool.home.display().to_string()),
            ("CODEX_HOME", &tool.home.display().to_string()),
        ],
    )?;
    let running = Running::new(child)?;
    let mut user = User::start(&sandbox, running, &tool)?;
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    for (name, check) in CHECKS {
        if wanted(name) {
            user.run_check(name, check, &mut passed, &mut failed);
        }
    }
    if let Some(dir) = evidence_dir() {
        for log in ["received.log", "answers.log"] {
            let _ = fs::copy(tool.state.join(log), dir.join(format!("tool-{log}")));
        }
    }
    let quit = user.ask("quit").and_then(|_| {
        let (status, stderr) = user.running.finish(WAIT)?;
        ensure(status.success(), || format!("the app exited {status}: {stderr}"))
    });
    let mut summary = String::new();
    for name in &passed {
        let _ = writeln!(summary, "pass {name}");
    }
    for (name, why) in &failed {
        let _ = writeln!(summary, "FAIL {name}: {why}");
    }
    say(&summary);
    if let Some(dir) = evidence_dir() {
        let _ = fs::write(dir.join("results.txt"), &summary);
    }
    say(&format!(
        "qa_final_dot_window: {} passed, {} failed",
        passed.len(),
        failed.len()
    ));
    quit?;
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("{} checks failed", failed.len()).into())
    }
}

/// The made-up tool as the parent sees it.
struct FakeTool {
    bin: PathBuf,
    state: PathBuf,
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
        let program = bin.join("claude");
        fs::write(&program, FAKE_CLAUDE.replace("__STATE__", &state.display().to_string()))?;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755))?;
        let path = std::env::join_paths([bin.clone(), commands].into_iter().chain(kept))?
            .into_string()
            .map_err(|_| "a PATH that isn't UTF-8")?;
        let tool = Self { bin, state, home, path };
        tool.rules(FIX)?;
        Ok(tool)
    }

    /// What the made-up model does to the next drafts.
    fn rules(&self, rules: &str) -> SandboxResult {
        fs::write(self.state.join("rules.sed"), rules)?;
        Ok(())
    }

    fn answers(&self) -> Vec<String> {
        fs::read_to_string(self.state.join("answers.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// How the user asks for a correction and takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum By {
    /// Ctrl+Shift+G, then Ctrl+Shift+G and Enter to take it.
    Key,
    /// The Correct row of More options, its button reached by `n` Tabs from the text.
    More(usize),
}

/// What a correction must end in.
#[derive(Clone, Copy, Debug)]
enum Want<'a> {
    /// "No corrections needed.", the draft as it was.
    Nothing,
    /// A proposal that, taken, leaves this text in the box.
    Text(&'a str),
}

struct User<'a> {
    sandbox: &'a Sandbox,
    running: Running,
    input: VirtualInput,
    tool: &'a FakeTool,
    asked: u32,
}

impl<'a> User<'a> {
    fn start(sandbox: &'a Sandbox, running: Running, tool: &'a FakeTool) -> SandboxResult<Self> {
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| format!("no ready line: {error:?}"))?;
        let mut user = Self {
            sandbox,
            running,
            input: sandbox.input()?,
            tool,
            asked: 0,
        };
        user.park()?;
        user.answer_ok("open-ab")?;
        thread::sleep(SETTLE);
        Ok(user)
    }

    fn run_check(
        &mut self,
        name: &str,
        check: fn(&mut User<'_>) -> SandboxResult,
        passed: &mut Vec<String>,
        failed: &mut Vec<(String, String)>,
    ) {
        say(&format!("check {name}"));
        match check(self) {
            Ok(()) => {
                say(&format!("  pass {name}"));
                passed.push(name.to_owned());
            }
            Err(error) => {
                say(&format!("  FAIL {name}: {error}"));
                let _ = self.shot(&format!("failed-{name}"));
                failed.push((name.to_owned(), error.to_string()));
            }
        }
        // the next check starts from a wide window, a new message, an empty box, Teams Light
        let _ = self.home();
    }

    fn ask(&mut self, verb: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = self.sandbox.path(&format!("{ASK}.staged"));
        fs::write(&staged, format!("{} {verb}", self.asked))?;
        fs::rename(&staged, self.sandbox.path(ASK))?;
        let prefix = format!("re {} ", self.asked);
        let line = self
            .running
            .line_where(WAIT, |line| line.starts_with(&prefix))
            .map_err(|error| format!("no answer to {verb}: {error:?}"))?;
        Ok(line[prefix.len()..].to_owned())
    }

    fn answer_ok(&mut self, verb: &str) -> SandboxResult {
        let answer = self.ask(verb)?;
        ensure(answer == "ok", || format!("{verb}: {answer}"))
    }

    fn state(&mut self) -> SandboxResult<Value> {
        Ok(serde_json::from_str(&self.ask("state")?)?)
    }

    fn until(&mut self, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let start = Instant::now();
        loop {
            let state = self.state()?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {state}").into());
            }
            thread::sleep(POLL);
        }
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

    /// Pastes `text` with Ctrl+V, as a copy in another program put it on the clipboard.
    fn paste(&mut self, text: &str) -> SandboxResult {
        let bytes = text.as_bytes().to_vec();
        self.sandbox.set_clipboard(vec![
            ("text/plain;charset=utf-8".to_owned(), bytes.clone()),
            ("text/plain".to_owned(), bytes),
        ])?;
        self.press("ctrl+v")
    }

    fn park(&mut self) -> SandboxResult {
        self.input.move_to(300, 23)
    }

    /// A screenshot as `qa406-<name>.png`, under `target/ui-shots/` and the evidence dir.
    fn shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        let file = format!("qa406-{name}.png");
        let png = self.sandbox.screenshot(&file, false)?;
        let kept = shots_dir()?.join(&file);
        fs::copy(&png, &kept)?;
        if let Some(dir) = evidence_dir() {
            let dir = dir.join("shots");
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(&file))?;
        }
        Ok(kept)
    }

    fn settled_shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        self.park()?;
        thread::sleep(SETTLE);
        self.shot(name)
    }

    /// The same screen in Teams Light and Teams Dark; Teams Light after.
    fn shots_in_both_themes(&mut self, name: &str) -> SandboxResult {
        self.settled_shot(&format!("{name}-light"))?;
        self.answer_ok("theme-dark")?;
        self.settled_shot(&format!("{name}-dark"))?;
        self.answer_ok("theme-light")?;
        thread::sleep(SETTLE);
        Ok(())
    }

    // --- windows ---

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

    /// The compositor sizes the focused window to `width` wide, as a user's drag does.
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

    // --- the box ---

    /// Back to the start of a check: a wide window, a's chat with b, a new message in an empty, focused box, Teams
    /// Light, the model fixing typos only.
    fn home(&mut self) -> SandboxResult {
        self.press("escape")?;
        self.press("escape")?;
        self.resize(SCREEN.0)?;
        self.answer_ok("theme-light")?;
        self.answer_ok("open-ab")?;
        if self.state()?["box"]["mode"] != "new" {
            self.answer_ok("focus")?;
            self.press("escape")?;
            self.until("the edit ended", |state| state["box"]["mode"] == "new")?;
        }
        if self.state()?["box"]["expanded"] == true {
            self.answer_ok("focus")?;
            self.press("ctrl+shift+x")?;
        }
        self.answer_ok("focus")?;
        self.press("ctrl+a")?;
        self.press("backspace")?;
        self.tool.rules(FIX)?;
        self.until("an empty, focused box", |state| {
            text(state).is_empty() && state["box"]["focused"] == true && state["box"]["mode"] == "new"
        })?;
        Ok(())
    }

    /// Types `draft` into the empty, focused box.
    fn type_draft(&mut self, draft: &str) -> SandboxResult {
        self.type_text(draft)?;
        self.until("the draft typed", |state| text(state) == draft)?;
        Ok(())
    }

    /// Pastes `draft` into the empty, focused box.
    fn paste_draft(&mut self, draft: &str) -> SandboxResult {
        self.paste(draft)?;
        self.until("the draft pasted", |state| text(state) == draft)?;
        Ok(())
    }

    /// Asks for a correction `by`, the model on `rules`; the box's state when the answer is in.
    fn correct(&mut self, by: By, rules: &str) -> SandboxResult<Value> {
        self.tool.rules(rules)?;
        let before = self.tool.answers().len();
        match by {
            By::Key => self.press("ctrl+shift+g")?,
            By::More(tabs) => self.more_row(tabs)?,
        }
        let tool = self.tool;
        wait_until(WAIT, || (tool.answers().len() > before).then_some(()))
            .ok_or("the made-up model was never asked")?;
        let state = self.until("the correction's outcome", |state| {
            state["box"]["correction"] != "Running"
        })?;
        if let Some(answer) = self.tool.answers().last() {
            say(&format!("  answer: {answer}"));
        }
        say(&format!(
            "  outcome {}, preview {}",
            state["box"]["correction"], state["box"]["preview"]
        ));
        Ok(state)
    }

    /// Opens More options by its button (`tabs` Tabs from the text, then Enter) and takes its first row.
    fn more_row(&mut self, tabs: usize) -> SandboxResult {
        self.open_more(tabs)?;
        self.press("down")?;
        self.press("enter")
    }

    fn open_more(&mut self, tabs: usize) -> SandboxResult<Value> {
        for _ in 0..tabs {
            self.press("tab")?;
        }
        self.press("enter")?;
        self.until("More options open", |state| state["box"]["more_open"] == true)
    }

    /// Takes the proposal shown (`by`'s way); the box's state after.
    fn take(&mut self, by: By) -> SandboxResult<Value> {
        match by {
            By::Key => self.press("ctrl+shift+g")?,
            By::More(tabs) => self.more_row(tabs)?,
        }
        self.press("enter")?;
        self.until("the correction taken", |state| state["box"]["correction"] == "Idle")
    }

    /// Corrects the draft in the box `by`, the model on `rules`, and checks the outcome is `want`. `shots` names the
    /// screenshots (before, the outcome, after) in both themes.
    fn case(&mut self, by: By, rules: &str, want: Want<'_>, shots: Option<&str>) -> SandboxResult<Value> {
        let draft = text(&self.state()?);
        if let Some(name) = shots {
            self.shots_in_both_themes(&format!("{name}-1-draft"))?;
        }
        let state = self.correct(by, rules)?;
        if let Some(name) = shots {
            self.shots_in_both_themes(&format!("{name}-2-outcome"))?;
        }
        match want {
            Want::Nothing => {
                ensure(state["box"]["correction"] == "NothingToCorrect", || {
                    format!("{draft:?}: wanted \"No corrections needed.\", got {}", state["box"])
                })?;
                ensure(text(&state) == draft, || format!("the draft changed: {}", state["box"]))?;
                Ok(state)
            }
            Want::Text(wanted) => {
                ensure(state["box"]["correction"] == "Proposed", || {
                    format!("{draft:?}: wanted a proposal, got {}", state["box"])
                })?;
                ensure(state["box"]["proposal"] == wanted, || {
                    format!(
                        "{draft:?}: the proposal is {}, wanted {wanted:?}",
                        state["box"]["proposal"]
                    )
                })?;
                let after = self.take(by)?;
                if let Some(name) = shots {
                    self.shots_in_both_themes(&format!("{name}-3-taken"))?;
                }
                ensure(text(&after) == wanted, || {
                    format!("{draft:?}: the box holds {:?}, wanted {wanted:?}", text(&after))
                })?;
                Ok(after)
            }
        }
    }

    /// Selects a's own message `name` (Up from the box, as a user walks the messages) and edits it (`e`).
    fn edit_own(&mut self, name: &str) -> SandboxResult<Value> {
        let id = self.ask(&format!("own {name}"))?;
        self.press("escape")?;
        let mut found = false;
        for key in ["up", "down"] {
            for _ in 0..24 {
                if self.state()?["selected"] == id.as_str() {
                    found = true;
                    break;
                }
                self.press(key)?;
            }
            if found {
                break;
            }
        }
        ensure(found, || format!("a's message {name} ({id}) never selected"))?;
        self.type_text("e")?;
        self.until(&format!("editing {name}"), |state| {
            state["box"]["mode"] == "edit" && state["box"]["focused"] == true && !text(state).is_empty()
        })
    }

    /// The bodies of the requests of `method` the mock got with `mark` in them.
    fn sent(&mut self, method: &str, mark: &str) -> SandboxResult<Vec<String>> {
        let answer = self.ask(&format!("sent {method} {mark}"))?;
        Ok(serde_json::from_str(&answer)?)
    }

    fn wait_sent(&mut self, method: &str, mark: &str) -> SandboxResult<String> {
        let start = Instant::now();
        loop {
            if let Some(body) = self.sent(method, mark)?.into_iter().next() {
                return Ok(body);
            }
            if start.elapsed() > WAIT {
                return Err(format!("the mock got no {method} with {mark:?}").into());
            }
            thread::sleep(POLL);
        }
    }
}

fn text(state: &Value) -> String {
    state["box"]["text"].as_str().unwrap_or_default().to_owned()
}

/// The runs of the box's blocks as `(text, what)`: `what` names a link, a mention or bold.
fn runs(state: &Value) -> Vec<(String, String)> {
    let mut all = Vec::new();
    for block in state["box"]["blocks"].as_array().into_iter().flatten() {
        for run in block["runs"].as_array().into_iter().flatten() {
            let what = if !run["mention"].is_null() {
                "mention".to_owned()
            } else if let Some(link) = run["link"].as_str() {
                format!("link {link}")
            } else if run["bold"] == true {
                "bold".to_owned()
            } else {
                String::new()
            };
            all.push((run["t"].as_str().unwrap_or_default().to_owned(), what));
        }
    }
    all
}

fn rules(parts: &[&str]) -> String {
    parts.concat()
}

// --- the checks ---

/// An answer that only adds the final dot is no change: "No corrections needed.", the draft as it was.
fn only_a_dot_is_no_change(user: &mut User<'_>) -> SandboxResult {
    user.type_draft("can you send the notes")?;
    user.case(By::Key, &rules(&[FIX, DOT_END]), Want::Nothing, Some("only-dot"))?;
    // the box stays usable: a word typed after it
    user.type_text(" now")?;
    user.until("the draft after no change", |state| {
        text(state) == "can you send the notes now"
    })?;
    Ok(())
}

/// A real fix plus an added dot: the fix stays, the dot goes.
fn fix_kept_dot_dropped(user: &mut User<'_>) -> SandboxResult {
    user.type_draft("can you send teh notes")?;
    let state = user.case(
        By::Key,
        &rules(&[FIX, DOT_END]),
        Want::Text("can you send the notes"),
        Some("fix-plus-dot"),
    )?;
    say(&format!("  after: {}", state["box"]["blocks"]));
    // Ctrl+Z takes the correction back in one step
    user.press("ctrl+z")?;
    user.until("Ctrl+Z gives the draft back", |state| {
        text(state) == "can you send teh notes"
    })?;
    Ok(())
}

/// A typed ".", "?" and "!" stay, with no second mark; a model that adds a dot after "?" or "!" loses that dot.
fn typed_marks_stay(user: &mut User<'_>) -> SandboxResult {
    let cases = [
        ("send teh notes.", "send the notes."),
        ("can you send teh notes?", "can you send the notes?"),
        ("teh notes are great!", "the notes are great!"),
    ];
    for (draft, wanted) in cases {
        user.home()?;
        user.type_draft(draft)?;
        user.case(By::Key, &rules(&[FIX, DOT_END]), Want::Text(wanted), None)?;
        user.home()?;
        user.type_draft(draft)?;
        user.case(
            By::Key,
            &rules(&[FIX, DOT_END, DOT_AFTER_MARK]),
            Want::Text(wanted),
            None,
        )?;
    }
    user.home()?;
    user.type_draft("can you send the notes?")?;
    user.case(By::Key, &rules(&[FIX, DOT_AFTER_MARK]), Want::Nothing, None)?;
    Ok(())
}

/// Paragraphs (Shift+Enter in the box): no paragraph gets a dot it did not have; a typed one stays.
fn paragraphs(user: &mut User<'_>) -> SandboxResult {
    user.type_text("hi teh team")?;
    user.press("shift+enter")?;
    user.type_text("the notes are in teh folder.")?;
    user.press("shift+enter")?;
    user.type_text("thanks")?;
    let draft = "hi teh team\nthe notes are in teh folder.\nthanks";
    let state = user.until("three paragraphs", |state| text(state) == draft)?;
    ensure(state["box"]["blocks"].as_array().map_or(0, Vec::len) == 3, || {
        format!("not three blocks: {}", state["box"]["blocks"])
    })?;
    let after = user.case(
        By::Key,
        &rules(&[FIX, DOT_END]),
        Want::Text("hi the team\nthe notes are in the folder.\nthanks"),
        Some("paragraphs"),
    )?;
    ensure(after["box"]["blocks"].as_array().map_or(0, Vec::len) == 3, || {
        format!("the paragraphs changed: {}", after["box"]["blocks"])
    })?;
    // only dots added: no change
    user.home()?;
    user.type_text("hi team")?;
    user.press("shift+enter")?;
    user.type_text("see you")?;
    user.until("two paragraphs", |state| text(state) == "hi team\nsee you")?;
    user.case(By::Key, &rules(&[FIX, DOT_END]), Want::Nothing, None)?;
    Ok(())
}

/// Line breaks (Shift+Enter in the expanded box): no line gets a dot it did not have.
fn line_breaks_expanded(user: &mut User<'_>) -> SandboxResult {
    user.press("ctrl+shift+x")?;
    user.until("the expanded box", |state| state["box"]["expanded"] == true)?;
    user.answer_ok("focus")?;
    user.type_text("hi teh team")?;
    user.press("shift+enter")?;
    user.type_text("see teh notes")?;
    user.press("shift+enter")?;
    user.type_text("thanks")?;
    let state = user.until("three lines", |state| {
        text(state) == "hi teh team\nsee teh notes\nthanks"
    })?;
    say(&format!("  the lines' blocks: {}", state["box"]["blocks"]));
    user.case(
        By::Key,
        &rules(&[FIX, DOT_BREAK, DOT_END]),
        Want::Text("hi the team\nsee the notes\nthanks"),
        Some("line-breaks"),
    )?;
    user.press("ctrl+shift+x")?;
    Ok(())
}

/// a's own messages edited: a dot after a link, a mention, bold (inside and after), a named link (inside and after)
/// and before line breaks goes; the fix, the link, the mention and the bold stay; a typed dot after a link stays.
fn rich_ends_in_an_edit(user: &mut User<'_>) -> SandboxResult {
    let cases: [(&str, String, &str, &str); 9] = [
        (
            "link",
            rules(&[FIX, DOT_ATOM]),
            "the notes are at https://example.com/notes",
            "link",
        ),
        (
            "mention",
            rules(&[FIX, DOT_ATOM]),
            "please send the notes to Lab B",
            "mention",
        ),
        ("bold", rules(&[FIX, DOT_END]), "send the notes today", "bold"),
        ("bold", rules(&[FIX, DOT_AFTER_STYLED]), "send the notes today", "bold"),
        (
            "named",
            rules(&[FIX, DOT_END]),
            "the notes are in the docs",
            "link https://example.com/docs",
        ),
        (
            "named",
            rules(&[FIX, DOT_AFTER_STYLED]),
            "the notes are in the docs",
            "link https://example.com/docs",
        ),
        (
            "breaks",
            rules(&[FIX, DOT_BREAK, DOT_END]),
            "hi the team\nsee the notes\nthanks",
            "",
        ),
        (
            "typed-dot",
            rules(&[FIX, DOT_ATOM, DOT_END]),
            "see the notes at https://example.com/notes.",
            "link",
        ),
        ("named", rules(&[DOT_END]), "", ""),
    ];
    let mut shot = false;
    for (name, model, wanted, kept) in &cases {
        user.home()?;
        let before = user.edit_own(name)?;
        let shots = (*name == "named" && !shot).then_some("edit-named");
        shot |= shots.is_some();
        let want = if wanted.is_empty() {
            Want::Nothing
        } else {
            Want::Text(wanted)
        };
        let after = user.case(By::Key, model, want, shots)?;
        if wanted.is_empty() {
            continue;
        }
        let (old, new) = (runs(&before), runs(&after));
        say(&format!("  {name}: {old:?} -> {new:?}"));
        if !kept.is_empty() {
            let had = old.iter().filter(|(_, what)| what.starts_with(kept)).count();
            let has: Vec<&(String, String)> = new.iter().filter(|(_, what)| what.starts_with(kept)).collect();
            ensure(had == has.len() && had > 0, || {
                format!("{name}: the {kept} runs changed: {old:?} -> {new:?}")
            })?;
            ensure(has.iter().all(|(text, _)| !text.ends_with('.')), || {
                format!("{name}: a dot inside the {kept}: {new:?}")
            })?;
        }
    }
    Ok(())
}

/// An edit taken and saved: the PUT carries the fix and no added dot, the mention kept.
fn edit_saved_without_dot(user: &mut User<'_>) -> SandboxResult {
    user.edit_own("mention")?;
    user.case(
        By::Key,
        &rules(&[FIX, DOT_ATOM, DOT_END]),
        Want::Text("please send the notes to Lab B"),
        None,
    )?;
    user.press("enter")?;
    user.until("the edit saved", |state| state["box"]["mode"] == "new")?;
    let body = user.wait_sent("PUT", "please send the notes")?;
    say(&format!("  the PUT: {body}"));
    let content = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|body| body["content"].as_str().map(str::to_owned))
        .unwrap_or(body);
    ensure(content.contains("Mention") && content.contains("Lab B"), || {
        format!("the mention is gone: {content}")
    })?;
    ensure(
        !content.contains("Lab B</span>.") && !content.contains("Lab B.</span>"),
        || format!("a dot after the mention went out: {content}"),
    )?;
    user.settled_shot("edit-saved")?;
    Ok(())
}

/// Cyrillic and CJK: no added dot or full stop; a typed "。" stays without a dot after it.
fn other_languages(user: &mut User<'_>) -> SandboxResult {
    user.paste_draft("превет, как дела")?;
    user.case(
        By::Key,
        &rules(&[FIX, DOT_END]),
        Want::Text("привет, как дела"),
        Some("cyrillic"),
    )?;
    user.home()?;
    user.paste_draft("как дела")?;
    user.case(By::Key, &rules(&[FIX, DOT_END]), Want::Nothing, None)?;
    user.home()?;
    user.paste_draft("好的。")?;
    user.case(By::Key, &rules(&[FIX, DOT_END]), Want::Nothing, None)?;
    user.home()?;
    user.paste_draft("我们明天见")?;
    user.case(By::Key, &rules(&[FIX, DOT_END]), Want::Nothing, None)?;
    Ok(())
}

/// A CJK full stop "。" that the model adds where the writer typed none is no change either (QA 406 Q1).
fn cjk_full_stop_added(user: &mut User<'_>) -> SandboxResult {
    user.paste_draft("我们明天见")?;
    user.case(By::Key, &rules(&[FIX, CJK_STOP]), Want::Nothing, Some("cjk-stop"))?;
    Ok(())
}

/// An emoji, ":)" and "..." at the end: no dot after them; the "..." stays as typed.
fn emoji_smiley_ellipsis(user: &mut User<'_>) -> SandboxResult {
    user.paste_draft("see you tomorow 👍")?;
    user.case(
        By::Key,
        &rules(&[FIX, DOT_END]),
        Want::Text("see you tomorrow 👍"),
        Some("emoji"),
    )?;
    user.home()?;
    user.type_draft("thanks teh team :)")?;
    user.case(By::Key, &rules(&[FIX, DOT_END]), Want::Text("thanks the team :)"), None)?;
    user.home()?;
    user.type_draft("wait for teh build...")?;
    user.case(
        By::Key,
        &rules(&[FIX, DOT_END]),
        Want::Text("wait for the build..."),
        None,
    )?;
    user.home()?;
    user.paste_draft("ok 👍")?;
    user.case(By::Key, &rules(&[FIX, DOT_END]), Want::Nothing, None)?;
    Ok(())
}

/// "Send corrected" (Ctrl+Enter on the proposal): the POST has the fix and no added dot.
fn send_corrected(user: &mut User<'_>) -> SandboxResult {
    let mark = "zqfinal";
    user.type_draft(&format!("please check teh notes {mark}"))?;
    let state = user.correct(By::Key, &rules(&[FIX, DOT_END]))?;
    ensure(
        state["box"]["proposal"] == format!("please check the notes {mark}"),
        || format!("the proposal: {}", state["box"]),
    )?;
    user.press("ctrl+shift+g")?;
    user.press("ctrl+enter")?;
    let body = user.wait_sent("POST", mark)?;
    say(&format!("  the POST: {body}"));
    ensure(body.contains(&format!("please check the notes {mark}")), || {
        format!("the fix did not go out: {body}")
    })?;
    ensure(!body.contains(&format!("{mark}.")), || {
        format!("the added dot went out: {body}")
    })?;
    user.until("the box empty after Send corrected", |state| text(state).is_empty())?;
    user.settled_shot("send-corrected")?;
    Ok(())
}

/// A narrow window, Correct in More options: its row asks, its row again gives the keyboard to the proposal, Enter
/// takes it; an answer that only adds a dot is no change there too.
fn more_options_row(user: &mut User<'_>) -> SandboxResult {
    // Correct is a row of "+" until the user shows it on the bar; only a shown tool goes into More options
    user.answer_ok("show-correct")?;
    let mut chosen = None;
    for width in [700, 640, 600, 560] {
        user.resize(width)?;
        user.answer_ok("focus")?;
        let state = user.state()?;
        say(&format!(
            "  {width} px: stacked {} narrow {}",
            state["box"]["stacked"], state["box"]["narrow"]
        ));
        if state["box"]["stacked"] == true {
            chosen = Some(width);
            break;
        }
    }
    let width = chosen.ok_or("the box never stacked")?;
    // which Tab stop is More options
    user.type_draft("probe")?;
    let mut tabs = None;
    for stops in 1..=7 {
        user.answer_ok("focus")?;
        for _ in 0..stops {
            user.press("tab")?;
        }
        user.press("enter")?;
        let state = user.state()?;
        let found = state["box"]["more_open"] == true;
        say(&format!(
            "  Tab x{stops}, Enter: more {} expanded {} text {:?}",
            state["box"]["more_open"],
            state["box"]["expanded"],
            text(&state)
        ));
        user.press("escape")?;
        if found {
            tabs = Some(stops);
            break;
        }
        if state["box"]["expanded"] == true {
            user.answer_ok("focus")?;
            user.press("ctrl+shift+x")?;
        }
        if text(&state) != "probe" {
            user.answer_ok("focus")?;
            user.press("ctrl+a")?;
            user.press("backspace")?;
            user.type_draft("probe")?;
        }
    }
    let tabs = tabs.ok_or("no Tab stop opens More options")?;
    say(&format!("  More options at {width} px: Tab x{tabs}"));
    user.answer_ok("focus")?;
    user.press("ctrl+a")?;
    user.press("backspace")?;
    user.type_draft("can you send teh notes")?;
    user.open_more(tabs)?;
    user.shots_in_both_themes("more-1-menu")?;
    user.press("escape")?;
    user.answer_ok("focus")?;
    user.case(
        By::More(tabs),
        &rules(&[FIX, DOT_END]),
        Want::Text("can you send the notes"),
        Some("more"),
    )?;
    user.answer_ok("focus")?;
    user.press("ctrl+a")?;
    user.press("backspace")?;
    user.type_draft("can you send the notes")?;
    user.case(
        By::More(tabs),
        &rules(&[FIX, DOT_END]),
        Want::Nothing,
        Some("more-only-dot"),
    )?;
    Ok(())
}

// --- the child: the app on the mock, answering the parent ---

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

/// The mock, with a's own messages to edit, and a's engine on it.
struct MockLab {
    runtime: tokio::runtime::Runtime,
    mock: ChatServiceMock,
    _trouter: TrouterMock,
    dir: tempfile::TempDir,
    /// a's own messages: their names and ids.
    own: Vec<(&'static str, String)>,
}

impl MockLab {
    fn start() -> SandboxResult<(Self, EngineHandle)> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        let own = seed_own(&mock)?;
        let dir = tempfile::tempdir()?;
        let tokens = Arc::new(Tokens {
            token: mock.token(LAB_A).ok_or("the mock's token for a")?,
            renewals: broadcast::channel(4).0,
        });
        let origin = mock.origin();
        let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
        let transport = Arc::new(http()?);
        let trouter_client = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), trouter.origin())?
            .with_options(Options {
                backoff_base: Duration::from_millis(50),
                backoff_cap: Duration::from_millis(50),
                backoff_jitter_percent: 0,
                ..Options::default()
            });
        let author = Author::new(Mri::new(LAB_A), display_name("bot-a@lab.example.test"));
        let engine = engine::start(Parts {
            calls: None,
            store: Store::open(&support::store_file(dir.path(), Alias::A))?,
            client: Arc::new(Client::new(
                http()?
                    .with_ams(AMS_BASE)?
                    .with_presence(PRESENCE_BASE)?
                    .with_csa(CSA_BASE)?
                    .with_user_profile(USER_PROFILE_BASE)?
                    .with_provisioning(PROVISIONING_BASE)?
                    .with_middle_tier(MIDDLE_TIER_BASE)?,
            )),
            outbox: Outbox::new(transport, author),
            push: Some(Push::new(trouter_client)),
            options: EngineOptions::default(),
            known_names: vec![
                (LAB_B.to_owned(), "Lab B".to_owned()),
                (LAB_C.to_owned(), "Lab C".to_owned()),
            ],
        })?;
        let lab = Self {
            runtime,
            mock,
            _trouter: trouter,
            dir,
            own,
        };
        Ok((lab, engine))
    }

    /// The bodies of the requests of `method` the mock got with `mark` in them, as a JSON list.
    fn sent(&self, method: &str, mark: &str) -> String {
        let bodies: Vec<String> = self
            .runtime
            .block_on(self.mock.received_requests())
            .iter()
            .filter(|request| request.method.as_str() == method)
            .map(|request| String::from_utf8_lossy(&request.body).into_owned())
            .filter(|body| body.contains(mark))
            .collect();
        json!(bodies).to_string()
    }
}

/// a's own messages in the a–b chat ([`OWN`]), the mention named; their ids.
fn seed_own(mock: &ChatServiceMock) -> SandboxResult<Vec<(&'static str, String)>> {
    let mut own = Vec::new();
    for (name, html) in OWN {
        let id = mock.post_as(LAB_A, AB_CHAT, html).ok_or("a's post refused")?;
        if name == "mention" {
            let mentions = json!([{
                "@type": "http://schema.skype.com/Mention", "itemid": 0, "mri": LAB_B, "mentionType": "person",
                "displayName": "Lab B",
            }])
            .to_string();
            let client_message_id = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
            let changed = mock.change_message(AB_CHAT, &client_message_id, |message| {
                message["properties"]["mentions"] = json!(mentions);
            });
            ensure(changed, || "a's post isn't in the mock".to_owned())?;
        }
        own.push((name, id));
    }
    Ok(own)
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
    let settings_path = PathBuf::from(std::env::var_os(SETTINGS_ENV).ok_or("no settings path")?);
    let (lab, engine) = MockLab::start()?;
    let lab = Arc::new(lab);
    let first = support::started(Alias::A, engine)?;
    let settings = teams_ui::SettingsFile::new(settings_path);
    let runner = support::runner(lab.dir.path())?;
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let in_app = Arc::clone(&lab);
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
                let window = launched.window;
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, 3, |_, _| say("ready"));
                });
                answer(window, Arc::clone(&in_app), cx);
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
    drop(lab);
    Ok(())
}

fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

/// Answers the parent's questions as they come, until it asks to quit.
fn answer(window: WindowHandle<KitRoot>, lab: Arc<MockLab>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let ask = PathBuf::from(dir).join(ASK);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(10)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let (seq, verb) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            let reply = if verb == "quit" {
                say(&format!("re {seq} ok"));
                cx.update(|cx| cx.quit());
                return;
            } else if let Some(name) = verb.strip_prefix("own ") {
                lab.own
                    .iter()
                    .find(|(own, _)| *own == name)
                    .map_or_else(|| format!("no own message {name}"), |(_, id)| id.clone())
            } else if let Some(rest) = verb.strip_prefix("sent ") {
                let (method, mark) = rest.split_once(' ').unwrap_or((rest, ""));
                lab.sent(method, mark)
            } else {
                window
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
                    .unwrap_or_else(|| "no shell".to_owned())
            };
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn chat_box(shell: &Entity<Shell>, cx: &App) -> Option<Entity<Compose>> {
    let shell = shell.read(cx);
    shell
        .chat()
        .filter(|_| matches!(shell.main(), Main::Chat(_)))
        .map(|chat| chat.read(cx).compose().clone())
}

fn on_shell(shell: &Entity<Shell>, verb: &str, window: &mut Window, cx: &mut App) -> String {
    match verb {
        "open-ab" => {
            shell.update(cx, |shell, cx| shell.open_chat(AB_CHAT.to_owned(), window, cx));
            "ok".to_owned()
        }
        "focus" => chat_box(shell, cx).map_or_else(
            || "no box".to_owned(),
            |compose| {
                compose.update(cx, |compose, cx| compose.focus(window, cx));
                "ok".to_owned()
            },
        ),
        "theme-dark" | "theme-light" => {
            let key = if verb == "theme-dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            let model = shell.read(cx).settings().clone();
            model.update(cx, |model, cx| {
                model.change(
                    |settings| {
                        settings.appearance.follow_system = false;
                        settings.appearance.theme = key;
                    },
                    cx,
                );
            });
            "ok".to_owned()
        }
        "show-correct" => {
            let model = shell.read(cx).settings().clone();
            model.update(cx, |model, cx| {
                model.change(
                    |settings| settings.show_compose_tool(teams_ui::settings::ComposeTool::Correct, true),
                    cx,
                );
            });
            "ok".to_owned()
        }
        "state" => state_of(shell, window, cx).to_string(),
        other => format!("unknown {other}"),
    }
}

fn state_of(shell: &Entity<Shell>, window: &Window, cx: &App) -> Value {
    let shell_ref = shell.read(cx);
    let mut selected = Value::Null;
    let mut boxed = Value::Null;
    if let Some(chat) = shell_ref.chat().filter(|_| matches!(shell_ref.main(), Main::Chat(_))) {
        let chat = chat.read(cx);
        selected = chat.selected().map_or(Value::Null, |id| json!(id));
        boxed = box_state(chat.compose(), window, cx);
    }
    json!({ "selected": selected, "box": boxed })
}

fn box_state(compose: &Entity<Compose>, window: &Window, cx: &App) -> Value {
    let compose = compose.read(cx);
    let doc = compose.editor().read(cx).doc();
    let blocks: Vec<Value> = doc
        .blocks
        .iter()
        .map(|block| {
            let runs: Vec<Value> = block
                .runs
                .iter()
                .map(|run| {
                    json!({
                        "t": run.text,
                        "bold": run.attrs.marks.contains(Marks::BOLD),
                        "link": run.attrs.link,
                        "mention": run.attrs.mention,
                    })
                })
                .collect();
            json!({"kind": format!("{:?}", block.kind), "runs": runs})
        })
        .collect();
    let mode = match compose.mode() {
        Mode::New => "new",
        Mode::Reply(_) => "reply",
        Mode::Edit { .. } => "edit",
    };
    json!({
        "text": doc.plain_text(),
        "blocks": blocks,
        "correction": format!("{:?}", compose.correction_status()),
        "preview": compose.correction_preview_text(),
        "proposal": compose.correction_proposal().map(|proposal| proposal.corrected.plain_text()),
        "mode": mode,
        "focused": compose.focused(window, cx),
        "narrow": compose.is_narrow(),
        "stacked": compose.is_stacked(cx),
        "expanded": compose.is_expanded(),
        "more_open": compose.more_open(),
    })
}
