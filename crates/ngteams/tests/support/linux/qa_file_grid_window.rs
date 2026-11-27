//! QA of task 525 (round 1), in the UI sandbox: a message with several files shows Teams' grid of file cards, and a
//! user uses the cards (`harness = false`: GPUI runs on the process's main thread).
//!
//! The parent runs the mock (`teams-mock`). Lab account c posts messages with files to chats of its own with a, and
//! the mock serves the videos' thumbnails and bytes (Graph's thumbnail list, the media host's picture, the file read by
//! ranges). The files' links are links for specific people (task 793): the mock's Graph answers their reads only when
//! they redeem the link, as Graph does. The parent runs a's app as a child in the sandbox with a control socket. It
//! uses the app as a user does: it clicks with the sandbox's pointer and presses keys, and finds the cards on the
//! screen by their borders. It reads the links the sandbox's URI opener got (it opens nothing), the sandbox's
//! clipboard, and the views' rows from the child.
//!
//! - **The grid**: one file, two videos, five and eight mixed files, and text over three files. Each shows cards, never
//!   the line "This message has content that can't be shown here". At most two cards in a row, of equal width, 10 px
//!   apart, inside the message's bubble; the eight files take four rows; a file alone, one card two columns wide (task
//!   793). Teams Light and Teams Dark.
//! - **A narrow window**: one card in a row, none cut at the right; then wide again.
//! - **A channel post with files and its thread** beside the posts.
//! - **The actions**: a click on a name opens the file's link; a click on a video's picture plays it in the card, a
//!   second click pauses it; Share opens "Share this file" (task 543) and Escape closes it; the zip has no Share;
//!   "More options" of a document: "Open in browser", "Download", "Share" (the dialog) and "Copy link"; of a video:
//!   "Download", "Share" and "Copy link", no "Open in browser", as Teams on the web has them; Escape closes a menu.
//!   "Download" saves the file in a's downloads folder through the engine (task 991), never through the browser.
//! - **Odd shapes**, each beside good entries: a lone surrogate escape, an entry with no `@type`, an entry with no
//!   SharePoint link, a list encoded twice.
//! - **A video opened** (task 793, `qa_file_grid_video.rs`): a video alone, a link Graph refuses, the keyboard, and
//!   the Recap tab of a meeting whose recap names no drive or item of its recording.
//! - **The keyboard on the cards** (task 849, `qa_file_grid_keys.rs`): Tab, Shift+Tab, Enter, Space and Escape on a
//!   document, an image, a video and a recap's thumbnail.
//!
//! Every name, id and text is made up. Screenshots go to `target/ui-shots/qa525-*.png`, and also to the directory
//! `QA_FILE_GRID_EVIDENCE` names. Without weston 15, niri or `dbus-daemon` it says so and passes, unless
//! `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, RawFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, future, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
use ngteams::descriptors::adopt_trace;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_ctl::protocol::{OpenTarget, Request};
use teams_ctl::{Client, Timeouts};
use teams_mock::{AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, TEAM_CHANNEL, TrouterMock};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::chat_view::{Body, ChatView, MessageItem};
use teams_ui::rich_text::Block;
use teams_ui::shell::Shell;
use teams_ui::tray::{MENU_PATH, MENU_QUIT};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "qa_file_grid_keys.rs"]
mod card_keys;
#[path = "../window.rs"]
mod support;
#[path = "qa_file_grid_video.rs"]
mod video;
use support::{Alias, display_name};
use video::{LONE_CLIP, MCPS_BASE, REFUSED};

const CHILD_ENV: &str = "QA_FILE_GRID_CHILD";
const TRACE_FD_ENV: &str = "QA_FILE_GRID_TRACE_FD";
const ORIGIN_ENV: &str = "QA_FILE_GRID_ORIGIN";
const TROUTER_ENV: &str = "QA_FILE_GRID_TROUTER";
const TOKEN_ENV: &str = "QA_FILE_GRID_TOKEN";
const DIR_ENV: &str = "QA_FILE_GRID_DIR";
/// A directory that gets a copy of each screenshot.
const EVIDENCE_ENV: &str = "QA_FILE_GRID_EVIDENCE";
/// The file in the app's directory the parent asks the child about its views through.
const ASK: &str = "qa525-ask";
/// The file in the app's directory the child reads the theme to show from (`light` or `dark`).
const THEME_FILE: &str = "theme";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);
/// How long the screen gets to settle before a shot.
const SETTLE: Duration = Duration::from_millis(900);
/// How long a click that must do nothing gets to do it anyway.
const QUIET: Duration = Duration::from_millis(1500);
/// The screen: tall, so that the eight files' four rows fit in the chat at once.
const SCREEN: (u32, u32) = (1280, 1440);
/// The narrow main window's width.
const NARROW: u32 = 700;
/// niri's layout: each window takes the whole screen until the test sizes it, and no hot corner.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";

/// A card's size and the grid's gap, as Teams draws them (`teams_ui::files`).
const CARD_WIDTH: u32 = 316;
/// The card of a message's only file: two columns wide, its 16:9 picture under the header (`teams_ui::files`).
const LONE_WIDTH: u32 = 2 * CARD_WIDTH;
const LONE_HEIGHT: u32 = 460;
const HEADER: u32 = 64;
const GAP: u32 = 10;
/// The thread pane's width beside a channel's posts.
const THREAD_WIDTH: u32 = 440;

/// What the line under a message says when a part of it can't be shown.
const UNSHOWN: &str = "This message has content that can't be shown here";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_file_grid_window: {error}");
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
    let Some(sandbox) = sandbox_or_skip("ngteams file grid", &options)? else {
        return Ok(());
    };
    let service = Service::start(&runtime)?;
    let dir = tempfile::tempdir()?;
    if let Err(error) = walk(&sandbox, &service, dir.path()) {
        return Err(format!("{error}{}", sandbox.logs()).into());
    }
    let findings = std::mem::take(&mut *FINDINGS.lock().unwrap_or_else(PoisonError::into_inner));
    if !findings.is_empty() {
        return Err(format!("the walk ended with findings:\n- {}", findings.join("\n- ")).into());
    }
    say("ngteams file grid: ok");
    Ok(())
}

fn walk(sandbox: &Sandbox, service: &Service, dir: &Path) -> SandboxResult {
    let mut run = AppRun::launch(sandbox, service, dir)?;
    let outcome = (|| -> SandboxResult {
        let mut ctx = Walk {
            run: &mut run,
            sandbox,
            service,
        };
        ctx.grids()?;
        ctx.actions()?;
        ctx.odd_shapes()?;
        ctx.narrow()?;
        ctx.thread()?;
        ctx.videos_open()?;
        ctx.card_keys()?;
        ctx.downloads()
    })();
    if let Err(error) = outcome {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit(sandbox)
}

/// What one step of the walk works with.
struct Walk<'a> {
    run: &'a mut AppRun,
    sandbox: &'a Sandbox,
    service: &'a Service,
}

