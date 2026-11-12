//! The account's writes as a chat shows them while they go out (M2 Task 13): each outbox row's state, from the row
//! the engine told or the store holds, and whether "send again" is offered.
//!
//! - **States** ([`RowState`]): sending (waiting for its attempt, or one under way), waiting (a `Retry-After`), sent,
//!   refused before sending, failed, may have arrived (a read met an entry it couldn't read carrying the row's
//!   `clientmessageid`: [`OutboxEntry::may_have_arrived`]), and left by an earlier session (pending or in flight when
//!   that run ended).
//! - **Send again** ([`Again`]) is offered on a failed row and on an earlier session's row, and only ever goes out by
//!   the engine's explicit `resend`; for a row that may have arrived the view warns first, and for an edit, a delete
//!   or a reaction too (a later change to the message may have gone out since, in a session whose rows the view
//!   doesn't know). A row recorded as denied is not offered resend.
//! - **Out of date** ([`superseded`]): a write on a message that a newer one on the same thing follows (an edit or a
//!   delete after an edit, anything before a delete, the same reaction set or taken back again) isn't shown, so it is
//!   never sent again over the newer one.
//! - **Where**: a message post shows as a pending message of its own until its message is in the store; an edit, a
//!   delete or a reaction shows on the message it acts on. Read receipts and a new chat's creation aren't shown.
//! - **Waiting on one** ([`settled_row`]): a view that needs a write's outcome (a new chat made, people added) watches
//!   its row until the outbox settles it, never writing it again; a watch no view owns ends once the engine stopped.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::time::Duration;

use gpui::BackgroundExecutor;
use teams_core::{Command, EngineError, Reply};
use teams_store::{OutboxEntry, OutboxKind, OutboxState};

use crate::bridge::Core;

/// How often a write's row is looked at while it is still going out.
const SETTLE_EVERY: Duration = Duration::from_millis(500);

/// A write's outbox row once the outbox settled it (sent, failed, denied...), none when the write was refused. A
/// throttled or uncertain write already owns its row: it is watched, never written again.
pub(crate) async fn settled_row(
    core: &dyn Core,
    reply: Reply<OutboxEntry>,
    executor: &BackgroundExecutor,
) -> Option<OutboxEntry> {
    settle(core, reply, executor, false).await
}

/// [`settled_row`] for a watch no view owns (a profile card's quick message, which outlives its card): it ends, with
/// none, once the engine stopped, rather than watching a row nobody settles any more.
pub(crate) async fn settled_row_unowned(
    core: &dyn Core,
    reply: Reply<OutboxEntry>,
    executor: &BackgroundExecutor,
) -> Option<OutboxEntry> {
    settle(core, reply, executor, true).await
}

async fn settle(
    core: &dyn Core,
    reply: Reply<OutboxEntry>,
    executor: &BackgroundExecutor,
    until_stopped: bool,
) -> Option<OutboxEntry> {
    let mut row = reply.await.ok()?;
    while matches!(row.state, OutboxState::Pending | OutboxState::Sending) {
        executor.timer(SETTLE_EVERY).await;
        match core.outbox_entry(row.id).await {
            Ok(Some(updated)) => row = updated,
            Err(EngineError::Stopped) if until_stopped => return None,
            Ok(None) | Err(_) => {}
        }
    }
    Some(row)
}

/// Where an outbox row stands, as the view shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    Sending,
    /// Waiting out a `Retry-After` (or a check's backoff) before its next attempt.
    Waiting,
    Sent,
    /// Refused before sending: nothing was sent.
    Denied,
    Failed,
    /// It isn't known sent, but a read met its message in a form it couldn't read.
    MayHaveArrived,
    /// An earlier session left it pending or in flight: it goes out only if sent again.
    EarlierSession,
}

impl RowState {
    /// `row`'s state; `earlier`: an earlier session left it; `now_ms`: the time now.
    #[must_use]
    pub fn of(row: &OutboxEntry, earlier: bool, now_ms: i64) -> Self {
        if row.may_have_arrived() {
            return Self::MayHaveArrived;
        }
        match row.state {
            OutboxState::Sent => Self::Sent,
            OutboxState::Denied => Self::Denied,
            OutboxState::Failed => Self::Failed,
            OutboxState::Pending | OutboxState::Sending if earlier => Self::EarlierSession,
            OutboxState::Pending if row.not_before.is_some_and(|at| at > now_ms) => Self::Waiting,
            OutboxState::Pending | OutboxState::Sending => Self::Sending,
        }
    }

