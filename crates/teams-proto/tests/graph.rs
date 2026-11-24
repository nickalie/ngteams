//! Graph reads against synthetic mocks: selected user fields, typed pictures, service boundaries and credential renewal.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_proto::{
    Client, Credentials, Endpoint, FileDownload, FileRange, FileThumbnails, GRAPH_AUDIENCE, GraphAnswerError,
    GraphPicture, GraphUser, HttpError, HttpTransport, MAX_PICTURE, PictureFormat, Read, Refusal, Response, Service,
    is_thumbnail_link, object_id,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const GUID_A: &str = "0000000a-0000-4000-8000-00000000000a";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const FIXTURE: &str = include_str!("../../../tests/fixtures/graph/profile.json");
const THUMBNAILS: &str = include_str!("../../../tests/fixtures/graph/thumbnails.json");
/// A made-up sharing link of a video, on the documentation tenant's OneDrive.
const SHARE: &str = "https://contoso-my.sharepoint.com/:v:/g/personal/made_up_user_contoso_test/MadeUpShareLink0001";

fn lab_a() -> String {
    format!("8:orgid:{GUID_A}")
}

/// The fixture with `<guid:a>` filled in.
fn fixture() -> Value {
    serde_json::from_str(&FIXTURE.replace("<guid:a>", GUID_A)).unwrap()
}

/// Hands out placeholder tokens and records what the transport asked for.
#[derive(Default)]
struct FakeTokens {
    generation: AtomicUsize,
    audiences: Mutex<Vec<String>>,
}

impl FakeTokens {
    fn audiences(&self) -> Vec<String> {
        self.audiences.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn current(&self, audience: &str) -> Secret {
        self.audiences
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(audience.to_owned());
        Secret::new(format!(
            "placeholder-graph-token-{}",
            self.generation.load(Ordering::SeqCst)
        ))
    }
}

impl Credentials for FakeTokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(self.current(audience)))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.generation.fetch_add(1, Ordering::SeqCst);
        future::ready(Ok(self.current(audience)))
    }
}

async fn setup() -> (MockServer, Arc<FakeTokens>, Client<HttpTransport<FakeTokens>>) {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
    (server, tokens, Client::new(transport))
}

fn json_answer(case: &Value) -> ResponseTemplate {
    ResponseTemplate::new(u16::try_from(case["status"].as_u64().unwrap()).unwrap())
        .set_body_raw(case["body"].to_string(), case["content_type"].as_str().unwrap())
}

#[tokio::test]
async fn a_user_read_is_a_get_of_its_probed_shape_with_the_graph_token_and_its_name_is_read() {
    let (server, tokens, client) = setup().await;
    let fixture = fixture();
    Mock::given(method("GET"))
        .and(path(format!("/v1.0/users/{GUID_A}")))
        .and(query_param("$select", "id,displayName"))
        .respond_with(json_answer(&fixture["user"]))
        .mount(&server)
        .await;
    let user = GraphUser::of(&lab_a()).unwrap();
    let answer = client.read(&user).await.unwrap();
    assert_eq!(
        user.read_answer(answer.body()).unwrap().as_deref(),
        Some("Lab Account A")
    );

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method.as_str(), "GET");
    assert!(request.body.is_empty());
    assert_eq!(request.url.query(), Some("%24select=id%2CdisplayName"));
    let mut names: Vec<&str> = request.headers.keys().map(wiremock::http::HeaderName::as_str).collect();
    names.sort_unstable();
    for name in ["accept", "authorization"] {
        assert!(names.contains(&name), "{names:?}");
    }
    for name in &names {
        assert!(
            !name.starts_with("x-ms-") && *name != "behavioroverride" && *name != "content-type",
            "{names:?}"
        );
    }
    assert_eq!(
        request.headers.get("authorization").unwrap().to_str().unwrap(),
        "Bearer placeholder-graph-token-0"
    );
    assert_eq!(tokens.audiences(), vec![GRAPH_AUDIENCE.to_owned()]);
}

