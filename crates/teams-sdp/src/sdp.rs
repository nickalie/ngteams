//! Teams' SDP dialect, `application/sdp-ngc-1.0`, to and from the standard SDP libwebrtc speaks
//! (`docs/protocol/calling.md`, "The SDP dialect").
//!
//! The call controller and the media controller take and give offers and answers only in their own dialect (a
//! `mediaContent`'s `blob` beside its `contentType`); a group call's invitation carries the mixer's offer in an older
//! one, `application/sdp-ngc-0.5` ([`Dialect::Mixer`]), which is read the same way and never written. The web client
//! converts between the two at its `RTCPeerConnection`, and so does [`NgcSession`], line for line as the captures show
//! it:
//!
//! - **Our side, written** ([`NgcSession::write_offer`], [`NgcSession::write_answer`]): `RTP/SAVP` instead of
//!   `UDP/TLS/RTP/SAVPF`, the data channel's `m=application … webrtc-datachannel` as `m=x-data … RTP/SAVP 127 126`;
//!   the m-line's port and `c=` those of our relayed candidate, inline; `b=CT:4000`, `a=rtcp:<port>` in an offer,
//!   Teams' own `a=x-signaling-fb`, `a=x-ssrc-range` and `a=label`; the two extension URIs Teams writes with
//!   backslashes so written; a rejected line as `m=<kind> 0 RTP/SAVP …` with its label only, left out of the bundle.
//!   Only relayed candidates go out, with no related address: a host or server-reflexive candidate (a local address)
//!   is refused ([`SdpError::LocalCandidate`]).
//! - **Their side, read** ([`NgcSession::read_answer`], [`NgcSession::read_offer`]): back to the standard
//!   transports; Teams' own attributes, labels, `a=rtcp`, SDES keys (`a=crypto`, the mixer's) and every attribute
//!   libwebrtc has no use for dropped; `a=rtcp-fb:*` written out per payload type (without Teams' `x-message`);
//!   backslashes in extension URIs turned back; candidates kept for component 1 over UDP only, Teams' own extensions
//!   (`MTURNID`) dropped; an `a=ssrc` (with a cname) added from `a=x-ssrc-range` or `a=ssrc-group` where the line
//!   names none, as the web client does; what a line leaves out (the mixer's lines name no `a=setup`) taken from the
//!   bundle's first line; an offer that names no `a=setup` (the mixer's) is read as `actpass`, so that libwebrtc
//!   answers `active`, the DTLS client, as the web client does (and keeps the call's role in a renegotiation).
//! - **Mids**: libwebrtc names its m-lines with mids of its own; Teams' lines are numbered (`0`, `1`, … in our offers;
//!   the mixer numbers its lines from `1` and out of line order). The session keeps the pairs for the whole call, so
//!   a renegotiation offered by either side names the same lines, and keeps each line's label; a line new to the call
//!   is numbered one past the highest number the session has.
//! - **Labels**: a line of ours takes the label the web client gives a line of its kind (`main-audio`, `main-video`,
//!   `data`), except the sharing line: the line our screen goes on ([`NgcSession::mark_sharing`]), or else the last
//!   video line of an offer with more than one while the call has none, is `applicationsharing-video`, as in the web
//!   client's 1:1 and group offers.
//! - **A screen shared, 1:1** (`docs/protocol/calling.md`, "More of a running call"): the far end's offer adding a
//!   sharing line it sends on is taken, read as one it only sends on, so libwebrtc answers it `recvonly` as the web
//!   client does; the sharing line of ours a far end's offer names receiving is taken as offered. A sharing line
//!   offered `inactive` (every 1:1 start's and invitation's) stays rejected. Which lines are sharing lines
//!   ([`NgcSession::sharing_mids`]) is what routes their frames apart from the camera's.
//! - **A group call's sharing line** goes both ways in our offers and answers, as the web client's: someone's screen
//!   comes on it, and ours goes on it without a renegotiation. While libwebrtc sends on it (our idle screen track is on
//!   it), its own streams stay named, as the web client names its; otherwise it names a stream of its own, as a
//!   receiving line does.
//!
//! - **Video** ([`NgcSession::set_video`], [`VideoLines`]): which of an offer's video lines the call takes is the
//!   session's to say before libwebrtc sees it: an audio-only call (a group call's, until it takes video) reads every
//!   video line as rejected, so libwebrtc answers each one rejected as an audio-only client does; a 1:1 call takes its
//!   main video line (the first one that isn't the sharing line, then the same line for the call's life) and reads
//!   the others rejected. While our camera is off, the line taken is read as one the far end only sends on (its
//!   `sendrecv` as `sendonly`, its `recvonly` as `inactive`), so libwebrtc answers that we only receive, and our own
//!   offer writes that line `recvonly` (the camera's track stays on it, sending nothing). A group call's main and
//!   receiving lines are answered both ways, as the web client answers them, whichever way libwebrtc took them; each
//!   receiving line names a stream of its own, drawn once for the call, on which nothing is ever sent.
//!
//! A browser's SDP (or libwebrtc's as it is) is refused where the dialect is required ([`NgcSdp::new`]).

use std::fmt;

use crate::{SdpAnswer, SdpOffer};

/// The content type of Teams' SDP dialect: every offer and answer the native client sends, and what the call
/// controller and the media controller hand back.
pub const SDP_NGC: &str = "application/sdp-ngc-1.0";

/// The content type of the mixer's older dialect, which a group call's invitation carries (read only).
pub const SDP_NGC_MIXER: &str = "application/sdp-ngc-0.5";

/// The extension URIs Teams writes with backslashes for slashes (the others go as they are).
const BACKSLASHED: [&str; 2] = [
    "http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time",
    "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01",
];

/// The attributes of an audio or video line the standard side keeps; everything else is Teams' own (or of no use to
/// libwebrtc) and is dropped.
const KEPT_RTP: [&str; 21] = [
    "rtpmap",
    "fmtp",
    "rtcp-fb",
    "extmap",
    "setup",
    "mid",
    "msid",
    "sendrecv",
    "sendonly",
    "recvonly",
    "inactive",
    "ice-ufrag",
    "ice-pwd",
    "fingerprint",
    "candidate",
    "ice-options",
    "ssrc",
    "ssrc-group",
    "rtcp-mux",
    "rtcp-rsize",
    "ptime",
];

/// The attributes of a data line the standard side keeps.
const KEPT_DATA: [&str; 9] = [
    "setup",
    "mid",
    "ice-ufrag",
    "ice-pwd",
    "fingerprint",
    "candidate",
    "ice-options",
    "sctp-port",
    "max-message-size",
];

/// What attributes a line's transport takes from the bundle's first line when it leaves them out.
const INHERITED: [&str; 4] = ["ice-ufrag", "ice-pwd", "fingerprint", "setup"];

/// The SCTP port of a data channel whose line names none (the web client's choice for the media controller's).
const DEFAULT_SCTP_PORT: &str = "5000";

/// The cname given to a stream Teams names only by its SSRC.
const SYNTHESIZED_CNAME: &str = "teams-ngc";

/// The label of the screen-sharing video line: the last video line of the web client's offer.
pub const SHARING_LABEL: &str = "applicationsharing-video";

/// The bandwidth line of every session the web client writes.
const SESSION_BANDWIDTH: &str = "b=CT:4000";

/// Which of Teams' dialects an SDP is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// [`SDP_NGC`]: what the native client writes, and what the services hand back.
    Ngc,
    /// [`SDP_NGC_MIXER`]: a group call's mixer's offer in its invitation; read, never written.
    Mixer,
}

impl Dialect {
    /// The dialect a `mediaContent`'s `contentType` names.
    ///
    /// # Errors
    /// [`SdpError::ContentType`] for any other content type.
    pub fn from_content_type(content_type: &str) -> Result<Self, SdpError> {
        match content_type {
            SDP_NGC => Ok(Self::Ngc),
            SDP_NGC_MIXER => Ok(Self::Mixer),
            other => Err(SdpError::ContentType(other.to_owned())),
        }
    }

    /// The `contentType` that goes beside an SDP in this dialect.
    #[must_use]
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Ngc => SDP_NGC,
            Self::Mixer => SDP_NGC_MIXER,
        }
    }
}

