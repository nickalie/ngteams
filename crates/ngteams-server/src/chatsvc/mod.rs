//! Chat service membership is checked against the database for every request, including kept sync links.
//! Database versions order changes across nodes; a page cursor keeps its upper bound so writes during paging
//! appear in the next sync. No client-supplied author or roster can grant access to another account.

mod create;
mod reads;
mod repo;
mod schema;
mod wire;

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{FromRequestParts, Path};
use axum::http::{StatusCode, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use crate::app::App;
use crate::auth::extract::{Audience, Bearer};
use crate::db::{DbError, users};

struct Ic3;
impl Audience for Ic3 {
    const AUDIENCE: &'static str = "https://ic3.teams.office.com";
}

pub(super) fn router() -> Router<Arc<App>> {
    Router::new()
        .route("/api/chatsvc/{region}/v1/threads", post(create::create))
        .route("/api/chatsvc/{region}/v1/threads/{chat}", get(reads::thread))
        .route("/api/chatsvc/{region}/v1/threads/{chat}/members", get(reads::members))
        .route(
            "/api/chatsvc/{region}/v1/users/{owner}/conversations",
            get(reads::conversations),
        )
        .route(
            "/api/chatsvc/{region}/v1/users/ME/conversations/{chat}/messages",
            get(reads::empty_messages),
        )
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
}

struct Caller {
    id: String,
    tenant: String,
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
        if user.is_none_or(|user| user.disabled) {
            return Err(Error::Denied.into_response());
        }
        Ok(Self {
            id: bearer.claims.oid,
            tenant: bearer.claims.tid,
        })
    }
}

#[derive(Debug)]
enum Error {
    BadRequest,
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

async fn member_of(app: &App, caller: &Caller, id: &str) -> Result<(repo::Chat, Vec<repo::Member>), Error> {
    let chat = repo::chat(&app.db, id).await?.ok_or(Error::Missing)?;
    let members = repo::members(&app.db, id).await?;
    if !members.iter().any(|member| member.user_id == caller.id) {
        return Err(Error::Denied);
    }
    Ok((chat, members))
}
