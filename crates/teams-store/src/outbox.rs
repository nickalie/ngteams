//! Writes waiting to go out. One row per command, keyed by the `clientmessageid` it keeps across every attempt; the
//! core serializes the command into `payload` and the store doesn't read it.
//!
//! A row belongs to the run that made it ([`Store::run_id`]): only that run can begin an attempt on it. Rows a run
//! that has ended (its store dropped, or its process gone) left pending or in flight are listed by
//! [`Store::outbox_left_by_earlier_runs`] and go out again only through [`Store::resend`], an explicit command; nothing
//! is resent on start (the web client's late resend of a failed send, seen in M0, is what this rules out). A pending
//! or in-flight row of a run whose store is still open (a second process on the account) is never adopted: its
//! attempt may not have reached the service yet, so no sync could tell whether a second attempt sends it twice.
//!
//! States: `pending` → [`Store::begin_attempt`] → `sending` → [`Store::finish_attempt`] → `sent`, `denied`, `failed`
//! (all final) or back to `pending` for a retry after `not_before`. A check before a new attempt that couldn't be made
//! ([`Store::check_failed`]) leaves a pending row waiting until a later `not_before`, or fails it. Our message coming
//! back (Trouter or a sync) marks its row `sent` whatever state it is in ([`Store::outbox_arrived`]).
//!
//! A row's budget counts from when it was made or last resent: `attempts` counts every attempt it ever had (whether
//! it was attempted at all), [`OutboxEntry::attempts_since_resend`] those since, and `failed_checks` the checks since
//! that couldn't be made.
//!
//! A read that met an entry it couldn't read at all carrying a row's `clientmessageid` records it on the row
//! ([`Store::outbox_unreadable_seen`]): its message may have arrived ([`OutboxEntry::may_have_arrived`]), and once the
//! read moved past the entry nothing else tells. The record stays until the row is sent or explicitly resent.

use std::collections::HashMap;
use std::fmt;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, Row, ToSql, params};

use crate::{Store, StoreError, now_ms, runs};

/// What a row asks the service to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboxKind {
    Send,
    Edit,
    Delete,
    AddReaction,
    RemoveReaction,
    ReadReceipt,
    CreateChat,
    /// People added to a group chat.
    AddMembers,
    /// An item of the account's activity feed marked read or unread.
    ActivityRead,
    /// The whole activity feed marked read.
    FeedRead,
}

/// Where a row stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboxState {
    /// Waiting for an attempt (the first, or a retry after `not_before`).
    Pending,
    /// An attempt was begun and hasn't finished; left like this by an earlier run, whether it arrived is unknown.
    Sending,
    /// The service took it, or our message came back.
    Sent,
    /// Refused before sending: nothing was sent.
    Denied,
    /// The service refused it for good.
    Failed,
}

macro_rules! text_enum {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $name {
            /// The stored text.
            #[must_use]
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }

            fn from_text(text: &str) -> Option<Self> {
                match text { $($text => Some(Self::$variant),)+ _ => None }
            }
        }

        impl ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.as_str()))
            }
        }

        impl FromSql for $name {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                let text = value.as_str()?;
                Self::from_text(text).ok_or_else(|| FromSqlError::Other(Box::new(UnknownText(text.to_owned()))))
            }
        }
    };
}

text_enum!(OutboxKind {
    Send => "send",
    Edit => "edit",
    Delete => "delete",
    AddReaction => "add_reaction",
    RemoveReaction => "remove_reaction",
    ReadReceipt => "read_receipt",
    CreateChat => "create_chat",
    AddMembers => "add_members",
    ActivityRead => "activity_read",
    FeedRead => "feed_read",
});

text_enum!(OutboxState {
    Pending => "pending",
    Sending => "sending",
    Sent => "sent",
    Denied => "denied",
    Failed => "failed",
});

/// A stored kind or state this build doesn't know.
#[derive(Debug)]
struct UnknownText(String);

impl fmt::Display for UnknownText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown outbox value {:?}", self.0)
    }
}

impl std::error::Error for UnknownText {}

/// A command to put in the outbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewOutboxEntry {
    /// The conversation it writes to; none for a chat that doesn't exist yet.
    pub conversation_id: Option<String>,
    pub kind: OutboxKind,
    /// Fresh for each command, kept across its attempts.
    pub client_message_id: String,
    /// The command as the core serialized it.
    pub payload: String,
}

