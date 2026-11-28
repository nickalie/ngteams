//! QA of task 929, in the UI sandbox: the theme picker in Settings and the settings file of the app, across
//! restarts (`harness = false`: GPUI runs on the process's main thread).
//!
//! The child is the app's desktop window over the fake engine, with the app's own settings model over a settings
//! file in a temporary directory (`ngteams::app::settings`). The parent clicks the theme tiles where a user sees them
//! and asks the child, through a file in the sandbox dir, which theme draws the window and what the settings and the
//! file hold.
//!
//! - **A file of an earlier release** (synthetic, [`OLD_FILE`]): it loads with no problem and is not written at
//!   start. Each theme tile applies its theme and writes it to the file. A save keeps every key of the file and every
//!   other value.
//! - **A restart** over the saved file: the last theme, and every other value, come back. "Match the system" on and
//!   off again.
//! - **A damaged file**: the defaults, one problem, and the file is never written over.
//! - **No file**: the defaults and no problem.
//!
//! `QA929_EVIDENCE` names a directory that gets the screenshots, the files and the states. Screenshots also go to
//! `target/ui-shots/qa929-*.png`. Every value is made up.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the shared fake engine of the view tests (test code, which unwraps): its test-context rig and trace \
              capture serve them, not a window; this binary runs without the test harness, so not as `cfg(test)`"
)]
#[path = "../../../../teams-ui/tests/common/mod.rs"]
mod common;

use std::collections::BTreeSet;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};
use std::{fs, mem, thread};

use common::{FakeCore, FakeEvents};
use gpui::{App, AppContext as _, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use serde_json::{Value, json};
use teams_ui::desktop::{Desktop, DesktopParts};
use teams_ui::settings::{Settings, SettingsFile};
use teams_ui::settings_view::Page;
use teams_ui::shell::Shell;
use teams_ui::theme::THEMES;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use teams_ui::{Bridge, Models, ShellParts, ViewTrace};
use tokio::sync::mpsc;
use ui_sandbox::input::keys;
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

/// The child's settings directory; the parent runs without it.
const CHILD_ENV: &str = "QA929_CHILD";
const EVIDENCE_ENV: &str = "QA929_EVIDENCE";
/// The file in the sandbox dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "qa929-ask";
const APP_ID: &str = "ngteams-qa-theme";
const FILE: &str = "settings.toml";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(100);
const SETTLE: Duration = Duration::from_millis(700);
const READY_FRAMES: usize = 30;

/// The middle of the first theme tile with one theme chosen, at the old file's font size 17, and the step to the
/// next tile (one row of seven).
const FIRST_TILE: (u32, u32) = (415, 295);
const TILE_STEP: u32 = 109;
/// The "Match the system" switch, at font size 17.
const FOLLOW_SWITCH: (u32, u32) = (1125, 209);
/// The same switch below the warning box of a damaged file after a save was held (two problems).
const FOLLOW_SWITCH_BELOW_PROBLEMS: (u32, u32) = (1135, 294);
/// The first run's Confirm, in its dialog over the window.
const CONFIRM: (u32, u32) = (639, 534);
/// "Keep AI features on", in the AI notice that comes after the first run's dialog.
const KEEP_AI: (u32, u32) = (789, 541);

/// A settings file of the release before the move (main at 4edd10db), with a value other than the default for each
/// setting where one is simple to write. The theme is Solarized Dark.
const OLD_FILE: &str = r#"selected_chat = "19:qa929-chat@thread.v2"
send_typing = false
send_read_receipts = false
send_presence = false
video_calls = false
save_call_captions = true
meeting_started_notification = false
meeting_reminders = false
first_run = "confirmed"
ai_features = false
notifications = "mentions"
idle_minutes = 15
manual_status = "do-not-disturb"
log_level = "debug"
microphone = "alsa_input.qa929-headset"
camera = "qa929-camera"
video_effect = "blur"
follow_system_theme = false
theme = "solarized-dark"
light_theme = "solarized-light"
dark_theme = "high-contrast"
font_size = 17
animations = false
time_format = "24-hour"
caption_translate_to = "de"
caption_translator = { tool = "codex", model = "qa929-captions" }
recap_summarizer = { tool = "claude", model = "default" }
first_account = "8:orgid:00000000-0000-4000-8000-00000000929a"
chat_translator = { tool = "codex", model = "qa929-model" }
message_corrector = { tool = "claude", model = "default" }
message_translate_to = "German"
draft_translate_to = "French"
message_translator = { tool = "codex", model = "recommended" }
compose_hidden_tools = ["emoji"]
compose_shown_tools = ["translate"]
sound_pack = "builtin/default"
sounds_while_busy = false
sounds_volume = 40

[accounts."8:orgid:00000000-0000-4000-8000-00000000929b"]
selected_chat = "19:qa929-other@thread.v2"
manual_status = "busy"

[[people_calendars]]
account = "8:orgid:00000000-0000-4000-8000-00000000929a"
address = "avery@example.com"
name = "Avery Example"

[[chat_languages]]
account = "8:orgid:00000000-0000-4000-8000-00000000929a"
chat = "19:qa929-chat@thread.v2"
language = "de"

[message_bar."8:orgid:00000000-0000-4000-8000-00000000929a"]
hidden = ["reply"]
shown = ["copy"]

[noise_suppression]
"8:orgid:00000000-0000-4000-8000-00000000929a" = "high"

[caption_height]
"8:orgid:00000000-0000-4000-8000-00000000929a" = 420
"#;

const DAMAGED_FILE: &str = "theme = \"nord\"\nfont_size = [[[ not toml\n";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(dir) => child(PathBuf::from(dir)),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_theme_settings_window: {error}");
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

// --- the parent: the user's eyes and hands ---

/// One run of the app over a settings directory.
struct Run<'a> {
    sandbox: &'a Sandbox,
    running: Option<Running>,
    asked: u32,
    evidence: Option<PathBuf>,
}

