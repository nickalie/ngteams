//! QA of task 697 (round 1), in the UI sandbox: files dropped from a file manager into the app's window go out in a
//! message, and the other member of the chat gets them (`harness = false`: GPUI runs on the process's main thread).
//!
//! The parent runs the real `ngteams-server` binary (TLS with a made-up certificate authority, SQLite, files in its
//! memory store) with the made-up accounts a and b, so the upload and the share are real. b is a client in the parent
//! (`ngteams::signin::add_own`, `ngteams::engine::start`) and makes its 1:1 chat with a. a's app runs as a child in
//! the sandbox with a control socket. The user drags two made-up files from the sandbox's drag source (Dolphin's
//! offer) onto the chat's box with a draft, waits for the uploads, and presses Enter. Then a's chat shows the message
//! with both files, b's engine has the message with both files' names, and b reads each file's bytes as they were.
//!
//! Every name, address and file is made up; nothing leaves loopback. Screenshots go to
//! `target/ui-shots/qa697-own-*.png`, and also to the directory `QA697_OWN_EVIDENCE` names. Without weston 15, niri,
//! `dbus-daemon` or the server binary (`NGTEAMS_QA_SERVER_BIN`, else `ngteams-server` beside the test's target
//! directory) it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::accounts_file::{self, PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::setup::Setup;
use serde_json::{Value, json};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_core::CallFloor;
use teams_core::account::AccountEngine;
use teams_core::engine::Action;
use teams_ctl::protocol::{OpenTarget, Request};
use teams_ctl::{Client, Timeouts};
use teams_proto::Mri;
use teams_proto::chat::MessageText;
use teams_ui::compose::attachments::AttachmentState;
use teams_ui::shell::Shell;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use ui_sandbox::drag::{DndAction, DragEvent, DragOptions};
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "../own_server_binary.rs"]
#[expect(
    dead_code,
    reason = "the own server's tests share this support; this one starts the server and adds users"
)]
mod own_server_binary;
#[path = "../window.rs"]
#[expect(
    dead_code,
    reason = "the GUI tests share this support; this one uses only its runner"
)]
mod support;
use own_server_binary::{BinaryServer, PASSWORD};

const CHILD_ENV: &str = "QA697_OWN_CHILD";
const ORIGIN_ENV: &str = "QA697_OWN_ORIGIN";
const CA_ENV: &str = "QA697_OWN_CA";
const DIR_ENV: &str = "QA697_OWN_DIR";
/// A directory that gets a copy of each screenshot and the server's log.
const EVIDENCE_ENV: &str = "QA697_OWN_EVIDENCE";
/// The file in a's directory the parent asks the child about its views through.
const ASK: &str = "qa697-own-ask";
/// The file whose presence quits the child, as the tray's Quit does.
const QUIT_FILE: &str = "quit";
const STARTUP: Duration = Duration::from_secs(180);
const WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(100);
const SETTLE: Duration = Duration::from_millis(900);
const SCREEN: (u32, u32) = (1280, 800);
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
/// The drag source's surface over the chat list, and where the pointer presses it.
const SOURCE_AT: (u32, u32) = (20, 600);
const SOURCE_SIZE: (u32, u32) = (200, 160);
const PRESS: (u32, u32) = (120, 680);
/// Where the pointer rests: the title bar, away from every button.
const REST: (u32, u32) = (640, 12);

const A: (&str, &str) = ("ada@own.example.test", "Ada Made-Up");
const B: (&str, &str) = ("bert@own.example.test", "Bert Made-Up");
/// The made-up files, with their bytes.
const FILES: [(&str, &[u8]); 2] = [
    (
        "made-up drop notes.txt",
        b"Made-up notes dropped from a file manager.\n",
    ),
    ("made-up-drop-table.csv", b"made-up,table\n1,2\n3,4\n"),
];
const DRAFT: &str = "Made-up files from the file manager";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_file_drop_own_window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn step(what: &str) {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    eprintln!("[{:>7.2}s] {what}", start.elapsed().as_secs_f64());
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn env(name: &str) -> SandboxResult<String> {
    Ok(std::env::var(name).map_err(|_| format!("{name} is not set"))?)
}

fn ensure(holds: bool, what: impl FnOnce() -> String) -> SandboxResult {
    if holds { Ok(()) } else { Err(what().into()) }
}

/// An own server's account signed in with its password and started as the app starts it, with no calls.
fn own_account(
    dir: &Path,
    (origin, ca_file): (&str, &str),
    upn: &str,
    push: bool,
) -> SandboxResult<(PersonalAccount, AccountEngine<MemorySecretStore>)> {
    let paths = PersonalPaths::new(dir.join("config"), dir.join("data"));
    let setup = Setup::new(paths)?;
    let server = accounts_file::OwnServer::parse(origin, ca_file)?;
    let store = MemorySecretStore::new();
    let runtime = ngteams::accounts::runtime()?;
    let account = runtime.block_on(ngteams::signin::add_own(
        &setup,
        upn,
        &server,
        &Secret::new(PASSWORD),
        &store,
    ))?;
    let engine = ngteams::engine::start(&setup.paths, &account, CallFloor::new(), store, push, None)?;
    Ok((account, engine))
}

/// Where the screenshots are kept: `target/ui-shots/` (never committed), and the evidence directory, if set.
fn evidence_dirs() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    let mut dirs = vec![dir];
    dirs.extend(std::env::var_os(EVIDENCE_ENV).map(PathBuf::from));
    dirs
}

