//! A call's media in the engine: set up before the account's start, join or answer goes ([`prepare`]: the relay's
//! credentials read, the session started without its devices (libwebrtc allocating on the relay), our offer or our answer
//! written in Teams' dialect), its devices opened once that start, join or answer was admitted and taken
//! ([`open_devices`]), the far end's SDP applied and renegotiations answered or offered while it runs ([`apply`],
//! [`answer`], [`reoffer`]), the camera turned on or off with the renegotiation it takes ([`camera`]; offered again
//! once a renegotiation of ours gave way to the far end's, [`offer_camera`]), and what the session reports turned into
//! what the call does ([`LiveMedia::poll`]: a camera whose device went away is off).
//!
//! **Video** (M3 Tasks 18, 19): a call's media takes video when the engine has video for its calls ([`CallVideo`]): the
//! session's video starts with it (received frames to the account's sinks by video line, [`VideoOutput`]). A 1:1 call's
//! dialect takes the main video line of what the far end offers, receiving until the camera is on (its camera turned on
//! by a renegotiation). A group call's (placed, joined or answered) takes the main video line, sent on once the camera
//! is on, and up to [`GROUP_SLOTS`] receiving lines, each carrying the participant a source request chose: its offer
//! is the web client's group offer's kind (audio, the main line, the receiving lines, the sharing line, the data
//! channel), its start, join or answer describes those lines (`mediaDescriptions`), and its control plane (the data
//! channel's) is started once the far end's SDP was applied (`teams_media::mcp`). The camera is off when a call starts;
//! only the account's action turns it on. The test call's media is a 1:1 call's (the media controller answers it, its
//! camera turned by a renegotiation); a call whose video can't start carries audio only.
//! **A screen shared, 1:1** (`docs/protocol/calling.md`, "More of a running call"): the account's screen, picked at the
//! desktop's portal ([`pick_screen`]), goes on a sharing line of its own its renegotiation adds ([`share`], stopped by
//! [`stop_share`]); the far end's goes on the sharing line its renegotiation adds, which the dialect answers receiving
//! and whose frames go to the video output's [`teams_media::session::SHARING_LINE`] sink.
//!
//! Every step that blocks (the allocation, opening a device, an SDP command, the session's close) runs on tokio's
//! blocking pool, never on the engine's thread; the session's own threads carry the media.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use teams_call::CallId;
use teams_media::devices::VideoSource;
use teams_media::devices::{Devices, VideoSink};
use teams_media::relay::{self, RelayConfig, RelayServer, teams_relay};
use teams_media::sdp::{NgcSdp, NgcSession, SHARING_LABEL, VideoLines};
use teams_media::session::{MediaSession, RemoteSinks, SessionConfig, SessionDevice, SessionEvent, VideoParts};
use teams_media::video::codec::EncoderSettings;
use teams_media::video::convert::MaxSize;
use teams_media::video::effects::BackgroundChoice;
use teams_proto::{
    ChannelParameters, Client, Cloud, Credentials, HttpTransport, MediaContent, MediaDescription, MediaDescriptions,
    RelayTokens,
};

use super::gallery::Gallery;

/// How long the media may take to connect once the far end's SDP was applied before the call fails.
pub const CONNECT_WITHIN: Duration = Duration::from_secs(30);
/// How long the media may stay lost (ICE disconnected) before the call fails.
pub const RECOVER_WITHIN: Duration = Duration::from_secs(15);
/// How long after the far end closed the media the call's own end may still come (a hang-up closes both): after it a
/// call that was up ends as the far end's (`MediaChange::Closed`), and one not up yet fails.
pub const CLOSED_GRACE: Duration = Duration::from_secs(2);
/// How many participants' video a group call receives at once (the mixer offers as many receiving lines; the M0
/// spike drew nine 720p tiles at 30 fps).
pub const GROUP_SLOTS: usize = 9;

/// Where a call's media is relayed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Relay {
    /// The relay of the account's own service, chosen by the cloud of the account's own transport at each call's
    /// setup. A Microsoft account's is Teams' relay host, as the web client names it: UDP 3478, TCP 443 and TLS 443
    /// ([`teams_relay`]); libwebrtc resolves it and allocates over what it reaches. An own-server account's is the
    /// servers its own server's credentials' answer names (`urls`, `docs/server.md`), none on a Microsoft host: the
    /// own server's credential goes only to its own relay (and to the alternate that relay names in a `300 Try
    /// Alternate`, which libwebrtc follows: the relay already holds the credential). With none, the call's media
    /// isn't set up.
    Teams,
    /// These servers, in order: a test's loopback TURN server. Tests only (`test-support`).
    #[cfg(feature = "test-support")]
    Fixed(Vec<RelayServer>),
}

impl Relay {
    /// The servers libwebrtc allocates on for an account of `cloud`, whose credentials' answer named `urls`.
    fn servers(&self, cloud: &Cloud, urls: &[String]) -> Result<Vec<RelayServer>, String> {
        match self {
            Self::Teams if cloud.is_microsoft() => Ok(teams_relay()),
            Self::Teams => own_relay(urls),
            #[cfg(feature = "test-support")]
            Self::Fixed(servers) => Ok(servers.clone()),
        }
    }
}

/// The domains of Microsoft's services. An own server's relay never names a host in one.
const MICROSOFT_DOMAINS: [&str; 15] = [
    "microsoft",
    "microsoft.com",
    "microsoftonline.com",
    "skype.com",
    "lync.com",
    "live.com",
    "office.com",
    "office.net",
    "windows.net",
    "azure.com",
    "azure.net",
    "azureedge.net",
    "cloudapp.net",
    "trafficmanager.net",
    "msedge.net",
];

/// `host` is in one of [`MICROSOFT_DOMAINS`].
fn microsoft_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    MICROSOFT_DOMAINS
        .iter()
        .any(|domain| host == *domain || host.strip_suffix(domain).is_some_and(|sub| sub.ends_with('.')))
}

