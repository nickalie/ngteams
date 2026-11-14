//! Call bookkeeping independent of transport policy: offers, callbacks, acceptance and media links.

mod context;
mod json;
mod offer;
use context::{Context, Reply};
use json::strict_json;
pub use offer::unread_invitation;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};
use teams_proto::{CallOperation, Request, Response};
use teams_proto::{Cloud, Mri};
use url::Url;
/// One Trouter delivery as the socket received it (a socket.io `3:::` request), its body decoded: the connection it
/// came on (an opaque key of the caller's, the same for the responses sent on that connection), its signed id, the
/// path it was delivered to, its `trouter-request`, `X-Microsoft-Skype-Chain-ID` and `X-Microsoft-Skype-Message-ID`
/// headers and its body.
#[derive(Clone, PartialEq, Eq)]
pub struct Delivery {
    pub connection: String,
    pub id: i64,
    pub path: String,
    pub trouter_request: Option<String>,
    pub chain_id: Option<String>,
    pub message_id: Option<String>,
    pub body: Vec<u8>,
}

impl fmt::Debug for Delivery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Delivery")
            .field("connection", &self.connection)
            .field("id", &self.id)
            .field("body_len", &self.body.len())
            .finish_non_exhaustive()
    }
}

/// What a delivery did to the registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observed {
    /// It is no delivery of a registered or pending call's callback the tracker reads.
    Nothing,
    /// A call's acceptance: its call controller links are bound (or were already, to the same call), and may be acknowledged.
    Acceptance,
    /// A call's acceptance whose links bound nothing (unreadable, or naming another call).
    AcceptanceUnbound,
    /// The end of a call: it has ended.
    Ended,
    /// An incoming call's invitation the tracker offers (or another delivery of one it offers): its ring-time sends may
    /// go.
    Offered,
    /// An incoming call's invitation the tracker doesn't offer (not addressed to the account, a
    /// ring that ended, or one it can't read): nothing may be sent for it, and it is handed on to nobody.
    Ignored,
    /// The end of an offered invitation's ring (or of its answered call): nothing passes on it any more.
    RingEnded,
    /// A media negotiation delivery of a live call or an answered invitation's (a renegotiation, an answer): the link
    /// its answer or acknowledgement goes to is bound.
    Renegotiation,
}

/// Where a call is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// A sent start, waiting for its answer.
    Pending,
    Live,
    /// A sent `leave` went: only it passes again.
    Ending,
    /// Ended, or never registered: nothing passes.
    Ended,
}

/// A call controller: its host and call id.
type Controller = (String, String);

/// A link a mapped operation may go to, exactly.
type Link = (CallOperation, String);

/// An endpoint callback the start named: its URL's authority (host and port) and path.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Callback {
    authority: String,
    path: String,
}

/// One call of the run.
#[derive(Debug)]
struct Call {
    state: State,
    context: Context,
    acceptance: Option<Callback>,
    /// The start's `call/end` and `conversationEnd` callbacks.
    ends: Vec<Callback>,
    /// The authority and the `/v4/f/<endpoint>/callAgent/<agent>/` path prefix every callback of the start shares.
    agent: Option<Callback>,
    /// Each link a mapped operation may go to, exactly.
    links: Vec<Link>,
    /// The call controller's host and call id, once an acceptance bound them.
    controller: Option<Controller>,
}

impl Call {
    fn answered(&mut self, response: &Response) {
        self.context.answer(response.body());
        let links = response
            .is_success()
            .then(|| answer_links_in(&self.context.cloud, response.body()))
            .flatten();
        self.state = if links.is_some() { State::Live } else { State::Ended };
        self.links.extend(links.into_iter().flatten());
    }

    /// Whether a delivery at `path` carries a link of this live call's media negotiation ([`media_link`]): at a
    /// `mediaRenegotiation` or `mediaAnswer` callback on the start's own call agent.
    fn negotiates_at(&self, path: &str) -> bool {
        self.state == State::Live && self.agent.as_ref().is_some_and(|agent| is_media_callback(agent, path))
    }

