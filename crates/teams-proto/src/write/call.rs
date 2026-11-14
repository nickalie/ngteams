//! Typed call-control requests with validated operation links and protocol-shaped bodies.

mod meeting;
mod parts;

use std::fmt;

use crate::CallOperation;
use crate::Mri;
use serde_json::{Map, Value, json};
use url::Url;

pub use self::meeting::{
    DEFAULT_TIMER_MS, MeetingJoinInfo, MeetingTimer, MuteParticipants, TimerState, TimerUpdate, UpdateMeetingSettings,
    UpdateMeetingStates, meeting_locked,
};
pub(crate) use self::parts::on_calling_base;
pub use self::parts::{
    CALL_START_URL, CALLING_BASE, CALLING_HOST, CallContext, CallLink, CallParticipant, ChannelParameters, ClientInfo,
    Invitee, Links, MediaContent, MediaDescription, MediaDescriptions,
};

use super::Prepared;
use super::Wire;
use crate::endpoint::Method;

/// Why a call write wasn't made. Nothing was sent.
#[derive(Debug, thiserror::Error)]
pub enum CallWriteError {
    /// A protocol value, callback or operation link is outside the supported wire shape.
    #[error("the request isn't in a supported protocol shape")]
    Unmapped,
    /// The request's message id couldn't be made.
    #[error("the system's random source failed: {0}")]
    Random(getrandom::Error),
}

/// A call write as the transport sends it: method, exact URL, headers (by lower-cased name, none of them a
/// credential), body. Only this module makes one.
#[derive(Clone, PartialEq, Eq)]
pub struct CallWire {
    method: Method,
    url: Url,
    headers: Vec<(&'static str, String)>,
    body: Option<Value>,
}

impl CallWire {
    pub(crate) fn body(&self) -> Option<&Value> {
        self.body.as_ref()
    }

    pub(crate) fn method(&self) -> Method {
        self.method
    }

    pub(crate) fn url(&self) -> &Url {
        &self.url
    }

    pub(crate) fn headers(&self) -> &[(&'static str, String)] {
        &self.headers
    }

    /// The body's bytes, exactly as classified and as sent.
    pub(crate) fn body_bytes(&self) -> Option<Vec<u8>> {
        self.body.as_ref().map(body_bytes)
    }
}

/// `Debug` shows the method, the host and path (never the query) and the body's length.
impl fmt::Debug for CallWire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallWire")
            .field("method", &self.method)
            .field(
                "url",
                &format_args!("{}{}", self.url.host_str().unwrap_or_default(), self.url.path()),
            )
            .field("body_len", &self.body_bytes().map(|body| body.len()))
            .finish_non_exhaustive()
    }
}

/// A JSON body as sent.
fn body_bytes(body: &Value) -> Vec<u8> {
    body.to_string().into_bytes()
}

/// A built call write: its target, as classified, and its wire.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Built {
    wire: CallWire,
}

/// The write a built request must classify as.
#[derive(Clone, Copy)]
enum Expect {
    /// The test call's start: `Call` on `Scope::New`.
    TestStart,
    /// A chat's call start: `Call` on `Scope::Conversation`.
    ChatStart,
    /// A channel's meeting start: `Call` on `Scope::Team`, the channel's.
    ChannelStart,
    /// This operation on `Scope::CallOperation`.
    Operation(CallOperation),
}

/// One request to build: what is sent, and what it must classify as.
struct Plan<'a> {
    context: &'a CallContext,
    method: Method,
    url: &'a Url,
    body: Option<Value>,
    expect: Expect,
}

/// Builds `plan`'s request and classifies it (see the module docs).
fn build(plan: Plan<'_>) -> Result<Built, CallWriteError> {
    if !crate::http::calling_base_in(plan.context.cloud(), plan.url)
        || plan
            .body
            .as_ref()
            .is_some_and(|body| !own_links_fit(plan.context.cloud(), body))
    {
        return Err(CallWriteError::Unmapped);
    }
    if let Expect::Operation(operation) = plan.expect {
        let last = plan
            .url
            .path_segments()
            .and_then(|mut parts| parts.rfind(|part| !part.is_empty()))
            .unwrap_or_default();
        if !operation.segment().is_empty() && last != operation.segment() {
            return Err(CallWriteError::Unmapped);
        }
    }
    let headers = plan.context.headers().map_err(CallWriteError::Random)?;
    Ok(Built {
        wire: CallWire {
            method: plan.method,
            url: plan.url.clone(),
            headers,
            body: plan.body,
        },
    })
}

fn own_links_fit(cloud: &crate::Cloud, value: &Value) -> bool {
    if cloud.is_microsoft() {
        return true;
    }
    match value {
        Value::Array(values) => values.iter().all(|value| own_links_fit(cloud, value)),
        Value::Object(fields) => fields.iter().all(|(name, value)| match name.as_str() {
            "links" | "clientContentForMediaController" => value
                .as_object()
                .is_some_and(|links| links.values().all(|value| own_url_fits(cloud, value))),
            "rosterUpdate" | "url" | "meetingUrl" => own_url_fits(cloud, value),
            _ => own_links_fit(cloud, value),
        }),
        _ => true,
    }
}

fn own_url_fits(cloud: &crate::Cloud, value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|text| Url::parse(text).is_ok_and(|url| cloud.contains(&url)))
}

/// [`build`] of an operation on a link, with no registry step.
fn operation(
    context: &CallContext,
    operation: CallOperation,
    link: &CallLink,
    body: Option<Value>,
) -> Result<Built, CallWriteError> {
    let method = match operation.method() {
        "PUT" => Method::Put,
        "DELETE" => Method::Delete,
        _ => Method::Post,
    };
    build(Plan {
        context,
        method,
        url: link.url(),
        body,
        expect: Expect::Operation(operation),
    })
}

/// The properties of the conversation a start or a join opens, as every capture sent them.
fn conversation_properties() -> Value {
    json!({
        "allowConversationWithoutHost": true,
        "enableGroupCallEventMessages": true,
        "enableGroupCallUpgradeMessage": false,
        "enableGroupCallMeetupGeneration": false,
    })
}

/// The endpoint's state as a start, a join and an answer report it.
fn endpoint_state(sequence: u64) -> Value {
    json!({
        "endpointStateSequenceNumber": sequence,
        "endpointProperties": {"additionalEndpointProperties": {"infoShownInReportMode": "FullInformation"}},
    })
}

/// What a call start sends besides its kind (the test call or a chat's): who calls, the endpoint's callbacks for the
/// conversation (its roster's and its `links`), for the call (`progress`, `mediaAnswer`, `acceptance`, `redirection`,
/// `end`) and for the media controller (`controlVideoStreaming`, `csrcInfo`), the call's modalities and offer, and what
/// the client says of itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartParts {
    pub from: CallParticipant,
    pub roster_update: String,
    pub conversation_links: Links,
    pub call_links: Links,
    pub media_controller_links: Links,
    pub modalities: Vec<String>,
    pub media: MediaContent,
    pub client: ClientInfo,
}

