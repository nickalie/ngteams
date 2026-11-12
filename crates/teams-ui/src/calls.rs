//! The account's calls as the views show them (M3 Task 17): a model the bridge's drain keeps up to date
//! ([`Calls::apply`], from the engine's call events), and the user's asks ([`ask`]), each one call action through the
//! engine ([`Core::call`]), which checks it again (one call at a time, an invitation it offered) and sends its
//! requests through the outbox.
//!
//! - **Shown or not**: a build shows calls only when its models were made with them ([`crate::Models::with_calls`]:
//!   the lab app, and the personal build once its calling opened); models made without (`Models::new`: a run whose
//!   engine has no calling, such as the personal build's when its devices couldn't be opened) show no call, ring for
//!   none and ask for none, whatever the engine tells.
//! - **What is kept**: whether the calling endpoint is registered; the invitation that rings (the engine offered it:
//!   [`IncomingCall`], its caller's name as the invitation gives it); the call up or being set up ([`ActiveCall`]: its
//!   kind, its chat, its status, when its media connected, muted, its camera on, taken by voicemail); the group chats
//!   an invitation
//!   named a running call in (a join goes there: [`Calls::joinable`]); how the last call ended and the last ask that
//!   went nowhere, until the next call.
//! - **A meeting's pre-join** ([`Prejoin`]): the calendar's Join opens it, and nothing goes out until its Join now
//!   ([`join_now`]); what its switches say is the user's, and turns into the call's mute and camera once the call can
//!   take them ([`Calls::next_choice`]).
//! - **The asks** ([`Ask`]): a call from a chat, the test call (the echo service's MRI the app gave), a join, a
//!   meeting's join (started or not), the answer or the decline of the invitation that rings, a hang-up, mute or unmute, the camera on or off (off when a
//!   call starts; a 1:1 call's video, M3 Task 18). Each is the user's: a button, a key, the switcher's command or the
//!   incoming call's notification. Nothing here answers, declines or turns the camera on by itself.
//! - **The video** a build shows ([`Calls::with_video`]: the lab app's or the personal build's, when its calls have
//!   video) is the feed the call's media draws on ([`crate::video::VideoFeed`]).
//!
//! - **A start never goes silently**: the engine tells a start's or a join's call as its media begins to be set up, so
//!   the call window shows it calling at once; one the engine never told within [`START_TOLD_WITHIN`] is given up and
//!   said, as is one whose setup failed (its reason kept with how the call ended).
//!
//! The asks are traced by the caller's trace (`call asked`, `call refused` with the ask and why), never with a name, and
//! logged at `info` with the chat's id (the diagnostic log).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{App, AsyncApp, Context, Entity, WeakEntity};
use teams_core::{
    CallAction, CallActionError, CallId, CallStatus, CallView, End, EngineError, Event, IncomingCall, LobbyPerson,
    MeetingInfo, RegistrationReport,
};
use teams_proto::Mri;

use crate::bridge::Core;
use crate::trace::{TraceLine, ViewTrace};
use crate::video::VideoFeed;

/// What kind of call it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallKind {
    /// A call from a chat (a 1:1 chat's other member, or a group chat's members).
    Chat,
    /// The test call of the echo service.
    TestCall,
    /// A join of a group chat's running call.
    Join,
    /// An invitation the account answered (or declined).
    Incoming,
    /// A meeting's call the account went into from its pre-join, started or not.
    Meeting,
}

impl CallKind {
    /// The trace's name for it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::TestCall => "test-call",
            Self::Join => "join",
            Self::Incoming => "incoming",
            Self::Meeting => "meeting",
        }
    }
}

/// The call up, or being set up, as the views show it.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is the call's own state, told by an event of its own and shown by a control of its own"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveCall {
    pub id: CallId,
    pub kind: CallKind,
    /// The chat it is in: the chat called or joined, a group call's chat; `None` for the test call and a 1:1
    /// invitation.
    pub chat: Option<String>,
    /// Who called, for an invitation.
    pub caller: Option<Mri>,
    /// The caller's name as the invitation gives it.
    pub caller_name: Option<String>,
    pub status: CallStatus,
    /// When its media first connected: the duration counts from there.
    pub connected_at: Option<Instant>,
    pub muted: bool,
    /// The camera is on (off when the call starts; only the user turns it on).
    pub camera: bool,
    /// The callee's voicemail took it (declined, or not answered).
    pub voicemail: bool,
    /// A meeting's title, as its pre-join named it.
    pub title: Option<String>,
    /// Who is in it besides the account, as its roster tells it, whose video each line shows and who speaks
    /// ([`teams_core::Event::CallParticipants`]); empty until told.
    pub view: CallView,
    /// What the call showed of the devices so far: a test call's results once it ended.
    pub check: DeviceCheck,
    /// Who waits in the meeting's lobby, as its roster tells it ([`teams_core::Event::Lobby`]): Teams' "Waiting in
    /// the lobby" with Admit and Deny for each.
    pub lobby: Vec<LobbyPerson>,
    /// The account's hand is raised, as its roster entry tells it ([`teams_core::Event::CallOwn`]).
    pub hand_raised: bool,
    /// The account organizes the meeting: it may end it for everyone.
    pub organizer: bool,
    /// The live reactions sent in the call lately, the newest last, each shown for [`REACTION_SHOWN`].
    pub reactions: Vec<LiveReaction>,
    /// Since when the account holds the call (Teams' Hold), while it does ([`teams_core::Event::CallHeld`]).
    pub held: Option<Instant>,
    /// The far end holds the 1:1 call ([`teams_core::Event::CallHeldByFarEnd`]): Teams' held party sees its avatar
    /// over "On hold", its own controls as they were.
    pub held_by_far_end: bool,
    /// What the meeting shares with everyone in it: its timer, its lock, how to join it, a recording someone runs
    /// ([`teams_core::Event::CallMeeting`]).
    pub meeting: teams_core::MeetingState,
    /// The account shares its screen (Teams' Share; [`teams_core::Event::CallSharing`]).
    pub sharing: bool,
    /// The far end shares its screen, shown on the stage ([`teams_core::Event::CallRemoteShare`]).
    pub remote_share: bool,
    /// Who the account added to the call and isn't in it yet ([`Ask::Add`]), in the order added: People lists them
    /// ringing, or "No response" once they didn't come in ([`teams_core::Event::CallAddFailed`]).
    pub invited: Vec<Invited>,
}

/// Someone the account added to its call, until the roster has them in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invited {
    pub mri: Mri,
    pub name: String,
    /// They didn't come in (Teams' "No response", with its call-again).
    pub no_response: bool,
}

/// A live reaction someone in the call sent ([`teams_core::Event::CallReaction`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveReaction {
    pub from: Mri,
    /// Their name as the reaction gave it.
    pub name: Option<String>,
    /// `like`, `heart`, `applause`, `laugh` or `surprised`.
    pub reaction: String,
    pub at: Instant,
}

/// Whether `event` is one of the engine's call events (what [`Calls::apply`] takes).
fn is_call_event(event: &Event) -> bool {
    matches!(
        event,
        Event::CallingRegistration(_)
            | Event::IncomingCall(_)
            | Event::RingEnded { .. }
            | Event::Call { .. }
            | Event::Voicemail { .. }
            | Event::CameraOff { .. }
            | Event::AudioClosed { .. }
            | Event::CallCaptions { .. }
            | Event::CallRecording { .. }
            | Event::CallParticipants { .. }
            | Event::Lobby { .. }
            | Event::CallOwn { .. }
            | Event::CallHeld { .. }
            | Event::CallHeldByFarEnd { .. }
            | Event::CallGroupChat { .. }
            | Event::CallAddFailed { .. }
            | Event::CallTransferFailed { .. }
            | Event::CallTransferred { .. }
            | Event::CallMeeting { .. }
            | Event::CallSharing { .. }
            | Event::CallRemoteShare { .. }
            | Event::CallReaction { .. }
    )
}

/// How long a live reaction shows on the stage, as Teams floats one up and away.
pub const REACTION_SHOWN: Duration = Duration::from_secs(4);

/// The most live reactions the stage shows at once.
const REACTIONS_SHOWN: usize = 12;

impl ActiveCall {
    /// Its results, when it is the test call.
    fn test_check(&self) -> Option<DeviceCheck> {
        (self.kind == CallKind::TestCall).then_some(self.check)
    }
}

/// What a call showed of the account's devices and its network, as a test call's results tell it once it ended
/// (Teams' "Test call results"): each a sign the call itself gave, never a guess.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a sign of its own the call gave, set by its own event and read by its own check"
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceCheck {
    /// Its media connected: the network carried it.
    pub connected: bool,
    /// The microphone picked up sound ([`CallView::hearing`], or the account speaking).
    pub heard: bool,
    /// The microphone went away during the call ([`teams_core::Event::AudioClosed`]): whatever it picked up before,
    /// it stays muted until an unmute opens it again.
    pub microphone_lost: bool,
    /// The speaker went away during the call ([`teams_core::Event::AudioClosed`]). The engine reopens it about once
    /// a second and playback resumes, but a speaker that dropped out is still a problem the check reports.
    pub speaker_lost: bool,
    /// The far end's audio played on the speaker (a participant speaking: the echo's greeting, the playback).
    pub played: bool,
    /// The camera was turned on.
    pub camera: bool,
    /// The camera stopped by itself while on (its device gone), or the media couldn't turn it on:
    /// it doesn't work.
    pub camera_failed: bool,
    /// The call failed (`End::Failed`: its media lost, a step not taken): whatever worked before, it didn't hold.
    pub failed: bool,
}

/// How one check of a test call came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// It worked.
    Fine,
    /// It showed a problem.
    Problem,
    /// It wasn't tried: the camera never turned on, or the call never connected to check the rest.
    Untried,
}

