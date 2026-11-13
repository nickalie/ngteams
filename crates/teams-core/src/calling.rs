//! The account's calls in the engine (M3 Task 14): the call session that joins the signaling machine (`teams-call`),
//! the calling stack's socket (`teams-trouter`'s `Trouter::for_calls`), the outbox's call kind and the call's media
//! (`teams-media`).
//!
//! - **Actions** ([`CallAction`], `EngineRequests::call`): a call from a chat or the test call, a join of a chat's
//!   running call, an answer or a decline of an invitation the engine offered ([`Event::IncomingCall`]), a hang-up,
//!   muting the microphone, the camera turned on or off, a renegotiation of the call's media. One call at a time: an
//!   action while one is up (or set up) is refused. Nothing answers or declines an invitation but its action: the engine never does by itself.
//! - **The media comes first, the devices last**: a start, a join or an answer first waits for device discovery,
//!   outside [`SETUP_TIMEOUT`], then goes only once its media is set up
//!   ([`CallMedia`]: the relay's credentials read, an allocation on the relay, the session started without its devices,
//!   our offer or our answer in Teams' dialect, within [`SETUP_TIMEOUT`]); a setup that fails refuses the action and
//!   sends nothing (an invitation keeps ringing, still answerable or declinable). A start's or a join's call is told
//!   preparing while discovery runs, otherwise placing as setup begins, so its window appears at once. Once the
//!   machine places it, it is told placing; a setup that fails ends that call
//!   (`End::Failed("setup")`), a hang-up meanwhile drops it unsent, and a mute meanwhile holds once its media runs.
//!   The microphone and the speaker open only once the start, the join or the answer was admitted and taken: when the
//!   machine tells the call ringing at its callees, joining, or connected. An audio device that cannot open is told
//!   as [`Event::AudioClosed`], without ending the call: a missing microphone stays muted, and received audio is
//!   decoded even without a speaker. A missing speaker is reopened about once a second for the rest of the call,
//!   playback resuming once it opens; a missing microphone only on an unmute, which retries it off the engine thread
//!   before it can send audio. An unmute a newer mute (the user's or the organizer's) overtook meanwhile leaves the
//!   microphone muted and is answered [`CallActionError::Superseded`], not as applied.
//! - **The writes** go out through the outbox's call kind (`Outbox::try_call`), one in flight at a time: beside it the
//!   machine's events and delivery responses go at once, its next writes wait in its own queue (so a call that ends
//!   drops them); a write refused without being acted on goes again only within `CallRetries`' allowance and only if
//!   the machine still wants it once its wait is over (`Calls::may_send_again`, asked again then: a hang-up during the
//!   wait fails it unsent); its answer (any status, with its body) goes back to the machine. The outbox and calling
//!   socket feed the call tracker: a start's answer records its call, a definite failure retires its pending attempt,
//!   and an acceptance binds its links. The core's shared floor admits one call or ring across the app's accounts.
//! - **A renegotiation of the account's** begins in the machine (`Calls::begin_renegotiation`) before the media session
//!   makes its offer, since a new offer replaces the one the session waits on: a second one, or one while the call
//!   isn't up or the far end's waits for our answer, is refused with the session untouched. The camera (a 1:1 call's
//!   video, [`CallVideo`]; off when a call starts) is one: turned on or off, our video line is offered going that
//!   way, the call's modalities naming `Video` while a video line is in use. One the call controller turned away
//!   (`491`) for the far end's (both cameras turned on together) gives way: the far end's is answered with our
//!   camera's line only receiving, then that line is offered again as the camera goes. A camera that stops by itself
//!   (its device gone) is off, told as [`Event::CameraOff`]; a microphone or speaker whose stream
//!   ended (not moved) is told as [`Event::AudioClosed`], the call going on (the speaker reopened, as above).
//! - **The media's SDP steps run one at a time, in the order the signaling asked for them** (the far end's answer
//!   applied, its renegotiation answered, our own offered): each waits for the one before, so an acceptance followed
//!   at once by a renegotiation reaches the session in that order.
//! - **A group call's video** (M3 Task 19): its camera goes on or off without an SDP change (its main video line goes
//!   both ways throughout, as the web client's): the camera turned, then its lines described again to the call's
//!   services (`Calls::describe_media`, with its send capabilities when it went on). Who is in the call comes from its
//!   roster (`CallEvent::Participants`); whose video each receiving line shows is the gallery's to say
//!   ([`gallery`]), sent to the mixer as source requests once its control plane is up; told as
//!   [`Event::CallParticipants`] with who speaks.
//! - **The calling socket's deliveries** go to the machine as the call tracker read them; a typed delivery response
//!   goes only through that delivery's connection-bound responder. An invitation the tracker ignores never gets here.
//! - **What the account is told**: [`Event::IncomingCall`], [`Event::RingEnded`] and [`Event::Call`] with the call's
//!   [`CallStatus`]: *connecting* once the signaling says the call is up until the media connected, *reconnecting*
//!   while the media is lost, *ended* once, with how.
//! - **What a failure does** (Task 14's decision with Codex): a step that fails (not sent, denied, unknown, or not
//!   answered `2xx`) ends the call, with a `leave` when the account is in it (`teams-call`); the far end's SDP that
//!   doesn't apply, a renegotiation that can't be answered, the session failing, media that doesn't connect within
//!   [`CONNECT_WITHIN`] of the far end's SDP or stays lost past [`RECOVER_WITHIN`] fail the call the same way
//!   (`End::Failed("media")`).
//! - **The engine stopping** hangs the call up (its `leave`, or the decline of an answer whose `accept` hasn't gone),
//!   lets an invitation that rings go unanswered, waits up to [`SHUTDOWN_GRACE`] for those last writes (a start or a
//!   join in flight up to its request's timeout more, for the answer its `leave` needs), closes the
//!   media and stops the calling socket.
//!
//! The calling runs as a task of its own on the engine's thread ([`Calling::spawn`]; a `LocalSet`'s, as the outbox it
//! writes with isn't `Send`), so the write in flight, the calling socket's deliveries and their responses go on while
//! the engine awaits a chat's work; the engine reaches it through its [`CallingTask`].
//!
//! A call survives the calling socket's reconnect: its links stay as they were (the capture shows a dropped socket
//! changes none), the socket registers again on the same endpoint, and the media runs on its own threads throughout.

mod captions;
pub use captions::{CallCaptions, Caption, CaptionPhase};
mod recording;
pub(crate) use recording::record_act;
pub use recording::{CallRecording, RecordingPhase, RecordingProblem};
mod gallery;
mod health;
pub use health::{CallHealth, MediaHealth, NetworkHealth, StreamHealth};
mod controls;
pub(crate) use controls::Options;
mod media;
mod people;
mod share;

pub use gallery::{CallParticipant, CallView, HEARING_LEVEL, SPEAKING_HOLD, SPEAKING_LEVEL};
pub use media::{CLOSED_GRACE, CONNECT_WITHIN, CallMedia, CallVideo, GROUP_SLOTS, RECOVER_WITHIN, Relay, VideoOutput};

use std::collections::{HashMap, VecDeque};
use std::future::{Future, pending};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use teams_call::{
    CallError, CallEvent, CallId, CallState, CallWrite, Calls, DeliveryKey, DeliveryReply, Effect, End, Identity,
    Outcome, Participant, Placement, WriteId, WriteSink,
};
use teams_proto::Mri;
use teams_proto::{
    ChannelMeetup, ChannelParameters, Client, Credentials, EndpointRegistrar, HttpError, HttpTransport, MediaContent,
    MeetingInfo, Request, Transport,
};
use teams_trouter::{CallResponder, CallResponse, Ignored, PushEvent, Trouter, TrouterCredentials, TrouterHandle};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;

use crate::engine::{EngineError, Event, Push, emit_to};
use crate::outbox::{CallRetries, CallTried, Outbox};
use media::{LiveMedia, MediaChange, MediaDone, Prepared, Setup, VideoKind};

/// How long the engine, stopping, waits for a call's last writes (its `leave`, its decline).
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// How often a running call's media is looked at.
const MEDIA_LOOK: Duration = Duration::from_millis(50);
/// How long a start's, a join's or an answer's media may take to be set up (the relay's credentials read, the session
/// made, its candidates gathered) before the action fails: a setup that never ends would hold the calling busy, every
/// later start refused, with nothing to show for it.
/// Device discovery is awaited first, outside this bound; a waiting action can be cancelled.
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a start or a join may wait for the engine to hand it to the calling (behind a write or a sync the engine
/// awaits) before it is refused ([`CallActionError::Late`]) rather than placed. A view gives up on a start it isn't
/// told of within a longer wait of its own and says so; one placed after that would ring people who were told it
/// didn't start.
pub const START_WITHIN: Duration = Duration::from_secs(15);

/// The account's calling, built and not started: its calling socket (`Trouter::for_calls`, over the transport the
/// outbox writes with, so its deliveries and the call writes feed one call registry), who the account is in its calls
/// and what their media is made of.
pub struct CallParts {
    push: Push,
    identity: Identity,
    media: CallMedia,
}

impl CallParts {
    /// The calling of `identity` (its `endpoint_id` taken from `trouter`, the calling endpoint it registers) on
    /// `trouter`, the calling stack's socket, its calls' media made of `media`.
    #[must_use]
    pub fn new<T, K>(trouter: Trouter<T, K>, mut identity: Identity, media: CallMedia) -> Self
    where
        T: EndpointRegistrar + Send + 'static,
        K: TrouterCredentials + 'static,
    {
        trouter.endpoint_id().clone_into(&mut identity.endpoint_id);
        Self {
            push: Push::new(trouter),
            identity,
            media,
        }
    }
}

impl std::fmt::Debug for CallParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallParts")
            .field("identity", &self.identity.mri)
            .field("media", &self.media)
            .finish_non_exhaustive()
    }
}

