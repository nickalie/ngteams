//! A team's webhooks on the middle tier (`beta/teams/<team>/outgoingWebhooks`, `docs/protocol/bots.md`
//! "Outgoing webhooks"): listed for the team's people, made, changed and removed by those who may add apps to it
//! (its owners, its members while the team's `uploadCustomApp` setting lets them, and the server's administrators).
//! The token shows once, in the answer that made it.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use base64::Engine as _;
use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};
use serde_json::{Value, json};

use super::schema::outgoing_webhooks;
use crate::app::App;
use crate::auth::extract::Spaces;
use crate::db::schema::users;
use crate::db::{Db, DbError, with_conn};
use crate::teams::Actor;
use crate::teams::repo::{self, Team};

/// The longest name: Teams' own limit isn't documented.
const NAME_LIMIT: usize = 100;
const DESCRIPTION_LIMIT: usize = 1024;
/// The largest JSON body: a picture's base64 and the rest.
const BODY_LIMIT: usize = 4 * 1024 * 1024;

type Me = Actor<Spaces>;

pub(super) fn routes(config: &crate::config::Config) -> Router<Arc<App>> {
    let base = format!(
        "{}/beta/teams/{{team}}/outgoingWebhooks",
        crate::services::middle_tier_prefix(config)
    );
    Router::new()
        .route(&base, get(list).post(create))
        .route(&format!("{base}/{{id}}"), put(change).delete(remove))
        .route(&format!("{base}/{{id}}/picture"), get(picture))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

/// A refusal, in the shape of the middle tier's.
#[derive(Debug)]
pub(super) enum Refusal {
    Code(StatusCode, &'static str),
    Database(DbError),
    Storage(String),
}

impl Refusal {
    const BAD: Self = Self::Code(StatusCode::BAD_REQUEST, "BadRequest");
    const FORBIDDEN: Self = Self::Code(StatusCode::FORBIDDEN, "Forbidden");
    const MISSING: Self = Self::Code(StatusCode::NOT_FOUND, "NotFound");
    const ARCHIVED: Self = Self::Code(StatusCode::FORBIDDEN, "OperationNotAllowedOnArchivedTeam");
    const TAKEN: Self = Self::Code(StatusCode::CONFLICT, "OutgoingWebhookNameAlreadyExists");
    const CHANGED: Self = Self::Code(StatusCode::CONFLICT, "OutgoingWebhookChanged");
}

impl From<DbError> for Refusal {
    fn from(error: DbError) -> Self {
        match error {
            DbError::Conflict => Self::TAKEN,
            error => Self::Database(error),
        }
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        match self {
            Self::Code(status, code) => (status, Json(json!({"errorCode": code, "message": code}))).into_response(),
            Self::Database(error) => unavailable("database", &error),
            Self::Storage(error) => unavailable("picture storage", &error),
        }
    }
}

fn unavailable(what: &str, error: &dyn std::fmt::Display) -> Response {
    tracing::error!(%error, what, "outgoing webhook failure");
    StatusCode::SERVICE_UNAVAILABLE.into_response()
}

type Answer<T = Response> = Result<T, Refusal>;

/// A registered webhook.
#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = outgoing_webhooks)]
pub(crate) struct Hook {
    pub id: String,
    pub team_id: String,
    pub name: String,
    pub name_key: String,
    pub description: String,
    pub callback_url: String,
    pub security_key: String,
    pub picture_etag: Option<String>,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl std::fmt::Debug for Hook {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Hook")
            .field("id", &self.id)
            .field("team_id", &self.team_id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Hook {
    /// The HMAC key its token names.
    pub(crate) fn key(&self) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(&self.security_key)
            .unwrap_or_default()
    }
}

/// Webhook `id`, when one is registered.
pub(crate) async fn hook(db: &Db, id: &str) -> Result<Option<Hook>, DbError> {
    with_conn!(db, |conn| Ok(outgoing_webhooks::table
        .find(id)
        .select(Hook::as_select())
        .first(conn)
        .await
        .optional()?))
}

/// The caller's access to a team: whether they may change its webhooks.
struct Access {
    team: Team,
    manage: bool,
}

/// What `user` may do with `team`'s webhooks; someone on neither the team nor the server's administrators learns
/// only that it isn't there for them.
async fn access(db: &Db, user: &str, team: &str) -> Answer<Access> {
    let team = repo::team(db, team).await?.ok_or(Refusal::MISSING)?;
    let admin = crate::db::users::by_id(db, user)
        .await?
        .is_some_and(|account| account.is_admin);
    let member = repo::membership(db, &team.id, user).await?;
    let manage = match &member {
        None if !admin => return Err(Refusal::MISSING),
        None => true,
        Some(member) => admin || member.owner || crate::teams::wire::members_may(&team, "uploadCustomApp", true),
    };
    Ok(Access { team, manage })
}

impl Access {
    fn manager(self) -> Answer<Team> {
        if !self.manage {
            return Err(Refusal::FORBIDDEN);
        }
        if self.team.archived {
            return Err(Refusal::ARCHIVED);
        }
        Ok(self.team)
    }
}

/// A webhook as the middle tier shows it; its address only to those who may change it.
fn shown(app: &App, hook: &Hook, manage: bool) -> Value {
    let mut value = json!({
        "id": hook.id, "mri": format!("28:{}", hook.id), "teamId": hook.team_id,
        "name": hook.name, "description": hook.description, "canManage": manage,
        "createdBy": format!("8:orgid:{}", hook.created_by),
        "createdTime": crate::chatsvc::iso(hook.created_at),
    });
    if manage {
        value["callbackUrl"] = hook.callback_url.clone().into();
    }
    if let Some(etag) = &hook.picture_etag {
        value["pictureUrl"] = format!(
            "{}{}/beta/teams/{}/outgoingWebhooks/{}/picture?v={etag}",
            app.config.origin(),
            crate::services::middle_tier_prefix(&app.config),
            hook.team_id,
            hook.id
        )
        .into();
    }
    value
}

async fn team_hooks(db: &Db, team: &str) -> Result<Vec<Hook>, DbError> {
    with_conn!(db, |conn| Ok(outgoing_webhooks::table
        .filter(outgoing_webhooks::team_id.eq(team))
        .order((outgoing_webhooks::name_key.asc(), outgoing_webhooks::id.asc()))
        .select(Hook::as_select())
        .load(conn)
        .await?))
}

async fn list(State(app): State<Arc<App>>, actor: Me, Path(team): Path<String>) -> Answer<Json<Value>> {
    let access = access(&app.db, &actor.id, &team).await?;
    let hooks = team_hooks(&app.db, &access.team.id).await?;
    let shown: Vec<Value> = hooks.iter().map(|hook| shown(&app, hook, access.manage)).collect();
    Ok(Json(json!({ "value": shown })))
}

/// The fields a create or a change sends, checked; `None` where it sent none.
#[derive(Default)]
struct Form {
    name: Option<String>,
    callback_url: Option<String>,
    description: Option<String>,
    picture: Picture,
}

/// What a create or a change does with the picture.
#[derive(Default)]
enum Picture {
    #[default]
    Keep,
    Remove,
    Set(Bytes),
}

fn form(app: &App, input: Result<Json<Value>, JsonRejection>) -> Answer<Form> {
    let Json(input) = input.map_err(|_| Refusal::BAD)?;
    if !input.is_object() {
        return Err(Refusal::BAD);
    }
    let text = |key: &str, max: usize, empty: bool| -> Answer<Option<String>> {
        match input.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => {
                let text = text.trim();
                let length = text.chars().count();
                if length > max || (!empty && length == 0) {
                    return Err(Refusal::BAD);
                }
                Ok(Some(text.to_owned()))
            }
            Some(_) => Err(Refusal::BAD),
        }
    };
    let name = text("name", NAME_LIMIT, false)?;
    let description = text("description", DESCRIPTION_LIMIT, true)?;
    let callback_url = text("callbackUrl", 2048, false)?
        .map(|url| address(app, &url))
        .transpose()?;
    let picture = match input.get("picture") {
        None => Picture::Keep,
        Some(Value::Null) => Picture::Remove,
        Some(Value::String(encoded)) => Picture::Set(picture_of(app, encoded)?),
        Some(_) => return Err(Refusal::BAD),
    };
    Ok(Form {
        name,
        callback_url,
        description,
        picture,
    })
}

/// A callback address the server may send to, as the address guard reads it now.
fn address(app: &App, url: &str) -> Answer<String> {
    let parsed = url::Url::parse(url).map_err(|_| Refusal::BAD)?;
    app.outbound.check(&parsed).map_err(|_| Refusal::BAD)?;
    Ok(parsed.to_string())
}

/// A PNG or JPEG picture in base64, within the configured size.
fn picture_of(app: &App, encoded: &str) -> Answer<Bytes> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|_| Refusal::BAD)?;
    if bytes.len() > app.config.outgoing_webhooks.max_picture_bytes || picture_type(&bytes).is_none() {
        return Err(Refusal::BAD);
    }
    Ok(Bytes::from(bytes))
}

fn picture_type(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0xff, 0xd8, ..] => Some("image/jpeg"),
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        _ => None,
    }
}

