//! `sdp-ngc-1.0` both ways, on the shapes the M3 calling runs captured (`crates/teams-sdp/tests/fixtures/sdp/captured.json`, synthetic):
//! libwebrtc's offer written as the web client writes its call start's and read back from the media controller's and a
//! callee's answers; an invitation's offer (the caller's, and the mixer's in `sdp-ngc-0.5`) read into libwebrtc and our
//! answer written; renegotiations offered by either side; a browser's SDP refused where the dialect is required.
//!
//! The standard side is a real media session (libwebrtc) behind a loopback TURN server of its own, as the native
//! client's is behind Teams' relay: its only candidates are relayed.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(
    dead_code,
    reason = "the helpers of tests/session.rs, of which these tests take the relay"
)]
mod support;

use std::ops::Deref;
use std::sync::LazyLock;

use serde_json::Value;
use teams_media::devices::VideoSink;
use teams_media::devices::file::FrameCounter;
use teams_media::processing::ProcessingConfig;
use teams_media::sdp::{Dialect, NgcSdp, NgcSession, SDP_NGC, SDP_NGC_MIXER, SdpError, VideoLines};
use teams_media::session::{MediaSession, SdpAnswer, SdpOffer, SessionConfig, VideoParts};
use teams_media::test_turn::TurnServer;
use teams_media::video::codec::EncoderSettings;
use teams_media::video::convert::MaxSize;

const FIXTURE: &str = include_str!("../../teams-sdp/tests/fixtures/sdp/captured.json");

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static CAPTURED: LazyLock<Result<Value, String>> =
    LazyLock::new(|| serde_json::from_str(FIXTURE).map_err(|error| error.to_string()));

/// A captured SDP's text as the service sent it or the web client wrote it.
fn captured_text(name: &str) -> TestResult<&'static str> {
    let fixture = CAPTURED.as_ref().map_err(Clone::clone)?;
    Ok(fixture[name]["blob"].as_str().ok_or("no such SDP in the fixture")?)
}

/// A captured SDP, with its content type.
fn captured(name: &str) -> TestResult<NgcSdp> {
    let fixture = CAPTURED.as_ref().map_err(Clone::clone)?;
    let content_type = fixture[name]["contentType"].as_str().ok_or("no content type")?;
    Ok(NgcSdp::new(content_type, captured_text(name)?)?)
}

/// A call's media session (libwebrtc) behind a loopback TURN server of its own: its only candidates relayed, as the
/// native client's are. The session goes before its relay.
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
        Ok(Self { session, _turn: turn })
    }

    /// A session whose video started (the camera's track on the main video line, off).
    fn with_video() -> TestResult<Self> {
        let peer = Self::new()?;
        peer.session.start_video(VideoParts {
            encoder: EncoderSettings::default(),
            max_size: MaxSize::HD,
            remote: Box::new(|_| Box::new(FrameCounter::new()) as Box<dyn VideoSink>),
            preview: None,
            background: teams_media::video::effects::BackgroundChoice::default(),
        })?;
        Ok(peer)
    }
}

impl Deref for Peer {
    type Target = MediaSession;

    fn deref(&self) -> &MediaSession {
        &self.session
    }
}

/// The address and port of the first relayed candidate of a standard SDP.
fn relayed(text: &str) -> TestResult<(String, String)> {
    let candidate = text
        .lines()
        .find_map(|line| line.trim_end().strip_prefix("a=candidate:"))
        .ok_or("no candidate")?;
    let fields: Vec<&str> = candidate.split(' ').collect();
    match fields[..] {
        [_, _, _, _, address, port, "typ", "relay", ..] => Ok((address.to_owned(), port.to_owned())),
        _ => Err(format!("not a relayed candidate: {candidate}").into()),
    }
}

/// The lines of an SDP, without empty ones.
fn lines(text: &str) -> Vec<&str> {
    text.split("\r\n").filter(|line| !line.is_empty()).collect()
}

/// The session part, then each media section, as lines.
fn sections(text: &str) -> Vec<Vec<&str>> {
    let mut out = vec![Vec::new()];
    for line in lines(text) {
        if line.starts_with("m=") {
            out.push(Vec::new());
        }
        if let Some(section) = out.last_mut() {
            section.push(line);
        }
    }
    out
}

