//! QA of task 523 (round 1), in the UI sandbox: a channel post that came by email, as a user meets it, for a
//! Microsoft account on the mock. The real app runs as a child in the sandbox, as lab account a on `teams-mock` (its
//! chat service and its Trouter). The parent drives it with the sandbox's virtual pointer and keyboard, asks the child
//! what the window shows through a file in the sandbox dir, and finds the card's rows on the screen by their hairlines.
//!
//! Before the app starts, the team's channel gets email posts in the shape of `tests/fixtures/chatsvc/email-post.json`
//! (`properties.subject`, `properties.emaildetails`), each in one way that differs: a long subject, more than three
//! recipients, cc only, no subject, an HTML body, no original email; and plain posts, one of them with a subject.
//! While the app shows the channel, one more email post arrives live.
//!
//! - **The look**: each post shows what Teams shows for it ("<sender> via email", "To: …" with "+N", the bold
//!   subject, the rows); plain posts keep their look. Shots at 1280 px and 760 px, Light and Dark.
//! - **View original email**: a click opens the SharePoint `.eml` link in the browser with `download=1`.
//! - **Add reaction** in the card: the picker, "like", Enter: the reaction goes to the service and shows in the card.
//! - **Reply in thread**: the thread opens with its box focused; a reply typed and sent reaches the service and shows.
//! - **A refresh and a restart**: the post keeps its look.
//! - **Search**: what the app finds for the subject's words, the body's words and a plain post's words; the email post
//!   is found as a plain post is (the mock serves no service search).
//!
//! `QA_EP_EVIDENCE` names a directory that gets the screenshots and the results too. Screenshots also go to
//! `target/ui-shots/qa523-*.png`. Every name, address, link and text is made up.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

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
use teams_ctl::protocol::{OpenTarget, Request, Response};
use teams_ctl::{Client, Timeouts};
use teams_mock::{
    AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE, PRESENCE_BASE,
    PROVISIONING_BASE, TEAM_CHANNEL, USER_PROFILE_BASE,
};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::SettingsFile;
use teams_ui::chat_view::{ChatView, MessageItem};
use teams_ui::shell::{Main, Shell};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "../window.rs"]
mod support;
use support::{Alias, display_name};

const CHILD_ENV: &str = "QA_EP_CHILD";
const EVIDENCE_ENV: &str = "QA_EP_EVIDENCE";
const TRACE_FD_ENV: &str = "QA_EP_TRACE_FD";
const ORIGIN_ENV: &str = "QA_EP_ORIGIN";
const TROUTER_ENV: &str = "QA_EP_TROUTER";
const IC3_ENV: &str = "QA_EP_IC3";
const SKYPE_ENV: &str = "QA_EP_SKYPE";
const DIR_ENV: &str = "QA_EP_DIR";
const CTL_ENV: &str = "QA_EP_CTL";

const ASK: &str = "qa523-ask";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(100);
const SETTLE: Duration = Duration::from_millis(900);
const STEP: Duration = Duration::from_millis(250);

const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
const SCREEN: (u32, u32) = (1280, 800);

/// The account that the service names as an email post's sender: nobody the app knows.
const MAILER: &str = "8:orgid:00000000-0000-4000-8000-0000000000e1";
const MAILER_NAME: &str = "alerts-noreply@example.com";
const CHANNEL_ADDRESS: &str = "made-up-channel.example@example.com";
const SHAREPOINT: &str =
    "https://contoso.sharepoint.com/sites/MadeUpTeam/Shared Documents/General/EmailMessages_10_2025";
const LIVE_SUBJECT: &str = "Made-up alert: memory use - test cluster";
const LIVE_BODY: &str =
    "<div><p>Made-up Cloud</p><p>Your made-up alert was resolved</p><p>==========</p><p>...</p></div>";
const REPLY: &str = "Made-up reply to the alert";

fn long_subject() -> String {
    let mut subject = "Made-up long subject:".to_owned();
    let mut n = 0;
    while subject.chars().count() < 240 {
        n += 1;
        let _ = write!(subject, " part{n} of a very long made-up alert title");
    }
    subject
}

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_email_post_window: {error}");
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

/// Why a check could show nothing, which is not a pass.
#[derive(Debug)]
struct Inconclusive(String);

impl std::fmt::Display for Inconclusive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Inconclusive {}

/// What the checks came to.
#[derive(Default)]
struct Tally {
    passed: Vec<String>,
    skipped: Vec<(String, String)>,
    failed: Vec<(String, String)>,
}

fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

// --- the posts ---

fn person(name: &str, smtp: &str) -> Value {
    json!({"name": name, "smtp": smtp})
}