impl DeviceCheck {
    /// The call's view told: the microphone heard something, the far end was heard.
    fn saw(&mut self, view: &CallView) {
        self.heard |= view.hearing || view.speaking;
        self.played |= view.participants.iter().any(|participant| participant.speaking);
    }

    /// The microphone's verdict and its words: nothing checked unless the call connected.
    #[must_use]
    pub fn microphone(self) -> (Verdict, &'static str) {
        match (self.connected, self.microphone_lost, self.heard) {
            (false, _, _) => (Verdict::Untried, "Not checked"),
            (true, true, _) => (Verdict::Problem, "Stopped during the call"),
            (true, false, true) => (Verdict::Fine, "Picked up sound"),
            (true, false, false) => (Verdict::Problem, "Picked up no sound"),
        }
    }

    /// The speaker's: the echo's voice played, and the speaker didn't go away afterwards.
    #[must_use]
    pub fn speaker(self) -> (Verdict, &'static str) {
        match (self.connected, self.speaker_lost, self.played) {
            (false, _, _) => (Verdict::Untried, "Not checked"),
            (true, true, _) => (Verdict::Problem, "Stopped during the call"),
            (true, false, true) => (Verdict::Fine, "Played the call"),
            (true, false, false) => (Verdict::Problem, "Played nothing"),
        }
    }

    /// The camera's: only one turned on is checked, and one that stopped by itself failed.
    #[must_use]
    pub fn camera(self) -> (Verdict, &'static str) {
        if self.camera_failed {
            (Verdict::Problem, "Didn't work")
        } else if self.camera {
            (Verdict::Fine, "Turned on")
        } else {
            (Verdict::Untried, "Not turned on")
        }
    }

    /// The network's: the call's media connected, and the call didn't fail afterwards.
    #[must_use]
    pub fn network(self) -> (Verdict, &'static str) {
        match (self.connected, self.failed) {
            (false, _) => (Verdict::Problem, "Couldn't connect"),
            (true, true) => (Verdict::Problem, "The call dropped"),
            (true, false) => (Verdict::Fine, "Connected"),
        }
    }

    /// Whether no check showed a problem (a camera not turned on isn't one).
    #[must_use]
    pub fn passed(self) -> bool {
        [self.microphone(), self.speaker(), self.camera(), self.network()]
            .iter()
            .all(|(verdict, _)| *verdict != Verdict::Problem)
    }
}

/// How the last call ended, shown until the next one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
    pub id: CallId,
    pub kind: CallKind,
    pub chat: Option<String>,
    /// Who called, for an invitation answered.
    pub caller: Option<Mri>,
    /// The caller's name as the invitation gave it.
    pub caller_name: Option<String>,
    pub end: End,
    pub voicemail: bool,
    /// How long it was connected (from its media's connect to its end); `None` when it never connected.
    pub duration: Option<Duration>,
    /// A test call's results: what it showed of the devices and the network ([`DeviceCheck`]); `None` for any other
    /// call.
    pub check: Option<DeviceCheck>,
}

impl Ended {
    /// How `active` ended with `end`, now.
    fn of(active: ActiveCall, end: End) -> Self {
        let check = active.test_check();
        Self {
            id: active.id,
            kind: active.kind,
            chat: active.chat,
            caller: active.caller,
            caller_name: active.caller_name,
            end,
            voicemail: active.voicemail,
            duration: active.connected_at.map(|at| at.elapsed()),
            check,
        }
    }

    /// Why it ended, in the user's terms; `me` is the account's MRI.
    #[must_use]
    pub fn reason(&self, me: &str) -> EndReason {
        let remote = match &self.end {
            End::HungUp | End::Declined => return EndReason::Own,
            End::Failed(_) if self.duration.is_some() => return EndReason::Dropped,
            End::Failed(_) => return EndReason::NotConnected,
            End::Remote(remote) => remote,
        };
        if self.voicemail {
            EndReason::Voicemail
        } else if remote.chat_has_running_call() {
            EndReason::ChatHasCall
        // the conversation's own ends come before who sent them: a real one names the last to leave as its sender
        } else if remote.sub_code == Some(NOBODY_JOINED) {
            EndReason::NobodyJoined
        } else if remote.sub_code == Some(EVERYONE_LEFT) {
            EndReason::EveryoneLeft
        } else if let Some(by) = &remote.by {
            if by.as_str().eq_ignore_ascii_case(me) {
                EndReason::EndedElsewhere
            } else {
                EndReason::EndedBy(remote.by_name.clone())
            }
        } else if remote.failed() && self.duration.is_some() {
            EndReason::Dropped
        } else if remote.failed() {
            EndReason::NotConnected
        } else {
            EndReason::Ended
        }
    }

    /// Whether its window stays open on how it ended: a test call's results, and any end the user didn't choose
    /// (the far end hung up, the call dropped or never connected); a hang-up or a decline of the user's closes it.
    #[must_use]
    pub fn stays_in_window(&self) -> bool {
        self.check.is_some() || !matches!(self.end, End::HungUp | End::Declined)
    }

    /// The group chat it was in, if it was a group call (a 1:1 chat's id names its two members).
    #[must_use]
    pub fn group_chat(&self) -> Option<&str> {
        self.chat
            .as_deref()
            .filter(|chat| teams_proto::one_to_one_pair(chat).is_none())
    }

    /// The chat a call back goes to: the call's chat, or for a 1:1 invitation the chat of the account (`me`) and its
    /// caller.
    #[must_use]
    pub fn call_back_chat(&self, me: &str) -> Option<String> {
        self.chat.clone().or_else(|| {
            let caller = self.caller.as_ref()?;
            teams_proto::one_to_one_id(me, caller.as_str())
        })
    }
}

/// A conversation's end's sub-code when nobody else joined a group call ("…as no one else has joined the group
/// call.", seen 2026-10-30).
const NOBODY_JOINED: i64 = 5013;
/// A conversation's end's sub-code when everyone left the call ("…as all participants left the audio-video
/// modality.", seen 2026-10-30).
const EVERYONE_LEFT: i64 = 5002;

/// Why a call ended, as the user is told it: never a code the service sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndReason {
    /// The user hung up or declined here.
    Own,
    /// Someone else in the call ended it (a 1:1 call's far end hung up), named as the end names them.
    EndedBy(Option<String>),
    /// The account ended it on another of its devices.
    EndedElsewhere,
    /// The callee's voicemail took it (declined there, or not answered), and the call ended there.
    Voicemail,
    /// Nobody else joined the group call.
    NobodyJoined,
    /// Everyone else left the call.
    EveryoneLeft,
    /// The chat already has a call running: the start was turned away.
    ChatHasCall,
    /// It connected, then dropped (its media lost, a step not taken, the service gave it up).
    Dropped,
    /// It never connected.
    NotConnected,
    /// It ended, and the end says nothing more.
    Ended,
}

impl EndReason {
    /// The trace's name for it.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::EndedBy(_) => "ended-by-other",
            Self::EndedElsewhere => "ended-elsewhere",
            Self::Voicemail => "voicemail",
            Self::NobodyJoined => "nobody-joined",
            Self::EveryoneLeft => "everyone-left",
            Self::ChatHasCall => "chat-has-call",
            Self::Dropped => "dropped",
            Self::NotConnected => "not-connected",
            Self::Ended => "ended",
        }
    }

    /// The heading the window shows.
    #[must_use]
    pub fn heading(&self) -> &'static str {
        match self {
            Self::NotConnected | Self::ChatHasCall => "Couldn't connect the call",
            Self::Dropped => "Call dropped",
            _ => "Call ended",
        }
    }

    /// What it says of why, `other` naming the one the call was with when the end names nobody; nothing when there is
    /// nothing more to say than its heading.
    #[must_use]
    pub fn text(&self, other: &str) -> Option<String> {
        Some(match self {
            Self::Own | Self::Ended => return None,
            Self::EndedBy(name) => format!("{} ended the call", name.as_deref().unwrap_or(other)),
            Self::EndedElsewhere => "You left the call on another device".to_owned(),
            Self::Voicemail => format!("{other} didn't pick up: the call went to voicemail"),
            Self::NobodyJoined => "No one else joined the call".to_owned(),
            Self::EveryoneLeft => "Everyone else left the call".to_owned(),
            Self::ChatHasCall => "A call is already going on in this chat".to_owned(),
            Self::Dropped => "The connection was lost".to_owned(),
            Self::NotConnected => "Check your network and try again".to_owned(),
        })
    }
}

