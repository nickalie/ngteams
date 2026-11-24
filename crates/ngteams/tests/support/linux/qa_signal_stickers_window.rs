//! QA of task 652 (round 1), in the UI sandbox: Signal sticker packs in the compose box's picker. The real app runs as
//! a child in the sandbox, with lab accounts a and b in one window on `teams-mock` (its chat service, its media service
//! and its Trouter), two loopback Signal CDNs (`teams_mock::signal`: one that answers well, one that the parent gives
//! faults to) and a mock Telegram Bot API (`teams_mock::telegram`). The parent drives the app with the sandbox's
//! virtual keyboard and pointer. A fake file chooser on the sandbox's bus gives the folder that the user picks. The
//! parent finds the stickers in the screenshots by their colours, and asks the child what the views hold through a
//! file in the sandbox dir (`harness = false`: GPUI runs on the process's main thread).
//!
//! - **The empty state** (a first run with an own server's capabilities, so no Teams packs): its words name Signal
//!   sticker packs; its "Import a Signal pack" button opens the panel; the Telegram and the Signal panels are never
//!   open together.
//! - **The menu row** "Import a Signal pack"; a link pasted with Ctrl+V and Enter: the pack's title, its cover, each
//!   sticker, the emoji labels (the tooltip), an animated sticker as a GIF that plays.
//! - **Errors**: malformed links, an unknown pack (403), a wrong key, a tampered manifest, a tampered and a missing
//!   sticker (skipped with the reason), a CDN that redirects, breaks, drops or cannot be reached (nothing kept).
//! - **The CDN's requests**: no credential, no key, only the manifest and sticker paths. No key in the app's debug log
//!   or its files.
//! - **a sends** a static and an animated Signal sticker; **b sees** them, and the animated one plays.
//! - **The keyboard**: Tab and Shift+Tab round the panel, Enter and Space, Escape. Teams Light and Dark shots.
//! - **No regression**: a folder import, a Telegram link import, Teams' own packs.
//!
//! `QA652_ONLY` (a comma list of check names) runs only those. `QA652_EVIDENCE` names a directory that gets the
//! screenshots, the results, the CDNs' requests and the app's debug log. Screenshots also go to
//! `target/ui-shots/qa652-*.png`. Every name, key, text and picture is made up.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use std::{fs, future, thread};

use gpui::{App, AsyncApp, ClipboardItem, Entity, Window, WindowHandle, WindowingModes};
use image::{ImageFormat, RgbImage, Rgba, RgbaImage};
use ngteams::accounts::Runner;
use ngteams::accounts_file::PersonalPaths;
use ngteams::app::{self, Launch};
use ngteams::descriptors::adopt_trace;
use ngteams::secrets::AppSecrets;
use ngteams::setup::Setup;
use serde_json::{Value, json};
use teams_auth::secrets::{Secret, SecretKey, SecretStore as _};
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::signal::SignalCdn;
use teams_core::telegram::BotApi;
use teams_core::{CallFloor, Outbox};
use teams_mock::signal::{self as cdn, MockFault, MockPack, SignalCdnMock};
use teams_mock::telegram::{BotApiMock, MockKind, MockSet, MockSticker, SYNTHETIC_BOT_TOKEN};
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
use teams_ui::signal_import::{self, SignalImport};
use teams_ui::stickers::signal::SignalCdns;
use teams_ui::stickers::telegram::{BotApis, TOKEN_NAME};
use teams_ui::stickers::{ImportState, Shelf};
use teams_ui::telegram_import::{TelegramImport, TokenState};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::VirtualInput;
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};
use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::interface;
use zbus::message::Header;
use zbus::zvariant::{OwnedObjectPath, Value as Variant};

#[path = "../window.rs"]
#[expect(
    dead_code,
    reason = "the shared window support serves other harnesses too: its runner is not used here"
)]
mod support;
use support::{Alias, display_name};

const CHILD_ENV: &str = "QA652_CHILD";
const ONLY_ENV: &str = "QA652_ONLY";
const EVIDENCE_ENV: &str = "QA652_EVIDENCE";
const TRACE_FD_ENV: &str = "QA652_TRACE_FD";
const ORIGIN_ENV: &str = "QA652_ORIGIN";
const TROUTER_ENV: &str = "QA652_TROUTER";
const DIR_ENV: &str = "QA652_DIR";
const TOKENS_ENV: &str = "QA652_TOKENS";
const BOT_ENV: &str = "QA652_BOT";
const CDN_ENV: &str = "QA652_CDN";
const FAULTY_CDN_ENV: &str = "QA652_FAULTY_CDN";
const DEAD_CDN_ENV: &str = "QA652_DEAD_CDN";
/// The file that keeps the Telegram bot token between runs, as a keyring keeps it.
const SEED_ENV: &str = "QA652_SEED";
/// The file in the sandbox dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "qa652-ask";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(30);
/// The longest an import may take in this debug build.
const IMPORT_WAIT: Duration = Duration::from_secs(240);
const POLL: Duration = Duration::from_millis(60);
const SETTLE: Duration = Duration::from_millis(700);
/// The time between a key or click and the next, so the app sees each.
const STEP: Duration = Duration::from_millis(250);
/// niri's layout: each window takes the whole screen, and no hot corner.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
const SCREEN: (u32, u32) = (1280, 800);

/// The picker on the screen, and the chat's messages (the UI tour's shots, 1280 px, font 14).
const PICKER: Rect = Rect {
    x: 777,
    y: 300,
    w: 338,
    h: 431,
};
const MESSAGES: Rect = Rect {
    x: 456,
    y: 96,
    w: 824,
    h: 620,
};

/// The colours of the made-up stickers: the cover (PNG), a WebP, the two frames of the APNG, a last PNG.
type Rgb = [u8; 3];
const COVER: Rgb = [230, 30, 30];
const LEAF: Rgb = [30, 200, 60];
const FRAME_ONE: Rgb = [0, 0, 255];
const FRAME_TWO: Rgb = [250, 230, 0];
const LAST: Rgb = [250, 140, 0];
/// The folder's and the Telegram pack's discs (the regressions).
const FOLDER_DISC: Rgb = [0, 200, 220];

const COVER_EMOJI: &str = "\u{1f600}";
const LEAF_EMOJI: &str = "\u{1f33f}";
const ANIMATED_EMOJI: &str = "\u{1f389}";
const LAST_EMOJI: &str = "\u{1f525}";

/// The made-up packs: the good one and the one the faults are given on.
const GOOD_TITLE: &str = "QA Signal blobs";
const FAULTS_ID: &str = "a5a5a5a5b6b6b6b6c7c7c7c7d8d8d8d8";
const FAULTS_KEY: &str = "ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100";
const FAULTS_TITLE: &str = "QA Signal faults";
/// A pack id that no CDN holds, and a key that opens no pack.
const UNKNOWN_ID: &str = "0badc0de0badc0de0badc0de0badc0de";
const WRONG_KEY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeefe";

/// The regressions' sources: a folder, and a Telegram pack.
const FOLDER: &str = "Qa sg folder";
const TELEGRAM_SET: &str = "QaSignalNeighbour";
const TELEGRAM_TITLE: &str = "QA Telegram neighbour";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_signal_stickers_window: {error}");
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

fn evidence_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os(EVIDENCE_ENV)?);
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn wanted(name: &str) -> bool {
    std::env::var(ONLY_ENV)
        .ok()
        .is_none_or(|list| list.split(',').map(str::trim).any(|one| one == name))
}

// --- the parent ---

type Check = (&'static str, fn(&mut User<'_>) -> SandboxResult);

/// The first run: both accounts with an own server's capabilities, so no Teams packs and no packs of the user's yet.
const OWN: [Check; 1] = [("empty_state_opens_panel", empty_state_opens_panel)];

/// The second run: Microsoft's cloud.
const MAIN: [Check; 11] = [
    ("menu_row", menu_row),
    ("paste_link_imports_pack", paste_link_imports_pack),
    ("emoji_labels_show", emoji_labels_show),
    ("malformed_links", malformed_links),
    ("pack_and_key_errors", pack_and_key_errors),
    ("skipped_stickers", skipped_stickers),
    ("cdn_faults_keep_nothing", cdn_faults_keep_nothing),
    ("panel_keyboard", panel_keyboard),
    ("send_static_and_animated", send_static_and_animated),
    ("themes", themes),
    ("other_imports_no_regression", other_imports_no_regression),
];

#[derive(Default)]
struct Tally {
    passed: Vec<String>,
    failed: Vec<(String, String)>,
}

