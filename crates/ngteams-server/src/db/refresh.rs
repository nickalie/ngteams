//! Refresh tokens, kept only as their SHA-256. Each is used once: redeeming it issues the next one of its family
//! (one sign-in's chain), and a used one presented again after the grace period revokes the family.

use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::schema::refresh_tokens;
use super::{Db, DbError, now, with_conn};

/// A refresh token's row (never the token itself).
#[derive(Clone, Debug, PartialEq, Eq, Queryable, Selectable, Insertable)]
#[diesel(table_name = refresh_tokens)]
pub struct RefreshToken {
    pub id: String,
    pub family_id: String,
    pub user_id: String,
    pub token_hash: String,
    pub client_id: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub used_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

/// Stores a new token's row.
///
/// # Errors
/// A database failure.
pub async fn insert(db: &Db, row: &RefreshToken) -> Result<(), DbError> {
    with_conn!(db, |conn| diesel::insert_into(refresh_tokens::table)
        .values(row)
        .execute(conn)
        .await?);
    Ok(())
}

/// The row of the token with this hash.
///
/// # Errors
/// A database failure.
pub async fn by_hash(db: &Db, token_hash: &str) -> Result<Option<RefreshToken>, DbError> {
    with_conn!(db, |conn| Ok(refresh_tokens::table
        .filter(refresh_tokens::token_hash.eq(token_hash))
        .select(RefreshToken::as_select())
        .first(conn)
        .await
        .optional()?))
}

/// Marks a token used, once: `true` for the one caller that did, `false` when it already was (another node's redeem
/// of the same token won).
///
/// # Errors
/// A database failure.
pub async fn mark_used(db: &Db, id: &str) -> Result<bool, DbError> {
    let at = now();
    let changed = with_conn!(db, |conn| diesel::update(
        refresh_tokens::table.filter(refresh_tokens::id.eq(id).and(refresh_tokens::used_at.is_null()))
    )
    .set(refresh_tokens::used_at.eq(Some(at)))
    .execute(conn)
    .await?);
    Ok(changed == 1)
}

/// Revokes every token of a family.
///
/// # Errors
/// A database failure.
pub async fn revoke_family(db: &Db, family_id: &str) -> Result<(), DbError> {
    let at = now();
    with_conn!(db, |conn| diesel::update(
        refresh_tokens::table.filter(
            refresh_tokens::family_id
                .eq(family_id)
                .and(refresh_tokens::revoked_at.is_null())
        )
    )
    .set(refresh_tokens::revoked_at.eq(Some(at)))
    .execute(conn)
    .await?);
    Ok(())
}

/// Revokes every token of an account.
///
/// # Errors
/// A database failure.
pub async fn revoke_user(db: &Db, user_id: &str) -> Result<(), DbError> {
    let at = now();
    with_conn!(db, |conn| diesel::update(
        refresh_tokens::table.filter(
            refresh_tokens::user_id
                .eq(user_id)
                .and(refresh_tokens::revoked_at.is_null())
        )
    )
    .set(refresh_tokens::revoked_at.eq(Some(at)))
    .execute(conn)
    .await?);
    Ok(())
}

/// Deletes the rows of tokens that expired before `before`.
///
/// # Errors
/// A database failure.
pub async fn purge_expired(db: &Db, before: i64) -> Result<usize, DbError> {
    with_conn!(db, |conn| Ok(diesel::delete(
        refresh_tokens::table.filter(refresh_tokens::expires_at.lt(before))
    )
    .execute(conn)
    .await?))
}
