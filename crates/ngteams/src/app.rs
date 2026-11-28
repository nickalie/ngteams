//! The personal app on GPUI: every account the user added, all running at once in one window
//! (`docs/personal-build.md`, "Several accounts"): a bridge per account over its engine, the main window
//! (`teams-ui`'s desktop with a shell per account, the rail's avatars switching between them, and the first run's
//! screen of the writes that happen on their own, [`settings`]), the notifications and the tray on the desktop's
//! session bus, the calls when the engines have their calling ([`PersonalCalls`]: a call button and "Make a test call", an
//! incoming call's ringtone, notification and banner, the in-call view, the video switch and the devices in the
//! settings; one call at a time across the accounts), the sign-out and the quit.
//!
//! **Adding an account** (the rail's "+"): the window asking for the account opens (the first sign-in's), the account
//! is signed in through the sign-in window off GPUI's foreground ([`crate::accounts::add`]), and once that goes through
//! its engine starts ([`Runner::start`]) and the window shows it; nothing else restarts. The
//! window asks first where the account is: an own server's is signed in with its password on that server alone
//! ([`crate::own_prompt`]) and runs beside the Microsoft 365 accounts, its tokens, store and services its own; one
//! that is an administrator of its server gets Manage users in its settings ([`offer_user_admin`]). **Removing one** ("Sign out
//! and remove" in its account menu): its engine is shut down, then it is forgotten as `ngteams --forget` forgets it,
//! then it leaves the window; the last one removed quits the app.
//!
//! **Signed out** ([`watch_sign_out`]): once an account's session can't renew its tokens (`invalid_grant`: a sign-in
//! frequency policy, a password change, a revoked session; or no refresh token stored), its shell shows
//! [`SIGNED_OUT_HINT`], its engine is shut down (nothing more is sent for it: every request answers that it stopped),
//! and the sign-in window opens again for it ([`SignInAgain`]). A sign-in that goes through starts the app afresh
//! (`main`: the process runs itself again), so a new engine starts only once it is signed in again. An account an
//! administrator turned off (`AADSTS50057`) shows [`TURNED_OFF_HINT`] instead and opens nothing.
//!
//! **The control socket** ([`crate::ctl`], [`Launch::ctl`]): served from the launch, once the window opened, until the
//! quit, which closes it first. A socket another app serves already is said, and the app runs without one.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use crate::accounts_file::{OwnServer, PersonalAccount};
use gpui::{App, AppContext as _, AsyncApp, Entity, Global, QuitMode, SharedString, Task, WeakEntity, WindowHandle};
use teams_auth::secrets::SecretStore;
use teams_auth::session::{Session, SignedOut};
use teams_core::EngineHandle;
use teams_log::diagnostics::Source as DiagnosticsSource;
use teams_ui::account_prompt::{self, AccountPrompt, AccountPromptEvent, OwnOffer, OwnSignIn};
use teams_ui::bus::BusError;
use teams_ui::window::{KitRoot, PERSONAL_APP_ID};
use teams_ui::{
    AccountParts, Bridge, Core, Desktop, DesktopEvent, DesktopParts, LogLevel, Models, SettingsFile, SettingsModel,
    ShellParts, VideoFeed, ViewTrace,
};
use tokio::sync::watch;

use crate::accounts::{self, Runner, Started};
use crate::calling::PersonalCalls;
use crate::engine::display_name;
use crate::own_admin::AdminService;
use crate::own_prompt::{OwnPrompt, Signed};
use crate::secrets::AppSecrets;

#[cfg(windows)]
#[path = "app/windows.rs"]
mod windows;

#[cfg(windows)]
pub(crate) use windows::shell_id;

#[cfg(unix)]
pub(crate) const fn shell_id(_: &crate::setup::Setup, _: bool, _: &App) -> &'static str {
    PERSONAL_APP_ID
}

/// The window's title and the app's name on its notifications and tray item.
pub const TITLE: &str = "ngteams";

/// What the window says once the account is signed out, after "Signed out:" (`teams_ui::view::status_text`).
pub const SIGNED_OUT_HINT: &str = "sign in again in the window that opened";

/// What the read-only mode's window says once the account is signed out: it never signs in itself.
pub const READ_ONLY_SIGNED_OUT_HINT: &str = "sign in again in your own Teams (this read-only window never signs in)";

/// What the window says once an administrator turned the account's sign-in off: no sign-in window opens, as the
/// account couldn't sign in there.
pub const TURNED_OFF_HINT: &str = "an administrator turned off sign-in for this account";

/// Opens the sign-in window again once the account is signed out (see the module docs).
pub type SignInAgain = Rc<dyn Fn(&mut App)>;

/// What makes an account's [`SignInAgain`] ([`Launch::sign_in_again`]).
pub type SignInAgainFor = Rc<dyn Fn(&PersonalAccount) -> SignInAgain>;