fn picture_key(hook: &Hook) -> String {
    match &hook.picture_etag {
        Some(etag) if uuid::Uuid::parse_str(etag).is_ok() => format!("outgoing-webhooks/{}/pictures/{etag}", hook.id),
        _ => format!("outgoing-webhooks/{}/picture", hook.id),
    }
}

/// A new HMAC key, as the token shows it: 32 random bytes in base64.
fn new_key() -> Answer<String> {
    let mut bytes = [0_u8; 32];
    aws_lc_rs::rand::fill(&mut bytes).map_err(|_| Refusal::Storage("no random bytes".to_owned()))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

async fn create(
    State(app): State<Arc<App>>,
    actor: Me,
    Path(team): Path<String>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<(StatusCode, Json<Value>)> {
    let team = access(&app.db, &actor.id, &team).await?.manager()?;
    let form = form(&app, input)?;
    let (Some(name), Some(callback_url)) = (form.name, form.callback_url) else {
        return Err(Refusal::BAD);
    };
    let id = uuid::Uuid::new_v4().to_string();
    let at = crate::chatsvc::now();
    let picture = match form.picture {
        Picture::Set(bytes) => Some(bytes),
        Picture::Keep | Picture::Remove => None,
    };
    let hook = Hook {
        name_key: name.to_lowercase(),
        name,
        id,
        team_id: team.id,
        description: form.description.unwrap_or_default(),
        callback_url,
        security_key: new_key()?,
        picture_etag: picture.as_ref().map(|_| uuid::Uuid::new_v4().to_string()),
        created_by: actor.id,
        created_at: at,
        updated_at: at,
    };
    if let Some(bytes) = picture {
        store_picture(&app, &hook, bytes).await?;
    }
    if let Err(error) = insert(&app.db, &hook).await {
        drop_picture(&app, &hook).await;
        return Err(error.into());
    }
    let mut made = shown(&app, &hook, true);
    made["securityToken"] = hook.security_key.clone().into();
    Ok((StatusCode::CREATED, Json(json!({ "value": made }))))
}

/// Keeps `hook` and its account (`28:<id>`, an app's, which signs in nowhere) in one transaction.
async fn insert(db: &Db, hook: &Hook) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            diesel::insert_into(users::table)
                .values((
                    users::id.eq(&hook.id),
                    users::upn.eq(format!("webhook:{}", hook.id)),
                    users::display_name.eq(&hook.name),
                    users::password_hash.eq(""),
                    users::disabled.eq(true),
                    users::created_at.eq(hook.created_at),
                    users::updated_at.eq(hook.created_at),
                    users::kind.eq(crate::db::users::APP),
                ))
                .execute(conn)
                .await?;
            diesel::insert_into(outgoing_webhooks::table)
                .values(hook)
                .execute(conn)
                .await
                .map_err(crate::db::conflict)?;
            Ok(())
        })
        .await)
}