/// The call invitation of a start or a join: its modalities, callbacks, offer and the client's description.
fn call_invitation(
    modalities: &[String],
    call_links: &Links,
    media_controller_links: &Links,
    media: &MediaContent,
    client: &ClientInfo,
) -> Value {
    let mut invitation = Map::new();
    invitation.insert("callModalities".to_owned(), json!(modalities));
    for key in ["replaces", "transferor", "clientTransferContext", "customContext"] {
        invitation.insert(key.to_owned(), Value::Null);
    }
    invitation.insert("links".to_owned(), call_links.json());
    if !media_controller_links.is_empty() {
        invitation.insert(
            "clientContentForMediaController".to_owned(),
            media_controller_links.json(),
        );
    }
    if let Some(pstn) = client.pstn_content() {
        invitation.insert("pstnContent".to_owned(), pstn);
    }
    invitation.insert("emergencyContent".to_owned(), Value::Null);
    invitation.insert("mediaContent".to_owned(), media.json());
    invitation.insert("voicemailSettings".to_owned(), json!({}));
    for key in ["locationContent", "networkContent", "areaContent"] {
        invitation.insert(key.to_owned(), Value::Null);
    }
    Value::Object(invitation)
}

/// A call start's body (`docs/protocol/calling.md`, "The start"): the test call's with its `scenario`, a chat's with
/// none.
fn start_body(parts: &StartParts, scenario: Option<&str>, to: &[Value], group_chat: Value) -> Value {
    let mut request = Map::new();
    request.insert("conversationType".to_owned(), Value::Null);
    request.insert("subject".to_owned(), Value::Null);
    request.insert("suppressDialout".to_owned(), json!(scenario.is_some()));
    request.insert(
        "roster".to_owned(),
        json!({"type": "Delta", "rosterUpdate": parts.roster_update}),
    );
    request.insert("properties".to_owned(), conversation_properties());
    request.insert("links".to_owned(), parts.conversation_links.json());
    if let Some(scenario) = scenario {
        request.insert("scenario".to_owned(), json!(scenario));
    }
    let mut body = Map::new();
    body.insert("conversationRequest".to_owned(), Value::Object(request));
    body.insert("contentSharing".to_owned(), Value::Null);
    body.insert("participants".to_owned(), json!({"from": parts.from.json(), "to": to}));
    parts.client.add_capabilities(&mut body, true);
    body.insert("endpointMetadata".to_owned(), json!({}));
    for key in ["groupContext", "meetingInfo", "meetingData"] {
        body.insert(key.to_owned(), Value::Null);
    }
    body.insert("groupChat".to_owned(), group_chat);
    body.insert("endpointState".to_owned(), endpoint_state(1));
    let invitation = call_invitation(
        &parts.modalities,
        &parts.call_links,
        &parts.media_controller_links,
        &parts.media,
        &parts.client,
    );
    body.insert("callInvitation".to_owned(), invitation);
    Value::Object(body)
}

/// A meeting's start (`docs/protocol/calling.md`, "Joining a meeting"): a chat call's body, nobody named, the meeting's
/// chat and its organizer in it.
fn meeting_start_body(parts: &StartParts, chat: &str, meeting: &MeetingInfo) -> Value {
    let mut body = start_body(
        parts,
        None,
        &[],
        json!({"threadId": chat, "messageId": MEETING_MESSAGE_ID}),
    );
    if let Some(suppress) = body.pointer_mut("/conversationRequest/suppressDialout") {
        *suppress = json!(true);
    }
    if let Some(info) = body.get_mut("meetingInfo") {
        *info = json!({
            "tenantId": meeting.tenant_id,
            "organizerId": meeting.organizer_id,
            "meetingType": MEETING_TYPE_CHAT,
        });
    }
    body
}

/// A channel's Meet now's subscription (`docs/protocol/calling.md`, "A channel's meeting"), as the web client's
/// pre-join sends it (seen 2026-11-03): the conversation's callbacks, the service asked to make the meeting and post it
/// to the channel (`enableGroupCallMeetupGeneration`), the channel in `groupChat` with no message, no `meetingInfo`,
/// nobody named and no call invitation. Its answer is a new `adHocMeeting` conversation; a start with media instead
/// (the web client's when its subscription fails) makes a plain group call in the channel, which no meeting post or
/// CSA `activeMeetups` names.
fn meet_now_body(parts: &StartParts, channel: &str) -> Value {
    let mut properties = conversation_properties();
    properties["enableGroupCallMeetupGeneration"] = json!(true);
    let mut body = Map::new();
    body.insert(
        "conversationRequest".to_owned(),
        json!({
            "subject": null,
            "roster": {"type": "Delta", "rosterUpdate": parts.roster_update},
            "properties": properties,
            "links": parts.conversation_links.json(),
        }),
    );
    body.insert("participants".to_owned(), json!({"from": parts.from.json()}));
    parts.client.add_capabilities(&mut body, true);
    body.insert("endpointMetadata".to_owned(), json!({}));
    body.insert("groupContext".to_owned(), Value::Null);
    body.insert("groupChat".to_owned(), json!({"threadId": channel, "messageId": null}));
    body.insert("meetingInfo".to_owned(), Value::Null);
    body.insert("endpointState".to_owned(), endpoint_state(0));
    Value::Object(body)
}

/// A meeting's `groupChat.messageId` when its chat is its own, as captured.
const MEETING_MESSAGE_ID: &str = "0";
/// A meeting's `meetingInfo.meetingType` when its chat is its own, scheduled or a series' (the web client's `Chat`; a
/// channel's meeting is another), as captured.
const MEETING_TYPE_CHAT: u64 = 1;

/// Starts a call: `POST` [`CALL_START_URL`].
///
/// - [`StartCall::echo_test`]: the test call, as the web client places it: a conversation of the caller alone (no
///   callee, the scenario [`TEST_CALL_SCENARIO`]), into which the caller then adds the echo service ([`AddToCall`]).
/// - [`StartCall::in_chat`]: a call from a 1:1 or group chat: its callees in `participants.to`, the group chat in
///   `groupChat` (`null` for a 1:1 call).
/// - [`StartCall::in_meeting`]: a meeting's call from its pre-join, started or not: nobody named, nobody rings; the
///   service answers with the meeting's one conversation.
/// - [`StartCall::meet_now`]: a channel's Meet now, its first step: the subscription to a new meeting in the channel,
///   nobody named and no call invitation (it enters no call); the service answers with the meeting's conversation
///   (`adHocMeeting`), which the account then enters as its organizer ([`JoinCall::enter`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartCall {
    caller: Mri,
    echo: Option<Mri>,
    built: Built,
}

impl StartCall {
    /// The test call of `parts.from` for `echo` (see the type's docs).
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't the test call's start in its captured shape.
    pub fn echo_test(context: &CallContext, parts: &StartParts, echo: &Mri) -> Result<Self, CallWriteError> {
        if parts.media.required_features.is_some()
            || parts
                .call_links
                .iter()
                .any(|(name, _)| !["progress", "mediaAnswer", "acceptance", "redirection", "end"].contains(&name))
        {
            return Err(CallWriteError::Unmapped);
        }
        let body = start_body(parts, Some(TEST_CALL_SCENARIO), &[], Value::Null);
        Self::start(context, parts, body, Expect::TestStart, Some(echo))
    }

    /// A call from a chat to `callees`: a group chat's (`group_chat`, its id) or, with none, the 1:1 chat of the caller
    /// and its one callee (see the type's docs).
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a chat's call start in its captured shape (no callee, a callee twice
    /// or the caller, an MRI that isn't a work account, a group chat id that isn't one, two callees and no group chat).
    pub fn in_chat(
        context: &CallContext,
        parts: &StartParts,
        callees: &[Invitee],
        group_chat: Option<&str>,
    ) -> Result<Self, CallWriteError> {
        let valid_chat = group_chat.map_or(callees.len() == 1, crate::is_group_chat_id);
        let mut people = std::collections::BTreeSet::new();
        if !valid_chat
            || callees.is_empty()
            || callees.iter().any(|callee| {
                !callee.mri.as_str().starts_with("8:orgid:")
                    || callee.mri == parts.from.mri
                    || !people.insert(&callee.mri)
            })
        {
            return Err(CallWriteError::Unmapped);
        }
        let to: Vec<Value> = callees.iter().map(Invitee::json).collect();
        let chat = group_chat.map_or(Value::Null, |id| json!({"threadId": id, "messageId": null}));
        let body = start_body(parts, None, &to, chat);
        Self::start(context, parts, body, Expect::ChatStart, None)
    }

