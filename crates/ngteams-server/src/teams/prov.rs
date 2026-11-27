//! The provisioning service's routes: private channels and their people, archiving a team or a channel, a
//! channel's moderators, a team's channel list, and what changes only the account's own view of a team.

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde_json::{Value, json};

use super::mt::{channel_name, layout, make_channel, rename};
use super::repo::{self, Changed, Channel, ChannelEdit, Team, TeamEdit};
use super::{Actor, Answer, Error, Seat, Templates, body, channel_of, channel_seat, text, wire};
use crate::app::App;
use crate::chatsvc::channels::announce;

type Me = Actor<Templates>;

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/teams/{group}/channels", post(create_channel))
        .route(
            "/teams/{group}/channels/{channel}",
            get(status).patch(edit_channel).delete(delete_channel),
        )
        .route(
            "/teams/{group}/channels/{channel}/users",
            post(put_members).patch(put_members),
        )
        .route("/teams/{group}/channels/{channel}/users/{user}", delete(remove_member))
        .route("/teams/{group}/channels/{channel}/archive", post(archive_channel))
        .route("/teams/{group}/channels/{channel}/unarchive", post(restore_channel))
        .route("/channels/{channel}/members", put(moderators))
        .route("/groups/{group}/archive", post(archive))
        .route("/groups/{group}/unarchive", post(restore))
        .route("/groups/{group}/channels", get(channels))
        .route("/groups/{group}/clone", post(clone))
        .route(
            "/users/me/invitations",
            get(|_: Me| async { Json(json!({"invitations": []})) }),
        )
        .route(
            "/teamtemplates/v1.0/en-us",
            get(|_: Me| async { Json(json!({"value": []})) }),
        )
        .route("/team/{team}/hide", put(hide))
        .route("/conversations/show", put(show))
}

fn oid(tenant: &str, id: &str) -> String {
    format!("OID:{id}@{tenant}")
}

/// The id an `OID:<id>@<tenant>` of this server's tenant names.
fn of_oid<'a>(tenant: &str, named: &'a str) -> Option<&'a str> {
    let (id, of) = named.strip_prefix("OID:")?.split_once('@')?;
    (of == tenant).then_some(id)
}

async fn create_channel(
    State(app): State<Arc<App>>,
    actor: Me,
    Path(group): Path<String>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let seat = Seat::of_group(&app.db, &actor.id, &group).await?;
    seat.active()?;
    if !(seat.may("createTopic") && seat.may("createPrivateSpace")) {
        return Err(Error::Code(
            StatusCode::FORBIDDEN,
            "CannotCreatePrivateChannelDueToUserPolicy",
        ));
    }
    let input = body(input)?;
    if input["spaceThreadId"].as_str() != Some(seat.team.id.as_str()) || input["channelType"] != "Private" {
        return Err(Error::BAD_REQUEST);
    }
    let channel = Channel {
        id: super::thread_id(),
        team_id: seat.team.id.clone(),
        group_id: Some(uuid::Uuid::new_v4().to_string()),
        name: channel_name(&input["displayName"])?,
        description: text(&input["description"], 1024, false)?,
        private: true,
        layout: layout(&input["chatModalityType"])?,
        archived: false,
        shown_by_default: false,
        settings: None,
        created_at: 0,
        version: 0,
    };
    let made = make_channel(&app, &actor.tenant, &actor.id, &channel).await?;
    Ok(Json(described(&actor.tenant, &seat.team, &made)))
}

fn described(tenant: &str, team: &Team, channel: &Channel) -> Value {
    json!({
        "groupId": oid(tenant, channel.group_id.as_deref().unwrap_or_default()), "threadId": channel.id,
        "displayName": channel.name, "description": channel.description, "hostTeamId": team.group_id,
        "hostTenantId": tenant, "createdTime": wire::iso(channel.created_at), "chatModalityType": channel.layout,
    })
}

/// A private channel of the team's group, by its own group, with the caller's seat: for its members alone.
async fn private(app: &App, actor: &Me, group: &str, named: &str) -> Answer<(Seat, Channel)> {
    let id = of_oid(&actor.tenant, named).ok_or(Error::NOT_FOUND)?;
    let channel = repo::channel_by_group(&app.db, id).await?.ok_or(Error::NOT_FOUND)?;
    let (seat, channel) = channel_seat(&app.db, &actor.id, channel).await?;
    if seat.team.group_id != group {
        return Err(Error::NOT_FOUND);
    }
    Ok((seat, channel))
}

