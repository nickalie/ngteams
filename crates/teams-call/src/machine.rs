//! The machine ([`Calls`]): one call at a time, from the account's action or an invitation to its end.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use crate::{Delivery, Observed};
use base64::Engine as _;
use teams_proto::Mri;
use teams_proto::{
    AcceptanceAcknowledgement, CallContext, CallLink, CallParticipant, ChannelMeetup, ChannelParameters, ClientInfo,
    InvitationReceipt, MediaContent, MediaDescriptions, MeetingInfo,
};

use crate::agent::{CallAgent, Kind, guid};
use crate::call::{Acknowledgement, Call, Ending, KeepAlive, Music, Negotiation, Phase, Side, Step};
use crate::read::{self, Invitation, RemoteEnd};
use crate::roster::Roster;
use crate::{CallWrite, Participant, WriteId, build};

/// The modality a call's renegotiation and answer name beside the call's others once its media carries video (the
/// web client's camera-on renegotiation and the answer to it: `["Audio", "Video"]`).
const VIDEO: &str = "Video";
/// The modalities a 1:1 call's renegotiation and answer name while a screen is shared on its sharing line: the
/// sharer's (`["Audio", "ScreenSharer"]`) and the far end's that is shown it (`["Audio", "ScreenViewer"]`), seen
/// 2026-11-06 (`ab-call --call steps`: Share).
const SCREEN_SHARER: &str = "ScreenSharer";
const SCREEN_VIEWER: &str = "ScreenViewer";

/// Which end of a 1:1 call's shared screen the account is ([`Calls::set_screen`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// It shares its screen.
    Sharer,
    /// It is shown the far end's.
    Viewer,
}
/// How long after the call controller turned the account's renegotiation away (another negotiation in progress, `491`)
/// it goes again, and how many times at most.
pub const RENEGOTIATION_RETRY: Duration = Duration::from_secs(3);
const RENEGOTIATION_RETRIES: u8 = 5;

/// The account as its calls name it: its MRI and name, the calling endpoint it registered (`endpointId`, the
/// calling socket's `epid`), its language, its tenant, what the client says of itself, and the account's region and
/// partition (the call requests' `MS-Teams-Region` and `MS-Teams-Partition`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub mri: Mri,
    pub display_name: Option<String>,
    pub endpoint_id: String,
    pub language_id: Option<String>,
    pub tenant_id: String,
    pub client: ClientInfo,
    pub region: Option<String>,
    pub partition: Option<String>,
}

/// One call of the machine's life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CallId(u64);

impl CallId {
    /// The number, for a log line.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }

    /// Call `number`, for a test that tells a view of a call without a machine (`test-support`).
    #[cfg(feature = "test-support")]
    #[must_use]
    pub const fn for_tests(number: u64) -> Self {
        Self(number)
    }
}

/// What the account calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    /// The test call: a conversation of the account alone, the echo service `echo` added once the media controller
    /// accepted it; it ends by itself, with a `leave`, once the echo left its roster (`docs/protocol/calling-echo.md`).
    TestCall { echo: Mri },
    /// A call from a chat: a 1:1 chat's one callee (`group_chat` `None`), or a group chat and its other members.
    Chat {
        callees: Vec<Mri>,
        group_chat: Option<String>,
    },
    /// The call of the meeting whose chat is `chat`, whether it runs yet or not: nobody rings, and the service answers
    /// with the meeting's one conversation (`docs/protocol/calling.md`, "Joining a meeting").
    Meeting { chat: String, meeting: MeetingInfo },
    /// An invitation code resolved by the calling service when the user explicitly joins.
    MeetingCode(teams_proto::MeetingCode),
    /// A new meeting in the channel `channel` (its Meet now): nobody rings; the service answers with the meeting's
    /// conversation and posts the meeting to the channel (`docs/protocol/calling.md`, "A channel's meeting").
    Channel { channel: String },
    /// The call to `target` a transfer of the account's call asked for, naming who transferred it
    /// ([`CallEvent::Transferred`]): a 1:1 call, as the web client places it once it took the transfer.
    Transferred {
        target: Mri,
        transferor: teams_proto::Transferor,
    },
}

/// Where a call is, as the account is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallState {
    /// The account's start or join went out.
    Placing,
    /// The account's call rings at its callees.
    Ringing,
    /// The account's join of a running call was taken; its acceptance comes next.
    Joining,
    /// An invitation rings here ([`CallEvent::Incoming`]).
    Offered,
    /// The account's answer went out.
    Answering,
    /// The account's decline went out.
    Declining,
    /// The call is up, as signaling goes (the media comes up on its own).
    Connected,
    /// The account leaves.
    Leaving,
}

/// How a call ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// The account hung up (or cancelled its call while it rang).
    HungUp,
    /// The account declined the invitation.
    Declined,
    /// The call's services ended it, as delivered.
    Remote(RemoteEnd),
    /// A step failed: the write named didn't go or wasn't taken. The machine ended the call (and sent a `leave` when
    /// the account was in it).
    Failed(&'static str),
}

/// An invitation that rings here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incoming {
    pub call: CallId,
    pub caller: Mri,
    /// The caller's name as the invitation gives it (no profile lookup).
    pub caller_name: Option<String>,
    /// The group chat of a group call.
    pub group_chat: Option<String>,
    /// The caller's (or a group call's mixer's) offer.
    pub offer: Option<MediaContent>,
}

/// What the account is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallEvent {
    /// Built-in caption service metadata for this call (independent of local caption visibility).
    CaptionService {
        call: CallId,
        service: Option<teams_proto::captions::CaptionService>,
    },
    /// The caption service invitation failed; the voice call continues.
    CaptionFailed { call: CallId },
    /// An invitation rings here: answer it ([`Calls::answer`]) or decline it ([`Calls::decline`]); nothing answers it
    /// otherwise.
    Incoming(Incoming),
    /// An offered invitation's ring ended before the account answered or declined it (the caller cancelled, someone
    /// answered elsewhere, it timed out: as delivered). The call is over here.
    RingEnded { call: CallId, end: RemoteEnd },
    /// The call moved on.
    State { call: CallId, state: CallState },
    /// The answer to the account's offer (a placed or joined call's acceptance, or the answer to its renegotiation).
    RemoteMedia { call: CallId, media: MediaContent },
    /// The call's services renegotiate its media (a group call's mixer does, once the call is up): `offer` is to be
    /// answered with [`Calls::answer_media`], or the call fails ([`Calls::fail_media`]).
    MediaOffer { call: CallId, offer: MediaContent },
    /// The account's renegotiation, turned away while another negotiation was in progress (`491`), gave way to the
    /// far end's, told next ([`CallEvent::MediaOffer`]): no answer to the account's offer comes and it doesn't go
    /// again. The media answers the far end's as it stands, then offers what it would have offered again (a
    /// renegotiation begun as any, [`Calls::begin_renegotiation`]).
    RenegotiationGaveWay { call: CallId },
    /// The call ended. Every call shown ends with this or [`CallEvent::RingEnded`], once.
    Ended { call: CallId, end: End },
    /// The call's keep-alive fell due (every 90 % of the interval the call's services asked for): it went to the call
    /// leg's own link when the call has one (`sent`), else nothing went.
    KeepAliveDue { call: CallId, sent: bool },
    /// The account's call was taken by the callee's voicemail (as captured for a 1:1 call the callee declined or
    /// didn't answer: the call controller forwards it there): told once it is up. It runs on as any call up.
    Voicemail { call: CallId },
    /// Who is in the call now besides the account, as its roster updates tell it ([`Participant`]): told whenever
    /// that changed (someone came or left, turned their camera on or off).
    Participants {
        call: CallId,
        participants: Vec<Participant>,
    },
    /// Who waits in a meeting's lobby now, the account aside, as the roster tells it: told whenever that changed.
    /// Each is let in ([`Calls::decide_lobby`] with `admit`) or turned away by whoever may admit; the service turns away
    /// an Admit of anyone else.
    Lobby { call: CallId, waiting: Vec<Participant> },
    /// The account itself waits in the meeting's lobby (`waiting`), or no longer does (let in: the call goes on, or
    /// turned away: its end follows).
    InLobby { call: CallId, waiting: bool },
    /// The account's own entry in the roster changed: its hand raised or lowered ([`Calls::raise_hand`]), or it
    /// organizes the meeting (it may end it for everyone: [`Calls::end_meeting`]).
    Own {
        call: CallId,
        hand_raised: bool,
        organizer: bool,
    },
    /// The account's call on hold (`held`) or off it, as its Hold or Resume was taken (or, not taken, as it was).
    Held { call: CallId, held: bool },
    /// Someone the account added to its call wasn't brought in: the `add` wasn't taken, or its failure was delivered
    /// (they didn't answer, or had no endpoint to ring). Teams shows them "No response".
    AddFailed { call: CallId, who: Mri },
    /// The call is in the group chat `chat` from now on: a 1:1 call someone was added to (the account's `add` was
    /// taken, or a conversation update named it), a group call's from then on.
    GroupChat { call: CallId, chat: String },
    /// The account's transfer of its call wasn't taken: the call stays as it was.
    TransferFailed { call: CallId },
    /// The far end transferred the account's call to `target` and the account took it: the call is over here, and
    /// [`Placement::Transferred`] is the call to the target the web client places then.
    Transferred {
        call: CallId,
        target: Mri,
        transferor: teams_proto::Transferor,
    },
    /// The far end of the account's call put it on hold (`held`: the service's music-on-hold app came into its roster
    /// while the account held nothing) or took it off hold (the app left).
    HeldByFarEnd { call: CallId, held: bool },
    /// What the meeting shares with everyone in it changed: its timer, its lock, how to join it, a recording someone
    /// runs ([`MeetingState`]), as its conversation's updates and its roster tell it.
    Meeting { call: CallId, state: MeetingState },
    /// Someone in the call sent a live reaction (`like`, `heart`, `applause`, `laugh`, `surprised`), the account's own
    /// as the service echoes it too: who, their name as given, and the reaction.
    Reaction {
        call: CallId,
        from: Mri,
        name: Option<String>,
        reaction: String,
    },
}

/// The Trouter delivery a response answers: its connection and its id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DeliveryKey {
    pub connection: String,
    pub id: i64,
}

impl DeliveryKey {
    fn of(delivery: &Delivery) -> Self {
        Self {
            connection: delivery.connection.clone(),
            id: delivery.id,
        }
    }
}

/// A typed delivery response, to go on the connection the delivery came on (through that delivery's responder).
#[derive(Clone, Debug)]
pub enum DeliveryReply {
    /// An offered invitation's receipt.
    Receipt(InvitationReceipt),
    /// The acknowledgement of a placed or joined call's acceptance.
    Acknowledgement(AcceptanceAcknowledgement),
}

/// What the engine does for the machine, in order.
#[derive(Clone, Debug)]
pub enum Effect {
    /// Send `write` through the outbox, and report its outcome under `id` ([`Calls::on_outcome`]).
    Send { id: WriteId, write: Box<CallWrite> },
    /// Answer `delivery` with `reply`; report a failure ([`Calls::on_response_failed`]).
    Respond {
        delivery: DeliveryKey,
        reply: DeliveryReply,
    },
    /// Tell the account.
    Event(CallEvent),
    /// The invitation whose `attach` link is `attach` was let go here before it rang (its `attach` failed for good, its
    /// answer didn't read, the call's media failed, its ring ended, the engine stopped): release the tracker
    /// ([`crate::track::Tracker::release`]), which ends its offer, so it holds no call open. Its service may never have taken
    /// the callbacks that `attach` handed out, so no delivery would end it there.
    LetGo { attach: CallLink },
}

