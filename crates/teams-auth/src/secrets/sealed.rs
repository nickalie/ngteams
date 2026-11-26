//! Secrets in sealed files: one file for each key in a private directory, its bytes sealed by a [`Sealer`]. On Android
//! the sealer is the Keystore's key, which the app installs at start ([`install`]); the platform's
//! [`super::KeyringSecretStore`] there is this store.
//!
//! A file's name is a hash of the namespace, the account and the audience, so no address reaches the file system. A
//! write goes to a temporary file first and then replaces the old file, so a crash never leaves half a secret.

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use sha2::{Digest as _, Sha256};
use teams_platform::private;
use zeroize::Zeroizing;

use super::{Secret, SecretError, SecretKey, SecretStore};

/// Seals the bytes of a secret before they reach a file, and opens them again. A sealer that cannot do it answers
/// [`SecretError::Sealer`].
pub trait Sealer: Send + Sync {
    /// The sealed form of `plain`.
    ///
    /// # Errors
    /// The sealer cannot seal now (its key is gone or locked).
    fn seal(&self, plain: &[u8]) -> Result<Vec<u8>, SecretError>;

    /// The plain bytes of `sealed`.
    ///
    /// # Errors
    /// `sealed` is not this sealer's, or its key is gone.
    fn open(&self, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, SecretError>;
}

/// A store of sealed files in one private directory, namespaced by `application`.
pub struct SealedFileStore {
    dir: PathBuf,
    application: String,
    sealer: Arc<dyn Sealer>,
    serial: Mutex<()>,
}

impl std::fmt::Debug for SealedFileStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedFileStore")
            .field("application", &self.application)
            .finish_non_exhaustive()
    }
}

impl SealedFileStore {
    /// The store in `dir`, which it makes private (and creates when it is missing).
    ///
    /// # Errors
    /// [`SecretError::SealedFile`] when `dir` cannot be created or made private.
    pub fn new(
        dir: impl Into<PathBuf>,
        application: impl Into<String>,
        sealer: Arc<dyn Sealer>,
    ) -> Result<Self, SecretError> {
        let dir = dir.into();
        private::create_dir_all(&dir).map_err(|error| file_error(&error))?;
        private::restrict_dir(&dir).map_err(|error| file_error(&error))?;
        Ok(Self {
            dir,
            application: application.into(),
            sealer,
            serial: Mutex::new(()),
        })
    }

    fn path(&self, key: &SecretKey, extension: &str) -> PathBuf {
        let mut digest = Sha256::new();
        for part in [self.application.as_str(), key.account(), key.audience()] {
            digest.update((part.len() as u64).to_be_bytes());
            digest.update(part.as_bytes());
        }
        let mut name = String::with_capacity(64 + 1 + extension.len());
        for byte in digest.finalize() {
            let _ = write!(name, "{byte:02x}");
        }
        name.push('.');
        name.push_str(extension);
        self.dir.join(name)
    }

    fn write(&self, key: &SecretKey, secret: &Secret) -> Result<(), SecretError> {
        let sealed = self.sealer.seal(secret.expose().as_bytes())?;
        let _serial = self.serial.lock().unwrap_or_else(PoisonError::into_inner);
        let temporary = self.path(key, "tmp");
        write_file(&temporary, &sealed)
            .and_then(|()| fs::rename(&temporary, self.path(key, "sealed")))
            .map_err(|error| {
                let _ = fs::remove_file(&temporary);
                file_error(&error)
            })
    }

    fn read(&self, key: &SecretKey) -> Result<Option<Secret>, SecretError> {
        let sealed = {
            let _serial = self.serial.lock().unwrap_or_else(PoisonError::into_inner);
            match fs::read(self.path(key, "sealed")) {
                Ok(sealed) => sealed,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(file_error(&error)),
            }
        };
        let plain = self.sealer.open(&sealed)?;
        let text = std::str::from_utf8(&plain).map_err(|_| SecretError::NotUtf8)?;
        Ok(Some(Secret::new(text)))
    }

    fn remove(&self, key: &SecretKey) -> Result<(), SecretError> {
        let _serial = self.serial.lock().unwrap_or_else(PoisonError::into_inner);
        match fs::remove_file(self.path(key, "sealed")) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(file_error(&error)),
        }
    }
}

fn write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = private::replace(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn file_error(error: &io::Error) -> SecretError {
    SecretError::SealedFile(error.kind())
}

impl SecretStore for SealedFileStore {
    fn put(&self, key: &SecretKey, secret: &Secret) -> impl Future<Output = Result<(), SecretError>> + Send {
        std::future::ready(self.write(key, secret))
    }

    fn get(&self, key: &SecretKey) -> impl Future<Output = Result<Option<Secret>, SecretError>> + Send {
        std::future::ready(self.read(key))
    }

    fn delete(&self, key: &SecretKey) -> impl Future<Output = Result<(), SecretError>> + Send {
        std::future::ready(self.remove(key))
    }
}

/// Where the process keeps its sealed files, and their sealer ([`install`]).
struct Installed {
    dir: PathBuf,
    sealer: Arc<dyn Sealer>,
}

static INSTALLED: OnceLock<Installed> = OnceLock::new();

/// The process's sealer is installed already: the first [`install`] stays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a secret sealer is installed already")]
pub struct AlreadyInstalled;

/// Sets the directory and the sealer of every [`installed`] store of this process. The app does it one time, at
/// start, before it opens an account.
///
/// # Errors
/// [`AlreadyInstalled`] on a second call: the first one stays.
pub fn install(dir: impl Into<PathBuf>, sealer: Arc<dyn Sealer>) -> Result<(), AlreadyInstalled> {
    INSTALLED
        .set(Installed {
            dir: dir.into(),
            sealer,
        })
        .map_err(|_| AlreadyInstalled)
}

/// The store of `application` in the installed directory, with the installed sealer.
///
/// # Errors
/// [`SecretError::Sealer`] when nothing is installed; [`SecretError::SealedFile`] as [`SealedFileStore::new`].
pub fn installed(application: impl Into<String>) -> Result<SealedFileStore, SecretError> {
    let installed = INSTALLED
        .get()
        .ok_or(SecretError::Sealer("no secret sealer is installed"))?;
    SealedFileStore::new(installed.dir.clone(), application, Arc::clone(&installed.sealer))
}