async fn owns(app: &App, user: &str, channel: &Channel) -> Answer<()> {
    let roles = repo::roles_of(&app.db, std::slice::from_ref(&channel.id)).await?;
    if roles.iter().any(|role| role.owner && role.user_id == user) {
        Ok(())
    } else {
        Err(Error::FORBIDDEN)
    }
}

/// The private channel for one of its owners to change, in a team that isn't archived.
async fn owned(app: &App, actor: &Me, group: &str, named: &str) -> Answer<(Seat, Channel)> {
    let (seat, channel) = private(app, actor, group, named).await?;
    seat.active()?;
    owns(app, &actor.id, &channel).await?;
    Ok((seat, channel))
}

async fn status(
    State(app): State<Arc<App>>,
    actor: Me,
    Path((group, channel)): Path<(String, String)>,
) -> Answer<Json<Value>> {
    let (seat, channel) = private(&app, &actor, &group, &channel).await?;
    Ok(Json(described(&actor.tenant, &seat.team, &channel)))
}

async fn edit_channel(
    State(app): State<Arc<App>>,
    actor: Me,
    Path((group, channel)): Path<(String, String)>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let (_, channel) = owned(&app, &actor, &group, &channel).await?;
    if channel.archived {
        return Err(Error::FORBIDDEN);
    }
    let input = body(input)?;
    rename(&app, &actor.tenant, &channel, &input).await
}

async fn delete_channel(
    State(app): State<Arc<App>>,
    actor: Me,
    Path((group, channel)): Path<(String, String)>,
) -> Answer<Json<Value>> {
    let (_, channel) = owned(&app, &actor, &group, &channel).await?;
    repo::delete_channel(&app.db, &channel).await?;
    announce(&app, &actor.tenant, &channel.team_id).await;
    Ok(Json(json!({"value": true})))
}

fn failure(id: &str, code: &str) -> Value {
    json!({"id": id, "statusSubCode": code, "message": code})
}

async fn put_members(
    State(app): State<Arc<App>>,
    actor: Me,
    method: axum::http::Method,
    Path((group, channel)): Path<(String, String)>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let (seat, channel) = owned(&app, &actor, &group, &channel).await?;
    let input = body(input)?;
    let people = repo::roster(&app.db, &seat.team.id).await?;
    let inside = repo::roles_of(&app.db, std::slice::from_ref(&channel.id)).await?;
    let mut failed = Vec::new();
    for user in input["users"].as_array().ok_or(Error::BAD_REQUEST)? {
        let named = user["id"].as_str().ok_or(Error::BAD_REQUEST)?;
        let owner = match user["role"].as_u64() {
            Some(1) => false,
            Some(2) => true,
            _ => return Err(Error::BAD_REQUEST),
        };
        let id = of_oid(&actor.tenant, named).filter(|id| people.iter().any(|person| person.user_id == *id));
        let Some(id) = id else {
            // a private channel's people are people of its team
            failed.push(failure(named, "UserNotInTeam"));
            continue;
        };
        let member = inside.iter().any(|role| role.user_id == id);
        let changed = if method == axum::http::Method::POST || !member {
            if member || method != axum::http::Method::POST {
                failed.push(failure(
                    named,
                    if member {
                        "UserAlreadyInChannel"
                    } else {
                        "UserNotInChannel"
                    },
                ));
                continue;
            }
            repo::put_channel_member(&app.db, &channel, id, owner).await?;
            Changed::Done
        } else {
            repo::change_channel_member(&app.db, &channel, id, Some(owner)).await?
        };
        if changed != Changed::Done {
            failed.push(failure(named, "LastOwnerCannotBeRemoved"));
        }
    }
    announce(&app, &actor.tenant, &channel.id).await;
    Ok(Json(json!({"multipleStatus": failed, "asyncOperations": []})))
}

