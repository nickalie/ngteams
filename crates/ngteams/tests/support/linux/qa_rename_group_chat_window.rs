//! QA of task 661 (round 1), in the UI sandbox: a user names, renames and unnames a group chat ("there is no way to
//! change Group Chat name"). The real app runs as a child in the sandbox ([`child`]); the user works with the sandbox's
//! pointer and keyboard. The test finds the header's pencil and the popup's buttons in screenshots, as a user's eye
//! does: the pencil is the mark after the chat's name, the popup's Save is the filled brand-coloured button (a focus
//! line or a border is never taken for it).
//!
//! Scenes ([`SCENES`]; `QA_RGC_SCENES` names the ones to run):
//! - `mock`: the parent runs the mock chat service and Trouter; the app runs mock accounts a and b in one window, and
//!   the a–b–c group chat is renamed by both. The pencil opens "Group name" with the current name; Escape and Cancel
//!   send nothing; Save and Enter rename it; the name is trimmed; a same name sends nothing; the box takes 250
//!   characters and says Teams' words past them; an empty name takes the name off; a failed save says so, keeps the
//!   old name and a retry works. The new name shows in the list, the header, Ctrl+K and the "changed the group name"
//!   line, for b too, without a sync by hand. No pencil in a 1:1 chat or a meeting chat. Teams Light and Dark, a font
//!   of 20 px. The name stays after a restart. The read-only mode shows no pencil. b and a see the other's name from
//!   the thread's `ThreadUpdate` the mock pushes, as Teams does.
//! - `narrow`: a window 640 px wide: the popup and its buttons whole, in Light, Dark and a font of 20 px; then the
//!   window narrowed to 480 and 360 px under the open popup: the popup moves and narrows to stay whole in it.
//! - `own`: the real `ngteams-server` binary; the app runs its made-up account a, b is a client in the parent. a
//!   renames the group chat b made: b's store has the name. b renames and unnames it: a's window shows it at once.
//!
//! `QA_RGC_EVIDENCE` names a directory that gets the screenshots and notes too; they also go to
//! `target/ui-shots/qa661-*.png`. Synthetic names only. Without weston 15, niri, `dbus-daemon` (or, for `own`, the
//! server binary) it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};
use std::{fs, thread};

use image::RgbImage;
use serde_json::Value;
use teams_mock::{AB_CHAT, ABC_CHAT, ChatServiceMock, LAB_A, LAB_B};
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

#[path = "qa_rename_group_chat_child.rs"]
mod child;
#[path = "qa_rename_group_chat_own.rs"]
mod own;
#[path = "qa_rename_group_chat_pixels.rs"]
mod pixels;

use pixels::{Blob, Rgb};

const EVIDENCE_ENV: &str = "QA_RGC_EVIDENCE";
const STARTUP: Duration = Duration::from_secs(180);
const WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(150);
const SETTLE: Duration = Duration::from_millis(700);
const STEP: Duration = Duration::from_millis(250);
/// The rail's width, px.
const RAIL: u32 = 76;
/// The title bar's height, px.
const TITLE: u32 = 48;
/// The meeting chat the parent adds to the mock for a and b.
const MEETING: &str = "19:meeting_qa661madeup@thread.v2";
/// What the popup says past 250 characters, and when a save failed (`teams_ui::chat_rename`).
const TOO_LONG: &str = "Your group name can't be longer than 250 characters.";
const NOT_SAVED: &str = "We couldn't save the group name. Check your connection and try again.";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(child::CHILD_ENV) {
        Ok(mode) => child::child(&mode),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_rename_group_chat_window: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The environment variable that hands mock account `letter`'s token to the child.
fn token_env(letter: char) -> String {
    format!("QA_RGC_TOKEN_{}", letter.to_ascii_uppercase())
}

/// The own server's users' password, for the child.
fn own_password() -> &'static str {
    own::password()
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn ensure(holds: bool, what: impl FnOnce() -> String) -> SandboxResult {
    if holds { Ok(()) } else { Err(what().into()) }
}

fn evidence() -> Option<PathBuf> {
    std::env::var_os(EVIDENCE_ENV).map(PathBuf::from)
}

/// Adds `line` to the evidence notes and says it.
fn note(line: &str) {
    say(&format!("note {line}"));
    if let Some(dir) = evidence() {
        let _ = fs::create_dir_all(&dir);
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("notes.txt"))
        {
            let _ = writeln!(file, "{line}");
        }
    }
}

