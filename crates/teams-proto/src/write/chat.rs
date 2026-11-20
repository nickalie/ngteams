//! Typed message, edit, reaction, receipt, typing, chat-creation and chat-membership requests. `MessageText` keeps the composed wire form.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::Mri;
use serde_json::{Map, Value, json};
use time::OffsetDateTime;

use super::Prepared;
use super::Wire;
use crate::chat::MessageText;
use crate::endpoint::{Endpoint, Method, Service};

/// Who writes: the account's MRI and the name its messages carry (`imdisplayname`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Author {
    mri: Mri,
    display_name: String,
}

impl Author {
    #[must_use]
    pub fn new(mri: Mri, display_name: impl Into<String>) -> Self {
        Self {
            mri,
            display_name: display_name.into(),
        }
    }

    #[must_use]
    pub fn mri(&self) -> &Mri {
        &self.mri
    }

    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }
}

/// Posts a message to an existing chat: `POST v1/users/ME/conversations/<chat>/messages`, the message as the web
/// client posts it (`mention.json`, `reply.json`). Its target is [`WriteKind::Message`] on the chat, adding whoever
/// its text names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendMessage {
    conversation: String,
    author: Author,
    text: MessageText,
    compose_time_ms: i64,
    client_message_id: Option<String>,
}

impl SendMessage {
    /// `text` from `author` to `conversation`, composed now.
    #[must_use]
    pub fn new(conversation: impl Into<String>, author: Author, text: MessageText) -> Self {
        Self {
            conversation: conversation.into(),
            author,
            text,
            compose_time_ms: now_ms(),
            client_message_id: None,
        }
    }

    /// The same message composed at `time_ms` (milliseconds since the Unix epoch): its `composetime` and
    /// `originalarrivaltime` as sent.
    #[must_use]
    pub fn composed_at(mut self, time_ms: i64) -> Self {
        self.compose_time_ms = time_ms;
        self
    }

    /// The same message posted with `clientmessageid` `id`: the id the outbox gives a command and keeps across its
    /// attempts, by which the message is recognised when it comes back. It isn't part of the target. The web client
    /// always sends one; a post without it leaves the key out.
    #[must_use]
    pub fn with_client_message_id(mut self, id: impl Into<String>) -> Self {
        self.client_message_id = Some(id.into());
        self
    }

    /// The `clientmessageid` it is posted with, if any.
    #[must_use]
    pub fn client_message_id(&self) -> Option<&str> {
        self.client_message_id.as_deref()
    }

    /// The conversation id the message goes to.
    #[must_use]
    pub fn conversation(&self) -> &str {
        &self.conversation
    }

    #[must_use]
    pub fn author(&self) -> &Author {
        &self.author
    }

    #[must_use]
    pub fn text(&self) -> &MessageText {
        &self.text
    }

    /// When it was composed, in milliseconds since the Unix epoch.
    #[must_use]
    pub fn compose_time_ms(&self) -> i64 {
        self.compose_time_ms
    }
}

/// The message as the web client posts it (`mention.json`, `reply.json`): `id` `-1`, `version` `0`, the
/// `conversationLink` it sends (`blah/<chat>`), and `clientmessageid` when the request has one.
impl Prepared for SendMessage {
    fn wire(&self) -> Option<Wire> {
        // a mention of everyone in another chat would reach beyond the target: no route for it
        if !self.text.mentions_everyone_only_in(&self.conversation) {
            return None;
        }
        let compose_time = iso_time(self.compose_time_ms);
        let mut body = message_body(&MessageFields {
            id: "-1",
            conversation: &self.conversation,
            link: format!("blah/{}", self.conversation),
            from: self.author.mri.as_str().to_owned(),
            author: &self.author,
            compose_time: &compose_time,
            arrival_time: &compose_time,
            version: "0".to_owned(),
            text: &self.text,
            client_message_id: self.client_message_id.as_deref(),
        });
        if let Some(fields) = body.as_object_mut() {
            fields.insert("properties".to_owned(), Value::Object(properties(&self.text)));
        }
        Some(Wire {
            raw: None,
            service: Service::ChatService,
            headers: Vec::new(),
            method: Method::Post,
            endpoint: messages(&self.conversation),
            body: Some(body),
        })
    }
}

