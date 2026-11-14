//! Handwritten protocol answers and placeholder identities; no captured account data.
use std::sync::Arc;

use crate::Mri;
use crate::one_to_one_id;
use serde_json::{Value, json};
use teams_auth::{secrets::Secret, token::TokenError};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

use super::{Credentials, HttpTransport, Transport};
use crate::{DriveSite, DriveWrite, UploadedFile};

const A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const X: &str = "8:orgid:00000000-0000-4000-8000-00000000000d";
const SITE: &str = "https://madeup-my.sharepoint.com/personal/lab_a/";
const DRIVE: &str = "/personal/lab_a/_api/v2.0/drive";
const GUID: &str = "00000000-0000-4000-8000-000000000001";

struct Tokens;
impl Credentials for Tokens {
    fn bearer(&self, _: &str) -> impl std::future::Future<Output = Result<Secret, TokenError>> + Send {
        std::future::ready(Ok(Secret::new("placeholder")))
    }
    async fn bearer_rejected(&self, audience: &str, _: &Secret) -> Result<Secret, TokenError> {
        self.bearer(audience).await
    }
}
fn transport(server: &MockServer) -> HttpTransport<Tokens> {
    HttpTransport::with_mock_origin(
        Arc::new(Tokens),
        "https://teams.microsoft.com/api/chatsvc/amer",
        &server.uri(),
    )
    .unwrap()
    .for_account(Mri::new(A))
}
fn site() -> DriveSite {
    DriveSite::from_web_url(SITE).unwrap()
}
fn file() -> UploadedFile {
    UploadedFile::from_upload(&site(), &serde_json::to_vec(&json!({"id":"ABC123","name":"demo.txt","size":3,"webUrl":format!("{SITE}Documents/demo.txt"),"parentReference":{"driveId":"drive1"},"sharepointIds":{"listItemUniqueId":GUID}})).unwrap()).unwrap()
}
fn grants(people: &[&str]) -> Value {
    json!({"value":people.iter().map(|mri|json!({"id":format!("grant-{mri}"),"roles":[if *mri==A {"owner"} else {"write"}],"grantedToV2":{"user":{"id":mri.strip_prefix("8:orgid:").unwrap()}}})).collect::<Vec<_>>()})
}
async fn get(server: &MockServer, address: &str, answer: Value) {
    Mock::given(method("GET"))
        .and(path(address.replace(' ', "%20")))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer))
        .mount(server)
        .await;
}
async fn identity(server: &MockServer, actor: &str) {
    get(
        server,
        "/v1.0/me",
        json!({"id":actor.strip_prefix("8:orgid:").unwrap(),"userPrincipalName":"a@lab.example.test"}),
    )
    .await;
}
async fn chat(server: &MockServer, members: &[&str]) -> String {
    let id = one_to_one_id(A, B).unwrap();
    get(
        server,
        &format!("/api/chatsvc/amer/v1/threads/{id}"),
        json!({"id":id,"members":members.iter().map(|mri|json!({"id":mri})).collect::<Vec<_>>()}),
    )
    .await;
    id
}
async fn owner(server: &MockServer) {
    get(
        server,
        DRIVE,
        json!({"owner":{"user":{"id":A.strip_prefix("8:orgid:").unwrap()}}}),
    )
    .await;
}
async fn permissions(server: &MockServer, body: Value) {
    get(
        server,
        &format!("/personal/lab_a/_api/v2.0/sites/root/items/{GUID}/driveItem/permissions"),
        body,
    )
    .await;
}
async fn writes(server: &MockServer) -> Vec<wiremock::Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.method.as_str() != "GET")
        .collect()
}

#[tokio::test]
async fn a_private_upload_uses_encoded_filename_and_never_overwrites() {
    let server = MockServer::start().await;
    identity(&server, A).await;
    owner(&server).await;
    get(
        &server,
        &format!("{DRIVE}/root:/Microsoft Teams Chat Files"),
        json!({"id":"folder"}),
    )
    .await;
    get(
        &server,
        &format!("{DRIVE}/root:/Microsoft Teams Chat Files:/permissions"),
        grants(&[A]),
    )
    .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;
    let write = DriveWrite::upload(&site(), "100% #notes;.txt", b"abc".to_vec(), A).unwrap();
    let answer = transport(&server).deliver_attachment(write).await.unwrap().unwrap();
    assert_eq!(answer.status(), 201);
    let requests = writes(&server).await;
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].url.path().ends_with("100%25%20%23notes;.txt:/content"),
        "{}",
        requests[0].url
    );
    assert!(
        requests[0]
            .url
            .query_pairs()
            .any(|(key, value)| key == "@name.conflictBehavior" && value == "rename")
    );
    assert_eq!(requests[0].body, b"abc");
}

