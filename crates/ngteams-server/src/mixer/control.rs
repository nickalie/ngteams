//! The mixer's end of a leg's control plane: MCP messages on the client's `main-channel` (`teams_sdp::mcp`). A `syn`
//! is answered with an `ack` naming what the mixer sends and takes; the mixer tells the dominant speakers (`dsh`); a
//! source request (`sr`: which source a receiving line is to show, and the largest picture it takes) is answered
//! `sr_res` `ok` and handed to the room.
//! Any other message is ignored: the native client runs without `heartbeat`, `bwe` and the rest.

use serde_json::{Value, json};
use teams_sdp::mcp::{Depacketizer, MCP_DATA_ID, MIXER_RECIPIENT, Packetizer};

/// What the mixer's `ack` says it sends and takes: the dominant speakers and the answers to source requests out, source
/// requests in (without `sr` the client asks for no video).
const SEND_CAPABILITIES: [&str; 2] = ["dsh", "sr_res"];
const RECEIVE_CAPABILITIES: [&str; 1] = ["sr"];
/// The version byte in front of every message array.
const VERSION: u8 = 1;

/// A source request: the receiving line (by its stream id) is to show the source `source` (`-1`: nothing), in
/// pictures of at most `max_fs` macroblocks (its `fmtParams`' `max-fs`; none when it names none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SourceRequest {
    pub stream: u32,
    pub source: i64,
    pub max_fs: Option<u32>,
}

/// What a packet of the client's came to.
#[derive(Debug, Default)]
pub(crate) struct Received {
    /// The packets that answer it.
    pub answers: Vec<Vec<u8>>,
    pub requests: Vec<SourceRequest>,
}

/// One leg's control plane.
#[derive(Debug)]
pub(crate) struct Control {
    depacketizer: Depacketizer,
    packetizer: Packetizer,
    acked: bool,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            depacketizer: Depacketizer::new(),
            packetizer: Packetizer::new(MCP_DATA_ID),
            acked: false,
        }
    }
}

impl Control {
    /// Whether the client's handshake was answered.
    pub(crate) fn acked(&self) -> bool {
        self.acked
    }

    /// A packet the client sent on its channel: the packets that answer it, and the source requests it made.
    pub(crate) fn receive(&mut self, packet: &[u8]) -> Received {
        let mut received = Received::default();
        let Ok(Some(message)) = self.depacketizer.depacketize(packet) else {
            return received;
        };
        let Some((&VERSION, body)) = message.data.split_first() else {
            return received;
        };
        let Ok(Value::Array(messages)) = serde_json::from_slice::<Value>(body) else {
            return received;
        };
        for message in messages {
            match message.get("type").and_then(Value::as_str) {
                Some("syn") => {
                    self.acked = true;
                    let ack = json!([{"type": "ack", "send_capabilities": SEND_CAPABILITIES,
                        "receive_capabilities": RECEIVE_CAPABILITIES}]);
                    received.answers.push(self.packet(&ack));
                }
                Some("sr") if self.acked => self.source_request(&message, &mut received),
                _ => {}
            }
        }
        received
    }

    /// A source request: kept when it names a line and a source, answered `ok` either way.
    fn source_request(&mut self, message: &Value, received: &mut Received) {
        let streaming = message.get("controlVideoStreaming");
        let info = streaming.and_then(|streaming| streaming.get("controlInfo"));
        let stream = info
            .and_then(|info| info.get("streamMsid")?.as_u64())
            .and_then(|stream| u32::try_from(stream).ok());
        let source = info.and_then(|info| info.get("sourceId")?.as_i64());
        let max_fs = info
            .and_then(|info| info.pointer("/fmtParams/0/max-fs")?.as_u64())
            .and_then(|max_fs| u32::try_from(max_fs).ok());
        if let (Some(stream), Some(source)) = (stream, source) {
            received.requests.push(SourceRequest { stream, source, max_fs });
        }
        let sequence = streaming
            .and_then(|streaming| streaming.get("sequenceNumber"))
            .cloned()
            .unwrap_or(Value::Null);
        let answer = json!([{"type": "sr_res", "sequenceNumber": sequence, "result": "ok"}]);
        received.answers.push(self.packet(&answer));
    }

