//! Teams' SDP dialect and media control plane, independent of a WebRTC engine.

pub mod mcp;
pub mod sdp;
mod text;

pub use text::{SdpAnswer, SdpOffer};
