//! The rows of the files in channels' folders (`channel_files`): what [`super::sites`] reads and writes.

use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};

use super::repo::{Item, charge};
use super::schema::{channel_files, media_usage};
use crate::chatsvc::schema::chat_members;
use crate::db::{Db, DbError, conflict, with_conn};

/// An empty file, as `addStub` makes it.
pub(super) const STUB: &str = "stub";
/// A file whose bytes one `StartUpload` streams now.
const UPLOADING: &str = "uploading";
/// A file that holds its bytes.
pub(super) const FILLED: &str = "filled";

#[derive(Clone, Debug, Queryable, Selectable, Insertable)]
#[diesel(table_name = channel_files)]
pub(super) struct ChannelFile {
    pub id: String,
    pub team_id: String,
    pub channel_id: String,
    pub folder: String,
    pub name: String,
    pub owner_id: String,
    pub stage: String,
    pub upload_id: Option<String>,
    pub size: i64,
    pub created_at: i64,
}

impl ChannelFile {
    /// The file as the drive's reads and links take one: its bytes are kept under the same kind of key.
    pub(super) fn item(&self) -> Item {
        Item {
            id: self.id.clone(),
            owner_id: self.owner_id.clone(),
            name: self.name.clone(),
            size: self.size,
            created_at: self.created_at,
        }
    }
}

/// Adds an empty file; [`DbError::Conflict`] when the folder has a file of that name.
pub(super) async fn add(db: &Db, file: &ChannelFile) -> Result<(), DbError> {
    with_conn!(db, |conn| {
        diesel::insert_into(channel_files::table)
            .values(file)
            .execute(conn)
            .await
            .map_err(conflict)?;
        Ok(())
    })
}

pub(super) async fn file(db: &Db, id: &str) -> Result<Option<ChannelFile>, DbError> {
    with_conn!(db, |conn| Ok(channel_files::table
        .find(id)
        .select(ChannelFile::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(super) async fn named(db: &Db, team: &str, folder: &str, name: &str) -> Result<Option<ChannelFile>, DbError> {
    with_conn!(db, |conn| Ok(channel_files::table
        .filter(channel_files::team_id.eq(team))
        .filter(channel_files::folder.eq(folder))
        .filter(channel_files::name.eq(name))
        .select(ChannelFile::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(super) async fn owned(db: &Db, owner: &str) -> Result<Vec<ChannelFile>, DbError> {
    with_conn!(db, |conn| Ok(channel_files::table
        .filter(channel_files::owner_id.eq(owner))
        .order((channel_files::created_at.asc(), channel_files::id.asc()))
        .select(ChannelFile::as_select())
        .load(conn)
        .await?))
}

/// Makes the empty file `id` the one `upload` fills; whether it was empty and nothing filled it yet. Of two uploads
/// at once, one gets it.
pub(super) async fn claim(db: &Db, id: &str, upload: &str) -> Result<bool, DbError> {
    with_conn!(db, |conn| Ok(diesel::update(
        channel_files::table
            .filter(channel_files::id.eq(id))
            .filter(channel_files::stage.eq(STUB))
    )
    .set((channel_files::stage.eq(UPLOADING), channel_files::upload_id.eq(upload),))
    .execute(conn)
    .await?
        == 1))
}

/// Gives a claimed file back as empty, after its upload failed.
pub(super) async fn unclaim(db: &Db, id: &str, upload: &str) -> Result<(), DbError> {
    with_conn!(db, |conn| {
        diesel::update(
            channel_files::table
                .filter(channel_files::id.eq(id))
                .filter(channel_files::upload_id.eq(upload))
                .filter(channel_files::stage.eq(UPLOADING)),
        )
        .set((
            channel_files::stage.eq(STUB),
            channel_files::upload_id.eq(None::<String>),
        ))
        .execute(conn)
        .await?;
        Ok(())
    })
}

/// What [`fill`] found.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Filled {
    Done,
    /// The owner's stored bytes would go over the quota.
    Quota,
    /// The file was recycled while its bytes arrived.
    Gone,
}

/// Makes the file `upload` claimed hold `size` bytes, when its owner's stored bytes stay within `quota` (0: no
/// limit). The size and the owner's count change in one transaction.
pub(super) async fn fill(db: &Db, file: &ChannelFile, upload: &str, size: i64, quota: i64) -> Result<Filled, DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let claimed = channel_files::table
                .filter(channel_files::id.eq(&file.id))
                .filter(channel_files::upload_id.eq(upload))
                .filter(channel_files::stage.eq(UPLOADING));
            // changes nothing, but holds the row: a recycle at once waits for this transaction
            if diesel::update(claimed)
                .set(channel_files::size.eq(channel_files::size))
                .execute(conn)
                .await?
                == 0
            {
                return Ok(Filled::Gone);
            }
            if !charge!(conn, &file.owner_id, size, quota) {
                return Ok(Filled::Quota);
            }
            diesel::update(claimed)
                .set((channel_files::stage.eq(FILLED), channel_files::size.eq(size)))
                .execute(conn)
                .await?;
            Ok(Filled::Done)
        })
        .await)
}

/// Removes the file and takes its bytes off its owner's count; whether it was there. Of two removals at once, one
/// finds it.
pub(super) async fn remove(db: &Db, id: &str) -> Result<bool, DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            // the size comes from the deleted row: a fill that held the row has committed by then
            let Some((owner, size)) = diesel::delete(channel_files::table.find(id))
                .returning((channel_files::owner_id, channel_files::size))
                .get_result::<(String, i64)>(conn)
                .await
                .optional()?
            else {
                return Ok(false);
            };
            let freed = -size;
            let _ = charge!(conn, &owner, freed, 0);
            Ok(true)
        })
        .await)
}

/// Whether `user` is in the thread `thread` now.
pub(super) async fn in_thread(db: &Db, thread: &str, user: &str) -> Result<bool, DbError> {
    with_conn!(db, |conn| Ok(diesel::select(diesel::dsl::exists(
        chat_members::table
            .filter(chat_members::chat_id.eq(thread))
            .filter(chat_members::user_id.eq(user))
            .filter(chat_members::left_at.is_null())
    ))
    .get_result(conn)
    .await?))
}
