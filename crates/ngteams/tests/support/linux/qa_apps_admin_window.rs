//! Manage apps (`NGTEAMS_BOTS=1`) as a real administrator uses it, in the UI sandbox (`harness = false`: GPUI runs on
//! the process's main thread). It is the QA test of task 232.
//!
//! The parent runs the real `ngteams-server` binary (`NGTEAMS_QA_SERVER_BIN`, else the one `cargo build -p
//! ngteams-server` left in the target directory) as an operator runs it, adds made-up users with `user add`, and runs
//! the app as a child in the sandbox, signed in to that server with the password grant. The parent types with the
//! sandbox's keyboard where the keyboard is the point, asks the child for the rest, and checks each change against
//! the server: the server's own list, its command line, and its token endpoint (a bot's secret works, or does not).
//!
//! - **The administrator** (`bots.allow_unsigned` off): register a bot from the keyboard (its secret once), the
//!   secret gets a token; refusals (unsigned, a plain `http` endpoint, credentials in the endpoint, a long name);
//!   upload a good package, one too large, a broken one, one without a manifest, the same one again; turn an app off
//!   (no token) and on (a token); change the endpoint (good; private, metadata, loopback, `http`, unsigned refused);
//!   limit it to selected people; a new secret (the old one stops); delete it after a confirmation that starts on Back;
//!   many apps with long names and no picture, and the search. Afterwards no secret is in the app's files, its debug
//!   log, its stderr or the server's log.
//! - **A user who is no administrator**: Settings offer neither Manage users nor Manage apps.
//! - **A server with `apps.enabled = false`**: its administrator gets Manage users and no Manage apps.
//! - **A server with `bots.allow_unsigned`**: an unsigned bot is registered, marked and gets no token.
//! - **A bot's delivery and its test activity** (task 524, a server whose `bots.allowed_networks` opens loopback): a
//!   new bot's details say nothing was sent yet; "Send a test activity" reaches a loopback bot that takes it, one
//!   that answers 500, and an endpoint where nothing listens, and the details say how each went; the details of an
//!   app that is off and of an app without an endpoint show no test (the shots show the button off).
//!
//! Without the server binary, weston 15, niri or `dbus-daemon` it fails: this test is run on purpose. Every user, app
//! and value is made up; nothing leaves loopback. Shots go to `target/ui-shots/qa-apps-admin-*.png`, each in Teams
//! Light and Dark.

use std::io::{Cursor, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use std::{fs, io, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::accounts::{Runner, Started};
use ngteams::accounts_file::{OwnServer as OwnAddress, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::secrets::AppSecrets;
use ngteams::setup::Setup;
use serde_json::{Value, json};
use teams_auth::admin::BotAuthMode;
use teams_auth::secrets::Secret;
use teams_core::CallFloor;
use teams_ui::app_admin::sdk::Sdk;
use teams_ui::app_admin::{AppDialog, DialogKind, ManageApps, shown_secret};
use teams_ui::settings_view::Page;
use teams_ui::shell::Shell;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "../own_server_binary.rs"]
#[expect(
    dead_code,
    reason = "the own server's tests share this support; this one uses a part of it"
)]
mod own_server_binary;
use own_server_binary::{BinaryServer, PASSWORD};

const CHILD_ENV: &str = "NGTEAMS_QA_APPS_ADMIN_TEST";
const ORIGIN_ENV: &str = "NGTEAMS_QA_APPS_ADMIN_ORIGIN";
const CA_ENV: &str = "NGTEAMS_QA_APPS_ADMIN_CA";
const DIR_ENV: &str = "NGTEAMS_QA_APPS_ADMIN_DIR";
const UPN_ENV: &str = "NGTEAMS_QA_APPS_ADMIN_UPN";
const ASK: &str = "qa-apps-admin-ask";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(30);
const READY_FRAMES: usize = 3;
const SETTLE: Duration = Duration::from_millis(900);
/// How long a page that must not come gets to come anyway.
const NOT_OFFERED: Duration = Duration::from_secs(12);

const ADMIN: &str = "ada.qa@own.example.test";
const DANA: &str = "dana.qa@own.example.test";
const ELI: &str = "eli.qa@own.example.test";
const BOT_NAME: &str = "Synthetic Helper";
const ENDPOINT: &str = "https://bots.example.test/api/messages";
const PACKAGE_APP: &str = "00000000-0000-4000-8000-0000000002a1";
const PACKAGE_BOT: &str = "00000000-0000-4000-8000-0000000002b1";
const PACKAGE_NAME: &str = "Synthetic Polls";
/// The longest name the server takes (30 characters), with spaces and without.
const LONG_NAME: &str = "Quarterly planning retro board";
const WIDE_NAME: &str = "WWWWWWWWWWWWWWWWWWWWWWWWWWWWWW";
const MANY: usize = 40;
const SCOPE: &str = "https://api.botframework.com/.default";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa apps admin window: {error}");
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

// --- the parent: the server, the sandbox, the keyboard and the checks ---

#[derive(Default)]
struct Tally {
    passed: Vec<String>,
    failed: Vec<(String, String)>,
    notes: Vec<String>,
}

impl Tally {
    fn note(&mut self, line: String) {
        say(&format!("NOTE {line}"));
        self.notes.push(line);
    }

    fn outcome(&mut self, name: &str, outcome: SandboxResult) {
        match outcome {
            Ok(()) => {
                say(&format!("ok {name}"));
                self.passed.push(name.to_owned());
            }
            Err(error) => {
                say(&format!("FAILED {name}: {error}"));
                self.failed.push((name.to_owned(), error.to_string()));
            }
        }
    }
}

/// One run: its own server, and the app as one of its accounts.
type Run = fn(&Sandbox, &Path, &mut Tally) -> SandboxResult;

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams qa apps admin window", &options)? else {
        return Ok(());
    };
    let binary = own_server_binary::binary()?.ok_or("no ngteams-server binary: cargo build -p ngteams-server")?;
    let mut tally = Tally::default();
    let runs: [(&str, Run); 5] = [
        ("administrator", administrator),
        ("not_an_administrator", not_an_administrator),
        ("server_without_apps", server_without_apps),
        ("server_with_unsigned_bots", server_with_unsigned_bots),
        ("delivery_and_test_activity", delivery_and_test_activity),
    ];
    for (name, run) in runs {
        say(&format!("run {name}"));
        let outcome = run(&sandbox, &binary, &mut tally);
        if let Err(error) = outcome {
            tally.outcome(
                &format!("{name} (run)"),
                Err(format!("{error}{}", sandbox.logs()).into()),
            );
        }
    }
    for note in &tally.notes {
        say(&format!("note: {note}"));
    }
    for (name, error) in &tally.failed {
        say(&format!("FAILED {name}: {error}"));
    }
    say(&format!(
        "qa apps admin checks: {} passed, {} failed",
        tally.passed.len(),
        tally.failed.len()
    ));
    if tally.failed.is_empty() {
        say("ngteams qa apps admin window: ok");
        Ok(())
    } else {
        Err(format!("{} check(s) failed", tally.failed.len()).into())
    }
}

fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// What the run learned on its way: the ids and every secret the window showed.
#[derive(Default)]
struct Seen {
    bot: String,
    secret: String,
    package_secret: String,
    dana: String,
    eli: String,
    secrets: Vec<String>,
}

struct Qa<'a> {
    sandbox: &'a Sandbox,
    server: &'a BinaryServer,
    running: Option<Running>,
    input: VirtualInput,
    dir: PathBuf,
    seen: Seen,
}

impl<'a> Qa<'a> {
    fn start(sandbox: &'a Sandbox, server: &'a BinaryServer, run: &str, upn: &str) -> SandboxResult<Self> {
        let dir = sandbox.path(&format!("app-{run}"));
        fs::create_dir_all(&dir)?;
        let ca = server.ca_file.to_string_lossy().into_owned();
        let data = dir.to_string_lossy().into_owned();
        let vars = [
            (CHILD_ENV, "app"),
            (teams_ui::bots_flag::BOTS_ENV, "1"),
            (ORIGIN_ENV, server.origin.as_str()),
            (CA_ENV, ca.as_str()),
            (DIR_ENV, data.as_str()),
            (UPN_ENV, upn),
        ];
        let child = sandbox.spawn_with_fds(&std::env::current_exe()?, &[], &vars, &[])?;
        let mut running = Running::new(child)?;
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
        Ok(Qa {
            sandbox,
            server,
            running: Some(running),
            input: sandbox.input()?,
            dir,
            seen: Seen::default(),
        })
    }

