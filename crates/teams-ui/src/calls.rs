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
//!   an invitation, or the chat's own stored call start ([`Calls::posted_running`]), named a running call in (a join
//!   goes there: [`Calls::joinable`]); how the last call ended and the last ask that went nowhere, until the next call.
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

use std::collections::{BTreeSet, HashMap, HashSet};
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
    /// The live reactions sent in the call lately, the newest last, each kept for [`REACTION_SHOWN`] (on its sender's
    /// tile; one that floats goes sooner).
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
    /// ringing, or "No response" once they didn't come in ([`teams_core::Event::CallAddFailed`]). A meeting's invitee
    /// who declined leaves it for [`ActiveCall::passed`] after [`DECLINED_SHOWN`].
    pub invited: Vec<Invited>,
    /// A meeting's invitees who declined its Request to join and, [`DECLINED_SHOWN`] after it, went back to "Others
    /// invited", where Teams lists them with "No response"; until they are asked again or the roster has them in it.
    pub passed: Vec<Mri>,
}

/// Someone the account added to its call, until the roster has them in it or, a meeting's invitee who declined,
/// until [`DECLINED_SHOWN`] moves them to [`ActiveCall::passed`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invited {
    pub mri: Mri,
    pub name: String,
    /// They didn't come in (Teams' "No response", with its call-again).
    pub no_response: bool,
    /// When they declined the ring, on the app's clock (Teams' "Declined", with its call-again).
    pub declined: Option<Instant>,
}

/// A live reaction someone in the call sent ([`teams_core::Event::CallReaction`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveReaction {
    pub from: Mri,
    /// Their name as the reaction gave it.
    pub name: Option<String>,
    /// `like`, `heart`, `applause`, `laugh` or `surprised`.
    pub reaction: String,
    /// When it came, on the app's clock (the executor's).
    pub at: Instant,
    /// Its number among the call's reactions, from 0: the stage's float of it keeps its place by it.
    pub seq: u64,
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

/// How long a live reaction shows on its sender's tile, as Teams plays it there. A reaction that floats (its sender's
/// tile is not on the stage) goes sooner, after the call stage's float time.
pub const REACTION_SHOWN: Duration = Duration::from_millis(4_250);

/// How long a meeting's invitee who declined stays "Declined" in "In this meeting" before going back to "Others
/// invited" (Teams had moved the row back within 13 s of the decline).
pub const DECLINED_SHOWN: Duration = Duration::from_secs(10);

/// The most live reactions the stage shows at once.
const REACTIONS_SHOWN: usize = 12;

impl ActiveCall {
    /// Whether it is a meeting's call: one gone into as a meeting, or one in a meeting's chat (an invitation into it).
    #[must_use]
    pub fn is_meeting(&self) -> bool {
        self.kind == CallKind::Meeting || self.chat.as_deref().is_some_and(teams_proto::is_meeting_chat_id)
    }