/// What a line is (`m`, `c`, `b`, or an attribute's name), with runs of the same kind collapsed: the shape of a
/// section, apart from how many codecs, extensions or candidates it lists.
fn shape<'a>(section: &[&'a str]) -> Vec<&'a str> {
    let mut kinds: Vec<&str> = Vec::new();
    for line in section {
        let kind = match line.strip_prefix("a=") {
            Some(rest) => rest.split(':').next().unwrap_or(rest),
            None => &line[..1],
        };
        if kinds.last() != Some(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

fn value<'a>(section: &[&'a str], name: &str) -> Option<&'a str> {
    let prefix = format!("a={name}:");
    section.iter().find_map(|line| line.strip_prefix(prefix.as_str()))
}

/// Each media section's label.
fn labels<'a>(parts: &[Vec<&'a str>]) -> Vec<Option<&'a str>> {
    parts[1..].iter().map(|section| value(section, "label")).collect()
}

/// Each media section's mid.
fn mids<'a>(parts: &[Vec<&'a str>]) -> Vec<Option<&'a str>> {
    parts[1..].iter().map(|section| value(section, "mid")).collect()
}

/// An SDP's text from its session part and media sections.
fn joined(parts: &[Vec<&str>]) -> String {
    parts.concat().join("\r\n") + "\r\n"
}

/// A session that takes a call's main video line (a 1:1 call's), as these tests of the lines' naming need one.
fn video_session() -> NgcSession {
    let mut session = NgcSession::new();
    session.set_video(VideoLines::Main { sending: true });
    session
}

/// A session that read and answered the mixer's offer with `lines`, as a callee in a group call.
fn mixer_session(peer: &Peer, lines: VideoLines) -> TestResult<NgcSession> {
    let mut session = NgcSession::new();
    session.set_video(lines);
    let offer = session.read_offer(&captured("mixer_offer")?)?;
    let answer = peer.accept_offer(offer)?;
    session.write_answer(&answer)?;
    Ok(session)
}

// ---- the caller's side ----

#[test]
fn our_offer_is_written_in_the_shape_of_the_web_clients_call_start() -> TestResult {
    let peer = Peer::new()?;
    let offer = peer.offer_audio()?;
    let mut session = NgcSession::new();

    let ngc = session.write_offer(&offer).unwrap();

    assert_eq!(ngc.dialect(), Dialect::Ngc);
    assert_eq!(ngc.content_type(), SDP_NGC);
    let ours = sections(ngc.as_str());
    let web = sections(captured_text("start_offer")?);
    assert_eq!(shape(&ours[0]), shape(&web[0]), "the session part");
    assert_eq!(shape(&ours[1]), shape(&web[1]), "the audio line:\n{}", ngc.as_str());
    assert_eq!(ours.len(), 2, "audio only");
    assert!(!ngc.as_str().contains("UDP/TLS"));
    Ok(())
}

/// Our candidates as Teams writes them: foundations numbered from 1, component 1 over UDP, relayed, no related address
/// and no extension.
fn assert_teams_candidates(candidates: &[&str]) {
    assert!(!candidates.is_empty(), "no candidate");
    for candidate in candidates {
        let fields: Vec<&str> = candidate.split(' ').collect();
        let foundation = fields[0].strip_prefix("a=candidate:").unwrap_or_default();
        assert!(
            foundation
                .parse::<usize>()
                .is_ok_and(|number| (1..=candidates.len()).contains(&number)),
            "foundations numbered from 1: {candidates:#?}"
        );
        assert_eq!(fields[1..3], ["1", "udp"], "{candidate}");
        assert_eq!(
            fields[6..],
            ["typ", "relay", "raddr", "0.0.0.0", "rport", "0"],
            "relayed, no related address, no ufrag extension: {candidate}"
        );
    }
}

