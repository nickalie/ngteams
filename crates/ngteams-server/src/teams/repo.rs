use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};
use serde_json::{Map, Value};

use super::schema::{
    channel_roles, channels, conversation_folders, team_join_requests, team_members, team_tags, teams,
};
use crate::chatsvc::repo::advance;
use crate::chatsvc::schema::{chat_members, chat_messages, chat_properties, chats};
use crate::db::schema::users;
use crate::db::{Db, DbError, with_conn};

#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = teams)]
pub(crate) struct Team {
    pub id: String,
    pub group_id: String,
    pub name: String,
    pub description: String,
    pub public: bool,
    pub archived: bool,
    pub join_code: Option<String>,
    pub settings: String,
    pub tag_settings: String,
    pub picture_etag: String,
    pub created_at: i64,
    pub version: i64,
}

#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = channels)]
pub(crate) struct Channel {
    pub id: String,
    pub team_id: String,
    pub group_id: Option<String>,
    pub name: String,
    pub description: String,
    pub private: bool,
    pub layout: String,
    pub archived: bool,
    pub shown_by_default: bool,
    pub settings: Option<String>,
    pub created_at: i64,
    pub version: i64,
}

impl Channel {
    pub(crate) fn general(&self) -> bool {
        self.id == self.team_id
    }
}

#[derive(Clone, Queryable, Selectable)]
#[diesel(table_name = team_members)]
pub(crate) struct TeamMember {
    pub user_id: String,
    pub owner: bool,
    pub hidden: bool,
    pub joined_at: i64,
}

#[derive(Clone, Queryable, Selectable)]
#[diesel(table_name = channel_roles)]
pub(crate) struct Role {
    pub channel_id: String,
    pub user_id: String,
    pub owner: bool,
    pub moderator: bool,
}

#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = team_tags)]
pub(crate) struct Tag {
    pub id: String,
    pub team_id: String,
    pub name: String,
    pub description: String,
    pub members: String,
    pub version: i64,
}

fn position(at: i64) -> i32 {
    i32::try_from(at / 1000).unwrap_or(i32::MAX)
}

macro_rules! join_chat {
    ($conn:expr, $chat:expr, $user:expr, $at:expr) => {
        diesel::insert_into(chat_members::table)
            .values((
                chat_members::chat_id.eq($chat),
                chat_members::user_id.eq($user),
                chat_members::position.eq(position($at)),
                chat_members::joined_at.eq($at),
            ))
            .on_conflict_do_nothing()
            .execute($conn)
            .await?
    };
}

macro_rules! touch_team {
    ($conn:expr, $team:expr, $version:expr) => {{
        diesel::update(teams::table.find($team))
            .set(teams::version.eq($version))
            .execute($conn)
            .await?;
        diesel::update(chats::table.find($team))
            .set(chats::version.eq($version))
            .execute($conn)
            .await?;
    }};
}

macro_rules! touch_channel {
    ($conn:expr, $channel:expr, $version:expr) => {{
        diesel::update(channels::table.find($channel))
            .set(channels::version.eq($version))
            .execute($conn)
            .await?;
        diesel::update(chats::table.find($channel))
            .set(chats::version.eq($version))
            .execute($conn)
            .await?;
    }};
}
fn now() -> i64 {
    crate::chatsvc::now()
}

/// A new team with its creator as owner and its general channel, whose thread is the team's id.
pub(crate) async fn create_team(db: &Db, team: &Team, first_channel: &str, creator: &str) -> Result<Team, DbError> {
    let at = now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            let mut team = team.clone();
            team.created_at = version;
            team.version = version;
            insert_thread!(conn, &team.id, creator, first_channel, version);
            diesel::insert_into(teams::table).values(&team).execute(conn).await?;
            diesel::insert_into(team_members::table)
                .values((
                    team_members::team_id.eq(&team.id),
                    team_members::user_id.eq(creator),
                    team_members::owner.eq(true),
                    team_members::joined_at.eq(version),
                ))
                .execute(conn)
                .await?;
            diesel::insert_into(channels::table)
                .values(&Channel {
                    id: team.id.clone(),
                    team_id: team.id.clone(),
                    group_id: None,
                    name: first_channel.to_owned(),
                    description: String::new(),
                    private: false,
                    layout: "Conversational".to_owned(),
                    archived: false,
                    shown_by_default: true,
                    settings: None,
                    created_at: version,
                    version,
                })
                .execute(conn)
                .await?;
            Ok(team)
        })
        .await)
}

