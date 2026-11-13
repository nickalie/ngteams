//! The chat service's answers as the native client reads them (`docs/protocol/chat.md`): the conversation list, message
//! pages, messages with their versions, edits, deletes, reactions, mentions and quotes, threads and member lists.
//!
//! **Reading never fails on what this build doesn't know.** Every object keeps the fields it doesn't name (`extra`,
//! `serde(flatten)`); a message type, a conversation kind or a mention kind it doesn't name is `Unknown` with the text
//! it came as; a property in a form no capture showed is [`Field::Unknown`] with its value, never guessed at. A list
//! entry that isn't even the shape every captured one had (no `id`, a known field of another JSON type) is kept as it
//! came in the page's `unreadable`, and the rest of the page is read.
//!
//! Numbers the service writes sometimes as numbers and sometimes as strings of digits (a message's `id` and `version`,
//! `sequenceId`, a quoted message's `messageId`) are read in either form.

use crate::{is_group_chat_id, one_to_one_pair};
use serde::de::{DeserializeOwned, Deserializer};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::chat::files::{SharedFiles, shared_files};
use crate::chat::html::{self, RichText, ThreadActivity};
use crate::chat::links::{LinkPreview, link_previews};
use crate::chat::policy::PolicyViolation;

/// A value that may be missing, in a form a capture showed ([`Field::Known`]), or in another one, kept as it came
/// ([`Field::Unknown`]). A named field that came as `null` is `Unknown(Value::Null)`: no capture showed one.
///
/// It serializes as the value it holds (`Known` as `T`, `Unknown` as the raw value); a struct field that is `Absent`
/// is left out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Field<T> {
    #[default]
    Absent,
    Known(T),
    Unknown(Value),
}

impl<T> Field<T> {
    /// The known value, if there is one.
    #[must_use]
    pub fn known(self) -> Option<T> {
        match self {
            Self::Known(value) => Some(value),
            Self::Absent | Self::Unknown(_) => None,
        }
    }

    /// The known value by reference, if there is one.
    #[must_use]
    pub const fn as_known(&self) -> Option<&T> {
        match self {
            Self::Known(value) => Some(value),
            Self::Absent | Self::Unknown(_) => None,
        }
    }

    /// Whether the value is missing (neither known nor in another form).
    #[must_use]
    pub const fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }
}

impl<T: Serialize> Serialize for Field<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Absent => serializer.serialize_none(),
            Self::Known(value) => value.serialize(serializer),
            Self::Unknown(raw) => raw.serialize(serializer),
        }
    }
}

/// The conversation list (`GET v1/users/ME/conversations`, or its `syncState` link followed).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConversationList {
    pub conversations: Vec<Conversation>,
    /// Entries that aren't the shape of a conversation, kept as they came.
    pub unreadable: Vec<Value>,
    /// `_metadata` as it came: `totalCount`, `forwardLink`, `syncState`, and anything else.
    pub metadata: Map<String, Value>,
    /// The answer's other top-level fields.
    pub extra: Map<String, Value>,
}

impl ConversationList {
    /// Reads an answer's body: a JSON object, its `conversations` a list (or missing) and its `_metadata` an object
    /// (or missing).
    ///
    /// # Errors
    /// [`Malformed`] for any other body.
    pub fn from_body(body: &[u8]) -> Result<Self, Malformed> {
        const WHAT: &str = "a conversation list";
        let mut top = object_body(body, WHAT)?;
        let (conversations, unreadable) = entries(top.remove("conversations"), WHAT)?;
        Ok(Self {
            conversations,
            unreadable,
            metadata: metadata(&mut top, WHAT)?,
            extra: top,
        })
    }

    /// `_metadata.totalCount`.
    #[must_use]
    pub fn total_count(&self) -> Field<u64> {
        number_field(self.metadata.get("totalCount"))
    }

    /// `_metadata.syncState`: the full URL of the next incremental read, followed only on the same chat base.
    #[must_use]
    pub fn sync_state(&self) -> Option<&str> {
        link(&self.metadata, "syncState")
    }

    /// `_metadata.forwardLink`, the next page, when it isn't empty (an empty one was all a capture showed).
    #[must_use]
    pub fn forward_link(&self) -> Option<&str> {
        link(&self.metadata, "forwardLink")
    }
}

