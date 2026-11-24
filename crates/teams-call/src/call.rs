//! One call as the machine keeps it: who the account is in it, its callbacks, the links its services handed out, where
//! it is in its life, and the writes of it still waiting for their outcome.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use crate::{Delivery, Observed};
use teams_proto::Mri;
use teams_proto::{CallContext, CallLink, CallParticipant, Links, MediaContent};

use crate::agent::{CallAgent, Callback, Kind};
use crate::read::{Attached, Conversation, GroupMediaLinks, Invitation};
use crate::roster::Roster;
use crate::{CallId, CallState, CallWrite, End, WriteId};

/// The conversation's callbacks a start, a join or an `attach` hands out (`conversationRequest.links`, as captured).
pub(crate) const CONVERSATION_LINKS: [&str; 6] = [
    "conversationEnd",
    "conversationUpdate",
    "localParticipantUpdate",
    "addParticipantSuccess",
    "addParticipantFailure",
    // the live reactions of everyone in the call (seen 2026-11-06, the web client's own callbacks)
    "receiveMessage",
];

/// The call's callbacks a start or a join hands out (`callInvitation.links`).
pub(crate) const START_CALL_LINKS: [&str; 5] = ["progress", "mediaAnswer", "acceptance", "redirection", "end"];

/// The callbacks for the media controller (`clientContentForMediaController`).
pub(crate) const MEDIA_CONTROLLER_LINKS: [&str; 2] = ["controlVideoStreaming", "csrcInfo"];

/// The call's callbacks an acceptance's acknowledgement and a callee's `accept` hand out.
pub(crate) const ANSWERED_CALL_LINKS: [&str; 7] = [
    "mediaRenegotiation",
    "transfer",
    "replacement",
    "balanceUpdate",
    "retargetCompletion",
    "controlVideoStreaming",
    "updateMediaDescriptions",
];

/// Which way the call goes.
#[derive(Clone, Debug)]
pub(crate) enum Side {
    /// The account placed it: a chat's call, or the test call of the echo service (`Some`), added once accepted.
    Placed { echo: Option<Mri> },
    /// The account joined the running call at `conversation` in the group chat `chat`.
    Joined { chat: String, conversation: CallLink },
    /// The account went into a meeting's call, started or not: nobody rings, the acceptance is awaited as a join's.
    Meeting,
    /// The account was invited.
    Invited(Box<Invitation>),
}

/// Where the call is in its life (see [`crate::Calls`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// A start or a join sent, its answer awaited.
    Placing,
    /// A start answered: the callees ring, the acceptance awaited.
    Ringing,
    /// A join answered: the acceptance awaited.
    Joining,
    /// An invitation's receipt and `attach` sent, the `attach`'s answer awaited (nothing shown yet).
    Attaching,
    /// An invitation offered: it rings here, the account's answer or decline awaited.
    Offered,
    /// The account's `accept` sent.
    Answering,
    /// The account's decline sent.
    Declining,
    /// The call is up (signaling: the acceptance applied, or the `accept` answered).
    Connected,
    /// The account leaves: its `leave` sent (`sent`), or waiting for the conversation's `leave` link.
    Leaving { sent: bool, end: Ending },
}

/// Why the account leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ending {
    HungUp,
    Failed(Step),
    /// The call's media failed ([`crate::Calls::fail_media`]).
    Media,
    /// The far end closed the call's media and its end wasn't delivered ([`crate::Calls::media_closed`]).
    Closed,
    /// The test call's echo left once it played the recording back: the call is over, as the web client ends it.
    EchoLeft,
}

/// The name a call whose media failed ends with ([`End::Failed`]).
pub(crate) const MEDIA: &str = "media";

impl Ending {
    pub(crate) fn end(self) -> End {
        match self {
            Self::HungUp => End::HungUp,
            Self::Failed(step) => End::Failed(step.name()),
            Self::Media => End::Failed(MEDIA),
            Self::Closed | Self::EchoLeft => End::Remote(crate::RemoteEnd::default()),
        }
    }
}

impl Phase {
    /// The state a caller is told, if this phase is one.
    pub(crate) fn state(self) -> Option<CallState> {
        match self {
            Self::Placing => Some(CallState::Placing),
            Self::Ringing => Some(CallState::Ringing),
            Self::Joining => Some(CallState::Joining),
            Self::Offered => Some(CallState::Offered),
            Self::Answering => Some(CallState::Answering),
            Self::Declining => Some(CallState::Declining),
            Self::Connected => Some(CallState::Connected),
            Self::Leaving { .. } => Some(CallState::Leaving),
            Self::Attaching => None,
        }
    }
}

