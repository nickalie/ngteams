//! `ngteamsctl`'s command line: its arguments, the socket they choose, the request each command asks, and the exit
//! code it ends with.
//!
//! - **The socket**: `--socket <path>`, otherwise `$NGTEAMS_CTL_SOCKET`, otherwise the mode's own path: a lab alias's
//!   (`--lab <alias>`), the read-only runner's (`--read-only`), or the personal app's ([`crate::path`]).
//! - **Output**: human-readable ([`crate::render`]): tables for chats and messages, one line per event. `--json`
//!   prints each response or event line as JSON instead.
//! - **Exit codes** ([`Exit`]): 0 on success, 1 on an error answer (or a stream that ended badly, or a write whose
//!   outbox row failed; with `--wait`, one not sent), 2 when no app runs on the socket or on a usage error, 3 when
//!   `wait` times out.
//! - **Writes** answer with their outbox row as its first attempt left it (the app answers within [`WRITE_ANSWER`]);
//!   `--wait` waits for it to settle: sent, failed, or `unknown` (it may have arrived), [`WRITE_WAIT`] more at most.
//!   `read` with nothing to mark answers done. The client waits a little past the app's bounds ([`answer_time`]);
//!   a write still unanswered then, or that the app timed out, may still go out, and the error says where to look.
//!   `send` and `edit` read their text from stdin when it is `-`.
//! - **Own status**: `status` alone is the app's; `status get` tells the account's reported status and the one
//!   chosen, `status set` chooses one as the status menu does. `sync` answers with what its catch-up did, the app
//!   answering within [`SYNC_ANSWER`].
//! - **Calls**: `calls` lists the call up and the invitation that rings; `call`, `join`, `answer`, `decline`,
//!   `hangup`, `mute` and `camera` answer with the call's row as the app shows it once the engine took the ask, the
//!   app answering within [`CALL_ANSWER`]. They default to the account ringing or in the call.
//! - **Calendar**: `calendar` lists the account's events of today, or of the days `--from` and `--to` name;
//!   `calendar show <id>` one event. `join meeting <id>` opens a meeting's pre-join as the calendar's Join does, and
//!   with `--now` joins it as the pre-join's Join now does, answering with the call's row as `join` does.
//! - **Events**: `events --follow` prints every event the filters let through until the app closes the stream;
//!   without `--follow`, `events` prints the first one and exits. `wait <kind>` prints the first event of that kind
//!   whose JSON line matches `--match`, within `--timeout` (which the connect and the subscribe spend too). A kind
//!   that names no event kind is a usage error. A refused subscription is an error answer like any other.
//! - **Completions**: `completions bash|fish|zsh` prints that shell's completion script, and `events kinds` the event
//!   kinds, without asking the app.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Args, CommandFactory as _, Parser, Subcommand, ValueEnum};
use clap_complete::Generator as _;
use regex::Regex;

use crate::client::{Client, ClientError, Events, Refused, Timeouts};
use crate::path::{Env, Mode};
use crate::protocol::{
    CALENDAR_ANSWER, CALL_ANSWER, CtlEvent, DATE_FORMAT, EventFilter, EventLine, OpenTarget, OutboxState, Outcome,
    Request, Response, ResponseLine, SYNC_ANSWER, StatusChoice, Toggle, UNANSWERED_WRITE, WRITE_ANSWER, WRITE_WAIT,
    WindowOp,
};
use crate::render;

