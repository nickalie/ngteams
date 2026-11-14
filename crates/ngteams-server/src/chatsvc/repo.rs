use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};

use super::schema::{chat_clock, chat_members, chats};
use crate::db::schema::users;
use crate::db::{Db, DbError, with_conn};

#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = chats)]
pub(super) struct Chat {
    pub id: String,
    pub creator: String,
    pub topic: Option<String>,
    pub unique_roster: bool,
    pub created_at: i64,
    pub version: i64,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = chat_members)]
pub(super) struct Member {
    pub user_id: String,
    pub joined_at: i64,
}

macro_rules! advance {
    ($conn:expr, $at:expr) => {
        diesel::update($crate::chatsvc::schema::chat_clock::table)
            .set(
                $crate::chatsvc::schema::chat_clock::version.eq(diesel::dsl::case_when::<
                    _,
                    _,
                    diesel::sql_types::BigInt,
                >(
                    $crate::chatsvc::schema::chat_clock::version.lt($at),
                    $at,
                )
                .otherwise($crate::chatsvc::schema::chat_clock::version + 1)),
            )
            .returning($crate::chatsvc::schema::chat_clock::version)
            .get_result($conn)
            .await?
    };
}
pub(super) use advance;

pub(super) async fn create(db: &Db, chat: &Chat, members: &[String]) -> Result<bool, DbError> {
    let at = super::wire::now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            let active: i64 = users::table
                .filter(users::id.eq_any(members))
                .filter(users::disabled.eq(false))
                .count()
                .get_result(conn)
                .await?;
            if usize::try_from(active).ok() != Some(members.len()) {
                return Err(DbError::Conflict);
            }
            let mut chat = chat.clone();
            chat.version = version;
            chat.created_at = version;
            let inserted = diesel::insert_into(chats::table)
                .values(&chat)
                .on_conflict_do_nothing()
                .execute(conn)
                .await?;
            if inserted == 0 {
                return Ok(false);
            }
            for (position, user) in members.iter().enumerate() {
                diesel::insert_into(chat_members::table)
                    .values((
                        chat_members::chat_id.eq(&chat.id),
                        chat_members::user_id.eq(user),
                        chat_members::position.eq(i32::try_from(position).unwrap_or(i32::MAX)),
                        chat_members::joined_at.eq(chat.created_at),
                    ))
                    .execute(conn)
                    .await?;
            }
            Ok(true)
        })
        .await)
}

pub(super) async fn chat(db: &Db, id: &str) -> Result<Option<Chat>, DbError> {
    with_conn!(db, |conn| Ok(chats::table
        .find(id)
        .select(Chat::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(super) async fn members(db: &Db, id: &str) -> Result<Vec<Member>, DbError> {
    with_conn!(db, |conn| Ok(chat_members::table
        .filter(chat_members::chat_id.eq(id))
        .order(chat_members::position)
        .select(Member::as_select())
        .load(conn)
        .await?))
}

pub(super) async fn watermark(db: &Db) -> Result<i64, DbError> {
    with_conn!(db, |conn| Ok(chat_clock::table
        .select(chat_clock::version)
        .first(conn)
        .await?))
}

pub(super) async fn list(db: &Db, user: &str, after: i64, until: i64, limit: i64) -> Result<Vec<Chat>, DbError> {
    with_conn!(db, |conn| Ok(chats::table
        .inner_join(chat_members::table)
        .filter(chat_members::user_id.eq(user))
        .filter(chats::version.gt(after))
        .filter(chats::version.le(until))
        .order(chats::version.asc())
        .limit(limit)
        .select(Chat::as_select())
        .load(conn)
        .await?))
}

pub(super) async fn tick(db: &Db) -> Result<i64, DbError> {
    let at = super::wire::now();
    with_conn!(db, |conn| {
        let version: i64 = advance!(conn, at);
        Ok(version)
    })
}
