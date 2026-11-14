//! Entra (v2 endpoint) tokens for the native Teams client: the authorize URL, the code and refresh-token grants, and
//! the error answers they give. `docs/protocol/auth.md` has the flow.
//!
//! The refresh token is kept in a [`SecretStore`](crate::secrets::SecretStore) by the methods in
//! [`refresh`](crate::refresh); this module only talks to the token endpoint. At `debug` the wire logger records the
//! full exchange with credentials redacted, including sent values echoed by an error. No error or `Debug` output
//! carries a token, code or verifier: Entra's error text is cut before its trace part and scrubbed of every secret
//! the request sent.

use std::fmt;
use std::time::{Duration, SystemTime};

use reqwest::StatusCode;
use reqwest::header::{CONTENT_TYPE, HeaderMap, RETRY_AFTER};
use serde_json::{Map, Value};
use url::form_urlencoded;

use crate::authz::AuthzError;
use crate::id_token::IdTokenError;
use crate::pkce::{Pkce, PkceError, random_state};
use crate::secrets::{Secret, SecretError};
use crate::session::{Clock, SystemClock};

/// The Teams desktop client (OneAuth): a public native client, so its refresh tokens have a sliding lifetime.
pub const TEAMS_NATIVE_CLIENT_ID: &str = "1fec8e78-bce4-4aaf-ab1b-5451cc387264";
/// The redirect registered for the native client: a page on Microsoft's login host.
pub const NATIVE_CLIENT_REDIRECT: &str = "https://login.microsoftonline.com/common/oauth2/nativeclient";
/// Work and school accounts of any tenant.
pub const ORGANIZATIONS_AUTHORITY: &str = "https://login.microsoftonline.com/organizations/";
/// The audience authz takes for the skypetoken exchange.
pub const SPACES_RESOURCE: &str = "https://api.spaces.skype.com";

/// Asked with every resource: the ID token, the profile and a refresh token.
const SIGN_IN_SCOPES: &str = "openid profile offline_access";
/// Resources that already name a scope and are passed as they are (the web client's list).
const NAMED_SCOPES: [&str; 2] = [
    "https://substrate.office.com/M365.Access",
    "https://graph.microsoft.com/Files.ReadWrite",
];
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Characters of Entra's error description kept in an [`EntraError`].
const DESCRIPTION_LIMIT: usize = 300;
const REDACTED: &str = "<redacted>";

/// Where and as whom to ask Entra for tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntraConfig {
    authority: String,
    client_id: String,
    redirect_uri: String,
}

impl EntraConfig {
    /// The native Teams client with the `nativeclient` redirect at the `organizations` authority.
    #[must_use]
    pub fn teams_native() -> Self {
        Self {
            authority: ORGANIZATIONS_AUTHORITY.to_owned(),
            client_id: TEAMS_NATIVE_CLIENT_ID.to_owned(),
            redirect_uri: NATIVE_CLIENT_REDIRECT.to_owned(),
        }
    }

    /// The token and authorization endpoints of the account's cloud.
    #[must_use]
    pub fn for_cloud(self, cloud: &crate::cloud::Cloud) -> Self {
        self.with_authority(cloud.authority())
    }

    /// Another authority (a tenant, or a mock server in tests): the base the `oauth2/v2.0/…` endpoints hang off.
    #[must_use]
    pub fn with_authority(mut self, authority: impl Into<String>) -> Self {
        self.authority = authority.into();
        if !self.authority.ends_with('/') {
            self.authority.push('/');
        }
        self
    }

    #[must_use]
    pub fn with_client_id(mut self, client_id: impl Into<String>) -> Self {
        self.client_id = client_id.into();
        self
    }

    /// Another redirect, such as the `msauth.com.microsoft.teams2://auth` fallback.
    #[must_use]
    pub fn with_redirect_uri(mut self, redirect_uri: impl Into<String>) -> Self {
        self.redirect_uri = redirect_uri.into();
        self
    }

    #[must_use]
    pub fn authority(&self) -> &str {
        &self.authority
    }

    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    #[must_use]
    pub fn authorize_endpoint(&self) -> String {
        format!("{}oauth2/v2.0/authorize", self.authority)
    }

    #[must_use]
    pub fn token_endpoint(&self) -> String {
        format!("{}oauth2/v2.0/token", self.authority)
    }
}

/// The `scope` of a token request: one resource's scope plus `openid profile offline_access`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope(String);