macro_rules! insert_thread {
    ($conn:expr, $id:expr, $creator:expr, $topic:expr, $version:expr) => {{
        diesel::insert_into(chats::table)
            .values((
                chats::id.eq($id),
                chats::creator.eq($creator),
                chats::topic.eq(Some($topic)),
                chats::unique_roster.eq(false),
                chats::created_at.eq($version),
                chats::version.eq($version),
            ))
            .execute($conn)
            .await?;
        diesel::insert_into(chat_members::table)
            .values((
                chat_members::chat_id.eq($id),
                chat_members::user_id.eq($creator),
                chat_members::position.eq(0),
                chat_members::joined_at.eq($version),
            ))
            .execute($conn)
            .await?;
    }};
}
use insert_thread;

pub(crate) async fn team(db: &Db, id: &str) -> Result<Option<Team>, DbError> {
    with_conn!(db, |conn| Ok(teams::table
        .find(id)
        .select(Team::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(crate) async fn team_by_group(db: &Db, group: &str) -> Result<Option<Team>, DbError> {
    with_conn!(db, |conn| Ok(teams::table
        .filter(teams::group_id.eq(group))
        .select(Team::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(crate) async fn team_by_code(db: &Db, code: &str) -> Result<Option<Team>, DbError> {
    with_conn!(db, |conn| Ok(teams::table
        .filter(teams::join_code.eq(code))
        .select(Team::as_select())
        .first(conn)
        .await
        .optional()?))
}

/// The teams `user` is on, oldest first, each with the account's membership.
pub(crate) async fn memberships(db: &Db, user: &str) -> Result<Vec<(Team, TeamMember)>, DbError> {
    with_conn!(db, |conn| Ok(teams::table
        .inner_join(team_members::table)
        .filter(team_members::user_id.eq(user))
        .order(teams::created_at.asc())
        .select((Team::as_select(), TeamMember::as_select()))
        .load(conn)
        .await?))
}

pub(crate) async fn membership(db: &Db, team: &str, user: &str) -> Result<Option<TeamMember>, DbError> {
    with_conn!(db, |conn| Ok(team_members::table
        .find((team, user))
        .select(TeamMember::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(crate) async fn roster(db: &Db, team: &str) -> Result<Vec<TeamMember>, DbError> {
    with_conn!(db, |conn| Ok(team_members::table
        .filter(team_members::team_id.eq(team))
        .order((team_members::joined_at.asc(), team_members::user_id.asc()))
        .select(TeamMember::as_select())
        .load(conn)
        .await?))
}

/// A `LIKE` pattern (escaped with `\`) for text holding `keyword`, any case.
fn like(keyword: &str) -> String {
    format!(
        "%{}%",
        keyword
            .to_lowercase()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}

/// An account as a people picker lists it.
#[derive(Queryable)]
pub(crate) struct Person {
    pub id: String,
    pub upn: String,
    pub name: String,
}

/// Up to `limit` accounts that can sign in whose name or address holds `keyword` (any case), by name: everyone,
/// or the people of `team`.
pub(crate) async fn people(db: &Db, keyword: &str, team: Option<&str>, limit: i64) -> Result<Vec<Person>, DbError> {
    let pattern = like(keyword);
    with_conn!(db, |conn| {
        let mut query = users::table
            .filter(crate::db::users::person())
            .filter(users::disabled.eq(false))
            .filter(users::deleted_at.is_null())
            .filter(
                crate::db::lower(users::upn)
                    .like(&pattern)
                    .escape('\\')
                    .or(crate::db::lower(users::display_name).like(&pattern).escape('\\')),
            )
            .order((users::display_name.asc(), users::id.asc()))
            .limit(limit)
            .select((users::id, users::upn, users::display_name))
            .into_boxed();
        if let Some(team) = team {
            query = query.filter(diesel::dsl::exists(
                team_members::table
                    .filter(team_members::team_id.eq(team))
                    .filter(team_members::user_id.eq(users::id)),
            ));
        }
        Ok(query.load(conn).await?)
    })
}

/// Public teams `user` isn't on whose name or description holds `keywords` (any case), by name.
pub(crate) async fn joinable(db: &Db, user: &str, keywords: Option<&str>) -> Result<Vec<Team>, DbError> {
    let pattern = keywords.map(like);
    with_conn!(db, |conn| {
        let mut query = teams::table
            .filter(teams::public.eq(true))
            .filter(teams::archived.eq(false))
            .filter(diesel::dsl::not(diesel::dsl::exists(
                team_members::table
                    .filter(team_members::team_id.eq(teams::id))
                    .filter(team_members::user_id.eq(user)),
            )))
            .order(teams::name.asc())
            .limit(100)
            .select(Team::as_select())
            .into_boxed();
        if let Some(pattern) = &pattern {
            query = query.filter(
                crate::db::lower(teams::name)
                    .like(pattern)
                    .escape('\\')
                    .or(crate::db::lower(teams::description).like(pattern).escape('\\')),
            );
        }
        Ok(query.load(conn).await?)
    })
}

pub(crate) async fn channels_of(db: &Db, team_ids: &[String]) -> Result<Vec<Channel>, DbError> {
    with_conn!(db, |conn| Ok(channels::table
        .filter(channels::team_id.eq_any(team_ids))
        .order((channels::created_at.asc(), channels::id.asc()))
        .select(Channel::as_select())
        .load(conn)
        .await?))
}

pub(crate) async fn channel(db: &Db, id: &str) -> Result<Option<Channel>, DbError> {
    with_conn!(db, |conn| Ok(channels::table
        .find(id)
        .select(Channel::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(crate) async fn channel_by_group(db: &Db, group: &str) -> Result<Option<Channel>, DbError> {
    with_conn!(db, |conn| Ok(channels::table
        .filter(channels::group_id.eq(group))
        .select(Channel::as_select())
        .first(conn)
        .await
        .optional()?))
}

/// The threads among `ids` that `user` is a member of.
pub(crate) async fn joined(db: &Db, user: &str, ids: &[String]) -> Result<Vec<String>, DbError> {
    with_conn!(db, |conn| Ok(chat_members::table
        .filter(chat_members::user_id.eq(user))
        .filter(chat_members::left_at.is_null())
        .filter(chat_members::chat_id.eq_any(ids))
        .select(chat_members::chat_id)
        .load(conn)
        .await?))
}

/// Who made each of the threads `ids`.
pub(crate) async fn creators(db: &Db, ids: &[String]) -> Result<Vec<(String, String)>, DbError> {
    with_conn!(db, |conn| Ok(chats::table
        .filter(chats::id.eq_any(ids))
        .select((chats::id, chats::creator))
        .load(conn)
        .await?))
}

/// The account's own properties of the threads among `ids`.
pub(crate) async fn own_properties(
    db: &Db,
    user: &str,
    ids: &[String],
) -> Result<Vec<(String, Map<String, Value>)>, DbError> {
    let rows: Vec<(String, String)> = with_conn!(db, |conn| chat_properties::table
        .filter(chat_properties::user_id.eq(user))
        .filter(chat_properties::chat_id.eq_any(ids))
        .select((chat_properties::chat_id, chat_properties::body))
        .load(conn)
        .await?);
    rows.into_iter()
        .map(|(id, body)| Ok((id, crate::chatsvc::decode_properties(Some(body))?)))
        .collect()
}

pub(crate) async fn roles_of(db: &Db, channel_ids: &[String]) -> Result<Vec<Role>, DbError> {
    with_conn!(db, |conn| Ok(channel_roles::table
        .filter(channel_roles::channel_id.eq_any(channel_ids))
        .select(Role::as_select())
        .load(conn)
        .await?))
}

/// How many people each of `team_ids` has, and how many of them own it.
pub(crate) async fn head_counts(db: &Db, team_ids: &[String]) -> Result<Vec<(String, bool, i64)>, DbError> {
    with_conn!(db, |conn| Ok(team_members::table
        .filter(team_members::team_id.eq_any(team_ids))
        .group_by((team_members::team_id, team_members::owner))
        .select((team_members::team_id, team_members::owner, diesel::dsl::count_star()))
        .load(conn)
        .await?))
}

pub(crate) struct TeamEdit<'a> {
    pub name: Option<&'a str>,
    pub description: Option<&'a str>,
    pub public: Option<bool>,
    pub archived: Option<bool>,
    pub settings: Option<&'a str>,
    pub tag_settings: Option<&'a str>,
    pub picture_etag: Option<&'a str>,
    pub join_code: Option<Code<'a>>,
}

/// A team's code made (or made anew), or taken away.
#[derive(Clone, Copy)]
pub(crate) enum Code<'a> {
    Set(&'a str),
    Gone,
}

impl TeamEdit<'_> {
    pub(crate) const NONE: Self = Self {
        name: None,
        description: None,
        public: None,
        archived: None,
        settings: None,
        tag_settings: None,
        picture_etag: None,
        join_code: None,
    };
}

pub(crate) async fn edit_team(db: &Db, id: &str, edit: &TeamEdit<'_>) -> Result<(), DbError> {
    let at = now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            let Some(mut team) = teams::table
                .find(id)
                .select(Team::as_select())
                .first(conn)
                .await
                .optional()?
            else {
                return Ok(());
            };
            if let Some(name) = edit.name {
                name.clone_into(&mut team.name);
            }
            if let Some(description) = edit.description {
                description.clone_into(&mut team.description);
            }
            if let Some(settings) = edit.settings {
                settings.clone_into(&mut team.settings);
            }
            if let Some(settings) = edit.tag_settings {
                settings.clone_into(&mut team.tag_settings);
            }
            if let Some(etag) = edit.picture_etag {
                etag.clone_into(&mut team.picture_etag);
            }
            match edit.join_code {
                Some(Code::Set(code)) => team.join_code = Some(code.to_owned()),
                Some(Code::Gone) => team.join_code = None,
                None => {}
            }
            team.public = edit.public.unwrap_or(team.public);
            team.archived = edit.archived.unwrap_or(team.archived);
            diesel::update(teams::table.find(id))
                .set((
                    teams::name.eq(&team.name),
                    teams::description.eq(&team.description),
                    teams::public.eq(team.public),
                    teams::archived.eq(team.archived),
                    teams::settings.eq(&team.settings),
                    teams::tag_settings.eq(&team.tag_settings),
                    teams::picture_etag.eq(&team.picture_etag),
                    teams::join_code.eq(&team.join_code),
                ))
                .execute(conn)
                .await
                .map_err(crate::db::conflict)?;
            touch_team!(conn, id, version);
            Ok(())
        })
        .await)
}

/// The team, its channels, their threads and messages, gone.
pub(crate) async fn delete_team(db: &Db, id: &str) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let threads: Vec<String> = channels::table
                .filter(channels::team_id.eq(id))
                .filter(channels::id.ne(id))
                .select(channels::id)
                .load(conn)
                .await?;
            diesel::delete(chats::table.filter(chats::id.eq_any(&threads)))
                .execute(conn)
                .await?;
            diesel::delete(chats::table.find(id)).execute(conn).await?;
            Ok(())
        })
        .await)
}

/// Puts `user` on the team with a role and into its standard channels. Someone already on it keeps their role:
/// only `change_member`, which keeps an owner, changes one.
pub(crate) async fn put_member(db: &Db, team: &str, user: &str, owner: bool) -> Result<(), DbError> {
    let at = now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            diesel::insert_into(team_members::table)
                .values((
                    team_members::team_id.eq(team),
                    team_members::user_id.eq(user),
                    team_members::owner.eq(owner),
                    team_members::joined_at.eq(version),
                ))
                .on_conflict_do_nothing()
                .execute(conn)
                .await?;
            let standard: Vec<String> = channels::table
                .filter(channels::team_id.eq(team))
                .filter(channels::private.eq(false))
                .select(channels::id)
                .load(conn)
                .await?;
            for channel in &standard {
                join_chat!(conn, channel, user, version);
            }
            diesel::delete(team_join_requests::table.find((team, user)))
                .execute(conn)
                .await?;
            touch_team!(conn, team, version);
            Ok(())
        })
        .await)
}

/// Puts the bot of an app added to the team into its standard channels (`add`), or takes it out of every channel of
/// the team.
pub(crate) async fn seat_bot(db: &Db, team: &str, bot: &str, add: bool) -> Result<(), DbError> {
    let at = now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            let threads: Vec<String> = channels::table
                .filter(channels::team_id.eq(team))
                .select((channels::id, channels::private))
                .load::<(String, bool)>(conn)
                .await?
                .into_iter()
                .filter(|(_, private)| !add || !private)
                .map(|(id, _)| id)
                .collect();
            if add {
                for channel in &threads {
                    join_chat!(conn, channel, bot, version);
                }
            } else {
                diesel::delete(
                    chat_members::table
                        .filter(chat_members::chat_id.eq_any(&threads))
                        .filter(chat_members::user_id.eq(bot)),
                )
                .execute(conn)
                .await?;
            }
            Ok(())
        })
        .await)
}

/// What a change of someone's place on a team came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Changed {
    Done,
    NotMember,
    LastOwner,
}

/// Before `user` goes from the team: each private channel only they own gets its longest-standing other member as
/// owner, and one with nobody else in it is deleted. The channels that got an owner.
macro_rules! hand_on_channels {
    ($conn:expr, $team:expr, $user:expr, $version:expr) => {{
        let private: Vec<String> = channels::table
            .filter(channels::team_id.eq($team))
            .filter(channels::private.eq(true))
            .select(channels::id)
            .load($conn)
            .await?;
        let owners: Vec<(String, String)> = channel_roles::table
            .filter(channel_roles::channel_id.eq_any(&private))
            .filter(channel_roles::owner.eq(true))
            .select((channel_roles::channel_id, channel_roles::user_id))
            .load($conn)
            .await?;
        let mut handed = Vec::new();
        for channel in private {
            let of_channel = owners.iter().filter(|(of, _)| *of == channel);
            if !of_channel.map(|(_, owner)| owner.as_str()).eq([$user]) {
                continue;
            }
            let heir: Option<String> = chat_members::table
                .filter(chat_members::chat_id.eq(&channel))
                .filter(chat_members::user_id.ne($user))
                .order((chat_members::joined_at.asc(), chat_members::user_id.asc()))
                .select(chat_members::user_id)
                .first($conn)
                .await
                .optional()?;
            let Some(heir) = heir else {
                diesel::delete(chats::table.find(&channel)).execute($conn).await?;
                continue;
            };
            diesel::insert_into(channel_roles::table)
                .values((
                    channel_roles::channel_id.eq(&channel),
                    channel_roles::user_id.eq(&heir),
                    channel_roles::owner.eq(true),
                ))
                .on_conflict((channel_roles::channel_id, channel_roles::user_id))
                .do_update()
                .set(channel_roles::owner.eq(true))
                .execute($conn)
                .await?;
            touch_channel!($conn, &channel, $version);
            handed.push(channel);
        }
        handed
    }};
}

/// Changes the role of someone on the team (`owner`), or takes them off it (`None`), never leaving it or a private
/// channel of it ownerless. With what it came to, the private channels that got a new owner.
pub(crate) async fn change_member(
    db: &Db,
    team: &str,
    user: &str,
    owner: Option<bool>,
) -> Result<(Changed, Vec<String>), DbError> {
    let at = now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            let Some(current) = team_members::table
                .find((team, user))
                .select(team_members::owner)
                .first::<bool>(conn)
                .await
                .optional()?
            else {
                return Ok((Changed::NotMember, Vec::new()));
            };
            if current && owner != Some(true) {
                let owners: i64 = team_members::table
                    .filter(team_members::team_id.eq(team))
                    .filter(team_members::owner.eq(true))
                    .count()
                    .get_result(conn)
                    .await?;
                if owners < 2 {
                    return Ok((Changed::LastOwner, Vec::new()));
                }
            }
            let mut handed = Vec::new();
            if let Some(owner) = owner {
                diesel::update(team_members::table.find((team, user)))
                    .set(team_members::owner.eq(owner))
                    .execute(conn)
                    .await?;
            } else {
                handed = hand_on_channels!(conn, team, user, version);
                let threads: Vec<String> = channels::table
                    .filter(channels::team_id.eq(team))
                    .select(channels::id)
                    .load(conn)
                    .await?;
                diesel::delete(
                    channel_roles::table
                        .filter(channel_roles::channel_id.eq_any(&threads))
                        .filter(channel_roles::user_id.eq(user)),
                )
                .execute(conn)
                .await?;
                diesel::delete(
                    chat_members::table
                        .filter(chat_members::chat_id.eq_any(&threads))
                        .filter(chat_members::user_id.eq(user)),
                )
                .execute(conn)
                .await?;
                diesel::delete(team_members::table.find((team, user)))
                    .execute(conn)
                    .await?;
            }
            touch_team!(conn, team, version);
            Ok((Changed::Done, handed))
        })
        .await)
}