/// What the user asks of the calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Show or hide ephemeral live captions for this viewer.
    ToggleCaptions,
    Captions(bool),
    /// Start (or stop) the call's live captions for its saved transcript alone, their pane left as the user has it
    /// ([`crate::call_transcript`]): asked by the app, not the user.
    RecordCaptions(bool),
    /// Shared meeting spoken language, confirmed in the caption settings.
    CaptionLanguage(String),
    /// Call the chat: its other member, or a group chat's members.
    Call {
        chat: String,
    },
    /// The test call of the echo service.
    TestCall,
    /// Join the running call of the group chat.
    Join {
        chat: String,
    },
    /// Go into the call of the meeting whose chat is `chat`: it starts the call when nobody has, and joins it when it
    /// runs.
    JoinMeeting {
        chat: String,
        meeting: MeetingInfo,
    },
    /// Join by invitation ID after the user confirms Join now.
    JoinMeetingCode(teams_core::MeetingCode),
    /// Start a new meeting in the channel `channel` (its header's Meet now): the account organizes it.
    MeetNow {
        channel: String,
    },
    /// Answer the invitation that rings.
    Answer,
    /// Decline the invitation that rings.
    Decline,
    /// Hang up the call up (or cancel it while it rings at its callees).
    HangUp,
    /// Mute the microphone, or unmute it.
    ToggleMute,
    /// Turn the camera on, or off.
    ToggleCamera,
    /// Let someone waiting in the meeting's lobby in (`admit`), or turn them away.
    Lobby {
        waiting: Mri,
        admit: bool,
    },
    /// Raise the account's hand, or lower it.
    ToggleHand,
    /// Send a live reaction to everyone in the call (`like`, `heart`, `applause`, `laugh`, `surprised`).
    React(String),
    /// End the meeting for everyone in it (its organizer's End meeting).
    EndMeeting,
    /// Put the 1:1 call on hold, or take it off hold (Teams' Hold and Resume).
    ToggleHold,
    /// Set the meeting's timer (Teams' Timer: Start, Pause, Play, Reset, Cancel timer).
    Timer(teams_proto::MeetingTimer),
    /// Lock the meeting (`true`) or unlock it (its organizer's Lock the meeting).
    LockMeeting(bool),
    /// Share the screen (the desktop asks which), or stop sharing it (Teams' Share and Stop sharing).
    ToggleShare,
    /// Add `invitee` (named `name`) to the call, as People's "Invite someone" does, or call them again
    /// once they didn't answer.
    Add {
        invitee: Mri,
        name: String,
    },
    /// Transfer the 1:1 call's far end to `target` (Teams' Transfer to an audio call).
    Transfer {
        target: Mri,
    },
    /// Mute everyone else in the call (its organizer's Mute all).
    MuteAll,
    /// Change one of the call's meeting options (its organizer's Controls).
    MeetingOption(teams_proto::MeetingOption),
    /// Start recording, with its transcript unless one runs (More → Record and transcribe → Start recording,
    /// confirmed): the call's `title` as its recording is named, a meeting's `organizer`, the spoken `language`
    /// picked, and whether it records the audio alone.
    Record {
        title: String,
        organizer: Option<String>,
        language: String,
        audio_only: bool,
    },
    /// Stop recording, with the transcription while it runs (Stop recording, confirmed).
    StopRecording,
    /// Start the transcription alone (Start transcription, its language confirmed), named as a recording is.
    Transcribe {
        title: String,
        organizer: Option<String>,
        language: String,
    },
    /// Stop the transcription alone (Stop transcription, confirmed).
    StopTranscription,
}

impl Ask {
    /// The trace's name for it.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::ToggleCaptions | Self::Captions(_) => "captions",
            Self::RecordCaptions(_) => "record-captions",
            Self::CaptionLanguage(_) => "caption-language",
            Self::Call { .. } => "call",
            Self::TestCall => "test-call",
            Self::Join { .. } => "join",
            Self::JoinMeeting { .. } | Self::JoinMeetingCode(_) => "join-meeting",
            Self::MeetNow { .. } => "meet-now",
            Self::Answer => "answer",
            Self::Decline => "decline",
            Self::HangUp => "hang-up",
            Self::ToggleMute => "mute",
            Self::ToggleCamera => "camera",
            Self::Lobby { admit: true, .. } => "admit",
            Self::Lobby { admit: false, .. } => "deny",
            Self::ToggleHand => "hand",
            Self::React(_) => "react",
            Self::EndMeeting => "end-meeting",
            Self::ToggleHold => "hold",
            Self::Timer(_) => "timer",
            Self::LockMeeting(_) => "lock-meeting",
            Self::MuteAll => "mute-all",
            Self::MeetingOption(_) => "meeting-option",
            Self::ToggleShare => "share",
            Self::Add { .. } => "add",
            Self::Transfer { .. } => "transfer",
            Self::Record { .. } => "record",
            Self::StopRecording => "stop-recording",
            Self::Transcribe { .. } => "transcribe",
            Self::StopTranscription => "stop-transcription",
        }
    }

    fn chat(&self) -> Option<&str> {
        match self {
            Self::Call { chat }
            | Self::Join { chat }
            | Self::JoinMeeting { chat, .. }
            | Self::MeetNow { channel: chat } => Some(chat),
            _ => None,
        }
    }

    /// The kind and chat of the call it starts, for a start or a join.
    fn starts(&self) -> Option<(CallKind, Option<String>)> {
        match self {
            Self::Call { chat } => Some((CallKind::Chat, Some(chat.clone()))),
            Self::TestCall => Some((CallKind::TestCall, None)),
            Self::JoinMeetingCode(_) => Some((CallKind::Meeting, None)),
            Self::Join { chat } => Some((CallKind::Join, Some(chat.clone()))),
            Self::JoinMeeting { chat, .. } | Self::MeetNow { channel: chat } => {
                Some((CallKind::Meeting, Some(chat.clone())))
            }
            _ => None,
        }
    }
}

/// A meeting a user can go into, as its calendar event names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeetingToJoin {
    /// A calendar or chat meeting with known organizer and tenant.
    Chat {
        chat: String,
        meeting: MeetingInfo,
        title: String,
    },
    /// An invitation resolved by the calling service on Join now.
    Code(teams_core::MeetingCode),
}

impl MeetingToJoin {
    /// The known chat, if the invitation has already been resolved.
    #[must_use]
    pub fn chat(&self) -> Option<&str> {
        match self {
            Self::Chat { chat, .. } => Some(chat),
            Self::Code(_) => None,
        }
    }

    /// The organizer and tenant, if already known.
    #[must_use]
    pub fn meeting(&self) -> Option<&MeetingInfo> {
        match self {
            Self::Chat { meeting, .. } => Some(meeting),
            Self::Code(_) => None,
        }
    }

    /// The heading without exposing the invitation's code or passcode.
    #[must_use]
    pub fn title(&self) -> &str {
        match self {
            Self::Chat { title, .. } => title,
            Self::Code(_) => "Meeting",
        }
    }

    fn ask(&self) -> Ask {
        match self {
            Self::Chat { chat, meeting, .. } => Ask::JoinMeeting {
                chat: chat.clone(),
                meeting: meeting.clone(),
            },
            Self::Code(code) => Ask::JoinMeetingCode(code.clone()),
        }
    }
}

/// A meeting's pre-join while it shows: what it goes into, and what its switches say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prejoin {
    pub to_join: MeetingToJoin,
    /// The microphone's switch is off.
    pub muted: bool,
    /// The camera's switch is on.
    pub camera: bool,
    /// The audio picked: the computer's, or "Don't use audio" (the call is joined muted, as Teams joins it).
    pub audio: PrejoinAudio,
    /// Join now was asked, and its call isn't told yet.
    pub joining: bool,
    /// What Join now does: go into the meeting, or start a new one in the channel `to_join.chat` (its Meet now).
    pub entry: PrejoinEntry,
}

/// What a pre-join's Join now does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PrejoinEntry {
    /// Goes into the meeting: starts its call when nobody has, joins it when it runs.
    #[default]
    Join,
    /// Starts a new meeting in the channel (its header's Meet now).
    MeetNow,
}

/// The audio a pre-join picks, as Teams lists it (Phone audio is offered greyed out, Room audio not at all).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PrejoinAudio {
    /// Computer audio: the microphone and the speaker.
    #[default]
    Computer,
    /// Don't use audio: the call is joined muted.
    None,
}

/// What a pre-join's switches said at Join now, until the call took it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Choices {
    muted: bool,
    camera: bool,
}

/// An ask that went nowhere: which, why (a name for the trace) and what to tell the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub ask: &'static str,
    pub why: &'static str,
    pub text: String,
}

/// The caption pane as the user shows it, and whether the captions are saved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CaptionPane {
    /// The user shows the pane (Show live captions), until the call ends or they hide it.
    shown: bool,
    /// The calls' captions are saved ("Save live captions of every call", [`crate::call_transcript`]): they keep
    /// running while their pane is hidden.
    saving: bool,
}

/// The calls' model (see the module docs). A GPUI entity in [`crate::Models`].
#[derive(Debug, Default)]
pub struct Calls {
    shown: bool,
    captions: teams_core::CallCaptions,
    /// The caption pane as the user shows it, and whether the captions are saved.
    pane: CaptionPane,
    /// The account's own start or stop of a recording in its call (empty once the call ends).
    recording: teams_core::CallRecording,
    echo: Option<Mri>,
    registered: bool,
    ringing: Option<IncomingCall>,
    /// The invitation that last stopped ringing without being answered here, and who ended it.
    rang_out: Option<(CallId, Option<Mri>)>,
    call: Option<ActiveCall>,
    /// The kind and chat of a start or a join asked for, until its call is told (the engine tells every call it
    /// started, at the latest as it ends), it is refused or the engine stops.
    asked: Option<(CallKind, Option<String>)>,
    /// How many starts or joins were asked for: which one [`START_TOLD_WITHIN`] gives up on.
    starts: u64,
    /// The starts or joins (their numbers) given up on untold and not answered yet: the engine's late refusal of one
    /// is not told again.
    gave_up: BTreeSet<u64>,
    running: BTreeSet<String>,
    ended: Option<Ended>,
    refused: Option<Refused>,
    video: Option<VideoFeed>,
    /// The meeting's pre-join, while it shows.
    prejoin: Option<Prejoin>,
    /// The meeting last gone into: its call's title, and what a Rejoin goes into.
    meeting: Option<MeetingToJoin>,
    /// The call that actually adopted that pre-join, so Rejoin cannot reuse a different meeting.
    meeting_call: Option<CallId>,
    /// What the pre-join chose, until its call took it.
    choices: Option<Choices>,
    /// The pre-join was cancelled after its Join now went: the meeting's call that comes of it is hung up as soon as
    /// it is told ([`Self::next_choice`]), and none of its choices is applied.
    taken_back: bool,
}