    /// Binds the link [`media_link`] reads from a delivery at `path`, of the call the acceptance bound. Whether it bound.
    fn bind_media(&mut self, path: &str, body: &[u8]) -> bool {
        let Some(link) = self
            .controller
            .as_ref()
            .and_then(|(_, call)| media_link_in(&self.context.cloud, path, body, call))
        else {
            return false;
        };
        if !self.links.contains(&link) {
            self.links.push(link);
        }
        true
    }

    /// Follows the call to the call id a media answer's acknowledgement delivered at `path` moves it to (a retarget:
    /// `{"mediaAcknowledgement": {…, "resultCategories": ["Success"]}, "links": {"callLeg", "mediaRenegotiation", …}}`,
    /// seen 2026-11-03 when a meeting's lobby let the account in): the controller the acknowledgement's links all name,
    /// and the links of the mapped operations on it, as an acceptance binds them. The same call, its participants
    /// unchanged: only where the call controller takes its operations moved. One whose links don't all name one call, or
    /// name no call leg of their own, moves nothing.
    fn follow_retarget(&mut self, body: &[u8]) {
        // read as the web client reads it (`JSON.parse`, the last of a duplicate key kept), not strictly: the retarget's
        // `links` names `callLeg` twice, `null` and then the new call leg (seen 2026-11-03). What it binds is still
        // only the mapped operations' links on the one call they all name, from the call's own callback.
        let Ok(body) = serde_json::from_slice::<Value>(body) else {
            log::debug!("call registry: a media acknowledgement that doesn't read moves nothing");
            return;
        };
        let succeeded = body
            .pointer("/mediaAcknowledgement/resultCategories")
            .and_then(Value::as_array)
            .is_some_and(|categories| categories.iter().any(|category| category.as_str() == Some("Success")));
        let Some((controller, links)) = body
            .get("links")
            .and_then(Value::as_object)
            .filter(|_| succeeded)
            .and_then(|value| controller_links_in(&self.context.cloud, value))
        else {
            log::debug!("call registry: a media acknowledgement names no one call (success: {succeeded})");
            return;
        };
        let leg = links
            .iter()
            .any(|(operation, _)| *operation == CallOperation::KeepAlive);
        if !leg || self.controller.as_ref() == Some(&controller) {
            log::debug!(
                "call registry: a media acknowledgement keeps the call at {} (a call leg named: {leg})",
                controller.1
            );
            return;
        }
        log::debug!("call registry: the call moved to call id {}", controller.1);
        self.controller = Some(controller);
        for link in links {
            if !self.links.contains(&link) {
                self.links.push(link);
            }
        }
    }
}

/// Whether `path` is a media negotiation callback (`…/call/mediaRenegotiation/`, `…/call/mediaAnswer/`, and
/// `…/call/mediaAcknowledgement/`, where the answerer hears its answer was acknowledged) on `agent`.
fn is_media_callback(agent: &Callback, path: &str) -> bool {
    path.starts_with(&agent.path)
        && (path.ends_with("/call/mediaRenegotiation/")
            || path.ends_with("/call/mediaAnswer/")
            || is_acknowledged_at(path))
}

/// Whether `path` is a call's `transfer` callback (`…/call/transfer/`), where a transfer the far end asked for is
/// delivered.
fn is_transfer_callback(path: &str) -> bool {
    path.ends_with("/call/transfer/")
}

/// Binds the `transferAcceptance` link a transfer delivered at a live call's or an answered invitation's `transfer`
/// callback hands out (`callTransfer.links.transferAcceptance`, `…/cc/v1/transfer/<call>/…/acceptance` on the same call,
/// seen 2026-11-06): where the transferred party takes it.
fn bind_transfer(cloud: &Cloud, inner: &mut Inner, delivery: &Delivery) {
    if let Some(offer) = inner.offers.iter_mut().find(|offer| offer.transfers_at(&delivery.path)) {
        let call = offer.invitation.leg.1.clone();
        if let Some(link) = transfer_link_in(cloud, &delivery.body, &call)
            && !offer.links.contains(&link)
        {
            offer.links.push(link);
        }
        return;
    }
    let Some(call) = inner.calls.iter_mut().find(|call| {
        call.state == State::Live
            && call
                .agent
                .as_ref()
                .is_some_and(|agent| delivery.path.starts_with(&agent.path))
    }) else {
        return;
    };
    let Some(link) = call
        .controller
        .as_ref()
        .and_then(|(_, id)| transfer_link_in(cloud, &delivery.body, id))
    else {
        return;
    };
    if !call.links.contains(&link) {
        call.links.push(link);
    }
}