    fn running(&self) -> SandboxResult<&Running> {
        self.running.as_ref().ok_or_else(|| "the app is gone".into())
    }

    fn check(&mut self, tally: &mut Tally, name: &str, flow: fn(&mut Qa<'_>, &mut Tally) -> SandboxResult) {
        let outcome = flow(self, tally);
        if outcome.is_err() {
            let _ = self.shot(&format!("failed-{name}"));
            let _ = self.ask("do close");
            let _ = self.ask("theme light");
        }
        tally.outcome(name, outcome);
    }

    fn ask(&self, question: &str) -> SandboxResult<Value> {
        let staged = self.sandbox.path("qa-apps-admin-ask.staged");
        fs::write(&staged, question)?;
        fs::rename(&staged, self.sandbox.path(ASK))?;
        let line = self
            .running()?
            .line_where(WAIT, |line| line.starts_with("answer "))
            .map_err(|error| format!("no answer to {question}: {error:?}"))?;
        let answer: Value = serde_json::from_str(line.trim_start_matches("answer "))?;
        if let Some(error) = answer.get("error") {
            return Err(format!("{question}: {error}").into());
        }
        Ok(answer)
    }

    fn act(&self, what: &str) -> SandboxResult<Value> {
        let answer = self.ask(&format!("do {what}"))?;
        thread::sleep(SETTLE);
        Ok(answer)
    }

    fn state_until(&self, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        self.state_within(WAIT, what, wanted)
    }

    fn state_within(&self, within: Duration, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let mut last = Value::Null;
        let found = wait_until(within, || match self.ask("state") {
            Ok(state) if wanted(&state) => Some(state),
            Ok(state) => {
                last = state;
                thread::sleep(Duration::from_millis(200));
                None
            }
            Err(_) => None,
        });
        found.ok_or_else(|| format!("{what}: the window holds {}", redacted(&last)).into())
    }

    /// The step's problem once the server answered.
    fn problem_after(&self, what: &str) -> SandboxResult<String> {
        let state = self.state_until(what, |state| state["dialog"]["problem"].is_string())?;
        Ok(state["dialog"]["problem"].as_str().unwrap_or_default().to_owned())
    }

    /// What the server holds now, by app name, as the administrator's API answers a fresh request.
    fn server_apps(&self) -> SandboxResult<Value> {
        Ok(self.ask("server")?["apps"].clone())
    }

    fn shot(&self, name: &str) -> SandboxResult<PathBuf> {
        thread::sleep(SETTLE);
        let file = format!("qa-apps-admin-{name}.png");
        let png = self.sandbox.screenshot(&file, true)?;
        let kept = shots_dir()?.join(&file);
        fs::copy(&png, &kept)?;
        Ok(kept)
    }

    /// Shots `name` in Teams Light, then `name-dark` in Teams Dark; light again after.
    fn shots(&self, name: &str) -> SandboxResult {
        self.shot(name)?;
        self.ask("theme dark")?;
        let dark = self.shot(&format!("{name}-dark"));
        self.ask("theme light")?;
        dark.map(|_| ())
    }

    fn type_text(&mut self, text: &str) -> SandboxResult {
        self.input.type_text(text)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn key(&mut self, key: u32) -> SandboxResult {
        self.input.key(key)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// Notes the secret the made step shows (the window shows each secret one time).
    fn shown_secret(&mut self, state: &Value) -> SandboxResult<String> {
        let secret = state["dialog"]["secret"].as_str().ok_or("no secret shown")?.to_owned();
        ensure(secret.len() >= 20, || "the secret shown is short".to_owned())?;
        self.seen.secrets.push(secret.clone());
        Ok(secret)
    }

    /// Asks the app to quit; its stderr.
    fn quit(&mut self) -> SandboxResult<String> {
        let _ = self.ask("quit");
        let running = self.running.take().ok_or("the app is gone")?;
        let (status, stderr) = running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("the app exited {status}: {stderr}").into());
        }
        Ok(stderr)
    }

    /// The token endpoint's answer to `client` with `secret`: its status and `access_token` or its error code.
    fn token(&self, client: &str, secret: &str) -> SandboxResult<(u16, String)> {
        token(self.server, client, secret)
    }
}

impl Drop for Qa<'_> {
    fn drop(&mut self) {
        if self.running.is_some() {
            let _ = self.quit();
        }
    }
}

/// `state` with each shown secret and the settings' text left out, for a failure's message.
fn redacted(state: &Value) -> Value {
    let mut state = state.clone();
    if let Some(dialog) = state.get_mut("dialog").and_then(Value::as_object_mut) {
        dialog.remove("secret");
        dialog.remove("settings");
    }
    state
}

/// The client-credentials grant as MSAL sends it, to the server's token endpoint under the Bot Framework tenant.
fn token(server: &BinaryServer, client: &str, secret: &str) -> SandboxResult<(u16, String)> {
    let output = Command::new("curl")
        .args(["-sS", "--max-time", "20", "-o", "-", "-w", "\n%{http_code}", "--cacert"])
        .arg(&server.ca_file)
        .args(["--data-urlencode", "grant_type=client_credentials", "--data-urlencode"])
        .arg(format!("client_id={client}"))
        .arg("--data-urlencode")
        .arg(format!("client_secret={secret}"))
        .arg("--data-urlencode")
        .arg(format!("scope={SCOPE}"))
        .arg(format!("{}/botframework.com/oauth2/v2.0/token", server.origin))
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let (body, status) = text.rsplit_once('\n').ok_or("curl said nothing")?;
    let status: u16 = status.trim().parse()?;
    let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let said = if body["access_token"].is_string() {
        "access_token".to_owned()
    } else {
        format!("{} {}", body["error"].as_str().unwrap_or("?"), body["error_codes"])
    };
    Ok((status, said))
}

fn dialog_is(state: &Value, kind: &str) -> bool {
    state["dialog"]["kind"] == kind
}

fn app_named<'s>(state: &'s Value, name: &str) -> Option<&'s Value> {
    state["apps"].as_array()?.iter().find(|app| app["name"] == name)
}

// --- run 1: the administrator ---

