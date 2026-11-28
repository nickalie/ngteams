#![cfg_attr(windows, windows_subsystem = "windows")]
//! `ngteams`: desktop, read-only and lab startup. Build refusals come before anything is read or opened; application
//! mode is checked after the internal sign-in page worker has had its chance to serve.

use std::cell::Cell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser as _;
use gpui::{App, AppContext as _, AsyncApp, Entity};
#[cfg(not(test))]
use ngteams::accounts::{self, Runner, StartFailed, Started};
use ngteams::accounts_file::{FromEnv as _, PersonalAccount, PersonalPaths};
#[cfg(not(test))]
use ngteams::app;
use ngteams::app::{Launch, SignInAgain};
use ngteams::calling::CallDevices;
use ngteams::cli::{Cli, Command as Subcommand, LabMode, ReadOnlyMode};
use ngteams::descriptors::{RawDescriptor, adopt_socket, adopt_trace};
use ngteams::forget::Forgot;
use ngteams::handoff::{self, HandOff};
use ngteams::own_prompt::{OwnPrompt, SignIns};
use ngteams::setup::Setup;
use ngteams::{KEYRING_APPLICATION, lab, read_only, secrets, snapshot};
use teams_auth::secrets::LazyKeyring;
use teams_core::CallFloor;
use teams_media::devices::Reach;
#[cfg(not(test))]
use teams_ui::account_prompt;
use teams_ui::account_prompt::{AccountPrompt, AccountPromptEvent, OwnOffer};
use teams_ui::window::PERSONAL_APP_ID;
use teams_ui::{SettingsFile, ViewTrace};
use teams_update::Restart;

#[cfg(test)]
#[path = "../tests/support/lab_startup.rs"]
mod lab_startup_tests;
#[cfg(test)]
#[path = "../tests/support/startup.rs"]
mod startup_tests;
#[cfg(test)]
use startup_tests::accounts::{self, Runner, StartFailed, Started};
#[cfg(test)]
use startup_tests::{app, platform, prompt as account_prompt, window_runner};

// Keep the native backend choice at startup; lab/read-only sandbox checks run before constructing it.
#[cfg(not(test))]
mod platform {
    pub fn application() -> gpui::Application {
        #[cfg(windows)]
        {
            if let Err(error) = teams_platform::windows_desktop::set_app_id(crate::PERSONAL_APP_ID) {
                log::warn!("Windows app identity: {error}");
            }
            gpui_platform::application()
        }
        #[cfg(target_os = "linux")]
        {
            gpui_platform::linux(gpui::WindowingModes::WAYLAND)
        }
        #[cfg(target_os = "macos")]
        {
            let application = gpui_platform::application();
            // the Dock's icon clicked with no window open shows the main window again
            application.on_reopen(ngteams::mac_app::reopened);
            // a clicked `msteams:` link reaches the app here, never in its arguments
            application.on_open_urls(ngteams::mac_app::links_given);
            application
        }
    }
}

#[cfg(not(test))]
fn window_runner(runner: Runner) -> Runner {
    runner
}

/// How the app ended.
enum Outcome {
    Quit,
    /// Signed in again after a sign-out: the process runs itself again, so a new engine starts. `opened` is the link
    /// this run started with and opened already, left out of the next run's arguments so its pre-join isn't shown again.
    Restart {
        opened: Option<String>,
    },
    /// Lab sign-in keeps the broker and trace available for this same process's next image.
    LabRestart(ngteams::restart::RestartDescriptors),
    /// An update is in (`ngteams::update`): the process runs what `restart` names, its new version with this run's
    /// arguments (the link it `opened` left out, as for [`Outcome::Restart`]), or on Windows the installer that puts
    /// the new version in place once this process has exited and starts it.
    Update {
        restart: Restart,
        opened: Option<String>,
    },
}