/// Why an SDP can't be taken or written.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SdpError {
    /// The `contentType` names neither of Teams' dialects.
    #[error("not one of Teams' SDP dialects: content type {0:?}")]
    ContentType(String),
    /// The blob isn't an SDP in Teams' dialect (a browser's or libwebrtc's, or not an SDP at all).
    #[error("not an SDP in Teams' dialect: {0}")]
    NotNgc(&'static str),
    /// A line in use isn't on Teams' profile (`RTP/SAVP`; the mixer's dialect also `RTP/AVP`): a browser's SDP, or a
    /// form no capture showed. Its kind, port and profile, nothing else of it.
    #[error("not an SDP in Teams' dialect: a {media} line on port {port} is on {proto}, not RTP/SAVP")]
    Profile { media: String, port: String, proto: String },
    /// libwebrtc's SDP can't be written in Teams' dialect.
    #[error("libwebrtc's SDP can't be written in Teams' dialect: {0}")]
    Unwritable(&'static str),
    /// An offer or answer of ours would carry a local address: only relayed candidates go out.
    #[error("only relayed candidates go out, not a {0} candidate (a local address)")]
    LocalCandidate(String),
    /// An answer names lines of an offer we never made, or our answer lines of an offer we never read.
    #[error("the m-lines don't match the offer: {0}")]
    Mismatch(&'static str),
    /// libwebrtc refused the standard SDP the conversion produced.
    #[error("libwebrtc refused the SDP: {0}")]
    Refused(String),
    /// No random source for the data line's SSRC.
    #[error("no random source: {0}")]
    Random(String),
}

/// An SDP in Teams' dialect, as a `mediaContent` carries it: checked to be one (never a browser's), with its dialect.
#[derive(Clone, PartialEq, Eq)]
pub struct NgcSdp {
    dialect: Dialect,
    text: String,
}

impl fmt::Debug for NgcSdp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // the text holds ICE credentials and addresses: never in a log
        f.debug_struct("NgcSdp")
            .field("dialect", &self.dialect)
            .field("media", &self.text.matches("\nm=").count())
            .finish_non_exhaustive()
    }
}

impl NgcSdp {
    /// Takes a `mediaContent`'s `blob` with its `contentType`.
    ///
    /// # Errors
    /// [`SdpError::ContentType`] when the content type is neither dialect's; [`SdpError::NotNgc`] when the blob
    /// isn't an SDP, has no media line, or has one that isn't Teams' (a browser's `UDP/TLS/RTP/SAVPF` or
    /// `m=application`, the placeholder port `9`, the mixer's `RTP/AVP` outside its dialect).
    pub fn new(content_type: &str, blob: &str) -> Result<Self, SdpError> {
        let dialect = Dialect::from_content_type(content_type)?;
        let parsed = Parsed::ngc(blob, dialect)?;
        if parsed.media.is_empty() {
            return Err(SdpError::NotNgc("no media line"));
        }
        Ok(Self {
            dialect,
            text: blob.to_owned(),
        })
    }

    /// Its dialect.
    #[must_use]
    pub fn dialect(&self) -> Dialect {
        self.dialect
    }

    /// The `contentType` that goes beside it.
    #[must_use]
    pub fn content_type(&self) -> &'static str {
        self.dialect.content_type()
    }

    /// The SDP text: the `blob`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Whether a video line other than the sharing line is in use (on a port, going either way, not `inactive`): the
    /// media carries video. The web client's answer to a share, its main video line `inactive`, names no `Video`
    /// modality (seen 2026-11-06).
    #[must_use]
    pub fn carries_video(&self) -> bool {
        self.video_in_use()
            .any(|section| section.value("label") != Some(SHARING_LABEL) && section.direction() != Some("inactive"))
    }

    /// Which way the sharing line goes, as its writer sends or receives on it, while one is in use: `None` without
    /// one, or with one `inactive`.
    #[must_use]
    pub fn sharing_line(&self) -> Option<SharingLine> {
        let section = self
            .video_in_use()
            .find(|section| section.value("label") == Some(SHARING_LABEL))?;
        match section.direction() {
            Some("sendonly" | "sendrecv") | None => Some(SharingLine::Sends),
            Some("recvonly") => Some(SharingLine::Receives),
            _ => None,
        }
    }

    /// The video sections on a port.
    fn video_in_use(&self) -> impl Iterator<Item = Section> {
        Parsed::ngc(&self.text, self.dialect)
            .map(|parsed| parsed.media)
            .unwrap_or_default()
            .into_iter()
            .filter(|section| section.kind == Kind::Video && !section.rejected())
    }
}

/// Which way an SDP's sharing line goes, as the SDP's writer has it ([`NgcSdp::sharing_line`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharingLine {
    /// Its writer shares a screen on it (`sendonly`).
    Sends,
    /// Its writer is shown a screen on it (`recvonly`).
    Receives,
}

/// A media line's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Audio,
    Video,
    Data,
}

impl Kind {
    fn ngc(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Data => "x-data",
        }
    }

    fn standard(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Data => "application",
        }
    }

    /// The label the web client gives a line of the kind (a video line other than the sharing one,
    /// [`SHARING_LABEL`]).
    fn label(self) -> &'static str {
        match self {
            Self::Audio => "main-audio",
            Self::Video => "main-video",
            Self::Data => "data",
        }
    }
}

/// One media section: its m-line and the lines under it (`c=`, `b=`, `a=`).
#[derive(Clone, Debug)]
struct Section {
    kind: Kind,
    port: u16,
    formats: Vec<String>,
    lines: Vec<String>,
}

impl Section {
    /// The values of every `a=<name>:<value>` line.
    fn values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.lines.iter().filter_map(move |line| attribute(line, name))
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.lines.iter().find_map(|line| attribute(line, name))
    }

    fn has(&self, name: &str) -> bool {
        self.lines.iter().any(|line| attribute_name(line) == Some(name))
    }

    fn mid(&self) -> Option<&str> {
        self.value("mid")
    }

    fn rejected(&self) -> bool {
        self.port == 0
    }

    fn connection(&self) -> Option<&str> {
        self.lines
            .iter()
            .find(|line| line.starts_with("c="))
            .map(String::as_str)
    }

    fn direction(&self) -> Option<&'static str> {
        ["sendrecv", "sendonly", "recvonly", "inactive"]
            .into_iter()
            .find(|direction| self.has(direction))
    }
}

/// An SDP split into its session part and its media sections.
#[derive(Clone, Debug)]
struct Parsed {
    session: Vec<String>,
    media: Vec<Section>,
}

/// An m-line split into its fields.
struct MediaLine<'a> {
    media: &'a str,
    port: &'a str,
    proto: &'a str,
    formats: Vec<String>,
}

fn media_line(line: &str) -> Option<MediaLine<'_>> {
    let mut fields = line.strip_prefix("m=")?.split(' ');
    let media = fields.next()?;
    let port = fields.next()?;
    let proto = fields.next()?;
    Some(MediaLine {
        media,
        port,
        proto,
        formats: fields.filter(|format| !format.is_empty()).map(str::to_owned).collect(),
    })
}

/// The SDP's lines, CRLF or LF, without empty ones.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.is_empty())
}