async fn store_picture(app: &App, hook: &Hook, bytes: Bytes) -> Answer<()> {
    app.storage
        .put(&picture_key(hook), bytes)
        .await
        .map_err(|error| Refusal::Storage(error.to_string()))
}

async fn drop_picture(app: &App, hook: &Hook) {
    if let Err(error) = app.storage.delete(&picture_key(hook)).await {
        tracing::warn!(%error, "an outgoing webhook's picture wasn't dropped");
    }
}

/// The team's webhook `id`.
async fn of_team(db: &Db, team: &str, id: &str) -> Answer<Hook> {
    hook(db, id)
        .await?
        .filter(|hook| hook.team_id == team)
        .ok_or(Refusal::MISSING)
}

async fn change(
    State(app): State<Arc<App>>,
    actor: Me,
    Path((team, id)): Path<(String, String)>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let team = access(&app.db, &actor.id, &team).await?.manager()?;
    let mut hook = of_team(&app.db, &team.id, &id).await?;
    let form = form(&app, input)?;
    let previous = hook.clone();
    let at = crate::chatsvc::now();
    if let Some(name) = form.name {
        hook.name_key = name.to_lowercase();
        hook.name = name;
    }
    if let Some(url) = form.callback_url {
        hook.callback_url = url;
    }
    if let Some(description) = form.description {
        hook.description = description;
    }
    change_picture(&app, &mut hook, form.picture).await?;
    hook.updated_at = at;
    let updated = update(&app.db, &hook, &previous).await;
    if !matches!(updated, Ok(true)) {
        if hook.picture_etag != previous.picture_etag && hook.picture_etag.is_some() {
            drop_picture(&app, &hook).await;
        }
        updated?;
        return Err(Refusal::CHANGED);
    }
    if hook.picture_etag != previous.picture_etag && previous.picture_etag.is_some() {
        drop_picture(&app, &previous).await;
    }
    Ok(Json(json!({ "value": shown(&app, &hook, true) })))
}