/// A conversation as the list gives it.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Conversation {
    pub id: String,
    /// The conversation's version (ms): raised by every change.
    #[serde(
        default,
        deserialize_with = "number_or_unknown",
        skip_serializing_if = "Field::is_absent"
    )]
    pub version: Field<u64>,
    #[serde(
        rename = "lastUpdatedMessageId",
        default,
        deserialize_with = "number_or_unknown",
        skip_serializing_if = "Field::is_absent"
    )]
    pub last_updated_message_id: Field<u64>,
    #[serde(
        rename = "lastUpdatedMessageVersion",
        default,
        deserialize_with = "number_or_unknown",
        skip_serializing_if = "Field::is_absent"
    )]
    pub last_updated_message_version: Field<u64>,
    /// The last message, without its `properties`; `{}` (here `None`) for an empty chat, and so is one that isn't the
    /// shape of a message (the raw value stays in [`Conversation::last_message_raw`]).
    #[serde(rename = "lastMessage", default)]
    pub last_message_raw: Option<Value>,
    /// The account's own membership: `role`, `interest`, `isReader`, … as they came.
    #[serde(rename = "memberProperties", default)]
    pub member_properties: Map<String, Value>,
    /// `addedBy`, `consumptionhorizon`, `isemptyconversation`, … as they came (booleans mostly as strings).
    #[serde(default)]
    pub properties: Map<String, Value>,
    /// `threadType`, `productThreadType`, `rosterVersion`, `topic`, `topics`, … as they came. A group or meeting chat's
    /// name is `topic` (the web client titles a chat by it); a team's name is `spaceThreadTopic`.
    #[serde(rename = "threadProperties", default)]
    pub thread_properties: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Conversation {
    /// What kind of conversation it is, from `threadProperties.threadType` and the id (see [`ConversationKind`]).
    #[must_use]
    pub fn kind(&self) -> ConversationKind {
        ConversationKind::of(&self.id, self.thread_property("threadType"))
    }

    /// Whether it is anything but a 1:1 or a group chat: the native client keeps no members of it, and writes to it only
    /// as a channel (its posts and replies) when it is one.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.kind().is_read_only()
    }

    /// A thread property that came as a string.
    #[must_use]
    pub fn thread_property(&self, name: &str) -> Option<&str> {
        self.thread_properties.get(name).and_then(Value::as_str)
    }

    /// The chat's name (`threadProperties.topic`), when it has one: a named group chat's or a meeting's title.
    #[must_use]
    pub fn topic(&self) -> Option<&str> {
        named(self.thread_property("topic"))
    }

    /// The roster's version (`threadProperties.rosterVersion`): a number or a string of digits is known, anything else
    /// is kept as it came.
    #[must_use]
    pub fn roster_version(&self) -> Field<u64> {
        number_field(self.thread_properties.get("rosterVersion"))
    }

    /// The last message, if the list gave one of a message's shape.
    #[must_use]
    pub fn last_message(&self) -> Option<Message> {
        let raw = self.last_message_raw.as_ref()?;
        if raw.as_object().is_some_and(Map::is_empty) {
            return None;
        }
        serde_json::from_value(raw.clone()).ok()
    }

    /// Whether the chat service calls the conversation empty (`properties.isemptyconversation`, `"True"` or `"False"` in
    /// every capture): a chat nobody wrote in yet, which Teams leaves out of its chat list. `None` when it is absent or
    /// in another form: not known, never read as either.
    #[must_use]
    pub fn is_empty_conversation(&self) -> Option<bool> {
        match self.properties.get("isemptyconversation").and_then(Value::as_str) {
            Some("True") => Some(true),
            Some("False") => Some(false),
            _ => None,
        }
    }

    /// The account's own read horizon (`properties.consumptionhorizon`), when it has read anything.
    #[must_use]
    pub fn consumption_horizon(&self) -> Option<ConsumptionHorizon> {
        self.properties
            .get("consumptionhorizon")
            .and_then(Value::as_str)
            .map(ConsumptionHorizon::parse)
    }

    /// When the account last marked the whole conversation read (`properties.consumptionHorizonBookmark`,
    /// `0;<ms>;0`, ms): the activity feed's "Mark all as read" (`docs/protocol/chat.md`, "The activity feed");
    /// `None` when it is absent or its time can't be read.
    #[must_use]
    pub fn horizon_bookmark(&self) -> Option<u64> {
        self.properties
            .get("consumptionHorizonBookmark")
            .and_then(Value::as_str)
            .and_then(|raw| ConsumptionHorizon::parse(raw).updated)
    }
}

/// What kind of conversation a thread is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConversationKind {
    /// `threadType` `chat` with a 1:1 chat's id (`19:<oid>_<oid>@unq.gbl.spaces`).
    OneToOne,
    /// `threadType` `chat` with a group chat's id (`19:<id>@thread.v2`, not a meeting's).
    Group,
    /// `threadType` `space`: a team, read as its general channel (its other channels in `topics`); or `topic`: one of
    /// its other channels (M2 Task 16's probe read a standard channel's thread with that type). Its members aren't kept;
    /// its posts, replies and reactions are written as a chat's.
    Channel,
    /// `threadType` `streamofnotifications`: the account's activity feed (`48:notifications`). Read-only.
    Notifications,
    /// Anything else, with the `threadType` it came with (empty when there was none). Read-only: a meeting's chat is
    /// one, since no capture showed its `threadType`.
    Unknown(String),
}

impl ConversationKind {
    /// The kind of conversation `id` whose `threadProperties.threadType` is `thread_type`.
    #[must_use]
    pub fn of(id: &str, thread_type: Option<&str>) -> Self {
        match thread_type {
            Some("chat") if one_to_one_pair(id).is_some() => Self::OneToOne,
            Some("chat") if is_group_chat_id(id) => Self::Group,
            Some("space" | "topic") => Self::Channel,
            Some("streamofnotifications") => Self::Notifications,
            other => Self::Unknown(other.unwrap_or_default().to_owned()),
        }
    }

    /// Anything but a 1:1 or a group chat.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        !matches!(self, Self::OneToOne | Self::Group)
    }
}

