//! The accounts a host signs in, as `accounts.toml` holds them: each one given is written there at once without an
//! object id ([`AccountFiles::ask_for`]), and its first sign-in adds the object id and tenant
//! (`crate::signin`). One whose sign-in never went through can be taken out again while it holds nothing but its UPN
//! ([`AccountFiles::abandon`]): one the user wrote more into (its `idp_hosts`, an own server's `ca_file`) stays, a
//! pending entry the next sign-in of that UPN takes up, never an account the app starts.

use super::{OwnServer, PersonalAccount, PersonalError, PersonalPaths};

/// The account files in the host's directories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountFiles {
    pub paths: PersonalPaths,
}

impl AccountFiles {
    /// The account files in `paths`, as they are (no migration).
    #[must_use]
    pub const fn new(paths: PersonalPaths) -> Self {
        Self { paths }
    }

    /// Every account, in the order added, signed in or not.
    ///
    /// # Errors
    /// When the account file cannot be read or parsed.
    pub fn accounts(&self) -> Result<Vec<PersonalAccount>, PersonalError> {
        PersonalAccount::load_all(&self.paths)
    }

    /// The accounts a sign-in named (their object id and tenant known), in the order added: those the app may start an
    /// engine for once their tokens are there (`crate::signin::ready`).
    ///
    /// # Errors
    /// As [`Self::accounts`].
    pub fn signed_in_accounts(&self) -> Result<Vec<PersonalAccount>, PersonalError> {
        let mut accounts = self.accounts()?;
        accounts.retain(PersonalAccount::is_signed_in);
        Ok(accounts)
    }

    /// The UPNs of the accounts no sign-in named yet, in the order given: the first run's screen shows the last one
    /// again.
    ///
    /// # Errors
    /// When `accounts.toml` can't be read.
    pub fn pending_upns(&self) -> Result<Vec<String>, PersonalError> {
        Ok(self
            .accounts()?
            .into_iter()
            .filter(|account| !account.is_signed_in() && account.server.is_none())
            .map(|account| account.upn)
            .collect())
    }

    /// The Microsoft 365 account `upn` (case aside), to sign in: the one `accounts.toml` holds of that UPN, signed in or
    /// not, else `upn` added after the others without an object id (`0600`). An own server's account of the same UPN
    /// is another account ([`Self::ask_for_on`]) and is never taken for it.
    ///
    /// # Errors
    /// For a bad UPN or a file that can't be read or written.
    pub fn ask_for(&self, upn: &str) -> Result<PersonalAccount, PersonalError> {
        let pending = PersonalAccount::pending(upn)?;
        if let Some(held) = self.held(&pending)? {
            return Ok(held);
        }
        pending.save(&self.paths)?;
        Ok(pending)
    }

    /// The entry `accounts.toml` holds of `account`'s server and UPN (case aside), and of no other account's.
    fn held(&self, account: &PersonalAccount) -> Result<Option<PersonalAccount>, PersonalError> {
        Ok(self.accounts()?.into_iter().find(|held| held.is(account)))
    }

    /// `account` as `accounts.toml` holds it now, with whatever the user wrote into its entry since it was read (its
    /// `ca_file`, say, while its sign-in screen is open); as given when the file holds no entry of its server and UPN.
    ///
    /// # Errors
    /// As [`Self::accounts`].
    pub fn as_held(&self, account: &PersonalAccount) -> Result<PersonalAccount, PersonalError> {
        Ok(self.held(account)?.unwrap_or_else(|| account.clone()))
    }

    /// The own server `server`'s account `upn`, to sign in: the one `accounts.toml` holds of that server and UPN (with
    /// whatever the user wrote into it, its `ca_file` say; a CA file given now replaces the entry's), else added after
    /// the others without an object id.
    ///
    /// # Errors
    /// For a bad UPN or server, or a file that can't be read or written.
    pub fn ask_for_on(&self, upn: &str, server: &OwnServer) -> Result<PersonalAccount, PersonalError> {
        let pending = PersonalAccount::pending_on(upn, server)?;
        match self.held(&pending)? {
            Some(held) if server.ca_file.is_none() || held.ca_file == server.ca_file => Ok(held),
            Some(held) => {
                let held = PersonalAccount {
                    ca_file: server.ca_file.clone(),
                    ..held
                };
                held.save(&self.paths)?;
                Ok(held)
            }
            None => {
                pending.save(&self.paths)?;
                Ok(pending)
            }
        }
    }

    /// Takes the account whose id is `id` (case aside; [`PersonalAccount::id`]: a Microsoft 365 account's UPN, an own
    /// server's `<server>/<UPN>`) out of `accounts.toml` when no sign-in ever named it and it holds nothing but its
    /// UPN and server (as [`Self::ask_for`] or [`Self::ask_for_on`] wrote it), so an account added whose sign-in stored
    /// nothing leaves nothing behind. An entry the user wrote more into (its `idp_hosts` or its `ca_file`, by hand)
    /// stays pending for the next sign-in of that account, and an account a sign-in named stays. A bare UPN never
    /// names an own server's account here. Whether one was taken out.
    ///
    /// # Errors
    /// When `accounts.toml` can't be read or written.
    pub fn abandon(&self, id: &str) -> Result<bool, PersonalError> {
        let held = self
            .accounts()?
            .into_iter()
            .find(|held| held.id().eq_ignore_ascii_case(id));
        match held {
            // by its id, which `remove` reads before any bare UPN
            Some(held) if is_bare(&held) => Ok(PersonalAccount::remove(&self.paths, &held.id())?.is_some()),
            _ => Ok(false),
        }
    }
}

/// Whether `account` holds nothing but its UPN and server: no sign-in named it and nothing was written into it by hand.
fn is_bare(account: &PersonalAccount) -> bool {
    !account.is_signed_in() && account.name.is_none() && account.idp_hosts.is_empty() && account.ca_file.is_none()
}