impl Scope {
    /// The scope for a resource, by the web client's rule: `<resource>/.default`, unless the resource already names
    /// a scope (it ends in `/.default`, or it's one of the few named scopes such as
    /// `https://substrate.office.com/M365.Access`). A resource ending in `/` gets `//.default`, as Entra expects.
    #[must_use]
    pub fn for_resource(resource: &str) -> Self {
        let scope = if resource.ends_with("/.default") || NAMED_SCOPES.contains(&resource) {
            resource.to_owned()
        } else {
            format!("{resource}/.default")
        };
        Self(format!("{scope} {SIGN_IN_SCOPES}"))
    }

    /// The scope for `https://api.spaces.skype.com`, the audience of the sign-in and of authz.
    #[must_use]
    pub fn spaces() -> Self {
        Self::for_resource(SPACES_RESOURCE)
    }

    /// A scope string taken as it is.
    #[must_use]
    pub fn exact(scope: impl Into<String>) -> Self {
        Self(scope.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One authorize request: the URL to open in the lab profile, and what the code exchange needs afterwards.
///
/// The caller opens [`url`](Self::url), takes `code` and `state` from the redirect, checks the state against
/// [`state`](Self::state), and hands the code to [`TokenClient::exchange_code`], which stores the refresh token only
/// once the answer's ID token names this account.
#[derive(Clone, Debug)]
pub struct Authorization {
    account: String,
    scope: Scope,
    pkce: Pkce,
    state: String,
    redirect_uri: String,
    url: String,
    expected_tenant: Option<String>,
    expected_object_id: Option<String>,
}

impl Authorization {
    /// A request for `account` (its UPN, sent as `login_hint`) with a fresh PKCE verifier and state.
    ///
    /// # Errors
    /// When the system random source fails.
    pub fn new(config: &EntraConfig, account: impl Into<String>, scope: Scope) -> Result<Self, PkceError> {
        Ok(Self::with_pkce(
            config,
            account,
            scope,
            Pkce::generate()?,
            random_state()?,
        ))
    }

    /// A request with a given verifier and state.
    #[must_use]
    pub fn with_pkce(
        config: &EntraConfig,
        account: impl Into<String>,
        scope: Scope,
        pkce: Pkce,
        state: String,
    ) -> Self {
        let account = account.into();
        let query = form_urlencoded::Serializer::new(String::new())
            .append_pair("client_id", config.client_id())
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", config.redirect_uri())
            .append_pair("scope", scope.as_str())
            .append_pair("code_challenge", pkce.challenge())
            .append_pair("code_challenge_method", Pkce::METHOD)
            .append_pair("state", &state)
            .append_pair("login_hint", &account)
            .finish();
        Self {
            url: format!("{}?{query}", config.authorize_endpoint()),
            account,
            scope,
            pkce,
            state,
            redirect_uri: config.redirect_uri().to_owned(),
            expected_tenant: None,
            expected_object_id: None,
        }
    }

    /// Also require the ID token's `tid` to be this tenant id.
    #[must_use]
    pub fn expect_tenant(mut self, tenant: impl Into<String>) -> Self {
        self.expected_tenant = Some(tenant.into());
        self
    }

    /// Also require the ID token's `oid` to be this object id (an account seen before).
    #[must_use]
    pub fn expect_object_id(mut self, object_id: impl Into<String>) -> Self {
        self.expected_object_id = Some(object_id.into());
        self
    }

    /// The authorize URL. It carries the challenge, never the verifier.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The account the request is for (sent as `login_hint`, which Entra doesn't enforce). Its refresh token is stored
    /// under this name, and only when the ID token names it too.
    #[must_use]
    pub fn account(&self) -> &str {
        &self.account
    }

    #[must_use]
    pub fn expected_tenant(&self) -> Option<&str> {
        self.expected_tenant.as_deref()
    }

    #[must_use]
    pub fn expected_object_id(&self) -> Option<&str> {
        self.expected_object_id.as_deref()
    }

    #[must_use]
    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    #[must_use]
    pub fn pkce(&self) -> &Pkce {
        &self.pkce
    }

    /// The `state` the redirect must carry back.
    #[must_use]
    pub fn state(&self) -> &str {
        &self.state
    }

    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }
}

/// An access token and what Entra said about it. `Debug` shows no token.
#[derive(Clone, Debug)]
pub struct Tokens {
    pub access_token: Secret,
    /// `Bearer`.
    pub token_type: String,
    /// The time of the request plus `expires_in`: on the early side.
    pub expires_at: SystemTime,
    /// The granted scopes, as Entra lists them.
    pub scopes: Vec<String>,
    pub id_token: Option<Secret>,
}

/// A successful grant: the tokens, and the refresh token if Entra sent one.
pub(crate) struct Grant {
    pub(crate) tokens: Tokens,
    pub(crate) refresh_token: Option<Secret>,
}

/// An error answer from Entra's token endpoint, without its trace part and without any secret the request sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntraError {
    /// The OAuth error, such as `invalid_grant`.
    pub error: String,
    /// The `AADSTS` numbers, from `error_codes` or else from the description.
    pub codes: Vec<u32>,
    /// The description after its `AADSTSnnn:` prefix and before `Trace ID:`, at most 300 characters.
    pub description: String,
    pub correlation_id: Option<String>,
    pub suberror: Option<String>,
}

impl fmt::Display for EntraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.error)?;
        for code in &self.codes {
            write!(f, " AADSTS{code}")?;
        }
        if !self.description.is_empty() {
            write!(f, ": {}", self.description)?;
        }
        Ok(())
    }
}