/// An own server's relay: the TURN servers its `urls` name, leaving out any that isn't a TURN URL or names a
/// Microsoft host.
fn own_relay(urls: &[String]) -> Result<Vec<RelayServer>, String> {
    let servers: Vec<RelayServer> = urls
        .iter()
        .filter_map(|url| RelayServer::parse(url).ok())
        .filter(|server| !microsoft_host(&server.host))
        .collect();
    if servers.len() < urls.len() {
        log::warn!(
            "{} of the own server's {} relay URLs left out: not a TURN URL, or a Microsoft host",
            urls.len() - servers.len(),
            urls.len()
        );
    }
    if servers.is_empty() {
        return Err("the own server names no relay its credential may go to".to_owned());
    }
    Ok(servers)
}

/// What a call's media is made of: the devices it opens (the microphone and the speaker, only once the account's
/// start, join or answer was admitted and taken; the camera only on the account's action), the relay it goes
/// through, how the session runs, and its video (`None`: calls without video).
#[derive(Clone)]
pub struct CallMedia {
    pub devices: Arc<dyn Devices + Send + Sync>,
    pub relay: Relay,
    pub session: SessionConfig,
    pub video: Option<CallVideo>,
}

/// A call's video: how the camera is encoded, the largest picture sent, and where the frames are shown.
#[derive(Clone)]
pub struct CallVideo {
    pub encoder: EncoderSettings,
    pub max_size: MaxSize,
    pub output: Arc<dyn VideoOutput>,
}

impl std::fmt::Debug for CallVideo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallVideo")
            .field("encoder", &self.encoder)
            .field("max_size", &self.max_size)
            .finish_non_exhaustive()
    }
}

/// Where a call's video is shown (one call at a time): the far end's decoded frames, and our camera's as sent.
pub trait VideoOutput: Send + Sync {
    /// The sink of the frames that come on video line `line` (0 the main line, a 1:1 call's far end; 1, 2, … a group
    /// call's receiving lines, each one participant's video as [`crate::Event::CallParticipants`] tells), made the
    /// first time the line brings a frame.
    fn remote(&self, line: usize) -> Box<dyn VideoSink>;
    /// The sink of our camera's frames (a self view), or `None`.
    fn preview(&self) -> Option<Box<dyn VideoSink>>;

    /// Whether the calls may use the video codec now (the settings' video switch): a call set up while it
    /// is off starts no video.
    fn enabled(&self) -> bool {
        true
    }

    /// The background our camera's frames are sent with (the settings' video effects), as chosen at each frame;
    /// none by default.
    fn background(&self) -> BackgroundChoice {
        BackgroundChoice::default()
    }
}

impl std::fmt::Debug for CallMedia {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallMedia")
            .field("relay", &self.relay)
            .field("session", &self.session)
            .field("video", &self.video)
            .finish_non_exhaustive()
    }
}

/// A call's media set up: its session, its dialect, and our SDP (an offer, or our answer to the far end's) in Teams'
/// dialect.
pub(crate) struct Prepared {
    pub(super) session: MediaSession,
    pub(super) dialect: NgcSession,
    pub(super) sdp: NgcSdp,
    /// The account's tenant, read from its token for a test call whose engine didn't know it.
    pub(super) tenant: Option<String>,
    /// The call's media takes video (its session's video started).
    pub(super) video: bool,
    /// A group call's that takes video: its lines as its start, join or answer describes them.
    pub(super) group: Option<MediaDescriptions>,
    /// It is a group call's (placed, joined or answered), whether or not its video started.
    pub(super) group_call: bool,
}

/// Which video a call's media takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum VideoKind {
    /// A 1:1 call's: the main video line.
    OneToOne,
    /// A group call's: the main video line and the receiving lines, with the control plane.
    Group,
}

/// Whether a call's setup reads the account's tenant from its token first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TenantRead {
    /// Known already, or not wanted.
    No,
    /// Wanted, the setup going on without it: the organizer's Controls name it (the meeting options' URL).
    Wanted,
    /// Needed: a test call's `add` and a Meet now's entry name it; the setup is refused without one.
    Needed,
}

/// What a call's media is set up for: the far end's offer to answer (none for a start or a join), whether to read
/// the account's tenant first ([`TenantRead`]), and which video the call takes.
pub(super) struct Setup {
    pub(super) offer: Option<MediaContent>,
    pub(super) tenant: TenantRead,
    pub(super) video: VideoKind,
}

/// Sets up a call's media (see the module docs): our offer, or our answer to the far end's.
pub(super) async fn prepare<C: Credentials + 'static>(
    client: Arc<Client<HttpTransport<C>>>,
    media: CallMedia,
    setup: Setup,
) -> Result<Prepared, String> {
    let Setup { offer, tenant, video } = setup;
    let tenant = match tenant {
        TenantRead::Needed => Some(
            client
                .transport()
                .account_tenant()
                .await
                .ok_or("the account's tenant can't be read from its token")?,
        ),
        TenantRead::Wanted => client.transport().account_tenant().await,
        TenantRead::No => None,
    };
    let mut prepared = set_up_with(&client, media, offer, video).await?;
    prepared.tenant = tenant;
    Ok(prepared)
}

/// [`prepare`]'s media part.
async fn set_up_with<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    media: CallMedia,
    offer: Option<MediaContent>,
    video: VideoKind,
) -> Result<Prepared, String> {
    let answer = client
        .read(&RelayTokens)
        .await
        .map_err(|error| format!("the relay's credentials can't be read: {error}"))?;
    let tokens = RelayTokens::read_answer(answer.body()).map_err(|error| error.to_string())?;
    let credential = tokens
        .credentials
        .into_iter()
        .next()
        .ok_or("the relay handed out no credential")?;
    let relay = RelayConfig {
        servers: media.relay.servers(client.transport().cloud(), &tokens.urls)?,
        credentials: relay::Credentials {
            username: credential.username,
            password: credential.password.expose().to_owned(),
        },
    };
    tokio::task::spawn_blocking(move || set_up(&media, &relay, offer.as_ref(), video))
        .await
        .map_err(|error| format!("the media's setup stopped: {error}"))?
}