/// A read horizon, `<arrival id>;<ms>;<clientmessageid>`: the last message read and when the horizon was set. A part
/// that isn't the form a capture showed is `None`; `raw` keeps the whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsumptionHorizon {
    /// The last message read: its id (its arrival time, ms).
    pub last_read_arrival: Option<u64>,
    /// When the horizon was set (ms).
    pub updated: Option<u64>,
    /// The last message read's `clientmessageid`.
    pub client_message_id: Option<String>,
    pub raw: String,
}

impl ConsumptionHorizon {
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let parts: Vec<&str> = raw.split(';').collect();
        let (arrival, updated, client) = match parts.as_slice() {
            [arrival, updated, client] => (arrival.parse().ok(), updated.parse().ok(), Some(*client)),
            _ => (None, None, None),
        };
        Self {
            last_read_arrival: arrival,
            updated,
            client_message_id: client.filter(|id| !id.is_empty()).map(str::to_owned),
            raw: raw.to_owned(),
        }
    }
}

/// Where the account last marked everything read in Teams (milliseconds): the account's own property
/// `appConsumptionHorizon`, `{"chat": <ms>, "channel": <ms>, "engage_community": <ms>}`, read as the web client reads
/// it: as an object or as that object's JSON text, with `chat` or `channel` there and every value a number (`docs/
/// protocol/chat.md`, "Which chats are unread"). The one it left out is 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AppConsumptionHorizon {
    pub chat: i64,
    pub channel: i64,
}

impl AppConsumptionHorizon {
    /// The account's horizon among its `properties` (a read's whole answer, or a delivery's changed ones). `None` when
    /// they don't name one, or it isn't the form above.
    #[must_use]
    pub fn from_properties(properties: &Value) -> Option<Self> {
        let value = &properties["appConsumptionHorizon"];
        let parsed;
        let value = match value.as_str() {
            Some(text) => {
                parsed = serde_json::from_str::<Value>(text).ok()?;
                &parsed
            }
            None => value,
        };
        let times = value.as_object()?;
        let known = times.contains_key("chat") || times.contains_key("channel");
        let numbers = times.values().all(|time| time.as_i64().is_some_and(|time| time >= 0));
        (known && numbers).then(|| Self {
            chat: times.get("chat").and_then(Value::as_i64).unwrap_or_default(),
            channel: times.get("channel").and_then(Value::as_i64).unwrap_or_default(),
        })
    }
}

/// A page of messages: the first page, a sync read, a backward page or the read around a time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MessagePage {
    /// Newest first, as the service orders them.
    pub messages: Vec<Message>,
    /// Entries that aren't the shape of a message, kept as they came.
    pub unreadable: Vec<Value>,
    /// `_metadata` as it came: `syncState`, `backwardLink`, `forwardLink`, the segment times, and anything else.
    pub metadata: Map<String, Value>,
    /// `tenantId` and any other top-level field.
    pub extra: Map<String, Value>,
}

impl MessagePage {
    /// Reads an answer's body: a JSON object, its `messages` a list (or missing) and its `_metadata` an object (or
    /// missing).
    ///
    /// # Errors
    /// [`Malformed`] for any other body.
    pub fn from_body(body: &[u8]) -> Result<Self, Malformed> {
        const WHAT: &str = "a message page";
        let mut top = object_body(body, WHAT)?;
        let (messages, unreadable) = entries(top.remove("messages"), WHAT)?;
        Ok(Self {
            messages,
            unreadable,
            metadata: metadata(&mut top, WHAT)?,
            extra: top,
        })
    }

    /// `_metadata.syncState`: the full URL of the next sync read.
    #[must_use]
    pub fn sync_state(&self) -> Option<&str> {
        link(&self.metadata, "syncState")
    }

    /// `_metadata.backwardLink`: the next older page (seen on a newest page with more behind it, and on a sync answer
    /// longer than a page: `docs/protocol/chat.md`, "Sync answers").
    #[must_use]
    pub fn backward_link(&self) -> Option<&str> {
        link(&self.metadata, "backwardLink")
    }

    /// `_metadata.forwardLink`: the newer messages (the bundle's name; never captured).
    #[must_use]
    pub fn forward_link(&self) -> Option<&str> {
        link(&self.metadata, "forwardLink")
    }

    /// `_metadata.lastCompleteSegmentStartTime` (ms): how far back the complete segment reaches, on a page of a sync
    /// answer its oldest message's id, or the start of the segment it merged into (0 seen on the read around a time).
    #[must_use]
    pub fn last_complete_segment_start(&self) -> Field<u64> {
        number_field(self.metadata.get("lastCompleteSegmentStartTime"))
    }

    /// `_metadata.lastCompleteSegmentEndTime` (ms): the newest version the segment is complete to.
    #[must_use]
    pub fn last_complete_segment_end(&self) -> Field<u64> {
        number_field(self.metadata.get("lastCompleteSegmentEndTime"))
    }
}

