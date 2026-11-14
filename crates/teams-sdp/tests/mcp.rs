//! A group call's media control plane (`teams_sdp::mcp`): the data channel's packets and the control plane's
//! messages, as the web client's code makes and reads them and the group video run captured them.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use teams_sdp::mcp::{
    ControlPlane, Depacketizer, FrameError, HANDSHAKE_TIMEOUT, MCP_DATA_ID, MIXER_RECIPIENT, McpError, McpEvent,
    McpState, Message, Packetizer, ReceiveLimits, UNKNOWN_SOURCE,
};

/// The mixer's side of a packet: a control plane message (`messages`, a JSON array) from source 7, to the client.
fn from_mixer(packetizer: &mut Packetizer, messages: &Value) -> Vec<u8> {
    let mut payload = vec![1];
    payload.extend_from_slice(messages.to_string().as_bytes());
    packetizer.packetize(7, &[], &payload)
}

/// What a packet the client sent says: its header's fields and its control plane messages (`None` when it isn't one
/// whole message of the control plane's version 1 in JSON).
fn read_sent(packet: &[u8]) -> Option<(Message, Value)> {
    let message = Depacketizer::new().depacketize(packet).ok()??;
    if message.data.first() != Some(&1) {
        return None;
    }
    let messages = serde_json::from_slice(&message.data[1..]).ok()?;
    Some((message, messages))
}

#[test]
fn a_packet_carries_its_header_as_the_web_client_writes_it() {
    let mut packetizer = Packetizer::new(MCP_DATA_ID);
    let first = packetizer.packetize(426, &[MIXER_RECIPIENT], b"{}");
    // version 1, a 15-byte header (11 and one recipient), the start flag and data id 18, sequence 0, nothing after it,
    // the sender 426, one recipient
    assert_eq!(
        &first[..15],
        &[
            0x10,
            15,
            0x80 | 0x12,
            0,
            0,
            0,
            0,
            0,
            0x01,
            0xaa,
            1,
            0xff,
            0xff,
            0xff,
            0xfc
        ]
    );
    assert_eq!(&first[15..], b"{}");
    let second = packetizer.packetize(426, &[MIXER_RECIPIENT], b"[]");
    assert_eq!(u16::from_be_bytes([second[3], second[4]]), 1, "numbered on");
    let read = Depacketizer::new().depacketize(&second).unwrap().unwrap();
    assert_eq!(
        read,
        Message {
            data_id: MCP_DATA_ID,
            source: 426,
            recipients: vec![MIXER_RECIPIENT],
            data: b"[]".to_vec(),
        }
    );
}

/// A message in `parts` packets, as a sender that fragments makes them: the first with the start flag and how many
/// follow, each next one numbered on.
fn fragments(data_id: u8, parts: &[&[u8]; 3], first_sequence: u16) -> Vec<Vec<u8>> {
    let follow = 2;
    parts
        .iter()
        .zip(0_u16..)
        .map(|(part, index)| {
            let sequence = first_sequence.wrapping_add(index);
            let mut packet = vec![0x10, 11, if index == 0 { 0x80 | data_id } else { data_id }];
            packet.extend_from_slice(&sequence.to_be_bytes());
            packet.push(if index == 0 { follow } else { 0 });
            packet.extend_from_slice(&5_u32.to_be_bytes());
            packet.push(0);
            packet.extend_from_slice(part);
            packet
        })
        .collect()
}

#[test]
fn a_message_in_several_packets_is_put_back_together() {
    let mut depacketizer = Depacketizer::new();
    let packets = fragments(MCP_DATA_ID, &[b"ab", b"cd", b"ef"], u16::MAX);
    assert_eq!(depacketizer.depacketize(&packets[0]), Ok(None));
    assert_eq!(depacketizer.depacketize(&packets[1]), Ok(None), "the sequence wraps");
    let message = depacketizer.depacketize(&packets[2]).unwrap().unwrap();
    assert_eq!(message.data, b"abcdef");
    assert_eq!(message.source, 5);
    // a continuation with nothing before it, and one out of sequence, are refused
    assert_eq!(depacketizer.depacketize(&packets[1]), Err(FrameError::NoStart));
    let packets = fragments(MCP_DATA_ID, &[b"ab", b"cd", b"ef"], 10);
    assert_eq!(depacketizer.depacketize(&packets[0]), Ok(None));
    assert_eq!(depacketizer.depacketize(&packets[2]), Err(FrameError::Sequence));
    assert_eq!(
        depacketizer.depacketize(&packets[1]),
        Err(FrameError::NoStart),
        "the message was dropped"
    );
    assert_eq!(depacketizer.depacketize(&[0x10, 11]), Err(FrameError::Short));
    assert_eq!(
        depacketizer.depacketize(&[0x20, 11, 0x80, 0, 0, 0, 0, 0, 0, 0, 0]),
        Err(FrameError::Version(2))
    );
}

