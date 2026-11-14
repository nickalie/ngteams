//! The skypetoken exchange: Teams' authz endpoint takes an Entra access token for `https://api.spaces.skype.com` and
//! answers with a skypetoken and the account's region: its name, partition and the base URLs of the regional services
//! (the "region GTMs"). `docs/protocol/auth.md` has the request and the answer as the web client sends and reads them.
//!
//! - Only `authz` itself is ever posted to. [`AuthzClient::with_endpoint`] refuses any URL whose last path segment isn't
//!   `authz`, so the "start a Teams trial" variant (`…/authz/signuptrial`), a licensing change, can't be reached
//!   through this client at all.
//! - The skypetoken's expiry is its JWT `exp` when it has one, as the web client schedules it, and the answer's
//!   `expiresIn` only otherwise; the region falls back to the token's `rgn` claim.
//! - A refusal keeps only the status and Teams' `errorCode` (`UserLicenseNotPresentTrialEligible` for an account
//!   without a Teams license), never the body's message. At `debug` the wire logger records the full exchange with
//!   credentials redacted; no error or `Debug` carries a token.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, HeaderMap, WWW_AUTHENTICATE};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use url::Url;

use crate::claims::{payload, text, time};
use crate::secrets::{Secret, SecretError, SecretKey, SecretStore};
use crate::session::{Clock, SystemClock};
use crate::token::{retry_after, seconds};

/// Teams' authz endpoint in the commercial cloud.
pub const AUTHZ_ENDPOINT: &str = "https://teams.microsoft.com/api/authsvc/v1.0/authz";
/// The audience name a skypetoken is stored under, next to the account's refresh token.
pub const SKYPETOKEN: &str = "skypetoken";
/// The audience name a stored skypetoken's expiry is kept under ([`skypetoken_expiry`]): its `exp`, or earlier when
/// it was capped at the Entra token it was exchanged for, which a skypetoken read back must not outlive either.
pub const SKYPETOKEN_EXPIRY: &str = "skypetoken-expiry";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest `errorCode` kept from a refusal; Teams' codes are short identifiers.
const ERROR_CODE_LIMIT: usize = 100;
/// GTMs a service falls back to when the region has none of its own name: the web client's chat fallback
/// (`chatServiceAfd` → `chatService`), and the image service, which has the middle tier's base
/// (`docs/protocol/auth.md`).
const GTM_FALLBACKS: [(&str, &str); 2] = [("chatServiceAfd", "chatService"), ("mtImageService", "middleTier")];

/// The GTM the chat service's base comes from, as the web client takes it (`useChatServiceAfd`); [`Region::base`]
/// falls back to `chatService` when the region lacks it.
pub const CHAT_BASE_GTM: &str = "chatServiceAfd";

/// Where `account`'s skypetoken lives in a [`SecretStore`].
#[must_use]
pub fn skypetoken_key(account: &str) -> SecretKey {
    SecretKey::new(account, SKYPETOKEN)
}

/// Where the expiry of `account`'s stored skypetoken lives in a [`SecretStore`] ([`SKYPETOKEN_EXPIRY`]).
#[must_use]
pub fn skypetoken_expiry_key(account: &str) -> SecretKey {
    SecretKey::new(account, SKYPETOKEN_EXPIRY)
}

/// What is stored under [`skypetoken_expiry_key`] for `token` expiring at `expires_at`: its Unix seconds, a space and
/// the token's SHA-256 (base64url), which binds the expiry to the one token it was kept with.
#[must_use]
pub fn skypetoken_expiry(token: &Secret, expires_at: SystemTime) -> Secret {
    Secret::new(format!("{} {}", unix_seconds(expires_at), token_digest(token)))
}

fn token_digest(token: &Secret) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.expose().as_bytes()))
}

/// Why the exchange is made, as the `ms-teams-authz-type` header tells Teams.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthzKind {
    /// The first exchange after signing in.
    ExplicitLogin,
    /// A renewal before the skypetoken runs out.
    TokenRefresh,
}

impl AuthzKind {
    fn header(self) -> &'static str {
        match self {
            Self::ExplicitLogin => "ExplicitLogin",
            Self::TokenRefresh => "TokenRefresh",
        }
    }
}