/// A message (`type` `Message`), or a thread's event in the same list.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Message {
    /// Its arrival time (ms) as the service names it.
    #[serde(deserialize_with = "string_or_number")]
    pub id: String,
    /// 1, 2, … in the chat.
    #[serde(
        rename = "sequenceId",
        default,
        deserialize_with = "number_or_unknown",
        skip_serializing_if = "Field::is_absent"
    )]
    pub sequence_id: Field<u64>,
    /// Raised by every edit, delete and reaction change (ms).
    #[serde(
        default,
        deserialize_with = "number_or_unknown",
        skip_serializing_if = "Field::is_absent"
    )]
    pub version: Field<u64>,
    #[serde(rename = "conversationid", default)]
    pub conversation_id: Option<String>,
    #[serde(rename = "messagetype", default)]
    pub message_type: MessageType,
    /// As it came: HTML for `RichText/Html`, XML for a thread's event, empty once deleted.
    #[serde(default, deserialize_with = "string_or_null")]
    pub content: String,
    /// `…/contacts/<MRI>`: see [`Message::sender`].
    #[serde(default)]
    pub from: Option<String>,
    /// The sender's own id for it, kept across edits and deletes; none on a thread's event.
    #[serde(
        rename = "clientmessageid",
        default,
        deserialize_with = "text_or_unknown",
        skip_serializing_if = "Field::is_absent"
    )]
    pub client_message_id: Field<String>,
    /// The name the sender's client gave.
    #[serde(rename = "imdisplayname", default)]
    pub display_name: Option<String>,
    /// The name from the sender's token.
    #[serde(rename = "fromDisplayNameInToken", default)]
    pub display_name_in_token: Option<String>,
    /// ISO time, seven fraction digits.
    #[serde(rename = "composetime", default)]
    pub compose_time: Option<String>,
    /// ISO time, seven fraction digits.
    #[serde(rename = "originalarrivaltime", default)]
    pub arrival_time: Option<String>,
    /// Seen only on a deleted message, set to its `clientmessageid`.
    #[serde(rename = "skypeeditedid", default)]
    pub skype_edited_id: Option<String>,
    /// References to the message's AMS objects (images): only `[]` was ever seen, so the entries are kept as they come.
    #[serde(rename = "amsreferences", default)]
    pub ams_references: Option<Vec<Value>>,
    /// `{}`, or a count per reaction key (`{"emotions": {"like": 1}}`).
    #[serde(rename = "annotationsSummary", default)]
    pub annotations_summary: Option<Value>,
    /// `mentions`, `files`, `emotions`, `edittime`, `deletetime`, `qtdMsgs`, … as they came.
    #[serde(default)]
    pub properties: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Message {
    /// The sender's MRI: the last segment of `from` (`…/contacts/<MRI>`). For a thread's event it is the thread's id.
    #[must_use]
    pub fn sender(&self) -> Option<&str> {
        let from = self.from.as_deref()?;
        let (_, last) = from.rsplit_once("/contacts/")?;
        (!last.is_empty() && !last.contains('/')).then_some(last)
    }

    /// The post a channel's message belongs to (its reply chain's root): the `;messageid=<post>` its `conversationLink`
    /// ends with (`…/conversations/<channel>;messageid=<post>`), when the link names `conversation` and the post is an
    /// id of digits. M2 Task 16's probe saw every message of a channel's page carry one: a post names itself, a reply
    /// its post. `None` for anything else (a chat's message, whose link names the chat alone).
    #[must_use]
    pub fn reply_root(&self, conversation: &str) -> Option<String> {
        let link = self.extra.get("conversationLink")?.as_str()?;
        let (_, last) = link.rsplit_once("/conversations/")?;
        let last = percent_encoding::percent_decode_str(last).decode_utf8().ok()?;
        let (channel, post) = last.split_once(";messageid=")?;
        let digits = !post.is_empty() && post.bytes().all(|byte| byte.is_ascii_digit());
        (channel == conversation && digits).then(|| post.to_owned())
    }

    /// `originalarrivaltime` in ms since the epoch.
    #[must_use]
    pub fn arrival_time_ms(&self) -> Option<i64> {
        self.arrival_time.as_deref().and_then(iso_millis)
    }

    /// `composetime` in ms since the epoch.
    #[must_use]
    pub fn compose_time_ms(&self) -> Option<i64> {
        self.compose_time.as_deref().and_then(iso_millis)
    }

    /// `properties.edittime` (ms, set by the service): the message was edited.
    #[must_use]
    pub fn edit_time(&self) -> Field<u64> {
        number_field(self.properties.get("edittime"))
    }

    /// `properties.deletetime` (ms): the message was deleted.
    #[must_use]
    pub fn delete_time(&self) -> Field<u64> {
        number_field(self.properties.get("deletetime"))
    }

    /// A soft-deleted message: `properties.deletetime` a time. A `deletetime` in a form no capture showed isn't read
    /// as a delete either way: [`Message::delete_time`] has it as [`Field::Unknown`].
    #[must_use]
    pub fn is_deleted(&self) -> bool {
        matches!(self.delete_time(), Field::Known(_))
    }

    /// `properties.emotions`: one entry per reaction key, a key taken back left with no users.
    #[must_use]
    pub fn reactions(&self) -> Field<Vec<Reaction>> {
        match self.properties.get("emotions") {
            None => Field::Absent,
            Some(value @ Value::Array(_)) => list_of(value),
            Some(other) => Field::Unknown(other.clone()),
        }
    }

    /// `properties.mentions`, read as a JSON string (as captured, sent and read back: `docs/protocol/chat.md`) or as a
    /// list (the bundle's reader takes both; not seen). An entry of another form makes it all [`Field::Unknown`].
    #[must_use]
    pub fn mentions(&self) -> Field<Vec<Mention>> {
        json_list(self.properties.get("mentions"))
    }

    /// `properties.qtdMsgs`, the messages this one quotes: a JSON string as sent, a list as read back.
    #[must_use]
    pub fn quotes(&self) -> Field<Vec<QuotedMessage>> {
        json_list(self.properties.get("qtdMsgs"))
    }

    /// `properties.originalMessageContext`: where a forwarded message came from ([`forwarded`]).
    #[must_use]
    pub fn forwarded(&self) -> Field<Forwarded> {
        forwarded(&self.properties)
    }

    /// `properties.files`, a JSON string of a list: received files ([`shared_files`]), each entry read only in the
    /// shape `docs/protocol/chat.md` records ("Files: received"); any other is kept as it came, never guessed at (and
    /// nothing is fetched here).
    #[must_use]
    pub fn files(&self) -> SharedFiles {
        shared_files(self.properties.get("files"))
    }

    /// `properties.policyViolation`: what a tenant's policy did to the message ([`PolicyViolation::of`]).
    #[must_use]
    pub fn policy_violation(&self) -> Field<PolicyViolation> {
        PolicyViolation::of(self.properties.get("policyViolation"))
    }

    /// `properties.links`, a JSON string of a list: the previews of the links the message carries ([`link_previews`]),
    /// each entry read only in the shape `docs/protocol/chat.md` records ("Link previews"); any other is left out.
    #[must_use]
    pub fn link_previews(&self) -> Vec<LinkPreview> {
        link_previews(self.properties.get("links"))
    }

    /// What the message carries beside its text that this build can't show ([`unshown_parts`]): file entries it can't
    /// read as a file, cards and AMS objects (`amsreferences` with an entry), each kept as it came. Empty for a message
    /// every captured message was like, and for one whose files are all read ([`Message::files`]).
    #[must_use]
    pub fn unshown(&self) -> Vec<Unshown> {
        let mut parts = unshown_parts(&self.properties);
        if let Some(references) = self.ams_references.as_ref().filter(|references| !references.is_empty()) {
            parts.push(Unshown::AmsReferences(Value::Array(references.clone())));
        }
        parts
    }

    /// The content as rich text: parsed HTML for `RichText/Html`, the text as it came for `Text`; `None` for other
    /// types (a thread's event, typing, anything unknown).
    #[must_use]
    pub fn rich_text(&self) -> Option<RichText> {
        match self.message_type {
            MessageType::RichTextHtml => Some(html::parse(&self.content)),
            MessageType::Text => Some(RichText::plain(&self.content)),
            _ => None,
        }
    }

    /// A thread's event (`ThreadActivity/…`), its XML content read: who did it, to whom, the roster version.
    #[must_use]
    pub fn thread_activity(&self) -> Option<ThreadActivity> {
        matches!(self.message_type, MessageType::AddMember)
            .then(|| html::thread_activity(&self.content))
            .flatten()
    }
}

