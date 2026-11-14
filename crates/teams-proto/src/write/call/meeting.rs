//! A meeting's shared states and settings, as its participants' clients change them while the call runs
//! (`docs/protocol/calling.md`, "More of a running call"): the timer (`updateMeetingStates` with `meetingStates.timer`)
//! and the lock (`updateMeetingSettings` with `meetingSettings.lockMeeting`). Everyone in the call learns either from
//! the conversation's `conversationUpdate` ([`MeetingTimer::read`], [`meeting_locked`]).

use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::{
    Built, CallContext, CallLink, CallParticipant, CallWriteError, operation, operation as operation_write, parts,
};
use crate::CallOperation;

/// Where a meeting's timer is (`state` in its JSON text).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerState {
    /// Counting down.
    InProgress,
    /// Stopped where it was (a reset leaves it paused at its whole time).
    Paused,
    /// Taken away: no timer shows.
    Cancelled,
}

impl TimerState {
    /// The wire's word for it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InProgress => "InProgress",
            Self::Paused => "Paused",
            Self::Cancelled => "Cancelled",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "InProgress" => Some(Self::InProgress),
            "Paused" => Some(Self::Paused),
            "Cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// A meeting's timer (Teams' More → Timer, seen 2026-11-06): its state, how long it has left as of its last edit, and
/// its whole time. Its wire form is the `adhocTimer` of the JSON text in `meetingStates.timer.value.data`, beside an
/// `agendaTimer` and a `rosTimer` the native client keeps as they were told (cancelled when none was).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingTimer {
    pub state: TimerState,
    /// What was left, in milliseconds, when it was last edited.
    pub remaining_ms: u64,
    /// Its whole time, in milliseconds (`sections[0].originalDuration`).
    pub original_ms: u64,
    /// When the service took its last edit (`value.lastEditTime`), in Unix milliseconds: an `InProgress` timer has
    /// counted down from `remaining_ms` since. `None` when the delivery didn't say.
    pub edited_ms: Option<i64>,
    /// The state it had before this edit, as the web client tells it with each change after the first.
    pub previous: Option<TimerState>,
}

/// A meeting timer as the conversation last told it: the timer, the sequence number its next change names, and the
/// JSON text it came in (whose other timers a change sends back as they were).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimerUpdate {
    pub timer: MeetingTimer,
    pub sequence: u64,
    pub data: String,
}

/// The whole time a timer starts with when nobody set another: five minutes, as Teams' editor opens.
pub const DEFAULT_TIMER_MS: u64 = 300_000;

impl MeetingTimer {
    /// The timer a `conversationUpdate`'s `meetingStates.timer` (`{"value": {"data", "lastEditTime"},
    /// "sequenceNumber"}`) tells; `None` unless its `adhocTimer` reads.
    #[must_use]
    pub fn read(timer: &Value) -> Option<TimerUpdate> {
        let data = timer.pointer("/value/data")?.as_str()?;
        let parsed: Value = serde_json::from_str(data).ok()?;
        let adhoc = parsed.get("adhocTimer")?;
        let state = TimerState::parse(adhoc.get("state")?.as_str()?)?;
        let remaining_ms = adhoc.get("remainingDuration")?.as_u64()?;
        let original_ms = adhoc
            .pointer("/sections/0/originalDuration")
            .and_then(Value::as_u64)
            .unwrap_or(remaining_ms);
        let edited_ms = timer
            .pointer("/value/lastEditTime")
            .and_then(Value::as_str)
            .and_then(|at| OffsetDateTime::parse(at, &Rfc3339).ok())
            .and_then(|at| i64::try_from(at.unix_timestamp_nanos() / 1_000_000).ok());
        let previous = adhoc
            .get("previousState")
            .and_then(Value::as_str)
            .and_then(TimerState::parse);
        Some(TimerUpdate {
            timer: Self {
                state,
                remaining_ms,
                original_ms,
                edited_ms,
                previous,
            },
            sequence: timer.get("sequenceNumber").and_then(Value::as_u64).unwrap_or(0),
            data: data.to_owned(),
        })
    }

    /// What it has left at `now_ms` (Unix milliseconds): counted down since its last edit while in progress, as it
    /// stood otherwise.
    #[must_use]
    pub fn left_at(&self, now_ms: i64) -> u64 {
        match (self.state, self.edited_ms) {
            (TimerState::InProgress, Some(edited)) => {
                let gone = u64::try_from(now_ms.saturating_sub(edited)).unwrap_or(0);
                self.remaining_ms.saturating_sub(gone)
            }
            _ => self.remaining_ms,
        }
    }