/// A new channel: everyone on the team in a standard one, its creator alone (as owner) in a private one.
/// `Conflict` when the team has a channel of that name (any case).
pub(crate) async fn create_channel(db: &Db, channel: &Channel, creator: &str) -> Result<Channel, DbError> {
    let at = now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            let taken: i64 = channels::table
                .filter(channels::team_id.eq(&channel.team_id))
                .filter(crate::db::lower(channels::name).eq(channel.name.to_lowercase()))
                .count()
                .get_result(conn)
                .await?;
            if taken > 0 {
                return Err(DbError::Conflict);
            }
            let mut channel = channel.clone();
            channel.created_at = version;
            channel.version = version;
            insert_thread!(conn, &channel.id, creator, channel.name.as_str(), version);
            diesel::insert_into(channels::table)
                .values(&channel)
                .execute(conn)
                .await?;
            if channel.private {
                diesel::insert_into(channel_roles::table)
                    .values((
                        channel_roles::channel_id.eq(&channel.id),
                        channel_roles::user_id.eq(creator),
                        channel_roles::owner.eq(true),
                    ))
                    .execute(conn)
                    .await?;
            } else {
                let people: Vec<String> = team_members::table
                    .filter(team_members::team_id.eq(&channel.team_id))
                    .select(team_members::user_id)
                    .load(conn)
                    .await?;
                for person in &people {
                    join_chat!(conn, &channel.id, person, version);
                }
                let bots: Vec<String> = crate::apps::schema::bots::table
                    .inner_join(
                        crate::apps::schema::app_installs::table
                            .on(crate::apps::schema::app_installs::app_id.eq(crate::apps::schema::bots::app_id)),
                    )
                    .filter(crate::apps::schema::app_installs::scope.eq("team"))
                    .filter(crate::apps::schema::app_installs::target.eq(&channel.team_id))
                    .select(crate::apps::schema::bots::id)
                    .load(conn)
                    .await?;
                for bot in &bots {
                    join_chat!(conn, &channel.id, bot, version);
                }
            }
            touch_team!(conn, &channel.team_id, version);
            Ok(channel)
        })
        .await)
}