/// A message's `messagetype`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(from = "String", into = "String")]
pub enum MessageType {
    /// `Text`: plain text (seen in the notifications stream).
    Text,
    /// `RichText/Html`: what the web client posts.
    RichTextHtml,
    /// `ThreadActivity/AddMember`: members added (a chat's first event).
    AddMember,
    /// `Control/Typing`: typing (seen sent, never in a read).
    Typing,
    /// `Control/ClearTyping`: typing stopped (seen sent by the web client, never in a read).
    ClearTyping,
    /// Any other, as it came (empty when there was none).
    Unknown(String),
}

impl MessageType {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Text => "Text",
            Self::RichTextHtml => "RichText/Html",
            Self::AddMember => "ThreadActivity/AddMember",
            Self::Typing => "Control/Typing",
            Self::ClearTyping => "Control/ClearTyping",
            Self::Unknown(text) => text,
        }
    }

    /// A `Control/…` type: a signal (typing, its end, any other control), never a message to keep or show.
    #[must_use]
    pub fn is_control(&self) -> bool {
        self.as_str().starts_with("Control/")
    }
}

impl Default for MessageType {
    fn default() -> Self {
        Self::Unknown(String::new())
    }
}

impl From<String> for MessageType {
    fn from(text: String) -> Self {
        match text.as_str() {
            "Text" => Self::Text,
            "RichText/Html" => Self::RichTextHtml,
            "ThreadActivity/AddMember" => Self::AddMember,
            "Control/Typing" => Self::Typing,
            "Control/ClearTyping" => Self::ClearTyping,
            _ => Self::Unknown(text),
        }
    }
}

impl From<MessageType> for String {
    fn from(kind: MessageType) -> Self {
        kind.as_str().to_owned()
    }
}

