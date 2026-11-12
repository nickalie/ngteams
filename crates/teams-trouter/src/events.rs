//! Deliveries read into typed chat events, from the shapes M1 Task 14 captured (`docs/protocol/trouter.md`,
//! `tests/fixtures/trouter/deliveries.json`): a delivery's body is an `EventMessage` naming its `resourceType` and
//! carrying the `resource`, a message or a conversation as the chat service's reads give them (`teams_proto::chat`).
//!
//! - `NewMessage`: a message ([`ChatEvent::Message`]), or a signal when its `messagetype` is a `Control/…` one:
//!   typing for `Control/Typing` ([`ChatEvent::Typing`]), its end for `Control/ClearTyping`
//!   ([`ChatEvent::TypingStopped`]), any other control [`ChatEvent::Unknown`]. A control is never a message.
//! - `MessageUpdate`: the whole message again with a new `version` ([`ChatEvent::MessageUpdate`]), told apart as the
//!   captures showed: a delete's `deletetime` is its `version`, an edit's `edittime` is its `version`, a reaction
//!   change carries `emotions` (the whole set) and neither.
//! - `ConversationUpdate`: the chat ([`ChatEvent::ConversationUpdate`]), or the account's own read horizon when its
//!   `propertiesUpdated` names `consumptionhorizon` ([`ChatEvent::ReadHorizon`]). A delivery also carrying
//!   `consumptionHorizonBookmark` stays a `ConversationUpdate` so the engine stores both properties. The engine
//!   stores a carried bookmark even when `propertiesUpdated` does not name the horizon.
//! - `CustomUserProperties`: the account's own properties that changed; its mark-all-read horizon when they name one
//!   ([`ChatEvent::AppReadHorizon`], `docs/protocol/chat.md`), else [`ChatEvent::Unknown`].
//! - anything else is [`ChatEvent::Unknown`] with its `resourceType`, and logged by that kind only.
//!
//! A delivery is applied by conversation, message id and version: the service replays unanswered ones on connect
//! (`stored`), so the same event can come more than once.

use serde_json::Value;
use teams_proto::chat::{AppConsumptionHorizon, ConsumptionHorizon, Conversation, Field, Message, MessageType};

use crate::frames::RawDelivery;

/// A chat delivery: its event, whether it is a stored replay, and the service's id for it.
#[derive(Clone, Debug, PartialEq)]
pub struct Delivery {
    /// The socket.io message's `id` (the same for a stored delivery from one connect to the next), signed.
    pub id: i64,
    /// `X-Microsoft-Skype-Message-ID`: at most a hint for telling deliveries apart.
    pub message_id: Option<String>,
    /// A stored delivery, replayed on connect (`"mstore":true`).
    pub stored: bool,
    pub event: ChatEvent,
}

/// A chat event (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub enum ChatEvent {
    /// A new message in `conversation`.
    Message {
        conversation: String,
        message: Box<Message>,
    },
    /// Someone typing in `conversation` (their MRI, when the message names one).
    Typing { conversation: String, from: Option<String> },
    /// Someone stopped typing in `conversation` (`Control/ClearTyping`).
    TypingStopped { conversation: String, from: Option<String> },
    /// A message changed: the whole message as it is now.
    MessageUpdate {
        conversation: String,
        change: Change,
        message: Box<Message>,
    },
    /// The account's own read horizon in `conversation`.
    ReadHorizon {
        conversation: String,
        horizon: ConsumptionHorizon,
    },
    /// The account marked everything read in Teams, or took that back.
    AppReadHorizon(AppConsumptionHorizon),
    /// The chat changed (its last message, its thread properties, …).
    ConversationUpdate { conversation: Box<Conversation> },
    /// A delivery of another kind, or one whose body doesn't read: its `resourceType` (empty when there was none).
    Unknown { resource_type: String },
}

/// What a [`ChatEvent::MessageUpdate`] changed, as the captures tell it apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Its `edittime` is its `version`.
    Edit,
    /// Its `deletetime` is its `version`.
    Delete,
    /// It carries `emotions` and is neither of those.
    Reaction,
    /// None of those.
    Other,
}