/// What the account asks of its calls (`EngineRequests::call`); each answered with the call it acted on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallAction {
    /// Show or hide this viewer's live captions.
    Captions { call: CallId, enabled: bool },
    /// Change shared spoken language after the user confirms everyone is speaking it.
    CaptionLanguage { call: CallId, language: String },
    /// Start recording (More → Record and transcribe → Start recording, confirmed), with its transcript unless a
    /// transcription runs already: the call's `title` as Teams names its recording ("Call with …", a meeting's title),
    /// a meeting's `organizer`'s name, the spoken `language` picked (a `teams_proto::captions::SPOKEN_LANGUAGES` code),
    /// and whether it records the audio alone ("Choose what to record" → Audio only).
    Record {
        call: CallId,
        title: String,
        organizer: Option<String>,
        language: String,
        audio_only: bool,
    },
    /// Stop recording (Stop recording, confirmed), with the transcription while it runs, whoever started them.
    StopRecording { call: CallId },
    /// Start the transcription alone (More → Record and transcribe → Start transcription, its language confirmed),
    /// named as [`CallAction::Record`] names a recording.
    Transcribe {
        call: CallId,
        title: String,
        organizer: Option<String>,
        language: String,
    },
    /// Stop the transcription alone (Stop transcription, confirmed), whoever started it.
    StopTranscription { call: CallId },
    /// A call from the chat `chat`: a 1:1 chat's other member, or a group chat's other members as the store holds
    /// them.
    Call { chat: String },
    /// The test call of the echo service `echo`.
    TestCall { echo: Mri },
    /// A join of the running call in the group chat `chat`, one an invitation named.
    Join { chat: String },
    /// The account goes into the call of the meeting whose chat is `chat`: it starts the call when nobody has yet, and
    /// joins it when it runs.
    JoinMeeting { chat: String, meeting: MeetingInfo },
    /// Join an invitation by ID and passcode, resolved by the calling service.
    JoinMeetingCode(teams_proto::MeetingCode),
    /// A new meeting in the channel `channel` (a team's channel thread), its header's Meet now: the account starts it
    /// and organizes it, and the service posts it to the channel.
    MeetNow { channel: String },
    /// The account goes into the meeting that runs in the channel `channel` now, as CSA names it (its header's Join).
    JoinChannel { channel: String },
    /// The answer of an invitation the engine offered ([`Event::IncomingCall`]).
    Answer { call: CallId },
    /// The decline of an invitation the engine offered.
    Decline { call: CallId },
    /// A hang-up: a cancel while it rings at its callees, a `leave` once it is up, a decline of an answer whose
    /// `accept` hasn't gone.
    HangUp { call: CallId },
    /// The microphone muted (`true`) or not: silence goes in its place, and the call's conversation is told, as the
    /// web client tells it (`updateEndpointState`, seen 2026-11-06), so the others see the account muted.
    Mute { call: CallId, muted: bool },
    /// The call's media offered again (its lines as they are) as a renegotiation.
    Renegotiate { call: CallId },
    /// The camera of a call that takes video turned on (`true`) or off, the camera's frames sent while it is on: a 1:1
    /// call's video line offered again going that way (a renegotiation); a group call's lines described again
    /// (`updateMediaDescriptions`) with no SDP change, only once the call is up.
    Camera { call: CallId, on: bool },
    /// Someone waiting in the lobby of the account's meeting ([`Event::Lobby`]) let in (`admit`) or turned away, as
    /// Teams' Admit and Deny.
    Lobby { call: CallId, waiting: Mri, admit: bool },
    /// The account's hand raised (`true`) or lowered in the call, as Teams' Raise and Lower hand.
    RaiseHand { call: CallId, raised: bool },
    /// A live reaction to everyone in the call, as Teams' React offers them (`like`, `heart`, `applause`, `laugh`,
    /// `surprised`).
    React { call: CallId, reaction: String },
    /// The meeting ended for everyone in it, as its organizer's End meeting ([`Event::CallOwn`] says who organizes).
    EndMeeting { call: CallId },
    /// A 1:1 call put on hold (`true`: the far end hears the service's music, the microphone sends silence) or taken
    /// off it, as Teams' Hold and Resume ([`Event::CallHeld`] tells it as it stands; the user's own mute comes back).
    Hold { call: CallId, held: bool },
    /// The meeting's timer set as Teams' Timer sets it (Start, Pause, Play, Reset, Cancel timer): everyone in the call
    /// sees it ([`Event::CallMeeting`] tells it as the conversation says).
    Timer {
        call: CallId,
        timer: teams_proto::MeetingTimer,
    },
    /// The meeting locked (`true`: nobody else can join) or unlocked by its organizer, as Teams' Lock the meeting.
    LockMeeting { call: CallId, locked: bool },
    /// The call's screen shared (`true`: the user picks it at the desktop's portal, its frames sent on a sharing line
    /// of its own in a 1:1 call, on the mixer's sharing line in a group call or meeting) or no longer, as Teams' Share
    /// and Stop sharing ([`Event::CallSharing`] tells it as it stands). A call whose media carries no video shares
    /// nothing.
    Share { call: CallId, on: bool },
    /// `invitee` added to the call, as Teams' People pane adds someone (seen 2026-11-06): a 1:1 call first gets a new
    /// group chat of the three and becomes a group call in it ([`Event::CallGroupChat`]); a group call's own chat is
    /// named. The invitee rings; one who doesn't come in is told ([`Event::CallAddFailed`]).
    Add { call: CallId, invitee: Mri },
    /// The 1:1 call's far end transferred to `target`, as Teams' Transfer to an audio call (seen 2026-11-06): once the
    /// far end took it the account leaves the call; one not taken is told ([`Event::CallTransferFailed`]).
    Transfer { call: CallId, target: Mri },
    /// Everyone else in the call muted by its organizer, as People's Mute all (seen 2026-11-06): they see themselves
    /// muted ([`Event::CallMeeting`]'s `muted_by_organizer` at their end).
    MuteAll { call: CallId },
    /// One of the call's meeting options changed by its organizer, as Teams' Controls change it (Enable lobby, Allow
    /// all participants to present, chat, unmute themselves, turn on their cameras): answered once the service took
    /// it; everyone in the call is told with the conversation's next update ([`Event::CallMeeting`]).
    MeetingOption {
        call: CallId,
        option: teams_proto::MeetingOption,
    },
}

impl CallAction {
    /// Whether it starts or joins a call (what [`START_WITHIN`] bounds), rather than acting on one.
    #[must_use]
    pub fn starts(&self) -> bool {
        matches!(
            self,
            Self::Call { .. }
                | Self::TestCall { .. }
                | Self::Join { .. }
                | Self::JoinMeeting { .. }
                | Self::JoinMeetingCode(_)
                | Self::MeetNow { .. }
                | Self::JoinChannel { .. }
        )
    }
}

/// Where a call is, as the account is told ([`Event::Call`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallStatus {
    /// Native device discovery is still running; the start or join has not been sent.
    Preparing,
    /// The account's start or join goes out.
    Placing,
    /// The account's call rings at its callees.
    Ringing,
    /// The account's join was taken; its acceptance comes next.
    Joining,
    /// The account's answer goes out.
    Answering,
    /// The account's decline goes out.
    Declining,
    /// The call is up as signaling goes; its media isn't connected (yet).
    Connecting,
    /// The call is up and its media connected.
    Connected,
    /// The media was lost and may come back ([`RECOVER_WITHIN`]).
    Reconnecting,
    /// The account waits in the meeting's lobby until someone in the meeting lets it in (the call goes on as it was
    /// then) or turns it away (its end follows).
    Lobby,
    /// The account leaves.
    Leaving,
    /// The call is over, as `End` says.
    Ended(End),
}

/// Someone waiting in the lobby of the account's meeting ([`Event::Lobby`]), as the roster names them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LobbyPerson {
    pub mri: Mri,
    /// As the roster gives it.
    pub name: Option<String>,
}

/// An invitation that rings here: answer it or decline it ([`CallAction::Answer`], [`CallAction::Decline`]); nothing
/// does otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncomingCall {
    pub call: CallId,
    pub caller: Mri,
    /// The caller's name as the invitation gives it (no profile lookup).
    pub caller_name: Option<String>,
    /// The group chat of a group call.
    pub group_chat: Option<String>,
}