/// `properties` of an email post: its subject (none when `None`) and its `emaildetails` as a JSON string.
fn email_properties(subject: Option<&str>, details: &Value) -> Value {
    let mut properties = json!({
        "emaildetails": details.to_string(),
        "files": "[]",
        "links": "[]",
    });
    if let Some(subject) = subject {
        properties["subject"] = json!(subject);
    }
    properties
}

/// `emaildetails` from the mailer to `to` and `cc`, with the original email `eml` in the team's SharePoint.
fn details(to: &[Value], cc: &[Value], eml: Option<&str>) -> Value {
    let mut details = json!({
        "from": person("", MAILER_NAME),
        "sender": person("", MAILER_NAME),
        "to": to,
        "cc": cc,
        "totalRecipients": to.len() + cc.len(),
    });
    if let Some(eml) = eml {
        details["emailLink"] = json!(format!("{SHAREPOINT}/{eml}"));
    }
    details
}

fn channel_only() -> Vec<Value> {
    vec![person("", CHANNEL_ADDRESS)]
}

/// A post seeded before the app starts: what it is, who sent it, its body and its properties.
struct Seed {
    key: &'static str,
    from: (&'static str, &'static str),
    html: String,
    properties: Value,
}

/// A plain post from b, with no properties.
fn plain(key: &'static str, text: &str) -> Seed {
    Seed {
        key,
        from: (LAB_B, "Lab B"),
        html: format!("<p>{text}</p>"),
        properties: json!({}),
    }
}

fn seeds() -> Vec<Seed> {
    let many: Vec<Value> = (1..=5)
        .map(|n| person(&format!("Ops Person {n}"), &format!("ops{n}@example.com")))
        .collect();
    let cc = vec![
        person("", "lead@example.com"),
        person("Made-up Auditor", "audit@example.com"),
    ];
    vec![
        plain("plain-before", "A plain post before the emails."),
        Seed {
            key: "long-subject",
            from: (MAILER, MAILER_NAME),
            html: "<div><p>The long subject's body.</p></div>".to_owned(),
            properties: email_properties(
                Some(&long_subject()),
                &details(&channel_only(), &[], Some("Long subject.eml")),
            ),
        },
        Seed {
            key: "many-recipients",
            from: (MAILER, MAILER_NAME),
            html: "<div><p>Sent to many people.</p></div>".to_owned(),
            properties: email_properties(
                Some("Made-up notice to many people"),
                &details(&many, &cc, Some("Many recipients.eml")),
            ),
        },
        Seed {
            key: "cc-only",
            from: (MAILER, MAILER_NAME),
            html: "<div><p>The channel got this as a cc.</p></div>".to_owned(),
            properties: email_properties(
                Some("Made-up notice with the channel in cc"),
                &details(&[], &channel_only(), Some("Cc only.eml")),
            ),
        },
        Seed {
            key: "no-subject",
            from: (MAILER, MAILER_NAME),
            html: "<div><p>An email without a subject.</p></div>".to_owned(),
            properties: email_properties(None, &details(&channel_only(), &[], Some("No subject.eml"))),
        },
        Seed {
            key: "html-body",
            from: (MAILER, MAILER_NAME),
            html: "<div><p><b>Bold words</b> and <i>slanted words</i>.</p><br><br><p>After two blank lines, \
                   <a href=\"https://example.com/made-up\">a made-up link</a>.</p><ul><li>First point</li>\
                   <li>Second point</li></ul><table><tr><td>Cell one</td><td>Cell two</td></tr></table></div>"
                .to_owned(),
            properties: email_properties(
                Some("Made-up report with an HTML body"),
                &details(&channel_only(), &[], Some("Html body.eml")),
            ),
        },
        Seed {
            key: "no-link",
            from: (MAILER, MAILER_NAME),
            html: "<div><p>No original email for this one.</p></div>".to_owned(),
            properties: email_properties(
                Some("Made-up notice without its original"),
                &details(&channel_only(), &[], None),
            ),
        },
        Seed {
            key: "plain-with-subject",
            from: (LAB_C, "Lab C"),
            html: "<p>A plain post that has a subject.</p>".to_owned(),
            properties: json!({"subject": "Made-up plain post subject"}),
        },
        plain("plain-after", "A plain post after the emails."),
    ]
}

/// Puts `html` in the channel from `from`, with `properties`, as the service keeps and delivers it; its id.
fn put_post(mock: &ChatServiceMock, from: (&str, &str), html: &str, properties: &Value) -> SandboxResult<String> {
    // the properties go in with the post: a delivery before a later change would show it plain
    let id = mock
        .add_message_with(TEAM_CHANNEL, from, "RichText/Html", html, |message| {
            message["properties"] = properties.clone();
        })
        .ok_or("no channel on the mock")?;
    Ok(id)
}

