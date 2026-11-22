//! A file uploaded into a channel's folder through the engine's requests, against wiremock as the chat service and the
//! team's SharePoint site (the transport's `test-support` mock origin): the folder read from the channel's thread, an
//! empty file of the name (numbered while the name is taken), its bytes, the end of the upload; a failed step
//! recycles the empty file; Remove recycles the file; a read-only app sends nothing.
//!
//! Tokens, ids, names and links here are made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::Arc;

use serde_json::json;
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts};
use teams_core::{EngineError, Outbox};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LAB_A: &str = "8:orgid:0000000a-0000-4000-8000-00000000000a";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const CHANNEL: &str = "19:madeupchannel000000000000000000@thread.tacv2";
const FOLDER: &str = "/sites/madeupteam/Shared Documents/General";
const FILE: &str = "00000000-0000-4000-8000-0000000000f1";

struct FakeTokens;

impl Credentials for FakeTokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(format!("placeholder-token-for-{audience}"))))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

fn engine(server: &MockServer, dir: &tempfile::TempDir, read_only: bool) -> EngineHandle {
    let tokens = Arc::new(FakeTokens);
    let transport = || {
        let made = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri())
            .unwrap()
            .for_account(Mri::new(LAB_A));
        if read_only { made.read_only() } else { made }
    };
    let client = Arc::new(Client::new(transport()));
    let outbox = Outbox::new(Arc::new(transport()), Author::new(Mri::new(LAB_A), "lab-a"));
    engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("a.sqlite")).unwrap(),
        client,
        outbox,
        push: None,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap()
}

fn sp_file(name: &str, length: usize) -> serde_json::Value {
    json!({"odata.type": "SP.File", "UniqueId": FILE, "Name": name, "Length": length.to_string(),
        "ServerRelativeUrl": format!("{FOLDER}/{name}")})
}

/// The channel's thread naming its folder, and the file read by id (the preflight's) right in it as `name`.
async fn channel(server: &MockServer, name: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/api/chatsvc/amer/v1/threads/{CHANNEL}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": CHANNEL, "properties": {
            "sharepointSiteUrl": "https://madeup.sharepoint.com/sites/madeupteam",
            "sharepointRootLibrary": "Shared Documents", "sharepointChannelDocsFolder": "General",
            "channelDocsFolderRelativeUrl": FOLDER}})),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/sites/madeupteam/_api/web/GetFileById('{FILE}')")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"UniqueId": FILE,
            "ServerRelativeUrl": format!("{FOLDER}/{name}")})))
        .mount(server)
        .await;
}

async fn stub_answers(server: &MockServer, name: &str, status: u16, body: serde_json::Value) {
    Mock::given(method("POST"))
        .and(path(
            "/sites/madeupteam/_api/web/GetFolderByServerRelativeUrl(@v)/Files/addStub(@fn)",
        ))
        .and(query_param("@fn", format!("'{name}'")))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

async fn step_answers(server: &MockServer, step: &str, status: u16, body: serde_json::Value) {
    Mock::given(method("POST"))
        .and(path_regex(format!(r"/GetFileById\('{FILE}'\)/{step}\(")))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

async fn recycles(server: &MockServer) {
    Mock::given(method("DELETE"))
        .and(path(format!(
            "/sites/madeupteam/_api/web/GetFileById('{FILE}')/recycle"
        )))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
}

/// The writes the server took, as `<method> <path's last part>`.
async fn writes(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.method.as_str() != "GET")
        .map(|request| {
            let last = request.url.path().rsplit('/').next().unwrap_or_default().to_owned();
            let step = last.split('(').next().unwrap_or_default().to_owned();
            format!("{} {step}", request.method)
        })
        .collect()
}

fn made_up_file(dir: &tempfile::TempDir, name: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, b"made-up channel notes").unwrap();
    path
}