    /// The JSON text of `meetingStates.timer.value.data` this timer goes as: its `adhocTimer`, and the `agendaTimer`
    /// and `rosTimer` of `told` (the text last told) as they were, or cancelled at the default time when none was, as
    /// the web client's first Start sent them.
    #[must_use]
    pub fn data(&self, told: Option<&str>) -> String {
        let told: Map<String, Value> = told
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        let mut adhoc = json!({
            "state": self.state.as_str(),
            "remainingDuration": self.remaining_ms,
            "sections": [{"title": "", "originalDuration": self.original_ms}],
            "pendingConsent": false,
            "consentRequested": false,
        });
        if let (Some(previous), Some(object)) = (self.previous, adhoc.as_object_mut()) {
            object.insert("previousState".to_owned(), json!(previous.as_str()));
        }
        let idle = |sections: Value| {
            json!({
                "state": TimerState::Cancelled.as_str(),
                "remainingDuration": DEFAULT_TIMER_MS,
                "sections": sections,
                "pendingConsent": false,
                "consentRequested": false,
            })
        };
        let agenda = told.get("agendaTimer").cloned().unwrap_or_else(|| idle(json!([])));
        let ros = told
            .get("rosTimer")
            .cloned()
            .unwrap_or_else(|| idle(json!([{"title": "", "originalDuration": DEFAULT_TIMER_MS}])));
        json!({"adhocTimer": adhoc, "agendaTimer": agenda, "rosTimer": ros}).to_string()
    }
}

/// Whether a `conversationUpdate` (`body`) says the meeting is locked (`meetingDetails.meetingCapability.lockMeeting`);
/// `None` when it doesn't say.
#[must_use]
pub fn meeting_locked(body: &Value) -> Option<bool> {
    body.pointer("/meetingDetails/meetingCapability/lockMeeting")?.as_bool()
}

/// A change of the meeting's shared states: `POST` to the conversation's `updateMeetingStates` link (`{"from",
/// "meetingStates": {"timer": {"sequenceNumber", "value": {"data"}}}, "updateMeetingStatesStatus", "operationId"}`,
/// seen 2026-11-06: the timer's Start, Reset and Play), answered `202`; everyone in the call is told with the
/// conversation's next `conversationUpdate`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateMeetingStates(pub(super) Built);

impl UpdateMeetingStates {
    /// `from` sets the meeting's timer to `timer` at `link`: the change of the timer last told as `sequence` (0 when
    /// none was), its other timers as `told` had them; how it went is told at the callback `status`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't an `updateMeetingStates` in its captured shape.
    pub fn timer(
        context: &CallContext,
        link: &CallLink,
        from: &CallParticipant,
        (timer, sequence, told): (&MeetingTimer, u64, Option<&str>),
        status: &str,
    ) -> Result<Self, CallWriteError> {
        let body = json!({
            "from": from.json(),
            "meetingStates": {"timer": {"sequenceNumber": sequence, "value": {"data": timer.data(told)}}},
            "updateMeetingStatesStatus": status,
            "operationId": parts::guid().map_err(CallWriteError::Random)?,
        });
        Ok(Self(operation(
            context,
            CallOperation::UpdateMeetingStates,
            link,
            Some(body),
        )?))
    }
}

/// A change of the meeting's settings by its organizer: `PUT` to the conversation's `updateMeetingSettings` link
/// (`{"from", "meetingSettings": {"lockMeeting"}, "sequenceNumber"}`, seen 2026-11-06: Lock the meeting and Unlock
/// the meeting), answered `202`; everyone in the call is told with the conversation's next `conversationUpdate`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateMeetingSettings(pub(super) Built);

impl UpdateMeetingSettings {
    /// `from` locks the meeting (`locked`) or unlocks it at `link`, the `sequence`th settings change it made.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't an `updateMeetingSettings` in its captured shape.
    pub fn lock(
        context: &CallContext,
        link: &CallLink,
        from: &CallParticipant,
        locked: bool,
        sequence: u64,
    ) -> Result<Self, CallWriteError> {
        let body = json!({
            "from": from.json(),
            "meetingSettings": {"lockMeeting": locked},
            "sequenceNumber": sequence,
        });
        Ok(Self(operation(
            context,
            CallOperation::UpdateMeetingSettings,
            link,
            Some(body),
        )?))
    }
}

/// Everyone in the call muted but the sender: `POST` to the conversation's `mute` link (`{"from", "scope": "all",
/// "muteParticipants": [], "mediaTypes": ["audio"], "links": {"clientCallback"}, "operationId"}`, seen 2026-11-06:
/// People's Mute all, confirmed), answered `202` `{"muteUnmuteResponse": {}}`. The outcome comes at the
/// `clientCallback` (`muteUnmuteAsync`: each participant muted, with its new `serverMuteVersion`); everyone learns it
/// from the roster, whose entries then have their main audio `serverMuted`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MuteParticipants(pub(super) Built);

