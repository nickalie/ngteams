//! A channel is a thread of the chat service: its people are the thread's members, so every read and write of its
//! messages passes the same membership check as a chat's. What differs is here: a channel's thread and conversation
//! shapes, a post's reply chain (`<channel>;messageid=<post>`), and the roles its member list names.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State, rejection::JsonRejection};
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::repo::{Chat, Member};
use super::{Caller, Error, member_of, wire};
use crate::app::App;
use crate::db::{Db, DbError};
use crate::teams::repo::{self, Channel, Team};

/// A channel with the team it belongs to.
pub(crate) struct Place {
    pub channel: Channel,
    pub team: Team,
}

pub(crate) async fn place(db: &Db, thread: &str) -> Result<Option<Place>, DbError> {
    let Some(channel) = repo::channel(db, thread).await? else {
        return Ok(None);
    };
    Ok(repo::team(db, &channel.team_id)
        .await?
        .map(|team| Place { channel, team }))
}

/// The root a new message of `thread` is kept under: in a channel, the post `under` replies to, or none for a post
/// of its own (each post is the root of its thread of replies); in a chat, no root.
pub(crate) async fn root(db: &Db, thread: &str, under: Option<i64>) -> Result<Option<i64>, DbError> {
    Ok(place(db, thread).await?.map(|_| under.unwrap_or(0)))
}

/// A conversation path's thread and, for a post's reply chain, the post.
pub(super) fn split(target: &str) -> Result<(&str, Option<i64>), Error> {
    match target.split_once(";messageid=") {
        None => Ok((target, None)),
        Some((thread, post)) => Ok((thread, Some(post.parse().map_err(|_| Error::BadRequest)?))),
    }
}

/// Whether the channel takes new messages.
pub(super) fn writable(place: &Place) -> bool {
    !place.channel.archived && !place.team.archived
}

fn kinds(place: &Place) -> (&'static str, &'static str) {
    if place.channel.general() {
        ("space", "TeamsTeam")
    } else if place.channel.private {
        ("space", "TeamsPrivateChannel")
    } else {
        ("topic", "TeamsStandardChannel")
    }
}

fn shared_properties(place: &Place) -> Value {
    let (thread_type, product) = kinds(place);
    let mut properties = json!({
        "threadType": thread_type, "productThreadType": product, "spaceType": "standard",
        "groupId": place.team.group_id, "spaceThreadTopic": place.team.name,
        "topic": place.channel.name, "topicThreadTopic": place.channel.name,
        "description": place.channel.description, "gapDetectionEnabled": "True",
        "visibility": if place.team.public { "public" } else { "private" },
        "isArchived": (place.channel.archived || place.team.archived).to_string(),
    });
    if !place.channel.general() {
        properties["spaceId"] = json!(place.team.id);
    }
    properties
}

/// The role of each of a channel's members: an owner of the team (of a private channel: of the channel) is `Admin`.
pub(crate) struct Roles {
    owners: Vec<String>,
    moderators: Vec<String>,
}

pub(crate) async fn roles(db: &Db, place: &Place) -> Result<Roles, DbError> {
    let listed = repo::roles_of(db, std::slice::from_ref(&place.channel.id)).await?;
    let owners = if place.channel.private {
        listed
            .iter()
            .filter(|role| role.owner)
            .map(|role| role.user_id.clone())
            .collect()
    } else {
        repo::roster(db, &place.team.id)
            .await?
            .into_iter()
            .filter(|member| member.owner)
            .map(|member| member.user_id)
            .collect()
    };
    Ok(Roles {
        owners,
        moderators: listed
            .into_iter()
            .filter(|role| role.moderator)
            .map(|role| role.user_id)
            .collect(),
    })
}

impl Roles {
    pub(crate) fn owner(&self, user: &str) -> bool {
        self.owners.iter().any(|owner| owner == user)
    }

    fn name(&self, user: &str) -> &'static str {
        if self.owner(user) { "Admin" } else { "User" }
    }
}

