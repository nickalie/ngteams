//! The pictures messages show, cached across runs, through two engines run one after the other on the same store,
//! against wiremock as AMS, Graph and SharePoint's media host (the transport's `test-support` mock origin): a
//! message's picture and a file's thumbnail read by the first run are shown by the second without a read; a picture
//! the service has none of isn't cached, so the next run reads it again; the cache is a private file beside the store.
//! A GIF and a link preview's picture are cached by what they are of; asks that come while a read runs wait for it; a
//! file Graph lists no thumbnail for has none; a cache that can't be opened leaves every run reading. An app's tile on
//! the CDN is read once per version and cached alike; a URL off the CDN's app folders is never read.
//!
//! Tokens, ids and links here are made up; the thumbnails' answer follows the synthetic fixture
//! (`tests/fixtures/graph/thumbnails.json`).

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts};
use teams_proto::Mri;
use teams_proto::{Author, Client, Credentials, FileThumbnails, HttpTransport, PastedFormat};
use teams_store::{PictureCache, Store};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[path = "../../teams-platform/tests/support/privacy.rs"]
mod privacy;

const LAB_A: &str = "8:orgid:0000000a-0000-4000-8000-00000000000a";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const AMS_BASE: &str = "https://us-prod.asyncgw.teams.microsoft.com";
const OBJECT: &str = "0-eus-d9-00000000000000000000000000000abc";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nmade-up picture";
const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
const THUMBNAILS: &str = include_str!("../../../tests/fixtures/graph/thumbnails.json");
const SHARE: &str = "https://contoso-my.sharepoint.com/:v:/g/personal/made_up_user_contoso_test/MadeUpShareLink0001";

fn src(view: &str) -> String {
    format!("https://us-api.asm.skype.com/v1/objects/{OBJECT}/views/{view}")
}

fn view_path(view: &str) -> String {
    format!("/v1/objects/{OBJECT}/views/{view}")
}

/// Placeholder tokens for every audience.
struct FakeTokens;

impl Credentials for FakeTokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(format!("placeholder-token-for-{audience}"))))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

