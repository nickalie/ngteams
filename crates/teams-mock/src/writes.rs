//! Writes: message posts and typing, edits, soft deletes, reactions, read receipts and chat creation. Each one changes
//! the state as the service did in the captures behind the fixtures (`docs/protocol/chat.md`), answers as its fixture
//! recorded, and queues a [`crate::ChatEvent`] for every other member of the chat.

use serde_json::{Map, Value, json};
use wiremock::ResponseTemplate;

use crate::chatsvc::{Call, json, status};
use crate::fixtures::object;
use crate::reads::{member_of, thread_body};
use crate::world::{self, Kind, State, iso_time};
use crate::{EventKind, NOTIFICATIONS};

/// The string-valued message properties a post or an edit carries through as sent (JSON strings of lists).
const LISTED_PROPERTIES: [&str; 4] = ["mentions", "cards", "links", "files"];

/// The message keys a post carries through as sent, when it has them.
const SENT_KEYS: [&str; 4] = ["clientmessageid", "contenttype", "imdisplayname", "amsreferences"];

/// `POST …/conversations/<chat>/messages`: a message (`Text`, `RichText`, `RichText/…`) is stored as a read gives it
/// back and answered 201 with its arrival time (its id) and a `Location`; typing (`Control/Typing`,
/// `Control/ClearTyping`) is only an event, answered with an arrival time too.
pub(crate) fn post(state: &mut State, call: &Call<'_>, chat: &str) -> ResponseTemplate {
    if let Some((channel, post)) = chat.split_once(";messageid=") {
        return reply(state, call, channel, post);
    }
    if let Err(code) = writable(state, chat, &call.caller) {
        return status(code);
    }
    let Some(body) = call.object() else {
        return status(400);
    };
    let (Some(messagetype), Some(_)) = (text(&body, "messagetype"), text(&body, "content")) else {
        return status(400);
    };
    if matches!(messagetype, "Control/Typing" | "Control/ClearTyping") {
        let arrival = state.tick();
        state.queue(
            chat,
            &call.caller,
            EventKind::Typing,
            &json!({"messagetype": messagetype}),
        );
        return posted(state, arrival, None);
    }
    if !is_message(messagetype) {
        return status(400);
    }
    let arrival = state.tick();
    let Some(message) = new_message(state, &call.caller, chat, &body, arrival) else {
        return status(400);
    };
    store(state, &call.caller, chat, None, message, arrival);
    let location = format!(
        "{}/v1/users/ME/conversations/{chat}/messages/{arrival}",
        state.location_base()
    );
    posted(state, arrival, Some(location))
}

/// Stores `from`'s `message`, arrived at `arrival`, in `chat` and queues it, as a read gives it back: a team channel's
/// message names its chain's post (`conversationLink` `…/<channel>;messageid=<post>`), the post `post` for a reply
/// in its thread, its own id for a post, as every channel message a read gave did.
fn store(state: &mut State, from: &str, chat: &str, post: Option<&str>, mut message: Value, arrival: u64) {
    if state.conversation(chat).is_some_and(world::Conversation::is_channel) {
        let post = post.map_or_else(|| arrival.to_string(), str::to_owned);
        object(&mut message).insert(
            "conversationLink".to_owned(),
            json!(format!(
                "{}/v1/users/ME/conversations/{chat};messageid={post}",
                state.chat_base()
            )),
        );
    }
    if let Some(conversation) = state.conversation_mut(chat) {
        conversation.messages.push(message.clone());
        conversation.version = arrival;
    }
    state.queue(chat, from, EventKind::Message, &message);
}

