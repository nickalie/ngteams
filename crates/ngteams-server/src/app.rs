//! The state every request shares, and the router over it.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;

use crate::config::Config;
use crate::db::{Db, DbError};

/// What a node holds while it runs. Shared behind an `Arc`.
#[derive(Debug)]
pub struct App {
    pub config: Config,
    pub db: Db,
    /// The server's tenant id, every account's `tid`.
    pub tenant: String,
}

/// Why a node can't start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Db(#[from] DbError),
}

impl App {
    /// Connects to the database (migrating it) and reads the tenant.
    ///
    /// # Errors
    /// See [`StartError`].
    pub async fn start(config: Config) -> Result<Arc<Self>, StartError> {
        let db = Db::connect(&config.database_url, config.database_pool).await?;
        let tenant = crate::db::meta::tenant(&db).await?;
        Ok(Arc::new(Self { config, db, tenant }))
    }
}

/// Every route the node serves.
pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/health/live", get(|| async { StatusCode::NO_CONTENT }))
        .route("/health/ready", get(ready))
        .with_state(app)
}

/// Ready when the database answers.
async fn ready(State(app): State<Arc<App>>) -> StatusCode {
    match app.db.ping().await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(error) => {
            tracing::warn!(%error, "not ready");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}
