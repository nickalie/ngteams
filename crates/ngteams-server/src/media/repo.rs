use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};

use super::schema::{drive_grants, drive_items, media_object_readers, media_objects, media_usage};
use crate::chatsvc::schema::chat_messages;
use crate::db::{Db, DbError, conflict, with_conn};

/// Adds `$bytes` to what `$user` has stored when the sum stays within `$quota` (0: no limit; taking bytes away
/// always passes); whether it did. The row is updated in one statement, so two nodes can't both pass the limit.
macro_rules! charge {
    ($conn:expr, $user:expr, $bytes:expr, $quota:expr) => {{
        diesel::insert_into(media_usage::table)
            .values((media_usage::user_id.eq($user), media_usage::bytes.eq(0)))
            .on_conflict_do_nothing()
            .execute($conn)
            .await?;
        let limit = if $quota == 0 || $bytes <= 0 { i64::MAX } else { $quota };
        let changed = diesel::update(media_usage::table.find($user))
            .filter(media_usage::bytes.le(limit.saturating_sub($bytes.max(0))))
            .set(media_usage::bytes.eq(media_usage::bytes + $bytes))
            .execute($conn)
            .await?;
        changed == 1
    }};
}
pub(super) use charge;

#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = media_objects)]
pub(super) struct Object {
    pub id: String,
    pub owner_id: String,
    pub kind: String,
    pub filename: String,
    pub content_type: Option<String>,
    /// The version of its bytes the object reads from: each fill stores its own.
    pub blob: Option<String>,
    pub size: i64,
    pub created_at: i64,
}

#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = drive_items)]
pub(super) struct Item {
    pub id: String,
    pub owner_id: String,
    pub name: String,
    pub size: i64,
    pub created_at: i64,
}

pub(super) async fn create_object(db: &Db, object: &Object, chats: &[String]) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            diesel::insert_into(media_objects::table)
                .values(object)
                .execute(conn)
                .await?;
            for chat in chats {
                diesel::insert_into(media_object_readers::table)
                    .values((
                        media_object_readers::object_id.eq(&object.id),
                        media_object_readers::chat_id.eq(chat),
                    ))
                    .execute(conn)
                    .await?;
            }
            Ok(())
        })
        .await)
}

pub(super) async fn object(db: &Db, id: &str) -> Result<Option<Object>, DbError> {
    with_conn!(db, |conn| Ok(media_objects::table
        .find(id)
        .select(Object::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(super) async fn object_chats(db: &Db, id: &str) -> Result<Vec<String>, DbError> {
    with_conn!(db, |conn| Ok(media_object_readers::table
        .filter(media_object_readers::object_id.eq(id))
        .select(media_object_readers::chat_id)
        .load(conn)
        .await?))
}

/// What a fill took the place of.
pub(super) struct Replaced {
    /// The version of the bytes the object read from before, now read by nothing.
    pub blob: Option<String>,
}

/// Makes `object` hold the stored bytes `blob` (`content_type`, `size`), when its owner's stored bytes stay
/// within `quota` (0: no limit): what they replace, or `None` when they wouldn't. The version, the type, the
/// size and the owner's count change in one transaction that holds the row, so fills of one object at once leave it
/// describing one fill's bytes, counted once.
pub(super) async fn fill_object(
    db: &Db,
    object: &Object,
    blob: &str,
    content_type: &str,
    size: i64,
    quota: i64,
) -> Result<Option<Replaced>, DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            // changes nothing, but holds the row: another fill reads it only after this one ends
            diesel::update(media_objects::table.find(&object.id))
                .set(media_objects::size.eq(media_objects::size))
                .execute(conn)
                .await?;
            let (held_blob, held_size): (Option<String>, i64) = media_objects::table
                .find(&object.id)
                .select((media_objects::blob, media_objects::size))
                .first(conn)
                .await?;
            if !charge!(conn, &object.owner_id, size - held_size, quota) {
                return Ok(None);
            }
            diesel::update(media_objects::table.find(&object.id))
                .set((
                    media_objects::blob.eq(blob),
                    media_objects::content_type.eq(content_type),
                    media_objects::size.eq(size),
                ))
                .execute(conn)
                .await?;
            Ok(Some(Replaced { blob: held_blob }))
        })
        .await)
}

/// If a message in a reader chat names `$id`, under the lock that message writes hold.
macro_rules! shown {
    ($conn:expr, $id:expr) => {{
        let chats: Vec<String> = media_object_readers::table
            .filter(media_object_readers::object_id.eq($id))
            .select(media_object_readers::chat_id)
            .load($conn)
            .await?;
        let named = format!("%{}%", $id);
        diesel::select(diesel::dsl::exists(
            chat_messages::table
                .filter(chat_messages::chat_id.eq_any(chats))
                .filter(chat_messages::body.like(named)),
        ))
        .get_result::<bool>($conn)
        .await?
    }};
}