type Scene = (&'static str, fn() -> SandboxResult);

const SCENES: [Scene; 3] = [("mock", mock_scene), ("narrow", narrow), ("own", own::scene)];

fn parent() -> SandboxResult {
    let only = std::env::var("QA_RGC_SCENES").unwrap_or_default();
    let wanted: Vec<&str> = only.split(',').map(str::trim).filter(|name| !name.is_empty()).collect();
    let unknown = wanted.iter().any(|name| !SCENES.iter().any(|(scene, _)| scene == name));
    if unknown || (!only.trim().is_empty() && wanted.is_empty()) {
        return Err(
            format!("QA_RGC_SCENES={only:?} names an unknown scene or none; the scenes: mock,narrow,own").into(),
        );
    }
    let mut failed = Vec::new();
    let mut passed = 0_usize;
    for (name, scene) in SCENES {
        if !wanted.is_empty() && !wanted.contains(&name) {
            continue;
        }
        match scene() {
            Ok(()) => {
                passed += 1;
                say(&format!("ok {name}"));
            }
            Err(error) => {
                say(&format!("FAILED {name}: {error}"));
                failed.push(format!("{name}: {error}"));
            }
        }
    }
    say(&format!("scenes: {passed} passed, {} failed", failed.len()));
    if failed.is_empty() {
        say("qa_rename_group_chat_window: ok");
        Ok(())
    } else {
        Err(failed.join("\n").into())
    }
}

fn sandbox(width: u32) -> SandboxResult<Option<Sandbox>> {
    let options = SandboxOptions {
        width,
        height: 800,
        niri_extra: NIRI_FULL_WINDOW,
        fill_output: true,
        ..SandboxOptions::default()
    };
    sandbox_or_skip("qa_rename_group_chat_window", &options)
}

/// The parent's mock chat service and Trouter, with a meeting chat of a and b besides the fixtures.
struct Mock {
    runtime: tokio::runtime::Runtime,
    service: ChatServiceMock,
    trouter: teams_mock::TrouterMock,
}

impl Mock {
    fn start() -> SandboxResult<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        ensure(
            mock.add_meeting_chat(MEETING, "Made-up planning", &[LAB_A, LAB_B]),
            || "the mock made no meeting chat".to_owned(),
        )?;
        // a chat with no message isn't listed: b says hello in each
        for chat in [ABC_CHAT, MEETING] {
            ensure(mock.post_as(LAB_B, chat, "<p>Made-up hello</p>").is_some(), || {
                format!("the mock refused b's hello in {chat}")
            })?;
        }
        Ok(Self {
            runtime,
            service: mock,
            trouter,
        })
    }

    /// The environment the child reaches the mock with.
    fn env(&self) -> SandboxResult<Vec<(String, String)>> {
        let mut env = vec![
            (child::ORIGIN_ENV.to_owned(), self.service.origin()),
            (child::TROUTER_ENV.to_owned(), self.trouter.origin().to_owned()),
        ];
        for (letter, mri) in [('a', LAB_A), ('b', LAB_B)] {
            env.push((token_env(letter), self.service.token(mri).ok_or("the mock's token")?));
        }
        Ok(env)
    }

    /// The names every rename the mock got carried, in order (`PUT …/threads/<chat>/properties?name=topic`).
    fn renames(&self) -> Vec<String> {
        let requests = self.runtime.block_on(self.service.received_requests());
        requests
            .iter()
            .filter(|request| {
                request.method.as_str() == "PUT"
                    && request.url.path().ends_with("/properties")
                    && request
                        .url
                        .query_pairs()
                        .any(|(key, value)| key == "name" && value == "topic")
            })
            .filter_map(|request| serde_json::from_slice::<Value>(&request.body).ok())
            .map(|body| body["topic"].as_str().unwrap_or("<no topic>").to_owned())
            .collect()
    }

    /// The next rename fails as a lost gateway does (502): the world doesn't change.
    fn fail_next_rename(&self) {
        self.runtime
            .block_on(self.service.refuse_next("PUT", "/properties", 502));
    }
}

// --- the user ---

/// The user at the app in the sandbox.
struct User<'a> {
    sandbox: &'a Sandbox,
    running: Option<Running>,
    input: VirtualInput,
    asked: u32,
    /// Where the pointer rests: on the rail, below its buttons, away from the header and the popup.
    rest: (u32, u32),
}

