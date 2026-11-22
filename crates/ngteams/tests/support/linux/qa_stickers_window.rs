//! QA of task 649 (round 1), in the UI sandbox: Teams stickers that people send, as the app shows them. The real app
//! runs as a child in the sandbox, as lab account a on `teams-mock` (its chat service, its Trouter, its group video).
//! The mock serves a made-up animated sticker on the path of Teams' sticker CDN. The parent drives the app with the
//! sandbox's virtual pointer and keyboard. It finds each sticker in the screenshots by the colour of its face, as a
//! user's eye does, and asks the child what the views hold through a file in the sandbox dir (`harness = false`: GPUI
//! runs on the process's main thread).
//!
//! - **Bare stickers**: b's sticker (the service's read-back form) and a's own (the web client's sent form), each drawn
//!   alone at 180 by 180 with no bubble, b's on the left with b's name and time, a's on the right; the GIF plays.
//! - **A sticker beside text**: in the bubble under the text.
//! - **A click** on a sticker opens no picture viewer. The hover bar and "More options" still work: a quick reaction,
//!   Reply, Copy text. Edit is not offered on a's own sticker.
//! - **A quoted sticker**: a's reply to b's sticker, sent and drawn.
//! - **Texts**: the chat list's preview, search, and the desktop banner of a sticker that comes live.
//! - **A broken or slow CDN**: a 404, a 500, bytes that are no picture, an answer slower than the client waits: the
//!   label's placeholder, no crash, no endless "reading".
//! - **Sizes**: Teams Light and Dark, font 20 and 24, a 520 px and an 800 px window, the call's Chat pane.
//! - **A restart**: the sticker shows again from the disk cache, with no second read of the CDN.
//!
//! `QA649_ONLY` (a comma list of check names) runs only those. `QA649_EVIDENCE` names a directory that gets the
//! screenshots and the results too. Screenshots also go to `target/ui-shots/qa649-*.png`. Every name, id, text and
//! picture is made up.
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
use ngteams::calling::PersonalCalls;
use ngteams::descriptors::adopt_trace;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::account::web_client_info;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{CallMedia, CallParts, Identity, Outbox, Relay};
use teams_ctl::protocol::{CallRow, Request, Response};
use teams_ctl::{Client, Timeouts};
use teams_media::devices::Devices;
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::processing::ProcessingConfig;
use teams_media::relay::RelayServer;
use teams_media::session::SessionConfig;
use teams_media::tone::Tone;
use teams_mock::{
    AB_CHAT, ABC_CHAT, AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE,
    PRESENCE_BASE, PROVISIONING_BASE, TENANT, USER_PROFILE_BASE,
};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use teams_ui::chat_view::{Body, ChatView, MessageAction};
use teams_ui::compose::Mode;
use teams_ui::rich_text::Block;
use teams_ui::shell::{Main, Shell};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use teams_ui::{SettingsFile, VideoFeed};
use tokio::sync::broadcast;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "../window.rs"]
mod support;
use support::{Alias, display_name};

const CHILD_ENV: &str = "QA649_CHILD";
const ONLY_ENV: &str = "QA649_ONLY";
const EVIDENCE_ENV: &str = "QA649_EVIDENCE";
const TRACE_FD_ENV: &str = "QA649_TRACE_FD";
const ORIGIN_ENV: &str = "QA649_ORIGIN";
const TROUTER_ENV: &str = "QA649_TROUTER";
const RELAY_ENV: &str = "QA649_RELAY";
const IC3_ENV: &str = "QA649_IC3";
const SKYPE_ENV: &str = "QA649_SKYPE";
const DIR_ENV: &str = "QA649_DIR";
const CTL_ENV: &str = "QA649_CTL";
/// The file in the sandbox dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "qa649-ask";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(60);
const SETTLE: Duration = Duration::from_millis(700);
/// The time between a key or click and the next, so the app sees each.
const STEP: Duration = Duration::from_millis(250);
/// niri's layout: each window takes the whole screen until the test sizes it, and no hot corner.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
/// The screen.
const SCREEN: (u32, u32) = (1280, 800);
const PEER_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.2,
};

/// Teams' sticker CDN, as a message names a sticker on it.
const CDN: &str = "https://teams.public.onecdn.static.microsoft";
/// The CDN's folder of the made-up pack.
const PACK: &str = "/evergreen-assets/stickerassets/madeup-250x250/en-us";
const STICKER_ITEMTYPE: &str = "http://schema.skype.com/Sticker";
/// The made-up sticker the mock serves, and its label.
const HI: &str = "MadeUp_Hi.gif";
const HI_LABEL: &str = "A made-up sticker says hi";
/// A second made-up sticker, the same picture, that comes live.
const WAVE: &str = "MadeUp_Wave.gif";
const WAVE_LABEL: &str = "A made-up sticker waves";
/// The stickers the CDN fails: none at the path, a 500, bytes that are no picture, an answer slower than the client
/// waits.
const BROKEN: [(&str, &str); 4] = [
    ("MadeUp_Gone.gif", "A made-up sticker that is gone"),
    ("MadeUp_Fails.gif", "A made-up sticker that fails"),
    ("MadeUp_Junk.gif", "A made-up sticker of junk"),
    ("MadeUp_Slow.gif", "A made-up sticker that is slow"),
];
/// Longer than the client waits for an answer (`teams_proto::http::REQUEST_TIMEOUT`, 30 s).
const SLOW: Duration = Duration::from_secs(45);

/// The sticker's colours (`write_made_up_sticker`): the face, and the rim, eyes and mouth.
const FACE: [u8; 3] = [250, 200, 40];
const INK: [u8; 3] = [60, 40, 20];
/// The sticker's size, and where its face's centre is in it.
const STICKER: i32 = 180;
const FACE_AT: (i32, i32) = (80, 100);

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_stickers_window: {error}");
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

fn src_of(file: &str) -> String {
    format!("{CDN}{PACK}/{file}?v=5")
}

/// A sticker as the service gives one back: the bare `img` between two blank paragraphs.
fn read_back(file: &str, label: &str) -> String {
    let src = src_of(file);
    format!(
        "<p>&nbsp;</p>\r\n<img src=\"{src}\" height=\"180\" alt=\"{label}\" width=\"180\" \
         itemtype=\"{STICKER_ITEMTYPE}\">\r\n<p>&nbsp;</p>"
    )
}

/// A sticker as the web client sends one: in its `readonly` element.
fn as_sent(file: &str, label: &str) -> String {
    let src = src_of(file);
    format!(
        "<p>&nbsp;</p><readonly aria-label=\"{label}\" contenteditable=\"false\" itemtype=\"{STICKER_ITEMTYPE}\" \
         title=\"{label}\"><img src=\"{src}\" height=\"180\" alt=\"{label}\" width=\"180\" \
         data-image-type=\"standard\" data-image-mode=\"single\" itemtype=\"{STICKER_ITEMTYPE}\"></readonly>\
         <p>&nbsp;</p>"
    )
}

// --- the parent ---

type Check = (&'static str, fn(&mut User<'_>) -> SandboxResult);

/// The first run of the app.
const FIRST: [Check; 12] = [
    ("bare_stickers", bare_stickers),
    ("gif_moves", gif_moves),
    ("beside_text", beside_text),
    ("click_opens_no_viewer", click_opens_no_viewer),
    ("hover_bar_and_menu", hover_bar_and_menu),
    ("own_sticker_no_edit", own_sticker_no_edit),
    ("preview_search_banner", preview_search_banner),
    ("quoted_sticker", quoted_sticker),
    ("broken_and_slow_cdn", broken_and_slow_cdn),
    ("themes_fonts_narrow", themes_fonts_narrow),
    ("call_chat_pane", call_chat_pane),
    ("reads_before_restart", reads_before_restart),
];

/// The second run of the app over the same account dir.
const RESTART: [Check; 1] = [("cached_after_restart", cached_after_restart)];

#[derive(Default)]
struct Tally {
    passed: Vec<String>,
    failed: Vec<(String, String)>,
}