/// An engine of lab account a on the store at `store_file`, its reads on `server`.
fn engine(server: &MockServer, store_file: &Path) -> EngineHandle {
    let tokens = Arc::new(FakeTokens);
    let transport = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
    let client = Arc::new(Client::new(transport().with_ams(AMS_BASE).unwrap()));
    let author = Author::new(Mri::new(LAB_A), "lab-a");
    let outbox = Outbox::new(Arc::new(transport()), author);
    engine::start(Parts {
        calls: None,
        store: Store::open(store_file).unwrap(),
        client,
        outbox,
        push: None,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap()
}

fn store_file(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("store").join("a.sqlite")
}

async fn requests_to(server: &MockServer, wanted: &str) -> usize {
    let requests = server.received_requests().await.unwrap();
    requests.iter().filter(|request| request.url.path() == wanted).count()
}

#[tokio::test]
async fn a_picture_read_once_is_shown_from_the_cache_by_the_next_run() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(view_path("imgo")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(PNG.to_vec(), "image/png"))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store_file = store_file(&dir);

    let first = engine(&server, &store_file);
    let read = first.requests().message_picture(src("imgo")).await.unwrap().unwrap();
    assert_eq!((read.format, read.bytes.as_slice()), (PastedFormat::Png, PNG));
    // the same run answers from memory
    let again = first.requests().message_picture(src("imgo")).await.unwrap().unwrap();
    assert_eq!(again.bytes, PNG);
    first.shutdown();
    assert_eq!(requests_to(&server, &view_path("imgo")).await, 1);

    let cache = PictureCache::file_beside(&store_file);
    privacy::assert_private(&cache, false);
    privacy::assert_private(cache.parent().unwrap(), false);

    // the next run: the same picture, by a `src` on another of AMS's hosts, without a read
    let second = engine(&server, &store_file);
    let other_host = src("imgo").replace("us-api.asm.skype.com", "us-prod.asyncgw.teams.microsoft.com");
    let cached = second.requests().message_picture(other_host).await.unwrap().unwrap();
    assert_eq!((cached.format, cached.bytes.as_slice()), (PastedFormat::Png, PNG));
    second.shutdown();
    assert_eq!(
        requests_to(&server, &view_path("imgo")).await,
        1,
        "no read in the second run"
    );
}

#[tokio::test]
async fn a_picture_the_service_has_none_of_is_read_again_by_the_next_run() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(view_path("imgpsh_fullsize")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store_file = store_file(&dir);
    for run in 1..=2 {
        let handle = engine(&server, &store_file);
        let answer = handle.requests().message_picture(src("imgpsh_fullsize")).await.unwrap();
        assert!(answer.is_none());
        handle.shutdown();
        assert_eq!(requests_to(&server, &view_path("imgpsh_fullsize")).await, run);
    }
}

#[tokio::test]
async fn a_files_thumbnail_read_once_is_shown_from_the_cache_by_the_next_run() {
    let server = MockServer::start().await;
    let share = FileThumbnails::of(SHARE).unwrap();
    let listed = format!("/v1.0/shares/{}/driveItem/thumbnails", share.share());
    let mut answer: Value = serde_json::from_str::<Value>(THUMBNAILS).unwrap()["answer"].clone();
    answer["value"][0]["large"]["url"] = Value::String(
        "https://westus1-mediap.svc.ms/transform/thumbnail?provider=spo&cs=made-up&docid=made-up&width=800".to_owned(),
    );
    Mock::given(method("GET"))
        .and(path(listed.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_raw(answer.to_string(), "application/json"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/transform/thumbnail"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(JPEG.to_vec(), "image/jpeg"))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store_file = store_file(&dir);

    let first = engine(&server, &store_file);
    let read = first.requests().file_thumbnail(SHARE).await.unwrap().unwrap();
    assert_eq!((read.format, read.bytes.as_slice()), (PastedFormat::Jpeg, JPEG));
    first.shutdown();

    let second = engine(&server, &store_file);
    let cached = second.requests().file_thumbnail(SHARE).await.unwrap().unwrap();
    assert_eq!(cached.bytes, JPEG);
    second.shutdown();
    assert_eq!(requests_to(&server, &listed).await, 1, "Graph read once");
    assert_eq!(
        requests_to(&server, "/transform/thumbnail").await,
        1,
        "the picture read once"
    );
}

/// A sharing link of `name`, on SharePoint's host as [`SHARE`] is.
fn share_of(name: &str) -> String {
    SHARE.replace("MadeUpShareLink0001", name)
}

/// Graph's thumbnails of `share` answered with `response`.
async fn thumbnails_answer(server: &MockServer, share: &str, response: ResponseTemplate) -> String {
    let listed = format!(
        "/v1.0/shares/{}/driveItem/thumbnails",
        FileThumbnails::of(share).unwrap().share()
    );
    Mock::given(method("GET"))
        .and(path(listed.as_str()))
        .respond_with(response)
        .mount(server)
        .await;
    listed
}

/// The fixture's thumbnails, the large one's link on the media host at `/transform/<picture>`.
fn listing(picture: &str) -> String {
    let mut answer: Value = serde_json::from_str::<Value>(THUMBNAILS).unwrap()["answer"].clone();
    answer["value"][0]["large"]["url"] = Value::String(format!(
        "https://westus1-mediap.svc.ms/transform/{picture}?provider=spo&cs=made-up&width=800"
    ));
    answer.to_string()
}

#[tokio::test]
async fn a_file_without_a_thumbnail_to_show_has_none() {
    let server = MockServer::start().await;
    let refused = thumbnails_answer(&server, &share_of("Refused0001"), ResponseTemplate::new(404)).await;
    let empty: Value = serde_json::from_str::<Value>(THUMBNAILS).unwrap()["empty"].clone();
    thumbnails_answer(
        &server,
        &share_of("Empty0001"),
        ResponseTemplate::new(200).set_body_raw(empty.to_string(), "application/json"),
    )
    .await;
    thumbnails_answer(
        &server,
        &share_of("Gone0001"),
        ResponseTemplate::new(200).set_body_raw(listing("gone"), "application/json"),
    )
    .await;
    thumbnails_answer(
        &server,
        &share_of("NotAPicture0001"),
        ResponseTemplate::new(200).set_body_raw(listing("text"), "application/json"),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/transform/gone"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/transform/text"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"made-up text".to_vec(), "text/plain"))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(&server, &store_file(&dir));
    let requests = handle.requests();

    // a link no read can name is answered at once
    assert!(
        requests
            .file_thumbnail("https://example.test/a-file")
            .await
            .unwrap()
            .is_none()
    );
    assert!(server.received_requests().await.unwrap().is_empty());
    // Graph refuses, lists none, or hands out a picture the media host has none of, or that isn't a picture
    for name in ["Refused0001", "Empty0001", "Gone0001", "NotAPicture0001"] {
        let answer = requests.file_thumbnail(share_of(name)).await.unwrap();
        assert!(answer.is_none(), "{name}");
    }
    // that there is none is kept for the run: no read again
    assert!(
        requests
            .file_thumbnail(share_of("Refused0001"))
            .await
            .unwrap()
            .is_none()
    );
    handle.shutdown();
    assert_eq!(requests_to(&server, &refused).await, 1);
    assert_eq!(requests_to(&server, "/transform/gone").await, 1);
    assert_eq!(requests_to(&server, "/transform/text").await, 1);
}

#[tokio::test]
async fn asks_for_a_thumbnail_while_it_is_read_all_wait_for_the_one_read() {
    let server = MockServer::start().await;
    let share = share_of("Slow0001");
    let listed = thumbnails_answer(
        &server,
        &share,
        ResponseTemplate::new(200)
            .set_body_raw(listing("slow"), "application/json")
            .set_delay(std::time::Duration::from_millis(300)),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/transform/slow"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(JPEG.to_vec(), "image/jpeg"))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(&server, &store_file(&dir));
    let requests = handle.requests();

    let (first, second) = tokio::join!(requests.file_thumbnail(&share), requests.file_thumbnail(&share));
    let (first, second) = (first.unwrap().unwrap(), second.unwrap().unwrap());
    // once read, the same run answers from memory
    let third = requests.file_thumbnail(&share).await.unwrap().unwrap();
    handle.shutdown();

    for picture in [&first, &second, &third] {
        assert_eq!((picture.format, picture.bytes.as_slice()), (PastedFormat::Jpeg, JPEG));
    }
    assert_eq!(requests_to(&server, &listed).await, 1, "one read for every ask");
    assert_eq!(requests_to(&server, "/transform/slow").await, 1);
}

#[tokio::test]
async fn without_a_usable_cache_every_run_reads_the_picture_and_concurrent_asks_share_a_read() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(view_path("imgo")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(PNG.to_vec(), "image/png")
                .set_delay(std::time::Duration::from_millis(200)),
        )
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store_file = store_file(&dir);
    // something that isn't a cache stands where the cache would be
    let cache = PictureCache::file_beside(&store_file);
    std::fs::create_dir_all(&cache).unwrap();

    for run in 1..=2 {
        let handle = engine(&server, &store_file);
        let requests = handle.requests();
        assert!(
            requests
                .message_picture("https://example.test/not-a-picture")
                .await
                .unwrap()
                .is_none()
        );
        let (first, second) = tokio::join!(
            requests.message_picture(src("imgo")),
            requests.message_picture(src("imgo"))
        );
        for picture in [first.unwrap().unwrap(), second.unwrap().unwrap()] {
            assert_eq!((picture.format, picture.bytes.as_slice()), (PastedFormat::Png, PNG));
        }
        handle.shutdown();
        assert_eq!(
            requests_to(&server, &view_path("imgo")).await,
            run,
            "one read a run, none from a cache"
        );
    }
    assert!(cache.is_dir(), "left as it was");
}

#[tokio::test]
async fn a_gif_and_a_link_preview_s_picture_are_cached_by_what_they_are_of() {
    const GIF: &[u8] = b"GIF89a made-up animation";
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/media/MadeUpGif01/giphy.gif"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(GIF.to_vec(), "image/gif"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/urlp/v1/url/image/Thumbnail"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(PNG.to_vec(), "image/png"))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store_file = store_file(&dir);
    let gif = "https://media2.giphy.com/media/v1.MadeUpToken/MadeUpGif01/giphy.gif";
    let preview = |site: &str| {
        format!(
            "https://us-prod.asyncgw.teams.microsoft.com/urlp/v1/url/image/Thumbnail?url=https%3A%2F%2F{site}%2Fpicture.png"
        )
    };

    let first = engine(&server, &store_file);
    let read = first.requests().message_picture(gif).await.unwrap().unwrap();
    assert_eq!((read.format, read.bytes.as_slice()), (PastedFormat::Gif, GIF));
    let read = first
        .requests()
        .message_picture(preview("example.test"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((read.format, read.bytes.as_slice()), (PastedFormat::Png, PNG));
    first.shutdown();

    // the next run: the same GIF on another of Giphy's hosts, and the same page's picture, from the cache
    let second = engine(&server, &store_file);
    let other_shard = gif.replace("media2.", "media.");
    assert_eq!(
        second
            .requests()
            .message_picture(other_shard)
            .await
            .unwrap()
            .unwrap()
            .bytes,
        GIF
    );
    assert_eq!(
        second
            .requests()
            .message_picture(preview("example.test"))
            .await
            .unwrap()
            .unwrap()
            .bytes,
        PNG
    );
    // another page's picture is another read
    assert!(
        second
            .requests()
            .message_picture(preview("example.org"))
            .await
            .unwrap()
            .is_some()
    );
    second.shutdown();
    assert_eq!(requests_to(&server, "/media/MadeUpGif01/giphy.gif").await, 1);
    assert_eq!(requests_to(&server, "/urlp/v1/url/image/Thumbnail").await, 2);
}

#[tokio::test]
async fn an_app_s_tile_is_read_once_cached_by_its_version_and_refused_off_the_cdn() {
    const TILE: &str = "/teamsappdata/evergreen-assets/apps/00000000-0000-4000-8000-00000000a002_largeImage.png";
    let url = |version: &str| format!("https://res.cdn.office.net{TILE}?v={version}");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(TILE))
        .respond_with(ResponseTemplate::new(200).set_body_raw(PNG.to_vec(), "application/octet-stream"))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store_file = store_file(&dir);

    let first = engine(&server, &store_file);
    let requests = first.requests();
    let (one, two) = tokio::join!(requests.app_picture(url("1")), requests.app_picture(url("1")));
    for tile in [one.unwrap().unwrap(), two.unwrap().unwrap()] {
        assert_eq!((tile.format, tile.bytes.as_slice()), (PastedFormat::Png, PNG));
    }
    // a host or a folder the CDN's app tiles don't use is never read
    for refused in [
        "https://cdn.example.test/teamsappdata/apps/made-up.png".to_owned(),
        "https://res.cdn.office.net/other/made-up.png".to_owned(),
        url("1").replace("https://", "http://"),
    ] {
        assert!(requests.app_picture(refused).await.unwrap().is_none());
    }
    first.shutdown();
    assert_eq!(requests_to(&server, TILE).await, 1, "one read for both asks");

    // the next run: the same version from the cache; a new version is read
    let second = engine(&server, &store_file);
    assert_eq!(
        second.requests().app_picture(url("1")).await.unwrap().unwrap().bytes,
        PNG
    );
    assert_eq!(requests_to(&server, TILE).await, 1, "no read in the second run");
    assert_eq!(
        second.requests().app_picture(url("2")).await.unwrap().unwrap().bytes,
        PNG
    );
    second.shutdown();
    assert_eq!(requests_to(&server, TILE).await, 2);
}

#[tokio::test]
async fn an_app_s_tile_that_isn_t_a_picture_is_none_and_a_failed_read_is_tried_again() {
    const TILE: &str = "/teamsappdata/evergreen-assets/apps/made-up_largeImage.png";
    let url = format!("https://res.cdn.office.net{TILE}");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(TILE))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"<html></html>".to_vec(), "text/html"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(&server, &store_file(&dir));
    assert!(handle.requests().app_picture(url.clone()).await.unwrap().is_none());
    // kept as none for the run: not read again
    assert!(handle.requests().app_picture(url.clone()).await.unwrap().is_none());
    handle.shutdown();
    assert_eq!(requests_to(&server, TILE).await, 1);

    // a read that fails on the way (the connection) is an error, and the next ask reads again
    // nothing listens on port 1
    let gone_uri = "http://127.0.0.1:1";
    let tokens = Arc::new(FakeTokens);
    let client = Arc::new(Client::new(
        HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, gone_uri).unwrap(),
    ));
    let author = Author::new(Mri::new(LAB_A), "lab-a");
    let outbox = Outbox::new(
        Arc::new(HttpTransport::with_mock_origin(tokens, CHAT_BASE, gone_uri).unwrap()),
        author,
    );
    let other = tempfile::tempdir().unwrap();
    let handle = engine::start(Parts {
        calls: None,
        store: Store::open(&store_file(&other)).unwrap(),
        client,
        outbox,
        push: None,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap();
    for _ in 0..2 {
        let answer = handle.requests().app_picture(url.clone()).await;
        assert!(
            matches!(answer, Err(teams_core::EngineError::PictureUnread)),
            "{:?}",
            answer.map(|tile| tile.map(|tile| tile.bytes.len()))
        );
    }
    handle.shutdown();
}