#[tokio::test]
async fn a_share_proves_actor_peers_and_existing_access_before_explicit_direct_grant() {
    let server = MockServer::start().await;
    identity(&server, A).await;
    let id = chat(&server, &[A, B]).await;
    owner(&server).await;
    permissions(&server, grants(&[A])).await;
    get(
        &server,
        "/v1.0/users/00000000-0000-4000-8000-00000000000b",
        json!({"id":B.strip_prefix("8:orgid:").unwrap(),"userPrincipalName":"b@lab.example.test"}),
    )
    .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let write = DriveWrite::share(&file(), &id, &[(B.into(), "b@lab.example.test".into())], A).unwrap();
    assert_eq!(
        transport(&server)
            .deliver_attachment(write)
            .await
            .unwrap()
            .unwrap()
            .status(),
        200
    );
    let requests = writes(&server).await;
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        body,
        json!({"recipients":[{"email":"b@lab.example.test"}],"roles":["write"],"requireSignIn":true,"sendInvitation":false})
    );
}

#[tokio::test]
async fn wrong_actor_mixed_chat_and_broad_existing_grants_never_write() {
    for fault in ["actor", "chat", "link", "outside", "upn"] {
        let server = MockServer::start().await;
        identity(&server, if fault == "actor" { X } else { A }).await;
        let id = chat(&server, if fault == "chat" { &[A, X] } else { &[A, B] }).await;
        owner(&server).await;
        let permission = if fault == "link" {
            json!({"value":[{"roles":["write"],"link":{"scope":"anonymous"}}]})
        } else if fault == "outside" {
            grants(&[A, X])
        } else {
            grants(&[A])
        };
        permissions(&server, permission).await;
        get(&server,"/v1.0/users/00000000-0000-4000-8000-00000000000b",json!({"id":B.strip_prefix("8:orgid:").unwrap(),"userPrincipalName":if fault=="upn" {"external@outside.test"} else {"b@lab.example.test"}})).await;
        let write = DriveWrite::share(&file(), &id, &[(B.into(), "b@lab.example.test".into())], A).unwrap();
        assert!(
            transport(&server).deliver_attachment(write).await.unwrap().is_err(),
            "{fault}"
        );
        assert!(writes(&server).await.is_empty(), "{fault}");
    }
}