/// The chats the parent made on the mock.
struct Chats {
    stickers: String,
    words: String,
    broken: String,
}

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_stickers_window", &options)? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mock = runtime.block_on(ChatServiceMock::start())?;
    let chats = seed(&mock, &runtime)?;
    let trouter = mock.start_trouter()?;
    let dir = tempfile::tempdir()?;
    let relay = mock.start_group_video(dir.path(), PEER_TONE)?;
    let mut tally = Tally::default();
    let mut quits = Vec::new();
    let mut reads = None;
    for (run, checks) in [("first", &FIRST[..]), ("restart", &RESTART[..])] {
        if !checks.iter().any(|(name, _)| wanted(name)) {
            continue;
        }
        say(&format!("run {run}"));
        let app = AppUnderTest::start(&sandbox, &mock, &relay.to_string(), trouter.origin(), dir.path())?;
        let mut user = User::new(&sandbox, &mock, &runtime, app, &chats)?;
        user.reads_before = reads;
        for (name, check) in checks {
            if wanted(name) {
                user.run_check(name, *check, &mut tally.passed, &mut tally.failed);
            }
        }
        reads = user.reads_before;
        let _ = user.hang_up();
        quits.push(user.quit());
    }
    drop(trouter);
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
        "qa_stickers_window: {} passed, {} failed",
        tally.passed.len(),
        tally.failed.len()
    ));
    for quit in quits {
        quit?;
    }
    ensure(tally.failed.is_empty(), || {
        format!("{} checks failed", tally.failed.len())
    })
}

/// The mock's CDN and chats: "Lab stickers" (b's words, b's sticker, a's own), "Lab sticker words" (b's sticker under
/// b's text in one message), "Lab broken stickers" (b's stickers the CDN fails), and b's sticker in the a–b–c chat (the
/// group call's).
fn seed(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) -> SandboxResult<Chats> {
    let mut gif = Vec::new();
    write_made_up_sticker(&mut gif)?;
    for file in [HI, WAVE] {
        runtime.block_on(mock.serve_bytes(&format!("{PACK}/{file}"), "image/gif", gif.clone()));
    }
    let [gone, fails, junk, slow] = BROKEN;
    let _ = gone;
    runtime.block_on(mock.refuse_next("GET", fails.0, 500));
    runtime.block_on(mock.serve_bytes(&format!("{PACK}/{}", junk.0), "image/gif", b"no picture".to_vec()));
    runtime.block_on(mock.refuse_next_after("GET", slow.0, 200, SLOW));
    let stickers = mock.add_group("Lab stickers", &[LAB_A, LAB_B]).ok_or("no chat made")?;
    mock.post_as(LAB_B, &stickers, "<p>Made-up sticker test</p>")
        .ok_or("b's words refused")?;
    mock.post_as(LAB_B, &stickers, &read_back(HI, HI_LABEL))
        .ok_or("b's sticker refused")?;
    mock.post_as(LAB_A, &stickers, &as_sent(HI, HI_LABEL))
        .ok_or("a's sticker refused")?;
    let words = mock
        .add_group("Lab sticker words", &[LAB_A, LAB_B])
        .ok_or("no chat made")?;
    let src = src_of(HI);
    mock.post_as(
        LAB_B,
        &words,
        &format!(
            "<p>Look at this made-up one</p>\r\n<img src=\"{src}\" height=\"180\" alt=\"{HI_LABEL}\" width=\"180\" \
             itemtype=\"{STICKER_ITEMTYPE}\">\r\n<p>&nbsp;</p>"
        ),
    )
    .ok_or("b's words and sticker refused")?;
    let broken = mock
        .add_group("Lab broken stickers", &[LAB_A, LAB_B])
        .ok_or("no chat made")?;
    for (file, label) in BROKEN {
        mock.post_as(LAB_B, &broken, &read_back(file, label))
            .ok_or("b's broken sticker refused")?;
    }
    mock.post_as(LAB_B, ABC_CHAT, &read_back(HI, HI_LABEL))
        .ok_or("b's sticker in the group refused")?;
    Ok(Chats {
        stickers,
        words,
        broken,
    })
}

/// A made-up animated sticker, 180 by 180 on a clear ground: a yellow face whose mouth opens and closes over four
/// frames, a blue speech mark beside it (as the UI tour's).
fn write_made_up_sticker(out: &mut Vec<u8>) -> SandboxResult {
    let mut encoder = image::codecs::gif::GifEncoder::new(out);
    encoder.set_repeat(image::codecs::gif::Repeat::Infinite)?;
    for at in 0u32..4 {
        let mut frame = image::RgbaImage::from_pixel(180, 180, image::Rgba([0, 0, 0, 0]));
        for y in 0..180u32 {
            for x in 0..180u32 {
                let (dx, dy) = (i64::from(x) - 80, i64::from(y) - 100);
                let face = dx * dx + dy * dy <= 60 * 60;
                let rim = face && dx * dx + dy * dy >= 56 * 56;
                let eye = (dx + 20).pow(2) + (dy + 15).pow(2) <= 64 || (dx - 20).pow(2) + (dy + 15).pow(2) <= 64;
                let open = 4 + i64::from(at % 2) * 10;
                let mouth = dx.abs() <= 22 && (dy - 22).abs() <= open / 2 + 2;
                let mark = (130..170).contains(&x) && (20..55).contains(&y);
                let colour = if rim || eye || (face && mouth) {
                    [INK[0], INK[1], INK[2], 255]
                } else if face {
                    [FACE[0], FACE[1], FACE[2], 255]
                } else if mark {
                    [70, 110, 220, 255]
                } else {
                    continue;
                };
                frame.put_pixel(x, y, image::Rgba(colour));
            }
        }
        let delay = image::Delay::from_numer_denom_ms(250, 1);
        encoder.encode_frame(image::Frame::from_parts(frame, 0, 0, delay))?;
    }
    Ok(())
}

/// The child app in its sandbox: its process, its trace, its control socket.
struct AppUnderTest {
    running: Option<Running>,
    trace: TraceReader,
    socket: PathBuf,
}

