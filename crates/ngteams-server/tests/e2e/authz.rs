//! The skypetoken exchange and the region it hands out, as the client's session and account bring-up read them.

use std::time::SystemTime;

use teams_auth::authz::{AuthzError, AuthzKind, CHAT_BASE_GTM};
use teams_auth::secrets::MemorySecretStore;
use teams_auth::token::{SPACES_RESOURCE, TokenError};
use teams_core::account::{self, Account, AccountError};
use teams_proto::IC3_AUDIENCE;

use crate::support::{Backend, TestServer, backends};

const UPN: &str = "ada@own.test";

#[tokio::test(flavor = "multi_thread")]
async fn the_session_gets_a_skypetoken_with_the_region_and_the_servers_own_bases() {
    for backend in backends() {
        let server = TestServer::start(backend).await;
        let oid = server.add_user(UPN, "Ada").await;
        let session = server.session(UPN).await;
        let skypetoken = session.skypetoken().await.unwrap();
        assert_eq!(
            skypetoken.skype_id.as_deref(),
            Some(format!("orgid:{oid}").as_str()),
            "{backend:?}"
        );
        assert!(skypetoken.expires_at > SystemTime::now());
        let spaces = session.access_token(SPACES_RESOURCE).await.unwrap();
        assert!(skypetoken.expires_at <= spaces.expires_at);
        let region = session.region();
        assert_eq!(region.name.as_deref(), Some("own"));
        assert_eq!(region.partition.as_deref(), Some("own01"));
        assert_eq!(
            session.service_base("authService").as_deref(),
            Some(format!("{}/api/authsvc", server.origin).as_str())
        );
        assert!(region.endpoints.values().all(|base| base.starts_with(&server.origin)));
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_account_bring_up_reads_its_bases_from_the_servers_authz_and_keeps_absent_services_off() {
    let server = TestServer::start(Backend::Sqlite).await;
    server.add_user(UPN, "Ada").await;
    let session = server.session(UPN).await;
    session.skypetoken().await.unwrap();
    let region = session.region();
    for gtm in [
        CHAT_BASE_GTM,
        account::MIDDLE_TIER_GTM,
        account::CSA_GTM,
        account::PRESENCE_GTM,
        account::AMS_GTM,
        account::PROVISIONING_GTM,
        account::USER_PROFILE_GTM,
        account::MCPS_GTM,
    ] {
        assert_eq!(region.base(gtm), None, "{gtm} isn't served yet");
    }
    let dir = tempfile::tempdir().unwrap();
    let mri = format!("8:{}", session.skypetoken().await.unwrap().skype_id.unwrap());
    let started = account::start(
        Account {
            upn: UPN.into(),
            mri: mri.clone(),
            display_name: "Ada".into(),
            store_file: dir.path().join("account.sqlite"),
            region,
            secrets: MemorySecretStore::new(),
            push: false,
            known_names: vec![(mri, "Ada".into())],
            calls: None,
            read_only: false,
        },
        teams_core::CallFloor::new(),
    );
    assert!(matches!(started, Err(AccountError::NoChatBase)));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn authz_refuses_a_token_for_another_audience_or_none() {
    let server = TestServer::start(Backend::Sqlite).await;
    server.add_user(UPN, "Ada").await;
    let session = server.session(UPN).await;
    let ic3 = session.access_token(IC3_AUDIENCE).await.unwrap();
    let error = server
        .authz_client()
        .exchange(&ic3.token, AuthzKind::ExplicitLogin)
        .await
        .unwrap_err();
    assert!(matches!(error, AuthzError::Unauthorized { .. }), "{error:?}");
    let error = server
        .authz_client()
        .exchange(&teams_auth::secrets::Secret::new("a.b.c"), AuthzKind::ExplicitLogin)
        .await
        .unwrap_err();
    assert!(matches!(error, AuthzError::Unauthorized { .. }), "{error:?}");
    let answer = reqwest::Client::new()
        .post(server.authz_endpoint())
        .send()
        .await
        .unwrap();
    assert_eq!(answer.status(), 401);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn authz_refuses_a_disabled_account_as_teams_refuses_an_unlicensed_one() {
    let server = TestServer::start(Backend::Sqlite).await;
    server.add_user(UPN, "Ada").await;
    let session = server.session(UPN).await;
    let spaces = session.access_token(SPACES_RESOURCE).await.unwrap();
    ngteams_server::admin::set_disabled(&server.app.db, UPN, true)
        .await
        .unwrap();
    let error = server
        .authz_client()
        .exchange(&spaces.token, AuthzKind::ExplicitLogin)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, AuthzError::Refused { status: 403, code: Some(code) } if code == "UserNotFoundInSkypeTeam"),
        "{error:?}"
    );
    let error = session.skypetoken().await.unwrap_err();
    assert!(
        matches!(error, TokenError::InvalidGrant(_) | TokenError::Authz(_)),
        "{error:?}"
    );
    server.stop().await;
}