fn main() -> ExitCode {
    // before any thread: the environment is read and written here alone
    #[cfg(target_os = "macos")]
    ngteams::mac_app::bundled_gstreamer();
    if let Some(refusal) = ngteams::refusal() {
        eprintln!("teams: {refusal}");
        return ExitCode::FAILURE;
    }
    pin_sign_in_worker();
    let cli = Cli::parse_from(ngteams::cli::shell_arguments(std::env::args_os()));
    if cli.build_identity {
        println!("{}", env!("CARGO_PKG_VERSION"));
        println!("{}", option_env!("NGTEAMS_BUILD_COMMIT").unwrap_or(""));
        return ExitCode::SUCCESS;
    }
    if cli.build_marker {
        println!(
            "{}",
            ngteams::refusal::marker_name(ngteams::refusal::INSTALLED, ngteams::refusal::LAB_BUILD)
        );
        return ExitCode::SUCCESS;
    }
    if cli.prepare {
        #[cfg(windows)]
        if let Err(error) = ngteams::prepare::end_children_with_this_run() {
            eprintln!("teams: prepare: {error}");
        }
        return prepare(ngteams::prepare::steps());
    }
    if let Some(archive) = &cli.install_web_engine {
        return install_web_engine(archive);
    }
    if let Some(refusal) = ngteams::refusal::refusal_for(cli.lab_requested()) {
        return failed("startup", &refusal);
    }
    match &cli.command {
        Some(Subcommand::Secrets { account, lab, fd }) => {
            return match (account.as_deref(), lab.as_deref()) {
                (Some(upn), None) => broker(upn, *fd),
                (None, Some(alias)) => lab_broker(alias, *fd),
                _ => failed("secrets", &"give exactly one of --account or --lab"),
            };
        }
        Some(Subcommand::SignIn { lab }) => return lab_sign_in(lab, &mut std::io::stdin().lock()),
        Some(Subcommand::Snapshot { to }) => return take_snapshot(to),
        None => {}
    }
    let outcome = match cli.read_only_mode(std::env::var_os(read_only::ENV).as_deref()) {
        Ok(Some(mode)) => run_read_only(&mode),
        Ok(None) => match cli.lab_mode() {
            Ok(Some(mode)) => run_lab(&cli, &mode),
            Ok(None) => run(&cli, Reach::Desktop),
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };
    match outcome {
        Ok(Outcome::Quit) => ExitCode::SUCCESS,
        Ok(Outcome::Restart { opened }) => {
            let given = given_link(&cli, opened.as_deref());
            restarted(&mut |command| {
                ngteams::restart::exec(command.args(restart_args(
                    std::env::args_os().skip(1),
                    opened.as_deref(),
                    given.as_deref(),
                )))
            })
        }
        Ok(Outcome::Update { restart, opened }) => {
            let given = given_link(&cli, opened.as_deref());
            updated(restart, opened.as_deref(), given.as_deref(), &mut |command| {
                ngteams::restart::exec(command)
            })
        }
        Ok(Outcome::LabRestart(descriptors)) => {
            restarted(&mut |command| descriptors.exec(command.args(std::env::args_os().skip(1))))
        }
        Err(error) => {
            eprintln!("teams: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Holds the sign-in window's engine worker from the start, so an update that replaces the files can't swap it under
/// this run.
fn pin_sign_in_worker() {
    #[cfg(target_os = "linux")]
    if let Err(error) = teams_wpe_client::worker::pin_worker() {
        eprintln!("teams: {error}");
    }
    #[cfg(target_os = "macos")]
    if let Err(error) = teams_signin::window::pin_worker() {
        eprintln!("teams: {error}");
    }
    // off the main thread: after a restart of Windows, Defender reads the whole file again before it opens
    #[cfg(windows)]
    std::thread::spawn(|| {
        if let Err(error) = teams_signin::window::pin_worker() {
            eprintln!("teams: {error}");
        }
    });
}

/// The installer's run of the app ([`ngteams::prepare`]). The installer goes on also when this run fails.
fn prepare(steps: Vec<ngteams::prepare::Step>) -> ExitCode {
    match ngteams::prepare::run(steps, ngteams::prepare::LIMIT) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => failed("prepare", &error),
    }
}

/// Installs a sign-in engine runtime from a file, under the same checks a download gets.
#[cfg(target_os = "linux")]
fn install_web_engine(archive: &std::path::Path) -> ExitCode {
    let installed = (|| {
        let pins = teams_web_engine::Pins::compiled()
            .map_err(|error| error.to_string())?
            .ok_or("this build has no pinned web engine: install your distribution's wpewebkit package")?;
        let locations = PersonalPaths::from_env().map_err(|error| error.to_string())?;
        pins.install(locations.data_dir(), archive)
            .map_err(|error| error.to_string())
    })();
    match installed {
        Ok(directory) => {
            println!("web engine installed in {}", directory.display());
            ExitCode::SUCCESS
        }
        Err(error) => failed("install-web-engine", &error),
    }
}

#[cfg(target_os = "macos")]
fn install_web_engine(_archive: &std::path::Path) -> ExitCode {
    failed(
        "install-web-engine",
        &"macOS signs in with the system's WebKit; there is no engine to install",
    )
}

#[cfg(windows)]
fn install_web_engine(_archive: &std::path::Path) -> ExitCode {
    failed(
        "install-web-engine",
        &"Windows uses Microsoft's installed WebView2 Runtime; install or repair it through Windows Settings",
    )
}

/// This process run again by `exec` (which returns only when it couldn't).
fn restarted(exec: &mut dyn FnMut(&mut Command) -> std::io::Error) -> ExitCode {
    let mut command = Command::new(std::env::current_exe().unwrap_or_else(|_| "ngteams".into()));
    let error = exec(&mut command);
    eprintln!("teams: signed in again, but it can't start again ({error}): start it again yourself");
    ExitCode::FAILURE
}

/// What `restart` names run by `exec` once an update is in: the new version with this run's arguments (the link it
/// `opened` left out, a link macOS handed it and it didn't open, `given`, added), without the AppImage runtime's
/// variables (the new AppImage's runtime sets them again), or the Windows installer with its own, in the folder the
/// update names; `exec` returns only when it couldn't.
fn updated(
    restart: Restart,
    opened: Option<&str>,
    given: Option<&str>,
    exec: &mut dyn FnMut(&mut Command) -> std::io::Error,
) -> ExitCode {
    let mut command = Command::new(&restart.program);
    match restart.arguments {
        Some(arguments) => command.args(arguments),
        None => command.args(restart_args(std::env::args_os().skip(1), opened, given)),
    };
    if let Some(directory) = &restart.directory {
        command.current_dir(directory);
    }
    #[cfg(unix)]
    for variable in ngteams::update::APPIMAGE_ENV {
        command.env_remove(variable);
    }
    let error = exec(&mut command);
    eprintln!(
        "teams: the update is ready, but {} can't start ({error}): start it yourself",
        restart.program.display()
    );
    ExitCode::FAILURE
}

/// The arguments a restart runs with: this run's (`args`), the link it `opened` left out and the link it was `given`
/// and didn't open ([`given_link`]) added.
fn restart_args(args: impl Iterator<Item = OsString>, opened: Option<&str>, given: Option<&str>) -> Vec<OsString> {
    args.filter(|arg| opened.is_none_or(|link| arg.as_os_str() != link))
        .chain(given.map(OsString::from))
        .collect()
}

/// The link a restart carries to its next run besides this run's arguments: on macOS, one macOS handed this run that
/// it didn't open (a link clicked to launch an app with no account ready waits through the first sign-in, and `exec`
/// would lose it). Elsewhere a clicked link is in the arguments already.
fn given_link(cli: &Cli, opened: Option<&str>) -> Option<String> {
    #[cfg(all(target_os = "macos", not(test)))]
    let given = ngteams::mac_app::take_given_links();
    #[cfg(not(all(target_os = "macos", not(test))))]
    let given = Vec::new();
    carried_link(cli.url.as_deref(), opened, given)
}

/// Of the links `given` and not opened, the one the next run's arguments carry: the last (the one clicked most
/// recently), and none when those arguments keep a link of their own (`url`, unless this run `opened` it), as a run
/// takes one link.
fn carried_link(url: Option<&str>, opened: Option<&str>, mut given: Vec<String>) -> Option<String> {
    let kept = url.is_some_and(|url| opened != Some(url));
    let carried = if kept { None } else { given.pop() };
    if !given.is_empty() {
        log::warn!(
            "{} link(s) macOS handed the app aren't opened: a restart carries one link",
            given.len()
        );
    }
    carried
}

/// How long a launch whose accounts another app holds waits for that app's control socket, and how often it asks:
/// an app locks its accounts before it serves its socket, so one still starting holds them without answering yet.
const STARTING_WAIT: Duration = Duration::from_secs(15);
const STARTING_POLL: Duration = Duration::from_millis(250);

fn run(cli: &Cli, reach: Reach) -> Result<Outcome, String> {
    let ctl = personal_ctl(cli);
    // before anything is opened or locked: an app already running takes this launch over
    if let (Some(socket), Some(request)) = (ctl.as_deref(), cli.hand_off())
        && let Some(handed) = handed(handoff::hand_off(socket, request, handoff::TIMEOUTS))
    {
        return handed;
    }
    let setup = Setup::from_env().map_err(|error| error.to_string())?;
    // the user's own app, installed from a release, updates itself; no other mode does
    let updater = ngteams::update::for_this_run(&setup.paths);
    run_setup(cli, reach, setup, ViewTrace::off(), ctl, updater)
}

/// How this launch ends once handed off: `None` when nobody took it, so it starts the app.
fn handed(hand_off: HandOff) -> Option<Result<Outcome, String>> {
    match hand_off {
        HandOff::Done { note } => {
            if let Some(note) = note {
                eprintln!("teams: {note}");
            }
            Some(Ok(Outcome::Quit))
        }
        HandOff::NotRunning => None,
        HandOff::Failed(why) => Some(Err(why)),
    }
}

/// The first account couldn't start (`failed`): when another app holds it and this launch hands off (the personal
/// app's), that app may still be starting, so the hand-off is asked again until its socket answers, for
/// [`STARTING_WAIT`]; otherwise, or past that, the launch ends with why.
fn hand_off_to_starting(cli: &Cli, ctl: Option<&Path>, failed: StartFailed) -> Result<Outcome, String> {
    let (Some(socket), Some(request), true) = (ctl, cli.hand_off(), failed.running) else {
        return Err(failed.into());
    };
    let deadline = Instant::now() + STARTING_WAIT;
    while Instant::now() < deadline {
        std::thread::sleep(STARTING_POLL);
        if let Some(handed) = handed(handoff::hand_off(socket, request.clone(), handoff::TIMEOUTS)) {
            return handed;
        }
    }
    Err(format!(
        "{failed}: Teams is still starting, or runs without its control socket {}; start it again once its window \
         shows",
        socket.display()
    ))
}

/// Where the personal app serves its control socket: `--ctl-socket`, else `$XDG_RUNTIME_DIR/ngteams/ctl.sock`
/// (`$NGTEAMS_CTL_SOCKET` is a client's, not the app's); none without a runtime dir.
fn personal_ctl(cli: &Cli) -> Option<PathBuf> {
    if let Some(path) = &cli.ctl_socket {
        return Some(path.clone());
    }
    match teams_ctl::Env::from_process().mode_path(&teams_ctl::Mode::Personal) {
        Ok(path) => Some(path),
        Err(error) => {
            eprintln!("teams: no control socket: {error}");
            None
        }
    }
}

fn run_setup(
    cli: &Cli,
    reach: Reach,
    setup: Setup,
    trace: ViewTrace,
    ctl: Option<PathBuf>,
    updater: Option<Updater>,
) -> Result<Outcome, String> {
    if let Some(upn) = cli.forget.as_deref() {
        forget(&setup, upn)?;
        return Ok(Outcome::Quit);
    }
    // The first sign-in needs the same credential-redacted guard diagnostics as an already signed-in account.
    let file = SettingsFile::new(setup.paths.settings_file());
    let diagnostics = app::install_log(setup.paths.log_dir(), file.load().settings.log_level);
    if let Some(asked) = cli.account.as_deref() {
        // the shortcut: that account, through the sign-in window at once when it isn't signed in
        let account = setup.ask_for(asked).map_err(|error| error.to_string())?;
        if !(account.is_signed_in() && accounts::ready(&setup, &account)?) {
            return sign_in_first(setup, account);
        }
    }
    let (ready, unready) = ready_accounts(&setup)?;
    if ready.is_empty() {
        if setup.secrets().application() == lab::LAB_KEYRING_APPLICATION {
            let account = lab::account(&setup.paths)?;
            return sign_in_first(setup, account);
        }
        return match unready.into_iter().next() {
            // an own server's account signed out: its password again, never Microsoft's field holding its user name
            Some(account) if account.server.is_some() => sign_in_first(setup, account),
            held => ask_account(setup, held.map(|account| account.upn)),
        };
    }
    let devices = Some(CallDevices::open(reach));
    let runner = Runner {
        setup: setup.clone(),
        floor: CallFloor::new(),
        devices,
    };
    let (first, others) = match start_accounts(&runner, &ready) {
        Ok(started) => started,
        Err(failed) if failed.unserved => return unserved_screen(runner, ready, failed),
        Err(failed) => return hand_off_to_starting(cli, ctl.as_deref(), failed),
    };
    let restart = Rc::new(Cell::new(false));
    let failed = Rc::new(Cell::new(false));
    let (restart_in_app, failed_in_app) = (Rc::clone(&restart), Rc::clone(&failed));
    let update = Rc::new(Cell::new(None));
    let update_in_app = Rc::clone(&update);
    // a link this launch was given with no app to hand it to (only the personal app takes one: `Cli`)
    let link = cli.url.clone();
    let application = platform::application().with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let (again_setup, again_restart) = (setup.clone(), Rc::clone(&restart_in_app));
        let launch = Launch {
            first,
            others,
            settings: app::settings(file, Some(diagnostics)),
            // the desktop's own session bus: the notifications, the tray and nothing else
            bus: teams_ui::bus::address_from_env(),
            sign_in_again: Rc::new(move |account| {
                sign_in_again(again_setup.clone(), account.clone(), Rc::clone(&again_restart))
            }),
            runner: window_runner(runner),
            read_only: false,
            trace: Arc::new(trace),
            ctl,
        };
        let Some(launched) = launched_or_said(launch, &failed_in_app, cx) else {
            return;
        };
        if let Some(updater) = updater {
            start_updates(updater, &launched, update_in_app, cx);
        }
        if let Some(url) = link {
            app::open_link(url, cx);
        }
        // the links macOS hands the app: the personal app's alone, as a link on the command line is (`Cli::lab_mode`)
        #[cfg(all(target_os = "macos", not(test)))]
        if setup.secrets().application() != lab::LAB_KEYRING_APPLICATION {
            ngteams::mac_app::open_given_links(cx);
        }
        // an account a sign-in named that can't start without one: its sign-in window, once the others run
        if let Some(account) = unready.into_iter().next() {
            sign_in_again(setup, account, restart_in_app)(cx);
        }
    });
    if failed.get() {
        return Err("the window couldn't be opened".to_owned());
    }
    // the link was opened in this run: a sign-in's restart doesn't show its pre-join again
    Ok(ended(restart.get(), update.take(), cli.url.clone()))
}

/// No account started, and the first is an own server's whose server serves no chats (`failed`, said on stderr by
/// [`start_accounts`]): said on its sign-in screen too, which stays for another try.
fn unserved_screen(runner: Runner, ready: Vec<PersonalAccount>, failed: StartFailed) -> Result<Outcome, String> {
    let setup = runner.setup.clone();
    drop(runner);
    let account = ready.into_iter().next().ok_or(failed.message.as_str())?;
    Ok(own_screen(setup, account, Some(failed.message)))
}

/// How a run that opened its window ended: into an update's new version, signed in again, or quit.
fn ended(restart: bool, update: Option<Restart>, opened: Option<String>) -> Outcome {
    match (update, restart) {
        (Some(restart), _) => Outcome::Update { restart, opened },
        (None, true) => Outcome::Restart { opened },
        (None, false) => Outcome::Quit,
    }
}

/// The app launched over `launch`, or why its window can't be opened said, `failed` set and the engines' shutdown
/// followed by the quit.
fn launched_or_said<S: teams_auth::secrets::SecretStore + 'static>(
    launch: Launch<S>,
    failed: &Cell<bool>,
    cx: &mut App,
) -> Option<ngteams::app::Launched> {
    match app::launch(cx, launch) {
        Ok(launched) => Some(launched),
        Err(failure) => {
            eprintln!("teams: the window can't be opened: {}", failure.error);
            failed.set(true);
            app::quit_after(failure.stopped, cx);
            None
        }
    }
}

/// The app's updates (`ngteams::update`) for the window `launched`: its restart waits on the desktop's calls and
/// drafts, and its program goes to `restart_into` before the app quits.
fn start_updates(
    updater: Updater,
    launched: &ngteams::app::Launched,
    restart_into: Rc<Cell<Option<Restart>>>,
    cx: &mut App,
) {
    ngteams::update::start(
        ngteams::update::Started {
            updater,
            holds: ngteams::update::holds(launched.desktop.downgrade()),
            restart: restart_into,
            quit: Rc::new(app::quit),
            first_check: Some(ngteams::update::FIRST_CHECK),
        },
        cx,
    );
}

/// What checks for and installs this copy's updates (`ngteams::update`).
type Updater = ngteams::update::Updater;

/// `ngteams secrets --account <UPN> --fd <n>`: the read-only mode's credential broker (`ngteams::read_only`), its
/// socket taken over first, before anything opens a descriptor of its own; `get` only, of an account
/// `accounts.toml` holds.
fn broker(upn: &str, fd: RawDescriptor) -> ExitCode {
    let socket = match adopt_socket(fd) {
        Ok(socket) => socket,
        Err(error) => return failed("secrets", &error),
    };
    let account = PersonalPaths::from_env()
        .map_err(|error| error.to_string())
        .and_then(|paths| PersonalAccount::find(&paths, upn).map_err(|error| error.to_string()));
    let account = match account {
        Ok(Some(account)) if account.is_signed_in() => account,
        Ok(_) => return failed("secrets", &format!("{upn} isn't one of your signed-in accounts")),
        Err(error) => return failed("secrets", &error),
    };
    let scope = read_only::broker_scope(&account.id());
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => return failed("secrets", &format!("can't start the async runtime: {error}")),
    };
    let store = LazyKeyring::new(KEYRING_APPLICATION);
    match runtime.block_on(read_only::serve_broker(socket, &scope, &store, &mut std::io::stderr())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => failed("secrets", &format!("{error}; the socket is closed")),
    }
}

