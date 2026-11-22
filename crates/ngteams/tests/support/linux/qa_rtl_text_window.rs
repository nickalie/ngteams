//! Arabic, Hebrew and mixed right-to-left and left-to-right text as a user sees it in the app's window in the UI
//! sandbox (`harness = false`: GPUI runs on the process's main thread). It is the QA test of task 699 ("arabic renders
//! wrong": a fragment of a mixed paragraph drew at the far left and the rest was missing).
//!
//! The parent runs the mock (`teams-mock`) with made-up group chats, one message of b's in each, and runs a's app as a
//! child in the sandbox. The parent presses the sandbox's keys and clicks, reads the views from the child, puts text on
//! the sandbox's clipboard and reads it back, and measures each message's text in the screenshots: the newest bubble,
//! its rows of text, where each row starts and ends, and that nothing draws outside it.
//!
//! - **Messages**: the user's case (an English message with an Arabic sentence and English words in quotes, wrapped),
//!   pure Arabic, Hebrew, Arabic with digits and brackets, bold, a link and emoji in Arabic, a list and a code block,
//!   a forward of an Arabic message, a reply to one written in the app. Teams Light and Dark.
//! - **The compose box**: English typed, Arabic pasted, clicks on the Arabic words, Backspace, a selection, then the
//!   message sent.
//! - **A copy**: a word, a drag over mixed text both ways and a whole paragraph, as Ctrl+C copies it.
//! - **The chat list's preview** and **search hits** of the messages.
//! - **Left-to-right text** (English, Russian) and the user's case at each font size, 10 to 24 px.
//!
//! Every name, id and text is made up. Screenshots are kept under `target/ui-shots/qa699-*.png`; `QA699_EVIDENCE`
//! names a directory that gets a copy of each, the measures and the results. Without weston 15, niri or `dbus-daemon`
//! it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, future, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
#[path = "../window.rs"]
mod support;
use serde_json::{Value, json};
use support::{Alias, display_name};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_mock::{
    AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE, PRESENCE_BASE,
    PROVISIONING_BASE, TrouterMock, USER_PROFILE_BASE,
};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::chat_view::{Body, ChatView, Item};
use teams_ui::compose::Mode;
use teams_ui::rich_text::Block;
use teams_ui::shell::{Main, Shell};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::VirtualInput;
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

const CHILD_ENV: &str = "QA699_CHILD";
const ORIGIN_ENV: &str = "QA699_ORIGIN";
const TROUTER_ENV: &str = "QA699_TROUTER";
const TOKEN_ENV: &str = "QA699_TOKEN";
const DIR_ENV: &str = "QA699_DIR";
const EVIDENCE_ENV: &str = "QA699_EVIDENCE";
/// The file in the app's directory the parent asks the child through (`<n> <verb>`; the answer is `re <n> …`).
const ASK: &str = "qa699-ask";
/// The file in the app's directory the child reads the look to show from (`light 14`: the theme and the font size).
const LOOK_FILE: &str = "look";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(40);
const POLL: Duration = Duration::from_millis(100);
const SETTLE: Duration = Duration::from_millis(900);
/// The time between a key and the next, so the app sees each.
const STEP: Duration = Duration::from_millis(120);
/// niri's layout: each window takes the whole screen, and no hot corner.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
const SCREEN: (u32, u32) = (1280, 800);
/// The app's default font size, px.
const DEFAULT_SIZE: u16 = 14;

// --- the made-up text ---

const USER_FIRST: &str = "Each option is kept in a column named after its id. Both of yours use the same wording:";
const USER_ARABIC: &str = "أوافق على تلقي العروض الترويجية المستقبلية والأخبار والعروض والاتصالات من النادي.";
const USER_ENGLISH: &str = "I agree to receive future promotions, news, offers and communications from the club.";
const USER_LAST: &str = "The column is empty when nobody chose it.";
const ARABIC: &str = "مرحبا بكم في الفريق، هل أنتم جاهزون للاجتماع غدا؟ سنبدأ في الساعة العاشرة صباحا ونناقش خطة \
                      العمل للأسبوع القادم وكل المهام المفتوحة.";
const ARABIC_SHORT: &str = "مرحبا بكم في الفريق، هل أنتم جاهزون للاجتماع غدا؟";
const HEBREW: &str = "שלום לכולם, הפגישה מחר בשעה 10. נא להביא את המסמכים ואת הרשימה המעודכנת של כל המשימות הפתוחות \
                      לשבוע הבא.";
const DIGITS: &str = "تم شحن الطلب رقم 123 اليوم (2 قطع) [المخزن 45] والمبلغ 7.50 دولار.";
const ENGLISH: &str = "The quarterly report is ready for review. Please read the summary, check the numbers in the second \
                       table and send your comments before Friday so that we can close it on time.";
const RUSSIAN: &str = "Квартальный отчёт готов к проверке. Прочитайте краткое содержание, проверьте цифры во второй \
                       таблице и пришлите замечания до пятницы, чтобы мы закрыли его вовремя.";
/// The two Arabic words of the copy and compose steps: each joins all its letters, so each draws as one shape.
const HOUSE: &str = "بيت";
const BIG: &str = "كبير";
const SELECT_LINE: &str = "See بيت كبير in notes";
/// What a's reply says after its typed "Yes, ".
const REPLY_ARABIC: &str = "نعم، أنا جاهز تماما.";

/// The user's paragraph as Teams stores it: the Arabic sentence and the English one in quotes.
fn user_line() -> String {
    format!("\"{USER_ARABIC} {USER_ENGLISH}\"")
}

/// How a message's rows must stand in its bubble.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// Each row starts at the bubble's left padding.
    Ltr,
    /// Each row ends at the bubble's right padding.
    Rtl,
    /// Cards, lists or code: only nothing outside the bubble.
    Other,
}

/// A made-up chat with one message of b's.
struct Case {
    key: &'static str,
    topic: &'static str,
    html: String,
    /// Words the message's shown text must hold, each whole.
    words: Vec<String>,
    /// Whether Teams draws it right to left (its first strong letter).
    rtl: bool,
    shape: Shape,
    /// The rows of text it takes at the default size in a 1280 px window, at least.
    rows: usize,
}

fn emoticon(id: &str, alt: &str) -> String {
    format!(
        "<span title=\"{id}\" type=\"({id})\" class=\"animated-emoticon-20-{id}\" itemscope=\"\"><img itemscope=\"\" \
         itemtype=\"http://schema.skype.com/Emoji\" itemid=\"{id}\" \
         src=\"https://statics.teams.cdn.office.net/evergreen-assets/personal-expressions/v2/assets/emoticons/{id}/default/\
         20_f.png\" alt=\"{alt}\" style=\"width:20px; height:20px\"></span>"
    )
}