async fn remove_member(
    State(app): State<Arc<App>>,
    actor: Me,
    Path((group, channel, user)): Path<(String, String, String)>,
) -> Answer<Json<Value>> {
    let (seat, channel) = private(&app, &actor, &group, &channel).await?;
    seat.active()?;
    let id = of_oid(&actor.tenant, &user).ok_or(Error::BAD_REQUEST)?;
    if id != actor.id {
        owns(&app, &actor.id, &channel).await?;
    }
    match repo::change_channel_member(&app.db, &channel, id, None).await? {
        Changed::Done => {}
        Changed::NotMember => return Err(Error::NOT_FOUND),
        Changed::LastOwner => return Err(Error::LAST_OWNER),
    }
    super::folders::forget(&app.db, &actor.tenant, id, &channel.id).await;
    crate::chatsvc::channels::announce_with_gone(&app, &actor.tenant, &channel.id, id).await;
    Ok(Json(json!({"multipleStatus": [], "asyncOperations": []})))
}

/// Archives or restores the channel `thread` of the team's group: a team's owner for a standard channel, the
/// channel's for a private one, never the general channel.
async fn set_channel_archived(app: &App, actor: &Me, group: &str, thread: &str, archived: bool) -> Answer {
    let (seat, channel) = channel_of(&app.db, &actor.id, thread).await?;
    if seat.team.group_id != group {
        return Err(Error::NOT_FOUND);
    }
    seat.active()?;
    if channel.general() {
        return Err(Error::FORBIDDEN);
    }
    if channel.private {
        owns(app, &actor.id, &channel).await?;
    } else {
        seat.owner()?;
    }
    let edit = ChannelEdit {
        archived: Some(archived),
        ..ChannelEdit::NONE
    };
    repo::edit_channel(&app.db, &channel.id, &edit).await?;
    announce(app, &actor.tenant, &channel.id).await;
    Ok(StatusCode::ACCEPTED.into_response())
}

async fn archive_channel(
    State(app): State<Arc<App>>,
    actor: Me,
    Path((group, channel)): Path<(String, String)>,
) -> Answer {
    set_channel_archived(&app, &actor, &group, &channel, true).await
}

async fn restore_channel(
    State(app): State<Arc<App>>,
    actor: Me,
    Path((group, channel)): Path<(String, String)>,
) -> Answer {
    set_channel_archived(&app, &actor, &group, &channel, false).await
}

async fn moderators(
    State(app): State<Arc<App>>,
    actor: Me,
    Path(channel): Path<String>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let (seat, channel) = channel_of(&app.db, &actor.id, &channel).await?;
    seat.owner()?;
    seat.active()?;
    if channel.private || channel.general() || channel.archived {
        return Err(Error::FORBIDDEN);
    }
    let input = body(input)?;
    let people = repo::roster(&app.db, &seat.team.id).await?;
    let mut failed = Vec::new();
    for user in input["users"].as_array().ok_or(Error::BAD_REQUEST)? {
        let id = user["id"].as_str().ok_or(Error::BAD_REQUEST)?;
        let moderator = user["isModerator"].as_bool().ok_or(Error::BAD_REQUEST)?;
        if people.iter().any(|person| person.user_id == id) {
            repo::set_moderator(&app.db, &channel.id, id, moderator).await?;
        } else {
            failed.push(failure(id, "UserNotInTeam"));
        }
    }
    announce(&app, &actor.tenant, &channel.id).await;
    Ok(Json(json!({"updatedMembersError": failed})))
}

async fn set_archived(app: &App, actor: &Me, group: &str, archived: bool) -> Answer {
    let seat = Seat::of_group(&app.db, &actor.id, group).await?;
    seat.owner()?;
    let edit = TeamEdit {
        archived: Some(archived),
        ..TeamEdit::NONE
    };
    repo::edit_team(&app.db, &seat.team.id, &edit).await?;
    announce(app, &actor.tenant, &seat.team.id).await;
    Ok(StatusCode::ACCEPTED.into_response())
}

async fn archive(State(app): State<Arc<App>>, actor: Me, Path(group): Path<String>) -> Answer {
    set_archived(&app, &actor, &group, true).await
}

async fn restore(State(app): State<Arc<App>>, actor: Me, Path(group): Path<String>) -> Answer {
    set_archived(&app, &actor, &group, false).await
}

