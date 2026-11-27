//! The sign-ins under way and the people's tokens, as the database keeps them. Every token and verifier here is
//! sealed ([`super::seal`]); a code is kept only as its digest.

use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};

use super::schema::{bot_connections, bot_sign_ins, bot_user_tokens};
use crate::db::{Db, DbError, with_conn};
use super::now_ms as now;

/// How many wrong codes end a sign-in.
pub(super) const MAX_ATTEMPTS: i32 = 5;

/// A sign-in under way: made with its link, given its code and the provider's token (sealed) once the person
/// signed in.
#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = bot_sign_ins)]
pub(super) struct SignIn {
    pub id: String,
    pub bot_id: String,
    pub connection: String,
    pub user_id: String,
    pub verifier: Vec<u8>,
    pub code_hash: Option<String>,
    pub token: Option<Vec<u8>>,
    pub attempts: i32,
    pub expires_at: i64,
    pub created_at: i64,
}

/// Whose token, for which bot and connection.
#[derive(Clone, Copy, Debug)]
pub(super) struct Owner<'a> {
    pub bot: &'a str,
    pub connection: &'a str,
    pub user: &'a str,
}

/// Keeps a new sign-in, and drops the owner's sign-ins that ran out.
pub(super) async fn begin(db: &Db, row: &SignIn) -> Result<(), DbError> {
    let at = now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            diesel::delete(
                bot_sign_ins::table
                    .filter(bot_sign_ins::bot_id.eq(&row.bot_id))
                    .filter(bot_sign_ins::user_id.eq(&row.user_id))
                    .filter(bot_sign_ins::expires_at.le(at)),
            )
            .execute(conn)
            .await?;
            diesel::insert_into(bot_sign_ins::table)
                .values(row)
                .execute(conn)
                .await?;
            Ok(())
        })
        .await)
}

/// The sign-in `id`, while it runs.
pub(super) async fn sign_in(db: &Db, id: &str) -> Result<Option<SignIn>, DbError> {
    let at = now();
    with_conn!(db, |conn| Ok(bot_sign_ins::table
        .find(id)
        .filter(bot_sign_ins::expires_at.gt(at))
        .select(SignIn::as_select())
        .first(conn)
        .await
        .optional()?))
}

/// Gives the sign-in `id` its code and token, once: whether it took them.
pub(super) async fn signed_in(db: &Db, id: &str, code_hash: &str, token: &[u8]) -> Result<bool, DbError> {
    let updated = with_conn!(db, |conn| diesel::update(
        bot_sign_ins::table
            .find(id)
            .filter(bot_sign_ins::token.is_null())
    )
    .set((bot_sign_ins::code_hash.eq(code_hash), bot_sign_ins::token.eq(token)))
    .execute(conn)
    .await)?;
    Ok(updated == 1)
}

/// The owner's sign-ins that have a code and still run, newest first.
pub(super) async fn with_codes(db: &Db, owner: Owner<'_>) -> Result<Vec<SignIn>, DbError> {
    let at = now();
    with_conn!(db, |conn| Ok(bot_sign_ins::table
        .filter(bot_sign_ins::bot_id.eq(owner.bot))
        .filter(bot_sign_ins::connection.eq(owner.connection))
        .filter(bot_sign_ins::user_id.eq(owner.user))
        .filter(bot_sign_ins::code_hash.is_not_null())
        .filter(bot_sign_ins::expires_at.gt(at))
        .order(bot_sign_ins::created_at.desc())
        .select(SignIn::as_select())
        .load(conn)
        .await?))
}

/// A wrong code for the owner: each of their sign-ins with a code counts it, and one that reached
/// [`MAX_ATTEMPTS`] ends.
pub(super) async fn missed(db: &Db, owner: Owner<'_>) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let mine = bot_sign_ins::table
                .filter(bot_sign_ins::bot_id.eq(owner.bot))
                .filter(bot_sign_ins::connection.eq(owner.connection))
                .filter(bot_sign_ins::user_id.eq(owner.user))
                .filter(bot_sign_ins::code_hash.is_not_null());
            diesel::update(mine)
                .set(bot_sign_ins::attempts.eq(bot_sign_ins::attempts + 1))
                .execute(conn)
                .await?;
            diesel::delete(mine.filter(bot_sign_ins::attempts.ge(MAX_ATTEMPTS)))
                .execute(conn)
                .await?;
            Ok(())
        })
        .await)
}

