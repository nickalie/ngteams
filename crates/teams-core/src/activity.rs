//! The account's activity feed, as Teams' Activity pane shows it (`docs/protocol/chat.md`, "The activity feed").
//!
//! - **Where it comes from**: the chat service keeps the feed as the account's own stream `48:notifications`; the
//!   catch-up and the push channel store its items as any conversation's messages, each with its `activity` in its
//!   properties (who, what, where, a preview). [`ActivityFeed::from_store`] reads the newest [`FEED_LIMIT`] of them and
//!   leaves out the removed ones (soft-deleted).
//! - **What an item says** ([`ActivityItem`]): its kind from `activityType`/`activitySubtype` ([`ActivityKind`]), the
//!   person or app behind it, the message preview and where it happened (the source thread, its type and topic), and
//!   the source message it opens at. A kind no capture or bundle names is [`ActivityKind::Other`], still listed.
//! - **Folded reactions**: several reactions to the same message are one item, as Teams shows "Sean Hopkins +1 reacted
//!   to your message": the newest one's, counting the other people ([`ActivityItem::others`]) and carrying every
//!   item's id ([`ActivityItem::ids`]) so marking it marks them all.
//! - **Read or not** ([`ActivityFeed::is_read`]): an item is read when each folded member either has `isread` set or
//!   has a `version` no later than the feed's all-read bookmark (`consumptionHorizonBookmark`, set by "Mark all as
//!   read"). Marking a member unread moves its version past the bookmark, so the item reads unread again.
//! - **New** ([`ActivityFeed::unseen`], the rail's badge): unread items newer than the feed's own read horizon, which
//!   Teams moves to the newest item whenever the Activity pane is opened ([`ActivityFeed::seen_horizon`]).

use serde_json::{Map, Value};
use teams_store::{Message, Store, StoreError};

/// The account's activity feed stream.
pub const FEED: &str = teams_proto::ACTIVITY_FEED;

/// How many of the newest items the feed reads.
pub const FEED_LIMIT: usize = 200;

/// What kind of mention an item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mention {
    /// The account by name.
    You,
    /// Everyone in a chat.
    Everyone,
    /// A team.
    Team,
    /// A channel.
    Channel,
    /// A tag the account has.
    Tag,
}

/// What an item tells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivityKind {
    Mention(Mention),
    /// A reaction to the account's message, by its key (`like`, `heart`, …).
    Reaction(String),
    /// A reply to a thread the account is in.
    Reply,
    MissedCall,
    Voicemail,
    /// The account was added to a team.
    AddedToTeam,
    /// A meeting the account is invited to, by what happened to it.
    Meeting(MeetingChange),
    /// An app's own notification (Workflows' "Follow up on a message", …): its title is the app's.
    App,
    /// Any other kind, listed with what the service says of it.
    Other,
}

impl ActivityKind {
    /// Its key, for `ngteamsctl`: `mention`, `everyone`, `reaction`, `missed-call`, `meeting-updated`, …
    #[must_use]
    pub fn key(&self) -> &'static str {
        match self {
            Self::Mention(Mention::You) => "mention",
            Self::Mention(Mention::Everyone) => "everyone",
            Self::Mention(Mention::Team) => "team-mention",
            Self::Mention(Mention::Channel) => "channel-mention",
            Self::Mention(Mention::Tag) => "tag-mention",
            Self::Reaction(_) => "reaction",
            Self::Reply => "reply",
            Self::MissedCall => "missed-call",
            Self::Voicemail => "voicemail",
            Self::AddedToTeam => "added-to-team",
            Self::Meeting(MeetingChange::Invited) => "meeting-invited",
            Self::Meeting(MeetingChange::Updated) => "meeting-updated",
            Self::Meeting(MeetingChange::Canceled) => "meeting-canceled",
            Self::Meeting(MeetingChange::Forwarded) => "meeting-forwarded",
            Self::App => "app",
            Self::Other => "other",
        }
    }
}

/// What happened to a meeting, by the Meet app's activity types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeetingChange {
    Invited,
    Updated,
    Canceled,
    Forwarded,
}

