//! A call as the controller keeps it, and what each operation makes of it: the next state and the deliveries its
//! parties get. Nothing here reads a clock, the database or the network, so every node makes the same step from the
//! same row ([`super::repo`] stores the step only if no other node made one meanwhile).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::media::{MediaHome, Sources};
use super::wire::{self, Links};

mod group;
mod meeting;

/// The call's keep-alive interval, in seconds, as Teams hands it out.
pub(crate) const KEEP_ALIVE: i64 = 2700;
/// How long after its last keep-alive was due a call is given up, in seconds.
const KEEP_ALIVE_SLACK: i64 = 600;
/// How long a renegotiation may wait for its answer before another one takes its place, in milliseconds.
const NEGOTIATION_WAIT_MS: i64 = 30_000;
/// The app Teams lists in a held call's roster.
pub(crate) const MUSIC_ON_HOLD: &str = "28:56aa0a8f-4cd3-446b-8146-0023476a5ff2";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Phase {
    Ringing,
    Connected,
    Ended,
}

impl Phase {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Ringing => "ringing",
            Self::Connected => "connected",
            Self::Ended => "ended",
        }
    }
}

/// Which party of a call: its place in [`Call::parties`], the caller's first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Seat(pub usize);

impl Seat {
    pub(crate) const CALLER: Self = Self(0);
    /// A 1:1 call's one callee.
    pub(crate) const CALLEE: Self = Self(1);
}

/// Whether a call is between two people, or a group chat's call through a media node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    #[default]
    OneToOne,
    Group,
}

/// Where a group call's party is. A 1:1 call's parties keep the default but a callee's decline: whether they are in
/// the call is whether they have an endpoint in it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Status {
    #[default]
    Ringing,
    InCall,
    Declined,
    Missed,
    Left,
    /// It waits in a meeting's lobby, its offer kept until someone lets it in.
    Lobby,
}

/// A group call party's leg on the call's media node: its id there and the sources it sends as.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Leg {
    pub id: String,
    pub sources: Sources,
    /// While it is reserved: the seat's leg it was reserved over (its own `before` the one that leg was).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Box<Leg>>,
}

impl Leg {
    /// This leg's id and the ids of the legs it was reserved over.
    pub(crate) fn ids(&self) -> impl Iterator<Item = &str> {
        std::iter::successors(Some(self), |leg| leg.before.as_deref()).map(|leg| leg.id.as_str())
    }

    /// Takes the leg `gone` out of the ones it was reserved over, the one before it taking its place.
    fn forget(&mut self, gone: &str) {
        let Some(before) = self.before.as_mut() else {
            return;
        };
        if before.id == gone {
            let earlier = before.before.take();
            self.before = earlier;
        } else {
            before.forget(gone);
        }
    }
}

/// A chat member who follows a group call's roster without being in it: its endpoint and callbacks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Watcher {
    pub id: String,
    pub endpoint: String,
    pub links: Links,
}

/// What makes a group call a meeting's: who organizes the meeting (the account the organizer's controls are for), and
/// whether it is a channel's meeting, with its post in the channel once it has one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Meeting {
    pub organizer: String,
    #[serde(default)]
    pub channel: bool,
    #[serde(default)]
    pub post: Option<String>,
    /// The meeting's options in effect, as its organizer last saved them.
    #[serde(default)]
    pub controls: Controls,
}

/// A meeting's options that bind its call (`docs/protocol/calling.md`, "Controls"), as saved: who comes in without
/// the lobby (`AutoAdmittedUsers`), who presents (`PresenterOption`), and what attendees may not do
/// (`AttendeeRestrictions`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Controls {
    pub lobby: String,
    pub presenters: String,
    pub restrictions: String,
}

impl Controls {
    /// Only the organizer presents: everyone else is an attendee.
    pub(crate) fn attendees(&self) -> bool {
        matches!(self.presenters.as_str(), "OrganizerOnly" | "SpecificPeople")
    }

    /// Attendees may not unmute themselves.
    pub(crate) fn audio_blocked(&self) -> bool {
        self.restrictions.contains("DisableAudio")
    }
}

/// One of a party's endpoints: its Trouter endpoint and the callbacks it handed out, by name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Endpoint {
    pub endpoint: String,
    pub links: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Party {
    /// The account's object id.
    pub id: String,
    pub name: String,
    /// The party as it names itself (`id`, `displayName`, `endpointId`, `participantId`, `languageId`).
    pub participant: Value,
    /// The caller's one endpoint; each endpoint of the callee that attached.
    pub endpoints: Vec<Endpoint>,
    /// The endpoint in the call: the caller's from the start, the callee's once it answered.
    pub active: Option<String>,
    pub muted: bool,
    /// The endpoint state's own sequence number, as the client counts it.
    pub state_sequence: u64,
    /// When it came into the call (RFC 3339).
    pub joined: Option<String>,
    #[serde(default)]
    pub status: Status,
    /// When a group call's ring of it ends (Unix seconds), while it rings.
    #[serde(default)]
    pub ring_due: Option<i64>,
    #[serde(default)]
    pub leg: Option<Leg>,
    /// How many times a group call rang it: its invitation's links name its ring, so one ring's isn't another's.
    #[serde(default)]
    pub rings: u64,
    /// A group call party's renegotiation whose answer waits for its acknowledgement.
    #[serde(default)]
    pub answered: Option<Pending>,
    /// What a group call party sends, as its media descriptions said last.
    #[serde(default)]
    pub showing: Showing,
    /// Its share is the call's: the newest one started. The roster lists no other party's.
    #[serde(default)]
    pub presenting: bool,
    /// While it waits in a meeting's lobby: the offer its join carried, which its leg is made with once it is let in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<Value>,
    /// Its raised hand, while it is up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hand: Option<Hand>,
    /// A host muted its main audio, and it hasn't unmuted itself since.
    #[serde(default)]
    pub server_muted: bool,
    /// How many times the service changed its mutes: the roster's `serverMuteVersion`.
    #[serde(default)]
    pub mute_version: u64,
    /// While an add rings it: the adder's `addParticipantFailure` link, where its decline is told.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_failure: Option<String>,
}

/// A raised hand: the state the roster names it by, its rank among the hands raised (the first is 1), and what its
/// raise sent along (`content`: the skin tone).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Hand {
    pub id: String,
    pub rank: u64,
    pub content: Value,
}

/// What a group call party sends besides its audio: its camera, its screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Showing {
    pub camera: bool,
    pub sharing: bool,
}

/// A group call party's renegotiation the media node answered: its number, and when (Unix milliseconds).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Pending {
    pub number: u64,
    pub at: i64,
}

/// What a join's or an add's reservation of a seat took ([`Call::reserve`]), to undo it if its leg or its step fails.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Reserved {
    pub seat: Seat,
    pub sources: Sources,
    /// How the seat named its party before (`participant`, `name`).
    named: (Value, String),
    /// The seat was made for it.
    made: bool,
}

