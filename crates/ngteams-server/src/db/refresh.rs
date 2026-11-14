//! Refresh tokens, kept only as their SHA-256. Each is used once: redeeming it issues the next one of its family
//! (one sign-in's chain), and a used one presented again after the grace period revokes the family.

use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};

use super::schema::{refresh_tokens, users};
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

/// Whether a refresh token was stored, or why the sign-in became invalid before insertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertOutcome {
    Stored,
    AccountDisabled,
    PasswordChanged,
    FamilyRevoked,
}

/// Stores a token only while its account and family remain active. Password grants also recheck the verified hash.
///
/// # Errors
/// A database failure.
pub async fn insert(db: &Db, row: &RefreshToken, verified_hash: Option<&str>) -> Result<InsertOutcome, DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            // Inserts and both revocation paths lock the account before looking at its family.
            diesel::update(users::table.find(&row.user_id))
                .set(users::updated_at.eq(users::updated_at))
                .execute(conn)
                .await?;
            let account = users::table
                .find(&row.user_id)
                .select((users::disabled, users::password_hash))
                .first::<(bool, String)>(conn)
                .await
                .optional()?;
            let Some((false, password_hash)) = account else {
                return Ok(InsertOutcome::AccountDisabled);
            };
            if verified_hash.is_some_and(|verified| verified != password_hash) {
                return Ok(InsertOutcome::PasswordChanged);
            }
            let revoked = diesel::select(diesel::dsl::exists(
                refresh_tokens::table.filter(
                    refresh_tokens::family_id
                        .eq(&row.family_id)
                        .and(refresh_tokens::revoked_at.is_not_null()),
                ),
            ))
            .get_result::<bool>(conn)
            .await?;
            if revoked {
                return Ok(InsertOutcome::FamilyRevoked);
            }
            diesel::insert_into(refresh_tokens::table)
                .values(row)
                .execute(conn)
                .await?;
            Ok(InsertOutcome::Stored)
        })
        .await)
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
    let user_id = with_conn!(db, |conn| refresh_tokens::table
        .filter(refresh_tokens::family_id.eq(family_id))
        .select(refresh_tokens::user_id)
        .first::<String>(conn)
        .await
        .optional()?);
    if let Some(user_id) = user_id {
        revoke(db, &user_id, Some(family_id)).await?;
    }
    Ok(())
}

/// Revokes every token of an account.
///
/// # Errors
/// A database failure.
pub async fn revoke_user(db: &Db, user_id: &str) -> Result<(), DbError> {
    revoke(db, user_id, None).await
}

async fn revoke(db: &Db, user_id: &str, family_id: Option<&str>) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            diesel::update(users::table.find(user_id))
                .set(users::updated_at.eq(users::updated_at))
                .execute(conn)
                .await?;
            let tokens = refresh_tokens::table
                .filter(refresh_tokens::user_id.eq(user_id))
                .filter(refresh_tokens::revoked_at.is_null());
            let revoked = refresh_tokens::revoked_at.eq(Some(now()));
            if let Some(family_id) = family_id {
                diesel::update(tokens.filter(refresh_tokens::family_id.eq(family_id)))
                    .set(revoked)
                    .execute(conn)
                    .await?;
            } else {
                diesel::update(tokens).set(revoked).execute(conn).await?;
            }
            Ok(())
        })
        .await)
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
