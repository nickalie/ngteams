//! The Store as a user meets it, in the UI sandbox (`harness = false`: GPUI runs on the process's
//! main thread). QA of task 226; the own server's side is in `apps_window_own.rs`.
//!
//! The test runs itself again inside the sandbox as the app of lab account a, its engine on the mock (`teams-mock`,
//! whose apps are stateful: an add or a remove changes what the next read gives). The parent clicks and types with
//! the sandbox's virtual pointer and keyboard where a place is fixed (the rail's Store, the search box, a tile, Add,
//! the gear), and asks the child for the rest (a dialog's buttons drawn where the content puts them). The child answers
//! "what does the window hold now" and "what does the mock hold now" through a file in the sandbox dir. Every check
//! is named and counted. The shots go to `target/ui-shots/apps-qa-*.png` and, when `APPS_QA_EVIDENCE` names a
//! directory, there too.
//!
//! The flows: the store's sections; search, also with no match; an app's details and Add for oneself, then "Manage
//! your apps"; Add to a chat and the chat's apps; Add to a team and Manage team's Apps; Remove in each place; a refused
//! add and a refused remove in each place; a store of many apps, a long name, no picture, and a picture on a host that
//! is not Microsoft's CDN. Each in Teams Light, the main ones in Teams Dark too.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`. Every app, chat,
//! team and person is made up.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use std::{fs, future, io, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
#[path = "apps_window_own.rs"]
mod own;
#[path = "../own_server_binary.rs"]
#[expect(
    dead_code,
    reason = "the own server's tests share this support; this one starts it plainly"
)]
mod own_server_binary;
#[path = "../window.rs"]
mod support;
use serde_json::{Value, json};
use support::Alias;
use support::display_name;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_mock::{
    AB_CHAT, AppPlace, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, MIDDLE_TIER_BASE, PRESENCE_BASE, TrouterMock,
    USER_PROFILE_BASE,
};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri, StoreApp};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::apps_page::chat_apps::ChatAppsDialog;
use teams_ui::apps_page::details::AppDetailsDialog;
use teams_ui::apps_page::place::AddToPlaceDialog;
use teams_ui::apps_page::{AppsPage, AppsView, StoreSection};
use teams_ui::manage_team::ManageTab;
use teams_ui::shell::Shell;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::{Modifier, VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

/// Which mode the child runs in.
const CHILD_ENV: &str = "NGTEAMS_APPS_QA_TEST";
/// Where the shots are copied too, when set.
const EVIDENCE_ENV: &str = "APPS_QA_EVIDENCE";
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(120);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(30);
/// Frames the window presents before the child says `ready`.
const READY_FRAMES: usize = 3;
/// The file in the sandbox dir the parent writes a question to.
const ASK: &str = "apps-ask";
/// How long a refused write gets to be answered and told.
const ANSWERED: Duration = Duration::from_secs(3);
/// How long the views get to settle after an input, and before a shot.
const SETTLE: Duration = Duration::from_millis(900);

/// The mock's team (`teams-mock`'s `CHANNEL`).
const TEAM: &str = "19:channel0001@thread.tacv2";
/// The mock's organization app: a personal bot and a page.
const HELPER: &str = "00000000-0000-4000-8000-00000000a004";
/// The mock's chats-and-teams app.
const POLLS: &str = "00000000-0000-4000-8000-00000000a005";
/// The mock's tab app, on the team before the run.
const PLANNER: &str = "00000000-0000-4000-8000-00000000a001";
/// The mock's app that the account has as `PreConsented`, as the lab account has Approvals.
const PRE_CONSENTED: &str = teams_mock::PRE_CONSENTED;
/// The odd store's app with a long name and no picture.
const LONG: &str = "00000000-0000-4000-8000-00000000c001";
/// The odd store's app whose picture is on a host that is not Microsoft's CDN.
const OFF_CDN: &str = "00000000-0000-4000-8000-00000000c002";
/// The file name of [`OFF_CDN`]'s picture: no request may name it.
const OFF_CDN_FILE: &str = "made-up-off-cdn_largeImage.png";
/// The odd store's popular apps.
const MANY: usize = 30;

// Places in the 1280 × 800 window (the tour's `tour-apps-*.png`).
const RAIL_STORE: (u32, u32) = (37, 245);
const SEARCH_BOX: (u32, u32) = (674, 247);
const ORG_TILE: (u32, u32) = (230, 410);
const POLLS_TILE: (u32, u32) = (480, 550);
const DETAILS_ADD: (u32, u32) = (350, 143);
const DETAILS_CLOSE: (u32, u32) = (1013, 76);
const GEAR: (u32, u32) = (1243, 71);
const STORE_MIDDLE: (u32, u32) = (674, 560);
const AWAY: (u32, u32) = (1200, 760);

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child_app(),
        Ok(own::MODE) => own::child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("apps window test: {error}");
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
    let Some(sandbox) = sandbox_or_skip("ngteams apps window", &options)? else {
        return Ok(());
    };
    let outcome = run_checks(&sandbox).map_err(|error| format!("{error}{}", sandbox.logs()));
    let failed = outcome?;
    if failed > 0 {
        return Err(format!("{failed} apps check(s) failed").into());
    }
    say("ngteams apps window: ok");
    Ok(())
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

/// The run: the app, the user's devices, and the tally.
pub(super) struct Qa<'a> {
    sandbox: &'a Sandbox,
    running: Running,
    input: VirtualInput,
    pub(super) passed: Vec<&'static str>,
    pub(super) failed: Vec<(&'static str, String)>,
}

/// One user flow, by name.
type Check = (&'static str, fn(&mut Qa<'_>) -> SandboxResult);

