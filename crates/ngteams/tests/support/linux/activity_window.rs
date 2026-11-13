//! The Activity pane as a user meets it, in the UI sandbox on the mock (`harness = false`: GPUI runs on the process's
//! main thread); an app item's "Turn off <app>" is in `activity_turn_off.rs`, the desktop banners' pictures in
//! `activity_banner_pictures.rs`.
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
use std::time::{Duration, Instant};
use std::{fs, future, io, thread};

use clap::Parser as _;
use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
use ngteams::calling::PersonalCalls;
#[path = "activity_banner_pictures.rs"]
mod pictures;
#[path = "../window.rs"]
mod support;
#[path = "activity_tile_runs.rs"]
mod tile_runs;
#[path = "activity_app_tiles.rs"]
mod tiles;
#[path = "activity_turn_off.rs"]
mod turn_off;
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
    AB_CHAT, ABC_CHAT, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE, NOTIFICATIONS,
    PRESENCE_BASE, TrouterMock, USER_PROFILE_BASE,
};
use teams_proto::Mri;
use teams_proto::{Author, Client, Credentials, HttpError, HttpTransport, Transport as _};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::desktop::Desktop;
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
/// Around the first row's avatar (left, top, right, bottom): an app's tile there.
const FIRST_AVATAR: (u32, u32, u32, u32) = (80, 160, 150, 215);
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
        Ok(mode @ ("app" | "read-only" | "tiles-v1" | "tiles-v2" | pictures::MODE)) => child_app(mode),
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
    /// When the app's child was spawned: no request of its own came before.
    launched: Instant,
    passed: Vec<&'static str>,
    failed: Vec<(&'static str, String)>,
    /// Whether the search's box is open (it moves the chips and rows down by [`SEARCH_SHIFT`]).
    box_open: bool,
}

/// One user flow, by name.
type Check = (&'static str, fn(&mut Qa<'_>) -> SandboxResult);

/// The flows, in the order a user meets them (each leaves the window as the next expects it).
const CHECKS: [Check; 26] = [
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
    (
        "reopened_pane_opens_its_newest_again_chip_cleared_keys_on_the_pane",
        newest_opens_each_time,
    ),
    ("missed_call_chat_and_call", missed_call_buttons),
    ("meeting_item_time_line", meeting_item),
    ("chat_on_screen_reads_its_items_and_new_ones", chat_on_screen),
    ("live_activity_counts_on_the_rail", live_activity_counts),
    ("newest_missed_call_opens_the_chat_with_the_caller", newest_missed_call),
    ("newest_whose_chat_is_gone_opens_nothing_quietly", newest_gone_chat),
    (
        "newest_meeting_only_selected_enter_and_click_open_its_card",
        newest_meeting,
    ),
    ("meeting_the_calendar_doesnt_know_shows_its_day", unknown_meeting),
    ("many_items_scroll_to_the_oldest", many_items_scroll),
    ("item_whose_chat_is_gone", gone_chat_item),
    ("mark_all_read_by_shift_esc_and_menu", mark_all_read),
];

fn run_checks(sandbox: &Sandbox) -> SandboxResult<usize> {
    let mut qa = Qa::start(sandbox, "app")?;
    for (name, check) in CHECKS {
        qa.check(name, check);
    }
    // then the same pane in Teams Dark, its hover and menus
    qa.check("dark_theme_pane_hover_and_menus", dark_theme);
    qa.check("dark_theme_gone_chat_group_place_missed_call", dark_fixed_states);
    // an app item's "Turn off <app>" (#27)
    qa.check(
        "app_item_shows_a_banner_while_its_notifications_are_on",
        turn_off::banner_while_on,
    );
    qa.check(
        "app_item_banner_opens_a_window_closed_into_the_tray_at_the_item",
        turn_off::banner_reopens_the_window,
    );
    qa.check(
        "dark_theme_app_item_banner_opens_a_window_closed_into_the_tray",
        turn_off::dark_banner_reopens_the_window,
    );
    for (name, check) in turn_off::CHECKS {
        qa.check(name, check);
    }
    // an app item's tile (#28): last, as its second app's item comes first under Apps from then on
    for (name, check) in tiles::CHECKS {
        qa.check(name, check);
    }
    let (mut passed, mut failed) = (qa.passed.clone(), qa.failed.clone());
    qa.quit()?;
    // the same feed in the read-only mode, the app again in the sandbox
    let _ = fs::remove_file(sandbox.path(CTL_SOCKET));
    let mut qa = Qa::start(sandbox, "read-only")?;
    qa.check("read_only_offers_no_turn_off", turn_off::read_only_offers_none);
    passed.append(&mut qa.passed.clone());
    failed.append(&mut qa.failed.clone());
    qa.quit()?;
    // an app's tile across runs (#28): its read, its cache and a new version, the app quit and started again
    let (mut tile_passed, mut tile_failed) = tile_runs::run(sandbox)?;
    passed.append(&mut tile_passed);
    failed.append(&mut tile_failed);
    // the desktop banners' pictures (#114): people's and apps', in a run of their own
    let (mut picture_passed, mut picture_failed) = pictures::run(sandbox)?;
    passed.append(&mut picture_passed);
    failed.append(&mut picture_failed);
    for (name, error) in &failed {
        say(&format!("FAILED {name}: {error}"));
    }
    say(&format!(
        "activity checks: {} passed, {} failed",
        passed.len(),
        failed.len()
    ));
    Ok(failed.len())
}