/// One reaction key and who reacted with it.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Reaction {
    pub key: String,
    #[serde(default)]
    pub users: Vec<ReactionUser>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A user's standing reaction.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct ReactionUser {
    pub mri: String,
    /// The service's time (ms).
    #[serde(
        default,
        deserialize_with = "number_or_unknown",
        skip_serializing_if = "Field::is_absent"
    )]
    pub time: Field<u64>,
    /// The value the reacting client sent, as a string.
    #[serde(
        default,
        deserialize_with = "text_or_unknown",
        skip_serializing_if = "Field::is_absent"
    )]
    pub value: Field<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One entry of `properties.mentions`, as captured (`mention.json`: `itemid` a string as sent, a number as read back;
/// `mentionType` `person`). Its `item_id` is the `itemid` of the mention's span in the content
/// ([`crate::chat::Node::Mention`]).
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Mention {
    #[serde(rename = "itemid", deserialize_with = "string_or_number")]
    pub item_id: String,
    #[serde(default)]
    pub mri: Option<String>,
    #[serde(rename = "mentionType", default)]
    pub kind: MentionKind,
    #[serde(rename = "displayName", default)]
    pub display_name: Option<String>,
    /// `@type`, `appId` (a bot's), and anything else.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A mention's `mentionType`, as the web bundle names them.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(from = "String", into = "String")]
pub enum MentionKind {
    Person,
    Bot,
    Everyone,
    Tag,
    Channel,
    Team,
    /// Any other (the bundle also names `community`, `nearby`, `share-contact`), as it came; empty when there was none.
    Unknown(String),
}

impl MentionKind {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Person => "person",
            Self::Bot => "bot",
            Self::Everyone => "everyone",
            Self::Tag => "tag",
            Self::Channel => "channel",
            Self::Team => "team",
            Self::Unknown(text) => text,
        }
    }
}

impl Default for MentionKind {
    fn default() -> Self {
        Self::Unknown(String::new())
    }
}

impl From<String> for MentionKind {
    fn from(text: String) -> Self {
        match text.as_str() {
            "person" => Self::Person,
            "bot" => Self::Bot,
            "everyone" => Self::Everyone,
            "tag" => Self::Tag,
            "channel" => Self::Channel,
            "team" => Self::Team,
            _ => Self::Unknown(text),
        }
    }
}

impl From<MentionKind> for String {
    fn from(kind: MentionKind) -> Self {
        kind.as_str().to_owned()
    }
}

/// One entry of `properties.qtdMsgs`: a message this one quotes.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct QuotedMessage {
    /// A number as read back, a string as sent.
    #[serde(rename = "messageId", deserialize_with = "string_or_number")]
    pub message_id: String,
    #[serde(default)]
    pub sender: Option<String>,
    #[serde(
        default,
        deserialize_with = "number_or_unknown",
        skip_serializing_if = "Field::is_absent"
    )]
    pub time: Field<u64>,
    /// `Valid` as read back.
    #[serde(rename = "validationResult", default)]
    pub validation_result: Option<String>,
    /// `message`, `sharedRefId`, `replyChainId`, and anything else.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Where a forwarded message came from: its `properties.originalMessageContext`, an object as read back and as
/// delivered (`docs/protocol/chat.md`, "Forwarded messages"). Its content is a quote of itemtype Forward holding the
/// original message's content ([`crate::chat::Quote::forward`]).
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Forwarded {
    /// `originalSender`: the original message's sender's MRI.
    #[serde(rename = "originalSender", default, deserialize_with = "present_string")]
    pub sender: Option<String>,
    /// `originalSentTime`: when the original was sent, an ISO time (three fraction digits as captured).
    #[serde(rename = "originalSentTime", default, deserialize_with = "present_string")]
    pub sent_time: Option<String>,
    /// `messageId`: the original message's id, a number as captured (read as its digits).
    #[serde(rename = "messageId", default, deserialize_with = "optional_string_or_number")]
    pub message_id: Option<String>,
    /// `originalThreadId`: the conversation the original was sent in.
    #[serde(rename = "originalThreadId", default, deserialize_with = "present_string")]
    pub thread_id: Option<String>,
    /// `threadType` (`chat` as captured), `messageType`, and anything else.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Forwarded {
    /// `originalSentTime` in ms since the epoch.
    #[must_use]
    pub fn sent_time_ms(&self) -> Option<i64> {
        self.sent_time.as_deref().and_then(iso_millis)
    }
}

/// Where the message whose `properties` these are was forwarded from (`originalMessageContext`): an object of the
/// captured shape is [`Field::Known`]; any other form (a JSON string, a field of another JSON type, `null` included)
/// is kept as it came, never guessed at. [`Field::Absent`] for a message that wasn't forwarded.
#[must_use]
pub fn forwarded(properties: &Map<String, Value>) -> Field<Forwarded> {
    match properties.get("originalMessageContext") {
        None => Field::Absent,
        Some(context @ Value::Object(_)) => {
            serde_json::from_value(context.clone()).map_or_else(|_| Field::Unknown(context.clone()), Field::Known)
        }
        Some(other) => Field::Unknown(other.clone()),
    }
}

