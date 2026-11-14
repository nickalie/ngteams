//! Incoming invitation parsing and retained call-link bookkeeping.

use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::Value;
use url::Url;

use super::json::strict_json;
use super::{Callback, Controller, Delivery, Link, controller_path, parse_callback, proxy_link_in};
use teams_proto::{CallOperation, Cloud, INVITATION_EVENTS, Mri, is_group_chat_id};

/// What an invitation delivery says, read: who calls whom, in which group chat, and its links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Invitation {
    pub(crate) caller: Mri,
    pub(crate) callee: Mri,
    pub(crate) group_chat: Option<String>,
    /// The endpoint root it was delivered to (`/v4/f/<endpoint>/`).
    endpoint: String,
    /// `conversationInvitation.conversationController`, exactly.
    controller: String,
    /// `callNotification.links.attach`, exactly.
    attach: String,
    /// The forked call leg the links name (the call controller's host and call id).
    pub(crate) leg: Controller,
}

/// Where an offered invitation is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OfferState {
    /// Ringing: its ring-time sends may go, and its answer or decline.
    Ringing,
    /// Answered by a sent `accept`: that `accept` passes again (the browser decides a request on two layers,
    /// and the web client resends it), and the answered call's follow-ups to its bound links.
    Answered,
    /// Answered, and its callee left by a sent `leave`: only that `leave` passes again (the web client resends it).
    Leaving,
    /// Declined by a sent `reject`: only that `reject` passes again (as an answered invitation's `accept`).
    Declined,
    /// Its ring ended: nothing passes on it any more.
    Ended,
}

/// One offered invitation of the run.
#[derive(Debug)]
pub(crate) struct Offer {
    pub(super) context: super::Context,
    pub(crate) state: OfferState,
    pub(crate) invitation: Invitation,
    /// The links a ring-time send may go to, exactly.
    pub(crate) links: Vec<Link>,
    /// The call agent every callback of the callee's attach shares, once one was permitted.
    agent: Option<Callback>,
    /// The callbacks the call's end comes back to, from the sent attach.
    ends: Vec<Callback>,
    /// When it was offered: an offer never attached holds the call floor only for [`UNATTACHED_RING`].
    offered: Instant,
}

/// How long an offered invitation no attach bound (so its ring's end is never seen) holds the call floor: longer
/// than an invitation rings before the service gives up on it.
pub(crate) const UNATTACHED_RING: Duration = Duration::from_secs(90);

impl Offer {
    pub(crate) fn new_in(cloud: Cloud, invitation: Invitation) -> Self {
        let links = vec![(CallOperation::Attach, invitation.attach.clone())];
        let chat = invitation
            .group_chat
            .clone()
            .or_else(|| teams_proto::one_to_one_id(invitation.callee.as_str(), invitation.caller.as_str()));
        let context = super::Context::incoming(cloud, chat, invitation.controller.clone());
        Self {
            context,
            state: OfferState::Ringing,
            invitation,
            links,
            agent: None,
            ends: Vec::new(),
            offered: Instant::now(),
        }
    }

    /// Whether it holds the call floor ([`crate::CallFloor`]): answered and not left, or ringing (attached, until its
    /// ring ends; never attached, for [`UNATTACHED_RING`] at most).
    pub(crate) fn holds_floor(&self) -> bool {
        match self.state {
            OfferState::Answered => true,
            OfferState::Ringing => self.ring_end_known() || self.offered.elapsed() < UNATTACHED_RING,
            _ => false,
        }
    }

    /// Whether `invitation` is another delivery of this one (the same leg, the same parties).
    pub(crate) fn is_redelivery(&self, invitation: &Invitation) -> bool {
        self.invitation == *invitation
    }

    /// Whether a delivery at `path` ends the ring.
    pub(crate) fn ends_at(&self, path: &str) -> bool {
        self.ends.iter().any(|end| end.path == path)
    }

    /// Whether an `attach` bound where its ring ends (its `call/end`, its `conversationEnd`), so its end is seen.
    pub(crate) fn ring_end_known(&self) -> bool {
        !self.ends.is_empty()
    }

    /// Ends the ring for good.
    pub(crate) fn end(&mut self) {
        self.state = OfferState::Ended;
    }

    /// Whether it holds exactly `url` for `operation`.
    pub(crate) fn holds(&self, operation: CallOperation, url: &str) -> bool {
        self.links.iter().any(|(held, link)| *held == operation && link == url)
    }

