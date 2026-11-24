//! A meeting recording's DASH presentation: where it is read (the item on its personal site, with that site's token
//! and the player's headers), how its MPD is read into tracks and part addresses, which it refuses, and a part read
//! with no token. The MPD is shaped like the transform service's (an audio and a video adaptation set, each a
//! `SegmentTemplate` with a `SegmentTimeline`); every id, host, signature and value here is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::{Arc, Mutex, PoisonError};

use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_proto::recording::{
    MEDIA_HOST_SUFFIX, RecordingExpiration, RecordingFile, RecordingManifest, SegmentTime, TrackKind, is_media_host,
    is_media_link, read_expiration, read_manifest, read_recording_file,
};
use teams_proto::{Credentials, HttpTransport, RecordingAccess};
use url::Url;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const SHARING_LINK: &str = "https://madeup-my.sharepoint.com/:v:/g/personal/lab_a/MadeUpShare1";

/// A presentation shaped like the transform service's, with `base` as its `BaseURL` and `extra` in its audio set.
fn mpd(base: &str, extra: &str) -> String {
    format!(
        r#"<?xml version="1.0"?>
<MPD type="static" maxSegmentDuration="PT6.000S" profiles="urn:mpeg:dash:profile:isoff-live:2011" mediaPresentationDuration="PT0H0M12.500S" minBufferTime="PT4.500S" xmlns="urn:mpeg:DASH:schema:MPD:2011">
  <BaseURL>{base}</BaseURL>
  <Period>
    <AdaptationSet id="1" contentType="video" mimeType="video/mp4" codecs="avc1.42c028">
      <SegmentTemplate timescale="16000" initialization="videotranscode?provider=Spo&amp;docId=MadeUpDoc&amp;part=initsegment&amp;track=video&amp;quality=$RepresentationID$" media="videotranscode?provider=Spo&amp;docId=MadeUpDoc&amp;part=mediasegment&amp;track=video&amp;quality=$RepresentationID$&amp;segmentTime=$Time$">
        <SegmentTimeline>
          <S d="96000" r="1" />
          <S d="8000" />
        </SegmentTimeline>
      </SegmentTemplate>
      <Representation id="vlow" width="640" height="360" bandwidth="100000" codecs="avc1.42c028" />
      <Representation id="vcopy" width="1920" height="1080" bandwidth="503106" codecs="avc1.42c028" />
    </AdaptationSet>
    <AdaptationSet id="0" contentType="audio" mimeType="audio/mp4" codecs="mp4a.40.2">
      {extra}
      <Label>OriginalAudio</Label>
      <SegmentTemplate timescale="16000" initialization="videotranscode?part=initsegment&amp;track=audio&amp;quality=$RepresentationID$" media="videotranscode?part=mediasegment&amp;track=audio&amp;quality=$RepresentationID$&amp;segmentTime=$Time$">
        <SegmentTimeline>
          <S t="0" d="96256" r="1" />
          <S d="7488" />
        </SegmentTimeline>
      </SegmentTemplate>
      <Representation id="audcopy" audioSamplingRate="16000" bandwidth="192000" codecs="mp4a.40.2" />
    </AdaptationSet>
  </Period>
</MPD>"#
    )
}

const MEDIA_BASE: &str = "https://madeupregion1-mediap.svc.ms/transform/";

#[test]
fn a_presentation_reads_into_its_audio_and_video_tracks_and_their_parts() {
    let presentation = read_manifest(mpd(MEDIA_BASE, "").as_bytes()).expect("a presentation");
    assert_eq!(presentation.duration_ms, 12_500);
    let kinds: Vec<TrackKind> = presentation.tracks.iter().map(|track| track.kind).collect();
    assert_eq!(kinds, [TrackKind::Audio, TrackKind::Video]);
    let video = &presentation.tracks[1];
    assert_eq!(video.timescale, 16_000);
    assert_eq!(
        video.segments,
        [
            SegmentTime {
                start: 0,
                duration: 96_000
            },
            SegmentTime {
                start: 96_000,
                duration: 96_000
            },
            SegmentTime {
                start: 192_000,
                duration: 8000
            },
        ]
    );
    assert_eq!(presentation.tracks[0].segments[2].start, 192_512);
    assert!(video.segment(2).is_some());
    assert!(video.segment(3).is_none());
    // a part's address never shows in a `Debug`
    let shown = format!("{:?} {:?} {video:?}", video.init(), video.segment(0));
    assert!(!shown.contains("MadeUpDoc") && !shown.contains("mediap"), "{shown}");
}