/// What the runs share.
struct World<'a> {
    sandbox: &'a Sandbox,
    mock: &'a ChatServiceMock,
    cdn: &'a SignalCdnMock,
    faulty: &'a SignalCdnMock,
    bot: &'a BotApiMock,
    runtime: &'a tokio::runtime::Runtime,
    chooser: &'a Chooser,
    sources: &'a Path,
    dir: &'a Path,
    trouter: &'a str,
    dead: &'a str,
}

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_signal_stickers_window", &options)? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mock = runtime.block_on(ChatServiceMock::start())?;
    mock.post_as(LAB_B, AB_CHAT, "<p>Made-up words before the Signal stickers</p>")
        .ok_or("b's words refused")?;
    let trouter = mock.start_trouter()?;
    let packs = signal_packs()?;
    let live = SignalCdnMock::start(packs.clone())?;
    let faulty = SignalCdnMock::start(packs)?;
    let bot = BotApiMock::start(telegram_sets()?)?;
    let dead = dead_origin()?;
    let chooser = Chooser::start(sandbox.bus().address())?;
    let sources = tempfile::tempdir()?;
    make_sources(sources.path())?;
    let dir = tempfile::tempdir()?;
    // a bot token the keyring kept before this run: the Telegram regression needs no token typed
    fs::write(dir.path().join("kept-token"), SYNTHETIC_BOT_TOKEN)?;
    let world = World {
        sandbox: &sandbox,
        mock: &mock,
        cdn: &live,
        faulty: &faulty,
        bot: &bot,
        runtime: &runtime,
        chooser: &chooser,
        sources: sources.path(),
        dir: dir.path(),
        trouter: trouter.origin(),
        dead: &dead,
    };
    let mut tally = run_all(&world)?;
    key_faults(&world, &mut tally);
    drop(trouter);
    report(&tally)
}

/// A loopback origin where nothing listens.
fn dead_origin() -> SandboxResult<String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(format!("http://127.0.0.1:{port}"))
}

fn run_all(world: &World<'_>) -> SandboxResult<Tally> {
    let mut tally = Tally::default();
    let mut quits = Vec::new();
    for (run, checks) in [("own", &OWN[..]), ("main", &MAIN[..])] {
        if !checks.iter().any(|(name, _)| wanted(name)) {
            continue;
        }
        say(&format!("run {run}"));
        let app = AppUnderTest::start(world, run)?;
        let mut user = User::new(world, app)?;
        for (name, check) in checks {
            if wanted(name) {
                user.run_check(name, *check, &mut tally);
            }
        }
        if user.both_open > 0 {
            tally.failed.push((
                format!("panels_one_at_a_time ({run})"),
                format!(
                    "{} states had the Telegram and the Signal panel open together",
                    user.both_open
                ),
            ));
        } else {
            tally.passed.push(format!("panels_one_at_a_time ({run})"));
        }
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
    if let Some(dir) = evidence_dir() {
        let _ = fs::write(dir.join("results.txt"), &summary);
    }
    say(&format!(
        "qa_signal_stickers_window: {} passed, {} failed",
        tally.passed.len(),
        tally.failed.len()
    ));
    ensure(tally.failed.is_empty(), || {
        format!("{} checks failed", tally.failed.len())
    })
}

/// The pack keys the runs used, and where they must never be: the app's debug log, its other files, the CDNs'
/// requests and the chat service's requests. No CDN request has a credential.
fn key_faults(world: &World<'_>, tally: &mut Tally) {
    let check = "key_in_no_log_file_or_request".to_owned();
    let mut logs = String::new();
    let mut files = Vec::new();
    for entry in walk(world.dir) {
        let text = fs::read(&entry)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        if entry.extension().is_some_and(|extension| extension == "log") {
            logs.push_str(&text);
        }
        files.push((entry, text));
    }
    let chat: Vec<String> = world
        .runtime
        .block_on(world.mock.received_requests())
        .iter()
        .map(|request| {
            format!(
                "{} {}\n{}",
                request.method,
                request.url,
                String::from_utf8_lossy(&request.body)
            )
        })
        .collect();
    let mut cdn_requests = Vec::new();
    for (name, mock) in [("live", world.cdn), ("faulty", world.faulty)] {
        for request in mock.requests() {
            cdn_requests.push(format!("{name} {} credential={}", request.target, request.credential));
        }
    }
    if let Some(dir) = evidence_dir() {
        let _ = fs::write(dir.join("app-debug-log.txt"), &logs);
        let _ = fs::write(dir.join("signal-cdn-requests.txt"), cdn_requests.join("\n"));
    }
    let mut found = Vec::new();
    for key in [cdn::SYNTHETIC_PACK_KEY, FAULTS_KEY, WRONG_KEY] {
        if logs.contains(key) {
            found.push(format!("a pack key in the app's debug log ({}…)", &key[..8]));
        }
        for (path, text) in &files {
            if path.extension().is_none_or(|extension| extension != "log") && text.contains(key) {
                found.push(format!("a pack key in {} ({}…)", path.display(), &key[..8]));
            }
        }
        if chat.iter().any(|request| request.contains(key)) {
            found.push(format!("a pack key in a request to the chat service ({}…)", &key[..8]));
        }
        if cdn_requests.iter().any(|request| request.contains(key)) {
            found.push(format!("a pack key in a request to the Signal CDN ({}…)", &key[..8]));
        }
    }
    found.extend(cdn_request_faults(&cdn_requests));
    say(&format!(
        "check {check}: log {} bytes, {} files, {} chat requests, {} CDN requests",
        logs.len(),
        files.len(),
        chat.len(),
        cdn_requests.len()
    ));
    if !logs.contains("DEBUG") {
        found.push("the app's log has no DEBUG line: it did not log at debug".to_owned());
    }
    if found.is_empty() {
        tally.passed.push(check);
    } else {
        tally.failed.push((check, found.join("; ")));
    }
}

/// The CDN requests (`<mock> <target> credential=<bool>`) with a credential or off the pack's paths.
fn cdn_request_faults(requests: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    for request in requests {
        if request.ends_with("credential=true") {
            found.push(format!("a CDN request with a credential: {request}"));
        }
        let target = request.split(' ').nth(1).unwrap_or_default();
        let parts: Vec<&str> = target.trim_start_matches('/').split('/').collect();
        let good = match parts.as_slice() {
            ["stickers", _, "manifest.proto"] => true,
            ["stickers", _, "full", number] => number.parse::<u32>().is_ok(),
            _ => false,
        };
        if !good {
            found.push(format!("a CDN request off the pack's paths: {request}"));
        }
    }
    found
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}

// --- the made-up stickers ---

/// A still picture: a disc of `colour` on a clear 512 px square, as a sticker is.
fn disc(colour: Rgb, format: ImageFormat) -> SandboxResult<Vec<u8>> {
    let mut picture = RgbaImage::new(512, 512);
    for (x, y, pixel) in picture.enumerate_pixels_mut() {
        let (dx, dy) = (f64::from(x) - 256.0, f64::from(y) - 256.0);
        if dx.hypot(dy) < 150.0 {
            *pixel = Rgba([colour[0], colour[1], colour[2], 255]);
        }
    }
    let mut bytes = io::Cursor::new(Vec::new());
    picture.write_to(&mut bytes, format)?;
    Ok(bytes.into_inner())
}

fn signal_sticker(emoji: &str, content_type: &str, bytes: Vec<u8>) -> cdn::MockSticker {
    cdn::MockSticker {
        emoji: emoji.to_owned(),
        content_type: content_type.to_owned(),
        bytes,
    }
}

/// The four stickers of a made-up Signal pack: the cover (PNG), a WebP, an APNG of two frames, a PNG.
fn four() -> SandboxResult<Vec<cdn::MockSticker>> {
    let rgba = |colour: Rgb| [colour[0], colour[1], colour[2], 255];
    Ok(vec![
        signal_sticker(COVER_EMOJI, "image/png", disc(COVER, ImageFormat::Png)?),
        signal_sticker(LEAF_EMOJI, "image/webp", disc(LEAF, ImageFormat::WebP)?),
        signal_sticker(
            ANIMATED_EMOJI,
            "image/apng",
            cdn::animated_png(256, &[rgba(FRAME_ONE), rgba(FRAME_TWO)]),
        ),
        signal_sticker(LAST_EMOJI, "image/png", disc(LAST, ImageFormat::Png)?),
    ])
}

fn signal_packs() -> SandboxResult<Vec<MockPack>> {
    let good = MockPack::synthetic(GOOD_TITLE, four()?);
    let faults = MockPack {
        id: FAULTS_ID.to_owned(),
        key: FAULTS_KEY.to_owned(),
        title: FAULTS_TITLE.to_owned(),
        author: "Made-up author".to_owned(),
        stickers: four()?,
    };
    Ok(vec![good, faults])
}

fn telegram_sets() -> SandboxResult<Vec<MockSet>> {
    let stickers = (0..2)
        .map(|_| {
            Ok(MockSticker {
                kind: MockKind::Still,
                emoji: "\u{1f63a}".to_owned(),
                bytes: disc(FOLDER_DISC, ImageFormat::WebP)?,
            })
        })
        .collect::<SandboxResult<Vec<_>>>()?;
    Ok(vec![MockSet {
        name: TELEGRAM_SET.to_owned(),
        title: TELEGRAM_TITLE.to_owned(),
        stickers,
    }])
}

/// The folder of the regression: two still pictures.
fn make_sources(root: &Path) -> SandboxResult {
    let dir = root.join(FOLDER);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("one.png"), disc(FOLDER_DISC, ImageFormat::Png)?)?;
    fs::write(dir.join("two.webp"), disc(FOLDER_DISC, ImageFormat::WebP)?)?;
    Ok(())
}

