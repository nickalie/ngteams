//! A call's media, apart from its signaling (`docs/protocol/calling.md`, `docs/protocol/calling-echo.md`).
//!
//! libwebrtc carries it ([`rtc`]): ICE through Teams' relay only, DTLS-SRTP, Opus and H.264 (its software
//! encoder), the jitter buffer, bandwidth estimation, retransmissions and A/V sync. What is ours:
//!
//! - [`sdp`]: Teams' SDP dialect, `application/sdp-ngc-1.0`, to and from libwebrtc's standard SDP, both ways and on
//!   both sides of a call: our offer written for the call start and the media controller's or the callee's answer
//!   read back; the caller's (or the mixer's) offer an invitation carries read in and our answer written for the
//!   acceptance; a renegotiation offered by either side. A browser's SDP is refused where the dialect is required.
//! - [`relay`]: where the media is relayed, Teams' relay or a test's TURN server, with the relay's credentials.
//! - [`session`]: a call's media on threads of its own: the microphone through the audio processing ([`processing`],
//!   libwebrtc's own module) to libwebrtc, libwebrtc's audio to the speaker, the camera's frames to libwebrtc and its
//!   decoded video to the caller's sinks ([`video`]: the camera's bounds and the frames' conversions).
//! - [`health`]: the call's health as libwebrtc's own statistics tell it (Teams' Call health), read when asked.
//! - [`devices`]: the device seam (a microphone, a speaker, a camera): a file backend that opens no audio server, and
//!   PipeWire on Linux (the caller selects the sandbox's or desktop's server), or WASAPI/Media Foundation and
//!   Windows Graphics Capture on Windows. [`tone`] makes and measures the test tone.
//! - [`mcp`]: a group call's media control plane, the messages the client and the call's mixer exchange over the
//!   group call's data channel: whose video the mixer sends on each receiving video line, who speaks.
//! - [`player`]: a video a message carries, played in its card by `GStreamer`'s `playbin`, its bytes from the engine
//!   (an `appsrc`), its frames to the view and its sound to the device seam's speaker.
//!
//! Nothing here sends signaling.

use std::io;

pub mod captions;
pub mod devices;
pub mod health;
pub use teams_sdp::mcp;
pub mod player;
pub mod processing;
mod queue;
pub mod relay;
pub mod rtc;
pub mod sdp;
pub mod session;
pub mod sound_file;
#[cfg(feature = "test-support")]
pub mod stun;
#[cfg(feature = "test-support")]
pub mod test_signal;
#[cfg(feature = "test-support")]
pub mod test_turn;
pub mod tone;
pub mod video;

/// Everything the media can fail with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("socket: {0}")]
    Io(#[from] io::Error),
    #[error("timed out: {0}")]
    Timeout(&'static str),
    /// libwebrtc refused an SDP, a track or its configuration, or the relay allocated nothing.
    #[error("libwebrtc: {0}")]
    Rtc(String),
    /// Media was written before the media was connected.
    #[error("the media is not connected")]
    NotConnected,
    #[error("device: {0}")]
    Device(#[from] devices::DeviceError),
    /// The audio processing module refused its setup or a frame.
    #[error("audio processing: {0}")]
    Processing(String),
    /// The video refused its setup, or failed on a frame.
    #[error("video: {0}")]
    Video(String),
    /// The media session stopped.
    #[error("the media session ended")]
    SessionEnded,
}
