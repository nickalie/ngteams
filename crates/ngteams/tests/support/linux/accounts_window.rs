//! Several accounts in the app's one window, in the UI sandbox, on the mock (`harness = false`: GPUI runs on the
//! process's main thread).
//!
//! The test runs itself again inside the sandbox ([`Sandbox::spawn_with_fds`], only the view trace's pipe passed
//! through) as the app of lab accounts a and b, each one's engine on the mock chat service and its Trouter
//! (`teams-mock`, through the `test-support` mock origins, a test roster and log), handed to the app as `ngteams`
//! hands its own ([`ngteams::app::launch`]). The parent reads the view trace: both accounts on the rail, a shown and
//! connected; screenshots the window (the rail's avatars above Settings; kept under `target/ui-shots/`); switches to b
//! with the sandbox's virtual keyboard (Ctrl+Alt+Down) and screenshots it again; then c posts on the mock to a's chat
//! with c while b is shown: a's notification reaches the sandbox's fake server naming no account, and activating it
//! shows a again with that chat open. Then an own server's account is added beside the two: the
//! rail's "+" opens the window asking for an account, "Own server" shows its fields, and the server's address, the user
//! name and the password typed sign it in on an in-process `ngteams-server` over TLS (its certificate authority in the
//! account's `ca_file`); its tokens are kept under its server and UPN, its region is the server's own, and its engine
//! starts on the server's chat service: it is on the rail beside the two and shown, its tile naming its server when
//! pointed at. Its chats are read once its push channel is up, which needs the server's Trouter: until the server
//! serves one the account stays "Reconnecting…" (said as `note own-account-connected=false`), and from then on its
//! chat requests are required on its server. The tray's "Quit" quits the app, the engines' threads joined.
//!
//! Then the app is started again on the same files and the same server (the parent keeps both; the first run's
//! credential store, which is this process's alone in the sandbox, hands the own account's entries over in a file
//! the second run deletes, standing in for the desktop's keyring): all three accounts come back as `ngteams` starts
//! them, the own one from its `accounts.toml` entry on its server, with its chat list.
//!
//! The own server is the real `ngteams-server` binary when there is one, its user made with `user add`
//! (`accounts_own.rs`), and the user has a's user name: the same name on Microsoft 365 and on the own server stays two
//! accounts. Before the sign-in goes through, a server nothing listens on and a wrong password each say why under the
//! fields; Tab walks address, user name, password and Enter submits. Afterwards the app's own request log and the
//! mock say where the requests went: the own account's to its server alone, none of its to the mock.
//!
//! More scenes, each the app started again in the same sandbox ([`SCENES`]; `NGTEAMS_ACCOUNTS_SCENES` names the ones
//! to run; the `qa_` ones need the server binary and are skipped without it): the own server's form in Teams Dark, the place chosen with the keyboard alone (Shift+Tab to the choice,
//! Right, Enter), and what it says of an empty field; the window without own servers,
//! which asks for a Microsoft 365 account alone; an own server's account signed out, asked for its password again
//! and forgotten as `ngteams --forget <server>/<UPN>` forgets it; an own server's account whose server serves no
//! chats, told so on its sign-in screen, and what that screen says of a server that can't be reached (with and
//! without a `ca_file` in its entry, the entry read again at each sign-in), in Teams Light and Dark; an own server's
//! entry written by hand that never signed in, asked its password with no "again" and signed in on the same screen
//! once the `ca_file` it lacked is written into the entry; and an own server's account as the window shows
//! one whose server isn't on loopback (its engine on the mock; nothing is sent to that address): `ngteamsctl accounts`
//! names its server, its address `<server>/<UPN>` switches to it, Settings > General, its rail tile and its account
//! menu name its server, and the Microsoft 365 account of the same user name beside it names none. And the choice of
//! place worked as a user without a pointer works it, in Teams Light and Dark: Tab reaches it from the account's field
//! past "Sign in", each arrow, Home and End choose, Space and Enter go on to the place's first field, and Tab walks
//! on through the fields and "Sign in" back to the choice; then the pointer chooses each place.
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
use gpui::{App, AsyncApp, Window, WindowingModes};
use ngteams::accounts::Started;
use ngteams::app::{self, Launch};
use ngteams::secrets::AppSecrets;
#[path = "accounts_admin.rs"]
mod admin;
#[path = "accounts_admin_qa.rs"]
mod admin_qa;
#[path = "accounts_card_qa.rs"]
mod card_qa;
#[path = "accounts_hidden_qa.rs"]
mod hidden_qa;
#[path = "accounts_own.rs"]
mod own;
#[path = "../own_server.rs"]
mod own_server;
#[path = "../own_server_binary.rs"]
#[expect(
    dead_code,
    reason = "the own server's tests share this support; this one registers no app"
)]
mod own_server_binary;
#[path = "../window.rs"]
mod support;
use ngteams::descriptors::adopt_trace;
use support::Alias;
use support::display_name;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_ctl::cli::{self, Cli, Exit};
use teams_mock::{AC_CHAT, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, TrouterMock};
use teams_proto::Mri;
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::tray::{ITEM_PATH, MENU_PATH, MENU_QUIT};
use tokio::sync::broadcast;
use ui_sandbox::input::{Modifier, VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

/// Which mode the child runs in.
const CHILD_ENV: &str = "NGTEAMS_ACCOUNTS_TEST";
/// The child's view trace descriptor.
const TRACE_FD_ENV: &str = "NGTEAMS_ACCOUNTS_TRACE_FD";
/// From the child's start to its `ready` (the mock started, the window drawn).
const STARTUP: Duration = Duration::from_secs(180);
/// A sign-in on the own server (its password hash is slow in a debug build on a loaded host).
const SIGN_IN: Duration = Duration::from_secs(120);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(30);
/// Frames the window presents before the child says `ready`.
const READY_FRAMES: usize = 3;
/// The file in the sandbox dir the parent writes a chat's id to, for c to post there.
const POST_TRIGGER: &str = "post-as-c";
/// What c posts.
const C_SAYS: &str = "hello a, from c";
/// Where the app's files are, when the parent keeps them for a second run; the child's own temporary directory
/// otherwise.
const ROOT_ENV: &str = "NGTEAMS_ACCOUNTS_ROOT";
/// The own server the parent runs: its origin and its certificate authority's file.
const OWN_ORIGIN_ENV: &str = "NGTEAMS_ACCOUNTS_OWN_ORIGIN";
const OWN_CA_ENV: &str = "NGTEAMS_ACCOUNTS_OWN_CA";
/// The own server's user: a's user name, so the same name is on Microsoft 365 and on the own server.
const OWN_UPN: &str = "bot-a@lab.example.test";
const OWN_NAME: &str = "Bot A Own";
/// The server of the account the window shows as an own server's (the `shown` scenes; nothing is sent to it).
const SHOWN_SERVER: &str = "https://chat.example.test:8443";
const SHOWN_HOST: &str = "chat.example.test:8443";
/// The app's control socket, in the sandbox dir.
const CTL_SOCKET: &str = "ctl.sock";
/// The file in the sandbox dir the child adds a line `<UPN>` to each time the app opens the sign-in window again for
/// an account signed out (a release's `main` opens it; the child only says it).
const SIGN_IN_AGAIN_FILE: &str = "sign-in-again";
/// The file in the sandbox dir the child adds a line `<MRI>` to, one time, when an account's engine has stopped and its
/// thread is joined (`Bridge::running` is false). The app opens the sign-in window again only after this.
const ENGINE_STOPPED_FILE: &str = "engine-stopped";
/// The file in the sandbox dir the parent writes to have the mock's requests said.
const MOCK_ASK: &str = "say-mock";
/// The rail's "+", and the "Own server" segment of the window it opens, as the sandbox's output shows them.
const RAIL_ADD_AT: (u32, u32) = (38, 713);
const OWN_SEGMENT_AT: (u32, u32) = (936, 164);
const MICROSOFT_SEGMENT_AT: (u32, u32) = (342, 164);
/// The own server's address field in that window.
const SERVER_FIELD_AT: (u32, u32) = (640, 264);
/// The third account's tile on the rail (three accounts).
const THIRD_TILE_AT: (u32, u32) = (38, 660);
/// The first account's tile on the rail (three accounts).
const FIRST_TILE_AT: (u32, u32) = (38, 534);

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("again") => own::child_again(false),
        Ok("again-dark") => own::child_again(true),
        Ok("said") => own::child_said(false),
        Ok("said-dark") => own::child_said(true),
        Ok("first") => own::child_first(false),
        Ok("first-dark") => own::child_first(true),
        Ok("setup") => admin::child_setup(false),
        Ok("setup-dark") => admin::child_setup(true),
        Ok("change") => admin::child_change(false),
        Ok("change-dark") => admin::child_change(true),
        Ok(other) => match Mode::named(other) {
            Some(mode) => child_window(mode),
            None => Err(format!("unknown mode {other}").into()),
        },
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("accounts window test: {error}");
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

// --- the parent: the sandbox, and the checks from outside ---

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams accounts window", &options)? else {
        return Ok(());
    };
    let only = std::env::var("NGTEAMS_ACCOUNTS_SCENES").unwrap_or_default();
    let mut failed = Vec::new();
    let mut passed = 0_usize;
    for (name, scene) in SCENES {
        if !only.is_empty() && !only.split(',').any(|wanted| name.contains(wanted.trim())) {
            continue;
        }
        if name.starts_with(QA_PREFIX) && own_server_binary::binary()?.is_none() {
            say(&format!(
                "skipped {name}: needs the ngteams-server binary (cargo build -p ngteams-server, or {})",
                own_server_binary::BIN_ENV
            ));
            continue;
        }
        match scene(&sandbox) {
            Ok(()) => {
                passed += 1;
                say(&format!("ok {name}"));
            }
            Err(error) => {
                say(&format!("FAILED {name}: {error}"));
                failed.push(format!("{name}: {error}"));
            }
        }
    }
    say(&format!("scenes: {passed} passed, {} failed", failed.len()));
    if failed.is_empty() {
        say("ngteams accounts window: ok");
        return Ok(());
    }
    Err(format!("{}{}", failed.join("\n"), sandbox.logs()).into())
}