/// A pack's `signal.art` link.
fn link(id: &str, key: &str) -> String {
    format!("https://signal.art/addstickers/#pack_id={id}&pack_key={key}")
}

fn good_link() -> String {
    link(cdn::SYNTHETIC_PACK_ID, cdn::SYNTHETIC_PACK_KEY)
}

fn faults_link() -> String {
    link(FAULTS_ID, FAULTS_KEY)
}

// --- the fake file chooser ---

const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";

/// A fake `org.freedesktop.portal.FileChooser` on the sandbox's bus: each `OpenFile` gets the next files queued (one
/// or more `file://` URIs, as the real portal gives them), or a cancel when none are.
struct Chooser {
    answers: Arc<Mutex<VecDeque<Vec<PathBuf>>>>,
    _connection: Connection,
}

impl Chooser {
    fn start(address: &str) -> SandboxResult<Self> {
        let answers = Arc::new(Mutex::new(VecDeque::new()));
        let connection = Builder::address(address)?
            .name(PORTAL_NAME)?
            .serve_at(
                PORTAL_PATH,
                FileChooser {
                    answers: Arc::clone(&answers),
                },
            )?
            .build()?;
        Ok(Self {
            answers,
            _connection: connection,
        })
    }

    fn queue(&self, files: Vec<PathBuf>) {
        let mut answers = self.answers.lock().unwrap_or_else(PoisonError::into_inner);
        answers.clear();
        answers.push_back(files);
    }
}

struct FileChooser {
    answers: Arc<Mutex<VecDeque<Vec<PathBuf>>>>,
}

#[interface(name = "org.freedesktop.portal.FileChooser")]
impl FileChooser {
    async fn open_file(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        parent_window: &str,
        title: &str,
        options: HashMap<&str, Variant<'_>>,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let _ = (parent_window, title);
        let sender = header.sender().map(ToString::to_string).unwrap_or_default();
        let token = match options.get("handle_token") {
            Some(Variant::Str(token)) => token.as_str().to_owned(),
            _ => "t".to_owned(),
        };
        let path = format!(
            "{PORTAL_PATH}/request/{}/{token}",
            sender.trim_start_matches(':').replace('.', "_")
        );
        let files = self
            .answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or_default();
        let mut results: HashMap<&str, Variant<'_>> = HashMap::new();
        let code = if files.is_empty() {
            1_u32
        } else {
            let uris: Vec<String> = files.iter().map(|file| format!("file://{}", file.display())).collect();
            results.insert("uris", Variant::from(uris));
            0
        };
        connection
            .emit_signal(
                Some(sender.as_str()),
                path.as_str(),
                REQUEST_INTERFACE,
                "Response",
                &(code, results),
            )
            .await?;
        OwnedObjectPath::try_from(path).map_err(|error| zbus::fdo::Error::Failed(error.to_string()))
    }
}

// --- the app under test ---

/// The child app in its sandbox: its process, its trace.
struct AppUnderTest {
    running: Option<Running>,
    _trace: TraceReader,
}