impl Parsed {
    /// Splits `text` into sections, each m-line read by `read`.
    fn split(text: &str, read: impl Fn(&MediaLine<'_>) -> Result<(Kind, u16), SdpError>) -> Result<Self, SdpError> {
        let mut all = lines(text);
        if all.next() != Some("v=0") {
            return Err(SdpError::NotNgc("no session part"));
        }
        let mut session = vec!["v=0".to_owned()];
        let mut media: Vec<Section> = Vec::new();
        for line in all {
            if let Some(fields) = media_line(line) {
                let (kind, port) = read(&fields)?;
                media.push(Section {
                    kind,
                    port,
                    formats: fields.formats,
                    lines: Vec::new(),
                });
            } else if let Some(section) = media.last_mut() {
                section.lines.push(line.to_owned());
            } else {
                session.push(line.to_owned());
            }
        }
        Ok(Self { session, media })
    }

    /// Reads an SDP in Teams' `dialect`.
    fn ngc(text: &str, dialect: Dialect) -> Result<Self, SdpError> {
        Self::split(text, |fields| {
            // a rejected line (port 0) carries nothing: it may name any profile
            let proto_ok = fields.proto == "RTP/SAVP"
                || (dialect == Dialect::Mixer && fields.proto == "RTP/AVP")
                || fields.port == "0";
            if !proto_ok {
                return Err(SdpError::Profile {
                    media: fields.media.to_owned(),
                    port: fields.port.to_owned(),
                    proto: fields.proto.to_owned(),
                });
            }
            let kind = match fields.media {
                "audio" => Kind::Audio,
                "video" => Kind::Video,
                "x-data" => Kind::Data,
                _ => return Err(SdpError::NotNgc("a media line of an unknown kind")),
            };
            let port = fields
                .port
                .parse::<u16>()
                .map_err(|_| SdpError::NotNgc("a media line's port isn't a number"))?;
            if port == 9 {
                return Err(SdpError::NotNgc(
                    "a media line on the placeholder port 9 (a browser's SDP?)",
                ));
            }
            Ok((kind, port))
        })
    }

    /// Reads libwebrtc's standard SDP.
    fn standard(text: &str) -> Result<Self, SdpError> {
        Self::split(text, |fields| {
            let kind = match (fields.media, fields.proto) {
                ("audio", "UDP/TLS/RTP/SAVPF") => Kind::Audio,
                ("video", "UDP/TLS/RTP/SAVPF") => Kind::Video,
                ("application", "UDP/DTLS/SCTP" | "DTLS/SCTP") => Kind::Data,
                _ => {
                    return Err(SdpError::Unwritable(
                        "a media line of a kind Teams' dialect has no form for",
                    ));
                }
            };
            let port = fields
                .port
                .parse::<u16>()
                .map_err(|_| SdpError::Unwritable("a media line's port isn't a number"))?;
            Ok((kind, port))
        })
    }

    fn session_value(&self, name: &str) -> Option<&str> {
        self.session.iter().find_map(|line| attribute(line, name))
    }

    /// The mids of the `a=group:BUNDLE` line.
    fn bundle(&self) -> Vec<&str> {
        self.session_value("group")
            .and_then(|group| group.strip_prefix("BUNDLE "))
            .map(|mids| mids.split(' ').collect())
            .unwrap_or_default()
    }
}

/// The value of `line` when it is `a=<name>:<value>`.
fn attribute<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    line.strip_prefix("a=")?.strip_prefix(name)?.strip_prefix(':')
}

/// The name of an `a=` line.
fn attribute_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("a=")?;
    Some(rest.split_once(':').map_or(rest, |(name, _)| name))
}

/// One m-line of a call in both namings: libwebrtc's mid, Teams' mid, and the line's label; `ours` when an offer of
/// ours added it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Line {
    local: String,
    remote: String,
    label: String,
    ours: bool,
}

/// Which of the video lines offered to us a call takes ([`NgcSession::set_video`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VideoLines {
    /// None: every video line is read rejected (an audio-only call's).
    #[default]
    None,
    /// The main video line, receiving; sending too when `sending` (our camera is on).
    Main { sending: bool },
    /// A group call's: the main video line as offered (the mixer offers it sending and receiving, and the web client
    /// answers it so; what we send on it the camera decides, and the call's services are told by signaling), and up
    /// to `slots` more video lines, taken as offered (both ways, as the web client answers them; described receive-only
    /// and nothing sent on them): each carries one participant's video,
    /// the one a source request chose ([`crate::mcp`]). The sharing line is taken as offered too (both ways, as the web
    /// client answers it): whoever shares their screen, the one a source request chose, comes on it. Any other video
    /// line is read rejected.
    Group { slots: usize },
    /// Every video line as the far end offered it: a mixer's side (the tests' mixer answering a client's group
    /// offer), which sends on every line the client receives on.
    Offered,
}

/// What a call takes an offered video line as ([`NgcSession::with_video_lines`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Taken {
    No,
    Main,
    Slot,
    /// As offered ([`VideoLines::Offered`]).
    Offered,
    /// A 1:1 call's sharing line: the far end's screen, read as one it only sends on; or ours, which it receives, as
    /// offered.
    Sharing,
    /// A group call's sharing line (the mixer's): taken as offered, a stream of its own named as a receiving line's.
    GroupSharing,
}

/// One call's media session as Teams' dialect sees it: the pairs of libwebrtc's and Teams' mids for every m-line and
/// each line's label, kept for the call's life so that a renegotiation, offered by either side, names the same
/// lines. On the caller's side our offer numbers the lines; on the callee's side the offer it reads does.
#[derive(Clone, Debug, Default)]
pub struct NgcSession {
    lines: Vec<Line>,
    /// Which video lines offered to us the call takes.
    video: VideoLines,
    /// The Teams mid of the video line the call took (or our offer added), kept for the call's life.
    video_line: Option<String>,
    /// A group call's receiving video lines, by Teams mid, in the order the call took them (or our offer added them).
    slots: Vec<String>,
    /// Each line's stream id (`a=x-source-streamid`) as the far end's last offer or answer named it, by Teams mid.
    stream_ids: Vec<(String, u32)>,
    /// A group call's receiving lines' own streams (an SSRC each, by Teams mid), drawn once when the line is taken
    /// and named by every offer and answer of the call: nothing is ever sent on them.
    slot_streams: Vec<(String, u32)>,
    /// libwebrtc's mid of the line our screen goes on, once [`NgcSession::mark_sharing`] named it: labelled the sharing
    /// line when our offer adds it.
    our_sharing: Option<String>,
    /// A group call's sharing line, by Teams mid: the mixer's (taken as offered) or our group offer's.
    group_sharing: Option<String>,
}

/// Which half of an exchange an SDP is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Offer,
    Answer,
}

impl NgcSession {
    /// A session with no m-line yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets which of the video lines offered to us the call takes from the next offer read on
    /// ([`NgcSession::read_offer`]).
    pub fn set_video(&mut self, lines: VideoLines) {
        self.video = lines;
    }

    /// Which of the video lines offered to us the call takes.
    #[must_use]
    pub fn video(&self) -> VideoLines {
        self.video
    }

    /// `parsed` (an offer to us) with the video lines the call doesn't take read as rejected (and left out of the
    /// bundle), and the main one it takes read as one the far end only sends on while a 1:1 call's camera is off; a
    /// group call's main line and receiving lines are taken as offered.
    fn with_video_lines(&mut self, mut parsed: Parsed) -> Parsed {
        let mut refused = Vec::new();
        for section in &mut parsed.media {
            if section.kind != Kind::Video || section.rejected() {
                continue;
            }
            let mid = section.mid().map(str::to_owned);
            match self.take_video(section, mid.as_deref()) {
                Taken::No => {
                    section.port = 0;
                    refused.extend(mid);
                }
                Taken::Main => {
                    if self.video_line.is_none() {
                        self.video_line = mid;
                    }
                    if self.video == (VideoLines::Main { sending: false }) {
                        only_theirs(section);
                    }
                }
                Taken::Slot => {
                    if let Some(mid) = mid
                        && !self.slots.contains(&mid)
                    {
                        self.take_slot(mid);
                    }
                    // taken as offered (both ways): the web client answers the receiving lines it shows both ways
                    // and describes them receive-only; the mixer refuses a line described receive-only that the
                    // answer made receive-only already (`410 InvalidFinalAnswerMediaDescriptions`, M3 Task 19). Nothing
                    // is sent on them.
                }
                Taken::Sharing => {
                    // the far end's screen: answered receiving; ours, which it receives, as offered
                    if section.direction() != Some("recvonly") {
                        only_theirs(section);
                    }
                }
                Taken::GroupSharing => {
                    if let Some(mid) = mid {
                        self.take_sharing(mid);
                    }
                }
                Taken::Offered => {}
            }
        }
        if !refused.is_empty() {
            for line in &mut parsed.session {
                if let Some(mids) = line.strip_prefix("a=group:BUNDLE ") {
                    let kept: Vec<&str> = mids
                        .split(' ')
                        .filter(|mid| !refused.iter().any(|r| r == mid))
                        .collect();
                    *line = format!("a=group:BUNDLE {}", kept.join(" "));
                }
            }
        }
        parsed
    }