/// The flows, in the order a user meets them (each leaves the window as the next expects it).
const CHECKS: [Check; 13] = [
    ("rail_store_opens_the_org_s_and_popular_apps", rail_opens_the_store),
    ("search_finds_by_name_and_maker_and_says_none", search),
    ("details_show_what_the_app_does_and_add_it_for_me", details_and_add),
    ("manage_your_apps_lists_the_added_app_at_once", manage_lists_it),
    ("add_to_a_chat_and_the_chat_s_apps_list_it", add_to_a_chat),
    ("remove_from_a_chat_asks_then_removes", remove_from_the_chat),
    ("add_to_a_team_adds_it_on_the_service", add_to_a_team),
    ("manage_team_apps_remove_asks_then_removes", remove_from_the_team),
    ("remove_in_manage_your_apps_asks_then_removes", remove_mine),
    ("a_refused_add_says_so_and_adds_nothing", refused_add),
    ("a_refused_add_to_a_chat_says_so", refused_chat_add),
    ("a_refused_remove_says_so_and_keeps_the_app", refused_removes),
    ("many_apps_a_long_name_no_picture_and_an_off_cdn_picture", odd_store),
];

fn run_checks(sandbox: &Sandbox) -> SandboxResult<usize> {
    let mut qa = Qa::start(sandbox, "app", &[])?;
    for (name, check) in CHECKS {
        qa.check(name, check);
    }
    let (mut passed, mut failed) = (qa.passed.clone(), qa.failed.clone());
    qa.quit()?;
    let (mut own_passed, mut own_failed) = own::run(sandbox)?;
    passed.append(&mut own_passed);
    failed.append(&mut own_failed);
    for (name, error) in &failed {
        say(&format!("FAILED {name}: {error}"));
    }
    say(&format!(
        "apps checks: {} passed, {} failed",
        passed.len(),
        failed.len()
    ));
    Ok(failed.len())
}

impl<'a> Qa<'a> {
    /// The app started in the sandbox in `mode`, with `env` for the child too, its window drawn.
    pub(super) fn start(sandbox: &'a Sandbox, mode: &str, env: &[(&str, &str)]) -> SandboxResult<Self> {
        let mut vars = vec![(CHILD_ENV, mode)];
        vars.extend_from_slice(env);
        let child = sandbox.spawn_with_fds(&std::env::current_exe()?, &[], &vars, &[])?;
        let mut running = Running::new(child)?;
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
        // the frames drawn are not yet a window on screen: niri shows it once its first buffer comes, and a click
        // before that goes to no window
        if focused_window(sandbox).is_none() {
            wait_until(WAIT, || focused_window(sandbox))
                .ok_or_else(|| running.fail(&"niri never showed the window"))?;
            say("the window was not on screen at ready: the clicks waited for niri to show it");
        }
        Ok(Qa {
            sandbox,
            running,
            input: sandbox.input()?,
            passed: Vec::new(),
            failed: Vec::new(),
        })
    }

