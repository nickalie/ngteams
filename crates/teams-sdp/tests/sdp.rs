//! Pure dialect checks against synthetic Teams SDP shapes.

use std::sync::LazyLock;

use serde_json::Value;
use teams_sdp::sdp::{Dialect, NgcSdp, NgcSession, SDP_NGC, SdpError, VideoLines};
use teams_sdp::{SdpAnswer, SdpOffer};

const FIXTURE: &str = include_str!("fixtures/sdp/captured.json");
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
static CAPTURED: LazyLock<Result<Value, String>> =
    LazyLock::new(|| serde_json::from_str(FIXTURE).map_err(|error| error.to_string()));

fn captured_text(name: &str) -> TestResult<&'static str> {
    Ok(CAPTURED.as_ref().map_err(Clone::clone)?[name]["blob"]
        .as_str()
        .ok_or("no such SDP")?)
}

fn captured(name: &str) -> TestResult<NgcSdp> {
    Ok(NgcSdp::new(
        CAPTURED.as_ref().map_err(Clone::clone)?[name]["contentType"]
            .as_str()
            .ok_or("no content type")?,
        captured_text(name)?,
    )?)
}

fn sections(text: &str) -> Vec<Vec<&str>> {
    let mut out: Vec<Vec<&str>> = Vec::new();
    for line in text.split("\r\n").filter(|line| !line.is_empty()) {
        if line.starts_with("m=") || out.is_empty() {
            out.push(Vec::new());
        }
        if let Some(part) = out.last_mut() {
            part.push(line);
        }
    }
    out
}

fn joined(parts: &[Vec<&str>]) -> String {
    parts.iter().flatten().copied().collect::<Vec<_>>().join("\r\n") + "\r\n"
}

#[test]
fn a_rejected_line_may_name_any_profile_and_a_line_in_use_only_teams_own() -> TestResult {
    // seen live (M3 Task 16): the mixer's renegotiation of a call whose video lines we rejected names a profile on
    // them that isn't RTP/SAVP; a rejected line carries nothing, so whatever it names is read as rejected
    let mut parts = sections(captured_text("mixer_renegotiation_offer")?);
    let video = parts
        .iter()
        .position(|part| part[0].starts_with("m=video "))
        .ok_or("a video line")?;
    parts[video][0] = "m=video 0 RTP/AVP 34";
    let text = joined(&parts);
    let read = NgcSdp::new(SDP_NGC, &text)?;
    assert!(read.as_str().contains("m=video 0 RTP/AVP 34"));
    // a line in use on another profile is refused, the error naming its kind, port and profile only
    parts[video][0] = "m=video 50000 RTP/AVPF 34";
    let refused = NgcSdp::new(SDP_NGC, &joined(&parts)).unwrap_err();
    assert_eq!(
        refused,
        SdpError::Profile {
            media: "video".to_owned(),
            port: "50000".to_owned(),
            proto: "RTP/AVPF".to_owned(),
        }
    );
    Ok(())
}

#[test]
fn an_answer_to_an_offer_we_never_made_is_refused() -> TestResult {
    let mut session = NgcSession::new();
    let error = session.read_answer(&captured("controller_answer")?).unwrap_err();
    assert!(matches!(error, SdpError::Mismatch(_)), "{error}");
    Ok(())
}

#[test]
fn the_sdp_never_shows_in_debug() -> TestResult {
    let debug = format!("{:?}", captured("start_offer")?);
    assert!(!debug.contains("ice-pwd") && !debug.contains("192.0.2"), "{debug}");
    Ok(())
}

fn relayed_standard(name: &str) -> TestResult<String> {
    let mut text = String::new();
    for line in captured_text(name)?.lines() {
        if line.starts_with("a=candidate:") {
            continue;
        }
        text.push_str(line.trim_end());
        text.push_str("\r\n");
        if line.starts_with("m=") && !line.starts_with("m=video 0 ") {
            text.push_str("a=candidate:1 1 udp 58532351 192.0.2.10 50000 typ relay\r\n");
        }
    }
    Ok(text)
}

#[test]
fn standard_text_is_preserved_and_debug_hides_credentials() {
    let text = "v=0\r\na=ice-pwd:SyntheticPrivateValue\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\n";
    let offer = SdpOffer::new(text.to_owned());
    let answer = SdpAnswer::new(text.to_owned());
    assert_eq!(offer.as_str(), text);
    assert_eq!(answer.as_str(), text);
    assert_eq!(offer.to_sdp_string(), text);
    assert_eq!(answer.to_sdp_string(), text);
    assert_eq!(offer.clone(), offer);
    assert_eq!(answer.clone(), answer);
    for debug in [format!("{offer:?}"), format!("{answer:?}")] {
        assert!(debug.contains("media: 1"));
        assert!(!debug.contains("SyntheticPrivateValue"));
    }
}

