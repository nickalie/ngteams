//! QA of task 525 (round 1), in the UI sandbox: an own server's message with several files shows Teams' grid of file
//! cards, and a user uses the cards (`harness = false`: GPUI runs on the process's main thread).
//!
//! The parent runs the real `ngteams-server` binary (TLS with a made-up certificate authority, `serve --role all` on
//! SQLite, files in its memory store) with the made-up accounts a and b. b is a client in the parent, brought up as
//! the app brings an own server's account up (`ngteams::signin::add_own`, `ngteams::engine::start`). b sends each file
//! through its engine's real upload path (`upload_file`, then a send that shares the files with the chat). a's app runs
//! as a child in the sandbox on the same server, with a control socket. The parent uses the app as a user does: it
//! clicks with the sandbox's pointer and finds the cards on the screen by their borders. It reads the links the
//! sandbox's URI opener got (it opens nothing), the sandbox's clipboard, the files in a's downloads folder, and the
//! views' rows from the child.
//!
//! - **Five files** in one message (a zip, two videos, a PDF and a picture): five cards, two in a row at most, 10 px
//!   apart, inside the message's bubble, never the line "This message has content that cannot be shown here". Teams
//!   Light and Teams Dark.
//! - **The actions**: Share on the videos alone; Share opens "Share this file" (task 543); "More options" (Teams' rows: "Open in desktop
//!   app", "Download", "Share", "Copy link"; a video's has no "Open"): "Download" saves the bytes b sent, "Copy link"
//!   puts the file's link on the clipboard, "Open in desktop app" saves the file and opens it; a click on a video's
//!   picture plays it in the card.
//! - **The location**: each card's second line names its owner by name (`personal > Bert Made-Up`), never by the
//!   account's id.
//! - **Text over two files** in a second message: its text and two cards in one row; "Download" of its video.
//! - **Shared on** (task 543): c (a third made-up account, a client in the parent) has a 1:1 chat with a and cannot
//!   read b's video. a picks c in "Share this file" with the keys and shares the video. Then c's engine reads all of
//!   the video's bytes, and a's chat with c shows the video's card.
//!
//! Every name, address and file is made up; nothing leaves loopback. Screenshots go to `target/ui-shots/qa525-own-*.png`,
//! and also to the directory `QA_FILE_GRID_OWN_EVIDENCE` names. Without weston 15, niri, `dbus-daemon`, `ffmpeg` or
//! the server binary (`NGTEAMS_QA_SERVER_BIN`, else `ngteams-server` beside the test's target directory) it says so
//! and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
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
use teams_proto::chat::{MessageText, RichText};
use teams_ui::chat_view::{Body, ChatView, MessageItem};
use teams_ui::rich_text::Block;
use teams_ui::shell::Shell;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
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

const CHILD_ENV: &str = "QA_FILE_GRID_OWN_CHILD";
const ORIGIN_ENV: &str = "QA_FILE_GRID_OWN_ORIGIN";
const CA_ENV: &str = "QA_FILE_GRID_OWN_CA";
const DIR_ENV: &str = "QA_FILE_GRID_OWN_DIR";
/// A directory that gets a copy of each screenshot and the server's log.
const EVIDENCE_ENV: &str = "QA_FILE_GRID_OWN_EVIDENCE";
/// The file in a's directory the parent asks the child about its views through.
const ASK: &str = "qa525-own-ask";
/// The file in a's directory the child reads the theme to show from (`light` or `dark`).
const THEME_FILE: &str = "theme";
/// The file in a's directory whose presence makes the app quit.
const QUIT_FILE: &str = "quit";
const STARTUP: Duration = Duration::from_secs(180);
const WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(100);
/// How long the screen gets to settle before a shot.
const SETTLE: Duration = Duration::from_millis(900);
/// The screen: tall, so that the five files' three rows fit in the chat at once.
const SCREEN: (u32, u32) = (1280, 1440);
/// niri's layout: each window takes the whole screen, and no hot corner.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";

/// A card's size and the grid's gap, as Teams draws them (`teams_ui::files`).
const CARD_WIDTH: u32 = 316;
/// The card of a message's only file: two columns wide, its 16:9 picture under the header (`teams_ui::files`).
const LONE_WIDTH: u32 = 2 * CARD_WIDTH;
const LONE_HEIGHT: u32 = 460;
const HEADER: u32 = 64;
const GAP: u32 = 10;

/// The largest box (width, height) "More options" of four rows takes, px.
const MENU_MOST: (u32, u32) = (240, 170);

/// The band at the bottom of the screen the compose box takes.
const COMPOSE_BAND: u32 = 90;

/// What the line under a message says when a part of it cannot be shown.
const UNSHOWN: &str = "This message has content that can't be shown here";

/// The people: user name, display name.
const A: (&str, &str) = ("ada@own.example.test", "Ada Made-Up");
const B: (&str, &str) = ("bert@own.example.test", "Bert Made-Up");
/// The account a shares b's file with (task 543).
const C: (&str, &str) = ("cara@own.example.test", "Cara Made-Up");

/// The text of the second message.
const NOTE: &str = "Made-up note over two files: the rota and the crowd clip.";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_file_grid_own_window: {error}");
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

fn note(finding: String) {
    step(&format!("FINDING: {finding}"));
    FINDINGS.lock().unwrap_or_else(PoisonError::into_inner).push(finding);
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn env(name: &str) -> SandboxResult<String> {
    Ok(std::env::var(name).map_err(|_| format!("{name} is not set"))?)
}

fn required() -> bool {
    std::env::var("GPUI_SMOKE_REQUIRE").as_deref() == Ok("1")
}

/// Stops the test: an error with `GPUI_SMOKE_REQUIRE=1`, else a skip that says why.
fn unavailable(why: &str) -> SandboxResult {
    if required() {
        return Err(why.into());
    }
    say(&format!("qa_file_grid_own_window: skipped: {why}"));
    Ok(())
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

// --- the parent ---

fn parent() -> SandboxResult {
    let Some(binary) = own_server_binary::binary()? else {
        return unavailable("no ngteams-server binary (NGTEAMS_QA_SERVER_BIN, or cargo build -p ngteams-server)");
    };
    if Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .status()
        .is_err()
    {
        return unavailable("no ffmpeg to make the made-up videos");
    }
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams own file grid", &options)? else {
        return Ok(());
    };
    let dir = tempfile::tempdir()?;
    let files = MadeFiles::make(&dir.path().join("made-up"))?;
    let server = BinaryServer::start(&binary)?;
    let a_oid = server.add_user(A.0, A.1)?;
    server.add_user(B.0, B.1)?;
    server.add_user(C.0, C.1)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let ca = server.ca_file.to_string_lossy().into_owned();
    let at = (server.origin.as_str(), ca.as_str());
    let (_, bert) = own_account(&dir.path().join("b"), at, B.0, false)?;
    let (_, cara) = own_account(&dir.path().join("c"), at, C.0, false)?;
    let outcome = walk(&sandbox, at, dir.path(), &files, ((&bert, &runtime, &a_oid), &cara));
    let log = server.log();
    for evidence in evidence_dirs() {
        let _ = fs::create_dir_all(&evidence);
        let _ = fs::write(evidence.join("qa525-own-server.log"), &log);
    }
    bert.handle.shutdown();
    cara.handle.shutdown();
    if let Err(error) = outcome {
        return Err(format!("{error}{}", sandbox.logs()).into());
    }
    let findings = std::mem::take(&mut *FINDINGS.lock().unwrap_or_else(PoisonError::into_inner));
    if !findings.is_empty() {
        return Err(format!("the walk ended with findings:\n- {}", findings.join("\n- ")).into());
    }
    say("ngteams own file grid: ok");
    Ok(())
}

/// b's engine, the parent's runtime, a's object id.
type Bert<'a> = (
    &'a AccountEngine<MemorySecretStore>,
    &'a tokio::runtime::Runtime,
    &'a str,
);