#[tokio::test]
async fn a_user_answer_without_a_name_or_of_another_account_gives_no_name() {
    let fixture = fixture();
    let user = GraphUser::of(&lab_a()).unwrap();
    let without = fixture["user_without_name"]["body"].to_string();
    assert_eq!(user.read_answer(without.as_bytes()), Ok(None));
    let blank = format!(r#"{{"id": "{GUID_A}", "displayName": "   "}}"#);
    assert_eq!(user.read_answer(blank.as_bytes()), Ok(None));
    let other = fixture["user"]["body"]
        .to_string()
        .replace(GUID_A, "0000000b-0000-4000-8000-00000000000b");
    assert_eq!(user.read_answer(other.as_bytes()), Err(GraphAnswerError::OtherUser));
    let upper = fixture["user"]["body"]
        .to_string()
        .replace(GUID_A, &GUID_A.to_uppercase());
    assert_eq!(
        user.read_answer(upper.as_bytes()).unwrap().as_deref(),
        Some("Lab Account A")
    );
    for malformed in [
        &b"[]"[..],
        b"{}",
        br#"{"id": 3}"#,
        br#"{"id": "x", "displayName": 3}"#,
        b"not json",
    ] {
        assert_eq!(user.read_answer(malformed), Err(GraphAnswerError::Malformed));
    }
}

#[tokio::test]
async fn a_picture_read_gives_the_image_bytes_and_an_account_without_one_answers_404() {
    let (server, _, client) = setup().await;
    let fixture = fixture();
    let bytes = STANDARD
        .decode(fixture["picture"]["body_base64"].as_str().unwrap())
        .unwrap();
    Mock::given(method("GET"))
        .and(path(format!("/v1.0/users/{GUID_A}/photos/96x96/$value")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(bytes.clone(), "image/jpeg"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1.0/users/{GUID_A}/photos/96x96/$value")))
        .respond_with(json_answer(&fixture["no_picture"]))
        .mount(&server)
        .await;
    let picture = GraphPicture::of(&lab_a()).unwrap();
    let answer = client.read(&picture).await.unwrap();
    let read = picture.read_answer(&answer).unwrap();
    assert_eq!(read.format, PictureFormat::Jpeg);
    assert_eq!(read.bytes, bytes);
    assert_eq!(
        format!("{read:?}"),
        format!("Picture {{ format: Jpeg, bytes: {} }}", bytes.len())
    );

    match client.read(&picture).await {
        Err(HttpError::Status(answer)) => assert_eq!(answer.status(), 404),
        other => panic!("{other:?}"),
    }
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .all(|request| request.method.as_str() == "GET" && request.url.query().is_none())
    );
}

#[test]
fn a_picture_of_another_type_too_large_or_empty_isnt_kept() {
    let picture = GraphPicture::of(&lab_a()).unwrap();
    let answer = |content_type: &str, size: usize| Response::new(200, vec![0xFF; size]).with_content_type(content_type);
    assert_eq!(
        picture.read_answer(&answer("image/png", 10)).unwrap().format,
        PictureFormat::Png
    );
    assert_eq!(
        picture
            .read_answer(&answer("IMAGE/JPEG; charset=binary", 10))
            .unwrap()
            .format,
        PictureFormat::Jpeg
    );
    for content_type in ["text/html", "image/svg+xml", "image/gif", "application/json"] {
        assert_eq!(
            picture.read_answer(&answer(content_type, 10)),
            Err(GraphAnswerError::UnknownType),
            "{content_type}"
        );
    }
    assert_eq!(
        picture.read_answer(&Response::new(200, vec![1, 2, 3])),
        Err(GraphAnswerError::UnknownType)
    );
    assert_eq!(
        picture.read_answer(&answer("image/jpeg", MAX_PICTURE + 1)),
        Err(GraphAnswerError::TooLarge)
    );
    assert_eq!(
        picture.read_answer(&answer("image/jpeg", 0)),
        Err(GraphAnswerError::Malformed)
    );
}

#[test]
fn only_an_orgid_mri_with_a_guid_is_read_from_graph() {
    assert_eq!(object_id(&lab_a()), Some(GUID_A));
    for mri in [
        "8:orgid:lab-a",
        "28:0000000a-0000-4000-8000-00000000000a",
        "8:orgid:0000000a-0000-4000-8000-00000000000a/photo",
        "8:orgid:0000000a00004000800000000000000a",
        "8:orgid:",
        "8:live:someone",
    ] {
        assert!(GraphUser::of(mri).is_none(), "{mri}");
        assert!(GraphPicture::of(mri).is_none(), "{mri}");
    }
    assert!(format!("{:?}", GraphUser::of(&lab_a()).unwrap()).find(GUID_A).is_none());
}

/// A read of Graph in a shape the probe didn't show: any type may implement `Read`, so the transport's path check is
/// what refuses it.
struct OtherGraphRead(Endpoint);

impl Read for OtherGraphRead {
    fn service(&self) -> Service {
        Service::Graph
    }

    fn endpoint(&self) -> Endpoint {
        self.0.clone()
    }
}

#[tokio::test]
async fn any_other_graph_read_goes_to_graph_with_its_query() {
    let (server, _tokens, client) = setup().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("{}", "application/json"))
        .mount(&server)
        .await;
    let others = [
        Endpoint::new(["users", GUID_A]).with_query("$select", "id,displayName,mail"),
        Endpoint::new(["users", GUID_A, "photo", "$value"]),
        Endpoint::new(["me"]),
    ];
    for endpoint in others {
        client.read(&OtherGraphRead(endpoint)).await.unwrap();
    }
    let urls: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| format!("{}?{}", request.url.path(), request.url.query().unwrap_or_default()))
        .collect();
    assert_eq!(
        urls,
        [
            format!("/v1.0/users/{GUID_A}?%24select=id%2CdisplayName%2Cmail"),
            format!("/v1.0/users/{GUID_A}/photo/$value?"),
            "/v1.0/me?".to_owned(),
        ]
    );
}

#[tokio::test]
async fn a_refused_graph_token_is_renewed_and_the_read_sent_once_more() {
    let (server, tokens, client) = setup().await;
    let fixture = fixture();
    Mock::given(method("GET"))
        .and(path(format!("/v1.0/users/{GUID_A}")))
        .respond_with(ResponseTemplate::new(401))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1.0/users/{GUID_A}")))
        .respond_with(json_answer(&fixture["user"]))
        .mount(&server)
        .await;
    let user = GraphUser::of(&lab_a()).unwrap();
    let answer = client.read(&user).await.unwrap();
    assert_eq!(
        user.read_answer(answer.body()).unwrap().as_deref(),
        Some("Lab Account A")
    );
    let requests = server.received_requests().await.unwrap();
    let bearers: Vec<&str> = requests
        .iter()
        .map(|request| request.headers.get("authorization").unwrap().to_str().unwrap())
        .collect();
    assert_eq!(
        bearers,
        ["Bearer placeholder-graph-token-0", "Bearer placeholder-graph-token-1"]
    );
    assert!(tokens.audiences().iter().all(|audience| audience == GRAPH_AUDIENCE));
}

/// The thumbnail fixture's answer with its `large` picture's link pointing at `large`.
fn thumbnails_answer(large: &str) -> String {
    let mut answer: Value = serde_json::from_str::<Value>(THUMBNAILS).unwrap()["answer"].clone();
    answer["value"][0]["large"]["url"] = Value::String(large.to_owned());
    answer.to_string()
}

#[test]
fn a_shared_files_thumbnails_are_read_by_its_sharing_link_and_the_large_ones_link_is_taken() {
    let read = FileThumbnails::of(SHARE).unwrap();
    assert_eq!(read.service(), Service::Graph);
    // Graph's sharing token: `u!` and the link in base64url without padding
    let token = format!(
        "u!{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(SHARE.as_bytes())
    );
    assert_eq!(read.share(), token);
    assert_eq!(
        read.endpoint(),
        Endpoint::new(["shares", token.as_str(), "driveItem", "thumbnails"])
    );
    // only a SharePoint `https` link names a file
    for other in [
        "http://contoso-my.sharepoint.com/:v:/g/x",
        "https://example.com/:v:/g/x",
        "https://contoso-my.sharepoint.com.example.com/x",
        "https://user@contoso-my.sharepoint.com/x",
        "https://contoso-my.sharepoint.com:8443/x",
        "not a link",
    ] {
        assert!(FileThumbnails::of(other).is_none(), "{other}");
    }
    let fixture: Value = serde_json::from_str(THUMBNAILS).unwrap();
    let answer = |body: &Value| Response::new(200, body.to_string());
    assert!(read.read_answer(&answer(&fixture["answer"])).is_some());
    // no thumbnail, or an answer that isn't Graph's: none
    assert!(read.read_answer(&answer(&fixture["empty"])).is_none());
    assert!(read.read_answer(&Response::new(200, "not json")).is_none());
    // a `large` link off the thumbnail hosts is passed over for the `medium` one; with neither, none
    let elsewhere = thumbnails_answer("https://example.com/thumbnail.jpg");
    let mut neither: Value = serde_json::from_str(&elsewhere).unwrap();
    assert!(read.read_answer(&Response::new(200, elsewhere)).is_some());
    neither["value"][0]["medium"]["url"] = Value::String("http://westus1-mediap.svc.ms/x".to_owned());
    assert!(read.read_answer(&answer(&neither)).is_none());
}

#[test]
fn a_thumbnails_link_is_read_only_on_the_media_host_or_a_sharepoint_host() {
    let link = |text: &str| url::Url::parse(text).unwrap();
    for taken in [
        "https://westus1-mediap.svc.ms/transform/thumbnail?provider=spo&docid=x",
        "https://contoso-my.sharepoint.com/_api/v2.0/thumbnail?tempauth=x",
    ] {
        assert!(is_thumbnail_link(&link(taken)), "{taken}");
    }
    for refused in [
        "http://westus1-mediap.svc.ms/transform/thumbnail",
        "https://svc.ms/transform/thumbnail",
        "https://westus1-mediap.svc.ms.example.com/x",
        "https://user@westus1-mediap.svc.ms/x",
        "https://westus1-mediap.svc.ms:444/x",
        "https://westus1-mediap.svc.ms/x#fragment",
        "https://graph.microsoft.com/v1.0/x",
    ] {
        assert!(!is_thumbnail_link(&link(refused)), "{refused}");
    }
}

#[tokio::test]
async fn a_thumbnail_is_listed_with_the_graph_token_and_its_picture_read_with_none() {
    let (server, tokens, client) = setup().await;
    let read = FileThumbnails::of(SHARE).unwrap();
    let large = "https://westus1-mediap.svc.ms/transform/thumbnail?provider=spo&cs=made-up&docid=made-up&width=800";
    Mock::given(method("GET"))
        .and(path(format!("/v1.0/shares/{}/driveItem/thumbnails", read.share())))
        .respond_with(ResponseTemplate::new(200).set_body_raw(thumbnails_answer(large), "application/json"))
        .mount(&server)
        .await;
    let jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10];
    Mock::given(method("GET"))
        .and(path("/transform/thumbnail"))
        .and(query_param("docid", "made-up"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(jpeg.clone(), "image/jpeg"))
        .mount(&server)
        .await;
    let listed = client.read(&read).await.unwrap();
    let link = read.read_answer(&listed).unwrap();
    assert!(
        !format!("{link:?}").contains("svc.ms"),
        "Debug shows nothing of the link"
    );
    let picture = client.transport().file_thumbnail(&link).await.unwrap();
    assert_eq!(picture.body(), jpeg);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].headers.get("authorization").unwrap().to_str().unwrap(),
        "Bearer placeholder-graph-token-0"
    );
    // the picture's read carries the link's own query and no credential of the account's
    assert_eq!(requests[1].method.as_str(), "GET");
    assert!(requests[1].headers.get("authorization").is_none());
    assert!(requests[1].headers.get("x-skypetoken").is_none());
    assert_eq!(
        requests[1].url.query(),
        Some("provider=spo&cs=made-up&docid=made-up&width=800")
    );
    assert_eq!(tokens.audiences(), vec![GRAPH_AUDIENCE.to_owned()]);
}