impl AppUnderTest {
    fn start(sandbox: &Sandbox, mock: &ChatServiceMock, relay: &str, trouter: &str, dir: &Path) -> SandboxResult<Self> {
        let own = dir.join("a");
        fs::create_dir_all(&own)?;
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let ic3 = mock.token(LAB_A).ok_or("the mock's token")?;
        let skype = mock.skypetoken(LAB_A).ok_or("the mock's skypetoken")?;
        let socket = sandbox.path("run/ctl.sock");
        let own_text = own.to_string_lossy().into_owned();
        let socket_text = socket.to_string_lossy().into_owned();
        let child = sandbox.spawn_with_fds(
            &std::env::current_exe()?,
            &[],
            &[
                (CHILD_ENV, "a"),
                (TRACE_FD_ENV, &fd),
                (ORIGIN_ENV, &mock.origin()),
                (TROUTER_ENV, trouter),
                (RELAY_ENV, relay),
                (IC3_ENV, &ic3),
                (SKYPE_ENV, &skype),
                (DIR_ENV, &own_text),
                (CTL_ENV, &socket_text),
            ],
            &[writer.as_fd()],
        )?;
        drop(writer);
        let mut running = Running::new(child)?;
        let mut trace = TraceReader::start(reader);
        if let Err(error) = running.line_where(STARTUP, |line| line == "ready") {
            return Err(running.fail(&format!("no ready line: {error:?}{}", sandbox.logs())));
        }
        trace.wait_for("status", "connected")?;
        trace.wait_for("calling", "registered")?;
        Ok(Self {
            running: Some(running),
            trace,
            socket,
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

    fn catch_up(&mut self) -> &[Value] {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
        &self.seen
    }

    fn count(&mut self, view: &str, change: &str) -> usize {
        self.catch_up()
            .iter()
            .filter(|line| line["view"] == view && line["change"] == change)
            .count()
    }

    fn wait_for(&mut self, view: &str, change: &str) -> SandboxResult {
        let deadline = Instant::now() + WAIT;
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

    fn overlaps(self, other: Self) -> bool {
        self.x < other.right() && other.x < self.right() && self.y < other.bottom() && other.y < self.bottom()
    }
}

/// One sticker found in a screenshot: its face, and the 180 by 180 square the sticker takes around it.
#[derive(Clone, Copy, Debug)]
struct Found {
    face: Rect,
    sticker: Rect,
}

impl Found {
    fn centre(self) -> (i32, i32) {
        (self.face.x + self.face.w / 2, self.face.y + self.face.h / 2)
    }
}

fn pixel(image: &RgbImage, x: i32, y: i32) -> Option<[u8; 3]> {
    let (x, y) = (u32::try_from(x).ok()?, u32::try_from(y).ok()?);
    (x < image.width() && y < image.height()).then(|| image.get_pixel(x, y).0)
}

fn near(a: [u8; 3], b: [u8; 3], tolerance: u8) -> bool {
    a.iter().zip(b).all(|(one, other)| one.abs_diff(other) <= tolerance)
}

/// The blobs of `colour` in `image` of `least` px at least, by their bounding boxes, top to bottom.
fn blobs(image: &RgbImage, colour: [u8; 3], least: usize) -> Vec<Rect> {
    let (width, height) = (image.width(), image.height());
    let mut seen = vec![false; (width * height) as usize];
    let mut found = Vec::new();
    for start_y in 0..height {
        for start_x in 0..width {
            let index = (start_y * width + start_x) as usize;
            if seen[index] || !near(image.get_pixel(start_x, start_y).0, colour, 10) {
                continue;
            }
            if let Some(blob) = flood(image, colour, &mut seen, (start_x, start_y), least) {
                found.push(blob);
            }
        }
    }
    found
}

/// The blob of `colour` that holds `start`, each of its px marked `seen`; its bounding box, when it has `least` px.
fn flood(image: &RgbImage, colour: [u8; 3], seen: &mut [bool], start: (u32, u32), least: usize) -> Option<Rect> {
    let (width, height) = (image.width(), image.height());
    seen[(start.1 * width + start.0) as usize] = true;
    let mut stack = vec![start];
    let (mut x0, mut y0, mut x1, mut y1, mut count) = (start.0, start.1, start.0, start.1, 0usize);
    while let Some((x, y)) = stack.pop() {
        count += 1;
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        let next = [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)];
        for (nx, ny) in next.into_iter().filter(|&(nx, ny)| nx < width && ny < height) {
            let at = (ny * width + nx) as usize;
            if !seen[at] && near(image.get_pixel(nx, ny).0, colour, 10) {
                seen[at] = true;
                stack.push((nx, ny));
            }
        }
    }
    let as_i32 = |value: u32| i32::try_from(value).unwrap_or(i32::MAX);
    (count >= least).then(|| Rect {
        x: as_i32(x0),
        y: as_i32(y0),
        w: as_i32(x1 - x0 + 1),
        h: as_i32(y1 - y0 + 1),
    })
}

/// The stickers in `image`: each face of the made-up sticker, top to bottom.
fn stickers_in(image: &RgbImage) -> Vec<Found> {
    blobs(image, FACE, 2_000)
        .into_iter()
        .map(|face| {
            let (cx, cy) = (face.x + face.w / 2, face.y + face.h / 2);
            Found {
                face,
                sticker: Rect {
                    x: cx - FACE_AT.0,
                    y: cy - FACE_AT.1,
                    w: STICKER,
                    h: STICKER,
                },
            }
        })
        .collect()
}

/// The ink in the sticker's mouth: it changes as the GIF plays.
fn mouth_ink(image: &RgbImage, found: Found) -> usize {
    let (cx, cy) = found.centre();
    let mut ink = 0;
    for y in cy + 10..cy + 34 {
        for x in cx - 22..=cx + 22 {
            if pixel(image, x, y).is_some_and(|colour| near(colour, INK, 30)) {
                ink += 1;
            }
        }
    }
    ink
}

/// How far the drawn sticker reaches from its square, along its row 10 px from its top and its column 10 px from its
/// left: the run of px that are not `ground`, where `ground` is the chat's background beside it. A sticker drawn bare
/// at its size runs about 180 px each way; a bubble or a scaled picture runs more or less.
fn runs(image: &RgbImage, found: Found, ground: [u8; 3]) -> (i32, i32) {
    let off = |x: i32, y: i32| pixel(image, x, y).is_some_and(|colour| !near(colour, ground, 6));
    let (row, column) = (found.sticker.y + 10, found.sticker.x + 10);
    let (mut left, mut right) = (column, column);
    while off(left - 1, row) && left > found.sticker.x - 60 {
        left -= 1;
    }
    while off(right + 1, row) && right < found.sticker.right() + 60 {
        right += 1;
    }
    let (mut top, mut bottom) = (row, row);
    while off(column, top - 1) && top > found.sticker.y - 60 {
        top -= 1;
    }
    while off(column, bottom + 1) && bottom < found.sticker.bottom() + 60 {
        bottom += 1;
    }
    (right - left + 1, bottom - top + 1)
}

/// The size of the frame drawn behind a sticker (of any scale): the run of the frame's fill along the row under the
/// face and along the column left of the face, where the made-up sticker is clear.
fn frame_size(image: &RgbImage, found: Found) -> (u32, u32) {
    let (x, y) = (found.face.x - 6, found.face.bottom() + 6);
    let Some(fill) = pixel(image, x, y) else {
        return (0, 0);
    };
    // a scaled picture's clear px are not all exactly the fill
    let same = |x: i32, y: i32| pixel(image, x, y).is_some_and(|colour| near(colour, fill, 12));
    let run = |step: (i32, i32), from: (i32, i32)| {
        let mut at = from;
        let mut count = 0u32;
        while same(at.0 + step.0, at.1 + step.1) && count < 300 {
            at = (at.0 + step.0, at.1 + step.1);
            count += 1;
        }
        count
    };
    (
        run((-1, 0), (x, y)) + run((1, 0), (x, y)) + 1,
        run((0, -1), (x, y)) + run((0, 1), (x, y)) + 1,
    )
}

/// The user at the sandbox: the pointer, the keyboard, the screenshots, the control socket, and the child it asks
/// about the window.
struct User<'a> {
    sandbox: &'a Sandbox,
    mock: &'a ChatServiceMock,
    runtime: &'a tokio::runtime::Runtime,
    chats: &'a Chats,
    app: AppUnderTest,
    input: VirtualInput,
    ctl: Client,
    asked: u32,
    main_window: Option<u64>,
    call_window: Option<u64>,
    call: Option<String>,
    marks: u32,
    /// The CDN's reads of the sticker before the restart.
    reads_before: Option<usize>,
}

impl<'a> User<'a> {
    fn new(
        sandbox: &'a Sandbox,
        mock: &'a ChatServiceMock,
        runtime: &'a tokio::runtime::Runtime,
        app: AppUnderTest,
        chats: &'a Chats,
    ) -> SandboxResult<Self> {
        let socket = app.socket.clone();
        let ctl = wait_until(WAIT, || Client::connect(&socket, Timeouts::default()).ok())
            .ok_or_else(|| format!("no control socket at {}", socket.display()))?;
        let mut user = Self {
            sandbox,
            mock,
            runtime,
            chats,
            app,
            input: sandbox.input()?,
            ctl,
            asked: 0,
            main_window: None,
            call_window: None,
            call: None,
            marks: 0,
            reads_before: None,
        };
        user.main_window = user.focused_window()?;
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
        // the next check starts from the main window at 1280 px, Teams Light, font 14, no menu
        let _ = self.ask("reset");
        let _ = self.press("escape");
    }

    fn running(&self) -> SandboxResult<&Running> {
        self.app.running.as_ref().ok_or_else(|| "the app has quit".into())
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
        self.runtime
            .block_on(self.mock.received_requests())
            .iter()
            .filter(|request| request.method.as_str() == method)
            .map(|request| String::from_utf8_lossy(&request.body).into_owned())
            .filter(|body| body.contains(text))
            .collect()
    }

    fn wait_sent(&self, method: &str, text: &str) -> SandboxResult<String> {
        wait_until(WAIT, || self.sent_with(method, text).into_iter().next())
            .ok_or_else(|| format!("the mock got no {method} with {text:?}").into())
    }

    /// The `GET`s the mock got of the sticker `file`.
    fn cdn_reads(&self, file: &str) -> usize {
        let path = format!("{PACK}/{file}");
        self.runtime
            .block_on(self.mock.received_requests())
            .iter()
            .filter(|request| request.method.as_str() == "GET" && request.url.path() == path)
            .count()
    }

    /// A screenshot as `qa649-<name>.png`, under `target/ui-shots/` and the evidence dir.
    fn shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        let file = format!("qa649-{name}.png");
        let png = self.sandbox.screenshot(&file, false)?;
        let kept = shots_dir()?.join(&file);
        fs::copy(&png, &kept)?;
        if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
            let dir = PathBuf::from(dir).join("shots");
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(&file))?;
        }
        Ok(kept)
    }

    fn settled_shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        self.park()?;
        thread::sleep(SETTLE);
        self.shot(name)
    }

    fn image(&mut self, name: &str) -> SandboxResult<RgbImage> {
        let path = self.settled_shot(name)?;
        Ok(image::open(path)?.to_rgb8())
    }

    /// The stickers on the screen after a settled shot `name`; at least `least` of them, waited for.
    fn stickers(&mut self, name: &str, least: usize) -> SandboxResult<(RgbImage, Vec<Found>)> {
        let start = Instant::now();
        loop {
            let image = self.image(name)?;
            let found = stickers_in(&image);
            if found.len() >= least {
                return Ok((image, found));
            }
            if start.elapsed() > WAIT {
                return Err(format!("{name}: {} stickers on the screen, {least} expected", found.len()).into());
            }
            thread::sleep(POLL);
        }
    }

    /// Opens `chat` in the main window and waits until its messages are there.
    fn open(&mut self, chat: &str, least: usize) -> SandboxResult<Value> {
        self.focus_window(self.main_window)?;
        self.ask(&format!("open {chat}"))?;
        self.until(&format!("{chat} open"), |state| {
            state["chat"]["id"] == chat
                && state["chat"]["items"]
                    .as_array()
                    .is_some_and(|items| items.len() >= least)
        })
    }

    // --- windows ---

    fn windows(&self) -> SandboxResult<Vec<Value>> {
        let text = self.sandbox.niri_msg(&["-j", "windows"])?;
        let windows: Value = serde_json::from_str(&text)?;
        Ok(windows.as_array().cloned().unwrap_or_default())
    }

    fn focused_window(&self) -> SandboxResult<Option<u64>> {
        Ok(self
            .windows()?
            .iter()
            .find(|window| window["is_focused"] == true)
            .and_then(|window| window["id"].as_u64()))
    }

    /// The size niri gives window `id`.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "niri's logical px are whole numbers on a scale of 1"
    )]
    fn window_size(&self, id: u64) -> SandboxResult<(i32, i32)> {
        let windows = self.windows()?;
        let window = windows
            .iter()
            .find(|window| window["id"].as_u64() == Some(id))
            .ok_or_else(|| format!("no window {id}"))?;
        let size = &window["layout"]["window_size"];
        let at = |index: usize| size[index].as_f64().map(|value| value.round() as i32);
        Ok((at(0).ok_or("no width")?, at(1).ok_or("no height")?))
    }

    fn focus_window(&mut self, id: Option<u64>) -> SandboxResult {
        let id = id.ok_or("no such window")?;
        self.sandbox
            .niri_msg(&["action", "focus-window", "--id", &id.to_string()])?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The compositor sizes the focused window to `width` wide, as a user's drag does; the window takes the screen's
    /// left edge.
    fn resize_focused(&mut self, width: i32) -> SandboxResult {
        let id = self.focused_window()?.ok_or("no window has the focus")?;
        self.sandbox
            .niri_msg(&["action", "set-column-width", &width.to_string()])?;
        wait_until(WAIT, || {
            self.window_size(id).ok().filter(|size| size.0 == width).map(|_| ())
        })
        .ok_or_else(|| format!("the window never took {width} px"))?;
        thread::sleep(SETTLE);
        Ok(())
    }

    // --- calls ---

    fn calls(&mut self) -> SandboxResult<Vec<CallRow>> {
        match self.ctl.request(None, Request::Calls)? {
            Response::Calls { calls } => Ok(calls),
            other => Err(format!("calls: {other:?}").into()),
        }
    }

    fn wait_call(&mut self, what: &str, wanted: impl Fn(&CallRow) -> bool) -> SandboxResult<CallRow> {
        let deadline = Instant::now() + WAIT;
        loop {
            let calls = self.calls()?;
            if let Some(row) = calls.iter().find(|row| wanted(row)) {
                return Ok(row.clone());
            }
            if Instant::now() > deadline {
                return Err(format!("{what}: the calls are {calls:?}").into());
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// The call window's id, once niri shows a window other than the main window with the focus.
    fn wait_call_window(&mut self) -> SandboxResult {
        let main = self.main_window;
        let id = wait_until(WAIT, || {
            self.windows().ok()?.iter().find_map(|window| {
                let id = window["id"].as_u64()?;
                (Some(id) != main && window["is_focused"] == true).then_some(id)
            })
        })
        .ok_or("no call window")?;
        self.call_window = Some(id);
        wait_until(WAIT, || {
            self.window_size(id)
                .ok()
                .filter(|size| size.0 == i32::try_from(SCREEN.0).unwrap_or(0))
        })
        .ok_or("the call window never filled the screen")?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// b rings a in the a–b–c chat's group call; a answers (as `ngteamsctl answer` does).
    fn group_call(&mut self) -> SandboxResult {
        self.call = Some(self.mock.ring_group(LAB_B, ABC_CHAT, &[LAB_A]));
        self.wait_call("ringing", |row| row.ringing)?;
        self.ctl.request(None, Request::Answer)?;
        self.wait_call("connected", |row| row.state == "connected")?;
        self.wait_call_window()?;
        self.park()
    }

    /// a leaves the call it is in.
    fn hang_up(&mut self) -> SandboxResult {
        if self.calls()?.is_empty() {
            return Ok(());
        }
        self.ctl.request(None, Request::HangUp)?;
        let deadline = Instant::now() + WAIT;
        while self.calls()?.iter().any(|row| row.state == "connected") {
            if Instant::now() > deadline {
                return Err("the call never ended".into());
            }
            thread::sleep(Duration::from_millis(100));
        }
        self.call = None;
        self.call_window = None;
        thread::sleep(SETTLE);
        Ok(())
    }
}

// --- the checks ---

fn items(state: &Value) -> Vec<Value> {
    state["chat"]["items"].as_array().cloned().unwrap_or_default()
}

/// The chat's message that is the sticker `label` alone (`own` or not).
fn sticker_item(state: &Value, own: bool) -> SandboxResult<Value> {
    items(state)
        .into_iter()
        .find(|item| item["own"] == own && item["sticker_alone"] == true)
        .ok_or_else(|| format!("no sticker message (own {own}): {state}").into())
}

fn id_of(item: &Value) -> String {
    item["id"].as_str().unwrap_or_default().to_owned()
}

/// The chat's background beside a sticker: the px 150 px away from its square, on the side away from the window's edge.
fn ground_beside(image: &RgbImage, found: Found, own: bool) -> SandboxResult<[u8; 3]> {
    let x = if own {
        found.sticker.x - 150
    } else {
        found.sticker.right() + 150
    };
    pixel(image, x, found.sticker.y + 10).ok_or_else(|| "no ground beside the sticker".into())
}

/// b's sticker (the leftmost) and a's own (the rightmost), as the screen shows them.
fn theirs_and_own(found: &[Found]) -> Option<(Found, Found)> {
    let theirs = found.iter().min_by_key(|one| one.face.x)?;
    let own = found.iter().max_by_key(|one| one.face.x)?;
    (own.face.x - theirs.face.x > 100).then_some((*theirs, *own))
}

/// Each of b's and a's stickers in `image`, at its size and with no bubble; b's left of a's.
fn check_bare(image: &RgbImage, found: &[Found], width: i32, what: &str) -> SandboxResult<Vec<String>> {
    let mut faults = Vec::new();
    let Some((theirs, own)) = theirs_and_own(found) else {
        return Ok(vec![format!("{what}: no sticker of b's left of one of a's: {found:?}")]);
    };
    for (sticker, is_own) in [(theirs, false), (own, true)] {
        let who = if is_own { "a's" } else { "b's" };
        if !(100..=124).contains(&sticker.face.w) || !(100..=124).contains(&sticker.face.h) {
            faults.push(format!(
                "{what}: {who} face is {}x{}, not about 111 (not drawn 1:1)",
                sticker.face.w, sticker.face.h
            ));
        }
        let ground = ground_beside(image, sticker, is_own)?;
        let (across, down) = runs(image, sticker, ground);
        if !(176..=186).contains(&across) || !(176..=186).contains(&down) {
            faults.push(format!(
                "{what}: {who} sticker reaches {across}x{down} px from the chat's ground {ground:?}, not 180x180 bare"
            ));
        }
        if sticker.sticker.x < 0 || sticker.sticker.right() > width {
            faults.push(format!(
                "{what}: {who} sticker is cut: {:?} in a {width} px window",
                sticker.sticker
            ));
        }
    }
    if theirs.face.x >= own.face.x {
        faults.push(format!(
            "{what}: b's sticker is not left of a's: {:?} {:?}",
            theirs.face, own.face
        ));
    }
    if own.sticker.right() < width - 60 {
        faults.push(format!(
            "{what}: a's own sticker is not on the right: {:?}",
            own.sticker
        ));
    }
    if theirs.sticker.overlaps(own.sticker) {
        faults.push(format!("{what}: the stickers overlap"));
    }
    Ok(faults)
}

/// Each sticker that the chat shows whole (not scrolled under its header): at its size, with no bubble, inside the
/// window. One at least.
fn check_visible(image: &RgbImage, found: &[Found], width: i32, what: &str) -> SandboxResult<Vec<String>> {
    // the chat's header ends about 96 px down at every font size
    let whole: Vec<Found> = found
        .iter()
        .copied()
        .filter(|one| {
            if width < 700 {
                one.face.y >= 100
            } else {
                one.sticker.y >= 100
            }
        })
        .collect();
    if whole.is_empty() {
        return Ok(vec![format!("{what}: no whole sticker on the screen: {found:?}")]);
    }
    let mut faults = Vec::new();
    for sticker in whole {
        if width < 700 {
            // a narrow chat pane: the app may scale the sticker down to the pane, but keeps it square and whole
            let square = sticker.face.w.abs_diff(sticker.face.h) <= 3 && sticker.face.w >= 50;
            if !square || sticker.face.right() > width {
                faults.push(format!("{what}: a face is {:?}: cut or not square", sticker.face));
            }
            let (across, down) = frame_size(image, sticker);
            if across.abs_diff(down) > 6 {
                faults.push(format!(
                    "{what}: the sticker's frame is {across}x{down} px around a {}x{} face, not square",
                    sticker.face.w, sticker.face.h
                ));
            }
            continue;
        }
        let own = sticker.face.x > width / 2 + 100;
        if !(100..=124).contains(&sticker.face.w) || !(100..=124).contains(&sticker.face.h) {
            faults.push(format!(
                "{what}: a face is {:?}, not about 111 px (cut or scaled)",
                sticker.face
            ));
        }
        let ground = ground_beside(image, sticker, own)?;
        let (across, down) = runs(image, sticker, ground);
        if !(176..=186).contains(&across) || !(176..=186).contains(&down) {
            faults.push(format!(
                "{what}: a sticker at {:?} reaches {across}x{down} px from the ground {ground:?}, not 180x180 bare",
                sticker.sticker
            ));
        }
        if sticker.sticker.x < 0 || sticker.sticker.right() > width {
            faults.push(format!(
                "{what}: a sticker is cut: {:?} in a {width} px window",
                sticker.sticker
            ));
        }
    }
    Ok(faults)
}

/// b's sticker and a's own in "Lab stickers": each alone, bare at 180 by 180, b's on the left under b's name and
/// time, a's on the right with its time; in Teams Light and Teams Dark.
fn bare_stickers(user: &mut User<'_>) -> SandboxResult {
    let chat = user.chats.stickers.clone();
    let state = user.open(&chat, 3)?;
    let theirs = sticker_item(&state, false)?;
    let own = sticker_item(&state, true)?;
    let mut faults = Vec::new();
    if theirs["bare"] != true || own["bare"] != true {
        faults.push(format!("not drawn bare: b's {theirs}, a's {own}"));
    }
    if theirs["sender"] != "Lab B" {
        faults.push(format!("b's sticker is not under b's name: {theirs}"));
    }
    for item in [&theirs, &own] {
        if item["label"] != HI_LABEL {
            faults.push(format!("the label: {item}"));
        }
        if item["size"] != json!([180, 180]) {
            faults.push(format!("the size the message gives: {item}"));
        }
    }
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        let (image, found) = user.stickers(&format!("bare-{theme}"), 2)?;
        faults.extend(check_bare(&image, &found, 1280, theme)?);
    }
    ensure(faults.is_empty(), || faults.join("; "))
}

/// The sticker's GIF plays: its mouth changes between screenshots, taken until it has or [`WAIT`] has passed.
fn gif_moves(user: &mut User<'_>) -> SandboxResult {
    let chat = user.chats.stickers.clone();
    user.open(&chat, 3)?;
    // GPUI plays a GIF only in the active window
    user.until("the main window active", |state| state["active"] == true)?;
    let (_, found) = user.stickers("gif-frame-0", 2)?;
    let start = Instant::now();
    let mut seen = BTreeSet::new();
    let mut frame = 0;
    while seen.len() < 2 && start.elapsed() < WAIT {
        frame += 1;
        let path = user.shot(&format!("gif-frame-{frame}"))?;
        let image = image::open(path)?.to_rgb8();
        for sticker in stickers_in(&image).into_iter().take(1) {
            seen.insert(mouth_ink(&image, sticker));
        }
        thread::sleep(Duration::from_millis(140));
    }
    say(&format!(
        "  mouth ink seen: {seen:?} in {frame} shots ({} stickers)",
        found.len()
    ));
    if seen.len() >= 2 {
        return Ok(());
    }
    // a state lookup that fails must not hide the frame counts
    let active = user
        .state()
        .map_or_else(|err| format!("unknown ({err})"), |state| state["active"].to_string());
    Err(
        format!("the GIF does not move: the mouth is always {seen:?} in {frame} shots; the window active: {active}")
            .into(),
    )
}

/// b's sticker under b's words in one message: in the bubble, under the text.
fn beside_text(user: &mut User<'_>) -> SandboxResult {
    let chat = user.chats.words.clone();
    let state = user.open(&chat, 1)?;
    let item = items(&state).into_iter().last().ok_or("no message")?;
    let mut faults = Vec::new();
    if item["bare"] != false || item["sticker_alone"] != false {
        faults.push(format!("words and a sticker drawn as a sticker alone: {item}"));
    }
    if !item["lines"].to_string().contains("Look at this made-up one") {
        faults.push(format!("the words are not there: {item}"));
    }
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        let (image, found) = user.stickers(&format!("beside-text-{theme}"), 1)?;
        let sticker = found[found.len() - 1];
        // in a bubble: the px just right of the sticker's square is the bubble's, not the chat's ground
        let ground = ground_beside(&image, sticker, false)?;
        let beside = pixel(&image, sticker.sticker.right() + 6, sticker.sticker.y + 90).ok_or("no px")?;
        if near(beside, ground, 4) {
            faults.push(format!(
                "{theme}: no bubble around the words and the sticker ({beside:?})"
            ));
        }
        if !(100..=124).contains(&sticker.face.w) {
            faults.push(format!("{theme}: the sticker is not at its size: {:?}", sticker.face));
        }
    }
    ensure(faults.is_empty(), || faults.join("; "))
}

/// A click on b's sticker: no picture viewer, no new window.
fn click_opens_no_viewer(user: &mut User<'_>) -> SandboxResult {
    let chat = user.chats.stickers.clone();
    user.open(&chat, 3)?;
    let (_, found) = user.stickers("click-before", 2)?;
    let (theirs, own) = theirs_and_own(&found).ok_or("no sticker of b's and of a's")?;
    let windows = user.windows()?.len();
    let opened = user.app.trace.count("chat-view", "picture opened");
    user.click(theirs.centre())?;
    user.click(own.centre())?;
    thread::sleep(Duration::from_millis(1500));
    user.settled_shot("click-after")?;
    let state = user.state()?;
    let mut faults = Vec::new();
    if state["viewer"] == true {
        faults.push("the picture viewer opened".to_owned());
    }
    if user.windows()?.len() != windows {
        faults.push(format!(
            "a window opened: {} windows, {windows} before",
            user.windows()?.len()
        ));
    }
    if user.app.trace.count("chat-view", "picture opened") != opened {
        faults.push("the trace says a picture opened".to_owned());
    }
    ensure(faults.is_empty(), || faults.join("; "))
}

/// The hover bar over a sticker: found by what the pointer's hover changes above the sticker's square.
fn hover_bar(user: &mut User<'_>, found: Found, name: &str) -> SandboxResult<Rect> {
    let before = user.image(&format!("{name}-no-hover"))?;
    user.point(found.centre())?;
    let path = user.shot(&format!("{name}-hover"))?;
    let after = image::open(path)?.to_rgb8();
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for y in found.sticker.y - 44..found.sticker.y + 2 {
        // the bar is wider than a sticker: it reaches past the sticker on the side away from the window's edge
        for x in found.sticker.x - 340..found.sticker.right() + 120 {
            let (Some(one), Some(other)) = (pixel(&before, x, y), pixel(&after, x, y)) else {
                continue;
            };
            let change: u32 = one.iter().zip(other).map(|(a, b)| u32::from(a.abs_diff(b))).sum();
            if change > 60 {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
    }
    ensure(x1 > x0 && x1 - x0 > 100, || {
        format!("no hover bar over the sticker at {:?}", found.sticker)
    })?;
    Ok(Rect {
        x: x0,
        y: y0,
        w: x1 - x0 + 1,
        h: y1 - y0 + 1,
    })
}

/// Over b's sticker: the hover bar; its first quick reaction reacts; Reply quotes the sticker; "More options" opens
/// its menu, whose "Copy text" works; a's own sticker's bar offers no Edit.
fn hover_bar_and_menu(user: &mut User<'_>) -> SandboxResult {
    let chat = user.chats.stickers.clone();
    let state = user.open(&chat, 3)?;
    let theirs = id_of(&sticker_item(&state, false)?);
    let (_, found) = user.stickers("menu-start", 2)?;
    let (sticker, _) = theirs_and_own(&found).ok_or("no sticker of b's and of a's")?;
    let bar = hover_bar(user, sticker, "theirs")?;
    let state = user.state()?;
    let mut faults = Vec::new();
    if state["chat"]["hovered"] != theirs.as_str() {
        faults.push(format!("the hover is not on b's sticker: {}", state["chat"]["hovered"]));
    }
    let actions = sticker_item(&state, false)?["actions"].clone();
    say(&format!("  b's sticker offers {actions}; its bar {bar:?}"));
    // the bar, left to right: the quick reactions, React, Reply, More options (28 px buttons, 2 px apart)
    let middle = bar.y + bar.h / 2;
    let more = (bar.right() - 16, middle);
    user.click(more)?;
    let state = user.until("the menu of b's sticker", |state| {
        state["chat"]["options"] != Value::Null
    });
    match state {
        Ok(_) => {
            user.shot("theirs-menu")?;
            // b's words first, to show that the clipboard can be read here
            let words = items(&user.state()?).first().map(id_of).unwrap_or_default();
            user.ask(&format!("act copy {words}"))?;
            let control = user.ask("clipboard")?;
            user.ask(&format!("act copy {theirs}"))?;
            let copied = user.ask("clipboard")?;
            say(&format!(
                "  Copy text: b's words copied {control}, b's sticker copied {copied}"
            ));
            if control.contains("Made-up sticker test") && !copied.contains(HI_LABEL) {
                faults.push(format!("Copy text on b's sticker copies {copied}, not its label"));
            }
            user.press("escape")?;
        }
        Err(error) => faults.push(format!("More options: {error}")),
    }
    user.park()?;
    thread::sleep(SETTLE);
    // Reply before the reaction: the reaction's row under the sticker moves the messages
    user.point(sticker.centre())?;
    let reply = (more.0 - 30, middle);
    user.click(reply)?;
    if let Err(error) = user.until("Reply on b's sticker", |state| {
        state["chat"]["compose"]["mode"] == "reply"
    }) {
        faults.push(format!("Reply: {error}"));
    }
    user.settled_shot("theirs-reply-box")?;
    user.ask("clear")?;
    user.park()?;
    thread::sleep(SETTLE);
    user.point(sticker.centre())?;
    let first = (bar.x + 16, middle);
    user.click(first)?;
    if let Err(error) = user.wait_sent("PUT", "emotions") {
        faults.push(format!("the first quick reaction: {error}"));
    }
    user.settled_shot("theirs-reacted")?;
    ensure(faults.is_empty(), || faults.join("; "))
}

/// a's own sticker: Edit is not offered (not on its hover bar, not by Up in the empty box).
fn own_sticker_no_edit(user: &mut User<'_>) -> SandboxResult {
    let chat = user.chats.stickers.clone();
    let state = user.open(&chat, 3)?;
    let own = sticker_item(&state, true)?;
    let (_, found) = user.stickers("own-start", 2)?;
    let (_, mine) = theirs_and_own(&found).ok_or("no sticker of b's and of a's")?;
    let bar = hover_bar(user, mine, "own")?;
    user.click((bar.right() - 16, bar.y + bar.h / 2))?;
    thread::sleep(SETTLE);
    user.shot("own-menu")?;
    user.press("escape")?;
    user.ask("focus-box")?;
    user.press("up")?;
    thread::sleep(SETTLE);
    let state = user.state()?;
    user.settled_shot("own-up-in-box")?;
    let mut faults = Vec::new();
    if own["actions"]
        .as_array()
        .is_some_and(|actions| actions.iter().any(|action| action == "Edit"))
    {
        faults.push(format!("Edit is offered on a's own sticker: {}", own["actions"]));
    }
    let compose = &state["chat"]["compose"];
    if compose["mode"] == "edit" || compose["problem"] != Value::Null {
        faults.push(format!("Up in the empty box takes the sticker to edit: {compose}"));
    }
    user.ask("clear")?;
    ensure(faults.is_empty(), || faults.join("; "))
}

/// a replies to b's sticker: the box quotes it, the reply goes with its quote, and the chat draws the quote.
fn quoted_sticker(user: &mut User<'_>) -> SandboxResult {
    let chat = user.chats.stickers.clone();
    let state = user.open(&chat, 3)?;
    let theirs = id_of(&sticker_item(&state, false)?);
    user.ask(&format!("act reply {theirs}"))?;
    let state = user.until("the reply box", |state| state["chat"]["compose"]["mode"] == "reply")?;
    say(&format!("  the box: {}", state["chat"]["compose"]));
    user.ask("focus-box")?;
    let mark = user.mark();
    user.type_text(&format!("a quoted reply {mark}"))?;
    user.settled_shot("quote-in-box")?;
    user.press("enter")?;
    let sent = user.wait_sent("POST", &mark)?;
    say(&format!("  sent: {sent}"));
    let state = user.until("the reply in the chat", |state| {
        items(state)
            .iter()
            .any(|item| item["lines"].to_string().contains(&mark))
    })?;
    let reply = items(&state)
        .into_iter()
        .find(|item| item["lines"].to_string().contains(&mark))
        .ok_or("no reply")?;
    say(&format!("  drawn: {reply}"));
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        user.settled_shot(&format!("quote-drawn-{theme}"))?;
    }
    let mut faults = Vec::new();
    if !sent.contains("schema.skype.com/Reply") || !sent.contains(&theirs) {
        faults.push(format!("the reply carries no quote of b's sticker: {sent}"));
    }
    if !sent.contains(HI_LABEL) {
        faults.push(format!("the quote does not name the sticker: {sent}"));
    }
    let quote = reply["quote"].to_string();
    if !quote.contains(HI_LABEL) {
        faults.push(format!("the drawn quote does not name the sticker: {quote}"));
    }
    ensure(faults.is_empty(), || faults.join("; "))
}

/// The sticker's label in the chat list's preview, in search, and in the banner of a sticker that comes live.
fn preview_search_banner(user: &mut User<'_>) -> SandboxResult {
    let chat = user.chats.stickers.clone();
    user.open(&chat, 3)?;
    let mut faults = Vec::new();
    let chats = match user.ctl.request(
        None,
        Request::Chats {
            unread_only: false,
            limit: None,
        },
    )? {
        Response::Chats { chats } => chats,
        other => return Err(format!("chats: {other:?}").into()),
    };
    let row = chats
        .iter()
        .find(|row| row.id == chat)
        .ok_or("no sticker chat in the list")?;
    say(&format!("  the list's preview: {:?}", row.preview));
    if !row.preview.as_deref().unwrap_or_default().contains(HI_LABEL) {
        faults.push(format!("the list's preview: {:?}", row.preview));
    }
    let (_, list_preview) = user.list_preview(&chat)?;
    say(&format!("  the list row: {list_preview}"));
    if !list_preview.contains(HI_LABEL) {
        faults.push(format!("the list row: {list_preview}"));
    }
    user.settled_shot("list-preview")?;
    let hits = match user.ctl.request(
        None,
        Request::Search {
            query: "made-up sticker says".to_owned(),
            limit: None,
        },
    )? {
        Response::Search { messages } => messages,
        other => return Err(format!("search: {other:?}").into()),
    };
    say(&format!(
        "  search: {:?}",
        hits.iter().map(|hit| (&hit.chat, &hit.text)).collect::<Vec<_>>()
    ));
    if !hits.iter().any(|hit| hit.chat == chat && hit.text.contains(HI_LABEL)) {
        faults.push(format!("search finds no sticker: {} hits", hits.len()));
    }
    user.ask("search made-up sticker says")?;
    thread::sleep(Duration::from_secs(2));
    user.settled_shot("search-results")?;
    user.press("escape")?;
    // a banner: the sticker comes while another chat is shown
    user.open(AB_CHAT, 1)?;
    let shown = user.sandbox.bus().notifications().len();
    user.mock
        .post_as(LAB_B, &chat, &read_back(WAVE, WAVE_LABEL))
        .ok_or("b's live sticker refused")?;
    let banner = wait_until(WAIT, || user.sandbox.bus().notifications().get(shown).cloned());
    match banner {
        Some(banner) => {
            say(&format!("  banner: {:?} / {:?}", banner.summary, banner.body));
            if !banner.body.contains(WAVE_LABEL) {
                faults.push(format!("the banner says {:?}", banner.body));
            }
        }
        None => faults.push("no banner for the live sticker".to_owned()),
    }
    user.settled_shot("banner-list")?;
    ensure(faults.is_empty(), || faults.join("; "))
}

/// The chat list's row of `chat` as the list shows it.
impl User<'_> {
    fn list_preview(&mut self, chat: &str) -> SandboxResult<(Value, String)> {
        let state = self.state()?;
        let row = state["list"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] == chat).cloned())
            .unwrap_or(Value::Null);
        let preview = row["preview"].as_str().unwrap_or_default().to_owned();
        Ok((row, preview))
    }
}