/// An outbox row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxEntry {
    pub id: i64,
    /// The run it belongs to: only that run sends it.
    pub run_id: i64,
    pub client_message_id: String,
    pub conversation_id: Option<String>,
    pub kind: OutboxKind,
    pub payload: String,
    pub state: OutboxState,
    /// How many attempts were begun, over the row's whole life (a resend included).
    pub attempts: u32,
    /// `attempts` when the row was last resent ([`Store::resend`]); 0 for a row never resent.
    pub attempts_at_resend: u32,
    /// Checks before a new attempt that couldn't be made ([`Store::check_failed`]) since the row was made or last
    /// resent.
    pub failed_checks: u32,
    /// No attempt before this time (milliseconds): a `Retry-After`.
    pub not_before: Option<i64>,
    /// Why the last attempt didn't send it.
    pub last_error: Option<String>,
    /// The service's id for the message, once known.
    pub message_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// When (milliseconds) a read first met an entry it couldn't read at all carrying the row's `clientmessageid`,
    /// since the row was made or last resent ([`Store::outbox_unreadable_seen`]); none when none did.
    pub unreadable_seen_at: Option<i64>,
}

impl OutboxEntry {
    /// The attempts begun since the row was made or last resent.
    #[must_use]
    pub fn attempts_since_resend(&self) -> u32 {
        self.attempts.saturating_sub(self.attempts_at_resend)
    }

    /// Whether the row's message may have arrived though it isn't known sent: a read met an entry it couldn't read
    /// carrying its `clientmessageid` ([`OutboxEntry::unreadable_seen_at`]). Such a row isn't sent again on its own,
    /// only by an explicit [`Store::resend`], which its owner offers after saying so.
    #[must_use]
    pub fn may_have_arrived(&self) -> bool {
        self.state != OutboxState::Sent && self.unreadable_seen_at.is_some()
    }
}

/// What becomes of a pending row whose check before a new attempt couldn't be made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckFailed {
    /// It stays pending, not due before `not_before`.
    Wait { not_before: i64, error: String },
    /// It is failed: nothing more goes out for it until an explicit resend.
    GiveUp { error: String },
}

/// How an attempt ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttemptOutcome {
    Sent {
        message_id: Option<String>,
    },
    /// Try again with the same `clientmessageid`, not before `not_before`.
    Retry {
        not_before: i64,
        error: String,
    },
    Denied {
        reason: String,
    },
    Failed {
        error: String,
    },
}

const OUTBOX_COLUMNS: &str = "id, run_id, client_message_id, conversation_id, kind, payload, state, attempts, \
                              not_before, last_error, message_id, created_at, updated_at, attempts_at_resend, \
                              failed_checks, unreadable_seen_at";

fn entry_from_row(row: &Row<'_>) -> rusqlite::Result<OutboxEntry> {
    Ok(OutboxEntry {
        id: row.get(0)?,
        run_id: row.get(1)?,
        client_message_id: row.get(2)?,
        conversation_id: row.get(3)?,
        kind: row.get(4)?,
        payload: row.get(5)?,
        state: row.get(6)?,
        attempts: row.get(7)?,
        not_before: row.get(8)?,
        last_error: row.get(9)?,
        message_id: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        attempts_at_resend: row.get(13)?,
        failed_checks: row.get(14)?,
        unreadable_seen_at: row.get(15)?,
    })
}

fn entry(conn: &Connection, id: i64) -> Result<OutboxEntry, StoreError> {
    let sql = format!("SELECT {OUTBOX_COLUMNS} FROM outbox WHERE id = ?1");
    conn.query_row(&sql, [id], entry_from_row)
        .optional()?
        .ok_or(StoreError::UnknownOutboxEntry(id))
}

