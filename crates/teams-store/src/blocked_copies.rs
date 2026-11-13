//! The service's copies of the account's own messages a tenant's policy blocked (schema 25): a blocked message comes
//! back without its text and files and names its copy, which may come before it, after it, or across a restart. The
//! copy is kept here; its text is put back into the message where the message is stored.

use rusqlite::{OptionalExtension as _, params};

use crate::messages::{MESSAGE_COLUMNS, message_from_row};
use crate::{Message, Store, StoreError};

/// A copy as the service sent it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockedCopy {
    /// The copy's own message id: what the blocked message names.
    pub id: String,
    /// The blocked message's content (HTML).
    pub content: String,
    /// The copy's `properties` object as JSON text (its `files`).
    pub properties: Option<String>,
    /// When it arrived, in milliseconds.
    pub arrival_time: i64,
}

impl Store {
    /// Keeps a copy (a copy sent again replaces it).
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn keep_blocked_copy(&mut self, copy: &BlockedCopy) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO blocked_copies (id, content, properties, arrival_time) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (id) DO UPDATE SET content = excluded.content, properties = excluded.properties,
                 arrival_time = excluded.arrival_time",
            params![copy.id, copy.content, copy.properties, copy.arrival_time],
        )?;
        Ok(())
    }

    /// The copy kept under `id`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn blocked_copy(&self, id: &str) -> Result<Option<BlockedCopy>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, content, properties, arrival_time FROM blocked_copies WHERE id = ?1",
                [id],
                |row| {
                    Ok(BlockedCopy {
                        id: row.get(0)?,
                        content: row.get(1)?,
                        properties: row.get(2)?,
                        arrival_time: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// The stored blocked messages from `sender` that name the copy `id` (their
    /// `properties.policyViolation.genericStreamItemId`, as a number or as text): those it puts the text back into.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn messages_naming_copy(&self, sender: &str, id: &str) -> Result<Vec<Message>, StoreError> {
        let sql = format!(
            "WITH candidates AS (
                 SELECT {MESSAGE_COLUMNS},
                     CASE json_type(properties, '$.policyViolation')
                         WHEN 'text' THEN CASE WHEN json_valid(json_extract(properties, '$.policyViolation'))
                                               THEN json_extract(properties, '$.policyViolation') END
                         ELSE json_extract(properties, '$.policyViolation')
                     END AS verdict
                 FROM messages WHERE sender = ?1 AND delete_time IS NULL AND json_valid(properties)
             )
             SELECT {MESSAGE_COLUMNS} FROM candidates
             WHERE json_extract(verdict, '$.state') = 1
               AND CAST(json_extract(verdict, '$.genericStreamItemId') AS TEXT) = ?2"
        );
        let mut statement = self.conn.prepare_cached(&sql)?;
        let rows = statement.query_map(params![sender, id], message_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Puts a blocked message's text and properties back at the version stored and indexes its text in the same
    /// transaction; whether a row changed (none when a newer version or a delete came meanwhile).
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn restore_blocked(
        &mut self,
        restored: &Message,
        text: impl FnOnce(&Message) -> String,
    ) -> Result<bool, StoreError> {
        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE messages SET content = ?4, properties = ?5
             WHERE conversation_id = ?1 AND id = ?2 AND version = ?3 AND delete_time IS NULL",
            params![
                restored.conversation_id,
                restored.id,
                restored.version,
                restored.content,
                restored.properties
            ],
        )?;
        if changed > 0 {
            crate::search::index_text(
                &tx,
                &crate::IndexText {
                    conversation_id: restored.conversation_id.clone(),
                    message_id: restored.id.clone(),
                    version: restored.version,
                    text: text(restored),
                },
            )?;
        }
        tx.commit()?;
        Ok(changed > 0)
    }
}
