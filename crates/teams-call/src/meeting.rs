//! What a meeting's call shares with everyone in it while it runs (`docs/protocol/calling.md`, "More of a running
//! call"), as its conversation and its roster tell it: the timer (the conversation's `meetingStates.timer`), the lock
//! (`meetingDetails.meetingCapability.lockMeeting`), how to join it (`meetingDetails.invitation`), and a recording or a
//! transcription someone runs (Microsoft's recording bot in the roster, its endpoint's `processingModes.recording` and
//! `.realTimeTranscript`). The account
//! changes the timer and the lock ([`Calls::set_timer`], [`Calls::lock_meeting`]) and starts or stops a recording
//! through the recording bot's command link ([`Calls::record_command`]). Each change is told as
//! [`CallEvent::Meeting`].
//!
//! Its organizer's Controls (`meetingDetails.meetingCapability`: the lobby, who presents, what attendees may do) and
//! what binds the account: whether it is an attendee (its roster entry's `meetingRole`) and whether the organizer muted
//! it (its main audio `serverMuted`, Mute all's). The organizer mutes everyone else at the conversation's `mute` link
//! ([`Calls::mute_all`]).

use serde_json::Value;
use teams_proto::{
    MeetingJoinInfo, MeetingTimer, Mri, MuteParticipants, TimerUpdate, UpdateMeetingSettings, UpdateMeetingStates,
};

use super::{CallError, CallEvent, CallId, Calls, up};
use crate::agent::Kind;
use crate::call::Step;
use crate::{CallWrite, Delivery};

/// A meeting's shared state as last told ([`CallEvent::Meeting`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MeetingState {
    /// The timer, while one shows (none once cancelled, or before anyone set one).
    pub timer: Option<MeetingTimer>,
    /// Nobody else can join (the organizer locked it).
    pub locked: bool,
    /// How to join it, as its invitation says.
    pub join: Option<MeetingJoinInfo>,
    /// A recording runs.
    pub recording: Option<Recording>,
    /// A transcription runs (with a recording or alone).
    pub transcribing: Option<Transcription>,
    /// The organizer's Controls as the conversation's updates told them (each `None` until told).
    pub options: teams_proto::MeetingOptions,
    /// The account is an attendee (its roster entry's `meetingRole`): the attendees' restrictions bind it, and it may
    /// not present while only the organizer may.
    pub attendee: bool,
    /// The organizer muted the account (Mute all): the version of the endpoint's mutes it came as, while its main
    /// audio stays muted by them. A newer one is a new mute (Teams' "You've been muted").
    pub muted_by_organizer: Option<u64>,
}

impl MeetingState {
    /// What attendees may do binds the account: it is an attendee and not everyone may present.
    #[must_use]
    pub fn restricted(&self) -> bool {
        self.attendee && self.options.present != Some(true)
    }

    /// The account may not unmute itself (an attendee, attendees' unmuting turned off).
    #[must_use]
    pub fn unmute_blocked(&self) -> bool {
        self.restricted() && self.options.unmute == Some(false)
    }

    /// The account may not turn its camera on (an attendee, attendees' cameras turned off).
    #[must_use]
    pub fn camera_blocked(&self) -> bool {
        self.restricted() && self.options.camera == Some(false)
    }

    /// The account may not present (an attendee while only the organizer may).
    #[must_use]
    pub fn share_blocked(&self) -> bool {
        self.restricted()
    }
}

/// A recording someone runs in the call, as the recording bot's roster entry tells it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recording {
    /// Who started it, when the entry names them (`userId`).
    pub by: Option<Mri>,
    /// It records the audio alone (its `properties.recordVideo` is `"False"`).
    pub audio_only: bool,
}

/// A transcription someone runs in the call, as the recording bot's roster entry tells it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transcription {
    /// Who started it, when the entry names them (`userId`).
    pub by: Option<Mri>,
}

