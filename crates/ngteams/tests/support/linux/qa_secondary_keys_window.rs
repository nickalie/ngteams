//! QA of task 903 (round 1), in the UI sandbox: the app starts on GPUI 0.3.14 and its `secondary!` shortcuts (Ctrl on
//! Linux) fire when the user presses them on the keyboard (`harness = false`: GPUI runs on the process's main thread).
//! GPUI 0.3.14 does not read `secondary-` in a key binding, and a binding it cannot read stops the app at its start.
//!
//! The parent runs the mock (`teams-mock`) with one made-up group chat, and runs a's app as a child in the sandbox. The
//! parent presses each shortcut with the sandbox's keyboard and reads from the child what it changed:
//!
//! - **The compose box**: select all, copy, cut, paste, undo, redo, bold, italic, underline, strikethrough, the
//!   paragraph styles, the lists, the code block, the expanded box and the emoji picker.
//! - **The app**: Activity and its filters, Chat, Calendar, the switcher, the search and its go-to mode, the font
//!   size keys and New message.
//! - **Scrolling** the messages with the wheel (the sandbox's pointer) and a touchpad (GPUI's pixel scroll events).
//! - **Settings** opened from the rail: the General page shows from its top.
//! - **The date picker**: a new event as a series, its "Ends" picker opened and a day picked.
//!
//! Every name, id and text is made up. Screenshots go to `target/ui-shots/qa903-*.png`; `QA903_EVIDENCE` names a
//! directory that gets a copy of each and the results. Without weston 15, niri or `dbus-daemon` it says so and passes,
//! unless `GPUI_SMOKE_REQUIRE=1`.

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, future, thread};

use gpui::{
    App, AsyncApp, Entity, PlatformInput, ScrollDelta, ScrollEvent, TouchPhase, Window, WindowHandle, WindowingModes,
    point, px,
};
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
    AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE, PRESENCE_BASE,
    PROVISIONING_BASE, TrouterMock, USER_PROFILE_BASE,
};
use teams_proto::chat::Marks;
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::chat_view::ChatView;
use teams_ui::event_form;
use teams_ui::shell::{Main, Shell};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::VirtualInput;
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

const CHILD_ENV: &str = "QA903_CHILD";
const ORIGIN_ENV: &str = "QA903_ORIGIN";
const TROUTER_ENV: &str = "QA903_TROUTER";
const TOKEN_ENV: &str = "QA903_TOKEN";
const DIR_ENV: &str = "QA903_DIR";
const EVIDENCE_ENV: &str = "QA903_EVIDENCE";
/// The file in the app's directory the parent asks the child through (`<n> <verb>`; the answer is `re <n> …`).
const ASK: &str = "qa903-ask";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(40);
/// The longest time a shortcut's change may take to show.
const FIRED: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(100);
const SETTLE: Duration = Duration::from_millis(900);
/// The time between a key and the next, so the app sees each.
const STEP: Duration = Duration::from_millis(200);
/// niri's layout: each window takes the whole screen, and no hot corner.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
const SCREEN: (u32, u32) = (1280, 800);
/// The app's default font size, px.
const DEFAULT_SIZE: u64 = 14;
const TOPIC: &str = "Made-up shortcut review";
const DRAFT: &str = "hello shortcut world";
/// b's messages in the chat: more than the pane shows, so it scrolls.
const MESSAGES: usize = 40;
/// A point over the messages.
const MESSAGES_AT: (u32, u32) = (860, 400);
/// The rail's Settings.
const RAIL_SETTINGS: (u32, u32) = (38, 765);
/// The "Ends" date picker of a new series in the form's window.
const ENDS_PICKER: (u32, u32) = (278, 262);
/// The picker's next month arrow, once it is open.
const NEXT_MONTH: (u32, u32) = (388, 300);
/// The Wednesday of the third week in the picker's month: a day of that month in each month.
const PICKED_DAY: (u32, u32) = (301, 399);