/// How a write went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The service answered with `status` and `body`.
    Answered { status: u16, body: Vec<u8> },
    /// It wasn't sent (call admission or the transport refused it) or its outcome isn't known (a timeout, a broken
    /// connection). It is never sent again.
    Failed,
}

impl Outcome {
    /// The body of a `2xx` answer.
    fn body(&self) -> Option<&[u8]> {
        match self {
            Self::Answered { status, body } if (200..300).contains(status) => Some(body),
            _ => None,
        }
    }
}

/// Why an action of the account's was refused. Nothing was sent.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    /// A call is up (one call at a time).
    #[error("a call is already up")]
    Busy,
    /// The calling endpoint's push URL isn't known yet ([`Calls::set_push_url`]).
    #[error("the calling endpoint isn't up yet")]
    NoEndpoint,
    /// No such call is up.
    #[error("no such call is up")]
    NoSuchCall,
    /// The call can't take that action where it is.
    #[error("the call can't do that now")]
    NotNow,
    /// No running call is known in that chat.
    #[error("no running call is known in that chat")]
    NotJoinable,
    /// The request doesn't match a supported captured call shape.
    #[error(transparent)]
    Write(#[from] teams_proto::CallWriteError),
    /// The system's random source failed.
    #[error("the system's random source failed: {0}")]
    Random(getrandom::Error),
}

impl From<getrandom::Error> for CallError {
    fn from(error: getrandom::Error) -> Self {
        Self::Random(error)
    }
}

/// The effects waiting for the engine, and the write ids handed out.
#[derive(Debug, Default)]
struct Out {
    effects: VecDeque<Effect>,
    next_write: u64,
    /// The `leave`s and declines of calls that ended here before their outcome came: cleanup writes the engine may
    /// still send again ([`Calls::may_send_again`]), until their outcome comes.
    detached: BTreeSet<WriteId>,
}

impl Out {
    fn send(&mut self, call: &mut Call, step: Step, write: CallWrite) -> WriteId {
        self.next_write += 1;
        let id = WriteId(self.next_write);
        call.writes.insert(id, step);
        self.effects.push_back(Effect::Send {
            id,
            write: Box::new(write),
        });
        id
    }

    /// `writes` one after another: the first now, each next only once the one before it was answered `2xx`
    /// ([`Self::release`]). A step that fails ends the call, and the writes after it never go.
    fn send_in_turn(&mut self, call: &mut Call, writes: Vec<(Step, CallWrite)>) {
        let mut writes = writes.into_iter();
        let Some((step, write)) = writes.next() else { return };
        let first = self.send(call, step, write);
        call.held = writes.collect();
        call.held_after = (!call.held.is_empty()).then_some(first);
    }

    /// The write `answered` was answered `2xx`: the call's next held write goes, if it waited for that one.
    fn release(&mut self, call: &mut Call, answered: WriteId) {
        if call.held_after != Some(answered) {
            return;
        }
        call.held_after = None;
        if let Some((step, write)) = call.held.pop_front() {
            let next = self.send(call, step, write);
            call.held_after = (!call.held.is_empty()).then_some(next);
        }
    }

    /// The call's writes that weren't handed out yet, and those held, never go: the call leaves or is over. A
    /// `leave` already queued still goes.
    fn drop_queued(&mut self, call: &mut Call) {
        call.held.clear();
        call.held_after = None;
        let writes = &mut call.writes;
        self.effects.retain(|effect| {
            let Effect::Send { id, .. } = effect else {
                return true;
            };
            match writes.get(id) {
                None | Some(Step::Leave) => true,
                Some(_) => {
                    writes.remove(id);
                    false
                }
            }
        });
    }

    fn event(&mut self, event: CallEvent) {
        self.effects.push_back(Effect::Event(event));
    }

    fn state(&mut self, call: &Call) {
        if let Some(state) = call.phase.state() {
            self.event(CallEvent::State { call: call.id, state });
        }
    }

    fn respond(&mut self, delivery: DeliveryKey, reply: DeliveryReply) {
        self.effects.push_back(Effect::Respond { delivery, reply });
    }
}

/// The calls of one account: one at a time (see the crate docs).
#[derive(Debug)]
pub struct Calls {
    identity: Identity,
    agent: Option<CallAgent>,
    call: Option<Call>,
    /// The running calls invitations named, by their group chat: the conversation a join goes to (or the chat's own
    /// start named, [`Self::posted_running`]). An entry goes when its conversation's end is delivered, when a join to
    /// it fails, or when a newer invitation in the chat names another.
    running: BTreeMap<String, CallLink>,
    /// The rosters of running calls an invitation's `attach` subscribed to, by their group chat: who is in the call,
    /// kept from the ring on (see [`Followed`]).
    followed: BTreeMap<String, Followed>,
    next_call: u64,
    /// The number [`Self::reserve`] handed out for the next start or join, until one takes it.
    reserved: Option<u64>,
    out: Out,
}

/// A running group call's roster as the `attach` of its invitation subscribed to it: the service goes on delivering
/// its updates at that subscription's callbacks after the ring ended, and a join later gets only the changes from
/// then on (at both the `attach`'s callbacks and its own, as the web client's captures show), so the join starts
/// from this roster and takes the updates at those callbacks too. Read only to show who is in the call.
#[derive(Clone, Debug)]
struct Followed {
    conversation: CallLink,
    roster: Roster,
    /// The `attach`'s `rosterUpdate` callback paths.
    paths: Vec<String>,
}

impl Calls {
    #[must_use]
    pub fn new(identity: Identity) -> Self {
        Self {
            identity,
            agent: None,
            call: None,
            running: BTreeMap::new(),
            followed: BTreeMap::new(),
            next_call: 0,
            reserved: None,
            out: Out::default(),
        }
    }

    /// Whether the account's tenant is known (the test call's `add` names it).
    #[must_use]
    pub fn knows_tenant(&self) -> bool {
        !self.identity.tenant_id.is_empty()
    }

    /// The account's tenant, once it is read (an engine that couldn't name it when it was built reads it from the
    /// account's token before its first test call). An empty `tenant` changes nothing.
    pub fn set_tenant(&mut self, tenant: String) {
        if !tenant.is_empty() {
            self.identity.tenant_id = tenant;
        }
    }

    /// The calling endpoint's push URL (its `trouter.connected`'s `surl`): calls from now on hand out callbacks under
    /// a call agent of their own there. A call that is up keeps its own.
    ///
    /// # Errors
    /// [`CallError::NoEndpoint`] for a push URL that isn't `https://<host>/v4/f/<endpoint>/`.
    pub fn set_push_url(&mut self, push_url: &str) -> Result<(), CallError> {
        let agent = CallAgent::new(push_url, &guid()?).ok_or(CallError::NoEndpoint)?;
        self.agent = Some(agent);
        Ok(())
    }

    /// The next thing the engine does, oldest first.
    pub fn poll(&mut self) -> Option<Effect> {
        self.out.effects.pop_front()
    }

    /// The next thing the engine does that isn't a write, oldest first: for an engine with a write of the machine's in
    /// flight, which takes the next write only once that one's outcome came. The writes stay in the machine's own
    /// queue, in order, so a call that ends drops those not taken yet.
    pub fn poll_beside_write(&mut self) -> Option<Effect> {
        let at = self
            .out
            .effects
            .iter()
            .position(|effect| !matches!(effect, Effect::Send { .. }))?;
        self.out.effects.remove(at)
    }

    /// Whether the engine may send `write` once more, after an attempt the service refused without acting on it (a
    /// 401, a throttled one): only while its call still wants it. A call that leaves (or ended) wants no write but its
    /// `leave` or decline, and a cleanup write of a call that ended here goes on until its outcome came.
    #[must_use]
    pub fn may_send_again(&self, write: WriteId) -> bool {
        let Some(call) = self.call.as_ref() else {
            return self.out.detached.contains(&write);
        };
        match call.writes.get(&write) {
            Some(Step::Leave | Step::Decline) => true,
            Some(_) => !matches!(call.phase, Phase::Leaving { .. } | Phase::Declining),
            None => self.out.detached.contains(&write),
        }
    }

    /// The call that is up, and where it is.
    #[must_use]
    pub fn current(&self) -> Option<(CallId, Option<CallState>)> {
        self.call.as_ref().map(|call| (call.id, call.phase.state()))
    }

    /// The current call only when it owns this exact service callback. Old background subscriptions do not match.
    #[must_use]
    pub fn call_at(&self, path: &str) -> Option<CallId> {
        let call = self.call.as_ref()?;
        let followed = call.running().is_some_and(|(chat, conversation)| {
            self.followed.get(chat).is_some_and(|followed| {
                followed.conversation == *conversation && followed.paths.iter().any(|known| known == path)
            })
        });
        (call.callback_at(path).is_some() || followed).then_some(call.id)
    }

    /// The group chat `call` is in, while it is up and is a group call's: a group chat's call, or a 1:1 call someone
    /// was added to ([`CallEvent::GroupChat`]). `None` for a 1:1 call, a meeting's, or another call.
    #[must_use]
    pub fn group_chat(&self, call: CallId) -> Option<String> {
        let current = self.call.as_ref().filter(|current| current.id == call)?;
        current
            .caption_thread
            .clone()
            .filter(|chat| teams_proto::is_group_chat_id(chat))
    }

    /// The far end of the 1:1 call `call`, while it is up: the other member of its 1:1 chat. `None` for a group call, a
    /// meeting's, the test call, or another call.
    #[must_use]
    pub fn far_end(&self, call: CallId) -> Option<Mri> {
        let current = self
            .call
            .as_ref()
            .filter(|current| current.id == call && current.phase == Phase::Connected)?;
        let pair = teams_proto::one_to_one_pair(current.caption_thread.as_deref()?)?;
        pair.into_iter().find(|member| *member != self.identity.mri)
    }

    /// The account, as its calls name it.
    #[must_use]
    pub fn me(&self) -> &Mri {
        &self.identity.mri
    }

    /// The keep-alive interval the call's services asked for, once the call is up.
    #[must_use]
    pub fn keep_alive_interval(&self) -> Option<Duration> {
        self.call
            .as_ref()
            .and_then(|call| call.keep_alive.as_ref())
            .map(|keep| keep.interval)
    }

