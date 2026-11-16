//! The installed app's updates from its GitHub releases ([`teams_update`]), installed from the title bar's button
//! ([`teams_ui::update`]).
//!
//! - **Which runs check** ([`for_this_run`]): only a release build, which knows its release version (the release
//!   workflow compiles it in), carries no lab marker and no `test-support`, and was installed from a release package,
//!   run as the user's own app. `main` asks only in that mode, never `--lab` or `--read-only`. A source build
//!   (`scripts/install.sh`) never checks: it updates as it was installed, `git pull && scripts/install.sh`.
//! - **When** ([`FIRST_CHECK`], then every [`EVERY`]): offline or rate-limited, nothing is shown and the next check
//!   tries again. One anonymous request an hour is far under GitHub's limit of 60 per address. GitHub is no Teams
//!   service: it gets no token and no account data, only the app's name and version as its `User-Agent`.
//! - **The download**, as ngterm's: a check that finds a newer release downloads and verifies the release's asset for
//!   this installation at once, in the background, the button showing how far. Offline or rate-limited, nothing is
//!   shown and the next check tries again; another failure (a bad checksum, no permission, a full disk) shows Update
//!   failed, and the next check or a press tries again.
//! - **The press** (Restart to update, once the download is verified): unless a call is up or ringing or a box holds a
//!   message not sent yet ([`teams_ui::update::Waiting`]), the update is installed and the app restarts; else the
//!   press says what holds it up. Only a system package asks for a password, at the press: a per-user copy replaces
//!   its own files. A closed password prompt keeps the download and returns to Restart to update. A failed rollback
//!   keeps the previous installation in the stage named by the error.
//! - **The restart**: once the update is in, the app quits as it always does, every engine shut down first (the
//!   outbox's rows and the store stay), and `main` runs the new version in this process, with this run's arguments.
//!   On Windows, where a running program's files can't be replaced, "in" means the new installer is downloaded and
//!   checked: `main` starts it in its own folder as the app exits, and it waits for the exit, upgrades the folder and
//!   starts the app.
//!   The keyring's credentials are untouched.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, AppContext as _, AsyncApp, Global, SharedString, Task};
use teams_ui::update::{self as button, Holds, UpdateStatus};
use teams_update::{Error, Installation, Places, Prepared, Release, Restart, Source, Version};

use crate::accounts_file::PersonalPaths;

/// The first check, this long after the start: the app's own start goes first.
pub const FIRST_CHECK: Duration = Duration::from_secs(30);
/// The checks after it.
pub const EVERY: Duration = Duration::from_hours(1);

/// What checks for and installs this installation's updates.
#[derive(Clone, Debug)]
pub struct Updater {
    pub current: Version,
    pub installation: Installation,
    pub source: Source,
}

/// The updater of this run, when this build updates itself (see the module docs); why not is logged.
#[must_use]
pub fn for_this_run(paths: &PersonalPaths) -> Option<Updater> {
    let current = updating_build(
        Version::current(),
        crate::refusal::LAB_BUILD,
        crate::refusal::test_support(),
    )?;
    let places = for_this_copy(Places::from_process(
        paths.data_dir().join("updates"),
        vec![paths.data_dir().to_path_buf(), paths.config_dir().to_path_buf()],
    ))?;
    let installation = for_this_copy(Installation::detect(&places))?;
    log::info!("updates: release {current}, installed as {:?}", installation.format());
    Some(Updater {
        current,
        installation,
        source: Source::github(),
    })
}

/// Both path lookup and installation detection explain why updates aren't available for this copy.
fn for_this_copy<T>(result: Result<T, impl std::fmt::Display>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            log::info!("updates: not for this copy ({error})");
            None
        }
    }
}

/// This build's release version, when it is a release build that may update itself: no lab marker, no test-support.
fn updating_build(current: Option<Version>, lab_build: Option<&str>, test_support: bool) -> Option<Version> {
    current.filter(|_| lab_build.is_none_or(str::is_empty) && !test_support)
}

/// The app's update, as it goes.
struct Flow {
    updater: Updater,
    /// The newest release found, not yet installed.
    release: Option<Release>,
    /// Its download, verified, waiting for its install.
    prepared: Option<Prepared>,
    /// Already installed (on Windows, its installer checked and waiting to run), awaiting a safe restart; a later press
    /// must not install it again.
    installed: Option<Restart>,
    /// A download or an install runs.
    busy: bool,
    /// Where a restart's program goes for `main` to run, once the update is in.
    restart: Rc<Cell<Option<Restart>>>,
    /// The app's quit.
    quit: Rc<dyn Fn(&mut App)>,
    _checks: Option<Task<()>>,
}

