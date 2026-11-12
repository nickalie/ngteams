//! A large account's first sign-in, seen in the app's window in the UI sandbox (`harness = false`: GPUI runs on the
//! process's main thread).
//!
//! The parent runs the mock (`teams-mock`) with [`CHATS`] more chats of a's, its conversation list in pages of
//! [`PAGE`], and holds the reads it wants to watch the app wait for. It runs the app (a's engine on that mock, as
//! `ngteams` hands its own to the window) as a child in the sandbox with a control socket, and reads it as a user
//! would see it: the view trace, `ngteamsctl`'s requests over that socket, and screenshots kept under
//! `target/ui-shots/first-load-*.png`.
//!
//! - **The list fills page by page.** While the second list page is held, the list shows the first page's chats; a
//!   chat of it opened then shows no messages yet, and its message once its sync is done. The other pages add their
//!   chats under the first page's, which never move; while a later chat's messages are held the list is whole, the
//!   chat opened stays selected, a synced chat shows its message, and the held one shows none until its read is let
//!   go. The first page's rows show each chat's last message and unread mark from the list page at once; a chat's
//!   own sync replaces that preview (a message edited on the service after its list page came shows its new text).
//!   No chat is listed twice, no message shown twice, no message held twice.
//! - **A failed later page and a restart.** The second page refused, the catch-up fails and the list keeps the first
//!   page. The app quit and started again on the same store shows that page at once, then the whole list once its
//!   catch-up is done, each chat once with its one message. The previews kept from the list page are no messages of
//!   the chats' history.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, RawFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use std::{fs, future, thread};

use gpui::{App, Window, WindowingModes};
use ngteams::app::{self, Launch};
#[path = "../window.rs"]
mod support;
use ngteams::descriptors::adopt_trace;
use support::{Alias, display_name};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_ctl::protocol::{CtlEvent, EventFilter, EventLine, OpenTarget, Request, Response};
use teams_ctl::{Client, Timeouts};
use teams_mock::{
    AB_CHAT, ABC_CHAT, AC_CHAT, AX_CHAT, CHAT_BASE, ChatServiceMock, Front, Hold, LAB_A, TrouterMock, synthetic_chat,
    synthetic_text,
};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::tray::MENU_PATH;
use teams_ui::tray::MENU_QUIT;
use tokio::sync::broadcast;
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

