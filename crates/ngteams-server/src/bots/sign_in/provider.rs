//! The identity provider of a connection, as the `oauth2` crate's client talks to it: the authorization code grant
//! with PKCE (S256) and the refresh grant. Its token requests go through the server's address guard
//! ([`crate::outbound`]), as every request to an address an administrator set does.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use oauth2::basic::{BasicClient, BasicErrorResponse, BasicErrorResponseType, BasicTokenResponse};
use oauth2::{
    AsyncHttpClient, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet, EndpointSet,
    HttpRequest, HttpResponse, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, RefreshToken, RequestTokenError,
    Scope, TokenResponse as _, TokenUrl,
};
use serde::{Deserialize, Serialize};

use super::connections::Connection;
use crate::outbound::{Outbound, OutboundError};

/// The `oauth2` client's HTTP: the server's guarded outbound client. It keeps the answer's status, which the
/// crate's error drops.
struct Guarded<'a> {
    outbound: &'a Outbound,
    status: AtomicU16,
}

impl<'a> Guarded<'a> {
    const fn new(outbound: &'a Outbound) -> Self {
        Self {
            outbound,
            status: AtomicU16::new(0),
        }
    }

    /// Whether the provider can't answer now: a `429` or a `5xx`.
    fn busy(&self) -> bool {
        let status = self.status.load(Ordering::Relaxed);
        status == 429 || (500..600).contains(&status)
    }
}

impl<'c> AsyncHttpClient<'c> for Guarded<'_> {
    type Error = OutboundError;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse, OutboundError>> + Send + 'c>>;

    fn call(&'c self, request: HttpRequest) -> Self::Future {
        Box::pin(async move {
            let answer = self.outbound.exchange(request).await;
            if let Ok(answer) = &answer {
                self.status.store(answer.status().as_u16(), Ordering::Relaxed);
            }
            answer
        })
    }
}

type Client = BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

/// Why the provider gave no token.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ProviderError {
    /// The grant is gone: the provider refused it (`invalid_grant`), or there is no refresh token.
    #[error("the identity provider refused the grant: {0}")]
    Refused(String),
    /// The provider wasn't reached, or it can't answer now (`429`, `5xx`).
    #[error("the identity provider isn't available: {0}")]
    Unavailable(String),
    /// Anything else: the connection or the provider's answer.
    #[error("the identity provider gave no token: {0}")]
    Failed(String),
}

impl ProviderError {
    /// Whether the grant is gone, not only this attempt.
    pub(crate) fn refused(&self) -> bool {
        matches!(self, Self::Refused(_))
    }

