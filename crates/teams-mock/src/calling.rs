//! The mock's calling service (M3 Task 13): the flight proxy's call requests (`/api/v2/…`) as the captures show them
//! (`docs/protocol/calling.md`, `docs/protocol/calling-echo.md`; every answer and delivery shaped from the synthetic
//! fixtures under `tests/fixtures/calling/`), and the call deliveries it sends the accounts' calling endpoints through
//! the mock's Trouter.
//!
//! - **Credential**: `X-Skypetoken` with the account's skypetoken ([`crate::ChatServiceMock::skypetoken`]); anything
//!   else is a 401 with no body.
//! - **The start** (`POST epconv`): its answer the conversation (`answers.json`'s shape). A 1:1 or group call rings
//!   every callee's registered calling endpoint with an invitation (`deliveries.json`'s 1:1 or group shape, the
//!   caller's offer in a 1:1 one); the test call and a group call are accepted at once by the media controller or the
//!   call's mixer (the caller's `acceptance` callback, `answers.json`'s acceptance).
//! - **The callee** (on an invitation's links): `attach` (answered with `follow_ups.json`'s attach answer for its leg),
//!   `progress`, the endpoint state, `accept` (answered with its acknowledgement; the caller's acceptance carries the
//!   callee's answer, and every other endpoint of the callee that attached gets the call's end: answered elsewhere),
//!   `reject` (in a 1:1 call the caller is forwarded to the callee's voicemail, which accepts, as captured; with the
//!   media on, the voicemail's peer answers the caller's offer).
//! - **The call**: the acceptance's acknowledgement, the endpoint metadata, `add` (the echo's success and its roster
//!   entry delivered), and `leave` (while it rings, a cancel: every attached callee endpoint gets the call's end with
//!   the leave's code; once up, the call's end to everyone in it). A join (`POST conv/<id>`) with media enters a running call and is accepted
//!   by its mixer; without media it subscribes only.
//! - **A group call's roster** ([`crate::roster`]): delivered to everyone in the call as parties come, leave, and turn
//!   their cameras on (`updateMediaDescriptions`); in a group call a `leave` takes only its party out, and the call
//!   ends once nobody is left. With group video on, each party's media is a mixer of its own ([`crate::mixer`]): a
//!   callee's invitation carries its mixer's offer.
//! - **Calls of no client** ([`crate::ChatServiceMock::ring`], [`crate::ChatServiceMock::ring_group`]): a caller the
//!   mock plays rings an account; [`crate::ChatServiceMock::end_ring`] ends a call's ring (the caller cancels, it
//!   times out) and [`crate::ChatServiceMock::hang_up_call`] ends a call that is up.
//!
//! **Not captured, so labelled as the mock's own**: the bodies of the ends a callee's endpoint gets for a ring that
//! was cancelled, answered elsewhere or timed out, and for a call another party left (the echo call's `call/end`
//! envelope, a code and a phrase of the mock's: [`ANSWERED_ELSEWHERE`], [`TIMED_OUT`]); the answers of `progress`,
//! the endpoint state and metadata, `add`, the acknowledgement and `leave` (200 with no body).

use serde_json::{Value, json};
use wiremock::{Request, ResponseTemplate};

use crate::chatsvc::status;
use crate::roster::{self, Party};
use crate::trouter::{CALLING_APP, fill_guids, invitation};
use crate::world::State;

const ECHO_ANSWERS: &str = include_str!("../../../tests/fixtures/calling/echo/answers.json");
const ECHO_DELIVERIES: &str = include_str!("../../../tests/fixtures/calling/echo/deliveries.json");
const CHAT_DELIVERIES: &str = include_str!("../../../tests/fixtures/calling/chat/deliveries.json");
const FOLLOW_UPS: &str = include_str!("../../../tests/fixtures/calling/chat/follow_ups.json");

/// The flight proxy's links to the call's services.
const PROXY: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep";
/// The conversation service's host behind it.
const CONV_HOST: &str = "conv-zz-01-prod-aks.conv.skype.com";
/// The call controller's host behind it.
const CC_HOST: &str = "cc-zz-01-prod-aks.cc.skype.com";
/// The routing query of a conversation's links and of a call controller's.
const CONV_QUERY: &str = "?i=10-0-0-2&e=1";
const CC_QUERY: &str = "?i=10-0-0-1";

/// The phrase of the end the mock sends an endpoint whose ring another endpoint of its account answered (not captured).
pub const ANSWERED_ELSEWHERE: &str = "MockAnsweredElsewhere";
/// The phrase of the end the mock sends a ring that timed out ([`crate::ChatServiceMock::end_ring`]'s default; not
/// captured for a callee).
pub const TIMED_OUT: &str = "MockRingTimedOut";

/// A delivery waiting for the calling endpoint `endpoint` (its id in its push URL): at `path` under its push URL.
#[derive(Clone, Debug)]
pub(crate) struct Outgoing {
    pub(crate) endpoint: String,
    pub(crate) path: String,
    pub(crate) body: Value,
}

/// Where a call is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallPhase {
    /// Its callees ring.
    Ringing,
    /// Someone answered (or the media controller, the mixer, a voicemail accepted).
    Connected,
    Ended,
}

/// A callee's leg of a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leg {
    pub callee: String,
    /// Each endpoint of the callee that attached, with its `call/end` callback.
    pub attached: Vec<(String, String)>,
    /// The endpoints that said they ring (`progress`).
    pub ringing: Vec<String>,
    /// The endpoint that answered it.
    pub accepted_by: Option<String>,
    pub declined: bool,
    /// The `mediaRenegotiation` callback its answer handed out, where the call's renegotiation goes.
    renegotiation: Option<String>,
    /// The `transfer` callback its answer handed out, where a transfer of its callee is delivered.
    transfer: Option<String>,
    /// The `rosterUpdate` callback its `attach` handed out.
    roster: Option<String>,
    /// The `receiveMessage` callback its `attach` handed out.
    messages: Option<String>,
    /// The `conversationUpdate` callback its `attach` handed out.
    updates: Option<String>,
}

/// A call as the mock keeps it, for a test to read ([`crate::ChatServiceMock::calls`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MockCall {
    pub id: String,
    pub conversation: String,
    pub caller: String,
    pub group_chat: Option<String>,
    pub test_call: bool,
    pub phase: CallPhase,
    pub legs: Vec<Leg>,
    /// Every request on the call, by its operation (`start`, `attach`, `progress`, `accept`, `reject`, `leave`, …), with
    /// the account that sent it, in order.
    pub requests: Vec<(String, String)>,
    /// The tenant each `add` named for its invitation (`participantInvitationData.invitationData.meetingInfo.tenantId`,
    /// empty when it named none), in order.
    pub add_tenants: Vec<String>,
    /// Each person an `add` brought into the call (a work account, not a service), with the group chat it named, in
    /// order.
    pub added: Vec<(String, String)>,
    /// The call's caller transferred its far end: whom to, and the transferor's `transferAcceptance` callback.
    pub transferred: Option<(String, Option<String>)>,
    /// The `callInvitation.transferor` its start named: a transferred party's call to the transfer's target.
    pub transferor: Option<Value>,
    /// The caller's callbacks (none when the mock plays the caller).
    caller_links: Option<Callbacks>,
    /// A renegotiation forwarded from one client of the call to another, waiting for its answer: who renegotiated and
    /// the `mediaAnswer` callback its answer goes to.
    negotiation: Option<(String, String)>,
    /// Those who joined the running call, with their callbacks.
    joined: Vec<(String, Callbacks)>,
    offer: Option<Value>,
    /// A group call's roster: everyone who was in it.
    pub(crate) roster: Vec<Party>,
    pub(crate) roster_version: u64,
    /// The `rosterUpdate` callbacks of the callees' `attach`es: each told the roster when it attached, then every
    /// change while the call runs (whether its callee answered, or let the ring end and joined later).
    pub(crate) roster_watchers: Vec<String>,
    /// The meeting timer's sequence as last delivered (each change counts the next).
    pub(crate) timer_sequence: u64,
    /// Its meeting options as its organizer's Controls set them, by name (`crate::meeting_options`).
    pub(crate) options: serde_json::Map<String, Value>,
}

