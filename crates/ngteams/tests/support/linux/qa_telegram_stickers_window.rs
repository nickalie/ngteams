//! QA of task 651 (round 1), in the UI sandbox: Telegram stickers in the compose box's picker. The real app runs as a
//! child in the sandbox, with lab accounts a and b in one window on `teams-mock` (its chat service, its media service
//! and its Trouter), and a mock Telegram Bot API (`teams_mock::telegram`) on the loopback. The parent drives the app
//! with the sandbox's virtual keyboard and pointer. A fake file chooser on the sandbox's bus gives the folder, the
//! `.zip` file or the files that the user picks. The parent finds the stickers in the screenshots by their colours,
//! and asks the child what the views hold through a file in the sandbox dir (`harness = false`: GPUI runs on the
//! process's main thread).
//!
//! - **Files**: a folder, a `.zip` file and chosen files, each of a `.tgs`, a `.webp` and a `.webm`: the `.tgs` is an
//!   animated sticker (its first frame in the picker, playing in the chat), the `.webp` a still one, the `.webm` skipped with its note. The "+"
//!   menu's rows.
//! - **The Telegram panel**: the link's forms, a link that names no pack, no token, a malformed token, a refused
//!   token, a missing pack, a pack of only video stickers, a Bot API that can't be reached; each says the user's words
//!   and sends only what it must. A good import shows its progress and its summary.
//! - **The token**: never drawn (the masked field's pixels do not change with the token), Replace / Keep it / Remove,
//!   kept after a restart, and in no line of the app's debug log, its output or the chat service's requests.
//! - **The keyboard**: Tab, Shift+Tab, Enter, Space and Escape in the panel.
//! - **a sends** the imported animated sticker; **b sees** it play.
//! - **No regression**: text in the box after the panel, the emoji tab. Teams Light and Dark shots of the panel.
//!
//! The app's secret store is the in-memory one (`AppSecrets::in_memory`): the sandbox has no Secret Service. The
//! restart puts the token that the first run kept back in it, as a keyring keeps it.
//!
//! `QA651_ONLY` (a comma list of check names) runs only those. `QA651_EVIDENCE` names a directory that gets the
//! screenshots, the results, the Bot API's requests and the app's debug log. Screenshots also go to
//! `target/ui-shots/qa651-*.png`. Every name, token, text and picture is made up.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::collections::{BTreeSet, HashMap, VecDeque};
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
use teams_core::telegram::BotApi;
use teams_core::{CallFloor, Outbox};
use teams_mock::telegram::{BotApiMock, MockKind, MockRequest, MockSet, MockSticker, SYNTHETIC_BOT_TOKEN};
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
use teams_ui::stickers::telegram::{BotApis, BotToken, TOKEN_NAME};
use teams_ui::stickers::{ImportState, Shelf};
use teams_ui::telegram_import::{self, TelegramImport, TokenState};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::{VirtualInput, keys};
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

const CHILD_ENV: &str = "QA651_CHILD";
const ONLY_ENV: &str = "QA651_ONLY";
const EVIDENCE_ENV: &str = "QA651_EVIDENCE";
const TRACE_FD_ENV: &str = "QA651_TRACE_FD";
const ORIGIN_ENV: &str = "QA651_ORIGIN";
const TROUTER_ENV: &str = "QA651_TROUTER";
const DIR_ENV: &str = "QA651_DIR";
const TOKENS_ENV: &str = "QA651_TOKENS";
const BOT_ENV: &str = "QA651_BOT";
const DEAD_BOT_ENV: &str = "QA651_DEAD_BOT";
/// The file that keeps the bot token between runs, as a keyring keeps it.
const SEED_ENV: &str = "QA651_SEED";
/// The file in the sandbox dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "qa651-ask";
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

/// Where a user sees the box's emoji button and the picker's tabs at 1280 px and font 14 (the UI tour's shots).
const EMOJI_BUTTON: (i32, i32) = (1101, 750);
const TAB_STICKERS: (i32, i32) = (924, 370);
/// The picker on the screen, and the chat's messages.
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

/// The colours of the made-up stickers: the `.tgs` ball (pure blue, as the fixture draws it) and the `.webp` disc.
type Rgb = [u8; 3];
const BALL: Rgb = [0, 0, 255];
const DISC: Rgb = [250, 200, 40];

/// The made-up bot tokens: the mock's, one it refuses, one of the wrong form, and two of one length for the mask.
const REFUSED_TOKEN: &str = "987654321:QA_refused_made_up_token_0000000";
const MALFORMED_TOKEN: &str = "not-a-bot-token";
const MASK_ONE: &str = "111111111:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const MASK_TWO: &str = "999999999:mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm";

/// The mock Bot API's packs.
const GOOD_SET: &str = "QaSyntheticBlobs";
const GOOD_TITLE: &str = "QA synthetic blobs";
const VIDEO_SET: &str = "QaOnlyVideo";
const MISSING_SET: &str = "QaNoSuchPack";
/// The still stickers of the good pack, beside its animated and its video sticker: enough for a progress line.
const GOOD_STILLS: usize = 10;

/// The folders and the `.zip` file the user imports.
const FOLDER: &str = "Qa tg folder";
const ZIP: &str = "Qa tg zip";
const CHOSEN: &str = "Qa tg chosen";

const ANIMATED_EMOJI: &str = "\u{1f638}";
const VIDEO_EMOJI: &str = "\u{1f639}";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_telegram_stickers_window: {error}");
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

/// The first run of the app.
const FIRST: [Check; 13] = [
    ("import_folder", import_folder),
    ("menu_rows", menu_rows),
    ("import_zip", import_zip),
    ("import_chosen_files", import_chosen_files),
    ("animated_shows_in_picker", animated_shows_in_picker),
    ("panel_link_and_token_problems", panel_link_and_token_problems),
    ("panel_telegram_failures", panel_telegram_failures),
    ("panel_good_import", panel_good_import),
    ("token_never_drawn", token_never_drawn),
    ("token_replace_keep_remove", token_replace_keep_remove),
    ("panel_keyboard", panel_keyboard),
    ("send_animated_b_sees_it_play", send_animated_b_sees_it_play),
    ("no_regression_and_themes", no_regression_and_themes),
];

/// The second run of the app over the same settings, store and kept token.
const RESTART: [Check; 1] = [("token_kept_after_restart", token_kept_after_restart)];

#[derive(Default)]
struct Tally {
    passed: Vec<String>,
    failed: Vec<(String, String)>,
}

