//! The native client's local store, one SQLite file per account (`<data_dir>/store/<alias>.sqlite`, mode `0600`, in
//! WAL mode), outside the repo.
//!
//! - [`conversations`]: conversations by thread version, their members by roster version, the list by last activity,
//!   the threads forgotten as deleted (schema 19);
//! - [`messages`]: messages by id and version (an edit or a soft delete is a newer version), reactions per user, pages
//!   of a conversation, the unread count;
//! - [`state`]: read horizons, the account's mark-all-read one, and where each conversation's sync stands;
//! - [`profiles`]: people's names and pictures as the account last read them (M2 Task 12);
//! - [`outbox`]: writes waiting to go out, keyed by `clientmessageid`; a run sends only its own rows, and adopts only
//!   rows of runs that have ended (each open run holds a lock of its own, `runs`); one engine per account holds the
//!   engine lock ([`Store::engine_lock`]);
//! - [`search`]: local search over the messages' plain text (FTS5), the text the core hands in (M2 Task 15);
//! - [`pictures`]: the pictures messages show, cached in a file of their own beside the store
//!   (`<store>-cache/pictures.sqlite`), so a later run shows them without reading them again;
//! - [`teams`]: the account's teams and their channels as CSA last listed them, and each stored channel message's
//!   post (M2 Task 16);
//! - [`calendar`]: the account's calendar events as the calendar view last read them, each window's read replacing
//!   the events it overlaps (schema 16);
//! - [`translations`]: chats translated on this computer, each message's translation by its id, version and language,
//!   the account's own messages as written when they were sent translated (schema 20), and each translated chat's
//!   glossary of chosen renderings (schema 22);
//! - [`call_transcripts`]: calls' live captions saved on this computer, a transcript per call by its chat (schema 21);
//! - [`schema`]: the migrations, applied in order on open.
//!
//! Every versioned write is an upsert that never lets an older version overwrite a newer one, so a Trouter delivery
//! and a sync that bring the same change in either order leave the same rows ([`Applied`] says what happened). The
//! store keeps rows; the chat service's JSON is parsed by `teams-proto` and the decisions are `teams-core`'s.
//!
//! M1 Task 11 (`docs/plans/20261020-native-teams-m1-headless-chat-core.md`).

pub mod calendar;
pub mod call_transcripts;
pub mod conversations;
mod file;
pub mod messages;
pub mod outbox;
pub mod pictures;
pub mod profiles;
mod runs;
pub mod schema;
pub mod search;
pub mod snapshot;
pub mod state;
pub mod teams;
pub mod translations;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior};

pub use calendar::StoredEvent;
pub use call_transcripts::{CallTranscript, TranscriptLine};
pub use conversations::{Conversation, ConversationKind, Forgotten, ForgottenThread, Member};
pub use messages::{Message, MessagePage, PageCursor, Reaction, ReactionChange, counts_unread};
pub use outbox::{AttemptOutcome, CheckFailed, NewOutboxEntry, OutboxEntry, OutboxKind, OutboxState};
pub use pictures::PictureCache;
pub use profiles::{NameSource, PictureState, Profile, StoredPicture};
pub use schema::SCHEMA_VERSION;
pub use search::{IndexCursor, IndexText, SearchHit};
pub use state::{AppReadHorizon, ReadHorizon, SyncState};
pub use teams::{ChannelAbout, ChannelKind, Team, TeamAbout, TeamChannel};
pub use translations::{GLOSSARY_TERMS, GlossaryTerm, SentOriginal, StoredTranslation, TranslationKind};