/// Why a token request failed. Never carries a token, code or verifier.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    /// Nothing to refresh with: the account has to sign in through the browser first.
    #[error("no refresh token is stored for {account}: sign in first")]
    NotSignedIn { account: String },
    /// The code or refresh token is no longer good (expired, revoked, already used): sign in again.
    #[error("the grant was refused, sign in again: {0}")]
    InvalidGrant(Box<EntraError>),
    /// Conditional Access refused this grant (`AADSTS53xxx`, `AADSTS530xxx`). The refresh token may still work for
    /// other audiences.
    #[error("Conditional Access refused the token: {0}")]
    ConditionalAccess(Box<EntraError>),
    /// The redirect URI isn't registered for the client (`AADSTS50011`, `AADSTS500113`).
    #[error("the redirect URI is not registered for the client: {0}")]
    RedirectMismatch(Box<EntraError>),
    /// Entra wants the user in front of a sign-in page (MFA, consent).
    #[error("Entra needs user interaction: {0}")]
    InteractionRequired(Box<EntraError>),
    /// 429 or 503 from Entra, or from authz during a [`Session`](crate::session::Session)'s skypetoken renewal: wait
    /// `retry_after` (when given) before asking again. Not retried here, nor by the session.
    #[error(
        "the token service is throttling requests{}",
        .retry_after.map_or_else(String::new, |wait| format!(", retry after {} s", wait.as_secs()))
    )]
    Throttled {
        retry_after: Option<Duration>,
        error: Option<Box<EntraError>>,
    },
    /// Any other Entra error answer.
    #[error("Entra answered {status}: {error}")]
    Entra { status: u16, error: Box<EntraError> },
    /// An answer that isn't Entra's JSON.
    #[error("unexpected answer from the token endpoint (HTTP {status})")]
    UnexpectedResponse { status: u16 },
    /// A success without a required field, or with one of the wrong type.
    #[error("the token answer has no valid {0}")]
    MissingField(&'static str),
    /// The code exchange's ID token is missing or unreadable, so the signed-in account can't be checked; nothing was
    /// stored.
    #[error(transparent)]
    IdToken(#[from] IdTokenError),
    /// The code exchange signed in another account than the one asked for (the profile holds someone else's
    /// session), or the ID token is for another client; nothing was stored.
    #[error("the sign-in is not for {expected}: the ID token's {claim} is {found}; nothing was stored")]
    WrongAccount {
        /// The mismatching claim: `aud`, `preferred_username`, `upn`, `tid` or `oid`.
        claim: &'static str,
        expected: String,
        found: String,
    },
    /// The request didn't get an answer.
    #[error("token request failed: {0}")]
    Http(#[source] reqwest::Error),
    #[error(transparent)]
    Store(#[from] SecretError),
    /// A [`Session`](crate::session::Session)'s skypetoken renewal was refused by authz (other than throttling).
    #[error("the skypetoken renewal failed: {0}")]
    Authz(#[source] Box<AuthzError>),
    /// A skypetoken renewal would have exchanged an Entra token that runs out within the session's margin: authz copies
    /// that token's expiry, so the renewal would give a skypetoken due for renewal at once. Refused by our code before
    /// anything is sent.
    #[error("the Entra token for the skypetoken renewal is too old; nothing was sent to authz")]
    StaleEntraToken,
}

impl From<AuthzError> for TokenError {
    /// Authz's throttling (429, 503) is [`TokenError::Throttled`], as Entra's is; a store failure is
    /// [`TokenError::Store`]; anything else is [`TokenError::Authz`].
    fn from(error: AuthzError) -> Self {
        match error {
            AuthzError::Unavailable {
                status: 429 | 503,
                retry_after,
            } => Self::Throttled {
                retry_after,
                error: None,
            },
            AuthzError::Store(error) => Self::Store(error),
            error => Self::Authz(Box::new(error)),
        }
    }
}

/// Talks to Entra's token endpoint for one client.
#[derive(Clone, Debug)]
pub struct TokenClient {
    http: reqwest::Client,
    config: EntraConfig,
}

impl TokenClient {
    /// A client with no proxy, no redirects and a 30 s timeout.
    ///
    /// # Errors
    /// When the HTTP client can't be built (no TLS backend).
    pub fn new(config: EntraConfig) -> Result<Self, TokenError> {
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(http_error)?;
        Ok(Self { http, config })
    }

    #[must_use]
    pub fn config(&self) -> &EntraConfig {
        &self.config
    }

    /// Redeems an authorization code (`grant_type=authorization_code`) with the request's verifier and redirect.
    pub(crate) async fn redeem_code(&self, authorization: &Authorization, code: &str) -> Result<Grant, TokenError> {
        let verifier = authorization.pkce().verifier().expose();
        let form = [
            ("client_id", self.config.client_id()),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", authorization.redirect_uri()),
            ("code_verifier", verifier),
            ("scope", authorization.scope().as_str()),
        ];
        self.grant(&form, &[code, verifier], &SystemClock).await
    }

    /// Redeems a refresh token for `scope` (`grant_type=refresh_token`); the expiry counts from `clock`'s time of the
    /// request.
    pub(crate) async fn redeem_refresh_token(
        &self,
        refresh_token: &Secret,
        scope: &Scope,
        clock: &dyn Clock,
    ) -> Result<Grant, TokenError> {
        let form = [
            ("client_id", self.config.client_id()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.expose()),
            ("scope", scope.as_str()),
        ];
        self.grant(&form, &[refresh_token.expose()], clock).await
    }

    /// POSTs one grant. `secrets` are the sent values an error text must never echo; `clock` dates the request, which
    /// the answer's `expires_in` counts from.
    async fn grant(&self, form: &[(&str, &str)], secrets: &[&str], clock: &dyn Clock) -> Result<Grant, TokenError> {
        let body = form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();
        let sent = clock.now();
        let request = self
            .http
            .post(self.config.token_endpoint())
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body);
        let response = crate::wire::exchange(&self.http, request, secrets)
            .await
            .map_err(http_error)?;
        let status = response.status;
        let retry_after = retry_after(&response.headers);
        let answer = response.body;
        if status.is_success() {
            parse_grant(&answer, status, sent)
        } else {
            Err(classify(status, retry_after, EntraError::parse(&answer, secrets)))
        }
    }
}

/// A reqwest error without its URL (the token endpoint carries no secret, but nothing needs it either).
fn http_error(error: reqwest::Error) -> TokenError {
    TokenError::Http(error.without_url())
}

/// The tokens of a success answer. Parse failures name the field only: a serde message could quote a value.
fn parse_grant(answer: &[u8], status: StatusCode, sent: SystemTime) -> Result<Grant, TokenError> {
    let Ok(Value::Object(mut fields)) = serde_json::from_slice::<Value>(answer) else {
        return Err(TokenError::UnexpectedResponse {
            status: status.as_u16(),
        });
    };
    let access_token = take_secret(&mut fields, "access_token").ok_or(TokenError::MissingField("access_token"))?;
    let expires_at = seconds(fields.get("expires_in"))
        .and_then(|lifetime| sent.checked_add(lifetime))
        .ok_or(TokenError::MissingField("expires_in"))?;
    let token_type = match fields.get("token_type") {
        None => "Bearer".to_owned(),
        Some(Value::String(token_type)) => token_type.clone(),
        Some(_) => return Err(TokenError::MissingField("token_type")),
    };
    let scopes = fields
        .get("scope")
        .and_then(Value::as_str)
        .map(|scope| scope.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default();
    Ok(Grant {
        tokens: Tokens {
            access_token,
            token_type,
            expires_at,
            scopes,
            id_token: take_secret(&mut fields, "id_token"),
        },
        refresh_token: take_secret(&mut fields, "refresh_token"),
    })
}

/// A non-empty string field, moved straight into a [`Secret`].
fn take_secret(fields: &mut Map<String, Value>, key: &str) -> Option<Secret> {
    match fields.remove(key)? {
        Value::String(value) if !value.is_empty() => Some(Secret::from(value)),
        _ => None,
    }
}

/// A lifetime field (`expires_in`, authz's `expiresIn`): a number of seconds, or the same as a string.
pub(crate) fn seconds(value: Option<&Value>) -> Option<Duration> {
    let seconds = match value? {
        Value::Number(number) => number.as_u64()?,
        Value::String(text) => text.trim().parse().ok()?,
        _ => return None,
    };
    Some(Duration::from_secs(seconds))
}

/// `Retry-After` as delta-seconds or an HTTP date (Entra's and authz's answers alike, and `teams-proto`'s transport
/// reads the chat service's the same way); a date in the past is no wait.
#[must_use]
pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse() {
        return Some(Duration::from_secs(seconds));
    }
    let when = httpdate::parse_http_date(value).ok()?;
    Some(when.duration_since(SystemTime::now()).unwrap_or_default())
}

fn classify(status: StatusCode, retry_after: Option<Duration>, error: Option<EntraError>) -> TokenError {
    if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
        return TokenError::Throttled {
            retry_after,
            error: error.map(Box::new),
        };
    }
    let Some(error) = error else {
        return TokenError::UnexpectedResponse {
            status: status.as_u16(),
        };
    };
    if error.codes.iter().any(|&code| is_conditional_access(code)) {
        TokenError::ConditionalAccess(Box::new(error))
    } else if error.codes.iter().any(|&code| matches!(code, 50_011 | 500_113)) {
        TokenError::RedirectMismatch(Box::new(error))
    } else if error.error == "interaction_required" {
        TokenError::InteractionRequired(Box::new(error))
    } else if error.error == "invalid_grant" {
        TokenError::InvalidGrant(Box::new(error))
    } else {
        TokenError::Entra {
            status: status.as_u16(),
            error: Box::new(error),
        }
    }
}

/// `AADSTS53xxx` and `AADSTS530xxx`: device compliance, approved apps, Conditional Access blocks, security defaults.
fn is_conditional_access(code: u32) -> bool {
    (53_000..54_000).contains(&code) || (530_000..540_000).contains(&code)
}

impl EntraError {
    /// Entra's JSON error answer, or `None` when the answer isn't one. Every string is scrubbed of `secrets`.
    fn parse(answer: &[u8], secrets: &[&str]) -> Option<Self> {
        let Ok(Value::Object(fields)) = serde_json::from_slice::<Value>(answer) else {
            return None;
        };
        let text = |key: &str| {
            fields
                .get(key)
                .and_then(Value::as_str)
                .map(|value| scrub(value, secrets))
        };
        let error = text("error")?;
        let (code_in_text, description) = split_description(&text("error_description").unwrap_or_default());
        let mut codes: Vec<u32> = fields
            .get("error_codes")
            .and_then(Value::as_array)
            .map(|codes| {
                codes
                    .iter()
                    .filter_map(Value::as_u64)
                    .filter_map(|code| u32::try_from(code).ok())
                    .collect()
            })
            .unwrap_or_default();
        if codes.is_empty() {
            codes.extend(code_in_text);
        }
        Some(Self {
            error,
            codes,
            description,
            correlation_id: text("correlation_id"),
            suberror: text("suberror"),
        })
    }
}

fn scrub(text: &str, secrets: &[&str]) -> String {
    secrets
        .iter()
        .filter(|secret| !secret.is_empty())
        .fold(text.to_owned(), |text, secret| text.replace(secret, REDACTED))
}

/// The leading `AADSTSnnn` number of a description, and the text after it up to `Trace ID:`, capped.
pub(crate) fn split_description(raw: &str) -> (Option<u32>, String) {
    let text = raw.find("Trace ID:").map_or(raw, |end| &raw[..end]).trim();
    let (code, rest) = text
        .strip_prefix("AADSTS")
        .and_then(|after| {
            let digits = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
            let code = after[..digits].parse().ok()?;
            Some((Some(code), after[digits..].trim_start_matches(':').trim_start()))
        })
        .unwrap_or((None, text));
    (code, rest.chars().take(DESCRIPTION_LIMIT).collect())
}