#[derive(Debug, Parser)]
#[command(
    name = "ngteamsctl",
    version,
    about = "Drive the running ngteams: read chats and messages, open views, and follow what happens."
)]
pub struct Cli {
    /// The account addressed, by UPN, MRI or index (`ngteamsctl accounts`); the account the window shows by default.
    /// `events` and `wait`: only this account's events.
    #[arg(long, global = true, value_name = "UPN|MRI|INDEX")]
    pub account: Option<String>,
    /// Talk to the app of this lab alias (`scripts/run-lab.sh`).
    #[arg(long, global = true, value_name = "ALIAS", conflicts_with = "read_only")]
    pub lab: Option<String>,
    /// Talk to the read-only app (`scripts/run-personal-read-only.sh`).
    #[arg(long, global = true)]
    pub read_only: bool,
    /// The control socket to talk to, whatever the mode; `$NGTEAMS_CTL_SOCKET` when not given.
    #[arg(long, global = true, value_name = "PATH")]
    pub socket: Option<PathBuf>,
    /// Print each response or event line as JSON, as the app sent it.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// The app's version, its accounts with their state, the window and the call up; `status get` and `status set`
    /// the account's own status.
    Status {
        #[command(subcommand)]
        command: Option<StatusCommand>,
    },
    /// Catch the account up with the service now, and print what the catch-up did.
    Sync,
    /// The app's accounts.
    Accounts,
    /// The account's chats, the most recent first.
    Chats {
        /// Only the chats with unread messages.
        #[arg(long)]
        unread: bool,
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
    },
    /// A chat's messages, the newest first.
    Messages {
        /// The chat's id or title.
        chat: String,
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
        /// Only the messages before this message id.
        #[arg(long, value_name = "MESSAGE")]
        before: Option<String>,
    },
    /// The unread counts the badge shows: per chat and the total.
    Unread,
    /// The account's messages that match a query.
    Search {
        /// The words searched for.
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
    },
    /// A chat's members.
    Members {
        /// The chat's id or title.
        chat: String,
    },
    /// The account's activity feed, the newest first, as the Activity pane lists it.
    Activity {
        /// Only the items one of the pane's chips keeps.
        #[arg(long, value_name = "CHIP", value_parser = [
            "all", "unread", "mentions", "reactions", "missed-calls", "replies", "voicemail", "apps",
        ])]
        filter: Option<String>,
        /// Only the items whose title, preview or place holds these words, as the pane's search finds them.
        #[arg(long, value_name = "WORDS")]
        search: Option<String>,
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
    },
    /// Show, hide (to the tray), toggle or focus the main window.
    Window {
        #[arg(value_enum)]
        op: WindowArg,
    },
    /// Open a view in the window.
    Open {
        #[command(subcommand)]
        target: OpenArg,
    },
    /// The accounts the window shows.
    Account {
        #[command(subcommand)]
        command: AccountCommand,
    },
    /// What the window shows.
    Ui {
        #[command(subcommand)]
        command: UiCommand,
    },
    /// Dispatch one of the app's actions to its main window by its full or unique short name; `action list` lists
    /// them.
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    Action {
        #[command(subcommand)]
        command: Option<ActionCommand>,
        /// The action's name (`teams_ui::ShowCalendar`, or `ShowCalendar` when only one action has it).
        #[arg(required = true)]
        name: Option<String>,
    },
    /// Bring the window to the front.
    Activate,
    /// The account's calendar events: today's, or those of the days `--from` and `--to` name; `calendar show <id>`
    /// one event.
    #[command(args_conflicts_with_subcommands = true)]
    Calendar {
        #[command(subcommand)]
        command: Option<CalendarCommand>,
        /// Today's events (what `calendar` alone lists).
        #[arg(long, conflicts_with_all = ["from", "to"])]
        today: bool,
        /// The first day (YYYY-MM-DD); today without it.
        #[arg(long, value_name = "DATE", value_parser = date)]
        from: Option<String>,
        /// The last day, included (YYYY-MM-DD); the first day without it.
        #[arg(long, value_name = "DATE", value_parser = date)]
        to: Option<String>,
    },
    #[command(flatten)]
    Write(WriteCommand),
    #[command(flatten)]
    Call(CallCommand),
    /// Print what happens in the app: with `--follow` every event the filters let through, otherwise the first one.
    /// `events kinds` lists the kinds.
    #[command(args_conflicts_with_subcommands = true)]
    Events {
        #[command(subcommand)]
        command: Option<EventsCommand>,
        /// Go on until the app closes the stream.
        #[arg(long)]
        follow: bool,
        /// Only events of these kinds; `message*` names every kind it starts.
        #[arg(long = "kind", value_name = "KIND", value_delimiter = ',', value_parser = kind)]
        kinds: Vec<String>,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// Wait for the first event of a kind (and matching `--match`), print it and exit 0; exit 3 past `--timeout`.
    Wait {
        /// The event's kind; `message*` names every kind it starts.
        #[arg(value_parser = kind)]
        kind: String,
        /// A regular expression the event's JSON line must match.
        #[arg(long = "match", value_name = "REGEX")]
        pattern: Option<Regex>,
        /// Give up after this many seconds (fractions allowed).
        #[arg(long, value_name = "SECONDS", value_parser = seconds)]
        timeout: Option<Duration>,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// Print the shell's completion script for `ngteamsctl`, to source or put where the shell looks for it.
    Completions {
        #[arg(value_enum)]
        shell: ShellArg,
    },
}

/// The shells `completions` writes a script for.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ShellArg {
    Bash,
    Fish,
    Zsh,
}

