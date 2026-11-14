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
use serde_json::{Value, json};

use crate::app::App;
use crate::auth::extract::{Bearer, Spaces};
use crate::auth::tokens::{Minted, seconds_until};
use crate::config::Config;
use crate::db::users;

/// The authz endpoint.
pub async fn authz(State(app): State<Arc<App>>, bearer: Bearer<Spaces>) -> Response {
    match users::by_id(&app.db, &bearer.claims.oid).await {
        Ok(Some(user)) if !user.disabled => {}
        Ok(_) => return refused(),
        Err(error) => return unavailable(&error),
    }
    match app.issuer.skypetoken(&bearer.claims) {
        Ok(skypetoken) => ([(CACHE_CONTROL, "no-store")], Json(answer(&app.config, &skypetoken))).into_response(),
        Err(error) => unavailable(&error),
    }
}

/// The answer's body, in the shape Teams' authz gives.
fn answer(config: &Config, skypetoken: &Minted) -> Value {
    json!({
        "tokens": {
            "skypeToken": skypetoken.token,
            "expiresIn": seconds_until(skypetoken.expires_at),
            "tokenType": "SkypeToken",
        },
        "region": config.region,
        "partition": config.partition,
        "regionGtms": crate::services::region_gtms(config),
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
    })
}

fn unavailable(error: &dyn std::fmt::Display) -> Response {
    tracing::error!(%error, "authz failed");
    StatusCode::SERVICE_UNAVAILABLE.into_response()
}

/// The 403 Teams gives an account it won't serve.
fn refused() -> Response {
    let body = json!({
        "errorCode": "UserNotFoundInSkypeTeam",
        "message": "This account can't use this server.",
    });
    (StatusCode::FORBIDDEN, Json(body)).into_response()
}