pub(super) fn thread(app: &App, tenant: &str, chat: &Chat, place: &Place, reader: &str, roles: &Roles) -> Value {
    let mut properties = shared_properties(place);
    properties["createdat"] = json!(chat.created_at.to_string());
    properties["creator"] = json!(format!("8:orgid:{}", chat.creator));
    properties["tenantid"] = json!(tenant);
    if place.channel.general() {
        properties["spaceAdminSettings"] = json!(place.team.settings);
        properties["RootResourceGroupId"] = json!(format!("92:{}", chat.id.trim_start_matches("19:")));
    }
    if let Some(settings) = &place.channel.settings {
        properties["channelSettings"] = json!(settings);
    }
    json!({
        "id": chat.id, "type": "Thread", "tenantId": tenant, "version": chat.version,
        "rosterVersion": chat.created_at, "eTag": format!("\"{}\"", chat.version),
        "messages": format!("{}/v1/users/ME/conversations/{}/messages", wire::base(app), chat.id),
        "properties": properties,
        "members": [{"id": format!("8:orgid:{reader}"), "role": roles.name(reader), "isFollowing": false,
            "isReader": false}],
    })
}

/// A thread's member list, with `selectMemberRoles` (`Admin`, `User|Guest`) applied when asked.
pub(super) fn members(app: &App, chat: &Chat, members: &[Member], roles: &Roles, select: Option<&str>) -> Value {
    let wanted: Option<Vec<&str>> = select.map(|select| select.split('|').collect());
    let listed: Vec<Value> = members
        .iter()
        .filter(|member| {
            wanted
                .as_ref()
                .is_none_or(|wanted| wanted.contains(&roles.name(&member.user_id)))
        })
        .map(|member| {
            let mut entry = wire::member(app, member, false);
            entry["role"] = json!(roles.name(&member.user_id));
            entry["isModerator"] = json!(roles.moderators.contains(&member.user_id));
            entry["shareHistoryTime"] = json!(0);
            entry
        })
        .collect();
    json!({
        "eTag": format!("\"{}\"", chat.version), "isDeleted": false, "members": listed,
        "rosterVersion": chat.version, "totalMemberCount": members.len(),
    })
}

/// A channel's entry in the account's conversations, over a chat's entry `entry`.
pub(super) fn conversation(entry: &mut Value, caller: &Caller, chat: &Chat, place: &Place, roles: &Roles) {
    let mut properties = shared_properties(place);
    properties["createdat"] = json!(chat.created_at.to_string());
    properties["isCreator"] = json!(chat.creator == caller.id);
    properties["lastjoinat"] = json!(chat.created_at.to_string());
    properties["originalThreadId"] = json!(chat.id);
    properties["rosterVersion"] = json!(chat.created_at);
    properties["version"] = json!(chat.version.to_string());
    entry["threadProperties"] = properties;
    entry["memberProperties"]["role"] = json!(roles.name(&caller.id));
}

/// The `ThreadUpdate` of `thread` for each of its people and for `gone` (someone just taken out of it or gone from
/// it), by the channel it is published on.
async fn updates(app: &App, tenant: &str, thread: &str, deleted: bool, gone: Option<&str>) -> Vec<(String, Value)> {
    let found = async {
        let Some(chat) = super::repo::chat(&app.db, thread).await? else {
            return Ok(None);
        };
        let Some(place) = place(&app.db, thread).await? else {
            return Ok(None);
        };
        let members = super::repo::members(&app.db, thread).await?;
        let roles = roles(&app.db, &place).await?;
        Ok::<_, DbError>(Some((chat, place, members, roles)))
    };
    let (chat, place, members, roles) = match found.await {
        Ok(Some(found)) => found,
        Ok(None) => return Vec::new(),
        Err(error) => {
            tracing::warn!(%error, "team event read failed; the teams read will catch up");
            return Vec::new();
        }
    };
    let mut readers: Vec<&str> = members.iter().map(|member| member.user_id.as_str()).collect();
    let gone = gone.filter(|gone| !readers.contains(gone));
    readers.extend(gone);
    readers
        .into_iter()
        .map(|reader| {
            let mut resource = self::thread(app, tenant, &chat, &place, reader, &roles);
            if deleted {
                resource["isDeleted"] = json!(true);
            }
            // out of the thread now: its thread read answers 403, so its update names no member
            if gone == Some(reader) {
                resource["members"] = json!([]);
            }
            (
                format!("push:8:orgid:{reader}"),
                json!({"resourceType": "ThreadUpdate", "resource": resource}),
            )
        })
        .collect()
}

