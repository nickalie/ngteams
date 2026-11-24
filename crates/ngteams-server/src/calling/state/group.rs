//! A group call's own steps. The call is up from its start (the caller is in it at once, through the media node) and
//! goes on while anyone is in it; each party rings, answers, declines, joins and leaves alone. What the media node
//! answered is given to a step; the legs a step lets go of and the room it ends are in [`Out`], for the operation to
//! drop once the step is stored.

use serde_json::Value;

use super::{
    At, Call, KEEP_ALIVE, KEEP_ALIVE_SLACK, Kind, Leg, NEGOTIATION_WAIT_MS, Out, Outcome, Party, Pending, Phase,
    Refusal, Reserved, Seat, Showing, Status, Watcher,
};
use crate::calling::media::Sources;
use crate::calling::wire::{self, Links};

/// The first party's first source id; each party's sources are this far apart: its audio, video, sharing and data, then
/// its receiving lines' stream ids ([`Sources::first_slot`], [`Sources::SLOTS`] of them).
const FIRST_SOURCE: u32 = 1000;
const SOURCE_STEP: u32 = 4 + Sources::SLOTS;

impl Call {
    /// The sources of the next party to get a leg.
    pub(crate) fn next_sources(&mut self) -> Sources {
        let audio = self.next_source.max(FIRST_SOURCE);
        self.next_source = audio + SOURCE_STEP;
        Sources {
            audio,
            video: audio + 1,
            sharing: audio + 2,
            data: audio + 3,
        }
    }