/// b's stickers the CDN fails: each its placeholder with its label, no crash; the slow one is not read without end.
fn broken_and_slow_cdn(user: &mut User<'_>) -> SandboxResult {
    let chat = user.chats.broken.clone();
    let started = Instant::now();
    user.open(&chat, BROKEN.len())?;
    thread::sleep(Duration::from_secs(2));
    let early = user.ask("pictures")?;
    say(&format!("  after 2 s: {early}"));
    user.ask("theme-light")?;
    user.settled_shot("broken-early-light")?;
    let mut faults = Vec::new();
    let mut last;
    let settled = loop {
        let pictures: Value = serde_json::from_str(&user.ask("pictures")?)?;
        let reading = pictures
            .as_array()
            .is_some_and(|all| all.iter().any(|picture| picture["state"] == "reading"));
        last = pictures;
        if !reading {
            break true;
        }
        if started.elapsed() > SLOW + Duration::from_secs(20) {
            break false;
        }
        thread::sleep(Duration::from_secs(1));
    };
    say(&format!("  after {:?}: {last}", started.elapsed()));
    if !settled {
        faults.push(format!("still reading after {:?}: {last}", started.elapsed()));
    }
    for picture in last.as_array().cloned().unwrap_or_default() {
        if picture["state"] == "shown" {
            faults.push(format!("a broken sticker shows a picture: {picture}"));
        }
    }
    let state = user.state()?;
    let labels: Vec<String> = items(&state)
        .iter()
        .map(|item| item["label"].as_str().unwrap_or_default().to_owned())
        .collect();
    for (_, label) in BROKEN {
        if !labels.iter().any(|one| one == label) {
            faults.push(format!("no sticker labelled {label:?}: {labels:?}"));
        }
    }
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        user.settled_shot(&format!("broken-settled-{theme}"))?;
    }
    ensure(faults.is_empty(), || faults.join("; "))
}