type Flow = fn(&mut Qa<'_>, &mut Tally) -> SandboxResult;

fn administrator(sandbox: &Sandbox, binary: &Path, tally: &mut Tally) -> SandboxResult {
    let server = BinaryServer::start(binary)?;
    server.add_admin(ADMIN, "Ada Admin (synthetic)")?;
    let dana = server.add_user(DANA, "Dana Doe (synthetic)")?;
    let eli = server.add_user(ELI, "Eli Example (synthetic)")?;
    let mut qa = Qa::start(sandbox, &server, "admin", ADMIN)?;
    qa.seen.dana = dana;
    qa.seen.eli = eli;
    let flows: [(&str, Flow); 14] = [
        ("settings_offer_manage_apps_after_manage_users", offered),
        ("register_a_bot_from_the_keyboard_and_its_secret_gets_a_token", register),
        ("the_secret_shows_one_time_only", secret_once),
        ("register_refusals_say_why", register_refusals),
        ("upload_a_good_package", upload_good),
        ("upload_refusals_say_why", upload_refusals),
        ("turn_an_app_off_and_on_against_the_token_endpoint", off_and_on),
        ("change_the_endpoint_good_and_refused", endpoint),
        ("limit_an_app_to_selected_people", access),
        ("a_new_secret_stops_the_old_one", new_secret),
        ("the_delete_confirmation_starts_on_back", delete_starts_on_back),
        ("delete_an_app_and_its_secret_stops", delete),
        ("many_apps_long_names_and_the_search", many_apps),
        ("the_list_after_another_administrator_adds_an_app", list_freshness),
    ];
    for (name, flow) in flows {
        qa.check(tally, name, flow);
    }
    let stderr = qa.quit()?;
    let secrets = std::mem::take(&mut qa.seen.secrets);
    let dir = qa.dir.clone();
    drop(qa);
    tally.outcome(
        "no_secret_in_the_apps_files_logs_or_the_servers_log",
        no_secret_leaked(&dir, &stderr, &server.log(), &secrets),
    );
    Ok(())
}

fn offered(qa: &mut Qa<'_>, _: &mut Tally) -> SandboxResult {
    let state = qa.state_until("Manage apps offered", |state| {
        state["pages"]
            .as_array()
            .is_some_and(|pages| pages.iter().any(|page| page == "apps"))
    })?;
    ensure(state["pages"] == json!(["users", "apps"]), || {
        format!("the pages are {}", state["pages"])
    })?;
    qa.act("settings")?;
    qa.state_until("the page, read", |state| {
        state["page"] == "apps" && state["loaded"] == true
    })?;
    qa.shots("empty")
}

fn register(qa: &mut Qa<'_>, _: &mut Tally) -> SandboxResult {
    qa.act("register")?;
    qa.state_until("the register form", |state| dialog_is(state, "register"))?;
    qa.type_text(BOT_NAME)?;
    qa.key(keys::TAB)?;
    qa.type_text("Answers made-up questions")?;
    qa.key(keys::TAB)?;
    qa.type_text(ENDPOINT)?;
    qa.shots("register")?;
    qa.key(keys::ENTER)?;
    let state = qa.state_until("the bot made", |state| dialog_is(state, "made"))?;
    let secret = qa.shown_secret(&state)?;
    let server = qa.server_apps()?;
    let bot = server[BOT_NAME]["bot"].as_str().unwrap_or_default().to_owned();
    ensure(
        server[BOT_NAME]["endpoint"] == ENDPOINT && server[BOT_NAME]["mode"] == "signed" && !bot.is_empty(),
        || format!("the server holds {server}"),
    )?;
    ensure(state["dialog"]["app_id_shown"] == bot.as_str(), || {
        format!("the app id shown is not the bot's {bot}: {}", redacted(&state))
    })?;
    qa.shots("registered")?;
    let (status, said) = qa.token(&bot, &secret)?;
    ensure(status == 200 && said == "access_token", || {
        format!("the new secret got {status} {said}")
    })?;
    qa.seen.bot = bot;
    qa.seen.secret = secret;
    // the settings shown now carry the secret; the .NET ones name this server
    qa.act("go settings")?;
    let state = qa.state_until("the settings, read", |state| {
        dialog_is(state, "settings") && state["dialog"]["settings"].is_string()
    })?;
    let text = state["dialog"]["settings"].as_str().unwrap_or_default();
    ensure(
        text.contains(&qa.server.origin) && text.contains(&qa.seen.secret) && text.contains(&qa.seen.bot),
        || "the settings shown after the bot was made lack this server, the id or the secret".to_owned(),
    )?;
    qa.shots("sdk-dotnet")?;
    for sdk in ["python", "javascript", "teams", "agents"] {
        qa.act(&format!("sdk {sdk}"))?;
        let state = qa.state_until("the SDK's settings", |state| state["dialog"]["sdk"] == sdk)?;
        ensure(state["dialog"]["settings"].is_string(), || {
            format!("no settings for {sdk}")
        })?;
        qa.shot(&format!("sdk-{sdk}"))?;
    }
    qa.key(keys::ESC)?;
    let state = qa.state_until("the dialog closed", |state| state["dialog"].is_null())?;
    ensure(state["apps"].as_array().is_some_and(|apps| apps.len() == 1), || {
        format!("the list holds {}", state["apps"])
    })?;
    qa.shots("one-app")
}

fn secret_once(qa: &mut Qa<'_>, _: &mut Tally) -> SandboxResult {
    qa.act(&format!("open {BOT_NAME}"))?;
    let state = qa.state_until("the details", |state| dialog_is(state, "details"))?;
    ensure(state["dialog"]["secret"].is_null(), || {
        "the details show the secret".to_owned()
    })?;
    qa.act("go settings")?;
    let state = qa.state_until("the settings, read", |state| {
        dialog_is(state, "settings") && state["dialog"]["settings"].is_string()
    })?;
    let text = state["dialog"]["settings"].as_str().unwrap_or_default();
    ensure(
        !text.contains(&qa.seen.secret) && state["dialog"]["settings_have_secret"] == false,
        || "the settings shown later carry the secret".to_owned(),
    )?;
    qa.shots("settings-later")?;
    qa.act("close")?;
    qa.state_until("closed", |state| state["dialog"].is_null()).map(|_| ())
}

fn register_refusals(qa: &mut Qa<'_>, tally: &mut Tally) -> SandboxResult {
    let cases: [(&str, &str, &str, &str); 5] = [
        ("unsigned", "Synthetic Unsigned", ENDPOINT, "unsigned"),
        (
            "plain-http",
            "Synthetic Http",
            "http://bots.example.test/api/messages",
            "signed",
        ),
        (
            "credentials",
            "Synthetic Creds",
            "https://user:pass@bots.example.test/api/messages",
            "signed",
        ),
        ("long-name", "Synthetic name over thirty chars", ENDPOINT, "signed"),
        ("not-a-url", "Synthetic Garbage", "bots example test", "signed"),
    ];
    let mut failures = Vec::new();
    for (case, name, endpoint, mode) in cases {
        qa.act("register")?;
        qa.state_until("the register form", |state| dialog_is(state, "register"))?;
        qa.act(&format!("fill {name}|made-up|{endpoint}"))?;
        qa.act(&format!("mode {mode}"))?;
        qa.act("confirm")?;
        match qa.problem_after(&format!("the refusal of {case}")) {
            Ok(problem) => {
                say(&format!("refusal {case}: {problem}"));
                qa.shot(&format!("refused-{case}"))?;
            }
            Err(error) => failures.push(format!("{case}: no problem shown ({error})")),
        }
        qa.act("close")?;
        qa.state_until("closed", |state| state["dialog"].is_null())?;
    }
    qa.ask("theme dark")?;
    qa.act("register")?;
    qa.act(&format!("fill Synthetic Unsigned|made-up|{ENDPOINT}"))?;
    qa.act("mode unsigned")?;
    qa.act("confirm")?;
    let _ = qa.problem_after("the unsigned refusal in dark");
    qa.shot("refused-unsigned-dark")?;
    qa.ask("theme light")?;
    qa.act("close")?;
    let server = qa.server_apps()?;
    let held: Vec<&String> = server.as_object().map(|apps| apps.keys().collect()).unwrap_or_default();
    if held.len() != 1 {
        tally.note(format!("after the refusals the server holds {held:?}"));
    }
    ensure(failures.is_empty(), || failures.join("; "))
}

fn upload_good(qa: &mut Qa<'_>, _: &mut Tally) -> SandboxResult {
    qa.act("upload")?;
    qa.state_until("the upload step", |state| dialog_is(state, "upload"))?;
    qa.shots("upload-empty")?;
    qa.act("package good")?;
    qa.shots("upload")?;
    qa.act("confirm")?;
    let state = qa.state_until("the package's app made", |state| dialog_is(state, "made"))?;
    let secret = qa.shown_secret(&state)?;
    qa.shots("uploaded")?;
    let (status, said) = qa.token(PACKAGE_BOT, &secret)?;
    ensure(status == 200 && said == "access_token", || {
        format!("the package bot's secret got {status} {said}")
    })?;
    qa.seen.package_secret = secret;
    qa.key(keys::ESC)?;
    let state = qa.state_until("two apps listed", |state| {
        state["dialog"].is_null() && state["apps"].as_array().is_some_and(|apps| apps.len() == 2)
    })?;
    let app = app_named(&state, PACKAGE_NAME).ok_or("the package's app is not listed")?;
    ensure(app["endpoint"].is_null(), || {
        format!("the package's bot has an endpoint: {app}")
    })?;
    qa.shots("two-apps")
}

fn upload_refusals(qa: &mut Qa<'_>, _: &mut Tally) -> SandboxResult {
    let mut failures = Vec::new();
    for case in ["large", "broken", "no-manifest", "bad-icon", "good"] {
        qa.act("upload")?;
        qa.state_until("the upload step", |state| dialog_is(state, "upload"))?;
        qa.act(&format!("package {case}"))?;
        let state = qa.ask("state")?;
        if state["dialog"]["problem"].is_null() {
            qa.act("confirm")?;
        }
        match qa.problem_after(&format!("the refusal of the {case} package")) {
            Ok(problem) => {
                say(&format!("package {case}: {problem}"));
                qa.shot(&format!("upload-refused-{case}"))?;
            }
            Err(error) => failures.push(format!("{case}: no problem shown ({error})")),
        }
        qa.act("close")?;
        qa.state_until("closed", |state| state["dialog"].is_null())?;
    }
    let server = qa.server_apps()?;
    ensure(server.as_object().is_some_and(|apps| apps.len() == 2), || {
        format!("after the refused packages the server holds {server}")
    })?;
    ensure(failures.is_empty(), || failures.join("; "))
}

fn off_and_on(qa: &mut Qa<'_>, _: &mut Tally) -> SandboxResult {
    qa.act(&format!("open {BOT_NAME}"))?;
    qa.state_until("the details", |state| dialog_is(state, "details"))?;
    qa.shots("details")?;
    qa.act("enabled off")?;
    qa.act("confirm")?;
    qa.state_until("the details closed", |state| state["dialog"].is_null())?;
    let server = qa.server_apps()?;
    ensure(server[BOT_NAME]["enabled"] == false, || {
        format!("the server holds {server}")
    })?;
    let (status, said) = qa.token(&qa.seen.bot, &qa.seen.secret)?;
    ensure(status == 400 && said.contains("unauthorized_client"), || {
        format!("an app turned off still got {status} {said}")
    })?;
    qa.shots("app-off")?;
    qa.act(&format!("open {BOT_NAME}"))?;
    qa.act("enabled on")?;
    qa.act("confirm")?;
    qa.state_until("the details closed", |state| state["dialog"].is_null())?;
    let (status, said) = qa.token(&qa.seen.bot, &qa.seen.secret)?;
    ensure(status == 200 && said == "access_token", || {
        format!("an app turned on again got {status} {said}")
    })
}

fn endpoint(qa: &mut Qa<'_>, _: &mut Tally) -> SandboxResult {
    let good = "https://bots.example.test/v2/messages";
    qa.act(&format!("open {BOT_NAME}"))?;
    qa.act(&format!("endpoint {good}"))?;
    qa.act("confirm")?;
    qa.state_until("the details closed", |state| state["dialog"].is_null())?;
    let server = qa.server_apps()?;
    ensure(server[BOT_NAME]["endpoint"] == good, || {
        format!("the server holds {server}")
    })?;
    let mut failures = Vec::new();
    for (case, change) in [
        ("private", "endpoint https://10.20.30.40/api/messages"),
        ("metadata", "endpoint https://169.254.169.254/latest/meta-data"),
        ("loopback", "endpoint https://127.0.0.1:3978/api/messages"),
        ("plain-http", "endpoint http://bots.example.test/api/messages"),
        ("unsigned", "mode unsigned"),
    ] {
        qa.act(&format!("open {BOT_NAME}"))?;
        qa.act(change)?;
        qa.act("confirm")?;
        let state = qa.state_until(&format!("the answer to {case}"), |state| {
            state["dialog"].is_null() || (state["dialog"]["problem"].is_string())
        })?;
        if state["dialog"].is_null() {
            // the server refuses each: a private, metadata or loopback address as it is saved (no network is opened)
            let server = qa.server_apps()?;
            failures.push(format!("{case}: taken, the server holds {}", server[BOT_NAME]));
        } else {
            say(&format!("endpoint {case}: {}", state["dialog"]["problem"]));
            qa.shot(&format!("endpoint-refused-{case}"))?;
            qa.act("close")?;
        }
    }
    qa.ask("theme dark")?;
    qa.act(&format!("open {BOT_NAME}"))?;
    qa.act("endpoint http://bots.example.test/api/messages")?;
    qa.act("confirm")?;
    let _ = qa.problem_after("the http refusal in dark");
    qa.shot("endpoint-refused-plain-http-dark")?;
    qa.ask("theme light")?;
    qa.act("close")?;
    // back to a good endpoint for the later flows, when a change was taken (with nothing to change, Save is off)
    if qa.server_apps()?[BOT_NAME]["endpoint"] != good {
        qa.act(&format!("open {BOT_NAME}"))?;
        qa.act(&format!("endpoint {good}"))?;
        qa.act("confirm")?;
        qa.state_until("closed", |state| state["dialog"].is_null())?;
    }
    ensure(failures.is_empty(), || failures.join("; "))
}

fn access(qa: &mut Qa<'_>, _: &mut Tally) -> SandboxResult {
    qa.act(&format!("open {BOT_NAME}"))?;
    qa.act("go access")?;
    qa.state_until("the access step", |state| dialog_is(state, "access"))?;
    qa.shots("access-everyone")?;
    qa.act("who specific")?;
    // Save with nobody chosen says what to do and sends nothing
    qa.act("confirm")?;
    let problem = qa.problem_after("the refusal of nobody chosen")?;
    ensure(problem.contains("at least one person"), || {
        format!("with nobody chosen: {problem}")
    })?;
    let server = qa.server_apps()?;
    ensure(server[BOT_NAME]["everyone"] == true, || {
        format!("Save with nobody chosen changed the server: {}", server[BOT_NAME])
    })?;
    qa.shots("access-nobody")?;
    qa.act("search dana")?;
    qa.state_until("Dana found", |state| state["dialog"]["found"] == json!([DANA]))?;
    qa.act(&format!("add {DANA}"))?;
    qa.shots("access")?;
    qa.act("confirm")?;
    qa.state_until("back on the details", |state| dialog_is(state, "details"))?;
    let server = qa.server_apps()?;
    ensure(
        server[BOT_NAME]["everyone"] == false && server[BOT_NAME]["users"] == json!([qa.seen.dana]),
        || format!("the server holds {}", server[BOT_NAME]),
    )?;
    ensure(
        !server[BOT_NAME]["users"]
            .as_array()
            .is_some_and(|users| users.iter().any(|user| user == qa.seen.eli.as_str())),
        || "Eli may use the app".to_owned(),
    )?;
    qa.shots("details-specific")?;
    qa.key(keys::ESC)?;
    let state = qa.state_until("closed", |state| state["dialog"].is_null())?;
    let app = app_named(&state, BOT_NAME).ok_or("the bot is not listed")?;
    ensure(app["everyone"] == false, || format!("the list holds {app}"))?;
    qa.shots("list-specific")?;
    // the people chosen show again when the step opens again
    qa.act(&format!("open {BOT_NAME}"))?;
    qa.act("go access")?;
    let state = qa.state_until("the access step again", |state| dialog_is(state, "access"))?;
    ensure(state["dialog"]["users"] == json!([qa.seen.dana]), || {
        format!("the step shows {}", state["dialog"]["users"])
    })?;
    qa.act("close")?;
    qa.state_until("closed", |state| state["dialog"].is_null()).map(|_| ())
}

fn new_secret(qa: &mut Qa<'_>, _: &mut Tally) -> SandboxResult {
    qa.act(&format!("open {BOT_NAME}"))?;
    qa.act("go new-secret")?;
    qa.shots("new-secret-confirm")?;
    // Enter on the confirmation's first button (Back) makes no new secret
    qa.key(keys::ENTER)?;
    let state = qa.ask("state")?;
    let (status, _) = qa.token(&qa.seen.bot, &qa.seen.secret)?;
    ensure(status == 200 && !dialog_is(&state, "made"), || {
        format!(
            "Enter on the confirmation made a new secret ({status}): {}",
            redacted(&state)
        )
    })?;
    if !dialog_is(&state, "new-secret") {
        qa.act("go new-secret")?;
    }
    qa.act("confirm")?;
    let state = qa.state_until("the new secret", |state| dialog_is(state, "made"))?;
    let secret = qa.shown_secret(&state)?;
    ensure(secret != qa.seen.secret, || "the new secret is the old one".to_owned())?;
    qa.shots("new-secret")?;
    let (old_status, old_said) = qa.token(&qa.seen.bot, &qa.seen.secret)?;
    let (new_status, new_said) = qa.token(&qa.seen.bot, &secret)?;
    ensure(old_status == 401 && old_said.contains("invalid_client"), || {
        format!("the old secret got {old_status} {old_said}")
    })?;
    ensure(new_status == 200 && new_said == "access_token", || {
        format!("the new secret got {new_status} {new_said}")
    })?;
    qa.seen.secret = secret;
    qa.key(keys::ENTER)?;
    qa.state_until("closed", |state| state["dialog"].is_null()).map(|_| ())
}

fn delete_starts_on_back(qa: &mut Qa<'_>, _: &mut Tally) -> SandboxResult {
    qa.act(&format!("open {PACKAGE_NAME}"))?;
    qa.act("go delete")?;
    qa.state_until("the delete confirmation", |state| dialog_is(state, "delete"))?;
    qa.shots("delete-confirm")?;
    qa.key(keys::ENTER)?;
    let state = qa.ask("state")?;
    let server = qa.server_apps()?;
    ensure(server[PACKAGE_NAME].is_object(), || {
        "Enter on the delete confirmation deleted the app".to_owned()
    })?;
    say(&format!(
        "after Enter on the delete confirmation: {}",
        state["dialog"]["kind"]
    ));
    qa.act("close")?;
    qa.state_until("closed", |state| state["dialog"].is_null()).map(|_| ())
}

fn delete(qa: &mut Qa<'_>, _: &mut Tally) -> SandboxResult {
    qa.act(&format!("open {PACKAGE_NAME}"))?;
    qa.act("go delete")?;
    qa.act("confirm")?;
    qa.state_until("one app left", |state| {
        state["dialog"].is_null() && state["apps"].as_array().is_some_and(|apps| apps.len() == 1)
    })?;
    let server = qa.server_apps()?;
    ensure(server[PACKAGE_NAME].is_null(), || {
        format!("the server still holds {server}")
    })?;
    let (status, said) = qa.token(PACKAGE_BOT, &qa.seen.package_secret)?;
    ensure(status == 401 && said.contains("invalid_client"), || {
        format!("a deleted app's secret got {status} {said}")
    })?;
    qa.shots("after-delete")
}

fn many_apps(qa: &mut Qa<'_>, _: &mut Tally) -> SandboxResult {
    let mut names: Vec<String> = (1..=MANY).map(|n| format!("Synthetic app {n:02}")).collect();
    names.push(LONG_NAME.to_owned());
    names.push(WIDE_NAME.to_owned());
    for name in &names {
        qa.server.run(
            &[
                "app",
                "add",
                "--name",
                name.as_str(),
                "--endpoint",
                ENDPOINT,
                "--description",
                "A made-up app the operator registered",
            ],
            None,
        )?;
    }
    qa.act("reload")?;
    let want = names.len() + 1;
    let state = qa.state_until("every app listed", |state| {
        state["loaded"] == true && state["apps"].as_array().is_some_and(|apps| apps.len() == want)
    })?;
    ensure(
        state["shown"].as_array().is_some_and(|shown| shown.len() == want),
        || format!("{} shown of {want}", state["shown"]),
    )?;
    qa.shots("many")?;
    // the wheel over the list reaches its last rows (the wide name sorts last)
    for _ in 0..4 {
        qa.input.scroll(760, 500, 15)?;
        thread::sleep(Duration::from_millis(300));
    }
    qa.shots("many-end")?;
    for _ in 0..4 {
        qa.input.scroll(760, 500, -15)?;
        thread::sleep(Duration::from_millis(300));
    }
    qa.act("find synthetic app 1")?;
    let wanted: Vec<String> = names
        .iter()
        .filter(|name| name.to_lowercase().contains("synthetic app 1"))
        .cloned()
        .collect();
    let state = qa.state_until("the search narrowed", |state| state["shown"] == json!(wanted))?;
    ensure(!state["shown"].as_array().is_some_and(Vec::is_empty), || {
        "nothing found".to_owned()
    })?;
    qa.shots("search")?;
    qa.act("find no such app here")?;
    qa.state_until("nothing found", |state| state["shown"] == json!([]))?;
    qa.shots("search-none")?;
    qa.act("find")?;
    qa.act(&format!("open {LONG_NAME}"))?;
    qa.state_until("the details", |state| dialog_is(state, "details"))?;
    qa.shots("details-long-name")?;
    qa.act("close")?;
    qa.act(&format!("open {WIDE_NAME}"))?;
    qa.shots("details-wide-name")?;
    qa.act("close")?;
    qa.state_until("closed", |state| state["dialog"].is_null()).map(|_| ())
}

/// What another administrator (here the server's command line) adds while a page is open shows when the page opens
/// again: Manage apps, then Manage users.
fn list_freshness(qa: &mut Qa<'_>, _: &mut Tally) -> SandboxResult {
    const LATE_APP: &str = "Synthetic late app";
    const LATE_USER: &str = "fay.qa@own.example.test";
    let before = qa.ask("state")?["apps"].as_array().map_or(0, Vec::len);
    qa.server
        .run(&["app", "add", "--name", LATE_APP, "--endpoint", ENDPOINT], None)?;
    qa.act("leave")?;
    qa.act("settings")?;
    qa.state_within(Duration::from_secs(8), "the apps read again", |state| {
        state["apps"].as_array().is_some_and(|apps| apps.len() == before + 1)
            && state["shown"]
                .as_array()
                .is_some_and(|shown| shown.contains(&json!(LATE_APP)))
    })?;
    // the long list holds the new app below the fold: the search brings it into the shot
    qa.act("find synthetic late")?;
    qa.state_until("the new app found", |state| state["shown"] == json!([LATE_APP]))?;
    qa.shots("reopened-apps")?;
    qa.act("find")?;

    qa.act("users")?;
    qa.state_until("the users", |state| {
        state["page"] == "users" && state["users"].as_array().is_some_and(|users| !users.is_empty())
    })?;
    qa.server.add_user(LATE_USER, "Fay Late (synthetic)")?;
    qa.act("leave")?;
    qa.act("users")?;
    qa.state_within(Duration::from_secs(8), "the users read again", |state| {
        state["users"]
            .as_array()
            .is_some_and(|users| users.contains(&json!(LATE_USER)))
    })?;
    qa.shots("reopened-users")?;
    qa.act("settings")?;
    qa.state_until("Manage apps again", |state| state["page"] == "apps")
        .map(|_| ())
}

/// No secret the window showed is in a file the app wrote, its stderr, or the server's log.
fn no_secret_leaked(dir: &Path, stderr: &str, server_log: &str, secrets: &[String]) -> SandboxResult {
    ensure(!secrets.is_empty(), || "no secret was seen".to_owned())?;
    let mut files = Vec::new();
    walk(dir, &mut files)?;
    let log = files
        .iter()
        .find(|file| file.to_string_lossy().contains("log"))
        .ok_or_else(|| format!("the app wrote no log in {}", dir.display()))?;
    let log_text = fs::read_to_string(log).unwrap_or_default();
    ensure(log_text.contains("/api/admin/v1/apps"), || {
        "the app's debug log holds no admin request: the scan proves nothing".to_owned()
    })?;
    let mut found = Vec::new();
    for file in &files {
        let bytes = fs::read(file)?;
        let text = String::from_utf8_lossy(&bytes);
        for (n, secret) in secrets.iter().enumerate() {
            if text.contains(secret.as_str()) {
                found.push(format!("secret {n} in {}", file.display()));
            }
        }
    }
    for (n, secret) in secrets.iter().enumerate() {
        if stderr.contains(secret.as_str()) {
            found.push(format!("secret {n} in the app's stderr"));
        }
        if server_log.contains(secret.as_str()) {
            found.push(format!("secret {n} in the server's log"));
        }
    }
    say(&format!(
        "secret scan: {} secrets, {} files ({}), stderr, the server's log",
        secrets.len(),
        files.len(),
        files
            .iter()
            .filter_map(|file| file.strip_prefix(dir).ok())
            .map(|file| file.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    ensure(found.is_empty(), || found.join("; "))
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            walk(&path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

// --- runs 2 to 4 ---

fn not_an_administrator(sandbox: &Sandbox, binary: &Path, tally: &mut Tally) -> SandboxResult {
    let server = BinaryServer::start(binary)?;
    server.add_admin(ADMIN, "Ada Admin (synthetic)")?;
    server.add_user(DANA, "Dana Doe (synthetic)")?;
    server.run(
        &["app", "add", "--name", "Synthetic visible app", "--endpoint", ENDPOINT],
        None,
    )?;
    let mut qa = Qa::start(sandbox, &server, "user", DANA)?;
    let outcome = (|| -> SandboxResult {
        let offered = qa.state_within(NOT_OFFERED, "a page offered", |state| {
            state["pages"].as_array().is_some_and(|pages| !pages.is_empty())
        });
        ensure(offered.is_err(), || {
            format!("a user who is no administrator is offered {offered:?}")
        })?;
        qa.act("settings")?;
        qa.shots("not-admin-settings")
    })();
    tally.outcome("a_user_who_is_no_administrator_gets_no_manage_apps", outcome);
    qa.quit().map(|_| ())
}

fn server_without_apps(sandbox: &Sandbox, binary: &Path, tally: &mut Tally) -> SandboxResult {
    let server = BinaryServer::start_with(binary, "\n[apps]\nenabled = false\n")?;
    server.add_admin(ADMIN, "Ada Admin (synthetic)")?;
    let mut qa = Qa::start(sandbox, &server, "apps-off", ADMIN)?;
    let outcome = (|| -> SandboxResult {
        qa.state_until("Manage users offered", |state| {
            state["pages"]
                .as_array()
                .is_some_and(|pages| pages.iter().any(|page| page == "users"))
        })?;
        let apps = qa.state_within(NOT_OFFERED, "Manage apps offered", |state| {
            state["pages"]
                .as_array()
                .is_some_and(|pages| pages.iter().any(|page| page == "apps"))
        });
        ensure(apps.is_err(), || "a server without apps offers Manage apps".to_owned())?;
        qa.act("settings")?;
        qa.shots("apps-off-settings")
    })();
    tally.outcome("a_server_without_apps_offers_no_manage_apps", outcome);
    qa.quit().map(|_| ())
}

fn server_with_unsigned_bots(sandbox: &Sandbox, binary: &Path, tally: &mut Tally) -> SandboxResult {
    let server = BinaryServer::start_with(binary, "\n[bots]\nallow_unsigned = true\n")?;
    server.add_admin(ADMIN, "Ada Admin (synthetic)")?;
    let mut qa = Qa::start(sandbox, &server, "unsigned", ADMIN)?;
    let outcome = (|| -> SandboxResult {
        qa.state_until("Manage apps offered", |state| {
            state["pages"]
                .as_array()
                .is_some_and(|pages| pages.iter().any(|page| page == "apps"))
        })?;
        qa.act("settings")?;
        qa.state_until("the page, read", |state| state["loaded"] == true)?;
        qa.act("register")?;
        qa.act(&format!("fill Synthetic Dev Bot|made-up|{ENDPOINT}"))?;
        qa.act("mode unsigned")?;
        qa.shots("register-unsigned")?;
        qa.act("confirm")?;
        let state = qa.state_until("the unsigned bot made", |state| {
            dialog_is(state, "made") || state["dialog"]["problem"].is_string()
        })?;
        ensure(dialog_is(&state, "made"), || {
            format!("the unsigned bot was refused: {}", state["dialog"]["problem"])
        })?;
        let secret = state["dialog"]["secret"].as_str().unwrap_or_default().to_owned();
        qa.shots("made-unsigned")?;
        qa.act("go settings")?;
        let state = qa.state_until("the settings, read", |state| {
            dialog_is(state, "settings") && state["dialog"]["settings"].is_string()
        })?;
        let note = state["dialog"]["note"].as_str().unwrap_or_default();
        ensure(note.contains("unsigned"), || {
            format!("the unsigned settings say: {note}")
        })?;
        qa.shots("sdk-unsigned")?;
        qa.key(keys::ESC)?;
        let state = qa.state_until("closed", |state| state["dialog"].is_null())?;
        let app = app_named(&state, "Synthetic Dev Bot").ok_or("the bot is not listed")?;
        ensure(app["mode"] == "unsigned", || format!("the list holds {app}"))?;
        qa.shots("list-unsigned")?;
        let bot = app["bot"].as_str().unwrap_or_default().to_owned();
        if !secret.is_empty() {
            let (status, said) = qa.token(&bot, &secret)?;
            ensure(status == 400 && said.contains("unauthorized_client"), || {
                format!("an unsigned bot got a token: {status} {said}")
            })?;
        }
        Ok(())
    })();
    tally.outcome("an_unsigned_bot_on_a_server_that_allows_them", outcome);
    qa.quit().map(|_| ())
}

// --- run 5: a bot's delivery and its test activity (QA of task 524) ---

/// A bot on loopback that answers each request with `status`, and what it got.
struct LoopbackBot {
    url: String,
    status: Arc<Mutex<u16>>,
    got: Arc<Mutex<Vec<String>>>,
}

impl LoopbackBot {
    fn start() -> SandboxResult<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}/api/messages", listener.local_addr()?);
        let status = Arc::new(Mutex::new(200_u16));
        let got = Arc::new(Mutex::new(Vec::new()));
        let (answer, seen) = (Arc::clone(&status), Arc::clone(&got));
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let request = read_request(&stream);
                seen.lock().unwrap_or_else(PoisonError::into_inner).push(request);
                let status = *answer.lock().unwrap_or_else(PoisonError::into_inner);
                let mut stream = stream;
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                );
            }
        });
        Ok(Self { url, status, got })
    }

    fn answer(&self, status: u16) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = status;
    }

    fn got(&self) -> Vec<String> {
        self.got.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// One HTTP request's head and body, as text.
fn read_request(stream: &std::net::TcpStream) -> String {
    use std::io::{BufRead as _, Read as _};
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut reader = io::BufReader::new(stream);
    let mut head = String::new();
    let mut length = 0_usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
        head.push_str(&line);
    }
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);
    format!("{head}\r\n{}", String::from_utf8_lossy(&body))
}