    /// The transferred party's call to the target of the transfer its far end asked for: a 1:1 call to `target`, its
    /// `callInvitation.transferor` the delivered `transferor` with its authorization (seen 2026-11-06, the web callee
    /// once its far end transferred it).
    ///
    /// # Errors
    /// [`CallWriteError`] when `target` isn't a work account other than the caller.
    pub fn transferred(
        context: &CallContext,
        parts: &StartParts,
        target: &Invitee,
        transferor: &Transferor,
    ) -> Result<Self, CallWriteError> {
        if !target.mri.as_str().starts_with("8:orgid:") || target.mri == parts.from.mri {
            return Err(CallWriteError::Unmapped);
        }
        let mut body = start_body(parts, None, &[target.json()], Value::Null);
        if let Some(slot) = body.pointer_mut("/callInvitation/transferor") {
            *slot = transferor.json();
        }
        Self::start(context, parts, body, Expect::ChatStart, None)
    }

    /// The call of the meeting whose chat is `chat` (see the type's docs).
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a meeting's start in its captured shape (the chat not a meeting's, the
    /// sender or the organizer not a work account, the tenant not a GUID).
    pub fn in_meeting(
        context: &CallContext,
        parts: &StartParts,
        chat: &str,
        meeting: &MeetingInfo,
    ) -> Result<Self, CallWriteError> {
        if !crate::is_meeting_chat_id(chat) {
            return Err(CallWriteError::Unmapped);
        }
        let body = meeting_start_body(parts, chat, meeting);
        Self::start(context, parts, body, Expect::ChatStart, None)
    }

    /// Join by the code and passcode from an invitation. The service resolves the meeting; no chat or
    /// organizer is inferred from the account's sidebar. This is the short-link start with media.
    ///
    /// # Errors
    /// [`CallWriteError`] when the code or passcode is not valid input.
    pub fn by_code(
        context: &CallContext,
        parts: &StartParts,
        code: &crate::MeetingCode,
    ) -> Result<Self, CallWriteError> {
        if crate::MeetingCode::typed(&code.id, &code.passcode).as_ref() != Some(code) {
            return Err(CallWriteError::Unmapped);
        }
        let mut body = start_body(parts, None, &[], Value::Null);
        body["conversationRequest"]["suppressDialout"] = json!(true);
        let meeting_path = format!("/meet/{}", code.id);
        let mut meeting_url = context
            .cloud()
            .service_url(&format!("https://teams.microsoft.com{meeting_path}"), &meeting_path)
            .map_err(|_| CallWriteError::Unmapped)?;
        meeting_url.set_query(Some(&format!("p={}", code.passcode)));
        body["meetingData"] = json!({
            "meetingCode": code.id,
            "passcode": code.passcode,
            "meetingUrl": meeting_url.as_str(),
        });
        body["meetingPreferences"] = json!({"shouldResurrect": "resurrect"});
        Self::start(context, parts, body, Expect::ChatStart, None)
    }

    /// The subscription to a new meeting in the channel `channel` (a team's channel thread), its Meet now (see the
    /// type's docs). Of `parts` it sends who subscribes, the conversation's callbacks and the client's capabilities;
    /// it carries no call and pends nothing in the call registry.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a channel meeting's subscription in its captured shape (the channel
    /// not a team's thread, the sender not a work account).
    pub fn meet_now(context: &CallContext, parts: &StartParts, channel: &str) -> Result<Self, CallWriteError> {
        if !crate::is_team_thread_id(channel) {
            return Err(CallWriteError::Unmapped);
        }
        let body = meet_now_body(parts, channel);
        Self::start_as(context, parts, body, Expect::ChannelStart, None)
    }

    fn start(
        context: &CallContext,
        parts: &StartParts,
        body: Value,
        expect: Expect,
        echo: Option<&Mri>,
    ) -> Result<Self, CallWriteError> {
        Self::start_as(context, parts, body, expect, echo)
    }

    fn start_as(
        context: &CallContext,
        parts: &StartParts,
        body: Value,
        expect: Expect,
        echo: Option<&Mri>,
    ) -> Result<Self, CallWriteError> {
        let url = context
            .cloud()
            .service_url(CALL_START_URL, "/flightproxy/api/v2/epconv")
            .map_err(|_| CallWriteError::Unmapped)?;
        let built = build(Plan {
            context,
            method: Method::Post,
            url: &url,
            body: Some(body),
            expect,
        })?;
        Ok(Self {
            caller: parts.from.mri.clone(),
            echo: echo.cloned(),
            built,
        })
    }

    /// The account placing the call.
    #[must_use]
    pub fn caller(&self) -> &Mri {
        &self.caller
    }

    /// The echo service a test call is for (the start doesn't name it: an [`AddToCall`] adds it); `None` for a chat's
    /// call.
    #[must_use]
    pub fn echo(&self) -> Option<&Mri> {
        self.echo.as_ref()
    }
}

impl Prepared for StartCall {
    fn wire(&self) -> Option<Wire> {
        None
    }

    fn call(&self) -> Option<&CallWire> {
        Some(&self.built.wire)
    }
}

/// The conversation scenario of the web client's test call (`docs/protocol/calling-echo.md`): the call start that
/// opens it names no callee.
pub use crate::ids::TEST_CALL_SCENARIO;

/// The echo service's MRI from the web bundle's `teamsEchoBotMri` (its commercial config): the bot the personal build's
/// "Test call" calls ([`StartCall::echo_test`], then [`AddToCall`] of exactly it). Never taken from the network or UI.
pub use crate::ids::TEAMS_ECHO_BOT_MRI;

/// Invites someone into a call that exists: the conversation's `add` (how the test call brings the echo in).
///
/// [`AddToCall::invite`] posts to the `addParticipantAndModality` link the start's answer handed out (`…/conv/<id>/add`
/// with the conversation's query), from the call's caller, naming the 1:1 chat of the caller and the invitee and the
/// caller as the meeting's organizer, as the web client does; its target is `WriteKind::Invite` on
/// `Scope::CallOperation` with the invitee added, which the call registry resolves and judges per invitee.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddToCall {
    invitee: Mri,
    url: Url,
    built: Built,
}

/// What an `add` sends besides its link: who invites, whom (with a fresh participant id), the 1:1 chat of the two (its
/// thread id), the tenant the caller organizes the call in, and the endpoint's callbacks for the add's outcome
/// (`addParticipantSuccess`, `addParticipantFailure`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddParts {
    pub from: CallParticipant,
    pub invitee: Invitee,
    pub chat_thread: String,
    pub tenant_id: String,
    pub links: Links,
}

