//! Typed Teams requests and their logged transport. Writes require no permit.

mod call_state;
mod chat_members;
mod chat_service;
pub use call_state::CallState;
pub use chat_members::chat_members;
mod ids;
mod log_shape;
pub mod loki;

mod call_operation;
pub use call_operation::CallOperation;

mod ams;
mod attachment;
mod calendar;
pub mod captions;
mod channel_meeting;
mod channel_own;
mod channel_read;
mod channel_sections;
pub use channel_own::ChannelOwn;
pub use channel_sections::{ChannelSection, ChannelSectionAction, ChannelSectionWrite, ChannelSections};
pub use write::{ChannelChange, ChannelData, ChannelRead, ChannelTarget, ChannelWrite};
pub use write::{CreatePicture, MAX_SENT_PICTURE, UploadPicture};
mod activity_settings;
mod app_picture;
pub use app_picture::{APP_PICTURES_HOST, APP_PICTURES_PATH, AppPicture, STATIC_APP_PICTURES_HOST};
pub mod chat;
mod client;
mod emoticon;
mod endpoint;
mod file_content;
mod frame;
mod giphy;
mod http;
mod meeting;
mod own;
mod places;
mod post_read;
mod read;
pub mod recap;
pub mod recording;
mod recurrence;
mod relay;
mod request;
mod schedule;
mod search;
mod team_apps;
pub use activity_settings::{
    AppNotificationPreference, UserGraphSettings, UserGraphSettingsError, UserGraphSettingsRead,
};
mod team_read;
mod thumbnail;
pub mod transcript;
pub use attachment::{DriveSite, DriveWrite, MAX_ATTACHMENT_SIZE, OwnDrive, UploadedFile, direct_file_grants};
mod urlp;
pub mod workflow;
mod write;

pub use chat_service::chat_service_path;
pub use ids::{
    AFD_HOSTS, CALL_PROXY_HOST, INVITATION_EVENTS, Mri, is_chat_id, is_group_chat_id, is_meeting_chat_id,
    is_team_thread_id, one_to_one_id, one_to_one_pair, reply_chain, reply_chain_id, stored_conversation,
};
pub use log_shape::{log_url, path_shape};

