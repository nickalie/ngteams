//! An app's tile ([`AppPicture`], `docs/protocol/chat.md`, "App items") on wiremock, through the `test-support` mock
//! origin: the picture a definition's `largeImageUrl` names on Microsoft's CDN read as a `GET` of the same path and
//! version, with no token; its answer kept only when its bytes are a picture, whatever type the CDN says; a URL of any
//! other form makes no read.
//!
//! The app ids, versions and bytes are made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::{Arc, Mutex, PoisonError};

use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_proto::{
    APP_PICTURES_HOST, AppPicture, Client, Credentials, HttpError, HttpTransport, PastedFormat, Read as _,
    STATIC_APP_PICTURES_HOST, Service,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const APP: &str = "00000000-0000-4000-8000-00000000a002";

#[derive(Default)]
struct FakeTokens {
    audiences: Mutex<Vec<String>>,
}

impl Credentials for FakeTokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.audiences
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(audience.to_owned());
        future::ready(Ok(Secret::new("placeholder-token".to_owned())))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

fn evergreen() -> String {
    format!("https://{APP_PICTURES_HOST}/teamsappdata/evergreen-assets/apps/{APP}_largeImage.png?v=1.2.25")
}

#[test]
fn the_definition_s_cdn_forms_make_a_read_of_the_same_path_and_version() {
    let read = AppPicture::of(&evergreen()).unwrap();
    assert_eq!(read.service(), Service::AppPictures);
    let file = format!("{APP}_largeImage.png");
    assert_eq!(read.endpoint().segments(), ["evergreen-assets", "apps", file.as_str()]);
    assert_eq!(read.endpoint().query(), [("v".to_owned(), "1.2.25".to_owned())]);

    let preapproved = format!(
        "https://res.cdn.office.net/teamsappdata/ais_prod_v1/evergreen-assets/{APP}/v5madeup+a=/noop-{APP}/apps/\
         preapproved_global/{APP}/{APP}_largeimage.png"
    );
    let read = AppPicture::of(&preapproved).unwrap();
    assert_eq!(read.service(), Service::AppPictures);
    assert_eq!(read.endpoint().segments().len(), 9);
    assert_eq!(read.endpoint().segments()[3], "v5madeup+a=");
    assert!(read.endpoint().query().is_empty());

    for statics in [
        format!("https://{STATIC_APP_PICTURES_HOST}/app-assets/preapproved_global/{APP}/{APP}_largeImage.png?v=1.0.0"),
        format!("https://{STATIC_APP_PICTURES_HOST}/evergreen-assets/apps/{APP}_largeImage.png"),
    ] {
        let read = AppPicture::of(&statics).unwrap();
        assert_eq!(read.service(), Service::StaticAppPictures, "{statics}");
        assert_ne!(read.endpoint().segments()[0], "teamsappdata");
    }
    // the cache's key names the host, path and version
    assert_ne!(
        AppPicture::of(&evergreen()).unwrap().key(),
        AppPicture::of(&evergreen().replace("1.2.25", "1.2.26")).unwrap().key()
    );
}

#[test]
fn any_other_url_makes_no_read() {
    for other in [
        // another host, scheme, port, user or fragment
        format!("https://cdn.example.test/teamsappdata/evergreen-assets/apps/{APP}_largeImage.png"),
        format!("https://res.cdn.office.net.example.test/teamsappdata/apps/{APP}_largeImage.png"),
        format!("http://res.cdn.office.net/teamsappdata/apps/{APP}_largeImage.png"),
        format!("https://res.cdn.office.net:8443/teamsappdata/apps/{APP}_largeImage.png"),
        format!("https://user@res.cdn.office.net/teamsappdata/apps/{APP}_largeImage.png"),
        format!("https://res.cdn.office.net/teamsappdata/apps/{APP}_largeImage.png#x"),
        // another folder, or Teams' static CDN outside its app folders
        format!("https://res.cdn.office.net/other/apps/{APP}_largeImage.png"),
        "https://res.cdn.office.net/teamsappdata/".to_owned(),
        format!("https://{STATIC_APP_PICTURES_HOST}/evergreen-assets/personal-expressions/{APP}.png"),
        // not a picture's file, or an odd segment
        format!("https://res.cdn.office.net/teamsappdata/apps/{APP}_manifest.json"),
        "https://res.cdn.office.net/teamsappdata/apps/.png".to_owned(),
        "https://res.cdn.office.net/teamsappdata/apps/a%2Fb.png".to_owned(),
        "https://res.cdn.office.net/teamsappdata/apps/a%20b.png".to_owned(),
        format!("https://res.cdn.office.net/teamsappdata/{}/a.png", "a".repeat(129)),
        format!("https://res.cdn.office.net/teamsappdata/{}a.png", "a/".repeat(16)),
        // a query other than the version
        format!("https://res.cdn.office.net/teamsappdata/apps/{APP}_largeImage.png?v=1&x=2"),
        format!("https://res.cdn.office.net/teamsappdata/apps/{APP}_largeImage.png?x=1"),
        format!("https://res.cdn.office.net/teamsappdata/apps/{APP}_largeImage.png?v=a/b"),
        "not a url".to_owned(),
    ] {
        assert!(AppPicture::of(&other).is_none(), "{other}");
    }
}

#[tokio::test]
async fn the_tile_is_read_with_no_token_and_kept_only_when_it_is_a_picture() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
    let client = Client::new(transport);
    let tile_path = format!("/teamsappdata/evergreen-assets/apps/{APP}_largeImage.png");
    let png = b"\x89PNG\r\n\x1a\nmade-up tile".to_vec();
    // the CDN's own type for it
    Mock::given(method("GET"))
        .and(path(tile_path.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_raw(png.clone(), "application/octet-stream"))
        .mount(&server)
        .await;
    let read = AppPicture::of(&evergreen()).unwrap();
    let picture = read.read_answer(&client.read(&read).await.unwrap()).unwrap();
    assert_eq!((picture.format, picture.bytes), (PastedFormat::Png, png));
    assert!(
        tokens
            .audiences
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty(),
        "no token asked for"
    );
    let request = server.received_requests().await.unwrap().pop().unwrap();
    assert_eq!(request.url.query(), Some("v=1.2.25"));
    assert!(request.headers.get("authorization").is_none(), "no credential");
    assert!(
        request
            .headers
            .get("accept")
            .and_then(|accept| accept.to_str().ok())
            .is_some_and(|accept| accept.starts_with("image/"))
    );

    server.reset().await;
    Mock::given(method("GET"))
        .and(path(tile_path.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"<html></html>".to_vec(), "text/html"))
        .mount(&server)
        .await;
    assert_eq!(read.read_answer(&client.read(&read).await.unwrap()), None);

    server.reset().await;
    Mock::given(method("GET"))
        .and(path(tile_path.as_str()))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    assert!(matches!(client.read(&read).await, Err(HttpError::Status(_))));
}
