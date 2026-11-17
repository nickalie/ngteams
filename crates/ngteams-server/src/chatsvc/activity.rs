//! Who is told of a message in their activity feed (`48:notifications`) and mentions stream (`48:mentions`).
//!
//! An item is written in the transaction that writes its message, from the roster read there: nobody who isn't in
//! the thread at that moment gets one, whatever the message's mentions name. An item carries a preview of its
//! message, so a person who may not read the message is never given one either. A change of the message changes
//! the items of the people in the thread then, and no item of someone who left it.

use serde_json::{Map, Value, json};

use super::message_repo::{Effect, Message};
use super::repo::Chat;
use super::{streams, wire};
use crate::db::DbError;

/// The longest preview an item carries, in characters.
const PREVIEW: usize = 200;

/// A thread as its messages' activity sees it: what it is and who is in it now.
pub(super) struct Scene {
    pub chat: Chat,
    pub place: Option<Place>,
    pub people: Vec<Person>,
}

/// The channel a thread is, with its team.
pub(super) struct Place {
    pub team: String,
    pub team_name: String,
    pub private: bool,
}

pub(super) struct Person {
    pub id: String,
    pub disabled: bool,
    /// The first message the person may read.
    pub from: i64,
    /// The person's own properties of the thread: their notification preferences are among them.
    pub properties: Map<String, Value>,
}

impl Person {
    fn text(&self, name: &str) -> Option<&str> {
        self.properties.get(name).and_then(Value::as_str)
    }

    /// The thread is muted for them (`alerts` `false`).
    fn muted(&self) -> bool {
        self.text("alerts") == Some("false")
    }

    fn channel_settings(&self) -> Value {
        self.text("channelNotificationSettings")
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or(Value::Null)
    }

    /// "Notify me for all new messages" is on for the channel, and for replies when `reply`.
    fn wants_all(&self, reply: bool) -> bool {
        let settings = self.channel_settings();
        settings["allNewPosts"] == "On" && (!reply || settings["includeReplies"] == true)
    }

    /// "Follow all new threads" is on for the channel.
    fn follows_all(&self) -> bool {
        self.text("followAllRc").is_some_and(|since| !since.is_empty())
    }
}

/// What an item tells.
#[derive(Clone, Copy, PartialEq, Eq)]
enum What {
    /// A mention of the person, or one that takes in everyone (`everyone`, `team`, `channel`).
    Mention(&'static str),
    Reply,
    /// A post or reply the person asked to be told of by the channel's notification settings.
    Followed,
    Reaction,
}

/// One item still to be written.
pub(super) struct Draft {
    pub user: String,
    pub stream: &'static str,
    /// What makes the item one of a kind in its stream: a second write of the same finds it.
    pub key: String,
    pub write: Write,
    what: What,
    subtype: String,
    pub actor: String,
}

/// What a draft does where its key already has an item.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Write {
    /// Nothing: an item is written once.
    Once,
    /// It rewrites the item where it is, as read as it was; one its account removed stays removed.
    Keep,
    /// As [`Write::Keep`], and where the key has no item it writes none.
    Revise,
}

/// What a write of a draft comes to.
pub(super) enum Step {
    Skip,
    New,
    /// The item is rewritten under its id, keeping its read mark.
    Rewrite {
        id: i64,
        read: Value,
    },
}

/// Where an item is: its stream's thread, its account and its key.
pub(super) type Spot = (String, String, String);

/// What a change of a message does to the items it made.
#[derive(Default)]
pub(super) struct Plan {
    /// Items to remove, before anything is written.
    pub gone: Vec<Spot>,
    pub drafts: Vec<Draft>,
    /// Items of an edited message that no draft writes: they take its new wording, or go when a mention made them.
    pub reworded: Vec<Spot>,
}

impl Draft {
    pub(super) fn spot(&self) -> Spot {
        (
            streams::stored_id(self.stream, &self.user),
            self.user.clone(),
            self.key.clone(),
        )
    }