macro_rules! put {
    ($conn:ident, $owner:expr, $token:expr) => {{
        let owner = $owner;
        diesel::delete(bot_user_tokens::table.find((owner.bot, owner.connection, owner.user)))
            .execute($conn)
            .await?;
        diesel::insert_into(bot_user_tokens::table)
            .values((
                bot_user_tokens::bot_id.eq(owner.bot),
                bot_user_tokens::connection.eq(owner.connection),
                bot_user_tokens::user_id.eq(owner.user),
                bot_user_tokens::token.eq($token),
                bot_user_tokens::updated_at.eq(now()),
            ))
            .execute($conn)
            .await?;
    }};
}

/// The sign-in `id` ends and its token, sealed for the owner, becomes theirs: whether it was still there.
pub(super) async fn redeem(db: &Db, id: &str, owner: Owner<'_>, token: &[u8]) -> Result<bool, DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let ended = diesel::delete(bot_sign_ins::table.find(id)).execute(conn).await?;
            if ended == 0 {
                return Ok(false);
            }
            put!(conn, owner, token);
            Ok(true)
        })
        .await)
}


/// Keeps `token` (sealed) as the owner's, in place of any they had.
pub(super) async fn keep(db: &Db, owner: Owner<'_>, token: &[u8]) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            put!(conn, owner, token);
            Ok(())
        })
        .await)
}

/// The owner's token, sealed.
pub(super) async fn token(db: &Db, owner: Owner<'_>) -> Result<Option<Vec<u8>>, DbError> {
    with_conn!(db, |conn| Ok(bot_user_tokens::table
        .find((owner.bot, owner.connection, owner.user))
        .select(bot_user_tokens::token)
        .first(conn)
        .await
        .optional()?))
}

/// Signs `user` out of `bot`'s `connection`, or of all its connections: their tokens and sign-ins go.
pub(super) async fn sign_out(db: &Db, bot: &str, connection: Option<&str>, user: &str) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let mut tokens = diesel::delete(bot_user_tokens::table)
                .filter(bot_user_tokens::bot_id.eq(bot))
                .filter(bot_user_tokens::user_id.eq(user))
                .into_boxed();
            let mut sign_ins = diesel::delete(bot_sign_ins::table)
                .filter(bot_sign_ins::bot_id.eq(bot))
                .filter(bot_sign_ins::user_id.eq(user))
                .into_boxed();
            if let Some(connection) = connection {
                tokens = tokens.filter(bot_user_tokens::connection.eq(connection));
                sign_ins = sign_ins.filter(bot_sign_ins::connection.eq(connection));
            }
            tokens.execute(conn).await?;
            sign_ins.execute(conn).await?;
            Ok(())
        })
        .await)
}

/// Each of `bot`'s connections by name, and whether `user` has a token for it.
pub(super) async fn statuses(db: &Db, bot: &str, user: &str) -> Result<Vec<(String, bool)>, DbError> {
    let names: Vec<String> = with_conn!(db, |conn| bot_connections::table
        .filter(bot_connections::bot_id.eq(bot))
        .order(bot_connections::name)
        .select(bot_connections::name)
        .load(conn)
        .await)?;
    let held: Vec<String> = with_conn!(db, |conn| bot_user_tokens::table
        .filter(bot_user_tokens::bot_id.eq(bot))
        .filter(bot_user_tokens::user_id.eq(user))
        .select(bot_user_tokens::connection)
        .load(conn)
        .await)?;
    Ok(names
        .into_iter()
        .map(|name| {
            let has = held.contains(&name);
            (name, has)
        })
        .collect())
}

/// Drops the sign-ins that ran out before `before`.
pub(super) async fn purge(db: &Db, before: i64) -> Result<(), DbError> {
    with_conn!(db, |conn| diesel::delete(bot_sign_ins::table.filter(bot_sign_ins::expires_at.le(before)))
        .execute(conn)
        .await)?;
    Ok(())
}