fn walk(
    sandbox: &Sandbox,
    server: (&str, &str),
    dir: &Path,
    files: &MadeFiles,
    (bert, cara): (Bert<'_>, &AccountEngine<MemorySecretStore>),
) -> SandboxResult {
    let downloads = dir.join("downloads");
    fs::create_dir_all(&downloads)?;
    let mut run = AppRun::launch(sandbox, server, &dir.join("a"), &downloads)?;
    let outcome = (|| -> SandboxResult {
        let (engine, runtime, a_oid) = bert;
        let mut ctx = Walk {
            run: &mut run,
            sandbox,
            files,
            downloads: &downloads,
            sender: Sender {
                engine,
                runtime,
                chat: String::new(),
            },
        };
        ctx.sender.chat_with(a_oid)?;
        let cara = Sender {
            engine: cara,
            runtime,
            chat: String::new(),
        };
        let mut cara = cara;
        cara.chat_with(a_oid)?;
        cara.say_hello()?;
        ctx.five()?;
        ctx.five_actions()?;
        ctx.text_and_two()?;
        ctx.share_on(&cara)
    })();
    if let Err(error) = outcome {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit()
}

/// b, as a client in the parent: its engine and the chat it has with a.
struct Sender<'a> {
    engine: &'a AccountEngine<MemorySecretStore>,
    runtime: &'a tokio::runtime::Runtime,
    chat: String,
}

impl Sender<'_> {
    /// b makes its 1:1 chat with a.
    fn chat_with(&mut self, a_oid: &str) -> SandboxResult {
        let made = self.runtime.block_on(self.engine.handle.write(Action::CreateChat {
            others: vec![Mri::new(format!("8:orgid:{a_oid}"))],
            topic: None,
        }))?;
        self.chat = made.message_id.ok_or("b's chat with a was not made")?;
        step(&format!("b made the chat {}", self.chat));
        Ok(())
    }

    /// A line in the chat, so a's chat list names the chat after its sender.
    fn say_hello(&self) -> SandboxResult {
        let row = self.runtime.block_on(self.engine.handle.write(Action::Send {
            conversation: self.chat.clone(),
            text: MessageText::plain("Made-up hello")?,
        }))?;
        if row.state != teams_store::OutboxState::Sent {
            return Err(format!("the hello is not sent: {:?}", row.state).into());
        }
        Ok(())
    }

    /// b uploads each of `files` as the compose box does, then sends them in one message with `text` (none: files
    /// alone, as the compose box sends them).
    fn send(&self, text: Option<&str>, files: &[&MadeFile]) -> SandboxResult {
        let mut uploaded = Vec::new();
        for file in files {
            let done = self
                .runtime
                .block_on(self.engine.handle.requests().upload_file(file.path.clone()))
                .map_err(|error| format!("b's upload of {}: {error:?}", file.name))?;
            uploaded.push(done);
        }
        let message = match text {
            Some(text) => MessageText::plain(text)?,
            None => MessageText::new(
                RichText {
                    nodes: Vec::new(),
                    truncated: false,
                },
                [],
            )?,
        };
        let row = self.runtime.block_on(self.engine.handle.write(Action::Send {
            conversation: self.chat.clone(),
            text: message.with_files(uploaded),
        }))?;
        if row.state != teams_store::OutboxState::Sent {
            return Err(format!("b's message with {} files is not sent: {:?}", files.len(), row.state).into());
        }
        step(&format!("b sent {} files", files.len()));
        Ok(())
    }
}

/// What one step of the walk works with.
struct Walk<'a> {
    run: &'a mut AppRun,
    sandbox: &'a Sandbox,
    files: &'a MadeFiles,
    downloads: &'a Path,
    sender: Sender<'a>,
}

/// What a message's grid must look like.
#[derive(Clone, Copy, Debug)]
struct Grid {
    cards: usize,
    rows: usize,
}

impl Walk<'_> {
    /// Opens b's chat in a's window and waits until a row holds `count` files; the row.
    fn message_with(&mut self, count: usize) -> SandboxResult<Item> {
        let chat = self.sender.chat.clone();
        self.run.open(&chat)?;
        let state = self.run.until(&format!("a message with {count} files"), |state| {
            state.chat == chat && state.items.iter().any(|item| item.files.len() == count)
        })?;
        let item = state
            .items
            .iter()
            .find(|item| item.files.len() == count)
            .cloned()
            .ok_or("no row")?;
        thread::sleep(SETTLE * 2);
        Ok(item)
    }

    /// The shot `qa525-own-<name>-<theme>.png` in `look`'s theme; the cards on it.
    fn shot_in(&mut self, name: &str, look: Look) -> SandboxResult<(PathBuf, Vec<Rect>)> {
        self.run.theme(look.dark)?;
        thread::sleep(SETTLE);
        let path = self
            .run
            .shot(self.sandbox, &format!("qa525-own-{name}-{}.png", look.name))?;
        Ok((path, cards_in(&self.run.last, look)))
    }

    /// Shots of `name` in Teams Light and Dark, with the grid of the last message on the screen checked in each; then
    /// Teams Light again. That message's cards in Teams Light.
    fn both_themes(&mut self, name: &str, grid: Grid) -> SandboxResult<Vec<Rect>> {
        let (dark_shot, dark) = self.shot_in(name, DARK)?;
        let dark = last_message(&dark);
        check_grid(&self.run.last, &dark, DARK, name, grid, &dark_shot);
        let (light_shot, light) = self.shot_in(name, LIGHT)?;
        let light = last_message(&light);
        check_grid(&self.run.last, &light, LIGHT, name, grid, &light_shot);
        Ok(light)
    }

    /// The cards of the last message on the screen now, in Teams Light.
    fn cards_now(&mut self, name: &str) -> SandboxResult<Vec<Rect>> {
        let (_, cards) = self.shot_in(name, LIGHT)?;
        Ok(last_message(&cards))
    }

    /// Checks the names a's row holds against what b sent, and that no part of the message is left unshown.
    fn check_item(item: &Item, sent: &[&MadeFile], what: &str) {
        if item.unshown {
            note(format!("{what}: the message shows \"{UNSHOWN}\" beside its cards"));
        }
        let names: Vec<&str> = sent.iter().map(|file| file.name).collect();
        if item.files != names {
            note(format!(
                "{what}: the message's files are {:?}, b sent {names:?}",
                item.files
            ));
        }
        if let Some(link) = item.links.iter().find(|link| !link.starts_with("https://127.0.0.1:")) {
            note(format!("{what}: a file's link {link:?} is not on the own server"));
        }
        // Teams names the owner on the second line (`personal > <owner>`), never an id
        let owner = B.1;
        if let Some(location) = item
            .locations
            .iter()
            .find(|location| **location != format!("personal > {owner}"))
        {
            note(format!(
                "{what}: a card's second line says {location:?}, not the name of its owner {owner:?} as Teams does"
            ));
        }
    }
}

