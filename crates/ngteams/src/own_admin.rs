//! An own server's account management from the app (`docs/personal-build.md`, "Own-server accounts"):
//!
//! - before a sign-in: whether the server still needs its first administrator ([`needs_admin`]), creating that one
//!   ([`create_first_admin`]) and replacing a password the server says must change ([`change_password`]); each asks
//!   the server the user named and nothing else, with no token;
//! - for a running account: [`AdminService`], what the Manage users page asks (`teams_ui::user_admin`), with the
//!   account's own access token from its session, attached only when the server says the account is an
//!   administrator ([`AdminService::is_admin`]); the same service answers the Manage apps page
//!   (`teams_ui::app_admin`) once the server also serves apps ([`AdminService::serves_apps`]).
//!
//! Only an own server's account has any of it: a Microsoft 365 account gets no service, and nothing here is sent to
//! a Microsoft host (`teams_auth::admin` builds every URL on the account's own origin).

use std::sync::Arc;

use teams_auth::admin::{
    AdminApp, AdminClient, AdminError, AdminUser, AppChanges, FirstAdmin, MadeApp, NewBot, NewUser, Refusal,
    TestActivity, UserChanges, UserPage, apps_not_served,
};
use teams_auth::secrets::{Secret, SecretStore};
use teams_auth::session::Session;
use teams_auth::token::{SPACES_RESOURCE, TokenError};
use teams_ui::account_prompt::SAME_PASSWORD;
use teams_ui::app_admin::{AppAdmin, AppProblem, BotConnection, BotForm};
use teams_ui::user_admin::{AdminProblem, NewAccount, UserAdmin};
use tokio::sync::watch;

use crate::accounts::runtime;
use crate::accounts_file::{OwnServer, PersonalAccount};

/// How many accounts a page of Manage users lists.
const PAGE: u32 = 50;

/// What the sign-in screen says when a wrong setup code was typed.
pub const WRONG_SETUP_CODE: &str =
    "The setup code isn't right. Run ngteams-server setup-code on the server for a new one.";

/// What it says when the server got its administrator meanwhile.
pub const ADMIN_EXISTS: &str = "This server has an administrator now. Sign in with your account.";

/// The first administrator as the sign-in screen's form holds it.
#[derive(Clone, Copy, Debug)]
pub struct NewAdmin<'a> {
    pub code: Option<&'a Secret>,
    pub upn: &'a str,
    pub display_name: &'a str,
    pub password: &'a Secret,
}

/// Why the first administrator wasn't created: what the user is told, and whether the server has one now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupFailed {
    pub words: String,
    /// The setup code was the problem: its field is asked again.
    pub code: bool,
    /// The server has an administrator: the form no longer applies.
    pub admin_exists: bool,
}

impl std::fmt::Display for SetupFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.words)
    }
}

impl std::error::Error for SetupFailed {}

fn client(server: &OwnServer) -> Result<AdminClient, String> {
    let cloud = server.cloud().map_err(|error| error.to_string())?;
    AdminClient::for_cloud(&cloud).map_err(|error| error.to_string())
}

/// What the user is told of `error` before any account is signed in.
fn said(server: &OwnServer, error: &AdminError) -> String {
    match error {
        AdminError::Refused { refusal, message, .. } => match refusal {
            Refusal::SetupCode => WRONG_SETUP_CODE.to_owned(),
            Refusal::AdminExists => ADMIN_EXISTS.to_owned(),
            Refusal::InvalidCredentials => "The user name or password isn't right.".to_owned(),
            Refusal::SamePassword => SAME_PASSWORD.to_owned(),
            // said as the Manage users page says them
            Refusal::WeakPassword => AdminProblem::WeakPassword.words().to_string(),
            Refusal::InvalidUpn => AdminProblem::InvalidUpn.words().to_string(),
            Refusal::InvalidName => AdminProblem::InvalidName.words().to_string(),
            _ => message.clone(),
        },
        AdminError::Throttled { .. } => "Too many attempts: wait a few minutes, then try again.".to_owned(),
        AdminError::Http(error) if crate::signin::untrusted_certificate(error) => format!(
            "{}'s certificate isn't trusted. If the server has its own certificate authority, enter its file under \
             Certificate authority file.",
            server.origin
        ),
        AdminError::Http(_) => format!("{} can't be reached.", server.origin),
        other => other.to_string(),
    }
}

/// Whether the own server `server` still needs its first administrator, and whether creating one takes the setup
/// code (blocking: run it off GPUI's foreground). `None` when the server has one.
///
/// # Errors
/// What kept the server from answering (it can't be reached, its CA file can't be read).
pub fn needs_admin(server: &OwnServer) -> Result<Option<bool>, String> {
    let client = client(server)?;
    let state = runtime()?
        .block_on(client.setup_state())
        .map_err(|error| said(server, &error))?;
    Ok(state.needs_admin.then_some(state.code_required))
}