impl Walk<'_> {
    fn posted(&self, key: &str) -> SandboxResult<&Posted> {
        self.service
            .posted
            .iter()
            .find(|posted| posted.key == key)
            .ok_or_else(|| format!("no message {key}").into())
    }

    /// Opens the chat of message `key` and waits until its row holds its files; the row.
    fn open_message(&mut self, key: &str) -> SandboxResult<Item> {
        let posted = self.posted(key)?.clone();
        self.run.open(&posted.chat)?;
        let state = self.run.until("chat", key, |state| {
            state
                .item(&posted.id)
                .is_some_and(|item| item.files.len() == posted.cards)
        })?;
        let item = state.item(&posted.id).cloned().ok_or("no row")?;
        thread::sleep(SETTLE * 2);
        Ok(item)
    }

    /// The shot `qa525-<name>-<theme>.png` in `look`'s theme; the cards on it.
    fn shot_in(&mut self, name: &str, look: Look) -> SandboxResult<(PathBuf, Vec<Rect>)> {
        self.run.theme(look.dark)?;
        thread::sleep(SETTLE);
        let path = self
            .run
            .shot(self.sandbox, &format!("qa525-{name}-{}.png", look.name))?;
        Ok((path, cards_in(&self.run.last, look)))
    }

    /// Shots of `name` in Teams Light and Dark, with the grid checked in each; then Teams Light again. The cards in
    /// Teams Light.
    fn both_themes(&mut self, name: &str, grid: Grid) -> SandboxResult<Vec<Rect>> {
        let (dark_shot, dark) = self.shot_in(name, DARK)?;
        check_grid(&self.run.last, &dark, DARK, name, grid, &dark_shot);
        let (light_shot, light) = self.shot_in(name, LIGHT)?;
        check_grid(&self.run.last, &light, LIGHT, name, grid, &light_shot);
        Ok(light)
    }
}

/// What a message's grid must look like.
#[derive(Clone, Copy, Debug)]
struct Grid {
    cards: usize,
    rows: usize,
    /// The most cards in one row.
    across: usize,
}

// --- the grid ---

impl Walk<'_> {
    fn grids(&mut self) -> SandboxResult {
        for key in ["one", "two", "five", "eight", "text"] {
            let posted = self.posted(key)?.clone();
            let item = self.open_message(key)?;
            if item.unshown {
                note(format!("{key}: the message shows \"{UNSHOWN}\" beside its cards"));
            }
            if key == "text" && !item.text.contains("Made-up note over three files") {
                note(format!("text: the message's text is {:?}", item.text));
            }
            let grid = Grid {
                cards: posted.cards,
                rows: posted.cards.div_ceil(2),
                across: 2,
            };
            self.both_themes(key, grid)?;
            step(&format!("{key}: {} cards in {} rows", grid.cards, grid.rows));
        }
        Ok(())
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
    if let Some(row) = rows.iter().find(|row| row.len() > grid.across) {
        note(format!(
            "{what}: {} cards in one row, at most {} expected",
            row.len(),
            grid.across
        ));
    }
    check_sizes(cards, &what);
    check_gaps(&rows, &what);
    for card in cards {
        if !inside_bubble(shot, *card, look) {
            note(format!("{what}: the card {card:?} is not inside the message's bubble"));
        }
    }
    step(&format!(
        "{what}: cards {cards:?}, header {:?} px, bubble's inset right {:?} px",
        cards.first().map(|card| header_height(shot, *card)),
        cards
            .iter()
            .map(|card| card.x1)
            .max()
            .map(|right| inset_right(shot, right, cards, look))
    ));
}

/// Equal widths, Teams' width where the bubble has room for it.
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
        let (Some(upper), Some(lower)) = (pair[0].first(), pair[1].first()) else {
            continue;
        };
        let gap = lower.y0.saturating_sub(upper.y1);
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

// --- the actions ---