/// Which mode the process runs in: unset the parent, `app` the child.
const CHILD_ENV: &str = "NGTEAMS_FIRST_LOAD_TEST";
const TRACE_FD_ENV: &str = "NGTEAMS_FIRST_LOAD_TRACE_FD";
const ORIGIN_ENV: &str = "NGTEAMS_FIRST_LOAD_ORIGIN";
const TROUTER_ENV: &str = "NGTEAMS_FIRST_LOAD_TROUTER";
const TOKEN_ENV: &str = "NGTEAMS_FIRST_LOAD_TOKEN";
const DIR_ENV: &str = "NGTEAMS_FIRST_LOAD_DIR";
/// The synthetic chats the account has besides the lab's.
const CHATS: usize = 400;
/// The conversation list's page size.
const PAGE: usize = 150;
/// The chat opened while only the first page is listed.
const EARLY: usize = 2;
/// A chat synced before [`HELD`].
const SYNCED: usize = 5;
/// The chat whose messages read is held.
const HELD: usize = 10;
/// A first-page chat whose message is edited on the service after its list page came, before its sync.
const EDITED: usize = 7;
/// What [`EDITED`]'s message says after the edit.
const EDITED_TEXT: &str = "Edited synthetic message 0007";
const STARTUP: Duration = Duration::from_secs(90);
const WAIT: Duration = Duration::from_secs(30);
/// How soon something already synced should show.
const SOON: Duration = Duration::from_secs(4);
/// How long a list that should stay as it is is watched.
const STILL: Duration = Duration::from_secs(2);

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("first_load_window: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Notes a step on stderr with the time since the process started, for a failure's log.
fn step(what: &str) {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    eprintln!("[{:>7.2}s] {what}", start.elapsed().as_secs_f64());
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

// --- the parent ---

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams first load", &options)? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let service = Service::start(&runtime)?;
    let filled = the_list_fills_page_by_page(&sandbox, &service);
    drop(service);
    let service = Service::start(&runtime)?;
    let restarted = a_failed_page_and_a_restart_leave_the_whole_list(&sandbox, &service);
    let failures: Vec<String> = [filled, restarted]
        .into_iter()
        .filter_map(Result::err)
        .map(|error| error.to_string())
        .collect();
    if !failures.is_empty() {
        return Err(format!("{}{}", failures.join("\n"), sandbox.logs()).into());
    }
    say("ngteams first load: ok");
    Ok(())
}

/// The mock with the large account, and what its list should show.
struct Service {
    mock: ChatServiceMock,
    /// The app reaches the mock through it, where the holds wait.
    front: Front,
    trouter: TrouterMock,
    /// The chat list once it is whole: the synthetic chats, the newest first, then the a–b chat; the lab's other
    /// chats are empty, a channel or a stream, none of them listed.
    whole: Vec<String>,
    /// The list while only the first page has come.
    first_page: Vec<String>,
}

impl Service {
    fn start(runtime: &tokio::runtime::Runtime) -> SandboxResult<Self> {
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        let front = mock.start_front()?;
        let mut whole = mock.add_synthetic_chats(CHATS);
        if whole.len() != CHATS {
            return Err("the mock made no synthetic chats".into());
        }
        mock.page_conversation_list(PAGE);
        let first_page = whole.get(..PAGE).ok_or("fewer chats than a page")?.to_vec();
        whole.push(AB_CHAT.to_owned());
        Ok(Self {
            mock,
            front,
            trouter,
            whole,
            first_page,
        })
    }

    /// Holds the list pages after the first.
    fn hold_later_pages(&self) -> Hold {
        self.mock.hold(|target| target.contains("mock-page-"))
    }

    /// Holds a list read that starts a catch-up.
    fn hold_list_start(&self) -> Hold {
        self.mock.hold(|target| {
            target
                .split('?')
                .next()
                .is_some_and(|path| path.ends_with("/conversations"))
                && !target.contains("mock-page-")
        })
    }

    /// Holds the messages reads of synthetic chat `n`.
    fn hold_messages(&self, n: usize) -> Hold {
        let id = synthetic_chat(n);
        let segment = id.replace(':', "%3A").replace('@', "%40");
        self.mock.hold(move |target| {
            let path = target.split('?').next().unwrap_or_default();
            path.ends_with(&format!("/conversations/{segment}/messages")) || path.ends_with(&format!("/{id}/messages"))
        })
    }
}

/// The child's view trace, line by line as it comes.
struct TraceReader {
    lines: mpsc::Receiver<serde_json::Value>,
    seen: Vec<serde_json::Value>,
}

impl TraceReader {
    fn start(reader: io::PipeReader) -> Self {
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let value = serde_json::from_str(&line).unwrap_or(serde_json::Value::String(line));
                let _ = sender.send(value);
            }
        });
        Self {
            lines,
            seen: Vec::new(),
        }
    }

    /// Waits for a line, not seen before, that `found` takes, named `what` in the error.
    fn next_where(
        &mut self,
        what: &str,
        found: impl Fn(&serde_json::Value) -> bool,
    ) -> SandboxResult<serde_json::Value> {
        self.next_within(WAIT, what, found)
    }

    /// [`Self::next_where`], waiting `within` at most.
    fn next_within(
        &mut self,
        within: Duration,
        what: &str,
        found: impl Fn(&serde_json::Value) -> bool,
    ) -> SandboxResult<serde_json::Value> {
        let deadline = std::time::Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
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

    /// Takes in the lines that came.
    fn drain(&mut self) {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
    }

    /// The chat list's lines seen so far, each its chats in order.
    fn chat_lists(&mut self) -> Vec<Vec<String>> {
        self.drain();
        self.seen
            .iter()
            .filter(|line| line["view"] == "chat-list" && line["change"] == "shown")
            .map(|line| strings(line, "conversations"))
            .collect()
    }

    /// The latest chat list shown.
    fn chat_list(&mut self) -> Vec<String> {
        self.chat_lists().pop().unwrap_or_default()
    }

    /// Waits for the list to show `wanted`, in order.
    fn wait_list(&mut self, what: &str, wanted: &[String]) -> SandboxResult {
        if self.chat_list() == wanted {
            return Ok(());
        }
        self.next_where(what, |line| {
            line["view"] == "chat-list" && line["change"] == "shown" && strings(line, "conversations") == wanted
        })
        .map(|_| ())
        .map_err(|error| format!("{error}; the list showed {}", summary(&self.chat_list())).into())
    }

    /// Waits for the chat view of `chat` to show `count` messages; their ids.
    fn wait_chat_view(&mut self, chat: &str, count: usize) -> SandboxResult<Vec<String>> {
        self.chat_view_within(WAIT, chat, count)
    }

    /// [`Self::wait_chat_view`], waiting `within` at most.
    fn chat_view_within(&mut self, within: Duration, chat: &str, count: usize) -> SandboxResult<Vec<String>> {
        let found = |line: &serde_json::Value| {
            line["view"] == "chat-view"
                && line["change"] == "shown"
                && line["conversation"] == chat
                && strings(line, "messages").len() == count
        };
        self.drain();
        if let Some(line) = self.seen.iter().rev().find(|line| found(line)) {
            return Ok(strings(line, "messages"));
        }
        let line = self.next_within(
            within,
            &format!("chat-view shown for {chat} with {count} messages"),
            found,
        )?;
        Ok(strings(&line, "messages"))
    }
}