impl Calls {
    /// The calls of a build that shows them, its test call to `echo` (the echo service's MRI, when known).
    #[must_use]
    pub fn shown(echo: Option<Mri>) -> Self {
        Self {
            shown: true,
            echo,
            ..Self::default()
        }
    }

    /// The same calls, showing a 1:1 call's video on `feed` (the feed the engine's call media draws on).
    #[must_use]
    pub fn with_video(mut self, feed: VideoFeed) -> Self {
        self.video = Some(feed);
        self
    }

    /// The video feed, in a build that shows video.
    #[must_use]
    pub fn video(&self) -> Option<&VideoFeed> {
        self.video.as_ref().filter(|_| self.shown)
    }

    /// Current viewer's ephemeral captions (always empty once the call ends).
    #[must_use]
    pub fn captions(&self) -> &teams_core::CallCaptions {
        &self.captions
    }

    /// Whether the caption pane shows: the captions aren't off and, while the calls' captions are saved, the user showed
    /// it (captions that run for the saved transcript alone show nothing).
    #[must_use]
    pub fn captions_shown(&self) -> bool {
        self.captions.phase != teams_core::CaptionPhase::Off && (self.pane.shown || !self.pane.saving)
    }

    /// Whether the user showed the caption pane (Show live captions) in the call up.
    #[must_use]
    pub fn caption_pane_asked(&self) -> bool {
        self.pane.shown
    }

    /// Whether the calls' captions are saved ([`Self::set_saving_captions`]).
    #[must_use]
    pub fn saving_captions(&self) -> bool {
        self.pane.saving
    }

    /// The calls' captions are saved (`on`), as the settings say: hiding their pane leaves them running.
    pub fn set_saving_captions(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.pane.saving != on {
            self.pane.saving = on;
            cx.notify();
        }
    }

    /// What a caption ask leaves the engine to do once the pane is shown or hidden as it asks: the captions turned on
    /// or off, or nothing (`None`) when they already run for the saved transcript (shown) or keep running for it
    /// (hidden). Any other ask is left as it is.
    fn caption_pane_ask(&mut self, ask: Ask, cx: &mut Context<Self>) -> Option<Ask> {
        let show = match ask {
            Ask::ToggleCaptions => !self.captions_shown(),
            Ask::Captions(show) => show,
            _ => return Some(ask),
        };
        if self.call.is_none() {
            return Some(Ask::Captions(show));
        }
        if self.pane.shown != show {
            self.pane.shown = show;
            cx.notify();
        }
        let running = matches!(
            self.captions.phase,
            teams_core::CaptionPhase::On | teams_core::CaptionPhase::Starting
        );
        let local = running && (show || self.pane.saving);
        (!local).then_some(Ask::Captions(show))
    }

    /// The account's own start or stop of a recording in its call, and why the last one failed.
    #[must_use]
    pub fn recording(&self) -> &teams_core::CallRecording {
        &self.recording
    }

    /// Whether this build shows calls at all.
    #[must_use]
    pub fn shows(&self) -> bool {
        self.shown
    }

    /// Whether the calling endpoint is registered (invitations come only to a registered one).
    #[must_use]
    pub fn registered(&self) -> bool {
        self.registered
    }

    /// The invitation that rings, if one does.
    #[must_use]
    pub fn ringing(&self) -> Option<&IncomingCall> {
        self.ringing.as_ref()
    }

    /// The invitation `me` last missed, if one: it stopped ringing and `me` didn't end it from another device.
    #[must_use]
    pub fn missed(&self, me: &str) -> Option<CallId> {
        self.rang_out
            .as_ref()
            .filter(|(_, by)| !by.as_ref().is_some_and(|by| by.as_str().eq_ignore_ascii_case(me)))
            .map(|(call, _)| *call)
    }

    /// The call up, or being set up.
    #[must_use]
    pub fn call(&self) -> Option<&ActiveCall> {
        self.call.as_ref()
    }

    /// How the last call ended, until the next one.
    #[must_use]
    pub fn ended(&self) -> Option<&Ended> {
        self.ended.as_ref()
    }

    /// The last ask that went nowhere, until the next call.
    #[must_use]
    pub fn refused(&self) -> Option<&Refused> {
        self.refused.as_ref()
    }

    /// Whether the test call can be asked for: the echo service's MRI is known.
    #[must_use]
    pub fn has_test_call(&self) -> bool {
        self.echo.is_some()
    }

    /// The echo service the test call goes to, when known: the test call's other participant.
    #[must_use]
    pub fn echo(&self) -> Option<&Mri> {
        self.echo.as_ref()
    }

    /// Whether a call can start now: calls shown, none up, none ringing, no start or join asked for and not yet told.
    #[must_use]
    pub fn idle(&self) -> bool {
        self.shown && !self.busy()
    }

    /// Whether a call is up, rings, or a start or join was asked for and its call not yet told (its media being set
    /// up): another start waits for it to settle, and so do the other accounts' ([`crate::accounts`]).
    #[must_use]
    pub fn busy(&self) -> bool {
        self.call.is_some() || self.ringing.is_some() || self.asked.is_some()
    }

    /// The engine stopped (shut down, or the account signed out): no call event comes any more, so the invitation that
    /// rang stops ringing, the call up is shown ended (the engine hangs it up as it stops) and nothing is asked.
    pub fn stopped(&mut self, cx: &mut Context<Self>) {
        if !self.shown {
            return;
        }
        self.registered = false;
        self.ringing = None;
        self.asked = None;
        self.prejoin = None;
        self.choices = None;
        self.taken_back = false;
        if let Some(active) = self.call.take() {
            self.ended = Some(Ended::of(active, End::HungUp));
            self.refused = None;
        }
        cx.notify();
    }

    /// Whether an invitation named a running call in the group chat `chat`, so a join can go there.
    #[must_use]
    pub fn joinable(&self, chat: &str) -> bool {
        self.running.contains(chat)
    }

    /// The meeting's pre-join, while it shows.
    #[must_use]
    pub fn prejoin(&self) -> Option<&Prejoin> {
        self.prejoin.as_ref()
    }

    /// The meeting last gone into (its chat, organizer and tenant, its title), as its pre-join named it.
    #[must_use]
    pub fn meeting_to_join(&self) -> Option<&MeetingToJoin> {
        self.meeting.as_ref()
    }

    /// Shows the pre-join of `to_join`: its camera off, its microphone on, nothing sent.
    pub fn open_prejoin(&mut self, to_join: MeetingToJoin, cx: &mut Context<Self>) {
        if !self.shown {
            return;
        }
        self.refused = None;
        self.ended = None;
        self.taken_back = false;
        self.prejoin = Some(Prejoin {
            to_join,
            muted: false,
            camera: false,
            audio: PrejoinAudio::Computer,
            joining: false,
            entry: PrejoinEntry::Join,
        });
        cx.notify();
    }

    /// Shows the pre-join of a new meeting in the channel `to_join.chat` (its Meet now), as [`Self::open_prejoin`]
    /// does; its Join now starts the meeting.
    pub fn open_meet_now(&mut self, to_join: MeetingToJoin, cx: &mut Context<Self>) {
        self.open_prejoin(to_join, cx);
        if let Some(prejoin) = &mut self.prejoin {
            prejoin.entry = PrejoinEntry::MeetNow;
        }
    }

    /// The subject a Meet now's pre-join gives its new meeting (Teams' `Meeting in "<channel>"` until edited): the
    /// meeting's post is titled so once the service posted it.
    pub fn set_prejoin_title(&mut self, title: String, cx: &mut Context<Self>) {
        if let Some(prejoin) = &mut self.prejoin
            && prejoin.entry == PrejoinEntry::MeetNow
            && let MeetingToJoin::Chat { title: current, .. } = &mut prejoin.to_join
            && *current != title
        {
            *current = title;
            cx.notify();
        }
    }

    /// The pre-join's microphone switch.
    pub fn set_prejoin_muted(&mut self, muted: bool, cx: &mut Context<Self>) {
        if let Some(prejoin) = &mut self.prejoin {
            prejoin.muted = muted;
            cx.notify();
        }
    }

    /// The pre-join's audio: the computer's (`false`), or none ("Don't use audio").
    pub fn set_prejoin_no_audio(&mut self, no_audio: bool, cx: &mut Context<Self>) {
        if let Some(prejoin) = &mut self.prejoin {
            prejoin.audio = if no_audio {
                PrejoinAudio::None
            } else {
                PrejoinAudio::Computer
            };
            cx.notify();
        }
    }

    /// The pre-join's camera switch.
    pub fn set_prejoin_camera(&mut self, camera: bool, cx: &mut Context<Self>) {
        if let Some(prejoin) = &mut self.prejoin {
            prejoin.camera = camera;
            cx.notify();
        }
    }

    /// The pre-join is closed without joining: before its Join now nothing went out, and nothing will; after it, the
    /// call its Join now asked for is hung up once it is told, its choices (the camera) never applied.
    pub fn cancel_prejoin(&mut self, cx: &mut Context<Self>) {
        if let Some(prejoin) = self.prejoin.take() {
            if prejoin.joining {
                self.choices = None;
                self.taken_back = true;
            }
            self.refused = None;
            cx.notify();
        }
    }

    /// What goes into the meeting a call that ended was in once more, as its Rejoin.
    #[must_use]
    pub fn rejoin_of(&self, ended: &Ended) -> Option<Ask> {
        let to_join = self.meeting.as_ref().filter(|_| self.meeting_call == Some(ended.id))?;
        Some(to_join.ask())
    }

