//! The sign-in steps (`teams_app::signin`) over the account files a host gives: a Microsoft 365 account signs in
//! through a window that answers the redirect by itself, the code redeemed at a local token endpoint and the
//! skypetoken at a local authz (wiremock); an own server's account signs in with its password through a local TLS
//! front with a made-up certificate authority. Each says what the user is told when the sign-in stored nothing. Every
//! directory is a test's own; every value is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rustls::ServerConfig;
use rustls::pki_types::PrivateKeyDer;
use serde_json::{Value, json};
use teams_app::accounts::{AccountFiles, OwnServer, PersonalAccount, PersonalPaths};
use teams_app::signin::{
    self, ACCOUNT_TURNED_OFF, PASSWORD_MUST_CHANGE, SignInClients, SignInError, untrusted_certificate,
};
use teams_auth::authz::{AuthzClient, skypetoken_key};
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore as _};
use teams_auth::signin::{SignInAnswer, SignInRequest, WindowError, WindowSignInError};
use teams_auth::token::{EntraConfig, NATIVE_CLIENT_REDIRECT, TEAMS_NATIVE_CLIENT_ID, TokenClient};
use wiremock::matchers::{body_string_contains, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const UPN: &str = "someone@example.test";
const OID: &str = "00000000-0000-4000-8000-0000000000ff";
const OTHER_OID: &str = "00000000-0000-4000-8000-0000000000ee";
const TENANT: &str = "00000000-0000-4000-8000-000000000001";
const DISPLAY_NAME: &str = "Made-Up Person";
const AUTHZ_FIXTURE: &str = include_str!("../../../tests/fixtures/auth/authz-ok.json");

fn files_in(dir: &tempfile::TempDir) -> AccountFiles {
    AccountFiles::new(PersonalPaths::new(
        dir.path().join("config/ngteams"),
        dir.path().join("share/ngteams"),
    ))
}

/// An unsigned ID token naming `upn` and `oid` in the tenant, for the native client.
fn id_token(upn: &str, oid: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"RS256","kid":"test"}"#);
    let claims = json!({
        "aud": TEAMS_NATIVE_CLIENT_ID,
        "oid": oid,
        "tid": TENANT,
        "preferred_username": upn,
        "name": DISPLAY_NAME,
    });
    format!(
        "{header}.{}.c2lnbmF0dXJlLW5vdC1jaGVja2Vk",
        URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}

fn tokens(upn: &str, oid: &str) -> Value {
    json!({
        "token_type": "Bearer",
        "scope": "https://api.spaces.skype.com/.default",
        "expires_in": 3600,
        "access_token": "made-up-access-token-0123456789",
        "refresh_token": "made-up-refresh-token",
        "id_token": id_token(upn, oid),
    })
}

/// Mounts an authz that gives a made-up skypetoken and the fixture's region.
async fn mount_authz(server: &MockServer) {
    let fixture: Value = serde_json::from_str(AUTHZ_FIXTURE).unwrap();
    let mut authz = fixture["response"]["body"].clone();
    authz["tokens"]["skypeToken"] = json!("made-up-skypetoken-0123456789");
    Mock::given(method("POST"))
        .and(path("/api/authsvc/v1.0/authz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(authz))
        .mount(server)
        .await;
}

/// A local Entra token endpoint whose answers name `upn` and `oid`, a local authz, and the clients for them.
async fn clients(server: &MockServer, upn: &str, oid: &str) -> SignInClients {
    Mock::given(method("POST"))
        .and(path("/organizations/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens(upn, oid)))
        .mount(server)
        .await;
    mount_authz(server).await;
    let entra = EntraConfig::teams_native().with_authority(format!("{}/organizations/", server.uri()));
    SignInClients {
        token: TokenClient::new(entra.clone()).unwrap(),
        authz: AuthzClient::with_endpoint(&format!("{}/api/authsvc/v1.0/authz", server.uri())).unwrap(),
        entra,
    }
}

/// Stands in for the sign-in window: it answers the redirect to the request, its `state` taken from the request.
fn redirect() -> impl FnOnce(SignInRequest) -> SignInAnswer + Send + 'static {
    |request| redirect_to(&request)
}

fn redirect_to(request: &SignInRequest) -> SignInAnswer {
    let state = request
        .authorize
        .split(['?', '&'])
        .find_map(|pair| pair.strip_prefix("state="))
        .unwrap_or_default()
        .to_owned();
    SignInAnswer::Redirect(Secret::new(format!(
        "{NATIVE_CLIENT_REDIRECT}?code=made-up-code&state={state}"
    )))
}

#[tokio::test]
async fn a_first_sign_in_stores_the_token_names_the_account_and_saves_its_region() {
    let dir = tempfile::tempdir().unwrap();
    let files = files_in(&dir);
    let server = MockServer::start().await;
    let here = clients(&server, UPN, OID).await;
    let store = MemorySecretStore::new();
    let account = files.ask_for(UPN).unwrap();
    assert!(!signin::ready(&files, &account, &store).await.unwrap());

    let signed = signin::sign_in(&files, &account, &here, &store, redirect())
        .await
        .unwrap();
    assert_eq!(signed.oid.as_deref(), Some(OID));
    assert_eq!(signed.name.as_deref(), Some(DISPLAY_NAME));
    assert_eq!(files.signed_in_accounts().unwrap(), std::slice::from_ref(&signed));
    assert!(files.paths.region_file(&signed).unwrap().is_file());
    assert!(store.get(&refresh_token_key(UPN)).await.unwrap().is_some());
    assert!(store.get(&skypetoken_key(UPN)).await.unwrap().is_some());
    assert!(signin::ready(&files, &signed, &store).await.unwrap());

    // a later sign-in that lands in another account stores nothing
    let other = MockServer::start().await;
    let elsewhere = clients(&other, UPN, OTHER_OID).await;
    let landed = MemorySecretStore::new();
    let refused = signin::sign_in(&files, &signed, &elsewhere, &landed, redirect()).await;
    assert!(matches!(refused, Err(SignInError::Window(_))), "{refused:?}");
    assert!(landed.get(&refresh_token_key(UPN)).await.unwrap().is_none());
}

#[tokio::test]
async fn an_account_another_entry_already_is_has_its_token_deleted_again() {
    let dir = tempfile::tempdir().unwrap();
    let files = files_in(&dir);
    let store = MemorySecretStore::new();
    let first = MockServer::start().await;
    let account = files.ask_for(UPN).unwrap();
    signin::sign_in(&files, &account, &clients(&first, UPN, OID).await, &store, redirect())
        .await
        .unwrap();
    let second = MockServer::start().await;
    let again = signin::add(
        &files,
        "alias@example.test",
        &clients(&second, "alias@example.test", OID).await,
        &store,
        |_| Ok(redirect()),
    )
    .await;
    assert!(again.is_err_and(|said| said.contains("given twice")));
    assert!(
        store
            .get(&refresh_token_key("alias@example.test"))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(files.accounts().unwrap().len(), 1, "the failed entry is taken out");
}

#[tokio::test]
async fn an_added_account_whose_sign_in_stored_nothing_goes_unless_the_window_refused_a_host() {
    let dir = tempfile::tempdir().unwrap();
    let files = files_in(&dir);
    let server = MockServer::start().await;
    let clients = clients(&server, UPN, OID).await;
    let store = MemorySecretStore::new();

    let closed = signin::add(&files, UPN, &clients, &store, |_| Ok(|_| SignInAnswer::Closed)).await;
    assert!(closed.is_err_and(|said| said.starts_with("the sign-in stored nothing")));
    assert!(files.accounts().unwrap().is_empty());

    let no_window = signin::add(&files, UPN, &clients, &store, |_| {
        Err::<fn(SignInRequest) -> SignInAnswer, _>(SignInError::NoWindow(std::io::Error::other("no page")))
    })
    .await;
    assert!(no_window.is_err_and(|said| said.contains("no page")));
    assert!(files.accounts().unwrap().is_empty());

    let refused = signin::add(&files, UPN, &clients, &store, |_| {
        Ok(|_| SignInAnswer::Refused("sso.example.test".to_owned()))
    })
    .await
    .unwrap_err();
    assert!(refused.contains("refused sso.example.test"), "{refused}");
    assert!(refused.contains("idp_hosts = [\"sso.example.test\"]"), "{refused}");
    assert!(refused.contains(&files.paths.accounts_file().display().to_string()));
    assert_eq!(files.pending_upns().unwrap(), [UPN]);
    let error = SignInError::Window(WindowSignInError::Window(WindowError::Refused(
        "sso.example.test".into(),
    )));
    assert_eq!(signin::refused_host(&error), Some("sso.example.test"));
}

#[tokio::test]
async fn each_account_signs_in_only_its_own_way() {
    let dir = tempfile::tempdir().unwrap();
    let files = files_in(&dir);
    let store = MemorySecretStore::new();
    let server = MockServer::start().await;
    let clients = clients(&server, UPN, OID).await;
    let own = OwnServer::parse("chat.example.test", "").unwrap();
    let own_account = PersonalAccount::pending_on(UPN, &own).unwrap();
    let microsoft = PersonalAccount::pending(UPN).unwrap();
    let password = Secret::new("made-up password");
    assert!(matches!(
        signin::sign_in(&files, &own_account, &clients, &store, redirect()).await,
        Err(SignInError::WrongCloud)
    ));
    assert!(matches!(
        signin::sign_in_own(&files, &microsoft, &password, &store).await,
        Err(SignInError::WrongCloud)
    ));
    assert!(matches!(
        signin::region_again(&files, &microsoft, &store).await,
        Err(SignInError::WrongCloud)
    ));
    assert!(SignInClients::live().is_ok());
    assert!(SignInClients::for_cloud(&own.cloud().unwrap()).is_ok());
    assert!(!untrusted_certificate(&SignInError::WrongCloud));
}

/// A made-up certificate authority and its PEM.
struct Authority {
    pem: String,
    issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
}

fn authority() -> Authority {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(Vec::new()).unwrap();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "made-up certificate authority");
    let pem = params.self_signed(&key).unwrap().pem();
    Authority {
        pem,
        issuer: rcgen::Issuer::new(params, key),
    }
}

/// A TLS front on loopback for `localhost`, its certificate signed by `authority`, that hands each connection to
/// `backend` (a wiremock server); its origin.
async fn tls_front(authority: &Authority, backend: &MockServer) -> String {
    let key = rcgen::KeyPair::generate().unwrap();
    let leaf = rcgen::CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
    let cert = leaf.signed_by(&key, &authority.issuer).unwrap();
    let mut config = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            PrivateKeyDer::try_from(key.serialize_der()).unwrap(),
        )
        .unwrap();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let backend = *backend.address();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(stream).await else {
                    return;
                };
                let Ok(mut plain) = tokio::net::TcpStream::connect(backend).await else {
                    return;
                };
                let _ = tokio::io::copy_bidirectional(&mut tls, &mut plain).await;
            });
        }
    });
    format!("https://localhost:{port}")
}