impl MuteParticipants {
    /// `from` mutes everyone else at `link`, the outcome told at `callback` (a `…/conversation/muteUnmuteAsync/` of
    /// its own whose segment is `operation`, the request's id, as the web client names it).
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a `mute` in its captured shape: the callback not the
    /// `muteUnmuteAsync` of the operation, the operation id not a GUID.
    pub fn everyone(
        context: &CallContext,
        link: &CallLink,
        from: &CallParticipant,
        callback: &str,
        operation: &str,
    ) -> Result<Self, CallWriteError> {
        let guid = operation.len() == 36 && operation.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
        if !guid || !callback.ends_with(&format!("/{operation}/conversation/muteUnmuteAsync/")) {
            return Err(CallWriteError::Unmapped);
        }
        let body = json!({
            "from": from.json(),
            "scope": "all",
            "muteParticipants": [],
            "mediaTypes": ["audio"],
            "links": {"clientCallback": callback},
            "operationId": operation,
        });
        Ok(Self(operation_write(context, CallOperation::Mute, link, Some(body))?))
    }
}

/// How to join a meeting, as its invitation in the conversation's `meetingDetails.invitation` gives it (an HTML block,
/// form-encoded; seen 2026-11-06, `meeting-steps`): what Teams' Meeting info pane shows. Each part is read where the
/// invitation has it; one it doesn't is left out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MeetingJoinInfo {
    /// The short join link (`https://teams.microsoft.com/meet/<code>?p=<key>`): the anchor
    /// `meet_invite_block.action.join_link`.
    pub join_link: Option<String>,
    /// The meeting id as the invitation groups it (`267 266 268 900 510 1`).
    pub meeting_id: Option<String>,
    pub passcode: Option<String>,
    /// "Need help?": the anchor `meet_invite_block.action.help`.
    pub help_link: Option<String>,
    /// "System reference": the anchor `meet_invite_block.action.join_link_compatibility` (the meeting's full join URL).
    pub reference_link: Option<String>,
}

impl MeetingJoinInfo {
    /// The join info of a `conversationUpdate` (`body`); `None` when it carries no invitation, or one with no join
    /// link.
    #[must_use]
    pub fn read(body: &Value) -> Option<Self> {
        Self::read_in(&crate::Cloud::Microsoft, body)
    }

    /// Reads with the account's service origin.
    #[must_use]
    pub fn read_in(cloud: &crate::Cloud, body: &Value) -> Option<Self> {
        let encoded = body.pointer("/meetingDetails/invitation")?.as_str()?;
        let plus = encoded.replace('+', " ");
        let html = percent_encoding::percent_decode_str(&plus).decode_utf8().ok()?;
        let fragment = scraper::Html::parse_fragment(&html);
        let anchor = |id: &str| {
            let selector = scraper::Selector::parse(&format!("a[id=\"{id}\"]")).ok()?;
            let href = fragment.select(&selector).next()?.value().attr("href")?.trim();
            (if cloud.is_microsoft() {
                href.starts_with("https://")
            } else {
                url::Url::parse(href).is_ok_and(|url| cloud.contains(&url))
            })
            .then(|| href.to_owned())
        };
        let text = fragment
            .root_element()
            .text()
            .flat_map(str::split_whitespace)
            .collect::<Vec<_>>()
            .join(" ");
        let after = |label: &str, until: &[&str]| {
            let start = text.find(label)? + label.len();
            let rest = &text[start..];
            let end = until
                .iter()
                .filter_map(|stop| rest.find(stop))
                .min()
                .unwrap_or(rest.len());
            let value = rest[..end].trim();
            (!value.is_empty()).then(|| value.to_owned())
        };
        let info = Self {
            join_link: anchor("meet_invite_block.action.join_link"),
            meeting_id: after("Meeting ID:", &["Passcode:", "Need help?", "___"]),
            passcode: after("Passcode:", &["Need help?", "___", "Dial in"]),
            help_link: anchor("meet_invite_block.action.help"),
            reference_link: anchor("meet_invite_block.action.join_link_compatibility"),
        };
        info.join_link.is_some().then_some(info)
    }

    /// The join info as text, as Copy join info puts it on the clipboard: what the pane shows, line by line.
    #[must_use]
    pub fn text(&self) -> String {
        let mut lines = vec!["Microsoft Teams meeting".to_owned()];
        if let Some(link) = &self.join_link {
            lines.push(format!("Join: {link}"));
        }
        if let Some(id) = &self.meeting_id {
            lines.push(format!("Meeting ID: {id}"));
        }
        if let Some(passcode) = &self.passcode {
            lines.push(format!("Passcode: {passcode}"));
        }
        lines.join("\n")
    }
}