/// Why a call action was refused. Nothing was sent.
#[derive(Debug, thiserror::Error)]
pub enum CallActionError {
    /// The engine runs without calling (`Parts::calls` `None`) or native device discovery failed.
    #[error("this engine has no calling")]
    NoCalling,
    /// A call is up or being set up: one at a time.
    #[error("a call is already up")]
    Busy,
    /// The chat names nobody to call.
    #[error("that chat names nobody to call")]
    NotAChat,
    /// No media runs for that call.
    #[error("no media runs for that call")]
    NoMedia,
    /// That call's media carries no video: the engine has no video for its calls, the settings' video switch was off
    /// when it was set up, or the codec refused its setup.
    #[error("that call carries no video")]
    NoVideo,
    /// The call's media couldn't be set up, or its SDP made.
    #[error("the call's media couldn't be set up: {0}")]
    Media(String),
    /// A start or a join waited longer than [`START_WITHIN`] to be taken: nothing was sent.
    #[error("the call waited too long to start")]
    Late,
    /// The machine refused it.
    #[error(transparent)]
    Call(#[from] CallError),
    /// The service didn't take the change (a meeting option): it stays as it was.
    #[error("the change wasn't taken")]
    NotChanged,
    /// An unmute that waited on its microphone to reopen was overtaken by a newer mute (the user's own, or the
    /// organizer's Mute all): the microphone stays muted, as that newer mute left it.
    #[error("a newer mute took its place")]
    Superseded,
}

/// Where an action's answer goes.
pub(crate) type Waiting = oneshot::Sender<Result<CallId, EngineError>>;

/// An action the engine resolved (a chat's call into its placement).
pub(crate) enum Act {
    Captions(CallId, bool),
    CaptionLanguage(CallId, String),
    /// A recording or a transcription started or stopped.
    Record(CallId, teams_proto::captions::RecordCommand),
    Place(Placement),
    /// The running call of a group chat joined: the chat, and where its own newest start says it runs (the store's,
    /// `teams_proto::running_call`), for a meeting no invitation named.
    Join(String, Option<teams_proto::CallLink>),
    /// The meeting that runs in a channel joined: the channel, and the meeting as CSA names it, read as the media is
    /// set up ([`Calling::set_up`]).
    JoinChannel(String, Arc<std::sync::Mutex<Option<ChannelMeetup>>>),
    Answer(CallId),
    Decline(CallId),
    HangUp(CallId),
    Mute(CallId, bool),
    Renegotiate(CallId),
    Camera(CallId, bool),
    Lobby(CallId, Mri, bool),
    RaiseHand(CallId, bool),
    React(CallId, String),
    EndMeeting(CallId),
    Hold(CallId, bool),
    Timer(CallId, teams_proto::MeetingTimer),
    LockMeeting(CallId, bool),
    Share(CallId, bool),
    Add(CallId, Mri),
    Transfer(CallId, Mri),
    MuteAll(CallId),
    /// The call's meeting options read or one changed ([`controls`]).
    Options(CallId, controls::Options),
    /// The call's health read ([`health`]): no action, its answer goes to its own responder.
    Health(CallId, crate::engine::Responder<Option<CallHealth>>),
}

type Attempt = Pin<Box<dyn Future<Output = CallTried>>>;
type Again = Box<dyn FnMut() -> Attempt>;

/// The one call write in flight: where it is, how to make another attempt, and the further attempts it has used.
struct Writing {
    id: WriteId,
    name: &'static str,
    flight: Flight,
    again: Again,
    retries: CallRetries,
}

/// Where the write in flight is.
enum Flight {
    /// An attempt is under way.
    Attempt(Attempt),
    /// Another attempt is due once this wait is over, if the machine still wants the write then.
    Waiting(Pin<Box<tokio::time::Sleep>>),
}

/// What the write in flight came to.
enum Landed {
    /// Its attempt is over.
    Tried(CallTried),
    /// Its wait for another attempt is over.
    Due,
}

/// Media being set up for an action, with the action's answer.
struct Preparing {
    /// What it is set up for: a start or join (`None`), or the answer of an offered invitation.
    answering: Option<CallId>,
    /// A start's or a join's call, told preparing or placing as setup begins so its window appears at once;
    /// a failed setup ends it there.
    told: Option<CallId>,
    /// When its setup began, for the log.
    since: Instant,
    /// The microphone muted while it was set up.
    muted: bool,
    act: Act,
    reply: Waiting,
    setup: Pin<Box<dyn Future<Output = Result<Prepared, CallActionError>>>>,
}

/// A call's media being set up again once it was let in from its meeting's lobby ([`Calling::rebuild`]).
type Rebuilding = (CallId, Pin<Box<dyn Future<Output = Result<Prepared, String>>>>);

/// What [`Calling::next`] found to do.
enum CallStep {
    Push(Option<PushEvent>),
    Written(CallTried),
    CaptionWritten(captions::Operation, CallTried),
    RecordWritten(recording::Operation, CallTried),
    /// A 1:1 call's new group chat made (or not) for someone added to it.
    ChatMade(Box<(CallId, Mri, CallTried)>),
    Due,
    Prepared(Box<Result<Prepared, CallActionError>>),
    /// A call's media set up again once it was let in from its meeting's lobby ([`Calling::rebuild`]).
    Rebuilt(Box<(CallId, Result<Prepared, String>)>),
    Media(MediaDone),
    /// A media step ended without its outcome (it panicked).
    MediaLost,
    Responded(DeliveryKey, bool),
    Look,
}

/// What the recording bot's write in flight (live captions' or a recording's) came to; never, while there is none.
async fn service_written(
    captions: Option<&mut (captions::Operation, Attempt)>,
    recording: Option<&mut (recording::Operation, Attempt)>,
) -> CallStep {
    tokio::select! {
        (operation, tried) = captions::next(captions) => CallStep::CaptionWritten(operation, tried),
        (operation, tried) = recording::next(recording) => CallStep::RecordWritten(operation, tried),
    }
}

/// The engine's hold on its calling, which runs as a task of its own ([`Calling::spawn`]).
pub(crate) struct CallingTask {
    actions: mpsc::UnboundedSender<(Act, Waiting)>,
    task: tokio::task::JoinHandle<()>,
}

/// The calling's actions as another part of the engine hands them in ([`CallingTask::actions`]): the engine's inbox,
/// which hands a call action in while a catch-up's read waits. It doesn't keep the calling running: once its
/// [`CallingTask`] shut it down, an action is answered [`EngineError::Stopped`].
#[derive(Clone)]
pub(crate) struct CallActions(mpsc::WeakUnboundedSender<(Act, Waiting)>);

impl CallActions {
    /// The actions that go to `actions`, for a test that stands in for the calling.
    #[cfg(test)]
    pub(crate) fn of(actions: &mpsc::UnboundedSender<(Act, Waiting)>) -> Self {
        Self(actions.downgrade())
    }

    /// Hands the account's action to the calling; its answer goes to `reply`.
    pub(crate) fn act(&self, act: Act, reply: Waiting) {
        let Some(actions) = self.0.upgrade() else {
            let _ = reply.send(Err(EngineError::Stopped));
            return;
        };
        if let Err(refused) = actions.send((act, reply)) {
            let (_, reply) = refused.0;
            let _ = reply.send(Err(EngineError::Stopped));
        }
    }

    /// Hands a read of the call's health to the calling; its answer goes to `reply` (dropped unanswered, it hears the
    /// engine stopped).
    pub(crate) fn health(&self, call: CallId, reply: crate::engine::Responder<Option<CallHealth>>) {
        let (unused, _) = oneshot::channel();
        self.act(Act::Health(call, reply), unused);
    }

    /// Hands a read of the call's meeting options to the calling; its answer goes to `reply`.
    pub(crate) fn meeting_options(
        &self,
        call: CallId,
        reply: crate::engine::Responder<Option<teams_proto::MeetingOptions>>,
    ) {
        let (unused, _) = oneshot::channel();
        self.act(Act::Options(call, controls::Options::Read(reply)), unused);
    }
}

impl CallingTask {
    /// Its actions, for another part of the engine to hand them in.
    pub(crate) fn actions(&self) -> CallActions {
        CallActions(self.actions.downgrade())
    }

    /// Stops the calling as [`Calling::shut_down`] does, once that is done.
    pub(crate) async fn shut_down(self) {
        drop(self.actions);
        if let Err(error) = self.task.await {
            log::warn!("the calling's task ended badly: {error}");
        }
    }
}

/// The engine's calling (see the module docs).
pub(crate) struct Calling<C, T> {
    calls: Calls,
    captions: captions::Captions,
    recording: recording::Recordings,
    outbox: Arc<Outbox<T>>,
    client: Arc<Client<HttpTransport<C>>>,
    media: CallMedia,
    push: Option<TrouterHandle>,
    unstarted: Option<Push>,
    responders: HashMap<DeliveryKey, CallResponder>,
    writing: Option<Writing>,
    preparing: Option<Preparing>,
    /// The call told placing as its setup began, until the machine tells it placing in turn (told once).
    told_placing: Option<CallId>,
    /// A 1:1 call's new group chat being made for someone added to it ([`CallAction::Add`]).
    making: Option<people::Making>,
    /// The call to the target of a transfer the far end asked for, placed once the transferred call's media is gone.
    transfer_to: Option<(CallId, Placement)>,
    /// While the account holds its call: that call, and whether the user had the microphone muted (what the hold's end
    /// gives back; a mute asked while held is kept here).
    held_mute: Option<(CallId, bool)>,
    /// The organizer's latest mute of the account (Mute all) the microphone took: its call and its version.
    organizer_mute: Option<(CallId, u64)>,
    /// Invalidates asynchronous unmute requests when a newer user or organizer mute arrives.
    mute_generation: u64,
    media_steps: JoinSet<MediaDone>,
    /// The SDP steps waiting for the one running to finish, in the order the signaling asked for them.
    sdp_steps: VecDeque<Box<dyn FnOnce() -> MediaDone + Send>>,
    /// An SDP step runs now.
    sdp_running: bool,
    responses: JoinSet<(DeliveryKey, bool)>,
    live: Option<LiveMedia>,
    /// The offers of invitations the engine offered, by call (an answer answers it), with whether the invitation is
    /// a group call's.
    offers: HashMap<CallId, (Option<MediaContent>, bool)>,
    /// The call the signaling says is up: told *connecting* until its media connected.
    up: Option<CallId>,
    /// The call whose meeting's lobby the account waits in.
    lobby: Option<CallId>,
    /// The call the account was just let in from its meeting's lobby: the mixer's next offer is a session of its own
    /// (every line numbered anew, its RTP header extensions too: seen 2026-11-03), so the call's media is set up again
    /// for it ([`Self::rebuild`]).
    admitted: Option<CallId>,
    /// The call's media set up again for the mixer's offer once let in from the lobby, while it is.
    rebuilding: Option<Rebuilding>,
    /// The call whose media was set up again so: the roster telling it let in afterwards asks for nothing more.
    replaced: Option<CallId>,
    look: Option<tokio::time::Interval>,
    events: mpsc::UnboundedSender<Event>,
}

/// The engine's sink for one call write: it keeps the write (to try again) and makes each attempt through the outbox.
struct Sink<T> {
    outbox: Arc<Outbox<T>>,
    made: Option<(Attempt, Again)>,
}

impl<T: Transport<Error = HttpError> + 'static> WriteSink for Sink<T> {
    fn submit<R: Request + Clone + Send + 'static>(&mut self, _id: WriteId, write: R) {
        let outbox = Arc::clone(&self.outbox);
        let mut again: Again = Box::new(move || {
            let (outbox, write) = (Arc::clone(&outbox), write.clone());
            Box::pin(async move { outbox.try_call(write).await })
        });
        self.made = Some((again(), again));
    }
}

