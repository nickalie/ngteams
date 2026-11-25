//! QA of task 653 (round 1), in the UI sandbox: the compose box's picker with its tabs All, Emoji and Stickers, the
//! sticker packs the user imports, and the stickers a sends and b sees. The real app runs as a child in the sandbox,
//! with lab accounts a and b in one window on `teams-mock` (its chat service, its media service and its Trouter). The
//! parent drives it with the sandbox's virtual pointer and keyboard. A fake file chooser on the sandbox's bus gives the
//! folder or the `.zip` file that the user picks. The parent finds each sticker in the screenshots by the colour of
//! its face, as a user's eye does, and asks the child what the views hold through a file in the sandbox dir
//! (`harness = false`: GPUI runs on the process's main thread).
//!
//! - **Open**: the emoji button and Ctrl+. open the picker on All, with the search field focused; its tabs; Escape
//!   and the button close it; the pointer over the picker does not hover the messages under it.
//! - **Import**: a folder of a still PNG, an APNG, a still WebP, an animated WebP, an animated GIF, slow GIFs and bad
//!   files (a broken file, a note, a file over 10 MB, a picture over 4096 px, too many frames, a hidden file and a
//!   Mac's extras); a `.zip` file; the progress line, the summary and its reasons, Show and Close.
//! - **All**: six emoji and four stickers, each with "See all".
//! - **Pick and send**: a sticker by the pointer and by the keyboard goes in the box, Send posts it as a sticker, and
//!   Recent fills. A sticker beside typed text goes in the same message.
//! - **b sees** the stickers with no bubble, the animated one plays, and b's picker has the same packs.
//! - **Search** by a label and by a pack's name; **the keyboard** through the tabs, the items, Enter and Escape.
//! - **Rename and remove** a pack, with the confirmation; Recent loses the removed pack's sticker.
//! - **New message** has no Stickers tab; **sizes**: Teams Light and Dark, font 14 to 24, 1280, 800 and 520 px.
//! - **A restart** keeps the packs and Recent for a and b; **the read-only app** offers no emoji button.
//! - **Teams' own packs** (task 654, `qa_sticker_picker_teams.rs`): All with no pack, the packs after the user's
//!   with their pictures, a pack in 3 columns, a pick by the pointer and by the keyboard, b sees it, a PNG sticker
//!   from another client, the search, a sticker that cannot be read, Recent after a restart, and a fourth run as an
//!   own-server account.
//! - **Edit sticker** (task 650, `qa_sticker_picker_captions.rs`): the pencil marks, the editor opened by a click,
//!   a typed caption that redraws the picture, Done, Send and what b sees; a long and an empty caption; a meme; the
//!   keyboard, Cancel and back.
//!
//! The checks run in order and build on each other (the imports first). `QA653_ONLY` (a comma list of check names)
//! runs only those. `QA653_EVIDENCE` names a directory that gets the screenshots and the results too. Screenshots also
//! go to `target/ui-shots/qa653-*.png`. Every name, id, text and picture is made up.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use std::{fs, future, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use image::RgbImage;
use ngteams::app::{self, Launch};
use ngteams::descriptors::adopt_trace;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_mock::{
    AB_CHAT, AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE, PRESENCE_BASE,
    PROVISIONING_BASE, USER_PROFILE_BASE,
};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::chat_view::{Body, ChatView};
use teams_ui::compose::Compose;
use teams_ui::desktop::Desktop;
use teams_ui::editor::pictures::PictureState;
use teams_ui::fun_picker::{FunPicker, Item, Place};
use teams_ui::rich_text::Block;
use teams_ui::shell::{Main, Shell};
use teams_ui::stickers::{ImportState, Shelf};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "../window.rs"]
mod support;
use support::{Alias, display_name};

#[path = "qa_account_picture_portal.rs"]
#[expect(
    dead_code,
    reason = "the account picture QA's portal module also holds that QA's own pictures"
)]
mod portal;
use portal::{Answer, Chooser};

#[path = "qa_sticker_picker_files.rs"]
mod files;
use files::{Rgb, Sources};

#[path = "qa_sticker_picker_teams.rs"]
mod teams;

#[path = "qa_sticker_picker_captions.rs"]
mod captions;

const CHILD_ENV: &str = "QA653_CHILD";
const ONLY_ENV: &str = "QA653_ONLY";
const EVIDENCE_ENV: &str = "QA653_EVIDENCE";
const READ_ONLY_ENV: &str = "QA653_READ_ONLY";
const OWN_SERVER_ENV: &str = "QA653_OWN_SERVER";
const TRACE_FD_ENV: &str = "QA653_TRACE_FD";
const ORIGIN_ENV: &str = "QA653_ORIGIN";
const TROUTER_ENV: &str = "QA653_TROUTER";
const DIR_ENV: &str = "QA653_DIR";
const CTL_ENV: &str = "QA653_CTL";
const TOKENS_ENV: &str = "QA653_TOKENS";
/// The file in the sandbox dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "qa653-ask";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(30);
/// The longest an import of the folder may take in this debug build.
const IMPORT_WAIT: Duration = Duration::from_secs(240);
const POLL: Duration = Duration::from_millis(60);
const SETTLE: Duration = Duration::from_millis(700);
/// The time between a key or click and the next, so the app sees each.
const STEP: Duration = Duration::from_millis(250);
/// niri's layout: each window takes the whole screen until the test sizes it, and no hot corner.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
/// The screen.
const SCREEN: (u32, u32) = (1280, 800);

/// Where a user sees the box's controls and the picker at 1280 px and font 14 (the UI tour's shots).
const EMOJI_BUTTON: (i32, i32) = (1101, 750);
const SEND_BUTTON: (i32, i32) = (1236, 750);
const TAB_ALL: (i32, i32) = (804, 370);
const TAB_EMOJI: (i32, i32) = (856, 370);
const TAB_STICKERS: (i32, i32) = (924, 370);
const SEE_ALL_EMOJI: (i32, i32) = (1071, 445);
const SEE_ALL_STICKERS: (i32, i32) = (1071, 538);
/// A pack's "More options".
const PACK_MORE: (i32, i32) = (1096, 471);
/// The end of the search field: its magnifier or its X; and a point of the field's own background.
const SEARCH_MARK: Rect = Rect {
    x: 1077,
    y: 399,
    w: 26,
    h: 17,
};
const SEARCH_FIELD: (i32, i32) = (1031, 407);
/// The pixels of a 16 px glyph's strokes, at the least.
const MARK_INK: usize = 12;
/// The picker, and the chat's messages, on the screen.
const PICKER: Rect = Rect {
    x: 777,
    y: 349,
    w: 338,
    h: 382,
};
/// The stickers on All, and in a row of Recent on Stickers.
const ALL_STICKERS: usize = 4;
const ROW: usize = 4;
/// The stickers in a row of a pack at its fewest (520 px, font 24).
const NARROWEST_ROW: usize = 3;
const MESSAGES: Rect = Rect {
    x: 456,
    y: 96,
    w: 824,
    h: 620,
};

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_sticker_picker_window: {error}");
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

fn faults_to_result(faults: &[String]) -> SandboxResult {
    ensure(faults.is_empty(), || faults.join("; "))
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

/// The first run of the app.
const FIRST: [Check; 27] = [
    ("opens_by_button_and_keys", opens_by_button_and_keys),
    ("teams_all_before_imports", teams::teams_all_before_imports),
    ("import_folder", import_folder),
    ("import_zip", import_zip),
    ("all_tab_and_see_all", all_tab_and_see_all),
    ("pick_and_send", pick_and_send),
    ("beside_text", beside_text),
    ("b_sees_stickers", b_sees_stickers),
    ("search_label_and_pack", search_label_and_pack),
    ("search_mark_and_clear", search_mark_and_clear),
    ("keyboard_whole_picker", keyboard_whole_picker),
    ("rename_and_remove", rename_and_remove),
    ("new_chat_no_stickers", new_chat_no_stickers),
    ("themes_fonts_narrow", themes_fonts_narrow),
    ("teams_packs_listed", teams::teams_packs_listed),
    ("teams_pointer_pick_and_send", teams::teams_pointer_pick_and_send),
    ("teams_b_sees_sent", teams::teams_b_sees_sent),
    ("teams_png_from_other_client", teams::teams_png_from_other_client),
    ("teams_search_and_keys", teams::teams_search_and_keys),
    ("teams_unreadable_sticker", teams::teams_unreadable_sticker),
    ("teams_all_after_send", teams::teams_all_after_send),
    ("caption_pointer_send", captions::caption_pointer_send),
    ("caption_b_sees", captions::caption_b_sees),
    ("caption_long_and_empty", captions::caption_long_and_empty),
    ("caption_meme", captions::caption_meme),
    ("caption_keyboard_and_back", captions::caption_keyboard_and_back),
    ("remember_before_restart", remember_before_restart),
];

/// The second run of the app over the same settings and store.
const RESTART: [Check; 2] = [
    ("packs_after_restart", packs_after_restart),
    ("teams_recent_after_restart", teams::teams_recent_after_restart),
];

/// A third run, in the read-only mode.
const READ_ONLY: [Check; 1] = [("read_only_no_emoji", read_only_no_emoji)];

/// A fourth run, both accounts with an own server's capabilities (no Teams sticker packs).
const OWN_SERVER: [Check; 1] = [("own_server_only_own_packs", teams::own_server_only_own_packs)];

#[derive(Default)]
struct Tally {
    passed: Vec<String>,
    failed: Vec<(String, String)>,
}

/// The chats the parent made on the mock.
struct Chats {
    words: String,
    sizes: String,
}

/// What the runs share: the sandbox, the mock, the chooser and the files.
struct World<'a> {
    sandbox: &'a Sandbox,
    mock: &'a ChatServiceMock,
    runtime: &'a tokio::runtime::Runtime,
    chats: &'a Chats,
    chooser: &'a Chooser,
    sources: &'a Sources,
    dir: &'a Path,
    trouter: &'a str,
    /// The chat of a and b where b's other client sent a Teams PNG sticker.
    teams_chat: &'a str,
    /// The chat of a and b for the captioned stickers.
    captions_chat: &'a str,
}

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_sticker_picker_window", &options)? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mock = runtime.block_on(ChatServiceMock::start())?;
    let chats = seed(&mock)?;
    let teams_chat = teams::seed(&mock, &runtime)?;
    let captions_chat = captions::seed(&mock)?;
    let trouter = mock.start_trouter()?;
    let chooser = Chooser::start(sandbox.bus().address())?;
    let sources = Sources::make()?;
    let dir = tempfile::tempdir()?;
    let world = World {
        sandbox: &sandbox,
        mock: &mock,
        runtime: &runtime,
        chats: &chats,
        chooser: &chooser,
        sources: &sources,
        dir: dir.path(),
        trouter: trouter.origin(),
        teams_chat: &teams_chat,
        captions_chat: &captions_chat,
    };
    let tally = run_all(&world)?;
    drop(trouter);
    report(&tally)
}