impl Global for Flow {}

/// What [`start`] is handed.
pub struct Started {
    pub updater: Updater,
    /// What holds a restart up ([`teams_ui::desktop::Desktop::update_waiting`]).
    pub holds: Holds,
    pub restart: Rc<Cell<Option<Restart>>>,
    pub quit: Rc<dyn Fn(&mut App)>,
    /// When the first check runs; `None` runs none by itself (tests check with [`check`]).
    pub first_check: Option<Duration>,
}

/// Starts the updates: what an earlier run's update left removed, the button's press and what holds it up set, and
/// the checks running.
pub fn start(started: Started, cx: &mut App) {
    let Started {
        updater,
        holds,
        restart,
        quit,
        first_check,
    } = started;
    let installation = updater.installation.clone();
    cx.background_spawn(async move {
        let removed = installation.cleanup_stale();
        if removed > 0 {
            log::info!("updates: {removed} earlier update stage(s) removed");
        }
    })
    .detach();
    button::on_press(Rc::new(press), cx);
    button::holds_with(holds, cx);
    let checks = first_check.map(|first| {
        cx.spawn(async move |cx: &mut AsyncApp| {
            cx.background_executor().timer(first).await;
            loop {
                if let Some(checking) = cx.update(check) {
                    checking.await;
                }
                cx.background_executor().timer(EVERY).await;
            }
        })
    });
    cx.set_global(Flow {
        updater,
        release: None,
        prepared: None,
        installed: None,
        busy: false,
        restart,
        quit,
        _checks: checks,
    });
}

fn version(version: Version) -> SharedString {
    version.to_string().into()
}

/// Asks GitHub for a newer release, unless a download or install is under way or waits; one found is downloaded.
pub fn check(cx: &mut App) -> Option<Task<()>> {
    let flow = cx.try_global::<Flow>()?;
    if flow.busy || flow.prepared.is_some() || flow.installed.is_some() {
        return None;
    }
    let Updater {
        current,
        installation,
        source,
    } = flow.updater.clone();
    let checking = cx.background_spawn(async move { source.check(&installation, current) });
    Some(cx.spawn(async move |cx: &mut AsyncApp| {
        let found = checking.await;
        cx.update(|cx| found_release(found, cx));
    }))
}

fn found_release(found: Result<Option<Release>, Error>, cx: &mut App) {
    let release = match found {
        Ok(Some(release)) => release,
        Ok(None) => return,
        Err(error @ (Error::Offline | Error::RateLimited)) => {
            log::info!("updates: no check now: {error}");
            return;
        }
        Err(error) => {
            log::warn!("updates: the check failed: {error}");
            return;
        }
    };
    let flow = cx.global_mut::<Flow>();
    if flow.busy || flow.prepared.is_some() || flow.installed.is_some() {
        return;
    }
    if flow.release.as_ref() != Some(&release) {
        log::info!("updates: release {} is out", release.version);
    }
    flow.release = Some(release.clone());
    download(release, cx);
}

/// The button's press: the install of the download, or a new try of a download that failed.
fn press(cx: &mut App) {
    let Some(flow) = cx.try_global::<Flow>() else {
        return;
    };
    if flow.busy {
        return;
    }
    if flow.installed.is_some() {
        restart_when_ready(cx);
    } else if flow.prepared.is_some() {
        install(cx);
    } else if let Some(release) = flow.release.clone() {
        download(release, cx);
    }
}

/// `received` of `total` in whole percent, at most 100.
fn percent(received: u64, total: u64) -> u8 {
    let percent = (u128::from(received) * 100 / u128::from(total.max(1))).min(100);
    u8::try_from(percent).unwrap_or(100)
}