#[expect(clippy::too_many_lines, reason = "one table of the made-up messages")]
fn cases() -> Vec<Case> {
    let case = |key, topic, html: String, words: &[&str], rtl, shape, rows| Case {
        key,
        topic,
        html,
        words: words.iter().map(|word| (*word).to_owned()).collect(),
        rtl,
        shape,
        rows,
    };
    let user = user_line();
    vec![
        case(
            "compose",
            "Compose desk",
            "<p>Hello from c</p>".to_owned(),
            &["Hello from c"],
            false,
            Shape::Ltr,
            1,
        ),
        case(
            "ltr",
            "Report room",
            format!("<p>{ENGLISH}</p><p>{RUSSIAN}</p>"),
            &[ENGLISH, RUSSIAN],
            false,
            Shape::Ltr,
            4,
        ),
        case(
            "select",
            "Notes corner",
            format!("<p>{SELECT_LINE}</p>"),
            &[SELECT_LINE],
            false,
            Shape::Ltr,
            1,
        ),
        case(
            "list",
            "Task board",
            "<p>قائمة المهام:</p><ul><li>البند الأول: the first item</li><li>البند الثاني</li></ul>\
             <pre><code>// تعليق بالعربية\nlet x = 1;</code></pre>"
                .to_owned(),
            &[
                "قائمة المهام:",
                "البند الأول: the first item",
                "البند الثاني",
                "// تعليق بالعربية",
            ],
            true,
            Shape::Other,
            3,
        ),
        case(
            "forward",
            "Forward desk",
            format!("<blockquote itemtype=\"http://schema.skype.com/Forward\"><p>{ARABIC_SHORT}</p></blockquote>"),
            &[ARABIC_SHORT],
            true,
            Shape::Other,
            1,
        ),
        case(
            "rich",
            "Order desk",
            format!(
                "<p>هذا <b>نص عريض</b> وهذا <a href=\"https://example.com/order\">رابط الطلب</a> وهذا وجه 🙂 في \
                 النص {} هنا.</p>",
                emoticon("smile", "🙂")
            ),
            &["نص عريض", "رابط الطلب", "🙂"],
            true,
            Shape::Rtl,
            1,
        ),
        case(
            "digits",
            "Shipping desk",
            format!("<p>{DIGITS}</p>"),
            &[DIGITS],
            true,
            Shape::Rtl,
            1,
        ),
        case(
            "hebrew",
            "Hebrew desk",
            format!("<p>{HEBREW}</p>"),
            &[HEBREW],
            true,
            Shape::Rtl,
            2,
        ),
        case(
            "user",
            "Options room",
            format!("<p>{USER_FIRST}</p><p>{user}</p><p>{USER_LAST}</p>"),
            &[USER_FIRST, user.as_str(), USER_LAST],
            false,
            Shape::Ltr,
            4,
        ),
        // the newest chat: the top row of the list at the start
        case(
            "arabic",
            "Arabic desk",
            format!("<p>{ARABIC}</p>"),
            &[ARABIC],
            true,
            Shape::Rtl,
            2,
        ),
    ]
}

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_rtl_text_window: {error}");
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
/// What the walk checked and found right, for the results file.
static PASSED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Notes `finding` unless `ok`; else counts `what` as passed.
fn check(what: &str, ok: bool, finding: impl FnOnce() -> String) {
    if ok {
        step(&format!("pass: {what}"));
        PASSED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(what.to_owned());
    } else {
        let finding = format!("{what}: {}", finding());
        step(&format!("FINDING: {finding}"));
        FINDINGS.lock().unwrap_or_else(PoisonError::into_inner).push(finding);
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn evidence_dir() -> Option<PathBuf> {
    std::env::var_os(EVIDENCE_ENV).map(PathBuf::from)
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

/// Adds `text` as a line to `name` in the evidence dir, if any.
fn save_line(name: &str, text: &str) -> SandboxResult {
    if let Some(dir) = evidence_dir() {
        fs::create_dir_all(&dir)?;
        let mut file = fs::OpenOptions::new().create(true).append(true).open(dir.join(name))?;
        writeln!(file, "{text}")?;
    }
    Ok(())
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
    let Some(sandbox) = sandbox_or_skip("ngteams rtl text", &options)? else {
        return Ok(());
    };
    if let Some(dir) = evidence_dir() {
        let _ = fs::remove_file(dir.join("measures.txt"));
    }
    let service = Service::start(&runtime)?;
    let dir = tempfile::tempdir()?;
    let mut run = AppRun::launch(&sandbox, &service.env(), dir.path())?;
    let outcome = Walk {
        run: &mut run,
        sandbox: &sandbox,
        service: &service,
    }
    .all();
    if let Err(error) = outcome {
        return Err(format!("{}{}", run.fail(&sandbox, &*error), sandbox.logs()).into());
    }
    run.quit()?;
    summary()
}

/// Says what passed and what not; an error when anything did not.
fn summary() -> SandboxResult {
    let passed = std::mem::take(&mut *PASSED.lock().unwrap_or_else(PoisonError::into_inner));
    let findings = std::mem::take(&mut *FINDINGS.lock().unwrap_or_else(PoisonError::into_inner));
    let mut summary = String::new();
    for what in &passed {
        let _ = writeln!(summary, "pass {what}");
    }
    for what in &findings {
        let _ = writeln!(summary, "FAIL {what}");
    }
    let _ = writeln!(summary, "{} passed, {} failed", passed.len(), findings.len());
    say(&summary);
    if let Some(dir) = evidence_dir() {
        fs::write(dir.join("results.txt"), &summary)?;
    }
    if !findings.is_empty() {
        return Err(format!("the walk ended with findings:\n- {}", findings.join("\n- ")).into());
    }
    say("ngteams rtl text: ok");
    Ok(())
}

// --- the service ---

struct Service {
    mock: ChatServiceMock,
    trouter: TrouterMock,
    /// Each case's key and chat id.
    chats: Vec<(&'static str, String)>,
}

impl Service {
    /// The mock with a, b and c's made-up group chats, one message of b's in each (c's in the compose desk), posted
    /// before a's app starts.
    fn start(runtime: &tokio::runtime::Runtime) -> SandboxResult<Self> {
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        let mut chats = Vec::new();
        for case in cases() {
            let id = mock
                .add_group(case.topic, &[LAB_C, LAB_A, LAB_B])
                .ok_or_else(|| format!("no group {}", case.topic))?;
            let from = if case.key == "compose" { LAB_C } else { LAB_B };
            mock.post_as(from, &id, &case.html)
                .ok_or_else(|| format!("the post to {} refused", case.topic))?;
            chats.push((case.key, id));
        }
        let _ = mock.take_events(LAB_A);
        Ok(Self { mock, trouter, chats })
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            (ORIGIN_ENV, self.mock.origin()),
            (TROUTER_ENV, self.trouter.origin().to_owned()),
            (TOKEN_ENV, self.mock.token(LAB_A).unwrap_or_default()),
        ]
    }

    fn chat(&self, key: &str) -> String {
        self.chats
            .iter()
            .find(|(case, _)| *case == key)
            .map(|(_, id)| id.clone())
            .unwrap_or_default()
    }
}

// --- the app in the sandbox ---

/// The app running in the sandbox: its process, its directory and the user's keys and pointer.
struct AppRun {
    running: Running,
    dir: PathBuf,
    input: VirtualInput,
    asked: u32,
    /// The look last applied (`light 14`).
    shown: String,
}

impl AppRun {
    /// Starts the app with `env` and its store and settings in `dir`; once its window is drawn.
    fn launch(sandbox: &Sandbox, env: &[(&'static str, String)], dir: &Path) -> SandboxResult<Self> {
        let dir_text = dir.to_string_lossy().into_owned();
        let mut vars: Vec<(&str, &str)> = vec![(CHILD_ENV, "app"), (DIR_ENV, &dir_text)];
        vars.extend(env.iter().map(|(name, value)| (*name, value.as_str())));
        let child = sandbox.spawn(&std::env::current_exe()?, &[], &vars)?;
        let running = Running::new(child)?;
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| format!("no ready line: {error:?}"))?;
        let mut run = Self {
            running,
            dir: dir.to_owned(),
            input: sandbox.input()?,
            asked: 0,
            shown: String::new(),
        };
        run.park()?;
        Ok(run)
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

    fn ok(&mut self, verb: &str) -> SandboxResult {
        let answer = self.question(verb)?;
        if answer == "ok" {
            Ok(())
        } else {
            Err(format!("{verb}: {answer}").into())
        }
    }

    fn json(&mut self, verb: &str) -> SandboxResult<Value> {
        let answer = self.question(verb)?;
        serde_json::from_str(&answer).map_err(|error| format!("{verb} {answer}: {error}").into())
    }

    /// Waits up to [`WAIT`] for an answer to `verb` that `wanted` takes, named `what` in the error.
    fn until(&mut self, verb: &str, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let start = Instant::now();
        loop {
            let state = self.json(verb)?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {state}").into());
            }
            thread::sleep(POLL);
        }
    }

    /// Shows the app in Teams Light or Dark at font size `size`, as the user picks them in Settings; once applied.
    fn look(&mut self, dark: bool, size: u16) -> SandboxResult {
        let wanted = format!("{} {size}", if dark { "dark" } else { "light" });
        if wanted == self.shown {
            return Ok(());
        }
        fs::write(self.dir.join(LOOK_FILE), &wanted)?;
        let applied = format!("look {wanted}");
        self.running
            .line_where(WAIT, |line| line == applied)
            .map_err(|error| format!("{applied} never applied: {error:?}"))?;
        self.shown = wanted;
        Ok(())
    }

    fn park(&mut self) -> SandboxResult {
        self.input.move_to(640, 12)
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

    /// `count` clicks of the left button at (`x`, `y`), as fast as a double or triple click.
    fn clicks(&mut self, (x, y): (u32, u32), count: u32) -> SandboxResult {
        self.input.move_to(x, y)?;
        thread::sleep(STEP);
        for _ in 0..count {
            self.input.click(x, y, BUTTON_LEFT)?;
            thread::sleep(Duration::from_millis(40));
        }
        thread::sleep(STEP);
        Ok(())
    }

    /// A drag with the left button from `from` to `to`, in steps, as a hand does it.
    fn drag(&mut self, from: (u32, u32), to: (u32, u32)) -> SandboxResult {
        self.input.move_to(from.0, from.1)?;
        thread::sleep(STEP);
        self.input.button_down(BUTTON_LEFT)?;
        thread::sleep(STEP);
        for part in 1..=8_i64 {
            let along = |start: u32, end: u32| {
                let (start, end) = (i64::from(start), i64::from(end));
                u32::try_from(start + (end - start) * part / 8).unwrap_or(0)
            };
            self.input.move_to(along(from.0, to.0), along(from.1, to.1))?;
            thread::sleep(Duration::from_millis(30));
        }
        self.input.button_up(BUTTON_LEFT)?;
        thread::sleep(STEP);
        Ok(())
    }

    /// A screenshot as `qa699-<name>.png`, the pointer at rest, under `target/ui-shots/` and the evidence dir.
    fn shot(&mut self, sandbox: &Sandbox, name: &str) -> SandboxResult<PathBuf> {
        self.park()?;
        thread::sleep(SETTLE);
        Self::shot_here(sandbox, name)
    }

    /// A screenshot with the pointer where it is.
    fn shot_here(sandbox: &Sandbox, name: &str) -> SandboxResult<PathBuf> {
        let file = format!("qa699-{name}.png");
        let png = sandbox.screenshot(&file, false)?;
        let kept = shots_dir()?.join(&file);
        fs::copy(&png, &kept)?;
        if let Some(dir) = evidence_dir() {
            let dir = dir.join("shots");
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(&file))?;
        }
        Ok(kept)
    }

    /// The error, with the last screen and the app's stderr.
    fn fail(&mut self, sandbox: &Sandbox, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
        let shot = self
            .shot(sandbox, "failed-run")
            .map_or_else(|_| String::new(), |path| path.display().to_string());
        self.running.fail(&format!("{error} (last screen: {shot})"))
    }

    /// Quits the app; once it ended.
    fn quit(mut self) -> SandboxResult {
        self.asked += 1;
        let staged = self.dir.join(format!("{ASK}.staged"));
        fs::write(&staged, format!("{} quit", self.asked))?;
        fs::rename(&staged, self.dir.join(ASK))?;
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("the app exited {status}: {stderr}").into());
        }
        Ok(())
    }
}

/// The left button's evdev code.
const BUTTON_LEFT: u32 = 0x110;

// --- what the screenshots show ---

type Rgb = [u8; 3];

/// A rectangle of pixels: `left..right` by `top..bottom`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
}

impl Rect {
    fn width(&self) -> u32 {
        self.right.saturating_sub(self.left)
    }

    fn middle(&self) -> (u32, u32) {
        (
            u32::midpoint(self.left, self.right),
            u32::midpoint(self.top, self.bottom),
        )
    }
    fn holds(&self, x: u32, y: u32) -> bool {
        (self.left..self.right).contains(&x) && (self.top..self.bottom).contains(&y)
    }
}

fn pixel(shot: &image::RgbImage, x: u32, y: u32) -> Rgb {
    shot.get_pixel(x, y).0
}

fn distance(one: Rgb, other: Rgb) -> u32 {
    one.iter().zip(other).map(|(a, b)| u32::from(a.abs_diff(b))).sum()
}

/// How far from the background colour a pixel is to count as drawn ink.
const INK: u32 = 150;
/// The message list's part of the chat pane, above the compose box at every font size.
const MESSAGES: Rect = Rect {
    left: 456,
    top: 100,
    right: 1268,
    bottom: 716,
};

/// The colours of `area` by how many pixels show each, most first.
fn colours(shot: &image::RgbImage, area: Rect) -> Vec<(Rgb, u32)> {
    let mut counts: std::collections::HashMap<Rgb, u32> = std::collections::HashMap::new();
    for y in area.top..area.bottom.min(shot.height()) {
        for x in area.left..area.right.min(shot.width()) {
            *counts.entry(pixel(shot, x, y)).or_default() += 1;
        }
    }
    let mut sorted: Vec<(Rgb, u32)> = counts.into_iter().collect();
    sorted.sort_by_key(|&(_, count)| std::cmp::Reverse(count));
    sorted
}

/// The blocks of rows of `area` where at least 8 pixels show `colour`, each as the box of those pixels.
fn blocks(shot: &image::RgbImage, area: Rect, colour: Rgb) -> Vec<Rect> {
    let mut found: Vec<Rect> = Vec::new();
    for y in area.top..area.bottom {
        let xs: Vec<u32> = (area.left..area.right)
            .filter(|&x| distance(pixel(shot, x, y), colour) <= 6)
            .collect();
        let (Some(&left), Some(&right)) = (xs.first(), xs.last()) else {
            continue;
        };
        if xs.len() < 8 {
            continue;
        }
        match found.last_mut() {
            Some(block) if block.bottom + 1 >= y => {
                block.bottom = y + 1;
                block.left = block.left.min(left);
                block.right = block.right.max(right + 1);
            }
            _ => found.push(Rect {
                left,
                top: y,
                right: right + 1,
                bottom: y + 1,
            }),
        }
    }
    found
}

/// Whether at least half of `block` shows `colour`: a bubble, not a shadow's or a gradient's thin line.
fn solid(shot: &image::RgbImage, block: Rect, colour: Rgb) -> bool {
    let shown = (block.top..block.bottom)
        .flat_map(|y| (block.left..block.right).map(move |x| (x, y)))
        .filter(|&(x, y)| distance(pixel(shot, x, y), colour) <= 6)
        .count();
    let area = u64::from(block.width()) * u64::from(block.bottom - block.top);
    u64::try_from(shown).unwrap_or(u64::MAX) * 2 >= area
}

/// The pane's background at its top left corner, where nothing draws (a gradient: other places differ a little).
fn pane_background(shot: &image::RgbImage) -> Rgb {
    pixel(shot, MESSAGES.left + 4, MESSAGES.top + 4)
}

/// The newest message's bubble: of the commonest colours after the background, the block lowest in the list that is
/// not the compose box (which spans the pane from its left edge); with its colour.
fn newest_bubble(shot: &image::RgbImage) -> Option<(Rect, Rgb)> {
    let background = pane_background(shot);
    colours(shot, MESSAGES)
        .iter()
        .filter(|(colour, _)| distance(*colour, background) > 24)
        .take(8)
        .filter_map(|(colour, _)| {
            blocks(shot, MESSAGES, *colour)
                .into_iter()
                .rfind(|block| {
                    block.left > MESSAGES.left + 30
                        && block.width() > 30
                        && block.bottom - block.top > 20
                        && solid(shot, *block, *colour)
                })
                .map(|block| (block, *colour))
        })
        .max_by_key(|(block, _)| block.bottom)
}

/// The rows of ink of `area` (pixels `ink` takes), the gaps of at most `gap` rows joined; each row's box.
fn ink_rows(shot: &image::RgbImage, area: Rect, gap: u32, ink: impl Fn(Rgb) -> bool) -> Vec<Rect> {
    let mut rows: Vec<Rect> = Vec::new();
    for y in area.top..area.bottom {
        let xs: Vec<u32> = (area.left..area.right).filter(|&x| ink(pixel(shot, x, y))).collect();
        let (Some(&left), Some(&right)) = (xs.first(), xs.last()) else {
            continue;
        };
        match rows.last_mut() {
            Some(row) if row.bottom + gap >= y => {
                row.bottom = y + 1;
                row.left = row.left.min(left);
                row.right = row.right.max(right + 1);
            }
            _ => rows.push(Rect {
                left,
                top: y,
                right: right + 1,
                bottom: y + 1,
            }),
        }
    }
    rows.retain(|row| row.bottom - row.top >= 5);
    rows
}

/// The runs of ink across one row (`row`), split where at least `gap` columns hold none: the words as drawn.
fn ink_words(shot: &image::RgbImage, row: Rect, gap: u32, ink: impl Fn(Rgb) -> bool) -> Vec<Rect> {
    let mut words: Vec<Rect> = Vec::new();
    for x in row.left..row.right {
        if !(row.top..row.bottom).any(|y| ink(pixel(shot, x, y))) {
            continue;
        }
        match words.last_mut() {
            Some(word) if word.right + gap > x => word.right = x + 1,
            _ => words.push(Rect {
                left: x,
                top: row.top,
                right: x + 1,
                bottom: row.bottom,
            }),
        }
    }
    words
}

/// A message as drawn: its bubble, its rows of text, and the ink drawn beside the bubble where nothing may be.
#[derive(Debug)]
struct Drawn {
    bubble: Rect,
    rows: Vec<Rect>,
    /// Ink pixels beside the bubble over its full height, left of it (its avatar left out) and right of another's.
    outside: u32,
}

/// The newest message as `png` shows it.
fn drawn(png: &Path) -> SandboxResult<Drawn> {
    let shot = image::open(png)?.to_rgb8();
    let (bubble, colour) = newest_bubble(&shot).ok_or("no message bubble in the shot")?;
    let inside = Rect {
        left: bubble.left + 2,
        top: bubble.top + 2,
        right: bubble.right.saturating_sub(2),
        bottom: bubble.bottom.saturating_sub(2),
    };
    let rows = ink_rows(&shot, inside, 2, |seen| distance(seen, colour) > INK);
    let background = pane_background(&shot);
    let away = |seen: Rgb| distance(seen, background) > INK;
    let avatar = avatar_beside(bubble);
    let mut outside = 0;
    for y in bubble.top..bubble.bottom {
        outside += (MESSAGES.left..bubble.left.saturating_sub(2))
            .filter(|&x| !avatar.holds(x, y) && away(pixel(&shot, x, y)))
            .count();
        if bubble.left < 600 {
            outside += ((bubble.right + 2)..MESSAGES.right)
                .filter(|&x| away(pixel(&shot, x, y)))
                .count();
        }
    }
    Ok(Drawn {
        bubble,
        rows,
        outside: u32::try_from(outside).unwrap_or(u32::MAX),
    })
}

/// Where another's avatar may draw beside `bubble`: level with its top, a gap left of it, at each font size.
fn avatar_beside(bubble: Rect) -> Rect {
    Rect {
        left: bubble.left.saturating_sub(48),
        top: bubble.top.saturating_sub(4),
        right: bubble.left.saturating_sub(8),
        bottom: bubble.top + 40,
    }
}

/// The left half of a group chat's avatar tile in the chat list, in Teams Light and Dark.
const TILE: [Rgb; 2] = [[217, 236, 212], [36, 64, 42]];

/// The first group chat's row in the chat list (the Arabic chat's, the newest; avatar and badge left out): its name
/// and preview lines, level with its avatar tile.
fn arabic_row(shot: &image::RgbImage) -> Option<Rect> {
    let tile = |y: u32| TILE.iter().any(|colour| distance(pixel(shot, 92, y), *colour) <= 10);
    let top = (100..700).find(|&y| tile(y))?;
    let bottom = (top..700).find(|&y| !tile(y))?;
    Some(Rect {
        left: 140,
        top: top.saturating_sub(4),
        right: 420,
        bottom: bottom + 4,
    })
}

/// The compose box's text when it holds one line, left of its buttons.
const COMPOSE_LINE: Rect = Rect {
    left: 470,
    top: 728,
    right: 1010,
    bottom: 772,
};

// --- the walk ---

struct Walk<'a> {
    run: &'a mut AppRun,
    sandbox: &'a Sandbox,
    service: &'a Service,
}

