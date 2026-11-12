//! `proto-lab auth <a|b|c>`: the native sign-in of one lab account, with no human (M0 spike 1).
//!
//! 1. **Authorize** in the account's persistent profile, headless, with a redirect catcher. The authorize URL ([`Authorization`]: the native Teams client, the
//!    `nativeclient` redirect, PKCE, `login_hint`) is opened in the profile, whose Microsoft session signs it in
//!    silently. The browser catches Entra's redirect to the registered redirect URI and fails it
//!    ([`RedirectCatch`]); the code comes from that URL once its `state` is the request's. If Entra shows a page
//!    instead (the session has expired, or it wants an interaction), the Auth run stops without typing. Windows Login instead submits the roster password once on the native authorize page.
//! 2. **Code → tokens** in Rust ([`TokenClient::exchange_code`]): the refresh token is stored only once the ID token
//!    names the account (and its object id from `lab.toml`).
//! 3. **Refresh** for `api.spaces.skype.com`, proving the stored refresh token works; with `--audiences`, one refresh
//!    grant per other audience of the auth notes' table, to see what Conditional Access allows.
//! 4. **Skypetoken** through authz ([`AuthzClient`]), stored next to the refresh token; the account's region is
//!    persisted to `<data dir>/auth/<alias>.json` ([`save_region`]) for native HTTP probes and to the native client's
//!    `ngteams-lab/<alias>/regions/<oid>.json`. Its verified identity is provisioned in that alias's `accounts.toml`.
//!
//! The CLI keeps secrets in Linux Secret Service, Windows Credential Manager or the macOS lab keychain under
//! `ngteams-lab` ([`KEYRING_APPLICATION`]);
//! tests pass a memory store. Nothing prints or logs a token, a code or the redirect URL: the report
//! ([`AuthReport::lines`]) shows only each token's audience, expiry, scopes and tenant, and the skypetoken's expiry and
//! region, or the refusal.

use std::fmt::Write as _;
use std::future::Future;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde_json::json;
use teams_auth::authz::unix_seconds;
use teams_auth::authz::{AuthzClient, AuthzError, AuthzKind, Region, Skypetoken};
use teams_auth::claims::TokenSummary;
use teams_auth::id_token::Account;
use teams_auth::pkce::PkceError;
use teams_auth::redirect::RedirectError;
use teams_auth::secrets::{Secret, SecretStore};
use teams_auth::token::{Authorization, EntraConfig, Scope, TokenClient, TokenError, Tokens};
use tokio::sync::mpsc;

use crate::config::{Alias, LabConfig, LabPaths};
use crate::redirect_catch::{CatchError, RedirectCatch};
use crate::session::{LabSession, SessionError, SessionReport, SessionState};

/// The native lab client's fixed credential-store namespace. Browser profile files remain in `teams-lab`.
pub const KEYRING_APPLICATION: &str = "ngteams-lab";

pub use native::NativeTarget;
mod native;

/// The audiences `--audiences` asks a token for, besides `api.spaces.skype.com` (the auth notes' table).
pub const OTHER_AUDIENCES: [&str; 8] = [
    "https://templates.teams.microsoft.com",
    "https://chatsvcagg.teams.microsoft.com",
    "https://presence.teams.microsoft.com",
    "https://ic3.teams.office.com",
    "https://graph.microsoft.com",
    "https://outlook.office.com",
    "https://outlook.office.com/search",
    "https://substrate.office.com",
];

/// How long a sign-in page has to stand before the run gives up on a redirect: Entra may pass through a page of its
/// own that moves on by itself.
const SIGN_IN_GRACE: Duration = Duration::from_secs(5);
/// How often the session's events are looked at while waiting for the redirect.
const TICK: Duration = Duration::from_millis(250);

/// What to do beyond the sign-in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthOptions {
    /// How long the browser step may take, from opening the authorize URL to the redirect.
    pub timeout: Duration,
    /// Exchange the access token for a skypetoken.
    pub skypetoken: bool,
    /// Ask a token for each of [`OTHER_AUDIENCES`] too.
    pub other_audiences: bool,
}

