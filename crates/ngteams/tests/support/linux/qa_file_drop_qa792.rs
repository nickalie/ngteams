//! QA of task 792 (round 1), in the same window: files sent in a 1:1 chat through the account's own OneDrive, as a
//! user sends them. The mock's drive answers an upload as SharePoint does: an Office file's `webUrl` is its `Doc.aspx`
//! viewer link and its `webDavUrl` the path; another file has the path in both.
//!
//! - **Office files and others**: an .xlsx (Attach), a .docx, a .pptx, a .md and a .png (each dropped), one message
//!   each. Each sends; the message names the file by its path (never `Doc.aspx`); the file is shared with b alone;
//!   the card shows in a's chat with the file's name. Light and Dark shots of the sent Office card.
//! - **The receiver**: b's app shows the five cards, and a click on a card opens the file's path.
//! - **A slow upload**: a file of some MB whose upload answer comes after 45 s (past the old 30 s limit) sends.
//! - **No answer**: an upload that never answers gives the timeout text after the file's limit; Retry uploads it again,
//!   and it sends.
//! - **A bad path**: an upload answer whose `webDavUrl` is outside the upload site, or has a query: the card says the
//!   file details couldn't be read; Send refuses with the user's words; no share and no message go.

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use teams_mock::{AB_CHAT, DRIVE_HOST, DriveTwist, LAB_B};
use ui_sandbox::{SandboxResult, wait_until};

use super::{
    CALM, DOLPHIN, POLL, SETTLE, User, WAIT, box_centre, ensure, launch_as, problem_of, px, say, staged, text_line,
};

/// The made-up files of the first check: their names, sent in this order.
const SENT: [&str; 5] = [
    "made-up budget.xlsx",
    "made-up letter.docx",
    "made-up slides.pptx",
    "made-up notes.md",
    "made-up picture.png",
];
/// The time a slow upload's answer takes: more than the old limit of 30 s, less than the file's new limit.
const SLOW: Duration = Duration::from_secs(45);
/// A file of 3 MiB: its limit is 30 s and 48 s.
const LARGE: usize = 3 * 1024 * 1024;
/// A file of 2 MiB: its limit is 30 s and 32 s.
const MEDIUM: usize = 2 * 1024 * 1024;
/// The longest wait for an upload that is slow or never answers.
const LONG: Duration = Duration::from_secs(150);
const TIMEOUT_TEXT: &str =
    "The file service didn't confirm the upload or share. Check your connection before trying again.";
const UNREADABLE_TEXT: &str = "The upload completed, but its file details couldn't be read. Choose the file again.";
const SEND_REFUSED: &str = "Wait for your files to upload, or retry or remove a failed file.";
/// A card's Retry, beside its Remove: from the box's left edge, and down from its top edge while the box holds no text.
const RETRY_FROM_LEFT: u32 = 279;
const RETRY_FROM_TOP: u32 = 57;
/// The file card's look in Teams Light: the message's surface and the card's border.
const SURFACE: [u8; 3] = [255, 255, 255];
const BORDER: [u8; 3] = [224, 224, 224];
const CARD_WIDTH: u32 = 316;
/// A place over the messages, for the wheel.
const MESSAGES: (u32, u32) = (860, 300);
const HEADER: u32 = 64;

pub(super) const CHECKS: [super::Check; 5] = [
    ("qa792_office_files", office_files),
    ("qa792_receiver", receiver),
    ("qa792_slow_upload", slow_upload),
    ("qa792_no_answer_retry", no_answer_retry),
    ("qa792_bad_path", bad_path),
];