// --- the five files ---

impl Walk<'_> {
    fn five(&mut self) -> SandboxResult {
        let sent = self.files.five();
        self.sender.send(None, &sent)?;
        let item = self.message_with(sent.len())?;
        Self::check_item(&item, &sent, "five");
        let grid = Grid { cards: 5, rows: 3 };
        self.both_themes("five", grid)?;
        step("five: 5 cards in 3 rows");
        Ok(())
    }

    /// On the message of five files: a zip, two videos, a PDF and a picture.
    fn five_actions(&mut self) -> SandboxResult {
        let item = self.message_with(5)?;
        let cards = self.cards_now("actions")?;
        let [zip, video, _, _, _] = cards.as_slice() else {
            return Err(format!("the five cards are not on the screen: {cards:?}").into());
        };
        let link = |at: usize| item.links.get(at).cloned().unwrap_or_default();
        self.share_slots(*zip, *video);
        self.share(*video)?;
        let [pack, _, _, plan, _] = self.files.five();
        self.download(0, pack, "the zip")?;
        self.copy_link(3, &link(3), "the PDF")?;
        self.open_in_app(3, plan)?;
        let cards = self.cards_now("before-play")?;
        if let Some(video) = cards.get(1) {
            self.play(*video)?;
        }
        Ok(())
    }

    /// The zip's card has no Share; a video's has one.
    fn share_slots(&mut self, zip: Rect, video: Rect) {
        let shot = &self.run.last;
        let (on_zip, on_video) = (ink_in(shot, share_slot(zip)), ink_in(shot, share_slot(video)));
        if on_zip > 10 {
            note(format!(
                "the zip's card shows something where Share is ({on_zip} pixels): Share on a zip?"
            ));
        }
        if on_video < 10 {
            note(format!("a video's card shows no Share ({on_video} pixels)"));
        }
        step(&format!(
            "Share's place: {on_zip} pixels on the zip, {on_video} on the video"
        ));
    }

    /// Share on a video's card opens "Share this file" (task 543); Escape closes it.
    fn share(&mut self, video: Rect) -> SandboxResult {
        let (x, y) = share_at(video);
        self.run.click(x, y)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, "qa525-own-share-dialog-light.png")?;
        if !dialog_shown(&self.run.last) {
            note("Share on a video's card opens no \"Share this file\"".to_owned());
            return Ok(());
        }
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, "qa525-own-share-dialog-closed-light.png")?;
        if dialog_shown(&self.run.last) {
            note("Escape leaves \"Share this file\" open".to_owned());
            self.run.input.key(keys::ESC)?;
            thread::sleep(SETTLE);
        }
        step("Share opens \"Share this file\"");
        Ok(())
    }

    /// "More options" on card `at` of the last message on the screen; the menu, or `None` (a finding) when none opens.
    fn more_menu(&mut self, at: usize, name: &str) -> SandboxResult<Option<Rect>> {
        let cards = self.cards_now(&format!("{name}-cards"))?;
        let Some(card) = cards.get(at) else {
            note(format!("{name}: no card {at} on the screen: {cards:?}"));
            return Ok(None);
        };
        let shot = format!("qa525-own-{name}-light.png");
        let mut menu = self.run.open_menu(self.sandbox, more_at(*card), &shot)?;
        // another change on the screen (the message's hover bar) can join the menu's box: then it is too large for a
        // menu of four rows, and the menu is opened again once the screen is still
        if menu.is_some_and(|menu| menu.height() > MENU_MOST.1 || menu.width() > MENU_MOST.0) {
            step(&format!(
                "{name}: the menu's box {menu:?} holds more than the menu: again"
            ));
            self.run.input.key(keys::ESC)?;
            self.run.input.move_to(self.run.rest.0, self.run.rest.1)?;
            thread::sleep(SETTLE);
            menu = self.run.open_menu(self.sandbox, more_at(*card), &shot)?;
        }
        if menu.is_none() {
            note(format!("{name}: More options on card {at} opens no menu"));
        }
        Ok(menu)
    }

    /// "More options" → "Download" on card `at`: a's downloads folder gets the bytes b sent.
    fn download(&mut self, at: usize, file: &MadeFile, what: &str) -> SandboxResult {
        let Some(menu) = self.more_menu(at, &format!("more-{}", file.slug))? else {
            return Ok(());
        };
        let (row, rows) = if is_video(file) { (0, 3) } else { (1, 4) };
        let (x, y) = row_at(menu, row, rows);
        self.run.click(x, y)?;
        self.run.input.move_to(self.run.rest.0, self.run.rest.1)?;
        let target = self.downloads.join(file.name);
        let saved = wait_until(WAIT, || {
            fs::read(&target).ok().filter(|bytes| bytes.len() == file.bytes.len())
        });
        thread::sleep(SETTLE);
        self.run
            .shot(self.sandbox, &format!("qa525-own-downloaded-{}-light.png", file.slug))?;
        match saved {
            None => note(format!(
                "Download of {what} saves no file of {} bytes at {} in {WAIT:?}; the folder holds {:?}",
                file.bytes.len(),
                target.display(),
                listing(self.downloads)
            )),
            Some(bytes) if bytes != file.bytes => note(format!(
                "Download of {what} saves {} bytes that differ from what b sent",
                bytes.len()
            )),
            Some(_) => step(&format!("Download of {what}: the bytes b sent")),
        }
        Ok(())
    }

    /// "More options" → "Copy link" on card `at`: the file's link on the clipboard.
    fn copy_link(&mut self, at: usize, link: &str, what: &str) -> SandboxResult {
        let Some(menu) = self.more_menu(at, "more-copy")? else {
            return Ok(());
        };
        // a document's rows: Open in desktop app, Download, Share, Copy link
        let copied = self.run.click_copies(self.sandbox, row_at(menu, 3, 4))?;
        if copied != link {
            note(format!(
                "More options' Copy link on {what} copies {copied:?}, expected {link}"
            ));
        }
        step(&format!("Copy link of {what}: {copied}"));
        Ok(())
    }

    /// "More options" → "Open in desktop app" on card `at`: the file saved with the bytes b sent, and its `file://`
    /// link given to the system's opener.
    fn open_in_app(&mut self, at: usize, file: &MadeFile) -> SandboxResult {
        let Some(menu) = self.more_menu(at, "more-open")? else {
            return Ok(());
        };
        let (x, y) = row_at(menu, 0, 4);
        let Some(opened) = self.run.click_opens(self.sandbox, x, y)? else {
            note(format!("Open in desktop app on {} opens nothing", file.name));
            return Ok(());
        };
        let Some(path) = opened.strip_prefix("file://").map(percent_decoded) else {
            note(format!("Open in desktop app opens {opened}, not a file:// link"));
            return Ok(());
        };
        match fs::read(&path) {
            Ok(bytes) if bytes == file.bytes => step(&format!("Open in desktop app opens {path}")),
            Ok(bytes) => note(format!(
                "Open in desktop app opens {path}: {} bytes that differ from what b sent",
                bytes.len()
            )),
            Err(error) => note(format!(
                "Open in desktop app opens {path}, which cannot be read: {error}"
            )),
        }
        if !path.ends_with(file.name) {
            note(format!(
                "Open in desktop app opens {path}, not a file named {}",
                file.name
            ));
        }
        Ok(())
    }

    /// A click on a video's picture plays it in the card: a frame of it shows there.
    fn play(&mut self, video: Rect) -> SandboxResult {
        let before = self.run.shot(self.sandbox, "qa525-own-video-before-light.png")?;
        let (x, y) = picture_at(video);
        self.run.click(x, y)?;
        let playing = self.run.players_until("a video in play", |states| {
            states
                .iter()
                .any(|state| state.contains("Playing") || state.contains("Failed"))
        });
        let after = self.run.shot(self.sandbox, "qa525-own-video-in-play-light.png")?;
        match playing {
            Ok(states) if states.iter().any(|state| state.contains("Playing")) => {
                let picture = Rect {
                    y0: video.y0 + HEADER + 4,
                    ..video
                };
                let moved = changed_in(&load(&before)?, &load(&after)?, picture);
                if moved < 500 {
                    note(format!(
                        "the video's picture shows no frame of it in play ({moved} pixels, {})",
                        after.display()
                    ));
                }
                step(&format!("a click plays the video in its card: {states:?}"));
            }
            Ok(states) => note(format!("a click on a video's picture: the player says {states:?}")),
            Err(error) => note(format!("a click on a video's picture plays nothing: {error}")),
        }
        // the made-up video is 3 s long: let it play to its end
        thread::sleep(Duration::from_secs(4));
        Ok(())
    }
}

