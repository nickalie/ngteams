//! The middle tier's team routes (`beta/teams/…`): teams made, changed and deleted, their people, their codes and
//! join requests, standard channels, analytics and pictures.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use serde_json::{Value, json};

use super::repo::{self, Changed, Channel, ChannelEdit, Team, TeamEdit};
use super::{Actor, Answer, Error, Seat, body, channel_of, mri, object_id, text, wire};
use crate::app::App;
use crate::auth::extract::Spaces;
use crate::chatsvc::channels::{self, announce};
use crate::db::users;

type Me = Actor<Spaces>;
type Params = std::collections::HashMap<String, String>;

/// The largest team picture taken.
const PICTURE_LIMIT: usize = 4 * 1024 * 1024;

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/beta/teams/create", post(create))
        .route("/beta/teams/suggested", get(joinable))
        .route("/beta/teams/find", get(joinable))
        .route("/beta/teams/usergroups", get(|_: Me| async { Json(json!([])) }))
        .route("/beta/teams/joinByCode", post(join_by_code))
        .route("/beta/teams/joinRequest/{group}", post(join))
        .route("/beta/teams/sponsorJoinRequest/{group}", post(sponsor))
        .route("/beta/teams/findJoinRequests", post(join_requests))
        .route("/beta/teams/joinRequestAdminAction", post(answer_join_request))
        .route("/beta/teams/{id}", patch(edit))
        .route("/beta/teams/{id}/delete", axum::routing::delete(delete))
        .route(
            "/beta/teams/{id}/joinCode",
            get(code).put(reset_code).delete(remove_code),
        )
        .route("/beta/teams/{id}/bulkUpdateRoledMembers", put(add_members))
        .route("/beta/teams/{id}/members", put(update_member))
        .route("/beta/teams/{id}/channels", post(create_channel))
        .route(
            "/beta/teams/{id}/channels/{channel}",
            patch(edit_channel).delete(delete_channel),
        )
        .route(
            "/beta/teams/{id}/setDefaultChannel",
            put(show_by_default).delete(show_by_default),
        )
        .route(
            "/beta/teams/{id}/profilepicture",
            post(set_picture).layer(DefaultBodyLimit::max(PICTURE_LIMIT)),
        )
        .route("/beta/users/{own}/profilepicturev2/teams/{group}", get(picture))
        .route("/beta/users/searchV2", post(super::people::addable))
        .route("/beta/analytics/me/teams/summary-timeseries", get(all_analytics))
        .route("/beta/analytics/teams/{group}/summary-timeseries", get(team_analytics))
        .route(
            "/beta/analytics/teams/{group}/channels/{channel}/summary-timeseries",
            get(channel_analytics),
        )
}

const TEAM_TYPE: &str = "Microsoft.SkypeSpaces.MiddleTier.Models.TeamResponse";
const CHANNEL_TYPE: &str = "Microsoft.SkypeSpaces.MiddleTier.Models.ChannelResponse";

/// Whether a team's `accessType` is public (3) or private (1), the two the dialog offers.
fn public(access: &Value) -> Answer<bool> {
    match access.as_u64() {
        Some(1) => Ok(false),
        Some(3) => Ok(true),
        _ => Err(Error::BAD_REQUEST),
    }
}

async fn create(State(app): State<Arc<App>>, actor: Me, input: Result<Json<Value>, JsonRejection>) -> Answer {
    let input = body(input)?;
    let team = Team {
        id: super::thread_id(),
        group_id: uuid::Uuid::new_v4().to_string(),
        name: text(&input["displayName"], 256, true)?,
        description: text(&input["description"], 1024, false)?,
        public: public(&input["accessType"])?,
        archived: false,
        join_code: None,
        settings: wire::default_settings(),
        tag_settings: wire::DEFAULT_TAG_SETTINGS.to_owned(),
        picture_etag: String::new(),
        created_at: 0,
        version: 0,
    };
    let first = text(&input["topicThreadTopic"], 50, true)?;
    let team = repo::create_team(&app.db, &team, &first, &actor.id).await?;
    announce(&app, &actor.tenant, &team.id).await;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "value": {
                "objectId": team.id, "skypeThreadId": team.id, "displayName": team.name,
                "description": team.description, "accessType": if team.public { "Public" } else { "Private" },
                "siteInfo": {"groupId": team.group_id},
            },
            "type": TEAM_TYPE,
        })),
    )
        .into_response())
}

