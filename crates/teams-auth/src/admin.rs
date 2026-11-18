//! An own server's account management (`docs/server.md`, "Accounts and administration"): whether the server still needs
//! its first administrator and creating that one (`/api/admin/v1/setup`, asked before any account exists), an account
//! replacing its own password (`/api/account/v1/password`, by user name and current password: an account that must
//! change it holds no token), and what an administrator does to the accounts (`/api/admin/v1/users…`) with the
//! account's access token for `https://api.spaces.skype.com`.
//!
//! An administrator's apps and bots are in [`apps`].
//!
//! Only an own server has these: [`AdminClient::for_cloud`] refuses Microsoft's cloud, so no request of this module
//! can reach a Microsoft host, and every URL is built on the account's own origin.

use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{Map, Value, json};
use url::Url;

use crate::cloud::Cloud;
use crate::secrets::Secret;
use crate::token::retry_after;
use crate::wire;

mod apps;
pub use apps::{
    AdminApp, AdminBot, AppChanges, AppSource, BotAuthMode, BotHealth, BotMetadata, HealthState, MAX_PACKAGE, MadeApp,
    NewBot, TestActivity, apps_not_served,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest refusal message kept: the server's are one sentence.
const MESSAGE_LIMIT: usize = 300;

/// Why the server refused, by its `error.code`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The setup code is missing or wrong.
    SetupCode,
    /// The server has an administrator already.
    AdminExists,
    /// The user name or the current password isn't right.
    InvalidCredentials,
    /// The password is too short.
    WeakPassword,
    /// The new password is the current one.
    SamePassword,
    /// The user name isn't one the server takes.
    InvalidUpn,
    /// The display name isn't one the server takes.
    InvalidName,
    /// Another account has the user name.
    UpnTaken,
    /// The change would leave the server without an enabled administrator.
    LastAdmin,
    /// The account asked about is gone.
    NotFound,
    /// The asking account isn't an administrator (any more).
    NotAdmin,
    /// The asking account is disabled or deleted.
    AccountDisabled,
    /// An app's id is registered already.
    AppExists,
    /// A bot's id is another app's bot, or a person's.
    BotExists,
    /// A new package of an app names another bot.
    BotChanged,
    /// The app has no bot.
    NoBot,
    /// The bot's endpoint isn't one the server takes.
    InvalidEndpoint,
    /// The address written in the bot's endpoint is on a network the server doesn't send to.
    EndpointNotAllowed,
    /// The bot has no endpoint to send to.
    NoEndpoint,
    /// The app is turned off.
    AppDisabled,
    /// The server takes no unsigned bot.
    UnsignedNotAllowed,
    /// An account named isn't one the server has.
    UnknownUser,
    /// The app package isn't one the server takes.
    InvalidPackage,
    /// The app package is over the server's limit.
    PackageTooLarge,
    /// The request isn't one the server reads.
    InvalidRequest,
    /// A code this client doesn't know.
    Other,
}

impl Refusal {
    fn of(code: &str) -> Self {
        match code {
            "SetupCode" => Self::SetupCode,
            "AdminExists" => Self::AdminExists,
            "InvalidCredentials" => Self::InvalidCredentials,
            "WeakPassword" => Self::WeakPassword,
            "SamePassword" => Self::SamePassword,
            "InvalidUpn" => Self::InvalidUpn,
            "InvalidName" => Self::InvalidName,
            "UpnTaken" => Self::UpnTaken,
            "LastAdmin" => Self::LastAdmin,
            "NotFound" => Self::NotFound,
            "NotAdmin" => Self::NotAdmin,
            "AccountDisabled" => Self::AccountDisabled,
            "AppExists" => Self::AppExists,
            "BotExists" => Self::BotExists,
            "BotChanged" => Self::BotChanged,
            "NoBot" => Self::NoBot,
            "InvalidEndpoint" => Self::InvalidEndpoint,
            "EndpointNotAllowed" => Self::EndpointNotAllowed,
            "NoEndpoint" => Self::NoEndpoint,
            "AppDisabled" => Self::AppDisabled,
            "UnsignedNotAllowed" => Self::UnsignedNotAllowed,
            "UnknownUser" => Self::UnknownUser,
            "InvalidPackage" => Self::InvalidPackage,
            "PackageTooLarge" => Self::PackageTooLarge,
            "InvalidRequest" => Self::InvalidRequest,
            _ => Self::Other,
        }
    }
}