impl User<'_> {
    /// A made-up file `name` of `size` bytes (a real PNG for a .png) in a fresh folder of the sandbox.
    fn made_up_of(&mut self, name: &str, size: usize) -> SandboxResult<PathBuf> {
        let path = self.made_up(&[name])?.remove(0);
        let bytes = if std::path::Path::new(name).extension().is_some_and(|ext| ext == "png") {
            png()
        } else {
            let line = b"A synthetic attachment, not a real document.\n";
            line.iter().copied().cycle().take(size.max(line.len())).collect()
        };
        fs::write(&path, bytes)?;
        Ok(path)
    }

    /// As [`User::until`], with the wait `wait`.
    fn until_within(&mut self, what: &str, wait: Duration, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let start = Instant::now();
        loop {
            let state = self.state()?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > wait {
                return Err(format!("{what}: never came; last {state}").into());
            }
            thread::sleep(POLL.max(Duration::from_millis(250)));
        }
    }

    /// Puts `path` in the shown chat's box: with Attach, else dropped from a file manager.
    fn stage(&mut self, path: &std::path::Path, attach: bool) -> SandboxResult {
        if attach {
            return self.attach_by_button("chat", path);
        }
        let state = self.state()?;
        self.drop_files(DOLPHIN, &[path.to_owned()], box_centre(&state["chat"])?, CALM)?;
        Ok(())
    }

    /// Types a made-up line into the shown chat's box and presses Enter; the line.
    fn type_and_send(&mut self) -> SandboxResult<String> {
        let mark = self.mark();
        let line = format!("A made-up file {mark}");
        let state = self.state()?;
        let at = self.on_screen(text_line(&state["chat"])?);
        self.click(at)?;
        self.type_text(&line)?;
        self.press("enter")?;
        Ok(line)
    }

    /// The message with `line` the mock got, and the file entries it posts.
    fn sent_with(&mut self, line: &str) -> SandboxResult<Vec<Value>> {
        let (_, body) = wait_until(WAIT, || self.messages_with(line).into_iter().next())
            .ok_or_else(|| format!("the mock got no message with {line}"))?;
        let body: Value = serde_json::from_str(&body)?;
        let files = &body["properties"]["files"];
        let files: Value = match files {
            Value::String(text) => serde_json::from_str(text)?,
            other => other.clone(),
        };
        Ok(files.as_array().cloned().unwrap_or_default())
    }

    /// The newest file of `name` on a's drive.
    fn drive_file(&self, name: &str) -> SandboxResult<teams_mock::DriveFile> {
        self.mock
            .drive_files()
            .into_iter()
            .filter(|file| file.name == name)
            .max_by(|one, other| one.id.cmp(&other.id))
            .ok_or_else(|| format!("no {name} on the drive: {:?}", self.mock.drive_files()).into())
    }

    /// Sends `path` alone in the 1:1 chat with b, and checks the message, the share and the card.
    fn send_one(&mut self, path: &std::path::Path, attach: bool, wait: Duration) -> SandboxResult {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        self.fresh_chat(AB_CHAT)?;
        self.stage(path, attach)?;
        let state = self.until_within(&format!("{name} ready"), wait, |state| {
            ready_or_failed(&state["chat"], &name)
        })?;
        ensure(file_state(&state["chat"], &name) == "ready", || {
            format!("{name} did not upload: {}", state["chat"]["files"])
        })?;
        let line = self.type_and_send()?;
        let entries = self.sent_with(&line)?;
        let drive = self.drive_file(&name)?;
        ensure(entries.len() == 1 && entries[0]["fileName"] == name.as_str(), || {
            format!("the message posts {entries:?}, not {name}")
        })?;
        let object_url = entries[0]["objectUrl"].as_str().unwrap_or_default();
        ensure(object_url == drive.path && !object_url.contains('?'), || {
            format!("{name}: the message posts {object_url}, not the path {}", drive.path)
        })?;
        ensure(drive.shared_with == [LAB_B], || {
            format!("{name} is shared with {:?}, not b alone", drive.shared_with)
        })?;
        self.until(&format!("{name}'s card in the chat"), |state| {
            staged(&state["chat"]).is_empty() && card_of(state, &name, true).is_some_and(|url| url == drive.path)
        })?;
        say(&format!("  sent {name}: {object_url}"));
        Ok(())
    }

    /// Starts the app again as `who`, after the shown app quits.
    fn restart_as(&mut self, who: &str) -> SandboxResult {
        self.quit()?;
        let dir = if who == "a" {
            self.dir.clone()
        } else {
            self.sandbox.path(&format!("app-{who}"))
        };
        fs::create_dir_all(&dir)?;
        let running = launch_as(self.sandbox, self.mock, &self.trouter.clone(), &dir, who)?;
        self.running = Some(running);
        self.park()?;
        thread::sleep(SETTLE);
        Ok(())
    }
}