#[test]
fn our_offer_carries_the_relayed_candidate_and_teams_own_lines() -> TestResult {
    let peer = Peer::new()?;
    let offer = peer.offer_audio()?;
    let (address, port) = relayed(offer.as_str())?;
    let ngc = NgcSession::new().write_offer(&offer)?;
    let ours = sections(ngc.as_str());
    let web = sections(captured_text("start_offer")?);

    let audio = &ours[1];
    assert_eq!(
        audio[0],
        format!("m=audio {port} RTP/SAVP 111"),
        "the relayed candidate's port, Opus alone"
    );
    assert_eq!(audio[1], format!("c=IN IP4 {address}"));
    assert_eq!(value(audio, "rtcp"), Some(port.as_str()));
    assert_eq!(value(audio, "mid"), Some("0"), "Teams numbers its lines");
    assert_eq!(value(audio, "label"), Some("main-audio"));
    assert_eq!(value(&ours[0], "group"), Some("BUNDLE 0"));
    assert_eq!(ours[0][1].split(' ').next(), Some("o=-"));
    assert!(ours[0][1].ends_with(" IN IP4 127.0.0.1"));
    assert!(ours[0].contains(&"b=CT:4000"));
    let candidates: Vec<&str> = audio
        .iter()
        .filter(|line| line.starts_with("a=candidate:"))
        .copied()
        .collect();
    assert_teams_candidates(&candidates);
    assert!(candidates[0].contains(&format!(" {address} {port} typ relay")));
    // Teams' own lines exactly as the web client writes them
    for line in [
        "a=x-signaling-fb:* x-message app recv:dsh",
        r"a=extmap:2 http:\\www.webrtc.org\experiments\rtp-hdrext\abs-send-time",
        r"a=extmap:3 http:\\www.ietf.org\id\draft-holmer-rmcat-transport-wide-cc-extensions-01",
        "a=fmtp:111 minptime=10;useinbandfec=1",
        "a=rtpmap:111 opus/48000/2",
        "a=setup:actpass",
        "a=sendrecv",
        "a=rtcp-mux",
    ] {
        assert!(audio.contains(&line), "{line} in {audio:#?}");
        assert!(web[1].contains(&line), "{line} is the web client's too");
    }
    let ssrc = value(audio, "ssrc").unwrap().split(' ').next().unwrap();
    assert_eq!(value(audio, "x-ssrc-range"), Some(format!("{ssrc}-{ssrc}").as_str()));
    Ok(())
}

#[test]
fn the_media_controllers_answer_reads_back_for_libwebrtc() -> TestResult {
    let peer = Peer::new()?;
    let offer = peer.offer_audio()?;
    let mut session = NgcSession::new();
    session.write_offer(&offer).unwrap();

    let answer = session.read_answer(&captured("controller_answer")?).unwrap();

    let text = answer.to_sdp_string();
    let parts = sections(&text);
    assert_eq!(
        parts.len(),
        2,
        "only the line we offered; the other twelve dropped:\n{text}"
    );
    let audio = &parts[1];
    // the answer's own port and formats, on the standard transport
    assert!(
        audio[0].starts_with("m=audio 3478 UDP/TLS/RTP/SAVPF 120 111 "),
        "{}",
        audio[0]
    );
    assert_eq!(value(audio, "mid"), Some("0"), "libwebrtc's own mid");
    assert!(
        audio.contains(&"a=ssrc:1000 cname:teams-ngc"),
        "the stream x-ssrc-range names"
    );
    assert!(audio.contains(&"a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time"));
    let candidates: Vec<&str> = audio
        .iter()
        .filter(|line| line.starts_with("a=candidate:"))
        .copied()
        .collect();
    assert_eq!(
        candidates.len(),
        2,
        "the UDP relay candidates, IPv4 and IPv6; TCP left out: {candidates:#?}"
    );
    assert!(
        candidates
            .iter()
            .all(|line| line.contains(" udp ") && line.contains(" typ relay"))
    );
    for teams in ["MTURNID", "a=x-", "a=label", "a=rtcp:", "b=CT", "RTP/SAVP "] {
        assert!(!text.contains(teams), "{teams} left in:\n{text}");
    }

    peer.accept_answer(answer).unwrap();
    Ok(())
}

#[test]
fn an_answer_with_a_data_line_we_never_offered_drops_it() -> TestResult {
    // the media controller's answer cut to its audio and data lines, to an audio-only offer
    let full = sections(captured_text("controller_answer")?);
    let session_part: Vec<&str> = full[0]
        .iter()
        .map(|line| {
            if line.starts_with("a=group:") {
                "a=group:BUNDLE 0 12"
            } else {
                line
            }
        })
        .collect();
    let blob = [session_part, full[1].clone(), full[13].clone()].concat().join("\r\n") + "\r\n";
    let answer = NgcSdp::new(SDP_NGC, &blob).unwrap();
    let peer = Peer::new()?;
    let offer = peer.offer_audio()?;
    let mut session = NgcSession::new();
    session.write_offer(&offer).unwrap();

    let read = session.read_answer(&answer).unwrap();

    let text = read.to_sdp_string();
    assert!(!text.contains("m=application"), "{text}");
    peer.accept_answer(read).unwrap();
    Ok(())
}