    /// Runs one flow, counted; a failed one is screenshot as it was left.
    pub(super) fn check(&mut self, name: &'static str, flow: fn(&mut Qa<'_>) -> SandboxResult) {
        match flow(self) {
            Ok(()) => {
                say(&format!("ok {name}"));
                self.passed.push(name);
            }
            Err(error) => {
                say(&format!("FAILED {name}: {error}"));
                let _ = self.shot(&format!("failed-{name}"));
                self.failed.push((name, error.to_string()));
                let _ = self.ask("do close");
                let _ = self.ask("theme light");
            }
        }
    }

    /// Asks the child `question`; its answer.
    pub(super) fn ask(&self, question: &str) -> SandboxResult<Value> {
        let staged = self.sandbox.path("apps-ask.staged");
        fs::write(&staged, question)?;
        fs::rename(&staged, self.sandbox.path(ASK))?;
        let line = self
            .running
            .line_where(WAIT, |line| line.starts_with("answer "))
            .map_err(|error| format!("no answer to {question}: {error:?}"))?;
        let answer: Value = serde_json::from_str(line.trim_start_matches("answer "))?;
        if let Some(error) = answer.get("error") {
            return Err(format!("{question}: {error}").into());
        }
        Ok(answer)
    }

    /// Asks the child to do `what` in the window, then lets the views settle.
    pub(super) fn act(&self, what: &str) -> SandboxResult<Value> {
        let answer = self.ask(&format!("do {what}"))?;
        thread::sleep(SETTLE);
        Ok(answer)
    }

    /// What the window holds now (see the child's [`state_of`]).
    pub(super) fn state(&self) -> SandboxResult<Value> {
        self.ask("state")
    }

    /// Waits up to [`WAIT`] for the window to hold what `wanted` takes; the last state seen in the error.
    pub(super) fn state_until(&self, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
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

    /// The ids of the apps the mock holds at `place` (`personal`, `chat <id>`, `team <id>`).
    fn added(&self, place: &str) -> SandboxResult<Vec<String>> {
        let answer = self.ask(&format!("added {place}"))?;
        Ok(strings(&answer["added"]))
    }

    /// Waits for the mock to hold what `wanted` takes at `place`.
    fn added_until(&self, place: &str, what: &str, wanted: impl Fn(&[String]) -> bool) -> SandboxResult<Vec<String>> {
        let mut last = Vec::new();
        wait_until(WAIT, || match self.added(place) {
            Ok(held) if wanted(&held) => Some(held),
            Ok(held) => {
                last = held;
                thread::sleep(Duration::from_millis(200));
                None
            }
            Err(_) => None,
        })
        .ok_or_else(|| format!("{what}: the mock holds {last:?} at {place}").into())
    }

    /// Screenshots the window as `apps-qa-<name>.png`, kept with the tour's and in the evidence dir.
    pub(super) fn shot(&self, name: &str) -> SandboxResult<PathBuf> {
        thread::sleep(SETTLE);
        let file = format!("apps-qa-{name}.png");
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

    /// Shots `name` in Teams Light, then `name-dark` in Teams Dark; the theme light again after.
    pub(super) fn shots(&self, name: &str) -> SandboxResult {
        self.shot(name)?;
        self.ask("theme dark")?;
        let dark = self.shot(&format!("{name}-dark"));
        self.ask("theme light")?;
        dark.map(|_| ())
    }

    pub(super) fn click(&mut self, (x, y): (u32, u32)) -> SandboxResult {
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn hover(&mut self, (x, y): (u32, u32)) -> SandboxResult {
        self.input.move_to(x, y)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn type_text(&mut self, text: &str) -> SandboxResult {
        self.input.type_text(text)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// Ctrl+A, then Backspace: the focused box emptied.
    fn clear_box(&mut self) -> SandboxResult {
        let (key, _) = self.input.keymap().key_for('a').ok_or("a isn't on the US keymap")?;
        self.input.key_with(&[Modifier::Ctrl], key)?;
        self.input.key(keys::BACKSPACE)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The store shown with no search and no dialog, as the rail's Store opens it.
    fn plain_store(&mut self) -> SandboxResult<Value> {
        self.act("close")?;
        // the store first: a search needs it open, so a flow before that left it closed fails only itself
        self.act("store")?;
        self.act("search ")?;
        self.state_until("the store, read", |state| {
            state["main"] == "Apps" && state["store"]["view"] == "Store" && state["store"]["loaded"] == true
        })
    }

    pub(super) fn quit(self) -> SandboxResult {
        let _ = self.ask("quit");
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("the app exited {status}: {stderr}").into());
        }
        Ok(())
    }
}

/// `value` as a list of strings.
pub(super) fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|list| list.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

/// The id of the window niri shows focused: the app's, once niri shows it.
fn focused_window(sandbox: &Sandbox) -> Option<u64> {
    let windows: Value = serde_json::from_str(&sandbox.niri_msg(&["-j", "windows"]).ok()?).ok()?;
    windows.as_array()?.iter().find(|window| window["is_focused"] == true)?["id"].as_u64()
}

pub(super) fn ensure(holds: bool, what: impl FnOnce() -> String) -> SandboxResult {
    if holds { Ok(()) } else { Err(what().into()) }
}

fn holds(list: &Value, id: &str) -> bool {
    strings(list).iter().any(|listed| listed == id)
}

// --- the flows ---

fn rail_opens_the_store(qa: &mut Qa<'_>) -> SandboxResult {
    qa.click(RAIL_STORE)?;
    let state = qa
        .state_until("the store from the rail, read", |state| {
            state["main"] == "Apps" && state["store"]["loaded"] == true
        })
        .map_err(|error| {
            let windows = qa
                .sandbox
                .niri_msg(&["-j", "windows"])
                .unwrap_or_else(|error| error.to_string());
            format!("{error}; niri's windows: {windows}")
        })?;
    let store = &state["store"];
    ensure(strings(&store["org"]) == [HELPER], || {
        format!("the org's apps: {store}")
    })?;
    ensure(strings(&store["popular"]) == [PLANNER, POLLS], || {
        format!("the popular apps: {store}")
    })?;
    // Made-up Helper's definition names the CDN's tile; Planner's and Polls' name none
    qa.state_until("Made-up Helper drawn with its picture", |state| {
        state["store"]["pictures"][HELPER] == true
    })?;
    ensure(store["pictures"][POLLS] == false, || {
        format!("Polls has a picture: {store}")
    })?;
    qa.hover(AWAY)?;
    qa.shots("store")
}

fn search(qa: &mut Qa<'_>) -> SandboxResult {
    qa.plain_store()?;
    qa.click(SEARCH_BOX)?;
    qa.type_text("polls")?;
    qa.state_until("the search for polls finding Made-up Polls alone", |state| {
        strings(&state["store"]["found"]) == [POLLS]
    })?;
    qa.shots("search")?;
    // by maker: every app the store lists, the Microsoft section's too
    qa.clear_box()?;
    qa.type_text("made-up maker")?;
    qa.state_until("the search for the maker finding the store's five apps", |state| {
        strings(&state["store"]["found"]).len() == 5
    })?;
    // a Microsoft-section app by name
    qa.clear_box()?;
    qa.type_text("updates")?;
    qa.state_until(
        "the search for updates finding the Microsoft section's Made-up Updates",
        |state| strings(&state["store"]["found"]) == ["00000000-0000-4000-8000-00000000a002"],
    )?;
    // no match
    qa.clear_box()?;
    qa.type_text("zzqx")?;
    qa.state_until("the search for zzqx finding nothing", |state| {
        strings(&state["store"]["found"]).is_empty()
    })?;
    qa.shot("search-none")?;
    qa.clear_box()?;
    Ok(())
}

fn details_and_add(qa: &mut Qa<'_>) -> SandboxResult {
    qa.plain_store()?;
    qa.click(ORG_TILE)?;
    let state = qa.state_until("Made-up Helper's details", |state| state["dialog"]["kind"] == "details")?;
    ensure(state["dialog"]["added"] == false, || {
        format!("added before Add: {}", state["dialog"])
    })?;
    qa.hover(AWAY)?;
    qa.shots("details")?;
    qa.click(DETAILS_ADD)?;
    qa.added_until("personal", "Add for me", |held| held.iter().any(|id| id == HELPER))?;
    qa.state_until("the details saying added", |state| state["dialog"]["added"] == true)?;
    qa.shots("details-added")?;
    qa.click(DETAILS_CLOSE)?;
    let state = qa.state_until("the details closed and the store read again", |state| {
        state["dialog"]["kind"].is_null() && state["store"]["added"][HELPER] == true
    })?;
    ensure(state["toast"] == "App added", || {
        format!("the toast: {}", state["toast"])
    })?;
    qa.shot("store-added").map(|_| ())
}

fn manage_lists_it(qa: &mut Qa<'_>) -> SandboxResult {
    qa.click(GEAR)?;
    let state = qa.state_until("Manage your apps listing Made-up Helper", |state| {
        state["store"]["view"] == "Manage" && holds(&state["store"]["mine"], HELPER)
    })?;
    ensure(!holds(&state["store"]["mine"], POLLS), || {
        format!("Polls among mine: {}", state["store"])
    })?;
    ensure(holds(&state["store"]["mine"], PRE_CONSENTED), || {
        format!("the pre-consented app not among mine: {}", state["store"])
    })?;
    qa.act(&format!("row {HELPER}"))?;
    qa.shots("manage")
}

fn add_to_a_chat(qa: &mut Qa<'_>) -> SandboxResult {
    qa.plain_store()?;
    qa.click(POLLS_TILE)?;
    qa.state_until("Made-up Polls' details", |state| state["dialog"]["kind"] == "details")?;
    qa.hover(AWAY)?;
    qa.shots("details-polls")?;
    qa.act("add-to chat")?;
    let state = qa.state_until("the chat picker", |state| state["dialog"]["kind"] == "place")?;
    ensure(holds(&state["dialog"]["places"], AB_CHAT), || {
        format!("the a-b chat not offered: {}", state["dialog"])
    })?;
    qa.act(&format!("choose {AB_CHAT}"))?;
    qa.shots("place-chat")?;
    qa.act("go")?;
    qa.added_until(&format!("chat {AB_CHAT}"), "Add to the a-b chat", |held| {
        held.iter().any(|id| id == POLLS)
    })?;
    let state = qa.state_until("the picker closed", |state| state["dialog"]["kind"].is_null())?;
    ensure(state["toast"] == "App added to the chat", || {
        format!("the toast: {}", state["toast"])
    })?;
    qa.act(&format!("chat-apps {AB_CHAT}"))?;
    qa.state_until("the chat's apps listing Made-up Polls", |state| {
        state["dialog"]["kind"] == "chat-apps" && holds(&state["dialog"]["shown"], POLLS)
    })?;
    qa.shots("chat-apps")
}

fn remove_from_the_chat(qa: &mut Qa<'_>) -> SandboxResult {
    qa.act(&format!("chat-ask-remove {POLLS}"))?;
    qa.state_until("the chat's Remove asking", |state| state["dialog"]["removing"] == POLLS)?;
    ensure(
        qa.added(&format!("chat {AB_CHAT}"))?.contains(&POLLS.to_owned()),
        || "removed before the answer".to_owned(),
    )?;
    qa.shots("chat-apps-remove-ask")?;
    qa.act("chat-remove")?;
    qa.added_until(&format!("chat {AB_CHAT}"), "the chat's Remove", |held| {
        !held.iter().any(|id| id == POLLS)
    })?;
    qa.state_until("Made-up Polls gone from the chat's apps", |state| {
        state["dialog"]["kind"] == "chat-apps"
            && !holds(&state["dialog"]["shown"], POLLS)
            && state["dialog"]["removing"].is_null()
    })?;
    qa.shot("chat-apps-removed")?;
    qa.act("close")?;
    Ok(())
}

fn add_to_a_team(qa: &mut Qa<'_>) -> SandboxResult {
    qa.plain_store()?;
    qa.act(&format!("details {POLLS}"))?;
    qa.act("add-to team")?;
    let state = qa.state_until("the team picker", |state| state["dialog"]["kind"] == "place")?;
    ensure(holds(&state["dialog"]["places"], TEAM), || {
        format!("the team not offered: {}", state["dialog"])
    })?;
    qa.act(&format!("choose {TEAM}"))?;
    qa.shots("place-team")?;
    qa.act("go")?;
    qa.added_until(&format!("team {TEAM}"), "Add to the team", |held| {
        held.iter().any(|id| id == POLLS)
    })?;
    let state = qa.state_until("the picker closed", |state| state["dialog"]["kind"].is_null())?;
    ensure(state["toast"] == "App added to the team", || {
        format!("the toast: {}", state["toast"])
    })?;
    Ok(())
}

fn remove_from_the_team(qa: &mut Qa<'_>) -> SandboxResult {
    qa.act("close")?;
    qa.act("manage-team")?;
    qa.state_until("Manage team's Apps listing the added Made-up Polls", |state| {
        holds(&state["manage_team"]["apps"], PLANNER) && holds(&state["manage_team"]["apps"], POLLS)
    })?;
    qa.shot("manage-team-apps")?;
    qa.act(&format!("team-ask-remove {PLANNER}"))?;
    qa.shot("manage-team-remove-ask")?;
    qa.act("team-remove")?;
    qa.state_until("Made-up Planner gone from Manage team's Apps", |state| {
        !holds(&state["manage_team"]["apps"], PLANNER) && holds(&state["manage_team"]["apps"], POLLS)
    })?;
    let path = format!("teams/{TEAM}/apps/entitlements/{PLANNER}");
    let mut sent = Value::Null;
    wait_until(WAIT, || {
        sent = qa.ask(&format!("requests DELETE {path}")).ok()?;
        (sent["count"] == 1).then_some(())
    })
    .ok_or_else(|| format!("the team's DELETE of {path}: {sent}"))?;
    qa.shot("manage-team-removed").map(|_| ())
}

fn remove_mine(qa: &mut Qa<'_>) -> SandboxResult {
    qa.act("close")?;
    qa.plain_store()?;
    qa.click(GEAR)?;
    qa.state_until("Manage your apps listing Made-up Helper", |state| {
        state["store"]["view"] == "Manage" && holds(&state["store"]["mine"], HELPER)
    })?;
    qa.act(&format!("row {HELPER}"))?;
    qa.act(&format!("ask-remove {HELPER}"))?;
    qa.state_until("Remove asking", |state| state["store"]["removing"] == HELPER)?;
    qa.shots("manage-remove-ask")?;
    qa.act("remove")?;
    qa.added_until("personal", "Remove in Manage your apps", |held| {
        !held.iter().any(|id| id == HELPER)
    })?;
    qa.state_until("Made-up Helper gone from Manage your apps", |state| {
        !holds(&state["store"]["mine"], HELPER) && state["store"]["removing"].is_null()
    })?;
    qa.shot("manage-removed")?;
    // the pre-consented app, as Teams removes Approvals: the same DELETE, no body
    qa.act(&format!("row {PRE_CONSENTED}"))?;
    qa.act(&format!("ask-remove {PRE_CONSENTED}"))?;
    qa.state_until("Remove of the pre-consented app asking", |state| {
        state["store"]["removing"] == PRE_CONSENTED
    })?;
    qa.act("remove")?;
    qa.state_until("the pre-consented app gone from Manage your apps", |state| {
        !holds(&state["store"]["mine"], PRE_CONSENTED) && state["store"]["removing"].is_null()
    })?;
    let path = format!("users/apps/entitlements/{PRE_CONSENTED}");
    let sent = qa.ask(&format!("requests DELETE {path}"))?;
    ensure(sent["count"] == 1, || format!("the DELETE of {path}: {sent}"))?;
    // the store's tile has no check any more
    qa.plain_store()?;
    let state = qa.state_until("the store read again", |state| state["store"]["added"][HELPER] == false)?;
    ensure(state["store"]["loaded"] == true, || format!("{state}"))?;
    Ok(())
}

fn refused_add(qa: &mut Qa<'_>) -> SandboxResult {
    qa.plain_store()?;
    qa.ask("refuse POST users/apps/entitlements 500")?;
    qa.click(ORG_TILE)?;
    qa.state_until("Made-up Helper's details", |state| state["dialog"]["kind"] == "details")?;
    qa.click(DETAILS_ADD)?;
    // the refusal comes at once; the outbox tries a team write once
    thread::sleep(ANSWERED);
    let state = qa.state()?;
    ensure(state["dialog"]["kind"] == "details", || {
        format!("the details closed after a refusal: {}", state["dialog"])
    })?;
    ensure(state["dialog"]["added"] == false, || {
        format!("added after a refusal: {}", state["dialog"])
    })?;
    ensure(qa.added("personal")?.is_empty(), || "the mock holds an app".to_owned())?;
    qa.hover(AWAY)?;
    qa.shots("details-add-refused")?;
    // Add again: it goes through
    qa.click(DETAILS_ADD)?;
    qa.added_until("personal", "the second Add", |held| held.iter().any(|id| id == HELPER))?;
    qa.state_until("the details saying added", |state| state["dialog"]["added"] == true)?;
    qa.click(DETAILS_CLOSE)?;
    Ok(())
}

fn refused_chat_add(qa: &mut Qa<'_>) -> SandboxResult {
    qa.plain_store()?;
    qa.ask(&format!("refuse POST chats/{AB_CHAT}/apps/entitlements 403"))?;
    qa.act(&format!("details {POLLS}"))?;
    qa.act("add-to chat")?;
    qa.state_until("the chat picker", |state| state["dialog"]["kind"] == "place")?;
    qa.act(&format!("choose {AB_CHAT}"))?;
    qa.act("go")?;
    thread::sleep(ANSWERED);
    let state = qa.state()?;
    ensure(state["dialog"]["kind"] == "place", || {
        format!("the picker closed after a refusal: {}", state["dialog"])
    })?;
    ensure(qa.added(&format!("chat {AB_CHAT}"))?.is_empty(), || {
        "the mock holds an app in the chat".to_owned()
    })?;
    qa.shots("place-chat-refused")?;
    qa.act("close")?;
    Ok(())
}

fn refused_removes(qa: &mut Qa<'_>) -> SandboxResult {
    // for me: Made-up Helper, added again by the refused add's second try
    qa.plain_store()?;
    qa.act("manage")?;
    qa.state_until("Manage your apps listing Made-up Helper", |state| {
        holds(&state["store"]["mine"], HELPER)
    })?;
    qa.ask(&format!("refuse DELETE users/apps/entitlements/{HELPER} 500"))?;
    qa.act(&format!("ask-remove {HELPER}"))?;
    qa.act("remove")?;
    let state = qa.state_until("the refused Remove told", |state| {
        state["store"]["remove_failed"] == true
    })?;
    ensure(holds(&state["store"]["mine"], HELPER), || {
        format!("gone after a refusal: {}", state["store"])
    })?;
    ensure(qa.added("personal")?.contains(&HELPER.to_owned()), || {
        "the mock lost the app".to_owned()
    })?;
    qa.shots("manage-remove-refused")?;
    qa.act("keep")?;
    // in a chat: Made-up Polls added to the a-b chat again, its Remove refused
    qa.act(&format!("details {POLLS}"))?;
    qa.act("add-to chat")?;
    qa.act(&format!("choose {AB_CHAT}"))?;
    qa.act("go")?;
    qa.added_until(&format!("chat {AB_CHAT}"), "Add to the a-b chat", |held| {
        held.iter().any(|id| id == POLLS)
    })?;
    qa.ask(&format!("refuse DELETE chats/{AB_CHAT}/apps/entitlements/{POLLS} 500"))?;
    qa.act(&format!("chat-apps {AB_CHAT}"))?;
    qa.state_until("the chat's apps listing Made-up Polls", |state| {
        holds(&state["dialog"]["shown"], POLLS)
    })?;
    qa.act(&format!("chat-ask-remove {POLLS}"))?;
    qa.act("chat-remove")?;
    let state = qa.state_until("the chat's refused Remove told", |state| {
        state["dialog"]["remove_failed"] == true
    })?;
    ensure(holds(&state["dialog"]["shown"], POLLS), || {
        format!("gone from the chat after a refusal: {}", state["dialog"])
    })?;
    qa.shots("chat-apps-remove-refused")?;
    qa.act("close")?;
    Ok(())
}

fn odd_store(qa: &mut Qa<'_>) -> SandboxResult {
    qa.plain_store()?;
    qa.ask("odd-store")?;
    qa.act("refresh")?;
    let state = qa.state_until("the odd store read", |state| holds(&state["store"]["org"], LONG))?;
    let store = &state["store"];
    ensure(strings(&store["popular"]).len() == MANY, || {
        format!("the popular section's apps: {}", strings(&store["popular"]).len())
    })?;
    ensure(store["pictures"][LONG] == false, || {
        format!("a picture for the long name: {store}")
    })?;
    qa.hover(STORE_MIDDLE)?;
    qa.shots("odd-store")?;
    // the pictures had their time: the off-CDN one is never asked for
    qa.state_until("the CDN picture drawn", |state| {
        state["store"]["pictures"]["00000000-0000-4000-8000-00000000c003"] == true
    })?;
    let asked = qa.ask(&format!("requests ANY {OFF_CDN_FILE}"))?;
    ensure(asked["count"] == 0, || {
        format!("the off-CDN picture was asked for: {asked}")
    })?;
    let state = qa.state()?;
    ensure(state["store"]["pictures"][OFF_CDN] == false, || {
        format!("the off-CDN app drawn with a picture: {}", state["store"])
    })?;
    // to the bottom: every popular app reachable
    for _ in 0..12 {
        qa.input.scroll(STORE_MIDDLE.0, STORE_MIDDLE.1, 5)?;
    }
    qa.shots("odd-store-bottom")?;
    // the row shows 24 of the 30; "See all" shows every one
    qa.act("see-all popular")?;
    qa.state_until("Popular on Teams' See all", |state| {
        state["store"]["see_all"] == "Some(Popular)"
    })?;
    qa.hover(STORE_MIDDLE)?;
    qa.shots("odd-see-all")?;
    for _ in 0..12 {
        qa.input.scroll(STORE_MIDDLE.0, STORE_MIDDLE.1, 5)?;
    }
    qa.shots("odd-see-all-bottom")?;
    qa.act("see-all")?;
    qa.act(&format!("details {LONG}"))?;
    qa.hover(AWAY)?;
    qa.shots("odd-details-long")?;
    qa.act("close")?;
    qa.act("search made-up tool")?;
    qa.state_until("the search for made-up tool finding every popular app", |state| {
        strings(&state["store"]["found"]).len() == MANY
    })?;
    qa.shot("odd-search-many")?;
    qa.act("search ")?;
    Ok(())
}

// --- the child: the app on the mock ---

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

/// The mock and a's engine on it.
struct MockLab {
    runtime: tokio::runtime::Runtime,
    mock: Arc<ChatServiceMock>,
    _trouter: Arc<TrouterMock>,
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
        let dir = tempfile::tempdir()?;
        let tokens = Arc::new(Tokens {
            token: mock.token(LAB_A).ok_or("the mock's token for a")?,
            renewals: broadcast::channel(4).0,
        });
        let origin = mock.origin();
        let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
        // the app writes go to the middle tier
        let transport = Arc::new(http()?.with_middle_tier(MIDDLE_TIER_BASE)?);
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
            client: Arc::new(Client::new(
                http()?
                    .with_presence(PRESENCE_BASE)?
                    .with_csa(CSA_BASE)?
                    .with_user_profile(USER_PROFILE_BASE)?
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
            _trouter: Arc::new(trouter),
            dir,
        };
        Ok((lab, engine))
    }
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

fn child_app() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let sandbox_dir = PathBuf::from(std::env::var_os(ui_sandbox::SANDBOX_ENV).ok_or("no sandbox dir")?);
    let (lab, engine) = MockLab::start()?;
    let first = support::started(Alias::A, engine)?;
    let settings = teams_ui::SettingsFile::new(lab.dir.path().join("ui").join("a.toml"));
    let runner = support::runner(lab.dir.path())?;
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let probe = Probe {
        mock: Arc::clone(&lab.mock),
        runtime: lab.runtime.handle().clone(),
    };
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
                let _ = launched.window.update(cx, |_, window, _| {
                    after_frames(window, READY_FRAMES, |_, _| say("ready"));
                });
                answer(launched.desktop, Some(probe), sandbox_dir.join(ASK), cx);
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

/// What the child reads and changes on the mock for the parent's questions.
pub(super) struct Probe {
    mock: Arc<ChatServiceMock>,
    runtime: tokio::runtime::Handle,
}

impl Probe {
    /// Runs `work` on the mock's runtime, from a thread of its own (GPUI's isn't one to block a runtime on).
    fn block<T: Send>(&self, work: impl Future<Output = T> + Send) -> Option<T> {
        thread::scope(|scope| scope.spawn(|| self.runtime.block_on(work)).join().ok())
    }

    fn reply(&self, verb: &str, rest: &str) -> Option<Value> {
        Some(match verb {
            "added" => {
                let place = match rest.split_once(' ') {
                    Some(("chat", id)) => AppPlace::Chat(id.to_owned()),
                    Some(("team", id)) => AppPlace::Team(id.to_owned()),
                    _ => AppPlace::Personal,
                };
                json!({"added": self.mock.apps_added(&place)})
            }
            "refuse" => {
                let mut words = rest.split(' ');
                let (Some(method), Some(path), Some(Ok(status))) =
                    (words.next(), words.next(), words.next().map(str::parse::<u16>))
                else {
                    return Some(json!({"error": format!("refuse what: {rest}")}));
                };
                self.block(self.mock.refuse_next(method, path, status));
                json!({"refused": rest})
            }
            "requests" => {
                let (method, needle) = rest.split_once(' ').unwrap_or(("ANY", rest));
                let count = self
                    .block(self.mock.received_requests())
                    .unwrap_or_default()
                    .iter()
                    .filter(|request| method == "ANY" || request.method.as_str() == method)
                    .filter(|request| {
                        let url = request.url.as_str().replace("%3A", ":").replace("%40", "@");
                        url.contains(needle)
                    })
                    .count();
                json!({"count": count})
            }
            "odd-store" => {
                let (store, definitions) = odd_store_answers();
                self.block(self.mock.answer_next("POST", "users/apps/store", 200, &store));
                self.block(self.mock.answer_next("POST", "batchedDefinitions", 200, &definitions));
                json!({"odd": true})
            }
            _ => return None,
        })
    }
}

/// A store of many apps: [`LONG`] and [`OFF_CDN`] by the org, then [`MANY`] popular ones with no picture, and one
/// with the CDN's made-up tile.
fn odd_store_answers() -> (Value, Value) {
    let mut definitions = vec![
        json!({"id": LONG,
               "name": "Made-up Quarterly Cross-Team Planning, Review and Retrospective Assistant for Very Long Names",
               "developerName": "Made-up Maker With An Unusually Long Company Name Incorporated",
               "shortDescription": "A made-up app whose name, maker and line are all far longer than a tile has room for, to see where they stop",
               "version": "10.20.30", "accentColor": "#8f6400",
               "bots": [{"id": "00000000-0000-4000-8000-00000000d001", "scopes": ["personal", "groupChat", "team"]}]}),
        json!({"id": OFF_CDN, "name": "Made-up Off CDN", "developerName": "Made-up Maker",
               "shortDescription": "Its picture is on a host that is not the CDN", "accentColor": "#c239b3",
               "largeImageUrl": format!("https://apps.own.example.test/teamsappdata/evergreen-assets/apps/{OFF_CDN_FILE}")}),
        json!({"id": "00000000-0000-4000-8000-00000000c003", "name": "Made-up On CDN", "developerName": "Made-up Maker",
               "shortDescription": "Its picture is on the CDN", "accentColor": "#4f52b2",
               "largeImageUrl": "https://res.cdn.office.net/teamsappdata/evergreen-assets/apps/made-up-updates_largeImage.png?v=1"}),
    ];
    let mut popular = Vec::new();
    for at in 0..MANY {
        let id = format!("00000000-0000-4000-8000-0000000e{at:04}");
        definitions.push(
            json!({"id": id, "name": format!("Made-up Tool {at:02}"), "developerName": "Made-up Maker",
                                "shortDescription": format!("Made-up tool number {at}"), "accentColor": "#038387"}),
        );
        popular.push(json!({"id": id}));
    }
    let store = json!({"appCategoriesDictionary": {
        "adminPreInstalledApps": {"apps": [{"id": LONG}, {"id": OFF_CDN}, {"id": "00000000-0000-4000-8000-00000000c003"}],
                                  "totalAppsCount": 3},
        "popularApps": {"apps": popular, "totalAppsCount": MANY},
    }});
    (store, Value::Array(definitions))
}

/// Answers each question the parent writes to `ask`, one `answer <json>` line each.
pub(super) fn answer(desktop: Entity<teams_ui::Desktop>, probe: Option<Probe>, ask: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(50)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let question = question.trim_end_matches('\n').to_owned();
            if question == "quit" {
                say("answer {}");
                cx.update(|cx| cx.quit());
                return;
            }
            let (verb, rest) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            if let Some(reply) = probe.as_ref().and_then(|probe| probe.reply(verb, rest)) {
                say(&format!("answer {reply}"));
                continue;
            }
            let Some(window) = cx.update(|cx| desktop.read(cx).window()) else {
                say("answer {\"error\":\"no window\"}");
                continue;
            };
            let Some(shell) = shell_of(window, cx) else {
                say("answer {\"error\":\"no shell\"}");
                continue;
            };
            let reply = window
                .update(cx, |_, window, cx| reply_to(verb, rest, &shell, window, cx))
                .unwrap_or_else(|_| json!({"error": "no window"}));
            say(&format!("answer {reply}"));
        }
    })
    .detach();
}

/// The answer to a question about the window.
fn reply_to(verb: &str, rest: &str, shell: &Entity<Shell>, window: &mut Window, cx: &mut App) -> Value {
    match verb {
        "state" => {
            let mut state = state_of(shell, cx);
            // what the window saw of the pointer, so a click that went nowhere says where it went
            let pointer = window.mouse_position();
            state["window"] = json!({
                "active": window.is_window_active(),
                "hovered": window.is_window_hovered(),
                "pointer": [f32::from(pointer.x), f32::from(pointer.y)],
            });
            state
        }
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
        "do" => {
            let (what, arg) = rest.split_once(' ').unwrap_or((rest, ""));
            act(what, arg, shell, window, cx).map_or_else(|error| json!({"error": error}), |()| json!({"done": rest}))
        }
        _ => json!({"error": format!("unknown question {verb}")}),
    }
}

fn page_of(shell: &Entity<Shell>, cx: &App) -> Result<Entity<AppsPage>, String> {
    shell
        .read(cx)
        .apps()
        .cloned()
        .ok_or_else(|| "the store was never opened".to_owned())
}

fn dialog_of<T: 'static>(shell: &Entity<Shell>, cx: &App) -> Result<Entity<T>, String> {
    shell
        .read(cx)
        .team_dialog()
        .cloned()
        .and_then(|dialog| dialog.downcast::<T>().ok())
        .ok_or_else(|| format!("no {} open", std::any::type_name::<T>()))
}