/// Why an admin request gave nothing. Never carries a password, a setup code or a token.
#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    /// The account is Microsoft's: it has no such service.
    #[error("only an own server has account management")]
    NotOwnServer,
    #[error("the account management address isn't usable")]
    InvalidOrigin,
    #[error("the server refused ({status}): {message}")]
    Refused {
        refusal: Refusal,
        status: u16,
        message: String,
    },
    /// Too many attempts: wait `retry_after` (when given).
    #[error("the server asks to wait before another attempt")]
    Throttled { retry_after: Option<Duration> },
    /// The token was refused: the account is signed out.
    #[error("the server refused the account's token")]
    Unauthorized,
    #[error("the server answered {status} in a form this client doesn't read")]
    UnexpectedResponse { status: u16 },
    #[error("the server can't be reached: {0}")]
    Http(#[source] reqwest::Error),
}

impl AdminError {
    /// The server's refusal, when that is what this is.
    #[must_use]
    pub fn refusal(&self) -> Option<Refusal> {
        match self {
            Self::Refused { refusal, .. } => Some(*refusal),
            _ => None,
        }
    }
}

/// Whether the server still needs its first administrator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetupState {
    pub needs_admin: bool,
    /// Creating it takes the one-time setup code the server printed.
    pub code_required: bool,
}

/// The first administrator, as the person setting the server up names it.
#[derive(Clone, Copy, Debug)]
pub struct FirstAdmin<'a> {
    pub code: Option<&'a Secret>,
    pub upn: &'a str,
    pub display_name: &'a str,
    pub password: &'a Secret,
}

/// An account as an administrator sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminUser {
    pub id: String,
    pub upn: String,
    pub display_name: String,
    pub enabled: bool,
    pub admin: bool,
    pub must_change_password: bool,
}

impl AdminUser {
    fn parse(value: &Value) -> Option<Self> {
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        let flag = |key: &str| value.get(key).and_then(Value::as_bool);
        Some(Self {
            id: text("id")?,
            upn: text("upn")?,
            display_name: text("display_name").unwrap_or_default(),
            enabled: flag("enabled")?,
            admin: flag("admin")?,
            must_change_password: flag("must_change_password").unwrap_or(false),
        })
    }
}

/// A page of accounts by user name; `next` asks for the page after it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserPage {
    pub users: Vec<AdminUser>,
    pub next: Option<String>,
}

/// A new account.
#[derive(Clone, Copy, Debug)]
pub struct NewUser<'a> {
    pub upn: &'a str,
    pub display_name: &'a str,
    pub password: &'a Secret,
    pub admin: bool,
    pub must_change_password: bool,
}

/// What to change of an account: each when given.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserChanges {
    pub display_name: Option<String>,
    pub upn: Option<String>,
    pub enabled: Option<bool>,
    pub admin: Option<bool>,
}

impl UserChanges {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Asks an own server's account management.
#[derive(Clone, Debug)]
pub struct AdminClient {
    http: reqwest::Client,
    origin: Url,
}

impl AdminClient {
    /// The client of `cloud`'s own server, trusting its certificate authority. No proxy, no redirects.
    ///
    /// # Errors
    /// [`AdminError::NotOwnServer`] for Microsoft's cloud; when the HTTP client can't be built.
    pub fn for_cloud(cloud: &Cloud) -> Result<Self, AdminError> {
        let origin = cloud.origin().ok_or(AdminError::NotOwnServer)?.clone();
        Self::build(origin, cloud)
    }

