//! Apps and bots (`docs/server.md` "Apps and bots"): the app registry an administrator fills (a Teams app package,
//! or a bot from a form), the credentials a bot gets the server's token with (the client-credentials grant MSAL
//! sends, under a tenant's authority path and under `/adfs`), and the keys activities to a bot are signed with.
//!
//! A bot is a principal in chats: a `users` row of kind `app` with the bot's id (`28:<id>`), no password and no
//! sign-in, which no people read finds ([`crate::db::users::person`]). Its secret is server-made and high-entropy,
//! shown once, and kept only as its SHA-256, as refresh tokens are. The bot's own token never passes for a person's:
//! its audience is [`BotsConfig::scope`], which no person's token is issued for.

pub(crate) mod api;
mod catalogue;
pub mod cli;
mod credentials;
pub(crate) mod installs;
pub mod keys;
mod manifest;
pub mod repo;
pub(crate) mod schema;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use url::Url;

use self::repo::{AppRow, BotRow, Record};
use crate::admin::Actor;
use crate::app::App;
use crate::config::Config;
use crate::db::{Db, DbError, now};
use crate::storage::{Storage, StorageError};

pub use manifest::MAX_PACKAGE;

/// The GTM authz names while the server serves the app catalogue and bots: the middle tier, where the catalogue is.
pub const GTM: &str = "appsService";

/// The Bot Framework's resource, which a bot asks its token for by default.
pub const BOT_FRAMEWORK: &str = "https://api.botframework.com";
/// The longest app name (a Teams manifest's `name.short`), in characters.
pub const MAX_NAME: usize = 30;
/// The longest messaging endpoint.
const MAX_ENDPOINT: usize = 2048;
/// The scopes a bot from the form is offered in.
const FORM_SCOPES: [&str; 3] = ["personal", "team", "groupChat"];
/// The accent colour of a bot from the form: Teams' own.
const FORM_ACCENT: &str = "#5B5FC7";

/// `[apps]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AppsConfig {
    /// Whether the server serves apps and bots: the registry, a bot's credentials and the keys it checks with.
    pub enabled: bool,
}

impl Default for AppsConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// `[bots]`: how bots authenticate, both ways.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BotsConfig {
    /// The resource a bot asks its token for (`<scope>/.default`), and that token's audience. No person's token is
    /// ever issued for it.
    pub scope: String,
    /// The issuer of the tokens activities to bots carry; `<public_origin>/bot` when unset.
    pub issuer: Option<String>,
    /// The `serviceUrl` activities name, where a bot answers; `<public_origin>/bot/` when unset.
    pub service_url: Option<Url>,
    /// Seconds a bot's own token is valid.
    pub token_lifetime: u64,
    /// Seconds an activity's token is valid.
    pub activity_token_lifetime: u64,
    /// Seconds a bot signing key signs before a new one replaces it.
    pub key_rotation: u64,
    /// Whether a bot may be registered without authentication either way (development, or a private network):
    /// the server signs nothing to it and it takes no token.
    pub allow_unsigned: bool,
    /// Whether a bot's endpoint may be plain `http` (development only).
    pub allow_http_endpoints: bool,
    /// The networks a bot's endpoint may resolve into besides the public internet: private, loopback or link-local
    /// ranges where self-hosted bots live. A cloud metadata address is never reached ([`crate::outbound`]).
    pub allowed_networks: Vec<ipnet::IpNet>,
    /// Seconds before each new attempt of an activity a bot didn't take; one attempt more than there are entries.
    pub retry_delays: Vec<u64>,
    /// Seconds a node holds an activity it sends before another node may take it over.
    pub delivery_lease: u64,
    /// Seconds a request to a bot may take, from connecting to the end of its answer.
    pub send_timeout: u64,
    /// Activities a second the server sends one bot, on each node; a send over it waits ([`crate::bots`]).
    pub send_rate: u32,
    /// Activities the server may send one bot at once before [`Self::send_rate`] holds.
    pub send_burst: u32,
    /// Requests a second one bot may make to the connector, on each node; more get `429` with `Retry-After`.
    pub connector_rate: u32,
    /// Requests one bot may make to the connector at once before [`Self::connector_rate`] holds.
    pub connector_burst: u32,
    /// The key the people's tokens from a bot's OAuth connections and those connections' client secrets are sealed
    /// with (AES-256-GCM), 32 bytes in base64. Without it no connection is made and no one signs in to a bot.
    pub token_key: Option<crate::bots::sign_in::SealKey>,
}