fn run_all(world: &World<'_>) -> SandboxResult<Tally> {
    let mut tally = Tally::default();
    let mut quits = Vec::new();
    let mut before = None;
    for (run, checks, mode) in [
        ("first", &FIRST[..], Mode::Normal),
        ("restart", &RESTART[..], Mode::Normal),
        ("read-only", &READ_ONLY[..], Mode::ReadOnly),
        ("own-server", &OWN_SERVER[..], Mode::OwnServer),
    ] {
        if !checks.iter().any(|(name, _)| wanted(name)) {
            continue;
        }
        say(&format!("run {run}"));
        let app = AppUnderTest::start(world, mode)?;
        let mut user = User::new(world, app)?;
        user.before_restart = before.take();
        for (name, check) in checks {
            if wanted(name) {
                user.run_check(name, *check, &mut tally);
            }
        }
        before = user.before_restart.take();
        quits.push(user.quit());
    }
    for quit in quits {
        quit?;
    }
    Ok(tally)
}

fn report(tally: &Tally) -> SandboxResult {
    let mut summary = String::new();
    for name in &tally.passed {
        let _ = writeln!(summary, "pass {name}");
    }
    for (name, why) in &tally.failed {
        let _ = writeln!(summary, "FAIL {name}: {why}");
    }
    say(&summary);
    if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
        let _ = fs::write(PathBuf::from(dir).join("results.txt"), &summary);
    }
    say(&format!(
        "qa_sticker_picker_window: {} passed, {} failed",
        tally.passed.len(),
        tally.failed.len()
    ));
    ensure(tally.failed.is_empty(), || {
        format!("{} checks failed", tally.failed.len())
    })
}

/// The mock's chats: a–b (the stickers sent alone), "Lab picker words" (a sticker by the keyboard and one beside
/// text) and "Lab picker sizes" (only b's words, for the sizes).
fn seed(mock: &ChatServiceMock) -> SandboxResult<Chats> {
    mock.post_as(LAB_B, AB_CHAT, "<p>Made-up words before the stickers</p>")
        .ok_or("b's words refused")?;
    let words = mock
        .add_group("Lab picker words", &[LAB_A, LAB_B])
        .ok_or("no chat made")?;
    mock.post_as(LAB_B, &words, "<p>Made-up words in the picker chat</p>")
        .ok_or("b's words refused")?;
    let sizes = mock
        .add_group("Lab picker sizes", &[LAB_A, LAB_B])
        .ok_or("no chat made")?;
    mock.post_as(LAB_B, &sizes, "<p>Made-up words for the picker sizes</p>")
        .ok_or("b's words refused")?;
    Ok(Chats { words, sizes })
}

/// How a run starts the app.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    ReadOnly,
    /// Both accounts with an own server's capabilities.
    OwnServer,
}

/// The child app in its sandbox: its process, its trace.
struct AppUnderTest {
    running: Option<Running>,
    _trace: TraceReader,
}

impl AppUnderTest {
    fn start(world: &World<'_>, mode: Mode) -> SandboxResult<Self> {
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let mut tokens = Vec::new();
        for mri in [LAB_A, LAB_B] {
            tokens.push(world.mock.token(mri).ok_or("the mock's token")?);
            tokens.push(world.mock.skypetoken(mri).ok_or("the mock's skypetoken")?);
        }
        let tokens = tokens.join(" ");
        let dir = world.dir.to_string_lossy().into_owned();
        let socket = world.sandbox.path("run/ctl.sock").to_string_lossy().into_owned();
        let child = world.sandbox.spawn_with_fds(
            &std::env::current_exe()?,
            &[],
            &[
                (CHILD_ENV, "1"),
                (READ_ONLY_ENV, if mode == Mode::ReadOnly { "1" } else { "0" }),
                (OWN_SERVER_ENV, if mode == Mode::OwnServer { "1" } else { "0" }),
                (TRACE_FD_ENV, &fd),
                (ORIGIN_ENV, &world.mock.origin()),
                (TROUTER_ENV, world.trouter),
                (TOKENS_ENV, &tokens),
                (DIR_ENV, &dir),
                (CTL_ENV, &socket),
            ],
            &[writer.as_fd()],
        )?;
        drop(writer);
        let mut running = Running::new(child)?;
        let mut trace = TraceReader::start(reader);
        if let Err(error) = running.line_where(STARTUP, |line| line == "ready") {
            return Err(running.fail(&format!("no ready line: {error:?}{}", world.sandbox.logs())));
        }
        trace.wait_for("status", "connected")?;
        Ok(Self {
            running: Some(running),
            _trace: trace,
        })
    }
}

/// The child's view trace, line by line as it comes.
struct TraceReader {
    lines: mpsc::Receiver<Value>,
    seen: Vec<Value>,
}

impl TraceReader {
    fn start(reader: io::PipeReader) -> Self {
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let value = serde_json::from_str(&line).unwrap_or(Value::String(line));
                let _ = sender.send(value);
            }
        });
        Self {
            lines,
            seen: Vec::new(),
        }
    }

    fn count(&mut self, view: &str, change: &str) -> usize {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
        self.seen
            .iter()
            .filter(|line| line["view"] == view && line["change"] == change)
            .count()
    }

    fn wait_for(&mut self, view: &str, change: &str) -> SandboxResult {
        let deadline = Instant::now() + STARTUP;
        while self.count(view, change) == 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .map_err(|_| format!("no trace line {view} {change}"))?;
            self.seen.push(line);
        }
        Ok(())
    }
}

// --- the screen ---

/// A rectangle on the screen, in the screenshot's px.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Rect {
    fn right(self) -> i32 {
        self.x + self.w
    }

    fn bottom(self) -> i32 {
        self.y + self.h
    }

    fn centre(self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }

    fn overlaps(self, other: Self) -> bool {
        self.x < other.right() && other.x < self.right() && self.y < other.bottom() && other.y < self.bottom()
    }

    fn whole_screen() -> Self {
        Self {
            x: 0,
            y: 0,
            w: i32::try_from(SCREEN.0).unwrap_or(1280),
            h: i32::try_from(SCREEN.1).unwrap_or(800),
        }
    }
}

fn pixel(image: &RgbImage, x: i32, y: i32) -> Option<[u8; 3]> {
    let (x, y) = (u32::try_from(x).ok()?, u32::try_from(y).ok()?);
    (x < image.width() && y < image.height()).then(|| image.get_pixel(x, y).0)
}

fn near(a: [u8; 3], b: [u8; 3], tolerance: u8) -> bool {
    a.iter().zip(b).all(|(one, other)| one.abs_diff(other) <= tolerance)
}

/// The tolerance of a face's colour: a GIF's palette moves it a little.
const FACE_TOLERANCE: u8 = 18;

/// The blobs of `colour` inside `area` of `least` px at least, by their bounding boxes. A face is a filled disc, so a
/// border or a focus line of the UI (a line a few px wide) never makes a blob this large.
fn blobs(image: &RgbImage, colour: Rgb, area: Rect, least: usize) -> Vec<Rect> {
    let width = image.width();
    let mut seen = vec![false; (width * image.height()) as usize];
    let mut found = Vec::new();
    for y in area.y.max(0)..area.bottom() {
        for x in area.x.max(0)..area.right() {
            let Some(colour_here) = pixel(image, x, y) else {
                continue;
            };
            let index = (y.cast_unsigned() * width + x.cast_unsigned()) as usize;
            if seen[index] || !near(colour_here, colour, FACE_TOLERANCE) {
                continue;
            }
            if let Some(blob) = flood(image, colour, area, &mut seen, (x, y), least) {
                found.push(blob);
            }
        }
    }
    found
}

/// The blob of `colour` in `area` that holds `start`, each of its px marked `seen`; its box, when it has `least` px.
fn flood(
    image: &RgbImage,
    colour: Rgb,
    area: Rect,
    seen: &mut [bool],
    start: (i32, i32),
    least: usize,
) -> Option<Rect> {
    let width = image.width();
    let index = |x: i32, y: i32| (y.cast_unsigned() * width + x.cast_unsigned()) as usize;
    seen[index(start.0, start.1)] = true;
    let mut stack = vec![start];
    let (mut x0, mut y0, mut x1, mut y1, mut count) = (start.0, start.1, start.0, start.1, 0usize);
    while let Some((x, y)) = stack.pop() {
        count += 1;
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        for (nx, ny) in [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)] {
            if nx < area.x || ny < area.y || nx >= area.right() || ny >= area.bottom() {
                continue;
            }
            let Some(here) = pixel(image, nx, ny) else { continue };
            if !seen[index(nx, ny)] && near(here, colour, FACE_TOLERANCE) {
                seen[index(nx, ny)] = true;
                stack.push((nx, ny));
            }
        }
    }
    (count >= least).then_some(Rect {
        x: x0,
        y: y0,
        w: x1 - x0 + 1,
        h: y1 - y0 + 1,
    })
}

/// The largest face of `colour` in `area`.
fn face_of(image: &RgbImage, colour: Rgb, area: Rect) -> Option<Rect> {
    blobs(image, colour, area, 150)
        .into_iter()
        .max_by_key(|blob| blob.w * blob.h)
}

/// Every face of the made-up stickers in `area`, with its colour.
fn faces_in(image: &RgbImage, area: Rect) -> Vec<(Rgb, Rect)> {
    files::ALL
        .iter()
        .flat_map(|colour| {
            blobs(image, *colour, area, 150)
                .into_iter()
                .map(move |blob| (*colour, blob))
        })
        .collect()
}

/// The ink in a face's mouth: it changes as an animated sticker plays.
fn mouth_ink(image: &RgbImage, face: Rect) -> usize {
    let (cx, cy) = face.centre();
    let (reach, low) = (face.w / 5, face.h / 15);
    let mut ink = 0;
    for y in cy + low..cy + face.h * 3 / 8 {
        for x in cx - reach..=cx + reach {
            if pixel(image, x, y).is_some_and(|colour| near(colour, files::INK, 40)) {
                ink += 1;
            }
        }
    }
    ink
}

// --- the user ---

/// The user at the sandbox: the pointer, the keyboard, the screenshots, and the child it asks about the window.
struct User<'a> {
    world: &'a World<'a>,
    app: AppUnderTest,
    input: VirtualInput,
    asked: u32,
    marks: u32,
    /// The text sent beside a sticker, for b's check.
    sent_mark: Option<String>,
    /// The shelf as the first run left it, for the restart.
    before_restart: Option<Value>,
    /// The media service objects the mock made for the stickers sent so far.
    uploads: u32,
}

