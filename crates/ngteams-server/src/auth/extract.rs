//! Request credentials, as extractors: [`Bearer`] for an access token of one audience, [`Skype`] for a skypetoken.
//! A request without a valid one is answered 401 before its handler runs.

use std::marker::PhantomData;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use super::tokens::{AccessClaims, SkypeClaims};
use crate::app::App;

/// The audience a [`Bearer`] must carry.
pub trait Audience: Send + Sync + 'static {
    const AUDIENCE: &'static str;
}

/// `https://api.spaces.skype.com`: authz, the middle tier.
#[derive(Debug)]
pub struct Spaces;

impl Audience for Spaces {
    const AUDIENCE: &'static str = "https://api.spaces.skype.com";
}

/// An access token for `A`'s audience, from `Authorization: Bearer`.
#[derive(Debug)]
pub struct Bearer<A> {
    pub claims: AccessClaims,
    audience: PhantomData<A>,
}

/// A skypetoken, from `X-Skypetoken`, `Authentication: skypetoken=` or `Authorization: skype_token`, the forms Teams'
/// services take it in.
#[derive(Debug)]
pub struct Skype(pub SkypeClaims);

/// The 401 a missing or invalid credential gets.
#[derive(Debug)]
pub struct Unauthorized(&'static str);

impl IntoResponse for Unauthorized {
    fn into_response(self) -> Response {
        (StatusCode::UNAUTHORIZED, [(WWW_AUTHENTICATE, self.0)]).into_response()
    }
}

const BEARER_CHALLENGE: &str = "Bearer error=\"invalid_token\"";
const SKYPE_CHALLENGE: &str = "skypetoken";

impl<A: Audience> FromRequestParts<Arc<App>> for Bearer<A> {
    type Rejection = Unauthorized;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, Self::Rejection> {
        let token = bearer(&parts.headers).ok_or(Unauthorized(BEARER_CHALLENGE))?;
        let claims = app
            .issuer
            .verify_access(token, A::AUDIENCE)
            .await
            .map_err(|_| Unauthorized(BEARER_CHALLENGE))?;
        Ok(Self {
            claims,
            audience: PhantomData,
        })
    }
}

impl FromRequestParts<Arc<App>> for Skype {
    type Rejection = Unauthorized;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, Self::Rejection> {
        let token = skypetoken(&parts.headers).ok_or(Unauthorized(SKYPE_CHALLENGE))?;
        app.issuer
            .verify_skypetoken(token)
            .await
            .map(Self)
            .map_err(|_| Unauthorized(SKYPE_CHALLENGE))
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok().map(str::trim)
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = header(headers, AUTHORIZATION.as_str())?;
    let (scheme, token) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

fn skypetoken(headers: &HeaderMap) -> Option<&str> {
    if let Some(token) = header(headers, "x-skypetoken") {
        return Some(token);
    }
    if let Some(token) = header(headers, "authentication").and_then(|value| value.strip_prefix("skypetoken=")) {
        return Some(token.trim());
    }
    let (scheme, token) = header(headers, AUTHORIZATION.as_str())?.split_once(' ')?;
    scheme.eq_ignore_ascii_case("skype_token").then(|| token.trim())
}