    /// When [`Self::tick`] next has something to do.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        let call = self.call.as_ref()?;
        let keep = call.keep_alive.as_ref().map(|keep| keep.next);
        let retry = call.retry;
        keep.into_iter().chain(retry).min()
    }

    /// Whether an invitation named a running call in `chat` that [`Self::join`] can join. It says what the machine
    /// was told, not that the call still runs: a call whose end the machine wasn't delivered (once the ring ended
    /// here it hears no more of the call) stays joinable until a join to it fails, which forgets it.
    #[must_use]
    pub fn joinable(&self, chat: &str) -> bool {
        self.running.contains_key(chat)
    }

    /// The group chat `chat`'s call runs at `conversation`, as the chat's own start message says (a group chat's
    /// meeting, which rings nobody: `teams_proto::running_call`): [`Self::join`] goes there. The chat's newest start
    /// is newer than what an earlier invitation named.
    pub fn posted_running(&mut self, chat: &str, conversation: CallLink) {
        if teams_proto::is_group_chat_id(chat) {
            self.running.insert(chat.to_owned(), conversation);
        }
    }

    /// The account places a call to `placement` with `offer`.
    ///
    /// # Errors
    /// [`CallError`]; nothing was sent.
    pub fn place(&mut self, placement: &Placement, offer: MediaContent, now: Instant) -> Result<CallId, CallError> {
        let side = match placement {
            Placement::TestCall { echo } => Side::Placed {
                echo: Some(echo.clone()),
            },
            Placement::Chat { .. } | Placement::Transferred { .. } => Side::Placed { echo: None },
            Placement::Meeting { .. } | Placement::MeetingCode(_) | Placement::Channel { .. } => Side::Meeting,
        };
        let mut call = self.new_call(side, now)?;
        call.caption_thread = match placement {
            Placement::Chat {
                group_chat: Some(chat), ..
            }
            | Placement::Meeting { chat, .. } => Some(chat.clone()),
            Placement::Chat {
                callees,
                group_chat: None,
            } if callees.len() == 1 => teams_proto::one_to_one_id(self.identity.mri.as_str(), callees[0].as_str()),
            Placement::Channel { channel } => Some(channel.clone()),
            Placement::Transferred { target, .. } => {
                teams_proto::one_to_one_id(self.identity.mri.as_str(), target.as_str())
            }
            _ => None,
        };
        if let Placement::Channel { channel } = placement {
            call.meet_now = Some((channel.clone(), offer.clone()));
            let write = build::start(&mut call, &self.identity, placement, offer)?;
            return Ok(self.begin(call, Step::Subscribe, write));
        }
        let write = build::start(&mut call, &self.identity, placement, offer)?;
        Ok(self.begin(call, Step::Start, write))
    }

    /// A channel's Meet now's subscription was answered with its new meeting's conversation: the account enters it as
    /// its organizer, with the offer it kept (as the web client's Join now does: the channel, no post yet, the account
    /// and its tenant as the meeting's organizer, the meeting posted to the channel). An answer naming no conversation
    /// fails the call.
    ///
    /// A call hung up (or failed) while its subscription was unanswered ends here instead: the account never entered
    /// the new meeting, so there is nothing to leave, and entering it now would post the meeting to the channel the
    /// user cancelled it in.
    fn subscribed(&mut self, body: &[u8], now: Instant) {
        if let Some(Phase::Leaving { end, .. }) = self.call.as_ref().map(|call| call.phase) {
            self.finish(end.end());
            return;
        }
        let entry = read::conversation_controller(body).zip(self.call.as_mut().and_then(|call| call.meet_now.take()));
        let Some((conversation, (channel, offer))) = entry else {
            self.fail(Step::Subscribe, now);
            return;
        };
        let Some(oid) = self.identity.mri.as_str().strip_prefix("8:orgid:").map(str::to_owned) else {
            self.fail(Step::Subscribe, now);
            return;
        };
        let into = build::JoinInto {
            chat: &channel,
            message_id: None,
            meeting: Some(MeetingInfo {
                organizer_id: oid,
                tenant_id: self.identity.tenant_id.clone(),
            }),
            posts_meetup: true,
        };
        let Some(call) = self.call.as_mut() else { return };
        call.side = Side::Joined {
            chat: channel.clone(),
            conversation: conversation.clone(),
        };
        match build::join(call, &self.identity, &conversation, into, offer) {
            Ok(write) => {
                self.out.send(call, Step::Join, write);
            }
            Err(_) => self.fail(Step::Subscribe, now),
        }
    }

    /// The account joins the running call in the group chat `chat` (one an invitation named) with `offer`.
    ///
    /// # Errors
    /// [`CallError`]; nothing was sent.
    pub fn join(&mut self, chat: &str, offer: MediaContent, now: Instant) -> Result<CallId, CallError> {
        let conversation = self.running.get(chat).cloned().ok_or(CallError::NotJoinable)?;
        let side = Side::Joined {
            chat: chat.to_owned(),
            conversation: conversation.clone(),
        };
        let mut call = self.new_call(side, now)?;
        call.caption_thread = Some(chat.to_owned());
        if let Some(followed) = self
            .followed
            .get(chat)
            .filter(|followed| followed.conversation == conversation)
        {
            call.roster = followed.roster.seeded();
        }
        let into = build::JoinInto {
            chat,
            message_id: Some("0"),
            meeting: None,
            posts_meetup: false,
        };
        let write = build::join(&mut call, &self.identity, &conversation, into, offer)?;
        Ok(self.begin(call, Step::Join, write))
    }

    /// The account joins the meeting `meetup` that runs in the channel `channel` (its Join, as the web client's from the
    /// channel's post: a join at the meeting's conversation naming the channel, the post and the meeting's organizer,
    /// `docs/protocol/calling.md`, "A channel's meeting"), with `offer`.
    ///
    /// # Errors
    /// [`CallError`]; nothing was sent.
    pub fn join_channel_meeting(
        &mut self,
        channel: &str,
        meetup: &ChannelMeetup,
        offer: MediaContent,
        now: Instant,
    ) -> Result<CallId, CallError> {
        let side = Side::Joined {
            chat: channel.to_owned(),
            conversation: meetup.conversation.clone(),
        };
        let mut call = self.new_call(side, now)?;
        call.caption_thread = Some(channel.to_owned());
        let into = build::JoinInto {
            chat: channel,
            message_id: Some(&meetup.message_id),
            meeting: Some(meetup.meeting.clone()),
            posts_meetup: false,
        };
        let write = build::join(&mut call, &self.identity, &meetup.conversation, into, offer)?;
        Ok(self.begin(call, Step::Join, write))
    }

    fn new_call(&mut self, side: Side, now: Instant) -> Result<Call, CallError> {
        if self.call.is_some() {
            return Err(CallError::Busy);
        }
        let agent = self.agent.clone().ok_or(CallError::NoEndpoint)?;
        let context = CallContext::fresh()?.with_region(self.identity.region.clone(), self.identity.partition.clone());
        let me = self.participant()?;
        let number = self.reserved.take().unwrap_or_else(|| {
            self.next_call += 1;
            self.next_call
        });
        Ok(Call::new(CallId(number), side, context, me, agent, now))
    }

    /// Numbers the call the next start or join ([`Self::place`], [`Self::join`], [`Self::join_channel_meeting`]) will
    /// be, so it can be told before its media is set up and its first write goes. A later reservation replaces one no
    /// start took; an invitation that rings meanwhile is numbered after it.
    pub fn reserve(&mut self) -> CallId {
        self.next_call += 1;
        self.reserved = Some(self.next_call);
        CallId(self.next_call)
    }

    fn participant(&self) -> Result<CallParticipant, getrandom::Error> {
        Ok(CallParticipant {
            mri: self.identity.mri.clone(),
            display_name: self.identity.display_name.clone(),
            endpoint_id: self.identity.endpoint_id.clone(),
            participant_id: Some(guid()?),
            language_id: self.identity.language_id.clone(),
        })
    }

    fn begin(&mut self, mut call: Call, step: Step, write: CallWrite) -> CallId {
        let id = call.id;
        self.out.send(&mut call, step, write);
        self.out.state(&call);
        self.call = Some(call);
        id
    }

    /// The account answers the invitation `call` rings with, its `answer` to the offer: its endpoint state, then (once
    /// that was taken) its `accept`.
    ///
    /// # Errors
    /// [`CallError`]; nothing was sent.
    pub fn answer(&mut self, call: CallId, answer: MediaContent) -> Result<(), CallError> {
        let current = ringing(&mut self.call, call)?;
        let writes = build::answer(current, &self.identity, answer)?;
        self.out.send_in_turn(current, writes);
        current.phase = Phase::Answering;
        self.out.state(current);
        Ok(())
    }

    /// The account declines the invitation `call` rings with: the ring stops here and on the account's other
    /// endpoints. As captured (`docs/protocol/calling.md`, "The decline"), in a 1:1 call the call controller then
    /// forwards the call to the account's voicemail, so the caller's call connects to it; in a group call the caller
    /// is told this callee failed to join.
    ///
    /// # Errors
    /// [`CallError`]; nothing was sent.
    pub fn decline(&mut self, call: CallId) -> Result<(), CallError> {
        let current = ringing(&mut self.call, call)?;
        let write = build::decline(current)?;
        self.out.send(current, Step::Decline, write);
        current.phase = Phase::Declining;
        self.out.state(current);
        Ok(())
    }

    /// The account renegotiates the media of `call`, up, with its new `offer`: to the call controller's
    /// `mediaRenegotiation` link; the answer comes as [`CallEvent::RemoteMedia`] (and is acknowledged), a rejection or
    /// a failed renegotiation fails the call.
    ///
    /// A renegotiation the account began ([`Self::begin_renegotiation`]) goes on here; one it didn't is judged as that
    /// would judge it.
    ///
    /// # Errors
    /// [`CallError`]: the call isn't up, has no renegotiation link, or a renegotiation (either way) is under way;
    /// nothing was sent.
    pub fn renegotiate(&mut self, call: CallId, offer: &MediaContent) -> Result<(), CallError> {
        let begun = self
            .call
            .as_ref()
            .is_some_and(|current| current.id == call && current.reoffering);
        let current = if begun {
            connected(&mut self.call, call)?
        } else {
            renegotiable(&mut self.call, call)?
        };
        let link = current.renegotiation.clone().ok_or(CallError::NotNow)?;
        let (write, reoffer) = build::renegotiate(current, &link, offer)?;
        self.out.send(current, Step::Renegotiate, write);
        current.reoffering = false;
        current.offered = Some(offer.clone());
        current.retry = None;
        current.retried = 0;
        // only now: the callbacks a renegotiation that wasn't built made stay the call's, but nothing waits on them
        current.reoffer = Some(reoffer);
        Ok(())
    }

    /// The account begins a renegotiation of the media of `call`, up, before its offer is made (a media session's new
    /// offer replaces the one it waits on, so nothing may make one the machine wouldn't take): until the offer goes
    /// ([`Self::renegotiate`]) or is given up ([`Self::give_up_renegotiation`]) the call takes no other renegotiation.
    /// The account's second one is refused (as one is while the account's waits for its answer or to go again after a
    /// `491`), and the far end's waits until the account's settled (told once its answer came, it was turned away or
    /// given up).
    ///
    /// # Errors
    /// [`CallError`]: the call isn't up, has no renegotiation link, or a renegotiation (either way) is under way;
    /// nothing changed.
    pub fn begin_renegotiation(&mut self, call: CallId) -> Result<(), CallError> {
        renegotiable(&mut self.call, call)?.reoffering = true;
        Ok(())
    }

    /// The account's renegotiation of `call` it began ([`Self::begin_renegotiation`]) won't go (its offer couldn't be
    /// made): another may begin, and the far end's that came meanwhile is told.
    pub fn give_up_renegotiation(&mut self, call: CallId) {
        if let Some(current) = self.call.as_mut().filter(|current| current.id == call) {
            current.reoffering = false;
            self.tell_deferred();
        }
    }

    /// The account describes the media lines of `call`, up, a group call's, again (its camera turned on or off: its
    /// main video line `sendrecv` or `recvonly`, each receiving line `recvonly`; numbered on from the descriptions its
    /// start's, join's, `accept`'s and answers' media carried, as the media numbers them): to the call controller's
    /// `updateMediaDescriptions` link, and then, when given, its send capabilities to its `applyChannelParameters` link.
    /// Neither failing ends the call.
    ///
    /// # Errors
    /// [`CallError`]: the call can't be described now ([`Self::describable`]), or either write can't be built; nothing
    /// was sent.
    pub fn describe_media(
        &mut self,
        call: CallId,
        described: &MediaDescriptions,
        parameters: Option<ChannelParameters>,
    ) -> Result<(), CallError> {
        let current = describable(&mut self.call, call)?;
        let link = current.group_media.descriptions.clone().ok_or(CallError::NotNow)?;
        let write = build::describe(current, &link, described, None)?;
        // both built before either goes: a send capability that can't be built leaves the descriptions unsent too
        let parameters = match (parameters, current.group_media.channel_parameters.clone()) {
            (Some(parameters), Some(link)) => Some(build::channel_parameters(current, &link, &parameters)?),
            _ => None,
        };
        self.out.send(current, Step::Describe, write);
        if let Some(write) = parameters {
            self.out.send(current, Step::ChannelParameters, write);
        }
        Ok(())
    }

    /// The account's screen goes out in `call`, up, a group call's, or no longer: its media lines described again as
    /// `described` says (the sharing line `sendonly` and labelled, or `recvonly` once it stops), tagged as the web
    /// client tags a share's (`<its participant id>;ss_<n>`, `n` the share's `screenSharerAttributes` number), at the
    /// call controller's `updateMediaDescriptions` link. Nothing is renegotiated; its failing ends nothing.
    ///
    /// # Errors
    /// [`CallError`]: the call can't be described now ([`Self::describable`]), or the write can't be built; nothing was
    /// sent.
    pub fn describe_share(&mut self, call: CallId, described: &MediaDescriptions) -> Result<(), CallError> {
        let current = describable(&mut self.call, call)?;
        let link = current.group_media.descriptions.clone().ok_or(CallError::NotNow)?;
        // the capture's one share was `ss_1` both ways, its attributes' number 1: each share the number it was told by
        let tag = current
            .me
            .participant_id
            .as_ref()
            .map(|id| format!("{id};ss_{}", current.screen_shares.max(1)));
        let write = build::describe(current, &link, described, tag.as_deref())?;
        self.out.send(current, Step::Describe, write);
        Ok(())
    }

    /// Whether the media lines of `call` can be described now ([`Self::describe_media`]): it is up and connected (as
    /// signaling goes), nothing renegotiates it, and its services handed out an `updateMediaDescriptions` link.
    ///
    /// # Errors
    /// [`CallError`]: it isn't the call up, or can't be described now.
    pub fn describable(&mut self, call: CallId) -> Result<(), CallError> {
        describable(&mut self.call, call).map(|_| ())
    }

    /// Whether the media of `call` carries video now (a video line in use, either way): its next renegotiation and
    /// answer name the `Video` modality beside the call's others, as the web client's do once a camera is on.
    pub fn set_video(&mut self, call: CallId, video: bool) {
        let Some(current) = self.call.as_mut().filter(|current| current.id == call) else {
            return;
        };
        let named = current.modalities.iter().any(|modality| modality == VIDEO);
        if video && !named {
            current.modalities.push(VIDEO.to_owned());
        } else if !video && named {
            current.modalities.retain(|modality| modality != VIDEO);
        }
    }

    /// Which end of a shared screen the account is in `call`, a 1:1 call, now (`None`: no screen is shared): its next
    /// renegotiation and answer name `ScreenSharer` or `ScreenViewer` beside the call's other modalities, as the web
    /// client's do while a screen is shared, and neither once it stopped.
    pub fn set_screen(&mut self, call: CallId, screen: Option<Screen>) {
        let Some(current) = self.call.as_mut().filter(|current| current.id == call) else {
            return;
        };
        current
            .modalities
            .retain(|modality| modality != SCREEN_SHARER && modality != SCREEN_VIEWER);
        match screen {
            Some(Screen::Sharer) => current.modalities.push(SCREEN_SHARER.to_owned()),
            Some(Screen::Viewer) => current.modalities.push(SCREEN_VIEWER.to_owned()),
            None => {}
        }
    }

    /// The account tells `call`'s conversation it shares its screen (`screenSharerAttributes` at the conversation's
    /// `updateParticipantProperties` link), as the web client's Share does before its renegotiation adds the sharing
    /// line; its failing ends nothing.
    ///
    /// # Errors
    /// [`CallError::NoSuchCall`] for another call, [`CallError::NotNow`] while the call isn't up or its conversation
    /// named no such link; nothing was sent.
    pub fn tell_screen_shared(&mut self, call: CallId) -> Result<(), CallError> {
        let current = up(&mut self.call, call)?;
        let link = current
            .conversation
            .participant_properties
            .clone()
            .ok_or(CallError::NotNow)?;
        let write = build::screen_sharer(current, &link)?;
        self.out.send(current, Step::Properties, write);
        Ok(())
    }

    /// The account's `answer` to the renegotiation of `call` told as [`CallEvent::MediaOffer`].
    ///
    /// # Errors
    /// [`CallError`]: no renegotiation of that call waits for an answer; nothing was sent.
    pub fn answer_media(&mut self, call: CallId, answer: MediaContent) -> Result<(), CallError> {
        let current = connected(&mut self.call, call)?;
        let negotiation = current.negotiation.clone().ok_or(CallError::NotNow)?;
        let write = build::answer_media(current, &negotiation.link, answer)?;
        current.negotiation = None;
        self.out.send(current, Step::MediaAnswer, write);
        // answered once its answer is queued, whatever that answer's outcome: a replay of it is never answered again
        current.negotiated.push(negotiation);
        Ok(())
    }
}

