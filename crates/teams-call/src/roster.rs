//! Who is in a call, as its roster updates tell it (`docs/protocol/calling.md`, "The roster"): each `rosterUpdate`
//! delivered at the conversation's callback lists the participants that changed, each with a version that counts up;
//! the call's participants are the ones whose latest entry is active and in the call, in the order they were first
//! seen. The account's own entry is left out, and so is a service's (a nameless app that only listens). A meeting's
//! roster also lists who waits in its lobby (an endpoint with a `lobby` rather than a `call`): told apart, the
//! account's own wait too. Nothing is sent for it: the roster is read only to show who is in the call, whose video goes
//! where, and who speaks (independent of call admission).

use teams_proto::Mri;

use crate::read::RosterEntry;

/// One participant of a call, the account aside.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is its own roster field (video, mute, hand, meeting role), told and shown apart"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Participant {
    pub mri: Mri,
    /// As the roster gives it.
    pub name: Option<String>,
    /// The source id of its main audio: who a group call's mixer says spoke.
    pub audio_source: Option<i64>,
    /// The source id of its main video: what a source request names to receive it.
    pub video_source: Option<i64>,
    /// Whether it sends video now (its main video line `sendrecv` or `sendonly`).
    pub video_on: bool,
    /// The source id of its screen while it shares it in a group call: what a source request names to show it.
    pub sharing_source: Option<i64>,
    /// Whether its microphone is muted, as its endpoint reports it.
    pub muted: bool,
    /// Whether its hand is raised.
    pub hand_raised: bool,
    /// Whether it organizes the meeting (its roster entry's `meetingRole` `organizer`).
    pub organizer: bool,
    /// When its endpoint in the call joined (`endpointJoinTime`, RFC 3339, as the roster writes it).
    pub joined: Option<String>,
}

/// The account's own entry in the call's roster, as last told: its raised hand's id while it is up, and whether it
/// organizes the meeting.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Own {
    pub(crate) hand: Option<String>,
    pub(crate) organizer: bool,
}

/// A call's roster as delivered so far.
#[derive(Clone, Debug, Default)]
pub(crate) struct Roster {
    /// Each participant's latest entry, in the order first seen.
    entries: Vec<RosterEntry>,
    /// The participants last told.
    told: Vec<Participant>,
    /// Who was last told waiting in a meeting's lobby, the account aside.
    told_waiting: Vec<Participant>,
    /// Whether the account itself was last told waiting in the lobby.
    told_own_wait: bool,
    /// The account's own entry as last told.
    told_own: Own,
}

/// What a roster update changed of a meeting's lobby, as [`Roster::lobby`] tells it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LobbyChange {
    /// Who waits in the lobby now, the account aside, when that changed.
    pub(crate) waiting: Option<Vec<Participant>>,
    /// Whether the account itself waits there now, when that changed.
    pub(crate) own: Option<bool>,
}

impl Roster {
    /// The participants as last told.
    pub(crate) fn participants(&self) -> &[Participant] {
        &self.told
    }

    /// The account's own entry as the entries kept say now, when it changed since last told; `me` is the account.
    pub(crate) fn own_changed(&mut self, me: &Mri) -> Option<Own> {
        let own = self.own(me);
        if own == self.told_own {
            return None;
        }
        self.told_own = own.clone();
        Some(own)
    }

    /// The account's own entry as the entries kept say now; `me` is the account.
    pub(crate) fn own(&self, me: &Mri) -> Own {
        self.entries
            .iter()
            .find(|entry| is_me(&entry.mri, me) && entry.active)
            .map(|entry| Own {
                hand: entry.hand.clone(),
                organizer: entry.organizer,
            })
            .unwrap_or_default()
    }

    /// The source id of the account's own main video, as the entries kept say now; `me` is the account.
    pub(crate) fn own_video_source(&self, me: &Mri) -> Option<i64> {
        self.entries
            .iter()
            .find(|entry| is_me(&entry.mri, me) && entry.active)
            .and_then(|entry| entry.video_source)
    }

    /// Who was last told waiting in the lobby, the account aside.
    pub(crate) fn waiting(&self) -> &[Participant] {
        &self.told_waiting
    }

    /// This roster for another call: its entries kept, nobody told yet (the next [`Roster::apply`] tells them all).
    pub(crate) fn seeded(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            ..Self::default()
        }
    }

    /// The lobby as the entries kept say now (see [`LobbyChange`]); `me` is the account.
    pub(crate) fn lobby(&mut self, me: &Mri) -> LobbyChange {
        let waiting: Vec<Participant> = self
            .entries
            .iter()
            .filter(|entry| entry.in_lobby && !entry.service && !is_me(&entry.mri, me))
            .map(participant)
            .collect();
        let own = self.entries.iter().any(|entry| entry.in_lobby && is_me(&entry.mri, me));
        let mut change = LobbyChange::default();
        if waiting != self.told_waiting {
            self.told_waiting.clone_from(&waiting);
            change.waiting = Some(waiting);
        }
        if own != self.told_own_wait {
            self.told_own_wait = own;
            change.own = Some(own);
        }
        change
    }

    /// Takes an update's entries (an entry no newer than the one kept changes nothing); the participants now, when
    /// they changed since last told. `me` is the account, left out.
    pub(crate) fn apply(&mut self, update: Vec<RosterEntry>, me: &Mri) -> Option<Vec<Participant>> {
        for entry in update {
            match self.entries.iter_mut().find(|kept| kept.mri == entry.mri) {
                Some(kept) if kept.version < entry.version => *kept = entry,
                Some(_) => {}
                None => self.entries.push(entry),
            }
        }
        let now: Vec<Participant> = self
            .entries
            .iter()
            .filter(|entry| entry.active && !entry.service && !is_me(&entry.mri, me))
            .map(participant)
            .collect();
        if now == self.told {
            return None;
        }
        self.told.clone_from(&now);
        Some(now)
    }
}

/// Whether `mri` is the account `me`: an MRI's object id is a GUID, which a roster may write in another case.
pub(crate) fn is_me(mri: &Mri, me: &Mri) -> bool {
    mri.as_str().eq_ignore_ascii_case(me.as_str())
}

/// A roster entry as told.
fn participant(entry: &RosterEntry) -> Participant {
    Participant {
        mri: entry.mri.clone(),
        name: entry.name.clone(),
        audio_source: entry.audio_source,
        video_source: entry.video_source,
        video_on: entry.video_on,
        sharing_source: entry.sharing_source,
        muted: entry.muted,
        hand_raised: entry.hand.is_some(),
        organizer: entry.organizer,
        joined: entry.joined.clone(),
    }
}