/// The strings of a trace line's array `key`.
fn strings(line: &serde_json::Value, key: &str) -> Vec<String> {
    line[key]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// A list's length and its first and last chats, for an error.
fn summary(list: &[String]) -> String {
    format!("{} chats, {:?} … {:?}", list.len(), list.first(), list.last())
}

/// Every chat list shown is the start of `whole`, in its order: no chat moved, none came twice, none foreign.
fn the_list_never_jumped(trace: &mut TraceReader, whole: &[String]) -> SandboxResult {
    for (at, shown) in trace.chat_lists().iter().enumerate() {
        if !whole.starts_with(shown) {
            let first_odd = shown
                .iter()
                .zip(whole)
                .position(|(shown, wanted)| shown != wanted)
                .unwrap_or(whole.len());
            return Err(format!(
                "chat list {at} isn't the start of the whole list: {}, differing at row {first_odd} ({:?})",
                summary(shown),
                shown.get(first_odd)
            )
            .into());
        }
    }
    Ok(())
}

/// The app running in the sandbox: its process, its view trace and its control socket.
struct AppRun {
    running: Running,
    trace: TraceReader,
    ctl: PathBuf,
}

impl AppRun {
    /// Starts the app on `service` with its store and settings in `dir`; once its window is drawn.
    fn launch(sandbox: &Sandbox, service: &Service, dir: &Path) -> SandboxResult<Self> {
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let token = service.mock.token(LAB_A).ok_or("the mock's token for a")?;
        let origin = service.front.origin().to_owned();
        let dir_text = dir.to_string_lossy().into_owned();
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
            ],
            &[writer.as_fd()],
        )?;
        drop(writer);
        let mut running = Running::new(child)?;
        let trace = TraceReader::start(reader);
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
        let run = Self {
            running,
            trace,
            ctl: dir.join("ctl.sock"),
        };
        wait_until(WAIT, || Client::connect(&run.ctl, Timeouts::default()).ok()).ok_or("no control socket")?;
        Ok(run)
    }

    /// `request` over the control socket, as `ngteamsctl` sends it.
    fn ask(&self, request: Request) -> SandboxResult<Response> {
        let mut client = Client::connect(&self.ctl, Timeouts::default())?;
        Ok(client.request(None, request)?)
    }

    /// `ngteamsctl chats --limit 1000`: the chats' ids.
    fn chats(&self) -> SandboxResult<Vec<String>> {
        match self.ask(Request::Chats {
            unread_only: false,
            limit: Some(1000),
        })? {
            Response::Chats { chats } => Ok(chats.into_iter().map(|chat| chat.id).collect()),
            other => Err(format!("chats answered {other:?}").into()),
        }
    }

    /// `ngteamsctl chats --limit 1000`: each chat's preview line, by id.
    fn previews(&self) -> SandboxResult<Vec<(String, Option<String>)>> {
        match self.ask(Request::Chats {
            unread_only: false,
            limit: Some(1000),
        })? {
            Response::Chats { chats } => Ok(chats.into_iter().map(|chat| (chat.id, chat.preview)).collect()),
            other => Err(format!("chats answered {other:?}").into()),
        }
    }

    /// `ngteamsctl messages <chat>`: its messages' ids and texts.
    fn messages(&self, chat: &str) -> SandboxResult<Vec<(String, String)>> {
        match self.ask(Request::Messages {
            chat: chat.to_owned(),
            limit: Some(50),
            before: None,
        })? {
            Response::Messages { messages } => {
                Ok(messages.into_iter().map(|message| (message.id, message.text)).collect())
            }
            other => Err(format!("messages answered {other:?}").into()),
        }
    }

    /// `ngteamsctl ui state`: the chat open.
    fn open_chat(&self) -> SandboxResult<Option<String>> {
        match self.ask(Request::UiState)? {
            Response::UiState(state) => Ok(state.chat),
            other => Err(format!("ui state answered {other:?}").into()),
        }
    }

    /// `ngteamsctl open chat <chat>`, then the trace's `chat opened`.
    fn open(&mut self, chat: &str) -> SandboxResult {
        self.ask(Request::Open {
            target: OpenTarget::Chat { chat: chat.to_owned() },
        })?;
        self.trace.next_where(&format!("chat {chat} opened"), |line| {
            line["view"] == "chat" && line["change"] == "opened" && line["conversation"] == chat
        })?;
        Ok(())
    }

    /// `ngteamsctl events --kind caught_up,catch_up_failed --follow`, read on a thread of its own.
    fn follow_catch_up(&self) -> SandboxResult<mpsc::Receiver<EventLine>> {
        let client = Client::connect(&self.ctl, Timeouts::default())?;
        let filter = EventFilter {
            kinds: vec!["caught_up".to_owned(), "catch_up_failed".to_owned()],
            ..EventFilter::default()
        };
        let mut events = client.subscribe(None, filter)?;
        events.set_read_timeout(None)?;
        let (sender, received) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(Some(event)) = events.next_event() {
                if sender.send(event).is_err() {
                    return;
                }
            }
        });
        Ok(received)
    }

    /// A screenshot of the window, kept as `target/ui-shots/<name>`.
    fn shot(sandbox: &Sandbox, name: &str) -> SandboxResult {
        // the frames after the trace's line
        thread::sleep(Duration::from_millis(500));
        let png = sandbox.screenshot(name, true)?;
        fs::copy(&png, shots_dir()?.join(name))?;
        Ok(())
    }

    /// Quits through the tray menu, as a user does; once the app ended and released its engine.
    fn quit(mut self, sandbox: &Sandbox) -> SandboxResult {
        let name = format!("org.kde.StatusNotifierItem-{}-1", self.running.pid());
        let host = sandbox.bus().connect()?;
        let menu = zbus::blocking::Proxy::new(&host, name.as_str(), MENU_PATH, "com.canonical.dbusmenu")?;
        menu.call_method("Event", &(MENU_QUIT, "clicked", zbus::zvariant::Value::from(""), 0_u32))?;
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

/// Where the screenshots are kept: `target/ui-shots/`, never committed.
fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Fails when `ids` holds one twice.
fn once_each(what: &str, ids: &[String]) -> SandboxResult {
    let mut sorted = ids.to_vec();
    sorted.sort();
    if let Some([twice, _]) = sorted.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(format!("{what} holds {twice} twice").into());
    }
    Ok(())
}