/// The call `call`, if it is the one up and connected (as signaling goes) and nothing renegotiates it.
fn connected(current: &mut Option<Call>, call: CallId) -> Result<&mut Call, CallError> {
    let current = current
        .as_mut()
        .filter(|current| current.id == call)
        .ok_or(CallError::NoSuchCall)?;
    if current.phase == Phase::Connected && !current.waits_for_answer() {
        Ok(current)
    } else {
        Err(CallError::NotNow)
    }
}

/// The call `call`, if it is up and connected (as signaling goes), nothing renegotiates it, and it has an
/// `updateMediaDescriptions` link.
fn describable(current: &mut Option<Call>, call: CallId) -> Result<&mut Call, CallError> {
    let current = connected(current, call)?;
    if current.group_media.descriptions.is_some() {
        Ok(current)
    } else {
        Err(CallError::NotNow)
    }
}

/// The call `call`, if it is up and connected (as signaling goes), has a renegotiation link, and no renegotiation of
/// it is under way either way (the account's begun, waiting for its answer or to go again after a `491`, the far
/// end's waiting for the account's answer): the media's offer that a `491` turned away still waits for its answer
/// when it goes again, and a new offer would replace it.
fn renegotiable(current: &mut Option<Call>, call: CallId) -> Result<&mut Call, CallError> {
    let current = connected(current, call)?;
    if current.renegotiation.is_some()
        && current.negotiation.is_none()
        && !current.reoffering
        && current.retry.is_none()
    {
        Ok(current)
    } else {
        Err(CallError::NotNow)
    }
}

/// The offered invitation `call`, if it is `current`.
fn ringing(current: &mut Option<Call>, call: CallId) -> Result<&mut Call, CallError> {
    let current = current
        .as_mut()
        .filter(|current| current.id == call)
        .ok_or(CallError::NoSuchCall)?;
    if current.phase == Phase::Offered && current.leg.is_some() {
        Ok(current)
    } else {
        Err(CallError::NotNow)
    }
}

/// Hanging up, leaving, failing and the outcomes of writes.
impl Calls {
    /// The account lets `waiting` in from the meeting's lobby (`admit`) or turns them away: an admit at the meeting
    /// conversation's `admit` link, a removal at its `removeParticipant` link, as the web client's Admit and Deny send
    /// them. Only someone the roster tells waiting ([`CallEvent::Lobby`]), in a meeting's call the account is in.
    ///
    /// # Errors
    /// [`CallError::NoSuchCall`] for another call, [`CallError::NotNow`] when nobody by that MRI waits or the
    /// conversation named no such link; nothing was sent.
    pub fn decide_lobby(&mut self, call: CallId, waiting: &Mri, admit: bool) -> Result<(), CallError> {
        let current = self
            .call
            .as_mut()
            .filter(|current| current.id == call)
            .ok_or(CallError::NoSuchCall)?;
        let waits = current.roster.waiting().iter().any(|person| &person.mri == waiting);
        let link = if admit {
            current.conversation.admit.clone()
        } else {
            current.conversation.remove.clone()
        };
        let (true, Phase::Connected | Phase::Joining, Some(link)) = (waits, current.phase, link) else {
            return Err(CallError::NotNow);
        };
        let write = build::lobby(current, &link, waiting, admit)?;
        self.out.send(current, Step::Lobby, write);
        Ok(())
    }

    /// The account's microphone muted (`muted`) or not, told to the call's conversation at its `updateEndpointState`
    /// link, as the web client tells it each time it mutes and unmutes (seen 2026-11-06): the others' rosters show it.
    ///
    /// # Errors
    /// [`CallError::NoSuchCall`] for another call, [`CallError::NotNow`] while the call isn't up or its conversation
    /// named no such link; nothing was sent.
    pub fn set_muted(&mut self, call: CallId, muted: bool) -> Result<(), CallError> {
        let current = up(&mut self.call, call)?;
        let link = current.conversation.endpoint_state.clone().ok_or(CallError::NotNow)?;
        let write = build::muted(current, &link, muted)?;
        self.out.send(current, Step::Muted, write);
        Ok(())
    }

    /// The account raises its hand (`raised`) at the conversation's `publishState` link, or lowers it at its
    /// `removeState` link by the id its roster entry gives it, as the web client's Raise and Lower hand send them.
    ///
    /// # Errors
    /// [`CallError::NoSuchCall`] for another call, [`CallError::NotNow`] while the call isn't up, its conversation named
    /// no such link, or (lowering) no hand of the account's is up; nothing was sent.
    pub fn raise_hand(&mut self, call: CallId, raised: bool) -> Result<(), CallError> {
        let me = self.identity.mri.clone();
        let current = up(&mut self.call, call)?;
        let up = current.roster.own(&me).hand;
        let (link, lowered) = match (raised, up) {
            (true, _) => (current.conversation.publish_state.clone(), None),
            (false, Some(state)) => (current.conversation.remove_state.clone(), Some(state)),
            (false, None) => return Err(CallError::NotNow),
        };
        let link = link.ok_or(CallError::NotNow)?;
        let write = build::hand(current, &link, lowered.as_deref())?;
        self.out.send(current, Step::Hand, write);
        Ok(())
    }

    /// The account sends the live reaction `reaction` to everyone in the call at the conversation's `sendMessage`
    /// link.
    ///
    /// # Errors
    /// [`CallError::NoSuchCall`] for another call, [`CallError::NotNow`] while the call isn't up or its conversation
    /// named no such link, [`CallError::Write`] for a reaction the call doesn't take; nothing was sent.
    pub fn react(&mut self, call: CallId, reaction: &str) -> Result<(), CallError> {
        let current = up(&mut self.call, call)?;
        let link = current.conversation.send_message.clone().ok_or(CallError::NotNow)?;
        let write = build::reaction(current, &link, reaction)?;
        self.out.send(current, Step::React, write);
        Ok(())
    }

    /// The account ends the meeting for everyone in it, as its organizer's End meeting: a `DELETE` at the
    /// conversation's own URL. Only the organizer's (its roster entry says so).
    ///
    /// # Errors
    /// [`CallError::NoSuchCall`] for another call, [`CallError::NotNow`] while the call isn't up, the account doesn't
    /// organize it or the conversation isn't known; nothing was sent.
    pub fn end_meeting(&mut self, call: CallId) -> Result<(), CallError> {
        let me = self.identity.mri.clone();
        let current = up(&mut self.call, call)?;
        if !current.roster.own(&me).organizer {
            return Err(CallError::NotNow);
        }
        let link = current.conversation.controller.clone().ok_or(CallError::NotNow)?;
        let write = build::end_meeting(current, &link)?;
        self.out.send(current, Step::EndMeeting, write);
        Ok(())
    }