/// Why native sign-in or account provisioning failed; tokens may already be stored.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[cfg(windows)]
    #[error(transparent)]
    Login(#[from] super::login::LoginError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("can't make the authorization request: {0}")]
    Pkce(#[from] PkceError),
    #[error("can't catch the redirect: {0}")]
    Catch(#[from] CatchError),
    #[error("the redirect: {0}")]
    Redirect(#[from] RedirectError),
    #[error(transparent)]
    Token(#[from] TokenError),
    #[error(
        "Entra showed a page instead of redirecting: the profile's session has expired or needs an interaction (run \
         `proto-lab login`); nothing was typed"
    )]
    InteractionNeeded,
    #[error("the authorize page went to {0} instead of redirecting")]
    Elsewhere(String),
    #[error("no redirect within {0:?}")]
    Timeout(Duration),
    #[error("the capture connection closed to the browser")]
    ConnectionLost,
    #[error("capture setup failed during the sign-in: {0}")]
    CaptureFailed(String),
    #[error("the account's MRI in lab.toml isn't 8:orgid:<object id>")]
    BadMri,
    #[error("cannot provision the native lab account: {0}")]
    Provision(#[from] std::io::Error),
}

/// One audience's token, or why there is none.
#[derive(Debug)]
pub struct AudienceOutcome {
    pub resource: String,
    pub token: Result<Option<TokenSummary>, TokenError>,
}

/// What a sign-in got. Holds no token.
#[derive(Debug)]
pub struct AuthReport {
    /// The browser session and capture setup status.
    pub browser: SessionReport,
    /// The code exchange's access token for `api.spaces.skype.com`.
    pub signed_in: Option<TokenSummary>,
    /// The access token from refreshing it.
    pub refreshed: Option<TokenSummary>,
    /// One per [`OTHER_AUDIENCES`] when asked.
    pub audiences: Vec<AudienceOutcome>,
    /// The skypetoken's expiry and the region, or the refusal; `None` when not asked.
    pub skypetoken: Option<Result<SkypetokenSummary, AuthzError>>,
}

/// What may be shown of a skypetoken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkypetokenSummary {
    pub expires_at: SystemTime,
    pub region: Region,
}

impl From<&Skypetoken> for SkypetokenSummary {
    fn from(skypetoken: &Skypetoken) -> Self {
        Self {
            expires_at: skypetoken.expires_at,
            region: skypetoken.region.clone(),
        }
    }
}

/// Signs `alias` in natively through its lab profile (see the module docs), keeping the refresh token and the
/// skypetoken in `store`. Provisions the alias's native `ngteams-lab/<alias>/accounts.toml` and, when authz
/// succeeds, `regions/<oid>.json`.
///
/// # Errors
/// [`AuthError`] if the native target cannot be resolved or overlaps personal files (before sign-in), the browser
/// could not launch or capture setup failed, Entra didn't redirect, the redirect isn't this request's or carries
/// Entra's refusal, or the code exchange or first refresh failed. Native file writes can fail after tokens are
/// already stored. A refused audience or skypetoken is in the report instead.
pub async fn authorize<S: SecretStore>(
    config: &LabConfig,
    alias: Alias,
    store: &S,
    options: &AuthOptions,
) -> Result<AuthReport, AuthError> {
    let target = NativeTarget::from_env(alias)?;
    authorize_target(config, alias, store, options, target).await
}

/// Provisions a verified account into an explicitly selected isolated native target.
///
/// # Errors
/// The same browser, identity, token and private-file failures as [`authorize`].
pub async fn authorize_target<S: SecretStore>(
    config: &LabConfig,
    alias: Alias,
    store: &S,
    options: &AuthOptions,
    target: NativeTarget,
) -> Result<AuthReport, AuthError> {
    #[cfg(windows)]
    target.expect_account(&config.account(alias).upn)?;
    let entra = EntraConfig::teams_native();
    authorize_with(
        config,
        alias,
        store,
        options,
        entra.clone(),
        AuthSteps {
            target,
            redirect: async |authorization: &Authorization| {
                redirect_through_profile(config.paths(), alias, &entra, authorization, options.timeout).await
            },
            skype: async |tokens: &Tokens| skypetoken(store, &config.account(alias).upn, tokens).await,
        },
    )
    .await
}

/// Windows lab provisioning, using the existing dedicated password state machine on the native authorize page.
///
/// # Errors
/// Account mismatch, MFA/rejection, invalid redirect/identity, exchange, or private publication fails.
#[cfg(windows)]
pub async fn authorize_password<S: SecretStore>(
    config: &LabConfig,
    alias: Alias,
    store: &S,
    options: &AuthOptions,
    target: NativeTarget,
    credentials: &crate::accounts::Credentials,
) -> Result<AuthReport, AuthError> {
    let upn = &config.account(alias).upn;
    if !credentials.email().eq_ignore_ascii_case(upn) {
        return Err(super::login::LoginError::Accounts(crate::accounts::AccountsError::EmailMismatch(alias)).into());
    }
    target.expect_account(upn)?;
    authorize_with(
        config,
        alias,
        store,
        options,
        EntraConfig::teams_native(),
        AuthSteps {
            target,
            redirect: async |authorization: &Authorization| {
                super::login::authorization_profile(config.paths(), alias, authorization, credentials, options.timeout)
                    .await
                    .map_err(AuthError::from)
            },
            skype: async |tokens: &Tokens| skypetoken(store, upn, tokens).await,
        },
    )
    .await
}

/// Completes Windows native provisioning from a refresh token whose account was already verified by code exchange.
///
/// # Errors
/// Existing native/roster identity mismatch, refresh, authz or private publication fails.
#[cfg(windows)]
pub async fn refresh_native<S: SecretStore>(
    config: &LabConfig,
    alias: Alias,
    store: &S,
    target: &NativeTarget,
) -> Result<SkypetokenSummary, AuthError> {
    refresh_native_with(
        config,
        alias,
        store,
        target,
        &TokenClient::new(EntraConfig::teams_native())?,
        &AuthzClient::new().map_err(TokenError::from)?,
    )
    .await
}

#[cfg(windows)]
async fn refresh_native_with<S: SecretStore>(
    config: &LabConfig,
    alias: Alias,
    store: &S,
    target: &NativeTarget,
    client: &TokenClient,
    authz: &AuthzClient,
) -> Result<SkypetokenSummary, AuthError> {
    let configured = config.account(alias);
    let account = target.account(&configured.upn)?;
    if !configured
        .mri
        .as_ref()
        .is_some_and(|mri| mri.eq_ignore_ascii_case(&format!("8:orgid:{}", account.object_id)))
    {
        return Err(AuthError::BadMri);
    }
    let tokens = client.refresh(store, &configured.upn, &Scope::spaces()).await?;
    let skype = skypetoken_with(store, &configured.upn, &tokens, authz)
        .await
        .map_err(TokenError::from)?;
    target.save(&account, Some(&skype.region))?;
    Ok(skype)
}

struct AuthSteps<D, K> {
    target: NativeTarget,
    redirect: D,
    skype: K,
}

async fn authorize_with<S: SecretStore>(
    config: &LabConfig,
    alias: Alias,
    store: &S,
    options: &AuthOptions,
    entra: EntraConfig,
    steps: AuthSteps<
        impl AsyncFnOnce(&Authorization) -> Result<(Secret, SessionReport), AuthError>,
        impl AsyncFnOnce(&Tokens) -> Result<SkypetokenSummary, AuthzError>,
    >,
) -> Result<AuthReport, AuthError> {
    let account = config.account(alias);
    let mut authorization = Authorization::new(&entra, &account.upn, Scope::spaces())?;
    if let Some(mri) = &account.mri {
        let object_id = mri.strip_prefix("8:orgid:").ok_or(AuthError::BadMri)?;
        authorization = authorization.expect_object_id(object_id);
    }
    let (redirect, browser) = (steps.redirect)(&authorization).await?;
    let code = authorization.code_from_redirect(redirect.expose())?;

    let client = TokenClient::new(entra)?;
    let sign_in = client.exchange_code(store, &authorization, code.expose()).await?;
    let refreshed = client.refresh(store, &account.upn, &Scope::spaces()).await?;
    let mut audiences = Vec::new();
    if options.other_audiences {
        for resource in OTHER_AUDIENCES {
            let token = client
                .refresh(store, &account.upn, &Scope::for_resource(resource))
                .await;
            audiences.push(AudienceOutcome {
                resource: resource.to_owned(),
                token: token.map(|tokens| TokenSummary::read(&tokens.access_token)),
            });
        }
    }
    let skypetoken = if options.skypetoken {
        Some((steps.skype)(&refreshed).await)
    } else {
        None
    };
    let report = AuthReport {
        browser,
        signed_in: TokenSummary::read(&sign_in.tokens.access_token),
        refreshed: TokenSummary::read(&refreshed.access_token),
        audiences,
        skypetoken,
    };
    let identity = Account {
        username: account.upn.clone(),
        ..sign_in.account
    };
    steps.target.save(
        &identity,
        report
            .skypetoken
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map(|token| &token.region),
    )?;
    Ok(report)
}

async fn skypetoken<S: SecretStore>(
    store: &S,
    account: &str,
    spaces: &Tokens,
) -> Result<SkypetokenSummary, AuthzError> {
    skypetoken_with(store, account, spaces, &AuthzClient::new()?).await
}

async fn skypetoken_with<S: SecretStore>(
    store: &S,
    account: &str,
    spaces: &Tokens,
    client: &AuthzClient,
) -> Result<SkypetokenSummary, AuthzError> {
    let skypetoken = client
        .exchange_and_store(
            store,
            account,
            &spaces.access_token,
            spaces.expires_at,
            AuthzKind::ExplicitLogin,
        )
        .await?;
    Ok(SkypetokenSummary::from(&skypetoken))
}

/// Opens the authorize URL in `alias`'s browser profile and waits for the redirect the browser catches,
/// the two together bounded by `timeout` ([`within`]). The browser is closed whatever happened; a capture setup failure
/// is reported even if a redirect arrived. The browser report comes back with the redirect only; on an error the report adds nothing to it.
async fn redirect_through_profile(
    paths: &LabPaths,
    alias: Alias,
    entra: &EntraConfig,
    authorization: &Authorization,
    timeout: Duration,
) -> Result<(Secret, SessionReport), AuthError> {
    let (catch, mut caught) = RedirectCatch::new(&entra.authorize_endpoint(), entra.redirect_uri())?;
    let session = LabSession::open_catching(paths, alias, catch).await?;
    redirect_from_session(session, authorization, &mut caught, timeout).await
}

async fn redirect_from_session(
    mut session: LabSession,
    authorization: &Authorization,
    caught: &mut mpsc::UnboundedReceiver<Secret>,
    timeout: Duration,
) -> Result<(Secret, SessionReport), AuthError> {
    let redirect = within(timeout, async {
        session.navigate(authorization.url()).await?;
        wait_for_redirect(&mut session, caught).await
    })
    .await;
    let browser = session.close().await?;
    if browser.failed() {
        return Err(AuthError::CaptureFailed(browser.summary()));
    }
    Ok((redirect?, browser))
}

/// `step`, given up as [`AuthError::Timeout`] once `timeout` has passed, wherever it is waiting: the browser's
/// navigation (which doesn't answer until the page commits) as much as the wait for the redirect.
///
/// # Errors
/// `step`'s error, or [`AuthError::Timeout`].
pub async fn within<T>(timeout: Duration, step: impl Future<Output = Result<T, AuthError>>) -> Result<T, AuthError> {
    tokio::time::timeout(timeout, step)
        .await
        .unwrap_or(Err(AuthError::Timeout(timeout)))
}

/// Pumps the session's events until the browser catches the redirect. A sign-in page that stands for
/// [`SIGN_IN_GRACE`], a page on another host or a lost connection end the wait; the caller bounds it in time.
async fn wait_for_redirect(
    session: &mut LabSession,
    caught: &mut mpsc::UnboundedReceiver<Secret>,
) -> Result<Secret, AuthError> {
    let mut sign_in_since = None;
    loop {
        if let Ok(redirect) = caught.try_recv() {
            return Ok(redirect);
        }
        let now = tokio::time::Instant::now();
        match session.watch().state() {
            SessionState::SignInShown => {
                let since = *sign_in_since.get_or_insert(now);
                if now.saturating_duration_since(since) >= SIGN_IN_GRACE {
                    return Err(AuthError::InteractionNeeded);
                }
            }
            SessionState::Elsewhere(host) => return Err(AuthError::Elsewhere(host)),
            _ => sign_in_since = None,
        }
        if !session.pump(TICK).await {
            return Err(AuthError::ConnectionLost);
        }
    }
}

/// Where [`save_region`] keeps an account's region ([`LabPaths::region_file`]).
#[must_use]
pub fn region_file(paths: &LabPaths, alias: Alias) -> PathBuf {
    paths.region_file(alias)
}

/// Persists the account's region (`regionGtms` and all) and the skypetoken's expiry to [`region_file`], privately and
/// atomically. Unix uses `0700`/`0600`; Windows uses the current user's protected DACL. Concurrent saves never share
/// a staging file (the last rename wins, whole). Not the token.
///
/// # Errors
/// When the file can't be written.
pub fn save_region(paths: &LabPaths, alias: Alias, skypetoken: &SkypetokenSummary) -> std::io::Result<PathBuf> {
    let file = region_file(paths, alias);
    let mut record = skypetoken.region.to_json();
    record["account"] = json!(alias.as_str());
    record["fetchedAt"] = json!(unix_seconds(SystemTime::now()));
    record["skypetokenExpiresAt"] = json!(unix_seconds(skypetoken.expires_at));
    let text = serde_json::to_string_pretty(&record).map_err(std::io::Error::other)?;
    #[cfg(windows)]
    crate::private::atomic(&file, text.as_bytes())?;
    #[cfg(unix)]
    save_region_text(paths, alias, &file, &text)?;
    Ok(file)
}

#[cfg(unix)]
fn save_region_text(paths: &LabPaths, alias: Alias, file: &std::path::Path, text: &str) -> std::io::Result<()> {
    let dir = file.parent().unwrap_or(paths.data_dir());
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    // a uniquely named staging file, removed if anything fails before the rename
    let mut staging = tempfile::Builder::new()
        .prefix(&format!(".{}.json.", alias.as_str()))
        .suffix(".partial")
        .permissions(std::fs::Permissions::from_mode(0o600))
        .tempfile_in(dir)?;
    std::io::Write::write_all(&mut staging, text.as_bytes())?;
    staging.as_file().sync_all()?;
    staging.persist(file).map_err(|error| error.error)?;
    Ok(())
}

impl AuthReport {
    /// The report as lines for the operator, as of `now`: claims and expiries only, never a token.
    #[must_use]
    pub fn lines(&self, now: SystemTime) -> Vec<String> {
        let mut lines = vec![self.browser.summary()];
        lines.push(format!("signed in: {}", summary_line(self.signed_in.as_ref(), now)));
        lines.push(format!("refreshed: {}", summary_line(self.refreshed.as_ref(), now)));
        for audience in &self.audiences {
            let outcome = match &audience.token {
                Ok(summary) => summary_line(summary.as_ref(), now),
                Err(error) => format!("refused: {error}"),
            };
            lines.push(format!("{}: {outcome}", audience.resource));
        }
        match &self.skypetoken {
            None => {}
            Some(Ok(skypetoken)) => lines.push(skypetoken_line(skypetoken, now)),
            Some(Err(error)) => lines.push(format!("skypetoken: {error}")),
        }
        lines
    }
}

pub(crate) fn summary_line(summary: Option<&TokenSummary>, now: SystemTime) -> String {
    let Some(summary) = summary else {
        return "an opaque token (no claims to show)".to_owned();
    };
    let mut line = format!("aud {}", summary.audience.join(" "));
    if let Some(expires_at) = summary.expires_at {
        let _ = write!(line, ", exp {}", when(expires_at, now));
    }
    let _ = write!(line, ", scopes {}", summary.scopes.join(" "));
    if let Some(tenant) = &summary.tenant {
        let _ = write!(line, ", tenant {tenant}");
    }
    line
}

fn skypetoken_line(skypetoken: &SkypetokenSummary, now: SystemTime) -> String {
    let region = &skypetoken.region;
    format!(
        "skypetoken: expires {}, region {}, partition {}, {} regional endpoint(s)",
        when(skypetoken.expires_at, now),
        region.name.as_deref().unwrap_or("?"),
        region.partition.as_deref().unwrap_or("?"),
        region.endpoints.len()
    )
}

/// An HTTP date and how far off it is.
fn when(time: SystemTime, now: SystemTime) -> String {
    let relative = match time.duration_since(now) {
        Ok(ahead) => format!("in {} min", ahead.as_secs() / 60),
        Err(past) => format!("{} min ago", past.duration().as_secs() / 60),
    };
    format!("{} ({relative})", httpdate::fmt_http_date(time))
}

#[cfg(test)]
#[path = "../../tests/support/commands/auth.rs"]
mod tests;