impl Default for BotsConfig {
    fn default() -> Self {
        Self {
            scope: BOT_FRAMEWORK.to_owned(),
            issuer: None,
            service_url: None,
            token_lifetime: 3600,
            activity_token_lifetime: 3600,
            key_rotation: 30 * 86_400,
            allow_unsigned: false,
            allow_http_endpoints: false,
            allowed_networks: Vec::new(),
            retry_delays: vec![15, 45],
            delivery_lease: 30,
            send_timeout: 10,
            send_rate: 50,
            send_burst: 50,
            connector_rate: 50,
            connector_burst: 50,
            token_key: None,
        }
    }
}

impl BotsConfig {
    #[must_use]
    pub fn token_lifetime(&self) -> Duration {
        Duration::from_secs(self.token_lifetime)
    }

    #[must_use]
    pub fn activity_token_lifetime(&self) -> Duration {
        Duration::from_secs(self.activity_token_lifetime)
    }

    #[must_use]
    pub fn key_rotation(&self) -> Duration {
        Duration::from_secs(self.key_rotation)
    }

    /// Whether the values are usable: lifetimes above zero, an absolute `https` or `http` scope that names no
    /// resource a person's token is for ([`Config::person_audiences`]), and absolute issuer and service URLs.
    #[must_use]
    pub fn usable(&self, person_audiences: &[String]) -> bool {
        let scope = Url::parse(&self.scope).ok();
        let scope_ok = scope.is_some_and(|scope| matches!(scope.scheme(), "https" | "http") && scope.has_host())
            && !self.scope.ends_with('/')
            && !person_audiences
                .iter()
                .any(|audience| audience.trim_end_matches('/').eq_ignore_ascii_case(&self.scope));
        let issuer_ok = self.issuer.as_deref().is_none_or(|issuer| Url::parse(issuer).is_ok());
        let service_ok = self.service_url.as_ref().is_none_or(Url::has_host);
        let key_ok = self
            .token_key
            .as_ref()
            .is_none_or(crate::bots::sign_in::SealKey::usable);
        scope_ok
            && issuer_ok
            && key_ok
            && service_ok
            && self.token_lifetime > 0
            && self.activity_token_lifetime > 0
            && self.key_rotation > 0
            && self.send_timeout > 0
            && self.delivery_lease > self.send_timeout
            && [
                self.send_rate,
                self.send_burst,
                self.connector_rate,
                self.connector_burst,
            ]
            .iter()
            .all(|limit| *limit > 0)
    }
}

/// The `serviceUrl` activities to a bot name.
#[must_use]
pub fn service_url(config: &Config) -> String {
    config
        .bots
        .service_url
        .as_ref()
        .map_or_else(|| format!("{}/bot/", config.origin()), ToString::to_string)
}

/// The issuer of the tokens activities to a bot carry.
#[must_use]
pub fn issuer(config: &Config) -> String {
    config
        .bots
        .issuer
        .clone()
        .unwrap_or_else(|| format!("{}/bot", config.origin()))
}

/// The admin API's app routes and a bot's credential and key routes; nothing while apps are off.
pub(crate) fn router(config: &Config) -> Router<Arc<App>> {
    if !config.apps.enabled {
        return Router::new();
    }
    Router::new()
        .merge(api::router())
        .merge(credentials::router())
        .merge(catalogue::routes(config))
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("no such app")]
    NoSuchApp,
    #[error("an app with that id is registered")]
    AppExists,
    #[error("that bot id is taken")]
    BotExists,
    #[error("a new package of an app names the same bot")]
    BotChanged,
    #[error("the app has no bot")]
    NoBot,
    #[error("a name has 1 to {MAX_NAME} characters")]
    Name,
    #[error("a bot's endpoint is an absolute https URL without credentials")]
    Endpoint,
    #[error(
        "the endpoint's address is on a private, loopback, link-local or reserved network; the server sends to it \
         only when bots.allowed_networks names it"
    )]
    EndpointNetwork,
    #[error(
        "the endpoint's address is a cloud metadata address; the server never sends to it, and no setting opens it"
    )]
    EndpointMetadata,
    #[error("this server takes no bot without authentication (bots.allow_unsigned)")]
    Unsigned,
    #[error("no such user")]
    UnknownUser,
    #[error("the package isn't a Teams app package this server takes: {0}")]
    Package(String),
    #[error("the package is bigger than {MAX_PACKAGE} bytes")]
    TooLarge,
    #[error("the system's random source failed")]
    Random,
    #[error(
        "a connection is named with 1 to 64 letters, digits, '-', '_' or '.'; its authorize and token URLs are \
         absolute https URLs without credentials, and a new one has its client id and secret"
    )]
    Connection,
    #[error("no such connection")]
    NoSuchConnection,
    #[error("this server keeps no connection without bots.token_key")]
    SignInNotConfigured,
}