#[tokio::test]
async fn staging_refuses_shared_folders_and_read_only_never_requests_credentials_or_network() {
    let server = MockServer::start().await;
    identity(&server, A).await;
    owner(&server).await;
    get(
        &server,
        &format!("{DRIVE}/root:/Microsoft Teams Chat Files"),
        json!({"id":"folder"}),
    )
    .await;
    get(
        &server,
        &format!("{DRIVE}/root:/Microsoft Teams Chat Files:/permissions"),
        grants(&[A, B]),
    )
    .await;
    let write = DriveWrite::upload(&site(), "demo.txt", vec![1], A).unwrap();
    assert!(
        transport(&server)
            .deliver_attachment(write.clone())
            .await
            .unwrap()
            .is_err()
    );
    assert!(writes(&server).await.is_empty());
    server.reset().await;
    assert!(
        transport(&server)
            .read_only()
            .deliver_attachment(write)
            .await
            .unwrap()
            .is_err()
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_refused_or_uncertain_upload_is_not_replayed_by_the_transport() {
    for status in [401, 429, 500, 503] {
        let server = MockServer::start().await;
        identity(&server, A).await;
        owner(&server).await;
        get(
            &server,
            &format!("{DRIVE}/root:/Microsoft Teams Chat Files"),
            json!({"id":"folder"}),
        )
        .await;
        get(
            &server,
            &format!("{DRIVE}/root:/Microsoft Teams Chat Files:/permissions"),
            grants(&[A]),
        )
        .await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
        let write = DriveWrite::upload(&site(), "demo.txt", vec![1], A).unwrap();
        assert_eq!(
            transport(&server)
                .deliver_attachment(write)
                .await
                .unwrap()
                .unwrap()
                .status(),
            status
        );
        assert_eq!(writes(&server).await.len(), 1);
    }
}

const CHANNEL: &str = "19:synthetic-channel@thread.tacv2";
#[tokio::test]
async fn channel_own_preferences_send_once_after_actor_proof_without_contacting_a_team() {
    let server = MockServer::start().await;
    identity(&server, A).await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let request = crate::OwnTeamView::ChannelProperty {
        channel: CHANNEL.into(),
        property: crate::ChannelOwnProperty::Notifications,
        value: "{\"allNewPosts\":true}".into(),
    };
    assert_eq!(transport(&server).channel_own(&request).await.unwrap().status(), 200);
    assert_eq!(writes(&server).await.len(), 1);
    let reads = server.received_requests().await.unwrap();
    assert!(!reads.iter().any(|request| request.url.path().contains("/threads/")));
}

#[tokio::test]
async fn channel_own_refuses_changed_actor_other_interest_account_and_read_only() {
    let request = crate::OwnTeamView::ChannelInterest {
        channel: CHANNEL.into(),
        own: B.into(),
        shown: false,
    };
    let server = MockServer::start().await;
    identity(&server, X).await;
    assert!(transport(&server).channel_own(&request).await.is_err());
    let request = crate::OwnTeamView::ChannelFavorite {
        channel: CHANNEL.into(),
        general: false,
        shown: false,
    };
    assert!(transport(&server).channel_own(&request).await.is_err());
    assert!(transport(&server).read_only().channel_own(&request).await.is_err());
    assert!(writes(&server).await.is_empty());
}

#[tokio::test]
async fn channel_own_folder_conflict_is_returned_without_replaying_a_create() {
    let server = MockServer::start().await;
    identity(&server, A).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(409))
        .mount(&server)
        .await;
    let request = crate::ChannelSectionWrite::new(
        &crate::ChannelSectionAction::Create {
            channel: CHANNEL.into(),
            name: "Synthetic section".into(),
        },
        &crate::ChannelSections {
            version: 7,
            sections: Vec::new(),
        },
    )
    .unwrap();
    let transport = transport(&server)
        .with_csa("https://teams.microsoft.com/api/csa/amer")
        .unwrap();
    assert_eq!(transport.channel_own(&request).await.unwrap().status(), 409);
    assert_eq!(writes(&server).await.len(), 1);
}

#[derive(Default)]
struct RetryTokens {
    audiences: std::sync::Mutex<Vec<String>>,
    bearers: std::sync::atomic::AtomicUsize,
    renewals: std::sync::atomic::AtomicUsize,
}
impl Credentials for RetryTokens {
    fn bearer(&self, audience: &str) -> impl std::future::Future<Output = Result<Secret, TokenError>> + Send {
        self.audiences.lock().unwrap().push(audience.to_owned());
        let generation = self.bearers.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::future::ready(Ok(Secret::new(format!("placeholder-generation-{generation}"))))
    }
    fn bearer_rejected(
        &self,
        audience: &str,
        refused: &Secret,
    ) -> impl std::future::Future<Output = Result<Secret, TokenError>> + Send {
        assert_eq!(refused.expose(), "placeholder-generation-0");
        self.audiences.lock().unwrap().push(audience.to_owned());
        self.renewals.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::future::ready(Ok(Secret::new("placeholder-renewed")))
    }
}
fn retry_transport(server: &MockServer, tokens: Arc<RetryTokens>) -> HttpTransport<RetryTokens> {
    HttpTransport::with_mock_origin(tokens, "https://teams.microsoft.com/api/chatsvc/amer", &server.uri())
        .unwrap()
        .with_max_retry_wait(std::time::Duration::ZERO)
}
#[tokio::test]
async fn own_drive_reads_retry_once_with_renewal_or_capped_throttle_and_dynamic_audience() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for status in [401, 429, 503] {
        let server = MockServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&attempts);
        Mock::given(method("GET"))
            .and(path(DRIVE))
            .respond_with(move |_: &wiremock::Request| {
                if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(status).insert_header("Retry-After", "999999")
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({"owner":{"user":{"id":"synthetic-owner"}}}))
                }
            })
            .mount(&server)
            .await;
        let tokens = Arc::new(RetryTokens::default());
        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            retry_transport(&server, Arc::clone(&tokens)).drive_details(&site()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(answer.is_success());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(
            tokens.audiences.lock().unwrap().as_slice(),
            [site().audience(), site().audience()]
        );
        assert_eq!(tokens.renewals.load(Ordering::SeqCst), usize::from(status == 401));
        assert_eq!(tokens.bearers.load(Ordering::SeqCst), if status == 401 { 1 } else { 2 });
        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests[0].headers.get("authorization").unwrap(),
            "Bearer placeholder-generation-0"
        );
        assert_eq!(
            requests[1].headers.get("authorization").unwrap(),
            if status == 401 {
                "Bearer placeholder-renewed"
            } else {
                "Bearer placeholder-generation-1"
            }
        );
    }
}
#[tokio::test]
async fn own_drive_reads_stop_after_one_retry_and_other_errors_get_one_attempt() {
    for status in [401, 429, 503, 403, 404, 500] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(DRIVE))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
        let error = retry_transport(&server, Arc::new(RetryTokens::default()))
            .drive_details(&site())
            .await
            .unwrap_err();
        assert!(matches!(error,super::HttpError::Status(answer) if answer.status()==status));
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            if matches!(status, 401 | 429 | 503) { 2 } else { 1 }
        );
    }
}
#[tokio::test]
async fn own_drive_connection_failure_propagates_without_credential_refresh() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let tokens = Arc::new(RetryTokens::default());
    let transport = HttpTransport::with_mock_origin(
        Arc::clone(&tokens),
        "https://teams.microsoft.com/api/chatsvc/amer",
        &origin,
    )
    .unwrap();
    // Close the reserved loopback port before the request; no service endpoint is contacted.
    drop(listener);
    assert!(matches!(
        transport.drive_details(&site()).await,
        Err(super::HttpError::Failed(super::Failure::Network))
    ));
    assert_eq!(tokens.bearers.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(tokens.renewals.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn own_drive_upload_and_share_keep_every_preflight_and_write_on_own_services() {
    let cloud = crate::Cloud::own("https://chat.example:8443").unwrap();
    let server = MockServer::start().await;
    let transport = HttpTransport::for_cloud_with_mock_origin(
        Arc::new(Tokens),
        "https://chat.example:8443/api/chatsvc/amer",
        cloud.clone(),
        &server.uri(),
    )
    .unwrap()
    .for_account(Mri::new(A));
    let base = "https://chat.example:8443/sp/personal/lab_a/";
    let site = DriveSite::from_web_url_in(&cloud, base).unwrap();
    own_drive_preflight(&server).await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;
    let upload = DriveWrite::upload(&site, "own.txt", b"abc".to_vec(), A).unwrap();
    assert_eq!(
        transport.deliver_attachment(upload).await.unwrap().unwrap().status(),
        201
    );
    let file = UploadedFile::from_upload(
        &site,
        &serde_json::to_vec(&json!({
            "id":"ABC123","name":"own.txt","size":3,"webUrl":format!("{base}Documents/own.txt"),
            "parentReference":{"driveId":"drive1"},"sharepointIds":{"listItemUniqueId":GUID}
        }))
        .unwrap(),
    )
    .unwrap();
    let id = chat(&server, &[A, B]).await;
    get(
        &server,
        &format!("/sp/personal/lab_a/_api/v2.0/sites/root/items/{GUID}/driveItem/permissions"),
        grants(&[A]),
    )
    .await;
    get(
        &server,
        &format!("/graph/v1.0/users/{}", B.strip_prefix("8:orgid:").unwrap()),
        json!({"id":B.strip_prefix("8:orgid:").unwrap(),"userPrincipalName":"b@lab.example.test"}),
    )
    .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let share = DriveWrite::share_in(&cloud, &file, &id, &[(B.into(), "b@lab.example.test".into())], A).unwrap();
    assert_eq!(
        transport.deliver_attachment(share).await.unwrap().unwrap().status(),
        200
    );
    let writes = writes(&server).await;
    assert_eq!(writes.len(), 2);
    assert!(
        writes
            .iter()
            .all(|request| request.url.path().starts_with("/sp/personal/lab_a/"))
    );
    assert!(server.received_requests().await.unwrap().iter().all(|request| {
        ["/sp/", "/graph/", "/api/chatsvc/"]
            .iter()
            .any(|prefix| request.url.path().starts_with(prefix))
    }));
    assert!(transport.drive_details(&self::site()).await.is_err());
    assert!(
        transport
            .deliver_attachment(DriveWrite::upload(&self::site(), "foreign.txt", b"abc".to_vec(), A).unwrap())
            .await
            .unwrap()
            .is_err()
    );
}

async fn own_drive_preflight(server: &MockServer) {
    get(
        server,
        "/graph/v1.0/me",
        json!({"id":A.strip_prefix("8:orgid:").unwrap(),"userPrincipalName":"a@lab.example.test"}),
    )
    .await;
    get(
        server,
        "/sp/personal/lab_a/_api/v2.0/drive",
        json!({"owner":{"user":{"id":A.strip_prefix("8:orgid:").unwrap()}}}),
    )
    .await;
    get(
        server,
        "/sp/personal/lab_a/_api/v2.0/drive/root:/Microsoft Teams Chat Files",
        json!({"id":"folder"}),
    )
    .await;
    get(
        server,
        "/sp/personal/lab_a/_api/v2.0/drive/root:/Microsoft Teams Chat Files:/permissions",
        grants(&[A]),
    )
    .await;
}
