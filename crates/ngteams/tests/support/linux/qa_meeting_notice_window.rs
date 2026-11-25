//! QA of task 664 (round 1), in the UI sandbox: lab account a's app on the mock (its chat service, Trouter, calling
//! service and calendar), each run with a fresh mock and a fresh store. The parent asks the mock to push what the
//! services push, and reads what the user sees: the desktop notices on the sandbox's bus (and acts on them there, as
//! the notification daemon's buttons do), the window's cards (by the child's answers and by screenshots), and a click
//! of the sandbox's pointer on a card's Join. Every name, id and time is made up.
//!
//! - **The calling push** (`push`): b's start of a meeting a's calendar holds, pushed as `evt` 128 with no chat message,
//!   shows "Lab B started the meeting" with Join and Message (desktop notice and card, in Teams Light and Dark); the
//!   notice's Join opens the meeting's pre-join, its Message the meeting's chat; the push and the chat's start of one
//!   call, in either order, show one banner; the lobby's push says who waits; a group chat's Meet now pushed
//!   (`HuddleMeetingStart`) with its chat's start shows one banner; the pointer's click on the card's Join opens the
//!   pre-join.
//! - **A late start** (`late`): the app starts 3 minutes after b started the meeting: a banner; 7 minutes after: none.
//! - **The reminders** (`reminders`): at the event's own reminder time (5, 30 minutes) and at Outlook's default (15) for
//!   an event that names none; a late one up to 10 minutes after the start, none later; none for a declined,
//!   cancelled, all-day or offline event, nor before its time; Snooze (the notice's, 5 minutes; the card's 1 minute and
//!   "Until it starts") brings it back at its time; Dismiss and Join end it; "Starting now" at the start, unless
//!   dismissed or joined; never twice for one occasion; the notices never expire. Shots of the cards in Teams Light and
//!   Dark, at a larger font and in narrow windows.
//! - **A moved meeting** (`moved`): its reminder closes when it moves away, and it reminds again at its new time, once.
//! - **The switch** (`switch`): Settings → Notifications → "Meeting reminders" off: no reminder; on: it comes.
//!
//! `QA_MN_RUNS` (comma-separated run names; unset: all). `QA_MN_EVIDENCE`: a directory the shots go to as well.
//! `QA_MN_LOG_DIR`: a directory each app's debug log goes to (never commit it). Without weston 15, niri or
//! `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use std::{fs, future, io, thread};

use base64::Engine as _;
use gpui::{App, AsyncApp, Entity, WindowHandle, WindowingModes};
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
use teams_media::devices::Devices;
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::processing::ProcessingConfig;
use teams_media::relay::RelayServer;
use teams_media::session::SessionConfig;
use teams_media::tone::Tone;
use teams_mock::{
    ABC_CHAT, CHAT_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE, TENANT, TrouterMock,
};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use teams_ui::meeting_reminder::{MeetingReminders, Stage};
use teams_ui::settings::Settings;
use teams_ui::shell::Shell;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use teams_ui::{SettingsFile, VideoFeed};
use tokio::sync::broadcast;
use ui_sandbox::bus::Notification;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "../window.rs"]
mod support;

use support::Alias;

const CHILD_ENV: &str = "QA_MN_CHILD";
const RUNS_ENV: &str = "QA_MN_RUNS";
const EVIDENCE_ENV: &str = "QA_MN_EVIDENCE";
/// A directory each child's app log (debug) goes to, if set; never commit what it writes.
const LOG_ENV: &str = "QA_MN_LOG_DIR";
const TRACE_FD_ENV: &str = "QA_MN_TRACE_FD";
/// The child's own directory, its mock's origins, a's made-up tokens and whether its reminders start off.
const DIR_ENV: &str = "QA_MN_DIR";
const ORIGIN_ENV: &str = "QA_MN_ORIGIN";
const TROUTER_ENV: &str = "QA_MN_TROUTER";
const RELAY_ENV: &str = "QA_MN_RELAY";
const IC3_ENV: &str = "QA_MN_IC3";
const SKYPE_ENV: &str = "QA_MN_SKYPE";
const REMINDERS_OFF_ENV: &str = "QA_MN_REMINDERS_OFF";
/// The file in the sandbox dir the parent writes a question to: `<id> <verb> <rest>`; the child answers on its
/// stdout, `answer <id> <json>`.
const ASK: &str = "qa-ask";
const STARTUP: Duration = Duration::from_secs(180);
const WAIT: Duration = Duration::from_secs(60);
/// How long a banner may take to come, and how long none coming counts as none.
const BANNER_WAIT: Duration = Duration::from_secs(8);
/// How long a reminder may take to come: the reminders look at the calendar every 15 s.
const REMINDER_WAIT: Duration = Duration::from_secs(40);
const SETTLE: Duration = Duration::from_millis(800);
const SCREEN: (u32, u32) = (1280, 800);
/// The made-up meeting chats on a's calendar, b organizing, a and c attending.
const MEETING_CHAT: &str = "19:meeting_MadeUpQaPushed@thread.v2";
const JOIN_CHAT: &str = "19:meeting_MadeUpQaJoined@thread.v2";
const SUBJECT: &str = "Made-up roadmap review";
/// How many times the calendar's last answer of a run is mounted: every later read.
const CALENDAR_READS: usize = 400;
/// The made-up conversation a call runs at.
const LINK: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/QaMadeUp?i=10-0-0-2&amp;e=1";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown child {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa meeting notice window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn env(name: &str) -> SandboxResult<String> {
    Ok(std::env::var(name).map_err(|_| format!("{name} isn't set"))?)
}

fn ensure(holds: bool, what: impl FnOnce() -> String) -> SandboxResult {
    if holds { Ok(()) } else { Err(what().into()) }
}

// --- the parent ---

type Run = fn(&mut Qa<'_>) -> SandboxResult;
type Check = (&'static str, Run);
/// A run: its name, its calendar's answers, its checks, and whether the reminders' switch starts off.
type RunSpec = (&'static str, fn() -> Vec<Vec<Value>>, &'static [Check], bool);

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        width: SCREEN.0,
        height: SCREEN.1,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa meeting notice window", &options)? else {
        return Ok(());
    };
    let wanted = std::env::var(RUNS_ENV).unwrap_or_default();
    let runs: [RunSpec; 5] = [
        ("push", push_calendar, &PUSH_CHECKS, false),
        ("late", late_calendar, &LATE_CHECKS, false),
        ("reminders", reminder_calendar, &REMINDER_CHECKS, false),
        ("moved", moved_calendar, &MOVED_CHECKS, false),
        ("switch", switch_calendar, &SWITCH_CHECKS, true),
    ];
    let (mut passed, mut failed) = (Vec::new(), Vec::new());
    for (name, calendar, checks, reminders_off) in runs {
        if !wanted.is_empty() && !wanted.split(',').any(|run| run == name) {
            continue;
        }
        say(&format!("qa: run {name}"));
        match run(&sandbox, name, calendar(), checks, reminders_off) {
            Ok((mut ok, mut bad)) => {
                passed.append(&mut ok);
                failed.append(&mut bad);
            }
            Err(error) => failed.push((name, format!("the run failed: {error}"))),
        }
    }
    say(&format!("qa: {} passed, {} failed", passed.len(), failed.len()));
    for (name, why) in &failed {
        say(&format!("qa: FAILED {name}: {why}"));
    }
    if failed.is_empty() {
        say("qa meeting notice window: ok");
        Ok(())
    } else {
        Err(format!("{} checks failed", failed.len()).into())
    }
}

/// The flows that passed, and those that failed with why.
type Tally = (Vec<&'static str>, Vec<(&'static str, String)>);

fn run(
    sandbox: &Sandbox,
    name: &str,
    calendar: Vec<Vec<Value>>,
    checks: &[Check],
    reminders_off: bool,
) -> SandboxResult<Tally> {
    let world = World::start(calendar)?;
    if name == "late" {
        // the late runs start their own apps
        let mut qa = Qa::new(sandbox, &world, None)?;
        for (check, flow) in checks {
            qa.check(check, *flow);
        }
        return Ok((qa.passed, qa.failed));
    }
    let dir = tempfile::tempdir_in(world.dir.path())?;
    let app = Child::start(sandbox, &world, dir.path(), reminders_off)?;
    let mut qa = Qa::new(sandbox, &world, Some(app))?;
    for (check, flow) in checks {
        qa.check(check, *flow);
    }
    let (passed, failed) = (qa.passed.clone(), qa.failed.clone());
    if let Some(app) = qa.app.take() {
        app.quit(sandbox)?;
    }
    Ok((passed, failed))
}

/// One run's mock: its chat service, Trouter, TURN server and calendar.
struct World {
    _runtime: tokio::runtime::Runtime,
    mock: Arc<ChatServiceMock>,
    trouter: TrouterMock,
    relay: std::net::SocketAddr,
    dir: tempfile::TempDir,
}