impl<'a> Run<'a> {
    fn start(sandbox: &'a Sandbox, dir: &Path, evidence: Option<&Path>) -> SandboxResult<Self> {
        let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, &dir.to_string_lossy())])?;
        let mut running = Running::new(child)?;
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
        Ok(Self {
            sandbox,
            running: Some(running),
            asked: 0,
            evidence: evidence.map(Path::to_owned),
        })
    }

    fn running(&mut self) -> SandboxResult<&mut Running> {
        self.running.as_mut().ok_or_else(|| "the app is not running".into())
    }

    fn ask(&mut self, verb: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = self.sandbox.path(&format!("{ASK}.staged"));
        fs::write(&staged, format!("{} {verb}", self.asked))?;
        fs::rename(&staged, self.sandbox.path(ASK))?;
        let prefix = format!("re {} ", self.asked);
        let line = self
            .running()?
            .line_where(WAIT, |line| line.starts_with(&prefix))
            .map_err(|error| format!("no answer to {verb}: {error:?}"))?;
        Ok(line[prefix.len()..].to_owned())
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
                let last = json!({"active": state["active"], "problems": state["problems"], "file": state["file"]});
                return Err(format!("{what}: never came; last {last}").into());
            }
            thread::sleep(POLL);
        }
    }

    fn click(&mut self, (x, y): (u32, u32)) -> SandboxResult {
        self.sandbox.input()?.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn shot(&mut self, name: &str) -> SandboxResult {
        self.sandbox.input()?.move_to(5, 790)?;
        thread::sleep(SETTLE);
        let file = format!("qa929-{name}.png");
        let png = self.sandbox.screenshot(&file, false)?;
        fs::copy(&png, shots_dir()?.join(&file))?;
        if let Some(dir) = &self.evidence {
            fs::copy(&png, dir.join(&file))?;
        }
        Ok(())
    }

    fn keep(&self, name: &str, text: &str) -> SandboxResult {
        if let Some(dir) = &self.evidence {
            fs::write(dir.join(name), text)?;
        }
        Ok(())
    }

    fn quit(mut self) -> SandboxResult {
        let _ = self.ask("quit")?;
        let running = self.running.take().ok_or("the app is not running")?;
        let (status, stderr) = running.finish(WAIT)?;
        ensure(status.success(), || format!("the app exited {status}: {stderr}"))
    }
}

fn text(state: &Value, key: &str) -> String {
    state[key].as_str().unwrap_or_default().to_owned()
}