/// What the app starts with: engines over the selected desktop or lab broker credentials, or a test's own store.
pub struct Launch<S = AppSecrets> {
    /// The first account's engine, started; the others follow it in [`Launch::others`].
    pub first: Started<S>,
    pub others: Vec<Started<S>>,
    /// The settings, the first run's screen asked for ([`settings`]).
    pub settings: SettingsModel,
    /// The desktop's session bus, for the notifications and the tray item.
    pub bus: Result<String, BusError>,
    /// The sign-in window again for an account signed out ([`SignInAgain`]).
    pub sign_in_again: Rc<dyn Fn(&PersonalAccount) -> SignInAgain>,
    /// What starts more accounts (the rail's "+") and forgets one ("Sign out and remove").
    pub runner: Runner,
    /// The read-only mode (`ngteams --read-only`, [`crate::read_only`]): no account is added, removed or signed in, the
    /// rail offers neither, and a sign-out only says so ([`READ_ONLY_SIGNED_OUT_HINT`]).
    pub read_only: bool,
    /// Where the view trace goes (`--trace-fd`, the read-only mode's runner reads it); off otherwise.
    pub trace: Arc<ViewTrace>,
    /// Where the control socket is served (`ngteamsctl`, [`crate::ctl`]); `None` serves none.
    pub ctl: Option<PathBuf>,
}

/// The settings in `file` (`~/.local/share/ngteams/settings.toml`) with the first run's screen asked for: none of
/// typing, read receipts and presence is sent until the user confirms it (`docs/personal-build.md`, "The settings that
/// write on their own"); with where "copy diagnostics" reads from.
#[must_use]
pub fn settings(file: SettingsFile, diagnostics: Option<DiagnosticsSource>) -> SettingsModel {
    let settings = SettingsModel::load(file).requiring_first_run();
    match diagnostics {
        Some(diagnostics) => settings.with_diagnostics(diagnostics),
        None => settings,
    }
}

/// Installs the diagnostic log in `dir` at the settings' `setting` level (`TEAMS_APP_LOG` overriding it) before the
/// engine starts. It is the user's data, like the store: agents never read it. A log that can't be opened is said once
/// on stderr and in the settings view, and the app runs without it.
#[must_use]
pub fn install_log(dir: PathBuf, setting: LogLevel) -> DiagnosticsSource {
    let config = teams_log::Config::from_env(dir, setting);
    let (log_file, log_problem) = match teams_log::install(&config) {
        Ok(log) => (Some(log.path().to_owned()), None),
        Err(error) => {
            eprintln!("teams: {error}");
            (None, Some(error.to_string()))
        }
    };
    log::info!("teams {} starting", env!("CARGO_PKG_VERSION"));
    DiagnosticsSource {
        app: format!("teams {}", env!("CARGO_PKG_VERSION")),
        log_file,
        log_problem,
    }
}

/// The app's accounts as it runs them, held for its life: the desktop, and each account's bridge holding its engine.
struct Held {
    running: Entity<Running>,
}

impl Global for Held {}

/// The accounts the app runs: each one's bridge (its MRI, its UPN), what starts more and forgets one, the settings
/// every account shares and the prompt that adds one while it is open.
pub struct Running {
    desktop: Entity<Desktop>,
    bridges: Vec<(String, String, Entity<Bridge>)>,
    /// The own server's origin of each account that is on one, by its MRI.
    servers: std::collections::HashMap<String, String>,
    runner: Runner,
    settings: Entity<SettingsModel>,
    sign_in_again: Rc<dyn Fn(&PersonalAccount) -> SignInAgain>,
    adding: Option<WindowHandle<KitRoot>>,
    quitting: bool,
    /// The read-only mode: nothing is added, removed or signed in ([`Launch::read_only`]).
    read_only: bool,
    /// The control socket served, until the quit ([`Launch::ctl`]).
    ctl: Option<crate::ctl::Listener>,
    /// Its event streams, fed by each account's bridge tap ([`crate::ctl::events`]).
    hub: Rc<crate::ctl::events::Hub>,
    /// The view trace every account's views record to ([`Launch::trace`]): the control socket's call asks too.
    trace: Arc<ViewTrace>,
    /// The control socket's `mute` and `camera`, asked one at a time ([`crate::ctl::calls`]).
    call_switches: Rc<tokio::sync::Mutex<()>>,
}

/// What [`launch`] started.
pub struct Launched {
    pub window: WindowHandle<KitRoot>,
    pub desktop: Entity<Desktop>,
}

/// The window couldn't be opened: why, and the engines' shutdown (the caller quits after it).
pub struct LaunchFailed {
    pub error: String,
    pub stopped: Task<()>,
}

/// The desktop of the first account's `shell`, as `account` names it; no ringtone, no banner pictures.
fn desktop_parts(app_id: &'static str, shell: ShellParts, account: &PersonalAccount, read_only: bool) -> DesktopParts {
    DesktopParts {
        app_id,
        app_name: TITLE.into(),
        title: TITLE.into(),
        shell,
        account_name: display_name(account).to_owned().into(),
        account_detail: account.upn.clone().into(),
        account_server: account.server_host().map(Into::into),
        can_add: !read_only,
        ringer: None,
        pictures_dir: None,
    }
}

/// Makes `account` the first account of the settings, and keeps its calls' noise suppression at its level.
fn first_account_settings(
    settings: &Entity<SettingsModel>,
    account: &str,
    calls: Option<&PersonalCalls>,
    cx: &mut App,
) {
    settings.update(cx, |settings, cx| settings.set_first_account(account, cx));
    follow_noise(settings, account, calls, cx);
}

/// Keeps the noise suppression of `account`'s calls at its level in the settings, when it has calls.
fn follow_noise(settings: &Entity<SettingsModel>, account: &str, calls: Option<&PersonalCalls>, cx: &mut App) {
    if let Some(calls) = calls {
        teams_ui::settings::noise::follow(settings, account.to_owned(), calls.noise.clone(), cx);
    }
}

