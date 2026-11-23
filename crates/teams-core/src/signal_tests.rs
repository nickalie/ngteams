//! Signal's sticker packs: the keys and the decryption against known answers made outside Rust (Python's `hmac` and
//! `openssl enc`) from a synthetic key and text, the link's strict check, the manifest's protobuf, and the client
//! against the mock CDN (`teams_mock::signal`). Every key, id and byte here is made up.

use super::*;
use teams_mock::signal::{
    MockFault, MockPack, MockSticker, SYNTHETIC_PACK_ID, SYNTHETIC_PACK_KEY, SignalCdnMock, encrypt,
};

/// A made-up pack key: the bytes 0 to 31.
const KEY: [u8; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30,
    31,
];
/// HKDF-SHA256 of [`KEY`], a zero salt and `Sticker Pack`: its first and second 32 bytes.
const AES_KEY: &str = "64f46fd204462c27c05dd6f5db863edd9f286af100e5e9704efdd36ce89583ff";
const MAC_KEY: &str = "c483e5dfa58b86e9a7a2245b775677f5f6864b94f2302dcec6636f713de6de65";
/// [`PLAIN`] with the IV a0..af: IV | AES-256-CBC | HMAC-SHA256.
const BLOB: &str = "a0a1a2a3a4a5a6a7a8a9aaabacadaeafb34695c88a5497af2596105d336c7f10926aa6118dee1164fec707649d3c06a1\
                    7bc02649438955d629a6ab12210e3a0f0abb9a8e13eaed8e7cf85c738480ce69ca2fdea958c6e491a17842add4ece307";
const PLAIN: &[u8] = b"SYNTHETIC sticker bytes, made up for a test";

fn unhex(hex: &str) -> Vec<u8> {
    hex_bytes(hex).unwrap()
}

#[test]
fn the_keys_are_hkdf_of_the_pack_key_as_signal_derives_them() {
    let keys = PackKeys::derive(&KEY);
    assert_eq!(keys.cipher.as_slice(), unhex(AES_KEY));
    assert_eq!(keys.mac.as_slice(), unhex(MAC_KEY));
    assert_eq!(format!("{keys:?}"), "PackKeys(<keys>)", "never printed");
}

#[test]
fn a_blob_decrypts_to_its_known_answer() {
    assert_eq!(PackKeys::derive(&KEY).decrypt(&unhex(BLOB)).unwrap(), PLAIN);
}

#[test]
fn a_tampered_blob_fails_before_it_is_decrypted() {
    let keys = PackKeys::derive(&KEY);
    let blob = unhex(BLOB);
    for at in [0, 15, 16, 40, blob.len() - 33, blob.len() - 32, blob.len() - 1] {
        let mut changed = blob.clone();
        changed[at] ^= 0x01;
        assert_eq!(keys.decrypt(&changed), Err(SignalError::BadMac), "byte {at}");
    }
    let mut other = KEY;
    other[0] = 0xff;
    assert_eq!(
        PackKeys::derive(&other).decrypt(&blob),
        Err(SignalError::BadMac),
        "another key"
    );
    for short in [0, 31, 32 + 16, 32 + 16 + 15, blob.len() - 1] {
        assert_eq!(keys.decrypt(&blob[..short]), Err(SignalError::BadBlob), "{short} bytes");
    }
}

#[test]
fn a_blob_with_a_good_mac_and_bad_padding_is_refused() {
    let keys = PackKeys::derive(&KEY);
    let blob = unhex(BLOB);
    let mut body = blob[..blob.len() - 32].to_vec();
    // the last block's padding comes out of the block before it
    let last = body.len() - 17;
    body[last] ^= 0x55;
    let mut mac = Hmac::<Sha256>::new_from_slice(&keys.mac).unwrap();
    mac.update(&body);
    body.extend_from_slice(&mac.finalize().into_bytes());
    assert_eq!(keys.decrypt(&body), Err(SignalError::BadBlob));
}

const ID: &str = "0123456789abcdef0123456789abcdef";
const PACK_KEY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