impl From<diesel::result::Error> for AppError {
    fn from(error: diesel::result::Error) -> Self {
        Self::Db(DbError::from(error))
    }
}

impl From<diesel_async::pooled_connection::bb8::RunError> for AppError {
    fn from(error: diesel_async::pooled_connection::bb8::RunError) -> Self {
        Self::Db(error.into())
    }
}

/// How a bot and the server authenticate each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// Each activity carries the server's token; the bot gets its own with its secret.
    #[default]
    Signed,
    /// Neither: the bot's own service URL carries its credential (development or a private network).
    Unsigned,
}

impl AuthMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Signed => "signed",
            Self::Unsigned => "unsigned",
        }
    }
}

/// What the registry works with: the database, the file store for icons, and the configuration.
#[derive(Clone, Copy)]
pub struct Registry<'a> {
    pub db: &'a Db,
    pub storage: &'a dyn Storage,
    pub config: &'a Config,
}

impl<'a> Registry<'a> {
    #[must_use]
    pub fn of(app: &'a App) -> Self {
        Self {
            db: &app.db,
            storage: app.storage.as_ref(),
            config: &app.config,
        }
    }
}

/// A bot from the form.
#[derive(Clone, Debug, Default)]
pub struct BotForm {
    pub name: String,
    pub description: String,
    pub endpoint: Option<String>,
    pub auth_mode: AuthMode,
}

/// What to change of an app; `None` leaves a value as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub enabled: Option<bool>,
    pub everyone: Option<bool>,
    pub users: Option<Vec<String>>,
    pub endpoint: Option<String>,
    pub auth_mode: Option<AuthMode>,
}

/// A new bot secret and the digest kept of it.
struct NewSecret {
    secret: String,
    digest: String,
}

fn new_secret() -> Result<NewSecret, AppError> {
    let mut bytes = [0_u8; 32];
    aws_lc_rs::rand::fill(&mut bytes).map_err(|_| AppError::Random)?;
    let secret = URL_SAFE_NO_PAD.encode(bytes);
    Ok(NewSecret {
        digest: digest(&secret),
        secret,
    })
}

/// How a secret is kept: its SHA-256, base64url.
pub(crate) fn digest(secret: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_bytes()))
}

/// An endpoint as it is kept, when the server may send to it.
pub(crate) fn checked_endpoint(config: &Config, endpoint: &str) -> Result<String, AppError> {
    let url = Url::parse(endpoint.trim()).map_err(|_| AppError::Endpoint)?;
    let scheme = url.scheme() == "https" || (url.scheme() == "http" && config.bots.allow_http_endpoints);
    let bare = url.username().is_empty() && url.password().is_none() && url.fragment().is_none();
    if !(scheme && bare && url.has_host() && url.as_str().len() <= MAX_ENDPOINT) {
        return Err(AppError::Endpoint);
    }
    // an address written in it is checked now, as the guard checks it at each send; a name is checked then
    match crate::outbound::Guard::for_bots(&config.bots).check(&url) {
        Ok(()) => Ok(url.to_string()),
        Err(crate::outbound::OutboundError::Refused) => Err(AppError::EndpointNetwork),
        Err(crate::outbound::OutboundError::Metadata) => Err(AppError::EndpointMetadata),
        Err(_) => Err(AppError::Endpoint),
    }
}

fn checked_mode(config: &Config, mode: AuthMode) -> Result<AuthMode, AppError> {
    if mode == AuthMode::Unsigned && !config.bots.allow_unsigned {
        return Err(AppError::Unsigned);
    }
    Ok(mode)
}

fn checked_name(name: &str) -> Result<String, AppError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(AppError::Name);
    }
    Ok(name.to_owned())
}

/// The manifest a bot from the form stands for, as a package would carry it.
fn form_manifest(id: &str, name: &str, description: &str) -> Value {
    json!({
        "manifestVersion": "1.17",
        "version": "1.0.0",
        "id": id,
        "developer": {"name": "ngteams-server"},
        "name": {"short": name, "full": name},
        "description": {"short": description, "full": description},
        "accentColor": FORM_ACCENT,
        "bots": [{"botId": id, "scopes": FORM_SCOPES}],
    })
}