impl<'a> User<'a> {
    /// The app started in the sandbox with `mode` ([`child::CHILD_ENV`]) on the files in `root`, with `env` besides.
    fn start(sandbox: &'a Sandbox, root: &Path, mode: &str, env: &[(String, String)]) -> SandboxResult<Self> {
        let root = root.to_string_lossy().into_owned();
        let mut vars: Vec<(&str, &str)> = vec![(child::CHILD_ENV, mode), (child::ROOT_ENV, &root)];
        vars.extend(env.iter().map(|(name, value)| (name.as_str(), value.as_str())));
        let spawned = sandbox.spawn(&std::env::current_exe()?, &[], &vars)?;
        let mut running = Running::new(spawned)?;
        if let Err(error) = running.line_where(STARTUP, |line| line == "ready") {
            return Err(running.fail(&format!("no ready line: {error:?}{}", sandbox.logs())));
        }
        let mut user = Self {
            sandbox,
            running: Some(running),
            input: sandbox.input()?,
            asked: 0,
            rest: (RAIL / 2, 560),
        };
        user.park()?;
        thread::sleep(SETTLE);
        Ok(user)
    }

    /// Ends the run: a screenshot when `outcome` failed, then the app quits.
    fn finish(&mut self, outcome: SandboxResult) -> SandboxResult {
        if outcome.is_err() {
            let _ = self.shot("failed");
        }
        let quit = self.quit();
        outcome?;
        quit
    }

    fn running(&self) -> SandboxResult<&Running> {
        self.running.as_ref().ok_or_else(|| "the app has quit".into())
    }

    fn ask(&mut self, verb: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = self.sandbox.path(&format!("{}.staged", child::ASK));
        fs::write(&staged, format!("{} {verb}", self.asked))?;
        fs::rename(&staged, self.sandbox.path(child::ASK))?;
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
                return Err(format!("{what}: never came; last {state}").into());
            }
            thread::sleep(POLL);
        }
    }

    fn quit(&mut self) -> SandboxResult {
        if self.running.is_none() {
            return Ok(());
        }
        self.ask("quit")?;
        let running = self.running.take().ok_or("the app has quit")?;
        let (status, stderr) = running.finish(WAIT)?;
        ensure(status.success(), || format!("the app exited {status}: {stderr}"))
    }

    fn keys(&mut self, specs: &[&str]) -> SandboxResult {
        for spec in specs {
            let (modifiers, key) =
                ui_sandbox::input::parse_key(spec, self.input.keymap()).ok_or_else(|| format!("no key {spec}"))?;
            self.input.key_with(&modifiers, key)?;
            thread::sleep(STEP);
        }
        Ok(())
    }

    fn type_text(&mut self, text: &str) -> SandboxResult {
        self.input.type_text(text)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn click(&mut self, (x, y): (u32, u32)) -> SandboxResult {
        self.input.move_to(x, y)?;
        thread::sleep(Duration::from_millis(80));
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(STEP);
        self.park()?;
        thread::sleep(STEP);
        Ok(())
    }

    fn park(&mut self) -> SandboxResult {
        self.input.move_to(self.rest.0, self.rest.1)
    }

    /// A screenshot as `qa661-<name>.png`, under `target/ui-shots/` and the evidence dir's `shots/`.
    fn shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        self.park()?;
        thread::sleep(SETTLE);
        let file = format!("qa661-{name}.png");
        let png = self.sandbox.screenshot(&file, false)?;
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .parent()
            .map_or_else(|| PathBuf::from("target"), Path::to_owned)
            .join("ui-shots");
        fs::create_dir_all(&dir)?;
        let kept = dir.join(&file);
        fs::copy(&png, &kept)?;
        if let Some(dir) = evidence() {
            let dir = dir.join("shots");
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(&file))?;
        }
        Ok(kept)
    }

    /// niri sizes the app's window `width` px wide, as a user's drag does; the window keeps the screen's left edge.
    fn resize(&mut self, width: u32) -> SandboxResult {
        self.sandbox
            .niri_msg(&["action", "set-column-width", &width.to_string()])?;
        let started = Instant::now();
        loop {
            let windows: Value = serde_json::from_str(&self.sandbox.niri_msg(&["-j", "windows"])?)?;
            let took = windows.as_array().into_iter().flatten().any(|window| {
                window["is_focused"] == true && window["layout"]["window_size"][0].as_f64() == Some(f64::from(width))
            });
            if took {
                thread::sleep(SETTLE);
                return Ok(());
            }
            ensure(started.elapsed() < WAIT, || format!("the window never took {width} px"))?;
            thread::sleep(STEP);
        }
    }

    /// The screen now, the pointer parked.
    fn image(&mut self) -> SandboxResult<RgbImage> {
        self.park()?;
        thread::sleep(SETTLE);
        let png = self.sandbox.screenshot("qa661-probe.png", false)?;
        Ok(image::open(&png)?.to_rgb8())
    }

    /// The palette's colour `name` now.
    fn colour(&mut self, name: &str) -> SandboxResult<Rgb> {
        let state = self.state()?;
        pixels::rgb_of(state["palette"][name].as_str().unwrap_or_default())
            .ok_or_else(|| format!("no palette colour {name}: {}", state["palette"]).into())
    }

    /// The header's pencil on the screen now: the mark right after the chat's name.
    fn pencil(&mut self) -> SandboxResult<Blob> {
        let image = self.image()?;
        let border = self.colour("border")?;
        pixels::pencil(&image, border).ok_or_else(|| "no mark after the chat's name in the header".into())
    }

    /// Whether the header shows `pencil` (a crop of the pencil drawn in a group chat's header in the same look).
    fn shows_pencil(&mut self, pencil: &RgbImage) -> SandboxResult<bool> {
        let image = self.image()?;
        Ok(pixels::find_in_header(&image, pencil).is_some())
    }

    /// A click on the header's pencil; the popup opens on chat `chat` with `value` in its box.
    fn open_popup(&mut self, chat: &str, value: &str) -> SandboxResult<Value> {
        let pencil = self.pencil()?;
        self.click(pencil.centre())?;
        let state = self.until("the popup open", |state| !state["rename"].is_null())?;
        ensure(
            state["rename"]["chat"] == chat && state["rename"]["value"] == value,
            || {
                format!(
                    "the popup opened on {} with {:?}; expected {chat} with {value:?}",
                    state["rename"]["chat"], state["rename"]["value"]
                )
            },
        )?;
        Ok(state)
    }

    /// The popup's Save button on the screen now.
    fn save_button(&mut self) -> SandboxResult<Blob> {
        let image = self.image()?;
        let brand = self.colour("brand")?;
        pixels::filled_button(&image, brand, TITLE, 60).ok_or_else(|| "no Save button (a filled brand button)".into())
    }

    /// The popup's Cancel: the same size as Save, 16 px left of it.
    fn cancel_button(&mut self) -> SandboxResult<(u32, u32)> {
        let save = self.save_button()?;
        let (x, y) = save.centre();
        Ok((x.saturating_sub(save.width() + 16), y))
    }

    /// The row of chat `id` in `state`.
    fn row<'s>(state: &'s Value, id: &str) -> Option<&'s Value> {
        state["rows"].as_array()?.iter().find(|row| row["id"] == id)
    }

    /// Waits until chat `id`'s row is called `name`.
    fn row_named(&mut self, id: &str, name: &str) -> SandboxResult<Value> {
        self.until(&format!("the row of {id} called {name:?}"), |state| {
            Self::row(state, id).is_some_and(|row| row["name"] == name)
        })
    }

    /// Opens chat `id` with Ctrl+K: `query` typed, the first match is `id`, Enter.
    fn open_by_switcher(&mut self, query: &str, id: &str) -> SandboxResult {
        self.keys(&["ctrl+k"])?;
        self.until("Ctrl+K open", |state| !state["switcher"].is_null())?;
        self.type_text(query)?;
        self.until(&format!("Ctrl+K's first match for {query:?} is {id}"), |state| {
            state["switcher"]["matches"][0][0] == id
        })?;
        self.keys(&["enter"])?;
        self.until(&format!("chat {id} open"), |state| state["chat"] == id)?;
        Ok(())
    }
}

