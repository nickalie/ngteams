//! QA of task 697 (rounds 1 and 2), in the UI sandbox: files dragged from a file manager into the app's window, as a user
//! drops them (`harness = false`: GPUI runs on the process's main thread).
//!
//! The real app runs as a child in the sandbox, as lab account a on `teams-mock`. The parent is the user: the
//! sandbox's drag source ([`ui_sandbox::drag`]) offers the files as each file manager does, and the sandbox's
//! pointer drags them from it onto the window and lets go. The parent asks the child what the boxes hold through a
//! file in the sandbox dir; the app tells no positions except where its boxes are.
//!
//! - **Each file manager** (Dolphin's URI lists, Nautilus's portal key before its list, Thunar's GTK 3 types, a
//!   Flatpak program's portal key alone) onto the 1:1 chat's message box: both files in the box beside the draft.
//! - **Typed, then dropped**: keys typed, then a drop with no pointer motion over the app between them: the files
//!   beside the draft, the highlight gone, the next key in the box. The same on a box in edit: its message. After a
//!   drop, Tab moves the keyboard's ring from the box to the next stops.
//! - **Each chat**: over the messages of a group chat; the account's own notes, whose box takes files too.
//! - **A fast drop** (no pause over the chat) and **a late source** (the drop comes before the files are read).
//! - **The highlight**: "Share in this chat" while the files are over the chat, gone when they leave, nothing
//!   attached. **Several files** at once. **The chat under the pointer** gets the files, no other chat, and a drop on
//!   the chat list attaches nothing.
//! - **A folder** and **a file over the limit**: their message, the other file still in. **A text drag** and a web
//!   link are refused.
//! - **A channel**: a drop on the reply box and on the post box uploads into the channel's folder on the mock's team
//!   site, the highlight over that box alone; the same name again goes as a numbered copy, and the post names the
//!   file. **An edit**: its message.
//! - **Looks**: Teams Dark, a font of 20, a narrow window.
//! - **QA of task 706**: the `qa706_*` checks ([`qa706`]): Attach, a drop, a paste, a reply, a copy, Remove and a
//!   failed upload in a channel's boxes.
//!
//! The mock serves no OneDrive upload, so each file staged in a chat shows its upload error; the files' names are what
//! is checked there. `QA697_ONLY` (a comma list of check names) runs only those. `QA697_EVIDENCE` names a directory that gets
//! the screenshots and the results. Screenshots also go to `target/ui-shots/qa697-*.png`. Every name, id, file and
//! text is made up. Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use std::{fs, future, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_mock::{
    AB_CHAT, ABC_CHAT, AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE,
    TEAM_CHANNEL,
};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::compose::attachments::AttachmentState;
use teams_ui::compose::{Compose, Mode};
use teams_ui::shell::{Main, Shell};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::drag::{DndAction, DragEvent, DragOptions, DragSource};
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "../window.rs"]
mod support;
use support::{Alias, display_name};

#[expect(
    dead_code,
    reason = "the account picture QA's portal module also holds that QA's own pictures"
)]
#[path = "qa_account_picture_portal.rs"]
mod portal;
use portal::Chooser;

#[path = "qa_file_drop_qa706.rs"]
mod qa706;

#[path = "qa_file_drop_qa792.rs"]
mod qa792;

const CHILD_ENV: &str = "QA697_CHILD";
const ONLY_ENV: &str = "QA697_ONLY";
const EVIDENCE_ENV: &str = "QA697_EVIDENCE";
const ORIGIN_ENV: &str = "QA697_ORIGIN";
const TROUTER_ENV: &str = "QA697_TROUTER";
const TOKEN_ENV: &str = "QA697_TOKEN";
const DIR_ENV: &str = "QA697_DIR";
/// The file in the sandbox dir the parent asks the child about its views through.
const ASK: &str = "qa697-ask";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(60);
/// How long the screen gets to settle before a shot.
const SETTLE: Duration = Duration::from_millis(700);
const STEP: Duration = Duration::from_millis(250);
/// How long a drag that must not be taken gets to be taken anyway.
const QUIET: Duration = Duration::from_millis(1500);
const SCREEN: (u32, u32) = (1280, 800);
/// niri's layout: each window takes the whole screen, and no hot corner.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
/// The channel's post the reply box answers (the mock's).
const CHANNEL_POST: &str = "1760000300000";
/// The account's own notes (`teams_ui::chat_list::NOTES`).
const NOTES: &str = "48:notes";
/// The drag source's surface, at the screen's bottom left over the chat list, and where the pointer presses it.
const SOURCE_AT: (u32, u32) = (20, 600);
const SOURCE_SIZE: (u32, u32) = (200, 160);
const PRESS: (u32, u32) = (120, 680);
/// A place over the messages of the account's own notes.
const NOTES_MIDDLE: (u32, u32) = (860, 450);
/// A place on the chat list, away from the chat.
const ON_THE_LIST: (u32, u32) = (200, 250);
/// The portal key the sandboxed sources hand over; the fake portal names the files for it.
const TRANSFER_KEY: &str = "made-up-transfer-key";
const FOLDER_PROBLEM: &str = "Folders can't be attached. Choose the files inside the folder.";
const LIMIT_PROBLEM: &str = "Choose a file smaller than 250 MB.";
const EDIT_PROBLEM: &str = "Files can't be added to a message you edit. Send them in a new message.";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_file_drop_window: {error}");
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

fn wanted(name: &str) -> bool {
    std::env::var(ONLY_ENV)
        .ok()
        .is_none_or(|list| list.split(',').map(str::trim).any(|one| one == name))
}

// --- the parent ---

type Check = (&'static str, fn(&mut User<'_>) -> SandboxResult);

const CHECKS: [Check; 18] = [
    ("each_file_manager", each_file_manager),
    ("typed_then_dropped", typed_then_dropped),
    ("edit_after_keys", edit_after_keys),
    ("keys_after_drop", keys_after_drop),
    ("over_group_messages", over_group_messages),
    ("own_notes", own_notes),
    ("fast_drop", fast_drop),
    ("late_source", late_source),
    ("highlight_leaves", highlight_leaves),
    ("several_files", several_files),
    ("chat_under_pointer", chat_under_pointer),
    ("folder", folder),
    ("over_the_limit", over_the_limit),
    ("text_refused", text_refused),
    ("channel_boxes", channel_boxes),
    ("edit_box", edit_box),
    ("dark_and_font", dark_and_font),
    ("narrow_window", narrow_window),
];

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_file_drop_window", &options)? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mock = runtime.block_on(ChatServiceMock::start())?;
    let trouter = mock.start_trouter()?;
    for (chat, who) in [(AB_CHAT, (LAB_B, "Lab B")), (ABC_CHAT, (LAB_C, "Lab C"))] {
        mock.add_message(chat, who, "RichText/Html", "<p>Made-up hello</p>")
            .ok_or("a hello refused")?;
    }
    let dir = tempfile::tempdir()?;
    let chooser = Chooser::start(sandbox.bus().address())?;
    let running = launch(&sandbox, &mock, trouter.origin(), dir.path())?;
    let mut user = User::new(&sandbox, &mock, &runtime, &chooser, running)?;
    trouter.origin().clone_into(&mut user.trouter);
    dir.path().clone_into(&mut user.dir);
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    for (name, check) in CHECKS.into_iter().chain(qa706::CHECKS).chain(qa792::CHECKS) {
        if wanted(name) {
            user.run_check(name, check, &mut passed, &mut failed);
        }
    }
    let quit = user.quit();
    drop(trouter);
    report(&passed, &failed);
    quit?;
    ensure(failed.is_empty(), || format!("{} checks failed", failed.len()))
}

fn report(passed: &[String], failed: &[(String, String)]) {
    let mut summary = String::new();
    for name in passed {
        let _ = writeln!(summary, "pass {name}");
    }
    for (name, why) in failed {
        let _ = writeln!(summary, "FAIL {name}: {why}");
    }
    say(&summary);
    if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(PathBuf::from(dir).join("results.txt"), &summary);
    }
    say(&format!(
        "qa_file_drop_window: {} passed, {} failed",
        passed.len(),
        failed.len()
    ));
}