/// Downloads and verifies `release`, showing how far; then offers its restart.
fn download(release: Release, cx: &mut App) {
    let shown = version(release.version);
    log::info!("updates: downloading {}", release.asset());
    button::set_status(
        UpdateStatus::Downloading {
            version: shown.clone(),
            percent: 0,
        },
        cx,
    );
    let flow = cx.global_mut::<Flow>();
    flow.busy = true;
    let (installation, source) = (flow.updater.installation.clone(), flow.updater.source.clone());
    let (sender, progress) = async_channel::unbounded();
    let preparing = cx.background_spawn(async move {
        let mut last = 0;
        source.prepare(&installation, &release, move |received, total| {
            let now = percent(received, total);
            if now != last {
                last = now;
                let _ = sender.try_send(now);
            }
        })
    });
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok(percent) = progress.recv().await {
            let version = shown.clone();
            cx.update(|cx| button::set_status(UpdateStatus::Downloading { version, percent }, cx));
        }
        let prepared = preparing.await;
        cx.update(|cx| {
            cx.global_mut::<Flow>().busy = false;
            match prepared {
                Ok(prepared) => {
                    log::info!("updates: {shown} is downloaded and verified");
                    cx.global_mut::<Flow>().prepared = Some(prepared);
                    button::set_status(UpdateStatus::Ready(shown), cx);
                }
                Err(error @ (Error::Offline | Error::RateLimited)) => {
                    log::info!("updates: the download of {shown} stopped, the next check tries again: {error}");
                    button::set_status(UpdateStatus::None, cx);
                }
                Err(error) => failed(&shown, &error, cx),
            }
        });
    })
    .detach();
}

/// Installs the downloaded update and restarts, unless something holds the restart up: then it waits for the press.
fn install(cx: &mut App) {
    let Some(mut prepared) = cx.global_mut::<Flow>().prepared.take() else {
        return;
    };
    let shown = version(prepared.version());
    if let Some(waiting) = button::waiting(cx) {
        log::info!("updates: the restart waits ({waiting:?})");
        cx.global_mut::<Flow>().prepared = Some(prepared);
        button::set_status(UpdateStatus::Ready(shown), cx);
        return;
    }
    log::info!("updates: installing {shown}");
    button::set_status(UpdateStatus::Installing(shown.clone()), cx);
    cx.global_mut::<Flow>().busy = true;
    let installing = cx.background_spawn(async move {
        let installed = prepared.install();
        (prepared, installed)
    });
    cx.spawn(async move |cx: &mut AsyncApp| {
        let (prepared, installed) = installing.await;
        cx.update(|cx| installed_now(prepared, installed, &shown, cx));
    })
    .detach();
}

fn installed_now(prepared: Prepared, installed: Result<Restart, Error>, shown: &SharedString, cx: &mut App) {
    let flow = cx.global_mut::<Flow>();
    flow.busy = false;
    match installed {
        Ok(restart) => {
            log::info!(
                "updates: {shown} is in: the app restarts into {}",
                restart.program.display()
            );
            flow.installed = Some(restart);
            // Package prompts may have waited while a call arrived or a draft was typed.
            // Remember the successful install separately so the next press only restarts.
            flow.prepared = Some(prepared);
            restart_when_ready(cx);
        }
        Err(Error::Cancelled) => {
            log::info!("updates: the password prompt was closed");
            flow.prepared = Some(prepared);
            button::set_status(UpdateStatus::Ready(shown.clone()), cx);
        }
        Err(error) => failed(shown, &error, cx),
    }
}

/// Re-check holds after installation as well as on a later press; never quit automatically once held.
fn restart_when_ready(cx: &mut App) {
    if let Some(waiting) = button::waiting(cx) {
        log::info!("updates: the restart waits ({waiting:?})");
        if let Some(prepared) = &cx.global::<Flow>().prepared {
            button::set_status(UpdateStatus::Ready(version(prepared.version())), cx);
        }
        return;
    }
    let flow = cx.global_mut::<Flow>();
    flow.restart.set(flow.installed.take());
    flow.busy = true;
    let quit = Rc::clone(&flow.quit);
    quit(cx);
}

/// The update didn't go through: said on the button. The next check, or the press, starts over from the download.
fn failed(shown: &SharedString, error: &Error, cx: &mut App) {
    log::warn!("updates: {shown} wasn't installed: {error}");
    button::set_status(
        UpdateStatus::Failed {
            version: shown.clone(),
            reason: error.to_string().into(),
        },
        cx,
    );
}

/// The arguments' environment a restart into an update clears: an AppImage's runtime sets them again.
pub const APPIMAGE_ENV: [&str; 4] = ["APPIMAGE", "APPDIR", "ARGV0", "OWD"];

/// What holds a restart up in `desktop`'s window: its calls, its unsent messages.
#[must_use]
pub fn holds(desktop: gpui::WeakEntity<teams_ui::Desktop>) -> Holds {
    Rc::new(move |cx: &App| {
        desktop
            .upgrade()
            .and_then(|desktop| desktop.read(cx).update_waiting(cx))
    })
}

// The flow against the fake feed's Linux release trees. A separate attribute keeps the literal `cfg(test)` clippy's
// test allowances look for.
#[cfg(test)]
#[cfg(unix)]
#[path = "../tests/support/update.rs"]
mod tests;