impl Drop for User<'_> {
    fn drop(&mut self) {
        if let Some(mut running) = self.running.take() {
            let _ = running.fail(&"dropped");
        }
    }
}

// --- the scenes ---

/// The mock scene (see the module docs).
fn mock_scene() -> SandboxResult {
    let Some(sandbox) = sandbox(1280)? else {
        return Ok(());
    };
    let mock = Mock::start()?;
    let env = mock.env()?;
    let root = tempfile::tempdir()?;
    let mut user = User::start(&sandbox, root.path(), "mock:ab", &env)?;
    let (outcome, pencil) = match mock_flows(&mut user, &mock) {
        Ok(pencil) => (Ok(()), Some(pencil)),
        Err(error) => (Err(error), None),
    };
    user.finish(outcome)?;
    let pencil = pencil.ok_or("no pencil picture")?;
    let mut user = User::start(&sandbox, root.path(), "mock:ab", &env)?;
    let outcome = after_restart(&mut user);
    user.finish(outcome)?;
    let mut user = User::start(&sandbox, root.path(), "mock:ab:ro", &env)?;
    let outcome = read_only(&mut user, &pencil);
    user.finish(outcome)
}

/// The flows of the first run; the pencil's picture in Teams Light at 100 %.
fn mock_flows(user: &mut User<'_>, mock: &Mock) -> SandboxResult<RgbImage> {
    let state = user.until("a shown, the group chat listed", |state| {
        state["shown"] == "a" && User::row(state, ABC_CHAT).is_some()
    })?;
    let row = User::row(&state, ABC_CHAT).cloned().unwrap_or_default();
    note(&format!("a's group chat at the start: {row}"));
    let first = row["name"].as_str().unwrap_or_default().to_owned();
    let unnamed = row["chat_with"].is_string() || row["named"] == false;
    user.open_by_switcher(&first, ABC_CHAT)?;
    user.shot("1-group-chat")?;
    let pencil = pencil_picture(user)?;
    let current = if unnamed { String::new() } else { first.clone() };
    escape_and_cancel(user, mock, &current, &first)?;
    save_and_enter(user, mock, &current)?;
    b_sees_and_renames(user, mock)?;
    limit(user)?;
    failure_and_retry(user, mock)?;
    unname(user, mock)?;
    no_pencil_elsewhere(user, &pencil)?;
    looks(user)?;
    Ok(pencil)
}