impl AppUnderTest {
    fn start(world: &World<'_>, run: &str) -> SandboxResult<Self> {
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let mut tokens = Vec::new();
        for mri in [LAB_A, LAB_B] {
            tokens.push(world.mock.token(mri).ok_or("the mock's token")?);
            tokens.push(world.mock.skypetoken(mri).ok_or("the mock's skypetoken")?);
        }
        let tokens = tokens.join(" ");
        let dir = world.dir.to_string_lossy().into_owned();
        let seed = world.dir.join("kept-token").to_string_lossy().into_owned();
        let child = world.sandbox.spawn_with_fds(
            &std::env::current_exe()?,
            &[],
            &[
                (CHILD_ENV, run),
                (TRACE_FD_ENV, &fd),
                (ORIGIN_ENV, &world.mock.origin()),
                (TROUTER_ENV, world.trouter),
                (TOKENS_ENV, &tokens),
                (DIR_ENV, &dir),
                (BOT_ENV, &world.bot.origin()),
                (CDN_ENV, &world.cdn.origin()),
                (FAULTY_CDN_ENV, &world.faulty.origin()),
                (DEAD_CDN_ENV, world.dead),
                (SEED_ENV, &seed),
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

// --- the user ---

/// The user at the sandbox: the pointer, the keyboard, the screenshots, and the child it asks about the window.
struct User<'a> {
    world: &'a World<'a>,
    app: AppUnderTest,
    input: VirtualInput,
    asked: u32,
    uploads: u32,
    /// The states seen with the Telegram and the Signal panel open together.
    both_open: usize,
}

impl<'a> User<'a> {
    fn new(world: &'a World<'a>, app: AppUnderTest) -> SandboxResult<Self> {
        let mut user = Self {
            world,
            app,
            input: world.sandbox.input()?,
            asked: 0,
            uploads: 0,
            both_open: 0,
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
        // the next check starts from a's a–b chat, Teams Light, no picker, an empty box, the live Signal CDN
        let _ = self.press("escape");
        let _ = self.press("escape");
        let _ = self.ask("reset");
        let _ = self.ask("cdn live");
        let _ = self.ask("switch a");
        let _ = self.ask(&format!("open {AB_CHAT}"));
        let _ = self.ask("close-import");
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
        let state: Value = serde_json::from_str(&self.ask("state")?)?;
        if panel_open(&state) && telegram_open(&state) {
            self.both_open += 1;
        }
        Ok(state)
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
        let _ = fs::write(self.world.dir.join(format!("output-{}.txt", self.asked)), &stderr);
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

    fn park(&mut self) -> SandboxResult {
        // the window's left margin: no button, tooltip or tile there
        self.input.move_to(3, SCREEN.1 / 2)
    }

    /// A screenshot as `qa652-<name>.png`, under `target/ui-shots/` and the evidence dir.
    fn shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        let file = format!("qa652-{name}.png");
        let png = self.world.sandbox.screenshot(&file, false)?;
        let kept = shots_dir()?.join(&file);
        fs::copy(&png, &kept)?;
        if let Some(dir) = evidence_dir() {
            let dir = dir.join("shots");
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

    /// The bodies of the requests the chat mock got with `method` that hold `text`.
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
}

// --- the picker and the panel, as the user reaches them ---

fn picker(state: &Value) -> &Value {
    &state["picker"]
}

fn picker_open(state: &Value) -> bool {
    !picker(state).is_null()
}

fn panel(state: &Value) -> &Value {
    &state["picker"]["signal"]
}

fn telegram(state: &Value) -> &Value {
    &state["picker"]["telegram"]
}

fn telegram_open(state: &Value) -> bool {
    !telegram(state).is_null()
}

fn panel_open(state: &Value) -> bool {
    !panel(state).is_null()
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

/// The pack named `name`, as the shelf holds it.
fn pack_named<'s>(state: &'s Value, name: &str) -> Option<&'s Value> {
    state["shelf"]["packs"]
        .as_array()?
        .iter()
        .find(|pack| pack["name"] == name)
}

fn import_line(state: &Value) -> &Value {
    &state["shelf"]["import"]
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

impl User<'_> {
    /// Ctrl+. in the box, then Ctrl+Tab: the picker on its Stickers tab.
    fn open_stickers_by_keys(&mut self) -> SandboxResult<Value> {
        // Ctrl+. toggles: a picker left open (an import's line in it) is closed first
        self.ask("close-picker")?;
        self.ask("focus-box")?;
        self.press("ctrl+.")?;
        let mut state = self.until("the picker opened by Ctrl+.", picker_open)?;
        // an own server's account may have fewer tabs: Ctrl+Tab until the Stickers tab
        for _ in 0..4 {
            if picker(&state)["tab"] == "Stickers" {
                break;
            }
            self.press("ctrl+tab")?;
            state = self.state()?;
        }
        self.until("the Stickers tab", |state| picker(state)["tab"] == "Stickers")
    }

    /// The keyboard moves to the item `selector` in the picker.
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

    fn key_pick(&mut self, selector: &str) -> SandboxResult {
        self.key_to(selector)?;
        self.press("enter")
    }

    /// The "+" menu (or the empty state's buttons), then `item`, by the keyboard.
    fn pick_import(&mut self, item: &str) -> SandboxResult {
        let state = self.open_stickers_by_keys()?;
        if !has_item(&state, item) {
            self.key_pick("sticker-pack-add")?;
        }
        self.key_pick(item)
    }

    /// The states seen while the import ran, and the state at its end.
    fn finish_import(&mut self, name: &str) -> SandboxResult<(Vec<Value>, Value)> {
        let start = Instant::now();
        let mut running = Vec::new();
        loop {
            let state = self.state()?;
            let import = import_line(&state);
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

    /// "Import a Signal pack" from the picker, by the keyboard: the panel open, its link field focused.
    fn open_panel(&mut self) -> SandboxResult<Value> {
        self.pick_import("sticker-import-signal")?;
        self.until("the Signal panel open", panel_open)
    }

    /// Retypes the field, waits for its problem to clear, then presses Enter: the problem it gives, or `None` when
    /// the panel closed.
    fn submit(&mut self, text: Option<&str>, what: &str) -> SandboxResult<Option<String>> {
        if let Some(text) = text {
            self.retype(text)?;
            self.until(&format!("{what}: the problem cleared by typing"), |state| {
                panel_open(state) && problem(state).is_none()
            })?;
        }
        let state = self.enter_in_panel(what)?;
        Ok(if panel_open(&state) { problem(&state) } else { None })
    }

    /// Selects all of the focused field and types `text` in its place (an empty `text` clears it).
    fn retype(&mut self, text: &str) -> SandboxResult {
        self.press("ctrl+a")?;
        self.press("backspace")?;
        if !text.is_empty() {
            self.type_text(text)?;
            // a loaded machine draws the last keys late
            thread::sleep(SETTLE);
        }
        Ok(())
    }

    /// The panel's problem after Enter, or the panel closed.
    fn enter_in_panel(&mut self, what: &str) -> SandboxResult<Value> {
        self.press("enter")?;
        self.until(what, |state| !panel_open(state) || !panel(state)["problem"].is_null())
    }
}

/// Whether `rows` hold a sticker of pack `id`.
fn shows_pack_stickers(state: &Value, id: &str) -> bool {
    let prefix = format!("sticker-{id}-");
    rows(state).iter().flatten().any(|item| item.starts_with(&prefix))
}

fn differ(one: &RgbImage, two: &RgbImage, area: Rect) -> usize {
    let mut count = 0;
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let (Some(a), Some(b)) = (pixel(one, x, y), pixel(two, x, y))
                && !near(a, b, 24)
            {
                count += 1;
            }
        }
    }
    count
}

// --- the checks: the panel opens ---

/// The skipped lines of the summary.
fn skipped(state: &Value) -> Vec<String> {
    import_line(state)["skipped"]
        .as_array()
        .map(|all| all.iter().filter_map(|one| one.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

/// What the import line says of a failed import.
fn failure(state: &Value) -> Option<String> {
    import_line(state)["failed"].as_str().map(str::to_owned)
}

fn problem(state: &Value) -> Option<String> {
    panel(state)["problem"].as_str().map(str::to_owned)
}

fn pack_count(state: &Value) -> usize {
    state["shelf"]["packs"].as_array().map_or(0, Vec::len)
}

/// The empty state (no packs, no Teams packs): its words name Signal sticker packs; its Signal button opens the Signal
/// panel with the focus in it, Escape goes back to the empty state; its Telegram button opens the Telegram panel and
/// not the Signal one.
fn empty_state_opens_panel(user: &mut User<'_>) -> SandboxResult {
    let mut faults = Vec::new();
    let state = user.open_stickers_by_keys()?;
    if pack_count(&state) != 0 {
        faults.push(format!("the first run has packs: {}", state["shelf"]["packs"]));
    }
    for item in [
        "sticker-import-folder",
        "sticker-import-zip",
        "sticker-import-telegram",
        "sticker-import-signal",
    ] {
        if !has_item(&state, item) {
            faults.push(format!("the empty state has no {item}: {:?}", rows(&state)));
        }
    }
    if has_item(&state, "sticker-pack-add") {
        faults.push("the empty state shows the add-a-pack menu's button, not its own buttons".to_owned());
    }
    let hint = state["picker"]["no_packs_hint"].as_str().unwrap_or_default();
    if !hint.contains("Signal sticker pack") {
        faults.push(format!(
            "the empty state's words do not name Signal sticker packs: {hint:?}"
        ));
    }
    user.image("empty-state")?;
    user.ask("theme-dark")?;
    user.image("empty-state-dark")?;
    user.ask("theme-light")?;
    user.key_pick("sticker-import-signal")?;
    let state = user.until("the Signal panel from the empty state", panel_open)?;
    if panel(&state)["focus"] != true {
        faults.push("the Signal panel opened from the empty state without the focus".to_owned());
    }
    if telegram_open(&state) {
        faults.push("the Telegram panel is open with the Signal one".to_owned());
    }
    user.image("empty-state-signal-panel")?;
    user.press("escape")?;
    let state = user.until("Escape: back to the empty state", |state| {
        !panel_open(state) && picker_open(state)
    })?;
    if !has_item(&state, "sticker-import-signal") {
        faults.push("Escape did not go back to the empty state".to_owned());
    }
    user.key_pick("sticker-import-telegram")?;
    let state = user.until("the Telegram panel from the empty state", telegram_open)?;
    if panel_open(&state) {
        faults.push("the Signal panel is open with the Telegram one".to_owned());
    }
    user.image("empty-state-telegram-panel")?;
    user.press("escape")?;
    user.until("Escape closed the Telegram panel", |state| !telegram_open(state))?;
    user.key_pick("sticker-import-signal")?;
    let state = user.until("the Signal panel again", panel_open)?;
    if telegram_open(&state) {
        faults.push("the Telegram panel stayed open under the Signal one".to_owned());
    }
    faults_to_result(&faults)
}

/// The add-a-pack menu has "Import a Signal pack" last; Enter on it opens the panel, its link field focused.
fn menu_row(user: &mut User<'_>) -> SandboxResult {
    let mut faults = Vec::new();
    let state = user.open_stickers_by_keys()?;
    if !has_item(&state, "sticker-pack-add") {
        faults.push(format!("no add-a-pack button: {:?}", rows(&state)));
    }
    user.key_pick("sticker-pack-add")?;
    let state = user.until("the menu open", |state| has_item(state, "sticker-import-signal"))?;
    let wanted = [
        "sticker-import-folder",
        "sticker-import-zip",
        "sticker-import-files",
        "sticker-import-telegram",
        "sticker-import-signal",
    ];
    let order: Vec<String> = rows(&state)
        .into_iter()
        .flatten()
        .filter(|item| wanted.contains(&item.as_str()))
        .collect();
    if order != wanted {
        faults.push(format!("the menu's rows: {order:?}, {wanted:?} expected"));
    }
    user.image("menu-rows")?;
    user.key_pick("sticker-import-signal")?;
    let state = user.until("the Signal panel open", panel_open)?;
    if panel(&state)["focus"] != true {
        faults.push("the panel opened without the focus".to_owned());
    }
    if telegram_open(&state) {
        faults.push("the Telegram panel is open too".to_owned());
    }
    user.image("panel-light")?;
    faults_to_result(&faults)
}

// --- the checks: a good import ---

/// The stickers of `area` found by their colours: the cover, the WebP, the APNG's first or second frame, the last.
fn found_colours(image: &RgbImage, area: Rect) -> Vec<(&'static str, usize)> {
    [
        ("cover", COVER),
        ("webp", LEAF),
        ("apng-1", FRAME_ONE),
        ("apng-2", FRAME_TWO),
        ("last", LAST),
    ]
    .into_iter()
    .map(|(name, colour)| (name, blobs(image, colour, area, 60).len()))
    .collect()
}

fn seen(found: &[(&str, usize)], name: &str) -> bool {
    found.iter().any(|(one, count)| *one == name && *count > 0)
}

/// The good pack's link pasted (Ctrl+V) in the panel, Enter: the progress, then the summary with nothing skipped; the
/// pack named by its title with its four stickers, the animated one a GIF; its cover on the Stickers tab; each sticker
/// in the opened pack. The CDN is asked for the manifest and each sticker once.
fn paste_link_imports_pack(user: &mut User<'_>) -> SandboxResult {
    let before = user.world.cdn.requests().len();
    user.open_panel()?;
    user.ask(&format!("clipboard {}", good_link()))?;
    user.press("ctrl+v")?;
    thread::sleep(SETTLE);
    user.image("pasted-link")?;
    let state = user.enter_in_panel("Enter after the pasted link")?;
    if panel_open(&state) {
        return Err(format!("the pasted link was not taken: {:?}", problem(&state)).into());
    }
    let (running, state) = user.finish_import("good-import")?;
    user.image("good-import-summary")?;
    let mut faults = Vec::new();
    say(&format!(
        "  progress seen {} times: {:?}",
        running.len(),
        running.first()
    ));
    if let Some(failed) = failure(&state) {
        faults.push(format!("the import failed: {failed}"));
    }
    let lines = skipped(&state);
    if !lines.is_empty() {
        faults.push(format!("the good pack skipped {lines:?}"));
    }
    let pack = pack_named(&state, GOOD_TITLE).cloned();
    let Some(pack) = pack else {
        faults.push(format!("no pack {GOOD_TITLE:?}: {}", state["shelf"]["packs"]));
        return faults_to_result(&faults);
    };
    faults.extend(good_pack_faults(&pack));
    let requests: Vec<String> = user.world.cdn.requests()[before..]
        .iter()
        .map(|request| request.target.clone())
        .collect();
    let manifest = format!("/stickers/{}/manifest.proto", cdn::SYNTHETIC_PACK_ID);
    let wanted_requests: Vec<String> = std::iter::once(manifest)
        .chain((0..4).map(|n| format!("/stickers/{}/full/{n}", cdn::SYNTHETIC_PACK_ID)))
        .collect();
    if requests != wanted_requests {
        faults.push(format!("the CDN was asked {requests:?}"));
    }
    let id = pack["id"].as_str().unwrap_or_default().to_owned();
    faults.extend(good_pack_shows(user, &id)?);
    faults_to_result(&faults)
}

/// The good pack's stickers: the four emoji as labels in order, the animated one a GIF and the others PNGs.
fn good_pack_faults(pack: &Value) -> Vec<String> {
    let mut faults = Vec::new();
    let stickers = pack["stickers"].as_array().cloned().unwrap_or_default();
    let summary: Vec<(String, bool, String)> = stickers
        .iter()
        .map(|sticker| {
            (
                sticker["label"].as_str().unwrap_or_default().to_owned(),
                sticker["animated"] == true,
                sticker["file"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    say(&format!("  the pack's stickers: {summary:?}"));
    let wanted_labels = [COVER_EMOJI, LEAF_EMOJI, ANIMATED_EMOJI, LAST_EMOJI];
    let labels: Vec<&str> = summary.iter().map(|(label, _, _)| label.as_str()).collect();
    if labels != wanted_labels {
        faults.push(format!("the labels {labels:?}, {wanted_labels:?} expected"));
    }
    for (label, animated, file) in &summary {
        let wanted = if *animated { ".gif" } else { ".png" };
        if !file.ends_with(wanted) {
            faults.push(format!("{label}: {file} is not a {wanted}"));
        }
        if *animated != (label == ANIMATED_EMOJI) {
            faults.push(format!("{label}: animated is {animated}"));
        }
    }
    faults
}

/// The summary closed: the pack `id` on the Stickers tab with its cover; opened, each of its stickers shows.
fn good_pack_shows(user: &mut User<'_>, id: &str) -> SandboxResult<Vec<String>> {
    let mut faults = Vec::new();
    user.key_pick("sticker-import-close")?;
    let state = user.until("the summary closed", |state| import_line(state).is_null())?;
    if !has_item(&state, &format!("sticker-pack-{id}")) {
        faults.push(format!("the Stickers tab does not show the pack: {:?}", rows(&state)));
    }
    let image = user.image("good-import-tab")?;
    let found = found_colours(&image, PICKER);
    say(&format!("  the Stickers tab's colours: {found:?}"));
    if !seen(&found, "cover") {
        faults.push("the pack's cover does not show on the Stickers tab".to_owned());
    }
    user.key_pick(&format!("sticker-pack-{id}"))?;
    let state = user.until("the pack open", |state| shows_pack_stickers(state, id))?;
    let shown = rows(&state)
        .iter()
        .flatten()
        .filter(|item| item.starts_with(&format!("sticker-{id}-")))
        .count();
    if shown != 4 {
        faults.push(format!("the open pack shows {shown} stickers, 4 expected"));
    }
    let image = user.image("good-pack-open")?;
    let found = found_colours(&image, PICKER);
    say(&format!("  the open pack's colours: {found:?}"));
    for name in ["cover", "webp", "last"] {
        if !seen(&found, name) {
            faults.push(format!("the {name} sticker does not show in the open pack"));
        }
    }
    if !seen(&found, "apng-1") && !seen(&found, "apng-2") {
        faults.push("the animated sticker does not show in the open pack".to_owned());
    }
    let (one, two) = frames_seen(user, PICKER, "good-pack-plays")?;
    say(&format!(
        "  the animated sticker in the picker: frame one {one} shots, frame two {two} shots"
    ));
    Ok(faults)
}

/// Shots of `area` over a few seconds: in how many the APNG's first frame shows, and in how many its second.
fn frames_seen(user: &mut User<'_>, area: Rect, name: &str) -> SandboxResult<(usize, usize)> {
    user.park()?;
    let (mut one, mut two) = (0, 0);
    let start = Instant::now();
    let mut shot = 0;
    while (one == 0 || two == 0 || shot < 6) && start.elapsed() <= Duration::from_secs(10) {
        let path = user.shot(&format!("{name}-{}", shot % 6))?;
        let image = image::open(path)?.to_rgb8();
        one += usize::from(!blobs(&image, FRAME_ONE, area, 60).is_empty());
        two += usize::from(!blobs(&image, FRAME_TWO, area, 60).is_empty());
        shot += 1;
        thread::sleep(Duration::from_millis(45));
    }
    Ok((one, two))
}

/// The pointer on each sticker of the open good pack shows its emoji as the tooltip.
fn emoji_labels_show(user: &mut User<'_>) -> SandboxResult {
    let state = user.open_stickers_by_keys()?;
    let id = pack_named(&state, GOOD_TITLE)
        .and_then(|pack| pack["id"].as_str())
        .ok_or("no good pack")?
        .to_owned();
    user.key_pick(&format!("sticker-pack-{id}"))?;
    user.until("the pack open", |state| shows_pack_stickers(state, &id))?;
    let before = user.image("labels-before")?;
    let mut faults = Vec::new();
    for (name, colour) in [("cover", COVER), ("webp", LEAF), ("last", LAST)] {
        let Some(blob) = blobs(&before, colour, PICKER, 60).into_iter().next() else {
            faults.push(format!("no {name} sticker to point at"));
            continue;
        };
        let centre = (blob.x + blob.w / 2, blob.y + blob.h / 2);
        user.input.move_to(u32::try_from(centre.0)?, u32::try_from(centre.1)?)?;
        thread::sleep(Duration::from_millis(1500));
        let shot = user
            .world
            .sandbox
            .screenshot(&format!("qa652-label-{name}.png"), false)?;
        let kept = shots_dir()?.join(format!("qa652-label-{name}.png"));
        fs::copy(&shot, &kept)?;
        if let Some(dir) = evidence_dir() {
            let _ = fs::copy(&shot, dir.join("shots").join(format!("qa652-label-{name}.png")));
        }
        let after = image::open(&kept)?.to_rgb8();
        let around = Rect {
            x: (centre.0 - 160).max(0),
            y: (centre.1 - 90).max(0),
            w: 320,
            h: 180,
        };
        let changed = differ(&before, &after, around);
        say(&format!("  {name}: {changed} px changed near the pointer"));
        if changed < 150 {
            faults.push(format!("no tooltip shows on the {name} sticker ({changed} px changed)"));
        }
    }
    user.park()?;
    faults_to_result(&faults)
}

// --- the checks: errors ---

/// Links that name no pack: the panel says the bad-link line, keeps the focus in the field, and nothing goes to the
/// CDN. Typing clears the line.
fn malformed_links(user: &mut User<'_>) -> SandboxResult {
    let before = user.world.cdn.requests().len();
    let packs_before = pack_count(&user.state()?);
    user.open_panel()?;
    let mut faults = Vec::new();
    let bad = [
        String::new(),
        "signal.art/addstickers/".to_owned(),
        "https://signal.art/addstickers/#pack_id=made-up".to_owned(),
        format!("https://signal.art/addstickers/#pack_id={}", cdn::SYNTHETIC_PACK_ID),
        format!(
            "https://example.test/addstickers/#pack_id={}&pack_key={}",
            cdn::SYNTHETIC_PACK_ID,
            cdn::SYNTHETIC_PACK_KEY
        ),
        link(cdn::SYNTHETIC_PACK_ID, &cdn::SYNTHETIC_PACK_KEY[..60]),
        link("5f17e7105f17e7105f17e7105f17e71z", cdn::SYNTHETIC_PACK_KEY),
        "t.me/addstickers/QaSignalNeighbour".to_owned(),
    ];
    for (index, text) in bad.iter().enumerate() {
        let said = user.submit(Some(text), &format!("the link {text:?}"))?;
        if said.as_deref() != Some(signal_import::BAD_LINK) {
            faults.push(format!("the link {text:?} gives {said:?}, not the bad-link line"));
        }
        let state = user.state()?;
        if panel(&state)["focus"] != true {
            faults.push(format!("after the link {text:?} the focus is not in the panel"));
        }
        if index == 3 {
            user.image("bad-link-light")?;
            user.ask("theme-dark")?;
            user.image("bad-link-dark")?;
            user.ask("theme-light")?;
        }
    }
    user.type_text("x")?;
    if user
        .until("typing clears the bad-link line", |state| problem(state).is_none())
        .is_err()
    {
        faults.push("typing after the bad-link line does not clear it".to_owned());
    }
    let sent = user.world.cdn.requests().len() - before;
    if sent != 0 {
        faults.push(format!("{sent} requests went to the CDN for links that name no pack"));
    }
    if pack_count(&user.state()?) != packs_before {
        faults.push("a malformed link made a pack".to_owned());
    }
    faults_to_result(&faults)
}

/// One import from the panel with `text` typed: the state at its end.
fn import_typed(user: &mut User<'_>, name: &str, text: &str) -> SandboxResult<Value> {
    user.open_panel()?;
    if let Some(said) = user.submit(Some(text), name)? {
        return Err(format!("{name}: the panel says {said:?}").into());
    }
    let (_, state) = user.finish_import(name)?;
    user.image(&format!("{name}-line"))?;
    say(&format!(
        "  {name}: failed {:?}, skipped {:?}",
        failure(&state),
        skipped(&state)
    ));
    Ok(state)
}

const NO_SUCH_PACK: &str = "Signal has no sticker pack at that link.";
const BAD_KEY: &str = "That pack can't be opened with the key in its link. Check that you copied the whole link.";
const DOWN: &str = "Signal can't be reached. Check your connection and try again.";
const UNREADABLE: &str = "Signal's answer can't be read. Try again later.";

/// An unknown pack (403, its link in the `sgnl://` form), a wrong key and a tampered manifest: each says the user's
/// words and keeps nothing; the wrong key asks the CDN only for the manifest.
fn pack_and_key_errors(user: &mut User<'_>) -> SandboxResult {
    let mut faults = Vec::new();
    let packs_before = pack_count(&user.state()?);
    let unknown = format!(
        "sgnl://addstickers/?pack_id={UNKNOWN_ID}&pack_key={}",
        cdn::SYNTHETIC_PACK_KEY
    );
    let cases = [
        ("unknown-pack", unknown, NO_SUCH_PACK, "live"),
        ("wrong-key", link(cdn::SYNTHETIC_PACK_ID, WRONG_KEY), BAD_KEY, "live"),
        ("tampered-manifest", faults_link(), BAD_KEY, "faulty"),
    ];
    for (name, text, words, origin) in cases {
        if origin == "faulty" {
            user.world.faulty.fault(MockFault::TamperedManifest);
        }
        user.ask(&format!("cdn {origin}"))?;
        let mock = if origin == "faulty" {
            user.world.faulty
        } else {
            user.world.cdn
        };
        let before = mock.requests().len();
        let state = import_typed(user, name, &text)?;
        if failure(&state).as_deref() != Some(words) {
            faults.push(format!("{name}: the line says {:?}, not {words:?}", failure(&state)));
        }
        let asked: Vec<String> = mock.requests()[before..].iter().map(|one| one.target.clone()).collect();
        if asked.len() != 1 || !asked[0].ends_with("/manifest.proto") {
            faults.push(format!("{name}: the CDN was asked {asked:?}"));
        }
        if name == "unknown-pack" {
            user.ask("theme-dark")?;
            user.image("unknown-pack-line-dark")?;
            user.ask("theme-light")?;
        }
        user.ask("close-import")?;
    }
    if pack_count(&user.state()?) != packs_before {
        faults.push("a failed import kept a pack".to_owned());
    }
    faults_to_result(&faults)
}

/// A tampered sticker and a missing sticker: each is skipped with its reason, and the pack keeps the other three.
fn skipped_stickers(user: &mut User<'_>) -> SandboxResult {
    let mut faults = Vec::new();
    user.ask("cdn faulty")?;
    let cases = [
        (
            "tampered-sticker",
            MockFault::TamperedSticker(1),
            format!("Sticker 2 {LEAF_EMOJI} failed Signal's integrity check"),
            LEAF_EMOJI,
        ),
        (
            "missing-sticker",
            MockFault::MissingSticker(2),
            format!("Sticker 3 {ANIMATED_EMOJI} couldn't be downloaded"),
            ANIMATED_EMOJI,
        ),
    ];
    for (name, fault, line, gone) in cases {
        user.world.faulty.fault(fault);
        let packs_before = pack_count(&user.state()?);
        let state = import_typed(user, name, &faults_link())?;
        if let Some(failed) = failure(&state) {
            faults.push(format!("{name}: the import failed: {failed}"));
        }
        let lines = skipped(&state);
        if lines != [line.clone()] {
            faults.push(format!("{name}: skipped {lines:?}, [{line:?}] expected"));
        }
        if pack_count(&state) != packs_before + 1 {
            faults.push(format!("{name}: no new pack"));
        }
        let made = import_line(&state)["done"].clone();
        let labels: Vec<String> = state["shelf"]["packs"]
            .as_array()
            .and_then(|packs| packs.iter().find(|pack| pack["id"] == made))
            .and_then(|pack| pack["stickers"].as_array())
            .map(|stickers| {
                stickers
                    .iter()
                    .filter_map(|sticker| sticker["label"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if labels.len() != 3 || labels.iter().any(|label| label == gone) {
            faults.push(format!("{name}: the pack's stickers {labels:?}"));
        }
        if name == "tampered-sticker" {
            user.ask("theme-dark")?;
            user.image("tampered-sticker-line-dark")?;
            user.ask("theme-light")?;
        }
        user.ask("close-import")?;
    }
    faults_to_result(&faults)
}

/// A CDN that redirects, breaks (500), stops in the middle (503 or a dropped connection) or cannot be reached: the
/// import says so and keeps nothing; the redirect's target is never asked.
fn cdn_faults_keep_nothing(user: &mut User<'_>) -> SandboxResult {
    let mut faults = Vec::new();
    let packs_before = pack_count(&user.state()?);
    let stickers_dir = user.world.dir.join("stickers");
    let dirs_before = fs::read_dir(&stickers_dir).map_or(0, Iterator::count);
    let target = format!("{}/stickers/{}/manifest.proto", user.world.cdn.origin(), FAULTS_ID);
    let cases = [
        ("redirect", Some(MockFault::RedirectTo(target)), UNREADABLE),
        ("broken", Some(MockFault::Broken), UNREADABLE),
        ("broken-midway", Some(MockFault::BrokenFrom(1)), UNREADABLE),
        ("dropped-midway", Some(MockFault::DroppedFrom(2)), DOWN),
        ("unreachable", None, DOWN),
    ];
    for (name, fault, words) in cases {
        let live_before = user.world.cdn.requests().len();
        match fault {
            Some(fault) => {
                user.world.faulty.fault(fault);
                user.ask("cdn faulty")?;
            }
            None => {
                user.ask("cdn dead")?;
            }
        }
        let state = import_typed(user, name, &faults_link())?;
        if failure(&state).as_deref() != Some(words) {
            faults.push(format!("{name}: the line says {:?}, not {words:?}", failure(&state)));
        }
        if name == "redirect" && user.world.cdn.requests().len() != live_before {
            faults.push("the redirect was followed".to_owned());
        }
        if name == "broken-midway" {
            user.ask("theme-dark")?;
            user.image("broken-midway-line-dark")?;
            user.ask("theme-light")?;
        }
        user.ask("close-import")?;
    }
    if pack_count(&user.state()?) != packs_before {
        faults.push("a CDN fault kept a pack".to_owned());
    }
    let dirs_after = fs::read_dir(&stickers_dir).map_or(0, Iterator::count);
    if dirs_after != dirs_before {
        faults.push(format!(
            "the stickers dir has {dirs_after} entries after the faults, {dirs_before} before"
        ));
    }
    faults_to_result(&faults)
}

// --- the checks: the keyboard ---

/// The panel by the keyboard: from the link field, Tab goes to Import (Enter with no link says the bad-link line and
/// takes the focus back to the field), Cancel, the back arrow and round to the field; Shift+Tab the other way. Enter
/// or Space on Cancel and the back arrow goes back to the picker. Escape goes back to the picker, a second closes it.
fn panel_keyboard(user: &mut User<'_>) -> SandboxResult {
    let mut faults = Vec::new();
    let back_to_picker = |state: &Value| !panel_open(state) && picker_open(state) && picker(state)["tab"] == "Stickers";
    user.open_panel()?;
    for step in 1..=4 {
        user.press("tab")?;
        user.image(&format!("keys-tab-{step}"))?;
    }
    // round to the field: a link typed now goes into it
    user.type_text(&format!(
        "sgnl://addstickers/?pack_id={UNKNOWN_ID}&pack_key={}",
        cdn::SYNTHETIC_PACK_KEY
    ))?;
    thread::sleep(SETTLE);
    let state = user.enter_in_panel("Tab four times, a link, Enter")?;
    if panel_open(&state) {
        faults.push(format!(
            "Tab four times did not come round to the field: {:?}",
            problem(&state)
        ));
    } else {
        let (_, state) = user.finish_import("keys-tab-round")?;
        if failure(&state).as_deref() != Some(NO_SUCH_PACK) {
            faults.push(format!("Tab round: {}", import_line(&state)));
        }
        user.ask("close-import")?;
    }
    user.open_panel()?;
    for step in 1..=4 {
        user.press("shift+tab")?;
        user.image(&format!("keys-shift-tab-{step}"))?;
    }
    user.type_text("not a link")?;
    thread::sleep(SETTLE);
    let state = user.enter_in_panel("Shift+Tab four times, text, Enter")?;
    if problem(&state).as_deref() != Some(signal_import::BAD_LINK) {
        faults.push(format!(
            "Shift+Tab four times did not come round to the field: {}",
            panel(&state)
        ));
    }
    // Import with an empty field: the bad-link line, and the focus back in the field
    user.open_panel()?;
    user.press("tab")?;
    user.press("enter")?;
    let state = user.until("Enter on Import with no link", |state| problem(state).is_some())?;
    if problem(&state).as_deref() != Some(signal_import::BAD_LINK) {
        faults.push(format!("Import with no link: {}", panel(&state)));
    }
    user.image("keys-import-empty")?;
    user.type_text("x")?;
    if user
        .until("the focus back in the field", |state| problem(state).is_none())
        .is_err()
    {
        faults.push("after Import with no link, typing does not reach the field".to_owned());
    }
    for (keys_to, press, name) in [
        (&["tab", "tab"][..], "space", "Tab twice, Space: Cancel"),
        (&["tab", "tab"][..], "enter", "Tab twice, Enter: Cancel"),
        (&["shift+tab"][..], "enter", "Shift+Tab, Enter: the back arrow"),
        (&["shift+tab"][..], "space", "Shift+Tab, Space: the back arrow"),
    ] {
        user.open_panel()?;
        for key in keys_to {
            user.press(key)?;
        }
        user.press(press)?;
        if let Err(error) = user.until(name, back_to_picker) {
            faults.push(error.to_string());
        }
    }
    user.open_panel()?;
    user.press("escape")?;
    if let Err(error) = user.until("Escape: back to the picker", back_to_picker) {
        faults.push(error.to_string());
    }
    user.press("escape")?;
    if let Err(error) = user.until("a second Escape closes the picker", |state| !picker_open(state)) {
        faults.push(error.to_string());
    }
    faults_to_result(&faults)
}

// --- the checks: send, regressions, restart ---

impl User<'_> {
    /// The media service answers the next picture send as a new object of its own, which then serves the bytes of the
    /// kept sticker `file` of pack `id`, as the service gives a sent picture back to b.
    fn serve_upload(&mut self, id: &str, file: &str) -> SandboxResult {
        let bytes = fs::read(self.world.dir.join("stickers").join(id).join(file))?;
        let gif = Path::new(file)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gif"));
        let kind = if gif { "image/gif" } else { "image/png" };
        self.uploads += 1;
        let object = format!("0-eus-d1-{:032x}", 0x0065_1000 + self.uploads);
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
}

fn items(state: &Value) -> Vec<Value> {
    state["chat"]["items"].as_array().cloned().unwrap_or_default()
}

/// a sends one sticker of the good pack, `label`, by the keyboard and Enter; b sees it bare. The state of b's chat.
fn send_one(user: &mut User<'_>, label: &str, faults: &mut Vec<String>) -> SandboxResult<Value> {
    user.switch("a")?;
    user.open(AB_CHAT)?;
    let state = user.open_stickers_by_keys()?;
    let pack = pack_named(&state, GOOD_TITLE).ok_or("no Signal pack")?.clone();
    let id = pack["id"].as_str().unwrap_or_default().to_owned();
    let file = pack["stickers"]
        .as_array()
        .and_then(|stickers| stickers.iter().find(|sticker| sticker["label"] == label))
        .and_then(|sticker| sticker["file"].as_str())
        .ok_or_else(|| format!("no sticker {label} in the Signal pack"))?
        .to_owned();
    user.key_pick(&format!("sticker-pack-{id}"))?;
    user.until("the pack open", |state| shows_pack_stickers(state, &id))?;
    user.serve_upload(&id, &file)?;
    let number = file.split('.').next().unwrap_or_default();
    let before = user.sent_with("POST", "schema.skype.com/Sticker").len();
    user.key_pick(&format!("sticker-{id}-{number}"))?;
    let state = user.until("the sticker in the box, sent to the media service", |state| {
        let pictures = pictures(state);
        !picker_open(state) && pictures.len() == 1 && pictures[0]["state"] == "sent"
    })?;
    if pictures(&state)[0]["sticker"] != label {
        faults.push(format!("the sticker in the box: {}", pictures(&state)[0]));
    }
    user.image(&format!("send-in-box-{number}"))?;
    user.ask("focus-box")?;
    user.press("enter")?;
    wait_until(WAIT, || {
        let all = user.sent_with("POST", "schema.skype.com/Sticker");
        (all.len() > before).then(|| all.last().cloned().unwrap_or_default())
    })
    .ok_or_else(|| format!("no sticker message {label} went to the chat service"))?;
    user.switch("b")?;
    user.open(AB_CHAT)?;
    let state = user.until(&format!("a's Signal sticker {label} in b's chat"), |state| {
        items(state)
            .iter()
            .any(|item| item["label"] == label && item["own"] == false)
    })?;
    let item = items(&state)
        .into_iter()
        .rfind(|item| item["label"] == label)
        .unwrap_or_default();
    if item["bare"] != true || item["sticker_alone"] != true {
        faults.push(format!("b's message {label} is not a bare sticker: {item}"));
    }
    Ok(state)
}

/// a sends the static cover and the animated sticker; b sees each, the cover by its colour, the animated one play.
fn send_static_and_animated(user: &mut User<'_>) -> SandboxResult {
    let mut faults = Vec::new();
    send_one(user, COVER_EMOJI, &mut faults)?;
    thread::sleep(SETTLE);
    let image = user.image("b-sees-static")?;
    if blobs(&image, COVER, MESSAGES, 60).is_empty() {
        faults.push("b does not see the static Signal sticker".to_owned());
    }
    send_one(user, ANIMATED_EMOJI, &mut faults)?;
    thread::sleep(SETTLE);
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        let (one, two) = frames_seen(user, MESSAGES, &format!("b-sees-animated-{theme}"))?;
        say(&format!(
            "  b, {theme}: frame one in {one} shots, frame two in {two} shots"
        ));
        if one == 0 || two == 0 {
            faults.push(format!(
                "the animated Signal sticker does not play for b in Teams {theme}"
            ));
        }
    }
    user.ask("theme-light")?;
    faults_to_result(&faults)
}

/// Teams Light and Dark shots: the empty panel, the panel with a link typed, the bad-link line, the open pack.
fn themes(user: &mut User<'_>) -> SandboxResult {
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        user.open_panel()?;
        user.image(&format!("theme-{theme}-panel"))?;
        user.retype(&good_link())?;
        user.image(&format!("theme-{theme}-panel-link"))?;
        user.retype("signal.art/addstickers/#pack_id=made-up")?;
        user.enter_in_panel("the bad-link line")?;
        user.image(&format!("theme-{theme}-panel-bad-link"))?;
        user.press("escape")?;
        let state = user.until("back to the picker", |state| !panel_open(state))?;
        let id = pack_named(&state, GOOD_TITLE)
            .and_then(|pack| pack["id"].as_str())
            .ok_or("no good pack")?
            .to_owned();
        user.key_pick(&format!("sticker-pack-{id}"))?;
        user.until("the pack open", |state| shows_pack_stickers(state, &id))?;
        user.image(&format!("theme-{theme}-pack-open"))?;
        user.press("escape")?;
    }
    user.ask("theme-light")?;
    Ok(())
}

/// The other imports still work: a folder, a Telegram link (with the kept bot token), and Teams' own packs open.
fn other_imports_no_regression(user: &mut User<'_>) -> SandboxResult {
    let mut faults = Vec::new();
    user.world.chooser.queue(vec![user.world.sources.join(FOLDER)]);
    user.pick_import("sticker-import-folder")?;
    let (_, state) = user.finish_import("folder-import")?;
    user.image("folder-import-summary")?;
    match pack_named(&state, FOLDER) {
        Some(pack) if pack["stickers"].as_array().map_or(0, Vec::len) == 2 => {}
        other => faults.push(format!("the folder's pack: {other:?}; failed {:?}", failure(&state))),
    }
    user.ask("close-import")?;
    user.pick_import("sticker-import-telegram")?;
    let state = user.until("the Telegram panel, its token looked up", |state| {
        telegram_open(state) && telegram(state)["token_state"] != "Checking"
    })?;
    if telegram(&state)["token_state"] != "Saved" {
        faults.push(format!("the kept bot token is not seen: {}", telegram(&state)));
    }
    if panel_open(&state) {
        faults.push("the Signal panel is open with the Telegram one".to_owned());
    }
    user.type_text(&format!("t.me/addstickers/{TELEGRAM_SET}"))?;
    thread::sleep(SETTLE);
    user.press("enter")?;
    user.until("the Telegram panel closed", |state| !telegram_open(state))?;
    let (_, state) = user.finish_import("telegram-import")?;
    user.image("telegram-import-summary")?;
    match pack_named(&state, TELEGRAM_TITLE) {
        Some(pack) if pack["stickers"].as_array().map_or(0, Vec::len) == 2 => {}
        other => faults.push(format!("the Telegram pack: {other:?}; failed {:?}", failure(&state))),
    }
    user.ask("close-import")?;
    // the Home view shows the first packs only, the user's first; Teams' packs are under "See all"
    user.open_stickers_by_keys()?;
    user.key_pick("sticker-packs-see-all")?;
    let state = user.until("all the packs", |state| {
        rows(state).iter().flatten().any(|item| item.starts_with("teams-pack-"))
    })?;
    user.image("all-packs")?;
    let teams_pack = rows(&state)
        .into_iter()
        .flatten()
        .find(|item| item.starts_with("teams-pack-"));
    match teams_pack {
        Some(item) => {
            user.key_pick(&item)?;
            let state = user.until("a Teams pack open", |state| {
                rows(state)
                    .iter()
                    .flatten()
                    .any(|item| item.starts_with("sticker-teams-"))
            })?;
            let _ = state;
            user.image("teams-pack-open")?;
        }
        None => faults.push(format!("no Teams pack on the Stickers tab: {:?}", rows(&state))),
    }
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

/// The engine of `alias` (a or b) on the mock: its chats, its media service and its Trouter; no calls. With
/// `own_server`, an own server's capabilities (no Teams sticker packs).
fn engine(alias: Alias, tokens: (String, String), dir: &Path, own_server: bool) -> SandboxResult<EngineHandle> {
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
    let transport = Arc::new(transport);
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

/// The app's secret store, in memory, with the bot token the last run kept (as a keyring keeps it).
fn secrets(seed: &Path) -> SandboxResult<AppSecrets> {
    let secrets = AppSecrets::in_memory();
    if let Ok(token) = fs::read_to_string(seed) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let name = SecretKey::new(ngteams::ai_keys::TELEGRAM_ACCOUNT, TOKEN_NAME);
        runtime.block_on(secrets.put(&name, &Secret::new(token)))?;
    }
    Ok(secrets)
}

/// The Bot API the app reaches: the mock's.
fn bot_apis(origin: String) -> BotApis {
    BotApis::new(Arc::new(move |token| BotApi::with_mock_origin(token, &origin)))
}

/// The Signal CDN the app reaches: the live mock's, the faulty mock's, or a loopback port where nothing listens.
fn signal_cdns(origin: Arc<Mutex<String>>) -> SignalCdns {
    SignalCdns::new(Arc::new(move || {
        let origin = origin.lock().unwrap_or_else(PoisonError::into_inner).clone();
        SignalCdn::with_mock_origin(&origin)
    }))
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let run = env(CHILD_ENV)?;
    let diagnostics = app::install_log(dir.join(format!("logs-{run}")), teams_ui::settings::LogLevel::Debug);
    let tokens: Vec<String> = env(TOKENS_ENV)?.split(' ').map(str::to_owned).collect();
    let [ic3_a, skype_a, ic3_b, skype_b] = <[String; 4]>::try_from(tokens).map_err(|_| "four tokens expected")?;
    let own_server = run == "own";
    let first = support::started(Alias::A, engine(Alias::A, (ic3_a, skype_a), &dir, own_server)?)?;
    let second = support::started(Alias::B, engine(Alias::B, (ic3_b, skype_b), &dir, own_server)?)?;
    let seed = PathBuf::from(env(SEED_ENV)?);
    let paths = PersonalPaths::new(dir.join("ngteams-config"), dir.join("ngteams-data"));
    let runner = Runner {
        setup: Setup::new(paths)?.with_secrets(secrets(&seed)?),
        floor: CallFloor::new(),
        devices: None,
    };
    let settings = teams_ui::SettingsFile::new(dir.join("settings.toml"));
    let mut opened = settings.load().settings;
    opened.selected_chat = Some(AB_CHAT.to_owned());
    // the mock's made-up week starts a meeting at a fixed hour; its reminder card would cover the picker
    opened.meeting_reminders = false;
    settings.save(&opened).map_err(|problem| format!("{problem:?}"))?;
    let origins = Origins {
        live: env(CDN_ENV)?,
        faulty: env(FAULTY_CDN_ENV)?,
        dead: env(DEAD_CDN_ENV)?,
        now: Arc::new(Mutex::new(env(CDN_ENV)?)),
    };
    let bot = env(BOT_ENV)?;
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        cx.set_global(bot_apis(bot));
        cx.set_global(signal_cdns(Arc::clone(&origins.now)));
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: vec![second],
                trace: Arc::new(trace),
                ctl: None,
                settings: teams_ui::SettingsModel::load(settings).with_diagnostics(diagnostics),
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
                answer(window, launched.desktop, origins, cx);
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

/// The Signal CDN's origins, and the one in use.
struct Origins {
    live: String,
    faulty: String,
    dead: String,
    now: Arc<Mutex<String>>,
}

/// Answers the parent's questions as they come, until it asks to quit.
fn answer(window: WindowHandle<KitRoot>, desktop: Entity<Desktop>, origins: Origins, cx: &mut App) {
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
            let reply = match verb {
                "cdn live" | "cdn faulty" | "cdn dead" => {
                    let origin = match verb {
                        "cdn faulty" => &origins.faulty,
                        "cdn dead" => &origins.dead,
                        _ => &origins.live,
                    };
                    origin.clone_into(&mut origins.now.lock().unwrap_or_else(PoisonError::into_inner));
                    "ok".to_owned()
                }
                _ if verb.starts_with("clipboard ") => {
                    let text = verb.trim_start_matches("clipboard ").to_owned();
                    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(text)));
                    "ok".to_owned()
                }
                _ => cx.update(|cx| on_app(window, &desktop, verb, cx)),
            };
            say(&format!("re {seq} {}", reply.replace('\n', " ")));
        }
    })
    .detach();
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
        "close-import" => {
            let dir = shell.read(cx).settings().read(cx).stickers_dir();
            let held = Shelf::of(dir, cx);
            if !held.read(cx).importing() {
                held.update(cx, Shelf::dismiss_import);
            }
        }
        "close-picker" => {
            if let Some(compose) = shown_compose(shell.read(cx), cx)
                && compose.read(cx).emoji_picker().is_some()
            {
                compose.update(cx, |compose, cx| compose.toggle_emoji(window, cx));
            }
        }
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
        other => {
            let Some(id) = other.strip_prefix("open ") else {
                return format!("unknown {other}");
            };
            shell.update(cx, |shell, cx| shell.open_chat(id.to_owned(), window, cx));
        }
    }
    "ok".to_owned()
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
    json!({
        "tab": format!("{:?}", picker.tab()),
        "view": format!("{:?}", picker.sticker_view()),
        "place": place,
        "current": picker.current(window, cx).map(|item| item.selector()),
        "rows": rows,
        "focus": picker.holds_focus(window, cx),
        "no_packs_hint": picker.no_packs_hint(cx),
        "telegram": picker.telegram_import().map(|panel| telegram_state(panel, cx)),
        "signal": picker.signal_import().map(|panel| signal_state(panel, window, cx)),
    })
}

fn signal_state(panel: &Entity<SignalImport>, window: &Window, cx: &App) -> Value {
    let panel = panel.read(cx);
    json!({
        "problem": panel.problem().map(ToString::to_string),
        "focus": panel.holds_focus(window, cx),
    })
}

fn telegram_state(panel: &Entity<TelegramImport>, cx: &App) -> Value {
    let panel = panel.read(cx);
    let token_state = match panel.token_state() {
        TokenState::Checking => "Checking",
        TokenState::None => "None",
        TokenState::Saved => "Saved",
        TokenState::Unreadable => "Unreadable",
    };
    json!({
        "token_state": token_state,
        "asks_token": panel.asks_token(),
        "problem": panel.problem().map(ToString::to_string),
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
                "state": state,
                "given": [picture.given.0, picture.given.1],
            })
        })
        .collect();
    json!({
        "text": compose.value(cx).to_string(),
        "pictures": pictures,
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
        }),
        ImportState::Done(imported) => json!({
            "done": imported.pack.id,
            "skipped": imported.skipped.iter().map(ToString::to_string).collect::<Vec<_>>(),
        }),
        ImportState::Failed(error) => json!({"failed": error.to_string()}),
    });
    json!({
        "packs": packs,
        "import": import,
    })
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

fn change_settings(shell: &Entity<Shell>, cx: &mut App, change: impl FnOnce(&mut teams_ui::settings::Settings)) {
    let model = shell.read(cx).settings().clone();
    model.update(cx, |model, cx| model.change(change, cx));
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