/// A link for specific people in the short form Teams posts (`/:v:/p/<owner>/<token>`), made up.
const PEOPLE_SHARE: &str = "https://contoso-my.sharepoint.com/:v:/p/made_up_user_contoso_test/MadeUpPeopleLink0002";

/// Graph's answer to a read of a specific-people link without `Prefer: redeemSharingLink` (the shape of the user's
/// log, made up).
fn link_refused() -> ResponseTemplate {
    let refusal = serde_json::json!({"error": {
        "code": "accessDenied",
        "message": "The sharing link no longer exists, or you do not have permission to access it.",
        "innerError": {"code": "SharesAccessDeniedVroomException"},
    }});
    ResponseTemplate::new(403).set_body_raw(refusal.to_string(), "application/json")
}

#[tokio::test]
async fn a_specific_people_link_is_redeemed_as_teams_reads_it() {
    let (server, tokens, client) = setup().await;
    let thumbnails = FileThumbnails::of(PEOPLE_SHARE).unwrap();
    let item = FileDownload::of(PEOPLE_SHARE).unwrap();
    let large = "https://westus1-mediap.svc.ms/transform/thumbnail?provider=spo&cs=made-up&docid=made-up&width=800";
    let link = "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test/_layouts/15/download.aspx\
                ?UniqueId=made-up&tempauth=made-up";
    let answer = serde_json::json!({"id": "made-up", "size": 10, "@microsoft.graph.downloadUrl": link});
    for (address, body) in [
        (
            format!("/v1.0/shares/{}/driveItem/thumbnails", thumbnails.share()),
            thumbnails_answer(large),
        ),
        (format!("/v1.0/shares/{}/driveItem", item.share()), answer.to_string()),
    ] {
        Mock::given(method("GET"))
            .and(path(address.clone()))
            .and(header("prefer", "redeemSharingLink"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(address))
            .respond_with(link_refused())
            .mount(&server)
            .await;
    }
    let listed = client
        .read(&thumbnails)
        .await
        .expect("the thumbnails, the link redeemed");
    assert!(thumbnails.read_answer(&listed).is_some());
    let content = item.read_answer(&client.read(&item).await.expect("the item, the link redeemed"));
    assert_eq!(content.map(|content| content.size), Some(10));
    for request in server.received_requests().await.unwrap() {
        assert_eq!(
            request.headers.get("prefer").and_then(|value| value.to_str().ok()),
            Some("redeemSharingLink")
        );
    }
    assert!(tokens.audiences().iter().all(|audience| audience == GRAPH_AUDIENCE));
}

#[tokio::test]
async fn nothing_names_the_thumbnail_service_but_a_link_graph_handed_out() {
    struct OnThumbnails;
    impl Read for OnThumbnails {
        fn service(&self) -> Service {
            Service::FileThumbnail
        }
        fn endpoint(&self) -> Endpoint {
            Endpoint::new(["transform", "thumbnail"])
        }
    }
    let (server, tokens, client) = setup().await;
    match client.read(&OnThumbnails).await {
        Err(HttpError::Refused(Refusal::FileThumbnail)) => {}
        other => panic!("{other:?}"),
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(tokens.audiences().is_empty());
}

#[tokio::test]
async fn a_shared_files_bytes_are_read_by_ranges_at_the_link_graph_handed_out_with_no_token() {
    let (server, tokens, client) = setup().await;
    let read = FileDownload::of(SHARE).unwrap();
    let link = "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test/_layouts/15/download.aspx\
                ?UniqueId=made-up&tempauth=made-up";
    let item = serde_json::json!({"id": "made-up", "size": 10, "@microsoft.graph.downloadUrl": link});
    Mock::given(method("GET"))
        .and(path(format!("/v1.0/shares/{}/driveItem", read.share())))
        .respond_with(ResponseTemplate::new(200).set_body_raw(item.to_string(), "application/json"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/personal/made_up_user_contoso_test/_layouts/15/download.aspx"))
        .and(query_param("tempauth", "made-up"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-range", "bytes 4-7/10")
                .set_body_raw(b"4567".to_vec(), "video/mp4"),
        )
        .mount(&server)
        .await;
    let content = read.read_answer(&client.read(&read).await.unwrap()).unwrap();
    assert_eq!(content.size, 10);
    assert!(
        !format!("{content:?}").contains("tempauth"),
        "Debug shows nothing of the link"
    );
    let answer = client.transport().file_range(&content.link, 4, 4).await.unwrap();
    let part = FileRange::read(answer, 4, 4).unwrap();
    assert_eq!((part.bytes.as_slice(), part.total), (&b"4567"[..], 10));

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].headers.get("authorization").unwrap().to_str().unwrap(),
        "Bearer placeholder-graph-token-0"
    );
    assert_eq!(requests[1].headers.get("range").unwrap().to_str().unwrap(), "bytes=4-7");
    assert!(requests[1].headers.get("authorization").is_none());
    assert_eq!(tokens.audiences(), vec![GRAPH_AUDIENCE.to_owned()]);
    // nothing past a ranged read's limit, and nothing empty
    for len in [0, teams_proto::MAX_RANGE + 1] {
        match client.transport().file_range(&content.link, 0, len).await {
            Err(HttpError::Refused(Refusal::FileContent)) => {}
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn nothing_names_the_file_content_service_but_a_link_graph_handed_out() {
    struct OnContent;
    impl Read for OnContent {
        fn service(&self) -> Service {
            Service::FileContent
        }
        fn endpoint(&self) -> Endpoint {
            Endpoint::new(["download.aspx"])
        }
    }
    let (server, tokens, client) = setup().await;
    match client.read(&OnContent).await {
        Err(HttpError::Refused(Refusal::FileContent)) => {}
        other => panic!("{other:?}"),
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(tokens.audiences().is_empty());
}