const TAKES: &str = "Synthetic Taker Bot";
const FAILS: &str = "Synthetic Failing Bot";
const NOBODY: &str = "Synthetic Gone Bot";
const OFF: &str = "Synthetic Off Bot";
const BARE: &str = "Synthetic Bare Bot";

fn added_id(said: &str) -> SandboxResult<String> {
    said.lines()
        .find_map(|line| line.strip_prefix("added "))
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_owned)
        .ok_or_else(|| format!("app add said {said}").into())
}

fn delivery_and_test_activity(sandbox: &Sandbox, binary: &Path, tally: &mut Tally) -> SandboxResult {
    let taker = LoopbackBot::start()?;
    let failing = LoopbackBot::start()?;
    failing.answer(500);
    let gone = free_closed_port()?;
    let server = BinaryServer::start_with(
        binary,
        "\n[bots]\nallow_http_endpoints = true\nallowed_networks = [\"127.0.0.0/8\"]\n",
    )?;
    server.add_admin(ADMIN, "Ada Admin (synthetic)")?;
    for (name, endpoint) in [
        (TAKES, taker.url.clone()),
        (FAILS, failing.url.clone()),
        (NOBODY, format!("http://127.0.0.1:{gone}/api/messages")),
        (OFF, taker.url.clone()),
    ] {
        let said = server.run(&["app", "add", "--name", name, "--endpoint", &endpoint], None)?;
        if name == OFF {
            server.run(&["app", "disable", &added_id(&said)?], None)?;
        }
    }
    server.run(&["app", "add", "--name", BARE], None)?;
    let mut qa = Qa::start(sandbox, &server, "delivery", ADMIN)?;
    qa.state_until("Manage apps offered", |state| {
        state["pages"]
            .as_array()
            .is_some_and(|pages| pages.iter().any(|page| page == "apps"))
    })?;
    qa.act("settings")?;
    qa.state_until("the page, read", |state| state["loaded"] == true)?;
    qa.shots("delivery-list")?;

    let outcome = test_activity_taken(&mut qa, &taker);
    tally.outcome("a_new_bot_says_nothing_sent_and_takes_a_test_activity", outcome);
    let outcome = test_activity_refused(&mut qa, FAILS, "500", "delivery-test-500");
    tally.outcome("a_bot_that_answers_500_shows_it_did_not_take_the_test", outcome);
    let outcome = test_activity_refused(&mut qa, NOBODY, "", "delivery-test-unreachable");
    tally.outcome("an_endpoint_where_nothing_listens_shows_why_the_test_failed", outcome);
    for (name, shot, check) in [
        (OFF, "delivery-off", "an_app_that_is_off_offers_no_test"),
        (BARE, "delivery-no-endpoint", "an_app_without_an_endpoint_offers_no_test"),
    ] {
        let outcome = no_test_offered(&mut qa, name, shot);
        tally.outcome(check, outcome);
    }
    let log = server.log();
    let outcome = ensure(!log.contains("ngteams/testActivity\""), || {
        "the server's info log carries the test activity's payload".to_owned()
    });
    tally.outcome("the_test_activity_logs_no_payload", outcome);
    qa.quit().map(|_| ())
}