// --- the parent ---

type Check = (&'static str, fn(&mut User<'_>) -> SandboxResult);

const CHECKS: [Check; 7] = [
    ("seeded_posts_look_as_in_teams", seeded_posts),
    ("live_email_post_looks_as_in_teams", live_post),
    ("view_original_email_downloads_the_eml", view_original),
    ("add_reaction_in_the_card", react_in_card),
    ("reply_in_thread_sends_a_reply", reply_in_thread),
    ("refresh_keeps_the_look", refresh),
    ("search_finds_the_email_post", search),
];

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_email_post_window", &options)? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mock = runtime.block_on(ChatServiceMock::start())?;
    let trouter = mock.start_trouter()?;
    let dir = tempfile::tempdir()?;
    let mut posts = Vec::new();
    for seed in seeds() {
        let id = put_post(&mock, seed.from, &seed.html, &seed.properties)?;
        posts.push((seed.key.to_owned(), id));
    }
    let _ = mock.take_events(LAB_A);
    let mut tally = Tally::default();
    let own = dir.path().join("a");
    {
        let app = AppUnderTest::start(&sandbox, &mock, trouter.origin(), &own, "first")?;
        let mut user = User::new((&sandbox, &mock, &runtime), app, &mut posts)?;
        for (name, check) in CHECKS {
            user.run_check(name, check, &mut tally);
        }
        user.quit()?;
    }
    {
        let app = AppUnderTest::start(&sandbox, &mock, trouter.origin(), &own, "restart")?;
        let mut user = User::new((&sandbox, &mock, &runtime), app, &mut posts)?;
        user.run_check("restart_keeps_the_look", restart, &mut tally);
        user.quit()?;
    }
    drop(trouter);
    let mut summary = String::new();
    let Tally {
        passed,
        skipped,
        failed,
    } = tally;
    for name in &passed {
        let _ = writeln!(summary, "pass {name}");
    }
    for (name, why) in &skipped {
        let _ = writeln!(summary, "skip {name}: {why}");
    }
    for (name, why) in &failed {
        let _ = writeln!(summary, "FAIL {name}: {why}");
    }
    say(&summary);
    if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(PathBuf::from(dir).join("results.txt"), &summary);
    }
    say(&format!(
        "qa_email_post_window: {} passed, {} skipped, {} failed",
        passed.len(),
        skipped.len(),
        failed.len()
    ));
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("{} checks failed", failed.len()).into())
    }
}

/// The child app in its sandbox: its process, its trace, its control socket.
struct AppUnderTest {
    running: Option<Running>,
    _trace: TraceReader,
    socket: PathBuf,
}

