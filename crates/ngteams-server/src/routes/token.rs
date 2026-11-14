//! `POST /oauth2/v2.0/token`: Entra's v2 token endpoint as the client speaks to it (`docs/protocol/auth.md`), with the
//! password grant (an own-server sign-in) and the refresh grant (a token per audience). Answers and errors take
//! Entra's JSON shapes, so the client's `TokenClient` reads them unchanged: a refused grant is `invalid_grant` with an
//! `AADSTS` code, a held-off account 429 with `Retry-After`.
//!
//! Refresh tokens are single use: each grant issues the next one of the sign-in's family. One presented again within
//! the grace period (a client retrying after a lost answer) still works; later, it revokes the family.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Form;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, RETRY_AFTER};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;
use sha2::{Digest as _, Sha256};

use crate::app::App;
use crate::auth::keys::KeyError;
use crate::auth::password;
use crate::auth::tokens::{Requested, requested, seconds_until};
use crate::db::refresh::{self, RefreshToken};
use crate::db::users::{self, User, normalize_upn};
use crate::db::{DbError, now, seconds};

/// An Entra-shaped refusal.
#[derive(Debug)]
pub struct Refusal {
    status: StatusCode,
    error: &'static str,
    code: u32,
    description: &'static str,
    retry_after: Option<Duration>,
}

impl Refusal {
    const fn new(error: &'static str, code: u32, description: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error,
            code,
            description,
            retry_after: None,
        }
    }

    const fn missing() -> Self {
        Self::new(
            "invalid_request",
            900_144,
            "The request body must contain the parameters of its grant.",
        )
    }

    const fn credentials() -> Self {
        Self::new(
            "invalid_grant",
            50_126,
            "Error validating credentials due to invalid username or password.",
        )
    }

    const fn disabled() -> Self {
        Self::new("invalid_grant", 50_057, "The user account is disabled.")
    }

    const fn refresh() -> Self {
        Self::new("invalid_grant", 70_000, "The provided refresh token is not valid.")
    }

    const fn expired() -> Self {
        Self::new("invalid_grant", 700_082, "The refresh token has expired.")
    }

    const fn scope() -> Self {
        Self::new(
            "invalid_scope",
            70_011,
            "The scope names no resource this server issues tokens for.",
        )
    }

    const fn grant_type() -> Self {
        Self::new("unsupported_grant_type", 70_003, "The grant type is not supported.")
    }

    fn held(wait: Duration) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            retry_after: Some(wait),
            ..Self::new(
                "temporarily_unavailable",
                50_053,
                "The account is locked after failed sign-ins.",
            )
        }
    }

    const fn server() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            ..Self::new("server_error", 90_033, "The server can't issue a token now.")
        }
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let body = json!({
            "error": self.error,
            "error_description": format!("AADSTS{}: {}", self.code, self.description),
            "error_codes": [self.code],
            "timestamp": now(),
        });
        let mut response = (self.status, [(CACHE_CONTROL, "no-store")], axum::Json(body)).into_response();
        if let Some(wait) = self.retry_after
            && let Ok(value) = wait.as_secs().max(1).to_string().parse()
        {
            response.headers_mut().insert(RETRY_AFTER, value);
        }
        response
    }
}

impl From<DbError> for Refusal {
    fn from(error: DbError) -> Self {
        tracing::error!(%error, "token endpoint failed");
        Self::server()
    }
}

impl From<KeyError> for Refusal {
    fn from(error: KeyError) -> Self {
        tracing::error!(%error, "token endpoint failed");
        Self::server()
    }
}

/// The token endpoint.
///
/// # Errors
/// A [`Refusal`] in Entra's shape for a grant it won't give.
pub async fn token(
    State(app): State<Arc<App>>,
    Form(form): Form<BTreeMap<String, String>>,
) -> Result<Response, Refusal> {
    let field = |name: &str| form.get(name).map(String::as_str).filter(|value| !value.is_empty());
    let client_id = field("client_id").ok_or_else(Refusal::missing)?;
    let scope = field("scope").ok_or_else(Refusal::missing)?;
    let resource = requested(scope, &app.config.audiences).ok_or_else(Refusal::scope)?;
    let (user, refresh) = match field("grant_type") {
        Some("password") => {
            let upn = field("username").ok_or_else(Refusal::missing)?;
            let secret = field("password").ok_or_else(Refusal::missing)?;
            let user = password_grant(&app, upn, secret).await?;
            (user, None)
        }
        Some("refresh_token") => {
            let presented = field("refresh_token").ok_or_else(Refusal::missing)?;
            let (user, family) = refresh_grant(&app, presented, client_id).await?;
            (user, Some(family))
        }
        Some(_) => return Err(Refusal::grant_type()),
        None => return Err(Refusal::missing()),
    };
    issue(&app, &user, &resource, client_id, refresh.as_deref()).await
}

