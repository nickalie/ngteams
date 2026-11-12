//! Lab startup locations and sandbox reach. The agent checks account traffic before each live run.

#[cfg(target_os = "linux")]
use std::ffi::OsStr;

use crate::accounts_file::{PersonalAccount, PersonalLocations, PersonalPaths, overlapping};

/// The lab's fixed credential namespace and directory under the XDG data home.
pub const LAB_KEYRING_APPLICATION: &str = "ngteams-lab";

/// A single path component for a lab account's alias.
///
/// # Errors
/// An empty alias or one containing anything except ASCII letters, digits, `_` and `-`.
pub fn alias(value: &str) -> Result<String, String> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("a lab alias must contain only ASCII letters, digits, '_' or '-'".into());
    }
    Ok(value.to_owned())
}

/// The alias's account configuration and data in one directory, under the XDG data home.
///
/// # Errors
/// Invalid alias, unavailable data-home parent, or overlap with the user's own config/data directories.
pub fn paths_for(value: &str, mine: &PersonalLocations) -> Result<PersonalPaths, String> {
    let alias = alias(value)?;
    let data_home = mine
        .data_dir
        .parent()
        .ok_or("lab: the XDG data home cannot be resolved")?;
    // Windows and macOS keep `config` and `data` under one `ngteams` directory: the lab's sits beside it
    #[cfg(windows)]
    let data_home = data_home.parent().ok_or("lab: Local AppData cannot be resolved")?;
    #[cfg(target_os = "macos")]
    let data_home = data_home
        .parent()
        .ok_or("lab: Application Support cannot be resolved")?;
    let root = data_home.join(LAB_KEYRING_APPLICATION).join(alias);
    let paths = PersonalPaths::new(root.clone(), root);
    check_apart(
        &paths,
        &PersonalPaths::new(mine.config_dir.clone(), mine.data_dir.clone()),
    )?;
    Ok(paths)
}

/// [`paths_for`] from this process's XDG environment.
///
/// # Errors
/// Missing absolute home/XDG directories, invalid alias or an overlapping directory.
pub fn paths_from_env(alias: &str) -> Result<PersonalPaths, String> {
    let mine = PersonalLocations::from_env().ok_or("lab: the home/XDG directories cannot be resolved")?;
    paths_for(alias, &mine)
}

/// The single account provisioned for an alias, including a pending first sign-in.
///
/// # Errors
/// An unreadable account file, or an alias without exactly one account.
pub fn account(paths: &PersonalPaths) -> Result<PersonalAccount, String> {
    let accounts = PersonalAccount::load_all(paths).map_err(|error| error.to_string())?;
    match accounts.as_slice() {
        [account] => Ok(account.clone()),
        _ => Err("a lab alias needs exactly one account in its accounts.toml".into()),
    }
}

/// Refuses lab files that would open or change the user's own app files, resolving existing symlinks.
///
/// # Errors
/// The first overlapping config/data directory pair.
pub fn check_apart(lab: &PersonalPaths, mine: &PersonalPaths) -> Result<(), String> {
    for ours in [lab.config_dir(), lab.data_dir()] {
        for theirs in [mine.config_dir(), mine.data_dir()] {
            if overlapping(ours, theirs) {
                return Err(format!(
                    "lab: {} overlaps your own Teams files ({})",
                    ours.display(),
                    theirs.display()
                ));
            }
        }
    }
    Ok(())
}

/// Only a compositor inside the UI sandbox, with no X11 fallback.
///
/// # Errors
/// The same sandbox reach requirements as the read-only mode.
#[cfg(target_os = "linux")]
pub fn window_allowed(wayland: Option<&OsStr>, sandbox: Option<&OsStr>, display: Option<&OsStr>) -> Result<(), String> {
    crate::read_only::window_allowed(wayland, sandbox, display).map_err(|error| {
        error
            .replace("scripts/run-personal-read-only.sh", "scripts/run-lab.sh")
            .replacen("read-only:", "lab:", 1)
    })
}

/// The variable `scripts/run-lab-macos.py` sets on the app it starts: on macOS the logged-in console is the lab's
/// authorized GUI place, as the win11 VM's console is on Windows, only through that runner.
#[cfg(target_os = "macos")]
pub const MACOS_CONSOLE_RUNNER: &str = "NGTEAMS_MACOS_CONSOLE_RUNNER";

/// Whether a macOS lab window may open: only when the macOS lab runner started the app (`runner` is its marker).
///
/// # Errors
/// Without the runner's marker.
#[cfg(target_os = "macos")]
pub fn macos_console_allowed(runner: Option<&std::ffi::OsStr>) -> Result<(), String> {
    if runner.is_some_and(|value| value == "1") {
        Ok(())
    } else {
        Err("lab: on macOS a lab window opens only through scripts/run-lab-macos.py".to_owned())
    }
}

/// Verifies the current process's platform-specific UI sandbox.
///
/// # Errors
/// A desktop display or missing sandbox compositor.
pub fn window_allowed_here() -> Result<(), String> {
    #[cfg(windows)]
    {
        teams_platform::windows_sandbox::window_allowed_here()
            .map_err(|error| format!("lab: {error}; run scripts/run-lab.ps1"))
    }
    #[cfg(target_os = "macos")]
    {
        macos_console_allowed(std::env::var_os(MACOS_CONSOLE_RUNNER).as_deref())
    }
    #[cfg(target_os = "linux")]
    window_allowed(
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
        std::env::var_os(ui_sandbox::SANDBOX_ENV).as_deref(),
        std::env::var_os("DISPLAY").as_deref(),
    )
}