/// The `transferAcceptance` link a transfer's delivery `body` hands out for the call `call`, on the flight proxy.
fn transfer_link_in(cloud: &Cloud, body: &[u8], call: &str) -> Option<Link> {
    let url = proxy_link_in(
        cloud,
        strict_json(body)?.pointer("/callTransfer/links/transferAcceptance")?,
    )?;
    let (_, at, kind, last) = controller_path(&url)?;
    (at == call && kind == "transfer" && last == CallOperation::TransferAcceptance.segment())
        .then(|| (CallOperation::TransferAcceptance, url.as_str().to_owned()))
}

/// Whether `path` is a media acknowledgement callback (`…/call/mediaAcknowledgement/`): it binds no link, its
/// delivery waits only for its receipt.
fn is_acknowledged_at(path: &str) -> bool {
    path.ends_with("/call/mediaAcknowledgement/")
}

/// The link a media negotiation delivery at `path` hands out for the call `call` (the call controller's call id), on
/// the flight proxy: a renegotiation's (at a `mediaRenegotiation` callback) `mediaNegotiation.links.mediaAnswer`,
/// `…/cc/v1/negotiations/<call>/…/answer`; an answer's (at a `mediaAnswer` callback)
/// `mediaAnswer.links.mediaAcknowledgement`, `…/cc/v1/mediaAnswers/<call>/…/acknowledge`. `None` for anything else.
fn media_link_in(cloud: &Cloud, path: &str, body: &[u8], call: &str) -> Option<Link> {
    let (operation, pointer, kind) = if path.ends_with("/call/mediaRenegotiation/") {
        (
            CallOperation::MediaAnswer,
            "/mediaNegotiation/links/mediaAnswer",
            "negotiations",
        )
    } else if path.ends_with("/call/mediaAnswer/") {
        (
            CallOperation::MediaAcknowledge,
            "/mediaAnswer/links/mediaAcknowledgement",
            "mediaAnswers",
        )
    } else {
        return None;
    };
    let url = proxy_link_in(cloud, strict_json(body)?.pointer(pointer)?)?;
    let (_, at, of, last) = controller_path(&url)?;
    (at == call && of == kind && last == operation.segment()).then(|| (operation, url.as_str().to_owned()))
}

/// One account's calling state, shared by its socket and outbox.
#[derive(Debug)]
pub struct Tracker {
    account: Mri,
    cloud: Cloud,
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    calls: Vec<Call>,
    offers: Vec<offer::Offer>,
    connections: BTreeMap<String, String>,
    responses: BTreeMap<(String, i64), Reply>,
}

/// A pre-send binding waiting for this attempt's outcome; not an authorization.
#[derive(Clone, Copy, Debug)]
pub enum Attempt {
    Nothing,
    Start(usize),
    Answer {
        offer: usize,
        operation: CallOperation,
    },
    /// A live call's `hold` (a placed call's, or an answered offer's): its answer hands out where it is resumed.
    Hold(Holder),
}

/// Whose `hold` went: a call the account placed or joined, or an invitation it answered, by index.
#[derive(Clone, Copy, Debug)]
pub enum Holder {
    Call(usize),
    Offer(usize),
}

impl Tracker {
    #[must_use]
    pub fn new(account: Mri) -> Self {
        Self::for_cloud(account, Cloud::Microsoft)
    }

