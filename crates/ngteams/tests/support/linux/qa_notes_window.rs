//! QA of task 698 (round 1), in the UI sandbox: the user writes in the chat with themself ("<name> (You)",
//! `48:notes`), which was read-only before (`harness = false`: GPUI runs on the process's main thread).
//!
//! The real app runs as a child in the sandbox, with lab accounts a and b in one window on `teams-mock`. The parent is
//! the user: it types and presses keys with the sandbox's keyboard, drags files from the sandbox's drag source, puts
//! files and a GIF on the sandbox's clipboard, picks files in a fake file chooser, and clicks with the sandbox's
//! pointer. It asks the child what the views hold through a file in the sandbox dir, and it reads the writes on the
//! mock.
//!
//! - **First open**: a's notes are first in Favorites as "Lab A (You)". Empty (the service answers 404), they show
//!   "This is your space", the full box (it takes files) and no call button.
//! - **Notes**: text, bold text with a link, an emoji from the picker, a pasted GIF and a sticker of an imported pack.
//!   Each goes to a's own `48:notes`.
//! - **Actions** on a note, from the keys: edit, react, reply (a quote), copy, mark as unread, the menu, delete.
//! - **Files**: one attached (the file chooser), one pasted, one dropped. They go out in one note and nobody else
//!   gets a share.
//! - **History**: b has older notes. Its first open fails, so the view says "Older messages aren't synced yet" and
//!   "Load older" brings the notes. b's notes are its own. A note that a's other client wrote after the first note
//!   shows after a restart: the next sync reads it.
//! - **Restart**: every note stays. **Read-only**: the app's read-only mode shows the notes with no box.
//!
//! The runs (`walk`, `restart`, `read_only`) build on each other. `QA698_ONLY` (a comma list of run or check names)
//! runs only those. `QA698_EVIDENCE` names a directory that gets the screenshots and the results. Screenshots also go
//! to `target/ui-shots/qa698-*.png`. Every name, id, file and text is made up. Without weston 15, niri or
//! `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use std::{fs, future, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use image::RgbImage;
use ngteams::app::{self, Launch};
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_mock::{
    AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE, NOTES, PRESENCE_BASE,
    PROVISIONING_BASE, USER_PROFILE_BASE,
};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::Desktop;
use teams_ui::chat_view::{Body, ChatView, History, Item};
use teams_ui::compose::attachments::AttachmentState;
use teams_ui::compose::{Compose, Mode};
use teams_ui::editor::pictures::PictureState;
use teams_ui::fun_picker::{FunPicker, Place};
use teams_ui::shell::{Main, Shell};
use teams_ui::stickers::{ImportState, Shelf};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::drag::{DndAction, DragEvent, DragOptions};
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
use portal::{Answer, Chooser};

const CHILD_ENV: &str = "QA698_CHILD";
const ONLY_ENV: &str = "QA698_ONLY";
const EVIDENCE_ENV: &str = "QA698_EVIDENCE";
const ORIGIN_ENV: &str = "QA698_ORIGIN";
const TROUTER_ENV: &str = "QA698_TROUTER";
const TOKENS_ENV: &str = "QA698_TOKENS";
const DIR_ENV: &str = "QA698_DIR";
const READ_ONLY_ENV: &str = "QA698_READ_ONLY";
/// The file in the sandbox dir the parent asks the child through.
const ASK: &str = "qa698-ask";
const STARTUP: Duration = Duration::from_secs(180);
const WAIT: Duration = Duration::from_secs(40);
const POLL: Duration = Duration::from_millis(100);
/// How long the screen gets to settle before a shot.
const SETTLE: Duration = Duration::from_millis(800);
const STEP: Duration = Duration::from_millis(250);
/// How long the parent watches for something that must not happen.
const QUIET: Duration = Duration::from_millis(2000);
const SCREEN: (u32, u32) = (1280, 800);
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
/// The drag source's surface, at the screen's bottom left over the chat list, and where the pointer presses it.
const SOURCE_AT: (u32, u32) = (20, 600);
const SOURCE_SIZE: (u32, u32) = (200, 160);
const PRESS: (u32, u32) = (120, 680);
/// a's made-up OneDrive: its site and its drive's API path.
const SITE: &str = "https://madeup-my.sharepoint.com/personal/lab_a/";
/// What the empty notes show (`teams_ui::chat_view::NOTES_INTRO_TITLE`).
const INTRO: &str = "This is your space";
/// The texts of b's older notes, written in another client before the app starts.
const B_OLDER: [&str; 3] = [
    "Made-up older note one of b",
    "Made-up older note two of b",
    "Made-up older note three of b",
];
/// The note a's other client writes after a's first note.
const A_ELSEWHERE: &str = "Made-up note a wrote in another client";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_notes_window: {error}");
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

/// A path as the chat service routes it, its `%3A` and `%40` read back.
fn decoded(path: &str) -> String {
    path.replace("%3A", ":").replace("%3a", ":").replace("%40", "@")
}

// --- the parent ---

type Check = (&'static str, fn(&mut User<'_>) -> SandboxResult);

const WALK: [Check; 10] = [
    ("first_open", first_open),
    ("text_note", text_note),
    ("formatted_note", formatted_note),
    ("emoji_note", emoji_note),
    ("gif_note", gif_note),
    ("sticker_note", sticker_note),
    ("note_actions", note_actions),
    ("files", files),
    ("history_b", history_b),
    ("looks", looks),
];
const RESTART: [Check; 1] = [("after_restart", after_restart)];
const READ_ONLY: [Check; 1] = [("read_only_box", read_only_box)];

/// What the runs share: the sandbox, the mock, the chooser, the app's dir.
struct World<'a> {
    sandbox: &'a Sandbox,
    mock: &'a ChatServiceMock,
    runtime: &'a tokio::runtime::Runtime,
    trouter: &'a str,
    chooser: &'a Chooser,
    dir: &'a Path,
}

#[derive(Default)]
struct Tally {
    passed: Vec<String>,
    failed: Vec<(String, String)>,
}

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_notes_window", &options)? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mock = runtime.block_on(ChatServiceMock::start())?;
    let trouter = mock.start_trouter()?;
    seed(&mock, &runtime)?;
    let chooser = Chooser::start(sandbox.bus().address())?;
    let dir = tempfile::tempdir()?;
    let world = World {
        sandbox: &sandbox,
        mock: &mock,
        runtime: &runtime,
        trouter: trouter.origin(),
        chooser: &chooser,
        dir: dir.path(),
    };
    let tally = run_all(&world)?;
    drop(trouter);
    report(&tally);
    ensure(tally.failed.is_empty(), || {
        format!("{} checks failed", tally.failed.len())
    })
}

fn run_all(world: &World<'_>) -> SandboxResult<Tally> {
    let mut tally = Tally::default();
    let runs: [(&str, &[Check], bool); 3] = [
        ("walk", &WALK, false),
        ("restart", &RESTART, false),
        ("read_only", &READ_ONLY, true),
    ];
    for (run, checks, read_only) in runs {
        let picked: Vec<Check> = checks
            .iter()
            .copied()
            .filter(|(name, _)| wanted(run) || wanted(name))
            .collect();
        if picked.is_empty() {
            continue;
        }
        say(&format!("--- run {run} ---"));
        let running = launch(world, read_only)?;
        let mut user = User::new(world, running)?;
        for (name, check) in picked {
            user.run_check(name, check, &mut tally);
        }
        user.quit()?;
    }
    Ok(tally)
}

fn report(tally: &Tally) {
    let mut summary = String::new();
    for name in &tally.passed {
        let _ = writeln!(summary, "pass {name}");
    }
    for (name, why) in &tally.failed {
        let _ = writeln!(summary, "FAIL {name}: {why}");
    }
    say(&summary);
    if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(PathBuf::from(dir).join("results.txt"), &summary);
    }
    say(&format!(
        "qa_notes_window: {} passed, {} failed",
        tally.passed.len(),
        tally.failed.len()
    ));
}

/// The mock's made-up world for the notes: a's notes in its Favorites, b's older notes written in another client
/// (b's notes in no list).
fn seed(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) -> SandboxResult {
    mock.list_notes_in_favorites(LAB_A);
    // b's are in no list: the open is their first read, so the parent can make it fail
    for text in B_OLDER {
        post_elsewhere(mock, runtime, LAB_B, text)?;
    }
    ensure(mock.notes(LAB_A).is_empty(), || {
        "a has notes before the walk".to_owned()
    })?;
    ensure(mock.notes(LAB_B).len() == B_OLDER.len(), || {
        format!("b's older notes: {:?}", mock.notes(LAB_B))
    })
}