/// What an edit keeps of the message it changes, as the web client sends it again: its `clientmessageid` and its
/// times, in milliseconds since the Unix epoch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Original {
    pub client_message_id: String,
    pub compose_time_ms: i64,
    pub arrival_time_ms: i64,
}

/// Edits one of the author's messages: `PUT v1/users/ME/conversations/<chat>/messages/<id>` with the whole message
/// again (`edit.json`). Its target is [`WriteKind::Edit`] on the chat, adding whoever the new text names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditMessage {
    conversation: String,
    message_id: String,
    author: Author,
    text: MessageText,
    original: Original,
    attachment_properties: Map<String, Value>,
    edit_time_ms: i64,
}

impl EditMessage {
    /// `author`'s message `message_id` (the [`Original`] it was sent as) in `conversation`, its text now `text`, edited
    /// now.
    #[must_use]
    pub fn new(
        conversation: impl Into<String>,
        message_id: impl Into<String>,
        author: Author,
        text: MessageText,
        original: Original,
    ) -> Self {
        Self {
            conversation: conversation.into(),
            message_id: message_id.into(),
            author,
            text,
            original,
            attachment_properties: Map::new(),
            edit_time_ms: now_ms(),
        }
    }

    /// The same edit made at `time_ms`: its `version` and `properties.edittime` as sent.
    #[must_use]
    pub fn edited_at(mut self, time_ms: i64) -> Self {
        self.edit_time_ms = time_ms;
        self
    }

    /// Keeps the original message's opaque `files` and `cards` properties through a text edit. Other keys are
    /// ignored: mentions, quotes and the edit time still come from this edit.
    #[must_use]
    pub fn with_original_attachments(mut self, properties: Map<String, Value>) -> Self {
        self.attachment_properties = properties
            .into_iter()
            .filter(|(key, _)| matches!(key.as_str(), "files" | "cards"))
            .collect();
        self
    }

    #[must_use]
    pub fn conversation(&self) -> &str {
        &self.conversation
    }

    #[must_use]
    pub fn message_id(&self) -> &str {
        &self.message_id
    }

    #[must_use]
    pub fn text(&self) -> &MessageText {
        &self.text
    }
}

/// The whole message again (`edit.json`): its own `id` and `clientmessageid`, `from` `worker/<MRI>`, the
/// `conversationLink` `conversation/<chat>;messageid=<id>`, its times as sent, `version` the edit's time and
/// `properties.edittime` the same time in milliseconds.
impl Prepared for EditMessage {
    fn wire(&self) -> Option<Wire> {
        if !self.text.mentions_everyone_only_in(&self.conversation) {
            return None;
        }
        let compose_time = iso_time(self.original.compose_time_ms);
        let arrival_time = iso_time(self.original.arrival_time_ms);
        let mut body = message_body(&MessageFields {
            id: &self.message_id,
            conversation: &self.conversation,
            link: format!("conversation/{};messageid={}", self.conversation, self.message_id),
            from: format!("worker/{}", self.author.mri),
            author: &self.author,
            compose_time: &compose_time,
            arrival_time: &arrival_time,
            version: iso_time(self.edit_time_ms),
            text: &self.text,
            client_message_id: Some(&self.original.client_message_id),
        });
        let mut properties = properties(&self.text);
        properties.extend(self.attachment_properties.clone());
        properties.insert("edittime".to_owned(), json!(self.edit_time_ms));
        if let Some(fields) = body.as_object_mut() {
            fields.insert("properties".to_owned(), Value::Object(properties));
        }
        Some(Wire {
            raw: None,
            service: Service::ChatService,
            headers: Vec::new(),
            method: Method::Put,
            endpoint: message(&self.conversation, &self.message_id),
            body: Some(body),
        })
    }
}

/// Deletes one of the author's messages: `DELETE v1/users/ME/conversations/<chat>/messages/<id>?behavior=softDelete`
/// with no body (`delete.json`). Its target is [`WriteKind::Delete`] on the chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteMessage {
    conversation: String,
    message_id: String,
}

impl DeleteMessage {
    #[must_use]
    pub fn new(conversation: impl Into<String>, message_id: impl Into<String>) -> Self {
        Self {
            conversation: conversation.into(),
            message_id: message_id.into(),
        }
    }
}

