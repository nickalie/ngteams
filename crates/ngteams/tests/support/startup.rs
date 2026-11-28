//! The actual startup controllers on a mock GPUI platform and empty in-memory credentials.
//! Each case owns HOME/XDG in a subprocess; no user's files, keyring, display or service is reached.

use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::{Mutex, OnceLock};

use clap::Parser as _;
use ngteams::accounts_file::PersonalAccount;
use ngteams::setup::Setup;
use teams_auth::secrets::MemorySecretStore;
use teams_core::CallFloor;

use super::{Outcome, run};

#[path = "environment.rs"]
pub(crate) mod environment;
#[path = "handles.rs"]
pub(crate) mod handles;
mod startup_controls;
#[path = "transport.rs"]
mod transport;

const CASE: &str = "NGTEAMS_STARTUP_TEST_CASE";
const TID: &str = "00000000-0000-4000-8000-000000000001";
/// A made-up own server, its user and password.
const OWN_SERVER: &str = "https://chat.example.test";
const OWN_UPN: &str = "unready-own@example.test";
const OWN_PASSWORD: &str = "a made-up password";
/// An own server's user whose tokens are there ([`accounts::ready`]).
const READY_OWN_UPN: &str = "ready-own@example.test";

pub(crate) fn calls() -> &'static Mutex<Vec<String>> {
    static CALLS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    CALLS.get_or_init(Mutex::default)
}

fn record(kind: &str, upn: &str) {
    calls().lock().unwrap().push(format!("{kind}:{upn}"));
}

pub(super) fn window_runner(runner: accounts::Runner) -> ngteams::accounts::Runner {
    ngteams::accounts::Runner {
        setup: runner.setup,
        floor: runner.floor,
        devices: runner.devices,
    }
}

pub(super) mod accounts {
    use super::*;
    use ngteams::calling::CallDevices;
    use teams_signin::window::Windows;

    pub type Started<S = MemorySecretStore> = ngteams::accounts::Started<S>;
    pub use ngteams::accounts::{StartFailed, unserved};

    pub struct Runner {
        pub setup: Setup,
        pub floor: CallFloor,
        pub devices: Option<CallDevices>,
    }

    impl Runner {
        pub fn start(&self, account: &PersonalAccount) -> Result<Started, StartFailed> {
            record("start", &account.upn);

            let engine = ngteams::engine::start(
                &self.setup.paths,
                account,
                teams_core::CallFloor::new(),
                MemorySecretStore::new(),
                false,
                None,
            )
            .map_err(|error| ngteams::accounts::StartFailed::of(account, &error))?;
            Ok(Started {
                account: account.clone(),
                engine,
                calls: None,
            })
        }
    }

    pub fn ready(_: &Setup, account: &PersonalAccount) -> Result<bool, String> {
        record("ready", &account.upn);
        if account.upn.starts_with("broken-ready") {
            return Err("synthetic readiness failure".to_owned());
        }
        Ok(account.upn.starts_with("ready"))
    }

    pub fn add(setup: &Setup, upn: &str, _: &Windows) -> Result<PersonalAccount, String> {
        record("add", upn);
        if std::env::var(CASE).unwrap().starts_with("first-lab-") {
            let account = setup.ask_for(upn).unwrap();
            return crate::lab_startup_tests::failed_sign_in(setup, &account, true);
        }
        if std::env::var(CASE).unwrap().ends_with("error") {
            return Err("synthetic sign-in failure".to_owned());
        }
        setup.ask_for(upn).map_err(|error| error.to_string())
    }

    pub fn sign_in(setup: &Setup, account: &PersonalAccount, _: &Windows) -> Result<PersonalAccount, String> {
        record("sign-in", &account.upn);
        if std::env::var(CASE).unwrap().starts_with("first-lab-") {
            return crate::lab_startup_tests::failed_sign_in(setup, account, false);
        }
        if std::env::var(CASE).unwrap().ends_with("success") {
            Ok(account.clone())
        } else {
            Err("synthetic reauthentication failure".to_owned())
        }
    }

    pub fn add_own(
        setup: &Setup,
        upn: &str,
        server: &ngteams::accounts_file::OwnServer,
        password: &teams_auth::secrets::Secret,
    ) -> Result<PersonalAccount, String> {
        record("add-own", &format!("{}/{upn}", server.origin));
        assert_eq!(password.expose(), super::OWN_PASSWORD);
        if std::env::var(CASE).unwrap().ends_with("error") {
            return Err("synthetic own sign-in failure".to_owned());
        }
        if std::env::var(CASE).unwrap().ends_with("unserved") {
            // signed in on a server whose authz named no chat service
            return Ok(super::signed_own_as(setup, upn));
        }
        setup.ask_for_on(upn, server).map_err(|error| error.to_string())
    }

    pub fn sign_in_own(
        _: &Setup,
        account: &PersonalAccount,
        password: &teams_auth::secrets::Secret,
    ) -> Result<PersonalAccount, String> {
        record("sign-in-own", &account.id());
        assert_eq!(password.expose(), super::OWN_PASSWORD);
        if std::env::var(CASE).unwrap().ends_with("error") {
            return Err("synthetic own reauthentication failure".to_owned());
        }
        Ok(account.clone())
    }

