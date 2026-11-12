//! Chats translated on this computer (schema 20): each message's translation by its id, version and target language,
//! and the account's own messages as written when they were sent translated; and each chat's glossary (schema 22),
//! the renderings the translator chose for its terms, so a later translation keeps them. The app translates (`teams-ui`'s chat
//! translation); the store keeps what it made, so a later run shows it without asking the translator again. The rich
//! text is kept as the text the app hands in; the store never reads it. `Debug` leaves the text out.

use std::fmt;

use rusqlite::{params, params_from_iter};

use crate::{Store, StoreError};

/// What a kept translation is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TranslationKind {
    /// The message translated into the language.
    Translated,
    /// The message was in the language already: shown as it is.
    Same,
    /// The account's own message as it was written, before it was sent translated.
    Original,
}

impl TranslationKind {
    /// Its name in the store.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Translated => "translated",
            Self::Same => "same",
            Self::Original => "original",
        }
    }

    fn from_key(key: &str) -> Self {
        match key {
            "same" => Self::Same,
            "original" => Self::Original,
            _ => Self::Translated,
        }
    }
}

/// One message's translation into one language.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredTranslation {
    pub conversation_id: String,
    pub message_id: String,
    /// The message's version it translates: a newer version is translated again.
    pub version: i64,
    /// The language translated into (the app's code: `de`, `pt-br`).
    pub language: String,
    pub kind: TranslationKind,
    /// The rich text in `language`, as the app wrote it; empty for [`TranslationKind::Same`].
    pub content: String,
    /// The language the message was written in, as the translator named it.
    pub source_language: Option<String>,
}

impl fmt::Debug for StoredTranslation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredTranslation")
            .field("conversation_id", &self.conversation_id)
            .field("message_id", &self.message_id)
            .field("version", &self.version)
            .field("language", &self.language)
            .field("kind", &self.kind)
            .field("source_language", &self.source_language)
            .finish_non_exhaustive()
    }
}

/// The account's own message as written, kept when it was sent translated.
#[derive(Clone, PartialEq, Eq)]
pub struct SentOriginal {
    pub conversation_id: String,
    /// The post's `clientmessageid`, or `edit:<message id>` for an edit ([`SentOriginal::edit_key`]).
    pub key: String,
    /// The language it was written in (the chat's chosen language then).
    pub language: String,
    /// Its rich text as written, as the app wrote it.
    pub content: String,
    /// The plain text that was sent (the translation), to match the message it became.
    pub sent_text: String,
    /// When it was sent, ms since the epoch.
    pub created_at: i64,
}

impl SentOriginal {
    /// The key an edit of message `message_id` is kept by.
    #[must_use]
    pub fn edit_key(message_id: &str) -> String {
        format!("edit:{message_id}")
    }
}

impl fmt::Debug for SentOriginal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SentOriginal")
            .field("conversation_id", &self.conversation_id)
            .field("key", &self.key)
            .field("language", &self.language)
            .field("created_at", &self.created_at)
            .finish_non_exhaustive()
    }
}

/// A term of a translated chat as it writes it, and the rendering chosen for it in a language.
#[derive(Clone, PartialEq, Eq)]
pub struct GlossaryTerm {
    pub conversation_id: String,
    /// The language the chat is read in (the app's code).
    pub language: String,
    /// The term as the chat writes it.
    pub term: String,
    /// Its rendering in `language`.
    pub rendering: String,
    /// When the translator last named it, ms since the epoch.
    pub used_at: i64,
}

impl fmt::Debug for GlossaryTerm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GlossaryTerm")
            .field("conversation_id", &self.conversation_id)
            .field("language", &self.language)
            .field("used_at", &self.used_at)
            .finish_non_exhaustive()
    }
}

/// The most terms a chat's glossary keeps per language: the newest named.
pub const GLOSSARY_TERMS: usize = 40;

/// [`GLOSSARY_TERMS`] as SQLite takes a limit.
fn glossary_limit() -> i64 {
    i64::try_from(GLOSSARY_TERMS).unwrap_or(i64::MAX)
}

/// The most ids one read takes at a time (SQLite's bound parameters).
const CHUNK: usize = 400;