/// The blocking part of [`prepare`].
fn set_up(
    media: &CallMedia,
    relay: &RelayConfig,
    offer: Option<&MediaContent>,
    kind: VideoKind,
) -> Result<Prepared, String> {
    let text = |error: &dyn std::fmt::Display| error.to_string();
    // no device opens before the call's start, join or answer was admitted (`open_devices`)
    let session = MediaSession::start_without_devices(relay, media.session).map_err(|error| text(&error))?;
    let mut dialect = NgcSession::new();
    let video = start_video(&session, media.video.as_ref(), kind);
    let group = video && kind == VideoKind::Group;
    if group {
        // the main line, and the receiving lines each a participant's video
        dialect.set_video(VideoLines::Group { slots: GROUP_SLOTS });
    } else if video {
        // the far end's main video line is taken, receiving until the camera is on
        dialect.set_video(VideoLines::Main { sending: false });
    }
    let sdp = match offer {
        None if group => {
            let ours = session.offer_group(GROUP_SLOTS).map_err(|error| text(&error))?;
            dialect.write_offer(&ours).map_err(|error| text(&error))?
        }
        None => {
            let ours = session.offer_audio().map_err(|error| text(&error))?;
            dialect.write_offer(&ours).map_err(|error| text(&error))?
        }
        Some(offer) => {
            let theirs = ngc(offer)?;
            let theirs = dialect.read_offer(&theirs).map_err(|error| text(&error))?;
            session.set_sharing_lines(dialect.sharing_mids());
            // a group call's sharing line answered sending too: the account's screen goes out on it
            session.set_group_sharing(dialect.sharing_slot().map(|(mid, _)| mid));
            let answer = session.accept_offer(theirs).map_err(|error| text(&error))?;
            if group {
                session
                    .start_control(dialect.data_stream_id())
                    .map_err(|error| text(&error))?;
            }
            dialect.write_answer(&answer).map_err(|error| text(&error))?
        }
    };
    let group = group.then(|| descriptions(&dialect, false, None));
    Ok(Prepared {
        session,
        dialect,
        sdp,
        tenant: None,
        video,
        group,
        group_call: kind == VideoKind::Group,
    })
}

impl LiveMedia {
    /// Whether the account's screen is shared once the share switches it made are done: a switch that waits, else a
    /// share step that began, else the share as it is now.
    pub(super) fn share_goes(&self) -> bool {
        match &self.share_waits {
            Some(ShareWait::Start(_)) => true,
            Some(ShareWait::Stop) => false,
            None => self.shares.turning.unwrap_or(self.shares.own),
        }
    }

    /// It is a group call's that takes video (its receiving lines assigned, its camera turned by describing its
    /// lines).
    pub(super) fn is_group(&self) -> bool {
        self.gallery.has_lines()
    }

    /// A group call's video lines described anew, numbered on: the main line as its camera goes, each receiving line
    /// receiving, and the sharing line sending while the account shares its screen.
    pub(super) fn describe(&mut self) -> MediaDescriptions {
        let sharing = self.shares.own.then_some(true);
        self.describe_with(sharing)
    }

    /// [`LiveMedia::describe`] as a share starts (`on`) or stops: the sharing line `sendonly` or `recvonly` again.
    pub(super) fn describe_share(&mut self, on: bool) -> MediaDescriptions {
        self.describe_with(Some(on))
    }

    fn describe_with(&mut self, sharing: Option<bool>) -> MediaDescriptions {
        self.described += 1;
        let mut described = descriptions(&lock(&self.dialect), self.camera == Some(true), sharing);
        described.request_id = Some(self.described);
        described
    }

    /// Our answer to a renegotiation as its `mediaContent`: a group call's with its lines described anew and the
    /// bandwidth seeded, as the web client answers the mixer's renegotiations (without them the mixer stops sending
    /// the receiving lines' video, seen live, M3 Task 19).
    pub(super) fn answer_content(&mut self, sdp: &NgcSdp) -> MediaContent {
        let mut answer = content(sdp);
        if self.is_group() {
            answer.descriptions = Some(self.describe());
            answer.channel_parameters = Some(bandwidth_seed());
        }
        answer
    }
}

/// A group call's video lines as its requests describe them: the main line as the camera goes (`sending`), each
/// receiving line receiving; the sharing line, when `sharing` names it, `sendonly` and labelled while the account's
/// screen goes out on it (`true`) and `recvonly` once it stops (`false`), as the web client's share describes it.
pub(super) fn descriptions(dialect: &NgcSession, sending: bool, sharing: Option<bool>) -> MediaDescriptions {
    let lines = dialect
        .group_descriptions(sending)
        .into_iter()
        .map(|(mid, direction)| MediaDescription {
            mid,
            direction: direction.to_owned(),
            label: None,
        });
    let screen = sharing
        .zip(dialect.group_sharing_mid())
        .map(|(on, mid)| MediaDescription {
            mid: mid.to_owned(),
            direction: if on { "sendonly" } else { "recvonly" }.to_owned(),
            label: on.then(|| SHARING_LABEL.to_owned()),
        });
    MediaDescriptions {
        descriptions: lines.chain(screen).collect(),
        request_id: Some(1),
    }
}