/// The pencil found, cut out of the screen (for the chats that must not show it), its tooltip's place hovered.
fn pencil_picture(user: &mut User<'_>) -> SandboxResult<RgbImage> {
    let blob = user.pencil()?;
    let image = user.image()?;
    note(&format!("the header's pencil at {blob:?}"));
    let (x, y) = blob.centre();
    user.input.move_to(x, y)?;
    thread::sleep(Duration::from_millis(1500));
    let png = user.sandbox.screenshot("qa661-2-pencil-hover.png", false)?;
    if let Some(dir) = evidence() {
        let dir = dir.join("shots");
        fs::create_dir_all(&dir)?;
        fs::copy(&png, dir.join("qa661-2-pencil-hover.png"))?;
    }
    Ok(pixels::crop(&image, blob))
}

/// Escape and Cancel close the popup with nothing sent; the name stays.
fn escape_and_cancel(user: &mut User<'_>, mock: &Mock, current: &str, first: &str) -> SandboxResult {
    user.open_popup(ABC_CHAT, current)?;
    user.shot("3-popup-open")?;
    user.type_text("Made-up escaped")?;
    user.keys(&["escape"])?;
    user.until("Escape closed the popup", |state| state["rename"].is_null())?;
    user.open_popup(ABC_CHAT, current)?;
    user.type_text("Made-up cancelled")?;
    let cancel = user.cancel_button()?;
    user.click(cancel)?;
    user.until("Cancel closed the popup", |state| state["rename"].is_null())?;
    thread::sleep(SETTLE);
    let renames = mock.renames();
    ensure(renames.is_empty(), || {
        format!("Escape or Cancel sent a rename: {renames:?}")
    })?;
    user.row_named(ABC_CHAT, first)?;
    note("Escape and Cancel: closed, nothing sent, the name stays");
    Ok(())
}

/// Save renames it (trimmed), at once in the list, the header, Ctrl+K and its line; a same name sends nothing; Enter
/// renames it too.
fn save_and_enter(user: &mut User<'_>, mock: &Mock, current: &str) -> SandboxResult {
    user.open_popup(ABC_CHAT, current)?;
    user.type_text("   QA made-up group   ")?;
    let save = user.save_button()?;
    note(&format!("the popup's Save at {save:?}"));
    user.click(save.centre())?;
    user.until("Save closed the popup", |state| state["rename"].is_null())?;
    let renames = mock.renames();
    ensure(renames == ["QA made-up group"], || {
        format!("Save sent {renames:?}; expected the trimmed name")
    })?;
    user.row_named(ABC_CHAT, "QA made-up group")?;
    user.until("a's line: changed the group name", |state| {
        events_hold(state, "changed the group name to QA made-up group")
    })?;
    user.shot("4-renamed")?;
    user.keys(&["ctrl+k"])?;
    user.type_text("made-up group")?;
    let state = user.until("Ctrl+K lists the new name", |state| {
        state["switcher"]["matches"].as_array().is_some_and(|rows| {
            rows.iter()
                .any(|row| row[0] == ABC_CHAT && row[1] == "QA made-up group")
        })
    })?;
    note(&format!(
        "Ctrl+K for \"made-up group\": {}",
        state["switcher"]["matches"]
    ));
    user.shot("5-ctrl-k")?;
    user.keys(&["escape"])?;
    user.open_popup(ABC_CHAT, "QA made-up group")?;
    user.keys(&["enter"])?;
    user.until("Enter with the same name closed the popup", |state| {
        state["rename"].is_null()
    })?;
    thread::sleep(SETTLE);
    let renames = mock.renames();
    ensure(renames.len() == 1, || {
        format!("the same name sent a rename: {renames:?}")
    })?;
    user.open_popup(ABC_CHAT, "QA made-up group")?;
    user.type_text("Made-up second name")?;
    user.keys(&["enter"])?;
    user.until("Enter closed the popup", |state| state["rename"].is_null())?;
    user.row_named(ABC_CHAT, "Made-up second name")?;
    note("Save and Enter rename it, trimmed; the same name sends nothing");
    Ok(())
}