#[test]
fn an_answer_that_numbers_opus_otherwise_keeps_its_number() -> TestResult {
    // the media controller's answer to an audio-only offer, Opus numbered 102 where the offer said 111, as a live
    // test call answered: libwebrtc, like the web client's browser, sends with the answer's number
    let full = sections(captured_text("controller_answer")?);
    let session_part: Vec<&str> = full[0]
        .iter()
        .map(|line| {
            if line.starts_with("a=group:") {
                "a=group:BUNDLE 0"
            } else {
                line
            }
        })
        .collect();
    let audio: Vec<String> = full[1]
        .iter()
        .map(|line| {
            if line.starts_with("m=audio") {
                line.replace(" 111 ", " 102 ")
            } else {
                line.replace("a=rtpmap:111 ", "a=rtpmap:102 ")
                    .replace("a=fmtp:111 ", "a=fmtp:102 ")
                    .replace("a=rtcp-fb:111 ", "a=rtcp-fb:102 ")
                    .replace("111/111", "102/102")
            }
        })
        .collect();
    assert!(audio.iter().any(|line| line.starts_with("a=rtpmap:102 ")), "{audio:#?}");
    let audio: Vec<&str> = audio.iter().map(String::as_str).collect();
    let blob = [session_part, audio].concat().join("\r\n") + "\r\n";
    let answer = NgcSdp::new(SDP_NGC, &blob).unwrap();
    let peer = Peer::new()?;
    let offer = peer.offer_audio()?;
    assert!(
        offer.as_str().contains("a=rtpmap:111 opus/48000/2"),
        "libwebrtc offers Opus as 111"
    );
    let mut session = NgcSession::new();
    session.write_offer(&offer).unwrap();

    let read = session.read_answer(&answer).unwrap();

    let text = read.to_sdp_string();
    assert!(
        text.to_ascii_lowercase().contains("a=rtpmap:102 opus/48000/2"),
        "the answer's number kept:\n{text}"
    );
    assert!(!text.contains("a=rtpmap:111 "), "{text}");
    peer.accept_answer(read).unwrap();
    Ok(())
}

/// Our offer in the web client's shape of four lines: audio, the main video line, the sharing line, data (libwebrtc's
/// group offer with no receiving line).
fn four_line_offer(peer: &Peer) -> TestResult<SdpOffer> {
    Ok(peer.offer_group(0)?)
}

#[test]
fn a_callees_answer_in_a_one_to_one_call_reads_back() -> TestResult {
    let peer = Peer::new()?;
    let offer = four_line_offer(&peer)?;
    let (_, port) = relayed(offer.as_str())?;
    let mut session = NgcSession::new();
    let ngc = session.write_offer(&offer).unwrap();
    let ours = sections(ngc.as_str());
    assert_eq!(value(&ours[0], "group"), Some("BUNDLE 0 1 2 3"));
    let data = &ours[4];
    assert_eq!(
        data[0],
        format!("m=x-data {port} RTP/SAVP 127 126"),
        "the data channel as Teams writes it"
    );
    assert_eq!(
        shape(data),
        shape(&sections(captured_text("start_offer")?)[4]),
        "the data line"
    );
    assert_eq!(value(data, "sctp-port"), Some("5000"));
    assert_eq!(
        labels(&ours),
        labels(&sections(captured_text("start_offer")?)),
        "the web client's labels: the second video line is the sharing one"
    );

    let answer = session.read_answer(&captured("callee_answer")?).unwrap();

    let text = answer.to_sdp_string();
    let parts = sections(&text);
    assert_eq!(parts.len(), 5, "{text}");
    assert_eq!(
        value(&parts[3], "mid"),
        mids(&sections(offer.as_str()))[2],
        "the rejected line, by its place"
    );
    assert!(parts[3][0].starts_with("m=video 0 "), "{}", parts[3][0]);
    assert!(parts[4][0].starts_with("m=application "), "{}", parts[4][0]);
    assert_eq!(value(&parts[4], "sctp-port"), Some("5000"));
    peer.accept_answer(answer).unwrap();
    Ok(())
}

// ---- the callee's side ----

#[test]
fn an_invitations_offer_is_answered_in_teams_dialect() -> TestResult {
    let peer = Peer::new()?;
    let mut session = NgcSession::new();

    let offer = session.read_offer(&captured("invitation_offer")?).unwrap();
    let read = offer.to_sdp_string();
    assert!(
        !read
            .lines()
            .any(|line| line.starts_with("a=candidate:") && line.contains(" tcp")),
        "the caller's TCP candidates are out of the relay's reach:\n{read}"
    );
    assert!(read.contains(" typ relay "), "{read}");
    assert!(
        read.contains("m=application 50000 UDP/DTLS/SCTP webrtc-datachannel"),
        "{read}"
    );
    let answer = peer.accept_offer(offer).unwrap();
    let (_, port) = relayed(answer.as_str())?;
    let ngc = session.write_answer(&answer).unwrap();

    let ours = sections(ngc.as_str());
    let web = sections(captured_text("accept_answer")?);
    assert_eq!(
        shape(&ours[1]),
        shape(&web[1]),
        "the audio line, as the web client's accept carries it:\n{}",
        ngc.as_str()
    );
    assert_eq!(ours.len(), web.len(), "every offered line answered:\n{}", ngc.as_str());
    assert_eq!(
        labels(&ours),
        [
            Some("main-audio"),
            Some("main-video"),
            Some("applicationsharing-video"),
            Some("data")
        ],
        "the offer's labels"
    );
    assert_eq!(value(&ours[1], "mid"), Some("0"), "the offer's mids");
    assert_eq!(value(&ours[1], "rtcp"), None, "an answer has no a=rtcp");
    assert_eq!(
        ours[1][0].split(' ').nth(1),
        Some(port.as_str()),
        "our relayed candidate's port"
    );
    assert!(
        ours[4][0].starts_with(&format!("m=x-data {port} RTP/SAVP 127 126")),
        "{}",
        ours[4][0]
    );
    assert_eq!(
        value(&ours[1], "setup"),
        value(&web[1], "setup"),
        "the callee takes the DTLS client's part"
    );
    Ok(())
}