/// One item of the feed (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityItem {
    /// The feed message's id (the newest one's, for folded reactions).
    pub id: String,
    /// The service's id of the activity (`activityId`): what the feed's horizon names.
    pub activity_id: Option<String>,
    /// Every feed message the item stands for, the newest first.
    pub ids: Vec<String>,
    pub kind: ActivityKind,
    /// The person or app behind it (`sourceUserId`), when it names one.
    pub actor: Option<String>,
    /// Their name as the item carries it (`sourceUserImDisplayName`, else the app's name).
    pub actor_name: String,
    /// How many other people a folded item stands for.
    pub others: usize,
    /// The message's text as the service previews it (`messagePreview`), whitespace folded.
    pub preview: String,
    /// The thread it happened in (`sourceThreadId`), when it names one.
    pub thread: Option<String>,
    /// The message it is about in that thread (`sourceMessageId`), when it names a real one.
    pub message: Option<String>,
    /// The post whose thread the message is in, for a channel's reply (`sourceReplyChainId`).
    pub reply_chain: Option<String>,
    /// The thread's type (`sourceThreadType`: `chat`, `space`, `topic`, …).
    pub thread_type: Option<String>,
    /// The thread's or team's name, when the item carries it (`sourceThreadTopic`).
    pub topic: Option<String>,
    /// An app item's own title (`activityContext.templateParameters.customMessage`).
    pub custom_title: Option<String>,
    /// A meeting item's start and end, ms (`activityContext.location`'s `DateTimeRange`).
    pub meeting_time: Option<(i64, i64)>,
    /// The calendar event a meeting item opens (its `customTapAction`'s `l/meeting/details/?eventId=`), decoded.
    pub meeting_event: Option<String>,
    /// When it happened (`activityTimestamp`), else when it arrived, ms.
    pub at_ms: i64,
    /// The feed message's version: moved by every change of its read state.
    pub version: i64,
    /// Distinct known reactors; unknown identities do not establish an extra person.
    actors: Vec<String>,
    /// Each member's own read state and version, kept separately for the mutable all-read bookmark.
    read_states: Vec<(bool, i64)>,
}