/// The writes: each answers with its outbox row (exit 1 when it failed; with `--wait`, unless it was sent), `read`
/// with nothing to mark with done.
#[derive(Debug, Subcommand)]
pub enum WriteCommand {
    /// Send plain text to a chat, each line a paragraph.
    Send {
        /// The chat's id or title.
        chat: String,
        /// The text; `-` reads it from stdin.
        text: String,
        #[command(flatten)]
        wait: WaitArg,
    },
    /// Replace the text of one of the account's messages (its quote of another message kept).
    Edit {
        /// The chat's id or title.
        chat: String,
        /// The message's id.
        message: String,
        /// The new text; `-` reads it from stdin.
        text: String,
        #[command(flatten)]
        wait: WaitArg,
    },
    /// Delete one of the account's messages.
    Delete {
        /// The chat's id or title.
        chat: String,
        /// The message's id.
        message: String,
        #[command(flatten)]
        wait: WaitArg,
    },
    /// React to a message.
    React {
        /// The chat's id or title.
        chat: String,
        /// The message's id.
        message: String,
        /// The reaction's key: like, heart, laugh, surprised, sad, angry, …
        reaction: String,
        #[command(flatten)]
        wait: WaitArg,
    },
    /// Take the account's reaction off a message.
    Unreact {
        /// The chat's id or title.
        chat: String,
        /// The message's id.
        message: String,
        /// The reaction's key.
        reaction: String,
        #[command(flatten)]
        wait: WaitArg,
    },
    /// Mark a chat read up to its latest message.
    Read {
        /// The chat's id or title.
        chat: String,
        #[command(flatten)]
        wait: WaitArg,
    },
    /// Start a chat: the 1:1 chat with one person, a new group chat with more.
    NewChat {
        /// The people, by MRI or UPN.
        #[arg(required = true, num_args = 1..)]
        people: Vec<String>,
        #[command(flatten)]
        wait: WaitArg,
    },
    /// Send an outbox row again (a write that failed).
    Resend {
        /// The row's id, as a write's answer or an `outbox` event names it.
        row: i64,
        #[command(flatten)]
        wait: WaitArg,
    },
}

impl WriteCommand {
    /// Whether it waits for its row to settle.
    #[must_use]
    pub fn waits(&self) -> bool {
        match self {
            Self::Send { wait, .. }
            | Self::Edit { wait, .. }
            | Self::Delete { wait, .. }
            | Self::React { wait, .. }
            | Self::Unreact { wait, .. }
            | Self::Read { wait, .. }
            | Self::NewChat { wait, .. }
            | Self::Resend { wait, .. } => wait.wait,
        }
    }
}

/// The call commands: each asked as the call buttons ask it, of the account `--account` names, otherwise the one
/// ringing or in the call, otherwise the one the window shows; each but `calls` answers with the call's row.
#[derive(Debug, Subcommand)]
pub enum CallCommand {
    /// The call up and the invitation that rings, of every account (only `--account`'s with it).
    Calls,
    /// Call a chat: a 1:1 chat's other member, a group chat's members.
    Call {
        /// The chat's id or title.
        chat: String,
    },
    /// Answer the invitation that rings.
    Answer,
    /// Decline the invitation that rings.
    Decline,
    /// Leave the call up (or cancel it while it rings at its callees).
    Hangup,
    /// Join the running call of a group chat; `join meeting <event>` a calendar event's meeting.
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    Join {
        #[command(subcommand)]
        command: Option<JoinCommand>,
        /// The chat's id or title.
        #[arg(required = true)]
        chat: Option<String>,
    },
    /// Mute the microphone in the call up (`on`), unmute it (`off`), or the other way round (`toggle`).
    Mute {
        #[arg(value_enum)]
        op: ToggleArg,
    },
    /// Turn the camera on or off in the call up, or the other way round (`toggle`).
    Camera {
        #[arg(value_enum)]
        op: ToggleArg,
    },
}

/// `join meeting`.
#[derive(Debug, Subcommand)]
pub enum JoinCommand {
    /// A calendar event's meeting: its pre-join opened, as the calendar's Join opens it; with `--now`, joined at once
    /// as the pre-join's Join now joins it (the microphone on, the camera off, the devices the settings chose).
    Meeting {
        /// The event's id, as `calendar` lists it.
        event: String,
        /// Join at once rather than open the pre-join.
        #[arg(long)]
        now: bool,
    },
}

/// `calendar show`.
#[derive(Debug, Subcommand)]
pub enum CalendarCommand {
    /// One event, by its id as `calendar` lists it.
    Show { event: String },
}

/// A switch set on, off, or to the other state.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ToggleArg {
    On,
    Off,
    Toggle,
}

/// A write's `--wait`.
#[derive(Clone, Copy, Debug, Args)]
pub struct WaitArg {
    /// Answer once the write was sent, failed for good, or is unknown (it may have arrived; only `resend` sends it
    /// again), a minute at most, not as its first attempt left it.
    #[arg(long)]
    pub wait: bool,
}

