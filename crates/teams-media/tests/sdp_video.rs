//! The video lines in Teams' dialect, through the call's own media session (libwebrtc: Opus and H.264): which of an
//! offer's video lines a call takes ([`VideoLines`]) and how our camera's line is offered.
//!
//! - A 1:1 call takes the main video line of the web client's camera-on offer and rejects the sharing line; with our
//!   camera off it answers that it only receives, with it on that it sends too; H.264 as the web client numbers
//!   constrained baseline.
//! - An audio-only call (a group call's, until it takes video) rejects every video line as the web client writes a
//!   rejected line (`m=video 0 RTP/SAVP 34`), the mixer's twelve too, and leaves them out of the bundle (M3 Task 16's
//!   `410 SdpParsingFailure`).
//! - Our camera's line offered in a renegotiation is written as the web client writes its main video line, and a
//!   native far end that reads it takes it and answers it. A line whose offer was turned away (`491`) is offered again
//!   as it was: the camera turned off and on again before it went again offers it numbered as it was.
//!
//! Each session runs behind a loopback TURN server of its own, its only candidates relayed.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(
    dead_code,
    reason = "the helpers of tests/session.rs, of which these tests take the relay"
)]
mod support;

use serde_json::Value;
use teams_media::devices::Devices as _;
use teams_media::devices::VideoSink;
use teams_media::devices::file::{AudioInput, FileDevices, FrameCounter, Pacing, PatternCamera};
use teams_media::processing::ProcessingConfig;
use teams_media::sdp::{NgcSdp, NgcSession, VideoLines};
use teams_media::session::{MediaSession, SessionConfig, VideoParts};
use teams_media::test_turn::TurnServer;
use teams_media::video::codec::EncoderSettings;
use teams_media::video::convert::MaxSize;

const FIXTURE: &str = include_str!("../../teams-sdp/tests/fixtures/sdp/captured.json");

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn captured(name: &str) -> TestResult<NgcSdp> {
    let fixture: Value = serde_json::from_str(FIXTURE)?;
    let content_type = fixture[name]["contentType"].as_str().ok_or("no content type")?;
    let blob = fixture[name]["blob"].as_str().ok_or("no blob")?;
    Ok(NgcSdp::new(content_type, blob)?)
}

/// A call's media session behind a loopback TURN server of its own, its video started (the camera's track on the
/// main video line, off). The session goes before its relay.
struct Peer {
    session: MediaSession,
    _turn: TurnServer,
}

impl Peer {
    fn new() -> TestResult<Self> {
        let turn = TurnServer::start()?;
        let config = SessionConfig {
            processing: ProcessingConfig::OFF,
        };
        let session = MediaSession::start_without_devices(&support::relay(&turn), config)?;
        session.start_video(VideoParts {
            encoder: EncoderSettings::default(),
            max_size: MaxSize::HD,
            remote: Box::new(|_| Box::new(FrameCounter::new()) as Box<dyn VideoSink>),
            preview: None,
            background: teams_media::video::effects::BackgroundChoice::default(),
        })?;
        Ok(Self { session, _turn: turn })
    }

    /// Turns the test pattern camera on.
    fn camera_on(&self) -> TestResult {
        let devices = FileDevices {
            input: AudioInput::Silence,
            recording: None,
            camera: PatternCamera::default(),
            pacing: Pacing::RealTime,
        };
        let camera = devices.camera()?;
        Ok(self.session.set_camera(Some(camera))?)
    }
}

/// The port of the first relayed candidate of a standard SDP.
fn relayed_port(text: &str) -> TestResult<String> {
    let candidate = text
        .lines()
        .find_map(|line| line.trim_end().strip_prefix("a=candidate:"))
        .ok_or("no candidate")?;
    Ok(candidate
        .split(' ')
        .nth(5)
        .ok_or("a candidate without a port")?
        .to_owned())
}

/// The media sections of an SDP, as lines.
fn sections(text: &str) -> Vec<Vec<&str>> {
    let mut out: Vec<Vec<&str>> = Vec::new();
    for line in text.split("\r\n").filter(|line| !line.is_empty()) {
        if line.starts_with("m=") {
            out.push(Vec::new());
        }
        if let Some(section) = out.last_mut() {
            section.push(line);
        }
    }
    out
}