/// How long a write waits for another connection's lock (a second process on the same account) before it fails.
/// Every transaction takes the write lock when it begins (`BEGIN IMMEDIATE`), so one that reads before it writes
/// waits here too, rather than failing at once when another connection wrote in between.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// What a store operation can fail with.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("store file {}: {source}", path.display())]
    File { path: PathBuf, source: std::io::Error },
    #[error("the store's journal mode is {0}, not wal")]
    NotWal(String),
    #[error("the store's schema version {found} is newer than this build's {supported}")]
    SchemaTooNew { found: u32, supported: u32 },
    #[error("no conversation {0} in the store")]
    UnknownConversation(String),
    #[error("no message {message} in conversation {conversation}")]
    UnknownMessage { conversation: String, message: String },
    #[error("an outbox row with clientmessageid {0} exists already")]
    DuplicateClientMessageId(String),
    #[error("no outbox row {0}")]
    UnknownOutboxEntry(i64),
    #[error("outbox row {id} is {} and not this run's to send", state.as_str())]
    OutboxNotSendable { id: i64, state: OutboxState },
    #[error("outbox row {id} is {} in run {run_id}, whose store is still open", state.as_str())]
    OutboxRunOpen { id: i64, run_id: i64, state: OutboxState },
    /// Another engine runs on this account (in this process or another): its lock is held.
    #[error("another engine runs on this account: its lock {} is held", path.display())]
    EngineRunning { path: PathBuf },
    /// A store's copy ([`snapshot::snapshot`]) failed SQLite's check.
    #[error("the store's copy at {} failed SQLite's check", path.display())]
    Snapshot { path: PathBuf },
}

/// The account's engine lock, held until it is dropped ([`Store::engine_lock`]): one engine per account, so two never
/// hold two push channels, nor send read receipts, typing and presence twice.
#[derive(Debug)]
pub struct EngineLock {
    _file: File,
    path: PathBuf,
}

impl EngineLock {
    /// Takes the engine lock of the store at `store` as [`Store::engine_lock`] does, without opening the store (it
    /// need not exist): for a command that must not run beside the account's engine, holding it for as long as it
    /// runs.
    ///
    /// # Errors
    ///
    /// [`StoreError::EngineRunning`] when another holds it, [`StoreError::File`] when it can't be made or locked.
    pub fn hold(store: &Path) -> Result<Self, StoreError> {
        Self::hold_in(&runs::dir(store))
    }

    fn hold_in(runs: &Path) -> Result<Self, StoreError> {
        match runs::try_hold_engine(runs)? {
            Some((file, path)) => Ok(Self { _file: file, path }),
            None => Err(StoreError::EngineRunning {
                path: runs::engine_lock_path(runs),
            }),
        }
    }

    /// The lock file: `<store>-runs/engine.lock`.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// What a versioned write did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    /// There was no row: it was written.
    Inserted,
    /// The stored row was older: it was replaced.
    Updated,
    /// The stored row is as new or newer: nothing changed.
    Ignored,
}

/// One account's store. Writes take `&mut self`; the core owns it on one thread.
///
/// Several stores may be open on one file (two processes on the same account): each is a run of its own, and holds
/// its run's lock until it is dropped. The core keeps its store open until every attempt it began has finished: once
/// the store is dropped, another run may adopt the rows.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    /// The file it was opened at.
    path: PathBuf,
    run_id: i64,
    /// Where the runs' locks are.
    runs: PathBuf,
    /// This run's lock, held while the store is open.
    _run_lock: File,
}