fn items(state: &Value) -> Vec<Value> {
    state["items"].as_array().cloned().unwrap_or_default()
}

fn last_item(state: &Value) -> Value {
    items(state).last().cloned().unwrap_or(Value::Null)
}

fn text_of(item: &Value) -> String {
    item["text"].as_str().unwrap_or_default().to_owned()
}

/// `text` with every run of white space one space.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether each part of `snippet` between its ellipses is a part of `text`, in the same order of letters.
fn in_order(snippet: &str, text: &str) -> bool {
    let text = flat(text);
    snippet
        .split(['…'])
        .map(|part| flat(part.trim_matches('.')))
        .filter(|part| !part.is_empty())
        .all(|part| text.contains(&part))
}

impl Walk<'_> {
    fn all(&mut self) -> SandboxResult {
        self.run.look(false, DEFAULT_SIZE)?;
        self.list_at_start()?;
        for case in cases().iter().filter(|case| case.key != "compose") {
            self.message(case)?;
        }
        self.font_sizes()?;
        self.copy()?;
        self.compose()?;
        self.reply()?;
        self.search()
    }

    /// Opens the chat of case `key`; once its newest message shows.
    fn open(&mut self, key: &str) -> SandboxResult<Value> {
        let id = self.service.chat(key);
        self.run.ok(&format!("open {id}"))?;
        self.run.until("chat", &format!("the {key} chat open"), |state| {
            state["conversation"] == id.as_str() && !items(state).is_empty()
        })
    }

    /// Measures the newest message in `png` as `what`: nothing beside its bubble, at least `rows` rows of text, each
    /// row at the side `shape` says.
    fn measure(png: &Path, what: &str, shape: Shape, rows: usize) -> SandboxResult {
        let drawn = drawn(png)?;
        save_line("measures.txt", &format!("{what}: {drawn:?}"))?;
        check(
            &format!("{what}: no text draws beside the bubble"),
            drawn.outside == 0,
            || format!("{} ink pixels beside {:?}", drawn.outside, drawn.bubble),
        );
        check(
            &format!("{what}: the text shows in {rows} rows or more"),
            drawn.rows.len() >= rows,
            || format!("rows {:?}", drawn.rows),
        );
        let Some(first) = drawn.rows.first() else {
            return Ok(());
        };
        match shape {
            Shape::Ltr => check(
                &format!("{what}: each row starts at the bubble's left padding"),
                drawn.rows.iter().all(|row| row.left.abs_diff(first.left) <= 4)
                    && first.left.saturating_sub(drawn.bubble.left) <= 40,
                || format!("bubble {:?}, rows {:?}", drawn.bubble, drawn.rows),
            ),
            Shape::Rtl => {
                let right = drawn.rows.iter().map(|row| row.right).max().unwrap_or_default();
                check(
                    &format!("{what}: each row ends at the bubble's right padding"),
                    drawn.rows.iter().all(|row| row.right.abs_diff(right) <= 4)
                        && drawn.bubble.right.saturating_sub(right) <= 40,
                    || format!("bubble {:?}, rows {:?}", drawn.bubble, drawn.rows),
                );
            }
            Shape::Other => {}
        }
        Ok(())
    }

    /// A case's message: its words and direction as the view holds them, then as drawn in Light and Dark.
    fn message(&mut self, case: &Case) -> SandboxResult {
        let state = self.open(case.key)?;
        let item = last_item(&state);
        let text = text_of(&item);
        for word in &case.words {
            check(
                &format!("{}: the message shows {word:?} whole", case.key),
                text.contains(word.as_str()),
                || format!("it shows {text:?}"),
            );
        }
        check(
            &format!(
                "{}: the message is drawn {}",
                case.key,
                if case.rtl { "right to left" } else { "left to right" }
            ),
            item["rtl"].as_bool() == Some(case.rtl),
            || format!("rtl is {}", item["rtl"]),
        );
        for (dark, theme) in [(false, "light"), (true, "dark")] {
            self.run.look(dark, DEFAULT_SIZE)?;
            let png = self.run.shot(self.sandbox, &format!("{}-{theme}", case.key))?;
            Self::measure(&png, &format!("{} {theme}", case.key), case.shape, case.rows)?;
        }
        self.run.look(false, DEFAULT_SIZE)
    }

    /// English and Russian, and the user's case, at each font size from 10 to 24 px.
    fn font_sizes(&mut self) -> SandboxResult {
        for case in cases().iter().filter(|case| ["ltr", "user"].contains(&case.key)) {
            self.open(case.key)?;
            for size in teams_ui::theme::FONT_SIZES {
                self.run.look(false, size)?;
                let png = self.run.shot(self.sandbox, &format!("{}-size-{size}", case.key))?;
                Self::measure(&png, &format!("{} at {size} px", case.key), case.shape, case.rows)?;
            }
        }
        self.run.look(false, DEFAULT_SIZE)
    }

    /// What the sandbox's clipboard holds as text once it is `wanted`, or after a while what it holds then.
    fn clipboard_text(&self, wanted: &str) -> String {
        let read = || {
            self.sandbox
                .read_clipboard()
                .ok()?
                .into_iter()
                .find(|(kind, _)| kind.starts_with("text/plain"))
                .map(|(_, bytes)| String::from_utf8_lossy(&bytes).into_owned())
        };
        wait_until(Duration::from_secs(3), || read().filter(|text| text == wanted))
            .or_else(read)
            .unwrap_or_default()
    }

    fn paste(&mut self, text: &str) -> SandboxResult {
        self.sandbox
            .set_clipboard(vec![("text/plain;charset=utf-8".to_owned(), text.as_bytes().to_vec())])?;
        thread::sleep(STEP);
        self.run.press("ctrl+v")
    }
}