// --- the parent ---

fn parent() -> SandboxResult {
    let Some(binary) = own_server_binary::binary()? else {
        if std::env::var("GPUI_SMOKE_REQUIRE").as_deref() == Ok("1") {
            return Err("no ngteams-server binary (NGTEAMS_QA_SERVER_BIN, or cargo build -p ngteams-server)".into());
        }
        say("qa_file_drop_own_window: skipped: no ngteams-server binary");
        return Ok(());
    };
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_file_drop_own_window", &options)? else {
        return Ok(());
    };
    let dir = tempfile::tempdir()?;
    let server = BinaryServer::start(&binary)?;
    let a_oid = server.add_user(A.0, A.1)?;
    server.add_user(B.0, B.1)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let ca = server.ca_file.to_string_lossy().into_owned();
    let at = (server.origin.as_str(), ca.as_str());
    let (_, bert) = own_account(&dir.path().join("b"), at, B.0, true)?;
    let outcome = walk(&sandbox, at, dir.path(), (&bert, &runtime), &a_oid);
    let log = server.log();
    for evidence in evidence_dirs().into_iter().skip(1) {
        let _ = fs::create_dir_all(&evidence);
        let _ = fs::write(evidence.join("qa697-own-server.log"), &log);
    }
    bert.handle.shutdown();
    if let Err(error) = outcome {
        return Err(format!("{error}{}", sandbox.logs()).into());
    }
    say("qa_file_drop_own_window: ok");
    Ok(())
}

type Bert<'a> = (&'a AccountEngine<MemorySecretStore>, &'a tokio::runtime::Runtime);

fn walk(sandbox: &Sandbox, server: (&str, &str), dir: &Path, bert: Bert<'_>, a_oid: &str) -> SandboxResult {
    let chat = b_makes_the_chat(bert, a_oid)?;
    let mut run = AppRun::launch(sandbox, server, &dir.join("a"))?;
    let outcome = drop_and_send(&mut run, sandbox, &dir.join("made-up"), &chat, bert);
    if let Err(error) = outcome {
        let _ = run.shot(sandbox, "qa697-own-failed.png");
        let _ = run.quit();
        return Err(error);
    }
    run.quit()
}

/// b makes its 1:1 chat with a and says hello in it; the chat's id.
fn b_makes_the_chat((engine, runtime): Bert<'_>, a_oid: &str) -> SandboxResult<String> {
    let made = runtime.block_on(engine.handle.write(Action::CreateChat {
        others: vec![Mri::new(format!("8:orgid:{a_oid}"))],
        topic: None,
    }))?;
    let chat = made.message_id.ok_or("b's chat with a was not made")?;
    let row = runtime.block_on(engine.handle.write(Action::Send {
        conversation: chat.clone(),
        text: MessageText::plain("Made-up hello")?,
    }))?;
    ensure(row.state == teams_store::OutboxState::Sent, || {
        format!("the hello is not sent: {:?}", row.state)
    })?;
    step(&format!("b made the chat {chat}"));
    Ok(chat)
}