/// A party's callbacks the mock calls back: its acceptance, the call's end and progress.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Callbacks {
    acceptance: Option<String>,
    end: Option<String>,
    progress: Option<String>,
    renegotiation: Option<String>,
}

impl Callbacks {
    fn of(body: &Value) -> Self {
        let at = |name: &str| {
            body.pointer(&format!("/callInvitation/links/{name}"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        Self {
            acceptance: at("acceptance"),
            end: at("end"),
            progress: at("progress"),
            renegotiation: None,
        }
    }
}

/// The calling service's state in the mock's world.
#[derive(Debug, Default)]
pub(crate) struct Calling {
    pub(crate) calls: Vec<MockCall>,
    next: u64,
    /// Deliveries waiting for their endpoint's calling socket.
    pub(crate) outgoing: Vec<Outgoing>,
    /// The next acceptance the media controller or a mixer delivers names another call in its links
    /// ([`crate::ChatServiceMock::unbind_next_acceptance`]).
    pub(crate) unbind_next_acceptance: bool,
    /// The next call operation whose path ends with this segment is answered this much later, the world changed at
    /// once ([`crate::ChatServiceMock::delay_next_call_op`]).
    pub(crate) delay_next: Option<(String, std::time::Duration)>,
}

/// The skypetoken the mock takes as `mri`'s.
pub(crate) fn skypetoken_of(mri: &str) -> String {
    format!("mock-skypetoken-{}", mri.rsplit(':').next().unwrap_or_default())
}

fn conversation_url(conversation: &str, op: Option<&str>) -> String {
    let op = op.map(|op| format!("/{op}")).unwrap_or_default();
    format!("{PROXY}/{CONV_HOST}/conv/{conversation}{op}{CONV_QUERY}")
}

fn cc_url(path: &str) -> String {
    format!("{PROXY}/{CC_HOST}/cc/v1/{path}{CC_QUERY}")
}

/// A fixture's JSON with each `(from, to)` replaced, every other `<guid:…>` a made-up GUID.
fn fixture(text: &str, replace: &[(&str, &str)]) -> Value {
    let text = replace
        .iter()
        .fold(text.to_owned(), |text, (from, to)| text.replace(from, to));
    serde_json::from_str(&fill_guids(&text)).unwrap_or_default()
}

fn object_id(mri: &str) -> &str {
    mri.rsplit(':').next().unwrap_or_default()
}

/// A callback URL's endpoint and path under its push URL.
fn target(url: &str) -> Option<(String, String)> {
    let rest = &url[url.find("/v4/f/")? + "/v4/f/".len()..];
    let (endpoint, path) = rest.split_once('/')?;
    Some((endpoint.to_owned(), path.to_owned()))
}

impl State {
    /// The account whose skypetoken `request` carries.
    fn caller_of(&self, request: &Request) -> Option<String> {
        let token = request.headers.get("x-skypetoken")?.to_str().ok()?;
        self.accounts
            .iter()
            .find(|account| {
                account
                    .skypetoken
                    .as_deref()
                    .map_or_else(|| skypetoken_of(&account.mri) == token, |value| value == token)
            })
            .map(|account| account.mri.clone())
    }

    /// The calling endpoints `mri` registered (their ids in their push URLs).
    fn calling_endpoints(&self, mri: &str) -> Vec<String> {
        let mut endpoints: Vec<String> = self
            .registrations
            .iter()
            .filter(|registration| registration.account == mri && registration.app_id == CALLING_APP)
            .filter_map(|registration| target(&registration.path).map(|(endpoint, _)| endpoint))
            .collect();
        endpoints.dedup();
        endpoints
    }

    /// Queues `body` for the callback `url`.
    pub(crate) fn call_back(&mut self, url: Option<&str>, body: Value) {
        if let Some((endpoint, path)) = url.and_then(target) {
            self.calling.outgoing.push(Outgoing { endpoint, path, body });
            self.changed();
        }
    }

    pub(crate) fn display_name(&self, mri: &str) -> String {
        self.account(mri)
            .map(|account| account.display_name.clone())
            .unwrap_or_default()
    }
}

/// The TURN relay's credentials (`GET /trap/tokens`, `answers.json`'s shape) for an account's token: the loopback TURN
/// server's once a test turned the media on, made-up ones otherwise; `None` for any other path.
pub(crate) fn relay_tokens(state: &State, request: &Request) -> Option<ResponseTemplate> {
    if request.url.path() != "/trap/tokens" {
        return None;
    }
    let token = request
        .headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if token.and_then(|token| state.account_by_token(token)).is_none() {
        return Some(status(401));
    }
    if request.method.as_str() != "GET" {
        return Some(status(404));
    }
    let answers: Value = serde_json::from_str(ECHO_ANSWERS).unwrap_or_default();
    let (user, password) = state.relay_credentials();
    let tokens = fixture(
        &answers["trap_tokens"].to_string(),
        &[("<token:1>", user), ("<token:2>", password)],
    );
    Some(json_answer(state, 200, &tokens))
}

/// The calling service's answer to `request`, if its path is under `/api/v2/`.
pub(crate) fn serve(state: &mut State, request: &Request) -> Option<ResponseTemplate> {
    let prefix = if state.cloud.is_microsoft() {
        "/api/v2/"
    } else {
        "/flightproxy/api/v2/"
    };
    let path = request.url.path().strip_prefix(prefix)?;
    let Some(account) = state.caller_of(request) else {
        return Some(status(401));
    };
    let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
    let segments: Vec<&str> = path.split('/').collect();
    let method = request.method.as_str();
    let answer = match (method, segments.as_slice()) {
        ("POST", ["epconv"]) => start(state, &account, &body),
        ("POST", ["ep", CONV_HOST, "conv", conversation]) => join(state, &account, conversation, &body),
        ("DELETE", ["ep", CONV_HOST, "conv", conversation]) => end_for_all(state, &account, conversation, &body),
        (_, ["ep", CONV_HOST, "conv", conversation, op]) => conversation_op(state, &account, conversation, op, &body),
        (_, ["ep", CC_HOST, "cc", "v1", kind, call, leg, _, _, rest @ ..]) => {
            controller_op(state, &account, (kind, call, leg), rest.first().copied(), &body)
        }
        _ => None,
    };
    let answer = answer.unwrap_or_else(|| status(404));
    let delay = state
        .calling
        .delay_next
        .take_if(|(segment, _)| segments.last() == Some(&segment.as_str()));
    Some(match delay {
        Some((_, delay)) => answer.set_delay(delay),
        None => answer,
    })
}

/// A call start: the conversation answered, the callees rung (see the module docs).
fn start(state: &mut State, caller: &str, body: &Value) -> Option<ResponseTemplate> {
    let callees: Vec<String> = body
        .pointer("/participants/to")?
        .as_array()?
        .iter()
        .filter_map(|to| to.get("id")?.as_str().map(str::to_owned))
        .collect();
    let test_call =
        body.pointer("/conversationRequest/scenario").and_then(Value::as_str) == Some("UserInitiatedTestCall");
    let group_chat = body
        .pointer("/groupChat/threadId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let offer = body.pointer("/callInvitation/mediaContent").cloned();
    let index = new_call(state, caller, &callees, group_chat, Some(Callbacks::of(body)), offer);
    state.calling.calls[index].transferor = body
        .pointer("/callInvitation/transferor")
        .filter(|transferor| !transferor.is_null())
        .cloned();
    log(state, index, caller, "start");
    let call = state.calling.calls[index].clone();
    ring_callees(state, index);
    if test_call || call.group_chat.is_some() {
        state.calling.calls[index].test_call = test_call;
        let answer = call
            .offer
            .as_ref()
            .and_then(|offer| state.peer_answer(&call.id, caller, offer));
        accept_by_controller(
            state,
            index,
            call.caller_links.as_ref().and_then(|links| links.acceptance.as_deref()),
            answer,
        );
    }
    roster::enter(state, index, caller, roster::link_of(body));
    roster::listen(state, index, caller, roster::named(body, "receiveMessage").as_deref());
    roster::updates_at(
        state,
        index,
        caller,
        roster::named(body, "conversationUpdate").as_deref(),
    );
    Some(json_answer(state, 200, &conversation_answer(&call.conversation)))
}

/// A new call of `caller` to `callees`; its index.
fn new_call(
    state: &mut State,
    caller: &str,
    callees: &[String],
    group_chat: Option<String>,
    caller_links: Option<Callbacks>,
    offer: Option<Value>,
) -> usize {
    state.calling.next += 1;
    let n = state.calling.next;
    state.calling.calls.push(MockCall {
        id: format!("MockCall{n:04}"),
        conversation: format!("MockConversation{n:04}"),
        caller: caller.to_owned(),
        group_chat,
        test_call: false,
        phase: CallPhase::Ringing,
        legs: callees
            .iter()
            .map(|callee| Leg {
                callee: callee.clone(),
                attached: Vec::new(),
                ringing: Vec::new(),
                accepted_by: None,
                declined: false,
                renegotiation: None,
                transfer: None,
                roster: None,
                messages: None,
                updates: None,
            })
            .collect(),
        requests: Vec::new(),
        add_tenants: Vec::new(),
        caller_links,
        negotiation: None,
        joined: Vec::new(),
        offer,
        added: Vec::new(),
        transferred: None,
        transferor: None,
        roster: Vec::new(),
        roster_version: 0,
        roster_watchers: Vec::new(),
        timer_sequence: 0,
        options: crate::meeting_options::defaults(),
    });
    state.calling.calls.len() - 1
}

fn log(state: &mut State, index: usize, account: &str, op: &str) {
    if let Some(call) = state.calling.calls.get_mut(index) {
        call.requests.push((op.to_owned(), account.to_owned()));
    }
}

/// Now, as the service writes its edit times (`2026-11-06T09:06:07.0044889Z`, to the millisecond here).
pub(crate) fn now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // the civil date of a day count since 1970-01-01 (Howard Hinnant's days_from_civil, inverted)
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60,
        now.subsec_millis()
    )
}

/// The conversation a start or a join opens (`answers.json`'s call start answer, on `conversation`), with the links a
/// meeting's conversation hands out besides (seen 2026-11-06): its endpoint state, its published states, its
/// messages and the participants' own properties.
fn conversation_answer(conversation: &str) -> Value {
    let answers: Value = serde_json::from_str(ECHO_ANSWERS).unwrap_or_default();
    let text = answers["call_start_answer"].to_string();
    let mut answer = fixture(
        &text,
        &[("SyntheticConversation0", conversation), ("?i=0-0-0-0&e=1", CONV_QUERY)],
    );
    for op in [
        "updateEndpointState",
        "publishState",
        "removeState",
        "sendMessage",
        "updateMeetingStates",
        "updateMeetingSettings",
        "updateParticipantProperties",
        "mute",
    ] {
        answer["links"][op] = json!(conversation_url(conversation, Some(op)));
    }
    answer
}

/// Rings every callee of call `index` on each calling endpoint it registered (a group call's with its callee's mixer's
/// offer, when its parties have mixers).
fn ring_callees(state: &mut State, index: usize) {
    let call = state.calling.calls[index].clone();
    for (leg_number, leg) in (1_u64..).zip(&call.legs) {
        let mut body = invitation_of(state, &call, leg_number, &leg.callee);
        if call.group_chat.is_some()
            && let Some(offer) = state.peer_offer(&call.id, &leg.callee)
        {
            body = with_offer(&body, offer);
        }
        for endpoint in state.calling_endpoints(&leg.callee) {
            state.calling.outgoing.push(Outgoing {
                endpoint,
                path: String::new(),
                body: body.clone(),
            });
        }
    }
    state.changed();
}

/// `invitation` (`{"evt", "gp": <base64 JSON>}`) with `offer` as its `callNotification.mediaContent`.
fn with_offer(invitation: &Value, offer: Value) -> Value {
    use base64::Engine as _;
    let engine = base64::engine::general_purpose::STANDARD;
    let Some(mut gp) = invitation
        .get("gp")
        .and_then(Value::as_str)
        .and_then(|gp| engine.decode(gp).ok())
        .and_then(|gp| serde_json::from_slice::<Value>(&gp).ok())
    else {
        return invitation.clone();
    };
    gp["callNotification"]["mediaContent"] = offer;
    let mut invitation = invitation.clone();
    invitation["gp"] = json!(engine.encode(gp.to_string()));
    invitation
}

/// The invitation of `call`'s leg `leg` to `callee` (`deliveries.json`'s 1:1 or group shape).
fn invitation_of(state: &State, call: &MockCall, leg: u64, callee: &str) -> Value {
    let forked = |op: &str| cc_url(&format!("forked/{}/{leg}/i1/{leg}/{op}", call.id));
    let caller_name = state.display_name(&call.caller);
    let index = usize::from(call.group_chat.is_some());
    invitation(index, &call.caller, callee, |gp| {
        let notification = &mut gp["callNotification"];
        for op in ["attach", "progress", "reject"] {
            notification["links"][op] = json!(forked(op));
        }
        notification["from"]["displayName"] = json!(caller_name);
        if let (0, Some(offer)) = (index, &call.offer) {
            notification["mediaContent"] = offer.clone();
        }
        gp["conversationInvitation"]["conversationController"] = json!(conversation_url(&call.conversation, None));
        if let Some(chat) = &call.group_chat {
            gp["groupChat"]["threadId"] = json!(chat);
            gp["conversationInvitation"]["groupCallInitiator"] = json!(call.caller);
        }
    })
}

/// The media controller (the test call) or the call's mixer (a group call, a join) accepts the leg whose acceptance
/// callback is `acceptance` (`answers.json`'s acceptance, the controller's answer; `answer` in its place when a peer
/// answered). Its acknowledgement link names another call than its other links when a test asked for that
/// ([`Calling::unbind_next_acceptance`]): an acceptance the tracker binds nothing of.
fn accept_by_controller(state: &mut State, index: usize, acceptance: Option<&str>, answer: Option<Value>) {
    let call = &mut state.calling.calls[index];
    call.phase = CallPhase::Connected;
    let id = call.id.clone();
    let unbound = state.calling.unbind_next_acceptance;
    state.calling.unbind_next_acceptance = false;
    let answers: Value = serde_json::from_str(ECHO_ANSWERS).unwrap_or_default();
    let mut body = fixture(
        &answers["acceptance_callback"]["body"].to_string(),
        &[("<guid:5>", &id)],
    );
    if unbound && let Some(link) = body.pointer_mut("/callAcceptance/links/acknowledgement") {
        let other = link.as_str().unwrap_or_default().replace(&id, &format!("{id}Other"));
        *link = Value::String(other);
    }
    if let Some(answer) = answer {
        body["callAcceptance"]["mediaContent"] = answer;
    }
    body["callAcceptance"]["links"]["hold"] = json!(hold_url(&id));
    body["callAcceptance"]["links"]["transfer"] = json!(transfer_url(&id));
    if call_is_group(state, index) {
        // a group call's acceptance hands out where its lines are described again (as captured; the echo call's
        // fixture names none)
        body["callAcceptance"]["links"]["updateMediaDescriptions"] =
            json!(cc_url(&format!("active/{id}/1/a1/1/updateMediaDescriptions")));
    }
    state.call_back(acceptance, body);
}

fn call_is_group(state: &State, index: usize) -> bool {
    state
        .calling
        .calls
        .get(index)
        .is_some_and(|call| call.group_chat.is_some())
}

/// A join of the running call on `conversation`: with media it enters the call and the mixer accepts it; without, it
/// subscribes to the conversation only.
fn join(state: &mut State, account: &str, conversation: &str, body: &Value) -> Option<ResponseTemplate> {
    let index = state
        .calling
        .calls
        .iter()
        .position(|call| call.conversation == conversation && call.phase != CallPhase::Ended)?;
    log(state, index, account, "join");
    if body.get("callInvitation").is_some() {
        let links = Callbacks::of(body);
        let acceptance = links.acceptance.clone();
        state.calling.calls[index].joined.push((account.to_owned(), links));
        let id = state.calling.calls[index].id.clone();
        let answer = body
            .pointer("/callInvitation/mediaContent")
            .and_then(|offer| state.peer_answer(&id, account, offer));
        accept_by_controller(state, index, acceptance.as_deref(), answer);
        roster::enter(state, index, account, roster::link_of(body));
        roster::listen(state, index, account, roster::named(body, "receiveMessage").as_deref());
        roster::updates_at(
            state,
            index,
            account,
            roster::named(body, "conversationUpdate").as_deref(),
        );
    }
    Some(json_answer(state, 200, &conversation_answer(conversation)))
}

/// An operation on a conversation (`leave`, `add`, the endpoint's state and metadata, its participant properties).
fn conversation_op(
    state: &mut State,
    account: &str,
    conversation: &str,
    op: &str,
    body: &Value,
) -> Option<ResponseTemplate> {
    let index = state
        .calling
        .calls
        .iter()
        .position(|call| call.conversation == conversation)?;
    log(state, index, account, op);
    match op {
        // a group call's party leaves alone: the call ends once nobody is left
        "leave" if call_is_group(state, index) && state.calling.calls[index].phase == CallPhase::Connected => {
            if !roster::leave(state, index, account) {
                leave(state, index, body);
            }
        }
        "leave" => leave(state, index, body),
        "add" if adds_person(body) => {
            add_person(state, index, body);
            return Some(status(202));
        }
        "add" => {
            let tenant = body
                .pointer("/participantInvitationData/invitationData/meetingInfo/tenantId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if let Some(call) = state.calling.calls.get_mut(index) {
                call.add_tenants.push(tenant);
            }
            let echo = body
                .pointer("/participants/to/0/id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let deliveries: Value = serde_json::from_str(ECHO_DELIVERIES).unwrap_or_default();
            let success = deliveries["cases"]
                .as_array()
                .and_then(|cases| {
                    cases.iter().find(|case| {
                        case["callback"]
                            .as_str()
                            .unwrap_or_default()
                            .contains("addParticipantSuccess")
                    })
                })
                .map(|case| fixture(&case["body"].to_string(), &[("<guid:echo>", object_id(echo))]))
                .unwrap_or_default();
            let link = body
                .pointer("/links/addParticipantSuccess")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let echo = echo.to_owned();
            state.call_back(link.as_deref(), success);
            // the echo is in the call: its roster says so (as captured)
            roster::enter(state, index, &echo, None);
        }
        "updateEndpointState" => {
            if let Some(muted) = body.pointer("/endpointState/state/isMuted").and_then(Value::as_bool) {
                roster::muted(state, index, account, muted);
            }
        }
        "publishState" => {
            let id = fill_guids("<guid:hand>");
            roster::hand(state, index, account, Some(&id));
            return Some(json_answer(
                state,
                200,
                &json!({"publishStateResponse": {"typeRank": 1, "stateId": id}}),
            ));
        }
        "removeState" => roster::hand(state, index, account, None),
        "sendMessage" => {
            roster::react(state, index, body);
            return Some(status(202));
        }
        "updateMeetingStates" | "updateMeetingSettings" | "mute" => {
            return Some(meeting_op(state, index, account, op, body));
        }
        "updateEndpointMetadata" | "updateParticipantProperties" => {}
        _ => return None,
    }
    Some(status(200))
}

/// A change of the meeting's timer, its lock (everyone told the conversation's update) or Mute all (everyone else
/// muted), answered `202` as captured.
fn meeting_op(state: &mut State, index: usize, account: &str, op: &str, body: &Value) -> ResponseTemplate {
    if op == "mute" {
        roster::mute_all(state, index, account, body);
        return json_answer(state, 202, &json!({"muteUnmuteResponse": {}}));
    }
    roster::meeting_changed(state, index, body);
    status(202)
}

/// Whether an `add` brings a person (a work account) in, rather than a service.
fn adds_person(body: &Value) -> bool {
    body.pointer("/participants/to/0/id")
        .and_then(Value::as_str)
        .is_some_and(|id| id.starts_with("8:"))
}

/// A person added to call `index` (a work account, as People adds one): kept with the group chat the `add` named; one
/// with no calling endpoint to ring didn't come in, as captured (`addParticipantFailure`, 580 "no callee endpoints were
/// found"). Ringing an invitee who has one isn't the mock's.
fn add_person(state: &mut State, index: usize, body: &Value) {
    let invitee = body
        .pointer("/participants/to/0/id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let chat = body
        .pointer("/groupChat/threadId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if let Some(call) = state.calling.calls.get_mut(index) {
        call.added.push((invitee.clone(), chat));
    }
    if state.calling_endpoints(&invitee).is_empty() {
        let failure = body
            .pointer("/links/addParticipantFailure")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let phrase = "Audio-video modality controller could not invite participant to this conversation since no callee endpoints were found.";
        let delivered = json!({"participants": {invitee.as_str(): {"code": 580, "subCode": 5205, "phrase": phrase,
            "resultCategories": ["UnexpectedServerError"]}},
            "participantInfos": [{"participant": {"id": invitee}, "transactionEnd": {"code": 580}}]});
        state.call_back(failure.as_deref(), delivered);
    }
}

/// The meeting ended for everyone at its conversation's own URL (its organizer's End meeting): everyone in it gets the
/// call's end.
fn end_for_all(state: &mut State, account: &str, conversation: &str, body: &Value) -> Option<ResponseTemplate> {
    let index = state
        .calling
        .calls
        .iter()
        .position(|call| call.conversation == conversation && call.phase != CallPhase::Ended)?;
    log(state, index, account, "end_conversation");
    leave(state, index, body);
    Some(status(204))
}

/// Someone leaves call `index`: a cancel while it rings, the call's end to everyone once it is up.
fn leave(state: &mut State, index: usize, body: &Value) {
    let code = body
        .pointer("/callTransactionEnd/code")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let phrase = body
        .pointer("/callTransactionEnd/phrase")
        .and_then(Value::as_str)
        .unwrap_or("CallEndReasonLocalUserInitiated")
        .to_owned();
    let call = state.calling.calls[index].clone();
    if call.phase == CallPhase::Ended {
        return;
    }
    // the end names who left as its sender, as the service's does
    let mut end = end_body(code, &phrase);
    if let Some(from) = body.pointer("/participants/from").filter(|from| from.is_object()) {
        end["callEnd"]["sender"] = from.clone();
    }
    end_call_with(state, index, &end);
}

/// Ends call `index` for everyone in it, with `code` and `phrase`: every endpoint that attached to a leg not answered
/// elsewhere, the one that answered, the caller and those who joined get the call's end.
fn end_call(state: &mut State, index: usize, code: i64, phrase: &str) {
    end_call_with(state, index, &end_body(code, phrase));
}

/// Ends call `index` for everyone in it with the end `body`.
fn end_call_with(state: &mut State, index: usize, body: &Value) {
    let call = state.calling.calls[index].clone();
    if call.phase == CallPhase::Ended {
        return;
    }
    let mut ends: Vec<Option<String>> = call
        .legs
        .iter()
        .filter(|leg| !leg.declined)
        .flat_map(|leg| leg.attached.iter().map(|(_, end)| Some(end.clone())))
        .collect();
    ends.push(call.caller_links.as_ref().and_then(|links| links.end.clone()));
    ends.extend(call.joined.iter().map(|(_, links)| links.end.clone()));
    for end in ends {
        state.call_back(end.as_deref(), body.clone());
    }
    state.calling.calls[index].phase = CallPhase::Ended;
}

/// The call's end as delivered (the echo call's `call/end`, with `code` and `phrase`).
fn end_body(code: i64, phrase: &str) -> Value {
    let deliveries: Value = serde_json::from_str(ECHO_DELIVERIES).unwrap_or_default();
    let mut body = deliveries["cases"]
        .as_array()
        .and_then(|cases| cases.iter().find(|case| case["observed"] == "ended"))
        .map_or_else(
            || json!({"callEnd": {}}),
            |case| fixture(&case["body"].to_string(), &[]),
        );
    body["callEnd"]["code"] = json!(code);
    body["callEnd"]["phrase"] = json!(phrase);
    body
}

/// An operation on a call controller's link: `(kind, call, leg)` and its last segment.
fn controller_op(
    state: &mut State,
    account: &str,
    (kind, call, leg): (&str, &str, &str),
    op: Option<&str>,
    body: &Value,
) -> Option<ResponseTemplate> {
    let index = state.calling.calls.iter().position(|known| known.id == call)?;
    let leg_index = leg.parse::<usize>().ok().and_then(|leg| leg.checked_sub(1));
    // the call leg's own link takes the call's keep-alive (no operation after it)
    let op = op.unwrap_or(if kind == "active" { "keepAlive" } else { "" });
    log(state, index, account, op);
    match (kind, op) {
        ("forked", "attach") => attach(state, index, leg_index?, body),
        ("incoming", "progress") => {
            let endpoint = endpoint_of_sender(body, "/callProgress/sender/endpointId");
            leg_mut(state, index, leg_index?)?.ringing.push(endpoint);
            Some(status(200))
        }
        ("incoming", "accept") => accept(state, index, leg_index?, body),
        ("incoming", "reject") => reject(state, index, leg_index?, body),
        ("active", "renegotiate") => renegotiated(state, index, account, body),
        // answered `202` live (a native call's, M3 Task 16; its body not read): 202 with no body
        ("active", "keepAlive") if body.get("callParticipantUpdate").is_some() => Some(status(202)),
        ("negotiations", "answer") => {
            let answer = body.pointer("/mediaAnswer/mediaContent")?;
            if !answer_forwarded(state, index, account, answer) {
                let id = state.calling.calls[index].id.clone();
                state.peer_accept(&id, account, answer);
            }
            Some(status(200))
        }
        ("callAcceptance" | "mediaAnswers", "acknowledge") | ("mcProxy", "applyChannelParameters") => Some(status(200)),
        // Hold answers where the hold is taken back, as captured, and the music on hold joins the call's roster (both
        // ends see it) until Resume is taken
        ("active", "hold") => {
            let id = state.calling.calls[index].id.clone();
            let resume = cc_url(&format!("holdSession/{id}/1/a6/1/resume"));
            roster::enter(state, index, MUSIC_ON_HOLD, None);
            Some(json_answer(
                state,
                202,
                &json!({"holdResponse": {"links": {"resume": resume}}, "debugContent": null}),
            ))
        }
        ("active", "transfer") => transfer(state, index, account, body),
        // the transferred party took it: the transferor is told
        ("transfer", "acceptance") => {
            let told = state.calling.calls[index]
                .transferred
                .as_ref()
                .and_then(|(_, at)| at.clone());
            state.call_back(told.as_deref(), json!({"transferAcceptance": {}, "debugContent": null}));
            Some(status(202))
        }
        ("holdSession", "resume") => {
            roster::leave(state, index, MUSIC_ON_HOLD);
            Some(status(202))
        }
        // the main video line first, as captured: `sendrecv` while the camera is on
        ("active", "updateMediaDescriptions") => {
            let on = body
                .pointer("/UpdateMediaDescriptions/mediaDescriptions/descriptions/0/direction")
                .and_then(Value::as_str)
                == Some("sendrecv");
            roster::video(state, index, account, on);
            Some(status(200))
        }
        _ => None,
    }
}

/// The account renegotiates call `index`'s media: to the call's other client, when one answered it or placed it (its
/// `mediaRenegotiation` callback gets the renegotiation, `follow_ups.json`'s delivery, and its answer goes back to the
/// account's `mediaAnswer` callback: [`answer_forwarded`]); else the call's peer (the media controller, a caller the
/// mock plays) answers at the renegotiation's `mediaAnswer` callback (`follow_ups.json`'s answer delivery), its
/// acknowledgement link this call's. Without either nothing answers. Its own answer: 200 with no body (not captured).
fn renegotiated(state: &mut State, index: usize, account: &str, body: &Value) -> Option<ResponseTemplate> {
    let negotiation = body.get("mediaNegotiation")?;
    let offer = negotiation.get("mediaContent")?;
    let to = negotiation.pointer("/links/mediaAnswer")?.as_str()?.to_owned();
    let call = state.calling.calls[index].clone();
    if let Some((number, callback)) = other_client(&call, account) {
        let follow_ups: Value = serde_json::from_str(FOLLOW_UPS).unwrap_or_default();
        let template = follow_ups["deliveries"][0]["body"].to_string();
        let mut delivery = fixture(&template, &[("SyntheticCall1/27/", &format!("{}/{number}/", call.id))]);
        delivery["mediaNegotiation"]["mediaContent"] = offer.clone();
        delivery["mediaNegotiation"]["callModalities"] = negotiation.get("callModalities").cloned().unwrap_or_default();
        state.calling.calls[index].negotiation = Some((account.to_owned(), to));
        state.call_back(Some(&callback), delivery);
        return Some(status(200));
    }
    if let Some(answer) = state.peer_answer(&call.id, account, offer) {
        let follow_ups: Value = serde_json::from_str(FOLLOW_UPS).unwrap_or_default();
        let template = follow_ups["deliveries"][1]["body"].to_string();
        let mut delivery = fixture(
            &template,
            &[
                ("SyntheticCall1/27/", &format!("{}/1/", call.id)),
                ("<guid:a>", object_id(&call.caller)),
            ],
        );
        delivery["mediaAnswer"]["mediaContent"] = answer;
        state.call_back(Some(&to), delivery);
    }
    Some(status(200))
}

/// A delivery response (`3:::…`) `account`'s calling socket sent: the acknowledgement of its call's acceptance hands
/// out the caller's `mediaRenegotiation` callback (its start names none), kept as the newest such call's, where a
/// callee's renegotiation goes ([`other_client`]).
pub(crate) fn observe_response(state: &mut State, account: &str, frame: &str) {
    let Some(response) = frame
        .strip_prefix("3:::")
        .and_then(|json| serde_json::from_str::<Value>(json).ok())
    else {
        return;
    };
    let Some(body) = response.get("body").and_then(Value::as_str) else {
        return;
    };
    let Some(link) = serde_json::from_str::<Value>(body).ok().and_then(|body| {
        body.pointer("/callAcceptanceAcknowledgement/links/mediaRenegotiation")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }) else {
        return;
    };
    let caller = state
        .calling
        .calls
        .iter_mut()
        .rev()
        .find(|call| call.caller == account && call.caller_links.is_some());
    if let Some(links) = caller.and_then(|call| call.caller_links.as_mut()) {
        links.renegotiation = Some(link);
    }
}

/// The client of call `call` other than `account` that a renegotiation goes to: the leg's callee that answered when
/// the caller renegotiates, the caller (when it is a client, not the mock) when a callee does; with the leg's number
/// and its `mediaRenegotiation` callback.
fn other_client(call: &MockCall, account: &str) -> Option<(u64, String)> {
    let answered = (1_u64..).zip(&call.legs).filter(|(_, leg)| leg.accepted_by.is_some());
    if account == call.caller {
        return answered
            .filter(|(_, leg)| leg.callee != account)
            .find_map(|(number, leg)| Some((number, leg.renegotiation.clone()?)));
    }
    let (number, _) = answered.clone().find(|(_, leg)| leg.callee == account)?;
    let callback = call.caller_links.as_ref()?.renegotiation.clone()?;
    Some((number, callback))
}

/// `account`'s answer to a renegotiation forwarded to it from the call's other client: delivered at that client's
/// `mediaAnswer` callback (`follow_ups.json`'s answer delivery), its acknowledgement link this call's. Whether it was
/// one.
fn answer_forwarded(state: &mut State, index: usize, account: &str, answer: &Value) -> bool {
    let call = &mut state.calling.calls[index];
    let Some((_, to)) = call.negotiation.take_if(|(from, _)| from != account) else {
        return false;
    };
    let follow_ups: Value = serde_json::from_str(FOLLOW_UPS).unwrap_or_default();
    let template = follow_ups["deliveries"][1]["body"].to_string();
    let mut delivery = fixture(
        &template,
        &[
            ("SyntheticCall1/27/", &format!("{}/1/", call.id)),
            ("<guid:a>", object_id(account)),
        ],
    );
    delivery["mediaAnswer"]["mediaContent"] = answer.clone();
    state.call_back(Some(&to), delivery);
    true
}

/// The peer of call `id` renegotiates with `account`, which answered the call: its new offer at the
/// `mediaRenegotiation` callback the account's answer handed out (`follow_ups.json`'s renegotiation delivery), the
/// answer's link this call's. `false` without such a call, answered leg or peer.
pub(crate) fn renegotiate(state: &mut State, id: &str, account: &str) -> bool {
    renegotiate_with(state, id, account, State::peer_reoffer)
}

/// As [`renegotiate`], the peer offering its video (its camera on).
pub(crate) fn renegotiate_video(state: &mut State, id: &str, account: &str) -> bool {
    renegotiate_with(state, id, account, State::peer_video_offer)
}

/// As [`renegotiate`], the peer sharing its screen (`on`, a sharing line added) or no longer (that line rejected).
pub(crate) fn renegotiate_share(state: &mut State, id: &str, account: &str, on: bool) -> bool {
    let offer = if on {
        State::peer_share_on
    } else {
        State::peer_share_off
    };
    renegotiate_with(state, id, account, offer)
}

fn renegotiate_with(
    state: &mut State,
    id: &str,
    account: &str,
    offer_of: fn(&mut State, &str, &str) -> Option<Value>,
) -> bool {
    let Some(index) = state.calling.calls.iter().position(|call| call.id == id) else {
        return false;
    };
    let call = state.calling.calls[index].clone();
    let Some((number, leg)) = (1_u64..).zip(&call.legs).find(|(_, leg)| leg.callee == account) else {
        return false;
    };
    let Some(callback) = leg.renegotiation.clone() else {
        return false;
    };
    let Some(offer) = offer_of(state, id, account) else {
        return false;
    };
    let follow_ups: Value = serde_json::from_str(FOLLOW_UPS).unwrap_or_default();
    let template = follow_ups["deliveries"][0]["body"].to_string();
    let mut delivery = fixture(&template, &[("SyntheticCall1/27/", &format!("{id}/{number}/"))]);
    delivery["mediaNegotiation"]["mediaContent"] = offer;
    state.call_back(Some(&callback), delivery);
    true
}

fn leg_mut(state: &mut State, index: usize, leg: usize) -> Option<&mut Leg> {
    state.calling.calls.get_mut(index)?.legs.get_mut(leg)
}

fn endpoint_of_sender(body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// A callee's endpoint attaches to its leg: its `call/end` kept, the attach answer (`follow_ups.json`'s) for the leg.
fn attach(state: &mut State, index: usize, leg: usize, body: &Value) -> Option<ResponseTemplate> {
    let end = body.pointer("/attach/links/end")?.as_str()?.to_owned();
    let (endpoint, _) = target(&end)?;
    let call = state.calling.calls[index].clone();
    let leg_state = leg_mut(state, index, leg)?;
    leg_state.attached.push((endpoint, end));
    leg_state.roster = roster::link_of(body);
    leg_state.messages = roster::named(body, "receiveMessage");
    leg_state.updates = roster::named(body, "conversationUpdate");
    if let Some(link) = roster::link_of(body) {
        roster::watch(state, index, &link);
    }
    let number = (leg + 1).to_string();
    let follow_ups: Value = serde_json::from_str(FOLLOW_UPS).unwrap_or_default();
    let answer = fixture(
        &follow_ups["attach_answer"].to_string(),
        &[
            ("SyntheticCall1/27/t/2", &format!("{}/{number}/t/{number}", call.id)),
            ("SyntheticConversation1", &call.conversation),
            ("<guid:a>", object_id(&call.caller)),
            ("<guid:b>", object_id(&call.legs[leg].callee)),
        ],
    );
    let mut answer = answer;
    // the conversation's links for the account's hand and its reactions besides (seen 2026-11-06)
    if let Some(responses) = answer["additionalActionResponses"].as_array_mut() {
        for response in responses {
            for op in [
                "publishState",
                "removeState",
                "sendMessage",
                "updateMeetingStates",
                "updateMeetingSettings",
                "updateParticipantProperties",
                "mute",
            ] {
                response["output"]["links"][op] = json!(conversation_url(&call.conversation, Some(op)));
            }
        }
    }
    Some(json_answer(state, 200, &answer))
}

/// A callee's endpoint answers its leg: the call is up; the caller's acceptance carries the callee's answer; the
/// callee's other endpoints that attached get the call's end (answered elsewhere).
fn accept(state: &mut State, index: usize, leg: usize, body: &Value) -> Option<ResponseTemplate> {
    let call = state.calling.calls[index].clone();
    let callee = call.legs.get(leg)?.callee.clone();
    let endpoint = endpoint_of_sender(body, "/callAcceptance/acceptedBy/endpointId");
    // the answering endpoint: the one its callbacks are on
    let answering = body
        .pointer("/callAcceptance/links")
        .and_then(Value::as_object)
        .and_then(|links| links.values().find_map(|link| target(link.as_str()?)))
        .map(|(endpoint, _)| endpoint);
    let others: Vec<String> = call.legs[leg]
        .attached
        .iter()
        .filter(|(attached, _)| Some(attached) != answering.as_ref())
        .map(|(_, end)| end.clone())
        .collect();
    let renegotiation = body
        .pointer("/callAcceptance/links/mediaRenegotiation")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let transfer = body
        .pointer("/callAcceptance/links/transfer")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let leg_state = leg_mut(state, index, leg)?;
    leg_state.accepted_by = Some(endpoint);
    leg_state.attached.retain(|(_, end)| !others.contains(end));
    leg_state.renegotiation = renegotiation;
    leg_state.transfer = transfer;
    let roster_link = leg_state.roster.clone();
    let messages = leg_state.messages.clone();
    let updates = leg_state.updates.clone();
    state.calling.calls[index].phase = CallPhase::Connected;
    roster::enter(state, index, &callee, roster_link);
    roster::listen(state, index, &callee, messages.as_deref());
    roster::updates_at(state, index, &callee, updates.as_deref());
    if let Some(answer) = body.pointer("/callAcceptance/mediaContent") {
        // a caller the mock plays: its peer takes the callee's answer
        state.peer_accept(&call.id, &callee, answer);
    }
    for end in &others {
        state.call_back(Some(end), end_body(0, ANSWERED_ELSEWHERE));
    }
    let answer_media = body.pointer("/callAcceptance/mediaContent").cloned();
    let acceptance = acceptance_body(&call, &callee, None, answer_media);
    let caller_acceptance = call.caller_links.as_ref().and_then(|links| links.acceptance.clone());
    state.call_back(caller_acceptance.as_deref(), acceptance);
    let number = (leg + 1).to_string();
    let follow_ups: Value = serde_json::from_str(FOLLOW_UPS).unwrap_or_default();
    let mut answer = fixture(
        &follow_ups["accept_answer"].to_string(),
        &[("SyntheticCall1/27/", &format!("{}/{number}/", call.id))],
    );
    if let Some(links) = answer
        .pointer_mut("/callAcceptanceAcknowledgement/links")
        .and_then(Value::as_object_mut)
    {
        links.remove("controlVideoStreaming");
        links.insert("hold".to_owned(), json!(hold_url(&call.id)));
        links.insert("transfer".to_owned(), json!(transfer_url(&call.id)));
    }
    Some(json_answer(state, 200, &answer))
}

/// The caller's acceptance of `call` by `callee` (`deliveries.json`'s acceptance), its media the callee's answer;
/// `endpoint_type` `Some("voicemail")` for the callee's voicemail.
fn acceptance_body(call: &MockCall, callee: &str, endpoint_type: Option<&str>, media: Option<Value>) -> Value {
    let deliveries: Value = serde_json::from_str(CHAT_DELIVERIES).unwrap_or_default();
    let case = deliveries["cases"]
        .as_array()
        .and_then(|cases| cases.iter().find(|case| case["observed"] == "acceptance"))
        .map(|case| case["body"].to_string())
        .unwrap_or_default();
    let mut body = fixture(&case, &[("SyntheticCall1", &call.id), ("<guid:b>", object_id(callee))]);
    match endpoint_type {
        Some(kind) => body["callAcceptance"]["acceptedBy"]["endpointType"] = json!(kind),
        None => {
            if let Some(by) = body["callAcceptance"]["acceptedBy"].as_object_mut() {
                by.remove("endpointType");
            }
        }
    }
    if let Some(media) = media {
        body["callAcceptance"]["mediaContent"] = media;
    }
    body["callAcceptance"]["links"]["hold"] = json!(hold_url(&call.id));
    body["callAcceptance"]["links"]["transfer"] = json!(transfer_url(&call.id));
    body
}

/// The call leg's `transfer` link of call `id`, handed out beside its `hold`.
fn transfer_url(id: &str) -> String {
    cc_url(&format!("active/{id}/1/a3/1/transfer"))
}

/// The caller of call `index` transfers its callee (a blind transfer, as captured): the callee is told at its own
/// `transfer` callback, with whom to call, who transferred it, a made-up authorization and where it takes the transfer.
fn transfer(state: &mut State, index: usize, account: &str, body: &Value) -> Option<ResponseTemplate> {
    let target = body.pointer("/callTransfer/target/id")?.as_str()?.to_owned();
    let acceptance = body
        .pointer("/callTransfer/links/transferAcceptance")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let call = state.calling.calls.get_mut(index)?;
    call.transferred = Some((target.clone(), acceptance));
    let id = call.id.clone();
    let callee = call.legs.iter().find(|leg| leg.accepted_by.is_some())?.transfer.clone();
    let mut details = body
        .pointer("/callTransfer/transferor/details")
        .cloned()
        .unwrap_or(Value::Null);
    details["displayName"] = json!(state.display_name(account));
    let delivered = json!({"callTransfer": {
        "target": {"id": target, "displayName": null},
        "replacementDetails": null,
        "isConsultative": false,
        "transferor": {"details": details, "authorizationToken": "mock-transfer-authorization"},
        "newCallModalities": [],
        "links": {
            "transferAcceptance": cc_url(&format!("transfer/{id}/1/t/1/acceptance")),
            "transferCompletion": cc_url(&format!("transfer/{id}/1/t/1/completion")),
        },
    }});
    state.call_back(callee.as_deref(), delivered);
    Some(status(202))
}

/// The service's music-on-hold app, in a held call's roster while the hold lasts (as captured).
const MUSIC_ON_HOLD: &str = "28:56aa0a8f-4cd3-446b-8146-0023476a5ff2";

/// The call leg's `hold` link of call `id`, as an acceptance and an `accept`'s answer hand it out.
fn hold_url(id: &str) -> String {
    cc_url(&format!("active/{id}/1/a3/1/hold"))
}

/// A callee's endpoint declines its leg: its other endpoints stop ringing; a 1:1 call's caller is forwarded to the
/// callee's voicemail, which accepts (as captured).
fn reject(state: &mut State, index: usize, leg: usize, body: &Value) -> Option<ResponseTemplate> {
    let code = body
        .pointer("/callEnd/code")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let call = state.calling.calls[index].clone();
    let callee = call.legs.get(leg)?.callee.clone();
    let ends: Vec<String> = call.legs[leg].attached.iter().map(|(_, end)| end.clone()).collect();
    let leg_state = leg_mut(state, index, leg)?;
    leg_state.declined = true;
    for end in ends {
        state.call_back(Some(&end), end_body(code, "CallEndReasonLocalUserInitiated"));
    }
    if call.group_chat.is_none() {
        forward_to_voicemail(state, index, &callee);
    }
    Some(status(200))
}

/// The caller of call `index` is forwarded to `callee`'s voicemail, which accepts the call (as captured).
fn forward_to_voicemail(state: &mut State, index: usize, callee: &str) {
    let call = state.calling.calls[index].clone();
    let Some(links) = call.caller_links.clone() else {
        return;
    };
    let deliveries: Value = serde_json::from_str(CHAT_DELIVERIES).unwrap_or_default();
    let progress = deliveries["cases"]
        .as_array()
        .and_then(|cases| cases.iter().find(|case| case["observed"] == "nothing"))
        .map(|case| fixture(&case["body"].to_string(), &[("<guid:b>", object_id(callee))]))
        .unwrap_or_default();
    state.call_back(links.progress.as_deref(), progress);
    // with the media on, the voicemail answers the caller's offer as a peer of the mock's (it records what it hears);
    // without, its acceptance carries no media
    let answer = call
        .offer
        .as_ref()
        .and_then(|offer| state.peer_answer(&call.id, &call.caller, offer));
    let mut acceptance = acceptance_body(&call, callee, Some("voicemail"), answer.clone());
    if answer.is_none()
        && let Some(accepted) = acceptance["callAcceptance"].as_object_mut()
    {
        accepted.remove("mediaContent");
    }
    state.call_back(links.acceptance.as_deref(), acceptance);
    state.calling.calls[index].phase = CallPhase::Connected;
}

/// A caller the mock plays rings `to` (no client places it): the call's id.
pub(crate) fn ring(state: &mut State, from: &str, to: &[String], group_chat: Option<String>) -> String {
    let one_to_one = group_chat.is_none();
    let index = new_call(state, from, to, group_chat, None, None);
    let id = state.calling.calls[index].id.clone();
    if let (true, [callee]) = (one_to_one, to) {
        // with the media on, the caller's peer offers (a 1:1 invitation carries the caller's offer)
        state.calling.calls[index].offer = state.peer_offer(&id, callee);
    }
    ring_callees(state, index);
    id
}

/// Call `id`'s ring ends with `code` and `phrase` (the caller cancelled, it timed out): every endpoint that attached
/// to a leg still ringing gets the call's end; a group call runs on (to be joined); a caller of the account's own in a
/// 1:1 call that timed out is forwarded to the callee's voicemail (as captured).
pub(crate) fn end_ring(state: &mut State, id: &str, code: i64, phrase: &str) -> bool {
    let Some(index) = state.calling.calls.iter().position(|call| call.id == id) else {
        return false;
    };
    let call = state.calling.calls[index].clone();
    let ringing: Vec<&Leg> = call
        .legs
        .iter()
        .filter(|leg| leg.accepted_by.is_none() && !leg.declined)
        .collect();
    for leg in &ringing {
        for (_, end) in &leg.attached {
            state.call_back(Some(end), end_body(code, phrase));
        }
    }
    let first = ringing.first().map(|leg| leg.callee.clone());
    match (call.group_chat.is_some(), first) {
        // a group call runs on with its caller in it, to be joined
        (true, _) => state.calling.calls[index].phase = CallPhase::Connected,
        (false, Some(callee)) if call.caller_links.is_some() => forward_to_voicemail(state, index, &callee),
        (false, _) => state.calling.calls[index].phase = CallPhase::Ended,
    }
    true
}

/// Call `id`, up, is ended by the party the mock plays: everyone in it gets the call's end.
pub(crate) fn hang_up(state: &mut State, id: &str) -> bool {
    let Some(index) = state.calling.calls.iter().position(|call| call.id == id) else {
        return false;
    };
    end_call(state, index, 0, "CallEndReasonLocalUserInitiated");
    true
}

fn json_answer(state: &State, code: u16, body: &Value) -> ResponseTemplate {
    crate::chatsvc::json(code, &in_cloud(&state.cloud, body.clone()))
}

pub(crate) fn in_cloud(cloud: &teams_auth::cloud::Cloud, value: Value) -> Value {
    use base64::Engine as _;
    let Some(origin) = cloud.origin() else { return value };
    let base = format!("{}flightproxy", origin.as_str());
    match value {
        Value::String(text) => Value::String(text.replace("https://api.flightproxy.teams.microsoft.com", &base)),
        Value::Array(values) => Value::Array(values.into_iter().map(|value| in_cloud(cloud, value)).collect()),
        Value::Object(mut fields) => {
            if let Some(Value::String(gp)) = fields.get("gp") {
                let engine = base64::engine::general_purpose::STANDARD;
                if let Some(decoded) = engine
                    .decode(gp)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                {
                    fields.insert(
                        "gp".into(),
                        Value::String(engine.encode(in_cloud(cloud, decoded).to_string())),
                    );
                }
            }
            Value::Object(
                fields
                    .into_iter()
                    .map(|(key, value)| (key, in_cloud(cloud, value)))
                    .collect(),
            )
        }
        other => other,
    }
}
