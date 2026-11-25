//! A group call's control plane in the session's event thread ([`crate::mcp`]): started once the call's media is
//! a group call's ([`super::MediaSession::start_control`]), its `syn` sent when the data channel opens, and the
//! source requests the caller wants ([`super::MediaSession::request_sources`]) sent once the mixer answered: each
//! receiving line's change, only when it changed. A handshake the mixer doesn't answer, the dominant speakers it tells
//! and each source request's result are reported as session events (a result is logged, nothing more).

use std::time::Instant;

use crate::mcp::{CLIENT_CAPABILITIES, ControlPlane, McpEvent, ReceiveLimits};
use crate::rtc::Peer;

use super::{SessionEvent, Shared};

/// Which source the caller wants on which receiving line: the line's stream id, the participant's video source id
/// (`-1` for none), and whether the line is the sharing line (a screen takes larger pictures than a camera).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceRequest {
    pub stream: u32,
    pub source: i64,
    pub screen: bool,
}

impl SourceRequest {
    /// The camera of `source` on the receiving line `stream`.
    #[must_use]
    pub fn camera(stream: u32, source: i64) -> Self {
        Self {
            stream,
            source,
            screen: false,
        }
    }

    /// The screen of `source` on the sharing line `stream`.
    #[must_use]
    pub fn screen(stream: u32, source: i64) -> Self {
        Self {
            stream,
            source,
            screen: true,
        }
    }
}

/// The control plane, and what it is to ask the mixer for.
pub(super) struct Control {
    plane: ControlPlane,
    /// What the caller wants now.
    wanted: Vec<SourceRequest>,
    /// What went, by stream.
    sent: Vec<SourceRequest>,
    started: bool,
}

impl Control {
    /// A control plane sending as `source` (the call's data line's stream id, when the far end named one).
    pub(super) fn new(source: Option<u32>) -> Self {
        Self {
            plane: ControlPlane::new(source, &CLIENT_CAPABILITIES),
            wanted: Vec::new(),
            sent: Vec::new(),
            started: false,
        }
    }

    /// One turn of the network thread: the handshake begun once the data channel is open, its timeout told.
    pub(super) fn turn(&mut self, now: Instant, peer: &Peer, shared: &Shared) {
        if !self.started && peer.channel_open() {
            let syn = self.plane.start(now);
            self.started = peer.send_channel(&syn);
        }
        if let Some(McpEvent::TimedOut) = self.plane.poll(now) {
            shared.report(SessionEvent::ControlTimedOut);
        }
    }

    /// A message the data channel received.
    pub(super) fn receive(&mut self, data: &[u8], peer: &Peer, shared: &Shared) {
        for event in self.plane.receive(data) {
            match event {
                McpEvent::Active { send, receive } => {
                    shared.report(SessionEvent::ControlActive { send, receive });
                    self.flush(peer, shared);
                }
                McpEvent::DominantSpeakers(speakers) => shared.report(SessionEvent::DominantSpeakers(speakers)),
                McpEvent::SourceResult { sequence, result } => {
                    shared.report(SessionEvent::SourceResult { sequence, result });
                }
                McpEvent::TimedOut | McpEvent::Other(_) => {}
            }
        }
    }

    /// The caller wants `wanted` now: each line's change goes once the mixer takes source requests.
    pub(super) fn request(&mut self, wanted: Vec<SourceRequest>, peer: &Peer, shared: &Shared) {
        self.wanted = wanted;
        self.flush(peer, shared);
    }

    /// Sends each wanted line whose source changed since it last went (a line no longer wanted gets `-1`); a line that
    /// never had a source isn't told it has none.
    fn flush(&mut self, peer: &Peer, shared: &Shared) {
        if !self.plane.takes_source_requests() {
            return;
        }
        let had_source = |stream: u32, sent: &[SourceRequest]| {
            sent.iter()
                .any(|request| request.stream == stream && request.source != -1)
        };
        let mut changes: Vec<SourceRequest> = self
            .wanted
            .iter()
            .filter(|wanted| !self.sent.contains(wanted))
            .filter(|wanted| wanted.source != -1 || had_source(wanted.stream, &self.sent))
            .copied()
            .collect();
        changes.extend(
            self.sent
                .iter()
                .filter(|sent| sent.source != -1 && !self.wanted.iter().any(|wanted| wanted.stream == sent.stream))
                .map(|sent| SourceRequest { source: -1, ..*sent }),
        );
        for request in changes {
            let limits = ReceiveLimits::of(request.screen);
            let Ok((sequence, packet)) = self.plane.request_source(request.stream, request.source, limits) else {
                return;
            };
            if !peer.send_channel(&packet) {
                // the channel took nothing: what wasn't sent goes at the next change
                return;
            }
            if request.screen && request.source != -1 {
                shared.video.await_screen();
            }
            shared.report(SessionEvent::SourceRequested { sequence, request });
            self.sent.retain(|sent| sent.stream != request.stream);
            self.sent.push(request);
        }
    }
}