/// `mri`'s other Teams client posts `text` to its notes, as Teams on another device does.
fn post_elsewhere(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime, mri: &str, text: &str) -> SandboxResult {
    let url = format!(
        "{}/api/chatsvc/amer/v1/users/ME/conversations/{NOTES}/messages",
        mock.origin()
    );
    let token = mock.token(mri).ok_or("no token")?;
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let body = json!({
        "messagetype": "RichText/Html",
        "contenttype": "Text",
        "content": format!("<p>{text}</p>"),
        "clientmessageid": (stamp % 9_000_000_000_000_000_000).to_string(),
        "imdisplayname": if mri == LAB_A { "Lab A" } else { "Lab B" },
        "amsreferences": [],
        "properties": {"mentions": "[]", "files": "[]", "formatVariant": "TEAMS"},
    });
    let status = runtime.block_on(async {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()?
            .post(url)
            .bearer_auth(token)
            .header("X-MS-Migration", "True")
            .header("BehaviorOverride", "redirectAs404")
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map(|answer| answer.status().as_u16())
    })?;
    ensure((200..300).contains(&status), || {
        format!("the other client's note answered {status}")
    })
}

/// Starts the app with a and b on the mock, its store and settings in the world's dir; once its window is drawn.
fn launch(world: &World<'_>, read_only: bool) -> SandboxResult<Running> {
    let mut tokens = Vec::new();
    for mri in [LAB_A, LAB_B] {
        tokens.push(world.mock.token(mri).ok_or("the mock's token")?);
        tokens.push(world.mock.skypetoken(mri).ok_or("the mock's skypetoken")?);
    }
    let tokens = tokens.join(" ");
    let dir = world.dir.to_string_lossy().into_owned();
    let child = world.sandbox.spawn(
        &std::env::current_exe()?,
        &[],
        &[
            (CHILD_ENV, "1"),
            (READ_ONLY_ENV, if read_only { "1" } else { "0" }),
            (ORIGIN_ENV, &world.mock.origin()),
            (TROUTER_ENV, world.trouter),
            (TOKENS_ENV, &tokens),
            (DIR_ENV, &dir),
        ],
    )?;
    let mut running = Running::new(child)?;
    if let Err(error) = running.line_where(STARTUP, |line| line == "ready") {
        return Err(running.fail(&format!("no ready line: {error:?}{}", world.sandbox.logs())));
    }
    Ok(running)
}

/// One request the mock got, as the checks read it.
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    raw_path: String,
    query: String,
    body: String,
    token: String,
}

impl World<'_> {
    fn requests(&self) -> Vec<Seen> {
        self.runtime
            .block_on(self.mock.received_requests())
            .iter()
            .map(|request| Seen {
                method: request.method.as_str().to_owned(),
                path: decoded(request.url.path()),
                raw_path: request.url.path().to_owned(),
                query: request.url.query().unwrap_or_default().to_owned(),
                body: String::from_utf8_lossy(&request.body).into_owned(),
                token: request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.strip_prefix("Bearer "))
                    .unwrap_or_default()
                    .to_owned(),
            })
            .collect()
    }

    /// The writes (every method but `GET`) on `mri`'s own notes, oldest first.
    fn notes_writes(&self, mri: &str) -> Vec<Seen> {
        let token = self.mock.token(mri).unwrap_or_default();
        self.requests()
            .into_iter()
            .filter(|seen| seen.method != "GET" && seen.path.contains(NOTES) && seen.token == token)
            .collect()
    }

    /// The texts of `mri`'s notes as the service holds them, oldest first.
    fn note_texts(&self, mri: &str) -> Vec<String> {
        self.mock
            .notes(mri)
            .iter()
            .map(|note| note["content"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    /// The OneDrive answers of one file's upload, before the world sees it: a's drive, its owner, a private chat-files
    /// folder and the file's `PUT`.
    fn serve_upload(&self, name: &str, item: &str) {
        let owner = LAB_A.trim_start_matches("8:orgid:");
        let grants = json!({"value": [{
            "id": LAB_A, "roles": ["owner"], "grantedToV2": {"user": {"id": owner}}
        }]});
        let uploaded = json!({
            "id": item, "name": name, "size": attachment_bytes(name).len(),
            "webUrl": format!("{SITE}Documents/Microsoft%20Teams%20Chat%20Files/{}", name.replace(' ', "%20")),
            "parentReference": {"driveId": "made-up-drive"},
            "sharepointIds": {"listItemUniqueId": format!("00000000-0000-4000-8000-{item:0>12}")},
        });
        let me = json!({"id": owner, "userPrincipalName": "bot-a@lab.example.test"});
        let drive = json!({"owner": {"user": {"id": owner}}});
        let folder = json!({"id": "made-up-folder"});
        let mock = self.mock;
        self.runtime.block_on(async {
            let site = json!({"webUrl": format!("{SITE}Documents")});
            mock.answer_next("GET", "/v1.0/me/drive", 200, &site).await;
            // the upload reads them, then the write's own check reads them again
            for _ in 0..2 {
                mock.answer_next("GET", "/v1.0/me", 200, &me).await;
                mock.answer_next("GET", "/_api/v2.0/drive", 200, &drive).await;
                mock.answer_next("GET", "/Microsoft%20Teams%20Chat%20Files", 200, &folder)
                    .await;
                mock.answer_next("GET", "/permissions", 200, &grants).await;
            }
            mock.answer_next("PUT", ":/content", 201, &uploaded).await;
        });
    }

    /// The media service answers the next picture's send as the new object `object`, which then serves `bytes`.
    fn serve_picture(&self, object: &str, kind: &str, bytes: Vec<u8>) {
        let mock = self.mock;
        self.runtime.block_on(async {
            mock.answer_next("POST", "/v1/objects/", 201, &json!({ "id": object }))
                .await;
            mock.refuse_next("PUT", &format!("/v1/objects/{object}/content/imgpsh"), 201)
                .await;
            mock.serve_bytes(&format!("/v1/objects/{object}/views/imgo"), kind, bytes)
                .await;
        });
    }
}

// --- the user ---

/// The user at the sandbox: the pointer, the keyboard, the screenshots, and the child it asks about the window.
struct User<'a> {
    world: &'a World<'a>,
    running: Option<Running>,
    input: VirtualInput,
    asked: u32,
    marks: u32,
    files: u32,
    objects: u32,
    /// a's first note, which the actions work on.
    first: String,
}

impl<'a> User<'a> {
    fn new(world: &'a World<'a>, running: Running) -> SandboxResult<Self> {
        let mut user = Self {
            world,
            running: Some(running),
            input: world.sandbox.input()?,
            asked: 0,
            marks: 0,
            files: 0,
            objects: 0,
            first: String::new(),
        };
        user.park()?;
        thread::sleep(SETTLE);
        Ok(user)
    }

    fn run_check(&mut self, name: &'static str, check: fn(&mut User<'_>) -> SandboxResult, tally: &mut Tally) {
        say(&format!("check {name}"));
        match check(self) {
            Ok(()) => {
                say(&format!("  pass {name}"));
                tally.passed.push(name.to_owned());
            }
            Err(error) => {
                let error = error.to_string();
                let state = self.state().map(|state| state.to_string()).unwrap_or_default();
                let _ = self.shot(&format!("failed-{name}"));
                say(&format!("  FAIL {name}: {error}\n  state: {state}"));
                tally.failed.push((name.to_owned(), error));
            }
        }
        let _ = self.input.button_up(keys::BTN_LEFT);
        let _ = self.press("escape");
    }

    fn running(&self) -> SandboxResult<&Running> {
        self.running.as_ref().ok_or_else(|| "the app has quit".into())
    }

    fn ask(&mut self, verb: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = self.world.sandbox.path(&format!("{ASK}.staged"));
        fs::write(&staged, format!("{} {verb}", self.asked))?;
        fs::rename(&staged, self.world.sandbox.path(ASK))?;
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

    /// A unique made-up word for a note.
    fn mark(&mut self) -> String {
        self.marks += 1;
        format!("zq{}x", 100 + self.marks)
    }

    /// A screenshot as `qa698-<name>.png`, under `target/ui-shots/` and the evidence dir.
    fn shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        let file = format!("qa698-{name}.png");
        let png = self.world.sandbox.screenshot(&file, false)?;
        let kept = shots_dir()?.join(&file);
        fs::copy(&png, &kept)?;
        if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
            let dir = PathBuf::from(dir);
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(&file))?;
        }
        say(&format!("  shot {}", kept.display()));
        Ok(kept)
    }

    /// The shots of `name` in Teams Light, then Teams Dark, then Light again; the pointer parked.
    fn shots(&mut self, name: &str) -> SandboxResult {
        self.park()?;
        self.ask("theme-light")?;
        thread::sleep(SETTLE);
        self.shot(&format!("{name}-light"))?;
        self.ask("theme-dark")?;
        thread::sleep(SETTLE);
        self.shot(&format!("{name}-dark"))?;
        self.ask("theme-light")?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// Shows the account `alias` (a or b), as the rail's click does.
    fn switch(&mut self, alias: &str) -> SandboxResult<Value> {
        let answer = self.ask(&format!("switch {alias}"))?;
        ensure(answer == "ok", || format!("switch {alias}: {answer}"))?;
        let upn = format!("bot-{alias}@");
        self.until(&format!("account {alias} shown"), |state| {
            state["account"].as_str().is_some_and(|shown| shown.starts_with(&upn))
        })
    }

    /// Opens the notes of the shown account, as a click on their row in Favorites does; once they show.
    fn open_notes(&mut self) -> SandboxResult<Value> {
        self.ask(&format!("open {NOTES}"))?;
        let state = self.until("the notes shown", |state| {
            state["main"] == format!("chat:{NOTES}") && state["compose"]["bounds"].is_array()
                || state["main"] == format!("chat:{NOTES}") && state["compose"]["read_only"] == true
        })?;
        thread::sleep(SETTLE);
        Ok(state)
    }

    /// A click in the box's text line, as a user starts to type.
    fn click_box(&mut self) -> SandboxResult {
        let state = self.state()?;
        let at = text_line(&state["compose"])?;
        self.click(at)?;
        self.park()
    }

    /// Types `text` in the box and sends it with Enter; the note's id once the mock holds a note with `mark`.
    fn send_text(&mut self, text: &str, mark: &str) -> SandboxResult<String> {
        self.click_box()?;
        self.type_text(text)?;
        self.press("enter")?;
        self.note_with(mark)
    }

    /// The id of the note of a that holds `mark`, once the mock has it.
    fn note_with(&mut self, mark: &str) -> SandboxResult<String> {
        self.note_of(LAB_A, mark)
    }

    fn note_of(&mut self, mri: &str, mark: &str) -> SandboxResult<String> {
        let found = wait_until(WAIT, || {
            self.world.mock.notes(mri).iter().find_map(|note| {
                note["content"]
                    .as_str()
                    .is_some_and(|content| content.contains(mark))
                    .then(|| note["id"].as_str().map(str::to_owned))
                    .flatten()
            })
        });
        found.ok_or_else(|| {
            format!(
                "no note with {mark:?} reached the service: {:?}",
                self.world.note_texts(mri)
            )
            .into()
        })
    }

    /// The chat view shows the message `id`; its row.
    fn shown(&mut self, id: &str) -> SandboxResult<Value> {
        let state = self.until(&format!("the note {id} shown"), |state| item(state, id).is_some())?;
        item(&state, id).cloned().ok_or_else(|| "gone".into())
    }

    /// Selects the note `id` as the keys do: a click in the box, Escape, then Up until it is selected.
    fn select(&mut self, id: &str) -> SandboxResult {
        self.click_box()?;
        self.press("escape")?;
        // a selection left from before starts where it was: Up first, then Down
        for key in ["up", "down"] {
            for _ in 0..30 {
                self.press(key)?;
                if self.state()?["chat"]["selected"] == id {
                    return Ok(());
                }
            }
        }
        Err(format!("Up and Down never selected the note {id}").into())
    }

    /// Made-up files of `names` in a fresh folder of the sandbox, a few bytes each.
    fn made_up(&mut self, names: &[&str]) -> SandboxResult<Vec<PathBuf>> {
        self.files += 1;
        let dir = self.world.sandbox.path(&format!("files-{}", self.files));
        fs::create_dir_all(&dir)?;
        names
            .iter()
            .map(|name| {
                let path = dir.join(name);
                fs::write(&path, attachment_bytes(name))?;
                Ok(path)
            })
            .collect()
    }

    /// Drags `paths` from a file manager (Dolphin's offer) onto `to` and lets go; once the drag ended.
    fn drop_files(&mut self, paths: &[PathBuf], to: (u32, u32)) -> SandboxResult<Vec<DragEvent>> {
        let (uris, plain) = file_lists(paths);
        let source = self.world.sandbox.drag_source(DragOptions {
            offers: vec![
                ("text/uri-list".to_owned(), uris.clone().into_bytes()),
                ("application/x-kde4-urilist".to_owned(), uris.into_bytes()),
                ("text/plain;charset=utf-8".to_owned(), plain.into_bytes()),
            ],
            actions: DndAction::from_bits_truncate(DndAction::Copy.bits() | DndAction::Move.bits()),
            at: SOURCE_AT,
            size: SOURCE_SIZE,
            send_delay: Duration::ZERO,
        })?;
        self.input.move_to(PRESS.0, PRESS.1)?;
        thread::sleep(Duration::from_millis(200));
        self.input.button_down(keys::BTN_LEFT)?;
        self.glide(PRESS, to)?;
        thread::sleep(Duration::from_millis(300));
        self.input.move_to(to.0 + 2, to.1)?;
        thread::sleep(Duration::from_millis(700));
        self.input.button_up(keys::BTN_LEFT)?;
        let ended = wait_until(WAIT, || {
            let seen = source.events();
            (seen.contains(&DragEvent::Finished) || seen.contains(&DragEvent::Cancelled)).then_some(seen)
        });
        self.park()?;
        ended.ok_or_else(|| format!("the drag never ended: {:?}", source.events()).into())
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

    /// Puts `offers` on the clipboard, as a copy in another program does, then pastes them into the box (Ctrl+V).
    fn paste(&mut self, offers: Vec<(String, Vec<u8>)>) -> SandboxResult {
        self.world.sandbox.set_clipboard(offers)?;
        thread::sleep(STEP);
        self.click_box()?;
        self.press("ctrl+v")
    }

    /// A fresh media object's id for the next picture.
    fn object(&mut self) -> String {
        self.objects += 1;
        format!("0-eus-d1-{:032x}", 0x0069_8000 + self.objects)
    }

    /// The bodies of the media service's object creations, oldest first.
    fn object_creations(&self) -> Vec<String> {
        self.world
            .requests()
            .into_iter()
            .filter(|seen| seen.method == "POST" && seen.path.ends_with("/v1/objects/"))
            .map(|seen| seen.body)
            .collect()
    }
}

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

// --- the views as the child says them ---

fn item<'v>(state: &'v Value, id: &str) -> Option<&'v Value> {
    state["chat"]["items"].as_array()?.iter().find(|row| row["id"] == id)
}