/// Starts a's app on the mock, its store and settings in `dir`; once its window is drawn.
fn launch(sandbox: &Sandbox, mock: &ChatServiceMock, trouter: &str, dir: &Path) -> SandboxResult<Running> {
    launch_as(sandbox, mock, trouter, dir, "a")
}

/// Starts the app of lab account `who` (`a` or `b`) on the mock, its store and settings in `dir`; once its window is
/// drawn.
fn launch_as(
    sandbox: &Sandbox,
    mock: &ChatServiceMock,
    trouter: &str,
    dir: &Path,
    who: &str,
) -> SandboxResult<Running> {
    let token = mock
        .token(if who == "b" { LAB_B } else { LAB_A })
        .ok_or("the mock's token")?;
    let dir_text = dir.to_string_lossy().into_owned();
    let child = sandbox.spawn(
        &std::env::current_exe()?,
        &[],
        &[
            (CHILD_ENV, who),
            (ORIGIN_ENV, &mock.origin()),
            (TROUTER_ENV, trouter),
            (TOKEN_ENV, &token),
            (DIR_ENV, &dir_text),
        ],
    )?;
    let mut running = Running::new(child)?;
    if let Err(error) = running.line_where(STARTUP, |line| line == "ready") {
        return Err(running.fail(&format!("no ready line: {error:?}{}", sandbox.logs())));
    }
    Ok(running)
}

/// How a file manager offers its files.
#[derive(Clone, Copy)]
struct Shape {
    name: &'static str,
    /// What it offers, from the files' URI list and their plain paths.
    offers: fn(&str, &str) -> ui_sandbox::clipboard::Offers,
    actions: DndAction,
    /// The type the app reads the files from; `None`: it takes nothing from this drag.
    takes: Option<&'static str>,
}

fn offer(mime: &str, text: &str) -> (String, Vec<u8>) {
    (mime.to_owned(), text.as_bytes().to_vec())
}

const DOLPHIN: Shape = Shape {
    name: "dolphin",
    offers: |uris, plain| {
        vec![
            offer("text/uri-list", uris),
            offer("application/x-kde4-urilist", uris),
            offer("text/plain;charset=utf-8", plain),
        ]
    },
    actions: DndAction::from_bits_truncate(DndAction::Copy.bits() | DndAction::Move.bits()),
    takes: Some("text/uri-list"),
};

const NAUTILUS: Shape = Shape {
    name: "nautilus",
    offers: |uris, _| {
        vec![
            offer("application/vnd.portal.filetransfer", TRANSFER_KEY),
            offer("application/vnd.portal.files", TRANSFER_KEY),
            offer("text/uri-list", uris),
            offer("text/plain;charset=utf-8", uris),
        ]
    },
    actions: DndAction::from_bits_truncate(DndAction::Copy.bits() | DndAction::Move.bits() | DndAction::Ask.bits()),
    takes: Some("text/uri-list"),
};

const THUNAR: Shape = Shape {
    name: "thunar",
    offers: |uris, plain| {
        vec![
            offer("text/uri-list", uris),
            offer("x-special/gnome-copied-files", &format!("copy\n{uris}")),
            offer("UTF8_STRING", plain),
            offer("text/plain;charset=utf-8", plain),
        ]
    },
    actions: DndAction::from_bits_truncate(DndAction::Copy.bits() | DndAction::Move.bits() | DndAction::Ask.bits()),
    takes: Some("text/uri-list"),
};

const FLATPAK: Shape = Shape {
    name: "flatpak",
    offers: |_, _| vec![offer("application/vnd.portal.filetransfer", TRANSFER_KEY)],
    actions: DndAction::from_bits_truncate(DndAction::Copy.bits() | DndAction::Move.bits()),
    takes: Some("application/vnd.portal.filetransfer"),
};

const TEXT: Shape = Shape {
    name: "text",
    offers: |_, _| {
        vec![
            offer("text/plain;charset=utf-8", "Some made-up words"),
            offer("text/html", "<p>Some made-up words</p>"),
        ]
    },
    actions: DndAction::from_bits_truncate(DndAction::Copy.bits() | DndAction::Move.bits()),
    takes: None,
};

const LINK: Shape = Shape {
    name: "link",
    offers: |_, _| {
        vec![
            offer("text/uri-list", "https://example.invalid/made-up-page\r\n"),
            offer("text/plain;charset=utf-8", "https://example.invalid/made-up-page"),
        ]
    },
    actions: DndAction::from_bits_truncate(DndAction::Copy.bits() | DndAction::Move.bits()),
    takes: None,
};

/// How the hand moves: a pause over the target before it lets go, and how late the source answers a read.
#[derive(Clone, Copy)]
struct Hand {
    pause: bool,
    delay: Duration,
}

const CALM: Hand = Hand {
    pause: true,
    delay: Duration::ZERO,
};
const FAST: Hand = Hand {
    pause: false,
    delay: Duration::ZERO,
};
const LATE: Hand = Hand {
    pause: false,
    delay: Duration::from_millis(1500),
};

/// The files' URI list (RFC 2483) and their plain paths, a line each.
fn file_lists(paths: &[PathBuf]) -> (String, String) {
    paths
        .iter()
        .fold((String::new(), String::new()), |(mut uris, mut plain), path| {
            let shown = path.display().to_string();
            let _ = write!(uris, "file://{}\r\n", shown.replace(' ', "%20"));
            let _ = writeln!(plain, "{shown}");
            (uris, plain)
        })
}

/// A drag the user holds over the window until it lets go.
struct Held {
    source: DragSource,
}

/// The user at the sandbox: the pointer, the keyboard, the screenshots, and the child it asks about the window.
struct User<'a> {
    sandbox: &'a Sandbox,
    mock: &'a ChatServiceMock,
    runtime: &'a tokio::runtime::Runtime,
    /// The fake file chooser Attach opens.
    chooser: &'a Chooser,
    running: Option<Running>,
    input: VirtualInput,
    asked: u32,
    marks: u32,
    files: u32,
    /// Where the window's left edge is on the screen (niri centres a narrow column).
    left: u32,
    /// The mock Trouter's origin and the app's dir, to start the app again as another account.
    trouter: String,
    dir: PathBuf,
}

impl<'a> User<'a> {
    fn new(
        sandbox: &'a Sandbox,
        mock: &'a ChatServiceMock,
        runtime: &'a tokio::runtime::Runtime,
        chooser: &'a Chooser,
        running: Running,
    ) -> SandboxResult<Self> {
        let mut user = Self {
            sandbox,
            mock,
            runtime,
            chooser,
            running: Some(running),
            input: sandbox.input()?,
            asked: 0,
            marks: 0,
            files: 0,
            left: 0,
            trouter: String::new(),
            dir: PathBuf::new(),
        };
        user.park()?;
        thread::sleep(SETTLE);
        Ok(user)
    }