#[test]
fn the_handshake_then_source_requests() {
    let now = Instant::now();
    let mut plane = ControlPlane::new(Some(426), &["dsh", "sr"]);
    assert_eq!(plane.state(), McpState::Idle);
    assert_eq!(
        plane.request_source(416, 415, ReceiveLimits::HD),
        Err(McpError::NotActive),
        "nothing before the handshake"
    );
    let (header, syn) = read_sent(&plane.start(now)).unwrap();
    assert_eq!(
        (header.source, header.recipients.as_slice()),
        (426, &[MIXER_RECIPIENT][..])
    );
    assert_eq!(syn, json!([{"type": "syn", "client_capabilities": ["dsh", "sr"]}]));
    assert_eq!(plane.state(), McpState::Handshaking);

    let mut mixer = Packetizer::new(MCP_DATA_ID);
    let ack = json!([{"type": "ack", "send_capabilities": ["dsh", "sr_res"], "receive_capabilities": ["sr"]}]);
    assert_eq!(
        plane.receive(&from_mixer(&mut mixer, &ack)),
        [McpEvent::Active {
            send: vec!["dsh".into(), "sr_res".into()],
            receive: vec!["sr".into()],
        }]
    );
    assert!(plane.takes_source_requests());
    assert_eq!(plane.deadline(), None);

    let (sequence, packet) = plane.request_source(416, 415, ReceiveLimits::HD).unwrap();
    let (_, sr) = read_sent(&packet).unwrap();
    assert_eq!(
        sr,
        json!([{"type": "sr", "controlVideoStreaming": {"sequenceNumber": sequence, "controlInfo": {
            "sourceId": 415, "streamMsid": 416,
            "fmtParams": [{"max-fs": 3600, "max-mbps": 108_000, "max-fps": 3000}],
        }}}])
    );
    let (next, _) = plane.request_source(417, -1, ReceiveLimits::HD).unwrap();
    assert_eq!(next, sequence + 1, "numbered on");

    let told = plane.receive(&from_mixer(
        &mut mixer,
        &json!([{"type": "sr_res", "sequenceNumber": sequence, "result": "ok"},
                {"type": "dsh", "history": [414, 514]},
                {"type": "bwe", "bw": 1_000_000}]),
    ));
    assert_eq!(
        told,
        [
            McpEvent::SourceResult {
                sequence,
                result: "ok".into()
            },
            McpEvent::DominantSpeakers(vec![414, 514]),
            McpEvent::Other("bwe".into()),
        ]
    );
}

#[test]
fn a_mixer_that_takes_no_source_requests_gets_none() {
    let mut plane = ControlPlane::new(None, &["dsh", "sr"]);
    let (header, _) = read_sent(&plane.start(Instant::now())).unwrap();
    assert_eq!(header.source, UNKNOWN_SOURCE);
    let mut mixer = Packetizer::new(MCP_DATA_ID);
    plane.receive(&from_mixer(
        &mut mixer,
        &json!([{"type": "ack", "send_capabilities": ["dsh"], "receive_capabilities": []}]),
    ));
    assert_eq!(plane.state(), McpState::Active);
    assert_eq!(
        plane.request_source(416, 415, ReceiveLimits::HD),
        Err(McpError::NoSourceRequests)
    );
}

#[test]
fn an_unanswered_handshake_times_out_once() {
    let now = Instant::now();
    let mut plane = ControlPlane::new(Some(1), &["sr"]);
    plane.start(now);
    assert_eq!(plane.poll(now + Duration::from_secs(1)), None);
    assert_eq!(plane.poll(now + HANDSHAKE_TIMEOUT), Some(McpEvent::TimedOut));
    assert_eq!(plane.state(), McpState::TimedOut);
    assert_eq!(plane.poll(now + HANDSHAKE_TIMEOUT * 2), None);
}

#[test]
fn what_isnt_the_control_plane_tells_nothing() {
    let mut plane = ControlPlane::new(Some(1), &["sr"]);
    plane.start(Instant::now());
    // another data id, a broken packet, a payload of another version, JSON that isn't messages
    let mut other = Packetizer::new(0);
    assert!(plane.receive(&other.packetize(7, &[], b"\x01[]")).is_empty());
    assert!(plane.receive(&[1, 2, 3]).is_empty());
    let mut mixer = Packetizer::new(MCP_DATA_ID);
    assert_eq!(
        plane.receive(&mixer.packetize(7, &[], b"\x02[]")),
        [McpEvent::Other("version 2".into())]
    );
    assert_eq!(
        plane.receive(&mixer.packetize(7, &[], b"\x01not json")),
        [McpEvent::Other("unreadable".into())]
    );
    assert_eq!(plane.state(), McpState::Handshaking);
}
