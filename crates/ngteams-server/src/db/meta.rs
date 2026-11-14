//! Server-wide values every node reads alike: the tenant.

use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::schema::server_meta;
use super::{Db, DbError, with_conn};

const TENANT: &str = "tenant_id";

/// The server's tenant id (a GUID, its accounts' `tid`): made by the first node to ask, then the same for every node.
///
/// # Errors
/// A database failure.
pub async fn tenant(db: &Db) -> Result<String, DbError> {
    let fresh = uuid::Uuid::new_v4().to_string();
    with_conn!(db, |conn| {
        diesel::insert_into(server_meta::table)
            .values((server_meta::name.eq(TENANT), server_meta::value.eq(&fresh)))
            .on_conflict_do_nothing()
            .execute(conn)
            .await?;
        Ok(server_meta::table
            .filter(server_meta::name.eq(TENANT))
            .select(server_meta::value)
            .first::<String>(conn)
            .await?)
    })
}