#[test]
fn a_link_names_its_pack_in_each_form() {
    for link in [
        format!("https://signal.art/addstickers/#pack_id={ID}&pack_key={PACK_KEY}"),
        format!("  signal.art/addstickers/#pack_id={ID}&pack_key={PACK_KEY}\n"),
        format!("HTTPS://SIGNAL.ART/addstickers#pack_key={PACK_KEY}&pack_id={ID}"),
        format!(
            "http://www.signal.art/addstickers/#pack_id={}&pack_key={PACK_KEY}",
            ID.to_uppercase()
        ),
        format!("https://signal.art/addstickers/#pack_id={ID}&pack_key={PACK_KEY}&utm=1&"),
        format!("sgnl://addstickers/?pack_id={ID}&pack_key={PACK_KEY}"),
        format!("sgnl://addstickers?pack_id={ID}&pack_key={}", PACK_KEY.to_uppercase()),
    ] {
        let parsed = PackLink::parse(&link).unwrap_or_else(|| panic!("{link}"));
        assert_eq!(parsed.id(), ID, "{link}");
        assert_eq!(parsed.key.as_slice(), unhex(PACK_KEY), "{link}");
        assert!(!format!("{parsed:?}").contains(PACK_KEY), "the key is never printed");
    }
}

#[test]
fn a_link_is_checked_strictly() {
    let short_id = &ID[..31];
    let long_key = format!("{PACK_KEY}0");
    let odd_id = format!("{}g", &ID[..31]);
    for link in [
        String::new(),
        format!("https://example.test/addstickers/#pack_id={ID}&pack_key={PACK_KEY}"),
        format!("https://signal.art.example.test/addstickers/#pack_id={ID}&pack_key={PACK_KEY}"),
        format!("https://signal.art/addstickers/?pack_id={ID}&pack_key={PACK_KEY}"),
        format!("https://signal.art/other/#pack_id={ID}&pack_key={PACK_KEY}"),
        format!("ftp://signal.art/addstickers/#pack_id={ID}&pack_key={PACK_KEY}"),
        format!("https://signal.art/addstickers/#pack_id={short_id}&pack_key={PACK_KEY}"),
        format!("https://signal.art/addstickers/#pack_id={odd_id}&pack_key={PACK_KEY}"),
        format!("https://signal.art/addstickers/#pack_id={ID}&pack_key={long_key}"),
        format!(
            "https://signal.art/addstickers/#pack_id={ID}&pack_key={}",
            &PACK_KEY[..62]
        ),
        format!("https://signal.art/addstickers/#pack_id={ID}"),
        format!("https://signal.art/addstickers/#pack_key={PACK_KEY}"),
        format!("https://signal.art/addstickers/#pack_id={ID}&pack_id={ID}&pack_key={PACK_KEY}"),
        format!("https://signal.art/addstickers/#pack_id={ID}&pack_key"),
        format!(
            "https://signal.art/addstickers/#pack_id=+{}&pack_key={PACK_KEY}",
            &ID[1..]
        ),
        format!(
            "https://signal.art/addstickers/#pack_id={ID}&pack_key=ä{}",
            &PACK_KEY[2..]
        ),
        format!("sgnl://addstickers/#pack_id={ID}&pack_key={PACK_KEY}"),
        format!("sgnl://other/?pack_id={ID}&pack_key={PACK_KEY}"),
        ID.to_owned(),
    ] {
        assert_eq!(PackLink::parse(&link), None, "{link}");
    }
}

/// A protobuf field's key.
fn key(number: u8, wire: u8) -> u8 {
    number << 3 | wire
}

fn bytes_field(number: u8, bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![key(number, 2), u8::try_from(bytes.len()).unwrap()];
    out.extend_from_slice(bytes);
    out
}

fn sticker(id: u8, emoji: &str, kind: &str) -> Vec<u8> {
    let mut out = vec![key(1, 0)];
    if id < 0x80 {
        out.push(id);
    } else {
        out.extend([id | 0x80, 1]);
    }
    out.extend(bytes_field(2, emoji.as_bytes()));
    out.extend(bytes_field(3, kind.as_bytes()));
    out
}