/// `POST …/conversations/<channel>;messageid=<post>/messages`: a reply in a channel post's thread, posted to the
/// post's chain (`docs/protocol/chat-service.md`, "Ids"), stored with the channel's messages as a read gives a reply
/// back: the channel's `conversationid`, a `conversationLink` naming the post. 404 when the channel holds no such
/// post.
fn reply(state: &mut State, call: &Call<'_>, channel: &str, post: &str) -> ResponseTemplate {
    if let Err(code) = writable(state, channel, &call.caller) {
        return status(code);
    }
    if state
        .conversation(channel)
        .is_none_or(|conversation| !conversation.messages.iter().any(|message| message["id"] == post))
    {
        return status(404);
    }
    let Some(body) = call.object() else {
        return status(400);
    };
    if !text(&body, "messagetype").is_some_and(is_message) || text(&body, "content").is_none() {
        return status(400);
    }
    let arrival = state.tick();
    let Some(message) = new_message(state, &call.caller, channel, &body, arrival) else {
        return status(400);
    };
    store(state, &call.caller, channel, Some(post), message, arrival);
    let location = format!(
        "{}/v1/users/ME/conversations/{channel};messageid={post}/messages/{arrival}",
        state.location_base()
    );
    posted(state, arrival, Some(location))
}

/// `from`'s rich-text post of `html` to `chat`, as [`post`] stores and queues a client's: its id, or `None` when `from`
/// may not write there.
pub(crate) fn post_text(state: &mut State, from: &str, chat: &str, html: &str) -> Option<String> {
    writable(state, chat, from).ok()?;
    let arrival = state.tick();
    let display = state.account(from)?.display_name.clone();
    // a client's post as `text_message` in the mock's tests sends it: a client message id of its own, the sender's name
    let body = json!({
        "messagetype": "RichText/Html",
        "contenttype": "Text",
        "content": html,
        "clientmessageid": (9_000_000_000_000_000_000_u64 + arrival).to_string(),
        "imdisplayname": display,
        "amsreferences": [],
        "properties": {"mentions": "[]", "files": "[]", "formatVariant": "TEAMS"},
    });
    let message = new_message(state, from, chat, body.as_object()?, arrival)?;
    state.conversation(chat)?;
    store(state, from, chat, None, message, arrival);
    Some(arrival.to_string())
}

/// `PUT …/messages/<id>`: the whole message again (`edit.json`). The sender's own standing message takes the new
/// `content` and the properties sent; its `version` and `properties.edittime` are the service's time, and
/// `properties.composetime` is the one sent.
pub(crate) fn edit(state: &mut State, call: &Call<'_>, chat: &str, id: &str) -> ResponseTemplate {
    if let Err(code) = writable(state, chat, &call.caller) {
        return status(code);
    }
    let Some(body) = call.object() else {
        return status(400);
    };
    let (Some(messagetype), Some(content)) = (text(&body, "messagetype"), text(&body, "content")) else {
        return status(400);
    };
    let Some(sent) = sent_properties(body.get("properties")).filter(|_| is_message(messagetype)) else {
        return status(400);
    };
    let caller = call.caller.as_str();
    let contact = state.contact(caller);
    let changed = change(state, chat, id, caller, EventKind::Edit, |message, time| {
        own_and_standing(message, &contact)?;
        let fields = object(message);
        fields.insert("content".to_owned(), json!(stored_content(content)));
        fields.insert("version".to_owned(), json!(time.to_string()));
        let properties = object(fields.entry("properties").or_insert(Value::Null));
        properties.extend(sent);
        properties.insert("edittime".to_owned(), json!(time));
        if let Some(composetime) = body.get("composetime") {
            properties.insert("composetime".to_owned(), composetime.clone());
        }
        Ok(())
    });
    changed.map_or_else(status, |()| state.fixtures.edited.template())
}

/// `DELETE …/messages/<id>?behavior=softDelete` (`delete.json`): the sender's own message keeps its id and
/// `clientmessageid`, loses its content and every property but `deletetime`, and gains `skypeeditedid`.
pub(crate) fn delete(state: &mut State, call: &Call<'_>, chat: &str, id: &str) -> ResponseTemplate {
    // the activity feed takes no posts, but its owner removes its items (`docs/protocol/chat.md`)
    let allowed = if chat == NOTIFICATIONS {
        member_of(state, chat, &call.caller).map(|_| ())
    } else {
        writable(state, chat, &call.caller)
    };
    if let Err(code) = allowed {
        return status(code);
    }
    if call.query("behavior") != Some("softDelete") {
        return status(404);
    }
    let caller = call.caller.as_str();
    let contact = state.contact(caller);
    let changed = change(state, chat, id, caller, EventKind::Delete, |message, time| {
        own_and_standing(message, &contact)?;
        let fields = object(message);
        fields.insert("content".to_owned(), json!(""));
        fields.insert("version".to_owned(), json!(time.to_string()));
        fields.insert("properties".to_owned(), json!({"deletetime": time}));
        if let Some(client_id) = fields.get("clientmessageid").cloned() {
            fields.insert("skypeeditedid".to_owned(), client_id);
        }
        fields.remove("amsreferences");
        fields.remove("annotationsSummary");
        Ok(())
    });
    changed.map_or_else(status, |()| state.fixtures.deleted.template())
}