fn item_ids(state: &Value) -> Vec<String> {
    state["chat"]["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|row| row["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn staged(compose: &Value) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = compose["files"]
        .as_array()
        .map(|files| {
            files
                .iter()
                .map(|file| {
                    (
                        file["name"].as_str().unwrap_or_default().to_owned(),
                        file["state"].as_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort_unstable();
    files
}

fn pictures(state: &Value) -> Vec<Value> {
    state["compose"]["pictures"].as_array().cloned().unwrap_or_default()
}

fn picker_open(state: &Value) -> bool {
    !state["picker"].is_null()
}

fn picker_rows(state: &Value) -> Vec<Vec<String>> {
    state["picker"]["rows"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    row.as_array()
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| item.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn position(rows: &[Vec<String>], selector: &str) -> Option<(usize, usize)> {
    rows.iter().enumerate().find_map(|(row, items)| {
        items
            .iter()
            .position(|item| item == selector)
            .map(|column| (row, column))
    })
}

fn step_toward(now: (usize, usize), goal: (usize, usize)) -> &'static str {
    if now.0 < goal.0 {
        "down"
    } else if now.0 > goal.0 {
        "up"
    } else if now.1 < goal.1 {
        "right"
    } else {
        "left"
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a window's coordinates are small and positive"
)]
fn px(value: &Value) -> u32 {
    value.as_f64().unwrap_or_default().round().max(0.0) as u32
}

/// The box's text line: the middle of its first 40 px.
fn text_line(compose: &Value) -> SandboxResult<(u32, u32)> {
    let bounds = &compose["bounds"];
    ensure(bounds.is_array(), || format!("the box has no bounds: {compose}"))?;
    Ok((
        px(&bounds[0]) + px(&bounds[2]) / 2,
        px(&bounds[1]) + px(&bounds[3]).min(40) / 2,
    ))
}

/// A place over the messages above the box.
fn over_messages(compose: &Value) -> SandboxResult<(u32, u32)> {
    let (x, _) = text_line(compose)?;
    Ok((x, px(&compose["bounds"][1]).saturating_sub(160)))
}

impl User<'_> {
    /// Ctrl+. in the box: the picker opens.
    fn open_picker(&mut self) -> SandboxResult<Value> {
        self.click_box()?;
        self.press("ctrl+.")?;
        self.until("the picker opened by Ctrl+.", picker_open)
    }

    /// The keyboard moves to the item `selector` in the picker and presses Enter.
    fn key_pick(&mut self, selector: &str) -> SandboxResult {
        for _ in 0..80 {
            let state = self.state()?;
            ensure(picker_open(&state), || format!("the picker closed before {selector}"))?;
            if state["picker"]["current"] == selector {
                return self.press("enter");
            }
            let rows = picker_rows(&state);
            let goal = position(&rows, selector).ok_or_else(|| format!("no {selector} in the picker: {rows:?}"))?;
            let key = match state["picker"]["place"].as_str() {
                Some("search" | "tabs") => "down",
                Some("items") => {
                    let now = state["picker"]["current"]
                        .as_str()
                        .and_then(|current| position(&rows, current))
                        .unwrap_or((0, 0));
                    step_toward(now, goal)
                }
                _ => return Err(format!("the keyboard is not in the picker: {}", state["picker"]).into()),
            };
            self.press(key)?;
        }
        Err(format!("the keyboard never reached {selector}").into())
    }
}

/// A made-up animated GIF of `size` px: a square of each colour of `colours` in turn.
fn made_up_gif(size: u32, colours: &[[u8; 3]]) -> SandboxResult<Vec<u8>> {
    let mut bytes = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
        encoder.set_repeat(image::codecs::gif::Repeat::Infinite)?;
        for colour in colours {
            let frame = image::RgbaImage::from_pixel(size, size, image::Rgba([colour[0], colour[1], colour[2], 255]));
            encoder.encode_frame(image::Frame::from_parts(
                frame,
                0,
                0,
                image::Delay::from_numer_denom_ms(200, 1),
            ))?;
        }
    }
    Ok(bytes)
}

/// A made-up still sticker: a filled circle of `colour` on a clear ground.
fn made_up_sticker(path: &Path, colour: [u8; 3]) -> SandboxResult {
    let size = 160_u32;
    let image = image::RgbaImage::from_fn(size, size, |x, y| {
        let (dx, dy) = (f64::from(x) - 80.0, f64::from(y) - 80.0);
        if dx.hypot(dy) < 70.0 {
            image::Rgba([colour[0], colour[1], colour[2], 255])
        } else {
            image::Rgba([0, 0, 0, 0])
        }
    });
    image.save(path)?;
    Ok(())
}

/// The made-up content of the file `name`: each file's own, so a check can tell the uploads apart.
fn attachment_bytes(name: &str) -> Vec<u8> {
    format!("A synthetic attachment, not a real document: {name}.\n").into_bytes()
}

/// The share writes the mock got: grants or invitations of a file, the only way someone else gets it.
fn shares(seen: &[Seen]) -> Vec<String> {
    seen.iter()
        .filter(|seen| {
            seen.path.ends_with("/invite")
                || seen.path.contains("/grant")
                || seen.method == "POST" && seen.path.ends_with("/permissions")
        })
        .map(|seen| format!("{} {}", seen.method, seen.path))
        .collect()
}

// --- the checks: the walk ---

/// a's notes first in Favorites as "Lab A (You)"; opened empty (404), they show "This is your space", the full box
/// that takes files, and no call button.
fn first_open(user: &mut User<'_>) -> SandboxResult {
    user.ask("theme-light")?;
    let state = user.until("a's Favorites", |state| state["favorites"][0]["id"] == NOTES)?;
    let name = state["favorites"][0]["name"].as_str().unwrap_or_default().to_owned();
    say(&format!("  a's notes row: {name:?}"));
    ensure(name.ends_with(" (You)") && name.contains("Lab A"), || {
        format!("the notes row is named {name:?}, not \"Lab A (You)\"")
    })?;
    user.open_notes()?;
    let state = user.until("the empty notes read to their beginning", |state| {
        state["chat"]["history"] == "Complete"
    })?;
    let chat = &state["chat"];
    let compose = &state["compose"];
    say(&format!(
        "  empty notes: intro {}, top {:?}, items {}, read-only {}, takes files {}, call {} ({})",
        chat["intro"],
        chat["top_label"],
        item_ids(&state).len(),
        compose["read_only"],
        compose["accepts_files"],
        chat["call_button"],
        chat["can_call"]
    ));
    ensure(chat["intro"] == true, || {
        format!("no \"{INTRO}\" in the empty notes: {chat}")
    })?;
    ensure(item_ids(&state).is_empty(), || {
        format!("the empty notes hold {:?}", item_ids(&state))
    })?;
    ensure(compose["read_only"] == false && compose["bounds"].is_array(), || {
        format!("the notes have no box: {compose}")
    })?;
    ensure(compose["accepts_files"] == true, || {
        format!("the notes' box takes no files: {compose}")
    })?;
    ensure(chat["can_call"] == false && chat["call_button"] == "none", || {
        format!("the notes offer a call: {chat}")
    })?;
    user.shots("notes-empty")
}

/// a writes its first note: it goes to a's own notes, shows as a's, and "This is your space" goes. a's other client
/// then writes a note, which the next sync reads ([`after_restart`]).
fn text_note(user: &mut User<'_>) -> SandboxResult {
    let mark = user.mark();
    let id = user.send_text(&format!("Made-up first note to myself {mark}"), &mark)?;
    let row = user.shown(&id)?;
    ensure(row["own"] == true && row["kind"] == "text", || {
        format!("the note shows as {row}")
    })?;
    let state = user.until("the intro gone", |state| state["chat"]["intro"] == false)?;
    user.first.clone_from(&id);
    post_elsewhere(user.world.mock, user.world.runtime, LAB_A, A_ELSEWHERE)?;
    // the box's typing goes to the same path as a Control/Typing message
    let posts: Vec<Seen> = user
        .world
        .notes_writes(LAB_A)
        .into_iter()
        .filter(|seen| {
            seen.method == "POST" && seen.path.ends_with(&format!("{NOTES}/messages")) && seen.body.contains(&mark)
        })
        .collect();
    ensure(posts.len() == 1, || {
        format!("a's first note went out {} times", posts.len())
    })?;
    say(&format!("  first note {id}: {} items shown", item_ids(&state).len()));
    user.shots("note-text")
}

/// Bold words (Ctrl+B) and a link in one note: the note's HTML has both.
fn formatted_note(user: &mut User<'_>) -> SandboxResult {
    let mark = user.mark();
    user.click_box()?;
    user.press("ctrl+b")?;
    user.type_text(&format!("Bold made-up words {mark}"))?;
    user.press("ctrl+b")?;
    user.type_text(" and a made-up link https://example.test/made-up-page ")?;
    user.press("enter")?;
    let id = user.note_with(&mark)?;
    let content = content_of(user, LAB_A, &id);
    say(&format!("  formatted note: {content}"));
    ensure(content.contains("<b>") || content.contains("<strong"), || {
        format!("no bold text in the note: {content}")
    })?;
    // the app links no typed URL in any chat: recorded for the report, not this task's change
    let linked = content.contains("href=\"https://example.test/made-up-page\"");
    say(&format!("  FINDING-LINK typed URL linked: {linked}"));
    user.shown(&id)?;
    user.shots("note-formatted")
}

/// An emoji from the picker (Ctrl+., a search, Enter) with words after it.
fn emoji_note(user: &mut User<'_>) -> SandboxResult {
    user.open_picker()?;
    user.type_text("smile")?;
    let state = user.until("emoji for \"smile\"", |state| {
        picker_rows(state)
            .iter()
            .flatten()
            .any(|item| item.starts_with("fun-emoji-"))
    })?;
    let emoji = picker_rows(&state)
        .into_iter()
        .flatten()
        .find(|item| item.starts_with("fun-emoji-"))
        .ok_or("no emoji")?;
    user.key_pick(&emoji)?;
    if picker_open(&user.state()?) {
        user.press("escape")?;
    }
    let mark = user.mark();
    user.press("end")?;
    user.type_text(&format!(" made-up emoji note {mark}"))?;
    user.press("enter")?;
    let id = user.note_with(&mark)?;
    let content = content_of(user, LAB_A, &id);
    say(&format!("  emoji note ({emoji}): {content}"));
    ensure(
        content.contains("schema.skype.com/Emoji") || !content.is_ascii(),
        || format!("no emoji in the note: {content}"),
    )?;
    user.shown(&id)?;
    user.shots("note-emoji")
}

/// A GIF copied in another program and pasted in the box: it goes to the media service, then out in a note.
fn gif_note(user: &mut User<'_>) -> SandboxResult {
    let gif = made_up_gif(96, &[[0x22, 0x88, 0xdd], [0xdd, 0x66, 0x22], [0x33, 0xaa, 0x55]])?;
    let object = user.object();
    user.world.serve_picture(&object, "image/gif", gif.clone());
    let before = user.object_creations().len();
    user.paste(vec![("image/gif".to_owned(), gif)])?;
    user.until("the GIF in the box, sent", |state| {
        let pictures = pictures(state);
        pictures.len() == 1 && pictures[0]["state"] == "sent"
    })?;
    let created = user.object_creations();
    let body = created.get(before).cloned().unwrap_or_default();
    say(&format!("  the GIF's object: {body}"));
    ensure(body.contains(NOTES), || {
        format!("the GIF's object is not for the notes: {body}")
    })?;
    user.press("enter")?;
    let id = user.note_with(&object)?;
    say(&format!("  GIF note: {}", content_of(user, LAB_A, &id)));
    user.shown(&id)?;
    thread::sleep(SETTLE);
    user.shots("note-gif")
}

/// A sticker of a pack the user imports from a folder (the picker's Stickers tab, the file chooser), sent in a note.
fn sticker_note(user: &mut User<'_>) -> SandboxResult {
    let folder = user.world.sandbox.path("pack/Made-up notes pack");
    fs::create_dir_all(&folder)?;
    made_up_sticker(&folder.join("01 made-up circle.png"), [0xe0, 0x40, 0x90])?;
    user.open_picker()?;
    user.press("ctrl+tab")?;
    user.press("ctrl+tab")?;
    let state = user.until("the Stickers tab", |state| state["picker"]["tab"] == "Stickers")?;
    user.world.chooser.clear();
    if position(&picker_rows(&state), "sticker-import-folder").is_none() {
        user.key_pick("sticker-pack-add")?;
    }
    user.world.chooser.queue(Answer::Pick(folder));
    user.key_pick("sticker-import-folder")?;
    let state = user.until("the pack imported", |state| {
        state["shelf"]["import"]["done"].is_string()
    })?;
    let pack = state["shelf"]["import"]["done"].as_str().unwrap_or_default().to_owned();
    let file = state["shelf"]["packs"]
        .as_array()
        .and_then(|packs| packs.iter().find(|one| one["id"] == pack.as_str()))
        .and_then(|one| one["stickers"][0]["file"].as_str())
        .ok_or_else(|| format!("no sticker in the pack: {}", state["shelf"]))?
        .to_owned();
    let bytes = fs::read(user.world.dir.join("stickers").join(&pack).join(&file))?;
    let object = user.object();
    user.world.serve_picture(&object, "image/png", bytes);
    user.key_pick("sticker-import-open")?;
    let number = file.split('.').next().unwrap_or_default();
    user.key_pick(&format!("sticker-{pack}-{number}"))?;
    user.until("the sticker in the box, sent", |state| {
        let pictures = pictures(state);
        !picker_open(state)
            && pictures.len() == 1
            && pictures[0]["sticker"].is_string()
            && pictures[0]["state"] == "sent"
    })?;
    user.press("enter")?;
    let id = user.note_with(&object)?;
    let content = content_of(user, LAB_A, &id);
    say(&format!("  sticker note: {content}"));
    ensure(content.contains("schema.skype.com/Sticker"), || {
        format!("the note is no sticker: {content}")
    })?;
    user.shown(&id)?;
    thread::sleep(SETTLE);
    user.shots("note-sticker")
}

/// The content of `mri`'s note `id` as the service holds it.
fn content_of(user: &User<'_>, mri: &str, id: &str) -> String {
    user.world
        .mock
        .notes(mri)
        .iter()
        .find(|note| note["id"] == id)
        .and_then(|note| note["content"].as_str())
        .unwrap_or_default()
        .to_owned()
}

impl User<'_> {
    /// Waits for a write on a's notes after `since` of them that `wanted` takes; it.
    fn write_after(&self, since: usize, what: &str, wanted: impl Fn(&Seen) -> bool) -> SandboxResult<Seen> {
        let found = wait_until(WAIT, || {
            self.world
                .notes_writes(LAB_A)
                .into_iter()
                .skip(since)
                .find(|seen| wanted(seen))
        });
        found.ok_or_else(|| {
            let last: Vec<String> = self
                .world
                .notes_writes(LAB_A)
                .into_iter()
                .skip(since)
                .map(|seen| format!("{} {}?{}", seen.method, seen.path, seen.query))
                .collect();
            format!("{what}: no such write on the notes; after {since}: {last:?}").into()
        })
    }

    fn writes_now(&self) -> usize {
        self.world.notes_writes(LAB_A).len()
    }
}

/// Edit (E), react (A, 1), reply (R), copy (Ctrl+C), mark as unread (U), the menu (Shift+F10) on a's first note, and
/// delete (Delete, Enter) on another note: each goes out as Teams writes it on a chat.
fn note_actions(user: &mut User<'_>) -> SandboxResult {
    let id = user.first.clone();
    ensure(!id.is_empty(), || "no first note to act on".to_owned())?;
    edit_note(user, &id)?;
    react_note(user, &id)?;
    reply_note(user, &id)?;
    copy_note(user, &id)?;
    unread_note(user, &id)?;
    menu_note(user, &id)?;
    delete_note(user)?;
    user.shots("note-actions")
}

fn edit_note(user: &mut User<'_>, id: &str) -> SandboxResult {
    user.select(id)?;
    let since = user.writes_now();
    user.press("e")?;
    user.until("the box edits the note", |state| state["compose"]["mode"] == "edit")?;
    let mark = user.mark();
    user.press("ctrl+end")?;
    user.type_text(&format!(" edited {mark}"))?;
    user.shot("note-editing-light")?;
    user.press("enter")?;
    let put = user.write_after(since, "the edit", |seen| {
        seen.method == "PUT" && seen.path.ends_with(&format!("/messages/{id}"))
    })?;
    ensure(put.body.contains(&mark), || format!("the edit's body: {}", put.body))?;
    user.until("the note shown edited", |state| {
        item(state, id).is_some_and(|row| row["edited"] == true)
    })?;
    Ok(())
}

fn react_note(user: &mut User<'_>, id: &str) -> SandboxResult {
    user.select(id)?;
    let since = user.writes_now();
    user.press("a")?;
    thread::sleep(STEP);
    user.press("1")?;
    let put = user.write_after(since, "the reaction", |seen| {
        seen.method == "PUT"
            && seen.path.contains(&format!("/messages/{id}/properties"))
            && seen.query.contains("emotions")
    })?;
    say(&format!("  reaction: {}", put.body));
    user.until("the reaction shown", |state| {
        item(state, id).is_some_and(|row| row["reactions"].as_u64().unwrap_or(0) > 0)
    })?;
    Ok(())
}

fn reply_note(user: &mut User<'_>, id: &str) -> SandboxResult {
    user.select(id)?;
    user.press("r")?;
    user.until("the box replies", |state| state["compose"]["mode"] == "reply")?;
    let mark = user.mark();
    user.type_text(&format!("Made-up reply to my own note {mark}"))?;
    user.press("enter")?;
    let reply = user.note_with(&mark)?;
    let content = content_of(user, LAB_A, &reply);
    say(&format!("  reply: {content}"));
    ensure(
        content.contains("schema.skype.com/Reply") && content.contains(id),
        || format!("the reply quotes no note {id}: {content}"),
    )?;
    user.shown(&reply)?;
    Ok(())
}

fn copy_note(user: &mut User<'_>, id: &str) -> SandboxResult {
    user.world.sandbox.clear_clipboard()?;
    user.select(id)?;
    user.press("ctrl+c")?;
    let found = wait_until(WAIT, || {
        let offers = user.world.sandbox.read_clipboard().ok()?;
        offers
            .iter()
            .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
            .find(|text| text.contains("Made-up first note to myself"))
    });
    let text = found.ok_or("Ctrl+C put no note text on the clipboard")?;
    say(&format!("  copied: {:?}", text.lines().next().unwrap_or_default()));
    Ok(())
}

fn unread_note(user: &mut User<'_>, id: &str) -> SandboxResult {
    user.select(id)?;
    let since = user.writes_now();
    user.press("u")?;
    let put = user.write_after(since, "the mark as unread", |seen| {
        seen.method == "PUT" && seen.query.contains("consumptionHorizonBookmark")
    })?;
    say(&format!("  marked unread: {}", put.body));
    Ok(())
}

fn menu_note(user: &mut User<'_>, id: &str) -> SandboxResult {
    user.select(id)?;
    user.press("shift+F10")?;
    user.until("the note's menu", |state| state["chat"]["options"] == id)?;
    thread::sleep(SETTLE);
    user.shot("note-menu-light")?;
    user.ask("theme-dark")?;
    thread::sleep(SETTLE);
    user.shot("note-menu-dark")?;
    user.ask("theme-light")?;
    user.press("escape")?;
    user.until("the menu closed", |state| state["chat"]["options"].is_null())?;
    Ok(())
}

fn delete_note(user: &mut User<'_>) -> SandboxResult {
    let mark = user.mark();
    let id = user.send_text(&format!("Made-up note to delete {mark}"), &mark)?;
    user.shown(&id)?;
    user.select(&id)?;
    let since = user.writes_now();
    user.press("delete")?;
    thread::sleep(STEP);
    user.press("enter")?;
    let write = user.write_after(since, "the delete", |seen| {
        seen.path.contains(&format!("/messages/{id}")) && (seen.method == "DELETE" || seen.query.contains("softDelete"))
    })?;
    say(&format!("  deleted: {} {}?{}", write.method, write.path, write.query));
    user.until("the note shown deleted", |state| {
        item(state, &id).is_some_and(|row| row["kind"] == "deleted")
    })?;
    Ok(())
}

/// A file attached (Ctrl+Shift+U and the file chooser), one pasted (a file manager's copy) and one dropped from a
/// file manager: all three in the box, then out in one note. No grant or invitation goes anywhere, and b gets nothing.
fn files(user: &mut User<'_>) -> SandboxResult {
    let b_notes = user.world.mock.notes(LAB_B).len();
    let _ = user.world.mock.take_events(LAB_B);
    let attached = user.made_up(&["made-up attached note.txt"])?;
    user.world.serve_upload("made-up attached note.txt", "a1");
    user.world.chooser.clear();
    user.world.chooser.queue(Answer::Pick(attached[0].clone()));
    user.click_box()?;
    user.press("ctrl+shift+u")?;
    let ready = user.until("the attached file ready", |state| {
        staged(&state["compose"]) == [("made-up attached note.txt".to_owned(), "ready".to_owned())]
    });
    if ready.is_err() {
        let drive: Vec<String> = user
            .world
            .requests()
            .into_iter()
            .filter(|seen| seen.path.contains("/v1.0/") || seen.path.contains("/_api/"))
            .map(|seen| format!("{} {}", seen.method, seen.raw_path))
            .collect();
        say(&format!("  the drive requests: {drive:?}"));
    }
    ready?;
    let pasted = user.made_up(&["made-up pasted note.txt"])?;
    user.world.serve_upload("made-up pasted note.txt", "a2");
    let (uris, _) = file_lists(&pasted);
    user.paste(vec![("text/uri-list".to_owned(), uris.into_bytes())])?;
    user.until("the pasted file ready", |state| {
        staged(&state["compose"])
            .iter()
            .filter(|(_, state)| state == "ready")
            .count()
            == 2
    })?;
    let dropped = user.made_up(&["made-up dropped note.txt"])?;
    user.world.serve_upload("made-up dropped note.txt", "a3");
    let state = user.state()?;
    let seen = user.drop_files(&dropped, over_messages(&state["compose"])?)?;
    let state = user.until("the dropped file ready", |state| {
        staged(&state["compose"])
            .iter()
            .filter(|(_, state)| state == "ready")
            .count()
            == 3
    })?;
    say(&format!(
        "  staged {:?}; the drag source saw {seen:?}",
        staged(&state["compose"])
    ));
    user.shots("files-staged")?;
    send_files(user, b_notes)
}

fn send_files(user: &mut User<'_>, b_notes: usize) -> SandboxResult {
    let mark = user.mark();
    user.click_box()?;
    user.type_text(&format!("Made-up files to myself {mark}"))?;
    user.press("enter")?;
    let id = user.note_with(&mark)?;
    let note = user
        .world
        .mock
        .notes(LAB_A)
        .into_iter()
        .find(|note| note["id"] == id.as_str())
        .unwrap_or_default();
    let listed = note["properties"]["files"].as_str().unwrap_or_default().to_owned();
    say(&format!("  the files note's files: {listed}"));
    for name in [
        "made-up attached note.txt",
        "made-up pasted note.txt",
        "made-up dropped note.txt",
    ] {
        ensure(listed.contains(name), || format!("{name} is not in the note: {listed}"))?;
    }
    thread::sleep(QUIET);
    let requests = user.world.requests();
    let token = user.world.mock.token(LAB_A).unwrap_or_default();
    let sent: Vec<String> = requests
        .iter()
        .filter(|seen| seen.method == "POST" && seen.path.ends_with("/messages") && seen.body.contains(&mark))
        .map(|seen| {
            format!(
                "{} {}",
                seen.path,
                if seen.token == token { "as a" } else { "as another" }
            )
        })
        .collect();
    ensure(
        sent.len() == 1 && sent[0].ends_with(&format!("{NOTES}/messages as a")),
        || format!("the files note went to: {sent:?}"),
    )?;
    for name in [
        "made-up attached note.txt",
        "made-up pasted note.txt",
        "made-up dropped note.txt",
    ] {
        let expected = String::from_utf8_lossy(&attachment_bytes(name)).into_owned();
        let puts: Vec<&Seen> = requests
            .iter()
            .filter(|seen| {
                seen.method == "PUT"
                    && seen
                        .raw_path
                        .ends_with(&format!("/{}:/content", name.replace(' ', "%20")))
            })
            .collect();
        ensure(puts.len() == 1 && puts[0].body == expected, || {
            let bodies: Vec<&str> = puts.iter().map(|seen| seen.body.as_str()).collect();
            format!("the uploads of {name}: {bodies:?}")
        })?;
    }
    let shared = shares(&requests);
    ensure(shared.is_empty(), || {
        format!("the notes' files were shared: {shared:?}")
    })?;
    let told: Vec<_> = user.world.mock.take_events(LAB_B);
    ensure(told.iter().all(|event| event.conversation != NOTES), || {
        format!("b was told of a's notes: {told:?}")
    })?;
    ensure(user.world.mock.notes(LAB_B).len() == b_notes, || {
        "b's notes changed".to_owned()
    })?;
    let row = user.shown(&id)?;
    ensure(row["files"].as_u64() == Some(3), || {
        format!("the note shows {} files", row["files"])
    })?;
    thread::sleep(SETTLE);
    user.shots("files-sent")
}

/// b's notes, which have older notes from another client: the first read of them fails, so the view says "Older
/// messages aren't synced yet"; "Load older" reads them. b's notes are b's own, and a note b writes stays out of a's.
fn history_b(user: &mut User<'_>) -> SandboxResult {
    let tail = notes_messages_path(user)?;
    say(&format!(
        "  b's notes reads before the switch: {}",
        notes_reads(user, LAB_B)
    ));
    user.switch("b")?;
    user.world
        .runtime
        .block_on(user.world.mock.refuse_next("GET", &tail, 500));
    user.open_notes()?;
    let state = user.until("b's notes unsynced", |state| state["chat"]["history"] == "Unsynced");
    say(&format!(
        "  b's notes reads after the open: {}",
        notes_reads(user, LAB_B)
    ));
    let state = state?;
    let label = state["chat"]["top_label"].as_str().unwrap_or_default().to_owned();
    say(&format!(
        "  b's first read failed: {label:?}, intro {}",
        state["chat"]["intro"]
    ));
    ensure(state["chat"]["intro"] == false, || {
        "the unsynced notes say \"This is your space\"".to_owned()
    })?;
    user.shot("b-unsynced-light")?;
    click_load_older(user)?;
    let older: Vec<String> = user
        .world
        .mock
        .notes(LAB_B)
        .iter()
        .filter_map(|note| note["id"].as_str().map(str::to_owned))
        .collect();
    let state = user.until("b's older notes after Load older", |state| {
        older.iter().all(|id| item(state, id).is_some())
    })?;
    say(&format!(
        "  b's notes after Load older: {:?}, history {}",
        item_ids(&state),
        state["chat"]["history"]
    ));
    ensure(item_ids(&state).iter().all(|id| older.contains(id)), || {
        format!("b's notes show other notes: {:?}", item_ids(&state))
    })?;
    user.shots("b-loaded")?;
    let a_notes = user.world.mock.notes(LAB_A).len();
    let mark = user.mark();
    user.click_box()?;
    user.type_text(&format!("Made-up note of b {mark}"))?;
    user.press("enter")?;
    let id = user.note_of(LAB_B, &mark)?;
    user.shown(&id)?;
    ensure(user.world.mock.notes(LAB_A).len() == a_notes, || {
        "b's note reached a's notes".to_owned()
    })?;
    user.switch("a")?;
    Ok(())
}

/// The reads of `mri`'s notes the mock got: their paths and queries.
fn notes_reads(user: &User<'_>, mri: &str) -> String {
    let token = user.world.mock.token(mri).unwrap_or_default();
    let reads: Vec<String> = user
        .world
        .requests()
        .into_iter()
        .filter(|seen| seen.method == "GET" && seen.path.contains(NOTES) && seen.token == token)
        .map(|seen| format!("{}?{}", seen.raw_path, seen.query))
        .collect();
    format!("{reads:?}")
}

/// The end of the notes' message path as the app sends it (`48:notes/messages`, its colon as the app writes it).
fn notes_messages_path(user: &User<'_>) -> SandboxResult<String> {
    let raw = user
        .world
        .requests()
        .into_iter()
        .find(|seen| seen.method == "GET" && seen.path.ends_with(&format!("/{NOTES}/messages")))
        .map(|seen| seen.raw_path)
        .ok_or("no read of the notes' messages yet")?;
    let at = raw.rfind("/48").ok_or("no 48 in the notes' path")?;
    Ok(raw[at..].to_owned())
}

/// Clicks the top row's "Load older" as a user does: the button is the last ink of the centred line, after its label.
fn click_load_older(user: &mut User<'_>) -> SandboxResult {
    let state = user.state()?;
    let rect = &state["chat"]["top_rect"];
    // an empty view draws its top row over the box, outside the list
    let (x0, y0, width, height) = if rect.is_array() {
        (px(&rect[0]), px(&rect[1]), px(&rect[2]), px(&rect[3]))
    } else {
        let compose = &state["compose"]["bounds"];
        ensure(compose.is_array(), || format!("no top row and no box: {state}"))?;
        (px(&compose[0]), px(&compose[1]).saturating_sub(48), px(&compose[2]), 40)
    };
    let shot = image::open(user.shot("b-top-row")?)?.to_rgb8();
    let middle = y0 + height / 2;
    let right = last_ink(&shot, x0, x0 + width, y0, y0 + height).ok_or("no ink in the top row")?;
    for back in [24_u32, 36, 14, 48] {
        user.click((right.saturating_sub(back), middle))?;
        user.park()?;
        thread::sleep(SETTLE);
        if user.state()?["chat"]["history"] != "Unsynced" {
            say(&format!("  Load older clicked at x {}", right.saturating_sub(back)));
            return Ok(());
        }
    }
    Err(format!("no click made \"Load older\" read the notes (last ink at x {right}, y {middle})").into())
}

/// The rightmost column in the box (`x0`..`x1`, `y0`..`y1`) with a pixel unlike the box's left edge.
fn last_ink(shot: &RgbImage, x0: u32, x1: u32, y0: u32, y1: u32) -> Option<u32> {
    let x1 = x1.min(shot.width());
    let y1 = y1.min(shot.height());
    let ground = shot.get_pixel(x0.min(shot.width() - 1), y0.min(shot.height() - 1)).0;
    (x0..x1).rev().find(|&x| {
        (y0..y1).any(|y| {
            let pixel = shot.get_pixel(x, y).0;
            pixel.iter().zip(ground).any(|(one, other)| one.abs_diff(other) > 40)
        })
    })
}

/// a's notes as they look now: Teams Light and Dark, a font of 20.
fn looks(user: &mut User<'_>) -> SandboxResult {
    user.open_notes()?;
    user.press("escape")?;
    user.shots("notes-all")?;
    user.ask("font 20")?;
    thread::sleep(SETTLE);
    user.shots("notes-font20")?;
    user.ask("font 14")?;
    Ok(())
}

// --- the checks: the restart and the read-only mode ---

/// Every note of a and of b shows again after a restart, and the note a's other client wrote after a's first note
/// shows too: the next sync read it.
fn after_restart(user: &mut User<'_>) -> SandboxResult {
    for alias in ["a", "b"] {
        let mri = if alias == "a" { LAB_A } else { LAB_B };
        user.switch(alias)?;
        user.open_notes()?;
        let ids: Vec<String> = user
            .world
            .mock
            .notes(mri)
            .iter()
            .filter_map(|note| note["id"].as_str().map(str::to_owned))
            .collect();
        let state = user.until(&format!("{alias}'s {} notes after the restart", ids.len()), |state| {
            ids.iter().all(|id| item(state, id).is_some())
        })?;
        say(&format!(
            "  {alias}'s notes after the restart: {} shown",
            item_ids(&state).len()
        ));
        ensure(item_ids(&state).iter().all(|id| ids.contains(id)), || {
            format!("{alias}'s notes show others: {:?}", item_ids(&state))
        })?;
        user.press("escape")?;
        user.shots(&format!("restart-{alias}"))?;
    }
    let elsewhere = user.note_with(A_ELSEWHERE)?;
    user.switch("a")?;
    user.open_notes()?;
    user.shown(&elsewhere)?;
    Ok(())
}

/// The app's read-only mode: the notes show, with no box, as every chat in that mode.
fn read_only_box(user: &mut User<'_>) -> SandboxResult {
    let state = user.open_notes()?;
    let compose = &state["compose"];
    say(&format!("  read-only notes: box {compose}"));
    ensure(compose["read_only"] == true, || {
        format!("the read-only app shows a box: {compose}")
    })?;
    let ids = user.world.mock.notes(LAB_A).len();
    user.until("the notes shown in the read-only app", |state| {
        item_ids(state).len() >= ids
    })?;
    user.shots("read-only")
}

// --- the child ---

struct Tokens {
    ic3: String,
    skypetoken: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.ic3.clone())))
    }

    fn bearer_rejected(&self, _audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.ic3.clone())))
    }

    fn skypetoken(&self) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        future::ready(Ok(Some(Secret::new(self.skypetoken.clone()))))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(AccessToken {
            token: Secret::new(self.ic3.clone()),
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

/// The engine of `alias` (a or b) on the mock: its chats, its media service, its OneDrive and its Trouter; no calls.
/// In the read-only mode its writes are refused before sending, as `ngteams --read-only` builds it.
fn engine(alias: Alias, tokens: (String, String), dir: &Path, read_only: bool) -> SandboxResult<EngineHandle> {
    let mri = if alias == Alias::A { LAB_A } else { LAB_B };
    let tokens = Arc::new(Tokens {
        ic3: tokens.0,
        skypetoken: tokens.1,
        renewals: broadcast::channel(4).0,
    });
    let origin = env(ORIGIN_ENV)?;
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
    // as the app's account builds them: a drive write checks it is the account's own
    let transport = http()?
        .for_account(Mri::new(mri))
        .with_ams(AMS_BASE)?
        .with_middle_tier(MIDDLE_TIER_BASE)?;
    let transport = Arc::new(if read_only { transport.read_only() } else { transport });
    let options = Options {
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(200),
        backoff_jitter_percent: 0,
        ..Options::default()
    };
    let trouter = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), &env(TROUTER_ENV)?)?
        .with_options(options);
    let name = display_name(&format!("bot-{alias}@lab.example.test")).to_owned();
    let client = http()?
        .for_account(Mri::new(mri))
        .with_ams(AMS_BASE)?
        .with_presence(PRESENCE_BASE)?
        .with_csa(CSA_BASE)?
        .with_user_profile(USER_PROFILE_BASE)?
        .with_provisioning(PROVISIONING_BASE)?
        .with_middle_tier(MIDDLE_TIER_BASE)?;
    let client = if read_only { client.read_only() } else { client };
    Ok(engine::start(Parts {
        calls: None,
        store: Store::open(&support::store_file(dir, alias))?,
        client: Arc::new(ChatClient::new(client)),
        outbox: Outbox::new(transport, Author::new(Mri::new(mri), name)),
        push: Some(Push::new(trouter)),
        options: EngineOptions::default(),
        known_names: [(LAB_A, "Lab A"), (LAB_B, "Lab B"), (LAB_C, "Lab C")]
            .into_iter()
            .filter(|(other, _)| *other != mri)
            .map(|(other, name)| (other.to_owned(), name.to_owned()))
            .collect(),
    })?)
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let dir = PathBuf::from(env(DIR_ENV)?);
    let tokens: Vec<String> = env(TOKENS_ENV)?.split(' ').map(str::to_owned).collect();
    let [ic3_a, skype_a, ic3_b, skype_b] = <[String; 4]>::try_from(tokens).map_err(|_| "four tokens expected")?;
    let read_only = env(READ_ONLY_ENV)? == "1";
    let first = support::started(Alias::A, engine(Alias::A, (ic3_a, skype_a), &dir, read_only)?)?;
    let second = support::started(Alias::B, engine(Alias::B, (ic3_b, skype_b), &dir, read_only)?)?;
    let runner = support::runner(&dir)?;
    let settings = teams_ui::SettingsFile::new(dir.join("settings.toml"));
    let mut opened = settings.load().settings;
    // the mock's made-up week starts a meeting at a fixed hour; its reminder card would cover the notes
    opened.meeting_reminders = false;
    settings.save(&opened).map_err(|problem| format!("{problem:?}"))?;
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: vec![second],
                trace: Arc::new(teams_ui::ViewTrace::off()),
                ctl: None,
                settings: teams_ui::SettingsModel::load(settings),
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only,
            },
        );
        match launched {
            Ok(launched) => {
                let window = launched.window;
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, 3, |_, _| say("ready"));
                });
                answer(window, launched.desktop, cx);
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
fn answer(window: WindowHandle<KitRoot>, desktop: Entity<Desktop>, cx: &mut App) {
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
            let reply = cx.update(|cx| on_app(window, &desktop, verb, cx));
            say(&format!("re {seq} {}", reply.replace('\n', " ")));
        }
    })
    .detach();
}

