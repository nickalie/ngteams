//! Messages by conversation and id, ordered by their `version`: an edit or a soft delete is a newer version of the
//! same message, and a version that arrives late (a sync after the Trouter delivery, or the other way round) changes
//! nothing. Reactions are kept per user, each with the time of its last change.

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::{Applied, Store, StoreError, sql_limit};

/// What a meeting's chat gets from the meeting itself rather than from anyone writing in it. Teams doesn't call a
/// meeting's chat unread for one (the same list holds `Event/Call` and `ThreadActivity/AddMember`, which count nowhere
/// here), while in any other chat a recording counts as a message does.
const MEETING_ARTIFACTS: [&str; 3] = [
    "RichText/Media_CallRecording",
    "RichText/Media_LocalRecording",
    "RichText/Media_CallTranscript",
];

/// Whether a message of `message_type` in `conversation_id` can make it unread, as [`Store::unread_count`] counts:
/// `Text` and every `RichText…` type (a card too), but no [`MEETING_ARTIFACTS`] in a meeting's chat, told by its id as
/// Teams tells it.
#[must_use]
pub fn counts_unread(conversation_id: &str, message_type: &str) -> bool {
    (message_type == "Text" || message_type.starts_with("RichText"))
        && !(conversation_id.contains("meeting") && MEETING_ARTIFACTS.contains(&message_type))
}

/// One message as the store keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub conversation_id: String,
    /// The service's message id.
    pub id: String,
    /// The service's `version`: a newer one replaces the row, an older or equal one is ignored.
    pub version: i64,
    /// The `clientmessageid` it was sent with: how our own message coming back is matched to its outbox row.
    pub client_message_id: Option<String>,
    /// The sender's MRI.
    pub sender: Option<String>,
    /// The sender's display name as the message carried it (`imdisplayname`).
    pub sender_name: Option<String>,
    /// `messagetype` as the service sent it (`RichText/Html`, `Text`, `ThreadActivity/…`, …).
    pub message_type: String,
    /// The content as the service sent it (HTML for rich text); empty once deleted.
    pub content: String,
    /// The message's `properties` object (mentions, files, …) as JSON text; none once deleted.
    pub properties: Option<String>,
    /// `originalarrivaltime`, in milliseconds: the order of a conversation.
    pub arrival_time: i64,
    pub compose_time: Option<i64>,
    /// When it was last edited, if ever.
    pub edit_time: Option<i64>,
    /// When it was deleted (a soft delete keeps the row, without its content).
    pub delete_time: Option<i64>,
}

/// One user's reaction to a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reaction {
    /// The reaction's key (`like`, `heart`, …).
    pub key: String,
    /// The reacting user's MRI.
    pub user: String,
    /// When it was set, in milliseconds.
    pub time: i64,
}

/// One user's reaction set or removed. The change with the later `time` wins, so a late one can't undo a newer one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionChange {
    pub conversation_id: String,
    pub message_id: String,
    pub key: String,
    pub user: String,
    pub time: i64,
    /// Set (`true`) or removed.
    pub present: bool,
}

impl ReactionChange {
    #[must_use]
    pub fn add(conversation_id: &str, message_id: &str, key: &str, user: &str, time: i64) -> Self {
        Self::new(conversation_id, message_id, key, user, time, true)
    }

    #[must_use]
    pub fn remove(conversation_id: &str, message_id: &str, key: &str, user: &str, time: i64) -> Self {
        Self::new(conversation_id, message_id, key, user, time, false)
    }

    fn new(conversation_id: &str, message_id: &str, key: &str, user: &str, time: i64, present: bool) -> Self {
        Self {
            conversation_id: conversation_id.to_owned(),
            message_id: message_id.to_owned(),
            key: key.to_owned(),
            user: user.to_owned(),
            time,
            present,
        }
    }
}

/// Where a page of messages ends: the next page starts just before it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageCursor {
    pub arrival_time: i64,
    pub id: String,
}

/// A page of a conversation's messages, the newest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagePage {
    pub messages: Vec<Message>,
    /// The cursor for the next (older) page; none when this page reaches the oldest stored message.
    pub next: Option<PageCursor>,
}

