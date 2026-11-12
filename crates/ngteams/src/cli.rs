//! The command line: `ngteams` (the app for the accounts `accounts.toml` holds; before the first sign-in, or when an
//! account must sign in again, its window asks), `ngteams --account <UPN>` (a shortcut past that screen: the account to
//! sign in, added when it is a new one, through the sign-in window at once), `ngteams --forget <UPN>` (signs out and
//! forgets that account: its `ngteams` items, its `accounts.toml` entry and its saved region, refused while
//! Teams runs for it; its store stays, to delete by hand).
//!
//! The read-only mode (`crate::read_only`, run by `scripts/run-personal-read-only.sh`):
//! `ngteams --read-only --account <UPN> --data <snapshot> --secrets-fd <n> [--trace-fd <n>]` (or `TEAMS_READ_ONLY=1`
//! in place of `--read-only`), the window over a snapshot in the UI sandbox only; `ngteams snapshot --to <dir>` (the
//! snapshot of the user's files it runs on) and `ngteams secrets --account <UPN> --fd <n>` (its credential broker,
//! `get` only, run outside the sandbox).
//! Lab mode is `ngteams --lab <alias> --secrets-fd <n> [--trace-fd <n>]`, with writable lab credentials served by
//! `ngteams secrets --lab <alias> --fd <n>` outside its UI sandbox.
//! `--ctl-socket <path>` names where `ngteamsctl`'s control socket is served (`crate::ctl`). The personal app falls
//! back to `$XDG_RUNTIME_DIR/ngteams/ctl.sock`; a lab or read-only app serves only where its runner tells it (its
//! state dir, `teams_ctl::Mode`), and none without it.
//!
//! A second `ngteams`, or `ngteams <link>` (a Teams meeting's join link, as the desktop entry hands it), hands off to
//! the personal app already running (`crate::handoff`) and ends; a link the app doesn't run for yet opens once it
//! starts. Only the personal desktop app takes a link.

use crate::descriptors::RawDescriptor;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// COM appends `-Embedding` when it starts an unpackaged Windows local server. It is a shell startup marker,
/// not an application action: ordinary startup registers the live class factory and rejects old toast generations.
/// Other arguments still pass through Clap, including the runner requirements for lab/read-only launches.
pub fn shell_arguments(args: impl Iterator<Item = std::ffi::OsString>) -> impl Iterator<Item = std::ffi::OsString> {
    args.filter(|arg| !cfg!(windows) || arg != "-Embedding")
}

#[derive(Debug, Parser)]
#[command(
    name = "ngteams",
    about = "Microsoft Teams for your accounts. Prints no token.",
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
    /// Run a lab alias in its UI sandbox, using the lab credential broker and isolated account files.
    #[arg(long, value_name = "ALIAS", value_parser = crate::lab::alias, conflicts_with = "read_only")]
    pub lab: Option<String>,
    /// A work account to sign in (its UPN) before the app starts, skipping the window that asks for it: one of the
    /// accounts already given, or a new one added after them. In the read-only mode: the account to show.
    #[arg(long, value_name = "UPN")]
    pub account: Option<String>,
    /// Sign out and forget the account of this UPN: its Secret Service items, its entry in `accounts.toml` and its saved
    /// region; the other accounts stay. Refused while Teams runs for it (quit it first). Its store stays.
    #[arg(long, value_name = "UPN", conflicts_with_all = ["account", "read_only"])]
    pub forget: Option<String>,
    #[command(flatten)]
    pub read_only: ReadOnlyArgs,
    /// Print this build's version and commit, then exit.
    #[arg(long, hide = true)]
    pub build_identity: bool,
    /// Print this build's marker (`lab`, `install` or `none`), then exit.
    #[arg(long, hide = true, conflicts_with = "build_identity")]
    pub build_marker: bool,
    /// Install the sign-in window's web engine from a downloaded archive (the release's
    /// `ngteams-web-engine-*.tar.zst`), for a computer that can't download it; the same checks apply.
    #[arg(long, value_name = "ARCHIVE", conflicts_with_all = ["forget", "url"])]
    pub install_web_engine: Option<PathBuf>,
    /// Where `ngteamsctl`'s control socket is served; `$XDG_RUNTIME_DIR/ngteams/ctl.sock` by default. A lab or
    /// read-only app serves one only where this names, as its runner gives it.
    #[arg(long, value_name = "PATH")]
    pub ctl_socket: Option<PathBuf>,
    /// A Teams link to open (a meeting's join link, `msteams:` or `https://teams.microsoft.com/l/meetup-join/…`):
    /// handed to the app already running, or opened once it starts.
    #[arg(value_name = "URL", conflicts_with = "forget")]
    pub url: Option<String>,
}