fn events_hold(state: &Value, words: &str) -> bool {
    state["events"].as_array().is_some_and(|lines| {
        lines
            .iter()
            .any(|line| line.as_str().is_some_and(|line| line.contains(words)))
    })
}

/// b (the next account, Ctrl+Alt+Down) sees a's name at once, renames it with the pencil; a sees b's name.
fn b_sees_and_renames(user: &mut User<'_>, mock: &Mock) -> SandboxResult {
    user.keys(&["ctrl+alt+down"])?;
    user.until("b shown", |state| state["shown"] == "b")?;
    let state = user.row_named(ABC_CHAT, "Made-up second name")?;
    note(&format!(
        "b's row: {}",
        User::row(&state, ABC_CHAT).cloned().unwrap_or_default()
    ));
    user.open_by_switcher("second name", ABC_CHAT)?;
    let state = user.until("b's line: a changed the group name", |state| {
        events_hold(state, "changed the group name to Made-up second name")
    })?;
    note(&format!("b's lines: {}", state["events"]));
    user.shot("6-b-sees-it")?;
    user.open_popup(ABC_CHAT, "Made-up second name")?;
    user.type_text("Named by b")?;
    let save = user.save_button()?;
    user.click(save.centre())?;
    user.until("b's Save closed the popup", |state| state["rename"].is_null())?;
    user.row_named(ABC_CHAT, "Named by b")?;
    user.keys(&["ctrl+alt+up"])?;
    user.until("a shown", |state| state["shown"] == "a")?;
    let state = user.row_named(ABC_CHAT, "Named by b")?;
    ensure(state["chat"] == ABC_CHAT, || {
        format!("a's window left the chat: {}", state["chat"])
    })?;
    user.until("a's line: b changed the group name", |state| {
        events_hold(state, "changed the group name to Named by b")
    })?;
    user.shot("7-a-sees-b-s-name")?;
    note(&format!("renames sent so far: {:?}", mock.renames()));
    Ok(())
}

/// The box takes 250 characters; the 251st is refused with Teams' words. A pasted name past 250 is noted.
fn limit(user: &mut User<'_>) -> SandboxResult {
    user.open_popup(ABC_CHAT, "Named by b")?;
    user.type_text(&"x".repeat(251))?;
    let state = user.until("the box holds 250 characters", |state| {
        state["rename"]["value"]
            .as_str()
            .is_some_and(|value| value.chars().count() == 250)
    })?;
    ensure(state["rename"]["too_long"] == true, || {
        format!("no limit's words: {}", state["rename"])
    })?;
    user.shot("8-limit")?;
    user.keys(&["escape"])?;
    user.until("closed", |state| state["rename"].is_null())?;
    user.open_popup(ABC_CHAT, "Named by b")?;
    let long = "Made-up pasted name ".repeat(15);
    user.sandbox
        .set_clipboard(vec![("text/plain;charset=utf-8".to_owned(), long.clone().into_bytes())])?;
    thread::sleep(STEP);
    user.keys(&["ctrl+v"])?;
    thread::sleep(SETTLE);
    let state = user.state()?;
    note(&format!(
        "a paste of {} characters over the selected name: the box holds {} characters, too_long {}",
        long.chars().count(),
        state["rename"]["value"]
            .as_str()
            .map_or(0, |value| value.chars().count()),
        state["rename"]["too_long"]
    ));
    user.shot("9-limit-paste")?;
    user.keys(&["escape"])?;
    user.until("closed", |state| state["rename"].is_null())?;
    note(&format!("the limit's words: {TOO_LONG}"));
    Ok(())
}