impl ActivityItem {
    /// The item a feed message tells; `None` for one that carries no activity, or was removed.
    #[must_use]
    pub fn from_message(message: &Message) -> Option<Self> {
        if message.delete_time.is_some() {
            return None;
        }
        let properties: Map<String, Value> = serde_json::from_str(message.properties.as_deref()?).ok()?;
        let activity = properties.get("activity")?.as_object()?;
        let text = |key: &str| {
            activity
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let context = activity.get("activityContext").and_then(Value::as_object);
        let kind = kind(
            text("activityType").as_deref().unwrap_or_default(),
            text("activitySubtype").as_deref().unwrap_or_default(),
        );
        let actor_name = text("sourceUserImDisplayName")
            .or_else(|| text("sourceAppDisplayName"))
            .unwrap_or_default();
        let at_ms = text("activityTimestamp")
            .and_then(|stamp| teams_proto::chat::iso_millis(&stamp))
            .unwrap_or(message.arrival_time);
        // an app's item about a person's doing (the Calendar app's "AI updated") names that person besides the app
        let actor = text("sourceUserId")
            .filter(|source| !source.starts_with("28:"))
            .or_else(|| context.and_then(|context| context_text(context, "attributedToActorId")))
            .or_else(|| text("sourceUserId"));
        Some(Self {
            id: message.id.clone(),
            activity_id: id_of(activity.get("activityId")),
            ids: vec![message.id.clone()],
            kind,
            actors: actor.iter().cloned().collect(),
            actor,
            actor_name,
            others: 0,
            preview: text("messagePreview")
                .map(|preview| preview.split_whitespace().collect::<Vec<_>>().join(" "))
                .unwrap_or_default(),
            thread: text("sourceThreadId"),
            message: id_of(activity.get("sourceMessageId")),
            reply_chain: id_of(activity.get("sourceReplyChainId")),
            thread_type: text("sourceThreadType"),
            topic: text("sourceThreadTopic"),
            custom_title: context.and_then(custom_title),
            meeting_time: context.and_then(meeting_time),
            meeting_event: text("customTapAction").as_deref().and_then(meeting_event),
            at_ms,
            version: message.version,
            read_states: vec![(read_flag(properties.get("isread")), message.version)],
        })
    }

    /// What Teams titles the item with: "Sean Hopkins +1 reacted to your message", "Lab B mentioned you", "Missed call
    /// from Lab B", an app's own title, …
    #[must_use]
    pub fn title(&self) -> String {
        let who = match (self.actor_name.as_str(), self.others) {
            ("", _) => "Someone".to_owned(),
            (name, 0) => name.to_owned(),
            (name, others) => format!("{name} +{others}"),
        };
        match &self.kind {
            ActivityKind::Mention(Mention::You) => format!("{who} mentioned you"),
            ActivityKind::Mention(Mention::Everyone) => format!("{who} mentioned Everyone"),
            ActivityKind::Mention(Mention::Team) => format!("{who} mentioned your team"),
            ActivityKind::Mention(Mention::Channel) => format!("{who} mentioned a channel"),
            ActivityKind::Mention(Mention::Tag) => format!("{who} mentioned your tag"),
            ActivityKind::Reaction(_) => format!("{who} reacted to your message"),
            ActivityKind::Reply => format!("{who} replied"),
            ActivityKind::MissedCall => format!("Missed call from {who}"),
            ActivityKind::Voicemail => format!("Voicemail from {who}"),
            ActivityKind::AddedToTeam => match &self.topic {
                Some(team) => format!("A team owner added you to {team}"),
                None => "A team owner added you to a team".to_owned(),
            },
            ActivityKind::Meeting(MeetingChange::Invited) => format!("{who} invited you"),
            ActivityKind::Meeting(MeetingChange::Updated) => format!("{who} updated"),
            ActivityKind::Meeting(MeetingChange::Canceled) => format!("{who} canceled"),
            ActivityKind::Meeting(MeetingChange::Forwarded) => format!("{who} forwarded a meeting"),
            ActivityKind::App | ActivityKind::Other => self.custom_title.clone().unwrap_or(who),
        }
    }

    /// Whether it happened in a team's channel (`space`: a general channel, `topic`: another).
    #[must_use]
    pub fn in_channel(&self) -> bool {
        matches!(self.thread_type.as_deref(), Some("space" | "topic"))
            || self.thread.as_deref().is_some_and(teams_proto::is_team_thread_id)
    }

    /// Whether the item matches what the search's box holds: its title, its preview or where it happened contains it,
    /// case folded and its spaces folded, as Teams' "Filter by name or group name" finds a name typed.
    #[must_use]
    pub fn matches(&self, words: &str) -> bool {
        let words = fold(words);
        if words.is_empty() {
            return true;
        }
        [
            self.title(),
            self.preview.clone(),
            self.topic.clone().unwrap_or_default(),
        ]
        .iter()
        .any(|text| fold(text).contains(&words))
    }
}

/// The filter chips Teams offers over the feed (`MTMAHubActivityFilterTypes`): unread, mentions, reactions, missed
/// calls, replies, voicemail and apps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActivityFilter {
    #[default]
    All,
    Unread,
    Mentions,
    Reactions,
    MissedCalls,
    Replies,
    Voicemail,
    Apps,
}

impl ActivityFilter {
    /// Every chip, in Teams' order (not All, which is no chip chosen).
    pub const CHIPS: [Self; 7] = [
        Self::Unread,
        Self::Mentions,
        Self::Reactions,
        Self::MissedCalls,
        Self::Replies,
        Self::Voicemail,
        Self::Apps,
    ];

    /// The chip's words.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Unread => "Unread",
            Self::Mentions => "@Mentions",
            Self::Reactions => "Reactions",
            Self::MissedCalls => "Missed calls",
            Self::Replies => "Replies",
            Self::Voicemail => "Voicemail",
            Self::Apps => "Apps",
        }
    }

    /// Its key, for a test selector and `ngteamsctl`.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Unread => "unread",
            Self::Mentions => "mentions",
            Self::Reactions => "reactions",
            Self::MissedCalls => "missed-calls",
            Self::Replies => "replies",
            Self::Voicemail => "voicemail",
            Self::Apps => "apps",
        }
    }

    /// The filter of a key; `None` for another word.
    #[must_use]
    pub fn of_key(key: &str) -> Option<Self> {
        std::iter::once(Self::All)
            .chain(Self::CHIPS)
            .find(|filter| filter.key() == key)
    }
}

/// The feed as stored (see the module docs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivityFeed {
    /// The items, the newest first, reactions to one message folded.
    pub items: Vec<ActivityItem>,
    /// The feed's own read horizon: when the newest item seen happened (ms).
    pub seen_ms: Option<i64>,
    /// The all-read bookmark (ms).
    pub bookmark_ms: Option<i64>,
}