#[test]
fn a_mixers_offer_in_its_older_dialect_is_read_and_answered() -> TestResult {
    let mixer = captured("mixer_offer")?;
    assert_eq!(mixer.dialect(), Dialect::Mixer);
    assert_eq!(mixer.content_type(), SDP_NGC_MIXER);
    let peer = Peer::new()?;
    let mut session = video_session();

    let offer = session.read_offer(&mixer).unwrap();

    let read = offer.to_sdp_string();
    for gone in ["a=crypto", "a=cryptoscale", "RTP/AVP", "MTURNID", "a=x-", "x-message"] {
        assert!(!read.contains(gone), "{gone} left in:\n{read}");
    }
    let parts = sections(&read);
    assert_eq!(parts.len(), 14, "audio, twelve video lines and data");
    // the lines in use: the audio, the main video line the call took, the data line (the other video lines are
    // read rejected)
    for section in parts[1..].iter().filter(|section| !section[0].contains(" 0 ")) {
        assert!(
            value(section, "ice-ufrag").is_some(),
            "every line of the mixer's carries its ICE credentials, kept: {section:#?}"
        );
        assert_eq!(
            value(section, "setup"),
            Some("actpass"),
            "the mixer's offer names no setup: the role is left to us (libwebrtc takes the client's part)"
        );
    }
    assert!(
        parts[2].contains(&"a=rtcp-fb:107 nack pli"),
        "a=rtcp-fb:* written per payload type"
    );
    assert!(parts[13][0].starts_with("m=application "), "{}", parts[13][0]);
    assert_eq!(value(&parts[13], "sctp-port"), Some("5000"));
    let answer = peer.accept_offer(offer).unwrap();
    let ngc = session.write_answer(&answer).unwrap();
    assert_eq!(
        ngc.content_type(),
        SDP_NGC,
        "we answer in the current dialect, as the web client does"
    );
    assert_eq!(sections(ngc.as_str()).len(), 14);
    Ok(())
}

// ---- renegotiations ----

#[test]
fn a_renegotiation_offered_to_the_callee_names_the_same_lines() -> TestResult {
    let peer = Peer::new()?;
    let mut session = video_session();
    let offer = session.read_offer(&captured("invitation_offer")?).unwrap();
    let answer = peer.accept_offer(offer).unwrap();
    session.write_answer(&answer).unwrap();

    let renegotiation = session.read_offer(&captured("renegotiation_offer")?).unwrap();
    let answer = peer.accept_offer(renegotiation).unwrap();
    let ngc = session.write_answer(&answer).unwrap();

    let ours = sections(ngc.as_str());
    assert_eq!(value(&ours[0], "group"), Some("BUNDLE 0 1 3"), "{}", ngc.as_str());
    assert_eq!(value(&ours[2], "mid"), Some("1"));
    Ok(())
}

#[test]
fn a_renegotiation_offered_to_the_caller_maps_back_to_its_own_mids() -> TestResult {
    let peer = Peer::new()?;
    let offer = four_line_offer(&peer)?;
    let offered = offer.to_sdp_string();
    let mut session = video_session();
    session.write_offer(&offer).unwrap();
    peer.accept_answer(session.read_answer(&captured("callee_answer")?).unwrap())
        .unwrap();

    let renegotiation = session.read_offer(&captured("callee_renegotiation_offer")?).unwrap();

    let text = renegotiation.to_sdp_string();
    let parts = sections(&text);
    assert_eq!(value(&parts[2], "mid"), mids(&sections(&offered))[1], "{text}");
    let answer = peer.accept_offer(renegotiation).unwrap();
    let ngc = session.write_answer(&answer).unwrap();
    assert_eq!(value(&sections(ngc.as_str())[2], "mid"), Some("1"));
    Ok(())
}