async fn edit(
    State(app): State<Arc<App>>,
    actor: Me,
    Path(id): Path<String>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let seat = Seat::of(&app.db, &actor.id, &id).await?;
    seat.owner()?;
    seat.active()?;
    let input = body(input)?;
    let name = input.get("displayName").map(|name| text(name, 256, true)).transpose()?;
    let description = text(&input["description"], 1024, false)?;
    let edit = TeamEdit {
        name: name.as_deref(),
        description: Some(&description),
        public: input.get("accessType").map(public).transpose()?,
        ..TeamEdit::NONE
    };
    repo::edit_team(&app.db, &id, &edit).await?;
    announce(&app, &actor.tenant, &id).await;
    Ok(Json(json!({"value": {"objectId": id}})))
}

async fn delete(State(app): State<Arc<App>>, actor: Me, Path(id): Path<String>) -> Answer<Json<Value>> {
    Seat::of(&app.db, &actor.id, &id).await?.owner()?;
    // its people are known only while it is there
    let last = channels::farewell(&app, &actor.tenant, &id).await;
    repo::delete_team(&app.db, &id).await?;
    channels::publish(&app, last).await;
    Ok(Json(json!({"value": true})))
}

async fn code(State(app): State<Arc<App>>, actor: Me, Path(id): Path<String>) -> Answer {
    let seat = Seat::of(&app.db, &actor.id, &id).await?;
    seat.owner()?;
    Ok(match seat.team.join_code {
        Some(code) => Json(json!(code)).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    })
}

async fn reset_code(State(app): State<Arc<App>>, actor: Me, Path(id): Path<String>) -> Answer<Json<Value>> {
    let seat = Seat::of(&app.db, &actor.id, &id).await?;
    seat.owner()?;
    seat.active()?;
    let code: String = uuid::Uuid::new_v4().simple().to_string().chars().take(16).collect();
    let edit = TeamEdit {
        join_code: Some(repo::Code::Set(&code)),
        ..TeamEdit::NONE
    };
    repo::edit_team(&app.db, &id, &edit).await?;
    Ok(Json(json!(code)))
}

async fn remove_code(State(app): State<Arc<App>>, actor: Me, Path(id): Path<String>) -> Answer<Json<Value>> {
    Seat::of(&app.db, &actor.id, &id).await?.owner()?;
    let edit = TeamEdit {
        join_code: Some(repo::Code::Gone),
        ..TeamEdit::NONE
    };
    repo::edit_team(&app.db, &id, &edit).await?;
    Ok(Json(json!({"value": true})))
}

/// The enabled account an MRI names.
async fn account(app: &App, mri: &str) -> Answer<Option<String>> {
    let Some(id) = object_id(mri) else {
        return Ok(None);
    };
    Ok(users::by_id(&app.db, id)
        .await?
        .filter(|user| !user.disabled)
        .map(|user| user.id))
}

/// `by` puts `user` on the team with the role, or gives someone on it the role; not when the team would lose its
/// last owner.
async fn place(app: &App, team: &str, (user, owner): (&str, bool), on_team: bool, by: &str) -> Answer<bool> {
    if on_team {
        let change = repo::change_member(&app.db, team, user, Some(owner), by).await?;
        return Ok(change.changed == Changed::Done);
    }
    put_member(app, team, user, owner, by).await?;
    Ok(true)
}

/// [`repo::put_member`], waking the bot senders when a bot of the team was told.
pub(super) async fn put_member(app: &App, team: &str, user: &str, owner: bool, by: &str) -> Answer<()> {
    if repo::put_member(&app.db, team, user, owner, by).await? {
        crate::bots::wake(app).await;
    }
    Ok(())
}