impl Prepared for DeleteMessage {
    fn wire(&self) -> Option<Wire> {
        Some(Wire {
            raw: None,
            service: Service::ChatService,
            headers: Vec::new(),
            method: Method::Delete,
            endpoint: message(&self.conversation, &self.message_id).with_query("behavior", "softDelete"),
            body: None,
        })
    }
}

/// Reacts to a message: `PUT v1/users/ME/conversations/<chat>/messages/<id>/properties?name=emotions` with
/// `{"emotions": {"key", "value"}}`, the value the time in milliseconds (`reaction.json`). Its target is
/// [`WriteKind::Reaction`] on the chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetReaction {
    conversation: String,
    message_id: String,
    key: String,
    time_ms: i64,
}

impl SetReaction {
    /// The reaction `key` (`like`, …) on `message_id` in `conversation`, made now.
    #[must_use]
    pub fn new(conversation: impl Into<String>, message_id: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            conversation: conversation.into(),
            message_id: message_id.into(),
            key: key.into(),
            time_ms: now_ms(),
        }
    }

    /// The same reaction made at `time_ms`: its `value` as sent.
    #[must_use]
    pub fn at(mut self, time_ms: i64) -> Self {
        self.time_ms = time_ms;
        self
    }
}

impl Prepared for SetReaction {
    fn wire(&self) -> Option<Wire> {
        Some(Wire {
            raw: None,
            service: Service::ChatService,
            headers: Vec::new(),
            method: Method::Put,
            endpoint: emotions(&self.conversation, &self.message_id),
            body: Some(json!({"emotions": {"key": self.key, "value": self.time_ms}})),
        })
    }
}

/// Takes a reaction back: `DELETE` on [`SetReaction`]'s URL with `{"emotions": {"key"}}` (`reaction.json`). Its target
/// is [`WriteKind::Reaction`] on the chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoveReaction {
    conversation: String,
    message_id: String,
    key: String,
}

impl RemoveReaction {
    #[must_use]
    pub fn new(conversation: impl Into<String>, message_id: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            conversation: conversation.into(),
            message_id: message_id.into(),
            key: key.into(),
        }
    }
}

impl Prepared for RemoveReaction {
    fn wire(&self) -> Option<Wire> {
        Some(Wire {
            raw: None,
            service: Service::ChatService,
            headers: Vec::new(),
            method: Method::Delete,
            endpoint: emotions(&self.conversation, &self.message_id),
            body: Some(json!({"emotions": {"key": self.key}})),
        })
    }
}

/// Titles a channel meeting's post: `PUT v1/users/ME/conversations/<channel>/messages/<post>/properties?name=meeting`
/// with `{"meeting": {"meetingtitle": <title>}}`, what a channel's Meet now sends once the service posted its meeting
/// (the pre-join's subject, edited or not, seen 2026-11-03: `docs/protocol/calling.md`, "A channel's meeting"). Its
/// target is [`WriteKind::Edit`] on the channel's team ([`Scope::Team`]): everyone on it sees the post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetMeetingTitle {
    channel: String,
    post: String,
    title: String,
}

impl SetMeetingTitle {
    /// The title `title` on the post `post` (digits) of the channel `channel` (a team's thread); `None` for either in
    /// another form, or a title of nothing but spaces.
    #[must_use]
    pub fn new(channel: &str, post: &str, title: &str) -> Option<Self> {
        let post_reads = !post.is_empty() && post.bytes().all(|b| b.is_ascii_digit());
        let title = title.trim();
        (crate::is_team_thread_id(channel) && post_reads && !title.is_empty()).then(|| Self {
            channel: channel.to_owned(),
            post: post.to_owned(),
            title: title.to_owned(),
        })
    }
}

impl Prepared for SetMeetingTitle {
    fn wire(&self) -> Option<Wire> {
        Some(Wire {
            raw: None,
            service: Service::ChatService,
            headers: Vec::new(),
            method: Method::Put,
            endpoint: Endpoint::new([
                "v1",
                "users",
                "ME",
                "conversations",
                self.channel.as_str(),
                "messages",
                self.post.as_str(),
                "properties",
            ])
            .with_query("name", "meeting"),
            body: Some(json!({"meeting": {"meetingtitle": self.title}})),
        })
    }
}