pub(crate) struct ChannelEdit<'a> {
    pub name: Option<&'a str>,
    pub description: Option<&'a str>,
    pub archived: Option<bool>,
    pub shown_by_default: Option<bool>,
    pub settings: Option<&'a str>,
}

impl ChannelEdit<'_> {
    pub(crate) const NONE: Self = Self {
        name: None,
        description: None,
        archived: None,
        shown_by_default: None,
        settings: None,
    };
}

/// Changes the fields `edit` names of the channel as it is now, so two edits at once keep each other's change.
/// `Conflict` when the new name is another channel's of the team.
pub(crate) async fn edit_channel(db: &Db, id: &str, edit: &ChannelEdit<'_>) -> Result<(), DbError> {
    let at = now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            let Some(channel) = channels::table
                .find(id)
                .select(Channel::as_select())
                .first(conn)
                .await
                .optional()?
            else {
                return Ok(());
            };
            if let Some(name) = edit.name {
                let taken: i64 = channels::table
                    .filter(channels::team_id.eq(&channel.team_id))
                    .filter(channels::id.ne(id))
                    .filter(crate::db::lower(channels::name).eq(name.to_lowercase()))
                    .count()
                    .get_result(conn)
                    .await?;
                if taken > 0 {
                    return Err(DbError::Conflict);
                }
            }
            let name = edit.name.unwrap_or(&channel.name);
            diesel::update(channels::table.find(id))
                .set((
                    channels::name.eq(name),
                    channels::description.eq(edit.description.unwrap_or(&channel.description)),
                    channels::archived.eq(edit.archived.unwrap_or(channel.archived)),
                    channels::shown_by_default.eq(edit.shown_by_default.unwrap_or(channel.shown_by_default)),
                    channels::settings.eq(edit.settings.or(channel.settings.as_deref())),
                    channels::version.eq(version),
                ))
                .execute(conn)
                .await?;
            diesel::update(chats::table.find(id))
                .set((chats::topic.eq(Some(name)), chats::version.eq(version)))
                .execute(conn)
                .await?;
            touch_team!(conn, &channel.team_id, version);
            Ok(())
        })
        .await)
}