// --- text over two files ---

impl Walk<'_> {
    fn text_and_two(&mut self) -> SandboxResult {
        let sent = self.files.two();
        self.sender.send(Some(NOTE), &sent)?;
        let item = self.message_with(sent.len())?;
        Self::check_item(&item, &sent, "text");
        if !item.text.contains(NOTE) {
            note(format!("text: the message's text is {:?}, b sent {NOTE:?}", item.text));
        }
        self.both_themes("text", Grid { cards: 2, rows: 1 })?;
        step("text: its text over 2 cards in 1 row");
        self.download(1, sent[1], "the second message's video")
    }
}

// --- shared on (task 543) ---

impl Walk<'_> {
    /// a shares b's video of the second message with c from "Share this file"; c had no access to it, and c's engine
    /// then reads all of its bytes, as c's app opens it.
    fn share_on(&mut self, cara: &Sender<'_>) -> SandboxResult {
        let crowd = &self.files.two()[1];
        self.run.open(&self.sender.chat)?;
        let state = self.run.until("the second message's two files", |state| {
            state.items.last().is_some_and(|item| item.files.len() == 2)
        })?;
        let link = state
            .items
            .last()
            .and_then(|item| item.links.get(1).cloned())
            .ok_or("no link of the video")?;
        let reads = cara.engine.handle.requests();
        if let Ok(size) = cara.runtime.block_on(reads.file_size(link.clone())) {
            note(format!(
                "c reads b's video ({size} bytes) before anyone shared it with c"
            ));
        }
        thread::sleep(SETTLE);
        let cards = last_message(&self.cards_now("share-on-cards")?);
        let Some(video) = cards.get(1).copied() else {
            note(format!("share on: no video's card on the screen: {cards:?}"));
            return Ok(());
        };
        let (x, y) = share_at(video);
        self.run.click(x, y)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, "qa543-own-share-dialog-light.png")?;
        if !dialog_shown(&self.run.last) {
            note("share on: Share on the video's card opens no \"Share this file\"".to_owned());
            return Ok(());
        }
        self.run.input.type_text("Cara")?;
        thread::sleep(Duration::from_millis(1200));
        self.run.shot(self.sandbox, "qa543-own-share-suggested-light.png")?;
        self.run.input.key(keys::DOWN)?;
        self.run.input.key(keys::ENTER)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, "qa543-own-share-picked-light.png")?;
        self.run.input.key(keys::ENTER)?;
        let toast = wait_until(WAIT, || {
            self.run
                .question("toast")
                .ok()
                .filter(|toast| toast.contains("You've shared"))
        });
        self.run.shot(self.sandbox, "qa543-own-shared-light.png")?;
        if toast.is_none() || dialog_shown(&self.run.last) {
            note(format!(
                "share on: the share to c did not finish (toast {toast:?}, dialog {})",
                dialog_shown(&self.run.last)
            ));
            return Ok(());
        }
        let read = wait_until(WAIT, || {
            let size = cara.runtime.block_on(reads.file_size(link.clone())).ok()?;
            let len = u32::try_from(size).ok()?;
            cara.runtime.block_on(reads.file_range(link.clone(), 0, len)).ok()
        });
        match read {
            None => note("share on: c cannot read b's video after a shared it with c".to_owned()),
            Some(range) if range.bytes != crowd.bytes => note(format!(
                "share on: c reads {} bytes of b's video that differ from what b sent",
                range.bytes.len()
            )),
            Some(_) => step("share on: c reads all of b's video after a shared it"),
        }
        self.run.open(&cara.chat)?;
        let shown = self.run.until("the shared video in a's chat with c", |state| {
            state
                .items
                .iter()
                .any(|item| item.files.iter().any(|name| name == crowd.name))
        });
        if let Err(error) = shown {
            note(format!("share on: {error}"));
        }
        self.check_owner_named(crowd.name);
        self.check_owner_known_to(cara);
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, "qa543-own-shown-light.png")?;
        Ok(())
    }

    /// c, who has no chat with b, gets b's name for the card of b's file (round 2): c's chat view names a file's
    /// owner by what c's engine finds about b.
    fn check_owner_known_to(&self, cara: &Sender<'_>) {
        let owner = self.sender.engine.handle.me().to_owned();
        let named = wait_until(WAIT, || {
            let person = cara.runtime.block_on(cara.engine.handle.person(owner.clone())).ok()?;
            (person.name.as_deref() == Some(B.1)).then_some(())
        });
        if named.is_some() {
            step(&format!("share on: c's engine names the owner {:?}", B.1));
        } else {
            let last = cara.runtime.block_on(cara.engine.handle.person(owner.clone()));
            note(format!(
                "share on: c's engine does not name b, the owner of the shared video: {last:?}"
            ));
        }
    }

    /// The card of `file` in the open chat names b as its owner: b sends nothing in that chat, and the card names b
    /// as it does in b's chat.
    fn check_owner_named(&mut self, file: &str) {
        let owner = format!("personal > {}", B.1);
        let named = self.run.until("the owner's name on the shared video's card", |state| {
            state.items.iter().any(|item| {
                item.files
                    .iter()
                    .zip(&item.locations)
                    .any(|(name, location)| name == file && *location == owner)
            })
        });
        if let Err(error) = named {
            note(format!("share on: {error}"));
        }
    }
}

