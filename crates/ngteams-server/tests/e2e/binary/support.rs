use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::Value;
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore as _};
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, Scope, TokenClient};
use tempfile::TempDir;

use crate::support::{Backend, CLIENT_ID, database_url};

pub const TLS_DIR: &str = "NGTS_QA_TLS_DIR";
pub const LOGIN_WINDOW_SECONDS: u64 = 30;

pub struct Deployment {
    pub dir: TempDir,
    pub database: String,
    pub origin: String,
    pub tls: bool,
    pub redis: Option<String>,
}

impl Deployment {
    pub async fn new(backend: Backend, tls: bool, redis: Option<String>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let database = database_url(backend, &dir).await;
        let origin = format!("{}://127.0.0.1:{}", if tls { "https" } else { "http" }, free_port());
        Self {
            dir,
            database,
            origin,
            tls,
            redis,
        }
    }

    pub fn node(&self, name: &str) -> Node {
        let port = if name == "a" {
            url::Url::parse(&self.origin).unwrap().port().unwrap()
        } else {
            free_port()
        };
        let endpoint = format!("{}://127.0.0.1:{port}", if self.tls { "https" } else { "http" });
        let config = self.dir.path().join(format!("{name}.toml"));
        let mut text = format!(
            "listen = \"127.0.0.1:{port}\"\npublic_origin = {:?}\ndatabase_url = {:?}\n",
            self.origin, self.database
        );
        if let Some(redis) = &self.redis {
            writeln!(text, "redis_url = {redis:?}").unwrap();
        }
        writeln!(text, "[login]\nmax_failures = 3\nwindow = {LOGIN_WINDOW_SECONDS}").unwrap();
        if self.tls {
            let certs = PathBuf::from(std::env::var_os(TLS_DIR).unwrap());
            writeln!(
                text,
                "[tls]\ncert = {:?}\nkey = {:?}",
                certs.join("leaf.pem"),
                certs.join("key.pem")
            )
            .unwrap();
        }
        std::fs::write(&config, text).unwrap();
        Node {
            config,
            endpoint,
            process: None,
            http: http_client(),
        }
    }
}

pub struct Node {
    pub config: PathBuf,
    pub endpoint: String,
    process: Option<Child>,
    pub http: reqwest::Client,
}

impl Node {
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ngteams-server"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("NGTEAMS_SERVER_") {
                command.env_remove(key);
            }
        }
        command.args(["--config", self.config.to_str().unwrap()]);
        command
            .env("NGTEAMS_SERVER_LOG", "warn")
            .env("TOKIO_WORKER_THREADS", "2");
        command
    }

    pub fn cli(&self, args: &[&str], input: &str) -> String {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "CLI {args:?} failed (output withheld)");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stdout.contains("PRIVATE KEY"));
        stdout
    }

    pub fn add_user(&self, upn: &str, password: &str) {
        let added = self.cli(&["user", "add", upn, "--name", "QA Operator"], &format!("{password}\n"));
        assert!(added.starts_with(&format!("added {upn} (")));
        assert!(!added.contains(password));
    }

    pub async fn start(&mut self) {
        assert!(self.process.is_none());
        self.process = Some(
            self.command()
                .arg("serve")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while tokio::time::Instant::now() < deadline {
            assert!(
                self.process.as_mut().unwrap().try_wait().unwrap().is_none(),
                "binary exited before ready"
            );
            if self.status("/health/ready").await == Some(204) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("binary never became ready");
    }

    pub async fn stop(&mut self) {
        let Some(mut process) = self.process.take() else { return };
        assert!(
            Command::new("kill")
                .args(["-TERM", &process.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            if let Some(status) = process.try_wait().unwrap() {
                assert!(status.success(), "SIGTERM failed");
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let _ = process.kill();
        let _ = process.wait();
        panic!("binary did not stop after SIGTERM");
    }

    pub async fn status(&self, path: &str) -> Option<u16> {
        self.http
            .get(format!("{}{path}", self.endpoint))
            .send()
            .await
            .ok()
            .map(|r| r.status().as_u16())
    }

    pub async fn password(&self, upn: &str, password: &str) -> reqwest::Response {
        self.http
            .post(format!("{}/oauth2/v2.0/token", self.endpoint))
            .form(&[
                ("client_id", CLIENT_ID),
                ("grant_type", "password"),
                ("username", upn),
                ("password", password),
                ("scope", Scope::spaces().as_str()),
            ])
            .send()
            .await
            .unwrap()
    }

    pub async fn sign_in(&self, upn: &str, password: &str) -> Session<MemorySecretStore> {
        let response = self.password(upn, password).await;
        assert_eq!(response.status(), 200);
        let answer: Value = response.json().await.unwrap();
        let store = MemorySecretStore::new();
        store
            .put(
                &refresh_token_key(upn),
                &Secret::new(answer["refresh_token"].as_str().unwrap()),
            )
            .await
            .unwrap();
        self.session(upn, store)
    }

    pub fn session(&self, upn: &str, store: MemorySecretStore) -> Session<MemorySecretStore> {
        Session::new(
            upn,
            store,
            TokenClient::new(EntraConfig::teams_native().with_authority(&self.endpoint)).unwrap(),
            self.authz(),
            Region::default(),
        )
    }

    pub fn authz(&self) -> AuthzClient {
        AuthzClient::with_endpoint(&format!("{}/api/authsvc/v1.0/authz", self.endpoint)).unwrap()
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        if let Some(mut process) = self.process.take() {
            let _ = process.kill();
            let _ = process.wait();
        }
    }
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(12))
        .build()
        .unwrap()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

// Re-exec isolates the TLS trust environment from parallel tests and the user's trust store.
pub fn tls_parent(test: &str) -> bool {
    if std::env::var_os(TLS_DIR).is_some() {
        return false;
    }
    let dir = tempfile::tempdir().unwrap();
    write_test_ca(dir.path());
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(TLS_DIR, dir.path())
        .env("SSL_CERT_FILE", dir.path().join("ca.pem"))
        .env("SSL_CERT_DIR", dir.path().join("empty"))
        .status()
        .unwrap();
    assert!(status.success(), "TLS child failed");
    true
}

fn write_test_ca(dir: &Path) {
    use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca_key = KeyPair::generate().unwrap();
    let ca = params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(params, ca_key);
    let key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec!["127.0.0.1".into()])
        .unwrap()
        .signed_by(&key, &issuer)
        .unwrap();
    std::fs::write(dir.join("ca.pem"), ca.pem()).unwrap();
    std::fs::write(dir.join("leaf.pem"), leaf.pem()).unwrap();
    std::fs::write(dir.join("key.pem"), key.serialize_pem()).unwrap();
    std::fs::create_dir(dir.join("empty")).unwrap();
}