pub(crate) async fn delete_channel(db: &Db, channel: &Channel) -> Result<(), DbError> {
    let at = now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            diesel::delete(chats::table.find(&channel.id)).execute(conn).await?;
            touch_team!(conn, &channel.team_id, version);
            Ok(())
        })
        .await)
}

/// Someone on the team put into a private channel with a role. Someone already in it keeps their role: only
/// `change_channel_member`, which keeps an owner, changes one.
pub(crate) async fn put_channel_member(db: &Db, channel: &Channel, user: &str, owner: bool) -> Result<(), DbError> {
    let at = now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            join_chat!(conn, &channel.id, user, version);
            diesel::insert_into(channel_roles::table)
                .values((
                    channel_roles::channel_id.eq(&channel.id),
                    channel_roles::user_id.eq(user),
                    channel_roles::owner.eq(owner),
                ))
                .on_conflict_do_nothing()
                .execute(conn)
                .await?;
            touch_channel!(conn, &channel.id, version);
            Ok(())
        })
        .await)
}

/// Takes someone out of a private channel, or makes an owner of it a member, never leaving it ownerless.
pub(crate) async fn change_channel_member(
    db: &Db,
    channel: &Channel,
    user: &str,
    owner: Option<bool>,
) -> Result<Changed, DbError> {
    let at = now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            let member: i64 = chat_members::table
                .filter(chat_members::chat_id.eq(&channel.id))
                .filter(chat_members::user_id.eq(user))
                .count()
                .get_result(conn)
                .await?;
            if member == 0 {
                return Ok(Changed::NotMember);
            }
            let owners: Vec<String> = channel_roles::table
                .filter(channel_roles::channel_id.eq(&channel.id))
                .filter(channel_roles::owner.eq(true))
                .select(channel_roles::user_id)
                .load(conn)
                .await?;
            if owner != Some(true) && owners.len() < 2 && owners.iter().any(|owner| owner == user) {
                return Ok(Changed::LastOwner);
            }
            diesel::delete(channel_roles::table.find((&channel.id, user)))
                .execute(conn)
                .await?;
            match owner {
                Some(owner) => {
                    diesel::insert_into(channel_roles::table)
                        .values((
                            channel_roles::channel_id.eq(&channel.id),
                            channel_roles::user_id.eq(user),
                            channel_roles::owner.eq(owner),
                        ))
                        .execute(conn)
                        .await?;
                }
                None => {
                    diesel::delete(chat_members::table.find((&channel.id, user)))
                        .execute(conn)
                        .await?;
                }
            }
            touch_channel!(conn, &channel.id, version);
            Ok(Changed::Done)
        })
        .await)
}