impl ActivityFeed {
    /// The feed from feed messages (any order), the feed's own horizon and its bookmark.
    #[must_use]
    pub fn new(messages: &[Message], seen_ms: Option<i64>, bookmark_ms: Option<i64>) -> Self {
        let mut items: Vec<ActivityItem> = messages.iter().filter_map(ActivityItem::from_message).collect();
        items.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then_with(|| b.id.cmp(&a.id)));
        let mut feed = Self {
            items: Vec::new(),
            seen_ms,
            bookmark_ms,
        };
        for item in items {
            feed.push_folded(item);
        }
        feed
    }

    /// The feed as the store holds it for `me`.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when a read fails.
    pub fn from_store(store: &Store, me: &str) -> Result<Self, StoreError> {
        let page = store.messages_before(FEED, None, FEED_LIMIT)?;
        let seen = store.read_horizon(FEED, me)?.map(|horizon| horizon.last_read_arrival);
        let bookmark = store.horizon_bookmark(FEED)?;
        Ok(Self::new(&page.messages, seen, bookmark))
    }

    /// Adds an item, folding a reaction into the newer one to the same message.
    fn push_folded(&mut self, item: ActivityItem) {
        let folds = |kept: &ActivityItem| {
            matches!(kept.kind, ActivityKind::Reaction(_))
                && matches!(item.kind, ActivityKind::Reaction(_))
                && kept.thread.is_some()
                && kept.thread == item.thread
                && kept.message.is_some()
                && kept.message == item.message
        };
        let Some(kept) = self.items.iter_mut().find(|kept| folds(kept)) else {
            self.items.push(item);
            return;
        };
        for actor in item.actors {
            if !kept.actors.contains(&actor) {
                kept.actors.push(actor);
            }
        }
        kept.others = kept.actors.len().saturating_sub(1);
        kept.read_states.extend(item.read_states);
        kept.ids.push(item.id);
        kept.version = kept.version.max(item.version);
    }

    /// Whether `item` reads as read (see the module docs).
    #[must_use]
    pub fn is_read(&self, item: &ActivityItem) -> bool {
        item.read_states
            .iter()
            .all(|(read, version)| *read || self.bookmark_ms.is_some_and(|bookmark| *version <= bookmark))
    }

    /// How many unread items are newer than the feed's horizon: what the rail's badge counts.
    #[must_use]
    pub fn unseen(&self) -> usize {
        self.unseen_by(|item| self.is_read(item))
    }

    /// How many items newer than the feed's horizon `read` doesn't call read: [`Self::unseen`] for a view that also
    /// knows the marks it made and the items it removed, before the store has them.
    #[must_use]
    pub fn unseen_by(&self, read: impl Fn(&ActivityItem) -> bool) -> usize {
        self.items
            .iter()
            .filter(|item| !read(item) && self.seen_ms.is_none_or(|seen| item.at_ms > seen))
            .count()
    }

    /// The horizon to put when the pane is opened: the newest item's time and activity id, when it is past the
    /// stored one.
    #[must_use]
    pub fn seen_horizon(&self) -> Option<&ActivityItem> {
        self.items
            .first()
            .filter(|newest| self.seen_ms.is_none_or(|seen| newest.at_ms > seen))
    }

    /// Whether a filter keeps `item`.
    #[must_use]
    pub fn keeps(&self, filter: ActivityFilter, item: &ActivityItem) -> bool {
        match filter {
            ActivityFilter::All => true,
            ActivityFilter::Unread => !self.is_read(item),
            ActivityFilter::Mentions => matches!(item.kind, ActivityKind::Mention(_)),
            ActivityFilter::Reactions => matches!(item.kind, ActivityKind::Reaction(_)),
            ActivityFilter::MissedCalls => item.kind == ActivityKind::MissedCall,
            ActivityFilter::Replies => item.kind == ActivityKind::Reply,
            ActivityFilter::Voicemail => item.kind == ActivityKind::Voicemail,
            ActivityFilter::Apps => item.kind == ActivityKind::App,
        }
    }

    /// The item standing for feed message `id`.
    #[must_use]
    pub fn item(&self, id: &str) -> Option<&ActivityItem> {
        self.items.iter().find(|item| item.ids.iter().any(|own| own == id))
    }
}