    /// Whether the call takes the offered video line `section` (Teams mid `mid`), and as what.
    fn take_video(&self, section: &Section, mid: Option<&str>) -> Taken {
        let Some(mid) = mid else { return Taken::No };
        let sharing = section.value("label") == Some(SHARING_LABEL);
        match (self.video, &self.video_line) {
            (VideoLines::None, _) => Taken::No,
            (VideoLines::Offered, _) => Taken::Offered,
            (_, Some(line)) if line == mid => Taken::Main,
            (VideoLines::Main { .. } | VideoLines::Group { .. }, None) if !sharing => Taken::Main,
            (VideoLines::Group { .. }, _) if self.slots.iter().any(|slot| slot == mid) => Taken::Slot,
            (VideoLines::Group { slots }, Some(_)) if !sharing && self.slots.len() < slots => Taken::Slot,
            (VideoLines::Group { .. }, _) if sharing && self.group_sharing.as_ref().is_none_or(|line| line == mid) => {
                Taken::GroupSharing
            }
            // a 1:1 call's sharing line in use: the far end's screen, or ours it receives (never one offered
            // `inactive`, as every start and invitation offers it)
            (VideoLines::Main { .. }, _) if sharing && section.direction() != Some("inactive") => Taken::Sharing,
            _ => Taken::No,
        }
    }

    /// Names the line libwebrtc calls `local` (the one our screen goes on) the sharing line: labelled
    /// `applicationsharing-video` once our offer adds it.
    pub fn mark_sharing(&mut self, local: &str) {
        self.our_sharing = Some(local.to_owned());
    }

    /// libwebrtc's mids of the call's sharing lines (ours and the far end's, in use or not): their frames are a
    /// screen's, never a camera's.
    #[must_use]
    pub fn sharing_mids(&self) -> Vec<String> {
        self.lines
            .iter()
            .filter(|line| line.label == SHARING_LABEL)
            .map(|line| line.local.clone())
            .collect()
    }

    /// A group call's sharing line, once the call took it: libwebrtc's mid and the stream id the far end named it by
    /// (what a source request for someone's screen names it by).
    #[must_use]
    pub fn sharing_slot(&self) -> Option<(String, Option<u32>)> {
        let remote = self.group_sharing.as_deref()?;
        let line = self.by_remote(remote)?;
        Some((line.local.clone(), self.stream_id(remote)))
    }

    /// A group call's sharing line by Teams mid, once the call took it: what its media descriptions name it by while
    /// the account's screen goes out on it.
    #[must_use]
    pub fn group_sharing_mid(&self) -> Option<&str> {
        self.group_sharing.as_deref()
    }

    /// Takes a group call's sharing line `mid` (by Teams mid), with a stream of its own as a receiving line's.
    fn take_sharing(&mut self, mid: String) {
        if !self.slot_streams.iter().any(|(slot, _)| *slot == mid) {
            self.slot_streams.push((mid.clone(), fresh_ssrc()));
        }
        self.group_sharing = Some(mid);
    }

    /// Takes the receiving line `mid` (by Teams mid), with a stream of its own.
    fn take_slot(&mut self, mid: String) {
        if !self.slot_streams.iter().any(|(slot, _)| *slot == mid) {
            self.slot_streams.push((mid.clone(), fresh_ssrc()));
        }
        self.slots.push(mid);
    }

    /// A group call's receiving video lines, in the order the call took them: each line's libwebrtc mid and its stream
    /// id (what a source request names it by), when the far end named one.
    #[must_use]
    pub fn video_slots(&self) -> Vec<(String, Option<u32>)> {
        self.slots
            .iter()
            .filter_map(|remote| {
                let line = self.by_remote(remote)?;
                Some((line.local.clone(), self.stream_id(remote)))
            })
            .collect()
    }

    /// A group call's video lines as its requests describe them (`mediaDescriptions`, by Teams mid, as the web client
    /// describes them): the main line `sendrecv` while the camera is on (`sending`), `recvonly` while it is off, and
    /// each receiving line `recvonly`.
    #[must_use]
    pub fn group_descriptions(&self, sending: bool) -> Vec<(String, &'static str)> {
        let main = self
            .video_line
            .iter()
            .map(|mid| (mid.clone(), if sending { "sendrecv" } else { "recvonly" }));
        main.chain(self.slots.iter().map(|mid| (mid.clone(), "recvonly")))
            .collect()
    }

    /// The Teams mid of the call's main video line, once it has one.
    #[must_use]
    pub fn main_video_mid(&self) -> Option<&str> {
        self.video_line.as_deref()
    }

    /// The stream id of the call's data line (the source id the client sends its control plane messages as), when
    /// the far end named one.
    #[must_use]
    pub fn data_stream_id(&self) -> Option<u32> {
        self.lines
            .iter()
            .find(|line| line.label == Kind::Data.label())
            .and_then(|line| self.stream_id(&line.remote))
    }

    /// The stream id the far end named the line with Teams mid `remote` by.
    fn stream_id(&self, remote: &str) -> Option<u32> {
        self.stream_ids.iter().find(|(mid, _)| mid == remote).map(|(_, id)| *id)
    }

    /// Keeps the stream id each line of the far end's offer or answer names.
    fn keep_stream_ids(&mut self, parsed: &Parsed) {
        for section in &parsed.media {
            let (Some(mid), Some(id)) = (
                section.mid(),
                section.value("x-source-streamid").and_then(|id| id.parse::<u32>().ok()),
            ) else {
                continue;
            };
            self.stream_ids.retain(|(known, _)| known != mid);
            self.stream_ids.push((mid.to_owned(), id));
        }
    }

    fn by_local(&self, mid: &str) -> Option<&Line> {
        self.lines.iter().find(|line| line.local == mid)
    }

    fn by_remote(&self, mid: &str) -> Option<&Line> {
        self.lines.iter().find(|line| line.remote == mid)
    }

    /// The Teams mid of a line new to the call: one past the highest number the session has. Neither the count of
    /// lines nor a line's place will do: the mixer numbers its lines from `1` and out of line order.
    fn next_remote(&self) -> String {
        self.lines
            .iter()
            .filter_map(|line| line.remote.parse::<u32>().ok())
            .max()
            .map_or(0, |highest| highest + 1)
            .to_string()
    }

    /// The place among `parsed`'s sections of the line that becomes the sharing line when libwebrtc adds it: the last
    /// video line of an offer with more than one, while the call has no sharing line yet.
    fn sharing_place(&self, parsed: &Parsed) -> Option<usize> {
        if self.our_sharing.is_some() || self.lines.iter().any(|line| line.label == SHARING_LABEL) {
            return None;
        }
        let video: Vec<usize> = parsed
            .media
            .iter()
            .enumerate()
            .filter(|(_, section)| section.kind == Kind::Video)
            .map(|(place, _)| place)
            .collect();
        if video.len() > 1 { video.last().copied() } else { None }
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
        let parsed = Parsed::standard(&offer.to_sdp_string())?;
        self.forget_unanswered(&parsed);
        let sharing = self.sharing_place(&parsed);
        let mut names = Vec::new();
        for (place, section) in parsed.media.iter().enumerate() {
            let local = section.mid().ok_or(SdpError::Unwritable("an m-line without a mid"))?;
            if self.by_local(local).is_none() {
                let label = if sharing == Some(place) || self.our_sharing.as_deref() == Some(local) {
                    SHARING_LABEL
                } else {
                    section.kind.label()
                };
                self.lines.push(Line {
                    local: local.to_owned(),
                    remote: self.next_remote(),
                    label: label.to_owned(),
                    ours: true,
                });
            }
            let name = self
                .by_local(local)
                .cloned()
                .ok_or(SdpError::Unwritable("an m-line without a mid"))?;
            if matches!(self.video, VideoLines::Group { .. })
                && name.label == SHARING_LABEL
                && self.our_sharing.is_none()
                && self.group_sharing.is_none()
            {
                // a group offer's sharing line: whoever shares their screen comes on it
                self.group_sharing = Some(name.remote.clone());
            }
            if section.kind == Kind::Video && !section.rejected() && name.label != SHARING_LABEL {
                let main = self.video_line.get_or_insert_with(|| name.remote.clone()).clone();
                // a group call's offer: every other video line but the sharing one is one we receive on
                if matches!(self.video, VideoLines::Group { .. })
                    && main != name.remote
                    && !self.slots.contains(&name.remote)
                {
                    self.take_slot(name.remote.clone());
                }
            }
            names.push(name);
        }
        let parsed = self.directed(parsed, &names, Role::Offer);
        write(&parsed, &names, Role::Offer)
    }

