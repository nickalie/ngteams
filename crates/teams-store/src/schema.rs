//! Migrations, applied in order on open. The schema version is SQLite's `user_version`; migration `n` (1-based) takes
//! the store from version `n - 1` to `n` in one transaction, so a failed one leaves the store where it was. Each
//! transaction reads the version under the write lock, so two processes opening a new store at once apply each
//! migration once. A
//! migration once released is never edited: a change is a new file appended to [`MIGRATIONS`].

use rusqlite::{Connection, TransactionBehavior};

use crate::StoreError;

/// The schema version this build writes: the number of migrations.
pub const SCHEMA_VERSION: u32 = 23;

/// The migrations, in order.
const MIGRATIONS: [&str; SCHEMA_VERSION as usize] = [
    include_str!("migrations/0001_initial.sql"),
    include_str!("migrations/0002_outbox_budget.sql"),
    include_str!("migrations/0003_sync_chains.sql"),
    include_str!("migrations/0004_outbox_unreadable_seen.sql"),
    include_str!("migrations/0005_outbox_by_conversation.sql"),
    include_str!("migrations/0006_messages_by_sender.sql"),
    include_str!("migrations/0007_profiles.sql"),
    include_str!("migrations/0008_search.sql"),
    include_str!("migrations/0009_teams.sql"),
    include_str!("migrations/0010_topics.sql"),
    include_str!("migrations/0011_favorites_and_empty.sql"),
    include_str!("migrations/0012_sync_bounds.sql"),
    include_str!("migrations/0013_channel_creation.sql"),
    include_str!("migrations/0014_app_read_horizon.sql"),
    include_str!("migrations/0015_team_management.sql"),
    include_str!("migrations/0016_calendar_events.sql"),
    include_str!("migrations/0017_channel_management.sql"),
    include_str!("migrations/0018_team_site.sql"),
    include_str!("migrations/0019_forgotten_conversations.sql"),
    include_str!("migrations/0020_chat_translations.sql"),
    include_str!("migrations/0021_call_transcripts.sql"),
    include_str!("migrations/0022_translation_glossary.sql"),
    include_str!("migrations/0023_conversation_previews.sql"),
];

pub(crate) fn version(conn: &Connection) -> Result<u32, StoreError> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

/// Applies every migration past the store's version; refuses a store from a newer build.
pub(crate) fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    apply(conn, &MIGRATIONS, "store")
}

/// Applies every migration of `migrations` past the file's version (`user_version`), each in a transaction of its own
/// that reads the version under the write lock; refuses a file from a newer build. `what` names the file in the log.
pub(crate) fn apply(conn: &mut Connection, migrations: &[&str], what: &str) -> Result<(), StoreError> {
    let supported = u32::try_from(migrations.len()).unwrap_or(u32::MAX);
    loop {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let found = version(&tx)?;
        if found > supported {
            return Err(StoreError::SchemaTooNew { found, supported });
        }
        let Some(sql) = migrations.get(found as usize) else {
            return Ok(());
        };
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", found + 1)?;
        tx.commit()?;
        log::info!("{what} migrated from schema version {found} to {}", found + 1);
    }
}