/// Moves the account's read horizon in a chat (a read receipt others may see): `PUT
/// v1/users/ME/conversations/<chat>/properties?name=consumptionhorizon` with `{"consumptionhorizon": "<message
/// id>;<ms>;<clientmessageid>"}` (`docs/protocol/chat.md`). Its target is [`WriteKind::ReadReceipt`] on the chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadReceipt {
    conversation: String,
    message_id: String,
    client_message_id: String,
    time_ms: i64,
}

impl ReadReceipt {
    /// Read up to `message_id` (sent with `client_message_id`) in `conversation`, now.
    #[must_use]
    pub fn new(
        conversation: impl Into<String>,
        message_id: impl Into<String>,
        client_message_id: impl Into<String>,
    ) -> Self {
        Self {
            conversation: conversation.into(),
            message_id: message_id.into(),
            client_message_id: client_message_id.into(),
            time_ms: now_ms(),
        }
    }

    /// The same horizon set at `time_ms`.
    #[must_use]
    pub fn at(mut self, time_ms: i64) -> Self {
        self.time_ms = time_ms;
        self
    }

    /// The horizon as sent: `<message id>;<ms>;<clientmessageid>`.
    #[must_use]
    pub fn horizon(&self) -> String {
        format!("{};{};{}", self.message_id, self.time_ms, self.client_message_id)
    }
}

impl Prepared for ReadReceipt {
    fn wire(&self) -> Option<Wire> {
        let endpoint = Endpoint::new(["v1", "users", "ME", "conversations", &self.conversation, "properties"])
            .with_query("name", "consumptionhorizon");
        Some(Wire {
            raw: None,
            service: Service::ChatService,
            headers: Vec::new(),
            method: Method::Put,
            endpoint,
            body: Some(json!({"consumptionhorizon": self.horizon()})),
        })
    }
}

/// Marks a conversation unread from a message on, or ends that mark: `PUT
/// v1/users/ME/conversations/<conversation>/properties?name=consumptionHorizonBookmark` with
/// `{"consumptionHorizonBookmark": "<message id - 1>;<ms>;<clientmessageid>"}` ("Mark as unread" on a message, or on the
/// chat list's row for its newest message), or `"0;<ms>;0"` (the chat opened again, or "Mark as read"). Seen in browser
/// Teams on a chat and on a channel (`docs/protocol/chat.md`, "Mark as unread"). Nobody else is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnreadMark {
    conversation: String,
    from: Option<(u64, String)>,
    time_ms: i64,
}

impl UnreadMark {
    /// `conversation` unread from `message_id` (sent with `client_message_id`) on, now; `None` when the message id
    /// isn't the arrival number a chat's message has.
    #[must_use]
    pub fn from_message(
        conversation: impl Into<String>,
        message_id: &str,
        client_message_id: impl Into<String>,
    ) -> Option<Self> {
        let arrival = message_id.parse::<u64>().ok().filter(|id| *id > 0)?;
        Some(Self {
            conversation: conversation.into(),
            from: Some((arrival - 1, client_message_id.into())),
            time_ms: now_ms(),
        })
    }

    /// The mark on `conversation` ended, now.
    #[must_use]
    pub fn cleared(conversation: impl Into<String>) -> Self {
        Self {
            conversation: conversation.into(),
            from: None,
            time_ms: now_ms(),
        }
    }

    /// The same mark set at `time_ms`.
    #[must_use]
    pub fn at(mut self, time_ms: i64) -> Self {
        self.time_ms = time_ms;
        self
    }

    /// The bookmark as sent: `<message id - 1>;<ms>;<clientmessageid>`, or `0;<ms>;0`.
    #[must_use]
    pub fn bookmark(&self) -> String {
        match &self.from {
            Some((before, client_message_id)) => format!("{before};{};{client_message_id}", self.time_ms),
            None => format!("0;{};0", self.time_ms),
        }
    }
}

impl Prepared for UnreadMark {
    fn wire(&self) -> Option<Wire> {
        let endpoint = Endpoint::new(["v1", "users", "ME", "conversations", &self.conversation, "properties"])
            .with_query("name", "consumptionHorizonBookmark");
        Some(Wire {
            raw: None,
            service: Service::ChatService,
            headers: Vec::new(),
            method: Method::Put,
            endpoint,
            body: Some(json!({"consumptionHorizonBookmark": self.bookmark()})),
        })
    }
}

