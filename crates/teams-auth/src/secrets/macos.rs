//! The macOS Keychain, through Security.framework's generic password items (`security-framework`). Each secret is one
//! item in the user's default (login) keychain: service `<application>: <audience>`, account the signed-in account.
//! The application name never holds `:`, so the service names one namespace and audience without ambiguity, and
//! Keychain Access shows them readably. Only exact items are read, written or deleted; nothing is enumerated.
//!
//! The lab's namespace (`ngteams-lab`) is the exception: its items are in the lab's own keychain ([`lab`]), never in
//! the login keychain.
//!
//! Security.framework's calls block (and may wait on the user unlocking the keychain), so they run on the blocking
//! pool, one at a time per store. Errors keep only the `OSStatus` code: nothing of an item reaches them.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use security_framework::passwords;

use super::{Secret, SecretError, SecretKey, SecretStore};

#[path = "macos_lab.rs"]
mod lab;

/// `errSecItemNotFound`: no item has that service and account.
const ITEM_NOT_FOUND: i32 = -25300;

/// The application's items in the user's default keychain, or the lab's in the lab keychain.
#[derive(Debug)]
pub struct KeyringSecretStore {
    application: String,
    /// The lab keychain, for the lab's namespace.
    lab: Option<PathBuf>,
    serial: Arc<Mutex<()>>,
}

impl KeyringSecretStore {
    /// The store of `application`'s items (`ngteams`, `ngteams-<role>`, the lab's `ngteams-lab`). Opens nothing: the
    /// keychain is reached by each operation.
    ///
    /// # Errors
    /// For the lab's namespace without an absolute `HOME` to find its keychain in.
    pub fn new(application: impl Into<String>) -> impl Future<Output = Result<Self, SecretError>> {
        let application = application.into();
        let lab = if application == lab::APPLICATION {
            match lab::path(std::env::var_os("HOME")) {
                Some(path) => Some(path),
                None => return std::future::ready(Err(SecretError::Keychain(lab::NO_SUCH_KEYCHAIN))),
            }
        } else {
            None
        };
        std::future::ready(Ok(Self::with(application, lab)))
    }

    fn with(application: String, lab: Option<PathBuf>) -> Self {
        Self {
            application,
            lab,
            serial: Arc::new(Mutex::new(())),
        }
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
        .map_err(|_| SecretError::KeychainWorker)?
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
        let lab = self.lab.clone();
        self.execute(move || match lab {
            Some(keychain) => lab::put(&keychain, &service, &account, secret.expose().as_bytes()),
            None => passwords::set_generic_password(&service, &account, secret.expose().as_bytes()).map_err(failed),
        })
        .await
    }

    async fn get(&self, key: &SecretKey) -> Result<Option<Secret>, SecretError> {
        let service = service(&self.application, key);
        let account = key.account().to_owned();
        let lab = self.lab.clone();
        self.execute(move || {
            let bytes = match lab {
                Some(keychain) => lab::get(&keychain, &service, &account)?,
                None => match passwords::get_generic_password(&service, &account) {
                    Ok(bytes) => Some(zeroize::Zeroizing::new(bytes)),
                    Err(error) if error.code() == ITEM_NOT_FOUND => None,
                    Err(error) => return Err(failed(error)),
                },
            };
            bytes
                .map(|bytes| {
                    std::str::from_utf8(&bytes)
                        .map(Secret::new)
                        .map_err(|_| SecretError::NotUtf8)
                })
                .transpose()
        })
        .await
    }

