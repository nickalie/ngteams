//! The process's sealer (`secrets::sealed::install`), in a test process of its own: nothing before the install, the
//! first install stays, and every installed store uses its directory and sealer.

#![cfg(test)]

use std::sync::Arc;

use teams_auth::secrets::sealed::{self, AlreadyInstalled, Sealer};
use teams_auth::secrets::{Secret, SecretError, SecretKey, SecretStore};
use zeroize::Zeroizing;

struct PlainSealer;

impl Sealer for PlainSealer {
    fn seal(&self, plain: &[u8]) -> Result<Vec<u8>, SecretError> {
        Ok(plain.to_vec())
    }

    fn open(&self, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, SecretError> {
        Ok(Zeroizing::new(sealed.to_vec()))
    }
}

#[tokio::test]
async fn installed_stores_use_the_first_install() {
    let error = sealed::installed("synthetic-sealed").unwrap_err();
    assert!(matches!(error, SecretError::Sealer(_)), "{error:?}");
    assert_eq!(error.to_string(), "secret sealer: no secret sealer is installed");

    let dir = tempfile::tempdir().unwrap();
    sealed::install(dir.path().join("secrets"), Arc::new(PlainSealer)).unwrap();
    assert_eq!(
        sealed::install(dir.path().join("other"), Arc::new(PlainSealer)),
        Err(AlreadyInstalled)
    );

    let key = SecretKey::new("a@lab.example.com", "skypetoken");
    sealed::installed("synthetic-sealed")
        .unwrap()
        .put(&key, &Secret::new("kept"))
        .await
        .unwrap();
    let read = sealed::installed("synthetic-sealed").unwrap().get(&key).await.unwrap();
    assert_eq!(read.map(|secret| secret.expose().to_owned()).as_deref(), Some("kept"));
    assert!(dir.path().join("secrets").is_dir() && !dir.path().join("other").exists());
}
