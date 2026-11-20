//! Conversations (a thread's `version` orders them) and their members (the thread's `rosterVersion` orders the
//! member list as a whole).

use rusqlite::{OptionalExtension, Row, params};

use crate::{Applied, OutboxEntry, Store, StoreError, sql_limit};

/// What kind of conversation a thread is. A kind this build doesn't name is kept as the text it came as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConversationKind {
    OneToOne,
    Group,
    Meeting,
    /// A team channel: read, never written to in M1.
    Channel,
    Other(String),
}

impl ConversationKind {
    /// The stored text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::OneToOne => "one_to_one",
            Self::Group => "group",
            Self::Meeting => "meeting",
            Self::Channel => "channel",
            Self::Other(text) => text,
        }
    }

    /// Whether it is a meeting's chat: sync keeps one by the thread type the list gave it (`thread_type:meeting`).
    #[must_use]
    pub fn is_meeting(&self) -> bool {
        match self {
            Self::Meeting => true,
            Self::Other(text) => text == "thread_type:meeting",
            Self::OneToOne | Self::Group | Self::Channel => false,
        }
    }

    /// The kind a stored text names; any other text is [`ConversationKind::Other`].
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        match text {
            "one_to_one" => Self::OneToOne,
            "group" => Self::Group,
            "meeting" => Self::Meeting,
            "channel" => Self::Channel,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// One conversation as the store keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    pub id: String,
    pub kind: ConversationKind,
    pub topic: Option<String>,
    /// The thread's `version`: a newer one replaces the row, an older or equal one is ignored.
    pub version: i64,
    /// Milliseconds: the newest of what the service said about the last message and every stored message's arrival.
    /// It never moves back.
    pub last_activity: Option<i64>,
}

/// What [`Store::forget_conversation`] removed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Forgotten {
    /// The conversation was stored.
    pub stored: bool,
    /// Its outbox rows that weren't sent (waiting, in flight, denied or failed), as they stood, oldest first: removed
    /// with the rest of its rows, so none of them goes out or can be resent.
    pub unsent: Vec<OutboxEntry>,
}

/// A thread the store forgot as deleted for good ([`Store::forget_conversation`]), as it was stored then: a
/// conversation list that still names it shows it changed only at a newer version or a later last message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForgottenThread {
    /// The thread's `version` when it was forgotten.
    pub version: i64,
    /// Its last activity then (milliseconds), when one was known.
    pub last_activity: Option<i64>,
}

/// A member of a conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// The member's MRI (`8:orgid:<oid>`, or a service's).
    pub mri: String,
    pub role: Option<String>,
}

impl Member {
    #[must_use]
    pub fn new(mri: &str, role: Option<&str>) -> Self {
        Self {
            mri: mri.to_owned(),
            role: role.map(str::to_owned),
        }
    }
}

const CONVERSATION_COLUMNS: &str = "id, kind, topic, version, last_activity";

fn conversation_from_row(row: &Row<'_>) -> rusqlite::Result<Conversation> {
    Ok(Conversation {
        id: row.get(0)?,
        kind: ConversationKind::from_text(&row.get::<_, String>(1)?),
        topic: row.get(2)?,
        version: row.get(3)?,
        last_activity: row.get(4)?,
    })
}

impl Store {
    /// Writes a conversation unless the stored one is as new or newer (by `version`). `last_activity` keeps the newer
    /// of the two whatever the version: an as-new or older version that brings a later last activity is
    /// [`Applied::Ignored`] for its versioned fields, and its last activity is still taken. Its name is of no known
    /// thread version ([`Store::upsert_conversation_at`]).
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn upsert_conversation(&mut self, conversation: &Conversation) -> Result<Applied, StoreError> {
        self.upsert_conversation_at(conversation, None)
    }