/// Creates the first administrator of `server` (blocking). The account then signs in as any other.
///
/// # Errors
/// [`SetupFailed`], as the user is told it.
pub fn create_first_admin(server: &OwnServer, new: NewAdmin<'_>) -> Result<(), SetupFailed> {
    let failed = |words: String| SetupFailed {
        words,
        code: false,
        admin_exists: false,
    };
    let client = client(server).map_err(failed)?;
    let first = FirstAdmin {
        code: new.code,
        upn: new.upn,
        display_name: new.display_name,
        password: new.password,
    };
    let created = runtime().map_err(failed)?.block_on(client.create_first_admin(first));
    match created {
        Ok(_) => {
            log::info!("an own server's first administrator was created");
            Ok(())
        }
        Err(error) => Err(SetupFailed {
            words: said(server, &error),
            code: error.refusal() == Some(Refusal::SetupCode),
            admin_exists: error.refusal() == Some(Refusal::AdminExists),
        }),
    }
}

/// Replaces the password of `upn` on `server`, proving the `current` one (blocking).
///
/// # Errors
/// Why it wasn't replaced, as the user is told it.
pub fn change_password(server: &OwnServer, upn: &str, current: &Secret, new: &Secret) -> Result<(), String> {
    let client = client(server)?;
    runtime()?
        .block_on(client.change_password(upn, current, new))
        .map_err(|error| said(server, &error))?;
    log::info!("an own server's account replaced its password");
    Ok(())
}

/// What the page says when the account's token wasn't had: a server that can't be reached or asks to wait is said
/// so, an account whose sign-in is gone is signed out, and anything else (a locked keyring) is named.
fn token_problem(error: &TokenError) -> AdminProblem {
    match error {
        TokenError::Http(_) => AdminProblem::Unreachable,
        TokenError::Throttled { .. } => AdminProblem::Wait,
        TokenError::NotSignedIn { .. } | TokenError::InvalidGrant(_) => AdminProblem::SignedOut,
        other => {
            log::warn!("an own server's account had no token for Manage users: {other}");
            AdminProblem::Other(format!("Your sign-in couldn't be read: {other}"))
        }
    }
}

/// What the Manage users page of one own server's account asks (see the module docs).
pub struct AdminService<S> {
    client: AdminClient,
    session: Arc<Session<S>>,
    /// The runtime the session's own requests run on (the engine's): a token refresh asked here runs there, so the
    /// token client's pooled connection stays driven by the runtime that opened it.
    sessions: tokio::runtime::Handle,
    /// Its own runtime, for the page's requests to the admin API alone.
    runtime: tokio::runtime::Runtime,
    /// Set once the server said the account is no administrator any more ([`AdminService::demoted`]).
    demoted: watch::Sender<bool>,
}

impl<S> std::fmt::Debug for AdminService<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminService").finish_non_exhaustive()
    }
}

impl<S: SecretStore + 'static> AdminService<S> {
    /// The service of `account` over its `session`, whose requests run on `sessions` (the account's engine's runtime,
    /// `teams_core::EngineHandle::runtime`); `None` for a Microsoft 365 account, or when its server's address or CA
    /// file can't be used.
    #[must_use]
    pub fn of(account: &PersonalAccount, session: Arc<Session<S>>, sessions: tokio::runtime::Handle) -> Option<Self> {
        account.server.as_ref()?;
        let cloud = account.cloud().ok()?;
        Some(Self {
            client: AdminClient::for_cloud(&cloud).ok()?,
            session,
            sessions,
            runtime: runtime().ok()?,
            demoted: watch::Sender::new(false),
        })
    }

    /// Turns `true` once the server says the account is no administrator any more: Manage users then goes.
    #[must_use]
    pub fn demoted(&self) -> watch::Receiver<bool> {
        self.demoted.subscribe()
    }

    /// The account's access token, taken on the session's own runtime.
    fn token(&self) -> Result<Secret, AdminProblem> {
        let session = Arc::clone(&self.session);
        let token = self.runtime.block_on(
            self.sessions
                .spawn(async move { session.access_token(SPACES_RESOURCE).await }),
        );
        match token {
            Ok(token) => Ok(token.map_err(|error| token_problem(&error))?.token),
            // the engine stopped: the account is being removed, or the app quits
            Err(_) => Err(AdminProblem::SignedOut),
        }
    }

    /// Runs `ask` with the account's access token; a refusal that says the account is no administrator any more is
    /// told ([`AdminService::demoted`]).
    fn asked_as<T, P: From<AdminError> + From<AdminProblem>>(
        &self,
        ask: impl AsyncFnOnce(&AdminClient, &Secret) -> Result<T, AdminError>,
    ) -> Result<T, P> {
        let token = self.token()?;
        let asked = self.runtime.block_on(ask(&self.client, &token));
        if asked
            .as_ref()
            .is_err_and(|error| error.refusal() == Some(Refusal::NotAdmin))
        {
            self.demoted.send_replace(true);
        }
        asked.map_err(P::from)
    }

    /// Runs `ask` with the account's access token, for Manage users.
    fn asked<T>(
        &self,
        ask: impl AsyncFnOnce(&AdminClient, &Secret) -> Result<T, AdminError>,
    ) -> Result<T, AdminProblem> {
        self.asked_as(ask)
    }

    /// Whether the server serves apps to its administrator (blocking): Manage apps is offered only then.
    ///
    /// # Errors
    /// [`AdminProblem`] when the server didn't say.
    pub fn serves_apps(&self) -> Result<bool, AdminProblem> {
        let token = self.token()?;
        match self.runtime.block_on(self.client.apps(&token)) {
            Ok(_) => Ok(true),
            Err(error) if apps_not_served(&error) => Ok(false),
            Err(error) => Err(AdminProblem::from(error)),
        }
    }

    /// Whether the server says the account is an administrator now (blocking).
    ///
    /// # Errors
    /// [`AdminProblem`] when the server didn't say.
    pub fn is_admin(&self) -> Result<bool, AdminProblem> {
        let me: AdminUser = self.asked(async |client, token| client.me(token).await)?;
        Ok(me.admin && me.enabled)
    }
}

