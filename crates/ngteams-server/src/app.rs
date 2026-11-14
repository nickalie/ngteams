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
use crate::config::Config;
use crate::db::{Db, DbError};
use crate::fanout::{Fanout, FanoutError, Local, Redis};
use crate::storage::{ObjectStorage, Storage, StorageError};
use crate::throttle::Throttle;

/// The largest request body the auth endpoints take.
const AUTH_BODY_LIMIT: usize = 64 * 1024;
/// How long a request may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How often a node rotates keys when due and drops expired refresh tokens.
const MAINTENANCE: Duration = Duration::from_secs(3600);
/// Expired refresh tokens are kept this much longer (a late reuse still finds its family).
const EXPIRED_KEPT: Duration = Duration::from_secs(86_400);

/// What a node holds while it runs. Shared behind an `Arc`.
#[derive(Debug)]
pub struct App {
    pub config: Config,
    pub db: Db,
    /// The server's tenant id, every account's `tid`.
    pub tenant: String,
    pub issuer: Issuer,
    pub throttle: Throttle,
    pub fanout: Arc<dyn Fanout>,
    pub storage: Arc<dyn Storage>,
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
}

impl App {
    /// Connects to the database (migrating it), Redis (when configured) and the file store, and loads the keys.
    ///
    /// # Errors
    /// See [`StartError`].
    pub async fn start(config: Config) -> Result<Arc<Self>, StartError> {
        let db = Db::connect(&config.database_url, config.database_pool).await?;
        let tenant = crate::db::meta::tenant(&db).await?;
        // a key verifies as long as an access token it signed may live after it stops signing
        let keep = config.tokens.access() + config.tokens.rotation();
        let keys = KeyRing::load(db.clone(), config.tokens.rotation(), keep).await?;
        let issuer = Issuer::new(keys, &config.origin(), &tenant, &config.region, config.tokens.access());
        let (fanout, throttle): (Arc<dyn Fanout>, Throttle) = match &config.redis_url {
            Some(url) => {
                let redis = Redis::connect(url).await?;
                let throttle = Throttle::redis(config.login.clone(), redis.connection());
                (Arc::new(redis), throttle)
            }
            None => (Arc::new(Local::new()), Throttle::local(config.login.clone())),
        };
        let storage = Arc::new(ObjectStorage::open(&config.storage_url, &config.storage_options)?);
        Ok(Arc::new(Self {
            config,
            db,
            tenant,
            issuer,
            throttle,
            fanout,
            storage,
        }))
    }
}

/// Every route the node serves.
pub fn router(app: Arc<App>) -> Router {
    let auth = Router::new()
        .route("/oauth2/v2.0/token", post(crate::routes::token::token))
        .layer(DefaultBodyLimit::max(AUTH_BODY_LIMIT));
    Router::new()
        .route("/health/live", get(|| async { StatusCode::NO_CONTENT }))
        .route("/health/ready", get(ready))
        .merge(auth)
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, REQUEST_TIMEOUT))
        .layer(TraceLayer::new_for_http())
        .with_state(app)
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

/// The node's housekeeping, until the task is dropped: key rotation when due, expired refresh tokens dropped.
pub async fn maintain(app: Arc<App>) {
    let mut every = tokio::time::interval(MAINTENANCE);
    loop {
        every.tick().await;
        tidy(&app).await;
    }
}

/// One round of [`maintain`].
pub async fn tidy(app: &App) {
    if let Err(error) = app.issuer.keys().rotate_if_due().await {
        tracing::warn!(%error, "key rotation failed");
    }
    let before = crate::db::now() - crate::db::seconds(EXPIRED_KEPT);
    if let Err(error) = crate::db::refresh::purge_expired(&app.db, before).await {
        tracing::warn!(%error, "expired refresh tokens weren't dropped");
    }
}
