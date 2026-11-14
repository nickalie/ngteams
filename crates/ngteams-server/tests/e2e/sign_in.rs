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
    serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .unwrap(),
    )
    .unwrap()
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
        assert_eq!(access["tid"], server.app.issuer.tenant());
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
            assert_eq!(summary.tenant.as_deref(), Some(server.app.issuer.tenant()));
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
        assert_eq!(
            server.password_grant(UPN, "not the password", &Scope::spaces()).await.0,
            400
        );
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
        ngteams_server::admin::set_disabled(&server.app.db, UPN, true)
            .await
            .unwrap();
        ngteams_server::admin::reset_password(&server.app.db, "bob@own.test", "a new password")
            .await
            .unwrap();
        for session in [&ada, &bob] {
            let error = session.access_token(IC3_AUDIENCE).await.unwrap_err();
            assert!(matches!(error, TokenError::InvalidGrant(_)), "{backend:?}: {error:?}");
        }
        let (status, answer) = server.password_grant(UPN, PASSWORD, &Scope::spaces()).await;
        assert_eq!((status, answer["error_codes"][0].as_u64()), (400, Some(50057)));
        assert_eq!(
            server
                .password_grant("bob@own.test", PASSWORD, &Scope::spaces())
                .await
                .0,
            400
        );
        assert_eq!(
            server
                .password_grant("bob@own.test", "a new password", &Scope::spaces())
                .await
                .0,
            200
        );
        ngteams_server::admin::set_disabled(&server.app.db, UPN, false)
            .await
            .unwrap();
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