/// `ngteams snapshot --to <dir>`: the snapshot of the user's files the read-only mode runs on (`ngteams::snapshot`).
fn take_snapshot(to: &Path) -> ExitCode {
    let made = PersonalPaths::from_env()
        .map_err(|error| error.to_string())
        .and_then(|mine| snapshot::snapshot(&mine, to).map_err(|error| error.to_string()));
    match made {
        Ok(paths) => {
            println!("snapshot: {}", to.display());
            println!("config: {}", paths.config_dir().display());
            println!("data: {}", paths.data_dir().display());
            ExitCode::SUCCESS
        }
        Err(error) => failed("snapshot", &error),
    }
}

fn failed(what: &str, error: &dyn std::fmt::Display) -> ExitCode {
    eprintln!("teams {what}: {error}");
    ExitCode::FAILURE
}

/// A lab alias uses only its isolated files and a writable, account-scoped broker. The inherited descriptors are
/// adopted before logging, files or media can allocate a descriptor of their own.
fn run_lab(cli: &Cli, mode: &LabMode) -> Result<Outcome, String> {
    let socket = adopt_socket(mode.secrets_fd).map_err(|error| error.to_string())?;
    let trace = mode
        .trace_fd
        .map(adopt_trace)
        .transpose()
        .map_err(|error| error.to_string())?;
    let descriptors = ngteams::restart::RestartDescriptors::new(socket, trace);
    let trace = descriptors
        .trace()
        .map_err(|error| error.to_string())?
        .map_or_else(ViewTrace::off, ViewTrace::new);
    lab::window_allowed_here()?;
    let paths = lab::paths_from_env(&mode.alias)?;
    let socket = descriptors.socket().map_err(|error| error.to_string())?;
    let store = secrets::AppSecrets::lab(socket).map_err(|error| format!("the lab broker's socket: {error}"))?;
    let setup = Setup::new(paths)
        .map_err(|error| error.to_string())?
        .with_secrets(store);
    let account = lab::account(&setup.paths)?;
    for requested in [cli.account.as_deref(), cli.forget.as_deref()].into_iter().flatten() {
        if !requested.eq_ignore_ascii_case(&account.upn) {
            return Err(format!("lab alias {} is configured for {}", mode.alias, account.upn));
        }
    }
    // the control socket only where its runner says (its state dir), never the personal one
    match run_setup(cli, Reach::Sandbox, setup, trace, mode.ctl_socket.clone(), None)? {
        Outcome::Restart { .. } => Ok(Outcome::LabRestart(descriptors)),
        outcome => Ok(outcome),
    }
}