/// The scenes of [`admin_qa`], which run only on the real server binary.
const QA_PREFIX: &str = "qa_";

/// One run of the app in the sandbox, by name.
type Scene = (&'static str, fn(&Sandbox) -> SandboxResult);

/// The scenes, each the app started afresh (see the module docs).
const SCENES: [Scene; 37] = [
    (
        "two_accounts_and_an_own_servers_account_added",
        two_accounts_in_one_window,
    ),
    ("own_servers_form_in_teams_dark", own_form_in_dark),
    ("signed_out_own_account_password_again", |sandbox| {
        password_again(sandbox, false)
    }),
    ("signed_out_own_account_password_again_dark", |sandbox| {
        password_again(sandbox, true)
    }),
    ("own_sign_in_screen_says_why", |sandbox| said_reasons(sandbox, false)),
    ("own_sign_in_screen_says_why_dark", |sandbox| {
        said_reasons(sandbox, true)
    }),
    ("own_entry_written_by_hand_first_sign_in", |sandbox| {
        first_sign_in(sandbox, false)
    }),
    ("own_entry_written_by_hand_first_sign_in_dark", |sandbox| {
        first_sign_in(sandbox, true)
    }),
    ("own_entry_written_by_hand_first_sign_in_at_480", |_| {
        first_sign_in_at_480(false)
    }),
    ("own_entry_written_by_hand_first_sign_in_at_480_dark", |_| {
        first_sign_in_at_480(true)
    }),
    ("own_account_shown_names_its_server", |sandbox| shown(sandbox, false)),
    ("own_account_shown_names_its_server_dark", |sandbox| {
        shown(sandbox, true)
    }),
    ("sign_in_choice_by_keyboard", |sandbox| {
        choice_by_keyboard(sandbox, false)
    }),
    ("sign_in_choice_by_keyboard_dark", |sandbox| {
        choice_by_keyboard(sandbox, true)
    }),
    ("admin_first_run_and_manage_users", admin::first_admin_and_manage_users),
    ("admin_manage_users_dark", admin::manage_users_in_dark),
    ("admin_first_run_at_480", |_| admin::first_admin_at_480(false)),
    ("admin_first_run_at_480_dark", |_| admin::first_admin_at_480(true)),
    ("admin_password_change_at_480", |_| admin::password_change_at_480(false)),
    ("admin_password_change_at_480_dark", |_| {
        admin::password_change_at_480(true)
    }),
    ("qa_admin_first_run_on_the_binary", admin_qa::first_run_on_the_binary),
    ("qa_manage_users_seeded", admin_qa::manage_users_seeded),
    ("qa_roles_while_running", admin_qa::roles_while_running),
    ("qa_page_at_480", |_| admin_qa::page_at(480, false)),
    ("qa_page_at_480_dark", |_| admin_qa::page_at(480, true)),
    ("qa_page_at_1280", |_| admin_qa::page_at(1280, false)),
    ("qa_page_at_1280_dark", |_| admin_qa::page_at(1280, true)),
    ("qa_own_card_at_1280", |_| card_qa::card_at(1280, false)),
    ("qa_own_card_at_1280_dark", |_| card_qa::card_at(1280, true)),
    ("qa_own_card_at_480", |_| card_qa::card_at(480, false)),
    ("qa_own_card_at_480_dark", |_| card_qa::card_at(480, true)),
    ("qa_own_hidden_at_1280", |_| {
        hidden_qa::hidden_at(hidden(1280, false, false))
    }),
    ("qa_own_hidden_at_1280_dark", |_| {
        hidden_qa::hidden_at(hidden(1280, true, false))
    }),
    ("qa_own_hidden_at_480", |_| {
        hidden_qa::hidden_at(hidden(480, false, false))
    }),
    ("qa_own_hidden_at_480_dark", |_| {
        hidden_qa::hidden_at(hidden(480, true, false))
    }),
    ("qa_own_hidden_workflows_on_at_1280", |_| {
        hidden_qa::hidden_at(hidden(1280, false, true))
    }),
    ("qa_own_hidden_workflows_on_at_1280_dark", |_| {
        hidden_qa::hidden_at(hidden(1280, true, true))
    }),
];

/// The look of a [`hidden_qa`] scene.
fn hidden(width: u32, dark: bool, workflows: bool) -> hidden_qa::Look {
    hidden_qa::Look { width, dark, workflows }
}

/// The app's sign-in window again for `account`, said by [`said_sign_in_again`].
fn sign_in_again_saying(account: &ngteams::accounts_file::PersonalAccount) -> app::SignInAgain {
    let upn = account.upn.clone();
    std::rc::Rc::new(move |_: &mut App| said_sign_in_again(&upn))
}

/// Adds `upn` to [`SIGN_IN_AGAIN_FILE`]: the app opened the sign-in window again for that account.
fn said_sign_in_again(upn: &str) {
    say(&format!("sign-in-again {upn}"));
    let Ok(dir) = own::sandbox_dir() else {
        return;
    };
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(SIGN_IN_AGAIN_FILE));
    if let Ok(mut file) = file {
        let _ = writeln!(file, "{upn}");
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

    /// Waits for a line `found` takes (one seen already included), named `what` in the error.
    fn wait_where(&mut self, what: &str, found: impl Fn(&serde_json::Value) -> bool) -> SandboxResult {
        if self.seen.iter().any(&found) {
            return Ok(());
        }
        self.next_where(what, found)
    }

    /// Fails if a line `found` takes, named `what` in the error, comes within `within`.
    fn none_within(
        &mut self,
        within: Duration,
        what: &str,
        found: impl Fn(&serde_json::Value) -> bool,
    ) -> SandboxResult {
        let until = std::time::Instant::now() + within;
        while let Some(left) = until.checked_duration_since(std::time::Instant::now()) {
            let Ok(line) = self.lines.recv_timeout(left) else {
                break;
            };
            let hit = found(&line);
            self.seen.push(line);
            ensure(!hit, || format!("trace line {what}; seen {:?}", self.seen))?;
        }
        Ok(())
    }

    /// Whether a line `found` takes comes within `within`, among those not seen yet.
    fn next_within(&mut self, within: Duration, found: impl Fn(&serde_json::Value) -> bool) -> bool {
        let until = std::time::Instant::now() + within;
        while let Some(left) = until.checked_duration_since(std::time::Instant::now()) {
            let Ok(line) = self.lines.recv_timeout(left) else {
                return false;
            };
            let hit = found(&line);
            self.seen.push(line);
            if hit {
                return true;
            }
        }
        false
    }

    /// Waits for a line `found` takes among those not seen yet.
    fn next_where(&mut self, what: &str, found: impl Fn(&serde_json::Value) -> bool) -> SandboxResult {
        loop {
            let line = self
                .lines
                .recv_timeout(WAIT)
                .map_err(|_| format!("no trace line {what}; seen {:?}", self.seen))?;
            let hit = found(&line);
            self.seen.push(line);
            if hit {
                return Ok(());
            }
        }
    }
}

/// A trace line of `view` with `change` naming account `account` among its people.
fn about(line: &serde_json::Value, view: &str, change: &str, account: &str) -> bool {
    line["view"] == view && line["change"] == change && line["people"] == serde_json::json!([account])
}

/// Where the offline runs' screenshots are kept: `target/ui-shots/`, never committed.
fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Screenshots the window as `name` under [`shots_dir`], after the frames that follow the trace's line.
fn shot(sandbox: &Sandbox, name: &str) -> SandboxResult {
    thread::sleep(Duration::from_secs(1));
    let png = sandbox.screenshot(name, true)?;
    fs::copy(&png, shots_dir()?.join(name))?;
    Ok(())
}

/// The app's child in `mode`, started and `ready`: the lines it said before that, and its view trace.
fn start(sandbox: &Sandbox, mode: &str) -> SandboxResult<(Running, TraceReader, Vec<String>)> {
    start_with(sandbox, mode, &[])
}

/// [`start`] with `kept` in the child's environment.
fn start_with(
    sandbox: &Sandbox,
    mode: &str,
    kept: &[(&'static str, String)],
) -> SandboxResult<(Running, TraceReader, Vec<String>)> {
    let (reader, writer) = io::pipe()?;
    let fd = writer.as_raw_fd().to_string();
    let mut env = vec![(CHILD_ENV, mode.to_owned()), (TRACE_FD_ENV, fd)];
    env.extend(kept.iter().cloned());
    if let Some(binary) = std::env::var_os(own_server_binary::BIN_ENV) {
        env.push((own_server_binary::BIN_ENV, binary.to_string_lossy().into_owned()));
    }
    let env: Vec<(&str, &str)> = env.iter().map(|(name, value)| (*name, value.as_str())).collect();
    let child = sandbox.spawn_with_fds(&std::env::current_exe()?, &[], &env, &[writer.as_fd()])?;
    // the child holds the only writer from here, so the trace ends when it exits
    drop(writer);
    let mut running = Running::new(child)?;
    let trace = TraceReader::start(reader);
    let mut said = Vec::new();
    loop {
        let line = running
            .line_where(STARTUP, |_| true)
            .map_err(|error| running.fail(&format!("no ready line: {error:?}; said {said:?}")))?;
        if line == "ready" {
            return Ok((running, trace, said));
        }
        if line.starts_with("own-server ") {
            say(&line);
        }
        said.push(line);
    }
}

/// Waits for the accounts `accounts` on the rail, the first one connected and its chat list shown.
fn accounts_shown(trace: &mut TraceReader, running: &mut Running, accounts: &[&str]) -> SandboxResult {
    for account in accounts {
        trace
            .wait_where("account added", |line| about(line, "account", "added", account))
            .map_err(|error| running.fail(&error.to_string()))?;
    }
    trace
        .wait_where("status connected", |line| {
            line["view"] == "status" && line["change"] == "connected"
        })
        .map_err(|error| running.fail(&error.to_string()))?;
    trace
        .wait_where("a's chat list", |line| {
            line["view"] == "chat-list" && line["change"] == "shown"
        })
        .map_err(|error| running.fail(&error.to_string()))?;
    Ok(())
}

/// The tray's "Quit", then the child's exit with its engines released.
fn quit_and_finish(sandbox: &Sandbox, trace: &mut TraceReader, running: Running) -> SandboxResult {
    quit(sandbox, trace, &running)?;
    running
        .line_where(WAIT, |line| line == "engines-released")
        .map_err(|error| format!("the engines weren't released: {error:?}"))?;
    let (status, stderr) = running.finish(WAIT)?;
    if !status.success() {
        return Err(format!("the app exited {status}: {stderr}").into());
    }
    Ok(())
}

fn two_accounts_in_one_window(sandbox: &Sandbox) -> SandboxResult {
    // the own server and the app's files outlive the app's first run
    let server = own::Own::start()?;
    server.add_user(OWN_UPN, OWN_NAME)?;
    let root = tempfile::tempdir()?;
    let kept = [
        (ROOT_ENV, root.path().display().to_string()),
        (OWN_ORIGIN_ENV, server.origin().to_owned()),
        (OWN_CA_ENV, server.ca_file().display().to_string()),
    ];
    let (mut running, mut trace, _) = start_with(sandbox, "window", &kept)?;
    accounts_shown(&mut trace, &mut running, &[LAB_A, LAB_B])?;
    shot(sandbox, "ngteams-accounts-a.png")?;

    let mut input = sandbox.input()?;
    input.key_with(&[Modifier::Ctrl, Modifier::Alt], keys::DOWN)?;
    trace
        .next_where("switched to b", |line| about(line, "account", "switched", LAB_B))
        .map_err(|error| running.fail(&error.to_string()))?;
    shot(sandbox, "ngteams-accounts-b.png")?;

    a_s_message_notifies_and_shows_a(sandbox, &mut trace, &running)?;
    shot(sandbox, "ngteams-accounts-notified.png")?;
    let own = an_own_servers_account_is_added(sandbox, &mut input, &mut trace, &running, server.origin())?;
    let connected = own_account_connected(sandbox, server.origin())?;
    each_accounts_requests_stay_on_its_services(sandbox, &running, true, connected)?;
    own_tile_pointed_at(sandbox, &mut input, "ngteams-own-rail-tile-real.png")?;
    fs::write(sandbox.path(own::SECRETS_ASK), "")?;
    let handed = running
        .line_where(WAIT, |line| line.starts_with("own-secrets "))
        .map_err(|error| format!("the own account's credentials weren't handed over: {error:?}"))?;
    ensure(handed == "own-secrets handed=3", || {
        format!("the own account's credentials: {handed}")
    })?;
    quit_and_finish(sandbox, &mut trace, running)?;
    the_accounts_come_back(sandbox, &kept, server.origin(), &own)
}

/// The pointer on the third account's tile until its tooltip shows, screenshot as `name`, then away.
fn own_tile_pointed_at(sandbox: &Sandbox, input: &mut VirtualInput, name: &str) -> SandboxResult {
    tile_pointed_at(sandbox, input, THIRD_TILE_AT, name)
}

/// The pointer on the tile at `tile` until its tooltip shows, screenshot as `name`, then away.
fn tile_pointed_at(sandbox: &Sandbox, input: &mut VirtualInput, tile: (u32, u32), name: &str) -> SandboxResult {
    input.move_to(tile.0, tile.1)?;
    thread::sleep(Duration::from_secs(2));
    shot(sandbox, name)?;
    input.move_to(640, 400)?;
    Ok(())
}

/// A click on the shown account's tile at `tile` opens its account menu, screenshot as `name` with the pointer away;
/// a second click closes it.
fn account_menu_opened(
    sandbox: &Sandbox,
    input: &mut VirtualInput,
    trace: &mut TraceReader,
    tile: (u32, u32),
    name: &str,
) -> SandboxResult {
    let menu = |change: &'static str| {
        move |line: &serde_json::Value| line["view"] == "status-menu" && line["change"] == change
    };
    input.click(tile.0, tile.1, keys::BTN_LEFT)?;
    trace.next_where("the account menu opened", menu("opened"))?;
    input.move_to(640, 400)?;
    shot(sandbox, name)?;
    input.click(tile.0, tile.1, keys::BTN_LEFT)?;
    trace.next_where("the account menu closed", menu("closed"))?;
    input.move_to(640, 400)?;
    Ok(())
}

/// How long the own account's push channel has to come up before it is said not to be.
const CONNECTS: Duration = Duration::from_secs(10);

/// Whether the own server's account, the third of three in `ngteamsctl accounts` and named with its server
/// `origin`, is connected (its push channel up, its chats read); said as `note own-account-connected=<bool>`.
fn own_account_connected(sandbox: &Sandbox, origin: &str) -> SandboxResult<bool> {
    let start = std::time::Instant::now();
    loop {
        let table = ctl(sandbox, &["accounts"])?;
        let own_row = table
            .lines()
            .find(|line| line.contains(origin) && line.contains(OWN_UPN));
        ensure(table.lines().count() == 4 && own_row.is_some(), || {
            format!("the own server's account isn't the third of three:\n{table}")
        })?;
        let connected = own_row.is_some_and(|row| row.contains(" connected "));
        if connected || start.elapsed() > CONNECTS {
            say(&format!("note own-account-connected={connected}"));
            return Ok(connected);
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// The app started again on the first run's files and server: a, b and the own server's account `own` are on the
/// rail, the own one started from its entry as `ngteams` starts it (`ngteamsctl accounts`); switched to by its
/// `<server>/<UPN>`, it is shown, and what it asked went to its server alone.
fn the_accounts_come_back(
    sandbox: &Sandbox,
    kept: &[(&'static str, String)],
    origin: &str,
    own: &str,
) -> SandboxResult {
    let (mut running, mut trace, said) = start_with(sandbox, "restart", kept)?;
    ensure(said.iter().any(|line| line == "came-back entries=3 started=3"), || {
        format!("the accounts kept from the first run: {said:?}")
    })?;
    accounts_shown(&mut trace, &mut running, &[LAB_A, LAB_B, own])?;
    let connected = own_account_connected(sandbox, origin)?;
    ctl(sandbox, &["account", "switch", &format!("{origin}/{OWN_UPN}")])?;
    trace
        .next_where("switched to the own server's account", |line| {
            about(line, "account", "switched", own)
        })
        .map_err(|error| running.fail(&error.to_string()))?;
    let mut input = sandbox.input()?;
    shot(sandbox, "ngteams-accounts-own-restarted.png")?;
    own_tile_pointed_at(sandbox, &mut input, "ngteams-own-rail-tile-restarted.png")?;
    account_menu_opened(
        sandbox,
        &mut input,
        &mut trace,
        THIRD_TILE_AT,
        "ngteams-own-account-menu-restarted.png",
    )?;
    each_accounts_requests_stay_on_its_services(sandbox, &running, false, connected)?;
    quit_and_finish(sandbox, &mut trace, running)
}

/// c posts to a's chat with c while b is shown: a's notification is titled with the chat alone, naming no account,
/// and activating it shows a with the chat open.
fn a_s_message_notifies_and_shows_a(sandbox: &Sandbox, trace: &mut TraceReader, running: &Running) -> SandboxResult {
    let staged = sandbox.path("post-as-c.staged");
    fs::write(&staged, AC_CHAT)?;
    fs::rename(&staged, sandbox.path(POST_TRIGGER))?;
    running
        .line_where(WAIT, |line| line.starts_with("posted "))
        .map_err(|error| format!("c didn't post: {error:?}"))?;
    trace.wait_where("notification shown for c's message", |line| {
        line["view"] == "notification" && line["change"] == "shown" && line["conversation"] == AC_CHAT
    })?;
    let notifications = sandbox.bus().notifications();
    let notification = notifications
        .iter()
        .find(|notification| notification.body == C_SAYS)
        .ok_or_else(|| format!("no notification of c's message: {notifications:?}"))?;
    // the chat's title alone: notifications never name their account
    if notification.summary.contains(" · ") {
        return Err(format!("the notification names an account: {notification:?}").into());
    }
    sandbox.bus().invoke_action(notification.id, "default")?;
    trace.next_where("switched to a", |line| about(line, "account", "switched", LAB_A))?;
    trace.wait_where("its chat opened", |line| {
        line["view"] == "chat" && line["change"] == "opened" && line["conversation"] == AC_CHAT
    })?;
    Ok(())
}

fn ensure(holds: bool, what: impl FnOnce() -> String) -> SandboxResult {
    if holds { Ok(()) } else { Err(what().into()) }
}

/// The first `prompt …` line (the window asking for an account, changed) that `wanted` takes.
fn prompt_line(running: &Running, wait: Duration, what: &str, wanted: impl Fn(&str) -> bool) -> SandboxResult<String> {
    running
        .line_where(wait, |line| line.starts_with("prompt ") && wanted(line))
        .map_err(|error| format!("the window asking for an account: {what}: {error:?}").into())
}

/// What the window asking for an account holds now (`prompt-now …`), after the frames that follow the last input.
fn prompt_now(sandbox: &Sandbox, running: &Running) -> SandboxResult<String> {
    thread::sleep(Duration::from_millis(500));
    fs::write(sandbox.path(own::PROMPT_ASK), "")?;
    running
        .line_where(WAIT, |line| line.starts_with("prompt-now "))
        .map_err(|error| format!("the window asking for an account wasn't said: {error:?}").into())
}

/// Waits until the window asking for an account is the active one: a key pressed before that goes elsewhere.
fn prompt_active(sandbox: &Sandbox, running: &Running) -> SandboxResult<String> {
    let start = std::time::Instant::now();
    loop {
        let now = prompt_now(sandbox, running)?;
        if now.contains(" active=true ") {
            return Ok(now);
        }
        ensure(start.elapsed() < WAIT, || {
            format!("the window asking for an account isn't the active one: {now}")
        })?;
    }
}

/// Empties the field at `at` of `typed` characters, then types `text` into it.
fn retype(input: &mut VirtualInput, at: (u32, u32), typed: usize, text: &str) -> SandboxResult {
    input.click(at.0, at.1, keys::BTN_LEFT)?;
    input.key(keys::END)?;
    for _ in 0..typed {
        input.key(keys::BACKSPACE)?;
    }
    input.type_text(text)
}

/// The rail's "+", then "Own server". A server nothing listens on and a wrong password each say why under the
/// fields (Tab walks address, user name, password; Enter submits); then the right password, typed at once into the
/// password's field, signs the account in on the own server (see the module docs).
fn an_own_servers_account_is_added(
    sandbox: &Sandbox,
    input: &mut VirtualInput,
    trace: &mut TraceReader,
    running: &Running,
    origin: &str,
) -> SandboxResult<String> {
    let dead = fs::read_to_string(sandbox.path(own::DEAD_ORIGIN))?.trim().to_owned();
    input.click(RAIL_ADD_AT.0, RAIL_ADD_AT.1, keys::BTN_LEFT)?;
    let asked = prompt_now(sandbox, running)?;
    ensure(asked.contains("kind=microsoft"), || {
        format!("Microsoft 365 isn't the place chosen first: {asked}")
    })?;
    shot(sandbox, "ngteams-accounts-add-microsoft.png")?;
    // the keyboard on the choice (the place chosen), then the pointer chooses
    input.key_with(&[Modifier::Shift], keys::TAB)?;
    shot(sandbox, "ngteams-accounts-add-choice-focus.png")?;
    input.click(OWN_SEGMENT_AT.0, OWN_SEGMENT_AT.1, keys::BTN_LEFT)?;
    let chosen = prompt_now(sandbox, running)?;
    ensure(chosen.contains("kind=own"), || {
        format!("\"Own server\" wasn't chosen: {chosen}")
    })?;
    shot(sandbox, "ngteams-accounts-add-own.png")?;

    // a server nothing listens on; the fields in Tab's order
    input.type_text(&dead)?;
    input.key(keys::TAB)?;
    input.type_text(OWN_UPN)?;
    input.key(keys::TAB)?;
    input.type_text(own::PASSWORD)?;
    let typed = prompt_now(sandbox, running)?;
    ensure(typed.contains(&format!("server={dead} user={OWN_UPN} ")), || {
        format!("Tab didn't walk address, user name, password: {typed}")
    })?;
    shot(sandbox, "ngteams-accounts-add-own-filled.png")?;
    input.key(keys::ENTER)?;
    let unreachable = prompt_line(running, SIGN_IN, "the unreachable server's reason", |line| {
        line.contains("can't be reached")
    })?;
    // the form has the address to check: no advice about accounts.toml
    ensure(
        unreachable.contains("Check the address")
            && !unreachable.contains("accounts.toml")
            && unreachable.contains("signing=false"),
        || format!("the unreachable server's reason: {unreachable}"),
    )?;
    shot(sandbox, "ngteams-accounts-own-unreachable.png")?;

    // the right server, a wrong password
    retype(input, SERVER_FIELD_AT, dead.len(), origin)?;
    input.key(keys::TAB)?;
    input.key(keys::TAB)?;
    input.type_text("not the password")?;
    input.key(keys::ENTER)?;
    let wrong = prompt_line(running, SIGN_IN, "the wrong password's reason", |line| {
        line.contains("problem=The user name or password isn't right.")
    })?;
    ensure(wrong.contains(&format!("server={origin} user={OWN_UPN} ")), || {
        format!("the fields after it: {wrong}")
    })?;
    shot(sandbox, "ngteams-accounts-own-wrong-password.png")?;

    // the right password, typed at once: the password's field is empty again and still holds the keyboard
    input.type_text(own::PASSWORD)?;
    input.key(keys::ENTER)?;
    own_account_signed_in(sandbox, trace, running)
}

/// The own server's account signed in as the app keeps one (the child's `own-signed-in`), beside the Microsoft 365
/// account of the same user name, and on the rail with its chat list; its MRI.
fn own_account_signed_in(sandbox: &Sandbox, trace: &mut TraceReader, running: &Running) -> SandboxResult<String> {
    let entries = running
        .line_where(SIGN_IN, |line| {
            line.starts_with("own-entries ") || line.starts_with("own account")
        })
        .map_err(|error| format!("the own server's account wasn't signed in: {error:?}"))?;
    ensure(
        entries == "own-entries total=3 microsoft_same_upn=true own_same_upn=1",
        || format!("the same user name on Microsoft 365 and the own server isn't two accounts: {entries}"),
    )?;
    let signed = running
        .line_where(WAIT, |line| line.starts_with("own-signed-in "))
        .map_err(|error| format!("the own server's account wasn't kept: {error:?}"))?;
    ensure(signed.ends_with("chat=true"), || {
        format!("the own server names no chat service: {signed}")
    })?;
    let mri = signed
        .split_whitespace()
        .nth(1)
        .ok_or("the own account's MRI")?
        .to_owned();
    trace.next_where("the own server's account added", |line| {
        about(line, "account", "added", &mri)
    })?;
    trace.next_where("its chat list", |line| {
        line["view"] == "chat-list" && line["change"] == "shown"
    })?;
    shot(sandbox, "ngteams-accounts-own-added.png")?;
    Ok(mri)
}

/// A `name=value` field of a line the child said.
fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|word| word.strip_prefix(name)?.strip_prefix('='))
}

/// Where the requests went: the app's own log has the own account's requests on its server (its sign-in and authz
/// among them when this run signed it in, `signed_in`; its chats' once it is `connected`) and no sign-in or authz
/// request anywhere else, and the mock got no sign-in or authz request and nothing carrying the own account's token.
fn each_accounts_requests_stay_on_its_services(
    sandbox: &Sandbox,
    running: &Running,
    signed_in: bool,
    connected: bool,
) -> SandboxResult {
    fs::write(sandbox.path(own::REQUESTS_ASK), "")?;
    let requests = running
        .line_where(WAIT, |line| line.starts_with("requests "))
        .map_err(|error| format!("the app's requests weren't said: {error:?}"))?;
    say(&requests);
    fs::copy(
        sandbox.path(own::REQUESTS_FILE),
        shots_dir()?.join("ngteams-accounts-requests.txt"),
    )?;
    let count = |name: &str| field(&requests, name).and_then(|value| value.parse::<u32>().ok());
    let own_paths: Vec<&str> = field(&requests, "own_paths").unwrap_or_default().split(',').collect();
    let signed_there = ["/api/authsvc/v1.0/authz", "/oauth2/v2.0/token"]
        .iter()
        .all(|path| own_paths.contains(path));
    let chats_there = own_paths.iter().any(|path| path.contains("/conversations"));
    ensure(
        (signed_there || !signed_in)
            && (chats_there || !connected)
            && count("own") >= Some(1)
            && (count("dead") >= Some(1) || !signed_in)
            && count("elsewhere_auth") == Some(0),
        || format!("the own account's requests: {requests}"),
    )?;
    fs::write(sandbox.path(MOCK_ASK), "")?;
    let mock = running
        .line_where(WAIT, |line| line.starts_with("mock-requests "))
        .map_err(|error| format!("the mock's requests weren't said: {error:?}"))?;
    say(&mock);
    let count = |name: &str| field(&mock, name).and_then(|value| value.parse::<u32>().ok());
    ensure(
        count("total") > Some(0) && count("auth_paths") == Some(0) && count("own_token") == Some(0),
        || format!("the mock's requests: {mock}"),
    )
}

/// Teams Dark: the window asking for an account, the own server's form, and what it says of an empty
/// address and of a user name that isn't one, nothing sent.
fn own_form_in_dark(sandbox: &Sandbox) -> SandboxResult {
    let (mut running, mut trace, _) = start(sandbox, "dark")?;
    accounts_shown(&mut trace, &mut running, &[LAB_A, LAB_B])?;
    shot(sandbox, "ngteams-accounts-a-dark.png")?;
    let mut input = sandbox.input()?;
    input.click(RAIL_ADD_AT.0, RAIL_ADD_AT.1, keys::BTN_LEFT)?;
    let asked = prompt_now(sandbox, &running)?;
    ensure(asked.contains("kind=microsoft"), || format!("the window: {asked}"))?;
    shot(sandbox, "ngteams-accounts-add-microsoft-dark.png")?;
    // the keyboard alone: Shift+Tab from the account's field is on the choice, Right chooses "Own server" with the
    // keyboard still on it, Enter goes on to the server's field
    input.key_with(&[Modifier::Shift], keys::TAB)?;
    shot(sandbox, "ngteams-accounts-add-choice-focus-dark.png")?;
    input.key(keys::RIGHT)?;
    let chosen = prompt_now(sandbox, &running)?;
    ensure(chosen.contains("kind=own"), || {
        format!("the keyboard didn't choose \"Own server\": {chosen}")
    })?;
    shot(sandbox, "ngteams-accounts-add-own-focus-dark.png")?;
    input.key(keys::ENTER)?;
    input.type_text("x")?;
    let typed = prompt_now(sandbox, &running)?;
    ensure(typed.contains("kind=own") && typed.contains(" server=x "), || {
        format!("Enter on the choice didn't go on to the server's field: {typed}")
    })?;
    input.key(keys::BACKSPACE)?;
    shot(sandbox, "ngteams-accounts-add-own-dark.png")?;
    input.key(keys::ENTER)?;
    prompt_line(&running, WAIT, "an empty address", |line| {
        line.contains(&format!("problem={}", teams_ui::account_prompt::NOT_A_SERVER))
    })?;
    shot(sandbox, "ngteams-accounts-add-own-empty-dark.png")?;
    input.type_text("chat.example.test")?;
    input.key(keys::TAB)?;
    input.type_text("someone")?;
    input.key(keys::ENTER)?;
    prompt_line(&running, WAIT, "a user name that isn't one", |line| {
        line.contains(&format!("problem={}", teams_ui::account_prompt::NOT_A_USER))
    })?;
    quit_and_finish(sandbox, &mut trace, running)
}

/// An own server's account signed out: the screen asks its password alone, its server and user name fixed; a wrong
/// password says so; the right one, typed at once, signs it in on the server; then it is forgotten by its
/// `<server>/<UPN>` and the Microsoft 365 account of the same user name stays.
fn password_again(sandbox: &Sandbox, dark: bool) -> SandboxResult {
    let (mode, suffix) = if dark { ("again-dark", "-dark") } else { ("again", "") };
    let (running, _trace, _) = start(sandbox, mode)?;
    let asked = prompt_now(sandbox, &running)?;
    ensure(
        asked.contains("kind=own")
            && asked.contains("server=https://127.0.0.1:")
            && asked.contains(&format!(" user={} ", own::AGAIN_UPN))
            && asked.contains(&format!(
                " asks={} problem=",
                teams_ui::account_prompt::OWN_PASSWORD_AGAIN
            )),
        || format!("the screen doesn't ask the account's password again: {asked}"),
    )?;
    shot(sandbox, &format!("ngteams-own-password-again{suffix}.png"))?;
    let mut input = sandbox.input()?;
    input.type_text("not the password")?;
    input.key(keys::ENTER)?;
    prompt_line(&running, SIGN_IN, "the wrong password's reason", |line| {
        line.contains("problem=The user name or password isn't right.")
    })?;
    shot(sandbox, &format!("ngteams-own-password-again-wrong{suffix}.png"))?;
    input.type_text(own::PASSWORD)?;
    input.key(keys::ENTER)?;
    let signed = running
        .line_where(SIGN_IN, |line| line.starts_with("again-signed-in "))
        .map_err(|error| format!("the password typed again didn't sign in: {error:?}"))?;
    ensure(signed == "again-signed-in ready=true", || {
        format!("signed in again: {signed}")
    })?;
    let forgot = running
        .line_where(WAIT, |line| line.starts_with("forgot "))
        .map_err(|error| format!("the account wasn't forgotten: {error:?}"))?;
    ensure(
        forgot == "forgot own_left=false microsoft_left=true token_left=false",
        || format!("forgotten by <server>/<UPN>: {forgot}"),
    )?;
    let (status, stderr) = running.finish(WAIT)?;
    ensure(status.success(), || format!("the app exited {status}: {stderr}"))
}

/// An own server's entry written by hand, never signed in, without the server's certificate authority: its screen
/// asks its password with no "again"; the sign-in says where the CA file goes; with that line written into the entry
/// while the screen is open, the password typed again on the same screen signs in, with no restart.
fn first_sign_in(sandbox: &Sandbox, dark: bool) -> SandboxResult {
    first_sign_in_shot_as(sandbox, dark, "")
}

/// As [`first_sign_in`], in a sandbox as wide as the window asking for the password: the refusal wraps over several
/// lines and keeps `[[account]]` whole.
fn first_sign_in_at_480(dark: bool) -> SandboxResult {
    let Some(sandbox) = admin::narrow()? else {
        return Ok(());
    };
    first_sign_in_shot_as(&sandbox, dark, "-480")
}

/// [`first_sign_in`], its screenshots' names marked with `width` (empty at the sandbox's own width).
fn first_sign_in_shot_as(sandbox: &Sandbox, dark: bool, width: &str) -> SandboxResult {
    let (mode, suffix) = if dark {
        ("first-dark", format!("{width}-dark"))
    } else {
        ("first", width.to_owned())
    };
    let (running, _trace, _) = start(sandbox, mode)?;
    let asked = prompt_now(sandbox, &running)?;
    ensure(
        asked.contains("kind=own")
            && asked.contains(&format!(" user={} ", own::AGAIN_UPN))
            && asked.contains(&format!(
                " asks={} problem=-",
                teams_ui::account_prompt::OWN_PASSWORD_FIRST
            )),
        || format!("the screen of an account that never signed in: {asked}"),
    )?;
    shot(sandbox, &format!("ngteams-own-first-sign-in{suffix}.png"))?;
    let mut input = sandbox.input()?;
    input.type_text(own::PASSWORD)?;
    input.key(keys::ENTER)?;
    running
        .line_where(SIGN_IN, |line| line == "ca-file-written")
        .map_err(|error| format!("the first sign-in went through without the CA file: {error:?}"))?;
    prompt_line(&running, SIGN_IN, "where the CA file goes", |line| {
        line.contains("certificate isn't trusted") && line.contains(ngteams::signin::CA_FILE_EXAMPLE)
    })?;
    shot(sandbox, &format!("ngteams-own-first-sign-in-ca-missing{suffix}.png"))?;
    input.type_text(own::PASSWORD)?;
    input.key(keys::ENTER)?;
    let signed = running
        .line_where(WAIT, |line| line.starts_with("first-signed-in "))
        .map_err(|error| format!("the same screen didn't sign in once ca_file was written: {error:?}"))?;
    ensure(signed == "first-signed-in ready=true ca_file=true", || {
        format!("signed in on the same screen: {signed}")
    })?;
    let (status, stderr) = running.finish(WAIT)?;
    ensure(status.success(), || format!("the app exited {status}: {stderr}"))
}

/// An own server's account signed in on a server that serves no chats: its sign-in screen says so from the start
/// and stays. With the server gone, a password typed says the server can't be reached: for an entry that names its
/// certificate authority, with that file and no advice to add one; for an entry without, with the line to add.
fn said_reasons(sandbox: &Sandbox, dark: bool) -> SandboxResult {
    let (mode, suffix) = if dark { ("said-dark", "-dark") } else { ("said", "") };
    let (running, _trace, _) = start(sandbox, mode)?;
    let asked = prompt_now(sandbox, &running)?;
    ensure(
        asked.contains("kind=own")
            && asked.contains("signing=false")
            && asked.contains(&format!(" user={} ", own::AGAIN_UPN))
            && asked.contains(&format!("problem={} is signed in, but 127.0.0.1:", own::AGAIN_UPN))
            && asked.ends_with("doesn't serve chats, so it can't be shown yet."),
        || format!("the screen doesn't say the server serves no chats: {asked}"),
    )?;
    shot(sandbox, &format!("ngteams-own-no-chat-service-screen{suffix}.png"))?;
    let example = ngteams::signin::CA_FILE_EXAMPLE;
    let mut input = sandbox.input()?;
    input.type_text(own::PASSWORD)?;
    input.key(keys::ENTER)?;
    let named = prompt_line(&running, SIGN_IN, "the unreachable server, its CA file named", |line| {
        line.contains("can't be reached")
    })?;
    ensure(
        named.contains("ca.pem is its certificate authority") && !named.contains(example),
        || format!("an entry with a ca_file is told to add one: {named}"),
    )?;
    shot(sandbox, &format!("ngteams-own-unreachable-with-ca{suffix}.png"))?;
    input.type_text(own::PASSWORD)?;
    input.key(keys::ENTER)?;
    prompt_line(&running, SIGN_IN, "the unreachable server, the line to add", |line| {
        line.contains("can't be reached") && line.contains(example)
    })?;
    shot(sandbox, &format!("ngteams-own-unreachable-without-ca{suffix}.png"))?;
    input.type_text(own::PASSWORD)?;
    input.key(keys::ENTER)?;
    let (status, stderr) = running.finish(WAIT)?;
    ensure(status.success(), || format!("the app exited {status}: {stderr}"))
}

/// What `key` (named `named`) leaves in the window asking for an account: it must hold `wanted`.
fn key_leaves(
    sandbox: &Sandbox,
    input: &mut VirtualInput,
    running: &Running,
    (named, key): (&str, u32),
    wanted: &str,
) -> SandboxResult {
    input.key(key)?;
    let now = prompt_now(sandbox, running)?;
    ensure(now.contains(wanted), || {
        format!("after {named} the window doesn't hold `{wanted}`: {now}")
    })
}

/// The choice of place with the keyboard alone, as a user without a pointer works it (see the module docs), then
/// with the pointer.
fn choice_by_keyboard(sandbox: &Sandbox, dark: bool) -> SandboxResult {
    const OWN: &str = "kind=own ";
    const MICROSOFT: &str = "kind=microsoft ";
    let (mode, suffix) = if dark { ("dark", "-dark") } else { ("keys", "") };
    let (mut running, mut trace, _) = start(sandbox, mode)?;
    accounts_shown(&mut trace, &mut running, &[LAB_A, LAB_B])?;
    let mut input = sandbox.input()?;
    input.click(RAIL_ADD_AT.0, RAIL_ADD_AT.1, keys::BTN_LEFT)?;
    let asked = prompt_active(sandbox, &running)?;
    ensure(asked.contains(MICROSOFT), || format!("the window: {asked}"))?;

    // Tab from the account's field: "Sign in", then round to the choice
    input.key(keys::TAB)?;
    input.key(keys::TAB)?;
    shot(sandbox, &format!("ngteams-accounts-keys-choice{suffix}.png"))?;
    let mut press =
        |named: &str, key: u32, wanted: &str| key_leaves(sandbox, &mut input, &running, (named, key), wanted);
    press("Tab, Tab, Right", keys::RIGHT, OWN)?;
    shot(sandbox, &format!("ngteams-accounts-keys-own{suffix}.png"))?;
    press("Left", keys::LEFT, MICROSOFT)?;
    press("End", keys::END, OWN)?;
    press("Home", keys::HOME, MICROSOFT)?;
    press("Down", keys::DOWN, OWN)?;
    press("Up", keys::UP, MICROSOFT)?;
    press("Left on the first place", keys::LEFT, OWN)?;
    press("Right on the last place", keys::RIGHT, MICROSOFT)?;
    press("Right", keys::RIGHT, OWN)?;

    // a letter on the choice is nobody's; Space goes on to the server's field
    input.type_text("z")?;
    let untouched = prompt_now(sandbox, &running)?;
    ensure(
        untouched.contains("kind=own signing=false server= user= account= "),
        || format!("a letter typed on the choice went somewhere: {untouched}"),
    )?;
    input.key(keys::SPACE)?;
    input.type_text("x")?;
    let typed = prompt_now(sandbox, &running)?;
    ensure(
        typed.contains("kind=own ") && typed.contains(" server=x user= "),
        || format!("Space on the choice didn't go on to the server's field: {typed}"),
    )?;
    shot(sandbox, &format!("ngteams-accounts-keys-own-field{suffix}.png"))?;

    // back on the choice: Tab walks the server's address, the user name, the password, the certificate authority's
    // file, "Sign in", and round
    input.key_with(&[Modifier::Shift], keys::TAB)?;
    input.key(keys::TAB)?;
    input.type_text("1")?;
    input.key(keys::TAB)?;
    input.type_text("2")?;
    // the password, the certificate authority's file, "Sign in", and round
    input.key(keys::TAB)?;
    input.key(keys::TAB)?;
    input.key(keys::TAB)?;
    input.key(keys::TAB)?;
    let walked = prompt_now(sandbox, &running)?;
    ensure(
        walked.contains("kind=own ") && walked.contains(" server=x1 user=2 "),
        || format!("Tab didn't walk the choice, the address and the user name: {walked}"),
    )?;
    let mut press =
        |named: &str, key: u32, wanted: &str| key_leaves(sandbox, &mut input, &running, (named, key), wanted);
    press("Tab round to the choice, Left", keys::LEFT, MICROSOFT)?;

    // Enter goes on to the account's field
    input.key(keys::ENTER)?;
    input.type_text("y")?;
    let typed = prompt_now(sandbox, &running)?;
    ensure(typed.contains(MICROSOFT) && typed.contains(" account=y "), || {
        format!("Enter on the choice didn't go on to the account's field: {typed}")
    })?;
    ensure(typed.contains("signing=false") && typed.contains("problem=-"), || {
        format!("the keys on the choice started a sign-in or left a problem: {typed}")
    })?;

    // the pointer chooses each place
    input.click(OWN_SEGMENT_AT.0, OWN_SEGMENT_AT.1, keys::BTN_LEFT)?;
    let clicked = prompt_now(sandbox, &running)?;
    ensure(clicked.contains(OWN), || {
        format!("the pointer didn't choose \"Own server\": {clicked}")
    })?;
    input.click(MICROSOFT_SEGMENT_AT.0, MICROSOFT_SEGMENT_AT.1, keys::BTN_LEFT)?;
    let clicked = prompt_now(sandbox, &running)?;
    ensure(clicked.contains(MICROSOFT), || {
        format!("the pointer didn't choose \"Microsoft 365\": {clicked}")
    })?;
    shot(sandbox, &format!("ngteams-accounts-keys-pointer{suffix}.png"))?;
    quit_and_finish(sandbox, &mut trace, running)
}

/// `ngteamsctl <args>` against the app's socket, in-process: its exit, stdout and stderr.
fn ctl(sandbox: &Sandbox, args: &[&str]) -> SandboxResult<String> {
    let socket = sandbox.path(CTL_SOCKET).to_string_lossy().into_owned();
    let line = ["ngteamsctl", "--socket", socket.as_str()]
        .into_iter()
        .chain(args.iter().copied());
    let parsed = Cli::try_parse_from(line)?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = cli::run(&parsed, &teams_ctl::Env::default(), &mut out, &mut err);
    let (out, err) = (String::from_utf8(out)?, String::from_utf8(err)?);
    ensure(exit == Exit::Ok, || {
        format!("ngteamsctl {args:?} exited {exit:?}: {out}{err}")
    })?;
    Ok(out)
}

/// An own server's account as the window shows one, beside a (the same user name, on Microsoft 365) and b:
/// `ngteamsctl accounts` names its server in a SERVER column, `<server>/<UPN>` switches to it and the bare user name
/// to the Microsoft 365 one, and Settings > General names the server for it alone.
fn shown(sandbox: &Sandbox, dark: bool) -> SandboxResult {
    let (mode, suffix) = if dark { ("shown-dark", "-dark") } else { ("shown", "") };
    let (mut running, mut trace, _) = start(sandbox, mode)?;
    accounts_shown(&mut trace, &mut running, &[LAB_A, LAB_B, LAB_C])?;
    let table = ctl(sandbox, &["accounts"])?;
    fs::write(shots_dir()?.join("ngteams-accounts-ctl-accounts.txt"), &table)?;
    let own_rows: Vec<&str> = table.lines().filter(|line| line.contains(SHOWN_SERVER)).collect();
    ensure(
        table.lines().next().is_some_and(|header| header.contains("SERVER"))
            && own_rows.len() == 1
            && own_rows[0].contains(OWN_UPN),
        || format!("ngteamsctl accounts:\n{table}"),
    )?;
    let listed: serde_json::Value = serde_json::from_str(ctl(sandbox, &["--json", "accounts"])?.trim())?;
    let servers: Vec<Option<&str>> = listed["ok"]["accounts"]
        .as_array()
        .ok_or_else(|| format!("ngteamsctl --json accounts: {listed}"))?
        .iter()
        .map(|account| account["server"].as_str())
        .collect();
    ensure(servers == [None, None, Some(SHOWN_SERVER)], || {
        format!("the accounts' servers: {listed}")
    })?;

    ctl(sandbox, &["account", "switch", &format!("{SHOWN_SERVER}/{OWN_UPN}")])?;
    trace.next_where("switched to the own server's account", |line| {
        about(line, "account", "switched", LAB_C)
    })?;
    ctl(sandbox, &["open", "settings", "general"])?;
    shot(sandbox, &format!("ngteams-own-settings-general{suffix}.png"))?;
    let mut input = sandbox.input()?;
    own_tile_pointed_at(sandbox, &mut input, &format!("ngteams-own-rail-tile{suffix}.png"))?;
    account_menu_opened(
        sandbox,
        &mut input,
        &mut trace,
        THIRD_TILE_AT,
        &format!("ngteams-own-account-menu{suffix}.png"),
    )?;

    // the bare user name is the Microsoft 365 account's
    ctl(sandbox, &["account", "switch", OWN_UPN])?;
    trace.next_where("switched to the Microsoft 365 account", |line| {
        about(line, "account", "switched", LAB_A)
    })?;
    ctl(sandbox, &["open", "settings", "general"])?;
    shot(sandbox, &format!("ngteams-microsoft-settings-general{suffix}.png"))?;
    // the Microsoft 365 account of the same user name: its tile and its account menu name no server
    tile_pointed_at(
        sandbox,
        &mut input,
        FIRST_TILE_AT,
        &format!("ngteams-microsoft-rail-tile{suffix}.png"),
    )?;
    account_menu_opened(
        sandbox,
        &mut input,
        &mut trace,
        FIRST_TILE_AT,
        &format!("ngteams-microsoft-account-menu{suffix}.png"),
    )?;
    ensure(!ctl(sandbox, &["status"])?.is_empty(), || {
        "ngteamsctl status said nothing".to_owned()
    })?;
    quit_and_finish(sandbox, &mut trace, running)
}

/// The tray menu's "Quit".
fn quit(sandbox: &Sandbox, trace: &mut TraceReader, running: &Running) -> SandboxResult {
    let name = format!("org.kde.StatusNotifierItem-{}-1", running.pid());
    let host = sandbox.bus().connect()?;
    let item = zbus::blocking::Proxy::new(&host, name.as_str(), ITEM_PATH, "org.kde.StatusNotifierItem")?;
    let title: String = item.get_property("Title")?;
    if !title.starts_with(app::TITLE) {
        return Err(format!("the item's title: {title}").into());
    }
    let menu = zbus::blocking::Proxy::new(&host, name.as_str(), MENU_PATH, "com.canonical.dbusmenu")?;
    menu.call_method("Event", &(MENU_QUIT, "clicked", zbus::zvariant::Value::from(""), 0_u32))?;
    trace.next_where("tray quit", |line| line["view"] == "tray" && line["change"] == "quit")?;
    Ok(())
}

// --- the child: the app of a and b on the mock ---

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

/// The mock with a's and b's engines on it, each as `teams_core::account::start` builds one but for the mock origins, a
/// their own stores; their push channels on the mock's Trouter.
struct MockLab {
    _runtime: tokio::runtime::Runtime,
    mock: Arc<ChatServiceMock>,
    trouter: TrouterMock,
    /// Where the stores and the app's files are: the parent's directory ([`ROOT_ENV`]), else one of its own.
    root: PathBuf,
    _own_root: Option<tempfile::TempDir>,
    /// The accounts' clients read Loki (the profile card scenes).
    loki: bool,
}

impl MockLab {
    /// The mock, its accounts' clients reading Loki and the mock serving it when `loki` (the profile card scenes).
    fn start(loki: bool) -> SandboxResult<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        let (root, own_root) = if let Some(root) = std::env::var_os(ROOT_ENV) {
            (PathBuf::from(root), None)
        } else {
            let dir = tempfile::tempdir()?;
            (dir.path().to_owned(), Some(dir))
        };
        if loki {
            card_qa::serve_mock_loki(&mock, &runtime);
        }
        Ok(Self {
            _runtime: runtime,
            mock: Arc::new(mock),
            trouter,
            root,
            _own_root: own_root,
            loki,
        })
    }

    /// `alias`'s engine (its MRI `mri`) on the mock.
    fn engine(&self, alias: Alias, mri: &str, upn: &str) -> SandboxResult<EngineHandle> {
        self.engine_at(&support::store_file(&self.root, alias), mri, upn)
    }

    /// The store of the third account the `shown` scenes add.
    fn third_store(&self) -> PathBuf {
        self.root.join("store").join("third.sqlite")
    }

    /// The engine of the account `mri` on the mock, its store at `store`.
    fn engine_at(&self, store: &Path, mri: &str, upn: &str) -> SandboxResult<EngineHandle> {
        let tokens = Arc::new(Tokens {
            token: self.mock.token(mri).ok_or("the mock's token")?,
            renewals: broadcast::channel(4).0,
        });

        let origin = self.mock.origin();
        let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
        let transport = Arc::new(http()?);

        let trouter = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), self.trouter.origin())?
            .with_options(Options {
                backoff_base: Duration::from_millis(50),
                backoff_cap: Duration::from_millis(50),
                backoff_jitter_percent: 0,
                ..Options::default()
            });
        let author = Author::new(Mri::new(mri), display_name(upn));
        Ok(engine::start(Parts {
            calls: None,
            store: Store::open(store)?,
            client: Arc::new(Client::new(if self.loki { http()?.with_loki() } else { http()? })),
            outbox: Outbox::new(transport, author),
            push: Some(Push::new(trouter)),
            options: EngineOptions::default(),
            known_names: Vec::new(),
        })?)
    }

    /// Whether the accounts' engine locks are free (c's too when it ran, `third`): their engines' threads have ended.
    fn engines_released(&self, third: bool) -> SandboxResult<bool> {
        if third && Store::open(&self.third_store())?.engine_lock().is_err() {
            return Ok(false);
        }
        for alias in [Alias::A, Alias::B] {
            if Store::open(&support::store_file(&self.root, alias))?
                .engine_lock()
                .is_err()
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// How the app's child runs (the scenes of [`SCENES`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// An own server running for the account the window adds, the app's requests on record.
    Window,
    /// In Teams Dark.
    Dark,
    /// In Teams Light, no own server running.
    Keys,
    /// A third account that is on an own server shown beside a and b, the control socket served.
    Shown,
    /// [`Mode::Shown`] in Teams Dark.
    ShownDark,
    /// The app again on [`Mode::Window`]'s files and server: the own server's account started from its entry.
    Restart,
    /// As [`Mode::Window`] on a server without any account: nothing of it is written beforehand.
    Admin,
    /// Teams Dark, the parent's own server's administrator signed in and shown beside a and b.
    AdminDark,
    /// [`Mode::Admin`] with a request log of its own (the QA scenes).
    AdminQa,
    /// [`Mode::AdminDark`] in Teams Light.
    AdminLight,
    /// [`Mode::AdminLight`] with the parent's server's user [`admin_qa::RUNNING`] signed in beside its administrator.
    AdminTwo,
    /// The parent's server's seeded account signed in beside a and b, its colleagues' chats written
    /// beforehand, the mock serving Loki (the profile card QA scenes).
    Card,
    /// [`Mode::Card`] in Teams Dark.
    CardDark,
    /// The parent's server's seeded account signed in beside a and b, its team, channel and meeting
    /// made beforehand, the parent's steps answered (the hidden services QA scenes).
    Hidden,
    /// [`Mode::Hidden`] in Teams Dark.
    HiddenDark,
}

impl Mode {
    fn named(name: &str) -> Option<Self> {
        match name {
            "window" => Some(Self::Window),
            "dark" => Some(Self::Dark),
            "keys" => Some(Self::Keys),
            "shown" => Some(Self::Shown),
            "shown-dark" => Some(Self::ShownDark),
            "restart" => Some(Self::Restart),
            "admin" => Some(Self::Admin),
            "admin-dark" => Some(Self::AdminDark),
            "admin-qa" => Some(Self::AdminQa),
            "admin-light" => Some(Self::AdminLight),
            "admin-two" => Some(Self::AdminTwo),
            "card" => Some(Self::Card),
            "card-dark" => Some(Self::CardDark),
            "hidden" => Some(Self::Hidden),
            "hidden-dark" => Some(Self::HiddenDark),
            _ => None,
        }
    }

    /// The name of the mode's own request log: another scene's requests went to another server.
    fn request_log(self) -> &'static str {
        match self {
            Self::Restart => "restart",
            Self::Admin => "admin",
            Self::AdminDark => "admin-dark",
            Self::AdminQa => "admin-qa",
            Self::AdminLight => "admin-light",
            Self::AdminTwo => "admin-two",
            Self::Card => "card",
            Self::CardDark => "card-dark",
            Self::Hidden => "hidden",
            Self::HiddenDark => "hidden-dark",
            _ => "first",
        }
    }

    /// Teams Dark, whatever the system says.
    fn dark(self) -> bool {
        matches!(
            self,
            Self::Dark | Self::ShownDark | Self::AdminDark | Self::CardDark | Self::HiddenDark
        )
    }

    /// The profile card QA scenes.
    fn card(self) -> bool {
        matches!(self, Self::Card | Self::CardDark)
    }

    /// The hidden services QA scenes.
    fn hidden(self) -> bool {
        matches!(self, Self::Hidden | Self::HiddenDark)
    }

    fn shown(self) -> bool {
        matches!(self, Self::Shown | Self::ShownDark)
    }

    /// The control socket is served.
    fn controlled(self) -> bool {
        self.shown() || self.on_own_server()
    }

    /// The parent's own server is the one the window's account is on.
    fn on_own_server(self) -> bool {
        matches!(
            self,
            Self::Window
                | Self::Restart
                | Self::Admin
                | Self::AdminDark
                | Self::AdminQa
                | Self::AdminLight
                | Self::AdminTwo
                | Self::Card
                | Self::CardDark
                | Self::Hidden
                | Self::HiddenDark
        )
    }
}

/// A mock account started as [`support::started`] starts it, its session (the sign-out watch's alone) over a store of
/// the app's kind, so it can stand beside an account the app's runner started.
fn started(alias: Alias, engine: EngineHandle) -> SandboxResult<Started> {
    let started = support::started(alias, engine)?;
    let session = teams_auth::session::Session::new(
        started.account.upn.clone(),
        AppSecrets::in_memory(),
        teams_auth::token::TokenClient::new(
            teams_auth::token::EntraConfig::teams_native().with_authority("http://127.0.0.1:9/"),
        )?,
        teams_auth::authz::AuthzClient::with_endpoint("http://127.0.0.1:9/authz")?,
        teams_auth::authz::Region::default(),
    );
    Ok(Started {
        account: started.account,
        engine: teams_core::account::AccountEngine {
            handle: started.engine.handle,
            session: Arc::new(session),
        },
        calls: None,
    })
}

/// c as the window shows an own server's account: a's user name, on [`SHOWN_SERVER`] (its engine on the mock: the
/// server serves no chats yet, and nothing is sent to that address).
fn shown_account(lab: &MockLab) -> SandboxResult<Started> {
    let engine = lab.engine_at(&lab.third_store(), LAB_C, "bot-c@lab.example.test")?;
    // the session is only the sign-out watch's, as for a and b
    let mut c = started(Alias::B, engine)?;
    c.account = ngteams::accounts_file::PersonalAccount::new(
        OWN_UPN,
        LAB_C.strip_prefix("8:orgid:").ok_or("mock orgid")?,
        "00000000-0000-4000-8000-000000000001",
    )?;
    c.account.name = Some(OWN_NAME.to_owned());
    c.account.server = Some(SHOWN_SERVER.to_owned());
    if c.account.server_host().as_deref() != Some(SHOWN_HOST) {
        return Err("the shown account's server".into());
    }
    Ok(c)
}

fn child_window(mode: Mode) -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let fd: RawFd = std::env::var(TRACE_FD_ENV)?.parse()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(fd)?);
    let lab = MockLab::start(mode.card())?;
    let a = started(Alias::A, lab.engine(Alias::A, LAB_A, "bot-a@lab.example.test")?)?;
    let b = started(Alias::B, lab.engine(Alias::B, LAB_B, "bot-b@lab.example.test")?)?;
    let mut others = vec![b];
    if mode.shown() {
        others.push(shown_account(&lab)?);
    }
    let settings = teams_ui::SettingsFile::new(lab.root.join("ui").join("a.toml"));
    if mode.dark() {
        fs::create_dir_all(lab.root.join("ui"))?;
        fs::write(lab.root.join("ui").join("a.toml"), own::DARK_SETTINGS)?;
    }
    let server = if mode.on_own_server() {
        own::record_requests(mode.request_log())?;
        Some(ngteams::accounts_file::OwnServer::parse(
            &std::env::var(OWN_ORIGIN_ENV)?,
            &std::env::var(OWN_CA_ENV)?,
        )?)
    } else {
        None
    };
    let mut held = vec![a.account.clone()];
    held.extend(others.iter().map(|started| started.account.clone()));
    let runner = runner_of(&lab.root, mode, &held, server.as_ref())?;
    others.extend(own_started(mode, &runner, server.as_ref())?);
    let watched = runner.setup.clone();
    let origins = match &server {
        Some(_) => Some((std::env::var(OWN_ORIGIN_ENV)?, dead_origin()?)),
        None => None,
    };
    let ctl = if mode.controlled() {
        Some(own::sandbox_dir()?.join(CTL_SOCKET))
    } else {
        None
    };
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let mock = Arc::clone(&lab.mock);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first: a,
                others,
                trace: Arc::new(trace),
                ctl,
                settings: teams_ui::SettingsModel::load(settings),
                // the sandbox's private bus, with its fake notification server and tray watcher
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(sign_in_again_saying),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(launched) => {
                if mode.hidden() {
                    hidden_qa::answer(launched.window, cx);
                }
                tell_the_parent(mock, watched, origins, mode.card().then_some(launched.window), cx);
                let _ = launched.window.update(cx, |_, window, _| {
                    after_frames(window, READY_FRAMES, |_, _| say("ready"));
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
    if lab.engines_released(mode.shown())? {
        say("engines-released");
    }
    Ok(())
}

/// The own server's accounts a mode starts with: the first run's come back ([`Mode::Restart`]), or the server's
/// administrator signed in beforehand ([`Mode::AdminDark`], [`Mode::AdminLight`]; [`Mode::AdminTwo`] with its user
/// beside it).
fn own_started(
    mode: Mode,
    runner: &ngteams::accounts::Runner,
    server: Option<&ngteams::accounts_file::OwnServer>,
) -> SandboxResult<Vec<Started>> {
    match (mode, server) {
        (Mode::Restart, _) => Ok(vec![own::come_back(runner)?]),
        (Mode::AdminDark | Mode::AdminLight, Some(server)) => Ok(vec![admin::signed_in(runner, server)?]),
        (Mode::Card | Mode::CardDark, Some(server)) => card_qa::started(runner, server),
        (Mode::Hidden | Mode::HiddenDark, Some(server)) => hidden_qa::started(runner, server),
        (Mode::AdminTwo, Some(server)) => {
            let running =
                ngteams::accounts::add_own(&runner.setup, admin_qa::RUNNING, server, &Secret::new(own::PASSWORD))?;
            Ok(vec![
                admin::signed_in(runner, server)?,
                runner.start(&running).map_err(String::from)?,
            ])
        }
        _ => Ok(Vec::new()),
    }
}

/// What the child does and says for the parent while the app runs: c's post when asked, the window asking for an
/// account, and with an own server running (`origins`: its origin and the one nothing listens on) the account signed
/// in and where the requests went; in the profile card scenes (`card`: the window), the card's answers.
fn tell_the_parent(
    mock: Arc<ChatServiceMock>,
    setup: ngteams::setup::Setup,
    origins: Option<(String, String)>,
    card: Option<gpui::WindowHandle<teams_ui::window::KitRoot>>,
    cx: &mut App,
) {
    post_when_asked(Arc::clone(&mock), cx);
    own::say_prompt(cx);
    if let Some((origin, dead)) = origins {
        if let Some(window) = card {
            card_qa::answer(window, Arc::clone(&mock), setup.clone(), origin.clone(), cx);
        }
        say_when_own_signed_in(setup.clone(), cx);
        write_stopped_engines(cx);
        own::hand_over_secrets_when_asked(setup.clone(), cx);
        own::say_requests_when_asked(origin, dead, cx);
        say_mock_requests_when_asked(Arc::clone(&mock), setup.clone(), cx);
        admin_qa::say_mock_needles_when_asked(mock, setup, cx);
    }
}

/// Adds the MRI of each account whose engine has stopped (its thread joined) to [`ENGINE_STOPPED_FILE`], one time.
fn write_stopped_engines(cx: &mut App) {
    let Ok(dir) = own::sandbox_dir() else {
        say("no sandbox dir");
        return;
    };
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut written = std::collections::HashSet::new();
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let stopped: Vec<String> = cx.update(|cx| {
                app::bridges(cx)
                    .iter()
                    .map(|bridge| bridge.read(cx))
                    .filter(|bridge| !bridge.running())
                    .map(|bridge| bridge.core().me().to_owned())
                    .collect()
            });
            for mri in stopped {
                if !written.insert(mri.clone()) {
                    continue;
                }
                let file = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join(ENGINE_STOPPED_FILE));
                if let Ok(mut file) = file {
                    let _ = writeln!(file, "{mri}");
                }
            }
        }
    })
    .detach();
}