/// `ngteamsctl chats` lists `wanted` (in any order), each once; besides them only the lab's chats nobody wrote in, which
/// the chat list leaves out but `chats` lists.
fn ctl_lists(run: &AppRun, wanted: &[String]) -> SandboxResult {
    let listed = run.chats()?;
    once_each("ngteamsctl chats", &listed)?;
    let missing: Vec<&String> = wanted.iter().filter(|chat| !listed.contains(chat)).collect();
    let extra: Vec<&String> = listed
        .iter()
        .filter(|chat| !wanted.contains(chat) && ![AC_CHAT, AX_CHAT, ABC_CHAT].contains(&chat.as_str()))
        .collect();
    if !missing.is_empty() || !extra.is_empty() {
        return Err(format!(
            "ngteamsctl chats lists {} chats: {} of the {} shown missing ({:?}), {} others ({:?})",
            listed.len(),
            missing.len(),
            wanted.len(),
            missing.first(),
            extra.len(),
            extra.first()
        )
        .into());
    }
    Ok(())
}

/// What synthetic chat `n`'s one message says: [`EDITED_TEXT`] for [`EDITED`] once `edited`.
fn text_of(n: usize, edited: bool) -> String {
    if edited && n == EDITED {
        EDITED_TEXT.to_owned()
    } else {
        synthetic_text(n)
    }
}