/// The account's region as authz gives it: not secret, so it may be persisted and shown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Region {
    /// `region`, else the skypetoken's `rgn` claim.
    pub name: Option<String>,
    pub partition: Option<String>,
    /// `regionGtms`: service name → base URL, every string entry as returned.
    pub endpoints: BTreeMap<String, String>,
}

impl Region {
    /// The JSON the lab persists: `{"region", "partition", "regionGtms"}`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "region": self.name,
            "partition": self.partition,
            "regionGtms": self.endpoints,
        })
    }

    /// Reads back what [`Self::to_json`] wrote. `None` when it isn't that shape.
    #[must_use]
    pub fn from_json(value: &Value) -> Option<Self> {
        let optional = |key: &str| match value.get(key)? {
            Value::Null => Some(None),
            Value::String(text) => Some(Some(text.clone())),
            _ => None,
        };
        Some(Self {
            name: optional("region")?,
            partition: optional("partition")?,
            endpoints: endpoints(value.get("regionGtms")?)?,
        })
    }

    /// Reads back a region record a lab saved ([`Self::to_json`]'s keys, and any others beside them).
    ///
    /// # Errors
    /// [`RegionFileError::Read`] when the file can't be read, [`RegionFileError::NotARecord`] when it isn't a region
    /// record.
    pub fn load(path: &Path) -> Result<Self, RegionFileError> {
        let text = std::fs::read_to_string(path).map_err(|source| RegionFileError::Read {
            path: path.to_owned(),
            source,
        })?;
        serde_json::from_str::<Value>(&text)
            .ok()
            .as_ref()
            .and_then(Self::from_json)
            .ok_or_else(|| RegionFileError::NotARecord(path.to_owned()))
    }

    /// The base URL of one regional service, such as `chatServiceAfd` or `middleTier`, as returned.
    #[must_use]
    pub fn endpoint(&self, service: &str) -> Option<&str> {
        self.endpoints.get(service).map(String::as_str)
    }

    /// The base URL to use for a regional service: its GTM, else its fallback's ([`GTM_FALLBACKS`]: `chatServiceAfd`
    /// falls back to `chatService`, `mtImageService` to `middleTier`), trimmed (authz returns some values with stray
    /// whitespace). `None` when neither is there or the value is blank.
    #[must_use]
    pub fn base(&self, service: &str) -> Option<&str> {
        let fallback = GTM_FALLBACKS
            .iter()
            .find(|(name, _)| *name == service)
            .map(|(_, fallback)| *fallback);
        [Some(service), fallback]
            .into_iter()
            .flatten()
            .filter_map(|name| self.endpoint(name))
            .map(str::trim)
            .find(|base| !base.is_empty())
    }
}

/// Why a saved region can't be read back ([`Region::load`]).
#[derive(Debug, thiserror::Error)]
pub enum RegionFileError {
    #[error("can't read the region record {}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("{} isn't a region record", .0.display())]
    NotARecord(PathBuf),
}

/// A skypetoken and what came with it. `Debug` shows no token.
#[derive(Clone, Debug)]
pub struct Skypetoken {
    pub token: Secret,
    /// The token's `exp`, else the time of the answer plus `expiresIn`.
    pub expires_at: SystemTime,
    /// The token's `skypeid` claim; the account's MRI is `8:` and this.
    pub skype_id: Option<String>,
    pub region: Region,
}

impl Skypetoken {
    /// A skypetoken read back from a [`SecretStore`], with the `expiry` stored next to it ([`Self::store`]) and
    /// `region` (stored apart from both): `None` unless it is a JWT with an `exp` and its stored expiry
    /// ([`skypetoken_expiry`]) is readable and was kept with this token, since an expiry that can't be read, or is
    /// another token's (one stored by another client between this one's two reads), can't be trusted. It expires at
    /// the earlier of the two.
    pub(crate) fn from_stored(token: Secret, expiry: Option<&Secret>, region: Region) -> Option<Self> {
        let claims = payload(token.expose())?;
        let (seconds, digest) = expiry?.expose().split_once(' ')?;
        if digest != token_digest(&token) {
            return None;
        }
        let kept = UNIX_EPOCH.checked_add(Duration::from_secs(seconds.parse().ok()?))?;
        Some(Self {
            expires_at: time(&claims, "exp")?.min(kept),
            skype_id: text(&claims, "skypeid"),
            region,
            token,
        })
    }

