//! The native Teams desktop client: account files, sign-in, engines, media and views.
//!
//! [`accounts_file`] owns account metadata, XDG paths and legacy file migration. [`setup`] chooses accounts,
//! [`signin`] performs host-gated sign-in, and [`engine`] starts one engine and store per account.
//! [`accounts`] shares one call floor and device set across the accounts shown by [`app`].
//! [`own_prompt`] does what the sign-in screen asks about an own server, and [`own_admin`] asks that server's
//! account management: its first administrator, a password that must change, and the Manage users page.
//!
//! [`refusal`] rejects test-support and unmarked builds before opening anything; lab-marked builds only run lab mode.
//! [`read_only`] runs snapshots in the UI sandbox with a get-only credential broker, no writes, calls or push.
//! [`lab`] runs one lab alias in the sandbox with writable broker credentials and sandbox media.
//! [`ctl`] serves `ngteamsctl`'s control socket over the running app.
//! [`handoff`] hands a second launch, or a Teams link, to the app already running over that socket.
//! `update` installs a newer release of an app installed from one, from the title bar (Linux only for now).
//! [`prepare`] is the installer's run of the app, which does the slow work of a first start at install time.

pub mod accounts;
pub mod accounts_file;
pub mod ai_keys;
pub mod app;
pub mod calling;
pub mod cli;
pub mod ctl;
pub mod descriptors;
pub mod engine;
pub mod forget;
pub mod handoff;
pub mod lab;
#[cfg(target_os = "macos")]
pub mod mac_app;
pub mod own_admin;
pub mod own_prompt;
pub mod prepare;
pub mod read_only;
pub mod refusal;
pub mod restart;
pub mod run_end;
pub mod secrets;
pub mod setup;
pub mod signin;
pub mod snapshot;
pub mod update;

pub use refusal::refusal;

#[cfg(test)]
#[path = "../tests/support/log_capture.rs"]
mod log_capture;

/// The application's credential namespace on the desktop Secret Service; only this crate names it.
pub const KEYRING_APPLICATION: &str = "ngteams";