fn file_keys(text: &str) -> SandboxResult<BTreeSet<String>> {
    let table: toml::Table = text.parse()?;
    Ok(table.keys().cloned().collect())
}

fn tile(index: usize) -> (u32, u32) {
    let step = u32::try_from(index).unwrap_or_default() * TILE_STEP;
    (FIRST_TILE.0 + step, FIRST_TILE.1)
}

/// A check after the first run, which reads what the first run saved.
type Check<'a> = Box<dyn Fn() -> SandboxResult + 'a>;

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa929 theme settings window", &options)? else {
        return Ok(());
    };
    let evidence = std::env::var_os(EVIDENCE_ENV).map(PathBuf::from);
    if let Some(dir) = &evidence {
        fs::create_dir_all(dir)?;
    }
    let home = tempfile::tempdir()?;
    let mut failures = Vec::new();
    let saved = match old_file_and_each_theme(&sandbox, home.path(), evidence.as_deref()) {
        Ok(saved) => {
            say("check old file and each theme: ok");
            Some(saved)
        }
        Err(error) => {
            say(&format!("check old file and each theme: FAILED: {error}"));
            failures.push(format!("old file and each theme: {error}"));
            None
        }
    };
    let checks: [(&str, Check<'_>); 3] = [
        (
            "restart",
            Box::new(|| restart(&sandbox, home.path(), saved.as_ref(), evidence.as_deref())),
        ),
        (
            "damaged file",
            Box::new(|| damaged(&sandbox, home.path(), evidence.as_deref())),
        ),
        (
            "no file",
            Box::new(|| missing(&sandbox, home.path(), evidence.as_deref())),
        ),
    ];
    for (name, check) in checks {
        match check() {
            Ok(()) => say(&format!("check {name}: ok")),
            Err(error) => {
                say(&format!("check {name}: FAILED: {error}"));
                failures.push(format!("{name}: {error}"));
            }
        }
    }
    if failures.is_empty() {
        say("qa929 theme settings window: ok");
        Ok(())
    } else {
        Err(format!(
            "{} check(s) failed:\n{}{}",
            failures.len(),
            failures.join("\n"),
            sandbox.logs()
        )
        .into())
    }
}

/// What the first run left: the file's text and the settings, as the child shows them.
struct Saved {
    file: String,
    settings: String,
}

fn old_file_and_each_theme(sandbox: &Sandbox, home: &Path, evidence: Option<&Path>) -> SandboxResult<Saved> {
    let dir = home.join("old");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join(FILE), OLD_FILE)?;
    let mut run = Run::start(sandbox, &dir, evidence)?;
    let loaded = run.state()?;
    run.keep("old-loaded-settings.txt", &text(&loaded, "settings"))?;
    ensure(loaded["problems"] == json!([]), || {
        format!("problems: {}", loaded["problems"])
    })?;
    ensure(text(&loaded, "active") == "solarized-dark", || {
        format!("the window is drawn in {}", loaded["active"])
    })?;
    ensure(text(&loaded, "file") == OLD_FILE, || {
        "the start wrote the file".to_owned()
    })?;
    let _ = run.ask("page appearance")?;
    run.shot("picker-solarized-dark")?;
    for (index, spec) in THEMES.iter().enumerate() {
        run.click(tile(index))?;
        let key = spec.key;
        let line = format!("theme = \"{key}\"");
        let state = run.until(&format!("{key} applied and saved"), |state| {
            text(state, "active") == key && text(state, "file").lines().any(|kept| kept == line)
        })?;
        say(&format!("theme {key}: applied and saved"));
        if index == 0 {
            run.keep("saved-after-first-theme.toml", &text(&state, "file"))?;
        }
        run.shot(&format!("picker-{key}"))?;
    }
    let nord = THEMES.iter().position(|spec| spec.key == "nord").ok_or("no nord")?;
    run.click(tile(nord))?;
    let state = run.until("nord applied and saved", |state| {
        text(state, "active") == "nord" && text(state, "file").contains("theme = \"nord\"")
    })?;
    let file = text(&state, "file");
    let settings = text(&state, "settings");
    run.keep("saved-after-nord.toml", &file)?;
    run.keep("saved-after-nord-settings.txt", &settings)?;
    let (old_keys, new_keys) = (file_keys(OLD_FILE)?, file_keys(&file)?);
    ensure(old_keys == new_keys, || {
        format!(
            "the save changed the keys: lost {:?}, added {:?}",
            old_keys.difference(&new_keys).collect::<Vec<_>>(),
            new_keys.difference(&old_keys).collect::<Vec<_>>()
        )
    })?;
    let expected = text(&loaded, "settings").replace("theme: \"solarized-dark\"", "theme: \"nord\"");
    ensure(settings == expected, || {
        "a value other than the theme changed".to_owned()
    })?;
    run.quit()?;
    Ok(Saved { file, settings })
}