/// An own server's token endpoint: the password `made-up password` signs `UPN` in, any other is refused with
/// `refusal`; and its authz.
async fn own_backend(refusal: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex("/oauth2/v2.0/token$"))
        .and(body_string_contains("password=made-up+password"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens(UPN, OID)))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex("/oauth2/v2.0/token$"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens(UPN, OID)))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex("/oauth2/v2.0/token$"))
        .respond_with(refusal)
        .mount(&server)
        .await;
    mount_authz(&server).await;
    server
}

fn grant_refused(codes: &[u32], suberror: Option<&str>) -> ResponseTemplate {
    ResponseTemplate::new(400).set_body_json(json!({
        "error": "invalid_grant",
        "error_description": "made-up refusal",
        "error_codes": codes,
        "suberror": suberror,
    }))
}

/// Writes `authority`'s PEM to a file in `dir`; its path as the sign-in form takes it.
fn ca_file(dir: &tempfile::TempDir, name: &str, authority: &Authority) -> String {
    let file = dir.path().join(name);
    std::fs::write(&file, &authority.pem).unwrap();
    file.display().to_string()
}

#[tokio::test]
async fn an_own_server_account_signs_in_with_its_password_and_asks_its_region_again() {
    let dir = tempfile::tempdir().unwrap();
    let files = files_in(&dir);
    let store = MemorySecretStore::new();
    let authority = authority();
    let backend = own_backend(grant_refused(&[], None)).await;
    let origin = tls_front(&authority, &backend).await;
    let server = OwnServer::parse(&origin, &ca_file(&dir, "ca.pem", &authority)).unwrap();
    let password = Secret::new("made-up password");

    let wrong = signin::add_own(&files, UPN, &server, &Secret::new("wrong"), &store).await;
    assert_eq!(wrong.unwrap_err(), "The user name or password isn't right.");
    assert!(files.accounts().unwrap().is_empty(), "an entry this attempt made goes");

    let signed = signin::add_own(&files, UPN, &server, &password, &store).await.unwrap();
    assert_eq!(signed.server.as_deref(), Some(origin.as_str()));
    assert!(signin::ready(&files, &signed, &store).await.unwrap());
    assert!(store.get(&refresh_token_key(&signed.id())).await.unwrap().is_some());
    signin::region_again(&files, &signed, &store).await.unwrap();

    let again = signin::sign_in_own_again(&files, &signed, &password, &store)
        .await
        .unwrap();
    assert_eq!(again, signed);
    let wrong = signin::add_own(&files, UPN, &server, &Secret::new("wrong"), &store).await;
    assert!(wrong.is_err());
    assert_eq!(files.accounts().unwrap(), [signed], "a signed-in entry stays");
}