    /// Whether `operation` may act on the invitation as it stands: while it rings, its ring-time sends, its answer
    /// (the endpoint state and the `accept`) or its decline; once answered, its `accept` again and the answered call's
    /// captured operations (the endpoint's state and metadata, its own participant properties, its `leave`); once its
    /// callee left, only that `leave` again; once declined, only its `reject` again.
    pub(crate) fn open_for(&self, operation: CallOperation) -> bool {
        match self.state {
            OfferState::Ringing => matches!(
                operation,
                CallOperation::Attach
                    | CallOperation::Progress
                    | CallOperation::UpdateEndpointState
                    | CallOperation::Accept
                    | CallOperation::Reject
            ),
            OfferState::Answered => matches!(
                operation,
                CallOperation::Accept
                    | CallOperation::UpdateEndpointState
                    | CallOperation::UpdateEndpointMetadata
                    | CallOperation::UpdateParticipantProperties
                    | CallOperation::Leave
                    | CallOperation::MediaAnswer
                    | CallOperation::MediaAcknowledge
                    | CallOperation::Renegotiate
                    | CallOperation::UpdateMediaDescriptions
                    | CallOperation::ApplyChannelParameters
                    | CallOperation::KeepAlive
                    | CallOperation::PublishState
                    | CallOperation::RemoveState
                    | CallOperation::SendMessage
                    | CallOperation::Hold
                    | CallOperation::Resume
                    | CallOperation::UpdateMeetingStates
                    | CallOperation::UpdateMeetingSettings
                    | CallOperation::Mute
                    | CallOperation::Add
                    | CallOperation::Transfer
                    | CallOperation::TransferAcceptance
            ),
            OfferState::Leaving => operation == CallOperation::Leave,
            OfferState::Declined => operation == CallOperation::Reject,
            OfferState::Ended => false,
        }
    }

    /// After a sent `accept`: answered. After a sent `reject`: declined, so its ring ended. After the
    /// answered call's sent `leave`: leaving.
    pub(crate) fn decided(&mut self, operation: CallOperation) {
        match (operation, self.state) {
            (CallOperation::Accept, OfferState::Ringing) => {
                self.state = OfferState::Answered;
            }
            (CallOperation::Reject, OfferState::Ringing) => {
                self.state = OfferState::Declined;
            }
            (CallOperation::Leave, OfferState::Answered) => self.state = OfferState::Leaving,
            _ => {}
        }
    }

    /// Binds the links a sent `attach`'s answer hands out: of the call leg (`callInvitation.links`), on the flight
    /// proxy, `…/cc/v1/incoming/<call>/…/<op>` of the invitation's own call, its `progress`, its `acceptance`
    /// (`…/accept`) and its `callLeg` (`…/reject`); of the conversation its `join` subscribed to (the additional
    /// action's response for exactly the invitation's conversation), the `updateEndpointState` at the conversation's
    /// own path with its query. Whether anything bound.
    pub(crate) fn bind_answer(&mut self, answer: &[u8]) -> bool {
        let Some(answer) = strict_json(answer) else {
            return false;
        };
        let leg = [
            (CallOperation::Progress, "progress"),
            (CallOperation::Accept, "acceptance"),
            (CallOperation::Reject, "callLeg"),
        ];
        let mut found: Vec<Link> = leg
            .iter()
            .filter_map(|(operation, name)| {
                let url = proxy_link_in(
                    &self.context.cloud,
                    answer.pointer("/callInvitation/links")?.get(*name)?,
                )?;
                let fits = controller_path(&url).is_some_and(|(_, call, kind, last)| {
                    call == self.invitation.leg.1 && kind == "incoming" && last == operation.segment()
                });
                fits.then(|| (*operation, url.as_str().to_owned()))
            })
            .collect();
        found.extend(self.conversation_links(&answer));
        let mut bound = false;
        for (operation, url) in found {
            if !self.holds(operation, &url) {
                self.links.push((operation, url));
                bound = true;
            }
        }
        bound
    }