/// The user drops the files onto the box with a draft, waits for the uploads and sends; a's chat shows them, b gets
/// them.
fn drop_and_send(run: &mut AppRun, sandbox: &Sandbox, dir: &Path, chat: &str, bert: Bert<'_>) -> SandboxResult {
    fs::create_dir_all(dir)?;
    let paths: Vec<PathBuf> = FILES.iter().map(|(name, _)| dir.join(name)).collect();
    for (path, (_, bytes)) in paths.iter().zip(FILES) {
        fs::write(path, bytes)?;
    }
    run.open(chat)?;
    let state = run.until("the chat's box", |state| state["box"]["bounds"].is_array())?;
    let at = box_centre(&state["box"])?;
    run.click(at)?;
    run.input.type_text(DRAFT)?;
    // the hand takes the mouse again: a move over the box (a drop right after the keys is a known failure, task 697
    // QA round 1, checked by qa_file_drop_window's typed_then_dropped)
    run.input.move_to(at.0 + 6, at.1)?;
    run.until("the draft", |state| {
        state["box"]["text"].as_str().is_some_and(|text| text.contains(DRAFT))
    })?;
    let seen = run.drop_files(sandbox, &paths, at)?;
    step(&format!("the source saw {seen:?}"));
    ensure(seen.contains(&DragEvent::Finished), || {
        format!("the drop wasn't taken: {seen:?}")
    })?;
    let state = run.until("both files uploaded", |state| {
        let files = state["box"]["files"].as_array().cloned().unwrap_or_default();
        files.len() == 2 && files.iter().all(|file| file["state"] == "ready")
    })?;
    step(&format!("staged: {}", state["box"]));
    run.shot(sandbox, "qa697-own-ready.png")?;
    run.click(box_text(&state["box"])?)?;
    run.input.key(keys::ENTER)?;
    let state = run.until("a's message with both files", |state| {
        state["items"]
            .as_array()
            .and_then(|items| items.last())
            .is_some_and(|item| sorted(&item["files"]) == names())
    })?;
    run.shot(sandbox, "qa697-own-sent.png")?;
    let item = state["items"]
        .as_array()
        .and_then(|items| items.last())
        .cloned()
        .unwrap_or_default();
    ensure(item["text"].as_str().is_some_and(|text| text.contains(DRAFT)), || {
        format!("the draft didn't go with the files: {item}")
    })?;
    b_gets_them(bert, chat, &item)
}

/// b's engine has a's message with both files, and reads each file's bytes as a dropped them.
fn b_gets_them((engine, runtime): Bert<'_>, chat: &str, item: &Value) -> SandboxResult {
    let names = names();
    let got = wait_until(WAIT, || {
        let page = runtime.block_on(engine.handle.messages(chat, None, 20)).ok()?;
        page.messages.into_iter().find(|message| {
            let properties = message.properties.clone().unwrap_or_default();
            names.iter().all(|name| properties.contains(name.as_str()))
        })
    });
    let message = got.ok_or("b's engine never had a's message with both files")?;
    step(&format!("b has the message {} from {:?}", message.id, message.sender));
    let links = item["links"].as_array().cloned().unwrap_or_default();
    let reads = engine.handle.requests();
    let mut wrong = String::new();
    for ((name, bytes), link) in FILES.iter().zip(&links) {
        let Some(link) = link.as_str().filter(|link| !link.is_empty()) else {
            let _ = write!(wrong, "no link of {name}; ");
            continue;
        };
        let read = wait_until(WAIT, || {
            let size = runtime.block_on(reads.file_size(link.to_owned())).ok()?;
            runtime
                .block_on(reads.file_range(link.to_owned(), 0, u32::try_from(size).ok()?))
                .ok()
        });
        match read {
            Some(range) if range.bytes == *bytes => step(&format!("b reads {name} as it was dropped")),
            Some(range) => {
                let _ = write!(wrong, "b reads {} other bytes of {name}; ", range.bytes.len());
            }
            None => {
                let _ = write!(wrong, "b cannot read {name}; ");
            }
        }
    }
    ensure(wrong.is_empty(), || wrong)
}

fn names() -> Vec<String> {
    let mut names: Vec<String> = FILES.iter().map(|(name, _)| (*name).to_owned()).collect();
    names.sort_unstable();
    names
}