    /// Every server of these cases has its administrator, and none is asked.
    #[expect(clippy::unnecessary_wraps, reason = "the signature of the app's own")]
    pub fn needs_admin(_: &ngteams::accounts_file::OwnServer) -> Result<Option<bool>, String> {
        Ok(None)
    }

    /// The server asked for its authz again: in the `served-now` case it names a chat service by now.
    pub fn region_again(setup: &Setup, account: &PersonalAccount) -> Result<(), String> {
        record("region-again", &account.upn);
        if !std::env::var(CASE).unwrap().ends_with("served-now") {
            return Err("synthetic authz failure".to_owned());
        }
        std::fs::write(
            setup.paths.region_file(account).unwrap(),
            format!(
                r#"{{"_synthetic":true,"region":"own","partition":null,"regionGtms":{{"chatServiceAfd":"{}/api/chatsvc/own"}}}}"#,
                super::OWN_SERVER
            ),
        )
        .unwrap();
        Ok(())
    }

    pub fn forget(_: &Setup, upn: &str) -> Result<ngteams::forget::Forgot, String> {
        record("forget", upn);
        match upn {
            "missing@example.test" => Ok(ngteams::forget::Forgot::Nothing),
            "pending@example.test" => Ok(ngteams::forget::Forgot::Account {
                upn: upn.to_owned(),
                store: None,
            }),
            "stored@example.test" => Ok(ngteams::forget::Forgot::Account {
                upn: upn.to_owned(),
                store: Some("synthetic-store.sqlite".into()),
            }),
            _ => Err("synthetic forget failure".to_owned()),
        }
    }
}

pub(super) mod app {
    use gpui::AppContext as _;
    pub(crate) use ngteams::app::{TITLE, install_log, open_link, quit, quit_after, settings};

    pub(crate) fn launch<S: teams_auth::secrets::SecretStore + 'static>(
        cx: &mut gpui::App,
        launch: ngteams::app::Launch<S>,
    ) -> Result<ngteams::app::Launched, ngteams::app::LaunchFailed> {
        if std::env::var(super::CASE).unwrap().ends_with("window-error") {
            let stopped = cx.background_spawn(async move {
                for account in std::iter::once(launch.first).chain(launch.others) {
                    account.engine.handle.shutdown();
                }
            });
            return Err(ngteams::app::LaunchFailed {
                error: "synthetic window failure".to_owned(),
                stopped,
            });
        }
        let ctl = launch.ctl.clone();
        let launched = ngteams::app::launch(cx, launch)?;
        // where the control socket was bound, if one was asked for, while the app runs
        if let Some(path) = ctl {
            let bound = super::transport::connect(&path).is_ok();
            super::record(
                if bound { "ctl-bound" } else { "ctl-unbound" },
                &path.display().to_string(),
            );
        }
        Ok(launched)
    }
}

pub(super) mod prompt {
    pub(crate) fn open_offering(
        cx: &mut gpui::App,
        app_id: &str,
        title: gpui::SharedString,
        held: Option<&str>,
        own: &teams_ui::OwnOffer,
    ) -> gpui::Result<(
        gpui::WindowHandle<teams_ui::window::KitRoot>,
        gpui::Entity<teams_ui::AccountPrompt>,
    )> {
        if std::env::var(super::CASE).unwrap() == "prompt-window-error" {
            return Err(std::io::Error::other("synthetic prompt window failure").into());
        }
        teams_ui::account_prompt::open_offering(cx, app_id, title, held, own)
    }
}

pub(super) mod platform {
    use std::time::{Duration, Instant};

    use gpui::{App, TestAppContext};
    use teams_ui::account_prompt::{AccountPrompt, AccountPromptEvent};
    use teams_ui::window::KitRoot;

    pub struct Platform {
        cx: TestAppContext,
    }

    pub fn application() -> Platform {
        Platform {
            cx: TestAppContext::single(),
        }
    }

    impl Platform {
        /// The test context draws nothing, so it needs no assets.
        pub fn with_assets(self, _: impl gpui::AssetSource) -> Self {
            self
        }

        pub fn run(self, launch: impl FnOnce(&mut App) + 'static) {
            let cx = self.cx;
            cx.update(launch);
            cx.run_until_parked();
            let case = std::env::var(super::CASE).unwrap();
            if case == "unserved-own" {
                // said on the screen at the start, before anything is typed
                said_unserved(&cx, super::READY_OWN_UPN, true);
            }
            if matches!(case.as_str(), "prompt-success" | "prompt-error") {
                submit_prompt(&cx);
            }
            if case.starts_with("own-") {
                submit_own_prompt(&cx, case.starts_with("own-prompt-"));
            }
            if matches!(case.as_str(), "prompt-success" | "prompt-error")
                || case.starts_with("own-")
                || case.starts_with("first-")
                || case.starts_with("again-")
            {
                wait_for_backend(&cx);
            }
            if case == "own-prompt-unserved" {
                // the harness typed nothing into the first run's fields
                said_unserved(&cx, super::OWN_UPN, false);
            }
            cx.update(ngteams::app::quit);
            cx.run_until_parked();
        }
    }