    /// The conversation links of an `attach` answer's `join` response for exactly the invitation's conversation: its
    /// `updateEndpointState`, the conversation's path plus the segment, with the conversation's query.
    fn conversation_links(&self, answer: &Value) -> Vec<Link> {
        let Ok(controller) = Url::parse(&self.invitation.controller) else {
            return Vec::new();
        };
        let responses = answer
            .get("additionalActionResponses")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        responses
            .iter()
            .filter(|response| {
                response.get("url").and_then(Value::as_str) == Some(self.invitation.controller.as_str())
                    && response
                        .pointer("/output/conversationController")
                        .and_then(Value::as_str)
                        == Some(self.invitation.controller.as_str())
            })
            .flat_map(|response| {
                [
                    CallOperation::UpdateEndpointState,
                    CallOperation::UpdateEndpointMetadata,
                    CallOperation::UpdateParticipantProperties,
                    CallOperation::Leave,
                    CallOperation::PublishState,
                    CallOperation::RemoveState,
                    CallOperation::SendMessage,
                    CallOperation::UpdateMeetingStates,
                    CallOperation::UpdateMeetingSettings,
                    CallOperation::Mute,
                ]
                .into_iter()
                .filter_map(|operation| {
                    let url = proxy_link_in(
                        &self.context.cloud,
                        response.pointer("/output/links")?.get(operation.segment())?,
                    )?;
                    let fits = url.path() == format!("{}/{}", controller.path(), operation.segment())
                        && url.query() == controller.query();
                    fits.then(|| (operation, url.as_str().to_owned()))
                })
                .chain(
                    // the add: its link is named `addParticipantAndModality`, its path ends in `add`
                    proxy_link_in(
                        &self.context.cloud,
                        response
                            .pointer("/output/links/addParticipantAndModality")
                            .unwrap_or(&Value::Null),
                    )
                    .filter(|url| {
                        url.path() == format!("{}/{}", controller.path(), CallOperation::Add.segment())
                            && url.query() == controller.query()
                    })
                    .map(|url| (CallOperation::Add, url.as_str().to_owned())),
                )
                .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The one call agent all of `callbacks` are on, under this invitation's endpoint (and the agent already bound, if
    /// any); `None` when there are none or they aren't.
    fn agent_of(&self, callbacks: &[String]) -> Option<Callback> {
        let mut agent: Option<Callback> = None;
        for link in callbacks {
            let callback = parse_callback(link)?;
            let this = super::agent_of(&callback)?;
            if !this
                .path
                .starts_with(&format!("{}callAgent/", self.invitation.endpoint))
            {
                return None;
            }
            match &agent {
                Some(bound) if *bound != this => return None,
                _ => agent = Some(this),
            }
        }
        let agent = agent?;
        self.agent.as_ref().is_none_or(|bound| *bound == agent).then_some(agent)
    }

    /// After a sent `attach`: its call agent is bound, and its `call/end` and `conversationEnd` callbacks end the
    /// ring.
    pub(crate) fn attached(&mut self, callbacks: &[String]) {
        if let Some(agent) = self.agent_of(callbacks) {
            self.agent = Some(agent);
        }
        for link in callbacks {
            let Some(callback) = parse_callback(link) else { continue };
            let ends =
                callback.path.ends_with("/call/end/") || callback.path.ends_with("/conversation/conversationEnd/");
            if ends && !self.ends.contains(&callback) {
                self.ends.push(callback);
            }
        }
    }

    /// Binds the answered call's links a sent `accept`'s answer hands out (`callAcceptanceAcknowledgement.links`), on
    /// the flight proxy, of the invitation's own call: its `mediaRenegotiation` (`…/cc/v1/active/<call>/…/renegotiate`),
    /// its `updateMediaDescriptions` (`…/cc/v1/active/<call>/…/updateMediaDescriptions`), its `callLeg` (the keep-alive's,
    /// `…/cc/v1/active/<call>/<n>/<leg>/<n>`) and its `applyChannelParameters`
    /// (`…/cc/v1/mcProxy/<call>/…/applyChannelParameters`, a group call's). Whether anything bound.
    pub(crate) fn bind_acceptance(&mut self, answer: &[u8]) -> bool {
        const NAMES: [(CallOperation, &str, &str); 6] = [
            (CallOperation::Renegotiate, "mediaRenegotiation", "active"),
            (CallOperation::KeepAlive, "callLeg", "active"),
            (CallOperation::Hold, "hold", "active"),
            (CallOperation::Transfer, "transfer", "active"),
            (
                CallOperation::UpdateMediaDescriptions,
                "updateMediaDescriptions",
                "active",
            ),
            (
                CallOperation::ApplyChannelParameters,
                "applyChannelParameters",
                "mcProxy",
            ),
        ];
        let Some(answer) = strict_json(answer) else {
            return false;
        };
        let mut bound = false;
        for (operation, name, kind) in NAMES {
            let Some(url) = answer
                .pointer("/callAcceptanceAcknowledgement/links")
                .and_then(|links| links.get(name))
                .and_then(|value| proxy_link_in(&self.context.cloud, value))
            else {
                continue;
            };
            let fits = controller_path(&url).is_some_and(|(_, call, at, last)| {
                call == self.invitation.leg.1 && at == kind && super::link_is(operation, &url, &last)
            });
            if fits && !self.holds(operation, url.as_str()) {
                self.links.push((operation, url.as_str().to_owned()));
            }
            bound |= fits;
        }
        bound
    }

    /// Whether a delivery at `path` carries a link of the answered call's media negotiation ([`super::media_link`]): at
    /// a `mediaRenegotiation` or `mediaAnswer` callback on the call agent the sent attach bound.
    pub(crate) fn transfers_at(&self, path: &str) -> bool {
        self.state == OfferState::Answered
            && super::is_transfer_callback(path)
            && self.agent.as_ref().is_some_and(|agent| path.starts_with(&agent.path))
    }

    pub(crate) fn negotiates_at(&self, path: &str) -> bool {
        self.state == OfferState::Answered
            && self
                .agent
                .as_ref()
                .is_some_and(|agent| super::is_media_callback(agent, path))
    }

    /// Binds the link [`super::media_link`] reads from a delivery at `path`, of the invitation's own call. Whether it
    /// bound.
    pub(crate) fn bind_media(&mut self, path: &str, body: &[u8]) -> bool {
        let Some(link) = super::media_link_in(&self.context.cloud, path, body, &self.invitation.leg.1) else {
            return false;
        };
        if !self.links.contains(&link) {
            self.links.push(link);
        }
        true
    }
}

/// The invitation `delivery` is, read (see the module docs); `None` for any other delivery or an invitation that
/// doesn't read: at an endpoint's root, `evt` an invitation's, `gp` base64 of a JSON object with a caller and a callee,
/// an `attach` link and a conversation on the flight proxy, the leg's links all of one forked call, no call it
/// replaces, no group context (a meeting), and the call's kind in a captured shape: a 1:1 call's (107) names no group
/// chat and isn't multi-party (`conversationInvitation.isMultiParty` false); a group call's (109) is multi-party and
/// names its group chat by a group chat's id, the only field naming its other participants.
pub(crate) fn read_invitation_in(cloud: &Cloud, delivery: &Delivery) -> Option<Invitation> {
    if delivery.path.starts_with("/trouter/") == cloud.is_microsoft() {
        return None;
    }
    let endpoint = endpoint_root(&delivery.path)?;
    let body = strict_json(&delivery.body)?;
    let event = body.get("evt")?.as_u64()?;
    if !INVITATION_EVENTS.contains(&event) {
        return None;
    }
    let gp = base64::engine::general_purpose::STANDARD
        .decode(body.get("gp")?.as_str()?)
        .ok()?;
    let gp = strict_json(&gp)?;
    let notification = gp.get("callNotification")?;
    let text = |value: &Value, pointer: &str| value.pointer(pointer).and_then(Value::as_str).map(str::to_owned);
    let unset = |pointer: &str| gp.pointer(pointer).is_none_or(Value::is_null);
    if !unset("/callNotification/replaces") || !unset("/groupContext") {
        return None;
    }
    let attach_link = proxy_link_in(cloud, notification.pointer("/links/attach")?)?;
    let (host, call, kind, last) = controller_path(&attach_link)?;
    if kind != "forked" || last != "attach" {
        return None;
    }
    let controller = conversation_of(cloud, &gp)?;
    let group_chat = match gp.get("groupChat") {
        None | Some(Value::Null) => None,
        Some(chat) => Some(text(chat, "/threadId").filter(|id| is_group_chat_id(id))?),
    };
    if !kind_fits(event, &gp, group_chat.is_some()) {
        return None;
    }
    Some(Invitation {
        caller: Mri::new(text(notification, "/from/id")?),
        callee: Mri::new(text(notification, "/to/id")?),
        group_chat,
        endpoint,
        controller: controller.as_str().to_owned(),
        attach: attach_link.as_str().to_owned(),
        leg: (host, call),
    })
}

/// Why [`read_invitation_in`] doesn't read `delivery` as an invitation, in a few words naming the first part that doesn't
/// fit (never anything the delivery carries): what the log says of an incoming call it ignores.
#[must_use]
pub fn unread_invitation(delivery: &Delivery) -> String {
    let gp = strict_json(&delivery.body)
        .and_then(|body| body.get("gp").and_then(Value::as_str).map(str::to_owned))
        .and_then(|gp| base64::engine::general_purpose::STANDARD.decode(gp).ok());
    if let Some(key) = gp.as_deref().and_then(super::json::twice_named) {
        return format!("its notification names `{key}` twice");
    }
    first_misfit(delivery).to_owned()
}

/// The first part of `delivery` that keeps it from being read as an invitation ([`unread_invitation`]).
fn first_misfit(delivery: &Delivery) -> &'static str {
    let Some(body) = strict_json(&delivery.body) else {
        return "its body isn't JSON";
    };
    let Some(event) = body.get("evt").and_then(Value::as_u64) else {
        return "it names no event";
    };
    if !INVITATION_EVENTS.contains(&event) {
        return "its event isn't an invitation's";
    }
    let Some(gp) = body
        .get("gp")
        .and_then(Value::as_str)
        .and_then(|gp| base64::engine::general_purpose::STANDARD.decode(gp).ok())
        .and_then(|gp| strict_json(&gp))
    else {
        return "its notification doesn't decode";
    };
    let set = |pointer: &str| gp.pointer(pointer).is_some_and(|value| !value.is_null());
    if gp.get("callNotification").is_none() {
        "it has no call notification"
    } else if set("/callNotification/replaces") {
        "it replaces another call"
    } else if set("/groupContext") {
        "it names a group context"
    } else if gp.pointer("/callNotification/links/attach").is_none() {
        "it has no attach link"
    } else if conversation_of(&Cloud::Microsoft, &gp).is_none() {
        "it names no conversation"
    } else if !kind_fits(event, &gp, set("/groupChat")) {
        "its kind doesn't fit its chat (1:1 or group)"
    } else if endpoint_root(&delivery.path).is_none() {
        "it isn't at an endpoint's root"
    } else {
        "it isn't addressed to the account, or the account is busy"
    }
}

/// Whether a delivery that [`read_invitation_in`] doesn't read may still be an invitation, so that nothing of it is handed
/// on or answered: a delivery at an endpoint's root whose body isn't a strictly read JSON object with a numeric `evt`
/// other than an invitation's ([`INVITATION_EVENTS`]), or that carries a `callNotification` (at its top, or in a `gp`
/// that decodes, one that doesn't counting as one). The root's other captured events (`evt` 115 and 116, a `cp` and no
/// `gp`) aren't.
pub(crate) fn may_be_invitation(delivery: &Delivery) -> bool {
    if endpoint_root(&delivery.path).is_none() {
        return false;
    }
    let Some(Value::Object(body)) = strict_json(&delivery.body) else {
        return true;
    };
    let event = body.get("evt").and_then(Value::as_u64);
    if event.is_none_or(|event| INVITATION_EVENTS.contains(&event)) || body.contains_key("callNotification") {
        return true;
    }
    body.get("gp").is_some_and(|gp| {
        gp.as_str()
            .and_then(|gp| base64::engine::general_purpose::STANDARD.decode(gp).ok())
            .and_then(|gp| strict_json(&gp))
            .is_none_or(|gp| !gp.is_object() || gp.get("callNotification").is_some())
    })
}

/// The conversation an invitation's decoded `gp` invites to (`conversationInvitation.conversationController`), when
/// it is a conversation on the flight proxy (`/api/v2/ep/<host>/conv/<id>`).
fn conversation_of(cloud: &Cloud, gp: &Value) -> Option<Url> {
    let controller = proxy_link_in(cloud, gp.pointer("/conversationInvitation/conversationController")?)?;
    let fits = {
        let segments: Vec<&str> = controller
            .path()
            .trim_start_matches("/flightproxy")
            .trim_start_matches('/')
            .split('/')
            .collect();
        matches!(segments.as_slice(), ["api", "v2", "ep", host, "conv", id] if !host.is_empty() && !id.is_empty())
    };
    fits.then_some(controller)
}

/// Whether an invitation of `event` with this decoded `gp`, naming a group chat or not (`names_chat`), is of its call's
/// kind in a captured shape (see [`read_invitation_in`]): a 1:1 call's names no group chat and isn't multi-party, a group
/// call's names its group chat and is (its other participants are known only through its chat: one that names none
/// doesn't read).
fn kind_fits(event: u64, gp: &Value, names_chat: bool) -> bool {
    let multi_party = gp
        .pointer("/conversationInvitation/isMultiParty")
        .and_then(Value::as_bool);
    match event {
        107 => !names_chat && multi_party == Some(false),
        109 => names_chat && multi_party == Some(true),
        _ => false,
    }
}

/// `path` when it is an endpoint's root, `/v4/f/<endpoint>/`.
fn endpoint_root(path: &str) -> Option<String> {
    let url = Url::parse(&format!(
        "https://trouter.invalid{}",
        path.strip_prefix("/trouter").unwrap_or(path)
    ))
    .ok()?;
    let segments: Vec<&str> = url.path_segments()?.collect();
    match segments.as_slice() {
        ["v4", "f", endpoint, ""] if !endpoint.is_empty() && url.query().is_none() => Some(path.to_owned()),
        _ => None,
    }
}
