//! Translating the message being written, as a user reaches it: the real app in the UI sandbox, on the mock, driven
//! by the sandbox's own pointer and keyboard (`harness = false`: GPUI runs on the process's main thread).
//!
//! The test runs itself again inside the sandbox as the app of lab account a, its engine on the mock chat service and
//! its Trouter. The translating tool is a made-up `claude` ([`FAKE_CLAUDE`]) first on the app's `PATH`, which holds no
//! directory with a real `claude` or `codex` (the child checks that before it starts): the app's own engine runs it,
//! so the tool's command line, its input and its process are the app's. It answers German and French from a small
//! dictionary, logs every question it is asked, and holds or fails while the parent's files say so. Before the engine
//! starts, a's own message in the a–b chat gets a mention, a link showing its address, a bold word, an emoji, inline
//! code and a line break, to be edited.
//!
//! The child opens views where a user's click on a list row would (a chat, the team's channel, a post's thread) and
//! answers the parent's questions about the boxes through a file in the sandbox dir; everything else comes from the
//! sandbox's virtual pointer and keyboard. Buttons are found on the screen: Send's lit disc, the language button left
//! of it by the pointer resting on it (a ghost button's hover), Translate right against that, the line over the box by
//! its dark text, a menu's row by the keys' highlight, the settings' rows from their selected segment.
//!
//! - **The language menu**: the language button opens it with the language chosen first; typing filters it, Enter or
//!   a click picks one, any other language can be typed and is offered as "Use …"; Escape closes it.
//! - **Translate** by the button and by Ctrl+Shift+Y in a chat's box, a reply, an edit, the channel's post box, a
//!   thread's reply box and New message: the draft replaced in place, only the draft's words and the language sent,
//!   Ctrl+Z bringing the draft back. Mentions, links, bold, emoji, line breaks and code stay as they were.
//! - **Progress and cancel**: the line over the box while it runs; Cancel, Escape and the button again stop it, its
//!   process ends and the draft stays. **Failure** keeps the draft and Try again translates it. An edit while it runs
//!   drops the result; Translate again translates the new draft. Correct and Translate never run at once. An empty
//!   draft starts nothing; a draft of only a mention has no words to translate.
//! - **One language for every box**: the a–c chat's box shows it too.
//! - **After a restart**, without the tool on the `PATH`: the language is the one chosen; Translate says no tool is
//!   installed, Open settings opens Settings → General, and once the tool is there Try again translates and lists the
//!   models it offers. In Settings →
//!   General → Message translation: the language by its menu, the Model dropdown, Codex and then Claude chosen, a
//!   model's name typed under "Other model…" and the tool then started with it, and Codex chosen again (not
//!   installed: said so in the box, "Install it").
//! - **A narrow box**: the post box beside a thread compacts its toolbar and keeps its placeholder on one line.
//!
//! Screenshots are kept under `target/ui-shots/translate-*.png`, in Teams Light and Dark.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, Write as _};
use std::ops::Range;
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
use support::Alias;
use support::display_name;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_mock::{
    AB_CHAT, AC_CHAT, AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE,
    PRESENCE_BASE, PROVISIONING_BASE, TEAM_CHANNEL, TrouterMock, USER_PROFILE_BASE,
};
use teams_proto::Mri;
use teams_proto::chat::Marks;
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::compose::{Compose, Mode};
use teams_ui::draft_translate::menu::Row;
use teams_ui::shell::{Main, Shell};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::{Modifier, VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

/// Which mode the child runs in.
const CHILD_ENV: &str = "NGTEAMS_TRANSLATE_DRAFT_TEST";
/// The settings file the child's app keeps, the same across both runs.
const SETTINGS_ENV: &str = "NGTEAMS_TRANSLATE_DRAFT_SETTINGS";
/// The directory the made-up tool is in, the only one on the child's `PATH` with a `claude`.
const TOOLS_ENV: &str = "NGTEAMS_TRANSLATE_DRAFT_TOOLS";
/// The file in the sandbox dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "translate-ask";
/// From the child's start to its `ready` (the mock started, the window drawn).
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(20);
/// How often the parent looks again while it waits for a state.
const POLL: Duration = Duration::from_millis(40);
/// How long the views get to settle before a screenshot.
const SETTLE: Duration = Duration::from_millis(800);
/// How long the parent watches for something that must not happen.
const QUIET: Duration = Duration::from_millis(1200);
/// A toolbar button's side.
const TOOL: u32 = 28;
/// How long a hover takes to draw.
const HOVER: Duration = Duration::from_millis(350);

/// The draft typed, and what the made-up tool makes of it.
const DRAFT: &str = "hello team, please send the notes";
const GERMAN: &str = "hallo Mannschaft, bitte schicke die Notizen";
const FRENCH: &str = "bonjour équipe, svp envoie les notes";
/// The mock's team channel's post with replies under it.
const CHANNEL_POST: &str = "1760000300000";
/// What a's own message to be edited says, as the store loads it: words around a mention of b, a bold word, an emoji,
/// a line break, a link showing its address and inline code.
const OWN_HTML: &str = "<p>hello <b>team</b> \u{1f600} please send the notes to \
                        <span itemscope=\"\" itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Lab B</span>\
                        <br>see <a href=\"https://example.com/notes\">https://example.com/notes</a> and run \
                        <code>cargo test</code> with the team</p>";
/// The link in a's own message.
const LINK: &str = "https://example.com/notes";

/// A made-up Claude Code (`__STATE__` its state directory): `--help` names two models; otherwise it notes how it was
/// started and its process id, and answers each question with the message's markers kept and its words in German or
/// French from a small dictionary (any other language as it was), logging each question; it waits while `hold` exists
/// and fails as signed out while `fail` does.
const FAKE_CLAUDE: &str = r#"#!/bin/bash
state="__STATE__"
case "$1" in
  --help)
    printf '%s\n' "Usage: claude [options]" "  --model <model>  Model for the current session: an alias such as 'sonnet' or 'opus'" "  -p, --print  Print the answer"
    exit 0;;
esac
model=""
while [ $# -gt 0 ]; do
  [ "$1" = "--model" ] && model="$2"
  shift
done
printf 'start pid=%s model=%s\n' "$$" "$model" >> "$state/calls.log"
printf '%s' "$$" > "$state/pid"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$state/received.log"
  while [ -e "$state/hold" ]; do sleep 0.02; done
  if [ -e "$state/fail" ]; then echo 'Invalid API key. Please run /login' >&2; exit 1; fi
  lang=$(printf '%s' "$line" | sed -n 's/.*<language>\([^<]*\)<\/language>.*/\1/p')
  t=$(printf '%s' "$line" | sed 's/.*<message>\\n\(.*\)\\n<\/message>.*/\1/')
  case "$lang" in
    German) t=$(printf '%s' "$t" | sed 's/\bhello\b/hallo/g; s/\bteam\b/Mannschaft/g; s/\bplease\b/bitte/g; s/\bsend\b/schicke/g; s/\bthe\b/die/g; s/\bnotes\b/Notizen/g; s/\bsee\b/siehe/g; s/\band\b/und/g; s/\brun\b/starte/g; s/\bwith\b/mit/g; s/\bto\b/an/g; s/\bmore\b/mehr/g');;
    French) t=$(printf '%s' "$t" | sed 's/\bhello\b/bonjour/g; s/\bteam\b/équipe/g; s/\bplease\b/svp/g; s/\bsend\b/envoie/g; s/\bthe\b/les/g; s/\bnotes\b/notes/g');;
  esac
  printf '{"type":"result","subtype":"success","is_error":false,"result":"<translated>%s</translated>"}\n' "$t" | tee -a "$state/answers.log"
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
            eprintln!("translate draft window test: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Prints `line` to stdout at once.
fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

// --- the parent: the sandbox, the made-up tool, the user's pointer and keys, and the checks ---

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams translate draft window", &options)? else {
        return Ok(());
    };
    let tool = FakeTool::install(&sandbox)?;
    run(&sandbox, &tool, "first", first_run)?;
    // the second run starts without the tool on the PATH
    tool.uninstall()?;
    run(&sandbox, &tool, "second", second_run)?;
    say("ngteams translate draft window: ok");
    Ok(())
}