/// Starts the app inside GPUI's `run`: a bridge per account holding its engine, the window with every account in it,
/// the sign-out watches, the bus, and what the rail asks (an account added or removed) and the quit.
///
/// # Errors
/// The window can't be opened: every bridge is shut down then.
pub fn launch<S: SecretStore + 'static>(cx: &mut App, launch: Launch<S>) -> Result<Launched, LaunchFailed> {
    let Launch {
        first,
        others,
        settings,
        bus,
        sign_in_again,
        runner,
        read_only,
        trace,
        ctl,
    } = launch;
    let app_id = shell_id(&runner.setup, read_only, cx);
    let (hint, sign_in_again) = signing_out(read_only, sign_in_again);
    play_videos_on(runner.devices.as_ref(), cx);
    // closing the window keeps the app in the tray: it quits itself once the engines are shut down
    cx.set_quit_mode(QuitMode::Explicit);
    crate::ai_keys::install(&runner.setup.secrets(), read_only, cx);
    let settings = shared_settings(settings, first.calls.as_ref(), cx);
    let first_session = Arc::clone(&first.engine.session);
    let first_runtime = first.engine.handle.runtime();
    let (models, bridge) = bridge(first.engine.handle, first.calls.as_ref(), cx);
    watch_sign_out_saying(
        hint,
        first.engine.session.watch_signed_out(),
        bridge.downgrade(),
        sign_in_again(&first.account),
        cx,
    );
    let core = bridge.read(cx).core();
    let key = core.me().to_owned();
    first_account_settings(&settings, &key, first.calls.as_ref(), cx);
    let parts = ShellParts {
        core,
        models,
        settings: settings.clone(),
        trace: Arc::clone(&trace),
    };
    let parts = DesktopParts {
        ringer: first.calls.and_then(|calls| calls.ringer),
        pictures_dir: Some(runner.setup.paths.data_dir().to_path_buf()),
        ..desktop_parts(app_id, parts, &first.account, read_only)
    };
    let desktop = cx.new(|cx| Desktop::new(parts, cx));
    if !read_only {
        let own = (&first.account, &first_session, first_runtime);
        offer_user_admin(&runner.setup, own, key.clone(), &desktop, cx);
    }
    limit_lab_accounts(&desktop, &runner, cx);
    let servers = own_server_of(&first.account, &key);
    let running = cx.new(|_| Running {
        desktop: desktop.clone(),
        bridges: vec![(key, first.account.id(), bridge)],
        servers,
        runner,
        settings,
        sign_in_again,
        adding: None,
        quitting: false,
        read_only,
        ctl: None,
        hub: Rc::default(),
        trace,
        call_switches: Rc::default(),
    });
    start_others(&running, others, cx);
    let window = match desktop.update(cx, Desktop::open_window) {
        Ok(window) => window,
        Err(error) => {
            log::error!("the window can't be opened: {error:#}");
            return Err(LaunchFailed {
                error: format!("{error:#}"),
                stopped: running.update(cx, Running::shutdown),
            });
        }
    };
    hold(&running, ctl.as_deref(), cx);
    follow_desktop(&desktop, running, cx);
    connect(&desktop, bus, cx);
    #[cfg(target_os = "macos")]
    crate::mac_app::install(cx);
    Ok(Launched { window, desktop })
}

/// The own server's origin of `account` (its MRI `key`), by that MRI; nothing for a Microsoft 365 account.
fn own_server_of(account: &PersonalAccount, key: &str) -> std::collections::HashMap<String, String> {
    account
        .server
        .clone()
        .map(|server| (key.to_owned(), server))
        .into_iter()
        .collect()
}

/// The first account watched for the event streams, the `others` started after it, and the app's own events watched;
/// before the window opens, so its shells are watched as they are made.
fn start_others<S: SecretStore + 'static>(running: &Entity<Running>, others: Vec<Started<S>>, cx: &mut App) {
    running.update(cx, |running, cx| {
        running.watch_first(cx);
        for started in others {
            running.add_started(started, cx);
        }
    });
    let (hub, desktop) = {
        let held = running.read(cx);
        (Rc::clone(held.hub()), held.desktop.clone())
    };
    crate::ctl::watch::app(&hub, &desktop, crate::ctl::events::accounts_of(running.downgrade()), cx);
}

/// `running` held for the app's life, and the control socket at `ctl` served over it until its quit.
fn hold(running: &Entity<Running>, ctl: Option<&std::path::Path>, cx: &mut App) {
    cx.set_global(Held {
        running: running.clone(),
    });
    if let Some(path) = ctl {
        serve_ctl(path, running, cx);
    }
}

/// The control socket at `path`, held by `running` until its quit; one that can't be served is said, and the app runs
/// without it.
fn serve_ctl(path: &std::path::Path, running: &Entity<Running>, cx: &mut App) {
    match crate::ctl::serve(path, running.downgrade(), cx) {
        Ok(listener) => running.update(cx, |running, _| running.ctl = Some(listener)),
        Err(error) => {
            eprintln!("teams: no control socket: {error}");
            log::warn!("no control socket: {error}");
        }
    }
}

/// How long the app waits before asking again whether an account is an administrator, after a question that got no
/// answer; doubled after each, up to [`ADMIN_ASKED_AGAIN_AT_MOST`].
const ADMIN_ASKED_AGAIN: std::time::Duration = std::time::Duration::from_secs(15);
const ADMIN_ASKED_AGAIN_AT_MOST: std::time::Duration = std::time::Duration::from_mins(15);