/// "Lab stickers" at font 20 and 24, 800 and 520 px wide, both themes: each sticker whole, at its size, apart.
fn themes_fonts_narrow(user: &mut User<'_>) -> SandboxResult {
    let chat = user.chats.stickers.clone();
    user.open(&chat, 3)?;
    let mut faults = Vec::new();
    for (width, font) in [(1280, 20), (1280, 24), (800, 14), (520, 14), (520, 24)] {
        user.focus_window(user.main_window)?;
        user.resize_focused(width)?;
        user.ask(&format!("font {font}"))?;
        for theme in ["light", "dark"] {
            user.ask(&format!("theme-{theme}"))?;
            let name = format!("size-{width}-font-{font}-{theme}");
            match user.stickers(&name, 1) {
                Ok((image, found)) => faults.extend(check_visible(&image, &found, width, &name)?),
                Err(error) => faults.push(format!("{name}: {error}")),
            }
        }
    }
    user.ask("font 14")?;
    user.resize_focused(1280)?;
    ensure(faults.is_empty(), || faults.join("; "))
}

/// The call's Chat pane: b's sticker there, bare at its size.
fn call_chat_pane(user: &mut User<'_>) -> SandboxResult {
    user.group_call()?;
    user.ask("call-show-chat")?;
    user.until("the call's Chat pane", |state| state["call"]["chat_shown"] == true)?;
    let mut faults = Vec::new();
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        let name = format!("call-chat-{theme}");
        match user.stickers(&name, 1) {
            Ok((image, found)) => {
                let sticker = found[found.len() - 1];
                let ground = ground_beside(&image, sticker, true)?;
                let (across, down) = runs(&image, sticker, ground);
                say(&format!("  {name}: {:?}, {across}x{down}", sticker.sticker));
                if !(100..=124).contains(&sticker.face.w) {
                    faults.push(format!("{name}: the sticker is not at its size: {:?}", sticker.face));
                }
                if sticker.sticker.x < 0 || sticker.sticker.right() > i32::try_from(SCREEN.0)? {
                    faults.push(format!("{name}: the sticker is cut: {:?}", sticker.sticker));
                }
            }
            Err(error) => faults.push(format!("{name}: {error}")),
        }
    }
    user.hang_up()?;
    ensure(faults.is_empty(), || faults.join("; "))
}

