//! A group call's media control plane: the messages the native client and the call's mixer exchange over the data
//! channel of a group call's media (`docs/protocol/calling.md`, "Group video"), sans-IO.
//!
//! - **The data channel** is the client's own, labelled [`CHANNEL_LABEL`], opened on the group call's data line
//!   (`m=x-data` in Teams' dialect, an SCTP association beside the media). Each message on it is one packet
//!   ([`Packetizer`], [`Depacketizer`]): an 11-byte header (a version, the header's length, a start flag and the
//!   message's data id, a sequence number, how many packets follow, the sender's source id, how many recipients)
//!   followed by the recipients' ids (4 bytes each) and the payload; a payload may span several packets, sent one after
//!   the other with consecutive sequence numbers.
//! - **The control plane** ([`ControlPlane`]) is data id [`MCP_DATA_ID`], addressed to the mixer
//!   ([`MIXER_RECIPIENT`]): a payload of one version byte (`1`) and a JSON array of messages, each with its `type`.
//!   The client opens it with `syn` naming what it can take (`client_capabilities`), the mixer answers `ack` with what
//!   it sends and takes (`send_capabilities`, `receive_capabilities`); once it has, the client asks for video sources
//!   (`sr`): which participant's video (`sourceId`, a source id the call's roster names; `-1` for none) the mixer is to
//!   send on which of the client's receiving video lines (`streamMsid`, the line's stream id from the mixer's offer),
//!   with the largest picture the client takes there (`fmtParams`); the mixer answers each (`sr_res`) when it said it
//!   would, and tells who spoke last (`dsh`, the dominant speakers' audio source ids).
//!
//! Nothing here sends or reads a socket: the session's network thread carries the packets (in `teams-media`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The data channel's label (the web client's).
pub const CHANNEL_LABEL: &str = "main-channel";
/// The label of the channel a tests' mixer makes of its own to offer a data line (it takes the client's).
pub const MIXER_CHANNEL_LABEL: &str = "mixer";
/// The control plane's data id.
pub const MCP_DATA_ID: u8 = 18;
/// The recipient the control plane's messages are addressed to: the mixer.
pub const MIXER_RECIPIENT: u32 = 0xFFFF_FFFC;
/// The sender's source id when the client knows none of its own.
pub const UNKNOWN_SOURCE: u32 = 0xFFFF_FFFE;
/// The control plane's payload version.
const MCP_VERSION: u8 = 1;
/// The packet format's version (the header's first nibble).
const PACKET_VERSION: u8 = 1;
/// A packet's fixed header, before the recipients.
const HEADER: usize = 11;
/// The start flag in the header's third byte.
const START: u8 = 0x80;
/// The data id's bits in the header's third byte.
const DATA_ID_BITS: u8 = 63;
/// How long the mixer may take to answer `syn`.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// What the client asks the mixer for in its `syn`: dominant speakers, source requests and their results.
pub const CLIENT_CAPABILITIES: [&str; 2] = ["dsh", "sr"];

/// A packet that doesn't read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("a packet shorter than its header")]
    Short,
    #[error("a packet of format version {0}")]
    Version(u8),
    #[error("a packet that continues no message")]
    NoStart,
    #[error("a packet out of sequence")]
    Sequence,
}

/// One message on the data channel: its data id, its sender's source id, its recipients and its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub data_id: u8,
    pub source: u32,
    pub recipients: Vec<u32>,
    pub data: Vec<u8>,
}

/// Makes one data id's packets: each message one packet, numbered on.
#[derive(Debug, Clone)]
pub struct Packetizer {
    data_id: u8,
    sequence: u16,
}

impl Packetizer {
    /// A packetizer of `data_id`'s messages, numbering from 0.
    #[must_use]
    pub fn new(data_id: u8) -> Self {
        Self {
            data_id: data_id & DATA_ID_BITS,
            sequence: 0,
        }
    }

    /// `data` from `source` to `recipients`, as one packet.
    #[must_use]
    pub fn packetize(&mut self, source: u32, recipients: &[u32], data: &[u8]) -> Vec<u8> {
        let count = u8::try_from(recipients.len()).unwrap_or(u8::MAX);
        let header = HEADER + usize::from(count) * 4;
        let mut packet = Vec::with_capacity(header + data.len());
        let length = u16::try_from(header).unwrap_or(u16::MAX);
        let [high, low] = length.to_be_bytes();
        packet.push((PACKET_VERSION << 4) | (high & 15));
        packet.push(low);
        packet.push(START | self.data_id);
        packet.extend_from_slice(&self.sequence.to_be_bytes());
        packet.push(0);
        packet.extend_from_slice(&source.to_be_bytes());
        packet.push(count);
        for recipient in recipients.iter().take(usize::from(count)) {
            packet.extend_from_slice(&recipient.to_be_bytes());
        }
        packet.extend_from_slice(data);
        self.sequence = self.sequence.wrapping_add(1);
        packet
    }
}

/// A message being put together from its packets.
#[derive(Debug, Clone, Default)]
struct Pending {
    source: u32,
    recipients: Vec<u32>,
    data: Vec<u8>,
    remaining: u8,
    next: u16,
    open: bool,
}