async fn change_picture(app: &App, hook: &mut Hook, picture: Picture) -> Answer<()> {
    match picture {
        Picture::Set(bytes) => {
            hook.picture_etag = Some(uuid::Uuid::new_v4().to_string());
            store_picture(app, hook, bytes).await?;
        }
        Picture::Remove => hook.picture_etag = None,
        Picture::Keep => {}
    }
    Ok(())
}

async fn update(db: &Db, hook: &Hook, previous: &Hook) -> Result<bool, DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let changed = diesel::update(
                outgoing_webhooks::table
                    .find(&hook.id)
                    .filter(outgoing_webhooks::name.eq(&previous.name))
                    .filter(outgoing_webhooks::description.eq(&previous.description))
                    .filter(outgoing_webhooks::callback_url.eq(&previous.callback_url))
                    .filter(outgoing_webhooks::updated_at.eq(previous.updated_at))
                    .filter(
                        outgoing_webhooks::picture_etag.eq(previous.picture_etag.as_deref()).or(
                            outgoing_webhooks::picture_etag
                                .is_null()
                                .and(previous.picture_etag.is_none().into_sql::<diesel::sql_types::Bool>()),
                        ),
                    ),
            )
            .set((
                outgoing_webhooks::name.eq(&hook.name),
                outgoing_webhooks::name_key.eq(&hook.name_key),
                outgoing_webhooks::description.eq(&hook.description),
                outgoing_webhooks::callback_url.eq(&hook.callback_url),
                outgoing_webhooks::picture_etag.eq(&hook.picture_etag),
                outgoing_webhooks::updated_at.eq(hook.updated_at),
            ))
            .execute(conn)
            .await
            .map_err(crate::db::conflict)?;
            if changed == 0 {
                return Ok(false);
            }
            diesel::update(users::table.find(&hook.id))
                .set((
                    users::display_name.eq(&hook.name),
                    users::updated_at.eq(hook.updated_at),
                ))
                .execute(conn)
                .await?;
            Ok(true)
        })
        .await)
}

async fn remove(
    State(app): State<Arc<App>>,
    actor: Me,
    Path((team, id)): Path<(String, String)>,
) -> Answer<StatusCode> {
    let team = access(&app.db, &actor.id, &team).await?.manager()?;
    let hook = of_team(&app.db, &team.id, &id).await?;
    let at = crate::chatsvc::now();
    if !delete(&app.db, &hook, at).await? {
        return Err(Refusal::CHANGED);
    }
    if hook.picture_etag.is_some() {
        drop_picture(&app, &hook).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Deletes `hook` only if its stored picture etag still matches, then marks its account deleted but keeps it for replies.
/// Returns `Ok(false)` without changes if the row is absent or its picture etag changed.
async fn delete(db: &Db, hook: &Hook, at: i64) -> Result<bool, DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let changed = diesel::delete(
                outgoing_webhooks::table.find(&hook.id).filter(
                    outgoing_webhooks::picture_etag.eq(hook.picture_etag.as_deref()).or(
                        outgoing_webhooks::picture_etag
                            .is_null()
                            .and(hook.picture_etag.is_none().into_sql::<diesel::sql_types::Bool>()),
                    ),
                ),
            )
            .execute(conn)
            .await?;
            if changed == 0 {
                return Ok(false);
            }
            diesel::update(users::table.find(&hook.id))
                .set((users::deleted_at.eq(Some(at)), users::updated_at.eq(at)))
                .execute(conn)
                .await?;
            Ok(true)
        })
        .await)
}