fn shell_of(window: WindowHandle<KitRoot>, cx: &mut App) -> Option<Entity<Shell>> {
    window
        .update(cx, |kit, _, cx| {
            kit.view()
                .clone()
                .downcast::<Root>()
                .ok()?
                .read(cx)
                .shell_in(cx)
                .cloned()
        })
        .ok()
        .flatten()
}

fn on_app(window: WindowHandle<KitRoot>, desktop: &Entity<Desktop>, verb: &str, cx: &mut App) -> String {
    if let Some(alias) = verb.strip_prefix("switch ") {
        return switch(desktop, alias, cx);
    }
    let Some(shell) = shell_of(window, cx) else {
        return "no shell".to_owned();
    };
    window
        .update(cx, |_, window, cx| {
            if verb == "state" {
                state_of(&shell, desktop, window, cx).to_string()
            } else {
                on_shell(&shell, verb, window, cx)
            }
        })
        .unwrap_or_else(|_| "no window".to_owned())
}

/// Shows the account whose sign-in name starts `bot-<alias>@`, as the rail's click does.
fn switch(desktop: &Entity<Desktop>, alias: &str, cx: &mut App) -> String {
    let upn = format!("bot-{alias}@");
    let key = desktop
        .read(cx)
        .accounts()
        .read(cx)
        .tiles()
        .iter()
        .find(|tile| tile.detail.starts_with(&upn))
        .map(|tile| tile.key.clone());
    let Some(key) = key else {
        return format!("no account {alias}");
    };
    desktop.update(cx, |desktop, cx| desktop.switch(&key, cx));
    "ok".to_owned()
}