impl Walk<'_> {
    /// On the message of five files: a zip, two videos, a document and a picture with no preview.
    fn actions(&mut self) -> SandboxResult {
        self.open_message("five")?;
        let (_, cards) = self.shot_in("actions", LIGHT)?;
        let [zip, video, _, doc, _] = cards.as_slice() else {
            return Err(format!("the five cards are not on the screen: {cards:?}").into());
        };
        let (zip, video, doc) = (*zip, *video, *doc);
        self.names(doc, zip)?;
        self.share_slots(zip, video);
        self.play(video)?;
        self.share(video)?;
        self.more(doc)?;
        self.video_more(video)?;
        self.escape(zip)
    }

    /// "Download" of the document, then of the video, on the message of five files. The line under a card after a
    /// download (also on another card of the same file) makes a message taller and moves the cards, so this step is
    /// the last, and the cards are found again after the first one.
    fn downloads(&mut self) -> SandboxResult {
        self.open_message("five")?;
        let (_, cards) = self.shot_in("downloads", DARK)?;
        let doc = doc_of(&cards)?;
        if self
            .run
            .open_menu(self.sandbox, more_at(doc), "qa525-more-menu-dark.png")?
            .is_none()
        {
            note("More options on the document's card opens no menu in Teams Dark".to_owned());
        }
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        let doc = doc_of(&self.five_cards()?)?;
        self.download(doc, 1, 4, &DOC, MADE_UP_DOCUMENT)?;
        let video = video_of(&self.five_cards()?)?;
        if let Some(menu) = self.download(video, 0, 3, &CLIP_A, MADE_UP_VIDEO)?
            && menu.height() > 4 * 36
        {
            note(format!(
                "More options on a video's card has more than three rows: {menu:?}"
            ));
        }
        Ok(())
    }

    /// The cards of the message of five files, as the screen shows them now.
    fn five_cards(&mut self) -> SandboxResult<Vec<Rect>> {
        Ok(self.shot_in("downloads", LIGHT)?.1)
    }

    /// A click on a card's name opens the file's link.
    fn names(&mut self, doc: Rect, zip: Rect) -> SandboxResult {
        for (card, file, what) in [(doc, &DOC, "the document"), (zip, &PACK, "the zip")] {
            let (x, y) = name_at(card);
            let opened = self.run.click_opens(self.sandbox, x, y)?;
            if opened != share_url(file) {
                note(format!(
                    "a click on {what}'s name opens {opened}, expected {}",
                    share_url(file)
                ));
            }
        }
        step("a click on a card's name opens its link");
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

    /// A click on a video's picture plays it in the card; a second click pauses it. The made-up video is 2 s long:
    /// the second click comes as soon as it plays.
    fn play(&mut self, video: Rect) -> SandboxResult {
        let before = self.run.shot(self.sandbox, "qa525-video-before-light.png")?;
        let (x, y) = picture_at(video);
        self.run.click(x, y)?;
        let playing = self.run.players_until("a video playing", |states| {
            states
                .iter()
                .any(|state| state.contains("Playing") || state.contains("Failed"))
        });
        match playing {
            Ok(states) if states.iter().any(|state| state.contains("Playing")) => {
                self.run.click(x, y)?;
                if let Err(error) = self.run.players_until("the video paused", |states| {
                    states.iter().any(|state| state.contains("Paused"))
                }) {
                    note(format!(
                        "a second click on the playing video does not pause it: {error}"
                    ));
                }
            }
            Ok(states) => note(format!("a click on a video's picture: the player says {states:?}")),
            Err(error) => note(format!("a click on a video's picture plays nothing: {error}")),
        }
        let paused = self.run.shot(self.sandbox, "qa525-video-paused-light.png")?;
        let picture = Rect {
            y0: video.y0 + HEADER + 4,
            ..video
        };
        let moved = changed_in(&load(&before)?, &load(&paused)?, picture);
        if moved < 500 {
            note(format!(
                "the video's picture shows no frame of it after it played ({moved} pixels, {})",
                paused.display()
            ));
        }
        // played again to its end, for the shot of it while it plays
        self.run.click(x, y)?;
        self.run.shot(self.sandbox, "qa525-video-playing-light.png")?;
        thread::sleep(Duration::from_secs(2));
        step("a click plays the video in its card, a second click pauses it");
        Ok(())
    }

    /// Share on a video's card opens "Share this file" (task 543); Escape closes it.
    fn share(&mut self, video: Rect) -> SandboxResult {
        let (x, y) = share_at(video);
        self.run.click(x, y)?;
        self.share_dialog("Share on a video's card", "qa525-share-dialog-light.png")?;
        step("Share opens \"Share this file\"");
        Ok(())
    }

    /// "Share this file" is on the screen after `what` (the shot kept as `name`); Escape closes it.
    fn share_dialog(&mut self, what: &str, name: &str) -> SandboxResult {
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, name)?;
        if !dialog_shown(&self.run.last) {
            note(format!("{what} opens no \"Share this file\" ({name})"));
            return Ok(());
        }
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, &name.replace(".png", "-closed.png"))?;
        if dialog_shown(&self.run.last) {
            note(format!("Escape leaves \"Share this file\" open after {what}"));
            self.run.input.key(keys::ESC)?;
            thread::sleep(SETTLE);
        }
        Ok(())
    }

    /// "More options" on the document's card: Open in browser, Share and Copy link (Download: [`Self::downloads`]).
    fn more(&mut self, doc: Rect) -> SandboxResult {
        let link = share_url(&DOC);
        let rows = 4;
        let Some(menu) = self
            .run
            .open_menu(self.sandbox, more_at(doc), "qa525-more-menu-light.png")?
        else {
            note("More options on the document's card opens no menu".to_owned());
            return Ok(());
        };
        let (x, y) = row_at(menu, 0, rows);
        let opened = self.run.click_opens(self.sandbox, x, y)?;
        if opened != link {
            note(format!("More options' Open in browser opens {opened}, expected {link}"));
        }
        let Some(menu) = self
            .run
            .open_menu(self.sandbox, more_at(doc), "qa525-more-menu-light.png")?
        else {
            note("More options on the document's card opens no menu".to_owned());
            return Ok(());
        };
        let copied = self.run.click_copies(self.sandbox, row_at(menu, 3, rows))?;
        if copied != link {
            note(format!("More options' Copy link copies {copied:?}, expected {link}"));
        }
        self.more_share(doc, rows, 2, "the document")?;
        step("More options: Open in browser, Download, Share and Copy link");
        Ok(())
    }

    /// "More options" on a video's card: Share and Copy link, no "Open in browser" (Download: [`Self::downloads`]).
    fn video_more(&mut self, video: Rect) -> SandboxResult {
        let rows = 3;
        self.more_share(video, rows, 1, "a video")?;
        step("a video's More options: Download, Share and Copy link");
        Ok(())
    }

    /// "More options" → "Download" (row `row` of `rows`) on `card`: the engine reads `file` and saves its `bytes` in the
    /// app's downloads folder; the browser opens nothing.
    /// The menu it opened, if any.
    fn download(
        &mut self,
        card: Rect,
        row: u32,
        rows: u32,
        file: &MadeUpFile,
        bytes: &[u8],
    ) -> SandboxResult<Option<Rect>> {
        let shot = format!("qa525-more-download-{}-light.png", file.extension);
        let Some(menu) = self.run.open_menu(self.sandbox, more_at(card), &shot)? else {
            note(format!("More options on {} opens no menu", file.name));
            return Ok(None);
        };
        let before = self.sandbox.opened()?.len();
        let (x, y) = row_at(menu, row, rows);
        self.run.click(x, y)?;
        self.run.input.move_to(self.run.rest.0, self.run.rest.1)?;
        let target = self.run.dir.join("downloads").join(file.name);
        let saved = wait_until(WAIT, || fs::read(&target).ok().filter(|read| read.len() == bytes.len()));
        thread::sleep(SETTLE);
        self.run
            .shot(self.sandbox, &format!("qa525-downloaded-{}-light.png", file.extension))?;
        match saved {
            None => note(format!(
                "Download of {} saves no file of {} bytes at {} in {WAIT:?}",
                file.name,
                bytes.len(),
                target.display()
            )),
            Some(read) if read != bytes => note(format!("Download of {} saves other bytes", file.name)),
            Some(_) => step(&format!("Download of {}: saved through the engine", file.name)),
        }
        if self.sandbox.opened()?.len() != before {
            note(format!("Download of {} opened a link in the browser", file.name));
        }
        Ok(Some(menu))
    }

    /// "More options" → "Share" (row `row` of `rows`) on `card` opens "Share this file" (task 543).
    fn more_share(&mut self, card: Rect, rows: u32, row: u32, what: &str) -> SandboxResult {
        let Some(menu) = self
            .run
            .open_menu(self.sandbox, more_at(card), "qa525-more-share-light.png")?
        else {
            note(format!("More options on {what}'s card opens no menu"));
            return Ok(());
        };
        let (x, y) = row_at(menu, row, rows);
        self.run.click(x, y)?;
        self.share_dialog(
            &format!("More options' Share on {what}"),
            "qa525-more-share-dialog-light.png",
        )
    }

    /// Escape closes "More options".
    fn escape(&mut self, zip: Rect) -> SandboxResult {
        let before = self.run.shot(self.sandbox, "qa525-escape-before-light.png")?;
        let Some(menu) = self
            .run
            .open_menu(self.sandbox, more_at(zip), "qa525-escape-open-light.png")?
        else {
            note("More options on the zip's card opens no menu".to_owned());
            return Ok(());
        };
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        let after = self.run.shot(self.sandbox, "qa525-escape-closed-light.png")?;
        let left = changed_in(&load(&before)?, &load(&after)?, menu);
        if left > 200 {
            note(format!(
                "Escape leaves the menu open ({left} pixels changed, {})",
                after.display()
            ));
        }
        step("Escape closes More options");
        Ok(())
    }
}