/// What the server said of an account being its administrator.
enum AdminAnswer {
    Yes,
    No,
    /// No answer (the server unreachable, a wait asked, the keyring unread): asked again later.
    Unanswered(&'static str),
}

/// Whether the account behind `service` is an administrator of its server, asked only of an account the app itself
/// signed in (blocking).
fn admin_answer<S: SecretStore + 'static>(
    setup: &crate::setup::Setup,
    account: &PersonalAccount,
    service: &AdminService<S>,
) -> AdminAnswer {
    match accounts::ready(setup, account) {
        Ok(false) => return AdminAnswer::No,
        Ok(true) => {}
        Err(_) => return AdminAnswer::Unanswered("store"),
    }
    match service.is_admin() {
        Ok(true) => AdminAnswer::Yes,
        // an account signed out (or its engine stopped) gets nothing until it signs in again
        Ok(false) | Err(teams_ui::user_admin::AdminProblem::SignedOut) => AdminAnswer::No,
        Err(problem) => AdminAnswer::Unanswered(problem.kind()),
    }
}

/// Offers Manage users to `account` (its MRI `key`, its `session`, whose requests run on `sessions`: the account's
/// engine's runtime) when it is an own server's and that server says it is an administrator: asked off the
/// foreground, with the account's own token on its own server, and only of an account the app itself signed in (its
/// region saved and its refresh token stored, so its session has a token to ask with). A question that gets no answer
/// is logged (its kind alone) and asked again later; a "no" is final for the run. Once the server says the account is
/// no administrator any more (the page asked it), Manage users goes. A Microsoft 365 account is asked nothing and
/// offered nothing.
fn offer_user_admin<S: SecretStore + 'static>(
    setup: &crate::setup::Setup,
    (account, session, sessions): (&PersonalAccount, &Arc<Session<S>>, tokio::runtime::Handle),
    key: String,
    desktop: &Entity<Desktop>,
    cx: &mut App,
) {
    let Some(service) = AdminService::of(account, Arc::clone(session), sessions) else {
        return;
    };
    let service = Arc::new(service);
    let (setup, account, desktop) = (setup.clone(), account.clone(), desktop.downgrade());
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut wait = ADMIN_ASKED_AGAIN;
        loop {
            let (asked, setup, account) = (Arc::clone(&service), setup.clone(), account.clone());
            let answer = cx
                .background_executor()
                .spawn(async move { admin_answer(&setup, &account, &asked) })
                .await;
            match answer {
                AdminAnswer::Yes => break,
                AdminAnswer::No => return,
                AdminAnswer::Unanswered(kind) => {
                    log::warn!(
                        "an own server didn't say whether its account is an administrator ({kind}): asked again in \
                         {} s",
                        wait.as_secs()
                    );
                    cx.background_executor().timer(wait).await;
                    wait = (wait * 2).min(ADMIN_ASKED_AGAIN_AT_MOST);
                    if desktop.upgrade().is_none() {
                        return;
                    }
                }
            }
        }
        log::info!("an own server's account is an administrator: its settings offer Manage users");
        let mut demoted = service.demoted();
        let apps = Arc::clone(&service);
        // the desktop holds the service from here: once the account goes, so does it, and the wait below ends
        let offered = desktop.update(cx, |desktop, cx| desktop.set_user_admin(&key, Some(service), cx));
        if offered.is_ok() {
            offer_app_admin(apps, key.clone(), desktop.clone(), cx);
        }
        if offered.is_ok() && demoted.wait_for(|demoted| *demoted).await.is_ok() {
            log::info!("an own server's account is no administrator any more: Manage users and Manage apps go");
            let _ = desktop.update(cx, |desktop, cx| {
                desktop.set_user_admin(&key, None, cx);
                desktop.set_app_admin(&key, None, cx);
            });
        }
    })
    .detach();
}

/// How many times the app asks whether an administrator's server serves apps before it stops for the run.
const APPS_ASKED_AT_MOST: u32 = 4;

/// Offers Manage apps to the administrator `key` (its `service`) while its server serves apps: asked off the foreground, again after a question that got no answer, a "no" final for the run.
fn offer_app_admin<S: SecretStore + 'static>(
    service: Arc<AdminService<S>>,
    key: String,
    desktop: WeakEntity<Desktop>,
    cx: &mut AsyncApp,
) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut wait = ADMIN_ASKED_AGAIN;
        for _ in 0..APPS_ASKED_AT_MOST {
            let asked = Arc::clone(&service);
            let served = cx.background_executor().spawn(async move { asked.serves_apps() }).await;
            match served {
                Ok(true) => {
                    log::info!("an own server serves apps: its administrator's settings offer Manage apps");
                    let _ = desktop.update(cx, |desktop, cx| desktop.set_app_admin(&key, Some(service), cx));
                    return;
                }
                Ok(false) => return,
                Err(problem) => {
                    log::warn!(
                        "an own server didn't say whether it serves apps ({}): asked again in {} s",
                        problem.kind(),
                        wait.as_secs()
                    );
                    cx.background_executor().timer(wait).await;
                    wait = (wait * 2).min(ADMIN_ASKED_AGAIN_AT_MOST);
                    if desktop.upgrade().is_none() {
                        return;
                    }
                }
            }
        }
    })
    .detach();
}