/// The alias's one account in its lab namespace. It may still be pending: the broker must be available for its first
/// sign-in as well as token refresh and forget.
fn lab_broker(alias: &str, fd: RawDescriptor) -> ExitCode {
    let socket = match adopt_socket(fd) {
        Ok(socket) => socket,
        Err(error) => return failed("secrets", &error),
    };
    let account = lab::paths_from_env(alias).and_then(|paths| lab::account(&paths));
    let account = match account {
        Ok(account) => account,
        Err(error) => return failed("secrets", &error),
    };
    let runtime = match ngteams::accounts::runtime() {
        Ok(runtime) => runtime,
        Err(error) => return failed("secrets", &error),
    };
    let store = LazyKeyring::new(lab::LAB_KEYRING_APPLICATION);
    // an own server's account keeps its tokens under its server and UPN together
    let scope = secrets::lab_broker_scope(&account.id());
    match runtime.block_on(read_only::serve_broker(socket, &scope, &store, &mut std::io::stderr())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => failed("secrets", &format!("{error}; the socket is closed")),
    }
}

/// The alias's own server account signed in with the password on `input`, outside the UI sandbox
/// ([`lab::sign_in_own`]), its tokens in its lab namespace as its broker keeps them.
fn lab_sign_in(alias: &str, input: &mut dyn std::io::BufRead) -> ExitCode {
    let signed = lab::paths_from_env(alias).and_then(|paths| {
        let runtime = ngteams::accounts::runtime()?;
        let store = LazyKeyring::new(lab::LAB_KEYRING_APPLICATION);
        runtime.block_on(lab::sign_in_own(paths, input, &store))
    });
    match signed {
        Ok(said) => {
            println!("{said}");
            ExitCode::SUCCESS
        }
        Err(error) => failed("sign-in", &error),
    }
}

