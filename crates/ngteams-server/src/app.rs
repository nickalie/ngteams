//! The state every request shares, the router over it, and the node's housekeeping.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::auth::keys::{KeyError, KeyRing};
use crate::auth::tokens::Issuer;
use crate::config::{Config, LoginConfig};
use crate::db::{Db, DbError};
use crate::fanout::journal::{Journal, Keeping, LocalJournal, RedisJournal};
use crate::fanout::keyspace::Keyspace;
use crate::fanout::{Fanout, FanoutError, Local, Redis};
use crate::storage::{ObjectStorage, Storage, StorageError};
use crate::throttle::Throttle;
use crate::trouter::Hub;
use crate::workflows::Limiter;

/// A node's fan-out, journal, sign-in and setup throttles and webhook limiter, in-process or on Redis.
type Started = (Arc<dyn Fanout>, Arc<dyn Journal>, Throttle, Throttle, Limiter);

/// The largest request body the auth endpoints take.
pub(crate) const AUTH_BODY_LIMIT: usize = 64 * 1024;
/// How long a request may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How often a node rotates keys when due and drops expired refresh tokens.
pub(crate) const MAINTENANCE: Duration = Duration::from_secs(3600);
/// Expired refresh tokens are kept this much longer (a late reuse still finds its family).
const EXPIRED_KEPT: Duration = Duration::from_hours(24);

/// What a node holds while it runs. Shared behind an `Arc`.
#[derive(Debug)]
pub struct App {
    pub config: Config,
    pub db: Db,
    pub issuer: Issuer,
    pub throttle: Throttle,
    /// First-admin setup attempts per client address.
    pub setup_throttle: Throttle,
    /// The fan-out; an account's deliverable events (`push:<mri>`) are also kept in the journal.
    pub fanout: Arc<dyn Fanout>,
    pub journal: Arc<dyn Journal>,
    pub trouter: Arc<Hub>,
    pub storage: Arc<dyn Storage>,
    pub presence: crate::presence::Presence,
    /// What reads pages and pictures for link previews.
    pub(crate) fetcher: crate::media::Fetcher,
    pub(crate) clearing: crate::media::Clearing,
    /// The media nodes a group call's room goes to; none installed, a group call isn't served.
    pub mixers: crate::calling::Mixers,
    /// Posts per workflow webhook.
    pub workflow_limit: crate::workflows::Limiter,
    /// The node's Redis command connection, with Redis.
    pub redis: Option<crate::fanout::keyspace::Shared>,
    /// The keys activities to bots are signed with.
    pub bot_keys: crate::apps::keys::BotKeys,
    /// What sends activities to bots, through the address guard.
    pub(crate) outbound: crate::outbound::Outbound,
    /// The server's sends to each bot (`bots.send_rate`).
    pub(crate) bot_sends: crate::bots::SendLimit,
}

/// Why a node can't start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Key(#[from] KeyError),
    #[error(transparent)]
    Fanout(#[from] FanoutError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("the link preview client can't be built: {0}")]
    Previews(String),
    #[error("the bot connector's client can't be built: {0}")]
    Outbound(String),
}

/// How long a retired key is kept: as long as the longest-lived thing it signed, an access token, a bot's token or a
/// file link, may still have to verify.
fn key_retention(config: &Config) -> Duration {
    let longest = config
        .tokens
        .access()
        .max(config.bots.token_lifetime())
        .max(Duration::from_secs(config.media.link_lifetime));
    // Allow two missed maintenance ticks before other nodes reload their signer.
    longest + 3 * MAINTENANCE + Duration::from_secs(crate::auth::tokens::LEEWAY)
}

/// What sends to bots, through the address guard `bots.*` sets.
fn outbound(config: &Config) -> Result<crate::outbound::Outbound, StartError> {
    let guard = crate::outbound::Guard {
        allow_http: config.bots.allow_http_endpoints,
        networks: config.bots.allowed_networks.clone(),
    };
    crate::outbound::Outbound::new(guard, Duration::from_secs(config.bots.send_timeout))
        .map_err(|error| StartError::Outbound(error.to_string()))
}