    /// Tracks links only within this account's calling service.
    #[must_use]
    pub fn for_cloud(account: Mri, cloud: Cloud) -> Self {
        Self {
            cloud,
            account,
            inner: Mutex::new(Inner::default()),
        }
    }
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
    #[must_use]
    pub fn account(&self) -> &Mri {
        &self.account
    }
    #[must_use]
    pub fn holds_floor(&self) -> bool {
        let inner = self.lock();
        inner.calls.iter().any(|call| call.state == State::Live) || inner.offers.iter().any(offer::Offer::holds_floor)
    }
    /// Observes an incoming delivery. `other_busy` is read under the app's floor lock.
    pub fn observe_on_floor(&self, delivery: &Delivery, other_busy: bool) -> Observed {
        let observed = self.observe_filtered(delivery, other_busy);
        self.remember_delivery(delivery, observed);
        observed
    }
    fn observe_filtered(&self, delivery: &Delivery, other_busy: bool) -> Observed {
        let Some(invitation) = offer::read_invitation_in(&self.cloud, delivery) else {
            return if offer::may_be_invitation(delivery) {
                Observed::Ignored
            } else {
                self.observe(delivery)
            };
        };
        if invitation.callee != self.account || invitation.caller == self.account {
            return Observed::Ignored;
        }
        let mut inner = self.lock();
        if let Some(offer) = inner.offers.iter().find(|offer| offer.is_redelivery(&invitation)) {
            return if offer.state == offer::OfferState::Ended {
                Observed::Ignored
            } else {
                Observed::Offered
            };
        }
        let busy = inner.calls.iter().any(|call| call.state == State::Live)
            || inner.offers.iter().any(|offer| {
                offer.state == offer::OfferState::Answered
                    || (offer.state == offer::OfferState::Ringing && offer.ring_end_known())
            });
        if other_busy || busy {
            return Observed::Ignored;
        }
        inner.offers.push(offer::Offer::new_in(self.cloud.clone(), invitation));
        Observed::Offered
    }
    pub fn observe_delivery(&self, delivery: &Delivery) -> Observed {
        self.observe_on_floor(delivery, false)
    }
    /// Releases an invitation whose attach failed before it rang.
    pub fn let_go(&self, attach: &str) {
        for offer in self
            .lock()
            .offers
            .iter_mut()
            .filter(|offer| offer.state == offer::OfferState::Ringing && offer.holds(CallOperation::Attach, attach))
        {
            offer.end();
        }
    }
    /// Whether this write enters a call, for the cross-account floor.
    #[must_use]
    pub fn enters<R: Request>(request: &R) -> bool {
        request.call_url().is_some_and(|url| {
            url.path().ends_with("/epconv")
                || url.path().ends_with("/accept")
                || request.body().is_some_and(|body| body.get("callInvitation").is_some())
        })
    }
    /// Notes one actual attempt before the socket may deliver its callbacks.
    pub fn before<R: Request>(&self, request: &R) -> Attempt {
        let Some(url) = request.call_url().filter(|url| calling_url_fits(&self.cloud, url)) else {
            return Attempt::Nothing;
        };
        let body = request.body().unwrap_or(Value::Null);
        self.remember_request(url, &body);
        let callbacks = callbacks(&body);
        let mut inner = self.lock();
        let start = body
            .get("callInvitation")
            .and_then(|invitation| invitation.get("links"))
            .and_then(Value::as_object);
        if let Some(links) = start {
            let acceptance = callback(links.get("acceptance"));
            let ends = callbacks
                .iter()
                .filter_map(|url| parse_callback(url))
                .filter(|callback| {
                    callback.path.ends_with("/call/end/") || callback.path.ends_with("/conversation/conversationEnd/")
                })
                .collect();
            let agent = acceptance.as_ref().and_then(agent_of);
            inner.calls.push(Call {
                state: State::Pending,
                context: Context::from_request(self.cloud.clone(), &body, &self.account),
                acceptance,
                ends,
                agent,
                links: Vec::new(),
                controller: None,
            });
            return Attempt::Start(inner.calls.len() - 1);
        }
        let operation = CallOperation::ALL.into_iter().find(|operation| {
            !operation.segment().is_empty() && url.path().ends_with(&format!("/{}", operation.segment()))
        });
        if let Some(operation) = operation
            && let Some(index) = inner
                .offers
                .iter()
                .rposition(|offer| offer.holds(operation, url.as_str()) && offer.open_for(operation))
        {
            let offer = &mut inner.offers[index];
            if operation == CallOperation::Attach {
                offer.attached(&callbacks);
            }
            offer.decided(operation);
            if matches!(operation, CallOperation::Attach | CallOperation::Accept) {
                return Attempt::Answer {
                    offer: index,
                    operation,
                };
            }
            if operation == CallOperation::Hold {
                return Attempt::Hold(Holder::Offer(index));
            }
        }
        if operation == Some(CallOperation::Hold)
            && let Some(index) = inner.calls.iter().position(|call| {
                call.state == State::Live
                    && call
                        .links
                        .iter()
                        .any(|(op, link)| *op == CallOperation::Hold && link == url.as_str())
            })
        {
            return Attempt::Hold(Holder::Call(index));
        }
        if operation == Some(CallOperation::Leave) {
            for call in inner.calls.iter_mut().filter(|call| {
                call.state == State::Live
                    && call
                        .links
                        .iter()
                        .any(|(op, link)| *op == CallOperation::Leave && link == url.as_str())
            }) {
                call.state = State::Ending;
            }
        }
        Attempt::Nothing
    }
    /// Retires a start that the transport definitely did not send. Unknown outcomes keep their callbacks instead.
    pub fn not_sent(&self, attempt: Attempt) {
        if let Attempt::Start(index) = attempt
            && let Some(call) = self
                .lock()
                .calls
                .get_mut(index)
                .filter(|call| call.state == State::Pending)
        {
            call.state = State::Ended;
        }
    }
    /// Binds an explicit answer. Unknown transport outcomes leave pending callbacks intact.
    pub fn answered(&self, attempt: Attempt, response: &Response) {
        let mut inner = self.lock();
        match attempt {
            Attempt::Start(index) => {
                if let Some(call) = inner.calls.get_mut(index).filter(|call| call.state == State::Pending) {
                    call.answered(response);
                }
            }
            Attempt::Answer { offer, operation } if response.is_success() => {
                if let Some(offer) = inner.offers.get_mut(offer) {
                    match (operation, offer.state) {
                        (CallOperation::Attach, offer::OfferState::Ringing) => {
                            offer.bind_answer(response.body());
                            offer.context.answer(response.body());
                        }
                        (CallOperation::Accept, offer::OfferState::Answered) => {
                            offer.bind_acceptance(response.body());
                            offer.context.answer(response.body());
                        }
                        _ => {}
                    }
                }
            }
            Attempt::Hold(holder) if response.is_success() => {
                let (links, call) = match holder {
                    Holder::Call(index) => match inner.calls.get_mut(index) {
                        Some(call) => (&mut call.links, call.controller.as_ref().map(|(_, id)| id.clone())),
                        None => return,
                    },
                    Holder::Offer(index) => match inner.offers.get_mut(index) {
                        Some(offer) => (&mut offer.links, Some(offer.invitation.leg.1.clone())),
                        None => return,
                    },
                };
                if let Some(link) = call.and_then(|call| resume_link_in(&self.cloud, response.body(), &call))
                    && !links.contains(&link)
                {
                    links.push(link);
                }
            }
            _ => {}
        }
    }
    pub(crate) fn observe(&self, delivery: &Delivery) -> Observed {
        let mut inner = self.lock();
        if let Some(offer) = inner
            .offers
            .iter_mut()
            .find(|offer| offer.state != offer::OfferState::Ended && offer.ends_at(&delivery.path))
        {
            offer.end();
            return Observed::RingEnded;
        }
        if is_transfer_callback(&delivery.path) {
            bind_transfer(&self.cloud, &mut inner, delivery);
            return Observed::Nothing;
        }
        let negotiated = if let Some(offer) = inner
            .offers
            .iter_mut()
            .find(|offer| offer.negotiates_at(&delivery.path))
        {
            let bound = is_acknowledged_at(&delivery.path) || offer.bind_media(&delivery.path, &delivery.body);
            Some(bound)
        } else {
            inner
                .calls
                .iter_mut()
                .find(|call| call.negotiates_at(&delivery.path))
                .map(|call| {
                    let acknowledged = is_acknowledged_at(&delivery.path);
                    if acknowledged {
                        call.follow_retarget(&delivery.body);
                    }
                    acknowledged || call.bind_media(&delivery.path, &delivery.body)
                })
        };
        if let Some(bound) = negotiated {
            if !bound {
                return Observed::Nothing;
            }
            return Observed::Renegotiation;
        }
        let found = inner.calls.iter_mut().find_map(|call| {
            if call.state == State::Ended {
                return None;
            }
            if call.ends.iter().any(|end| end.path == delivery.path) {
                return Some((call, true));
            }
            let accepted = call
                .acceptance
                .as_ref()
                .is_some_and(|accept| accept.path == delivery.path);
            accepted.then_some((call, false))
        });
        match found {
            None => Observed::Nothing,
            Some((call, true)) => {
                call.state = State::Ended;
                Observed::Ended
            }
            Some((call, false)) => bind_acceptance_in(&self.cloud, call, delivery),
        }
    }
}