/// The read-only mode (`ngteams::read_only`): its descriptors taken over first, then the sandbox check, the snapshot
/// (apart from the user's own files), the account, its engine (read-only, no push channel, no calls) over the broker's
/// store, and the window; nothing adds, removes or signs in an account.
fn run_read_only(mode: &ReadOnlyMode) -> Result<Outcome, String> {
    let socket = adopt_socket(mode.secrets_fd).map_err(|error| error.to_string())?;
    let trace = match mode.trace_fd {
        Some(fd) => ViewTrace::new(adopt_trace(fd).map_err(|error| error.to_string())?),
        None => ViewTrace::off(),
    };
    read_only::window_allowed_here()?;
    let mine = PersonalPaths::from_env().map_err(|error| error.to_string())?;
    let paths = read_only::snapshot_paths(&mode.data, &mine)?;
    // own servers' accounts too: the mode never signs in, and its broker serves an account's own keys only
    let setup = Setup::new(paths).map_err(|error| error.to_string())?;
    let signed_in = setup.signed_in_accounts().map_err(|error| error.to_string())?;
    // named as the broker reads it: an id, or a bare UPN
    let account = PersonalAccount::named(&signed_in, &mode.account)
        .cloned()
        .ok_or_else(|| format!("read-only: {} isn't a signed-in account of the snapshot", mode.account))?;
    let secrets = read_only::secrets(socket).map_err(|error| format!("the broker's socket: {error}"))?;
    let file = SettingsFile::new(setup.paths.settings_file());
    let diagnostics = app::install_log(setup.paths.log_dir(), file.load().settings.log_level);
    log::info!("read-only mode: nothing that writes is sent");
    let engine = ngteams::engine::start_read_only(&setup.paths, &account, CallFloor::new(), secrets)
        .map_err(|error| format!("{}: {error}", account.upn))?;
    let first = ngteams::accounts::Started {
        account,
        engine,
        calls: None,
    };
    let runner = Runner {
        setup,
        floor: CallFloor::new(),
        devices: None,
    };
    let failed = Rc::new(Cell::new(false));
    let failed_in_app = Rc::clone(&failed);
    let ctl = mode.ctl_socket.clone();
    let application = platform::application().with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launch = Launch {
            first,
            others: Vec::new(),
            settings: app::settings(file, Some(diagnostics)).read_only(),
            // the bus it is given (the sandbox's): the notifications, and no tray item
            bus: teams_ui::bus::address_from_env(),
            // it never signs in: a sign-out only says so
            sign_in_again: Rc::new(|_| Rc::new(|_| {})),
            runner: window_runner(runner),
            read_only: true,
            trace: Arc::new(trace),
            // the control socket only where its runner says (its state dir), never the personal one
            ctl,
        };
        if let Err(failure) = app::launch(cx, launch) {
            eprintln!("teams: the window can't be opened: {}", failure.error);
            failed_in_app.set(true);
            app::quit_after(failure.stopped, cx);
        }
    });
    if failed.get() {
        return Err("the window couldn't be opened".to_owned());
    }
    Ok(Outcome::Quit)
}