fn listed(page: &Entity<AppsPage>, id: &str, cx: &App) -> Result<StoreApp, String> {
    let page = page.read(cx);
    page.listing()
        .and_then(|listing| listing.app(id).cloned())
        .or_else(|| page.mine_shown(cx).into_iter().find(|store| store.app.id == id))
        .ok_or_else(|| format!("{id} is not in the store"))
}

/// Does `what` in the window, as the button of that name does.
fn act(what: &str, arg: &str, shell: &Entity<Shell>, window: &mut Window, cx: &mut App) -> Result<(), String> {
    match what {
        "store" => shell.update(cx, |shell, cx| shell.open_apps(window, cx)),
        "refresh" => page_of(shell, cx)?.update(cx, AppsPage::refresh),
        "search" => page_of(shell, cx)?.update(cx, |page, cx| page.set_search(arg, window, cx)),
        "manage" => page_of(shell, cx)?.update(cx, |page, cx| page.show(AppsView::Manage, window, cx)),
        "see-all" => {
            let section = match arg {
                "org" => Some(StoreSection::Org),
                "popular" => Some(StoreSection::Popular),
                _ => None,
            };
            page_of(shell, cx)?.update(cx, |page, cx| page.see_all(section, cx));
        }
        "row" => page_of(shell, cx)?.update(cx, |page, cx| page.toggle_row(arg, cx)),
        "ask-remove" => {
            let page = page_of(shell, cx)?;
            let store = listed(&page, arg, cx)?;
            page.update(cx, |page, cx| page.ask_remove(store, cx));
        }
        "remove" => page_of(shell, cx)?.update(cx, AppsPage::remove),
        "keep" => page_of(shell, cx)?.update(cx, AppsPage::keep),
        "details" => {
            let store = listed(&page_of(shell, cx)?, arg, cx)?;
            shell.update(cx, |shell, cx| shell.open_app_details(store, window, cx));
        }
        "add" => dialog_of::<AppDetailsDialog>(shell, cx)?.update(cx, AppDetailsDialog::add),
        "add-to" => {
            let team = arg == "team";
            dialog_of::<AppDetailsDialog>(shell, cx)?.update(cx, |details, cx| details.add_to(team, cx));
        }
        "choose" => {
            let place = arg.to_owned();
            dialog_of::<AddToPlaceDialog>(shell, cx)?.update(cx, |dialog, cx| dialog.choose(place, cx));
        }
        "go" => dialog_of::<AddToPlaceDialog>(shell, cx)?.update(cx, AddToPlaceDialog::go),
        "close" => {
            if let Ok(details) = dialog_of::<AppDetailsDialog>(shell, cx) {
                details.update(cx, AppDetailsDialog::close);
            } else {
                shell.update(cx, Shell::close_team_dialog);
            }
        }
        "chat-apps" => {
            let chat = arg.to_owned();
            shell.update(cx, |shell, cx| {
                shell.open_chat(chat.clone(), window, cx);
                shell.open_chat_apps(chat, window, cx);
            });
        }
        "chat-ask-remove" => {
            let dialog = dialog_of::<ChatAppsDialog>(shell, cx)?;
            let store = dialog
                .read(cx)
                .shown(cx)
                .into_iter()
                .find(|store| store.app.id == arg)
                .ok_or_else(|| format!("{arg} is not in the chat's apps"))?;
            dialog.update(cx, |dialog, cx| dialog.ask_remove(store, cx));
        }
        "chat-remove" => dialog_of::<ChatAppsDialog>(shell, cx)?.update(cx, ChatAppsDialog::remove),
        "manage-team" => shell.update(cx, |shell, cx| {
            shell.open_manage_team(TEAM.to_owned(), ManageTab::Apps, window, cx);
        }),
        "team-ask-remove" => {
            let page = shell.read(cx).manage_team().cloned().ok_or("Manage team isn't open")?;
            let app = page
                .read(cx)
                .apps_shown()
                .into_iter()
                .find(|app| app.id == arg)
                .ok_or_else(|| format!("{arg} is not on the team"))?;
            page.update(cx, |page, cx| page.ask_remove_app(app, cx));
        }
        "team-remove" => {
            let page = shell.read(cx).manage_team().cloned().ok_or("Manage team isn't open")?;
            page.update(cx, teams_ui::manage_team::ManageTeam::remove_app);
        }
        _ => return Err(format!("unknown action {what}")),
    }
    Ok(())
}