impl<C, T> Calling<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    pub(crate) fn new(
        parts: CallParts,
        outbox: Arc<Outbox<T>>,
        client: Arc<Client<HttpTransport<C>>>,
        events: mpsc::UnboundedSender<Event>,
    ) -> Self {
        Self {
            calls: Calls::new(parts.identity),
            captions: captions::Captions::default(),
            recording: recording::Recordings::default(),
            outbox,
            client,
            media: parts.media,
            push: None,
            unstarted: Some(parts.push),
            responders: HashMap::new(),
            writing: None,
            preparing: None,
            told_placing: None,
            held_mute: None,
            organizer_mute: None,
            mute_generation: 0,
            making: None,
            transfer_to: None,
            media_steps: JoinSet::new(),
            sdp_steps: VecDeque::new(),
            sdp_running: false,
            responses: JoinSet::new(),
            live: None,
            offers: HashMap::new(),
            up: None,
            lobby: None,
            admitted: None,
            rebuilding: None,
            replaced: None,
            look: None,
            events,
        }
    }

    /// Starts the calling socket and runs the calling as a task of its own on the engine's thread (a `LocalSet`'s:
    /// the outbox it writes with isn't `Send`), taking the account's actions, until its [`CallingTask`] shuts it down.
    pub(crate) fn spawn(mut self) -> CallingTask {
        let (actions, mut asked) = mpsc::unbounded_channel::<(Act, Waiting)>();
        let task = tokio::task::spawn_local(async move {
            self.start();
            loop {
                tokio::select! {
                    action = asked.recv() => match action {
                        Some((act, reply)) => self.act(act, reply),
                        None => break,
                    },
                    step = self.next() => self.step(step),
                }
            }
            Box::pin(self.shut_down()).await;
        });
        CallingTask { actions, task }
    }

    /// Starts the calling socket (on the engine's runtime).
    fn start(&mut self) {
        if let Some(push) = self.unstarted.take() {
            self.push = Some(push.spawn());
        }
        let mut look = tokio::time::interval(MEDIA_LOOK);
        look.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        self.look = Some(look);
    }

    fn emit(&self, event: Event) {
        emit_to(&self.events, event);
    }

    /// Microsoft's recording bot changed in the call's roster: live captions and a recording waiting for it go on.
    fn service_changed(&mut self, call: CallId) {
        self.caption_service_changed(call);
        self.record_service_changed(call);
    }

    /// Microsoft's recording bot couldn't join the call.
    fn service_join_failed(&mut self, call: CallId) {
        self.record_join_failed(call);
        if self.up == Some(call) {
            self.caption_fail("Live captions could not join this call. Try again.");
        }
    }

    /// An action of Microsoft's recording bot: live captions shown or hidden, their language, a recording.
    fn service_act(&mut self, act: Act) -> Result<CallId, CallActionError> {
        match act {
            Act::Captions(call, enabled) => self.caption_toggle(call, enabled),
            Act::CaptionLanguage(call, language) => self.caption_language(call, &language),
            Act::Record(call, command) => self.record(call, command),
            _ => Err(CallActionError::Call(CallError::NotNow)),
        }
    }

    /// The next thing to do; never, while there is nothing.
    async fn next(&mut self) -> CallStep {
        let looking = self.live.is_some();
        tokio::select! {
            step = service_written(self.captions.writing.as_mut(), self.recording.writing.as_mut()) => step,
            made = people::made(self.making.as_mut()) => CallStep::ChatMade(Box::new(made)),
            event = next_push(self.push.as_mut()) => CallStep::Push(event),
            landed = land(self.writing.as_mut()) => match landed {
                Landed::Tried(tried) => CallStep::Written(tried),
                Landed::Due => CallStep::Due,
            },
            prepared = prepared(self.preparing.as_mut()) => CallStep::Prepared(Box::new(prepared)),
            rebuilt = rebuilt(self.rebuilding.as_mut()) => CallStep::Rebuilt(Box::new(rebuilt)),
            Some(done) = self.media_steps.join_next(), if !self.media_steps.is_empty() => match done {
                Ok(done) => CallStep::Media(done),
                Err(_) => CallStep::MediaLost,
            },
            Some(done) = self.responses.join_next(), if !self.responses.is_empty() => match done {
                Ok((key, sent)) => CallStep::Responded(key, sent),
                Err(_) => CallStep::Look,
            },
            () = look(self.look.as_mut()), if looking => CallStep::Look,
        }
    }

    /// Does what `step` asks, then carries out what the machine asks in turn.
    fn step(&mut self, step: CallStep) {
        let now = Instant::now();
        match step {
            CallStep::Push(event) => self.pushed(event, now),
            CallStep::CaptionWritten(operation, tried) => self.caption_written(operation, &tried),
            CallStep::RecordWritten(operation, tried) => self.record_written(operation, &tried),
            CallStep::ChatMade(made) => {
                let (call, invitee, tried) = *made;
                self.chat_made(call, invitee, &tried);
            }
            CallStep::Written(tried) => self.written(tried, now),
            CallStep::Due => self.due(now),
            CallStep::Prepared(prepared) => self.prepared(*prepared, now),
            CallStep::Rebuilt(rebuilt) => {
                let (call, result) = *rebuilt;
                self.rebuilt(call, result, now);
            }
            CallStep::Media(done) => self.media_done(done, now),
            CallStep::MediaLost => {
                self.sdp_running = false;
                self.next_sdp_step();
            }
            CallStep::Responded(key, sent) => {
                if !sent {
                    self.calls.on_response_failed(&key);
                }
            }
            CallStep::Look => self.look_at_media(now),
        }
        self.calls.tick(now);
        self.drive(now);
    }

    // --- the calling socket ---

    fn pushed(&mut self, event: Option<PushEvent>, now: Instant) {
        let Some(event) = event else {
            self.push = None;
            return;
        };
        match event {
            PushEvent::Connected { endpoint, push_url, .. } => {
                log::info!("calling socket connected (endpoint {endpoint})");
                if let Err(error) = self.calls.set_push_url(&push_url) {
                    log::warn!("the calling endpoint's push URL isn't one a call can use: {error}");
                }
            }
            PushEvent::Call(delivery) => {
                let key = DeliveryKey {
                    connection: delivery.delivery.connection.clone(),
                    id: delivery.delivery.id,
                };
                self.responders.insert(key.clone(), delivery.responder());
                self.calls.on_delivery(&delivery.delivery, delivery.observed, now);
                // a delivery is answered (if at all) while it is read: its responder isn't kept past that
                self.drive(now);
                self.responders.remove(&key);
            }
            PushEvent::Ignored(Ignored::IncomingCall) => {
                log::info!("an incoming call the tracker doesn't offer: ignored");
            }
            PushEvent::MessageLoss(loss) => loss.caught_up(),
            PushEvent::Disconnected { down, .. } => log::info!("calling socket down: {down:?}"),
            PushEvent::Stopped(stop) => log::warn!("calling socket stopped: {stop:?}"),
            PushEvent::Registration(report) => self.emit(Event::CallingRegistration(report)),
            PushEvent::Registered(_) | PushEvent::Delivery(_) | PushEvent::Presence(_) | PushEvent::Ignored(_) => {}
        }
    }

    // --- the machine's effects ---

    /// Carries out what the machine asks: one write in flight, everything else at once.
    fn drive(&mut self, now: Instant) {
        loop {
            let effect = if self.writing.is_some() {
                self.calls.poll_beside_write()
            } else {
                self.calls.poll()
            };
            match effect {
                None => return,
                Some(Effect::Send { id, write }) => self.send(id, *write),
                Some(Effect::Respond { delivery, reply }) => self.respond(delivery, reply),
                Some(Effect::Event(event)) => self.told(event, now),
                // an invitation dropped before it rang: its attach failed for good (no further attempt goes), its
                // answer didn't read, its media failed; the tracker ends its offer, which then holds no call open
                Some(Effect::LetGo { attach }) => self.outbox.tracker().let_go(attach.as_str()),
            }
        }
    }

    fn send(&mut self, id: WriteId, write: CallWrite) {
        let name = write.name();
        let mut sink = Sink {
            outbox: Arc::clone(&self.outbox),
            made: None,
        };
        write.submit_to(id, &mut sink);
        if let Some((attempt, again)) = sink.made {
            log::info!("call write {name} ({})", id.get());
            self.writing = Some(Writing {
                id,
                name,
                flight: Flight::Attempt(attempt),
                again,
                retries: CallRetries::default(),
            });
        }
    }

    /// The write in flight's attempt is over: the wait for another one, or its outcome to the machine.
    fn written(&mut self, tried: CallTried, now: Instant) {
        let Some(mut writing) = self.writing.take() else {
            return;
        };
        if let Some(wait) = writing.retries.again(&tried)
            && self.calls.may_send_again(writing.id)
        {
            log::info!(
                "call write {} ({}): {}, once more in {} ms if still wanted",
                writing.name,
                writing.id.get(),
                tried.summary(),
                wait.as_millis()
            );
            writing.flight = Flight::Waiting(Box::pin(tokio::time::sleep(wait)));
            self.writing = Some(writing);
            return;
        }
        log::info!(
            "call write {} ({}): {}",
            writing.name,
            writing.id.get(),
            tried.summary()
        );
        let outcome = match tried {
            CallTried::Answered(answer) => Outcome::Answered {
                status: answer.status(),
                body: answer.body().to_vec(),
            },
            _ => Outcome::Failed,
        };
        self.calls.on_outcome(writing.id, &outcome, now);
    }

    /// The wait before the write in flight's next attempt is over: the attempt goes if the machine still wants the
    /// write (a hang-up during the wait wants no more of it), else the write failed, unsent.
    fn due(&mut self, now: Instant) {
        let Some(mut writing) = self.writing.take() else {
            return;
        };
        if self.calls.may_send_again(writing.id) {
            writing.flight = Flight::Attempt((writing.again)());
            self.writing = Some(writing);
            return;
        }
        log::info!(
            "call write {} ({}): not sent again, its call wants no more of it",
            writing.name,
            writing.id.get()
        );
        self.calls.on_outcome(writing.id, &Outcome::Failed, now);
    }

    fn respond(&mut self, delivery: DeliveryKey, reply: DeliveryReply) {
        let Some(responder) = self.responders.get(&delivery).cloned() else {
            self.calls.on_response_failed(&delivery);
            return;
        };
        let response = match reply {
            DeliveryReply::Receipt(receipt) => CallResponse::from(receipt),
            DeliveryReply::Acknowledgement(acknowledgement) => CallResponse::from(acknowledgement),
        };
        self.responses.spawn(async move {
            let sent = responder.respond(response).await.is_ok();
            (delivery, sent)
        });
    }

    /// What the machine tells: to the account, and to the media.
    fn told(&mut self, event: CallEvent, now: Instant) {
        match event {
            CallEvent::CaptionService { call, .. } => self.service_changed(call),
            CallEvent::CaptionFailed { call } => self.service_join_failed(call),
            CallEvent::Incoming(incoming) => {
                let group = incoming.group_chat.is_some();
                self.offers.insert(incoming.call, (incoming.offer, group));
                self.emit(Event::IncomingCall(IncomingCall {
                    call: incoming.call,
                    caller: incoming.caller,
                    caller_name: incoming.caller_name,
                    group_chat: incoming.group_chat,
                }));
            }
            CallEvent::RingEnded { call, end } => {
                self.cancel_answer(call);
                self.offers.remove(&call);
                self.emit(Event::RingEnded { call, end });
            }
            CallEvent::State { call, state } => self.state(call, state),
            CallEvent::RemoteMedia { call, media } => self.on_media(call, |live| media::apply(live, media), now),
            // the mixer's offer once let in may come before the roster tells it: either way it is a session of its own;
            // its offers after that one (the call moved to another call id, the roster still telling the lobby) are
            // that session's own renegotiations (seen 2026-11-03)
            CallEvent::MediaOffer { call, offer }
                if (self.lobby == Some(call) || self.admitted == Some(call)) && self.replaced != Some(call) =>
            {
                self.rebuild(call, offer);
            }
            CallEvent::MediaOffer { call, offer }
                if self
                    .live
                    .as_ref()
                    .is_some_and(|live| live.call == call && offer.new_mixer_session) =>
            {
                self.rebuild(call, offer);
            }
            CallEvent::MediaOffer { call, offer } => self.on_media(call, |live| media::answer(live, offer), now),
            CallEvent::RenegotiationGaveWay { call } => {
                if let Some(live) = self.live.as_mut().filter(|live| live.call == call) {
                    log::info!("call {}: its renegotiation gave way to the far end's", call.get());
                    live.reoffer_owed = live.camera.is_some();
                }
            }
            CallEvent::Ended { call, end } => self.ended(call, end),
            CallEvent::Voicemail { call } => self.emit(Event::Voicemail { call }),
            CallEvent::Participants { call, participants } => self.participants(call, participants, now),
            CallEvent::InLobby { call, waiting } => self.in_lobby(call, waiting),
            event @ (CallEvent::Lobby { .. } | CallEvent::Own { .. } | CallEvent::Reaction { .. }) => {
                self.roster_told(event);
            }
            CallEvent::Held { call, held } => self.held(call, held),
            CallEvent::GroupChat { call, chat } => {
                log::info!("call {}: in a group chat now", call.get());
                self.emit(Event::CallGroupChat { call, chat });
            }
            CallEvent::AddFailed { call, who } => {
                log::info!("call {}: someone added didn't come in", call.get());
                self.emit(Event::CallAddFailed { call, who });
            }
            CallEvent::TransferFailed { call } => {
                log::info!("call {}: its transfer wasn't taken", call.get());
                self.emit(Event::CallTransferFailed { call });
            }
            CallEvent::Transferred {
                call,
                target,
                transferor,
            } => self.transferred(call, target, transferor),
            CallEvent::HeldByFarEnd { call, held } => {
                log::info!("call {}: held by its far end {held}", call.get());
                self.emit(Event::CallHeldByFarEnd { call, held });
            }
            CallEvent::Meeting { call, state } => {
                self.muted_by_organizer(call, &state);
                self.emit(Event::CallMeeting { call, state });
            }
            CallEvent::KeepAliveDue { call, sent } => {
                log::info!("call {}: its keep-alive fell due (sent: {sent})", call.get());
            }
        }
    }

    /// The account's call on hold or off it, as its Hold or Resume was taken: told, and while held the microphone sends
    /// silence (the far end hears the service's music, never the account), the user's own mute given back once the
    /// call resumes. The call controller moves a held 1:1 call to its mixer and keeps its lines as they are: nothing is
    /// renegotiated (an offer of ours on the mixer's lines was turned away as incompatible, native a ↔ b 2026-11-06).
    fn held(&mut self, call: CallId, held: bool) {
        self.emit(Event::CallHeld { call, held });
        let Some(live) = self.live.as_ref().filter(|live| live.call == call) else {
            self.held_mute = None;
            return;
        };
        match (held, self.held_mute) {
            (true, None) => {
                self.held_mute = Some((call, live.session.is_muted()));
                live.session.set_muted(true);
            }
            (false, Some((of, muted))) if of == call => {
                self.held_mute = None;
                live.session.set_muted(muted);
            }
            _ => {}
        }
        log::info!(
            "call {}: held {held}, its microphone muted {}",
            call.get(),
            live.session.is_muted()
        );
    }

    /// What the call's roster and its messages told, told on as the engine's events: who waits in the lobby, the
    /// account's own state, a live reaction.
    fn roster_told(&self, event: CallEvent) {
        match event {
            CallEvent::Lobby { call, waiting } => self.emit(Event::Lobby {
                call,
                waiting: waiting
                    .into_iter()
                    .map(|person| LobbyPerson {
                        mri: person.mri,
                        name: person.name,
                    })
                    .collect(),
            }),
            CallEvent::Own {
                call,
                hand_raised,
                organizer,
            } => self.emit(Event::CallOwn {
                call,
                hand_raised,
                organizer,
            }),
            CallEvent::Reaction {
                call,
                from,
                name,
                reaction,
            } => self.emit(Event::CallReaction {
                call,
                from,
                name,
                reaction,
            }),
            _ => {}
        }
    }

    fn state(&mut self, call: CallId, state: CallState) {
        let status = match state {
            // told as its setup began
            CallState::Placing if self.told_placing.take() == Some(call) => return,
            CallState::Placing => CallStatus::Placing,
            CallState::Ringing => {
                self.open_devices(call);
                CallStatus::Ringing
            }
            CallState::Joining => {
                self.open_devices(call);
                CallStatus::Joining
            }
            CallState::Answering => CallStatus::Answering,
            CallState::Declining => CallStatus::Declining,
            CallState::Leaving => CallStatus::Leaving,
            // told by its own event, as it rings
            CallState::Offered => return,
            CallState::Connected => {
                self.open_devices(call);
                self.up = Some(call);
                // muted before the call was up (a meeting joined muted, a mute while it was set up): the others see it
                if self
                    .live
                    .as_ref()
                    .is_some_and(|live| live.call == call && live.session.is_muted())
                    && let Err(error) = self.calls.set_muted(call, true)
                {
                    log::info!("call {}: its mute wasn't told: {error}", call.get());
                }
                if self.media_connected(call) {
                    CallStatus::Connected
                } else {
                    CallStatus::Connecting
                }
            }
        };
        self.emit_status(call, status);
    }

    /// `call`'s status told, as waiting in its meeting's lobby while it does: its media or its signaling moving on
    /// there doesn't let it in.
    fn emit_status(&self, call: CallId, status: CallStatus) {
        let waits = self.lobby == Some(call)
            && matches!(
                status,
                CallStatus::Joining | CallStatus::Connecting | CallStatus::Connected | CallStatus::Reconnecting
            );
        let status = if waits { CallStatus::Lobby } else { status };
        self.emit(Event::Call { call, status });
    }

    /// The mixer's first offer to a call let in from its meeting's lobby: the call's media is set up again to answer
    /// it, as an invitation's offer is answered, the lobby's media let go once the new one is in place.
    fn rebuild(&mut self, call: CallId, offer: MediaContent) {
        self.admitted = None;
        self.replaced = Some(call);
        log::info!(
            "call {}: service replaced its media lines, setting up a fresh session",
            call.get()
        );
        let setup = media::prepare(
            Arc::clone(&self.client),
            self.media.clone(),
            Setup {
                offer: Some(offer),
                tenant: media::TenantRead::No,
                video: VideoKind::Group,
            },
        );
        self.rebuilding = Some((call, Box::pin(setup)));
    }

    /// The media set up again for `call` (or not): it replaces the lobby's, its devices open, and its answer goes to
    /// the mixer.
    fn rebuilt(&mut self, call: CallId, result: Result<Prepared, String>, now: Instant) {
        self.rebuilding = None;
        if self.live.as_ref().is_none_or(|live| live.call != call) {
            return;
        }
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(why) => return self.media_failed(call, &why, now),
        };
        let sdp = prepared.sdp.clone();
        let muted = self.live.as_ref().is_some_and(|live| live.session.is_muted());
        let camera_on = self.live.as_ref().is_some_and(|live| live.camera == Some(true));
        // who is in the call stays as the roster told it: the roster tells only changes from here
        let participants = self
            .live
            .as_ref()
            .map(|old| old.gallery.participants().to_vec())
            .unwrap_or_default();
        let mut live = LiveMedia::new(call, prepared);
        live.session.set_muted(muted);
        live.gallery.set_participants(participants);
        live.applied(now);
        let content = if live.is_group() {
            live.answer_content(&sdp)
        } else {
            media::content(&sdp)
        };
        if let Some(old) = self.live.replace(live) {
            let session = old.session;
            tokio::task::spawn_blocking(move || close(call, session));
        }
        self.open_devices(call);
        if let Err(error) = self.calls.answer_media(call, content) {
            self.media_failed(call, &error.to_string(), now);
            return;
        }
        if camera_on && let Some(live) = self.live.as_ref() {
            let (reply, _) = oneshot::channel();
            self.media_steps
                .spawn_blocking(media::group_camera(live, Arc::clone(&self.media.devices), true, reply));
        }
        self.show_participants(now);
    }

    /// The account waits in its meeting's lobby, or no longer does: told as its status, the call's own once let in.
    fn in_lobby(&mut self, call: CallId, waiting: bool) {
        if waiting {
            self.lobby = Some(call);
            self.emit(Event::Call {
                call,
                status: CallStatus::Lobby,
            });
            return;
        }
        if self.lobby.take() != Some(call) || self.up != Some(call) {
            return;
        }
        if self.replaced != Some(call) {
            self.admitted = Some(call);
        }
        let status = if self.media_connected(call) {
            CallStatus::Connected
        } else {
            CallStatus::Connecting
        };
        self.emit(Event::Call { call, status });
    }

    /// The start, the join or the answer of `call` was admitted and taken: its media's microphone and speaker
    /// open now, once, off the engine's thread.
    fn open_devices(&mut self, call: CallId) {
        let Some(live) = self
            .live
            .as_mut()
            .filter(|live| live.call == call && !live.devices_opened)
        else {
            return;
        };
        live.devices_opened = true;
        log::info!("call {}: its devices open", call.get());
        self.media_steps
            .spawn_blocking(media::open_devices(live, Arc::clone(&self.media.devices), None));
    }

    fn media_connected(&self, call: CallId) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.call == call && live.connected)
    }

    /// An SDP step of `call`, off the engine's thread, after the ones asked for before it; the call fails when no
    /// media runs for it.
    fn on_media<F>(&mut self, call: CallId, step: impl FnOnce(&LiveMedia) -> F, now: Instant)
    where
        F: FnOnce() -> MediaDone + Send + 'static,
    {
        match self.live.as_ref().filter(|live| live.call == call) {
            Some(live) => {
                let step = step(live);
                self.sdp_step(step);
            }
            None => self.calls.fail_media(call, now),
        }
    }

    /// Queues an SDP step behind the ones asked for before it.
    fn sdp_step(&mut self, step: impl FnOnce() -> MediaDone + Send + 'static) {
        self.sdp_steps.push_back(Box::new(step));
        self.next_sdp_step();
    }

    /// Starts the next SDP step, unless one runs.
    fn next_sdp_step(&mut self) {
        if self.sdp_running {
            return;
        }
        if let Some(step) = self.sdp_steps.pop_front() {
            self.sdp_running = true;
            self.media_steps.spawn_blocking(step);
        }
    }

    fn ended(&mut self, call: CallId, end: End) {
        self.cancel_answer(call);
        self.held_mute = None;
        self.caption_ended(call);
        self.record_ended(call);
        if self.up == Some(call) {
            self.up = None;
        }
        self.offers.remove(&call);
        if self.live.as_ref().is_some_and(|live| live.call == call) {
            self.close_media();
        }
        self.emit(Event::Call {
            call,
            status: CallStatus::Ended(end),
        });
        self.call_transfer_target(call);
    }

    /// Closes the running media off the engine's thread (its close joins the session's threads).
    fn close_media(&mut self) {
        // what waits for the call's media won't run: its answers go unanswered (the action hears the engine stopped)
        self.sdp_steps.clear();
        if let Some(live) = self.live.take() {
            let (call, session) = (live.call, live.session);
            tokio::task::spawn_blocking(move || close(call, session));
        }
    }

    // --- the media ---

    fn media_done(&mut self, done: MediaDone, now: Instant) {
        if !matches!(
            done,
            MediaDone::Opened { .. }
                | MediaDone::GroupCamera { .. }
                | MediaDone::ScreenPicked { .. }
                | MediaDone::GroupShared { .. }
        ) {
            self.sdp_running = false;
        }
        self.media_step_done(done, now);
        self.next_sdp_step();
        // an SDP step may have changed the receiving lines: what they show follows
        self.show_participants(now);
    }

    /// Who is in `call` now, as its roster told it: the gallery follows, its source requests go.
    fn participants(&mut self, call: CallId, participants: Vec<Participant>, now: Instant) {
        if let Some(live) = self.live.as_mut().filter(|live| live.call == call) {
            live.gallery.set_participants(participants);
            self.show_participants(now);
        }
    }

    /// The running call's receiving lines as its dialect names them now, what they are to show sent to the mixer,
    /// and its view told when it changed.
    fn show_participants(&mut self, now: Instant) {
        let Some(live) = self.live.as_mut() else { return };
        let mut shared = None;
        if live.is_group() {
            let slots = media::slots(live);
            live.gallery.set_slots(slots);
            live.gallery.set_sharing_slot(media::sharing_slot(live));
            let wanted = live.gallery.assign();
            if !wanted.is_empty()
                && let Err(error) = live.session.request_sources(wanted)
            {
                log::warn!("call {}: its source requests didn't go: {error}", live.call.get());
            }
            // someone's screen on the sharing line, as a 1:1 call's far end's renegotiation tells it
            let shown = live.gallery.sharer().is_some();
            if live.shares.theirs != shown {
                live.shares.theirs = shown;
                shared = Some(shown);
            }
        }
        let call = live.call;
        let view = live.gallery.changed(now);
        if let Some(on) = shared {
            log::info!("call {}: a shared screen shown {on}", call.get());
            self.emit(Event::CallRemoteShare { call, on });
        }
        if let Some(view) = view {
            self.emit(Event::CallParticipants { call, view });
        }
    }

    /// A group call's camera turned (or not): its lines described again, its send capabilities given when it went on.
    /// A camera turned on whose lines can't be described (the call stopped being describable while it opened) is shut
    /// again and refused (told off when it was on); one turned off stays off, descriptions that can't go only logged.
    fn group_camera_turned(
        &mut self,
        call: CallId,
        on: bool,
        result: Result<(), String>,
    ) -> Result<(), CallActionError> {
        let Some(live) = self.live.as_mut().filter(|live| live.call == call) else {
            return Err(CallActionError::NoMedia);
        };
        if let Err(why) = result {
            if on && live.camera == Some(true) {
                live.camera = Some(false);
                self.emit(Event::CameraOff { call });
            }
            return Err(CallActionError::Media(why));
        }
        let was = live.camera;
        live.camera = Some(on);
        let described = live.describe();
        let main = media::lock(&live.dialect).main_video_mid().map(str::to_owned);
        let parameters = main
            .filter(|_| on)
            .map(|mid| send_capabilities(mid, live.capabilities + 1));
        match self.calls.describe_media(call, &described, parameters) {
            Ok(()) => {
                live.capabilities += u64::from(on);
                log::info!("call {}: camera {}", call.get(), if on { "on" } else { "off" });
                Ok(())
            }
            Err(error) if on => {
                log::warn!(
                    "call {}: its camera's lines can't be described ({error}): shut again",
                    call.get()
                );
                live.camera = Some(false);
                let session = Arc::clone(&live.session);
                tokio::task::spawn_blocking(move || {
                    if let Err(error) = session.set_camera(None) {
                        log::warn!("the camera didn't shut: {error}");
                    }
                });
                if was == Some(true) {
                    self.emit(Event::CameraOff { call });
                }
                Err(error.into())
            }
            Err(error) => {
                log::warn!("call {}: camera off, its lines not described: {error}", call.get());
                Ok(())
            }
        }
    }

    fn media_step_done(&mut self, done: MediaDone, now: Instant) {
        match done {
            MediaDone::Applied { call, result } => match result {
                Ok(()) => {
                    if let Some(live) = self.live.as_mut().filter(|live| live.call == call) {
                        live.applied(now);
                    }
                }
                Err(why) => self.media_failed(call, &why, now),
            },
            MediaDone::Answered { call, result } => {
                let answered = result.and_then(|answer| {
                    let group = self.live.as_mut().filter(|live| live.call == call && live.is_group());
                    // a 1:1 call's answer names the `Video` modality while a video line is in use, and the screen's
                    // while one is shared; a group call's names the call's own, as the web client's
                    let content = if let Some(live) = group {
                        live.answer_content(&answer)
                    } else {
                        self.calls.set_video(call, answer.carries_video());
                        self.answered_share(call, &answer);
                        media::content(&answer)
                    };
                    self.calls
                        .answer_media(call, content)
                        .map_err(|error| error.to_string())
                });
                match answered {
                    Ok(()) => self.offer_camera_again(call),
                    Err(why) => self.media_failed(call, &why, now),
                }
            }
            MediaDone::Reoffered { call, result, reply } => {
                let renegotiated = match result {
                    Ok(offer) => {
                        let sent = self.calls.renegotiate(call, &media::content(&offer));
                        if sent.is_err() {
                            // the session waits on an offer that won't go
                            self.media_failed(call, "its renegotiation's offer can't go", now);
                        }
                        sent.map_err(CallActionError::from)
                    }
                    Err(why) => {
                        self.calls.give_up_renegotiation(call);
                        Err(CallActionError::Media(why))
                    }
                };
                let _ = reply.send(renegotiated.map(|()| call).map_err(EngineError::from));
            }
            MediaDone::Opened { call, result, unmute } => self.devices_opened(call, result, unmute, now),
            MediaDone::SpeakerOpened { call } => {
                if let Some(live) = self.live.as_mut().filter(|live| live.call == call) {
                    live.speaker_retry = Some(media::SpeakerRetry::Waiting(now + Duration::from_secs(1)));
                }
            }
            MediaDone::Camera {
                call,
                on,
                result,
                reply,
            } => {
                let turned = self.camera_turned(call, on, result, now);
                let _ = reply.send(turned.map(|()| call).map_err(EngineError::from));
            }
            MediaDone::GroupCamera {
                call,
                on,
                result,
                reply,
            } => {
                let turned = self.group_camera_turned(call, on, result);
                let _ = reply.send(turned.map(|()| call).map_err(EngineError::from));
            }
            done @ (MediaDone::ScreenPicked { .. } | MediaDone::GroupShared { .. } | MediaDone::Shared { .. }) => {
                self.share_step_done(done, now);
            }
        }
    }

    fn devices_opened(
        &mut self,
        call: CallId,
        result: Result<(), String>,
        unmute: Option<(u64, Waiting)>,
        now: Instant,
    ) {
        if let Some((generation, reply)) = unmute {
            let result = result.map_err(CallActionError::Media).and_then(|()| {
                if generation == self.mute_generation {
                    self.mute(call, false)
                } else {
                    // not `Ok`: the caller would show the microphone unmuted while capture stays muted
                    Err(CallActionError::Superseded)
                }
            });
            let _ = reply.send(result.map_err(Into::into));
        } else if let Err(why) = result {
            self.media_failed(call, &why, now);
        }
    }

    /// Once the far end's renegotiation that ours gave way to is answered (our camera's line only receiving), our
    /// camera's line offered again as the camera goes, when it is on; tried again after the next answer when another
    /// renegotiation is under way.
    fn offer_camera_again(&mut self, call: CallId) {
        let Some(live) = self.live.as_mut().filter(|live| live.call == call && live.reoffer_owed) else {
            return;
        };
        if live.camera != Some(true) {
            live.reoffer_owed = false;
            return;
        }
        match self.calls.begin_renegotiation(call) {
            Ok(()) => {
                live.reoffer_owed = false;
                // nobody waits for this one's answer
                let (reply, _) = oneshot::channel();
                let step = media::offer_camera(live, true, reply);
                self.sdp_step(step);
            }
            Err(error) => log::info!(
                "call {}: its camera's line waits to be offered again: {error}",
                call.get()
            ),
        }
    }

    /// The camera of `call` turned (or not), with our renegotiation's offer to send (none when our line already went
    /// that way). An offer that can't be made turns the camera off: told when it was on (our camera's line offered
    /// again, [`Self::offer_camera_again`]).
    fn camera_turned(
        &mut self,
        call: CallId,
        on: bool,
        result: Result<Option<teams_media::sdp::NgcSdp>, String>,
        now: Instant,
    ) -> Result<(), CallActionError> {
        let turned = match result {
            Ok(Some(offer)) => {
                self.calls.set_video(call, offer.carries_video());
                let sent = self.calls.renegotiate(call, &media::content(&offer));
                if sent.is_err() {
                    // the session waits on an offer that won't go
                    self.media_failed(call, "its renegotiation's offer can't go", now);
                }
                sent.map_err(CallActionError::from)
            }
            Ok(None) => {
                self.calls.give_up_renegotiation(call);
                Ok(())
            }
            Err(why) => {
                self.calls.give_up_renegotiation(call);
                Err(CallActionError::Media(why))
            }
        };
        if let Some(live) = self.live.as_mut().filter(|live| live.call == call) {
            if turned.is_ok() {
                live.camera = live.camera.map(|_| on);
                log::info!("call {}: camera {}", call.get(), if on { "on" } else { "off" });
            } else if on && live.camera == Some(true) {
                live.camera = Some(false);
                self.emit(Event::CameraOff { call });
            }
        }
        turned
    }

    fn media_failed(&mut self, call: CallId, why: &str, now: Instant) {
        log::warn!("call {}: its media failed: {why}", call.get());
        self.calls.fail_media(call, now);
    }

    /// Playback may disappear independently of capture. Retry it off-thread at most once a second, also picking
    /// up a newly selected device when no old stream remains to follow the directory. Never opens a microphone.
    fn retry_speaker(&mut self, now: Instant) {
        let Some(live) = self.live.as_mut().filter(|live| {
            live.devices_opened
                && live.session.speaker_missing()
                && match live.speaker_retry {
                    Some(media::SpeakerRetry::Opening) => false,
                    Some(media::SpeakerRetry::Waiting(due)) => now >= due,
                    None => true,
                }
        }) else {
            return;
        };
        live.speaker_retry = Some(media::SpeakerRetry::Opening);
        self.media_steps
            .spawn_blocking(media::retry_speaker(live, Arc::clone(&self.media.devices)));
    }

    fn look_at_media(&mut self, now: Instant) {
        self.caption_check(now);
        self.record_check(now);
        let Some(live) = self.live.as_mut() else { return };
        let call = live.call;
        if self
            .rebuilding
            .as_ref()
            .is_some_and(|(replacing, _)| *replacing == call)
        {
            return;
        }
        let levels = live.session.levels();
        live.gallery.levels(levels, now);
        for change in live.poll(now) {
            match change {
                MediaChange::Connected if self.up == Some(call) => self.emit_status(call, CallStatus::Connected),
                MediaChange::Lost if self.up == Some(call) => self.emit_status(call, CallStatus::Reconnecting),
                MediaChange::Connected | MediaChange::Lost => {}
                MediaChange::CameraOff => {
                    log::warn!("call {}: its camera went off", call.get());
                    self.emit(Event::CameraOff { call });
                }
                MediaChange::AudioClosed { microphone } => {
                    if microphone {
                        let _ = self.mute(call, true);
                    } else if let Some(live) = self.live.as_mut() {
                        live.speaker_retry = Some(media::SpeakerRetry::Waiting(now + Duration::from_secs(1)));
                    }
                    self.emit(Event::AudioClosed { call, microphone });
                }
                MediaChange::ScreenEnded => self.screen_ended(call),
                MediaChange::Captions(lines) => self.caption_received(call, lines),
                MediaChange::Speakers(speakers) => {
                    if let Some(live) = self.live.as_mut() {
                        live.gallery.speakers(&speakers);
                    }
                }
                MediaChange::Failed(why) => {
                    self.media_failed(call, &why, now);
                    return;
                }
                MediaChange::Closed => {
                    log::info!(
                        "call {}: the far end closed the media and the call's end didn't come: it left",
                        call.get()
                    );
                    self.calls.media_closed(call, now);
                    return;
                }
            }
        }
        self.retry_speaker(now);
        // who speaks, as the levels and the mixer told it
        if let Some(live) = self.live.as_mut()
            && let Some(view) = live.gallery.changed(now)
        {
            let call = live.call;
            self.emit(Event::CallParticipants { call, view });
        }
    }
}