    /// The next of the pre-join's choices its call can take now: the microphone muted once the call's media is set up,
    /// the camera once the call is up. `None` when none is due, and each is asked once.
    pub fn next_choice(&mut self, cx: &mut Context<Self>) -> Option<Ask> {
        if self.taken_back
            && self.call.as_ref().is_some_and(|active| {
                active.kind == CallKind::Meeting && !matches!(active.status, CallStatus::Ended(_))
            })
        {
            // the pre-join's Cancel came after its Join now: the meeting's call goes as soon as it can be hung up
            self.taken_back = false;
            cx.notify();
            return Some(Ask::HangUp);
        }
        let choices = self.choices.as_mut()?;
        let active = self.call.as_ref().filter(|active| active.kind == CallKind::Meeting)?;
        let connected = active.status == CallStatus::Connected;
        let ask = if choices.muted && active.status != CallStatus::Placing {
            choices.muted = false;
            Some(Ask::ToggleMute)
        } else if choices.camera && connected {
            choices.camera = false;
            Some(Ask::ToggleCamera)
        } else {
            None
        };
        if *choices == Choices::default() {
            self.choices = None;
        }
        if ask.is_some() {
            cx.notify();
        }
        ask
    }

    /// Clears how the last call ended and the last refusal (the user dismissed them).
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.ended.take().is_some() | self.refused.take().is_some() {
            cx.notify();
        }
    }

    /// Applies one of the engine's call events; nothing while calls aren't shown. Whether it was one.
    pub fn apply(&mut self, event: &Event, cx: &mut Context<Self>) -> bool {
        if !self.shown {
            return is_call_event(event);
        }
        match event {
            Event::CallingRegistration(report) => {
                self.registered = matches!(report, RegistrationReport::Registered);
            }
            Event::IncomingCall(incoming) => {
                if let Some(chat) = &incoming.group_chat {
                    self.running.insert(chat.clone());
                }
                self.ringing = Some(incoming.clone());
            }
            Event::RingEnded { call, end } => {
                if self.ringing.as_ref().is_some_and(|ringing| ringing.call == *call) {
                    self.ringing = None;
                    self.rang_out = Some((*call, end.by.clone()));
                }
            }
            Event::CallCaptions { call, captions } => {
                if self.call.as_ref().is_some_and(|active| active.id == *call) {
                    self.captions = captions.clone();
                }
            }
            Event::CallRecording { call, recording } => {
                if self.call.as_ref().is_some_and(|active| active.id == *call) {
                    self.recording = *recording;
                }
            }
            Event::Call { call, status } => self.status(*call, status),
            Event::Voicemail { call } => {
                if let Some(active) = self.call.as_mut().filter(|active| active.id == *call) {
                    active.voicemail = true;
                }
            }
            Event::AudioClosed { call, microphone } => self.audio_closed(*call, *microphone),
            Event::CameraOff { call } => {
                if let Some(active) = self.call.as_mut().filter(|active| active.id == *call) {
                    // a camera that was on stopped by itself (its device gone; the engine tells
                    // no other): not a camera that works, whether or not its turning on was answered here yet
                    active.check.camera_failed = true;
                    active.camera = false;
                }
            }
            Event::CallParticipants { call, view } => {
                if let Some(active) = self.call.as_mut().filter(|active| active.id == *call) {
                    active.check.saw(view);
                    active.view = view.clone();
                    // someone added is in the call now: no longer listed apart
                    active.invited.retain(|invited| {
                        !view
                            .participants
                            .iter()
                            .any(|participant| participant.mri == invited.mri.as_str())
                    });
                }
            }
            Event::Lobby { call, waiting } => {
                if let Some(active) = self.call.as_mut().filter(|active| active.id == *call) {
                    active.lobby.clone_from(waiting);
                }
            }
            Event::CallOwn { .. }
            | Event::CallHeld { .. }
            | Event::CallHeldByFarEnd { .. }
            | Event::CallMeeting { .. }
            | Event::CallSharing { .. }
            | Event::CallRemoteShare { .. }
            | Event::CallGroupChat { .. }
            | Event::CallAddFailed { .. }
            | Event::CallReaction { .. } => self.in_call(event),
            Event::CallTransferFailed { .. } | Event::CallTransferred { .. } => self.transfer_told(event),
            _ => return false,
        }
        cx.notify();
        true
    }

    /// The account's own state in its call, or a live reaction sent in it.
    fn in_call(&mut self, event: &Event) {
        match event {
            Event::CallOwn {
                call,
                hand_raised,
                organizer,
            } => {
                if let Some(active) = self.call.as_mut().filter(|active| active.id == *call) {
                    active.hand_raised = *hand_raised;
                    active.organizer = *organizer;
                }
            }
            Event::CallHeld { call, held } => {
                if let Some(active) = self.call.as_mut().filter(|active| active.id == *call) {
                    active.held = held.then(|| active.held.unwrap_or_else(Instant::now));
                }
            }
            Event::CallHeldByFarEnd { call, held } => {
                if let Some(active) = self.call.as_mut().filter(|active| active.id == *call) {
                    active.held_by_far_end = *held;
                }
            }
            Event::CallMeeting { call, state } => {
                if let Some(active) = self.call.as_mut().filter(|active| active.id == *call) {
                    // the organizer muted the account anew (Mute all): its microphone shows muted, as the engine made it
                    if state.muted_by_organizer > active.meeting.muted_by_organizer {
                        active.muted = true;
                    }
                    active.meeting = state.clone();
                }
            }
            Event::CallSharing { call, on } => {
                if let Some(active) = self.call.as_mut().filter(|active| active.id == *call) {
                    active.sharing = *on;
                }
            }
            Event::CallRemoteShare { call, on } => {
                if let Some(active) = self.call.as_mut().filter(|active| active.id == *call) {
                    active.remote_share = *on;
                }
            }
            // a 1:1 call someone was added to is a group call in its new chat from now on
            Event::CallGroupChat { call, chat } => {
                if let Some(active) = self.call.as_mut().filter(|active| active.id == *call) {
                    active.chat = Some(chat.clone());
                }
            }
            Event::CallAddFailed { call, who } => {
                if let Some(invited) = self
                    .call
                    .as_mut()
                    .filter(|active| active.id == *call)
                    .and_then(|active| active.invited.iter_mut().find(|invited| invited.mri == *who))
                {
                    invited.no_response = true;
                }
            }
            Event::CallReaction {
                call,
                from,
                name,
                reaction,
            } => {
                if let Some(active) = self.call.as_mut().filter(|active| active.id == *call) {
                    let now = Instant::now();
                    active
                        .reactions
                        .retain(|shown| now.duration_since(shown.at) < REACTION_SHOWN);
                    active.reactions.push(LiveReaction {
                        from: from.clone(),
                        name: name.clone(),
                        reaction: reaction.clone(),
                        at: now,
                    });
                    let over = active.reactions.len().saturating_sub(REACTIONS_SHOWN);
                    active.reactions.drain(..over);
                }
            }
            _ => {}
        }
        self.resolve_meeting();
    }

    /// Replace an unresolved invitation only when the service supplies its matching thread and organizer.
    fn resolve_meeting(&mut self) {
        let Some(active) = &self.call else { return };
        if self.meeting_call != Some(active.id) || !matches!(self.meeting, Some(MeetingToJoin::Code(_))) {
            return;
        }
        let Some(reference) = active
            .meeting
            .join
            .as_ref()
            .and_then(|join| join.reference_link.as_deref())
        else {
            return;
        };
        if !reference.starts_with("https://teams.microsoft.com/l/meetup-join/") {
            return;
        }
        let Some(chat) = teams_proto::join_url_chat(reference).filter(|chat| active.chat.as_ref() == Some(chat)) else {
            return;
        };
        let Some(meeting) = teams_proto::join_url_meeting(reference) else {
            return;
        };
        self.meeting = Some(MeetingToJoin::Chat {
            chat,
            meeting,
            title: active.title.clone().unwrap_or_else(|| "Meeting".to_owned()),
        });
    }

    /// `who` (named `name`) was added to `call`: People lists them ringing until the roster has them in the call; one
    /// called again rings again.
    pub fn invited(&mut self, call: CallId, who: Mri, name: String, cx: &mut Context<Self>) {
        let Some(active) = self.call.as_mut().filter(|active| active.id == call) else {
            return;
        };
        active.invited.retain(|invited| invited.mri != who);
        active.invited.push(Invited {
            mri: who,
            name,
            no_response: false,
        });
        cx.notify();
    }

    /// The account's transfer not taken (the call stays held, and its bar says so), or the far end's transfer taken (the
    /// engine calls the target now: a call with them in their 1:1 chat).
    fn transfer_told(&mut self, event: &Event) {
        match event {
            Event::CallTransferFailed { call } if self.call.as_ref().is_some_and(|active| active.id == *call) => {
                self.refused = Some(Refused {
                    ask: "transfer",
                    why: "not-transferred",
                    text: "The call couldn't be transferred.".to_owned(),
                });
            }
            Event::CallTransferred { chat, .. } => self.asked = Some((CallKind::Chat, chat.clone())),
            _ => {}
        }
    }

    /// The microphone (`microphone`) or the speaker of `call` went away: its check fails. The microphone shows muted,
    /// as it stays until an unmute opens it again; the speaker is reopened by the engine about once a second.
    fn audio_closed(&mut self, call: CallId, microphone: bool) {
        if let Some(active) = self.call.as_mut().filter(|active| active.id == call) {
            if microphone {
                active.check.microphone_lost = true;
                active.muted = true;
            } else {
                active.check.speaker_lost = true;
            }
        }
    }

    /// A call of the account's moved on.
    fn status(&mut self, call: CallId, status: &CallStatus) {
        if self.call.as_ref().is_none_or(|active| active.id != call) {
            self.captions = teams_core::CallCaptions::default();
            self.pane.shown = false;
            self.recording = teams_core::CallRecording::default();
            self.call = Some(self.adopt(call));
            self.ended = None;
            self.refused = None;
            // a call took over from the pre-join; what it chose is for the meeting's own call only
            self.prejoin = None;
            if self
                .call
                .as_ref()
                .is_some_and(|active| active.kind != CallKind::Meeting)
            {
                self.choices = None;
                self.taken_back = false;
            }
        }
        let Some(active) = self.call.as_mut() else { return };
        active.status = status.clone();
        if *status == CallStatus::Connected && active.connected_at.is_none() {
            active.connected_at = Some(Instant::now());
            active.check.connected = true;
        }
        if let CallStatus::Ended(end) = status {
            self.captions = teams_core::CallCaptions::default();
            self.pane.shown = false;
            self.recording = teams_core::CallRecording::default();
            active.check.failed = matches!(end, End::Failed(_));
            // a refusal during the call was shown in its bar: how it ended is what is told now; but the start's own
            // refusal (its media couldn't be set up, told as it ended) says why it never connected
            let why_it_failed = matches!(end, End::Failed(_))
                && self
                    .refused
                    .as_ref()
                    .is_some_and(|refused| refused.why == "media" && START_ASKS.contains(&refused.ask));
            if !why_it_failed {
                self.refused = None;
            }
            self.ended = self.call.take().map(|active| Ended::of(active, end.clone()));
        }
    }

    /// A call told for the first time: the invitation it answers (or declines), else the start or join asked for.
    fn adopt(&mut self, call: CallId) -> ActiveCall {
        let invitation = self.ringing.take_if(|ringing| ringing.call == call);
        let (kind, chat, caller, caller_name) = if let Some(incoming) = invitation {
            (
                CallKind::Incoming,
                incoming.group_chat,
                Some(incoming.caller),
                incoming.caller_name,
            )
        } else {
            let (kind, chat) = self.asked.take().unwrap_or((CallKind::Chat, None));
            (kind, chat, None, None)
        };
        self.meeting_call = (kind == CallKind::Meeting && self.meeting.is_some()).then_some(call);
        let title = self
            .meeting
            .as_ref()
            .filter(|_| kind == CallKind::Meeting)
            .map(|to_join| to_join.title().to_owned());
        ActiveCall {
            id: call,
            kind,
            chat,
            caller,
            caller_name,
            status: CallStatus::Placing,
            connected_at: None,
            muted: false,
            camera: false,
            voicemail: false,
            title,
            view: CallView::default(),
            check: DeviceCheck::default(),
            lobby: Vec::new(),
            hand_raised: false,
            organizer: false,
            reactions: Vec::new(),
            held: None,
            held_by_far_end: false,
            meeting: teams_core::MeetingState::default(),
            sharing: false,
            remote_share: false,
            invited: Vec::new(),
        }
    }

    /// The call action `ask` stands for now, or why there is none.
    fn caption_action(&self, ask: &Ask) -> Result<CallAction, &'static str> {
        let hiding = matches!(ask, Ask::Captions(false) | Ask::RecordCaptions(false))
            || (matches!(ask, Ask::ToggleCaptions) && self.captions.phase != teams_core::CaptionPhase::Off);
        let call = self
            .call
            .as_ref()
            .filter(|call| hiding || (call.status == CallStatus::Connected && call.kind != CallKind::TestCall))
            .ok_or("no-call")?;
        Ok(match ask {
            Ask::ToggleCaptions => CallAction::Captions {
                call: call.id,
                enabled: self.captions.phase == teams_core::CaptionPhase::Off,
            },
            Ask::Captions(enabled) | Ask::RecordCaptions(enabled) => CallAction::Captions {
                call: call.id,
                enabled: *enabled,
            },
            Ask::CaptionLanguage(language) => CallAction::CaptionLanguage {
                call: call.id,
                language: language.clone(),
            },
            _ => unreachable!(),
        })
    }

    fn action_for(&self, ask: &Ask) -> Result<CallAction, &'static str> {
        if !self.shown {
            return Err("no-calls");
        }
        let busy = self.busy();
        match ask {
            Ask::ToggleCaptions | Ask::Captions(_) | Ask::RecordCaptions(_) | Ask::CaptionLanguage(_) => {
                self.caption_action(ask)
            }
            Ask::Call { .. }
            | Ask::TestCall
            | Ask::Join { .. }
            | Ask::JoinMeeting { .. }
            | Ask::JoinMeetingCode(_)
            | Ask::MeetNow { .. }
                if busy =>
            {
                Err("busy")
            }
            Ask::MeetNow { channel } => Ok(CallAction::MeetNow {
                channel: channel.clone(),
            }),
            Ask::Call { chat } => Ok(CallAction::Call { chat: chat.clone() }),
            Ask::TestCall => self
                .echo
                .clone()
                .map(|echo| CallAction::TestCall { echo })
                .ok_or("no-echo"),
            Ask::Join { chat } if !self.joinable(chat) => Err("not-joinable"),
            Ask::Join { chat } => Ok(CallAction::Join { chat: chat.clone() }),
            Ask::JoinMeetingCode(code) => Ok(CallAction::JoinMeetingCode(code.clone())),
            Ask::JoinMeeting { chat, meeting } => Ok(CallAction::JoinMeeting {
                chat: chat.clone(),
                meeting: meeting.clone(),
            }),
            Ask::Answer | Ask::Decline => {
                let call = self.ringing.as_ref().map(|ringing| ringing.call).ok_or("not-ringing")?;
                Ok(if *ask == Ask::Answer {
                    CallAction::Answer { call }
                } else {
                    CallAction::Decline { call }
                })
            }
            Ask::HangUp => {
                let call = self.call.as_ref().ok_or("no-call")?;
                Ok(CallAction::HangUp { call: call.id })
            }
            Ask::ToggleMute => {
                let call = self.call.as_ref().ok_or("no-call")?;
                // the organizer turned attendees' unmuting off: the microphone stays as it is, as Teams greys it
                if call.muted && call.meeting.unmute_blocked() {
                    return Err("unmute-blocked");
                }
                Ok(CallAction::Mute {
                    call: call.id,
                    muted: !call.muted,
                })
            }
            Ask::Lobby { waiting, admit } => {
                let call = self.call.as_ref().ok_or("no-call")?;
                if !call.lobby.iter().any(|person| &person.mri == waiting) {
                    return Err("not-waiting");
                }
                Ok(CallAction::Lobby {
                    call: call.id,
                    waiting: waiting.clone(),
                    admit: *admit,
                })
            }
            Ask::ToggleHand
            | Ask::React(_)
            | Ask::EndMeeting
            | Ask::Timer(_)
            | Ask::LockMeeting(_)
            | Ask::MuteAll
            | Ask::MeetingOption(_) => self.meeting_action(ask),
            Ask::ToggleHold | Ask::Add { .. } | Ask::Transfer { .. } => self.one_to_one_action(ask),
            Ask::Record { .. } | Ask::StopRecording | Ask::Transcribe { .. } | Ask::StopTranscription => {
                self.record_action(ask)
            }
            Ask::ToggleShare | Ask::ToggleCamera => self.video_action(ask),
        }
    }

    /// The engine's call action for the camera or Share (each on or off as it isn't now), on the call up; none in a
    /// build that shows no video.
    fn video_action(&self, ask: &Ask) -> Result<CallAction, &'static str> {
        if self.video.is_none() {
            return Err("no-video");
        }
        let call = self.call.as_ref().ok_or("no-call")?;
        // the organizer turned attendees' cameras off, or only it presents: greyed in Teams, refused here
        let blocked = match ask {
            Ask::ToggleShare => !call.sharing && call.meeting.share_blocked(),
            _ => !call.camera && call.meeting.camera_blocked(),
        };
        if blocked {
            return Err("not-allowed");
        }
        Ok(if *ask == Ask::ToggleShare {
            CallAction::Share {
                call: call.id,
                on: !call.sharing,
            }
        } else {
            CallAction::Camera {
                call: call.id,
                on: !call.camera,
            }
        })
    }

    /// The engine's call action for Hold or Resume, someone added, or a transfer, on the call up.
    fn one_to_one_action(&self, ask: &Ask) -> Result<CallAction, &'static str> {
        let call = self.call.as_ref().ok_or("no-call")?;
        Ok(match ask {
            Ask::Add { invitee, .. } => CallAction::Add {
                call: call.id,
                invitee: invitee.clone(),
            },
            Ask::Transfer { target } => CallAction::Transfer {
                call: call.id,
                target: target.clone(),
            },
            _ => CallAction::Hold {
                call: call.id,
                held: call.held.is_none(),
            },
        })
    }

    /// The engine's call action for Raise or Lower hand, a live reaction or End meeting, on the call up.
    fn meeting_action(&self, ask: &Ask) -> Result<CallAction, &'static str> {
        let call = self.call.as_ref().ok_or("no-call")?;
        match ask {
            Ask::ToggleHand => Ok(CallAction::RaiseHand {
                call: call.id,
                raised: !call.hand_raised,
            }),
            Ask::React(reaction) => Ok(CallAction::React {
                call: call.id,
                reaction: reaction.clone(),
            }),
            Ask::EndMeeting if call.organizer => Ok(CallAction::EndMeeting { call: call.id }),
            Ask::Timer(timer) => Ok(CallAction::Timer {
                call: call.id,
                timer: timer.clone(),
            }),
            Ask::LockMeeting(locked) if call.organizer => Ok(CallAction::LockMeeting {
                call: call.id,
                locked: *locked,
            }),
            Ask::MuteAll if call.organizer => Ok(CallAction::MuteAll { call: call.id }),
            Ask::MeetingOption(option) if call.organizer => Ok(CallAction::MeetingOption {
                call: call.id,
                option: *option,
            }),
            _ => Err("not-organizer"),
        }
    }

    /// The engine's action for a recording's or a transcription's start or stop, on the call up once it is connected
    /// (not the test call's).
    fn record_action(&self, ask: &Ask) -> Result<CallAction, &'static str> {
        let call = self
            .call
            .as_ref()
            .filter(|call| call.status == CallStatus::Connected && call.kind != CallKind::TestCall)
            .ok_or("no-call")?
            .id;
        Ok(match ask.clone() {
            Ask::Record {
                title,
                organizer,
                language,
                audio_only,
            } => CallAction::Record {
                call,
                title,
                organizer,
                language,
                audio_only,
            },
            Ask::Transcribe {
                title,
                organizer,
                language,
            } => CallAction::Transcribe {
                call,
                title,
                organizer,
                language,
            },
            Ask::StopTranscription => CallAction::StopTranscription { call },
            _ => CallAction::StopRecording { call },
        })
    }

    /// The start or join `start` (its number) still isn't told: the engine never answered it. It is given up, and said.
    fn not_told(&mut self, start: u64, ask: &Ask, cx: &mut Context<Self>) -> bool {
        if self.starts != start || self.asked.is_none() || self.call.is_some() {
            return false;
        }
        self.asked = None;
        self.gave_up.insert(start);
        self.refuse(ask, "not-told", local_refusal_text("not-told"), cx);
        true
    }

    fn refuse(&mut self, ask: &Ask, why: &'static str, text: String, cx: &mut Context<Self>) {
        if !self.shown {
            return;
        }
        if let Some(chat) = ask.chat().filter(|_| why == "not-joinable") {
            self.running.remove(chat);
        }
        if let Some(prejoin) = &mut self.prejoin {
            prejoin.joining = false;
        }
        if self.asked.is_none() {
            // no start or join is pending any more (the one a cancelled pre-join asked for was refused): there is no
            // call to hang up; a later ask refused as busy leaves the pending one to be hung up
            self.taken_back = false;
        }
        self.refused = Some(Refused {
            ask: ask.name(),
            why,
            text,
        });
        cx.notify();
    }

    /// How the engine answered `ask`'s action.
    fn answered(
        &mut self,
        ask: &Ask,
        start: Option<u64>,
        action: &CallAction,
        answer: Result<CallId, EngineError>,
        cx: &mut Context<Self>,
    ) {
        let given_up = start.is_some_and(|start| self.gave_up.remove(&start));
        match answer {
            Ok(call) => {
                let Some(active) = self.call.as_mut().filter(|active| active.id == call) else {
                    return;
                };
                match action {
                    CallAction::Mute { muted, .. } => active.muted = *muted,
                    // shown raised at once, as Teams does; the roster tells it as it stands
                    CallAction::RaiseHand { raised, .. } => active.hand_raised = *raised,
                    CallAction::Camera { on, .. } => {
                        active.camera = *on;
                        active.check.camera |= *on;
                    }
                    // listed ringing at once, as Teams lists them; called again, ringing again
                    CallAction::Add { invitee, .. } => {
                        let name = match ask {
                            Ask::Add { name, .. } => name.clone(),
                            _ => String::new(),
                        };
                        self.invited(call, invitee.clone(), name, cx);
                        return;
                    }
                    _ => return,
                }
                cx.notify();
            }
            // an unmute a newer mute overtook: that mute's state stands and nothing went wrong to tell
            Err(EngineError::Call(CallActionError::Superseded)) => {}
            Err(error) => {
                if let Some(start) = start {
                    // a start already given up on was said not to start: its refusal (the engine refuses it as late)
                    // says nothing more, and leaves a start asked since alone
                    if given_up {
                        return;
                    }
                    // the one start asked for (another is refused as busy meanwhile)
                    if self.starts == start {
                        self.asked = None;
                    }
                }
                // a camera the media couldn't turn on (its device missing, the portal refusing it): it doesn't work
                if let (CallAction::Camera { call, on: true }, EngineError::Call(CallActionError::Media(_))) =
                    (action, &error)
                    && let Some(active) = self.call.as_mut().filter(|active| active.id == *call)
                {
                    active.check.camera_failed = true;
                }
                self.refuse(ask, refusal_kind(&error), refusal_text(&error), cx);
            }
        }
    }
}