impl World {
    /// The mock, its calendar answering `calendar`'s answers in turn, the last one every read after.
    fn start(calendar: Vec<Vec<Value>>) -> SandboxResult<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mock = Arc::new(runtime.block_on(ChatServiceMock::start())?);
        let trouter = mock.start_trouter()?;
        let dir = tempfile::tempdir()?;
        let relay = mock.start_media(
            dir.path(),
            Tone {
                hz: 880.0,
                amplitude: 0.5,
            },
        )?;
        for (chat, topic) in [(MEETING_CHAT, SUBJECT), (JOIN_CHAT, "Made-up joined review")] {
            ensure(mock.add_meeting_chat(chat, topic, &[LAB_B, LAB_A, LAB_C]), || {
                format!("no meeting chat {chat}")
            })?;
        }
        let last = calendar.len().saturating_sub(1);
        for (index, events) in calendar.into_iter().enumerate() {
            let answer = json!({"type": "Microsoft.SkypeSpaces.MiddleTier.Models.CalendarEvent", "value": events});
            let times = if index == last { CALENDAR_READS } else { 1 };
            for _ in 0..times {
                runtime.block_on(mock.answer_next("GET", "calendarView", 200, &answer));
            }
        }
        Ok(Self {
            _runtime: runtime,
            mock,
            trouter,
            relay,
            dir,
        })
    }

    fn child_env(&self, dir: &Path, reminders_off: bool) -> SandboxResult<Vec<(&'static str, String)>> {
        Ok(vec![
            (CHILD_ENV, "app".to_owned()),
            (ORIGIN_ENV, self.mock.origin()),
            (TROUTER_ENV, self.trouter.origin().to_owned()),
            (RELAY_ENV, self.relay.to_string()),
            (IC3_ENV, self.mock.token(LAB_A).ok_or("the mock's token")?),
            (SKYPE_ENV, self.mock.skypetoken(LAB_A).ok_or("the mock's skypetoken")?),
            (DIR_ENV, dir.to_string_lossy().into_owned()),
            (REMINDERS_OFF_ENV, if reminders_off { "1" } else { "0" }.to_owned()),
        ])
    }

    /// b pushes a meeting's start to a's calling endpoint, as the calling service does (`evt` 128, its `gp` base64
    /// JSON).
    fn push(&self, kind: &str, thread: &str, call: &str, subject: Option<&str>) {
        let mut payload = json!({
            "_synthetic": true,
            "notificationType": kind,
            "callId": call,
            "threadId": thread,
            "messageId": "0",
            "groupCallInitiator": LAB_B,
            "groupCallInitiatorDisplayName": "Lab B",
            "meetingInfo": {"tenantId": TENANT, "organizerId": LAB_B.rsplit(':').next().unwrap_or_default()},
        });
        if let Some(subject) = subject {
            payload["subject"] = json!(subject);
        }
        let gp = base64::engine::general_purpose::STANDARD.encode(payload.to_string());
        let _ = self.trouter.deliver_call(LAB_A, "", &json!({"evt": 128, "gp": gp}));
    }

    /// b's call start (or end) as the chat's `Event/Call` message; its id.
    fn chat_call(&self, chat: &str, call: &str, started: bool, huddle: bool) -> SandboxResult<String> {
        let content = if started {
            start_content(call, huddle)
        } else {
            end_content(call, huddle)
        };
        Ok(self
            .mock
            .add_message(chat, (LAB_B, "Lab B"), "Event/Call", &content)
            .ok_or_else(|| format!("no chat {chat}"))?)
    }
}

fn start_content(call: &str, huddle: bool) -> String {
    let kind = if huddle {
        "<conversationType>\"huddleGroupCall\"</conversationType>"
    } else {
        ""
    };
    format!(
        "<partlist type=\"started\" alt=\"\"><part identity=\"{LAB_B}\"><name>{LAB_B}</name></part></partlist>\
         {kind}<callEventType>callStarted</callEventType>\
         <conversationUrl>{LINK}</conversationUrl><callId>{call}</callId>"
    )
}

fn end_content(call: &str, huddle: bool) -> String {
    let kind = if huddle {
        "<conversationType>\"huddleGroupCall\"</conversationType>"
    } else {
        ""
    };
    format!(
        "<ended/><partlist alt=\"\" count=\"0\"></partlist>{kind}\
         <callEventType>callEnded</callEventType><conversationUrl>{LINK}</conversationUrl><callId>{call}</callId>"
    )
}

/// The child app in the sandbox: its process, its trace and its next question's id.
struct Child {
    running: Running,
    trace: TraceReader,
    next: u64,
}

impl Child {
    /// a's app on its store and settings in `dir`.
    fn start(sandbox: &Sandbox, world: &World, dir: &Path, reminders_off: bool) -> SandboxResult<Self> {
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let env = world.child_env(dir, reminders_off)?;
        let mut all: Vec<(&str, &str)> = env.iter().map(|(key, value)| (*key, value.as_str())).collect();
        all.push((TRACE_FD_ENV, &fd));
        let log = std::env::var(LOG_ENV).unwrap_or_default();
        if !log.is_empty() {
            all.push((LOG_ENV, &log));
        }
        let _ = fs::remove_file(sandbox.path(ASK));
        let child = sandbox.spawn_with_fds(&std::env::current_exe()?, &[], &all, &[writer.as_fd()])?;
        drop(writer);
        let mut app = Self {
            running: Running::new(child)?,
            trace: TraceReader::start(reader),
            next: 0,
        };
        app.running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| app.running.fail(&format!("no ready line: {error:?}")))?;
        app.trace
            .since(0, "status connected", |line| is(line, "status", "connected"))?;
        app.trace
            .since(0, "calling registered", |line| is(line, "calling", "registered"))?;
        thread::sleep(Duration::from_secs(2));
        Ok(app)
    }

    /// Asks the child `question` (`<verb> <rest>`); its answer.
    fn ask(&mut self, sandbox: &Sandbox, question: &str) -> SandboxResult<Value> {
        self.next += 1;
        let id = self.next;
        fs::write(sandbox.path(ASK), format!("{id} {question}"))?;
        let prefix = format!("answer {id} ");
        let line = self
            .running
            .line_where(WAIT, |line| line.starts_with(&prefix))
            .map_err(|error| self.running.fail(&format!("no answer to {question}: {error:?}")))?;
        Ok(serde_json::from_str(&line[prefix.len()..])?)
    }

    /// How many trace lines so far are `view` `change` naming `chat`.
    fn traced(&mut self, view: &str, change: &str, chat: &str) -> usize {
        self.trace.drain();
        self.trace
            .seen
            .iter()
            .filter(|line| is(line, view, change) && line.to_string().contains(chat))
            .count()
    }

    fn quit(mut self, sandbox: &Sandbox) -> SandboxResult {
        fs::write(sandbox.path(ASK), "0 quit")?;
        self.running
            .line_where(WAIT, |line| line == "quit")
            .map_err(|error| self.running.fail(&format!("didn't quit: {error:?}")))?;
        let (status, stderr) = self.running.finish(WAIT)?;
        ensure(status.success(), || format!("exited {status}: {stderr}"))
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

    /// Takes every line that has come so far into `seen`.
    fn drain(&mut self) {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
    }

    fn since(&mut self, mark: usize, what: &str, found: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        self.drain();
        if let Some(line) = self.seen[mark..].iter().find(|line| found(line)) {
            return Ok(line.clone());
        }
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .map_err(|_| format!("no trace line {what}"))?;
            let hit = found(&line);
            self.seen.push(line.clone());
            if hit {
                return Ok(line);
            }
        }
    }
}

fn is(line: &Value, view: &str, change: &str) -> bool {
    line["view"] == view && line["change"] == change
}