/// A write the machine waits for the outcome of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Start,
    /// A channel's Meet now's subscription: its answer is the new meeting, which the account then enters.
    Subscribe,
    Join,
    Attach,
    Ringing,
    EndpointState,
    Accept,
    Decline,
    Acknowledge,
    EndpointMetadata,
    Add,
    Leave,
    /// The account's renegotiation of the call's media.
    Renegotiate,
    /// The account's answer to a renegotiation delivered to it.
    MediaAnswer,
    /// The acknowledgement of the answer to the account's renegotiation.
    MediaAcknowledge,
    /// The call's keep-alive: its failing ends nothing (the web client sends the next one regardless).
    KeepAlive,
    /// A group call's media lines described again (the camera on or off): its failing ends nothing.
    Describe,
    /// A group call's send capabilities: its failing ends nothing.
    ChannelParameters,
    /// A meeting lobby's Admit or Deny: its failing ends nothing (the person stays waiting, as the roster tells).
    Lobby,
    /// The account's microphone muted or not, told to the call's conversation: its failing ends nothing (the others
    /// see the state they last saw).
    Muted,
    /// The account's hand raised or lowered: its failing ends nothing (the roster tells the hand as it stands).
    Hand,
    /// A live reaction: its failing ends nothing.
    React,
    /// The organizer's end of the meeting for everyone: its failing leaves the call instead.
    EndMeeting,
    /// The account's hold of the call: its answer hands out where it is taken back; its failing leaves the call as
    /// it was (not held).
    Hold,
    /// The account's call taken off hold: its failing leaves it held.
    Resume,
    /// The meeting's timer changed: its failing ends nothing (the timer stays as last told).
    MeetingStates,
    /// The meeting locked or unlocked: its failing ends nothing (the lock stays as last told).
    MeetingSettings,
    /// Everyone else muted by the organizer: its failing ends nothing (the roster tells who is muted).
    MuteAll,
    /// The account's own participant properties (that it shares its screen): its failing ends nothing.
    Properties,
    /// Someone added to the call: its failing ends nothing (told as the add's failure).
    Invite,
    /// The call's far end transferred to someone else: its failing ends nothing (the call stays as it was, held).
    Transfer,
    /// The transfer the far end asked for, taken: the account calls the target once it was.
    AcceptTransfer,
}

impl Step {
    /// The write's name ([`crate::CallWrite::name`]).
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Subscribe => "subscribe",
            Self::Join => "join",
            Self::Attach => "attach",
            Self::Ringing => "progress",
            Self::EndpointState | Self::Muted => "update_endpoint_state",
            Self::Accept => "accept",
            Self::Decline => "reject",
            Self::Acknowledge => "acknowledge",
            Self::EndpointMetadata => "update_endpoint_metadata",
            Self::Add | Self::Invite => "add",
            Self::Leave => "leave",
            Self::Renegotiate => "renegotiate",
            Self::MediaAnswer => "media_answer",
            Self::MediaAcknowledge => "media_acknowledge",
            Self::KeepAlive => "keep_alive",
            Self::Describe => "update_media_descriptions",
            Self::ChannelParameters => "apply_channel_parameters",
            Self::Lobby => "lobby_decision",
            Self::Hand => "hand",
            Self::React => "send_message",
            Self::EndMeeting => "end_conversation",
            Self::Hold => "hold",
            Self::Resume => "resume",
            Self::MeetingStates => "update_meeting_states",
            Self::MeetingSettings => "update_meeting_settings",
            Self::MuteAll => "mute",
            Self::Properties => "update_participant_properties",
            Self::Transfer => "transfer",
            Self::AcceptTransfer => "transfer_acceptance",
        }
    }
}

/// The keep-alive interval the call's services asked for, when the keep-alive next falls due (every 90 % of the
/// interval, as the web client sends it), and the call leg's own link it goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeepAlive {
    pub(crate) interval: Duration,
    pub(crate) next: Instant,
    pub(crate) link: Option<teams_proto::CallLink>,
}