/// Tells the chat the account is typing: the message post with `{"content": "", "contenttype":
/// "Application/Message", "messagetype": "Control/Typing"}` (`writes.json`). Its target is [`WriteKind::Typing`] on
/// the chat. Typing stopped (`Control/ClearTyping`) wasn't captured, so it isn't here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Typing {
    conversation: String,
}

impl Typing {
    #[must_use]
    pub fn new(conversation: impl Into<String>) -> Self {
        Self {
            conversation: conversation.into(),
        }
    }
}

impl Prepared for Typing {
    fn wire(&self) -> Option<Wire> {
        Some(Wire {
            raw: None,
            service: Service::ChatService,
            headers: Vec::new(),
            method: Method::Post,
            endpoint: messages(&self.conversation),
            body: Some(json!({"content": "", "contenttype": "Application/Message", "messagetype": "Control/Typing"})),
        })
    }
}

/// Creates a chat: `POST v1/threads` with `{"members": [{"id", "role": "Admin"}, …], "properties": {"threadType":
/// "chat", …}}`, as the web client created the a–b 1:1 chat and the a–b–c group chat
/// (`docs/protocol/chat-service.md`), and a named group chat with its name as `properties.topic`, as New message's
/// "Add group name" sent it (2026-11-06). The members include the creator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateChat {
    /// The creator first, then the others in the order given.
    members: Vec<Mri>,
    one_to_one: bool,
    /// A group chat's name.
    topic: Option<String>,
}

impl CreateChat {
    /// The 1:1 chat of `me` and `other` (`fixedRoster`, `uniquerosterthread`: the service answers with the one that
    /// exists, if any). `None` when they are the same account.
    #[must_use]
    pub fn one_to_one(me: Mri, other: Mri) -> Option<Self> {
        (me != other).then(|| Self {
            members: vec![me, other],
            one_to_one: true,
            topic: None,
        })
    }

    /// A new group chat of `me` and `others`. `None` unless `others` are at least two accounts, none of them `me`,
    /// none given twice.
    #[must_use]
    pub fn group(me: Mri, others: impl IntoIterator<Item = Mri>) -> Option<Self> {
        let mut members = vec![me];
        for other in others {
            if members.contains(&other) {
                return None;
            }
            members.push(other);
        }
        (members.len() >= 3).then_some(Self {
            members,
            one_to_one: false,
            topic: None,
        })
    }

    /// The group chat named `topic` (trimmed; a blank one names nothing), as Teams' "Add group name" sends it. A 1:1
    /// chat takes no name and stays as it was.
    #[must_use]
    pub fn named(mut self, topic: &str) -> Self {
        let topic = topic.trim();
        if !self.one_to_one && !topic.is_empty() {
            self.topic = Some(topic.to_owned());
        }
        self
    }

    /// The group chat's name, when it is given one.
    #[must_use]
    pub fn topic(&self) -> Option<&str> {
        self.topic.as_deref()
    }

    /// The members, the creator first.
    #[must_use]
    pub fn members(&self) -> &[Mri] {
        &self.members
    }
}

impl Prepared for CreateChat {
    fn wire(&self) -> Option<Wire> {
        let members: Vec<Value> = self
            .members
            .iter()
            .map(|mri| json!({"id": mri.as_str(), "role": "Admin"}))
            .collect();
        let properties = match (&self.topic, self.one_to_one) {
            (_, true) => json!({"threadType": "chat", "fixedRoster": true, "uniquerosterthread": true}),
            (Some(topic), false) => json!({"threadType": "chat", "topic": topic}),
            (None, false) => json!({"threadType": "chat"}),
        };
        Some(Wire {
            raw: None,
            service: Service::ChatService,
            headers: Vec::new(),
            method: Method::Post,
            endpoint: Endpoint::new(["v1", "threads"]),
            body: Some(json!({"members": members, "properties": properties})),
        })
    }
}