/// A failed save says so in the popup and keeps the old name; Save again renames it.
fn failure_and_retry(user: &mut User<'_>, mock: &Mock) -> SandboxResult {
    mock.fail_next_rename();
    user.open_popup(ABC_CHAT, "Named by b")?;
    user.type_text("Made-up after a failure")?;
    user.keys(&["enter"])?;
    let state = user.until("the popup says the save failed", |state| {
        state["rename"]["problem"] == NOT_SAVED
    })?;
    ensure(state["rename"]["value"] == "Made-up after a failure", || {
        format!("the failure lost the name typed: {}", state["rename"])
    })?;
    let row = User::row(&state, ABC_CHAT).cloned().unwrap_or_default();
    ensure(row["name"] == "Named by b", || {
        format!("the failure changed the name: {row}")
    })?;
    user.shot("10-failed")?;
    let save = user.save_button()?;
    user.click(save.centre())?;
    user.until("Save again closed the popup", |state| state["rename"].is_null())?;
    user.row_named(ABC_CHAT, "Made-up after a failure")?;
    note("a failed save: the popup says so, the old name stays, Save again renames it");
    Ok(())
}

/// An emptied box takes the name off: the chat is called by its people; the line says the name was removed.
fn unname(user: &mut User<'_>, mock: &Mock) -> SandboxResult {
    user.open_popup(ABC_CHAT, "Made-up after a failure")?;
    user.keys(&["backspace"])?;
    user.until("the box empty", |state| state["rename"]["value"] == "")?;
    user.keys(&["enter"])?;
    user.until("closed", |state| state["rename"].is_null())?;
    let state = user.until("the chat called by its people", |state| {
        User::row(state, ABC_CHAT).is_some_and(|row| row["name"] != "Made-up after a failure")
    })?;
    let row = User::row(&state, ABC_CHAT).cloned().unwrap_or_default();
    note(&format!("unnamed: {row}"));
    ensure(mock.renames().last().is_some_and(String::is_empty), || {
        format!("the emptied box sent {:?}", mock.renames())
    })?;
    user.until("the line: removed the group name", |state| {
        events_hold(state, "removed the group name")
    })?;
    user.shot("11-unnamed")?;
    user.open_popup(ABC_CHAT, "")?;
    user.type_text("QA made-up group")?;
    user.keys(&["enter"])?;
    user.until("closed", |state| state["rename"].is_null())?;
    user.row_named(ABC_CHAT, "QA made-up group")?;
    Ok(())
}

/// A 1:1 chat and a meeting chat show no pencil.
fn no_pencil_elsewhere(user: &mut User<'_>, pencil: &RgbImage) -> SandboxResult {
    ensure(user.shows_pencil(pencil)?, || {
        "the pencil's picture isn't found in its own chat".to_owned()
    })?;
    for (chat, name) in [(AB_CHAT, "1-to-1"), (MEETING, "meeting")] {
        user.ask(&format!("open-chat {chat}"))?;
        user.until(&format!("chat {chat} open"), |state| state["chat"] == chat)?;
        thread::sleep(SETTLE);
        user.shot(&format!("12-no-pencil-{name}"))?;
        ensure(!user.shows_pencil(pencil)?, || {
            format!("the {name} chat's header shows the pencil")
        })?;
    }
    note("no pencil in the 1:1 chat and the meeting chat");
    Ok(())
}

/// The popup in Teams Light and Dark, and at a font of 20 px: whole on the screen.
fn looks(user: &mut User<'_>) -> SandboxResult {
    user.ask(&format!("open-chat {ABC_CHAT}"))?;
    user.until("the group chat open", |state| state["chat"] == ABC_CHAT)?;
    for (theme, font) in [("light", 14), ("dark", 14), ("dark", 20), ("light", 20)] {
        user.ask(&format!("theme {theme}"))?;
        user.ask(&format!("font {font}"))?;
        thread::sleep(SETTLE);
        user.open_popup(ABC_CHAT, "QA made-up group")?;
        popup_whole(user, &format!("13-popup-{theme}-{font}"))?;
        user.keys(&["escape"])?;
        user.until("closed", |state| state["rename"].is_null())?;
    }
    user.ask("font 14")?;
    user.ask("theme light")?;
    Ok(())
}

/// The popup's Save and Cancel whole on the screen (`name`'s shot).
fn popup_whole(user: &mut User<'_>, name: &str) -> SandboxResult {
    popup_whole_in(user, name, None)
}

/// The popup's Save and Cancel whole in the app's window, `window` px wide from the screen's left edge (the whole
/// screen when `None`), in `name`'s shot.
fn popup_whole_in(user: &mut User<'_>, name: &str, window: Option<u32>) -> SandboxResult {
    let shot = user.shot(name)?;
    let image = image::open(&shot)?.to_rgb8();
    let brand = user.colour("brand")?;
    // a part of a button counts too: one cut at the window's edge is still seen
    let save = pixels::filled_button(&image, brand, TITLE, 20).ok_or("no Save button at all")?;
    let (screen, height) = image.dimensions();
    let width = window.unwrap_or(screen);
    ensure(save.x1 + 2 < width && save.y1 + 2 < height, || {
        format!("{name}: the popup runs past the window's edge: Save {save:?} is cut at {width}x{height}")
    })?;
    let cancel = save.x0.saturating_sub(16 + save.width());
    ensure(save.width() >= 60 && cancel > RAIL, || {
        format!("{name}: the popup's buttons are cut: Save {save:?}, Cancel from x {cancel}")
    })?;
    note(&format!("{name}: Save at {save:?}"));
    Ok(())
}