/// Removes `owner`'s object and takes its bytes off the owner's count: the version it read from (`None` inside:
/// it was never filled), or `None` when the owner has no such object. The row is held as a fill holds it, so the
/// size taken off is the one counted.
pub(super) async fn remove_object(db: &Db, owner: &str, id: &str) -> Result<Option<Option<String>>, DbError> {
    remove_object_if(db, owner, id, false).await
}

pub(super) async fn release_object(db: &Db, owner: &str, id: &str) -> Result<Option<Option<String>>, DbError> {
    remove_object_if(db, owner, id, true).await
}

async fn remove_object_if(db: &Db, owner: &str, id: &str, unshown: bool) -> Result<Option<Option<String>>, DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            use crate::chatsvc::schema::chat_clock;
            // Message writes hold this row, so cleanup cannot race their reference checks.
            diesel::update(chat_clock::table)
                .set(chat_clock::version.eq(chat_clock::version))
                .execute(conn)
                .await?;
            if unshown && shown!(conn, id) {
                return Ok(None);
            }
            let own = media_objects::table
                .filter(media_objects::id.eq(id))
                .filter(media_objects::owner_id.eq(owner));
            if diesel::update(own)
                .set(media_objects::size.eq(media_objects::size))
                .execute(conn)
                .await?
                == 0
            {
                return Ok(None);
            }
            let (blob, size): (Option<String>, i64) = media_objects::table
                .find(id)
                .select((media_objects::blob, media_objects::size))
                .first(conn)
                .await?;
            diesel::delete(media_object_readers::table.filter(media_object_readers::object_id.eq(id)))
                .execute(conn)
                .await?;
            diesel::delete(media_objects::table.find(id)).execute(conn).await?;
            let freed = -size;
            let _ = charge!(conn, owner, freed, 0);
            Ok(Some(blob))
        })
        .await)
}

/// If each bot picture still exists for the chat, under the lock that cleanup holds.
macro_rules! pictures_exist {
    ($conn:expr, $owner:expr, $chat:expr, $body:expr) => {{
        use $crate::media::schema::{media_object_readers, media_objects};
        let pictures: std::collections::BTreeSet<&str> = $body["amsreferences"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        pictures.is_empty() || {
            let expected = i64::try_from(pictures.len()).unwrap_or(i64::MAX);
            let owned: i64 = media_objects::table
                .filter(media_objects::id.eq_any(&pictures))
                .filter(media_objects::owner_id.eq($owner))
                .filter(media_objects::blob.is_not_null())
                .count()
                .get_result($conn)
                .await?;
            let readable: i64 = media_object_readers::table
                .filter(media_object_readers::object_id.eq_any(&pictures))
                .filter(media_object_readers::chat_id.eq($chat))
                .count()
                .get_result($conn)
                .await?;
            owned == expected && readable == expected
        }
    }};
}
pub(crate) use pictures_exist;

pub(super) async fn objects(db: &Db, owner: &str) -> Result<Vec<Object>, DbError> {
    with_conn!(db, |conn| Ok(media_objects::table
        .filter(media_objects::owner_id.eq(owner))
        .order((media_objects::created_at.asc(), media_objects::id.asc()))
        .select(Object::as_select())
        .load(conn)
        .await?))
}

/// The bytes `user` has stored.
pub(super) async fn used(db: &Db, user: &str) -> Result<i64, DbError> {
    with_conn!(db, |conn| Ok(media_usage::table
        .find(user)
        .select(media_usage::bytes)
        .first(conn)
        .await
        .optional()?
        .unwrap_or(0)))
}

/// Adds a file to its owner's drive, readable by the owner alone, when the owner's stored bytes stay within `quota`.
/// `Ok(None)` when they wouldn't; [`DbError::Conflict`] when the owner has a file of that name.
pub(super) async fn add_item(db: &Db, item: &Item, quota: i64) -> Result<Option<()>, DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            if !charge!(conn, &item.owner_id, item.size, quota) {
                return Ok(None);
            }
            diesel::insert_into(drive_items::table)
                .values(item)
                .execute(conn)
                .await
                .map_err(conflict)?;
            diesel::insert_into(drive_grants::table)
                .values((
                    drive_grants::item_id.eq(&item.id),
                    drive_grants::user_id.eq(&item.owner_id),
                ))
                .execute(conn)
                .await?;
            Ok(Some(()))
        })
        .await)
}

/// Removes `owner`'s file with its grants and takes its bytes off the owner's count; whether it was there. Of two
/// removals at once, one finds it.
pub(super) async fn remove_item(db: &Db, owner: &str, id: &str) -> Result<bool, DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let own = drive_items::table
                .filter(drive_items::id.eq(id))
                .filter(drive_items::owner_id.eq(owner));
            let Some(size) = own.select(drive_items::size).first::<i64>(conn).await.optional()? else {
                return Ok(false);
            };
            diesel::delete(drive_grants::table.filter(drive_grants::item_id.eq(id)))
                .execute(conn)
                .await?;
            if diesel::delete(own).execute(conn).await? == 0 {
                return Ok(false);
            }
            let freed = -size;
            let _ = charge!(conn, owner, freed, 0);
            Ok(true)
        })
        .await)
}