async fn add_members(
    State(app): State<Arc<App>>,
    actor: Me,
    Path(id): Path<String>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let seat = Seat::of(&app.db, &actor.id, &id).await?;
    seat.active()?;
    if !seat.member.owner && !seat.team.public {
        return Err(Error::FORBIDDEN);
    }
    let input = body(input)?;
    if input["groupId"].as_str() != Some(seat.team.group_id.as_str()) {
        return Err(Error::BAD_REQUEST);
    }
    let people = repo::roster(&app.db, &id).await?;
    let (mut added, mut failed) = (Vec::new(), Vec::new());
    for user in input["users"].as_array().ok_or(Error::BAD_REQUEST)? {
        let named = user["mri"].as_str().ok_or(Error::BAD_REQUEST)?;
        let owner = match user["role"].as_u64() {
            Some(0) => false,
            Some(1) => true,
            _ => return Err(Error::BAD_REQUEST),
        };
        let current = people
            .iter()
            .find(|person| Some(person.user_id.as_str()) == object_id(named));
        // only an owner makes owners or changes the role of someone already on the team
        let allowed = seat.member.owner || (!owner && current.is_none());
        let placed = match account(&app, named).await? {
            Some(user) if allowed => place(&app, &id, (&user, owner), current.is_some(), &actor.id).await?,
            _ => false,
        };
        if placed { &mut added } else { &mut failed }.push(json!({"mri": named}));
    }
    announce(&app, &actor.tenant, &id).await;
    Ok(Json(json!({"value": {"addedMembers": added, "failedMembers": failed}})))
}

async fn update_member(
    State(app): State<Arc<App>>,
    actor: Me,
    Path(id): Path<String>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let seat = Seat::of(&app.db, &actor.id, &id).await?;
    let input = body(input)?;
    let user = input["userMri"]
        .as_str()
        .and_then(object_id)
        .ok_or(Error::BAD_REQUEST)?;
    let own = user == actor.id;
    let role = match input["updateType"].as_str() {
        Some("Promoted") => Some(true),
        Some("Demoted") => Some(false),
        Some("Removed") => None,
        Some("Left" | "DemotedAndLeft") if own => None,
        _ => return Err(Error::BAD_REQUEST),
    };
    let leaving = own && role.is_none() && input["updateType"] != "Removed";
    if !leaving {
        seat.owner()?;
        seat.active()?;
    }
    let change = repo::change_member(&app.db, &id, user, role, &actor.id).await?;
    if change.bots {
        crate::bots::wake(&app).await;
    }
    let handed = change.handed;
    match change.changed {
        Changed::Done => {}
        Changed::NotMember => return Err(Error::NOT_FOUND),
        Changed::LastOwner => return Err(Error::LAST_OWNER),
    }
    for channel in &handed {
        announce(&app, &actor.tenant, channel).await;
    }
    if role.is_none() {
        channels::announce_with_gone(&app, &actor.tenant, &id, user).await;
    } else {
        announce(&app, &actor.tenant, &id).await;
    }
    Ok(Json(json!({"value": true})))
}

fn joinable_entry(team: &Team) -> Value {
    json!({
        "groupId": team.group_id, "teamId": team.id, "displayName": team.name, "description": team.description,
        "accessType": if team.public { 3 } else { 1 }, "pictureETag": team.picture_etag,
    })
}

async fn joinable(State(app): State<Arc<App>>, actor: Me, Query(query): Query<Params>) -> Answer<Json<Value>> {
    let keywords = query
        .get("keywords")
        .map(|keywords| keywords.trim())
        .filter(|k| !k.is_empty());
    let mut teams = repo::joinable(&app.db, &actor.id, keywords).await?;
    // the databases collate names differently; the list reads the same from either
    teams.sort_by_key(|team| team.name.to_lowercase());
    Ok(Json(
        json!({"value": teams.iter().map(joinable_entry).collect::<Vec<_>>()}),
    ))
}

/// Puts the caller on `team`, unless they are on it.
async fn enter(app: &App, actor: &Me, team: &Team) -> Answer<Json<Value>> {
    if team.archived {
        return Err(Error::ARCHIVED);
    }
    if repo::membership(&app.db, &team.id, &actor.id).await?.is_none() {
        put_member(app, &team.id, &actor.id, false, &actor.id).await?;
        announce(app, &actor.tenant, &team.id).await;
    }
    Ok(Json(
        json!({"value": {"teamId": team.id, "groupId": team.group_id, "joined": true}}),
    ))
}