/// The filters `events` and `wait` share, beside the kind and `--account`.
#[derive(Debug, Args)]
pub struct FilterArgs {
    /// Only this chat's events (its id or title).
    #[arg(long, value_name = "CHAT")]
    pub chat: Option<String>,
    /// Only messages and typing from this person: an MRI, a display name, or one of the app's accounts by UPN.
    #[arg(long, value_name = "WHO")]
    pub from: Option<String>,
    /// Only messages that mention the account.
    #[arg(long)]
    pub mentions: bool,
    /// A `reaction` event carries the message's current reactions.
    #[arg(long)]
    pub detail: bool,
    /// The minutes before a meeting's start at which `meeting_starting` fires (5 and 0 when not given).
    #[arg(long = "lead", value_name = "MINUTES", value_delimiter = ',')]
    pub leads: Vec<u32>,
}

impl FilterArgs {
    fn filter(&self, kinds: Vec<String>) -> EventFilter {
        EventFilter {
            kinds,
            account: None,
            chat: self.chat.clone(),
            from: self.from.clone(),
            mentions_me: self.mentions,
            detail: self.detail,
            leads: self.leads.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum WindowArg {
    Show,
    /// To the tray; refused without a tray item, since closing would quit the app.
    Hide,
    Toggle,
    Focus,
}

#[derive(Debug, Subcommand)]
pub enum OpenArg {
    /// A chat, by id or title.
    Chat { chat: String },
    /// A channel, by id or name.
    Channel { channel: String },
    /// The calendar.
    Calendar,
    /// The Activity pane.
    Activity,
    /// The settings, at a page of them.
    Settings { page: Option<String> },
    /// The search results of a query.
    Search {
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// Show this account (UPN, MRI or index) in the window.
    Switch { to: String },
}

#[derive(Debug, Subcommand)]
pub enum StatusCommand {
    /// The status the app reports for the account, and the one chosen (`automatic` while the app decides).
    Get,
    /// Choose the account's status as its status menu does; `reset` hands it back to the app.
    Set {
        #[arg(value_enum)]
        status: StatusArg,
    },
}

/// The status menu's choices.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum StatusArg {
    Available,
    Busy,
    /// Do not disturb.
    Dnd,
    /// Appear away.
    Away,
    /// Automatic again: the app decides (available while used, nothing reported once idle).
    Reset,
}

#[derive(Debug, Subcommand)]
pub enum UiCommand {
    /// The account shown, the view, the chat open, and the windows.
    State,
}

#[derive(Debug, Subcommand)]
pub enum ActionCommand {
    /// The actions the app has.
    List,
}

#[derive(Debug, Subcommand)]
pub enum EventsCommand {
    /// The event kinds.
    Kinds,
}

/// How `ngteamsctl` ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    Ok = 0,
    /// The app answered with an error, or a stream ended badly.
    Error = 1,
    /// No app runs on the socket, or the command line didn't read.
    NoApp = 2,
    /// `wait` saw no matching event in time.
    Timeout = 3,
}

impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> Self {
        Self::from(exit as u8)
    }
}

/// The socket `cli` talks to (see the module docs).
///
/// # Errors
/// The mode has no path in `env` (no runtime dir, no state home, an alias that isn't a plain name).
pub fn socket(cli: &Cli, env: &Env) -> Result<PathBuf, crate::path::PathError> {
    if let Some(socket) = &cli.socket {
        return Ok(socket.clone());
    }
    let mode = match (&cli.lab, cli.read_only) {
        (Some(alias), _) => Mode::Lab(alias.clone()),
        (None, true) => Mode::ReadOnly,
        (None, false) => Mode::Personal,
    };
    env.socket_path(&mode)
}

/// What the `ngteamsctl` binary runs: the process's arguments and environment, stdout and stderr.
#[must_use]
pub fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            // clap prints help and version on stdout (exit 0) and usage errors on stderr (exit 2)
            let _ = error.print();
            return ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(2));
        }
    };
    run(
        &cli,
        &Env::from_process(),
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
    .into()
}

/// Run `cli` against the socket `env` gives it, printing to `out` and telling errors on `err`.
pub fn run(cli: &Cli, env: &Env, out: &mut dyn Write, err: &mut dyn Write) -> Exit {
    let request = match request(&cli.command, &mut || io::read_to_string(io::stdin())) {
        Ok(Some(request)) => request,
        Ok(None) => return local(&cli.command, out),
        Err(error) => return told(err, Exit::NoApp, &format!("stdin couldn't be read: {error}")),
    };
    let path = match socket(cli, env) {
        Ok(path) => path,
        Err(error) => return told(err, Exit::NoApp, &format!("no socket to talk to: {error}")),
    };
    // `wait --timeout` counts from here: the connect, the hello and the subscribe spend it as the events do
    let deadline = match &cli.command {
        Command::Wait {
            timeout: Some(timeout), ..
        } => Some(Instant::now() + *timeout),
        _ => None,
    };
    let client = match connect(&path, deadline) {
        Ok(client) => client,
        Err(error) => return setup_failed(err, &error, deadline),
    };
    match (request, &cli.command) {
        (Request::Subscribe { filter }, Command::Wait { pattern, .. }) => {
            let until = Until::Match {
                pattern: pattern.as_ref(),
                deadline,
            };
            stream(cli, client, filter, &until, out, err)
        }
        (Request::Subscribe { filter }, Command::Events { follow, .. }) => {
            stream(cli, client, filter, &Until::Events { follow: *follow }, out, err)
        }
        (request, _) => ask(cli, client, request, out, err),
    }
}

