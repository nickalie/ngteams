//! Passwords: Argon2id (RustCrypto's defaults) in PHC strings, hashed and checked on the blocking pool.

use std::sync::LazyLock;

use argon2::password_hash::{PasswordHasher as _, PasswordVerifier as _};
use argon2::{Argon2, PasswordHash};

/// The shortest password an account may get.
pub const MIN_LENGTH: usize = 8;

/// Checked in place of a missing account's hash, so an unknown UPN costs as long as a wrong password.
static DECOY: LazyLock<Option<String>> = LazyLock::new(|| hash_now("decoy password for unknown accounts").ok());

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("a password needs at least {MIN_LENGTH} characters")]
    TooShort,
    #[error("the password can't be hashed")]
    Hash,
}

/// The PHC string of `password`.
///
/// # Errors
/// [`PasswordError::TooShort`]; [`PasswordError::Hash`] when the system random source fails.
pub async fn hash(password: &str) -> Result<String, PasswordError> {
    if password.chars().count() < MIN_LENGTH {
        return Err(PasswordError::TooShort);
    }
    let password = password.to_owned();
    tokio::task::spawn_blocking(move || hash_now(&password))
        .await
        .map_err(|_| PasswordError::Hash)?
}

fn hash_now(password: &str) -> Result<String, PasswordError> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|_| PasswordError::Hash)
}

/// Whether `password` matches `hash` (a PHC string); with no hash (no such account) it checks a decoy and says no.
pub async fn verify(password: &str, hash: Option<&str>) -> bool {
    let known = hash.is_some();
    let Some(hash) = hash.map(str::to_owned).or_else(|| DECOY.clone()) else {
        return false;
    };
    let password = password.to_owned();
    let matched = tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash).is_ok_and(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
    })
    .await
    .unwrap_or(false);
    known && matched
}
