use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};
use serde_json::Value;

use super::activity::{self, Told};
use super::repo::advance;
use super::schema::{chat_members, chat_messages, chats};
use super::wire;
use crate::db::{Db, DbError, with_conn};

#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = chat_messages)]
pub(crate) struct Message {
    pub chat_id: String,
    pub id: i64,
    pub owner_id: String,
    pub client_id: Option<String>,
    pub version: i64,
    pub body: String,
    /// In a channel, the post the message belongs to: 0 in a draft of a new post, which is its own.
    pub root_id: Option<i64>,
}

impl Message {
    pub fn value(&self) -> Result<Value, DbError> {
        serde_json::from_str::<serde_json::Map<String, Value>>(&self.body)
            .map(Value::Object)
            .map_err(|_| super::property_repo::corrupt())
    }
}

/// The people who wrote in the reply chain of post `$root`.
macro_rules! thread_people {
    ($conn:expr, $chat:expr, $root:expr) => {
        chat_messages::table
            .filter(chat_messages::chat_id.eq($chat))
            .filter(chat_messages::root_id.eq($root))
            .select(chat_messages::owner_id)
            .distinct()
            .load($conn)
            .await?
    };
}

/// A message as posted: the people who may read it and the activity items it made, as of its transaction.
pub(crate) struct Posted {
    pub row: Message,
    /// A second post of a client message is answered with the first and tells nobody.
    pub inserted: bool,
    pub readers: Vec<String>,
    pub(super) told: Vec<Told>,
    /// It put an activity in the bots' outbox: the senders are woken after the commit.
    pub queued: bool,
    /// It put a mention in the outgoing webhooks' outbox: their senders are woken after the commit.
    pub hooked: bool,
}

/// Whether `$draft`'s owner may post it as `$poster`: a member is in its thread, an app's thread is there.
macro_rules! may_post {
    ($conn:expr, $draft:expr, $poster:expr) => {
        match $poster {
            Poster::Member => {
                diesel::select(diesel::dsl::exists(
                    chat_members::table
                        .filter(chat_members::chat_id.eq(&$draft.chat_id))
                        .filter(chat_members::user_id.eq(&$draft.owner_id))
                        .filter(chat_members::left_at.is_null()),
                ))
                .get_result($conn)
                .await?
            }
            Poster::App => {
                diesel::select(diesel::dsl::exists(chats::table.find(&$draft.chat_id)))
                    .get_result($conn)
                    .await?
            }
        }
    };
}

/// Posts `draft` for its owner, a member of its thread; `base` is the chat service's base, for an item's links.
pub(super) async fn insert(db: &Db, draft: &Message, base: &str) -> Result<Option<Posted>, DbError> {
    insert_by(db, draft, base, Poster::Member).await
}

/// Who a draft's owner is to its thread.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Poster {
    /// One of its people.
    Member,
    /// An app posting into it (the Workflows bot), which is none of its people.
    App,
}