/// Synthetic chat `n` holds its one message, once.
fn holds_its_message(run: &AppRun, n: usize) -> SandboxResult {
    holds_text(run, n, &synthetic_text(n))
}

/// Synthetic chat `n` holds one message, once, saying `text`.
fn holds_text(run: &AppRun, n: usize, text: &str) -> SandboxResult {
    let messages = run.messages(&synthetic_chat(n))?;
    if messages.len() != 1 || messages[0].1 != text {
        return Err(format!("synthetic chat {n} holds {messages:?}, not one {text:?}").into());
    }
    Ok(())
}

/// Waits until the engine gives synthetic chat `n`'s synced message: its own sync is done.
fn wait_synced(run: &AppRun, n: usize) -> SandboxResult {
    let chat = synthetic_chat(n);
    wait_until(WAIT, || {
        run.messages(&chat).ok().filter(|messages| !messages.is_empty())
    })
    .map(|_| ())
    .ok_or_else(|| format!("{chat} never synced while the catch-up went on").into())
}

/// Every synthetic chat's row (`ngteamsctl chats`) shows `text(n)` as its last message, and one unread message unless
/// the chat was `opened` (then none or one): no preview counted beside the message it stands for.
fn rows_are_right(run: &AppRun, text: impl Fn(usize) -> String, opened: &[usize]) -> SandboxResult {
    let Response::Chats { chats } = run.ask(Request::Chats {
        unread_only: false,
        limit: Some(1000),
    })?
    else {
        return Err("no chat rows".into());
    };
    for n in 0..CHATS {
        let id = synthetic_chat(n);
        let row = chats
            .iter()
            .find(|row| row.id == id)
            .ok_or_else(|| format!("chat {n} isn't listed"))?;
        let unread_ok = if opened.contains(&n) {
            row.unread <= 1
        } else {
            row.unread == 1
        };
        if row.preview.as_deref() != Some(text(n).as_str()) || !unread_ok {
            return Err(format!(
                "chat {n}'s row shows {:?} with {} unread, not {:?}",
                row.preview,
                row.unread,
                text(n)
            )
            .into());
        }
    }
    Ok(())
}

fn first_page_hints_are_shown(run: &AppRun) -> SandboxResult {
    let Response::Chats { chats } = run.ask(Request::Chats {
        unread_only: false,
        limit: Some(1000),
    })?
    else {
        return Err("no chat rows for the first page".into());
    };
    for n in 0..PAGE {
        let id = synthetic_chat(n);
        let row = chats.iter().find(|row| row.id == id).ok_or("first-page chat missing")?;
        if row.preview.as_deref() != Some(synthetic_text(n).as_str()) || row.unread != 1 {
            return Err(format!("first-page chat {n} lacks its list-page preview or unread mark: {row:?}").into());
        }
    }
    Ok(())
}

/// Waits for an event of `kind` on `events`.
fn wait_event(events: &mpsc::Receiver<EventLine>, kind: &str, within: Duration) -> SandboxResult<EventLine> {
    loop {
        let event = events.recv_timeout(within).map_err(|_| format!("no {kind} event"))?;
        if event.event.kind() == kind {
            return Ok(event);
        }
        if let CtlEvent::CatchUpFailed { .. } = event.event {
            return Err(format!("the catch-up failed: {event:?}").into());
        }
    }
}