/// A shortcut: the keys pressed, what it must do, the state that shows it did, and an optional screenshot name.
struct Shortcut {
    keys: &'static str,
    what: &'static str,
    done: fn(&Value) -> bool,
    shot: Option<&'static str>,
}

const fn key(keys: &'static str, what: &'static str, done: fn(&Value) -> bool) -> Shortcut {
    Shortcut {
        keys,
        what,
        done,
        shot: None,
    }
}

const fn shot(keys: &'static str, what: &'static str, done: fn(&Value) -> bool, name: &'static str) -> Shortcut {
    Shortcut {
        keys,
        what,
        done,
        shot: Some(name),
    }
}

fn text(state: &Value) -> &str {
    state["box"]["text"].as_str().unwrap_or_default()
}

fn kind(state: &Value) -> &str {
    state["box"]["kind"].as_str().unwrap_or_default()
}

fn font(state: &Value) -> u64 {
    state["font"].as_u64().unwrap_or_default()
}

/// The editor's shortcuts, in the order the user presses them on the draft [`DRAFT`].
const EDITOR: &[Shortcut] = &[
    key("ctrl+x", "Ctrl+X cuts the selection", |s| text(s).is_empty()),
    key("ctrl+v", "Ctrl+V pastes it back", |s| text(s) == DRAFT),
    key("ctrl+z", "Ctrl+Z undoes the paste", |s| text(s) != DRAFT),
    key("ctrl+y", "Ctrl+Y redoes it", |s| text(s) == DRAFT),
    key("ctrl+z", "Ctrl+Z undoes it again", |s| text(s) != DRAFT),
    key("ctrl+shift+z", "Ctrl+Shift+Z redoes it", |s| text(s) == DRAFT),
    key("ctrl+a", "Ctrl+A selects the draft for the marks", |s| {
        s["box"]["selected"] == DRAFT
    }),
    shot("ctrl+b", "Ctrl+B makes it bold", |s| s["box"]["bold"] == true, "bold"),
    key("ctrl+b", "Ctrl+B again removes the bold", |s| s["box"]["bold"] == false),
    key("ctrl+i", "Ctrl+I makes it italic", |s| s["box"]["italic"] == true),
    key("ctrl+i", "Ctrl+I again removes the italic", |s| {
        s["box"]["italic"] == false
    }),
    key("ctrl+u", "Ctrl+U underlines it", |s| s["box"]["underline"] == true),
    key("ctrl+u", "Ctrl+U again removes the underline", |s| {
        s["box"]["underline"] == false
    }),
    key("ctrl+alt+x", "Ctrl+Alt+X strikes it through", |s| {
        s["box"]["strike"] == true
    }),
    key("ctrl+alt+x", "Ctrl+Alt+X again removes the strike", |s| {
        s["box"]["strike"] == false
    }),
    shot(
        "ctrl+alt+1",
        "Ctrl+Alt+1 makes a heading 1",
        |s| kind(s) == "Heading(1)",
        "heading1",
    ),
    key("ctrl+alt+2", "Ctrl+Alt+2 makes a heading 2", |s| {
        kind(s) == "Heading(2)"
    }),
    key("ctrl+alt+3", "Ctrl+Alt+3 makes a heading 3", |s| {
        kind(s) == "Heading(3)"
    }),
    key("ctrl+alt+p", "Ctrl+Alt+P makes a paragraph", |s| kind(s) == "Paragraph"),
    key("ctrl+alt+4", "Ctrl+Alt+4 makes a quote", |s| kind(s) == "Quote"),
    key("ctrl+alt+p", "Ctrl+Alt+P makes the quote a paragraph", |s| {
        kind(s) == "Paragraph"
    }),
    shot(
        "ctrl+shift+8",
        "Ctrl+Shift+8 makes a bulleted list",
        |s| kind(s) == "Bullet",
        "bullets",
    ),
    key("ctrl+alt+p", "Ctrl+Alt+P makes the list a paragraph", |s| {
        kind(s) == "Paragraph"
    }),
    key("ctrl+shift+7", "Ctrl+Shift+7 makes a numbered list", |s| {
        kind(s) == "Numbered"
    }),
    key("ctrl+alt+p", "Ctrl+Alt+P makes the numbered list a paragraph", |s| {
        kind(s) == "Paragraph"
    }),
    key("ctrl+shift+b", "Ctrl+Shift+B makes a code block", |s| kind(s) == "Code"),
    key("ctrl+z", "Ctrl+Z undoes the code block", |s| kind(s) == "Paragraph"),
    shot(
        "ctrl+shift+x",
        "Ctrl+Shift+X expands the box",
        |s| s["box"]["expanded"] == true,
        "expanded",
    ),
    key("ctrl+shift+x", "Ctrl+Shift+X again makes it small", |s| {
        s["box"]["expanded"] == false
    }),
    shot(
        "ctrl+;",
        "Ctrl+; opens the emoji picker",
        |s| s["box"]["emoji"] == true,
        "emoji",
    ),
    key("escape", "Escape closes the emoji picker", |s| {
        s["box"]["emoji"] == false
    }),
    key("ctrl+.", "Ctrl+. opens the emoji picker", |s| s["box"]["emoji"] == true),
    key("escape", "Escape closes it again", |s| s["box"]["emoji"] == false),
];