/// The kind of `activityType`/`activitySubtype` (see the enum's docs for the names).
fn kind(activity_type: &str, subtype: &str) -> ActivityKind {
    let lower = activity_type.to_ascii_lowercase();
    match lower.as_str() {
        "mention" | "mentions" | "mentioninchat" | "mentionincommunity" | "mentioninsubcommunity" => {
            ActivityKind::Mention(match subtype.to_ascii_lowercase().as_str() {
                "everyone" => Mention::Everyone,
                "team" => Mention::Team,
                "channel" => Mention::Channel,
                "tag" => Mention::Tag,
                _ => Mention::You,
            })
        }
        "reaction" | "reactioninchat" | "like" | "likeinchat" | "reactionincommunity" => {
            ActivityKind::Reaction(if subtype.is_empty() { "like" } else { subtype }.to_owned())
        }
        "reply" | "replytoreply" | "communityreply" | "communityreplytoreply" => ActivityKind::Reply,
        "call" if subtype.eq_ignore_ascii_case("missedcall") => ActivityKind::MissedCall,
        "call" if subtype.eq_ignore_ascii_case("voicemail") => ActivityKind::Voicemail,
        "voicemail" => ActivityKind::Voicemail,
        "teammembershipchange" if subtype.eq_ignore_ascii_case("addedtoteam") => ActivityKind::AddedToTeam,
        "msgraph" => meeting(subtype).map_or(ActivityKind::App, ActivityKind::Meeting),
        "calendar" => ActivityKind::Meeting(meeting(subtype).unwrap_or(MeetingChange::Updated)),
        _ => ActivityKind::Other,
    }
}

/// The Meet app's activity types (`privateMeetingUpdated`, `channelMeetingCreated`, …).
fn meeting(subtype: &str) -> Option<MeetingChange> {
    let lower = subtype.to_ascii_lowercase();
    if !lower.contains("meeting") {
        return None;
    }
    Some(if lower.contains("forwarded") {
        MeetingChange::Forwarded
    } else if lower.contains("cancel") {
        MeetingChange::Canceled
    } else if lower.contains("created") {
        MeetingChange::Invited
    } else {
        MeetingChange::Updated
    })
}

/// A source id as the item gives it: a number or a string of digits, `0` and `1` (a placeholder) none.
fn id_of(value: Option<&Value>) -> Option<String> {
    let id = match value? {
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.trim().to_owned(),
        _ => return None,
    };
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && id != "0" && id != "1").then_some(id)
}

/// Search text lower-cased, trimmed and with whitespace runs folded to one space, for content and places.
#[must_use]
pub fn fold(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// `isread` as a boolean or its text.
fn read_flag(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(read)) => *read,
        Some(Value::String(text)) => text.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

/// A context's text value, trimmed; none when empty.
fn context_text(context: &Map<String, Value>, key: &str) -> Option<String> {
    context
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// A meeting item's start and end: `location` (a JSON object as text) `Custom`, which holds
/// `<DateTimeRange><StartDateTime>…</StartDateTime><EndDateTime>…</EndDateTime></DateTimeRange>` in UTC.
fn meeting_time(context: &Map<String, Value>) -> Option<(i64, i64)> {
    let location: Map<String, Value> = serde_json::from_str(context.get("location")?.as_str()?).ok()?;
    let range = location.get("Custom")?.as_str()?;
    let between = |tag: &str| {
        let open = format!("<{tag}>");
        let start = range.find(&open)? + open.len();
        let end = start + range[start..].find(&format!("</{tag}>"))?;
        teams_proto::chat::iso_millis(range[start..end].trim())
    };
    let (start, end) = (between("StartDateTime")?, between("EndDateTime")?);
    (start <= end).then_some((start, end))
}

/// The event a meeting's details link names: `…/l/meeting/details/?eventId=<id>&…`, the id percent-decoded.
fn meeting_event(link: &str) -> Option<String> {
    let (path, query) = link.split_once('?')?;
    if !path.trim_end_matches('/').ends_with("/l/meeting/details") {
        return None;
    }
    let id = query.split('&').find_map(|pair| pair.strip_prefix("eventId="))?;
    let id = percent_encoding::percent_decode_str(id).decode_utf8().ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

/// An app item's own title: `templateParameters` (a JSON object or its text) `customMessage`.
fn custom_title(context: &Map<String, Value>) -> Option<String> {
    let parameters = context.get("templateParameters")?;
    let parsed;
    let object = match parameters {
        Value::Object(object) => object,
        Value::String(text) => {
            parsed = serde_json::from_str::<Map<String, Value>>(text).ok()?;
            &parsed
        }
        _ => return None,
    };
    object
        .get("customMessage")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
}
