//! A meeting recording streamed through the engine, as the recap plays it: its DASH presentation read on its personal
//! site and its parts at the transform service's addresses, served only on loopback. A recording that can't be watched,
//! a presentation the player can't play and a part refused are unavailable; a read that failed on the way is unread.
//! Every link, id and byte here is made up.

#![cfg(test)]

use std::future::{self, Future};
use std::sync::Arc;

use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts};
use teams_core::{EngineError, Outbox};
use teams_mock::{CHAT_BASE, LAB_A};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SHARE: &str = "https://contoso-my.sharepoint.com/:v:/g/personal/made_up_user_contoso_test/MadeUpRecording01";
const MANIFEST: &str = "/personal/made_up_user_contoso_test/_api/v2.1/drives/b!madeup/items/ITEM1/content";
const PART: &str = "/transform/videotranscode";

const MPD: &str = r#"<MPD type="static" mediaPresentationDuration="PT0H0M12.000S" xmlns="urn:mpeg:DASH:schema:MPD:2011">
  <BaseURL>https://madeupregion1-mediap.svc.ms/transform/</BaseURL>
  <Period>
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <SegmentTemplate timescale="1000" initialization="videotranscode?part=initsegment&amp;quality=$RepresentationID$" media="videotranscode?part=mediasegment&amp;quality=$RepresentationID$&amp;segmentTime=$Time$">
        <SegmentTimeline><S t="0" d="6000" r="1" /></SegmentTimeline>
      </SegmentTemplate>
      <Representation id="vcopy" bandwidth="1" />
    </AdaptationSet>
  </Period>
</MPD>"#;

struct Tokens;

impl Credentials for Tokens {
    fn bearer(&self, _: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new("synthetic-site-token")))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

struct Rig {
    server: MockServer,
    handle: EngineHandle,
    _dir: tempfile::TempDir,
}

async fn running_engine() -> Rig {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let tokens = Arc::new(Tokens);
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
    let handle = engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("recording.sqlite")).unwrap(),
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
        _dir: dir,
    }
}

