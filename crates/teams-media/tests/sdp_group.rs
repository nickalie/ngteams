//! A group call's video lines in Teams' dialect (M3 Task 19), through the call's own media session (libwebrtc): the mixer's offer
//! (`sdp-ngc-0.5`, the captured shape) read with [`VideoLines::Group`], and our group offer.
//!
//! - The callee's answer takes the main video line and up to the slots' count of the other video lines both ways (as
//!   the web client answers them; nothing is sent on the receiving ones), each with reduced-size RTCP, its feedback for
//!   every payload type and the largest picture it takes (as the mixer takes them); it takes the sharing line the same
//!   way (someone's screen comes on it), rejects any line past the slots and keeps the data line; each receiving line's
//!   and the sharing line's stream id (`a=x-source-streamid`) is what a source request names it by, and the data line's
//!   is the client's own control plane source.
//! - The lines are described as the web client describes them: the main line `recvonly` while the camera is off,
//!   `sendrecv` while it is on, each receiving line `recvonly`.
//! - Our group offer (a caller's or a joiner's) has audio, the main line, the receiving lines, the sharing line last
//!   and the data line, as the web client's group offer.
//! - A receiving line goes both ways naming a stream of its own (never the placeholder `1`: the mixer refuses a line
//!   going both ways that names it, `410 SdpParsingFailure`), the same one in every offer and answer of the call.
//! - A receive-only line (a 1:1 call's main line while the camera is off) names the stream `1` and no media
//!   stream, as the web client's receive-only lines.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(
    dead_code,
    reason = "the helpers of tests/session.rs, of which these tests take the relay"
)]
mod support;

use serde_json::Value;
use teams_media::devices::VideoSink;
use teams_media::devices::file::FrameCounter;
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

/// A call's media session behind a loopback TURN server of its own (its only candidates relayed), its video started:
/// the camera's track on the main video line, off. The session goes before its relay.
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

fn value<'a>(section: &[&'a str], name: &str) -> Option<&'a str> {
    section.iter().find_map(|line| line.strip_prefix(&format!("a={name}:")))
}

/// Our answer to the captured mixer offer, read with `slots` receiving lines: the session and the answer's text.
fn mixer_answer(slots: usize) -> TestResult<(NgcSession, String)> {
    let mut session = NgcSession::new();
    session.set_video(VideoLines::Group { slots });
    let peer = Peer::new()?;
    let offer = session.read_offer(&captured("mixer_offer")?)?;
    let answer = peer.session.accept_offer(offer)?;
    let text = session.write_answer(&answer)?.as_str().to_owned();
    Ok((session, text))
}

/// A video line taken as the mixer takes it: both ways, as the web client answers them (described receive-only),
/// with reduced-size RTCP, the feedback for every payload type, its stream named and the largest picture it takes.
fn assert_taken(slot: &[&str]) {
    assert!(!slot[0].starts_with("m=video 0 "), "{slot:?}");
    assert!(slot.contains(&"a=sendrecv"), "{slot:?}");
    assert!(slot.contains(&"a=rtcp-rsize"), "{slot:?}");
    assert!(slot.contains(&"a=rtcp-fb:* nack pli"), "{slot:?}");
    let range = value(slot, "x-ssrc-range").unwrap_or_default();
    assert!(
        !range.is_empty() && range != "1-1",
        "a line going both ways names a stream of its own: {slot:?}"
    );
    let ssrc = range.split('-').next().unwrap_or_default();
    assert!(
        slot.iter()
            .any(|line| line.starts_with(&format!("a=ssrc:{ssrc} cname:"))),
        "its stream named by its range: {slot:?}"
    );
    assert!(
        value(slot, "fmtp").is_some_and(|fmtp| fmtp.contains("max-fs=3600")),
        "the largest picture taken: {slot:?}"
    );
}

