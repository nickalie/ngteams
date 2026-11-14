//! Account, callback and caption metadata on the canonical tracked call entries.
use super::{Delivery, Observed, State, Tracker, offer::OfferState};
use serde_json::Value;
use std::collections::BTreeSet;
use teams_proto::captions::{CaptionRoster, CaptionWrite, CaptionWriteKind};
use teams_proto::{CallState, Mri};

#[derive(Debug, Default)]
pub(super) struct Context {
    pub(super) cloud: teams_proto::Cloud,
    chat: Option<String>,
    actor: Option<(String, String)>,
    conversation: Option<String>,
    caption_add: Option<String>,
    captions: CaptionRoster,
    callbacks: BTreeSet<String>,
}
impl Context {
    pub(super) fn incoming(cloud: teams_proto::Cloud, chat: Option<String>, conversation: String) -> Self {
        Self {
            cloud,
            chat,
            conversation: Some(conversation),
            ..Self::default()
        }
    }
    pub(super) fn from_request(cloud: teams_proto::Cloud, body: &Value, own: &Mri) -> Self {
        let chat = body
            .pointer("/groupChat/threadId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                let peers = body.pointer("/participants/to")?.as_array()?;
                let [peer] = peers.as_slice() else { return None };
                teams_proto::one_to_one_id(own.as_str(), peer.get("id")?.as_str()?)
            });
        let mut context = Self {
            cloud,
            chat,
            ..Self::default()
        };
        context.request(body, own);
        context
    }
    pub(super) fn request(&mut self, body: &Value, own: &Mri) {
        for actor in actors(body) {
            if self.actor.is_none()
                && actor.get("id").and_then(Value::as_str) == Some(own.as_str())
                && let (Some(endpoint), Some(participant)) = (
                    actor.get("endpointId").and_then(Value::as_str),
                    actor.get("participantId").and_then(Value::as_str),
                )
            {
                self.actor = Some((endpoint.to_owned(), participant.to_owned()));
            }
        }
        self.callbacks.extend(all_callbacks(body));
    }
    fn matches_actor(&self, body: &Value) -> bool {
        self.actor.as_ref().is_none_or(|(endpoint, participant)| {
            actors(body).into_iter().all(|actor| {
                actor
                    .get("endpointId")
                    .is_none_or(|value| value.as_str() == Some(endpoint))
                    && actor
                        .get("participantId")
                        .is_none_or(|value| value.as_str() == Some(participant))
            })
        })
    }
    pub(super) fn answer(&mut self, body: &[u8]) {
        let Ok(value) = serde_json::from_slice::<Value>(body) else {
            return;
        };
        self.answer_value(&value);
        if let Some(roster) = value.get("roster") {
            self.captions.update(roster);
        }
    }
    fn answer_value(&mut self, body: &Value) {
        if self.conversation.is_none() {
            self.conversation = body
                .get("conversationController")
                .and_then(Value::as_str)
                .and_then(|link| teams_proto::CallLink::parse_in(&self.cloud, link))
                .map(|link| link.as_str().to_owned());
        }
        if let Some(link) = body.pointer("/links/addParticipant").and_then(Value::as_str)
            && self.conversation.as_ref().is_some_and(|root| {
                let (Ok(root), Ok(link)) = (url::Url::parse(root), url::Url::parse(link)) else {
                    return false;
                };
                root.origin() == link.origin()
                    && link.path() == format!("{}/addParticipant", root.path())
                    && root.query() == link.query()
            })
        {
            self.caption_add = Some(link.to_owned());
        }
        if let Some(responses) = body.get("additionalActionResponses").and_then(Value::as_array) {
            for response in responses {
                if response.get("url").and_then(Value::as_str) == self.conversation.as_deref()
                    && let Some(output) = response.get("output")
                    && output.get("conversationController").and_then(Value::as_str) == self.conversation.as_deref()
                {
                    self.answer_value(output);
                }
            }
        }
        if let Some(actions) = body.get("additionalActions").and_then(Value::as_array) {
            for action in actions {
                if let Some(response) = action.get("response").and_then(Value::as_str)
                    && let Ok(value) = serde_json::from_str::<Value>(response)
                {
                    self.answer_value(&value);
                }
                if let Some(value) = action.get("response").filter(|value| value.is_object()) {
                    self.answer_value(value);
                }
            }
        }
    }
    fn roster(&mut self, body: &[u8]) {
        let Ok(value) = serde_json::from_slice::<Value>(body) else {
            return;
        };
        self.captions.update(&value);
    }
    fn owns_callback(&self, path: &str) -> bool {
        self.callbacks
            .iter()
            .any(|url| super::parse_callback(url).is_some_and(|callback| callback.path == path))
    }
    fn allows(&self, write: &CaptionWrite, roots: impl Iterator<Item = String>) -> bool {
        if self.chat.as_deref() != Some(write.thread())
            || self.actor.as_ref().is_none_or(|(endpoint, participant)| {
                endpoint != &write.from().endpoint_id || write.from().participant_id.as_ref() != Some(participant)
            })
        {
            return false;
        }
        let expected = match write.kind() {
            CaptionWriteKind::Invite => self.caption_add.as_deref(),
            _ => self.captions.service().map(|service| service.command.as_str()),
        };
        if expected != Some(write.url().as_str()) {
            return false;
        }
        let roots: Vec<_> = roots.collect();
        if !self.callbacks.iter().any(|callback| {
            roots
                .iter()
                .any(|root| callback.starts_with(&format!("{root}callAgent/")))
        }) {
            return false;
        }
        super::callbacks(write.bare_body())
            .iter()
            .all(|url| roots.iter().any(|root| url.starts_with(&format!("{root}callAgent/"))))
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Entry {
    Call(usize),
    Offer(usize),
}
#[derive(Debug)]
pub(super) struct Reply {
    entry: Entry,
    request: String,
    chain: Option<String>,
    message: Option<String>,
    acceptance: bool,
}
impl Tracker {
    /// Later call requests hand out fresh callback paths before their deliveries can arrive.
    pub(super) fn remember_request(&self, url: &url::Url, body: &Value) {
        let mut inner = self.lock();
        if !request_fits(&self.account, &inner.connections, body) {
            return;
        }
        if let Some(call) = inner.calls.iter_mut().rfind(|call| {
            call.state == State::Live
                && call.context.matches_actor(body)
                && call.links.iter().any(|(_, link)| link == url.as_str())
        }) {
            call.context.request(body, &self.account);
            return;
        }
        if let Some(offer) = inner.offers.iter_mut().rfind(|offer| {
            matches!(offer.state, OfferState::Ringing | OfferState::Answered)
                && offer.context.matches_actor(body)
                && teams_proto::CallOperation::ALL
                    .into_iter()
                    .any(|operation| offer.holds(operation, url.as_str()) && offer.open_for(operation))
        }) {
            offer.context.request(body, &self.account);
        }
    }

    pub(super) fn remember_delivery(&self, delivery: &Delivery, observed: Observed) {
        if matches!(
            observed,
            Observed::Ignored | Observed::Ended | Observed::RingEnded | Observed::AcceptanceUnbound
        ) {
            return;
        }
        let mut inner = self.lock();
        if !inner.connections.get(&delivery.connection).is_some_and(|root| {
            url::Url::parse(root).is_ok_and(|root| {
                delivery.path == root.path() || delivery.path.starts_with(&format!("{}callAgent/", root.path()))
            })
        }) {
            return;
        }
        let entry = if observed == Observed::Offered {
            super::offer::read_invitation_in(&self.cloud, delivery)
                .and_then(|invitation| inner.offers.iter().rposition(|offer| offer.is_redelivery(&invitation)))
                .map(Entry::Offer)
        } else {
            inner
                .calls
                .iter()
                .rposition(|call| call.state != State::Ended && call.context.owns_callback(&delivery.path))
                .map(Entry::Call)
                .or_else(|| {
                    inner
                        .offers
                        .iter()
                        .rposition(|offer| {
                            offer.state != OfferState::Ended && offer.context.owns_callback(&delivery.path)
                        })
                        .map(Entry::Offer)
                })
        };
        let Some(entry) = entry else { return };
        match entry {
            Entry::Call(i) => inner.calls[i].context.roster(&delivery.body),
            Entry::Offer(i) => inner.offers[i].context.roster(&delivery.body),
        }
        let needs_reply = matches!(
            observed,
            Observed::Offered | Observed::Acceptance | Observed::Renegotiation
        );
        let Some(request) = delivery.trouter_request.as_ref().filter(|_| needs_reply) else {
            return;
        };
        let acceptance =
            serde_json::from_slice::<Value>(&delivery.body).is_ok_and(|value| value.get("callAcceptance").is_some());
        inner.responses.insert(
            (delivery.connection.clone(), delivery.id),
            Reply {
                entry,
                request: request.clone(),
                chain: delivery.chain_id.clone(),
                message: delivery.message_id.clone(),
                acceptance,
            },
        );
    }
}
impl CallState for Tracker {
    fn call_allowed(&self, url: &url::Url, body: Option<&Value>) -> bool {
        if !super::calling_url_fits(&self.cloud, url) {
            return false;
        }
        let empty = Value::Null;
        let body = body.unwrap_or(&empty);
        let inner = self.lock();
        if !request_fits(&self.account, &inner.connections, body) {
            return false;
        }
        if body.get("conversationRequest").is_some() {
            return !inner.connections.is_empty();
        }
        inner.calls.iter().any(|call| {
            call.context.matches_actor(body)
                && call.links.iter().any(|(bound, link)| {
                    link == url.as_str() && (call.state == State::Live || *bound == teams_proto::CallOperation::Leave)
                })
        }) || teams_proto::CallOperation::ALL.into_iter().any(|operation| {
            inner.offers.iter().any(|offer| {
                offer.context.matches_actor(body) && offer.holds(operation, url.as_str()) && offer.open_for(operation)
            })
        })
    }

    fn connected(&self, connection: &str, root: &str) -> bool {
        let Ok(url) = url::Url::parse(root) else { return false };
        if connection.is_empty()
            || url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || if self.cloud.is_microsoft() {
                !url.host_str()
                    .is_some_and(|host| host.ends_with(".trouter.teams.microsoft.com"))
                    || !url.port().is_none_or(|port| matches!(port, 443 | 3443))
            } else {
                !self.cloud.contains(&url)
            }
            || !url
                .path()
                .strip_prefix(if self.cloud.is_microsoft() {
                    "/v4/f/"
                } else {
                    "/trouter/v4/f/"
                })
                .and_then(|tail| tail.strip_suffix('/'))
                .is_some_and(|endpoint| {
                    !endpoint.is_empty()
                        && endpoint
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                })
        {
            return false;
        }
        self.lock().connections.insert(connection.to_owned(), url.to_string());
        true
    }
    fn disconnected(&self, connection: &str) {
        let mut inner = self.lock();
        inner.connections.remove(connection);
        inner.responses.retain(|(conn, _), _| conn != connection);
    }
    fn caption_allowed(&self, write: &CaptionWrite) -> bool {
        if write.from().mri != self.account {
            return false;
        }
        let mut inner = self.lock();
        let roots: Vec<_> = inner.connections.values().cloned().collect();
        if roots.is_empty() {
            return false;
        }
        let context =
            if let Some(call) = inner
                .calls
                .iter_mut()
                .find(|call| call.state == State::Live && call.context.allows(write, roots.clone().into_iter()))
            {
                &mut call.context
            } else if let Some(offer) = inner.offers.iter_mut().find(|offer| {
                offer.state == OfferState::Answered && offer.context.allows(write, roots.clone().into_iter())
            }) {
                &mut offer.context
            } else {
                return false;
            };
        context.callbacks.extend(all_callbacks(write.bare_body()));
        true
    }
    fn response_allowed(&self, connection: &str, id: i64, envelope: &str) -> bool {
        let Ok(body) = serde_json::from_str::<Value>(envelope) else {
            return false;
        };
        if body.get("id").and_then(Value::as_i64) != Some(id) || body.get("status").and_then(Value::as_u64) != Some(200)
        {
            return false;
        }
        let mut inner = self.lock();
        let Some(root) = inner.connections.get(connection).cloned() else {
            return false;
        };
        let key = (connection.to_owned(), id);
        let Some(reply) = inner.responses.get(&key) else {
            return false;
        };
        let live = match reply.entry {
            Entry::Call(i) => inner
                .calls
                .get(i)
                .is_some_and(|call| matches!(call.state, State::Pending | State::Live)),
            Entry::Offer(i) => inner
                .offers
                .get(i)
                .is_some_and(|offer| matches!(offer.state, OfferState::Ringing | OfferState::Answered)),
        };
        if !live || body.pointer("/headers/trouter-request").and_then(Value::as_str) != Some(reply.request.as_str()) {
            return false;
        }
        if reply.acceptance
            && [
                ("X-Microsoft-Skype-Chain-ID", &reply.chain),
                ("X-Microsoft-Skype-Message-ID", &reply.message),
            ]
            .into_iter()
            .any(|(name, expected)| {
                expected.as_ref().is_some_and(|expected| {
                    body.get("headers")
                        .and_then(|headers| headers.get(name))
                        .and_then(Value::as_str)
                        != Some(expected)
                })
            })
        {
            return false;
        }
        let Some(content) = body.get("body").and_then(Value::as_str) else {
            return false;
        };
        let callbacks = if reply.acceptance {
            let Some(callbacks) = acknowledgement_callbacks(content, &root) else {
                return false;
            };
            callbacks
        } else if !content.is_empty() {
            return false;
        } else {
            Vec::new()
        };
        let entry = reply.entry;
        match entry {
            Entry::Call(i) => inner.calls[i].context.callbacks.extend(callbacks),
            Entry::Offer(i) => inner.offers[i].context.callbacks.extend(callbacks),
        }
        inner.responses.remove(&key);
        true
    }
}

fn request_fits(account: &Mri, connections: &std::collections::BTreeMap<String, String>, body: &Value) -> bool {
    !actors(body)
        .iter()
        .any(|actor| actor.get("id").and_then(Value::as_str) != Some(account.as_str()))
        && (connections.is_empty()
            || all_callbacks(body).iter().all(|callback| {
                connections
                    .values()
                    .any(|root| callback.starts_with(&format!("{root}callAgent/")))
            }))
}

fn acknowledgement_callbacks(content: &str, root: &str) -> Option<Vec<String>> {
    let ack = serde_json::from_str::<Value>(content).ok()?;
    if !ack.get("callAcceptanceAcknowledgement").is_some_and(Value::is_object) {
        return None;
    }
    let callbacks = all_callbacks(&ack);
    callbacks
        .iter()
        .all(|callback| callback.starts_with(&format!("{root}callAgent/")))
        .then_some(callbacks)
}

fn all_callbacks(value: &Value) -> Vec<String> {
    match value {
        Value::String(text) => super::parse_callback(text)
            .filter(|callback| callback.path.contains("/callAgent/"))
            .map(|_| vec![text.clone()])
            .unwrap_or_default(),
        Value::Array(values) => values.iter().flat_map(all_callbacks).collect(),
        Value::Object(fields) => fields.values().flat_map(all_callbacks).collect(),
        _ => Vec::new(),
    }
}

// Explicit protocol sender positions; recipient identities are never interpreted as senders.
fn actors(body: &Value) -> Vec<&Value> {
    let mut found: Vec<_> = [
        "/participants/from",
        "/from",
        "/sender",
        "/callProgress/sender",
        "/callAcceptance/acceptedBy",
        "/mediaNegotiation/sender",
        "/mediaAnswer/sender",
    ]
    .into_iter()
    .filter_map(|pointer| body.pointer(pointer))
    .collect();
    if let Some(actions) = body.get("additionalActions").and_then(Value::as_array) {
        for input in actions.iter().filter_map(|action| action.get("input")) {
            found.extend(actors(input));
        }
    }
    found
}