async fn manifest_answers(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(MANIFEST))
        .and(query_param("format", "dash"))
        .and(header("authorization", "Bearer synthetic-site-token"))
        .and(header("enablecdn", "0"))
        .respond_with(response)
        .up_to_n_times(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn a_recording_streams_as_its_presentation_and_parts() {
    let rig = running_engine().await;
    manifest_answers(
        &rig.server,
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/dash+xml")
            .set_body_string(MPD),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(PART))
        .and(query_param("part", "mediasegment"))
        .and(query_param("segmentTime", "6000"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"made-up part".to_vec()))
        .up_to_n_times(1)
        .mount(&rig.server)
        .await;
    let presentation = rig
        .handle
        .recording_presentation(SHARE, "b!madeup", "ITEM1")
        .await
        .unwrap();
    assert_eq!(presentation.duration_ms, 12_000);
    let track = &presentation.tracks[0];
    assert_eq!(track.segments.len(), 2);
    let part = track.segment(1).unwrap();
    assert_eq!(
        rig.handle.recording_segment(part.clone()).await.unwrap(),
        b"made-up part"
    );
    // a part refused (its address expired), then one whose read failed on the way
    for (status, unavailable) in [(403, true), (500, false)] {
        Mock::given(method("GET"))
            .and(path(PART))
            .respond_with(ResponseTemplate::new(status))
            .up_to_n_times(1)
            .mount(&rig.server)
            .await;
        let error = rig.handle.recording_segment(part.clone()).await.unwrap_err();
        assert_eq!(matches!(error, EngineError::FileUnavailable), unavailable, "{error:?}");
        assert_eq!(matches!(error, EngineError::FileUnread), !unavailable, "{error:?}");
    }
    rig.handle.shutdown();
}

#[tokio::test]
async fn a_recording_its_recap_names_no_item_of_streams_by_its_item_on_graph() {
    let rig = running_engine().await;
    // the short link of a recording shared with its attendees, and its item as Graph answers a redeemed read of it
    let link = "https://contoso-my.sharepoint.com/:v:/p/made_up_user_contoso_test/MadeUpRecording02";
    let share = teams_proto::FileDownload::of(link).unwrap();
    let item = serde_json::json!({"id": "ITEM1", "parentReference": {"driveId": "b!madeup"}, "size": 10});
    Mock::given(method("GET"))
        .and(path(format!("/v1.0/shares/{}/driveItem", share.share())))
        .and(header("prefer", "redeemSharingLink"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(item.to_string(), "application/json"))
        .up_to_n_times(1)
        .mount(&rig.server)
        .await;
    manifest_answers(
        &rig.server,
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/dash+xml")
            .set_body_string(MPD),
    )
    .await;
    let presentation = rig.handle.recording_presentation(link, "", "").await.unwrap();
    assert_eq!(presentation.duration_ms, 12_000);
    let graph_item = |body: String| {
        Mock::given(method("GET"))
            .and(path(format!("/v1.0/shares/{}/driveItem", share.share())))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .up_to_n_times(1)
            .expect(1)
    };
    // once its item reads, a presentation refused is unavailable (the player plays the file), one failed on the way unread
    for (status, unavailable) in [(403, true), (500, false)] {
        graph_item(item.to_string()).mount(&rig.server).await;
        manifest_answers(&rig.server, ResponseTemplate::new(status)).await;
        let error = rig.handle.recording_presentation(link, "", "").await.unwrap_err();
        assert_eq!(matches!(error, EngineError::FileUnavailable), unavailable, "{error:?}");
        assert_eq!(matches!(error, EngineError::FileUnread), !unavailable, "{error:?}");
    }
    // an item that names no drive can't be streamed, though its presentation would read
    graph_item(r#"{"id": "ITEM1"}"#.to_owned()).mount(&rig.server).await;
    manifest_answers(
        &rig.server,
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/dash+xml")
            .set_body_string(MPD),
    )
    .await;
    let error = rig
        .handle
        .recording_presentation(link, "b!madeup", "")
        .await
        .unwrap_err();
    assert!(matches!(error, EngineError::FileUnavailable), "{error:?}");
    rig.server.verify().await;
    let received = rig.server.received_requests().await.unwrap();
    assert_eq!(received.iter().filter(|asked| asked.url.path() == MANIFEST).count(), 3);
    rig.handle.shutdown();
}

#[tokio::test]
async fn a_recording_that_can_t_be_streamed_is_unavailable_and_a_failed_read_unread() {
    let rig = running_engine().await;
    // refused (an account that may not watch it), not an MPD, encrypted, then failed on the way
    manifest_answers(&rig.server, ResponseTemplate::new(403)).await;
    manifest_answers(&rig.server, ResponseTemplate::new(200).set_body_string("<html/>")).await;
    manifest_answers(
        &rig.server,
        ResponseTemplate::new(200).set_body_string(MPD.replace(
            "<SegmentTemplate",
            r#"<ContentProtection schemeIdUri="urn:mpeg:dash:sea:2012" /><SegmentTemplate"#,
        )),
    )
    .await;
    manifest_answers(&rig.server, ResponseTemplate::new(500)).await;
    for expected_unavailable in [true, true, true, false] {
        let error = rig
            .handle
            .recording_presentation(SHARE, "b!madeup", "ITEM1")
            .await
            .unwrap_err();
        assert_eq!(
            matches!(error, EngineError::FileUnavailable),
            expected_unavailable,
            "{error:?}"
        );
    }
    // a link off a personal site, or an item that isn't one, is never asked for
    for (link, item) in [("https://example.com/no-file", "ITEM1"), (SHARE, "ITEM/1")] {
        assert!(matches!(
            rig.handle.recording_presentation(link, "b!madeup", item).await,
            Err(EngineError::FileUnavailable)
        ));
    }
    let asked = rig.server.received_requests().await.unwrap().len();
    assert_eq!(asked, 4);
    rig.handle.shutdown();
}

#[tokio::test]
async fn a_transcript_s_language_is_read_from_its_item_s_transcripts() {
    let rig = running_engine().await;
    let location = "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test/_api/v2.1/drives/b!madeup/items/ITEM1/versions/current/media/transcripts/t1/content";
    Mock::given(method("GET"))
        .and(path(
            "/personal/made_up_user_contoso_test/_api/v2.1/drives/b!madeup/items/ITEM1",
        ))
        .and(query_param("select", "media/transcripts"))
        .and(header("authorization", "Bearer synthetic-site-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"media": {"transcripts": [{"id": "t1", "languageTag": "en-US", "isDefault": true}]}}),
        ))
        .up_to_n_times(1)
        .mount(&rig.server)
        .await;
    assert_eq!(
        rig.handle.transcript_language(location).await.unwrap().as_deref(),
        Some("en-US")
    );
    // refused, and an address the app doesn't read
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&rig.server)
        .await;
    for location in [location, "https://example.com/no-transcript"] {
        assert!(matches!(
            rig.handle.transcript_language(location).await,
            Err(EngineError::RecapUnread)
        ));
    }
    assert_eq!(rig.server.received_requests().await.unwrap().len(), 2);
    rig.handle.shutdown();
}

#[tokio::test]
async fn a_recording_s_file_says_whether_it_may_be_downloaded() {
    let rig = running_engine().await;
    let item = Mock::given(method("GET"))
        .and(path(
            "/personal/made_up_user_contoso_test/_api/v2.1/drives/b!madeup/items/ITEM1",
        ))
        .and(query_param("$select", "name,size,accessViewpoint"))
        .and(header("authorization", "Bearer synthetic-site-token"));
    item.respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "name": "Made up-20260101_100000-Meeting Recording.mp4",
        "size": 1234,
        "accessViewpoint": {"canRead": true, "canDownload": true}
    })))
    .up_to_n_times(1)
    .mount(&rig.server)
    .await;
    let file = rig.handle.recording_file(SHARE, "b!madeup", "ITEM1").await.unwrap();
    assert!(file.can_download);
    assert_eq!(file.size, Some(1234));
    // not an item, refused, failed on the way (the transport's retry too)
    for (answer, times) in [
        (ResponseTemplate::new(200).set_body_string("<html/>"), 1),
        (ResponseTemplate::new(403), 1),
        (ResponseTemplate::new(503), 2),
    ] {
        Mock::given(method("GET"))
            .respond_with(answer)
            .up_to_n_times(times)
            .mount(&rig.server)
            .await;
    }
    for unavailable in [true, true, false] {
        let error = rig.handle.recording_file(SHARE, "b!madeup", "ITEM1").await.unwrap_err();
        assert_eq!(matches!(error, EngineError::FileUnavailable), unavailable, "{error:?}");
    }
    // a link off a personal site is never asked for
    assert!(matches!(
        rig.handle
            .recording_file("https://example.com/no-file", "b!madeup", "ITEM1")
            .await,
        Err(EngineError::FileUnavailable)
    ));
    rig.handle.shutdown();
}