async fn channels(State(app): State<Arc<App>>, actor: Me, Path(group): Path<String>) -> Answer<Json<Value>> {
    let seat = Seat::of_group(&app.db, &actor.id, &group).await?;
    let all = repo::channels_of(&app.db, std::slice::from_ref(&seat.team.id)).await?;
    let ids: Vec<String> = all.iter().map(|channel| channel.id.clone()).collect();
    let joined = repo::joined(&app.db, &actor.id, &ids).await?;
    let listed: Vec<Value> = all
        .iter()
        .filter(|channel| joined.contains(&channel.id))
        .map(|channel| {
            json!({
                "threadId": channel.id, "displayName": channel.name, "description": channel.description,
                "channelType": if channel.private { "Private" } else { "Standard" }, "channelState": "Active",
                "archiveState": if channel.archived { "Archived" } else { "None" },
                "lastActivity": wire::iso(channel.version),
            })
        })
        .collect();
    Ok(Json(json!({"channels": listed})))
}

/// A new team copied from one the caller owns: its General channel, and its standard channels, settings and people
/// when asked.
async fn clone(
    State(app): State<Arc<App>>,
    actor: Me,
    Path(group): Path<String>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let seat = Seat::of_group(&app.db, &actor.id, &group).await?;
    seat.owner()?;
    let input = body(input)?;
    let flag = |key: &str| input[key].as_bool() == Some(true);
    let public = match input["visibility"].as_str() {
        Some("Public") => true,
        Some("Private") => false,
        _ => return Err(Error::BAD_REQUEST),
    };
    let source = repo::channels_of(&app.db, std::slice::from_ref(&seat.team.id)).await?;
    let first = source.iter().find(|channel| channel.general());
    let team = Team {
        id: super::thread_id(),
        group_id: uuid::Uuid::new_v4().to_string(),
        name: text(&input["displayName"], 256, true)?,
        description: text(&input["description"], 1024, false)?,
        public,
        archived: false,
        join_code: None,
        settings: if flag("includeSettings") {
            seat.team.settings.clone()
        } else {
            wire::default_settings()
        },
        tag_settings: wire::DEFAULT_TAG_SETTINGS.to_owned(),
        picture_etag: String::new(),
        created_at: 0,
        version: 0,
    };
    let first = first.map_or("General", |channel| channel.name.as_str());
    let team = repo::create_team(&app.db, &team, first, &actor.id).await?;
    if flag("includeRoster") {
        for person in repo::roster(&app.db, &seat.team.id).await? {
            if person.user_id != actor.id {
                super::mt::put_member(&app, &team.id, &person.user_id, person.owner, &actor.id).await?;
            }
        }
    }
    if flag("includeChannels") {
        for channel in source.iter().filter(|channel| !channel.general() && !channel.private) {
            let copy = Channel {
                id: super::thread_id(),
                team_id: team.id.clone(),
                archived: false,
                settings: if flag("includeSettings") {
                    channel.settings.clone()
                } else {
                    None
                },
                ..channel.clone()
            };
            repo::create_channel(&app.db, &copy, &actor.id).await?;
        }
    }
    announce(&app, &actor.tenant, &team.id).await;
    Ok(Json(json!({
        "groupId": team.group_id, "threadId": team.id, "workflowId": uuid::Uuid::new_v4().to_string(),
    })))
}

async fn hide(State(app): State<Arc<App>>, actor: Me, Path(team): Path<String>) -> Answer {
    Seat::of(&app.db, &actor.id, &team).await?;
    repo::set_hidden(&app.db, &team, &actor.id, true).await?;
    Ok(StatusCode::ACCEPTED.into_response())
}

async fn show(
    State(app): State<Arc<App>>,
    actor: Me,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let input = body(input)?;
    let ids = |key: &str| -> Answer<Vec<String>> {
        input[key]
            .as_array()
            .ok_or(Error::BAD_REQUEST)?
            .iter()
            .map(|id| id.as_str().map(str::to_owned).ok_or(Error::BAD_REQUEST))
            .collect()
    };
    for team in ids("teamIds")? {
        Seat::of(&app.db, &actor.id, &team).await?;
        repo::set_hidden(&app.db, &team, &actor.id, false).await?;
    }
    for channel in ids("channelIds")? {
        let (_, channel) = channel_of(&app.db, &actor.id, &channel).await?;
        let name = if channel.general() {
            "isGeneralChannelFavorite"
        } else {
            "favorite"
        };
        crate::chatsvc::set_own_property(&app.db, &channel.id, &actor.id, name, "true").await?;
        super::listed(&app.db, &channel.id, &actor.id).await?;
    }
    Ok(Json(json!({"value": true})))
}