/// The ink of `shot` against the colour `under`.
fn ink_against(under: Rgb) -> impl Fn(Rgb) -> bool {
    move |seen| distance(seen, under) > INK
}

impl Walk<'_> {
    /// Each chat's row previews its message whole and in the order of its letters; the top row (the Arabic message,
    /// the newest) draws its preview from the left edge of its text, as Teams does, filling the row.
    fn list_at_start(&mut self) -> SandboxResult {
        let wanted = [
            ("arabic", ARABIC.to_owned()),
            ("hebrew", HEBREW.to_owned()),
            ("digits", DIGITS.to_owned()),
            ("user", format!("{USER_FIRST} {} {USER_LAST}", user_line())),
        ];
        let ids: Vec<(&str, String)> = wanted.iter().map(|(key, _)| (*key, self.service.chat(key))).collect();
        let rows = self
            .run
            .until("rows", "every row previews its message", |rows| {
                ids.iter().all(|(_, id)| !preview_of(rows, id).is_empty())
            })
            .or_else(|_| self.run.json("rows"))?;
        save_line("rows.json", &rows.to_string())?;
        for ((key, expected), (_, id)) in wanted.iter().zip(&ids) {
            let preview = preview_of(&rows, id);
            check(
                &format!("the {key} chat's row previews its message from its start, in the order of its letters"),
                // the list cuts a long preview after its first letters, with an ellipsis
                flat(preview.trim_end_matches('…')).chars().count() >= 40
                    && flat(expected).starts_with(&flat(preview.trim_end_matches('…'))),
                || format!("it previews {preview:?}"),
            );
        }
        let chats: Vec<String> = self.service.chats.iter().map(|(_, id)| id.clone()).collect();
        let top = rows
            .as_array()
            .and_then(|rows| rows.iter().find(|row| chats.iter().any(|id| row["id"] == id.as_str())))
            .map(|row| row["id"].clone());
        check(
            "the Arabic chat is the top chat row",
            top.as_ref().and_then(Value::as_str) == Some(self.service.chat("arabic").as_str()),
            || format!("the top row is {top:?}"),
        );
        for (dark, theme) in [(false, "light"), (true, "dark")] {
            self.run.look(dark, DEFAULT_SIZE)?;
            let png = self.run.shot(self.sandbox, &format!("chat-list-{theme}"))?;
            top_row(&png, theme)?;
        }
        self.run.look(false, DEFAULT_SIZE)
    }

    /// The words of the select chat's line as drawn, in the order they show from the left.
    fn select_words(&mut self) -> SandboxResult<Vec<Rect>> {
        self.open("select")?;
        let png = self.run.shot(self.sandbox, "select-light")?;
        let shot = image::open(&png)?.to_rgb8();
        let (bubble, colour) = newest_bubble(&shot).ok_or("no bubble in the select chat")?;
        let rows = ink_rows(&shot, bubble, 2, ink_against(colour));
        let row = rows.first().copied().ok_or("no row of text in the select chat")?;
        let words = ink_words(&shot, row, 3, ink_against(colour));
        save_line("measures.txt", &format!("select words: {words:?}"))?;
        Ok(words)
    }

    /// Clicks `count` times at `at`, presses Ctrl+C and checks the clipboard holds `wanted` (`what`).
    fn copied(&mut self, at: (u32, u32), count: u32, wanted: &str, what: &str) -> SandboxResult<String> {
        self.run.clicks(at, count)?;
        self.run.press("ctrl+c")?;
        let copied = self.clipboard_text(wanted);
        check(what, copied == wanted, || format!("the clipboard holds {copied:?}"));
        Ok(copied)
    }

    /// A drag from `from` to `to`, then Ctrl+C: what the clipboard holds.
    fn dragged(&mut self, from: (u32, u32), to: (u32, u32), name: &str) -> SandboxResult<String> {
        self.run.drag(from, to)?;
        AppRun::shot_here(self.sandbox, name)?;
        self.run.press("ctrl+c")?;
        Ok(self.clipboard_text(""))
    }

    /// Double-clicks, drags and a triple-click over mixed text copy the text selected, in the order of its letters.
    fn copy(&mut self) -> SandboxResult {
        let words = self.select_words()?;
        // a click on the empty list first, as a hand puts the window's focus on the messages
        self.run.clicks((MESSAGES.left + 200, MESSAGES.top + 20), 1)?;
        // "See بيت كبير in notes" shows as See, كبير, بيت, in, notes from the left
        check("copy: the mixed line draws as five words", words.len() == 5, || {
            format!("words {words:?}")
        });
        if words.len() == 5 {
            self.copy_words(&words)?;
        }
        self.run.clicks((MESSAGES.left + 8, MESSAGES.top + 4), 1)?;
        self.open("user")?;
        let png = self.run.shot(self.sandbox, "user-copy-light")?;
        let drawn = drawn(&png)?;
        let Some(line) = drawn.rows.get(1).copied() else {
            check("copy: the user's paragraph is drawn", false, || format!("{drawn:?}"));
            return Ok(());
        };
        let user = user_line();
        self.copied(
            (line.left + 40, line.middle().1),
            3,
            &user,
            "copy: a triple-click on the user's mixed paragraph copies it whole",
        )?;
        self.run.shot(self.sandbox, "user-copy-selected-light")?;
        if let Some(last) = drawn.rows.last().copied() {
            self.copied(
                (last.left + 40, last.middle().1),
                3,
                USER_LAST,
                "copy: a triple-click on the user's English last paragraph copies it whole (a control)",
            )?;
            self.run.shot(self.sandbox, "user-copy-english-light")?;
        }
        Ok(())
    }

    fn copy_words(&mut self, words: &[Rect]) -> SandboxResult {
        self.copied(
            words[1].middle(),
            2,
            BIG,
            "copy: a double-click on the second word drawn copies كبير",
        )?;
        self.copied(
            words[2].middle(),
            2,
            HOUSE,
            "copy: a double-click on the third word drawn copies بيت",
        )?;
        // from the middle of كبير to the middle of "in": the end of كبير, a space and the start of "in"
        let copied = self.dragged(words[1].middle(), words[3].middle(), "select-drag-right-light")?;
        let parts: Vec<&str> = copied.split(' ').collect();
        check(
            "copy: a drag from كبير to \"in\" copies the end of كبير and the start of \"in\"",
            parts.len() == 2 && !parts[0].is_empty() && BIG.ends_with(parts[0]) && "in".starts_with(parts[1]),
            || format!("the clipboard holds {copied:?}"),
        );
        // from the middle of بيت leftwards to the middle of كبير: the end of بيت, a space and the start of كبير
        let copied = self.dragged(words[2].middle(), words[1].middle(), "select-drag-left-light")?;
        let parts: Vec<&str> = copied.split(' ').collect();
        check(
            "copy: a drag from بيت leftwards to كبير copies the end of بيت and the start of كبير",
            parts.len() == 2
                && !parts[0].is_empty()
                && !parts[1].is_empty()
                && HOUSE.ends_with(parts[0])
                && BIG.starts_with(parts[1]),
            || format!("the clipboard holds {copied:?}"),
        );
        Ok(())
    }
}

