//! The Activity pane as a user meets it, in the UI sandbox on the mock (`harness = false`: GPUI runs on the process's
//! main thread).
//!
//! The test runs itself again inside the sandbox as the app of lab account a (its engine on the mock chat service and
//! its Trouter, the app's control socket served in the sandbox's dir), over a made-up feed: two dozen older read
//! replies, a team add, an app's note, an @everyone mention, a voicemail, a reply, a mention in a chat that is gone, a
//! missed call, a meeting's update, a mention, and b's and c's reactions to one of a's messages (folded into one item).
//! The parent drives the window only as a user does — the sandbox's virtual keyboard and pointer, and `ngteamsctl`'s
//! own command line against the app's socket — and checks what the window and the mock then hold: the child answers
//! "what does the pane hold now" and "what does the mock's feed hold" through files in the sandbox dir, and posts new
//! activity on the mock when asked, as the service does. Every check is named and counted; the screenshots go to
//! `target/ui-shots/activity-*.png` and, when `ACTIVITY_QA_EVIDENCE` names a directory, there too.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, RawFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use std::{fs, future, io, thread};

use clap::Parser as _;
use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
use ngteams::calling::PersonalCalls;
#[path = "../window.rs"]
mod support;
use ngteams::descriptors::adopt_trace;
use serde_json::{Value, json};
use support::Alias;
use support::display_name;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_ctl::cli::{self, Cli, Exit};
use teams_mock::{
    AB_CHAT, ABC_CHAT, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, NOTIFICATIONS, PRESENCE_BASE,
    TrouterMock, USER_PROFILE_BASE,
};
use teams_proto::Mri;
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::shell::Shell;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::{Modifier, VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

/// Which mode the child runs in.
const CHILD_ENV: &str = "NGTEAMS_ACTIVITY_TEST";
/// The child's view trace descriptor.
const TRACE_FD_ENV: &str = "NGTEAMS_ACTIVITY_TRACE_FD";
/// A directory the screenshots are copied to as well (QA's evidence), when set.
const EVIDENCE_ENV: &str = "ACTIVITY_QA_EVIDENCE";
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(30);
/// Frames the window presents before the child says `ready`.
const READY_FRAMES: usize = 3;
/// The file in the sandbox dir the parent writes a question to.
const ASK: &str = "activity-ask";
/// The app's control socket, in the sandbox dir.
const CTL_SOCKET: &str = "ctl.sock";
/// How long the views get to settle after an input before they are looked at.
const SETTLE: Duration = Duration::from_millis(700);
/// The team of the mock's channel.
const TEAM: &str = "19:channel0001@thread.tacv2";
/// A chat the feed names that no longer exists (left, or deleted): the mock has no such chat.
const GONE_CHAT: &str = "19:made-up-gone-chat@thread.v2";
/// The older read items that make the list scroll.
const FILLERS: u64 = 24;
/// How far apart in time the made-up older items are, ms (a day).
const DAY_MS: i64 = 86_400_000;
/// Just before the mock's first arrival time (`teams-mock`'s clock).
const MOCK_EPOCH_MS: i64 = 1_760_000_000_000;
/// The clientmessageid the mock gives a feed item: this plus its id.
const FEED_CLIENT_ID_BASE: u64 = 9_100_000_000_000_000_000;

// --- where things are drawn (1280×800 output, scale 1): the rail, the Activity pane's header, chips and first row ---

/// The rail's Activity (bell).
const RAIL_ACTIVITY: (u32, u32) = (37, 78);
/// The pane header's "More options" and "Filter" (magnifier).
const HEADER_MORE: (u32, u32) = (404, 71);
const HEADER_FILTER: (u32, u32) = (428, 71);
/// The header menu's "Mark all as read", below its button.
const HEADER_MENU_MARK_ALL: (u32, u32) = (258, 108);
/// Each chip's middle, in Teams' order: four and Replies on the first line, Voicemail and Apps on the second.
const CHIPS: [(&str, (u32, u32)); 7] = [
    ("unread", (112, 114)),
    ("mentions", (180, 114)),
    ("reactions", (253, 114)),
    ("missed-calls", (330, 114)),
    ("replies", (400, 114)),
    ("voicemail", (118, 143)),
    ("apps", (177, 143)),
];
/// The first row: its middle, its corner's "More options" (where the date is), its tooltip's place (the title).
const ROW: (u32, u32) = (260, 205);
const ROW_MORE: (u32, u32) = (430, 177);
const ROW_TITLE: (u32, u32) = (200, 184);
/// A missed call's buttons on the first row.
const ROW_CALL: (u32, u32) = (154, 229);
const ROW_CHAT: (u32, u32) = (202, 229);
/// The item menu's rows below its button: Mark as read/unread, Remove, (divider) Notification settings.
const ITEM_MENU_MARK: (u32, u32) = (325, 213);
const ITEM_MENU_REMOVE: (u32, u32) = (300, 247);
const ITEM_MENU_SETTINGS: (u32, u32) = (340, 290);
/// How far the search's open box pushes the chips and the rows down.
const SEARCH_SHIFT: u32 = 39;
/// The middle of the Activity list, and the list's area (left, top, right, bottom) below the chips.
const LIST_MIDDLE: (u32, u32) = (300, 600);
const LIST_AREA: (u32, u32, u32, u32) = (80, 165, 450, 790);
/// The close (×) of the banner over both panes (a call refused).
const BANNER_CLOSE: (u32, u32) = (1250, 66);
/// Somewhere neutral: the conversation pane's empty middle.
const AWAY: (u32, u32) = (900, 500);

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child_app(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("activity window test: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Prints `line` to stdout at once.
fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

// --- the parent: the sandbox, the user's input, and the checks ---

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams activity window", &options)? else {
        return Ok(());
    };
    let outcome = run_checks(&sandbox).map_err(|error| format!("{error}{}", sandbox.logs()));
    let failed = outcome?;
    if failed > 0 {
        return Err(format!("{failed} activity check(s) failed").into());
    }
    say("ngteams activity window: ok");
    Ok(())
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

    /// Takes the lines that came so far.
    fn drain(&mut self) {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
    }

    /// Waits for a line `found` takes among those not seen yet.
    fn next_where(&mut self, what: &str, found: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        loop {
            let line = self
                .lines
                .recv_timeout(WAIT)
                .map_err(|_| format!("no trace line {what}"))?;
            let hit = found(&line);
            self.seen.push(line.clone());
            if hit {
                return Ok(line);
            }
        }
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

/// The run: the app, the user's devices and `ngteamsctl`, and the tally.
struct Qa<'a> {
    sandbox: &'a Sandbox,
    running: Running,
    trace: TraceReader,
    input: VirtualInput,
    socket: PathBuf,
    passed: Vec<&'static str>,
    failed: Vec<(&'static str, String)>,
    /// Whether the search's box is open (it moves the chips and rows down by [`SEARCH_SHIFT`]).
    box_open: bool,
}

/// One user flow, by name.
type Check = (&'static str, fn(&mut Qa<'_>) -> SandboxResult);

/// The flows, in the order a user meets them (each leaves the window as the next expects it).
const CHECKS: [Check; 21] = [
    ("rail_counts_new_activity", rail_counts_new_activity),
    ("ctl_lists_the_feed_as_the_pane_does", ctl_lists_the_feed),
    ("ctl_filters_search_and_limit", ctl_filters_search_and_limit),
    (
        "rail_click_opens_the_pane_and_clears_the_count",
        rail_click_opens_the_pane,
    ),
    (
        "keys_switch_between_activity_and_chats",
        keys_switch_between_activity_and_chats,
    ),
    ("ctl_open_activity_and_ui_state", ctl_open_activity_and_ui_state),
    ("chips_filter_by_click_and_keys", chips_filter),
    ("search_keeps_matching_items_and_shows_empty", search_filters),
    ("hover_shows_more_options_and_the_tooltip", hover_and_tooltip),
    ("item_menu_marks_read_and_unread", item_menu_marks),
    ("item_menu_notification_settings", item_menu_notification_settings),
    ("item_menu_remove_hides_it_with_a_toast", item_menu_remove),
    ("click_opens_the_chat_at_the_message", click_opens_the_item),
    ("keyboard_walks_and_opens", keyboard_walks_and_opens),
    ("missed_call_chat_and_call", missed_call_buttons),
    ("meeting_item_time_line", meeting_item),
    ("chat_on_screen_reads_its_items_and_new_ones", chat_on_screen),
    ("live_activity_counts_on_the_rail", live_activity_counts),
    ("many_items_scroll_to_the_oldest", many_items_scroll),
    ("item_whose_chat_is_gone", gone_chat_item),
    ("mark_all_read_by_shift_esc_and_menu", mark_all_read),
];

fn run_checks(sandbox: &Sandbox) -> SandboxResult<usize> {
    let (reader, writer) = io::pipe()?;
    let fd = writer.as_raw_fd().to_string();
    let child = sandbox.spawn_with_fds(
        &std::env::current_exe()?,
        &[],
        &[(CHILD_ENV, "app"), (TRACE_FD_ENV, &fd)],
        &[writer.as_fd()],
    )?;
    drop(writer);
    let mut running = Running::new(child)?;
    let trace = TraceReader::start(reader);
    running
        .line_where(STARTUP, |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
    let mut qa = Qa {
        sandbox,
        running,
        trace,
        input: sandbox.input()?,
        socket: sandbox.path(CTL_SOCKET),
        passed: Vec::new(),
        failed: Vec::new(),
        box_open: false,
    };
    qa.settled()?;
    for (name, check) in CHECKS {
        qa.check(name, check);
    }
    // then the same pane in Teams Dark, its hover and menus
    qa.check("dark_theme_pane_hover_and_menus", dark_theme);
    let failed = qa.failed.len();
    for (name, error) in &qa.failed {
        say(&format!("FAILED {name}: {error}"));
    }
    say(&format!(
        "activity checks: {} passed, {} failed",
        qa.passed.len(),
        failed
    ));
    qa.quit()?;
    Ok(failed)
}

impl Qa<'_> {
    /// Runs one flow, counted; a failed one is screenshot as it was left.
    fn check(&mut self, name: &'static str, flow: fn(&mut Qa<'_>) -> SandboxResult) {
        match flow(self) {
            Ok(()) => {
                say(&format!("ok {name}"));
                self.passed.push(name);
            }
            Err(error) => {
                say(&format!("FAILED {name}: {error}"));
                let _ = self.shot(&format!("failed-{name}"));
                self.failed.push((name, error.to_string()));
                // whatever was left open goes, for the next flow
                let _ = self.input.key(keys::ESC);
            }
        }
    }

    /// Asks the child `question`; its answer.
    fn ask(&self, question: &str) -> SandboxResult<Value> {
        let staged = self.sandbox.path("activity-ask.staged");
        fs::write(&staged, question)?;
        fs::rename(&staged, self.sandbox.path(ASK))?;
        let line = self
            .running
            .line_where(WAIT, |line| line.starts_with("answer "))
            .map_err(|error| format!("no answer to {question}: {error:?}"))?;
        Ok(serde_json::from_str(line.trim_start_matches("answer "))?)
    }

    /// What the window holds now (see the child's [`state_of`]).
    fn state(&self) -> SandboxResult<Value> {
        self.ask("state")
    }

    /// Waits up to [`WAIT`] for the window to hold what `wanted` takes; the last state seen in the error.
    fn state_until(&self, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let mut last = Value::Null;
        let found = wait_until(WAIT, || match self.state() {
            Ok(state) if wanted(&state) => Some(state),
            Ok(state) => {
                last = state;
                thread::sleep(Duration::from_millis(200));
                None
            }
            Err(_) => None,
        });
        found.ok_or_else(|| format!("{what}: the window holds {last}").into())
    }

    /// The mock's own copy of feed item `id`: its `isread`, and whether it was removed.
    fn server(&self, id: &str) -> SandboxResult<Value> {
        self.ask(&format!("server {id}"))
    }

    /// Waits for the mock's copy of `id` to take what `wanted` takes.
    fn server_until(&self, id: &str, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let mut last = Value::Null;
        wait_until(WAIT, || match self.server(id) {
            Ok(held) if wanted(&held) => Some(held),
            Ok(held) => {
                last = held;
                thread::sleep(Duration::from_millis(200));
                None
            }
            Err(_) => None,
        })
        .ok_or_else(|| format!("{what}: the mock holds {last}").into())
    }

    /// `ngteamsctl <args>` against the app's socket, in-process: its exit, stdout and stderr.
    fn ctl(&self, args: &[&str]) -> SandboxResult<(Exit, String, String)> {
        let socket = self.socket.to_string_lossy().into_owned();
        let line = ["ngteamsctl", "--socket", socket.as_str()]
            .into_iter()
            .chain(args.iter().copied());
        let parsed = match Cli::try_parse_from(line) {
            Ok(parsed) => parsed,
            // the command line itself refused, as the binary refuses it (usage errors exit 2)
            Err(error) => return Ok((Exit::NoApp, String::new(), error.render().to_string())),
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let exit = cli::run(&parsed, &teams_ctl::Env::default(), &mut out, &mut err);
        Ok((exit, String::from_utf8(out)?, String::from_utf8(err)?))
    }

    /// `ngteamsctl --json <args>`, answered: the response's `ok` object.
    fn ctl_json(&self, args: &[&str]) -> SandboxResult<Value> {
        let full: Vec<&str> = std::iter::once("--json").chain(args.iter().copied()).collect();
        let (exit, out, err) = self.ctl(&full)?;
        if exit != Exit::Ok {
            return Err(format!("ngteamsctl {args:?} exited {exit:?}: {out}{err}").into());
        }
        let line: Value = serde_json::from_str(out.trim())?;
        Ok(line["ok"].clone())
    }

    /// The feed as `ngteamsctl activity` lists it with `args`.
    fn ctl_items(&self, args: &[&str]) -> SandboxResult<Vec<Value>> {
        let full: Vec<&str> = std::iter::once("activity").chain(args.iter().copied()).collect();
        let ok = self.ctl_json(&full)?;
        Ok(ok["items"].as_array().cloned().unwrap_or_default())
    }

    /// Screenshots the window as `activity-<name>.png`, kept with the tour's and in the evidence dir.
    fn shot(&self, name: &str) -> SandboxResult<PathBuf> {
        thread::sleep(SETTLE);
        let file = format!("activity-{name}.png");
        let png = self.sandbox.screenshot(&file, true)?;
        let kept = shots_dir()?.join(&file);
        fs::copy(&png, &kept)?;
        if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
            let dir = PathBuf::from(dir);
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(&file))?;
        }
        Ok(kept)
    }

    /// `at` (a place among the chips or the rows) where it is drawn now, the search's box open or not.
    fn below_box(&self, (x, y): (u32, u32)) -> (u32, u32) {
        if self.box_open { (x, y + SEARCH_SHIFT) } else { (x, y) }
    }

    /// Clicks the place `at` among the chips or the rows.
    fn click_listed(&mut self, at: (u32, u32)) -> SandboxResult {
        let at = self.below_box(at);
        self.click(at)
    }

    /// Hovers the place `at` among the chips or the rows.
    fn hover_listed(&mut self, at: (u32, u32)) -> SandboxResult {
        let at = self.below_box(at);
        self.hover(at)
    }

    fn click(&mut self, (x, y): (u32, u32)) -> SandboxResult {
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn hover(&mut self, (x, y): (u32, u32)) -> SandboxResult {
        self.input.move_to(x, y)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// Presses `typed` (a character on the US keymap) with `modifiers` held.
    fn chord(&mut self, modifiers: &[Modifier], typed: char) -> SandboxResult {
        let (key, _) = self
            .input
            .keymap()
            .key_for(typed)
            .ok_or_else(|| format!("{typed} isn't on the US keymap"))?;
        self.input.key_with(modifiers, key)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn key(&mut self, key: u32) -> SandboxResult {
        self.input.key(key)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// Waits until the catch-up has the whole feed in the pane.
    fn settled(&self) -> SandboxResult {
        self.state_until("the seeded feed listed", |state| {
            items(state).len() >= usize::try_from(FILLERS).unwrap_or(usize::MAX) + 10
        })?;
        Ok(())
    }

    /// The Activity pane shown with no chip, no search and no menu open, the pointer away. The search's box is open
    /// when the pane, with no chip, lists fewer items than the feed has.
    fn plain_pane(&mut self) -> SandboxResult<Value> {
        self.hover(AWAY)?;
        let state = self.state()?;
        // Settings cover the pane: Ctrl+1 shows it again
        if state["list"] != "Activity" || state["pane_shown"] != true {
            self.chord(&[Modifier::Ctrl], '1')?;
        }
        if self.box_open || self.searching()? {
            self.click(HEADER_FILTER)?;
        }
        self.box_open = false;
        let filter = self.state()?["filter"].as_str().unwrap_or("all").to_owned();
        if filter != "all" {
            self.click_chip(&filter)?;
        }
        let total = self.ctl_items(&[])?.len();
        self.state_until("the plain pane", |state| {
            state["list"] == "Activity"
                && state["pane_shown"] == true
                && state["filter"] == "all"
                && items(state).len() == total
        })
    }

    /// Whether the search's box keeps items out: the pane, with no chip, lists fewer than the feed has.
    fn searching(&self) -> SandboxResult<bool> {
        let state = self.state()?;
        Ok(state["filter"] == "all" && items(&state).len() < self.ctl_items(&[])?.len())
    }

    /// Clicks the chip `key`.
    fn click_chip(&mut self, key: &str) -> SandboxResult {
        let at = chip_at(key).ok_or_else(|| format!("no chip {key}"))?;
        self.click_listed(at)
    }

    /// Opens the search's box and types `words` into it (the pane shown).
    fn search(&mut self, words: &str) -> SandboxResult<Value> {
        self.plain_pane()?;
        self.click(HEADER_FILTER)?;
        self.box_open = true;
        self.input.type_text(words)?;
        let folded = words.to_lowercase();
        self.state_until(&format!("the search for {words}"), |state| {
            items(state).iter().all(|item| {
                let text = format!("{} {} {}", item["title"], item["preview"], item["place"]).to_lowercase();
                text.contains(&folded)
            })
        })
    }

    /// Opens the first row's "More options" (hovered first, as the button shows only then).
    fn open_row_menu(&mut self) -> SandboxResult<Value> {
        self.hover_listed(ROW)?;
        self.click_listed(ROW_MORE)?;
        self.state_until("the first row's menu open", |state| !state["item_menu"].is_null())
    }

    fn quit(mut self) -> SandboxResult {
        let _ = self.ask("quit");
        self.trace.drain();
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("the app exited {status}: {stderr}").into());
        }
        Ok(())
    }
}

/// The chip `key`'s place.
fn chip_at(key: &str) -> Option<(u32, u32)> {
    CHIPS.iter().find(|(chip, _)| *chip == key).map(|(_, at)| *at)
}

/// The items a state lists, in order.
fn items(state: &Value) -> Vec<Value> {
    state["items"].as_array().cloned().unwrap_or_default()
}

/// The first item a state lists.
fn first(state: &Value) -> SandboxResult<Value> {
    items(state)
        .first()
        .cloned()
        .ok_or_else(|| format!("no item listed: {state}").into())
}

/// The id of the item a state lists whose title or preview has `words`.
fn item_with(state: &Value, words: &str) -> SandboxResult<Value> {
    items(state)
        .into_iter()
        .find(|item| format!("{} {}", item["title"], item["preview"]).contains(words))
        .ok_or_else(|| format!("no item with {words}: {state}").into())
}

/// A string field of a JSON value.
fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_owned()
}

/// Whether the list's area differs between two screenshots.
fn list_differs(before: &Path, after: &Path) -> SandboxResult<bool> {
    let (before, after) = (image::open(before)?.to_rgb8(), image::open(after)?.to_rgb8());
    let (left, top, right, bottom) = LIST_AREA;
    let changed = (top..bottom)
        .flat_map(|y| (left..right).map(move |x| (x, y)))
        .filter(|&(x, y)| before.get_pixel(x, y) != after.get_pixel(x, y))
        .count();
    Ok(changed > 1000)
}

fn ensure(holds: bool, what: impl FnOnce() -> String) -> SandboxResult {
    if holds { Ok(()) } else { Err(what().into()) }
}

// --- the flows ---

/// Before the pane was ever shown: the rail counts the unread items (the folded reaction once), and the list shows the
/// chats.
fn rail_counts_new_activity(qa: &mut Qa<'_>) -> SandboxResult {
    let state = qa.state()?;
    ensure(state["list"] == "Chats", || format!("the chats listed first: {state}"))?;
    ensure(state["unseen"] == 7, || {
        format!("the rail's count of unread items: {}", state["unseen"])
    })?;
    qa.shot("rail-count-light")?;
    Ok(())
}

/// `ngteamsctl activity`: the whole feed, newest first, the reactions to one message folded ("Lab B +1"), each item's
/// kind, title, place and read state; the table without `--json` too.
fn ctl_lists_the_feed(qa: &mut Qa<'_>) -> SandboxResult {
    let listed = qa.ctl_items(&[])?;
    ensure(listed.len() == usize::try_from(FILLERS)? + 10, || {
        format!("{} items listed", listed.len())
    })?;
    let times: Vec<i64> = listed.iter().filter_map(|item| item["at"].as_i64()).collect();
    ensure(times.windows(2).all(|pair| pair[0] >= pair[1]), || {
        format!("not newest first: {times:?}")
    })?;
    let newest = &listed[0];
    ensure(
        text(newest, "title") == "Lab B +1 reacted to your message" && newest["read"] == false,
        || format!("the folded reaction first: {newest}"),
    )?;
    let kinds: Vec<String> = listed.iter().map(|item| text(item, "kind")).collect();
    for kind in [
        "mention",
        "everyone",
        "reaction",
        "reply",
        "missed-call",
        "voicemail",
        "app",
    ] {
        ensure(kinds.iter().any(|shown| shown == kind), || {
            format!("no {kind} item: {kinds:?}")
        })?;
    }
    let missed = listed
        .iter()
        .find(|item| item["kind"] == "missed-call")
        .ok_or("no missed call")?;
    ensure(
        text(missed, "title") == "Missed call from Lab B" && text(missed, "place") == "Teams call",
        || format!("the missed call: {missed}"),
    )?;
    let (exit, table, err) = qa.ctl(&["activity", "--limit", "3"])?;
    ensure(
        exit == Exit::Ok && table.contains("Lab B +1 reacted to your message"),
        || format!("the table: {exit:?} {table}{err}"),
    )?;
    Ok(())
}

/// `--filter` each chip keeps its kind, `--search` the words, `--limit` the count, and a filter that isn't a chip is
/// refused with the chips listed.
fn ctl_filters_search_and_limit(qa: &mut Qa<'_>) -> SandboxResult {
    let unread = qa.ctl_items(&["--filter", "unread"])?;
    ensure(
        unread.len() == 7 && unread.iter().all(|item| item["read"] == false),
        || format!("unread: {unread:?}"),
    )?;
    for (filter, kinds) in [
        ("mentions", &["mention", "everyone"][..]),
        ("reactions", &["reaction"]),
        ("missed-calls", &["missed-call"]),
        ("replies", &["reply"]),
        ("voicemail", &["voicemail"]),
        ("apps", &["app"]),
    ] {
        let kept = qa.ctl_items(&["--filter", filter])?;
        ensure(
            !kept.is_empty() && kept.iter().all(|item| kinds.contains(&text(item, "kind").as_str())),
            || format!("--filter {filter}: {kept:?}"),
        )?;
    }
    let found = qa.ctl_items(&["--search", "Lab C"])?;
    ensure(
        !found.is_empty()
            && found
                .iter()
                .all(|item| item.to_string().to_lowercase().contains("lab c")),
        || format!("--search Lab C: {found:?}"),
    )?;
    ensure(qa.ctl_items(&["--limit", "2"])?.len() == 2, || "--limit 2".to_owned())?;
    let (exit, out, err) = qa.ctl(&["activity", "--filter", "nonsense"])?;
    ensure(
        exit != Exit::Ok && format!("{out}{err}").contains("missed-calls"),
        || format!("an unknown filter: {exit:?} {out}{err}"),
    )?;
    Ok(())
}

/// A click on the rail's Activity shows the pane (the chats' place), the count clears, and the feed's seen horizon
/// reaches the service.
fn rail_click_opens_the_pane(qa: &mut Qa<'_>) -> SandboxResult {
    qa.click(RAIL_ACTIVITY)?;
    let state = qa.state_until("the pane shown and the count gone", |state| {
        state["list"] == "Activity" && state["unseen"] == 0
    })?;
    ensure(state["pane_shown"] == true, || format!("the pane on screen: {state}"))?;
    let ui = qa.ctl_json(&["ui", "state"])?;
    ensure(ui["activity"] == true, || format!("ui state: {ui}"))?;
    let (_, words, _) = qa.ctl(&["ui", "state"])?;
    ensure(words.contains("activity"), || format!("ui state says: {words}"))?;
    qa.shot("pane-light")?;
    Ok(())
}

/// Ctrl+2 goes back to the chats, Ctrl+1 to the Activity pane.
fn keys_switch_between_activity_and_chats(qa: &mut Qa<'_>) -> SandboxResult {
    qa.chord(&[Modifier::Ctrl], '2')?;
    qa.state_until("Ctrl+2: the chats", |state| state["list"] == "Chats")?;
    ensure(qa.ctl_json(&["ui", "state"])?["activity"] != true, || {
        "ui state after Ctrl+2".to_owned()
    })?;
    qa.chord(&[Modifier::Ctrl], '1')?;
    qa.state_until("Ctrl+1: the Activity pane", |state| state["list"] == "Activity")?;
    Ok(())
}

/// `ngteamsctl open settings` then `open activity`: the pane again, `ui state` saying so.
fn ctl_open_activity_and_ui_state(qa: &mut Qa<'_>) -> SandboxResult {
    let settings = qa.ctl_json(&["open", "settings"])?;
    ensure(settings["view"] == "settings", || format!("open settings: {settings}"))?;
    let shown = qa.ctl_json(&["open", "activity"])?;
    ensure(shown["activity"] == true && shown["view"] != "settings", || {
        format!("open activity: {shown}")
    })?;
    qa.state_until("the pane after open activity", |state| state["list"] == "Activity")?;
    Ok(())
}

/// Each chip clicked keeps its kind, clicked again shows everything; Ctrl+Alt+U and Ctrl+Alt+M toggle Unread and
/// @Mentions.
fn chips_filter(qa: &mut Qa<'_>) -> SandboxResult {
    qa.plain_pane()?;
    for (chip, kinds) in [
        ("unread", &[][..]),
        ("mentions", &["mention", "everyone"][..]),
        ("reactions", &["reaction"]),
        ("missed-calls", &["missed-call"]),
        ("replies", &["reply"]),
        ("voicemail", &["voicemail"]),
        ("apps", &["app"]),
    ] {
        qa.click_chip(chip)?;
        let state = qa.state_until(&format!("the {chip} chip chosen"), |state| state["filter"] == chip)?;
        let kept = items(&state);
        let right = |item: &Value| {
            if chip == "unread" {
                item["read"] == false
            } else {
                kinds.contains(&text(item, "kind").as_str())
            }
        };
        ensure(!kept.is_empty() && kept.iter().all(right), || {
            format!("{chip}: {kept:?}")
        })?;
        if chip == "unread" || chip == "missed-calls" {
            qa.shot(&format!("chip-{chip}-light"))?;
        }
        qa.click_chip(chip)?;
        qa.state_until(&format!("the {chip} chip let go"), |state| state["filter"] == "all")?;
    }
    qa.chord(&[Modifier::Ctrl, Modifier::Alt], 'u')?;
    qa.state_until("Ctrl+Alt+U: Unread", |state| state["filter"] == "unread")?;
    qa.chord(&[Modifier::Ctrl, Modifier::Alt], 'u')?;
    qa.state_until("Ctrl+Alt+U again: all", |state| state["filter"] == "all")?;
    qa.chord(&[Modifier::Ctrl, Modifier::Alt], 'm')?;
    qa.state_until("Ctrl+Alt+M: @Mentions", |state| state["filter"] == "mentions")?;
    qa.chord(&[Modifier::Ctrl, Modifier::Alt], 'm')?;
    qa.state_until("Ctrl+Alt+M again: all", |state| state["filter"] == "all")?;
    Ok(())
}

/// The magnifier opens "Filter by name or group name": typed words keep the items with them (by name and by group
/// name), words nothing has show the empty state, and closing the box shows everything again.
fn search_filters(qa: &mut Qa<'_>) -> SandboxResult {
    let state = qa.search("Lab C")?;
    ensure(!items(&state).is_empty(), || format!("Lab C: {state}"))?;
    qa.shot("search-light")?;
    qa.click(HEADER_FILTER)?;
    qa.box_open = false;
    // the group chat (no topic) by its members' first names, as the reply's third line says it in Teams' form
    let group = text(&item_with(&qa.state()?, "Made-up reply about")?, "place");
    ensure(group.starts_with("Chat with Lab, ") && !group.contains(" and "), || {
        format!("the group's place: {group}")
    })?;
    let state = qa.search(&group)?;
    ensure(
        items(&state).len() > 1 && items(&state).iter().all(|item| text(item, "place") == group),
        || format!("the group's name {group}: {state}"),
    )?;
    qa.input.type_text(" nothing has this")?;
    qa.state_until("no item has the words", |state| items(state).is_empty())?;
    qa.shot("search-empty-light")?;
    qa.click(HEADER_FILTER)?;
    qa.box_open = false;
    let total = qa.ctl_items(&[])?.len();
    qa.state_until("the box closed: everything again", |state| items(state).len() == total)?;
    Ok(())
}

/// The pointer on a row swaps its date for "More options" and, held on the title, shows the title's tooltip.
fn hover_and_tooltip(qa: &mut Qa<'_>) -> SandboxResult {
    qa.plain_pane()?;
    qa.hover_listed(ROW)?;
    qa.shot("hover-light")?;
    qa.hover_listed(ROW_TITLE)?;
    thread::sleep(Duration::from_millis(1500));
    qa.shot("tooltip-light")?;
    qa.hover(AWAY)?;
    Ok(())
}

/// The first row's "More options": Mark as read, then Mark as unread; the pane and the mock follow.
fn item_menu_marks(qa: &mut Qa<'_>) -> SandboxResult {
    let state = qa.plain_pane()?;
    let row = first(&state)?;
    let id = text(&row, "id");
    ensure(row["read"] == false, || format!("the first row starts unread: {row}"))?;
    qa.open_row_menu()?;
    qa.shot("item-menu-light")?;
    qa.click_listed(ITEM_MENU_MARK)?;
    qa.state_until("marked read", |state| {
        items(state)
            .iter()
            .any(|item| text(item, "id") == id && item["read"] == true)
    })?;
    qa.server_until(&id, "isread true on the service", |held| held["isread"] == true)?;
    qa.open_row_menu()?;
    qa.click_listed(ITEM_MENU_MARK)?;
    qa.state_until("marked unread", |state| {
        items(state)
            .iter()
            .any(|item| text(item, "id") == id && item["read"] == false)
    })?;
    qa.server_until(&id, "isread false on the service", |held| held["isread"] == false)?;
    Ok(())
}

/// "Notification settings" in an item's menu opens Settings at Notifications.
fn item_menu_notification_settings(qa: &mut Qa<'_>) -> SandboxResult {
    qa.plain_pane()?;
    qa.open_row_menu()?;
    qa.click_listed(ITEM_MENU_SETTINGS)?;
    let state = qa.state_until("Settings at Notifications", |state| state["main"] == "Settings")?;
    ensure(state["settings_page"] == "Notifications", || {
        format!("the page: {state}")
    })?;
    let ui = qa.ctl_json(&["ui", "state"])?;
    ensure(ui["view"] == "settings", || format!("ui state: {ui}"))?;
    qa.shot("notification-settings-light")?;
    Ok(())
}

/// Remove hides the item, says "Removed", and the service has it removed.
fn item_menu_remove(qa: &mut Qa<'_>) -> SandboxResult {
    let state = qa.search("filler note 7")?;
    let row = first(&state)?;
    let id = text(&row, "id");
    qa.open_row_menu()?;
    qa.click_listed(ITEM_MENU_REMOVE)?;
    let state = qa.state_until("the item gone", |state| {
        !items(state).iter().any(|item| text(item, "id") == id)
    })?;
    ensure(state["toast"] == "Removed", || format!("the toast: {}", state["toast"]))?;
    qa.shot("removed-toast-light")?;
    qa.server_until(&id, "removed on the service", |held| held["removed"] == true)?;
    let listed = qa.ctl_items(&[])?;
    ensure(!listed.iter().any(|item| text(item, "id") == id), || {
        "ngteamsctl still lists it".to_owned()
    })?;
    Ok(())
}

/// A click on a mention opens its chat at the message, the item read here and on the service.
fn click_opens_the_item(qa: &mut Qa<'_>) -> SandboxResult {
    let state = qa.search("check the made-up numbers")?;
    let row = item_with(&state, "check the made-up numbers")?;
    let id = text(&row, "id");
    qa.click_listed(ROW)?;
    qa.state_until("its chat open at the message", |state| {
        state["chat"] == ABC_CHAT && state["revealed"] == row["message"]
    })?;
    qa.state_until("the item read", |state| {
        items(state)
            .iter()
            .any(|item| text(item, "id") == id && item["read"] == true)
    })?;
    qa.server_until(&id, "isread true on the service", |held| held["isread"] == true)?;
    let ui = qa.ctl_json(&["ui", "state"])?;
    ensure(ui["chat"] == ABC_CHAT, || format!("ui state: {ui}"))?;
    qa.shot("opened-mention-light")?;
    Ok(())
}

/// Home then Enter in the pane opens the first item (the folded reaction: a's chat with b).
fn keyboard_walks_and_opens(qa: &mut Qa<'_>) -> SandboxResult {
    let state = qa.plain_pane()?;
    let row = first(&state)?;
    // the list takes the keys once a row is clicked; the first row's click opens it, so walk from the second
    qa.key(keys::HOME)?;
    qa.key(keys::DOWN)?;
    qa.key(keys::UP)?;
    qa.key(keys::ENTER)?;
    qa.state_until("Enter opened the first item at its message", |state| {
        state["chat"] == row["thread"] && state["revealed"] == row["message"]
    })?;
    Ok(())
}

/// A missed call: Chat opens the 1:1 chat with the caller; Call calls back through it.
fn missed_call_buttons(qa: &mut Qa<'_>) -> SandboxResult {
    qa.plain_pane()?;
    qa.ctl_json(&["open", "chat", ABC_CHAT])?;
    qa.chord(&[Modifier::Ctrl], '1')?;
    qa.click_chip("missed-calls")?;
    let state = qa.state_until("the missed calls", |state| state["filter"] == "missed-calls")?;
    ensure(text(&first(&state)?, "kind") == "missed-call", || format!("{state}"))?;
    qa.hover_listed(ROW)?;
    qa.shot("missed-call-light")?;
    let first_read = |state: &Value| items(state).first().is_some_and(|item| item["read"] == true);
    qa.click_listed(ROW_CHAT)?;
    qa.state_until("Chat: the 1:1 chat with b, the missed call read", |state| {
        state["chat"] == AB_CHAT && first_read(state)
    })?;
    qa.ctl_json(&["open", "chat", ABC_CHAT])?;
    qa.chord(&[Modifier::Ctrl], '1')?;
    // unread again, as a user would, to see Call read it too
    qa.open_row_menu()?;
    qa.click_listed(ITEM_MENU_MARK)?;
    qa.state_until("the missed call unread", |state| !first_read(state))?;
    qa.trace.drain();
    qa.click_listed(ROW_CALL)?;
    qa.state_until("Call: the 1:1 chat with b, the missed call read", |state| {
        state["chat"] == AB_CHAT && first_read(state)
    })?;
    let asked = qa.trace.next_where("a call asked in the 1:1 chat", |line| {
        line["view"] == "call" && line["change"] == "asked" && line.to_string().contains(AB_CHAT)
    })?;
    say(&format!("missed call's Call: {asked}"));
    // the test engine has no calling: the banner saying so pushes the panes down until it is closed
    qa.shot("missed-call-called-light")?;
    qa.click(BANNER_CLOSE)?;
    Ok(())
}

/// A meeting's update shows the meeting's time as its third line.
fn meeting_item(qa: &mut Qa<'_>) -> SandboxResult {
    let state = qa.search("weekly sync")?;
    let row = item_with(&state, "weekly sync")?;
    ensure(text(&row, "title") == "Lab C updated", || {
        format!("the meeting item: {row}")
    })?;
    let line = text(&row, "meeting_line");
    ensure(line.contains(" - ") && line.contains("Oct 10"), || {
        format!("its time line: {row}")
    })?;
    qa.hover(AWAY)?;
    qa.shot("meeting-light")?;
    Ok(())
}

/// A chat opened (here by `ngteamsctl open chat`) has its unread items read, and an item arriving for it while it is
/// on screen is read at once, with no count on the rail.
fn chat_on_screen(qa: &mut Qa<'_>) -> SandboxResult {
    // another chat on screen first: the a–b chat is then entered anew
    qa.ctl_json(&["open", "chat", ABC_CHAT])?;
    let state = qa.plain_pane()?;
    let reaction = first(&state)?;
    ensure(text(&reaction, "kind") == "reaction", || {
        format!("the reaction first: {reaction}")
    })?;
    let id = text(&reaction, "id");
    if reaction["read"] == true {
        // the earlier flows read it: unread again, as a user would, to see the chat read it
        qa.open_row_menu()?;
        qa.click_listed(ITEM_MENU_MARK)?;
        qa.state_until("the reaction unread", |state| {
            items(state)
                .iter()
                .any(|item| text(item, "id") == id && item["read"] == false)
        })?;
    }
    qa.chord(&[Modifier::Ctrl], '2')?;
    qa.ctl_json(&["open", "chat", AB_CHAT])?;
    qa.state_until("the reaction read with its chat open", |state| {
        state["chat"] == AB_CHAT
            && items(state)
                .iter()
                .any(|item| text(item, "id") == id && item["read"] == true)
    })?;
    qa.server_until(&id, "isread true on the service", |held| held["isread"] == true)?;
    let pushed = qa.ask("push-reaction")?;
    let new = text(&pushed, "id");
    let state = qa.state_until("the new item for the chat on screen, read", |state| {
        items(state)
            .iter()
            .any(|item| text(item, "id") == new && item["read"] == true)
    })?;
    // read on arrival: the rail never counts it, not even while its change syncs back
    ensure(state["unseen"] == 0, || {
        format!("rail count while the new item already shows read: {}", state["unseen"])
    })?;
    qa.server_until(&new, "the new item read on the service", |held| held["isread"] == true)?;
    qa.state_until("no count on the rail for it", |state| state["unseen"] == 0)?;
    Ok(())
}

/// New activity for a chat not on screen arrives live and the rail counts it; showing the pane clears the count.
fn live_activity_counts(qa: &mut Qa<'_>) -> SandboxResult {
    qa.chord(&[Modifier::Ctrl], '2')?;
    qa.ctl_json(&["open", "chat", AB_CHAT])?;
    let pushed = qa.ask("push-mention")?;
    let new = text(&pushed, "id");
    let state = qa.state_until("the new mention counted", |state| {
        state["unseen"].as_u64().is_some_and(|count| count >= 1)
            && items(state)
                .iter()
                .any(|item| text(item, "id") == new && item["read"] == false)
    })?;
    say(&format!("rail count after a live mention: {}", state["unseen"]));
    qa.shot("live-count-light")?;
    let listed = qa.ctl_items(&["--filter", "unread"])?;
    ensure(listed.iter().any(|item| text(item, "id") == new), || {
        format!("ngteamsctl: {listed:?}")
    })?;
    qa.click(RAIL_ACTIVITY)?;
    qa.state_until("the count cleared", |state| {
        state["list"] == "Activity" && state["unseen"] == 0
    })?;
    Ok(())
}

/// Shift+Esc in the pane reads every item (the Unread chip then shows its empty state); the header's "More options" →
/// "Mark all as read" asks the same. (Once the feed's all-read bookmark is set, no later mock item can be unread: the
/// mock's clock is in October 2025 while the bookmark is the computer's time, so the menu's run is seen in the trace.)
fn mark_all_read(qa: &mut Qa<'_>) -> SandboxResult {
    // the mention comes in the a–b–c chat: another chat on screen, so it isn't read on arrival
    qa.ctl_json(&["open", "chat", AB_CHAT])?;
    let pushed = qa.ask("push-mention")?;
    let new = text(&pushed, "id");
    qa.plain_pane()?;
    qa.state_until("the new mention unread", |state| {
        items(state)
            .iter()
            .any(|item| text(item, "id") == new && item["read"] == false)
    })?;
    qa.key(keys::HOME)?;
    qa.input.key_with(&[Modifier::Shift], keys::ESC)?;
    qa.state_until("Shift+Esc: everything read", |state| {
        items(state).iter().all(|item| item["read"] == true)
    })?;
    qa.click_chip("unread")?;
    qa.state_until("Unread: nothing", |state| {
        state["filter"] == "unread" && items(state).is_empty()
    })?;
    qa.shot("unread-empty-light")?;
    qa.click_chip("unread")?;
    wait_until(WAIT, || {
        qa.ctl_items(&["--filter", "unread"])
            .ok()
            .filter(Vec::is_empty)
            .or_else(|| {
                thread::sleep(Duration::from_millis(200));
                None
            })
    })
    .ok_or("ngteamsctl still lists unread items after Shift+Esc")?;
    qa.trace.drain();
    qa.click(HEADER_MORE)?;
    qa.state_until("the header menu open", |state| state["header_menu"] == true)?;
    qa.shot("header-menu-light")?;
    qa.click(HEADER_MENU_MARK_ALL)?;
    qa.trace.next_where("the menu's Mark all as read", |line| {
        line["view"] == "activity" && line["change"] == "all-read"
    })?;
    qa.state_until("the header menu closed", |state| state["header_menu"] == false)?;
    Ok(())
}

/// The list scrolls to its oldest items: the wheel, and End then Enter opening the oldest.
fn many_items_scroll(qa: &mut Qa<'_>) -> SandboxResult {
    let state = qa.plain_pane()?;
    let oldest = items(&state).last().cloned().ok_or("no items")?;
    let top = qa.shot("top-light")?;
    // the pointer rests on the list first, as a hand does: the wheel goes to what the pointer is over
    qa.hover(LIST_MIDDLE)?;
    qa.input.scroll(LIST_MIDDLE.0, LIST_MIDDLE.1, 20)?;
    let scrolled = qa.shot("scrolled-light")?;
    ensure(list_differs(&top, &scrolled)?, || {
        "the wheel didn't scroll the list".to_owned()
    })?;
    qa.key(keys::END)?;
    qa.key(keys::ENTER)?;
    qa.state_until("End, Enter: the oldest opened at its message", |state| {
        state["chat"] == oldest["thread"] && state["revealed"] == oldest["message"]
    })?;
    // back to the top for the flows after
    qa.hover(LIST_MIDDLE)?;
    qa.input.scroll(LIST_MIDDLE.0, LIST_MIDDLE.1, -80)?;
    thread::sleep(SETTLE);
    Ok(())
}

/// An item whose chat is gone: its third line names the person behind it; clicking it reads it and a toast says the
/// chat isn't available, no empty chat opened to write in; the app keeps working.
fn gone_chat_item(qa: &mut Qa<'_>) -> SandboxResult {
    let state = qa.search("the chat that is gone")?;
    let row = item_with(&state, "the chat that is gone")?;
    ensure(text(&row, "place") == "Chat with Lab", || {
        format!("its third line: {row}")
    })?;
    let id = text(&row, "id");
    qa.click_listed(ROW)?;
    let state = qa.state_until("the gone chat's item read", |state| {
        items(state)
            .iter()
            .any(|item| text(item, "id") == id && item["read"] == true)
    })?;
    ensure(
        state["chat"] != GONE_CHAT && state["toast"] == teams_ui::activity_view::CHAT_GONE,
        || {
            format!(
                "gone chat clicked: main {} chat {} toast {}",
                state["main"], state["chat"], state["toast"]
            )
        },
    )?;
    qa.shot("gone-chat-light")?;
    let (exit, out, err) = qa.ctl(&["status"])?;
    ensure(exit == Exit::Ok, || format!("the app after it: {out}{err}"))?;
    Ok(())
}

/// Teams Dark: the pane, a row hovered, its menu and the header's.
fn dark_theme(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ask("theme dark")?;
    qa.plain_pane()?;
    qa.shot("pane-dark")?;
    qa.hover_listed(ROW)?;
    qa.shot("hover-dark")?;
    qa.open_row_menu()?;
    qa.shot("item-menu-dark")?;
    qa.key(keys::ESC)?;
    qa.hover(AWAY)?;
    qa.click(HEADER_MORE)?;
    qa.state_until("the header menu open", |state| state["header_menu"] == true)?;
    qa.shot("header-menu-dark")?;
    qa.key(keys::ESC)?;
    qa.ask("theme light")?;
    Ok(())
}

// --- the child: the app on the mock, answering the parent's questions ---

/// After `frames` more frames of `window` were presented, runs `done`.
fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

/// One account's mock token, for the transport and the push channel alike; it never runs out in a test.
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

/// The mock with a's feed seeded, and a's engine on it.
struct MockLab {
    _runtime: tokio::runtime::Runtime,
    mock: Arc<ChatServiceMock>,
    _trouter: TrouterMock,
    dir: tempfile::TempDir,
}

impl MockLab {
    fn start() -> SandboxResult<(Self, EngineHandle)> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        seed_feed(&mock)?;
        let dir = tempfile::tempdir()?;
        let tokens = Arc::new(Tokens {
            token: mock.token(LAB_A).ok_or("the mock's token for a")?,
            renewals: broadcast::channel(4).0,
        });
        let origin = mock.origin();
        let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
        let transport = Arc::new(http()?);
        let trouter_client = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), trouter.origin())?
            .with_options(Options {
                backoff_base: Duration::from_millis(50),
                backoff_cap: Duration::from_millis(50),
                backoff_jitter_percent: 0,
                ..Options::default()
            });
        let author = Author::new(Mri::new(LAB_A), display_name("bot-a@lab.example.test"));
        let engine = engine::start(Parts {
            calls: None,
            store: Store::open(&support::store_file(dir.path(), Alias::A))?,
            // the people's names and pictures, as the app reads them
            client: Arc::new(Client::new(
                http()?
                    .with_presence(PRESENCE_BASE)?
                    .with_csa(CSA_BASE)?
                    .with_user_profile(USER_PROFILE_BASE)?,
            )),
            outbox: Outbox::new(transport, author),
            push: Some(Push::new(trouter_client)),
            options: EngineOptions::default(),
            known_names: Vec::new(),
        })?;
        let lab = Self {
            _runtime: runtime,
            mock: Arc::new(mock),
            _trouter: trouter,
            dir,
        };
        Ok((lab, engine))
    }
}