/// Inserts a message in the caller's transaction.
macro_rules! insert_in {
    ($conn:expr, $draft:expr, $base:expr, $poster:expr, $at:expr) => {{
        let conn = &mut *$conn;
        let draft = $draft;
        let base = $base;
        let poster = $poster;
        let at = $at;
        async {
            let version: i64 = advance!(conn, at);
            let allowed: bool = may_post!(conn, draft, poster);
            if !allowed {
                return Ok(None);
            }
            if let Some(id) = &draft.client_id {
                let found = chat_messages::table
                    .filter(chat_messages::chat_id.eq(&draft.chat_id))
                    .filter(chat_messages::owner_id.eq(&draft.owner_id))
                    .filter(chat_messages::client_id.eq(id))
                    .select(Message::as_select())
                    .first(conn)
                    .await
                    .optional()?;
                if let Some(row) = found {
                    return Ok(Some(Posted {
                        row,
                        inserted: false,
                        readers: Vec::new(),
                        told: Vec::new(),
                        queued: false,
                        hooked: false,
                    }));
                }
            }
            let mut row = draft.clone();
            row.id = version;
            row.version = version;
            let mut body = row.value()?;
            if let Some(root) = row.root_id {
                let root = if root == 0 { version } else { root };
                row.root_id = Some(root);
                if let Some(link) = body["conversationLink"].as_str() {
                    body["conversationLink"] = format!("{link};messageid={root}").into();
                }
            }
            stamp(&mut row, &mut body, version);
            diesel::insert_into(chat_messages::table)
                .values(&row)
                .execute(conn)
                .await?;
            crate::search::index::index!(conn, &row.chat_id, row.id, &body);
            diesel::update(chats::table.find(&row.chat_id))
                .set(chats::version.eq(version))
                .execute(conn)
                .await?;
            let scene = activity::scene!(conn, &row.chat_id);
            let thread: Vec<String> = match row.root_id {
                Some(root) if root != row.id => thread_people!(conn, &row.chat_id, root),
                _ => Vec::new(),
            };
            let drafts = scene.of_post(&row, &body, &thread);
            let told = activity::put!(conn, at, base, &scene, &body, drafts);
            let event = crate::bots::activity::Event::Message {
                id: row.id,
                body: &body,
            };
            let queued = crate::bots::outbox::enqueue!(conn, &row.chat_id, &row.owner_id, event, version);
            let root = row.root_id.unwrap_or(row.id);
            let hooked = crate::outgoing::enqueue!(conn, &row.chat_id, &row.owner_id, row.id, root, &body, at);
            let readers = scene.readers(row.id);
            let files = body.get("properties").and_then(|properties| properties.get("files"));
            crate::media::share_on!(conn, base, &row.owner_id, files, &readers);
            Ok(Some(Posted {
                readers,
                row,
                inserted: true,
                told,
                queued,
                hooked,
            }))
        }
        .await
    }};
}
/// A connection that can insert a message in its current transaction.
pub(crate) trait MessageConnection {
    fn insert_message(
        &mut self,
        draft: &Message,
        base: &str,
        poster: Poster,
        at: i64,
    ) -> impl Future<Output = Result<Option<Posted>, DbError>> + Send;
}

macro_rules! message_connection {
    ($connection:ty) => {
        impl MessageConnection for $connection {
            async fn insert_message(
                &mut self,
                draft: &Message,
                base: &str,
                poster: Poster,
                at: i64,
            ) -> Result<Option<Posted>, DbError> {
                insert_in!(self, draft, base, poster, at)
            }
        }
    };
}
message_connection!(diesel_async::AsyncPgConnection);
message_connection!(crate::db::Sqlite);

/// Posts `draft` for its owner, `poster` to its thread: a member must be in it, an app's thread must be there.
pub(super) async fn insert_by(db: &Db, draft: &Message, base: &str, poster: Poster) -> Result<Option<Posted>, DbError> {
    let at = wire::now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| conn.insert_message(draft, base, poster, at).await)
        .await)
}

#[derive(Clone, Copy)]
pub(super) struct Window {
    pub since: i64,
    pub until: i64,
    pub before: i64,
    pub size: i64,
    pub incremental: bool,
    /// Only the messages of this post's chain.
    pub root: Option<i64>,
}

/// The messages of `$chat` that `$reader` may read, as a boxed query. The statement reads the reader's seat itself,
/// so no removal falls between a check of the seat and the read: someone who left sees no later message and no
/// later change of an earlier one, and someone with no seat sees nothing.
macro_rules! within {
    ($chat:expr, $reader:expr) => {{
        let seat = || chat_members::table.find(($chat, $reader));
        chat_messages::table
            .filter(chat_messages::chat_id.eq($chat))
            .filter(
                chat_messages::id
                    .nullable()
                    .ge(seat().select(chat_members::history_from).single_value()),
            )
            .filter(
                chat_messages::version
                    .nullable()
                    .le(seat().select(until()).single_value()),
            )
            .into_boxed()
    }};
}

/// The last version a seat's person may read: the one they left at, or any.
fn until() -> diesel::expression::SqlLiteral<diesel::sql_types::BigInt> {
    diesel::dsl::sql("COALESCE(chat_members.left_at, 9223372036854775807)")
}