/// Puts messages back together from their packets, per data id.
#[derive(Debug, Clone, Default)]
pub struct Depacketizer {
    pending: HashMap<u8, Pending>,
}

impl Depacketizer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes one packet: the message it completes, if it does.
    ///
    /// # Errors
    /// [`FrameError`] when the packet doesn't read, or continues no message or not the one it should (that message is
    /// dropped).
    pub fn depacketize(&mut self, packet: &[u8]) -> Result<Option<Message>, FrameError> {
        if packet.len() < HEADER {
            return Err(FrameError::Short);
        }
        let version = packet[0] >> 4;
        if version != PACKET_VERSION {
            return Err(FrameError::Version(version));
        }
        let header = (usize::from(packet[0] & 15) << 8) + usize::from(packet[1]);
        let data_id = packet[2] & DATA_ID_BITS;
        let start = packet[2] & START != 0;
        let sequence = u16::from_be_bytes([packet[3], packet[4]]);
        let remaining = packet[5];
        let source = u32::from_be_bytes([packet[6], packet[7], packet[8], packet[9]]);
        let count = usize::from(packet[10]);
        if packet.len() < header || header < HEADER + count * 4 {
            return Err(FrameError::Short);
        }
        let pending = self.pending.entry(data_id).or_default();
        if start {
            let recipients = packet[HEADER..HEADER + count * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|id| u32::from_be_bytes(*id))
                .collect();
            *pending = Pending {
                source,
                recipients,
                data: Vec::new(),
                remaining,
                next: sequence,
                open: true,
            };
        } else {
            if !pending.open {
                return Err(FrameError::NoStart);
            }
            if pending.next != sequence {
                *pending = Pending::default();
                return Err(FrameError::Sequence);
            }
            pending.remaining = pending.remaining.saturating_sub(1);
        }
        pending.next = sequence.wrapping_add(1);
        pending.data.extend_from_slice(&packet[header..]);
        if pending.remaining > 0 {
            return Ok(None);
        }
        let done = std::mem::take(pending);
        Ok(Some(Message {
            data_id,
            source: done.source,
            recipients: done.recipients,
            data: done.data,
        }))
    }
}

/// Where the control plane is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpState {
    /// Not started: the data channel isn't open.
    Idle,
    /// `syn` sent, `ack` awaited.
    Handshaking,
    /// The mixer answered: source requests go.
    Active,
    /// The mixer didn't answer within [`HANDSHAKE_TIMEOUT`].
    TimedOut,
}

/// What the control plane tells.
#[derive(Debug, Clone, PartialEq)]
pub enum McpEvent {
    /// The mixer answered the handshake: what it sends and what it takes.
    Active { send: Vec<String>, receive: Vec<String> },
    /// The mixer didn't answer the handshake in time.
    TimedOut,
    /// The mixer's answer to the source request numbered `sequence` (`ok`, `duplicate`, or another word).
    SourceResult { sequence: u64, result: String },
    /// The dominant speakers as the mixer last told them: their audio source ids, as its message lists them.
    DominantSpeakers(Vec<i64>),
    /// A message of another kind, or one that didn't read: its type.
    Other(String),
}

/// The largest picture a receiving line takes (the source request's `fmtParams`), as the web client counts it: its
/// frame size in macroblocks of 16 by 16 pixels, its frame rate in hundredths of a frame a second (as H.264's
/// `max-fps` is written in Teams' SDP: `3000` is 30 frames a second), and the macroblocks a second they make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiveLimits {
    pub max_fs: u32,
    pub max_fps: u32,
}

impl ReceiveLimits {
    /// 1280 by 720 at 30 frames a second.
    pub const HD: Self = Self {
        max_fs: 3600,
        max_fps: 3000,
    };

    /// Macroblocks a second (`max_fs` times the frame rate).
    #[must_use]
    pub fn max_mbps(self) -> u32 {
        self.max_fs.saturating_mul(self.max_fps) / 100
    }

    fn json(self) -> Value {
        json!([{"max-fs": self.max_fs, "max-mbps": self.max_mbps(), "max-fps": self.max_fps}])
    }
}

/// A control plane's request that can't go.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpError {
    #[error("the control plane isn't active")]
    NotActive,
    #[error("the mixer takes no source requests")]
    NoSourceRequests,
}

/// The client's side of a group call's control plane (see the module docs): the handshake, the source requests, and
/// what the mixer tells. Each method that sends hands back the packet to write on the data channel.
#[derive(Debug, Clone)]
pub struct ControlPlane {
    packetizer: Packetizer,
    depacketizer: Depacketizer,
    source: u32,
    capabilities: Vec<String>,
    state: McpState,
    deadline: Option<Instant>,
    /// What the mixer said it takes (`receive_capabilities`).
    receives: Vec<String>,
    sequence: u64,
}