/// A made-up feed item's activity: `kind`/`sub` from `who` about `message` of `thread`, with `preview`.
fn activity(kind: &str, sub: &str, (from, name): (&str, &str), (thread, message): (&str, u64), preview: &str) -> Value {
    json!({
        "activityType": kind, "activitySubtype": sub, "activityId": 200_000 + message,
        "sourceThreadId": thread, "sourceMessageId": message, "sourceUserId": from,
        "sourceUserImDisplayName": name, "messagePreview": preview,
        "sourceThreadType": if thread.ends_with("tacv2") { "space" } else { "chat" },
    })
}

/// The same, happened `days` before the mock's clock starts (its items arrive from 9 Oct 2025 on).
fn older(mut activity: Value, days: i64) -> Value {
    let at = MOCK_EPOCH_MS - days * DAY_MS;
    let stamp = chrono::DateTime::from_timestamp_millis(at)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default();
    activity["activityTimestamp"] = json!(stamp);
    activity
}

/// A meeting c updated (the Calendar app's item, attributed to c), on 10 Oct 2025 14:30–15:00 UTC.
fn meeting_update() -> Value {
    json!({
        "activityType": "msGraph", "activitySubtype": "privateMeetingUpdated", "activityId": 8,
        "sourceThreadId": "", "sourceMessageId": 1, "sourceUserId": "28:app:made-up-calendar",
        "sourceUserImDisplayName": "Lab C", "messagePreview": "Made-up weekly sync",
        "activityContext": {
            "attributedToActorId": LAB_C,
            "location": "{\"Custom\":\"<DateTimeRange><StartDateTime>2025-10-10T14:30:00Z</StartDateTime><EndDateTime>2025-10-10T15:00:00Z</EndDateTime></DateTimeRange>\"}",
        },
    })
}