impl Party {
    pub(crate) fn mri(&self) -> String {
        format!("8:orgid:{}", self.id)
    }

    fn link_of(&self, endpoint: &str, name: &str) -> Option<&str> {
        self.endpoints
            .iter()
            .find(|known| known.endpoint == endpoint)
            .and_then(|known| known.links.get(name))
            .map(String::as_str)
    }

    /// The callback `name` of the endpoint in the call.
    fn link(&self, name: &str) -> Option<&str> {
        self.link_of(self.active.as_deref()?, name)
    }

    /// Keeps `links` with `endpoint`'s other callbacks, the newer of a name in place of the older.
    pub(crate) fn keep(&mut self, endpoint: &str, links: Links) {
        if let Some(known) = self.endpoints.iter_mut().find(|known| known.endpoint == endpoint) {
            known.links.extend(links);
        } else {
            self.endpoints.push(Endpoint {
                endpoint: endpoint.to_owned(),
                links,
            });
        }
    }
}

/// A renegotiation forwarded to the far end, waiting for its answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Negotiation {
    pub by: Seat,
    pub number: u64,
    /// Unix milliseconds.
    pub at: i64,
}

/// A renegotiation's answer, forwarded to the party that offered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Answered {
    pub by: Seat,
    pub number: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Call {
    pub id: String,
    pub conversation: String,
    pub phase: Phase,
    /// The caller first; a 1:1 call's callee second.
    pub parties: Vec<Party>,
    /// The caller's offer (`mediaContent`), as the invitation carries it.
    pub offer: Value,
    /// Unix milliseconds.
    pub started_at: i64,
    pub connected_at: Option<i64>,
    pub ended_at: Option<i64>,
    /// When the sweeper looks at the call next (Unix seconds): the ring's end, the keep-alive's, the row's.
    pub due_at: i64,
    pub roster_sequence: u64,
    pub negotiation: Option<Negotiation>,
    pub negotiations: u64,
    /// The answered renegotiation whose answer waits to be acknowledged.
    pub answered: Option<Answered>,
    pub held_by: Option<Seat>,
    /// The chat the call's start was written to.
    pub chat: Option<String>,
    /// The `endpointId` the callee's acceptance named, when it named one: the endpoint of the callee's that is in the
    /// call, as its own requests name it.
    #[serde(default)]
    pub accepted_as: Option<String>,
    #[serde(default)]
    pub kind: Kind,
    /// A group call's room on its media node.
    #[serde(default)]
    pub media: Option<MediaHome>,
    /// The first source id the next party of a group call gets.
    #[serde(default)]
    pub next_source: u32,
    #[serde(default)]
    pub watchers: Vec<Watcher>,
    /// When a group call is given up without a keep-alive (Unix seconds).
    #[serde(default)]
    pub alive_due: i64,
    /// A meeting's call: its meeting.
    #[serde(default)]
    pub meeting: Option<Meeting>,
}

/// A delivery to one calling endpoint: `body` at `path` under its push URL (empty: its root).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Delivery {
    pub endpoint: String,
    pub path: String,
    pub body: Value,
}

/// What the chat is told of a call that ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// It was up for this many seconds.
    Talked(i64),
    /// Nobody answered it: the callee missed it.
    Missed,
}

/// What a step did besides changing the call.
#[derive(Debug, Default)]
pub(crate) struct Out {
    pub deliveries: Vec<Delivery>,
    pub ended: Option<Outcome>,
    /// The media legs the step let go of (a group call's party left, declined or wasn't reached).
    pub released: Vec<String>,
    /// The step ended a group call: its room goes.
    pub room_over: bool,
}

impl Out {
    /// Queues `body` for the callback `link`, when the party handed one out.
    fn call_back(&mut self, origin: &str, link: Option<&str>, body: Value) {
        if let Some((endpoint, path)) = link.and_then(|link| wire::callback(origin, link)) {
            self.deliveries.push(Delivery {
                endpoint: endpoint.to_owned(),
                path: path.to_owned(),
                body,
            });
        }
    }
}

/// Why a step wasn't made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The request isn't one of the operation's.
    Bad,
    /// The account isn't who may do it.
    Forbidden,
    /// No such call, for this account.
    Missing,
    /// The call is over, or past the point the operation belongs to.
    Gone,
}

/// When and where a step is made.
#[derive(Clone, Copy, Debug)]
pub(crate) struct At<'a> {
    pub origin: &'a str,
    /// Unix milliseconds.
    pub now: i64,
}

impl At<'_> {
    pub(crate) fn seconds(&self) -> i64 {
        self.now / 1000
    }
}

impl Call {
    pub(crate) fn party(&self, seat: Seat) -> &Party {
        &self.parties[seat.0]
    }

    pub(crate) fn party_mut(&mut self, seat: Seat) -> &mut Party {
        &mut self.parties[seat.0]
    }

    /// Which party the account `id` is.
    pub(crate) fn seat_of(&self, id: &str) -> Option<Seat> {
        self.parties.iter().position(|party| party.id == id).map(Seat)
    }

    /// The other party of a 1:1 call.
    fn far(seat: Seat) -> Seat {
        if seat == Seat::CALLER {
            Seat::CALLEE
        } else {
            Seat::CALLER
        }
    }

    /// The party's seat when it is in the call that is up.
    pub(crate) fn in_call(&self, seat: Seat) -> Result<(), Refusal> {
        let party = self.party(seat);
        let present = match self.kind {
            Kind::OneToOne => party.active.is_some(),
            Kind::Group => party.status == Status::InCall,
        };
        match self.phase {
            Phase::Connected if present => Ok(()),
            Phase::Ended => Err(Refusal::Gone),
            _ => Err(Refusal::Forbidden),
        }
    }

    pub(crate) fn ringing(&self, seat: Seat) -> Result<(), Refusal> {
        if self.kind == Kind::Group {
            // whoever rings, the caller added back too
            return match (self.phase, self.party(seat).status) {
                (Phase::Ended, _) => Err(Refusal::Gone),
                (_, Status::Ringing) => Ok(()),
                _ => Err(Refusal::Gone),
            };
        }
        match (self.phase, seat) {
            (Phase::Ringing, Seat::CALLEE) => Ok(()),
            (Phase::Ringing, Seat::CALLER) => Err(Refusal::Forbidden),
            _ => Err(Refusal::Gone),
        }
    }

    /// The callee's endpoint attaches to its leg while it rings: its callbacks kept; the leg's links answered.
    pub(crate) fn attach(&mut self, seat: Seat, endpoint: &str, links: Links, at: At<'_>) -> Result<Value, Refusal> {
        self.ringing(seat)?;
        if !links.contains_key("end") {
            return Err(Refusal::Bad);
        }
        self.party_mut(seat).keep(endpoint, links);
        Ok(wire::attach_answer(at.origin, self, seat))
    }