/// The account's actions.
impl<C, T> Calling<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    /// The meeting's timer and lock changed.
    fn meeting_act(&mut self, act: Act) -> Result<CallId, CallActionError> {
        match act {
            Act::Timer(call, timer) => self
                .calls
                .set_timer(call, &timer)
                .map(|()| call)
                .map_err(CallActionError::from),
            Act::LockMeeting(call, locked) => self
                .calls
                .lock_meeting(call, locked)
                .map(|()| call)
                .map_err(CallActionError::from),
            Act::MuteAll(call) => self.calls.mute_all(call).map(|()| call).map_err(CallActionError::from),
            _ => Err(CallActionError::Call(CallError::NotNow)),
        }
    }

    /// Takes the account's action; its answer goes to `reply`, at once or once its media is set up.
    fn act(&mut self, act: Act, reply: Waiting) {
        let now = Instant::now();
        let answer = match act {
            act @ (Act::Health(..) | Act::Options(..)) => return self.aside(act, reply),
            Act::Captions(..) | Act::CaptionLanguage(..) | Act::Record(..) => self.service_act(act),
            Act::Place(_) | Act::Join(..) | Act::JoinChannel(..) | Act::Answer(_) => return self.set_up(act, reply),
            Act::Decline(call) => self.calls.decline(call).map(|()| call).map_err(CallActionError::from),
            Act::HangUp(call) => self.hang_up(call, now),
            Act::Mute(call, muted) => return self.mute_action(call, muted, reply),
            Act::Lobby(call, waiting, admit) => self
                .calls
                .decide_lobby(call, &waiting, admit)
                .map(|()| call)
                .map_err(CallActionError::from),
            Act::RaiseHand(call, raised) => self
                .calls
                .raise_hand(call, raised)
                .map(|()| call)
                .map_err(CallActionError::from),
            Act::React(call, reaction) => self
                .calls
                .react(call, &reaction)
                .map(|()| call)
                .map_err(CallActionError::from),
            Act::EndMeeting(call) => self
                .calls
                .end_meeting(call)
                .map(|()| call)
                .map_err(CallActionError::from),
            Act::Hold(call, held) => self
                .calls
                .hold(call, held)
                .map(|()| call)
                .map_err(CallActionError::from),
            act @ (Act::Timer(..) | Act::LockMeeting(..) | Act::MuteAll(_)) => self.meeting_act(act),
            Act::Share(call, on) => {
                self.share(call, on, reply);
                return self.drive(now);
            }
            act @ (Act::Add(..) | Act::Transfer(..)) => self.people_act(act),
            Act::Renegotiate(call) => match self.live.as_ref().filter(|live| live.call == call) {
                // the machine first: the session's new offer would replace the one it waits on
                Some(live) => match self.calls.begin_renegotiation(call) {
                    Ok(()) => {
                        let step = media::reoffer(live, reply);
                        self.sdp_step(step);
                        return;
                    }
                    Err(error) => Err(error.into()),
                },
                None => Err(CallActionError::NoMedia),
            },
            Act::Camera(call, on) => match self.live.as_ref().filter(|live| live.call == call) {
                // a group call's camera changes no SDP: its lines are described again once it turned; it opens only
                // while they can be (the call up, nothing renegotiating it), else it stays shut
                Some(live) if live.is_group() && live.camera.is_some() => match self.calls.describable(call) {
                    Err(error) if on => Err(error.into()),
                    _ => {
                        let step = media::group_camera(live, Arc::clone(&self.media.devices), on, reply);
                        self.media_steps.spawn_blocking(step);
                        return;
                    }
                },
                Some(live) if live.camera.is_some() => match self.calls.begin_renegotiation(call) {
                    Ok(()) => {
                        let step = media::camera(live, Arc::clone(&self.media.devices), on, reply);
                        self.sdp_step(step);
                        return;
                    }
                    Err(error) => Err(error.into()),
                },
                Some(_) => Err(CallActionError::NoVideo),
                None => Err(CallActionError::NoMedia),
            },
        };
        let _ = reply.send(answer.map_err(EngineError::from));
        self.drive(now);
    }

    fn cancel_answer(&mut self, call: CallId) {
        if let Some(preparing) = self.preparing.take_if(|preparing| preparing.answering == Some(call)) {
            refuse(preparing.reply, CallError::NotNow.into());
        }
    }

    fn hang_up(&mut self, call: CallId, now: Instant) -> Result<CallId, CallActionError> {
        if let Some(preparing) = self.preparing.take_if(|preparing| preparing.told == Some(call)) {
            // nothing went out yet: the start or join is dropped with its setup, and its call ends here
            log::info!("call {}: hung up while its media was being set up", call.get());
            self.told_placing = None;
            let _ = preparing.reply.send(Ok(call));
            self.ended(call, End::HungUp);
            return Ok(call);
        }
        if let Some(preparing) = self.preparing.take_if(|preparing| preparing.answering == Some(call)) {
            // its answer never went: the invitation still rings, and only its decline ends it
            let _ = preparing.reply.send(Err(EngineError::Call(CallActionError::Media(
                "hung up before the media was set up".to_owned(),
            ))));
            return self.calls.decline(call).map(|()| call).map_err(Into::into);
        }
        self.calls.hang_up(call, now)?;
        Ok(call)
    }

    /// Device reopening may wait on a native backend, so an explicit unmute retries it off this thread.
    fn mute_action(&mut self, call: CallId, muted: bool, reply: Waiting) {
        if self.live.as_ref().is_some_and(|live| live.call == call)
            || self
                .preparing
                .as_ref()
                .is_some_and(|preparing| preparing.told == Some(call))
        {
            self.mute_generation = self.mute_generation.wrapping_add(1);
        }
        let result = if let Some(preparing) = self.preparing.as_mut().filter(|preparing| preparing.told == Some(call)) {
            preparing.muted = muted;
            log::info!("call {}: muted {muted} (once its media is set up)", call.get());
            Ok(call)
        } else {
            if !muted
                && let Some(live) = self
                    .live
                    .as_ref()
                    .filter(|live| live.call == call && live.session.microphone_missing())
            {
                self.media_steps.spawn_blocking(media::open_devices(
                    live,
                    Arc::clone(&self.media.devices),
                    Some((self.mute_generation, reply)),
                ));
                return;
            }
            self.mute(call, muted)
        };
        let _ = reply.send(result.map_err(Into::into));
        self.drive(Instant::now());
    }

    fn mute(&mut self, call: CallId, muted: bool) -> Result<CallId, CallActionError> {
        let live = self
            .live
            .as_ref()
            .filter(|live| live.call == call)
            .ok_or(CallActionError::NoMedia)?;
        if !muted && live.session.microphone_missing() {
            return Err(CallActionError::Media(
                "No microphone is available. Connect one and try again.".into(),
            ));
        }
        self.mute_generation = self.mute_generation.wrapping_add(1);
        match self.held_mute.as_mut().filter(|(of, _)| *of == call) {
            // held: the microphone stays silent, the mute asked given back once the call resumes
            Some((_, wanted)) => *wanted = muted,
            None => live.session.set_muted(muted),
        }
        log::info!("call {}: muted {muted}", call.get());
        // the others see it once the call is up; before then it is told as it connects
        if self.up == Some(call)
            && let Err(error) = self.calls.set_muted(call, muted)
        {
            log::info!("call {}: its mute wasn't told: {error}", call.get());
        }
        Ok(call)
    }

    /// Sets up the media of a start, a join or an answer; the action goes once it is.
    fn set_up(&mut self, act: Act, reply: Waiting) {
        let busy = self.preparing.is_some() || self.live.is_some();
        if let Act::Join(chat, Some(posted)) = &act {
            self.calls.posted_running(chat, posted.clone());
        }
        // a 1:1 call takes its main video line, and so does the test call (its camera turned by a renegotiation, at
        // the link its acceptance hands out, as a 1:1 call's; it hands out no `updateMediaDescriptions` link, so its
        // lines couldn't be described as a group call's are); a group call's (placed, joined, answered) its main line
        // and receiving lines
        let (answering, offer, video) = match &act {
            Act::Answer(call) => {
                let ringing = self.calls.current() == Some((*call, Some(CallState::Offered)));
                match self.offers.get(call) {
                    Some((offer, group)) if ringing && !busy => {
                        let kind = if *group { VideoKind::Group } else { VideoKind::OneToOne };
                        (Some(*call), offer.clone(), kind)
                    }
                    _ => return refuse(reply, CallError::NotNow.into()),
                }
            }
            _ if busy || self.calls.current().is_some() => return refuse(reply, CallActionError::Busy),
            Act::Join(chat, _) if !self.calls.joinable(chat) => return refuse(reply, CallError::NotJoinable.into()),
            Act::Place(placement) => (None, None, placement_video(placement)),
            Act::Join(..) | Act::JoinChannel(..) => (None, None, VideoKind::Group),
            // only a start, a join or an answer sets its media up
            _ => return refuse(reply, CallError::NotNow.into()),
        };
        if answering.is_some() && offer.is_none() {
            return refuse(
                reply,
                CallActionError::Media("the invitation carries no offer".to_owned()),
            );
        }
        // a test call's `add` and a Meet now's entry (its organizer's tenant) name the account's tenant: read from its
        // token when the engine couldn't name it
        // any other call reads it too while it's unknown, without waiting on it: the organizer's Controls name it
        let tenant = if self.calls.knows_tenant() {
            media::TenantRead::No
        } else if matches!(act, Act::Place(Placement::TestCall { .. } | Placement::Channel { .. })) {
            media::TenantRead::Needed
        } else {
            media::TenantRead::Wanted
        };
        let told = self.tell_setup(answering);
        let prepare = media::prepare(
            Arc::clone(&self.client),
            self.media.clone(),
            Setup { offer, tenant, video },
        );
        let setup: Pin<Box<dyn Future<Output = Result<Prepared, String>>>> = match &act {
            // a channel's meeting: where it runs is read first (CSA's `activeMeetups`), then its media set up
            Act::JoinChannel(channel, meetup) => {
                let (client, channel, meetup) = (Arc::clone(&self.client), channel.clone(), Arc::clone(meetup));
                Box::pin(async move {
                    let found = crate::calendar::find_channel_meetup(&client, &channel)
                        .await?
                        .ok_or_else(|| "no meeting runs in the channel".to_owned())?;
                    *meetup.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(found);
                    prepare.await
                })
            }
            _ => Box::pin(prepare),
        };
        let devices = Arc::clone(&self.media.devices);
        let setup = Box::pin(async move {
            devices.ready().await.map_err(|_| CallActionError::NoCalling)?;
            tokio::time::timeout(SETUP_TIMEOUT, setup)
                .await
                .unwrap_or_else(|_| Err(format!("it took over {} s", SETUP_TIMEOUT.as_secs())))
                .map_err(CallActionError::Media)
        });
        self.preparing = Some(Preparing {
            answering,
            told,
            since: Instant::now(),
            muted: false,
            act,
            reply,
            setup,
        });
    }

    fn tell_setup(&mut self, answering: Option<CallId>) -> Option<CallId> {
        let told = answering.is_none().then(|| self.calls.reserve());
        match (told, answering) {
            (Some(call), _) => {
                log::info!("call {}: its media is being set up", call.get());
                let status = if self.media.devices.preparing() {
                    CallStatus::Preparing
                } else {
                    self.told_placing = Some(call);
                    CallStatus::Placing
                };
                self.emit_status(call, status);
            }
            (None, Some(call)) => log::info!("call {}: its media is being set up to answer it", call.get()),
            (None, None) => {}
        }
        told
    }

    /// The media of an action is set up (or not): the action goes now.
    fn prepared(&mut self, prepared: Result<Prepared, CallActionError>, now: Instant) {
        let Some(preparing) = self.preparing.take() else {
            return;
        };
        let call = preparing.told.or(preparing.answering).map_or(0, CallId::get);
        let took = preparing.since.elapsed().as_millis();
        let mut prepared = match prepared {
            Ok(prepared) => prepared,
            Err(why) => {
                log::warn!("call {call}: its media couldn't be set up in {took} ms: {why}");
                self.fail_told(preparing.told);
                return refuse(preparing.reply, why);
            }
        };
        log::info!("call {call}: its media was set up in {took} ms");
        if let Some(tenant) = prepared.tenant.take() {
            self.calls.set_tenant(tenant);
        }
        let sdp = media::first_content(&prepared);
        let acted = match &preparing.act {
            Act::Place(placement) => self.calls.place(placement, sdp, now),
            Act::Join(chat, _) => self.calls.join(chat, sdp, now),
            Act::JoinChannel(channel, meetup) => {
                let meetup = meetup.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
                meetup.map_or(Err(CallError::NotJoinable), |meetup| {
                    self.calls.join_channel_meeting(channel, &meetup, sdp, now)
                })
            }
            Act::Answer(call) => self.calls.answer(*call, sdp).map(|()| *call),
            _ => Err(CallError::NotNow),
        };
        match acted {
            Ok(call) => {
                let mut live = LiveMedia::new(call, prepared);
                if preparing.muted {
                    live.session.set_muted(true);
                }
                if preparing.answering.is_some() {
                    // our answer applied the far end's offer: the media connects from here
                    live.applied(now);
                }
                self.live = Some(live);
                let _ = preparing.reply.send(Ok(call));
            }
            Err(error) => {
                log::warn!("call {call}: refused once its media was set up: {error}");
                tokio::task::spawn_blocking(move || drop(prepared));
                self.fail_told(preparing.told);
                refuse(preparing.reply, error.into());
            }
        }
    }

    /// A start's or a join's call told placing that won't go after all: it ends, failed, where it shows.
    fn fail_told(&mut self, told: Option<CallId>) {
        if let Some(call) = told {
            self.told_placing = None;
            self.ended(call, End::Failed("setup"));
        }
    }
}

