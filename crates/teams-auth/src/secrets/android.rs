//! The secret store on Android: sealed files in the app's private directory, sealed by the Android Keystore's key
//! that the app installs at start ([`super::sealed::install`]).

use super::sealed::{self, SealedFileStore};
use super::{Secret, SecretError, SecretKey, SecretStore};

#[derive(Debug)]
pub struct KeyringSecretStore(SealedFileStore);

impl KeyringSecretStore {
    /// The sealed store of `application` ([`sealed::installed`]).
    ///
    /// # Errors
    /// When no sealer is installed, or the directory cannot be made private.
    pub fn new(application: impl Into<String>) -> impl Future<Output = Result<Self, SecretError>> {
        std::future::ready(sealed::installed(application).map(Self))
    }
}

impl SecretStore for KeyringSecretStore {
    fn put(&self, key: &SecretKey, secret: &Secret) -> impl Future<Output = Result<(), SecretError>> + Send {
        self.0.put(key, secret)
    }

    fn get(&self, key: &SecretKey) -> impl Future<Output = Result<Option<Secret>, SecretError>> + Send {
        self.0.get(key)
    }

    fn delete(&self, key: &SecretKey) -> impl Future<Output = Result<(), SecretError>> + Send {
        self.0.delete(key)
    }
}