fn restart(sandbox: &Sandbox, home: &Path, saved: Option<&Saved>, evidence: Option<&Path>) -> SandboxResult {
    let saved = saved.ok_or("the first run failed")?;
    let mut run = Run::start(sandbox, &home.join("old"), evidence)?;
    let state = run.state()?;
    ensure(text(&state, "active") == "nord", || {
        format!("restart drew {}", state["active"])
    })?;
    ensure(text(&state, "settings") == saved.settings, || {
        "a value changed across the restart".to_owned()
    })?;
    ensure(text(&state, "file") == saved.file, || {
        "the start wrote the file".to_owned()
    })?;
    let _ = run.ask("page appearance")?;
    run.shot("restart-nord")?;
    run.click(FOLLOW_SWITCH)?;
    let following = run.until("Match the system on", |state| {
        text(state, "file").contains("follow_system_theme = true")
    })?;
    let active = text(&following, "active");
    say(&format!("following the system: {active}"));
    ensure(active == "solarized-light" || active == "high-contrast", || {
        format!("following the system drew {active}")
    })?;
    run.shot("restart-follow-system")?;
    run.click(FOLLOW_SWITCH)?;
    let _ = run.until("Match the system off", |state| {
        text(state, "active") == "nord" && text(state, "file").contains("follow_system_theme = false")
    })?;
    let back = run.state()?;
    ensure(text(&back, "settings") == saved.settings, || {
        "the switch changed another value".to_owned()
    })?;
    run.keep("restart-settings.txt", &text(&back, "settings"))?;
    run.quit()
}

fn damaged(sandbox: &Sandbox, home: &Path, evidence: Option<&Path>) -> SandboxResult {
    let dir = home.join("damaged");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join(FILE), DAMAGED_FILE)?;
    let mut run = Run::start(sandbox, &dir, evidence)?;
    let state = run.state()?;
    run.keep("damaged-state.json", &serde_json::to_string_pretty(&state)?)?;
    ensure(text(&state, "settings") == text(&state, "default"), || {
        "a damaged file did not give the defaults".to_owned()
    })?;
    let problems = state["problems"].as_array().map_or(0, Vec::len);
    ensure(problems == 1, || format!("problems: {}", state["problems"]))?;
    let _ = run.ask("page general")?;
    run.shot("damaged-general")?;
    let _ = run.ask("page appearance")?;
    run.shot("damaged-appearance")?;
    run.click(CONFIRM)?;
    run.shot("damaged-confirmed")?;
    run.click(KEEP_AI)?;
    run.click(FOLLOW_SWITCH_BELOW_PROBLEMS)?;
    let changed = run.until("Match the system off", |state| {
        !text(state, "settings").contains("follow_system: true")
    })?;
    run.shot("damaged-one-theme")?;
    ensure(text(&changed, "file") == DAMAGED_FILE, || {
        "the damaged file was written over".to_owned()
    })?;
    run.quit()
}

fn missing(sandbox: &Sandbox, home: &Path, evidence: Option<&Path>) -> SandboxResult {
    let dir = home.join("none");
    fs::create_dir_all(&dir)?;
    let mut run = Run::start(sandbox, &dir, evidence)?;
    let state = run.state()?;
    run.keep("missing-state.json", &serde_json::to_string_pretty(&state)?)?;
    ensure(text(&state, "settings") == text(&state, "default"), || {
        "no file did not give the defaults".to_owned()
    })?;
    ensure(state["problems"] == json!([]), || {
        format!("problems: {}", state["problems"])
    })?;
    ensure(state["file"].is_null(), || "the start wrote a file".to_owned())?;
    let _ = run.ask("page appearance")?;
    run.shot("missing-appearance")?;
    run.click(CONFIRM)?;
    run.shot("missing-confirmed")?;
    run.click(KEEP_AI)?;
    run.shot("missing-ai-kept")?;
    run.quit()
}