    /// [`Store::upsert_conversation`] for a conversation whose name is of the thread version `thread_version`, when it
    /// is known (a listing's `threadProperties.version`, a thread read's `version`). A newer conversation version
    /// replaces the name too, unless the stored name is of a newer thread version ([`Store::set_topic`]): a push
    /// brought it after the listing was read. An as-new or older conversation version keeps the stored name, but when
    /// the stored name has no thread version (a row from before thread versions were kept) and is the listing's name,
    /// it takes `thread_version`: then an older push cannot bring back an older name.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn upsert_conversation_at(
        &mut self,
        conversation: &Conversation,
        thread_version: Option<i64>,
    ) -> Result<Applied, StoreError> {
        let tx = self.conn.transaction()?;
        let stored: Option<i64> = tx
            .query_row(
                "SELECT version FROM conversations WHERE id = ?1",
                [&conversation.id],
                |row| row.get(0),
            )
            .optional()?;
        let applied = match stored {
            None => {
                // stored again: no longer forgotten
                tx.execute("DELETE FROM forgotten_conversations WHERE id = ?1", [&conversation.id])?;
                tx.execute(
                    "INSERT INTO conversations (id, kind, topic, version, last_activity, topic_version)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        conversation.id,
                        conversation.kind.as_str(),
                        conversation.topic,
                        conversation.version,
                        conversation.last_activity,
                        thread_version
                    ],
                )?;
                Applied::Inserted
            }
            Some(version) if conversation.version > version => {
                tx.execute(
                    "UPDATE conversations SET kind = ?2, version = ?4,
                         topic = CASE WHEN ?6 < topic_version THEN topic ELSE ?3 END,
                         topic_version = MAX(COALESCE(topic_version, ?6), COALESCE(?6, topic_version)),
                         last_activity = MAX(COALESCE(last_activity, ?5), COALESCE(?5, last_activity))
                     WHERE id = ?1",
                    params![
                        conversation.id,
                        conversation.kind.as_str(),
                        conversation.topic,
                        conversation.version,
                        conversation.last_activity,
                        thread_version
                    ],
                )?;
                Applied::Updated
            }
            Some(_) => {
                // the version's fields stay, but a later last activity is still taken, and the same name's thread
                // version when the store has none
                tx.execute(
                    "UPDATE conversations
                     SET last_activity = MAX(COALESCE(last_activity, ?2), COALESCE(?2, last_activity)),
                         topic_version = CASE WHEN topic_version IS NULL AND topic IS ?3 THEN ?4 ELSE topic_version END
                     WHERE id = ?1",
                    params![
                        conversation.id,
                        conversation.last_activity,
                        conversation.topic,
                        thread_version
                    ],
                )?;
                Applied::Ignored
            }
        };
        tx.commit()?;
        Ok(applied)
    }

    /// Names the stored conversation `id` `topic` (`None` takes the name off), whatever its version: a group chat the
    /// account renamed (no `version`: the answer gives none), or one a push says was renamed at the thread version
    /// `version`. A name of a thread version that is not newer than that of the stored name is ignored, so a late or
    /// replayed push does not bring back an old name. The conversation's version stays, so the next listing of a newer
    /// version still replaces the row. Whether the name changed: `false` for the same name, an older one, or a
    /// conversation the store doesn't know.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn set_topic(&mut self, id: &str, topic: Option<&str>, version: Option<i64>) -> Result<bool, StoreError> {
        let tx = self.conn.transaction()?;
        let stored: Option<(Option<String>, Option<i64>)> = tx
            .query_row(
                "SELECT topic, topic_version FROM conversations WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((stored_topic, stored_version)) = stored else {
            return Ok(false);
        };
        if let (Some(version), Some(stored_version)) = (version, stored_version)
            && version <= stored_version
        {
            return Ok(false);
        }
        tx.execute(
            "UPDATE conversations SET topic = ?2, topic_version = COALESCE(?3, topic_version) WHERE id = ?1",
            params![id, topic, version],
        )?;
        tx.commit()?;
        Ok(stored_topic.as_deref() != topic)
    }

    /// Forgets `id` and all that is kept of it: its members, messages (their reactions, reply roots and search
    /// entries), read horizons, sync state and its mark to be synced again all go with its row; its outbox rows and its
    /// place among the favourites, which don't reference it, go in the same transaction. For a thread the service says
    /// is gone (hard-deleted): whether it was stored, and its rows that weren't sent (a write to it can never succeed).
    /// What stays is its version and last activity ([`Store::forgotten_thread`]) until it is stored again, so a
    /// conversation list that still names it isn't taken for one bringing a new conversation.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn forget_conversation(&mut self, id: &str) -> Result<Forgotten, StoreError> {
        let tx = self.conn.transaction()?;
        let unsent = crate::outbox::forget_rows(&tx, id)?;
        tx.execute("DELETE FROM favorites WHERE conversation_id = ?1", [id])?;
        tx.execute(
            "INSERT OR REPLACE INTO forgotten_conversations (id, version, last_activity)
             SELECT id, version, last_activity FROM conversations WHERE id = ?1",
            [id],
        )?;
        let removed = tx.execute("DELETE FROM conversations WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(Forgotten {
            stored: removed > 0,
            unsent,
        })
    }

    /// The thread `id` as the store forgot it as deleted ([`Store::forget_conversation`]); none when it never was, or
    /// was stored again since.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn forgotten_thread(&self, id: &str) -> Result<Option<ForgottenThread>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT version, last_activity FROM forgotten_conversations WHERE id = ?1",
                [id],
                |row| {
                    Ok(ForgottenThread {
                        version: row.get(0)?,
                        last_activity: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// One conversation.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn conversation(&self, id: &str) -> Result<Option<Conversation>, StoreError> {
        let sql = format!("SELECT {CONVERSATION_COLUMNS} FROM conversations WHERE id = ?1");
        Ok(self.conn.query_row(&sql, [id], conversation_from_row).optional()?)
    }

    /// Up to `limit` conversations, the most recently active first; ones with no activity last.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn conversations_by_activity(&self, limit: usize) -> Result<Vec<Conversation>, StoreError> {
        let sql = format!(
            "SELECT {CONVERSATION_COLUMNS} FROM conversations
             ORDER BY last_activity IS NULL, last_activity DESC, id LIMIT ?1"
        );
        let mut statement = self.conn.prepare_cached(&sql)?;
        let rows = statement.query_map([sql_limit(limit)], conversation_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Replaces a conversation's members with `members` unless the stored roster is as new or newer (by
    /// `rosterVersion`). The list is the whole membership: anyone not in it is removed.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownConversation`] when the conversation isn't stored, [`StoreError::Sqlite`] when the
    /// write fails.
    pub fn replace_members(
        &mut self,
        conversation_id: &str,
        roster_version: i64,
        members: &[Member],
    ) -> Result<Applied, StoreError> {
        let tx = self.conn.transaction()?;
        let stored: Option<Option<i64>> = tx
            .query_row(
                "SELECT roster_version FROM conversations WHERE id = ?1",
                [conversation_id],
                |row| row.get(0),
            )
            .optional()?;
        let applied = match stored {
            None => return Err(StoreError::UnknownConversation(conversation_id.to_owned())),
            Some(None) => Applied::Inserted,
            Some(Some(version)) if roster_version > version => Applied::Updated,
            Some(Some(_)) => return Ok(Applied::Ignored),
        };
        tx.execute("DELETE FROM members WHERE conversation_id = ?1", [conversation_id])?;
        {
            let mut insert =
                tx.prepare_cached("INSERT INTO members (conversation_id, mri, role) VALUES (?1, ?2, ?3)")?;
            for member in members {
                insert.execute(params![conversation_id, member.mri, member.role])?;
            }
        }
        tx.execute(
            "UPDATE conversations SET roster_version = ?2 WHERE id = ?1",
            params![conversation_id, roster_version],
        )?;
        tx.commit()?;
        Ok(applied)
    }

    /// A conversation's members, by MRI.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn members(&self, conversation_id: &str) -> Result<Vec<Member>, StoreError> {
        let mut statement = self
            .conn
            .prepare_cached("SELECT mri, role FROM members WHERE conversation_id = ?1 ORDER BY mri")?;
        let rows = statement.query_map([conversation_id], |row| {
            Ok(Member {
                mri: row.get(0)?,
                role: row.get(1)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The unnamed group chat whose members are exactly `members` (by MRI, in any order, each once), the one whose roster
    /// came to be last (its `rosterVersion`: a chat nobody joined or left since is the newest made) when there are
    /// several: the chat Teams' New message opens for those people, passing over a named one however new. None when no
    /// stored unnamed group chat has them all and no one else.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn group_chat_with(&self, members: &[String]) -> Result<Option<String>, StoreError> {
        let mut wanted: Vec<&str> = members.iter().map(String::as_str).collect();
        wanted.sort_unstable();
        wanted.dedup();
        if wanted.is_empty() {
            return Ok(None);
        }
        let mut statement = self.conn.prepare_cached(
            "SELECT c.id, group_concat(m.mri, char(10)) FROM conversations c JOIN members m ON m.conversation_id = c.id
             WHERE c.kind = 'group' AND COALESCE(c.topic, '') = '' GROUP BY c.id
             ORDER BY c.roster_version IS NULL, c.roster_version DESC, c.id",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, joined) = row?;
            let mut have: Vec<&str> = joined.split('\n').collect();
            have.sort_unstable();
            have.dedup();
            if have == wanted {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Keeps whether the chat service calls `conversation` empty (`None`: it said nothing readable); whether that
    /// changed. Nothing for a conversation the store doesn't hold.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn set_conversation_empty(&mut self, conversation: &str, empty: Option<bool>) -> Result<bool, StoreError> {
        let changed = self
            .conn
            .prepare_cached("UPDATE conversations SET empty = ?2 WHERE id = ?1 AND empty IS NOT ?2")?
            .execute(params![conversation, empty])?;
        Ok(changed > 0)
    }

    /// The conversations the chat service last called empty, by id, in no set order.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn empty_conversations(&self) -> Result<Vec<String>, StoreError> {
        let mut statement = self
            .conn
            .prepare_cached("SELECT id FROM conversations WHERE empty = 1")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The `rosterVersion` the stored members are at; none before the first roster (or for an unknown conversation).
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn roster_version(&self, conversation_id: &str) -> Result<Option<i64>, StoreError> {
        let version = self
            .conn
            .query_row(
                "SELECT roster_version FROM conversations WHERE id = ?1",
                [conversation_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(version.flatten())
    }
}