#[test]
fn the_manifest_gives_the_title_author_cover_and_stickers() {
    let mut bytes = bytes_field(1, b"Made-up cats");
    bytes.extend(bytes_field(2, b"Made-up author\x07"));
    bytes.extend(bytes_field(3, &sticker(1, "\u{1f63a}", "image/webp")));
    // unknown fields of each wire type are passed over
    bytes.extend([key(9, 0), 0x96, 0x01]);
    bytes.extend([key(10, 1), 1, 2, 3, 4, 5, 6, 7, 8]);
    bytes.extend([key(11, 5), 1, 2, 3, 4]);
    bytes.extend(bytes_field(12, b"later"));
    bytes.extend(bytes_field(4, &sticker(0, "\u{1f638}", "image/png")));
    let mut second = sticker(1, "\u{1f63a}", "image/apng");
    second.extend(bytes_field(7, b"x"));
    bytes.extend(bytes_field(4, &second));
    bytes.extend(bytes_field(4, &sticker(1, "again", "image/png")));
    let manifest = parse_manifest(&bytes).unwrap();
    assert_eq!(manifest.title, "Made-up cats");
    assert_eq!(manifest.author, "Made-up author", "control characters dropped");
    assert_eq!(manifest.cover.as_ref().map(|cover| cover.id), Some(1));
    let stickers: Vec<(u32, &str, &str)> = manifest
        .stickers
        .iter()
        .map(|sticker| (sticker.id, sticker.emoji.as_str(), sticker.content_type.as_str()))
        .collect();
    assert_eq!(
        stickers,
        [(0, "\u{1f638}", "image/png"), (1, "\u{1f63a}", "image/apng")],
        "each id once"
    );
    assert_eq!(parse_manifest(&[]).unwrap(), Manifest::default());
}

#[test]
fn a_manifest_keeps_at_most_a_pack_of_stickers() {
    let mut bytes = Vec::new();
    for id in 0..=250_u8 {
        bytes.extend(bytes_field(4, &sticker(id, "x", "")));
    }
    // a long id, as a multi-byte varint
    bytes.extend(bytes_field(4, &[key(1, 0), 0xff, 0xff, 0xff, 0xff, 0x0f]));
    let manifest = parse_manifest(&bytes).unwrap();
    assert_eq!(manifest.stickers.len(), MAX_PACK_STICKERS);
    let long: String = "é".repeat(300);
    let manifest = parse_manifest(&[vec![key(1, 2), 0xd8, 0x04], long.into_bytes()].concat()).unwrap();
    assert_eq!(manifest.title.chars().count(), 200);
}

#[test]
fn a_manifest_that_is_not_signal_s_protobuf_is_refused() {
    let too_large_id = bytes_field(4, &[key(1, 0), 0xff, 0xff, 0xff, 0xff, 0x10]);
    for bytes in [
        vec![key(1, 0), 1],
        vec![key(4, 0), 1],
        vec![key(1, 2), 5, b'a'],
        vec![key(1, 3)],
        vec![key(1, 2), 0x80],
        vec![key(10, 1), 1, 2],
        vec![key(11, 5), 1],
        vec![0xff; 11],
        bytes_field(1, &[0xff, 0xfe]),
        bytes_field(4, &[key(2, 0), 1]),
        bytes_field(3, &[key(1, 2), 0]),
        too_large_id,
    ] {
        assert_eq!(parse_manifest(&bytes), Err(SignalError::BadManifest), "{bytes:?}");
    }
}

// --- the client, against the mock CDN ---

fn mock_pack() -> MockPack {
    MockPack::synthetic(
        "Made-up cats",
        vec![
            MockSticker {
                emoji: "\u{1f638}".to_owned(),
                content_type: "image/png".to_owned(),
                bytes: b"SYNTHETIC first sticker".to_vec(),
            },
            MockSticker {
                emoji: "\u{1f63a}".to_owned(),
                content_type: "image/webp".to_owned(),
                bytes: vec![7; 4000],
            },
        ],
    )
}

fn mock_link() -> PackLink {
    PackLink::parse(&mock_pack().link()).unwrap()
}

#[test]
fn the_mock_encrypts_as_signal_does() {
    let link = mock_link();
    assert_eq!(link.id(), SYNTHETIC_PACK_ID);
    let blob = encrypt(SYNTHETIC_PACK_KEY, 3, PLAIN);
    assert_eq!(link.keys().decrypt(&blob).unwrap(), PLAIN);
    let key: [u8; 32] = unhex(SYNTHETIC_PACK_KEY).try_into().unwrap();
    assert_eq!(PackKeys::derive(&key), link.keys());
}