/// An app's note in the team's channel.
fn app_note() -> Value {
    json!({
        "activityType": "msGraph", "activitySubtype": "customNotification", "activityId": 7,
        "sourceThreadId": TEAM, "sourceMessageId": 1, "sourceUserImDisplayName": "Workflows",
        "messagePreview": "Made-up reminder to follow up", "sourceThreadType": "space",
        "activityContext": {"templateParameters": "{\"customMessage\":\"Follow up on a message\"}"},
    })
}

/// The feed's newer items, oldest first, each read or not: `mine` is a's message in the 1:1 chat, `theirs` c's in the
/// group chat.
fn recent_items(mine: u64, theirs: u64) -> [(Value, bool); 11] {
    let b_ab = (LAB_B, "Lab B");
    let c = (LAB_C, "Lab C");
    [
        (
            activity("teamMembershipChange", "addedToTeam", b_ab, (TEAM, 1), ""),
            true,
        ),
        (app_note(), true),
        (
            activity(
                "mentionInChat",
                "everyone",
                b_ab,
                (ABC_CHAT, theirs),
                "Everyone the made-up demo moved",
            ),
            true,
        ),
        (activity("call", "voicemail", b_ab, ("", 1), ""), false),
        (
            activity("reply", "", c, (ABC_CHAT, theirs), "Made-up reply about the numbers"),
            false,
        ),
        (
            activity(
                "mentionInChat",
                "person",
                b_ab,
                (GONE_CHAT, 5),
                "Made-up words in the chat that is gone",
            ),
            false,
        ),
        (activity("call", "missedCall", b_ab, ("", 1), ""), false),
        (meeting_update(), false),
        (
            activity(
                "mentionInChat",
                "person",
                c,
                (ABC_CHAT, theirs),
                "Lab A can you check the made-up numbers before noon?",
            ),
            false,
        ),
        (
            activity(
                "reactionInChat",
                "like",
                c,
                (AB_CHAT, mine),
                "The made-up report is ready for a look",
            ),
            true,
        ),
        (
            activity(
                "reactionInChat",
                "heart",
                b_ab,
                (AB_CHAT, mine),
                "The made-up report is ready for a look",
            ),
            false,
        ),
    ]
}