impl App {
    /// Starts a node that runs every role: see [`App::start_for`].
    ///
    /// # Errors
    /// See [`StartError`].
    pub async fn start(config: Config) -> Result<Arc<Self>, StartError> {
        Self::start_for(config, crate::mixer::Role::All).await
    }

    /// Connects to the database (migrating it), Redis (when configured) and the file store, and loads the keys.
    /// Only a node that serves the API makes or rotates the bot keys.
    ///
    /// # Errors
    /// See [`StartError`].
    pub async fn start_for(config: Config, role: crate::mixer::Role) -> Result<Arc<Self>, StartError> {
        let db = Db::connect(&config.database_url, config.database_pool).await?;
        let tenant = crate::db::meta::tenant(&db).await?;
        let keys = KeyRing::load(db.clone(), config.tokens.rotation(), key_retention(&config)).await?;
        let issuer = Issuer::new(keys, &config.origin(), &tenant, &config.region, config.tokens.access());
        let bot_keys = if role.serves_api() {
            crate::apps::keys::BotKeys::load(db.clone(), &config).await?
        } else {
            crate::apps::keys::BotKeys::read(db.clone(), &config).await?
        };
        let setup_limits = LoginConfig {
            max_failures: config.setup.max_attempts,
            window: config.setup.window,
        };
        let rate = config.workflows.rate_per_minute;
        let mut shared = None;
        let (fanout, journal, throttle, setup_throttle, workflow_limit): Started = match &config.redis_url {
            Some(url) => {
                let keys = Keyspace::of(config.redis_namespace.as_deref(), &tenant);
                let redis = Redis::connect(url, keys, config.redis_timeout()).await?;
                let throttle = Throttle::redis(config.login.clone(), redis.shared(), "login");
                let setup_throttle = Throttle::redis(setup_limits, redis.shared(), "setup");
                let journal = RedisJournal::new(redis.shared());
                let workflow_limit = Limiter::redis(redis.shared(), rate);
                shared = Some(redis.shared());
                (
                    Arc::new(redis),
                    Arc::new(journal),
                    throttle,
                    setup_throttle,
                    workflow_limit,
                )
            }
            None => (
                Arc::new(Local::new()),
                Arc::new(LocalJournal::new()),
                Throttle::local(config.login.clone()),
                Throttle::local(setup_limits),
                Limiter::local(rate),
            ),
        };
        let fanout: Arc<dyn Fanout> = Arc::new(Keeping::new(
            fanout,
            Arc::clone(&journal),
            crate::trouter::keep,
            crate::trouter::unkept,
        ));
        let presence =
            crate::presence::Presence::start(shared.clone(), Arc::clone(&fanout), db.clone(), config.presence.clone());
        let storage = Arc::new(ObjectStorage::open_with(
            &config.storage_url,
            &config.storage_options,
            &config.storage,
        )?);
        let fetcher =
            crate::media::Fetcher::new(&config.media).map_err(|error| StartError::Previews(error.to_string()))?;
        let outbound = outbound(&config)?;
        let bot_sends = crate::bots::SendLimit::new(&config.bots);
        let app = Arc::new(Self {
            config,
            db,
            issuer,
            throttle,
            setup_throttle,
            fanout,
            journal,
            trouter: Arc::new(Hub::new()),
            storage,
            presence,
            fetcher,
            clearing: crate::media::Clearing::default(),
            mixers: crate::calling::Mixers::default(),
            workflow_limit,
            redis: shared,
            bot_keys,
            outbound,
            bot_sends,
        });
        Ok(app)
    }

    /// Waits, at most `limit`, for the store clears a message delete left running after its answer, as a node does
    /// before it stops. The keys still not cleared then: the log names each, since no row does any more.
    pub async fn cleared(&self, limit: Duration) -> Vec<String> {
        self.clearing.done(limit).await
    }
}