pub(crate) const MESSAGE_COLUMNS: &str = "conversation_id, id, version, client_message_id, sender, sender_name, message_type, \
                               content, properties, arrival_time, compose_time, edit_time, delete_time";

pub(crate) fn message_from_row(row: &Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        conversation_id: row.get(0)?,
        id: row.get(1)?,
        version: row.get(2)?,
        client_message_id: row.get(3)?,
        sender: row.get(4)?,
        sender_name: row.get(5)?,
        message_type: row.get(6)?,
        content: row.get(7)?,
        properties: row.get(8)?,
        arrival_time: row.get(9)?,
        compose_time: row.get(10)?,
        edit_time: row.get(11)?,
        delete_time: row.get(12)?,
    })
}

/// Writes the message's row (a new one or a newer version of a stored one), without its content once deleted.
fn write_message(conn: &Connection, message: &Message) -> rusqlite::Result<()> {
    let deleted = message.delete_time.is_some();
    let content = if deleted { "" } else { message.content.as_str() };
    let properties = if deleted { None } else { message.properties.as_deref() };
    conn.execute(
        &format!(
            "INSERT INTO messages ({MESSAGE_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT (conversation_id, id) DO UPDATE SET version = excluded.version,
                 client_message_id = excluded.client_message_id, sender = excluded.sender,
                 sender_name = excluded.sender_name, message_type = excluded.message_type,
                 content = excluded.content, properties = excluded.properties,
                 arrival_time = excluded.arrival_time, compose_time = excluded.compose_time,
                 edit_time = excluded.edit_time, delete_time = excluded.delete_time"
        ),
        params![
            message.conversation_id,
            message.id,
            message.version,
            message.client_message_id,
            message.sender,
            message.sender_name,
            message.message_type,
            content,
            properties,
            message.arrival_time,
            message.compose_time,
            message.edit_time,
            message.delete_time,
        ],
    )?;
    conn.execute(
        "UPDATE conversations SET last_activity = MAX(COALESCE(last_activity, ?2), ?2) WHERE id = ?1",
        params![message.conversation_id, message.arrival_time],
    )?;
    Ok(())
}

/// One fact about one user's reaction: set or removed, ordered by `changed`.
struct ReactionFact<'a> {
    key: &'a str,
    user: &'a str,
    /// When the reaction was set (kept on a removal).
    time: i64,
    /// What orders it against the stored row: a per-user change's time, or the version of the set it came in.
    changed: i64,
    present: bool,
}

impl ReactionFact<'_> {
    /// Whether it wins over a stored row last changed at `changed`: the later change wins, and of two at the same
    /// time the removal, so the result doesn't depend on which came first.
    fn wins_over(&self, changed: i64, present: bool) -> bool {
        self.changed > changed || (self.changed == changed && present && !self.present)
    }
}