/// Whether "Share this file" is on the screen: its 600 px box's surface in the middle of the screen, over the dimmed
/// window (Teams Light).
fn dialog_shown(shot: &image::RgbImage) -> bool {
    let (x, y) = (SCREEN.0 / 2 - 295, SCREEN.1 / 2);
    let dimmed = at(shot, x - 10, y).iter().all(|&channel| channel < 200);
    near(at(shot, x, y), LIGHT.surface, 3) && dimmed
}

/// Where a card's name is: right of its icon, on the name's line.
const fn name_at(card: Rect) -> (u32, u32) {
    (card.x0 + 100, card.y0 + 24)
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

/// How far from its button a menu is looked for on the screen: more than a menu is wide or tall.
const MENU_REACH: u32 = 280;

/// The middle of row `row` of `rows` in a menu drawn as `menu`.
const fn row_at(menu: Rect, row: u32, rows: u32) -> (u32, u32) {
    (
        menu.x0 + menu.width() / 2,
        menu.y0 + menu.height() * (2 * row + 1) / (2 * rows),
    )
}

// --- odd shapes ---

impl Walk<'_> {
    fn odd_shapes(&mut self) -> SandboxResult {
        for key in ["surrogate", "no-type", "no-link", "twice"] {
            let posted = self.posted(key)?.clone();
            let item = self.open_message(key)?;
            let wanted = key == "no-link";
            if item.unshown != wanted {
                note(format!(
                    "{key}: \"{UNSHOWN}\" shown {}, expected {wanted}; the files read {:?}",
                    item.unshown, item.files
                ));
            }
            let grid = Grid {
                cards: posted.cards,
                rows: posted.cards.div_ceil(2),
                across: 2,
            };
            let cards = self.both_themes(key, grid)?;
            if key == "no-link"
                && let Some(card) = cards.get(1)
            {
                self.linkless(*card)?;
            }
            step(&format!("{key}: the files {:?}", item.files));
        }
        Ok(())
    }

    /// The card of a file with no SharePoint link: no action, its name opens nothing, no "More options".
    fn linkless(&mut self, card: Rect) -> SandboxResult {
        let (x, y) = name_at(card);
        if let Some(opened) = self.run.click_opens_quietly(self.sandbox, x, y)? {
            note(format!(
                "a click on the name of a file with no SharePoint link opens {opened}"
            ));
        }
        if self
            .run
            .open_menu(self.sandbox, more_at(card), "qa525-no-link-more-light.png")?
            .is_some()
        {
            note("a file with no SharePoint link has a menu on its card".to_owned());
            self.run.input.key(keys::ESC)?;
        }
        step("a file with no SharePoint link: its card has no action");
        Ok(())
    }
}

// --- a narrow window, a thread ---

impl Walk<'_> {
    /// The compositor sizes the main window to `width` wide, as a user's drag does.
    fn resize(&self, width: u32) -> SandboxResult {
        let sandbox = self.sandbox;
        sandbox.niri_msg(&["action", "set-column-width", &width.to_string()])?;
        wait_until(WAIT, || {
            let text = sandbox.niri_msg(&["-j", "windows"]).ok()?;
            let windows: Value = serde_json::from_str(&text).ok()?;
            windows
                .as_array()?
                .iter()
                .any(|window| window["layout"]["window_size"][0].as_f64() == Some(f64::from(width)))
                .then_some(())
        })
        .ok_or_else(|| format!("the window never took {width} px"))?;
        thread::sleep(SETTLE * 2);
        Ok(())
    }

    /// The message of text over three files: one row each fits the narrow chat's height.
    fn narrow(&mut self) -> SandboxResult {
        let posted = self.posted("text")?.clone();
        self.resize(NARROW)?;
        self.open_message("text")?;
        let one_across = Grid {
            cards: posted.cards,
            rows: posted.cards,
            across: 1,
        };
        let cards = self.both_themes("narrow", one_across)?;
        if let Some(card) = cards.iter().find(|card| card.x1 > NARROW - 4) {
            note(format!(
                "narrow: the card {card:?} reaches past the window's right edge at {NARROW}"
            ));
        }
        self.resize(SCREEN.0)?;
        let wide = Grid {
            cards: posted.cards,
            rows: posted.cards.div_ceil(2),
            across: 2,
        };
        self.both_themes("wide-again", wide)?;
        step("a narrow window: one card in a row; wide again: two");
        Ok(())
    }

    fn thread(&mut self) -> SandboxResult {
        let post = self.service.channel_post.clone();
        self.run.open_channel()?;
        self.run.until("posts", "the post with files", |state| {
            state.item(&post).is_some_and(|item| item.files.len() == 3)
        })?;
        let answer = self.run.question(&format!("thread {post}"))?;
        step(&format!("the thread: {answer}"));
        let state = self.run.until("thread", "the thread's post", |state| {
            state.item(&post).is_some_and(|item| item.files.len() == 3)
        })?;
        if state.item(&post).is_some_and(|item| item.unshown) {
            note(format!("the channel post shows \"{UNSHOWN}\" in its thread"));
        }
        thread::sleep(SETTLE * 2);
        for look in [DARK, LIGHT] {
            let (path, cards) = self.shot_in("thread", look)?;
            let pane = SCREEN.0 - THREAD_WIDTH;
            let (beside, posts): (Vec<Rect>, Vec<Rect>) = cards.iter().partition(|card| card.x0 + 4 >= pane);
            let what = format!("the thread ({}, {})", look.name, path.display());
            if beside.len() != 3 {
                note(format!(
                    "{what}: {} cards in the thread pane, expected 3: {beside:?}",
                    beside.len()
                ));
            }
            if posts.len() != 3 {
                note(format!(
                    "{what}: {} cards in the posts, expected 3: {posts:?}",
                    posts.len()
                ));
            }
            for card in cards.iter().filter(|card| !inside_bubble(&self.run.last, **card, look)) {
                note(format!("{what}: the card {card:?} is not inside its message's bubble"));
            }
        }
        step("a channel post's files beside its thread");
        Ok(())
    }
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

/// How far the bubble's surface goes right of the rightmost card, at its header.
fn inset_right(shot: &image::RgbImage, right: u32, cards: &[Rect], look: Look) -> u32 {
    let y = cards
        .iter()
        .find(|card| card.x1 == right)
        .map_or(0, |card| card.y0 + HEADER / 2);
    (right..shot.width())
        .take_while(|&x| near(at(shot, x, y), look.surface, 4))
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
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
    count_in(shot, region, |pixel| !near(pixel, fill, 40))
}