#[test]
fn a_renegotiation_offered_by_us_keeps_the_numbering() -> TestResult {
    // the camera's track is on the main video line from the first offer on, sending nothing until the camera turns
    let peer = Peer::with_video()?;
    let offer = four_line_offer(&peer)?;
    let mut session = NgcSession::new();
    session.write_offer(&offer).unwrap();
    peer.accept_answer(session.read_answer(&captured("callee_answer")?).unwrap())
        .unwrap();

    // the camera comes on: the renegotiation offers the main video line as it is, sending
    let offer = peer.offer_video(true)?.ok_or("an offer")?;
    let ngc = session.write_offer(&offer).unwrap();

    let ours = sections(ngc.as_str());
    let video = &ours[2];
    assert_eq!(value(video, "mid"), Some("1"));
    assert!(video.contains(&"a=sendrecv"), "{video:#?}");
    assert!(
        value(video, "x-ssrc-range").is_some(),
        "the camera's stream named as Teams names it"
    );
    let answer = session.read_answer(&captured("caller_renegotiation_answer")?).unwrap();
    peer.accept_answer(answer).unwrap();
    Ok(())
}

#[test]
fn the_mixers_renegotiation_is_read_and_answered() -> TestResult {
    let peer = Peer::new()?;
    let mut session = mixer_session(&peer, VideoLines::None)?;

    let renegotiation = session.read_offer(&captured("mixer_renegotiation_offer")?).unwrap();
    let answer = peer.accept_offer(renegotiation).unwrap();
    let ngc = session.write_answer(&answer).unwrap();

    assert_eq!(sections(ngc.as_str()).len(), 14);
    Ok(())
}

#[test]
fn a_line_we_add_in_a_group_call_is_numbered_past_the_mixers_mids() -> TestResult {
    // an audio-only group call (every video line of the mixer's rejected): our camera comes on as a line of its own
    let peer = Peer::with_video()?;
    let mut session = mixer_session(&peer, VideoLines::None)?;
    let offer = peer.offer_video(true)?.ok_or("an offer")?;
    let standard = offer.to_sdp_string();
    let camera_place = sections(&standard)
        .iter()
        .position(|section| section[0].starts_with("m=video ") && !section[0].starts_with("m=video 0 "))
        .ok_or("the camera's line in libwebrtc's offer")?;
    let camera = value(&sections(&standard)[camera_place], "mid")
        .ok_or("the camera line's mid")?
        .to_owned();

    let ngc = session.write_offer(&offer).unwrap();

    let ours = sections(ngc.as_str());
    let named = mids(&ours);
    assert_eq!(
        named[camera_place - 1],
        Some("14"),
        "one past the mixer's highest mid:\n{}",
        ngc.as_str()
    );
    assert_eq!(value(&ours[camera_place], "label"), Some("main-video"));
    assert!(ours[camera_place].contains(&"a=sendrecv"), "{:?}", ours[camera_place]);
    // the lines in use are named once each (the mixer's video lines the call doesn't take are written rejected,
    // naming no mid)
    for (place, mid) in named.iter().enumerate().filter(|(_, mid)| mid.is_some()) {
        assert_eq!(
            named.iter().filter(|other| *other == mid).count(),
            1,
            "{mid:?} at {place} named once"
        );
    }
    let bundle: Vec<&str> = value(&ours[0], "group")
        .and_then(|group| group.strip_prefix("BUNDLE "))
        .ok_or("no bundle")?
        .split(' ')
        .collect();
    assert_eq!(bundle.len(), named.iter().flatten().count());
    assert_eq!(bundle.iter().filter(|mid| **mid == "14").count(), 1);

    // a far end taking every line as offered answers it, and its answer goes back to the camera's own line
    let far = Peer::new()?;
    let mut theirs = NgcSession::new();
    theirs.set_video(VideoLines::Offered);
    let answer = far.accept_offer(theirs.read_offer(&ngc)?)?;
    let answer = theirs.write_answer(&answer)?;
    let read = session.read_answer(&answer).unwrap();

    let text = read.to_sdp_string();
    let parts = sections(&text);
    assert_eq!(
        value(&parts[camera_place], "mid"),
        Some(camera.as_str()),
        "the camera's answer goes to the camera's line:\n{text}"
    );
    assert_eq!(
        value(&parts[1], "mid"),
        Some("1"),
        "the mixer's audio line stays its own"
    );
    peer.accept_answer(read).unwrap();
    Ok(())
}