#[tokio::test]
async fn a_recording_s_expiry_is_read_from_its_item() {
    let rig = running_engine().await;
    Mock::given(method("GET"))
        .and(path(
            "/personal/made_up_user_contoso_test/_api/v2.1/drives/b!madeup/items/ITEM1",
        ))
        .and(query_param("$select", "expiration"))
        .and(header("authorization", "Bearer synthetic-site-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
            {"expiration": {"expirationDateTime": "2027-02-14T17:30:00Z", "isExpirationModifiable": true}}
        )))
        .up_to_n_times(1)
        .mount(&rig.server)
        .await;
    assert_eq!(
        rig.handle
            .recording_expiration(SHARE, "b!madeup", "ITEM1")
            .await
            .unwrap(),
        Some(1_802_626_200_000)
    );
    // refused, then failed on the way; a link off a personal site is never asked for
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .up_to_n_times(1)
        .mount(&rig.server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&rig.server)
        .await;
    assert!(matches!(
        rig.handle.recording_expiration(SHARE, "b!madeup", "ITEM1").await,
        Err(EngineError::FileUnavailable)
    ));
    assert!(matches!(
        rig.handle.recording_expiration(SHARE, "b!madeup", "ITEM1").await,
        Err(EngineError::FileUnread)
    ));
    assert!(matches!(
        rig.handle
            .recording_expiration("https://example.com/no-file", "b!madeup", "ITEM1")
            .await,
        Err(EngineError::FileUnavailable)
    ));
    rig.handle.shutdown();
}
