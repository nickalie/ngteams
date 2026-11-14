use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};
use serde_json::Value;

use super::repo::advance;
use super::schema::{chat_members, chat_messages, chats};
use super::wire;
use crate::db::{Db, DbError, with_conn};

#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = chat_messages)]
pub(super) struct Message {
    pub chat_id: String,
    pub id: i64,
    pub owner_id: String,
    pub client_id: Option<String>,
    pub version: i64,
    pub body: String,
}

impl Message {
    pub fn value(&self) -> Result<Value, DbError> {
        serde_json::from_str(&self.body)
            .map_err(|error| DbError::Query(diesel::result::Error::DeserializationError(Box::new(error))))
    }
}

pub(super) async fn insert(db: &Db, draft: &Message) -> Result<Option<(Message, bool)>, DbError> {
    let at = wire::now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            let member: bool = diesel::select(diesel::dsl::exists(
                chat_members::table
                    .filter(chat_members::chat_id.eq(&draft.chat_id))
                    .filter(chat_members::user_id.eq(&draft.owner_id)),
            ))
            .get_result(conn)
            .await?;
            if !member {
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
                if let Some(found) = found {
                    return Ok(Some((found, false)));
                }
            }
            let mut row = draft.clone();
            row.id = version;
            row.version = version;
            let mut body = row.value()?;
            body["id"] = version.to_string().into();
            body["version"] = version.to_string().into();
            body["sequenceId"] = version.into();
            body["composetime"] = wire::iso(version).into();
            body["originalarrivaltime"] = wire::iso(version).into();
            row.body = body.to_string();
            diesel::insert_into(chat_messages::table)
                .values(&row)
                .execute(conn)
                .await?;
            diesel::update(chats::table.find(&row.chat_id))
                .set(chats::version.eq(version))
                .execute(conn)
                .await?;
            Ok(Some((row, true)))
        })
        .await)
}

#[derive(Clone, Copy)]
pub(super) struct Window {
    pub since: i64,
    pub until: i64,
    pub before: i64,
    pub size: i64,
    pub incremental: bool,
}

pub(super) async fn page(db: &Db, chat: &str, window: Window) -> Result<Vec<Message>, DbError> {
    with_conn!(db, |conn| {
        let query = chat_messages::table
            .filter(chat_messages::chat_id.eq(chat))
            .filter(chat_messages::version.le(window.until));
        if window.incremental {
            Ok(query
                .filter(chat_messages::version.gt(window.since))
                .order(chat_messages::version.asc())
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

pub(super) async fn latest(db: &Db, chat: &str) -> Result<Option<Message>, DbError> {
    with_conn!(db, |conn| Ok(chat_messages::table
        .filter(chat_messages::chat_id.eq(chat))
        .order(chat_messages::version.desc())
        .select(Message::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(super) enum Updated {
    Missing,
    Denied,
    Message(Message),
}

pub(super) async fn update<F>(
    db: &Db,
    chat: &str,
    id: i64,
    actor: &str,
    owner_only: bool,
    transform: F,
) -> Result<Updated, DbError>
where
    F: Fn(&mut Value, i64) -> Result<(), DbError> + Sync,
{
    let at = wire::now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            let member: bool = diesel::select(diesel::dsl::exists(
                chat_members::table
                    .filter(chat_members::chat_id.eq(chat))
                    .filter(chat_members::user_id.eq(actor)),
            ))
            .get_result(conn)
            .await?;
            if !member {
                return Ok(Updated::Denied);
            }
            let Some(mut row) = chat_messages::table
                .find((chat, id))
                .select(Message::as_select())
                .first(conn)
                .await
                .optional()?
            else {
                return Ok(Updated::Missing);
            };
            if owner_only && row.owner_id != actor {
                return Ok(Updated::Denied);
            }
            let mut body = row.value()?;
            transform(&mut body, version)?;
            row.version = version;
            body["version"] = version.to_string().into();
            row.body = body.to_string();
            diesel::update(chat_messages::table.find((chat, id)))
                .set((chat_messages::version.eq(version), chat_messages::body.eq(&row.body)))
                .execute(conn)
                .await?;
            diesel::update(chats::table.find(chat))
                .set(chats::version.eq(version))
                .execute(conn)
                .await?;
            Ok(Updated::Message(row))
        })
        .await)
}
