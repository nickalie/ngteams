//! The database: Postgres (the source of truth several nodes share) or SQLite (one node), through Diesel.
//!
//! One schema ([`schema`]) and one set of queries serve both backends: each query is written once inside [`with_conn!`],
//! which runs it on whichever connection the pool holds. Postgres goes through diesel-async's tokio-postgres
//! connection; SQLite through Diesel's own connection on the blocking pool, one connection per node (its writes
//! serialize anyway, and a single writer never meets `SQLITE_BUSY`). Times are Unix seconds.

pub mod keys;
pub mod meta;
pub mod refresh;
pub mod schema;
pub mod users;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use diesel::{ConnectionError, SqliteConnection};
use diesel_async::pooled_connection::bb8::{Pool as Bb8, RunError};
use diesel_async::pooled_connection::{AsyncDieselConnectionManager, ManagerConfig, PoolError};
use diesel_async::sync_connection_wrapper::SyncConnectionWrapper;
use diesel_async::{AsyncConnection, AsyncMigrationHarness, AsyncPgConnection, SimpleAsyncConnection};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use futures_util::FutureExt as _;
use futures_util::future::BoxFuture;

pub type Sqlite = SyncConnectionWrapper<SqliteConnection>;

const POSTGRES_MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations/postgres");
const SQLITE_MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations/sqlite");

/// The Postgres advisory lock key migrations hold ("ngts").
const MIGRATION_LOCK: i64 = 0x6e67_7473;
/// How long a request waits for a pooled connection.
const CHECKOUT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long SQLite waits on a lock another process holds (the admin CLI beside a running server).
const SQLITE_BUSY_TIMEOUT_MS: u32 = 5000;

/// Why a database call failed. Never carries a value from a row.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("can't connect to the database: {0}")]
    Connect(#[from] ConnectionError),
    #[error("no database connection: {0}")]
    Pool(String),
    #[error("database query failed: {0}")]
    Query(#[from] diesel::result::Error),
    #[error("database migration failed: {0}")]
    Migration(String),
    #[error("a unique value is already taken")]
    Conflict,
}

impl From<RunError> for DbError {
    fn from(error: RunError) -> Self {
        Self::Pool(error.to_string())
    }
}

impl From<PoolError> for DbError {
    fn from(error: PoolError) -> Self {
        Self::Pool(error.to_string())
    }
}

/// The pool, of one backend or the other.
#[derive(Clone)]
pub enum Pool {
    Postgres(Bb8<AsyncPgConnection>),
    Sqlite(Bb8<Sqlite>),
}

/// The server's database. Cheap to clone.
#[derive(Clone)]
pub struct Db {
    pool: Pool,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let backend = match self.pool {
            Pool::Postgres(_) => "postgres",
            Pool::Sqlite(_) => "sqlite",
        };
        f.debug_struct("Db").field("backend", &backend).finish()
    }
}

/// Runs `$body` with `$conn` bound to a `&mut` connection of the pool's backend: the body is compiled once per
/// backend, so it must be a query both accept.
#[macro_export]
#[doc(hidden)]
macro_rules! with_conn {
    ($db:expr, |$conn:ident| $body:expr) => {
        match $db.pool() {
            $crate::db::Pool::Postgres(pool) => {
                let mut pooled = pool.get().await?;
                let $conn: &mut ::diesel_async::AsyncPgConnection = &mut pooled;
                $body
            }
            $crate::db::Pool::Sqlite(pool) => {
                let mut pooled = pool.get().await?;
                let $conn: &mut $crate::db::Sqlite = &mut pooled;
                $body
            }
        }
    };
}
pub use with_conn;

