use teams_auth::secrets::Secret;
use teams_mock::telegram::{BotApiMock, MockFault, MockKind, MockSet, MockSticker, SYNTHETIC_BOT_TOKEN};

use super::{BotApi, BotError, file_path_form, set_name_form, token_form};

fn set() -> MockSet {
    MockSet {
        name: "SyntheticCats".to_owned(),
        title: "Synthetic cats".to_owned(),
        stickers: vec![
            MockSticker {
                kind: MockKind::Still,
                emoji: "😺".to_owned(),
                bytes: b"RIFF still".to_vec(),
            },
            MockSticker {
                kind: MockKind::Animated,
                emoji: "😸".to_owned(),
                bytes: vec![0x1f, 0x8b, 1, 2, 3],
            },
            MockSticker {
                kind: MockKind::Video,
                emoji: "😹".to_owned(),
                bytes: vec![0x1a, 0x45, 0xdf, 0xa3],
            },
        ],
    }
}

fn api(mock: &BotApiMock, token: &str) -> BotApi {
    BotApi::with_mock_origin(Secret::new(token), &mock.origin()).unwrap()
}

#[test]
fn a_pack_and_its_files_are_read_with_the_token_only_in_the_path() {
    let mock = BotApiMock::start(vec![set()]).unwrap();
    let api = api(&mock, SYNTHETIC_BOT_TOKEN);
    let pack = api.sticker_set("SyntheticCats").unwrap();
    assert_eq!(pack.title, "Synthetic cats");
    let kinds: Vec<(bool, bool)> = pack.stickers.iter().map(|s| (s.is_animated, s.is_video)).collect();
    assert_eq!(kinds, [(false, false), (true, false), (false, true)]);
    assert_eq!(pack.stickers[1].emoji.as_deref(), Some("😸"));
    assert_eq!(
        api.file(&pack.stickers[1].file_id, 1024).unwrap(),
        [0x1f, 0x8b, 1, 2, 3]
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert!(!request.authorization);
        let (path, query) = request.target.split_once('?').unwrap_or((&request.target, ""));
        assert!(path.contains(SYNTHETIC_BOT_TOKEN), "{path}");
        assert!(!query.contains(SYNTHETIC_BOT_TOKEN));
    }
    assert!(requests[2].target.starts_with("/file/bot"));
}

#[test]
fn a_bad_token_a_missing_pack_and_a_large_file_are_told_apart() {
    let mock = BotApiMock::start(vec![set()]).unwrap();
    let wrong = api(&mock, "123456789:SYNTHETIC_wrong_token_made_up_0000");
    assert_eq!(wrong.sticker_set("SyntheticCats"), Err(BotError::BadToken));
    let api = api(&mock, SYNTHETIC_BOT_TOKEN);
    assert_eq!(api.sticker_set("NoSuchPack"), Err(BotError::NoSuchSet));
    assert_eq!(api.sticker_set("bad name"), Err(BotError::NameForm));
    assert_eq!(api.file("SyntheticCats__1", 2), Err(BotError::TooLarge));
    assert_eq!(api.file("SyntheticCats__9", 1024), Err(BotError::BadAnswer));
    assert_eq!(api.file("bad/id", 1024), Err(BotError::BadAnswer));
    assert_eq!(api.file("", 1024), Err(BotError::BadAnswer));
}

#[test]
fn a_dead_origin_is_unreachable() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let api = BotApi::with_mock_origin(Secret::new(SYNTHETIC_BOT_TOKEN), &format!("http://127.0.0.1:{port}")).unwrap();
    assert_eq!(api.sticker_set("SyntheticCats"), Err(BotError::Unreachable));
}

#[test]
fn a_redirect_is_not_followed() {
    let elsewhere = BotApiMock::start(vec![set()]).unwrap();
    let mock = BotApiMock::start(vec![set()]).unwrap();
    mock.fault(MockFault::RedirectTo(format!(
        "{}/bot{SYNTHETIC_BOT_TOKEN}/getStickerSet?name=SyntheticCats",
        elsewhere.origin()
    )));
    let api = api(&mock, SYNTHETIC_BOT_TOKEN);
    assert_eq!(api.sticker_set("SyntheticCats"), Err(BotError::BadAnswer));
    assert_eq!(api.file("SyntheticCats__0", 1024), Err(BotError::BadAnswer));
    assert_eq!(mock.requests().len(), 2);
    assert!(elsewhere.requests().is_empty(), "the redirect's target gets nothing");
}

#[test]
fn telegram_s_client_sends_nothing_over_http() {
    let mock = BotApiMock::start(vec![set()]).unwrap();
    let api = BotApi::build(&mock.origin(), Secret::new(SYNTHETIC_BOT_TOKEN), true).unwrap();
    assert!(api.sticker_set("SyntheticCats").is_err());
    assert!(api.file("SyntheticCats__0", 1024).is_err());
    assert!(mock.requests().is_empty(), "{:?}", mock.requests());
}

#[test]
fn the_client_takes_only_a_token_of_its_form_and_the_mock_s_only_a_loopback_http_origin() {
    assert_eq!(BotApi::new(Secret::new("not a token")).err(), Some(BotError::TokenForm));
    let api = BotApi::new(Secret::new(SYNTHETIC_BOT_TOKEN)).unwrap();
    assert!(!format!("{api:?}").contains(SYNTHETIC_BOT_TOKEN));
    for origin in [
        "https://127.0.0.1:1",
        "http://example.test:1",
        "http://127.0.0.1",
        "nonsense",
    ] {
        assert_eq!(
            BotApi::with_mock_origin(Secret::new(SYNTHETIC_BOT_TOKEN), origin).err(),
            Some(BotError::Unreachable),
            "{origin}"
        );
    }
}

#[test]
fn the_forms_are_checked() {
    assert!(token_form(SYNTHETIC_BOT_TOKEN));
    for token in [
        "",
        "123",
        ":abc",
        "12a:aaaaaaaaaaaaaaaaaaaaaaaa",
        "1:short",
        "1:aaaaaaaaaaaaaaaaaaaaaa/aa",
    ] {
        assert!(!token_form(token), "{token}");
    }
    assert!(set_name_form("Synthetic_cats2"));
    for name in ["", "a b", "a/b", &"a".repeat(65)] {
        assert!(!set_name_form(name), "{name}");
    }
    assert!(file_path_form("stickers/file_1.webp"));
    for path in ["", "/etc", "a/../b", "a//b", "a?b", "./a", &"a".repeat(257)] {
        assert!(!file_path_form(path), "{path}");
    }
}