/// Checks the cards found on `shot` against `grid`; each difference is a finding.
fn check_grid(shot: &image::RgbImage, cards: &[Rect], look: Look, what: &str, grid: Grid, path: &Path) {
    let what = format!("{what} ({}, {})", look.name, path.display());
    if cards.len() != grid.cards {
        note(format!(
            "{what}: {} cards on the screen, expected {}: {cards:?}",
            cards.len(),
            grid.cards
        ));
    }
    let rows = rows_of(cards);
    if rows.len() != grid.rows {
        note(format!("{what}: {} rows of cards, expected {}", rows.len(), grid.rows));
    }
    if let Some(row) = rows.iter().find(|row| row.len() > 2) {
        note(format!("{what}: {} cards in one row, at most 2 expected", row.len()));
    }
    check_sizes(cards, &what);
    check_gaps(&rows, &what);
    for card in cards {
        if !inside_bubble(shot, *card, look) {
            note(format!("{what}: the card {card:?} is not inside the message's bubble"));
        }
    }
    step(&format!(
        "{what}: cards {cards:?}, header {:?} px",
        cards.first().map(|card| header_height(shot, *card)),
    ));
}

/// Equal widths, no wider than Teams' width.
fn check_sizes(cards: &[Rect], what: &str) {
    let widths: Vec<u32> = cards.iter().map(Rect::width).collect();
    let (Some(least), Some(most)) = (widths.iter().min(), widths.iter().max()) else {
        return;
    };
    if most - least > 2 {
        note(format!("{what}: cards of unequal width {widths:?}"));
    }
    let teams = if cards.len() == 1 { LONE_WIDTH } else { CARD_WIDTH };
    if *most > teams + 2 {
        note(format!("{what}: a card {most} px wide, wider than Teams' {teams}"));
    }
}

/// [`GAP`] between two cards of a row, and between two rows.
fn check_gaps(rows: &[Vec<Rect>], what: &str) {
    for row in rows {
        for pair in row.windows(2) {
            let gap = pair[1].x0.saturating_sub(pair[0].x1);
            if gap.abs_diff(GAP) > 2 {
                note(format!("{what}: {gap} px between two cards of a row, expected {GAP}"));
            }
        }
    }
    for pair in rows.windows(2) {
        let Some(gap) = row_gap(&pair[0], &pair[1]) else {
            continue;
        };
        if gap.abs_diff(GAP) > 2 {
            note(format!("{what}: {gap} px between two rows of cards, expected {GAP}"));
        }
    }
}

/// The cards in rows, top down, each row left to right.
fn rows_of(cards: &[Rect]) -> Vec<Vec<Rect>> {
    let mut sorted = cards.to_vec();
    sorted.sort_by_key(|card| (card.y0, card.x0));
    let mut rows: Vec<Vec<Rect>> = Vec::new();
    for card in sorted {
        match rows.last_mut() {
            Some(row) if row.first().is_some_and(|first| first.y0.abs_diff(card.y0) <= 4) => row.push(card),
            _ => rows.push(vec![card]),
        }
    }
    rows
}

/// The space between the bottom of the row `upper`'s tallest card and the top of the row `lower`.
fn row_gap(upper: &[Rect], lower: &[Rect]) -> Option<u32> {
    let bottom = upper.iter().map(|card| card.y1).max()?;
    Some(lower.first()?.y0.saturating_sub(bottom))
}

/// The cards of the last message on the screen: the rows under the last space between two rows wider than the
/// grid's gap (the space between two messages).
fn last_message(cards: &[Rect]) -> Vec<Rect> {
    let rows = rows_of(cards);
    let start = rows
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| row_gap(&pair[0], &pair[1]).is_some_and(|gap| gap > GAP + 8))
        .map(|(at, _)| at + 1)
        .next_back()
        .unwrap_or(0);
    rows.into_iter().skip(start).flatten().collect()
}

/// The middle of a card's picture.
const fn picture_at(card: Rect) -> (u32, u32) {
    (
        card.x0 + card.width() / 2,
        card.y0 + HEADER + (card.height() - HEADER) / 2,
    )
}

/// "More options" (…) at the header's right end: a button of Teams' 27 px.
const fn more_at(card: Rect) -> (u32, u32) {
    (card.x1 - 25, card.y0 + HEADER / 2)
}

/// Whether "Share this file" is on the screen: its 600 px box's surface in the middle of the screen, over the dimmed
/// window (Teams Light).
fn dialog_shown(shot: &image::RgbImage) -> bool {
    let (x, y) = (SCREEN.0 / 2 - 295, SCREEN.1 / 2);
    let dimmed = at(shot, x - 10, y).iter().all(|&channel| channel < 200);
    near(at(shot, x, y), LIGHT.surface, 3) && dimmed
}

/// Share, left of "More options".
const fn share_at(card: Rect) -> (u32, u32) {
    (card.x1 - 56, card.y0 + HEADER / 2)
}

/// Where the top of Share's icon is drawn on a video's card: above the location's line.
const fn share_slot(card: Rect) -> Rect {
    Rect {
        x0: card.x1 - 65,
        y0: card.y0 + 22,
        x1: card.x1 - 47,
        y1: card.y0 + 31,
    }
}