/// One run of the app: started, walked by `walk`, and quit.
fn run(sandbox: &Sandbox, tool: &FakeTool, name: &str, walk: fn(&mut User) -> SandboxResult) -> SandboxResult {
    let settings = sandbox.path("settings.toml");
    let child = sandbox.spawn(
        &std::env::current_exe()?,
        &[],
        &[
            (CHILD_ENV, "app"),
            (SETTINGS_ENV, &settings.display().to_string()),
            (TOOLS_ENV, &tool.bin.display().to_string()),
            ("PATH", &tool.path),
            // the made-up tool's models come from its `--help`, not a catalog of the real one's
            ("CLAUDE_CONFIG_DIR", &tool.home.display().to_string()),
            ("CODEX_HOME", &tool.home.display().to_string()),
        ],
    )?;
    let running = Running::new(child)?;
    let mut user = User::start(sandbox, running, tool)?;
    match walk(&mut user) {
        Ok(()) => {
            user.ask("quit")?;
            let (status, stderr) = user.running.finish(WAIT)?;
            if !status.success() {
                return Err(format!("the {name} run's app exited {status}: {stderr}").into());
            }
            Ok(())
        }
        Err(error) => {
            // what the made-up tool was asked and how it was started, kept beside the last screen
            for log in ["received.log", "calls.log", "answers.log"] {
                let _ = fs::copy(
                    tool.state.join(log),
                    shots_dir()?.join(format!("translate-{name}-{log}")),
                );
            }
            let shot = user
                .shot(&format!("translate-{name}-failed.png"))
                .map_or_else(|_| String::new(), |path| path.display().to_string());
            Err(user
                .running
                .fail(&format!("{name} run: {error} (last screen: {shot}){}", sandbox.logs())))
        }
    }
}

/// The made-up tool as the parent sees it: where it is, what it was asked, and the files that steer it.
struct FakeTool {
    /// The directory its `claude` is in.
    bin: PathBuf,
    /// Its logs and switches.
    state: PathBuf,
    /// An empty home for the tools' own catalogs.
    home: PathBuf,
    /// The child's `PATH`: `bin`, then the sandbox's own, without any directory holding a real `claude` or `codex`.
    path: String,
}

impl FakeTool {
    fn install(sandbox: &Sandbox) -> SandboxResult<Self> {
        let (bin, state, home) = (
            sandbox.path("ai-bin"),
            sandbox.path("ai-state"),
            sandbox.path("ai-home"),
        );
        for dir in [&bin, &state, &home] {
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
        // the commands the made-up tool runs, from wherever they are (a real tool may share their directory)
        let commands = sandbox.path("ai-commands");
        fs::create_dir_all(&commands)?;
        for command in ["sed", "tee", "sleep"] {
            let found = std::env::split_paths(&inherited)
                .map(|dir| dir.join(command))
                .find(|candidate| candidate.is_file())
                .ok_or_else(|| format!("no {command} on the PATH"))?;
            std::os::unix::fs::symlink(found, commands.join(command))?;
        }
        let path = std::env::join_paths([bin.clone(), commands].into_iter().chain(kept))?
            .into_string()
            .map_err(|_| "a PATH that isn't UTF-8")?;
        let tool = Self { bin, state, home, path };
        tool.reinstall()?;
        Ok(tool)
    }

    /// Puts `claude` (back) on the child's `PATH`.
    fn reinstall(&self) -> SandboxResult {
        use std::os::unix::fs::PermissionsExt as _;
        let program = self.bin.join("claude");
        let script = FAKE_CLAUDE.replace("__STATE__", &self.state.display().to_string());
        fs::write(&program, script)?;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    /// Takes `claude` off the child's `PATH`.
    fn uninstall(&self) -> SandboxResult {
        fs::remove_file(self.bin.join("claude"))?;
        Ok(())
    }

    fn switch(&self, name: &str, on: bool) -> SandboxResult {
        let file = self.state.join(name);
        if on {
            fs::write(file, "")?;
        } else if file.exists() {
            fs::remove_file(file)?;
        }
        Ok(())
    }

    /// The tool waits before answering while `on`.
    fn hold(&self, on: bool) -> SandboxResult {
        self.switch("hold", on)
    }

    /// The tool fails as signed out while `on`.
    fn fail(&self, on: bool) -> SandboxResult {
        self.switch("fail", on)
    }

    fn lines(&self, log: &str) -> Vec<String> {
        fs::read_to_string(self.state.join(log))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Every question the tool was asked, as its stdin's lines.
    fn received(&self) -> Vec<String> {
        self.lines("received.log")
    }

    /// How each run of the tool was started: `start pid=<pid> model=<model>`.
    fn calls(&self) -> Vec<String> {
        self.lines("calls.log")
    }

    /// The last run's process id.
    fn pid(&self) -> Option<String> {
        fs::read_to_string(self.state.join("pid"))
            .ok()
            .filter(|pid| !pid.is_empty())
    }
}

/// Whether process `pid` still runs (a zombie counts as ended).
fn alive(pid: &str) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(") ")
            .is_some_and(|(_, rest)| !rest.starts_with('Z') && !rest.starts_with('X'))
    })
}

/// A rectangle on the screen, in output pixels: `x0..x1`, `y0..y1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Rect {
    fn center(self) -> (u32, u32) {
        (u32::midpoint(self.x0, self.x1), u32::midpoint(self.y0, self.y1))
    }

    fn width(self) -> u32 {
        self.x1 - self.x0
    }

    fn contains(self, (x, y): (u32, u32)) -> bool {
        (self.x0..self.x1).contains(&x) && (self.y0..self.y1).contains(&y)
    }
}

/// Where a box's controls are drawn.
#[derive(Clone, Copy, Debug)]
struct Toolbar {
    send: Rect,
    language: Rect,
    translate: Rect,
}

impl Toolbar {
    /// The row the line over the box is drawn on, and the right end of the box.
    fn line_band(self) -> Rect {
        Rect {
            x0: self.send.x1.saturating_sub(420),
            y0: self.send.y0.saturating_sub(80),
            x1: self.send.x1 + 12,
            y1: self.send.y0.saturating_sub(16),
        }
    }
}

/// The panes, as the window lays them out at the sandbox's size: the conversation pane, and a thread beside a
/// channel's posts.
const CONVERSATION: Range<u32> = 454..1280;
const POSTS: Range<u32> = 454..860;
const THREAD: Range<u32> = 860..1280;

