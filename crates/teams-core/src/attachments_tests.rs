//! End-to-end private upload and Send's direct grant, with handwritten service answers only.
use super::{Uploads, upload};
use crate::Outbox;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use teams_auth::{secrets::Secret, token::TokenError};
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_proto::{Mri, one_to_one_id};
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path},
};
const A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const D: &str = "8:orgid:00000000-0000-4000-8000-00000000000d";
const SITE: &str = "https://madeup-my.sharepoint.com/personal/lab_a/";
const DRIVE: &str = "/personal/lab_a/_api/v2.0/drive";
const ITEM: &str = "/personal/lab_a/_api/v2.0/sites/root/items/00000000-0000-4000-8000-000000000001/driveItem";
struct Tokens;
impl Credentials for Tokens {
    fn bearer(&self, _: &str) -> impl std::future::Future<Output = Result<Secret, TokenError>> + Send {
        std::future::ready(Ok(Secret::new("placeholder")))
    }
    async fn bearer_rejected(&self, audience: &str, _: &Secret) -> Result<Secret, TokenError> {
        self.bearer(audience).await
    }
}
async fn get(server: &MockServer, url: &str, body: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path(url))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}
fn grants(shared: bool, peer: &str) -> serde_json::Value {
    json!({"value":if shared {vec![grant(A,"owner"),grant(peer,"write")]} else {vec![grant(A,"owner")]}})
}
fn grant(mri: &str, role: &str) -> serde_json::Value {
    json!({"id":mri,"roles":[role],"grantedToV2":{"user":{"id":mri.strip_prefix("8:orgid:").unwrap()}}})
}
type NativeClient = Arc<Client<HttpTransport<Tokens>>>;
type NativeOutbox = Arc<Outbox<HttpTransport<Tokens>>>;
fn native(server: &MockServer) -> (NativeClient, NativeOutbox) {
    let transport = || {
        HttpTransport::with_mock_origin(
            Arc::new(Tokens),
            "https://teams.microsoft.com/api/chatsvc/amer",
            &server.uri(),
        )
        .unwrap()
        .for_account(Mri::new(A))
        .with_middle_tier("https://teams.microsoft.com/api/mt/part/amer-01")
        .unwrap()
    };
    let client = Arc::new(Client::new(transport()));
    let outbox = Arc::new(Outbox::new(Arc::new(transport()), Author::new(Mri::new(A), "A")));
    (client, outbox)
}
async fn mock_upload(server: &MockServer) {
    get(server, "/v1.0/me/drive", json!({"webUrl":format!("{SITE}Documents")})).await;
    get(
        server,
        "/v1.0/me",
        json!({"id":A.strip_prefix("8:orgid:").unwrap(),"userPrincipalName":"a@lab.example.test"}),
    )
    .await;
    get(
        server,
        DRIVE,
        json!({"owner":{"user":{"id":A.strip_prefix("8:orgid:").unwrap()}}}),
    )
    .await;
    get(
        server,
        &format!("{DRIVE}/root:/Microsoft%20Teams%20Chat%20Files"),
        json!({"id":"folder"}),
    )
    .await;
    get(
        server,
        &format!("{DRIVE}/root:/Microsoft%20Teams%20Chat%20Files:/permissions"),
        grants(false, B),
    )
    .await;
    let uploaded = json!({"id":"ABC1","name":"renamed.txt","size":3,"webUrl":format!("{SITE}Documents/renamed.txt"),"parentReference":{"driveId":"drive1"},"sharepointIds":{"listItemUniqueId":"00000000-0000-4000-8000-000000000001"}});
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(201).set_body_json(uploaded))
        .mount(server)
        .await;
}
async fn mock_recipients(server: &MockServer, peer: &str, group: bool) -> String {
    let chat = if group {
        "19:synthetic@thread.v2".to_owned()
    } else {
        one_to_one_id(A, peer).unwrap()
    };
    get(
        server,
        &format!("/api/chatsvc/amer/v1/threads/{chat}"),
        json!({"id":chat,"members":[{"id":A},{"id":peer}]}),
    )
    .await;
    get(
        server,
        &format!("/v1.0/users/{}", peer.strip_prefix("8:orgid:").unwrap()),
        json!({"id":peer.strip_prefix("8:orgid:").unwrap(),"userPrincipalName":"recipient@example.test"}),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/api/mt/part/amer-01/beta/users/fetchShortProfile"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "type": "IUserIdentity", "value": [{"mri": peer, "userPrincipalName": "recipient@example.test"}]
        })))
        .mount(server)
        .await;
    if group {
        get(
            server,
            &format!("/api/chatsvc/amer/v1/threads/{chat}/members"),
            json!({"members":[{"id":A},{"id":peer},{"id":teams_proto::TEAMS_ECHO_BOT_MRI}],"totalMemberCount":3,"isDeleted":false}),
        ).await;
    }
    chat
}