/// A port on loopback where nothing listens.
fn free_closed_port() -> SandboxResult<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn open_details(qa: &mut Qa<'_>, name: &str) -> SandboxResult<Value> {
    qa.act("close")?;
    qa.act(&format!("open {name}"))?;
    qa.state_until("the details", |state| {
        dialog_is(state, "details") && state["dialog"]["health"].is_string()
    })
}

fn test_activity_taken(qa: &mut Qa<'_>, taker: &LoopbackBot) -> SandboxResult {
    let state = open_details(qa, TAKES)?;
    let health = state["dialog"]["health"].as_str().unwrap_or_default();
    ensure(health == "Nothing sent to the bot yet.", || format!("a new bot's health says {health}"))?;
    qa.shots("delivery-new")?;
    qa.act("test")?;
    let state = qa.state_until("the test's answer", |state| state["dialog"]["test"].is_object())?;
    let test = &state["dialog"]["test"];
    let words = test["words"].as_str().unwrap_or_default();
    ensure(test["took"] == true && words.starts_with("The bot took it (200) in "), || {
        format!("the test says {test}")
    })?;
    qa.shots("delivery-test-taken")?;
    let got = taker.got();
    ensure(got.len() == 1, || format!("the bot got {} requests", got.len()))?;
    let request = &got[0];
    ensure(request.starts_with("POST /api/messages "), || format!("the bot got {request}"))?;
    ensure(request.contains("\"ngteams/testActivity\"") && request.contains("\"event\""), || {
        format!("the bot got {request}")
    })?;
    ensure(request.to_ascii_lowercase().contains("authorization: bearer "), || {
        "a signed bot's test activity carries no token".to_owned()
    })?;
    ensure(!request.contains("userAadToken") && !request.contains("\"token\""), || {
        "the test activity carries a user token".to_owned()
    })?;
    // the test is no delivery: the health stays new
    qa.act("reload")?;
    let state = open_details(qa, TAKES)?;
    let health = state["dialog"]["health"].as_str().unwrap_or_default();
    ensure(health == "Nothing sent to the bot yet.", || {
        format!("after a test the health says {health}")
    })
}