/// The app's shortcuts, pressed with the draft's chat open.
const APP: &[Shortcut] = &[
    shot(
        "ctrl+1",
        "Ctrl+1 shows Activity",
        |s| s["list"] == "Activity",
        "activity",
    ),
    key("ctrl+alt+u", "Ctrl+Alt+U filters Activity to unread", |s| {
        s["filter"] == "Unread"
    }),
    key("ctrl+alt+m", "Ctrl+Alt+M filters Activity to mentions", |s| {
        s["filter"] == "Mentions"
    }),
    key("ctrl+2", "Ctrl+2 shows Chat", |s| s["list"] == "Chats"),
    shot(
        "ctrl+4",
        "Ctrl+4 shows the calendar",
        |s| s["main"] == "Calendar",
        "calendar",
    ),
    key("ctrl+2", "Ctrl+2 leaves the calendar for the chat", |s| {
        s["main"] == "Chat"
    }),
    shot(
        "ctrl+k",
        "Ctrl+K opens the switcher",
        |s| s["switcher"] == true,
        "switcher",
    ),
    key("escape", "Escape closes the switcher", |s| s["switcher"] == false),
    key("ctrl+e", "Ctrl+E puts the keyboard in the search", |s| {
        s["search"]["focused"] == true && s["search"]["mode"] == "Search"
    }),
    key("escape", "Escape leaves the search", |s| {
        s["search"]["focused"] == false
    }),
    key("ctrl+shift+f", "Ctrl+Shift+F puts the keyboard in the search", |s| {
        s["search"]["focused"] == true && s["search"]["mode"] == "Search"
    }),
    key("escape", "Escape leaves the search again", |s| {
        s["search"]["focused"] == false
    }),
    shot(
        "ctrl+alt+g",
        "Ctrl+Alt+G opens the search's go-to mode",
        |s| s["search"]["focused"] == true && s["search"]["mode"] == "GoTo",
        "goto",
    ),
    key("escape", "Escape leaves the go-to mode", |s| {
        s["search"]["focused"] == false
    }),
    shot(
        "ctrl+=",
        "Ctrl+= makes the font larger",
        |s| font(s) > DEFAULT_SIZE,
        "font-larger",
    ),
    key("ctrl+0", "Ctrl+0 resets the font size", |s| font(s) == DEFAULT_SIZE),
    key("ctrl+-", "Ctrl+- makes the font smaller", |s| font(s) < DEFAULT_SIZE),
    key("ctrl+0", "Ctrl+0 resets it again", |s| font(s) == DEFAULT_SIZE),
    shot(
        "ctrl+n",
        "Ctrl+N opens New message",
        |s| s["main"] == "NewChat",
        "new-message",
    ),
];

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_secondary_keys_window: {error}");
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