async fn join(State(app): State<Arc<App>>, actor: Me, Path(group): Path<String>) -> Answer<Json<Value>> {
    let team = repo::team_by_group(&app.db, &group).await?.ok_or(Error::NOT_FOUND)?;
    if team.public {
        return enter(&app, &actor, &team).await;
    }
    // a private team is found only by someone who was told its group: the request waits for an owner
    if team.archived {
        return Err(Error::ARCHIVED);
    }
    if repo::membership(&app.db, &team.id, &actor.id).await?.is_none() {
        repo::request_join(&app.db, &team.id, &actor.id).await?;
    }
    Ok(Json(json!({"value": {"groupId": team.group_id, "joined": false}})))
}

async fn join_by_code(
    State(app): State<Arc<App>>,
    actor: Me,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let input = body(input)?;
    let code = input.as_str().map(str::trim).filter(|code| !code.is_empty());
    let team = repo::team_by_code(&app.db, code.ok_or(Error::BAD_REQUEST)?)
        .await?
        .ok_or(Error::Code(StatusCode::NOT_FOUND, "InvalidJoinCode"))?;
    enter(&app, &actor, &team).await
}

async fn sponsor(
    State(app): State<Arc<App>>,
    actor: Me,
    Path(group): Path<String>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let seat = Seat::of_group(&app.db, &actor.id, &group).await?;
    seat.active()?;
    let input = body(input)?;
    for named in input.as_array().ok_or(Error::BAD_REQUEST)? {
        let named = named.as_str().ok_or(Error::BAD_REQUEST)?;
        let user = account(&app, named).await?.ok_or(Error::BAD_REQUEST)?;
        if repo::membership(&app.db, &seat.team.id, &user).await?.is_none() {
            repo::request_join(&app.db, &seat.team.id, &user).await?;
        }
    }
    Ok(Json(json!({"value": true})))
}

async fn join_requests(
    State(app): State<Arc<App>>,
    actor: Me,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let input = body(input)?;
    let mut per_team = serde_json::Map::new();
    for group in input["GroupIdList"].as_array().ok_or(Error::BAD_REQUEST)? {
        let group = group.as_str().ok_or(Error::BAD_REQUEST)?;
        let seat = Seat::of_group(&app.db, &actor.id, group).await?;
        seat.owner()?;
        let asking = repo::join_requests(&app.db, &seat.team.id).await?;
        per_team.insert(
            group.to_owned(),
            asking
                .iter()
                .map(|user| json!({"userMri": mri(user), "groupId": group}))
                .collect(),
        );
    }
    Ok(Json(json!({"joinRequestsPerTeam": per_team})))
}

async fn answer_join_request(
    State(app): State<Arc<App>>,
    actor: Me,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let input = body(input)?;
    let group = input["GroupId"].as_str().ok_or(Error::BAD_REQUEST)?;
    let user = input["UserMri"]
        .as_str()
        .and_then(object_id)
        .ok_or(Error::BAD_REQUEST)?;
    let accept = match input["ActionType"].as_str() {
        Some("Accept") => true,
        Some("Reject") => false,
        _ => return Err(Error::BAD_REQUEST),
    };
    let seat = Seat::of_group(&app.db, &actor.id, group).await?;
    seat.owner()?;
    seat.active()?;
    if !repo::drop_join_request(&app.db, &seat.team.id, user).await? {
        return Err(Error::NOT_FOUND);
    }
    if accept {
        put_member(&app, &seat.team.id, user, false, &actor.id).await?;
        announce(&app, &actor.tenant, &seat.team.id).await;
    }
    Ok(Json(json!({"value": true})))
}

/// A channel's name as the dialog's rules take it: none of the characters a folder can't have, not `General`.
pub(super) fn channel_name(value: &Value) -> Answer<String> {
    let name = text(value, 50, true)?;
    let reserved = name.eq_ignore_ascii_case("general");
    if reserved
        || name.starts_with(['_', '.'])
        || name.ends_with('.')
        || name.contains("..")
        || name.contains([
            '~', '#', '%', '&', '*', '{', '}', '/', '\\', ':', '<', '>', '?', '+', '|', '\'', '"',
        ])
    {
        return Err(if reserved {
            Error::NAME_TAKEN
        } else {
            Error::BAD_REQUEST
        });
    }
    Ok(name)
}