/// Starts the session's video when the engine has video for its calls; whether it started (a codec that refuses its
/// setup leaves the call without video, its audio as it is).
fn start_video(session: &MediaSession, video: Option<&CallVideo>, kind: VideoKind) -> bool {
    let Some(video) = video.filter(|video| video.output.enabled()) else {
        return false;
    };
    let output = Arc::clone(&video.output);
    let remote: RemoteSinks = Box::new(move |line| output.remote(line));
    let (mut encoder, mut max_size) = (video.encoder, video.max_size);
    if kind == VideoKind::Group {
        // one stream for every receiver of the mixer (no simulcast): the size and rate of the web client's middle
        // layer, which the mixer forwards to a receiver whose downlink takes it
        encoder.max_bitrate = encoder.max_bitrate.min(GROUP_MAX_BITRATE);
        encoder.start_bitrate = encoder.start_bitrate.min(encoder.max_bitrate);
        max_size = MaxSize {
            width: max_size.width.min(GROUP_MAX_SIZE.width),
            height: max_size.height.min(GROUP_MAX_SIZE.height),
        };
    }
    let parts = VideoParts {
        encoder,
        max_size,
        remote,
        preview: video.output.preview(),
        background: video.output.background(),
    };
    match session.start_video(parts) {
        Ok(()) => true,
        Err(error) => {
            log::warn!("the call has no video: {error}");
            false
        }
    }
}

/// A `mediaContent` of Teams' dialect, read.
fn ngc(content: &MediaContent) -> Result<NgcSdp, String> {
    NgcSdp::new(&content.content_type, &content.blob).map_err(|error| error.to_string())
}

/// Our SDP as a call write's `mediaContent`.
pub(super) fn content(sdp: &NgcSdp) -> MediaContent {
    MediaContent::new(sdp.as_str(), sdp.content_type())
}

/// Our SDP as a start's, a join's or an `accept`'s `mediaContent`: a group call's with its lines described, as the
/// web client's.
pub(super) fn first_content(prepared: &Prepared) -> MediaContent {
    let mut content = content(&prepared.sdp);
    content.descriptions.clone_from(&prepared.group);
    if prepared.group.is_some() {
        content.channel_parameters = Some(bandwidth_seed());
    }
    content
}

/// The bandwidth the mixer may start sending at, as the web client seeds it (`sendSideBWSeed` on every line).
fn bandwidth_seed() -> ChannelParameters {
    ChannelParameters {
        mids: vec!["*".to_owned()],
        media_parameter: serde_json::json!({"sendSideBWSeed": {"seedValueBitsPerSec": BANDWIDTH_SEED}}).to_string(),
    }
}

/// The bandwidth a group call's mixer is told it may start sending at, in bits a second (the web client seeds its
/// measured downlink, a few hundred kb/s in the captures; the session's `b=CT` is 4000 kb/s). Without it the mixer
/// forwards no video at all (seen live, M3 Task 19).
const BANDWIDTH_SEED: u64 = 4_000_000;
/// The largest picture and the highest bitrate a group call sends.
const GROUP_MAX_SIZE: MaxSize = MaxSize {
    width: 640,
    height: 360,
};
const GROUP_MAX_BITRATE: u32 = 600_000;

/// What a media step of a running call came to.
pub(crate) enum MediaDone {
    /// The far end's answer applied (or not).
    Applied { call: CallId, result: Result<(), String> },
    /// Our answer to the far end's renegotiation (or why there is none).
    Answered {
        call: CallId,
        result: Result<NgcSdp, String>,
    },
    /// The camera turned on or off, with our renegotiation's offer (none when our video line already went that way),
    /// or why it didn't.
    Camera {
        call: CallId,
        on: bool,
        result: Result<Option<NgcSdp>, String>,
        reply: super::Waiting,
    },
    /// A group call's camera turned on or off (or why not): its lines are described again next.
    GroupCamera {
        call: CallId,
        on: bool,
        result: Result<(), String>,
        reply: super::Waiting,
    },
    /// Our renegotiation's offer (or why there is none).
    Reoffered {
        call: CallId,
        result: Result<NgcSdp, String>,
        reply: super::Waiting,
    },
    /// The call's devices opened (or not).
    Opened {
        call: CallId,
        result: Result<(), String>,
        unmute: Option<(u64, super::Waiting)>,
    },
    /// A missing speaker was retried without changing capture or mute state.
    SpeakerOpened { call: CallId },
    /// The screen the user picked to share (or why there is none).
    ScreenPicked {
        call: CallId,
        result: Result<Box<dyn VideoSource>, String>,
        reply: super::Waiting,
    },
    /// The screen the account shared on the session that the call's media replaced, given back by it (none once its
    /// stream ended): it goes on in the new session.
    ScreenKept {
        call: CallId,
        screen: Option<Box<dyn VideoSource>>,
    },
    /// The account's screen shared (`on`) or no longer on a group call's sharing line (or why not): its lines are
    /// described again next.
    GroupShared {
        call: CallId,
        on: bool,
        result: Result<(), String>,
        reply: super::Waiting,
    },
    /// The account's screen shared (`on`) or no longer, with our renegotiation's offer adding or rejecting its line
    /// (none when nothing was shared), or why not.
    Shared {
        call: CallId,
        on: bool,
        result: Result<Option<NgcSdp>, String>,
        reply: super::Waiting,
    },
}

#[derive(Clone, Copy)]
pub(super) enum SpeakerRetry {
    Waiting(Instant),
    Opening,
}

/// A call's running media.
pub(super) struct LiveMedia {
    pub(super) call: CallId,
    pub(super) session: Arc<MediaSession>,
    pub(super) dialect: Arc<Mutex<NgcSession>>,
    /// When the far end's SDP was applied: the media connects within [`CONNECT_WITHIN`] of it.
    applied: Option<Instant>,
    /// The session reports the media connected now.
    pub(super) connected: bool,
    /// Since when the media is lost (ICE disconnected).
    lost: Option<Instant>,
    /// When the far end closed the media.
    closed: Option<Instant>,
    /// Its devices were asked to open ([`open_devices`]).
    pub(super) devices_opened: bool,
    /// The screen the old session shared is on its way to this one ([`MediaDone::ScreenKept`]).
    pub(super) screen_coming: bool,
    pub(super) speaker_retry: Option<SpeakerRetry>,
    /// Its camera, as the account last turned it, when it takes video (its video started); `None`: no
    /// video.
    pub(super) camera: Option<bool>,
    /// What our camera's line owes once the open renegotiation settled; while one is owed the far end's
    /// renegotiation is answered with that line only receiving.
    pub(super) reoffer: Reoffer,
    /// Who shares a screen in the call now.
    pub(super) shares: Shares,
    /// A 1:1 call's share switch the account made while a renegotiation was open: it goes once that one settled.
    pub(super) share_waits: Option<ShareWait>,
    /// Who is in the call, whose video each receiving line shows, who speaks; a group call's that takes video
    /// ([`LiveMedia::is_group`]) has its camera go by describing its lines again, its receiving lines carrying the
    /// participants the source requests chose.
    pub(super) gallery: Gallery,
    /// How many times a group call's send capabilities went (each one's number).
    pub(super) capabilities: u64,
    /// The number of a group call's latest media descriptions (its start's, join's or `accept`'s is `1`).
    pub(super) described: u64,
}