/// Registers a bot from the form: an app of its own with the bot's id; the app and the bot's secret, shown once.
///
/// # Errors
/// A name, endpoint or mode the server refuses, or a database failure.
pub async fn register(registry: Registry<'_>, actor: Actor<'_>, form: &BotForm) -> Result<(Record, String), AppError> {
    let name = checked_name(&form.name)?;
    let endpoint = form
        .endpoint
        .as_deref()
        .filter(|endpoint| !endpoint.trim().is_empty())
        .map(|endpoint| checked_endpoint(registry.config, endpoint))
        .transpose()?;
    let auth_mode = checked_mode(registry.config, form.auth_mode)?;
    let id = uuid::Uuid::new_v4().to_string();
    let description = form.description.trim();
    let at = now();
    let app = AppRow {
        id: id.clone(),
        version: "1.0.0".to_owned(),
        name: name.clone(),
        full_name: name.clone(),
        short_description: description.to_owned(),
        full_description: description.to_owned(),
        developer: String::new(),
        accent_color: FORM_ACCENT.to_owned(),
        manifest: form_manifest(&id, &name, description).to_string(),
        source: "form".to_owned(),
        icons: false,
        everyone: true,
        enabled: true,
        created_at: at,
        updated_at: at,
    };
    let secret = new_secret()?;
    let bot = BotRow {
        id: id.clone(),
        app_id: id,
        endpoint,
        secret_hash: secret.digest,
        auth_mode: auth_mode.as_str().to_owned(),
        scopes: json!(FORM_SCOPES).to_string(),
        created_at: at,
        updated_at: at,
    };
    let record = repo::insert(registry.db, actor, app, Some(bot)).await?;
    Ok((record, secret.secret))
}

/// Registers the app a Teams package holds, with its bot; the app and, when it has a bot, the bot's secret.
///
/// # Errors
/// [`AppError::Package`] for a package Teams wouldn't take, a taken app or bot id, a file store or database
/// failure.
pub async fn add_package(
    registry: Registry<'_>,
    actor: Actor<'_>,
    package: Bytes,
) -> Result<(Record, Option<String>), AppError> {
    let package = manifest::read(package).await?;
    let at = now();
    let app = package.row(at);
    let secret = package.bot.as_ref().map(|_| new_secret()).transpose()?;
    let bot = package.bot.as_ref().zip(secret.as_ref()).map(|(bot, secret)| BotRow {
        id: bot.id.clone(),
        app_id: app.id.clone(),
        endpoint: None,
        secret_hash: secret.digest.clone(),
        auth_mode: AuthMode::Signed.as_str().to_owned(),
        scopes: json!(bot.scopes).to_string(),
        created_at: at,
        updated_at: at,
    });
    if repo::get(registry.db, &app.id).await?.is_some() {
        return Err(AppError::AppExists);
    }
    store_icons(registry.storage, &app.id, &package).await?;
    let record = repo::insert(registry.db, actor, app, bot).await?;
    Ok((record, secret.map(|secret| secret.secret)))
}

/// Replaces an app's package with a new one of the same app and bot: its version, names, manifest and icons. The
/// bot keeps its endpoint, mode and secret.
///
/// # Errors
/// No such app, a package of another app, [`AppError::BotChanged`], what [`add_package`] refuses.
pub async fn replace_package(
    registry: Registry<'_>,
    actor: Actor<'_>,
    id: &str,
    package: Bytes,
) -> Result<Record, AppError> {
    let current = repo::get(registry.db, id).await?.ok_or(AppError::NoSuchApp)?;
    let package = manifest::read(package).await?;
    if package.id != current.app.id {
        return Err(AppError::Package("the package is of another app".to_owned()));
    }
    let bot = package.bot.as_ref().map(|bot| bot.id.as_str());
    if bot != current.bot.as_ref().map(|bot| bot.id.as_str()) {
        return Err(AppError::BotChanged);
    }
    store_icons(registry.storage, id, &package).await?;
    let scopes = package.bot.as_ref().map(|bot| json!(bot.scopes).to_string());
    repo::replace(registry.db, actor, package.row(now()), scopes).await
}

async fn store_icons(storage: &dyn Storage, id: &str, package: &manifest::Package) -> Result<(), StorageError> {
    storage.put(&icon_key(id, "color"), package.color.clone()).await?;
    storage.put(&icon_key(id, "outline"), package.outline.clone()).await
}