    /// The account puts its 1:1 call on hold (`held`) at the call leg's `hold` link, or takes it off hold at the
    /// `resume` link the hold's answer handed out, as the web client's Hold and Resume send them (seen 2026-11-06): on
    /// hold the far end hears the service's music and the call controller moves the call to its mixer
    /// ([`CallEvent::Held`] tells both).
    ///
    /// # Errors
    /// [`CallError::NoSuchCall`] for another call, [`CallError::NotNow`] while the call isn't up, is already as asked,
    /// or its leg named no such link; nothing was sent. Teams offers Hold in a 1:1 call only: the view offers no more.
    pub fn hold(&mut self, call: CallId, held: bool) -> Result<(), CallError> {
        let current = up(&mut self.call, call)?;
        let (link, step) = match (held, current.resume.clone()) {
            (true, None) => (current.hold.clone().ok_or(CallError::NotNow)?, Step::Hold),
            (false, Some(resume)) => (resume, Step::Resume),
            _ => return Err(CallError::NotNow),
        };
        let write = build::hold(current, &link, held)?;
        self.out.send(current, step, write);
        Ok(())
    }

    /// The account adds `invitee` to its call, as Teams' People pane does (seen 2026-11-06): the conversation's `add`
    /// naming the call's `group_chat` (a 1:1 call's new group chat of the three, which the account made first; a group
    /// call's own). Once it is taken the call is a group call in that chat ([`CallEvent::GroupChat`]); the invitee
    /// rings, and one who doesn't come in is told ([`CallEvent::AddFailed`]).
    ///
    /// # Errors
    /// [`CallError::NoSuchCall`] for another call, [`CallError::NotNow`] while it isn't up or names no `add` link, a
    /// write error for an invitee or chat out of the captured shape; nothing was sent.
    pub fn add(&mut self, call: CallId, invitee: &Mri, group_chat: &str) -> Result<(), CallError> {
        let current = up(&mut self.call, call)?;
        let link = current.conversation.add.clone().ok_or(CallError::NotNow)?;
        let write = build::add_person(current, &link, invitee, group_chat)?;
        let id = self.out.send(current, Step::Invite, write);
        current.invites.insert(id, (invitee.clone(), group_chat.to_owned()));
        Ok(())
    }

    /// The account transfers its 1:1 call's far end to `target`, as Teams' Transfer to an audio call does (seen
    /// 2026-11-06): the call leg's `transfer`. Once the far end took it (its `transferAcceptance` delivered) the account
    /// leaves the call; one not taken is told ([`CallEvent::TransferFailed`]) and the call stays as it was.
    ///
    /// # Errors
    /// [`CallError::NoSuchCall`] for another call, [`CallError::NotNow`] while it isn't up or its leg named no
    /// `transfer` link, a write error for a target out of the captured shape; nothing was sent.
    pub fn transfer(&mut self, call: CallId, target: &Mri) -> Result<(), CallError> {
        let current = up(&mut self.call, call)?;
        let link = current.transfer.clone().ok_or(CallError::NotNow)?;
        let write = build::transfer(current, &link, target)?;
        self.out.send(current, Step::Transfer, write);
        Ok(())
    }

    /// The account hangs up `call`: cancels it while it rings at its callees, leaves it once it is up. An invitation
    /// that rings here is declined instead ([`Self::decline`]), and so is one being answered whose `accept` hasn't
    /// gone out yet (the account isn't in the call before it, and only a decline ends the ring).
    ///
    /// # Errors
    /// [`CallError`]; nothing was sent.
    pub fn hang_up(&mut self, call: CallId, now: Instant) -> Result<(), CallError> {
        let current = self
            .call
            .as_mut()
            .filter(|current| current.id == call)
            .ok_or(CallError::NoSuchCall)?;
        match current.phase {
            Phase::Leaving { .. } => Ok(()),
            Phase::Attaching | Phase::Offered | Phase::Declining => Err(CallError::NotNow),
            Phase::Placing => {
                self.leave_once_started(Ending::HungUp);
                Ok(())
            }
            Phase::Answering => {
                // built first, so a refusal leaves everything as it was
                let decline = build::decline(current)?;
                self.out.drop_queued(current);
                if current.writes.values().any(|step| *step == Step::Accept) {
                    self.leave(Ending::HungUp, now);
                } else {
                    // the `accept` never went (held for the endpoint state's answer, or not taken yet): the account
                    // isn't in the call and the tracker still holds the invitation as ringing, so a decline ends it
                    self.out.send(current, Step::Decline, decline);
                    current.phase = Phase::Declining;
                    self.out.state(current);
                }
                Ok(())
            }
            Phase::Ringing | Phase::Joining | Phase::Connected => {
                self.leave(Ending::HungUp, now);
                Ok(())
            }
        }
    }

    /// A call whose start or join is still unanswered leaves for `ending` once it is: the conversation's `leave` link
    /// comes with that answer, and a start or join the service took is always followed by its `leave`
    /// ([`Self::started`]). The call stays until then; a start or join that fails ends it with `ending`.
    fn leave_once_started(&mut self, ending: Ending) {
        let Some(call) = self.call.as_mut() else { return };
        call.phase = Phase::Leaving {
            sent: false,
            end: ending,
        };
        self.out.state(call);
    }

    /// The call leaves for `ending`: its `leave` sent when it may go, else the call ends here at once.
    fn leave(&mut self, ending: Ending, now: Instant) {
        let Some(call) = self.call.as_mut() else { return };
        let write = if call.may_leave() {
            build::leave(call, now).ok()
        } else {
            None
        };
        match write {
            Some(write) => {
                self.out.drop_queued(call);
                self.out.send(call, Step::Leave, write);
                call.phase = Phase::Leaving {
                    sent: true,
                    end: ending,
                };
                self.out.state(call);
            }
            None => self.finish(ending.end()),
        }
    }

    /// The call is over here: none of its writes not handed out yet goes, but a `leave`; its cleanup writes still
    /// waiting for their outcome stay the engine's to finish ([`Self::may_send_again`]).
    fn end_call(&mut self) -> Option<Call> {
        let mut call = self.call.take()?;
        self.follow(&call);
        self.out.drop_queued(&mut call);
        let cleanup = call
            .writes
            .iter()
            .filter(|(_, step)| matches!(step, Step::Leave | Step::Decline))
            .map(|(id, _)| *id);
        self.out.detached.extend(cleanup);
        Some(call)
    }

    /// The invitation being attached ([`Phase::Attaching`]) goes away here unseen, and the engine tells the tracker
    /// to let it go ([`Effect::LetGo`]).
    fn let_go(&mut self) {
        if let Some(call) = self.end_call()
            && let Some(invitation) = call.invitation()
        {
            self.out.effects.push_back(Effect::LetGo {
                attach: invitation.attach.clone(),
            });
        }
    }

    /// The call ends here with `end`.
    fn finish(&mut self, end: End) {
        if let Some(call) = self.end_call() {
            self.out.event(CallEvent::Ended { call: call.id, end });
        }
    }

    /// The running group call the call is in isn't joinable any more (its conversation ended, or a join to it
    /// failed), unless a newer invitation named another in its chat.
    fn forget_running(&mut self) {
        let Some((chat, conversation)) = self.call.as_ref().and_then(Call::running) else {
            return;
        };
        if self.running.get(chat) == Some(conversation) {
            let chat = chat.to_owned();
            self.running.remove(&chat);
            self.followed.remove(&chat);
        }
    }

    /// An invitation to a running group call that ends here (its ring ended, or it was declined or answered and left)
    /// leaves its `attach`'s roster followed while the call runs and is joinable.
    fn follow(&mut self, call: &Call) {
        let Some(invitation) = call.invitation() else { return };
        let Some(chat) = invitation.group_chat.as_ref() else {
            return;
        };
        if self.running.get(chat) != Some(&invitation.conversation) {
            return;
        }
        let paths = call.callback_paths(Kind::Conversation, "rosterUpdate");
        if paths.is_empty() {
            return;
        }
        let followed = Followed {
            conversation: invitation.conversation.clone(),
            roster: call.roster.clone(),
            paths,
        };
        self.followed.insert(chat.clone(), followed);
    }

    /// A roster update at a followed `attach`'s callback: the joined call's when the account is in it, otherwise
    /// kept for a join to come; anything else at no callback of the call's is dropped.
    fn followed_roster(&mut self, delivery: &Delivery) {
        let Some((chat, followed)) = self
            .followed
            .iter_mut()
            .find(|(_, followed)| followed.paths.contains(&delivery.path))
        else {
            return;
        };
        let joined = self
            .call
            .as_ref()
            .and_then(Call::running)
            .is_some_and(|(running, conversation)| running == chat && *conversation == followed.conversation);
        if let Some(update) = read::roster(&delivery.body) {
            let _ = followed.roster.apply(update, &self.identity.mri);
        }
        if joined {
            self.roster_updated(delivery);
        }
    }

    /// A step failed: the call leaves if it may and ends; an invitation not shown yet goes away unseen. A failed join
    /// forgets the running call it went to.
    fn fail(&mut self, step: Step, now: Instant) {
        if step == Step::Join {
            self.forget_running();
        }
        // an answer turned into a decline by a hang-up: its endpoint state failing changes nothing, the decline goes
        // on and its own outcome ends the call
        let declining = self.call.as_ref().map(|call| call.phase) == Some(Phase::Declining);
        if declining && step != Step::Decline {
            return;
        }
        self.abandon(Ending::Failed(step), now);
    }

    /// The call fails for `ending`: it leaves if it may and ends here at once (its `leave` goes out unwatched); an
    /// invitation not shown yet goes away unseen. A call that was leaving already before its start or join was
    /// answered ends for what it was leaving for.
    fn abandon(&mut self, ending: Ending, now: Instant) {
        match self.call.as_ref().map(|call| call.phase) {
            Some(Phase::Attaching) => self.let_go(),
            Some(Phase::Leaving { sent: false, end }) => self.leave(end, now),
            Some(_) => self.leave(ending, now),
            None => {}
        }
        if let Some(Phase::Leaving { sent: true, end }) = self.call.as_ref().map(|call| call.phase) {
            self.finish(end.end());
        }
    }

    /// The call `call`'s media failed (it can't be set up, the far end's SDP doesn't apply, the media never
    /// connects or is lost): the call fails as a failed step does ([`End::Failed`] `"media"`), with a `leave` when the
    /// account is in it. A call whose start or join is still unanswered waits for that answer and leaves then, as a
    /// hang-up does: the service may have taken it. Nothing when `call` isn't the call that is up, or it is ending
    /// already.
    pub fn fail_media(&mut self, call: CallId, now: Instant) {
        let phase = self
            .call
            .as_ref()
            .filter(|current| current.id == call)
            .map(|current| current.phase);
        match phase {
            None | Some(Phase::Leaving { .. } | Phase::Declining) => {}
            Some(Phase::Placing) => self.leave_once_started(Ending::Media),
            Some(Phase::Offered) => {
                // not answered yet: the account isn't in it, and only its own decline would end the ring
                if let Some(ended) = self.end_call() {
                    self.out.event(CallEvent::RingEnded {
                        call: ended.id,
                        end: RemoteEnd::default(),
                    });
                }
            }
            Some(_) => self.abandon(Ending::Media, now),
        }
    }

    /// The far end closed the call `call`'s media, and the call's own end wasn't delivered in the grace the engine gives
    /// it (a far end's hang-up closes the media first): the far end left. A call that is up ends as the far end's end
    /// ([`End::Remote`], nothing read into it), with a `leave` so nothing of the account's leg is left behind; a call
    /// in any other phase as [`Self::fail_media`] does.
    pub fn media_closed(&mut self, call: CallId, now: Instant) {
        let phase = self
            .call
            .as_ref()
            .filter(|current| current.id == call)
            .map(|current| current.phase);
        match phase {
            Some(Phase::Connected) => self.abandon(Ending::Closed, now),
            _ => self.fail_media(call, now),
        }
    }