/// Whether `file` is a video: its card's "More options" has no "Open".
fn is_video(file: &MadeFile) -> bool {
    std::path::Path::new(file.name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"))
}

/// The middle of row `row` of `rows` in a menu drawn as `menu`.
const fn row_at(menu: Rect, row: u32, rows: u32) -> (u32, u32) {
    (
        menu.x0 + menu.width() / 2,
        menu.y0 + menu.height() * (2 * row + 1) / (2 * rows),
    )
}

/// The names in `dir`, for a finding.
fn listing(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// `text` with each `%XX` turned back into its byte.
fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let pair = bytes
            .get(at + 1..at + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (bytes[at], pair) {
            (b'%', Some(byte)) => {
                out.push(byte);
                at += 3;
            }
            (byte, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// --- the screen ---

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Rect {
    const fn width(&self) -> u32 {
        self.x1 - self.x0
    }

    const fn height(&self) -> u32 {
        self.y1 - self.y0
    }

    const fn overlaps(&self, other: &Self) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }

    fn grown(&self, by: u32) -> Self {
        Self {
            x0: self.x0.saturating_sub(by),
            y0: self.y0.saturating_sub(by),
            x1: self.x1 + by,
            y1: self.y1 + by,
        }
    }
}

/// A theme's colours a card is found by: the bubble's surface and a card's border.
#[derive(Clone, Copy, Debug)]
struct Look {
    name: &'static str,
    dark: bool,
    surface: [u8; 3],
    border: [u8; 3],
}

const LIGHT: Look = Look {
    name: "light",
    dark: false,
    surface: [255, 255, 255],
    border: [224, 224, 224],
};

const DARK: Look = Look {
    name: "dark",
    dark: true,
    surface: [41, 41, 41],
    border: [61, 61, 61],
};

fn load(shot: &Path) -> SandboxResult<image::RgbImage> {
    Ok(image::open(shot)?.to_rgb8())
}

fn near(pixel: [u8; 3], wanted: [u8; 3], within: u8) -> bool {
    pixel.iter().zip(wanted).all(|(got, want)| got.abs_diff(want) <= within)
}

fn at(shot: &image::RgbImage, x: u32, y: u32) -> [u8; 3] {
    if x < shot.width() && y < shot.height() {
        shot.get_pixel(x, y).0
    } else {
        [0, 0, 0]
    }
}

/// The file cards on the screen: a border round a box of a card's size over the bubble's surface, found by its top
/// edge (the surface above it), its left and right edges, and its bottom edge (the surface under it).
fn cards_in(shot: &image::RgbImage, look: Look) -> Vec<Rect> {
    let mut found: Vec<Rect> = Vec::new();
    for y in 1..shot.height().saturating_sub(HEADER) {
        for (from, to) in runs(shot, y, 0, shot.width(), |pixel| near(pixel, look.border, 2)) {
            if to - from < 100 {
                continue;
            }
            let Some(card) = card_from(shot, look, y, (from, to)) else {
                continue;
            };
            if (150..=LONE_WIDTH + 4).contains(&card.width())
                && (200..=LONE_HEIGHT).contains(&card.height())
                && !found.iter().any(|known| known.overlaps(&card))
            {
                found.push(card);
            }
        }
    }
    found.sort_by_key(|card| (card.y0, card.x0));
    found
}

/// The card whose top edge is the run `from`..`to` of row `y`, when it is one.
fn card_from(shot: &image::RgbImage, look: Look, y: u32, (from, to): (u32, u32)) -> Option<Rect> {
    let mid = from + (to - from) / 2;
    let below = at(shot, mid, y + 1);
    if !near(at(shot, mid, y - 1), look.surface, 3) || near(below, look.surface, 3) || near(below, look.border, 2) {
        return None;
    }
    let probe = y + 12;
    let left = (from.saturating_sub(12)..=from).find(|&x| near(at(shot, x, probe), look.border, 2))?;
    let right = (to.saturating_sub(1)..to + 12)
        .rev()
        .find(|&x| near(at(shot, x, probe), look.border, 2))?;
    let mut down = probe;
    while down < shot.height() && near(at(shot, left, down), look.border, 2) {
        down += 1;
    }
    let bottom = (down.saturating_sub(2)..(down + 12).min(shot.height() - 1))
        .find(|&row| near(at(shot, mid, row), look.border, 2) && near(at(shot, mid, row + 1), look.surface, 3))?;
    Some(Rect {
        x0: left,
        y0: y,
        x1: right + 1,
        y1: bottom + 1,
    })
}

/// The runs of row `y` from `from` to `to` whose pixels `taken` takes.
fn runs(shot: &image::RgbImage, y: u32, from: u32, to: u32, taken: impl Fn([u8; 3]) -> bool) -> Vec<(u32, u32)> {
    let mut found = Vec::new();
    let mut x = from;
    let to = to.min(shot.width());
    while x < to {
        if !taken(at(shot, x, y)) {
            x += 1;
            continue;
        }
        let start = x;
        while x < to && taken(at(shot, x, y)) {
            x += 1;
        }
        found.push((start, x));
    }
    found
}

/// Whether the bubble's surface is on both sides of `card`, by its header.
fn inside_bubble(shot: &image::RgbImage, card: Rect, look: Look) -> bool {
    let y = card.y0 + HEADER / 2;
    near(at(shot, card.x0.saturating_sub(4), y), look.surface, 4) && near(at(shot, card.x1 + 3, y), look.surface, 4)
}

/// The header's height: from the card's top edge down to where its fill changes.
fn header_height(shot: &image::RgbImage, card: Rect) -> u32 {
    let x = card.x0 + 6;
    let fill = at(shot, x, card.y0 + 10);
    (card.y0 + 10..card.y1)
        .find(|&y| !near(at(shot, x, y), fill, 4))
        .map_or(0, |y| y - card.y0)
}

/// The pixels in `region` that differ from its first one: an icon's or a word's.
fn ink_in(shot: &image::RgbImage, region: Rect) -> usize {
    let fill = at(shot, region.x0, region.y0);
    (region.y0..region.y1.min(shot.height()))
        .flat_map(|y| (region.x0..region.x1.min(shot.width())).map(move |x| (x, y)))
        .filter(|&(x, y)| !near(at(shot, x, y), fill, 40))
        .count()
}

fn changed_in(before: &image::RgbImage, after: &image::RgbImage, region: Rect) -> usize {
    (region.y0..region.y1.min(before.height()))
        .flat_map(|y| (region.x0..region.x1.min(before.width())).map(move |x| (x, y)))
        .filter(|&(x, y)| !near(at(before, x, y), at(after, x, y), 8))
        .count()
}

/// The bounds of the pixels that changed in `within`, but in `except`.
fn changed_box(before: &image::RgbImage, after: &image::RgbImage, within: Rect, except: Rect) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    for y in within.y0..within.y1.min(before.height()).min(after.height()) {
        for x in within.x0..within.x1.min(before.width()).min(after.width()) {
            let skipped = except.x0 <= x && x < except.x1 && except.y0 <= y && y < except.y1;
            if skipped || near(at(before, x, y), at(after, x, y), 8) {
                continue;
            }
            bounds = Some(bounds.map_or(
                Rect {
                    x0: x,
                    y0: y,
                    x1: x + 1,
                    y1: y + 1,
                },
                |known| Rect {
                    x0: known.x0.min(x),
                    y0: known.y0.min(y),
                    x1: known.x1.max(x + 1),
                    y1: known.y1.max(y + 1),
                },
            ));
        }
    }
    bounds
}

// --- the made-up files ---

/// A made-up file b sends: its name, a short name for the shots, where it is, and its bytes.
struct MadeFile {
    name: &'static str,
    slug: &'static str,
    path: PathBuf,
    bytes: Vec<u8>,
}

/// The files b sends: five in the first message, two in the second.
struct MadeFiles {
    pack: MadeFile,
    walk: MadeFile,
    sound: MadeFile,
    plan: MadeFile,
    photo: MadeFile,
    rota: MadeFile,
    crowd: MadeFile,
}

impl MadeFiles {
    /// Makes each file in `dir`: the videos with `ffmpeg`, the others here.
    fn make(dir: &Path) -> SandboxResult<Self> {
        fs::create_dir_all(dir)?;
        let made = |name: &'static str, slug: &'static str, bytes: Vec<u8>| -> SandboxResult<MadeFile> {
            let path = dir.join(name);
            fs::write(&path, &bytes)?;
            Ok(MadeFile {
                name,
                slug,
                path,
                bytes,
            })
        };
        let video = |name: &'static str, slug: &'static str, source: &str| -> SandboxResult<MadeFile> {
            made(name, slug, made_up_video(dir, source)?)
        };
        Ok(Self {
            pack: made("made-up-pack.zip", "zip", made_up_zip()?)?,
            walk: video("01-made-up-stage-walk.mp4", "walk", "testsrc")?,
            sound: video("02-made-up-sound-check.mp4", "sound", "testsrc2")?,
            plan: made("made-up-plan.pdf", "pdf", MADE_UP_PDF.as_bytes().to_vec())?,
            photo: made("made-up-floor-photo.png", "png", made_up_png()?)?,
            rota: made(
                "made-up-rota.txt",
                "txt",
                b"Made-up rota: Mon Ada, Tue Bert, Wed Ada.\n".to_vec(),
            )?,
            crowd: video("03-made-up-crowd.mp4", "crowd", "smptebars")?,
        })
    }

    fn five(&self) -> [&MadeFile; 5] {
        [&self.pack, &self.walk, &self.sound, &self.plan, &self.photo]
    }

    fn two(&self) -> [&MadeFile; 2] {
        [&self.rota, &self.crowd]
    }
}

/// A made-up PDF of one empty page.
const MADE_UP_PDF: &str = "%PDF-1.4\n% made-up test file\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n\
     2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]>>endobj\n\
     trailer<</Root 1 0 R>>\n%%EOF\n";

/// A made-up video of 3 s, 640 by 360, from `ffmpeg`'s test `source`, made in `dir`; its bytes.
fn made_up_video(dir: &Path, source: &str) -> SandboxResult<Vec<u8>> {
    let out = dir.join(format!("{source}.mp4"));
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("{source}=duration=3:size=640x360:rate=25"))
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-movflags", "+faststart"])
        .arg(&out)
        .stdin(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(format!("ffmpeg made no {source} video: {status}").into());
    }
    let bytes = fs::read(&out)?;
    fs::remove_file(&out)?;
    Ok(bytes)
}

/// A made-up picture, 640 by 360: a gradient with a dark bar.
fn made_up_png() -> SandboxResult<Vec<u8>> {
    let picture = image::RgbImage::from_fn(640, 360, |x, y| {
        if (280..360).contains(&x) && y > 100 {
            image::Rgb([20, 20, 20])
        } else {
            let red = u8::try_from(x * 255 / 640).unwrap_or(u8::MAX);
            let green = u8::try_from(y * 255 / 360).unwrap_or(u8::MAX);
            image::Rgb([red, green, 160])
        }
    });
    let mut png = Vec::new();
    picture.write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    Ok(png)
}

/// A made-up zip with one text file, stored (no compression).
fn made_up_zip() -> SandboxResult<Vec<u8>> {
    let name = b"made-up-readme.txt";
    let data = b"Made-up pack for the QA of task 525.\n";
    let crc = crc32(data);
    let size = u32::try_from(data.len())?;
    let name_len = u16::try_from(name.len())?;
    let mut zip = Vec::new();
    let header = |zip: &mut Vec<u8>, signature: u32| {
        zip.extend_from_slice(&signature.to_le_bytes());
        // version, flags, method (stored), time, date (1 January 1980)
        for half in [20_u16, 0, 0, 0, 0x21] {
            zip.extend_from_slice(&half.to_le_bytes());
        }
        for word in [crc, size, size] {
            zip.extend_from_slice(&word.to_le_bytes());
        }
    };
    header(&mut zip, 0x0403_4b50);
    zip.extend_from_slice(&name_len.to_le_bytes());
    zip.extend_from_slice(&0_u16.to_le_bytes());
    zip.extend_from_slice(name);
    zip.extend_from_slice(data);
    let central = u32::try_from(zip.len())?;
    zip.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
    zip.extend_from_slice(&20_u16.to_le_bytes());
    let mut entry = Vec::new();
    header(&mut entry, 0);
    zip.extend_from_slice(&entry[4..]);
    // name, extra, comment, disk, internal attributes; external attributes, offset of the local header
    for half in [name_len, 0, 0, 0, 0] {
        zip.extend_from_slice(&half.to_le_bytes());
    }
    zip.extend_from_slice(&0_u32.to_le_bytes());
    zip.extend_from_slice(&0_u32.to_le_bytes());
    zip.extend_from_slice(name);
    let central_size = u32::try_from(zip.len())? - central;
    zip.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    for half in [0_u16, 0, 1, 1] {
        zip.extend_from_slice(&half.to_le_bytes());
    }
    zip.extend_from_slice(&central_size.to_le_bytes());
    zip.extend_from_slice(&central.to_le_bytes());
    zip.extend_from_slice(&0_u16.to_le_bytes());
    Ok(zip)
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

// --- what the app shows ---

/// A message's row as the child tells it.
#[derive(Clone, Debug, serde::Deserialize)]
struct Item {
    text: String,
    files: Vec<String>,
    /// Each file's link, as its card opens it and copies it.
    links: Vec<String>,
    /// Each file's location, as its card's second line shows it.
    locations: Vec<String>,
    unshown: bool,
}

/// The open chat's rows as the child tells them.
#[derive(Debug, serde::Deserialize)]
struct State {
    chat: String,
    items: Vec<Item>,
}

/// The app in the sandbox: its process, its control socket and the user's pointer.
struct AppRun {
    running: Running,
    ctl: PathBuf,
    dir: PathBuf,
    input: VirtualInput,
    /// Where the pointer rests between clicks: on the empty title bar, away from every card and button.
    rest: (u32, u32),
    asked: u32,
    /// The last screenshot.
    last: image::RgbImage,
}

impl AppRun {
    /// Starts a's app on the server with its data in `dir` and its downloads folder `downloads`; once its window is
    /// drawn.
    fn launch(sandbox: &Sandbox, (origin, ca): (&str, &str), dir: &Path, downloads: &Path) -> SandboxResult<Self> {
        fs::create_dir_all(dir)?;
        let dir_text = dir.to_string_lossy().into_owned();
        let downloads_text = downloads.to_string_lossy().into_owned();
        let child = sandbox.spawn(
            &std::env::current_exe()?,
            &[],
            &[
                (CHILD_ENV, "app"),
                (ORIGIN_ENV, origin),
                (CA_ENV, ca),
                (DIR_ENV, &dir_text),
                ("XDG_DOWNLOAD_DIR", &downloads_text),
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
            rest: (200, 24),
            asked: 0,
            last: image::RgbImage::new(1, 1),
        };
        run.input.move_to(run.rest.0, run.rest.1)?;
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
            if self.state()?.chat == chat {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("a's window never shows the chat {chat}").into());
            }
        }
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

    /// The rows of the open chat.
    fn state(&mut self) -> SandboxResult<State> {
        let answer = self.question("state")?;
        serde_json::from_str(&answer).map_err(|error| format!("the state {answer}: {error}").into())
    }

    /// Waits up to [`WAIT`] for a state that `wanted` takes, named `what` in the error.
    fn until(&mut self, what: &str, wanted: impl Fn(&State) -> bool) -> SandboxResult<State> {
        let start = Instant::now();
        loop {
            let state = self.state()?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {state:#?}").into());
            }
            thread::sleep(POLL);
        }
    }

    /// Waits up to [`WAIT`] for the video players' states that `wanted` takes; the states.
    fn players_until(&mut self, what: &str, wanted: impl Fn(&[String]) -> bool) -> SandboxResult<Vec<String>> {
        let start = Instant::now();
        loop {
            let answer = self.question("players")?;
            let states: Vec<String> = serde_json::from_str(&answer)?;
            if wanted(&states) {
                return Ok(states);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; the players say {states:?}").into());
            }
            thread::sleep(Duration::from_millis(200));
        }
    }

    /// Shows the app in Teams Light or Dark (`dark`), as the user picks it in Settings; once it is applied.
    fn theme(&mut self, dark: bool) -> SandboxResult {
        let key = if dark { "dark" } else { "light" };
        if fs::read_to_string(self.dir.join(THEME_FILE)).is_ok_and(|now| now == key) {
            return Ok(());
        }
        fs::write(self.dir.join(THEME_FILE), key)?;
        let applied = format!("theme {key}");
        self.running
            .line_where(WAIT, |line| line == applied)
            .map_err(|error| format!("{applied} never applied: {error:?}"))?;
        Ok(())
    }

    /// A screenshot of the screen, kept as `target/ui-shots/<name>` (and in the evidence directory); the sandbox's copy.
    fn shot(&mut self, sandbox: &Sandbox, name: &str) -> SandboxResult<PathBuf> {
        self.input.move_to(self.rest.0, self.rest.1)?;
        thread::sleep(Duration::from_millis(600));
        let png = sandbox.screenshot(name, false)?;
        for dir in evidence_dirs() {
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(name))?;
        }
        self.last = load(&png)?;
        Ok(png)
    }

    /// Clicks (`x`, `y`) as a user does.
    fn click(&mut self, x: u32, y: u32) -> SandboxResult {
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(Duration::from_millis(250));
        Ok(())
    }

    /// Clicks (`x`, `y`) as a user does; the link the app asked the opener for, if one in [`WAIT`].
    fn click_opens(&mut self, sandbox: &Sandbox, x: u32, y: u32) -> SandboxResult<Option<String>> {
        let before = sandbox.opened()?.len();
        self.input.click(x, y, keys::BTN_LEFT)?;
        let opened = wait_until(WAIT, || sandbox.opened().ok()?.get(before).cloned());
        self.input.move_to(self.rest.0, self.rest.1)?;
        thread::sleep(Duration::from_millis(300));
        Ok(opened.and_then(|opened| opened.args.last().cloned()))
    }

    /// Clicks the menu's row at (`x`, `y`); what the clipboard holds then.
    fn click_copies(&mut self, sandbox: &Sandbox, (x, y): (u32, u32)) -> SandboxResult<String> {
        sandbox.set_clipboard(vec![(
            "text/plain;charset=utf-8".to_owned(),
            b"made-up before".to_vec(),
        )])?;
        thread::sleep(Duration::from_millis(300));
        self.click(x, y)?;
        let copied = wait_until(Duration::from_secs(5), || {
            clipboard_text(sandbox).ok().filter(|text| text != "made-up before")
        });
        self.input.move_to(self.rest.0, self.rest.1)?;
        Ok(copied.unwrap_or_else(|| "(the clipboard did not change)".to_owned()))
    }

    /// Clicks the button at `button` and finds the menu it opens on the screen (the shot kept as `name`); `None` when
    /// none opened.
    fn open_menu(&mut self, sandbox: &Sandbox, button: (u32, u32), name: &str) -> SandboxResult<Option<Rect>> {
        let before = load(&self.shot(sandbox, &name.replace(".png", "-before.png"))?)?;
        self.click(button.0, button.1)?;
        let after = load(&self.shot(sandbox, name)?)?;
        let around = Rect {
            x0: button.0,
            y0: button.1,
            x1: button.0 + 1,
            y1: button.1 + 1,
        }
        .grown(20);
        // the click also takes the focus from the compose box, and the pointer's rest on the title bar can show a
        // tooltip: only the changes near the button are the menu
        let near_button = Rect {
            x0: button.0.saturating_sub(300),
            y0: button.1.saturating_sub(200),
            x1: button.0 + 300,
            y1: (button.1 + 200).min(SCREEN.1 - COMPOSE_BAND),
        };
        let menu =
            changed_box(&before, &after, near_button, around).filter(|menu| menu.height() > 20 && menu.width() > 60);
        step(&format!("{name}: the menu at {menu:?}"));
        Ok(menu)
    }

    /// The error, with the last screen and the app's stderr.
    fn fail(&mut self, sandbox: &Sandbox, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
        let shot = self
            .shot(sandbox, "qa525-own-failed.png")
            .map_or_else(|_| String::new(), |path| path.display().to_string());
        self.running.fail(&format!("{error} (last screen: {shot})"))
    }

    /// Quits as the tray's Quit does; once the app ended.
    fn quit(mut self) -> SandboxResult {
        fs::write(self.dir.join(QUIT_FILE), "")?;
        self.running
            .line_where(WAIT, |line| line == "quit")
            .map_err(|error| self.running.fail(&format!("the app did not quit: {error:?}")))?;
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("the app exited {status}: {stderr}").into());
        }
        Ok(())
    }
}

