//! List-page hints stay outside history so they cannot advance sync, match outbox rows or produce message events.

use rusqlite::{OptionalExtension as _, params};

use crate::messages::{MESSAGE_COLUMNS, message_from_row};
use crate::{Message, Store, StoreError};

impl Store {
    /// Keeps a list-page last message for a chat's row until its history arrives. Older hints never replace newer
    /// ones; deleted hints retain no content.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails, including for an unknown conversation.
    pub fn upsert_conversation_preview(&mut self, message: &Message) -> Result<bool, StoreError> {
        let deleted = message.delete_time.is_some();
        let changed = self.conn.execute(
            &format!(
                "INSERT INTO conversation_previews ({MESSAGE_COLUMNS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT (conversation_id) DO UPDATE SET
                     id = excluded.id, version = excluded.version, client_message_id = excluded.client_message_id,
                     sender = excluded.sender, sender_name = excluded.sender_name, message_type = excluded.message_type,
                     content = excluded.content, properties = excluded.properties, arrival_time = excluded.arrival_time,
                     compose_time = excluded.compose_time, edit_time = excluded.edit_time, delete_time = excluded.delete_time
                 WHERE (excluded.arrival_time, excluded.id) > (conversation_previews.arrival_time, conversation_previews.id)
                    OR (excluded.id = conversation_previews.id AND excluded.version > conversation_previews.version)"
            ),
            params![
                message.conversation_id,
                message.id,
                message.version,
                message.client_message_id,
                message.sender,
                message.sender_name,
                message.message_type,
                if deleted { "" } else { message.content.as_str() },
                if deleted { None } else { message.properties.as_deref() },
                message.arrival_time,
                message.compose_time,
                message.edit_time,
                message.delete_time,
            ],
        )?;
        Ok(changed > 0)
    }

    /// A list-page preview while the chat has neither stored messages nor a completed first sync. History reads
    /// never include it; once history arrives, its messages replace this hint, even if that history is empty.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn conversation_preview(&self, conversation: &str) -> Result<Option<Message>, StoreError> {
        let sql = format!("SELECT {MESSAGE_COLUMNS} FROM pending_conversation_previews WHERE conversation_id = ?1");
        Ok(self.conn.query_row(&sql, [conversation], message_from_row).optional()?)
    }
}