fn has(section: &[&str], line: &str) -> bool {
    section.contains(&line)
}

/// Our answer, in Teams' dialect, to the captured offer `name` read with `lines`.
fn answer(name: &str, lines: VideoLines) -> TestResult<String> {
    let mut session = NgcSession::new();
    session.set_video(lines);
    let peer = Peer::new()?;
    let offer = session.read_offer(&captured(name)?)?;
    let answer = peer.session.accept_offer(offer)?;
    Ok(session.write_answer(&answer)?.as_str().to_owned())
}

#[test]
fn a_one_to_one_call_takes_the_main_video_line_receiving_while_the_camera_is_off() -> TestResult {
    let text = answer("caller_renegotiation_offer", VideoLines::Main { sending: false })?;
    let lines = sections(&text);
    let video = &lines[1];
    assert!(
        video[0].starts_with("m=video ") && !video[0].starts_with("m=video 0 "),
        "{video:?}"
    );
    // H.264 as constrained baseline, on the offer's own number for it (the web client's `108`, not ours)
    assert!(has(video, "a=rtpmap:108 H264/90000"), "{video:?}");
    assert!(
        video
            .iter()
            .any(|line| line.starts_with("a=fmtp:108 ") && line.contains("profile-level-id=42e01f")),
        "{video:?}"
    );
    assert!(has(video, "a=recvonly"), "{video:?}");
    assert!(has(video, "a=label:main-video"));
    assert_eq!(lines[2], ["m=video 0 RTP/SAVP 34", "a=label:applicationsharing-video"]);
    assert!(text.contains("a=group:BUNDLE 0 1 3"), "{text}");
    Ok(())
}

#[test]
fn with_the_camera_on_the_main_video_line_sends_too() -> TestResult {
    let text = answer("caller_renegotiation_offer", VideoLines::Main { sending: true })?;
    let video = &sections(&text)[1];
    assert!(has(video, "a=sendrecv"), "{video:?}");
    Ok(())
}

#[test]
fn an_audio_only_call_rejects_every_video_line() -> TestResult {
    let text = answer("caller_renegotiation_offer", VideoLines::None)?;
    let lines = sections(&text);
    assert_eq!(lines[1], ["m=video 0 RTP/SAVP 34", "a=label:main-video"]);
    assert_eq!(lines[2], ["m=video 0 RTP/SAVP 34", "a=label:applicationsharing-video"]);
    assert!(!text.contains("H264"), "{text}");
    assert!(text.contains("a=group:BUNDLE 0 3"), "{text}");
    Ok(())
}

#[test]
fn the_mixers_video_lines_stay_rejected_in_a_group_call() -> TestResult {
    let text = answer("mixer_offer", VideoLines::None)?;
    let lines = sections(&text);
    let video: Vec<&Vec<&str>> = lines
        .iter()
        .filter(|section| section[0].starts_with("m=video"))
        .collect();
    assert_eq!(video.len(), 11);
    for section in video {
        assert_eq!(section[0], "m=video 0 RTP/SAVP 34", "{section:?}");
        assert_eq!(section.len(), 2, "its label alone: {section:?}");
    }
    assert!(!text.contains("H264"), "{text}");
    assert!(lines[0][0].starts_with("m=audio ") && !lines[0][0].starts_with("m=audio 0 "));
    Ok(())
}

/// Our audio offer answered by a native far end whose camera is off: the two sessions and their dialects.
fn call() -> TestResult<(Peer, Peer, NgcSession, NgcSession)> {
    let (ours, theirs) = (Peer::new()?, Peer::new()?);
    let (mut our_dialect, mut their_dialect) = (NgcSession::new(), NgcSession::new());
    their_dialect.set_video(VideoLines::Main { sending: false });
    let offer = our_dialect.write_offer(&ours.session.offer_audio()?)?;
    let answer = theirs.session.accept_offer(their_dialect.read_offer(&offer)?)?;
    ours.session
        .accept_answer(our_dialect.read_answer(&their_dialect.write_answer(&answer)?)?)?;
    Ok((ours, theirs, our_dialect, their_dialect))
}