/// The trace's name for why the engine refused a call action.
#[must_use]
pub fn refusal_kind(error: &EngineError) -> &'static str {
    match error {
        EngineError::Call(CallActionError::NoCalling) => "no-calling",
        EngineError::Call(CallActionError::Busy) => "busy",
        EngineError::Call(CallActionError::NotAChat) => "not-a-chat",
        EngineError::Call(CallActionError::NoMedia) => "no-media",
        EngineError::Call(CallActionError::NoVideo) => "no-video",
        EngineError::Call(CallActionError::Media(_)) => "media",
        EngineError::Call(CallActionError::Late) => "late",
        EngineError::Call(CallActionError::Superseded) => "superseded",
        EngineError::Call(CallActionError::Call(teams_core::CallError::NotJoinable)) => "not-joinable",
        EngineError::Call(CallActionError::Call(_)) => "refused",
        EngineError::Stopped => "stopped",
        _ => "failed",
    }
}

/// What the user is told of a refused call action.
fn refusal_text(error: &EngineError) -> String {
    match error {
        EngineError::Stopped => "the app is offline".to_owned(),
        EngineError::Call(CallActionError::Call(teams_core::CallError::NotJoinable)) => {
            "the call there has ended".to_owned()
        }
        EngineError::Call(CallActionError::NoVideo) => "this call carries no video".to_owned(),
        other => other.to_string(),
    }
}