async fn mock_share(server: &MockServer, peer: &str) {
    let shared = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&shared);
    let recipient = peer.to_owned();
    Mock::given(method("GET"))
        .and(path(format!("{ITEM}/permissions")))
        .respond_with(move |_: &Request| {
            ResponseTemplate::new(200).set_body_json(grants(observed.load(Ordering::SeqCst), &recipient))
        })
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{ITEM}/invite")))
        .respond_with(move |request: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["requireSignIn"], true);
            assert_eq!(body["sendInvitation"], false);
            shared.store(true, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(json!({"value":[]}))
        })
        .mount(server)
        .await;
}

async fn private_attachment_to(peer: &str, group: bool) {
    let server = MockServer::start().await;
    let (client, outbox) = native(&server);
    mock_upload(&server).await;
    let temp = tempfile::tempdir().unwrap();
    let local_file = temp.path().join("demo.txt");
    std::fs::write(&local_file, b"abc").unwrap();
    let file = upload(&client, &outbox, local_file).await.unwrap();
    assert_eq!(file.name(), "renamed.txt");
    assert_eq!(file.size(), 3);
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.method.as_str() == "PUT")
            .count(),
        1
    );
    assert!(
        !server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|request| request.method.as_str() == "POST"),
        "staging never shares"
    );
    let chat = mock_recipients(&server, peer, group).await;
    mock_share(&server, peer).await;
    let uploads = Uploads::new(client, outbox);
    uploads.share(&chat, &[file]).await.unwrap();
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.method.as_str() == "POST" && request.url.path().ends_with("/invite"))
            .count(),
        1
    );
}

#[tokio::test]
async fn private_upload_then_send_shares_once_with_named_chat_members_and_confirms_access() {
    private_attachment_to(B, false).await;
}

#[tokio::test]
async fn a_normal_account_shares_with_its_actual_recipient_without_a_lab_roster() {
    private_attachment_to(D, false).await;
}

#[tokio::test]
async fn a_group_share_uses_complete_current_people_and_does_not_grant_service_bots() {
    private_attachment_to(B, true).await;
}

#[tokio::test]
async fn incomplete_or_changed_recipient_evidence_never_creates_a_share() {
    for members in [
        json!({"members":[{"id":A},{"id":B}],"continuationToken":"more"}),
        json!({"members":[{"id":B}],"totalMemberCount":1}),
        json!({"members":[{"id":A},{"id":B}],"totalMemberCount":3}),
    ] {
        let server = MockServer::start().await;
        let (client, outbox) = native(&server);
        mock_upload(&server).await;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("demo.txt");
        std::fs::write(&path, b"abc").unwrap();
        let file = upload(&client, &outbox, path).await.unwrap();
        mock_recipients(&server, B, true).await;
        mock_share(&server, B).await;
        override_reply(
            &server,
            "GET",
            "/api/chatsvc/amer/v1/threads/19:synthetic@thread.v2/members",
            ResponseTemplate::new(200).set_body_json(members),
        )
        .await;
        assert!(
            Uploads::new(client, outbox)
                .share("19:synthetic@thread.v2", &[file])
                .await
                .is_err()
        );
        assert!(!server.received_requests().await.unwrap().iter().any(|request| {
            request.method.as_str() == "POST"
                && (request.url.path().ends_with("/invite") || request.url.path().ends_with("/fetchShortProfile"))
        }));
    }
}