fn entries(conn: &Connection, filter: &str, values: impl rusqlite::Params) -> Result<Vec<OutboxEntry>, StoreError> {
    let sql = format!("SELECT {OUTBOX_COLUMNS} FROM outbox WHERE {filter} ORDER BY id");
    let mut statement = conn.prepare_cached(&sql)?;
    let rows = statement.query_map(values, entry_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Removes every row of `conversation_id` (a conversation being forgotten: none of them can go out any more, nor be
/// resent); returns those that weren't sent, as they stood, oldest first.
pub(crate) fn forget_rows(conn: &Connection, conversation_id: &str) -> Result<Vec<OutboxEntry>, StoreError> {
    let unsent = entries(conn, "conversation_id = ?1 AND state <> 'sent'", [conversation_id])?;
    conn.execute("DELETE FROM outbox WHERE conversation_id = ?1", [conversation_id])?;
    Ok(unsent)
}

impl Store {
    /// Puts a command in the outbox as a pending row of this run.
    ///
    /// # Errors
    ///
    /// [`StoreError::DuplicateClientMessageId`] when a row has its `clientmessageid` already, [`StoreError::Sqlite`]
    /// when the write fails.
    pub fn enqueue(&mut self, new: &NewOutboxEntry) -> Result<OutboxEntry, StoreError> {
        let tx = self.conn.transaction()?;
        let taken: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM outbox WHERE client_message_id = ?1)",
            [&new.client_message_id],
            |row| row.get(0),
        )?;
        if taken {
            return Err(StoreError::DuplicateClientMessageId(new.client_message_id.clone()));
        }
        let now = now_ms();
        tx.execute(
            "INSERT INTO outbox (run_id, client_message_id, conversation_id, kind, payload, state, created_at,
                 updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                self.run_id,
                new.client_message_id,
                new.conversation_id,
                new.kind,
                new.payload,
                OutboxState::Pending,
                now
            ],
        )?;
        let stored = entry(&tx, tx.last_insert_rowid())?;
        tx.commit()?;
        Ok(stored)
    }

    /// One outbox row, if it exists.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn outbox_entry(&self, id: i64) -> Result<Option<OutboxEntry>, StoreError> {
        match entry(&self.conn, id) {
            Ok(found) => Ok(Some(found)),
            Err(StoreError::UnknownOutboxEntry(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The outbox row with a `clientmessageid`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn outbox_by_client_message_id(&self, client_message_id: &str) -> Result<Option<OutboxEntry>, StoreError> {
        Ok(entries(&self.conn, "client_message_id = ?1", [client_message_id])?
            .into_iter()
            .next())
    }

    /// This run's pending rows whose `not_before` has passed at `now` (milliseconds), oldest first.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn outbox_due(&self, now: i64) -> Result<Vec<OutboxEntry>, StoreError> {
        entries(
            &self.conn,
            "run_id = ?1 AND state = ?2 AND (not_before IS NULL OR not_before <= ?3)",
            params![self.run_id, OutboxState::Pending, now],
        )
    }

    /// A conversation's rows that aren't sent, of any run, oldest first: waiting, in flight, denied or failed (what
    /// its view shows of the outbox beside its messages). A sent row is the message once it comes back.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn outbox_unsettled(&self, conversation_id: &str) -> Result<Vec<OutboxEntry>, StoreError> {
        // `'sent'` written out, not bound: SQLite takes the partial index only for a term that implies its own
        entries(
            &self.conn,
            "conversation_id = ?1 AND state <> 'sent'",
            [conversation_id],
        )
    }

    /// Rows a run that has ended left pending or in flight: never sent on their own, only through [`Store::resend`].
    /// Rows of another run whose store is still open aren't listed.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails, [`StoreError::File`] when a run's lock can't be checked.
    pub fn outbox_left_by_earlier_runs(&self) -> Result<Vec<OutboxEntry>, StoreError> {
        let rows = entries(
            &self.conn,
            "run_id <> ?1 AND state IN (?2, ?3)",
            params![self.run_id, OutboxState::Pending, OutboxState::Sending],
        )?;
        let mut open_runs: HashMap<i64, bool> = HashMap::new();
        let mut left = Vec::with_capacity(rows.len());
        for row in rows {
            let run_open = if let Some(&known) = open_runs.get(&row.run_id) {
                known
            } else {
                let checked = self.run_is_open(row.run_id)?;
                open_runs.insert(row.run_id, checked);
                checked
            };
            if !run_open {
                left.push(row);
            }
        }
        Ok(left)
    }

    /// Replaces only the opaque payload of this run's pending edit. Core uses this before an attempt to persist a
    /// one-time attachment snapshot for a legacy edit; Core decides whether its payload still needs that upgrade.
    /// No identity, retry budget, timestamp, ordering or scheduling field changes, and no row is adopted or sent.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownOutboxEntry`] for no such row, [`StoreError::OutboxNotSendable`] for anything except this
    /// run's pending edit, [`StoreError::Sqlite`] when the write fails.
    pub fn upgrade_pending_edit(&mut self, id: i64, payload: &str) -> Result<OutboxEntry, StoreError> {
        let tx = self.conn.transaction()?;
        let row = entry(&tx, id)?;
        if row.run_id != self.run_id || row.state != OutboxState::Pending || row.kind != OutboxKind::Edit {
            return Err(StoreError::OutboxNotSendable { id, state: row.state });
        }
        tx.execute("UPDATE outbox SET payload = ?2 WHERE id = ?1", params![id, payload])?;
        let updated = entry(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    /// Begins an attempt on one of this run's pending rows: it becomes `sending` and its attempt count goes up.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownOutboxEntry`] for no such row, [`StoreError::OutboxNotSendable`] for a row that isn't
    /// pending or belongs to another run, [`StoreError::Sqlite`] when the write fails.
    pub fn begin_attempt(&mut self, id: i64) -> Result<OutboxEntry, StoreError> {
        let tx = self.conn.transaction()?;
        let row = entry(&tx, id)?;
        if row.state != OutboxState::Pending || row.run_id != self.run_id {
            return Err(StoreError::OutboxNotSendable { id, state: row.state });
        }
        tx.execute(
            "UPDATE outbox SET state = ?2, attempts = attempts + 1, updated_at = ?3 WHERE id = ?1",
            params![id, OutboxState::Sending, now_ms()],
        )?;
        let updated = entry(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    /// Finishes this run's attempt on a row. A row already `sent` (our message came back first) stays as it is,
    /// whatever the outcome.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownOutboxEntry`] for no such row, [`StoreError::OutboxNotSendable`] for a row with no attempt
    /// of this run in flight, [`StoreError::Sqlite`] when the write fails.
    pub fn finish_attempt(&mut self, id: i64, outcome: &AttemptOutcome) -> Result<OutboxEntry, StoreError> {
        let tx = self.conn.transaction()?;
        let row = entry(&tx, id)?;
        if row.state == OutboxState::Sent {
            return Ok(row);
        }
        if row.state != OutboxState::Sending || row.run_id != self.run_id {
            return Err(StoreError::OutboxNotSendable { id, state: row.state });
        }
        let (state, not_before, error, message_id) = match outcome {
            AttemptOutcome::Sent { message_id } => (OutboxState::Sent, None, None, message_id.as_deref()),
            AttemptOutcome::Retry { not_before, error } => (OutboxState::Pending, Some(*not_before), Some(error), None),
            AttemptOutcome::Denied { reason } => (OutboxState::Denied, None, Some(reason), None),
            AttemptOutcome::Failed { error } => (OutboxState::Failed, None, Some(error), None),
        };
        tx.execute(
            "UPDATE outbox SET state = ?2, not_before = ?3, last_error = COALESCE(?4, last_error),
                 message_id = COALESCE(?5, message_id), updated_at = ?6
             WHERE id = ?1",
            params![id, state, not_before, error, message_id, now_ms()],
        )?;
        let updated = entry(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    /// Records that the check before a new attempt on one of this run's pending rows couldn't tell whether the last
    /// one arrived (its conversation couldn't be synced, or a read met an entry it couldn't read carrying the row's
    /// `clientmessageid`): nothing was sent, its `failed_checks` goes up, and it waits until `not_before` or is
    /// failed. A row already `sent` (the check's sync matched our message before it failed) stays as it is.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownOutboxEntry`] for no such row, [`StoreError::OutboxNotSendable`] for a row that isn't
    /// pending or belongs to another run, [`StoreError::Sqlite`] when the write fails.
    pub fn check_failed(&mut self, id: i64, outcome: &CheckFailed) -> Result<OutboxEntry, StoreError> {
        let tx = self.conn.transaction()?;
        let row = entry(&tx, id)?;
        if row.state == OutboxState::Sent {
            return Ok(row);
        }
        if row.state != OutboxState::Pending || row.run_id != self.run_id {
            return Err(StoreError::OutboxNotSendable { id, state: row.state });
        }
        let (state, not_before, error) = match outcome {
            CheckFailed::Wait { not_before, error } => (OutboxState::Pending, Some(*not_before), error),
            CheckFailed::GiveUp { error } => (OutboxState::Failed, None, error),
        };
        tx.execute(
            "UPDATE outbox SET state = ?2, not_before = ?3, last_error = ?4, failed_checks = failed_checks + 1,
                 updated_at = ?5
             WHERE id = ?1",
            params![id, state, not_before, error, now_ms()],
        )?;
        let updated = entry(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    /// Records that a read met an entry it couldn't read at all carrying the row's `clientmessageid`, at `at`
    /// (milliseconds): its message may have arrived ([`OutboxEntry::may_have_arrived`]). The first sighting's time is
    /// kept. A row of any run is recorded, whatever its state; one already `sent` stays as it is.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownOutboxEntry`] for no such row, [`StoreError::Sqlite`] when the write fails.
    pub fn outbox_unreadable_seen(&mut self, id: i64, at: i64) -> Result<OutboxEntry, StoreError> {
        let tx = self.conn.transaction()?;
        let row = entry(&tx, id)?;
        if row.state == OutboxState::Sent || row.unreadable_seen_at.is_some() {
            return Ok(row);
        }
        tx.execute(
            "UPDATE outbox SET unreadable_seen_at = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, at, now_ms()],
        )?;
        let updated = entry(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    /// Adopts a row into this run as pending, with its `clientmessageid` unchanged: the explicit resend of a row a run
    /// that has ended left, or of one that was denied or failed. Its budget starts over (`attempts_at_resend` becomes
    /// its `attempts`, `failed_checks` 0), and an unreadable entry seen for it is forgotten (the resend is its owner's
    /// decision to send a row that may have arrived); `attempts` stays, so a row that was attempted is still known to be. For a row left `sending`, the caller first looks for
    /// its `clientmessageid` in a sync: the ended run's attempt may have arrived. A pending or sending row of a run
    /// whose store is open, this one or another, is refused: it is waiting for its attempt (and its `not_before`) or
    /// has one in flight that may not have reached the service yet, and a second attempt beside it could send the
    /// command twice. The check is made under the store's write lock, so two runs can't both adopt a row.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownOutboxEntry`] for no such row, [`StoreError::OutboxNotSendable`] for a row already sent or
    /// one of this run still pending or sending, [`StoreError::OutboxRunOpen`] for one of another open run still
    /// pending or sending, [`StoreError::File`] when its run's lock can't be checked, [`StoreError::Sqlite`] when the
    /// write fails.
    pub fn resend(&mut self, id: i64) -> Result<OutboxEntry, StoreError> {
        let tx = self.conn.transaction()?;
        let row = entry(&tx, id)?;
        let in_progress = matches!(row.state, OutboxState::Pending | OutboxState::Sending);
        if row.state == OutboxState::Sent || (in_progress && row.run_id == self.run_id) {
            return Err(StoreError::OutboxNotSendable { id, state: row.state });
        }
        // another run's row here: this run's own in-progress rows were refused above
        if in_progress && runs::is_open(&self.runs, row.run_id)? {
            return Err(StoreError::OutboxRunOpen {
                id,
                run_id: row.run_id,
                state: row.state,
            });
        }
        tx.execute(
            "UPDATE outbox SET run_id = ?2, state = ?3, not_before = NULL, attempts_at_resend = attempts,
                 failed_checks = 0, unreadable_seen_at = NULL, updated_at = ?4
             WHERE id = ?1",
            params![id, self.run_id, OutboxState::Pending, now_ms()],
        )?;
        let updated = entry(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    /// Our own message came back (Trouter or a sync) with `client_message_id`: its row, if any, is `sent` with the
    /// service's `message_id`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn outbox_arrived(
        &mut self,
        client_message_id: &str,
        message_id: &str,
    ) -> Result<Option<OutboxEntry>, StoreError> {
        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE outbox SET state = ?2, message_id = ?3, not_before = NULL, updated_at = ?4
             WHERE client_message_id = ?1",
            params![client_message_id, OutboxState::Sent, message_id, now_ms()],
        )?;
        let row = if changed == 0 {
            None
        } else {
            entries(&tx, "client_message_id = ?1", [client_message_id])?.pop()
        };
        tx.commit()?;
        Ok(row)
    }
}