impl KeepAlive {
    /// How often it goes: 90 % of the interval the services asked for (the web client's `scheduleKeepAlives`).
    pub(crate) fn period(&self) -> Duration {
        self.interval * 9 / 10
    }
}

/// The acknowledgement of a placed or joined call's acceptance: its delivery response went (or couldn't be built),
/// and the request that takes its place if it fails.
#[derive(Clone, Debug)]
pub(crate) struct Acknowledgement {
    pub(crate) delivery: Option<crate::DeliveryKey>,
    pub(crate) link: Option<teams_proto::CallLink>,
    pub(crate) links: Links,
    pub(crate) by_request: bool,
}

/// The account's renegotiation that waits for its answer: the paths of the `mediaAnswer` and `rejection` callbacks
/// its own request handed out. Every renegotiation hands out callbacks of its own, and an earlier one's stay the
/// call's, so only a delivery at one of these paths is this renegotiation's answer or rejection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reoffer {
    pub(crate) answer: String,
    pub(crate) rejection: String,
}

/// A renegotiation delivered to the account: its offer and the `mediaAnswer` link its answer goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Negotiation {
    pub(crate) offer: MediaContent,
    pub(crate) link: CallLink,
}

/// The music-on-hold app in a call's roster: its entry's version, whether it is in the call, and whether it plays for
/// the far end's hold (it came while the account held nothing) rather than the account's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Music {
    pub(crate) version: u64,
    pub(crate) present: bool,
    pub(crate) far_end: bool,
}

/// One call.
#[derive(Debug)]
pub(crate) struct Call {
    pub(crate) id: CallId,
    pub(crate) side: Side,
    pub(crate) phase: Phase,
    pub(crate) context: CallContext,
    pub(crate) caption_thread: Option<String>,
    pub(crate) caption_inviting: bool,
    /// The account in this call (its participant id fresh for the call).
    pub(crate) me: CallParticipant,
    agent: CallAgent,
    callbacks: Vec<Callback>,
    pub(crate) conversation: Conversation,
    /// An invitation's leg, from its `attach`'s answer.
    pub(crate) leg: Option<Attached>,
    pub(crate) acknowledgement: Option<Acknowledgement>,
    pub(crate) keep_alive: Option<KeepAlive>,
    /// The call's modalities, as its start, its join or its `accept` named them (a renegotiation names them again).
    pub(crate) modalities: Vec<String>,
    /// The call controller's `mediaRenegotiation` link, from the acceptance (a placed or joined call's) or the
    /// `accept`'s answer (an answered one's): where the account's renegotiation goes.
    pub(crate) renegotiation: Option<CallLink>,
    /// A group call's media links, from the same: where its lines are described again and its send capabilities go.
    pub(crate) group_media: GroupMediaLinks,
    /// The call leg's `hold` link, from the same (or a retarget): where the account puts the call on hold.
    pub(crate) hold: Option<CallLink>,
    /// While the account holds the call: the `resume` link its hold's answer handed out.
    pub(crate) resume: Option<CallLink>,
    /// The call leg's `transfer` link, from the same as `hold`: where the account transfers the far end.
    pub(crate) transfer: Option<CallLink>,
    /// The `add`s of people that went, by their write: whom, and the group chat it named.
    pub(crate) invites: BTreeMap<WriteId, (Mri, String)>,
    /// The transfer the far end asked for, while its acceptance goes.
    pub(crate) transferred: Option<crate::read::TransferAsked>,
    /// The music-on-hold app in the call's roster, as last delivered: whose hold it plays for.
    pub(crate) music: Option<Music>,
    /// Who is in the call, as its roster updates tell it.
    pub(crate) roster: Roster,
    /// The test call's echo was in its roster (it joined after its `add`): once it isn't any more, the test call is
    /// over.
    pub(crate) echo_joined: bool,
    /// A renegotiation delivered to the account and not answered yet: its offer and the `mediaAnswer` link its answer
    /// goes to.
    pub(crate) negotiation: Option<Negotiation>,
    /// The far end's renegotiation delivered while the account's was being made or waited for its answer: told once
    /// the account's settled (answered, turned away or given up), the media's SDP steps one after the other.
    pub(crate) deferred: Option<Negotiation>,
    /// Every renegotiation delivered to the account that it answered in this call (its answer built and queued): a
    /// delivery of one of them again is a replay, not a new one.
    pub(crate) negotiated: Vec<Negotiation>,
    /// The account's renegotiation that went and waits for its answer: the callbacks it handed out for that answer
    /// and its rejection. `None` while none waits.
    pub(crate) reoffer: Option<Reoffer>,
    /// The account's renegotiation began and its offer hasn't gone yet (`Calls::begin_renegotiation`).
    pub(crate) reoffering: bool,
    /// The offer of the account's last renegotiation, for it to go again once the call controller turned it away
    /// while another negotiation was in progress (`491`).
    pub(crate) offered: Option<MediaContent>,
    /// When the account's renegotiation turned away so goes again.
    pub(crate) retry: Option<Instant>,
    /// How many times the account's last renegotiation went again.
    pub(crate) retried: u8,
    pub(crate) writes: BTreeMap<WriteId, Step>,
    /// Writes that go one after another, each once the write before it (`held_after`) was answered `2xx`.
    pub(crate) held: VecDeque<(Step, CallWrite)>,
    pub(crate) held_after: Option<WriteId>,
    /// An acceptance delivered before the start's or the join's answer, applied once that answer came.
    pub(crate) early_acceptance: Option<(Delivery, Observed)>,
    /// A channel's Meet now whose subscription waits for its answer: the channel and the offer its entry goes with.
    pub(crate) meet_now: Option<(String, MediaContent)>,
    pub(crate) started: Instant,
    /// The endpoint state's reports so far (the start's, the join's or the answer's is the first): each mute tells the
    /// next.
    pub(crate) endpoint_states: u64,
    /// The states the account published or took back in the call so far (its raised hand): each counts the next.
    pub(crate) published: u64,
    /// What the meeting shares with everyone in it, as last told, and what its next changes name.
    pub(crate) meeting: crate::machine::Meeting,
    /// The meeting's organizer and tenant, when the call is a meeting's that named them: what its Request to join
    /// names.
    pub(crate) meeting_info: Option<teams_proto::MeetingInfo>,
    /// The account's `screenSharerAttributes` told so far: each Share tells the next.
    pub(crate) screen_shares: u64,
}