impl AddToCall {
    /// The `add` at `link` (see the type's docs).
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't an `add` in its captured shape: the caller's MRI not
    /// `8:orgid:<object id>`, a chat other than the 1:1 chat of the caller and the invitee, a link of another operation.
    pub fn invite(context: &CallContext, link: &CallLink, parts: &AddParts) -> Result<Self, CallWriteError> {
        let chat = parts
            .invitee
            .mri
            .as_str()
            .strip_prefix("28:")
            .and_then(|bot| {
                parts
                    .from
                    .mri
                    .as_str()
                    .strip_prefix("8:orgid:")
                    .map(|caller| format!("19:{caller}_{bot}@unq.gbl.spaces"))
            })
            .or_else(|| crate::one_to_one_id(parts.from.mri.as_str(), parts.invitee.mri.as_str()));
        if chat.as_deref() != Some(parts.chat_thread.as_str()) {
            return Err(CallWriteError::Unmapped);
        }
        let organizer = parts
            .from
            .mri
            .as_str()
            .strip_prefix("8:orgid:")
            .ok_or(CallWriteError::Unmapped)?;
        let body = json!({
            "disableUnmute": false,
            "participants": {"from": parts.from.json(), "to": [parts.invitee.json()]},
            "participantInvitationData": {
                "invitationData": {"meetingInfo": {"organizerId": organizer, "tenantId": parts.tenant_id}},
            },
            "replacementDetails": null,
            "groupContext": null,
            "groupChat": {"threadId": parts.chat_thread, "messageId": null},
            "links": parts.links.json(),
        });
        let built = operation(context, CallOperation::Add, link, Some(body))?;
        Ok(Self {
            invitee: parts.invitee.mri.clone(),
            url: link.url().clone(),
            built,
        })
    }

    /// Someone added to a call between people, as the web client's People pane adds them (seen 2026-11-06, `ab-call
    /// --call steps`): `from` the account, `to` the invitee with a fresh participant id, no invitation data, the call's
    /// group chat (`group_chat`: a 1:1 call's new group chat of the three, made first, or a group call's own) with no
    /// message, and the `addParticipantSuccess`/`addParticipantFailure` callbacks of `links`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't such an `add` in its captured shape: the invitee not a work account or
    /// the account itself, the chat not a group chat, a link of another operation.
    pub fn person(
        context: &CallContext,
        link: &CallLink,
        from: &CallParticipant,
        invitee: &Invitee,
        group_chat: &str,
        links: &Links,
    ) -> Result<Self, CallWriteError> {
        if !invitee.mri.as_str().starts_with("8:orgid:")
            || invitee.mri == from.mri
            || !crate::is_group_chat_id(group_chat)
            || links
                .iter()
                .any(|(name, _)| !["addParticipantSuccess", "addParticipantFailure"].contains(&name))
        {
            return Err(CallWriteError::Unmapped);
        }
        let body = json!({
            "disableUnmute": false,
            "participants": {"from": from.json(), "to": [invitee.json()]},
            "participantInvitationData": {},
            "replacementDetails": null,
            "groupContext": null,
            "groupChat": {"threadId": group_chat, "messageId": null},
            "links": links.json(),
        });
        let built = operation(context, CallOperation::Add, link, Some(body))?;
        Ok(Self {
            invitee: invitee.mri.clone(),
            url: link.url().clone(),
            built,
        })
    }

    /// Who is invited.
    #[must_use]
    pub fn invitee(&self) -> &Mri {
        &self.invitee
    }

    /// Where the `add` is posted: the link it was built with.
    #[must_use]
    pub fn url(&self) -> Url {
        self.url.clone()
    }
}

impl Prepared for AddToCall {
    fn wire(&self) -> Option<Wire> {
        None
    }

    fn call(&self) -> Option<&CallWire> {
        Some(&self.built.wire)
    }
}

/// The call writes that are only ever built whole: a newtype over [`Built`], its target and wire from it.
macro_rules! call_write {
    ($($name:ident),* $(,)?) => {$(


        impl Prepared for $name {
            fn wire(&self) -> Option<Wire> {
                None
            }

            fn call(&self) -> Option<&CallWire> {
                Some(&self.0.wire)
            }
        }
    )*};
}

call_write!(
    JoinCall,
    AttachCall,
    ReportRinging,
    UpdateEndpointState,
    AcceptCall,
    DeclineCall,
    AcknowledgeAcceptance,
    ApplyChannelParameters,
    UpdateEndpointMetadata,
    UpdateParticipantProperties,
    LeaveCall,
    RenegotiateMedia,
    AnswerMedia,
    AcknowledgeMediaAnswer,
    UpdateMediaDescriptions,
    KeepAliveCall,
    DecideLobby,
    PublishState,
    RemoveState,
    SendCallReaction,
    EndConversation,
    HoldCall,
    ResumeCall,
);

call_write!(UpdateMeetingStates, UpdateMeetingSettings, MuteParticipants);

/// The meeting a call is, as a join or a start names it (`meetingInfo`): its organizer's object id and tenant.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MeetingInfo {
    pub organizer_id: String,
    pub tenant_id: String,
}

/// What a join of a running call sends: who joins, the endpoint's callbacks for the conversation, the group chat the
/// call belongs to, the meeting, what the client says of itself, and to enter the call ([`JoinCall::enter`]) its
/// media ([`JoinMedia`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinParts {
    pub from: CallParticipant,
    pub roster_update: String,
    pub conversation_links: Links,
    pub group_chat: String,
    /// The message of the chat the call belongs to (`groupChat.messageId`): `"0"` for a group chat's or a meeting
    /// chat's call, the meeting's post for a channel's meeting, `None` (`null`) for the organizer's entry into the
    /// channel meeting it started (Meet now), which has no post yet.
    pub message_id: Option<String>,
    pub meeting: Option<MeetingInfo>,
    /// The service is asked to post the meeting to its channel (`enableGroupCallMeetupGeneration`): the organizer's
    /// entry after Meet now, as the web client's.
    pub posts_meetup: bool,
    pub client: ClientInfo,
}

/// What a join that enters the call adds: the endpoint's callbacks for the call and the media controller, the
/// modalities and the offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinMedia {
    pub call_links: Links,
    pub media_controller_links: Links,
    pub modalities: Vec<String>,
    pub media: MediaContent,
}

/// A join of a running call's conversation: `POST` to the conversation's own link (the invitation's
/// `conversationController`), from the chat it belongs to. With media ([`JoinCall::enter`]) the sender enters the call
/// (the call registry takes it as a start in that chat, pended before it goes and registered from its answer); without
/// ([`JoinCall::subscribe`]) it subscribes to the conversation's roster only, as the web client does once its ring
/// ended. Either is `WriteKind::Call` on `Scope::CallOperation` (`join`), resolved by the invitation that bound the
/// conversation to its group chat, the chat read fresh.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinCall(Built);

impl JoinCall {
    /// The join that enters the call at `conversation` with `media`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a join in its captured shape (the group chat not a group chat's id,
    /// a link that isn't a conversation's).
    pub fn enter(
        context: &CallContext,
        conversation: &CallLink,
        parts: &JoinParts,
        media: &JoinMedia,
    ) -> Result<Self, CallWriteError> {
        if crate::is_team_thread_id(&parts.group_chat) && parts.meeting.is_none() && !parts.posts_meetup {
            return Err(CallWriteError::Unmapped);
        }
        let mut body = join_body(parts, &json!(""));
        let invitation = call_invitation(
            &media.modalities,
            &media.call_links,
            &media.media_controller_links,
            &media.media,
            &parts.client,
        );
        body.insert("callInvitation".to_owned(), invitation);
        Self::join(context, conversation, body)
    }

    /// The roster-only join of the conversation at `conversation`.
    ///
    /// # Errors
    /// As [`Self::enter`].
    pub fn subscribe(
        context: &CallContext,
        conversation: &CallLink,
        parts: &JoinParts,
    ) -> Result<Self, CallWriteError> {
        Self::join(context, conversation, join_body(parts, &Value::Null))
    }