/// A channel's layout (`chatModalityType`); the dialog's default when none is named.
pub(super) fn layout(value: &Value) -> Answer<String> {
    match value.as_str() {
        None => Ok("Conversational".to_owned()),
        Some(layout @ ("Conversational" | "PostReply")) => Ok(layout.to_owned()),
        Some(_) => Err(Error::BAD_REQUEST),
    }
}

/// The most standard channels a team has, and the most private ones, as Teams limits them.
const MAX_CHANNELS: usize = 200;
const MAX_PRIVATE_CHANNELS: usize = 30;

/// Makes the channel and lists it for its creator.
pub(super) async fn make_channel(app: &App, tenant: &str, creator: &str, channel: &Channel) -> Answer<Channel> {
    let existing = repo::channels_of(&app.db, std::slice::from_ref(&channel.team_id)).await?;
    let alike = existing.iter().filter(|other| other.private == channel.private).count();
    if channel.private && alike >= MAX_PRIVATE_CHANNELS {
        return Err(Error::Code(StatusCode::FORBIDDEN, "MaxPrivateChannelLimitExceeded"));
    }
    if alike >= MAX_CHANNELS {
        return Err(Error::Code(StatusCode::FORBIDDEN, "MaxChannelLimitExceeded"));
    }
    let made = match repo::create_channel(&app.db, channel, creator).await {
        Ok(made) => made,
        Err(crate::db::DbError::Conflict) => return Err(Error::NAME_TAKEN),
        Err(error) => return Err(error.into()),
    };
    crate::chatsvc::set_own_property(&app.db, &made.id, creator, "favorite", "true").await?;
    announce(app, tenant, &made.id).await;
    announce(app, tenant, &made.team_id).await;
    Ok(made)
}

async fn create_channel(
    State(app): State<Arc<App>>,
    actor: Me,
    Path(id): Path<String>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer {
    let seat = Seat::of(&app.db, &actor.id, &id).await?;
    seat.active()?;
    if !seat.may("createTopic") {
        return Err(Error::Code(StatusCode::FORBIDDEN, "NoPermissionToCreateChannel"));
    }
    let input = body(input)?;
    if input["groupId"].as_str() != Some(seat.team.group_id.as_str()) || input["channelType"] != "Standard" {
        return Err(Error::BAD_REQUEST);
    }
    let channel = Channel {
        id: super::thread_id(),
        team_id: id,
        group_id: None,
        name: channel_name(&input["displayName"])?,
        description: text(&input["description"], 1024, false)?,
        private: false,
        layout: layout(&input["chatModalityType"])?,
        archived: false,
        shown_by_default: false,
        settings: None,
        created_at: 0,
        version: 0,
    };
    let made = make_channel(&app, &actor.tenant, &actor.id, &channel).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "value": {
                "objectId": made.id, "createdDateTime": wire::iso(made.created_at), "displayName": made.name,
                "description": made.description, "groupId": seat.team.group_id, "channelState": "Active",
                "isFavoriteByDefault": false, "chatModalityType": made.layout,
            },
            "type": CHANNEL_TYPE,
        })),
    )
        .into_response())
}

/// A standard channel of the team `id` that isn't its general one, with the caller's seat.
async fn standard(app: &App, actor: &Me, id: &str, channel: &str) -> Answer<(Seat, Channel)> {
    let (seat, channel) = channel_of(&app.db, &actor.id, channel).await?;
    if channel.team_id != id || channel.private {
        return Err(Error::NOT_FOUND);
    }
    if channel.general() {
        return Err(Error::FORBIDDEN);
    }
    seat.active()?;
    Ok((seat, channel))
}

async fn edit_channel(
    State(app): State<Arc<App>>,
    actor: Me,
    Path((id, channel)): Path<(String, String)>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let (seat, channel) = standard(&app, &actor, &id, &channel).await?;
    if !seat.may("updateTopic") || channel.archived {
        return Err(Error::FORBIDDEN);
    }
    let input = body(input)?;
    rename(&app, &actor.tenant, &channel, &input).await
}