fn test_activity_refused(qa: &mut Qa<'_>, name: &str, says: &str, shot: &str) -> SandboxResult {
    open_details(qa, name)?;
    qa.act("test")?;
    let state = qa.state_until("the test's answer", |state| state["dialog"]["test"].is_object())?;
    let test = &state["dialog"]["test"];
    let words = test["words"].as_str().unwrap_or_default();
    ensure(test["took"] == false && words.starts_with("The bot didn't take it: "), || {
        format!("the test says {test}")
    })?;
    ensure(words.contains(says), || format!("the test says {words}, not {says}"))?;
    qa.shots(shot)
}

/// The details of an app that can't be tested; the shot shows the button off and what to do.
fn no_test_offered(qa: &mut Qa<'_>, name: &str, shot: &str) -> SandboxResult {
    let state = open_details(qa, name)?;
    ensure(state["dialog"]["test"].is_null() && state["dialog"]["testing"] == false, || {
        format!("{name} was tested: {}", state["dialog"])
    })?;
    qa.shots(shot)
}

// --- the child: the app on the real server ---

fn png(width: u32, height: u32) -> SandboxResult<Vec<u8>> {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(width, height, image::Rgba([79, 82, 178, 255]))
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    Ok(bytes)
}

fn zip_of(files: &[(&str, Vec<u8>)]) -> SandboxResult<Vec<u8>> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, bytes) in files {
        zip.start_file(*name, options)?;
        zip.write_all(bytes)?;
    }
    Ok(zip.finish()?.into_inner())
}

