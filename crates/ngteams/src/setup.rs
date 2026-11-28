//! The app's account files and setup, including legacy single-account migration.
//!
//! The accounts are `accounts.toml`'s (`teams_app::accounts::AccountFiles`, which the setup derefs to), in the order
//! the user added them: each one given (the first run's screen, the window's "add account", `--account`) is written
//! there at once without an object id ([`Setup::ask_for`]), and its first sign-in adds the object id and tenant
//! (`crate::signin`). A build of one account's `account.toml` and `region.json` is moved into these files when the
//! setup is made (`PersonalPaths::migrate_single_account`).

use std::ops::{Deref, DerefMut};

use crate::accounts_file::{FromEnv as _, OwnServer, PersonalAccount, PersonalError, PersonalPaths};
use crate::secrets::AppSecrets;
use teams_app::accounts::AccountFiles;

/// Why the personal build can't run for an account here.
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error(transparent)]
    Personal(#[from] PersonalError),
}

/// The app's account files.
#[derive(Clone, Debug)]
pub struct Setup {
    files: AccountFiles,
    secrets: AppSecrets,
}

impl Deref for Setup {
    type Target = AccountFiles;

    fn deref(&self) -> &AccountFiles {
        &self.files
    }
}

impl DerefMut for Setup {
    fn deref_mut(&mut self) -> &mut AccountFiles {
        &mut self.files
    }
}

impl Setup {
    /// From the environment, migrating any earlier single-account files.
    ///
    /// # Errors
    /// When the default paths cannot be resolved or the account files cannot be moved.
    pub fn from_env() -> Result<Self, SetupError> {
        Self::new(PersonalPaths::from_env()?)
    }

    /// Uses `paths`, migrating earlier single-account files (see the module docs).
    ///
    /// # Errors
    /// When the files cannot be read or moved.
    pub fn new(paths: PersonalPaths) -> Result<Self, SetupError> {
        paths.migrate_single_account()?;
        Ok(Self {
            files: AccountFiles::new(paths),
            secrets: AppSecrets::personal(),
        })
    }

    /// Uses the startup-selected credential backend for every account operation.
    #[must_use]
    pub fn with_secrets(mut self, secrets: AppSecrets) -> Self {
        self.secrets = secrets;
        self
    }

    /// A handle to the selected store, including token renewal and account removal.
    #[must_use]
    pub fn secrets(&self) -> AppSecrets {
        self.secrets.clone()
    }

    /// Every account, in the order added, signed in or not.
    ///
    /// # Errors
    /// When the account file cannot be read or parsed.
    pub fn accounts(&self) -> Result<Vec<PersonalAccount>, SetupError> {
        Ok(self.files.accounts()?)
    }

    /// The accounts a sign-in named, in the order added ([`AccountFiles::signed_in_accounts`]).
    ///
    /// # Errors
    /// As [`Self::accounts`].
    pub fn signed_in_accounts(&self) -> Result<Vec<PersonalAccount>, SetupError> {
        Ok(self.files.signed_in_accounts()?)
    }

    /// The UPNs of the accounts no sign-in named yet ([`AccountFiles::pending_upns`]).
    ///
    /// # Errors
    /// [`SetupError::Personal`] when `accounts.toml` can't be read.
    pub fn pending_upns(&self) -> Result<Vec<String>, SetupError> {
        Ok(self.files.pending_upns()?)
    }

    /// The Microsoft 365 account `upn` to sign in ([`AccountFiles::ask_for`]).
    ///
    /// # Errors
    /// [`SetupError::Personal`] for a bad UPN or a file that can't be read or written.
    pub fn ask_for(&self, upn: &str) -> Result<PersonalAccount, SetupError> {
        Ok(self.files.ask_for(upn)?)
    }

    /// `account` as `accounts.toml` holds it now ([`AccountFiles::as_held`]).
    ///
    /// # Errors
    /// As [`Self::accounts`].
    pub fn as_held(&self, account: &PersonalAccount) -> Result<PersonalAccount, SetupError> {
        Ok(self.files.as_held(account)?)
    }

    /// The own server `server`'s account `upn` to sign in ([`AccountFiles::ask_for_on`]).
    ///
    /// # Errors
    /// [`SetupError::Personal`] for a bad UPN or server, or a file that can't be read or written.
    pub fn ask_for_on(&self, upn: &str, server: &OwnServer) -> Result<PersonalAccount, SetupError> {
        Ok(self.files.ask_for_on(upn, server)?)
    }

    /// Takes a pending entry that holds nothing but its UPN out of `accounts.toml` ([`AccountFiles::abandon`]; an
    /// account a sign-in named stays, and `crate::forget` forgets either). Whether one was taken out.
    ///
    /// # Errors
    /// [`SetupError::Personal`] when `accounts.toml` can't be read or written.
    pub fn abandon(&self, id: &str) -> Result<bool, SetupError> {
        Ok(self.files.abandon(id)?)
    }
}