/// While the held chat's read waits ([`MAX_HOLD`](teams_mock::MAX_HOLD) at most): the whole list, the early chat still
/// open and showing its message as soon as its sync is done, the chats synced before the held one listed with their
/// synced last message (the edited one's new text), and a synced chat opened showing its message.
fn the_synced_chats_show_while_a_later_one_waits(
    sandbox: &Sandbox,
    run: &mut AppRun,
    service: &Service,
    early: &str,
) -> SandboxResult {
    run.trace
        .wait_list("the whole list", &service.whole)
        .map_err(|error| run.running.fail(&error))?;
    step("the whole list shown");
    wait_synced(run, EARLY)?;
    step("the early chat synced");
    let shown = run.trace.chat_view_within(SOON, early, 1).map_err(|error| {
        format!("{early}, open since before its sync, doesn't show its synced message while a later chat's read waits: {error}")
    })?;
    once_each("the early chat's view", &shown)?;
    step("the early chat shows its message");
    for n in 0..HELD {
        wait_synced(run, n)?;
        holds_text(run, n, &text_of(n, true))?;
    }
    let previews = run.previews()?;
    let without: Vec<usize> = (0..HELD)
        .filter(|&n| {
            let id = synthetic_chat(n);
            !previews
                .iter()
                .any(|(chat, preview)| *chat == id && preview.as_deref() == Some(text_of(n, true).as_str()))
        })
        .collect();
    if !without.is_empty() {
        return Err(format!(
            "the list doesn't show the synced last message of chats {without:?} while a later chat's read waits"
        )
        .into());
    }
    step("the synced chats' rows show their synced last message, the edited one's new text");
    if run.open_chat()?.as_deref() != Some(early) {
        return Err(format!("the chat opened isn't {early} any more: {:?}", run.open_chat()?).into());
    }
    AppRun::shot(sandbox, "first-load-whole-list-syncing.png")?;
    let edited = synthetic_chat(EDITED);
    run.open(&edited)?;
    run.trace.chat_view_within(SOON, &edited, 1).map_err(|error| {
        format!("{edited}, synced, opened while a later chat's read waits, shows no message: {error}")
    })?;
    AppRun::shot(sandbox, "first-load-edited-chat-while-syncing.png")?;
    let synced = synthetic_chat(SYNCED);
    run.open(&synced)?;
    run.trace.chat_view_within(SOON, &synced, 1).map_err(|error| {
        format!("{synced}, synced, opened while a later chat's read waits, shows no message: {error}")
    })?;
    AppRun::shot(sandbox, "first-load-synced-chat-while-syncing.png")?;
    Ok(())
}

fn the_list_fills_page_by_page(sandbox: &Sandbox, service: &Service) -> SandboxResult {
    let dir = tempfile::tempdir()?;
    let later_pages = service.hold_later_pages();
    let mut run = AppRun::launch(sandbox, service, dir.path())?;
    let caught_up = run.follow_catch_up()?;
    if !later_pages.wait_entered(STARTUP) {
        return Err(run.running.fail(&"the second list page was never read"));
    }
    // the first page, while the next is held
    run.trace
        .wait_list("the first page listed", &service.first_page)
        .map_err(|error| run.running.fail(&error))?;
    step("the first page listed while the second is held");
    ctl_lists(&run, &service.first_page)?;
    first_page_hints_are_shown(&run)?;
    AppRun::shot(sandbox, "first-load-first-page.png")?;
    // edited on the service after its list page came: only the chat's own sync brings the new text
    if !service.mock.edit_synthetic_message(EDITED, EDITED_TEXT) {
        return Err("the mock has no chat to edit".into());
    }
    let early = synthetic_chat(EARLY);
    run.open(&early)?;
    let shown = run.trace.wait_chat_view(&early, 0)?;
    if !shown.is_empty() {
        return Err(format!("{early} shows messages before its sync: {shown:?}").into());
    }
    AppRun::shot(sandbox, "first-load-chat-not-synced.png")?;

    let held = service.hold_messages(HELD);
    if !later_pages.release() {
        return Err("the second page went before the test let it go".into());
    }
    if !held.wait_entered(WAIT) {
        return Err(run.running.fail(&"the held chat's messages were never read"));
    }
    step("the held chat's messages read waits");
    the_synced_chats_show_while_a_later_one_waits(sandbox, &mut run, service, &early)?;
    let held_chat = synthetic_chat(HELD);
    run.open(&held_chat)?;
    run.trace.wait_chat_view(&held_chat, 0)?;
    if run.trace.chat_view_within(STILL, &held_chat, 1).is_ok() {
        return Err(format!("{held_chat} shows its message while its read is held").into());
    }
    step("the held chat shows no message yet");
    AppRun::shot(sandbox, "first-load-chat-loading.png")?;
    ctl_lists(&run, &service.whole)?;
    if !held.release() {
        return Err("the held read went at its limit before the checks ended".into());
    }
    let shown = run.trace.wait_chat_view(&held_chat, 1)?;
    once_each("the held chat's view", &shown)?;
    step("the held chat shows its message");
    wait_event(&caught_up, "caught_up", Duration::from_secs(120))?;
    step("caught up");
    AppRun::shot(sandbox, "first-load-caught-up.png")?;

    run.trace
        .wait_list("the whole list after the catch-up", &service.whole)?;
    the_list_never_jumped(&mut run.trace, &service.whole)?;
    ctl_lists(&run, &service.whole)?;
    rows_are_right(&run, |n| text_of(n, true), &[EARLY, EDITED, SYNCED, HELD])?;
    for n in [0, EARLY, SYNCED, HELD, PAGE - 1, PAGE, CHATS - 1] {
        holds_its_message(&run, n)?;
    }
    holds_text(&run, EDITED, EDITED_TEXT)?;
    for n in [EARLY, SYNCED] {
        let chat = synthetic_chat(n);
        run.open(&chat)?;
        once_each("a chat's view after the catch-up", &run.trace.wait_chat_view(&chat, 1)?)?;
    }
    run.quit(sandbox)
}