    /// What the view says.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Sending => "Sending…",
            Self::Waiting => "Waiting to send",
            Self::Sent => "Sent",
            Self::Denied => "Refused: not sent",
            Self::Failed => "Failed: not sent",
            Self::MayHaveArrived => "May have arrived",
            Self::EarlierSession => "Not sent (left by an earlier session)",
        }
    }

    /// The trace's name for it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Sending => "sending",
            Self::Waiting => "waiting",
            Self::Sent => "sent",
            Self::Denied => "denied",
            Self::Failed => "failed",
            Self::MayHaveArrived => "may-have-arrived",
            Self::EarlierSession => "earlier-session",
        }
    }
}

/// Whether "send again" is offered on a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Again {
    No,
    Yes,
    /// After a warning that its message may have arrived already.
    AfterWarning,
}

impl Again {
    /// For `row` (`earlier`: an earlier session left it): offered on a failed row and an earlier session's, with a
    /// warning first when its message may have arrived, or when it changes a message (not a post): sent again, it
    /// would undo a later change to that message.
    #[must_use]
    pub fn of(row: &OutboxEntry, earlier: bool) -> Self {
        let left = earlier && matches!(row.state, OutboxState::Pending | OutboxState::Sending);
        let warn = row.may_have_arrived() || row.kind != OutboxKind::Send;
        match (row.state == OutboxState::Failed || left, warn) {
            (false, _) => Self::No,
            (true, true) => Self::AfterWarning,
            (true, false) => Self::Yes,
        }
    }
}

/// An outbox row as a chat shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub row: i64,
    pub kind: OutboxKind,
    pub state: RowState,
    pub again: Again,
}

impl Note {
    /// `row` as the view shows it.
    #[must_use]
    pub fn of(row: &OutboxEntry, earlier: bool, now_ms: i64) -> Self {
        Self {
            row: row.id,
            kind: row.kind,
            state: RowState::of(row, earlier, now_ms),
            again: Again::of(row, earlier),
        }
    }

    /// What it says, with what the row does when it isn't a message post (shown on the message it acts on).
    #[must_use]
    pub fn text(&self) -> String {
        let what = match self.kind {
            OutboxKind::Edit => "Edit",
            OutboxKind::Delete => "Delete",
            OutboxKind::AddReaction => "Reaction",
            OutboxKind::RemoveReaction => "Taking a reaction back",
            OutboxKind::Send
            | OutboxKind::ReadReceipt
            | OutboxKind::CreateChat
            | OutboxKind::AddMembers
            | OutboxKind::ActivityRead
            | OutboxKind::FeedRead => {
                return self.state.label().to_owned();
            }
        };
        format!("{what}: {}", self.state.label())
    }
}

/// The command a row carries, read back from its payload; `None` when it can't be read.
#[must_use]
pub fn command_of(row: &OutboxEntry) -> Option<Command> {
    serde_json::from_str(&row.payload).ok()
}

/// What a write on a message changes: its content (an edit, a delete) or one of the account's reactions on it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Changes {
    Content,
    Reaction(String),
}

/// The message a command acts on and what it changes there; `None` for a post, a read receipt and a new chat.
fn changes(command: &Command) -> Option<(&str, Changes)> {
    match command {
        Command::Edit { message_id, .. } | Command::Delete { message_id, .. } => Some((message_id, Changes::Content)),
        Command::React { message_id, key, .. } => Some((message_id, Changes::Reaction(key.clone()))),
        // a post, a read receipt, a new chat
        _ => None,
    }
}

/// The ids of the rows of `rows` that a newer row (a larger id) on the same message makes out of date, in any state:
/// an edit or a delete after an edit or a delete, a delete after a reaction, the same reaction key set or taken back
/// after it was set or taken back. Sent again, one would undo the newer write.
#[must_use]
pub fn superseded<'a>(rows: impl IntoIterator<Item = &'a OutboxEntry>) -> HashSet<i64> {
    let mut commands: Vec<(i64, Command)> = rows
        .into_iter()
        .filter_map(|row| Some((row.id, command_of(row)?)))
        .collect();
    commands.sort_by_key(|(id, _)| Reverse(*id));
    let mut later: HashSet<(String, Changes)> = HashSet::new();
    let mut deleted: HashSet<String> = HashSet::new();
    let mut out = HashSet::new();
    for (id, command) in &commands {
        let Some((message, change)) = changes(command) else {
            continue;
        };
        let key = (message.to_owned(), change);
        if deleted.contains(message) || later.contains(&key) {
            out.insert(*id);
        }
        if matches!(command, Command::Delete { .. }) {
            deleted.insert(message.to_owned());
        }
        later.insert(key);
    }
    out
}

/// Whether a row of `kind` shows on the message it acts on.
#[must_use]
pub fn shown_on_message(kind: OutboxKind) -> bool {
    matches!(
        kind,
        OutboxKind::Edit | OutboxKind::Delete | OutboxKind::AddReaction | OutboxKind::RemoveReaction
    )
}