/// How much of a group chat's history people added to it see, as Teams' "Add" asks it: sent as each member's
/// `shareHistoryTime`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharedHistory {
    /// "Don't include chat history": `-1`.
    None,
    /// "Include history from the past number of days": the messages since this time, in milliseconds since the Unix
    /// epoch (the time of the click less that many days, as the web client works it out).
    Since(i64),
    /// "Include all chat history": `0`.
    All,
}

impl SharedHistory {
    /// The `shareHistoryTime` each member is sent with.
    #[must_use]
    pub fn share_history_time(self) -> i64 {
        match self {
            Self::None => -1,
            Self::Since(at) => at.max(1),
            Self::All => 0,
        }
    }

    /// The choice a stored `shareHistoryTime` is: `-1` (or any other negative) none, `0` all, else since then.
    #[must_use]
    pub fn from_share_history_time(time: i64) -> Self {
        match time {
            0 => Self::All,
            time if time < 0 => Self::None,
            time => Self::Since(time),
        }
    }
}

/// Adds people to a group chat: `PUT v1/threads/<chat>/members` with `{"members": [{"id", "role": "Admin",
/// "shareHistoryTime"}, …]}`, as browser Teams' "Add people" sent it (2026-11-10, `docs/protocol/chat-service.md`,
/// "Adding people to a chat"). Everyone is added as an `Admin`, as a group chat's every member is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddChatMembers {
    conversation: String,
    members: Vec<Mri>,
    history: SharedHistory,
}

impl AddChatMembers {
    /// `members` added to `conversation` with `history`; `None` with nobody to add or someone given twice.
    #[must_use]
    pub fn new(conversation: impl Into<String>, members: Vec<Mri>, history: SharedHistory) -> Option<Self> {
        let twice = members
            .iter()
            .enumerate()
            .any(|(at, member)| members[..at].contains(member));
        (!members.is_empty() && !twice).then(|| Self {
            conversation: conversation.into(),
            members,
            history,
        })
    }

    /// The chat they are added to.
    #[must_use]
    pub fn conversation(&self) -> &str {
        &self.conversation
    }

    /// The people added, in the order given.
    #[must_use]
    pub fn members(&self) -> &[Mri] {
        &self.members
    }

    /// How much of the history they see.
    #[must_use]
    pub fn history(&self) -> SharedHistory {
        self.history
    }
}

impl Prepared for AddChatMembers {
    fn wire(&self) -> Option<Wire> {
        let time = self.history.share_history_time();
        let members: Vec<Value> = self
            .members
            .iter()
            .map(|mri| json!({"id": mri.as_str(), "role": "Admin", "shareHistoryTime": time}))
            .collect();
        Some(Wire {
            raw: None,
            service: Service::ChatService,
            headers: Vec::new(),
            method: Method::Put,
            endpoint: Endpoint::new(["v1", "threads", self.conversation.as_str(), "members"]),
            body: Some(json!({"members": members})),
        })
    }
}

/// The longest group chat name Teams' "Group name" box takes, in characters: one more is refused with "Your group
/// name can't be longer than 250 characters." (browser Teams, 2026-11-20).
pub const CHAT_TOPIC_MOST: usize = 250;

/// Names, renames or unnames a group chat: `PUT v1/threads/<chat>/properties?name=topic` with `{"topic": <name>}`,
/// as browser Teams' "Name group chat" sent it (2026-11-20, `docs/protocol/chat-service.md`, "Naming a group chat").
/// The name is trimmed; an empty one takes the name off (`{"topic": ""}`), and the chat is called by its people again.
/// Every member gets the thread's `ThreadUpdate` and a `ThreadActivity/TopicUpdate` message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetChatTopic {
    conversation: String,
    topic: String,
}

impl SetChatTopic {
    /// `conversation` named `topic` (trimmed, empty to take the name off); `None` for a name longer than
    /// [`CHAT_TOPIC_MOST`] characters.
    #[must_use]
    pub fn new(conversation: impl Into<String>, topic: &str) -> Option<Self> {
        let topic = topic.trim();
        (topic.chars().count() <= CHAT_TOPIC_MOST).then(|| Self {
            conversation: conversation.into(),
            topic: topic.to_owned(),
        })
    }

    /// The chat it names.
    #[must_use]
    pub fn conversation(&self) -> &str {
        &self.conversation
    }