    fn open_prompt(cx: &TestAppContext) -> gpui::Entity<AccountPrompt> {
        cx.update(|cx| {
            cx.windows()
                .into_iter()
                .filter_map(|window| window.downcast::<KitRoot>())
                .find_map(|window| window.read(cx).ok()?.view().clone().downcast::<AccountPrompt>().ok())
                .expect("the account prompt is open")
        })
    }

    /// The sign-in screen is open and says the server of the own server's account `user` serves no chats; `held`
    /// when the screen is that account's own, its server and user name in the fields.
    fn said_unserved(cx: &TestAppContext, user: &str, held: bool) {
        let prompt = open_prompt(cx);
        let (said, fields, signing) = cx.read(|cx| {
            let prompt = prompt.read(cx);
            (
                prompt.problem().map(ToString::to_string),
                prompt.own_fields(cx),
                prompt.is_signing_in(),
            )
        });
        assert_eq!(
            said.as_deref(),
            Some(
                format!("{user} is signed in, but chat.example.test doesn't serve chats, so it can't be shown yet.")
                    .as_str()
            )
        );
        if held {
            assert_eq!(fields, (super::OWN_SERVER.to_owned(), user.to_owned()));
        }
        assert!(!signing, "the screen takes another try");
    }

    fn submit_prompt(cx: &TestAppContext) {
        let prompt = open_prompt(cx);
        assert_eq!(cx.read(|cx| prompt.read(cx).account(cx)), "pending@example.test");
        cx.update(|cx| {
            prompt.update(cx, |_, cx| {
                cx.emit(AccountPromptEvent::SignIn("pending@example.test".into()));
            });
        });
    }

    /// The sign-in screen's own server's fields submitted: all three on the first run's screen (`choice`: the place
    /// is chosen first), the password alone for an account asked again (its server and user name are there already).
    fn submit_own_prompt(cx: &TestAppContext, choice: bool) {
        use teams_ui::{OwnSignIn, SignInKind};
        let prompt = open_prompt(cx);
        let (kind, held) = cx.read(|cx| (prompt.read(cx).kind(), prompt.read(cx).own_fields(cx)));
        if choice {
            assert_eq!(
                kind,
                SignInKind::Microsoft,
                "Microsoft 365 is chosen until the user chooses"
            );
        } else {
            assert_eq!(kind, SignInKind::OwnServer);
            assert_eq!(held, (super::OWN_SERVER.to_owned(), super::OWN_UPN.to_owned()));
        }
        cx.update(|cx| {
            prompt.update(cx, |_, cx| {
                cx.emit(AccountPromptEvent::SignInOwn(OwnSignIn {
                    server: "chat.example.test".into(),
                    user: super::OWN_UPN.into(),
                    password: super::OWN_PASSWORD.to_owned().into(),
                    ca_file: String::new(),
                }));
            });
        });
    }

    fn wait_for_backend(cx: &TestAppContext) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            cx.run_until_parked();
            let complete = super::calls().lock().unwrap().iter().any(|call| {
                ["add:", "sign-in:", "add-own:", "sign-in-own:"]
                    .iter()
                    .any(|kind| call.starts_with(kind))
            });
            if complete {
                // Flush the foreground continuation after the synchronous fake backend returns.
                for _ in 0..4 {
                    std::thread::sleep(Duration::from_millis(5));
                    cx.run_until_parked();
                }
                return;
            }
            assert!(Instant::now() < deadline, "startup did not call the synthetic backend");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

fn signed(setup: &Setup, upn: &str, suffix: u8, region: bool) -> PersonalAccount {
    let account = PersonalAccount::new(upn, &format!("00000000-0000-4000-8000-{suffix:012x}"), TID).unwrap();
    account.save(&setup.paths).unwrap();
    if region {
        let path = setup.paths.region_file(&account).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            format!(
                r#"{{"_synthetic":true,"region":"amer","partition":null,"regionGtms":{{"chatServiceAfd":"{}"}}}}"#,
                teams_mock::CHAT_BASE
            ),
        )
        .unwrap();
    }
    account
}

/// An own server's account a sign-in named, which can't start without its password ([`accounts::ready`]).
fn signed_own(setup: &Setup) -> PersonalAccount {
    let server = ngteams::accounts_file::OwnServer::parse(OWN_SERVER, "").unwrap();
    let account = PersonalAccount::pending_on(OWN_UPN, &server)
        .unwrap()
        .signed_in_as("00000000-0000-4000-8000-0000000000a1", TID, None)
        .unwrap();
    account.save(&setup.paths).unwrap();
    account
}

/// The own server's account `upn` as a sign-in left it on a server whose authz named no chat service: its region
/// saved without one, so its engine can't start.
fn signed_own_as(setup: &Setup, upn: &str) -> PersonalAccount {
    let server = ngteams::accounts_file::OwnServer::parse(OWN_SERVER, "").unwrap();
    let account = PersonalAccount::pending_on(upn, &server)
        .unwrap()
        .signed_in_as("00000000-0000-4000-8000-0000000000a2", TID, None)
        .unwrap();
    account.save(&setup.paths).unwrap();
    let path = setup.paths.region_file(&account).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        r#"{"_synthetic":true,"region":"own","partition":null,"regionGtms":{}}"#,
    )
    .unwrap();
    account
}