/// A channel's new name and description.
pub(super) async fn rename(app: &App, tenant: &str, channel: &Channel, input: &Value) -> Answer<Json<Value>> {
    let name = channel_name(&input["displayName"])?;
    let description = text(&input["description"], 1024, false)?;
    let edit = ChannelEdit {
        name: Some(&name),
        description: Some(&description),
        ..ChannelEdit::NONE
    };
    match repo::edit_channel(&app.db, &channel.id, &edit).await {
        Ok(()) => {}
        Err(crate::db::DbError::Conflict) => return Err(Error::NAME_TAKEN),
        Err(error) => return Err(error.into()),
    }
    announce(app, tenant, &channel.id).await;
    Ok(Json(
        json!({"value": {"objectId": channel.id, "displayName": name, "description": description}}),
    ))
}

async fn delete_channel(
    State(app): State<Arc<App>>,
    actor: Me,
    Path((id, channel)): Path<(String, String)>,
) -> Answer<Json<Value>> {
    let (seat, channel) = standard(&app, &actor, &id, &channel).await?;
    if !seat.may("deleteTopic") {
        return Err(Error::FORBIDDEN);
    }
    repo::delete_channel(&app.db, &channel).await?;
    announce(&app, &actor.tenant, &channel.team_id).await;
    Ok(Json(json!({"value": true})))
}

async fn show_by_default(
    State(app): State<Arc<App>>,
    actor: Me,
    method: axum::http::Method,
    Path(id): Path<String>,
) -> Answer<Json<Value>> {
    let (seat, channel) = channel_of(&app.db, &actor.id, &id).await?;
    seat.owner()?;
    seat.active()?;
    if channel.private || channel.general() {
        return Err(Error::BAD_REQUEST);
    }
    let edit = ChannelEdit {
        shown_by_default: Some(method == axum::http::Method::PUT),
        ..ChannelEdit::NONE
    };
    repo::edit_channel(&app.db, &channel.id, &edit).await?;
    Ok(Json(json!({"value": true})))
}

fn picture_key(group: &str) -> String {
    format!("teams/{group}/picture")
}

fn picture_type(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0xff, 0xd8, ..] => Some("image/jpeg"),
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        _ => None,
    }
}

fn unavailable(error: &dyn std::fmt::Display) -> Error {
    tracing::error!(%error, "team picture storage failure");
    Error::Code(StatusCode::SERVICE_UNAVAILABLE, "StorageUnavailable")
}

async fn set_picture(
    State(app): State<Arc<App>>,
    actor: Me,
    Path(group): Path<String>,
    Query(query): Query<Params>,
    bytes: Bytes,
) -> Answer {
    let seat = Seat::of_group(&app.db, &actor.id, &group).await?;
    seat.owner()?;
    seat.active()?;
    if query.get("threadId") != Some(&seat.team.id) {
        return Err(Error::BAD_REQUEST);
    }
    let kind = picture_type(&bytes).ok_or(Error::BAD_REQUEST)?;
    app.storage
        .put(&picture_key(&group), bytes.clone())
        .await
        .map_err(|error| unavailable(&error))?;
    let etag = crate::chatsvc::now().to_string();
    let edit = TeamEdit {
        picture_etag: Some(&etag),
        ..TeamEdit::NONE
    };
    repo::edit_team(&app.db, &seat.team.id, &edit).await?;
    Ok(([(CONTENT_TYPE, kind)], bytes).into_response())
}

async fn picture(State(app): State<Arc<App>>, actor: Me, Path((_, group)): Path<(String, String)>) -> Answer {
    let team = repo::team_by_group(&app.db, &group).await?.ok_or(Error::NOT_FOUND)?;
    if !team.public && repo::membership(&app.db, &team.id, &actor.id).await?.is_none() {
        return Err(Error::NOT_FOUND);
    }
    let stored = app.storage.get(&picture_key(&group)).await;
    let bytes = stored.map_err(|error| unavailable(&error))?.ok_or(Error::NOT_FOUND)?;
    let kind = picture_type(&bytes).unwrap_or("application/octet-stream");
    Ok(([(CONTENT_TYPE, kind)], bytes).into_response())
}

const DAY_MS: i64 = 86_400_000;

fn days(query: &Params) -> Answer<i64> {
    match query.get("timeperiod").map(String::as_str) {
        Some("last-seven-days") | None => Ok(7),
        Some("last-thirty-days") => Ok(30),
        Some("last-ninety-days") => Ok(90),
        Some(_) => Err(Error::BAD_REQUEST),
    }
}

