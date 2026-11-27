//! A bot's OAuth connections, as an administrator keeps them (`/api/admin/v1/apps/{id}/connections…`): an identity
//! provider's authorize and token URLs, the client the server is there, and the scopes it asks for. The client
//! secret is sealed and never shown again; a change without one keeps it. Deleting a connection drops its sign-ins
//! and every person's token for it. Each change is audited, as the registry's are.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use super::schema::bot_connections;
use super::{Sealer, redirect_uri};
use crate::admin::Actor;
use crate::admin::api::{Admin, ApiError};
use crate::app::App;
use crate::apps::AppError;
use crate::apps::api::{answer, body};
use crate::config::Config;
use crate::db::schema::admin_audit;
use crate::db::{Db, DbError, now, with_conn};

/// The longest connection name.
const MAX_NAME: usize = 64;
/// The longest client id, secret or scope list.
const MAX_FIELD: usize = 2048;

/// A connection as the database keeps it.
#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = bot_connections)]
pub(crate) struct Connection {
    pub bot_id: String,
    pub name: String,
    pub authorize_url: String,
    pub token_url: String,
    pub client_id: String,
    /// Sealed for [`secret_context`].
    pub client_secret: Vec<u8>,
    /// Space-separated.
    pub scopes: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("bot_id", &self.bot_id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// What a connection's client secret is sealed for.
pub(super) fn secret_context(bot: &str, name: &str) -> String {
    format!("connection-secret|{bot}|{name}")
}

pub(super) fn router() -> Router<Arc<App>> {
    Router::new()
        .route("/api/admin/v1/apps/{id}/connections", get(list))
        .route(
            "/api/admin/v1/apps/{id}/connections/{name}",
            axum::routing::put(save).delete(remove),
        )
}

/// A connection as the API shows it: never its secret.
fn shown(config: &Config, connection: &Connection) -> Value {
    json!({
        "name": connection.name,
        "authorize_url": connection.authorize_url,
        "token_url": connection.token_url,
        "client_id": connection.client_id,
        "scopes": connection.scopes,
        "redirect_uri": redirect_uri(config),
        "created_at": connection.created_at,
        "updated_at": connection.updated_at,
    })
}

/// The bot of the app `id`.
async fn bot_of(db: &Db, id: &str) -> Result<String, AppError> {
    let record = crate::apps::repo::get(db, id).await?.ok_or(AppError::NoSuchApp)?;
    Ok(record.bot.ok_or(AppError::NoBot)?.id)
}

/// The connection `name` of `bot`.
pub(super) async fn find(db: &Db, bot: &str, name: &str) -> Result<Option<Connection>, DbError> {
    with_conn!(db, |conn| Ok(bot_connections::table
        .find((bot, name))
        .select(Connection::as_select())
        .first(conn)
        .await
        .optional()?))
}

#[derive(Insertable)]
#[diesel(table_name = admin_audit)]
struct Entry<'a> {
    at: i64,
    actor_id: Option<&'a str>,
    actor: &'a str,
    action: &'a str,
    target_id: Option<&'a str>,
    target: &'a str,
    detail: String,
}

impl<'a> Entry<'a> {
    fn new(actor: Actor<'a>, action: &'a str, app_id: &'a str, connection: &str) -> Self {
        Self {
            at: now(),
            actor_id: actor.id,
            actor: actor.name,
            action,
            target_id: Some(app_id),
            target: app_id,
            detail: json!({ "connection": connection }).to_string(),
        }
    }
}

macro_rules! audit {
    ($conn:ident, $entry:expr) => {
        diesel::insert_into(admin_audit::table)
            .values($entry)
            .execute($conn)
            .await?
    };
}

/// `bot`'s connections, by name.
async fn all(db: &Db, bot: &str) -> Result<Vec<Connection>, DbError> {
    with_conn!(db, |conn| Ok(bot_connections::table
        .filter(bot_connections::bot_id.eq(bot))
        .order(bot_connections::name)
        .select(Connection::as_select())
        .load(conn)
        .await?))
}

/// `GET …/connections`, by name.
async fn list(State(app): State<Arc<App>>, Admin(_): Admin, Path(id): Path<String>) -> Result<Response, ApiError> {
    let bot = bot_of(&app.db, &id).await?;
    let rows = all(&app.db, &bot).await.map_err(AppError::from)?;
    let value: Vec<Value> = rows.iter().map(|row| shown(&app.config, row)).collect();
    Ok(answer(StatusCode::OK, &json!({ "value": value })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveBody {
    authorize_url: String,
    token_url: String,
    client_id: String,
    client_secret: Option<String>,
    #[serde(default)]
    scopes: String,
}

/// `PUT …/connections/{name}`: a new connection (201) or a change of one (200).
async fn save(
    State(app): State<Arc<App>>,
    Admin(admin): Admin,
    Path((id, name)): Path<(String, String)>,
    bytes: Bytes,
) -> Result<Response, ApiError> {
    let request: SaveBody = body(&bytes)?;
    let sealer = Sealer::of(&app.config).ok_or(AppError::SignInNotConfigured)?;
    let bot = bot_of(&app.db, &id).await?;
    let name = checked_name(&name)?;
    let authorize_url = checked_authorize(&app.config, &request.authorize_url)?;
    let token_url = crate::apps::checked_endpoint(&app.config, &request.token_url).map_err(|error| match error {
        AppError::Endpoint => AppError::Connection,
        other => other,
    })?;
    let client_id = field(&request.client_id)?;
    let scopes = request
        .scopes
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if scopes.len() > MAX_FIELD {
        return Err(AppError::Connection.into());
    }
    let existing = find(&app.db, &bot, &name).await.map_err(AppError::from)?;
    let client_secret = match (request.client_secret.as_deref(), &existing) {
        (Some(secret), _) => sealer
            .seal(&secret_context(&bot, &name), field(secret)?.as_bytes())
            .map_err(|_| AppError::Random)?,
        (None, Some(existing)) => existing.client_secret.clone(),
        (None, None) => return Err(AppError::Connection.into()),
    };
    let at = now();
    let row = Connection {
        bot_id: bot,
        name,
        authorize_url,
        token_url,
        client_id,
        client_secret,
        scopes,
        created_at: existing.as_ref().map_or(at, |existing| existing.created_at),
        updated_at: at,
    };
    let created = existing.is_none();
    write(&app.db, Actor::account(&admin), &id, &row, created).await?;
    let status = if created { StatusCode::CREATED } else { StatusCode::OK };
    Ok(answer(status, &shown(&app.config, &row)))
}

/// `DELETE …/connections/{name}`: with its sign-ins and tokens.
async fn remove(
    State(app): State<Arc<App>>,
    Admin(admin): Admin,
    Path((id, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let bot = bot_of(&app.db, &id).await?;
    let actor = Actor::account(&admin);
    let removed = delete(&app.db, actor, &id, &bot, &name)
        .await
        .map_err(AppError::from)?;
    if removed == 0 {
        return Err(AppError::NoSuchConnection.into());
    }
    tracing::info!(actor = actor.name, target = %id, connection = %name, "admin action app.connection.delete");
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn delete(db: &Db, actor: Actor<'_>, app_id: &str, bot: &str, name: &str) -> Result<usize, DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let removed = diesel::delete(bot_connections::table.find((bot, name)))
                .execute(conn)
                .await?;
            if removed > 0 {
                audit!(conn, &Entry::new(actor, "app.connection.delete", app_id, name));
            }
            Ok(removed)
        })
        .await)
}

async fn write(db: &Db, actor: Actor<'_>, app_id: &str, row: &Connection, created: bool) -> Result<(), AppError> {
    let action = if created {
        "app.connection.add"
    } else {
        "app.connection.change"
    };
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            diesel::delete(bot_connections::table.find((&row.bot_id, &row.name)))
                .execute(conn)
                .await?;
            diesel::insert_into(bot_connections::table)
                .values(row)
                .execute(conn)
                .await?;
            audit!(conn, &Entry::new(actor, action, app_id, &row.name));
            Ok(())
        })
        .await)?;
    tracing::info!(actor = actor.name, target = %app_id, connection = %row.name, "admin action {action}");
    Ok(())
}

fn checked_name(name: &str) -> Result<String, AppError> {
    let fits = !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    fits.then(|| name.to_owned()).ok_or(AppError::Connection)
}

/// Where a person's browser goes to sign in: an absolute `https` URL (or `http` where bots' endpoints may be) with
/// no credentials and no fragment.
fn checked_authorize(config: &Config, url: &str) -> Result<String, AppError> {
    let parsed = Url::parse(url.trim()).map_err(|_| AppError::Connection)?;
    let scheme = parsed.scheme() == "https" || (parsed.scheme() == "http" && config.bots.allow_http_endpoints);
    let bare = parsed.username().is_empty() && parsed.password().is_none() && parsed.fragment().is_none();
    if !(scheme && bare && parsed.has_host() && parsed.as_str().len() <= MAX_FIELD) {
        return Err(AppError::Connection);
    }
    Ok(parsed.to_string())
}

fn field(value: &str) -> Result<String, AppError> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_FIELD {
        return Err(AppError::Connection);
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_name_and_its_authorize_url_are_checked() {
        assert!(checked_name("github.work_1-a").is_ok());
        for bad in ["", "a b", "a/b", &"x".repeat(65)] {
            assert!(matches!(checked_name(bad), Err(AppError::Connection)), "{bad}");
        }
        let mut config = Config::default();
        assert!(checked_authorize(&config, "https://idp.example.test/authorize?x=1").is_ok());
        for bad in [
            "http://idp.example.test/authorize",
            "javascript:alert(1)",
            "https://u:p@idp.example.test/",
            "https://idp.example.test/#f",
            "relative",
        ] {
            assert!(checked_authorize(&config, bad).is_err(), "{bad}");
        }
        config.bots.allow_http_endpoints = true;
        assert!(checked_authorize(&config, "http://idp.example.test/authorize").is_ok());
        assert!(field(" ").is_err());
        assert_eq!(field(" id ").unwrap(), "id");
    }
}