    fn join(context: &CallContext, conversation: &CallLink, body: Map<String, Value>) -> Result<Self, CallWriteError> {
        let built = build(Plan {
            context,
            method: Method::Post,
            url: conversation.url(),
            body: Some(Value::Object(body)),
            expect: Expect::Operation(CallOperation::Join),
        })?;
        Ok(Self(built))
    }
}

/// A join's body without its call invitation (`docs/protocol/calling.md`, "Joining a running call").
fn join_body(parts: &JoinParts, subject: &Value) -> Map<String, Value> {
    let request = json!({
        "conversationType": null,
        "subject": subject,
        "roster": {"type": "Delta", "rosterUpdate": parts.roster_update},
        "properties": conversation_properties(),
        "links": parts.conversation_links.json(),
    });
    let mut request = request;
    if parts.posts_meetup {
        request["properties"]["enableGroupCallMeetupGeneration"] = json!(true);
    }
    let mut body = Map::new();
    body.insert("conversationRequest".to_owned(), request);
    body.insert("groupContext".to_owned(), Value::Null);
    body.insert(
        "groupChat".to_owned(),
        json!({"threadId": parts.group_chat, "messageId": parts.message_id}),
    );
    body.insert("participants".to_owned(), json!({"from": parts.from.json()}));
    parts.client.add_capabilities(&mut body, true);
    body.insert("endpointMetadata".to_owned(), json!({}));
    if let Some(meeting) = &parts.meeting {
        body.insert(
            "meetingInfo".to_owned(),
            json!({"organizerId": meeting.organizer_id, "tenantId": meeting.tenant_id, "isBroadcast": false}),
        );
    }
    body.insert("meetingData".to_owned(), Value::Null);
    body.insert("endpointState".to_owned(), endpoint_state(0));
    body
}

/// What an `attach` sends: the callee, the call leg's `end` callback, the conversation subscription's callbacks (its
/// roster's and its `links`), the conversation it subscribes to (the invitation's `conversationController`), and the
/// client's endpoint capabilities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachParts {
    pub from: CallParticipant,
    pub end: String,
    pub roster_update: String,
    pub conversation_links: Links,
    pub conversation: CallLink,
    pub client: ClientInfo,
}

/// The callee's endpoint attached to its ringing call leg: `POST` to the invitation's `attach` link, with the
/// roster-only subscription to the call's conversation (it joins no call, answers nothing). Sent by the client by
/// itself while an offered invitation rings; its answer hands out the leg's `progress`, `acceptance` and `callLeg`
/// links, which the call tracker binds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachCall(Built);

impl AttachCall {
    /// The `attach` at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't an `attach` in its captured shape.
    pub fn new(context: &CallContext, link: &CallLink, parts: &AttachParts) -> Result<Self, CallWriteError> {
        let mut input = Map::new();
        parts.client.add_capabilities(&mut input, false);
        input.insert(
            "conversationRequest".to_owned(),
            json!({
                "roster": {"type": "Delta", "rosterUpdate": parts.roster_update},
                "links": parts.conversation_links.json(),
            }),
        );
        input.insert("endpointMetadata".to_owned(), json!({}));
        input.insert("participants".to_owned(), json!({"from": parts.from.json()}));
        let mut body = Map::new();
        body.insert(
            "attach".to_owned(),
            json!({
                "requireMediaContent": false,
                "links": {"end": parts.end},
                "locationContent": null,
                "networkContent": null,
                "areaContent": null,
            }),
        );
        parts.client.add_capabilities(&mut body, false);
        body.insert(
            "additionalActions".to_owned(),
            json!([{
                "name": "join",
                "url": parts.conversation.as_str(),
                "waitForResponse": true,
                "input": Value::Object(input),
            }]),
        );
        let built = operation(context, CallOperation::Attach, link, Some(Value::Object(body)))?;
        Ok(Self(built))
    }
}

/// The callee's endpoint tells the call controller it rings: `POST` to the `progress` link the `attach`'s answer handed
/// out, `ringing` only. Sent by the client by itself while an offered invitation rings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportRinging(Built);

impl ReportRinging {
    /// `sender` rings, at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a `progress` in its captured shape.
    pub fn new(context: &CallContext, link: &CallLink, sender: &CallParticipant) -> Result<Self, CallWriteError> {
        let body = json!({"callProgress": {"sender": sender.json(), "status": "ringing", "phrase": "ringing"}});
        Ok(Self(operation(context, CallOperation::Progress, link, Some(body))?))
    }
}

/// The callee's endpoint state in the call's conversation (muted or not): `POST` to the conversation subscription's
/// `updateEndpointState` link, as the callee sends it when it answers and on its call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateEndpointState(Built);

impl UpdateEndpointState {
    /// `from`'s state at `link`: `muted`, the `sequence`th report of the call.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't an endpoint state in its captured shape.
    pub fn new(
        context: &CallContext,
        link: &CallLink,
        from: &CallParticipant,
        muted: bool,
        sequence: u64,
    ) -> Result<Self, CallWriteError> {
        let mut state = endpoint_state(sequence);
        if let Some(state) = state.as_object_mut() {
            state.insert("state".to_owned(), json!({"isMuted": muted}));
        }
        let body = json!({"from": from.json(), "endpointState": state});
        Ok(Self(operation(
            context,
            CallOperation::UpdateEndpointState,
            link,
            Some(body),
        )?))
    }
}

/// What an answer sends: who answers, the modalities it accepts, its endpoint's callbacks for the call (the names of an
/// acknowledgement's: `mediaRenegotiation`, `transfer`, …) and for the media controller, its media answer, and what the
/// client says of itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptParts {
    pub accepted_by: CallParticipant,
    pub modalities: Vec<String>,
    pub call_links: Links,
    pub media_controller_links: Links,
    pub media: MediaContent,
    pub client: ClientInfo,
}

/// The callee answers an offered invitation: `POST` to the `acceptance` link its `attach`'s answer handed out. Its
/// answer (an inline acknowledgement) hands out the answered call's links, which the call tracker binds. Only on an explicit answer of an invitation the engine offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptCall(Built);

impl AcceptCall {
    /// The answer at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't an `accept` in its captured shape.
    pub fn new(context: &CallContext, link: &CallLink, parts: &AcceptParts) -> Result<Self, CallWriteError> {
        let mut acceptance = Map::new();
        acceptance.insert("acceptedBy".to_owned(), parts.accepted_by.json());
        acceptance.insert("acceptedCallModalities".to_owned(), json!(parts.modalities));
        parts.client.add_capabilities(&mut acceptance, true);
        acceptance.insert("links".to_owned(), parts.call_links.json());
        if !parts.media_controller_links.is_empty() {
            acceptance.insert(
                "clientContentForMediaController".to_owned(),
                parts.media_controller_links.json(),
            );
        }
        acceptance.insert("mediaContent".to_owned(), parts.media.json());
        if let Some(pstn) = parts.client.pstn_content() {
            acceptance.insert("pstnContent".to_owned(), pstn);
        }
        acceptance.insert("callKeepAliveInterval".to_owned(), Value::Null);
        let body = json!({"callAcceptance": Value::Object(acceptance)});
        let built = operation(context, CallOperation::Accept, link, Some(body))?;
        Ok(Self(built))
    }
}

/// The callee declines an offered invitation, as the web client does: `DELETE` to the `callLeg` link its `attach`'s
/// answer handed out, the call's end `603` `CallEndReasonLocalUserInitiated`. The ringing stops everywhere and the
/// caller sees it declined. Only on an explicit decline of an invitation the engine offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclineCall(Built);