/// `PUT`/`DELETE …/messages/<id>/properties?name=emotions` (`reaction.json`): the caller's reaction under its key,
/// with the service's time and the value sent as a string; taken back, the key stays with the users left. The
/// message's `version` moves, and `annotationsSummary` counts the standing reactions per key. Taking back a reaction
/// the caller doesn't have (a retry) changes nothing and tells nobody, answered as a take-back: the captures only
/// showed one of a standing reaction.
pub(crate) fn reaction(state: &mut State, call: &Call<'_>, chat: &str, id: &str, remove: bool) -> ResponseTemplate {
    if let Err(code) = writable(state, chat, &call.caller) {
        return status(code);
    }
    if call.query("name") != Some("emotions") {
        return status(404);
    }
    let Some((key, value)) = emotion(call, remove) else {
        return status(400);
    };
    let caller = call.caller.as_str();
    if remove {
        match has_reaction(state, chat, id, &key, caller) {
            Ok(true) => {}
            Ok(false) => return state.fixtures.unreacted.template(),
            Err(code) => return status(code),
        }
    }
    let changed = change(state, chat, id, caller, EventKind::Reaction, |message, time| {
        if is_deleted(message) {
            return Err(404);
        }
        let user = value.map(|value| json!({"mri": caller, "time": time, "value": value}));
        react(message, &key, caller, user);
        object(message).insert("version".to_owned(), json!(time.to_string()));
        Ok(())
    });
    let fixture = if remove {
        &state.fixtures.unreacted
    } else {
        &state.fixtures.reacted
    };
    changed.map_or_else(status, |()| fixture.template())
}

/// `PUT …/conversations/48:notifications/messages/<id>/properties?name=isread` with `{"isread": "true"|"false"}`: an
/// item of the caller's activity feed marked read or unread (`docs/protocol/chat.md`, "The activity feed"). The item's
/// `isread` becomes the boolean and its `version` the service's time, answered 200 with an empty body; the change is
/// told to the caller as the item's edit.
pub(crate) fn activity_read(state: &mut State, call: &Call<'_>, chat: &str, id: &str) -> ResponseTemplate {
    // only the feed's items have a read mark; the feed takes no posts, but its owner marks its items
    if chat != NOTIFICATIONS {
        return status(404);
    }
    if let Err(code) = member_of(state, chat, &call.caller) {
        return status(code);
    }
    let read = match call
        .object()
        .and_then(|body| body.get("isread")?.as_str().map(str::to_owned))
    {
        Some(text) if text == "true" => true,
        Some(text) if text == "false" => false,
        _ => return status(400),
    };
    let caller = call.caller.clone();
    let changed = change(state, chat, id, &caller, EventKind::Edit, |message, time| {
        if is_deleted(message) {
            return Err(404);
        }
        let fields = object(message);
        object(fields.entry("properties").or_insert(Value::Null)).insert("isread".to_owned(), json!(read));
        fields.insert("version".to_owned(), json!(time.to_string()));
        Ok(())
    });
    changed.map_or_else(status, |()| status(200))
}