impl ControlPlane {
    /// A control plane that sends as `source` (the client's own data line's source id, [`UNKNOWN_SOURCE`] when it
    /// knows none) and asks for `capabilities` ([`CLIENT_CAPABILITIES`]).
    #[must_use]
    pub fn new(source: Option<u32>, capabilities: &[&str]) -> Self {
        Self {
            packetizer: Packetizer::new(MCP_DATA_ID),
            depacketizer: Depacketizer::new(),
            source: source.unwrap_or(UNKNOWN_SOURCE),
            capabilities: capabilities.iter().map(|&name| name.to_owned()).collect(),
            state: McpState::Idle,
            deadline: None,
            receives: Vec::new(),
            sequence: 0,
        }
    }

    #[must_use]
    pub fn state(&self) -> McpState {
        self.state
    }

    /// Whether source requests may go now: the mixer answered the handshake and takes them.
    #[must_use]
    pub fn takes_source_requests(&self) -> bool {
        self.state == McpState::Active && self.receives.iter().any(|name| name == "sr")
    }

    /// The data channel opened: the `syn` to send.
    pub fn start(&mut self, now: Instant) -> Vec<u8> {
        self.state = McpState::Handshaking;
        self.deadline = Some(now + HANDSHAKE_TIMEOUT);
        self.receives.clear();
        self.packet(&json!({"type": "syn", "client_capabilities": self.capabilities}))
    }

    /// When [`ControlPlane::poll`] is next due, if ever.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// The handshake's timeout, once it is due.
    pub fn poll(&mut self, now: Instant) -> Option<McpEvent> {
        if self.state == McpState::Handshaking && self.deadline.is_some_and(|deadline| now >= deadline) {
            self.state = McpState::TimedOut;
            self.deadline = None;
            return Some(McpEvent::TimedOut);
        }
        None
    }

    /// Asks the mixer to send the video of `source` (a participant's video source id; `-1` for none) on the receiving
    /// line whose stream id is `stream`, at most `limits`: the packet to send, and the request's number (its result
    /// comes as [`McpEvent::SourceResult`]).
    ///
    /// # Errors
    /// [`McpError`] before the handshake is done, or when the mixer said it takes no source requests.
    pub fn request_source(
        &mut self,
        stream: u32,
        source: i64,
        limits: ReceiveLimits,
    ) -> Result<(u64, Vec<u8>), McpError> {
        if self.state != McpState::Active {
            return Err(McpError::NotActive);
        }
        if !self.takes_source_requests() {
            return Err(McpError::NoSourceRequests);
        }
        self.sequence += 1;
        let message = json!({
            "type": "sr",
            "controlVideoStreaming": {
                "sequenceNumber": self.sequence,
                "controlInfo": {"sourceId": source, "streamMsid": stream, "fmtParams": limits.json()},
            },
        });
        Ok((self.sequence, self.packet(&message)))
    }

    /// Takes one packet the data channel received: what it tells. A packet of another data id, or one that doesn't
    /// read, tells nothing.
    pub fn receive(&mut self, packet: &[u8]) -> Vec<McpEvent> {
        let Ok(Some(message)) = self.depacketizer.depacketize(packet) else {
            return Vec::new();
        };
        if message.data_id != MCP_DATA_ID {
            return Vec::new();
        }
        let Some((&version, json)) = message.data.split_first() else {
            return Vec::new();
        };
        if version != MCP_VERSION {
            return vec![McpEvent::Other(format!("version {version}"))];
        }
        let messages = match serde_json::from_slice::<Value>(json) {
            Ok(Value::Array(messages)) => messages,
            Ok(single @ Value::Object(_)) => vec![single],
            _ => return vec![McpEvent::Other("unreadable".to_owned())],
        };
        messages.iter().filter_map(|message| self.read(message)).collect()
    }

    fn read(&mut self, message: &Value) -> Option<McpEvent> {
        let kind = message.get("type").and_then(Value::as_str).unwrap_or_default();
        match kind {
            "syn" => None,
            "ack" => {
                let send = names(message.get("send_capabilities"));
                let receive = names(message.get("receive_capabilities"));
                self.receives.clone_from(&receive);
                self.state = McpState::Active;
                self.deadline = None;
                Some(McpEvent::Active { send, receive })
            }
            "sr_res" => Some(McpEvent::SourceResult {
                sequence: message
                    .get("sequenceNumber")
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
                result: message
                    .get("result")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            }),
            "dsh" => Some(McpEvent::DominantSpeakers(speakers(message.get("history")))),
            other => Some(McpEvent::Other(other.to_owned())),
        }
    }

    fn packet(&mut self, message: &Value) -> Vec<u8> {
        let mut payload = vec![MCP_VERSION];
        payload.extend_from_slice(json!([message]).to_string().as_bytes());
        self.packetizer.packetize(self.source, &[MIXER_RECIPIENT], &payload)
    }
}

/// A list of names, as a capability list gives them.
fn names(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

/// The audio source ids a dominant speaker history lists, newest first as it lists them: each entry a number, or an
/// object naming its `sourceId`.
fn speakers(value: Option<&Value>) -> Vec<i64> {
    value
        .and_then(Value::as_array)
        .map(|history| {
            history
                .iter()
                .filter_map(|entry| entry.as_i64().or_else(|| entry.get("sourceId").and_then(Value::as_i64)))
                .collect()
        })
        .unwrap_or_default()
}