impl Delivery {
    /// The delivery's event, from its body.
    #[must_use]
    pub fn from_raw(raw: &RawDelivery) -> Self {
        Self {
            id: raw.id,
            message_id: raw.header("X-Microsoft-Skype-Message-ID").map(str::to_owned),
            stored: raw.is_stored(),
            event: ChatEvent::from_body(&raw.body),
        }
    }
}

impl ChatEvent {
    /// The event an `EventMessage` body names.
    #[must_use]
    pub fn from_body(body: &str) -> Self {
        let Ok(body) = serde_json::from_str::<Value>(body) else {
            return unknown("");
        };
        let resource_type = body["resourceType"].as_str().unwrap_or_default();
        log::debug!("trouter: a delivery of kind {resource_type:?}");
        let resource = &body["resource"];
        let event = match resource_type {
            "NewMessage" => new_message(resource),
            "MessageUpdate" => message_update(resource),
            "ConversationUpdate" => conversation_update(resource),
            "CustomUserProperties" => AppConsumptionHorizon::from_properties(resource).map(ChatEvent::AppReadHorizon),
            _ => None,
        };
        event.unwrap_or_else(|| unknown(resource_type))
    }
}

fn unknown(resource_type: &str) -> ChatEvent {
    log::debug!("trouter: a delivery of kind {resource_type:?} isn't read");
    ChatEvent::Unknown {
        resource_type: resource_type.to_owned(),
    }
}

/// The message and the chat it is in: `to` (as delivered), else the last part of its `conversationLink`.
fn message_in(resource: &Value) -> Option<(String, Message)> {
    let message: Message = serde_json::from_value(resource.clone()).ok()?;
    let conversation = resource["to"]
        .as_str()
        .or_else(|| resource["conversationLink"].as_str()?.rsplit('/').next())
        .filter(|id| !id.is_empty())?
        .to_owned();
    Some((conversation, message))
}

fn new_message(resource: &Value) -> Option<ChatEvent> {
    let (conversation, message) = message_in(resource)?;
    let from = message.sender().map(str::to_owned);
    Some(match &message.message_type {
        MessageType::Typing => ChatEvent::Typing { conversation, from },
        MessageType::ClearTyping => ChatEvent::TypingStopped { conversation, from },
        // a control no capture showed: a signal all the same, never a message
        control if control.is_control() => unknown("NewMessage"),
        _ => ChatEvent::Message {
            conversation,
            message: Box::new(message),
        },
    })
}

fn message_update(resource: &Value) -> Option<ChatEvent> {
    let (conversation, message) = message_in(resource)?;
    Some(ChatEvent::MessageUpdate {
        conversation,
        change: change_of(&message),
        message: Box::new(message),
    })
}

fn change_of(message: &Message) -> Change {
    let version = message.version.as_known().copied();
    let at_version = |time: Field<u64>| version.is_some() && time.known() == version;
    if at_version(message.delete_time()) {
        Change::Delete
    } else if at_version(message.edit_time()) {
        Change::Edit
    } else if !message.reactions().is_absent() {
        Change::Reaction
    } else {
        Change::Other
    }
}

fn conversation_update(resource: &Value) -> Option<ChatEvent> {
    let conversation: Conversation = serde_json::from_value(resource.clone()).ok()?;
    let horizon_updated = resource["propertiesUpdated"]
        .as_array()
        .is_some_and(|names| names.iter().any(|name| name == "consumptionhorizon"));
    // Preserve both properties when a single update carries the feed's bookmark and its seen horizon.
    if horizon_updated
        && conversation.horizon_bookmark().is_none()
        && let Some(horizon) = conversation.consumption_horizon()
    {
        return Some(ChatEvent::ReadHorizon {
            conversation: conversation.id.clone(),
            horizon,
        });
    }
    Some(ChatEvent::ConversationUpdate {
        conversation: Box::new(conversation),
    })
}