/// An origin nothing listens on, written to the sandbox dir for the parent to type.
fn dead_origin() -> SandboxResult<String> {
    let dead = format!("https://127.0.0.1:{}", own_server_binary::free_port()?);
    fs::write(own::sandbox_dir()?.join(own::DEAD_ORIGIN), &dead)?;
    Ok(dead)
}

/// The app's runner with a credential store of this process alone (the sandbox has no Secret Service). The accounts the window starts with (`held`) are in `accounts.toml` as accounts signed in
/// before. With an own server running, its certificate authority is written into the account's entry beforehand, as
/// a user of a self-signed deployment writes it; started again ([`Mode::Restart`]), the entry is the first run's.
fn runner_of(
    root: &Path,
    mode: Mode,
    held: &[ngteams::accounts_file::PersonalAccount],
    server: Option<&ngteams::accounts_file::OwnServer>,
) -> SandboxResult<ngteams::accounts::Runner> {
    let setup = support::runner(root)?.setup.with_secrets(AppSecrets::in_memory());
    for account in held.iter().filter(|account| account.server.is_none()) {
        account.save(&setup.paths)?;
    }
    if let Some(server) =
        server.filter(|_| matches!(mode, Mode::Window | Mode::AdminDark | Mode::AdminLight | Mode::AdminTwo))
    {
        setup.ask_for_on(OWN_UPN, server)?;
    }
    Ok(ngteams::accounts::Runner {
        setup,
        floor: teams_core::CallFloor::new(),
        devices: None,
    })
}