pub(super) async fn page(db: &Db, chat: &str, reader: &str, window: Window) -> Result<Vec<Message>, DbError> {
    with_conn!(db, |conn| {
        let mut query = within!(chat, reader).filter(chat_messages::version.le(window.until));
        if let Some(root) = window.root {
            query = query.filter(chat_messages::root_id.eq(root));
        }
        if window.incremental {
            Ok(query
                .filter(chat_messages::version.gt(window.since))
                .filter(chat_messages::id.lt(window.before))
                .order(chat_messages::id.desc())
                .limit(window.size)
                .select(Message::as_select())
                .load(conn)
                .await?)
        } else {
            Ok(query
                .filter(chat_messages::id.lt(window.before))
                .order(chat_messages::id.desc())
                .limit(window.size)
                .select(Message::as_select())
                .load(conn)
                .await?)
        }
    })
}

/// Which of `ids` are messages of `chat` that `reader` may read and that aren't deleted.
pub(super) async fn readable(db: &Db, chat: &str, reader: &str, ids: &[i64]) -> Result<Vec<i64>, DbError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let found: Vec<Message> = with_conn!(db, |conn| within!(chat, reader)
        .filter(chat_messages::id.eq_any(ids))
        .select(Message::as_select())
        .load(conn)
        .await?);
    let mut live = Vec::with_capacity(found.len());
    for row in found {
        if row.value()?["properties"].get("deletetime").is_none() {
            live.push(row.id);
        }
    }
    Ok(live)
}

