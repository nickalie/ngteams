//! Synthetic credentials in a private session bus and temporary keyring; no desktop services are inherited.

#![cfg(target_os = "linux")]

#[cfg(unix)]
#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write as _;
    use std::os::unix::process::CommandExt as _;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::time::{Duration, Instant};

    use teams_auth::secrets::{KeyringSecretStore, LazyKeyring, Secret, SecretError, SecretKey, SecretStore};

    const CHILD_ROOT: &str = "NGTEAMS_PRIVATE_KEYRING_TEST_ROOT";
    const TEST_NAME: &str = "tests::private_keyring_stores_and_lazy_connections_use_only_the_private_service";
    const WAIT: Duration = Duration::from_secs(15);
    const KEYRING_READY_WAIT: Duration = Duration::from_secs(5);

    struct Process {
        child: Child,
        group: bool,
    }

    impl Process {
        fn wait(&mut self) -> ExitStatus {
            let deadline = Instant::now() + WAIT;
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    return status;
                }
                assert!(Instant::now() < deadline, "private service test did not finish");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            if self.group {
                // The parent owns a new process group, so a timeout also stops its bus and service descendants.
                let _ = Command::new("/usr/bin/kill")
                    .args(["-TERM", "--", &format!("-{}", self.child.id())])
                    .env_clear()
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn private_child(dir: &Path) -> Command {
        for name in ["home", "config", "data", "cache", "runtime", "control"] {
            fs::create_dir(dir.join(name)).unwrap();
        }
        let config = dir.join("bus.conf");
        fs::write(
            &config,
            format!(
                "<busconfig><type>session</type><listen>unix:path={}/bus</listen><auth>EXTERNAL</auth>\
             <policy context=\"default\"><allow user=\"*\"/><allow send_destination=\"*\"/>\
             <allow receive_sender=\"*\"/><allow own=\"*\"/></policy></busconfig>",
                dir.display()
            ),
        )
        .unwrap();
        let mut command = Command::new("/usr/bin/dbus-run-session");
        command
            .args(["--config-file", config.to_str().unwrap(), "--"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", TEST_NAME, "--nocapture"])
            .process_group(0)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", dir.join("home"))
            .env("XDG_CONFIG_HOME", dir.join("config"))
            .env("XDG_DATA_HOME", dir.join("data"))
            .env("XDG_CACHE_HOME", dir.join("cache"))
            .env("XDG_RUNTIME_DIR", dir.join("runtime"))
            .env(
                "DBUS_SYSTEM_BUS_ADDRESS",
                format!("unix:path={}/no-system-bus", dir.display()),
            )
            .env(CHILD_ROOT, dir)
            .stdin(Stdio::null())
            .stdout(fs::File::create(dir.join("child.stdout")).unwrap())
            .stderr(fs::File::create(dir.join("child.stderr")).unwrap());
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }
        command
    }

    fn assert_private_environment(dir: &Path) {
        let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap();
        let expected = format!("unix:path={}/bus", dir.display());
        assert!(address == expected || address.starts_with(&format!("{expected},")));
        for (variable, name) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_RUNTIME_DIR", "runtime"),
        ] {
            assert_eq!(PathBuf::from(std::env::var_os(variable).unwrap()), dir.join(name));
        }
        assert!(std::env::var_os("DISPLAY").is_none());
        assert!(std::env::var_os("WAYLAND_DISPLAY").is_none());
    }

    fn start_keyring(dir: &Path) -> Process {
        let child = Command::new("/usr/bin/gnome-keyring-daemon")
            .args([
                "--foreground",
                "--components=secrets",
                "--unlock",
                "--control-directory",
            ])
            .arg(dir.join("control"))
            .stdin(Stdio::piped())
            .stdout(fs::File::create(dir.join("daemon.stdout")).unwrap())
            .stderr(fs::File::create(dir.join("daemon.stderr")).unwrap())
            .spawn()
            .unwrap();
        let mut daemon = Process { child, group: false };
        daemon.child.stdin.take().unwrap().write_all(b"\n").unwrap();
        daemon
    }

    async fn wait_for_keyring(daemon: &mut Process) -> KeyringSecretStore {
        let deadline = Instant::now() + KEYRING_READY_WAIT;
        loop {
            if let Ok(store) = KeyringSecretStore::new("synthetic-private-main").await {
                return store;
            }
            assert!(
                daemon.child.try_wait().unwrap().is_none(),
                "private keyring daemon exited"
            );
            assert!(Instant::now() < deadline, "private keyring daemon did not become ready");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn assert_value(store: &impl SecretStore, key: &SecretKey, expected: Option<&str>) {
        let value = store.get(key).await.unwrap();
        assert_eq!(value.as_ref().map(Secret::expose), expected);
    }

    async fn storage_contract(store: &impl SecretStore) {
        let first = SecretKey::new("synthetic-a", "synthetic-spaces");
        let audience = SecretKey::new("synthetic-a", "synthetic-skype");
        let account = SecretKey::new("synthetic-b", "synthetic-spaces");
        assert_value(store, &first, None).await;
        store.delete(&first).await.unwrap();
        for (key, value) in [(&first, "first"), (&audience, "audience"), (&account, "account")] {
            store.put(key, &Secret::new(value)).await.unwrap();
        }
        store.put(&first, &Secret::new("replacement")).await.unwrap();
        assert_value(store, &first, Some("replacement")).await;
        assert_value(store, &audience, Some("audience")).await;
        assert_value(store, &account, Some("account")).await;
        storage_deletes(store, &first, [&audience, &account]).await;
    }

    async fn storage_deletes(store: &impl SecretStore, first: &SecretKey, others: [&SecretKey; 2]) {
        store.delete(first).await.unwrap();
        store.delete(first).await.unwrap();
        assert_value(store, first, None).await;
        assert_value(store, others[0], Some("audience")).await;
        assert_value(store, others[1], Some("account")).await;
        for key in others {
            store.delete(key).await.unwrap();
            assert_value(store, key, None).await;
        }
    }

    async fn malformed_secret_is_rejected() {
        let backend = oo7::Keyring::new().await.unwrap();
        let attributes = [
            ("application", "synthetic-private-main"),
            ("account", "synthetic-invalid"),
            ("audience", "synthetic-binary"),
        ];
        backend
            .create_item(
                "synthetic-invalid-utf8",
                &attributes,
                oo7::Secret::blob([0xff, 0xfe]),
                true,
            )
            .await
            .unwrap();
        let store = KeyringSecretStore::new("synthetic-private-main").await.unwrap();
        let key = SecretKey::new("synthetic-invalid", "synthetic-binary");
        let error = store.get(&key).await.unwrap_err();
        assert!(matches!(error, SecretError::NotUtf8));
        assert_eq!(error.to_string(), "the stored secret is not UTF-8");
        store.delete(&key).await.unwrap();
        assert_value(&store, &key, None).await;
    }

    async fn namespace_and_debug_contract(store: &KeyringSecretStore) {
        let key = SecretKey::new("synthetic-shared", "synthetic-audience");
        let other = KeyringSecretStore::new("synthetic-private-other").await.unwrap();
        store.put(&key, &Secret::new("synthetic-planted-secret")).await.unwrap();
        assert_value(&other, &key, None).await;
        other.put(&key, &Secret::new("synthetic-other-secret")).await.unwrap();
        other.delete(&key).await.unwrap();
        assert_value(store, &key, Some("synthetic-planted-secret")).await;
        assert!(!format!("{store:?}").contains("synthetic-planted-secret"));
        store.delete(&key).await.unwrap();
    }

    async fn private_contract(dir: &Path) {
        let mut daemon = start_keyring(dir);
        let store = wait_for_keyring(&mut daemon).await;
        storage_contract(&store).await;
        namespace_and_debug_contract(&store).await;
        let lazy = LazyKeyring::new("synthetic-private-lazy");
        assert!(!lazy.connected());
        storage_contract(&lazy).await;
        assert!(lazy.connected());
        malformed_secret_is_rejected().await;
        drop(daemon);
        failed_connections_are_retried(dir).await;
    }

    async fn failed_connections_are_retried(dir: &Path) {
        let key = SecretKey::new("synthetic-retry", "synthetic-audience");
        let unavailable = LazyKeyring::new("synthetic-private-unavailable");
        for operation in ["get", "put", "delete"] {
            let result = match operation {
                "put" => unavailable.put(&key, &Secret::new("synthetic-unused")).await,
                "delete" => unavailable.delete(&key).await,
                _ => unavailable.get(&key).await.map(|_| ()),
            };
            assert!(matches!(result, Err(SecretError::Keyring(_))), "{operation}");
            assert!(!unavailable.connected());
        }
        let mut restarted = start_keyring(dir);
        drop(wait_for_keyring(&mut restarted).await);
        unavailable
            .put(&key, &Secret::new("synthetic-after-retry"))
            .await
            .unwrap();
        assert!(unavailable.connected());
        assert_value(&unavailable, &key, Some("synthetic-after-retry")).await;
        unavailable.delete(&key).await.unwrap();
        assert_value(&unavailable, &key, None).await;
    }

    #[test]
    fn private_keyring_stores_and_lazy_connections_use_only_the_private_service() {
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            assert_private_environment(&root);
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async { tokio::time::timeout(Duration::from_secs(10), private_contract(&root)).await });
            assert!(
                result.is_ok(),
                "private keyring contract timed out: {}",
                fs::read_to_string(root.join("daemon.stderr")).unwrap_or_default()
            );
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut child = Process {
            child: private_child(dir.path()).spawn().unwrap(),
            group: true,
        };
        let status = child.wait();
        let stdout = fs::read_to_string(dir.path().join("child.stdout")).unwrap();
        let stderr = fs::read_to_string(dir.path().join("child.stderr")).unwrap();
        assert!(status.success(), "private keyring child failed:\n{stdout}\n{stderr}");
        assert!(stdout.contains("1 passed; 0 failed"), "{stdout}");
    }
}