/// a's feed (see the module docs), oldest first.
fn seed_feed(mock: &ChatServiceMock) -> SandboxResult {
    let mine: u64 = mock
        .post_as(LAB_A, AB_CHAT, "<p>The made-up report is ready for a look</p>")
        .ok_or("a's post refused")?
        .parse()?;
    let theirs: u64 = mock
        .post_as(
            LAB_C,
            ABC_CHAT,
            "<p>Lab A can you check the made-up numbers before noon?</p>",
        )
        .ok_or("c's post refused")?
        .parse()?;
    let b_ab = (LAB_B, "Lab B");
    let mut feed: Vec<(Value, bool)> = Vec::new();
    for note in 0..FILLERS {
        let days = i64::try_from(FILLERS - note)? + 1;
        let filler = activity(
            "reply",
            "",
            b_ab,
            (ABC_CHAT, theirs),
            &format!("Made-up filler note {note}"),
        );
        feed.push((older(filler, days), true));
    }
    feed.extend(recent_items(mine, theirs));
    for (item, read) in feed {
        mock.add_activity(LAB_A, &item, read).ok_or("no feed")?;
    }
    Ok(())
}

/// The trace descriptor the parent passed.
fn passed_trace_fd() -> SandboxResult<RawFd> {
    Ok(std::env::var(TRACE_FD_ENV)?.parse()?)
}