    /// What writing the draft does, given the item `existing` its key has.
    pub(super) fn step(&self, existing: Option<&Message>) -> Result<Step, DbError> {
        let was = existing.map(Message::value).transpose()?;
        Ok(match (self.write, existing, was) {
            (Write::Once | Write::Keep, None, _) => Step::New,
            (Write::Keep | Write::Revise, Some(row), Some(was)) if live(&was) => Step::Rewrite {
                id: row.id,
                read: was["properties"]["isread"].clone(),
            },
            _ => Step::Skip,
        })
    }
}

/// An item as written, to be pushed to its account.
pub(super) struct Told {
    pub user: String,
    pub kind: &'static str,
    pub resource: Value,
}

impl Scene {
    pub(super) fn new(chat: Chat, people: Vec<Person>, place: Option<Place>) -> Self {
        Self { chat, place, people }
    }

    /// The people in the thread who may read `message`: who is told of it and of its changes.
    pub(super) fn readers(&self, message: i64) -> Vec<String> {
        let reading = self.people.iter().filter(|person| person.from <= message);
        reading.map(|person| person.id.clone()).collect()
    }

    fn person(&self, id: &str) -> Option<&Person> {
        self.people.iter().find(|person| person.id == id && !person.disabled)
    }

    /// Drops the items of people who may not read `message`: an item carries its preview.
    fn readable(&self, message: i64, mut drafts: Vec<Draft>) -> Vec<Draft> {
        let readers = self.readers(message);
        drafts.retain(|draft| readers.contains(&draft.user));
        drafts
    }