/// What our camera's line owes once the open renegotiation settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reoffer {
    Nothing,
    /// A renegotiation of ours gave way to the far end's: our camera's line is offered again as the camera goes
    /// ([`offer_camera`]).
    Line,
    /// The account turned the camera while a renegotiation was open: `camera` says how it goes, and the camera turns
    /// that way with its line offered ([`camera`]).
    Camera,
}

/// Who shares a screen in a call: the account (`own`), the far end (`theirs`: a 1:1 call's far end's sharing line in
/// use, our answer receiving on it; in a group call, someone the roster says shares).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Shares {
    pub(super) own: bool,
    pub(super) theirs: bool,
    /// A share step of the account's that began and is not done yet: whether it shares. A 1:1 call's, a group call's
    /// Stop sharing, or a group session's screen that waited for the media to be set up again
    /// (`Calling::share_after_rebuild`); a Stop sharing while that screen starts sets it to stop.
    pub(super) turning: Option<bool>,
    /// How the share was last told ([`crate::Event::CallSharing`]): as it goes once the switches made are done.
    pub(super) told: bool,
}

/// A 1:1 call's share switch that waits for the open renegotiation to settle.
pub(super) enum ShareWait {
    /// Share this screen, which the user picked.
    Start(Box<dyn VideoSource>),
    /// Stop sharing.
    Stop,
}

/// What the session's reports since the last look mean for the call.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum MediaChange {
    Connected,
    Lost,
    /// The media failed, for good: the call fails.
    Failed(String),
    /// The far end closed the media and the call's own end didn't come within [`CLOSED_GRACE`]: the far end left.
    Closed,
    /// The camera went off by itself (its device went away): nothing more is sent on our video line until the account
    /// turns it on again.
    CameraOff,
    /// The microphone (`true`) or the speaker (`false`) went away or couldn't open (its stream ended, not moved),
    /// while the call goes on: the microphone stays muted until an unmute opens it again; the speaker is reopened
    /// about once a second, and playback resumes once it opens.
    AudioClosed {
        microphone: bool,
    },
    /// Who spoke last in a group call: the dominant speakers' audio source ids, as its mixer told them.
    Speakers(Vec<i64>),
    /// The screen the account shares ended at the desktop (the user stopped it there): nothing more goes on its line.
    ScreenEnded,
    Captions(Vec<teams_media::captions::Caption>),
}

impl LiveMedia {
    pub(super) fn new(call: CallId, prepared: Prepared) -> Self {
        Self {
            camera: prepared.video.then_some(false),
            reoffer: Reoffer::Nothing,
            shares: Shares::default(),
            share_waits: None,
            gallery: Gallery::new(prepared.group_call, prepared.group.is_some()),
            capabilities: 0,
            described: 1,
            call,
            session: Arc::new(prepared.session),
            dialect: Arc::new(Mutex::new(prepared.dialect)),
            applied: None,
            connected: false,
            lost: None,
            closed: None,
            devices_opened: false,
            screen_coming: false,
            speaker_retry: None,
        }
    }

    /// The far end's SDP was applied at `now`: the connect deadline runs from here (it already does for an answer we
    /// made, which applied theirs).
    pub(super) fn applied(&mut self, now: Instant) {
        self.applied.get_or_insert(now);
    }

    /// What the session reported since the last look, and the deadlines at `now`.
    pub(super) fn poll(&mut self, now: Instant) -> Vec<MediaChange> {
        let mut changes = Vec::new();
        while let Some(event) = self.session.next_event(Duration::ZERO) {
            match event {
                SessionEvent::Captions(lines) => changes.push(MediaChange::Captions(lines)),
                SessionEvent::Connected => {
                    self.connected = true;
                    self.lost = None;
                    changes.push(MediaChange::Connected);
                }
                SessionEvent::Disconnected => {
                    self.connected = false;
                    self.lost.get_or_insert(now);
                    changes.push(MediaChange::Lost);
                }
                SessionEvent::Closed => {
                    self.connected = false;
                    self.closed.get_or_insert(now);
                }
                SessionEvent::Failed(why) => changes.push(MediaChange::Failed(why)),
                SessionEvent::DeviceClosed(device) => {
                    log::warn!("call {}: the {device:?} went away", self.call.get());
                    changes.extend(self.device_closed(device));
                }
                SessionEvent::ControlActive { send, receive } => {
                    log::info!(
                        "call {}: its control plane is up (the mixer sends {send:?}, takes {receive:?})",
                        self.call.get()
                    );
                }
                SessionEvent::SourceRequested { sequence, request } => {
                    let what = if request.screen { "screen" } else { "line" };
                    log::info!(
                        "call {}: source request {sequence}: {what} {} to source {}",
                        self.call.get(),
                        request.stream,
                        request.source
                    );
                }
                SessionEvent::SourceResult { sequence, result } => {
                    log::info!("call {}: source request {sequence}: {result}", self.call.get());
                }
                SessionEvent::ControlTimedOut => {
                    log::warn!("call {}: its mixer didn't answer the control plane", self.call.get());
                }
                SessionEvent::DominantSpeakers(speakers) => changes.push(MediaChange::Speakers(speakers)),
            }
        }
        match self.overdue(now) {
            Some(CLOSED) => changes.push(MediaChange::Closed),
            Some(why) => changes.push(MediaChange::Failed(why.to_owned())),
            None => {}
        }
        changes
    }

