//! Where the control socket of each mode is.
//!
//! - personal: `$XDG_RUNTIME_DIR/ngteams/ctl.sock`;
//! - a lab alias: `ctl.sock` in that alias's runner state dir, `$XDG_STATE_HOME/ngteams-lab/<alias>/`
//!   (`scripts/run-lab.sh`);
//! - read-only: `ctl.sock` in the read-only runner's state dir, `$XDG_STATE_HOME/ngteams-read-only/run/`
//!   (`scripts/run-personal-read-only.sh`);
//! - `$NGTEAMS_CTL_SOCKET` ([`ENV_SOCKET`]) overrides any of them for a client.
//!
//! A lab or read-only app runs in the UI sandbox, whose environment is cleared and whose runtime dir is its own, so
//! its runner hands it the path ([`Env::mode_path`]) rather than the app working it out inside.
//!
//! `XDG_STATE_HOME` falls back to `~/.local/state` when unset or relative, as both runners do; a relative
//! `XDG_RUNTIME_DIR` or `HOME` counts as unset.
//! On Windows these are logical endpoint identities: the native transport hashes each absolute path and the user
//! SID into a local pipe name. `from_process` uses LOCALAPPDATA for state/runtime and USERPROFILE for the fallback
//! home; no filesystem socket is created.
//! On macOS, which has no `XDG_RUNTIME_DIR`, the personal socket is under the per-user `$TMPDIR` (private to the user
//! and cleared at boot, as a runtime dir is) and the state home is `~/Library/Application Support`.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// The variable naming the socket a client connects to, whatever the mode.
pub const ENV_SOCKET: &str = "NGTEAMS_CTL_SOCKET";

/// The socket's file name in its directory.
pub const SOCKET_NAME: &str = "ctl.sock";

/// The personal socket's directory under `$XDG_RUNTIME_DIR`. A constant, so the directory isn't written as a
/// keyring application argument is (`ngteams/tests/apart.rs` keeps that name to the app's own crate).
const PERSONAL_DIR: &str = "ngteams";

/// Which app a socket belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The installed app with the user's own accounts.
    Personal,
    /// A lab alias run by `scripts/run-lab.sh`.
    Lab(String),
    /// The installed app run read-only by `scripts/run-personal-read-only.sh`.
    ReadOnly,
}

/// Why a mode's socket has no path.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("XDG_RUNTIME_DIR isn't set to an absolute path")]
    NoRuntimeDir,
    #[error("neither XDG_STATE_HOME nor HOME is set to an absolute path")]
    NoStateHome,
    #[error("a lab alias is ASCII letters, digits, '_' and '-', not {0:?}")]
    BadAlias(String),
}

/// The environment the paths are worked out from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    /// `$NGTEAMS_CTL_SOCKET`.
    pub socket: Option<OsString>,
    /// Unix `$XDG_RUNTIME_DIR`, macOS `$TMPDIR`, or Windows local profile runtime directory.
    pub runtime_dir: Option<OsString>,
    /// Unix `$XDG_STATE_HOME`, macOS `~/Library/Application Support`, or Windows `$LOCALAPPDATA`.
    pub state_home: Option<OsString>,
    /// Unix `$HOME`, or Windows `$USERPROFILE`.
    pub home: Option<OsString>,
}

impl Env {
    /// The process's own environment.
    #[must_use]
    #[cfg(all(unix, not(target_os = "macos")))]
    pub fn from_process() -> Self {
        Self {
            socket: std::env::var_os(ENV_SOCKET),
            runtime_dir: std::env::var_os("XDG_RUNTIME_DIR"),
            state_home: std::env::var_os("XDG_STATE_HOME"),
            home: std::env::var_os("HOME"),
        }
    }

    /// macOS: the per-user `$TMPDIR` as the runtime dir, `~/Library/Application Support` as the state home.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn from_process() -> Self {
        let home = std::env::var_os("HOME");
        Self {
            socket: std::env::var_os(ENV_SOCKET),
            runtime_dir: std::env::var_os("TMPDIR"),
            state_home: home
                .as_ref()
                .filter(|home| Path::new(home).is_absolute())
                .map(|home| Path::new(home).join("Library/Application Support").into_os_string()),
            home,
        }
    }

    /// Native Windows state is under the current user's local profile; no roaming account data or pipe name.
    #[cfg(windows)]
    #[must_use]
    pub fn from_process() -> Self {
        let local = std::env::var_os("LOCALAPPDATA");
        Self {
            socket: std::env::var_os(ENV_SOCKET),
            runtime_dir: local
                .as_ref()
                .map(|dir| PathBuf::from(dir).join("ngteams/runtime").into_os_string()),
            state_home: local,
            home: std::env::var_os("USERPROFILE"),
        }
    }

    /// The socket a client of `mode` connects to: `$NGTEAMS_CTL_SOCKET` when set, otherwise the mode's own path.
    ///
    /// # Errors
    /// As [`Env::mode_path`], when there is no override.
    pub fn socket_path(&self, mode: &Mode) -> Result<PathBuf, PathError> {
        match self.socket.as_ref().filter(|socket| !socket.is_empty()) {
            Some(socket) => Ok(PathBuf::from(socket)),
            None => self.mode_path(mode),
        }
    }

    /// The mode's own socket, with no override: what the app serves on by default and what a runner hands its app.
    ///
    /// # Errors
    /// [`PathError::NoRuntimeDir`] for the personal mode without an absolute `XDG_RUNTIME_DIR`;
    /// [`PathError::NoStateHome`] for the others without an absolute `XDG_STATE_HOME` or `HOME`;
    /// [`PathError::BadAlias`] for a lab alias that isn't a plain name.
    pub fn mode_path(&self, mode: &Mode) -> Result<PathBuf, PathError> {
        match mode {
            Mode::Personal => {
                let runtime = absolute(self.runtime_dir.as_ref()).ok_or(PathError::NoRuntimeDir)?;
                Ok(runtime.join(PERSONAL_DIR).join(SOCKET_NAME))
            }
            Mode::Lab(alias) => {
                if !plain_alias(alias) {
                    return Err(PathError::BadAlias(alias.clone()));
                }
                Ok(self.state_home()?.join("ngteams-lab").join(alias).join(SOCKET_NAME))
            }
            Mode::ReadOnly => Ok(self
                .state_home()?
                .join("ngteams-read-only")
                .join("run")
                .join(SOCKET_NAME)),
        }
    }

    fn state_home(&self) -> Result<PathBuf, PathError> {
        if let Some(state) = absolute(self.state_home.as_ref()) {
            return Ok(state);
        }
        absolute(self.home.as_ref())
            .map(|home| home.join(".local").join("state"))
            .ok_or(PathError::NoStateHome)
    }
}

/// The socket a client of `mode` connects to, from the process's environment ([`Env::socket_path`]).
///
/// # Errors
/// As [`Env::socket_path`].
pub fn socket_path(mode: &Mode) -> Result<PathBuf, PathError> {
    Env::from_process().socket_path(mode)
}

/// Make an endpoint's filesystem parent privately (Unix 0700, Windows protected ACL). Existing directories are left
/// as they are: an override may name a directory of the user's. A Unix server binds its socket 0600; a Windows
/// server creates a local pipe with its own ACL, independently of this logical path's parent.
///
/// # Errors
/// The directory couldn't be made, or the path's parent is there but isn't a directory.
pub fn prepare_dir(socket: &Path) -> io::Result<()> {
    let dir = match socket.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    teams_platform::private::create_dir_all(dir)
}

fn absolute(value: Option<&OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|path| path.is_absolute())
}

fn plain_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}