/// Run a command that asks the app nothing: `completions` and `events kinds`.
fn local(command: &Command, out: &mut dyn Write) -> Exit {
    match command {
        Command::Completions { shell } => written(completions(*shell, out)),
        _ => written(render::kinds(out)),
    }
}

/// Write `shell`'s completion script for `ngteamsctl` on `out`.
///
/// # Errors
/// `out` couldn't be written.
pub fn completions(shell: ShellArg, out: &mut dyn Write) -> io::Result<()> {
    let mut command = Cli::command();
    command.set_bin_name("ngteamsctl");
    command.build();
    let shell = match shell {
        ShellArg::Bash => clap_complete::Shell::Bash,
        ShellArg::Fish => clap_complete::Shell::Fish,
        ShellArg::Zsh => clap_complete::Shell::Zsh,
    };
    shell.try_generate(&command, out)?;
    out.flush()
}

/// Connect to the app at `path`, each wait no longer than what is left before `deadline`.
fn connect(path: &Path, deadline: Option<Instant>) -> Result<Client, ClientError> {
    let mut timeouts = Timeouts::default();
    if let Some(deadline) = deadline {
        let left = left(deadline).ok_or(ClientError::Timeout)?;
        timeouts = Timeouts {
            connect: timeouts.connect.min(left),
            read: timeouts.read.min(left),
            write: timeouts.write.min(left),
        };
    }
    Client::connect(path, timeouts)
}

/// The time left before `deadline`; none once it passed.
fn left(deadline: Instant) -> Option<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    (!left.is_zero()).then_some(left)
}

/// How early a socket's timeout may fire before the deadline it was set to (the kernel counts in scheduler ticks),
/// and still be the deadline's.
const TICK: Duration = Duration::from_millis(20);

/// The exit a client error before the first event ends with: a timeout at `wait`'s deadline is the deadline's (3),
/// any other as [`failed`].
fn setup_failed(err: &mut dyn Write, error: &ClientError, deadline: Option<Instant>) -> Exit {
    let at_deadline = deadline.is_some_and(|deadline| deadline.saturating_duration_since(Instant::now()) <= TICK);
    if matches!(error, ClientError::Timeout) && at_deadline {
        return timed_out(err);
    }
    failed(err, error)
}

fn timed_out(err: &mut dyn Write) -> Exit {
    told(err, Exit::Timeout, "no matching event came in time")
}

/// An event kind `events --kind` and `wait` take: one of [`CtlEvent::KINDS`] (the stream's own `lagged` left out),
/// or a prefix ending in `*` that names at least one.
fn kind(value: &str) -> Result<String, String> {
    let mut kinds = CtlEvent::KINDS.iter().filter(|kind| **kind != "lagged");
    let known = match value.strip_suffix('*') {
        Some(prefix) => kinds.any(|kind| kind.starts_with(prefix)),
        None => kinds.any(|kind| *kind == value),
    };
    if known {
        Ok(value.to_owned())
    } else {
        Err(format!(
            "{value:?} isn't an event kind (`ngteamsctl events kinds` lists them)"
        ))
    }
}