#[test]
fn a_rejected_line_without_a_mid_in_a_mixers_renegotiation_is_the_line_at_its_place() -> TestResult {
    let peer = Peer::new()?;
    let mut session = mixer_session(&peer, VideoLines::Main { sending: true })?;

    // the mixer's renegotiation with its sharing line (mid 3, at place 11) rejected, named only by its label
    let mut parts = sections(captured_text("mixer_renegotiation_offer")?);
    for line in &mut parts[0] {
        if line.starts_with("a=group:BUNDLE ") {
            *line = "a=group:BUNDLE 1 2 5 6 7 8 9 10 11 12 13 4";
        }
    }
    assert_eq!(value(&parts[12], "mid"), Some("3"));
    parts[12] = vec!["m=video 0 RTP/SAVP 34", "a=label:applicationsharing-video"];
    let renegotiation = NgcSdp::new(SDP_NGC, &joined(&parts))?;

    let offer = session.read_offer(&renegotiation).unwrap();

    let text = offer.to_sdp_string();
    let read = sections(&text);
    assert_eq!(value(&read[12], "mid"), Some("3"), "the sharing line's mid:\n{text}");
    assert!(read[12][0].starts_with("m=video 0 "), "{}", read[12][0]);
    let named = mids(&read);
    assert_eq!(
        named.iter().filter(|mid| **mid == Some("11")).count(),
        1,
        "the camera line keeps mid 11 to itself"
    );
    let answer = peer.accept_offer(offer).unwrap();
    let ngc = session.write_answer(&answer).unwrap();
    assert_eq!(sections(ngc.as_str()).len(), 14);
    Ok(())
}

#[test]
fn our_offer_names_no_line_naming_header_extension_and_our_answer_keeps_the_offers() -> TestResult {
    // seen live (M3 Task 16): our packets carry our own mid, which Teams' dialect renames; a web client that answered
    // an offer naming the mid extension dropped every packet of ours
    let peer = Peer::new()?;
    let offer = peer.offer_audio()?;
    let standard = offer.to_sdp_string();
    assert!(
        standard.contains("urn:ietf:params:rtp-hdrext:sdes:mid"),
        "libwebrtc offers it"
    );
    let ngc = NgcSession::new().write_offer(&offer)?;
    for uri in ["sdes:mid", "sdes:rtp-stream-id", "sdes:repaired-rtp-stream-id"] {
        assert!(!ngc.as_str().contains(uri), "{uri} in our offer:\n{}", ngc.as_str());
    }
    assert!(ngc.as_str().contains("abs-send-time"), "the other extensions stay");
    // answering an offer that names it, it stays: our answer names the offer's own mids
    let answerer = Peer::new()?;
    let mut session = NgcSession::new();
    let invitation = captured("invitation_offer")?;
    let read = session.read_offer(&invitation)?;
    let offered_mid = invitation.as_str().contains("sdes:mid");
    let answer = answerer.accept_offer(read)?;
    let written = session.write_answer(&answer)?;
    assert_eq!(
        written.as_str().contains("sdes:mid"),
        offered_mid,
        "{}",
        written.as_str()
    );
    Ok(())
}

#[test]
fn an_audio_only_answer_to_the_mixer_names_a_format_on_each_rejected_video_line() -> TestResult {
    // an audio-only call: every video line read rejected, and libwebrtc answers each one rejected; the dialect writes
    // the web client's static `34` on each, never an m-line without a format (the mixer refuses that:
    // `SdpParsingFailure`)
    let peer = Peer::new()?;
    let mut session = NgcSession::new();
    let offer = session.read_offer(&captured("mixer_offer")?)?;
    let answer = peer.accept_offer(offer)?;
    let ngc = session.write_answer(&answer)?;
    let parts = sections(ngc.as_str());
    assert_eq!(parts.len(), 14);
    let videos: Vec<&str> = parts
        .iter()
        .map(|part| part[0])
        .filter(|line| line.starts_with("m=video"))
        .collect();
    assert_eq!(
        videos.len(),
        11,
        "every video line of the mixer's, the sharing line among them"
    );
    assert!(
        videos.iter().all(|line| *line == "m=video 0 RTP/SAVP 34"),
        "{videos:#?}"
    );
    assert!(parts[1][0].starts_with("m=audio ") && !parts[1][0].starts_with("m=audio 0 "));
    Ok(())
}

// ---- what is refused ----