/// What the runs share.
struct World<'a> {
    sandbox: &'a Sandbox,
    mock: &'a ChatServiceMock,
    bot: &'a BotApiMock,
    runtime: &'a tokio::runtime::Runtime,
    chooser: &'a Chooser,
    sources: &'a Path,
    dir: &'a Path,
    trouter: &'a str,
    dead_bot: &'a str,
}

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_telegram_stickers_window", &options)? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mock = runtime.block_on(ChatServiceMock::start())?;
    mock.post_as(LAB_B, AB_CHAT, "<p>Made-up words before the Telegram stickers</p>")
        .ok_or("b's words refused")?;
    let trouter = mock.start_trouter()?;
    let bot = BotApiMock::start(bot_sets()?)?;
    let dead_bot = dead_origin()?;
    let chooser = Chooser::start(sandbox.bus().address())?;
    let sources = tempfile::tempdir()?;
    make_sources(sources.path())?;
    let dir = tempfile::tempdir()?;
    let world = World {
        sandbox: &sandbox,
        mock: &mock,
        bot: &bot,
        runtime: &runtime,
        chooser: &chooser,
        sources: sources.path(),
        dir: dir.path(),
        trouter: trouter.origin(),
        dead_bot: &dead_bot,
    };
    let mut tally = run_all(&world)?;
    secrets_faults(&world, &mut tally);
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
    for (run, checks) in [("first", &FIRST[..]), ("restart", &RESTART[..])] {
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
        "qa_telegram_stickers_window: {} passed, {} failed",
        tally.passed.len(),
        tally.failed.len()
    ));
    ensure(tally.failed.is_empty(), || {
        format!("{} checks failed", tally.failed.len())
    })
}

