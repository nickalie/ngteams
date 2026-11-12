//! The actual startup controllers on a mock GPUI platform and empty in-memory credentials.
//! Each case owns HOME/XDG in a subprocess; no user's files, keyring, display or service is reached.

use std::process::Command;
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
    pub use ngteams::accounts::StartFailed;

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
    pub(crate) fn open(
        cx: &mut gpui::App,
        app_id: &str,
        title: gpui::SharedString,
        held: Option<&str>,
    ) -> gpui::Result<(
        gpui::WindowHandle<teams_ui::window::KitRoot>,
        gpui::Entity<teams_ui::AccountPrompt>,
    )> {
        if std::env::var(super::CASE).unwrap() == "prompt-window-error" {
            return Err(std::io::Error::other("synthetic prompt window failure").into());
        }
        teams_ui::account_prompt::open(cx, app_id, title, held)
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
            if matches!(case.as_str(), "prompt-success" | "prompt-error") {
                submit_prompt(&cx);
            }
            if matches!(case.as_str(), "prompt-success" | "prompt-error")
                || case.starts_with("first-")
                || case.starts_with("again-")
            {
                wait_for_backend(&cx);
            }
            cx.update(ngteams::app::quit);
            cx.run_until_parked();
        }
    }

    fn submit_prompt(cx: &TestAppContext) {
        let prompt = cx.update(|cx| {
            cx.windows()
                .into_iter()
                .filter_map(|window| window.downcast::<KitRoot>())
                .find_map(|window| window.read(cx).ok()?.view().clone().downcast::<AccountPrompt>().ok())
                .expect("the account prompt is open")
        });
        assert_eq!(cx.read(|cx| prompt.read(cx).account(cx)), "pending@example.test");
        cx.update(|cx| {
            prompt.update(cx, |_, cx| {
                cx.emit(AccountPromptEvent::SignIn("pending@example.test".into()));
            });
        });
    }

    fn wait_for_backend(cx: &TestAppContext) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            cx.run_until_parked();
            let complete = super::calls()
                .lock()
                .unwrap()
                .iter()
                .any(|call| call.starts_with("add:") || call.starts_with("sign-in:"));
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
        "empty" | "prompt-window-error" => {}
        other => panic!("unknown synthetic case {other}"),
    }
    let outcome = run(
        &ngteams::cli::Cli::try_parse_from(args).unwrap(),
        teams_media::devices::Reach::Sandbox,
    );
    if let Err(error) = &outcome {
        eprintln!("synthetic startup {case}: {error}");
    }
    match case.as_str() {
        "prompt-success" | "first-success" | "first-unready-success" | "again-success" => {
            // no link was given: the restart leaves nothing out
            assert!(matches!(outcome, Ok(Outcome::Restart { opened: None })));
        }
        "first-error"
        | "first-start-fails"
        | "broken-ready"
        | "forget-error"
        | "window-error"
        | "prompt-window-error" => assert!(outcome.is_err()),
        _ => assert!(matches!(outcome, Ok(Outcome::Quit))),
    }
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

/// A link macOS handed the app and it didn't open (one that launched it before the first sign-in) goes to the next
/// run's arguments, unless those keep a link of their own: a run takes one.
#[test]
fn a_restart_carries_a_given_link_it_did_not_open() {
    let given = |links: &[&str]| links.iter().map(|link| (*link).to_owned()).collect::<Vec<_>>();
    let (first, second) = ("msteams:/l/meetup-join/synthetic-1", "msteams:/l/meetup-join/synthetic-2");
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
            ["--ctl-socket", "/run/synthetic.sock"].map(std::ffi::OsString::from).into_iter(),
            None,
            Some(first)
        ),
        ["--ctl-socket", "/run/synthetic.sock", first].map(std::ffi::OsString::from)
    );
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
    if case == "many" {
        assert!(
            seen.iter().any(|call| call == "start:ready-other@example.test"),
            "a later account still starts"
        );
    }
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