pub(crate) async fn set_moderator(db: &Db, channel: &str, user: &str, moderator: bool) -> Result<(), DbError> {
    let at = now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            diesel::insert_into(channel_roles::table)
                .values((
                    channel_roles::channel_id.eq(channel),
                    channel_roles::user_id.eq(user),
                    channel_roles::moderator.eq(moderator),
                ))
                .on_conflict((channel_roles::channel_id, channel_roles::user_id))
                .do_update()
                .set(channel_roles::moderator.eq(moderator))
                .execute(conn)
                .await?;
            touch_channel!(conn, channel, version);
            Ok(())
        })
        .await)
}

/// Hides or shows a team for `user` alone: none of a hidden team's channels is in the account's list afterwards,
/// the general one of a team shown again is.
pub(crate) async fn set_hidden(db: &Db, team: &str, user: &str, hidden: bool) -> Result<(), DbError> {
    let all = channels_of(db, &[team.to_owned()]).await?;
    let ids: Vec<String> = all.into_iter().map(|channel| channel.id).collect();
    mark_hidden(db, team, user, hidden).await?;
    let value = if hidden { "false" } else { "true" };
    for id in joined(db, user, &ids).await? {
        if id == team {
            crate::chatsvc::set_own_property(db, &id, user, "isGeneralChannelFavorite", value).await?;
        } else if hidden {
            crate::chatsvc::set_own_property(db, &id, user, "favorite", value).await?;
        }
    }
    Ok(())
}