const STARTUP_CASES: &[&str] = &[
    "command-invalid-env",
    "command-invalid-account",
    "command-error-files",
    "command-no-ready",
    "command-read-engine-error",
    "command-read-bad-accounts",
    "command-read-window-error",
    "window-error",
    "prompt-window-error",
    "command-broker",
    "command-snapshot",
    "command-bootstrap-failures",
    "command-read-refusal",
    "command-read-missing",
    "command-read-window",
    "empty",
    "pending",
    "prompt-success",
    "prompt-error",
    "first-success",
    "first-error",
    "ready",
    "ready-shortcut",
    "first-unready-success",
    "many",
    "first-start-fails",
    "broken-ready",
    "again-success",
    "again-error",
    "own-prompt-success",
    "own-prompt-error",
    "own-first-again",
    "own-first-again-error",
    "own-again-running",
    "own-prompt-unserved",
    "unserved-own",
    "unserved-own-then-ready",
    "ready-then-unserved-own",
    "unserved-own-served-now",
    "served-own",
    "previous-release",
    "damaged-accounts",
    "forget-missing",
    "forget-pending",
    "forget-stored",
    "forget-error",
    "handoff",
    "handoff-link",
    "handoff-stale",
    "handoff-starting",
];

#[test]
fn startup_controllers_use_only_the_fixture_backend_and_close_their_engines() {
    for &case in STARTUP_CASES {
        let root = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "startup_tests::child", "--nocapture", "--test-threads", "1"])
            .env_clear()
            .env(CASE, case)
            .env("HOME", root.path().join("home"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .env("XDG_RUNTIME_DIR", root.path().join("runtime"))
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}/no-user-bus.sock", root.path().display()),
            );
        if (case.starts_with("command-read-") && case != "command-read-refusal") || case == "command-invalid-env" {
            child
                .env("UI_SANDBOX_DIR", root.path().join("sandbox"))
                .env("WAYLAND_DISPLAY", root.path().join("sandbox/synthetic-wayland"));
        }
        for key in ["PATH", "LANG", "LD_LIBRARY_PATH", "LLVM_PROFILE_FILE"] {
            if let Some(value) = std::env::var_os(key) {
                child.env(key, value);
            }
        }
        environment::isolate(
            &mut child,
            root.path(),
            case == "command-invalid-env" || (case.starts_with("command-read-") && case != "command-read-refusal"),
        );
        if case == "command-invalid-env" {
            child
                .env("HOME", "relative")
                .env("XDG_CONFIG_HOME", "relative")
                .env("XDG_DATA_HOME", "relative");
        }
        #[cfg(windows)]
        if case == "command-invalid-env" {
            child.env("LOCALAPPDATA", "relative").env("USERPROFILE", "relative");
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        said_not_asked_again(case, &String::from_utf8_lossy(&output.stderr));
    }
}

/// A failed re-ask of the own server's authz is said on stderr only for an account whose region names no chat
/// service.
fn said_not_asked_again(case: &str, stderr: &str) {
    let said = stderr.contains("region wasn't asked again");
    match case {
        "unserved-own" => assert!(said, "{case}: {stderr}"),
        "served-own" => assert!(!said, "{case}: {stderr}"),
        _ => {}
    }
}

#[test]
fn child() {
    let Ok(case) = std::env::var(CASE) else {
        return;
    };
    if case == "command-invalid-env" {
        startup_controls::invalid_environment();
        return;
    }
    let setup = Setup::from_env().unwrap();
    std::fs::create_dir_all(std::env::var_os("XDG_RUNTIME_DIR").unwrap()).unwrap();
    if case.starts_with("command-") {
        startup_controls::run(&case, &setup);
        return;
    }
    if case.starts_with("handoff") {
        handoff::run(&case, &setup);
        return;
    }
    let mut args = vec!["ngteams".to_owned()];
    match case.as_str() {
        "pending" | "prompt-success" | "prompt-error" => {
            setup.ask_for("pending@example.test").unwrap();
        }
        "first-success" | "first-error" => args.extend(["--account".into(), "pending@example.test".into()]),
        "ready" | "ready-shortcut" | "window-error" => {
            signed(&setup, "ready@example.test", 2, true);
            if case == "ready-shortcut" {
                args.extend(["--account".into(), "ready@example.test".into()]);
            }
        }
        "first-unready-success" => {
            signed(&setup, "unready@example.test", 2, true);
            args.extend(["--account".into(), "unready@example.test".into()]);
        }
        "many" => {
            signed(&setup, "ready@example.test", 2, true);
            signed(&setup, "ready-failed@example.test", 3, false);
            signed(&setup, "ready-other@example.test", 4, true);
        }
        "first-start-fails" => {
            signed(&setup, "ready@example.test", 2, false);
        }
        "broken-ready" => {
            signed(&setup, "broken-ready@example.test", 2, true);
        }
        "again-success" | "again-error" => {
            signed(&setup, "ready@example.test", 2, true);
            signed(&setup, "unready@example.test", 3, true);
        }
        value if value.starts_with("forget-") => {
            args.extend([
                "--forget".into(),
                format!("{}@example.test", value.trim_start_matches("forget-")),
            ]);
        }
        "own-first-again" | "own-first-again-error" => {
            signed_own(&setup);
        }
        "own-again-running" => {
            signed(&setup, "ready@example.test", 2, true);
            signed_own(&setup);
        }
        // signed in before, on a server that serves no chats: nothing else to start
        "unserved-own" | "unserved-own-served-now" => {
            signed_own_as(&setup, READY_OWN_UPN);
        }
        // signed in before, on a server that serves chats, but whose authz can't be reached now
        "served-own" => served_own(&setup),
        "unserved-own-then-ready" | "ready-then-unserved-own" => unserved_beside_ready(&setup, &case),
        "previous-release" => previous_release(&setup),
        "damaged-accounts" => damaged_accounts(&setup),
        "empty" | "prompt-window-error" | "own-prompt-success" | "own-prompt-error" | "own-prompt-unserved" => {}
        other => panic!("unknown synthetic case {other}"),
    }
    let outcome = run(
        &ngteams::cli::Cli::try_parse_from(args).unwrap(),
        teams_media::devices::Reach::Sandbox,
    );
    check_outcome(&case, &outcome);
    seen_after(&case);
}