/// The user at the sandbox: its pointer and keyboard, its screenshots, the child it asks about the views, and the
/// made-up tool.
struct User<'a> {
    sandbox: &'a Sandbox,
    running: Running,
    input: VirtualInput,
    tool: &'a FakeTool,
    asked: u32,
    looks: std::cell::Cell<u32>,
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
            looks: std::cell::Cell::new(0),
        };
        user.park()?;
        user.open("open-ab")?;
        thread::sleep(SETTLE);
        Ok(user)
    }

    /// Asks the child `verb`; its answer.
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

    /// Opens a view (`open-ab`, `open-ac`, `open-channel`, `open-thread`) and waits for its box to take the keys.
    fn open(&mut self, verb: &str) -> SandboxResult {
        let answer = self.ask(verb)?;
        if answer != "ok" {
            return Err(format!("{verb}: {answer}").into());
        }
        Ok(())
    }

    fn state(&mut self) -> SandboxResult<Value> {
        Ok(serde_json::from_str(&self.ask("state")?)?)
    }

    /// Waits up to [`WAIT`] for a state `wanted` takes, named `what` in the error; that state.
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

    /// Watches for [`QUIET`] that the state stays as `kept` wants, named `what` in the error.
    fn stays(&mut self, what: &str, kept: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let start = Instant::now();
        loop {
            let state = self.state()?;
            if !kept(&state) {
                return Err(format!("{what}: changed after {:?}: {state}", start.elapsed()).into());
            }
            if start.elapsed() > QUIET {
                return Ok(state);
            }
            thread::sleep(POLL);
        }
    }

    fn key(&mut self, key: u32) -> SandboxResult {
        self.input.key(key)
    }

    fn chord(&mut self, modifiers: &[Modifier], key: u32) -> SandboxResult {
        self.input.key_with(modifiers, key)
    }

    /// A key pressed by its name (`ctrl+shift+y`).
    fn press(&mut self, spec: &str) -> SandboxResult {
        let (modifiers, key) =
            ui_sandbox::input::parse_key(spec, self.input.keymap()).ok_or_else(|| format!("no key {spec}"))?;
        self.input.key_with(&modifiers, key)
    }

    fn type_text(&mut self, text: &str) -> SandboxResult {
        self.input.type_text(text)
    }

    fn click(&mut self, (x, y): (u32, u32)) -> SandboxResult {
        self.input.click(x, y, keys::BTN_LEFT)
    }

    /// The pointer on the title bar, away from every control that reacts to it.
    fn park(&mut self) -> SandboxResult {
        self.input.move_to(300, 23)
    }

    /// Screenshots the screen into the sandbox dir and keeps a copy under `target/ui-shots/`.
    fn shot(&self, name: &str) -> SandboxResult<PathBuf> {
        let png = self.sandbox.screenshot(name, false)?;
        fs::copy(&png, shots_dir()?.join(name))?;
        Ok(png)
    }

    /// The screen as it is now, kept only in the sandbox dir (each look a file of its own: a file written again could
    /// be read before niri finished it).
    fn look(&self, name: &str) -> SandboxResult<image::RgbImage> {
        let n = self.looks.get() + 1;
        self.looks.set(n);
        let png = self.sandbox.screenshot(&format!("look-{n}-{name}"), false)?;
        let shot = image::open(&png)?.to_rgb8();
        fs::remove_file(png)?;
        Ok(shot)
    }

    /// Screenshots after the views settled, in the theme the app shows now.
    fn settled_shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        self.park()?;
        thread::sleep(SETTLE);
        self.shot(name)
    }

    /// The same view in Teams Light and Dark, as `translate-<name>-{light,dark}.png`; back in Light after.
    fn shots_in_both_themes(&mut self, name: &str) -> SandboxResult {
        self.settled_shot(&format!("translate-{name}-light.png"))?;
        self.ask("theme-dark")?;
        self.settled_shot(&format!("translate-{name}-dark.png"))?;
        self.ask("theme-light")?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The screen once `band` stays the same between two looks (up to [`WAIT`]).
    fn steady(&self, band: Rect) -> SandboxResult<image::RgbImage> {
        let start = Instant::now();
        let mut last = self.look("steady.png")?;
        loop {
            thread::sleep(HOVER);
            let now = self.look("steady.png")?;
            let moved = bounds_of(&now, |x, y, pixel| {
                band.contains((x, y)) && *pixel != *last.get_pixel(x, y)
            });
            if moved.is_none() {
                return Ok(now);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{band:?} never stays still: {moved:?}").into());
            }
            last = now;
        }
    }

    /// What lights up while the pointer rests on `at`, within `band`: a ghost button's hover.
    fn hovered(&mut self, at: (u32, u32), band: Rect) -> SandboxResult<Option<Rect>> {
        // a tooltip shown by the last probe gone first, and the band still (no button fading in)
        self.park()?;
        thread::sleep(SETTLE);
        let before = self.steady(band)?;
        self.input.move_to(at.0, at.1)?;
        thread::sleep(HOVER);
        let after = self.look("probe-after.png")?;
        self.park()?;
        Ok(bounds_of(&after, |x, y, pixel| {
            band.contains((x, y))
                && pixel
                    .0
                    .iter()
                    .zip(before.get_pixel(x, y).0)
                    .any(|(&got, was)| got.abs_diff(was) > 5)
        }))
    }

    /// The controls of the box in the columns `pane`, found on the screen: Send's lit disc, the language button just
    /// left of the divider by it, Translate right against it.
    fn toolbar(&mut self, pane: Range<u32>) -> SandboxResult<Toolbar> {
        self.park()?;
        thread::sleep(SETTLE);
        let screen = self.look("toolbar.png")?;
        let send = send_disc(&screen, pane.clone()).ok_or("no lit Send button: is the draft empty?")?;
        let (_, row) = send.center();
        let band = Rect {
            x0: pane.start,
            y0: row - 15,
            x1: send.x0,
            y1: row + 15,
        };
        // the language button ends a divider's width left of Send (its caret alone in a narrow box)
        let mut language = None;
        for _ in 0..3 {
            let found = self.hovered((send.x0 - 26, row), band)?;
            language = found.filter(|found| {
                (14..=130).contains(&found.width()) && (send.x0 - 24..=send.x0 - 10).contains(&found.x1)
            });
            if language.is_some() {
                break;
            }
            say(&format!("note: the language probe left of {send:?} found {found:?}"));
        }
        let language = language.ok_or_else(|| format!("no language button left of Send {send:?}"))?;
        // Translate is the toolbar's square button right against the language button's left
        let translate = Rect {
            x0: language.x0 - TOOL,
            y0: row - TOOL / 2,
            x1: language.x0,
            y1: row + TOOL / 2,
        };
        Ok(Toolbar {
            send,
            language,
            translate,
        })
    }

    /// The buttons and words of the line over the box, right to left, as runs of dark pixels: the line's
    /// controls first.
    fn line_parts(&mut self, bar: Toolbar) -> SandboxResult<Vec<Rect>> {
        self.park()?;
        thread::sleep(SETTLE);
        let screen = self.look("line.png")?;
        let band = bar.line_band();
        let dark = |pixel: &image::Rgb<u8>| pixel.0.iter().all(|&channel| channel < 170);
        // the line's rows: upwards from the box, the first rows with dark pixels, up to a gap (the messages above)
        let inked = |y: u32| (band.x0..band.x1).any(|x| dark(screen.get_pixel(x, y)));
        let bottom = (band.y0..band.y1)
            .rev()
            .find(|&y| inked(y))
            .ok_or("no line over the box")?;
        let mut top = bottom;
        while top > band.y0 && (1..=4).any(|up| top >= band.y0 + up && inked(top - up)) {
            top -= 1;
        }
        let line = Rect {
            x0: band.x0,
            y0: top,
            x1: band.x1,
            y1: bottom + 1,
        };
        Ok(runs_right_to_left(&screen, line, dark, 9))
    }

    /// Clicks the `nth` (0: the rightmost) control of the line over the box.
    fn click_line(&mut self, bar: Toolbar, nth: usize) -> SandboxResult {
        let parts = self.line_parts(bar)?;
        let part = parts
            .get(nth)
            .ok_or_else(|| format!("the line over the box has no part {nth}: {parts:?}"))?;
        self.click(part.center())?;
        self.park()
    }
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

/// The bounds of the pixels `taken` takes, if any.
fn bounds_of(shot: &image::RgbImage, taken: impl Fn(u32, u32, &image::Rgb<u8>) -> bool) -> Option<Rect> {
    let mut found: Option<Rect> = None;
    for (x, y, pixel) in shot.enumerate_pixels() {
        if !taken(x, y, pixel) {
            continue;
        }
        found = Some(found.map_or(
            Rect {
                x0: x,
                y0: y,
                x1: x + 1,
                y1: y + 1,
            },
            |rect| Rect {
                x0: rect.x0.min(x),
                y0: rect.y0.min(y),
                x1: rect.x1.max(x + 1),
                y1: rect.y1.max(y + 1),
            },
        ));
    }
    found
}

/// A pixel of the brand's blues: Send's lit disc, a selected segment.
fn brandish(pixel: image::Rgb<u8>) -> bool {
    let [r, g, b] = pixel.0.map(i32::from);
    b >= 150 && b - r >= 40 && b - g >= 30
}

/// Runs of `ink` within `within`, right to left: a column belongs to one when any pixel of it is ink, and runs closer
/// than `gap` columns are one.
fn runs_right_to_left(
    shot: &image::RgbImage,
    within: Rect,
    ink: impl Fn(&image::Rgb<u8>) -> bool,
    gap: u32,
) -> Vec<Rect> {
    let mut runs: Vec<Rect> = Vec::new();
    for x in (within.x0..within.x1).rev() {
        let column: Vec<u32> = (within.y0..within.y1).filter(|&y| ink(shot.get_pixel(x, y))).collect();
        let (Some(&top), Some(&bottom)) = (column.first(), column.last()) else {
            continue;
        };
        match runs.last_mut() {
            Some(run) if run.x0 - x <= gap => {
                run.x0 = x;
                run.y0 = run.y0.min(top);
                run.y1 = run.y1.max(bottom + 1);
            }
            _ => runs.push(Rect {
                x0: x,
                y0: top,
                x1: x + 1,
                y1: bottom + 1,
            }),
        }
    }
    runs
}

/// Send's lit disc in the columns `pane`, near the foot of the screen: the rightmost run of columns with many of the
/// brand's blues, its rows the longest run of such rows there (not the box's accent line under it).
fn send_disc(shot: &image::RgbImage, pane: Range<u32>) -> Option<Rect> {
    let height = shot.height();
    let top = height.saturating_sub(220);
    let foot = Rect {
        x0: pane.start,
        y0: top,
        x1: pane.end.min(shot.width()),
        y1: height,
    };
    // a column of the disc has many of them; the box's accent line under it, only a few
    let lit = |x: u32, y: u32| foot.contains((x, y)) && brandish(*shot.get_pixel(x, y));
    let tall = |x: u32| (top..height).filter(|&y| lit(x, y)).count() >= 16;
    let mut disc: Option<Rect> = None;
    for x in (foot.x0..foot.x1).rev() {
        match (&mut disc, tall(x)) {
            (Some(run), true) if run.x0 == x + 1 => run.x0 = x,
            (Some(run), false) if run.width() >= 20 => break,
            (_, true) => {
                disc = Some(Rect {
                    x0: x,
                    y0: top,
                    x1: x + 1,
                    y1: height,
                });
            }
            (_, false) => disc = None,
        }
    }
    let disc = disc.filter(|run| (20..=40).contains(&run.width()))?;
    let mut best: Option<(u32, u32)> = None;
    let mut current: Option<(u32, u32)> = None;
    for y in top..height {
        let lit = (disc.x0..disc.x1).filter(|&x| brandish(*shot.get_pixel(x, y))).count() >= 4;
        current = match (lit, current) {
            (true, Some((start, _))) => Some((start, y + 1)),
            (true, None) => Some((y, y + 1)),
            (false, _) => None,
        };
        if let Some((start, end)) = current
            && best.is_none_or(|(a, b)| end - start > b - a)
        {
            best = Some((start, end));
        }
    }
    let (y0, y1) = best?;
    Some(Rect {
        x0: disc.x0,
        y0,
        x1: disc.x1,
        y1,
    })
}

/// The pixels of `after` in the menu's hover colour that weren't in `before`: the row under the keys.
fn row_lit(before: &image::RgbImage, after: &image::RgbImage) -> Option<Rect> {
    let wanted = teams_ui::theme::LIGHT_PALETTE.hover.to_be_bytes();
    let near = |pixel: &image::Rgb<u8>| {
        pixel
            .0
            .iter()
            .zip(&wanted[1..])
            .all(|(&got, &want)| got.abs_diff(want) <= 2)
    };
    bounds_of(after, |x, y, pixel| near(pixel) && !near(before.get_pixel(x, y)))
}

// --- what the child says of a box ---

fn status(state: &Value, place: &str) -> String {
    state["boxes"][place]["status"].as_str().unwrap_or("-").to_owned()
}

fn text(state: &Value, place: &str) -> String {
    state["boxes"][place]["text"].as_str().unwrap_or("-").to_owned()
}

fn language(state: &Value) -> String {
    state["language"].as_str().unwrap_or("-").to_owned()
}

fn focused(state: &Value, place: &str) -> bool {
    state["boxes"][place]["focused"] == true
}

/// The box's runs that never go to the tool (mentions, links, code), with their text.
fn atoms(state: &Value, place: &str) -> Vec<Value> {
    state["boxes"][place]["blocks"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|block| block["runs"].as_array().cloned().unwrap_or_default())
        .filter(|run| !run["mention"].is_null() || !run["link"].is_null() || run["code"] == true)
        .collect()
}

/// The box's bold runs' text.
fn bold(state: &Value, place: &str) -> Vec<String> {
    state["boxes"][place]["blocks"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|block| block["runs"].as_array().cloned().unwrap_or_default())
        .filter(|run| run["bold"] == true)
        .filter_map(|run| run["t"].as_str().map(str::to_owned))
        .collect()
}

/// The language and the message of the tool's last question, as its stdin carried them.
fn last_question(tool: &FakeTool) -> SandboxResult<String> {
    tool.received()
        .last()
        .cloned()
        .ok_or_else(|| "the tool was asked nothing".into())
}

/// The question carries only the language and the draft's words: no chat, names, ids or tokens.
fn only_language_and_draft(question: &str, language: &str, words: &[&str]) -> SandboxResult {
    if !question.contains(&format!("<language>{language}</language>")) {
        return Err(format!("the question doesn't name {language}: {question}").into());
    }
    for word in words {
        if !question.contains(word) {
            return Err(format!("the question lacks {word:?}: {question}").into());
        }
    }
    for leak in [
        AB_CHAT,
        AC_CHAT,
        LAB_A,
        LAB_B,
        LAB_C,
        "Lab B",
        "bot-a",
        "Bearer",
        "token",
        LINK,
        "cargo test",
    ] {
        if question.contains(leak) {
            return Err(format!("the question carries {leak:?}: {question}").into());
        }
    }
    Ok(())
}

/// Clears the focused box: everything selected and deleted.
fn clear_box(user: &mut User, place: &str) -> SandboxResult {
    user.press("ctrl+a")?;
    user.key(keys::BACKSPACE)?;
    user.until(&format!("the {place} box emptied"), |state| {
        text(state, place).is_empty()
    })?;
    Ok(())
}

/// Types `draft` into the focused box of `place`, after emptying it.
fn type_draft(user: &mut User, place: &str, draft: &str) -> SandboxResult {
    clear_box(user, place)?;
    user.type_text(draft)?;
    user.until(&format!("the {place} box holds the draft"), |state| {
        text(state, place) == draft
    })?;
    Ok(())
}

/// Translate in `place` (by the `bar`'s button, else by Ctrl+Shift+Y) turns `draft` into `translated`; Ctrl+Z brings
/// `draft` back in one step.
fn translates(user: &mut User, place: &str, by: Option<Toolbar>, (draft, translated): (&str, &str)) -> SandboxResult {
    let asked = user.tool.received().len();
    match by {
        Some(bar) => click_until(
            user,
            bar.translate.center(),
            &format!("Translate in {place} {bar:?}"),
            |state| status(state, place) != "idle" || text(state, place) != draft,
        )?,
        None => user.press("ctrl+shift+y")?,
    }
    user.until(&format!("the {place} box translated"), |state| {
        text(state, place) == translated && status(state, place) == "idle"
    })?;
    if user.tool.received().len() != asked + 1 {
        return Err(format!("the tool wasn't asked once: {:?}", user.tool.received()).into());
    }
    user.press("ctrl+z")?;
    user.until(&format!("Ctrl+Z brings the {place} draft back"), |state| {
        text(state, place) == draft
    })?;
    user.press("ctrl+shift+z")?;
    user.until(&format!("Ctrl+Shift+Z puts the {place} translation back"), |state| {
        text(state, place) == translated
    })?;
    Ok(())
}

// --- the first run: the tool on the PATH ---

fn first_run(user: &mut User) -> SandboxResult {
    the_language_menu(user)?;
    translate_in_a_chat(user)?;
    progress_and_cancel(user)?;
    failure_and_try_again(user)?;
    a_draft_changed_while_running(user)?;
    correct_and_translate_not_at_once(user)?;
    nothing_to_translate(user)?;
    a_reply_and_an_edit(user)?;
    another_chat_s_box(user)?;
    the_channel_s_boxes(user)?;
    new_message(user)
}

/// A click on the language button opens its menu.
fn open_menu(user: &mut User, bar: Toolbar) -> SandboxResult {
    click_until(user, bar.language.center(), "the language menu opens", |state| {
        state["menu"]["open"] == true
    })
}

/// A click at `at` brings a state `wanted` takes, named `what` in the error. A click the app didn't take is clicked
/// again and said, at most twice.
fn click_until(user: &mut User, at: (u32, u32), what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult {
    for attempt in 0..3 {
        user.click(at)?;
        user.park()?;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(3) {
            if wanted(&user.state()?) {
                if attempt > 0 {
                    say(&format!("note: {what}: taken on click {}", attempt + 1));
                }
                return Ok(());
            }
            thread::sleep(POLL);
        }
        user.shot(&format!("translate-missed-click-{}.png", user.asked))?;
    }
    Err(format!("{what}: three clicks at {at:?} didn't").into())
}

/// The language button opens the menu with the language chosen first; a search picked by Enter; a language typed;
/// Escape; a row clicked.
fn the_language_menu(user: &mut User) -> SandboxResult {
    let state = user.until("the a–b chat's box has the keys", |state| focused(state, "chat"))?;
    if language(&state) != "English" {
        return Err(format!("the language isn't English at first: {state}").into());
    }
    user.type_text(DRAFT)?;
    user.until("the draft typed", |state| text(state, "chat") == DRAFT)?;
    user.shots_in_both_themes("control")?;
    let bar = user.toolbar(CONVERSATION)?;

    open_menu(user, bar)?;
    let state = user.until("the language menu opens", |state| state["menu"]["open"] == true)?;
    if state["menu"]["rows"][0] != "English" {
        return Err(format!("the chosen language isn't first: {state}").into());
    }
    user.shots_in_both_themes("menu")?;
    user.type_text("germ")?;
    user.until("the search leaves German", |state| {
        state["menu"]["rows"] == json!(["German"])
    })?;
    user.key(keys::ENTER)?;
    let state = user.until("Enter picks German and closes the menu", |state| {
        language(state) == "German" && state["menu"]["open"] == false
    })?;
    if !focused(&state, "chat") || text(&state, "chat") != DRAFT {
        return Err(format!("the box didn't get the keys back, its draft kept: {state}").into());
    }

    let bar = user.toolbar(CONVERSATION)?;
    open_menu(user, bar)?;
    user.until("German first now", |state| {
        state["menu"]["open"] == true && state["menu"]["rows"][0] == "German"
    })?;
    user.type_text("Klingon")?;
    user.until("a typed language offered", |state| {
        state["menu"]["rows"] == json!(["use:Klingon"])
    })?;
    user.shots_in_both_themes("typed")?;
    user.key(keys::ENTER)?;
    user.until("the typed language used", |state| {
        language(state) == "Klingon" && state["menu"]["open"] == false
    })?;

    let bar = user.toolbar(CONVERSATION)?;
    open_menu(user, bar)?;
    user.until("Klingon first, by its name", |state| {
        state["menu"]["open"] == true && state["menu"]["rows"][0] == "Klingon"
    })?;
    user.key(keys::ESC)?;
    user.until("Escape closes the menu, the language kept", |state| {
        state["menu"]["open"] == false && language(state) == "Klingon" && focused(state, "chat")
    })?;

    // a row clicked: the search left with French, its row found by the keys' highlight
    open_menu(user, bar)?;
    user.until("the menu again", |state| state["menu"]["open"] == true)?;
    user.type_text("fren")?;
    user.until("French alone", |state| state["menu"]["rows"] == json!(["French"]))?;
    thread::sleep(SETTLE);
    let before = user.look("menu-before.png")?;
    user.key(keys::DOWN)?;
    thread::sleep(SETTLE);
    let after = user.look("menu-after.png")?;
    let row = row_lit(&before, &after).ok_or("Down lit no row")?;
    user.click(row.center())?;
    user.park()?;
    user.until("a click picks French", |state| {
        language(state) == "French" && state["menu"]["open"] == false
    })?;
    Ok(())
}

/// The chat's box: Translate by its button and by Ctrl+Shift+Y, one undo step; what the tool was asked; bold, a
/// mention and a line break kept.
fn translate_in_a_chat(user: &mut User) -> SandboxResult {
    let bar = user.toolbar(CONVERSATION)?;
    translates(user, "chat", Some(bar), (DRAFT, FRENCH))?;
    only_language_and_draft(&last_question(user.tool)?, "French", &["hello", "team", "notes"])?;
    user.shots_in_both_themes("translated")?;

    // German again, by the menu's keys: Down to its row and Enter
    open_menu(user, bar)?;
    user.until("the menu", |state| state["menu"]["open"] == true)?;
    user.type_text("German")?;
    user.until("German alone", |state| state["menu"]["rows"] == json!(["German"]))?;
    user.key(keys::DOWN)?;
    user.key(keys::ENTER)?;
    user.until("German picked", |state| language(state) == "German")?;

    type_draft(user, "chat", DRAFT)?;
    translates(user, "chat", None, (DRAFT, GERMAN))?;
    let calls = user.tool.calls();
    if !calls.last().is_some_and(|call| call.ends_with("model=sonnet")) {
        return Err(format!("the tool wasn't started with the recommended model: {calls:?}").into());
    }

    // bold, a mention picked from the box's list, a line break
    clear_box(user, "chat")?;
    user.type_text("hello ")?;
    user.press("ctrl+b")?;
    user.type_text("team")?;
    user.press("ctrl+b")?;
    user.type_text(" ")?;
    user.type_text("@lab")?;
    user.until("the mention list offers b", |state| {
        state["boxes"]["chat"]["mentioning"].as_u64() > Some(0)
    })?;
    user.key(keys::ENTER)?;
    user.until("a mention in the box", |state| !atoms(state, "chat").is_empty())?;
    user.chord(&[Modifier::Shift], keys::ENTER)?;
    user.type_text("please send the notes")?;
    let before = user.until("the line break typed", |state| {
        text(state, "chat").contains('\n') && text(state, "chat").ends_with("notes")
    })?;
    user.press("ctrl+shift+y")?;
    let after = user.until("the formatted draft translated", |state| {
        text(state, "chat").ends_with("bitte schicke die Notizen") && status(state, "chat") == "idle"
    })?;
    if atoms(&after, "chat") != atoms(&before, "chat") {
        return Err(format!(
            "the mention changed: {:?} to {:?}",
            atoms(&before, "chat"),
            atoms(&after, "chat")
        )
        .into());
    }
    if bold(&after, "chat") != ["Mannschaft"] {
        return Err(format!("the bold word isn't the translated one: {after}").into());
    }
    if !text(&after, "chat").contains('\n') || !text(&after, "chat").starts_with("hallo ") {
        return Err(format!("the line break or the words went: {after}").into());
    }
    clear_box(user, "chat")
}

/// A way to stop a translation running.
type Stop = fn(&mut User, Toolbar) -> SandboxResult;

/// While it runs: the line with Cancel; Cancel, Escape and the button again stop it, the tool's process ends and the
/// draft stays.
fn progress_and_cancel(user: &mut User) -> SandboxResult {
    type_draft(user, "chat", DRAFT)?;
    let bar = user.toolbar(CONVERSATION)?;
    user.tool.hold(true)?;
    let stopped_by: [(&str, Stop); 3] = [
        ("Cancel", |user, bar| user.click_line(bar, 0)),
        ("Escape", |user, _| user.key(keys::ESC)),
        ("the button again", |user, bar| {
            user.click(bar.translate.center())?;
            user.park()
        }),
    ];
    for (at, (how, stop)) in stopped_by.into_iter().enumerate() {
        let asked = user.tool.received().len();
        user.press("ctrl+shift+y")?;
        user.until("the translation running", |state| status(state, "chat") == "running")?;
        user.until("the tool asked", |_| user_asked(user.tool, asked))?;
        let pid = user.tool.pid().ok_or("no tool process")?;
        if at == 0 {
            user.shots_in_both_themes("progress")?;
        }
        stop(user, bar)?;
        user.until(&format!("{how} stops it"), |state| {
            status(state, "chat") == "idle" && text(state, "chat") == DRAFT && focused(state, "chat")
        })?;
        wait_for(&format!("{how} ends the tool's process"), || !alive(&pid))?;
    }
    user.tool.hold(false)?;
    user.stays("nothing comes after a cancel", |state| text(state, "chat") == DRAFT)?;
    Ok(())
}

fn user_asked(tool: &FakeTool, before: usize) -> bool {
    tool.received().len() > before
}

/// Waits up to [`WAIT`] for `done`, named `what` in the error.
fn wait_for(what: &str, done: impl Fn() -> bool) -> SandboxResult {
    let start = Instant::now();
    while !done() {
        if start.elapsed() > WAIT {
            return Err(format!("{what}: never").into());
        }
        thread::sleep(POLL);
    }
    Ok(())
}

/// The tool signed out: the line says so, the draft is kept; Try again translates once it is signed in.
fn failure_and_try_again(user: &mut User) -> SandboxResult {
    let bar = user.toolbar(CONVERSATION)?;
    user.tool.fail(true)?;
    user.press("ctrl+shift+y")?;
    let state = user.until("the failure said", |state| status(state, "chat").starts_with("failed:"))?;
    if !status(&state, "chat").contains("signed in") || text(&state, "chat") != DRAFT {
        return Err(format!("the failure isn't the tool signed out, the draft kept: {state}").into());
    }
    user.shots_in_both_themes("failed")?;
    let parts = user.line_parts(bar)?;
    // Dismiss, Try again, the words
    if parts.len() < 3 {
        return Err(format!("the failure's line has no Try again: {parts:?}").into());
    }
    user.tool.fail(false)?;
    user.click_line(bar, 1)?;
    user.until("Try again translates", |state| {
        text(state, "chat") == GERMAN && status(state, "chat") == "idle"
    })?;
    // the failure dismissed by its button
    type_draft(user, "chat", DRAFT)?;
    user.tool.fail(true)?;
    user.press("ctrl+shift+y")?;
    user.until("the failure again", |state| {
        status(state, "chat").starts_with("failed:")
    })?;
    user.click_line(bar, 0)?;
    user.until("Dismiss takes the line away", |state| {
        status(state, "chat") == "idle" && text(state, "chat") == DRAFT
    })?;
    user.tool.fail(false)
}

/// The draft edited while the tool works: the result isn't used, the line says the message changed, and Translate
/// again translates the draft as it is.
fn a_draft_changed_while_running(user: &mut User) -> SandboxResult {
    let bar = user.toolbar(CONVERSATION)?;
    user.tool.hold(true)?;
    let asked = user.tool.received().len();
    user.press("ctrl+shift+y")?;
    user.until("running", |state| status(state, "chat") == "running")?;
    user.until("the tool asked", |_| user_asked(user.tool, asked))?;
    user.type_text(" more")?;
    let edited = format!("{DRAFT} more");
    user.until("the edit drops the translation", |state| {
        status(state, "chat") == "changed" && text(state, "chat") == edited
    })?;
    user.tool.hold(false)?;
    user.stays("the late answer isn't used", |state| text(state, "chat") == edited)?;
    user.shots_in_both_themes("changed")?;
    user.click_line(bar, 1)?;
    user.until("Translate again translates the edited draft", |state| {
        text(state, "chat") == format!("{GERMAN} mehr") && status(state, "chat") == "idle"
    })?;
    Ok(())
}

/// Correct does nothing while Translate runs; Translate ends a correction running.
fn correct_and_translate_not_at_once(user: &mut User) -> SandboxResult {
    type_draft(user, "chat", DRAFT)?;
    user.tool.hold(true)?;
    user.press("ctrl+shift+y")?;
    user.until("translating", |state| status(state, "chat") == "running")?;
    user.press("ctrl+shift+g")?;
    user.stays("no correction while translating", |state| {
        state["boxes"]["chat"]["correction"] == "Idle" && status(state, "chat") == "running"
    })?;
    user.key(keys::ESC)?;
    user.until("cancelled", |state| status(state, "chat") == "idle")?;

    user.press("ctrl+shift+g")?;
    user.until("correcting", |state| state["boxes"]["chat"]["correction"] == "Running")?;
    user.press("ctrl+shift+y")?;
    user.until("Translate ends the correction and runs alone", |state| {
        state["boxes"]["chat"]["correction"] == "Idle" && status(state, "chat") == "running"
    })?;
    user.tool.hold(false)?;
    user.until("translated", |state| text(state, "chat") == GERMAN)?;
    Ok(())
}

/// An empty box starts nothing, by the key or the button; a draft of a mention alone has no words to translate.
fn nothing_to_translate(user: &mut User) -> SandboxResult {
    let bar = user.toolbar(CONVERSATION)?;
    clear_box(user, "chat")?;
    let asked = user.tool.received().len();
    user.press("ctrl+shift+y")?;
    user.click(bar.translate.center())?;
    user.park()?;
    user.stays("an empty box starts nothing", |state| {
        status(state, "chat") == "idle" && text(state, "chat").is_empty()
    })?;
    if user.tool.received().len() != asked {
        return Err("the tool was asked about an empty box".into());
    }
    user.type_text("@lab")?;
    user.until("the mention list offers b", |state| {
        state["boxes"]["chat"]["mentioning"].as_u64() > Some(0)
    })?;
    user.key(keys::ENTER)?;
    user.until("a mention alone", |state| !atoms(state, "chat").is_empty())?;
    user.press("ctrl+shift+y")?;
    user.until("no words to translate", |state| status(state, "chat") == "nothing")?;
    if user.tool.received().len() != asked {
        return Err("the tool was asked about a mention alone".into());
    }
    user.key(keys::ESC)?;
    user.until("Escape takes the line away", |state| status(state, "chat") == "idle")?;
    clear_box(user, "chat")
}

/// A reply to b's message and an edit of a's own: translated in place, the reply's quote and the edit's mention,
/// link, code, emoji and line break kept.
fn a_reply_and_an_edit(user: &mut User) -> SandboxResult {
    // the box hands the keys to the messages; Up walks to a message of b's
    user.key(keys::ESC)?;
    for _ in 0..16 {
        user.key(keys::UP)?;
        thread::sleep(POLL);
        if user.state()?["replyable"] == true {
            break;
        }
    }
    user.type_text("r")?;
    user.until("replying", |state| {
        state["boxes"]["chat"]["mode"] == "reply" && focused(state, "chat")
    })?;
    user.type_text(DRAFT)?;
    user.until("the reply typed", |state| text(state, "chat") == DRAFT)?;
    let bar = user.toolbar(CONVERSATION)?;
    user.click(bar.translate.center())?;
    user.park()?;
    user.until("the reply translated, still a reply", |state| {
        text(state, "chat") == GERMAN && state["boxes"]["chat"]["mode"] == "reply"
    })?;
    user.shots_in_both_themes("reply")?;
    clear_box(user, "chat")?;
    user.key(keys::ESC)?;
    user.until("the reply ended", |state| state["boxes"]["chat"]["mode"] == "new")?;

    // a's own message: Up from the newest until it is selected, then e
    user.key(keys::ESC)?;
    let own = user.ask("own-id")?;
    for _ in 0..16 {
        user.key(keys::UP)?;
        thread::sleep(POLL);
        if user.state()?["selected"] == own.as_str() {
            break;
        }
    }
    user.type_text("e")?;
    let before = user.until("editing a's message", |state| {
        state["boxes"]["chat"]["mode"] == "edit" && text(state, "chat").contains("cargo test")
    })?;
    let original = text(&before, "chat");
    user.press("ctrl+shift+y")?;
    let after = user.until("the edit translated", |state| {
        text(state, "chat").starts_with("hallo") && status(state, "chat") == "idle"
    })?;
    only_language_and_draft(&last_question(user.tool)?, "German", &["hello", "please send"])?;
    let translated = text(&after, "chat");
    if atoms(&after, "chat") != atoms(&before, "chat") || atoms(&after, "chat").len() < 3 {
        return Err(format!(
            "the mention, link or code changed: {:?} to {:?}",
            atoms(&before, "chat"),
            atoms(&after, "chat")
        )
        .into());
    }
    if bold(&after, "chat") != ["Mannschaft"] {
        return Err(format!("the bold word isn't kept bold, translated: {after}").into());
    }
    for kept in ['\u{1f600}', '\n'] {
        if translated.matches(kept).count() != original.matches(kept).count() {
            return Err(format!("{kept:?} wasn't kept: {original:?} to {translated:?}").into());
        }
    }
    if !translated.contains("siehe") || !translated.contains("mit die Mannschaft") {
        return Err(format!("not every line's words translated: {translated:?}").into());
    }
    user.shots_in_both_themes("edit")?;
    user.press("ctrl+z")?;
    user.until("Ctrl+Z brings the message as written back", |state| {
        text(state, "chat") == original
    })?;
    user.key(keys::ESC)?;
    user.until("the edit cancelled", |state| state["boxes"]["chat"]["mode"] == "new")?;
    Ok(())
}

/// The a–c chat's box shows the same language and translates into it.
fn another_chat_s_box(user: &mut User) -> SandboxResult {
    user.open("open-ac")?;
    user.until("the a–c chat's box has the keys", |state| {
        focused(state, "chat") && state["chat"] == AC_CHAT
    })?;
    user.type_text(DRAFT)?;
    user.until("the chat draft typed", |state| text(state, "chat") == DRAFT)?;
    let bar = user.toolbar(CONVERSATION)?;
    open_menu(user, bar)?;
    user.until("the a–c box's menu has German first", |state| {
        state["menu"]["open"] == true && state["menu"]["rows"][0] == "German"
    })?;
    user.key(keys::ESC)?;
    translates(user, "chat", Some(bar), (DRAFT, GERMAN))?;
    clear_box(user, "chat")
}

/// The team channel's post box, and a post's thread's reply box.
fn the_channel_s_boxes(user: &mut User) -> SandboxResult {
    user.open("open-channel")?;
    user.until("the channel's post box", |state| state["boxes"]["post"].is_object())?;
    // the box at the foot of the posts, clicked
    user.click((640, 749))?;
    user.park()?;
    user.until("the post box has the keys", |state| focused(state, "post"))?;
    user.type_text(DRAFT)?;
    user.until("the post draft typed", |state| text(state, "post") == DRAFT)?;
    let bar = user.toolbar(CONVERSATION)?;
    translates(user, "post", Some(bar), (DRAFT, GERMAN))?;
    user.shots_in_both_themes("channel")?;
    clear_box(user, "post")?;

    user.open("open-thread")?;
    user.until("the thread's box", |state| state["boxes"]["thread"].is_object())?;
    // beside the thread the post box is narrow: its toolbar compacted, its placeholder one line
    user.until("the post box compacted beside the thread", |state| {
        state["boxes"]["post"]["narrow"] == true && text(state, "post").is_empty()
    })?;
    user.shots_in_both_themes("thread-placeholder")?;
    user.click((THREAD.start + 120, 749))?;
    user.park()?;
    user.until("the thread's box has the keys", |state| focused(state, "thread"))?;
    user.type_text(DRAFT)?;
    user.until("the thread draft typed", |state| text(state, "thread") == DRAFT)?;
    let bar = user.toolbar(THREAD)?;
    if bar.send.x0 < THREAD.start || bar.send.x0 < POSTS.end {
        return Err(format!("the thread's toolbar isn't in the thread: {bar:?}").into());
    }
    translates(user, "thread", None, (DRAFT, GERMAN))?;
    user.click(bar.translate.center())?;
    user.park()?;
    user.until("the button translates the thread's reply too", |state| {
        status(state, "thread") == "idle" && user.tool.received().len() > 1
    })?;
    user.shots_in_both_themes("thread")?;
    clear_box(user, "thread")
}

/// New message: its box translates.
fn new_message(user: &mut User) -> SandboxResult {
    user.press("ctrl+n")?;
    user.until("New message", |state| state["boxes"]["new"].is_object())?;
    user.click((640, 749))?;
    user.park()?;
    user.until("New message's box has the keys", |state| focused(state, "new"))?;
    user.type_text(DRAFT)?;
    user.until("the new draft typed", |state| text(state, "new") == DRAFT)?;
    let bar = user.toolbar(CONVERSATION)?;
    translates(user, "new", Some(bar), (DRAFT, GERMAN))?;
    user.shots_in_both_themes("new-message")?;
    clear_box(user, "new")
}

// --- the second run: the app started again, the tool not on the PATH ---

fn second_run(user: &mut User) -> SandboxResult {
    let state = user.until("the box has the keys", |state| focused(state, "chat"))?;
    if language(&state) != "German" {
        return Err(format!("the language wasn't kept across the restart: {state}").into());
    }
    let kept = fs::read_to_string(user.sandbox.path("settings.toml"))?;
    if !kept.contains("message_translate_to = \"German\"") {
        return Err(format!("the settings file doesn't keep the language: {kept}").into());
    }
    no_tool_installed(user)?;
    the_settings(user)
}

/// No tool: the line says so with Open settings, which opens Settings → General; once it is installed, Try again
/// translates.
fn no_tool_installed(user: &mut User) -> SandboxResult {
    user.type_text(DRAFT)?;
    user.until("typed", |state| text(state, "chat") == DRAFT)?;
    let bar = user.toolbar(CONVERSATION)?;
    user.click(bar.translate.center())?;
    user.park()?;
    let state = user.until("no tool said", |state| status(state, "chat").starts_with("failed:"))?;
    if !status(&state, "chat").contains("Settings") || text(&state, "chat") != DRAFT {
        return Err(format!("the failure doesn't point at the settings, the draft kept: {state}").into());
    }
    user.shots_in_both_themes("no-tool")?;
    // Dismiss, Try again, Open settings, the words
    user.click_line(bar, 2)?;
    user.until("Open settings opens Settings → General", |state| {
        state["main"] == "settings:General"
    })?;
    user.shots_in_both_themes("settings")?;
    user.open("open-ab")?;
    user.until("the failure still over the box", |state| {
        status(state, "chat").starts_with("failed:")
    })?;
    user.tool.reinstall()?;
    user.click_line(bar, 1)?;
    user.until("Try again finds the tool and translates", |state| {
        text(state, "chat") == GERMAN && status(state, "chat") == "idle"
    })?;
    // and the models it offers are listed for the settings
    user.until("the tool found lists its models", |state| {
        state["claude_models"] == json!(["sonnet", "opus"])
    })?;
    Ok(())
}

/// Settings → General → Message translation: the language by its menu; Codex (not installed); Claude and a model.
fn the_settings(user: &mut User) -> SandboxResult {
    type_draft(user, "chat", DRAFT)?;
    user.ask("open-settings")?;
    user.until("Settings → General", |state| state["main"] == "settings:General")?;
    user.shots_in_both_themes("settings-general")?;
    let (x, y) = message_translation_tools(user)?;
    user.shots_in_both_themes("settings-message-translation")?;

    user.click((x + 57, y - 83))?;
    user.park()?;
    thread::sleep(SETTLE);
    user.shot("translate-settings-menu-light.png")?;
    user.type_text("fren")?;
    user.key(keys::ENTER)?;
    user.until("French set in the settings", |state| language(state) == "French")?;

    // the model's dropdown, the page's last control, where the page ends: its menu shows (the rows found again
    // after each change, as the page may move)
    let (_, y) = lowest_segment_center(user)?;
    if !dropdown_opens(user, (x + 55, y + 85))? {
        return Err("a click on the model dropdown (Automatic) shows no menu".into());
    }
    user.shot("translate-settings-model-automatic-light.png")?;
    user.key(keys::ESC)?;
    let (_, y) = lowest_segment_center(user)?;
    user.click((x + 135, y))?;
    user.park()?;
    user.until("Codex chosen", |state| state["tool"] == "Only(Codex)")?;
    let (_, y) = lowest_segment_center(user)?;
    user.click((x + 73, y))?;
    user.park()?;
    user.until("Claude chosen", |state| state["tool"] == "Only(Claude)")?;
    let (_, y) = lowest_segment_center(user)?;
    if !dropdown_opens(user, (x + 55, y + 85))? {
        return Err("a click on the model dropdown (Claude) shows no menu".into());
    }
    user.shot("translate-settings-model-claude-light.png")?;
    // its last row, "Other model…" (after Recommended, Claude Code's default and the tool's two models), and a model's
    // name typed
    for _ in 0..5 {
        user.key(keys::DOWN)?;
    }
    user.key(keys::ENTER)?;
    thread::sleep(SETTLE);
    user.type_text("opus")?;
    user.key(keys::ENTER)?;
    user.until("a model typed and used", |state| state["model"] == "Named(\"opus\")")?;
    user.settled_shot("translate-settings-chosen-light.png")?;

    user.open("open-ab")?;
    user.until("back in the chat", |state| focused(state, "chat"))?;
    user.press("ctrl+shift+y")?;
    user.until("translated into French with Claude Code's opus", |state| {
        text(state, "chat") == FRENCH
    })?;
    let calls = user.tool.calls();
    if !calls.last().is_some_and(|call| call.ends_with("model=opus")) {
        return Err(format!("the tool wasn't started with the model typed: {calls:?}").into());
    }
    // Codex chosen and not installed: said so, with the settings offered
    user.ask("open-settings")?;
    user.until("Settings again", |state| state["main"] == "settings:General")?;
    // scrolled to the end again (Claude now the segment chosen)
    message_translation_tools(user)?;
    let (_, y) = lowest_segment_center(user)?;
    user.click((x + 135, y))?;
    user.park()?;
    user.until("Codex chosen", |state| state["tool"] == "Only(Codex)")?;
    user.open("open-ab")?;
    type_draft(user, "chat", DRAFT)?;
    user.press("ctrl+shift+y")?;
    let state = user.until("Codex isn't installed", |state| {
        status(state, "chat").starts_with("failed:") && status(state, "chat").contains("Codex")
    })?;
    if !status(&state, "chat").ends_with("Install it, or choose another tool in Settings under General.") {
        return Err(format!("the one tool missing isn't worded for it: {state}").into());
    }
    user.shots_in_both_themes("codex-missing")?;
    Ok(())
}

/// A click on a settings dropdown at `at` shows its menu: the page changes away from the dropdown's own row.
fn dropdown_opens(user: &mut User, at: (u32, u32)) -> SandboxResult<bool> {
    let page = Rect {
        x0: 330,
        y0: 60,
        x1: 1270,
        y1: 795,
    };
    let before = user.steady(page)?;
    user.click(at)?;
    user.park()?;
    thread::sleep(SETTLE);
    let after = user.look("dropdown.png")?;
    let changed = after
        .enumerate_pixels()
        .filter(|&(x, y, pixel)| {
            page.contains((x, y))
                && !(at.1 - 16..=at.1 + 16).contains(&y)
                && pixel
                    .0
                    .iter()
                    .zip(before.get_pixel(x, y).0)
                    .any(|(&got, was)| got.abs_diff(was) > 6)
        })
        .count();
    Ok(changed > 400)
}

/// The centre of the lowest selected segment on the settings' page as it is now.
fn lowest_segment_center(user: &mut User) -> SandboxResult<(u32, u32)> {
    let page = Rect {
        x0: 330,
        y0: 50,
        x1: 1270,
        y1: 790,
    };
    let screen = user.steady(page)?;
    Ok(bounds_lowest_segment(&screen)
        .ok_or("no selected segment on the page")?
        .center())
}

/// The General page scrolled to its end by the wheel: the centre of Message translation's tool segment chosen,
/// Automatic (the page's last selected segment).
fn message_translation_tools(user: &mut User) -> SandboxResult<(u32, u32)> {
    for _ in 0..6 {
        user.input.scroll(740, 400, 20)?;
    }
    user.park()?;
    let automatic = lowest_segment_center(user)?;
    if automatic.1 < 450 {
        return Err(format!("the page isn't scrolled to Message translation: {automatic:?}").into());
    }
    Ok(automatic)
}

/// The lowest run of rows in the settings' page with a wide stretch of the brand's blues: a selected segment.
fn bounds_lowest_segment(shot: &image::RgbImage) -> Option<Rect> {
    let page = 800..shot.width().min(1270);
    let lit = |y: u32| page.clone().filter(|&x| brandish(*shot.get_pixel(x, y))).count() >= 40;
    // above the window's frame along the screen's foot, drawn in the brand's blues too
    let mut bottom = shot.height().saturating_sub(12);
    loop {
        let found = (60..bottom).rev().find(|&y| lit(y))?;
        let top = (60..=found).rev().take_while(|&y| lit(y)).last()?;
        let band = Rect {
            x0: page.start,
            y0: top,
            x1: page.end,
            y1: found + 1,
        };
        // its filled columns: text's coloured fringes around it count only a few
        let filled = |x: u32| (band.y0..band.y1).filter(|&y| brandish(*shot.get_pixel(x, y))).count() >= 15;
        let columns: Vec<u32> = page.clone().filter(|&x| filled(x)).collect();
        let segment = match (columns.first(), columns.last()) {
            (Some(&x0), Some(&x1)) => Rect {
                x0,
                y0: band.y0,
                x1: x1 + 1,
                y1: band.y1,
            },
            _ => band,
        };
        if (20..=40).contains(&(segment.y1 - segment.y0)) && (44..=120).contains(&segment.width()) {
            return Some(segment);
        }
        bottom = top;
    }
}

// --- the child: the app on the mock, answering the parent ---

/// One account's mock token, for the transport and the push channel alike; it never runs out in a test.
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

/// The mock, with a's own message to edit, and a's engine on it.
struct MockLab {
    _runtime: tokio::runtime::Runtime,
    _mock: ChatServiceMock,
    _trouter: TrouterMock,
    dir: tempfile::TempDir,
    /// a's own message's id.
    own: String,
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
            _runtime: runtime,
            _mock: mock,
            _trouter: trouter,
            dir,
            own,
        };
        Ok((lab, engine))
    }
}