fn preview_of(rows: &Value, chat: &str) -> String {
    rows.as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == chat))
        .and_then(|row| row["preview"].as_str())
        .unwrap_or_default()
        .to_owned()
}

/// The top row of the list: its name line, then its preview line from the same left edge, long enough to fill it.
fn top_row(png: &Path, theme: &str) -> SandboxResult {
    let shot = image::open(png)?.to_rgb8();
    let row = arabic_row(&shot).ok_or("no group chat's tile in the list")?;
    let under = colours(&shot, row).first().map_or([0; 3], |(colour, _)| *colour);
    let rows = ink_rows(&shot, row, 1, ink_against(under));
    save_line("measures.txt", &format!("top row {theme}: {rows:?}"))?;
    let [name, preview] = rows[..] else {
        check(
            &format!("list {theme}: the top row shows a name and a preview"),
            false,
            || format!("rows {rows:?}"),
        );
        return Ok(());
    };
    check(
        &format!("list {theme}: the Arabic preview starts where the name starts and fills the row"),
        preview.left.abs_diff(name.left) <= 4 && preview.width() >= 200,
        || format!("name {name:?}, preview {preview:?}"),
    );
    let side = ellipsis_side(&shot, preview, under);
    check(
        &format!("list {theme}: the cut Arabic preview ends in its \"...\" at the right, as a left-to-right line does"),
        side == Some("right"),
        || format!("the \"...\" is at {side:?}"),
    );
    Ok(())
}