    /// Forgets the lines an earlier offer of ours added that `parsed` (our new offer) no longer has: that offer was
    /// turned away and replaced, so no answer committed them (libwebrtc offers every line the session has), and the new
    /// offer's lines are numbered and its video line taken as if they had never been offered.
    fn forget_unanswered(&mut self, parsed: &Parsed) {
        let offered = |line: &Line| {
            parsed
                .media
                .iter()
                .any(|section| section.mid() == Some(line.local.as_str()))
        };
        let (kept, forgotten): (Vec<Line>, Vec<Line>) = std::mem::take(&mut self.lines)
            .into_iter()
            .partition(|line| !line.ours || offered(line));
        self.lines = kept;
        if self
            .video_line
            .as_ref()
            .is_some_and(|video| forgotten.iter().any(|line| &line.remote == video))
        {
            self.video_line = None;
        }
    }

    /// The answer to our offer (the media controller's, or a callee's in a 1:1 call), for libwebrtc. A line of an
    /// offer we never made (the media controller's `m=x-data` to an offer without one) is left out. A codec the answer
    /// numbers otherwise than our offer did keeps the answer's number: libwebrtc sends with the answer's numbers and
    /// takes the far end's media with our offer's, as the web client does (the media controller answers Opus offered as
    /// `111` with `102`, and the mixer H.264 with `107`).
    ///
    /// # Errors
    /// [`SdpError::Mismatch`] when no line of the answer is one of ours.
    pub fn read_answer(&mut self, answer: &NgcSdp) -> Result<SdpAnswer, SdpError> {
        let parsed = Parsed::ngc(answer.as_str(), answer.dialect())?;
        self.keep_stream_ids(&parsed);
        let mut ours = Vec::new();
        for (index, section) in parsed.media.iter().enumerate() {
            let line = match section.mid() {
                Some(mid) => self.by_remote(mid),
                // a rejected line names no mid: it is the offer's line at its place
                None if section.rejected() => self.lines.get(index),
                None => None,
            };
            if let Some(line) = line {
                ours.push((section.clone(), line.local.clone()));
            }
        }
        let kept: Vec<(&Section, String)> = ours.iter().map(|(section, local)| (section, local.clone())).collect();
        if kept.is_empty() {
            return Err(SdpError::Mismatch("the answer has none of the offer's m-lines"));
        }
        let text = read(&parsed, &kept, Role::Answer, |mid| {
            self.by_remote(mid).map(|line| line.local.clone())
        });
        Ok(SdpAnswer::new(text))
    }

    /// An offer we received (the caller's or the mixer's in an invitation, or a renegotiation), for libwebrtc. Lines
    /// the session knows keep their libwebrtc mids; a new line keeps Teams' mid on our side too. A rejected line that names
    /// no mid is the session's line at its place (or, new to the call, numbered as a line we add).
    ///
    /// # Errors
    /// [`SdpError::NotNgc`] when a line that isn't rejected names no mid, or one too long for libwebrtc.
    pub fn read_offer(&mut self, offer: &NgcSdp) -> Result<SdpOffer, SdpError> {
        let parsed = Parsed::ngc(offer.as_str(), offer.dialect())?;
        self.keep_stream_ids(&parsed);
        let parsed = self.with_video_lines(parsed);
        let mut kept = Vec::new();
        for (index, section) in parsed.media.iter().enumerate() {
            let remote = match section.mid() {
                Some(mid) => mid.to_owned(),
                // the m-lines keep their order for the call: a rejected line is the one at its place
                None if section.rejected() => self
                    .lines
                    .get(index)
                    .map_or_else(|| self.next_remote(), |line| line.remote.clone()),
                None => return Err(SdpError::NotNgc("an offered m-line without a mid")),
            };
            if remote.len() > 16 {
                return Err(SdpError::NotNgc("a mid longer than 16 characters"));
            }
            if self.by_remote(&remote).is_none() {
                self.lines.push(Line {
                    local: remote.clone(),
                    remote: remote.clone(),
                    label: section.value("label").unwrap_or(section.kind.label()).to_owned(),
                    ours: false,
                });
            }
            let local = self.by_remote(&remote).map(|line| line.local.clone()).unwrap_or(remote);
            kept.push((section, local));
        }
        let text = read(&parsed, &kept, Role::Offer, |mid| {
            self.by_remote(mid).map(|line| line.local.clone())
        });
        Ok(SdpOffer::new(text))
    }

