//! Signing in with a password and refreshing per audience, as the client's session does.

use teams_auth::claims::TokenSummary;
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{Secret, SecretStore as _};
use teams_auth::token::{SPACES_RESOURCE, Scope, TokenError};
use teams_proto::{CSA_AUDIENCE, IC3_AUDIENCE, PRESENCE_AUDIENCE};

use crate::support::{Backend, CLIENT_ID, PASSWORD, TestServer, backends};

const UPN: &str = "ada@own.test";

fn claims(token: &str) -> serde_json::Value {
    use base64::Engine as _;
    let payload = token.split('.').nth(1).unwrap();
    serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_password_sign_in_answers_as_entra_with_tokens_for_the_asked_audience() {
    for backend in backends() {
        let server = TestServer::start(backend).await;
        let oid = server.add_user("Ada@Own.Test", "Ada Lovelace").await;
        let (status, answer) = server.password_grant(UPN, PASSWORD, &Scope::spaces()).await;
        assert_eq!(status, 200, "{backend:?}: {answer}");
        assert_eq!(answer["token_type"], "Bearer");
        assert!(answer["expires_in"].as_u64().unwrap() > 3000);
        assert!(answer["refresh_token"].as_str().is_some_and(|token| token.len() >= 40));
        let access = claims(answer["access_token"].as_str().unwrap());
        assert_eq!(access["aud"], SPACES_RESOURCE);
        assert_eq!(access["oid"], oid.as_str());
        assert_eq!(access["tid"], server.app.tenant.as_str());
        assert_eq!(access["upn"], UPN);
        assert_eq!(access["name"], "Ada Lovelace");
        let id = claims(answer["id_token"].as_str().unwrap());
        assert_eq!(id["aud"], CLIENT_ID);
        assert_eq!(id["preferred_username"], UPN);
        assert_eq!(id["oid"], oid.as_str());
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_session_refreshes_a_token_per_audience_and_keeps_the_rotated_refresh_token() {
    for backend in backends() {
        let server = TestServer::start(backend).await;
        server.add_user(UPN, "Ada").await;
        let session = server.session(UPN).await;
        let first = session.store().get(&refresh_token_key(UPN)).await.unwrap().unwrap();
        for audience in [IC3_AUDIENCE, CSA_AUDIENCE, PRESENCE_AUDIENCE, SPACES_RESOURCE] {
            let token = session.access_token(audience).await.unwrap();
            let summary = TokenSummary::read(&token.token).unwrap();
            assert_eq!(summary.audience, [audience], "{backend:?}");
            assert_eq!(summary.tenant.as_deref(), Some(server.app.tenant.as_str()));
        }
        let rotated = session.store().get(&refresh_token_key(UPN)).await.unwrap().unwrap();
        assert_ne!(rotated.expose(), first.expose());
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_password_or_an_unknown_account_is_an_invalid_grant() {
    let server = TestServer::start(Backend::Sqlite).await;
    server.add_user(UPN, "Ada").await;
    for (upn, password) in [(UPN, "not the password"), ("nobody@own.test", PASSWORD)] {
        let (status, answer) = server.password_grant(upn, password, &Scope::spaces()).await;
        assert_eq!(status, 400);
        assert_eq!(answer["error"], "invalid_grant");
        assert_eq!(answer["error_codes"][0], 50126);
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_failures_hold_the_account_off_with_retry_after() {
    let server = TestServer::start_with(Backend::Sqlite, |config| config.login.max_failures = 2).await;
    server.add_user(UPN, "Ada").await;
    for _ in 0..2 {
        assert_eq!(server.password_grant(UPN, "not the password", &Scope::spaces()).await.0, 400);
    }
    let response = reqwest::Client::new()
        .post(server.token_endpoint())
        .form(&[
            ("client_id", CLIENT_ID),
            ("grant_type", "password"),
            ("username", UPN),
            ("password", PASSWORD),
            ("scope", Scope::spaces().as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    assert!(response.headers().get("retry-after").is_some());
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resource_the_server_does_not_serve_is_refused() {
    let server = TestServer::start(Backend::Sqlite).await;
    server.add_user(UPN, "Ada").await;
    let session = server.session(UPN).await;
    let error = session.access_token("https://evil.example").await.unwrap_err();
    assert!(
        matches!(&error, TokenError::Entra { status: 400, error } if error.error == "invalid_scope"),
        "{error:?}"
    );
    assert!(session.signed_out().is_none());
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refresh_token_used_twice_signs_both_holders_out() {
    for backend in backends() {
        let server = TestServer::start_with(backend, |config| config.tokens.reuse_grace = 0).await;
        server.add_user(UPN, "Ada").await;
        let session = server.session(UPN).await;
        let stolen = session.store().get(&refresh_token_key(UPN)).await.unwrap().unwrap();
        session.access_token(IC3_AUDIENCE).await.unwrap();
        let thief = server.session(UPN).await;
        thief.store().put(&refresh_token_key(UPN), &stolen).await.unwrap();
        let error = thief.access_token(IC3_AUDIENCE).await.unwrap_err();
        assert!(matches!(error, TokenError::InvalidGrant(_)), "{backend:?}: {error:?}");
        let error = session.access_token(CSA_AUDIENCE).await.unwrap_err();
        assert!(matches!(error, TokenError::InvalidGrant(_)), "{backend:?}: {error:?}");
        assert!(session.signed_out().is_some());
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refresh_token_presented_again_within_the_grace_still_refreshes() {
    let server = TestServer::start(Backend::Sqlite).await;
    server.add_user(UPN, "Ada").await;
    let session = server.session(UPN).await;
    let earlier = session.store().get(&refresh_token_key(UPN)).await.unwrap().unwrap();
    session.access_token(IC3_AUDIENCE).await.unwrap();
    let retry = server.session(UPN).await;
    retry.store().put(&refresh_token_key(UPN), &earlier).await.unwrap();
    retry.access_token(IC3_AUDIENCE).await.unwrap();
    session.access_token(CSA_AUDIENCE).await.unwrap();
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_account_or_a_reset_password_signs_the_session_out() {
    for backend in backends() {
        let server = TestServer::start(backend).await;
        server.add_user(UPN, "Ada").await;
        server.add_user("bob@own.test", "Bob").await;
        let ada = server.session(UPN).await;
        let bob = server.session("bob@own.test").await;
        ngteams_server::admin::set_disabled(&server.app.db, UPN, true).await.unwrap();
        ngteams_server::admin::reset_password(&server.app.db, "bob@own.test", "a new password").await.unwrap();
        for session in [&ada, &bob] {
            let error = session.access_token(IC3_AUDIENCE).await.unwrap_err();
            assert!(matches!(error, TokenError::InvalidGrant(_)), "{backend:?}: {error:?}");
        }
        let (status, answer) = server.password_grant(UPN, PASSWORD, &Scope::spaces()).await;
        assert_eq!((status, answer["error_codes"][0].as_u64()), (400, Some(50057)));
        assert_eq!(server.password_grant("bob@own.test", PASSWORD, &Scope::spaces()).await.0, 400);
        assert_eq!(server.password_grant("bob@own.test", "a new password", &Scope::spaces()).await.0, 200);
        ngteams_server::admin::set_disabled(&server.app.db, UPN, false).await.unwrap();
        assert_eq!(server.password_grant(UPN, PASSWORD, &Scope::spaces()).await.0, 200);
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_garbage_refresh_token_is_an_invalid_grant() {
    let server = TestServer::start(Backend::Sqlite).await;
    server.add_user(UPN, "Ada").await;
    let session = server.session(UPN).await;
    session
        .store()
        .put(&refresh_token_key(UPN), &Secret::new("not-a-token"))
        .await
        .unwrap();
    let error = session.access_token(IC3_AUDIENCE).await.unwrap_err();
    assert!(matches!(error, TokenError::InvalidGrant(_)), "{error:?}");
    server.stop().await;
}