/// The request a command asks: `events` and `wait` subscribe; a write's text `-` is what `stdin` reads. None for a
/// command that asks the app nothing ([`local`]).
fn request(command: &Command, stdin: &mut dyn FnMut() -> io::Result<String>) -> io::Result<Option<Request>> {
    Ok(Some(match command {
        Command::Completions { .. }
        | Command::Events {
            command: Some(EventsCommand::Kinds),
            ..
        } => return Ok(None),
        Command::Write(write) => write_request(write, stdin)?,
        Command::Call(call) => call_request(call),
        Command::Status { command: None } => Request::Status,
        Command::Status {
            command: Some(StatusCommand::Get),
        } => Request::GetStatus,
        Command::Status {
            command: Some(StatusCommand::Set { status }),
        } => Request::SetStatus {
            status: status_choice(*status),
        },
        Command::Sync => Request::Sync,
        Command::Accounts => Request::Accounts,
        Command::Chats { unread, limit } => Request::Chats {
            unread_only: *unread,
            limit: *limit,
        },
        Command::Messages { chat, limit, before } => Request::Messages {
            chat: chat.clone(),
            limit: *limit,
            before: before.clone(),
        },
        Command::Unread => Request::Unread,
        Command::Search { query, limit } => Request::Search {
            query: query.join(" "),
            limit: *limit,
        },
        Command::Members { chat } => Request::Members { chat: chat.clone() },
        Command::Activity { filter, search, limit } => Request::Activity {
            filter: filter.clone(),
            search: search.clone(),
            limit: *limit,
        },
        Command::Window { op } => Request::Window { op: window_op(*op) },
        Command::Open { target } => Request::Open {
            target: open_target(target),
        },
        Command::Account {
            command: AccountCommand::Switch { to },
        } => Request::SwitchAccount { to: to.clone() },
        Command::Ui {
            command: UiCommand::State,
        } => Request::UiState,
        Command::Action {
            command: Some(ActionCommand::List),
            ..
        } => Request::ActionList,
        Command::Action { name, .. } => Request::Action {
            name: name.clone().unwrap_or_default(),
        },
        Command::Activate => Request::Activate,
        Command::Calendar {
            command: Some(CalendarCommand::Show { event }),
            ..
        } => Request::CalendarEvent { event: event.clone() },
        Command::Calendar { from, to, .. } => Request::Calendar {
            from: from.clone(),
            to: to.clone(),
        },
        Command::Events { kinds, filter, .. } => Request::Subscribe {
            filter: filter.filter(kinds.clone()),
        },
        Command::Wait { kind, filter, .. } => Request::Subscribe {
            filter: filter.filter(vec![kind.clone()]),
        },
    }))
}

/// The request a write asks.
fn write_request(write: &WriteCommand, stdin: &mut dyn FnMut() -> io::Result<String>) -> io::Result<Request> {
    let wait = write.waits();
    let text = |text: &str, stdin: &mut dyn FnMut() -> io::Result<String>| {
        if text == "-" { stdin() } else { Ok(text.to_owned()) }
    };
    Ok(match write {
        WriteCommand::Send { chat, text: given, .. } => Request::Send {
            chat: chat.clone(),
            text: text(given, stdin)?,
            wait,
        },
        WriteCommand::Edit {
            chat,
            message,
            text: given,
            ..
        } => Request::Edit {
            chat: chat.clone(),
            message: message.clone(),
            text: text(given, stdin)?,
            wait,
        },
        WriteCommand::Delete { chat, message, .. } => Request::Delete {
            chat: chat.clone(),
            message: message.clone(),
            wait,
        },
        WriteCommand::React {
            chat,
            message,
            reaction,
            ..
        } => Request::React {
            chat: chat.clone(),
            message: message.clone(),
            reaction: reaction.clone(),
            wait,
        },
        WriteCommand::Unreact {
            chat,
            message,
            reaction,
            ..
        } => Request::Unreact {
            chat: chat.clone(),
            message: message.clone(),
            reaction: reaction.clone(),
            wait,
        },
        WriteCommand::Read { chat, .. } => Request::MarkRead {
            chat: chat.clone(),
            wait,
        },
        WriteCommand::NewChat { people, .. } => Request::NewChat {
            people: people.clone(),
            wait,
        },
        WriteCommand::Resend { row, .. } => Request::Resend { row: *row, wait },
    })
}

/// The request a call command asks.
fn call_request(call: &CallCommand) -> Request {
    match call {
        CallCommand::Calls => Request::Calls,
        CallCommand::Call { chat } => Request::Call { chat: chat.clone() },
        CallCommand::Answer => Request::Answer,
        CallCommand::Decline => Request::Decline,
        CallCommand::Hangup => Request::HangUp,
        CallCommand::Join {
            command: Some(JoinCommand::Meeting { event, now }),
            ..
        } => Request::JoinMeeting {
            event: event.clone(),
            now: *now,
        },
        CallCommand::Join { chat, .. } => Request::Join {
            chat: chat.clone().unwrap_or_default(),
        },
        CallCommand::Mute { op } => Request::Mute { op: toggle(*op) },
        CallCommand::Camera { op } => Request::Camera { op: toggle(*op) },
    }
}

fn toggle(op: ToggleArg) -> Toggle {
    match op {
        ToggleArg::On => Toggle::On,
        ToggleArg::Off => Toggle::Off,
        ToggleArg::Toggle => Toggle::Toggle,
    }
}

fn window_op(op: WindowArg) -> WindowOp {
    match op {
        WindowArg::Show => WindowOp::Show,
        WindowArg::Hide => WindowOp::Hide,
        WindowArg::Toggle => WindowOp::Toggle,
        WindowArg::Focus => WindowOp::Focus,
    }
}

fn status_choice(status: StatusArg) -> StatusChoice {
    match status {
        StatusArg::Available => StatusChoice::Available,
        StatusArg::Busy => StatusChoice::Busy,
        StatusArg::Dnd => StatusChoice::Dnd,
        StatusArg::Away => StatusChoice::Away,
        StatusArg::Reset => StatusChoice::Reset,
    }
}