impl Call {
    pub(crate) fn new(
        id: CallId,
        side: Side,
        context: CallContext,
        me: CallParticipant,
        agent: CallAgent,
        started: Instant,
    ) -> Self {
        let phase = match side {
            Side::Invited(_) => Phase::Attaching,
            Side::Placed { .. } | Side::Joined { .. } | Side::Meeting => Phase::Placing,
        };
        Self {
            id,
            side,
            phase,
            context,
            caption_thread: None,
            caption_inviting: false,
            me,
            agent,
            callbacks: Vec::new(),
            conversation: Conversation::default(),
            leg: None,
            acknowledgement: None,
            keep_alive: None,
            modalities: Vec::new(),
            renegotiation: None,
            group_media: GroupMediaLinks::default(),
            hold: None,
            resume: None,
            transfer: None,
            invites: BTreeMap::new(),
            transferred: None,
            music: None,
            roster: Roster::default(),
            echo_joined: false,
            negotiation: None,
            deferred: None,
            negotiated: Vec::new(),
            reoffer: None,
            reoffering: false,
            offered: None,
            retry: None,
            retried: 0,
            writes: BTreeMap::new(),
            held: VecDeque::new(),
            held_after: None,
            early_acceptance: None,
            meet_now: None,
            started,
            endpoint_states: 1,
            published: 0,
            meeting: crate::machine::Meeting::default(),
            meeting_info: None,
            screen_shares: 0,
        }
    }

    /// A new callback of this call.
    pub(crate) fn callback(&mut self, kind: Kind, name: &'static str) -> Result<String, getrandom::Error> {
        Ok(self.made(kind, name)?.url)
    }

    /// A callback of this call under `segment` (a GUID the machine made: see [`CallAgent::callback_at`]), kept among
    /// its callbacks.
    pub(crate) fn callback_in(&mut self, kind: Kind, name: &'static str, segment: &str) -> String {
        let callback = self.agent.callback_at(kind, name, segment);
        self.callbacks.push(callback.clone());
        callback.url
    }

    /// A new callback of this call, kept among its callbacks.
    fn made(&mut self, kind: Kind, name: &'static str) -> Result<Callback, getrandom::Error> {
        let callback = self.agent.callback(kind, name)?;
        self.callbacks.push(callback.clone());
        Ok(callback)
    }

