//! The sign-ins under way and the people's tokens, as the database keeps them. Every token and verifier here is
//! sealed ([`super::seal`]); a code is kept only as its digest.

use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};

use super::connections::Connection;
use super::now_ms as now;
use super::schema::{bot_connections, bot_sign_ins, bot_user_tokens};
use crate::db::{Db, DbError, with_conn};

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

/// The sign-in `id` and its connection, read together, while it runs and has no token yet.
pub(super) async fn running(db: &Db, id: &str) -> Result<Option<(SignIn, Connection)>, DbError> {
    let at = now();
    with_conn!(db, |conn| Ok(bot_sign_ins::table
        .inner_join(
            bot_connections::table.on(bot_connections::bot_id
                .eq(bot_sign_ins::bot_id)
                .and(bot_connections::name.eq(bot_sign_ins::connection))),
        )
        .filter(bot_sign_ins::id.eq(id))
        .filter(bot_sign_ins::expires_at.gt(at))
        .filter(bot_sign_ins::token.is_null())
        .select((SignIn::as_select(), Connection::as_select()))
        .first(conn)
        .await
        .optional()?))
}

/// Gives the sign-in `id` its code and token, once, and a full lifetime from now for the code: whether it took them.
pub(super) async fn signed_in(db: &Db, id: &str, code_hash: &str, token: &[u8]) -> Result<bool, DbError> {
    let updated = with_conn!(db, |conn| diesel::update(
        bot_sign_ins::table.find(id).filter(bot_sign_ins::token.is_null())
    )
    .set((
        bot_sign_ins::code_hash.eq(code_hash),
        bot_sign_ins::token.eq(token),
        bot_sign_ins::expires_at.eq(now() + super::SIGN_IN_LIFETIME_MS),
    ))
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
        let at = now();
        diesel::insert_into(bot_user_tokens::table)
            .values((
                bot_user_tokens::bot_id.eq(owner.bot),
                bot_user_tokens::connection.eq(owner.connection),
                bot_user_tokens::user_id.eq(owner.user),
                bot_user_tokens::token.eq($token),
                bot_user_tokens::updated_at.eq(at),
            ))
            .on_conflict((
                bot_user_tokens::bot_id,
                bot_user_tokens::connection,
                bot_user_tokens::user_id,
            ))
            .do_update()
            .set((bot_user_tokens::token.eq($token), bot_user_tokens::updated_at.eq(at)))
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

/// Keeps the renewed `token` (sealed) in place of the owner's token `read`, unless a sign-out, a refusal, a change
/// of provider or another renewal changed it meanwhile: whether it was kept.
pub(super) async fn keep(db: &Db, owner: Owner<'_>, read: &[u8], token: &[u8]) -> Result<bool, DbError> {
    let updated = with_conn!(db, |conn| diesel::update(
        bot_user_tokens::table
            .find((owner.bot, owner.connection, owner.user))
            .filter(bot_user_tokens::token.eq(read))
    )
    .set((bot_user_tokens::token.eq(token), bot_user_tokens::updated_at.eq(now())))
    .execute(conn)
    .await)?;
    Ok(updated == 1)
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

/// The provider refused to renew the owner's token `read` (sealed): signs them out of the connection, unless another
/// request kept a new token meanwhile. Whether they were signed out.
pub(super) async fn refused(db: &Db, owner: Owner<'_>, read: &[u8]) -> Result<bool, DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let dropped = diesel::delete(
                bot_user_tokens::table
                    .find((owner.bot, owner.connection, owner.user))
                    .filter(bot_user_tokens::token.eq(read)),
            )
            .execute(conn)
            .await?;
            if dropped == 0 {
                return Ok(false);
            }
            diesel::delete(
                bot_sign_ins::table
                    .filter(bot_sign_ins::bot_id.eq(owner.bot))
                    .filter(bot_sign_ins::connection.eq(owner.connection))
                    .filter(bot_sign_ins::user_id.eq(owner.user)),
            )
            .execute(conn)
            .await?;
            Ok(true)
        })
        .await)
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
    with_conn!(db, |conn| diesel::delete(
        bot_sign_ins::table.filter(bot_sign_ins::expires_at.le(before))
    )
    .execute(conn)
    .await)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use diesel_async::SimpleAsyncConnection as _;

    use super::*;

    const OWNER: Owner<'static> = Owner {
        bot: "b",
        connection: "github",
        user: "ada",
    };

    fn sign_in(id: &str) -> SignIn {
        SignIn {
            id: id.to_owned(),
            bot_id: OWNER.bot.to_owned(),
            connection: OWNER.connection.to_owned(),
            user_id: OWNER.user.to_owned(),
            verifier: Vec::new(),
            code_hash: None,
            token: None,
            attempts: 0,
            expires_at: now() + 60_000,
            created_at: now(),
        }
    }

    async fn sign_ins(db: &Db) -> Result<i64, DbError> {
        with_conn!(db, |conn| Ok(bot_sign_ins::table.count().get_result(conn).await?))
    }

    #[tokio::test]
    async fn a_renewal_or_refusal_changes_only_the_token_it_read() -> Result<(), DbError> {
        let dir = tempfile::tempdir().map_err(|error| DbError::Pool(error.to_string()))?;
        let db = Db::connect(&format!("sqlite://{}", dir.path().join("server.sqlite").display()), 1).await?;
        // the owner's bot and connection aren't registered: this tests only the store's own conditions
        with_conn!(db, |conn| conn.batch_execute("PRAGMA foreign_keys = OFF").await?);
        begin(&db, &sign_in("first")).await?;
        assert!(redeem(&db, "first", OWNER, b"x").await?);
        begin(&db, &sign_in("second")).await?;
        assert!(redeem(&db, "second", OWNER, b"y").await?);
        assert_eq!(token(&db, OWNER).await?.as_deref(), Some(&b"y"[..]));

        assert!(
            !keep(&db, OWNER, b"x", b"z").await?,
            "a renewal of a replaced token keeps nothing"
        );
        begin(&db, &sign_in("third")).await?;
        assert!(
            !refused(&db, OWNER, b"x").await?,
            "a refusal of a replaced token signs no one out"
        );
        assert_eq!(token(&db, OWNER).await?.as_deref(), Some(&b"y"[..]));
        assert_eq!(sign_ins(&db).await?, 1);

        assert!(keep(&db, OWNER, b"y", b"z").await?);
        assert_eq!(token(&db, OWNER).await?.as_deref(), Some(&b"z"[..]));
        assert!(refused(&db, OWNER, b"z").await?);
        assert_eq!(token(&db, OWNER).await?, None);
        assert_eq!(sign_ins(&db).await?, 0, "a refusal ends the owner's sign-ins");
        assert!(
            !keep(&db, OWNER, b"z", b"w").await?,
            "a renewal after a sign-out keeps nothing"
        );
        assert_eq!(token(&db, OWNER).await?, None);
        Ok(())
    }
}
