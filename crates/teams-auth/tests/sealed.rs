//! The sealed-file store (`secrets::sealed`, the store on Android) with a fake sealer: the files it writes, what a
//! sealer that refuses does, and the I/O failures it reports.

#![cfg(test)]

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use teams_auth::secrets::sealed::{SealedFileStore, Sealer};
use teams_auth::secrets::{Secret, SecretError, SecretKey, SecretStore};
use zeroize::Zeroizing;

const SPACES: &str = "https://api.spaces.skype.com";
const PLANTED: &str = "planted-secret-value-0123456789";
const ACCOUNT: &str = "a@lab.example.com";

/// A sealer for tests: a tag, then the bytes reversed and masked. It refuses while `refuse` is set.
#[derive(Debug, Default)]
struct FakeSealer {
    refuse: AtomicBool,
}

const TAG: &[u8] = b"fake-sealed:";

impl Sealer for FakeSealer {
    fn seal(&self, plain: &[u8]) -> Result<Vec<u8>, SecretError> {
        if self.refuse.load(Ordering::SeqCst) {
            return Err(SecretError::Sealer("refused"));
        }
        Ok(TAG
            .iter()
            .copied()
            .chain(plain.iter().rev().map(|byte| byte ^ 0x5a))
            .collect())
    }

    fn open(&self, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, SecretError> {
        let body = sealed.strip_prefix(TAG).ok_or(SecretError::Sealer("not sealed here"))?;
        if self.refuse.load(Ordering::SeqCst) {
            return Err(SecretError::Sealer("refused"));
        }
        Ok(Zeroizing::new(body.iter().rev().map(|byte| byte ^ 0x5a).collect()))
    }
}

fn key(audience: &str) -> SecretKey {
    SecretKey::new(ACCOUNT, audience)
}

fn store(dir: &std::path::Path) -> (SealedFileStore, Arc<FakeSealer>) {
    let sealer = Arc::new(FakeSealer::default());
    let store = SealedFileStore::new(dir, "synthetic-sealed", Arc::clone(&sealer) as Arc<dyn Sealer>).unwrap();
    (store, sealer)
}

async fn value(store: &SealedFileStore, audience: &str) -> Option<String> {
    store
        .get(&key(audience))
        .await
        .unwrap()
        .map(|secret| secret.expose().to_owned())
}

fn files(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn puts_gets_replaces_and_deletes_per_namespace_account_and_audience() {
    let dir = tempfile::tempdir().unwrap();
    let (store, sealer) = store(dir.path());
    let other = SealedFileStore::new(dir.path(), "synthetic-other", sealer).unwrap();

    assert_eq!(value(&store, SPACES).await, None);
    store.put(&key(SPACES), &Secret::new("spaces-1")).await.unwrap();
    store.put(&key("skypetoken"), &Secret::new("skype-1")).await.unwrap();
    store
        .put(&SecretKey::new("b@lab.example.com", SPACES), &Secret::new("b-1"))
        .await
        .unwrap();
    assert_eq!(value(&store, SPACES).await.as_deref(), Some("spaces-1"));
    assert_eq!(value(&store, "skypetoken").await.as_deref(), Some("skype-1"));
    assert_eq!(value(&other, SPACES).await, None, "namespaces are separate");

    store.put(&key(SPACES), &Secret::new("spaces-2")).await.unwrap();
    assert_eq!(value(&store, SPACES).await.as_deref(), Some("spaces-2"));
    assert_eq!(
        files(dir.path()).len(),
        3,
        "a replace leaves one file and no temporary file"
    );

    store.delete(&key(SPACES)).await.unwrap();
    store.delete(&key(SPACES)).await.unwrap();
    assert_eq!(value(&store, SPACES).await, None);
    assert_eq!(
        value(&store, "skypetoken").await.as_deref(),
        Some("skype-1"),
        "a delete touches only its key"
    );
}

#[tokio::test]
async fn files_are_private_sealed_and_named_by_a_hash() {
    let parent = tempfile::tempdir().unwrap();
    let dir = parent.path().join("secrets");
    let (store, _) = store(&dir);
    store.put(&key(SPACES), &Secret::new(PLANTED)).await.unwrap();

    let names = files(&dir);
    assert_eq!(names.len(), 1);
    let name = &names[0];
    let stem = name.strip_suffix(".sealed").unwrap();
    assert_eq!(stem.len(), 64);
    assert!(stem.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let bytes = fs::read(dir.join(name)).unwrap();
    assert!(bytes.starts_with(TAG), "the bytes on disk are the sealer's");
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains(PLANTED) && !text.contains(ACCOUNT));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |path: &std::path::Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(name)), 0o600);
    }

    let shown = format!("{store:?}");
    assert!(
        shown.contains("synthetic-sealed") && !shown.contains("secrets"),
        "{shown}"
    );
}