/// What the window holds: the main view, the toast, the store (its view, sections, search, what it found, the
/// account's apps, the pictures drawn, Remove's question), the dialog open, and Manage team's apps.
pub(super) fn state_of(shell: &Entity<Shell>, cx: &App) -> Value {
    let shell_read = shell.read(cx);
    let main = format!("{:?}", shell_read.main());
    let main = main.split('(').next().unwrap_or_default().to_owned();
    let store = shell_read.apps().map_or(Value::Null, |page| store_state(page, cx));
    let manage_team = shell_read.manage_team().map_or(Value::Null, |page| {
        let ids: Vec<String> = page.read(cx).apps_shown().into_iter().map(|app| app.id).collect();
        json!({"apps": ids})
    });
    json!({
        "main": main,
        "toast": shell_read.toast().map(ToString::to_string),
        "store": store,
        "dialog": dialog_state(shell, cx),
        "manage_team": manage_team,
    })
}

fn store_state(page: &Entity<AppsPage>, cx: &App) -> Value {
    let page = page.read(cx);
    let ids = |apps: Vec<StoreApp>| -> Vec<String> { apps.into_iter().map(|store| store.app.id).collect() };
    let (org, popular, every) = page.listing().map_or_else(Default::default, |listing| {
        (
            listing.sections.org.clone(),
            listing.sections.popular.clone(),
            listing
                .apps
                .iter()
                .map(|store| store.app.id.clone())
                .collect::<Vec<_>>(),
        )
    });
    let pictures: serde_json::Map<String, Value> = every
        .iter()
        .map(|id| (id.clone(), Value::Bool(page.shows_picture(id))))
        .collect();
    let added: serde_json::Map<String, Value> = every
        .iter()
        .map(|id| (id.clone(), Value::Bool(page.added(id))))
        .collect();
    let (removing, remove_failed) = page
        .removing()
        .map_or((Value::Null, false), |(store, failed)| (json!(store.app.id), failed));
    json!({
        "view": format!("{:?}", page.view()),
        "see_all": format!("{:?}", page.seeing_all()),
        "loaded": page.listing().is_some(),
        "org": org,
        "popular": popular,
        "found": ids(page.found(cx)),
        "mine": ids(page.mine_shown(cx)),
        "pictures": pictures,
        "added": added,
        "removing": removing,
        "remove_failed": remove_failed,
    })
}

fn dialog_state(shell: &Entity<Shell>, cx: &App) -> Value {
    if let Ok(details) = dialog_of::<AppDetailsDialog>(shell, cx) {
        let details = details.read(cx);
        return json!({"kind": "details", "added": details.added()});
    }
    if let Ok(place) = dialog_of::<AddToPlaceDialog>(shell, cx) {
        let place = place.read(cx);
        let places: Vec<String> = place.shown(cx).into_iter().map(|place| place.id).collect();
        return json!({"kind": "place", "places": places});
    }
    if let Ok(chat_apps) = dialog_of::<ChatAppsDialog>(shell, cx) {
        let chat_apps = chat_apps.read(cx);
        let shown: Vec<String> = chat_apps.shown(cx).into_iter().map(|store| store.app.id).collect();
        let (removing, failed) = chat_apps
            .removing()
            .map_or((Value::Null, false), |(store, failed)| (json!(store.app.id), failed));
        return json!({"kind": "chat-apps", "shown": shown, "removing": removing, "remove_failed": failed});
    }
    if shell.read(cx).team_dialog().is_some() {
        return json!({"kind": "other"});
    }
    json!({"kind": null})
}