/// Every route the node serves.
pub fn router(app: Arc<App>) -> Router {
    let auth = Router::new()
        .route("/oauth2/v2.0/token", post(crate::routes::token::token))
        .route("/api/authsvc/v1.0/authz", post(crate::routes::authz::authz))
        .layer(DefaultBodyLimit::max(AUTH_BODY_LIMIT));
    Router::new()
        .route("/health/live", get(|| async { StatusCode::NO_CONTENT }))
        .route("/health/ready", get(ready))
        .merge(auth)
        .merge(crate::chatsvc::router())
        .merge(crate::admin::api::router())
        .merge(crate::teams::router(&app.config))
        .merge(crate::people::router(&app))
        .merge(crate::presence::router(&app))
        .merge(crate::registrar::router())
        .merge(crate::search::router())
        .merge(crate::trouter::router())
        .merge(crate::calling::router())
        .merge(crate::workflows::router(&app.config))
        .merge(crate::apps::router(&app.config))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        // after the timeout: a file's upload or download may run longer than other requests
        .merge(crate::media::router(&app))
        .layer(TraceLayer::new_for_http())
        // after the trace: a webhook's link carries its secret in the query, which that trace would log
        .merge(crate::workflows::hook::router(&app.config))
        // after the trace too: an unsigned bot's service URL carries its credential
        .merge(crate::bots::router(&app))
        .merge(crate::outgoing::router(&app.config))
        .layer(axum::middleware::from_fn(crate::serve::drain_unread_body))
        .with_state(app)
}

/// `shutdown`, as the listener takes it: once it resolves the node's push sockets are closed too, so the listener's
/// open connections end without waiting out their grace.
pub async fn closing(app: Arc<App>, shutdown: impl Future<Output = ()> + Send) {
    shutdown.await;
    app.trouter.close_all();
}

/// Ready when the database and the fan-out answer.
async fn ready(State(app): State<Arc<App>>) -> StatusCode {
    let db = app.db.ping().await.map_err(|error| error.to_string());
    let fanout = app.fanout.ready().await.map_err(|error| error.to_string());
    match db.and(fanout) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(error) => {
            tracing::warn!(%error, "not ready");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}

/// The node's housekeeping for its `role`, until the task is dropped: key rotation when due, expired refresh tokens
/// and push registrations dropped.
pub async fn maintain(app: Arc<App>, role: crate::mixer::Role) {
    let mut every = tokio::time::interval(MAINTENANCE);
    loop {
        every.tick().await;
        tidy_for(&app, role).await;
    }
}

/// One round of [`maintain`] on a node that runs every role.
pub async fn tidy(app: &App) {
    tidy_for(app, crate::mixer::Role::All).await;
}

/// One round of [`maintain`] on a node that runs `role`.
pub async fn tidy_for(app: &App, role: crate::mixer::Role) {
    rotate(app, role).await;
    purge(app).await;
}

async fn rotate(app: &App, role: crate::mixer::Role) {
    if let Err(error) = app.issuer.keys().rotate_if_due().await {
        tracing::warn!(%error, "key rotation failed");
    }
    // only the API signs activities to bots: a media node alone doesn't make or retire their keys
    if role.serves_api() {
        crate::apps::keys::rotate(app).await;
    }
}

async fn purge(app: &App) {
    let before = crate::db::now() - crate::db::seconds(EXPIRED_KEPT);
    if let Err(error) = crate::db::refresh::purge_expired(&app.db, before).await {
        tracing::warn!(%error, "expired refresh tokens weren't dropped");
    }
    purge_registrations(app).await;
    crate::bots::purge(app).await;
    crate::outgoing::purge(app).await;
}

async fn purge_registrations(app: &App) {
    if let Err(error) = crate::registrar::purge_expired(&app.db, crate::db::now()).await {
        tracing::warn!(%error, "expired push registrations weren't dropped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_retired_key_outlives_the_longest_token_or_link_it_signed() {
        let allowance = 3 * MAINTENANCE + Duration::from_secs(crate::auth::tokens::LEEWAY);
        let mut config = Config::default();
        config.tokens.access_lifetime = 3600;
        config.media.link_lifetime = 60;
        assert_eq!(key_retention(&config), Duration::from_hours(1) + allowance);
        config.media.link_lifetime = 86_400;
        assert_eq!(key_retention(&config), Duration::from_hours(24) + allowance);
        config.bots.token_lifetime = 2 * 86_400;
        assert_eq!(key_retention(&config), Duration::from_hours(48) + allowance);
    }
}