/// Says `own-entries total=<n> microsoft_same_upn=<bool> own_same_upn=<n>` and `own-signed-in <mri> chat=<bool>` once
/// the own server's account is signed in as the app keeps one: its entry named by the sign-in beside the Microsoft
/// 365 account of the same user name, its refresh token under its server and UPN, its region's bases on its server
/// alone; `chat` is whether the server's region names a chat service (so its engine can start).
fn say_when_own_signed_in(setup: ngteams::setup::Setup, cx: &mut App) {
    use teams_auth::secrets::SecretStore as _;
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(200)).await;
            let Ok(accounts) = setup.signed_in_accounts() else {
                continue;
            };
            let Some(account) = accounts
                .iter()
                .find(|account| account.upn == OWN_UPN && account.server.is_some())
                .cloned()
            else {
                continue;
            };
            let server = account.server.clone().unwrap_or_default();
            let region = setup
                .paths
                .region_file(&account)
                .and_then(|path| teams_auth::authz::Region::load(&path).ok());
            let Some(region) = region else {
                continue;
            };
            let key = teams_auth::refresh::refresh_token_key(&account.id());
            let bare = teams_auth::refresh::refresh_token_key(&account.upn);
            let secrets = setup.secrets();
            let stored = secrets.get(&key).await.ok().flatten().is_some();
            let under_upn = secrets.get(&bare).await.ok().flatten().is_some();
            let own_bases = region.base("authService").is_some_and(|base| base.starts_with(&server));
            if !stored || under_upn || !own_bases {
                say(&format!(
                    "own account kept wrongly: stored={stored} under_upn={under_upn} own_bases={own_bases}"
                ));
                return;
            }
            let all = setup.accounts().unwrap_or_default();
            let same = |own: bool| {
                all.iter()
                    .filter(|held| held.upn == OWN_UPN && held.server.is_some() == own && held.is_signed_in())
                    .count()
            };
            say(&format!(
                "own-entries total={} microsoft_same_upn={} own_same_upn={}",
                all.len(),
                same(false) == 1,
                same(true)
            ));
            let chat = region.base("chatServiceAfd").is_some() || region.base("chatService").is_some();
            let mri = account.mri().map(|mri| mri.as_str().to_owned()).unwrap_or_default();
            say(&format!("own-signed-in {mri} chat={chat}"));
            return;
        }
    })
    .detach();
}