#[tokio::test]
async fn a_sealer_that_refuses_writes_nothing_and_reads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (store, sealer) = store(dir.path());
    store.put(&key(SPACES), &Secret::new("kept")).await.unwrap();

    sealer.refuse.store(true, Ordering::SeqCst);
    let error = store.put(&key(SPACES), &Secret::new("new")).await.unwrap_err();
    assert!(matches!(error, SecretError::Sealer("refused")), "{error:?}");
    let error = store.get(&key(SPACES)).await.unwrap_err();
    assert!(matches!(error, SecretError::Sealer("refused")), "{error:?}");

    sealer.refuse.store(false, Ordering::SeqCst);
    assert_eq!(value(&store, SPACES).await.as_deref(), Some("kept"));
}

#[tokio::test]
async fn foreign_and_non_text_files_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (store, sealer) = store(dir.path());
    store.put(&key(SPACES), &Secret::new("x")).await.unwrap();
    let path = dir.path().join(&files(dir.path())[0]);

    fs::write(&path, b"another sealer's bytes").unwrap();
    let error = store.get(&key(SPACES)).await.unwrap_err();
    assert!(matches!(error, SecretError::Sealer("not sealed here")), "{error:?}");

    let not_text = sealer.seal(&[0xff, 0xfe]).unwrap();
    fs::write(&path, not_text).unwrap();
    assert!(matches!(store.get(&key(SPACES)).await, Err(SecretError::NotUtf8)));
}

#[tokio::test]
async fn io_failures_are_reported_by_kind_and_leave_no_temporary_file() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = store(dir.path());
    store.put(&key(SPACES), &Secret::new("x")).await.unwrap();
    let name = files(dir.path()).remove(0);
    fs::remove_file(dir.path().join(&name)).unwrap();
    fs::create_dir(dir.path().join(&name)).unwrap();
    fs::write(
        dir.path().join(&name).join("inside"),
        b"keeps the directory from a rename",
    )
    .unwrap();

    assert!(matches!(store.get(&key(SPACES)).await, Err(SecretError::SealedFile(_))));
    assert!(matches!(
        store.put(&key(SPACES), &Secret::new("y")).await,
        Err(SecretError::SealedFile(_))
    ));
    assert!(matches!(
        store.delete(&key(SPACES)).await,
        Err(SecretError::SealedFile(_))
    ));
    assert_eq!(files(dir.path()), [name], "the failed put removed its temporary file");
}

#[test]
fn a_directory_that_cannot_be_made_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("file");
    fs::write(&blocker, b"").unwrap();
    let sealer: Arc<dyn Sealer> = Arc::new(FakeSealer::default());
    let error = SealedFileStore::new(blocker.join("secrets"), "synthetic-sealed", sealer).unwrap_err();
    assert!(matches!(error, SecretError::SealedFile(_)), "{error:?}");
    assert!(error.to_string().starts_with("sealed secret file: "));
}

#[test]
fn two_stores_of_one_directory_never_leave_a_partial_secret() {
    let dir = tempfile::tempdir().unwrap();
    let (first, sealer) = store(dir.path());
    let second = SealedFileStore::new(dir.path(), "synthetic-sealed", sealer).unwrap();
    let long = |mark: char| mark.to_string().repeat(64 * 1024);
    std::thread::scope(|scope| {
        for (store, mark) in [(&first, 'a'), (&second, 'b')] {
            scope.spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
                for _ in 0..50 {
                    runtime
                        .block_on(store.put(&key(SPACES), &Secret::new(long(mark))))
                        .unwrap();
                    let read = runtime.block_on(store.get(&key(SPACES))).unwrap().unwrap();
                    assert!(
                        [long('a'), long('b')].iter().any(|whole| read.expose() == whole),
                        "a whole secret, never part of one"
                    );
                }
            });
        }
    });
}