/// `PUT …/conversations/<chat>/properties?name=consumptionhorizon`: the caller's own read horizon, answered 200 with
/// an empty body (`docs/protocol/chat.md`).
pub(crate) fn read_horizon(state: &mut State, call: &Call<'_>, chat: &str) -> ResponseTemplate {
    // a horizon is the reader's own: a stream that takes no posts (the activity feed) takes its member's
    if let Err(code) = member_of(state, chat, &call.caller) {
        return status(code);
    }
    if call.query("name") != Some("consumptionhorizon") {
        return status(404);
    }
    let Some(horizon) = call
        .object()
        .and_then(|body| body.get("consumptionhorizon")?.as_str().map(str::to_owned))
    else {
        return status(400);
    };
    let time = state.tick();
    if let Some(conversation) = state.conversation_mut(chat) {
        conversation.horizons.insert(call.caller.clone(), horizon.clone());
        conversation.version = time;
    }
    let resource = json!({"consumptionhorizon": horizon});
    state.queue(chat, &call.caller, EventKind::ReadReceipt, &resource);
    status(200)
}

/// `PUT …/conversations/<chat>/properties?name=<channelNotificationSettings|followAllRc|alerts>`: one of the caller's
/// own channel preferences (`alerts` `false`: a channel the classic client muted), its text value kept for the caller's own read of the conversation, answered 200
/// (`docs/protocol/channels.md`). Nobody else is told.
pub(crate) fn own_property(state: &mut State, call: &Call<'_>, chat: &str) -> ResponseTemplate {
    if let Err(code) = member_of(state, chat, &call.caller) {
        return status(code);
    }
    let Some(name) = call.query("name") else {
        return status(400);
    };
    let Some(value) = call
        .object()
        .and_then(|body| body.get(name)?.as_str().map(str::to_owned))
    else {
        return status(400);
    };
    // A bookmark is returned by the changed-conversation listing as well as the direct read.
    if name == "consumptionHorizonBookmark" {
        let version = state.tick();
        if let Some(conversation) = state.conversation_mut(chat) {
            conversation.version = version;
        }
    }
    state
        .own_properties
        .entry((call.caller.clone(), chat.to_owned()))
        .or_default()
        .insert(name.to_owned(), Value::String(value));
    status(200)
}

/// `POST v1/threads` (`docs/protocol/chat-service.md`): `{members: [{id, role}], properties: {threadType: "chat"}}`
/// with the caller among the members. With `uniquerosterthread` and two members it is their 1:1 chat (the one there
/// is, if any), otherwise a new group chat. Answered 201 with `{"threadResource": <the thread read>}` and a
/// `Location`.
pub(crate) fn create_chat(state: &mut State, call: &Call<'_>) -> ResponseTemplate {
    let Some((members, unique)) = chat_members(call) else {
        return status(400);
    };
    if !members.contains(&call.caller) {
        return status(403);
    }
    let id = if unique {
        match one_to_one_id(&members) {
            Some(id) => id,
            None => return status(400),
        }
    } else {
        state.new_group_id()
    };
    if state.conversation(&id).is_none() {
        let Some(empty) = state.fixtures.entries.get(1).cloned() else {
            return status(500);
        };
        let time = state.tick();
        let kind = if unique { Kind::OneToOne } else { Kind::Group };
        // the caller first: the creator
        let mut ordered: Vec<&str> = vec![call.caller.as_str()];
        ordered.extend(
            members
                .iter()
                .map(String::as_str)
                .filter(|member| *member != call.caller),
        );
        let mut conversation = world::created(&empty, &id, kind, &ordered, time);
        // a named group chat (`properties.topic`, New message's "Add group name") lists with its name
        if let Some(topic) = chat_topic(call).filter(|_| !unique) {
            conversation.entry["threadProperties"]["topic"] = json!(topic);
        }
        let thread = thread_body(state, &conversation);
        state.conversations.push(conversation);
        state.queue(&id, &call.caller, EventKind::ChatCreated, &thread);
    }
    let Some(conversation) = state.conversation(&id) else {
        return status(500);
    };
    json(201, &json!({"threadResource": thread_body(state, conversation)})).insert_header(
        "Location",
        format!("{}/v1/threads/{id}", state.location_base()).as_str(),
    )
}