pub use ams::{MessagePicture, PastedFormat, PastedPicture, sent_picture};
pub use calendar::{
    Attendee, CALENDAR_VIEW_PATH, CalendarAnswer, CalendarAnswerError, CalendarEvent, CalendarView, ChannelEventRead,
    ChannelEventRef, EventKind, EventRead, EventTime, FloatingTime, FullEvent, MAX_CALENDAR_WINDOW_MS,
    Response as EventResponse, ShowAs, TimeZoneSettings, join_url_chat, join_url_meeting,
};
pub use channel_meeting::{CANCELLED_MEETING_TYPE, SCHEDULED_MEETING_TYPE, ScheduledMeeting};
pub use channel_read::{ChannelCreationStatus, TeamPeopleSearch, TeamPerson};
pub use client::{Client, Response, Transport};
pub use emoticon::{MAX_ANIMATION, ReactionAnimation};
pub use endpoint::{Endpoint, Method, Service};
pub use file_content::{
    FileContent, FileContentLink, FileDownload, FileRange, MAX_RANGE, MAX_STREAM, StreamStart, is_content_link,
};
pub use frame::{
    AcceptanceAcknowledgement, Authenticate, ConnectParams, DeliveryResponse, Disconnect, DroppedIndicator, FrameError,
    FrameSink, FrameText, FrameWriter, InvitationReceipt, MessageLossAck, OwnFrame, Ping, RespondError,
    ResponseDelivery, Sent,
};
pub use giphy::{GiphyPicture, InlinePicture};
pub use http::{
    AMS_HOST_SUFFIX, BuildError, CONNECT_TIMEOUT, CSA_AUDIENCE, Credentials, DEFAULT_RETRY_WAIT, EMOTICONS_HOST,
    Failure, FileBody, GIPHY_HOST, GRAPH_AUDIENCE, GRAPH_HOST, HttpError, HttpTransport, IC3_AUDIENCE, MAX_BODY,
    MAX_PICTURE_BODY, MAX_RETRY_WAIT, MAX_SEGMENT_BODY, MIDDLE_TIER_AUDIENCE, PRESENCE_AUDIENCE, PROVISIONING_AUDIENCE,
    RELAY_HOST, REQUEST_TIMEOUT, Refusal, STREAM_TIMEOUT, TROUTER_HOST,
};
pub use loki::{
    ClockTime, FreeBusyStatus, LOKI_AUDIENCE, LokiAnswerError, LokiConfiguration, LokiOrganization, LokiOutOfOffice,
    LokiPerson, LokiSchedule, LokiWorkingWith, Organization, OutOfOfficeState, PersonDetails, PersonaRef, Schedule,
};
pub use meeting::{MeetingCode, MeetingDetails, MeetingThread};
#[cfg(feature = "test-support")]
pub use own::ProbeOwnState;
pub use own::{
    CALLING_APP_ID, CALLING_TEMPLATE, CLIENT_BUILD, CallingRegistration, EndpointRegistrar, EndpointRegistration,
    MESSAGING_APP_ID, MESSAGING_TEMPLATE, OwnState, REGISTRATION_TTL, RegisteredEndpoint, TROUTER_DOMAIN, register,
    register_calling,
};
pub use places::{PLACES_PAGE, Place, RoomLists, Rooms};
#[cfg(feature = "test-support")]
pub use post_read::ProbePostRead;
pub use post_read::{
    AnswerError, PRESENCE_PUSH_PATH, PostRead, PresenceRead, PresenceSubscription, ShortProfile, ShortProfileLookup,
    SubscriptionStatus, UserPresence,
};
pub use read::{
    CSA_TEAMS_PATH, CSA_TEAMS_QUERY, Channel, ChannelMeetup, ChannelType, CsaAnswerError, CsaTeams, GRAPH_PICTURE_SIZE,
    GRAPH_USER_SELECT, GraphAnswerError, GraphPicture, GraphUser, MAX_PICTURE, Picture, PictureFormat, Read, Team,
    TeamAbout, TeamsAnswer, object_id, running_call, start_ended,
};
pub use recap::{MCPS_AUDIENCE, MeetingContent, MeetingContents, RecapPart, TranscriptLocation};
pub use recording::{
    MediaSegmentLink, Presentation, PresentedTrack, RecordingAccess, RecordingExpiration, RecordingFile,
    RecordingManifest, TrackKind,
};
pub use recurrence::{Pattern as RecurrencePattern, Recurrence, WeekIndex, Weekday};
pub use relay::{RelayAnswer, RelayAnswerError, RelayCredential, RelayTokens};
pub use request::Request;
pub use schedule::{
    AVAILABILITY_DAYS, BusyTime, FreeBusy, FreeBusyAsk, MeetingTimes, PersonSchedule, SUGGESTION_DAYS, Schedules,
};
pub use search::{
    ALL_PAGE, Found, FoundChannel, FoundChat, FoundFile, FoundMessage, FoundPerson, PAGE, SUBSTRATE_AUDIENCE,
    SUBSTRATE_HOST, SearchAnswerError, SearchKind, SubstrateSearch, highlighted,
};
pub use team_apps::{
    AppDefinitions, AppStore, DEFINITIONS_PAGE, DefaultTeamPicture, InstalledApp, OwnedGroups, StoreApp,
    TEMPLATE_LOCALE, TeamApps, TeamTemplateCard, TeamTemplateDetails, TeamTemplateRead, TeamTemplates, TemplateChannel,
};
pub use team_read::{
    AddablePeopleSearch, AllTeamsAnalyticsRead, AnalyticsPeriod, ChannelAnalyticsRead, ChannelOwnProperty,
    JoinableTeam, JoinableTeams, ManagedChannel, OwnTeamView, PendingInvite, PendingInvites, RosterMember, TagsRead,
    TeamAnalytics, TeamAnalyticsRead, TeamChannels, TeamCode, TeamJoinRequests, TeamPicture, TeamRoster, TeamTag,
    TeamTags, TeamThread,
};
pub use thumbnail::{FileThumbnails, THUMBNAIL_HOST_SUFFIX, ThumbnailLink, is_thumbnail_link};
pub use urlp::LinkPreviewPicture;
pub use write::{
    ACTIVITY_FEED, AcceptCall, AcceptParts, AcceptTransfer, AcknowledgeAcceptance, AcknowledgeMediaAnswer,
    AddChatMembers, AddParts, AddToCall, AnswerMedia, AnswerMediaParts, AppDefinition, ApplyChannelParameters,
    AttachCall, AttachParts, Author, CALL_REACTIONS, CALL_START_URL, CALLING_BASE, CALLING_HOST, CallContext, CallLink,
    CallParticipant, CallWriteError, ChannelParameters, ClientInfo, CreateChat, DecideLobby, DeclineCall,
    DeleteMessage, EditMessage, EndConversation, ForceAvailability, HoldCall, Invitee, JoinCall, JoinMedia, JoinParts,
    KeepAliveCall, LeaveCall, LeaveReason, Links, ManualAvailability, MarkFeedRead, MediaContent, MediaDescription,
    MediaDescriptions, MeetingInfo, MemberUpdate, NewTeam, Original, PublishState, RAISE_HANDS, RawBody, ReadReceipt,
    RemoveReaction, RemoveState, RenegotiateMedia, ReportRinging, ResumeCall, SendCallReaction, SendMessage,
    SetActivityRead, SetEndpointAvailability, SetMeetingTitle, SetReaction, SetUserGraphSettings, SharedHistory,
    StartCall, StartParts, TEAMS_ECHO_BOT_MRI, TEST_CALL_SCENARIO, TeamCopy, TeamRole, TeamWrite, TemplateTeam,
    TransferCall, Transferor, Typing, UpdateEndpointMetadata, UpdateEndpointState, UpdateMediaDescriptions,
    UpdateParticipantProperties, VoiceConsent, Wire, team_created, team_error_code,
};
pub use write::{
    AddChannelMembers, ChannelCreated, ChannelLayout, CreateChannel, NewChannel, NewChannelKind, ShowChannelByDefault,
    channel_error_code,
};
pub use write::{
    CancelChannelEvent, CancelEvent, CreateChannelEvent, CreateEvent, CreateOnlineMeeting, DeleteEvent, EventAttendee,
    EventDraft, EventShowAs, MeetNow, MeetNowMeeting, MeetingChannel, Organizer, RespondEvent, RsvpAnswer,
    UpdateChannelEvent, UpdateEvent, ZonedTime,
};
pub use write::{
    DEFAULT_TIMER_MS, MeetingJoinInfo, MeetingTimer, MuteParticipants, TimerState, TimerUpdate, UpdateMeetingSettings,
    UpdateMeetingStates, meeting_locked,
};

pub use write::{MeetingOption, MeetingOptions, MeetingOptionsRead, OptionsOf, SetMeetingOption};

pub use workflow::{MessageRun, MessageSender, Principal, WorkflowFailure, WorkflowRequest, WorkflowResponse};

/// Whether test-only transport origins are enabled in this build.
#[must_use]
pub const fn test_support_enabled() -> bool {
    cfg!(feature = "test-support")
}