/// A thread read (`GET v1/threads/<id>`).
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Thread {
    pub id: String,
    #[serde(default)]
    pub members: Vec<ThreadMember>,
    /// `threadType`, `createdat`, `creator`, `capabilities`, … as they came.
    #[serde(default)]
    pub properties: Map<String, Value>,
    #[serde(
        rename = "rosterVersion",
        default,
        deserialize_with = "number_or_unknown",
        skip_serializing_if = "Field::is_absent"
    )]
    pub roster_version: Field<u64>,
    #[serde(
        default,
        deserialize_with = "number_or_unknown",
        skip_serializing_if = "Field::is_absent"
    )]
    pub version: Field<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Thread {
    /// What kind of conversation it is, from `properties.threadType` and the id.
    #[must_use]
    pub fn kind(&self) -> ConversationKind {
        ConversationKind::of(&self.id, self.properties.get("threadType").and_then(Value::as_str))
    }

    /// The chat's name (`properties.topic`), when it has one.
    #[must_use]
    pub fn topic(&self) -> Option<&str> {
        named(self.properties.get("topic").and_then(Value::as_str))
    }
}

/// A topic that names something: not empty, nor only whitespace.
fn named(topic: Option<&str>) -> Option<&str> {
    topic.filter(|topic| !topic.trim().is_empty())
}

/// A member of a thread, as the thread read and the member list give it.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct ThreadMember {
    /// The member's MRI.
    pub id: String,
    #[serde(default)]
    pub role: Option<String>,
    /// `capabilities`, `userLink`, `rangeStart`, … as they came.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A thread's member list (`GET v1/threads/<id>/members`). Only an unpaged one was ever seen.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct MemberList {
    #[serde(default)]
    pub members: Vec<ThreadMember>,
    #[serde(
        rename = "totalMemberCount",
        default,
        deserialize_with = "number_or_unknown",
        skip_serializing_if = "Field::is_absent"
    )]
    pub total_member_count: Field<u64>,
    #[serde(
        rename = "rosterVersion",
        default,
        deserialize_with = "number_or_unknown",
        skip_serializing_if = "Field::is_absent"
    )]
    pub roster_version: Field<u64>,
    /// `eTag`, `isDeleted`, any paging key, and anything else.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Why an answer couldn't be read as its model. It names what was expected, never the body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the chat service's answer isn't the shape of {0}")]
pub struct Malformed(pub &'static str);

/// A single object (a thread, a member list) from a body.
pub(crate) fn object<T: DeserializeOwned>(body: &[u8], what: &'static str) -> Result<T, Malformed> {
    serde_json::from_slice(body).map_err(|_| Malformed(what))
}

fn object_body(body: &[u8], what: &'static str) -> Result<Map<String, Value>, Malformed> {
    match serde_json::from_slice(body) {
        Ok(Value::Object(top)) => Ok(top),
        _ => Err(Malformed(what)),
    }
}

/// The page's list, each entry read on its own: those that aren't the model's shape are kept apart. A missing list is
/// an empty one; a list that isn't a list is malformed.
fn entries<T: DeserializeOwned>(list: Option<Value>, what: &'static str) -> Result<(Vec<T>, Vec<Value>), Malformed> {
    let list = match list {
        None => return Ok((Vec::new(), Vec::new())),
        Some(Value::Array(list)) => list,
        Some(_) => return Err(Malformed(what)),
    };
    let mut read = Vec::new();
    let mut unreadable = Vec::new();
    for entry in list {
        match serde_json::from_value(entry.clone()) {
            Ok(value) => read.push(value),
            Err(_) => unreadable.push(entry),
        }
    }
    Ok((read, unreadable))
}

fn metadata(top: &mut Map<String, Value>, what: &'static str) -> Result<Map<String, Value>, Malformed> {
    match top.remove("_metadata") {
        None => Ok(Map::new()),
        Some(Value::Object(metadata)) => Ok(metadata),
        Some(_) => Err(Malformed(what)),
    }
}

/// A link in `metadata`, when it is a non-empty string.
fn link<'m>(metadata: &'m Map<String, Value>, name: &str) -> Option<&'m str> {
    metadata
        .get(name)
        .and_then(Value::as_str)
        .filter(|link| !link.is_empty())
}

/// A number as the service writes it: a JSON number, or a string of digits.
fn number_field(value: Option<&Value>) -> Field<u64> {
    match value {
        None => Field::Absent,
        Some(value) => number(value).map_or_else(|| Field::Unknown(value.clone()), Field::Known),
    }
}

fn number(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) => text.parse().ok(),
        _ => None,
    }
}

/// A part of a message no capture showed, kept as it came: what [`Message::unshown`] and [`unshown_parts`] find. A
/// view shows that the message has content it can't show, never a guess at it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unshown {
    /// `properties.files` entries this build can't read ([`SharedFiles::unread`]: not a file's in the shape
    /// `docs/protocol/chat.md` records), or a `files` that isn't a list.
    Files(Value),
    /// `properties.cards` with an entry, or in a form that isn't a list (only `"[]"` was captured; a card comes from a
    /// bot or a connector, none in the lab).
    Cards(Value),
    /// `amsreferences` with an entry (only `[]` was captured: an image's message never went out in M1 Task 20's
    /// `ab-image`).
    AmsReferences(Value),
}