/// `PUT v1/threads/<id>/members` (`docs/protocol/chat-service.md`, "Adding people to a chat"): `{members: [{id,
/// role, shareHistoryTime}]}` from a member of the group chat `id`; the people not in it yet join it, its roster moves
/// on, and it is answered 200. A 1:1 chat or a stream takes nobody (400), as the web client offers no add there.
pub(crate) fn add_members(state: &mut State, call: &Call<'_>, id: &str) -> ResponseTemplate {
    let kind = match member_of(state, id, &call.caller) {
        Ok(conversation) => conversation.kind,
        Err(code) => return status(code),
    };
    let added: Option<Vec<String>> = call
        .object()
        .and_then(|body| body.get("members")?.as_array().cloned())
        .and_then(|members| {
            members
                .iter()
                .map(|member| {
                    member.get("shareHistoryTime")?.as_i64()?;
                    member
                        .get("id")?
                        .as_str()
                        .filter(|id| is_orgid_mri(id))
                        .map(str::to_owned)
                })
                .collect()
        });
    let Some(added) = added.filter(|added| !added.is_empty()) else {
        return status(400);
    };
    if kind != Kind::Group {
        return status(400);
    }
    let time = state.tick();
    let Some(conversation) = state.conversation_mut(id) else {
        return status(404);
    };
    for member in added {
        if !conversation.is_member(&member) {
            conversation.members.push(member);
        }
    }
    conversation.roster = time;
    conversation.version = time;
    conversation.entry["threadProperties"]["rosterVersion"] = json!(time);
    state.changed();
    status(200)
}

/// Whether `caller` may write to the chat `id`: a member, and not of a read-only conversation (403).
fn writable(state: &State, id: &str, caller: &str) -> Result<(), u16> {
    let conversation = member_of(state, id, caller)?;
    // a team's channel takes its members' posts, edits and reactions as a chat does; a stream takes none
    if conversation.kind == Kind::ReadOnly && !conversation.is_channel() {
        Err(403)
    } else {
        Ok(())
    }
}

/// Applies `apply` to message `id` of `chat` at a new time, moves the chat's `version` to it, and queues `kind` with
/// the message as it now reads for the other members.
fn change(
    state: &mut State,
    chat: &str,
    id: &str,
    caller: &str,
    kind: EventKind,
    apply: impl FnOnce(&mut Value, u64) -> Result<(), u16>,
) -> Result<(), u16> {
    let time = state.tick();
    let conversation = state.conversation_mut(chat).ok_or(404_u16)?;
    let message = conversation.message_mut(id).ok_or(404_u16)?;
    apply(message, time)?;
    let message = message.clone();
    conversation.version = time;
    state.queue(chat, caller, kind, &message);
    Ok(())
}

/// Whether `caller` sent the message and nobody deleted it: 403 for someone else's, 404 for a deleted one.
fn own_and_standing(message: &Value, contact: &str) -> Result<(), u16> {
    if is_deleted(message) {
        return Err(404);
    }
    let from = message["from"].as_str().unwrap_or_default();
    if from == contact { Ok(()) } else { Err(403) }
}

/// Whether `caller` has a standing reaction under `key` on message `id` of `chat`: 404 for no such message, or a
/// deleted one.
fn has_reaction(state: &State, chat: &str, id: &str, key: &str, caller: &str) -> Result<bool, u16> {
    let message = state
        .conversation(chat)
        .and_then(|conversation| {
            conversation
                .messages
                .iter()
                .find(|message| message["id"].as_str() == Some(id))
        })
        .ok_or(404_u16)?;
    if is_deleted(message) {
        return Err(404);
    }
    let emotions = message["properties"]["emotions"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let reacted = |entry: &Value| {
        entry["users"]
            .as_array()
            .is_some_and(|users| users.iter().any(|user| user["mri"].as_str() == Some(caller)))
    };
    Ok(emotions
        .iter()
        .any(|entry| entry["key"].as_str() == Some(key) && reacted(entry)))
}

fn is_deleted(message: &Value) -> bool {
    message["properties"].get("deletetime").is_some()
}

/// A message `messagetype` in the captured forms (`Text`, `RichText`, `RichText/…`).
fn is_message(messagetype: &str) -> bool {
    messagetype == "Text" || messagetype == "RichText" || messagetype.starts_with("RichText/")
}

fn text<'a>(body: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    body.get(key).and_then(Value::as_str)
}