/// Which end of `row` draws three dots on one baseline (a cut line's "..."): `left`, `right` or none.
fn ellipsis_side(shot: &image::RgbImage, row: Rect, under: Rgb) -> Option<&'static str> {
    let ink = ink_against(under);
    let blobs: Vec<Rect> = ink_words(shot, row, 1, &ink)
        .into_iter()
        .map(|word| {
            let ys: Vec<u32> = (word.top..word.bottom)
                .filter(|&y| (word.left..word.right).any(|x| ink(pixel(shot, x, y))))
                .collect();
            Rect {
                top: ys.first().copied().unwrap_or(word.top),
                bottom: ys.last().map_or(word.bottom, |y| y + 1),
                ..word
            }
        })
        .collect();
    let dots = |three: &[Rect]| {
        three.len() == 3
            && three
                .iter()
                .all(|blob| blob.width() <= 4 && blob.bottom - blob.top <= 4 && blob.top.abs_diff(three[0].top) <= 1)
    };
    if dots(blobs.get(blobs.len().saturating_sub(3)..).unwrap_or_default()) {
        Some("right")
    } else if dots(blobs.get(..3).unwrap_or_default()) {
        Some("left")
    } else {
        None
    }
}

/// The compose box's text as the view holds it, and its selection's ends (block 0 offsets).
fn box_text(state: &Value) -> (String, u64, u64) {
    let boxed = &state["box"];
    (
        boxed["text"].as_str().unwrap_or_default().to_owned(),
        boxed["anchor"].as_u64().unwrap_or(u64::MAX),
        boxed["head"].as_u64().unwrap_or(u64::MAX),
    )
}

fn byte_range(text: &str, word: &str) -> std::ops::RangeInclusive<u64> {
    let start = text.find(word).unwrap_or_default();
    let end = start + word.len();
    u64::try_from(start).unwrap_or_default()..=u64::try_from(end).unwrap_or_default()
}