    /// A client for a server on loopback over plain HTTP (a test's mock); anything else is refused.
    ///
    /// # Errors
    /// [`AdminError::InvalidOrigin`] unless `origin` is `http://` on a loopback host with no path.
    pub fn with_loopback(origin: &str) -> Result<Self, AdminError> {
        let origin = Url::parse(origin).map_err(|_| AdminError::InvalidOrigin)?;
        let loopback = origin.host().is_some_and(|host| match host {
            url::Host::Domain(_) => false,
            url::Host::Ipv4(address) => address.is_loopback(),
            url::Host::Ipv6(address) => address.is_loopback(),
        });
        if origin.scheme() != "http" || !loopback || origin.path() != "/" || origin.query().is_some() {
            return Err(AdminError::InvalidOrigin);
        }
        Self::build(origin, &Cloud::Microsoft)
    }

    fn build(origin: Url, cloud: &Cloud) -> Result<Self, AdminError> {
        let http = cloud
            .sign_in_client()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(http_error)?;
        Ok(Self { http, origin })
    }

    /// The URL of `segments` under the server's origin, each segment escaped.
    fn url(&self, segments: &[&str]) -> Result<Url, AdminError> {
        let mut url = self.origin.clone();
        url.path_segments_mut()
            .map_err(|()| AdminError::InvalidOrigin)?
            .clear()
            .extend(segments);
        Ok(url)
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        secrets: &[&str],
    ) -> Result<(StatusCode, Value), AdminError> {
        let answer = wire::exchange(&self.http, request.header(ACCEPT, "application/json"), secrets)
            .await
            .map_err(http_error)?;
        let status = answer.status;
        let body = serde_json::from_slice::<Value>(&answer.body).unwrap_or(Value::Null);
        if status.is_success() {
            return Ok((status, body));
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
            return Err(AdminError::Throttled {
                retry_after: retry_after(&answer.headers),
            });
        }
        let error = body.get("error").and_then(Value::as_object);
        let code = error.and_then(|error| error.get("code")).and_then(Value::as_str);
        match code {
            Some(code) => Err(AdminError::Refused {
                refusal: Refusal::of(code),
                status: status.as_u16(),
                message: message_of(error, secrets),
            }),
            None if status == StatusCode::UNAUTHORIZED => Err(AdminError::Unauthorized),
            None => Err(AdminError::UnexpectedResponse {
                status: status.as_u16(),
            }),
        }
    }

    /// `GET /api/admin/v1/setup`.
    ///
    /// # Errors
    /// [`AdminError`]: no answer, or one that doesn't say.
    pub async fn setup_state(&self) -> Result<SetupState, AdminError> {
        let url = self.url(&["api", "admin", "v1", "setup"])?;
        let (status, body) = self.send(self.http.get(url), &[]).await?;
        let flag = |key: &str| body.get(key).and_then(Value::as_bool);
        match (flag("needs_admin"), flag("code_required")) {
            (Some(needs_admin), Some(code_required)) => Ok(SetupState {
                needs_admin,
                code_required,
            }),
            _ => Err(unexpected(status)),
        }
    }

    /// `POST /api/admin/v1/setup`: creates the server's first administrator.
    ///
    /// # Errors
    /// [`AdminError::Refused`] with [`Refusal::SetupCode`], [`Refusal::AdminExists`], [`Refusal::WeakPassword`],
    /// [`Refusal::InvalidUpn`] or [`Refusal::InvalidName`]; [`AdminError::Throttled`] after too many attempts.
    pub async fn create_first_admin(&self, new: FirstAdmin<'_>) -> Result<AdminUser, AdminError> {
        let url = self.url(&["api", "admin", "v1", "setup"])?;
        let mut body = Map::new();
        if let Some(code) = new.code {
            body.insert("code".to_owned(), json!(code.expose()));
        }
        body.insert("upn".to_owned(), json!(new.upn));
        body.insert("display_name".to_owned(), json!(new.display_name));
        body.insert("password".to_owned(), json!(new.password.expose()));
        let secrets = [new.password.expose(), new.code.map_or("", Secret::expose)];
        let (status, body) = self
            .send(json_body(self.http.post(url), &Value::Object(body)), &secrets)
            .await?;
        AdminUser::parse(&body).ok_or_else(|| unexpected(status))
    }

