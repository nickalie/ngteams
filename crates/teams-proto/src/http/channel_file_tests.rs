//! Handwritten protocol answers after a channel file's capture; made-up tenant, team, channel and file.
use std::sync::Arc;

use serde_json::{Value, json};
use teams_auth::{secrets::Secret, token::TokenError};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

use super::{Credentials, HttpError, HttpTransport, Refusal, Transport};
use crate::{ChannelFileWrite, ChannelFolder, Cloud, Mri};

const A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const CHANNEL: &str = "19:madeupchannel000000000000000000@thread.tacv2";
const SITE: &str = "https://madeup.sharepoint.com/sites/labteam";
const FOLDER: &str = "/sites/labteam/Shared Documents/General";
const FILE: &str = "00000000-0000-4000-8000-0000000000f1";

struct Tokens;
impl Credentials for Tokens {
    fn bearer(&self, audience: &str) -> impl std::future::Future<Output = Result<Secret, TokenError>> + Send {
        std::future::ready(Ok(Secret::new(format!("placeholder-for-{audience}"))))
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

fn thread(folder: &str) -> Value {
    json!({"id": CHANNEL, "properties": {"sharepointSiteUrl": SITE, "channelDocsFolderRelativeUrl": folder}})
}

fn folder() -> ChannelFolder {
    ChannelFolder::from_thread_in(&Cloud::Microsoft, CHANNEL, &thread(FOLDER)).unwrap()
}

async fn channel_says(server: &MockServer, folder: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/api/chatsvc/amer/v1/threads/{CHANNEL}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(thread(folder)))
        .mount(server)
        .await;
}

async fn file_is_at(server: &MockServer, at: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/sites/labteam/_api/web/GetFileById('{FILE}')")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"UniqueId": FILE, "ServerRelativeUrl": at})))
        .mount(server)
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

fn refused(answer: Option<&Result<super::Response, HttpError>>) -> bool {
    matches!(answer, Some(Err(HttpError::Refused(Refusal::ChannelFile))))
}

#[tokio::test]
async fn each_step_goes_to_the_channels_folder_with_the_sites_token() {
    let server = MockServer::start().await;
    channel_says(&server, FOLDER).await;
    file_is_at(&server, &format!("{FOLDER}/a.txt")).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": "3"})))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let transport = transport(&server);
    let folder = folder();
    let stub = ChannelFileWrite::add_stub(&folder, "a.txt").unwrap();
    assert_eq!(
        transport.deliver_channel_file(stub).await.unwrap().unwrap().status(),
        200
    );
    let start = ChannelFileWrite::start_upload(&folder, FILE, "a.txt", b"abc".to_vec()).unwrap();
    let finish = ChannelFileWrite::finish_upload(&start, "a.txt", 3).unwrap();
    assert_eq!(
        transport.deliver_channel_file(start).await.unwrap().unwrap().status(),
        200
    );
    assert_eq!(
        transport.deliver_channel_file(finish).await.unwrap().unwrap().status(),
        200
    );
    let recycle = ChannelFileWrite::recycle(&folder, FILE).unwrap();
    assert_eq!(
        transport.deliver_channel_file(recycle).await.unwrap().unwrap().status(),
        200
    );
    let sent = writes(&server).await;
    assert_eq!(sent.len(), 4);
    for request in &sent {
        assert_eq!(
            request.headers.get("authorization").unwrap(),
            "Bearer placeholder-for-https://madeup.sharepoint.com"
        );
        assert!(request.url.path().starts_with("/sites/labteam/_api/web/"));
    }
    assert_eq!(sent[1].body, b"abc");
    assert_eq!(sent[1].headers.get("content-type").unwrap(), "application/octet-stream");
    assert!(sent[0].headers.get("content-type").is_none());
    assert_eq!(sent[3].method.as_str(), "DELETE");
}

#[tokio::test]
async fn a_write_for_a_folder_the_channel_no_longer_names_is_never_sent() {
    let server = MockServer::start().await;
    channel_says(&server, "/sites/labteam/Shared Documents/Renamed").await;
    let transport = transport(&server);
    let stub = ChannelFileWrite::add_stub(&folder(), "a.txt").unwrap();
    assert!(refused(transport.deliver_channel_file(stub).await.as_ref()));
    assert!(writes(&server).await.is_empty());
}

#[tokio::test]
async fn a_write_naming_a_file_outside_the_folder_is_never_sent() {
    let server = MockServer::start().await;
    channel_says(&server, FOLDER).await;
    file_is_at(&server, "/sites/labteam/Shared Documents/Other/a.txt").await;
    let transport = transport(&server);
    let recycle = ChannelFileWrite::recycle(&folder(), FILE).unwrap();
    assert!(refused(transport.deliver_channel_file(recycle).await.as_ref()));
    let start = ChannelFileWrite::start_upload(&folder(), FILE, "a.txt", b"abc".to_vec()).unwrap();
    assert!(refused(transport.deliver_channel_file(start).await.as_ref()));
    assert!(writes(&server).await.is_empty());
}

#[tokio::test]
async fn a_read_only_transport_sends_no_channel_file_write() {
    let server = MockServer::start().await;
    channel_says(&server, FOLDER).await;
    let transport = transport(&server).read_only();
    let stub = ChannelFileWrite::add_stub(&folder(), "a.txt").unwrap();
    assert!(matches!(transport.deliver_channel_file(stub).await, Some(Err(_))));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn the_channel_folder_read_refuses_a_thread_naming_another_place() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/chatsvc/amer/v1/threads/{CHANNEL}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": CHANNEL, "properties": {
            "sharepointSiteUrl": "https://madeup-my.sharepoint.com/personal/someone",
            "channelDocsFolderRelativeUrl": "/personal/someone/Documents/x"}})),
        )
        .mount(&server)
        .await;
    let transport = transport(&server);
    assert!(matches!(
        transport.channel_folder(CHANNEL).await,
        Err(HttpError::Refused(Refusal::ChannelFile))
    ));
    assert_eq!(
        transport
            .channel_folder(CHANNEL)
            .await
            .err()
            .map(|error| error.to_string()),
        Some(HttpError::Refused(Refusal::ChannelFile).to_string())
    );
}
