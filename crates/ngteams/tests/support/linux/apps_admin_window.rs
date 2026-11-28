//! Manage apps as an own server's administrator meets it, in the UI sandbox (`harness = false`:
//! GPUI runs on the process's main thread).
//!
//! The test runs itself again inside the sandbox as the app of the administrator of an in-process `ngteams-server`
//! (TLS on loopback, SQLite, a made-up user beside the administrator), signed in with the password grant. The parent
//! types with the sandbox's virtual keyboard where the keyboard is the point (the register form, Escape), and asks
//! the child for the rest (a dialog's step, a choice) and for "what does the window hold now". Each action of the
//! page goes to the server and is checked against the server's own list: register a bot (its secret once, its SDK
//! settings), upload a package, turn an app off and change its endpoint, choose who may use it, a new secret, a
//! deletion. The shots go to `target/ui-shots/apps-admin-*.png`, each in Teams Light and Dark.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`. Every app, account
//! and value is made up; nothing leaves loopback.

use std::io::{Cursor, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
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
use teams_ui::app_admin::{AppDialog, DialogKind, ManageApps};
use teams_ui::settings_view::Page;
use teams_ui::shell::Shell;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "../own_server.rs"]
#[expect(
    dead_code,
    reason = "the own server's tests share this support; this one reads its accounts through the app"
)]
mod own_server;

const CHILD_ENV: &str = "NGTEAMS_APPS_ADMIN_TEST";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(30);
const READY_FRAMES: usize = 3;
const ASK: &str = "apps-admin-ask";
const SETTLE: Duration = Duration::from_millis(900);

const ADMIN: &str = "ada.apps@own.example.test";
const DANA: &str = "dana.apps@own.example.test";
const BOT_NAME: &str = "Made-up Helper";
const ENDPOINT: &str = "https://bots.example.test/api/messages";
const PACKAGE_APP: &str = "00000000-0000-4000-8000-0000000000e1";
const PACKAGE_BOT: &str = "00000000-0000-4000-8000-0000000000e2";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("apps admin window test: {error}");
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

// --- the parent: the sandbox, the keyboard, and the checks ---

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams apps admin window", &options)? else {
        return Ok(());
    };
    let outcome = run_checks(&sandbox).map_err(|error| format!("{error}{}", sandbox.logs()));
    let failed = outcome?;
    if failed > 0 {
        return Err(format!("{failed} manage apps check(s) failed").into());
    }
    say("ngteams apps admin window: ok");
    Ok(())
}

fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

struct Qa<'a> {
    sandbox: &'a Sandbox,
    running: Running,
    input: VirtualInput,
    passed: Vec<&'static str>,
    failed: Vec<(&'static str, String)>,
}

type Check = (&'static str, fn(&mut Qa<'_>) -> SandboxResult);

/// The flows, in the order an administrator meets them (each leaves the window as the next expects it).
const CHECKS: [Check; 8] = [
    ("settings_offer_manage_apps_to_the_administrator", offered),
    ("register_a_bot_from_the_keyboard_shows_its_secret_once", register),
    ("the_bots_sdk_settings_name_this_server", sdk_settings),
    ("upload_a_package_adds_its_app_and_bot", upload),
    ("turn_an_app_off_and_change_its_endpoint", details),
    ("choose_who_may_use_an_app", access),
    ("a_new_secret_is_confirmed_and_shown_once", new_secret),
    ("delete_an_app_after_a_confirmation", delete),
];

fn run_checks(sandbox: &Sandbox) -> SandboxResult<usize> {
    let mut qa = Qa::start(sandbox)?;
    for (name, check) in CHECKS {
        qa.check(name, check);
    }
    let failed = qa.failed.clone();
    let passed = qa.passed.len();
    qa.quit()?;
    for (name, error) in &failed {
        say(&format!("FAILED {name}: {error}"));
    }
    say(&format!("manage apps checks: {passed} passed, {} failed", failed.len()));
    Ok(failed.len())
}