/// Once the parent writes [`MOCK_ASK`]: `mock-requests total=<n> auth_paths=<n> own_token=<n>` of the requests the
/// mock (Microsoft's services here) got: how many asked a sign-in or authz service, and how many carried the own
/// server's account's skypetoken.
fn say_mock_requests_when_asked(mock: Arc<ChatServiceMock>, setup: ngteams::setup::Setup, cx: &mut App) {
    use teams_auth::secrets::SecretStore as _;
    let Ok(dir) = own::sandbox_dir() else {
        say("no sandbox dir");
        return;
    };
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            if fs::remove_file(dir.join(MOCK_ASK)).is_err() {
                continue;
            }
            let own = setup
                .signed_in_accounts()
                .unwrap_or_default()
                .into_iter()
                .find(|account| account.server.is_some());
            let token = match own {
                Some(account) => setup
                    .secrets()
                    .get(&teams_auth::authz::skypetoken_key(&account.id()))
                    .await
                    .ok()
                    .flatten(),
                None => None,
            };
            let Some(token) = token else {
                say("mock-requests: the own account holds no skypetoken to look for");
                continue;
            };
            let requests = mock.received_requests().await;
            let auth_paths = requests
                .iter()
                .filter(|request| {
                    let path = request.url.path();
                    path.contains("/oauth2/") || path.contains("/authsvc/")
                })
                .count();
            let own_token = requests
                .iter()
                .filter(|request| {
                    request
                        .headers
                        .values()
                        .any(|value| value.to_str().is_ok_and(|value| value.contains(token.expose())))
                })
                .count();
            say(&format!(
                "mock-requests total={} auth_paths={auth_paths} own_token={own_token}",
                requests.len()
            ));
        }
    })
    .detach();
}

/// Once the parent writes a chat's id to [`POST_TRIGGER`] in the sandbox dir, c posts [`C_SAYS`] there on the mock,
/// which delivers it to the members' push channels as any post of c's.
fn post_when_asked(mock: Arc<ChatServiceMock>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let trigger = PathBuf::from(dir).join(POST_TRIGGER);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(chat) = fs::read_to_string(&trigger) else {
                continue;
            };
            let _ = fs::remove_file(&trigger);
            match mock.post_as(LAB_C, chat.trim(), &format!("<p>{C_SAYS}</p>")) {
                Some(id) => say(&format!("posted {id}")),
                None => say("post refused"),
            }
            return;
        }
    })
    .detach();
}