    fn run_check(
        &mut self,
        name: &'static str,
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
                let error = error.to_string();
                let state = self.state().map(|state| state.to_string()).unwrap_or_default();
                let _ = self.shot(&format!("failed-{name}"));
                say(&format!("  FAIL {name}: {error}\n  state: {state}"));
                failed.push((name.to_owned(), error));
            }
        }
        // a drag left pressed would hold the next check's pointer
        let _ = self.input.button_up(keys::BTN_LEFT);
    }

    fn running(&self) -> SandboxResult<&Running> {
        self.running.as_ref().ok_or_else(|| "the app has quit".into())
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
                return Err(format!("{what}: never came; last {state}").into());
            }
            thread::sleep(POLL);
        }
    }

    fn quit(&mut self) -> SandboxResult {
        self.ask("quit")?;
        let running = self.running.take().ok_or("the app has quit")?;
        let (status, stderr) = running.finish(WAIT)?;
        ensure(status.success(), || format!("the app exited {status}: {stderr}"))
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

    fn click(&mut self, (x, y): (u32, u32)) -> SandboxResult {
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn park(&mut self) -> SandboxResult {
        // the window's left margin: no button, tooltip or tile there
        self.input.move_to(3, SCREEN.1 / 2)
    }

    /// A unique made-up word for a draft or a message.
    fn mark(&mut self) -> String {
        self.marks += 1;
        format!("zq{}x", 100 + self.marks)
    }

    /// The bodies of the posts the mock got that hold `text`.
    fn posted_with(&self, text: &str) -> Vec<String> {
        self.runtime
            .block_on(self.mock.received_requests())
            .iter()
            .filter(|request| request.method.as_str() == "POST")
            .map(|request| String::from_utf8_lossy(&request.body).into_owned())
            .filter(|body| body.contains(text))
            .collect()
    }

    /// A screenshot as `qa697-<name>.png`, under `target/ui-shots/` and the evidence dir.
    fn shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        let file = format!("qa697-{name}.png");
        let png = self.sandbox.screenshot(&file, false)?;
        let kept = shots_dir()?.join(&file);
        fs::copy(&png, &kept)?;
        if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
            let dir = PathBuf::from(dir).join("shots");
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(&file))?;
        }
        say(&format!("  shot {}", kept.display()));
        Ok(kept)
    }

    /// The compositor sizes the window to `width` wide, as a user's drag of its edge does.
    fn resize(&mut self, width: u32) -> SandboxResult {
        self.sandbox
            .niri_msg(&["action", "set-column-width", &width.to_string()])?;
        self.until(&format!("a window {width} px wide"), |state| {
            state["width"]
                .as_f64()
                .is_some_and(|now| (now - f64::from(width)).abs() < 1.0)
        })?;
        thread::sleep(SETTLE);
        self.left = self.window_left()?;
        say(&format!("  the window is {width} px wide from x {}", self.left));
        Ok(())
    }

    /// The left edge of the focused window on the screen, as niri lays it out.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "niri's logical px are small and positive on a scale of 1"
    )]
    fn window_left(&self) -> SandboxResult<u32> {
        let windows: Value = serde_json::from_str(&self.sandbox.niri_msg(&["-j", "windows"])?)?;
        let focused = windows
            .as_array()
            .and_then(|all| all.iter().find(|window| window["is_focused"] == true))
            .ok_or("no window has the focus")?;
        let layout = &focused["layout"];
        let tile = layout["tile_pos_in_workspace_view"][0].as_f64().unwrap_or_default();
        let inside = layout["window_offset_in_tile"][0].as_f64().unwrap_or_default();
        Ok((tile + inside).round().max(0.0) as u32)
    }

    /// A point of the window (logical px) on the screen.
    const fn on_screen(&self, (x, y): (u32, u32)) -> (u32, u32) {
        (x + self.left, y)
    }
}

// --- the files and the drags ---

impl User<'_> {
    /// Made-up files of `names` in a fresh folder of the sandbox, a few bytes each.
    fn made_up(&mut self, names: &[&str]) -> SandboxResult<Vec<PathBuf>> {
        self.files += 1;
        let dir = self.sandbox.path(&format!("files-{}", self.files));
        fs::create_dir_all(&dir)?;
        names
            .iter()
            .map(|name| {
                let path = dir.join(name);
                fs::write(&path, b"A synthetic attachment, not a real document.\n")?;
                Ok(path)
            })
            .collect()
    }

    /// Shows `shape`'s drag of `paths` at the screen's bottom left, presses it and moves the files over `to`; the
    /// button stays down. With a calm hand, it rests there until the files are read.
    fn hover(&mut self, shape: Shape, paths: &[PathBuf], to: (u32, u32), hand: Hand) -> SandboxResult<Held> {
        let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
        self.sandbox.bus().start_file_transfer(TRANSFER_KEY, &refs)?;
        let (uris, plain) = file_lists(paths);
        let source = self.sandbox.drag_source(DragOptions {
            offers: (shape.offers)(&uris, &plain),
            actions: shape.actions,
            at: SOURCE_AT,
            size: SOURCE_SIZE,
            send_delay: hand.delay,
        })?;
        self.input.move_to(PRESS.0, PRESS.1)?;
        thread::sleep(Duration::from_millis(200));
        self.input.button_down(keys::BTN_LEFT)?;
        self.glide(PRESS, to)?;
        if hand.pause {
            thread::sleep(Duration::from_millis(300));
            self.input.move_to(to.0 + 2, to.1)?;
            // the file list is read and the highlight drawn
            thread::sleep(Duration::from_millis(700));
        }
        Ok(Held { source })
    }

    /// Moves the held pointer from `from` to `to` in small steps, as a hand does.
    fn glide(&mut self, from: (u32, u32), to: (u32, u32)) -> SandboxResult {
        const STEPS: i64 = 30;
        for step in 1..=STEPS {
            let along = |a: u32, b: u32| {
                let at = i64::from(a) + (i64::from(b) - i64::from(a)) * step / STEPS;
                u32::try_from(at).unwrap_or_default()
            };
            self.input.move_to(along(from.0, to.0), along(from.1, to.1))?;
            thread::sleep(Duration::from_millis(12));
        }
        Ok(())
    }

    /// Lets go and parks the pointer; what the source was told once the drag ended.
    fn release(&mut self, held: Held) -> SandboxResult<Vec<DragEvent>> {
        let seen = self.let_go(held)?;
        self.park()?;
        Ok(seen)
    }

    /// Lets go; what the source was told once the drag ended (finished or cancelled).
    fn let_go(&mut self, held: Held) -> SandboxResult<Vec<DragEvent>> {
        self.input.button_up(keys::BTN_LEFT)?;
        let ended = wait_until(WAIT, || {
            let seen = held.source.events();
            (seen.contains(&DragEvent::Finished) || seen.contains(&DragEvent::Cancelled)).then_some(seen)
        });
        let seen = ended.ok_or_else(|| format!("the drag never ended: {:?}", held.source.events()))?;
        drop(held);
        Ok(seen)
    }

    /// As [`Self::drop_files`], and the pointer stays where the files were let go.
    fn drop_files_resting(
        &mut self,
        shape: Shape,
        paths: &[PathBuf],
        to: (u32, u32),
        hand: Hand,
    ) -> SandboxResult<Vec<DragEvent>> {
        let held = self.hover(shape, paths, to, hand)?;
        self.let_go(held)
    }

    /// Drags `shape`'s `paths` onto `to` and lets go; what the source was told.
    fn drop_files(
        &mut self,
        shape: Shape,
        paths: &[PathBuf],
        to: (u32, u32),
        hand: Hand,
    ) -> SandboxResult<Vec<DragEvent>> {
        let held = self.hover(shape, paths, to, hand)?;
        self.release(held)
    }
}

/// The source was told the drop was taken, as a copy, after the app read `mime`.
fn taken(seen: &[DragEvent], mime: &str) -> SandboxResult {
    ensure(
        seen.contains(&DragEvent::Sent(mime.to_owned()))
            && seen.contains(&DragEvent::DropPerformed)
            && seen.contains(&DragEvent::Finished)
            && !seen.contains(&DragEvent::Cancelled),
        || format!("the source wasn't told the drop was taken from {mime}: {seen:?}"),
    )?;
    let action = |event: &DragEvent| match event {
        DragEvent::Action(bits) => Some(*bits),
        _ => None,
    };
    let last_before_drop = seen
        .iter()
        .take_while(|event| **event != DragEvent::DropPerformed)
        .filter_map(action)
        .last();
    ensure(last_before_drop == Some(DndAction::Copy.bits()), || {
        format!("the drop wasn't a copy: {seen:?}")
    })
}