/// Every account's engine: the first one that can be shown must start (another `ngteams` running on it, say, ends
/// this launch, after its hand-off: [`hand_off_to_starting`]); a later one that can't is said and left out, and so is
/// an own server's account whose server serves no chats, which fails the start only when nothing else starts.
fn start_accounts(runner: &Runner, ready: &[PersonalAccount]) -> Result<(Started, Vec<Started>), StartFailed> {
    let mut accounts = ready.iter();
    let mut unserved = None;
    let first = loop {
        let Some(account) = accounts.next() else {
            return Err(unserved.unwrap_or_else(|| StartFailed {
                message: "no account is signed in".to_owned(),
                running: false,
                unserved: false,
            }));
        };
        match runner.start(account) {
            Ok(started) => break started,
            Err(failed) if failed.unserved => {
                not_started(&failed);
                unserved.get_or_insert(failed);
            }
            Err(failed) => return Err(failed),
        }
    };
    let mut others = Vec::new();
    for account in accounts {
        match runner.start(account) {
            Ok(started) => others.push(started),
            Err(failed) => not_started(&failed),
        }
    }
    Ok((first, others))
}

fn not_started(failed: &StartFailed) {
    eprintln!("teams: an account isn't started: {failed}");
    log::warn!("an account isn't started");
}

/// The accounts a sign-in named, in the order they were added: those that can start without a sign-in, and those
/// that can't (a refresh token or the region missing), said on stderr; the first of these signs in again once the
/// app runs, or on the first sign-in screen when none can start.
fn ready_accounts(setup: &Setup) -> Result<(Vec<PersonalAccount>, Vec<PersonalAccount>), String> {
    let (mut ready, mut unready) = (Vec::new(), Vec::new());
    for account in setup.signed_in_accounts().map_err(|error| error.to_string())? {
        if accounts::ready(setup, &account)? {
            ask_own_again(setup, &account);
            ready.push(account);
        } else {
            eprintln!("teams: {} must sign in again", account.upn);
            unready.push(account);
        }
    }
    Ok((ready, unready))
}