    /// `POST /api/account/v1/password`: the account `upn` replaces its password, proving the current one.
    ///
    /// # Errors
    /// [`AdminError::Refused`] with [`Refusal::InvalidCredentials`], [`Refusal::SamePassword`] or
    /// [`Refusal::WeakPassword`]; [`AdminError::Throttled`] while the account is held off.
    pub async fn change_password(&self, upn: &str, current: &Secret, new: &Secret) -> Result<(), AdminError> {
        let url = self.url(&["api", "account", "v1", "password"])?;
        let body = json!({
            "upn": upn,
            "current_password": current.expose(),
            "new_password": new.expose(),
        });
        let secrets = [current.expose(), new.expose()];
        self.send(json_body(self.http.post(url), &body), &secrets).await?;
        Ok(())
    }

    /// `GET /api/account/v1/me`: the account `token` names, as the server has it now.
    ///
    /// # Errors
    /// [`AdminError::Unauthorized`] for a token the server doesn't take; [`Refusal::AccountDisabled`].
    pub async fn me(&self, token: &Secret) -> Result<AdminUser, AdminError> {
        let url = self.url(&["api", "account", "v1", "me"])?;
        let (status, body) = self.send(bearer(self.http.get(url), token), &[token.expose()]).await?;
        AdminUser::parse(&body).ok_or_else(|| unexpected(status))
    }

    /// `GET /api/admin/v1/users`: up to `limit` accounts whose user name or display name holds `query`, after the
    /// user name `after`.
    ///
    /// # Errors
    /// [`AdminError`]: [`Refusal::NotAdmin`] for an account that isn't an administrator.
    pub async fn users(
        &self,
        token: &Secret,
        query: &str,
        after: Option<&str>,
        limit: u32,
    ) -> Result<UserPage, AdminError> {
        let mut url = self.url(&["api", "admin", "v1", "users"])?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("limit", &limit.to_string());
            if !query.is_empty() {
                pairs.append_pair("q", query);
            }
            if let Some(after) = after {
                pairs.append_pair("after", after);
            }
        }
        let (status, body) = self.send(bearer(self.http.get(url), token), &[token.expose()]).await?;
        let users = body
            .get("users")
            .and_then(Value::as_array)
            .ok_or_else(|| unexpected(status))?
            .iter()
            .map(AdminUser::parse)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| unexpected(status))?;
        let next = body.get("next").and_then(Value::as_str).map(str::to_owned);
        Ok(UserPage { users, next })
    }

    /// `POST /api/admin/v1/users`.
    ///
    /// # Errors
    /// [`AdminError::Refused`] with [`Refusal::UpnTaken`], [`Refusal::InvalidUpn`], [`Refusal::InvalidName`] or
    /// [`Refusal::WeakPassword`].
    pub async fn create_user(&self, token: &Secret, new: NewUser<'_>) -> Result<AdminUser, AdminError> {
        let url = self.url(&["api", "admin", "v1", "users"])?;
        let body = json!({
            "upn": new.upn,
            "display_name": new.display_name,
            "password": new.password.expose(),
            "admin": new.admin,
            "must_change_password": new.must_change_password,
        });
        let secrets = [token.expose(), new.password.expose()];
        let request = json_body(bearer(self.http.post(url), token), &body);
        let (status, body) = self.send(request, &secrets).await?;
        AdminUser::parse(&body).ok_or_else(|| unexpected(status))
    }

    /// `PATCH /api/admin/v1/users/{id}`.
    ///
    /// # Errors
    /// [`AdminError::Refused`] with [`Refusal::LastAdmin`], [`Refusal::UpnTaken`], [`Refusal::NotFound`], …
    pub async fn update_user(&self, token: &Secret, id: &str, changes: &UserChanges) -> Result<AdminUser, AdminError> {
        let url = self.url(&["api", "admin", "v1", "users", id])?;
        let mut body = Map::new();
        if let Some(name) = &changes.display_name {
            body.insert("display_name".to_owned(), json!(name));
        }
        if let Some(upn) = &changes.upn {
            body.insert("upn".to_owned(), json!(upn));
        }
        if let Some(enabled) = changes.enabled {
            body.insert("enabled".to_owned(), json!(enabled));
        }
        if let Some(admin) = changes.admin {
            body.insert("admin".to_owned(), json!(admin));
        }
        let request = json_body(bearer(self.http.patch(url), token), &Value::Object(body));
        let (status, body) = self.send(request, &[token.expose()]).await?;
        AdminUser::parse(&body).ok_or_else(|| unexpected(status))
    }

    /// `DELETE /api/admin/v1/users/{id}`.
    ///
    /// # Errors
    /// [`AdminError::Refused`] with [`Refusal::LastAdmin`] or [`Refusal::NotFound`].
    pub async fn delete_user(&self, token: &Secret, id: &str) -> Result<(), AdminError> {
        let url = self.url(&["api", "admin", "v1", "users", id])?;
        self.send(bearer(self.http.delete(url), token), &[token.expose()])
            .await?;
        Ok(())
    }

    /// `POST /api/admin/v1/users/{id}/password`: replaces the account's password, signing it out everywhere.
    ///
    /// # Errors
    /// [`AdminError::Refused`] with [`Refusal::WeakPassword`] or [`Refusal::NotFound`].
    pub async fn reset_password(
        &self,
        token: &Secret,
        id: &str,
        password: &Secret,
        must_change: bool,
    ) -> Result<(), AdminError> {
        let url = self.url(&["api", "admin", "v1", "users", id, "password"])?;
        let body = json!({"password": password.expose(), "must_change_password": must_change});
        let request = json_body(bearer(self.http.post(url), token), &body);
        self.send(request, &[token.expose(), password.expose()]).await?;
        Ok(())
    }
}