    /// A device of the session went away: the camera as [`Self::camera_stopped`], the microphone or the speaker told
    /// while it is still missing (the speaker is reopened about once a second, the microphone on an unmute).
    fn device_closed(&mut self, device: SessionDevice) -> Option<MediaChange> {
        match device {
            SessionDevice::Camera => self.camera_stopped(),
            // A retry may already have reopened capture before an older stream's closure is polled.
            SessionDevice::Microphone => self
                .session
                .microphone_missing()
                .then_some(MediaChange::AudioClosed { microphone: true }),
            SessionDevice::Speaker => self
                .session
                .speaker_missing()
                .then_some(MediaChange::AudioClosed { microphone: false }),
            SessionDevice::Screen => self.shares.own.then_some(MediaChange::ScreenEnded),
        }
    }

    /// The camera stopped by itself: off, told once while it was on.
    fn camera_stopped(&mut self) -> Option<MediaChange> {
        (self.camera == Some(true)).then(|| {
            self.camera = Some(false);
            MediaChange::CameraOff
        })
    }

    /// A deadline that passed at `now`, if one did.
    fn overdue(&self, now: Instant) -> Option<&'static str> {
        Deadlines {
            applied: self.applied,
            connected: self.connected,
            lost: self.lost,
            closed: self.closed,
        }
        .overdue(now)
    }
}

/// Why the media ended when the far end closed it ([`Deadlines::overdue`]).
const CLOSED: &str = "the far end closed the media";

/// What a call's media deadlines run from.
#[derive(Clone, Copy, Debug, Default)]
struct Deadlines {
    applied: Option<Instant>,
    connected: bool,
    lost: Option<Instant>,
    closed: Option<Instant>,
}

impl Deadlines {
    /// A deadline that passed at `now`, if one did: the far end's close gets [`CLOSED_GRACE`] for the call's own end to
    /// come (a hang-up closes the media first), a loss [`RECOVER_WITHIN`], media never connected [`CONNECT_WITHIN`] of
    /// the far end's SDP. Media the far end closed isn't media that didn't connect.
    fn overdue(self, now: Instant) -> Option<&'static str> {
        let past = |since: Option<Instant>, within: Duration| since.is_some_and(|since| now >= since + within);
        if past(self.closed, CLOSED_GRACE) {
            Some(CLOSED)
        } else if past(self.lost, RECOVER_WITHIN) {
            Some("the media was lost and didn't come back")
        } else if !self.connected && self.lost.is_none() && self.closed.is_none() && past(self.applied, CONNECT_WITHIN)
        {
            Some("the media didn't connect")
        } else {
            None
        }
    }
}

/// Opens the microphone and the speaker of `devices` on the running media, off the engine's thread.
pub(super) fn open_devices(
    live: &LiveMedia,
    devices: Arc<dyn Devices + Send + Sync>,
    unmute: Option<(u64, super::Waiting)>,
) -> impl FnOnce() -> MediaDone + Send + use<> {
    let (call, session) = (live.call, Arc::clone(&live.session));
    move || MediaDone::Opened {
        call,
        result: session.open_devices(&*devices).map_err(|error| error.to_string()),
        unmute,
    }
}

/// Retry playback independently; microphone recovery remains an explicit unmute action.
pub(super) fn retry_speaker(
    live: &LiveMedia,
    devices: Arc<dyn Devices + Send + Sync>,
) -> impl FnOnce() -> MediaDone + Send + use<> {
    let (call, session) = (live.call, Arc::clone(&live.session));
    move || {
        session.retry_speaker(&*devices);
        MediaDone::SpeakerOpened { call }
    }
}