impl<'a> User<'a> {
    fn new(world: &'a World<'a>, app: AppUnderTest) -> SandboxResult<Self> {
        let mut user = Self {
            world,
            app,
            input: world.sandbox.input()?,
            asked: 0,
            marks: 0,
            sent_mark: None,
            before_restart: None,
            uploads: 0,
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
        // the next check starts from a's a–b chat at 1280 px, Teams Light, font 14, no picker, an empty box
        let _ = self.press("escape");
        let _ = self.ask("reset");
        let _ = self.ask("switch a");
        let _ = self.resize(1280);
        let _ = self.ask("clear");
    }

    fn running(&self) -> SandboxResult<&Running> {
        self.app.running.as_ref().ok_or_else(|| "the app has quit".into())
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
        self.until_in(what, WAIT, wanted)
    }

    fn until_in(&mut self, what: &str, wait: Duration, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let start = Instant::now();
        loop {
            let state = self.state()?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > wait {
                return Err(format!("{what}: never came; last {state}").into());
            }
            thread::sleep(POLL);
        }
    }

    fn quit(&mut self) -> SandboxResult {
        self.ask("quit")?;
        let running = self.app.running.take().ok_or("the app has quit")?;
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

    fn click(&mut self, (x, y): (i32, i32)) -> SandboxResult {
        self.input.click(u32::try_from(x)?, u32::try_from(y)?, keys::BTN_LEFT)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn point(&mut self, (x, y): (i32, i32)) -> SandboxResult {
        self.input.move_to(u32::try_from(x)?, u32::try_from(y)?)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn park(&mut self) -> SandboxResult {
        // the window's left margin: no button, tooltip or tile there
        self.input.move_to(3, SCREEN.1 / 2)
    }

    /// A unique made-up word for a message, found again in what the mock was sent.
    fn mark(&mut self) -> String {
        self.marks += 1;
        format!("zq{}x", 100 + self.marks)
    }

    /// The bodies of the requests the mock got with `method` that hold `text`.
    fn sent_with(&self, method: &str, text: &str) -> Vec<String> {
        self.world
            .runtime
            .block_on(self.world.mock.received_requests())
            .iter()
            .filter(|request| request.method.as_str() == method)
            .map(|request| String::from_utf8_lossy(&request.body).into_owned())
            .filter(|body| body.contains(text))
            .collect()
    }

    fn wait_sent(&self, text: &str) -> SandboxResult<String> {
        wait_until(WAIT, || self.sent_with("POST", text).into_iter().next())
            .ok_or_else(|| format!("the mock got no message with {text:?}").into())
    }

    /// A screenshot as `qa653-<name>.png`, under `target/ui-shots/` and the evidence dir.
    fn shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        let file = format!("qa653-{name}.png");
        let png = self.world.sandbox.screenshot(&file, false)?;
        let kept = shots_dir()?.join(&file);
        fs::copy(&png, &kept)?;
        if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
            let dir = PathBuf::from(dir).join("shots");
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(&file))?;
        }
        Ok(kept)
    }

    fn image(&mut self, name: &str) -> SandboxResult<RgbImage> {
        self.park()?;
        thread::sleep(SETTLE);
        let path = self.shot(name)?;
        Ok(image::open(path)?.to_rgb8())
    }

    /// The faults `check` finds in a screenshot of a stable frame: the faces in `area` the same in two shots in a
    /// row and no fault found, or after `WAIT` the last shot's faults. A loaded machine draws the pictures late.
    fn drawn(
        &mut self,
        name: &str,
        area: Rect,
        check: impl Fn(&RgbImage) -> Vec<String>,
    ) -> SandboxResult<Vec<String>> {
        self.park()?;
        let start = Instant::now();
        let mut before = None;
        loop {
            thread::sleep(STEP);
            let path = self.shot(name)?;
            let image = image::open(path)?.to_rgb8();
            let faces = faces_in(&image, area);
            let stable = before.as_ref() == Some(&faces);
            let mut faults = check(&image);
            if stable && faults.is_empty() {
                return Ok(faults);
            }
            if start.elapsed() > WAIT {
                if !stable {
                    faults.push(format!("{name}: the faces never stopped changing: {faces:?}"));
                }
                return Ok(faults);
            }
            before = Some(faces);
        }
    }

    /// Opens `chat` in the shown account's window and waits until it shows.
    fn open(&mut self, chat: &str) -> SandboxResult<Value> {
        self.ask(&format!("open {chat}"))?;
        self.until(&format!("{chat} open"), |state| state["chat"]["id"] == chat)
    }

    /// Shows account `alias` ("a" or "b").
    fn switch(&mut self, alias: &str) -> SandboxResult<Value> {
        self.ask(&format!("switch {alias}"))?;
        let upn = format!("bot-{alias}@");
        self.until(&format!("account {alias} shown"), |state| {
            state["account"]
                .as_str()
                .is_some_and(|account| account.starts_with(&upn))
        })
    }

    // --- the window's size ---

    fn windows(&self) -> SandboxResult<Vec<Value>> {
        let text = self.world.sandbox.niri_msg(&["-j", "windows"])?;
        let windows: Value = serde_json::from_str(&text)?;
        Ok(windows.as_array().cloned().unwrap_or_default())
    }

    /// The width niri gives the focused window.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "niri's logical px are whole numbers on a scale of 1"
    )]
    fn focused_width(&self) -> SandboxResult<Option<i32>> {
        Ok(self
            .windows()?
            .iter()
            .find(|window| window["is_focused"] == true)
            .and_then(|window| window["layout"]["window_size"][0].as_f64())
            .map(|width| width.round() as i32))
    }

    /// The compositor sizes the window to `width` wide, as a user's drag does.
    fn resize(&mut self, width: i32) -> SandboxResult {
        if self.focused_width()? == Some(width) {
            return Ok(());
        }
        self.world
            .sandbox
            .niri_msg(&["action", "set-column-width", &width.to_string()])?;
        wait_until(WAIT, || self.focused_width().ok().flatten().filter(|now| *now == width))
            .ok_or_else(|| format!("the window never took {width} px"))?;
        thread::sleep(SETTLE);
        Ok(())
    }
}

// --- the picker, as the user reaches it ---

fn picker(state: &Value) -> &Value {
    &state["picker"]
}

fn picker_open(state: &Value) -> bool {
    !picker(state).is_null()
}

fn rows(state: &Value) -> Vec<Vec<String>> {
    picker(state)["rows"]
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

fn has_item(state: &Value, selector: &str) -> bool {
    position(&rows(state), selector).is_some()
}

fn pictures(state: &Value) -> Vec<Value> {
    state["compose"]["pictures"].as_array().cloned().unwrap_or_default()
}

/// The pack `id` as the shelf holds it.
fn pack<'s>(state: &'s Value, id: &str) -> Option<&'s Value> {
    state["shelf"]["packs"].as_array()?.iter().find(|pack| pack["id"] == id)
}

/// The selector of the sticker `label` of pack `id`.
fn sticker_selector(state: &Value, id: &str, label: &str) -> SandboxResult<String> {
    let file = pack(state, id)
        .and_then(|pack| pack["stickers"].as_array())
        .and_then(|stickers| stickers.iter().find(|sticker| sticker["label"] == label))
        .and_then(|sticker| sticker["file"].as_str())
        .ok_or_else(|| format!("no sticker {label:?} in pack {id}"))?;
    let number = file.split('.').next().unwrap_or_default();
    Ok(format!("sticker-{id}-{number}"))
}

impl User<'_> {
    /// Ctrl+. in the box: the picker opens.
    fn open_picker_by_keys(&mut self) -> SandboxResult<Value> {
        self.ask("focus-box")?;
        self.press("ctrl+.")?;
        self.until("the picker opened by Ctrl+.", picker_open)
    }

    /// The picker opened by the keys, on its Stickers tab.
    fn open_stickers_by_keys(&mut self) -> SandboxResult<Value> {
        self.open_picker_by_keys()?;
        self.press("ctrl+tab")?;
        self.press("ctrl+tab")?;
        self.until("the Stickers tab", |state| picker(state)["tab"] == "Stickers")
    }

    /// The keyboard moves to the item `selector` in the picker (from the search field, the tabs or another item).
    fn key_to(&mut self, selector: &str) -> SandboxResult<Value> {
        for _ in 0..80 {
            let state = self.state()?;
            ensure(picker_open(&state), || format!("the picker closed before {selector}"))?;
            let rows = rows(&state);
            if picker(&state)["current"] == selector {
                return Ok(state);
            }
            let goal = position(&rows, selector).ok_or_else(|| format!("no {selector} in the picker: {rows:?}"))?;
            let key = match picker(&state)["place"].as_str() {
                Some("search" | "tabs") => "down",
                Some("items") => {
                    let now = picker(&state)["current"]
                        .as_str()
                        .and_then(|current| position(&rows, current))
                        .unwrap_or((0, 0));
                    step_toward(now, goal)
                }
                _ => return Err(format!("the keyboard is not in the picker: {}", picker(&state)).into()),
            };
            self.press(key)?;
        }
        Err(format!("the keyboard never reached {selector}").into())
    }

    /// The keyboard picks the item `selector`: it moves there and presses Enter.
    fn key_pick(&mut self, selector: &str) -> SandboxResult {
        self.key_to(selector)?;
        self.press("enter")
    }

    /// The pack `id` opened by the keyboard on the Stickers tab.
    fn key_open_pack(&mut self, id: &str) -> SandboxResult<Value> {
        self.key_pick(&format!("sticker-pack-{id}"))?;
        let view = format!("Pack {{ id: \"{id}\"");
        self.until(&format!("pack {id} shown"), |state| {
            picker(state)["view"]
                .as_str()
                .is_some_and(|shown| shown.starts_with(&view))
        })
    }
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

// --- the checks ---

/// The emoji button opens the picker on All with its search field focused, the tabs switch, Escape closes it,
/// Ctrl+. opens it, the button closes it again, and the pointer over it hovers no message under it.
fn opens_by_button_and_keys(user: &mut User<'_>) -> SandboxResult {
    user.open(AB_CHAT)?;
    user.ask("focus-box")?;
    user.click(EMOJI_BUTTON)?;
    let state = user.until("the picker opened by its button", picker_open)?;
    let mut faults = Vec::new();
    if picker(&state)["tab"] != "All" || picker(&state)["place"] != "search" {
        faults.push(format!("not on All with the search focused: {}", picker(&state)));
    }
    user.image("open-by-button")?;
    user.point((PICKER.x + 60, PICKER.bottom() - 40))?;
    let state = user.state()?;
    if !state["chat"]["hovered"].is_null() {
        faults.push(format!(
            "a message under the picker is hovered: {}",
            state["chat"]["hovered"]
        ));
    }
    user.press("escape")?;
    let state = user.until("Escape closed the picker", |state| !picker_open(state))?;
    if state["compose"]["focused"] != true {
        faults.push("the box lost the focus after Escape".to_owned());
    }
    user.open_picker_by_keys()?;
    user.image("open-by-keys")?;
    for (at, tab) in [(TAB_EMOJI, "Emoji"), (TAB_STICKERS, "Stickers"), (TAB_ALL, "All")] {
        user.click(at)?;
        if let Err(error) = user.until(&format!("the {tab} tab"), |state| picker(state)["tab"] == tab) {
            faults.push(error.to_string());
        }
        user.image(&format!("tab-{}", tab.to_lowercase()))?;
    }
    user.click(TAB_STICKERS)?;
    let state = user.until("the Stickers tab", |state| picker(state)["tab"] == "Stickers")?;
    // a Microsoft account has Teams' packs, so its Stickers tab with no pack of the user's is not empty
    if !has_item(&state, "sticker-pack-add") || !has_item(&state, "teams-pack-popular") {
        faults.push(format!(
            "no Add a sticker pack and Teams' packs on a Stickers tab with no pack of the user's: {:?}",
            rows(&state)
        ));
    }
    user.image("stickers-empty")?;
    user.click(EMOJI_BUTTON)?;
    if let Err(error) = user.until("the button closed the picker", |state| !picker_open(state)) {
        faults.push(error.to_string());
    }
    faults_to_result(&faults)
}