    /// New `mediaAnswer` and `rejection` callbacks for the account's renegotiation, as its request's links, and what
    /// it waits on once it went ([`Self::reoffer`]).
    pub(crate) fn reoffer_links(&mut self) -> Result<(Links, Reoffer), getrandom::Error> {
        let answer = self.made(Kind::Call, "mediaAnswer")?;
        let rejection = self.made(Kind::Call, "rejection")?;
        let links = Links::new()
            .with("mediaAnswer", answer.url)
            .with("rejection", rejection.url);
        Ok((
            links,
            Reoffer {
                answer: answer.path,
                rejection: rejection.path,
            },
        ))
    }

    /// Whether the account's renegotiation went and its answer hasn't come.
    pub(crate) fn waits_for_answer(&self) -> bool {
        self.reoffer.is_some()
    }

    /// New callbacks `kind`/`names` of this call, as a request's links.
    pub(crate) fn links(&mut self, kind: Kind, names: &[&'static str]) -> Result<Links, getrandom::Error> {
        names.iter().try_fold(Links::new(), |links, name| {
            Ok(links.with(*name, self.callback(kind, name)?))
        })
    }

    /// The callback a delivery at `path` came to, if it is one of this call's.
    pub(crate) fn callback_at(&self, path: &str) -> Option<(Kind, &'static str)> {
        self.callbacks
            .iter()
            .find(|callback| callback.path == path)
            .map(|callback| (callback.kind, callback.name))
    }

    /// The paths of this call's callbacks `kind`/`name`.
    pub(crate) fn callback_paths(&self, kind: Kind, name: &str) -> Vec<String> {
        self.callbacks
            .iter()
            .filter(|callback| callback.kind == kind && callback.name == name)
            .map(|callback| callback.path.clone())
            .collect()
    }

    /// The path of this call's callback `kind`/`name` (the first made).
    #[cfg(test)]
    pub(crate) fn path_of(&self, kind: Kind, name: &str) -> Option<&str> {
        self.callbacks
            .iter()
            .find(|callback| callback.kind == kind && callback.name == name)
            .map(|callback| callback.path.as_str())
    }

    /// The path of this call's callback `kind`/`name` made last: the one the latest request that made it handed out.
    #[cfg(test)]
    pub(crate) fn latest_path_of(&self, kind: Kind, name: &str) -> Option<&str> {
        self.callbacks
            .iter()
            .rev()
            .find(|callback| callback.kind == kind && callback.name == name)
            .map(|callback| callback.path.as_str())
    }

    /// The invitation this call answers, if it was invited.
    pub(crate) fn invitation(&self) -> Option<&Invitation> {
        match &self.side {
            Side::Invited(invitation) => Some(invitation),
            Side::Placed { .. } | Side::Joined { .. } | Side::Meeting => None,
        }
    }

    /// The running group call this call is in, as an invitation named it: its group chat and its conversation.
    pub(crate) fn running(&self) -> Option<(&str, &CallLink)> {
        match &self.side {
            Side::Joined { chat, conversation } => Some((chat, conversation)),
            Side::Invited(invitation) => invitation
                .group_chat
                .as_deref()
                .map(|chat| (chat, &invitation.conversation)),
            Side::Placed { .. } | Side::Meeting => None,
        }
    }

    /// Whether the call is in a group chat (its modalities are a group call's).
    pub(crate) fn in_group(&self, group_chat: Option<&str>) -> bool {
        group_chat.is_some()
            || self
                .invitation()
                .is_some_and(|invitation| invitation.group_chat.is_some())
    }

    /// Whether a `leave` may go for the call now: it has its conversation's `leave` link, and the account is in it
    /// (placed and answered by its services, or answered by the account).
    pub(crate) fn may_leave(&self) -> bool {
        let in_it = match self.phase {
            Phase::Ringing | Phase::Joining | Phase::Answering | Phase::Connected => true,
            Phase::Leaving { sent, .. } => !sent,
            Phase::Placing | Phase::Attaching | Phase::Offered | Phase::Declining => false,
        };
        in_it && self.conversation.leave.is_some()
    }

    /// How long it has rung, in whole seconds (a cancel names it).
    pub(crate) fn rang_seconds(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.started).as_secs()
    }
}