/// Applies the far end's `answer` to our offer, off the engine's thread.
pub(super) fn apply(live: &LiveMedia, answer: MediaContent) -> impl FnOnce() -> MediaDone + Send + use<> {
    let (call, session, dialect) = (live.call, Arc::clone(&live.session), Arc::clone(&live.dialect));
    let group = live.is_group();
    move || {
        let result = ngc(&answer).and_then(|answer| {
            let mut dialect = lock(&dialect);
            let answer = dialect.read_answer(&answer).map_err(|error| error.to_string())?;
            session.set_sharing_lines(dialect.sharing_mids());
            session.accept_answer(answer).map_err(|error| error.to_string())?;
            // a group call's control plane, on the data channel our offer made
            if group {
                session
                    .start_control(dialect.data_stream_id())
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        });
        MediaDone::Applied { call, result }
    }
}

/// Turns a group call's camera on (opened from `devices`) or off, off the engine's thread: no SDP changes (its main
/// line goes both ways throughout), its lines are described again once it turned.
pub(super) fn group_camera(
    live: &LiveMedia,
    devices: Arc<dyn Devices + Send + Sync>,
    on: bool,
    reply: super::Waiting,
) -> impl FnOnce() -> MediaDone + Send + use<> {
    let (call, session) = (live.call, Arc::clone(&live.session));
    move || {
        let result = if on {
            devices
                .camera()
                .map_err(|error| error.to_string())
                .and_then(|camera| session.set_camera(Some(camera)).map_err(|error| error.to_string()))
        } else {
            session.set_camera(None).map_err(|error| error.to_string())
        };
        MediaDone::GroupCamera {
            call,
            on,
            result,
            reply,
        }
    }
}

/// Answers the far end's renegotiation `offer`, off the engine's thread: with our camera's line only receiving while
/// that line owes a re-offer (`LiveMedia::reoffer`: a renegotiation of ours gave way to it, or the account turned the
/// camera while one was open); the line is offered again next ([`offer_camera`]), or the camera turned ([`camera`]).
pub(super) fn answer(live: &LiveMedia, offer: MediaContent) -> impl FnOnce() -> MediaDone + Send + use<> {
    let (call, session, dialect) = (live.call, Arc::clone(&live.session), Arc::clone(&live.dialect));
    let receive_only = live.reoffer != Reoffer::Nothing;
    move || {
        let result = ngc(&offer).and_then(|offer| {
            let mut dialect = lock(&dialect);
            if receive_only && dialect.video() != VideoLines::None {
                dialect.set_video(VideoLines::Main { sending: false });
            }
            let offer = dialect.read_offer(&offer).map_err(|error| error.to_string())?;
            // the far end's screen's frames are the share's, not a camera's
            session.set_sharing_lines(dialect.sharing_mids());
            session.set_group_sharing(dialect.sharing_slot().map(|(mid, _)| mid));
            let answer = session.accept_offer(offer).map_err(|error| error.to_string())?;
            dialect.write_answer(&answer).map_err(|error| error.to_string())
        });
        MediaDone::Answered { call, result }
    }
}

/// Asks the user which screen to share, off the engine's thread (the desktop's portal asks and waits for their
/// answer): the call's negotiations go on meanwhile.
pub(super) fn pick_screen(
    call: CallId,
    devices: Arc<dyn Devices + Send + Sync>,
    reply: super::Waiting,
) -> impl FnOnce() -> MediaDone + Send + use<> {
    move || MediaDone::ScreenPicked {
        call,
        result: devices.screen().map_err(|error| error.to_string()),
        reply,
    }
}

/// Shares `screen` on a sharing line of its own and offers it, off the engine's thread; `reply` is answered once the
/// machine took the offer.
pub(super) fn share(
    live: &LiveMedia,
    screen: Box<dyn VideoSource>,
    reply: super::Waiting,
) -> impl FnOnce() -> MediaDone + Send + use<> {
    let (call, session, dialect) = (live.call, Arc::clone(&live.session), Arc::clone(&live.dialect));
    move || {
        let result = session
            .share_screen(screen)
            .map_err(|error| error.to_string())
            .and_then(|(offer, mid)| {
                let mut dialect = lock(&dialect);
                dialect.mark_sharing(&mid);
                let written = dialect.write_offer(&offer).map_err(|error| error.to_string());
                session.set_sharing_lines(dialect.sharing_mids());
                written.map(Some)
            });
        if result.is_err() {
            // whatever went of it stops: no line of ours is left sending a screen nobody negotiated
            let _ = session.stop_screen();
        }
        MediaDone::Shared {
            call,
            on: true,
            result,
            reply,
        }
    }
}

/// Shares `screen` (`Some`) on a group call's sharing line, or stops sharing it there (`None`), off the engine's
/// thread: nothing is negotiated; `reply` is answered once its lines are described.
pub(super) fn share_group(
    live: &LiveMedia,
    screen: Option<Box<dyn VideoSource>>,
    reply: super::Waiting,
) -> impl FnOnce() -> MediaDone + Send + use<> {
    let (call, session) = (live.call, Arc::clone(&live.session));
    move || {
        let on = screen.is_some();
        let result = if let Some(screen) = screen {
            session.share_group_screen(screen).map_err(|error| error.to_string())
        } else {
            session.stop_group_screen();
            Ok(())
        };
        MediaDone::GroupShared {
            call,
            on,
            result,
            reply,
        }
    }
}

/// Stops sharing the screen and offers its line rejected, off the engine's thread; `reply` is answered as [`share`]'s.
pub(super) fn stop_share(live: &LiveMedia, reply: super::Waiting) -> impl FnOnce() -> MediaDone + Send + use<> {
    let (call, session, dialect) = (live.call, Arc::clone(&live.session), Arc::clone(&live.dialect));
    move || {
        let result = session
            .stop_screen()
            .map_err(|error| error.to_string())
            .and_then(|offer| {
                offer
                    .map(|offer| lock(&dialect).write_offer(&offer).map_err(|error| error.to_string()))
                    .transpose()
            });
        MediaDone::Shared {
            call,
            on: false,
            result,
            reply,
        }
    }
}

/// Offers the running media again (its lines as they are), off the engine's thread; `reply` is answered once the machine
/// took it.
pub(super) fn reoffer(live: &LiveMedia, reply: super::Waiting) -> impl FnOnce() -> MediaDone + Send + use<> {
    let (call, session, dialect) = (live.call, Arc::clone(&live.session), Arc::clone(&live.dialect));
    move || {
        let result = session
            .reoffer()
            .map_err(|error| error.to_string())
            .and_then(|offer| lock(&dialect).write_offer(&offer).map_err(|error| error.to_string()));
        MediaDone::Reoffered { call, result, reply }
    }
}

/// Turns the camera on (opened from `devices`) or off, then offers our video line going that way, off the engine's
/// thread; `reply` is answered once the machine took the offer (or at once, with none to make).
pub(super) fn camera(
    live: &LiveMedia,
    devices: Arc<dyn Devices + Send + Sync>,
    on: bool,
    reply: super::Waiting,
) -> impl FnOnce() -> MediaDone + Send + use<> {
    let (call, session, dialect) = (live.call, Arc::clone(&live.session), Arc::clone(&live.dialect));
    move || {
        let result = turn_camera(&session, &dialect, &*devices, on);
        MediaDone::Camera {
            call,
            on,
            result,
            reply,
        }
    }
}

/// Offers our camera's line again going as the camera does (`on`, the camera left as it is), off the engine's thread:
/// once a renegotiation of ours gave way to the far end's, which was answered with that line only receiving. `reply`
/// is answered as [`camera`]'s is.
pub(super) fn offer_camera(
    live: &LiveMedia,
    on: bool,
    reply: super::Waiting,
) -> impl FnOnce() -> MediaDone + Send + use<> {
    let (call, session, dialect) = (live.call, Arc::clone(&live.session), Arc::clone(&live.dialect));
    move || MediaDone::Camera {
        call,
        on,
        result: offer_line(&session, &dialect, on),
        reply,
    }
}

/// [`camera`]'s blocking part: a camera that doesn't open, or an offer that can't be made, leaves it off. A camera
/// already on is not opened again (an exclusive device wouldn't open twice): only its line is offered.
fn turn_camera(
    session: &MediaSession,
    dialect: &Mutex<NgcSession>,
    devices: &dyn Devices,
    on: bool,
) -> Result<Option<NgcSdp>, String> {
    if on && session.camera_on() {
        return offer_line(session, dialect, on);
    }
    let source = if on {
        Some(devices.camera().map_err(|error| error.to_string())?)
    } else {
        None
    };
    session.set_camera(source).map_err(|error| error.to_string())?;
    offer_line(session, dialect, on)
}

/// Offers our video line going as the camera does (`on`); an offer that can't be made turns the camera off.
fn offer_line(session: &MediaSession, dialect: &Mutex<NgcSession>, on: bool) -> Result<Option<NgcSdp>, String> {
    let text = |error: &dyn std::fmt::Display| error.to_string();
    let mut dialect = lock(dialect);
    dialect.set_video(VideoLines::Main { sending: on });
    let offered = session.offer_video(on).map_err(|error| text(&error)).and_then(|offer| {
        offer
            .map(|offer| dialect.write_offer(&offer).map_err(|error| text(&error)))
            .transpose()
    });
    if offered.is_err() && on {
        dialect.set_video(VideoLines::Main { sending: false });
        let _ = session.set_camera(None);
    }
    offered
}

pub(super) fn lock(dialect: &Mutex<NgcSession>) -> std::sync::MutexGuard<'_, NgcSession> {
    dialect.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A group call's receiving lines' stream ids, in line order, as its dialect names them now.
/// The running group call's sharing line's stream id, as its dialect names it now.
pub(super) fn sharing_slot(live: &LiveMedia) -> Option<u32> {
    lock(&live.dialect).sharing_slot().and_then(|(_, stream)| stream)
}

pub(super) fn slots(live: &LiveMedia) -> Vec<Option<u32>> {
    lock(&live.dialect)
        .video_slots()
        .into_iter()
        .map(|(_, stream)| stream)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(urls: &[&str]) -> Vec<String> {
        urls.iter().map(|url| (*url).to_owned()).collect()
    }

    #[test]
    fn a_microsoft_accounts_relay_is_teams_whatever_its_answer_names() {
        let named = urls(&["turn:turn.own.example.test:3478?transport=udp"]);
        for named in [Vec::new(), named] {
            assert_eq!(Relay::Teams.servers(&Cloud::Microsoft, &named), Ok(teams_relay()));
        }
    }

    #[test]
    fn an_own_server_accounts_relay_is_the_one_its_server_names_never_microsofts() {
        let own = Cloud::own("https://chat.own.example.test").unwrap();
        let servers = Relay::Teams
            .servers(
                &own,
                &urls(&[
                    "turn:turn.own.example.test:3478?transport=udp",
                    "stun:turn.own.example.test",
                    "turns:gateway-world.az.relay.teams.cloud.microsoft:443",
                    "turn:relay.Skype.com.",
                    "turn:a.trafficmanager.net",
                    "turns:turn.own.example.test",
                ]),
            )
            .unwrap();
        let urls_of: Vec<String> = servers.iter().map(RelayServer::url).collect();
        assert_eq!(
            urls_of,
            [
                "turn:turn.own.example.test:3478?transport=udp",
                "turns:turn.own.example.test:5349?transport=tcp"
            ]
        );
        // a host that only ends like a Microsoft domain is the own server's
        assert!(Relay::Teams.servers(&own, &urls(&["turn:notmicrosoft.com"])).is_ok());
        for named in [
            Vec::new(),
            urls(&["turn:gateway-world.az.relay.teams.cloud.microsoft"]),
            urls(&["https://turn.own.example.test"]),
        ] {
            assert!(Relay::Teams.servers(&own, &named).is_err(), "{named:?}");
        }
        let fixed = Relay::Fixed(vec![RelayServer::udp("127.0.0.1:3478".parse().unwrap())]);
        assert_eq!(fixed.servers(&own, &[]).unwrap().len(), 1);
    }

    #[test]
    fn media_the_far_end_closed_gets_its_grace_however_long_ago_it_was_applied() {
        let start = Instant::now();
        let closed_at = start + Duration::from_secs(40);
        let closed = Deadlines {
            applied: Some(start),
            connected: false,
            lost: None,
            closed: Some(closed_at),
        };
        // the call's own end may still come: no failure yet, though the connect deadline is long past
        assert_eq!(closed.overdue(closed_at), None);
        assert_eq!(
            closed.overdue(closed_at + CLOSED_GRACE.saturating_sub(Duration::from_millis(1))),
            None
        );
        assert_eq!(
            closed.overdue(closed_at + CLOSED_GRACE),
            Some("the far end closed the media")
        );
    }

    #[test]
    fn media_that_never_connects_or_stays_lost_fails_on_its_deadline() {
        let start = Instant::now();
        let never = Deadlines {
            applied: Some(start),
            ..Deadlines::default()
        };
        assert_eq!(
            never.overdue(start + CONNECT_WITHIN.saturating_sub(Duration::from_millis(1))),
            None
        );
        assert_eq!(never.overdue(start + CONNECT_WITHIN), Some("the media didn't connect"));
        let lost = Deadlines {
            applied: Some(start),
            lost: Some(start + Duration::from_secs(60)),
            ..Deadlines::default()
        };
        assert_eq!(lost.overdue(start + Duration::from_secs(61)), None);
        assert_eq!(
            lost.overdue(start + Duration::from_secs(60) + RECOVER_WITHIN),
            Some("the media was lost and didn't come back")
        );
        let up = Deadlines {
            applied: Some(start),
            connected: true,
            ..Deadlines::default()
        };
        assert_eq!(up.overdue(start + Duration::from_secs(3_600)), None);
    }
}

#[cfg(test)]
#[path = "../../tests/support/media_fallbacks.rs"]
mod fallback_tests;