    async fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        let service = service(&self.application, key);
        let account = key.account().to_owned();
        let lab = self.lab.clone();
        self.execute(move || match lab {
            Some(keychain) => lab::delete(&keychain, &service, &account),
            None => match passwords::delete_generic_password(&service, &account) {
                Err(error) if error.code() != ITEM_NOT_FOUND => Err(failed(error)),
                _ => Ok(()),
            },
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

    /// The real login keychain: opt-in (`NGTEAMS_KEYCHAIN_TESTS=1`), as it needs an unlocked keychain, which an SSH
    /// session usually doesn't have; CI's macOS runner has one. Its items use a random namespace and are deleted.
    #[tokio::test]
    async fn items_are_written_read_replaced_and_deleted_in_the_keychain() {
        if std::env::var_os("NGTEAMS_KEYCHAIN_TESTS").is_none() {
            eprintln!("skipped: set NGTEAMS_KEYCHAIN_TESTS=1 where the login keychain is unlocked");
            return;
        }
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let namespace = random.iter().fold(String::new(), |mut text, byte| {
            let _ = std::fmt::Write::write_fmt(&mut text, format_args!("{byte:02x}"));
            text
        });
        let store = KeyringSecretStore::new(format!("ngteams-synthetic-{namespace}"))
            .await
            .unwrap();
        let other = KeyringSecretStore::new(format!("ngteams-synthetic-other-{namespace}"))
            .await
            .unwrap();
        let key = SecretKey::new("synthetic@example.test", "https://api.example.test/.default");
        assert!(store.get(&key).await.unwrap().is_none());
        store.put(&key, &Secret::new("synthetic-first")).await.unwrap();
        store.put(&key, &Secret::new("synthetic-second ✓")).await.unwrap();
        assert_eq!(store.get(&key).await.unwrap().unwrap().expose(), "synthetic-second ✓");
        assert!(other.get(&key).await.unwrap().is_none(), "namespaces are apart");
        store.delete(&key).await.unwrap();
        store.delete(&key).await.unwrap();
        assert!(store.get(&key).await.unwrap().is_none());
    }

    #[test]
    fn the_lab_keychain_is_in_the_lab_directory() {
        assert_eq!(
            lab::path(Some("/Users/synthetic".into())),
            Some(PathBuf::from(
                "/Users/synthetic/Library/Application Support/ngteams-lab/ngteams-lab.keychain-db"
            ))
        );
        assert_eq!(lab::path(Some("relative".into())), None);
        assert_eq!(lab::path(None), None);
    }

    #[tokio::test]
    async fn only_the_lab_namespace_uses_the_lab_keychain() {
        let lab = KeyringSecretStore::new("ngteams-lab").await.unwrap();
        assert!(lab.lab.is_some());
        assert_eq!(lab.lab, lab::path(std::env::var_os("HOME")));
        for personal in ["ngteams", "ngteams-readonly", "ngteams-lab-other"] {
            assert!(
                KeyringSecretStore::new(personal).await.unwrap().lab.is_none(),
                "{personal}"
            );
        }
    }

    /// A lab keychain of its own in a temporary directory: it needs no login keychain, so it runs over SSH too.
    #[tokio::test]
    async fn lab_items_are_kept_in_the_lab_keychain_and_another_program_reads_them() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = dir.path().join("lab/ngteams-lab.keychain-db");
        let store = KeyringSecretStore::with("ngteams-lab".into(), Some(keychain.clone()));
        let key = SecretKey::new("synthetic@example.test", "https://api.example.test/.default");
        assert!(store.get(&key).await.unwrap().is_none());
        store.delete(&key).await.unwrap();
        assert!(!keychain.exists(), "a read or a delete makes no keychain");

        store.put(&key, &Secret::new("synthetic-first ✓")).await.unwrap();
        assert_eq!(store.get(&key).await.unwrap().unwrap().expose(), "synthetic-first ✓");
        store.put(&key, &Secret::new("synthetic-second")).await.unwrap();
        assert_eq!(store.get(&key).await.unwrap().unwrap().expose(), "synthetic-second");
        let mode = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&keychain).unwrap().permissions());
        assert_eq!(mode & 0o777, 0o600);

        // another program (Apple's `security`) reads it with no prompt: its decrypt rule names no program
        let other = std::process::Command::new("sh")
            .arg("-c")
            .arg(concat!(
                r#"security unlock-keychain -p ngteams-lab "$1" && "#,
                r#"security find-generic-password -s "$2" -a "$3" -w "$1""#
            ))
            .arg("sh")
            .arg(&keychain)
            .arg(service("ngteams-lab", &key))
            .arg(key.account())
            .output()
            .unwrap();
        assert!(other.status.success(), "{}", String::from_utf8_lossy(&other.stderr));
        assert_eq!(String::from_utf8_lossy(&other.stdout).trim(), "synthetic-second");
        let searched = std::process::Command::new("security")
            .arg("list-keychains")
            .output()
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&searched.stdout).contains("ngteams-lab"),
            "the lab keychain stays out of the user's search list"
        );

        store.delete(&key).await.unwrap();
        store.delete(&key).await.unwrap();
        assert!(store.get(&key).await.unwrap().is_none());
    }

    #[test]
    fn keychain_errors_carry_only_the_status() {
        let error = failed(security_framework::base::Error::from_code(-25293));
        assert!(matches!(error, SecretError::Keychain(-25293)));
        assert_eq!(error.to_string(), "macOS Keychain: OSStatus -25293");
    }

    /// A worker that panics (or is cancelled) is said as the worker's failure, never as a Keychain status. The
    /// operation panics before it reaches the keychain.
    #[tokio::test]
    async fn a_failed_worker_is_not_reported_as_a_status() {
        let store = KeyringSecretStore::new("ngteams-synthetic").await.unwrap();
        let result = store
            .execute(|| -> Result<(), SecretError> { panic!("synthetic worker failure") })
            .await;
        let error = result.unwrap_err();
        assert!(matches!(error, SecretError::KeychainWorker), "{error:?}");
        assert_eq!(error.to_string(), "macOS Keychain: its worker failed");
    }
}
