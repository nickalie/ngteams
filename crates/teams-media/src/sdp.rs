//! Teams' SDP dialect, with libwebrtc validation at the media boundary.

pub use teams_sdp::sdp::{Dialect, NgcSdp, SDP_NGC, SDP_NGC_MIXER, SHARING_LABEL, SdpError, SharingLine, VideoLines};

use crate::rtc::{SdpAnswer, SdpOffer};

/// A call's dialect session, validating converted standard SDP with libwebrtc.
#[derive(Clone, Debug, Default)]
pub struct NgcSession(teams_sdp::sdp::NgcSession);

impl NgcSession {
    /// A session with no m-line yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets which of the video lines offered to us the call takes from the next offer read on
    /// ([`NgcSession::read_offer`]).
    pub fn set_video(&mut self, lines: VideoLines) {
        self.0.set_video(lines);
    }

    /// Which of the video lines offered to us the call takes.
    #[must_use]
    pub fn video(&self) -> VideoLines {
        self.0.video()
    }

    /// Names the line libwebrtc calls `local` (the one our screen goes on) the sharing line: labelled
    /// `applicationsharing-video` once our offer adds it.
    pub fn mark_sharing(&mut self, local: &str) {
        self.0.mark_sharing(local);
    }

    /// libwebrtc's mids of the call's sharing lines (ours and the far end's, in use or not): their frames are a
    /// screen's, never a camera's.
    #[must_use]
    pub fn sharing_mids(&self) -> Vec<String> {
        self.0.sharing_mids()
    }

    /// A group call's sharing line, once the call took it: libwebrtc's mid and the stream id the far end named it by
    /// (what a source request for someone's screen names it by).
    #[must_use]
    pub fn sharing_slot(&self) -> Option<(String, Option<u32>)> {
        self.0.sharing_slot()
    }

    /// A group call's sharing line by Teams mid, once the call took it: what its media descriptions name it by while
    /// the account's screen goes out on it.
    #[must_use]
    pub fn group_sharing_mid(&self) -> Option<&str> {
        self.0.group_sharing_mid()
    }

    /// A group call's receiving video lines, in the order the call took them: each line's libwebrtc mid and its stream
    /// id (what a source request names it by), when the far end named one.
    #[must_use]
    pub fn video_slots(&self) -> Vec<(String, Option<u32>)> {
        self.0.video_slots()
    }

    /// A group call's video lines as its requests describe them (`mediaDescriptions`, by Teams mid, as the web client
    /// describes them): the main line `sendrecv` while the camera is on (`sending`), `recvonly` while it is off, and
    /// each receiving line `recvonly`.
    #[must_use]
    pub fn group_descriptions(&self, sending: bool) -> Vec<(String, &'static str)> {
        self.0.group_descriptions(sending)
    }

    /// The Teams mid of the call's main video line, once it has one.
    #[must_use]
    pub fn main_video_mid(&self) -> Option<&str> {
        self.0.main_video_mid()
    }

    /// The stream id of the call's data line (the source id the client sends its control plane messages as), when
    /// the far end named one.
    #[must_use]
    pub fn data_stream_id(&self) -> Option<u32> {
        self.0.data_stream_id()
    }

    /// Our offer (libwebrtc's, at a call's start or in a renegotiation) in Teams' dialect, for the start's, a join's or
    /// a `mediaNegotiation`'s `mediaContent`. A line libwebrtc adds is numbered one past the highest number the session
    /// has, and labelled as the web client labels a line of its kind (the last video line of an offer with more than
    /// one, while the call has no sharing line, is the sharing line).
    ///
    /// # Errors
    /// [`SdpError::LocalCandidate`] when the offer carries a candidate that isn't relayed, [`SdpError::Unwritable`]
    /// when a line has no relayed candidate, no mid, or a kind Teams' dialect has no form for, [`SdpError::Random`]
    /// when the data line's SSRC can't be drawn.
    pub fn write_offer(&mut self, offer: &SdpOffer) -> Result<NgcSdp, SdpError> {
        self.0.write_offer(&teams_sdp::SdpOffer::new(offer.to_sdp_string()))
    }

    /// The answer to our offer (the media controller's, or a callee's in a 1:1 call), for libwebrtc. A line of an
    /// offer we never made (the media controller's `m=x-data` to an offer without one) is left out. A codec the answer
    /// numbers otherwise than our offer did keeps the answer's number: libwebrtc sends with the answer's numbers and
    /// takes the far end's media with our offer's, as the web client does (the media controller answers Opus offered as
    /// `111` with `102`, and the mixer H.264 with `107`).
    ///
    /// # Errors
    /// [`SdpError::Mismatch`] when no line of the answer is one of ours, [`SdpError::Refused`] when libwebrtc refuses
    /// what the conversion made.
    pub fn read_answer(&mut self, answer: &NgcSdp) -> Result<SdpAnswer, SdpError> {
        let answer = self.0.read_answer(answer)?;
        SdpAnswer::from_sdp_string(answer.as_str()).map_err(SdpError::Refused)
    }

    /// An offer we received (the caller's or the mixer's in an invitation, or a renegotiation), for libwebrtc. Lines
    /// the session knows keep their libwebrtc mids; a new line keeps Teams' mid on our side too. A rejected line that names
    /// no mid is the session's line at its place (or, new to the call, numbered as a line we add).
    ///
    /// # Errors
    /// [`SdpError::NotNgc`] when a line that isn't rejected names no mid, or one too long for libwebrtc;
    /// [`SdpError::Refused`] when libwebrtc refuses what the conversion made.
    pub fn read_offer(&mut self, offer: &NgcSdp) -> Result<SdpOffer, SdpError> {
        let offer = self.0.read_offer(offer)?;
        SdpOffer::from_sdp_string(offer.as_str()).map_err(SdpError::Refused)
    }

    /// Our answer (libwebrtc's) to an offer [`NgcSession::read_offer`] read, in Teams' dialect, for the `accept`'s or a
    /// `mediaAnswer`'s `mediaContent`.
    ///
    /// # Errors
    /// [`SdpError::Mismatch`] when the answer has a line the offer didn't, [`SdpError::LocalCandidate`] and
    /// [`SdpError::Unwritable`] as for [`NgcSession::write_offer`].
    pub fn write_answer(&self, answer: &SdpAnswer) -> Result<NgcSdp, SdpError> {
        self.0.write_answer(&teams_sdp::SdpAnswer::new(answer.to_sdp_string()))
    }
}