/// The checks' view of a run: the sandbox, the mock, the app and the tally.
struct Qa<'a> {
    sandbox: &'a Sandbox,
    world: &'a World,
    app: Option<Child>,
    input: VirtualInput,
    passed: Vec<&'static str>,
    failed: Vec<(&'static str, String)>,
    /// When the reminders run's app was ready.
    ready: Instant,
    /// When a reminder was snoozed, by its subject.
    snoozed: Vec<(&'static str, Instant)>,
    /// What the app started 3 minutes late showed: desktop banners, and banners traced shown (cards).
    late: Option<(usize, usize)>,
}

impl<'a> Qa<'a> {
    fn new(sandbox: &'a Sandbox, world: &'a World, app: Option<Child>) -> SandboxResult<Self> {
        Ok(Self {
            sandbox,
            world,
            app,
            input: sandbox.input()?,
            passed: Vec::new(),
            failed: Vec::new(),
            ready: Instant::now(),
            snoozed: Vec::new(),
            late: None,
        })
    }

    fn check(&mut self, name: &'static str, flow: Run) {
        let outcome = flow(self);
        match outcome {
            Ok(()) => {
                say(&format!("qa: ok {name}"));
                self.passed.push(name);
            }
            Err(error) => {
                say(&format!("qa: FAILED {name}: {error}"));
                let _ = self.shot(&format!("failed-{name}"));
                self.failed.push((name, error.to_string()));
            }
        }
    }

    fn ask(&mut self, question: &str) -> SandboxResult<Value> {
        let app = self.app.as_mut().ok_or("no app runs")?;
        app.ask(self.sandbox, question)
    }

    /// Screenshots the window as `qa664-<name>.png` under `target/ui-shots/` (and the evidence dir).
    fn shot(&self, name: &str) -> SandboxResult<PathBuf> {
        thread::sleep(SETTLE);
        let file = format!("qa664-{name}.png");
        let png = self.sandbox.screenshot(&file, true)?;
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .parent()
            .map_or_else(|| PathBuf::from("target"), Path::to_owned)
            .join("ui-shots");
        fs::create_dir_all(&dir)?;
        fs::copy(&png, dir.join(&file))?;
        if let Some(evidence) = std::env::var_os(EVIDENCE_ENV) {
            let evidence = PathBuf::from(evidence);
            fs::create_dir_all(&evidence)?;
            fs::copy(&png, evidence.join(&file))?;
        }
        say(&format!("qa: shot {file}"));
        Ok(png)
    }

    fn notices(&self) -> Vec<Notification> {
        self.sandbox.bus().notifications()
    }

    /// The notices since `seen` whose title is `summary`.
    fn notices_of(&self, seen: usize, summary: &str) -> Vec<Notification> {
        self.notices()
            .into_iter()
            .skip(seen)
            .filter(|notice| notice.summary == summary)
            .collect()
    }

    /// The started meetings' banners since `seen` (those saying someone started one, or waits).
    fn banners(&self, seen: usize) -> Vec<Notification> {
        self.notices()
            .into_iter()
            .skip(seen)
            .filter(|notice| notice.body.contains(" started ") || notice.body.contains(" lobby"))
            .collect()
    }

    fn banner_after(&self, seen: usize, what: &str) -> SandboxResult<Notification> {
        wait_until(BANNER_WAIT, || self.banners(seen).first().cloned())
            .ok_or_else(|| format!("no banner for {what}").into())
    }

    fn closed(&self, id: u32, what: &str) -> SandboxResult {
        wait_until(BANNER_WAIT, || {
            self.sandbox.bus().closed_notifications().contains(&id).then_some(())
        })
        .ok_or_else(|| format!("the notice stayed {what}").into())
    }

    /// The window's state: the chat open, the pre-join's chat.
    fn ui(&mut self) -> SandboxResult<Value> {
        self.ask("ui")
    }

    fn prejoin_until(&mut self, chat: &str) -> SandboxResult {
        let deadline = Instant::now() + BANNER_WAIT * 2;
        loop {
            let ui = self.ui()?;
            if ui["prejoin"] == chat {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("no pre-join of {chat}: {ui}").into());
            }
            thread::sleep(Duration::from_millis(300));
        }
    }

    /// The titles and stages of the reminders' cards.
    /// The reminders' cards: each one's title and what it says of when the meeting starts.
    fn cards(&mut self) -> SandboxResult<Vec<(String, String)>> {
        let cards = self.ask("cards")?;
        Ok(cards["cards"]
            .as_array()
            .map(|cards| {
                cards
                    .iter()
                    .map(|card| (text(card, "title"), text(card, "when")))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Waits until the card titled `title` holds desktop notice `id`.
    fn card_has_notice(&mut self, title: &str, id: u32) -> SandboxResult {
        let deadline = Instant::now() + BANNER_WAIT;
        loop {
            let cards = self.ask("cards")?;
            let held = cards["cards"].as_array().is_some_and(|cards| {
                cards
                    .iter()
                    .any(|card| text(card, "title") == title && card["notice"].as_u64() == Some(u64::from(id)))
            });
            if held {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("the card {title:?} doesn't hold notice {id}: {cards}").into());
            }
            thread::sleep(Duration::from_millis(300));
        }
    }

    /// How many times the app traced the reminder of the meeting in `chat` shown.
    fn reminded(&mut self, chat: &str) -> usize {
        self.app
            .as_mut()
            .map_or(0, |app| app.traced("meeting-reminder", "shown", chat))
    }

    /// Waits until the reminder of the meeting in `chat` was traced shown `times` times; when it was.
    fn reminded_times(&mut self, chat: &str, times: usize, wait: Duration) -> SandboxResult<Instant> {
        let deadline = Instant::now() + wait;
        loop {
            if self.reminded(chat) >= times {
                return Ok(Instant::now());
            }
            if Instant::now() > deadline {
                return Err(format!("{chat} reminded {} times, not {times}", self.reminded(chat)).into());
            }
            thread::sleep(Duration::from_millis(500));
        }
    }

    fn click(&mut self, (x, y): (u32, u32)) -> SandboxResult {
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(SETTLE);
        Ok(())
    }
}

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_owned()
}

fn offers(notice: &Notification, id: &str, label: &str) -> bool {
    notice
        .actions
        .chunks(2)
        .any(|pair| pair.first().map(String::as_str) == Some(id) && pair.get(1).map(String::as_str) == Some(label))
}

/// The calendar's form of a time: `2026-11-13T10:00:00.0000000Z`.
fn iso(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%S.0000000Z").to_string()
}

/// A made-up Teams meeting on a's calendar, `start` seconds from now (before: negative), `minutes` long, its chat
/// `chat`, organized by b.
fn event(id: &str, subject: &str, start: i64, minutes: i64, chat: &str) -> Value {
    let now = chrono::Utc::now();
    let link = format!(
        "https://teams.microsoft.com/l/meetup-join/{}/0?context=%7b%22Tid%22%3a%22{TENANT}%22%2c%22Oid%22%3a%22{}%22%7d",
        chat.replace(':', "%3a").replace('@', "%40"),
        LAB_B.rsplit(':').next().unwrap_or_default(),
    );
    let starts = now + chrono::Duration::seconds(start);
    json!({
        "objectId": id, "subject": subject,
        "startTime": iso(starts), "endTime": iso(starts + chrono::Duration::minutes(minutes)),
        "isAllDayEvent": false, "eventType": "Single", "isCancelled": false, "isOnlineMeeting": true,
        "isOrganizer": false, "myResponseType": "Accepted", "showAs": "Busy",
        "organizerName": "Lab B", "organizerAddress": "lab-b@lab.example.com",
        "location": "Microsoft Teams Meeting", "isPrivate": false,
        "attendees": [{"name": "Lab B", "address": "lab-b@lab.example.com"}, {"name": "Lab A", "address": "a@example.com"}],
        "skypeTeamsMeetingUrl": link,
        "skypeTeamsData": json!({"cid": chat, "type": 0}).to_string(),
    })
}

/// [`event`] with its own reminder `minutes` before its start (`None`: reminders off).
fn reminded(mut event: Value, minutes: Option<u32>) -> Value {
    event["isReminderSet"] = json!(minutes.is_some());
    event["reminderMinutesBeforeStart"] = json!(minutes.unwrap_or(15));
    event
}

// --- the calling push ---

fn push_calendar() -> Vec<Vec<Value>> {
    vec![vec![reminded(
        event("AAMkQaPushed", SUBJECT, -60, 30, MEETING_CHAT),
        None,
    )]]
}

const PUSH_CHECKS: [Check; 7] = [
    (
        "pushed_start_without_a_chat_message_shows_a_banner_join_opens_the_prejoin",
        pushed_start,
    ),
    ("pushed_start_message_opens_the_meeting_chat", pushed_message),
    ("push_then_chat_start_of_one_call_show_one_banner", push_then_chat),
    ("chat_start_then_push_of_one_call_show_one_banner", chat_then_push),
    ("pushed_lobby_wait_says_who_waits", lobby),
    ("pushed_meet_now_in_a_group_chat_shows_one_banner", huddle),
    ("pointer_click_on_the_card_s_join_opens_the_prejoin", card_join),
];

/// b starts the meeting; only the calling service tells a (no chat message): "Lab B started the meeting", titled with
/// the subject, Join and Message; the notice's Join opens the pre-join.
fn pushed_start(qa: &mut Qa<'_>) -> SandboxResult {
    let seen = qa.notices().len();
    qa.world.push("MeetingStart", MEETING_CHAT, "qa-call-1", Some(SUBJECT));
    let banner = qa.banner_after(seen, "the pushed start")?;
    say(&format!(
        "qa: pushed start: {:?}: {:?} {:?}",
        banner.summary, banner.body, banner.actions
    ));
    ensure(
        banner.summary == SUBJECT && banner.body == "Lab B started the meeting",
        || format!("the banner: {:?}: {:?}", banner.summary, banner.body),
    )?;
    ensure(
        offers(&banner, "join", "Join") && offers(&banner, "default", "Message"),
        || format!("the banner's actions: {:?}", banner.actions),
    )?;
    qa.shot("push-banner-light")?;
    qa.ask("theme dark")?;
    let dark = qa.shot("push-banner-dark");
    qa.ask("theme light")?;
    dark?;
    qa.sandbox.bus().invoke_action(banner.id, "join")?;
    let prejoin = qa.prejoin_until(MEETING_CHAT);
    qa.shot("push-join-prejoin")?;
    qa.ask("cancel-prejoin")?;
    prejoin?;
    qa.closed(banner.id, "after its Join")?;
    qa.world.chat_call(MEETING_CHAT, "qa-call-1", false, false)?;
    Ok(())
}

/// The banner's Message opens the meeting's chat.
fn pushed_message(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ask("chats")?;
    let seen = qa.notices().len();
    qa.world.push("MeetingStart", MEETING_CHAT, "qa-call-2", Some(SUBJECT));
    let banner = qa.banner_after(seen, "the pushed start")?;
    qa.sandbox.bus().invoke_action(banner.id, "default")?;
    let deadline = Instant::now() + BANNER_WAIT;
    let mut ui = qa.ui()?;
    while ui["chat"] != MEETING_CHAT && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(300));
        ui = qa.ui()?;
    }
    qa.shot("push-message-opens-chat")?;
    ensure(ui["chat"] == MEETING_CHAT, || format!("Message opened {ui}"))?;
    qa.closed(banner.id, "after Message")?;
    qa.world.chat_call(MEETING_CHAT, "qa-call-2", false, false)?;
    Ok(())
}

fn one_banner(qa: &Qa<'_>, seen: usize, what: &str) -> SandboxResult {
    qa.banner_after(seen, what)?;
    thread::sleep(BANNER_WAIT);
    let banners = qa.banners(seen);
    ensure(banners.len() == 1, || {
        format!("{} banners for {what}: {banners:?}", banners.len())
    })
}