    /// The packet that tells the dominant speakers, `history` the newest first.
    pub(crate) fn dominant(&mut self, history: &[u32]) -> Vec<u8> {
        self.packet(&json!([{"type": "dsh", "history": history}]))
    }

    fn packet(&mut self, messages: &Value) -> Vec<u8> {
        let mut payload = vec![VERSION];
        payload.extend_from_slice(messages.to_string().as_bytes());
        self.packetizer.packetize(MIXER_RECIPIENT, &[], &payload)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use teams_sdp::mcp::{CLIENT_CAPABILITIES, ControlPlane, McpEvent, McpState, ReceiveLimits};

    use super::*;

    #[test]
    fn the_clients_handshake_is_acked_and_it_hears_the_dominant_speakers() {
        let mut client = ControlPlane::new(Some(1003), &CLIENT_CAPABILITIES);
        let mut mixer = Control::default();
        let syn = client.start(Instant::now());
        assert!(!mixer.acked());
        let received = mixer.receive(&syn);
        assert!(mixer.acked());
        let [ack] = received.answers.as_slice() else {
            panic!("one answer: {received:?}");
        };
        client.receive(ack);
        assert_eq!(client.state(), McpState::Active);
        assert!(client.takes_source_requests());
        let told = client.receive(&mixer.dominant(&[1010, 1000]));
        assert!(told.contains(&McpEvent::DominantSpeakers(vec![1010, 1000])), "{told:?}");
    }

    #[test]
    fn a_source_request_is_handed_on_and_answered_ok() {
        let now = Instant::now();
        let mut client = ControlPlane::new(Some(1003), &CLIENT_CAPABILITIES);
        let mut mixer = Control::default();
        let syn = client.start(now);
        for ack in mixer.receive(&syn).answers {
            client.receive(&ack);
        }
        let (sequence, asked) = client.request_source(1004, 1021, ReceiveLimits::HD).unwrap();
        let received = mixer.receive(&asked);
        assert_eq!(
            received.requests,
            [SourceRequest {
                stream: 1004,
                source: 1021,
                max_fs: Some(3600),
            }]
        );
        let [answer] = received.answers.as_slice() else {
            panic!("one answer: {received:?}");
        };
        let told = client.receive(answer);
        assert!(
            told.contains(&McpEvent::SourceResult {
                sequence,
                result: "ok".to_owned()
            }),
            "{told:?}"
        );
        // one that names no line is answered and kept back
        let mut packetizer = Packetizer::new(MCP_DATA_ID);
        let unnamed = packetizer.packetize(1003, &[MIXER_RECIPIENT], b"\x01[{\"type\":\"sr\"}]");
        let received = mixer.receive(&unnamed);
        assert!(received.requests.is_empty());
        assert_eq!(received.answers.len(), 1);
    }

    #[test]
    fn what_isnt_a_handshake_is_ignored() {
        let mut mixer = Control::default();
        let mut packetizer = Packetizer::new(MCP_DATA_ID);
        let mut message = |payload: &[u8]| {
            mixer
                .receive(&packetizer.packetize(1003, &[MIXER_RECIPIENT], payload))
                .answers
        };
        assert!(message(b"\x01[{\"type\":\"heartbeat\"}]").is_empty());
        assert!(message(b"\x02[{\"type\":\"syn\"}]").is_empty());
        assert!(message(b"\x01{\"type\":\"syn\"}").is_empty());
        assert!(message(b"").is_empty());
        // before the handshake, a source request is nothing
        assert!(message(b"\x01[{\"type\":\"sr\"}]").is_empty());
        assert!(mixer.receive(b"not a packet").answers.is_empty());
        assert!(!mixer.acked());
    }
}