fn manifest() -> Value {
    json!({
        "manifestVersion": "1.17", "version": "1.0.0", "id": PACKAGE_APP,
        "developer": {"name": "Synthetic Tools", "websiteUrl": "https://tools.example.test",
                      "privacyUrl": "https://tools.example.test/privacy", "termsOfUseUrl": "https://tools.example.test/terms"},
        "icons": {"color": "color.png", "outline": "outline.png"},
        "name": {"short": PACKAGE_NAME},
        "description": {"short": "Quick polls", "full": "Synthetic polls."},
        "accentColor": "#C239B3",
        "bots": [{"botId": PACKAGE_BOT, "scopes": ["personal", "team"]}],
    })
}

/// The package of the case: a good one, one over 5 MB, bytes that are no zip, a zip without a manifest, a zip whose
/// colour icon is the wrong size.
fn package(case: &str) -> SandboxResult<Vec<u8>> {
    let manifest = serde_json::to_vec(&manifest())?;
    match case {
        "good" => zip_of(&[
            ("manifest.json", manifest),
            ("color.png", png(192, 192)?),
            ("outline.png", png(32, 32)?),
        ]),
        "large" => Ok(vec![7; 5 * 1024 * 1024 + 1]),
        "broken" => Ok(b"this is not a zip file, only made-up text".to_vec()),
        "no-manifest" => zip_of(&[("color.png", png(192, 192)?), ("outline.png", png(32, 32)?)]),
        "bad-icon" => zip_of(&[
            ("manifest.json", manifest),
            ("color.png", png(50, 50)?),
            ("outline.png", png(32, 32)?),
        ]),
        other => Err(format!("no package {other}").into()),
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

fn env(name: &str) -> SandboxResult<String> {
    std::env::var(name).map_err(|_| format!("no {name}").into())
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let sandbox_dir = PathBuf::from(std::env::var_os(ui_sandbox::SANDBOX_ENV).ok_or("no sandbox dir")?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let config = teams_log::Config {
        dir: dir.join("log"),
        setting: teams_log::Level::Debug,
        env: None,
        rotation: teams_log::file::Rotation::default(),
    };
    teams_log::install(&config).map_err(|error| format!("the app's log: {error}"))?;
    let paths = PersonalPaths::new(dir.join("config"), dir.join("data"));
    let setup = Setup::new(paths)?.with_secrets(AppSecrets::in_memory());
    let address = OwnAddress::parse(&env(ORIGIN_ENV)?, &env(CA_ENV)?)?;
    let account = ngteams::accounts::add_own(&setup, &env(UPN_ENV)?, &address, &Secret::new(PASSWORD))?;
    let engine = ngteams::engine::start(&setup.paths, &account, CallFloor::new(), setup.secrets(), true, None)?;
    let first = Started {
        account,
        engine,
        calls: None,
    };
    let runner = Runner {
        setup,
        floor: CallFloor::new(),
        devices: None,
    };
    let settings = teams_ui::SettingsFile::new(dir.join("settings.toml"));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
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
                answer(launched.desktop, sandbox_dir.join(ASK), cx);
            }
            Err(failed) => {
                *failed_in_app.lock().unwrap_or_else(PoisonError::into_inner) = Some(failed.error);
                app::quit_after(failed.stopped, cx);
            }
        }
    });
    if let Some(log) = teams_log::installed() {
        let _ = log.flush_written();
    }
    if let Some(error) = failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
        return Err(error.into());
    }
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

