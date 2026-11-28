//! Shared files through the engine, with made-up sharing/download links and bytes served only on loopback. Missing
//! files, failed reads and expired links have different outcomes; a later read can recover without a poisoned cache.

#![cfg(test)]

use std::future::{self, Future};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts};
use teams_core::{EngineError, FileStream, Outbox};
use teams_mock::{CHAT_BASE, LAB_A};
use teams_proto::Mri;
use teams_proto::{Author, Client, Credentials, FileDownload, HttpTransport, MAX_RANGE, MAX_STREAM};
use teams_store::Store;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SHARE: &str =
    "https://contoso-my.sharepoint.com/:v:/g/personal/made_up_user_contoso_test/MadeUpCoreFileFailure01";
const DOWNLOAD: &str = "/personal/made_up_user_contoso_test/_layouts/15/download.aspx";
const BYTES: &[u8] = b"made-up file bytes";

struct Tokens(Arc<AtomicBool>);

impl Credentials for Tokens {
    fn bearer(&self, _: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(if self.0.load(Ordering::SeqCst) {
            Err(TokenError::UnexpectedResponse { status: 503 })
        } else {
            Ok(Secret::new("synthetic-file-token"))
        })
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

struct Rig {
    server: MockServer,
    handle: EngineHandle,
    failing: Arc<AtomicBool>,
    _dir: tempfile::TempDir,
}

async fn running_engine() -> Rig {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let failing = Arc::new(AtomicBool::new(false));
    let tokens = Arc::new(Tokens(Arc::clone(&failing)));
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
    let handle = engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("file.sqlite")).unwrap(),
        client: Arc::new(Client::new(http())),
        outbox: Outbox::new(Arc::new(http()), Author::new(Mri::new(LAB_A), "Lab A")),
        push: None,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap();
    Rig {
        server,
        handle,
        failing,
        _dir: dir,
    }
}

fn item_path() -> String {
    format!("/v1.0/shares/{}/driveItem", FileDownload::of(SHARE).unwrap().share())
}

fn item(size: u64) -> Value {
    json!({
        "id": "made-up-file", "size": size,
        "@microsoft.graph.downloadUrl": format!("https://contoso-my.sharepoint.com{DOWNLOAD}?UniqueId=made-up&tempauth=made-up"),
    })
}

async fn answer(server: &MockServer, endpoint: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(endpoint))
        .respond_with(response)
        .mount(server)
        .await;
}

async fn next_status(server: &MockServer, endpoint: &str, status: u16) {
    Mock::given(method("GET"))
        .and(path(endpoint))
        .respond_with(ResponseTemplate::new(status))
        .with_priority(1)
        .up_to_n_times(1)
        .mount(server)
        .await;
}

async fn readable_file(server: &MockServer) {
    answer(
        server,
        &item_path(),
        ResponseTemplate::new(200).set_body_json(item(BYTES.len() as u64)),
    )
    .await;
    answer(
        server,
        DOWNLOAD,
        ResponseTemplate::new(206)
            .insert_header("Content-Range", format!("bytes 0-{}/{}", BYTES.len() - 1, BYTES.len()))
            .set_body_bytes(BYTES),
    )
    .await;
}

async fn count(server: &MockServer, endpoint: &str) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == endpoint)
        .count()
}

async fn collect(stream: &mut FileStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    while let Some(piece) = stream.next().await {
        bytes.extend(piece.unwrap());
    }
    bytes
}

#[tokio::test]
async fn a_standin_stream_keeps_its_metadata_and_splits_the_bytes_even_for_a_zero_piece_size() {
    let mut stream = FileStream::of(7, BYTES, 100, 0);
    assert_eq!((stream.offset, stream.len, stream.total), (7, BYTES.len() as u64, 100));
    assert_eq!(collect(&mut stream).await, BYTES);
    assert!(stream.next().await.is_none());
    let mut empty = FileStream::of(9, &[], 100, 3);
    assert!(empty.next().await.is_none());
}