    /// The name it sends: empty when it takes the name off.
    #[must_use]
    pub fn topic(&self) -> &str {
        &self.topic
    }
}

impl Prepared for SetChatTopic {
    fn wire(&self) -> Option<Wire> {
        Some(Wire {
            raw: None,
            service: Service::ChatService,
            headers: Vec::new(),
            method: Method::Put,
            endpoint: Endpoint::new(["v1", "threads", self.conversation.as_str(), "properties"])
                .with_query("name", "topic"),
            body: Some(json!({"topic": self.topic})),
        })
    }
}

/// `v1/users/ME/conversations/<chat>/messages`.
fn messages(conversation: &str) -> Endpoint {
    Endpoint::new(["v1", "users", "ME", "conversations", conversation, "messages"])
}

/// `v1/users/ME/conversations/<chat>/messages/<id>`.
fn message(conversation: &str, id: &str) -> Endpoint {
    Endpoint::new(["v1", "users", "ME", "conversations", conversation, "messages", id])
}

/// `v1/users/ME/conversations/<chat>/messages/<id>/properties?name=emotions`.
fn emotions(conversation: &str, id: &str) -> Endpoint {
    Endpoint::new([
        "v1",
        "users",
        "ME",
        "conversations",
        conversation,
        "messages",
        id,
        "properties",
    ])
    .with_query("name", "emotions")
}

/// The fields a post and an edit fill differently.
struct MessageFields<'a> {
    id: &'a str,
    conversation: &'a str,
    link: String,
    from: String,
    author: &'a Author,
    compose_time: &'a str,
    arrival_time: &'a str,
    version: String,
    text: &'a MessageText,
    client_message_id: Option<&'a str>,
}

/// A message's keys as the web client sends them in a post and an edit, `properties` aside.
fn message_body(fields: &MessageFields<'_>) -> Value {
    let mut body = json!({
        "id": fields.id,
        "type": "Message",
        "conversationid": fields.conversation,
        "conversationLink": fields.link,
        "from": fields.from,
        "fromUserId": fields.author.mri.as_str(),
        "composetime": fields.compose_time,
        "originalarrivaltime": fields.arrival_time,
        "content": fields.text.html(),
        "messagetype": "RichText/Html",
        "contenttype": "Text",
        "imdisplayname": fields.author.display_name,
        "callId": "",
        "state": 0,
        "version": fields.version,
        "amsreferences": fields.text.pictures(),
        "crossPostChannels": [],
    });
    if let (Some(id), Some(object)) = (fields.client_message_id, body.as_object_mut()) {
        object.insert("clientmessageid".to_owned(), id.into());
    }
    body
}

/// A message's `properties` as the web client sends them: the empty lists and fields it always sends, the text's
/// `mentions`, and its `qtdMsgs` when it quotes a message.
fn properties(text: &MessageText) -> Map<String, Value> {
    let mut properties = Map::new();
    for (key, value) in [
        ("importance", json!("")),
        ("subject", json!("")),
        ("title", json!("")),
        ("cards", json!(text.cards_property())),
        ("links", json!("[]")),
        ("mentions", json!(text.mentions_property())),
        ("onbehalfof", Value::Null),
        ("files", json!(text.files_property())),
        ("policyViolation", Value::Null),
        ("formatVariant", json!("TEAMS")),
    ] {
        properties.insert(key.to_owned(), value);
    }
    if let Some(quoted) = text.quoted_property() {
        properties.insert("qtdMsgs".to_owned(), json!(quoted));
    }
    properties
}

/// The latest time `iso_time` writes: the end of year 9999, in milliseconds.
const LAST_MS: i64 = 253_402_300_799_999;

/// `time_ms` as the web client writes a time, `2026-01-01T00:00:00.000Z`; clamped to 1970–9999.
fn iso_time(time_ms: i64) -> String {
    let time_ms = time_ms.clamp(0, LAST_MS);
    let time = OffsetDateTime::from_unix_timestamp_nanos(i128::from(time_ms) * 1_000_000)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        time.year(),
        u8::from(time.month()),
        time.day(),
        time.hour(),
        time.minute(),
        time.second(),
        time.millisecond()
    )
}

/// The time now, in milliseconds since the Unix epoch.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}