pub(super) async fn items(db: &Db, owner: &str) -> Result<Vec<Item>, DbError> {
    with_conn!(db, |conn| Ok(drive_items::table
        .filter(drive_items::owner_id.eq(owner))
        .order((drive_items::created_at.asc(), drive_items::name.asc()))
        .select(Item::as_select())
        .load(conn)
        .await?))
}

pub(super) async fn item(db: &Db, id: &str) -> Result<Option<Item>, DbError> {
    with_conn!(db, |conn| Ok(drive_items::table
        .find(id)
        .select(Item::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(super) async fn item_named(db: &Db, owner: &str, name: &str) -> Result<Option<Item>, DbError> {
    with_conn!(db, |conn| Ok(drive_items::table
        .filter(drive_items::owner_id.eq(owner))
        .filter(drive_items::name.eq(name))
        .select(Item::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(super) async fn has_items(db: &Db, owner: &str) -> Result<bool, DbError> {
    with_conn!(db, |conn| Ok(diesel::select(diesel::dsl::exists(
        drive_items::table.filter(drive_items::owner_id.eq(owner))
    ))
    .get_result(conn)
    .await?))
}

/// The accounts a file is shared with, its owner among them, by id.
pub(super) async fn grants(db: &Db, item: &str) -> Result<Vec<String>, DbError> {
    with_conn!(db, |conn| Ok(drive_grants::table
        .filter(drive_grants::item_id.eq(item))
        .order(drive_grants::user_id.asc())
        .select(drive_grants::user_id)
        .load(conn)
        .await?))
}

pub(super) async fn grant(db: &Db, item: &str, users: &[String]) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            for user in users {
                diesel::insert_into(drive_grants::table)
                    .values((drive_grants::item_id.eq(item), drive_grants::user_id.eq(user)))
                    .on_conflict_do_nothing()
                    .execute(conn)
                    .await?;
            }
            Ok(())
        })
        .await)
}

pub(super) async fn revoke(db: &Db, item: &str, user: &str) -> Result<bool, DbError> {
    with_conn!(db, |conn| Ok(diesel::delete(drive_grants::table.find((item, user)))
        .execute(conn)
        .await?
        == 1))
}

#[cfg(test)]
pub(super) mod testing {
    use super::*;
    use crate::db::schema::users;
    use crate::db::users::User;

    pub async fn db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("server.sqlite").display());
        (Db::connect(&url, 1).await.unwrap(), dir)
    }

    pub fn owner() -> User {
        User {
            id: "owner".to_owned(),
            upn: "owner@own.test".to_owned(),
            display_name: "Owner".to_owned(),
            password_hash: String::new(),
            disabled: false,
            created_at: 0,
            updated_at: 0,
            is_admin: false,
            must_change_password: false,
            deleted_at: None,
            password_version: 0,
        }
    }

    async fn add_owner(db: &Db) -> Result<String, DbError> {
        let owner = owner();
        with_conn!(db, |conn| diesel::insert_into(users::table)
            .values(&owner)
            .execute(conn)
            .await?);
        Ok(owner.id)
    }

    /// An empty object of a new account.
    pub async fn object_of(db: &Db) -> Object {
        let object = Object {
            id: "object".to_owned(),
            owner_id: add_owner(db).await.unwrap(),
            kind: "pish/image".to_owned(),
            filename: "picture.png".to_owned(),
            content_type: None,
            blob: None,
            size: 0,
            created_at: 0,
        };
        create_object(db, &object, &[]).await.unwrap();
        object
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{db, object_of};
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn fills_that_started_from_the_same_size_count_the_objects_bytes_once() {
        let (db, _dir) = db().await;
        let object = object_of(&db).await;
        let fill = async |blob: &str, content_type: &str, size: i64, quota: i64| {
            fill_object(&db, &object, blob, content_type, size, quota)
                .await
                .unwrap()
                .map(|replaced| replaced.blob)
        };

        // two first fills, both begun while the object was empty
        assert_eq!(fill("one", "image/png", 10, 10).await, Some(None));
        assert_eq!(fill("two", "image/png", 10, 10).await, Some(Some("one".to_owned())));
        assert_eq!(used(&db, &object.owner_id).await.unwrap(), 10);
        // two replacements, both begun while it held 10 bytes
        assert_eq!(fill("three", "image/gif", 1, 10).await, Some(Some("two".to_owned())));
        assert_eq!(fill("four", "image/gif", 1, 10).await, Some(Some("three".to_owned())));
        assert_eq!(used(&db, &object.owner_id).await.unwrap(), 1);

        // a fill over the quota changes nothing
        assert_eq!(fill("five", "image/png", 11, 10).await, None);
        let now = super::object(&db, &object.id).await.unwrap().unwrap();
        assert_eq!(
            (now.blob.as_deref(), now.content_type.as_deref(), now.size),
            (Some("four"), Some("image/gif"), 1)
        );
        assert_eq!(used(&db, &object.owner_id).await.unwrap(), 1);
    }
}