fn on_shell(shell: &Entity<Shell>, verb: &str, window: &mut Window, cx: &mut App) -> String {
    if let Some(id) = verb.strip_prefix("open ") {
        shell.update(cx, |shell, cx| shell.open_chat(id.to_owned(), window, cx));
        return "ok".to_owned();
    }
    if let Some(size) = verb.strip_prefix("font ") {
        let Ok(size) = size.parse::<u16>() else {
            return format!("bad size {size}");
        };
        change_settings(shell, cx, |settings| settings.appearance.font_size = size);
        return "ok".to_owned();
    }
    let key = match verb {
        "theme-dark" => teams_ui::theme::DARK_KEY,
        "theme-light" => teams_ui::theme::LIGHT_KEY,
        other => return format!("unknown {other}"),
    };
    change_settings(shell, cx, |settings| {
        settings.appearance.follow_system = false;
        settings.appearance.theme = key;
    });
    "ok".to_owned()
}

fn change_settings(shell: &Entity<Shell>, cx: &mut App, change: impl FnOnce(&mut teams_ui::settings::Settings)) {
    let model = shell.read(cx).settings().clone();
    model.update(cx, |model, cx| model.change(change, cx));
}

fn state_of(shell: &Entity<Shell>, desktop: &Entity<Desktop>, window: &Window, cx: &mut App) -> Value {
    let dir = shell.read(cx).settings().read(cx).stickers_dir();
    let packs = shelf_state(&Shelf::of(dir, cx), cx);
    let shell_ref = shell.read(cx);
    let chat = shell_ref.chat().filter(|_| matches!(shell_ref.main(), Main::Chat(_)));
    let compose = chat.map(|chat| chat.read(cx).compose().clone());
    let picker = compose
        .as_ref()
        .and_then(|compose| compose.read(cx).emoji_picker().cloned());
    let favorites: Vec<Value> = shell_ref
        .list()
        .read(cx)
        .favorites()
        .iter()
        .map(|row| json!({"id": row.id, "name": row.name.to_string()}))
        .collect();
    json!({
        "main": match shell_ref.main() {
            Main::Chat(id) => format!("chat:{id}"),
            other => format!("{other:?}"),
        },
        "account": desktop.read(cx).accounts().read(cx).active().map(|tile| tile.detail.to_string()),
        "favorites": favorites,
        "chat": chat.map(|chat| chat_state(shell_ref, chat, cx)),
        "compose": compose.map(|compose| compose_state(&compose, window, cx)),
        "picker": picker.map(|picker| picker_state(&picker, window, cx)),
        "shelf": packs,
    })
}