impl Unshown {
    /// The name of the part, for a trace: `files`, `cards`, `amsreferences`.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Files(_) => "files",
            Self::Cards(_) => "cards",
            Self::AmsReferences(_) => "amsreferences",
        }
    }
}

/// What a message's `properties` (`properties`, as the message carried them or the store keeps them) hold that this
/// build can't show: `files` entries it can't read ([`SharedFiles::unread`]), and `cards` other than an empty list (a
/// JSON string of one, as captured, or a list), each kept as it came. A missing one is nothing to show.
#[must_use]
pub fn unshown_parts(properties: &Map<String, Value>) -> Vec<Unshown> {
    let unknown = |key: &str| match empty_list(properties.get(key)) {
        Field::Unknown(raw) => Some(raw),
        Field::Absent | Field::Known(_) => None,
    };
    let mut parts = Vec::new();
    if let Some(raw) = shared_files(properties.get("files")).unread {
        parts.push(Unshown::Files(raw));
    }
    if let Some(raw) = unknown("cards") {
        parts.push(Unshown::Cards(raw));
    }
    parts
}

/// The web client's origin, where a message this build can't show in full is read ([`web_chat_link`]).
pub const WEB_CLIENT: &str = "https://teams.microsoft.com";

/// The web client's link that opens the conversation `conversation`: `<WEB_CLIENT>/l/chat/<id>/0`, the id encoded as
/// one segment exactly as `abc-chat`'s `chat_link_by_id` encodes it (`application/x-www-form-urlencoded`: letters,
/// digits and `*-._` kept, every other byte escaped, `:`, `@` and `/` included) and, like it, without the `?tenantId`
/// the captured form carries: the deep link `abc-chat` opens the lab's existing group chat by
/// (`docs/protocol/endpoints.md`). Opened in the browser, never fetched.
#[must_use]
pub fn web_chat_link(conversation: &str) -> String {
    let id: String = url::form_urlencoded::byte_serialize(conversation.as_bytes()).collect();
    format!("{WEB_CLIENT}/l/chat/{id}/0")
}

/// A list property only ever captured empty, as a list or a JSON string of one: an empty list is known, anything else
/// (a list with an entry, another form) is kept as it came.
fn empty_list(value: Option<&Value>) -> Field<Vec<Value>> {
    match json_list::<Value>(value) {
        Field::Known(list) if !list.is_empty() => Field::Unknown(value.cloned().unwrap_or(Value::Null)),
        other => other,
    }
}

/// A list property, as a list or a JSON string of one; any entry not of `T`'s shape makes it all unknown.
fn json_list<T: DeserializeOwned>(value: Option<&Value>) -> Field<Vec<T>> {
    match value {
        None => Field::Absent,
        Some(Value::String(text)) => match serde_json::from_str::<Value>(text) {
            Ok(list @ Value::Array(_)) => list_of(&list).or_unknown(Value::String(text.clone())),
            _ => Field::Unknown(Value::String(text.clone())),
        },
        Some(list @ Value::Array(_)) => list_of(list),
        Some(other) => Field::Unknown(other.clone()),
    }
}

fn list_of<T: DeserializeOwned>(list: &Value) -> Field<Vec<T>> {
    serde_json::from_value(list.clone()).map_or_else(|_| Field::Unknown(list.clone()), Field::Known)
}

impl<T> Field<T> {
    /// An unknown value replaced with `raw` (the form it came in).
    fn or_unknown(self, raw: Value) -> Self {
        match self {
            Self::Unknown(_) => Self::Unknown(raw),
            other => other,
        }
    }
}

/// An ISO time (RFC 3339, any number of fraction digits) in ms since the epoch.
#[must_use]
pub fn iso_millis(text: &str) -> Option<i64> {
    let time = OffsetDateTime::parse(text, &Rfc3339).ok()?;
    i64::try_from(time.unix_timestamp_nanos() / 1_000_000).ok()
}

/// A named number field: a number or a string of digits is known; anything else (`null` too) is kept as it came,
/// never an error. A missing field is [`Field::Absent`] (`serde(default)`).
fn number_or_unknown<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Field<u64>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(match number(&value) {
        Some(number) => Field::Known(number),
        None => Field::Unknown(value),
    })
}

/// A named text field: a string, or a whole non-negative number as its digits, is known; anything else (`null` too)
/// is kept as it came, never an error. A missing field is [`Field::Absent`] (`serde(default)`).
fn text_or_unknown<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Field<String>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::String(text) => Field::Known(text),
        Value::Number(number) if number.is_u64() => Field::Known(number.to_string()),
        other => Field::Unknown(other),
    })
}

/// A string, or a number as its text; anything else is an error (the entry isn't the model's shape).
fn string_or_number<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::String(text) => Ok(text),
        Value::Number(number) => Ok(number.to_string()),
        _ => Err(serde::de::Error::custom("not a string or a number")),
    }
}

/// A string, for a field that may be missing (`serde(default)`); anything else, `null` too, is an error (the entry
/// isn't the model's shape).
fn present_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

/// As [`string_or_number`], for a field that may be missing (`serde(default)`); `null` is an error too.
fn optional_string_or_number<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    string_or_number(deserializer).map(Some)
}

/// A string; `null` is an empty one. Anything else is an error.
fn string_or_null<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}