/// An own server's account: its server's authz is asked again before it starts, so that its region names the
/// services the server has now (a server updated, or an option turned on or off, since the sign-in), and the client
/// shows only those. A server that takes no connection soon is not waited on ([`teams_auth::cloud::OWN_CONNECT_TIMEOUT`]).
/// One that can't be asked starts on the region it has; when that names no chat service, the start says why.
fn ask_own_again(setup: &Setup, account: &PersonalAccount) {
    if account.server_host().is_none() {
        return;
    }
    let unserved = accounts::unserved(setup, account).is_some();
    match accounts::region_again(setup, account) {
        Ok(()) => log::info!("an own server's region was asked again at the start"),
        Err(error) => {
            if unserved {
                eprintln!(
                    "teams: {}: its server's region wasn't asked again: {error}",
                    account.upn
                );
            }
            log::warn!("an own server's region wasn't asked again at the start");
        }
    }
}

/// The first sign-in screen (`teams_ui::account_prompt`): asks for the work account (`held`, or the last one typed
/// before, filled in), signs it in through the sign-in window, and once that goes through the app starts afresh
/// (`main`: the process runs itself again) with the account signed in. A sign-in that stores nothing is said on the
/// screen, which asks again; its entry goes as the rail's "+" leaves one ([`accounts::add`]: kept when the user wrote
/// its `idp_hosts` or the window refused an identity provider's host); closing it quits.
fn ask_account(setup: Setup, held: Option<String>) -> Result<Outcome, String> {
    let held = match held {
        Some(held) => Some(held),
        None => setup.pending_upns().map_err(|error| error.to_string())?.pop(),
    };
    let restart = Rc::new(Cell::new(false));
    let failed = Rc::new(Cell::new(false));
    let (restart_in_app, failed_in_app) = (Rc::clone(&restart), Rc::clone(&failed));
    let application = platform::application().with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        match account_prompt::open_offering(
            cx,
            PERSONAL_APP_ID,
            app::TITLE.into(),
            held.as_deref(),
            &OwnOffer::Choice,
        ) {
            Ok((_, prompt)) => cx
                .subscribe(&prompt, move |prompt, event, cx| {
                    let (setup, restart) = (setup.clone(), Rc::clone(&restart_in_app));
                    match event {
                        AccountPromptEvent::SignIn(upn) => sign_in_from_prompt(setup, upn.clone(), prompt, restart, cx),
                        event => {
                            own_prompt(&setup, None, &restart).handle(event, prompt, cx);
                        }
                    }
                })
                .detach(),
            Err(error) => {
                eprintln!("teams: the window can't be opened: {error:#}");
                failed_in_app.set(true);
                ngteams::run_end::end(cx);
            }
        }
    });
    if failed.get() {
        return Err("the window couldn't be opened".to_owned());
    }
    Ok(if restart.get() {
        Outcome::Restart { opened: None }
    } else {
        Outcome::Quit
    })
}

/// Signs `upn` in from the first sign-in screen, off GPUI's foreground ([`accounts::add`]). One that goes through
/// quits the app to start it afresh; one that doesn't is said on `prompt`.
fn sign_in_from_prompt(
    setup: Setup,
    upn: String,
    prompt: Entity<AccountPrompt>,
    restart: Rc<Cell<bool>>,
    cx: &mut App,
) {
    let windows = ngteams::signin::windows_for(&setup, cx);
    let signing = cx.background_spawn(async move { accounts::add(&setup, &upn, &windows) });
    cx.spawn(async move |cx: &mut AsyncApp| match signing.await {
        Ok(_) => {
            log::info!("the first account was signed in: the app restarts");
            restart.set(true);
            cx.update(app::quit);
        }
        Err(error) => {
            eprintln!("teams: {error}");
            cx.update(|cx| prompt.update(cx, |prompt, cx| prompt.failed(error, cx)));
        }
    })
    .detach();
}

/// What a sign-in screen does about an own server ([`ngteams::own_prompt`]): `again` is the account it asks again
/// (signed out, or never signed in), none where an account is added. A sign-in that goes through quits the app to
/// start it afresh; one whose server serves no chats is signed in and can't be shown, which is said like a sign-in
/// that didn't go through, so the screen stays instead of the app restarting into nothing.
fn own_prompt(setup: &Setup, again: Option<PersonalAccount>, restart: &Rc<Cell<bool>>) -> OwnPrompt {
    let (held, restart) = (setup.clone(), Rc::clone(restart));
    OwnPrompt {
        setup: setup.clone(),
        sign_ins: SignIns {
            add_own: accounts::add_own,
            sign_in_own: accounts::sign_in_own,
            needs_admin: accounts::needs_admin,
        },
        again,
        signed: Rc::new(move |account, prompt, cx| {
            if let Some(unserved) = accounts::unserved(&held, &account) {
                eprintln!("teams: {unserved}");
                prompt.update(cx, |prompt, cx| prompt.failed(unserved, cx));
            } else {
                log::info!("an own server's account was signed in: the app restarts");
                restart.set(true);
                app::quit(cx);
            }
        }),
    }
}