/// The meeting-state part of a call the machine keeps: the state told, the timer as last delivered (its sequence and
/// its JSON text, which the next change names and sends back), the settings changes the account made, and the
/// per-MRI recording/caption service roster that supplies both the recording modes and the caption command binding.
#[derive(Clone, Debug, Default)]
pub(crate) struct Meeting {
    pub(crate) state: MeetingState,
    timer: Option<TimerUpdate>,
    settings: u64,
    // Captions and meeting modes read the same merged roster and endpoint selection.
    pub(crate) services: teams_proto::captions::CaptionRoster,
    own_version: Option<u64>,
}

/// What a `conversationUpdate` (or a start's answer) tells of the meeting, each part only where it says it.
fn conversation_update(cloud: &teams_proto::Cloud, body: &[u8]) -> Option<ConversationUpdate> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let timer = body.pointer("/meetingStates/timer").and_then(MeetingTimer::read);
    Some(ConversationUpdate {
        timer,
        locked: teams_proto::meeting_locked(&body),
        join: MeetingJoinInfo::read_in(cloud, &body),
        options: teams_proto::MeetingOptions::of_capability(&body),
    })
}

/// What a `conversationUpdate` tells of the meeting.
struct ConversationUpdate {
    timer: Option<TimerUpdate>,
    locked: Option<bool>,
    join: Option<MeetingJoinInfo>,
    options: Option<teams_proto::MeetingOptions>,
}