fn a_failed_page_and_a_restart_leave_the_whole_list(sandbox: &Sandbox, service: &Service) -> SandboxResult {
    let dir = tempfile::tempdir()?;
    let later_pages = service.hold_later_pages();
    let mut run = AppRun::launch(sandbox, service, dir.path())?;
    let failed = run.follow_catch_up()?;
    if !later_pages.wait_entered(STARTUP) {
        return Err(run.running.fail(&"the second list page was never read"));
    }
    run.trace.wait_list("the first page listed", &service.first_page)?;
    // a retry's later page waits until the app has quit
    let retry = service.hold_later_pages();
    if !later_pages.refuse(500) {
        return Err("the second page went before the test refused it".into());
    }
    let event = failed.recv_timeout(WAIT).map_err(|_| "no catch_up_failed event")?;
    if event.event.kind() != "catch_up_failed" {
        return Err(format!("the refused page ended in {event:?}").into());
    }
    thread::sleep(STILL);
    if run.trace.chat_list() != service.first_page {
        return Err(format!(
            "after the failed page the list shows {}",
            summary(&run.trace.chat_list())
        )
        .into());
    }
    the_list_never_jumped(&mut run.trace, &service.whole)?;
    ctl_lists(&run, &service.first_page)?;
    step("the failed page left the first page listed");
    first_page_hints_are_shown(&run)?;
    AppRun::shot(sandbox, "first-load-failed-page.png")?;
    let quitting = thread::spawn(move || {
        // let a retry's held page go once the quit was asked, refused as the first was
        thread::sleep(STILL);
        let _ = retry.refuse(500);
    });
    run.quit(sandbox)?;
    let _ = quitting.join();

    step("quit after the failed page");
    let start = service.hold_list_start();
    let mut run = AppRun::launch(sandbox, service, dir.path())?;
    let caught_up = run.follow_catch_up()?;
    run.trace
        .wait_list("the kept first page after the restart", &service.first_page)
        .map_err(|error| run.running.fail(&error))?;
    if !start.wait_entered(WAIT) {
        return Err(run.running.fail(&"the restart didn't read the list"));
    }
    first_page_hints_are_shown(&run)?;
    // the previews kept from the list page are no history
    for n in [0, EARLY, PAGE - 1] {
        let messages = run.messages(&synthetic_chat(n))?;
        if !messages.is_empty() {
            return Err(format!("chat {n}, never synced, holds {messages:?} after the restart").into());
        }
    }
    if !start.release() {
        return Err("the restart's list read went before the test let it go".into());
    }
    step("the restart shows the kept first page");
    wait_event(&caught_up, "caught_up", Duration::from_secs(120))?;
    step("the restart caught up");
    run.trace
        .wait_list("the whole list after the restart", &service.whole)?;
    the_list_never_jumped(&mut run.trace, &service.whole)?;
    ctl_lists(&run, &service.whole)?;
    rows_are_right(&run, synthetic_text, &[])?;
    for n in [0, PAGE - 1, PAGE, CHATS - 1] {
        holds_its_message(&run, n)?;
    }
    let first = synthetic_chat(0);
    run.open(&first)?;
    once_each("the first chat's view", &run.trace.wait_chat_view(&first, 1)?)?;
    AppRun::shot(sandbox, "first-load-restarted.png")?;
    run.quit(sandbox)
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

/// a's engine on the parent's mock, its store in `dir`, as `teams_core::account::start` builds one but for the mock
/// origins.
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
        client: Arc::new(ChatClient::new(http()?)),
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
                let _ = launched.window.update(cx, |_, window, _| {
                    after_frames(window, 3, |_, _| say("ready"));
                });
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
