//! The in-process server on loopback: SQLite in a temporary directory, or a fresh database on the Postgres at
//! `NGTEAMS_SERVER_PG_URL` when that is set; Redis at `NGTEAMS_SERVER_REDIS_URL` likewise.

use std::sync::Arc;

use ngteams_server::app::{self, App};
use ngteams_server::config::Config;
use ngteams_server::serve::Listener;
use serde_json::Value;
use teams_auth::authz::AuthzClient;
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore as _};
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, Scope, TokenClient};
use tempfile::TempDir;
use tokio::sync::oneshot;

pub const PG_ENV: &str = "NGTEAMS_SERVER_PG_URL";
pub const REDIS_ENV: &str = "NGTEAMS_SERVER_REDIS_URL";
/// The client id the native client signs in as.
pub const CLIENT_ID: &str = "1fec8e78-bce4-4aaf-ab1b-5451cc387264";
pub const PASSWORD: &str = "correct horse battery";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Sqlite,
    Postgres,
}

/// SQLite always; Postgres too when its URL is set.
pub fn backends() -> Vec<Backend> {
    let mut backends = vec![Backend::Sqlite];
    if std::env::var_os(PG_ENV).is_some() {
        backends.push(Backend::Postgres);
    }
    backends
}

pub fn redis_url() -> Option<String> {
    std::env::var(REDIS_ENV).ok()
}

/// A database URL of `backend` nobody else uses.
pub async fn database_url(backend: Backend, dir: &TempDir) -> String {
    match backend {
        Backend::Sqlite => format!("sqlite://{}", dir.path().join("server.sqlite").display()),
        Backend::Postgres => fresh_postgres().await,
    }
}

async fn fresh_postgres() -> String {
    use diesel_async::{AsyncConnection as _, AsyncPgConnection, SimpleAsyncConnection as _};
    let base = std::env::var(PG_ENV).unwrap();
    let name = format!("ngts_{}", uuid::Uuid::new_v4().simple());
    let mut admin = AsyncPgConnection::establish(&base).await.unwrap();
    admin.batch_execute(&format!("CREATE DATABASE {name}")).await.unwrap();
    let mut url = url::Url::parse(&base).unwrap();
    url.set_path(&format!("/{name}"));
    url.to_string()
}

/// A running server.
pub struct TestServer {
    pub origin: String,
    pub app: Arc<App>,
    pub backend: Backend,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
    _dir: TempDir,
}

impl TestServer {
    pub async fn start(backend: Backend) -> Self {
        Self::start_with(backend, |_| {}).await
    }

    pub async fn start_with(backend: Backend, configure: impl FnOnce(&mut Config)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let listener = Listener::bind(([127, 0, 0, 1], 0).into()).unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://127.0.0.1:{}", address.port());
        let mut config = Config {
            listen: address,
            public_origin: url::Url::parse(&origin).unwrap(),
            database_url: database_url(backend, &dir).await,
            ..Config::default()
        };
        configure(&mut config);
        let app = App::start(config).await.unwrap();
        let router = app::router(Arc::clone(&app));
        let (shutdown, stop) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            listener
                .serve(router, None, async move {
                    let _ = stop.await;
                })
                .await
                .unwrap();
        });
        Self {
            origin,
            app,
            backend,
            shutdown: Some(shutdown),
            task: Some(task),
            _dir: dir,
        }
    }

    /// Stops it and waits for it.
    pub async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.await.unwrap();
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }

    pub fn token_endpoint(&self) -> String {
        self.url("/oauth2/v2.0/token")
    }

    pub fn authz_endpoint(&self) -> String {
        self.url("/api/authsvc/v1.0/authz")
    }

    /// Adds an account with [`PASSWORD`]; its object id.
    pub async fn add_user(&self, upn: &str, name: &str) -> String {
        ngteams_server::admin::add_user(&self.app.db, upn, name, PASSWORD)
            .await
            .unwrap()
            .id
    }

    /// The client's token client with this server as its authority.
    pub fn token_client(&self) -> TokenClient {
        TokenClient::new(EntraConfig::teams_native().with_authority(&self.origin)).unwrap()
    }

    pub fn authz_client(&self) -> AuthzClient {
        AuthzClient::with_endpoint(&self.authz_endpoint()).unwrap()
    }

    /// An own-server sign-in: the password grant for the skypetoken's audience, as the client's sign-in form posts
    /// it; the answer's status and JSON.
    pub async fn password_grant(&self, upn: &str, password: &str, scope: &Scope) -> (u16, Value) {
        let response = reqwest::Client::new()
            .post(self.token_endpoint())
            .form(&[
                ("client_id", CLIENT_ID),
                ("grant_type", "password"),
                ("username", upn),
                ("password", password),
                ("scope", scope.as_str()),
            ])
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    /// Signs `upn` in with the password grant and stores its refresh token as the client keeps it: a session's
    /// store.
    pub async fn signed_in_store(&self, upn: &str) -> MemorySecretStore {
        let (status, answer) = self.password_grant(upn, PASSWORD, &Scope::spaces()).await;
        assert_eq!(status, 200, "{answer}");
        let store = MemorySecretStore::new();
        let refresh = answer["refresh_token"].as_str().unwrap();
        store.put(&refresh_token_key(upn), &Secret::new(refresh)).await.unwrap();
        store
    }

    /// The client's session for a signed-in `upn` against this server.
    pub async fn session(&self, upn: &str) -> Session<MemorySecretStore> {
        let store = self.signed_in_store(upn).await;
        Session::new(upn, store, self.token_client(), self.authz_client(), Default::default())
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