#[test]
fn a_callee_takes_the_main_line_both_ways_and_the_slots_receiving() -> TestResult {
    let (session, text) = mixer_answer(9)?;
    let lines = sections(&text);
    // audio, the main line, nine receiving lines, the sharing line, the data line: as offered
    assert_eq!(lines.len(), 13, "{text}");
    let main = &lines[1];
    assert!(
        main[0].starts_with("m=video ") && !main[0].starts_with("m=video 0 "),
        "{main:?}"
    );
    assert_eq!(value(main, "mid"), Some("2"));
    assert!(main.contains(&"a=sendrecv"), "{main:?}");
    assert!(
        value(main, "x-ssrc-range").is_some(),
        "the main line's stream named: {main:?}"
    );
    for slot in &lines[1..11] {
        assert_taken(slot);
    }
    assert_eq!(value(&lines[11], "label"), Some("applicationsharing-video"));
    assert_taken(&lines[11]);
    assert!(
        lines[12][0].starts_with("m=x-data "),
        "the data line kept: {:?}",
        lines[12]
    );
    // the receiving lines by their stream ids, the data line's the control plane's source
    let slots = session.video_slots();
    assert_eq!(slots.len(), 9);
    assert_eq!(
        slots.iter().map(|(_, stream)| *stream).collect::<Vec<_>>(),
        (416..=424).map(Some).collect::<Vec<_>>()
    );
    assert_eq!(session.data_stream_id(), Some(426));
    assert_eq!(session.main_video_mid(), Some("2"));
    // the sharing line by its stream id, as a source request for someone's screen names it
    assert_eq!(session.sharing_slot().and_then(|(_, stream)| stream), Some(425));
    Ok(())
}

#[test]
fn fewer_slots_reject_the_lines_past_them() -> TestResult {
    let (session, text) = mixer_answer(3)?;
    let lines = sections(&text);
    let taken = lines[2..11]
        .iter()
        .filter(|slot| !slot[0].starts_with("m=video 0 "))
        .count();
    assert_eq!(taken, 3, "{text}");
    assert_eq!(session.video_slots().len(), 3);
    Ok(())
}

#[test]
fn the_lines_are_described_as_the_web_client_describes_them() -> TestResult {
    let (session, _) = mixer_answer(9)?;
    let off = session.group_descriptions(false);
    assert_eq!(off[0], ("2".to_owned(), "recvonly"));
    assert_eq!(off.len(), 10);
    assert!(off[1..].iter().all(|(_, direction)| *direction == "recvonly"));
    assert_eq!(session.group_descriptions(true)[0], ("2".to_owned(), "sendrecv"));
    Ok(())
}

#[test]
fn our_group_offer_is_the_web_client_s_kind() -> TestResult {
    let mut session = NgcSession::new();
    session.set_video(VideoLines::Group { slots: 9 });
    let peer = Peer::new()?;
    let offer = peer.session.offer_group(9)?;
    let text = session.write_offer(&offer)?.as_str().to_owned();
    let lines = sections(&text);
    assert_eq!(lines.len(), 13, "{text}");
    assert!(lines[0][0].starts_with("m=audio "));
    assert_eq!(value(&lines[1], "label"), Some("main-video"));
    assert!(lines[1].contains(&"a=sendrecv"));
    for slot in &lines[2..11] {
        assert_eq!(value(slot, "label"), Some("main-video"), "{slot:?}");
        assert!(slot.contains(&"a=sendrecv"), "{slot:?}");
    }
    assert_eq!(value(&lines[11], "label"), Some("applicationsharing-video"));
    assert!(lines[12][0].starts_with("m=x-data "), "{:?}", lines[12]);
    assert_eq!(
        session.video_slots().len(),
        9,
        "the receiving lines, the sharing line not among them"
    );
    // the sharing line known as the group call's own, its stream id the mixer's answer will name
    assert_eq!(
        session.sharing_slot().map(|(mid, _)| mid),
        value(&lines[11], "mid").map(str::to_owned)
    );
    // the descriptions name the offer's own mids: the main line, then each receiving line
    assert_eq!(session.main_video_mid(), value(&lines[1], "mid"));
    let described: Vec<String> = session
        .group_descriptions(true)
        .into_iter()
        .map(|(mid, _)| mid)
        .collect();
    let offered: Vec<String> = lines[1..11]
        .iter()
        .filter_map(|line| value(line, "mid"))
        .map(str::to_owned)
        .collect();
    assert_eq!(described, offered);
    Ok(())
}