impl DeclineCall {
    /// The decline at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the link isn't a `reject`'s.
    pub fn new(context: &CallContext, link: &CallLink) -> Result<Self, CallWriteError> {
        let body = json!({"callEnd": {
            "code": 603,
            "subCode": 0,
            "phrase": "CallEndReasonLocalUserInitiated",
            "resultCategories": ["Success"],
        }});
        Ok(Self(operation(context, CallOperation::Reject, link, Some(body))?))
    }
}

/// The caller acknowledges its call's acceptance by request: `POST` to the acceptance's `acknowledgement` link, handing
/// the call controller the endpoint's callbacks for the call (`mediaRenegotiation`, `transfer`, …). The web client sends
/// it only when the acceptance's delivery response failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcknowledgeAcceptance(Built);

impl AcknowledgeAcceptance {
    /// The acknowledgement at `link`, handing out `links`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't an acknowledgement in its captured shape.
    pub fn new(context: &CallContext, link: &CallLink, links: &Links) -> Result<Self, CallWriteError> {
        let body = json!({"callAcceptanceAcknowledgement": {"links": links.json()}});
        Ok(Self(operation(context, CallOperation::Acknowledge, link, Some(body))?))
    }
}

/// A media line's send capabilities: `POST` to the call controller's `applyChannelParameters` link (`mcProxy`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyChannelParameters(Built);

impl ApplyChannelParameters {
    /// `parameters` at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't channel parameters in their captured shape.
    pub fn new(context: &CallContext, link: &CallLink, parameters: &ChannelParameters) -> Result<Self, CallWriteError> {
        let body = json!({"applyChannelParameters": parameters.json()});
        Ok(Self(operation(
            context,
            CallOperation::ApplyChannelParameters,
            link,
            Some(body),
        )?))
    }
}

/// The sender's endpoint metadata in the call's conversation: `PUT` to the `updateEndpointMetadata` link (the start's
/// answer's for the caller, the conversation subscription's for the callee).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateEndpointMetadata(Built);

impl UpdateEndpointMetadata {
    /// `from`'s metadata at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't endpoint metadata in its captured shape.
    pub fn new(context: &CallContext, link: &CallLink, from: &CallParticipant) -> Result<Self, CallWriteError> {
        let body = json!({"participants": {"from": from.json()}, "endpointMetadata": {}});
        Ok(Self(operation(
            context,
            CallOperation::UpdateEndpointMetadata,
            link,
            Some(body),
        )?))
    }
}

/// The sender's own participant properties in the call's conversation (its AI voice consent, or that it shares its
/// screen): `POST` to the conversation subscription's `updateParticipantProperties` link, with the callback for the
/// update's status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateParticipantProperties(Built);

/// The sender's AI voice consent as the call's conversation takes it: the value (`"0"` in every capture) and the
/// sender's own sequence number for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceConsent {
    pub value: String,
    pub sequence: u64,
}

impl UpdateParticipantProperties {
    /// `from`'s `consent` at `link`, its status called back at `status`, the update numbered `operation_id`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't participant properties in their captured shape.
    pub fn new(
        context: &CallContext,
        link: &CallLink,
        from: &CallParticipant,
        consent: &VoiceConsent,
        status: &str,
        operation_id: &str,
    ) -> Result<Self, CallWriteError> {
        let property = json!({"aiVoiceConsent": {
            "sequenceNumber": consent.sequence,
            "value": {"aiVoiceConsentValue": consent.value},
        }});
        Self::with(context, link, from, &property, (status, operation_id))
    }

    /// `from` sharing its screen (`screenSharerAttributes`, its `sequence`-th, `{"source": 1}` as the web client's
    /// Share sends it before its renegotiation adds the sharing line, seen 2026-11-06) at `link`, its status called
    /// back at `status`, the update numbered `operation_id`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't participant properties in their captured shape.
    pub fn screen_sharer(
        context: &CallContext,
        link: &CallLink,
        from: &CallParticipant,
        sequence: u64,
        status: &str,
        operation_id: &str,
    ) -> Result<Self, CallWriteError> {
        let property = json!({"screenSharerAttributes": {
            "sequenceNumber": sequence,
            "value": {"source": 1},
        }});
        Self::with(context, link, from, &property, (status, operation_id))
    }

    fn with(
        context: &CallContext,
        link: &CallLink,
        from: &CallParticipant,
        property: &Value,
        (status, operation_id): (&str, &str),
    ) -> Result<Self, CallWriteError> {
        let body = json!({
            "from": from.short_json(),
            "to": [{"id": from.mri.as_str(), "propertyBag": property}],
            "links": {"updateParticipantPropertiesStatus": status},
            "operationId": operation_id,
        });
        Ok(Self(operation(
            context,
            CallOperation::UpdateParticipantProperties,
            link,
            Some(body),
        )?))
    }
}

/// Why the sender leaves its call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaveReason {
    /// It hangs up a call that connected (the call's end `0`).
    HungUp,
    /// The caller cancels its call while it still rings, after `rang_seconds` (the call's end `487`, with how long it
    /// rang in its call quality diagnostics).
    Cancelled { rang_seconds: u64 },
}

/// The sender leaves its call: `POST` to the conversation's `leave` link (the start's answer's for the caller, the
/// conversation subscription's for the callee), the conversation's and the call's end as the web client sends them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaveCall(Built);

impl LeaveCall {
    /// `from` leaves at `link`, for `reason`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a `leave` in its captured shape.
    pub fn new(
        context: &CallContext,
        link: &CallLink,
        from: &CallParticipant,
        reason: LeaveReason,
    ) -> Result<Self, CallWriteError> {
        let mut call_end = json!({
            "code": 0,
            "subCode": 0,
            "phrase": "CallEndReasonLocalUserInitiated",
            "resultCategories": ["Success"],
        });
        if let (LeaveReason::Cancelled { rang_seconds }, Some(end)) = (reason, call_end.as_object_mut()) {
            end.insert("code".to_owned(), json!(487));
            end.insert(
                "callQualityDiagnosticsInformation".to_owned(),
                json!({"cancelationDuration": rang_seconds}),
            );
        }
        let body = json!({
            "participants": {"from": from.json()},
            "conversationTransactionEnd": {"reason": "noError", "code": 0, "phrase": "ConversationEndNoModalityConnected"},
            "callTransactionEnd": call_end,
        });
        Ok(Self(operation(context, CallOperation::Leave, link, Some(body))?))
    }
}

/// A media renegotiation of the sender's call (its camera turned on, say): `POST` to the call controller's
/// `mediaRenegotiation` link, the sender's new offer and its callbacks for the answer and the rejection
/// (`mediaAnswer`, `rejection`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenegotiateMedia(Built);

impl RenegotiateMedia {
    /// `sender`'s renegotiation at `link`: the call's `modalities`, the callbacks `links`, the offer `media`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a renegotiation in its captured shape.
    pub fn new(
        context: &CallContext,
        link: &CallLink,
        sender: &CallParticipant,
        modalities: &[String],
        links: &Links,
        media: &MediaContent,
    ) -> Result<Self, CallWriteError> {
        let body = json!({"mediaNegotiation": {
            "callModalities": modalities,
            "sender": sender.json(),
            "links": links.json(),
            "mediaContent": media.json(),
        }});
        Ok(Self(operation(context, CallOperation::Renegotiate, link, Some(body))?))
    }
}

/// The sender's answer to a renegotiation of its call: `POST` to the `mediaAnswer` link the renegotiation delivered,
/// its callbacks for the media acknowledgement (`links`) and the media controller, and its answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnswerMedia(Built);

/// What an answer to a renegotiation sends beside its link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnswerMediaParts {
    pub sender: CallParticipant,
    pub modalities: Vec<String>,
    pub links: Links,
    pub media_controller_links: Links,
    pub media: MediaContent,
}