impl Walk<'_> {
    /// The box after `what`: once its text is `wanted`.
    fn box_until(&mut self, wanted: &str, what: &str) -> SandboxResult<Value> {
        let state = self
            .run
            .until("chat", what, |state| box_text(state).0 == wanted)
            .or_else(|_| self.run.json("chat"))?;
        let (text, _, _) = box_text(&state);
        check(what, text == wanted, || format!("the box holds {text:?}"));
        Ok(state)
    }

    /// The user types English, pastes Arabic, clicks on the Arabic words, deletes with Backspace, selects with
    /// Shift+Home, pastes the user's paragraph over all and sends it.
    fn compose(&mut self) -> SandboxResult {
        self.open("compose")?;
        self.run.ok("focus-box")?;
        self.run.type_text("Hello ")?;
        self.paste(&format!("{HOUSE} {BIG}"))?;
        self.run.type_text(" world")?;
        let typed = format!("Hello {HOUSE} {BIG} world");
        let state = self.box_until(&typed, "compose: typed and pasted text stands in the box in its order")?;
        let (_, _, head) = box_text(&state);
        check(
            "compose: the caret is at the end after the typing",
            head == u64::try_from(typed.len()).unwrap_or_default(),
            || format!("the caret is at {head}"),
        );
        let png = self.run.shot(self.sandbox, "compose-typed-light")?;
        self.compose_clicks(&png, &typed)?;
        self.run.press("end")?;
        for _ in 0.." world".chars().count() {
            self.run.press("backspace")?;
        }
        self.box_until(
            &format!("Hello {HOUSE} {BIG}"),
            "compose: Backspace deletes \" world\" at the end",
        )?;
        for _ in 0..BIG.chars().count() {
            self.run.press("backspace")?;
        }
        let left = format!("Hello {HOUSE} ");
        self.box_until(
            &left,
            "compose: Backspace deletes the Arabic word before the caret, letter by letter",
        )?;
        self.run.press("shift+home")?;
        let state = self.run.json("chat")?;
        let selected = state["box"]["selected"].as_str().unwrap_or_default().to_owned();
        check(
            "compose: Shift+Home selects the whole mixed line",
            selected == left,
            || format!("it selects {selected:?}"),
        );
        self.run.shot(self.sandbox, "compose-selected-light")?;
        self.run.press("ctrl+a")?;
        let user = user_line();
        self.paste(&user)?;
        self.box_until(&user, "compose: the user's paragraph pasted over all stands in the box")?;
        self.run.shot(self.sandbox, "compose-pasted-light")?;
        self.run.press("enter")?;
        self.sent(&user)
    }

    /// Clicks on the two Arabic words of the typed line: the caret goes into the word clicked.
    fn compose_clicks(&mut self, png: &Path, typed: &str) -> SandboxResult {
        let shot = image::open(png)?.to_rgb8();
        let under = colours(&shot, COMPOSE_LINE)
            .first()
            .map_or([0; 3], |(colour, _)| *colour);
        let rows = ink_rows(&shot, COMPOSE_LINE, 2, ink_against(under));
        let Some(row) = rows.iter().copied().max_by_key(Rect::width) else {
            check("compose: the typed line is drawn", false, || format!("rows {rows:?}"));
            return Ok(());
        };
        let words = ink_words(&shot, row, 3, ink_against(under));
        save_line("measures.txt", &format!("compose words: {words:?}"))?;
        // "Hello بيت كبير world" shows as Hello, كبير, بيت, world from the left
        if words.len() < 4 {
            check("compose: the typed line draws as four words", false, || {
                format!("words {words:?}")
            });
            return Ok(());
        }
        for (word, at, name) in [(BIG, words[1], "second"), (HOUSE, words[2], "third")] {
            self.run.clicks(at.middle(), 1)?;
            let (_, _, head) = box_text(&self.run.json("chat")?);
            let range = byte_range(typed, word);
            check(
                &format!("compose: a click on the {name} word drawn puts the caret in {word}"),
                range.contains(&head),
                || format!("the caret is at byte {head}, {word} is {range:?}"),
            );
            AppRun::shot_here(self.sandbox, &format!("compose-click-{name}-light"))?;
        }
        Ok(())
    }

    /// a's sent message shows the same text, drawn left to right in Light and Dark.
    fn sent(&mut self, user: &str) -> SandboxResult {
        let state = self
            .run
            .until("chat", "the sent message", |state| {
                text_of(&last_item(state)).contains(USER_ARABIC) && box_text(state).0.is_empty()
            })
            .or_else(|_| self.run.json("chat"))?;
        let text = text_of(&last_item(&state));
        check("compose: the sent message shows the text sent", text == user, || {
            format!("it shows {text:?}")
        });
        let rtl = last_item(&state)["rtl"].clone();
        check(
            "compose: the sent message, Arabic first, is drawn right to left",
            rtl == true,
            || format!("rtl is {rtl}"),
        );
        // a's own bubble is a gradient near the pane's colour: the shots are read by eye, not measured
        for (dark, theme) in [(false, "light"), (true, "dark")] {
            self.run.look(dark, DEFAULT_SIZE)?;
            self.run.shot(self.sandbox, &format!("compose-sent-{theme}"))?;
        }
        self.run.look(false, DEFAULT_SIZE)
    }

    /// a replies to b's Arabic message from the keys (Escape, Up, R), typed English and pasted Arabic: the quote shows
    /// b's text, the reply the text sent.
    fn reply(&mut self) -> SandboxResult {
        let state = self.open("arabic")?;
        let id = last_item(&state)["id"].as_str().unwrap_or_default().to_owned();
        self.run.ok("focus-box")?;
        self.run.press("escape")?;
        let mut selected = false;
        for _ in 0..8 {
            if self.run.json("chat")?["selected"] == id.as_str() {
                selected = true;
                break;
            }
            self.run.press("up")?;
        }
        if !selected {
            return Err("reply: b's message was never selected".into());
        }
        self.run.type_text("r")?;
        let state = self.run.until("chat", "the reply's quote in the box", |state| {
            state["box"]["mode"] == "reply"
        })?;
        let quote = state["box"]["shown"]
            .as_str()
            .or_else(|| state["box"]["preview"].as_str())
            .unwrap_or_default()
            .to_owned();
        check(
            "reply: the box's quote shows b's Arabic text",
            quote.contains(ARABIC_SHORT),
            || format!("it shows {quote:?}"),
        );
        self.run.shot(self.sandbox, "reply-box-light")?;
        self.run.type_text("Yes, ")?;
        self.paste(REPLY_ARABIC)?;
        self.run.press("enter")?;
        let wanted = format!("Yes, {REPLY_ARABIC}");
        let state = self
            .run
            .until("chat", "the sent reply", |state| text_of(&last_item(state)) == wanted)
            .or_else(|_| self.run.json("chat"))?;
        let item = last_item(&state);
        check(
            "reply: the sent reply shows the text sent",
            text_of(&item) == wanted,
            || format!("it shows {item}"),
        );
        let quoted = item["quote"].as_str().unwrap_or_default().to_owned();
        check(
            "reply: the sent reply quotes b's Arabic text",
            quoted.contains(ARABIC_SHORT),
            || format!("its quote is {quoted:?}"),
        );
        for (dark, theme) in [(false, "light"), (true, "dark")] {
            self.run.look(dark, DEFAULT_SIZE)?;
            self.run.shot(self.sandbox, &format!("reply-sent-{theme}"))?;
        }
        self.run.look(false, DEFAULT_SIZE)
    }

    /// Search (Ctrl+E, the words, Enter, its Messages tab) finds the sender's message in `chat` by `words` (pasted or typed): its
    /// snippet holds `words` and is a part of `text`, in the order of its letters.
    fn search(&mut self) -> SandboxResult {
        self.search_for(("جاهزون", true), ("Arabic desk", "Lab B"), ARABIC, "search-arabic")?;
        let user = format!("{USER_FIRST} {} {USER_LAST}", user_line());
        self.search_for(("agree", false), ("Options room", "Lab B"), &user, "search-user")
    }

    fn search_for(
        &mut self,
        (words, pasted): (&str, bool),
        (chat, sender): (&str, &str),
        text: &str,
        name: &str,
    ) -> SandboxResult {
        self.run.press("ctrl+e")?;
        if pasted {
            self.paste(words)?;
        } else {
            self.run.type_text(words)?;
        }
        self.run.press("enter")?;
        self.run.ok("tab-messages")?;
        let results = self
            .run
            .until("search", "the search's results", |results| {
                hit(results, chat, sender).is_some()
            })
            .or_else(|_| self.run.json("search"))?;
        save_line(&format!("{name}.json"), &results.to_string())?;
        let snippet = hit(&results, chat, sender)
            .and_then(|hit| hit["snippet"].as_str())
            .unwrap_or_default()
            .to_owned();
        check(
            &format!("search {words:?} finds {sender}'s message in {chat}, its snippet in the order of its letters"),
            snippet.contains(words) && in_order(&snippet, text),
            || format!("its snippet is {snippet:?}; results {results}"),
        );
        self.run.shot(self.sandbox, name)?;
        self.run.press("escape")?;
        Ok(())
    }
}