/// A lab alias can forget its current account, but its single-account broker cannot add another one.
fn limit_lab_accounts(desktop: &Entity<Desktop>, runner: &Runner, cx: &mut App) {
    if runner.setup.secrets().application() == crate::lab::LAB_KEYRING_APPLICATION {
        let rail = desktop.read(cx).accounts().clone();
        rail.update(cx, teams_ui::accounts::Accounts::prevent_adding);
    }
}

/// A video a message carries plays its sound on the calls' devices (silently without them).
fn play_videos_on(devices: Option<&crate::calling::CallDevices>, cx: &mut App) {
    let playback = devices.map(crate::calling::CallDevices::playback).unwrap_or_default();
    teams_ui::video_player::set_playback(playback, cx);
}

/// What a sign-out shows and opens: the sign-in window again with [`SIGNED_OUT_HINT`], or in the read-only mode
/// nothing but [`READ_ONLY_SIGNED_OUT_HINT`] (it never signs in, whatever was handed in).
fn signing_out(read_only: bool, sign_in_again: SignInAgainFor) -> (&'static str, SignInAgainFor) {
    if read_only {
        (READ_ONLY_SIGNED_OUT_HINT, Rc::new(|_| Rc::new(|_| {})))
    } else {
        (SIGNED_OUT_HINT, sign_in_again)
    }
}

/// The desktop's notifications and tray on `bus`, what failed said.
fn connect(desktop: &Entity<Desktop>, bus: Result<String, BusError>, cx: &mut App) {
    let connecting = desktop.update(cx, |desktop, cx| desktop.connect(bus, cx));
    cx.spawn(async move |_: &mut AsyncApp| {
        for problem in connecting.await {
            eprintln!("teams: {problem}");
            log::warn!("{problem}");
        }
    })
    .detach();
}

/// The settings every account shares, their problems said; the first account's calls' video following them and the
/// calls' devices attached, from the start.
fn shared_settings(settings: SettingsModel, calls: Option<&PersonalCalls>, cx: &mut App) -> Entity<SettingsModel> {
    for problem in settings.problems() {
        eprintln!("teams: {problem}");
        log::warn!("{problem}");
    }
    let video = calls.and_then(|calls| calls.video.clone());
    let settings = settings_entity(settings, video, cx);
    if let Some(devices) = calls.and_then(|calls| calls.devices.clone()) {
        teams_ui::settings::attach_devices(&settings, devices, cx);
    }
    if let Some(sounds) = calls.and_then(|calls| calls.sounds.clone()) {
        teams_ui::settings::attach_sounds(&settings, sounds, cx);
    }
    if let Some(camera) = calls.and_then(|calls| calls.camera.clone()) {
        // the video effects' preview, on the camera the settings choose
        teams_ui::settings::attach_camera_preview(&settings, teams_ui::CameraPreview::new(camera), cx);
    }
    settings
}

/// What the desktop asks, done: an account added, one removed, the quit.
fn follow_desktop(desktop: &Entity<Desktop>, running: Entity<Running>, cx: &mut App) {
    cx.subscribe(desktop, move |_, event, cx| {
        running.update(cx, |running, cx| match event {
            DesktopEvent::Quit => running.quit(cx),
            DesktopEvent::AddAccount => running.ask_account(cx),
            DesktopEvent::RemoveAccount(key) => running.remove(key, cx),
        });
    })
    .detach();
}

impl Running {
    /// The desktop: the window, the rail, the tray.
    pub(crate) fn desktop(&self) -> &Entity<Desktop> {
        &self.desktop
    }

    /// The own server's origin of the account `mri`; `None` for a Microsoft 365 account.
    pub(crate) fn server_of(&self, mri: &str) -> Option<&str> {
        self.servers.get(mri).map(String::as_str)
    }

    /// Each account as it runs, in the rail's order: its MRI, its name ([`PersonalAccount::id`]: its UPN, with its
    /// server before it for an own server's) and its bridge.
    pub(crate) fn bridges(&self) -> &[(String, String, Entity<Bridge>)] {
        &self.bridges
    }

    /// Whether the app is quitting: its engines shutting down.
    pub(crate) fn quitting(&self) -> bool {
        self.quitting
    }

    /// The control socket's event streams.
    pub(crate) fn hub(&self) -> &Rc<crate::ctl::events::Hub> {
        &self.hub
    }

    /// The view trace the accounts' views record to.
    pub(crate) fn trace(&self) -> &Arc<ViewTrace> {
        &self.trace
    }

    /// What the control socket's `mute` and `camera` hold while each is asked and answered.
    pub(crate) fn call_switches(&self) -> &Rc<tokio::sync::Mutex<()>> {
        &self.call_switches
    }

    /// The first account watched for the event streams (the others are as they start).
    fn watch_first(&self, cx: &mut App) {
        if let Some((_, upn, bridge)) = self.bridges.first() {
            crate::ctl::watch::account(&self.hub, upn, bridge, &self.desktop, cx);
        }
    }

    /// `event` of the account `upn` (its bridge `bridge`) told to the event streams, while anyone listens.
    fn tell(&self, upn: &str, bridge: &Entity<Bridge>, event: teams_ctl::protocol::CtlEvent, cx: &mut App) {
        if self.hub.listening() {
            let origin = crate::ctl::events::Origin::of(upn, bridge, cx);
            self.hub.publish(&origin, event, cx);
        }
    }