impl AnswerMedia {
    /// The answer at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a renegotiation's answer in its captured shape.
    pub fn new(context: &CallContext, link: &CallLink, parts: &AnswerMediaParts) -> Result<Self, CallWriteError> {
        let mut answer = Map::new();
        answer.insert("callModalities".to_owned(), json!(parts.modalities));
        answer.insert("sender".to_owned(), parts.sender.json());
        answer.insert("links".to_owned(), parts.links.json());
        if !parts.media_controller_links.is_empty() {
            answer.insert(
                "clientContentForMediaController".to_owned(),
                parts.media_controller_links.json(),
            );
        }
        answer.insert("mediaContent".to_owned(), parts.media.json());
        let body = json!({"mediaAnswer": Value::Object(answer)});
        Ok(Self(operation(context, CallOperation::MediaAnswer, link, Some(body))?))
    }
}

/// The answer to the sender's renegotiation acknowledged: `POST` with no body to the `mediaAcknowledgement` link that
/// answer handed out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcknowledgeMediaAnswer(Built);

impl AcknowledgeMediaAnswer {
    /// The acknowledgement at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the link isn't a media answer's acknowledgement.
    pub fn new(context: &CallContext, link: &CallLink) -> Result<Self, CallWriteError> {
        Ok(Self(operation(context, CallOperation::MediaAcknowledge, link, None)?))
    }
}

/// Which media lines the sender sends and receives (its camera on: its main video line `sendrecv`): `POST` to the call
/// controller's `updateMediaDescriptions` link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateMediaDescriptions(Built);

impl UpdateMediaDescriptions {
    /// `descriptions` at `link`, tagged `negotiation_tag` when given.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't media descriptions in their captured shape.
    pub fn new(
        context: &CallContext,
        link: &CallLink,
        descriptions: &MediaDescriptions,
        negotiation_tag: Option<&str>,
    ) -> Result<Self, CallWriteError> {
        let mut described = descriptions.json();
        if let (Some(tag), Some(object)) = (negotiation_tag, described.as_object_mut()) {
            object.insert("negotiationTag".to_owned(), json!(tag));
        }
        let body = json!({"UpdateMediaDescriptions": {"mediaDescriptions": described}});
        Ok(Self(operation(
            context,
            CallOperation::UpdateMediaDescriptions,
            link,
            Some(body),
        )?))
    }
}

/// The call's keep-alive: `POST` of an empty participant update (`{"callParticipantUpdate": {}}`) to the call leg's
/// own `callLeg` link (the caller's acceptance's, the callee's acceptance's acknowledgement's), sent every 90 % of the
/// call's `callKeepAliveInterval` as the web client does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeepAliveCall(Built);

impl KeepAliveCall {
    /// The keep-alive at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the link isn't a call leg's own.
    pub fn new(context: &CallContext, link: &CallLink) -> Result<Self, CallWriteError> {
        let body = json!({"callParticipantUpdate": {}});
        Ok(Self(operation(context, CallOperation::KeepAlive, link, Some(body))?))
    }
}

/// Someone waiting in a meeting's lobby let in, or turned away: `POST` to the meeting conversation's `admit` link (the
/// organizer's Admit) or its `removeParticipant` link (its Deny), the sender, the one person it names and the
/// callbacks for how it went (`admitSuccess` and `admitFailure`, `removeParticipantSuccess` and
/// `removeParticipantFailure`), as the web client sends them (seen 2026-11-03, `meeting-steps`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecideLobby(Built);

impl DecideLobby {
    /// `from` admits `waiting` at `link` (the conversation's `admit` link), its callbacks `links`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't an admit in its captured shape.
    pub fn admit(
        context: &CallContext,
        link: &CallLink,
        (from, waiting): (&CallParticipant, &Mri),
        links: &Links,
    ) -> Result<Self, CallWriteError> {
        Ok(Self(operation(
            context,
            CallOperation::Admit,
            link,
            Some(lobby_body(from, waiting, links)),
        )?))
    }

    /// `from` turns `waiting` away at `link` (the conversation's `removeParticipant` link), its callbacks `links`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a removal in its captured shape.
    pub fn deny(
        context: &CallContext,
        link: &CallLink,
        (from, waiting): (&CallParticipant, &Mri),
        links: &Links,
    ) -> Result<Self, CallWriteError> {
        Ok(Self(operation(
            context,
            CallOperation::RemoveParticipant,
            link,
            Some(lobby_body(from, waiting, links)),
        )?))
    }
}

/// An admit's or a removal's body: the sender, the one person it names, the callbacks for how it went.
fn lobby_body(from: &CallParticipant, waiting: &Mri, links: &Links) -> Value {
    json!({"participants": {"from": from.json(), "to": [{"id": waiting.as_str()}]}, "links": links.json()})
}

/// The sender's raised hand, published in the call's conversation: `POST` to its `publishState` link
/// (`{"from", "publishedState": {"content": {"skinTone": 0}, "level": "user", "stateType": "raiseHands",
/// "sequenceNumber"}}`, seen 2026-11-06). Its answer names the state's id (`publishStateResponse.stateId`), which the
/// roster's entry of the sender lists too (`publishedStates`), and which [`RemoveState`] takes back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishState(Built);

impl PublishState {
    /// `from` raises its hand at `link`, the `sequence`th state the sender changed in the call.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a `publishState` in its captured shape.
    pub fn raise_hand(
        context: &CallContext,
        link: &CallLink,
        from: &CallParticipant,
        sequence: u64,
    ) -> Result<Self, CallWriteError> {
        let body = json!({
            "from": from.json(),
            "publishedState": {
                "content": {"skinTone": 0},
                "level": "user",
                "stateType": RAISE_HANDS,
                "sequenceNumber": sequence,
            },
        });
        Ok(Self(operation(context, CallOperation::PublishState, link, Some(body))?))
    }
}

/// The type of a raised hand's published state.
pub const RAISE_HANDS: &str = "raiseHands";

/// A state the sender published taken back (its hand lowered): `POST` to the conversation's `removeState` link
/// (`{"from", "sequenceNumber", "scope": "specified", "stateIds": [<id>]}`, seen 2026-11-06).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoveState(Built);

impl RemoveState {
    /// `from` takes back its state `state` at `link`, the `sequence`th state the sender changed in the call.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a `removeState` in its captured shape.
    pub fn new(
        context: &CallContext,
        link: &CallLink,
        from: &CallParticipant,
        state: &str,
        sequence: u64,
    ) -> Result<Self, CallWriteError> {
        let body = json!({
            "from": from.json(),
            "sequenceNumber": sequence,
            "scope": "specified",
            "stateIds": [state],
        });
        Ok(Self(operation(context, CallOperation::RemoveState, link, Some(body))?))
    }
}

/// The live reactions a call takes, as Teams' React offers them, by the name the wire gives each.
pub const CALL_REACTIONS: [&str; 5] = ["like", "heart", "applause", "laugh", "surprised"];

/// A live reaction to everyone in the call: `POST` to the conversation's `sendMessage` link (`{"from",
/// "messageContent": [{"operationId", "payload": {"values": [{"id", "name", "count": 1, "attributes": {"skinTone":
/// 0}}]}, "scope": "all", "to": [], "type": "Reaction"}]}`, seen 2026-11-06). Everyone in the call gets it at their
/// conversation's `receiveMessage` callback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendCallReaction(Built);