#[test]
fn a_restart_leaves_out_only_the_link_it_opened() {
    let args = || {
        [
            "--ctl-socket",
            "/run/synthetic.sock",
            "msteams:/l/meetup-join/synthetic",
        ]
        .map(std::ffi::OsString::from)
    };
    assert_eq!(super::restart_args(args().into_iter(), None, None), args());
    assert_eq!(
        super::restart_args(args().into_iter(), Some("msteams:/l/meetup-join/synthetic"), None),
        ["--ctl-socket", "/run/synthetic.sock"].map(std::ffi::OsString::from)
    );
}

/// The installer's `--prepare` run exits with success only when each step passed. `--prepare` parses with no command
/// and no account, and it does not go with `--build-identity`.
#[test]
fn the_installers_run_tells_a_failed_step_by_its_exit_code() {
    let step = |result: Result<(), String>| ngteams::prepare::Step {
        name: "synthetic",
        work: Box::new(move || result),
    };
    assert_eq!(super::prepare(vec![step(Ok(()))]), ExitCode::SUCCESS);
    assert_eq!(super::prepare(vec![step(Err("refused".into()))]), ExitCode::FAILURE);
    let cli = super::Cli::try_parse_from(["ngteams", "--prepare"]).unwrap();
    assert!(cli.prepare && cli.command.is_none() && cli.account.is_none());
    assert!(super::Cli::try_parse_from(["ngteams", "--prepare", "--build-identity"]).is_err());
}

/// A link macOS handed the app and it didn't open (one that launched it before the first sign-in) goes to the next
/// run's arguments, unless those keep a link of their own: a run takes one.
#[test]
fn a_restart_carries_a_given_link_it_did_not_open() {
    let given = |links: &[&str]| links.iter().map(|link| (*link).to_owned()).collect::<Vec<_>>();
    let (first, second) = (
        "msteams:/l/meetup-join/synthetic-1",
        "msteams:/l/meetup-join/synthetic-2",
    );
    assert_eq!(super::carried_link(None, None, given(&[first])).as_deref(), Some(first));
    assert_eq!(
        super::carried_link(None, None, given(&[first, second])).as_deref(),
        Some(second),
        "the most recent click"
    );
    assert_eq!(super::carried_link(None, None, Vec::new()), None);
    assert_eq!(super::carried_link(Some(first), None, given(&[second])), None);
    assert_eq!(
        super::carried_link(Some(first), Some(first), given(&[second])).as_deref(),
        Some(second)
    );
    assert_eq!(
        super::restart_args(
            ["--ctl-socket", "/run/synthetic.sock"]
                .map(std::ffi::OsString::from)
                .into_iter(),
            None,
            Some(first)
        ),
        ["--ctl-socket", "/run/synthetic.sock", first].map(std::ffi::OsString::from)
    );
}

/// An update's program starts in the folder the update names (Windows' installer, in its own folder), and with its own
/// arguments; the new version of an archive starts where this run did, with this run's arguments.
#[test]
fn an_update_starts_its_program_in_the_folder_it_names() {
    let mut seen = Vec::new();
    let mut exec = |command: &mut std::process::Command| {
        seen.push((
            command.get_program().to_owned(),
            command
                .get_args()
                .map(std::ffi::OsStr::to_os_string)
                .collect::<Vec<_>>(),
            command.get_current_dir().map(std::path::Path::to_path_buf),
        ));
        std::io::Error::other("synthetic: not started")
    };
    let installer = teams_update::Restart {
        program: PathBuf::from("/synthetic/updates/stage/ngteams-setup.exe"),
        arguments: Some(vec!["/VERYSILENT".into()]),
        directory: Some(PathBuf::from("/synthetic/updates/stage")),
    };
    assert_eq!(super::updated(installer, None, None, &mut exec), ExitCode::FAILURE);
    let archive = teams_update::Restart {
        program: PathBuf::from("/synthetic/opt/ngteams/usr/bin/ngteams"),
        arguments: None,
        directory: None,
    };
    assert_eq!(super::updated(archive, None, None, &mut exec), ExitCode::FAILURE);
    assert_eq!(
        seen[0],
        (
            "/synthetic/updates/stage/ngteams-setup.exe".into(),
            vec!["/VERYSILENT".into()],
            Some(PathBuf::from("/synthetic/updates/stage"))
        )
    );
    assert_eq!(seen[1].0, "/synthetic/opt/ngteams/usr/bin/ngteams");
    assert_eq!(seen[1].2, None);
}

