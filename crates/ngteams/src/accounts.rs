//! Accounts running side by side, with one shared call floor and device set.
//! Each engine owns its account's store and sends writes through its outbox.

use crate::accounts_file::PersonalAccount;
use teams_auth::secrets::SecretStore;
use teams_core::CallFloor;
use teams_signin::window::Windows;

use crate::calling::{CallDevices, PersonalCalls};
use crate::engine::{self, PersonalEngine};
use crate::forget::{self, Forgot};
use crate::secrets::AppSecrets;
use crate::setup::Setup;
use crate::signin::{self, SignInClients};

/// An account's engine, started: over the selected desktop or lab store in the app, or a test's own store.
pub struct Started<S = AppSecrets> {
    pub account: PersonalAccount,
    pub engine: PersonalEngine<S>,
    /// The window's calling; its shared devices may still be preparing.
    pub calls: Option<PersonalCalls>,
}

impl<S> std::fmt::Debug for Started<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Started")
            .field("account", &self.account)
            .field("calls", &self.calls.is_some())
            .finish_non_exhaustive()
    }
}

/// What every account the app runs shares: the setup, the call floor, the calls' devices.
#[derive(Clone, Debug)]
pub struct Runner {
    pub setup: Setup,
    pub floor: CallFloor,
    /// The shared device discovery; `None` opens no call devices.
    pub devices: Option<CallDevices>,
}

/// Why an account's engine didn't start: `message` says it for the user; `running` when another engine holds the
/// account (another `ngteams`, which may still be starting: it locks its accounts before it serves its control socket).
#[derive(Debug)]
pub struct StartFailed {
    pub message: String,
    pub running: bool,
}

impl StartFailed {
    /// `account`'s start refused with `error`.
    #[must_use]
    pub fn of(account: &PersonalAccount, error: &engine::StartError) -> Self {
        use teams_core::account::AccountError;
        use teams_core::engine::EngineError;
        let running = matches!(
            error,
            engine::StartError::Account(
                AccountError::Engine(EngineError::AlreadyRunning { .. })
                    | AccountError::Store(teams_store::StoreError::EngineRunning { .. })
            )
        );
        Self {
            message: format!("{}: {error}", account.upn),
            running,
        }
    }
}

impl std::fmt::Display for StartFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<StartFailed> for String {
    fn from(failed: StartFailed) -> Self {
        failed.message
    }
}

impl Runner {
    /// Starts `account`'s engine on the shared call floor and devices, with its push channel.
    ///
    /// # Errors
    /// Why it couldn't start, said for the user (another `ngteams` runs on the account, its region can't be read).
    pub fn start(&self, account: &PersonalAccount) -> Result<Started, StartFailed> {
        self.start_on(account, self.setup.secrets())
    }

    fn start_on<S: SecretStore + 'static>(
        &self,
        account: &PersonalAccount,
        secrets: S,
    ) -> Result<Started<S>, StartFailed> {
        let (engine_calls, calls) = self
            .devices
            .as_ref()
            .map(CallDevices::for_account)
            .map_or((None, None), |(engine, window)| (Some(engine), Some(window)));
        let engine = engine::start(
            &self.setup.paths,
            account,
            self.floor.clone(),
            secrets,
            true,
            engine_calls,
        )
        .map_err(|error| StartFailed::of(account, &error))?;
        Ok(Started {
            account: account.clone(),
            engine,
            calls,
        })
    }
}

/// A runtime of its own for a blocking sign-in or forget.
///
/// # Errors
/// When it can't be built.
pub fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("can't start the async runtime: {error}"))
}

/// Whether `account` can start without a sign-in ([`signin::ready`]).
///
/// # Errors
/// When the selected credential store can't be read.
pub fn ready(setup: &Setup, account: &PersonalAccount) -> Result<bool, String> {
    let store = setup.secrets();
    runtime()?
        .block_on(signin::ready(setup, account, &store))
        .map_err(|error| error.to_string())
}

/// Signs `account` in through the sign-in window `windows` opens (blocking: run it off GPUI's foreground), as
/// [`signin::sign_in`].
///
/// # Errors
/// Why the sign-in stored nothing, as the user is told it ([`signin::failure`]).
pub fn sign_in(setup: &Setup, account: &PersonalAccount, windows: &Windows) -> Result<PersonalAccount, String> {
    let store = setup.secrets();
    runtime()?.block_on(async {
        let clients = SignInClients::live().map_err(|error| error.to_string())?;
        let signed = match signin::window_for(setup, account, windows) {
            Ok(window) => signin::sign_in(setup, account, &clients, &store, window).await,
            Err(error) => Err(error),
        }
        .map_err(|error| signin::failure(setup, account, &error))?;
        log::info!("an account was signed in (its tenant matched)");
        Ok(signed)
    })
}

/// Adds the account `upn` (or takes up the entry held of it) and signs it in (blocking: run it off GPUI's foreground),
/// as [`signin::add`]: one that doesn't go through is taken out again only while its entry holds nothing but its UPN,
/// and kept when the window refused an identity provider's host, for the user to add to its `idp_hosts`.
///
/// # Errors
/// Why it wasn't added, as the user is told it ([`signin::failure`]).
pub fn add(setup: &Setup, upn: &str, windows: &Windows) -> Result<PersonalAccount, String> {
    let store = setup.secrets();
    runtime()?.block_on(async {
        let clients = SignInClients::live().map_err(|error| error.to_string())?;
        let window = |account: &PersonalAccount| signin::window_for(setup, account, windows);
        let signed = signin::add(setup, upn, &clients, &store, window).await?;
        log::info!("an account was added (its tenant matched)");
        Ok(signed)
    })
}

/// Forgets the account `upn` (blocking), as `ngteams --forget` does: refused while its engine runs.
///
/// # Errors
/// Why it wasn't forgotten.
pub fn forget(setup: &Setup, upn: &str) -> Result<Forgot, String> {
    let store = setup.secrets();
    runtime()?
        .block_on(forget::forget(setup, upn, &store))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "../tests/support/accounts.rs"]
mod tests;