#[test]
fn our_cameras_line_is_offered_as_the_web_clients_main_video_line_and_a_native_far_end_takes_it() -> TestResult {
    let (ours, theirs, mut our_dialect, mut their_dialect) = call()?;

    // our camera goes on: the renegotiation adds our video line
    ours.camera_on()?;
    let standard = ours.session.offer_video(true)?.ok_or("an offer")?;
    let port = relayed_port(standard.as_str())?;
    let offer = our_dialect.write_offer(&standard)?;
    let lines = sections(offer.as_str());
    let video = &lines[1];
    assert!(video[0].starts_with(&format!("m=video {port} RTP/SAVP ")), "{video:?}");
    // H.264 alone (with its retransmissions), packetization mode 1, as the web client offers it
    let h264: Vec<&str> = video
        .iter()
        .filter_map(|line| line.strip_prefix("a=rtpmap:"))
        .filter(|rtpmap| rtpmap.ends_with(" H264/90000"))
        .collect();
    assert!(!h264.is_empty(), "{video:?}");
    assert!(
        video
            .iter()
            .filter_map(|line| line.strip_prefix("a=rtpmap:"))
            .all(|rtpmap| rtpmap.ends_with(" H264/90000") || rtpmap.ends_with(" rtx/90000")),
        "{video:?}"
    );
    assert!(has(video, "a=sendrecv"));
    assert!(has(video, "a=mid:1") && has(video, "a=label:main-video"), "{video:?}");
    assert!(
        !offer.as_str().contains("sdes:mid"),
        "no line-naming extension (M3 Task 16)"
    );

    // the far end (camera off) takes it receiving
    let answer = theirs.session.accept_offer(their_dialect.read_offer(&offer)?)?;
    let answer = their_dialect.write_answer(&answer)?;
    assert!(has(&sections(answer.as_str())[1], "a=recvonly"), "{}", answer.as_str());
    ours.session.accept_answer(our_dialect.read_answer(&answer)?)?;
    Ok(())
}

#[test]
fn a_camera_turned_off_and_on_while_its_offer_was_turned_away_offers_its_line_as_it_was() -> TestResult {
    let (ours, theirs, mut our_dialect, mut their_dialect) = call()?;

    // our camera goes on: the call controller turns its offer away (`491`), so no answer commits its line
    ours.camera_on()?;
    let offer = ours.session.offer_video(true)?.ok_or("an offer")?;
    let _ = our_dialect.write_offer(&offer)?;

    // off and on again before that offer went again: each offers the line again, numbered as the first
    ours.session.set_camera(None)?;
    let offer = ours.session.offer_video(false)?.ok_or("an offer")?;
    let _ = our_dialect.write_offer(&offer)?;
    ours.camera_on()?;
    let offer = ours.session.offer_video(true)?.ok_or("an offer")?;
    let offer = our_dialect.write_offer(&offer)?;
    let lines = sections(offer.as_str());
    assert_eq!(
        lines.len(),
        2,
        "one video line, however often it was offered: {lines:?}"
    );
    assert!(
        has(&lines[1], "a=mid:1") && has(&lines[1], "a=sendrecv"),
        "numbered as the first: {:?}",
        lines[1]
    );

    // the far end takes it, and the camera's next turn offers the same line
    let answer = theirs.session.accept_offer(their_dialect.read_offer(&offer)?)?;
    ours.session
        .accept_answer(our_dialect.read_answer(&their_dialect.write_answer(&answer)?)?)?;
    ours.session.set_camera(None)?;
    let offer = ours.session.offer_video(false)?.ok_or("an offer")?;
    let offer = our_dialect.write_offer(&offer)?;
    let lines = sections(offer.as_str());
    assert_eq!(lines.len(), 2);
    assert!(has(&lines[1], "a=mid:1"), "{:?}", lines[1]);
    Ok(())
}
