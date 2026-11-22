//! A sticker from Teams' own packs ([`StickerPicture`], `docs/protocol/chat.md`, "Stickers") on wiremock, through the
//! `test-support` mock origin: the GIF or PNG a message's `<img>` names on Teams' sticker CDN read as a `GET` of
//! `evergreen-assets/stickerassets/<folder>[/<locale>]/<file>` from that CDN, with no token and no query; its answer
//! kept only when its signature is the format its file's name says; a `src` of any other form makes no read. The
//! picker's preview of a sticker ([`StickerPreview`]) is read the same way from `evergreen-assets/funstuff/`, and kept
//! only when it is a PNG.
//!
//! The pack folders, file names and bytes are made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::{Arc, Mutex, PoisonError};

use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_proto::{
    Client, Credentials, HttpError, HttpTransport, InlinePicture, MAX_BODY, PastedFormat, Read as _, STICKERS_HOST,
    Service, StickerPicture, StickerPreview,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";

/// A made-up sticker as a message names it, on the host the web client reads its stickers from.
const SRC: &str = "https://teams.public.onecdn.static.microsoft/evergreen-assets/stickerassets/madeup-250x250/en-us/MadeUp_Hi.gif?v=5";

/// Records every audience a token was asked for.
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

#[test]
fn only_a_gif_in_teams_sticker_assets_makes_a_read() {
    let read = StickerPicture::of(SRC).unwrap();
    assert_eq!(read.service(), Service::Stickers);
    assert_eq!(read.endpoint().segments(), ["madeup-250x250", "en-us", "MadeUp_Hi.gif"]);
    assert!(read.endpoint().query().is_empty(), "the query stays out");
    assert_eq!(
        (read.folder(), read.locale(), read.file()),
        ("madeup-250x250", Some("en-us"), "MadeUp_Hi.gif")
    );
    assert_eq!(InlinePicture::of(SRC), Some(InlinePicture::Sticker(read.clone())));
    // Teams' static CDN host serves the same files, read from the sticker host; a path with no locale too
    let statics =
        "https://statics.teams.cdn.office.net/evergreen-assets/stickerassets/madeup-250x250/en-us/MadeUp_Hi.gif";
    assert_eq!(StickerPicture::of(statics), Some(read));
    let plain = StickerPicture::of(
        "https://statics.teams.cdn.office.net/evergreen-assets/stickerassets/meme-500x500/Made-Up_1.gif?v=5",
    )
    .unwrap();
    assert_eq!(plain.endpoint().segments(), ["meme-500x500", "Made-Up_1.gif"]);
    assert_eq!(plain.locale(), None);
    // a still sticker is a PNG
    let still = StickerPicture::of(
        "https://teams.public.onecdn.static.microsoft/evergreen-assets/stickerassets/madeup-250x250/MadeUp_Still.png?v=5",
    )
    .unwrap();
    assert_eq!(still.endpoint().segments(), ["madeup-250x250", "MadeUp_Still.png"]);
    // a meme's picture with no caption can be a JPEG
    let jpeg = format!("https://{STICKERS_HOST}/evergreen-assets/stickerassets/meme-500x500/Made_template.jpg");
    assert!(StickerPicture::of(&jpeg).is_some());
    let base = "evergreen-assets/stickerassets";
    for other in [
        // another host, scheme, port, user or fragment
        format!("https://teams.public.onecdn.static.microsoft.example.test/{base}/a-250x250/A.gif"),
        format!("https://media.giphy.com/{base}/a-250x250/A.gif"),
        format!("http://{STICKERS_HOST}/{base}/a-250x250/A.gif"),
        format!("https://{STICKERS_HOST}:8443/{base}/a-250x250/A.gif"),
        format!("https://user@{STICKERS_HOST}/{base}/a-250x250/A.gif"),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/A.gif#x"),
        // another path, folder, locale or file
        format!("https://{STICKERS_HOST}/evergreen-assets/funstuff/stickers/en-us/assets/A/Preview/A.png"),
        format!("https://{STICKERS_HOST}/{base}/A.gif"),
        format!("https://{STICKERS_HOST}/{base}/A-250x250/A.gif"),
        format!("https://{STICKERS_HOST}/{base}/a_250/A.gif"),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/EN-US/A.gif"),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/e/A.gif"),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/en-us/x/A.gif"),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/A.webp"),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/A.PNG"),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/.gif"),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/A%20B.gif"),
        format!("https://{STICKERS_HOST}/{base}/../A.gif"),
        format!("https://{STICKERS_HOST}/{base}/{}/A.gif", "a".repeat(65)),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/{}/A.gif", "e".repeat(17)),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/{}.gif", "A".repeat(93)),
        "not a url".to_owned(),
    ] {
        assert!(StickerPicture::of(&other).is_none(), "{other}");
    }
}