/// What a refusal the views decided themselves tells the user.
fn local_refusal_text(why: &str) -> String {
    match why {
        "busy" => "a call is already up".to_owned(),
        "no-echo" => "the echo service isn't known".to_owned(),
        "not-joinable" => "no running call is known there".to_owned(),
        "not-ringing" => "no call rings".to_owned(),
        "no-call" => "no call is up".to_owned(),
        "no-chat" => "no chat is open".to_owned(),
        "no-roster" => "this chat's members are still loading: try again in a moment".to_owned(),
        "another-account" => "another account is in a call".to_owned(),
        "no-meeting" => "this meeting couldn't be found: try again in a moment".to_owned(),
        "no-video" => "calls here carry no video".to_owned(),
        "not-waiting" => "they aren't waiting in the lobby any more".to_owned(),
        "not-organizer" => "only the meeting's organizer can end it for everyone".to_owned(),
        "unmute-blocked" => "the organizer doesn't let attendees unmute".to_owned(),
        "not-allowed" => "the organizer doesn't let attendees do that".to_owned(),
        "not-told" => "the call didn't start: the app got no answer from the calling service, try again".to_owned(),
        _ => "calls aren't available".to_owned(),
    }
}

/// The user asks `ask` of the calls: the engine's call action for it, traced (`call asked`, and `call refused` with
/// why when it goes nowhere: decided here, or refused by the engine). The answer updates `calls`.
/// Nothing at all in a build that shows no calls: no trace, no refusal, nothing asked of the engine.
pub fn ask(calls: &Entity<Calls>, core: &Arc<dyn Core>, trace: &Arc<ViewTrace>, ask: Ask, cx: &mut App) {
    ask_then(calls, core, trace, ask, cx, |_, _| {});
}

/// How an ask went, as [`ask_then`] tells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answered {
    /// The engine took it (or the views did it themselves).
    Taken,
    /// Refused here or by the engine, or no calls shown.
    Refused,
    /// An unmute a newer mute or unmute overtook while its microphone reopened
    /// ([`CallActionError::Superseded`]): the newer one decides, and nothing went wrong, so nothing is kept in
    /// [`Calls::refused`].
    Superseded,
}

impl Answered {
    /// The engine's answer to an ask's action, as it goes.
    fn of<T>(answer: &Result<T, EngineError>) -> Self {
        match answer {
            Ok(_) => Self::Taken,
            Err(EngineError::Call(CallActionError::Superseded)) => Self::Superseded,
            Err(_) => Self::Refused,
        }
    }
}

/// [`ask`], and then `then` told how it went ([`Answered`]). `then` is told later, never while this runs, so a view may
/// ask from within its own update.
pub fn ask_then(
    calls: &Entity<Calls>,
    core: &Arc<dyn Core>,
    trace: &Arc<ViewTrace>,
    ask: Ask,
    cx: &mut App,
    then: impl FnOnce(Answered, &mut App) + 'static,
) {
    if !calls.read(cx).shows() {
        cx.defer(move |cx| then(Answered::Refused, cx));
        return;
    }
    let mut asked = TraceLine::new("call", "asked").kind(ask.name());
    if let Some(chat) = ask.chat() {
        asked = asked.conversation(chat);
    }
    trace.record(&asked);
    log::info!("call: {} asked{}", ask.name(), logged_chat(&ask));
    // the caption pane shown or hidden here; the captions themselves only when they don't run for the saved transcript
    let Some(ask) = calls.update(cx, |calls, cx| calls.caption_pane_ask(ask, cx)) else {
        cx.defer(move |cx| then(Answered::Taken, cx));
        return;
    };
    let action = match calls.read(cx).action_for(&ask) {
        Ok(action) => action,
        Err(why) => {
            refused(trace, &ask, why);
            calls.update(cx, |calls, cx| calls.refuse(&ask, why, local_refusal_text(why), cx));
            cx.defer(move |cx| then(Answered::Refused, cx));
            return;
        }
    };
    let start = ask.starts().map(|start| {
        // busy until its call is told or it is refused: the header's Call goes, and another start is refused meanwhile
        calls.update(cx, |calls, cx| {
            if !matches!(ask, Ask::MeetNow { .. }) && calls.meeting.as_ref().is_some_and(|target| target.ask() != ask) {
                calls.meeting = None;
            }
            calls.asked = Some(start);
            calls.starts += 1;
            cx.notify();
            calls.starts
        })
    });
    let reply = core.call(action.clone());
    let (weak, trace) = (calls.downgrade(), Arc::clone(trace));
    if let Some(start) = start {
        give_up_untold(weak.clone(), Arc::clone(&trace), ask.clone(), start, cx);
    }
    cx.spawn(async move |cx: &mut AsyncApp| {
        let answer = reply.await;
        if let Err(error) = &answer {
            refused(&trace, &ask, refusal_kind(error));
        }
        let answered = Answered::of(&answer);
        // calls that are gone have nobody left to tell
        let _ = weak.update(cx, |calls, cx| calls.answered(&ask, start, &action, answer, cx));
        cx.update(|cx| then(answered, cx));
    })
    .detach();
}