fn open_target(target: &OpenArg) -> OpenTarget {
    match target {
        OpenArg::Chat { chat } => OpenTarget::Chat { chat: chat.clone() },
        OpenArg::Channel { channel } => OpenTarget::Channel {
            channel: channel.clone(),
        },
        OpenArg::Calendar => OpenTarget::Calendar,
        OpenArg::Activity => OpenTarget::Activity,
        OpenArg::Settings { page } => OpenTarget::Settings { page: page.clone() },
        OpenArg::Search { query } => OpenTarget::Search { query: query.join(" ") },
    }
}

/// Ask `request` and print its answer, waiting for it as long as [`answer_time`] says.
fn ask(cli: &Cli, mut client: Client, request: Request, out: &mut dyn Write, err: &mut dyn Write) -> Exit {
    let write = matches!(cli.command, Command::Write(_));
    match client
        .set_read_timeout(answer_time(&cli.command))
        .and_then(|()| client.request_line(cli.account.as_deref(), request))
    {
        Ok(line) => answered(cli, line, out, err),
        Err(error) => ask_failed(err, &error, write),
    }
}

/// How long `command`'s answer is waited for: a write's past the app's own bound on it ([`WRITE_ANSWER`], and
/// [`WRITE_WAIT`] more with `--wait`), a `sync`'s past [`SYNC_ANSWER`], a call command's (`join meeting` too) past
/// [`CALL_ANSWER`], a calendar read's past [`CALENDAR_ANSWER`], so the app's answer (its own timeout too) comes first;
/// any other's the client's default.
#[must_use]
pub fn answer_time(command: &Command) -> Duration {
    let slack = Timeouts::default().read;
    match command {
        Command::Write(write) if write.waits() => WRITE_ANSWER + WRITE_WAIT + slack,
        Command::Write(_) => WRITE_ANSWER + slack,
        Command::Sync => SYNC_ANSWER + slack,
        Command::Call(CallCommand::Calls) => slack,
        Command::Call(_) => CALL_ANSWER + slack,
        Command::Calendar { .. } => CALENDAR_ANSWER + slack,
        _ => slack,
    }
}

/// The exit a request that got no answer ends with, told on `err` as [`failed`] tells it; a `write` whose answer
/// didn't come in time is told it may still go out, which an exit of 1 alone wouldn't tell.
pub fn ask_failed(err: &mut dyn Write, error: &ClientError, write: bool) -> Exit {
    if write && matches!(error, ClientError::Timeout) {
        return told(
            err,
            Exit::Error,
            &format!("the app didn't answer the write in time: it may still go out. {UNANSWERED_WRITE}"),
        );
    }
    failed(err, error)
}

fn waits(command: &Command) -> bool {
    matches!(command, Command::Write(write) if write.waits())
}

/// Print an answer's line: its response on `out`, its error on `err` (both on `out` with `--json`). A write's row
/// that failed ends it with an error, as one that waited and wasn't sent.
fn answered(cli: &Cli, line: ResponseLine, out: &mut dyn Write, err: &mut dyn Write) -> Exit {
    let exit = |printed: Exit, response: &Response| {
        if printed == Exit::Ok && unsent(response, waits(&cli.command)) {
            Exit::Error
        } else {
            printed
        }
    };
    if cli.json {
        let printed = written(json_line(out, &line));
        return match &line.outcome {
            Outcome::Ok(response) => exit(printed, response),
            Outcome::Error(_) => Exit::Error,
        };
    }
    match line.outcome {
        Outcome::Ok(response) => exit(written(render::response(out, &response)), &response),
        Outcome::Error(error) => {
            let _ = render::error(err, &error);
            Exit::Error
        }
    }
}

/// Whether `response` is a write's row that failed, or with `waited` one not sent.
fn unsent(response: &Response, waited: bool) -> bool {
    match response {
        Response::Outbox(row) => row.state == OutboxState::Failed || (waited && row.state != OutboxState::Sent),
        _ => false,
    }
}

fn json_line(out: &mut dyn Write, line: &ResponseLine) -> io::Result<()> {
    serde_json::to_writer(&mut *out, line)?;
    writeln!(out)
}

/// When a stream's command ends.
enum Until<'a> {
    /// `events`: at the first event, or when the stream ends with `follow`.
    Events { follow: bool },
    /// `wait`: at the first event that matches, or past the deadline.
    Match {
        pattern: Option<&'a Regex>,
        deadline: Option<Instant>,
    },
}