async fn picture(State(app): State<Arc<App>>, actor: Me, Path((team, id)): Path<(String, String)>) -> Answer {
    let access = access(&app.db, &actor.id, &team).await?;
    let hook = of_team(&app.db, &access.team.id, &id).await?;
    if hook.picture_etag.is_none() {
        return Err(Refusal::MISSING);
    }
    let stored = app.storage.get(&picture_key(&hook)).await;
    let bytes = stored
        .map_err(|error| Refusal::Storage(error.to_string()))?
        .ok_or(Refusal::MISSING)?;
    let kind = picture_type(&bytes).unwrap_or("application/octet-stream");
    Ok(([(CONTENT_TYPE, kind)], bytes).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_hook() -> Hook {
        Hook {
            id: "hook".to_owned(),
            team_id: "team".to_owned(),
            name: "Synthetic hook".to_owned(),
            name_key: "synthetic hook".to_owned(),
            description: String::new(),
            callback_url: "https://example.test/private".to_owned(),
            security_key: "synthetic-key".to_owned(),
            picture_etag: None,
            created_by: "author".to_owned(),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn hook_debug_does_not_show_its_key_or_callback_url() {
        let hook = synthetic_hook();
        let text = format!("{hook:?}");
        assert!(text.contains("Synthetic hook"));
        assert!(!text.contains(&hook.security_key));
        assert!(!text.contains(&hook.callback_url));
    }

    #[tokio::test]
    async fn a_stale_update_cannot_restore_a_previous_picture() -> Result<(), Box<dyn std::error::Error>> {
        use diesel_async::SimpleAsyncConnection as _;
        let dir = tempfile::tempdir()?;
        let db = Db::connect(&format!("sqlite://{}", dir.path().join("server.sqlite").display()), 1).await?;
        with_conn!(&db, |conn| conn.batch_execute("PRAGMA foreign_keys = OFF").await?);
        let mut current = synthetic_hook();
        insert(&db, &current).await?;
        for picture in [Some("first"), Some("second"), None] {
            let previous = current.clone();
            let mut stale = previous.clone();
            current.picture_etag = picture.map(str::to_owned);
            assert!(update(&db, &current, &previous).await?);
            stale.name = "Stale name".to_owned();
            stale.description = "Stale description".to_owned();
            assert!(!update(&db, &stale, &previous).await?);
            let kept = hook(&db, &current.id).await?.unwrap();
            assert_eq!(kept.picture_etag, current.picture_etag);
            assert_eq!(kept.description, current.description);
            assert_eq!(
                crate::db::users::by_id(&db, &current.id).await?.unwrap().display_name,
                current.name
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_stale_update_cannot_restore_other_webhook_fields() -> Result<(), Box<dyn std::error::Error>> {
        use diesel_async::SimpleAsyncConnection as _;
        let dir = tempfile::tempdir()?;
        let db = Db::connect(&format!("sqlite://{}", dir.path().join("server.sqlite").display()), 1).await?;
        with_conn!(&db, |conn| conn.batch_execute("PRAGMA foreign_keys = OFF").await?);
        let mut current = synthetic_hook();
        insert(&db, &current).await?;
        for field in ["name", "description", "callback_url"] {
            let previous = current.clone();
            let mut stale = previous.clone();
            match field {
                "name" => {
                    current.name = "Current name".to_owned();
                    current.name_key = current.name.to_lowercase();
                }
                "description" => current.description = "Current description".to_owned(),
                _ => current.callback_url = "https://example.test/current".to_owned(),
            }
            assert!(update(&db, &current, &previous).await?);
            stale.description = "Stale description".to_owned();
            assert!(!update(&db, &stale, &previous).await?);
            let kept = hook(&db, &current.id).await?.unwrap();
            assert_eq!(kept.name, current.name);
            assert_eq!(kept.description, current.description);
            assert_eq!(kept.callback_url, current.callback_url);
            assert_eq!(
                crate::db::users::by_id(&db, &current.id).await?.unwrap().display_name,
                current.name
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_stale_delete_keeps_the_current_picture_and_account() -> Result<(), Box<dyn std::error::Error>> {
        use diesel_async::SimpleAsyncConnection as _;
        let dir = tempfile::tempdir()?;
        let db = Db::connect(&format!("sqlite://{}", dir.path().join("server.sqlite").display()), 1).await?;
        with_conn!(&db, |conn| conn.batch_execute("PRAGMA foreign_keys = OFF").await?);
        let mut current = synthetic_hook();
        insert(&db, &current).await?;
        for picture in [Some("first"), Some("second"), None] {
            let previous = current.clone();
            current.picture_etag = picture.map(str::to_owned);
            assert!(update(&db, &current, &previous).await?);
            assert!(!delete(&db, &previous, 1).await?);
            let kept = hook(&db, &current.id).await?.unwrap();
            assert_eq!(kept.picture_etag, current.picture_etag);
            assert_eq!(
                crate::db::users::by_id(&db, &current.id).await?.unwrap().deleted_at,
                None
            );
        }
        assert!(delete(&db, &current, 2).await?);
        assert!(hook(&db, &current.id).await?.is_none());
        assert_eq!(
            crate::db::users::by_id(&db, &current.id).await?.unwrap().deleted_at,
            Some(2)
        );
        assert!(!delete(&db, &current, 3).await?);
        assert_eq!(
            crate::db::users::by_id(&db, &current.id).await?.unwrap().deleted_at,
            Some(2)
        );
        Ok(())
    }
}