#[tokio::test]
async fn a_failed_item_is_not_cached_and_the_next_request_can_read_it() {
    let rig = running_engine().await;
    readable_file(&rig.server).await;
    for status in [404, 500] {
        next_status(&rig.server, &item_path(), status).await;
        let error = rig.handle.file_size(SHARE).await.unwrap_err();
        assert!(match status {
            404 => matches!(error, EngineError::FileUnavailable),
            _ => matches!(error, EngineError::FileUnread),
        });
    }
    rig.failing.store(true, Ordering::SeqCst);
    assert!(matches!(
        rig.handle.file_size(SHARE).await,
        Err(EngineError::FileUnread)
    ));
    rig.failing.store(false, Ordering::SeqCst);
    assert_eq!(rig.handle.file_size(SHARE).await.unwrap(), BYTES.len() as u64);
    assert_eq!(
        count(&rig.server, &item_path()).await,
        3,
        "only the successful item is cached"
    );
    assert!(matches!(
        rig.handle.file_size("https://example.com/no-file").await,
        Err(EngineError::FileUnavailable)
    ));
    rig.handle.shutdown();
}

#[tokio::test]
async fn an_item_without_a_usable_download_link_is_unavailable_and_can_be_read_again() {
    let rig = running_engine().await;
    answer(
        &rig.server,
        &item_path(),
        ResponseTemplate::new(200).set_body_json(json!({"size": 7})),
    )
    .await;
    for _ in 0..2 {
        assert!(matches!(
            rig.handle.file_range(SHARE, 0, 7).await,
            Err(EngineError::FileUnavailable)
        ));
    }
    assert_eq!(count(&rig.server, &item_path()).await, 2);
    rig.handle.shutdown();
}

#[tokio::test]
async fn ranges_distinguish_missing_parts_failed_reads_and_unusable_range_answers() {
    let rig = running_engine().await;
    readable_file(&rig.server).await;
    next_status(&rig.server, DOWNLOAD, 404).await;
    assert!(matches!(
        rig.handle
            .file_range(SHARE, 0, u32::try_from(BYTES.len()).unwrap())
            .await,
        Err(EngineError::FileUnavailable)
    ));
    next_status(&rig.server, DOWNLOAD, 500).await;
    assert!(matches!(
        rig.handle
            .file_range(SHARE, 0, u32::try_from(BYTES.len()).unwrap())
            .await,
        Err(EngineError::FileUnread)
    ));
    // A successful HTTP answer whose range differs from the request is unavailable too.
    assert!(matches!(
        rig.handle.file_range(SHARE, 1, 3).await,
        Err(EngineError::FileUnavailable)
    ));
    assert!(matches!(
        rig.handle.file_range(SHARE, 0, MAX_RANGE + 1).await,
        Err(EngineError::FileUnread)
    ));
    assert_eq!(
        rig.handle
            .file_range(SHARE, 0, u32::try_from(BYTES.len()).unwrap())
            .await
            .unwrap()
            .bytes,
        BYTES
    );
    assert_eq!(
        count(&rig.server, &item_path()).await,
        1,
        "a failed part does not discard a valid link"
    );
    rig.handle.shutdown();
}

#[tokio::test]
async fn an_expired_stream_link_is_refreshed_once_and_the_new_stream_reads_to_its_end() {
    let rig = running_engine().await;
    readable_file(&rig.server).await;
    next_status(&rig.server, DOWNLOAD, 403).await;
    let mut stream = rig.handle.file_stream(SHARE, 0, 1_000).await.unwrap();
    assert_eq!(stream.len, BYTES.len() as u64);
    assert_eq!(collect(&mut stream).await, BYTES);
    assert_eq!(count(&rig.server, &item_path()).await, 2);
    // Two refusals in one read exhaust the one refresh and make the file unavailable.
    Mock::given(method("GET"))
        .and(path(DOWNLOAD))
        .respond_with(ResponseTemplate::new(403))
        .with_priority(1)
        .up_to_n_times(2)
        .mount(&rig.server)
        .await;
    assert!(matches!(
        rig.handle.file_stream(SHARE, 0, 7).await,
        Err(EngineError::FileUnavailable)
    ));
    assert_eq!(count(&rig.server, &item_path()).await, 3);
    rig.handle.shutdown();
}