    /// A party in the call describes its media lines again or seeds its channel: nothing the far end is told.
    pub(crate) fn described(&self, seat: Seat) -> Result<(), Refusal> {
        self.in_call(seat)
    }

    /// The callee's endpoint says it rings. One whose ring is over already is taken and changes nothing: its client
    /// reads a refusal as a failed call, while the ring's end (a cancel, an answer elsewhere) is on its way to it.
    pub(crate) fn progress(&self, seat: Seat) -> Result<(), Refusal> {
        match self.ringing(seat) {
            Err(Refusal::Gone) => Ok(()),
            rung => rung,
        }
    }

    /// The callee's endpoint answers: the call is up, the caller gets the acceptance with the callee's answer, the
    /// callee's other endpoints stop ringing and both parties get the roster.
    pub(crate) fn accept(
        &mut self,
        seat: Seat,
        endpoint: &str,
        (links, body): (Links, &Value),
        at: At<'_>,
        out: &mut Out,
    ) -> Result<Value, Refusal> {
        self.ringing(seat)?;
        let acceptance = body.get("callAcceptance").ok_or(Refusal::Bad)?;
        let media = acceptance.get("mediaContent").filter(|media| media.is_object());
        self.party_mut(Seat::CALLEE).keep(endpoint, links);
        for other in self
            .party(Seat::CALLEE)
            .endpoints
            .iter()
            .filter(|known| known.endpoint != endpoint)
        {
            let end = wire::end(487, "Call cancelled as it was accepted by another fork.", None);
            out.call_back(at.origin, other.links.get("end").map(String::as_str), end);
        }
        self.party_mut(Seat::CALLEE)
            .endpoints
            .retain(|known| known.endpoint == endpoint);
        self.party_mut(Seat::CALLEE).active = Some(endpoint.to_owned());
        if let Some(by) = acceptance.get("acceptedBy").filter(|by| by.is_object()) {
            let named = wire::named_by(by, self.party(Seat::CALLEE));
            self.party_mut(Seat::CALLEE).participant = named;
            self.accepted_as = by.get("endpointId").and_then(Value::as_str).map(str::to_owned);
        }
        self.phase = Phase::Connected;
        self.connected_at = Some(at.now);
        self.due_at = at.seconds() + KEEP_ALIVE + KEEP_ALIVE_SLACK;
        let joined = wire::iso(at.now);
        self.party_mut(Seat::CALLER).joined = Some(joined.clone());
        self.party_mut(Seat::CALLEE).joined = Some(joined);
        let told = wire::acceptance(at.origin, self, media.ok_or(Refusal::Bad)?);
        out.call_back(at.origin, self.party(Seat::CALLER).link("acceptance"), told);
        self.tell_roster(at.origin, &[Seat::CALLER, Seat::CALLEE], out);
        Ok(wire::accept_answer(at.origin, self, seat))
    }

    /// The callee declines: every endpoint of its that rang stops, and the caller's call ends as declined.
    pub(crate) fn reject(&mut self, seat: Seat, body: &Value, at: At<'_>, out: &mut Out) -> Result<(), Refusal> {
        if self.kind == Kind::Group {
            return self.group_reject(seat, body, at, out);
        }
        self.ringing(seat)?;
        let code = body.pointer("/callEnd/code").and_then(Value::as_i64).unwrap_or(603);
        let callee = Some(&self.party(Seat::CALLEE).participant);
        for known in &self.party(Seat::CALLEE).endpoints {
            let end = wire::end(code, "CallEndReasonLocalUserInitiated", callee);
            out.call_back(at.origin, known.links.get("end").map(String::as_str), end);
        }
        let end = wire::end(code, "Declined", callee);
        out.call_back(at.origin, self.party(Seat::CALLER).link("end"), end);
        self.party_mut(Seat::CALLEE).status = Status::Declined;
        self.finish(at, Outcome::Missed, out);
        Ok(())
    }

    /// A party leaves: the caller's leave while it rings cancels the ring, anyone's once it is up ends the call. Once
    /// it is up, a leave of the callee's that names another endpoint than the one that answered changes nothing: it
    /// comes from an endpoint whose answer lost to the other's.
    pub(crate) fn leave(&mut self, seat: Seat, body: &Value, at: At<'_>, out: &mut Out) {
        if self.kind == Kind::Group {
            self.group_leave(seat, body, at, out);
            return;
        }
        if self.phase == Phase::Connected && seat == Seat::CALLEE && self.lost_fork(body) {
            return;
        }
        let code = body
            .pointer("/callTransactionEnd/code")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let leaver = self.party(seat).participant.clone();
        let end = wire::end(code, "CallEndReasonLocalUserInitiated", Some(&leaver));
        match (self.phase, seat) {
            (Phase::Ringing, Seat::CALLER) => {
                self.end_for_all(at.origin, &end, &end, out);
                self.finish(at, Outcome::Missed, out);
            }
            (Phase::Ended | Phase::Ringing, _) => {}
            (Phase::Connected, _) => {
                self.end_for_all(at.origin, &end, &end, out);
                let talked = (at.now - self.connected_at.unwrap_or(at.now)) / 1000;
                self.finish(at, Outcome::Talked(talked.max(0)), out);
            }
        }
    }

    /// Whether `body` names an endpoint of the callee's other than the one whose acceptance is in the call.
    fn lost_fork(&self, body: &Value) -> bool {
        let leaver = body.pointer("/participants/from/endpointId").and_then(Value::as_str);
        matches!((leaver, &self.accepted_as), (Some(leaver), Some(accepted)) if leaver != accepted)
    }

    /// What the sweeper makes of a call that is due: a ring nobody answered ends at both ends, a call whose
    /// keep-alives stopped is given up. `false` when it isn't due (another node was first) or is over.
    pub(crate) fn lapse(&mut self, at: At<'_>, out: &mut Out) -> bool {
        if self.due_at > at.seconds() {
            return false;
        }
        if self.kind == Kind::Group {
            return self.group_lapse(at, out);
        }
        match self.phase {
            Phase::Ended => false,
            Phase::Ringing => {
                let rung = wire::end(487, "Call cancelled on timeout.", None);
                let placed = wire::end(480, "No answer", None);
                self.end_for_all(at.origin, &placed, &rung, out);
                self.finish(at, Outcome::Missed, out);
                true
            }
            Phase::Connected => {
                let end = wire::failure(408, "Call timed out without a keep-alive.");
                self.end_for_all(at.origin, &end, &end, out);
                let talked = (at.now - self.connected_at.unwrap_or(at.now)) / 1000;
                self.finish(at, Outcome::Talked(talked.max(0)), out);
                true
            }
        }
    }

    /// Ends the call at the caller's endpoint with `placed` and at each of the callee's with `rung`.
    fn end_for_all(&self, origin: &str, placed: &Value, rung: &Value, out: &mut Out) {
        out.call_back(origin, self.party(Seat::CALLER).link("end"), placed.clone());
        for known in &self.party(Seat::CALLEE).endpoints {
            out.call_back(origin, known.links.get("end").map(String::as_str), rung.clone());
        }
    }

