//! The loopback media peer (the `media` feature): a native session (`teams-media`, as the native client runs one) and
//! the peer on the loopback TURN server, their offer and answer carried in Teams' dialect both ways: the peer answers
//! the native offer (a call the account places), and the native side answers the peer's (a call the account is
//! invited to); either way both connect and each receives the other's audio.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::path::Path;
use std::time::{Duration, Instant};

use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::processing::ProcessingConfig;
use teams_media::sdp::{NgcSdp, NgcSession, SDP_NGC};
use teams_media::session::{MediaSession, SessionConfig, SessionEvent};
use teams_media::test_turn::TurnServer;
use teams_media::tone::Tone;
use teams_mock::media::LoopbackPeer;

/// How long ICE and DTLS may take through the relay on a loaded machine.
const CONNECT_WAIT: Duration = Duration::from_secs(20);
/// How long the media runs once connected.
const TALK: Duration = Duration::from_secs(2);

const NATIVE_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.5,
};
const PEER_TONE: Tone = Tone {
    hz: 660.0,
    amplitude: 0.5,
};

fn devices(tone: Tone, recording: &Path) -> FileDevices {
    FileDevices {
        input: AudioInput::Tone(tone),
        recording: Some(recording.to_path_buf()),
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    }
}

/// The native client's session relayed through `turn`.
fn native(turn: &TurnServer, devices: &FileDevices) -> MediaSession {
    let relay = teams_media::test_turn::relay(turn.udp);
    let config = SessionConfig {
        processing: ProcessingConfig::OFF,
        ..Default::default()
    };
    MediaSession::start(&relay, devices, config).unwrap()
}

fn connected(session: &MediaSession) {
    let limit = Instant::now() + CONNECT_WAIT;
    loop {
        let left = limit.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "the native session didn't connect");
        match session.next_event(left) {
            Some(SessionEvent::Connected) => return,
            Some(event @ (SessionEvent::Failed(_) | SessionEvent::Closed)) => panic!("{event:?}"),
            _ => {}
        }
    }
}

/// Each side received audio from the other once both connected.
fn both_heard(native: MediaSession, peer: LoopbackPeer) {
    connected(&native);
    assert!(
        peer.reports(&SessionEvent::Connected, CONNECT_WAIT),
        "the peer didn't connect"
    );
    std::thread::sleep(TALK);
    let (native, peer) = (native.close(), peer.close());
    assert!(
        native.received > 0 && native.frames_played > 0,
        "the native side heard nothing: {native:?}"
    );
    assert!(
        peer.received > 0 && peer.frames_played > 0,
        "the peer heard nothing: {peer:?}"
    );
}

#[test]
fn the_peer_answers_a_native_offer_in_teams_dialect() {
    let turn = TurnServer::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let ours = devices(NATIVE_TONE, &dir.path().join("native.wav"));
    let theirs = devices(PEER_TONE, &dir.path().join("peer.wav"));
    let session = native(&turn, &ours);
    let mut peer = LoopbackPeer::start(&turn, &theirs).unwrap();

    let mut dialect = NgcSession::new();
    let offer = dialect.write_offer(&session.offer_audio().unwrap()).unwrap();
    assert_eq!(offer.content_type(), SDP_NGC);
    let answer = peer.answer(&offer).unwrap();
    assert_eq!(answer.content_type(), SDP_NGC, "the peer answers in sdp-ngc-1.0");
    let carried = NgcSdp::new(answer.content_type(), answer.as_str()).unwrap();
    session.accept_answer(dialect.read_answer(&carried).unwrap()).unwrap();
    both_heard(session, peer);
}

#[test]
fn a_native_session_answers_the_peers_offer() {
    let turn = TurnServer::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let ours = devices(NATIVE_TONE, &dir.path().join("native.wav"));
    let theirs = devices(PEER_TONE, &dir.path().join("peer.wav"));
    let session = native(&turn, &ours);
    let mut peer = LoopbackPeer::start(&turn, &theirs).unwrap();

    let offer = peer.offer().unwrap();
    assert_eq!(offer.content_type(), SDP_NGC);
    let mut dialect = NgcSession::new();
    let carried = NgcSdp::new(offer.content_type(), offer.as_str()).unwrap();
    let answer = session.accept_offer(dialect.read_offer(&carried).unwrap()).unwrap();
    let answer = dialect.write_answer(&answer).unwrap();
    peer.accept_answer(&answer).unwrap();
    both_heard(session, peer);
}