fn push_then_chat(qa: &mut Qa<'_>) -> SandboxResult {
    let seen = qa.notices().len();
    qa.world.push("MeetingStart", MEETING_CHAT, "qa-call-3", Some(SUBJECT));
    thread::sleep(Duration::from_secs(2));
    qa.world.chat_call(MEETING_CHAT, "qa-call-3", true, false)?;
    let one = one_banner(qa, seen, "the push, then the chat's start");
    qa.world.chat_call(MEETING_CHAT, "qa-call-3", false, false)?;
    one
}

fn chat_then_push(qa: &mut Qa<'_>) -> SandboxResult {
    let seen = qa.notices().len();
    qa.world.chat_call(MEETING_CHAT, "qa-call-4", true, false)?;
    thread::sleep(Duration::from_secs(2));
    qa.world.push("MeetingStart", MEETING_CHAT, "qa-call-4", Some(SUBJECT));
    let one = one_banner(qa, seen, "the chat's start, then the push");
    qa.world.chat_call(MEETING_CHAT, "qa-call-4", false, false)?;
    one
}

fn lobby(qa: &mut Qa<'_>) -> SandboxResult {
    let seen = qa.notices().len();
    qa.world
        .push("MeetingStartFromLobby", MEETING_CHAT, "qa-call-5", Some(SUBJECT));
    let banner = qa.banner_after(seen, "the lobby's push")?;
    say(&format!(
        "qa: lobby: {:?}: {:?} {:?}",
        banner.summary, banner.body, banner.actions
    ));
    qa.shot("push-lobby")?;
    ensure(
        banner.body == "Lab B is waiting in the lobby. Join the meeting to let them in."
            && offers(&banner, "join", "Join"),
        || format!("the lobby's banner: {:?} {:?}", banner.body, banner.actions),
    )?;
    qa.sandbox.bus().invoke_action(banner.id, "dismiss").ok();
    Ok(())
}

/// b's Meet now in the a–b–c chat: the push and the chat's start show one banner, "Lab B started a meeting".
fn huddle(qa: &mut Qa<'_>) -> SandboxResult {
    let seen = qa.notices().len();
    qa.world.push("HuddleMeetingStart", ABC_CHAT, "qa-call-6", None);
    thread::sleep(Duration::from_secs(1));
    qa.world.chat_call(ABC_CHAT, "qa-call-6", true, true)?;
    let banner = qa.banner_after(seen, "the Meet now")?;
    say(&format!(
        "qa: Meet now: {:?}: {:?} {:?}",
        banner.summary, banner.body, banner.actions
    ));
    let one = one_banner(qa, seen, "the Meet now's push and its chat's start");
    qa.world.chat_call(ABC_CHAT, "qa-call-6", false, true)?;
    ensure(banner.body == "Lab B started a meeting", || {
        format!("the Meet now's banner says {:?}", banner.body)
    })?;
    one
}

/// The pointer clicks the started meeting's card's Join (found by its fill): the pre-join opens.
fn card_join(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ask("chats")?;
    // the earlier cards go after their time: one card is left to click
    thread::sleep(Duration::from_secs(21));
    let seen = qa.notices().len();
    qa.world.push("MeetingStart", MEETING_CHAT, "qa-call-7", Some(SUBJECT));
    let banner = qa.banner_after(seen, "the pushed start")?;
    let png = qa.shot("push-card-before-click")?;
    let at = filled_button(&png, (SCREEN.0 - 420, SCREEN.1 - 320)).ok_or("no Join button on a card")?;
    say(&format!("qa: the card's Join at {at:?}"));
    qa.click(at)?;
    let prejoin = qa.prejoin_until(MEETING_CHAT);
    qa.shot("push-card-join-prejoin")?;
    qa.ask("cancel-prejoin")?;
    qa.world.chat_call(MEETING_CHAT, "qa-call-7", false, false)?;
    prejoin?;
    qa.closed(banner.id, "after the card's Join")
}

/// The middle of a button filled with the brand's blue-violet in the screenshot `png`, right and below `from`: rows
/// with a run of that hue 30 to 200 px long, in the lowest band of such rows that spans 14 to 40 rows (a border or a
/// focus line, such as the message box's, is a band of one or two rows; the card's mark is tinted pale and its glyph's
/// strokes are short; the button's label breaks its middle rows; the window's frame below it runs the whole width).
fn filled_button(png: &Path, from: (u32, u32)) -> Option<(u32, u32)> {
    let image = image::open(png).ok()?.to_rgb8();
    let near = |pixel: &image::Rgb<u8>| {
        let [red, green, blue] = pixel.0.map(u16::from);
        blue >= 150 && blue >= red + 40 && blue >= green + 40
    };
    let longest = |y: u32| {
        let (mut best, mut start, mut run) = ((0, 0), 0, 0);
        for x in from.0..image.width() {
            run = if near(image.get_pixel(x, y)) { run + 1 } else { 0 };
            if run == 1 {
                start = x;
            }
            if run > best.1 {
                best = (start, run);
            }
        }
        best
    };
    let rows: Vec<(u32, (u32, u32))> = (from.1..image.height())
        .map(|y| (y, longest(y)))
        .filter(|(_, (_, run))| (30..=200).contains(run))
        .collect();
    // bands of rows at most 12 apart (a button's label breaks its middle rows): a line's band is a row or two, a
    // button's 14 to 40
    let mut bands: Vec<Vec<(u32, (u32, u32))>> = Vec::new();
    for row in rows {
        match bands.last_mut() {
            Some(band) if band.last().is_some_and(|last| row.0 - last.0 <= 12) => band.push(row),
            _ => bands.push(vec![row]),
        }
    }
    let band = bands.into_iter().rev().find(|band| {
        let span = band.last().map_or(0, |last| last.0) - band.first().map_or(0, |first| first.0) + 1;
        band.len() >= 6 && (14..=40).contains(&span)
    })?;
    let (first, last) = (band.first()?.0, band.last()?.0);
    say(&format!(
        "qa: {}×{} px, the button's rows {first}–{last}",
        image.width(),
        image.height()
    ));
    let (x, width) = band.iter().map(|(_, run)| *run).max_by_key(|(_, width)| *width)?;
    Some((x + width / 2, u32::midpoint(first, last)))
}

// --- the app started late ---

fn late_calendar() -> Vec<Vec<Value>> {
    vec![vec![
        reminded(event("AAMkQaLateThree", SUBJECT, -180, 30, MEETING_CHAT), None),
        reminded(
            event("AAMkQaLateSeven", "Made-up late review", -420, 30, JOIN_CHAT),
            None,
        ),
    ]]
}

const LATE_CHECKS: [Check; 3] = [
    ("app_started_three_minutes_after_the_start_shows_a_banner", late_three),
    (
        "app_started_three_minutes_after_the_start_shows_the_desktop_notice",
        late_three_desktop,
    ),
    ("app_started_seven_minutes_after_the_start_shows_none", late_seven),
];

/// a's app runs once and quits; b starts `chat`'s meeting `minutes` ago; a's app starts again on its store (a
/// catch-up brings the start): its desktop notices of started meetings, and how many banners it traced shown.
fn started_late(qa: &mut Qa<'_>, chat: &str, call: &str, minutes: i64) -> SandboxResult<(Vec<Notification>, usize)> {
    let dir = tempfile::tempdir_in(qa.world.dir.path())?;
    Child::start(qa.sandbox, qa.world, dir.path(), false)?.quit(qa.sandbox)?;
    let id = qa.world.chat_call(chat, call, true, false)?;
    let stamp = iso(chrono::Utc::now() - chrono::Duration::minutes(minutes));
    let client = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
    let changed = qa.world.mock.change_message(chat, &client, |message| {
        message["originalarrivaltime"] = json!(stamp);
        message["composetime"] = json!(stamp);
    });
    ensure(changed, || format!("the start {id} wasn't changed"))?;
    // the start reaches a only by the catch-up, as for an app that wasn't running
    let _ = qa.world.mock.take_events(LAB_A);
    qa.world.trouter.clear_stored_deliveries(LAB_A);
    let seen = qa.notices().len();
    qa.app = Some(Child::start(qa.sandbox, qa.world, dir.path(), false)?);
    thread::sleep(BANNER_WAIT * 2);
    let banners = qa.banners(seen);
    let cards = qa
        .app
        .as_mut()
        .map_or(0, |app| app.traced("meeting-notification", "shown", chat));
    say(&format!(
        "qa: started {minutes} min late: {cards} shown, desktop {banners:?}"
    ));
    if minutes == 3 {
        qa.late = Some((banners.len(), cards));
    }
    qa.shot(&format!("late-{minutes}"))?;
    qa.world.chat_call(chat, call, false, false)?;
    thread::sleep(Duration::from_secs(1));
    if let Some(app) = qa.app.take() {
        app.quit(qa.sandbox)?;
    }
    Ok((banners, cards))
}

/// The window shows the banner (its card), 3 minutes after the start.
fn late_three(qa: &mut Qa<'_>) -> SandboxResult {
    started_late(qa, MEETING_CHAT, "qa-late-3", 3)?;
    let (_, shown) = qa.late.ok_or("no late start")?;
    ensure(shown == 1, || {
        format!("{shown} banners shown 3 minutes after the start")
    })
}

/// The desktop's notice of that banner comes too, as for any other banner.
fn late_three_desktop(qa: &mut Qa<'_>) -> SandboxResult {
    let (desktop, shown) = qa.late.ok_or("no late start")?;
    ensure(desktop == 1, || {
        format!(
            "{desktop} desktop notices for the {shown} banners shown at the app's start, 3 minutes after the meeting's"
        )
    })
}

fn late_seven(qa: &mut Qa<'_>) -> SandboxResult {
    let (banners, shown) = started_late(qa, JOIN_CHAT, "qa-late-7", 7)?;
    ensure(banners.is_empty() && shown == 0, || {
        format!("7 minutes after the start: {shown} shown, desktop {banners:?}")
    })
}