    /// Our answer (libwebrtc's) to an offer [`NgcSession::read_offer`] read, in Teams' dialect, for the `accept`'s or a
    /// `mediaAnswer`'s `mediaContent`.
    ///
    /// # Errors
    /// [`SdpError::Mismatch`] when the answer has a line the offer didn't, [`SdpError::LocalCandidate`] and
    /// [`SdpError::Unwritable`] as for [`NgcSession::write_offer`].
    pub fn write_answer(&self, answer: &SdpAnswer) -> Result<NgcSdp, SdpError> {
        let parsed = Parsed::standard(&answer.to_sdp_string())?;
        let names = parsed
            .media
            .iter()
            .map(|section| {
                section
                    .mid()
                    .and_then(|mid| self.by_local(mid))
                    .cloned()
                    .ok_or(SdpError::Mismatch("the answer has an m-line the offer didn't"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let parsed = self.directed(parsed, &names, Role::Answer);
        write(&parsed, &names, Role::Answer)
    }

    /// `parsed` (ours) with each video line going as Teams is to hear it: in an offer, a 1:1 call's main line
    /// `recvonly` while the camera is off (libwebrtc keeps the camera's track on it, sending nothing); in an answer, a
    /// group call's main and receiving lines both ways, as the web client answers them (libwebrtc answers a line it has
    /// no track for receive-only; the mixer refuses that: `410 SdpParsingFailure`).
    fn directed(&self, mut parsed: Parsed, names: &[Line], role: Role) -> Parsed {
        // the call's own cname, as libwebrtc names its streams (a receiving line's stream takes it too)
        let call_cname = parsed
            .media
            .iter()
            .flat_map(|section| section.values("ssrc"))
            .find_map(|value| value.split_once(" cname:").map(|(_, cname)| cname.to_owned()))
            .unwrap_or_else(|| SYNTHESIZED_CNAME.to_owned());
        for (section, name) in parsed.media.iter_mut().zip(names) {
            if section.kind != Kind::Video || section.rejected() {
                continue;
            }
            let main = self.video_line.as_deref() == Some(name.remote.as_str());
            let sharing = self.group_sharing.as_deref() == Some(name.remote.as_str());
            // a group call's sharing line answered both ways, as a receiving line is (the web client answers it so)
            let slot = self.slots.contains(&name.remote) || (role == Role::Answer && sharing);
            // libwebrtc sends on it (our screen's idle track is on it): its own streams stay named, as the web
            // client's are, so the screen goes out on the stream the mixer was told
            let screen =
                sharing && section.has("ssrc") && !matches!(section.direction(), Some("recvonly" | "inactive"));
            match (role, self.video) {
                (_, VideoLines::Group { .. }) if screen => directed(section, "sendrecv"),
                (Role::Offer, VideoLines::Main { sending: false }) if main => directed(section, "recvonly"),
                // a receiving line sends nothing, yet goes both ways naming a stream of its own, as the web client
                // writes the receiving lines it shows: the mixer refuses a line going both ways that names the
                // placeholder stream (`410 SdpParsingFailure`), and one going receive-only that it describes so
                // (`410 InvalidFinalAnswerMediaDescriptions`)
                (_, VideoLines::Group { .. }) if slot => {
                    directed(section, "sendrecv");
                    unnamed(section);
                    if let Some((_, ssrc)) = self.slot_streams.iter().find(|(mid, _)| *mid == name.remote) {
                        section.lines.push(format!("a=ssrc:{ssrc} cname:{call_cname}"));
                    }
                }
                (Role::Answer, VideoLines::Group { .. }) if main && section.direction() == Some("recvonly") => {
                    directed(section, "sendrecv");
                }
                _ => {}
            }
            // a receive-only line names no stream of ours (libwebrtc names the camera's track on it anyway)
            if matches!(section.direction(), Some("recvonly" | "inactive")) {
                unnamed(section);
            }
        }
        parsed
    }
}

/// A stream id (SSRC) of our own, drawn at random: never `0` or the placeholder stream [`PLACEHOLDER_SSRC`].
fn fresh_ssrc() -> u32 {
    let mut bytes = [0; 4];
    if getrandom::fill(&mut bytes).is_err() {
        // no randomness to be had: the clock's nanoseconds, distinct enough for the lines of one call
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.subsec_nanos());
        bytes = nanos.to_be_bytes();
    }
    (u32::from_be_bytes(bytes) & 0x7fff_ffff).max(PLACEHOLDER_SSRC + 1)
}

/// `section` going `direction`; a receive-only or inactive line names no stream of ours.
fn directed(section: &mut Section, direction: &str) {
    section.lines.retain(|line| {
        !["sendrecv", "sendonly", "recvonly", "inactive"]
            .iter()
            .any(|d| line == &format!("a={d}"))
    });
    if direction == "recvonly" || direction == "inactive" {
        unnamed(section);
    }
    section.lines.push(format!("a={direction}"));
}

/// `section` naming no stream of ours (its SSRCs and media stream left out).
fn unnamed(section: &mut Section) {
    section
        .lines
        .retain(|line| !matches!(attribute_name(line), Some("ssrc" | "ssrc-group" | "msid")));
}

/// A video line the far end offered to send and receive on, read as one it only sends on (our camera is off).
fn only_theirs(section: &mut Section) {
    let mut found = false;
    for line in &mut section.lines {
        let turned = match line.as_str() {
            "a=sendrecv" => "a=sendonly",
            "a=recvonly" => "a=inactive",
            "a=sendonly" | "a=inactive" => line.as_str(),
            _ => continue,
        };
        found = true;
        *line = turned.to_owned();
    }
    if !found {
        section.lines.push("a=sendonly".to_owned());
    }
}

// ---- standard → Teams' dialect ----

/// Writes `parsed` (libwebrtc's) in Teams' dialect, each section named by its entry of `names`.
fn write(parsed: &Parsed, names: &[Line], role: Role) -> Result<NgcSdp, SdpError> {
    let mut foundations = Vec::new();
    let candidates = parsed
        .media
        .iter()
        .map(|section| relayed_candidates(section, &mut foundations))
        .collect::<Result<Vec<_>, _>>()?;
    // a bundled line carries no candidate of its own: it goes where the first one does, as the web client writes it
    let bundle_default = candidates
        .iter()
        .find_map(|ours| ours.first.clone())
        .ok_or(SdpError::Unwritable("no relayed candidate"))?;
    let mut out = Vec::new();
    write_session(parsed, names, &mut out)?;
    for ((section, name), ours) in parsed.media.iter().zip(names).zip(candidates) {
        if section.rejected() {
            // a rejected line names the static format the web client's rejected lines name (`34` for video; `0`, PCMU,
            // for audio), whatever libwebrtc listed
            let formats = match section.kind {
                Kind::Video => "34".to_owned(),
                Kind::Audio => "0".to_owned(),
                Kind::Data if section.formats.is_empty() => "0".to_owned(),
                Kind::Data => section.formats.join(" "),
            };
            out.push(format!("m={} 0 RTP/SAVP {formats}", section.kind.ngc()));
            out.push(format!("a=label:{}", name.label));
        } else {
            let at = ours.first.unwrap_or_else(|| bundle_default.clone());
            let written = Written {
                candidates: ours.lines,
                at,
            };
            write_section(section, name, role, written, &mut out)?;
        }
    }
    let mut text = out.join("\r\n");
    text.push_str("\r\n");
    NgcSdp::new(SDP_NGC, &text)
}

/// A section's candidates as Teams writes them, and the address and port its m-line and `c=` go to.
struct Written {
    candidates: Vec<String>,
    at: (String, String),
}

fn write_session(parsed: &Parsed, names: &[Line], out: &mut Vec<String>) -> Result<(), SdpError> {
    let origin = parsed
        .session
        .iter()
        .find_map(|line| line.strip_prefix("o="))
        .ok_or(SdpError::Unwritable("no origin line"))?;
    let fields: Vec<&str> = origin.split(' ').collect();
    let (Some(id), Some(version)) = (fields.get(1), fields.get(2)) else {
        return Err(SdpError::Unwritable("an origin line without a session id"));
    };
    out.extend([
        "v=0".to_owned(),
        format!("o=- {id} {version} IN IP4 127.0.0.1"),
        "s=-".to_owned(),
        SESSION_BANDWIDTH.to_owned(),
        "t=0 0".to_owned(),
    ]);
    if parsed.session.iter().any(|line| line == "a=extmap-allow-mixed") {
        out.push("a=extmap-allow-mixed".to_owned());
    }
    out.push("a=msid-semantic: WMS *".to_owned());
    let bundled: Vec<&str> = parsed
        .media
        .iter()
        .zip(names)
        .filter(|(section, _)| !section.rejected())
        .map(|(_, name)| name.remote.as_str())
        .collect();
    if !bundled.is_empty() {
        out.push(format!("a=group:BUNDLE {}", bundled.join(" ")));
    }
    Ok(())
}

/// A section's relayed candidates in Teams' form, and the address and port of the first, if it has any.
struct Relayed {
    lines: Vec<String>,
    first: Option<(String, String)>,
}

/// Our relayed candidates of a section, rewritten as Teams' candidates (numbered foundations, no related address,
/// no extension), with the address and port of the first, if it has any.
fn relayed_candidates(section: &Section, foundations: &mut Vec<String>) -> Result<Relayed, SdpError> {
    let mut written = Vec::new();
    let mut default = None;
    for candidate in section.values("candidate") {
        let fields: Vec<&str> = candidate.split(' ').collect();
        let [
            foundation,
            component,
            transport,
            priority,
            address,
            port,
            "typ",
            kind,
            ..,
        ] = fields[..]
        else {
            return Err(SdpError::Unwritable("a candidate that doesn't read"));
        };
        if kind != "relay" {
            return Err(SdpError::LocalCandidate(kind.to_owned()));
        }
        if component != "1" {
            continue;
        }
        if !foundations.iter().any(|known| known == foundation) {
            foundations.push(foundation.to_owned());
        }
        let number = foundations
            .iter()
            .position(|known| known == foundation)
            .unwrap_or_default()
            + 1;
        written.push(format!(
            "a=candidate:{number} 1 {transport} {priority} {address} {port} typ relay raddr 0.0.0.0 rport 0"
        ));
        default.get_or_insert((address.to_owned(), port.to_owned()));
    }
    Ok(Relayed {
        lines: written,
        first: default,
    })
}

fn write_section(
    section: &Section,
    name: &Line,
    role: Role,
    written: Written,
    out: &mut Vec<String>,
) -> Result<(), SdpError> {
    let Written {
        candidates,
        at: (address, port),
    } = written;
    let family = if address.contains(':') { "IP6" } else { "IP4" };
    let formats = match section.kind {
        Kind::Data => "127 126".to_owned(),
        Kind::Audio | Kind::Video => section.formats.join(" "),
    };
    out.push(format!("m={} {port} RTP/SAVP {formats}", section.kind.ngc()));
    out.push(format!("c=IN {family} {address}"));
    if section.kind == Kind::Data {
        write_data_payload(out)?;
    } else {
        write_rtp_payload(section, &port, role, out);
    }
    write_transport(section, name, candidates, out);
    if section.kind == Kind::Data {
        // the web client writes it on its data line too
        out.push("a=rtcp-mux".to_owned());
    }
    for kept in [["ssrc", "ssrc-group"].as_slice(), &["rtcp-mux", "rtcp-rsize"]] {
        out.extend(
            section
                .lines
                .iter()
                .filter(|line| attribute_name(line).is_some_and(|name| kept.contains(&name)))
                // reduced-size RTCP only on video lines, as the web client writes them (libwebrtc names it on audio too)
                .filter(|line| section.kind == Kind::Video || attribute_name(line) != Some("rtcp-rsize"))
                .cloned(),
        );
        if kept.contains(&"ssrc") && section.kind != Kind::Data && !section.has("ssrc") {
            // a line naming no stream names the stream `1`, as the web client writes its receive-only lines (with its
            // `a=x-ssrc-range:1-1`)
            out.push(format!("a=ssrc:1 cname:{SYNTHESIZED_CNAME}"));
        }
    }
    if section.kind == Kind::Video && !section.has("rtcp-rsize") {
        // reduced-size RTCP on every video line, as the web client's (Chrome's) are: the mixer refuses a group call's
        // answer whose receiving lines don't name it (`410 SdpParsingFailure`, M3 Task 19)
        out.push("a=rtcp-rsize".to_owned());
    }
    out.push(format!("a=label:{}", name.label));
    if section.kind == Kind::Data {
        out.push(format!(
            "a=sctp-port:{}",
            section.value("sctp-port").unwrap_or(DEFAULT_SCTP_PORT)
        ));
        if let Some(size) = section.value("max-message-size") {
            out.push(format!("a=max-message-size:{size}"));
        }
    }
    Ok(())
}

/// The payload lines of an audio or video section, from Teams' feedback attribute to the extensions.
fn write_rtp_payload(section: &Section, port: &str, role: Role, out: &mut Vec<String>) {
    out.push(match section.kind {
        Kind::Audio => "a=x-signaling-fb:* x-message app recv:dsh".to_owned(),
        Kind::Video | Kind::Data => "a=x-signaling-fb:* x-message app send:src recv:src,vc".to_owned(),
    });
    // a line naming no stream names the range `1-1`, as the web client writes its receive-only lines (the mixer
    // refuses an answer whose line names no range: `410 SdpParsingFailure`)
    match section.values("ssrc").next().and_then(|ssrc| ssrc.split(' ').next()) {
        Some(ssrc) => out.push(format!("a=x-ssrc-range:{ssrc}-{ssrc}")),
        None => out.push("a=x-ssrc-range:1-1".to_owned()),
    }
    out.extend(section.values("rtpmap").map(|value| format!("a=rtpmap:{value}")));
    out.extend(
        section
            .values("fmtp")
            .map(|value| format!("a=fmtp:{}", with_receive_limits(section, value))),
    );
    if role == Role::Offer {
        out.push(format!("a=rtcp:{port}"));
    }
    out.extend(rtcp_feedback(section));
    out.extend(
        section
            .values("extmap")
            .filter(|value| role == Role::Answer || !names_a_line(value))
            .map(|value| format!("a=extmap:{}", backslashed(value))),
    );
}

/// The largest picture we take on a video line, as the web client names its own on H.264's format (`max-fs` in 16 by 16
/// macroblocks, `max-mbps` macroblocks a second, `max-fps` frames a second times 100): 1280 by 720 at 30 frames a
/// second, what the native client decodes. The mixer refuses a group call's answer whose receiving lines name none:
/// `410 SdpParsingFailure`, M3 Task 19.
const H264_RECEIVE_LIMITS: &str = "max-fs=3600;max-mbps=108000;max-fps=3000";

/// An `a=fmtp` value of `section` with [`H264_RECEIVE_LIMITS`] added when it is H.264's on a video line that names none.
fn with_receive_limits(section: &Section, value: &str) -> String {
    let Some((pt, params)) = value.split_once(' ') else {
        return value.to_owned();
    };
    let h264 = section.values("rtpmap").any(|map| {
        map.split_once(' ')
            .is_some_and(|(mapped, codec)| mapped == pt && codec.to_ascii_lowercase().starts_with("h264/"))
    });
    if section.kind != Kind::Video || !h264 || params.contains("max-fs=") {
        return value.to_owned();
    }
    format!("{pt} {params};{H264_RECEIVE_LIMITS}")
}

/// A section's `a=rtcp-fb` lines as Teams writes them: a video line's once each for every payload type (`a=rtcp-fb:*
/// nack`), as the web client, the mixer and the media controller write theirs (the mixer refuses a group call's answer
/// whose video lines name their feedback per payload type: `410 SdpParsingFailure`, M3 Task 19); an audio line's as
/// they are (the web client writes Opus's per payload type).
fn rtcp_feedback(section: &Section) -> Vec<String> {
    if section.kind != Kind::Video {
        return section
            .values("rtcp-fb")
            .map(|value| format!("a=rtcp-fb:{value}"))
            .collect();
    }
    let mut feedback: Vec<&str> = Vec::new();
    for value in section.values("rtcp-fb") {
        let kind = value.split_once(' ').map_or(value, |(_, kind)| kind);
        if !feedback.contains(&kind) {
            feedback.push(kind);
        }
    }
    feedback.into_iter().map(|kind| format!("a=rtcp-fb:* {kind}")).collect()
}

/// The header extensions that carry a line's mid or a stream's id in every packet. Teams' dialect may name our lines
/// otherwise than libwebrtc does ([`NgcSession`]): offered, the far end would take our packets for a line it doesn't
/// know and drop them (seen live, M3 Task 16: a web client answering our offer received nothing), so our offers name
/// none of them; without them in the answer libwebrtc sends none. An answer of ours keeps them: it answers the offer's
/// own mids.
const LINE_NAMING_EXTENSIONS: [&str; 3] = [
    "urn:ietf:params:rtp-hdrext:sdes:mid",
    "urn:ietf:params:rtp-hdrext:sdes:rtp-stream-id",
    "urn:ietf:params:rtp-hdrext:sdes:repaired-rtp-stream-id",
];

/// Whether an `a=extmap` value names one of [`LINE_NAMING_EXTENSIONS`].
fn names_a_line(value: &str) -> bool {
    value
        .split_whitespace()
        .nth(1)
        .is_some_and(|uri| LINE_NAMING_EXTENSIONS.contains(&uri))
}

/// The payload lines of a data section, as the web client writes them (an SSRC of its own, drawn at random).
fn write_data_payload(out: &mut Vec<String>) -> Result<(), SdpError> {
    let mut bytes = [0; 4];
    getrandom::fill(&mut bytes).map_err(|error| SdpError::Random(error.to_string()))?;
    let ssrc = (u32::from_be_bytes(bytes) & 0x7fff_ffff).max(1);
    out.extend([
        "a=x-data-protocol:sctp".to_owned(),
        format!("a=x-ssrc-range:{ssrc}-{ssrc}"),
        "a=rtpmap:127 x-data/90000".to_owned(),
        "a=rtpmap:126 rtx/90000".to_owned(),
        "a=fmtp:126 apt=127".to_owned(),
    ]);
    Ok(())
}

/// The transport lines: setup, mid, stream, direction, ICE and DTLS, the candidates.
fn write_transport(section: &Section, name: &Line, candidates: Vec<String>, out: &mut Vec<String>) {
    if let Some(setup) = section.value("setup") {
        out.push(format!("a=setup:{setup}"));
    }
    out.push(format!("a=mid:{}", name.remote));
    // a line naming no stream of ours names no media stream either, as the web client writes its receive-only lines
    // (the mixer refuses a group call's answer whose receive-only lines name one: `410 SdpParsingFailure`, M3 Task 19)
    if let Some(msid) = section.value("msid").filter(|_| section.has("ssrc")) {
        out.push(format!("a=msid:{msid}"));
    }
    out.push(format!("a={}", section.direction().unwrap_or("sendrecv")));
    for name in ["ice-ufrag", "ice-pwd", "fingerprint"] {
        if let Some(value) = section.value(name) {
            out.push(format!("a={name}:{value}"));
        }
    }
    out.extend(candidates);
    if let Some(options) = section.value("ice-options") {
        out.push(format!("a=ice-options:{options}"));
    }
}

/// An `a=extmap` value with the URI written as Teams writes it.
fn backslashed(value: &str) -> String {
    match value.split_once(' ') {
        Some((id, uri)) if BACKSLASHED.contains(&uri) => format!("{id} {}", uri.replace('/', "\\")),
        _ => value.to_owned(),
    }
}

// ---- Teams' dialect → standard ----

/// Writes the `kept` sections of `parsed` (Teams') as standard SDP, each named by its libwebrtc mid; `local` maps a
/// Teams mid of the bundle to ours.
fn read(parsed: &Parsed, kept: &[(&Section, String)], role: Role, local: impl Fn(&str) -> Option<String>) -> String {
    let mut out = read_session(parsed, &local);
    let first = parsed
        .bundle()
        .first()
        .and_then(|mid| parsed.media.iter().find(|section| section.mid() == Some(mid)))
        .or_else(|| parsed.media.iter().find(|section| !section.rejected()));
    for (section, mid) in kept {
        if section.rejected() {
            read_rejected(section, mid, &mut out);
        } else {
            read_section(parsed, section, first, mid, role, &mut out);
        }
    }
    let mut text = out.join("\r\n");
    text.push_str("\r\n");
    text
}

fn read_session(parsed: &Parsed, local: &impl Fn(&str) -> Option<String>) -> Vec<String> {
    let mut out = vec!["v=0".to_owned()];
    out.extend(parsed.session.iter().find(|line| line.starts_with("o=")).cloned());
    out.extend(["s=-".to_owned(), "t=0 0".to_owned()]);
    let bundled: Vec<String> = parsed.bundle().into_iter().filter_map(local).collect();
    if !bundled.is_empty() {
        out.push(format!("a=group:BUNDLE {}", bundled.join(" ")));
    }
    if parsed.session.iter().any(|line| line == "a=extmap-allow-mixed") {
        out.push("a=extmap-allow-mixed".to_owned());
    }
    out
}

fn read_rejected(section: &Section, mid: &str, out: &mut Vec<String>) {
    out.push(match section.kind {
        Kind::Data => "m=application 0 UDP/DTLS/SCTP webrtc-datachannel".to_owned(),
        // payload type 0 needs no rtpmap, as the web client writes a rejected line
        Kind::Audio | Kind::Video => format!("m={} 0 UDP/TLS/RTP/SAVPF 0", section.kind.standard()),
    });
    out.push("c=IN IP4 0.0.0.0".to_owned());
    out.push(format!("a=mid:{mid}"));
    out.push("a=inactive".to_owned());
}

fn read_section(
    parsed: &Parsed,
    section: &Section,
    first: Option<&Section>,
    mid: &str,
    role: Role,
    out: &mut Vec<String>,
) {
    out.push(match section.kind {
        Kind::Data => format!("m=application {} UDP/DTLS/SCTP webrtc-datachannel", section.port),
        Kind::Audio | Kind::Video => format!(
            "m={} {} UDP/TLS/RTP/SAVPF {}",
            section.kind.standard(),
            section.port,
            section.formats.join(" ")
        ),
    });
    let connection = section
        .connection()
        .or_else(|| first.and_then(Section::connection))
        .or_else(|| {
            parsed
                .session
                .iter()
                .find(|line| line.starts_with("c="))
                .map(String::as_str)
        })
        .unwrap_or("c=IN IP4 0.0.0.0");
    out.push(connection.to_owned());
    out.extend(section.lines.iter().filter(|line| line.starts_with("b=AS:")).cloned());
    for name in INHERITED {
        let value = section
            .value(name)
            .or_else(|| first.and_then(|first| first.value(name)));
        match (name, value, role) {
            // an offer naming no DTLS role (the mixer's) leaves it to us, as `actpass` does: libwebrtc then takes the
            // client's part, as the web client does, and in a renegotiation keeps the role the call has
            ("setup", None, Role::Offer) => out.push("a=setup:actpass".to_owned()),
            (_, Some(value), _) => out.push(format!("a={name}:{value}")),
            (_, None, _) => {}
        }
    }
    out.push(format!("a=mid:{mid}"));
    out.extend(section.values("candidate").filter_map(standard_candidate));
    let kept: &[&str] = if section.kind == Kind::Data {
        &KEPT_DATA
    } else {
        &KEPT_RTP
    };
    for line in &section.lines {
        let Some(name) = attribute_name(line) else { continue };
        if !kept.contains(&name) || INHERITED.contains(&name) || ["mid", "candidate"].contains(&name) {
            continue;
        }
        if name == "ssrc" && placeholder_stream(line) {
            continue;
        }
        out.extend(standard_attribute(section, name, line));
    }
    if section.kind != Kind::Data && section.direction().is_none() {
        out.push("a=sendrecv".to_owned());
    }
    if section.kind == Kind::Data && !section.has("sctp-port") {
        out.push(format!("a=sctp-port:{DEFAULT_SCTP_PORT}"));
    }
    if section.kind != Kind::Data && !section.has("ssrc") {
        out.extend(synthesized_ssrcs(section));
    }
}

/// One kept attribute line in its standard form (none, one, or one per payload type).
fn standard_attribute(section: &Section, name: &str, line: &str) -> Vec<String> {
    match name {
        "rtcp-fb" => {
            let Some((pt, feedback)) = attribute(line, "rtcp-fb").and_then(|value| value.split_once(' ')) else {
                return Vec::new();
            };
            if feedback.starts_with("x-message") {
                Vec::new()
            } else if pt == "*" {
                section
                    .formats
                    .iter()
                    .map(|pt| format!("a=rtcp-fb:{pt} {feedback}"))
                    .collect()
            } else {
                vec![line.to_owned()]
            }
        }
        "extmap" => vec![line.replace('\\', "/")],
        _ => vec![line.to_owned()],
    }
}

/// A Teams candidate in its standard form: component 1 over UDP only (the native client's media goes through its
/// UDP relay; a TCP candidate is out of its reach), Teams' own extensions left out.
fn standard_candidate(candidate: &str) -> Option<String> {
    let fields: Vec<&str> = candidate.split(' ').collect();
    let [
        foundation,
        "1",
        transport,
        priority,
        address,
        port,
        "typ",
        kind,
        ref rest @ ..,
    ] = fields[..]
    else {
        return None;
    };
    if !transport.eq_ignore_ascii_case("udp") {
        return None;
    }
    let related = match rest {
        ["raddr", raddr, "rport", rport, ..] => format!(" raddr {raddr} rport {rport}"),
        _ => String::new(),
    };
    Some(format!(
        "a=candidate:{foundation} 1 udp {priority} {address} {port} typ {kind}{related}"
    ))
}

/// The stream a receive-only line names (`a=ssrc:1`, `a=x-ssrc-range:1-1`, as the web client writes such a line):
/// no stream at all, so it is left out of the standard side (a peer would take the same SSRC on several lines for one
/// stream and refuse the description).
const PLACEHOLDER_SSRC: u32 = 1;

/// Whether an `a=ssrc` line names [`PLACEHOLDER_SSRC`].
fn placeholder_stream(line: &str) -> bool {
    attribute(line, "ssrc")
        .and_then(|value| value.split(' ').next())
        .and_then(|ssrc| ssrc.parse::<u32>().ok())
        == Some(PLACEHOLDER_SSRC)
}

/// `a=ssrc` lines for a line that names its streams only by Teams' `a=ssrc-group` or `a=x-ssrc-range`.
fn synthesized_ssrcs(section: &Section) -> Vec<String> {
    let grouped: Vec<&str> = section
        .value("ssrc-group")
        .and_then(|group| group.strip_prefix("FID "))
        .map(|ssrcs| ssrcs.split(' ').collect())
        .unwrap_or_default();
    let ssrcs = if grouped.is_empty() {
        section
            .value("x-ssrc-range")
            .and_then(|range| range.split('-').next())
            .into_iter()
            .collect()
    } else {
        grouped
    };
    ssrcs
        .into_iter()
        .filter(|ssrc| ssrc.parse::<u32>().is_ok_and(|ssrc| ssrc != PLACEHOLDER_SSRC))
        .map(|ssrc| format!("a=ssrc:{ssrc} cname:{SYNTHESIZED_CNAME}"))
        .collect()
}