fn sorted(files: &Value) -> Vec<String> {
    let mut names: Vec<String> = files
        .as_array()
        .map(|files| {
            files
                .iter()
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    names.sort_unstable();
    names
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a window's coordinates are small and positive"
)]
fn px(value: &Value) -> u32 {
    value.as_f64().unwrap_or_default().round().max(0.0) as u32
}

fn box_centre(boxed: &Value) -> SandboxResult<(u32, u32)> {
    let bounds = &boxed["bounds"];
    ensure(bounds.is_array(), || format!("the box has no bounds: {boxed}"))?;
    Ok((px(&bounds[0]) + px(&bounds[2]) / 2, px(&bounds[1]) + px(&bounds[3]) / 2))
}

/// The text line of the box: its top, over the file cards and the tools.
fn box_text(boxed: &Value) -> SandboxResult<(u32, u32)> {
    let (x, _) = box_centre(boxed)?;
    Ok((x, px(&boxed["bounds"][1]) + 20))
}

struct AppRun {
    running: Running,
    ctl: PathBuf,
    dir: PathBuf,
    input: VirtualInput,
    asked: u32,
}

impl AppRun {
    /// Starts a's app on the server with its data in `dir`; once its window is drawn.
    fn launch(sandbox: &Sandbox, (origin, ca): (&str, &str), dir: &Path) -> SandboxResult<Self> {
        fs::create_dir_all(dir)?;
        let dir_text = dir.to_string_lossy().into_owned();
        let child = sandbox.spawn(
            &std::env::current_exe()?,
            &[],
            &[
                (CHILD_ENV, "app"),
                (ORIGIN_ENV, origin),
                (CA_ENV, ca),
                (DIR_ENV, &dir_text),
            ],
        )?;
        let mut running = Running::new(child)?;
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
        let ctl = dir.join("ctl.sock");
        wait_until(WAIT, || Client::connect(&ctl, Timeouts::default()).ok()).ok_or("no control socket")?;
        let mut run = Self {
            running,
            ctl,
            dir: dir.to_owned(),
            input: sandbox.input()?,
            asked: 0,
        };
        run.input.move_to(REST.0, REST.1)?;
        Ok(run)
    }

    /// `ngteamsctl open chat <chat>`, again after a sync until the window shows that chat.
    fn open(&mut self, chat: &str) -> SandboxResult {
        let deadline = Instant::now() + WAIT;
        loop {
            let mut client = Client::connect(&self.ctl, Timeouts::default())?;
            let open = Request::Open {
                target: OpenTarget::Chat { chat: chat.to_owned() },
            };
            if let Err(error) = client.request(None, open) {
                step(&format!("a's app cannot open the chat yet: {error}"));
                let _ = client.request(None, Request::Sync);
            }
            thread::sleep(Duration::from_millis(500));
            if self.state()?["chat"] == chat {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("a's window never shows the chat {chat}").into());
            }
        }
    }

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

    fn state(&mut self) -> SandboxResult<Value> {
        let answer = self.question("state")?;
        serde_json::from_str(&answer).map_err(|error| format!("the state {answer}: {error}").into())
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

    fn click(&mut self, (x, y): (u32, u32)) -> SandboxResult {
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(Duration::from_millis(250));
        Ok(())
    }

    /// Dolphin's drag of `paths` from the screen's bottom left onto `to`, a rest over it, then let go; what the
    /// source was told once the drag ended.
    fn drop_files(&mut self, sandbox: &Sandbox, paths: &[PathBuf], to: (u32, u32)) -> SandboxResult<Vec<DragEvent>> {
        let uris = paths.iter().fold(String::new(), |mut uris, path| {
            let _ = write!(uris, "file://{}\r\n", path.display().to_string().replace(' ', "%20"));
            uris
        });
        let source = sandbox.drag_source(DragOptions {
            offers: vec![
                ("text/uri-list".to_owned(), uris.clone().into_bytes()),
                ("application/x-kde4-urilist".to_owned(), uris.into_bytes()),
            ],
            actions: DndAction::Copy | DndAction::Move,
            at: SOURCE_AT,
            size: SOURCE_SIZE,
            send_delay: Duration::ZERO,
        })?;
        self.input.move_to(PRESS.0, PRESS.1)?;
        thread::sleep(Duration::from_millis(200));
        self.input.button_down(keys::BTN_LEFT)?;
        for step in 1..=30_i64 {
            let along = |a: u32, b: u32| {
                u32::try_from(i64::from(a) + (i64::from(b) - i64::from(a)) * step / 30).unwrap_or_default()
            };
            self.input.move_to(along(PRESS.0, to.0), along(PRESS.1, to.1))?;
            thread::sleep(Duration::from_millis(12));
        }
        thread::sleep(Duration::from_millis(300));
        self.input.move_to(to.0 + 2, to.1)?;
        thread::sleep(Duration::from_millis(700));
        self.input.button_up(keys::BTN_LEFT)?;
        let seen = wait_until(WAIT, || {
            let seen = source.events();
            (seen.contains(&DragEvent::Finished) || seen.contains(&DragEvent::Cancelled)).then_some(seen)
        });
        self.input.move_to(REST.0, REST.1)?;
        seen.ok_or_else(|| format!("the drag never ended: {:?}", source.events()).into())
    }

    fn shot(&mut self, sandbox: &Sandbox, name: &str) -> SandboxResult<PathBuf> {
        self.input.move_to(REST.0, REST.1)?;
        thread::sleep(SETTLE);
        let png = sandbox.screenshot(name, false)?;
        for dir in evidence_dirs() {
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(name))?;
        }
        say(&format!("shot {name}"));
        Ok(png)
    }

    /// Quits as the tray's Quit does; once the app ended.
    fn quit(mut self) -> SandboxResult {
        fs::write(self.dir.join(QUIT_FILE), "")?;
        self.running
            .line_where(WAIT, |line| line == "quit")
            .map_err(|error| self.running.fail(&format!("the app did not quit: {error:?}")))?;
        let (status, stderr) = self.running.finish(WAIT)?;
        ensure(status.success(), || format!("the app exited {status}: {stderr}"))
    }
}

