//! Operator commands and native client sessions against real server processes; all accounts are synthetic.

mod support;

use std::time::Duration;

use serde_json::Value;
use teams_auth::authz::{AuthzError, AuthzKind};
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore as _};
use teams_auth::session::{AccessToken, Session};
use teams_auth::token::{SPACES_RESOURCE, TokenError};
use teams_proto::IC3_AUDIENCE;

use self::support::{Deployment, LOGIN_WINDOW_SECONDS, Node, tls_parent};
use crate::support::{Backend, PASSWORD, PG_ENV, backends, redis_url};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operator_and_native_client_over_http() {
    for backend in backends() {
        operator_flow(backend, false).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operator_and_native_client_over_tls() {
    if tls_parent("binary::operator_and_native_client_over_tls") {
        return;
    }
    for backend in backends() {
        operator_flow(backend, true).await;
    }
}

async fn operator_flow(backend: Backend, tls: bool) {
    eprintln!("operator: {backend:?}, tls={tls}");
    let deployment = Deployment::new(backend, tls, redis_url()).await;
    let mut node = deployment.node("a");
    node.cli(&["migrate"], "");
    node.cli(&["migrate"], "");
    let upn = account();
    node.add_user(&upn, PASSWORD);
    let listed = node.cli(&["user", "list"], "");
    assert!(listed.starts_with(&format!("{upn}\t")));
    assert!(listed.contains("\tenabled\tQA Operator"));
    assert!(!listed.contains(PASSWORD));
    node.start().await;
    check_native_client(&node, &upn, &deployment.origin).await;
    if tls {
        rejects_an_untrusted_certificate(&node).await;
    }
    node.stop().await;
}

async fn check_native_client(node: &Node, upn: &str, origin: &str) {
    let session = node.sign_in(upn, PASSWORD).await;
    let before = session.store().get(&refresh_token_key(upn)).await.unwrap().unwrap();
    let access = session.access_token(SPACES_RESOURCE).await.unwrap();
    let after = session.store().get(&refresh_token_key(upn)).await.unwrap().unwrap();
    assert!(
        before.expose() != after.expose(),
        "refresh must rotate without printing either token"
    );
    session.access_token(IC3_AUDIENCE).await.unwrap();
    check_region(&session, origin).await;
    node.authz()
        .exchange(&access.token, AuthzKind::ExplicitLogin)
        .await
        .unwrap();
}

async fn check_region(session: &Session<MemorySecretStore>, origin: &str) {
    let skype = session.skypetoken().await.unwrap();
    let access = session.access_token(SPACES_RESOURCE).await.unwrap();
    assert!(skype.skype_id.as_deref().unwrap().starts_with("orgid:"));
    assert!(skype.expires_at <= access.expires_at);
    assert_eq!(session.region().name.as_deref(), Some("own"));
    assert_eq!(session.region().partition.as_deref(), Some("own01"));
    assert_eq!(
        session.service_base("authService"),
        Some(format!("{origin}/api/authsvc"))
    );
    assert_eq!(
        session.service_base("chatServiceAfd"),
        Some(format!("{origin}/api/chatsvc/own"))
    );
    assert_eq!(session.region().endpoints.len(), 2);
}

async fn rejects_an_untrusted_certificate(node: &Node) {
    let unrelated = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let root = reqwest::Certificate::from_pem(unrelated.cert.pem().as_bytes()).unwrap();
    let http = reqwest::Client::builder()
        .no_proxy()
        .tls_certs_only([root])
        .build()
        .unwrap();
    let error = http
        .get(format!("{}/health/live", node.endpoint))
        .send()
        .await
        .unwrap_err();
    assert!(
        error.is_connect(),
        "an unrelated CA must not trust the server certificate"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn password_controls_over_http() {
    for backend in backends() {
        password_flow(backend, false).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn password_controls_over_tls() {
    if tls_parent("binary::password_controls_over_tls") {
        return;
    }
    for backend in backends() {
        password_flow(backend, true).await;
    }
}

async fn password_flow(backend: Backend, tls: bool) {
    eprintln!("password controls: {backend:?}, tls={tls}");
    let deployment = Deployment::new(backend, tls, redis_url()).await;
    let mut node = deployment.node("a");
    let upn = account();
    node.cli(&["migrate"], "");
    node.add_user(&upn, PASSWORD);
    node.start().await;
    for (user, password) in [(&upn, "wrong password"), (&account(), PASSWORD)] {
        invalid_password(&node, user, password, 50126).await;
    }
    disable_and_enable(&node, &upn).await;
    reset_password(&node, &upn).await;
    lockout_flow(&node).await;
    node.stop().await;
}

async fn disable_and_enable(node: &Node, upn: &str) {
    let session = node.sign_in(upn, PASSWORD).await;
    let access = session.access_token(SPACES_RESOURCE).await.unwrap();
    node.cli(&["user", "disable", upn], "");
    assert!(node.cli(&["user", "list"], "").contains("\tdisabled\t"));
    invalid_password(node, upn, PASSWORD, 50057).await;
    assert!(matches!(
        session.access_token(IC3_AUDIENCE).await,
        Err(TokenError::InvalidGrant(_))
    ));
    assert!(
        matches!(node.authz().exchange(&access.token, AuthzKind::TokenRefresh).await,
        Err(AuthzError::Refused { status: 403, code: Some(code) }) if code == "UserNotFoundInSkypeTeam")
    );
    node.cli(&["user", "enable", upn], "");
}

async fn reset_password(node: &Node, upn: &str) {
    let session = node.sign_in(upn, PASSWORD).await;
    node.cli(
        &["user", "reset-password", upn],
        "replacement password\r\nignored second line\n",
    );
    assert!(matches!(
        session.access_token(IC3_AUDIENCE).await,
        Err(TokenError::InvalidGrant(_))
    ));
    invalid_password(node, upn, PASSWORD, 50126).await;
    node.sign_in(upn, "replacement password")
        .await
        .skypetoken()
        .await
        .unwrap();
}

async fn invalid_password(node: &Node, upn: &str, password: &str, code: u64) {
    let response = node.password(upn, password).await;
    assert_eq!(response.status(), 400);
    let answer: Value = response.json().await.unwrap();
    assert_eq!(answer["error"], "invalid_grant");
    assert_eq!(answer["error_codes"][0], code);
}

async fn lockout_flow(node: &Node) {
    let upn = account();
    node.add_user(&upn, PASSWORD);
    for _ in 0..3 {
        invalid_password(node, &upn, "wrong password", 50126).await;
    }
    let response = node.password(&upn, PASSWORD).await;
    assert_eq!(response.status(), 429);
    let retry = response.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!((1..=LOGIN_WINDOW_SECONDS).contains(&retry));
    let answer: Value = response.json().await.unwrap();
    assert_eq!(answer["error"], "temporarily_unavailable");
    tokio::time::sleep(Duration::from_secs(retry + 1)).await;
    node.sign_in(&upn, PASSWORD).await.skypetoken().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_nodes_share_refresh_keys_and_login_holds_over_http() {
    shared_flow(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_nodes_share_refresh_keys_and_login_holds_over_tls() {
    if tls_parent("binary::two_nodes_share_refresh_keys_and_login_holds_over_tls") {
        return;
    }
    shared_flow(true).await;
}

async fn shared_flow(tls: bool) {
    if std::env::var_os(PG_ENV).is_none() || redis_url().is_none() {
        eprintln!("SKIP shared binary nodes: set {PG_ENV} and NGTEAMS_SERVER_REDIS_URL");
        return;
    }
    eprintln!("two binary nodes: Postgres + Redis, tls={tls}");
    let deployment = Deployment::new(Backend::Postgres, tls, redis_url()).await;
    let mut a = deployment.node("a");
    let mut b = deployment.node("b");
    a.cli(&["migrate"], "");
    let upn = account();
    a.add_user(&upn, PASSWORD);
    tokio::join!(a.start(), b.start());
    let old = refresh_on_another_node(&a, &b, &upn).await;
    rotate_shared_keys(&a, &mut b, &upn, &old).await;
    shared_hold(&a, &b).await;
    b.stop().await;
    a.stop().await;
}

async fn refresh_on_another_node(a: &Node, b: &Node, upn: &str) -> Secret {
    let on_a = a.sign_in(upn, PASSWORD).await;
    let old = on_a.access_token(SPACES_RESOURCE).await.unwrap();
    let refresh = on_a.store().get(&refresh_token_key(upn)).await.unwrap().unwrap();
    let store = MemorySecretStore::new();
    store.put(&refresh_token_key(upn), &refresh).await.unwrap();
    let on_b = b.session(upn, store);
    let other = on_b.access_token(IC3_AUDIENCE).await.unwrap();
    assert_eq!(
        teams_auth::claims::TokenSummary::read(&other.token).unwrap().audience,
        [IC3_AUDIENCE]
    );
    let rotated_refresh = on_b.store().get(&refresh_token_key(upn)).await.unwrap().unwrap();
    assert!(
        refresh.expose() != rotated_refresh.expose(),
        "cross-node refresh must rotate"
    );
    on_b.skypetoken().await.unwrap();
    old.token
}

async fn rotate_shared_keys(a: &Node, b: &mut Node, upn: &str, old: &Secret) {
    let rotated = a.cli(&["keys", "rotate"], "");
    for node in [a, &*b] {
        node.authz().exchange(old, AuthzKind::TokenRefresh).await.unwrap();
    }
    b.stop().await;
    b.start().await;
    let new = b
        .sign_in(upn, PASSWORD)
        .await
        .access_token(SPACES_RESOURCE)
        .await
        .unwrap();
    assert_ne!(kid(old), kid(&new.token));
    assert!(rotated.starts_with(&format!("created key {};", kid(&new.token))));
    for node in [a, &*b] {
        check_rotated_key(node, upn, old, &new.token).await;
    }
}

async fn check_rotated_key(node: &Node, upn: &str, old: &Secret, new: &Secret) {
    for token in [old, new] {
        node.authz().exchange(token, AuthzKind::TokenRefresh).await.unwrap();
    }
    let issued = node
        .sign_in(upn, PASSWORD)
        .await
        .access_token(SPACES_RESOURCE)
        .await
        .unwrap();
    assert_eq!(kid(&issued.token), kid(new));
}

async fn shared_hold(a: &Node, b: &Node) {
    let upn = account();
    a.add_user(&upn, PASSWORD);
    for node in [a, b, a] {
        invalid_password(node, &upn, "wrong password", 50126).await;
    }
    for node in [a, b] {
        let response = node.password(&upn, PASSWORD).await;
        assert_eq!(response.status(), 429);
        assert!(
            response.headers()["retry-after"]
                .to_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                > 0
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_persists_accounts_keys_and_readiness_tracks_database_over_http() {
    for backend in backends() {
        restart_flow(backend, false).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_persists_accounts_keys_and_readiness_tracks_database_over_tls() {
    if tls_parent("binary::restart_persists_accounts_keys_and_readiness_tracks_database_over_tls") {
        return;
    }
    for backend in backends() {
        restart_flow(backend, true).await;
    }
}

async fn restart_flow(backend: Backend, tls: bool) {
    eprintln!("restart and health: {backend:?}, tls={tls}");
    let deployment = Deployment::new(backend, tls, redis_url()).await;
    let mut node = deployment.node("a");
    let upn = account();
    node.cli(&["migrate"], "");
    node.add_user(&upn, PASSWORD);
    node.start().await;
    let session = node.sign_in(&upn, PASSWORD).await;
    let old = session.access_token(SPACES_RESOURCE).await.unwrap();
    let skype = session.skypetoken().await.unwrap();
    restart_node(&mut node).await;
    check_persisted_session(&node, &upn, &session, &old, skype.skype_id).await;
    if backend == Backend::Postgres {
        database_outage(&deployment, &node).await;
    }
    node.stop().await;
}

async fn restart_node(node: &mut Node) {
    node.stop().await;
    assert!(node.status("/health/live").await.is_none());
    node.start().await;
    assert_eq!(node.status("/health/live").await, Some(204));
    assert_eq!(node.status("/health/ready").await, Some(204));
}

async fn check_persisted_session(
    node: &Node,
    upn: &str,
    session: &Session<MemorySecretStore>,
    old: &AccessToken,
    skype_id: Option<String>,
) {
    let verified = node
        .authz()
        .exchange(&old.token, AuthzKind::TokenRefresh)
        .await
        .unwrap();
    assert_eq!(verified.skype_id, skype_id);
    let fresh = session.access_token(IC3_AUDIENCE).await.unwrap();
    assert_eq!(kid(&fresh.token), kid(&old.token));
    let signed_in = node.sign_in(upn, PASSWORD).await;
    assert_eq!(signed_in.skypetoken().await.unwrap().skype_id, skype_id);
}

async fn database_outage(deployment: &Deployment, node: &Node) {
    use diesel_async::{AsyncConnection as _, AsyncPgConnection, SimpleAsyncConnection as _};
    let url = url::Url::parse(&deployment.database).unwrap();
    let name = url.path().trim_start_matches('/');
    let mut admin = AsyncPgConnection::establish(&std::env::var(PG_ENV).unwrap())
        .await
        .unwrap();
    admin
        .batch_execute(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .await
        .unwrap();
    assert_eq!(node.status("/health/ready").await, Some(503));
    assert_eq!(node.status("/health/live").await, Some(204));
}

fn kid(token: &Secret) -> String {
    jsonwebtoken::decode_header(token.expose()).unwrap().kid.unwrap()
}

fn account() -> String {
    format!("qa-{}@own.test", uuid::Uuid::new_v4().simple())
}