/// Notes `finding` unless `ok`; else counts `what` as passed.
fn check(what: &str, ok: bool, finding: impl FnOnce() -> String) {
    if ok {
        step(&format!("pass: {what}"));
        PASSED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(what.to_owned());
    } else {
        let finding = format!("{what}: {}", finding());
        step(&format!("FINDING: {finding}"));
        FINDINGS.lock().unwrap_or_else(PoisonError::into_inner).push(finding);
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
    let Some(sandbox) = sandbox_or_skip("ngteams secondary keys", &options)? else {
        return Ok(());
    };
    let service = Service::start(&runtime)?;
    let dir = tempfile::tempdir()?;
    let mut run = AppRun::launch(&sandbox, &service.env(), dir.path())?;
    check("the app starts with the secondary! bindings", true, String::new);
    let outcome = Walk {
        run: &mut run,
        sandbox: &sandbox,
        chat: &service.chat,
    }
    .all();
    if let Err(error) = outcome {
        return Err(format!("{}{}", run.fail(&sandbox, &*error), sandbox.logs()).into());
    }
    run.quit()?;
    summary()
}

/// Says what passed and what not; an error when anything did not.
fn summary() -> SandboxResult {
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
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("results.txt"), &summary)?;
    }
    if !findings.is_empty() {
        return Err(format!("the walk ended with findings:\n- {}", findings.join("\n- ")).into());
    }
    say("ngteams secondary keys: ok");
    Ok(())
}

// --- the service ---

struct Service {
    mock: ChatServiceMock,
    trouter: TrouterMock,
    chat: String,
}