fn count_in(shot: &image::RgbImage, region: Rect, taken: impl Fn([u8; 3]) -> bool) -> usize {
    (region.y0..region.y1.min(shot.height()))
        .flat_map(|y| (region.x0..region.x1.min(shot.width())).map(move |x| (x, y)))
        .filter(|&(x, y)| taken(at(shot, x, y)))
        .count()
}

fn changed_in(before: &image::RgbImage, after: &image::RgbImage, region: Rect) -> usize {
    (region.y0..region.y1.min(before.height()))
        .flat_map(|y| (region.x0..region.x1.min(before.width())).map(move |x| (x, y)))
        .filter(|&(x, y)| !near(at(before, x, y), at(after, x, y), 8))
        .count()
}

/// The bounds of the pixels that changed in `within`, but in `except`. A changed line across all of `within` is the
/// edge of the message list's focus ring, which goes when a menu takes the keys: it is no part of the box.
fn changed_box(before: &image::RgbImage, after: &image::RgbImage, within: Rect, except: Rect) -> Option<Rect> {
    let x1 = within.x1.min(before.width()).min(after.width());
    let y1 = within.y1.min(before.height()).min(after.height());
    let changed = |x: u32, y: u32| {
        let skipped = except.x0 <= x && x < except.x1 && except.y0 <= y && y < except.y1;
        !skipped && !near(at(before, x, y), at(after, x, y), 8)
    };
    let across = |count: usize, length: u32| count * 10 >= length as usize * 9;
    let line_columns: Vec<u32> = (within.x0..x1)
        .filter(|&x| across((within.y0..y1).filter(|&y| changed(x, y)).count(), y1 - within.y0))
        .collect();
    let line_rows: Vec<u32> = (within.y0..y1)
        .filter(|&y| across((within.x0..x1).filter(|&x| changed(x, y)).count(), x1 - within.x0))
        .collect();
    let mut bounds: Option<Rect> = None;
    for y in (within.y0..y1).filter(|y| !line_rows.contains(y)) {
        for x in (within.x0..x1).filter(|x| !line_columns.contains(x)) {
            if !changed(x, y) {
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

/// A made-up file c shares: its name, its extension, its sharing link's kind and id, and for a video the colours its
/// made-up thumbnail is drawn in.
struct MadeUpFile {
    name: &'static str,
    extension: &'static str,
    kind: char,
    share: &'static str,
    frame: Option<([u8; 3], [u8; 3])>,
}

const DOC: MadeUpFile = MadeUpFile {
    name: "made-up-budget-notes.docx",
    extension: "docx",
    kind: 'w',
    share: "MadeUpShareLinkBudget0101",
    frame: None,
};
const CLIP_A: MadeUpFile = MadeUpFile {
    name: "01-made-up-stage-walk.mp4",
    extension: "mp4",
    kind: 'v',
    share: "MadeUpShareLinkStageWalk0102",
    frame: Some(([24, 120, 40], [170, 230, 120])),
};
const CLIP_B: MadeUpFile = MadeUpFile {
    name: "02-made-up-sound-check.mp4",
    extension: "mp4",
    kind: 'v',
    share: "MadeUpShareLinkSoundCheck0103",
    frame: Some(([14, 18, 32], [60, 90, 200])),
};
const CLIP_C: MadeUpFile = MadeUpFile {
    name: "03-made-up-crowd.mp4",
    extension: "mp4",
    kind: 'v',
    share: "MadeUpShareLinkCrowd0104",
    frame: Some(([90, 20, 30], [240, 120, 90])),
};
const PACK: MadeUpFile = MadeUpFile {
    name: "made-up-pack.zip",
    extension: "zip",
    kind: 'u',
    share: "MadeUpShareLinkPack0105",
    frame: None,
};
const PHOTO: MadeUpFile = MadeUpFile {
    name: "made-up-floor-photo.png",
    extension: "png",
    kind: 'i',
    share: "MadeUpShareLinkFloorPhoto0106",
    frame: None,
};
const PLAN: MadeUpFile = MadeUpFile {
    name: "made-up-plan.pdf",
    extension: "pdf",
    kind: 'b',
    share: "MadeUpShareLinkPlan0107",
    frame: None,
};
const SHEET: MadeUpFile = MadeUpFile {
    name: "made-up-rota.xlsx",
    extension: "xlsx",
    kind: 'x',
    share: "MadeUpShareLinkRota0108",
    frame: None,
};
const SLIDES: MadeUpFile = MadeUpFile {
    name: "made-up-pitch.pptx",
    extension: "pptx",
    kind: 'p',
    share: "MadeUpShareLinkPitch0109",
    frame: None,
};
/// A video whose links are on a host that is not SharePoint.
const ELSEWHERE: MadeUpFile = MadeUpFile {
    name: "04-made-up-elsewhere.mp4",
    extension: "mp4",
    kind: 'v',
    share: "MadeUpShareLinkElsewhere0110",
    frame: None,
};

const SITE: &str = "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test/";

/// A file's entry in `properties.files`, in the shape `tests/fixtures/chatsvc/files.json` has (made up).
fn file_entry(file: &MadeUpFile, at: usize) -> Value {
    let object = format!("{SITE}Documents/Microsoft Teams Chat Files/{}", file.name);
    json!({
        "itemid": format!("made-up-item-{at}"),
        "fileName": file.name,
        "fileType": file.extension,
        "fileInfo": {"fileUrl": object, "siteUrl": SITE, "shareUrl": share_url(file)},
        "@type": "http://schema.skype.com/File",
        "version": 2,
        "objectUrl": object,
        "type": file.extension,
        "title": file.name,
        "state": "active",
        "filePreview": {"previewUrl": "", "previewHeight": 0, "previewWidth": 0},
    })
}

/// Its link, in the short form of a link for specific people Teams posts (task 793): Graph answers it only redeemed.
fn share_url(file: &MadeUpFile) -> String {
    format!(
        "https://contoso-my.sharepoint.com/:{}:/p/made_up_user_contoso_test/{}",
        file.kind, file.share
    )
}

/// `files` as `properties.files` carries them: a JSON string of their list.
fn files_value(files: &[&MadeUpFile]) -> Value {
    Value::String(entries(files).to_string())
}

fn entries(files: &[&MadeUpFile]) -> Value {
    Value::Array((0..).zip(files).map(|(at, file)| file_entry(file, at)).collect())
}

/// Three files, the third's name with a lone surrogate escape (`\ud800`): a string `serde_json` refuses.
fn lone_surrogate() -> Value {
    let mut list = entries(&[&DOC, &CLIP_A, &PLAN]);
    list[2]["fileName"] = json!("made-up-@@LONE@@-notes.pdf");
    list[2]["title"] = json!("made-up-@@LONE@@-notes.pdf");
    let text = list.to_string().replace("@@LONE@@", "\\ud800");
    Value::String(text)
}

/// Three files, the second's entry with no `@type`.
fn no_type() -> Value {
    let mut list = entries(&[&DOC, &SHEET, &CLIP_B]);
    if let Some(entry) = list[1].as_object_mut() {
        entry.remove("@type");
    }
    Value::String(list.to_string())
}

/// Three files, the second's links on a host that is not SharePoint.
fn no_link() -> Value {
    let mut list = entries(&[&DOC, &ELSEWHERE, &CLIP_A]);
    let elsewhere = "https://files.example.test/made-up/04-made-up-elsewhere.mp4";
    list[1]["objectUrl"] = json!(elsewhere);
    list[1]["fileInfo"]["fileUrl"] = json!(elsewhere);
    list[1]["fileInfo"]["shareUrl"] = json!(elsewhere);
    Value::String(list.to_string())
}

/// Three files, their list encoded twice: a JSON string of a JSON string of the list.
fn twice() -> Value {
    let once = entries(&[&PACK, &CLIP_B, &DOC]).to_string();
    Value::String(Value::String(once).to_string())
}

// --- the service ---

/// A message c posted: its chat, its id and how many cards it shows.
#[derive(Clone, Debug)]
struct Posted {
    key: &'static str,
    chat: String,
    id: String,
    cards: usize,
}

struct Service {
    mock: ChatServiceMock,
    runtime: tokio::runtime::Handle,
    trouter: TrouterMock,
    posted: Vec<Posted>,
    channel_post: String,
}

impl Service {
    fn start(runtime: &tokio::runtime::Runtime) -> SandboxResult<Self> {
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        let text = "<p>Made-up note over three files: the walk, the plan and the pack.</p>";
        let messages: [(&str, &str, &str, Value, usize); 11] = [
            ("one", "Made-up one file", "", files_value(&[&DOC]), 1),
            ("two", "Made-up two videos", "", files_value(&[&CLIP_A, &CLIP_B]), 2),
            (
                "five",
                "Made-up five files",
                "",
                files_value(&[&PACK, &CLIP_A, &CLIP_B, &DOC, &PHOTO]),
                5,
            ),
            ("eight", "Made-up eight files", "", eight(), 8),
            (
                "text",
                "Made-up text and files",
                text,
                files_value(&[&CLIP_C, &PLAN, &PACK]),
                3,
            ),
            ("surrogate", "Made-up odd name", "", lone_surrogate(), 3),
            ("no-type", "Made-up no type", "", no_type(), 3),
            ("no-link", "Made-up no link", "", no_link(), 3),
            ("twice", "Made-up twice encoded", "", twice(), 3),
            ("lone-video", "Made-up lone video", "", files_value(&[&LONE_CLIP]), 1),
            ("refused", "Made-up refused video", "", files_value(&[&REFUSED]), 1),
        ];
        let mut posted = Vec::new();
        for (key, topic, html, files, cards) in messages {
            let chat = mock.add_group(topic, &[LAB_C, LAB_A]).ok_or("no group made")?;
            let id = post_files(&mock, &chat, html, files)?;
            posted.push(Posted { key, chat, id, cards });
        }
        posted.extend(card_keys::post(&mock, runtime)?);
        let channel_post = channel_post(&mock)?;
        for (at, file) in [&CLIP_A, &CLIP_B, &CLIP_C, &LONE_CLIP, &DOC].into_iter().enumerate() {
            serve_file(&mock, runtime, file, at)?;
        }
        video::serve_refused(&mock, runtime)?;
        video::serve_recap(&mock, runtime)?;
        let _ = mock.take_events(LAB_A);
        Ok(Self {
            mock,
            runtime: runtime.handle().clone(),
            trouter,
            posted,
            channel_post,
        })
    }
}

fn eight() -> Value {
    files_value(&[&PACK, &CLIP_A, &DOC, &PHOTO, &PLAN, &SHEET, &SLIDES, &CLIP_C])
}

/// c's post of `html` in `chat`, `properties.files` set to `files`; its id.
fn post_files(mock: &ChatServiceMock, chat: &str, html: &str, files: Value) -> SandboxResult<String> {
    let id = mock.post_as(LAB_C, chat, html).ok_or("c's post refused")?;
    let client_message_id = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
    let changed = mock.change_message(chat, &client_message_id, |message| {
        message["properties"]["files"] = files;
    });
    changed.then_some(id).ok_or_else(|| "c's post isn't in the mock".into())
}

/// c's post with three files in the team's channel, and b's reply under it; the post's id.
fn channel_post(mock: &ChatServiceMock) -> SandboxResult<String> {
    let html = "<p>Made-up rehearsal files for the channel</p>";
    let post = post_files(mock, TEAM_CHANNEL, html, files_value(&[&CLIP_A, &DOC, &PACK]))?;
    mock.add_message(
        &format!("{TEAM_CHANNEL};messageid={post}"),
        (LAB_B, "Lab Bee"),
        "RichText/Html",
        "<p>Made-up thanks, I will look at them</p>",
    )
    .ok_or("b's reply refused")?;
    Ok(post)
}

/// The mock's thumbnail of video `file` (a made-up picture) and the file itself, read by ranges. `at` makes its links
/// its own.
fn serve_file(
    mock: &ChatServiceMock,
    runtime: &tokio::runtime::Runtime,
    file: &MadeUpFile,
    at: usize,
) -> SandboxResult {
    let read = teams_proto::FileThumbnails::of(&share_url(file)).ok_or("a made-up link no read names")?;
    let token = read.share();
    let Some((dark, light)) = file.frame else {
        serve_content(mock, runtime, file, token, at, MADE_UP_DOCUMENT);
        return Ok(());
    };
    let picture = format!("/transform/thumbnail/made-up-qa525-{at}");
    let list = json!({"value": [{"id": "0", "large": {
        "height": 450, "width": 800,
        "url": format!("https://westus1-mediap.svc.ms{picture}?provider=spo&docid=made-up"),
    }}]});
    runtime.block_on(mock.serve_redeemed(
        &format!("/v1.0/shares/{token}/driveItem/thumbnails"),
        "application/json",
        list.to_string().into_bytes(),
    ));
    let mut png = Vec::new();
    made_up_frame(dark, light).write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    runtime.block_on(mock.serve_bytes(&picture, "image/png", png));
    serve_content(mock, runtime, file, token, at, MADE_UP_VIDEO);
    Ok(())
}

/// The mock's item of `file` (its sharing token `token`) and its `bytes` at the download link the item gives.
fn serve_content(
    mock: &ChatServiceMock,
    runtime: &tokio::runtime::Runtime,
    file: &MadeUpFile,
    token: &str,
    at: usize,
    bytes: &[u8],
) {
    let download = format!("/personal/made_up_user_contoso_test/_layouts/15/download-qa525-{at}.aspx");
    let item = json!({
        "id": format!("made-up-item-{at}"),
        "name": file.name,
        "size": bytes.len(),
        "@microsoft.graph.downloadUrl": format!("https://contoso-my.sharepoint.com{download}?UniqueId=made-up-{at}"),
    });
    runtime.block_on(mock.serve_redeemed(
        &format!("/v1.0/shares/{token}/driveItem"),
        "application/json",
        item.to_string().into_bytes(),
    ));
    runtime.block_on(mock.serve_ranged(&download, "application/octet-stream", bytes.to_vec()));
}

/// The document's card among the five cards (a zip, two videos, the document and a picture).
fn doc_of(cards: &[Rect]) -> SandboxResult<Rect> {
    cards
        .get(3)
        .copied()
        .ok_or_else(|| format!("the five cards are not on the screen: {cards:?}").into())
}

/// The first video's card among the five cards.
fn video_of(cards: &[Rect]) -> SandboxResult<Rect> {
    cards
        .get(1)
        .copied()
        .ok_or_else(|| format!("the five cards are not on the screen: {cards:?}").into())
}

/// Made-up bytes of the document (synthetic).
const MADE_UP_DOCUMENT: &[u8] = b"made-up budget notes, every byte synthetic";

/// A made-up test pattern video (synthetic).
const MADE_UP_VIDEO: &[u8] = include_bytes!("../../../../teams-media/tests/fixtures/media/made-up-test-pattern.mp4");

/// A made-up video frame, 800 by 450: a gradient from `dark` to `light` with a bar.
fn made_up_frame(dark: [u8; 3], light: [u8; 3]) -> image::RgbaImage {
    image::RgbaImage::from_fn(800, 450, |x, y| {
        let mix = |from: u8, to: u8| {
            let (from, to) = (u32::from(from), u32::from(to));
            let value = (from * (800 - x) + to * x) / 800;
            u8::try_from(value).unwrap_or(u8::MAX)
        };
        if (360..440).contains(&x) && y > 120 {
            image::Rgba([10, 10, 10, 255])
        } else {
            image::Rgba([
                mix(dark[0], light[0]),
                mix(dark[1], light[1]),
                mix(dark[2], light[2]),
                255,
            ])
        }
    })
}

// --- what the app shows ---

/// A message's row as the child tells it.
#[derive(Clone, Debug, serde::Deserialize)]
struct Item {
    id: String,
    text: String,
    files: Vec<String>,
    unshown: bool,
}

/// A view's rows as the child tells them.
#[derive(Debug, serde::Deserialize)]
struct State {
    items: Vec<Item>,
}

impl State {
    fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|item| item.id == id)
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

    /// Waits for a line, not seen before, that `found` takes, named `what` in the error.
    fn next_where(&mut self, what: &str, found: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .map_err(|_| format!("no trace line {what}; the last seen {:?}", self.seen.last()))?;
            self.seen.push(line.clone());
            if found(&line) {
                return Ok(line);
            }
        }
    }
}

/// The app running in the sandbox: its process, its view trace, its control socket and the user's pointer.
struct AppRun {
    running: Running,
    trace: TraceReader,
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
    /// Starts the app on `service` with its store and settings in `dir`; once its window is drawn.
    fn launch(sandbox: &Sandbox, service: &Service, dir: &Path) -> SandboxResult<Self> {
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let token = service.mock.token(LAB_A).ok_or("the mock's token for a")?;
        let origin = service.mock.origin();
        let dir_text = dir.to_string_lossy().into_owned();
        let downloads = dir.join("downloads");
        fs::create_dir_all(&downloads)?;
        let downloads_text = downloads.to_string_lossy().into_owned();
        let child = sandbox.spawn_with_fds(
            &std::env::current_exe()?,
            &[],
            &[
                (CHILD_ENV, "app"),
                (TRACE_FD_ENV, &fd),
                (ORIGIN_ENV, &origin),
                (TROUTER_ENV, service.trouter.origin()),
                (TOKEN_ENV, &token),
                (DIR_ENV, &dir_text),
                ("XDG_DOWNLOAD_DIR", &downloads_text),
            ],
            &[writer.as_fd()],
        )?;
        drop(writer);
        let mut running = Running::new(child)?;
        let trace = TraceReader::start(reader);
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
        let ctl = dir.join("ctl.sock");
        wait_until(WAIT, || Client::connect(&ctl, Timeouts::default()).ok()).ok_or("no control socket")?;
        let mut run = Self {
            running,
            trace,
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

    /// `ngteamsctl open chat <chat>`, then the trace's `chat opened`.
    fn open(&mut self, chat: &str) -> SandboxResult {
        let mut client = Client::connect(&self.ctl, Timeouts::default())?;
        client.request(
            None,
            Request::Open {
                target: OpenTarget::Chat { chat: chat.to_owned() },
            },
        )?;
        self.trace.next_where(&format!("chat {chat} opened"), |line| {
            line["view"] == "chat" && line["change"] == "opened" && line["conversation"] == chat
        })?;
        Ok(())
    }

    /// `ngteamsctl open channel <team channel>`.
    fn open_channel(&mut self) -> SandboxResult {
        let mut client = Client::connect(&self.ctl, Timeouts::default())?;
        client.request(
            None,
            Request::Open {
                target: OpenTarget::Channel {
                    channel: TEAM_CHANNEL.to_owned(),
                },
            },
        )?;
        Ok(())
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

    /// The rows of `view` (`chat`, `posts` or `thread`).
    fn state_in(&mut self, view: &str) -> SandboxResult<State> {
        let answer = self.question(&format!("state {view}"))?;
        serde_json::from_str(&answer).map_err(|error| format!("the state {answer}: {error}").into())
    }

    /// Waits up to [`WAIT`] for a state of `view` that `wanted` takes, named `what` in the error.
    fn until(&mut self, view: &str, what: &str, wanted: impl Fn(&State) -> bool) -> SandboxResult<State> {
        let start = Instant::now();
        loop {
            let state = self.state_in(view)?;
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
        fs::copy(&png, shots_dir()?.join(name))?;
        if let Some(evidence) = std::env::var_os(EVIDENCE_ENV) {
            let evidence = PathBuf::from(evidence);
            fs::create_dir_all(&evidence)?;
            fs::copy(&png, evidence.join(name))?;
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

    /// Clicks (`x`, `y`) as a user does; the link the app asked the opener for.
    fn click_opens(&mut self, sandbox: &Sandbox, x: u32, y: u32) -> SandboxResult<String> {
        let before = sandbox.opened()?.len();
        self.input.click(x, y, keys::BTN_LEFT)?;
        let opened = wait_until(WAIT, || sandbox.opened().ok()?.get(before).cloned())
            .ok_or_else(|| format!("a click at ({x}, {y}) opened nothing"))?;
        self.input.move_to(self.rest.0, self.rest.1)?;
        thread::sleep(Duration::from_millis(300));
        opened
            .args
            .last()
            .cloned()
            .ok_or_else(|| format!("the opener got no address: {opened:?}").into())
    }

    /// Clicks (`x`, `y`) as a user does; what the app asked the opener for in [`QUIET`], when anything.
    fn click_opens_quietly(&mut self, sandbox: &Sandbox, x: u32, y: u32) -> SandboxResult<Option<String>> {
        let before = sandbox.opened()?.len();
        self.click(x, y)?;
        thread::sleep(QUIET);
        self.input.move_to(self.rest.0, self.rest.1)?;
        Ok(sandbox.opened()?.get(before).map(|opened| opened.args.join(" ")))
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
        // the pointer's rest on the title bar can show a tooltip too, and the pointer on the message its bar above the
        // card: the menu is the box near the button, under it where it fits, else over it
        let reach = around.grown(MENU_REACH);
        let under = Rect { y0: around.y0, ..reach };
        let over = Rect { y1: around.y1, ..reach };
        Ok([under, over].into_iter().find_map(|within| {
            changed_box(&before, &after, within, around).filter(|menu| menu.height() > 20 && menu.width() > 60)
        }))
    }

    /// The error, with the last screen and the app's stderr.
    fn fail(&mut self, sandbox: &Sandbox, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
        let shot = self
            .shot(sandbox, "qa525-failed.png")
            .map_or_else(|_| String::new(), |path| path.display().to_string());
        self.running.fail(&format!("{error} (last screen: {shot})"))
    }

    /// Quits through the tray menu, as a user does; once the app ended and released its engine.
    fn quit(mut self, sandbox: &Sandbox) -> SandboxResult {
        let name = format!("org.kde.StatusNotifierItem-{}-1", self.running.pid());
        let host = sandbox.bus().connect()?;
        let menu = zbus::blocking::Proxy::new(&host, name.as_str(), MENU_PATH, "com.canonical.dbusmenu")?;
        let quit = || menu.call_method("Event", &(MENU_QUIT, "clicked", zbus::zvariant::Value::from(""), 0_u32));
        if wait_until(Duration::from_secs(10), || quit().ok()).is_none() {
            step("no tray item to quit through: the app is stopped");
            let _ = self.running.fail(&"stopped by the walk");
            return Ok(());
        }
        self.running
            .line_where(WAIT, |line| line == "engine-released")
            .map_err(|error| self.running.fail(&format!("the engine wasn't released: {error:?}")))?;
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

/// Where the screenshots are kept: `target/ui-shots/`, never committed.
fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

// --- the child ---

/// One account's mock token, for the transport and the push channel alike.
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

/// a's engine on the parent's mock, its store in `dir`.
fn engine_on_the_mock(dir: &Path) -> SandboxResult<EngineHandle> {
    let origin = std::env::var(ORIGIN_ENV)?;
    let tokens = Arc::new(Tokens {
        token: std::env::var(TOKEN_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
    let transport = Arc::new(http()?);
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
            http()?.with_ams(AMS_BASE)?.with_csa(CSA_BASE)?.with_mcps(MCPS_BASE)?,
        )),
        outbox: Outbox::new(transport, author),
        push: Some(Push::new(trouter)),
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })?)
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

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let fd: RawFd = std::env::var(TRACE_FD_ENV)?.parse()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(fd)?);
    let dir = PathBuf::from(std::env::var(DIR_ENV)?);
    let engine = engine_on_the_mock(&dir)?;
    let first = support::started(Alias::A, engine)?;
    let settings = teams_ui::SettingsFile::new(dir.join("ui").join("a.toml"));
    let runner = support::runner(&dir)?;
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let ctl = dir.join("ctl.sock");
    let theme_file = dir.join(THEME_FILE);
    let ask = dir.join(ASK);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: Vec::new(),
                trace: Arc::new(trace),
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
                follow_theme_file(&settings, theme_file.clone(), cx);
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
    let store = Store::open(&support::store_file(&dir, Alias::A))?;
    if store.engine_lock().is_ok() {
        say("engine-released");
    }
    Ok(())
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

fn on_shell(shell: &Entity<Shell>, verb: &str, window: &mut Window, cx: &mut App) -> String {
    let (verb, rest) = verb.split_once(' ').unwrap_or((verb, ""));
    match verb {
        "state" => view_of(shell, rest, cx).map_or_else(|| json!({"items": []}), |view| state_of(view.read(cx))),
        "thread" => match shell.read(cx).channel().cloned() {
            Some(channel) => {
                channel.update(cx, |channel, cx| channel.open_thread(rest.to_owned(), window, cx));
                json!("opened")
            }
            None => json!("no channel"),
        },
        "players" => {
            let states: Vec<String> = teams_ui::video_player::players(cx)
                .iter()
                .map(|player| {
                    let player = player.read(cx);
                    format!("{} {:?}", player.id(), player.state())
                })
                .collect();
            json!(states)
        }
        "tab" => json!(format!("{:?}", shell.read(cx).chat_tab())),
        "focus" => json!(focus_of(shell, window, cx)),
        "control" => json!(control_of(window, cx)),
        other => json!(format!("unknown {other}")),
    }
    .to_string()
}

/// Where the keys are: the open chat's `list` or `box`, another element, or `none`.
fn focus_of(shell: &Entity<Shell>, window: &Window, cx: &App) -> &'static str {
    if let Some(chat) = shell.read(cx).chat() {
        let chat = chat.read(cx);
        if chat.focus_handle().is_focused(window) {
            return "list";
        }
        if chat.compose().read(cx).focused(window, cx) {
            return "box";
        }
    }
    if window.focused(cx).is_some() { "other" } else { "none" }
}

/// The control of a video player that has the keys (`play`, `fullscreen`..), `player` for another place in a player
/// (its picture, "More options"), else `outside`.
fn control_of(window: &Window, cx: &App) -> &'static str {
    const NAMES: [&str; 9] = [
        "play",
        "back",
        "forward",
        "mute",
        "captions",
        "speed",
        "settings",
        "fullscreen",
        "open",
    ];
    let mut place = "outside";
    for player in teams_ui::video_player::players(cx) {
        let player = player.read(cx);
        if let Some(name) = NAMES
            .into_iter()
            .find(|name| player.control_focus(name, cx).is_focused(window))
        {
            return name;
        }
        if player.holds_keys(window, cx) {
            place = "player";
        }
    }
    place
}

/// The view `which` names: the channel's `posts`, its `thread`, else the chat open.
fn view_of(shell: &Entity<Shell>, which: &str, cx: &App) -> Option<Entity<ChatView>> {
    let shell = shell.read(cx);
    match which {
        "posts" => Some(shell.channel()?.read(cx).posts().clone()),
        "thread" => Some(shell.channel()?.read(cx).thread()?.1.clone()),
        _ => shell.chat().cloned(),
    }
}

/// The view's rows, as [`State`] reads them.
fn state_of(view: &ChatView) -> Value {
    let items: Vec<Value> = view.message_items().map(item_of).collect();
    json!({ "items": items })
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
    json!({ "id": item.id, "text": text, "files": files, "unshown": item.unshown.any() })
}