/// A meeting uses the mixer's receiving lines even when its invitation has no local chat yet.
fn placement_video(placement: &Placement) -> VideoKind {
    match placement {
        Placement::Chat { group_chat: None, .. } | Placement::TestCall { .. } | Placement::Transferred { .. } => {
            VideoKind::OneToOne
        }
        Placement::Chat { .. } | Placement::Meeting { .. } | Placement::MeetingCode(_) | Placement::Channel { .. } => {
            VideoKind::Group
        }
    }
}

/// The engine stopping.
impl<C, T> Calling<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    /// Hangs the call up, lets an invitation that rings go, sends the last writes within [`SHUTDOWN_GRACE`] (a start or
    /// a join still in flight first gets up to its request's timeout for its answer, so the `leave` that answer makes
    /// possible can go: without it the call would stay running in its chat), closes the media and stops the calling
    /// socket.
    async fn shut_down(mut self) {
        let now = Instant::now();
        if let Some(preparing) = self.preparing.take() {
            let _ = preparing.reply.send(Err(EngineError::Stopped));
        }
        let grace = match self.writing.as_ref().map(|writing| writing.name) {
            Some("start" | "join") => teams_proto::REQUEST_TIMEOUT + SHUTDOWN_GRACE,
            _ => SHUTDOWN_GRACE,
        };
        self.calls.shut_down(now);
        self.drive(now);
        let last_writes = async {
            while self.writing.is_some() {
                match land(self.writing.as_mut()).await {
                    Landed::Tried(tried) => self.written(tried, Instant::now()),
                    Landed::Due => self.due(Instant::now()),
                }
                self.drive(Instant::now());
            }
        };
        if tokio::time::timeout(grace, last_writes).await.is_err() {
            log::warn!("the call's last writes didn't finish within {grace:?}");
        }
        if let Some(live) = self.live.take() {
            let (call, session) = (live.call, live.session);
            let _ = tokio::task::spawn_blocking(move || close(call, session)).await;
        }
        if let Some(push) = self.push.take() {
            push.stop().await;
        }
    }
}