/// The CDN's reads of the sticker so far, for the restart.
fn reads_before_restart(user: &mut User<'_>) -> SandboxResult {
    let chat = user.chats.stickers.clone();
    user.open(&chat, 3)?;
    user.stickers("before-restart", 2)?;
    let reads = user.cdn_reads(HI);
    say(&format!("  CDN reads of the sticker: {reads}"));
    user.reads_before = Some(reads);
    ensure(reads >= 1, || "the sticker was never read".to_owned())
}

/// After a restart: the sticker shows, and the CDN was not read again.
fn cached_after_restart(user: &mut User<'_>) -> SandboxResult {
    let chat = user.chats.stickers.clone();
    user.open(&chat, 3)?;
    let (image, found) = user.stickers("after-restart", 2)?;
    let faults = check_bare(&image, &found, 1280, "after restart")?;
    let reads = user.cdn_reads(HI);
    say(&format!(
        "  CDN reads of the sticker: {reads} (before the restart {:?})",
        user.reads_before
    ));
    let mut faults = faults;
    if let Some(before) = user.reads_before
        && reads != before
    {
        faults.push(format!(
            "the CDN was read again: {reads} reads, {before} before the restart"
        ));
    }
    ensure(faults.is_empty(), || faults.join("; "))
}

// --- the child ---