/// Writes one reaction fact unless the stored row (or, with no row, the newest whole set that left it out) is as
/// new or newer.
fn apply_reaction_fact(
    conn: &Connection,
    conversation_id: &str,
    message_id: &str,
    fact: &ReactionFact<'_>,
) -> rusqlite::Result<Applied> {
    let stored: Option<(i64, bool)> = conn
        .query_row(
            "SELECT changed, present FROM reactions
             WHERE conversation_id = ?1 AND message_id = ?2 AND reaction = ?3 AND user_mri = ?4",
            params![conversation_id, message_id, fact.key, fact.user],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let applied = match stored {
        Some((changed, present)) if fact.wins_over(changed, present) => Applied::Updated,
        Some(_) => return Ok(Applied::Ignored),
        None => {
            let left_out_at: Option<i64> = conn.query_row(
                "SELECT reactions_as_of FROM messages WHERE conversation_id = ?1 AND id = ?2",
                params![conversation_id, message_id],
                |row| row.get(0),
            )?;
            // with no row, the reaction was absent when that set was taken
            if left_out_at.is_some_and(|as_of| !fact.wins_over(as_of, false)) {
                return Ok(Applied::Ignored);
            }
            Applied::Inserted
        }
    };
    conn.execute(
        "INSERT INTO reactions (conversation_id, message_id, reaction, user_mri, time, changed, present)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT DO UPDATE SET time = excluded.time, changed = excluded.changed, present = excluded.present",
        params![
            conversation_id,
            message_id,
            fact.key,
            fact.user,
            fact.time,
            fact.changed,
            fact.present
        ],
    )?;
    Ok(applied)
}

/// Merges a message's whole reaction set as of its `version`: each reaction in it is set, and each stored one not in
/// it removed, at that version, unless the stored row changed later; a reaction with no row is taken as absent from
/// then on.
fn merge_reactions(conn: &Connection, message: &Message, reactions: &[Reaction]) -> rusqlite::Result<()> {
    let (conversation_id, message_id, as_of) = (message.conversation_id.as_str(), message.id.as_str(), message.version);
    let stored: Vec<(String, String, i64)> = {
        let mut statement = conn.prepare_cached(
            "SELECT reaction, user_mri, time FROM reactions WHERE conversation_id = ?1 AND message_id = ?2",
        )?;
        let rows = statement.query_map(params![conversation_id, message_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        rows.collect::<Result<_, _>>()?
    };
    let left_out = stored
        .iter()
        .filter(|(key, user, _)| !reactions.iter().any(|r| &r.key == key && &r.user == user));
    for (key, user, time) in left_out {
        let removed = ReactionFact {
            key,
            user,
            time: *time,
            changed: as_of,
            present: false,
        };
        apply_reaction_fact(conn, conversation_id, message_id, &removed)?;
    }
    for reaction in reactions {
        let set = ReactionFact {
            key: &reaction.key,
            user: &reaction.user,
            time: reaction.time,
            changed: as_of.max(reaction.time),
            present: true,
        };
        apply_reaction_fact(conn, conversation_id, message_id, &set)?;
    }
    conn.execute(
        "UPDATE messages SET reactions_as_of = MAX(COALESCE(reactions_as_of, ?3), ?3)
         WHERE conversation_id = ?1 AND id = ?2",
        params![conversation_id, message_id, as_of],
    )?;
    Ok(())
}

/// [`Store::upsert_message`]'s write, inside the caller's transaction.
pub(crate) fn upsert(
    conn: &Connection,
    message: &Message,
    reactions: Option<&[Reaction]>,
) -> Result<Applied, StoreError> {
    Store::require_conversation(conn, &message.conversation_id)?;
    let stored: Option<(i64, bool)> = conn
        .query_row(
            "SELECT version, delete_time IS NOT NULL FROM messages WHERE conversation_id = ?1 AND id = ?2",
            params![message.conversation_id, message.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let applied = match stored {
        None => Applied::Inserted,
        Some((version, _)) if message.version > version => Applied::Updated,
        Some(_) => Applied::Ignored,
    };
    if applied != Applied::Ignored {
        write_message(conn, message)?;
    }
    let deleted = if applied == Applied::Ignored {
        stored.is_some_and(|(_, deleted)| deleted)
    } else {
        message.delete_time.is_some()
    };
    let reactions = if deleted || message.delete_time.is_some() {
        Some(&[][..])
    } else {
        reactions
    };
    if let Some(reactions) = reactions {
        merge_reactions(conn, message, reactions)?;
    }
    if deleted {
        // Deletion is decided by the stored body, independently of reaction clocks. Keep the facts as tombstones
        // so a newer nondeleted body cannot revive them through an older reaction snapshot.
        conn.execute(
            "UPDATE reactions SET present = 0 WHERE conversation_id = ?1 AND message_id = ?2 AND present = 1",
            params![message.conversation_id, message.id],
        )?;
    }
    Ok(applied)
}

impl Store {
    /// Writes a message unless the stored one is as new or newer (by `version`), and moves the conversation's last
    /// activity up to its arrival. A deleted message (`delete_time` set) is stored without content, properties or
    /// reactions.
    ///
    /// `reactions`, when given, is the message's whole reaction set as of its `version` (the chat service's time of
    /// the change, in milliseconds, on the clock reaction times use). For a live stored message it is merged per
    /// user, like a [`ReactionChange`] for each reaction in it and a removal for each stored one not in it, all at
    /// that version: a stored change that is later stays, and a reaction left out stays absent against a later-arriving
    /// older change, so a set and the per-user changes leave the same rows in whichever order they arrive. A stale
    /// version's set is merged too and changes only what is older than it. When not given, the stored reactions
    /// stay. If the effective stored message is deleted, any incoming set is ignored and every reaction is cleared,
    /// regardless of its change time. Cleared rows retain their change times to order later sets if the message is
    /// restored.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownConversation`] when the conversation isn't stored, [`StoreError::Sqlite`] when the write
    /// fails.
    ///
    /// The message's text isn't indexed for search here: [`Store::upsert_message_indexed`] indexes it, and the core's
    /// backfill indexes a message stored this way later.
    pub fn upsert_message(&mut self, message: &Message, reactions: Option<&[Reaction]>) -> Result<Applied, StoreError> {
        let tx = self.conn.transaction()?;
        let applied = upsert(&tx, message, reactions)?;
        tx.commit()?;
        Ok(applied)
    }

    /// Applies one user's reaction change unless a change at the same or a later time is stored (of two at the same
    /// time, the removal wins), or the newest whole reaction set merged in left the reaction out and is as new or
    /// newer. A change on a deleted message is ignored.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownMessage`] when the message isn't stored, [`StoreError::Sqlite`] when the write fails.
    pub fn apply_reaction(&mut self, change: &ReactionChange) -> Result<Applied, StoreError> {
        let tx = self.conn.transaction()?;
        let message: Option<Option<i64>> = tx
            .query_row(
                "SELECT delete_time FROM messages WHERE conversation_id = ?1 AND id = ?2",
                params![change.conversation_id, change.message_id],
                |row| row.get(0),
            )
            .optional()?;
        match message {
            None => {
                return Err(StoreError::UnknownMessage {
                    conversation: change.conversation_id.clone(),
                    message: change.message_id.clone(),
                });
            }
            Some(Some(_deleted)) => return Ok(Applied::Ignored),
            Some(None) => {}
        }
        let fact = ReactionFact {
            key: &change.key,
            user: &change.user,
            time: change.time,
            changed: change.time,
            present: change.present,
        };
        let applied = apply_reaction_fact(&tx, &change.conversation_id, &change.message_id, &fact)?;
        tx.commit()?;
        Ok(applied)
    }

    /// A message's reactions that are set, by key and then user.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn reactions(&self, conversation_id: &str, message_id: &str) -> Result<Vec<Reaction>, StoreError> {
        let mut statement = self.conn.prepare_cached(
            "SELECT reaction, user_mri, time FROM reactions
             WHERE conversation_id = ?1 AND message_id = ?2 AND present = 1 ORDER BY reaction, user_mri",
        )?;
        let rows = statement.query_map(params![conversation_id, message_id], |row| {
            Ok(Reaction {
                key: row.get(0)?,
                user: row.get(1)?,
                time: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The reaction keys `user` has set and not taken back, on any stored message, the one set most recently first; at
    /// most `limit`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn recent_reactions(&self, user: &str, limit: usize) -> Result<Vec<String>, StoreError> {
        let mut statement = self.conn.prepare_cached(
            "SELECT reaction FROM reactions WHERE user_mri = ?1 AND present = 1
             GROUP BY reaction ORDER BY MAX(time) DESC, reaction LIMIT ?2",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = statement.query_map(params![user, limit], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The reaction keys `user` uses most: how many stored messages hold each one it set and hasn't taken back, the
    /// most used first (of as many, the one set most recently); at most `limit`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn frequent_reactions(&self, user: &str, limit: usize) -> Result<Vec<String>, StoreError> {
        let mut statement = self.conn.prepare_cached(
            "SELECT reaction FROM reactions WHERE user_mri = ?1 AND present = 1
             GROUP BY reaction ORDER BY COUNT(*) DESC, MAX(time) DESC, reaction LIMIT ?2",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = statement.query_map(params![user, limit], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// One message.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn message(&self, conversation_id: &str, id: &str) -> Result<Option<Message>, StoreError> {
        let sql = format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE conversation_id = ?1 AND id = ?2");
        Ok(self
            .conn
            .query_row(&sql, params![conversation_id, id], message_from_row)
            .optional()?)
    }

    /// The message sent with `client_message_id` in a conversation: how our own message is recognised when it comes
    /// back through Trouter or a sync.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn message_by_client_id(
        &self,
        conversation_id: &str,
        client_message_id: &str,
    ) -> Result<Option<Message>, StoreError> {
        let sql = format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages WHERE conversation_id = ?1 AND client_message_id = ?2
             ORDER BY arrival_time, id LIMIT 1"
        );
        Ok(self
            .conn
            .query_row(&sql, params![conversation_id, client_message_id], message_from_row)
            .optional()?)
    }

    /// Up to `limit` messages of a conversation older than `before` (from the newest when none), the newest first.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn messages_before(
        &self,
        conversation_id: &str,
        before: Option<&PageCursor>,
        limit: usize,
    ) -> Result<MessagePage, StoreError> {
        let sql = format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE conversation_id = ?1 AND (?2 IS NULL OR arrival_time < ?2 OR (arrival_time = ?2 AND id < ?3))
             ORDER BY arrival_time DESC, id DESC LIMIT ?4"
        );
        let mut statement = self.conn.prepare_cached(&sql)?;
        let (arrival, id) = before.map(|cursor| (cursor.arrival_time, cursor.id.as_str())).unzip();
        // one more than asked for, to know whether an older page exists
        let rows = statement.query_map(
            params![conversation_id, arrival, id, sql_limit(limit).saturating_add(1)],
            message_from_row,
        )?;
        let mut messages: Vec<Message> = rows.collect::<Result<_, _>>()?;
        let more = messages.len() > limit;
        messages.truncate(limit);
        let next = messages.last().filter(|_| more).map(|last| PageCursor {
            arrival_time: last.arrival_time,
            id: last.id.clone(),
        });
        Ok(MessagePage { messages, next })
    }

    /// How many messages of a conversation `me` hasn't read: messages from others that [count](counts_unread), not
    /// deleted, that arrived after `me`'s read horizon (all of them without one) and the account's mark-all-read one
    /// ([`Store::app_read_horizon`]). None when the newest message that counts is `me`'s own, as Teams calls such a
    /// chat read whatever its horizon says (`docs/protocol/chat.md`, "Which chats are unread").
    /// Before history arrives, a list-page preview can supply one unread message.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn unread_count(&self, conversation_id: &str, me: &str) -> Result<u64, StoreError> {
        let count: i64 = self.conn.query_row(
            "WITH available AS (
                 SELECT id, sender, arrival_time, delete_time, message_type FROM messages WHERE conversation_id = ?1
                 UNION ALL
                 SELECT id, sender, arrival_time, delete_time, message_type FROM pending_conversation_previews
                 WHERE conversation_id = ?1 AND sender IS NOT NULL
             ), counted AS (
                 SELECT id, sender, arrival_time, delete_time FROM available
                 WHERE (message_type = 'Text' OR message_type GLOB 'RichText*')
                   AND NOT (?1 GLOB '*meeting*' AND message_type IN
                       ('RichText/Media_CallRecording', 'RichText/Media_LocalRecording',
                        'RichText/Media_CallTranscript'))
             )
             SELECT CASE WHEN (SELECT sender FROM counted ORDER BY arrival_time DESC, id DESC LIMIT 1) IS ?2 THEN 0
             ELSE (SELECT COUNT(*) FROM counted
                   WHERE delete_time IS NULL AND sender IS NOT ?2
                     AND arrival_time > COALESCE((SELECT CASE (SELECT kind FROM conversations WHERE id = ?1)
                                                  WHEN 'channel' THEN channel ELSE chat END
                                                  FROM app_read_horizon), 0)
                     AND NOT EXISTS (SELECT 1 FROM read_horizons AS horizon
                         WHERE horizon.conversation_id = ?1 AND horizon.user_mri = ?2
                           AND horizon.last_read_arrival >= counted.arrival_time))
             END",
            params![conversation_id, me],
            |row| row.get(0),
        )?;
        Ok(u64::try_from(count).unwrap_or_default())
    }

    /// The name `mri`'s newest stored message carried (`imdisplayname`), in any conversation; `None` when no stored
    /// message of theirs carried a name. A blank name is skipped.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn display_name(&self, mri: &str) -> Result<Option<String>, StoreError> {
        let name = self
            .conn
            .prepare_cached(
                "SELECT sender_name FROM messages
                 WHERE sender = ?1 AND sender_name IS NOT NULL AND trim(sender_name) <> ''
                 ORDER BY arrival_time DESC LIMIT 1",
            )?
            .query_row([mri], |row| row.get(0))
            .optional()?;
        Ok(name)
    }
}
