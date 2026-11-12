//! The read-only mode (`ngteams --read-only`, or `TEAMS_READ_ONLY=1`; AGENTS.md, "Agents run the personal build only
//! read-only"; `docs/personal-build.md`, "What agents may do"): the user's own account, looked at and navigated in a
//! window of the UI sandbox, sending nothing that anyone sees or that changes the account. It is decided at the start
//! and nothing turns it off while the app runs. `scripts/run-personal-read-only.sh` runs it.
//!
//! - **Nothing that writes goes out**, refused below the views: the engine's transports are read-only
//!   (`teams_proto::HttpTransport::read_only`), so a message, an edit, a delete, a reaction, typing, a read receipt, a
//!   chat created, an availability or a status, a registration and any call write is refused before sending and
//!   logged to the snapshot's diagnostic log as `refused_read_only` (method, host and path shape). Reads of
//!   every kind go (reads by `POST` too), and the session's own token renewal. The views offer no write
//!   (`teams_ui::Core::read_only`), the settings send no typing, read receipt or presence
//!   (`teams_ui::SettingsModel::read_only`), the rail adds and removes no account.
//! - **No socket on the account**: no push channel (the user's own `ngteams` keeps its one Trouter connection and its
//!   endpoint's presence; this one catches up by reads, `teams_core::account::READ_ONLY_POLL`) and no calling (no
//!   PipeWire, camera or portal is opened, no calling endpoint registered, no call shown).
//! - **Only in the UI sandbox** ([`window_allowed_here`]): no window on the desktop, `DISPLAY` unset; no tray item, and the
//!   notifications go to the bus it is given (the sandbox's).
//! - **On a snapshot** ([`crate::snapshot`]): its files are a copy (`--data <dir>`, laid out as
//!   [`crate::snapshot::layout`]), refused when they overlap the user's own ([`check_apart`]), so it never opens the
//!   user's store, never takes its engine lock and writes only the snapshot's settings and diagnostic logs.
//! - **Its credentials through a broker** started outside the sandbox (`ngteams secrets --account <UPN> --fd <n>`,
//!   [`broker_scope`]: `get` only, a `put` or a `delete` refused), the app's end (`--secrets-fd`) used through an
//!   [`OverlaySecretStore`]: a token renewed while it runs (a rotated refresh token included) lives in its memory only,
//!   and the user's stored items stay as they are. An account whose stored token can't be used shows "signed out" and
//!   is never signed in: no sign-in window opens.

use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;
use teams_auth::broker::BrokerStream;

use crate::accounts_file::PersonalPaths;
use crate::accounts_file::overlapping;
use teams_auth::broker::{BrokerScope, BrokerSecretStore, Note, Outcome, ServeError, serve};
use teams_auth::secrets::{OverlaySecretStore, SecretStore};
#[cfg(target_os = "linux")]
use ui_sandbox::{SANDBOX_ENV, in_sandbox, names_wayland_display};

use crate::KEYRING_APPLICATION;

/// The environment variable that turns the read-only mode on like `--read-only` (set to anything but empty or `0`).
pub const ENV: &str = "TEAMS_READ_ONLY";

/// The store the read-only mode's session takes its tokens from: the broker's, changes kept in memory.
pub type ReadOnlySecrets = OverlaySecretStore<BrokerSecretStore>;

/// Whether the read-only mode is asked for: `flag` (`--read-only`), or [`ENV`]'s value `env`.
#[must_use]
pub fn asked(flag: bool, env: Option<&OsStr>) -> bool {
    flag || env.is_some_and(|value| !value.is_empty() && value != "0")
}

/// Whether the read-only mode may open its window with this `WAYLAND_DISPLAY`, sandbox opt-in (`UI_SANDBOX_DIR`) and
/// `DISPLAY`: only on the UI sandbox's compositor ([`ui_sandbox::in_sandbox`]), with
/// `DISPLAY` unset.
///
/// # Errors
/// Why not, as the message to print.
#[cfg(target_os = "linux")]
pub fn window_allowed(wayland: Option<&OsStr>, sandbox: Option<&OsStr>, display: Option<&OsStr>) -> Result<(), String> {
    if display.is_some_and(|display| !display.is_empty()) {
        return Err(
            "read-only: DISPLAY is set; refusing to start where GPUI could fall back to X11 (run it in the UI sandbox: \
             scripts/run-personal-read-only.sh)"
                .into(),
        );
    }
    if !names_wayland_display(wayland) || !in_sandbox(wayland, sandbox) {
        return Err(format!(
            "read-only: not the UI sandbox's compositor (WAYLAND_DISPLAY must be an absolute socket inside the dir \
             {SANDBOX_ENV} names); refusing to open a window on the desktop (run it with \
             scripts/run-personal-read-only.sh)"
        ));
    }
    Ok(())
}

