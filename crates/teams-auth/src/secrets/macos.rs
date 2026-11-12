//! The macOS Keychain, through Security.framework's generic password items (`security-framework`). Each secret is one
//! item in the user's default (login) keychain: service `<application>: <audience>`, account the signed-in account.
//! The application name never holds `:`, so the service names one namespace and audience without ambiguity, and
//! Keychain Access shows them readably. Only exact items are read, written or deleted; nothing is enumerated.
//!
//! Security.framework's calls block (and may wait on the user unlocking the keychain), so they run on the blocking
//! pool, one at a time per store. Errors keep only the `OSStatus` code: nothing of an item reaches them.

use std::sync::{Arc, Mutex, PoisonError};

use security_framework::passwords;

use super::{Secret, SecretError, SecretKey, SecretStore};

/// `errSecItemNotFound`: no item has that service and account.
const ITEM_NOT_FOUND: i32 = -25300;

/// The application's items in the user's default keychain.
#[derive(Debug)]
pub struct KeyringSecretStore {
    application: String,
    serial: Arc<Mutex<()>>,
}

impl KeyringSecretStore {
    /// The store of `application`'s items (`ngteams`, `ngteams-<role>`, the lab's `ngteams-lab`). Opens nothing: the
    /// keychain is reached by each operation.
    ///
    /// # Errors
    /// Never on macOS; the signature is the other platforms'.
    pub fn new(application: impl Into<String>) -> impl Future<Output = Result<Self, SecretError>> {
        std::future::ready(Ok(Self {
            application: application.into(),
            serial: Arc::new(Mutex::new(())),
        }))
    }

    async fn execute<T, F>(&self, operation: F) -> Result<T, SecretError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, SecretError> + Send + 'static,
    {
        let serial = Arc::clone(&self.serial);
        tokio::task::spawn_blocking(move || {
            let _guard = serial.lock().unwrap_or_else(PoisonError::into_inner);
            operation()
        })
        .await
        .map_err(|_| SecretError::Keychain(0))?
    }
}

/// The item's service: the namespace and the audience.
fn service(application: &str, key: &SecretKey) -> String {
    format!("{application}: {}", key.audience())
}

fn failed(error: security_framework::base::Error) -> SecretError {
    SecretError::Keychain(error.code())
}

impl SecretStore for KeyringSecretStore {
    async fn put(&self, key: &SecretKey, secret: &Secret) -> Result<(), SecretError> {
        let service = service(&self.application, key);
        let account = key.account().to_owned();
        let secret = secret.clone();
        self.execute(move || {
            passwords::set_generic_password(&service, &account, secret.expose().as_bytes()).map_err(failed)
        })
        .await
    }

    async fn get(&self, key: &SecretKey) -> Result<Option<Secret>, SecretError> {
        let service = service(&self.application, key);
        let account = key.account().to_owned();
        self.execute(move || match passwords::get_generic_password(&service, &account) {
            Ok(bytes) => {
                let bytes = zeroize::Zeroizing::new(bytes);
                let text = std::str::from_utf8(&bytes).map_err(|_| SecretError::NotUtf8)?;
                Ok(Some(Secret::new(text)))
            }
            Err(error) if error.code() == ITEM_NOT_FOUND => Ok(None),
            Err(error) => Err(failed(error)),
        })
        .await
    }

    async fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        let service = service(&self.application, key);
        let account = key.account().to_owned();
        self.execute(move || match passwords::delete_generic_password(&service, &account) {
            Err(error) if error.code() != ITEM_NOT_FOUND => Err(failed(error)),
            _ => Ok(()),
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn services_keep_namespaces_and_audiences_apart() {
        let key = SecretKey::new("synthetic@example.test", "https://api.example.test/.default");
        assert_eq!(service("ngteams", &key), "ngteams: https://api.example.test/.default");
        assert_ne!(service("ngteams", &key), service("ngteams-lab", &key));
        assert_ne!(
            service("ngteams", &key),
            service("ngteams", &SecretKey::new("synthetic@example.test", "other"))
        );
    }

    #[test]
    fn keychain_errors_carry_only_the_status() {
        let error = failed(security_framework::base::Error::from_code(-25293));
        assert!(matches!(error, SecretError::Keychain(-25293)));
        assert_eq!(error.to_string(), "macOS Keychain: OSStatus -25293");
    }
}
