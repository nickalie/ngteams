//! The admin commands' operations on accounts, shared by the command line and tests.

use crate::auth::password::{self, PasswordError};
use crate::db::users::{self, User};
use crate::db::{Db, DbError};

#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error(transparent)]
    Password(#[from] PasswordError),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("an account with that UPN already exists")]
    Exists,
    #[error("no account has that UPN")]
    NoSuchUser,
    #[error("a UPN looks like name@domain")]
    Upn,
}

/// Adds an account.
///
/// # Errors
/// A UPN that isn't `name@domain` or is taken, a short password, or a database failure.
pub async fn add_user(db: &Db, upn: &str, display_name: &str, password: &str) -> Result<User, AdminError> {
    let upn = users::normalize_upn(upn);
    let well_formed = upn
        .split_once('@')
        .is_some_and(|(name, domain)| !name.is_empty() && domain.contains('.') && !upn.contains(char::is_whitespace));
    if !well_formed {
        return Err(AdminError::Upn);
    }
    let hash = password::hash(password).await?;
    let name = if display_name.trim().is_empty() { upn.as_str() } else { display_name.trim() };
    users::add(db, &upn, name, &hash).await.map_err(|error| match error {
        DbError::Conflict => AdminError::Exists,
        error => AdminError::Db(error),
    })
}

/// Disables (signing it out everywhere) or enables an account.
///
/// # Errors
/// No such account, or a database failure.
pub async fn set_disabled(db: &Db, upn: &str, disabled: bool) -> Result<(), AdminError> {
    if users::set_disabled(db, upn, disabled).await? {
        Ok(())
    } else {
        Err(AdminError::NoSuchUser)
    }
}

/// Gives an account a new password, signing it out everywhere.
///
/// # Errors
/// No such account, a short password, or a database failure.
pub async fn reset_password(db: &Db, upn: &str, password: &str) -> Result<(), AdminError> {
    let hash = password::hash(password).await?;
    if users::set_password(db, upn, &hash).await? {
        Ok(())
    } else {
        Err(AdminError::NoSuchUser)
    }
}