#[tokio::test]
async fn an_own_server_s_refusals_are_said_in_the_user_s_words() {
    let dir = tempfile::tempdir().unwrap();
    let files = files_in(&dir);
    let store = MemorySecretStore::new();
    let authority = authority();
    let ca = ca_file(&dir, "ca.pem", &authority);
    for (refusal, said) in [
        (grant_refused(&[], Some("user_password_expired")), PASSWORD_MUST_CHANGE),
        (grant_refused(&[50_057], None), ACCOUNT_TURNED_OFF),
        (
            ResponseTemplate::new(429).insert_header("retry-after", "30"),
            "Too many attempts: wait a few minutes, then sign in again.",
        ),
    ] {
        let backend = own_backend(refusal).await;
        let server = OwnServer::parse(&tls_front(&authority, &backend).await, &ca).unwrap();
        let account = files.ask_for_on(UPN, &server).unwrap();
        let refused = signin::sign_in_own_again(&files, &account, &Secret::new("wrong"), &store).await;
        assert_eq!(refused.unwrap_err(), said);
        assert!(files.abandon(&account.id()).is_ok());
    }
    let backend = own_backend(ResponseTemplate::new(500)).await;
    let server = OwnServer::parse(&tls_front(&authority, &backend).await, &ca).unwrap();
    let account = files.ask_for_on(UPN, &server).unwrap();
    let refused = signin::sign_in_own_again(&files, &account, &Secret::new("wrong"), &store).await;
    assert!(refused.is_err_and(|said| said.starts_with("the sign-in stored nothing")));
}

