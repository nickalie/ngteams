//! The admin API's app routes (`/api/admin/v1/apps…`, `docs/server.md` has the shapes): register a bot from the
//! form, upload or replace a Teams app package, read and list apps with their bot's delivery health, change who may
//! use one, its state or its bot's endpoint and mode, give its bot a new secret, send its bot a test activity
//! ([`crate::bots::test_activity`]), delete it. Admins only, as the account routes ([`Admin`]); each change is
//! audited. A bot's secret is in the answer that made it and nowhere else.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::repo::{self, Record};
use super::{AppError, AuthMode, BotForm, Changes, Registry};
use crate::admin::Actor;
use crate::admin::api::{Admin, ApiError};
use crate::app::App;

/// The largest JSON body these routes take.
const BODY_LIMIT: usize = 16 * 1024;

pub(super) fn router() -> Router<Arc<App>> {
    let packages = Router::new()
        .route("/api/admin/v1/apps/package", post(upload))
        .route("/api/admin/v1/apps/{id}/package", put(replace))
        .layer(DefaultBodyLimit::max(super::MAX_PACKAGE));
    Router::new()
        .route("/api/admin/v1/apps", get(list).post(register))
        .route("/api/admin/v1/apps/{id}", get(read).patch(update).delete(remove))
        .route("/api/admin/v1/apps/{id}/secret", post(rotate))
        .route("/api/admin/v1/apps/{id}/test", post(test))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .merge(packages)
}

fn refused(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::Refused {
        status,
        code,
        message: message.into(),
        retry_after: None,
    }
}

impl From<AppError> for ApiError {
    fn from(error: AppError) -> Self {
        let (status, code) = match error {
            AppError::Db(error) => return Self::from(error),
            AppError::Storage(_) | AppError::Random => {
                tracing::error!(%error, "admin API failed");
                return refused(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "ServerError",
                    "The server can't do this now.",
                );
            }
            AppError::NoSuchApp => (StatusCode::NOT_FOUND, "NotFound"),
            AppError::AppExists => (StatusCode::CONFLICT, "AppExists"),
            AppError::BotExists => (StatusCode::CONFLICT, "BotExists"),
            AppError::BotChanged => (StatusCode::BAD_REQUEST, "BotChanged"),
            AppError::NoBot => (StatusCode::BAD_REQUEST, "NoBot"),
            AppError::Name => (StatusCode::BAD_REQUEST, "InvalidName"),
            AppError::Endpoint => (StatusCode::BAD_REQUEST, "InvalidEndpoint"),
            AppError::EndpointNetwork => (StatusCode::BAD_REQUEST, "EndpointNotAllowed"),
            AppError::Unsigned => (StatusCode::BAD_REQUEST, "UnsignedNotAllowed"),
            AppError::UnknownUser => (StatusCode::BAD_REQUEST, "UnknownUser"),
            AppError::Package(_) => (StatusCode::BAD_REQUEST, "InvalidPackage"),
            AppError::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "PackageTooLarge"),
        };
        refused(status, code, error.to_string())
    }
}

fn answer(status: StatusCode, body: &Value) -> Response {
    (status, [(CACHE_CONTROL, "no-store")], axum::Json(body)).into_response()
}

/// A JSON body: anything that isn't the request's shape is a 400.
fn body<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(bytes).map_err(|error| refused(StatusCode::BAD_REQUEST, "InvalidRequest", error.to_string()))
}

/// An app as the API shows it: never its bot's secret.
pub(crate) fn app_json(record: &Record) -> Value {
    let app = &record.app;
    let bot = record.bot.as_ref().map(|bot| {
        json!({
            "id": bot.id,
            "mri": format!("28:{}", bot.id),
            "endpoint": bot.endpoint,
            "auth_mode": bot.auth_mode,
            "scopes": record.scopes(),
        })
    });
    json!({
        "id": app.id,
        "version": app.version,
        "name": app.name,
        "full_name": app.full_name,
        "short_description": app.short_description,
        "full_description": app.full_description,
        "developer": app.developer,
        "accent_color": app.accent_color,
        "source": app.source,
        "enabled": app.enabled,
        "everyone": app.everyone,
        "users": record.users,
        "manifest": serde_json::from_str::<Value>(&app.manifest).unwrap_or(Value::Null),
        "bot": bot,
        "created_at": app.created_at,
        "updated_at": app.updated_at,
    })
}