/// How a case's run must have ended.
fn check_outcome(case: &str, outcome: &Result<Outcome, String>) {
    if let Err(error) = outcome {
        eprintln!("synthetic startup {case}: {error}");
    }
    match case {
        "prompt-success"
        | "first-success"
        | "first-unready-success"
        | "again-success"
        | "own-prompt-success"
        | "own-first-again"
        | "own-again-running" => {
            // no link was given: the restart leaves nothing out
            assert!(matches!(outcome, Ok(Outcome::Restart { opened: None })));
        }
        "first-error"
        | "first-start-fails"
        | "broken-ready"
        | "forget-error"
        | "window-error"
        | "prompt-window-error" => assert!(outcome.is_err()),
        "damaged-accounts" => {
            assert!(
                matches!(outcome, Err(error) if error.starts_with("accounts.toml: ")),
                "{:?}",
                outcome.as_ref().err()
            );
        }
        _ => assert!(matches!(outcome, Ok(Outcome::Quit))),
    }
}

/// The own server's account [`READY_OWN_UPN`] as a sign-in left it on a server that serves chats.
fn served_own(setup: &Setup) {
    let account = signed_own_as(setup, READY_OWN_UPN);
    std::fs::write(
        setup.paths.region_file(&account).unwrap(),
        format!(
            r#"{{"_synthetic":true,"region":"own","partition":null,"regionGtms":{{"chatServiceAfd":"{OWN_SERVER}/api/chatsvc/own"}}}}"#
        ),
    )
    .unwrap();
}

/// The unserved own server's account and a Microsoft 365 account that starts, in the order `case` names: added
/// before it, the own one doesn't hold it back; added after it, the app runs on the Microsoft 365 one.
fn unserved_beside_ready(setup: &Setup, case: &str) {
    if case == "unserved-own-then-ready" {
        signed_own_as(setup, READY_OWN_UPN);
        signed(setup, "ready@example.test", 2, true);
    } else {
        signed(setup, "ready@example.test", 2, true);
        signed_own_as(setup, READY_OWN_UPN);
    }
}

/// What a case's run did besides its outcome.
fn seen_after(case: &str) {
    let seen = calls().lock().unwrap();
    if case == "ready" {
        // the personal app's default: the runtime dir's socket
        let personal = teams_ctl::Env::from_process()
            .mode_path(&teams_ctl::Mode::Personal)
            .unwrap();
        assert!(seen.contains(&format!("ctl-bound:{}", personal.display())), "{seen:?}");
        assert!(!personal.exists(), "the quit removes the socket");
    }
    seen_own(case, &seen);
    seen_files(case, &seen);
    if case == "many" {
        assert!(
            seen.iter().any(|call| call == "start:ready-other@example.test"),
            "a later account still starts"
        );
    }
}

/// `accounts.toml` as the release before `teams-app` wrote it (made-up accounts): two Microsoft 365 accounts (the
/// first with its name and identity provider hosts), one no sign-in named, and an own server's account with its CA
/// file.
const PREVIOUS_RELEASE: &str =
    "# The accounts the personal build signs in, in the order they were added (a first sign-in adds oid and
# tid). Never a token.