    /// The engine stops: the call that is up is hung up as [`Self::hang_up`] does (a `leave`, or the decline of an
    /// answer whose `accept` hasn't gone), and an invitation that rings here (or isn't shown yet) is let go here
    /// unanswered: nothing is sent for it, its ring goes on at the account's other endpoints.
    pub fn shut_down(&mut self, now: Instant) {
        let Some((id, phase)) = self.call.as_ref().map(|call| (call.id, call.phase)) else {
            return;
        };
        match phase {
            Phase::Attaching => self.let_go(),
            Phase::Offered => {
                drop(self.end_call());
                self.out.event(CallEvent::RingEnded {
                    call: id,
                    end: RemoteEnd::default(),
                });
            }
            Phase::Declining | Phase::Leaving { .. } => {}
            Phase::Placing | Phase::Ringing | Phase::Joining | Phase::Answering | Phase::Connected => {
                let _ = self.hang_up(id, now);
            }
        }
    }

    /// How a write went (see [`Effect::Send`]). An outcome of a call that ended is dropped.
    pub fn on_outcome(&mut self, write: WriteId, outcome: &Outcome, now: Instant) {
        self.out.detached.remove(&write);
        let Some(call) = self.call.as_mut() else { return };
        let Some(step) = call.writes.remove(&write) else {
            return;
        };
        let body = outcome.body();
        if body.is_some() {
            self.out.release(call, write);
        }
        match (step, body) {
            (Step::Start | Step::Join, Some(body)) => self.started(step, body, now),
            (Step::Subscribe, Some(body)) => self.subscribed(body, now),
            (Step::Attach, Some(body)) => self.attached(body),
            (Step::Accept, Some(body)) => self.connected_by_accept(body, now),
            (Step::Decline, Some(_)) => self.finish(End::Declined),
            (Step::Leave, Some(_)) => self.left(),
            // the meeting ended for everyone: the call is over here (the conversation's end follows)
            (Step::EndMeeting, Some(_)) => self.finish(End::HungUp),
            // not ended for everyone: the account leaves it alone, as a hang-up does
            (Step::EndMeeting, None) => self.leave(Ending::HungUp, now),
            (Step::Hold | Step::Resume, body) => self.held(step, body),
            (Step::Invite, body) => self.invited_person(write, body.is_some()),
            (Step::Transfer, None) => {
                let id = call.id;
                self.out.event(CallEvent::TransferFailed { call: id });
            }
            (Step::AcceptTransfer, body) => self.transfer_taken(body.is_some()),
            // a keep-alive not taken ends nothing: the next one goes when it falls due, as the web client's does; a
            // group call's media described again and not taken leaves the call as it was (the camera goes unseen);
            // an in-call control not taken (mute, hand, reaction, the meeting's states, settings or properties, mute
            // all, the lobby, the channel parameters) is no reason to end the call: it stays as it was
            (_, Some(_))
            | (
                Step::KeepAlive
                | Step::Describe
                | Step::ChannelParameters
                | Step::Lobby
                | Step::Muted
                | Step::Hand
                | Step::React
                | Step::MeetingStates
                | Step::MeetingSettings
                | Step::MuteAll
                | Step::Properties,
                None,
            ) => {}
            (step, None) => self.fail(step, now),
        }
    }

    /// The hold's or the resume's outcome (`body` its answer when taken): the call held while the hold's answer handed
    /// out where it is taken back, off hold once the resume was taken; told either way, as it stands.
    fn held(&mut self, step: Step, body: Option<&[u8]>) {
        let Some(call) = self.call.as_mut() else { return };
        match (step, body) {
            (Step::Hold, Some(body)) => call.resume = read::resume_link(body),
            (Step::Resume, Some(_)) => call.resume = None,
            _ => {}
        }
        let (id, held) = (call.id, call.resume.is_some());
        self.out.event(CallEvent::Held { call: id, held });
    }

    /// The outcome of the `add` `write` of someone (`taken` or not): taken, the call is in its group chat from now on;
    /// not taken, the invitee wasn't brought in.
    fn invited_person(&mut self, write: WriteId, taken: bool) {
        let Some(call) = self.call.as_mut() else { return };
        let Some((who, chat)) = call.invites.remove(&write) else {
            return;
        };
        if taken {
            in_group_chat(call, &mut self.out, chat);
        } else {
            let id = call.id;
            self.out.event(CallEvent::AddFailed { call: id, who });
        }
    }

    /// The far end's transfer, taken (`taken`) or not: taken, the call is over here and the account calls the target
    /// ([`CallEvent::Transferred`], the old call's own end left to the far end, as the web client leaves it); not taken,
    /// the call goes on as it was.
    fn transfer_taken(&mut self, taken: bool) {
        let Some(call) = self.call.as_mut() else { return };
        let Some(asked) = call.transferred.take() else {
            return;
        };
        if !taken {
            log::info!("call {}: its transfer wasn't taken", call.id.get());
            return;
        }
        let id = call.id;
        self.out.event(CallEvent::Transferred {
            call: id,
            target: asked.target,
            transferor: asked.transferor,
        });
        self.finish(End::Remote(RemoteEnd::default()));
    }

    /// A transfer the far end asked for, at the call's `transfer` callback: taken at once, as the web client takes it
    /// (seen 2026-11-06); one that doesn't read (a consultative transfer, not captured) or comes while the call isn't up
    /// changes nothing.
    fn transfer_asked(&mut self, delivery: &Delivery) {
        let Some(call) = self.call.as_mut().filter(|call| call.phase == Phase::Connected) else {
            return;
        };
        let Some(asked) = read::transfer_asked(&delivery.body) else {
            log::info!("call {}: a transfer it doesn't read", call.id.get());
            return;
        };
        if call.transferred.is_some() {
            return;
        }
        match build::accept_transfer(call, &asked.acceptance) {
            Ok(write) => {
                call.transferred = Some(asked);
                self.out.send(call, Step::AcceptTransfer, write);
            }
            Err(error) => log::warn!("call {}: its transfer can't be taken: {error}", call.id.get()),
        }
    }

    /// The `leave` was taken: the call is over here.
    fn left(&mut self) {
        let end = match self.call.as_ref().map(|call| call.phase) {
            Some(Phase::Leaving { end, .. }) => end.end(),
            _ => End::HungUp,
        };
        self.finish(end);
    }

    /// A start's or a join's answer: the conversation it opened.
    fn started(&mut self, step: Step, body: &[u8], now: Instant) {
        let Some(conversation) = read::conversation(body) else {
            self.fail(step, now);
            return;
        };
        let Some(call) = self.call.as_mut() else { return };
        call.conversation = conversation;
        self.meeting_answered(body);
        let Some(call) = self.call.as_mut() else { return };
        // a meeting's call that runs already: who is in it comes only with this answer (its updates tell changes)
        if let Some(update) = read::answered_roster(body) {
            if let Some(participants) = call.roster.apply(update, &self.identity.mri) {
                let id = call.id;
                self.out.event(CallEvent::Participants { call: id, participants });
            }
            lobby_told(call, &mut self.out, &self.identity.mri);
        }
        match call.phase {
            Phase::Leaving { sent: false, end } => {
                call.phase = Phase::Ringing;
                self.leave(end, now);
            }
            Phase::Placing => {
                call.phase = match call.side {
                    Side::Joined { .. } | Side::Meeting => Phase::Joining,
                    Side::Placed { .. } | Side::Invited(_) => Phase::Ringing,
                };
                self.out.state(call);
                if let Some((delivery, observed)) = call.early_acceptance.take() {
                    self.accepted(&delivery, observed, now);
                }
            }
            _ => {}
        }
    }
}

/// Deliveries, responses and time.
impl Calls {
    /// A call delivery as the tracker read it (`teams_trouter::CallDelivery`'s `delivery` and `observed`).
    pub fn on_delivery(&mut self, delivery: &Delivery, observed: Observed, now: Instant) {
        if observed == Observed::Offered && !delivery.path.contains("/callAgent/") {
            self.invited(delivery, now);
            return;
        }
        let Some(callback) = self.call.as_ref().and_then(|call| call.callback_at(&delivery.path)) else {
            self.followed_roster(delivery);
            return;
        };
        if observed == Observed::Renegotiation {
            // a renegotiation or its answer, at the call's own callback, gets its receipt at once, as the web client
            // gives it (M3 Task 18): without one the service delivers it again every few seconds and holds the call's
            // next negotiation (the acknowledgement answered after 22 s, a renegotiation meanwhile rejected)
            self.receipt(delivery);
        }
        match callback {
            (Kind::Call, "acceptance") => self.accepted(delivery, observed, now),
            (Kind::Call, "end") => self.ended(delivery),
            (Kind::Conversation, "conversationEnd") => {
                self.forget_running();
                self.ended(delivery);
            }
            (Kind::Call, "mediaRenegotiation") => self.renegotiated(delivery, observed, now),
            (Kind::Call, "mediaAnswer") => self.media_answered(delivery, now),
            (Kind::Call, "mediaAcknowledgement") => self.retargeted(delivery, observed),
            (Kind::Call, "rejection") => self.reoffer_rejected(delivery, now),
            (Kind::Conversation, "rosterUpdate") => {
                self.roster_updated(delivery);
                self.echo_present(now);
            }
            (Kind::Conversation, "receiveMessage") => self.message_received(delivery),
            (Kind::Conversation, "conversationUpdate") => {
                self.conversation_updated(delivery);
                // a 1:1 call someone was added to: its new group chat named
                if let (Some(call), Some(chat)) = (self.call.as_mut(), read::update_group_chat(&delivery.body)) {
                    in_group_chat(call, &mut self.out, chat);
                }
            }
            (Kind::Conversation, "addParticipantFailure") => {
                if let Some(call) = self.call.as_mut().filter(|call| call.caption_inviting) {
                    call.caption_inviting = false;
                    self.out.event(CallEvent::CaptionFailed { call: call.id });
                }
                self.add_failed(delivery);
            }
            (Kind::Call, "transfer") => self.transfer_asked(delivery),
            // the far end took the account's transfer: the account leaves, as the web client does
            (Kind::Call, "transferAcceptance")
                if self.call.as_ref().is_some_and(|call| call.phase == Phase::Connected) =>
            {
                self.leave(Ending::HungUp, now);
            }
            // progress and conversation updates
            _ => {}
        }
    }