pub(crate) async fn mark_hidden(db: &Db, team: &str, user: &str, hidden: bool) -> Result<(), DbError> {
    with_conn!(db, |conn| {
        diesel::update(team_members::table.find((team, user)))
            .set(team_members::hidden.eq(hidden))
            .execute(conn)
            .await?;
    });
    Ok(())
}

/// Stamped from the server's clock, so requests asked together in one call read back in the order asked.
pub(crate) async fn request_join(db: &Db, team: &str, user: &str) -> Result<(), DbError> {
    let at = now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let stamp: i64 = advance!(conn, at);
            diesel::insert_into(team_join_requests::table)
                .values((
                    team_join_requests::team_id.eq(team),
                    team_join_requests::user_id.eq(user),
                    team_join_requests::created_at.eq(stamp),
                ))
                .on_conflict_do_nothing()
                .execute(conn)
                .await?;
            Ok(())
        })
        .await)
}

pub(crate) async fn join_requests(db: &Db, team: &str) -> Result<Vec<String>, DbError> {
    with_conn!(db, |conn| Ok(team_join_requests::table
        .filter(team_join_requests::team_id.eq(team))
        .order((team_join_requests::created_at.asc(), team_join_requests::user_id.asc()))
        .select(team_join_requests::user_id)
        .load(conn)
        .await?))
}

