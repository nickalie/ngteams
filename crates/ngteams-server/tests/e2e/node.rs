//! The node itself: health, readiness, TLS and configuration.

use diesel_async::{AsyncConnection as _, AsyncPgConnection, SimpleAsyncConnection as _};
use ngteams_server::app::App;
use ngteams_server::config::{Config, ConfigError, TlsConfig};
use ngteams_server::serve::Listener;

use crate::support::{TestServer, backends};

#[tokio::test(flavor = "multi_thread")]
async fn the_node_answers_live_and_ready_while_its_database_answers() {
    for backend in backends() {
        let server = TestServer::start(backend).await;
        let http = reqwest::Client::new();
        for path in ["/health/live", "/health/ready"] {
            assert_eq!(http.get(server.url(path)).send().await.unwrap().status(), 204, "{path}");
        }
        assert_eq!(http.get(server.url("/nothing")).send().await.unwrap().status(), 404);
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_node_serves_tls_with_its_configured_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let tls = TlsConfig {
        cert: dir.path().join("cert.pem"),
        key: dir.path().join("key.pem"),
    };
    std::fs::write(&tls.cert, certified.cert.pem()).unwrap();
    std::fs::write(&tls.key, certified.signing_key.serialize_pem()).unwrap();
    let listener = Listener::bind(([127, 0, 0, 1], 0).into()).unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = Config {
        public_origin: url::Url::parse(&format!("https://localhost:{port}")).unwrap(),
        database_url: format!("sqlite://{}", dir.path().join("db.sqlite").display()),
        tls: Some(tls.clone()),
        ..Config::default()
    };
    let app = App::start(config).await.unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(listener.serve(ngteams_server::app::router(app), Some(tls), async move {
        let _ = stopped.await;
    }));
    let root = reqwest::Certificate::from_pem(certified.cert.pem().as_bytes()).unwrap();
    let http = reqwest::Client::builder()
        .tls_certs_only([root])
        .resolve("localhost", ([127, 0, 0, 1], port).into())
        .build()
        .unwrap();
    let answer = http
        .get(format!("https://localhost:{port}/health/live"))
        .send()
        .await
        .unwrap();
    assert_eq!(answer.status(), 204);
    stop.send(()).unwrap();
    served.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_with_unreadable_tls_files_does_not_serve() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bad.pem"), "not a pem").unwrap();
    for (cert, key) in [("missing.pem", "missing.pem"), ("bad.pem", "bad.pem")] {
        let tls = TlsConfig {
            cert: dir.path().join(cert),
            key: dir.path().join(key),
        };
        let listener = Listener::bind(([127, 0, 0, 1], 0).into()).unwrap();
        let router = axum::Router::new();
        assert!(listener.serve(router, Some(tls), async {}).await.is_err());
    }
}

#[test]
fn the_configuration_layers_a_file_over_the_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("server.toml");
    std::fs::write(
        &file,
        "public_origin = \"https://teams.example.com\"\nregion = \"emea\"\n[tokens]\naccess_lifetime = 600\n",
    )
    .unwrap();
    let config = Config::load(Some(&file)).unwrap();
    assert_eq!(config.origin(), "https://teams.example.com");
    assert_eq!(config.region, "emea");
    assert_eq!(config.tokens.access_lifetime, 600);
    assert_eq!(config.tokens.reuse_grace, 60);
    assert!(Config::load(None).is_ok());
    std::fs::write(&file, "unknown = 1\n").unwrap();
    assert!(matches!(Config::load(Some(&file)), Err(ConfigError::Load(_))));
}

fn with_origin(origin: &str) -> Config {
    Config {
        public_origin: url::Url::parse(origin).unwrap(),
        ..Config::default()
    }
}

#[test]
fn the_configuration_refuses_an_origin_clients_would_not_take() {
    for origin in [
        "http://teams.example.com",
        "https://teams.example.com/path",
        "https://u@teams.example.com",
    ] {
        assert!(
            matches!(with_origin(origin).check(), Err(ConfigError::PublicOrigin)),
            "{origin}"
        );
    }
    for origin in [
        "http://localhost:1",
        "http://[::1]:1",
        "http://127.0.0.1:1",
        "https://teams.example.com",
    ] {
        assert!(with_origin(origin).check().is_ok(), "{origin}");
    }
}