impl Db {
    /// Connects to `url` (`postgres://…` or `sqlite://<path>`) with at most `max` connections (SQLite: one), and
    /// brings the schema up to date.
    ///
    /// # Errors
    /// The database can't be reached, or a migration fails.
    pub async fn connect(url: &str, max: u32) -> Result<Self, DbError> {
        let pool = if let Some(path) = url.strip_prefix("sqlite://") {
            let mut config = ManagerConfig::default();
            config.custom_setup = Box::new(sqlite_setup);
            let manager = AsyncDieselConnectionManager::<Sqlite>::new_with_config(path, config);
            Pool::Sqlite(
                Bb8::builder()
                    .max_size(1)
                    .connection_timeout(CHECKOUT_TIMEOUT)
                    .build(manager)
                    .await?,
            )
        } else {
            let manager = AsyncDieselConnectionManager::<AsyncPgConnection>::new(url);
            Pool::Postgres(
                Bb8::builder()
                    .max_size(max.max(1))
                    .connection_timeout(CHECKOUT_TIMEOUT)
                    .build(manager)
                    .await?,
            )
        };
        let db = Self { pool };
        db.migrate().await?;
        Ok(db)
    }

    #[must_use]
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Applies the migrations not yet applied. On Postgres a session advisory lock makes nodes starting at once take
    /// turns; the lock goes with the dedicated connection.
    ///
    /// # Errors
    /// A migration fails.
    pub async fn migrate(&self) -> Result<(), DbError> {
        match &self.pool {
            Pool::Postgres(pool) => {
                let mut connection = pool.dedicated_connection().await?;
                connection
                    .batch_execute(&format!("SELECT pg_advisory_lock({MIGRATION_LOCK})"))
                    .await?;
                run_migrations(connection, POSTGRES_MIGRATIONS).await
            }
            Pool::Sqlite(pool) => {
                let connection = pool.dedicated_connection().await?;
                run_migrations(connection, SQLITE_MIGRATIONS).await
            }
        }
    }

    /// Whether the database answers (readiness).
    ///
    /// # Errors
    /// It doesn't.
    pub async fn ping(&self) -> Result<(), DbError> {
        with_conn!(self, |conn| conn.batch_execute("SELECT 1").await?);
        Ok(())
    }
}

/// A SQLite connection with foreign keys on, WAL and a busy timeout.
fn sqlite_setup(path: &str) -> BoxFuture<'_, diesel::ConnectionResult<Sqlite>> {
    async move {
        let mut connection = Sqlite::establish(path).await?;
        connection
            .batch_execute(&format!(
                "PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA busy_timeout = {SQLITE_BUSY_TIMEOUT_MS};"
            ))
            .await
            .map_err(ConnectionError::CouldntSetupConfiguration)?;
        Ok(connection)
    }
    .boxed()
}

/// Runs the pending migrations on the blocking pool (the harness blocks on the connection's futures).
async fn run_migrations<C>(connection: C, migrations: EmbeddedMigrations) -> Result<(), DbError>
where
    C: AsyncConnection + 'static,
    AsyncMigrationHarness<C>: MigrationHarness<C::Backend>,
{
    tokio::task::spawn_blocking(move || {
        let mut harness = AsyncMigrationHarness::new(connection);
        harness
            .run_pending_migrations(migrations)
            .map(|_| ())
            .map_err(|error| DbError::Migration(error.to_string()))
    })
    .await
    .map_err(|error| DbError::Migration(error.to_string()))?
}

/// Now, in Unix seconds.
#[must_use]
pub fn now() -> i64 {
    unix(SystemTime::now())
}

/// A time in Unix seconds (0 before the epoch, saturating far ahead).
#[must_use]
pub fn unix(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX))
}

/// A duration in whole seconds, as a column takes it.
#[must_use]
pub fn seconds(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}

/// A unique-key violation as [`DbError::Conflict`], any other error as it is.
pub(crate) fn conflict(error: diesel::result::Error) -> DbError {
    match error {
        diesel::result::Error::DatabaseError(diesel::result::DatabaseErrorKind::UniqueViolation, _) => {
            DbError::Conflict
        }
        error => DbError::Query(error),
    }
}