#[test]
fn every_fixture_offer_converts_for_each_video_mode() -> TestResult {
    for name in [
        "invitation_offer",
        "mixer_offer",
        "mixer_renegotiation_offer",
        "callee_renegotiation_offer",
    ] {
        for mode in [
            VideoLines::None,
            VideoLines::Main { sending: false },
            VideoLines::Main { sending: true },
            VideoLines::Group { slots: 2 },
            VideoLines::Offered,
        ] {
            let mut session = NgcSession::new();
            session.set_video(mode);
            assert_eq!(session.video(), mode);
            let offer = session.read_offer(&captured(name)?)?;
            assert!(offer.as_str().starts_with("v=0\r\n"));
            assert!(!offer.as_str().contains("RTP/SAVP "));
            assert!(!offer.as_str().contains("a=crypto:"));
            assert!(!offer.as_str().contains("MTURNID"));
            let count = session.video_slots().len();
            assert!(count <= 2 || mode == VideoLines::Offered);
            if count > 0 {
                assert!(session.main_video_mid().is_some());
                assert_eq!(session.group_descriptions(false).len(), count + 1);
                assert!(
                    session
                        .group_descriptions(true)
                        .iter()
                        .any(|(_, direction)| *direction == "sendrecv")
                );
            }
            let _ = session.sharing_mids();
            let _ = session.sharing_slot();
            let _ = session.group_sharing_mid();
            let _ = session.data_stream_id();
            let mut again = session.clone();
            assert_eq!(again.read_offer(&captured(name)?)?.as_str(), offer.as_str());
        }
    }
    Ok(())
}

#[test]
fn standard_offers_and_answers_use_the_same_dialect_and_mids() -> TestResult {
    let offer = SdpOffer::new(relayed_standard("browser_offer")?);
    let answer = SdpAnswer::new(relayed_standard("browser_answer")?);
    for mode in [
        VideoLines::None,
        VideoLines::Main { sending: false },
        VideoLines::Main { sending: true },
        VideoLines::Group { slots: 2 },
        VideoLines::Offered,
    ] {
        let mut caller = NgcSession::new();
        caller.set_video(mode);
        caller.mark_sharing("2");
        let ngc = caller.write_offer(&offer)?;
        assert_eq!(ngc.dialect(), Dialect::Ngc);
        assert!(ngc.as_str().contains("m=x-data"));
        assert!(ngc.as_str().contains("typ relay raddr 0.0.0.0 rport 0"));
        assert!(ngc.as_str().contains("a=label:applicationsharing-video"));
        assert!(!ngc.as_str().contains("sdes:mid"));
        assert_eq!(caller.sharing_mids(), vec!["2"]);
        let mut answerer = NgcSession::new();
        answerer.set_video(mode);
        let standard = answerer.read_offer(&ngc)?;
        assert!(standard.as_str().contains("UDP/TLS/RTP/SAVPF"));
        let reply = answerer.write_answer(&answer)?;
        assert!(!reply.as_str().contains("UDP/TLS"));
        let standard = caller.read_answer(&reply)?;
        assert!(standard.as_str().starts_with("v=0\r\n"));
        for name in [
            "callee_answer",
            "caller_renegotiation_answer",
            "renegotiation_answer",
            "controller_answer",
        ] {
            assert!(caller.read_answer(&captured(name)?)?.as_str().starts_with("v=0\r\n"));
        }
        let first = caller.write_offer(&offer)?;
        let again = caller.write_offer(&offer)?;
        let mids = |text: &str| {
            text.lines()
                .filter(|line| line.starts_with("a=mid:"))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(mids(again.as_str()), mids(first.as_str()));
    }
    Ok(())
}

#[test]
fn local_candidates_and_missing_relays_are_refused() -> TestResult {
    let text = relayed_standard("browser_offer")?;
    let local = SdpOffer::new(text.replace("typ relay", "typ host"));
    assert_eq!(
        NgcSession::new().write_offer(&local).unwrap_err(),
        SdpError::LocalCandidate("host".to_owned())
    );
    let without = SdpOffer::new(captured_text("browser_offer")?.to_owned());
    assert!(matches!(
        NgcSession::new().write_offer(&without),
        Err(SdpError::Unwritable(_))
    ));
    for name in ["browser_offer", "browser_answer"] {
        assert!(NgcSdp::new(SDP_NGC, captured_text(name)?).is_err());
    }
    assert!(matches!(
        Dialect::from_content_type("application/sdp"),
        Err(SdpError::ContentType(_))
    ));
    Ok(())
}

#[test]
fn video_and_sharing_metadata_keep_their_wire_meaning() -> TestResult {
    use teams_sdp::sdp::{SDP_NGC_MIXER, SharingLine};

    assert_eq!(Dialect::Ngc.content_type(), SDP_NGC);
    assert_eq!(Dialect::Mixer.content_type(), SDP_NGC_MIXER);
    assert_eq!(captured("mixer_offer")?.content_type(), SDP_NGC_MIXER);
    assert!(captured("invitation_offer")?.carries_video());
    let base = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=video 50000 RTP/SAVP 102\r\na=mid:0\r\na=label:applicationsharing-video\r\n";
    for (direction, expected) in [
        ("sendonly", Some(SharingLine::Sends)),
        ("sendrecv", Some(SharingLine::Sends)),
        ("recvonly", Some(SharingLine::Receives)),
        ("inactive", None),
    ] {
        let ngc = NgcSdp::new(SDP_NGC, &format!("{base}a={direction}\r\n"))?;
        assert_eq!(ngc.sharing_line(), expected);
        assert!(!ngc.carries_video());
    }
    assert_eq!(NgcSdp::new(SDP_NGC, base)?.sharing_line(), Some(SharingLine::Sends));
    assert_eq!(NgcSdp::new(SDP_NGC, &base.replace("50000", "0"))?.sharing_line(), None);
    assert!(NgcSdp::new(SDP_NGC, &base.replace("applicationsharing-video", "main-video"))?.carries_video());
    let mut session = NgcSession::new();
    let offer = SdpOffer::new(relayed_standard("browser_offer")?);
    let ngc = session.write_offer(&offer)?;
    assert_eq!(session.sharing_mids(), vec!["2"]);
    assert!(ngc.as_str().contains("a=label:applicationsharing-video"));
    Ok(())
}
