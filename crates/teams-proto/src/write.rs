//! Typed write request serialization. Writes are ordinary requests sent without permits.

use serde_json::Value;

use crate::endpoint::{Endpoint, Method, Service};

mod activity;
mod availability;
mod calendar;
mod call;
mod channel;
mod channel_admin;
pub use channel_admin::{ChannelChange, ChannelData, ChannelRead, ChannelTarget, ChannelWrite};
mod chat;
mod meeting_options;
mod picture;
pub use picture::{CreatePicture, MAX_SENT_PICTURE, UploadPicture};
mod team;

pub use activity::{ACTIVITY_FEED, MarkFeedRead, SetActivityRead, SetUserGraphSettings};
pub use availability::{ForceAvailability, ManualAvailability, SetEndpointAvailability};
pub use calendar::{
    CancelChannelEvent, CancelEvent, CreateChannelEvent, CreateEvent, CreateOnlineMeeting, DeleteEvent, EventAttendee,
    EventDraft, EventShowAs, MeetNow, MeetNowMeeting, MeetingChannel, Organizer, RespondEvent, RsvpAnswer,
    UpdateChannelEvent, UpdateEvent, ZonedTime,
};
pub(crate) use call::on_calling_base;
pub use call::{
    AcceptCall, AcceptParts, AcceptTransfer, AcknowledgeAcceptance, AcknowledgeMediaAnswer, AddParts, AddToCall,
    AnswerMedia, AnswerMediaParts, ApplyChannelParameters, AttachCall, AttachParts, CALL_REACTIONS, CALL_START_URL,
    CALLING_BASE, CALLING_HOST, CallContext, CallLink, CallParticipant, CallWriteError, ChannelParameters, ClientInfo,
    DecideLobby, DeclineCall, EndConversation, HoldCall, Invitee, JoinCall, JoinMedia, JoinParts, KeepAliveCall,
    LeaveCall, LeaveReason, Links, MediaContent, MediaDescription, MediaDescriptions, MeetingInfo, PublishState,
    RAISE_HANDS, RemoveState, RenegotiateMedia, ReportRinging, ResumeCall, SendCallReaction, StartCall, StartParts,
    TEAMS_ECHO_BOT_MRI, TEST_CALL_SCENARIO, TransferCall, Transferor, UpdateEndpointMetadata, UpdateEndpointState,
    UpdateMediaDescriptions, UpdateParticipantProperties, VoiceConsent,
};
pub use call::{
    DEFAULT_TIMER_MS, MeetingJoinInfo, MeetingTimer, MuteParticipants, TimerState, TimerUpdate, UpdateMeetingSettings,
    UpdateMeetingStates, meeting_locked,
};
pub use channel::{
    AddChannelMembers, ChannelCreated, ChannelLayout, CreateChannel, NewChannel, NewChannelKind, ShowChannelByDefault,
    channel_error_code,
};
pub use chat::{
    AddChatMembers, Author, CreateChat, DeleteMessage, EditMessage, Original, ReadReceipt, RemoveReaction, SendMessage,
    SetMeetingTitle, SetReaction, SharedHistory, Typing,
};
pub use meeting_options::{MeetingOption, MeetingOptions, MeetingOptionsRead, OptionsOf, SetMeetingOption};
pub use team::{
    AppDefinition, MemberUpdate, NewTeam, TeamCopy, TeamRole, TeamWrite, TemplateTeam, team_created, team_error_code,
};

/// Builds a typed request's wire fields. This is serialization, not authorization.
pub(crate) trait Prepared {
    fn wire(&self) -> Option<Wire>;
    fn call(&self) -> Option<&call::CallWire> {
        None
    }
}

impl<T: Prepared> crate::Request for T {
    fn service(&self) -> Service {
        self.wire().map_or(Service::Calling, |wire| wire.service)
    }
    fn method(&self) -> Method {
        self.call().map_or_else(
            || self.wire().map_or(Method::Post, |wire| wire.method),
            call::CallWire::method,
        )
    }
    fn endpoint(&self) -> Endpoint {
        self.wire()
            .map_or_else(|| Endpoint::new([] as [&str; 0]), |wire| wire.endpoint)
    }
    fn body(&self) -> Option<Value> {
        self.call()
            .map_or_else(|| self.wire().and_then(|wire| wire.body), |wire| wire.body().cloned())
    }
    fn headers(&self) -> Vec<(String, String)> {
        let headers = self.call().map_or_else(
            || self.wire().map_or_else(Vec::new, |wire| wire.headers),
            |wire| wire.headers().to_vec(),
        );
        headers
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect()
    }
    fn raw_body(&self) -> Option<RawBody> {
        self.wire().and_then(|wire| wire.raw)
    }
    fn call_url(&self) -> Option<&url::Url> {
        self.call().map(call::CallWire::url)
    }
    fn routable(&self) -> bool {
        self.call().is_some() || self.wire().is_some()
    }
    fn is_write(&self) -> bool {
        true
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wire {
    service: Service,
    method: Method,
    endpoint: Endpoint,
    /// By lower-cased name, none of them a credential.
    headers: Vec<(&'static str, String)>,
    body: Option<Value>,
    /// A body of bytes sent as they are, in place of `body`: an image. The target never comes from it.
    raw: Option<RawBody>,
}

/// A write's body of bytes (a picture). `Debug` shows only its length.
#[derive(Clone, PartialEq, Eq)]
pub struct RawBody(std::sync::Arc<[u8]>);

impl RawBody {
    /// The bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for RawBody {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes.into())
    }
}

impl std::fmt::Debug for RawBody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "RawBody({} bytes)", self.0.len())
    }
}

impl Wire {
    /// The bytes it sends: its raw body, or its JSON body as text.
    #[cfg(test)]
    pub(crate) fn body_bytes(&self) -> Option<Vec<u8>> {
        self.raw
            .as_ref()
            .map(|raw| raw.bytes().to_vec())
            .or_else(|| self.body.as_ref().map(|body| body.to_string().into_bytes()))
    }

    #[cfg(test)]
    pub(crate) fn service(&self) -> Service {
        self.service
    }

    #[cfg(test)]
    pub(crate) fn headers(&self) -> &[(&'static str, String)] {
        &self.headers
    }

    #[cfg(test)]
    pub(crate) fn method(&self) -> Method {
        self.method
    }

    #[cfg(test)]
    pub(crate) fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    #[cfg(test)]
    pub(crate) fn body(&self) -> Option<&Value> {
        self.body.as_ref()
    }
}