/// [`window_allowed`] for this process's environment.
///
/// # Errors
/// Why not.
pub fn window_allowed_here() -> Result<(), String> {
    #[cfg(windows)]
    {
        teams_platform::windows_sandbox::window_allowed_here()
            .map_err(|error| format!("read-only: {error}; run scripts/run-personal-read-only.ps1"))
    }
    #[cfg(target_os = "macos")]
    {
        Err(
            "read-only: macOS has no UI sandbox for agents; a read-only window runs only in the Linux UI sandbox \
             (scripts/run-personal-read-only.sh) or the win11 VM console"
                .to_owned(),
        )
    }
    #[cfg(target_os = "linux")]
    window_allowed(
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
        std::env::var_os(SANDBOX_ENV).as_deref(),
        std::env::var_os("DISPLAY").as_deref(),
    )
}

/// Refuses snapshot files (`snapshot`) that are, contain or lie inside the user's own (`mine`): the read-only mode
/// never opens the user's store or writes the user's files.
///
/// # Errors
/// Which overlap, as the message to print.
pub fn check_apart(snapshot: &PersonalPaths, mine: &PersonalPaths) -> Result<(), String> {
    for ours in [snapshot.config_dir(), snapshot.data_dir()] {
        for theirs in [mine.config_dir(), mine.data_dir()] {
            if overlapping(ours, theirs) {
                return Err(format!(
                    "read-only: {} overlaps your own Teams files ({}); it runs only on a snapshot \
                     (ngteams snapshot --to <dir>)",
                    ours.display(),
                    theirs.display()
                ));
            }
        }
    }
    Ok(())
}

/// The snapshot at `dir`, checked apart from the user's own files (`mine`).
///
/// # Errors
/// As [`check_apart`]; `dir` holding no `accounts.toml` (not a snapshot).
pub fn snapshot_paths(dir: &Path, mine: &PersonalPaths) -> Result<PersonalPaths, String> {
    let paths = crate::snapshot::layout(dir);
    check_apart(&paths, mine)?;
    if !paths.accounts_file().is_file() {
        return Err(format!(
            "read-only: {} holds no snapshot (no {}): make one with ngteams snapshot --to <dir>",
            dir.display(),
            paths.accounts_file().display()
        ));
    }
    Ok(paths)
}

/// What the read-only mode's broker serves (`ngteams secrets --account <UPN>`): `upn`'s `ngteams` items, `get`
/// only.
#[must_use]
pub fn broker_scope(upn: &str) -> BrokerScope {
    BrokerScope::new(KEYRING_APPLICATION, upn).get_only()
}

/// The read-only mode's secret store over the broker's socket `socket` (see the module docs).
///
/// # Errors
/// The socket's mode can't be set.
pub fn secrets(socket: BrokerStream) -> std::io::Result<ReadOnlySecrets> {
    Ok(OverlaySecretStore::new(BrokerSecretStore::new(
        socket,
        KEYRING_APPLICATION,
    )?))
}

/// Serves `store`'s items in `scope` on `socket` until it closes, noting each refused or failed request on `log` (its
/// operation, account and audience, never a value). Runs on the caller's tokio runtime.
///
/// # Errors
/// The socket can't be used, or a frame was malformed or oversized (the socket is closed then).
pub async fn serve_broker<S: SecretStore>(
    socket: BrokerStream,
    scope: &BrokerScope,
    store: &S,
    log: &mut impl Write,
) -> Result<(), ServeError> {
    let socket = teams_auth::broker::into_async(socket)?;
    serve(socket, store, scope, |note: &Note<'_>| {
        if matches!(
            note.outcome,
            Outcome::Refused | Outcome::RefusedGetOnly | Outcome::Failed
        ) {
            let _ = writeln!(log, "teams secrets: {note}");
        }
    })
    .await
}