pub(crate) async fn publish(app: &App, updates: Vec<(String, Value)>) {
    for (channel, payload) in updates {
        if app.fanout.publish(&channel, payload.to_string().into()).await.is_err() {
            tracing::warn!("team event fan-out unavailable; the teams read will catch up");
        }
    }
}

/// Tells everyone in `thread` that it changed (a team's or channel's name, people, state), as a chat's creation does.
pub(crate) async fn announce(app: &App, tenant: &str, thread: &str) {
    publish(app, updates(app, tenant, thread, false, None).await).await;
}

/// As [`announce`], `gone` (taken off a team or out of a private channel, or gone by themself) told too, so their
/// clients read their teams again and the team or channel leaves their lists.
pub(crate) async fn announce_with_gone(app: &App, tenant: &str, thread: &str, gone: &str) {
    publish(app, updates(app, tenant, thread, false, Some(gone)).await).await;
}

/// What tells everyone in `thread` that it is gone (`isDeleted`), read before the delete and published after it.
pub(crate) async fn farewell(app: &App, tenant: &str, thread: &str) -> Vec<(String, Value)> {
    updates(app, tenant, thread, true, None).await
}

type Params = HashMap<String, String>;

/// `PUT v1/threads/<thread>/properties?name=…`: a group chat's name (`topic`, [`super::topic`]), a team's settings
/// (`spaceAdminSettings`, on its general channel's thread) or a standard channel's moderation (`channelSettings`),
/// each the whole JSON as text, from a team owner.
pub(super) async fn set_property(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    Query(query): Query<Params>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<StatusCode, Error> {
    let thread = super::reads::target(&path)?;
    let (found, _) = member_of(&app, &caller, thread).await?;
    let name = query.get("name").map(String::as_str).ok_or(Error::BadRequest)?;
    let Json(body) = body.map_err(|_| Error::BadRequest)?;
    if name == "topic" {
        return super::topic::set(&app, &caller, &found.id, &body).await;
    }
    let place = place(&app.db, thread).await?.ok_or(Error::BadRequest)?;
    let text = body[name].as_str().ok_or(Error::BadRequest)?;
    let settings: Value = serde_json::from_str(text).map_err(|_| Error::BadRequest)?;
    let owner = repo::membership(&app.db, &place.team.id, &caller.id)
        .await?
        .is_some_and(|member| member.owner);
    if !owner || place.team.archived {
        return Err(Error::Denied);
    }
    match name {
        "spaceAdminSettings" if place.channel.general() && settings["userSettings"].is_object() => {
            let edit = repo::TeamEdit {
                settings: Some(text),
                ..repo::TeamEdit::NONE
            };
            repo::edit_team(&app.db, &place.team.id, &edit).await?;
        }
        "channelSettings"
            if settings.is_object()
                && !place.channel.general()
                && !place.channel.private
                && !place.channel.archived =>
        {
            let edit = repo::ChannelEdit {
                settings: Some(text),
                ..repo::ChannelEdit::NONE
            };
            repo::edit_channel(&app.db, &place.channel.id, &edit).await?;
        }
        _ => return Err(Error::BadRequest),
    }
    announce(&app, &caller.tenant, thread).await;
    Ok(StatusCode::OK)
}

/// `PUT v1/threads/<thread>/members/<own mri>` with `{"interest"}`: the account's own interest in a channel, sent
/// beside the `favorite` property that lists or hides it. Only the property is kept; nobody else's can be set.
pub(super) async fn set_interest(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<StatusCode, Error> {
    let thread = super::reads::target(&path)?;
    member_of(&app, &caller, thread).await?;
    if path.get("member") != Some(&caller.mri()) {
        return Err(Error::Denied);
    }
    let Json(body) = body.map_err(|_| Error::BadRequest)?;
    match body["interest"].as_str() {
        Some("Interested" | "NotInterested") if place(&app.db, thread).await?.is_some() => Ok(StatusCode::OK),
        _ => Err(Error::BadRequest),
    }
}