    /// Whether the people it is with stand in for its roster while the roster names nobody else: a ring's or a 1:1
    /// call's far end, a group chat's members. Never a meeting's: its chat's members are who was invited, not who
    /// joined.
    #[must_use]
    pub fn people_stand_in(&self) -> bool {
        self.view.participants.is_empty() && !self.is_meeting()
    }

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
    /// The user declined it here (or hung up before its answer went): its end is the user's own, also when the
    /// `reject` was not taken and it ended as failed.
    pub declined: bool,
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
            declined: false,
        }
    }

    /// Why it ended, in the user's terms; `me` is the account's MRI.
    #[must_use]
    pub fn reason(&self, me: &str) -> EndReason {
        let remote = match &self.end {
            _ if self.declined => return EndReason::Own,
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
    /// (the far end hung up, the call dropped or never connected); a hang-up or a decline of the user's closes it,
    /// also a decline whose `reject` was not taken.
    #[must_use]
    pub fn stays_in_window(&self) -> bool {
        self.check.is_some() || !(self.declined || matches!(self.end, End::HungUp | End::Declined))
    }

    /// Whether it was a meeting's call: one gone into as a meeting, or one in a meeting's chat (an invitation into it).
    #[must_use]
    pub fn is_meeting(&self) -> bool {
        self.kind == CallKind::Meeting || self.chat.as_deref().is_some_and(teams_proto::is_meeting_chat_id)
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

impl Ask {
    /// Whether the account's cloud serves what it asks for: a cloud without captions takes no caption ask, one
    /// without recording no recording's or transcription's.
    #[must_use]
    pub fn served(&self, capabilities: teams_core::Capabilities) -> bool {
        match self {
            Self::ToggleCaptions | Self::Captions(_) | Self::RecordCaptions(_) | Self::CaptionLanguage(_) => {
                capabilities.captions
            }
            Self::Record { .. } | Self::StopRecording | Self::Transcribe { .. } | Self::StopTranscription => {
                capabilities.recording
            }
            _ => true,
        }
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
    /// runs. In a channel, `post` is the channel's post of the meeting: the join goes into the meeting of that post.
    JoinMeeting {
        chat: String,
        meeting: MeetingInfo,
        post: Option<String>,
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

/// The join of the meeting whose chat is `chat`: in a channel whose meeting's post (`post`) is known, the join of the
/// meeting of that post, so that the join goes into that meeting and not into another meeting of the channel.
fn join_meeting_action(chat: &str, meeting: &MeetingInfo, post: Option<&str>) -> CallAction {
    match post {
        Some(post) if teams_proto::is_team_thread_id(chat) => CallAction::JoinChannel {
            channel: chat.to_owned(),
            post: Some(post.to_owned()),
        },
        _ => CallAction::JoinMeeting {
            chat: chat.to_owned(),
            meeting: meeting.clone(),
        },
    }
}

/// A meeting a user can go into, as its calendar event names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeetingToJoin {
    /// A calendar or chat meeting with known organizer and tenant. For a channel's meeting, `post` is its post in the
    /// channel when it is known: the join goes into the meeting of that post, not into another meeting of the channel.
    Chat {
        chat: String,
        meeting: MeetingInfo,
        title: String,
        post: Option<String>,
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
            Self::Chat {
                chat, meeting, post, ..
            } => Ask::JoinMeeting {
                chat: chat.clone(),
                meeting: meeting.clone(),
                post: post.clone(),
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
    /// The captions run for the call's live transcript (a transcription runs): they keep running while their pane is
    /// hidden.
    transcript: bool,
}

/// One utterance of the call's live transcript: its last final result, and when it was first heard (ms since the
/// epoch).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpokenLine {
    pub at: i64,
    pub caption: teams_core::Caption,
}

/// The call's live transcript: each final utterance heard while a transcription runs, by its key
/// ([`crate::call_transcript::utterance`]). The captions keep only their last lines; this keeps the whole call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Spoken {
    lines: Vec<SpokenLine>,
    /// Each utterance's line in `lines`.
    index: HashMap<String, usize>,
    /// When each utterance not final yet was first heard.
    heard: HashMap<String, i64>,
    /// A transcription runs and its captions are heard.
    listening: bool,
    /// The utterances already final as the transcription started: said before it, not in it.
    before: HashSet<String>,
}

impl Spoken {
    /// The captions changed from `was` to `now_lines`: while a transcription runs (`transcribing`), what is said in it
    /// is heard.
    fn follow(
        &mut self,
        call: CallId,
        transcribing: bool,
        was: &[teams_core::Caption],
        now_lines: &[teams_core::Caption],
        now: i64,
    ) {
        if !transcribing {
            self.listening = false;
            return;
        }
        if !self.listening {
            self.listening = true;
            self.before = was
                .iter()
                .filter(|caption| caption.final_result)
                .map(|caption| crate::call_transcript::utterance(call, caption))
                .collect();
        }
        self.hear(call, now_lines, now);
    }

    fn hear(&mut self, call: CallId, captions: &[teams_core::Caption], now: i64) {
        for caption in captions {
            let key = crate::call_transcript::utterance(call, caption);
            if self.before.contains(&key) {
                continue;
            }
            let at = *self.heard.entry(key.clone()).or_insert(now);
            if !caption.final_result || caption.text.trim().is_empty() {
                continue;
            }
            if let Some(line) = self.index.get(&key).and_then(|index| self.lines.get_mut(*index)) {
                line.caption.clone_from(caption);
            } else {
                self.index.insert(key, self.lines.len());
                self.lines.push(SpokenLine {
                    at,
                    caption: caption.clone(),
                });
            }
        }
    }
}

/// The calls' model (see the module docs). A GPUI entity in [`crate::Models`].
#[derive(Debug, Default)]
pub struct Calls {
    shown: bool,
    captions: teams_core::CallCaptions,
    /// The caption pane as the user shows it, and whether the captions are saved.
    pane: CaptionPane,
    /// The call's live transcript (empty once another call comes).
    spoken: Spoken,
    /// The account's own start or stop of a recording in its call (empty once the call ends).
    recording: teams_core::CallRecording,
    echo: Option<Mri>,
    registered: bool,
    ringing: Option<IncomingCall>,
    /// The ringing invitation the user answered, until its call is told, its ring ends or the answer is refused: the
    /// answer waits for the call's devices and media, which can take minutes on a first start.
    answering: Option<CallId>,
    /// Where the call devices' discovery is, as [`follow_devices`] last saw it.
    devices: teams_media::devices::catalog::DeviceStatus,
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

    /// Whether the caption pane shows: the captions aren't off and, while the calls' captions are saved or held for the
    /// live transcript, the user showed it (captions that run for the saved or the live transcript alone show nothing).
    #[must_use]
    pub fn captions_shown(&self) -> bool {
        self.captions.phase != teams_core::CaptionPhase::Off
            && (self.pane.shown || !(self.pane.saving || self.pane.transcript))
    }

    /// The call's live transcript: each final utterance heard while a transcription ran, in the order first heard.
    #[must_use]
    pub fn spoken(&self) -> &[SpokenLine] {
        &self.spoken.lines
    }

    /// Whether the captions run for the call's live transcript ([`Self::set_transcript_captions`]).
    #[must_use]
    pub fn transcript_captions(&self) -> bool {
        self.pane.transcript
    }

    /// The captions run for the call's live transcript (`on`): hiding their pane leaves them running.
    pub fn set_transcript_captions(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.pane.transcript != on {
            self.pane.transcript = on;
            cx.notify();
        }
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
    /// or off, or nothing (`None`) when they already run for the saved or the live transcript (shown) or keep running
    /// for it (hidden). Any other ask is left as it is.
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
        let local = running && (show || self.pane.saving || self.pane.transcript);
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

    /// Whether the user answered the invitation that rings and the answer still waits to go.
    #[must_use]
    pub fn answering(&self) -> bool {
        self.ringing
            .as_ref()
            .is_some_and(|ringing| self.answering == Some(ringing.call))
    }

    /// The invitation to ring aloud and notify: the one that rings, unless it is being answered.
    #[must_use]
    pub fn alerting(&self) -> Option<&IncomingCall> {
        self.ringing.as_ref().filter(|_| !self.answering())
    }

    /// Whether the call devices are still being discovered, so a call waits before it goes.
    #[must_use]
    pub fn devices_preparing(&self) -> bool {
        self.devices == teams_media::devices::catalog::DeviceStatus::Preparing
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
        self.answering = None;
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

    /// Whether an invitation, or the chat's own stored call start ([`Calls::posted_running`]), named a running call in
    /// the group chat `chat`, so a join can go there; or `chat` is the meeting's chat a Request to join rang in, which
    /// stays joinable for the run (its join is the meeting's own).
    #[must_use]
    pub fn joinable(&self, chat: &str) -> bool {
        self.running.contains(chat)
    }

    /// The group chat `chat`'s own stored call events say its call `runs` (an unended start) or not, as
    /// [`crate::meeting_alert`] reads them: a group chat's meeting rings nobody, so only its start tells that a join can
    /// go there (the engine joins where the start says it runs). Nothing for any other chat.
    pub fn posted_running(&mut self, chat: &str, runs: bool, cx: &mut Context<Self>) {
        if !self.shown || !teams_proto::is_group_chat_id(chat) {
            return;
        }
        let changed = if runs {
            self.running.insert(chat.to_owned())
        } else {
            self.running.remove(chat)
        };
        if changed {
            cx.notify();
        }
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
        let ask = if choices.muted && !matches!(active.status, CallStatus::Preparing | CallStatus::Placing) {
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
                    self.answering = None;
                    self.rang_out = Some((*call, end.by.clone()));
                }
            }
            Event::CallCaptions { call, captions } => {
                if let Some(active) = self.call.as_ref().filter(|active| active.id == *call) {
                    let now = chrono::Utc::now().timestamp_millis();
                    let transcribing = active.meeting.transcribing.is_some();
                    self.spoken
                        .follow(*call, transcribing, &self.captions.lines, &captions.lines, now);
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
                    let joined = |mri: &Mri| {
                        view.participants
                            .iter()
                            .any(|participant| participant.mri.eq_ignore_ascii_case(mri.as_str()))
                    };
                    active.invited.retain(|invited| !joined(&invited.mri));
                    active.passed.retain(|passed| !joined(passed));
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
            | Event::CallReaction { .. } => self.in_call(event, cx.background_executor().now()),
            Event::CallAddFailed { .. } => self.add_failed(event, cx),
            Event::CallTransferFailed { .. } | Event::CallTransferred { .. } => self.transfer_told(event),
            _ => return false,
        }
        cx.notify();
        true
    }

    /// Someone the account added didn't come in: a meeting's invitee who declined goes back to "Others invited" after
    /// [`DECLINED_SHOWN`].
    fn add_failed(&mut self, event: &Event, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        self.in_call(event, now);
        if let Event::CallAddFailed {
            call,
            who,
            declined: true,
        } = event
            && self
                .call
                .as_ref()
                .is_some_and(|active| active.id == *call && active.is_meeting())
        {
            move_declined_back(*call, who.clone(), now, cx);
        }
    }

    /// The account's own state in its call, or a live reaction sent in it (at `now`, on the app's clock).
    fn in_call(&mut self, event: &Event, now: Instant) {
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
            Event::CallAddFailed { call, who, declined } => {
                if let Some(invited) = self
                    .call
                    .as_mut()
                    .filter(|active| active.id == *call)
                    .and_then(|active| active.invited.iter_mut().find(|invited| invited.mri == *who))
                {
                    invited.no_response = true;
                    invited.declined = declined.then_some(now);
                }
            }
            Event::CallReaction {
                call,
                from,
                name,
                reaction,
            } => {
                if let Some(active) = self.call.as_mut().filter(|active| active.id == *call) {
                    let seq = active.reactions.last().map_or(0, |last| last.seq + 1);
                    active
                        .reactions
                        .retain(|shown| now.saturating_duration_since(shown.at) < REACTION_SHOWN);
                    active.reactions.push(LiveReaction {
                        from: from.clone(),
                        name: name.clone(),
                        reaction: reaction.clone(),
                        at: now,
                        seq,
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
            post: None,
        });
    }

    /// `who` (named `name`) was added to `call`: People lists them ringing until the roster has them in the call; one
    /// called again rings again.
    pub fn invited(&mut self, call: CallId, who: Mri, name: String, cx: &mut Context<Self>) {
        let Some(active) = self.call.as_mut().filter(|active| active.id == call) else {
            return;
        };
        active.invited.retain(|invited| invited.mri != who);
        active.passed.retain(|passed| *passed != who);
        active.invited.push(Invited {
            mri: who,
            name,
            no_response: false,
            declined: None,
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
            self.pane.transcript = false;
            self.spoken = Spoken::default();
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
        // a decline is the user's end, also when its `reject` fails (the caller's cancel came first)
        let declined = active.status == CallStatus::Declining;
        active.status = status.clone();
        if *status == CallStatus::Connected && active.connected_at.is_none() {
            active.connected_at = Some(Instant::now());
            active.check.connected = true;
        }
        if let CallStatus::Ended(end) = status {
            self.captions = teams_core::CallCaptions::default();
            self.pane.shown = false;
            self.pane.transcript = false;
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
            self.ended = self.call.take().map(|active| Ended {
                declined,
                ..Ended::of(active, end.clone())
            });
        }
    }

    /// A call told for the first time: the invitation it answers (or declines), else the start or join asked for.
    fn adopt(&mut self, call: CallId) -> ActiveCall {
        let invitation = self.ringing.take_if(|ringing| ringing.call == call);
        self.answering.take_if(|answering| *answering == call);
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
            passed: Vec::new(),
        }
    }

    /// `who`, who declined `call` (a meeting's) at `at`, goes back to "Others invited" unless asked again since.
    fn declined_shown(&mut self, call: CallId, who: &Mri, at: Instant, cx: &mut Context<Self>) {
        let Some(active) = self.call.as_mut().filter(|active| active.id == call) else {
            return;
        };
        let before = active.invited.len();
        active
            .invited
            .retain(|invited| !(invited.mri == *who && invited.declined == Some(at)));
        if active.invited.len() < before {
            active.passed.push(who.clone());
            cx.notify();
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
            Ask::JoinMeeting { chat, meeting, post } => Ok(join_meeting_action(chat, meeting, post.as_deref())),
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

    /// The user asked the camera of `call` on or off: shown so at once.
    fn camera_asked(&mut self, call: CallId, on: bool, cx: &mut Context<Self>) {
        if let Some(active) = self.call.as_mut().filter(|active| active.id == call) {
            active.camera = on;
            cx.notify();
        }
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
                    CallAction::Camera { on, .. } => active.check.camera |= *on,
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
                // a refused answer leaves the invitation ringing, answerable again; one whose invitation was declined
                // or ended while it waited has nothing left to tell
                if let CallAction::Answer { call } = action {
                    if self.answering.take_if(|answering| answering == call).is_none() {
                        return;
                    }
                    cx.notify();
                }
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
                // a camera the engine didn't turn shows as it was, unless a later switch was asked since
                if let CallAction::Camera { call, on } = action
                    && let Some(active) = self.call.as_mut().filter(|active| active.id == *call)
                {
                    if active.camera == *on {
                        active.camera = !*on;
                        cx.notify();
                    }
                    // a camera the media couldn't turn on (its device missing, the portal refusing it): it doesn't work
                    if *on && matches!(error, EngineError::Call(CallActionError::Media(_))) {
                        active.check.camera_failed = true;
                    }
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
        EngineError::Call(CallActionError::NoCalling) => "calls aren't available".to_owned(),
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

/// Stops presenting calls if background device discovery fails, as a startup without calling would.
pub fn follow_devices(
    calls: &Entity<Calls>,
    directory: Arc<dyn teams_media::devices::catalog::DeviceDirectory>,
    cx: &mut App,
) {
    use teams_media::devices::catalog::DeviceStatus;
    let (told, changes) = async_channel::unbounded();
    directory.watch(Box::new(move |_| {
        let _ = told.try_send(());
    }));
    match directory.status() {
        DeviceStatus::Unavailable => {
            calls.update(cx, |calls, cx| {
                *calls = Calls::default();
                cx.notify();
            });
            return;
        }
        DeviceStatus::Ready => return,
        DeviceStatus::Preparing => calls.update(cx, |calls, cx| {
            calls.devices = DeviceStatus::Preparing;
            cx.notify();
        }),
    }
    let weak = calls.downgrade();
    cx.spawn(async move |cx: &mut AsyncApp| {
        while changes.recv().await.is_ok() {
            match directory.status() {
                DeviceStatus::Unavailable => {
                    let _ = weak.update(cx, |calls, cx| {
                        *calls = Calls::default();
                        cx.notify();
                    });
                    break;
                }
                DeviceStatus::Ready => {
                    let _ = weak.update(cx, |calls, cx| {
                        calls.devices = DeviceStatus::Ready;
                        cx.notify();
                    });
                    break;
                }
                DeviceStatus::Preparing if weak.upgrade().is_none() => break,
                DeviceStatus::Preparing => {}
            }
        }
    })
    .detach();
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
    if !calls.read(cx).shows() || !ask.served(core.capabilities()) {
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
    // an answer that already waits for its media: the key again changes nothing
    let Some(ask) = calls.update(cx, |calls, cx| {
        (ask != Ask::Answer || !calls.answering()).then(|| calls.caption_pane_ask(ask, cx))?
    }) else {
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
    if let CallAction::Answer { call } = action {
        calls.update(cx, |calls, cx| {
            calls.answering = Some(call);
            cx.notify();
        });
    }
    // the camera shows as asked at once, as Teams' toolbar shows it, while it opens or waits for a renegotiation
    if let CallAction::Camera { call, on } = action {
        calls.update(cx, |calls, cx| calls.camera_asked(call, on, cx));
    }
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

/// Moves `who`, who declined `call` at `at`, back to "Others invited" once [`DECLINED_SHOWN`] passed, as Teams does.
fn move_declined_back(call: CallId, who: Mri, at: Instant, cx: &mut Context<Calls>) {
    cx.spawn(async move |calls, cx: &mut AsyncApp| {
        cx.background_executor().timer(DECLINED_SHOWN).await;
        let _ = calls.update(cx, |calls, cx| calls.declined_shown(call, &who, at, cx));
    })
    .detach();
}

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
        CallStatus::Preparing => "preparing",
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
        CallStatus::Preparing => "Preparing…".to_owned(),
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

    fn said(id: &str, speaker: &str, text: &str, final_result: bool, audio: u64) -> teams_core::Caption {
        teams_core::Caption {
            id: id.into(),
            speaker: Some(speaker.into()),
            display_name: Some(format!("Synthetic {speaker}")),
            text: text.into(),
            final_result,
            spoken_language: "en-us".into(),
            track_language: "en-us".into(),
            audio_timestamp: audio,
            duration: 10,
        }
    }

    #[test]
    fn the_transcript_keeps_each_final_utterance_from_when_it_was_first_heard() {
        let call = CallId::for_tests(1);
        let mut spoken = Spoken::default();
        spoken.hear(call, &[said("1", "a", "Syn", false, 100)], 1_000);
        assert!(spoken.lines.is_empty(), "a partial result waits");
        spoken.hear(call, &[said("2", "a", "Synthetic one.", true, 100)], 2_000);
        spoken.hear(call, &[said("3", "b", "Synthetic two.", true, 200)], 3_000);
        // the captions dropped the first utterance: the transcript keeps it, and a later final result replaces words
        spoken.hear(call, &[said("4", "b", "Synthetic two, again.", true, 200)], 4_000);
        spoken.hear(call, &[said("5", "a", " ", true, 300)], 5_000);
        let lines: Vec<(i64, &str)> = spoken
            .lines
            .iter()
            .map(|line| (line.at, line.caption.text.as_str()))
            .collect();
        assert_eq!(lines, [(1_000, "Synthetic one."), (3_000, "Synthetic two, again.")]);
    }

    #[test]
    fn the_transcript_leaves_out_what_was_said_before_its_transcription_started() {
        let call = CallId::for_tests(1);
        let mut spoken = Spoken::default();
        let before = [said("1", "a", "Synthetic before.", true, 100)];
        spoken.follow(call, false, &[], &before, 1_000);
        assert!(spoken.lines.is_empty(), "no transcription runs");
        let started = [
            said("1", "a", "Synthetic before.", true, 100),
            said("2", "b", "Synthetic still", false, 200),
        ];
        spoken.follow(call, true, &before, &started, 2_000);
        let heard = [
            said("1", "a", "Synthetic before.", true, 100),
            said("2", "b", "Synthetic still going.", true, 200),
            said("3", "a", "Synthetic during.", true, 300),
        ];
        spoken.follow(call, true, &started, &heard, 3_000);
        let gap = [said("4", "b", "Synthetic between.", true, 400)];
        spoken.follow(call, false, &heard, &gap, 4_000);
        let again = [
            said("4", "b", "Synthetic between.", true, 400),
            said("5", "a", "Synthetic after.", true, 500),
        ];
        spoken.follow(call, true, &gap, &again, 5_000);
        let lines: Vec<(i64, &str)> = spoken
            .lines
            .iter()
            .map(|line| (line.at, line.caption.text.as_str()))
            .collect();
        assert_eq!(
            lines,
            [
                (2_000, "Synthetic still going."),
                (3_000, "Synthetic during."),
                (5_000, "Synthetic after.")
            ]
        );
    }
}