    /// Read-only views cannot add accounts, and each lab alias has its own single-account broker.
    fn can_add_account(&self) -> bool {
        !self.read_only && self.runner.setup.secrets().application() != crate::lab::LAB_KEYRING_APPLICATION
    }

    /// An account started: its bridge, its sign-out watch, its calls' video following the settings, and its place in
    /// the window.
    fn add_started<S: SecretStore + 'static>(&mut self, started: Started<S>, cx: &mut gpui::Context<Self>) {
        let Started { account, engine, calls } = started;
        if let Some(feed) = calls.as_ref().and_then(|calls| calls.video.clone()) {
            teams_ui::video::follow_settings(&feed, self.settings.read(cx));
            cx.observe(&self.settings, move |_, model, cx| {
                teams_ui::video::follow_settings(&feed, model.read(cx));
            })
            .detach();
        }
        let (session, sessions) = (Arc::clone(&engine.session), engine.handle.runtime());
        let (models, bridge) = bridge(engine.handle, calls.as_ref(), cx);
        let hint = if self.read_only {
            READ_ONLY_SIGNED_OUT_HINT
        } else {
            SIGNED_OUT_HINT
        };
        watch_sign_out_saying(
            hint,
            engine.session.watch_signed_out(),
            bridge.downgrade(),
            (self.sign_in_again)(&account),
            cx,
        );
        let core = bridge.read(cx).core();
        let key = core.me().to_owned();
        follow_noise(&self.settings, &key, calls.as_ref(), cx);
        let parts = AccountParts {
            core,
            models,
            name: display_name(&account).to_owned().into(),
            detail: account.upn.clone().into(),
            server: account.server_host().map(Into::into),
        };
        self.desktop.update(cx, |desktop, cx| desktop.add_account(parts, cx));
        if !self.read_only {
            let own = (&account, &session, sessions);
            offer_user_admin(&self.runner.setup, own, key.clone(), &self.desktop, cx);
        }
        let id = account.id();
        if let Some(server) = account.server {
            self.servers.insert(key.clone(), server);
        }
        crate::ctl::watch::account(&self.hub, &id, &bridge, &self.desktop, cx);
        self.tell(&id, &bridge, teams_ctl::protocol::CtlEvent::AccountAdded, cx);
        self.bridges.push((key, id, bridge));
    }

    /// The rail's "+": the window asking for the account (the first sign-in's), brought forward if it is open. The
    /// account it names is signed in through the sign-in window off GPUI's foreground; once that goes through its
    /// engine starts and the window shows it, and the prompt closes; one that doesn't is said on the prompt. The
    /// prompt's events come to `Running` as it is being updated already (GPUI's subscriber is leased to its callback),
    /// so they are handled on the `running` handed in, never by updating `Running` again.
    fn ask_account(&mut self, cx: &mut gpui::Context<Self>) {
        if !self.can_add_account() {
            log::info!("this mode adds no account");
            return;
        }
        if let Some(window) = self.adding
            && window.update(cx, |_, window, _| window.activate_window()).is_ok()
        {
            return;
        }
        let opened =
            account_prompt::open_offering(cx, PERSONAL_APP_ID, "Add an account".into(), None, &OwnOffer::Choice);
        let (window, prompt) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                log::warn!("the window asking for an account can't be opened: {error:#}");
                return;
            }
        };
        self.adding = Some(window);
        let own = self.own_prompt(cx);
        cx.subscribe(&prompt, move |running, prompt, event, cx| match event {
            AccountPromptEvent::SignIn(upn) => running.sign_in_added(upn.clone(), prompt, cx),
            AccountPromptEvent::SignInOwn(typed) if running.own_added_already(typed, &prompt, cx) => {}
            event => {
                if running.can_add_account() {
                    own.handle(event, prompt, cx);
                }
            }
        })
        .detach();
    }

    /// What the window asking for an account does about an own server ([`crate::own_prompt`]): an account signed in
    /// there is started beside the others and shown, and one that can't start is said on the window.
    fn own_prompt(&self, cx: &mut gpui::Context<Self>) -> OwnPrompt {
        let this = cx.weak_entity();
        let signed: Signed = Rc::new(move |account, prompt, cx| {
            let _ = this.update(cx, |running, cx| match running.runner.start(&account) {
                Ok(started) => running.added(started, cx),
                Err(error) => {
                    log::warn!("an own server's account wasn't added");
                    prompt.update(cx, |prompt, cx| prompt.failed(String::from(error), cx));
                }
            });
        });
        OwnPrompt {
            setup: self.runner.setup.clone(),
            sign_ins: crate::own_prompt::SignIns::app(),
            again: None,
            signed,
        }
    }

    /// Whether the own server's account the prompt named runs already: said on the prompt then.
    fn own_added_already(&self, own: &OwnSignIn, prompt: &Entity<AccountPrompt>, cx: &mut gpui::Context<Self>) -> bool {
        let Ok(server) = OwnServer::parse(&own.server, "") else {
            return false;
        };
        let id = format!("{}/{}", server.origin, own.user);
        let added = self.bridges.iter().any(|(_, held, _)| held.eq_ignore_ascii_case(&id));
        if added {
            prompt.update(cx, |prompt, cx| {
                prompt.failed(format!("{} is added already", own.user), cx);
            });
        }
        added
    }

    /// Once `signing` went through, starts the account and shows it; one that didn't is said on `prompt`.
    fn start_signed(
        signing: Task<Result<PersonalAccount, String>>,
        prompt: Entity<AccountPrompt>,
        cx: &mut gpui::Context<Self>,
    ) {
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let signed = signing.await;
            let _ = this.update(cx, |running, cx| {
                match signed.and_then(|account| running.runner.start(&account).map_err(String::from)) {
                    Ok(started) => running.added(started, cx),
                    Err(error) => {
                        log::warn!("an account wasn't added");
                        prompt.update(cx, |prompt, cx| prompt.failed(error, cx));
                    }
                }
            });
        })
        .detach();
    }

    /// Signs the account `upn` the prompt named in, then starts it (see [`Running::ask_account`]).
    fn sign_in_added(&mut self, upn: String, prompt: Entity<AccountPrompt>, cx: &mut gpui::Context<Self>) {
        if !self.can_add_account() {
            return;
        }
        if self.bridges.iter().any(|(_, held, _)| held.eq_ignore_ascii_case(&upn)) {
            prompt.update(cx, |prompt, cx| prompt.failed(format!("{upn} is added already"), cx));
            return;
        }
        let setup = self.runner.setup.clone();
        let windows = crate::signin::windows_for(&setup, cx);
        let signing = cx.background_spawn(async move { accounts::add(&setup, &upn, &windows) });
        Self::start_signed(signing, prompt, cx);
    }

    /// An account added: started, shown, and the prompt that asked for it closed.
    fn added<S: SecretStore + 'static>(&mut self, started: Started<S>, cx: &mut gpui::Context<Self>) {
        self.add_started(started, cx);
        if let Some(key) = self.bridges.last().map(|(key, _, _)| key.clone()) {
            self.desktop.update(cx, |desktop, cx| {
                desktop.switch(&key, cx);
                desktop.show(cx);
            });
        }
        if let Some(window) = self.adding.take() {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
    }

    /// "Sign out and remove" of the account `key`: its engine shut down, then the account forgotten as
    /// `ngteams --forget` forgets it (its store stays), then it leaves the window. The last account removed quits the
    /// app (the next start asks for an account).
    fn remove(&mut self, key: &str, cx: &mut gpui::Context<Self>) {
        if self.read_only {
            // its Secret Service items, its entry and its region stay as the user's own Teams keeps them
            log::info!("read-only: no account is removed");
            return;
        }
        let Some(index) = self.bridges.iter().position(|(held, _, _)| held == key) else {
            return;
        };
        let (key, upn, bridge) = self.bridges.remove(index);
        self.tell(&upn, &bridge, teams_ctl::protocol::CtlEvent::AccountRemoved, cx);
        let last = self.bridges.is_empty();
        // its own settings go with it; the next account, when it was the first, takes the first's place
        let next = self.bridges.first().map(|(next, _, _)| next.clone());
        self.settings
            .update(cx, |settings, cx| settings.forget_account(&key, next.as_deref(), cx));
        log::info!("an account is signed out and removed: its engine shuts down");
        let stopped = bridge.update(cx, Bridge::shutdown);
        let setup = self.runner.setup.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            stopped.await;
            let forgot = cx.background_spawn(async move { accounts::forget(&setup, &upn) }).await;
            if let Err(error) = forgot {
                eprintln!("teams: the account wasn't forgotten: {error}");
                log::warn!("an account removed wasn't forgotten");
            }
            let _ = this.update(cx, |running, cx| {
                if last {
                    running.quit(cx);
                } else {
                    running
                        .desktop
                        .update(cx, |desktop, cx| desktop.remove_account(&key, cx));
                }
            });
        })
        .detach();
    }

    /// Every engine shut down, joined.
    fn shutdown(&mut self, cx: &mut gpui::Context<Self>) -> Task<()> {
        let stopping: Vec<Task<()>> = self
            .bridges
            .iter()
            .map(|(_, _, bridge)| bridge.update(cx, Bridge::shutdown))
            .collect();
        cx.background_spawn(async move {
            for stopped in stopping {
                stopped.await;
            }
        })
    }

    /// The quit: every engine shut down, then the app quits; the first ask does it.
    fn quit(&mut self, cx: &mut gpui::Context<Self>) {
        if std::mem::replace(&mut self.quitting, true) {
            return;
        }
        log::info!("the app quits: every account's engine shuts down first");
        // nothing more is asked over the control socket: its event streams and connections closed, its file removed
        self.hub.close();
        if let Some(mut ctl) = self.ctl.take() {
            ctl.stop();
        }
        let stopped = self.shutdown(cx);
        quit_after(stopped, cx);
    }
}