#[tokio::test]
async fn a_file_goes_into_the_channels_folder_as_a_numbered_copy_when_its_name_is_taken() {
    let server = MockServer::start().await;
    channel(&server, "notes 1.txt").await;
    let taken = json!({"odata.error": {"code": "-2130575257, Microsoft.SharePoint.SPException",
        "message": {"lang": "en-US", "value": "A file with the name exists."}}});
    stub_answers(&server, "notes.txt", 400, taken).await;
    stub_answers(&server, "notes 1.txt", 200, sp_file("notes 1.txt", 0)).await;
    step_answers(&server, "StartUpload", 200, json!({"value": "21"})).await;
    step_answers(&server, "FinishUpload", 200, sp_file("notes 1.txt", 21)).await;
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(&server, &dir, false);
    let reply = teams_proto::reply_chain_id(CHANNEL, "1795000000000").unwrap();

    let file = handle
        .requests()
        .upload_channel_file(reply, made_up_file(&dir, "notes.txt"))
        .await
        .unwrap();

    assert_eq!(file.name(), "notes 1.txt");
    assert_eq!(file.size(), 21);
    assert_eq!(file.channel(), Some(CHANNEL));
    assert_eq!(
        file.entry()["objectUrl"],
        "https://madeup.sharepoint.com/sites/madeupteam/Shared Documents/General/notes 1.txt"
    );
    assert_eq!(
        writes(&server).await,
        ["POST addStub", "POST addStub", "POST StartUpload", "POST FinishUpload"]
    );
    let bytes = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|request| request.url.path().contains("StartUpload"))
        .unwrap()
        .body;
    assert_eq!(bytes, b"made-up channel notes");
}

#[tokio::test]
async fn a_failed_upload_recycles_the_empty_file_it_left() {
    let server = MockServer::start().await;
    channel(&server, "notes.txt").await;
    stub_answers(&server, "notes.txt", 200, sp_file("notes.txt", 0)).await;
    step_answers(&server, "StartUpload", 500, json!({})).await;
    recycles(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(&server, &dir, false);

    let error = handle
        .requests()
        .upload_channel_file(CHANNEL.to_owned(), made_up_file(&dir, "notes.txt"))
        .await
        .unwrap_err();

    assert!(
        matches!(&error, EngineError::Attachment { why } if why == "The file couldn't be uploaded. Try again."),
        "{error:?}"
    );
    assert_eq!(
        writes(&server).await,
        ["POST addStub", "POST StartUpload", "DELETE recycle"]
    );
}

#[tokio::test]
async fn a_removed_file_goes_to_the_sites_recycle_bin() {
    let server = MockServer::start().await;
    channel(&server, "notes.txt").await;
    stub_answers(&server, "notes.txt", 200, sp_file("notes.txt", 0)).await;
    step_answers(&server, "StartUpload", 200, json!({"value": "21"})).await;
    step_answers(&server, "FinishUpload", 200, sp_file("notes.txt", 21)).await;
    recycles(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(&server, &dir, false);
    let requests = handle.requests();
    let file = requests
        .upload_channel_file(CHANNEL.to_owned(), made_up_file(&dir, "notes.txt"))
        .await
        .unwrap();

    requests.discard_channel_file(file).await.unwrap();

    assert_eq!(writes(&server).await.last().map(String::as_str), Some("DELETE recycle"));
}

#[tokio::test]
async fn a_read_only_app_or_a_chat_uploads_nothing_to_a_channel() {
    let server = MockServer::start().await;
    channel(&server, "notes.txt").await;
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(&server, &dir, true);
    let read_only = handle
        .requests()
        .upload_channel_file(CHANNEL.to_owned(), made_up_file(&dir, "notes.txt"))
        .await
        .unwrap_err();
    assert!(matches!(&read_only, EngineError::Attachment { why } if why.contains("read-only")));
    let writing = engine(&server, &tempfile::tempdir().unwrap(), false);
    let chat = writing
        .requests()
        .upload_channel_file("19:made-up-group@thread.v2".to_owned(), made_up_file(&dir, "notes.txt"))
        .await
        .unwrap_err();
    assert!(matches!(chat, EngineError::Attachment { .. }));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_channel_naming_no_folder_of_its_own_takes_no_file() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/chatsvc/amer/v1/threads/{CHANNEL}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": CHANNEL, "properties": {}})))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(&server, &dir, false);
    let error = handle
        .requests()
        .upload_channel_file(CHANNEL.to_owned(), made_up_file(&dir, "notes.txt"))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, EngineError::Attachment { why } if why.contains("no files folder")),
        "{error:?}"
    );
    assert!(writes(&server).await.is_empty());
}