fn hit<'v>(results: &'v Value, chat: &str, sender: &str) -> Option<&'v Value> {
    results["hits"]
        .as_array()?
        .iter()
        .find(|hit| hit["chat"] == chat && hit["sender"] == sender)
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

fn engine_on_the_mock(dir: &Path) -> SandboxResult<EngineHandle> {
    let origin = std::env::var(ORIGIN_ENV)?;
    let tokens = Arc::new(Tokens {
        token: std::env::var(TOKEN_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
    let transport = Arc::new(http()?.with_middle_tier(MIDDLE_TIER_BASE)?);
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
            http()?
                .with_ams(AMS_BASE)?
                .with_presence(PRESENCE_BASE)?
                .with_csa(CSA_BASE)?
                .with_user_profile(USER_PROFILE_BASE)?
                .with_provisioning(PROVISIONING_BASE)?
                .with_middle_tier(MIDDLE_TIER_BASE)?,
        )),
        outbox: Outbox::new(transport, author),
        push: Some(Push::new(trouter)),
        options: EngineOptions::default(),
        known_names: vec![
            (LAB_B.to_owned(), "Lab B".to_owned()),
            (LAB_C.to_owned(), "Lab C".to_owned()),
        ],
    })?)
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let dir = PathBuf::from(std::env::var(DIR_ENV)?);
    let engine = engine_on_the_mock(&dir)?;
    let first = support::started(Alias::A, engine)?;
    let runner = support::runner(&dir)?;
    let settings = teams_ui::SettingsFile::new(dir.join("ui").join("a.toml"));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let look_file = dir.join(LOOK_FILE);
    let _ = fs::remove_file(&look_file);
    let ask = dir.join(ASK);
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
                let settings = launched.desktop.read(cx).settings().clone();
                follow_look_file(&settings, look_file.clone(), cx);
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
    Ok(())
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

/// The settings as the Appearance page leaves them once theme `key` and font size `size` are picked.
fn show_look(settings: &mut teams_ui::Settings, key: &'static str, size: u16) {
    settings.appearance.follow_system = false;
    settings.appearance.theme = key;
    settings.appearance.font_size = size;
}

/// Shows the look `file` names (`light 14`) once it changes, and says `look <it>` once it is applied.
fn follow_look_file(settings: &Entity<teams_ui::SettingsModel>, file: PathBuf, cx: &mut App) {
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
            let (theme, size) = wanted.split_once(' ').unwrap_or((wanted.as_str(), ""));
            let key = if theme == "dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            let size = size.parse().unwrap_or(DEFAULT_SIZE);
            let changed = cx
                .update(|cx| settings.update(cx, |model, cx| model.change(|chosen| show_look(chosen, key, size), cx)));
            if changed.is_err() {
                return;
            }
            cx.background_executor().timer(Duration::from_millis(400)).await;
            say(&format!("look {wanted}"));
            applied = wanted;
        }
    })
    .detach();
}

/// Answers the parent's questions as they come, until it asks to quit.
fn answer(window: WindowHandle<KitRoot>, ask: PathBuf, cx: &mut App) {
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
                cx.update(|cx| cx.quit());
                return;
            }
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

fn chat_view(shell: &Entity<Shell>, cx: &App) -> Option<Entity<ChatView>> {
    let shell = shell.read(cx);
    shell.chat().filter(|_| matches!(shell.main(), Main::Chat(_))).cloned()
}

fn on_shell(shell: &Entity<Shell>, verb: &str, window: &mut Window, cx: &mut App) -> String {
    let (verb, rest) = verb.split_once(' ').unwrap_or((verb, ""));
    match verb {
        "open" => {
            shell.update(cx, |shell, cx| shell.open_chat(rest.to_owned(), window, cx));
            "ok".to_owned()
        }
        "rows" => rows_of(shell, cx).to_string(),
        "chat" => chat_state(shell, window, cx).to_string(),
        "search" => search_state(shell, cx).to_string(),
        // the results page's Messages tab, as its click picks it
        "tab-messages" => {
            let search = shell.read(cx).search().clone();
            search.update(cx, |search, cx| {
                search.set_tab(teams_ui::global_search::Tab::Messages, cx);
            });
            "ok".to_owned()
        }
        "focus-box" => chat_view(shell, cx).map_or_else(
            || "no chat".to_owned(),
            |chat| {
                let compose = chat.read(cx).compose().clone();
                compose.update(cx, |compose, cx| compose.focus(window, cx));
                "ok".to_owned()
            },
        ),
        other => format!("unknown {other}"),
    }
}

/// Each chat list row: its chat, name and the preview it shows.
fn rows_of(shell: &Entity<Shell>, cx: &mut App) -> Value {
    let list = shell.read(cx).list().clone();
    let rows: Vec<teams_ui::chat_list::ChatRow> = list.read(cx).all_rows().cloned().collect();
    let shown: Vec<Value> = rows
        .iter()
        .map(|row| {
            let preview = list.update(cx, |list, cx| list.preview_shown(row, cx));
            json!({"id": row.id, "name": row.name.as_ref(), "preview": preview.as_ref()})
        })
        .collect();
    Value::Array(shown)
}

/// A message's shown text (its lines), whether it is drawn right to left, and the text of its quotes.
fn message_state(id: &str, body: &Body) -> Value {
    let Body::Text(rendered) = body else {
        return json!({"id": id, "text": "", "rtl": Value::Null, "quote": ""});
    };
    let blocks = &rendered.0.blocks;
    let text = blocks
        .iter()
        .flat_map(Block::plain_lines)
        .collect::<Vec<_>>()
        .join("\n");
    let quote = blocks
        .iter()
        .filter_map(|block| match block {
            Block::Quote(quote) => Some(quote.blocks.iter().flat_map(Block::plain_lines).collect::<Vec<_>>()),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
    json!({"id": id, "text": text, "rtl": rendered.0.right_to_left(), "quote": quote})
}

/// The open chat: its messages, the one selected, and the box's mode, quote, text and selection.
fn chat_state(shell: &Entity<Shell>, window: &Window, cx: &App) -> Value {
    let Some(chat) = chat_view(shell, cx) else {
        return json!({"items": []});
    };
    let chat = chat.read(cx);
    let items: Vec<Value> = chat
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::Message(message) => Some(message_state(&message.id, &message.body)),
            _ => None,
        })
        .collect();
    let compose = chat.compose().read(cx);
    let mut boxed = match compose.mode() {
        Mode::New => json!({"mode": "new"}),
        Mode::Reply(quoted) => json!({"mode": "reply", "preview": quoted.preview, "shown": quoted.shown}),
        Mode::Edit { .. } => json!({"mode": "edit"}),
    };
    let editor = compose.editor().read(cx);
    let selection = editor.selection();
    let (start, end) = selection.range();
    boxed["text"] = json!(editor.text());
    boxed["anchor"] = json!(selection.anchor.offset);
    boxed["head"] = json!(selection.head.offset);
    boxed["selected"] = json!(editor.doc().text_between(start, end));
    json!({
        "conversation": chat.conversation(),
        "selected": chat.selected(),
        "focused": compose.focused(window, cx),
        "items": items,
        "box": boxed,
    })
}

/// The search's message hits: their chat's name and snippet.
fn search_state(shell: &Entity<Shell>, cx: &App) -> Value {
    let search = shell.read(cx).search().read(cx);
    let hits: Vec<Value> = search
        .message_hits(cx)
        .iter()
        .map(|hit| {
            json!({
                "chat": hit.chat.as_ref(),
                "sender": hit.sender.as_ref(),
                "snippet": hit.snippet.as_ref(),
            })
        })
        .collect();
    json!({"query": search.query(cx), "failed": search.failed(), "hits": hits})
}
