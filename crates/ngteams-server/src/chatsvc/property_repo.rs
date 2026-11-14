use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};
use serde_json::{Map, Value};

use super::repo::advance;
use super::schema::{chat_members, chat_properties, chats};
use super::wire;
use crate::db::{Db, DbError, with_conn};

pub(super) fn decode(body: Option<String>) -> Result<Map<String, Value>, DbError> {
    body.map(|body| serde_json::from_str(&body).map_err(|_| corrupt()))
        .transpose()
        .map(Option::unwrap_or_default)
}

pub(super) fn corrupt() -> DbError {
    DbError::Query(diesel::result::Error::DeserializationError(
        "stored chat data is invalid".into(),
    ))
}

pub(super) async fn conversation(db: &Db, chat: &str, user: &str) -> Result<Map<String, Value>, DbError> {
    let body = with_conn!(db, |conn| chat_properties::table
        .find((chat, user))
        .select(chat_properties::body)
        .first::<String>(conn)
        .await
        .optional()?);
    decode(body)
}

pub(super) async fn set_conversation(
    db: &Db,
    chat: &str,
    user: &str,
    name: &str,
    value: &str,
) -> Result<bool, DbError> {
    let at = wire::now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            let member: bool = diesel::select(diesel::dsl::exists(
                chat_members::table
                    .filter(chat_members::chat_id.eq(chat))
                    .filter(chat_members::user_id.eq(user)),
            ))
            .get_result(conn)
            .await?;
            if !member {
                return Ok(false);
            }
            let body = chat_properties::table
                .find((chat, user))
                .select(chat_properties::body)
                .first(conn)
                .await
                .optional()?;
            let mut properties = decode(body)?;
            properties.insert(name.into(), value.into());
            let body = serde_json::to_string(&properties).map_err(|_| corrupt())?;
            diesel::insert_into(chat_properties::table)
                .values((
                    chat_properties::chat_id.eq(chat),
                    chat_properties::user_id.eq(user),
                    chat_properties::body.eq(&body),
                ))
                .on_conflict((chat_properties::chat_id, chat_properties::user_id))
                .do_update()
                .set(chat_properties::body.eq(&body))
                .execute(conn)
                .await?;
            diesel::update(chats::table.find(chat))
                .set(chats::version.eq(version))
                .execute(conn)
                .await?;
            Ok(true)
        })
        .await)
}