/// The letters and digits a suggested password is made of: 32 of them, none easily taken for another (`l`, `o`, `0`
/// and `1` are left out).
const PASSWORD_ALPHABET: &[u8; 32] = b"abcdefghijkmnpqrstuvwxyz23456789";

/// A password made up for an account an administrator adds or resets: four groups of five letters and digits (100
/// random bits), easy to read out and to type. Plain text: it is made to be shown to the administrator and copied.
///
/// # Errors
/// When the system's random source fails.
pub fn suggested_password() -> Result<String, getrandom::Error> {
    let mut random = [0_u8; 20];
    getrandom::fill(&mut random)?;
    let mut password = String::with_capacity(23);
    for (at, byte) in random.iter().enumerate() {
        if at > 0 && at % 5 == 0 {
            password.push('-');
        }
        password.push(char::from(PASSWORD_ALPHABET[usize::from(byte & 31)]));
    }
    Ok(password)
}

fn bearer(request: reqwest::RequestBuilder, token: &Secret) -> reqwest::RequestBuilder {
    request.header(AUTHORIZATION, format!("Bearer {}", token.expose()))
}

fn json_body(request: reqwest::RequestBuilder, body: &Value) -> reqwest::RequestBuilder {
    request.header(CONTENT_TYPE, "application/json").body(body.to_string())
}

/// The refusal's message, capped, without any secret the request sent.
fn message_of(error: Option<&Map<String, Value>>, secrets: &[&str]) -> String {
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    secrets
        .iter()
        .filter(|secret| !secret.is_empty())
        .fold(message.to_owned(), |message, secret| {
            message.replace(secret, "[redacted]")
        })
        .chars()
        .take(MESSAGE_LIMIT)
        .collect()
}

fn unexpected(status: StatusCode) -> AdminError {
    AdminError::UnexpectedResponse {
        status: status.as_u16(),
    }
}

fn http_error(error: reqwest::Error) -> AdminError {
    AdminError::Http(error.without_url())
}