impl Store {
    /// Opens the store at `path`, creating it (and its directory, mode `0700`) when missing, with the file `0600`
    /// (a looser existing file is tightened); turns on WAL and foreign keys, applies the pending migrations, and starts
    /// a new run (see [`Store::run_id`]) holding its lock in `<path>-runs/` (lock files of ended runs are removed).
    /// Any number of stores may open the same file at once, in one process or several, a new file too: they wait for
    /// each other (the open lock in `<path>-runs/` until the file is in WAL mode, SQLite's write lock after).
    ///
    /// # Errors
    ///
    /// [`StoreError::File`] when the file or its directory can't be made private or the open lock or the run's lock
    /// can't be taken, [`StoreError::NotWal`] when SQLite keeps another journal mode, [`StoreError::SchemaTooNew`] for
    /// a store written by a newer build, [`StoreError::Sqlite`] when SQLite refuses it.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let runs_dir = runs::dir(path);
        let mut conn = {
            // concurrent opens wait here until the file is in WAL mode, which SQLite can't wait for (see `runs`)
            let _open_lock = runs::open_lock(&runs_dir)?;
            file::prepare(path)?;
            Self::connect(path)?
        };
        schema::migrate(&mut conn)?;
        let (run_id, run_lock) = Self::start_run(&mut conn, &runs_dir)?;
        Ok(Self {
            conn,
            path: path.to_path_buf(),
            run_id,
            runs: runs_dir,
            _run_lock: run_lock,
        })
    }

    /// Opens a connection to the prepared file and puts the file in WAL mode. Called under the open lock.
    fn connect(path: &Path) -> Result<Connection, StoreError> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let mut conn = Connection::open_with_flags(path, flags)?;
        // before the first statement that touches the file
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.set_transaction_behavior(TransactionBehavior::Immediate);
        // SQLite answers with the mode it took: the old one when it can't switch
        let mode = conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StoreError::NotWal(mode));
        }
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // `rarray`: a search in some conversations binds them as one value (`Store::search_in`)
        rusqlite::vtab::array::load_module(&conn)?;
        Ok(conn)
    }

    /// Adds a run and takes its lock in one write transaction, removing the locks of ended runs first (see `runs`).
    fn start_run(conn: &mut Connection, runs_dir: &Path) -> Result<(i64, File), StoreError> {
        let tx = conn.transaction()?;
        runs::sweep(runs_dir)?;
        tx.execute("INSERT INTO runs (started_at) VALUES (?1)", [now_ms()])?;
        let run_id = tx.last_insert_rowid();
        // declared after `tx`, so a failed commit drops the lock before the rollback
        let lock = runs::hold(runs_dir, run_id)?;
        tx.commit()?;
        Ok((run_id, lock))
    }

    /// Whether another run's store is still open (this run's always is).
    fn run_is_open(&self, run_id: i64) -> Result<bool, StoreError> {
        if run_id == self.run_id {
            return Ok(true);
        }
        runs::is_open(&self.runs, run_id)
    }

    /// The file the store was opened at.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// This process's run: the outbox rows it made or adopted. Rows of other runs are never sent on their own.
    #[must_use]
    pub fn run_id(&self) -> i64 {
        self.run_id
    }

    /// Takes the account's engine lock (`flock` on `<store>-runs/engine.lock`) without waiting, for an engine's owner
    /// to hold while the engine runs. A second engine on the account, in this process or another, is refused rather
    /// than kept waiting, and the lock is never broken: the kernel drops it when its holder closes it or dies. The
    /// store's own runs ([`Store::run_id`]) are unaffected: any number of stores may still be open on the file.
    ///
    /// # Errors
    ///
    /// [`StoreError::EngineRunning`] when another holds it, [`StoreError::File`] when it can't be made or locked.
    pub fn engine_lock(&self) -> Result<EngineLock, StoreError> {
        EngineLock::hold_in(&self.runs)
    }

    /// The schema version the store is at.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the version can't be read.
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        schema::version(&self.conn)
    }

    fn require_conversation(conn: &Connection, id: &str) -> Result<(), StoreError> {
        let known: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM conversations WHERE id = ?1)",
            [id],
            |row| row.get(0),
        )?;
        if known {
            Ok(())
        } else {
            Err(StoreError::UnknownConversation(id.to_owned()))
        }
    }
}

/// Milliseconds since the Unix epoch, for the store's own bookkeeping (run starts, outbox row times).
fn now_ms() -> i64 {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(since_epoch.as_millis()).unwrap_or(i64::MAX)
}

/// A `LIMIT` from a count: anything past `i64::MAX` is no limit at all.
fn sql_limit(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}
