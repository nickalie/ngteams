//! Accounts: an object id (the `oid` and MRI `8:orgid:<id>`), a UPN (kept lowercase, unique), a name, an Argon2
//! password hash, and whether the account may sign in.

use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::schema::users;
use super::{Db, DbError, conflict, now, refresh, with_conn};

/// An account as the database keeps it.
#[derive(Clone, PartialEq, Eq, Queryable, Selectable)]
#[diesel(table_name = users)]
pub struct User {
    pub id: String,
    pub upn: String,
    pub display_name: String,
    pub password_hash: String,
    pub disabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl std::fmt::Debug for User {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("User")
            .field("id", &self.id)
            .field("upn", &self.upn)
            .field("disabled", &self.disabled)
            .finish_non_exhaustive()
    }
}

/// A UPN as it is stored and looked up: trimmed and lowercase.
#[must_use]
pub fn normalize_upn(upn: &str) -> String {
    upn.trim().to_lowercase()
}

/// Adds an account with a fresh object id.
///
/// # Errors
/// [`DbError::Conflict`] when the UPN is taken; a database failure.
pub async fn add(db: &Db, upn: &str, display_name: &str, password_hash: &str) -> Result<User, DbError> {
    let at = now();
    let user = User {
        id: uuid::Uuid::new_v4().to_string(),
        upn: normalize_upn(upn),
        display_name: display_name.to_owned(),
        password_hash: password_hash.to_owned(),
        disabled: false,
        created_at: at,
        updated_at: at,
    };
    with_conn!(db, |conn| {
        diesel::insert_into(users::table)
            .values((
                users::id.eq(&user.id),
                users::upn.eq(&user.upn),
                users::display_name.eq(&user.display_name),
                users::password_hash.eq(&user.password_hash),
                users::disabled.eq(false),
                users::created_at.eq(at),
                users::updated_at.eq(at),
            ))
            .execute(conn)
            .await
            .map_err(conflict)?;
    });
    Ok(user)
}

/// The account with this UPN (any case).
///
/// # Errors
/// A database failure.
pub async fn by_upn(db: &Db, upn: &str) -> Result<Option<User>, DbError> {
    let upn = normalize_upn(upn);
    with_conn!(db, |conn| Ok(users::table
        .filter(users::upn.eq(&upn))
        .select(User::as_select())
        .first(conn)
        .await
        .optional()?))
}

/// The account with this object id.
///
/// # Errors
/// A database failure.
pub async fn by_id(db: &Db, id: &str) -> Result<Option<User>, DbError> {
    with_conn!(db, |conn| Ok(users::table
        .find(id)
        .select(User::as_select())
        .first(conn)
        .await
        .optional()?))
}

/// Every account, by UPN.
///
/// # Errors
/// A database failure.
pub async fn list(db: &Db) -> Result<Vec<User>, DbError> {
    with_conn!(db, |conn| Ok(users::table
        .order(users::upn.asc())
        .select(User::as_select())
        .load(conn)
        .await?))
}

/// Disables or enables an account by UPN. Disabling also revokes its refresh tokens, so it is signed out everywhere
/// once its access tokens run out. `false` when there is no such account.
///
/// # Errors
/// A database failure.
pub async fn set_disabled(db: &Db, upn: &str, disabled: bool) -> Result<bool, DbError> {
    let Some(id) = id_of(db, upn).await? else {
        return Ok(false);
    };
    let at = now();
    with_conn!(db, |conn| diesel::update(users::table.find(&id))
        .set((users::disabled.eq(disabled), users::updated_at.eq(at)))
        .execute(conn)
        .await?);
    if disabled {
        refresh::revoke_user(db, &id).await?;
    }
    Ok(true)
}

/// Replaces an account's password hash and revokes its refresh tokens. `false` when there is no such account.
///
/// # Errors
/// A database failure.
pub async fn set_password(db: &Db, upn: &str, password_hash: &str) -> Result<bool, DbError> {
    let Some(id) = id_of(db, upn).await? else {
        return Ok(false);
    };
    let at = now();
    with_conn!(db, |conn| diesel::update(users::table.find(&id))
        .set((users::password_hash.eq(password_hash), users::updated_at.eq(at)))
        .execute(conn)
        .await?);
    refresh::revoke_user(db, &id).await?;
    Ok(true)
}

async fn id_of(db: &Db, upn: &str) -> Result<Option<String>, DbError> {
    Ok(by_upn(db, upn).await?.map(|user| user.id))
}