/// Where an app's icon (`color` or `outline`) is kept in the file store.
#[must_use]
pub fn icon_key(id: &str, icon: &str) -> String {
    format!("apps/{id}/{icon}.png")
}

/// Changes an app's state, its users, or its bot's endpoint or mode.
///
/// # Errors
/// No such app, a value the server refuses, [`AppError::NoBot`] for a bot setting of an app without one,
/// [`AppError::UnknownUser`], or a database failure.
pub async fn update(registry: Registry<'_>, actor: Actor<'_>, id: &str, changes: Changes) -> Result<Record, AppError> {
    let changes = Changes {
        endpoint: changes
            .endpoint
            .as_deref()
            .map(|endpoint| checked_endpoint(registry.config, endpoint))
            .transpose()?,
        auth_mode: changes
            .auth_mode
            .map(|mode| checked_mode(registry.config, mode))
            .transpose()?,
        ..changes
    };
    repo::update(registry.db, actor, id, &changes).await
}

/// Gives an app's bot a new secret, which replaces the old one at once; the secret, shown once.
///
/// # Errors
/// No such app, [`AppError::NoBot`], or a database failure.
pub async fn rotate_secret(db: &Db, actor: Actor<'_>, id: &str) -> Result<String, AppError> {
    let secret = new_secret()?;
    repo::set_secret(db, actor, id, &secret.digest).await?;
    Ok(secret.secret)
}

/// Deletes an app: its bot no longer gets a token, and its row stays for what it posted. Its icons go from the
/// file store after.
///
/// # Errors
/// No such app, or a database failure.
pub async fn delete(registry: Registry<'_>, actor: Actor<'_>, id: &str) -> Result<(), AppError> {
    let removed = repo::delete(registry.db, actor, id).await?;
    if removed.app.icons {
        for icon in ["color", "outline"] {
            if let Err(error) = registry.storage.delete(&icon_key(id, icon)).await {
                tracing::warn!(%error, "a deleted app's icon wasn't removed");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bot_scope_names_no_persons_audience_and_lifetimes_are_above_zero() {
        let people = vec![
            "https://api.spaces.skype.com".to_owned(),
            "https://presence.teams.microsoft.com/".to_owned(),
        ];
        assert!(BotsConfig::default().usable(&people));
        for scope in [
            "https://api.spaces.skype.com",
            "https://presence.teams.microsoft.com",
            "https://api.botframework.com/",
            "not a url",
            "ftp://bots.example.test",
        ] {
            let config = BotsConfig {
                scope: scope.to_owned(),
                ..BotsConfig::default()
            };
            assert!(!config.usable(&people), "{scope}");
        }
        let zero = BotsConfig {
            token_lifetime: 0,
            ..BotsConfig::default()
        };
        assert!(!zero.usable(&people));
        let issuer = BotsConfig {
            issuer: Some("bot".to_owned()),
            ..BotsConfig::default()
        };
        assert!(!issuer.usable(&people));
    }

    #[test]
    fn a_config_whose_bot_scope_is_the_origin_or_a_workflows_audience_is_refused() {
        let mut config = Config::default();
        assert!(config.check().is_ok());
        for scope in [
            config.origin(),
            config.origin().to_uppercase(),
            crate::workflows::audiences(&config)[0].clone(),
        ] {
            config.bots.scope = scope;
            assert!(
                matches!(config.check(), Err(crate::config::ConfigError::Bots)),
                "{}",
                config.bots.scope
            );
        }
    }

    #[test]
    fn issuer_and_service_url_follow_the_origin_unless_set() {
        let mut config = Config::default();
        assert_eq!(service_url(&config), "http://127.0.0.1:8080/bot/");
        assert_eq!(issuer(&config), "http://127.0.0.1:8080/bot");
        config.bots.issuer = Some("https://issuer.example.test".to_owned());
        config.bots.service_url = Some(Url::parse("https://teams.example.test/connector/").unwrap());
        assert_eq!(service_url(&config), "https://teams.example.test/connector/");
        assert_eq!(issuer(&config), "https://issuer.example.test");
    }

    #[test]
    fn a_secret_is_long_random_and_kept_only_as_its_digest() {
        let one = new_secret().unwrap();
        let two = new_secret().unwrap();
        assert_eq!(one.secret.len(), 43);
        assert_ne!(one.secret, two.secret);
        assert_eq!(one.digest, digest(&one.secret));
        assert_ne!(one.digest, one.secret);
    }
}