    fn thread_type(&self) -> &'static str {
        match &self.place {
            None => "chat",
            Some(place) if place.team == self.chat.id => "space",
            Some(_) => "topic",
        }
    }

    fn topic(&self) -> Option<&str> {
        match &self.place {
            Some(place) => Some(&place.team_name),
            None => self.chat.topic.as_deref(),
        }
    }

    /// The ids of the thread a mention of everyone in it may name: its own, and for a channel its team's.
    fn names(&self, mri: &str) -> bool {
        mri == self.chat.id || self.place.as_ref().is_some_and(|place| place.team == mri)
    }

    /// The items a new message makes: `thread` are the people already in its reply chain, for a channel's reply.
    pub(super) fn of_post(&self, row: &Message, body: &Value, thread: &[String]) -> Vec<Draft> {
        if streams::name_of(&self.chat.id).is_some() || !is_user_message(body) {
            return Vec::new();
        }
        let mut drafts = self.of_mentions(row, body);
        let reply = row.root_id.is_some_and(|root| root != row.id);
        for person in &self.people {
            if person.disabled || person.id == row.owner_id || person.muted() || self.place.is_none() {
                continue;
            }
            if drafts.iter().any(|draft| draft.user == person.id) {
                continue;
            }
            let what = if reply && (thread.contains(&person.id) || person.follows_all()) {
                What::Reply
            } else if person.wants_all(reply) {
                What::Followed
            } else {
                continue;
            };
            drafts.push(self.draft(person, row, what, FEED_KEY));
        }
        self.readable(row.id, drafts)
    }

    /// What `effect`, a change `actor` made to `row`, does to the message's items: `before` is the message as it
    /// was and `body` as it is now.
    pub(super) fn plan(&self, effect: Effect<'_>, row: &Message, before: &Value, body: &Value, actor: &str) -> Plan {
        match effect {
            Effect::None => Plan::default(),
            Effect::Reacted(key) => self.of_reaction(row, actor, key),
            Effect::Unreacted(_) => self.of_unreaction(row, body, actor),
            Effect::Edited => self.of_edit(row, body),
            Effect::Deleted => Plan {
                gone: self.items_of(row, before),
                ..Plan::default()
            },
        }
    }

    /// An edit's mentions rewrite the items they made and tell the people they newly name; the message's other
    /// items take its new wording, and a mention's item of someone no longer named goes.
    fn of_edit(&self, row: &Message, body: &Value) -> Plan {
        let mut drafts = self.mentioned(row, body, true);
        for draft in &mut drafts {
            let quiet = self.person(&draft.user).is_some_and(Person::muted) && draft.what != PERSON;
            draft.write = if quiet { Write::Revise } else { Write::Keep };
        }
        let mut reworded = self.items_of(row, body);
        reworded.retain(|spot| drafts.iter().all(|draft| draft.spot() != *spot));
        Plan {
            drafts,
            reworded,
            ..Plan::default()
        }
    }

    /// The items of the people a message's mentions reach: named themselves, or taken in by one of everyone.
    fn of_mentions(&self, row: &Message, body: &Value) -> Vec<Draft> {
        self.mentioned(row, body, false)
    }

    /// [`Self::of_mentions`]; with `muted`, also of those a mention of everyone would reach had they not muted the
    /// thread.
    fn mentioned(&self, row: &Message, body: &Value, muted: bool) -> Vec<Draft> {
        let mut reached: Vec<(&Person, What)> = Vec::new();
        for (person, what) in mentions(body).iter().flat_map(|mention| self.reach(mention, muted)) {
            if person.id == row.owner_id {
                continue;
            }
            match reached.iter_mut().find(|(known, _)| known.id == person.id) {
                Some(known) if what == PERSON => known.1 = what,
                Some(_) => {}
                None => reached.push((person, what)),
            }
        }
        let mut drafts = Vec::new();
        for (person, what) in reached {
            drafts.push(self.draft(person, row, what, FEED_KEY));
            if what == PERSON {
                let mut own = self.draft(person, row, what, MENTIONS_KEY);
                own.stream = streams::MENTIONS;
                drafts.push(own);
            }
        }
        self.readable(row.id, drafts)
    }

    /// Whom one mention reaches: a person of the thread, or everyone in it when it names the thread itself.
    fn reach(&self, mention: &Value, muted: bool) -> Vec<(&Person, What)> {
        let Some(mri) = mention["mri"].as_str() else {
            return Vec::new();
        };
        let kind = mention["mentionType"].as_str().unwrap_or("person");
        match (kind, mri.strip_prefix("8:orgid:")) {
            ("person", Some(id)) => self.person(id).map(|person| (person, PERSON)).into_iter().collect(),
            ("everyone" | "team" | "channel", None) if self.names(mri) && !self.chat.unique_roster => {
                let what = What::Mention(broad(kind));
                let wide = self
                    .people
                    .iter()
                    .filter(|person| !person.disabled && (muted || !person.muted()));
                wide.map(|person| (person, what)).collect()
            }
            _ => Vec::new(),
        }
    }

    /// `actor`'s reaction `key` to `row` is new activity for the message's sender: the item of an earlier reaction
    /// of theirs goes, and a new one is written.
    fn of_reaction(&self, row: &Message, actor: &str, key: &str) -> Plan {
        let told = self.person(&row.owner_id).filter(|owner| !owner.muted());
        let Some(draft) = told.and_then(|owner| self.reaction(owner, row, actor, key)) else {
            return Plan::default();
        };
        Plan {
            gone: vec![draft.spot()],
            drafts: vec![draft],
            ..Plan::default()
        }
    }

    /// A reaction `actor` took back from `row`, now `body`: their item names the newest reaction of theirs that
    /// stays, and goes with the last. The item of a sender who left the thread is no longer changed.
    fn of_unreaction(&self, row: &Message, body: &Value, actor: &str) -> Plan {
        let Some(owner) = self.people.iter().find(|person| person.id == row.owner_id) else {
            return Plan::default();
        };
        let left = reaction_left(body, actor).unwrap_or_default();
        let Some(mut draft) = self.reaction(owner, row, actor, &left) else {
            return Plan::default();
        };
        if left.is_empty() {
            return Plan {
                gone: vec![draft.spot()],
                ..Plan::default()
            };
        }
        draft.write = Write::Revise;
        Plan {
            drafts: vec![draft],
            ..Plan::default()
        }
    }

    /// The item `actor`'s reaction `key` to `row` makes for the message's sender `owner`.
    fn reaction(&self, owner: &Person, row: &Message, actor: &str, key: &str) -> Option<Draft> {
        if streams::name_of(&self.chat.id).is_some() || actor == row.owner_id || owner.from > row.id {
            return None;
        }
        let mut draft = self.draft(owner, row, What::Reaction, FEED_KEY);
        draft.key = reaction_key(&self.chat.id, row.id, actor);
        key.clone_into(&mut draft.subtype);
        actor.clone_into(&mut draft.actor);
        Some(draft)
    }

    /// Where every item of `row` is: its mentions', its replies' and its reactions'.
    fn items_of(&self, row: &Message, body: &Value) -> Vec<Spot> {
        let mut found: Vec<Spot> = Vec::new();
        for person in self.people.iter().filter(|person| person.id != row.owner_id) {
            for (stream, kind) in [(streams::FEED, FEED_KEY), (streams::MENTIONS, MENTIONS_KEY)] {
                found.push((
                    streams::stored_id(stream, &person.id),
                    person.id.clone(),
                    item_key(kind, &self.chat.id, row.id),
                ));
            }
        }
        let reactors = body["properties"]["emotions"].as_array().into_iter().flatten();
        for user in reactors.flat_map(|emotion| emotion["users"].as_array().into_iter().flatten()) {
            if let Some(actor) = user["mri"].as_str().and_then(|mri| mri.strip_prefix("8:orgid:")) {
                found.push((
                    streams::stored_id(streams::FEED, &row.owner_id),
                    row.owner_id.clone(),
                    reaction_key(&self.chat.id, row.id, actor),
                ));
            }
        }
        found
    }

    fn draft(&self, person: &Person, row: &Message, what: What, kind: &str) -> Draft {
        let subtype = match what {
            PERSON if self.place.is_none() && self.chat.unique_roster => "mention",
            What::Mention(kind) => kind,
            What::Reply => "reply",
            What::Followed => "channelNewMessage",
            What::Reaction => "like",
        };
        Draft {
            user: person.id.clone(),
            stream: streams::FEED,
            key: item_key(kind, &self.chat.id, row.id),
            write: Write::Once,
            what,
            subtype: subtype.to_owned(),
            actor: row.owner_id.clone(),
        }
    }

    fn activity_type(&self, draft: &Draft) -> &'static str {
        let chat = self.place.is_none();
        match draft.what {
            What::Mention(_) if draft.stream == streams::MENTIONS => "mentions",
            What::Mention(_) if chat && self.chat.unique_roster => "mentions",
            What::Mention(_) if chat => "mentionInChat",
            What::Mention(_) => "mention",
            What::Reply => "reply",
            What::Followed => "follow",
            What::Reaction if chat => "reactionInChat",
            What::Reaction => "reaction",
        }
    }

    /// The stream message of `draft` about the message `source`, with the item's own `id`.
    pub(super) fn item(&self, base: &str, draft: &Draft, source: &Value, actor_name: &str, id: i64) -> Value {
        let chat = &self.chat.id;
        let message = number(&source["id"]);
        let reference = format!("T_{chat}_M_{message}");
        let root = source["conversationLink"]
            .as_str()
            .and_then(|link| link.split_once(";messageid=")?.1.parse::<i64>().ok())
            .unwrap_or(message);
        let mut activity = json!({
            "activityType": self.activity_type(draft), "activitySubtype": draft.subtype,
            "activityTimestamp": wire::iso(id), "activityId": id,
            "sourceThreadId": chat, "sourceMessageId": message, "sourceMessageVersion": number(&source["version"]),
            "sourceReplyChainId": root, "sourceUserId": format!("8:orgid:{}", draft.actor),
            "sourceUserImDisplayName": actor_name, "targetUserId": format!("8:orgid:{}", draft.user),
            "messagePreview": preview(source), "messagePreviewTemplateOption": "hasText",
            "activityContext": {}, "sourceThreadType": self.thread_type(),
            "sourceThreadRosterNonBotMemberCount": self.people.len(),
            "sourceThreadIsPrivateChannel": self.place.as_ref().is_some_and(|place| place.private).to_string(),
        });
        if draft.what == PERSON {
            activity["activityReason"] = json!("mention");
        }
        if let Some(topic) = self.topic() {
            activity["sourceThreadTopic"] = json!(topic);
        }
        let views = json!({"version": "1.0", "viewReferences": [
            {"view": "ActivityFeed", "stream": streams::FEED, "secondaryReferenceId": reference},
            {"view": "Mentions", "stream": streams::MENTIONS, "secondaryReferenceId": reference},
        ]});
        let link = format!("{base}/v1/users/ME");
        let kind = draft.stream.trim_start_matches("48:");
        let mut properties = json!({"activity": activity, "isread": false, "messageUpdatePolicyValue": message,
            "views": views.to_string(), "s2spartnername": "skypespaces"});
        let (message_type, content) = if matches!(draft.what, What::Mention(_)) {
            properties["mentions"] = source["properties"]["mentions"].clone();
            ("RichText/Html", source["content"].clone())
        } else {
            ("Text", json!(""))
        };
        json!({
            "type": "Message", "id": id.to_string(), "version": id.to_string(), "sequenceId": id,
            "clientmessageid": id.to_string(), "messagetype": message_type, "contenttype": message_type,
            "content": content, "conversationid": draft.stream, "to": draft.stream,
            "threadtype": format!("streamof{kind}"),
            "conversationLink": format!("{link}/conversations/{}", draft.stream),
            "from": format!("{link}/contacts/8:orgid:{}", draft.user), "imdisplayname": "",
            "s2spartnername": "skypespaces", "clumpId": chat, "secondaryReferenceId": reference,
            "composetime": wire::iso(id), "originalarrivaltime": wire::iso(id),
            "properties": properties,
        })
    }
}