fn child_app() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(passed_trace_fd()?)?);
    let dir = PathBuf::from(std::env::var_os(ui_sandbox::SANDBOX_ENV).ok_or("no sandbox dir")?);
    let (lab, engine) = MockLab::start()?;
    // the calls shown (Call buttons ask them); the engine has no calling, so a call asked is refused there
    let first = ngteams::accounts::Started {
        calls: Some(PersonalCalls {
            ringer: None,
            sounds: None,
            echo: Mri::new(teams_mock::ECHO),
            video: None,
            camera: None,
            devices: None,
        }),
        ..support::started(Alias::A, engine)?
    };
    let settings = teams_ui::SettingsFile::new(lab.dir.path().join("ui").join("a.toml"));
    let runner = support::runner(lab.dir.path())?;
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let mock = Arc::clone(&lab.mock);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: Vec::new(),
                trace: Arc::new(trace),
                ctl: Some(dir.join(CTL_SOCKET)),
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
                    after_frames(window, READY_FRAMES, |_, _| say("ready"));
                });
                answer(window, mock, dir.join(ASK), cx);
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
    drop(lab);
    Ok(())
}

/// The window's shell.
fn shell_of(window: WindowHandle<KitRoot>, cx: &mut AsyncApp) -> Option<Entity<Shell>> {
    window
        .update(cx, |kit, _, cx| {
            let root = kit.view().clone().downcast::<Root>().ok()?;
            root.read(cx).shell_in(cx).cloned()
        })
        .ok()
        .flatten()
}