fn chat_state(shell: &Shell, chat: &Entity<ChatView>, cx: &App) -> Value {
    let view = chat.read(cx);
    let list = view.list_state();
    let rect = |index: usize| {
        list.bounds_for_item(index)
            .map(|bounds| [bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height].map(f32::from))
    };
    let mut top_rect = None;
    let mut items = Vec::new();
    for (index, shown) in view.items().iter().enumerate() {
        match shown {
            Item::Top(_) => top_rect = rect(index),
            Item::Message(message) => items.push(json!({
                "id": message.id,
                "own": message.own,
                "kind": match &message.body {
                    Body::Text(_) => "text",
                    Body::Deleted => "deleted",
                    _ => "other",
                },
                "edited": message.edited,
                "reactions": message.reactions.iter().map(teams_ui::chat_view::ReactionCount::count).sum::<usize>(),
                "files": message.files.len(),
                "rect": rect(index),
            })),
            _ => {}
        }
    }
    let conversation = view.conversation();
    json!({
        "conversation": conversation,
        "history": format!("{:?}", view.history()),
        "top_label": view.history().label(),
        "intro": conversation == NOTES && view.history() == History::Complete && view.is_empty(),
        "can_call": shell.list().read(cx).can_call(conversation),
        "call_button": shell.call_button(cx),
        "selected": view.selected(),
        "options": view.options(),
        "top_rect": top_rect,
        "items": items,
    })
}

