//! Chat service membership is checked against the database for every request, including kept sync links.
//! Database versions order changes across nodes; a page cursor keeps its upper bound so writes during paging
//! appear in the next sync. No client-supplied author or roster can grant access to another account.

mod activity;
pub(crate) mod call_events;
pub(crate) mod channels;
mod create;
mod events;
mod member_repo;
mod members;
pub(crate) use members::{BotSeat, seat_bot};
pub(crate) mod message_repo;
mod messages;
mod mutations;
mod properties;
mod property_repo;
mod reactions;
mod reads;
pub(crate) mod repo;
mod rich;
pub(crate) mod schema;
pub(crate) mod service;
mod streams;
mod topic;
mod wire;

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{FromRequestParts, Path};
use axum::http::{StatusCode, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde_json::json;

use crate::app::App;
use crate::auth::extract::{Bearer, Ic3};
use crate::db::{DbError, users};

pub(super) fn router() -> Router<Arc<App>> {
    Router::new()
        .route("/api/chatsvc/{region}/v1/threads", post(create::create))
        .route("/api/chatsvc/{region}/v1/threads/{chat}", get(reads::thread))
        .route(
            "/api/chatsvc/{region}/v1/threads/{chat}/members",
            get(reads::members).put(members::add),
        )
        .route(
            "/api/chatsvc/{region}/v1/threads/{chat}/members/{member}",
            put(channels::set_interest).delete(members::remove),
        )
        .route(
            "/api/chatsvc/{region}/v1/threads/{chat}/properties",
            put(channels::set_property),
        )
        .route(
            "/api/chatsvc/{region}/v1/users/{owner}/conversations",
            get(reads::conversations),
        )
        .route(
            "/api/chatsvc/{region}/v1/users/ME/conversations/{chat}/messages",
            get(messages::page).post(messages::post),
        )
        .route(
            "/api/chatsvc/{region}/v1/users/ME/conversations/{chat}/messages/{message}",
            put(mutations::edit).delete(mutations::delete).get(reads::missing_copy),
        )
        .route(
            "/api/chatsvc/{region}/v1/users/ME/conversations/{chat}/messages/{message}/properties",
            put(reactions::react).delete(reactions::react),
        )
        .route(
            "/api/chatsvc/{region}/v1/users/ME/conversations/{chat}/properties",
            put(properties::set_conversation),
        )
        .route(
            "/api/chatsvc/{region}/v1/users/ME/properties",
            get(properties::user).put(properties::set_user),
        )
        .route(
            "/api/chatsvc/{region}/v1/users/ME/conversations/{chat}",
            get(properties::conversation),
        )
        .route(
            "/api/chatsvc/{region}/v1/users/ME/conversations/{chat}/messages/epochTimeStamp/{time}",
            get(messages::around),
        )
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
}

struct Caller {
    id: String,
    tenant: String,
    name: String,
}

impl Caller {
    fn mri(&self) -> String {
        format!("8:orgid:{}", self.id)
    }
}

impl FromRequestParts<Arc<App>> for Caller {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, Self::Rejection> {
        let bearer = Bearer::<Ic3>::from_request_parts(parts, app)
            .await
            .map_err(IntoResponse::into_response)?;
        let Path(path) = Path::<HashMap<String, String>>::from_request_parts(parts, app)
            .await
            .map_err(IntoResponse::into_response)?;
        if path.get("region") != Some(&app.config.region) {
            return Err(Error::Missing.into_response());
        }
        let user = users::by_id(&app.db, &bearer.claims.oid)
            .await
            .map_err(|error| Error::from(error).into_response())?;
        let user = user
            .filter(|user| !user.disabled)
            .ok_or_else(|| Error::Denied.into_response())?;
        Ok(Self {
            id: bearer.claims.oid,
            tenant: bearer.claims.tid,
            name: user.display_name,
        })
    }
}

#[derive(Debug)]
enum Error {
    BadRequest,
    InvalidThread,
    Denied,
    Missing,
    Database(DbError),
}

impl From<DbError> for Error {
    fn from(error: DbError) -> Self {
        match error {
            DbError::Conflict => Self::BadRequest,
            other => Self::Database(other),
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::InvalidThread => (StatusCode::BAD_REQUEST, 201, "The Thread Id is invalid"),
            Self::BadRequest => (StatusCode::BAD_REQUEST, 201, "Invalid threadId or request"),
            Self::Denied => (StatusCode::FORBIDDEN, 209, "AclCheckFailed"),
            Self::Missing => (StatusCode::NOT_FOUND, 201, "ThreadNotFound"),
            Self::Database(error) => {
                tracing::error!(%error, "chat service database failure");
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        };
        (status, Json(json!({"errorCode": code, "message": message}))).into_response()
    }
}

/// The stored thread `id` names and its members, for one of them: `id` is a chat's or one of the caller's streams.
async fn member_of(app: &App, caller: &Caller, id: &str) -> Result<(repo::Chat, Vec<repo::Member>), Error> {
    let stored = streams::stored(id, caller)?;
    let chat = repo::chat(&app.db, &stored).await?.ok_or(Error::Missing)?;
    let members = repo::members(&app.db, &stored).await?;
    if !members.iter().any(|member| member.user_id == caller.id) {
        return Err(Error::Denied);
    }
    Ok((chat, members))
}

/// The stored thread `id` names, for someone with a seat in it: someone who left still reads what they could. What
/// they may read is the message query's own to decide (`message_repo::within`).
async fn reader_of(app: &App, caller: &Caller, id: &str) -> Result<repo::Chat, Error> {
    let stored = streams::stored(id, caller)?;
    let chat = repo::chat(&app.db, &stored).await?.ok_or(Error::Missing)?;
    if !repo::seated(&app.db, &stored, &caller.id).await? {
        return Err(Error::Denied);
    }
    Ok(chat)
}

pub(crate) fn now() -> i64 {
    wire::now()
}

/// A time of the chat service's clock as its messages write it.
pub(crate) fn iso(at: i64) -> String {
    wire::iso(at)
}

pub(crate) fn decode_properties(body: Option<String>) -> Result<serde_json::Map<String, serde_json::Value>, DbError> {
    property_repo::decode(body)
}

/// Sets one of `user`'s own properties of a thread it is in; whether it is in it.
pub(crate) async fn set_own_property(
    db: &crate::db::Db,
    thread: &str,
    user: &str,
    name: &str,
    value: &str,
) -> Result<bool, DbError> {
    let people = property_repo::set_conversation(db, thread, user, name, value).await?;
    Ok(people.is_some())
}