/// Answers each question the parent writes to `ask`, one `answer <json>` line each.
fn answer(window: WindowHandle<KitRoot>, mock: Arc<ChatServiceMock>, ask: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(50)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let question = question.trim().to_owned();
            if question == "quit" {
                say("answer {}");
                cx.update(|cx| cx.quit());
                return;
            }
            let Some(shell) = shell_of(window, cx) else {
                say("answer {\"error\":\"no shell\"}");
                continue;
            };
            let reply = window
                .update(cx, |_, _, cx| reply_to(&question, &shell, &mock, cx))
                .unwrap_or_else(|_| json!({"error": "no window"}));
            say(&format!("answer {reply}"));
        }
    })
    .detach();
}

/// The answer to one question.
fn reply_to(question: &str, shell: &Entity<Shell>, mock: &ChatServiceMock, cx: &mut App) -> Value {
    let (verb, rest) = question.split_once(' ').unwrap_or((question, ""));
    match verb {
        "state" => state_of(shell, cx),
        "server" => server_copy(mock, rest),
        "push-reaction" => push_reaction(mock),
        "push-mention" => push_mention(mock),
        "theme" => {
            let key = if rest == "dark" {
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
            json!({"theme": key})
        }
        _ => json!({"error": format!("unknown question {question}")}),
    }
}

/// What the window holds: the list's place, the rail's count, the pane's chip, search and menus, its items as listed
/// (each with its read state as the pane draws it and a meeting's time line), the main view, the chat open and the
/// message it was opened at, the settings' page and the toast.
fn state_of(shell: &Entity<Shell>, cx: &App) -> Value {
    let shell = shell.read(cx);
    let view = shell.activity_view().read(cx);
    let cycle = teams_ui::clock::cycle(cx);
    let listed: Vec<Value> = view
        .shown_items(cx)
        .into_iter()
        .map(|item| {
            let meeting_line = item
                .meeting_time
                .map(|(start, end)| teams_ui::activity_view::meeting_span(start, end, &chrono::Local, cycle));
            json!({
                "id": item.id, "kind": item.kind.key(), "title": item.title(), "preview": item.preview,
                "place": view.place(item, cx), "read": view.is_read(item), "thread": item.thread,
                "message": item.message, "meeting_line": meeting_line,
            })
        })
        .collect();
    let (header_menu, item_menu) = view.menus();
    let chat = shell.chat().map(|chat| chat.read(cx));
    json!({
        "list": format!("{:?}", shell.list_place()),
        "pane_shown": view.shown(),
        "unseen": view.unseen(),
        "filter": view.filter().key(),
        "items": listed,
        "header_menu": header_menu,
        "item_menu": item_menu,
        "main": main_name(shell),
        "chat": chat.map(teams_ui::chat_view::ChatView::conversation),
        "revealed": chat.and_then(teams_ui::chat_view::ChatView::selected),
        "settings_page": format!("{:?}", shell.settings_view().read(cx).page()),
        "toast": shell.toast().map(ToString::to_string),
    })
}

/// The main view's kind, without its id.
fn main_name(shell: &Shell) -> String {
    let main = format!("{:?}", shell.main());
    main.split('(').next().unwrap_or_default().to_owned()
}

/// The mock's copy of feed item `id`: its `isread` and whether it was removed.
fn server_copy(mock: &ChatServiceMock, id: &str) -> Value {
    let Ok(number) = id.parse::<u64>() else {
        return json!({"error": "not an id"});
    };
    let mut held = json!({"found": false});
    let client_id = (FEED_CLIENT_ID_BASE + number).to_string();
    mock.change_message(NOTIFICATIONS, &client_id, |message| {
        let properties = &message["properties"];
        held = json!({
            "found": true,
            "isread": properties["isread"].as_bool().or_else(|| properties["isread"].as_str().map(|text| text == "true")),
            "removed": properties.get("deletetime").is_some(),
        });
    });
    held
}

/// a posts a new message to the a–b chat and c reacts to it, as the feed then tells a.
fn push_reaction(mock: &ChatServiceMock) -> Value {
    let Some(message) = mock
        .post_as(LAB_A, AB_CHAT, "<p>A second made-up report</p>")
        .and_then(|id| id.parse::<u64>().ok())
    else {
        return json!({"error": "a's post refused"});
    };
    let item = activity(
        "reactionInChat",
        "like",
        (LAB_C, "Lab C"),
        (AB_CHAT, message),
        "A second made-up report",
    );
    mock.add_activity(LAB_A, &item, false)
        .map_or_else(|| json!({"error": "no feed"}), |id| json!({"id": id}))
}

/// c posts in the group chat mentioning a, as the feed then tells a.
fn push_mention(mock: &ChatServiceMock) -> Value {
    let Some(message) = mock
        .post_as(LAB_C, ABC_CHAT, "<p>Lab A one more made-up question</p>")
        .and_then(|id| id.parse::<u64>().ok())
    else {
        return json!({"error": "c's post refused"});
    };
    let item = activity(
        "mentionInChat",
        "person",
        (LAB_C, "Lab C"),
        (ABC_CHAT, message),
        "Lab A one more made-up question",
    );
    mock.add_activity(LAB_A, &item, false)
        .map_or_else(|| json!({"error": "no feed"}), |id| json!({"id": id}))
}