fn compose_state(compose: &Entity<Compose>, window: &Window, cx: &App) -> Value {
    let compose = compose.read(cx);
    let editor = compose.editor().read(cx);
    let pictures: Vec<Value> = editor
        .shown_pictures()
        .into_iter()
        .filter_map(|id| editor.picture(id))
        .map(|picture| {
            let state = match picture.state {
                PictureState::Sending => "sending",
                PictureState::Sent(_) => "sent",
                PictureState::Hosted(_) => "hosted",
                PictureState::Failed(_) => "failed",
            };
            json!({"sticker": picture.sticker.as_ref().map(ToString::to_string), "state": state})
        })
        .collect();
    let files: Vec<Value> = compose
        .attachments()
        .iter()
        .map(|file| {
            let state = match &file.state {
                AttachmentState::Uploading => "uploading",
                AttachmentState::Ready(_) => "ready",
                AttachmentState::Failed(_) => "failed",
            };
            let why = match &file.state {
                AttachmentState::Failed(why) => Some(why.to_string()),
                _ => None,
            };
            json!({"name": file.name.clone(), "state": state, "why": why})
        })
        .collect();
    json!({
        "text": compose.value(cx).to_string(),
        "mode": match compose.mode() {
            Mode::New => "new",
            Mode::Reply(_) => "reply",
            Mode::Edit { .. } => "edit",
        },
        "read_only": compose.is_read_only(),
        "accepts_files": compose.accepts_files(),
        "files": files,
        "pictures": pictures,
        "problem": compose.problem().map(ToString::to_string),
        "focused": compose.focused(window, cx),
        "bounds": compose.box_bounds().map(|bounds| {
            [bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height].map(f32::from)
        }),
    })
}