// --- the reminders ---

const DEFAULT_SUBJECT: &str = "Made-up default reminder";
const FIVE_SUBJECT: &str = "Made-up five-minute reminder";
const THIRTY_SUBJECT: &str = "Made-up thirty-minute reminder";
const ONE_SUBJECT: &str = "Made-up one-minute snooze";
const LATE_SUBJECT: &str = "Made-up late reminder";
const SOON_SUBJECT: &str = "Made-up meeting soon";
const DISMISSED_SUBJECT: &str = "Made-up dismissed early";
const JOINED_SUBJECT: &str = "Made-up joined early";
/// Events that never remind in the run.
const NEVER: [&str; 7] = [
    "Made-up later meeting",
    "Made-up declined meeting",
    "Made-up cancelled meeting",
    "Made-up all-day meeting",
    "Made-up offline meeting",
    "Made-up long-started meeting",
    "Made-up reminder off",
];

fn reminder_calendar() -> Vec<Vec<Value>> {
    let mut events = reminded_events();
    events.extend(never_events());
    vec![events]
}

/// The meetings the run reminds of: at their own time, at the default, late, and soon after the start.
fn reminded_events() -> Vec<Value> {
    let mut default = event("AAMkQaDefault", DEFAULT_SUBJECT, 14 * 60, 30, DEFAULT_CHAT);
    if let Some(fields) = default.as_object_mut() {
        fields.remove("isReminderSet");
        fields.remove("reminderMinutesBeforeStart");
    }
    let soon = |id: &str, subject: &str, chat: &str| reminded(event(id, subject, 200, 30, chat), Some(2));
    vec![
        default,
        reminded(event("AAMkQaFive", FIVE_SUBJECT, 270, 30, FIVE_CHAT), Some(5)),
        reminded(
            event("AAMkQaThirty", THIRTY_SUBJECT, 25 * 60, 30, THIRTY_CHAT),
            Some(30),
        ),
        reminded(event("AAMkQaOne", ONE_SUBJECT, 12 * 60, 30, ONE_CHAT), Some(15)),
        reminded(event("AAMkQaLate", LATE_SUBJECT, -8 * 60, 30, LATE_CHAT), Some(15)),
        soon("AAMkQaSoon", SOON_SUBJECT, SOON_CHAT),
        soon("AAMkQaDismissed", DISMISSED_SUBJECT, DISMISSED_CHAT),
        soon("AAMkQaJoined", JOINED_SUBJECT, JOIN_CHAT),
    ]
}

/// The meetings the run never reminds of: later than their reminder, declined, cancelled, all day, offline, started
/// more than 10 minutes ago, and one whose reminder is off.
fn never_events() -> Vec<Value> {
    let mut declined = reminded(event("AAMkQaDeclined", NEVER[1], 600, 30, JOIN_CHAT), Some(15));
    declined["myResponseType"] = json!("Declined");
    let mut cancelled = reminded(event("AAMkQaCancelled", NEVER[2], 600, 30, JOIN_CHAT), Some(15));
    cancelled["isCancelled"] = json!(true);
    let mut all_day = reminded(event("AAMkQaAllDay", NEVER[3], 600, 30, JOIN_CHAT), Some(15));
    if let Some(today) = chrono::Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .map(|at| at.and_utc())
    {
        all_day["startTime"] = json!(iso(today));
        all_day["endTime"] = json!(iso(today + chrono::Duration::days(1)));
    }
    all_day["isAllDayEvent"] = json!(true);
    let mut offline = reminded(event("AAMkQaOffline", NEVER[4], 600, 30, JOIN_CHAT), Some(15));
    offline["isOnlineMeeting"] = json!(false);
    if let Some(fields) = offline.as_object_mut() {
        fields.remove("skypeTeamsMeetingUrl");
        fields.remove("skypeTeamsData");
    }
    offline["location"] = json!("Made-up room 1");
    vec![
        reminded(
            event(
                "AAMkQaLater",
                NEVER[0],
                20 * 60,
                30,
                "19:meeting_MadeUpQaLater@thread.v2",
            ),
            Some(5),
        ),
        declined,
        cancelled,
        all_day,
        offline,
        reminded(
            event(
                "AAMkQaLong",
                NEVER[5],
                -12 * 60,
                30,
                "19:meeting_MadeUpQaLong@thread.v2",
            ),
            Some(15),
        ),
        reminded(
            event("AAMkQaOff", NEVER[6], 5 * 60, 30, "19:meeting_MadeUpQaOff@thread.v2"),
            None,
        ),
    ]
}

/// The made-up meetings' chats, each reminded meeting its own.
const DEFAULT_CHAT: &str = "19:meeting_MadeUpQaDefault@thread.v2";
const FIVE_CHAT: &str = "19:meeting_MadeUpQaFive@thread.v2";
const THIRTY_CHAT: &str = "19:meeting_MadeUpQaThirty@thread.v2";
const ONE_CHAT: &str = "19:meeting_MadeUpQaOne@thread.v2";
const LATE_CHAT: &str = "19:meeting_MadeUpQaLate@thread.v2";
const SOON_CHAT: &str = "19:meeting_MadeUpQaSoon@thread.v2";
const DISMISSED_CHAT: &str = "19:meeting_MadeUpQaDismissed@thread.v2";
/// The reminders due when the app starts: each one's title and what its card says first.
const AT_START: [(&str, &str); 5] = [
    (DEFAULT_SUBJECT, "Starts in 1"),
    (FIVE_SUBJECT, "Starts in "),
    (THIRTY_SUBJECT, "Starts in 2"),
    (ONE_SUBJECT, "Starts in 1"),
    (LATE_SUBJECT, "Starting now"),
];

const REMINDER_CHECKS: [Check; 16] = [
    (
        "reminders_due_at_start_show_as_cards_at_their_own_time_the_default_and_late",
        start_cards,
    ),
    ("reminders_due_at_start_show_desktop_notices", start_notices),
    (
        "no_reminder_for_later_declined_cancelled_all_day_offline_long_started_or_off",
        no_reminders,
    ),
    ("cards_in_light_dark_a_larger_font_and_narrow_windows", card_shots),
    (
        "many_cards_at_a_larger_font_stay_whole_below_the_chat_header",
        cards_at_font_20,
    ),
    (
        "cards_leave_the_compose_box_free_and_fit_narrow_windows",
        cards_clear_of_compose,
    ),
    ("card_snoozes_and_dismiss_close_the_cards", card_actions),
    ("card_snooze_one_minute_brings_it_back_after_a_minute", one_minute_back),
    (
        "desktop_notice_has_times_join_snooze_dismiss_and_no_expiry",
        notice_shape,
    ),
    ("desktop_notice_snooze_closes_it", notice_snooze),
    ("reminder_before_the_start_desktop_dismiss_and_join", soon_reminders),
    ("second_reminder_starting_now_unless_dismissed_or_joined", starting_now),
    ("card_snooze_until_it_starts_brings_it_back_at_the_start", until_start),
    (
        "card_snooze_five_minutes_brings_it_back_after_five_minutes",
        default_back,
    ),
    ("desktop_snooze_brings_it_back_after_five_minutes", notice_snooze_back),
    ("never_twice_for_one_occasion", never_twice),
];

/// The first reminder of `subject` since `seen` on the desktop, waiting for it.
fn reminder_after(qa: &Qa<'_>, seen: usize, subject: &str, wait: Duration) -> SandboxResult<Notification> {
    wait_until(wait, || qa.notices_of(seen, subject).first().cloned())
        .ok_or_else(|| format!("no desktop reminder of {subject:?}").into())
}

/// The reminders due at the app's start show as the window's cards, each saying when its meeting starts.
fn start_cards(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ready = Instant::now();
    let deadline = Instant::now() + REMINDER_WAIT;
    let mut cards = qa.cards()?;
    while cards.len() < AT_START.len() && Instant::now() < deadline {
        thread::sleep(Duration::from_secs(1));
        cards = qa.cards()?;
    }
    say(&format!("qa: cards at the start {cards:?}"));
    let wrong: Vec<String> = AT_START
        .iter()
        .filter(|(subject, when)| {
            !cards
                .iter()
                .any(|(title, said)| title == subject && said.starts_with(when))
        })
        .map(|(subject, when)| format!("no card of {subject} saying {when:?}"))
        .collect();
    ensure(wrong.is_empty(), || format!("{}; cards {cards:?}", wrong.join("; ")))
}

/// Each of them shows on the desktop too.
fn start_notices(qa: &mut Qa<'_>) -> SandboxResult {
    thread::sleep(Duration::from_secs(5));
    let missing: Vec<&str> = AT_START
        .iter()
        .map(|(subject, _)| *subject)
        .filter(|subject| qa.notices_of(0, subject).is_empty())
        .collect();
    ensure(missing.is_empty(), || format!("no desktop notice of {missing:?}"))
}

fn no_reminders(qa: &mut Qa<'_>) -> SandboxResult {
    thread::sleep(Duration::from_secs(20));
    let mut shown: Vec<String> = qa
        .notices()
        .into_iter()
        .filter(|notice| NEVER.contains(&notice.summary.as_str()))
        .map(|notice| notice.summary)
        .collect();
    shown.extend(
        qa.cards()?
            .into_iter()
            .map(|(title, _)| title)
            .filter(|title| NEVER.contains(&title.as_str())),
    );
    ensure(shown.is_empty(), || format!("reminders that shouldn't show: {shown:?}"))
}