#[test]
fn the_length_is_the_longest_track_s_without_a_presentation_duration() {
    let text = mpd(MEDIA_BASE, "").replace(r#"mediaPresentationDuration="PT0H0M12.500S""#, "");
    let presentation = read_manifest(text.as_bytes()).expect("a presentation");
    // audio: 2 × 96256 + 7488 = 200000 at 16 kHz
    assert_eq!(presentation.duration_ms, 12_500);
    let text = mpd(MEDIA_BASE, "").replace("PT0H0M12.500S", "P1DT1H");
    assert_eq!(read_manifest(text.as_bytes()).unwrap().duration_ms, 90_000_000);
}

#[test]
fn a_presentation_the_player_cant_play_isnt_read() {
    let protected = r#"<ContentProtection schemeIdUri="urn:mpeg:dash:sea:2012" />"#;
    let refused = [
        // encrypted parts (the CDN's), whose key the app isn't given
        mpd(MEDIA_BASE, protected),
        // parts on SharePoint's cached API, which take the site's token
        mpd(
            "https://madeup-my.sharepoint.com/_api_cached/v2.1/drives/D/items/I/",
            "",
        ),
        // parts anywhere else
        mpd("https://media.example.test/transform/", ""),
        mpd("http://madeupregion1-mediap.svc.ms/transform/", ""),
        mpd("https://madeupregion1-mediap.svc.ms/other/", ""),
        // a live presentation
        mpd(MEDIA_BASE, "").replace(r#"type="static""#, r#"type="dynamic""#),
        // a template identifier this doesn't fill
        mpd(MEDIA_BASE, "").replace("segmentTime=$Time$", "n=$Number%05d$"),
        mpd(MEDIA_BASE, "").replace("segmentTime=$Time$", "b=$Bandwidth$"),
        // an open-ended repeat
        mpd(MEDIA_BASE, "").replace(r#"r="1""#, r#"r="-1""#),
        // a representation id that would change the address
        mpd(MEDIA_BASE, "")
            .replace(r#"id="audcopy""#, r#"id="a&amp;b""#)
            .replace(r#"id="vcopy""#, r#"id="../x""#)
            .replace(r#"id="vlow""#, r#"id="v?""#),
        "<NotAnMpd/>".to_owned(),
        "not xml".to_owned(),
    ];
    for text in &refused {
        assert!(read_manifest(text.as_bytes()).is_none(), "{text}");
    }
    assert!(read_manifest(&[0xff, 0xfe]).is_none());
}

#[test]
fn a_track_without_a_timeline_or_kind_is_left_out() {
    let text = mpd(MEDIA_BASE, "").replace(
        r#"contentType="video" mimeType="video/mp4""#,
        r#"contentType="text" mimeType="text/vtt""#,
    );
    let presentation = read_manifest(text.as_bytes()).expect("the audio alone");
    assert_eq!(presentation.tracks.len(), 1);
    assert_eq!(presentation.tracks[0].kind, TrackKind::Audio);
    let text = mpd(MEDIA_BASE, "").replace(r#"contentType="video" "#, "");
    assert_eq!(read_manifest(text.as_bytes()).unwrap().tracks.len(), 2);
    let numbered = mpd(MEDIA_BASE, "").replace("segmentTime=$Time$", "n=$Number$&amp;d=$$");
    let presentation = read_manifest(numbered.as_bytes()).expect("numbered parts");
    assert!(presentation.tracks[1].segment(1).is_some());
}

#[test]
fn the_presentation_is_read_on_the_recording_s_personal_site() {
    let manifest = RecordingManifest::of(SHARING_LINK, "b!drive1", "ITEM1").expect("a manifest");
    assert_eq!(
        manifest.url(),
        "https://madeup-my.sharepoint.com/personal/lab_a/_api/v2.1/drives/b!drive1/items/ITEM1/content?format=dash"
    );
    assert_eq!(manifest.site().audience(), "https://madeup-my.sharepoint.com");
    let on_site = "https://madeup-my.sharepoint.com/personal/lab_a/Documents/Recordings/x.mp4";
    assert_eq!(
        RecordingManifest::of(on_site, "b!drive1", "ITEM1"),
        Some(manifest.clone())
    );
    // the short form of a link for specific people names its owner's site the same way
    let for_people = "https://madeup-my.sharepoint.com/:v:/p/lab_a/MadeUpPeopleLink";
    assert_eq!(RecordingManifest::of(for_people, "b!drive1", "ITEM1"), Some(manifest));
    for (link, drive, item) in [
        ("https://madeup.sharepoint.com/:v:/g/sites/team/x", "b!drive1", "ITEM1"),
        (
            "https://madeup-my.sharepoint.com/:v:/x/personal/lab_a/x",
            "b!drive1",
            "ITEM1",
        ),
        ("https://example.test/:v:/g/personal/lab_a/x", "b!drive1", "ITEM1"),
        ("https://example.test/:v:/p/lab_a/x", "b!drive1", "ITEM1"),
        ("https://madeup-my.sharepoint.com/:v:/p/", "b!drive1", "ITEM1"),
        ("https://madeup-my.sharepoint.com/v/p/lab_a/x", "b!drive1", "ITEM1"),
        (SHARING_LINK, "b!drive/1", "ITEM1"),
        (SHARING_LINK, "b!drive1", "ITEM1?x"),
        (SHARING_LINK, "", "ITEM1"),
    ] {
        assert!(
            RecordingManifest::of(link, drive, item).is_none(),
            "{link} {drive} {item}"
        );
    }
}

#[test]
fn only_the_transform_service_s_parts_are_read() {
    assert!(is_media_host(&format!("westus1{MEDIA_HOST_SUFFIX}")));
    for host in [
        "mediap.svc.ms",
        "-mediap.svc.ms",
        "a.b-mediap.svc.ms",
        "evil.svc.ms",
        "x-mediap.svc.ms.example",
    ] {
        assert!(!is_media_host(host), "{host}");
    }
    let good = Url::parse("https://westus1-mediap.svc.ms/transform/videotranscode?a=1").unwrap();
    assert!(is_media_link(&good));
    for bad in [
        "https://user@westus1-mediap.svc.ms/transform/x",
        "https://westus1-mediap.svc.ms:444/transform/x",
        "https://westus1-mediap.svc.ms/transform/x#f",
        "https://westus1-mediap.svc.ms/thumbnail",
    ] {
        assert!(!is_media_link(&Url::parse(bad).unwrap()), "{bad}");
    }
    let long = format!("https://westus1-mediap.svc.ms/transform/x?{}", "a".repeat(20_000));
    assert!(!is_media_link(&Url::parse(&long).unwrap()));
}

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

#[tokio::test]
async fn the_manifest_goes_with_the_site_s_token_and_a_part_with_none() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/personal/lab_a/_api/v2.1/drives/b!drive1/items/ITEM1/content"))
        .and(query_param("format", "dash"))
        .and(header("authorization", "Bearer placeholder-token"))
        .and(header("prefer", "noredirect"))
        .and(header("enablecdn", "0"))
        .and(header("applymediaedits", "true"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/dash+xml")
                .set_body_string(mpd(MEDIA_BASE, "")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/transform/videotranscode"))
        .and(query_param("part", "mediasegment"))
        .and(query_param("segmentTime", "96000"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"made-up segment".to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    let manifest = RecordingManifest::of(SHARING_LINK, "b!drive1", "ITEM1").unwrap();
    let answer = transport.recording_manifest(&manifest).await.expect("the manifest");
    let presentation = read_manifest(answer.body()).expect("a presentation");
    let part = presentation.tracks[1].segment(1).unwrap();
    let answer = transport.media_segment(&part).await.expect("the part");
    assert_eq!(answer.body(), b"made-up segment");
    assert_eq!(
        tokens.audiences.lock().unwrap().as_slice(),
        ["https://madeup-my.sharepoint.com"]
    );
    let requests = server.received_requests().await.unwrap();
    let segment = requests
        .iter()
        .find(|request| request.url.path() == "/transform/videotranscode")
        .unwrap();
    assert!(segment.headers.get("authorization").is_none());
}

#[tokio::test]
async fn a_refused_manifest_is_the_service_s_answer() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let manifest = RecordingManifest::of(SHARING_LINK, "b!drive1", "ITEM1").unwrap();
    assert!(matches!(
        transport.recording_manifest(&manifest).await,
        Err(teams_proto::HttpError::Status(answer)) if answer.status() == 403
    ));
}

#[test]
fn the_recording_s_file_is_read_on_its_personal_site_with_what_the_recap_needs() {
    let file = RecordingFile::of(SHARING_LINK, "b!drive1", "ITEM1").expect("a file");
    assert_eq!(
        file.url(),
        "https://madeup-my.sharepoint.com/personal/lab_a/_api/v2.1/drives/b!drive1/items/ITEM1\
         ?$select=name,size,accessViewpoint"
    );
    assert_eq!(file.site().audience(), "https://madeup-my.sharepoint.com");
    assert!(RecordingFile::of("https://madeup.sharepoint.com/:v:/g/sites/team/x", "b!drive1", "ITEM1").is_none());
    assert!(RecordingFile::of(SHARING_LINK, "b!drive1", "ITEM1/../x").is_none());
}

#[test]
fn the_file_says_whether_the_account_may_download_it_its_name_and_size() {
    // shaped like the item's answer to the Recap player's read (made up)
    let organizer = br#"{"@odata.etag":"\"{X},1\"","name":"Weekly sync-20260101_100000-Meeting Recording.mp4",
        "size":4659019,"title":"Weekly sync","accessViewpoint":{"canRead":true,"canEdit":true,"canDelete":true,
        "canComment":true,"canDownload":true,"canManagePermissions":true}}"#;
    assert_eq!(
        read_recording_file(organizer),
        Some(RecordingAccess {
            can_download: true,
            name: Some("Weekly sync-20260101_100000-Meeting Recording.mp4".to_owned()),
            size: Some(4_659_019),
        })
    );
    // an attendee whose download is blocked
    let blocked = br#"{"name":"x.mp4","accessViewpoint":{"canRead":true,"canDownload":false}}"#;
    assert!(!read_recording_file(blocked).unwrap().can_download);
    // an answer that doesn't say may not download; a name that isn't a plain file name is left out
    let silent = br#"{"name":"../x.mp4","size":-1}"#;
    assert_eq!(
        read_recording_file(silent),
        Some(RecordingAccess {
            can_download: false,
            name: None,
            size: None,
        })
    );
    for name in ["", "  ", "..", "a\\b.mp4", "a\u{7}.mp4"] {
        let body = serde_json::json!({ "name": name }).to_string();
        assert_eq!(read_recording_file(body.as_bytes()).unwrap().name, None, "{name:?}");
    }
    let long = serde_json::json!({ "name": format!("{}.mp4", "a".repeat(400)) }).to_string();
    assert_eq!(read_recording_file(long.as_bytes()).unwrap().name, None);
    assert_eq!(read_recording_file(b"[]"), None);
    assert_eq!(read_recording_file(b"not json"), None);
}

#[tokio::test]
async fn the_file_is_read_with_the_site_s_token() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/personal/lab_a/_api/v2.1/drives/b!drive1/items/ITEM1"))
        .and(query_param("$select", "name,size,accessViewpoint"))
        .and(header("authorization", "Bearer placeholder-token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"name":"x.mp4","accessViewpoint":{"canDownload":true}}"#),
        )
        .expect(1)
        .mount(&server)
        .await;
    let file = RecordingFile::of(SHARING_LINK, "b!drive1", "ITEM1").unwrap();
    let answer = transport.recording_file(&file).await.expect("the item");
    assert!(read_recording_file(answer.body()).unwrap().can_download);
    assert_eq!(
        tokens.audiences.lock().unwrap().as_slice(),
        ["https://madeup-my.sharepoint.com"]
    );
}

#[tokio::test]
async fn a_recording_s_expiry_is_read_from_its_item_with_the_site_s_token() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/personal/lab_a/_api/v2.1/drives/b!drive1/items/ITEM1"))
        .and(query_param("$select", "expiration"))
        .and(header("authorization", "Bearer placeholder-token"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"@odata.etag":"made-up","expiration":{"expirationDateTime":"2027-02-14T09:30:00-08:00","isExpirationModifiable":true}}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let read = RecordingExpiration::of(SHARING_LINK, "b!drive1", "ITEM1").unwrap();
    let answer = transport.recording_expiration(&read).await.expect("the item");
    // 2027-02-14 17:30 UTC
    assert_eq!(read_expiration(answer.body()), Some(1_802_626_200_000));
    assert_eq!(
        tokens.audiences.lock().unwrap().as_slice(),
        ["https://madeup-my.sharepoint.com"]
    );
}

#[test]
fn an_expiry_is_read_only_as_an_instant() {
    assert_eq!(
        read_expiration(br#"{"expiration":{"expirationDateTime":"2027-02-14T17:30:00Z"}}"#),
        Some(1_802_626_200_000)
    );
    // a recording set never to expire names no date
    assert_eq!(read_expiration(br#"{"expiration":{"expirationDateTime":null}}"#), None);
    assert_eq!(read_expiration(br#"{"expiration":{}}"#), None);
    assert_eq!(
        read_expiration(br#"{"expiration":{"expirationDateTime":"in 120 days"}}"#),
        None
    );
    assert_eq!(read_expiration(b"not json"), None);
    // an item off a personal site, or an id that isn't one, is no read
    assert!(RecordingExpiration::of("https://example.com/video", "b!drive1", "ITEM1").is_none());
    assert!(RecordingExpiration::of(SHARING_LINK, "b!drive1", "../ITEM1").is_none());
}