async fn form(server: &TestServer, fields: &[(&str, &str)]) -> (u16, serde_json::Value) {
    let answer = reqwest::Client::new()
        .post(server.token_endpoint())
        .form(fields)
        .send()
        .await
        .unwrap();
    (answer.status().as_u16(), answer.json().await.unwrap_or_default())
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_token_requests_get_entras_errors() {
    let server = TestServer::start(Backend::Sqlite).await;
    server.add_user(UPN, "Ada").await;
    let spaces = Scope::spaces();
    let two = format!("{IC3_AUDIENCE}/.default {CSA_AUDIENCE}/.default");
    for (fields, error, code) in [
        (vec![("scope", spaces.as_str())], "invalid_request", 900_144),
        (vec![("client_id", CLIENT_ID)], "invalid_request", 900_144),
        (
            vec![("client_id", CLIENT_ID), ("scope", spaces.as_str())],
            "invalid_request",
            900_144,
        ),
        (
            vec![("client_id", CLIENT_ID), ("scope", "openid profile")],
            "invalid_scope",
            70_011,
        ),
        (
            vec![("client_id", CLIENT_ID), ("scope", two.as_str())],
            "invalid_scope",
            70_011,
        ),
        (
            vec![("client_id", CLIENT_ID), ("scope", "https://ic3.teams.office.com")],
            "invalid_scope",
            70_011,
        ),
        (
            vec![
                ("client_id", CLIENT_ID),
                ("scope", spaces.as_str()),
                ("grant_type", "client_credentials"),
            ],
            "unsupported_grant_type",
            70_003,
        ),
        (
            vec![
                ("client_id", CLIENT_ID),
                ("scope", spaces.as_str()),
                ("grant_type", "password"),
                ("username", UPN),
            ],
            "invalid_request",
            900_144,
        ),
        (
            vec![
                ("client_id", CLIENT_ID),
                ("scope", spaces.as_str()),
                ("grant_type", "refresh_token"),
            ],
            "invalid_request",
            900_144,
        ),
    ] {
        let (status, answer) = form(&server, &fields).await;
        assert_eq!((status, answer["error"].as_str()), (400, Some(error)), "{fields:?}");
        assert_eq!(answer["error_codes"][0], code);
        assert!(
            answer["error_description"]
                .as_str()
                .unwrap()
                .starts_with(&format!("AADSTS{code}: "))
        );
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refresh_token_is_bound_to_its_client_and_runs_out() {
    let server = TestServer::start_with(Backend::Sqlite, |config| config.tokens.refresh_lifetime = 1).await;
    server.add_user(UPN, "Ada").await;
    let (_, answer) = server.password_grant(UPN, PASSWORD, &Scope::spaces()).await;
    let refresh = answer["refresh_token"].as_str().unwrap().to_owned();
    let grant = |client: &'static str, token: String| {
        let scope = Scope::spaces();
        let server = &server;
        async move {
            form(
                server,
                &[
                    ("client_id", client),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", token.as_str()),
                    ("scope", scope.as_str()),
                ],
            )
            .await
        }
    };
    let (status, answer) = grant("another-client", refresh.clone()).await;
    assert_eq!((status, answer["error_codes"][0].as_u64()), (400, Some(70_000)));
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    let (status, answer) = grant(CLIENT_ID, refresh).await;
    assert_eq!((status, answer["error_codes"][0].as_u64()), (400, Some(700_082)));
    ngteams_server::app::tidy(&server.app).await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_password_guesses_reserve_the_login_limit() {
    for redis in [None].into_iter().chain(crate::support::redis_url().map(Some)) {
        let server = TestServer::start_with(Backend::Sqlite, |config| {
            config.redis_url = redis;
            config.login.max_failures = 2;
        })
        .await;
        let upn = format!("{}@own.test", uuid::Uuid::new_v4().simple());
        server.add_user(&upn, "Ada").await;
        let scope = Scope::spaces();
        let attempts = (0..12).map(|_| server.password_grant(&upn, "wrong password", &scope));
        let answers = futures_util::future::join_all(attempts).await;
        assert_eq!(answers.iter().filter(|(status, _)| *status == 400).count(), 2);
        assert_eq!(answers.iter().filter(|(status, _)| *status == 429).count(), 10);
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_successor_inserted_after_revocation_cannot_refresh() {
    use base64::Engine as _;
    use ngteams_server::db::{refresh, users};
    use sha2::{Digest as _, Sha256};
    for backend in backends() {
        for family_only in [true, false] {
            let server = TestServer::start(backend).await;
            let oid = server.add_user(UPN, "Ada").await;
            let session = server.session(UPN).await;
            let original = session.store().get(&refresh_token_key(UPN)).await.unwrap().unwrap();
            let hash = |token: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(token));
            let mut successor = refresh::by_hash(&server.app.db, &hash(original.expose()))
                .await
                .unwrap()
                .unwrap();
            refresh::mark_used(&server.app.db, &successor.id).await.unwrap();
            if family_only {
                refresh::revoke_family(&server.app.db, &successor.family_id)
                    .await
                    .unwrap();
            } else {
                users::set_password(&server.app.db, UPN, "replacement hash")
                    .await
                    .unwrap();
            }
            successor.id = uuid::Uuid::new_v4().to_string();
            successor.token_hash = hash("delayed-successor");
            successor.used_at = None;
            let _ = refresh::insert(&server.app.db, &successor).await.unwrap();
            session
                .store()
                .put(&refresh_token_key(UPN), &Secret::new("delayed-successor"))
                .await
                .unwrap();
            let answer = session.access_token(IC3_AUDIENCE).await;
            assert!(
                matches!(answer, Err(TokenError::InvalidGrant(_))),
                "{backend:?}, family={family_only}, user={oid}"
            );
            server.stop().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn refresh_purge_removes_only_tokens_before_the_cutoff() {
    use ngteams_server::db::{now, refresh};
    for backend in backends() {
        let server = TestServer::start(backend).await;
        let oid = server.add_user(UPN, "Ada").await;
        let cutoff = now();
        for (id, expiry) in [("expired", cutoff - 1), ("boundary", cutoff), ("live", cutoff + 3600)] {
            refresh::insert(
                &server.app.db,
                &refresh::RefreshToken {
                    id: id.into(),
                    family_id: id.into(),
                    user_id: oid.clone(),
                    token_hash: id.into(),
                    client_id: CLIENT_ID.into(),
                    created_at: cutoff - 100,
                    expires_at: expiry,
                    used_at: None,
                    revoked_at: None,
                },
            )
            .await
            .unwrap();
        }
        assert_eq!(refresh::purge_expired(&server.app.db, cutoff).await.unwrap(), 1);
        assert!(refresh::by_hash(&server.app.db, "expired").await.unwrap().is_none());
        for hash in ["boundary", "live"] {
            assert!(refresh::by_hash(&server.app.db, hash).await.unwrap().is_some());
        }
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn revocation_and_successor_insertion_serialize_on_each_backend() {
    use ngteams_server::db::{now, refresh};
    for backend in backends() {
        let server = TestServer::start(backend).await;
        let oid = server.add_user(UPN, "Ada").await;
        for family_only in [true, false] {
            for _ in 0..6 {
                let mut token = refresh::RefreshToken {
                    id: uuid::Uuid::new_v4().to_string(),
                    family_id: uuid::Uuid::new_v4().to_string(),
                    user_id: oid.clone(),
                    token_hash: uuid::Uuid::new_v4().to_string(),
                    client_id: CLIENT_ID.into(),
                    created_at: now(),
                    expires_at: now() + 3600,
                    used_at: None,
                    revoked_at: None,
                };
                assert!(refresh::insert(&server.app.db, &token).await.unwrap());
                token.id = uuid::Uuid::new_v4().to_string();
                token.token_hash = uuid::Uuid::new_v4().to_string();
                let (inserted, revoked) = tokio::join!(
                    refresh::insert(&server.app.db, &token),
                    revoke_for_race(&server.app.db, &token, family_only)
                );
                inserted.unwrap();
                revoked.unwrap();
                let row = refresh::by_hash(&server.app.db, &token.token_hash).await.unwrap();
                assert!(row.is_none_or(|row| row.revoked_at.is_some()));
            }
        }
        server.stop().await;
    }
}

async fn revoke_for_race(
    db: &ngteams_server::db::Db,
    token: &ngteams_server::db::refresh::RefreshToken,
    family_only: bool,
) -> Result<(), ngteams_server::db::DbError> {
    if family_only {
        ngteams_server::db::refresh::revoke_family(db, &token.family_id).await
    } else {
        ngteams_server::db::refresh::revoke_user(db, &token.user_id).await
    }
}