async fn override_reply(server: &MockServer, verb: &str, route: &str, response: ResponseTemplate) {
    Mock::given(method(verb))
        .and(path(route))
        .respond_with(response)
        .with_priority(1)
        .mount(server)
        .await;
}
async fn rejects_upload(fault: &str) {
    let server = MockServer::start().await;
    let (client, outbox) = native(&server);
    mock_upload(&server).await;
    let folder = format!("{DRIVE}/root:/Microsoft%20Teams%20Chat%20Files:/permissions");
    let (verb, route, response) = match fault {
        "drive-status" => ("GET", "/v1.0/me/drive", ResponseTemplate::new(403)),
        "drive-json" => (
            "GET",
            "/v1.0/me/drive",
            ResponseTemplate::new(200).set_body_string("not json"),
        ),
        "drive-site" => (
            "GET",
            "/v1.0/me/drive",
            ResponseTemplate::new(200).set_body_json(json!({"webUrl":"https://unrelated.example/file"})),
        ),
        "owner-status" => ("GET", DRIVE, ResponseTemplate::new(403)),
        "owner-json" => ("GET", DRIVE, ResponseTemplate::new(200).set_body_string("not json")),
        "owner" => (
            "GET",
            DRIVE,
            ResponseTemplate::new(200)
                .set_body_json(json!({"owner":{"user":{"id":B.strip_prefix("8:orgid:").unwrap()}}})),
        ),
        "permissions-status" => ("GET", folder.as_str(), ResponseTemplate::new(403)),
        "permissions-json" => (
            "GET",
            folder.as_str(),
            ResponseTemplate::new(200).set_body_string("not json"),
        ),
        "broad" => (
            "GET",
            folder.as_str(),
            ResponseTemplate::new(200)
                .set_body_json(json!({"value":[{"roles":["write"],"link":{"scope":"anonymous"}}]})),
        ),
        "shared" => (
            "GET",
            folder.as_str(),
            ResponseTemplate::new(200).set_body_json(grants(true, B)),
        ),
        "upload-status" => (
            "PUT",
            "/personal/lab_a/_api/v2.0/drive/root:/Microsoft%20Teams%20Chat%20Files/demo.txt:/content",
            ResponseTemplate::new(403),
        ),
        "upload-json" => (
            "PUT",
            "/personal/lab_a/_api/v2.0/drive/root:/Microsoft%20Teams%20Chat%20Files/demo.txt:/content",
            ResponseTemplate::new(201).set_body_string("not json"),
        ),
        _ => panic!("unknown fault {fault}"),
    };
    override_reply(&server, verb, route, response).await;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("demo.txt");
    std::fs::write(&path, b"abc").unwrap();
    let error = upload(&client, &outbox, path).await.unwrap_err();
    assert!(
        matches!(error, crate::EngineError::Attachment { .. }),
        "{fault}: {error:?}"
    );
    let sent = server.received_requests().await.unwrap();
    assert!(!sent.iter().any(|r| r.method.as_str() == "POST"));
    assert_eq!(
        sent.iter().filter(|r| r.method.as_str() == "PUT").count(),
        usize::from(fault.starts_with("upload-")),
        "{fault}"
    );
}
#[tokio::test]
async fn native_upload_requires_readable_drive_metadata_and_current_storage_owner() {
    for fault in [
        "drive-status",
        "drive-json",
        "drive-site",
        "owner-status",
        "owner-json",
        "owner",
    ] {
        rejects_upload(fault).await;
    }
}
#[tokio::test]
async fn native_upload_requires_private_complete_grants_and_valid_service_confirmation() {
    for fault in [
        "permissions-status",
        "permissions-json",
        "broad",
        "shared",
        "upload-status",
        "upload-json",
    ] {
        rejects_upload(fault).await;
    }
}
fn staged_file() -> teams_proto::UploadedFile {
    teams_proto::UploadedFile::from_upload(&teams_proto::DriveSite::from_web_url(SITE).unwrap(),&json!({"id":"ABC1","name":"demo.txt","size":3,"webUrl":format!("{SITE}Documents/demo.txt"),"parentReference":{"driveId":"drive1"},"sharepointIds":{"listItemUniqueId":"00000000-0000-4000-8000-000000000001"}}).to_string().into_bytes()).unwrap()
}
#[tokio::test]
async fn native_share_requires_every_current_recipient_to_have_a_readable_address() {
    for response in [
        ResponseTemplate::new(403),
        ResponseTemplate::new(200).set_body_string("not json"),
        ResponseTemplate::new(200).set_body_json(json!({"value":[]})),
        ResponseTemplate::new(200).set_body_json(json!({"type":"IUserIdentity","value":[{"mri":B}]})),
    ] {
        let server = MockServer::start().await;
        let (client, outbox) = native(&server);
        let chat = mock_recipients(&server, B, false).await;
        override_reply(
            &server,
            "POST",
            "/api/mt/part/amer-01/beta/users/fetchShortProfile",
            response,
        )
        .await;
        assert!(
            Uploads::new(client, outbox)
                .share(&chat, &[staged_file()])
                .await
                .is_err()
        );
        assert!(
            !server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|r| r.url.path().ends_with("/invite"))
        );
    }
}
#[tokio::test]
async fn a_file_sent_to_the_account_s_own_notes_is_shared_with_nobody() {
    let server = MockServer::start().await;
    let (client, outbox) = native(&server);
    Uploads::new(client, outbox)
        .share(crate::NOTES, &[staged_file()])
        .await
        .unwrap();
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "no member read and no grant: nobody else is in the notes"
    );
}
#[tokio::test]
async fn native_share_confirms_exact_people_after_the_single_successful_grant() {
    use std::sync::atomic::AtomicUsize;
    for outsider in [true, false] {
        let server = MockServer::start().await;
        let (client, outbox) = native(&server);
        mock_upload(&server).await;
        let chat = mock_recipients(&server, B, false).await;
        let reads = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&reads);
        Mock::given(method("GET"))
            .and(path(format!("{ITEM}/permissions")))
            .respond_with(move |_: &Request| {
                let after = observed.fetch_add(1, Ordering::SeqCst) >= 2;
                ResponseTemplate::new(200).set_body_json(if after && outsider {
                    grants(true, D)
                } else {
                    grants(false, B)
                })
            })
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("{ITEM}/invite")))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let error = Uploads::new(client, outbox)
            .share(&chat, &[staged_file()])
            .await
            .unwrap_err();
        assert!(matches!(error, crate::EngineError::Attachment { .. }));
        assert_eq!(reads.load(Ordering::SeqCst), 3);
        assert_eq!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|r| r.url.path().ends_with("/invite"))
                .count(),
            1
        );
    }
}