/// The name stays after a restart.
fn after_restart(user: &mut User<'_>) -> SandboxResult {
    let state = user.until("a shown", |state| state["shown"] == "a")?;
    let row = User::row(&state, ABC_CHAT).cloned().unwrap_or_default();
    note(&format!("after a restart, at once: {row}"));
    user.row_named(ABC_CHAT, "QA made-up group")?;
    user.ask(&format!("open-chat {ABC_CHAT}"))?;
    user.until("the group chat open", |state| state["chat"] == ABC_CHAT)?;
    user.shot("14-after-restart")?;
    Ok(())
}

/// The read-only mode shows no pencil, and its place opens nothing.
fn read_only(user: &mut User<'_>, pencil: &RgbImage) -> SandboxResult {
    user.until("a shown", |state| state["shown"] == "a")?;
    user.ask(&format!("open-chat {ABC_CHAT}"))?;
    user.until("the group chat open", |state| state["chat"] == ABC_CHAT)?;
    thread::sleep(SETTLE);
    user.shot("15-read-only")?;
    ensure(!user.shows_pencil(pencil)?, || {
        "the read-only mode shows the pencil".to_owned()
    })?;
    note("read-only: no pencil");
    Ok(())
}

/// A window 640 px wide: the popup and its buttons whole in Light, Dark and a font of 20 px; and in a window narrowed
/// to 480 and 360 px.
fn narrow() -> SandboxResult {
    let Some(sandbox) = sandbox(640)? else {
        return Ok(());
    };
    let mock = Mock::start()?;
    let env = mock.env()?;
    let root = tempfile::tempdir()?;
    let mut user = User::start(&sandbox, root.path(), "mock:a", &env)?;
    let outcome = narrow_flows(&mut user);
    user.finish(outcome)
}

fn narrow_flows(user: &mut User<'_>) -> SandboxResult {
    let state = user.until("the group chat listed", |state| User::row(state, ABC_CHAT).is_some())?;
    let row = User::row(&state, ABC_CHAT).cloned().unwrap_or_default();
    let name = row["name"].as_str().unwrap_or_default().to_owned();
    user.open_by_switcher(&name, ABC_CHAT)?;
    user.shot("20-narrow-chat")?;
    user.open_popup(ABC_CHAT, "")?;
    user.type_text("A made-up name long enough to fill the box of the popup")?;
    user.keys(&["enter"])?;
    user.row_named(ABC_CHAT, "A made-up name long enough to fill the box of the popup")?;
    user.shot("21-narrow-renamed")?;
    let mut cut = Vec::new();
    for (theme, font) in [("light", 14), ("dark", 14), ("light", 20)] {
        user.ask(&format!("theme {theme}"))?;
        user.ask(&format!("font {font}"))?;
        thread::sleep(SETTLE);
        user.open_popup(ABC_CHAT, "A made-up name long enough to fill the box of the popup")?;
        // each look is shot and checked before the scene fails
        if let Err(error) = popup_whole(user, &format!("22-narrow-popup-{theme}-{font}")) {
            note(&format!("FINDING {error}"));
            cut.push(error.to_string());
        }
        user.keys(&["escape"])?;
        user.until("closed", |state| state["rename"].is_null())?;
    }
    user.ask("font 14")?;
    user.ask("theme light")?;
    // narrower still: the user narrows the window under the open popup (the pane is too narrow for the pencil then)
    user.open_popup(ABC_CHAT, "A made-up name long enough to fill the box of the popup")?;
    for width in [480, 360] {
        user.resize(width)?;
        for theme in ["light", "dark"] {
            user.ask(&format!("theme {theme}"))?;
            thread::sleep(SETTLE);
            if let Err(error) = popup_whole_in(user, &format!("23-narrower-popup-{width}-{theme}"), Some(width)) {
                note(&format!("FINDING {error}"));
                cut.push(error.to_string());
            }
        }
        user.ask("theme light")?;
    }
    user.keys(&["escape"])?;
    user.until("closed", |state| state["rename"].is_null())?;
    ensure(cut.is_empty(), || cut.join("; "))
}