const PERSON: What = What::Mention("person");
const FEED_KEY: &str = "item";
const MENTIONS_KEY: &str = "mention";

fn item_key(kind: &str, chat: &str, message: i64) -> String {
    format!("{kind}|T_{chat}_M_{message}")
}

fn reaction_key(chat: &str, message: i64, actor: &str) -> String {
    format!("reaction|T_{chat}_M_{message}|{actor}")
}

fn broad(kind: &str) -> &'static str {
    match kind {
        "team" => "team",
        "channel" => "channel",
        _ => "everyone",
    }
}

fn is_user_message(body: &Value) -> bool {
    body["messagetype"]
        .as_str()
        .is_some_and(|kind| kind == "Text" || kind.starts_with("RichText"))
}

fn number(value: &Value) -> i64 {
    value
        .as_i64()
        .or_else(|| value.as_str()?.parse().ok())
        .unwrap_or_default()
}

/// A message's mentions: `properties.mentions`, a list or its JSON text.
fn mentions(body: &Value) -> Vec<Value> {
    match &body["properties"]["mentions"] {
        Value::Array(list) => list.clone(),
        Value::String(text) => serde_json::from_str(text).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// A message's words without its markup, spaces folded, at most [`PREVIEW`] characters.
fn preview(source: &Value) -> String {
    let content = source["content"].as_str().unwrap_or_default();
    let text = if source["messagetype"] == "Text" {
        content.to_owned()
    } else {
        let html = scraper::Html::parse_fragment(content);
        html.root_element().text().collect::<Vec<_>>().join(" ")
    };
    let folded = text.split_whitespace().collect::<Vec<_>>().join(" ");
    folded.chars().take(PREVIEW).collect()
}

/// A removed item: what a deleted message leaves.
pub(super) fn removed(body: &mut Value, at: i64) {
    body["content"] = "".into();
    body["properties"] = json!({"deletetime": at});
    body["version"] = at.to_string().into();
}

/// [`removed`], as a change of an item that frees its key.
pub(super) fn gone(item: &mut Value, at: i64) -> bool {
    removed(item, at);
    false
}

/// An item after its message's edit to `source`: a mention's goes, as the edit no longer makes it; any other takes
/// the new wording. Whether the item stays.
pub(super) fn reworded(item: &mut Value, source: &Value, at: i64) -> bool {
    let kind = item["properties"]["activity"]["activityType"].as_str();
    if matches!(kind, Some("mentions" | "mentionInChat" | "mention")) {
        return gone(item, at);
    }
    let activity = &mut item["properties"]["activity"];
    activity["messagePreview"] = preview(source).into();
    activity["sourceMessageVersion"] = number(&source["version"]).into();
    item["version"] = at.to_string().into();
    true
}

/// Whether a stored item is still there: not removed by its account or by a change of its message.
pub(super) fn live(item: &Value) -> bool {
    item["properties"].get("deletetime").is_none()
}

/// The key of `actor`'s newest reaction in `body`.
fn reaction_left(body: &Value, actor: &str) -> Option<String> {
    let mri = format!("8:orgid:{actor}");
    let emotions = body["properties"]["emotions"].as_array()?;
    let theirs = emotions.iter().filter_map(|emotion| {
        let users = emotion["users"].as_array()?;
        let given = users.iter().find(|user| user["mri"] == mri.as_str())?;
        Some((given["time"].as_i64()?, emotion["key"].as_str()?))
    });
    theirs.max_by_key(|(time, _)| *time).map(|(_, key)| key.to_owned())
}

/// Reads the [`Scene`] of thread `$chat` inside a transaction that holds the clock.
macro_rules! scene {
    ($conn:expr, $chat:expr) => {{
        use $crate::chatsvc::schema::{chat_members, chat_properties, chats};
        use $crate::db::schema::users;
        use $crate::teams::schema::{channels, teams};
        let chat: $crate::chatsvc::repo::Chat = chats::table
            .find($chat)
            .select($crate::chatsvc::repo::Chat::as_select())
            .first($conn)
            .await?;
        let members: Vec<(String, bool, i64)> = chat_members::table
            .inner_join(users::table.on(users::id.eq(chat_members::user_id)))
            .filter(chat_members::chat_id.eq($chat))
            .filter(chat_members::left_at.is_null())
            .order(chat_members::position)
            .select((chat_members::user_id, users::disabled, chat_members::history_from))
            .load($conn)
            .await?;
        let mut own: std::collections::HashMap<String, String> = chat_properties::table
            .filter(chat_properties::chat_id.eq($chat))
            .select((chat_properties::user_id, chat_properties::body))
            .load::<(String, String)>($conn)
            .await?
            .into_iter()
            .collect();
        let place: Option<(String, bool, String)> = channels::table
            .inner_join(teams::table)
            .filter(channels::id.eq($chat))
            .select((channels::team_id, channels::private, teams::name))
            .first($conn)
            .await
            .optional()?;
        let mut people = Vec::new();
        for (id, disabled, from) in members {
            let properties = $crate::chatsvc::property_repo::decode(own.remove(&id))?;
            people.push($crate::chatsvc::activity::Person {
                id,
                disabled,
                from,
                properties,
            });
        }
        let place = place.map(|(team, private, team_name)| $crate::chatsvc::activity::Place {
            team,
            team_name,
            private,
        });
        $crate::chatsvc::activity::Scene::new(chat, people, place)
    }};
}
pub(super) use scene;

/// Writes `$drafts` about the message `$source` (its body) into their streams; the [`Told`] of each one written.
macro_rules! put {
    ($conn:expr, $at:expr, $base:expr, $scene:expr, $source:expr, $drafts:expr) => {{
        use $crate::chatsvc::activity::Step;
        use $crate::chatsvc::schema::{chat_messages, chats};
        let actor_names: std::collections::HashMap<String, String> = $crate::db::schema::users::table
            .filter($crate::db::schema::users::id.eq_any($drafts.iter().map(|draft| &draft.actor)))
            .select(($crate::db::schema::users::id, $crate::db::schema::users::display_name))
            .load::<(String, String)>($conn)
            .await?
            .into_iter()
            .collect();
        let mut told: Vec<$crate::chatsvc::activity::Told> = Vec::new();
        for draft in $drafts.iter() {
            let (stream, _, _) = draft.spot();
            let existing: Option<$crate::chatsvc::message_repo::Message> = chat_messages::table
                .filter(chat_messages::chat_id.eq(&stream))
                .filter(chat_messages::owner_id.eq(&draft.user))
                .filter(chat_messages::client_id.eq(&draft.key))
                .select($crate::chatsvc::message_repo::Message::as_select())
                .first($conn)
                .await
                .optional()?;
            let step = draft.step(existing.as_ref())?;
            if matches!(step, Step::Skip) {
                continue;
            }
            let version: i64 = $crate::chatsvc::repo::advance!($conn, $at);
            let name = actor_names.get(&draft.actor).map_or("", String::as_str);
            let (id, kind) = match &step {
                Step::Rewrite { id, .. } => (*id, "MessageUpdate"),
                _ => (version, "NewMessage"),
            };
            let mut resource = $scene.item($base, draft, $source, name, id);
            resource["version"] = version.to_string().into();
            if let Step::Rewrite { read, .. } = step {
                resource["properties"]["isread"] = read;
            } else {
                $crate::chatsvc::streams::ensure_in!($conn, &stream, &draft.user, version);
            }
            let row = $crate::chatsvc::message_repo::Message {
                chat_id: stream.clone(),
                id,
                owner_id: draft.user.clone(),
                client_id: Some(draft.key.clone()),
                version,
                body: resource.to_string(),
                root_id: None,
            };
            diesel::insert_into(chat_messages::table)
                .values(&row)
                .on_conflict((chat_messages::chat_id, chat_messages::id))
                .do_update()
                .set((chat_messages::version.eq(version), chat_messages::body.eq(&row.body)))
                .execute($conn)
                .await?;
            diesel::update(chats::table.find(&stream))
                .set(chats::version.eq(version))
                .execute($conn)
                .await?;
            told.push($crate::chatsvc::activity::Told {
                user: draft.user.clone(),
                kind,
                resource,
            });
        }
        told
    }};
}
pub(super) use put;

/// Changes the items at `$spots` that are there with `$change` (the item and the change's version; whether the
/// item stays); the [`Told`] of each one changed. An item that goes, or is gone already, gives its key up: what
/// its message makes later is a new item.
macro_rules! amend {
    ($conn:expr, $at:expr, $spots:expr, $change:expr) => {{
        use $crate::chatsvc::schema::{chat_messages, chats};
        let mut told: Vec<$crate::chatsvc::activity::Told> = Vec::new();
        for (stream, user, key) in $spots {
            let found: Option<$crate::chatsvc::message_repo::Message> = chat_messages::table
                .filter(chat_messages::chat_id.eq(&stream))
                .filter(chat_messages::owner_id.eq(&user))
                .filter(chat_messages::client_id.eq(&key))
                .select($crate::chatsvc::message_repo::Message::as_select())
                .first($conn)
                .await
                .optional()?;
            let Some(row) = found else { continue };
            let mut resource = row.value()?;
            if !$crate::chatsvc::activity::live(&resource) {
                diesel::update(chat_messages::table.find((&stream, row.id)))
                    .set(chat_messages::client_id.eq(None::<String>))
                    .execute($conn)
                    .await?;
                continue;
            }
            let version: i64 = $crate::chatsvc::repo::advance!($conn, $at);
            let stays = ($change)(&mut resource, version);
            diesel::update(chat_messages::table.find((&stream, row.id)))
                .set((
                    chat_messages::client_id.eq(stays.then_some(key)),
                    chat_messages::version.eq(version),
                    chat_messages::body.eq(resource.to_string()),
                ))
                .execute($conn)
                .await?;
            diesel::update(chats::table.find(&stream))
                .set(chats::version.eq(version))
                .execute($conn)
                .await?;
            told.push($crate::chatsvc::activity::Told {
                user,
                kind: "MessageUpdate",
                resource,
            });
        }
        told
    }};
}
pub(super) use amend;