impl AppUnderTest {
    fn start(sandbox: &Sandbox, mock: &ChatServiceMock, trouter: &str, own: &Path, run: &str) -> SandboxResult<Self> {
        fs::create_dir_all(own)?;
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let ic3 = mock.token(LAB_A).ok_or("the mock's token")?;
        let skype = mock.skypetoken(LAB_A).ok_or("the mock's skypetoken")?;
        let socket = sandbox.path(&format!("run/ctl-{run}.sock"));
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
        Ok(Self {
            running: Some(running),
            _trace: trace,
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

    fn wait_for(&mut self, view: &str, change: &str) -> SandboxResult {
        let deadline = Instant::now() + STARTUP;
        while !self
            .seen
            .iter()
            .any(|line| line["view"] == view && line["change"] == change)
        {
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

/// Where an email post's card rows are on the screen: its hairlines, its left edge, its "Add reaction".
#[derive(Clone, Copy, Debug)]
struct CardRows {
    left: u32,
    original: (i32, i32),
    reply: (i32, i32),
    add_reaction: (i32, i32),
}

/// The user at the sandbox: the pointer, the keyboard, the screenshots, the control socket, and the child it asks
/// about the window.
struct User<'a> {
    sandbox: &'a Sandbox,
    mock: &'a ChatServiceMock,
    runtime: &'a tokio::runtime::Runtime,
    app: AppUnderTest,
    /// The posts by what each shows: the seeds' keys, and "live" once it came.
    posts: &'a mut Vec<(String, String)>,
    input: VirtualInput,
    ctl: Client,
    asked: u32,
}

impl<'a> User<'a> {
    fn new(
        (sandbox, mock, runtime): (&'a Sandbox, &'a ChatServiceMock, &'a tokio::runtime::Runtime),
        app: AppUnderTest,
        posts: &'a mut Vec<(String, String)>,
    ) -> SandboxResult<Self> {
        let socket = app.socket.clone();
        let ctl = wait_until(WAIT, || Client::connect(&socket, Timeouts::default()).ok())
            .ok_or_else(|| format!("no control socket at {}", socket.display()))?;
        let mut user = Self {
            sandbox,
            mock,
            runtime,
            app,
            posts,
            input: sandbox.input()?,
            ctl,
            asked: 0,
        };
        user.park()?;
        thread::sleep(SETTLE);
        Ok(user)
    }

    fn post(&self, key: &str) -> SandboxResult<String> {
        self.posts
            .iter()
            .find(|(known, _)| known == key)
            .map(|(_, id)| id.clone())
            .ok_or_else(|| format!("no post {key}").into())
    }

    fn run_check(&mut self, name: &'static str, check: fn(&mut User<'_>) -> SandboxResult, tally: &mut Tally) {
        say(&format!("check {name}"));
        match check(self) {
            Ok(()) => {
                say(&format!("  pass {name}"));
                tally.passed.push(name.to_owned());
            }
            Err(error) if error.is::<Inconclusive>() => {
                say(&format!("  skip {name}: {error}"));
                tally.skipped.push((name.to_owned(), error.to_string()));
            }
            Err(error) => {
                let error = error.to_string();
                let state = self.state().map(|state| state.to_string()).unwrap_or_default();
                let _ = self.shot(&format!("failed-{name}"), false);
                say(&format!("  FAIL {name}: {error}\n  state: {state}"));
                tally.failed.push((name.to_owned(), error));
            }
        }
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

    fn click(&mut self, (x, y): (i32, i32)) -> SandboxResult {
        let (x, y) = (u32::try_from(x)?, u32::try_from(y)?);
        self.input.move_to(x, y)?;
        thread::sleep(Duration::from_millis(120));
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn park(&mut self) -> SandboxResult {
        // the window's left margin: no button, tooltip or tile there
        self.input.move_to(3, SCREEN.1 / 2)
    }

    /// A screenshot as `qa523-<name>.png` (the focused window alone, or the screen), under `target/ui-shots/` and the
    /// evidence dir.
    fn shot(&mut self, name: &str, window: bool) -> SandboxResult<PathBuf> {
        let file = format!("qa523-{name}.png");
        let png = self.sandbox.screenshot(&file, window)?;
        let kept = shots_dir()?.join(&file);
        fs::copy(&png, &kept)?;
        if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
            let dir = PathBuf::from(dir).join("shots");
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(&file))?;
        }
        say(&format!("  shot qa523-{name}.png"));
        Ok(kept)
    }

    /// The window in Teams Light and Teams Dark, at the full width and at `narrow` px when there is one; Teams Light
    /// and the full width after.
    fn shots(&mut self, name: &str, narrow: Option<i32>) -> SandboxResult {
        for (verb, theme) in [("theme-light", "light"), ("theme-dark", "dark")] {
            self.ask(verb)?;
            self.park()?;
            thread::sleep(SETTLE);
            self.shot(&format!("{name}-{theme}"), true)?;
            if let Some(width) = narrow {
                self.resize(width)?;
                self.park()?;
                thread::sleep(SETTLE);
                self.shot(&format!("{name}-{theme}-{width}"), true)?;
                self.resize(i32::try_from(SCREEN.0)?)?;
            }
        }
        self.ask("theme-light")?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn resize(&mut self, width: i32) -> SandboxResult {
        self.sandbox
            .niri_msg(&["action", "set-column-width", &width.to_string()])?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The wheel over the channel's list, `steps` up (negative) or down.
    fn scroll_list(&mut self, steps: i32) -> SandboxResult {
        self.input.scroll(860, 400, steps)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn open_channel(&mut self) -> SandboxResult {
        self.ctl.request(
            None,
            Request::Open {
                target: OpenTarget::Channel {
                    channel: TEAM_CHANNEL.to_owned(),
                },
            },
        )?;
        self.until("the channel open", |state| {
            state["main"] == format!("channel:{TEAM_CHANNEL}")
                && state["posts"].as_array().is_some_and(|posts| !posts.is_empty())
        })?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The screen as an image, the pointer parked.
    fn screen(&mut self) -> SandboxResult<RgbImage> {
        self.park()?;
        thread::sleep(SETTLE);
        let png = self.sandbox.screenshot("qa523-find.png", false)?;
        let image = image::open(&png)?.to_rgb8();
        let _ = fs::remove_file(png);
        Ok(image)
    }

    /// The rows of the lowest email card on the screen (the newest post; Teams Light).
    fn card_rows(&mut self) -> SandboxResult<CardRows> {
        let image = self.screen()?;
        card_rows(&image).ok_or_else(|| "no email card's rows on the screen".into())
    }
}

// --- finding the card's rows on the screen ---

/// The rows of `image` where a card's hairline runs: one row of one colour across the card's middle, the card's own
/// white over and under it.
fn hairlines(image: &RgbImage) -> Vec<u32> {
    let (from, to) = (640, 960);
    (1..image.height() - 1)
        .filter(|&y| {
            let line = *image.get_pixel(from, y);
            let card = *image.get_pixel(from, y - 1);
            // in Teams Light a card is white, its hairline a grey with white rows around it
            line != card
                && card.0.iter().all(|&channel| channel >= 250)
                && y >= 3
                && y + 3 < image.height()
                && *image.get_pixel(from, y - 3) == card
                && *image.get_pixel(from, y + 3) == card
                && *image.get_pixel(from, y + 1) == card
                && (from..=to).all(|x| {
                    *image.get_pixel(x, y) == line
                        && *image.get_pixel(x, y - 1) == card
                        && *image.get_pixel(x, y + 1) == card
                })
        })
        .collect()
}

/// The newest email card's rows: its last two hairlines over the compose box.
fn card_rows(image: &RgbImage) -> Option<CardRows> {
    let lines = hairlines(image);
    let (&second, rest) = lines.split_last()?;
    let &first = rest.last()?;
    let line = *image.get_pixel(640, first);
    let mut left = 640;
    while left > 0 && *image.get_pixel(left - 1, first) == line {
        left -= 1;
    }
    let card = *image.get_pixel(left + 4, first - 2);
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for y in first.saturating_sub(48)..first {
        for x in left..left + 120 {
            if *image.get_pixel(x, y) != card {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
    }
    if x1 < x0 {
        return None;
    }
    let row = second - first;
    let at = |x: u32, y: u32| Some((i32::try_from(x).ok()?, i32::try_from(y).ok()?));
    Some(CardRows {
        left,
        original: at(left + 90, first + row / 2)?,
        reply: at(left + 90, second + row / 2)?,
        add_reaction: at(x0.midpoint(x1), y0.midpoint(y1))?,
    })
}

// --- the checks ---

fn post_of<'v>(state: &'v Value, id: &str) -> SandboxResult<&'v Value> {
    state["posts"]
        .as_array()
        .and_then(|posts| posts.iter().find(|post| post["id"] == id))
        .ok_or_else(|| format!("post {id} not shown").into())
}

fn expect(post: &Value, key: &str, wanted: &Value) -> SandboxResult {
    ensure(&post[key] == wanted, || {
        format!("{key}: {} (wanted {wanted}) in {post}", post[key])
    })
}

fn seeded_posts(user: &mut User<'_>) -> SandboxResult {
    user.open_channel()?;
    let state = user.until("every seeded post", |state| {
        state["posts"].as_array().is_some_and(|posts| posts.len() >= 9)
    })?;
    let via = json!(format!("{MAILER_NAME} via email"));
    let to_channel = json!(format!("To: {CHANNEL_ADDRESS}"));
    let link = |eml: &str| json!(format!("{SHAREPOINT}/{eml}"));
    let post = post_of(&state, &user.post("long-subject")?)?;
    expect(post, "sender", &via)?;
    expect(post, "subject", &json!(long_subject()))?;
    expect(post, "to", &to_channel)?;
    expect(post, "link", &link("Long subject.eml"))?;
    let post = post_of(&state, &user.post("many-recipients")?)?;
    expect(post, "to", &json!("To: Ops Person 1, Ops Person 2, Ops Person 3 +4"))?;
    let post = post_of(&state, &user.post("cc-only")?)?;
    expect(post, "to", &to_channel)?;
    let post = post_of(&state, &user.post("no-subject")?)?;
    expect(post, "subject", &Value::Null)?;
    expect(post, "email", &json!(true))?;
    expect(post, "sender", &via)?;
    let post = post_of(&state, &user.post("html-body")?)?;
    expect(post, "subject", &json!("Made-up report with an HTML body"))?;
    expect(post, "kind", &json!("text"))?;
    let post = post_of(&state, &user.post("no-link")?)?;
    expect(post, "email", &json!(true))?;
    expect(post, "link", &Value::Null)?;
    let post = post_of(&state, &user.post("plain-with-subject")?)?;
    expect(post, "email", &json!(false))?;
    expect(post, "subject", &json!("Made-up plain post subject"))?;
    expect(post, "sender", &json!("Lab C"))?;
    for key in ["plain-before", "plain-after"] {
        let post = post_of(&state, &user.post(key)?)?;
        expect(post, "email", &json!(false))?;
        expect(post, "subject", &Value::Null)?;
        expect(post, "sender", &json!("Lab B"))?;
    }
    // every email post starts its own group: its header and its "To" line
    for key in [
        "long-subject",
        "many-recipients",
        "cc-only",
        "no-subject",
        "html-body",
        "no-link",
    ] {
        let post = post_of(&state, &user.post(key)?)?;
        expect(post, "header", &json!(true))?;
        expect(post, "thread", &json!("post"))?;
    }
    // the list, from its end up, in the light and dark themes
    user.shots("seeded-end", None)?;
    for page in 1..=6 {
        user.scroll_list(-4)?;
        user.shots(&format!("seeded-up{page}"), None)?;
    }
    user.scroll_list(40)?;
    Ok(())
}

fn live_post(user: &mut User<'_>) -> SandboxResult {
    let properties = email_properties(
        Some(LIVE_SUBJECT),
        &details(&channel_only(), &[], Some("Made-up alert resolved.eml")),
    );
    let id = put_post(user.mock, (MAILER, MAILER_NAME), LIVE_BODY, &properties)?;
    user.posts.push(("live".to_owned(), id.clone()));
    let state = user.until("the live email post", |state| {
        post_of(state, &id).is_ok_and(|post| post["email"] == true)
    })?;
    let post = post_of(&state, &id)?;
    expect(post, "sender", &json!(format!("{MAILER_NAME} via email")))?;
    expect(post, "subject", &json!(LIVE_SUBJECT))?;
    expect(post, "to", &json!(format!("To: {CHANNEL_ADDRESS}")))?;
    expect(post, "header", &json!(true))?;
    thread::sleep(SETTLE);
    user.shots("live", Some(760))?;
    Ok(())
}

fn view_original(user: &mut User<'_>) -> SandboxResult {
    let rows = user.card_rows()?;
    say(&format!("  card rows {rows:?}"));
    let before = user.sandbox.opened()?.len();
    user.click(rows.original)?;
    let opened = wait_until(WAIT, || {
        let opened = user.sandbox.opened().ok()?;
        (opened.len() > before).then(|| opened[before..].to_vec())
    })
    .ok_or("View original email opened nothing")?;
    let url = opened
        .iter()
        .flat_map(|call| call.args.iter())
        .find(|arg| arg.starts_with("https://"))
        .cloned()
        .ok_or_else(|| format!("no https link opened: {opened:?}"))?;
    say(&format!("  opened {url}"));
    if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
        let _ = fs::write(PathBuf::from(dir).join("opened.txt"), format!("{opened:?}\n"));
    }
    ensure(
        url.starts_with("https://contoso.sharepoint.com/sites/MadeUpTeam/Shared%20Documents/General/"),
        || format!("the link {url}"),
    )?;
    ensure(url.ends_with("Made-up%20alert%20resolved.eml?download=1"), || {
        format!("the link {url}")
    })?;
    // the app does not post or change anything for it
    ensure(user.state()?["thread"].is_null(), || "a thread opened".to_owned())?;
    Ok(())
}

fn react_in_card(user: &mut User<'_>) -> SandboxResult {
    let id = user.post("live")?;
    let rows = user.card_rows()?;
    user.click(rows.add_reaction)?;
    user.until("the reaction picker", |state| state["picker"] == true)?;
    user.shot("picker", true)?;
    user.input.type_text("like")?;
    thread::sleep(SETTLE);
    user.press("Enter")?;
    let state = user.until("a's like on the email post", |state| {
        post_of(state, &id).is_ok_and(|post| {
            post["reactions"]
                .as_array()
                .is_some_and(|reactions| reactions.iter().any(|r| r["key"] == "like" && r["mine"] == true))
        })
    })?;
    let post = post_of(&state, &id)?;
    expect(post, "email", &json!(true))?;
    expect(post, "subject", &json!(LIVE_SUBJECT))?;
    let sent = wait_until(WAIT, || {
        user.requests()
            .into_iter()
            .find(|line| line.contains(&format!("messages/{id}/properties")) && line.contains("emotions"))
    })
    .ok_or("no reaction write reached the service")?;
    say(&format!("  sent {sent}"));
    // the picked post stays selected: Escape clears it, as a user's next key does
    user.press("Escape")?;
    user.press("Escape")?;
    // the reaction is drawn in the card, over its rows
    let after = user.card_rows()?;
    ensure(after.left == rows.left, || {
        format!("the card moved: {rows:?} -> {after:?}")
    })?;
    user.shots("reacted", None)?;
    Ok(())
}

impl User<'_> {
    /// The mock's requests so far, each as "<method> <path>?<query>".
    fn requests(&self) -> Vec<String> {
        self.runtime
            .block_on(self.mock.received_requests())
            .into_iter()
            .map(|request| {
                format!(
                    "{} {}?{}",
                    request.method,
                    request.url.path(),
                    request.url.query().unwrap_or_default()
                )
            })
            .collect()
    }
}

fn reply_in_thread(user: &mut User<'_>) -> SandboxResult {
    let id = user.post("live")?;
    let rows = user.card_rows()?;
    user.click(rows.reply)?;
    user.until("the post's thread", |state| state["thread"]["post"] == id.as_str())?;
    thread::sleep(SETTLE);
    user.shot("thread-open", true)?;
    user.input.type_text(REPLY)?;
    thread::sleep(STEP);
    user.press("Enter")?;
    let state = user.until("the reply in the thread and its count under the post", |state| {
        state["thread"]["texts"].as_array().is_some_and(|texts| {
            texts
                .iter()
                .any(|text| text.as_str().is_some_and(|text| text.contains(REPLY)))
        }) && post_of(state, &id).is_ok_and(|post| post["replies"] == 1)
    })?;
    let top = &state["thread"]["top"];
    expect(top, "email", &json!(true))?;
    expect(top, "subject", &json!(LIVE_SUBJECT))?;
    let requests = user.requests();
    ensure(
        requests
            .iter()
            .any(|line| line.starts_with("POST") && line.contains(&format!(";messageid={id}/messages"))),
        || "no reply reached the service".to_owned(),
    )?;
    user.shots("replied", None)?;
    user.press("Escape")?;
    Ok(())
}

fn expect_live_look(user: &mut User<'_>, what: &str) -> SandboxResult {
    let id = user.post("live")?;
    let state = user.until(what, |state| post_of(state, &id).is_ok())?;
    let post = post_of(&state, &id)?;
    expect(post, "email", &json!(true))?;
    expect(post, "subject", &json!(LIVE_SUBJECT))?;
    expect(post, "sender", &json!(format!("{MAILER_NAME} via email")))?;
    expect(post, "to", &json!(format!("To: {CHANNEL_ADDRESS}")))?;
    ensure(post["replies"] == 1, || format!("the replies {post}"))?;
    ensure(
        post["reactions"]
            .as_array()
            .is_some_and(|reactions| reactions.iter().any(|r| r["key"] == "like")),
        || format!("the reactions {post}"),
    )
}

fn refresh(user: &mut User<'_>) -> SandboxResult {
    user.ctl.request(None, Request::Sync)?;
    thread::sleep(SETTLE);
    // away to the calendar and back, as a user leaves the channel and comes back to it
    user.ctl.request(
        None,
        Request::Open {
            target: OpenTarget::Calendar,
        },
    )?;
    user.until("the calendar open", |state| state["main"] == "Calendar")?;
    user.open_channel()?;
    expect_live_look(user, "the email post after a refresh")?;
    user.shots("refreshed", None)?;
    Ok(())
}

fn search(user: &mut User<'_>) -> SandboxResult {
    let mut found = String::new();
    // the subject's words, the body's, and a plain post's
    for query in ["cluster", "resolved", "plain"] {
        let response = user.ctl.request(
            None,
            Request::Search {
                query: query.to_owned(),
                limit: Some(20),
            },
        )?;
        let text = match response {
            Response::Search { messages } => format!("{messages:?}"),
            other => format!("not a search answer: {other:?}"),
        };
        let _ = writeln!(found, "{query}: {text}");
        say(&format!("  search {query}: {text}"));
    }
    if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
        let _ = fs::write(PathBuf::from(dir).join("search.txt"), &found);
    }
    user.ctl.request(
        None,
        Request::Open {
            target: OpenTarget::Search {
                query: "resolved".to_owned(),
            },
        },
    )?;
    thread::sleep(Duration::from_secs(3));
    user.shot("search-light", true)?;
    // the mock has no service search, and the app's own index may not hold channel posts: an email post must only be
    // found as a plain post is
    let plain = user.post("plain-after")?;
    let live = user.post("live")?;
    let hits = |n: usize, id: &str| found.lines().nth(n).is_some_and(|line| line.contains(id));
    if !hits(2, &plain) {
        return Err(Inconclusive(format!("search found no plain post either: {found}")).into());
    }
    ensure(hits(0, &live) && hits(1, &live), || {
        format!("a plain post is found by its words, the email post not by its subject and body: {found}")
    })
}

fn restart(user: &mut User<'_>) -> SandboxResult {
    user.open_channel()?;
    expect_live_look(user, "the email post after a restart")?;
    user.shots("restarted", None)?;
    Ok(())
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

/// a's engine on the mock (its chats, teams and Trouter), with no calls.
fn engine(dir: &Path) -> SandboxResult<EngineHandle> {
    let tokens = Arc::new(Tokens {
        ic3: env(IC3_ENV)?,
        skypetoken: env(SKYPE_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let origin = env(ORIGIN_ENV)?;
    let trouter_origin = env(TROUTER_ENV)?;
    let transport = Arc::new(HttpTransport::with_mock_origin(
        Arc::clone(&tokens),
        CHAT_BASE,
        &origin,
    )?);
    let options = Options {
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(200),
        backoff_jitter_percent: 0,
        ..Options::default()
    };
    let trouter =
        Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), &trouter_origin)?.with_options(options);
    let name = display_name("bot-a@lab.example.test").to_owned();
    let client = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin)?
        .with_ams(AMS_BASE)?
        .with_presence(PRESENCE_BASE)?
        .with_csa(CSA_BASE)?
        .with_user_profile(USER_PROFILE_BASE)?
        .with_provisioning(PROVISIONING_BASE)?
        .with_middle_tier(MIDDLE_TIER_BASE)?;
    Ok(engine::start(Parts {
        calls: None,
        store: Store::open(&support::store_file(dir, Alias::A))?,
        client: Arc::new(ChatClient::new(client)),
        outbox: Outbox::new(transport, Author::new(Mri::new(LAB_A), name)),
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
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let engine = engine(&dir)?;
    let first = support::started(Alias::A, engine)?;
    let runner = support::runner(&dir)?;
    let settings = SettingsFile::new(dir.join("settings.toml"));
    settings
        .save(&teams_ui::settings::Settings::default())
        .map_err(|problem| format!("{problem:?}"))?;
    let ctl = PathBuf::from(env(CTL_ENV)?);
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
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

fn on_app(window: WindowHandle<KitRoot>, verb: &str, cx: &mut App) -> String {
    window
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
        .unwrap_or_else(|| "no shell".to_owned())
}

fn on_shell(shell: &Entity<Shell>, verb: &str, cx: &mut App) -> String {
    match verb {
        "state" => state_of(shell, cx).to_string(),
        "theme-dark" | "theme-light" => {
            let key = if verb == "theme-dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            let model = shell.read(cx).settings().clone();
            model.update(cx, |model, cx| {
                model.change(
                    |settings| {
                        settings.appearance.follow_system = false;
                        settings.appearance.theme = key;
                    },
                    cx,
                );
            });
            "ok".to_owned()
        }
        other => format!("unknown {other}"),
    }
}

/// What a post shows: its sender's name, its header, its subject, its email's "To" line and link, its reactions and
/// its replies.
fn item_json(item: &MessageItem) -> Value {
    let email = item.email.as_deref();
    json!({
        "id": item.id,
        "kind": item.body.kind(),
        "sender": item.sender_name.to_string(),
        "header": item.header,
        "subject": item.subject.as_ref().map(ToString::to_string),
        "email": email.is_some(),
        "to": email.and_then(|post| post.to.as_ref()).map(ToString::to_string),
        "link": email.and_then(|post| post.link.clone()),
        "thread": format!("{:?}", item.thread).to_lowercase(),
        "reactions": item.reactions.iter().map(|reaction| json!({
            "key": reaction.key,
            "count": reaction.count(),
            "mine": reaction.mine,
        })).collect::<Vec<_>>(),
        "replies": item.replies.as_ref().map(|replies| replies.count),
        "text": format!("{:?}", item.body).chars().take(400).collect::<String>(),
    })
}

fn items(view: &Entity<ChatView>, cx: &App) -> Vec<Value> {
    view.read(cx).message_items().map(item_json).collect()
}

/// What the window shows: the main view, the channel's posts, the thread open beside them, and whether a reaction
/// picker is open.
fn state_of(shell: &Entity<Shell>, cx: &App) -> Value {
    let shell_ref = shell.read(cx);
    let main = match shell_ref.main() {
        Main::Chat(id) => format!("chat:{id}"),
        Main::Channel(id) => format!("channel:{id}"),
        other => format!("{other:?}"),
    };
    let channel = shell_ref
        .channel()
        .filter(|_| matches!(shell_ref.main(), Main::Channel(_)));
    let posts = channel
        .map(|channel| items(channel.read(cx).posts(), cx))
        .unwrap_or_default();
    let picker = channel.is_some_and(|channel| {
        let channel = channel.read(cx);
        channel.posts().read(cx).more_reactions().is_some()
            || channel
                .thread()
                .is_some_and(|(_, thread)| thread.read(cx).more_reactions().is_some())
    });
    let thread = channel.and_then(|channel| {
        let channel = channel.read(cx);
        let (post, view) = channel.thread()?;
        let rows = items(view, cx);
        Some(json!({
            "post": post,
            "top": rows.first().cloned(),
            "texts": rows.iter().map(|row| row["text"].clone()).collect::<Vec<_>>(),
        }))
    });
    json!({
        "main": main,
        "posts": posts,
        "thread": thread,
        "picker": picker,
    })
}