/// `app_json` with the secret shown once.
fn made(record: &Record, secret: Option<String>) -> Value {
    let mut json = app_json(record);
    if let Some(secret) = secret {
        json["secret"] = secret.into();
    }
    json
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterBody {
    name: String,
    #[serde(default)]
    description: String,
    endpoint: Option<String>,
    #[serde(default)]
    auth_mode: AuthMode,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateBody {
    enabled: Option<bool>,
    everyone: Option<bool>,
    users: Option<Vec<String>>,
    endpoint: Option<String>,
    auth_mode: Option<AuthMode>,
}

/// `records` as the API shows them, each with its bot's delivery health (none without a bot).
async fn with_health(app: &App, records: &[Record]) -> Result<Vec<Value>, ApiError> {
    let bots: Vec<String> = records
        .iter()
        .filter_map(|record| record.bot.as_ref().map(|bot| bot.id.clone()))
        .collect();
    let health = crate::bots::health::of(&app.db, &bots).await?;
    let now = crate::chatsvc::now();
    Ok(records
        .iter()
        .map(|record| {
            let mut json = app_json(record);
            json["health"] = record
                .bot
                .as_ref()
                .and_then(|bot| health.get(&bot.id))
                .map_or(Value::Null, |health| health.json(now));
            json
        })
        .collect())
}

/// `GET /api/admin/v1/apps`: every app, by name.
async fn list(State(app): State<Arc<App>>, Admin(_): Admin) -> Result<Response, ApiError> {
    let apps = with_health(&app, &repo::list(&app.db).await?).await?;
    Ok(answer(StatusCode::OK, &json!({ "apps": apps })))
}

/// `POST /api/admin/v1/apps`: a bot from the form.
async fn register(State(app): State<Arc<App>>, Admin(admin): Admin, bytes: Bytes) -> Result<Response, ApiError> {
    let request: RegisterBody = body(&bytes)?;
    let form = BotForm {
        name: request.name,
        description: request.description,
        endpoint: request.endpoint,
        auth_mode: request.auth_mode,
    };
    let (record, secret) = super::register(Registry::of(&app), Actor::account(&admin), &form).await?;
    Ok(answer(StatusCode::CREATED, &made(&record, Some(secret))))
}

/// `POST /api/admin/v1/apps/package`: the app a Teams package holds.
async fn upload(State(app): State<Arc<App>>, Admin(admin): Admin, package: Bytes) -> Result<Response, ApiError> {
    let (record, secret) = super::add_package(Registry::of(&app), Actor::account(&admin), package).await?;
    Ok(answer(StatusCode::CREATED, &made(&record, secret)))
}

/// `PUT /api/admin/v1/apps/{id}/package`: a new package of the app.
async fn replace(
    State(app): State<Arc<App>>,
    Admin(admin): Admin,
    Path(id): Path<String>,
    package: Bytes,
) -> Result<Response, ApiError> {
    let record = super::replace_package(Registry::of(&app), Actor::account(&admin), &id, package).await?;
    Ok(answer(StatusCode::OK, &app_json(&record)))
}

/// `GET /api/admin/v1/apps/{id}`.
async fn read(State(app): State<Arc<App>>, Admin(_): Admin, Path(id): Path<String>) -> Result<Response, ApiError> {
    let record = repo::get(&app.db, &id).await?.ok_or(AppError::NoSuchApp)?;
    let mut shown = with_health(&app, std::slice::from_ref(&record)).await?;
    Ok(answer(StatusCode::OK, &shown.pop().unwrap_or(Value::Null)))
}

/// `PATCH /api/admin/v1/apps/{id}`: its state, who may use it, its bot's endpoint and mode, each when given.
async fn update(
    State(app): State<Arc<App>>,
    Admin(admin): Admin,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Result<Response, ApiError> {
    let request: UpdateBody = body(&bytes)?;
    let changes = Changes {
        enabled: request.enabled,
        everyone: request.everyone,
        users: request.users,
        endpoint: request.endpoint,
        auth_mode: request.auth_mode,
    };
    let record = super::update(Registry::of(&app), Actor::account(&admin), &id, changes).await?;
    Ok(answer(StatusCode::OK, &app_json(&record)))
}

/// `POST /api/admin/v1/apps/{id}/secret`: a new secret for its bot, shown once; the old one stops working.
async fn rotate(
    State(app): State<Arc<App>>,
    Admin(admin): Admin,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let secret = super::rotate_secret(&app.db, Actor::account(&admin), &id).await?;
    Ok(answer(StatusCode::OK, &json!({ "secret": secret })))
}

/// `POST /api/admin/v1/apps/{id}/test`: a test activity to its bot; how it went.
async fn test(State(app): State<Arc<App>>, Admin(admin): Admin, Path(id): Path<String>) -> Result<Response, ApiError> {
    use crate::bots::ProbeError;
    let probe = crate::bots::test_activity(&app, &id, &admin)
        .await
        .map_err(|error| match error {
            ProbeError::Db(error) => ApiError::from(error),
            ProbeError::NoSuchApp => ApiError::from(AppError::NoSuchApp),
            ProbeError::NoBot => ApiError::from(AppError::NoBot),
            ProbeError::NoEndpoint => refused(StatusCode::BAD_REQUEST, "NoEndpoint", error.to_string()),
            ProbeError::Disabled => refused(StatusCode::CONFLICT, "AppDisabled", error.to_string()),
            ProbeError::Limited(wait) => ApiError::Refused {
                status: StatusCode::TOO_MANY_REQUESTS,
                code: "TooManyRequests",
                message: error.to_string(),
                retry_after: Some(std::time::Duration::from_secs(crate::bots::retry_after(wait))),
            },
            ProbeError::Token(_) => {
                tracing::error!(%error, "admin API failed");
                refused(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "ServerError",
                    "The server can't do this now.",
                )
            }
        })?;
    Ok(answer(StatusCode::OK, &probe.json()))
}

/// `DELETE /api/admin/v1/apps/{id}`.
async fn remove(
    State(app): State<Arc<App>>,
    Admin(admin): Admin,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    super::delete(Registry::of(&app), Actor::account(&admin), &id).await?;
    Ok((StatusCode::NO_CONTENT, [(CACHE_CONTROL, "no-store")]).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_of_the_server_is_a_500_that_says_nothing_of_it() {
        let failures = [
            AppError::Random,
            AppError::Storage(crate::storage::StorageError::Key),
            AppError::Db(crate::db::DbError::Conflict),
        ];
        for failure in failures {
            let response = ApiError::from(failure).into_response();
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        }
        let too_large = ApiError::from(AppError::TooLarge).into_response();
        assert_eq!(too_large.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
