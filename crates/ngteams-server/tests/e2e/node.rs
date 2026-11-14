//! The node itself: health, readiness, TLS and configuration.

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
    let answer = http.get(format!("https://localhost:{port}/health/live")).send().await.unwrap();
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
fn the_configuration_layers_a_file_over_the_defaults_and_refuses_bad_values() {
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
    let refused = |change: fn(&mut Config)| {
        let mut config = Config::default();
        change(&mut config);
        config.check().unwrap_err()
    };
    for origin in ["http://teams.example.com", "https://teams.example.com/path", "https://u@teams.example.com"] {
        let mut config = Config::default();
        config.public_origin = url::Url::parse(origin).unwrap();
        assert!(matches!(config.check(), Err(ConfigError::PublicOrigin)), "{origin}");
    }
    for origin in ["http://localhost:1", "http://[::1]:1", "https://teams.example.com"] {
        let mut config = Config::default();
        config.public_origin = url::Url::parse(origin).unwrap();
        assert!(config.check().is_ok(), "{origin}");
    }
    assert!(matches!(refused(|c| c.region = "Bad Region".into()), ConfigError::Region));
    assert!(matches!(refused(|c| c.partition = String::new()), ConfigError::Region));
    assert!(matches!(refused(|c| c.database_url = "mysql://x".into()), ConfigError::Database));
    assert!(matches!(refused(|c| c.tokens.access_lifetime = 0), ConfigError::Lifetime));
    assert!(matches!(refused(|c| c.login.window = 0), ConfigError::Lifetime));
}