fn picker_state(picker: &Entity<FunPicker>, window: &Window, cx: &App) -> Value {
    let picker = picker.read(cx);
    let place = match picker.place(window, cx) {
        Some(Place::Search) => Some("search"),
        Some(Place::Tabs) => Some("tabs"),
        Some(Place::Items(..)) => Some("items"),
        None => None,
    };
    let rows: Vec<Vec<String>> = picker
        .rows(cx)
        .iter()
        .map(|row| row.iter().map(teams_ui::fun_picker::Item::selector).collect())
        .collect();
    json!({
        "tab": format!("{:?}", picker.tab()),
        "place": place,
        "current": picker.current(window, cx).map(|item| item.selector()),
        "rows": rows,
    })
}

fn shelf_state(shelf: &Entity<Shelf>, cx: &App) -> Value {
    let shelf = shelf.read(cx);
    let packs: Vec<Value> = shelf
        .packs()
        .iter()
        .map(|pack| {
            let stickers: Vec<Value> = pack
                .stickers
                .iter()
                .map(|sticker| json!({"file": sticker.file, "label": sticker.label}))
                .collect();
            json!({"id": pack.id, "name": pack.name, "stickers": stickers})
        })
        .collect();
    let import = shelf.import_state().map(|state| match state {
        ImportState::Running(progress) => json!({"running": true, "done_count": progress.done}),
        ImportState::Done(imported) => json!({"done": imported.pack.id}),
        ImportState::Failed(error) => json!({"failed": format!("{error:?}")}),
    });
    json!({"packs": packs, "import": import})
}