#[tokio::test]
async fn the_sticker_is_read_from_its_cdn_with_no_token_and_only_a_gif_is_kept() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let client = Client::new(HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap());
    let gif_path = "/evergreen-assets/stickerassets/madeup-250x250/en-us/MadeUp_Hi.gif";
    // past the 4 MB a plain body may take: a sticker is read up to a picture's limit
    let mut gif = b"GIF89a made-up frames".to_vec();
    gif.resize(MAX_BODY + 1, 0);
    Mock::given(method("GET"))
        .and(path(gif_path))
        .respond_with(ResponseTemplate::new(200).set_body_raw(gif.clone(), "image/gif"))
        .mount(&server)
        .await;
    let read = InlinePicture::of(SRC).unwrap();
    let picture = read.read_answer(&client.read(&read).await.unwrap()).unwrap();
    assert_eq!((picture.format, picture.bytes.len()), (PastedFormat::Gif, gif.len()));
    assert!(
        tokens
            .audiences
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty(),
        "no token asked for"
    );
    let request = server.received_requests().await.unwrap().pop().unwrap();
    assert!(request.url.query().is_none(), "no query: {}", request.url);
    assert!(request.headers.get("authorization").is_none(), "no credential");
    assert_eq!(
        request.headers.get("accept").and_then(|accept| accept.to_str().ok()),
        Some("image/gif,image/png,image/jpeg")
    );

    server.reset().await;
    // a picture that isn't a GIF is none; a missing one an error
    Mock::given(method("GET"))
        .and(path(gif_path))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"\x89PNG\r\n\x1a\nmade-up".to_vec(), "image/png"))
        .mount(&server)
        .await;
    assert_eq!(read.read_answer(&client.read(&read).await.unwrap()), None);
    server.reset().await;
    Mock::given(method("GET"))
        .and(path(gif_path))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    assert!(matches!(client.read(&read).await, Err(HttpError::Status(_))));
}

/// A made-up sticker's preview and a made-up pack's header, as the picker names them.
const PREVIEW: &str = "https://teams.public.onecdn.static.microsoft/evergreen-assets/funstuff/stickers/en-us/assets/MadeUp/Preview/MadeUp_Hi.png?v=5";
const MEME_PREVIEW: &str =
    "https://statics.teams.cdn.office.net/evergreen-assets/funstuff/meme/assets/Preview/made-up_1.png";

#[test]
fn only_a_png_in_teams_sticker_previews_makes_a_preview_read() {
    let read = StickerPreview::of(PREVIEW).unwrap();
    assert_eq!(read.service(), Service::StickerPreviews);
    assert_eq!(
        read.endpoint().segments(),
        ["stickers", "en-us", "assets", "MadeUp", "Preview", "MadeUp_Hi.png"]
    );
    assert!(read.endpoint().query().is_empty(), "the query stays out");
    assert_eq!(read.path(), "stickers/en-us/assets/MadeUp/Preview/MadeUp_Hi.png");
    assert_eq!(InlinePicture::of(PREVIEW), Some(InlinePicture::StickerPreview(read)));
    let meme = StickerPreview::of(MEME_PREVIEW).unwrap();
    assert_eq!(
        meme.endpoint().segments(),
        ["meme", "assets", "Preview", "made-up_1.png"]
    );
    // a preview is no sticker, and a sticker no preview
    assert!(StickerPicture::of(PREVIEW).is_none());
    assert!(StickerPreview::of(SRC).is_none());
    let base = "evergreen-assets/funstuff";
    for other in [
        format!("https://teams.public.onecdn.static.microsoft.example.test/{base}/meme/assets/Preview/A.png"),
        format!("http://{STICKERS_HOST}/{base}/meme/assets/Preview/A.png"),
        format!("https://{STICKERS_HOST}:8443/{base}/meme/assets/Preview/A.png"),
        format!("https://user@{STICKERS_HOST}/{base}/meme/assets/Preview/A.png"),
        format!("https://{STICKERS_HOST}/{base}/meme/assets/Preview/A.png#x"),
        format!("https://{STICKERS_HOST}/{base}/meme/assets/Preview/A.gif"),
        format!("https://{STICKERS_HOST}/{base}/meme/assets/A.png"),
        format!("https://{STICKERS_HOST}/{base}/meme/Preview/A.png"),
        format!("https://{STICKERS_HOST}/{base}/stickers/en-us/assets/A/Preview/x/A.png"),
        format!("https://{STICKERS_HOST}/{base}/stickers/EN-US/assets/A/Preview/A.png"),
        format!("https://{STICKERS_HOST}/{base}/stickers/en-us/assets/A-B/Preview/A.png"),
        format!(
            "https://{STICKERS_HOST}/{base}/stickers/en-us/assets/{}/Preview/A.png",
            "A".repeat(65)
        ),
        format!("https://{STICKERS_HOST}/{base}/stickers/en-us/files/A/Preview/A.png"),
        format!("https://{STICKERS_HOST}/{base}/stickers/en-us/assets/A/preview/A.png"),
        format!("https://{STICKERS_HOST}/{base}/stickers/en-us/assets/A/Preview/A%20B.png"),
        format!("https://{STICKERS_HOST}/{base}/stickers/en-us/assets/A/Preview/.png"),
        format!("https://{STICKERS_HOST}/evergreen-assets/stickerassets/meme/assets/Preview/A.png"),
        "not a url".to_owned(),
    ] {
        assert!(StickerPreview::of(&other).is_none(), "{other}");
    }
}