#[tokio::test]
async fn an_own_server_not_trusted_or_not_reached_points_to_its_certificate_authority() {
    let dir = tempfile::tempdir().unwrap();
    let files = files_in(&dir);
    let store = MemorySecretStore::new();
    let password = Secret::new("made-up password");
    let authority = authority();
    let backend = own_backend(grant_refused(&[], None)).await;
    let origin = tls_front(&authority, &backend).await;
    let other = ca_file(&dir, "other.pem", &self::authority());

    // the sign-in form names its own fields
    for (ca, said) in [
        (
            "",
            "certificate isn't trusted. If the server has its own certificate authority",
        ),
        (other.as_str(), "isn't trusted by the certificate authority in"),
    ] {
        let server = OwnServer::parse(&origin, ca).unwrap();
        let refused = signin::add_own(&files, UPN, &server, &password, &store)
            .await
            .unwrap_err();
        assert!(refused.contains(said), "{refused}");
    }
    let closed = OwnServer::parse("https://localhost:1", "").unwrap();
    let refused = signin::add_own(&files, UPN, &closed, &password, &store)
        .await
        .unwrap_err();
    assert!(refused.contains("can't be reached. Check the address"), "{refused}");
    assert!(files.accounts().unwrap().is_empty());

    // signing in again names `accounts.toml`
    let accounts_file = files.paths.accounts_file().display().to_string();
    for (server, said) in [
        (OwnServer::parse(&origin, "").unwrap(), "add ca_file"),
        (OwnServer::parse(&origin, &other).unwrap(), "(ca_file in"),
        (
            OwnServer::parse("https://localhost:1", "").unwrap(),
            "can't be reached. For a server",
        ),
        (
            OwnServer::parse("https://localhost:1", &other).unwrap(),
            "is its certificate authority",
        ),
    ] {
        let account = files.ask_for_on(UPN, &server).unwrap();
        let refused = signin::sign_in_own_again(&files, &account, &password, &store)
            .await
            .unwrap_err();
        assert!(refused.contains(said), "{refused}");
        assert!(refused.contains(&accounts_file), "{refused}");
        PersonalAccount::remove(&files.paths, &account.id()).unwrap();
    }
}

#[tokio::test]
async fn an_accounts_file_that_cant_be_parsed_adds_no_account() {
    let dir = tempfile::tempdir().unwrap();
    let files = files_in(&dir);
    std::fs::create_dir_all(files.paths.config_dir()).unwrap();
    std::fs::write(files.paths.accounts_file(), "not = [valid\n").unwrap();
    let store = MemorySecretStore::new();
    let server = MockServer::start().await;
    let clients = clients(&server, UPN, OID).await;
    let own = OwnServer::parse("chat.example.test", "").unwrap();
    let password = Secret::new("made-up password");
    assert!(
        signin::add(&files, UPN, &clients, &store, |_| Ok(redirect()))
            .await
            .is_err()
    );
    assert!(signin::add_own(&files, UPN, &own, &password, &store).await.is_err());
    let account = PersonalAccount::pending_on(UPN, &own).unwrap();
    assert!(
        signin::sign_in_own_again(&files, &account, &password, &store)
            .await
            .is_err()
    );
}