impl SendCallReaction {
    /// `from` sends `reaction` (one of [`CALL_REACTIONS`]) at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] for a reaction the call doesn't take, or when the request isn't a `sendMessage` in its
    /// captured shape.
    pub fn new(
        context: &CallContext,
        link: &CallLink,
        from: &CallParticipant,
        reaction: &str,
    ) -> Result<Self, CallWriteError> {
        if !CALL_REACTIONS.contains(&reaction) {
            return Err(CallWriteError::Unmapped);
        }
        let (operation_id, value_id) = (
            parts::guid().map_err(CallWriteError::Random)?,
            parts::guid().map_err(CallWriteError::Random)?,
        );
        let body = json!({
            "from": from.json(),
            "messageContent": [{
                "operationId": operation_id,
                "payload": {"values": [{
                    "id": value_id,
                    "name": reaction,
                    "count": 1,
                    "attributes": {"skinTone": 0},
                }]},
                "scope": "all",
                "to": [],
                "type": "Reaction",
            }],
        });
        Ok(Self(operation(context, CallOperation::SendMessage, link, Some(body))?))
    }
}

/// The organizer ends the meeting for everyone in it: `DELETE` at the conversation's own URL (its
/// `conversationController`), with an end for all (`ConversationEndForAllInitiated`, seen 2026-11-06). Everyone in it
/// gets the conversation's end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndConversation(Built);

impl EndConversation {
    /// `from` ends the conversation at `link` for everyone.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't the conversation's end in its captured shape.
    pub fn new(context: &CallContext, link: &CallLink, from: &CallParticipant) -> Result<Self, CallWriteError> {
        let body = json!({
            "participants": {"from": from.json()},
            "conversationTransactionEnd": {"reason": "noError", "code": 0, "phrase": "ConversationEndForAllInitiated"},
            "callTransactionEnd": {
                "code": 0,
                "subCode": 0,
                "phrase": "CallEndReasonLocalUserInitiated",
                "resultCategories": ["Success"],
            },
        });
        Ok(Self(operation(
            context,
            CallOperation::EndConversation,
            link,
            Some(body),
        )?))
    }
}

/// The hold of a 1:1 call: `POST` to the call leg's `hold` link (`{"hold": {"holdType": "musicOnHold", "links":
/// {"holdCompletion"}}, "debugContent": {"causeId"}}`, seen 2026-11-06). Its answer hands out where the hold is taken
/// back (`{"holdResponse": {"links": {"resume"}}}`, [`ResumeCall`]); the call controller then tells the hold done at
/// the `holdCompletion` callback, and the far end hears the service's music.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoldCall(Built);

impl HoldCall {
    /// The call put on hold at `link`, its completion told at the `holdCompletion` callback of `links`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a `hold` in its captured shape.
    pub fn new(context: &CallContext, link: &CallLink, links: &Links) -> Result<Self, CallWriteError> {
        let body = json!({
            "hold": {"holdType": "musicOnHold", "links": links.json()},
            "debugContent": {"causeId": parts::guid().map_err(CallWriteError::Random)?},
        });
        Ok(Self(operation(context, CallOperation::Hold, link, Some(body))?))
    }
}

/// A held call taken off hold: `POST` to the `resume` link the hold's answer handed out (`{"resume": {"links":
/// {"resumeCompletion"}}, "debugContent": {"causeId"}}`, seen 2026-11-06), answered `202`; its completion is told at the
/// `resumeCompletion` callback. The media goes again once the account renegotiates it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeCall(Built);

impl ResumeCall {
    /// The call held at `link` taken off hold, its completion told at the `resumeCompletion` callback of `links`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a `resume` in its captured shape.
    pub fn new(context: &CallContext, link: &CallLink, links: &Links) -> Result<Self, CallWriteError> {
        let body = json!({
            "resume": {"links": links.json()},
            "debugContent": {"causeId": parts::guid().map_err(CallWriteError::Random)?},
        });
        Ok(Self(operation(context, CallOperation::Resume, link, Some(body))?))
    }
}

/// A 1:1 call's far end transferred to someone else (a blind transfer): `POST` to the call leg's `transfer` link
/// `{"callTransfer": {"target": {"id"}, "transferor": {"details": {id, endpointId, participantId, languageId},
/// "authorizationToken": null}, "links": {"transferAcceptance", "transferCompletion"}, "transferContext":
/// {"customContext": {"callDetails": {}}}}}` (seen 2026-11-06, `ab-call --call steps`: Transfer to an audio call),
/// answered `202`. The far end is told at its own `transfer` callback and calls the target; the transferor is told
/// at `transferAcceptance` once it took it, and then leaves the call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferCall(Built);

impl TransferCall {
    /// `from`'s call transferred to `target` at `link`, its acceptance and completion told at the callbacks of `links`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a `transfer` in its captured shape: the target not a work account or
    /// the account itself, a link of another operation.
    pub fn new(
        context: &CallContext,
        link: &CallLink,
        from: &CallParticipant,
        target: &Mri,
        links: &Links,
    ) -> Result<Self, CallWriteError> {
        if !target.as_str().starts_with("8:orgid:")
            || *target == from.mri
            || links
                .iter()
                .any(|(name, _)| !["transferAcceptance", "transferCompletion"].contains(&name))
        {
            return Err(CallWriteError::Unmapped);
        }
        let mut details = from.json();
        if let Some(details) = details.as_object_mut() {
            details.remove("displayName");
        }
        let body = json!({
            "callTransfer": {
                "target": {"id": target.as_str()},
                "transferor": {"details": details, "authorizationToken": null},
                "links": links.json(),
                "transferContext": {"customContext": {"callDetails": {}}},
            },
        });
        Ok(Self(operation(context, CallOperation::Transfer, link, Some(body))?))
    }
}

/// The transfer the far end asked for, taken by the transferred party: `POST {"transferAcceptance": {}}` to the
/// `transferAcceptance` link its `callTransfer` delivery handed out (seen 2026-11-06), before it calls the target
/// ([`StartCall::transferred`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptTransfer(Built);

impl AcceptTransfer {
    /// The transfer taken at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when `link` isn't a transfer's acceptance link.
    pub fn new(context: &CallContext, link: &CallLink) -> Result<Self, CallWriteError> {
        let body = json!({"transferAcceptance": {}});
        Ok(Self(operation(
            context,
            CallOperation::TransferAcceptance,
            link,
            Some(body),
        )?))
    }
}

call_write!(TransferCall, AcceptTransfer);

/// Who transferred the account's call, as the `callTransfer` delivery names them: their details (`id`,
/// `displayName`, `endpointId`, `participantId`, tenant, region…, kept as delivered) and the authorization the call
/// controller minted for the transfer, which the transferred party's call to the target hands back to it
/// (`callInvitation.transferor`). Its `Debug` never shows the authorization.
#[derive(Clone, PartialEq, Eq)]
pub struct Transferor {
    details: Value,
    authorization: Option<String>,
}

impl Transferor {
    /// The transferor `details` as delivered, with the `authorization` the delivery carried.
    #[must_use]
    pub fn new(details: Value, authorization: Option<String>) -> Self {
        Self { details, authorization }
    }

    /// Who transferred the call.
    #[must_use]
    pub fn mri(&self) -> Option<Mri> {
        self.details.get("id").and_then(Value::as_str).map(Mri::new)
    }

    /// Their name as the delivery gave it.
    #[must_use]
    pub fn display_name(&self) -> Option<&str> {
        self.details
            .get("displayName")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
    }

    fn json(&self) -> Value {
        json!({"details": self.details, "authorizationToken": self.authorization})
    }
}

impl fmt::Debug for Transferor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transferor")
            .field("mri", &self.mri())
            .field("authorization", &self.authorization.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}