/// How long a start or a join may go untold by the engine (it tells one as its media begins to be set up, at once)
/// before it is given up and the user told so, rather than the header's Call staying gone with nothing shown.
/// Longer than the engine's own bound ([`teams_core::START_WITHIN`]), past which it refuses a start rather than placing
/// it: one given up on here never rings anyone afterwards.
pub const START_TOLD_WITHIN: Duration = Duration::from_secs(teams_core::START_WITHIN.as_secs() + 5);

/// The asks that start or join a call, by their trace names.
const START_ASKS: [&str; 5] = ["call", "test-call", "join", "join-meeting", "meet-now"];

/// Gives up on the start or join `start` (its number) once [`START_TOLD_WITHIN`] passed with its call untold.
fn give_up_untold(calls: WeakEntity<Calls>, trace: Arc<ViewTrace>, ask: Ask, start: u64, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        cx.background_executor().timer(START_TOLD_WITHIN).await;
        let gave_up = calls
            .update(cx, |calls, cx| calls.not_told(start, &ask, cx))
            .unwrap_or(false);
        if gave_up {
            log::warn!(
                "call: {} not told by the engine within {} s: given up",
                ask.name(),
                START_TOLD_WITHIN.as_secs()
            );
            refused(&trace, &ask, "not-told");
        }
    })
    .detach();
}

/// The ask's chat for a log line, when it has one.
fn logged_chat(ask: &Ask) -> String {
    ask.chat().map_or_else(String::new, |chat| format!(" in {chat}"))
}

/// Join now of the pre-join that shows: its choices kept for its call, its join asked. Nothing while none shows or its
/// join is under way.
pub fn join_now(calls: &Entity<Calls>, core: &Arc<dyn Core>, trace: &Arc<ViewTrace>, cx: &mut App) {
    join_now_then(calls, core, trace, cx, |_, _| {});
}

/// [`join_now`], and then `then` told whether the engine took its join ([`Answered::Taken`] of [`ask_then`]; `false` too when
/// nothing was asked: no pre-join shows or its join is under way). `then` is told later, never while this runs.
pub fn join_now_then(
    calls: &Entity<Calls>,
    core: &Arc<dyn Core>,
    trace: &Arc<ViewTrace>,
    cx: &mut App,
    then: impl FnOnce(bool, &mut App) + 'static,
) {
    let asked = calls.update(cx, |calls, cx| {
        let prejoin = calls.prejoin.as_mut().filter(|prejoin| !prejoin.joining)?;
        prejoin.joining = true;
        let to_join = prejoin.to_join.clone();
        let starts = prejoin.entry == PrejoinEntry::MeetNow;
        let choices = Choices {
            muted: prejoin.muted || prejoin.audio == PrejoinAudio::None,
            camera: prejoin.camera,
        };
        calls.refused = None;
        calls.choices = Some(choices);
        calls.meeting = Some(to_join.clone());
        cx.notify();
        // a Rejoin of a meeting started here goes into it as it runs (`Self::rejoin_of`)
        let ask = if starts {
            Ask::MeetNow {
                channel: to_join.chat()?.to_owned(),
            }
        } else {
            to_join.ask()
        };
        Some((ask, starts.then(|| to_join.title().to_owned())))
    });
    let Some((asked, title)) = asked else {
        cx.defer(move |cx| then(false, cx));
        return;
    };
    let titled = match (&asked, title) {
        (Ask::MeetNow { channel }, Some(title)) if !title.trim().is_empty() => Some((channel.clone(), title)),
        _ => None,
    };
    ask_then(calls, core, trace, asked, cx, move |answered, cx| {
        then(answered == Answered::Taken, cx);
    });
    // as Teams' pre-join does: its subject titles the new meeting's post once the service posted it (the engine
    // looks for the post, and titles nothing when the meeting didn't start); a Meet now refused here asks nothing,
    // so no other meeting's post of the account's in that channel is titled
    let went = calls.read(cx).prejoin().is_some_and(|prejoin| prejoin.joining);
    if let Some((channel, title)) = titled.filter(|_| went) {
        drop(core.title_meet_now(channel, title.trim().to_owned()));
    }
}

/// The calendar's Join: `to_join`'s pre-join, in its own window; while a call of the account is up, rings or is being
/// set up, or another account's is (`busy_elsewhere`: calls are one at a time across the accounts), its refusal instead.
/// Whether the pre-join shows (never in a build that shows no calls).
pub fn show_prejoin(
    calls: &Entity<Calls>,
    trace: &Arc<ViewTrace>,
    to_join: MeetingToJoin,
    busy_elsewhere: bool,
    cx: &mut App,
) -> bool {
    let busy = calls.read(cx).busy();
    if busy || busy_elsewhere {
        let ask = to_join.ask();
        let why = if busy { "busy" } else { "another-account" };
        refuse_here(calls, trace, &ask, why, cx);
        return false;
    }
    let mut shown = TraceLine::new("call", "prejoin");
    if let Some(chat) = to_join.chat() {
        shown = shown.conversation(chat);
    }
    trace.record(&shown);
    calls.update(cx, |calls, cx| calls.open_prejoin(to_join, cx));
    crate::call_window::raise(cx);
    calls.read(cx).prejoin().is_some()
}

/// The user asks that something nothing stands for (no chat open to call or join): traced, and told; nothing in a
/// build that shows no calls.
pub fn refuse_here(calls: &Entity<Calls>, trace: &Arc<ViewTrace>, ask: &Ask, why: &'static str, cx: &mut App) {
    if !calls.read(cx).shows() {
        return;
    }
    trace.record(&TraceLine::new("call", "asked").kind(ask.name()));
    refused(trace, ask, why);
    calls.update(cx, |calls, cx| calls.refuse(ask, why, local_refusal_text(why), cx));
}

fn refused(trace: &ViewTrace, ask: &Ask, why: &'static str) {
    log::info!("call: {} refused: {why}", ask.name());
    trace.record(&TraceLine::new("call", "refused").kind(ask.name()).kind(why));
}

/// What a call waiting in its meeting's lobby says, as Teams' lobby screen does.
pub const LOBBY_WAIT: &str = "Someone in the meeting should let you in soon";

/// The trace's name for a call's status.
#[must_use]
pub fn status_name(status: &CallStatus) -> &'static str {
    match status {
        CallStatus::Placing => "placing",
        CallStatus::Ringing => "ringing",
        CallStatus::Joining => "joining",
        CallStatus::Answering => "answering",
        CallStatus::Declining => "declining",
        CallStatus::Connecting => "connecting",
        CallStatus::Connected => "connected",
        CallStatus::Reconnecting => "reconnecting",
        CallStatus::Lobby => "lobby",
        CallStatus::Leaving => "leaving",
        CallStatus::Ended(_) => "ended",
    }
}

/// The trace's name for how a call ended.
#[must_use]
pub fn end_name(end: &End) -> &'static str {
    match end {
        End::HungUp => "hung-up",
        End::Declined => "declined",
        End::Remote(_) => "remote",
        End::Failed(_) => "failed",
    }
}

/// What the in-call view says of a call's status.
#[must_use]
pub fn status_text(active: &ActiveCall) -> String {
    if active.voicemail {
        return "Declined or not answered: voicemail".to_owned();
    }
    match &active.status {
        CallStatus::Placing if active.kind == CallKind::Meeting => "Joining…".to_owned(),
        CallStatus::Placing => "Calling…".to_owned(),
        CallStatus::Ringing => "Ringing…".to_owned(),
        CallStatus::Joining => "Joining…".to_owned(),
        CallStatus::Answering => "Answering…".to_owned(),
        CallStatus::Declining => "Declining…".to_owned(),
        CallStatus::Connecting => "Connecting…".to_owned(),
        CallStatus::Connected => "Connected".to_owned(),
        CallStatus::Reconnecting => "Reconnecting…".to_owned(),
        CallStatus::Lobby => LOBBY_WAIT.to_owned(),
        CallStatus::Leaving => "Leaving…".to_owned(),
        CallStatus::Ended(end) => end_text(end, false),
    }
}

/// What the in-call view says of how a call ended, in a few words and never with a code the service sent (the
/// call window says why: [`Ended::reason`]).
#[must_use]
pub fn end_text(end: &End, voicemail: bool) -> String {
    match end {
        _ if voicemail => "Call ended (it went to voicemail)".to_owned(),
        End::HungUp | End::Remote(_) => "Call ended".to_owned(),
        End::Declined => "Call declined".to_owned(),
        End::Failed(_) => "Call failed".to_owned(),
    }
}

/// A call's duration as the in-call view shows it: `m:ss`, or `h:mm:ss` past an hour.
#[must_use]
pub fn duration_text(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_duration_is_minutes_and_seconds_then_hours() {
        assert_eq!(duration_text(0), "0:00");
        assert_eq!(duration_text(75), "1:15");
        assert_eq!(duration_text(3_725), "1:02:05");
    }
}