#[tokio::test]
async fn a_preview_is_read_from_the_cdn_with_no_token_and_only_a_png_is_kept() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let client = Client::new(HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap());
    let preview_path = "/evergreen-assets/funstuff/stickers/en-us/assets/MadeUp/Preview/MadeUp_Hi.png";
    let png = b"\x89PNG\r\n\x1a\nmade-up preview".to_vec();
    Mock::given(method("GET"))
        .and(path(preview_path))
        .respond_with(ResponseTemplate::new(200).set_body_raw(png.clone(), "image/png"))
        .mount(&server)
        .await;
    let read = InlinePicture::of(PREVIEW).unwrap();
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
    assert!(request.url.query().is_none(), "no query: {}", request.url);
    assert!(request.headers.get("authorization").is_none(), "no credential");
    assert_eq!(
        request.headers.get("accept").and_then(|accept| accept.to_str().ok()),
        Some("image/png")
    );

    // the meme pack's previews are under their own path
    let meme = InlinePicture::of(MEME_PREVIEW).unwrap();
    Mock::given(method("GET"))
        .and(path("/evergreen-assets/funstuff/meme/assets/Preview/made-up_1.png"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"GIF89a made-up".to_vec(), "image/gif"))
        .mount(&server)
        .await;
    // a preview that isn't a PNG is none
    assert_eq!(meme.read_answer(&client.read(&meme).await.unwrap()), None);
}

#[tokio::test]
async fn a_still_sticker_is_kept_only_as_its_file_s_format() {
    let server = MockServer::start().await;
    let client = Client::new(
        HttpTransport::with_mock_origin(Arc::new(FakeTokens::default()), CHAT_BASE, &server.uri()).unwrap(),
    );
    let still = "https://teams.public.onecdn.static.microsoft/evergreen-assets/stickerassets/madeup-250x250/MadeUp_Still.png?v=5";
    let png = b"\x89PNG\r\n\x1a\nmade-up still".to_vec();
    Mock::given(method("GET"))
        .and(path("/evergreen-assets/stickerassets/madeup-250x250/MadeUp_Still.png"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(png.clone(), "image/png"))
        .mount(&server)
        .await;
    let read = InlinePicture::of(still).unwrap();
    let picture = read.read_answer(&client.read(&read).await.unwrap()).unwrap();
    assert_eq!((picture.format, picture.bytes), (PastedFormat::Png, png));
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/evergreen-assets/stickerassets/madeup-250x250/MadeUp_Still.png"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"GIF89a made-up".to_vec(), "image/gif"))
        .mount(&server)
        .await;
    assert_eq!(
        read.read_answer(&client.read(&read).await.unwrap()),
        None,
        "a GIF named .png is none"
    );
    let jpeg =
        "https://teams.public.onecdn.static.microsoft/evergreen-assets/stickerassets/meme-500x500/Made_template.jpg";
    Mock::given(method("GET"))
        .and(path("/evergreen-assets/stickerassets/meme-500x500/Made_template.jpg"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"\xff\xd8\xff\xe0made-up".to_vec(), "image/jpeg"))
        .mount(&server)
        .await;
    let read = InlinePicture::of(jpeg).unwrap();
    let picture = read.read_answer(&client.read(&read).await.unwrap()).unwrap();
    assert_eq!(picture.format, PastedFormat::Jpeg);
}