/// The tokens the runs typed, and where they must never be: the app's debug log, its stderr, the other files of its
/// data dir and the chat service's requests. The Bot API's requests and the log go to the evidence.
fn secrets_faults(world: &World<'_>, tally: &mut Tally) {
    let check = "token_in_no_log_or_request".to_owned();
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
    let requests: Vec<String> = world
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
    if let Some(dir) = evidence_dir() {
        let _ = fs::write(dir.join("app-debug-log.txt"), &logs);
        let bot: Vec<String> = world
            .bot
            .requests()
            .iter()
            .map(|request| format!("{} auth={}", masked(&request.target), request.authorization))
            .collect();
        let _ = fs::write(dir.join("bot-api-requests.txt"), bot.join("\n"));
    }
    let mut found = Vec::new();
    for token in [SYNTHETIC_BOT_TOKEN, REFUSED_TOKEN, MASK_ONE, MASK_TWO] {
        let secret = token.split_once(':').map_or(token, |(_, key)| key);
        if logs.contains(secret) {
            found.push(format!("a token in the app's debug log ({})", masked(token)));
        }
        for (path, text) in &files {
            let seed = path.file_name().is_some_and(|name| name == "kept-token");
            if !seed && text.contains(secret) {
                found.push(format!("a token in {} ({})", path.display(), masked(token)));
            }
        }
        if requests.iter().any(|request| request.contains(secret)) {
            found.push(format!("a token in a request to the chat service ({})", masked(token)));
        }
    }
    say(&format!(
        "check {check}: log {} bytes, {} files, {} chat service requests",
        logs.len(),
        files.len(),
        requests.len()
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

/// A Bot API request's target with any token's secret part hidden, for the evidence.
fn masked(text: &str) -> String {
    let mut out = text.to_owned();
    for token in [SYNTHETIC_BOT_TOKEN, REFUSED_TOKEN, MASK_ONE, MASK_TWO] {
        out = out.replace(token, "<token>");
        if let Some((_, key)) = token.split_once(':') {
            out = out.replace(key, "<key>");
        }
    }
    out
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

/// The synthetic, marked animation of the fixtures, gzipped as a `.tgs` is.
fn synthetic_tgs() -> SandboxResult<Vec<u8>> {
    let json = include_str!("../../../../../tests/fixtures/telegram/synthetic-sticker.json");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(json.as_bytes())?;
    Ok(encoder.finish()?)
}

/// A still WebP: a disc of `colour` on a clear 512 px square, as a Telegram sticker is.
fn disc_webp(colour: Rgb) -> SandboxResult<Vec<u8>> {
    let mut picture = RgbaImage::new(512, 512);
    for (x, y, pixel) in picture.enumerate_pixels_mut() {
        let (dx, dy) = (f64::from(x) - 256.0, f64::from(y) - 256.0);
        if dx.hypot(dy) < 150.0 {
            *pixel = Rgba([colour[0], colour[1], colour[2], 255]);
        }
    }
    let mut bytes = io::Cursor::new(Vec::new());
    picture.write_to(&mut bytes, ImageFormat::WebP)?;
    Ok(bytes.into_inner())
}

/// The start of a Matroska file: a Telegram video sticker, as the import sees one.
fn video() -> Vec<u8> {
    vec![0x1a, 0x45, 0xdf, 0xa3, 0x80, 0x42, 0x86, 0x81, 0x01]
}

fn bot_sets() -> SandboxResult<Vec<MockSet>> {
    let mut good = vec![
        MockSticker {
            kind: MockKind::Animated,
            emoji: ANIMATED_EMOJI.to_owned(),
            bytes: synthetic_tgs()?,
        },
        MockSticker {
            kind: MockKind::Video,
            emoji: VIDEO_EMOJI.to_owned(),
            bytes: video(),
        },
    ];
    for _ in 0..GOOD_STILLS {
        good.push(MockSticker {
            kind: MockKind::Still,
            emoji: "\u{1f63a}".to_owned(),
            bytes: disc_webp(DISC)?,
        });
    }
    let only_video = (0..2)
        .map(|_| MockSticker {
            kind: MockKind::Video,
            emoji: VIDEO_EMOJI.to_owned(),
            bytes: video(),
        })
        .collect();
    Ok(vec![
        MockSet {
            name: GOOD_SET.to_owned(),
            title: GOOD_TITLE.to_owned(),
            stickers: good,
        },
        MockSet {
            name: VIDEO_SET.to_owned(),
            title: "QA only video".to_owned(),
            stickers: only_video,
        },
    ])
}

/// The three of a kind each source holds: an animated `.tgs`, a still `.webp` and a `.webm` video.
fn three(prefix: &str) -> SandboxResult<Vec<(String, Vec<u8>)>> {
    Ok(vec![
        (format!("{prefix}_ball.tgs"), synthetic_tgs()?),
        (format!("{prefix}_disc.webp"), disc_webp(DISC)?),
        (format!("{prefix}_clip.webm"), video()),
    ])
}

/// A folder, a `.zip` file and a folder whose files the user chooses one by one, in `root`.
fn make_sources(root: &Path) -> SandboxResult {
    for (name, prefix) in [(FOLDER, "folder"), (CHOSEN, "chosen")] {
        let dir = root.join(name);
        fs::create_dir_all(&dir)?;
        for (file, bytes) in three(prefix)? {
            fs::write(dir.join(file), bytes)?;
        }
    }
    let file = fs::File::create(root.join(format!("{ZIP}.zip")))?;
    let mut zip = zip::ZipWriter::new(file);
    for (file, bytes) in three("zip")? {
        zip.start_file(format!("{ZIP}/{file}"), zip::write::SimpleFileOptions::default())?;
        zip.write_all(&bytes)?;
    }
    zip.finish()?;
    Ok(())
}

fn chosen_files(root: &Path) -> Vec<PathBuf> {
    let dir = root.join(CHOSEN);
    ["chosen_ball.tgs", "chosen_disc.webp", "chosen_clip.webm"]
        .iter()
        .map(|file| dir.join(file))
        .collect()
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
                (DEAD_BOT_ENV, world.dead_bot),
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
}

impl<'a> User<'a> {
    fn new(world: &'a World<'a>, app: AppUnderTest) -> SandboxResult<Self> {
        let mut user = Self {
            world,
            app,
            input: world.sandbox.input()?,
            asked: 0,
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
        // the next check starts from a's a–b chat, Teams Light, no picker, an empty box, the live Bot API
        let _ = self.press("escape");
        let _ = self.press("escape");
        let _ = self.ask("reset");
        let _ = self.ask("bot live");
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

    fn click(&mut self, (x, y): (i32, i32)) -> SandboxResult {
        self.input.click(u32::try_from(x)?, u32::try_from(y)?, keys::BTN_LEFT)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn park(&mut self) -> SandboxResult {
        // the window's left margin: no button, tooltip or tile there
        self.input.move_to(3, SCREEN.1 / 2)
    }

    /// A screenshot as `qa651-<name>.png`, under `target/ui-shots/` and the evidence dir.
    fn shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        let file = format!("qa651-{name}.png");
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

    /// A screenshot of a frame that stopped changing in the picker (but for the caret): typed keys drawn late show.
    fn stable_image(&mut self, name: &str) -> SandboxResult<RgbImage> {
        let start = Instant::now();
        let mut before = self.image(name)?;
        loop {
            let now = self.image(name)?;
            if differ(&before, &now, PICKER) <= 100 || start.elapsed() > WAIT {
                return Ok(now);
            }
            before = now;
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

    /// The messages the chat mock was sent: the POSTs to a conversation's messages.
    fn messages_sent(&self) -> usize {
        self.world
            .runtime
            .block_on(self.world.mock.received_requests())
            .iter()
            .filter(|request| request.method.as_str() == "POST" && request.url.path().ends_with("/messages"))
            .count()
    }

    /// The Bot API's requests since `from`.
    fn bot_requests(&self, from: usize) -> Vec<MockRequest> {
        self.world.bot.requests().into_iter().skip(from).collect()
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
    &state["picker"]["telegram"]
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
    /// Ctrl+. in the box, then Ctrl+Tab twice: the picker on its Stickers tab.
    fn open_stickers_by_keys(&mut self) -> SandboxResult<Value> {
        // Ctrl+. toggles: a picker left open (an import's line in it) is closed first
        self.ask("close-picker")?;
        self.ask("focus-box")?;
        self.press("ctrl+.")?;
        self.until("the picker opened by Ctrl+.", picker_open)?;
        self.press("ctrl+tab")?;
        self.press("ctrl+tab")?;
        self.until("the Stickers tab", |state| picker(state)["tab"] == "Stickers")
    }

    /// The emoji button, then the Stickers tab, by the pointer.
    fn open_stickers_by_pointer(&mut self) -> SandboxResult<Value> {
        self.ask("focus-box")?;
        self.click(EMOJI_BUTTON)?;
        self.until("the picker opened by its button", picker_open)?;
        self.click(TAB_STICKERS)?;
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

    /// "Import a Telegram sticker pack" from the picker, by the keyboard: the panel open, its link field focused.
    fn open_panel(&mut self) -> SandboxResult<Value> {
        self.pick_import("sticker-import-telegram")?;
        self.until("the Telegram panel open, its token looked up", |state| {
            panel_open(state) && panel(state)["token_state"] != "Checking"
        })
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

// --- the checks: files ---

/// The skipped lines of the summary.
fn skipped(state: &Value) -> Vec<String> {
    import_line(state)["skipped"]
        .as_array()
        .map(|all| all.iter().filter_map(|one| one.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

/// A pack made of the three files: an animated sticker, a still one, and the video skipped with its note.
fn three_faults(state: &Value, name: &str, video: &str) -> Vec<String> {
    let mut faults = Vec::new();
    let Some(pack) = pack_named(state, name) else {
        return vec![format!("no pack {name:?}: {}", state["shelf"]["packs"])];
    };
    let stickers = pack["stickers"].as_array().cloned().unwrap_or_default();
    let animated: Vec<bool> = stickers.iter().map(|sticker| sticker["animated"] == true).collect();
    let mut kinds = animated.clone();
    kinds.sort_unstable();
    if kinds != [false, true] {
        faults.push(format!("{name}: not one animated and one still sticker: {stickers:?}"));
    }
    for sticker in &stickers {
        let file = sticker["file"].as_str().unwrap_or_default();
        let wanted = if sticker["animated"] == true { ".gif" } else { ".png" };
        if !file.ends_with(wanted) {
            faults.push(format!("{name}: {file} is not a {wanted}"));
        }
    }
    let lines = skipped(state);
    say(&format!("  {name} skipped: {lines:?}"));
    if !lines
        .iter()
        .any(|line| line.contains(video) && line.contains("is a video sticker, which can't be imported yet"))
    {
        faults.push(format!("{name}: no video note for {video}: {lines:?}"));
    }
    if lines.len() != 1 {
        faults.push(format!("{name}: {} skipped lines, 1 expected", lines.len()));
    }
    faults
}

/// The empty state's "Import a folder": a folder of a `.tgs`, a `.webp` and a `.webm`.
fn import_folder(user: &mut User<'_>) -> SandboxResult {
    let state = user.open_stickers_by_pointer()?;
    let mut faults = Vec::new();
    for item in ["sticker-import-folder", "sticker-import-zip", "sticker-import-telegram"] {
        if !has_item(&state, item) {
            faults.push(format!("the empty state has no {item}: {:?}", rows(&state)));
        }
    }
    user.image("empty-state")?;
    user.world.chooser.queue(vec![user.world.sources.join(FOLDER)]);
    user.key_pick("sticker-import-folder")?;
    let (_, state) = user.finish_import("import-folder")?;
    user.image("import-folder-summary")?;
    faults.extend(three_faults(&state, FOLDER, "folder_clip.webm"));
    faults_to_result(&faults)
}

/// The "+" menu has the four rows; Escape closes it.
fn menu_rows(user: &mut User<'_>) -> SandboxResult {
    user.open_stickers_by_keys()?;
    user.key_pick("sticker-pack-add")?;
    let state = user.until("the menu open", |state| has_item(state, "sticker-import-files"))?;
    let mut faults = Vec::new();
    let wanted = [
        "sticker-import-folder",
        "sticker-import-zip",
        "sticker-import-files",
        "sticker-import-telegram",
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
    faults_to_result(&faults)
}

/// "Import a .zip file of pictures" from the menu.
fn import_zip(user: &mut User<'_>) -> SandboxResult {
    user.world
        .chooser
        .queue(vec![user.world.sources.join(format!("{ZIP}.zip"))]);
    user.pick_import("sticker-import-zip")?;
    let (_, state) = user.finish_import("import-zip")?;
    user.image("import-zip-summary")?;
    faults_to_result(&three_faults(&state, ZIP, "zip_clip.webm"))
}

/// "Import picture or sticker files" from the menu: three files chosen at once make a pack named after their folder.
fn import_chosen_files(user: &mut User<'_>) -> SandboxResult {
    user.world.chooser.queue(chosen_files(user.world.sources));
    user.pick_import("sticker-import-files")?;
    let (_, state) = user.finish_import("import-files")?;
    user.image("import-files-summary")?;
    faults_to_result(&three_faults(&state, CHOSEN, "chosen_clip.webm"))
}

/// The x of every ball in `area`, left to right.
fn balls(image: &RgbImage, area: Rect) -> Vec<i32> {
    let mut xs: Vec<i32> = blobs(image, BALL, area, 60).iter().map(|blob| blob.x).collect();
    xs.sort_unstable();
    xs
}

/// Whether the balls in `area` move between screenshots within `WAIT`, and the still discs show.
fn plays(user: &mut User<'_>, area: Rect, name: &str) -> SandboxResult<(bool, usize)> {
    user.park()?;
    let mut seen = BTreeSet::new();
    let mut discs = 0;
    let start = Instant::now();
    let mut frame = 0;
    while seen.len() < 3 && start.elapsed() <= WAIT {
        let path = user.shot(&format!("{name}-{}", frame % 6))?;
        let image = image::open(path)?.to_rgb8();
        let found = balls(&image, area);
        if !found.is_empty() {
            seen.insert(found);
        }
        discs = discs.max(blobs(&image, DISC, area, 60).len());
        frame += 1;
        thread::sleep(Duration::from_millis(90));
    }
    say(&format!(
        "  {name}: ball places {seen:?}, discs {discs}, in {frame} shots"
    ));
    Ok((seen.len() >= 2, discs))
}

/// The folder's pack, open in the picker: its still disc and its animated ball show. The picker draws each sticker's
/// first frame (`Shelf::thumb`, as for the animated GIF and WebP stickers before this task); whether it plays is said.
fn animated_shows_in_picker(user: &mut User<'_>) -> SandboxResult {
    let state = user.open_stickers_by_keys()?;
    let id = pack_named(&state, FOLDER)
        .and_then(|pack| pack["id"].as_str())
        .ok_or("no folder pack")?
        .to_owned();
    user.key_pick(&format!("sticker-pack-{id}"))?;
    user.until("the pack open", |state| shows_pack_stickers(state, &id))?;
    let image = user.image("picker-pack-shows")?;
    let (moved, discs) = plays(user, PICKER, "picker-pack-plays")?;
    say(&format!("  the animated sticker plays in the picker: {moved}"));
    let mut faults = Vec::new();
    if balls(&image, PICKER).is_empty() {
        faults.push("the animated (.tgs) sticker does not show in the picker".to_owned());
    }
    if discs == 0 {
        faults.push("the still (.webp) sticker does not show in the picker".to_owned());
    }
    faults_to_result(&faults)
}

// --- the checks: the Telegram panel ---

const GOOD_LINK: &str = "https://t.me/addstickers/QaSyntheticBlobs";

/// What the import line says of a failed import.
fn failure(state: &Value) -> Option<String> {
    import_line(state)["failed"].as_str().map(str::to_owned)
}

fn problem(state: &Value) -> Option<String> {
    panel(state)["problem"].as_str().map(str::to_owned)
}

impl User<'_> {
    /// Retypes the focused field, waits for its problem to clear, then presses Enter: the problem it gives, or `None`
    /// when the panel closed.
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

    /// The import the panel started ends: its failure's words, or `None` when it imported.
    fn import_end(&mut self, name: &str) -> SandboxResult<(Vec<Value>, Value)> {
        let (running, state) = self.finish_import(name)?;
        Ok((running, state))
    }

    /// Whether the app's keyring keeps the mock's token ("mock"), another ("other") or none ("none").
    fn kept_token(&mut self) -> SandboxResult<String> {
        self.ask("kept-token")
    }
}

/// The Bot API requests' faults: none may carry an `Authorization` header (the token goes only in the path).
fn bot_request_faults(requests: &[MockRequest], what: &str) -> Vec<String> {
    let mut faults = Vec::new();
    for request in requests {
        if request.authorization {
            faults.push(format!("{what}: a request to the Bot API with an Authorization header"));
        }
        let path = request.target.split('?').next().unwrap_or_default();
        if !(path.starts_with("/bot") || path.starts_with("/file/bot")) {
            faults.push(format!("{what}: a request off the Bot API's paths: {}", masked(path)));
        }
    }
    faults
}

/// A link that names no pack, the link's forms, no token and a malformed token: the panel says each in the user's
/// words, and nothing goes to Telegram or the chat service.
fn panel_link_and_token_problems(user: &mut User<'_>) -> SandboxResult {
    let (bot_before, sent_before) = (user.world.bot.requests().len(), user.messages_sent());
    let state = user.open_panel()?;
    let mut faults = Vec::new();
    let shown = panel(&state);
    if shown["token_state"] != "None" || shown["asks_token"] != true || shown["focus"] != true {
        faults.push(format!("the panel at open: {shown}"));
    }
    user.image("panel-light-no-token")?;
    user.ask("theme-dark")?;
    user.image("panel-dark-no-token")?;
    user.ask("theme-light")?;
    for bad in [
        "https://example.test/addstickers/QaX",
        "t.me/joinchat/QaSyntheticBlobs",
        "t.me/addstickers/",
    ] {
        let said = user.submit(Some(bad), &format!("the link {bad}"))?;
        if said.as_deref() != Some(telegram_import::BAD_LINK) {
            faults.push(format!("the link {bad:?} gives {said:?}, not the bad-link line"));
        }
    }
    user.image("panel-bad-link")?;
    for good in [
        GOOD_LINK,
        "t.me/addstickers/QaSyntheticBlobs",
        "telegram.me/addstickers/QaSyntheticBlobs/",
        "tg://addstickers?set=QaSyntheticBlobs",
        "https://t.me/addemoji/QaSyntheticBlobs",
        "QaSyntheticBlobs",
    ] {
        let said = user.submit(Some(good), &format!("the link {good}"))?;
        if said.as_deref() != Some(telegram_import::NO_TOKEN) {
            faults.push(format!(
                "the link {good:?} with no token gives {said:?}, not the no-token line"
            ));
        }
        // the problem took the focus to the token field: Shift+Tab goes back to the link
        user.press("shift+tab")?;
    }
    user.image("panel-no-token")?;
    user.press("tab")?;
    let said = user.submit(Some(MALFORMED_TOKEN), "a malformed token")?;
    if said.as_deref() != Some(telegram_import::BAD_TOKEN) {
        faults.push(format!("a malformed token gives {said:?}"));
    }
    user.image("panel-light-bad-token")?;
    user.ask("theme-dark")?;
    user.image("panel-dark-bad-token")?;
    user.ask("theme-light")?;
    let sent = user.bot_requests(bot_before);
    if !sent.is_empty() {
        faults.push(format!(
            "{} requests went to Telegram before a good link and token",
            sent.len()
        ));
    }
    if user.messages_sent() != sent_before {
        faults.push("a message went to the chat service".to_owned());
    }
    let kept = user.kept_token()?;
    if kept != "none" {
        faults.push(format!("a token was kept after a malformed one: {kept}"));
    }
    faults_to_result(&faults)
}

/// One import from the panel that fails: the line it ends with and the Bot API's requests for it.
fn failed_import(
    user: &mut User<'_>,
    name: &str,
    link: &str,
    token: Option<&str>,
) -> SandboxResult<(Option<String>, Vec<MockRequest>)> {
    let before = user.world.bot.requests().len();
    let state = user.open_panel()?;
    user.retype(link)?;
    if let Some(token) = token {
        if panel(&state)["asks_token"] == true {
            user.press("tab")?;
        } else {
            // a kept token: Tab to Replace, Space shows the field and focuses it
            user.press("tab")?;
            user.press("space")?;
            user.until("Replace showed the token field", |state| {
                panel(state)["asks_token"] == true
            })?;
        }
        user.retype(token)?;
    }
    let said = user.submit(None, name)?;
    if let Some(said) = said {
        return Err(format!("{name}: the panel says {said:?}").into());
    }
    let (_, state) = user.import_end(name)?;
    user.image(&format!("{name}-line"))?;
    let failed = failure(&state);
    say(&format!("  {name}: {failed:?}"));
    Ok((failed, user.bot_requests(before)))
}

fn methods(requests: &[MockRequest]) -> Vec<String> {
    requests
        .iter()
        .map(|request| {
            let path = request.target.split('?').next().unwrap_or_default();
            if path.starts_with("/file/") {
                "download".to_owned()
            } else {
                path.rsplit('/').next().unwrap_or_default().to_owned()
            }
        })
        .collect()
}

/// A failed import from the panel: its name, the link, the token typed, the line it ends with, what Telegram is asked.
type FailureCase = (
    &'static str,
    &'static str,
    Option<&'static str>,
    &'static str,
    &'static [&'static str],
);

/// A refused token, a missing pack, a pack of only video stickers and a Bot API that can't be reached: each import
/// ends in the user's words, keeps no pack and asks Telegram only what it must.
fn panel_telegram_failures(user: &mut User<'_>) -> SandboxResult {
    let sent_before = user.messages_sent();
    let packs_before = user.state()?["shelf"]["packs"].as_array().map_or(0, Vec::len);
    let mut faults = Vec::new();
    let cases: [FailureCase; 4] = [
        (
            "refused-token",
            GOOD_LINK,
            Some(REFUSED_TOKEN),
            "Telegram refused the bot token. Check it, or replace it.",
            &["getStickerSet"],
        ),
        (
            "missing-pack",
            "t.me/addstickers/QaNoSuchPack",
            Some(SYNTHETIC_BOT_TOKEN),
            "Telegram has no sticker pack at that link.",
            &["getStickerSet"],
        ),
        (
            "only-video",
            "t.me/addstickers/QaOnlyVideo",
            None,
            "No PNG, WebP or GIF picture or Telegram sticker there could be made a sticker.",
            &["getStickerSet"],
        ),
        (
            "dead-bot-api",
            GOOD_LINK,
            None,
            "Telegram can't be reached. Check your connection and try again.",
            &[],
        ),
    ];
    for (name, link, token, words, asked) in cases {
        if name == "dead-bot-api" {
            user.ask("bot dead")?;
        }
        let (failed, requests) = failed_import(user, name, link, token)?;
        if failed.as_deref() != Some(words) {
            faults.push(format!("{name}: the import line says {failed:?}, not {words:?}"));
        }
        let got = methods(&requests);
        if got != asked {
            faults.push(format!("{name}: Telegram was asked {got:?}, {asked:?} expected"));
        }
        faults.extend(bot_request_faults(&requests, name));
        if name == "refused-token" && user.kept_token()? != "other" {
            faults.push("the refused token was not kept for Replace".to_owned());
        }
        user.ask("close-import")?;
        user.ask("bot live")?;
    }
    let _ = MISSING_SET;
    let _ = VIDEO_SET;
    let state = user.state()?;
    let packs = state["shelf"]["packs"].as_array().map_or(0, Vec::len);
    if packs != packs_before {
        faults.push(format!(
            "a failed import kept a pack: {packs_before} packs before, {packs} after"
        ));
    }
    if user.messages_sent() != sent_before {
        faults.push("a message went to the chat service".to_owned());
    }
    if user.kept_token()? != "mock" {
        faults.push("the replaced token is not the one typed".to_owned());
    }
    faults_to_result(&faults)
}

/// A good link with the kept token: the progress line, then the summary with the video sticker's note; the pack with
/// its animated and still stickers, labelled by their emoji. Each request has the token in its path only, and the
/// video sticker is never downloaded.
fn panel_good_import(user: &mut User<'_>) -> SandboxResult {
    let before = user.world.bot.requests().len();
    user.open_panel()?;
    user.retype(GOOD_LINK)?;
    if let Some(said) = user.submit(None, "the good link")? {
        return Err(format!("the panel says {said:?}").into());
    }
    let (running, state) = user.import_end("good-import")?;
    user.image("good-import-summary")?;
    let mut faults = Vec::new();
    say(&format!(
        "  progress seen {} times: {:?}",
        running.len(),
        running.first()
    ));
    if running.is_empty() {
        faults.push("no progress line was seen".to_owned());
    }
    if let Some(failed) = failure(&state) {
        faults.push(format!("the import failed: {failed}"));
    }
    let lines = skipped(&state);
    say(&format!("  skipped: {lines:?}"));
    let note = format!("Sticker 2 {VIDEO_EMOJI} is a video sticker, which can't be imported yet");
    if lines != [note.clone()] && !lines.iter().any(|line| line.contains(&note)) {
        faults.push(format!("the summary's skipped lines {lines:?}, {note:?} expected"));
    }
    match pack_named(&state, GOOD_TITLE) {
        Some(pack) => {
            let stickers = pack["stickers"].as_array().cloned().unwrap_or_default();
            let animated: Vec<&Value> = stickers.iter().filter(|sticker| sticker["animated"] == true).collect();
            if stickers.len() != GOOD_STILLS + 1 || animated.len() != 1 {
                faults.push(format!("the pack's stickers: {stickers:?}"));
            }
            if animated
                .first()
                .is_none_or(|sticker| sticker["label"] != ANIMATED_EMOJI)
            {
                faults.push(format!("the animated sticker's label: {animated:?}"));
            }
        }
        None => faults.push(format!("no pack {GOOD_TITLE:?}: {}", state["shelf"]["packs"])),
    }
    let requests = user.bot_requests(before);
    let got = methods(&requests);
    let wanted_files = GOOD_STILLS + 1;
    let count = |method: &str| got.iter().filter(|one| *one == method).count();
    if count("getStickerSet") != 1 || count("getFile") != wanted_files || count("download") != wanted_files {
        faults.push(format!("Telegram was asked {got:?}"));
    }
    let video = format!("{GOOD_SET}__1");
    if requests.iter().any(|request| {
        request.target.ends_with(&video)
            || request.target.contains(&format!("{video}&"))
            || request.target.contains(&format!("{video}."))
    }) {
        faults.push("the video sticker was asked for".to_owned());
    }
    faults.extend(bot_request_faults(&requests, "the good import"));
    user.key_pick("sticker-import-close")?;
    let state = user.until("the summary closed", |state| import_line(state).is_null())?;
    let id = pack_named(&state, GOOD_TITLE)
        .and_then(|pack| pack["id"].as_str())
        .unwrap_or_default()
        .to_owned();
    if !has_item(&state, &format!("sticker-pack-{id}")) {
        faults.push(format!("the Stickers tab does not show the pack: {:?}", rows(&state)));
    }
    user.image("good-import-pack-shows")?;
    faults_to_result(&faults)
}

/// The pixels in `area` that differ between `one` and `two`.
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

/// A token typed in the token field is never drawn: two tokens of one length give the same pixels there (but for the
/// caret), while the same two in the link field give different pixels.
fn token_never_drawn(user: &mut User<'_>) -> SandboxResult {
    let state = user.open_panel()?;
    ensure(panel(&state)["token_state"] == "Saved", || {
        format!("no token kept: {}", panel(&state))
    })?;
    let mut faults = Vec::new();
    if panel(&state)["asks_token"] != false {
        faults.push("the token field shows while a token is kept".to_owned());
    }
    user.image("panel-token-saved")?;
    user.press("tab")?;
    user.press("space")?;
    user.until("Replace showed the token field", |state| {
        panel(state)["asks_token"] == true
    })?;
    let mut shots = Vec::new();
    for (field, name) in [("token", "mask"), ("link", "plain")] {
        if field == "link" {
            // from the token field, past Remove and "Keep it", drawn above it
            for _ in 0..3 {
                user.press("shift+tab")?;
            }
        }
        for (index, token) in [MASK_ONE, MASK_TWO].into_iter().enumerate() {
            user.retype(token)?;
            shots.push(user.stable_image(&format!("{name}-{index}"))?);
        }
    }
    let masked = differ(&shots[0], &shots[1], PICKER);
    let plain = differ(&shots[2], &shots[3], PICKER);
    say(&format!(
        "  px that change with the token: token field {masked}, link field {plain}"
    ));
    if plain < 300 {
        faults.push(format!(
            "the link field shows little of two different texts ({plain} px): the method fails"
        ));
    }
    if masked * 5 > plain || masked > 200 {
        faults.push(format!(
            "the token field shows the token: {masked} px change with it ({plain} in the link)"
        ));
    }
    user.press("escape")?;
    user.until("Escape closed the panel", |state| !panel_open(state))?;
    if user.kept_token()? != "mock" {
        faults.push("a token typed then cancelled replaced the kept one".to_owned());
    }
    faults_to_result(&faults)
}

/// With a token kept: Replace shows the field ("Keep it" hides it again and keeps the token), Remove forgets it and
/// the field shows; a token typed in then is kept again.
fn token_replace_keep_remove(user: &mut User<'_>) -> SandboxResult {
    let mut faults = Vec::new();
    user.open_panel()?;
    user.press("tab")?;
    user.press("enter")?;
    let state = user.until("Replace showed the token field", |state| {
        panel(state)["asks_token"] == true
    })?;
    if panel(&state)["token_state"] != "Saved" {
        faults.push(format!("Replace forgot the token: {}", panel(&state)));
    }
    user.image("panel-replacing")?;
    // the token field has the focus; it is drawn under "Keep it" and Remove: Shift+Tab twice to "Keep it", Space
    user.press("shift+tab")?;
    user.press("shift+tab")?;
    user.press("space")?;
    let state = user.until("Keep it hid the field", |state| panel(state)["asks_token"] == false)?;
    if panel(&state)["token_state"] != "Saved" || user.kept_token()? != "mock" {
        faults.push(format!("Keep it lost the token: {}", panel(&state)));
    }
    user.image("panel-kept")?;
    user.press("tab")?;
    user.press("enter")?;
    let state = user.until("Remove forgot the token", |state| panel(state)["token_state"] == "None")?;
    if panel(&state)["asks_token"] != true {
        faults.push("no token field after Remove".to_owned());
    }
    if user.kept_token()? != "none" {
        faults.push("Remove left the token in the keyring".to_owned());
    }
    if panel(&state)["focus"] != true {
        faults.push("after Remove by the keyboard the focus is not in the panel".to_owned());
    }
    user.image("panel-removed")?;
    user.press("escape")?;
    if user
        .until("Escape closed the panel", |state| !panel_open(state))
        .is_err()
    {
        faults.push("after Remove by the keyboard, Escape does not close the panel".to_owned());
        user.ask("close-picker")?;
    }
    // the token again, typed with a link to a pack that is not there: kept, and nothing imported
    let (failed, _) = failed_import(
        user,
        "token-again",
        "t.me/addstickers/QaNoSuchPack",
        Some(SYNTHETIC_BOT_TOKEN),
    )?;
    if failed.as_deref() != Some("Telegram has no sticker pack at that link.") || user.kept_token()? != "mock" {
        faults.push(format!("the token typed after Remove: {failed:?}"));
    }
    faults_to_result(&faults)
}

/// The panel by the keyboard: it opens from the menu's row with Enter, Shift+Tab reaches the back arrow and Enter
/// presses it, Tab reaches Cancel and Space presses it, Tab and Shift+Tab go round, and Escape goes back to the
/// picker, not out of it.
fn panel_keyboard(user: &mut User<'_>) -> SandboxResult {
    let mut faults = Vec::new();
    let back_to_picker = |state: &Value| !panel_open(state) && picker_open(state) && picker(state)["tab"] == "Stickers";
    let state = user.open_panel()?;
    if panel(&state)["focus"] != true {
        faults.push("the panel opened without the focus".to_owned());
    }
    user.press("shift+tab")?;
    user.press("enter")?;
    if let Err(error) = user.until("Shift+Tab, Enter: the back arrow", back_to_picker) {
        faults.push(error.to_string());
    }
    // with a token kept, the stops are: back, link, Replace, Remove, Import, Cancel
    user.open_panel()?;
    for _ in 0..4 {
        user.press("tab")?;
    }
    user.press("space")?;
    if let Err(error) = user.until("Tab four times, Space: Cancel", back_to_picker) {
        faults.push(error.to_string());
    }
    for (key, name) in [("tab", "Tab"), ("shift+tab", "Shift+Tab")] {
        let state = user.open_panel()?;
        // back, link, Import, Cancel, and the token field or (a token kept) Replace and Remove
        let stops = if panel(&state)["token_state"] == "Saved" { 6 } else { 5 };
        for step in 1..=stops {
            user.press(key)?;
            user.image(&format!("keys-{}-{step}", name.to_lowercase().replace('+', "-")))?;
        }
        // round to the link field: a link typed there and Enter import it (from Import, an empty link is refused)
        user.type_text("t.me/addstickers/QaNoSuchPack")?;
        let said = user.submit(None, &format!("{name} round"))?;
        if let Some(said) = said {
            faults.push(format!(
                "{name} {stops} times did not come round to the link field: {said:?}"
            ));
            continue;
        }
        let (_, state) = user.import_end(&format!("keys-{}", name.to_lowercase().replace('+', "-")))?;
        if failure(&state).as_deref() != Some("Telegram has no sticker pack at that link.") {
            faults.push(format!("{name} round: {}", import_line(&state)));
        }
        user.ask("close-import")?;
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

/// a picks the Telegram pack's animated sticker by the keyboard and sends it with Enter; it goes as a Teams sticker.
/// b sees it bare, and it plays.
fn send_animated_b_sees_it_play(user: &mut User<'_>) -> SandboxResult {
    let mut faults = Vec::new();
    let state = user.open_stickers_by_keys()?;
    let pack = pack_named(&state, GOOD_TITLE).ok_or("no Telegram pack")?.clone();
    let id = pack["id"].as_str().unwrap_or_default().to_owned();
    let file = pack["stickers"]
        .as_array()
        .and_then(|stickers| stickers.iter().find(|sticker| sticker["animated"] == true))
        .and_then(|sticker| sticker["file"].as_str())
        .ok_or("no animated sticker in the Telegram pack")?
        .to_owned();
    user.key_pick(&format!("sticker-pack-{id}"))?;
    user.until("the pack open", |state| shows_pack_stickers(state, &id))?;
    if balls(&user.image("telegram-pack-in-picker")?, PICKER).is_empty() {
        faults.push("the Telegram pack's animated sticker does not show in the picker".to_owned());
    }
    user.serve_upload(&id, &file)?;
    let number = file.split('.').next().unwrap_or_default();
    let before = user.sent_with("POST", "schema.skype.com/Sticker").len();
    user.key_pick(&format!("sticker-{id}-{number}"))?;
    let state = user.until("the sticker in the box, sent to the media service", |state| {
        let pictures = pictures(state);
        !picker_open(state) && pictures.len() == 1 && pictures[0]["state"] == "sent"
    })?;
    if pictures(&state)[0]["sticker"] != ANIMATED_EMOJI {
        faults.push(format!("the sticker in the box: {}", pictures(&state)[0]));
    }
    user.image("send-in-box")?;
    user.ask("focus-box")?;
    user.press("enter")?;
    let sent = wait_until(WAIT, || {
        let all = user.sent_with("POST", "schema.skype.com/Sticker");
        (all.len() > before).then(|| all.last().cloned().unwrap_or_default())
    })
    .ok_or("no sticker message went to the chat service")?;
    if !sent.contains(ANIMATED_EMOJI) && !sent.contains("\\ud83d\\ude38") {
        faults.push(format!("the message does not name the sticker by its emoji: {sent}"));
    }
    user.switch("b")?;
    user.open(AB_CHAT)?;
    let state = user.until("a's Telegram sticker in b's chat", |state| {
        items(state)
            .iter()
            .any(|item| item["label"] == ANIMATED_EMOJI && item["own"] == false)
    })?;
    let item = items(&state)
        .into_iter()
        .rfind(|item| item["label"] == ANIMATED_EMOJI)
        .unwrap_or_default();
    if item["bare"] != true || item["sticker_alone"] != true {
        faults.push(format!("b's message is not a bare sticker: {item}"));
    }
    let (moved, _) = plays(user, MESSAGES, "b-sees-telegram-sticker")?;
    if !moved {
        faults.push("the animated Telegram sticker does not play for b".to_owned());
    }
    user.ask("theme-dark")?;
    let (moved, _) = plays(user, MESSAGES, "b-sees-telegram-sticker-dark")?;
    if !moved {
        faults.push("the animated Telegram sticker does not play for b in Teams Dark".to_owned());
    }
    faults_to_result(&faults)
}

/// After the panel: text typed in the box goes as a message, and the picker's Emoji tab still works. Teams Light and
/// Dark shots of the panel with a token kept and with a problem.
fn no_regression_and_themes(user: &mut User<'_>) -> SandboxResult {
    let mut faults = Vec::new();
    user.open_panel()?;
    user.press("escape")?;
    user.press("escape")?;
    user.ask("focus-box")?;
    user.type_text("zq651 words after the panel")?;
    user.press("enter")?;
    if wait_until(WAIT, || {
        user.sent_with("POST", "zq651 words after the panel").into_iter().next()
    })
    .is_none()
    {
        faults.push("text typed after the panel was not sent".to_owned());
    }
    user.ask("focus-box")?;
    user.press("ctrl+.")?;
    user.until("the picker opened by Ctrl+.", picker_open)?;
    user.press("ctrl+tab")?;
    let state = user.until("the Emoji tab", |state| picker(state)["tab"] == "Emoji")?;
    user.image("emoji-tab")?;
    let _ = state;
    user.press("escape")?;
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        user.open_panel()?;
        user.image(&format!("panel-{theme}-saved"))?;
        user.press("tab")?;
        user.press("space")?;
        user.until("Replace showed the token field", |state| {
            panel(state)["asks_token"] == true
        })?;
        // the link is three stops up from the token field, past Remove and "Keep it"
        for _ in 0..3 {
            user.press("shift+tab")?;
        }
        user.retype(GOOD_LINK)?;
        for _ in 0..3 {
            user.press("tab")?;
        }
        let said = user.submit(Some(MALFORMED_TOKEN), "a malformed token")?;
        if said.as_deref() != Some(telegram_import::BAD_TOKEN) {
            faults.push(format!("{theme}: a malformed token gives {said:?}"));
        }
        user.image(&format!("panel-{theme}-replace-problem"))?;
        user.press("escape")?;
        user.press("escape")?;
    }
    if user.kept_token()? != "mock" {
        faults.push("the kept token changed".to_owned());
    }
    faults_to_result(&faults)
}

/// The second run: the token kept by the first is there, and the panel says so without the field.
fn token_kept_after_restart(user: &mut User<'_>) -> SandboxResult {
    let state = user.open_panel()?;
    user.image("restart-panel")?;
    let mut faults = Vec::new();
    if panel(&state)["token_state"] != "Saved" || panel(&state)["asks_token"] != false {
        faults.push(format!("the token after a restart: {}", panel(&state)));
    }
    if user.kept_token()? != "mock" {
        faults.push("the app's keyring lost the token in the restart".to_owned());
    }
    user.press("escape")?;
    let state = user.state()?;
    if pack_named(&state, GOOD_TITLE).is_none() {
        faults.push("the Telegram pack is gone after the restart".to_owned());
    }
    faults_to_result(&faults)
}

// --- the child ---

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
        options: EngineOptions::default(),
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

/// The Bot API the app reaches: the mock's, or, after `bot dead`, a loopback port where nothing listens.
fn bot_apis(origin: Arc<Mutex<String>>) -> BotApis {
    BotApis::new(Arc::new(move |token| {
        let origin = origin.lock().unwrap_or_else(PoisonError::into_inner).clone();
        BotApi::with_mock_origin(token, &origin)
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
    let first = support::started(Alias::A, engine(Alias::A, (ic3_a, skype_a), &dir, false)?)?;
    let second = support::started(Alias::B, engine(Alias::B, (ic3_b, skype_b), &dir, false)?)?;
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
        live: env(BOT_ENV)?,
        dead: env(DEAD_BOT_ENV)?,
        now: Arc::new(Mutex::new(env(BOT_ENV)?)),
        seed,
    };
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        cx.set_global(bot_apis(Arc::clone(&origins.now)));
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

/// The Bot API's origins, the one in use, and the file the kept token goes to at the quit.
struct Origins {
    live: String,
    dead: String,
    now: Arc<Mutex<String>>,
    seed: PathBuf,
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
                // the keyring outlives the app: the token goes where the next run's store finds it
                let kept = cx.update(|cx| BotToken::of(cx).get());
                match kept {
                    Ok(Some(token)) => {
                        let _ = fs::write(&origins.seed, token.expose());
                    }
                    _ => {
                        let _ = fs::remove_file(&origins.seed);
                    }
                }
                say(&format!("re {seq} ok"));
                cx.update(app::quit);
                return;
            }
            let reply = match verb {
                "bot dead" | "bot live" => {
                    let origin = if verb == "bot dead" {
                        &origins.dead
                    } else {
                        &origins.live
                    };
                    origin.clone_into(&mut origins.now.lock().unwrap_or_else(PoisonError::into_inner));
                    "ok".to_owned()
                }
                "kept-token" => match cx.update(|cx| BotToken::of(cx).get()) {
                    Ok(Some(token)) if token.expose() == SYNTHETIC_BOT_TOKEN => "mock".to_owned(),
                    Ok(Some(_)) => "other".to_owned(),
                    Ok(None) => "none".to_owned(),
                    Err(_) => "unreadable".to_owned(),
                },
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
        "telegram": picker.telegram_import().map(|panel| panel_state(panel, window, cx)),
    })
}

fn panel_state(panel: &Entity<TelegramImport>, window: &Window, cx: &App) -> Value {
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
        "focus": panel.holds_focus(window, cx),
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
