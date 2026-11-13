//! The lazy store through the lab's own keychain under a temporary home: it needs no login keychain, so it runs over
//! SSH and on CI alike. Each case runs in a child of this test binary with its own `HOME`, so no process changes its
//! environment and the user's lab keychain is never reached.

#![cfg(target_os = "macos")]
#![cfg(test)]

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

use teams_auth::secrets::{LazyKeyring, Secret, SecretError, SecretKey, SecretStore};

/// Set in a child: which case it runs.
const CHILD_CASE: &str = "NGTEAMS_LAB_KEYRING_TEST_CASE";
const LAB: &str = "ngteams-lab";
/// `errSecNoSuchKeychain`: no absolute home to find the lab keychain in.
const NO_SUCH_KEYCHAIN: i32 = -25294;

fn key() -> SecretKey {
    SecretKey::new("synthetic@example.test", "https://api.example.test/.default")
}

/// Runs `test` alone in a child of this binary with `home` as its `HOME`.
fn child(test: &str, home: &OsStr) {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([test, "--exact", "--test-threads=1"])
        .env("HOME", home)
        .env(CHILD_CASE, test)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{test} child failed:\n{stdout}\n{stderr}");
    assert!(stdout.contains("1 passed; 0 failed"), "{stdout}");
}

fn in_child(test: &str) -> bool {
    std::env::var_os(CHILD_CASE).is_some_and(|case| case == test)
}

fn block_on(future: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future);
}

#[test]
fn a_lazy_lab_store_connects_on_first_use_and_keeps_items_in_the_lab_keychain() {
    const TEST: &str = "a_lazy_lab_store_connects_on_first_use_and_keeps_items_in_the_lab_keychain";
    if !in_child(TEST) {
        let home = tempfile::tempdir().unwrap();
        child(TEST, home.path().as_os_str());
        return;
    }
    let home = std::env::var_os("HOME").unwrap();
    let keychain = Path::new(&home).join("Library/Application Support/ngteams-lab/ngteams-lab.keychain-db");
    block_on(async {
        let lazy = LazyKeyring::new(LAB);
        assert!(!lazy.connected());
        assert!(lazy.get(&key()).await.unwrap().is_none());
        assert!(lazy.connected(), "the first use connects");
        assert!(!keychain.exists(), "a read makes no keychain");

        lazy.put(&key(), &Secret::new("synthetic-first ✓")).await.unwrap();
        assert!(keychain.exists(), "the item is kept in the lab keychain under the home");
        assert_eq!(lazy.get(&key()).await.unwrap().unwrap().expose(), "synthetic-first ✓");
        lazy.put(&key(), &Secret::new("synthetic-second")).await.unwrap();
        assert_eq!(lazy.get(&key()).await.unwrap().unwrap().expose(), "synthetic-second");

        lazy.delete(&key()).await.unwrap();
        lazy.delete(&key()).await.unwrap();
        assert!(lazy.get(&key()).await.unwrap().is_none());
    });
}

#[test]
fn a_lazy_lab_store_without_an_absolute_home_fails_each_request_and_stays_unconnected() {
    const TEST: &str = "a_lazy_lab_store_without_an_absolute_home_fails_each_request_and_stays_unconnected";
    if !in_child(TEST) {
        child(TEST, OsStr::new("relative-home"));
        return;
    }
    block_on(async {
        let lazy = LazyKeyring::new(LAB);
        for operation in ["get", "put", "delete"] {
            let result = match operation {
                "put" => lazy.put(&key(), &Secret::new("synthetic-unused")).await,
                "delete" => lazy.delete(&key()).await,
                _ => lazy.get(&key()).await.map(|_| ()),
            };
            assert!(
                matches!(result, Err(SecretError::Keychain(NO_SUCH_KEYCHAIN))),
                "{operation}: {result:?}"
            );
            assert!(
                !lazy.connected(),
                "{operation}: a failed connection is tried again on the next request"
            );
        }
    });
    assert!(!Path::new("relative-home").exists(), "no keychain directory is made");
}