impl<'a> Qa<'a> {
    fn start(sandbox: &'a Sandbox) -> SandboxResult<Self> {
        let vars = [(CHILD_ENV, "app")];
        let child = sandbox.spawn_with_fds(&std::env::current_exe()?, &[], &vars, &[])?;
        let mut running = Running::new(child)?;
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
        Ok(Qa {
            sandbox,
            running,
            input: sandbox.input()?,
            passed: Vec::new(),
            failed: Vec::new(),
        })
    }

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
                let _ = self.ask("do close");
                let _ = self.ask("theme light");
            }
        }
    }

    fn ask(&self, question: &str) -> SandboxResult<Value> {
        let staged = self.sandbox.path("apps-admin-ask.staged");
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

    fn act(&self, what: &str) -> SandboxResult<Value> {
        let answer = self.ask(&format!("do {what}"))?;
        thread::sleep(SETTLE);
        Ok(answer)
    }

    fn state_until(&self, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let mut last = Value::Null;
        let found = wait_until(WAIT, || match self.ask("state") {
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

    /// What the server holds now, by app name: `{"<name>": {"enabled", "everyone", "users", "endpoint"}}`.
    fn server(&self) -> SandboxResult<Value> {
        Ok(self.ask("server")?["apps"].clone())
    }

    fn shot(&self, name: &str) -> SandboxResult<PathBuf> {
        thread::sleep(SETTLE);
        let file = format!("apps-admin-{name}.png");
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

    fn quit(self) -> SandboxResult {
        let _ = self.ask("quit");
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("the app exited {status}: {stderr}").into());
        }
        Ok(())
    }
}

fn dialog_is(state: &Value, kind: &str) -> bool {
    state["dialog"]["kind"] == kind
}

// --- the flows ---

fn offered(qa: &mut Qa<'_>) -> SandboxResult {
    let state = qa.state_until("Manage apps offered", |state| {
        state["pages"]
            .as_array()
            .is_some_and(|pages| pages.iter().any(|page| page == "apps"))
    })?;
    ensure(
        state["pages"]
            .as_array()
            .is_some_and(|pages| pages.iter().any(|page| page == "users")),
        || format!("Manage users isn't beside it: {state}"),
    )?;
    qa.act("settings")?;
    qa.state_until("the page, read", |state| {
        state["page"] == "apps" && state["loaded"] == true
    })?;
    qa.shots("empty")
}

fn register(qa: &mut Qa<'_>) -> SandboxResult {
    qa.act("register")?;
    qa.state_until("the register form", |state| dialog_is(state, "register"))?;
    // the name has the keyboard; Tab walks to the description and the endpoint; Enter registers
    qa.type_text(BOT_NAME)?;
    qa.key(keys::TAB)?;
    qa.type_text("Answers made-up questions")?;
    qa.key(keys::TAB)?;
    qa.type_text(ENDPOINT)?;
    qa.shots("register")?;
    qa.key(keys::ENTER)?;
    let state = qa.state_until("the bot made", |state| dialog_is(state, "made"))?;
    ensure(state["dialog"]["secret_shown"] == true, || {
        format!("no secret shown: {state}")
    })?;
    let server = qa.server()?;
    ensure(server[BOT_NAME]["endpoint"] == ENDPOINT, || {
        format!("the server holds {server}")
    })?;
    qa.shots("registered")
}

fn sdk_settings(qa: &mut Qa<'_>) -> SandboxResult {
    qa.act("go settings")?;
    let state = qa.state_until("the settings, read", |state| {
        dialog_is(state, "settings") && state["dialog"]["settings"].is_string()
    })?;
    let text = state["dialog"]["settings"].as_str().unwrap_or_default();
    ensure(
        text.contains("/botframework.com") && text.contains("/bot/v1/.well-known/openidconfiguration"),
        || format!("the settings name another server: {text}"),
    )?;
    ensure(state["dialog"]["settings_have_secret"] == true, || {
        "the settings shown right after the bot was made lack its secret".to_owned()
    })?;
    qa.shots("sdk-dotnet")?;
    qa.act("sdk javascript")?;
    qa.shots("sdk-javascript")?;
    qa.act("close")?;
    let state = qa.state_until("the dialog closed", |state| state["dialog"].is_null())?;
    ensure(state["apps"].as_array().is_some_and(|apps| apps.len() == 1), || {
        format!("{state}")
    })?;
    qa.shots("one-app")
}

fn upload(qa: &mut Qa<'_>) -> SandboxResult {
    qa.act("upload")?;
    qa.act("package")?;
    qa.shots("upload")?;
    qa.act("confirm")?;
    qa.state_until("the package's app made", |state| dialog_is(state, "made"))?;
    qa.shots("uploaded")?;
    qa.key(keys::ESC)?;
    let state = qa.state_until("two apps listed", |state| {
        state["dialog"].is_null() && state["apps"].as_array().is_some_and(|apps| apps.len() == 2)
    })?;
    let server = qa.server()?;
    ensure(server["Polls (synthetic)"].is_object(), || {
        format!("the server holds {server}; {state}")
    })
}

fn details(qa: &mut Qa<'_>) -> SandboxResult {
    qa.act(&format!("open {BOT_NAME}"))?;
    qa.state_until("the details", |state| dialog_is(state, "details"))?;
    qa.shots("details")?;
    qa.act("enabled off")?;
    qa.act("endpoint https://bots.example.test/v2")?;
    qa.act("confirm")?;
    qa.state_until("the details closed", |state| state["dialog"].is_null())?;
    let server = qa.server()?;
    ensure(
        server[BOT_NAME]["enabled"] == false && server[BOT_NAME]["endpoint"] == "https://bots.example.test/v2",
        || format!("the server holds {server}"),
    )?;
    qa.shots("app-off")
}

fn access(qa: &mut Qa<'_>) -> SandboxResult {
    qa.act(&format!("open {BOT_NAME}"))?;
    qa.act("go access")?;
    qa.act("who specific")?;
    qa.act("search dana")?;
    qa.state_until("Dana found", |state| state["dialog"]["found"] == json!([DANA]))?;
    qa.act(&format!("add {DANA}"))?;
    qa.shots("access")?;
    qa.act("confirm")?;
    qa.state_until("back on the details", |state| dialog_is(state, "details"))?;
    let server = qa.server()?;
    ensure(
        server[BOT_NAME]["everyone"] == false
            && server[BOT_NAME]["users"]
                .as_array()
                .is_some_and(|users| users.len() == 1),
        || format!("the server holds {server}"),
    )?;
    qa.shots("details-specific")?;
    qa.key(keys::ESC)?;
    qa.state_until("closed", |state| state["dialog"].is_null()).map(|_| ())
}

fn new_secret(qa: &mut Qa<'_>) -> SandboxResult {
    qa.act(&format!("open {BOT_NAME}"))?;
    qa.act("go new-secret")?;
    qa.shots("new-secret-confirm")?;
    qa.act("confirm")?;
    let state = qa.state_until("the new secret", |state| dialog_is(state, "made"))?;
    ensure(state["dialog"]["secret_shown"] == true, || format!("{state}"))?;
    qa.shots("new-secret")?;
    qa.key(keys::ENTER)?;
    qa.state_until("closed", |state| state["dialog"].is_null()).map(|_| ())
}

fn delete(qa: &mut Qa<'_>) -> SandboxResult {
    qa.act(&format!("open {BOT_NAME}"))?;
    qa.act("go delete")?;
    qa.shots("delete-confirm")?;
    qa.act("confirm")?;
    qa.state_until("one app left", |state| {
        state["dialog"].is_null() && state["apps"].as_array().is_some_and(|apps| apps.len() == 1)
    })?;
    let server = qa.server()?;
    ensure(server[BOT_NAME].is_null(), || {
        format!("the server still holds {server}")
    })?;
    qa.shots("after-delete")
}

// --- the child: the app on the in-process server ---

fn png(side: u32) -> SandboxResult<Vec<u8>> {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(side, side, image::Rgba([79, 82, 178, 255]))
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    Ok(bytes)
}

/// A synthetic Teams app package with one bot.
fn package() -> SandboxResult<Vec<u8>> {
    let manifest = json!({
        "manifestVersion": "1.17", "version": "1.0.0", "id": PACKAGE_APP,
        "developer": {"name": "Synthetic Tools", "websiteUrl": "https://tools.example.test",
                      "privacyUrl": "https://tools.example.test/privacy", "termsOfUseUrl": "https://tools.example.test/terms"},
        "icons": {"color": "color.png", "outline": "outline.png"},
        "name": {"short": "Polls (synthetic)"},
        "description": {"short": "Quick polls", "full": "Synthetic polls."},
        "accentColor": "#C239B3",
        "bots": [{"botId": PACKAGE_BOT, "scopes": ["personal"]}],
    });
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, bytes) in [
        ("manifest.json", serde_json::to_vec(&manifest)?),
        ("color.png", png(192)?),
        ("outline.png", png(32)?),
    ] {
        zip.start_file(name, options)?;
        zip.write_all(&bytes)?;
    }
    Ok(zip.finish()?.into_inner())
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

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let sandbox_dir = PathBuf::from(std::env::var_os(ui_sandbox::SANDBOX_ENV).ok_or("no sandbox dir")?);
    let server = own_server::OwnServer::start()?;
    server.add_admin(ADMIN, "Ada Admin")?;
    server.add_user(DANA, "Dana Doe")?;
    let dir = tempfile::tempdir()?;
    let paths = PersonalPaths::new(dir.path().join("config"), dir.path().join("data"));
    let setup = Setup::new(paths)?.with_secrets(AppSecrets::in_memory());
    let address = OwnAddress::parse(&server.origin, &server.ca_file.display().to_string())?;
    let account = ngteams::accounts::add_own(&setup, ADMIN, &address, &Secret::new(own_server::PASSWORD))?;
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
    let settings = teams_ui::SettingsFile::new(dir.path().join("settings.toml"));
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
    if let Some(error) = failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
        return Err(error.into());
    }
    drop(server);
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

/// The server's apps as the administrator's own service reads them, by name.
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
            let by_name: serde_json::Map<String, Value> = apps
                .into_iter()
                .map(|app| {
                    let endpoint = app.bot.as_ref().and_then(|bot| bot.endpoint.clone());
                    let held = json!({"enabled": app.enabled, "everyone": app.everyone, "users": app.users,
                                      "endpoint": endpoint});
                    (app.name, held)
                })
                .collect();
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

fn state_of(shell: &Entity<Shell>, cx: &App) -> Value {
    let view = shell.read(cx).settings_view().read(cx);
    let pages: Vec<&str> = [Page::Users, Page::Apps]
        .into_iter()
        .filter(|page| view.offers(*page, cx))
        .map(Page::key)
        .collect();
    let page = page_of(shell, cx);
    let apps: Vec<String> = page
        .as_ref()
        .map(|page| page.read(cx).apps().iter().map(|app| app.name.clone()).collect())
        .unwrap_or_default();
    let loaded = page.as_ref().is_some_and(|page| !page.read(cx).loading());
    let dialog = dialog_of(shell, cx).map_or(Value::Null, |dialog| {
        let dialog = dialog.read(cx);
        let settings = dialog.settings();
        json!({
            "kind": dialog.kind().key(),
            "problem": dialog.problem().map(ToString::to_string),
            "secret_shown": dialog.shows_secret(),
            "found": dialog.found().iter().map(|person| person.upn.clone()).collect::<Vec<_>>(),
            "settings_have_secret": settings.as_ref().is_some_and(|settings| !settings.text.contains(teams_ui::app_admin::sdk::SECRET_PLACEHOLDER)),
            "settings": settings.map(|settings| settings.text),
        })
    });
    json!({"pages": pages, "page": view.page().key(), "apps": apps, "loaded": loaded, "dialog": dialog})
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
            act(what, arg, shell, window, cx).map_or_else(|error| json!({"error": error}), |()| json!({"done": rest}))
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
    if what == "settings" {
        shell.update(cx, |shell, cx| shell.open_settings_page(Page::Apps, window, cx));
        return Ok(());
    }
    let page = page_of(shell, cx).ok_or("no Manage apps page")?;
    match what {
        "register" => page.update(cx, |page, cx| page.open(DialogKind::Register, None, window, cx)),
        "upload" => page.update(cx, |page, cx| page.open(DialogKind::Upload, None, window, cx)),
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
            if let Some(dialog) = dialog_of(shell, cx) {
                dialog.update(cx, |_, cx| cx.emit(teams_ui::app_admin::AppDialogEvent::Closed));
            }
        }
        _ => {
            let dialog = dialog_of(shell, cx).ok_or("no dialog")?;
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
            let bytes = package().map_err(|error| error.to_string())?;
            dialog.update(cx, |dialog, cx| {
                dialog.choose_package("polls.zip".to_owned(), bytes, cx);
            });
        }
        "enabled" => dialog.update(cx, |dialog, cx| dialog.set_enabled(arg == "on", cx)),
        "mode" => {
            let mode = if arg == "unsigned" {
                BotAuthMode::Unsigned
            } else {
                BotAuthMode::Signed
            };
            dialog.update(cx, |dialog, cx| dialog.set_mode(mode, cx));
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