/// What the messages of `threads` came to in the last `days` days: who wrote (by day too), posts, replies,
/// reactions and mentions.
async fn metrics(app: &App, threads: &[String], days: i64) -> Answer<Value> {
    let today = crate::chatsvc::now() / DAY_MS;
    let since = (today - days + 1) * DAY_MS;
    let rows = repo::message_bodies(&app.db, threads, since).await?;
    let mut by_day: BTreeMap<i64, BTreeSet<&str>> = (0..days).map(|day| (today - day, BTreeSet::new())).collect();
    let mut people = BTreeSet::new();
    let (mut posts, mut replies, mut reactions, mut mentions) = (0_u64, 0_u64, 0_usize, 0_usize);
    for (id, root, owner, body) in &rows {
        let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        if body["properties"].get("deletetime").is_some() {
            continue;
        }
        people.insert(owner.as_str());
        by_day.entry(id / DAY_MS).or_default().insert(owner);
        if *root == Some(*id) {
            posts += 1;
        } else {
            replies += 1;
        }
        let listed = |key: &str| -> usize {
            match &body["properties"][key] {
                Value::String(text) => serde_json::from_str::<Vec<Value>>(text).map_or(0, |list| list.len()),
                Value::Array(list) => list.len(),
                _ => 0,
            }
        };
        mentions += listed("mentions");
        reactions += body["properties"]["emotions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|emotion| emotion["users"].as_array().map_or(0, Vec::len))
            .sum::<usize>();
    }
    let series: Vec<Value> = by_day
        .iter()
        .map(|(day, people)| json!({"date": wire::iso(day * DAY_MS), "value": people.len()}))
        .collect();
    Ok(json!({
        "activeUsers": {"value": people.len(), "timeSeries": series},
        "guests": {"value": 0}, "postMessages": {"value": posts}, "replyMessages": {"value": replies},
        "reactions": {"value": reactions}, "mentions": {"value": mentions}, "meetingsOrganized": {"value": 0},
    }))
}

/// The threads of the team's channels the caller is in.
async fn threads(app: &App, user: &str, team: &Team) -> Answer<Vec<String>> {
    let channels = repo::channels_of(&app.db, std::slice::from_ref(&team.id)).await?;
    let ids: Vec<String> = channels.into_iter().map(|channel| channel.id).collect();
    Ok(repo::joined(&app.db, user, &ids).await?)
}

async fn summary(app: &App, user: &str, team: &Team, days: i64) -> Answer<Value> {
    let threads = threads(app, user, team).await?;
    Ok(json!({
        "id": team.group_id, "displayName": team.name,
        "privacy": if team.public { "Public" } else { "Private" },
        "metrics": metrics(app, &threads, days).await?,
    }))
}

async fn team_analytics(
    State(app): State<Arc<App>>,
    actor: Me,
    Path(group): Path<String>,
    Query(query): Query<Params>,
) -> Answer<Json<Value>> {
    let seat = Seat::of_group(&app.db, &actor.id, &group).await?;
    Ok(Json(
        json!({"teams": summary(&app, &actor.id, &seat.team, days(&query)?).await?}),
    ))
}

async fn all_analytics(State(app): State<Arc<App>>, actor: Me, Query(query): Query<Params>) -> Answer<Json<Value>> {
    let days = days(&query)?;
    let mut teams = Vec::new();
    for (team, _) in repo::memberships(&app.db, &actor.id).await?.iter().take(40) {
        teams.push(summary(&app, &actor.id, team, days).await?);
    }
    Ok(Json(json!({"teams": teams, "nextCursor": null})))
}

async fn channel_analytics(
    State(app): State<Arc<App>>,
    actor: Me,
    Path((group, channel)): Path<(String, String)>,
    Query(query): Query<Params>,
) -> Answer<Json<Value>> {
    let (seat, channel) = channel_of(&app.db, &actor.id, &channel).await?;
    if seat.team.group_id != group {
        return Err(Error::NOT_FOUND);
    }
    let metrics = metrics(&app, std::slice::from_ref(&channel.id), days(&query)?).await?;
    Ok(Json(
        json!({"channels": {"id": channel.id, "displayName": channel.name, "metrics": metrics}}),
    ))
}