    fn finish(&mut self, at: At<'_>, outcome: Outcome, out: &mut Out) {
        self.phase = Phase::Ended;
        self.ended_at = Some(at.now);
        self.due_at = at.seconds() + super::KEPT_AFTER_END;
        self.negotiation = None;
        out.ended = Some(outcome);
    }

    /// The call's keep-alive from a party in it.
    pub(crate) fn keep_alive(&mut self, seat: Seat, at: At<'_>) -> Result<(), Refusal> {
        self.in_call(seat)?;
        if self.kind == Kind::Group {
            self.alive_due = at.seconds() + KEEP_ALIVE + KEEP_ALIVE_SLACK;
            self.reschedule();
        } else {
            self.due_at = at.seconds() + KEEP_ALIVE + KEEP_ALIVE_SLACK;
        }
        Ok(())
    }

    /// A party's endpoint reports its state (its microphone): the roster tells the call's parties once the party is
    /// in the call.
    pub(crate) fn endpoint_state(
        &mut self,
        seat: Seat,
        body: &Value,
        at: At<'_>,
        out: &mut Out,
    ) -> Result<(), Refusal> {
        if self.phase == Phase::Ended {
            return Ok(());
        }
        let state = body.get("endpointState").ok_or(Refusal::Bad)?;
        let muted = state.pointer("/state/isMuted").and_then(Value::as_bool);
        let sequence = state.get("endpointStateSequenceNumber").and_then(Value::as_u64);
        let party = self.party_mut(seat);
        if sequence.is_some_and(|sequence| sequence < party.state_sequence) {
            return Ok(());
        }
        // an unmute of its own takes back a host's mute
        let unmuted = muted == Some(false) && std::mem::take(&mut party.server_muted);
        let changed = unmuted || muted.is_some_and(|muted| muted != party.muted);
        party.muted = muted.unwrap_or(party.muted);
        party.state_sequence = sequence.unwrap_or(party.state_sequence + 1);
        if changed && self.in_call(seat).is_ok() {
            self.tell_roster(at.origin, &[seat], out);
        }
        Ok(())
    }

    /// A party's later callbacks (`notificationLinks`, the acknowledgement of its acceptance).
    pub(crate) fn more_links(&mut self, seat: Seat, endpoint: &str, links: Links) {
        let over = self.phase == Phase::Ended;
        let party = self.party_mut(seat);
        if !over && party.endpoints.iter().any(|known| known.endpoint == endpoint) {
            party.keep(endpoint, links);
        }
    }

    /// A party offers the call's media again: the far end gets the offer at its own callback, with where its answer
    /// goes; while another negotiation waits for its answer, the offer is turned away (`491`).
    pub(crate) fn renegotiate(
        &mut self,
        seat: Seat,
        endpoint: &str,
        (links, body): (Links, &Value),
        at: At<'_>,
        out: &mut Out,
    ) -> Result<(), Refusal> {
        self.in_call(seat)?;
        if self.kind == Kind::Group {
            return Err(Refusal::Forbidden);
        }
        let offered = body.get("mediaNegotiation").ok_or(Refusal::Bad)?;
        let media = offered
            .get("mediaContent")
            .filter(|media| media.is_object())
            .ok_or(Refusal::Bad)?;
        if !links.contains_key("mediaAnswer") {
            return Err(Refusal::Bad);
        }
        let rejection = links.get("rejection").cloned();
        self.party_mut(seat).keep(endpoint, links);
        let waiting = self
            .negotiation
            .as_ref()
            .is_some_and(|waiting| at.now - waiting.at < NEGOTIATION_WAIT_MS);
        let far = self
            .party(Self::far(seat))
            .link("mediaRenegotiation")
            .map(str::to_owned);
        let (Some(far), false) = (far, waiting) else {
            let (code, phrase) = if waiting {
                (491, "Negotiation is in progress.")
            } else {
                (480, "The far end takes no renegotiation.")
            };
            out.call_back(at.origin, rejection.as_deref(), wire::negotiation_failure(code, phrase));
            return Ok(());
        };
        self.negotiations += 1;
        self.negotiation = Some(Negotiation {
            by: seat,
            number: self.negotiations,
            at: at.now,
        });
        let sender = self.party(seat).participant.clone();
        let forwarded = wire::negotiation(at.origin, self, &sender, offered, media);
        out.call_back(at.origin, Some(&far), forwarded);
        Ok(())
    }

    /// The far end answers the renegotiation `number`: its answer goes to the party that offered, with where it is
    /// acknowledged. An answer to an earlier negotiation than the one that waits has nothing to answer.
    pub(crate) fn answer(
        &mut self,
        seat: Seat,
        number: u64,
        (endpoint, links, body): (&str, Links, &Value),
        at: At<'_>,
        out: &mut Out,
    ) -> Result<(), Refusal> {
        self.in_call(seat)?;
        let answer = body.get("mediaAnswer").ok_or(Refusal::Bad)?;
        let media = answer
            .get("mediaContent")
            .filter(|media| media.is_object())
            .ok_or(Refusal::Bad)?;
        let offered = self
            .negotiation
            .take_if(|waiting| waiting.by != seat && waiting.number == number)
            .ok_or(Refusal::Gone)?;
        self.party_mut(seat).keep(endpoint, links);
        self.answered = Some(Answered { by: seat, number });
        let told = wire::media_answer(at.origin, self, &self.party(seat).mri(), answer, media);
        out.call_back(at.origin, self.party(offered.by).link("mediaAnswer"), told);
        Ok(())
    }

    /// The far end turns the renegotiation `number` away: the party that offered is told at its `rejection`
    /// callback.
    pub(crate) fn turn_away(
        &mut self,
        seat: Seat,
        number: u64,
        body: &Value,
        at: At<'_>,
        out: &mut Out,
    ) -> Result<(), Refusal> {
        self.in_call(seat)?;
        let offered = self
            .negotiation
            .take_if(|waiting| waiting.by != seat && waiting.number == number)
            .ok_or(Refusal::Gone)?;
        let failure = if body.is_object() {
            body.clone()
        } else {
            wire::negotiation_failure(488, "The far end rejected the negotiation.")
        };
        out.call_back(at.origin, self.party(offered.by).link("rejection"), failure);
        Ok(())
    }

    /// The party that offered acknowledges the answer to its renegotiation `number`: the answerer hears it at its
    /// own callback.
    pub(crate) fn acknowledge_answer(
        &mut self,
        seat: Seat,
        number: u64,
        at: At<'_>,
        out: &mut Out,
    ) -> Result<(), Refusal> {
        self.in_call(seat)?;
        if self.kind == Kind::Group {
            self.party_mut(seat)
                .answered
                .take_if(|answered| answered.number == number);
            return Ok(());
        }
        let answered = self
            .answered
            .take_if(|answered| answered.by != seat && answered.number == number);
        if let Some(answered) = answered {
            let link = self.party(answered.by).link("mediaAcknowledgement");
            out.call_back(at.origin, link, wire::media_acknowledgement());
        }
        Ok(())
    }