/// The models and the bridge holding `engine`. The models show calls when the window has them (`calls`: its test call
/// to the echo service, its calls' video on their feed); without them whatever the engine told of a call is left out.
pub fn bridge(engine: EngineHandle, calls: Option<&PersonalCalls>, cx: &mut App) -> (Models, Entity<Bridge>) {
    let models = match calls {
        Some(calls) => match calls.video.clone() {
            Some(feed) => Models::with_video_calls(Some(calls.echo.clone()), feed, cx),
            None => Models::with_calls(Some(calls.echo.clone()), cx),
        },
        None => Models::new(cx),
    };
    if let Some(directory) = calls.and_then(|calls| calls.devices.clone()) {
        teams_ui::calls::follow_devices(&models.calls, directory, cx);
    }
    let core: Arc<dyn Core> = Arc::new(engine.requests());
    let drained = models.clone();
    let bridge = cx.new(|cx| Bridge::start(engine, core, drained, cx));
    (models, bridge)
}

/// The settings as an entity; when the calls carry video, with the video switch, which `video` follows.
fn settings_entity(settings: SettingsModel, video: Option<VideoFeed>, cx: &mut App) -> Entity<SettingsModel> {
    let settings = match &video {
        Some(feed) => {
            let settings = settings.with_video_switch();
            teams_ui::video::follow_settings(feed, &settings);
            settings
        }
        None => settings,
    };
    settings.publish_ai(cx);
    let settings = cx.new(|_| settings);
    if let Some(feed) = video {
        cx.observe(&settings, move |model, cx| {
            teams_ui::video::follow_settings(&feed, model.read(cx));
        })
        .detach();
    }
    settings
}