/// The read-only mode's arguments.
#[derive(Debug, Args)]
pub struct ReadOnlyArgs {
    /// The read-only mode: sends nothing anyone sees or that changes the account, opens no push channel and no calls,
    /// runs on a snapshot (`--data`) and only in the UI sandbox. `TEAMS_READ_ONLY=1` asks for it too.
    #[arg(long = "read-only")]
    pub read_only: bool,
    /// The snapshot the read-only mode runs on (`ngteams snapshot --to <dir>`); lab paths are fixed by its alias.
    #[arg(long, value_name = "DIR")]
    pub data: Option<PathBuf>,
    /// The credential broker's socket (`ngteams secrets`, the other end of a socket pair), handed to this process at
    /// its start (3 or above): read-only and lab modes use it for the account's tokens.
    #[arg(long, value_name = "N")]
    pub secrets_fd: Option<RawDescriptor>,
    /// Where the view trace goes in read-only or lab mode: a descriptor open for writing, handed to this process at its
    /// start (3 or above).
    #[arg(long, value_name = "N")]
    pub trace_fd: Option<RawDescriptor>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Copies your Teams files (the accounts, the settings, each account's region and a consistent copy of its store,
    /// read without writing it) into a new directory, for the read-only mode to run on. Writes nothing of yours.
    Snapshot {
        /// The new (or empty) directory, apart from your own Teams files.
        #[arg(long, value_name = "DIR")]
        to: PathBuf,
    },
    /// The credential broker, run outside the UI sandbox. Personal --account serves get only; --lab serves that
    /// alias's ngteams-lab items with get, put and delete. Other accounts/applications are refused.
    Secrets {
        /// The account (its UPN).
        #[arg(long, value_name = "UPN", required_unless_present = "lab", conflicts_with = "lab")]
        account: Option<String>,
        /// The lab alias, whose account is read from its isolated accounts.toml.
        #[arg(long, value_name = "ALIAS", value_parser = crate::lab::alias, required_unless_present = "account")]
        lab: Option<String>,
        /// The socket's descriptor, handed to this process at its start (3 or above).
        #[arg(long, value_name = "N")]
        fd: RawDescriptor,
    },
}

/// The read-only mode, as the command line gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadOnlyMode {
    pub account: String,
    pub data: PathBuf,
    pub secrets_fd: RawDescriptor,
    pub trace_fd: Option<RawDescriptor>,
    /// Where its control socket is served (`--ctl-socket`); none when its runner gives none.
    pub ctl_socket: Option<PathBuf>,
}

/// A lab alias's sending-mode window over its fixed paths and writable credential broker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabMode {
    pub alias: String,
    pub secrets_fd: RawDescriptor,
    pub trace_fd: Option<RawDescriptor>,
    /// Where its control socket is served (`--ctl-socket`); none when its runner gives none.
    pub ctl_socket: Option<PathBuf>,
}