    /// A party puts the call on hold: both rosters gain the music-on-hold app, the holder is told it is done and
    /// where the hold is taken back.
    pub(crate) fn hold(
        &mut self,
        seat: Seat,
        endpoint: &str,
        links: Links,
        at: At<'_>,
        out: &mut Out,
    ) -> Result<Value, Refusal> {
        self.in_call(seat)?;
        if self.kind == Kind::Group || self.held_by.is_some_and(|holder| holder != seat) {
            return Err(Refusal::Forbidden);
        }
        self.party_mut(seat).keep(endpoint, links);
        if self.held_by.is_none() {
            self.held_by = Some(seat);
            self.tell_music(at, true, out);
        }
        out.call_back(
            at.origin,
            self.party(seat).link("holdCompletion"),
            wire::hold_completion(),
        );
        Ok(wire::hold_answer(at.origin, self))
    }

    /// The holder takes the call off hold: the music-on-hold app leaves both rosters.
    pub(crate) fn resume(
        &mut self,
        seat: Seat,
        endpoint: &str,
        links: Links,
        at: At<'_>,
        out: &mut Out,
    ) -> Result<(), Refusal> {
        self.in_call(seat)?;
        if self.kind == Kind::Group {
            return Err(Refusal::Forbidden);
        }
        if self.held_by != Some(seat) {
            return Err(Refusal::Gone);
        }
        self.party_mut(seat).keep(endpoint, links);
        self.held_by = None;
        self.tell_music(at, false, out);
        out.call_back(
            at.origin,
            self.party(seat).link("resumeCompletion"),
            wire::resume_completion(),
        );
        Ok(())
    }

    fn tell_music(&mut self, at: At<'_>, playing: bool, out: &mut Out) {
        self.roster_sequence += 1;
        let entry = wire::music_entry(self.roster_sequence, playing);
        let update = json!({
            "participants": {MUSIC_ON_HOLD: entry}, "type": "Delta", "sequenceNumber": self.roster_sequence,
        });
        self.send_roster(at.origin, &update, out);
    }

    /// Tells the call's parties the roster's entries of `seats` (a group call's ringing parties and followers too).
    fn tell_roster(&mut self, origin: &str, seats: &[Seat], out: &mut Out) {
        if self.kind == Kind::Group {
            self.tell_group(origin, seats, None, out);
            return;
        }
        self.roster_sequence += 1;
        let update = wire::roster(self, seats);
        self.send_roster(origin, &update, out);
    }