/// The text the sandbox's clipboard holds.
fn clipboard_text(sandbox: &Sandbox) -> SandboxResult<String> {
    let offers = sandbox.read_clipboard()?;
    offers
        .iter()
        .find(|(mime, _)| mime.starts_with("text/plain"))
        .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
        .ok_or_else(|| "no text on the clipboard".into())
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
                let settings = launched.desktop.read(cx).settings().clone();
                follow_theme_file(&settings, dir.join(THEME_FILE), cx);
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

/// Runs `done` once `frames` more frames of `window` are presented.
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

/// Shows the theme `file` names (`light` or `dark`) once it changes, as the user picks one in Settings, and says
/// `theme <name>` once it is applied.
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
            // the frames in the new theme
            cx.background_executor().timer(Duration::from_millis(300)).await;
            say(&format!("theme {wanted}"));
            applied = wanted;
        }
    })
    .detach();
}

/// Once the parent writes `trigger`: the app quits.
fn quit_when_asked(trigger: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        while !trigger.exists() {
            cx.background_executor().timer(Duration::from_millis(100)).await;
        }
        cx.update(|cx| {
            app::quit(cx);
        });
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
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn on_shell(shell: &Entity<Shell>, verb: &str, cx: &App) -> String {
    match verb {
        "state" => shell
            .read(cx)
            .chat()
            .map_or_else(|| json!({"chat": "", "items": []}), |view| state_of(view.read(cx))),
        "toast" => json!(shell.read(cx).toast().map(ToString::to_string).unwrap_or_default()),
        "players" => {
            let states: Vec<String> = teams_ui::video_player::players(cx)
                .iter()
                .map(|player| format!("{:?}", player.read(cx).state()))
                .collect();
            json!(states)
        }
        other => json!(format!("unknown {other}")),
    }
    .to_string()
}

/// The view's rows, as [`State`] reads them.
fn state_of(view: &ChatView) -> Value {
    let items: Vec<Value> = view.message_items().map(item_of).collect();
    json!({ "chat": view.conversation(), "items": items })
}

fn item_of(item: &MessageItem) -> Value {
    let text = match &item.body {
        Body::Text(rendered) => rendered
            .0
            .blocks
            .iter()
            .flat_map(Block::plain_lines)
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let files: Vec<&str> = item.files.iter().map(|file| file.name.as_str()).collect();
    let locations: Vec<&str> = item
        .files
        .iter()
        .map(|file| file.location.as_deref().unwrap_or_default())
        .collect();
    let links: Vec<&str> = item
        .files
        .iter()
        .map(|file| file.open_link().unwrap_or_default())
        .collect();
    json!({ "text": text, "files": files, "links": links, "locations": locations, "unshown": item.unshown.any() })
}