/// Answers each question the parent writes to `ask`, one `answer <json>` line each.
fn answer(desktop: Entity<teams_ui::Desktop>, ask: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(50)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let question = question.trim_end_matches('\n').to_owned();
            if question == "quit" {
                if let Some(log) = teams_log::installed() {
                    let _ = log.flush_written();
                }
                say("answer {}");
                cx.update(|cx| cx.quit());
                return;
            }
            let (verb, rest) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            if verb == "server" {
                let reply = server_apps(&desktop, cx).await;
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

fn mode_key(mode: BotAuthMode) -> &'static str {
    match mode {
        BotAuthMode::Signed => "signed",
        BotAuthMode::Unsigned => "unsigned",
    }
}

fn app_json(app: &teams_auth::admin::AdminApp) -> Value {
    let bot = app.bot.as_ref();
    json!({
        "name": app.name, "id": app.id, "enabled": app.enabled, "everyone": app.everyone, "users": app.users,
        "bot": bot.map(|bot| bot.id.clone()), "endpoint": bot.and_then(|bot| bot.endpoint.clone()),
        "mode": bot.map(|bot| mode_key(bot.auth_mode)),
    })
}

/// The server's apps as the administrator's own service reads them now, by name.
async fn server_apps(desktop: &Entity<teams_ui::Desktop>, cx: &mut AsyncApp) -> Value {
    let admin = cx.update(|cx| {
        let accounts = desktop.read(cx).accounts().read(cx);
        accounts.tiles().first().and_then(|tile| tile.app_admin.clone())
    });
    let Some(admin) = admin else {
        return json!({"error": "no app admin"});
    };
    let apps = cx.background_executor().spawn(async move { admin.apps() }).await;
    match apps {
        Ok(apps) => {
            let by_name: serde_json::Map<String, Value> =
                apps.iter().map(|app| (app.name.clone(), app_json(app))).collect();
            json!({"apps": by_name})
        }
        Err(problem) => json!({"error": problem.kind()}),
    }
}

fn page_of(shell: &Entity<Shell>, cx: &App) -> Option<Entity<ManageApps>> {
    shell.read(cx).settings_view().read(cx).apps().cloned()
}

fn dialog_of(shell: &Entity<Shell>, cx: &App) -> Option<Entity<AppDialog>> {
    page_of(shell, cx)?.read(cx).dialog().cloned()
}

fn dialog_json(dialog: &Entity<AppDialog>, cx: &App) -> Value {
    let read = dialog.read(cx);
    let settings = read.settings();
    json!({
        "kind": read.kind().key(),
        "problem": read.problem().map(ToString::to_string),
        "can_confirm": read.can_confirm(cx),
        "secret_shown": read.shows_secret(),
        "secret": shown_secret(read),
        "app_id_shown": read.app().and_then(|app| app.bot.as_ref().map(|bot| bot.id.clone())),
        "users": read.users(),
        "found": read.found().iter().map(|person| person.upn.clone()).collect::<Vec<_>>(),
        "sdk": read.sdk().key(),
        "settings_have_secret": settings.as_ref().is_some_and(|settings| !settings.text.contains(teams_ui::app_admin::sdk::SECRET_PLACEHOLDER)),
        "note": settings.as_ref().and_then(|settings| settings.note),
        "settings": settings.map(|settings| settings.text),
        "health": read.app().map(|app| teams_ui::app_admin::health::summary(app.health.as_ref(), 0).0),
        "testing": read.testing(),
        "test": read.test_result().map(|result| {
            let (words, took) = teams_ui::app_admin::health::went(result);
            json!({"words": words, "took": took})
        }),
    })
}

fn state_of(shell: &Entity<Shell>, cx: &App) -> Value {
    let view = shell.read(cx).settings_view().read(cx);
    let pages: Vec<&str> = [Page::Users, Page::Apps]
        .into_iter()
        .filter(|page| view.offers(*page, cx))
        .map(Page::key)
        .collect();
    let page = page_of(shell, cx);
    let apps: Vec<Value> = page
        .as_ref()
        .map(|page| page.read(cx).apps().iter().map(app_json).collect())
        .unwrap_or_default();
    let shown: Vec<String> = page
        .as_ref()
        .map(|page| {
            page.read(cx)
                .shown_apps(cx)
                .iter()
                .map(|app| app.name.clone())
                .collect()
        })
        .unwrap_or_default();
    let loaded = page.as_ref().is_some_and(|page| !page.read(cx).loading());
    let list_problem = page
        .as_ref()
        .and_then(|page| page.read(cx).problem().map(teams_ui::app_admin::AppProblem::kind));
    let dialog = dialog_of(shell, cx).map_or(Value::Null, |dialog| dialog_json(&dialog, cx));
    let users: Vec<String> = view
        .users()
        .map(|users| users.read(cx).users().iter().map(|user| user.upn.clone()).collect())
        .unwrap_or_default();
    json!({"pages": pages, "page": view.page().key(), "apps": apps, "shown": shown, "loaded": loaded,
           "list_problem": list_problem, "dialog": dialog, "users": users})
}

fn reply_to(verb: &str, rest: &str, shell: &Entity<Shell>, window: &mut Window, cx: &mut App) -> Value {
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
        "do" => {
            let (what, arg) = rest.split_once(' ').unwrap_or((rest, ""));
            act(what, arg, shell, window, cx).map_or_else(|error| json!({"error": error}), |()| json!({"done": what}))
        }
        _ => json!({"error": format!("unknown question {verb}")}),
    }
}

fn kind_of(key: &str) -> Option<DialogKind> {
    [
        DialogKind::Access,
        DialogKind::NewSecret,
        DialogKind::Delete,
        DialogKind::Settings,
        DialogKind::Details,
    ]
    .into_iter()
    .find(|kind| kind.key() == key)
}

/// Does `what` in the window, as the administrator would.
fn act(what: &str, arg: &str, shell: &Entity<Shell>, window: &mut Window, cx: &mut App) -> Result<(), String> {
    match what {
        "settings" => {
            shell.update(cx, |shell, cx| shell.open_settings_page(Page::Apps, window, cx));
            return Ok(());
        }
        "leave" => {
            shell.update(cx, |shell, cx| shell.open_settings_page(Page::General, window, cx));
            return Ok(());
        }
        "users" => {
            shell.update(cx, |shell, cx| shell.open_settings_page(Page::Users, window, cx));
            return Ok(());
        }
        _ => {}
    }
    let page = page_of(shell, cx).ok_or("no Manage apps page")?;
    match what {
        "register" => page.update(cx, |page, cx| page.open(DialogKind::Register, None, window, cx)),
        "upload" => page.update(cx, |page, cx| page.open(DialogKind::Upload, None, window, cx)),
        "reload" => page.update(cx, ManageApps::reload),
        "find" => {
            let field = page.read(cx).search().clone();
            let query = arg.to_owned();
            field.update(cx, |field, cx| field.set_value(query, window, cx));
            page.update(cx, |_, cx| cx.notify());
        }
        "open" => {
            let app = page
                .read(cx)
                .apps()
                .iter()
                .find(|app| app.name == arg)
                .cloned()
                .ok_or_else(|| format!("no app {arg}"))?;
            page.update(cx, |page, cx| page.open(DialogKind::Details, Some(app), window, cx));
        }
        "close" => {
            if let Some(dialog) = page.read(cx).dialog().cloned() {
                dialog.update(cx, |_, cx| cx.emit(teams_ui::app_admin::AppDialogEvent::Closed));
            }
        }
        _ => {
            let dialog = page.read(cx).dialog().cloned().ok_or("no dialog")?;
            dialog_act(what, arg, &dialog, window, cx)?;
        }
    }
    Ok(())
}

fn dialog_act(
    what: &str,
    arg: &str,
    dialog: &Entity<AppDialog>,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), String> {
    match what {
        "confirm" => dialog.update(cx, AppDialog::confirm),
        "go" => {
            let kind = kind_of(arg).ok_or_else(|| format!("no step {arg}"))?;
            dialog.update(cx, |dialog, cx| dialog.go(kind, window, cx));
        }
        "sdk" => {
            let sdk = Sdk::ALL.into_iter().find(|sdk| sdk.key() == arg).ok_or("no such SDK")?;
            dialog.update(cx, |dialog, cx| dialog.set_sdk(sdk, cx));
        }
        "package" => {
            let bytes = package(arg).map_err(|error| error.to_string())?;
            dialog.update(cx, |dialog, cx| dialog.choose_package(format!("{arg}.zip"), bytes, cx));
        }
        "enabled" => dialog.update(cx, |dialog, cx| dialog.set_enabled(arg == "on", cx)),
        "test" => dialog.update(cx, AppDialog::send_test),
        "mode" => {
            let mode = if arg == "unsigned" {
                BotAuthMode::Unsigned
            } else {
                BotAuthMode::Signed
            };
            dialog.update(cx, |dialog, cx| dialog.set_mode(mode, cx));
        }
        "fill" => {
            let mut parts = arg.splitn(3, '|');
            let (name, description, endpoint) = (
                parts.next().unwrap_or_default(),
                parts.next().unwrap_or_default(),
                parts.next().unwrap_or_default(),
            );
            dialog.update(cx, |dialog, cx| dialog.fill(name, description, endpoint, window, cx));
        }
        "endpoint" => dialog.update(cx, |dialog, cx| dialog.fill("", "", arg, window, cx)),
        "who" => dialog.update(cx, |dialog, cx| dialog.set_everyone(arg == "everyone", cx)),
        "search" => dialog.update(cx, |dialog, cx| dialog.search_for(arg, window, cx)),
        "add" => {
            let person = dialog
                .read(cx)
                .found()
                .iter()
                .find(|person| person.upn == arg)
                .cloned()
                .ok_or_else(|| format!("{arg} not found"))?;
            dialog.update(cx, |dialog, cx| dialog.add_person(&person, cx));
        }
        _ => return Err(format!("unknown action {what}")),
    }
    Ok(())
}