impl Cli {
    /// What this launch of the personal desktop app hands the app already running ([`crate::handoff`]): the link
    /// given, otherwise `activate`; nothing for `--forget`, `--account` (which keep their own way), a subcommand or the
    /// lab.
    #[must_use]
    pub fn hand_off(&self) -> Option<teams_ctl::protocol::Request> {
        if self.command.is_some() || self.forget.is_some() || self.account.is_some() || self.lab.is_some() {
            return None;
        }
        Some(self.url.clone().map_or(teams_ctl::protocol::Request::Activate, |url| {
            teams_ctl::protocol::Request::OpenLink { url }
        }))
    }

    /// Whether either the app or the credential-broker subcommand explicitly selects lab mode.
    #[must_use]
    pub fn lab_requested(&self) -> bool {
        self.lab.is_some() || matches!(&self.command, Some(Command::Secrets { lab: Some(_), .. }))
    }

    /// The lab mode and its inherited descriptors; its paths cannot be overridden with --data.
    ///
    /// # Errors
    /// Missing broker, conflicting snapshot/read-only arguments, or a descriptor given twice.
    pub fn lab_mode(&self) -> Result<Option<LabMode>, String> {
        let Some(alias) = &self.lab else {
            return Ok(None);
        };
        let args = &self.read_only;
        if args.read_only || args.data.is_some() {
            return Err("--lab uses its own sending mode and fixed paths; do not give --read-only or --data".into());
        }
        if self.url.is_some() {
            return Err("--lab opens no link given on its command line".into());
        }
        let secrets_fd = args.secrets_fd.ok_or("the lab mode needs --secrets-fd <n>")?;
        check_descriptors(secrets_fd, args.trace_fd)?;
        Ok(Some(LabMode {
            alias: alias.clone(),
            secrets_fd,
            trace_fd: args.trace_fd,
            ctl_socket: self.ctl_socket.clone(),
        }))
    }

    /// The read-only mode when `--read-only` or `env` ([`crate::read_only::ENV`]'s value) asks for it, with what it
    /// needs; `Ok(None)` when it isn't asked for (and none of its arguments is given).
    ///
    /// # Errors
    /// What is missing or given twice, as the message to print.
    pub fn read_only_mode(&self, env: Option<&std::ffi::OsStr>) -> Result<Option<ReadOnlyMode>, String> {
        let args = &self.read_only;
        if self.lab.is_some() {
            if crate::read_only::asked(args.read_only, env) {
                return Err("--lab cannot be combined with the read-only mode".into());
            }
            return Ok(None);
        }
        if !crate::read_only::asked(args.read_only, env) {
            if args.data.is_some() || args.secrets_fd.is_some() || args.trace_fd.is_some() {
                return Err("--data, --secrets-fd and --trace-fd are the read-only mode's: give --read-only".into());
            }
            return Ok(None);
        }
        if self.forget.is_some() {
            return Err("the read-only mode forgets no account".into());
        }
        if self.url.is_some() {
            return Err("the read-only mode opens no link given on its command line".into());
        }
        let missing = |what: &str| format!("the read-only mode needs {what}");
        let account = self.account.clone().ok_or_else(|| missing("--account <UPN>"))?;
        let data = args.data.clone().ok_or_else(|| missing("--data <snapshot>"))?;
        let secrets_fd = args.secrets_fd.ok_or_else(|| missing("--secrets-fd <n>"))?;
        check_descriptors(secrets_fd, args.trace_fd)?;
        Ok(Some(ReadOnlyMode {
            account,
            data,
            secrets_fd,
            trace_fd: args.trace_fd,
            ctl_socket: self.ctl_socket.clone(),
        }))
    }
}

fn check_descriptors(secrets_fd: RawDescriptor, trace_fd: Option<RawDescriptor>) -> Result<(), String> {
    if trace_fd == Some(secrets_fd) {
        return Err(format!(
            "descriptor {secrets_fd} is named for both --secrets-fd and --trace-fd"
        ));
    }
    Ok(())
}
