//! Token signing keys (Ed25519): the private key as PKCS#8, the public key raw. The newest unretired key signs; every
//! key verifies until it is deleted, which happens once nothing it signed can still be valid.

use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};

use super::schema::{server_meta, signing_keys};
use super::{Db, DbError, with_conn};

#[derive(Clone, PartialEq, Eq, Queryable, Selectable, Insertable)]
#[diesel(table_name = signing_keys)]
pub struct SigningKey {
    pub kid: String,
    pub private_der: Vec<u8>,
    pub public_der: Vec<u8>,
    pub created_at: i64,
    pub retired_at: Option<i64>,
}

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningKey")
            .field("kid", &self.kid)
            .field("created_at", &self.created_at)
            .field("retired_at", &self.retired_at)
            .finish_non_exhaustive()
    }
}

/// Every key, newest first.
///
/// # Errors
/// A database failure.
pub async fn all(db: &Db) -> Result<Vec<SigningKey>, DbError> {
    with_conn!(db, |conn| Ok(signing_keys::table
        .order((signing_keys::created_at.desc(), signing_keys::kid.desc()))
        .select(SigningKey::as_select())
        .load(conn)
        .await?))
}

/// Stores the new signer and retires the previous ones atomically across nodes.
///
/// # Errors
/// A database failure.
pub async fn rotate(db: &Db, key: &SigningKey) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            diesel::insert_into(server_meta::table)
                .values((server_meta::name.eq("signing_key_lock"), server_meta::value.eq("")))
                .on_conflict_do_nothing()
                .execute(conn)
                .await?;
            // Every node locks the same row before it replaces the signer.
            diesel::update(server_meta::table.find("signing_key_lock"))
                .set(server_meta::value.eq(server_meta::value))
                .execute(conn)
                .await?;
            diesel::insert_into(signing_keys::table)
                .values(key)
                .execute(conn)
                .await?;
            diesel::update(
                signing_keys::table.filter(signing_keys::kid.ne(&key.kid).and(signing_keys::retired_at.is_null())),
            )
            .set(signing_keys::retired_at.eq(Some(key.created_at)))
            .execute(conn)
            .await?;
            Ok(())
        })
        .await)
}

/// Deletes the keys retired before `before`.
///
/// # Errors
/// A database failure.
pub async fn delete_retired(db: &Db, before: i64) -> Result<usize, DbError> {
    with_conn!(db, |conn| Ok(diesel::delete(
        signing_keys::table.filter(signing_keys::retired_at.lt(before))
    )
    .execute(conn)
    .await?))
}