// --- the child: the app's window over its settings file ---

fn child(dir: PathBuf) -> SandboxResult {
    ui_sandbox::window_allowed()?;
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| match open(&dir, cx) {
        Ok((window, desktop)) => answer(window, desktop, dir.join(FILE), cx),
        Err(error) => {
            eprintln!("failed to open the window: {error:#}");
            cx.quit();
        }
    });
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

fn open(dir: &Path, cx: &mut App) -> gpui::Result<(WindowHandle<KitRoot>, Entity<Desktop>)> {
    let core: Arc<dyn teams_ui::Core> = Arc::new(FakeCore::default());
    let (events, received) = mpsc::unbounded_channel();
    let source = FakeEvents {
        events: received,
        shutdowns: Arc::new(AtomicUsize::new(0)),
    };
    let models = Models::new(cx);
    let drained = models.clone();
    let bridge = cx.new(|cx| Bridge::start(source, core, drained, cx));
    let settings = ngteams::app::settings(SettingsFile::new(dir.join(FILE)), None);
    let shell = ShellParts {
        core: bridge.read(cx).core(),
        models,
        settings: cx.new(|_| settings),
        trace: Arc::new(ViewTrace::off()),
    };
    let desktop = cx.new(|cx| {
        Desktop::new(
            DesktopParts {
                app_id: APP_ID,
                app_name: "Teams (QA)".into(),
                title: "Teams (QA)".into(),
                shell,
                account_name: "Lab A".into(),
                account_detail: "synthetic account".into(),
                account_server: None,
                can_add: false,
                ringer: None,
                pictures_dir: None,
            },
            cx,
        )
    });
    let window = desktop.update(cx, Desktop::open_window)?;
    mem::forget((bridge, events));
    window.update(cx, |_, window, _| {
        after_frames(window, READY_FRAMES, |_, _| say("ready"));
    })?;
    Ok((window, desktop))
}

fn answer(window: WindowHandle<KitRoot>, desktop: Entity<Desktop>, file: PathBuf, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let ask = PathBuf::from(dir).join(ASK);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(20)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let (seq, verb) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            if verb == "quit" {
                say(&format!("re {seq} ok"));
                cx.update(|cx| cx.quit());
                break;
            }
            let reply = window
                .update(cx, |kit, window, cx| on_window(kit, &desktop, &file, verb, window, cx))
                .unwrap_or_else(|_| "no window".to_owned());
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn shell_of(kit: &KitRoot, cx: &App) -> Option<Entity<Shell>> {
    kit.view()
        .clone()
        .downcast::<Root>()
        .ok()?
        .read(cx)
        .shell_in(cx)
        .cloned()
}

fn on_window(
    kit: &KitRoot,
    desktop: &Entity<Desktop>,
    file: &Path,
    verb: &str,
    window: &mut Window,
    cx: &mut App,
) -> String {
    match verb {
        "state" => state_of(desktop, file, cx).to_string(),
        "page appearance" | "page general" => {
            let page = if verb.ends_with("appearance") {
                Page::Appearance
            } else {
                Page::General
            };
            let Some(shell) = shell_of(kit, cx) else {
                return "no shell".to_owned();
            };
            shell.update(cx, |shell, cx| shell.open_settings_page(page, window, cx));
            "ok".to_owned()
        }
        other => format!("unknown verb {other}"),
    }
}

fn state_of(desktop: &Entity<Desktop>, file: &Path, cx: &App) -> Value {
    let model = desktop.read(cx).settings().read(cx);
    json!({
        "active": teams_ui::theme::active(cx).key,
        "settings": format!("{:#?}", model.settings()),
        "default": format!("{:#?}", Settings::default()),
        "problems": model.problems().iter().map(ToString::to_string).collect::<Vec<_>>(),
        "file": fs::read_to_string(file).ok(),
    })
}