    /// A renegotiation of the call's media delivered to the account: told, to be answered ([`Self::answer_media`]).
    /// One the tracker didn't read (it binds no link for the answer) or that doesn't read, or one while the call isn't
    /// up or the account answers another, fails the call: no capture shows how one is refused.
    ///
    /// One while the account's renegotiation is being made or waits for its answer (both ends turned their cameras on
    /// together: the call controller turns away the one that reached it second) waits until the account's settled
    /// (`deferred`): the media's offer waits for its answer, which a new answer of the media's would leave without its
    /// offer. One while the account's waits to go again after a `491` goes first: the account's gives way.
    ///
    /// A delivery of the renegotiation that waits for the account's answer, or of one the account answered, again (its
    /// answer link and its offer both the same) is a replay (deliveries aren't answered, so the service delivers them
    /// again): nothing. A renegotiation the far end means to send again as it was, on the same answer link, can't be
    /// told from such a replay and is taken as one; a different offer on a link seen before is a new renegotiation.
    fn renegotiated(&mut self, delivery: &Delivery, observed: Observed, now: Instant) {
        let Some(call) = self.call.as_mut() else { return };
        let read = read::negotiation(&delivery.body)
            .filter(|_| observed == Observed::Renegotiation)
            .map(|(offer, link)| Negotiation { offer, link });
        if let Some(negotiation) = &read {
            let replay = call.negotiation.as_ref() == Some(negotiation)
                || call.deferred.as_ref() == Some(negotiation)
                || call.negotiated.contains(negotiation);
            if replay {
                return;
            }
        }
        let up = call.phase == Phase::Connected && call.negotiation.is_none();
        match read {
            Some(negotiation) if up && (call.reoffering || call.waits_for_answer()) => {
                call.deferred = Some(negotiation);
            }
            Some(negotiation) if up => {
                // the account's turned away and waiting to go again gives way to it
                if call.retry.take().is_some() {
                    let id = call.id;
                    self.out.event(CallEvent::RenegotiationGaveWay { call: id });
                }
                call.deferred = Some(negotiation);
                self.tell_deferred();
            }
            _ if matches!(call.phase, Phase::Leaving { .. }) => {}
            _ => self.fail(Step::MediaAnswer, now),
        }
    }

    /// The acknowledgement of the account's media answer, read only for a retarget ([`read::retarget`]): a meeting's
    /// call moved to another call id once the account was let in from its lobby, and the call's own links (its
    /// renegotiation's, its keep-alive's call leg, a group call's media links) follow it. The renegotiations after it
    /// name the new call's answer links, which the tracker bound from the same delivery. Anything else, or one the
    /// tracker didn't read, changes nothing.
    fn retargeted(&mut self, delivery: &Delivery, observed: Observed) {
        if observed != Observed::Renegotiation {
            return;
        }
        let Some(call) = self.call.as_mut() else { return };
        let Some(retarget) = read::retarget(&delivery.body) else {
            return;
        };
        if retarget.renegotiation.is_some() {
            call.renegotiation = retarget.renegotiation;
        }
        if retarget.hold.is_some() {
            call.hold = retarget.hold;
        }
        if retarget.transfer.is_some() {
            call.transfer = retarget.transfer;
        }
        if let Some(keep_alive) = call.keep_alive.as_mut() {
            keep_alive.link = Some(retarget.call_leg);
        }
        if retarget.group_media.descriptions.is_some() {
            call.group_media.descriptions = retarget.group_media.descriptions;
        }
        if retarget.group_media.channel_parameters.is_some() {
            call.group_media.channel_parameters = retarget.group_media.channel_parameters;
        }
    }

    /// The answer to the account's renegotiation: told as the call's remote media, and acknowledged where it hands out
    /// a link. Nothing while no renegotiation of the account's waits for one, and nothing for a delivery at an earlier
    /// renegotiation's callback (a replay of an answer that came): only the waiting one's own callback is its answer.
    fn media_answered(&mut self, delivery: &Delivery, now: Instant) {
        let Some(call) = self.call.as_mut() else { return };
        if call
            .reoffer
            .as_ref()
            .is_none_or(|reoffer| reoffer.answer != delivery.path)
        {
            return;
        }
        call.reoffer = None;
        let Some((media, acknowledgement)) = read::media_answer(&delivery.body) else {
            self.fail(Step::Renegotiate, now);
            return;
        };
        let id = call.id;
        if let Some(Ok(write)) = acknowledgement.map(|link| build::acknowledge_media(call, &link)) {
            self.out.send(call, Step::MediaAcknowledge, write);
        }
        self.out.event(CallEvent::RemoteMedia { call: id, media });
        // the far end's that came meanwhile, after the account's answer is applied
        self.tell_deferred();
    }

    /// The far end's renegotiation that waited for the account's to settle, told now (the call up and nothing else
    /// waiting for the account's answer).
    fn tell_deferred(&mut self) {
        let Some(call) = self.call.as_mut() else { return };
        if call.phase != Phase::Connected || call.negotiation.is_some() {
            return;
        }
        let Some(negotiation) = call.deferred.take() else {
            return;
        };
        let offer = negotiation.offer.clone();
        call.negotiation = Some(negotiation);
        let id = call.id;
        self.out.event(CallEvent::MediaOffer { call: id, offer });
    }

    /// The account's renegotiation was rejected: the call fails (its media waits for an answer that won't come), but
    /// for a renegotiation turned away while another negotiation was in progress (`491`): the far end's delivered
    /// meanwhile goes first, the account's giving way to it ([`CallEvent::RenegotiationGaveWay`]); with none, the
    /// account's goes again [`RENEGOTIATION_RETRY`] later (at most [`RENEGOTIATION_RETRIES`] times), as the web
    /// client's does. Nothing for a delivery at an earlier renegotiation's `rejection` callback, or while none waits.
    fn reoffer_rejected(&mut self, delivery: &Delivery, now: Instant) {
        let Some(call) = self.call.as_mut() else { return };
        if call
            .reoffer
            .as_ref()
            .is_none_or(|reoffer| reoffer.rejection != delivery.path)
        {
            return;
        }
        call.reoffer = None;
        let in_progress = read::negotiation_in_progress(&delivery.body);
        if in_progress && call.deferred.is_some() {
            let id = call.id;
            self.out.event(CallEvent::RenegotiationGaveWay { call: id });
            self.tell_deferred();
            return;
        }
        if in_progress && call.offered.is_some() && call.retried < RENEGOTIATION_RETRIES {
            call.retry = Some(now + RENEGOTIATION_RETRY);
            return;
        }
        self.fail(Step::Renegotiate, now);
    }

    /// The account's renegotiation turned away goes again, its offer as it was, once its time came and nothing else
    /// negotiates the call.
    fn renegotiate_again(&mut self, now: Instant) {
        let Some(call) = self.call.as_mut() else { return };
        let Some(at) = call.retry else { return };
        if now < at {
            return;
        }
        let free = call.negotiation.is_none() && !call.waits_for_answer() && !call.reoffering;
        let (Some(offer), Some(link), true) = (call.offered.clone(), call.renegotiation.clone(), free) else {
            return;
        };
        match build::renegotiate(call, &link, &offer) {
            Ok((write, reoffer)) => {
                self.out.send(call, Step::Renegotiate, write);
                call.reoffer = Some(reoffer);
                call.retry = None;
                call.retried += 1;
            }
            Err(_) => self.fail(Step::Renegotiate, now),
        }
    }

    /// An invitation the tracker offered (see the crate docs).
    fn invited(&mut self, delivery: &Delivery, now: Instant) {
        let Some(invitation) = read::invitation(&delivery.body) else {
            return;
        };
        if invitation.callee != self.identity.mri {
            return;
        }
        if let Some(chat) = &invitation.group_chat {
            self.running.insert(chat.clone(), invitation.conversation.clone());
        }
        if let Some(call) = self.call.as_ref() {
            let again = call
                .invitation()
                .is_some_and(|ringing| ringing.attach == invitation.attach);
            if again && matches!(call.phase, Phase::Attaching | Phase::Offered) {
                self.receipt(delivery);
            }
            // a second invitation while a call is up isn't offered: nothing is sent for it
            return;
        }
        let _ = self.ring(delivery, invitation, now);
    }

    /// A new invitation rings: its receipt, then the `attach` (it is shown once the `attach` is answered).
    fn ring(&mut self, delivery: &Delivery, invitation: Invitation, now: Instant) -> Result<(), CallError> {
        let agent = self.agent.clone().ok_or(CallError::NoEndpoint)?;
        let context = CallContext::fresh()?.with_region(self.identity.region.clone(), self.identity.partition.clone());
        let mut me = self.participant()?;
        if let Some(id) = invitation.callee_participant.clone() {
            me.participant_id = Some(id);
        }
        let caption_thread = invitation
            .group_chat
            .clone()
            .or_else(|| teams_proto::one_to_one_id(self.identity.mri.as_str(), invitation.caller.as_str()));
        let mut call = Call::new(
            CallId(self.next_call + 1),
            Side::Invited(Box::new(invitation)),
            context,
            me,
            agent,
            now,
        );
        call.caption_thread = caption_thread;
        let write = build::attach(&mut call, &self.identity)?;
        self.next_call += 1;
        self.receipt(delivery);
        self.out.send(&mut call, Step::Attach, write);
        self.call = Some(call);
        Ok(())
    }

    /// The receipt of an offered invitation's delivery.
    fn receipt(&mut self, delivery: &Delivery) {
        if let Some(receipt) = InvitationReceipt::new(delivery, 0) {
            self.out
                .respond(DeliveryKey::of(delivery), DeliveryReply::Receipt(receipt));
        }
    }

    /// The `attach`'s answer: the leg's links; it rings here now.
    fn attached(&mut self, body: &[u8]) {
        let Some(invitation) = self.call.as_ref().and_then(|call| call.invitation().cloned()) else {
            return;
        };
        let Some(leg) = read::attached(body, &invitation.conversation) else {
            self.let_go();
            return;
        };
        let Some(call) = self.call.as_mut() else { return };
        call.conversation = leg.conversation.clone();
        let progress = leg.progress.clone();
        call.leg = Some(leg);
        call.phase = Phase::Offered;
        if let Ok(write) = build::ringing(call, &progress) {
            self.out.send(call, Step::Ringing, write);
        }
        self.out.event(CallEvent::Incoming(Incoming {
            call: call.id,
            caller: invitation.caller,
            caller_name: invitation.caller_name,
            group_chat: invitation.group_chat,
            offer: invitation.offer,
        }));
    }

    /// A roster update at the call's own callback: the participants told when they changed.
    fn roster_updated(&mut self, delivery: &Delivery) {
        if let Ok(roster) = serde_json::from_slice::<serde_json::Value>(&delivery.body) {
            self.service_roster(&roster);
        }
        self.own_roster_told(&delivery.body);
        let Some(update) = read::roster(&delivery.body) else {
            return;
        };
        let Some(call) = self.call.as_mut() else { return };
        if let Some(participants) = call.roster.apply(update, &self.identity.mri) {
            let id = call.id;
            self.out.event(CallEvent::Participants { call: id, participants });
        }
        lobby_told(call, &mut self.out, &self.identity.mri);
        music_told(call, &mut self.out, &delivery.body);
    }

    /// An `add`'s failure delivered: each person it names (but a service the account added) wasn't brought in.
    fn add_failed(&mut self, delivery: &Delivery) {
        let Some(call) = self.call.as_ref() else { return };
        let id = call.id;
        for who in read::add_failed(&delivery.body)
            .into_iter()
            .filter(|who| who.as_str().starts_with("8:"))
        {
            self.out.event(CallEvent::AddFailed { call: id, who });
        }
    }

    /// A message at the conversation's `receiveMessage` callback: a live reaction told; anything else changes nothing.
    fn message_received(&mut self, delivery: &Delivery) {
        let Some(call) = self.call.as_ref() else { return };
        if let Some((from, name, reaction)) = read::reaction(&delivery.body) {
            let id = call.id;
            self.out.event(CallEvent::Reaction {
                call: id,
                from,
                name,
                reaction,
            });
        }
    }

    /// The test call's echo in its roster, or no longer: it leaves once it played the recording back (its entry turns
    /// inactive about 50 s after the media connected; the call controller ends nothing by itself), and the test call
    /// ends then with a `leave`, as the web client ends it ([`End::Remote`]). Nothing for any other call, nor before
    /// the echo was in it.
    fn echo_present(&mut self, now: Instant) {
        let Some(call) = self.call.as_mut().filter(|call| call.phase == Phase::Connected) else {
            return;
        };
        let Side::Placed { echo: Some(echo) } = &call.side else {
            return;
        };
        if call
            .roster
            .participants()
            .iter()
            .any(|participant| participant.mri == *echo)
        {
            call.echo_joined = true;
        } else if call.echo_joined {
            self.leave(Ending::EchoLeft, now);
        }
    }