/// Closes a call's media (the close joins the session's threads), its counts logged first: what went out and what came
/// in, never what it carried.
fn close(call: CallId, session: Arc<teams_media::session::MediaSession>) {
    let stats = session.stats();
    let video = stats.video;
    log::info!(
        "call {}: its video closes: {} frames captured, {} sent, {} unsent; {} received, {} shown; {} of a shared screen \
         sent",
        call.get(),
        video.captured,
        video.sent,
        video.unsent,
        video.received,
        video.shown,
        video.screen_sent,
    );
    log::info!(
        "call {}: its media closes: {} audio frames captured, {} sent, {} unsent; {} received, {} malformed; {} \
         played",
        call.get(),
        stats.encoded,
        stats.sent,
        stats.unsent,
        stats.received,
        stats.malformed,
        stats.frames_played,
    );
    drop(session);
}

/// A group call's send capabilities once its camera is on (`applyChannelParameters` on its main video line `mid`, as
/// the web client gives them, numbered `sequence`): the largest picture it sends, one stream (no simulcast).
fn send_capabilities(mid: String, sequence: u64) -> ChannelParameters {
    let caps = serde_json::json!({"maxVideoSendCapabilities": {"caps": {
        "max-width": 1280, "max-height": 1280, "max-fps": 30, "max-streams": 1, "max-layers": 1,
        "sequence-number": sequence,
    }}});
    ChannelParameters {
        mids: vec![mid],
        media_parameter: caps.to_string(),
    }
}