    /// Stores it under `account`, replacing the one there: the token, then its expiry ([`skypetoken_expiry`]). A
    /// store failing between the two leaves the token with an expiry that isn't its own, which isn't read back
    /// ([`Self::from_stored`]).
    ///
    /// # Errors
    /// A store failure.
    pub(crate) async fn store<S: SecretStore>(&self, store: &S, account: &str) -> Result<(), SecretError> {
        store.put(&skypetoken_key(account), &self.token).await?;
        let expiry = skypetoken_expiry(&self.token, self.expires_at);
        store.put(&skypetoken_expiry_key(account), &expiry).await
    }
}

/// Why the exchange gave no skypetoken. Never carries a token.
#[derive(Debug, thiserror::Error)]
pub enum AuthzError {
    /// The endpoint isn't an `authz` URL over HTTPS (plain HTTP only on a loopback host, for tests).
    #[error("not an authz endpoint: the URL must be https and end in /authz")]
    InvalidEndpoint,
    /// Teams won't serve the account (403), such as one without a Teams license; asking again doesn't change that.
    #[error(
        "Teams refused the account (authz answered {status}{})",
        .code.as_deref().map_or_else(String::new, |code| format!(": {code}"))
    )]
    Refused { status: u16, code: Option<String> },
    /// The access token was refused (401): refresh it, with the claims if Teams sent a challenge, and ask again.
    #[error("authz refused the access token (401){}", if *.claims_challenge { ", with a claims challenge" } else { "" })]
    Unauthorized { claims_challenge: bool },
    /// 429 or a 5xx: wait `retry_after` (when given) before asking again. Not retried here.
    #[error(
        "authz is unavailable or throttling (HTTP {status}){}",
        .retry_after.map_or_else(String::new, |wait| format!(", retry after {} s", wait.as_secs()))
    )]
    Unavailable { status: u16, retry_after: Option<Duration> },
    /// Any other status.
    #[error("unexpected answer from authz (HTTP {status})")]
    UnexpectedResponse { status: u16 },
    /// A success without a field the exchange needs.
    #[error("the authz answer has no valid {0}")]
    MissingField(&'static str),
    /// The system random source failed (the request's correlation id).
    #[error("the system random source failed")]
    Random,
    #[error("authz request failed: {0}")]
    Http(#[source] reqwest::Error),
    #[error(transparent)]
    Store(#[from] SecretError),
}

/// Posts to authz.
#[derive(Clone, Debug)]
pub struct AuthzClient {
    http: reqwest::Client,
    endpoint: Url,
}

impl AuthzClient {
    /// A client for [`AUTHZ_ENDPOINT`].
    ///
    /// # Errors
    /// When the HTTP client can't be built.
    pub fn new() -> Result<Self, AuthzError> {
        Self::with_endpoint(AUTHZ_ENDPOINT)
    }

    /// The authz endpoint belonging to this account's cloud.
    ///
    /// # Errors
    /// When the HTTP client cannot be built.
    pub fn for_cloud(cloud: &crate::cloud::Cloud) -> Result<Self, AuthzError> {
        let endpoint = cloud
            .service_url(AUTHZ_ENDPOINT, "/api/authsvc/v1.0/authz")
            .map_err(|_| AuthzError::InvalidEndpoint)?;
        Self::with_endpoint(endpoint.as_str())
    }

    /// A client for another authz URL (another cloud, or a mock server). No proxy, no redirects, a 30 s timeout.
    ///
    /// # Errors
    /// [`AuthzError::InvalidEndpoint`] unless the URL is HTTPS (or HTTP on a loopback host) and its path ends in the
    /// segment `authz`, with no query or fragment; or when the HTTP client can't be built.
    pub fn with_endpoint(endpoint: &str) -> Result<Self, AuthzError> {
        let endpoint = Url::parse(endpoint).map_err(|_| AuthzError::InvalidEndpoint)?;
        if !is_authz_endpoint(&endpoint) {
            return Err(AuthzError::InvalidEndpoint);
        }
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(http_error)?;
        Ok(Self { http, endpoint })
    }

    #[must_use]
    pub fn endpoint(&self) -> &str {
        self.endpoint.as_str()
    }

    /// Exchanges an access token for `https://api.spaces.skype.com` for a skypetoken: `POST` with no body and the
    /// headers the web client sends (`docs/protocol/auth.md`).
    ///
    /// # Errors
    /// [`AuthzError::Refused`], [`AuthzError::Unauthorized`], [`AuthzError::Unavailable`] or
    /// [`AuthzError::UnexpectedResponse`] by status; [`AuthzError::MissingField`] for a success without a skypetoken,
    /// an expiry or the region GTMs; [`AuthzError::Http`] when no answer came.
    pub async fn exchange(&self, spaces_token: &Secret, kind: AuthzKind) -> Result<Skypetoken, AuthzError> {
        self.exchange_at(spaces_token, kind, &SystemClock).await
    }

    /// [`Self::exchange`], with an `expiresIn` (used only when the token has no `exp`) counted from `clock`'s time of
    /// the answer.
    pub(crate) async fn exchange_at(
        &self,
        spaces_token: &Secret,
        kind: AuthzKind,
        clock: &dyn Clock,
    ) -> Result<Skypetoken, AuthzError> {
        let correlation = correlation_id()?;
        let request = self
            .http
            .post(self.endpoint.clone())
            .header(AUTHORIZATION, format!("Bearer {}", spaces_token.expose()))
            .header("claimsChallengeCapable", "true")
            .header("X-MS-Migration", "True")
            .header("ms-teams-authz-type", kind.header())
            .header("clientRequestId", &correlation)
            .header("x-ms-request-id", &correlation)
            .header(CONTENT_LENGTH, "0");
        let answer = crate::wire::exchange(&self.http, request, &[spaces_token.expose()])
            .await
            .map_err(http_error)?;
        if answer.status.is_success() {
            parse_answer(&answer.body, clock.now())
        } else {
            Err(refusal(answer.status, &answer.headers, &answer.body))
        }
    }

    /// [`Self::exchange`], with the skypetoken's expiry capped at `spaces_expires_at` (the Entra token's), then stores
    /// it under `account` ([`skypetoken_key`], its expiry under [`skypetoken_expiry_key`]), replacing the one there.
    /// When the exchange fails nothing is stored, and a skypetoken already stored is left as it was.
    ///
    /// # Errors
    /// As [`Self::exchange`], or a store failure.
    pub async fn exchange_and_store<S: SecretStore>(
        &self,
        store: &S,
        account: &str,
        spaces_token: &Secret,
        spaces_expires_at: SystemTime,
        kind: AuthzKind,
    ) -> Result<Skypetoken, AuthzError> {
        let mut skypetoken = self.exchange(spaces_token, kind).await?;
        skypetoken.expires_at = skypetoken.expires_at.min(spaces_expires_at);
        skypetoken.store(store, account).await?;
        Ok(skypetoken)
    }
}

/// HTTPS (or HTTP on a loopback host), a host, no user info, query or fragment, and `authz` as the last path segment.
fn is_authz_endpoint(url: &Url) -> bool {
    let loopback = url.host().is_some_and(|host| match host {
        url::Host::Domain(name) => name == "localhost",
        url::Host::Ipv4(address) => address.is_loopback(),
        url::Host::Ipv6(address) => address.is_loopback(),
    });
    let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && loopback);
    scheme_ok
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path_segments().and_then(Iterator::last) == Some("authz")
}

/// A fresh random (version 4) GUID for the request's correlation headers.
fn correlation_id() -> Result<String, AuthzError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| AuthzError::Random)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes.iter().fold(String::with_capacity(32), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

fn http_error(error: reqwest::Error) -> AuthzError {
    AuthzError::Http(error.without_url())
}

/// The error for a non-success answer.
fn refusal(status: StatusCode, headers: &HeaderMap, answer: &[u8]) -> AuthzError {
    let code = status.as_u16();
    match status {
        StatusCode::FORBIDDEN => AuthzError::Refused {
            status: code,
            code: error_code(answer),
        },
        StatusCode::UNAUTHORIZED => AuthzError::Unauthorized {
            claims_challenge: headers
                .get_all(WWW_AUTHENTICATE)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .any(|value| value.contains("insufficient_claims")),
        },
        StatusCode::TOO_MANY_REQUESTS => unavailable(code, headers),
        _ if status.is_server_error() => unavailable(code, headers),
        _ => AuthzError::UnexpectedResponse { status: code },
    }
}

fn unavailable(status: u16, headers: &HeaderMap) -> AuthzError {
    AuthzError::Unavailable {
        status,
        retry_after: retry_after(headers),
    }
}

/// Teams' `errorCode`, if the body is JSON with one shaped like an identifier.
fn error_code(answer: &[u8]) -> Option<String> {
    let Ok(Value::Object(fields)) = serde_json::from_slice::<Value>(answer) else {
        return None;
    };
    let code = fields.get("errorCode")?.as_str()?;
    let identifier = !code.is_empty()
        && code.len() <= ERROR_CODE_LIMIT
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    identifier.then(|| code.to_owned())
}

/// The skypetoken and region of a success answer. Failures name the field only.
fn parse_answer(answer: &[u8], received: SystemTime) -> Result<Skypetoken, AuthzError> {
    let Ok(Value::Object(mut fields)) = serde_json::from_slice::<Value>(answer) else {
        return Err(AuthzError::MissingField("JSON body"));
    };
    let (token, expires_in) = take_token(&mut fields).ok_or(AuthzError::MissingField("tokens.skypeToken"))?;
    let claims = payload(token.expose()).unwrap_or_default();
    let expires_at = time(&claims, "exp")
        .or_else(|| expires_in.and_then(|lifetime| received.checked_add(lifetime)))
        .ok_or(AuthzError::MissingField("tokens.expiresIn"))?;
    let text_field = |key: &str| fields.get(key).and_then(Value::as_str).map(str::to_owned);
    let endpoints = fields
        .get("regionGtms")
        .and_then(endpoints)
        .filter(|endpoints| !endpoints.is_empty())
        .ok_or(AuthzError::MissingField("regionGtms"))?;
    Ok(Skypetoken {
        expires_at,
        skype_id: text(&claims, "skypeid"),
        region: Region {
            name: text_field("region").or_else(|| text(&claims, "rgn")),
            partition: text_field("partition"),
            endpoints,
        },
        token,
    })
}

/// The skypetoken and its `expiresIn`: from a top-level `skypeToken` object first, else from `tokens`. Either spelling
/// of the token's field is taken (`skypeToken`, `skypetoken`), as the web client does.
fn take_token(fields: &mut Map<String, Value>) -> Option<(Secret, Option<Duration>)> {
    let container = match fields.get_mut("skypeToken") {
        Some(Value::Object(_)) => "skypeToken",
        _ => "tokens",
    };
    let Some(Value::Object(tokens)) = fields.get_mut(container) else {
        return None;
    };
    let token = ["skypeToken", "skypetoken"]
        .iter()
        .find_map(|key| match tokens.remove(*key) {
            Some(Value::String(token)) if !token.is_empty() => Some(Secret::from(token)),
            _ => None,
        })?;
    let expires_in = seconds(tokens.get("expiresIn"));
    Some((token, expires_in))
}

/// `regionGtms` as a map of its string entries; `None` when it isn't an object.
fn endpoints(value: &Value) -> Option<BTreeMap<String, String>> {
    let Value::Object(gtms) = value else {
        return None;
    };
    Some(
        gtms.iter()
            .filter_map(|(name, url)| url.as_str().map(|url| (name.clone(), url.to_owned())))
            .collect(),
    )
}

/// Seconds since the epoch, for showing and persisting an expiry.
#[must_use]
pub fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}
