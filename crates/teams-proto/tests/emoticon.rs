//! A reaction's animation on Teams' CDN ([`ReactionAnimation`], `docs/protocol/chat.md`, "Reaction pictures") on
//! wiremock, through the `test-support` mock origin: a `GET` of the picker's `<id>/default/50_anim_f.png` or a call
//! reaction's `skype/v2/<id>V4-3D/<side>a.png` with no token, its answer kept only when it is a whole PNG of at most
//! `MAX_ANIMATION` bytes; an id that isn't one path segment of Teams' form, or a side the CDN has not, makes no read.
//!
//! The strip's bytes are made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::{Arc, Mutex, PoisonError};

use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_proto::{
    Client, Credentials, HttpError, HttpTransport, MAX_ANIMATION, ReactionAnimation, Read as _, Service,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
/// A PNG's signature and the image header chunk of a 1 px picture, with a made-up checksum.
const PNG_HEAD: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\0\0\0\0";
/// A PNG's end chunk, with a made-up checksum.
const PNG_END: &[u8] = b"\0\0\0\0IEND\0\0\0\0";
const STRIP_PATH: &str = "/evergreen-assets/personal-expressions/v2/assets/emoticons/1f440_eyes/default/50_anim_f.png";

/// Records every audience a token was asked for: the CDN's read asks for none.
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

/// A chunk of `kind` that holds `data`, with a made-up checksum.
fn chunk(kind: &[u8], data: &[u8]) -> Vec<u8> {
    let length = u32::try_from(data.len()).unwrap().to_be_bytes();
    [&length[..], kind, data, &[0; 4]].concat()
}

/// A made-up whole PNG: its image data chunk holds `data`.
fn png(data: &[u8]) -> Vec<u8> {
    [PNG_HEAD, &chunk(b"IDAT", data), PNG_END].concat()
}

/// A whole PNG one byte over [`MAX_ANIMATION`]: only the length limit refuses it.
fn oversized_png() -> Vec<u8> {
    let overhead = png(b"").len();
    png(&vec![0; MAX_ANIMATION + 1 - overhead])
}

async fn setup() -> (MockServer, Arc<FakeTokens>, Client<HttpTransport<FakeTokens>>) {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
    (server, tokens, Client::new(transport))
}

#[test]
fn only_an_id_of_teams_form_makes_a_read() {
    let read = ReactionAnimation::of("1f440_eyes").unwrap();
    assert_eq!(read.service(), Service::Emoticons);
    assert_eq!(
        read.endpoint().segments(),
        [
            "personal-expressions",
            "v2",
            "assets",
            "emoticons",
            "1f440_eyes",
            "default",
            "50_anim_f.png"
        ]
    );
    assert!(read.endpoint().query().is_empty());
    for id in ["", "../x", "a/b", "yes-tone2", "Eyes", "a.b", &"a".repeat(65)] {
        assert!(ReactionAnimation::of(id).is_none(), "{id:?}");
    }
}

#[test]
fn a_call_reaction_s_3d_emoji_is_read_by_the_cdn_s_name_and_a_side_it_has() {
    let segments = |id: &str, side: u16| {
        ReactionAnimation::emoji_3d(id, side)
            .unwrap()
            .endpoint()
            .segments()
            .to_vec()
    };
    assert_eq!(segments("heart", 112), ["skype", "v2", "heartV4-3D", "112a.png"]);
    assert_eq!(segments("like", 140), ["skype", "v2", "yesV4-3D", "140a.png"]);
    assert_eq!(segments("applause", 56), ["skype", "v2", "clapV4-3D", "56a.png"]);
    assert_eq!(segments("clap", 56), ["skype", "v2", "clapV4-3D", "56a.png"]);
    assert_eq!(segments("surprised", 56), ["skype", "v2", "surprisedV4-3D", "56a.png"]);
    let read = ReactionAnimation::emoji_3d("laugh", 56).unwrap();
    assert_eq!(read.service(), Service::Emoticons);
    assert!(read.endpoint().query().is_empty());
    for side in [0, 50, 64, 100, 141] {
        assert!(ReactionAnimation::emoji_3d("heart", side).is_none(), "{side}");
    }
    for id in ["", "../x", "Heart", "a/b"] {
        assert!(ReactionAnimation::emoji_3d(id, 112).is_none(), "{id:?}");
    }
    assert_eq!(read.key(), "laughV4-3D/56");
    assert_eq!(ReactionAnimation::of("laugh").unwrap().key(), "laugh");
    assert!(format!("{read:?}").contains("Emoji3d(56)"));
}

#[tokio::test]
async fn a_call_reaction_s_3d_emoji_is_a_get_with_no_token() {
    let (server, tokens, client) = setup().await;
    let sprite = png(b"made-up sprite");
    Mock::given(method("GET"))
        .and(path("/evergreen-assets/skype/v2/yesV4-3D/112a.png"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sprite.clone(), "image/png"))
        .mount(&server)
        .await;
    let read = ReactionAnimation::emoji_3d("like", 112).unwrap();
    assert_eq!(read.read_answer(&client.read(&read).await.unwrap()), Some(sprite));
    let request = server.received_requests().await.unwrap().pop().unwrap();
    assert!(request.headers.get("authorization").is_none());
    assert!(
        tokens
            .audiences
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty(),
        "no token asked for"
    );
}

#[tokio::test]
async fn the_strip_is_a_get_with_no_token_and_only_a_png_is_kept() {
    let (server, tokens, client) = setup().await;
    let strip = png(b"made-up strip");
    Mock::given(method("GET"))
        .and(path(STRIP_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(strip.clone(), "image/png"))
        .mount(&server)
        .await;
    let read = ReactionAnimation::of("1f440_eyes").unwrap();
    let answer = client.read(&read).await.unwrap();
    assert_eq!(read.read_answer(&answer), Some(strip));
    let request = server.received_requests().await.unwrap().pop().unwrap();
    assert!(request.headers.get("authorization").is_none());
    assert!(
        tokens
            .audiences
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty(),
        "no token asked for"
    );

    server.reset().await;
    Mock::given(method("GET"))
        .and(path(STRIP_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"<html/>".to_vec(), "text/html"))
        .mount(&server)
        .await;
    assert_eq!(read.read_answer(&client.read(&read).await.unwrap()), None, "not a PNG");

    server.reset().await;
    Mock::given(method("GET"))
        .and(path(STRIP_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"made-up text".to_vec(), "image/png"))
        .mount(&server)
        .await;
    assert_eq!(
        read.read_answer(&client.read(&read).await.unwrap()),
        None,
        "labelled a PNG, but not one"
    );

    server.reset().await;
    Mock::given(method("GET"))
        .and(path(STRIP_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(oversized_png(), "image/png"))
        .mount(&server)
        .await;
    assert_eq!(read.read_answer(&client.read(&read).await.unwrap()), None, "too large");

    server.reset().await;
    Mock::given(method("GET"))
        .and(path(STRIP_PATH))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    assert!(matches!(client.read(&read).await, Err(HttpError::Status(_))));
}

#[tokio::test]
async fn a_png_that_is_not_whole_is_not_kept() {
    let (server, _, client) = setup().await;
    let read = ReactionAnimation::of("1f440_eyes").unwrap();
    let no_picture = [&PNG_HEAD[..8], b"made-up text"].concat();
    let empty_picture = [
        &PNG_HEAD[..16],
        &[0; 4],
        &PNG_HEAD[20..],
        &chunk(b"IDAT", b"x"),
        PNG_END,
    ]
    .concat();
    let whole = png(b"made-up strip");
    for (body, why) in [
        (PNG_HEAD[..8].to_vec(), "a signature only"),
        (no_picture, "a signature with no image header"),
        (empty_picture, "a picture with no width"),
        (PNG_HEAD[..24].to_vec(), "a body that stops after the sides"),
        (PNG_HEAD.to_vec(), "an image header only"),
        ([PNG_HEAD, PNG_END].concat(), "no image data"),
        (whole[..whole.len() - PNG_END.len()].to_vec(), "no end chunk"),
        (whole[..whole.len() - 1].to_vec(), "an end chunk cut short"),
        ([&whole[..], b"x"].concat(), "bytes after the end chunk"),
        ([PNG_HEAD, &chunk(b"IDAT", b"x")[..6]].concat(), "image data cut short"),
        (
            [&PNG_HEAD[..8], &chunk(b"IDAT", b"x"), PNG_END].concat(),
            "image data before the image header",
        ),
    ] {
        server.reset().await;
        Mock::given(method("GET"))
            .and(path(STRIP_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "image/png"))
            .mount(&server)
            .await;
        assert_eq!(read.read_answer(&client.read(&read).await.unwrap()), None, "{why}");
    }
}