/// a's own message in the a–b chat ([`OWN_HTML`]), its mention of b named; its id.
fn seed_own(mock: &ChatServiceMock) -> SandboxResult<String> {
    let id = mock.post_as(LAB_A, AB_CHAT, OWN_HTML).ok_or("a's post refused")?;
    let mentions = json!([{
        "@type": "http://schema.skype.com/Mention", "itemid": 0, "mri": LAB_B, "mentionType": "person",
        "displayName": "Lab B",
    }])
    .to_string();
    let client_message_id = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
    let changed = mock.change_message(AB_CHAT, &client_message_id, |message| {
        message["properties"]["mentions"] = json!(mentions);
    });
    if !changed {
        return Err("a's post isn't in the mock".into());
    }
    Ok(id)
}

/// The child's `PATH` holds no tool but the made-up one.
fn only_the_made_up_tool() -> SandboxResult {
    let made_up = PathBuf::from(std::env::var_os(TOOLS_ENV).ok_or("no made-up tool's directory")?);
    let found = teams_ui::local_ai::Tools::detect();
    for backend in [teams_ui::local_ai::Backend::Claude, teams_ui::local_ai::Backend::Codex] {
        if let Ok((_, program)) = found.pick(teams_ui::local_ai::ToolChoice::Only(backend))
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
    let first = support::started(Alias::A, engine)?;
    let settings = teams_ui::SettingsFile::new(settings_path);
    let runner = support::runner(lab.dir.path())?;
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let own = lab.own.clone();
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
                answer(window, own, cx);
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
fn answer(window: WindowHandle<KitRoot>, own: String, cx: &mut App) {
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
            let reply = match verb {
                "quit" => {
                    say(&format!("re {seq} ok"));
                    cx.update(|cx| cx.quit());
                    return;
                }
                "own-id" => own.clone(),
                _ => window
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
                    .unwrap_or_else(|| "no shell".to_owned()),
            };
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn on_shell(shell: &Entity<Shell>, verb: &str, window: &mut Window, cx: &mut App) -> String {
    match verb {
        "open-ab" | "open-ac" => {
            let chat = if verb == "open-ab" { AB_CHAT } else { AC_CHAT };
            shell.update(cx, |shell, cx| shell.open_chat(chat.to_owned(), window, cx));
            "ok".to_owned()
        }
        "open-channel" => {
            let list = shell.read(cx).list().clone();
            list.update(cx, |list, cx| list.open(TEAM_CHANNEL.to_owned(), cx));
            "ok".to_owned()
        }
        "open-thread" => {
            let Some(channel) = shell.read(cx).channel().cloned() else {
                return "no channel".to_owned();
            };
            channel.update(cx, |channel, cx| {
                channel.open_thread(CHANNEL_POST.to_owned(), window, cx);
            });
            "ok".to_owned()
        }
        "open-settings" => {
            shell.update(cx, |shell, cx| {
                shell.open_settings_page(teams_ui::settings_view::Page::General, window, cx);
            });
            "ok".to_owned()
        }
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
        "state" => state_of(shell, window, cx).to_string(),
        other => format!("unknown {other}"),
    }
}

/// The boxes shown and the settings, as the parent reads them.
fn state_of(shell: &Entity<Shell>, window: &Window, cx: &App) -> Value {
    let shell_ref = shell.read(cx);
    let mut boxes = serde_json::Map::new();
    let mut menu = json!({"open": false, "rows": []});
    let mut selected = Value::Null;
    let mut chat_id = Value::Null;
    let mut replyable = false;
    if let Some(chat) = shell_ref.chat().filter(|_| matches!(shell_ref.main(), Main::Chat(_))) {
        let chat = chat.read(cx);
        selected = chat.selected().map_or(Value::Null, |id| json!(id));
        // whose the selected message is, and whether it is one to reply to: b's, with text
        replyable = chat
            .message_items()
            .find(|item| Some(item.id.as_str()) == chat.selected())
            .is_some_and(|item| {
                item.sender.as_deref() == Some(LAB_B) && matches!(item.body, teams_ui::chat_view::Body::Text(_))
            });
        if let Main::Chat(id) = shell_ref.main() {
            chat_id = json!(id);
        }
        boxes.insert("chat".to_owned(), box_state(chat.compose(), window, cx));
    }
    if let Some(channel) = shell_ref
        .channel()
        .filter(|_| matches!(shell_ref.main(), Main::Channel(_)))
    {
        let channel = channel.read(cx);
        boxes.insert(
            "post".to_owned(),
            box_state(channel.posts().read(cx).compose(), window, cx),
        );
        if let Some((_, thread)) = channel.thread() {
            boxes.insert("thread".to_owned(), box_state(thread.read(cx).compose(), window, cx));
        }
    }
    if let Some(new_chat) = shell_ref
        .new_chat()
        .filter(|_| matches!(shell_ref.main(), Main::NewChat))
    {
        boxes.insert("new".to_owned(), box_state(&new_chat.read(cx).compose(cx), window, cx));
    }
    for compose in boxes.values() {
        if compose["menu"]["open"] == true {
            menu = compose["menu"].clone();
        }
    }
    let main = match shell_ref.main() {
        Main::Chat(_) => "chat".to_owned(),
        Main::Channel(_) => "channel".to_owned(),
        Main::NewChat => "new".to_owned(),
        Main::Settings => format!("settings:{:?}", shell_ref.settings_view().read(cx).page()),
        other => format!("{other:?}"),
    };
    let settings = shell_ref.settings().read(cx).settings();
    let choice = settings.ai(teams_ui::local_ai::Work::Draft);
    json!({
        "main": main,
        "chat": chat_id,
        "selected": selected,
        "replyable": replyable,
        "boxes": boxes,
        "menu": menu,
        "language": settings.message_translate_to,
        "tool": format!("{:?}", choice.tool),
        "model": format!("{:?}", choice.model),
        "claude_models": cx
            .try_global::<teams_ui::local_ai::Catalog>()
            .map_or(Vec::new(), |catalog| {
                catalog
                    .models(teams_ui::local_ai::Backend::Claude)
                    .iter()
                    .map(|listed| listed.id.clone())
                    .collect()
            }),
    })
}

/// One box: its text, structure, translation and correction, mode, focus, and its language menu.
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
                        "code": run.attrs.marks.contains(Marks::CODE),
                        "link": run.attrs.link,
                        "mention": run.attrs.mention,
                    })
                })
                .collect();
            json!({"kind": format!("{:?}", block.kind), "runs": runs})
        })
        .collect();
    let status = match compose.draft_translation_status() {
        teams_ui::compose::DraftTranslationStatus::Idle => "idle".to_owned(),
        teams_ui::compose::DraftTranslationStatus::Running => "running".to_owned(),
        teams_ui::compose::DraftTranslationStatus::NothingToTranslate => "nothing".to_owned(),
        teams_ui::compose::DraftTranslationStatus::DraftChanged => "changed".to_owned(),
        teams_ui::compose::DraftTranslationStatus::Failed(why) => format!("failed: {why}"),
    };
    let mode = match compose.mode() {
        Mode::New => "new",
        Mode::Reply(_) => "reply",
        Mode::Edit { .. } => "edit",
    };
    let menu = compose.language_menu().map_or_else(
        || json!({"open": false, "rows": []}),
        |menu| {
            let menu = menu.read(cx);
            let rows: Vec<String> = menu
                .rows(cx)
                .into_iter()
                .take(6)
                .map(|row| match row {
                    Row::Typed(name) if menu.chosen(cx) != name => format!("use:{name}"),
                    row => row.language().to_owned(),
                })
                .collect();
            json!({"open": menu.is_open(), "rows": rows})
        },
    );
    json!({
        "text": doc.plain_text(),
        "blocks": blocks,
        "status": status,
        "correction": format!("{:?}", compose.correction_status()),
        "mode": mode,
        "focused": compose.focused(window, cx),
        "narrow": compose.is_narrow(),
        "mentioning": compose.picker().map_or(0, |picker| picker.matches.len()),
        "menu": menu,
    })
}