impl Store {
    /// The kept translations into `language` of the messages `message_ids` of `conversation_id`, every version kept.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn translations(
        &self,
        conversation_id: &str,
        language: &str,
        message_ids: &[String],
    ) -> Result<Vec<StoredTranslation>, StoreError> {
        let mut found = Vec::new();
        for chunk in message_ids.chunks(CHUNK) {
            let marks = vec!["?"; chunk.len()].join(", ");
            let sql = format!(
                "SELECT message_id, version, kind, content, source_language FROM message_translations
                 WHERE conversation_id = ? AND language = ? AND message_id IN ({marks})"
            );
            let values = [conversation_id, language]
                .into_iter()
                .chain(chunk.iter().map(String::as_str));
            let rows = self
                .conn
                .prepare(&sql)?
                .query_map(params_from_iter(values), |row| {
                    Ok(StoredTranslation {
                        conversation_id: conversation_id.to_owned(),
                        message_id: row.get(0)?,
                        version: row.get(1)?,
                        language: language.to_owned(),
                        kind: TranslationKind::from_key(&row.get::<_, String>(2)?),
                        content: row.get(3)?,
                        source_language: row.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            found.extend(rows);
        }
        Ok(found)
    }

    /// Keeps `translations`, in one transaction: each replaces what was kept for its message, version and language,
    /// and a message's older versions in that language go. One of a message the store doesn't hold is left out.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails; nothing changed then.
    pub fn put_translations(&mut self, translations: &[StoredTranslation]) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        {
            let mut older = tx.prepare_cached(
                "DELETE FROM message_translations
                 WHERE conversation_id = ?1 AND message_id = ?2 AND language = ?3 AND version < ?4",
            )?;
            let mut row = tx.prepare_cached(
                "INSERT INTO message_translations
                     (conversation_id, message_id, version, language, kind, content, source_language)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7
                 WHERE EXISTS (SELECT 1 FROM messages WHERE conversation_id = ?1 AND id = ?2)
                 ON CONFLICT (conversation_id, message_id, version, language) DO UPDATE SET
                     kind = excluded.kind, content = excluded.content, source_language = excluded.source_language",
            )?;
            for kept in translations {
                older.execute(params![
                    kept.conversation_id,
                    kept.message_id,
                    kept.language,
                    kept.version
                ])?;
                row.execute(params![
                    kept.conversation_id,
                    kept.message_id,
                    kept.version,
                    kept.language,
                    kept.kind.key(),
                    kept.content,
                    kept.source_language,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The account's own messages of `conversation_id` as written, by `keys` ([`SentOriginal::key`]).
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn sent_originals(&self, conversation_id: &str, keys: &[String]) -> Result<Vec<SentOriginal>, StoreError> {
        let mut found = Vec::new();
        for chunk in keys.chunks(CHUNK) {
            let marks = vec!["?"; chunk.len()].join(", ");
            let sql = format!(
                "SELECT key, language, content, sent_text, created_at FROM sent_originals
                 WHERE conversation_id = ? AND key IN ({marks})"
            );
            let values = std::iter::once(conversation_id).chain(chunk.iter().map(String::as_str));
            let rows = self
                .conn
                .prepare(&sql)?
                .query_map(params_from_iter(values), |row| {
                    Ok(SentOriginal {
                        conversation_id: conversation_id.to_owned(),
                        key: row.get(0)?,
                        language: row.get(1)?,
                        content: row.get(2)?,
                        sent_text: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            found.extend(rows);
        }
        Ok(found)
    }

    /// Keeps `original`, replacing one kept under its key (a later edit of the same message). One of a conversation
    /// the store doesn't hold is left out.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn put_sent_original(&mut self, original: &SentOriginal) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO sent_originals (conversation_id, key, language, content, sent_text, created_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6 WHERE EXISTS (SELECT 1 FROM conversations WHERE id = ?1)
             ON CONFLICT (conversation_id, key) DO UPDATE SET language = excluded.language,
                 content = excluded.content, sent_text = excluded.sent_text, created_at = excluded.created_at",
            params![
                original.conversation_id,
                original.key,
                original.language,
                original.content,
                original.sent_text,
                original.created_at,
            ],
        )?;
        Ok(())
    }

    /// The glossary of `conversation_id` in `language`: at most [`GLOSSARY_TERMS`], the newest named first.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn glossary(&self, conversation_id: &str, language: &str) -> Result<Vec<GlossaryTerm>, StoreError> {
        let rows = self
            .conn
            .prepare_cached(
                "SELECT term, rendering, used_at FROM translation_glossary
                 WHERE conversation_id = ?1 AND language = ?2 ORDER BY used_at DESC, term LIMIT ?3",
            )?
            .query_map(params![conversation_id, language, glossary_limit()], |row| {
                Ok(GlossaryTerm {
                    conversation_id: conversation_id.to_owned(),
                    language: language.to_owned(),
                    term: row.get(0)?,
                    rendering: row.get(1)?,
                    used_at: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Keeps `terms`, in one transaction: each replaces what its chat and language kept for the term, and each chat's
    /// glossary in each language is trimmed to its newest [`GLOSSARY_TERMS`]. One of a conversation the store doesn't
    /// hold is left out.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails; nothing changed then.
    pub fn put_glossary(&mut self, terms: &[GlossaryTerm]) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        {
            let mut row = tx.prepare_cached(
                "INSERT INTO translation_glossary (conversation_id, language, term, rendering, used_at)
                 SELECT ?1, ?2, ?3, ?4, ?5 WHERE EXISTS (SELECT 1 FROM conversations WHERE id = ?1)
                 ON CONFLICT (conversation_id, language, term) DO UPDATE SET
                     rendering = excluded.rendering, used_at = excluded.used_at",
            )?;
            let mut trim = tx.prepare_cached(
                "DELETE FROM translation_glossary WHERE conversation_id = ?1 AND language = ?2 AND term NOT IN (
                     SELECT term FROM translation_glossary WHERE conversation_id = ?1 AND language = ?2
                     ORDER BY used_at DESC, term LIMIT ?3)",
            )?;
            for term in terms {
                row.execute(params![
                    term.conversation_id,
                    term.language,
                    term.term,
                    term.rendering,
                    term.used_at,
                ])?;
            }
            let mut chats: Vec<(&str, &str)> = terms
                .iter()
                .map(|term| (term.conversation_id.as_str(), term.language.as_str()))
                .collect();
            chats.sort_unstable();
            chats.dedup();
            for (conversation_id, language) in chats {
                trim.execute(params![conversation_id, language, glossary_limit()])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}