/// The account's mock tokens; they never run out in a test.
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

/// a's engine on the mock (its chats, teams and calling), its calls' media on file devices, relayed through the
/// mock's TURN server.
fn engine(feed: VideoFeed, dir: &Path) -> SandboxResult<EngineHandle> {
    let tokens = Arc::new(Tokens {
        ic3: env(IC3_ENV)?,
        skypetoken: env(SKYPE_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let tracker = Arc::new(teams_call::Tracker::new(Mri::new(LAB_A)));
    let origin = env(ORIGIN_ENV)?;
    let trouter_origin = env(TROUTER_ENV)?;
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
    let transport = Arc::new(http()?);
    let options = Options {
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(200),
        backoff_jitter_percent: 0,
        ..Options::default()
    };
    let trouter = || -> SandboxResult<_> {
        Ok(
            Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), &trouter_origin)?
                .with_options(options),
        )
    };
    let calling = trouter()?.for_calls(CallStack::new(Arc::clone(&tracker)));
    let name = display_name("bot-a@lab.example.test").to_owned();
    let identity = Identity {
        mri: Mri::new(LAB_A),
        display_name: Some(name.clone()),
        endpoint_id: String::new(),
        language_id: Some("en-us".into()),
        tenant_id: TENANT.into(),
        client: web_client_info(),
        region: Some("amer".into()),
        partition: Some("amer01".into()),
    };
    let devices: Arc<dyn Devices + Send + Sync> = Arc::new(FileDevices {
        input: AudioInput::Silence,
        recording: None,
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    });
    let media = CallMedia {
        devices,
        video: Some(teams_core::account::call_video(Arc::new(feed))),
        relay: Relay::Fixed(vec![RelayServer::udp(env(RELAY_ENV)?.parse()?)]),
        session: SessionConfig {
            processing: ProcessingConfig::OFF,
        },
    };
    let push = Push::new(trouter()?);
    let client = http()?
        .with_ams(AMS_BASE)?
        .with_presence(PRESENCE_BASE)?
        .with_csa(CSA_BASE)?
        .with_user_profile(USER_PROFILE_BASE)?
        .with_provisioning(PROVISIONING_BASE)?
        .with_middle_tier(MIDDLE_TIER_BASE)?;
    Ok(engine::start(Parts {
        calls: Some(CallParts::new(calling, identity, media)),
        store: Store::open(&support::store_file(dir, Alias::A))?,
        client: Arc::new(ChatClient::new(client)),
        outbox: Outbox::new(transport, Author::new(Mri::new(LAB_A), name))
            .with_calls(Arc::clone(&tracker), teams_core::CallFloor::new()),
        push: Some(push),
        options: EngineOptions::default(),
        known_names: vec![
            (LAB_B.to_owned(), "Lab B".to_owned()),
            (LAB_C.to_owned(), "Lab C".to_owned()),
        ],
    })?)
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let feed = VideoFeed::new();
    let engine = engine(feed.clone(), &dir)?;
    let first = support::started(Alias::A, engine)?;
    let runner = support::runner(&dir)?;
    let settings = SettingsFile::new(dir.join("settings.toml"));
    let mut opened = settings.load().settings;
    opened.selected_chat = Some(AB_CHAT.to_owned());
    settings.save(&opened).map_err(|problem| format!("{problem:?}"))?;
    let ctl = PathBuf::from(env(CTL_ENV)?);
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first: ngteams::accounts::Started {
                    calls: Some(PersonalCalls {
                        ringer: None,
                        sounds: None,
                        echo: Mri::new(ECHO),
                        video: Some(feed),
                        camera: None,
                        devices: None,
                    }),
                    ..first
                },
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

fn on_app(window: WindowHandle<KitRoot>, verb: &str, cx: &mut App) -> String {
    match verb {
        "clipboard" => {
            return cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .map_or_else(|| "none".to_owned(), |text| json!(text).to_string());
        }
        "pictures" => return pictures(window, cx).to_string(),
        "call-show-chat" => return show_call_chat(cx),
        _ => {}
    }
    let Some(shell) = shell_of(window, cx) else {
        return "no shell".to_owned();
    };
    let reply = window
        .update(cx, |_, window, cx| on_shell(&shell, verb, window, cx))
        .unwrap_or_else(|_| "no window".to_owned());
    if verb != "state" {
        return reply;
    }
    let Ok(mut state) = serde_json::from_str::<Value>(&reply) else {
        return reply;
    };
    state["viewer"] = json!(teams_ui::picture_viewer::shown(cx).is_some());
    state["call"] = call_state(cx);
    state.to_string()
}

/// The call window's stage and its window, if one is open.
fn call_stage(cx: &App) -> Option<(WindowHandle<KitRoot>, Entity<teams_ui::call_stage::CallStage>)> {
    cx.windows().into_iter().find_map(|window| {
        let kit = window.downcast::<KitRoot>()?;
        let stage = kit
            .read(cx)
            .ok()?
            .view()
            .clone()
            .downcast::<teams_ui::call_stage::CallStage>()
            .ok()?;
        Some((kit, stage))
    })
}

fn call_state(cx: &mut App) -> Value {
    let Some((_, stage)) = call_stage(cx) else {
        return json!({"open": false});
    };
    json!({"open": true, "chat_shown": stage.read(cx).chat_shown()})
}

fn show_call_chat(cx: &mut App) -> String {
    let Some((kit, stage)) = call_stage(cx) else {
        return "no call window".to_owned();
    };
    kit.update(cx, |_, window, cx| {
        stage.update(cx, |stage, cx| stage.show_chat(true, window, cx));
        "ok".to_owned()
    })
    .unwrap_or_else(|_| "no call window".to_owned())
}

/// Each sticker of the open chat's messages and how its picture stands (only ones the view already asked for).
fn pictures(window: WindowHandle<KitRoot>, cx: &mut App) -> Value {
    let Some(shell) = shell_of(window, cx) else {
        return json!([]);
    };
    let Some(chat) = shell.read(cx).chat().cloned() else {
        return json!([]);
    };
    let mut asked = Vec::new();
    for item in chat.read(cx).message_items() {
        let Body::Text(text) = &item.body else { continue };
        let Some(source) = text.0.source.clone() else { continue };
        for block in &text.0.blocks {
            if let Block::Picture(picture) = block
                && picture.sticker
            {
                asked.push((picture.src.clone(), picture.alt.clone(), source.clone()));
            }
        }
    }
    let all: Vec<Value> = asked
        .into_iter()
        .map(|(src, alt, source)| {
            let state = match teams_ui::inline::picture(&src, &source, cx) {
                teams_ui::inline::PictureState::Reading => "reading",
                teams_ui::inline::PictureState::Shown(..) => "shown",
                teams_ui::inline::PictureState::Missing => "missing",
            };
            json!({"src": src, "label": alt, "state": state})
        })
        .collect();
    json!(all)
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
        "reset" => {
            change_settings(shell, cx, |settings| {
                settings.appearance.follow_system = false;
                settings.appearance.theme = teams_ui::theme::LIGHT_KEY;
                settings.appearance.font_size = 14;
            });
            if let Some(chat) = shell.read(cx).chat().cloned() {
                chat.update(cx, |chat, cx| chat.hover(None, cx));
            }
            "ok".to_owned()
        }
        "clear" | "focus-box" => {
            let Some(chat) = shell.read(cx).chat().cloned() else {
                return "no chat".to_owned();
            };
            let compose = chat.read(cx).compose().clone();
            compose.update(cx, |compose, cx| {
                if verb == "clear" {
                    compose.discard(window, cx);
                }
                compose.focus(window, cx);
            });
            "ok".to_owned()
        }
        other => on_shell_with(shell, other, window, cx),
    }
}

/// A verb with an argument: a chat to open, an action on a message, a search, the font.
fn on_shell_with(shell: &Entity<Shell>, other: &str, window: &mut Window, cx: &mut App) -> String {
    if let Some(id) = other.strip_prefix("open ") {
        shell.update(cx, |shell, cx| shell.open_chat(id.to_owned(), window, cx));
        "ok".to_owned()
    } else if let Some(rest) = other.strip_prefix("act ") {
        let Some((action, id)) = rest.split_once(' ') else {
            return format!("bad act {rest}");
        };
        let action = match action {
            "reply" => MessageAction::Reply,
            "react" => MessageAction::React,
            "copy" => MessageAction::Copy,
            "edit" => MessageAction::Edit,
            other => return format!("no action {other}"),
        };
        let Some(chat) = shell.read(cx).chat().cloned() else {
            return "no chat".to_owned();
        };
        chat.update(cx, |chat, cx| chat.act(action, id, window, cx));
        "ok".to_owned()
    } else if let Some(query) = other.strip_prefix("search ") {
        shell.update(cx, |shell, cx| shell.search_for(query, window, cx));
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

fn state_of(shell: &Entity<Shell>, window: &Window, cx: &App) -> Value {
    let shell_ref = shell.read(cx);
    let main = match shell_ref.main() {
        Main::Chat(id) => format!("chat:{id}"),
        other => format!("{other:?}"),
    };
    let chat = shell_ref
        .chat()
        .filter(|_| matches!(shell_ref.main(), Main::Chat(_)))
        .map(|chat| chat_state(chat, window, cx));
    json!({
        "main": main,
        "active": window.is_window_active(),
        "chat": chat,
        "list": list_state(shell_ref, cx),
    })
}

/// The chat list's rows as it shows them: each chat's id and preview.
fn list_state(shell: &Shell, cx: &App) -> Value {
    let rows: Vec<Value> = shell
        .list()
        .read(cx)
        .rows()
        .iter()
        .map(|row| json!({"id": row.id, "preview": row.preview.to_string()}))
        .collect();
    json!(rows)
}

fn chat_state(chat: &Entity<ChatView>, window: &Window, cx: &App) -> Value {
    let view = chat.read(cx);
    let items: Vec<Value> = view.message_items().map(item_state).collect();
    let compose = view.compose().read(cx);
    let mode = match compose.mode() {
        Mode::New => "new",
        Mode::Reply(_) => "reply",
        Mode::Edit { .. } => "edit",
    };
    json!({
        "id": view.conversation(),
        "items": items,
        "hovered": view.hovered(),
        "options": view.options(),
        "selected": view.selected(),
        "compose": {
            "mode": mode,
            "text": compose.value(cx).to_string(),
            "problem": compose.problem().map(ToString::to_string),
            "focused": compose.focused(window, cx),
        },
    })
}

fn item_state(item: &teams_ui::chat_view::MessageItem) -> Value {
    let (bare, sticker_alone, label, size, lines, quote) = match &item.body {
        Body::Text(text) => {
            let rendered = &text.0;
            let sticker = rendered.blocks.iter().find_map(|block| match block {
                Block::Picture(picture) if picture.sticker => Some(picture),
                _ => None,
            });
            let lines: Vec<String> = rendered.blocks.iter().flat_map(Block::plain_lines).collect();
            let quote: Vec<String> = rendered
                .blocks
                .iter()
                .filter_map(|block| match block {
                    Block::Quote(quote) => Some(quote.blocks.iter().flat_map(Block::plain_lines).collect::<Vec<_>>()),
                    _ => None,
                })
                .flatten()
                .collect();
            (
                rendered.bare,
                rendered.sticker_alone().is_some(),
                sticker.and_then(|picture| picture.alt.clone()),
                sticker.and_then(|picture| picture.size).map(|(w, h)| vec![w, h]),
                lines,
                quote,
            )
        }
        _ => (false, false, None, None, Vec::new(), Vec::new()),
    };
    json!({
        "id": item.id,
        "own": item.own,
        "header": item.header,
        "sender": item.sender_name.to_string(),
        "bare": bare,
        "sticker_alone": sticker_alone,
        "label": label,
        "size": size,
        "lines": lines,
        "quote": quote,
        "actions": item.actions().iter().map(|action| action.label()).collect::<Vec<_>>(),
    })
}