/// Whether the request was there to drop.
pub(crate) async fn drop_join_request(db: &Db, team: &str, user: &str) -> Result<bool, DbError> {
    with_conn!(db, |conn| Ok(diesel::delete(
        team_join_requests::table.find((team, user))
    )
    .execute(conn)
    .await?
        > 0))
}

pub(crate) async fn tags(db: &Db, team: &str) -> Result<Vec<Tag>, DbError> {
    with_conn!(db, |conn| Ok(team_tags::table
        .filter(team_tags::team_id.eq(team))
        .order(team_tags::name.asc())
        .select(Tag::as_select())
        .load(conn)
        .await?))
}

pub(crate) async fn tag(db: &Db, team: &str, id: &str) -> Result<Option<Tag>, DbError> {
    with_conn!(db, |conn| Ok(team_tags::table
        .find(id)
        .filter(team_tags::team_id.eq(team))
        .select(Tag::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(crate) async fn insert_tag(db: &Db, tag: &Tag) -> Result<(), DbError> {
    with_conn!(db, |conn| {
        diesel::insert_into(team_tags::table).values(tag).execute(conn).await?;
    });
    Ok(())
}

/// Whether the tag was still at `version` and so changed.
pub(crate) async fn edit_tag(db: &Db, tag: &Tag, name: &str, description: &str) -> Result<bool, DbError> {
    with_conn!(db, |conn| Ok(diesel::update(
        team_tags::table
            .find(&tag.id)
            .filter(team_tags::version.eq(tag.version))
    )
    .set((
        team_tags::name.eq(name),
        team_tags::description.eq(description),
        team_tags::version.eq(tag.version + 1),
    ))
    .execute(conn)
    .await?
        > 0))
}

pub(crate) async fn delete_tag(db: &Db, team: &str, id: &str) -> Result<bool, DbError> {
    with_conn!(db, |conn| Ok(diesel::delete(
        team_tags::table.find(id).filter(team_tags::team_id.eq(team))
    )
    .execute(conn)
    .await?
        > 0))
}

pub(crate) async fn folders(db: &Db, user: &str) -> Result<Option<(i64, String)>, DbError> {
    with_conn!(db, |conn| Ok(conversation_folders::table
        .find(user)
        .select((conversation_folders::version, conversation_folders::body))
        .first(conn)
        .await
        .optional()?))
}

/// Keeps the account's folders when they are still at `read` (`None`: never kept); whether it did.
pub(crate) async fn keep_folders(
    db: &Db,
    user: &str,
    read: Option<i64>,
    version: i64,
    body: &str,
) -> Result<bool, DbError> {
    let Some(read) = read else {
        return first_folders(db, user, version, body).await;
    };
    with_conn!(db, |conn| Ok(diesel::update(
        conversation_folders::table
            .find(user)
            .filter(conversation_folders::version.eq(read))
    )
    .set((
        conversation_folders::version.eq(version),
        conversation_folders::body.eq(body),
    ))
    .execute(conn)
    .await?
        > 0))
}

async fn first_folders(db: &Db, user: &str, version: i64, body: &str) -> Result<bool, DbError> {
    with_conn!(db, |conn| Ok(diesel::insert_into(conversation_folders::table)
        .values((
            conversation_folders::user_id.eq(user),
            conversation_folders::version.eq(version),
            conversation_folders::body.eq(body),
        ))
        .on_conflict_do_nothing()
        .execute(conn)
        .await?
        > 0))
}

/// The id, post, author and body of each message of `threads` that arrived since `since`, oldest first (counted
/// by `mt::metrics`).
pub(crate) async fn message_bodies(
    db: &Db,
    threads: &[String],
    since: i64,
) -> Result<Vec<(i64, Option<i64>, String, String)>, DbError> {
    with_conn!(db, |conn| Ok(chat_messages::table
        .filter(chat_messages::chat_id.eq_any(threads))
        .filter(chat_messages::id.ge(since))
        .order(chat_messages::id.asc())
        .select((
            chat_messages::id,
            chat_messages::root_id,
            chat_messages::owner_id,
            chat_messages::body,
        ))
        .load(conn)
        .await?))
}