/// The source was told the drag was refused: no drop performed, the drag cancelled.
fn refused(seen: &[DragEvent]) -> SandboxResult {
    ensure(
        seen.contains(&DragEvent::Cancelled) && !seen.contains(&DragEvent::DropPerformed),
        || format!("the drag wasn't refused: {seen:?}"),
    )
}

// --- the boxes as the child says them ---

fn names_of(paths: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = paths
        .iter()
        .filter_map(|path| path.file_name()?.to_str().map(str::to_owned))
        .collect();
    names.sort_unstable();
    names
}

fn staged(boxed: &Value) -> Vec<String> {
    let mut names: Vec<String> = boxed["files"]
        .as_array()
        .map(|files| {
            files
                .iter()
                .filter_map(|file| file["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    names.sort_unstable();
    names
}

fn text_of(boxed: &Value) -> String {
    boxed["text"].as_str().unwrap_or_default().trim().to_owned()
}

fn problem_of(boxed: &Value) -> String {
    boxed["problem"].as_str().unwrap_or_default().to_owned()
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a window's coordinates are small and positive"
)]
fn px(value: &Value) -> u32 {
    value.as_f64().unwrap_or_default().round().max(0.0) as u32
}

/// The centre of a box, and a place over the messages above it.
fn box_centre(boxed: &Value) -> SandboxResult<(u32, u32)> {
    let bounds = &boxed["bounds"];
    ensure(bounds.is_array(), || format!("the box has no bounds: {boxed}"))?;
    Ok((px(&bounds[0]) + px(&bounds[2]) / 2, px(&bounds[1]) + px(&bounds[3]) / 2))
}

/// The box's text line: its top, where the text is when the box stacks its tools under it in a narrow window.
fn text_line(boxed: &Value) -> SandboxResult<(u32, u32)> {
    let (x, _) = box_centre(boxed)?;
    Ok((x, px(&boxed["bounds"][1]) + px(&boxed["bounds"][3]).min(40) / 2))
}

fn over_messages(boxed: &Value) -> SandboxResult<(u32, u32)> {
    let (x, _) = box_centre(boxed)?;
    Ok((x, px(&boxed["bounds"][1]).saturating_sub(160)))
}

impl User<'_> {
    /// Chat `id` shown, its box empty, Teams Light, font 14, the window as wide as the screen.
    fn fresh_chat(&mut self, id: &str) -> SandboxResult<Value> {
        self.ask("theme-light")?;
        self.ask("font 14")?;
        self.ask(&format!("open {id}"))?;
        self.ask("clear")?;
        self.until(&format!("chat {id}"), |state| {
            state["main"] == format!("chat:{id}") && state["chat"]["bounds"].is_array()
        })
    }

    /// Types `text` into the shown chat's box, as a user clicks it and types.
    fn draft(&mut self, text: &str) -> SandboxResult<Value> {
        let state = self.state()?;
        let at = self.on_screen(text_line(&state["chat"])?);
        self.click(at)?;
        self.type_text(text)?;
        // the hand takes the mouse again: the pointer moves over the box on its way to the file manager
        self.input.move_to(at.0 + 6, at.1)?;
        thread::sleep(STEP);
        self.until("the draft", |state| text_of(&state["chat"]).contains(text))
    }

    /// The shown chat's box once it holds `names` (sorted).
    fn staged_now(&mut self, names: &[String]) -> SandboxResult<Value> {
        self.until(&format!("{names:?} in the box"), |state| {
            staged(&state["chat"]) == names
        })
    }
}

// --- the checks ---

/// Each file manager's offer onto the 1:1 chat's box: both files beside the draft, the drop a copy.
fn each_file_manager(user: &mut User<'_>) -> SandboxResult {
    for shape in [DOLPHIN, NAUTILUS, THUNAR, FLATPAK] {
        user.fresh_chat(AB_CHAT)?;
        let mark = user.mark();
        let state = user.draft(&format!("Keep this draft {mark}"))?;
        let paths = user.made_up(&["made-up notes.txt", "made-up-report.pdf"])?;
        let held = user.hover(shape, &paths, box_centre(&state["chat"])?, CALM)?;
        let hovering = user.until(&format!("{}: the highlight", shape.name), |state| {
            state["chat"]["drop_active"] == true
        });
        if shape.name == DOLPHIN.name {
            user.shot("box-hover-light")?;
        }
        let seen = user.release(held)?;
        hovering?;
        let state = user
            .staged_now(&names_of(&paths))
            .map_err(|error| format!("{}: {error}; source saw {seen:?}", shape.name))?;
        taken(&seen, shape.takes.unwrap_or_default()).map_err(|error| format!("{}: {error}", shape.name))?;
        ensure(text_of(&state["chat"]).contains(&mark), || {
            format!("{}: the draft went: {state}", shape.name)
        })?;
        ensure(state["chat"]["drop_active"] == false, || {
            format!("{}: the highlight stayed after the drop", shape.name)
        })?;
        if shape
            .takes
            .is_some_and(|mime| mime.starts_with("application/vnd.portal"))
        {
            let asked = user.sandbox.bus().retrieved_transfers();
            ensure(asked.iter().any(|key| key == TRANSFER_KEY), || {
                format!("{}: the portal was never asked for the files", shape.name)
            })?;
        }
        if shape.name == DOLPHIN.name {
            thread::sleep(SETTLE);
            user.shot("box-dropped-light")?;
        }
    }
    Ok(())
}

/// The user types the draft while the pointer rests away from the box (on the chat list), then drags files onto the
/// box: the files are in beside the draft, and the highlight goes. No pointer motion reaches the app between the last
/// key and the drop. The next key types into the box and gives the window back to the keyboard.
fn typed_then_dropped(user: &mut User<'_>) -> SandboxResult {
    let state = user.fresh_chat(AB_CHAT)?;
    let at = box_centre(&state["chat"])?;
    user.input.move_to(PRESS.0, PRESS.1)?;
    thread::sleep(STEP);
    let mark = user.mark();
    user.type_text(&format!("Typed then dropped {mark}"))?;
    user.until("the typed draft", |state| {
        text_of(&state["chat"]).contains(&mark) && state["keyboard"] == true
    })?;
    let paths = user.made_up(&["made-up after typing.txt", "made-up-after-typing.pdf"])?;
    let seen = user.drop_files_resting(DOLPHIN, &paths, at, CALM)?;
    thread::sleep(QUIET);
    let state = user.state()?;
    thread::sleep(SETTLE);
    user.shot("typed-then-dropped")?;
    ensure(staged(&state["chat"]) == names_of(&paths), || {
        format!("files dropped after typing are not in the box: {state}; source saw {seen:?}")
    })?;
    taken(&seen, "text/uri-list")?;
    ensure(state["chat"]["drop_active"] == false, || {
        format!("\"Share in this chat\" stayed over the chat after the drop: {state}")
    })?;
    ensure(text_of(&state["chat"]).contains(&mark), || {
        format!("the draft went: {state}")
    })?;
    let next = user.mark();
    user.type_text(&format!(" {next}"))?;
    let state = user.until("the next key in the box", |state| {
        text_of(&state["chat"]).contains(&next)
    })?;
    thread::sleep(SETTLE);
    user.shot("typed-then-dropped-next-key")?;
    ensure(
        text_of(&state["chat"]).contains(&mark) && state["chat"]["focused"] == true && state["keyboard"] == true,
        || format!("the keys after the drop: {state}"),
    )?;
    ensure(staged(&state["chat"]) == names_of(&paths), || {
        format!("the files went after the next key: {state}")
    })
}

/// A message sent, the pointer rests away from the box (on the chat list), then Up to edit the last own message (keys
/// only) and a drop on the box: the edit's message, the edit kept, the highlight gone.
fn edit_after_keys(user: &mut User<'_>) -> SandboxResult {
    user.fresh_chat(AB_CHAT)?;
    let mark = user.mark();
    user.draft(&format!("Edit me by keys {mark}"))?;
    user.press("enter")?;
    wait_until(WAIT, || (!user.posted_with(&mark).is_empty()).then_some(()))
        .ok_or("the mock got no post of the message")?;
    user.until("the box empty", |state| text_of(&state["chat"]).is_empty())?;
    user.input.move_to(PRESS.0, PRESS.1)?;
    thread::sleep(SETTLE);
    user.press("up")?;
    let state = user.until("the edit", |state| {
        state["chat"]["mode"] == "edit" && state["keyboard"] == true
    })?;
    let editing = text_of(&state["chat"]);
    let at = box_centre(&state["chat"])?;
    let paths = user.made_up(&["made-up for the keyed edit.txt"])?;
    let seen = user.drop_files_resting(DOLPHIN, &paths, at, CALM)?;
    let state = user
        .until("the edit's message", |state| problem_of(&state["chat"]) == EDIT_PROBLEM)
        .map_err(|error| format!("{error}; source saw {seen:?}"))?;
    ensure(
        state["chat"]["mode"] == "edit" && text_of(&state["chat"]) == editing,
        || format!("the edit went: {state}; source saw {seen:?}"),
    )?;
    ensure(staged(&state["chat"]).is_empty(), || {
        format!("the edit took files: {state}")
    })?;
    ensure(state["chat"]["drop_active"] == false, || {
        format!("\"Share in this chat\" stayed over the edit after the drop: {state}")
    })?;
    thread::sleep(SETTLE);
    user.shot("edit-after-keys")?;
    user.press("escape")?;
    Ok(())
}

/// After a drop, the keyboard still moves through the window: Tab takes the focus from the box to the staged file's
/// stop and draws its ring, and the next Tab moves the ring on.
fn keys_after_drop(user: &mut User<'_>) -> SandboxResult {
    user.fresh_chat(AB_CHAT)?;
    let mark = user.mark();
    let state = user.draft(&format!("Keys after the drop {mark}"))?;
    let paths = user.made_up(&["made-up before tab.txt"])?;
    let seen = user.drop_files_resting(DOLPHIN, &paths, box_centre(&state["chat"])?, CALM)?;
    let state = user
        .staged_now(&names_of(&paths))
        .map_err(|error| format!("{error}; source saw {seen:?}"))?;
    ensure(state["chat"]["focused"] == true, || {
        format!("the box lost the focus to the drop: {state}")
    })?;
    // the card's status line changes when the upload ends; that change must not count as a ring
    user.until("the upload settled", |state| {
        state["chat"]["files"]
            .as_array()
            .is_some_and(|files| files.iter().all(|file| file["state"] != "uploading"))
    })?;
    thread::sleep(SETTLE);
    let dropped = user.shot("keys-after-drop")?;
    user.press("tab")?;
    let state = user.until("the focus off the box", |state| {
        state["chat"]["focused"] == false && state["keyboard"] == true
    })?;
    thread::sleep(SETTLE);
    let first = user.shot("keys-after-drop-tab")?;
    ring_drawn(&dropped, &first, &state["chat"])?;
    user.press("tab")?;
    thread::sleep(SETTLE);
    let second = user.shot("keys-after-drop-tab-2")?;
    ring_drawn(&first, &second, &state["chat"])?;
    let state = user.state()?;
    ensure(
        text_of(&state["chat"]).contains(&mark) && staged(&state["chat"]) == names_of(&paths),
        || format!("the draft or the file went: {state}"),
    )
}

/// Tab drew a ring in the box under its text line (the staged file's or the tools' stop): the caret and the box's
/// focus line at its bottom edge are not counted.
fn ring_drawn(before: &Path, after: &Path, chat: &Value) -> SandboxResult {
    let load = |path: &Path| image::open(path).map(|image| image.to_rgb8());
    let (before, after) = (load(before)?, load(after)?);
    let bounds = &chat["bounds"];
    let (left, top) = (px(&bounds[0]), px(&bounds[1]));
    let (right, bottom) = (left + px(&bounds[2]), top + px(&bounds[3]));
    let mut changed = 0_u32;
    for y in top + 32..bottom.saturating_sub(10).min(before.height()) {
        for x in left + 4..right.saturating_sub(4).min(before.width()) {
            if before.get_pixel(x, y) != after.get_pixel(x, y) {
                changed += 1;
            }
        }
    }
    say(&format!("  focus: {changed} pixels changed in the box after Tab"));
    ensure(changed > 40, || {
        format!("Tab drew no ring in the box ({changed} pixels)")
    })
}

/// A drop over a group chat's messages, not its box: the files in its box.
fn over_group_messages(user: &mut User<'_>) -> SandboxResult {
    let state = user.fresh_chat(ABC_CHAT)?;
    let paths = user.made_up(&["made-up agenda.txt", "made-up-minutes.pdf"])?;
    let held = user.hover(DOLPHIN, &paths, over_messages(&state["chat"])?, CALM)?;
    let hovering = user.until("the highlight over the messages", |state| {
        state["chat"]["drop_active"] == true
    });
    user.shot("group-messages-hover")?;
    let seen = user.release(held)?;
    hovering?;
    user.staged_now(&names_of(&paths))
        .map_err(|error| format!("{error}; source saw {seen:?}"))?;
    taken(&seen, "text/uri-list")?;
    thread::sleep(SETTLE);
    user.shot("group-messages-dropped")?;
    Ok(())
}

/// The account's own notes: their box takes files as any chat's box does. The drop over them is answered as a copy,
/// and the file is in the box.
fn own_notes(user: &mut User<'_>) -> SandboxResult {
    user.ask("theme-light")?;
    user.ask(&format!("open {NOTES}"))?;
    user.ask("clear")?;
    user.until("the notes", |state| {
        state["main"] == format!("chat:{NOTES}") && state["chat"]["bounds"].is_array()
    })?;
    thread::sleep(SETTLE);
    let paths = user.made_up(&["made-up note to self.txt"])?;
    let held = user.hover(DOLPHIN, &paths, NOTES_MIDDLE, CALM)?;
    let hovering = user.until("the notes' highlight", |state| state["chat"]["drop_active"] == true);
    user.shot("notes-hover")?;
    let seen = user.release(held)?;
    hovering?;
    let state = user.staged_now(&names_of(&paths))?;
    user.shot("notes-dropped")?;
    say(&format!(
        "  notes: staged {:?}, read-only {}, problem {:?}, source {seen:?}",
        staged(&state["chat"]),
        state["chat"]["read_only"],
        problem_of(&state["chat"])
    ));
    taken(&seen, "text/uri-list")?;
    user.ask("clear")?;
    Ok(())
}

/// The files let go the moment they reach the box: both in.
fn fast_drop(user: &mut User<'_>) -> SandboxResult {
    let state = user.fresh_chat(AB_CHAT)?;
    let paths = user.made_up(&["made-up quick.txt", "made-up-quick.pdf"])?;
    let seen = user.drop_files(DOLPHIN, &paths, box_centre(&state["chat"])?, FAST)?;
    user.staged_now(&names_of(&paths))
        .map_err(|error| format!("{error}; source saw {seen:?}"))?;
    taken(&seen, "text/uri-list")
}

/// A source that answers 1.5 s late, the drop before the files were read: both in.
fn late_source(user: &mut User<'_>) -> SandboxResult {
    let state = user.fresh_chat(AB_CHAT)?;
    let paths = user.made_up(&["made-up slow.txt", "made-up-slow.pdf"])?;
    let seen = user.drop_files(DOLPHIN, &paths, box_centre(&state["chat"])?, LATE)?;
    user.staged_now(&names_of(&paths))
        .map_err(|error| format!("{error}; source saw {seen:?}"))?;
    taken(&seen, "text/uri-list")
}

/// "Share in this chat" while files are over the chat; gone when they leave; let go off the chat, nothing attached.
fn highlight_leaves(user: &mut User<'_>) -> SandboxResult {
    let state = user.fresh_chat(AB_CHAT)?;
    let mark = user.mark();
    user.draft(&format!("Draft under the highlight {mark}"))?;
    user.park()?;
    thread::sleep(SETTLE);
    let before = user.shot("highlight-before")?;
    let paths = user.made_up(&["made-up leaving.txt"])?;
    let held = user.hover(DOLPHIN, &paths, over_messages(&state["chat"])?, CALM)?;
    user.until("the highlight", |state| state["chat"]["drop_active"] == true)?;
    thread::sleep(SETTLE);
    let over = user.shot("highlight-over")?;
    // back over the file manager: off the window
    user.glide(over_messages(&state["chat"])?, PRESS)?;
    let left = user.until("the highlight gone", |state| state["chat"]["drop_active"] == false);
    thread::sleep(SETTLE);
    let gone = user.shot("highlight-left")?;
    let seen = user.release(held)?;
    left?;
    thread::sleep(QUIET);
    let state = user.state()?;
    ensure(staged(&state["chat"]).is_empty(), || {
        format!("a drag that left attached files: {state}")
    })?;
    ensure(text_of(&state["chat"]).contains(&mark), || {
        format!("the draft went: {state}")
    })?;
    ensure(!seen.contains(&DragEvent::DropPerformed), || {
        format!("a drop off the window was performed: {seen:?}")
    })?;
    highlight_drawn(&before, &over, &gone, &state["chat"])
}

/// The chat's middle changed under the highlight and came back after it left. The middle is far from any border or
/// focus line: the messages' pane between the header and the box.
fn highlight_drawn(before: &Path, over: &Path, gone: &Path, chat: &Value) -> SandboxResult {
    let load = |path: &Path| image::open(path).map(|image| image.to_rgb8());
    let (before, over, gone) = (load(before)?, load(over)?, load(gone)?);
    let (x, _) = box_centre(chat)?;
    let top = 120;
    let bottom = px(&chat["bounds"][1]).saturating_sub(40);
    let changed = |a: &image::RgbImage, b: &image::RgbImage| {
        let mut count = 0_u32;
        for y in (top..bottom).step_by(2) {
            for dx in (0..400).step_by(2) {
                let px_x = x + dx - 200;
                if a.get_pixel(px_x, y) != b.get_pixel(px_x, y) {
                    count += 1;
                }
            }
        }
        count
    };
    let shown = changed(&before, &over);
    let after = changed(&before, &gone);
    say(&format!(
        "  highlight: {shown} pixels changed over the chat, {after} after it left"
    ));
    ensure(shown > 2_000, || {
        format!("no highlight drawn over the chat ({shown} pixels)")
    })?;
    ensure(after < shown / 10, || {
        format!("the highlight stayed after the drag left ({after} pixels)")
    })
}

/// Five files at once onto the box: all five beside the draft.
fn several_files(user: &mut User<'_>) -> SandboxResult {
    user.fresh_chat(AB_CHAT)?;
    let mark = user.mark();
    let state = user.draft(&format!("Five files {mark}"))?;
    let paths = user.made_up(&[
        "made-up one.txt",
        "made-up two.pdf",
        "made-up three.docx",
        "made-up four.xlsx",
        "made-up five.png",
    ])?;
    let seen = user.drop_files(THUNAR, &paths, box_centre(&state["chat"])?, CALM)?;
    let state = user
        .staged_now(&names_of(&paths))
        .map_err(|error| format!("{error}; source saw {seen:?}"))?;
    taken(&seen, "text/uri-list")?;
    ensure(text_of(&state["chat"]).contains(&mark), || {
        format!("the draft went: {state}")
    })?;
    thread::sleep(SETTLE);
    user.shot("five-files")?;
    Ok(())
}

/// The files go to the chat they were dropped on, and to no other chat; a drop on the chat list attaches nothing.
fn chat_under_pointer(user: &mut User<'_>) -> SandboxResult {
    user.fresh_chat(AB_CHAT)?;
    let state = user.fresh_chat(ABC_CHAT)?;
    let paths = user.made_up(&["made-up for the group.txt"])?;
    user.drop_files(DOLPHIN, &paths, box_centre(&state["chat"])?, CALM)?;
    user.staged_now(&names_of(&paths))?;
    user.ask(&format!("open {AB_CHAT}"))?;
    let state = user.until("the 1:1 chat", |state| state["main"] == format!("chat:{AB_CHAT}"))?;
    ensure(staged(&state["chat"]).is_empty(), || {
        format!("the group's files are in the 1:1 chat's box: {state}")
    })?;
    let more = user.made_up(&["made-up on the list.txt"])?;
    let held = user.hover(DOLPHIN, &more, ON_THE_LIST, CALM)?;
    let hover = user.state()?;
    let seen = user.release(held)?;
    thread::sleep(QUIET);
    let state = user.state()?;
    ensure(hover["chat"]["drop_active"] == false, || {
        format!("the chat was highlighted while the files were over the list: {hover}")
    })?;
    ensure(staged(&state["chat"]).is_empty(), || {
        format!("a drop on the list attached files: {state}")
    })?;
    say(&format!("  drop on the list: source {seen:?}"));
    user.ask(&format!("open {ABC_CHAT}"))?;
    user.staged_now(&names_of(&paths)).map(|_| ())
}

/// A folder dropped with a file: the folder's message, the file in.
fn folder(user: &mut User<'_>) -> SandboxResult {
    let state = user.fresh_chat(AB_CHAT)?;
    let mut paths = user.made_up(&["made-up beside the folder.txt"])?;
    let folder = user.sandbox.path(&format!("made-up folder {}", user.mark()));
    fs::create_dir_all(&folder)?;
    fs::write(folder.join("inside.txt"), b"A synthetic file in a made-up folder.\n")?;
    paths.push(folder);
    let held = user.hover(DOLPHIN, &paths, box_centre(&state["chat"])?, CALM)?;
    let hovering = user.until("the highlight for a folder", |state| {
        state["chat"]["drop_active"] == true
    });
    let seen = user.release(held)?;
    hovering?;
    let state = user.until("the folder's message", |state| {
        problem_of(&state["chat"]) == FOLDER_PROBLEM
    })?;
    ensure(staged(&state["chat"]) == names_of(&paths[..1]), || {
        format!("the file beside the folder isn't in: {state}; source saw {seen:?}")
    })?;
    thread::sleep(SETTLE);
    user.shot("folder")?;
    Ok(())
}

/// A file over 250 MB dropped with a small one: the limit's message, the small file in. The big file is sparse: it
/// takes no disk.
fn over_the_limit(user: &mut User<'_>) -> SandboxResult {
    let state = user.fresh_chat(AB_CHAT)?;
    let mut paths = user.made_up(&["made-up small.txt"])?;
    let big = paths[0].with_file_name("made-up big.bin");
    fs::File::create(&big)?.set_len(251 * 1024 * 1024)?;
    paths.push(big);
    let seen = user.drop_files(DOLPHIN, &paths, box_centre(&state["chat"])?, CALM)?;
    let state = user.until("the limit's message", |state| {
        problem_of(&state["chat"]) == LIMIT_PROBLEM
    })?;
    ensure(staged(&state["chat"]) == names_of(&paths[..1]), || {
        format!("the small file isn't in: {state}; source saw {seen:?}")
    })?;
    thread::sleep(SETTLE);
    user.shot("over-the-limit")?;
    fs::remove_file(&paths[1])?;
    Ok(())
}

/// A drag of text and a web link: no highlight, the drag refused, nothing in the box.
fn text_refused(user: &mut User<'_>) -> SandboxResult {
    for shape in [TEXT, LINK] {
        let state = user.fresh_chat(AB_CHAT)?;
        let held = user.hover(shape, &[], box_centre(&state["chat"])?, CALM)?;
        thread::sleep(QUIET);
        let hover = user.state()?;
        if shape.name == TEXT.name {
            user.shot("text-hover")?;
        }
        let seen = user.release(held)?;
        thread::sleep(QUIET);
        let state = user.state()?;
        ensure(hover["chat"]["drop_active"] == false, || {
            format!("{}: highlighted as files: {hover}", shape.name)
        })?;
        refused(&seen).map_err(|error| format!("{}: {error}", shape.name))?;
        ensure(staged(&state["chat"]).is_empty(), || {
            format!("{}: files staged: {state}", shape.name)
        })?;
    }
    Ok(())
}

/// Whether `boxed` holds exactly `names`, each uploaded.
fn ready(boxed: &Value, names: &[&str]) -> bool {
    staged(boxed) == names
        && boxed["files"]
            .as_array()
            .is_some_and(|files| files.iter().all(|file| file["state"] == "ready"))
}

/// A channel's reply box and post box take files into the channel's folder on the team's site, each on the box the
/// files were dropped on, the highlight over that box; the same name again goes as a numbered copy; the post names the
/// file.
fn channel_boxes(user: &mut User<'_>) -> SandboxResult {
    const NAME: &str = "made-up for the channel.txt";
    const COPY: &str = "made-up for the channel 1.txt";
    user.ask("theme-light")?;
    user.ask("open-channel")?;
    user.until("the channel", |state| state["post"]["bounds"].is_array())?;
    user.ask("clear")?;
    user.ask("open-thread")?;
    let state = user.until("the thread", |state| state["thread"]["bounds"].is_array())?;
    let paths = user.made_up(&[NAME])?;
    let held = user.hover(DOLPHIN, &paths, box_centre(&state["thread"])?, CALM)?;
    let hover = user.until("the reply box's highlight", |state| {
        state["thread"]["drop_active"] == true
    })?;
    ensure(hover["post"]["drop_active"] != true, || {
        format!("the post box lit up for the reply box: {hover}")
    })?;
    user.shot("channel-reply-hover")?;
    let seen = user.release(held)?;
    let state = user.until("the reply box's file", |state| ready(&state["thread"], &[NAME]))?;
    say(&format!("  reply box: source {seen:?}"));
    ensure(problem_of(&state["thread"]).is_empty(), || {
        format!("the reply box said: {state}")
    })?;
    ensure(staged(&state["post"]).is_empty(), || {
        format!("the post box took the reply box's file: {state}")
    })?;
    thread::sleep(SETTLE);
    user.shot("channel-reply")?;
    let seen = user.drop_files(DOLPHIN, &paths, box_centre(&state["post"])?, CALM)?;
    let state = user.until("the post box's copy", |state| ready(&state["post"], &[COPY]))?;
    ensure(problem_of(&state["post"]).is_empty(), || {
        format!("the post box said: {state}; source saw {seen:?}")
    })?;
    thread::sleep(SETTLE);
    user.shot("channel-post")?;
    user.ask("theme-dark")?;
    thread::sleep(SETTLE);
    user.shot("channel-post-dark")?;
    user.ask("theme-light")?;
    let files: Vec<String> = user.mock.channel_files().into_iter().map(|file| file.path).collect();
    ensure(
        files
            == [
                format!("/sites/mockteam/Shared Documents/Channel/{NAME}"),
                format!("/sites/mockteam/Shared Documents/Channel/{COPY}"),
            ],
        || format!("the channel's folder holds {files:?}"),
    )?;
    let mark = user.mark();
    let at = user.on_screen(text_line(&state["post"])?);
    user.click(at)?;
    user.type_text(&format!("A made-up post {mark}"))?;
    user.press("enter")?;
    let posted = wait_until(WAIT, || {
        let posts = user.posted_with(&mark);
        (!posts.is_empty()).then_some(posts)
    })
    .ok_or("the mock got no post with the file")?;
    ensure(
        posted[0].contains(r#"\"serviceName\":\"teams\""#) && posted[0].contains(COPY),
        || format!("the post doesn't name the channel file: {}", posted[0]),
    )?;
    user.ask("close-thread")?;
    Ok(())
}

/// A message sent, then Up to edit the last own message: a drop on the box in edit says why, the edit kept.
fn edit_box(user: &mut User<'_>) -> SandboxResult {
    user.fresh_chat(AB_CHAT)?;
    let mark = user.mark();
    user.draft(&format!("Edit me {mark}"))?;
    user.press("enter")?;
    wait_until(WAIT, || (!user.posted_with(&mark).is_empty()).then_some(()))
        .ok_or("the mock got no post of the message")?;
    user.until("the box empty", |state| text_of(&state["chat"]).is_empty())?;
    thread::sleep(SETTLE);
    user.press("up")?;
    let state = user.until("the edit", |state| state["chat"]["mode"] == "edit")?;
    let editing = text_of(&state["chat"]);
    let at = box_centre(&state["chat"])?;
    // the hand takes the mouse again
    user.input.move_to(at.0 + 6, at.1)?;
    thread::sleep(STEP);
    let paths = user.made_up(&["made-up for the edit.txt"])?;
    let seen = user.drop_files(DOLPHIN, &paths, at, CALM)?;
    let state = user.until("the edit's message", |state| problem_of(&state["chat"]) == EDIT_PROBLEM)?;
    ensure(
        state["chat"]["mode"] == "edit" && text_of(&state["chat"]) == editing,
        || format!("the edit went: {state}; source saw {seen:?}"),
    )?;
    ensure(staged(&state["chat"]).is_empty(), || {
        format!("the edit took files: {state}")
    })?;
    thread::sleep(SETTLE);
    user.shot("edit-box")?;
    user.press("escape")?;
    Ok(())
}

/// The highlight and the files in the box in Teams Dark and at a font of 20.
fn dark_and_font(user: &mut User<'_>) -> SandboxResult {
    for (look, theme, font) in [("dark", "theme-dark", 14), ("font-20", "theme-light", 20)] {
        user.fresh_chat(AB_CHAT)?;
        user.ask(theme)?;
        user.ask(&format!("font {font}"))?;
        thread::sleep(SETTLE);
        hover_and_drop(user, look)?;
    }
    user.ask("theme-light")?;
    user.ask("font 14")?;
    Ok(())
}

/// A window 640 px wide: the highlight over the narrow chat, the files in its box.
fn narrow_window(user: &mut User<'_>) -> SandboxResult {
    user.fresh_chat(AB_CHAT)?;
    user.resize(640)?;
    user.shot("narrow-before")?;
    let outcome = hover_and_drop(user, "narrow");
    user.resize(SCREEN.0)?;
    outcome
}

/// A draft, two files over the box (a shot), then dropped (a shot), as the look named `look`.
fn hover_and_drop(user: &mut User<'_>, look: &str) -> SandboxResult {
    let mark = user.mark();
    let state = user.draft(&format!("A draft in {look} {mark}"))?;
    let paths = user.made_up(&["made-up notes.txt", "made-up-report.pdf"])?;
    let held = user.hover(DOLPHIN, &paths, user.on_screen(box_centre(&state["chat"])?), CALM)?;
    let hovering = user.until(&format!("{look}: the highlight"), |state| {
        state["chat"]["drop_active"] == true
    });
    thread::sleep(SETTLE);
    user.shot(&format!("{look}-hover"))?;
    let seen = user.release(held)?;
    hovering?;
    let state = user
        .staged_now(&names_of(&paths))
        .map_err(|error| format!("{look}: {error}; source saw {seen:?}"))?;
    ensure(text_of(&state["chat"]).contains(&mark), || {
        format!("{look}: the draft went: {state}")
    })?;
    thread::sleep(SETTLE);
    user.shot(&format!("{look}-dropped"))?;
    Ok(())
}

// --- the child ---

struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(&self, _audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        self.ic3()
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

fn env(name: &str) -> SandboxResult<String> {
    Ok(std::env::var(name).map_err(|_| format!("{name} isn't set"))?)
}

/// The engine of the child's account (a, or b) on the parent's mock, its store in `dir`.
fn engine_on_the_mock(dir: &Path) -> SandboxResult<EngineHandle> {
    let alias = child_alias();
    let (me, name, other) = match alias {
        Alias::B => (LAB_B, "bot-b@lab.example.test", (LAB_A, "Lab A")),
        _ => (LAB_A, "bot-a@lab.example.test", (LAB_B, "Lab B")),
    };
    let origin = env(ORIGIN_ENV)?;
    let tokens = Arc::new(Tokens {
        token: env(TOKEN_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    // the app's transports name their account, which a file's upload and share check
    let http = || {
        HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin)
            .map(|http| http.for_account(Mri::new(me)))
    };
    let transport = Arc::new(http()?);
    let trouter = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), &env(TROUTER_ENV)?)?
        .with_options(Options {
            backoff_base: Duration::from_millis(50),
            backoff_cap: Duration::from_millis(50),
            backoff_jitter_percent: 0,
            ..Options::default()
        });
    let author = Author::new(Mri::new(me), display_name(name));
    Ok(engine::start(Parts {
        calls: None,
        store: Store::open(&support::store_file(dir, alias))?,
        client: Arc::new(ChatClient::new(
            http()?
                .with_ams(AMS_BASE)?
                .with_csa(CSA_BASE)?
                .with_middle_tier(MIDDLE_TIER_BASE)?,
        )),
        outbox: Outbox::new(transport, author),
        push: Some(Push::new(trouter)),
        options: EngineOptions::default(),
        known_names: vec![
            (other.0.to_owned(), other.1.to_owned()),
            (LAB_C.to_owned(), "Lab C".to_owned()),
        ],
    })?)
}

fn child_alias() -> Alias {
    if std::env::var(CHILD_ENV).as_deref() == Ok("b") {
        Alias::B
    } else {
        Alias::A
    }
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let dir = PathBuf::from(env(DIR_ENV)?);
    let engine = engine_on_the_mock(&dir)?;
    let first = support::started(child_alias(), engine)?;
    let runner = support::runner(&dir)?;
    let settings = teams_ui::SettingsFile::new(dir.join("settings.toml"));
    let mut opened = settings.load().settings;
    opened.selected_chat = Some(AB_CHAT.to_owned());
    settings.save(&opened).map_err(|problem| format!("{problem:?}"))?;
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
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
                answer(window, cx);
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
fn answer(window: WindowHandle<KitRoot>, cx: &mut App) {
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
            if verb == "quit" {
                say(&format!("re {seq} ok"));
                cx.update(app::quit);
                return;
            }
            let reply = cx.update(|cx| on_app(window, verb, cx));
            say(&format!("re {seq} {}", reply.replace('\n', " ")));
        }
    })
    .detach();
}

fn on_app(window: WindowHandle<KitRoot>, verb: &str, cx: &mut App) -> String {
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
}

fn on_shell(shell: &Entity<Shell>, verb: &str, window: &mut Window, cx: &mut App) -> String {
    match verb {
        "state" => state_of(shell, window, cx).to_string(),
        "theme-dark" | "theme-light" => {
            let key = if verb == "theme-dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            change_settings(shell, cx, |settings| {
                settings.appearance.follow_system = false;
                settings.appearance.theme = key;
            });
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
        "close-thread" => {
            if let Some(channel) = shell.read(cx).channel().cloned() {
                channel.update(cx, teams_ui::ChannelView::close_thread);
            }
            "ok".to_owned()
        }
        "clear" => {
            for compose in boxes(shell, cx) {
                compose.update(cx, |compose, cx| {
                    compose.discard(window, cx);
                    // an empty attach takes the last message away
                    compose.attach_files(&[], window, cx);
                });
            }
            "ok".to_owned()
        }
        other => on_shell_with(shell, other, window, cx),
    }
}

/// A verb with an argument: a chat to open, the font.
fn on_shell_with(shell: &Entity<Shell>, other: &str, window: &mut Window, cx: &mut App) -> String {
    if let Some(id) = other.strip_prefix("open ") {
        shell.update(cx, |shell, cx| shell.open_chat(id.to_owned(), window, cx));
        "ok".to_owned()
    } else if let Some(size) = other.strip_prefix("font ") {
        let Ok(size) = size.parse::<u16>() else {
            return format!("bad size {size}");
        };
        change_settings(shell, cx, |settings| settings.appearance.font_size = size);
        "ok".to_owned()
    } else {
        format!("unknown {other}")
    }
}

fn change_settings(shell: &Entity<Shell>, cx: &mut App, change: impl FnOnce(&mut teams_ui::settings::Settings)) {
    let model = shell.read(cx).settings().clone();
    model.update(cx, |model, cx| model.change(change, cx));
}

/// Every box the main window shows now.
fn boxes(shell: &Entity<Shell>, cx: &App) -> Vec<Entity<Compose>> {
    let shell_ref = shell.read(cx);
    let mut all = Vec::new();
    if let Some(chat) = shell_ref.chat() {
        all.push(chat.read(cx).compose().clone());
    }
    if let Some(channel) = shell_ref.channel() {
        let channel = channel.read(cx);
        all.push(channel.posts().read(cx).compose().clone());
        if let Some((_, thread)) = channel.thread() {
            all.push(thread.read(cx).compose().clone());
        }
    }
    all
}

fn state_of(shell: &Entity<Shell>, window: &Window, cx: &App) -> Value {
    let shell_ref = shell.read(cx);
    let main = match shell_ref.main() {
        Main::Chat(id) => format!("chat:{id}"),
        Main::Channel(id) => format!("channel:{id}"),
        other => format!("{other:?}"),
    };
    let view_state = |view: &Entity<teams_ui::chat_view::ChatView>| {
        let view = view.read(cx);
        let mut state = box_state(view.compose(), window, cx);
        state["drop_active"] = json!(view.file_drop_active());
        state
    };
    let chat = shell_ref
        .chat()
        .filter(|_| matches!(shell_ref.main(), Main::Chat(_)))
        .map(view_state);
    let (post, thread) = shell_ref
        .channel()
        .filter(|_| matches!(shell_ref.main(), Main::Channel(_)))
        .map_or((None, None), |channel| {
            let channel = channel.read(cx);
            (
                Some(view_state(channel.posts())),
                channel.thread().map(|(_, thread)| view_state(thread)),
            )
        });
    let cards: Vec<Value> = shell_ref
        .chat()
        .filter(|_| matches!(shell_ref.main(), Main::Chat(_)))
        .map(|view| {
            view.read(cx)
                .message_items()
                .filter(|item| !item.files.is_empty())
                .map(|item| {
                    json!({
                        "own": item.own,
                        "files": item.files.iter().map(|file| json!({
                            "name": file.name,
                            "object_url": file.object_url,
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "main": main,
        "cards": cards,
        "width": f32::from(window.viewport_size().width),
        "keyboard": window.last_input_was_keyboard(),
        "chat": chat,
        "post": post,
        "thread": thread,
    })
}

/// One box: its text, mode, files, message and place.
fn box_state(compose: &Entity<Compose>, window: &Window, cx: &App) -> Value {
    let compose = compose.read(cx);
    let mode = match compose.mode() {
        Mode::New => "new",
        Mode::Reply(_) => "reply",
        Mode::Edit { .. } => "edit",
    };
    let files: Vec<Value> = compose
        .attachments()
        .iter()
        .map(|file| {
            let (state, error) = match &file.state {
                AttachmentState::Uploading => ("uploading", None),
                AttachmentState::Ready(_) => ("ready", None),
                AttachmentState::Failed(why) => ("failed", Some(why.to_string())),
            };
            json!({"name": file.name.clone(), "state": state, "error": error})
        })
        .collect();
    json!({
        "text": compose.value(cx).to_string(),
        "mode": mode,
        "focused": compose.focused(window, cx),
        "read_only": compose.is_read_only(),
        "files": files,
        "problem": compose.problem().map(ToString::to_string),
        "bounds": compose.box_bounds().map(|bounds| {
            [bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height].map(f32::from)
        }),
    })
}