fn card_shots(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ask("chats")?;
    qa.shot("reminders-light")?;
    qa.ask("theme dark")?;
    qa.shot("reminders-dark")?;
    qa.ask("theme light")?;
    qa.ask("font 20")?;
    qa.shot("reminders-font-20")?;
    qa.ask("font 14")?;
    for width in [640, 480, 400] {
        narrow(qa, width)?;
        qa.shot(&format!("reminders-width-{width}"))?;
    }
    narrow(qa, i32::try_from(SCREEN.0)?)?;
    Ok(())
}

/// Where the window drew the cards: the stack, the chat's messages area (between its header and its compose box),
/// the window's size, how many cards showed and how many "+N more" held back.
struct Layout {
    stack: [f64; 4],
    area: [f64; 4],
    viewport: [f64; 2],
    shown: u64,
    more: u64,
}

fn rect(value: &Value) -> Option<[f64; 4]> {
    let all: Vec<f64> = value.as_array()?.iter().filter_map(Value::as_f64).collect();
    all.try_into().ok()
}

fn layout(qa: &mut Qa<'_>) -> SandboxResult<Layout> {
    thread::sleep(SETTLE);
    let answer = qa.ask("layout")?;
    let (Some(stack), Some(area)) = (rect(&answer["stack"]), rect(&answer["area"])) else {
        return Err(format!("no cards or no chat drawn: {answer}").into());
    };
    let viewport = [
        answer["viewport"][0].as_f64().unwrap_or_default(),
        answer["viewport"][1].as_f64().unwrap_or_default(),
    ];
    Ok(Layout {
        stack,
        area,
        viewport,
        shown: answer["shown"].as_u64().unwrap_or_default(),
        more: answer["more"].as_u64().unwrap_or_default(),
    })
}

/// What is wrong with `layout`: cards over the chat's header or its compose box, or out of the window.
fn misplaced(layout: &Layout) -> Vec<String> {
    let [x, y, w, h] = layout.stack;
    let [_, area_y, _, area_h] = layout.area;
    let mut wrong = Vec::new();
    if y < area_y {
        wrong.push(format!(
            "the cards' top {y} is above the messages' top {area_y} (the chat's header)"
        ));
    }
    if y + h > area_y + area_h {
        wrong.push(format!(
            "the cards' bottom {} is below the messages' bottom {} (the compose box)",
            y + h,
            area_y + area_h
        ));
    }
    if x < 0.0 || x + w > layout.viewport[0] {
        wrong.push(format!(
            "the cards {x}..{} leave the window 0..{}",
            x + w,
            layout.viewport[0]
        ));
    }
    wrong
}

/// Five reminders at font 20: at most three cards and "+2 more", each whole between the chat's header and its compose
/// box, the stack not cut by its scroll.
fn cards_at_font_20(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ask("chats")?;
    qa.ask("font 20")?;
    let cards = qa.cards()?.len();
    let layout = layout(qa)?;
    qa.shot("reminders-font-20-placed")?;
    qa.ask("font 14")?;
    let mut wrong = misplaced(&layout);
    let shown = usize::try_from(layout.shown)?;
    if shown > teams_ui::shell::MAX_MEETING_CARDS || shown + usize::try_from(layout.more)? != cards {
        wrong.push(format!("{shown} cards shown and {} more, of {cards}", layout.more));
    }
    let room = layout.area[3] - 32.0;
    if layout.stack[3] >= room - 1.0 {
        wrong.push(format!(
            "the stack fills its room ({} of {room}): a card is cut",
            layout.stack[3]
        ));
    }
    ensure(wrong.is_empty(), || wrong.join("; "))
}

/// At the window's width and narrower ones: the cards leave the compose box's row free and stay in the window.
fn cards_clear_of_compose(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ask("chats")?;
    let mut wrong = Vec::new();
    for width in [SCREEN.0, 640, 480, 400] {
        narrow(qa, i32::try_from(width)?)?;
        let layout = layout(qa)?;
        qa.shot(&format!("reminders-placed-width-{width}"))?;
        wrong.extend(
            misplaced(&layout)
                .into_iter()
                .map(|why| format!("at {width} px: {why}")),
        );
    }
    narrow(qa, i32::try_from(SCREEN.0)?)?;
    ensure(wrong.is_empty(), || wrong.join("; "))
}

/// The compositor sizes the window `width` wide, as a user's drag does.
fn narrow(qa: &Qa<'_>, width: i32) -> SandboxResult {
    qa.sandbox
        .niri_msg(&["action", "set-column-width", &width.to_string()])?;
    thread::sleep(Duration::from_secs(2));
    Ok(())
}

/// The cards' Snooze (5 minutes, 1 minute, until it starts) and Dismiss: each card goes.
fn card_actions(qa: &mut Qa<'_>) -> SandboxResult {
    for (question, subject) in [
        (format!("snooze 5 {DEFAULT_SUBJECT}"), DEFAULT_SUBJECT),
        (format!("dismiss {THIRTY_SUBJECT}"), THIRTY_SUBJECT),
        (format!("snooze 1 {ONE_SUBJECT}"), ONE_SUBJECT),
        (format!("snooze 0 {FIVE_SUBJECT}"), FIVE_SUBJECT),
    ] {
        let done = qa.ask(&question)?;
        ensure(done.get("error").is_none(), || format!("{question}: {done}"))?;
        if question.starts_with("snooze") {
            qa.snoozed.push((subject, Instant::now()));
        }
    }
    thread::sleep(SETTLE);
    let cards = qa.cards()?;
    qa.shot("reminders-after-card-actions")?;
    let left: Vec<String> = cards
        .into_iter()
        .map(|(title, _)| title)
        .filter(|title| [DEFAULT_SUBJECT, THIRTY_SUBJECT, ONE_SUBJECT, FIVE_SUBJECT].contains(&title.as_str()))
        .collect();
    ensure(left.is_empty(), || format!("cards still shown: {left:?}"))
}

fn snoozed_at(qa: &Qa<'_>, subject: &str) -> SandboxResult<Instant> {
    qa.snoozed
        .iter()
        .rev()
        .find(|(snoozed, _)| *snoozed == subject)
        .map(|(_, at)| *at)
        .ok_or_else(|| format!("{subject} wasn't snoozed").into())
}

fn back_within(after: Duration, low: u64, high: u64) -> SandboxResult {
    ensure(
        after >= Duration::from_secs(low) && after <= Duration::from_secs(high),
        || format!("it came back after {after:?}, not {low}–{high} s"),
    )
}

fn one_minute_back(qa: &mut Qa<'_>) -> SandboxResult {
    let since = snoozed_at(qa, ONE_SUBJECT)?;
    let back = qa.reminded_times(ONE_CHAT, 2, Duration::from_secs(120))?;
    let after = back.duration_since(since);
    say(&format!("qa: the card's one-minute snooze came back after {after:?}"));
    let cards = qa.cards()?;
    ensure(cards.iter().any(|(title, _)| title == ONE_SUBJECT), || {
        format!("no card back: {cards:?}")
    })?;
    back_within(after, 55, 100)
}

/// The notice back from the one-minute snooze, waited for: the app traces "shown" before the bus has its notice.
fn one_minute_notice(qa: &Qa<'_>) -> SandboxResult<Notification> {
    wait_until(BANNER_WAIT, || qa.notices_of(0, ONE_SUBJECT).get(1).cloned())
        .ok_or_else(|| format!("no desktop reminder of {ONE_SUBJECT:?} after its snooze").into())
}

/// The desktop notice of the reminder back from its snooze: "Starts in N minutes", its times, Join, Snooze and
/// Dismiss, and it never expires.
fn notice_shape(qa: &mut Qa<'_>) -> SandboxResult {
    let notice = one_minute_notice(qa)?;
    say(&format!(
        "qa: desktop reminder: {:?} {:?} expire {} hints {:?}",
        notice.body, notice.actions, notice.expire_timeout, notice.hints
    ));
    let lines: Vec<&str> = notice.body.lines().collect();
    ensure(
        notice.body.starts_with("Starts in ")
            && lines.len() == 2
            && lines.get(1).is_some_and(|times| times.contains(" – "))
            && offers(&notice, "join", "Join")
            && offers(&notice, "snooze", "Snooze")
            && offers(&notice, "dismiss", "Dismiss")
            && notice.expire_timeout == 0,
        || {
            format!(
                "the notice: {:?} {:?} expire {}",
                notice.body, notice.actions, notice.expire_timeout
            )
        },
    )
}

fn notice_snooze(qa: &mut Qa<'_>) -> SandboxResult {
    let notice = one_minute_notice(qa)?;
    // the bus records a notice before the app has its id, and the app ignores an action on an id it doesn't have
    qa.card_has_notice(ONE_SUBJECT, notice.id)?;
    qa.sandbox.bus().invoke_action(notice.id, "snooze")?;
    qa.snoozed.push((ONE_SUBJECT, Instant::now()));
    qa.closed(notice.id, "after the notice's Snooze")?;
    thread::sleep(SETTLE);
    let cards = qa.cards()?;
    ensure(!cards.iter().any(|(title, _)| title == ONE_SUBJECT), || {
        format!("the card stayed: {cards:?}")
    })
}