#[test]
fn a_browsers_sdp_is_refused_where_teams_dialect_is_required() -> TestResult {
    for browser in [captured_text("browser_offer")?, captured_text("browser_answer")?] {
        assert!(
            matches!(
                NgcSdp::new(SDP_NGC, browser),
                Err(SdpError::NotNgc(_) | SdpError::Profile { .. })
            ),
            "a browser's SDP as it is"
        );
    }
    let peer = Peer::new()?;
    let offer = peer.offer_audio()?;
    assert!(
        NgcSdp::new(SDP_NGC, &offer.to_sdp_string()).is_err(),
        "libwebrtc's as it is"
    );
    assert!(
        NgcSdp::new(SDP_NGC, "m=audio 50000 RTP/SAVP 111\r\n").is_err(),
        "no session part"
    );
    assert!(NgcSdp::new(SDP_NGC, "v=0\r\ns=-\r\n").is_err(), "no media line");
    assert!(
        NgcSdp::new(SDP_NGC, "v=0\r\ns=-\r\nm=audio 9 RTP/SAVP 111\r\n").is_err(),
        "the browser's placeholder port"
    );
    assert!(
        NgcSdp::new(SDP_NGC, captured_text("mixer_offer")?).is_err(),
        "the mixer's RTP/AVP outside its dialect"
    );
    assert!(matches!(
        NgcSdp::new("application/sdp", captured_text("start_offer")?),
        Err(SdpError::ContentType(_))
    ));
    for name in [
        "start_offer",
        "callee_answer",
        "invitation_offer",
        "accept_answer",
        "controller_answer",
    ] {
        assert_eq!(captured(name)?.dialect(), Dialect::Ngc, "{name}");
    }
    Ok(())
}

/// libwebrtc's offer with each candidate's line changed by `edit` (`None`: left out).
fn with_candidates(offer: &SdpOffer, edit: impl Fn(&str) -> Option<String>) -> TestResult<SdpOffer> {
    let text = offer.to_sdp_string();
    let edited: Vec<String> = text
        .split("\r\n")
        .filter_map(|line| {
            if line.starts_with("a=candidate:") {
                edit(line)
            } else {
                Some(line.to_owned())
            }
        })
        .collect();
    Ok(SdpOffer::from_sdp_string(&edited.join("\r\n"))?)
}

#[test]
fn a_local_address_never_goes_out() -> TestResult {
    // the peer gathers relayed candidates only: a host candidate written into its offer stands for one that slipped
    let peer = Peer::new()?;
    let offer = with_candidates(&peer.offer_audio()?, |line| {
        let mut fields: Vec<&str> = line.split(' ').take(6).collect();
        fields[4] = "192.0.2.60";
        Some(format!("{} typ host", fields.join(" ")))
    })?;
    assert!(offer.as_str().contains(" typ host"), "{}", offer.as_str());

    let error = NgcSession::new().write_offer(&offer).unwrap_err();

    assert_eq!(error, SdpError::LocalCandidate("host".to_owned()));
    Ok(())
}

#[test]
fn an_offer_without_a_relayed_candidate_is_unwritable() -> TestResult {
    let peer = Peer::new()?;
    let offer = with_candidates(&peer.offer_audio()?, |_| None)?;

    let error = NgcSession::new().write_offer(&offer).unwrap_err();

    assert!(matches!(error, SdpError::Unwritable(_)), "{error}");
    Ok(())
}

#[test]
fn every_captured_answer_reads_as_an_answer() -> TestResult {
    // `SdpAnswer` and `SdpOffer` are libwebrtc's parse: the conversion of each captured form must be one it takes
    let peer = Peer::new()?;
    let offer = four_line_offer(&peer)?;
    let mut session = NgcSession::new();
    session.write_offer(&offer).unwrap();
    for name in ["callee_answer", "caller_renegotiation_answer", "renegotiation_answer"] {
        let answer: SdpAnswer = session.read_answer(&captured(name)?).unwrap();
        assert!(answer.to_sdp_string().starts_with("v=0"), "{name}");
    }
    Ok(())
}

#[test]
fn converted_offers_and_answers_still_require_libwebrtc_validation() -> TestResult {
    let original = captured("invitation_offer")?;
    let malformed = original
        .as_str()
        .lines()
        .map(|line| {
            if line.starts_with("o=") {
                "o=- a=ice-pwd:SyntheticPrivateValue"
            } else {
                line.trim_end()
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n";
    let malformed = NgcSdp::new(SDP_NGC, &malformed)?;
    let mut session = NgcSession::new();
    assert_eq!(
        session.read_offer(&malformed).unwrap_err(),
        SdpError::Refused("invalid SDP origin line".to_owned())
    );
    session.read_offer(&original)?;
    assert_eq!(
        session.read_answer(&malformed).unwrap_err(),
        SdpError::Refused("invalid SDP origin line".to_owned())
    );
    Ok(())
}