/// The screen asking for the password of the own server's `account` again (signed out, or not signed in yet), with
/// `said` under its fields when there is why it is shown: a sign-in that goes through restarts the app.
fn ask_own_password(
    setup: &Setup,
    account: PersonalAccount,
    said: Option<String>,
    restart: &Rc<Cell<bool>>,
    cx: &mut App,
) {
    let own = own_offer(&account);
    match account_prompt::open_offering(cx, PERSONAL_APP_ID, app::TITLE.into(), None, &own) {
        Ok((_, prompt)) => {
            if let Some(said) = said {
                prompt.update(cx, |prompt, cx| prompt.failed(said, cx));
            }
            let own = own_prompt(setup, Some(account), restart);
            cx.subscribe(&prompt, move |prompt, event, cx| {
                own.handle(event, prompt, cx);
            })
            .detach();
        }
        Err(error) => {
            eprintln!("teams: the window can't be opened: {error:#}");
            log::warn!("the sign-in screen of an own server's account can't be opened");
        }
    }
}

/// How the sign-in screen asks for the own server's `account`: "again" only when it signed in before (an entry
/// written by hand never did).
fn own_offer(account: &PersonalAccount) -> OwnOffer {
    let (server, user) = (account.server.clone().unwrap_or_default(), account.upn.clone());
    if account.is_signed_in() {
        OwnOffer::Again { server, user }
    } else {
        OwnOffer::First { server, user }
    }
}

/// [`sign_in_first`] for an own server's account: its password asked on the sign-in screen, alone, with `said`
/// under its fields ([`ask_own_password`]).
fn own_screen(setup: Setup, account: PersonalAccount, said: Option<String>) -> Outcome {
    let restart = Rc::new(Cell::new(false));
    let restart_in_app = Rc::clone(&restart);
    let application = platform::application().with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| ask_own_password(&setup, account, said, &restart_in_app, cx));
    if restart.get() {
        Outcome::Restart { opened: None }
    } else {
        Outcome::Quit
    }
}

/// The selected account's sign-in window, alone. A successful sign-in restarts the app; a failed personal add
/// abandons its bare entry as the rail's "+" does ([`accounts::add`]). The lab identity was provisioned beforehand:
/// sign it in as an existing account so closing or failing its first window leaves it available for another try.
fn sign_in_first(setup: Setup, account: PersonalAccount) -> Result<Outcome, String> {
    if account.server.is_some() {
        return Ok(own_screen(setup, account, None));
    }
    let restart = Rc::new(Cell::new(false));
    let failure = Rc::new(std::cell::RefCell::new(None));
    let (restart_in_app, failure_in_app) = (Rc::clone(&restart), Rc::clone(&failure));
    let application = platform::application().with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        // the sign-in window is the only one: closing it mustn't quit before the sign-in is done
        cx.set_quit_mode(gpui::QuitMode::Explicit);
        let windows = ngteams::signin::windows_for(&setup, cx);
        let signing = cx.background_spawn(async move {
            if setup.secrets().application() == lab::LAB_KEYRING_APPLICATION {
                accounts::sign_in(&setup, &account, &windows)
            } else {
                accounts::add(&setup, &account.upn, &windows)
            }
        });
        cx.spawn(async move |cx: &mut AsyncApp| {
            match signing.await {
                Ok(signed) => {
                    eprintln!("teams: signed in as {} (its tenant matched)", signed.upn);
                    restart_in_app.set(true);
                }
                Err(error) => *failure_in_app.borrow_mut() = Some(error),
            }
            cx.update(ngteams::run_end::end);
        })
        .detach();
    });
    if let Some(error) = failure.borrow_mut().take() {
        return Err(error);
    }
    Ok(if restart.get() {
        Outcome::Restart { opened: None }
    } else {
        Outcome::Quit
    })
}

/// Once an account is signed out: its sign-in window again, off GPUI's foreground; a sign-in that goes through quits
/// the app (every account's engine shut down first: [`app::quit`]) to start it afresh, one that doesn't leaves the
/// account signed out.
fn sign_in_again(setup: Setup, account: PersonalAccount, restart: Rc<Cell<bool>>) -> SignInAgain {
    Rc::new(move |cx: &mut App| {
        let (setup, account, restart) = (setup.clone(), account.clone(), Rc::clone(&restart));
        if account.server.is_some() {
            // an own server's account never sees Microsoft's window: its password is asked again
            ask_own_password(&setup, account, None, &restart, cx);
            return;
        }
        let windows = ngteams::signin::windows_for(&setup, cx);
        let signing = cx.background_spawn(async move { accounts::sign_in(&setup, &account, &windows) });
        cx.spawn(async move |cx: &mut AsyncApp| match signing.await {
            Ok(_) => {
                log::info!("signed in again after a sign-out: the app restarts");
                restart.set(true);
                cx.update(app::quit);
            }
            Err(error) => {
                eprintln!("teams: {error}; quit and start Teams again to sign in");
                log::warn!("the sign-in after a sign-out stored nothing");
            }
        })
        .detach();
    })
}

/// Signs out and forgets the account `upn` (see [`ngteams::forget`]): refused while Teams runs for it.
fn forget(setup: &Setup, upn: &str) -> Result<(), String> {
    match accounts::forget(setup, upn)? {
        Forgot::Nothing => eprintln!("teams: {upn} isn't one of the accounts: nothing to forget"),
        Forgot::Account {
            upn,
            store: Some(store),
        } => eprintln!(
            "teams: {upn} is forgotten; its store stays at {} (delete it by hand)",
            store.display()
        ),
        Forgot::Account { upn, store: None } => eprintln!("teams: {upn} is forgotten"),
    }
    Ok(())
}
