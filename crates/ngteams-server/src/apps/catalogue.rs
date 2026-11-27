//! The app catalogue on the middle tier (`docs/protocol/apps.md`), in the shapes the client reads from Teams: the
//! store's apps (`beta/users/apps/store`), their definitions (`beta/users/apps/batchedDefinitions`), the apps added
//! for the account, to a team and to a chat (`…/aggregatedEntitlements`, `…/chatentitlements`), and each app's icons
//! on the server's own origin under the CDN's folder (`/teamsappdata/apps/<id>/color.png`), read with no token as
//! Teams' CDN is. An account sees only the apps it may use: enabled, and for everyone or for it.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use super::installs::{self, Scope};
use super::repo::{self, Record};
use crate::app::App;
use crate::auth::extract::Spaces;
use crate::config::Config;
use crate::db::{Db, DbError};
use crate::teams::Actor;

type Me = Actor<Spaces>;

/// The folder an app's icons are under on the origin, as on Teams' CDN.
pub(crate) const ICONS: &str = "teamsappdata";

/// The store categories the server lists its apps in: "Added by your org", and the one the client's "Add an app"
/// of a team reads.
const CATEGORIES: [&str; 2] = ["adminPreInstalledApps", "popularApps"];

/// The middle tier's catalogue routes, under its prefix.
pub(super) fn routes(config: &Config) -> Router<Arc<App>> {
    let mt = crate::services::middle_tier_prefix(config);
    Router::new()
        .route(&format!("{mt}/beta/users/apps/store"), post(store))
        .route(&format!("{mt}/beta/users/apps/batchedDefinitions"), post(definitions))
        .route(
            &format!("{mt}/beta/users/apps/aggregatedEntitlements"),
            post(user_entitlements),
        )
        .route(
            &format!("{mt}/beta/teams/apps/aggregatedEntitlements"),
            post(team_entitlements),
        )
        .route(
            &format!("{mt}/beta/chats/{{chat}}/apps/chatentitlements"),
            get(chat_entitlements),
        )
        .route(&format!("/{ICONS}/apps/{{id}}/{{file}}"), get(icon))
        .merge(installs::routes(&mt))
}