/// A post's answer (`reply.json`'s, 201 `{"OriginalArrivalTime": …}`) at `arrival`, with `location` if given.
fn posted(state: &State, arrival: u64, location: Option<String>) -> ResponseTemplate {
    let posted = &state.fixtures.posted;
    let mut body = posted.json().cloned().unwrap_or(Value::Null);
    object(&mut body).insert("OriginalArrivalTime".to_owned(), json!(arrival));
    let answer = ResponseTemplate::new(posted.status()).set_body_json(&body);
    match location {
        Some(location) => answer.insert_header("Location", location.as_str()),
        None => answer,
    }
}

/// A posted message as a read gives it back (`reply.json`'s `read_back`, `messages.json`): the service's id, version,
/// sequence and times, the sender from the token, the sent keys and properties carried through.
fn new_message(state: &State, caller: &str, chat: &str, body: &Map<String, Value>, arrival: u64) -> Option<Value> {
    let conversation = state.conversation(chat)?;
    let display = state.account(caller)?.display_name.clone();
    let properties = sent_properties(body.get("properties"))?;
    let mut message = json!({
        "sequenceId": conversation.next_sequence(),
        "conversationid": chat,
        "conversationLink": format!("{}/v1/users/ME/conversations/{chat}", state.chat_base()),
        "type": "Message",
        "fromDisplayNameInToken": display,
        "id": arrival.to_string(),
        "version": arrival.to_string(),
        "messagetype": body.get("messagetype")?,
        "content": stored_content(body.get("content")?.as_str()?),
        "from": state.contact(caller),
        "composetime": iso_time(arrival, 7),
        "originalarrivaltime": iso_time(arrival, 7),
        "properties": properties,
    });
    let fields = object(&mut message);
    for key in SENT_KEYS {
        if let Some(value) = body.get(key) {
            fields.insert(key.to_owned(), value.clone());
        }
    }
    Some(message)
}

/// A message's `content` as the service stores it: as sent, but with a line break after a quote's `</blockquote>`
/// when markup follows it straight away (`reply.json`: sent without one, read back with one).
fn stored_content(sent: &str) -> String {
    sent.replace("</blockquote><", "</blockquote>\n<")
}

/// The properties a post or an edit sent, as a read gives them back: the listed ones (JSON strings) and
/// `formatVariant` as sent, and `qtdMsgs` (sent as a JSON string) as a list of objects with
/// `hasValidMsgReferences`. `None` for properties that aren't an object, or one of these in another form.
fn sent_properties(sent: Option<&Value>) -> Option<Map<String, Value>> {
    let mut properties = Map::new();
    let Some(sent) = sent else {
        return Some(properties);
    };
    let sent = sent.as_object()?;
    for key in LISTED_PROPERTIES {
        if let Some(value) = sent.get(key) {
            properties.insert(key.to_owned(), json!(value.as_str()?));
        }
    }
    if let Some(variant) = sent.get("formatVariant") {
        properties.insert("formatVariant".to_owned(), variant.clone());
    }
    if let Some(quoted) = sent.get("qtdMsgs") {
        properties.insert("qtdMsgs".to_owned(), Value::Array(quoted_messages(quoted)?));
        properties.insert("hasValidMsgReferences".to_owned(), json!(true));
    }
    Some(properties)
}

/// `qtdMsgs` as sent (`[{"messageId": "<id>", "sender", "time"}]` in a JSON string), as read back.
fn quoted_messages(sent: &Value) -> Option<Vec<Value>> {
    let entries: Vec<Value> = serde_json::from_str(sent.as_str()?).ok()?;
    entries
        .iter()
        .map(|entry| {
            let id = entry.get("messageId")?;
            let id: u64 = id.as_u64().or_else(|| id.as_str()?.parse().ok())?;
            Some(json!({
                "messageId": id,
                "sender": entry.get("sender")?,
                "time": entry.get("time")?,
                "message": null,
                "validationResult": "Valid",
                "sharedRefId": null,
                "replyChainId": null,
            }))
        })
        .collect()
}