/// The meetings 200 s after the mount remind 2 minutes before, on the desktop: "Starts in 2 minutes"; the notice's
/// Dismiss ends one (its card too); the notice's Join of another opens its pre-join.
fn soon_reminders(qa: &mut Qa<'_>) -> SandboxResult {
    let wait = Duration::from_secs(150);
    let soon = reminder_after(qa, 0, SOON_SUBJECT, wait)?;
    let dismissed = reminder_after(qa, 0, DISMISSED_SUBJECT, wait)?;
    let joined = reminder_after(qa, 0, JOINED_SUBJECT, wait)?;
    say(&format!(
        "qa: soon {:?}, at {:?} after ready",
        soon.body,
        qa.ready.elapsed()
    ));
    qa.shot("reminders-soon")?;
    before_start(qa, DISMISSED_SUBJECT, dismissed.id)?;
    qa.sandbox.bus().invoke_action(dismissed.id, "dismiss")?;
    qa.closed(dismissed.id, "after the notice's Dismiss")?;
    before_start(qa, JOINED_SUBJECT, joined.id)?;
    qa.sandbox.bus().invoke_action(joined.id, "join")?;
    let prejoin = qa.prejoin_until(JOIN_CHAT);
    // the pre-join (its window titled with the meeting's subject) comes to the front, over the main window its Join
    // showed
    let front = wait_until(BANNER_WAIT, || {
        focused_title(qa).ok().filter(|title| title == JOINED_SUBJECT)
    });
    let focused = focused_title(qa);
    qa.shot("reminders-join-prejoin")?;
    qa.ask("cancel-prejoin")?;
    prejoin?;
    ensure(front.is_some(), || {
        format!("the window in front after Join is {focused:?}, not the pre-join")
    })?;
    qa.closed(joined.id, "after the notice's Join")?;
    let cards = qa.cards()?;
    ensure(
        !cards
            .iter()
            .any(|(title, _)| title == DISMISSED_SUBJECT || title == JOINED_SUBJECT),
        || format!("cards after Dismiss and Join: {cards:?}"),
    )?;
    ensure(
        soon.body.starts_with("Starts in 2 minutes") || soon.body.starts_with("Starts in 1 minute"),
        || format!("the reminder before the start says {:?}", soon.body),
    )
}

/// Fails unless the card of `subject` holds notice `id` and both still say the meeting starts later: the
/// "Starting now" notice takes the id of the notice it replaces.
fn before_start(qa: &mut Qa<'_>, subject: &str, id: u32) -> SandboxResult {
    qa.card_has_notice(subject, id)?;
    let cards = qa.cards()?;
    let notices = qa.notices_of(0, subject);
    ensure(
        cards
            .iter()
            .any(|(title, when)| title == subject && when.starts_with("Starts in "))
            && !notices.iter().any(|notice| notice.body.starts_with("Starting now")),
        || format!("{subject:?} is no longer before its start: cards {cards:?}, notices {notices:?}"),
    )
}

/// The title of the window the compositor has in front.
fn focused_title(qa: &Qa<'_>) -> SandboxResult<String> {
    let focused: Value = serde_json::from_str(&qa.sandbox.niri_msg(&["-j", "focused-window"])?)?;
    Ok(text(&focused, "title"))
}

fn starting_now(qa: &mut Qa<'_>) -> SandboxResult {
    let left = Duration::from_secs(260).saturating_sub(qa.ready.elapsed());
    let now = wait_until(left, || {
        qa.notices_of(0, SOON_SUBJECT)
            .into_iter()
            .find(|notice| notice.body.starts_with("Starting now"))
    })
    .ok_or("no \"Starting now\" for the meeting soon")?;
    say(&format!(
        "qa: starting now {:?} (replaces {}) at {:?}",
        now.body,
        now.replaces_id,
        qa.ready.elapsed()
    ));
    qa.shot("reminders-starting-now")?;
    thread::sleep(Duration::from_secs(20));
    let again: Vec<String> = [DISMISSED_CHAT, JOIN_CHAT]
        .into_iter()
        .filter(|chat| qa.reminded(chat) > 1)
        .map(str::to_owned)
        .collect();
    ensure(again.is_empty(), || {
        format!("reminded again at the start after a Dismiss or a Join: {again:?}")
    })
}

fn until_start(qa: &mut Qa<'_>) -> SandboxResult {
    let left = Duration::from_secs(330).saturating_sub(qa.ready.elapsed());
    qa.reminded_times(FIVE_CHAT, 2, left)?;
    let cards = qa.cards()?;
    say(&format!(
        "qa: until it starts: back at {:?} after ready, cards {cards:?}",
        qa.ready.elapsed()
    ));
    ensure(
        cards
            .iter()
            .any(|(title, when)| title == FIVE_SUBJECT && when == "Starting now"),
        || format!("no \"Starting now\" card: {cards:?}"),
    )
}

fn default_back(qa: &mut Qa<'_>) -> SandboxResult {
    let since = snoozed_at(qa, DEFAULT_SUBJECT)?;
    let back = qa.reminded_times(
        DEFAULT_CHAT,
        2,
        Duration::from_secs(360).saturating_sub(since.elapsed()),
    )?;
    let after = back.duration_since(since);
    say(&format!("qa: the card's five-minute snooze came back after {after:?}"));
    qa.shot("reminders-snooze-back")?;
    back_within(after, 295, 340)
}

fn notice_snooze_back(qa: &mut Qa<'_>) -> SandboxResult {
    let since = snoozed_at(qa, ONE_SUBJECT)?;
    let back = qa.reminded_times(ONE_CHAT, 3, Duration::from_secs(360).saturating_sub(since.elapsed()))?;
    let after = back.duration_since(since);
    say(&format!("qa: the notice's snooze came back after {after:?}"));
    back_within(after, 295, 340)
}

/// Each meeting was shown once per stage and once per snooze's end, and no more, across the calendar's reads.
fn never_twice(qa: &mut Qa<'_>) -> SandboxResult {
    let counts = [
        (DEFAULT_CHAT, 2),
        (FIVE_CHAT, 2),
        (THIRTY_CHAT, 1),
        (ONE_CHAT, 3),
        (LATE_CHAT, 1),
        (SOON_CHAT, 2),
        (DISMISSED_CHAT, 1),
        (JOIN_CHAT, 1),
    ];
    let wrong: Vec<String> = counts
        .into_iter()
        .filter_map(|(chat, count)| {
            let shown = qa.reminded(chat);
            (shown != count).then(|| format!("{chat}: shown {shown} times, not {count}"))
        })
        .collect();
    for subject in [DEFAULT_SUBJECT, FIVE_SUBJECT, ONE_SUBJECT, LATE_SUBJECT, SOON_SUBJECT] {
        qa.ask(&format!("dismiss {subject}"))?;
    }
    ensure(wrong.is_empty(), || wrong.join("; "))
}

// --- a moved meeting ---

const MOVED_SUBJECT: &str = "Made-up moved meeting";

fn moved_calendar() -> Vec<Vec<Value>> {
    let at = |start: i64| reminded(event("AAMkQaMoved", MOVED_SUBJECT, start, 30, MEETING_CHAT), Some(15));
    vec![vec![at(10 * 60)], vec![at(40 * 60)], vec![at(12 * 60)]]
}

const MOVED_CHECKS: [Check; 1] = [(
    "moved_meeting_closes_its_reminder_and_reminds_again_at_its_new_time_once",
    moved,
)];

/// The moved meeting's card: what it says of when.
fn moved_card(qa: &mut Qa<'_>) -> SandboxResult<Option<String>> {
    Ok(qa
        .cards()?
        .into_iter()
        .find(|(title, _)| title == MOVED_SUBJECT)
        .map(|(_, when)| when))
}

fn moved(qa: &mut Qa<'_>) -> SandboxResult {
    qa.reminded_times(MEETING_CHAT, 1, REMINDER_WAIT)?;
    let first = moved_card(qa)?;
    say(&format!("qa: before the move: {first:?}"));
    ensure(
        first
            .as_deref()
            .is_some_and(|when| when.starts_with("Starts in 10") || when.starts_with("Starts in 9")),
        || format!("before the move: {first:?}"),
    )?;
    qa.ask("refresh")?;
    thread::sleep(Duration::from_secs(3));
    let away = moved_card(qa)?;
    ensure(away.is_none(), || {
        format!("the card after the move 40 minutes away: {away:?}")
    })?;
    qa.ask("refresh")?;
    qa.reminded_times(MEETING_CHAT, 2, REMINDER_WAIT)?;
    let again = moved_card(qa)?;
    say(&format!("qa: after the move: {again:?}"));
    qa.shot("moved-reminds-again")?;
    qa.ask("refresh")?;
    thread::sleep(Duration::from_secs(20));
    let shown = qa.reminded(MEETING_CHAT);
    ensure(
        again
            .as_deref()
            .is_some_and(|when| when.starts_with("Starts in 12") || when.starts_with("Starts in 11")),
        || format!("after the move: {again:?}"),
    )?;
    ensure(shown == 2, || format!("the moved meeting reminded {shown} times"))
}

// --- the switch ---

const SWITCH_SUBJECT: &str = "Made-up switch meeting";

fn switch_calendar() -> Vec<Vec<Value>> {
    vec![vec![reminded(
        event("AAMkQaSwitch", SWITCH_SUBJECT, 10 * 60, 30, MEETING_CHAT),
        Some(15),
    )]]
}

const SWITCH_CHECKS: [Check; 2] = [
    ("switch_off_shows_no_reminder", switch_off),
    ("switch_on_brings_the_reminder", switch_on),
];

fn switch_off(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ask("settings")?;
    qa.shot("switch-off-settings")?;
    thread::sleep(REMINDER_WAIT);
    let shown = qa.notices_of(0, SWITCH_SUBJECT);
    let cards = qa.cards()?;
    ensure(shown.is_empty() && cards.is_empty(), || {
        format!("with the switch off: {shown:?} {cards:?}")
    })
}