/// Whether the bot records and transcribes now, each with who started it (`userId`, which stays once it stopped:
/// its `state` tells whether it runs). Read only the endpoint selected from the merged service roster.
fn recorder(metadata: Option<&Value>) -> (Option<Recording>, Option<Transcription>) {
    let running = |name: &str| {
        metadata?
            .get("processingModes")?
            .get(name)
            .filter(|mode| mode.get("state").and_then(Value::as_str) == Some("Active"))
    };
    let by = |mode: &Value| {
        mode.get("userId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(Mri::new)
    };
    let recording = running("recording").map(|mode| Recording {
        by: by(mode),
        audio_only: mode.pointer("/properties/recordVideo").and_then(Value::as_str) == Some("False"),
    });
    let transcribing = running("realTimeTranscript").map(|mode| Transcription { by: by(mode) });
    (recording, transcribing)
}

impl Meeting {
    /// Takes what a `conversationUpdate` (or a start's answer) `body` tells; whether the state changed.
    pub(crate) fn conversation(&mut self, cloud: &teams_proto::Cloud, body: &[u8]) -> bool {
        let Some(ConversationUpdate {
            timer,
            locked,
            join,
            options,
        }) = conversation_update(cloud, body)
        else {
            return false;
        };
        let before = self.state.clone();
        if let Some(update) = timer {
            let newer = self.timer.as_ref().is_none_or(|told| told.sequence <= update.sequence);
            if newer {
                self.state.timer =
                    (update.timer.state != teams_proto::TimerState::Cancelled).then(|| update.timer.clone());
                self.timer = Some(update);
            }
        }
        if let Some(locked) = locked {
            self.state.locked = locked;
        }
        if join.is_some() {
            self.state.join = join;
        }
        if let Some(options) = options {
            self.state.options = self.state.options.updated(options);
        }
        self.state != before
    }

    /// Takes the account's own entry of a roster update (`entries`, `me` the account): whether it is an attendee and
    /// whether the organizer muted it; whether the state changed.
    pub(crate) fn own(&mut self, entries: &[crate::read::RosterEntry], me: &Mri) -> bool {
        let Some(entry) = entries.iter().find(|entry| &entry.mri == me && entry.active) else {
            return false;
        };
        if self.own_version.is_some_and(|told| told >= entry.version) {
            return false;
        }
        self.own_version = Some(entry.version);
        let before = self.state.clone();
        self.state.attendee = entry.attendee;
        self.state.muted_by_organizer = entry.server_mute;
        self.state != before
    }

    /// Merges a service roster and projects its recording modes; whether the meeting state changed.
    pub(crate) fn roster(&mut self, body: &Value) -> bool {
        self.services.update(body);
        self.recorder_state()
    }

    /// Projects recording modes from the same endpoint that supplies the caption command binding.
    fn recorder_state(&mut self) -> bool {
        let (recording, transcribing) = recorder(self.services.followed().map(|service| service.metadata));
        let changed = self.state.recording != recording || self.state.transcribing != transcribing;
        self.state.recording = recording;
        self.state.transcribing = transcribing;
        changed
    }
}

impl Calls {
    /// The account sets the meeting's timer to `timer` (Start, Pause, Play, Reset, Cancel timer) at the conversation's
    /// `updateMeetingStates` link, as the change of the timer last told, as the web client sends it; everyone in the
    /// call is told with the conversation's next update ([`CallEvent::Meeting`]).
    ///
    /// # Errors
    /// [`CallError::NoSuchCall`] for another call, [`CallError::NotNow`] while the call isn't up or its conversation
    /// named no such link; nothing was sent.
    pub fn set_timer(&mut self, call: CallId, timer: &MeetingTimer) -> Result<(), CallError> {
        let current = up(&mut self.call, call)?;
        let link = current.conversation.meeting_states.clone().ok_or(CallError::NotNow)?;
        let status = current.callback(Kind::Conversation, "updateMeetingStatesStatus")?;
        let told = current.meeting.timer.as_ref();
        let sequence = told.map_or(0, |told| told.sequence);
        let data = told.map(|told| told.data.clone());
        let write = UpdateMeetingStates::timer(
            &current.context,
            &link,
            &current.me,
            (timer, sequence, data.as_deref()),
            &status,
        )?;
        self.out
            .send(current, Step::MeetingStates, CallWrite::MeetingStates(write));
        Ok(())
    }

    /// The account, the meeting's organizer, locks it (`locked`: nobody else can join) or unlocks it at the
    /// conversation's `updateMeetingSettings` link, as Teams' Lock the meeting and Unlock the meeting send it.
    ///
    /// # Errors
    /// [`CallError::NoSuchCall`] for another call, [`CallError::NotNow`] while the call isn't up, the account doesn't
    /// organize it or its conversation named no such link; nothing was sent.
    pub fn lock_meeting(&mut self, call: CallId, locked: bool) -> Result<(), CallError> {
        let me = self.identity.mri.clone();
        let current = up(&mut self.call, call)?;
        if !current.roster.own(&me).organizer {
            return Err(CallError::NotNow);
        }
        let link = current.conversation.meeting_settings.clone().ok_or(CallError::NotNow)?;
        let sequence = current.meeting.settings + 1;
        let write = UpdateMeetingSettings::lock(&current.context, &link, &current.me, locked, sequence)?;
        current.meeting.settings = sequence;
        self.out
            .send(current, Step::MeetingSettings, CallWrite::MeetingSettings(write));
        Ok(())
    }

    /// A `conversationUpdate` at the call's own callback: the meeting's state told when it changed.
    pub(crate) fn conversation_updated(&mut self, delivery: &Delivery) {
        let cloud = self.cloud.clone();
        self.meeting_told(|meeting| meeting.conversation(&cloud, &delivery.body));
    }

    /// A roster update's word on the account itself: the meeting's state told when it changed.
    pub(crate) fn own_roster_told(&mut self, body: &[u8]) {
        let me = self.identity.mri.clone();
        let entries = crate::read::roster(body).unwrap_or_default();
        self.meeting_told(|meeting| meeting.own(&entries, &me));
    }

    /// Why [`Calls::options_of`] names no options for `call`, in a few words (never an id): what the log says of a
    /// Controls change that didn't go.
    #[must_use]
    pub fn options_missing(&self, call: CallId) -> &'static str {
        let Some(current) = self
            .call
            .as_ref()
            .filter(|current| current.id == call && current.phase == super::Phase::Connected)
        else {
            return "the call isn't up";
        };
        if !current.roster.own(&self.identity.mri).organizer {
            "the roster doesn't name the account its organizer"
        } else if self.identity.tenant_id.is_empty() {
            "the account's tenant isn't known yet"
        } else if current.caption_thread.is_none() {
            "the call names no chat"
        } else {
            "its tenant, organizer or chat isn't of the options' form"
        }
    }

    /// Whose meeting options the organizer's Controls change for `call` (the middle tier's
    /// [`teams_proto::MeetingOptionsRead`]): the account's tenant, its object id and the call's chat, while the call is
    /// up in a group chat or a meeting's chat and the account organizes it; `None` otherwise.
    #[must_use]
    pub fn options_of(&self, call: CallId) -> Option<teams_proto::OptionsOf> {
        let current = self
            .call
            .as_ref()
            .filter(|current| current.id == call && current.phase == super::Phase::Connected)?;
        if !current.roster.own(&self.identity.mri).organizer {
            return None;
        }
        let organizer = teams_proto::object_id(self.identity.mri.as_str())?;
        teams_proto::OptionsOf::new(&self.identity.tenant_id, organizer, current.caption_thread.as_deref()?)
    }

    /// The account, the call's organizer, mutes everyone else in it (People's Mute all) at the conversation's `mute`
    /// link, the outcome told at a `muteUnmuteAsync` callback named by the request's operation id, as the web client
    /// sends it; everyone sees them muted as the roster tells it.
    ///
    /// # Errors
    /// [`CallError::NoSuchCall`] for another call, [`CallError::NotNow`] while the call isn't up, the account doesn't
    /// organize it or its conversation named no `mute` link; nothing was sent.
    pub fn mute_all(&mut self, call: CallId) -> Result<(), CallError> {
        let me = self.identity.mri.clone();
        let current = up(&mut self.call, call)?;
        if !current.roster.own(&me).organizer {
            return Err(CallError::NotNow);
        }
        let link = current.conversation.mute.clone().ok_or(CallError::NotNow)?;
        let operation = crate::agent::guid()?;
        let callback = current.callback_in(Kind::Conversation, "muteUnmuteAsync", &operation);
        let write = MuteParticipants::everyone(&current.context, &link, &current.me, &callback, &operation)?;
        self.out.send(current, Step::MuteAll, CallWrite::MuteAll(write));
        Ok(())
    }

    /// A start's or a join's answer, as the conversation it is: what it tells of the meeting.
    pub(crate) fn meeting_answered(&mut self, body: &[u8]) {
        let cloud = self.cloud.clone();
        self.meeting_told(|meeting| meeting.conversation(&cloud, body));
        if let Ok(answer) = serde_json::from_slice::<Value>(body)
            && let Some(roster) = answer.get("roster")
        {
            self.service_roster(roster);
        }
    }

    fn meeting_told(&mut self, apply: impl FnOnce(&mut Meeting) -> bool) {
        let Some(call) = self.call.as_mut() else { return };
        if apply(&mut call.meeting) {
            let (id, state) = (call.id, call.meeting.state.clone());
            self.out.event(CallEvent::Meeting { call: id, state });
        }
        // A code start initially knows no chat. The resolved invitation can name its exact thread, which
        // captions, recording and organizer controls need; keep it unknown until that link is supplied.
        if call.caption_thread.is_none()
            && matches!(call.side, crate::call::Side::Meeting)
            && let Some(chat) = call
                .meeting
                .state
                .join
                .as_ref()
                .and_then(|join| join.reference_link.as_deref())
                .filter(|link| {
                    !self.cloud.is_microsoft() || link.starts_with("https://teams.microsoft.com/l/meetup-join/")
                })
                .and_then(|link| teams_proto::join_url_chat_in(&self.cloud, link))
                .filter(|chat| teams_proto::is_meeting_chat_id(chat))
        {
            call.caption_thread = Some(chat.clone());
            self.out.event(CallEvent::GroupChat { call: call.id, chat });
        }
    }
}