/// The account `upn` names when `secret` is its password, counting failures toward the hold.
async fn password_grant(app: &App, upn: &str, secret: &str) -> Result<User, Refusal> {
    let upn = normalize_upn(upn);
    if let Some(wait) = app.throttle.reserve(&upn).await {
        return Err(Refusal::held(wait));
    }
    let user = users::by_upn(&app.db, &upn).await?;
    if !password::verify(secret, user.as_ref().map(|user| user.password_hash.as_str())).await {
        return Err(Refusal::credentials());
    }
    app.throttle.succeeded(&upn).await;
    let user = user.ok_or_else(Refusal::credentials)?;
    if user.disabled {
        return Err(Refusal::disabled());
    }
    Ok(user)
}

/// The account a refresh token stands for and its family, once the token is spent.
async fn refresh_grant(app: &App, presented: &str, client_id: &str) -> Result<(User, String), Refusal> {
    let row = refresh::by_hash(&app.db, &digest(presented))
        .await?
        .ok_or_else(Refusal::refresh)?;
    if row.revoked_at.is_some() || row.client_id != client_id {
        return Err(Refusal::refresh());
    }
    if row.expires_at <= now() {
        return Err(Refusal::expired());
    }
    if !refresh::mark_used(&app.db, &row.id).await? {
        spent_again(app, &row).await?;
    }
    let user = users::by_id(&app.db, &row.user_id)
        .await?
        .filter(|user| !user.disabled)
        .ok_or_else(Refusal::disabled)?;
    Ok((user, row.family_id))
}

/// A used token presented again: let it through within the grace period, else revoke its family.
async fn spent_again(app: &App, row: &RefreshToken) -> Result<(), Refusal> {
    let used_at = match row.used_at {
        Some(used_at) => used_at,
        None => refresh::by_hash(&app.db, &row.token_hash)
            .await?
            .and_then(|row| row.used_at)
            .unwrap_or(0),
    };
    if now() - used_at < i64::try_from(app.config.tokens.reuse_grace).unwrap_or(i64::MAX) {
        return Ok(());
    }
    tracing::warn!(family = %row.family_id, "a spent refresh token came back: its sign-in is revoked");
    refresh::revoke_family(&app.db, &row.family_id).await?;
    Err(Refusal::refresh())
}

/// The answer: an access token for `resource`, an ID token, and the next refresh token of `family` (a new family
/// for a password sign-in).
async fn issue(
    app: &App,
    user: &User,
    resource: &Requested,
    client_id: &str,
    family: Option<&str>,
) -> Result<Response, Refusal> {
    let access = app.issuer.access(user, resource, client_id)?;
    let id_token = app.issuer.id_token(user, client_id)?;
    let next = new_refresh_token()?;
    let created_at = now();
    let inserted = refresh::insert(
        &app.db,
        &RefreshToken {
            id: uuid::Uuid::new_v4().to_string(),
            family_id: family.map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned),
            user_id: user.id.clone(),
            token_hash: digest(&next),
            client_id: client_id.to_owned(),
            created_at,
            expires_at: created_at + seconds(app.config.tokens.refresh()),
            used_at: None,
            revoked_at: None,
        },
    )
    .await?;
    if !inserted {
        return Err(Refusal::refresh());
    }
    let expires_in = seconds_until(access.expires_at);
    let scope = format!("{}/{} openid profile offline_access", resource.audience, resource.scope);
    let body = json!({
        "token_type": "Bearer",
        "scope": scope,
        "expires_in": expires_in,
        "ext_expires_in": expires_in,
        "access_token": access.token,
        "refresh_token": next,
        "id_token": id_token,
    });
    Ok(([(CACHE_CONTROL, "no-store")], axum::Json(body)).into_response())
}

/// A fresh opaque refresh token: 32 random bytes, base64url.
fn new_refresh_token() -> Result<String, Refusal> {
    let mut bytes = [0_u8; 32];
    aws_lc_rs::rand::fill(&mut bytes).map_err(|_| Refusal::server())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// How a refresh token is kept: its SHA-256, base64url.
fn digest(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}