// --- the child ---

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let dir = PathBuf::from(env(DIR_ENV)?);
    let (origin, ca) = (env(ORIGIN_ENV)?, env(CA_ENV)?);
    let (account, engine) = own_account(&dir.join("account"), (&origin, &ca), A.0, true)?;
    let first = ngteams::accounts::Started {
        account,
        engine,
        calls: None,
    };
    let runner = support::runner(&dir)?;
    let settings = teams_ui::SettingsFile::new(dir.join("settings.toml"));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let ctl = dir.join("ctl.sock");
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: Vec::new(),
                trace: Arc::new(teams_ui::ViewTrace::off()),
                ctl: Some(ctl),
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
                answer(window, dir.join(ASK), cx);
                quit_when_asked(dir.join(QUIT_FILE), cx);
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
    say("quit");
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

fn quit_when_asked(trigger: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        while !trigger.exists() {
            cx.background_executor().timer(Duration::from_millis(100)).await;
        }
        cx.update(app::quit);
    })
    .detach();
}

/// Answers the parent's questions as they come.
fn answer(window: WindowHandle<KitRoot>, ask: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(10)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let (seq, verb) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            let reply = window
                .update(cx, |kit, _, cx| {
                    let shell = kit
                        .view()
                        .clone()
                        .downcast::<Root>()
                        .ok()?
                        .read(cx)
                        .shell_in(cx)
                        .cloned()?;
                    Some(on_shell(&shell, verb, cx))
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| "no shell".to_owned());
            say(&format!("re {seq} {}", reply.replace('\n', " ")));
        }
    })
    .detach();
}

fn on_shell(shell: &Entity<Shell>, verb: &str, cx: &App) -> String {
    if verb != "state" {
        return json!(format!("unknown {verb}")).to_string();
    }
    let Some(view) = shell.read(cx).chat() else {
        return json!({"chat": "", "items": [], "box": null}).to_string();
    };
    let view = view.read(cx);
    let items: Vec<Value> = view
        .message_items()
        .map(|item| {
            let text = match &item.body {
                teams_ui::chat_view::Body::Text(rendered) => rendered
                    .0
                    .blocks
                    .iter()
                    .flat_map(teams_ui::rich_text::Block::plain_lines)
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            let files: Vec<&str> = item.files.iter().map(|file| file.name.as_str()).collect();
            let links: Vec<String> = item
                .files
                .iter()
                .map(|file| file.open_link().unwrap_or_default().to_owned())
                .collect();
            json!({"text": text, "files": files, "links": links})
        })
        .collect();
    json!({"chat": view.conversation(), "items": items, "box": box_state(view.compose(), cx)}).to_string()
}

fn box_state(compose: &Entity<teams_ui::compose::Compose>, cx: &App) -> Value {
    let compose = compose.read(cx);
    let files: Vec<Value> = compose
        .attachments()
        .iter()
        .map(|file| {
            let state = match &file.state {
                AttachmentState::Uploading => "uploading".to_owned(),
                AttachmentState::Ready(_) => "ready".to_owned(),
                AttachmentState::Failed(why) => format!("failed: {why}"),
            };
            json!({"name": file.name.clone(), "state": state})
        })
        .collect();
    json!({
        "text": compose.value(cx).to_string(),
        "files": files,
        "problem": compose.problem().map(ToString::to_string),
        "bounds": compose.box_bounds().map(|bounds| {
            [bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height].map(f32::from)
        }),
    })
}