#[test]
fn the_client_reads_a_pack_s_manifest_and_stickers_with_no_credential() {
    let mock = SignalCdnMock::start(vec![mock_pack()]).unwrap();
    let cdn = SignalCdn::with_mock_origin(&mock.origin()).unwrap();
    assert!(format!("{cdn:?}").contains("127.0.0.1"));
    let link = mock_link();
    let manifest = cdn.manifest(&link).unwrap();
    assert_eq!(
        (manifest.title.as_str(), manifest.author.as_str()),
        ("Made-up cats", "Made-up author")
    );
    assert_eq!(manifest.stickers.len(), 2);
    assert_eq!(manifest.cover.map(|cover| cover.emoji), Some("\u{1f638}".to_owned()));
    assert_eq!(cdn.sticker(&link, 0, 1000).unwrap(), b"SYNTHETIC first sticker");
    assert_eq!(
        cdn.sticker(&link, 1, 3999),
        Err(SignalError::TooLarge),
        "the plain size"
    );
    assert_eq!(
        cdn.sticker(&link, 1, 100),
        Err(SignalError::TooLarge),
        "the size the CDN gives"
    );
    assert_eq!(
        cdn.sticker(&link, 2, 1000),
        Err(SignalError::NoSuchPack),
        "no such sticker"
    );
    let targets: Vec<String> = mock.requests().iter().map(|request| request.target.clone()).collect();
    assert_eq!(
        targets[..2],
        [
            format!("/stickers/{SYNTHETIC_PACK_ID}/manifest.proto"),
            format!("/stickers/{SYNTHETIC_PACK_ID}/full/0"),
        ]
    );
    assert!(mock.requests().iter().all(|request| !request.credential));
    assert!(
        targets.iter().all(|target| !target.contains(SYNTHETIC_PACK_KEY)),
        "the key never leaves"
    );
}

#[test]
fn the_client_s_failures_are_categories() {
    let mock = SignalCdnMock::start(vec![mock_pack()]).unwrap();
    let cdn = SignalCdn::with_mock_origin(&format!("{}/", mock.origin())).unwrap();
    let link = mock_link();
    let unknown = PackLink::parse(&format!("signal.art/addstickers/#pack_id={ID}&pack_key={PACK_KEY}")).unwrap();
    assert_eq!(cdn.manifest(&unknown), Err(SignalError::NoSuchPack));
    let wrong_key = PackLink::parse(&format!(
        "signal.art/addstickers/#pack_id={SYNTHETIC_PACK_ID}&pack_key={PACK_KEY}"
    ))
    .unwrap();
    let wrong_key = PackLink {
        key: [9; 32],
        ..wrong_key
    };
    assert_eq!(cdn.manifest(&wrong_key), Err(SignalError::BadMac));
    for (fault, manifest, sticker) in [
        (MockFault::TamperedManifest, Err(SignalError::BadMac), Ok(())),
        (MockFault::TamperedSticker(0), Ok(()), Err(SignalError::BadMac)),
        (MockFault::MissingSticker(0), Ok(()), Err(SignalError::NoSuchPack)),
        (MockFault::DroppedFrom(0), Ok(()), Err(SignalError::Unreachable)),
        (
            MockFault::RedirectTo("https://example.test/".to_owned()),
            Err(SignalError::BadAnswer),
            Err(SignalError::BadAnswer),
        ),
        (
            MockFault::Broken,
            Err(SignalError::BadAnswer),
            Err(SignalError::BadAnswer),
        ),
        (
            MockFault::NotFound,
            Err(SignalError::BadAnswer),
            Err(SignalError::BadAnswer),
        ),
    ] {
        mock.fault(fault.clone());
        assert_eq!(cdn.manifest(&link).map(|_| ()), manifest, "{fault:?}");
        assert_eq!(cdn.sticker(&link, 0, 1000).map(|_| ()), sticker, "{fault:?}");
    }
    let dead = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let cdn = SignalCdn::with_mock_origin(&format!("http://{dead}")).unwrap();
    assert_eq!(cdn.manifest(&link), Err(SignalError::Unreachable));
}

#[test]
fn only_a_loopback_mock_or_signal_s_https_cdn_is_asked() {
    for origin in [
        "https://127.0.0.1:1",
        "http://example.test:1",
        "http://127.0.0.1",
        "http://127.0.0.1:1/path",
        "not a url",
    ] {
        assert_eq!(
            SignalCdn::with_mock_origin(origin).map(|_| ()),
            Err(SignalError::Unreachable),
            "{origin}"
        );
    }
    let cdn = SignalCdn::new().unwrap();
    assert_eq!(cdn.origin, CDN_ORIGIN);
    // an `http` origin is refused before anything is sent
    let mock = SignalCdnMock::start(vec![mock_pack()]).unwrap();
    let plain = SignalCdn {
        origin: mock.origin(),
        ..cdn
    };
    assert_eq!(plain.manifest(&mock_link()), Err(SignalError::BadAnswer));
    assert!(mock.requests().is_empty());
}