    /// The `accept`'s answer: the call is up.
    fn connected_by_accept(&mut self, body: &[u8], now: Instant) {
        let answer = read::accept_answer(body);
        let Some(call) = self.call.as_mut() else { return };
        if call.phase != Phase::Answering {
            return;
        }
        call.renegotiation = answer.renegotiation;
        call.group_media = answer.group_media;
        call.hold = answer.hold;
        call.transfer = answer.transfer;
        self.connect((answer.keep_alive, answer.call_leg), now);
    }

    /// The call is up: told, its keep-alive armed, the account's endpoint metadata sent, then (once that was taken)
    /// the test call's echo added.
    fn connect(&mut self, (keep_alive, link): (Option<u64>, Option<CallLink>), now: Instant) {
        let Some(call) = self.call.as_mut() else { return };
        call.phase = Phase::Connected;
        call.keep_alive = keep_alive.map(|seconds| {
            let mut keep = KeepAlive {
                interval: Duration::from_secs(seconds),
                next: now,
                link,
            };
            keep.next = now + keep.period();
            keep
        });
        self.out.state(call);
        let mut writes = Vec::new();
        let metadata = call.conversation.endpoint_metadata.clone();
        if let Some(Ok(write)) = metadata.map(|link| build::endpoint_metadata(call, &link)) {
            writes.push((Step::EndpointMetadata, write));
        }
        if let Some(write) = echo_add(call, &self.identity) {
            writes.push((Step::Add, write));
        }
        self.out.send_in_turn(call, writes);
        // who is in it, as the roster told it so far (a callee's from its ring on, a joiner's from its invitation's),
        // told again now that the call is up and shows them
        let participants = call
            .roster
            .apply(Vec::new(), &self.identity.mri)
            .unwrap_or_else(|| call.roster.participants().to_vec());
        if !participants.is_empty() {
            let id = call.id;
            self.out.event(CallEvent::Participants { call: id, participants });
        }
    }

    /// A placed or joined call's acceptance.
    fn accepted(&mut self, delivery: &Delivery, observed: Observed, now: Instant) {
        let Some(phase) = self.call.as_ref().map(|call| call.phase) else {
            return;
        };
        if phase == Phase::Connected {
            // the same acceptance again: its response again
            self.acknowledge(delivery);
            return;
        }
        if phase == Phase::Placing {
            // before the start's or the join's answer: applied once it came (it hands out the links the call needs)
            if let Some(call) = self.call.as_mut() {
                call.early_acceptance = Some((delivery.clone(), observed));
            }
            return;
        }
        if !matches!(phase, Phase::Ringing | Phase::Joining) {
            return;
        }
        let acceptance = read::acceptance(&delivery.body);
        let Some(acceptance) = acceptance.filter(|_| observed == Observed::Acceptance) else {
            self.fail(Step::Acknowledge, now);
            return;
        };
        let links = self
            .call
            .as_mut()
            .map(|call| call.links(Kind::Call, &crate::call::ANSWERED_CALL_LINKS));
        let Some(Ok(links)) = links else {
            self.fail(Step::Acknowledge, now);
            return;
        };
        if let Some(call) = self.call.as_mut() {
            call.acknowledgement = Some(Acknowledgement {
                delivery: None,
                link: acceptance.acknowledgement.clone(),
                links,
                by_request: false,
            });
            call.renegotiation.clone_from(&acceptance.renegotiation);
            call.group_media.clone_from(&acceptance.group_media);
            call.hold.clone_from(&acceptance.hold);
            call.transfer.clone_from(&acceptance.transfer);
        }
        self.acknowledge(delivery);
        if let (Some(call), Some(media)) = (self.call.as_ref(), acceptance.answer) {
            self.out.event(CallEvent::RemoteMedia { call: call.id, media });
        }
        self.connect((acceptance.keep_alive, acceptance.call_leg), now);
        if let Some(call) = self.call.as_ref().filter(|_| acceptance.voicemail) {
            let id = call.id;
            self.out.event(CallEvent::Voicemail { call: id });
        }
    }

    /// The acceptance's acknowledgement as its delivery response; by request when the response can't be built.
    fn acknowledge(&mut self, delivery: &Delivery) {
        let Some(call) = self.call.as_mut() else { return };
        let Some(acknowledgement) = call.acknowledgement.as_mut() else {
            return;
        };
        let reply = AcceptanceAcknowledgement::new(delivery, correlation(), 0, acknowledgement.links.clone());
        match reply {
            Some(reply) => {
                acknowledgement.delivery = Some(DeliveryKey::of(delivery));
                self.out
                    .respond(DeliveryKey::of(delivery), DeliveryReply::Acknowledgement(reply));
            }
            None => self.acknowledge_by_request(),
        }
    }

    /// The acceptance's acknowledgement by request, once (the web client's way when the response failed).
    fn acknowledge_by_request(&mut self) {
        let Some(call) = self.call.as_mut() else { return };
        let Some(acknowledgement) = call.acknowledgement.as_mut() else {
            return;
        };
        if acknowledgement.by_request {
            return;
        }
        acknowledgement.by_request = true;
        let (link, links) = (acknowledgement.link.clone(), acknowledgement.links.clone());
        if let Some(Ok(write)) = link.map(|link| build::acknowledge(call, &link, &links)) {
            self.out.send(call, Step::Acknowledge, write);
        }
    }

    /// The call's end, or an offered invitation's ring's. The end of a call the account is leaving (its `leave` on its
    /// way) ends it for what it left for: the call controller ends a call for the `leave`, and may deliver that end
    /// before the `leave`'s answer comes (a live test call's did).
    fn ended(&mut self, delivery: &Delivery) {
        let end = read::remote_end(&delivery.body);
        match self.call.as_ref().map(|call| (call.id, call.phase)) {
            Some((_, Phase::Attaching)) => self.let_go(),
            Some((id, Phase::Offered)) => {
                drop(self.end_call());
                self.out.event(CallEvent::RingEnded { call: id, end });
            }
            // The end callback can precede the reject's HTTP answer. Only that write's outcome tells us whether
            // the local decline succeeded: a concurrent remote end cannot prove the reject was accepted. Keep
            // waiting for on_outcome, which reports Declined on 2xx and Failed("reject") otherwise.
            Some((_, Phase::Declining)) | None => {}
            Some((
                _,
                Phase::Leaving {
                    sent: true,
                    end: leaving,
                },
            )) => self.finish(leaving.end()),
            Some(_) => self.finish(End::Remote(end)),
        }
    }

    /// The delivery response `delivery` didn't go: an acceptance's is replaced by its acknowledgement by request.
    pub fn on_response_failed(&mut self, delivery: &DeliveryKey) {
        let failed = self
            .call
            .as_ref()
            .and_then(|call| call.acknowledgement.as_ref())
            .is_some_and(|acknowledgement| acknowledgement.delivery.as_ref() == Some(delivery));
        if failed {
            self.acknowledge_by_request();
        }
    }

    /// Time passed: a keep-alive that fell due goes to the call leg's own link (only while the call is up), and is
    /// told ([`CallEvent::KeepAliveDue`]).
    pub fn tick(&mut self, now: Instant) {
        if self.call.as_ref().is_some_and(|call| call.phase == Phase::Connected) {
            self.renegotiate_again(now);
        }
        let Some(call) = self.call.as_mut() else { return };
        if call.phase != Phase::Connected {
            return;
        }
        let Some(keep) = call.keep_alive.as_mut() else { return };
        if now < keep.next {
            return;
        }
        keep.next = now + keep.period();
        let write = keep.link.clone().map(|link| build::keep_alive(call, &link));
        let sent = match write {
            Some(Ok(write)) => {
                self.out.send(call, Step::KeepAlive, write);
                true
            }
            // a link that builds no keep-alive in its captured shape, or none: nothing goes (told as not sent)
            Some(Err(_)) | None => false,
        };
        let id = call.id;
        self.out.event(CallEvent::KeepAliveDue { call: id, sent });
    }
}

/// The `add` of the test call's echo, once its call was accepted: `None` for any other call.
fn echo_add(call: &mut Call, identity: &Identity) -> Option<CallWrite> {
    let Side::Placed { echo: Some(echo) } = call.side.clone() else {
        return None;
    };
    let link = call.conversation.add.clone()?;
    build::add(call, identity, &link, &echo).ok()
}

/// A fresh correlation vector (`MS-CV`) for a delivery response: 22 base64 characters and a step.
fn correlation() -> String {
    let mut bytes = [0_u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        return "AAAAAAAAAAAAAAAAAAAAAA.1".to_owned();
    }
    let base = base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes);
    format!("{base}.1")
}

#[cfg(test)]
mod tests;

/// The lobby's change since last told (who waits, whether the account itself does), as events.
/// The call `call` in `slot` while it is up.
fn up(slot: &mut Option<Call>, call: CallId) -> Result<&mut Call, CallError> {
    let current = slot
        .as_mut()
        .filter(|current| current.id == call)
        .ok_or(CallError::NoSuchCall)?;
    if current.phase == Phase::Connected {
        Ok(current)
    } else {
        Err(CallError::NotNow)
    }
}

/// The music-on-hold app's entry in a roster update `body`, told as the far end's hold when it came while the account
/// held nothing (no hold of its own taken or in flight; Teams' held party sees "On hold", seen 2026-11-06), and its
/// end once it left. Its entry while the account holds the call is the account's own hold's, told by nothing here.
fn music_told(call: &mut Call, out: &mut Out, body: &[u8]) {
    let Some((version, present)) = read::music_on_hold(body) else {
        return;
    };
    if call.music.is_some_and(|music| music.version >= version) {
        return;
    }
    let holding = call.resume.is_some()
        || call
            .writes
            .values()
            .any(|step| matches!(step, Step::Hold | Step::Resume));
    let before = call.music.filter(|music| music.present);
    // whose hold it plays for is settled as it comes: the account's own hold since then doesn't make it the account's
    let far_end = present && before.map_or(!holding, |music| music.far_end);
    call.music = Some(Music {
        version,
        present,
        far_end,
    });
    if far_end != before.is_some_and(|music| music.far_end) {
        out.event(CallEvent::HeldByFarEnd {
            call: call.id,
            held: far_end,
        });
    }
}

/// The call is in the group chat `chat` from now on (someone was added to it): told once, when it changed.
fn in_group_chat(call: &mut Call, out: &mut Out, chat: String) {
    if call.caption_thread.as_deref() == Some(chat.as_str()) {
        return;
    }
    call.caption_thread = Some(chat.clone());
    out.event(CallEvent::GroupChat { call: call.id, chat });
}

fn lobby_told(call: &mut Call, out: &mut Out, me: &Mri) {
    let change = call.roster.lobby(me);
    if let Some(waiting) = change.waiting {
        out.event(CallEvent::Lobby { call: call.id, waiting });
    }
    if let Some(waiting) = change.own {
        out.event(CallEvent::InLobby { call: call.id, waiting });
    }
    if let Some(own) = call.roster.own_changed(me) {
        out.event(CallEvent::Own {
            call: call.id,
            hand_raised: own.hand.is_some(),
            organizer: own.organizer,
        });
    }
}

#[path = "captions.rs"]
mod captions;

#[path = "meeting.rs"]
mod meeting;
pub(crate) use meeting::Meeting;
pub use meeting::{MeetingState, Recording, Transcription};