impl<S: SecretStore + 'static> UserAdmin for AdminService<S> {
    fn list(&self, query: &str, after: Option<&str>) -> Result<UserPage, AdminProblem> {
        self.asked(async |client, token| client.users(token, query, after, PAGE).await)
    }

    fn add(&self, new: &NewAccount) -> Result<AdminUser, AdminProblem> {
        let user = NewUser {
            upn: &new.upn,
            display_name: &new.display_name,
            password: &new.password,
            admin: new.admin,
            must_change_password: new.must_change_password,
        };
        self.asked(async |client, token| client.create_user(token, user).await)
    }

    fn change(&self, id: &str, changes: &UserChanges) -> Result<AdminUser, AdminProblem> {
        self.asked(async |client, token| client.update_user(token, id, changes).await)
    }

    fn delete(&self, id: &str) -> Result<(), AdminProblem> {
        self.asked(async |client, token| client.delete_user(token, id).await)
    }

    fn reset_password(&self, id: &str, password: &Secret, must_change: bool) -> Result<(), AdminProblem> {
        self.asked(async |client, token| client.reset_password(token, id, password, must_change).await)
    }
}

impl<S: SecretStore + 'static> AppAdmin for AdminService<S> {
    fn apps(&self) -> Result<Vec<AdminApp>, AppProblem> {
        self.asked_as(async |client, token| client.apps(token).await)
    }

    fn register(&self, form: &BotForm) -> Result<MadeApp, AppProblem> {
        let new = NewBot {
            name: &form.name,
            description: &form.description,
            endpoint: form.endpoint.as_deref(),
            auth_mode: form.auth_mode,
        };
        self.asked_as(async |client, token| client.register_bot(token, new).await)
    }

    fn upload(&self, package: Vec<u8>) -> Result<MadeApp, AppProblem> {
        self.asked_as(async |client, token| client.upload_package(token, package).await)
    }

    fn change(&self, id: &str, changes: &AppChanges) -> Result<AdminApp, AppProblem> {
        self.asked_as(async |client, token| client.update_app(token, id, changes).await)
    }

    fn new_secret(&self, id: &str) -> Result<String, AppProblem> {
        self.asked_as(async |client, token| client.rotate_secret(token, id).await)
    }

    fn delete(&self, id: &str) -> Result<(), AppProblem> {
        self.asked_as(async |client, token| client.delete_app(token, id).await)
    }

    fn people(&self, query: &str, after: Option<&str>) -> Result<UserPage, AppProblem> {
        self.asked_as(async |client, token| client.users(token, query, after, PAGE).await)
    }

    fn test_activity(&self, id: &str) -> Result<TestActivity, AppProblem> {
        self.asked_as(async |client, token| client.test_activity(token, id).await)
    }

    fn connection(&self) -> Result<BotConnection, AppProblem> {
        let metadata = self
            .runtime
            .block_on(self.client.bot_metadata())
            .map_err(AppProblem::from)?;
        Ok(BotConnection {
            origin: self.client.origin(),
            issuer: metadata.issuer,
        })
    }
}

#[cfg(test)]
#[path = "../tests/support/own_admin_tokens.rs"]
mod tests;