/// Callback links carried in a typed call request's nested link objects.
fn callbacks(value: &Value) -> Vec<String> {
    let mut links = Vec::new();
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if key == "links" {
                    if let Some(fields) = value.as_object() {
                        links.extend(
                            fields
                                .values()
                                .filter_map(Value::as_str)
                                .filter(|url| parse_callback(url).is_some())
                                .map(str::to_owned),
                        );
                    }
                } else {
                    links.extend(callbacks(value));
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                links.extend(callbacks(item));
            }
        }
        _ => {}
    }
    links
}

/// An endpoint callback link: `https`, a host, no user info, query or fragment.
fn callback(link: Option<&Value>) -> Option<Callback> {
    parse_callback(link?.as_str()?)
}

/// [`callback`] of a link's text.
fn parse_callback(link: &str) -> Option<Callback> {
    let url = Url::parse(link).ok()?;
    let plain =
        url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none();
    if url.scheme() != "https" || !plain {
        return None;
    }
    Some(Callback {
        authority: authority(&url)?,
        path: url.path().to_owned(),
    })
}

/// A URL's host and port, as the URL gives them (the port only when it isn't the scheme's default).
fn authority(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

/// A callback's authority and `/v4/f/<endpoint>/callAgent/<agent>/` prefix, with `/trouter` prepended for Own:
/// every callback of one start shares it.
fn agent_of(callback: &Callback) -> Option<Callback> {
    let prefix = if callback.path.starts_with("/trouter/") {
        "/trouter"
    } else {
        ""
    };
    let segments: Vec<&str> = callback
        .path
        .strip_prefix(prefix)?
        .strip_prefix('/')?
        .split('/')
        .collect();
    match segments.as_slice() {
        ["v4", "f", endpoint, "callAgent", agent, ..] if !endpoint.is_empty() && !agent.is_empty() => Some(Callback {
            authority: callback.authority.clone(),
            path: format!("{prefix}/v4/f/{endpoint}/callAgent/{agent}/"),
        }),
        _ => None,
    }
}

fn calling_url_fits(cloud: &Cloud, url: &Url) -> bool {
    let start = cloud
        .service_url(teams_proto::CALL_START_URL, "/flightproxy/api/v2/epconv")
        .ok();
    start.as_ref() == Some(url) || teams_proto::CallLink::parse_in(cloud, url.as_str()).is_some()
}

/// A flight proxy operation link at `/api/v2/ep/<host>/…`: `https` on [`teams_proto::CALLING_HOST`] without a port
/// for Microsoft, or the exact Own origin (including its port) under `/flightproxy`; no user info or fragment.
fn proxy_link_in(cloud: &Cloud, link: &Value) -> Option<Url> {
    let link = teams_proto::CallLink::parse_in(cloud, link.as_str()?)?;
    Url::parse(link.as_str()).ok()
}

/// The conversation links of a start's answer (see the module docs); `None` unless the answer is a JSON object with a
/// `conversationController` on the account's flight proxy at `/api/v2/ep/<host>/conv/<id>`, after stripping Own's
/// leading `/flightproxy`.
fn answer_links_in(cloud: &Cloud, answer: &[u8]) -> Option<Vec<Link>> {
    const NAMES: [(CallOperation, &str); 13] = [
        (CallOperation::UpdateEndpointMetadata, "updateEndpointMetadata"),
        (
            CallOperation::UpdateParticipantProperties,
            "updateParticipantProperties",
        ),
        (CallOperation::Add, "addParticipantAndModality"),
        (CallOperation::Leave, "leave"),
        (CallOperation::Admit, "admit"),
        (CallOperation::RemoveParticipant, "removeParticipant"),
        (CallOperation::UpdateEndpointState, "updateEndpointState"),
        (CallOperation::PublishState, "publishState"),
        (CallOperation::RemoveState, "removeState"),
        (CallOperation::SendMessage, "sendMessage"),
        (CallOperation::UpdateMeetingStates, "updateMeetingStates"),
        (CallOperation::UpdateMeetingSettings, "updateMeetingSettings"),
        (CallOperation::Mute, "mute"),
    ];
    let answer = strict_json(answer)?;
    let controller = proxy_link_in(cloud, answer.get("conversationController")?)?;
    let segments: Vec<&str> = controller
        .path()
        .trim_start_matches("/flightproxy")
        .trim_start_matches('/')
        .split('/')
        .collect();
    if !matches!(segments.as_slice(), ["api", "v2", "ep", host, "conv", id] if !host.is_empty() && !id.is_empty()) {
        return None;
    }
    let links = answer.get("links").and_then(Value::as_object);
    // the conversation itself: where its organizer ends it for everyone
    let end = (CallOperation::EndConversation, controller.as_str().to_owned());
    Some(
        NAMES
            .iter()
            .filter_map(|(operation, name)| {
                let link = proxy_link_in(cloud, links?.get(*name)?)?;
                let fits = link.path() == format!("{}/{}", controller.path(), operation.segment())
                    && link.query() == controller.query();
                fits.then(|| (*operation, link.as_str().to_owned()))
            })
            .chain(std::iter::once(end))
            .collect(),
    )
}

/// Binds an acceptance delivery's call controller links to `call` (see the module docs).
fn bind_acceptance_in(cloud: &Cloud, call: &mut Call, delivery: &Delivery) -> Observed {
    let Some((controller, links)) = acceptance_links_in(cloud, &delivery.body) else {
        return Observed::AcceptanceUnbound;
    };
    if call.controller.as_ref().is_some_and(|bound| *bound != controller) {
        return Observed::AcceptanceUnbound;
    }
    call.controller = Some(controller);
    for link in links {
        if !call.links.contains(&link) {
            call.links.push(link);
        }
    }
    Observed::Acceptance
}

/// The call controller (host, call id) and the links of a mapped operation an acceptance's body names; `None` unless
/// every link under `callAcceptance.links` that names a call names the same one, and at least one does.
fn acceptance_links_in(cloud: &Cloud, body: &[u8]) -> Option<(Controller, Vec<Link>)> {
    let body = strict_json(body)?;
    controller_links_in(cloud, body.pointer("/callAcceptance/links")?.as_object()?)
}

/// The call controller (host, call id) and the links of a mapped operation `links` names (an acceptance's
/// `callAcceptance.links`, a retarget's `links`); `None` unless every link that names a call names the same one, and at
/// least one does.
fn controller_links_in(cloud: &Cloud, links: &Map<String, Value>) -> Option<(Controller, Vec<Link>)> {
    const NAMES: [(CallOperation, &str, &str); 7] = [
        (CallOperation::Acknowledge, "acknowledgement", "callAcceptance"),
        (CallOperation::KeepAlive, "callLeg", "active"),
        (CallOperation::Hold, "hold", "active"),
        (CallOperation::Transfer, "transfer", "active"),
        (
            CallOperation::ApplyChannelParameters,
            "applyChannelParameters",
            "mcProxy",
        ),
        (CallOperation::Renegotiate, "mediaRenegotiation", "active"),
        (
            CallOperation::UpdateMediaDescriptions,
            "updateMediaDescriptions",
            "active",
        ),
    ];
    let mut controller: Option<Controller> = None;
    for link in links.values() {
        let Some(url) = proxy_link_in(cloud, link) else {
            continue;
        };
        let Some((host, call, _, _)) = controller_path(&url) else {
            continue;
        };
        match &controller {
            Some(bound) if *bound != (host.clone(), call.clone()) => return None,
            _ => controller = Some((host, call)),
        }
    }
    let controller = controller?;
    let bound = NAMES
        .iter()
        .filter_map(|(operation, name, kind)| {
            let url = proxy_link_in(cloud, links.get(*name)?)?;
            let (host, call, at, last) = controller_path(&url)?;
            let fits = (host, call) == controller && at == *kind && link_is(*operation, &url, &last);
            fits.then(|| (*operation, url.as_str().to_owned()))
        })
        .collect();
    Some((controller, bound))
}

/// Where a hold's answer `body` says the hold of call `call` (the call controller's call id) is taken back, on the
/// flight proxy: `holdResponse.links.resume`, `…/cc/v1/holdSession/<call>/…/resume` (seen 2026-11-06).
fn resume_link_in(cloud: &Cloud, body: &[u8], call: &str) -> Option<Link> {
    let url = proxy_link_in(cloud, strict_json(body)?.pointer("/holdResponse/links/resume")?)?;
    let (_, at, kind, last) = controller_path(&url)?;
    (at == call && kind == "holdSession" && last == CallOperation::Resume.segment())
        .then(|| (CallOperation::Resume, url.as_str().to_owned()))
}

/// Whether a call controller link ending in `last` is `operation`'s (its kind already checked): its captured segment
/// last, or for a keep-alive the call leg's own link (`…/cc/v1/active/<call>/<n>/<leg>/<n>`, nothing after it).
pub(crate) fn link_is(operation: CallOperation, url: &Url, last: &str) -> bool {
    if operation == CallOperation::KeepAlive {
        return url.path_segments().is_some_and(|segments| {
            segments.count()
                == if url.path().starts_with("/flightproxy/") {
                    12
                } else {
                    11
                }
        });
    }
    last == operation.segment()
}

/// A call controller link's host, call id, kind (`callAcceptance`, `mcProxy`, `active`, …) and last segment:
/// `/api/v2/ep/<host>/cc/v1/<kind>/<call>/…`, after stripping Own's leading `/flightproxy`.
fn controller_path(url: &Url) -> Option<(String, String, String, String)> {
    let segments: Vec<&str> = url
        .path()
        .trim_start_matches("/flightproxy")
        .trim_start_matches('/')
        .split('/')
        .collect();
    match segments.as_slice() {
        ["api", "v2", "ep", host, "cc", "v1", kind, call, .., last]
            if !host.is_empty() && !call.is_empty() && !kind.is_empty() =>
        {
            Some((
                (*host).to_owned(),
                (*call).to_owned(),
                (*kind).to_owned(),
                (*last).to_owned(),
            ))
        }
        _ => None,
    }
}

impl teams_proto::ResponseDelivery for Delivery {
    fn connection(&self) -> &str {
        &self.connection
    }
    fn id(&self) -> i64 {
        self.id
    }
    fn trouter_request(&self) -> Option<&str> {
        self.trouter_request.as_deref()
    }
    fn chain_id(&self) -> Option<&str> {
        self.chain_id.as_deref()
    }
    fn message_id(&self) -> Option<&str> {
        self.message_id.as_deref()
    }
}

#[cfg(test)]
mod tests;