[[account]]
upn = \"ready@example.test\"
oid = \"00000000-0000-4000-8000-000000000002\"
tid = \"00000000-0000-4000-8000-000000000001\"
name = \"Ready Made-Up\"
idp_hosts = [\"sso.example.test\"]

[[account]]
upn = \"pending@example.test\"
idp_hosts = []

[[account]]
upn = \"ready-own@example.test\"
oid = \"00000000-0000-4000-8000-0000000000a2\"
tid = \"00000000-0000-4000-8000-000000000001\"
idp_hosts = []
server = \"https://chat.example.test\"
ca_file = \"/synthetic/ca.pem\"

[[account]]
upn = \"ready-other@example.test\"
oid = \"00000000-0000-4000-8000-000000000004\"
tid = \"00000000-0000-4000-8000-000000000001\"
idp_hosts = []
";

/// An accounts file that does not parse.
const DAMAGED: &str = "[[account]]\nupn = \"ready@example.test\"\nnot = [valid\n";

/// The previous release's `accounts.toml` and the regions its sign-ins saved.
fn previous_release(setup: &Setup) {
    std::fs::create_dir_all(setup.paths.config_dir()).unwrap();
    std::fs::write(setup.paths.accounts_file(), PREVIOUS_RELEASE).unwrap();
    for account in setup.signed_in_accounts().unwrap() {
        let (region, chat) = if account.server.is_some() {
            ("own", format!("{OWN_SERVER}/api/chatsvc/own"))
        } else {
            ("amer", teams_mock::CHAT_BASE.to_owned())
        };
        let path = setup.paths.region_file(&account).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            format!(r#"{{"_synthetic":true,"region":"{region}","partition":null,"regionGtms":{{"chatServiceAfd":"{chat}"}}}}"#),
        )
        .unwrap();
    }
}

fn damaged_accounts(setup: &Setup) {
    std::fs::create_dir_all(setup.paths.config_dir()).unwrap();
    std::fs::write(setup.paths.accounts_file(), DAMAGED).unwrap();
}

/// What a case's run did with an accounts file it did not write: the previous release's accounts start in their order
/// and the one no sign-in named waits; a damaged file starts and asks nothing. Neither file is written.
fn seen_files(case: &str, seen: &[String]) {
    let setup = Setup::from_env().unwrap();
    let file = || std::fs::read_to_string(setup.paths.accounts_file()).unwrap();
    match case {
        "previous-release" => {
            let started: Vec<&str> = seen.iter().filter_map(|call| call.strip_prefix("start:")).collect();
            assert_eq!(
                started,
                [
                    "ready@example.test",
                    "ready-own@example.test",
                    "ready-other@example.test"
                ],
                "{seen:?}"
            );
            assert_eq!(asked_again(seen), 1, "{seen:?}");
            // the engines here hold no token, so an account signed out after its start is asked again
            assert!(
                !seen
                    .iter()
                    .any(|call| call.starts_with("add") || call.contains("pending@example.test")),
                "{seen:?}"
            );
            assert_eq!(file(), PREVIOUS_RELEASE);
        }
        "damaged-accounts" => {
            assert!(
                !seen.iter().any(|call| ["start:", "ready:", "sign-in", "add"]
                    .iter()
                    .any(|kind| call.starts_with(kind))),
                "{seen:?}"
            );
            assert_eq!(file(), DAMAGED);
        }
        _ => {}
    }
}

/// What a case's run did with an own server's account.
fn seen_own(case: &str, seen: &[String]) {
    let own = format!("{OWN_SERVER}/{OWN_UPN}");
    match case {
        // the server's address typed without its scheme is the account's HTTPS origin
        "own-prompt-success" | "own-prompt-error" | "own-prompt-unserved" => {
            assert!(seen.contains(&format!("add-own:{own}")), "{seen:?}");
            assert!(
                !seen.iter().any(|call| call.starts_with("add:")),
                "no Microsoft window: {seen:?}"
            );
        }
        // signed out with nothing else to start, or beside a running account: its password again, on its server
        "own-first-again" | "own-first-again-error" | "own-again-running" => {
            assert!(seen.contains(&format!("sign-in-own:{own}")), "{seen:?}");
            assert!(
                !seen
                    .iter()
                    .any(|call| call.starts_with("sign-in:") && call.contains(OWN_UPN)),
                "no Microsoft window for it: {seen:?}"
            );
            assert_eq!(
                seen.iter().any(|call| call == "start:ready@example.test"),
                case == "own-again-running"
            );
        }
        _ => seen_unserved(case, seen),
    }
}

/// What a case's run did with an own server's account that signed in before: its server's authz asked again.
fn seen_unserved(case: &str, seen: &[String]) {
    match case {
        // the server's authz was asked again and named no chat service still (or wasn't reached): the engine was
        // tried, and its refusal is on the screen; no sign-in is made
        "unserved-own" => {
            assert_eq!(asked_again(seen), 1, "{seen:?}");
            assert!(seen.contains(&format!("start:{READY_OWN_UPN}")), "{seen:?}");
            assert!(!seen.iter().any(|call| call.starts_with("sign-in-own:")), "{seen:?}");
        }
        // the server serves chats by now; or served them already, asked again for the services it has now, and
        // can't be asked (the account starts on the region it has): asked once, the account opens, no password
        "unserved-own-served-now" | "served-own" => {
            assert_eq!(asked_again(seen), 1, "{seen:?}");
            assert!(seen.contains(&format!("start:{READY_OWN_UPN}")), "{seen:?}");
            assert!(seen.iter().any(|call| call.starts_with("ctl-bound:")), "{seen:?}");
            assert!(!seen.iter().any(|call| call.starts_with("sign-in-own:")), "{seen:?}");
        }
        // a Microsoft 365 account is never asked again
        "ready" | "many" => assert_eq!(asked_again(seen), 0, "{seen:?}"),
        // left out, and the account beside it runs the app: no sign-in screen stands in its way
        "unserved-own-then-ready" | "ready-then-unserved-own" => {
            assert!(seen.contains(&format!("start:{READY_OWN_UPN}")), "{seen:?}");
            assert!(seen.contains(&"start:ready@example.test".to_owned()), "{seen:?}");
            assert!(seen.iter().any(|call| call.starts_with("ctl-bound:")), "{seen:?}");
        }
        _ => {}
    }
}

/// How many times the run asked the own server of [`READY_OWN_UPN`] for its authz again.
fn asked_again(seen: &[String]) -> usize {
    let asked = format!("region-again:{READY_OWN_UPN}");
    seen.iter().filter(|call| **call == asked).count()
}

/// An own server's entry that never signed in (written by hand) is asked its password without "again"; one a sign-in
/// named is asked again.
#[test]
fn only_an_account_that_signed_in_before_is_asked_again() {
    use teams_ui::OwnOffer;
    let server = ngteams::accounts_file::OwnServer::parse(OWN_SERVER, "").unwrap();
    let written = PersonalAccount::pending_on(OWN_UPN, &server).unwrap();
    let (server, user) = (OWN_SERVER.to_owned(), OWN_UPN.to_owned());
    assert_eq!(
        super::own_offer(&written),
        OwnOffer::First {
            server: server.clone(),
            user: user.clone()
        }
    );
    let signed = written
        .signed_in_as("00000000-0000-4000-8000-0000000000a1", TID, None)
        .unwrap();
    assert_eq!(super::own_offer(&signed), OwnOffer::Again { server, user });
}

/// A second launch: handed to the app the runtime dir's socket names, or started as usual when nobody is behind it.
mod handoff {
    use super::transport::Listener;
    use std::io::BufReader;
    use std::path::Path;
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use clap::Parser as _;
    use teams_ctl::protocol::{Hello, Outcome as Answered, PROTOCOL_VERSION, Request, Response, ResponseLine};
    use teams_ctl::{LineReader, RequestLine, codec};

    use super::{Outcome, Setup, calls, signed};

    /// A synthetic meeting join link (a made-up thread).
    const LINK: &str = "msteams:/l/meetup-join/19%3ameeting_SyntheticHandoff%40thread.v2/0";

    pub fn run(case: &str, setup: &Setup) {
        // an account that starts at once: a launch not handed off shows in `calls` as its start
        signed(setup, "ready@example.test", 2, true);
        let socket = teams_ctl::Env::from_process()
            .mode_path(&teams_ctl::Mode::Personal)
            .unwrap();
        teams_ctl::prepare_dir(&socket).unwrap();
        let mut args = vec!["ngteams".to_owned()];
        let server = match case {
            "handoff" => Some(serve_once(&socket)),
            "handoff-link" => {
                args.push(LINK.to_owned());
                Some(serve_once(&socket))
            }
            // an app still starting: its account locked, its socket served only a moment later
            "handoff-starting" => {
                args.push(LINK.to_owned());
                let account = setup.signed_in_accounts().unwrap().pop().unwrap();
                let held = teams_store::EngineLock::hold(&setup.paths.store_file(&account).unwrap()).unwrap();
                let socket = socket.clone();
                Some(thread::spawn(move || {
                    thread::sleep(Duration::from_millis(600));
                    let asked = serve_once(&socket).join().unwrap();
                    drop(held);
                    asked
                }))
            }
            // the socket file an app that ended left: nobody behind it
            _ => {
                drop(Listener::bind(&socket).unwrap());
                None
            }
        };
        let cli = ngteams::cli::Cli::try_parse_from(args).unwrap();
        let outcome = super::run(&cli, teams_media::devices::Reach::Sandbox);
        assert!(matches!(outcome, Ok(Outcome::Quit)), "{case}: {:?}", outcome.err());
        let started = calls().lock().unwrap().iter().any(|call| call.starts_with("start:"));
        if let Some(server) = server {
            let wanted = if case == "handoff" {
                Request::Activate
            } else {
                Request::OpenLink { url: LINK.to_owned() }
            };
            assert_eq!(server.join().unwrap(), wanted);
            // the starting app's account is tried (and refused by its lock) before the hand-off is asked again
            assert_eq!(
                started,
                case == "handoff-starting",
                "a launch handed off starts no account"
            );
        } else {
            assert!(started, "nobody behind the socket: the app starts");
            let seen = calls().lock().unwrap();
            assert!(seen.contains(&format!("ctl-bound:{}", socket.display())), "{seen:?}");
        }
    }

    /// A running app's socket at `path` for one client: its `hello` answered, then what it asks next answered and
    /// handed back.
    fn serve_once(path: &Path) -> JoinHandle<Request> {
        let listener = Listener::bind(path).unwrap();
        thread::spawn(move || {
            let stream = listener.accept().unwrap();
            let mut reader = LineReader::new(BufReader::new(stream.try_clone().unwrap()));
            let mut writer = stream;
            let mut answer = |id: u64, response| {
                let line = ResponseLine {
                    id,
                    outcome: Answered::Ok(response),
                };
                codec::write_line(&mut writer, &line).unwrap();
            };
            let hello: RequestLine = reader.read().unwrap().unwrap();
            assert!(matches!(hello.request, Request::Hello { .. }), "{hello:?}");
            answer(
                hello.id,
                Response::Hello(Hello {
                    protocol: PROTOCOL_VERSION,
                    app: "synthetic".to_owned(),
                }),
            );
            let asked: RequestLine = reader.read().unwrap().unwrap();
            answer(asked.id, Response::Done);
            asked.request
        })
    }
}