#[test]
fn the_configuration_refuses_bad_names_databases_and_lifetimes() {
    let refused = |change: fn(&mut Config)| {
        let mut config = Config::default();
        change(&mut config);
        config.check().unwrap_err()
    };
    assert!(matches!(
        refused(|c| c.region = "Bad Region".into()),
        ConfigError::Region
    ));
    assert!(matches!(refused(|c| c.partition = String::new()), ConfigError::Region));
    assert!(matches!(
        refused(|c| c.database_url = "mysql://x".into()),
        ConfigError::Database
    ));
    assert!(matches!(
        refused(|c| c.tokens.access_lifetime = 0),
        ConfigError::Lifetime
    ));
    assert!(matches!(refused(|c| c.login.window = 0), ConfigError::Lifetime));
}

#[tokio::test(flavor = "multi_thread")]
async fn housekeeping_rotates_a_due_key_and_the_earlier_tokens_still_verify() {
    let server = TestServer::start_with(crate::support::Backend::Sqlite, |config| config.tokens.key_rotation = 1).await;
    server.add_user("ada@own.test", "Ada").await;
    let session = server.session("ada@own.test").await;
    let before = session.access_token(teams_auth::token::SPACES_RESOURCE).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    ngteams_server::app::tidy(&server.app).await;
    let kid = |token: &teams_auth::secrets::Secret| jsonwebtoken::decode_header(token.expose()).unwrap().kid.unwrap();
    let after = server.session("ada@own.test").await;
    let fresh = after.access_token(teams_auth::token::SPACES_RESOURCE).await.unwrap();
    assert_ne!(kid(&before.token), kid(&fresh.token));
    session.skypetoken().await.unwrap();
    assert!(format!("{:?}", server.app.issuer.keys()).contains("KeyRing"));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_whose_database_is_gone_is_not_ready() {
    if !backends().contains(&crate::support::Backend::Postgres) {
        return;
    }
    let server = TestServer::start(crate::support::Backend::Postgres).await;
    let url = url::Url::parse(&server.app.config.database_url).unwrap();
    let name = url.path().trim_start_matches('/').to_owned();
    let mut admin = AsyncPgConnection::establish(&std::env::var(crate::support::PG_ENV).unwrap())
        .await
        .unwrap();
    admin
        .batch_execute(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .await
        .unwrap();
    let ready = reqwest::get(server.url("/health/ready")).await.unwrap();
    assert_eq!(ready.status(), 503);
    server.stop().await;
}

/// The binary itself: `migrate`, then `serve` until SIGTERM.
#[test]
fn the_binary_migrates_and_serves_until_terminated() {
    let dir = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = dir.path().join("server.toml");
    std::fs::write(
        &config,
        format!(
            "listen = \"127.0.0.1:{port}\"\npublic_origin = \"http://127.0.0.1:{port}\"\ndatabase_url = \"sqlite://{}\"\n",
            dir.path().join("db.sqlite").display()
        ),
    )
    .unwrap();
    let binary = env!("CARGO_BIN_EXE_ngteams-server");
    let migrated = std::process::Command::new(binary)
        .args(["--config", config.to_str().unwrap(), "migrate"])
        .status()
        .unwrap();
    assert!(migrated.success());
    let failed = std::process::Command::new(binary)
        .args(["--config", dir.path().join("missing.toml").to_str().unwrap(), "migrate"])
        .status()
        .unwrap();
    assert!(!failed.success());
    let mut serving = std::process::Command::new(binary)
        .args(["--config", config.to_str().unwrap(), "serve"])
        .env("NGTEAMS_SERVER_LOG", "warn")
        .spawn()
        .unwrap();
    let live = format!("http://127.0.0.1:{port}/health/live");
    let up = (0..100).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
    });
    assert!(up, "{live} never answered");
    let terminated = std::process::Command::new("kill")
        .args(["-TERM", &serving.id().to_string()])
        .status()
        .unwrap();
    assert!(terminated.success());
    assert!(serving.wait().unwrap().success());
}

#[tokio::test(flavor = "multi_thread")]
async fn serve_on_the_command_line_stops_when_told() {
    use clap::Parser as _;
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("server.toml");
    std::fs::write(
        &config,
        format!(
            "listen = \"127.0.0.1:0\"\ndatabase_url = \"sqlite://{}\"\n",
            dir.path().join("db.sqlite").display()
        ),
    )
    .unwrap();
    let cli = ngteams_server::cli::Cli::parse_from(["ngteams-server", "--config", config.to_str().unwrap(), "serve"]);
    ngteams_server::cli::run(cli, &mut "".as_bytes(), &mut Vec::new(), async {})
        .await
        .unwrap();
}