/// The stream each receiving line of `text` names, in line order.
fn slot_streams(text: &str) -> Vec<String> {
    sections(text)[2..11]
        .iter()
        .filter_map(|slot| value(slot, "x-ssrc-range"))
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_receiving_line_keeps_a_stream_of_its_own_for_the_call() -> TestResult {
    // the callee's answer, then its answer to the mixer's renegotiation (the same offer again)
    let mut session = NgcSession::new();
    session.set_video(VideoLines::Group { slots: 9 });
    let peer = Peer::new()?;
    let offer = session.read_offer(&captured("mixer_offer")?)?;
    let first = session
        .write_answer(&peer.session.accept_offer(offer)?)?
        .as_str()
        .to_owned();
    let offer = session.read_offer(&captured("mixer_offer")?)?;
    let again = session
        .write_answer(&peer.session.accept_offer(offer)?)?
        .as_str()
        .to_owned();
    let streams = slot_streams(&first);
    assert_eq!(streams.len(), 9, "{first}");
    let mut distinct = streams.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 9, "each line a stream of its own: {streams:?}");
    assert_eq!(
        slot_streams(&again),
        streams,
        "the same streams in every answer of the call"
    );
    // and a caller's group offer's receiving lines, the same way
    let mut ours = NgcSession::new();
    ours.set_video(VideoLines::Group { slots: 9 });
    let caller = Peer::new()?;
    let text = ours.write_offer(&caller.session.offer_group(9)?)?.as_str().to_owned();
    for slot in &sections(&text)[2..11] {
        assert_own_stream(slot);
    }
    let offered = slot_streams(&text);
    let mut distinct = offered.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 9, "each offered line a stream of its own: {offered:?}");
    // a renegotiation's offer keeps them
    let again = ours.write_offer(&caller.session.offer_group(9)?)?.as_str().to_owned();
    assert_eq!(
        slot_streams(&again),
        offered,
        "the same streams in every offer of the call"
    );
    Ok(())
}

/// A line going both ways that names a stream of its own (its range and its `a=ssrc`), not the placeholder.
fn assert_own_stream(slot: &[&str]) {
    assert!(slot.contains(&"a=sendrecv"), "{slot:?}");
    let range = value(slot, "x-ssrc-range").unwrap_or_default();
    assert!(!range.is_empty() && range != "1-1", "{slot:?}");
    let ssrc = range.split('-').next().unwrap_or_default();
    assert!(
        slot.iter()
            .any(|line| line.starts_with(&format!("a=ssrc:{ssrc} cname:"))),
        "its stream named by its range: {slot:?}"
    );
}

#[test]
fn a_receive_only_line_names_the_stream_one_as_the_web_client_s() -> TestResult {
    // a 1:1 call's main line while the camera is off: receive-only
    let mut session = NgcSession::new();
    session.set_video(VideoLines::Main { sending: false });
    let peer = Peer::new()?;
    let offer = session.read_offer(&captured("caller_renegotiation_offer")?)?;
    let answer = peer.session.accept_offer(offer)?;
    let text = session.write_answer(&answer)?.as_str().to_owned();
    let lines = sections(&text);
    let video = lines
        .iter()
        .find(|section| section[0].starts_with("m=video ") && !section[0].starts_with("m=video 0 "))
        .ok_or("a video line")?;
    assert!(video.contains(&"a=recvonly"), "{video:?}");
    assert_eq!(value(video, "x-ssrc-range"), Some("1-1"));
    assert!(video.contains(&"a=ssrc:1 cname:teams-ngc"), "{video:?}");
    assert_eq!(value(video, "msid"), None, "no stream of ours: no media stream named");
    Ok(())
}