fn refuse(reply: Waiting, error: CallActionError) {
    let _ = reply.send(Err(EngineError::Call(error)));
}

/// The calling socket's next event; never, without one.
async fn next_push(push: Option<&mut TrouterHandle>) -> Option<PushEvent> {
    match push {
        Some(push) => push.next().await,
        None => pending().await,
    }
}

/// What the write in flight comes to next: its attempt over, or its wait for another; never, without one.
async fn land(writing: Option<&mut Writing>) -> Landed {
    match writing.map(|writing| &mut writing.flight) {
        Some(Flight::Attempt(attempt)) => Landed::Tried(attempt.await),
        Some(Flight::Waiting(wait)) => {
            wait.await;
            Landed::Due
        }
        None => pending().await,
    }
}

/// The media set up for an action; never, while none is.
/// The media set up again for a call let in from its lobby, once it is; never, while none is.
async fn rebuilt(rebuilding: Option<&mut Rebuilding>) -> (CallId, Result<Prepared, String>) {
    match rebuilding {
        Some((call, setup)) => (*call, setup.await),
        None => pending().await,
    }
}

async fn prepared(preparing: Option<&mut Preparing>) -> Result<Prepared, CallActionError> {
    match preparing {
        Some(preparing) => (&mut preparing.setup).await,
        None => pending().await,
    }
}

/// The next look at the media; never, before the calling started.
async fn look(look: Option<&mut tokio::time::Interval>) {
    match look {
        Some(look) => {
            look.tick().await;
        }
        None => pending().await,
    }
}

impl From<CallActionError> for EngineError {
    fn from(error: CallActionError) -> Self {
        Self::Call(error)
    }
}