    /// The role `party` has in a meeting's call, as its roster names it: the meeting's organizer, everyone else a
    /// presenter, or an attendee while only the organizer presents. `None` in a call that is no meeting's.
    pub(crate) fn role_of(&self, party: &Party) -> Option<&'static str> {
        let meeting = self.meeting.as_ref()?;
        Some(if party.id == meeting.organizer {
            "organizer"
        } else if meeting.controls.attendees() {
            "attendee"
        } else {
            "presenter"
        })
    }

    /// The seats a whole roster lists: whoever is or was in the call, and whoever waits in its lobby.
    pub(crate) fn rostered(&self) -> Vec<Seat> {
        (0..self.parties.len())
            .map(Seat)
            .filter(|seat| {
                let party = self.party(*seat);
                party.joined.is_some() || party.status == Status::Lobby
            })
            .collect()
    }

    /// The seats of the parties in the call, and of those waiting in its lobby: while any is, the call goes on.
    fn present_seats(&self) -> Vec<Seat> {
        (0..self.parties.len())
            .map(Seat)
            .filter(|seat| matches!(self.party(*seat).status, Status::InCall | Status::Lobby))
            .collect()
    }

    /// Looks at the call next at the earliest ring's end or its keep-alive's.
    pub(crate) fn reschedule(&mut self) {
        let rings = self
            .parties
            .iter()
            .filter(|party| party.status == Status::Ringing)
            .filter_map(|party| party.ring_due);
        self.due_at = rings.fold(self.alive_due, i64::min);
    }

    /// The call as its start makes it: the caller in it with its leg, each callee ringing until `ring_due`.
    pub(crate) fn started_group(&mut self, at: At<'_>, ring_due: i64) {
        self.kind = Kind::Group;
        self.phase = Phase::Connected;
        self.connected_at = Some(at.now);
        self.alive_due = at.seconds() + KEEP_ALIVE + KEEP_ALIVE_SLACK;
        let joined = wire::iso(at.now);
        for (number, party) in self.parties.iter_mut().enumerate() {
            if number == Seat::CALLER.0 {
                party.status = Status::InCall;
                party.joined = Some(joined.clone());
            } else {
                party.status = Status::Ringing;
                party.ring_due = Some(ring_due);
                party.rings += 1;
            }
        }
        self.reschedule();
    }

    /// What the start tells the caller: the media node's `answer` in its acceptance, and the first roster.
    pub(crate) fn tell_started(&mut self, origin: &str, answer: &Value, out: &mut Out) {
        let caller = self.party(Seat::CALLER);
        let accepted = wire::mixer_acceptance(origin, self, Seat::CALLER, answer);
        out.call_back(origin, caller.link("acceptance"), accepted);
        self.tell_group(origin, &[Seat::CALLER], Some(Seat::CALLER), out);
    }

    /// The leg a ringing party's answer is for: the one it rings on, while no join has reserved its seat over it (the
    /// join's leg is then the one the party gets).
    pub(crate) fn answerable(&self, seat: Seat) -> Result<&str, Refusal> {
        self.ringing(seat)?;
        match &self.party(seat).leg {
            Some(leg) if leg.before.is_none() => Ok(&leg.id),
            _ => Err(Refusal::Gone),
        }
    }

    /// A ringing party's endpoint answers on its leg `leg` (the answer already with the media node): the party is in
    /// the call, its other endpoints stop ringing, and the roster tells everyone (the party the whole of it). An answer
    /// for a leg that isn't [`Self::answerable`] any more is refused: a join reserved the seat over it since.
    pub(crate) fn group_accept(
        &mut self,
        seat: Seat,
        (leg, endpoint): (&str, &str),
        (links, body): (Links, &Value),
        at: At<'_>,
        out: &mut Out,
    ) -> Result<Value, Refusal> {
        if self.answerable(seat)? != leg {
            return Err(Refusal::Gone);
        }
        let acceptance = body.get("callAcceptance").ok_or(Refusal::Bad)?;
        let party = self.party_mut(seat);
        party.keep(endpoint, links);
        for other in party.endpoints.iter().filter(|known| known.endpoint != endpoint) {
            let end = wire::end(487, "Call cancelled as it was accepted by another fork.", None);
            out.call_back(at.origin, other.links.get("end").map(String::as_str), end);
        }
        party.endpoints.retain(|known| known.endpoint == endpoint);
        party.active = Some(endpoint.to_owned());
        if let Some(by) = acceptance.get("acceptedBy").filter(|by| by.is_object()) {
            party.participant = wire::named_by(by, party);
        }
        party.status = Status::InCall;
        party.ring_due = None;
        party.joined = Some(wire::iso(at.now));
        Self::settle(party, out);
        self.reschedule();
        self.tell_group(at.origin, &[seat], Some(seat), out);
        self.tell_presence(at.origin, wire::JOINED, seat, out);
        Ok(wire::accept_answer(at.origin, self, seat))
    }

    /// A ringing party declines: its endpoints stop ringing and its leg goes; the call goes on for the others.
    pub(crate) fn group_reject(&mut self, seat: Seat, body: &Value, at: At<'_>, out: &mut Out) -> Result<(), Refusal> {
        self.ringing(seat)?;
        let code = body.pointer("/callEnd/code").and_then(Value::as_i64).unwrap_or(603);
        let party = self.party_mut(seat);
        for known in &party.endpoints {
            let end = wire::end(code, "CallEndReasonLocalUserInitiated", Some(&party.participant));
            out.call_back(at.origin, known.links.get("end").map(String::as_str), end);
        }
        party.status = Status::Declined;
        Self::let_go(party, out);
        self.reschedule();
        Ok(())
    }

    /// A party in the call leaves: it leaves the roster and its leg goes. The last one out ends the call, and the rings
    /// still going end with it. A leave naming another endpoint of the party's than the one in the call changes
    /// nothing (it comes from an endpoint whose answer lost to the other's), nor does a leave of anyone not in it.
    pub(crate) fn group_leave(&mut self, seat: Seat, body: &Value, at: At<'_>, out: &mut Out) {
        let party = self.party(seat);
        let named = body.pointer("/participants/from/endpointId").and_then(Value::as_str);
        let own = party
            .participant
            .get("endpointId")
            .and_then(Value::as_str)
            .filter(|own| *own != wire::ZERO_ENDPOINT);
        let lost_fork = matches!((named, own), (Some(named), Some(own)) if named != own);
        let present = matches!(party.status, Status::InCall | Status::Lobby);
        if self.phase == Phase::Ended || !present || lost_fork {
            return;
        }
        self.take_out(seat, out);
        if self.present_seats().is_empty() {
            let end = wire::end(487, "Call ended as everyone left.", None);
            self.end_group(at, &end, out);
        } else {
            self.tell_group(at.origin, &[seat], None, out);
            self.tell_presence(at.origin, wire::LEFT, seat, out);
        }
    }

    /// The media node lost the party's media: it is taken out of the call, as a leave would, and its endpoint is told.
    pub(crate) fn media_lost(&mut self, seat: Seat, at: At<'_>, out: &mut Out) {
        if self.phase == Phase::Ended || self.party(seat).status != Status::InCall {
            return;
        }
        let end = wire::failure(408, "The call's media was lost.");
        out.call_back(at.origin, self.party(seat).link("end"), end);
        let leave = serde_json::json!({});
        self.group_leave(seat, &leave, at, out);
    }

    /// The seat whose leg (or a leg it was reserved over) is `leg`.
    pub(crate) fn seat_of_leg(&self, leg: &str) -> Option<Seat> {
        self.parties
            .iter()
            .position(|party| party.leg.as_ref().is_some_and(|held| held.ids().any(|id| id == leg)))
            .map(Seat)
    }

    /// The call's media node is gone: the call ends for everyone in it (503) and every ring ends; its room went with
    /// the node, so nothing is dropped there. `false` when nothing changed.
    pub(crate) fn media_gone(&mut self, at: At<'_>, out: &mut Out) -> bool {
        if self.phase == Phase::Ended || self.kind != Kind::Group {
            return false;
        }
        let end = wire::failure(503, "The call's media node is gone.");
        self.end_group(at, &end, out);
        out.released.clear();
        out.room_over = false;
        true
    }

    /// The party leaves the call: out of the roster, its leg let go.
    pub(super) fn take_out(&mut self, seat: Seat, out: &mut Out) {
        let party = self.party_mut(seat);
        party.status = Status::Left;
        party.waiting = None;
        party.active = None;
        party.endpoints.clear();
        party.answered = None;
        Self::let_go(party, out);
    }

    fn let_go(party: &mut Party, out: &mut Out) {
        party.ring_due = None;
        party.showing = Showing::default();
        party.presenting = false;
        party.hand = None;
        if let Some(leg) = party.leg.take() {
            out.released.extend(leg.ids().map(str::to_owned));
        }
    }

    /// The party's leg is its own now: the legs it was reserved over go.
    fn settle(party: &mut Party, out: &mut Out) {
        if let Some(before) = party.leg.as_mut().and_then(|leg| leg.before.take()) {
            out.released.extend(before.ids().map(str::to_owned));
        }
    }

    /// Ends the call: everyone in it gets `end`, every ring still going ends, every leg and the room go.
    pub(super) fn end_group(&mut self, at: At<'_>, end: &Value, out: &mut Out) {
        let rung = wire::end(487, "Call cancelled as the call ended.", None);
        for seat in 0..self.parties.len() {
            let party = self.party_mut(Seat(seat));
            match party.status {
                Status::InCall | Status::Lobby => out.call_back(at.origin, party.link("end"), end.clone()),
                Status::Ringing => {
                    for known in &party.endpoints {
                        out.call_back(at.origin, known.links.get("end").map(String::as_str), rung.clone());
                    }
                    party.status = Status::Missed;
                }
                _ => {}
            }
            if matches!(party.status, Status::InCall | Status::Lobby) {
                party.status = Status::Left;
                party.waiting = None;
            }
            Self::let_go(party, out);
        }
        out.room_over = true;
        let talked = (at.now - self.connected_at.unwrap_or(at.now)) / 1000;
        self.finish(at, Outcome::Talked(talked.max(0)), out);
    }

    /// What the sweeper makes of a group call that is due: the rings that are due end (the call goes on), a call whose
    /// keep-alives stopped is given up. `false` when nothing changed.
    pub(crate) fn group_lapse(&mut self, at: At<'_>, out: &mut Out) -> bool {
        if self.phase == Phase::Ended {
            return false;
        }
        if self.alive_due <= at.seconds() {
            let end = wire::failure(408, "Call timed out without a keep-alive.");
            self.end_group(at, &end, out);
            return true;
        }
        let rung = wire::end(487, "Call cancelled on timeout.", None);
        let mut changed = false;
        for party in &mut self.parties {
            let due = party.status == Status::Ringing && party.ring_due.is_some_and(|due| due <= at.seconds());
            if due {
                for known in &party.endpoints {
                    out.call_back(at.origin, known.links.get("end").map(String::as_str), rung.clone());
                }
                party.status = Status::Missed;
                Self::let_go(party, out);
                changed = true;
            }
        }
        self.reschedule();
        changed
    }

    /// Makes `account` a seat with a new leg `leg`, reserved over the seat's leg (a ringing party's, which its
    /// invitation's offer is for): the first step of a join or an add, before the media node is asked for the leg.
    /// Someone in the call already is refused.
    pub(crate) fn reserve(&mut self, account: &Party, leg: &str) -> Result<Reserved, Refusal> {
        if self.phase == Phase::Ended {
            return Err(Refusal::Gone);
        }
        let (seat, made) = match self.seat_of(&account.id) {
            Some(seat) if self.party(seat).status == Status::InCall => return Err(Refusal::Forbidden),
            Some(seat) => (seat, false),
            None => {
                self.parties.push(Party {
                    status: Status::Left,
                    ..account.clone()
                });
                (Seat(self.parties.len() - 1), true)
            }
        };
        let sources = self.next_sources();
        let party = self.party_mut(seat);
        let named = (
            std::mem::replace(&mut party.participant, account.participant.clone()),
            std::mem::replace(&mut party.name, account.name.clone()),
        );
        let before = party.leg.take().map(Box::new);
        party.leg = Some(Leg {
            id: leg.to_owned(),
            sources,
            before,
        });
        Ok(Reserved {
            seat,
            sources,
            named,
            made,
        })
    }

    /// [`Self::reserve`] for an add: a party that still rings is left ringing as it is (`None`), its invitation's offer
    /// being the one its answer is made for. One waiting in a meeting's lobby is refused: a host lets it in instead.
    pub(crate) fn reserve_added(&mut self, account: &Party, leg: &str) -> Result<Option<Reserved>, Refusal> {
        let status = self.seat_of(&account.id).map(|seat| self.party(seat).status);
        if status == Some(Status::Lobby) {
            return Err(Refusal::Forbidden);
        }
        let ringing = status == Some(Status::Ringing);
        if ringing && self.phase != Phase::Ended {
            return Ok(None);
        }
        self.reserve(account, leg).map(Some)
    }

    /// Undoes `reserved`, whose leg `leg` the media node didn't make or whose step wasn't stored, and lets `leg` go.
    /// The seat gets back the leg and the name it had; a seat made for it goes when no seat was made after it (else it
    /// stays, without a leg and in no roster, for that account's next try). A later reservation over `leg` is over
    /// the leg `leg` was reserved over instead. A party that got in on `leg` keeps it.
    pub(crate) fn unreserve(&mut self, reserved: &Reserved, leg: &str, out: &mut Out) {
        let seat = reserved.seat;
        let party = self.party_mut(seat);
        let Some(kept) = party.leg.as_mut() else {
            out.released.push(leg.to_owned());
            return;
        };
        if kept.id != leg {
            kept.forget(leg);
            out.released.push(leg.to_owned());
            return;
        }
        if party.status == Status::InCall {
            return;
        }
        let before = kept.before.take();
        party.leg = before.map(|before| *before);
        party.participant.clone_from(&reserved.named.0);
        party.name.clone_from(&reserved.named.1);
        out.released.push(leg.to_owned());
        if reserved.made && seat.0 + 1 == self.parties.len() {
            self.parties.pop();
        }
    }

    /// The party at `seat`, its leg `leg` made, joins with its endpoint: the media node's `answer` goes to its
    /// acceptance, the roster tells everyone (the joiner the whole of it).
    pub(crate) fn join(
        &mut self,
        seat: Seat,
        (leg, endpoint, links): (&str, &str, Links),
        answer: &Value,
        at: At<'_>,
        out: &mut Out,
    ) -> Result<(), Refusal> {
        let party = self.party(seat);
        if self.phase == Phase::Ended || party.leg.as_ref().is_none_or(|kept| kept.id != leg) {
            return Err(Refusal::Gone);
        }
        if party.status == Status::InCall {
            return Err(Refusal::Forbidden);
        }
        let was_ringing = party.status == Status::Ringing;
        let party = self.party_mut(seat);
        if was_ringing {
            for other in party.endpoints.iter().filter(|known| known.endpoint != endpoint) {
                let end = wire::end(487, "Call cancelled as it was joined from another fork.", None);
                out.call_back(at.origin, other.links.get("end").map(String::as_str), end);
            }
        }
        party.endpoints.retain(|known| known.endpoint == endpoint);
        party.keep(endpoint, links);
        party.active = Some(endpoint.to_owned());
        party.status = Status::InCall;
        party.waiting = None;
        party.ring_due = None;
        party.joined = Some(wire::iso(at.now));
        Self::settle(party, out);
        let accepted = wire::mixer_acceptance(at.origin, self, seat, answer);
        out.call_back(at.origin, self.party(seat).link("acceptance"), accepted);
        self.reschedule();
        self.tell_group(at.origin, &[seat], Some(seat), out);
        self.tell_presence(at.origin, wire::JOINED, seat, out);
        Ok(())
    }

    /// The party at `seat`, its leg `leg` made with the media node's offer, rings anew until `ring_due` (someone added
    /// it to the call).
    pub(crate) fn ring(&mut self, seat: Seat, leg: &str, ring_due: i64, out: &mut Out) -> Result<(), Refusal> {
        let party = self.party(seat);
        if self.phase == Phase::Ended || party.leg.as_ref().is_none_or(|kept| kept.id != leg) {
            return Err(Refusal::Gone);
        }
        if party.status == Status::InCall {
            return Err(Refusal::Forbidden);
        }
        let party = self.party_mut(seat);
        party.status = Status::Ringing;
        party.ring_due = Some(ring_due);
        party.rings += 1;
        party.endpoints.clear();
        party.active = None;
        Self::settle(party, out);
        self.reschedule();
        Ok(())
    }

    /// A chat member follows the roster from its endpoint (a subscription without media): told the whole roster now
    /// and its changes from then on.
    pub(crate) fn watch(
        &mut self,
        account: &str,
        endpoint: &str,
        links: Links,
        at: At<'_>,
        out: &mut Out,
    ) -> Result<(), Refusal> {
        if self.phase == Phase::Ended {
            return Err(Refusal::Gone);
        }
        let update = wire::roster(self, &self.rostered());
        out.call_back(at.origin, links.get("rosterUpdate").map(String::as_str), update);
        self.watchers
            .retain(|watcher| !(watcher.id == account && watcher.endpoint == endpoint));
        self.watchers.push(Watcher {
            id: account.to_owned(),
            endpoint: endpoint.to_owned(),
            links,
        });
        Ok(())
    }

    /// A party in the call offered its media again and the media node answered `answer`: the answer goes back to the
    /// party at its own callback. Never forwarded to anyone else.
    pub(crate) fn group_renegotiated(
        &mut self,
        seat: Seat,
        (endpoint, links): (&str, Links),
        answer: &Value,
        at: At<'_>,
        out: &mut Out,
    ) -> Result<(), Refusal> {
        self.in_call(seat)?;
        if !links.contains_key("mediaAnswer") {
            return Err(Refusal::Bad);
        }
        if self.turned_back(seat, &links, at, out) {
            return Ok(());
        }
        self.negotiations += 1;
        let number = self.negotiations;
        let party = self.party_mut(seat);
        party.keep(endpoint, links);
        party.answered = Some(Pending { number, at: at.now });
        let told = wire::mixer_media_answer(at.origin, self, seat, answer);
        out.call_back(at.origin, self.party(seat).link("mediaAnswer"), told);
        Ok(())
    }

    /// A party in the call described its media lines again (`updateMediaDescriptions`): what it sends now is told in
    /// the roster when it changed, so the others ask the media node for its camera or its screen.
    pub(crate) fn group_described(
        &mut self,
        seat: Seat,
        showing: Showing,
        at: At<'_>,
        out: &mut Out,
    ) -> Result<(), Refusal> {
        self.in_call(seat)?;
        let before = self.party(seat).showing;
        if before == showing {
            return Ok(());
        }
        let mut changed = vec![seat];
        if showing.sharing && !before.sharing {
            // as in Teams, a new share takes over the one going: that sharer's client stops it once it hears
            for (number, party) in self.parties.iter_mut().enumerate() {
                if party.presenting {
                    party.presenting = false;
                    changed.push(Seat(number));
                }
            }
        }
        let party = self.party_mut(seat);
        party.showing = showing;
        party.presenting = showing.sharing && (party.presenting || !before.sharing);
        self.tell_group(at.origin, &changed, None, out);
        Ok(())
    }

    /// Whether the party's last renegotiation answer waits for its acknowledgement at `now`. One that waited
    /// [`NEGOTIATION_WAIT_MS`] is taken as lost: the client never sends it again.
    pub(crate) fn waits_for_acknowledgement(&self, seat: Seat, now: i64) -> bool {
        self.party(seat)
            .answered
            .is_some_and(|answered| now - answered.at < NEGOTIATION_WAIT_MS)
    }

    /// Whether the party's renegotiation is turned away (`491` at its `rejection` callback): its last answer isn't
    /// acknowledged yet.
    pub(crate) fn turned_back(&self, seat: Seat, links: &Links, at: At<'_>, out: &mut Out) -> bool {
        if !self.waits_for_acknowledgement(seat, at.now) {
            return false;
        }
        let failure = wire::negotiation_failure(491, "Negotiation is in progress.");
        out.call_back(at.origin, links.get("rejection").map(String::as_str), failure);
        true
    }

    /// Tells the roster's entries of `changed`: to each party in the call at its endpoint in it, to each ringing one at
    /// every endpoint that subscribed while it rang, and to every watcher. `whole` is told the whole roster instead.
    pub(crate) fn tell_group(&mut self, origin: &str, changed: &[Seat], whole: Option<Seat>, out: &mut Out) {
        self.roster_sequence += 1;
        let delta = wire::roster(self, changed);
        let full = wire::roster(self, &self.rostered());
        for (number, party) in self.parties.iter().enumerate() {
            let update = if whole == Some(Seat(number)) { &full } else { &delta };
            match party.status {
                Status::InCall | Status::Lobby => out.call_back(origin, party.link("rosterUpdate"), update.clone()),
                Status::Ringing => {
                    for known in &party.endpoints {
                        out.call_back(
                            origin,
                            known.links.get("rosterUpdate").map(String::as_str),
                            update.clone(),
                        );
                    }
                }
                _ => {}
            }
        }
        for watcher in &self.watchers {
            out.call_back(
                origin,
                watcher.links.get("rosterUpdate").map(String::as_str),
                delta.clone(),
            );
        }
    }

    /// Tells each calling endpoint of every party in the call or ringing, but the one that changed, that `seat` came
    /// in or left (`evt` 115 or 116 at the endpoint's root).
    pub(super) fn tell_presence(&self, origin: &str, evt: u64, seat: Seat, out: &mut Out) {
        let told = wire::presence(origin, self, evt);
        let changed = self.party(seat).active.as_deref();
        for party in &self.parties {
            let endpoints = match party.status {
                Status::InCall | Status::Ringing => &party.endpoints,
                _ => continue,
            };
            for known in endpoints
                .iter()
                .filter(|known| Some(known.endpoint.as_str()) != changed)
            {
                out.deliveries.push(super::Delivery {
                    endpoint: known.endpoint.clone(),
                    path: String::new(),
                    body: told.clone(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use serde_json::json;

    use super::*;
    use crate::calling::media::MediaHome;
    use crate::calling::state::Delivery;

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

    fn party(id: &str) -> Party {
        Party {
            id: id.to_owned(),
            name: id.to_uppercase(),
            participant: json!({"id": format!("8:orgid:{id}"), "endpointId": format!("ep-{id}"),
                "participantId": format!("p-{id}")}),
            ..Party::default()
        }
    }

    fn at(now: i64) -> At<'static> {
        At { origin: ORIGIN, now }
    }

    fn answer() -> Value {
        json!({"blob": "answer", "contentType": "application/sdp-ngc-1.0"})
    }

    /// a's group call to b, c and d at second 1000: their rings end at 1030; b and c attached, d has no endpoint.
    fn started() -> (Call, Out) {
        let mut caller = party("a");
        caller.keep("A", links("A", &["acceptance", "end", "rosterUpdate"]));
        caller.active = Some("A".to_owned());
        let mut call = Call {
            id: "call".to_owned(),
            conversation: "conversation".to_owned(),
            phase: Phase::Connected,
            parties: vec![caller, party("b"), party("c"), party("d")],
            offer: Value::Null,
            started_at: 1_000_000,
            connected_at: None,
            ended_at: None,
            due_at: 0,
            roster_sequence: 0,
            negotiation: None,
            negotiations: 0,
            answered: None,
            held_by: None,
            chat: Some("19:group@thread.v2".to_owned()),
            accepted_as: None,
            kind: Kind::Group,
            media: Some(MediaHome {
                node: "n".into(),
                url: "http://n.invalid".into(),
                epoch: 1,
            }),
            next_source: 0,
            watchers: Vec::new(),
            alive_due: 0,
            meeting: None,
        };
        for seat in 0..4 {
            let sources = call.next_sources();
            call.party_mut(Seat(seat)).leg = Some(Leg {
                id: format!("leg{seat}"),
                sources,
                before: None,
            });
        }
        call.started_group(at(1_000_000), 1_030);
        let mut out = Out::default();
        call.tell_started(ORIGIN, &answer(), &mut out);
        for (seat, endpoint) in [(1, "B"), (1, "B2"), (2, "C")] {
            call.attach(
                Seat(seat),
                endpoint,
                links(endpoint, &["end", "rosterUpdate"]),
                at(1_001_000),
            )
            .unwrap();
        }
        (call, out)
    }

    fn accepted(call: &mut Call, seat: usize, endpoint: &str, now: i64) -> Out {
        let mut out = Out::default();
        let body = json!({"callAcceptance": {"acceptedBy": {"endpointId": format!("ep-{endpoint}")}}});
        let links = links(endpoint, &["mediaAnswer", "rejection"]);
        let leg = call.party(Seat(seat)).leg.clone().unwrap().id;
        call.group_accept(Seat(seat), (&leg, endpoint), (links, &body), at(now), &mut out)
            .unwrap();
        out
    }

    /// The ids of the seat's leg and of the legs it was reserved over.
    fn legs(call: &Call, seat: Seat) -> Vec<&str> {
        call.party(seat).leg.iter().flat_map(Leg::ids).collect()
    }

    /// The legs an undone reservation lets go of.
    fn undo(call: &mut Call, reserved: &Reserved, leg: &str) -> Vec<String> {
        let mut out = Out::default();
        call.unreserve(reserved, leg, &mut out);
        out.released
    }

    fn paths(out: &Out) -> Vec<(&str, &str)> {
        out.deliveries
            .iter()
            .map(|delivery| {
                let kind = delivery.path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
                (delivery.endpoint.as_str(), kind)
            })
            .collect()
    }

    fn evt(delivery: &Delivery) -> Option<u64> {
        delivery.body.get("evt").and_then(Value::as_u64)
    }

    #[test]
    fn a_start_takes_the_caller_in_at_once_and_rings_every_callee_until_its_due() {
        let (call, out) = started();
        assert_eq!(call.party(Seat::CALLER).status, Status::InCall);
        assert!((1..4).all(|seat| call.party(Seat(seat)).status == Status::Ringing));
        assert_eq!(call.due_at, 1_030, "the earliest ring's end");
        assert_eq!(paths(&out), [("A", "acceptance"), ("A", "rosterUpdate")]);
        let accepted = &out.deliveries[0].body["callAcceptance"];
        assert_eq!(accepted["mediaContent"]["fromMixer"], true);
        assert_eq!(accepted["mediaContent"]["mediaLegId"], "leg0");
        assert!(accepted["links"].get("hold").is_none(), "a group call has no hold");
        assert!(
            accepted["links"]["callLeg"]
                .as_str()
                .unwrap()
                .contains("/active/call/0/")
        );
        let roster = &out.deliveries[1].body["participants"]["8:orgid:a"];
        let streams = &roster["endpoints"]["ep-a"]["call"]["mediaStreams"];
        assert_eq!(streams[0]["sourceId"], 1000);
        assert_eq!(streams[1]["sourceId"], 1001);
        assert_eq!(streams[1]["direction"], "recvonly");
        let sources: Vec<u32> = (0..4)
            .map(|seat| call.party(Seat(seat)).leg.as_ref().unwrap().sources.audio)
            .collect();
        assert_eq!(sources, [1000, 1020, 1040, 1060]);
        // the caller, in the call, rings nowhere: its links aren't a ringing leg's
        let mut caller = call.clone();
        assert_eq!(
            caller
                .attach(Seat::CALLER, "A", links("A", &["end"]), at(1_002_000))
                .unwrap_err(),
            Refusal::Gone
        );
    }

    #[test]
    fn an_answer_takes_the_callee_in_and_tells_everyone_while_the_others_still_ring() {
        let (mut call, _) = started();
        let out = accepted(&mut call, 1, "B", 1_005_000);
        assert_eq!(call.party(Seat(1)).status, Status::InCall);
        assert_eq!(call.party(Seat(1)).participant["endpointId"], "ep-B");
        let told = paths(&out);
        // its other endpoint stops ringing, a and the ringing c get the change, b the whole roster; 115 to the others
        assert_eq!(
            &told[..4],
            [
                ("B2", "end"),
                ("A", "rosterUpdate"),
                ("B", "rosterUpdate"),
                ("C", "rosterUpdate")
            ]
        );
        assert_eq!(out.deliveries[2].body["participants"].as_object().unwrap().len(), 2);
        assert_eq!(out.deliveries[1].body["participants"].as_object().unwrap().len(), 1);
        let presence: Vec<&str> = out
            .deliveries
            .iter()
            .filter(|delivery| evt(delivery) == Some(wire::JOINED))
            .map(|delivery| delivery.endpoint.as_str())
            .collect();
        assert_eq!(presence, ["A", "C"]);
        assert_eq!(call.due_at, 1_030, "c and d still ring");
        // answered: it rings no more; a second answer is too late
        let body = json!({"callAcceptance": {}});
        assert_eq!(
            call.group_accept(
                Seat(1),
                ("leg1", "B2"),
                (Links::new(), &body),
                at(1_006_000),
                &mut Out::default()
            ),
            Err(Refusal::Gone)
        );
        assert_eq!(
            call.group_accept(
                Seat(2),
                ("leg2", "C"),
                (Links::new(), &json!({})),
                at(1_006_000),
                &mut Out::default()
            ),
            Err(Refusal::Bad)
        );
    }

    #[test]
    fn a_decline_and_a_ring_that_runs_out_let_their_legs_go_and_the_call_runs_on() {
        let (mut call, _) = started();
        let mut out = Out::default();
        call.reject(Seat(2), &json!({}), at(1_005_000), &mut out).unwrap();
        assert_eq!(paths(&out), [("C", "end")]);
        assert_eq!(out.deliveries[0].body["callEnd"]["code"], 603);
        assert_eq!(
            (call.party(Seat(2)).status, out.released.clone()),
            (Status::Declined, vec!["leg2".to_owned()])
        );
        assert_eq!(
            call.reject(Seat(2), &json!({}), at(1_005_000), &mut Out::default()),
            Err(Refusal::Gone)
        );
    }

    #[test]
    fn a_ring_that_runs_out_lets_its_leg_go_and_the_call_runs_on() {
        let (mut call, _) = started();
        call.reject(Seat(2), &json!({}), at(1_005_000), &mut Out::default())
            .unwrap();
        // nothing is due before 1030
        let mut out = Out::default();
        assert!(!call.lapse(at(1_029_000), &mut out));
        assert!(call.lapse(at(1_030_000), &mut out));
        assert_eq!(paths(&out), [("B", "end"), ("B2", "end")]);
        assert_eq!(out.released, ["leg1", "leg3"]);
        assert!(!out.room_over && call.phase == Phase::Connected);
        assert_eq!(call.due_at, call.alive_due, "only the keep-alive is due now");
    }

    #[test]
    fn a_call_whose_keep_alives_stopped_is_given_up() {
        let (mut call, _) = started();
        call.lapse(at(1_030_000), &mut Out::default());
        // a call whose keep-alives stopped is given up
        let mut out = Out::default();
        assert!(!call.lapse(at(1_100_000), &mut out), "nothing is due");
        assert!(call.lapse(at(call.alive_due * 1000), &mut out));
        assert_eq!(paths(&out), [("A", "end")]);
        assert_eq!(out.deliveries[0].body["callEnd"]["code"], 408);
        assert!(out.room_over && call.phase == Phase::Ended);
        assert!(!call.group_lapse(at(9_999_999_000), &mut Out::default()));
    }

    #[test]
    fn leaves_take_parties_out_until_the_last_one_ends_the_call() {
        let (mut call, _) = started();
        accepted(&mut call, 1, "B", 1_005_000);
        // a ringing party's leave, and one from an endpoint whose answer lost, change nothing
        let before = call.clone();
        call.leave(Seat(2), &json!({}), at(1_006_000), &mut Out::default());
        let lost = json!({"participants": {"from": {"endpointId": "ep-B2"}}});
        call.leave(Seat(1), &lost, at(1_006_000), &mut Out::default());
        assert_eq!(call, before);
        // the caller leaves: b goes on, told the caller left
        let mut out = Out::default();
        call.leave(Seat::CALLER, &json!({}), at(1_010_000), &mut out);
        assert_eq!(call.party(Seat::CALLER).status, Status::Left);
        assert_eq!(out.released, ["leg0"]);
        let entry = &out.deliveries[0].body["participants"]["8:orgid:a"];
        assert_eq!(
            (
                entry["state"].as_str(),
                entry["endpoints"].as_object().map(serde_json::Map::len)
            ),
            (Some("inactive"), Some(0))
        );
        assert!(
            out.deliveries
                .iter()
                .any(|delivery| evt(delivery) == Some(wire::LEFT) && delivery.endpoint == "B")
        );
        assert!(out.ended.is_none());
        // the last one leaves: the rings still going end, every leg and the room go
        let mut out = Out::default();
        call.leave(Seat(1), &json!({}), at(1_070_000), &mut out);
        assert_eq!(call.phase, Phase::Ended);
        assert_eq!(out.ended, Some(Outcome::Talked(70)));
        assert!(out.room_over);
        assert_eq!(out.released, ["leg1", "leg2", "leg3"]);
        assert_eq!(paths(&out), [("C", "end")]);
        assert_eq!(call.rostered(), [Seat(0), Seat(1)]);
        call.leave(Seat(1), &json!({}), at(1_071_000), &mut Out::default());
    }

    #[test]
    fn a_lost_leg_takes_its_party_out_and_tells_its_endpoint() {
        let (mut call, _) = started();
        accepted(&mut call, 1, "B", 1_005_000);
        let mut out = Out::default();
        call.media_lost(Seat(1), at(1_010_000), &mut out);
        assert_eq!(out.deliveries[0].body["callEnd"]["code"], 408);
        assert_eq!(out.deliveries[0].endpoint, "B");
        assert_eq!(call.party(Seat(1)).status, Status::Left);
        let mut out = Out::default();
        call.media_lost(Seat(2), at(1_011_000), &mut out);
        assert!(out.deliveries.is_empty(), "c isn't in the call");
    }

    #[test]
    fn a_gone_media_node_ends_the_call_for_everyone_and_drops_nothing_there() {
        let (mut call, _) = started();
        accepted(&mut call, 1, "B", 1_005_000);
        assert_eq!(call.seat_of_leg("leg1"), Some(Seat(1)));
        assert_eq!(call.seat_of_leg("nobody's"), None);
        let mut out = Out::default();
        assert!(call.media_gone(at(1_020_000), &mut out));
        assert_eq!(call.phase, Phase::Ended);
        assert!(out.ended.is_some());
        assert!(out.released.is_empty() && !out.room_over);
        let ends: Vec<i64> = out
            .deliveries
            .iter()
            .filter_map(|delivery| delivery.body.pointer("/callEnd/code")?.as_i64())
            .collect();
        assert!(ends.contains(&503) && ends.contains(&487), "{ends:?}");
        assert!(!call.media_gone(at(1_021_000), &mut Out::default()));
    }

    #[test]
    fn a_join_and_an_add_reserve_a_seat_first_and_take_it_with_the_leg_they_made() {
        let (mut call, _) = started();
        // e joins: a new seat, then in the call with the node's answer at its own acceptance
        let reserved = call.reserve(&party("e"), "leg-e").unwrap();
        let seat = reserved.seat;
        assert_eq!((seat, reserved.sources.audio), (Seat(4), 1080));
        assert_eq!(legs(&call, seat), ["leg-e"]);
        assert_eq!(
            call.join(
                seat,
                ("other", "E", Links::new()),
                &answer(),
                at(1_002_000),
                &mut Out::default()
            ),
            Err(Refusal::Gone),
            "a leg it didn't reserve"
        );
        let mut out = Out::default();
        let joined = links("E", &["acceptance", "rosterUpdate"]);
        call.join(seat, ("leg-e", "E", joined), &answer(), at(1_002_000), &mut out)
            .unwrap();
        assert_eq!(paths(&out)[0], ("E", "acceptance"));
        assert_eq!(
            out.deliveries[0].body["callAcceptance"]["mediaContent"]["mediaLegId"],
            "leg-e"
        );
        assert_eq!(call.reserve(&party("e"), "again").unwrap_err(), Refusal::Forbidden);
    }

    #[test]
    fn a_ringing_callee_joins_instead_of_answering_with_a_new_leg() {
        let (mut call, _) = started();
        // rung, c is named by its invitation, which names no endpoint of its
        call.party_mut(Seat(2)).participant["endpointId"] = json!(wire::ZERO_ENDPOINT);
        // a ringing c joins instead of answering: its new leg replaces the one it rang with
        let reserved = call.reserve(&party("c"), "leg-c2").unwrap();
        let seat = reserved.seat;
        assert_eq!(seat, Seat(2));
        assert_eq!(legs(&call, seat), ["leg-c2", "leg2"]);
        let mut out = Out::default();
        call.join(
            seat,
            ("leg-c2", "C9", links("C9", &["acceptance"])),
            &answer(),
            at(1_003_000),
            &mut out,
        )
        .unwrap();
        assert_eq!(paths(&out)[0], ("C", "end"));
        assert_eq!(
            (legs(&call, seat), &out.released[..]),
            (vec!["leg-c2"], &["leg2".to_owned()][..])
        );
        assert_eq!(call.party(seat).participant["endpointId"], "ep-c", "named as it joined");
        let named = json!({"participants": {"from": {"endpointId": "ep-c"}}});
        assert_eq!(
            call.join(seat, ("leg-c2", "C9", Links::new()), &answer(), at(1_003_000), &mut out),
            Err(Refusal::Forbidden)
        );
        call.leave(seat, &named, at(1_004_000), &mut Out::default());
        assert_eq!(call.party(seat).status, Status::Left, "its own leave takes it out");
    }

    #[test]
    fn an_added_member_rings_with_the_leg_reserved_for_it() {
        let (mut call, _) = started();
        let seat = call.reserve(&party("e"), "leg-e").unwrap().seat;
        call.join(
            seat,
            ("leg-e", "E", Links::new()),
            &answer(),
            at(1_002_000),
            &mut Out::default(),
        )
        .unwrap();
        // f is added: it rings with the leg reserved for it
        let seat = call.reserve(&party("f"), "leg-f").unwrap().seat;
        let mut out = Out::default();
        assert_eq!(call.ring(seat, "other", 1_050, &mut out), Err(Refusal::Gone));
        call.ring(seat, "leg-f", 1_050, &mut out).unwrap();
        assert_eq!(call.party(seat).status, Status::Ringing);
        assert_eq!(call.ring(Seat(4), "leg-e", 1_050, &mut out), Err(Refusal::Forbidden));
        // f rings: a second add leaves its ring as it is
        assert_eq!(call.reserve_added(&party("f"), "leg-f2"), Ok(None));
        assert_eq!(call.party(seat).leg.as_ref().map(|leg| leg.id.as_str()), Some("leg-f"));
        // a reservation the node didn't make a leg for is let go, and the seat made for it goes
        let reserved = call.reserve_added(&party("g"), "leg-g").unwrap().unwrap();
        assert_eq!(undo(&mut call, &reserved, "other"), ["other"]);
        assert_eq!(call.parties.len(), 7);
        assert_eq!(undo(&mut call, &reserved, "leg-g"), ["leg-g"]);
        assert_eq!(call.parties.len(), 6);
        let mut over = call.clone();
        over.phase = Phase::Ended;
        assert_eq!(over.reserve(&party("h"), "leg-h").unwrap_err(), Refusal::Gone);
        assert_eq!(over.reserve_added(&party("f"), "leg-h").unwrap_err(), Refusal::Gone);
        assert_eq!(over.ring(Seat(5), "leg-f", 1_050, &mut out), Err(Refusal::Gone));
        assert_eq!(
            over.watch("h", "H", Links::new(), at(1_004_000), &mut Out::default()),
            Err(Refusal::Gone)
        );
    }

    #[test]
    fn a_failed_join_of_a_ringing_party_gives_its_ring_back_its_leg_and_name() {
        let (mut call, _) = started();
        let mut joining = party("c");
        joining.participant["endpointId"] = json!("ep-c9");
        let reserved = call.reserve(&joining, "leg-c2").unwrap();
        assert_eq!(call.party(Seat(2)).participant["endpointId"], "ep-c9");
        assert_eq!(undo(&mut call, &reserved, "leg-c2"), ["leg-c2"]);
        let party = call.party(Seat(2));
        assert_eq!(party.leg.as_ref().map(|leg| leg.id.as_str()), Some("leg2"));
        assert_eq!(party.participant["endpointId"], "ep-c");
        assert_eq!(party.status, Status::Ringing);
        // its ring is still answered on that leg
        accepted(&mut call, 2, "C", 1_003_000);
        assert_eq!(call.party(Seat(2)).status, Status::InCall);
    }

    #[test]
    fn overlapping_failed_reservations_give_the_ring_back_the_leg_it_rang_with() {
        let (mut call, _) = started();
        let first = call.reserve(&party("c"), "leg-c2").unwrap();
        let second = call.reserve(&party("c"), "leg-c3").unwrap();
        assert_eq!(legs(&call, Seat(2)), ["leg-c3", "leg-c2", "leg2"]);
        // the first fails while the second holds the seat: the second is over the leg the first was over
        assert_eq!(undo(&mut call, &first, "leg-c2"), ["leg-c2"]);
        assert_eq!(legs(&call, Seat(2)), ["leg-c3", "leg2"]);
        assert_eq!(undo(&mut call, &second, "leg-c3"), ["leg-c3"]);
        assert_eq!(legs(&call, Seat(2)), ["leg2"]);
        accepted(&mut call, 2, "C", 1_003_000);
        assert_eq!(call.party(Seat(2)).status, Status::InCall);
        // or the second fails first: the first still gives the ring its leg back
        let first = call.reserve(&party("d"), "leg-d2").unwrap();
        let second = call.reserve(&party("d"), "leg-d3").unwrap();
        assert_eq!(undo(&mut call, &second, "leg-d3"), ["leg-d3"]);
        assert_eq!(undo(&mut call, &first, "leg-d2"), ["leg-d2"]);
        assert_eq!(legs(&call, Seat(3)), ["leg3"]);
        assert_eq!(call.party(Seat(3)).status, Status::Ringing);
        // three at once: the first to fail may be deep under the others
        let first = call.reserve(&party("d"), "leg-d4").unwrap();
        call.reserve(&party("d"), "leg-d5").unwrap();
        call.reserve(&party("d"), "leg-d6").unwrap();
        assert_eq!(undo(&mut call, &first, "leg-d4"), ["leg-d4"]);
        assert_eq!(legs(&call, Seat(3)), ["leg-d6", "leg-d5", "leg3"]);
    }

    #[test]
    fn a_ring_that_runs_out_during_a_reservation_lets_every_leg_go_and_the_late_one_too() {
        let (mut call, _) = started();
        let reserved = call.reserve(&party("c"), "leg-c2").unwrap();
        let mut out = Out::default();
        assert!(call.group_lapse(at(1_030_000), &mut out));
        assert!(out.released.contains(&"leg-c2".to_owned()) && out.released.contains(&"leg2".to_owned()));
        // the join's leg, made after its ring ran out, goes once the join fails
        assert_eq!(undo(&mut call, &reserved, "leg-c2"), ["leg-c2"]);
        assert_eq!(call.party(Seat(2)).status, Status::Missed);
    }

    #[test]
    fn a_reservation_a_later_step_took_over_isnt_undone() {
        let (mut call, _) = started();
        // a seat made for someone that isn't the last one stays, without a leg, in nobody's roster
        let e = call.reserve(&party("e"), "leg-e").unwrap();
        call.reserve(&party("f"), "leg-f").unwrap();
        assert_eq!(undo(&mut call, &e, "leg-e"), ["leg-e"]);
        assert_eq!(call.parties.len(), 6);
        assert!(call.party(e.seat).leg.is_none());
        assert!(!call.rostered().contains(&e.seat));
        // a party that got in on the reserved leg keeps it
        let g = call.reserve(&party("g"), "leg-g").unwrap();
        call.join(
            g.seat,
            ("leg-g", "G", Links::new()),
            &answer(),
            at(1_004_000),
            &mut Out::default(),
        )
        .unwrap();
        assert!(undo(&mut call, &g, "leg-g").is_empty());
        assert_eq!(call.party(g.seat).status, Status::InCall);
    }

    #[test]
    fn an_answer_for_a_leg_a_join_reserved_the_seat_over_is_refused_and_the_join_takes_the_seat() {
        let (mut call, _) = started();
        let acceptance = json!({"callAcceptance": {}});
        let reserved = call.reserve(&party("b"), "leg-b2").unwrap();
        assert_eq!(
            call.group_accept(
                Seat(1),
                ("leg1", "B"),
                (Links::new(), &acceptance),
                at(1_002_000),
                &mut Out::default()
            ),
            Err(Refusal::Gone)
        );
        assert_eq!(call.party(Seat(1)).status, Status::Ringing);
        let mut out = Out::default();
        call.join(
            reserved.seat,
            ("leg-b2", "B2", Links::new()),
            &answer(),
            at(1_003_000),
            &mut out,
        )
        .unwrap();
        assert_eq!(call.party(Seat(1)).status, Status::InCall);
        assert_eq!(out.released, ["leg1"]);
    }

    #[test]
    fn an_answer_while_a_join_has_the_seat_reserved_is_refused_on_either_leg() {
        let (mut call, _) = started();
        let acceptance = json!({"callAcceptance": {}});
        call.reserve(&party("b"), "leg-b2").unwrap();
        assert_eq!(call.answerable(Seat(1)), Err(Refusal::Gone));
        for leg in ["leg1", "leg-b2"] {
            let mut out = Out::default();
            let accepted = call.group_accept(
                Seat(1),
                (leg, "B"),
                (Links::new(), &acceptance),
                at(1_002_000),
                &mut out,
            );
            assert_eq!(accepted, Err(Refusal::Gone), "on {leg}");
            assert!(out.released.is_empty(), "on {leg}: {:?}", out.released);
        }
        assert_eq!(call.party(Seat(1)).status, Status::Ringing);
    }

    #[test]
    fn the_caller_who_left_rings_again_when_added_back() {
        let (mut call, _) = started();
        accepted(&mut call, 1, "B", 1_002_000);
        call.group_leave(Seat::CALLER, &json!({}), at(1_003_000), &mut Out::default());
        assert_eq!(call.ringing(Seat::CALLER), Err(Refusal::Gone));
        let reserved = call.reserve_added(&party("a"), "leg-a2").unwrap().unwrap();
        assert_eq!(reserved.seat, Seat::CALLER);
        call.ring(Seat::CALLER, "leg-a2", 1_050, &mut Out::default()).unwrap();
        assert_eq!(call.ringing(Seat::CALLER), Ok(()));
        let mut out = Out::default();
        call.attach(Seat::CALLER, "A2", links("A2", &["end"]), at(1_004_000))
            .unwrap();
        let acceptance = json!({"callAcceptance": {}});
        call.group_accept(
            Seat::CALLER,
            ("leg-a2", "A2"),
            (links("A2", &["rosterUpdate"]), &acceptance),
            at(1_005_000),
            &mut out,
        )
        .unwrap();
        assert_eq!(call.party(Seat::CALLER).status, Status::InCall);
    }

    #[test]
    fn each_ring_of_a_party_has_links_of_its_own() {
        let (mut call, _) = started();
        let attach = |call: &Call| {
            let offer = json!({"blob": "offer", "contentType": "application/sdp-ngc-1.0"});
            let invitation = wire::group_invitation(ORIGIN, call, (Seat::CALLER, Seat(2)), ("t", "own"), &offer);
            let gp = base64::engine::general_purpose::STANDARD
                .decode(invitation["gp"].as_str().unwrap())
                .unwrap();
            let gp: Value = serde_json::from_slice(&gp).unwrap();
            gp["callNotification"]["links"]["attach"].as_str().unwrap().to_owned()
        };
        assert!(attach(&call).ends_with("/forked/call/2/i1/1/attach"));
        call.reject(Seat(2), &json!({}), at(1_005_000), &mut Out::default())
            .unwrap();
        let reserved = call.reserve_added(&party("c"), "leg-c2").unwrap().unwrap();
        call.ring(reserved.seat, "leg-c2", 1_050, &mut Out::default()).unwrap();
        assert!(attach(&call).ends_with("/forked/call/2/i1/2/attach"));
    }

    #[test]
    fn an_unacknowledged_renegotiation_answer_holds_the_next_one_only_for_a_while() {
        let (mut call, _) = started();
        accepted(&mut call, 1, "B", 1_005_000);
        let offered = links("B", &["mediaAnswer", "rejection"]);
        call.group_renegotiated(
            Seat(1),
            ("B", offered.clone()),
            &answer(),
            at(1_006_000),
            &mut Out::default(),
        )
        .unwrap();
        assert!(call.waits_for_acknowledgement(Seat(1), 1_006_000 + NEGOTIATION_WAIT_MS - 1));
        assert!(!call.waits_for_acknowledgement(Seat(1), 1_006_000 + NEGOTIATION_WAIT_MS));
        // its acknowledgement was lost: the next is answered once the wait is over
        let mut out = Out::default();
        let later = at(1_006_000 + NEGOTIATION_WAIT_MS);
        call.group_renegotiated(Seat(1), ("B", offered), &answer(), later, &mut out)
            .unwrap();
        assert_eq!(paths(&out), [("B", "mediaAnswer")]);
        assert_eq!(call.party(Seat(1)).answered.map(|answered| answered.number), Some(2));
    }

    #[test]
    fn a_watcher_is_told_the_whole_roster_and_its_changes() {
        let (mut call, _) = started();
        let mut out = Out::default();
        call.watch("w", "W", links("W", &["rosterUpdate"]), at(1_002_000), &mut out)
            .unwrap();
        assert_eq!(paths(&out), [("W", "rosterUpdate")]);
        assert!(out.deliveries[0].body["participants"].get("8:orgid:a").is_some());
        call.watch(
            "w",
            "W",
            links("W", &["rosterUpdate"]),
            at(1_002_000),
            &mut Out::default(),
        )
        .unwrap();
        assert_eq!(call.watchers.len(), 1, "a watcher's endpoint is kept once");
        let out = accepted(&mut call, 2, "C", 1_003_000);
        assert!(paths(&out).contains(&("W", "rosterUpdate")));
    }

    #[test]
    fn a_renegotiation_is_answered_to_its_party_alone_and_one_at_a_time() {
        let (mut call, _) = started();
        accepted(&mut call, 1, "B", 1_005_000);
        let offered = links("B", &["mediaAnswer", "rejection"]);
        assert_eq!(
            call.group_renegotiated(
                Seat(2),
                ("C", offered.clone()),
                &answer(),
                at(1_006_000),
                &mut Out::default()
            ),
            Err(Refusal::Forbidden),
            "c isn't in the call"
        );
        assert_eq!(
            call.group_renegotiated(
                Seat(1),
                ("B", Links::new()),
                &answer(),
                at(1_006_000),
                &mut Out::default()
            ),
            Err(Refusal::Bad)
        );
        let mut out = Out::default();
        call.group_renegotiated(Seat(1), ("B", offered.clone()), &answer(), at(1_006_000), &mut out)
            .unwrap();
        assert_eq!(paths(&out), [("B", "mediaAnswer")]);
        let told = &out.deliveries[0].body["mediaAnswer"];
        assert_eq!(told["mediaContent"]["fromMixer"], true);
        assert!(
            told["links"]["mediaAcknowledgement"]
                .as_str()
                .unwrap()
                .contains("/mediaAnswers/call/1/t/1/")
        );
        // the next waits for the acknowledgement
        let mut out = Out::default();
        call.group_renegotiated(Seat(1), ("B", offered.clone()), &answer(), at(1_007_000), &mut out)
            .unwrap();
        assert_eq!(paths(&out), [("B", "rejection")]);
        assert_eq!(out.deliveries[0].body["mediaNegotiationFailure"]["code"], 491);
        call.acknowledge_answer(Seat(1), 2, at(1_008_000), &mut Out::default())
            .unwrap();
        assert_eq!(
            call.party(Seat(1)).answered.map(|answered| answered.number),
            Some(1),
            "another negotiation's acknowledgement"
        );
        call.acknowledge_answer(Seat(1), 1, at(1_008_000), &mut Out::default())
            .unwrap();
        assert!(call.party(Seat(1)).answered.is_none());
        // what 1:1 calls do with their media and holds isn't a group call's
        let body = json!({"mediaNegotiation": {"mediaContent": {"blob": "x", "contentType": "y"}}});
        assert_eq!(
            call.renegotiate(Seat(1), "B", (offered, &body), at(1_009_000), &mut Out::default()),
            Err(Refusal::Forbidden)
        );
        assert_eq!(
            call.hold(Seat(1), "B", Links::new(), at(1_009_000), &mut Out::default())
                .unwrap_err(),
            Refusal::Forbidden
        );
        assert_eq!(
            call.resume(Seat(1), "B", Links::new(), at(1_009_000), &mut Out::default()),
            Err(Refusal::Forbidden)
        );
    }

    #[test]
    fn a_keep_alive_and_a_mute_of_a_party_in_the_call() {
        let (mut call, _) = started();
        assert_eq!(
            call.keep_alive(Seat(1), at(1_002_000)),
            Err(Refusal::Forbidden),
            "b still rings"
        );
        call.keep_alive(Seat::CALLER, at(2_000_000)).unwrap();
        assert_eq!(call.alive_due, 2_000 + KEEP_ALIVE + KEEP_ALIVE_SLACK);
        assert_eq!(call.due_at, 1_030);
        let mut out = Out::default();
        let muted = json!({"endpointState": {"state": {"isMuted": true}, "endpointStateSequenceNumber": 2}});
        call.endpoint_state(Seat::CALLER, &muted, at(2_001_000), &mut out)
            .unwrap();
        assert_eq!(
            paths(&out),
            [
                ("A", "rosterUpdate"),
                ("B", "rosterUpdate"),
                ("B2", "rosterUpdate"),
                ("C", "rosterUpdate")
            ]
        );
    }

    #[test]
    fn a_group_calls_invitation_and_presence_read_as_the_client_and_the_captures_have_them() {
        let (call, _) = started();
        let offer = json!({"blob": "offer to b", "contentType": "application/sdp-ngc-1.0"});
        let invitation = wire::group_invitation(ORIGIN, &call, (Seat::CALLER, Seat(1)), ("tenant", "own"), &offer);
        assert_eq!(invitation["evt"], 109);
        let gp = base64::engine::general_purpose::STANDARD
            .decode(invitation["gp"].as_str().unwrap())
            .unwrap();
        let gp: Value = serde_json::from_slice(&gp).unwrap();
        let notification = &gp["callNotification"];
        assert_eq!(notification["mediaContent"]["blob"], "offer to b");
        assert_eq!(notification["mediaContent"]["mediaLegId"], "leg1");
        assert_eq!(notification["conversationType"], "groupCall");
        assert!(
            notification["links"]["attach"]
                .as_str()
                .unwrap()
                .contains("/forked/call/1/i1/1/attach")
        );
        assert_eq!(gp["groupChat"]["threadId"], "19:group@thread.v2");
        assert_eq!(gp["conversationInvitation"]["groupCallInitiator"], "8:orgid:a");
        assert_eq!(gp["conversationInvitation"]["isMultiParty"], true);
        assert!(notification.get("invitationData").is_none(), "no meeting's");
        // a meeting's (its host's Request to join) names the meeting's organizer and tenant, as Teams' does
        let mut call = call;
        call.meeting = Some(crate::calling::state::Meeting {
            organizer: "organizer-id".to_owned(),
            ..Default::default()
        });
        let invitation = wire::group_invitation(ORIGIN, &call, (Seat::CALLER, Seat(1)), ("tenant", "own"), &offer);
        let gp = base64::engine::general_purpose::STANDARD
            .decode(invitation["gp"].as_str().unwrap())
            .unwrap();
        let gp: Value = serde_json::from_slice(&gp).unwrap();
        assert_eq!(
            gp["callNotification"]["invitationData"],
            json!({"meetingInfo": {"organizerId": "organizer-id", "tenantId": "tenant"}})
        );
        assert_eq!(gp["callNotification"]["conversationType"], "scheduledMeeting");
    }

    #[test]
    fn a_group_calls_presence_and_conversation_read_as_the_captures_have_them() {
        let (call, _) = started();
        let presence = wire::presence(ORIGIN, &call, wire::JOINED);
        assert_eq!(presence["evt"], 115);
        let packed = base64::engine::general_purpose::STANDARD
            .decode(presence["cp"].as_str().unwrap())
            .unwrap();
        let mut text = String::new();
        std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(packed.as_slice()), &mut text).unwrap();
        let content: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(content["participants"][0]["id"], "8:orgid:a");
        let conversation = wire::conversation(ORIGIN, "own", &call);
        assert_eq!(conversation["state"]["isMultiParty"], true);
        assert!(
            conversation["links"]["addParticipantAndModality"]
                .as_str()
                .unwrap()
                .ends_with("/add")
        );
        assert!(conversation["roster"]["participants"].get("8:orgid:a").is_some());
        let refused = wire::running_elsewhere(ORIGIN, &call);
        assert_eq!(refused["conversationEnd"]["subCode"], 5704);
        assert_eq!(wire::add_failure("8:orgid:x")["participants"]["8:orgid:x"]["code"], 580);
    }

    #[test]
    fn a_camera_or_a_screen_described_is_told_in_the_roster_once() {
        let (mut call, _) = started();
        accepted(&mut call, 1, "B", 1_005_000);
        let camera = Showing {
            camera: true,
            sharing: false,
        };
        let mut out = Out::default();
        call.group_described(Seat(1), camera, at(1_006_000), &mut out).unwrap();
        assert_eq!(
            paths(&out),
            [("A", "rosterUpdate"), ("B", "rosterUpdate"), ("C", "rosterUpdate")]
        );
        let streams = &out.deliveries[0].body["participants"]["8:orgid:b"]["endpoints"]["ep-B"]["call"]["mediaStreams"];
        assert_eq!(streams[1]["direction"], "sendrecv");
        assert_eq!(streams.as_array().unwrap().len(), 2);
        // the same again tells nothing
        let mut again = Out::default();
        call.group_described(Seat(1), camera, at(1_006_500), &mut again)
            .unwrap();
        assert!(again.deliveries.is_empty());
        let both = Showing {
            camera: true,
            sharing: true,
        };
        let mut out = Out::default();
        call.group_described(Seat(1), both, at(1_007_000), &mut out).unwrap();
        let streams = &out.deliveries[0].body["participants"]["8:orgid:b"]["endpoints"]["ep-B"]["call"]["mediaStreams"];
        assert_eq!(streams[2]["label"], "applicationsharing-video");
        assert_eq!(streams[2]["sourceId"], 1022);
        assert_eq!(streams[2]["direction"], "sendonly");
        // a party not in the call describes nothing; one that leaves shows nothing more
        assert_eq!(
            call.group_described(Seat(2), both, at(1_007_000), &mut Out::default()),
            Err(Refusal::Forbidden)
        );
        call.group_leave(Seat(1), &json!({}), at(1_008_000), &mut Out::default());
        assert_eq!(call.party(Seat(1)).showing, Showing::default());
    }

    /// The roster's sharing stream of `party` (its first endpoint's), if it lists one.
    fn shares(call: &Call, seat: usize) -> bool {
        let party = call.party(Seat(seat));
        let streams = &wire::roster(call, &[Seat(seat)])["participants"][party.mri()]["endpoints"];
        streams
            .as_object()
            .into_iter()
            .flat_map(|endpoints| endpoints.values())
            .filter_map(|endpoint| endpoint["call"]["mediaStreams"].as_array())
            .flatten()
            .any(|stream| stream["label"] == "applicationsharing-video")
    }

    #[test]
    fn a_new_share_takes_over_the_one_going_and_the_old_sharer_s_next_description_takes_nothing_back() {
        let (mut call, _) = started();
        accepted(&mut call, 1, "B", 1_005_000);
        accepted(&mut call, 2, "C", 1_005_000);
        let sharing = |camera| Showing { camera, sharing: true };
        call.group_described(Seat(1), sharing(false), at(1_006_000), &mut Out::default())
            .unwrap();
        assert!(shares(&call, 1));
        // c shares while b does: the roster lists c's share alone, told of both
        let mut out = Out::default();
        call.group_described(Seat(2), sharing(false), at(1_007_000), &mut out)
            .unwrap();
        assert!(shares(&call, 2) && !shares(&call, 1));
        let told = &out.deliveries[0].body["participants"];
        assert!(
            told.get("8:orgid:b").is_some() && told.get("8:orgid:c").is_some(),
            "{told}"
        );
        // b, which didn't hear it yet, turns its camera on with its sharing line still described: c keeps the share
        call.group_described(Seat(1), sharing(true), at(1_007_500), &mut Out::default())
            .unwrap();
        assert!(shares(&call, 2) && !shares(&call, 1));
        // b stops, then shares again: its new share takes over c's
        call.group_described(Seat(1), Showing::default(), at(1_008_000), &mut Out::default())
            .unwrap();
        call.group_described(Seat(1), sharing(false), at(1_009_000), &mut Out::default())
            .unwrap();
        assert!(shares(&call, 1) && !shares(&call, 2));
        // the sharer leaves: nobody's share is listed
        call.group_leave(Seat(1), &json!({}), at(1_010_000), &mut Out::default());
        assert!(!shares(&call, 2));
    }
}