#[tokio::test]
async fn streams_distinguish_a_missing_file_failed_read_and_an_invalid_size() {
    let rig = running_engine().await;
    answer(
        &rig.server,
        &item_path(),
        ResponseTemplate::new(200).set_body_json(item(MAX_STREAM + 1)),
    )
    .await;
    for (status, missing) in [(404, true), (500, false)] {
        next_status(&rig.server, DOWNLOAD, status).await;
        let error = rig.handle.file_stream(SHARE, 0, 7).await.unwrap_err();
        assert!(if missing {
            matches!(error, EngineError::FileUnavailable)
        } else {
            matches!(error, EngineError::FileUnread)
        });
    }
    assert!(matches!(
        rig.handle.file_stream(SHARE, 0, MAX_STREAM + 1).await,
        Err(EngineError::FileUnavailable)
    ));
    rig.handle.shutdown();
}

#[tokio::test]
async fn an_abandoned_stream_request_does_not_wait_for_its_slow_item_to_finish() {
    let rig = running_engine().await;
    answer(
        &rig.server,
        &item_path(),
        ResponseTemplate::new(200)
            .set_delay(std::time::Duration::from_millis(150))
            .set_body_json(item(BYTES.len() as u64)),
    )
    .await;
    answer(
        &rig.server,
        DOWNLOAD,
        ResponseTemplate::new(206)
            .insert_header("Content-Range", format!("bytes 0-{}/{}", BYTES.len() - 1, BYTES.len()))
            .set_body_bytes(BYTES),
    )
    .await;
    let pending = rig.handle.file_stream(SHARE, 0, BYTES.len() as u64);
    // Ensure the stream request reached the engine and its link read started before seeking away.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while count(&rig.server, &item_path()).await == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(pending);
    assert_eq!(
        rig.handle.file_size(SHARE).await.unwrap(),
        BYTES.len() as u64,
        "the shared item read survives the seek"
    );
    assert_eq!(count(&rig.server, &item_path()).await, 1);
    assert_eq!(
        count(&rig.server, DOWNLOAD).await,
        0,
        "the abandoned stream does not download a body"
    );
    rig.handle.shutdown();
}

/// A made-up own address of a file in a person's OneDrive, as an entry with no sharing link names it.
const OBJECT: &str = "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test/Documents/Microsoft%20Teams%20Chat%20Files/made-up%20answers.md";

#[tokio::test]
async fn a_file_with_no_sharing_link_is_streamed_whole_by_its_own_address() {
    let rig = running_engine().await;
    let item_path = format!("/v1.0/shares/{}/driveItem", FileDownload::of(OBJECT).unwrap().share());
    answer(
        &rig.server,
        &item_path,
        ResponseTemplate::new(200).set_body_json(item(BYTES.len() as u64)),
    )
    .await;
    answer(
        &rig.server,
        DOWNLOAD,
        ResponseTemplate::new(206)
            .insert_header("Content-Range", format!("bytes 0-{}/{}", BYTES.len() - 1, BYTES.len()))
            .set_body_bytes(BYTES),
    )
    .await;
    let mut stream = rig.handle.file_stream(OBJECT, 0, BYTES.len() as u64).await.unwrap();
    assert_eq!(collect(&mut stream).await, BYTES);
    assert_eq!(count(&rig.server, &item_path).await, 1);
    let sent = rig.server.received_requests().await.unwrap();
    let download = sent.iter().find(|request| request.url.path() == DOWNLOAD).unwrap();
    assert!(
        !download.headers.contains_key("authorization"),
        "the download link carries its own grant: no token goes to it"
    );
    rig.handle.shutdown();
}

#[tokio::test]
async fn an_empty_file_streams_no_bytes_and_asks_its_host_for_none() {
    let rig = running_engine().await;
    answer(
        &rig.server,
        &item_path(),
        ResponseTemplate::new(200).set_body_json(item(0)),
    )
    .await;
    let mut stream = rig.handle.file_stream(SHARE, 0, 1_000).await.unwrap();
    assert_eq!((stream.offset, stream.len, stream.total), (0, 0, 0));
    assert!(collect(&mut stream).await.is_empty());
    assert_eq!(count(&rig.server, DOWNLOAD).await, 0);
    rig.handle.shutdown();
}