    /// Whether a later attempt can work with no change.
    pub(crate) fn transient(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

type TokenError = RequestTokenError<OutboundError, BasicErrorResponse>;

fn token_error(error: &TokenError, busy: bool) -> ProviderError {
    match error {
        RequestTokenError::ServerResponse(answer) if *answer.error() == BasicErrorResponseType::InvalidGrant => {
            ProviderError::Refused(error.to_string())
        }
        RequestTokenError::Request(_) => ProviderError::Unavailable(error.to_string()),
        _ if busy => ProviderError::Unavailable(error.to_string()),
        _ => ProviderError::Failed(error.to_string()),
    }
}

/// A person's token from the provider, as it is kept (sealed). Its `Debug` shows no token.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Held {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// When it runs out, Unix milliseconds; none when the provider didn't say.
    pub expires_at: Option<i64>,
}

impl std::fmt::Debug for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Held")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

fn client(connection: &Connection, secret: String, redirect: &str) -> Result<Client, ProviderError> {
    let failed = |error: url::ParseError| ProviderError::Failed(error.to_string());
    Ok(BasicClient::new(ClientId::new(connection.client_id.clone()))
        .set_client_secret(ClientSecret::new(secret))
        .set_auth_uri(AuthUrl::new(connection.authorize_url.clone()).map_err(failed)?)
        .set_token_uri(TokenUrl::new(connection.token_url.clone()).map_err(failed)?)
        .set_redirect_uri(RedirectUrl::new(redirect.to_owned()).map_err(failed)?))
}

/// A new PKCE verifier, to keep with a sign-in.
pub(crate) fn new_verifier() -> String {
    let (_, verifier) = PkceCodeChallenge::new_random_sha256();
    verifier.secret().clone()
}

/// Where a person's browser signs in: the provider's authorize URL with the connection's scopes, `state` and the
/// challenge of `verifier`.
pub(crate) fn authorize_url(
    connection: &Connection,
    secret: String,
    redirect: &str,
    state: &str,
    verifier: &str,
) -> Result<String, ProviderError> {
    let challenge = PkceCodeChallenge::from_code_verifier_sha256(&PkceCodeVerifier::new(verifier.to_owned()));
    let state = state.to_owned();
    let (url, _) = client(connection, secret, redirect)?
        .authorize_url(move || CsrfToken::new(state))
        .add_scopes(
            connection
                .scopes
                .split_whitespace()
                .map(|scope| Scope::new(scope.to_owned())),
        )
        .set_pkce_challenge(challenge)
        .url();
    Ok(url.to_string())
}

/// The token for the provider's `code`, redeemed with `verifier`.
pub(crate) async fn redeem(
    outbound: &Outbound,
    connection: &Connection,
    secret: String,
    redirect: &str,
    code: String,
    verifier: String,
) -> Result<Held, ProviderError> {
    let http = Guarded::new(outbound);
    let answer = client(connection, secret, redirect)?
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(PkceCodeVerifier::new(verifier))
        .request_async(&http)
        .await
        .map_err(|error| token_error(&error, http.busy()))?;
    Ok(held(&answer, None))
}

/// A new token for `held`'s refresh token; the old refresh token stays when the provider gives no new one.
pub(crate) async fn renew(
    outbound: &Outbound,
    connection: &Connection,
    secret: String,
    redirect: &str,
    held_now: &Held,
) -> Result<Held, ProviderError> {
    let refresh = held_now
        .refresh_token
        .clone()
        .ok_or_else(|| ProviderError::Refused("no refresh token".to_owned()))?;
    let http = Guarded::new(outbound);
    let answer = client(connection, secret, redirect)?
        .exchange_refresh_token(&RefreshToken::new(refresh.clone()))
        .request_async(&http)
        .await
        .map_err(|error| token_error(&error, http.busy()))?;
    Ok(held(&answer, Some(refresh)))
}

fn held(answer: &BasicTokenResponse, old_refresh: Option<String>) -> Held {
    let lifetime = answer
        .expires_in()
        .map(|left| left.min(Duration::from_hours(10 * 365 * 24)));
    Held {
        access_token: answer.access_token().secret().clone(),
        refresh_token: answer
            .refresh_token()
            .map(|token| token.secret().clone())
            .or(old_refresh),
        expires_at: lifetime
            .and_then(|left| i64::try_from(left.as_millis()).ok())
            .map(|left| super::now_ms() + left),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection() -> Connection {
        Connection {
            bot_id: "b".to_owned(),
            name: "github".to_owned(),
            authorize_url: "https://idp.example.test/authorize".to_owned(),
            token_url: "https://idp.example.test/token".to_owned(),
            client_id: "client".to_owned(),
            client_secret: Vec::new(),
            scopes: "read:user  repo".to_owned(),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn the_authorize_url_asks_for_a_code_with_the_scopes_state_and_s256_challenge() {
        let verifier = new_verifier();
        let url = authorize_url(
            &connection(),
            "s".to_owned(),
            "https://own.example.test/api/oauth/callback",
            "state-1",
            &verifier,
        )
        .unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        let pairs: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(pairs["response_type"], "code");
        assert_eq!(pairs["client_id"], "client");
        assert_eq!(pairs["state"], "state-1");
        assert_eq!(pairs["scope"], "read:user repo");
        assert_eq!(pairs["code_challenge_method"], "S256");
        assert_eq!(pairs["redirect_uri"], "https://own.example.test/api/oauth/callback");
        assert!(!url.contains(&verifier));
        assert!(!url.contains("client_secret"));
        let mut broken = connection();
        broken.token_url = "not a url".to_owned();
        assert!(authorize_url(&broken, "s".to_owned(), "https://o/", "s", &verifier).is_err());
        let held = Held {
            access_token: "synthetic-access".to_owned(),
            refresh_token: Some("synthetic-refresh".to_owned()),
            expires_at: Some(1),
        };
        assert!(!format!("{held:?}").contains("synthetic"));
    }

    #[test]
    fn only_a_refused_grant_is_the_grants_end() {
        let answer = |kind| RequestTokenError::ServerResponse(BasicErrorResponse::new(kind, None, None));
        assert!(token_error(&answer(BasicErrorResponseType::InvalidGrant), false).refused());
        assert!(token_error(&answer(BasicErrorResponseType::InvalidGrant), true).refused());
        let client = token_error(&answer(BasicErrorResponseType::InvalidClient), false);
        assert!(!client.refused() && !client.transient());
        let timeout = token_error(&RequestTokenError::Request(OutboundError::Timeout), false);
        assert!(!timeout.refused() && timeout.transient());
        let busy = token_error(&RequestTokenError::Other("empty".to_owned()), true);
        assert!(!busy.refused() && busy.transient());
        assert!(!token_error(&RequestTokenError::Other("empty".to_owned()), false).transient());
    }
}