/// A reaction's key, and (unless taken back) its value as a string.
fn emotion(call: &Call<'_>, remove: bool) -> Option<(String, Option<String>)> {
    let body = call.object()?;
    let emotion = body.get("emotions")?.as_object()?;
    let key = emotion.get("key")?.as_str()?.to_owned();
    if remove {
        return Some((key, None));
    }
    let value = match emotion.get("value")? {
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        _ => return None,
    };
    Some((key, Some(value)))
}

/// Puts `caller`'s reaction `user` under `key` in the message's `properties.emotions` (or takes it out when `None`),
/// and counts the standing ones in `annotationsSummary`.
fn react(message: &mut Value, key: &str, caller: &str, user: Option<Value>) {
    let fields = object(message);
    let properties = object(fields.entry("properties").or_insert(Value::Null));
    let emotions = properties.entry("emotions").or_insert(Value::Null);
    if !emotions.is_array() {
        *emotions = json!([]);
    }
    let Value::Array(emotions) = emotions else {
        return;
    };
    let position = if let Some(position) = emotions.iter().position(|entry| entry["key"].as_str() == Some(key)) {
        position
    } else {
        emotions.push(json!({"key": key, "users": []}));
        emotions.len() - 1
    };
    if let Some(Value::Array(users)) = emotions.get_mut(position).and_then(|entry| entry.get_mut("users")) {
        users.retain(|standing| standing["mri"].as_str() != Some(caller));
        users.extend(user);
    }
    let counts: Map<String, Value> = emotions
        .iter()
        .filter_map(|entry| {
            let count = entry["users"].as_array()?.len();
            (count > 0).then(|| Some((entry["key"].as_str()?.to_owned(), json!(count))))?
        })
        .collect();
    let summary = if counts.is_empty() {
        json!({})
    } else {
        json!({"emotions": counts})
    };
    fields.insert("annotationsSummary".to_owned(), summary);
}

/// A chat create's members (distinct, in the body's order) and whether it asks for the unique-roster (1:1) thread;
/// `None` unless its body is `{members: [{id}, …two or more…], properties: {threadType: "chat"}}` with every `id` a
/// work account's MRI (`8:orgid:<oid>`).
fn chat_members(call: &Call<'_>) -> Option<(Vec<String>, bool)> {
    let body = call.object()?;
    let mut members: Vec<String> = Vec::new();
    for member in body.get("members")?.as_array()? {
        let id = member.get("id")?.as_str().filter(|id| is_orgid_mri(id))?;
        if !members.iter().any(|known| known == id) {
            members.push(id.to_owned());
        }
    }
    let properties = body.get("properties")?.as_object()?;
    if properties.get("threadType")?.as_str()? != "chat" || members.len() < 2 {
        return None;
    }
    let unique = properties.get("uniquerosterthread").and_then(Value::as_bool) == Some(true);
    Some((members, unique))
}

/// The name a new chat's `properties.topic` gives it, when it gives one.
fn chat_topic(call: &Call<'_>) -> Option<String> {
    let body = call.object()?;
    let topic = body.get("properties")?.get("topic")?.as_str()?;
    Some(topic.to_owned())
}

/// Whether `mri` is `8:orgid:` and an object id (a GUID, `8-4-4-4-12` hex digits).
fn is_orgid_mri(mri: &str) -> bool {
    let Some(oid) = mri.strip_prefix("8:orgid:") else {
        return false;
    };
    let groups: Vec<&str> = oid.split('-').collect();
    groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|group| group.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// The 1:1 chat id of two `8:orgid:` members: `19:<oid>_<oid>@unq.gbl.spaces`, lowercase and sorted.
fn one_to_one_id(members: &[String]) -> Option<String> {
    let [first, second] = members else {
        return None;
    };
    let oid = |mri: &str| mri.strip_prefix("8:orgid:").map(str::to_ascii_lowercase);
    let mut oids = [oid(first)?, oid(second)?];
    oids.sort();
    Some(format!("19:{}_{}@unq.gbl.spaces", oids[0], oids[1]))
}