impl Service {
    /// The mock with a, b and c's made-up group chat and one message of b's in it, posted before a's app starts.
    fn start(runtime: &tokio::runtime::Runtime) -> SandboxResult<Self> {
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        let chat = mock.add_group(TOPIC, &[LAB_C, LAB_A, LAB_B]).ok_or("no group chat")?;
        for n in 0..MESSAGES {
            mock.post_as(LAB_B, &chat, &format!("<p>Made-up note {n} for the scroll.</p>"))
                .ok_or("the post refused")?;
        }
        let _ = mock.take_events(LAB_A);
        Ok(Self { mock, trouter, chat })
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            (ORIGIN_ENV, self.mock.origin()),
            (TROUTER_ENV, self.trouter.origin().to_owned()),
            (TOKEN_ENV, self.mock.token(LAB_A).unwrap_or_default()),
        ]
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
    fn launch(sandbox: &Sandbox, env: &[(&'static str, String)], dir: &Path) -> SandboxResult<Self> {
        let dir_text = dir.to_string_lossy().into_owned();
        let mut vars: Vec<(&str, &str)> = vec![(CHILD_ENV, "app"), (DIR_ENV, &dir_text)];
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

    fn state(&mut self) -> SandboxResult<Value> {
        let answer = self.question("state")?;
        serde_json::from_str(&answer).map_err(|error| format!("state {answer}: {error}").into())
    }

    /// The state once `wanted` takes it, or the last state after `limit`.
    fn until(&mut self, limit: Duration, wanted: impl Fn(&Value) -> bool) -> SandboxResult<(bool, Value)> {
        let start = Instant::now();
        loop {
            let state = self.state()?;
            if wanted(&state) {
                return Ok((true, state));
            }
            if start.elapsed() > limit {
                return Ok((false, state));
            }
            thread::sleep(POLL);
        }
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

    fn click(&mut self, (x, y): (u32, u32)) -> SandboxResult {
        self.input.move_to(x, y)?;
        thread::sleep(STEP);
        self.input.click(x, y, BUTTON_LEFT)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn type_text(&mut self, text: &str) -> SandboxResult {
        self.input.type_text(text)?;
        thread::sleep(STEP);
        Ok(())
    }

    /// A screenshot as `qa903-<name>.png`, the pointer at rest, under `target/ui-shots/` and the evidence dir.
    fn shot(&mut self, sandbox: &Sandbox, name: &str) -> SandboxResult<PathBuf> {
        self.park()?;
        thread::sleep(SETTLE);
        let file = format!("qa903-{name}.png");
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

/// The left button's evdev code.
const BUTTON_LEFT: u32 = 0x110;

// --- the walk ---

struct Walk<'a> {
    run: &'a mut AppRun,
    sandbox: &'a Sandbox,
    chat: &'a str,
}

impl Walk<'_> {
    fn all(&mut self) -> SandboxResult {
        self.run.ok(&format!("open {}", self.chat))?;
        let chat = self.chat.to_owned();
        let (open, state) = self.run.until(WAIT, |state| state["chat"] == chat.as_str())?;
        if !open {
            return Err(format!("the chat never opened: {state}").into());
        }
        self.run.ok("focus-box")?;
        self.run.type_text(DRAFT)?;
        let (typed, state) = self.run.until(FIRED, |state| text(state) == DRAFT)?;
        if !typed {
            return Err(format!("the draft was not typed: {state}").into());
        }
        self.copy()?;
        self.shortcuts(EDITOR)?;
        self.scrolling()?;
        self.shortcuts(APP)?;
        self.settings_first_open()?;
        self.date_picker()
    }

    /// The messages scrolled up and back down with the wheel, then with a touchpad.
    fn scrolling(&mut self) -> SandboxResult {
        let up = |state: &Value| state["scrolled_up"] == true;
        let down = |state: &Value| state["scrolled_up"] == false;
        let (newest, state) = self.run.until(FIRED, down)?;
        check("the chat opens at its newest message", newest, || {
            format!("state {state}")
        });
        self.run.input.scroll(MESSAGES_AT.0, MESSAGES_AT.1, -10)?;
        let (done, state) = self.run.until(FIRED, up)?;
        check("the wheel scrolls the messages up", done, || format!("state {state}"));
        self.run.shot(self.sandbox, "wheel-up")?;
        self.run.input.scroll(MESSAGES_AT.0, MESSAGES_AT.1, 80)?;
        let (done, state) = self.run.until(FIRED, down)?;
        check("the wheel scrolls them back to the newest", done, || {
            format!("state {state}")
        });
        self.run.ok("touchpad 600")?;
        let (done, state) = self.run.until(FIRED, up)?;
        check("a touchpad scrolls the messages up", done, || format!("state {state}"));
        self.run.shot(self.sandbox, "touchpad-up")?;
        self.run.ok("touchpad -8000")?;
        let (done, state) = self.run.until(FIRED, down)?;
        check("a touchpad scrolls them back to the newest", done, || {
            format!("state {state}")
        });
        Ok(())
    }

    /// The rail's Settings clicked for the first time: the General page, its title at the top.
    fn settings_first_open(&mut self) -> SandboxResult {
        self.run.click(RAIL_SETTINGS)?;
        let (open, state) = self
            .run
            .until(WAIT, |state| state["main"] == "Settings" && state["page"] == "General")?;
        check("the rail's Settings opens the General page", open, || {
            format!("state {state}")
        });
        let png = self.run.shot(self.sandbox, "settings-first-open")?;
        let shot = image::open(&png)?.to_rgb8();
        let (title, beside) = title_ink(&shot);
        check(
            "the General page shows from its top on its first open",
            title > 20 && beside == 0,
            || {
                format!(
                    "ink at the title {title}, beside it {beside}: the page is scrolled, see {}",
                    png.display()
                )
            },
        );
        Ok(())
    }

    /// A new event as a series; its "Ends" picker opened and a day picked.
    fn date_picker(&mut self) -> SandboxResult {
        self.run.ok("event-new")?;
        let (shown, state) = self.run.until(WAIT, |state| state["event"].is_object())?;
        if !shown {
            return Err(format!("the event form never showed: {state}").into());
        }
        self.run.ok("event-series")?;
        let (series, state) = self.run.until(WAIT, |state| state["event"]["repeats"].is_string())?;
        check("the form makes the event a series", series, || format!("state {state}"));
        self.run.shot(self.sandbox, "event-series")?;
        self.run.click(ENDS_PICKER)?;
        self.run.shot(self.sandbox, "ends-open")?;
        self.run.click(NEXT_MONTH)?;
        self.run.shot(self.sandbox, "ends-next-month")?;
        self.run.click(PICKED_DAY)?;
        let (picked, state) = self.run.until(FIRED, |state| {
            state["event"]["repeats"]
                .as_str()
                .is_some_and(|repeats| repeats.contains(" until "))
        })?;
        check("a day picked in the Ends picker ends the series", picked, || {
            format!("state {state}")
        });
        self.run.shot(self.sandbox, "ends-picked")?;
        Ok(())
    }

    /// Ctrl+A and Ctrl+C: the draft selected and on the clipboard.
    fn copy(&mut self) -> SandboxResult {
        self.run.press("ctrl+a")?;
        let (selected, state) = self.run.until(FIRED, |state| state["box"]["selected"] == DRAFT)?;
        check("Ctrl+A selects the draft", selected, || format!("state {state}"));
        self.sandbox
            .set_clipboard(vec![("text/plain;charset=utf-8".to_owned(), b"nothing yet".to_vec())])?;
        self.run.press("ctrl+c")?;
        let copied = self.clipboard_text(DRAFT);
        check("Ctrl+C copies the selection", copied == DRAFT, || {
            format!("the clipboard holds {copied:?}")
        });
        Ok(())
    }

    /// What the sandbox's clipboard holds as text once it is `wanted`, or after a while what it holds then.
    fn clipboard_text(&self, wanted: &str) -> String {
        let read = || {
            self.sandbox
                .read_clipboard()
                .ok()?
                .into_iter()
                .find(|(kind, _)| kind.starts_with("text/plain"))
                .map(|(_, bytes)| String::from_utf8_lossy(&bytes).into_owned())
        };
        wait_until(Duration::from_secs(3), || read().filter(|text| text == wanted))
            .or_else(read)
            .unwrap_or_default()
    }

    fn shortcuts(&mut self, list: &[Shortcut]) -> SandboxResult {
        for shortcut in list {
            self.run.press(shortcut.keys)?;
            let (done, state) = self.run.until(FIRED, shortcut.done)?;
            check(shortcut.what, done, || format!("state {state}"));
            if let Some(name) = shortcut.shot {
                self.run.shot(self.sandbox, name)?;
            }
        }
        Ok(())
    }
}

/// The ink of the page title "General" at its place at the page's top, and the ink right of it, at the default font
/// size in Teams Light: a page scrolled down shows a row's text there instead.
fn title_ink(shot: &image::RgbImage) -> (usize, usize) {
    let background = shot.get_pixel(1200, 90).0;
    let ink = |x0: u32, x1: u32| {
        (x0..x1)
            .flat_map(|x| (76..100).map(move |y| (x, y)))
            .filter(|&(x, y)| {
                let pixel = shot.get_pixel(x, y).0;
                pixel
                    .iter()
                    .zip(background)
                    .map(|(seen, under)| u32::from(seen.abs_diff(under)))
                    .sum::<u32>()
                    > 120
            })
            .count()
    };
    (ink(344, 430), ink(460, 1100))
}

// --- the child: a's app on the mock ---

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

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let dir = PathBuf::from(std::env::var(DIR_ENV)?);
    let engine = engine_on_the_mock(&dir)?;
    let first = support::started(Alias::A, engine)?;
    let runner = support::runner(&dir)?;
    let settings = teams_ui::SettingsFile::new(dir.join("ui").join("a.toml"));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
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
        "state" => state_of(shell, window, cx).to_string(),
        "touchpad" => {
            touchpad(rest.parse::<f32>().unwrap_or_default(), window, cx);
            "ok".to_owned()
        }
        "event-new" => {
            let calendar = shell.read(cx).calendar_view().clone();
            calendar.update(cx, |calendar, cx| {
                let slot = calendar.next_slot();
                calendar.new_event(slot, false, cx);
            });
            "ok".to_owned()
        }
        "event-series" => {
            let (Some(form), Some(handle)) = (event_form::shown(cx), event_form::window(cx)) else {
                return "no event form".to_owned();
            };
            let _ = handle.update(cx, |_, window, cx| {
                form.update(cx, |form, cx| form.choose_series(true, window, cx));
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
        other => format!("unknown {other}"),
    }
}

/// A touchpad's two-finger scroll of `dy` pixels over the messages, in ten moves, as GPUI's Wayland client reports
/// one (positive: toward the oldest).
fn touchpad(dy: f32, window: &mut Window, cx: &mut App) {
    let position = point(px(860.0), px(400.0));
    let phases = std::iter::once((TouchPhase::Started, 0.0))
        .chain(std::iter::repeat_n((TouchPhase::Moved, dy / 10.0), 10))
        .chain(std::iter::once((TouchPhase::Ended, 0.0)));
    for (touch_phase, step) in phases {
        window.dispatch_event(
            PlatformInput::Scroll(ScrollEvent {
                position,
                delta: ScrollDelta::Pixels(point(px(0.0), px(step))),
                touch_phase,
                ..ScrollEvent::default()
            }),
            cx,
        );
    }
}

/// What the shortcuts change: the place the shell shows, the switcher, the search, the font size, the Activity
/// filter and the compose box of the open chat.
fn state_of(shell: &Entity<Shell>, window: &Window, cx: &App) -> Value {
    let shell_ref = shell.read(cx);
    let main = match shell_ref.main() {
        Main::Chat(id) => json!({"main": "Chat", "chat": id}),
        other => json!({"main": format!("{other:?}")}),
    };
    let search = shell_ref.search().read(cx);
    let mut state = json!({
        "main": main["main"],
        "chat": main["chat"],
        "list": format!("{:?}", shell_ref.list_place()),
        "filter": format!("{:?}", shell_ref.activity_view().read(cx).filter()),
        "switcher": shell_ref.switcher().is_some(),
        "search": {
            "focused": search.holds_focus(window, cx),
            "mode": format!("{:?}", search.mode()),
        },
        "font": shell_ref.settings().read(cx).settings().appearance.font_size,
        "page": format!("{:?}", shell_ref.settings_view().read(cx).page()),
        "event": event_form::shown(cx).map(|form| json!({"repeats": form.read(cx).printable(cx).repeats})),
    });
    if let Some(chat) = chat_view(shell, cx) {
        state["box"] = box_state(&chat, cx);
        state["scrolled_up"] = json!(chat.read(cx).scrolled_up());
    }
    state
}

fn box_state(chat: &Entity<ChatView>, cx: &App) -> Value {
    let compose = chat.read(cx).compose().read(cx);
    let editor = compose.editor().read(cx);
    json!({
        "text": editor.text(),
        "selected": editor.selected_text(),
        "bold": editor.has_mark(Marks::BOLD),
        "italic": editor.has_mark(Marks::ITALIC),
        "underline": editor.has_mark(Marks::UNDERLINE),
        "strike": editor.has_mark(Marks::STRIKE),
        "kind": format!("{:?}", editor.block_kind()),
        "expanded": compose.is_expanded(),
        "emoji": compose.emoji_picker().is_some(),
    })
}