impl<'a> Qa<'a> {
    /// The app started in the sandbox in `mode` (`app`, or `read-only`: the read-only mode), its feed listed.
    fn start(sandbox: &'a Sandbox, mode: &str) -> SandboxResult<Self> {
        Self::start_in(sandbox, mode, &[])
    }

    /// [`Self::start`], with `env` set for the child too.
    fn start_in(sandbox: &'a Sandbox, mode: &str, env: &[(&str, &str)]) -> SandboxResult<Self> {
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let mut vars = vec![(CHILD_ENV, mode), (TRACE_FD_ENV, fd.as_str())];
        vars.extend_from_slice(env);
        let launched = Instant::now();
        let child = sandbox.spawn_with_fds(&std::env::current_exe()?, &[], &vars, &[writer.as_fd()])?;
        drop(writer);
        let mut running = Running::new(child)?;
        let trace = TraceReader::start(reader);
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
        let qa = Qa {
            sandbox,
            running,
            trace,
            input: sandbox.input()?,
            socket: sandbox.path(CTL_SOCKET),
            launched,
            passed: Vec::new(),
            failed: Vec::new(),
            box_open: false,
        };
        if mode.starts_with("tiles") {
            tile_runs::settled(&qa)?;
        } else {
            qa.settled()?;
        }
        Ok(qa)
    }
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
/// reaches the service; as Teams' pane does, it opens its newest item (the folded reaction: a's chat with b at the
/// message), read here and on the service.
fn rail_click_opens_the_pane(qa: &mut Qa<'_>) -> SandboxResult {
    let newest = qa.ctl_items(&[])?.first().cloned().ok_or("no items")?;
    let id = text(&newest, "id");
    qa.click(RAIL_ACTIVITY)?;
    let state = qa.state_until("the pane shown and the count gone", |state| {
        state["list"] == "Activity" && state["unseen"] == 0
    })?;
    qa.state_until("the newest item opened beside the pane, read", |state| {
        state["chat"] == AB_CHAT
            && state["revealed"] == newest["message"]
            && items(state)
                .iter()
                .any(|item| text(item, "id") == id && item["read"] == true)
    })?;
    qa.server_until(&id, "the newest item's isread true on the service", |held| {
        held["isread"] == true
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
    qa.hover(AWAY)?;
    qa.shot("group-place-light")?;
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

/// The first row's "More options": Mark as unread (the pane read it, opening it), then Mark as read; the pane and the
/// mock follow.
fn item_menu_marks(qa: &mut Qa<'_>) -> SandboxResult {
    let state = qa.plain_pane()?;
    let row = first(&state)?;
    let id = text(&row, "id");
    ensure(row["read"] == true, || {
        format!("the first row read, opened with the pane: {row}")
    })?;
    qa.open_row_menu()?;
    qa.shot("item-menu-light")?;
    qa.click_listed(ITEM_MENU_MARK)?;
    qa.state_until("marked unread", |state| {
        items(state)
            .iter()
            .any(|item| text(item, "id") == id && item["read"] == false)
    })?;
    qa.server_until(&id, "isread false on the service", |held| held["isread"] == false)?;
    qa.open_row_menu()?;
    qa.click_listed(ITEM_MENU_MARK)?;
    qa.state_until("marked read", |state| {
        items(state)
            .iter()
            .any(|item| text(item, "id") == id && item["read"] == true)
    })?;
    qa.server_until(&id, "isread true on the service", |held| held["isread"] == true)?;
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

/// Teams doesn't keep the last pick: after another item was opened and a chip chosen, leaving the pane and coming
/// back (Ctrl+1, then the rail with @Mentions chosen) opens the newest item again beside the pane, the chip let go;
/// the keyboard stays on the pane (Down, Enter opens the second item).
fn newest_opens_each_time(qa: &mut Qa<'_>) -> SandboxResult {
    let state = qa.plain_pane()?;
    let listed = items(&state);
    let (newest, second) = (
        listed.first().cloned().ok_or("no items")?,
        listed.get(1).cloned().ok_or("one item")?,
    );
    ensure(newest["thread"] != second["thread"], || {
        format!("the two newest in one chat: {newest} {second}")
    })?;
    let opened_at = |item: &Value| {
        let item = item.clone();
        move |state: &Value| state["chat"] == item["thread"] && state["revealed"] == item["message"]
    };
    // another item picked, the search closed again, a chip chosen
    let other = item_with(&qa.search("check the made-up numbers")?, "check the made-up numbers")?;
    qa.click_listed(ROW)?;
    qa.state_until("the other item opened", opened_at(&other))?;
    qa.click(HEADER_FILTER)?;
    qa.box_open = false;
    qa.click_chip("unread")?;
    qa.state_until("the Unread chip chosen", |state| state["filter"] == "unread")?;
    qa.chord(&[Modifier::Ctrl], '2')?;
    let away = qa.state_until("Ctrl+2: the chats", |state| state["list"] == "Chats")?;
    ensure(away["chat"] == other["thread"], || {
        format!("the picked chat kept while the chats show: {away}")
    })?;
    qa.trace.drain();
    qa.chord(&[Modifier::Ctrl], '1')?;
    let state = qa.state_until("Ctrl+1: the newest opened again, the chip let go", |state| {
        state["list"] == "Activity" && state["filter"] == "all" && opened_at(&newest)(state)
    })?;
    ensure(state["toast"].is_null(), || format!("a toast: {}", state["toast"]))?;
    let traced = qa.trace.next_where("activity newest", |line| {
        line["view"] == "activity" && line["change"] == "newest"
    })?;
    say(&format!("newest on Ctrl+1: {traced}"));
    qa.hover(AWAY)?;
    qa.shot("newest-again-light")?;
    qa.chord(&[Modifier::Ctrl, Modifier::Alt], 'm')?;
    qa.state_until("Ctrl+Alt+M: @Mentions", |state| state["filter"] == "mentions")?;
    qa.ctl_json(&["open", "chat", ABC_CHAT])?;
    qa.chord(&[Modifier::Ctrl], '2')?;
    qa.state_until("Ctrl+2: the chats", |state| state["list"] == "Chats")?;
    qa.click(RAIL_ACTIVITY)?;
    qa.state_until("the rail: the newest opened again, @Mentions let go", |state| {
        state["list"] == "Activity" && state["filter"] == "all" && opened_at(&newest)(state)
    })?;
    // the keyboard is still on the pane, on the newest item
    qa.key(keys::DOWN)?;
    qa.key(keys::ENTER)?;
    qa.state_until("Down, Enter: the second item opened", opened_at(&second))?;
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
    qa.hover(AWAY)?;
    qa.shot("missed-call-read-light")?;
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

/// A meeting's update shows the meeting's time as its third line; a click opens the calendar at its day with its card
/// (the calendar doesn't know its event's id, so found at its time: the mock's Friday "Weekly sync").
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
    open_the_meeting(qa, "meeting-opened-light")?;
    // back to the pane for the flows after
    qa.chord(&[Modifier::Ctrl], '1')?;
    qa.state_until("the pane again", |state| state["pane_shown"] == true)?;
    Ok(())
}

/// Clicks the meeting row (searched for), sees the calendar at 10 Oct 2025 with the Weekly sync's card, and shoots it.
fn open_the_meeting(qa: &mut Qa<'_>, shot: &str) -> SandboxResult {
    let id = text(&item_with(&qa.search("weekly sync")?, "weekly sync")?, "id");
    qa.click_listed(ROW)?;
    qa.box_open = false;
    let state = qa.state_until("the calendar at the meeting's day with its card", |state| {
        state["main"] == "Calendar" && state["calendar_day"] == "2025-10-10" && state["calendar_card"] == "Weekly sync"
    })?;
    ensure(
        items(&state)
            .iter()
            .all(|item| text(item, "id") != id || item["read"] == true),
        || format!("the meeting read: {state}"),
    )?;
    thread::sleep(SETTLE);
    qa.shot(shot)?;
    qa.key(keys::ESC)?;
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
    let before = qa.state_until("no count on the rail", |state| state["unseen"] == 0)?;
    let pushed = qa.ask("push-reaction")?;
    let new = text(&pushed, "id");
    read_on_arrival_never_counted(qa, &new)?;
    qa.shot("read-on-arrival-light")?;
    let after = qa.state()?;
    ensure(after["unseen"] == before["unseen"], || {
        format!("rail count after it synced back: {}", after["unseen"])
    })?;
    Ok(())
}

/// Samples the window from the push of `new` (for the chat on screen) until the service has it read and a while
/// after, as its read syncs back: it is never drawn unread and the rail never counts it.
fn read_on_arrival_never_counted(qa: &mut Qa<'_>, new: &str) -> SandboxResult {
    let deadline = Instant::now() + WAIT;
    let mut synced: Option<Instant> = None;
    let mut samples = 0_u32;
    loop {
        let state = qa.state()?;
        samples += 1;
        ensure(state["unseen"] == 0, || {
            format!("rail count while the new item comes in, read on arrival (sample {samples}): {state}")
        })?;
        let shown = items(&state).into_iter().find(|item| text(item, "id") == new);
        if let Some(item) = &shown {
            ensure(item["read"] == true, || format!("the new item drawn unread: {item}"))?;
        }
        if synced.is_none() && shown.is_some() && qa.server(new)?["isread"] == true {
            synced = Some(Instant::now());
        }
        // the read's change comes back from the service after the write: watch through it
        if synced.is_some_and(|at| at.elapsed() > Duration::from_secs(4)) {
            say(&format!(
                "read on arrival: {samples} samples, the rail count 0 throughout"
            ));
            return Ok(());
        }
        ensure(Instant::now() < deadline, || {
            format!(
                "the new item never read on the service: {}",
                qa.server(new).unwrap_or_default()
            )
        })?;
        thread::sleep(Duration::from_millis(40));
    }
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

/// The pane put aside (the chats shown, the group chat open), `what` pushed on the mock as the service tells a, and
/// seen arriving unread; its id.
fn push_while_away(qa: &mut Qa<'_>, what: &str) -> SandboxResult<String> {
    qa.chord(&[Modifier::Ctrl], '2')?;
    qa.state_until("Ctrl+2: the chats", |state| state["list"] == "Chats")?;
    qa.ctl_json(&["open", "chat", ABC_CHAT])?;
    qa.state_until("the group chat open", |state| state["chat"] == ABC_CHAT)?;
    let pushed = qa.ask(what)?;
    let id = text(&pushed, "id");
    ensure(!id.is_empty(), || format!("{what}: {pushed}"))?;
    qa.state_until(&format!("{what} arrived unread, the newest"), |state| {
        items(state)
            .first()
            .is_some_and(|item| text(item, "id") == id && item["read"] == false)
    })?;
    Ok(id)
}

/// The newest item a missed call from b: Ctrl+1 opens the 1:1 chat with b (Teams opens its Calls page, which the app
/// doesn't have), the call read here and on the service, nothing said.
fn newest_missed_call(qa: &mut Qa<'_>) -> SandboxResult {
    let id = push_while_away(qa, "push-missed-call")?;
    qa.chord(&[Modifier::Ctrl], '1')?;
    let state = qa.state_until("the chat with the caller opened, the call read", |state| {
        state["list"] == "Activity"
            && state["chat"] == AB_CHAT
            && items(state)
                .first()
                .is_some_and(|item| text(item, "id") == id && item["read"] == true)
    })?;
    ensure(state["toast"].is_null(), || format!("a toast: {}", state["toast"]))?;
    qa.server_until(&id, "the missed call's isread true on the service", |held| {
        held["isread"] == true
    })?;
    qa.hover(AWAY)?;
    qa.shot("newest-missed-call-light")?;
    Ok(())
}

/// The newest item from a chat that is gone: opening the pane opens nothing and says nothing (the chat on screen
/// stays), the item is read, and the keyboard stays on the pane (Down, Enter opens the missed call's chat).
fn newest_gone_chat(qa: &mut Qa<'_>) -> SandboxResult {
    let id = push_while_away(qa, "push-gone-mention")?;
    let before = qa.state()?;
    qa.chord(&[Modifier::Ctrl], '1')?;
    qa.state_until("the gone chat's newest read", |state| {
        state["list"] == "Activity"
            && items(state)
                .first()
                .is_some_and(|item| text(item, "id") == id && item["read"] == true)
    })?;
    // a toast would have come with the read; watch a while for one
    for _ in 0..5 {
        let state = qa.state()?;
        ensure(
            state["toast"].is_null() && state["chat"] == ABC_CHAT && state["main"] == before["main"],
            || {
                format!(
                    "gone chat as newest: main {} chat {} toast {}",
                    state["main"], state["chat"], state["toast"]
                )
            },
        )?;
        thread::sleep(Duration::from_millis(300));
    }
    qa.hover(AWAY)?;
    qa.shot("newest-gone-chat-light")?;
    qa.key(keys::DOWN)?;
    qa.key(keys::ENTER)?;
    qa.state_until("Down, Enter: the missed call's chat with b", |state| {
        state["chat"] == AB_CHAT
    })?;
    Ok(())
}

/// The newest item a meeting (its details link names the mock's Friday 17 Oct "Weekly roundup", 21:00 here): opening
/// the pane only selects it (the chat on screen stays, the item unread, no calendar); Enter on it, then a click on it,
/// open the calendar at its day, scrolled to its evening start, with its card.
fn newest_meeting(qa: &mut Qa<'_>) -> SandboxResult {
    let id = push_while_away(qa, "push-meeting-known")?;
    qa.trace.drain();
    qa.chord(&[Modifier::Ctrl], '1')?;
    qa.trace.next_where("activity newest: a meeting", |line| {
        line["view"] == "activity" && line["change"] == "newest" && line.to_string().contains("meeting")
    })?;
    thread::sleep(SETTLE);
    let state = qa.state()?;
    let newest = first(&state)?;
    ensure(
        state["list"] == "Activity"
            && state["pane_shown"] == true
            && state["main"] == "Chat"
            && state["chat"] == ABC_CHAT
            && text(&newest, "id") == id
            && newest["read"] == false,
        || format!("the newest meeting only selected: {state}"),
    )?;
    qa.hover(AWAY)?;
    qa.shot("newest-meeting-light")?;
    // Enter opens what the keyboard is on: the meeting
    qa.key(keys::ENTER)?;
    let roundup = |state: &Value| {
        state["main"] == "Calendar"
            && state["calendar_day"] == "2025-10-17"
            && state["calendar_card"] == "Weekly roundup"
    };
    let state = qa.state_until("Enter: the calendar at the roundup's day with its card", roundup)?;
    ensure(
        items(&state)
            .iter()
            .all(|item| text(item, "id") != id || item["read"] == true),
        || format!("the meeting read: {state}"),
    )?;
    thread::sleep(SETTLE);
    qa.shot("meeting-by-id-light")?;
    qa.key(keys::ESC)?;
    qa.state_until("the card closed", |state| state["calendar_card"].is_null())?;
    qa.shot("meeting-by-id-card-closed-light")?;
    qa.chord(&[Modifier::Ctrl], '1')?;
    qa.state_until("the pane again", |state| {
        state["pane_shown"] == true && state["main"] != "Calendar"
    })?;
    qa.click_listed(ROW)?;
    qa.state_until("a click: the calendar at the roundup's day with its card", roundup)?;
    qa.key(keys::ESC)?;
    qa.chord(&[Modifier::Ctrl], '1')?;
    qa.state_until("the pane again", |state| state["pane_shown"] == true)?;
    Ok(())
}

/// A meeting the calendar has no event for (its id unknown, nothing at its time on Wednesday 22 Oct): a click shows
/// that day, and once the day's events are read no card opens.
fn unknown_meeting(qa: &mut Qa<'_>) -> SandboxResult {
    push_while_away(qa, "push-meeting-unknown")?;
    qa.chord(&[Modifier::Ctrl], '1')?;
    qa.state_until("the pane", |state| state["pane_shown"] == true)?;
    qa.click_listed(ROW)?;
    qa.state_until("the calendar at the meeting's day, done looking", |state| {
        state["main"] == "Calendar" && state["calendar_day"] == "2025-10-22" && state["calendar_revealing"] == false
    })?;
    thread::sleep(Duration::from_secs(2));
    let state = qa.state()?;
    ensure(
        state["calendar_card"].is_null() && state["calendar_day"] == "2025-10-22" && state["toast"].is_null(),
        || format!("no card, the day kept: {state}"),
    )?;
    qa.shot("meeting-unknown-light")?;
    qa.chord(&[Modifier::Ctrl], '1')?;
    qa.state_until("the pane again", |state| state["pane_shown"] == true)?;
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
    let before = qa.state()?;
    qa.click_listed(ROW)?;
    let state = qa.state_until("the gone chat's item read", |state| {
        items(state)
            .iter()
            .any(|item| text(item, "id") == id && item["read"] == true)
    })?;
    ensure(
        state["chat"] != GONE_CHAT
            && state["main"] == before["main"]
            && state["chat"] == before["chat"]
            && state["toast"] == teams_ui::activity_view::CHAT_GONE,
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

/// Teams Dark: the pane opened anew on its newest item, a row hovered, its menu and the header's, and the meeting
/// opened.
fn dark_theme(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ask("theme dark")?;
    // away and back: the pane opens its newest item again
    qa.chord(&[Modifier::Ctrl], '2')?;
    qa.state_until("Ctrl+2: the chats", |state| state["list"] == "Chats")?;
    let state = qa.plain_pane()?;
    let newest = first(&state)?;
    qa.state_until("the newest opened beside the pane", |state| {
        state["chat"] == newest["thread"] && state["revealed"] == newest["message"]
    })?;
    qa.hover(AWAY)?;
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
    let outcome = open_the_meeting(qa, "meeting-opened-dark");
    qa.chord(&[Modifier::Ctrl], '1')?;
    qa.ask("theme light")?;
    outcome
}

/// Teams Dark: the gone chat's toast, a group chat's "Chat with …" third line and the read missed call.
fn dark_fixed_states(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ask("theme dark")?;
    let outcome = dark_fixed_shots(qa);
    qa.ask("theme light")?;
    outcome
}

fn dark_fixed_shots(qa: &mut Qa<'_>) -> SandboxResult {
    qa.search("the chat that is gone")?;
    qa.click_listed(ROW)?;
    qa.state_until("the gone chat's toast", |state| {
        state["toast"] == teams_ui::activity_view::CHAT_GONE && state["chat"] != GONE_CHAT
    })?;
    qa.shot("gone-chat-dark")?;
    let group = text(&item_with(&qa.plain_pane()?, "Made-up reply about")?, "place");
    qa.search(&group)?;
    qa.hover(AWAY)?;
    qa.shot("group-place-dark")?;
    qa.plain_pane()?;
    qa.click_chip("missed-calls")?;
    let state = qa.state_until("the missed calls", |state| state["filter"] == "missed-calls")?;
    let call = first(&state)?;
    ensure(text(&call, "kind") == "missed-call" && call["read"] == true, || {
        format!("the missed call read: {call}")
    })?;
    qa.shot("missed-call-read-dark")?;
    qa.click_chip("missed-calls")?;
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
    runtime: tokio::runtime::Runtime,
    mock: Arc<ChatServiceMock>,
    _trouter: TrouterMock,
    /// a's data: a dir of its own, or the one a tile run keeps across runs.
    dir: PathBuf,
    _temp: Option<tempfile::TempDir>,
    /// a's own way to the mock outside the app, for the test's reads of what the mock holds.
    probe: Arc<HttpTransport<Tokens>>,
}

impl MockLab {
    /// `read_only`: the engine's writes are refused before sending, as `ngteams --read-only` refuses them; otherwise
    /// a's app preferences are seeded ([`turn_off::seeded_prefs`]). `tiles`: a tile run's feed and data dir
    /// ([`tile_runs`]), else the module docs' feed: with `pictures`, the banner pictures' run's data dir and mock
    /// ([`pictures`]), else in a dir of its own.
    fn start(
        read_only: bool,
        tiles: Option<&tile_runs::Run>,
        pictures: Option<PathBuf>,
    ) -> SandboxResult<(Self, EngineHandle)> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        let (dir, temp) = if let Some(run) = tiles {
            runtime.block_on(tile_runs::seed(&mock, run))?;
            (run.dir.clone(), None)
        } else if let Some(dir) = pictures {
            seed_feed(&mock)?;
            runtime.block_on(pictures::seed(&mock))?;
            (dir, None)
        } else {
            seed_feed(&mock)?;
            let temp = tempfile::tempdir()?;
            (temp.path().to_owned(), Some(temp))
        };
        let tokens = Arc::new(Tokens {
            token: mock.token(LAB_A).ok_or("the mock's token for a")?,
            renewals: broadcast::channel(4).0,
        });
        let origin = mock.origin();
        let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
        let probe = Arc::new(http()?);
        if !read_only {
            let answer = json!({"userGraphSettings": turn_off::seeded_prefs().to_string()}).to_string();
            let settings = teams_proto::UserGraphSettings::read_answer(answer.as_bytes())?;
            runtime.block_on(probe.deliver(teams_proto::SetUserGraphSettings(settings)))?;
        }
        let transport = Arc::new(if read_only { http()?.read_only() } else { http()? });
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
            store: Store::open(&support::store_file(&dir, Alias::A))?,
            // the people's names and pictures, as the app reads them
            client: Arc::new(Client::new(
                http()?
                    .with_presence(PRESENCE_BASE)?
                    .with_csa(CSA_BASE)?
                    .with_user_profile(USER_PROFILE_BASE)?
                    // the calendar a meeting item opens; the apps' definitions ("Turn off <app>")
                    .with_middle_tier(MIDDLE_TIER_BASE)?,
            )),
            outbox: Outbox::new(transport, author),
            push: Some(Push::new(trouter_client)),
            options: EngineOptions::default(),
            known_names: Vec::new(),
        })?;
        let lab = Self {
            runtime,
            mock: Arc::new(mock),
            _trouter: trouter,
            dir,
            _temp: temp,
            probe,
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

/// A meeting c updated (the Calendar app's item, attributed to c), on Friday 10 Oct 2025 15:30–16:00 UTC (the mock
/// calendar's Weekly sync), its details link naming an event id the calendar doesn't have.
fn meeting_update() -> Value {
    json!({
        "activityType": "msGraph", "activitySubtype": "privateMeetingUpdated", "activityId": 8,
        "sourceThreadId": "", "sourceMessageId": 1, "sourceUserId": "28:app:made-up-calendar",
        "sourceUserImDisplayName": "Lab C", "messagePreview": "Made-up weekly sync",
        "customTapAction": "https://teams.microsoft.com/l/meeting/details/?eventId=AAMkMadeUpSeriesMaster%3D",
        "activityContext": {
            "attributedToActorId": LAB_C,
            "location": "{\"Custom\":\"<DateTimeRange><StartDateTime>2025-10-10T15:30:00Z</StartDateTime><EndDateTime>2025-10-10T16:00:00Z</EndDateTime></DateTimeRange>\"}",
        },
    })
}

/// An app's note in the team's channel.
pub(super) fn app_note() -> Value {
    json!({
        "activityType": "msGraph", "activitySubtype": "customNotification", "activityId": 7,
        "sourceThreadId": TEAM, "sourceMessageId": 1, "sourceUserImDisplayName": "Workflows", "sourceAppDisplayName": "Workflows",
        "messagePreview": "Made-up reminder to follow up", "sourceThreadType": "space",
        "activityContext": {"teamsAppId": "00000000-0000-4000-8000-00000000a002", "templateParameters": "{\"customMessage\":\"Follow up on a message\"}"},
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

fn child_app(mode: &str) -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let read_only = mode == "read-only";
    let tiles = tile_runs::Run::of_mode(mode)?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(passed_trace_fd()?)?);
    let dir = PathBuf::from(std::env::var_os(ui_sandbox::SANDBOX_ENV).ok_or("no sandbox dir")?);
    let (lab, engine) = MockLab::start(read_only, tiles.as_ref(), pictures::data_dir(mode)?)?;
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
    let settings = teams_ui::SettingsFile::new(lab.dir.join("ui").join("a.toml"));
    let runner = support::runner(&lab.dir)?;
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let probe = Probe {
        mock: Arc::clone(&lab.mock),
        runtime: lab.runtime.handle().clone(),
        transport: Arc::clone(&lab.probe),
    };
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
                read_only,
            },
        );
        match launched {
            Ok(launched) => {
                let _ = launched.window.update(cx, |_, window, _| {
                    after_frames(window, READY_FRAMES, |_, _| say("ready"));
                });
                answer(launched.desktop, probe, dir.join(ASK), cx);
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

/// What the child reads and changes on the mock for the parent's questions: the mock, its runtime, and a's own
/// transport to it.
struct Probe {
    mock: Arc<ChatServiceMock>,
    runtime: tokio::runtime::Handle,
    transport: Arc<HttpTransport<Tokens>>,
}

impl Probe {
    /// Runs `work` on the mock's runtime, from a thread of its own (GPUI's isn't one to block a runtime on).
    fn block<T: Send>(&self, work: impl Future<Output = T> + Send) -> Option<T> {
        thread::scope(|scope| scope.spawn(|| self.runtime.block_on(work)).join().ok())
    }

    /// a's app preferences as the mock holds them: its read's `status`, the `settings` it holds (parsed; `null` when
    /// it holds none), and every `PUT` of them it got (refused ones too), each as the setting it carried.
    fn prefs(&self) -> Value {
        let read = self.block(self.transport.read(&teams_proto::UserGraphSettingsRead));
        let (status, body) = match read {
            Some(Ok(answer)) => (answer.status(), answer.body().to_vec()),
            Some(Err(HttpError::Status(answer))) => (answer.status(), answer.body().to_vec()),
            Some(Err(error)) => return json!({"error": error.to_string()}),
            None => return json!({"error": "no answer"}),
        };
        let setting = |answer: &Value| {
            answer["userGraphSettings"]
                .as_str()
                .and_then(|setting| serde_json::from_str::<Value>(setting).ok())
        };
        let settings = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|answer| setting(&answer));
        let puts: Vec<Value> = self
            .block(self.mock.received_requests())
            .unwrap_or_default()
            .iter()
            .filter(|request| request.method.as_str() == "PUT" && request.url.path().ends_with("/users/ME/properties"))
            .map(|request| {
                serde_json::from_slice::<Value>(&request.body)
                    .ok()
                    .and_then(|body| setting(&body))
                    .unwrap_or(Value::Null)
            })
            .collect();
        json!({"status": status, "settings": settings, "puts": puts})
    }

    /// `rest` is `<METHOD> <status> <times>`: the mock refuses a's next `times` requests of its app preferences of
    /// that method with `status`, before its world sees them.
    fn refuse(&self, rest: &str) -> Value {
        let mut words = rest.split(' ');
        let (Some(method), Some(Ok(status)), Some(Ok(times))) = (
            words.next(),
            words.next().map(str::parse::<u16>),
            words.next().map(str::parse::<usize>),
        ) else {
            return json!({"error": format!("refuse what: {rest}")});
        };
        for _ in 0..times {
            self.block(self.mock.refuse_next(method, "users/ME/properties", status));
        }
        json!({"refused": rest})
    }
}

/// Answers each question the parent writes to `ask`, one `answer <json>` line each, from the window the desktop shows
/// now (a window closed into the tray and opened again is another one).
fn answer(desktop: Entity<Desktop>, probe: Probe, ask: PathBuf, cx: &mut App) {
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
            if let Some(reply) = mock_reply(&question, &probe) {
                say(&format!("answer {reply}"));
                continue;
            }
            let Some(window) = cx.update(|cx| desktop.read(cx).window()) else {
                say("answer {\"window\":false}");
                continue;
            };
            let Some(shell) = shell_of(window, cx) else {
                say("answer {\"error\":\"no shell\"}");
                continue;
            };
            let reply = window
                .update(cx, |_, _, cx| reply_to(&question, &shell, cx))
                .unwrap_or_else(|_| json!({"error": "no window"}));
            say(&format!("answer {reply}"));
        }
    })
    .detach();
}

/// The answer to a question about the mock alone (asked with the window closed too), or `None` for one about the window.
fn mock_reply(question: &str, probe: &Probe) -> Option<Value> {
    let (verb, rest) = question.split_once(' ').unwrap_or((question, ""));
    let mock = &probe.mock;
    Some(match verb {
        "server" => server_copy(mock, rest),
        "push-reaction" => push_reaction(mock),
        "push-mention" => push_mention(mock),
        "push-missed-call" => push(mock, &activity("call", "missedCall", (LAB_B, "Lab B"), ("", 2), "")),
        "push-gone-mention" => push(
            mock,
            &activity(
                "mentionInChat",
                "person",
                (LAB_B, "Lab B"),
                (GONE_CHAT, 6),
                "Made-up newest words where the chat went",
            ),
        ),
        "push-meeting-known" => push(mock, &roundup_update()),
        "push-meeting-unknown" => push(mock, &unknown_meeting_update()),
        "push-app" => push_app(mock, rest),
        "push-other-app" => tiles::push_other_app(mock),
        "tile-reads" => tile_runs::reads(probe),
        "tile-push" => tile_runs::push_update(mock),
        "push-chat" => push_chat(mock),
        "pic" => pictures::reply(rest, probe),
        "prefs" => probe.prefs(),
        "refuse" => probe.refuse(rest),
        _ => return None,
    })
}

/// The answer to a question about the window.
fn reply_to(question: &str, shell: &Entity<Shell>, cx: &mut App) -> Value {
    let (verb, rest) = question.split_once(' ').unwrap_or((question, ""));
    match verb {
        "state" => state_of(shell, cx),
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

/// What the window holds: the list's place, the rail's count, the pane's chip, search, selected item and menus, its
/// items as listed (each with its read state as the pane draws it and a meeting's time line), the main view, the chat
/// open and the message it was opened at, the settings' page and the toast.
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
                "message": item.message, "meeting_line": meeting_line, "tile": view.shows_app_tile(item),
                "app": item.teams_app_id,
            })
        })
        .collect();
    let (header_menu, item_menu) = view.menus();
    let chat = shell.chat().map(|chat| chat.read(cx));
    let calendar = shell.calendar_view().read(cx);
    json!({
        "list": format!("{:?}", shell.list_place()),
        "pane_shown": view.shown(),
        "selected": view.selected(),
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
        "calendar_day": calendar.anchor().to_string(),
        "calendar_card": calendar.card().map(|shown| shown.event.subject.clone()),
        "calendar_revealing": calendar.revealing().is_some(),
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

/// `item` added to a's feed, unread, as the service tells a.
fn push(mock: &ChatServiceMock, item: &Value) -> Value {
    mock.add_activity(LAB_A, item, false)
        .map_or_else(|| json!({"error": "no feed"}), |id| json!({"id": id}))
}

/// A meeting c updated whose details link names the mock calendar's Friday 17 Oct 2025 "Weekly roundup" (18:00–19:00
/// UTC) by its event id.
fn roundup_update() -> Value {
    meeting_activity(
        9,
        "Made-up weekly roundup",
        "AAMkMadeUpEvent037813",
        ("2025-10-17T18:00:00Z", "2025-10-17T19:00:00Z"),
    )
}

/// A meeting c updated that the mock calendar has no event for: an unknown id, nothing at its time (Wednesday 22 Oct
/// 2025 05:00–05:30 UTC).
fn unknown_meeting_update() -> Value {
    meeting_activity(
        10,
        "Made-up early check-in",
        "AAMkMadeUpNoSuchEvent%3D",
        ("2025-10-22T05:00:00Z", "2025-10-22T05:30:00Z"),
    )
}

/// The Calendar app's item for a meeting c updated: `event` its details link's id, `(start, end)` its time.
fn meeting_activity(number: u64, preview: &str, event: &str, (start, end): (&str, &str)) -> Value {
    let range = format!(
        "<DateTimeRange><StartDateTime>{start}</StartDateTime><EndDateTime>{end}</EndDateTime></DateTimeRange>"
    );
    json!({
        "activityType": "msGraph", "activitySubtype": "privateMeetingUpdated", "activityId": number,
        "sourceThreadId": "", "sourceMessageId": 1, "sourceUserId": "28:app:made-up-calendar",
        "sourceUserImDisplayName": "Lab C", "messagePreview": preview,
        "customTapAction": format!("https://teams.microsoft.com/l/meeting/details/?eventId={event}"),
        "activityContext": {
            "attributedToActorId": LAB_C,
            "location": json!({"Custom": range}).to_string(),
        },
    })
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

/// A later note of the app ([`app_note`]'s), `which` in its words, as the feed then tells a.
fn push_app(mock: &ChatServiceMock, which: &str) -> Value {
    let mut item = app_note();
    item["activityId"] = json!(match which {
        "later" => 10,
        "tray" => 11,
        "tray-dark" => 12,
        "undone" => 13,
        _ => 9,
    });
    item["messagePreview"] = json!(format!("Made-up {which} reminder"));
    mock.add_activity(LAB_A, &item, false)
        .map_or_else(|| json!({"error": "no feed"}), |id| json!({"id": id}))
}

/// b posts to the a–b chat, as a's banner for it then comes.
fn push_chat(mock: &ChatServiceMock) -> Value {
    mock.post_as(LAB_B, AB_CHAT, "<p>A made-up note for a banner</p>")
        .map_or_else(|| json!({"error": "b's post refused"}), |id| json!({"id": id}))
}