impl User<'_> {
    /// The user imports `source` (a folder or a `.zip` file) from the Stickers tab: the empty state's button, or "Add
    /// a sticker pack" and its menu. It returns the states seen while the import ran, and the state at its end.
    fn import(&mut self, source: &Path, archive: bool, name: &str) -> SandboxResult<(Vec<Value>, Value)> {
        self.start_import(source, archive)?;
        self.finish_import(name)
    }

    /// The user picks `source` to import from the Stickers tab.
    fn start_import(&mut self, source: &Path, archive: bool) -> SandboxResult {
        self.open_stickers_by_keys()?;
        self.world.chooser.clear();
        self.open_import_menu()?;
        self.world.chooser.queue(Answer::Pick(source.to_owned()));
        self.key_pick(if archive {
            "sticker-import-zip"
        } else {
            "sticker-import-folder"
        })
    }

    /// The import actions shown: by "Add a sticker pack" when the tab has packs (Teams' too), it has no other.
    fn open_import_menu(&mut self) -> SandboxResult {
        if !has_item(&self.state()?, "sticker-import-folder") {
            self.key_pick("sticker-pack-add")?;
            self.until("the Add a sticker pack menu", |state| {
                has_item(state, "sticker-import-folder")
            })?;
        }
        Ok(())
    }

    /// The states seen while the import ran, and the state at its end.
    fn finish_import(&mut self, name: &str) -> SandboxResult<(Vec<Value>, Value)> {
        let start = Instant::now();
        let mut running = Vec::new();
        loop {
            let state = self.state()?;
            let import = &state["shelf"]["import"];
            if import["running"] == true {
                if running.is_empty() {
                    self.shot(&format!("{name}-progress"))?;
                }
                running.push(import.clone());
            } else if !import.is_null() {
                return Ok((running, state));
            }
            if start.elapsed() > IMPORT_WAIT {
                return Err(format!("the import of {name} never ended: {import}").into());
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// While an import runs, the user asks for a second one (the spare folder): the faults in how the app refuses it.
    fn second_import(&mut self) -> SandboxResult<Vec<String>> {
        // The progress check: a fast import can end before `finish_import` polls, so it cannot see the line.
        self.until("the import runs", |state| state["shelf"]["import"]["running"] == true)?;
        self.shot("import-folder-progress")?;
        self.open_import_menu()?;
        self.world.chooser.queue(Answer::Pick(self.world.sources.spare.clone()));
        self.key_pick("sticker-import-folder")?;
        let state = self.until_in("the second import refused, or the first ended", IMPORT_WAIT, |state| {
            let import = &state["shelf"]["import"];
            import["refused"] == true || import["running"] != true
        })?;
        if state["shelf"]["import"]["refused"] != true {
            let fault = format!(
                "the second import was not refused while the first ran: {}",
                state["shelf"]
            );
            return Ok(vec![fault]);
        }
        self.image("import-busy")?;
        Ok(Vec::new())
    }
}

/// After the refused import: the note gone with the first import, and no pack from the spare folder.
fn spare_faults(state: &Value) -> Vec<String> {
    let mut faults = Vec::new();
    if state["shelf"]["import"]["refused"] == true {
        faults.push("the refusal stays after the import ended".to_owned());
    }
    let named = state["shelf"]["packs"]
        .as_array()
        .is_some_and(|packs| packs.iter().any(|pack| pack["name"] == files::SPARE));
    if pack(state, files::SPARE_ID).is_some() || named {
        faults.push(format!("the refused import made a pack: {}", state["shelf"]["packs"]));
    }
    faults
}

/// The skipped files a summary names, and the faults in them.
fn summary_faults(state: &Value) -> Vec<String> {
    let skipped: Vec<String> = state["shelf"]["import"]["skipped"]
        .as_array()
        .map(|all| all.iter().filter_map(|one| one.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    say(&format!("  skipped: {skipped:?}"));
    let mut faults = Vec::new();
    for (file, reason) in files::SKIPPED {
        if !skipped.iter().any(|line| line.contains(file) && line.contains(reason)) {
            faults.push(format!("the summary does not skip {file} ({reason:?}): {skipped:?}"));
        }
    }
    for file in files::PASSED_OVER {
        if skipped.iter().any(|line| line.contains(file)) {
            faults.push(format!("the summary names {file}, which it must pass over"));
        }
    }
    faults
}

/// The folder: a progress line, a second import refused while it runs, a summary with each skipped file and its
/// reason, the pack with its stickers (the animated ones as such), and Close.
fn import_folder(user: &mut User<'_>) -> SandboxResult {
    user.open(AB_CHAT)?;
    let source = user.world.sources.zorblets.clone();
    user.start_import(&source, false)?;
    let mut faults = user.second_import()?;
    let (running, state) = user.finish_import("import-folder")?;
    user.image("import-folder-summary")?;
    faults.extend(spare_faults(&state));
    faults.extend(summary_faults(&state));
    say(&format!(
        "  progress seen {} times: {:?}",
        running.len(),
        running.first()
    ));
    if state["shelf"]["import"]["done"] != files::ZORBLETS_ID {
        faults.push(format!(
            "the import did not end with the pack: {}",
            state["shelf"]["import"]
        ));
    }
    faults.extend(zorblets_faults(&state));
    user.key_pick("sticker-import-close")?;
    let state = user.until("the summary closed", |state| state["shelf"]["import"].is_null())?;
    if !has_item(&state, &format!("sticker-pack-{}", files::ZORBLETS_ID)) {
        faults.push(format!("the Stickers tab does not show the pack: {:?}", rows(&state)));
    }
    user.image("import-folder-pack-shows")?;
    thread::sleep(SETTLE);
    faults.extend(spare_faults(&user.state()?));
    faults_to_result(&faults)
}

/// The folder's pack: its name and each good file, animated or still as its source.
fn zorblets_faults(state: &Value) -> Vec<String> {
    let Some(pack) = pack(state, files::ZORBLETS_ID) else {
        return vec![format!("no pack {}: {}", files::ZORBLETS_ID, state["shelf"])];
    };
    let mut faults = Vec::new();
    if pack["name"] != files::ZORBLETS {
        faults.push(format!("the pack's name: {}", pack["name"]));
    }
    let stickers = pack["stickers"].as_array().cloned().unwrap_or_default();
    if stickers.len() != 5 + files::FILLERS {
        faults.push(format!("{} stickers, {} expected", stickers.len(), 5 + files::FILLERS));
    }
    for (label, animated) in [
        ("Zorb sunny", false),
        ("Zorb minty", true),
        ("Zorb rosy", false),
        ("Zorb tangy", true),
        ("Zorb plummy", true),
    ] {
        match stickers.iter().find(|sticker| sticker["label"] == label) {
            Some(sticker) if sticker["animated"] == animated && sticker["size"] == json!([180, 180]) => {}
            Some(sticker) => faults.push(format!("{label}: {sticker}, animated {animated} at 180 px expected")),
            None => faults.push(format!("no sticker {label}")),
        }
    }
    faults
}

/// The `.zip` file through "Add a sticker pack": its pack, the note skipped, the Mac's extras and the hidden file
/// passed over, and Show opens the pack. Then a third pack (to remove later) from a folder.
fn import_zip(user: &mut User<'_>) -> SandboxResult {
    user.open(AB_CHAT)?;
    let source = user.world.sources.quibbles.clone();
    let (_, state) = user.import(&source, true, "import-zip")?;
    user.image("import-zip-summary")?;
    let mut faults = Vec::new();
    let skipped = state["shelf"]["import"]["skipped"].clone();
    if skipped.as_array().map(Vec::len) != Some(1) || !skipped.to_string().contains("readme.md") {
        faults.push(format!("the zip's summary skips {skipped}, only readme.md expected"));
    }
    match pack(&state, files::QUIBBLES_ID) {
        Some(pack) if pack["name"] == files::QUIBBLES && pack["stickers"].as_array().map(Vec::len) == Some(2) => {}
        other => faults.push(format!("the zip's pack: {other:?}")),
    }
    user.key_pick("sticker-import-open")?;
    let view = format!("Pack {{ id: \"{}\"", files::QUIBBLES_ID);
    if let Err(error) = user.until("Show opened the pack", |state| {
        picker(state)["view"]
            .as_str()
            .is_some_and(|shown| shown.starts_with(&view))
    }) {
        faults.push(error.to_string());
    }
    user.image("import-zip-shown")?;
    user.press("escape")?;
    let source = user.world.sources.doomed.clone();
    let (_, state) = user.import(&source, false, "import-doomed")?;
    if pack(&state, files::DOOMED_ID).is_none() {
        faults.push(format!("no pack {}: {}", files::DOOMED_ID, state["shelf"]));
    }
    user.key_pick("sticker-import-close")?;
    faults_to_result(&faults)
}

/// The sticker selectors in a row of the picker.
fn sticker_items(row: &[String]) -> usize {
    row.iter()
        .filter(|item| item.starts_with("sticker-") && !item.starts_with("sticker-pack"))
        .count()
}

/// All: six emoji and four stickers, each section with "See all"; the four drawn whole in the picker; each "See all"
/// opens its tab.
fn all_tab_and_see_all(user: &mut User<'_>) -> SandboxResult {
    user.open(AB_CHAT)?;
    let state = user.open_picker_by_keys()?;
    let rows = rows(&state);
    let mut faults = Vec::new();
    let emoji = rows
        .iter()
        .map(|row| row.iter().filter(|item| item.starts_with("fun-emoji-")).count())
        .max()
        .unwrap_or(0);
    let stickers = rows.iter().map(|row| sticker_items(row)).max().unwrap_or(0);
    if emoji != 6 || stickers != 4 {
        faults.push(format!("All shows {emoji} emoji and {stickers} stickers: {rows:?}"));
    }
    for see_all in ["emoji-see-all", "stickers-see-all"] {
        if position(&rows, see_all).is_none() {
            faults.push(format!("no {see_all} on All"));
        }
    }
    let image = user.image("all-tab")?;
    let faces = faces_in(&image, PICKER);
    say(&format!("  faces on All: {faces:?}"));
    if faces.len() != ALL_STICKERS {
        faults.push(format!(
            "{} sticker faces drawn on All, {ALL_STICKERS} expected",
            faces.len()
        ));
    }
    user.click(SEE_ALL_STICKERS)?;
    if let Err(error) = user.until("See all opened Stickers", |state| {
        picker(state)["tab"] == "Stickers" && picker(state)["view"] == "Home"
    }) {
        faults.push(error.to_string());
    }
    user.click(TAB_ALL)?;
    user.until("the All tab", |state| picker(state)["tab"] == "All")?;
    user.click(SEE_ALL_EMOJI)?;
    if let Err(error) = user.until("See all opened Emoji", |state| picker(state)["tab"] == "Emoji") {
        faults.push(error.to_string());
    }
    user.image("see-all-emoji")?;
    faults_to_result(&faults)
}

impl User<'_> {
    /// The media service answers the next picture send as a new object of its own, which then serves the bytes of the
    /// kept sticker `label` of pack `id` (the bytes the app sends), as the service gives a sent picture back to b.
    fn serve_upload(&mut self, id: &str, label: &str) -> SandboxResult {
        let state = self.state()?;
        let file = pack(&state, id)
            .and_then(|pack| pack["stickers"].as_array())
            .and_then(|stickers| stickers.iter().find(|sticker| sticker["label"] == label))
            .and_then(|sticker| sticker["file"].as_str())
            .ok_or_else(|| format!("no sticker {label:?} in pack {id}"))?
            .to_owned();
        let bytes = fs::read(self.world.dir.join("stickers").join(id).join(&file))?;
        let gif = Path::new(&file)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gif"));
        let kind = if gif { "image/gif" } else { "image/png" };
        self.uploads += 1;
        let object = format!("0-eus-d1-{:032x}", 0x0065_3000 + self.uploads);
        let mock = self.world.mock;
        self.world.runtime.block_on(async {
            mock.answer_next("POST", "/v1/objects/", 201, &json!({ "id": object }))
                .await;
            mock.refuse_next("PUT", &format!("/v1/objects/{object}/content/imgpsh"), 201)
                .await;
            mock.serve_bytes(&format!("/v1/objects/{object}/views/imgo"), kind, bytes)
                .await;
        });
        Ok(())
    }

    /// The user clicks the face of `colour` in the picker.
    fn click_face(&mut self, colour: Rgb, name: &str) -> SandboxResult {
        let image = self.image(name)?;
        let face =
            face_of(&image, colour, PICKER).ok_or_else(|| format!("{name}: no face {colour:?} in the picker"))?;
        self.click(face.centre())
    }

    /// The box holds one sticker `label`, sent to the media service, and the picker is closed.
    fn sticker_in_box(&mut self, label: &str) -> SandboxResult<Value> {
        self.until(&format!("{label} in the box, sent"), |state| {
            let pictures = pictures(state);
            !picker_open(state)
                && pictures.len() == 1
                && pictures[0]["sticker"] == label
                && pictures[0]["state"] == "sent"
        })
    }

    /// Send posts the box's sticker `label`: the message the mock got names it as a sticker; the box is empty after.
    fn send_sticker(&mut self, label: &str, by_key: bool) -> SandboxResult<String> {
        if by_key {
            self.ask("focus-box")?;
            self.press("enter")?;
        } else {
            self.click(SEND_BUTTON)?;
        }
        let body = self.wait_sent(label)?;
        self.until("the box empty after Send", |state| pictures(state).is_empty())?;
        ensure(body.contains("schema.skype.com/Sticker"), || {
            format!("{label} was not sent as a sticker: {body}")
        })?;
        Ok(body)
    }
}

/// a sends a still PNG (by the pointer: the pack's tile, the sticker, Send), an animated WebP (the pointer) and an
/// animated GIF (the keyboard, Enter to send); each goes in the box first, at its size; Recent fills, last first.
fn pick_and_send(user: &mut User<'_>) -> SandboxResult {
    user.open(AB_CHAT)?;
    user.ask("focus-box")?;
    user.click(EMOJI_BUTTON)?;
    user.until("the picker opened by its button", picker_open)?;
    user.click(TAB_STICKERS)?;
    user.until("the Stickers tab", |state| picker(state)["tab"] == "Stickers")?;
    // the folder's pack tile shows its first sticker, the APNG
    user.click_face(files::MINTY, "pick-stickers-home")?;
    let view = format!("Pack {{ id: \"{}\"", files::ZORBLETS_ID);
    user.until("the pack's tile opened it", |state| {
        picker(state)["view"]
            .as_str()
            .is_some_and(|shown| shown.starts_with(&view))
    })?;
    user.serve_upload(files::ZORBLETS_ID, "Zorb sunny")?;
    user.click_face(files::SUNNY, "pick-pack-zorblets")?;
    let state = user.sticker_in_box("Zorb sunny")?;
    let mut faults = Vec::new();
    if pictures(&state)[0]["given"] != json!([180, 180]) {
        faults.push(format!("the sticker in the box: {}", pictures(&state)[0]));
    }
    user.image("pick-sunny-in-box")?;
    user.send_sticker("Zorb sunny", false)?;
    user.open_stickers_by_keys()?;
    user.key_open_pack(files::ZORBLETS_ID)?;
    user.serve_upload(files::ZORBLETS_ID, "Zorb tangy")?;
    user.click_face(files::TANGY, "pick-pack-again")?;
    user.sticker_in_box("Zorb tangy")?;
    user.send_sticker("Zorb tangy", false)?;
    let chat = user.world.chats.words.clone();
    user.open(&chat)?;
    let state = user.open_stickers_by_keys()?;
    let plummy = sticker_selector(&state, files::ZORBLETS_ID, "Zorb plummy")?;
    user.key_open_pack(files::ZORBLETS_ID)?;
    user.serve_upload(files::ZORBLETS_ID, "Zorb plummy")?;
    user.key_pick(&plummy)?;
    user.sticker_in_box("Zorb plummy")?;
    user.send_sticker("Zorb plummy", true)?;
    let state = user.open_stickers_by_keys()?;
    let recent = state["shelf"]["recent"].clone();
    say(&format!("  recent: {recent}"));
    let first: Vec<String> = recent
        .as_array()
        .map(|all| {
            all.iter()
                .take(3)
                .filter_map(|one| one.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let label_of = |reference: &str| {
        let (id, file) = reference.split_once('/').unwrap_or_default();
        pack(&state, id)
            .and_then(|pack| pack["stickers"].as_array())
            .and_then(|all| all.iter().find(|sticker| sticker["file"] == file))
            .map(|sticker| sticker["label"].as_str().unwrap_or_default().to_owned())
            .unwrap_or_default()
    };
    let labels: Vec<String> = first.iter().map(|one| label_of(one)).collect();
    if labels != ["Zorb plummy", "Zorb tangy", "Zorb sunny"] {
        faults.push(format!("Recent is {labels:?}, the last pick first expected"));
    }
    user.image("recent-after-picks")?;
    faults_to_result(&faults)
}

/// Text typed, then a sticker found by its label and taken with Enter: both in the box, then in one message.
fn beside_text(user: &mut User<'_>) -> SandboxResult {
    let chat = user.world.chats.words.clone();
    user.open(&chat)?;
    user.ask("focus-box")?;
    let mark = user.mark();
    user.type_text(&format!("{mark} beside a sticker "))?;
    user.press("ctrl+.")?;
    user.until("the picker opened by Ctrl+.", picker_open)?;
    user.type_text("rosy")?;
    let state = user.until("the search found Zorb rosy", |state| {
        rows(state).iter().any(|row| sticker_items(row) > 0)
    })?;
    let rosy = sticker_selector(&state, files::ZORBLETS_ID, "Zorb rosy")?;
    let mut faults = Vec::new();
    if !has_item(&state, &rosy) {
        faults.push(format!("the search for rosy shows {:?}", rows(&state)));
    }
    user.serve_upload(files::ZORBLETS_ID, "Zorb rosy")?;
    user.press("enter")?;
    let state = user.until("the sticker beside the text", |state| {
        pictures(state).len() == 1
            && pictures(state)[0]["state"] == "sent"
            && state["compose"]["text"]
                .as_str()
                .is_some_and(|text| text.contains(&mark))
    })?;
    if pictures(&state)[0]["sticker"] != "Zorb rosy" {
        faults.push(format!("the box holds {}", pictures(&state)[0]));
    }
    user.image("text-and-sticker-in-box")?;
    user.ask("focus-box")?;
    user.press("enter")?;
    let body = user.wait_sent(&mark)?;
    if !body.contains("schema.skype.com/Sticker") || !body.contains("Zorb rosy") {
        faults.push(format!("the text and the sticker are not in one message: {body}"));
    }
    user.sent_mark = Some(mark);
    faults_to_result(&faults)
}

fn items(state: &Value) -> Vec<Value> {
    state["chat"]["items"].as_array().cloned().unwrap_or_default()
}

/// Whether a face is drawn whole: not cut by the chat's top or bottom edge as it scrolls.
fn whole(face: Rect) -> bool {
    face.w.abs_diff(face.h) <= 4
}

/// Whether the sticker `face`, on the left of the chat, is drawn at 180 px and bare, as Teams draws one: no frame,
/// tile or bubble behind it. The made-up sticker is a disc on a clear square, so the chat's ground shows at the
/// corners and the sides of its 180 px square, and right outside the square, where a bubble has its own fill.
fn bare_faults(image: &RgbImage, face: Rect, what: &str) -> Vec<String> {
    let mut faults = Vec::new();
    if !(138..=162).contains(&face.w) || !(138..=162).contains(&face.h) {
        faults.push(format!(
            "{what}: the face is {}x{}, not about 150 (not shown at 180 px)",
            face.w, face.h
        ));
    }
    let (cx, cy) = face.centre();
    let Some(ground) = pixel(image, face.right() + 220, cy) else {
        return vec![format!("{what}: no ground beside the sticker")];
    };
    // inside the square past the disc (its corners and sides), then outside the square
    let clear = [
        (-84, -84),
        (84, -84),
        (-84, 84),
        (84, 84),
        (-84, 0),
        (84, 0),
        (0, 84),
        (-100, 0),
        (100, 0),
    ];
    // the ground is a gradient: each point against the ground on its own row, right of the sticker
    let behind: Vec<((i32, i32), [u8; 3])> = clear
        .into_iter()
        .map(|(dx, dy)| (cx + dx, cy + dy))
        .filter_map(|(x, y)| {
            let ground = pixel(image, face.right() + 220, y)?;
            pixel(image, x, y)
                .filter(|here| !near(*here, ground, 8))
                .map(|here| ((x, y), here))
        })
        .collect();
    say(&format!("  {what}: behind it {behind:?}, the ground {ground:?}"));
    if !behind.is_empty() {
        faults.push(format!(
            "{what}: {behind:?} behind the sticker, the ground is {ground:?}: a frame or a bubble"
        ));
    }
    faults
}

/// Whether the face of `colour` moves: its mouth changes between screenshots within `WAIT`. A loaded machine plays
/// the frames late.
fn moves(user: &mut User<'_>, colour: Rgb, area: Rect, name: &str) -> SandboxResult<bool> {
    let mut seen = BTreeSet::new();
    let start = Instant::now();
    let mut frame = 0;
    while seen.len() < 2 && start.elapsed() <= WAIT {
        let path = user.shot(&format!("{name}-{}", frame % 8))?;
        let image = image::open(path)?.to_rgb8();
        if let Some(face) = face_of(&image, colour, area) {
            seen.insert(mouth_ink(&image, face));
        }
        frame += 1;
        thread::sleep(Duration::from_millis(110));
    }
    say(&format!("  {name}: mouth ink seen {seen:?} in {frame} shots"));
    Ok(seen.len() >= 2)
}

/// a's still and animated stickers in b's a–b chat in `theme`: one whole at least, each whole one bare at 180 px.
fn b_chat_faults(image: &RgbImage, theme: &str) -> Vec<String> {
    let shown: Vec<(Rect, &str)> = [(files::SUNNY, "Zorb sunny"), (files::TANGY, "Zorb tangy")]
        .into_iter()
        .filter_map(|(colour, label)| face_of(image, colour, MESSAGES).map(|face| (face, label)))
        .collect();
    let mut faults = Vec::new();
    if !shown.iter().any(|(face, _)| whole(*face)) {
        faults.push(format!("{theme}: b sees no whole sticker of a's: {shown:?}"));
    }
    for (face, label) in shown.into_iter().filter(|(face, _)| whole(*face)) {
        faults.extend(bare_faults(image, face, &format!("{theme} {label}")));
    }
    faults
}

/// The GIF sticker in b's words chat, bare at 180 px.
fn gif_faults(image: &RgbImage) -> Vec<String> {
    match face_of(image, files::PLUMMY, MESSAGES) {
        Some(face) => bare_faults(image, face, "the GIF"),
        None => vec!["b does not see the GIF sticker".to_owned()],
    }
}

/// b: a's still and animated stickers bare at 180 px, the animated one playing; the GIF bare too, and the sticker
/// beside the text in a's bubble under the text; b's picker has a's packs.
fn b_sees_stickers(user: &mut User<'_>) -> SandboxResult {
    user.switch("b")?;
    user.open(AB_CHAT)?;
    let state = user.until("a's stickers in b's chat", |state| {
        has_labels(state, &["Zorb sunny", "Zorb tangy"])
    })?;
    let mut faults = b_items_faults(&state, &["Zorb sunny", "Zorb tangy"]);
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        faults.extend(user.drawn(&format!("b-ab-chat-{theme}"), MESSAGES, |image| {
            b_chat_faults(image, theme)
        })?);
    }
    user.ask("theme-light")?;
    if !moves(user, files::TANGY, MESSAGES, "b-tangy-plays")? {
        faults.push("the animated WebP sticker does not play for b".to_owned());
    }
    let chat = user.world.chats.words.clone();
    user.open(&chat)?;
    let state = user.until("a's stickers in b's words chat", |state| {
        has_labels(state, &["Zorb plummy", "Zorb rosy"])
    })?;
    faults.extend(b_items_faults(&state, &["Zorb plummy"]));
    let mark = user.sent_mark.clone().unwrap_or_default();
    match items(&state)
        .iter()
        .find(|item| item["lines"].to_string().contains(&mark))
    {
        Some(item) if item["label"] == "Zorb rosy" && item["bare"] == false => {}
        other => faults.push(format!("the text and sticker message for b: {other:?}")),
    }
    faults.extend(user.drawn("b-words-chat", MESSAGES, gif_faults)?);
    if !moves(user, files::PLUMMY, MESSAGES, "b-plummy-plays")? {
        faults.push("the GIF sticker does not play for b".to_owned());
    }
    let state = user.open_stickers_by_keys()?;
    for id in [files::ZORBLETS_ID, files::QUIBBLES_ID] {
        if !has_item(&state, &format!("sticker-pack-{id}")) {
            faults.push(format!("b's picker has no pack {id}"));
        }
    }
    user.image("b-picker-stickers")?;
    faults_to_result(&faults)
}

/// Whether the open chat holds a message with each sticker of `labels`.
fn has_labels(state: &Value, labels: &[&str]) -> bool {
    let items = items(state);
    labels
        .iter()
        .all(|label| items.iter().any(|item| item["label"] == *label))
}

/// The messages of a that b sees as stickers alone, bare, each `label`.
fn b_items_faults(state: &Value, labels: &[&str]) -> Vec<String> {
    let mut faults = Vec::new();
    for label in labels {
        let found = items(state)
            .into_iter()
            .find(|item| item["label"] == *label && item["own"] == false);
        match found {
            Some(item) if item["sticker_alone"] == true && item["bare"] == true => {}
            other => faults.push(format!("b's message {label}: {other:?}")),
        }
    }
    faults
}

/// The search finds a sticker by its label on All and the stickers of a pack by the pack's name, on All and on
/// Stickers.
fn search_label_and_pack(user: &mut User<'_>) -> SandboxResult {
    user.open(AB_CHAT)?;
    let state = user.open_picker_by_keys()?;
    let rosy = sticker_selector(&state, files::ZORBLETS_ID, "Zorb rosy")?;
    let cyan = sticker_selector(&state, files::QUIBBLES_ID, "Quib cyan")?;
    let lime = sticker_selector(&state, files::QUIBBLES_ID, "Quib lime")?;
    let mut faults = Vec::new();
    user.type_text("rosy")?;
    if let Err(error) = user.until("rosy found on All", |state| has_item(state, &rosy)) {
        faults.push(error.to_string());
    }
    user.image("search-label-all")?;
    user.press("ctrl+a")?;
    user.type_text("quibbles")?;
    let state = user.until("the pack's stickers found", |state| has_item(state, &cyan))?;
    if !has_item(&state, &lime) || has_item(&state, &rosy) {
        faults.push(format!("a search for the pack's name finds {:?}", rows(&state)));
    }
    user.image("search-pack-all")?;
    user.press("ctrl+tab")?;
    user.press("ctrl+tab")?;
    let state = user.until("the Stickers tab with the search", |state| {
        picker(state)["tab"] == "Stickers" && has_item(state, &cyan)
    })?;
    if !has_item(&state, &lime) {
        faults.push(format!("Stickers' search finds {:?}", rows(&state)));
    }
    user.image("search-pack-stickers")?;
    faults_to_result(&faults)
}

/// The pixels in `area` that stand out from the colour at `background`.
fn ink(image: &RgbImage, area: Rect, background: (i32, i32)) -> BTreeSet<(i32, i32)> {
    let Some(field) = pixel(image, background.0, background.1) else {
        return BTreeSet::new();
    };
    (area.y..area.bottom())
        .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
        .filter(|&(x, y)| pixel(image, x, y).is_some_and(|colour| !near(colour, field, 60)))
        .collect()
}

impl User<'_> {
    /// The glyph at the end of the search field, as a screenshot `name` shows it.
    fn search_mark(&mut self, name: &str) -> SandboxResult<BTreeSet<(i32, i32)>> {
        let image = self.image(name)?;
        let mark = ink(&image, SEARCH_MARK, SEARCH_FIELD);
        say(&format!("  {name}: {} pixels of the mark", mark.len()));
        Ok(mark)
    }
}

/// The search field's end, as in Teams: a magnifier while it is empty, an X once it has text. The X clears the
/// search and keeps the focus in the field; Escape still closes the picker.
fn search_mark_and_clear(user: &mut User<'_>) -> SandboxResult {
    user.open(AB_CHAT)?;
    let state = user.open_picker_by_keys()?;
    let cyan = sticker_selector(&state, files::QUIBBLES_ID, "Quib cyan")?;
    let unsearched = rows(&state);
    let mut faults = Vec::new();
    let magnifier = user.search_mark("search-empty")?;
    if magnifier.len() < MARK_INK {
        faults.push("the empty field shows no magnifier".to_owned());
    }
    user.type_text("rosy")?;
    user.until("the search changed the items", |state| rows(state) != unsearched)?;
    let cross = user.search_mark("search-typed")?;
    if cross.len() < MARK_INK {
        faults.push("the field with text shows no X".to_owned());
    }
    if magnifier.symmetric_difference(&cross).count() < MARK_INK {
        faults.push("the field with text still shows the magnifier, not an X".to_owned());
    }
    user.click(SEARCH_MARK.centre())?;
    match user.until("the X cleared the search", |state| rows(state) == unsearched) {
        Ok(state) if picker(&state)["place"] != "search" => {
            faults.push(format!("the X took the focus from the field: {}", picker(&state)));
        }
        Ok(_) => {}
        Err(error) => faults.push(error.to_string()),
    }
    let again = user.search_mark("search-cleared")?;
    if magnifier.symmetric_difference(&again).count() >= MARK_INK {
        faults.push("the cleared field does not show the magnifier again".to_owned());
    }
    user.type_text("quib")?;
    if let Err(error) = user.until("typing after the X searches", |state| has_item(state, &cyan)) {
        faults.push(error.to_string());
    }
    user.image("search-after-clear")?;
    user.press("escape")?;
    if let Err(error) = user.until("Escape closed the picker", |state| !picker_open(state)) {
        faults.push(error.to_string());
    }
    faults_to_result(&faults)
}

/// One key and the state it must lead to, in the picker.
fn key_leads(user: &mut User<'_>, key: &str, what: &str, wanted: impl Fn(&Value) -> bool) -> Option<String> {
    if let Err(error) = user.press(key) {
        return Some(error.to_string());
    }
    user.until(&format!("{key}: {what}"), wanted)
        .err()
        .map(|error| error.to_string())
}

/// The tabs by the arrows, Home and End; the search field; the items; a sticker by Enter; Escape; Ctrl+Tab.
fn keyboard_whole_picker(user: &mut User<'_>) -> SandboxResult {
    user.open(AB_CHAT)?;
    let state = user.open_picker_by_keys()?;
    let cyan = sticker_selector(&state, files::QUIBBLES_ID, "Quib cyan")?;
    let tab = |want: &'static str| move |state: &Value| picker(state)["tab"] == want;
    let mut faults: Vec<String> = Vec::new();
    faults.extend(key_leads(user, "up", "the tabs", |state| {
        picker(state)["place"] == "tabs"
    }));
    for (key, want) in [
        ("right", "Emoji"),
        ("right", "Stickers"),
        ("right", "All"),
        ("left", "Stickers"),
        ("home", "All"),
        ("end", "Stickers"),
    ] {
        faults.extend(key_leads(user, key, want, tab(want)));
    }
    user.image("keys-tabs")?;
    faults.extend(key_leads(user, "enter", "the search field", |state| {
        picker(state)["place"] == "search"
    }));
    faults.extend(key_leads(user, "down", "the items", |state| {
        picker(state)["place"] == "items"
    }));
    user.image("keys-items")?;
    user.key_open_pack(files::QUIBBLES_ID)?;
    user.serve_upload(files::QUIBBLES_ID, "Quib cyan")?;
    user.key_to(&cyan)?;
    user.image("keys-on-sticker")?;
    user.press("enter")?;
    let state = user.sticker_in_box("Quib cyan")?;
    if state["compose"]["focused"] != true {
        faults.push("the box has no focus after the pick".to_owned());
    }
    user.open_picker_by_keys()?;
    faults.extend(key_leads(user, "escape", "closed, the box focused", |state| {
        !picker_open(state) && state["compose"]["focused"] == true && pictures(state).len() == 1
    }));
    user.open_picker_by_keys()?;
    faults.extend(key_leads(user, "ctrl+tab", "Emoji", tab("Emoji")));
    faults.extend(key_leads(user, "ctrl+tab", "Stickers", tab("Stickers")));
    faults.extend(key_leads(user, "ctrl+shift+tab", "Emoji", tab("Emoji")));
    faults.extend(key_leads(user, "escape", "closed", |state| !picker_open(state)));
    faults_to_result(&faults)
}

impl User<'_> {
    /// "More options" of the shown pack, by the pointer; its menu opens.
    fn pack_menu(&mut self, name: &str) -> SandboxResult<Value> {
        self.click(PACK_MORE)?;
        let opened = self.until("the pack's menu", |state| has_item(state, "sticker-pack-rename"));
        self.image(name)?;
        opened
    }

    fn pack_name(&mut self, id: &str) -> SandboxResult<String> {
        let state = self.state()?;
        Ok(pack(&state, id)
            .and_then(|pack| pack["name"].as_str())
            .unwrap_or_default()
            .to_owned())
    }
}

/// Rename a pack (Enter saves it, Escape keeps the old name); remove one after its question (Cancel keeps it), its
/// sticker gone from Recent and its files from the disk.
fn rename_and_remove(user: &mut User<'_>) -> SandboxResult {
    user.open(AB_CHAT)?;
    user.open_stickers_by_keys()?;
    user.key_open_pack(files::QUIBBLES_ID)?;
    let mut faults = Vec::new();
    user.pack_menu("pack-menu")?;
    user.key_pick("sticker-pack-rename")?;
    user.image("renaming")?;
    user.press("ctrl+a")?;
    user.type_text("Qa renamed quibbles")?;
    user.press("enter")?;
    if let Err(error) = user.until("the new name", |state| {
        pack(state, files::QUIBBLES_ID).is_some_and(|pack| pack["name"] == "Qa renamed quibbles")
    }) {
        faults.push(error.to_string());
    }
    user.image("renamed")?;
    user.key_pick("sticker-pack-menu")?;
    user.key_pick("sticker-pack-rename")?;
    user.press("ctrl+a")?;
    user.type_text("Not this name")?;
    user.press("escape")?;
    let name = user.pack_name(files::QUIBBLES_ID)?;
    let state = user.state()?;
    if name != "Qa renamed quibbles" || !picker_open(&state) {
        faults.push(format!(
            "Escape in the rename: the name is {name:?}, the picker open {}",
            picker_open(&state)
        ));
    }
    user.press("escape")?;
    faults.extend(remove_doomed(user)?);
    faults_to_result(&faults)
}

/// The doomed pack's sticker into Recent, then the pack removed after its question.
fn remove_doomed(user: &mut User<'_>) -> SandboxResult<Vec<String>> {
    let state = user.open_stickers_by_keys()?;
    let tan = sticker_selector(&state, files::DOOMED_ID, "Doomed tan")?;
    user.key_open_pack(files::DOOMED_ID)?;
    user.serve_upload(files::DOOMED_ID, "Doomed tan")?;
    user.key_pick(&tan)?;
    user.sticker_in_box("Doomed tan")?;
    user.ask("clear")?;
    user.open_stickers_by_keys()?;
    user.key_open_pack(files::DOOMED_ID)?;
    user.pack_menu("doomed-menu")?;
    user.key_pick("sticker-pack-remove")?;
    user.until("the question", |state| has_item(state, "sticker-pack-remove-confirm"))?;
    user.image("remove-question")?;
    let mut faults = Vec::new();
    user.key_pick("sticker-pack-remove-cancel")?;
    let state = user.until("Cancel", |state| !has_item(state, "sticker-pack-remove-confirm"))?;
    if pack(&state, files::DOOMED_ID).is_none() {
        faults.push("Cancel removed the pack".to_owned());
    }
    user.key_pick("sticker-pack-menu")?;
    user.key_pick("sticker-pack-remove")?;
    user.key_pick("sticker-pack-remove-confirm")?;
    let state = user.until("the pack removed", |state| pack(state, files::DOOMED_ID).is_none())?;
    if picker(&state)["view"] != "Home" {
        faults.push(format!("after the removal the picker shows {}", picker(&state)["view"]));
    }
    if state["shelf"]["recent"].to_string().contains(files::DOOMED_ID) {
        faults.push(format!(
            "Recent keeps the removed pack's sticker: {}",
            state["shelf"]["recent"]
        ));
    }
    if user.world.dir.join("stickers").join(files::DOOMED_ID).exists() {
        faults.push("the removed pack's files stay on the disk".to_owned());
    }
    user.image("after-remove")?;
    Ok(faults)
}

/// New message's box takes no pictures: its picker has All and Emoji, no Stickers.
fn new_chat_no_stickers(user: &mut User<'_>) -> SandboxResult {
    user.open(AB_CHAT)?;
    user.press("ctrl+n")?;
    user.until("New message", |state| state["main"] == "NewChat")?;
    user.open_picker_by_keys()?;
    let mut faults = Vec::new();
    let tab = |want: &'static str| move |state: &Value| picker(state)["tab"] == want;
    faults.extend(key_leads(user, "ctrl+tab", "Emoji", tab("Emoji")));
    faults.extend(key_leads(user, "ctrl+tab", "All, with no Stickers between", tab("All")));
    let state = user.state()?;
    if rows(&state).iter().any(|row| sticker_items(row) > 0) {
        faults.push(format!("New message's picker offers stickers: {:?}", rows(&state)));
    }
    user.image("new-chat-picker")?;
    user.press("escape")?;
    faults_to_result(&faults)
}

/// The tile's margin left and right of `face`, along the face's middle row: the run of the tile's own colour (the
/// px just left of the face's rim) on each side. A tile cut by the picker's edge has less on its right.
fn tile_margins(image: &RgbImage, face: Rect) -> Option<(i32, i32)> {
    let (_, cy) = face.centre();
    let tile = pixel(image, face.x - 3, cy)?;
    let run = |from: i32, step: i32| {
        let mut at = from;
        while pixel(image, at + step, cy).is_some_and(|colour| near(colour, tile, 6)) && (at - from).abs() < 80 {
            at += step;
        }
        (at - from).abs()
    };
    Some((run(face.x - 3, -1) + 3, run(face.right() + 2, 1) + 3))
}

/// The faces in the picker at `width`: `least` stickers at least, each face whole (square), inside the window, apart
/// from the others, and each sticker's tile whole (as much margin right of the face as left of it).
fn size_faults(image: &RgbImage, width: i32, least: usize, what: &str) -> Vec<String> {
    let faces = faces_in(image, Rect::whole_screen());
    let tallest = faces.iter().map(|(_, face)| face.h).max().unwrap_or(0);
    let mut faults = Vec::new();
    // a pack tile's small face is not a sticker
    let stickers = faces.iter().filter(|(_, face)| face.h >= 40).count();
    if stickers < least {
        faults.push(format!("{what}: {stickers} stickers drawn, {least} at least"));
    }
    for (index, (colour, face)) in faces.iter().enumerate() {
        if face.x < 0 || face.right() > width {
            faults.push(format!(
                "{what}: the face {colour:?} {face:?} is outside the {width} px window"
            ));
        }
        // a face cut by the picker's scroll edge is shorter than the others; the others are whole
        if face.h * 10 >= tallest * 9 && face.w.abs_diff(face.h) > 4 {
            faults.push(format!("{what}: the face {colour:?} is {face:?}, not round"));
        }
        // a pack tile's small face has the pack's name on its left; a sticker's tile has the face in its middle
        if face.h >= 40
            && let Some((left, right)) = tile_margins(image, *face)
            && left.abs_diff(right) > 4
        {
            faults.push(format!(
                "{what}: the tile of the face {colour:?} at {face:?} is cut: {left} px of tile left of it, {right} right"
            ));
        }
        for (_, other) in &faces[index + 1..] {
            if face.overlaps(*other) {
                faults.push(format!("{what}: the faces {face:?} and {other:?} overlap"));
            }
        }
    }
    faults
}

/// All, Stickers and a pack at each size, font and theme: each sticker whole, inside the window, apart.
fn themes_fonts_narrow(user: &mut User<'_>) -> SandboxResult {
    let chat = user.world.chats.sizes.clone();
    let mut faults = Vec::new();
    for (width, font) in [1280, 800, 520]
        .into_iter()
        .flat_map(|width| [14, 20, 24].map(|font| (width, font)))
    {
        user.resize(width)?;
        user.ask(&format!("font {font}"))?;
        for theme in ["light", "dark"] {
            user.ask(&format!("theme-{theme}"))?;
            user.open(&chat)?;
            let name = format!("size-{width}-font-{font}-{theme}");
            user.open_picker_by_keys()?;
            faults.extend(user.drawn(&format!("{name}-all"), Rect::whole_screen(), |image| {
                size_faults(image, width, ALL_STICKERS, &format!("{name} All"))
            })?);
            user.press("ctrl+tab")?;
            user.press("ctrl+tab")?;
            let state = user.until("the Stickers tab", |state| picker(state)["tab"] == "Stickers")?;
            let recent = state["shelf"]["recent"].as_array().map_or(0, Vec::len).min(ROW);
            faults.extend(user.drawn(&format!("{name}-stickers"), Rect::whole_screen(), |image| {
                size_faults(image, width, recent, &format!("{name} Stickers"))
            })?);
            let state = user.key_open_pack(files::ZORBLETS_ID)?;
            let stickers = pack(&state, files::ZORBLETS_ID)
                .and_then(|pack| pack["stickers"].as_array())
                .map_or(0, Vec::len)
                .min(NARROWEST_ROW);
            faults.extend(user.drawn(&format!("{name}-pack"), Rect::whole_screen(), |image| {
                size_faults(image, width, stickers, &format!("{name} pack"))
            })?);
            user.press("escape")?;
        }
    }
    faults_to_result(&faults)
}

/// The shelf as the first run leaves it.
fn remember_before_restart(user: &mut User<'_>) -> SandboxResult {
    let state = user.state()?;
    user.before_restart = Some(state["shelf"].clone());
    Ok(())
}

/// After a restart: the same packs (the new name, not the removed one) and Recent, for a and for b.
fn packs_after_restart(user: &mut User<'_>) -> SandboxResult {
    let state = user.state()?;
    let mut faults = Vec::new();
    let names: Vec<String> = state["shelf"]["packs"]
        .as_array()
        .map(|all| {
            all.iter()
                .filter_map(|pack| pack["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if names != [files::ZORBLETS, "Qa renamed quibbles"] {
        faults.push(format!("the packs after the restart: {names:?}"));
    }
    if let Some(before) = &user.before_restart
        && (before["packs"] != state["shelf"]["packs"] || before["recent"] != state["shelf"]["recent"])
    {
        faults.push(format!("the shelf changed: before {before}, after {}", state["shelf"]));
    }
    for alias in ["a", "b"] {
        user.switch(alias)?;
        user.open(AB_CHAT)?;
        let state = user.open_stickers_by_keys()?;
        for id in [files::ZORBLETS_ID, files::QUIBBLES_ID] {
            if !has_item(&state, &format!("sticker-pack-{id}")) {
                faults.push(format!("{alias}'s picker after the restart has no pack {id}"));
            }
        }
        user.image(&format!("restart-{alias}-stickers"))?;
        user.press("escape")?;
    }
    faults_to_result(&faults)
}

/// The read-only app: the box offers no emoji button, and Ctrl+. opens no picker.
fn read_only_no_emoji(user: &mut User<'_>) -> SandboxResult {
    let state = user.open(AB_CHAT)?;
    let mut faults = Vec::new();
    if state["compose"]["read_only"] != true {
        faults.push(format!("the box is not read-only: {}", state["compose"]));
    }
    user.ask("focus-box")?;
    user.press("ctrl+.")?;
    thread::sleep(Duration::from_secs(1));
    if picker_open(&user.state()?) {
        faults.push("Ctrl+. opened the picker in the read-only app".to_owned());
    }
    user.click(EMOJI_BUTTON)?;
    thread::sleep(Duration::from_secs(1));
    if picker_open(&user.state()?) {
        faults.push("a click where the emoji button is opened the picker".to_owned());
    }
    user.image("read-only-box")?;
    faults_to_result(&faults)
}

// --- the child ---

/// An account's mock tokens; they never run out in a test.
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

/// The engine of `alias` (a or b) on the mock: its chats, its media service and its Trouter; no calls. In the
/// read-only mode its writes are refused before sending.
fn engine(alias: Alias, tokens: (String, String), dir: &Path, read_only: bool) -> SandboxResult<EngineHandle> {
    let own_server = env(OWN_SERVER_ENV)? == "1";
    let mri = if alias == Alias::A { LAB_A } else { LAB_B };
    let tokens = Arc::new(Tokens {
        ic3: tokens.0,
        skypetoken: tokens.1,
        renewals: broadcast::channel(4).0,
    });
    let origin = env(ORIGIN_ENV)?;
    let trouter_origin = env(TROUTER_ENV)?;
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
    // the outbox sends a picked sticker's picture to the media service at once
    let transport = http()?.with_ams(AMS_BASE)?;
    // as `ngteams --read-only` builds it: every write refused before sending
    let transport = Arc::new(if read_only { transport.read_only() } else { transport });
    let options = Options {
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(200),
        backoff_jitter_percent: 0,
        ..Options::default()
    };
    let trouter =
        Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), &trouter_origin)?.with_options(options);
    let name = display_name(&format!("bot-{alias}@lab.example.test")).to_owned();
    let client = http()?
        .with_ams(AMS_BASE)?
        .with_presence(PRESENCE_BASE)?
        .with_csa(CSA_BASE)?
        .with_user_profile(USER_PROFILE_BASE)?
        .with_provisioning(PROVISIONING_BASE)?
        .with_middle_tier(MIDDLE_TIER_BASE)?;
    Ok(engine::start(Parts {
        calls: None,
        store: Store::open(&support::store_file(dir, alias))?,
        client: Arc::new(ChatClient::new(client)),
        outbox: Outbox::new(transport, Author::new(Mri::new(mri), name)),
        push: Some(Push::new(trouter)),
        options: if own_server {
            EngineOptions {
                capabilities: teams_core::Capabilities::of(
                    &teams_proto::Cloud::own("https://own.example.test").map_err(|_| "own origin")?,
                    |_| true,
                ),
                ..EngineOptions::default()
            }
        } else {
            EngineOptions::default()
        },
        known_names: vec![
            (LAB_A.to_owned(), "Lab A".to_owned()),
            (LAB_B.to_owned(), "Lab B".to_owned()),
            (LAB_C.to_owned(), "Lab C".to_owned()),
        ],
    })?)
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let tokens: Vec<String> = env(TOKENS_ENV)?.split(' ').map(str::to_owned).collect();
    let [ic3_a, skype_a, ic3_b, skype_b] = <[String; 4]>::try_from(tokens).map_err(|_| "four tokens expected")?;
    let read_only = env(READ_ONLY_ENV)? == "1";
    let first = support::started(Alias::A, engine(Alias::A, (ic3_a, skype_a), &dir, read_only)?)?;
    let second = support::started(Alias::B, engine(Alias::B, (ic3_b, skype_b), &dir, read_only)?)?;
    let runner = support::runner(&dir)?;
    let settings = teams_ui::SettingsFile::new(dir.join("settings.toml"));
    let mut opened = settings.load().settings;
    opened.selected_chat = Some(AB_CHAT.to_owned());
    // the mock's made-up week starts a meeting at a fixed hour; its reminder card would cover the picker
    opened.meeting_reminders = false;
    settings.save(&opened).map_err(|problem| format!("{problem:?}"))?;
    let ctl = PathBuf::from(env(CTL_ENV)?);
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: vec![second],
                trace: Arc::new(trace),
                ctl: Some(ctl),
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

/// The box the user sees: New message's, or the open chat's.
fn shown_compose(shell: &Shell, cx: &App) -> Option<Entity<Compose>> {
    if *shell.main() == Main::NewChat {
        return shell.new_chat().map(|new_chat| new_chat.read(cx).compose(cx));
    }
    shell.chat().map(|chat| chat.read(cx).compose().clone())
}

fn on_shell(shell: &Entity<Shell>, verb: &str, window: &mut Window, cx: &mut App) -> String {
    match verb {
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
        }
        "reset" => change_settings(shell, cx, |settings| {
            settings.appearance.follow_system = false;
            settings.appearance.theme = teams_ui::theme::LIGHT_KEY;
            settings.appearance.font_size = 14;
        }),
        "clear" | "focus-box" => {
            let Some(compose) = shown_compose(shell.read(cx), cx) else {
                return "no box".to_owned();
            };
            compose.update(cx, |compose, cx| {
                if verb == "clear" {
                    if compose.emoji_picker().is_some() {
                        compose.toggle_emoji(window, cx);
                    }
                    compose.discard(window, cx);
                }
                compose.focus(window, cx);
            });
        }
        other => return on_shell_with(shell, other, window, cx),
    }
    "ok".to_owned()
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

fn state_of(shell: &Entity<Shell>, desktop: &Entity<Desktop>, window: &Window, cx: &mut App) -> Value {
    let dir = shell.read(cx).settings().read(cx).stickers_dir();
    let packs = shelf_state(&Shelf::of(dir, cx), cx);
    let shell_ref = shell.read(cx);
    let chat = shell_ref
        .chat()
        .filter(|_| matches!(shell_ref.main(), Main::Chat(_)))
        .map(|chat| chat_state(chat, cx));
    let compose = shown_compose(shell_ref, cx);
    let picker = compose
        .as_ref()
        .and_then(|compose| compose.read(cx).emoji_picker().cloned());
    json!({
        "main": format!("{:?}", shell_ref.main()),
        "account": desktop
            .read(cx)
            .accounts()
            .read(cx)
            .active()
            .map(|tile| tile.detail.to_string()),
        "chat": chat,
        "compose": compose.map(|compose| compose_state(&compose, window, cx)),
        "picker": picker.map(|picker| picker_state(&picker, window, cx)),
        "shelf": packs,
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
        .map(|row| row.iter().map(Item::selector).collect())
        .collect();
    let editor = picker.sticker_editor().map(|editor| {
        let editor = editor.read(cx);
        json!({
            "sticker": editor.sticker(),
            "typed": editor.typed(cx),
            "can_finish": editor.can_finish(cx),
            "failed": editor.failed(),
            "drawn": editor.drawn().map(|(width, height)| [width, height]),
            "focus": editor.holds_focus(window, cx),
        })
    });
    json!({
        "editor": editor,
        "tab": format!("{:?}", picker.tab()),
        "view": format!("{:?}", picker.sticker_view()),
        "place": place,
        "current": picker.current(window, cx).map(|item| item.selector()),
        "rows": rows,
        "focus": picker.holds_focus(window, cx),
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
            json!({
                "sticker": picture.sticker.as_ref().map(ToString::to_string),
                "alt": picture.alt.as_ref().map(ToString::to_string),
                "state": state,
                "given": [picture.given.0, picture.given.1],
            })
        })
        .collect();
    json!({
        "text": compose.value(cx).to_string(),
        "read_only": compose.is_read_only(),
        "pictures": pictures,
        "problem": compose.problem().map(ToString::to_string),
        "focused": compose.focused(window, cx),
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
                .map(|sticker| {
                    json!({
                        "file": sticker.file,
                        "label": sticker.label,
                        "size": [sticker.size.0, sticker.size.1],
                        "animated": sticker.animated(),
                    })
                })
                .collect();
            json!({"id": pack.id, "name": pack.name, "stickers": stickers})
        })
        .collect();
    let import = shelf.import_state().map(|state| match state {
        ImportState::Running(progress) => json!({
            "running": true,
            "done": progress.done,
            "total": progress.total,
            "refused": shelf.refused(),
        }),
        ImportState::Done(imported) => json!({
            "done": imported.pack.id,
            "skipped": imported.skipped.iter().map(ToString::to_string).collect::<Vec<_>>(),
        }),
        ImportState::Failed(error) => json!({"failed": format!("{error:?}")}),
    });
    json!({
        "packs": packs,
        "recent": shelf
            .recent()
            .iter()
            .map(|sticker| format!("{}/{}", sticker.pack, sticker.file))
            .collect::<Vec<_>>(),
        "import": import,
    })
}

fn chat_state(chat: &Entity<ChatView>, cx: &App) -> Value {
    let view = chat.read(cx);
    let items: Vec<Value> = view.message_items().map(item_state).collect();
    json!({
        "id": view.conversation(),
        "items": items,
        "hovered": view.hovered(),
    })
}

fn item_state(item: &teams_ui::chat_view::MessageItem) -> Value {
    let (bare, sticker_alone, label, lines) = match &item.body {
        Body::Text(text) => {
            let rendered = &text.0;
            let sticker = rendered.blocks.iter().find_map(|block| match block {
                Block::Picture(picture) if picture.sticker => Some(picture),
                _ => None,
            });
            let lines: Vec<String> = rendered.blocks.iter().flat_map(Block::plain_lines).collect();
            (
                rendered.bare,
                rendered.sticker_alone().is_some(),
                sticker.and_then(|picture| picture.alt.clone()),
                lines,
            )
        }
        _ => (false, false, None, Vec::new()),
    };
    json!({
        "id": item.id,
        "own": item.own,
        "bare": bare,
        "sticker_alone": sticker_alone,
        "label": label,
        "lines": lines,
    })
}