pub(super) async fn latest(db: &Db, chat: &str, reader: &str) -> Result<Option<Message>, DbError> {
    with_conn!(db, |conn| Ok(within!(chat, reader)
        .order(chat_messages::id.desc())
        .select(Message::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(super) async fn last_updated(db: &Db, chat: &str, reader: &str) -> Result<Option<(i64, i64)>, DbError> {
    with_conn!(db, |conn| Ok(within!(chat, reader)
        .order(chat_messages::version.desc())
        .select((chat_messages::id, chat_messages::version))
        .first(conn)
        .await
        .optional()?))
}

pub(super) enum Updated {
    Missing,
    Denied,
    Message(Changed),
    Unchanged(Message),
}

/// A changed message, with the people who may read it and the activity items the change made or removed.
pub(super) struct Changed {
    pub row: Message,
    pub readers: Vec<String>,
    pub told: Vec<Told>,
    /// It put an activity in the bots' outbox.
    pub queued: bool,
}

/// What a change means for the activity of the message's people.
#[derive(Clone, Copy)]
pub(super) enum Effect<'a> {
    None,
    /// The actor's reaction, by its key.
    Reacted(&'a str),
    /// The actor's reaction taken back, by its key.
    Unreacted(&'a str),
    Edited,
    Deleted,
}

/// One change of a message.
#[derive(Clone, Copy)]
pub(super) struct Change<'a> {
    pub chat: &'a str,
    pub id: i64,
    pub actor: &'a str,
    pub who: Who,
    pub effect: Effect<'a>,
    /// The chat service's base, for an item's links.
    pub base: &'a str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Who {
    Owner,
    AnyMember,
}

pub(super) async fn update<F>(db: &Db, change: Change<'_>, transform: F) -> Result<Updated, DbError>
where
    F: Fn(&mut Value, i64) -> Result<bool, DbError> + Sync,
{
    let at = wire::now();
    let Change { chat, id, actor, .. } = change;
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            let from: Option<i64> = chat_members::table
                .filter(chat_members::chat_id.eq(chat))
                .filter(chat_members::user_id.eq(actor))
                .filter(chat_members::left_at.is_null())
                .select(chat_members::history_from)
                .first(conn)
                .await
                .optional()?;
            let Some(from) = from else {
                return Ok(Updated::Denied);
            };
            let found = chat_messages::table
                .find((chat, id))
                .select(Message::as_select())
                .first(conn)
                .await
                .optional()?;
            // a message from before the history the actor was given is none to them
            let Some(mut row) = found.filter(|row: &Message| row.id >= from) else {
                return Ok(Updated::Missing);
            };
            let before = row.value()?;
            if (change.who == Who::Owner && row.owner_id != actor) || is_event(&before) {
                return Ok(Updated::Denied);
            }
            let mut body = before.clone();
            if !transform(&mut body, version)? {
                return Ok(Updated::Unchanged(row));
            }
            row.version = version;
            body["version"] = version.to_string().into();
            row.body = body.to_string();
            diesel::update(chat_messages::table.find((chat, id)))
                .set((chat_messages::version.eq(version), chat_messages::body.eq(&row.body)))
                .execute(conn)
                .await?;
            crate::search::index::index!(conn, chat, id, &body);
            diesel::update(chats::table.find(chat))
                .set(chats::version.eq(version))
                .execute(conn)
                .await?;
            let scene = activity::scene!(conn, chat);
            let plan = scene.plan(change.effect, &row, &before, &body, actor);
            let mut told = activity::amend!(conn, at, plan.gone, activity::gone);
            told.extend(activity::put!(conn, at, change.base, &scene, &body, plan.drafts));
            let reword = |item: &mut Value, at: i64| activity::reworded(item, &body, at);
            told.extend(activity::amend!(conn, at, plan.reworded, reword));
            let event = match change.effect {
                Effect::Edited => Some(crate::bots::activity::Event::Edited { id, body: &body }),
                Effect::Deleted => Some(crate::bots::activity::Event::Deleted { id }),
                Effect::Reacted(key) | Effect::Unreacted(key) => Some(crate::bots::activity::Event::Reaction {
                    id,
                    owner: &row.owner_id,
                    key,
                    added: matches!(change.effect, Effect::Reacted(_)),
                }),
                Effect::None => None,
            };
            let queued = match event {
                Some(event) => crate::bots::outbox::enqueue!(conn, chat, actor, event, version),
                None => false,
            };
            Ok(Updated::Message(Changed {
                readers: scene.readers(row.id),
                row,
                told,
                queued,
            }))
        })
        .await)
}

pub(super) async fn around(db: &Db, chat: &str, reader: &str, at: i64, size: i64) -> Result<Vec<Message>, DbError> {
    with_conn!(db, |conn| {
        let mut past = within!(chat, reader)
            .filter(chat_messages::id.le(at))
            .order(chat_messages::id.desc())
            .limit(size)
            .select(Message::as_select())
            .load::<Message>(conn)
            .await?;
        let mut future = within!(chat, reader)
            .filter(chat_messages::id.gt(at))
            .order(chat_messages::id.asc())
            .limit(size)
            .select(Message::as_select())
            .load::<Message>(conn)
            .await?;
        let size = usize::try_from(size).unwrap_or(200);
        let initial_past = size.div_ceil(2).min(past.len());
        let future_count = (size - initial_past).min(future.len());
        let past_count = (size - future_count).min(past.len());
        past.truncate(past_count);
        future.truncate(future_count);
        past.extend(future);
        past.sort_unstable_by_key(|row| std::cmp::Reverse(row.id));
        Ok(past)
    })
}

/// Gives a new message `row` with `body` its id and times (`version`), and its `clientmessageid` when it has none.
fn stamp(row: &mut Message, body: &mut Value, version: i64) {
    if row.client_id.is_none() && !is_event(body) {
        // the server's own posts (a bot's, the Workflows bot's, a call's): Teams gives every message that is not a
        // thread event a `clientmessageid`, and a read receipt names it
        let made = wire::client_message_id();
        body["clientmessageid"] = made.clone().into();
        row.client_id = Some(made);
    }
    body["id"] = version.to_string().into();
    body["version"] = version.to_string().into();
    body["sequenceId"] = version.into();
    body["composetime"] = wire::iso(version).into();
    body["originalarrivaltime"] = wire::iso(version).into();
    row.body = body.to_string();
}

/// A thread's own event (`ThreadActivity/…`): written by the service, changed by nobody.
fn is_event(body: &Value) -> bool {
    body["messagetype"]
        .as_str()
        .is_some_and(|kind| kind.starts_with("ThreadActivity/"))
}

/// Whether `post` is a post of the channel that replies can go under: there, a post itself, not deleted.
pub(super) async fn is_post(db: &Db, chat: &str, post: i64) -> Result<bool, DbError> {
    let found: Option<Message> = with_conn!(db, |conn| chat_messages::table
        .find((chat, post))
        .select(Message::as_select())
        .first(conn)
        .await
        .optional()?);
    match found {
        Some(row) if row.root_id == Some(post) => Ok(row.value()?["properties"].get("deletetime").is_none()),
        _ => Ok(false),
    }
}