/// A refusal, in the shape of the middle tier's.
#[derive(Debug)]
pub(super) enum Refusal {
    Code(StatusCode, &'static str),
    Database(DbError),
}

impl Refusal {
    pub(super) const BAD: Self = Self::Code(StatusCode::BAD_REQUEST, "BadRequest");
    pub(super) const MISSING: Self = Self::Code(StatusCode::NOT_FOUND, "NotFound");
    pub(super) const FORBIDDEN: Self = Self::Code(StatusCode::FORBIDDEN, "Forbidden");
}

impl From<DbError> for Refusal {
    fn from(error: DbError) -> Self {
        Self::Database(error)
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        match self {
            Self::Code(status, code) => (status, Json(json!({"errorCode": code, "message": code}))).into_response(),
            Self::Database(error) => {
                tracing::error!(%error, "app catalogue failure");
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            }
        }
    }
}

pub(super) type Answer<T = Response> = Result<T, Refusal>;

/// Whether `user` may use the app: it is enabled, and for everyone or for them.
pub(super) fn usable(record: &Record, user: &str) -> bool {
    record.app.enabled && (record.app.everyone || record.users.iter().any(|listed| listed == user))
}

/// The apps `user` may use, by name.
pub(super) async fn usable_apps(db: &Db, user: &str) -> Result<Vec<Record>, DbError> {
    let mut records = repo::list(db).await?;
    records.retain(|record| usable(record, user));
    Ok(records)
}

/// The app `id` when `user` may use it.
pub(super) async fn usable_app(db: &Db, user: &str, id: &str) -> Result<Option<Record>, DbError> {
    Ok(repo::get(db, id).await?.filter(|record| usable(record, user)))
}

/// An app's definition, as Teams' catalogue gives it, rendered from its manifest.
pub(crate) fn definition(config: &Config, record: &Record) -> Value {
    let manifest: Value = serde_json::from_str(&record.app.manifest).unwrap_or_default();
    let app = &record.app;
    let developer = Some(app.developer.as_str())
        .filter(|name| !name.is_empty())
        .or_else(|| manifest.pointer("/developer/name").and_then(Value::as_str))
        .unwrap_or_default();
    let mut definition = json!({
        "id": app.id, "externalId": null, "name": app.name, "version": app.version,
        "manifestVersion": manifest.get("manifestVersion").cloned().unwrap_or(Value::Null),
        "developerName": developer, "shortDescription": app.short_description,
        "longDescription": app.full_description, "accentColor": app.accent_color,
        "isCoreApp": false, "systemPackage": false, "isAppBlocked": false,
        "bots": bots(&manifest, record),
        "inputExtensions": extensions(&manifest),
        "staticTabs": listed(&manifest, "staticTabs"),
        "configurableTabs": listed(&manifest, "configurableTabs"),
        "permissions": listed(&manifest, "permissions"),
        "validDomains": listed(&manifest, "validDomains"),
    });
    let links = [
        ("developerUrl", "/developer/websiteUrl"),
        ("privacyUrl", "/developer/privacyUrl"),
        ("termsOfUseUrl", "/developer/termsOfUseUrl"),
    ];
    for (key, pointer) in links {
        if let Some(url) = manifest.pointer(pointer).and_then(Value::as_str) {
            definition[key] = url.into();
        }
    }
    if app.icons {
        definition["largeImageUrl"] = icon_url(config, record, "color").into();
        definition["smallImageUrl"] = icon_url(config, record, "outline").into();
    }
    definition
}

/// Where an app's icon (`color` or `outline`) is read, its version the app's last change.
fn icon_url(config: &Config, record: &Record, icon: &str) -> String {
    format!(
        "{}/{ICONS}/apps/{}/{icon}.png?v={}",
        config.origin(),
        record.app.id,
        record.app.updated_at
    )
}

fn listed(manifest: &Value, key: &str) -> Value {
    manifest
        .get(key)
        .filter(|value| value.is_array())
        .cloned()
        .unwrap_or_else(|| json!([]))
}

/// The manifest's bot as the catalogue names it (`id`, its scopes and command lists), with the scopes the registry
/// keeps for it.
fn bots(manifest: &Value, record: &Record) -> Value {
    let Some(bot) = &record.bot else {
        return json!([]);
    };
    let declared = manifest
        .get("bots")
        .and_then(Value::as_array)
        .and_then(|bots| {
            bots.iter()
                .find(|entry| entry["botId"].as_str() == Some(bot.id.as_str()))
        })
        .cloned()
        .unwrap_or_default();
    let mut entry = Map::new();
    entry.insert("id".into(), bot.id.clone().into());
    entry.insert("scopes".into(), json!(record.scopes()));
    for key in [
        "commandLists",
        "isNotificationOnly",
        "supportsFiles",
        "supportsCalling",
        "supportsVideo",
    ] {
        if let Some(value) = declared.get(key) {
            entry.insert(key.into(), value.clone());
        }
    }
    json!([entry])
}

/// The manifest's message extensions as the catalogue names them (`inputExtensions`).
fn extensions(manifest: &Value) -> Value {
    let Some(extensions) = manifest.get("composeExtensions").and_then(Value::as_array) else {
        return json!([]);
    };
    extensions
        .iter()
        .filter(|extension| extension.is_object())
        .map(|extension| {
            let mut entry = extension.clone();
            if entry.get("scopes").is_none() {
                entry["scopes"] = json!(["personal", "groupChat", "team"]);
            }
            entry
        })
        .collect()
}

fn listing(records: &[Record]) -> Value {
    let apps: Vec<Value> = records
        .iter()
        .map(|record| json!({"id": record.app.id, "version": record.app.version, "metaCapabilities": []}))
        .collect();
    let category = json!({"apps": apps, "totalAppsCount": apps.len(), "blockedAppsCount": 0});
    let categories: Map<String, Value> = CATEGORIES
        .iter()
        .map(|name| ((*name).to_owned(), category.clone()))
        .collect();
    json!({"appCategoriesDictionary": categories, "appMetaCapabilitiesDictionary": {}, "frequentCollaborators": []})
}

/// `POST beta/users/apps/store`: the apps the account may use, by name.
async fn store(State(app): State<Arc<App>>, me: Me) -> Answer<Json<Value>> {
    let records = usable_apps(&app.db, &me.id).await?;
    Ok(Json(listing(&records)))
}

/// `POST beta/users/apps/batchedDefinitions`: the definitions of the asked apps the account may use, in its order.
async fn definitions(State(app): State<Arc<App>>, me: Me, body: Bytes) -> Answer<Json<Value>> {
    let ids: Vec<String> = serde_json::from_slice(&body).map_err(|_| Refusal::BAD)?;
    let records = usable_apps(&app.db, &me.id).await?;
    let found: Vec<Value> = ids
        .iter()
        .filter_map(|id| records.iter().find(|record| record.app.id == *id))
        .map(|record| definition(&app.config, record))
        .collect();
    Ok(Json(Value::Array(found)))
}

/// An entitlement as the catalogue lists it.
fn entitlement(id: &str, state: &str, install: Option<&installs::Install>) -> Value {
    let mut value = json!({"id": id, "state": state});
    if let Some(install) = install {
        value["installedVersion"] = install.version.clone().into();
        value["createdTime"] = crate::chatsvc::iso(install.at * 1000).into();
        if let Some(by) = &install.by {
            value["installedBy"] = by.clone().into();
            value["installerIdentity"] = json!({"id": by, "type": "user"});
        }
    }
    value
}

/// `POST beta/users/apps/aggregatedEntitlements`: the account's apps (`Installed`, the rest it may use
/// `Featured`) and, for each team of the body the account is on, the team's; every app's definition.
async fn user_entitlements(State(app): State<Arc<App>>, me: Me, body: Bytes) -> Answer<Json<Value>> {
    let teams: Vec<String> = serde_json::from_slice(&body).map_err(|_| Refusal::BAD)?;
    let records = usable_apps(&app.db, &me.id).await?;
    let mine = installs::of(&app.db, Scope::Personal, &me.id).await?;
    let own: Vec<Value> = records
        .iter()
        .map(|record| {
            let install = mine.iter().find(|install| install.app == record.app.id);
            let state = if install.is_some() { "Installed" } else { "Featured" };
            entitlement(&record.app.id, state, install)
        })
        .collect();
    let mut answer = json!({
        "userEntitlements": {me.id.clone(): own},
        "definitions": defined(&app.config, &records),
        "userEntitlementsHash": hash(&mine),
    });
    if !teams.is_empty() {
        answer["teamsEntitlements"] = teams_of(&app, &me.id, &teams, &records).await?;
    }
    Ok(Json(json!({ "value": answer })))
}

/// `POST beta/teams/apps/aggregatedEntitlements`: for each team of the body the account is on, the apps added to it.
async fn team_entitlements(State(app): State<Arc<App>>, me: Me, body: Bytes) -> Answer<Json<Value>> {
    let teams: Vec<String> = serde_json::from_slice(&body).map_err(|_| Refusal::BAD)?;
    let records = usable_apps(&app.db, &me.id).await?;
    let entitled = teams_of(&app, &me.id, &teams, &records).await?;
    Ok(Json(json!({"value": {
        "teamsEntitlements": entitled,
        "definitions": defined(&app.config, &records),
    }})))
}

async fn teams_of(app: &App, user: &str, teams: &[String], records: &[Record]) -> Result<Value, DbError> {
    let mut entitled = Map::new();
    for team in teams {
        if crate::teams::repo::membership(&app.db, team, user).await?.is_none() {
            continue;
        }
        let added = installs::of(&app.db, Scope::Team, team).await?;
        let listed: Vec<Value> = added
            .iter()
            .filter(|install| records.iter().any(|record| record.app.id == install.app))
            .map(|install| entitlement(&install.app, "Installed", Some(install)))
            .collect();
        entitled.insert(team.clone(), Value::Array(listed));
    }
    Ok(Value::Object(entitled))
}

fn defined(config: &Config, records: &[Record]) -> Value {
    let definitions: Map<String, Value> = records
        .iter()
        .map(|record| (record.app.id.clone(), definition(config, record)))
        .collect();
    Value::Object(definitions)
}

/// A hash of what is installed, which changes when it does.
fn hash(installs: &[installs::Install]) -> String {
    let joined: Vec<String> = installs
        .iter()
        .map(|install| format!("{}@{}", install.app, install.at))
        .collect();
    super::digest(&joined.join(","))
}

/// `GET beta/chats/<chat>/apps/chatentitlements`: the apps added to a chat the account is in, with their definitions
/// (a list).
async fn chat_entitlements(State(app): State<Arc<App>>, me: Me, Path(chat): Path<String>) -> Answer<Json<Value>> {
    if !crate::chatsvc::service::members(&app.db, &chat).await?.contains(&me.id) {
        return Err(Refusal::FORBIDDEN);
    }
    let records = usable_apps(&app.db, &me.id).await?;
    let added = installs::of(&app.db, Scope::Chat, &chat).await?;
    let mut entitlements = Vec::new();
    let mut definitions = Vec::new();
    for install in &added {
        if let Some(record) = records.iter().find(|record| record.app.id == install.app) {
            entitlements.push(entitlement(&install.app, "Installed", Some(install)));
            definitions.push(definition(&app.config, record));
        }
    }
    Ok(Json(json!({"value": {
        "entitlements": entitlements,
        "chatEntitlementsHash": hash(&added),
        "definitions": definitions,
        "appDefinitionsHash": hash(&added),
    }})))
}

/// `GET /teamsappdata/apps/<id>/<color|outline>.png`: an enabled app's icon, with no token, as Teams' CDN serves it.
async fn icon(State(app): State<Arc<App>>, Path((id, file)): Path<(String, String)>) -> Response {
    let Some(icon) = file
        .strip_suffix(".png")
        .filter(|icon| matches!(*icon, "color" | "outline"))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let record = match repo::get(&app.db, &id).await {
        Ok(Some(record)) if record.app.enabled && record.app.icons => record,
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return Refusal::Database(error).into_response(),
    };
    match app.storage.get(&super::icon_key(&record.app.id, icon)).await {
        Ok(Some(bytes)) => (
            [(CONTENT_TYPE, "image/png"), (CACHE_CONTROL, "public, max-age=86400")],
            bytes,
        )
            .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::error!(%error, "an app's icon wasn't read");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}