/// The state (`uploading`, `ready`, `failed`) of the file `name` in `boxed`.
fn file_state(boxed: &Value, name: &str) -> String {
    file_in(boxed, name)
        .and_then(|file| file["state"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn file_in<'v>(boxed: &'v Value, name: &str) -> Option<&'v Value> {
    boxed["files"].as_array()?.iter().find(|file| file["name"] == name)
}

fn ready_or_failed(boxed: &Value, name: &str) -> bool {
    matches!(file_state(boxed, name).as_str(), "ready" | "failed")
}

/// The address of the newest card of `name` in the chat shown (sent by the account when `own`), when there is one.
fn card_of(state: &Value, name: &str, own: bool) -> Option<String> {
    state["cards"]
        .as_array()?
        .iter()
        .rev()
        .filter(|item| item["own"] == own)
        .flat_map(|item| item["files"].as_array().cloned().unwrap_or_default())
        .find(|file| file["name"] == name)
        .map(|file| file["object_url"].as_str().unwrap_or_default().to_owned())
}

/// A made-up picture: a PNG of 1 x 1 px.
fn png() -> Vec<u8> {
    let mut picture = image::RgbImage::new(1, 1);
    picture.put_pixel(0, 0, image::Rgb([98, 100, 167]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    let _ = picture.write_to(&mut bytes, image::ImageFormat::Png);
    bytes.into_inner()
}

// --- the checks ---

/// An .xlsx with Attach, then a .docx, a .pptx, a .md and a .png dropped, each sent alone.
fn office_files(user: &mut User<'_>) -> SandboxResult {
    for (at, name) in SENT.iter().enumerate() {
        let path = user.made_up_of(name, 4096)?;
        user.send_one(&path, at == 0, WAIT)?;
        if at == 0 {
            thread::sleep(SETTLE);
            user.shots_light_and_dark("qa792-sent-xlsx")?;
        }
    }
    thread::sleep(SETTLE);
    user.shots_light_and_dark("qa792-sent-files")
}

/// b's app shows the five cards, and a click on each card on the screen opens that file's path.
fn receiver(user: &mut User<'_>) -> SandboxResult {
    user.restart_as("b")?;
    let outcome = receiver_sees(user);
    user.restart_as("a")?;
    outcome
}

fn receiver_sees(user: &mut User<'_>) -> SandboxResult {
    user.ask("theme-light")?;
    user.ask("font 14")?;
    user.ask(&format!("open {AB_CHAT}"))?;
    let state = user.until("b sees the five cards", |state| {
        SENT.iter().all(|name| card_of(state, name, false).is_some())
    })?;
    let paths: Vec<String> = SENT
        .iter()
        .map(|name| user.drive_file(name).map(|file| file.path))
        .collect::<SandboxResult<_>>()?;
    for (name, path) in SENT.iter().zip(&paths) {
        let url = card_of(&state, name, false).unwrap_or_default();
        ensure(&url == path, || format!("b's card of {name} names {url}, not {path}"))?;
    }
    thread::sleep(SETTLE);
    user.shots_light_and_dark("qa792-receiver")?;
    // a card is tall: the screen shows one or two; the wheel brings the older ones down
    let mut opened: Vec<String> = Vec::new();
    for round in 0..12 {
        let shot = user.shot(&format!("qa792-receiver-cards-{round}"))?;
        let cards = cards_in(&image::open(&shot)?.to_rgb8());
        say(&format!("  cards on the screen: {cards:?}"));
        let known = opened.len();
        for card in &cards {
            let link = user.open_card(*card)?;
            ensure(paths.iter().any(|path| link.starts_with(path.as_str())), || {
                format!("the card at {card:?} opened {link}, no sent file's path")
            })?;
            if !opened.contains(&link) {
                opened.push(link);
            }
        }
        if opened.len() >= SENT.len() {
            break;
        }
        // up first; at the top, down again to the cards between
        let notches = if round == 0 || opened.len() > known { -16 } else { 6 };
        user.input.scroll(MESSAGES.0, MESSAGES.1, notches)?;
        user.park()?;
        thread::sleep(SETTLE);
    }
    say(&format!("  the cards opened {opened:?}"));
    ensure(opened.len() == SENT.len(), || {
        format!("only {} cards opened a file", opened.len())
    })?;
    ensure(opened[0].starts_with(paths[SENT.len() - 1].as_str()), || {
        format!("the newest card opened {}, not {}", opened[0], paths[SENT.len() - 1])
    })
}

impl User<'_> {
    /// Clicks the name on `card` as a user does; the link the app asked the opener for.
    fn open_card(&mut self, card: (u32, u32, u32, u32)) -> SandboxResult<String> {
        let before = self.sandbox.opened()?.len();
        self.click((card.0 + 90, card.1 + 22))?;
        let link = wait_until(WAIT, || self.sandbox.opened().ok()?.get(before).cloned())
            .and_then(|seen| seen.args.last().cloned())
            .ok_or_else(|| format!("a click on the card at {card:?} opened nothing"))?;
        self.park()?;
        thread::sleep(SETTLE);
        Ok(link)
    }
}

/// A file of 3 MiB whose upload answer comes after 45 s: it uploads and sends.
fn slow_upload(user: &mut User<'_>) -> SandboxResult {
    const NAME: &str = "made-up large budget.xlsx";
    let path = user.made_up_of(NAME, LARGE)?;
    user.mock.twist_next_drive_upload(DriveTwist::Slow(SLOW));
    let started = Instant::now();
    user.fresh_chat(AB_CHAT)?;
    user.stage(&path, false)?;
    user.until("the card while the file uploads", |state| {
        file_state(&state["chat"], NAME) == "uploading"
    })?;
    thread::sleep(SETTLE);
    user.shot("qa792-slow-uploading")?;
    let state = user.until_within("the slow upload's end", LONG, |state| {
        ready_or_failed(&state["chat"], NAME)
    })?;
    let took = started.elapsed();
    say(&format!("  the upload ended after {} s", took.as_secs()));
    ensure(file_state(&state["chat"], NAME) == "ready", || {
        format!(
            "the slow upload failed after {} s: {}",
            took.as_secs(),
            state["chat"]["files"]
        )
    })?;
    ensure(took >= Duration::from_secs(40), || {
        format!("the upload ended after {} s, before the mock answered", took.as_secs())
    })?;
    let line = user.type_and_send()?;
    let entries = user.sent_with(&line)?;
    ensure(entries.first().is_some_and(|entry| entry["fileName"] == NAME), || {
        format!("the message posts {entries:?}")
    })?;
    user.until("the large file's card", |state| card_of(state, NAME, true).is_some())?;
    thread::sleep(SETTLE);
    user.shot("qa792-slow-sent")?;
    Ok(())
}

/// A file of 2 MiB whose upload never answers: the timeout text after its limit, then Retry uploads it and it sends.
fn no_answer_retry(user: &mut User<'_>) -> SandboxResult {
    const NAME: &str = "made-up unanswered report.docx";
    let path = user.made_up_of(NAME, MEDIUM)?;
    user.mock.twist_next_drive_upload(DriveTwist::Never);
    let started = Instant::now();
    user.fresh_chat(AB_CHAT)?;
    user.stage(&path, false)?;
    let state = user.until_within("the unanswered upload's end", LONG, |state| {
        ready_or_failed(&state["chat"], NAME)
    })?;
    let took = started.elapsed();
    say(&format!("  the upload ended after {} s", took.as_secs()));
    let file = file_in(&state["chat"], NAME).cloned().unwrap_or_default();
    ensure(file["state"] == "failed" && file["error"] == TIMEOUT_TEXT, || {
        format!("the unanswered upload shows {file}")
    })?;
    ensure((55..=90).contains(&took.as_secs()), || {
        format!(
            "the unanswered upload ended after {} s, not at its limit of 62 s",
            took.as_secs()
        )
    })?;
    thread::sleep(SETTLE);
    user.shots_light_and_dark("qa792-no-answer")?;
    let puts = user.mock.drive_files().iter().filter(|file| file.name == NAME).count();
    let bounds = &state["chat"]["bounds"];
    let retry = user.on_screen((px(&bounds[0]) + RETRY_FROM_LEFT, px(&bounds[1]) + RETRY_FROM_TOP));
    user.input.move_to(retry.0, retry.1)?;
    thread::sleep(SETTLE);
    user.shot("qa792-retry-hover")?;
    user.click(retry)?;
    let state = user.until("the file after Retry", |state| {
        file_state(&state["chat"], NAME) == "ready"
    })?;
    ensure(
        user.mock.drive_files().iter().filter(|file| file.name == NAME).count() == puts + 1,
        || format!("Retry did not upload the file again: {:?}", user.mock.drive_files()),
    )?;
    ensure(problem_of(&state["chat"]).is_empty(), || {
        format!("the box says {state}")
    })?;
    let line = user.type_and_send()?;
    let entries = user.sent_with(&line)?;
    ensure(entries.first().is_some_and(|entry| entry["fileName"] == NAME), || {
        format!("the message posts {entries:?}")
    })?;
    user.until("the card after Retry", |state| card_of(state, NAME, true).is_some())?;
    Ok(())
}

/// An upload answer whose `webDavUrl` is outside the upload site, or has a query: the card's text, Send refused, and
/// no share and no message.
fn bad_path(user: &mut User<'_>) -> SandboxResult {
    let own_folder = format!(
        "{DRIVE_HOST}/personal/lab_00000000_0000_4000_8000_00000000000a/Documents/Microsoft%20Teams%20Chat%20Files"
    );
    let cases = [
        (
            "made-up outside.xlsx",
            "https://elsewhere-my.sharepoint.com/personal/someone_else/Documents/made-up%20outside.xlsx".to_owned(),
        ),
        (
            "made-up query.docx",
            format!("{own_folder}/made-up%20query.docx?share=anyone"),
        ),
    ];
    for (name, web_dav_url) in cases {
        let path = user.made_up_of(name, 4096)?;
        user.mock
            .twist_next_drive_upload(DriveTwist::WebDavUrl(web_dav_url.clone()));
        user.fresh_chat(AB_CHAT)?;
        user.stage(&path, false)?;
        let state = user.until(&format!("{name}'s upload end"), |state| {
            ready_or_failed(&state["chat"], name)
        })?;
        let file = file_in(&state["chat"], name).cloned().unwrap_or_default();
        ensure(file["state"] == "failed" && file["error"] == UNREADABLE_TEXT, || {
            format!("{name} with {web_dav_url} shows {file}")
        })?;
        let line = user.type_and_send()?;
        let state = user.until(&format!("{name}: Send refused"), |state| {
            problem_of(&state["chat"]) == SEND_REFUSED
        })?;
        thread::sleep(SETTLE);
        user.shot(&format!(
            "qa792-bad-path-{}",
            name.trim_start_matches("made-up ").replace('.', "-")
        ))?;
        thread::sleep(Duration::from_secs(2));
        ensure(user.messages_with(&line).is_empty(), || {
            format!("{name}: a message went: {state}")
        })?;
        let drive = user.drive_file(name)?;
        ensure(drive.shared_with.is_empty(), || {
            format!("{name} was shared with {:?}", drive.shared_with)
        })?;
        let invites = user
            .runtime
            .block_on(user.mock.received_requests())
            .iter()
            .filter(|request| request.url.path().ends_with("/invite") && request.url.path().contains(&drive.id))
            .count();
        ensure(invites == 0, || format!("{name}: {invites} invites went"))?;
    }
    user.ask("clear")?;
    Ok(())
}

// --- the cards on the screen, as in the share-file QA ---

/// The file cards on the screen in Teams Light (left, top, right, bottom): a border round a box of a card's size over
/// the message's surface.
fn cards_in(shot: &image::RgbImage) -> Vec<(u32, u32, u32, u32)> {
    let mut found: Vec<(u32, u32, u32, u32)> = Vec::new();
    for y in 1..shot.height().saturating_sub(HEADER) {
        for (from, to) in runs(shot, y, |pixel| near(pixel, BORDER, 2)) {
            if to - from < 100 {
                continue;
            }
            let Some(card) = card_from(shot, y, (from, to)) else {
                continue;
            };
            let (width, height) = (card.2 - card.0, card.3 - card.1);
            let overlaps = |known: &(u32, u32, u32, u32)| {
                card.0 < known.2 && known.0 < card.2 && card.1 < known.3 && known.1 < card.3
            };
            if (150..=CARD_WIDTH + 4).contains(&width) && (60..=300).contains(&height) && !found.iter().any(overlaps) {
                found.push(card);
            }
        }
    }
    found.sort_by_key(|card| (card.1, card.0));
    found
}

fn card_from(shot: &image::RgbImage, y: u32, (from, to): (u32, u32)) -> Option<(u32, u32, u32, u32)> {
    let mid = from + (to - from) / 2;
    let below = at(shot, mid, y + 1);
    if !near(at(shot, mid, y - 1), SURFACE, 3) || near(below, SURFACE, 3) || near(below, BORDER, 2) {
        return None;
    }
    let probe = y + 12;
    let left = (from.saturating_sub(12)..=from).find(|&x| near(at(shot, x, probe), BORDER, 2))?;
    let right = (to.saturating_sub(1)..to + 12)
        .rev()
        .find(|&x| near(at(shot, x, probe), BORDER, 2))?;
    let mut down = probe;
    while down < shot.height() && near(at(shot, left, down), BORDER, 2) {
        down += 1;
    }
    let bottom = (down.saturating_sub(2)..(down + 12).min(shot.height() - 1))
        .find(|&row| near(at(shot, mid, row), BORDER, 2) && near(at(shot, mid, row + 1), SURFACE, 3))?;
    Some((left, y, right + 1, bottom + 1))
}

fn runs(shot: &image::RgbImage, y: u32, taken: impl Fn([u8; 3]) -> bool) -> Vec<(u32, u32)> {
    let mut found = Vec::new();
    let mut x = 0;
    while x < shot.width() {
        if !taken(at(shot, x, y)) {
            x += 1;
            continue;
        }
        let start = x;
        while x < shot.width() && taken(at(shot, x, y)) {
            x += 1;
        }
        found.push((start, x));
    }
    found
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