/// Subscribe with `filter` and print events until `until` says the command is done.
fn stream(
    cli: &Cli,
    client: Client,
    filter: EventFilter,
    until: &Until<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Exit {
    let deadline = match until {
        Until::Match { deadline, .. } => *deadline,
        Until::Events { .. } => None,
    };
    let mut events = match subscribe(cli, client, filter, deadline) {
        Ok(Ok(events)) => events,
        Ok(Err(refused)) => return answered(cli, refused.into(), out, err),
        Err(error) => return setup_failed(err, &error, deadline),
    };
    loop {
        let event = match next(&mut events, until) {
            Ok(Some(event)) => event,
            Ok(None) => return ended(err, until),
            Err(Next::TimedOut) => return timed_out(err),
            Err(Next::Failed(error)) => return failed(err, &error),
        };
        if matches!(event.event, CtlEvent::Lagged) {
            let _ = print_event(cli, out, &event);
            return told(
                err,
                Exit::Error,
                "fell behind the app's events: the app ended the stream",
            );
        }
        if !wanted(&event, until) {
            continue;
        }
        if let Err(error) = print_event(cli, out, &event) {
            return written(Err(error));
        }
        if !matches!(until, Until::Events { follow: true }) {
            return Exit::Ok;
        }
    }
}

/// Subscribe with `filter`, the answer waited for no longer than what is left before `deadline`.
fn subscribe(
    cli: &Cli,
    client: Client,
    filter: EventFilter,
    deadline: Option<Instant>,
) -> Result<Result<Events, Refused>, ClientError> {
    if let Some(deadline) = deadline {
        let left = left(deadline).ok_or(ClientError::Timeout)?;
        client.set_read_timeout(left.min(Timeouts::default().read))?;
    }
    client.try_subscribe(cli.account.as_deref(), filter)
}

enum Next {
    TimedOut,
    Failed(ClientError),
}

/// The next event, waiting no longer than the deadline.
fn next(events: &mut Events, until: &Until<'_>) -> Result<Option<EventLine>, Next> {
    let deadline = match until {
        Until::Match {
            deadline: Some(deadline),
            ..
        } => Some(*deadline),
        _ => None,
    };
    loop {
        if let Some(deadline) = deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Next::TimedOut);
            }
            events.set_read_timeout(Some(left)).map_err(Next::Failed)?;
        }
        match events.next_event() {
            Err(ClientError::Timeout) if deadline.is_some() => {}
            other => return other.map_err(Next::Failed),
        }
    }
}

/// Whether `event` ends a `wait` (every event counts for `events`).
fn wanted(event: &EventLine, until: &Until<'_>) -> bool {
    match until {
        Until::Match {
            pattern: Some(pattern), ..
        } => serde_json::to_string(event).is_ok_and(|line| pattern.is_match(&line)),
        _ => true,
    }
}

fn print_event(cli: &Cli, out: &mut dyn Write, event: &EventLine) -> io::Result<()> {
    if cli.json {
        serde_json::to_writer(&mut *out, event)?;
        writeln!(out)?;
    } else {
        writeln!(out, "{}", render::event(event))?;
    }
    out.flush()
}

/// The app closed the stream: the end of `events --follow`, early for the others.
fn ended(err: &mut dyn Write, until: &Until<'_>) -> Exit {
    match until {
        Until::Events { follow: true } => Exit::Ok,
        _ => told(err, Exit::Error, "the app closed the stream"),
    }
}

/// The exit a client error ends with, told on `err`.
fn failed(err: &mut dyn Write, error: &ClientError) -> Exit {
    match error {
        ClientError::NotRunning { .. } => told(err, Exit::NoApp, &error.to_string()),
        ClientError::Server(error) => {
            let _ = render::error(err, error);
            Exit::Error
        }
        error => told(err, Exit::Error, &error.to_string()),
    }
}

fn told(err: &mut dyn Write, exit: Exit, message: &str) -> Exit {
    let _ = writeln!(err, "ngteamsctl: {message}");
    exit
}

/// The exit after printing: a reader that went away (`| head -1`) ends it quietly, any other write error is an
/// error.
fn written(result: io::Result<()>) -> Exit {
    match result {
        Ok(()) => Exit::Ok,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Exit::Ok,
        Err(_) => Exit::Error,
    }
}

/// A day `calendar --from` and `--to` take: `YYYY-MM-DD`.
fn date(value: &str) -> Result<String, String> {
    chrono::NaiveDate::parse_from_str(value, DATE_FORMAT)
        .map(|_| value.to_owned())
        .map_err(|_| format!("{value:?} isn't a date (YYYY-MM-DD)"))
}

/// `--timeout`'s seconds, fractions allowed.
fn seconds(value: &str) -> Result<Duration, String> {
    let seconds: f64 = value
        .parse()
        .map_err(|_| format!("{value:?} isn't a number of seconds"))?;
    Duration::try_from_secs_f64(seconds).map_err(|_| format!("{value:?} isn't a number of seconds"))
}