fn switch_on(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ask("reminders on")?;
    qa.shot("switch-on-settings")?;
    let notice = reminder_after(qa, 0, SWITCH_SUBJECT, REMINDER_WAIT)?;
    say(&format!("qa: switched on: {:?}", notice.body));
    qa.ask("chats")?;
    qa.shot("switch-on-reminder")?;
    Ok(())
}

// --- the child: a's app ---

/// a's mock tokens: its `ic3` token and its skypetoken; they never run out in a test.
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

/// a's engine on the mock with its calling, its calls' media on file devices.
fn mock_engine(dir: &Path, feed: VideoFeed) -> SandboxResult<EngineHandle> {
    let tokens = Arc::new(Tokens {
        ic3: env(IC3_ENV)?,
        skypetoken: env(SKYPE_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let tracker = Arc::new(teams_call::Tracker::new(Mri::new(LAB_A)));
    let (origin, trouter_origin) = (env(ORIGIN_ENV)?, env(TROUTER_ENV)?);
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
    let name = support::display_name("bot-a@lab.example.test").to_owned();
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
            ..Default::default()
        },
    };
    let push = Push::new(trouter()?);
    Ok(engine::start(Parts {
        calls: Some(CallParts::new(calling, identity, media)),
        store: Store::open(&support::store_file(dir, Alias::A))?,
        // the calendar the banners and the reminders read
        client: Arc::new(Client::new(http()?.with_middle_tier(MIDDLE_TIER_BASE)?)),
        outbox: Outbox::new(transport, Author::new(Mri::new(LAB_A), name))
            .with_calls(Arc::clone(&tracker), teams_core::CallFloor::new()),
        push: Some(push),
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })?)
}

/// a's settings: the a–b–c chat open, Teams Light, the reminders' switch as the run wants it.
fn settings(dir: &Path) -> SandboxResult<SettingsFile> {
    let file = SettingsFile::new(dir.join("settings.toml"));
    let mut opened = Settings {
        selected_chat: Some(ABC_CHAT.to_owned()),
        meeting_reminders: env(REMINDERS_OFF_ENV)? != "1",
        ..Settings::default()
    };
    opened.appearance.follow_system = false;
    opened.appearance.theme = teams_ui::theme::LIGHT_KEY;
    file.save(&opened).map_err(|problem| format!("{problem:?}"))?;
    Ok(file)
}

/// The app's log at debug level under [`LOG_ENV`], when it is set.
fn app_log() -> SandboxResult {
    let Ok(dir) = std::env::var(LOG_ENV) else {
        return Ok(());
    };
    let config = teams_log::Config {
        dir: Path::new(&dir).join(format!("a-{}", std::process::id())),
        setting: teams_log::Level::Debug,
        env: None,
        rotation: teams_log::file::Rotation::default(),
    };
    teams_log::install(&config).map_err(|error| format!("the app's log: {error}"))?;
    Ok(())
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    app_log()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let feed = VideoFeed::new();
    let engine = mock_engine(&dir, feed.clone())?;
    let first = ngteams::accounts::Started {
        calls: Some(PersonalCalls {
            ringer: None,
            sounds: None,
            echo: Mri::new(ECHO),
            video: Some(feed),
            camera: None,
            devices: None,
            noise: teams_media::processing::NoiseControl::default(),
        }),
        ..support::started(Alias::A, engine)?
    };
    let runner = support::runner(&dir)?;
    let settings = settings(&dir)?;
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
                answer_asks(launched.window, cx);
                say("ready");
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

fn shell_of(window: WindowHandle<KitRoot>, cx: &mut AsyncApp) -> Option<Entity<Shell>> {
    window
        .update(cx, |kit, _, cx| {
            let root = kit.view().clone().downcast::<Root>().ok()?;
            root.read(cx).shell_in(cx).cloned()
        })
        .ok()
        .flatten()
}

/// Each time the parent writes [`ASK`] in the sandbox dir: the answer, on stdout.
fn answer_asks(window: WindowHandle<KitRoot>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let ask = PathBuf::from(dir).join(ASK);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(asked) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let (id, question) = asked.split_once(' ').unwrap_or(("0", asked.as_str()));
            if question == "quit" {
                cx.update(app::quit);
                return;
            }
            let answer = shell_of(window, cx).map_or_else(
                || json!({"error": "no shell"}),
                |shell| {
                    window
                        .update(cx, |_, window, cx| answer(question, &shell, window, cx))
                        .unwrap_or_else(|_| json!({"error": "no window"}))
                },
            );
            say(&format!("answer {id} {answer}"));
        }
    })
    .detach();
}

/// The answer to `question` (`<verb> <rest>`).
fn answer(question: &str, shell: &Entity<Shell>, window: &mut gpui::Window, cx: &mut App) -> Value {
    let (verb, rest) = question.split_once(' ').unwrap_or((question, ""));
    let reminders = shell.read(cx).meeting_reminders().cloned();
    match verb {
        "theme" => change(shell, cx, |settings| {
            settings.appearance.follow_system = false;
            settings.appearance.theme = if rest == "dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
        }),
        "font" => {
            let size = rest.parse().unwrap_or(teams_ui::theme::FONT_SIZE);
            change(shell, cx, |settings| settings.appearance.font_size = size)
        }
        "reminders" => change(shell, cx, |settings| settings.meeting_reminders = rest == "on"),
        "chats" => {
            shell.update(cx, |shell, cx| shell.show_chats(window, cx));
            json!({"chats": true})
        }
        "settings" => {
            let page = teams_ui::settings_view::Page::Notifications;
            shell.update(cx, |shell, cx| shell.open_settings_page(page, window, cx));
            json!({"settings": true})
        }
        "ui" => ui_state(shell, cx),
        "layout" => cards_layout(shell, window, cx),
        "cancel-prejoin" => {
            let calls = shell.read(cx).models().calls.clone();
            calls.update(cx, teams_ui::Calls::cancel_prejoin);
            json!({"cancelled": true})
        }
        "refresh" => reminders.map_or_else(
            || json!({"error": "no reminders"}),
            |reminders| {
                reminders.update(cx, MeetingReminders::refresh);
                json!({"refreshed": true})
            },
        ),
        "cards" => reminders.map_or_else(|| json!({"cards": []}), |reminders| cards(&reminders, cx)),
        "snooze" | "dismiss" => reminders.map_or_else(
            || json!({"error": "no reminders"}),
            |reminders| act(&reminders, verb, rest, cx),
        ),
        _ => json!({"error": format!("unknown question {question}")}),
    }
}

fn change(shell: &Entity<Shell>, cx: &mut App, edit: impl FnOnce(&mut Settings)) -> Value {
    let model = shell.read(cx).settings().clone();
    model.update(cx, |model, cx| model.change(edit, cx));
    json!({"changed": true})
}

fn ui_state(shell: &Entity<Shell>, cx: &App) -> Value {
    let shell = shell.read(cx);
    let calls = shell.models().calls.read(cx);
    json!({
        "chat": shell.chat().map(|chat| chat.read(cx).conversation().to_owned()),
        "prejoin": calls.prejoin().and_then(|prejoin| prejoin.to_join.chat().map(str::to_owned)),
    })
}

/// Where the cards were drawn (`[x, y, width, height]`), the chat's messages area and the window's size.
fn cards_layout(shell: &Entity<Shell>, window: &gpui::Window, cx: &App) -> Value {
    let rect = |bounds: gpui::Bounds<gpui::Pixels>| {
        json!([
            f32::from(bounds.origin.x),
            f32::from(bounds.origin.y),
            f32::from(bounds.size.width),
            f32::from(bounds.size.height)
        ])
    };
    let shell = shell.read(cx);
    let drawn = shell.meeting_cards_drawn();
    let area = shell.chat().and_then(|chat| chat.read(cx).messages_area().get());
    let viewport = window.viewport_size();
    json!({
        "stack": drawn.map(|drawn| rect(drawn.stack)),
        "shown": drawn.map(|drawn| drawn.shown),
        "more": drawn.map(|drawn| drawn.more),
        "area": area.map(rect),
        "viewport": [f32::from(viewport.width), f32::from(viewport.height)],
    })
}

fn cards(reminders: &Entity<MeetingReminders>, cx: &App) -> Value {
    let now = teams_ui::meeting_reminder::wall_ms(cx);
    let cards: Vec<Value> = reminders
        .read(cx)
        .cards(cx)
        .into_iter()
        .map(|card| {
            let when = if card.stage == Stage::Start {
                teams_ui::meeting_reminder::when_text(card.start, card.start)
            } else {
                teams_ui::meeting_reminder::when_text(card.start, now)
            };
            json!({"title": card.title, "when": when, "times": card.times, "notice": card.notice})
        })
        .collect();
    json!({"cards": cards})
}

/// The card's Snooze (`snooze <minutes> <title>`) or Dismiss (`dismiss <title>`) of the reminder titled so.
fn act(reminders: &Entity<MeetingReminders>, verb: &str, rest: &str, cx: &mut App) -> Value {
    let (minutes, title) = if verb == "snooze" {
        let (minutes, title) = rest.split_once(' ').unwrap_or(("5", rest));
        (minutes.parse().unwrap_or(5), title)
    } else {
        (0, rest)
    };
    let card = reminders
        .read(cx)
        .cards(cx)
        .into_iter()
        .find(|card| card.title == title);
    let Some(card) = card else {
        return json!({"error": format!("no card {title}")});
    };
    reminders.update(cx, |reminders, cx| {
        if verb == "snooze" {
            reminders.snooze(&card.occasion, minutes, cx);
        } else {
            reminders.dismiss(&card.occasion, cx);
        }
    });
    json!({"done": verb})
}