    fn send_roster(&self, origin: &str, update: &Value, out: &mut Out) {
        for party in &self.parties {
            out.call_back(origin, party.link("rosterUpdate"), update.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Map;

    use super::*;

    const ORIGIN: &str = "https://own.example.test";

    fn link(endpoint: &str, name: &str) -> String {
        format!("{ORIGIN}/trouter/v4/f/{endpoint}/callAgent/agent/1/call/{name}/")
    }

    fn links(endpoint: &str, names: &[&str]) -> Links {
        names
            .iter()
            .map(|name| ((*name).to_owned(), link(endpoint, name)))
            .collect()
    }

    fn party(id: &str, name: &str) -> Party {
        Party {
            id: id.to_owned(),
            name: name.to_owned(),
            participant: json!({"id": format!("8:orgid:{id}"), "displayName": name, "endpointId": format!("ep-{id}"),
                "participantId": format!("p-{id}")}),
            ..Party::default()
        }
    }

    fn at(now: i64) -> At<'static> {
        At { origin: ORIGIN, now }
    }

    /// A call a placed to b at second 1000, ringing for 30 seconds.
    fn ringing() -> Call {
        let mut caller = party("a", "Ada");
        caller.keep(
            "A",
            links("A", &["acceptance", "end", "rosterUpdate", "mediaAnswer", "rejection"]),
        );
        caller.active = Some("A".to_owned());
        Call {
            id: "call".to_owned(),
            conversation: "conversation".to_owned(),
            phase: Phase::Ringing,
            parties: vec![caller, party("b", "Bert")],
            offer: json!({"blob": "offer", "contentType": "application/sdp-ngc-1.0"}),
            started_at: 1_000_000,
            connected_at: None,
            ended_at: None,
            due_at: 1_030,
            roster_sequence: 0,
            negotiation: None,
            negotiations: 0,
            answered: None,
            held_by: None,
            chat: None,
            accepted_as: None,
            kind: Kind::OneToOne,
            media: None,
            next_source: 0,
            watchers: Vec::new(),
            alive_due: 0,
            meeting: None,
        }
    }

    fn acceptance() -> Value {
        json!({"callAcceptance": {"acceptedBy": {"endpointId": "ep-b2", "participantId": "p-b"},
            "mediaContent": {"blob": "answer", "contentType": "application/sdp-ngc-1.0"}}})
    }

    /// The call answered on b's endpoint `B` at second 1010, another endpoint of b's still ringing.
    fn connected() -> Call {
        let mut call = ringing();
        for endpoint in ["B", "B2"] {
            call.attach(
                Seat::CALLEE,
                endpoint,
                links(endpoint, &["end", "rosterUpdate"]),
                at(1_005_000),
            )
            .unwrap();
        }
        let mut out = Out::default();
        let accepted = links("B", &["mediaRenegotiation", "mediaAcknowledgement"]);
        call.accept(Seat::CALLEE, "B", (accepted, &acceptance()), at(1_010_000), &mut out)
            .unwrap();
        call
    }

    fn paths(out: &Out) -> Vec<(&str, &str)> {
        out.deliveries
            .iter()
            .map(|delivery| {
                let kind = delivery.path.trim_end_matches('/').rsplit('/').next().unwrap();
                (delivery.endpoint.as_str(), kind)
            })
            .collect()
    }

    #[test]
    fn only_the_callee_attaches_and_only_while_it_rings() {
        let mut call = ringing();
        let end = links("B", &["end"]);
        assert_eq!(
            call.attach(Seat::CALLER, "A", end.clone(), at(1_001_000)).unwrap_err(),
            Refusal::Forbidden
        );
        assert_eq!(
            call.attach(Seat::CALLEE, "B", links("B", &["rosterUpdate"]), at(1_001_000))
                .unwrap_err(),
            Refusal::Bad
        );
        let answer = call.attach(Seat::CALLEE, "B", end.clone(), at(1_001_000)).unwrap();
        assert!(
            answer["callInvitation"]["links"]["acceptance"]
                .as_str()
                .unwrap()
                .ends_with("/cc/v1/incoming/call/1/t/1/accept")
        );
        assert_eq!(call.progress(Seat::CALLEE), Ok(()));
        assert_eq!(call.progress(Seat::CALLER), Err(Refusal::Forbidden));
        let call = connected();
        assert_eq!(call.progress(Seat::CALLEE), Ok(()), "a late progress");
        assert_eq!(
            connected().attach(Seat::CALLEE, "B", end, at(1_011_000)).unwrap_err(),
            Refusal::Gone
        );
    }

    #[test]
    fn an_answer_connects_the_call_tells_the_caller_and_ends_the_other_rings() {
        let mut call = ringing();
        for endpoint in ["B", "B2"] {
            call.attach(
                Seat::CALLEE,
                endpoint,
                links(endpoint, &["end", "rosterUpdate"]),
                at(1_005_000),
            )
            .unwrap();
        }
        let mut out = Out::default();
        let no_media = json!({"callAcceptance": {}});
        assert_eq!(
            call.clone()
                .accept(Seat::CALLEE, "B", (Links::new(), &no_media), at(1_010_000), &mut out)
                .unwrap_err(),
            Refusal::Bad
        );
        assert_eq!(
            call.clone()
                .accept(Seat::CALLEE, "B", (Links::new(), &json!({})), at(1_010_000), &mut out)
                .unwrap_err(),
            Refusal::Bad
        );
        let mut out = Out::default();
        let answer = call
            .accept(
                Seat::CALLEE,
                "B",
                (Links::new(), &acceptance()),
                at(1_010_000),
                &mut out,
            )
            .unwrap();
        assert_eq!(answer["callAcceptanceAcknowledgement"]["callKeepAliveInterval"], 2700);
        assert_eq!(
            paths(&out),
            [
                ("B2", "end"),
                ("A", "acceptance"),
                ("A", "rosterUpdate"),
                ("B", "rosterUpdate")
            ]
        );
        assert_eq!(out.deliveries[0].body["callEnd"]["code"], 487);
        assert_eq!(
            out.deliveries[1].body["callAcceptance"]["mediaContent"]["blob"],
            "answer"
        );
        assert_eq!(call.parties[1].participant["endpointId"], "ep-b2");
        let roster = &out.deliveries[2].body;
        assert_eq!(roster["participants"].as_object().unwrap().len(), 2);
        assert_eq!(
            roster["participants"]["8:orgid:b"]["endpoints"]["ep-b2"]["participantId"],
            "p-b"
        );
        assert_eq!((call.phase, call.due_at), (Phase::Connected, 1_010 + 2700 + 600));
        // answered: nothing rings any more
        assert_eq!(
            call.accept(
                Seat::CALLEE,
                "B2",
                (Links::new(), &acceptance()),
                at(1_011_000),
                &mut out
            )
            .unwrap_err(),
            Refusal::Gone
        );
    }

    #[test]
    fn a_decline_and_a_cancel_end_the_ring_as_a_missed_call() {
        let mut call = ringing();
        call.attach(Seat::CALLEE, "B", links("B", &["end"]), at(1_005_000))
            .unwrap();
        let mut declined = call.clone();
        let mut out = Out::default();
        assert_eq!(
            declined.reject(Seat::CALLER, &json!({}), at(1_006_000), &mut out),
            Err(Refusal::Forbidden)
        );
        declined
            .reject(Seat::CALLEE, &json!({}), at(1_006_000), &mut out)
            .unwrap();
        assert_eq!(paths(&out), [("B", "end"), ("A", "end")]);
        assert_eq!(out.deliveries[1].body["callEnd"]["code"], 603);
        assert_eq!(out.deliveries[1].body["callEnd"]["sender"]["id"], "8:orgid:b");
        assert_eq!(out.ended, Some(Outcome::Missed));
        assert_eq!((declined.phase, declined.due_at), (Phase::Ended, 1_006 + 300));
        // the callee's own leave while it rings changes nothing; the caller's cancels
        let mut out = Out::default();
        let before = call.clone();
        call.leave(Seat::CALLEE, &json!({}), at(1_006_000), &mut out);
        assert!(call == before && out.deliveries.is_empty());
        let leave = json!({"callTransactionEnd": {"code": 487}});
        call.leave(Seat::CALLER, &leave, at(1_007_000), &mut out);
        assert_eq!(paths(&out), [("A", "end"), ("B", "end")]);
        assert_eq!(out.deliveries[1].body["callEnd"]["code"], 487);
        assert_eq!(out.ended, Some(Outcome::Missed));
        // over: a second leave tells nobody
        let mut out = Out::default();
        call.leave(Seat::CALLER, &leave, at(1_008_000), &mut out);
        assert!(out.deliveries.is_empty() && out.ended.is_none());
    }

    #[test]
    fn a_hang_up_ends_the_call_for_both_with_its_length() {
        let mut call = connected();
        let mut out = Out::default();
        // an endpoint of b's whose answer lost to B's leaves: the call goes on
        let lost = json!({"participants": {"from": {"id": "8:orgid:b", "endpointId": "ep-b9"}}});
        let before = call.clone();
        call.leave(Seat::CALLEE, &lost, at(1_010_500), &mut out);
        assert!(call == before && out.deliveries.is_empty() && out.ended.is_none());
        // the caller names its own endpoint: its leave ends the call whatever it names
        let mut caller_left = call.clone();
        caller_left.leave(Seat::CALLER, &lost, at(1_011_000), &mut out);
        assert_eq!(caller_left.phase, Phase::Ended);
        let mut out = Out::default();
        let won = json!({"participants": {"from": {"id": "8:orgid:b", "endpointId": "ep-b2"}}});
        call.clone().leave(Seat::CALLEE, &won, at(1_011_000), &mut out);
        assert_eq!(out.ended, Some(Outcome::Talked(1)));
        let mut out = Out::default();
        call.leave(Seat::CALLEE, &json!({}), at(1_070_500), &mut out);
        assert_eq!(paths(&out), [("A", "end"), ("B", "end")]);
        assert_eq!(out.deliveries[0].body["callEnd"]["sender"]["id"], "8:orgid:b");
        assert_eq!(out.ended, Some(Outcome::Talked(60)));
        assert_eq!(call.keep_alive(Seat::CALLER, at(1_071_000)), Err(Refusal::Gone));
    }

    #[test]
    fn the_sweeper_ends_a_ring_and_a_call_without_keep_alives_only_once_due() {
        let mut call = ringing();
        call.attach(Seat::CALLEE, "B", links("B", &["end"]), at(1_005_000))
            .unwrap();
        let mut out = Out::default();
        assert!(!call.lapse(at(1_029_000), &mut out));
        assert!(call.lapse(at(1_030_000), &mut out));
        assert_eq!(paths(&out), [("A", "end"), ("B", "end")]);
        assert_eq!(out.deliveries[0].body["callEnd"]["code"], 480);
        assert_eq!(
            out.deliveries[1].body["callEnd"]["phrase"],
            "Call cancelled on timeout."
        );
        assert_eq!(out.ended, Some(Outcome::Missed));
        assert!(!call.lapse(at(9_999_000), &mut Out::default()), "it is over");

        let mut call = connected();
        assert_eq!(call.keep_alive(Seat::CALLEE, at(2_000_000)), Ok(()));
        assert_eq!(call.due_at, 2_000 + 2700 + 600);
        let mut out = Out::default();
        assert!(!call.lapse(at(5_299_000), &mut out));
        assert!(call.lapse(at(5_300_000), &mut out));
        assert_eq!(out.deliveries[0].body["callEnd"]["code"], 408);
        assert_eq!(out.ended, Some(Outcome::Talked(4290)));
        assert_eq!(
            ringing().keep_alive(Seat::CALLER, at(1_001_000)),
            Err(Refusal::Forbidden)
        );
    }

    #[test]
    fn a_mute_is_told_to_both_once_the_call_is_up() {
        let muted = json!({"endpointState": {"state": {"isMuted": true}, "endpointStateSequenceNumber": 4}});
        let mut call = ringing();
        let mut out = Out::default();
        call.endpoint_state(Seat::CALLEE, &muted, at(1_001_000), &mut out)
            .unwrap();
        assert!(
            call.parties[1].muted && out.deliveries.is_empty(),
            "nobody is told while it rings"
        );
        assert_eq!(
            call.endpoint_state(Seat::CALLEE, &json!({}), at(1_001_000), &mut out),
            Err(Refusal::Bad)
        );
        let mut call = connected();
        call.endpoint_state(Seat::CALLER, &muted, at(1_020_000), &mut out)
            .unwrap();
        assert_eq!(paths(&out), [("A", "rosterUpdate"), ("B", "rosterUpdate")]);
        let state = &out.deliveries[0].body["participants"]["8:orgid:a"]["endpoints"]["ep-a"]["endpointState"];
        assert_eq!(state["state"]["isMuted"], true);
        assert_eq!(state["endpointStateSequenceNumber"], 4);
        // the same state again tells nobody
        let mut out = Out::default();
        call.endpoint_state(Seat::CALLER, &muted, at(1_021_000), &mut out)
            .unwrap();
        assert!(out.deliveries.is_empty());
        // an older state that comes after a newer one changes nothing
        let late = json!({"endpointState": {"state": {"isMuted": false}, "endpointStateSequenceNumber": 3}});
        call.endpoint_state(Seat::CALLER, &late, at(1_021_500), &mut out)
            .unwrap();
        assert!(call.parties[0].muted && call.parties[0].state_sequence == 4 && out.deliveries.is_empty());
        call.leave(Seat::CALLER, &json!({}), at(1_022_000), &mut out);
        // one that was on its way when the call ended is taken and tells nobody
        let mut out = Out::default();
        assert_eq!(
            call.endpoint_state(Seat::CALLER, &muted, at(1_023_000), &mut out),
            Ok(())
        );
        call.more_links(Seat::CALLER, "A", Links::new());
        assert!(out.deliveries.is_empty());
    }

    #[test]
    fn later_callbacks_are_kept_only_for_an_endpoint_of_the_call() {
        let mut call = connected();
        call.more_links(Seat::CALLER, "Other", links("Other", &["mediaRenegotiation"]));
        assert!(call.parties[0].link("mediaRenegotiation").is_none());
        call.more_links(Seat::CALLER, "A", links("A", &["mediaRenegotiation"]));
        assert_eq!(
            call.parties[0].link("mediaRenegotiation"),
            Some(link("A", "mediaRenegotiation").as_str())
        );
    }

    fn offer() -> Value {
        json!({"mediaNegotiation": {"callModalities": ["Audio", "Video"],
            "mediaContent": {"blob": "offer 2", "contentType": "application/sdp-ngc-1.0"}}})
    }

    #[test]
    fn a_renegotiation_is_forwarded_answered_and_acknowledged() {
        let mut call = connected();
        let mut out = Out::default();
        let from_a = links("A", &["mediaAnswer", "rejection"]);
        assert_eq!(
            call.renegotiate(Seat::CALLER, "A", (from_a.clone(), &json!({})), at(1_020_000), &mut out),
            Err(Refusal::Bad)
        );
        assert_eq!(
            call.renegotiate(Seat::CALLER, "A", (Links::new(), &offer()), at(1_020_000), &mut out),
            Err(Refusal::Bad)
        );
        call.renegotiate(Seat::CALLER, "A", (from_a.clone(), &offer()), at(1_020_000), &mut out)
            .unwrap();
        assert_eq!(paths(&out), [("B", "mediaRenegotiation")]);
        let told = &out.deliveries[0].body["mediaNegotiation"];
        assert_eq!(told["mediaContent"]["blob"], "offer 2");
        assert_eq!(told["callModalities"], json!(["Audio", "Video"]));
        assert!(
            told["links"]["mediaAnswer"]
                .as_str()
                .unwrap()
                .ends_with("/cc/v1/negotiations/call/1/t/1/answer")
        );
        // another while it waits is turned away with 491, at the offerer's rejection callback
        let mut out = Out::default();
        call.renegotiate(Seat::CALLER, "A", (from_a.clone(), &offer()), at(1_021_000), &mut out)
            .unwrap();
        assert_eq!(paths(&out), [("A", "rejection")]);
        assert_eq!(out.deliveries[0].body["mediaNegotiationFailure"]["code"], 491);
    }

    #[test]
    fn a_renegotiations_answer_goes_back_and_its_acknowledgement_to_the_answerer() {
        let mut call = connected();
        let from_a = links("A", &["mediaAnswer", "rejection"]);
        call.renegotiate(
            Seat::CALLER,
            "A",
            (from_a, &offer()),
            at(1_020_000),
            &mut Out::default(),
        )
        .unwrap();
        // only the far end answers it
        let answer = json!({"mediaAnswer": {"mediaContent": {"blob": "answer 2", "contentType": "x"}}});
        let mut out = Out::default();
        assert_eq!(
            call.answer(Seat::CALLER, 1, ("A", Links::new(), &answer), at(1_022_000), &mut out),
            Err(Refusal::Gone)
        );
        assert_eq!(
            call.answer(
                Seat::CALLEE,
                1,
                ("B", Links::new(), &json!({})),
                at(1_022_000),
                &mut out
            ),
            Err(Refusal::Bad)
        );
        call.answer(Seat::CALLEE, 1, ("B", Links::new(), &answer), at(1_022_000), &mut out)
            .unwrap();
        assert_eq!(paths(&out), [("A", "mediaAnswer")]);
        assert_eq!(out.deliveries[0].body["mediaAnswer"]["sender"]["id"], "8:orgid:b");
        assert!(
            out.deliveries[0].body["mediaAnswer"]["links"]["mediaAcknowledgement"]
                .as_str()
                .unwrap()
                .ends_with("/cc/v1/mediaAnswers/call/1/t/1/acknowledge")
        );
        // the answerer's own acknowledgement tells nobody; the offerer's tells the answerer
        let mut out = Out::default();
        call.acknowledge_answer(Seat::CALLEE, 1, at(1_023_000), &mut out)
            .unwrap();
        assert!(out.deliveries.is_empty());
        // nor does the offerer's of another negotiation
        call.acknowledge_answer(Seat::CALLER, 2, at(1_023_000), &mut out)
            .unwrap();
        assert!(out.deliveries.is_empty());
        call.acknowledge_answer(Seat::CALLER, 1, at(1_023_000), &mut out)
            .unwrap();
        assert_eq!(paths(&out), [("B", "mediaAcknowledgement")]);
        // settled: an answer now has nothing to answer
        assert_eq!(
            call.answer(Seat::CALLEE, 1, ("B", Links::new(), &answer), at(1_024_000), &mut out),
            Err(Refusal::Gone)
        );
    }

    #[test]
    fn a_renegotiation_the_far_end_cant_take_or_turns_away_is_told_to_its_offerer() {
        let mut call = connected();
        let mut out = Out::default();
        // the caller handed out no renegotiation callback yet: b's offer has nowhere to go
        let from_b = links("B", &["mediaAnswer", "rejection"]);
        call.renegotiate(Seat::CALLEE, "B", (from_b, &offer()), at(1_020_000), &mut out)
            .unwrap();
        assert_eq!(paths(&out), [("B", "rejection")]);
        assert_eq!(out.deliveries[0].body["mediaNegotiationFailure"]["code"], 480);
        assert!(call.negotiation.is_none());
        // a's is forwarded and b turns it away
        let mut out = Out::default();
        let from_a = links("A", &["mediaAnswer", "rejection"]);
        call.renegotiate(Seat::CALLER, "A", (from_a.clone(), &offer()), at(1_021_000), &mut out)
            .unwrap();
        assert_eq!(
            call.turn_away(Seat::CALLER, 1, &Value::Null, at(1_022_000), &mut out),
            Err(Refusal::Gone)
        );
        let mut out = Out::default();
        call.turn_away(Seat::CALLEE, 1, &Value::Null, at(1_022_000), &mut out)
            .unwrap();
        assert_eq!(paths(&out), [("A", "rejection")]);
        assert_eq!(out.deliveries[0].body["mediaNegotiationFailure"]["code"], 488);
        // one left unanswered gives way to the next after its wait
        call.renegotiate(Seat::CALLER, "A", (from_a.clone(), &offer()), at(1_030_000), &mut out)
            .unwrap();
        let mut out = Out::default();
        call.renegotiate(Seat::CALLER, "A", (from_a, &offer()), at(1_061_000), &mut out)
            .unwrap();
        assert_eq!(paths(&out), [("B", "mediaRenegotiation")]);
        // what comes late for the one that gave way answers nothing of the one that waits now
        let answer = json!({"mediaAnswer": {"mediaContent": {"blob": "late", "contentType": "x"}}});
        let mut out = Out::default();
        assert_eq!(
            call.answer(Seat::CALLEE, 2, ("B", Links::new(), &answer), at(1_062_000), &mut out),
            Err(Refusal::Gone)
        );
        assert_eq!(
            call.turn_away(Seat::CALLEE, 2, &Value::Null, at(1_062_000), &mut out),
            Err(Refusal::Gone)
        );
        assert!(out.deliveries.is_empty() && call.negotiation.as_ref().is_some_and(|waiting| waiting.number == 3));
        let own = json!({"mediaNegotiationFailure": {"code": 488}});
        call.turn_away(Seat::CALLEE, 3, &own, at(1_062_000), &mut out).unwrap();
        assert_eq!(out.deliveries[0].body, own);
        assert_eq!(ringing().described(Seat::CALLER), Err(Refusal::Forbidden));
        assert_eq!(call.described(Seat::CALLER), Ok(()));
    }

    #[test]
    fn a_hold_brings_the_music_into_both_rosters_until_its_holder_resumes() {
        let mut call = connected();
        let mut out = Out::default();
        let answer = call
            .hold(
                Seat::CALLER,
                "A",
                links("A", &["holdCompletion"]),
                at(1_020_000),
                &mut out,
            )
            .unwrap();
        assert!(
            answer["holdResponse"]["links"]["resume"]
                .as_str()
                .unwrap()
                .ends_with("/cc/v1/holdSession/call/1/a6/1/resume")
        );
        assert_eq!(
            paths(&out),
            [("A", "rosterUpdate"), ("B", "rosterUpdate"), ("A", "holdCompletion")]
        );
        let music = &out.deliveries[1].body["participants"][MUSIC_ON_HOLD];
        assert_eq!(music["state"], "active");
        assert_eq!(music["endpoints"].as_object().unwrap().len(), 1);
        // held: the far end neither holds nor resumes it; the holder's second hold tells the roster nothing new
        let mut out = Out::default();
        assert_eq!(
            call.hold(Seat::CALLEE, "B", Links::new(), at(1_021_000), &mut out)
                .unwrap_err(),
            Refusal::Forbidden
        );
        assert_eq!(
            call.resume(Seat::CALLEE, "B", Links::new(), at(1_021_000), &mut out),
            Err(Refusal::Gone)
        );
        call.hold(Seat::CALLER, "A", Links::new(), at(1_021_000), &mut out)
            .unwrap();
        assert_eq!(paths(&out), [("A", "holdCompletion")]);
        let mut out = Out::default();
        call.resume(
            Seat::CALLER,
            "A",
            links("A", &["resumeCompletion"]),
            at(1_022_000),
            &mut out,
        )
        .unwrap();
        assert_eq!(
            paths(&out),
            [("A", "rosterUpdate"), ("B", "rosterUpdate"), ("A", "resumeCompletion")]
        );
        let music = &out.deliveries[0].body["participants"][MUSIC_ON_HOLD];
        assert_eq!(
            (music["state"].as_str(), music["endpoints"].as_object().map(Map::len)),
            (Some("inactive"), Some(0))
        );
        assert_eq!(
            call.resume(Seat::CALLER, "A", Links::new(), at(1_023_000), &mut out),
            Err(Refusal::Gone)
        );
    }

    #[test]
    fn a_call_names_its_parties_sides_and_phases() {
        let call = ringing();
        assert_eq!(call.seat_of("a"), Some(Seat::CALLER));
        assert_eq!(call.seat_of("b"), Some(Seat::CALLEE));
        assert_eq!(call.seat_of("c"), None);
        assert_eq!(
            [Phase::Ringing, Phase::Connected, Phase::Ended].map(Phase::name),
            ["ringing", "connected", "ended"]
        );
        let kept: Call = serde_json::from_str(&serde_json::to_string(&call).unwrap()).unwrap();
        assert_eq!(kept, call);
    }
}
