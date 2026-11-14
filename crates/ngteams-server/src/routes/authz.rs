//! `POST /api/authsvc/v1.0/authz`: an access token for `https://api.spaces.skype.com` exchanged for a skypetoken, the
//! account's region and the region GTMs, in the answer's shape the client reads (`docs/protocol/auth.md`). As Teams'
//! does, the skypetoken runs out when the exchanged token does. The GTMs name only the services this server serves
//! ([`crate::services`]): a service the client finds no GTM for stays off.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::app::App;
use crate::auth::extract::{Bearer, Spaces};
use crate::auth::tokens::seconds_until;
use crate::db::users;

/// The authz endpoint.
pub async fn authz(State(app): State<Arc<App>>, bearer: Bearer<Spaces>) -> Response {
    match users::by_id(&app.db, &bearer.claims.oid).await {
        Ok(Some(user)) if !user.disabled => {}
        Ok(_) => return refused(),
        Err(error) => {
            tracing::error!(%error, "authz failed");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }
    let skypetoken = match app.issuer.skypetoken(&bearer.claims) {
        Ok(skypetoken) => skypetoken,
        Err(error) => {
            tracing::error!(%error, "authz failed");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let body = json!({
        "tokens": {
            "skypeToken": skypetoken.token,
            "expiresIn": seconds_until(skypetoken.expires_at),
            "tokenType": "SkypeToken",
        },
        "region": app.config.region,
        "partition": app.config.partition,
        "regionGtms": crate::services::region_gtms(&app.config),
        "regionSettings": {
            "isAppsDiscoveryEnabled": false,
            "isContactMigrationEnabled": false,
            "isFederationEnabled": false,
            "isOutOfOfficeIntegrationEnabled": false,
            "isUnifiedPresenceEnabled": false,
        },
        "licenseDetails": {
            "isFreemium": false,
            "isTrial": false,
            "isTeamsEnabled": true,
            "isCopilot": false,
        },
        "isMultiGeo": false,
    });
    ([(CACHE_CONTROL, "no-store")], Json(body)).into_response()
}

/// The 403 Teams gives an account it won't serve.
fn refused() -> Response {
    let body = json!({
        "errorCode": "UserNotFoundInSkypeTeam",
        "message": "This account can't use this server.",
    });
    (StatusCode::FORBIDDEN, Json(body)).into_response()
}