/// Once `signed_out` says the session is signed out: shows [`SIGNED_OUT_HINT`], shuts the engine down (nothing more is
/// sent) and, once it is down, opens the sign-in window again (`again`), once; for an account an administrator turned
/// off, shows [`TURNED_OFF_HINT`] and opens nothing.
pub fn watch_sign_out(
    signed_out: watch::Receiver<Option<SignedOut>>,
    bridge: WeakEntity<Bridge>,
    again: SignInAgain,
    cx: &mut App,
) {
    watch_sign_out_saying(SIGNED_OUT_HINT, signed_out, bridge, again, cx);
}

/// [`watch_sign_out`], the window saying `hint` (the read-only mode's [`READ_ONLY_SIGNED_OUT_HINT`], whose `again`
/// does nothing); for an account an administrator turned off, [`TURNED_OFF_HINT`] in its place.
pub fn watch_sign_out_saying(
    hint: &'static str,
    mut signed_out: watch::Receiver<Option<SignedOut>>,
    bridge: WeakEntity<Bridge>,
    again: SignInAgain,
    cx: &mut App,
) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        // the session dropped without signing out: nothing to watch
        let turned_off = match signed_out.wait_for(Option::is_some).await {
            Ok(why) => turned_off(why.as_ref()),
            Err(_) => return,
        };
        let hint = if turned_off { TURNED_OFF_HINT } else { hint };
        let Ok(stopped) = bridge.update(cx, |bridge, cx| bridge.sign_out(SharedString::from(hint), cx)) else {
            return;
        };
        stopped.await;
        if !turned_off {
            cx.update(|cx| again(cx));
        }
    })
    .detach();
}

/// Whether the session was signed out because an administrator turned the account off.
fn turned_off(why: Option<&SignedOut>) -> bool {
    matches!(why, Some(SignedOut::InvalidGrant(refusal)) if refusal.codes.contains(&crate::signin::ACCOUNT_DISABLED_CODE))
}

/// Each account's bridge as the app runs it, in the rail's order; none before the launch.
#[must_use]
pub fn bridges(cx: &App) -> Vec<Entity<Bridge>> {
    cx.try_global::<Held>().map_or_else(Vec::new, |held| {
        held.running
            .read(cx)
            .bridges
            .iter()
            .map(|(_, _, bridge)| bridge.clone())
            .collect()
    })
}

/// Whether a control socket client follows the events ([`crate::ctl::events::Hub::listening`]); none before the launch.
#[must_use]
pub fn ctl_listening(cx: &App) -> bool {
    cx.try_global::<Held>()
        .is_some_and(|held| held.running.read(cx).hub.listening())
}

/// Opens the Teams link `url` the app was started with (`ngteams <link>` with no app running to hand it to,
/// [`crate::handoff`]), or one macOS handed it (a clicked `msteams:` link, `mac_app`), as `open-link` opens it ([`crate::ctl::links`]); what keeps it from opening is said on stderr.
/// Nothing before the launch.
pub fn open_link(url: String, cx: &mut App) {
    let Some(running) = cx.try_global::<Held>().map(|held| held.running.downgrade()) else {
        return;
    };
    cx.spawn(async move |cx: &mut AsyncApp| {
        let request = teams_ctl::protocol::Request::OpenLink { url };
        if let Err(error) = crate::ctl::links::answer(&running, None, &request, cx).await {
            eprintln!("teams: the link isn't opened: {}", error.message);
            log::warn!("a link the app was given isn't opened ({:?})", error.code);
        }
    })
    .detach();
}

/// The running app's desktop; none before the launch.
#[cfg(target_os = "macos")]
pub(crate) fn desktop(cx: &App) -> Option<Entity<Desktop>> {
    cx.try_global::<Held>()
        .map(|held| held.running.read(cx).desktop.clone())
}

/// Quits the app: every account's engine shut down first ([`Running`]'s quit), then the app; before the app was
/// launched (the first sign-in screen), at once.
pub fn quit(cx: &mut App) {
    match cx.try_global::<Held>().map(|held| held.running.clone()) {
        Some(running) => running.update(cx, Running::quit),
        None => crate::run_end::end(cx),
    }
}

/// Quits the app once `stopped` (the engine's shutdown) has ended.
pub fn quit_after(stopped: Task<()>, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        stopped.await;
        log::info!("the engines are shut down: the app quits now");
        cx.update(crate::run_end::end);
    })
    .detach();
}

#[cfg(test)]
#[path = "../tests/support/app.rs"]
pub(crate) mod tests;
