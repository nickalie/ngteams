//! Reads: the conversation list and its sync link, message pages (first page, sync, backward links) and the page
//! around a time, the thread read, the member list and the filtered view. Each answer has its fixture's shape with
//! the mock's state filled in.

use serde_json::{Value, json};
use wiremock::ResponseTemplate;

use crate::chatsvc::{Call, json, status};
use crate::fixtures::object;
use crate::world::{Conversation, Kind, State};
use crate::{CHAT_BASE, REGION, TENANT};

/// The mock's sync token: what changed after a time.
const SINCE: &str = "mock-since-";
/// The mock's backward-page token: the messages before a `sequenceId`, then `-` and its first page's sync time.
const BEFORE: &str = "mock-before-";
/// The mock's conversation list page token: the conversations changed after a time, from an offset.
const LIST_PAGE: &str = "mock-page-";
/// The largest page a read asks for and gets (the web client asks for 200).
const MAX_PAGE: usize = 200;

/// `GET v1/users/<owner>/conversations`: the caller's conversations, or with the sync link's `syncState` those
/// changed since it was handed out; one page (no `forwardLink` was ever seen), unless a test asked for pages
/// (`startup.rs`): then newest first, each page but the last with a `forwardLink` to the next.
pub(crate) fn conversations(state: &State, call: &Call<'_>, owner: &str) -> ResponseTemplate {
    if owner != "ME" && owner != call.caller {
        return status(403);
    }
    let (since, offset) = match call.query("syncState") {
        None => (0, 0),
        Some(token) => match (token_value(token, SINCE), list_page_token(token)) {
            (Some(since), _) => (since, 0),
            (None, Some(page)) => page,
            (None, None) => return status(400),
        },
    };
    let mut listed: Vec<&Conversation> = state
        .conversations
        .iter()
        .filter(|conversation| {
            conversation.listed && conversation.is_member(&call.caller) && conversation.version > since
        })
        .collect();
    if state.list_page.is_some() {
        listed.sort_by_key(|conversation| std::cmp::Reverse(conversation.version));
    }
    let end = state
        .list_page
        .map_or(listed.len(), |size| offset.saturating_add(size).min(listed.len()));
    let entries: Vec<Value> = listed
        .get(offset.min(end)..end)
        .unwrap_or_default()
        .iter()
        .map(|conversation| list_entry(state, conversation, &call.caller))
        .collect();
    let sync = format!(
        "{CHAT_BASE}/v1/users/{}/conversations?view=msnp24Equivalent&syncState={SINCE}{}&pageSize=200",
        call.caller,
        state.now()
    );
    let forward = match state.list_page {
        Some(size) if end < listed.len() => format!(
            "{CHAT_BASE}/v1/users/{}/conversations?view=msnp24Equivalent&syncState={LIST_PAGE}{since}-{end}&pageSize={size}",
            call.caller
        ),
        _ => String::new(),
    };
    json(
        200,
        &json!({
            "conversations": entries,
            "_metadata": {"totalCount": listed.len(), "forwardLink": forward, "syncState": sync},
        }),
    )
}

/// A list page token's values, `<LIST_PAGE><since>-<offset>`.
fn list_page_token(token: &str) -> Option<(u64, usize)> {
    let (since, offset) = token.strip_prefix(LIST_PAGE)?.split_once('-')?;
    Some((since.parse().ok()?, offset.parse().ok()?))
}

/// `GET …/conversations/<chat>/messages`: newest first, a page of `pageSize`; with the mock's `syncState` the messages
/// changed since it was handed out (all of them), or the page before a backward link's message. Every answer carries
/// a sync link, and a `backwardLink` while older messages remain. A backward page's sync link is the one its first
/// page handed out (the backward token carries its time), so a client keeping the latest sync link it got while paging
/// back still gets what changed after the first page.
pub(crate) fn messages(state: &State, call: &Call<'_>, chat: &str) -> ResponseTemplate {
    let conversation = match member_of(state, chat, &call.caller) {
        Ok(conversation) => conversation,
        Err(code) => return status(code),
    };
    let size = page_size(call);
    let Some((page, older, watermark)) = message_page(state, conversation, call.query("syncState"), size) else {
        return status(400);
    };
    let first = conversation
        .messages
        .first()
        .map_or(1, |message| number(&message["id"]));
    let last_version = conversation
        .messages
        .iter()
        .map(|message| number(&message["version"]))
        .max()
        .unwrap_or(0);
    let mut metadata = json!({
        "lastCompleteSegmentStartTime": 1,
        "lastCompleteSegmentEndTime": last_version,
        "syncState": format!(
            "{CHAT_BASE}/v1/users/ME/conversations/{chat}/messages?startTime={first}&syncState={SINCE}{watermark}&pageSize=200&view=msnp24Equivalent"
        ),
    });
    if let (true, Some(oldest)) = (older, page.last().and_then(|message| message["sequenceId"].as_u64())) {
        object(&mut metadata).insert(
            "backwardLink".to_owned(),
            json!(format!(
                "{CHAT_BASE}/v1/users/ME/conversations/{chat}/messages?syncState={BEFORE}{oldest}-{watermark}&pageSize={size}&view=msnp24Equivalent"
            )),
        );
    }
    json(
        200,
        &json!({"messages": page, "tenantId": TENANT, "_metadata": metadata}),
    )
}

/// The messages a read with `token` (its `syncState`) gets, newest first and at most `size` of them (all of them for a
/// sync), whether older ones remain, and the time its sync link starts from: now, or a backward token's first page's
/// time. `None` for a token the mock didn't hand out.
fn message_page(
    state: &State,
    conversation: &Conversation,
    token: Option<&str>,
    size: usize,
) -> Option<(Vec<Value>, bool, u64)> {
    let newest_first = conversation.messages.iter().rev();
    let Some(token) = token else {
        let (page, older) = page_of(newest_first, size);
        return Some((page, older, state.now()));
    };
    if let Some(since) = token_value(token, SINCE) {
        let changed = newest_first.filter(|message| number(&message["version"]) > since);
        let (page, older) = page_of(changed, usize::MAX);
        return Some((page, older, state.now()));
    }
    let (before, watermark) = backward_token(token)?;
    let earlier = newest_first.filter(|message| message["sequenceId"].as_u64().is_some_and(|seq| seq < before));
    let (page, older) = page_of(earlier, size);
    Some((page, older, watermark))
}

/// `GET …/messages/epochTimeStamp/<ms>`: as the web client read it around the time the reader joined, a page newest
/// first with no link; the time isn't looked at (the lab's chats fit in one page).
pub(crate) fn around(state: &State, call: &Call<'_>, chat: &str) -> ResponseTemplate {
    let conversation = match member_of(state, chat, &call.caller) {
        Ok(conversation) => conversation,
        Err(code) => return status(code),
    };
    let (page, _) = page_of(conversation.messages.iter().rev(), page_size(call));
    json(
        200,
        &json!({
            "messages": page,
            "tenantId": TENANT,
            "_metadata": {"lastCompleteSegmentStartTime": 0, "lastCompleteSegmentEndTime": 0},
        }),
    )
}

/// `GET v1/threads/<id>`: the thread read of `thread-group.json`, with this chat's members and times.
pub(crate) fn thread(state: &State, call: &Call<'_>, id: &str) -> ResponseTemplate {
    match member_of(state, id, &call.caller) {
        Ok(conversation) => json(200, &thread_body(state, conversation)),
        Err(code) => status(code),
    }
}

/// `GET v1/threads/<id>/members`: the member list of `thread-group-members.json`, every member, counted, unpaged.
pub(crate) fn members(state: &State, call: &Call<'_>, id: &str) -> ResponseTemplate {
    let conversation = match member_of(state, id, &call.caller) {
        Ok(conversation) => conversation,
        Err(code) => return status(code),
    };
    let mut body = state.fixtures.members.clone();
    let template = body["members"][0].clone();
    let members: Vec<Value> = conversation
        .members
        .iter()
        .map(|mri| member_entry(&template, mri, None))
        .collect();
    let fields = object(&mut body);
    fields.insert("totalMemberCount".to_owned(), json!(members.len()));
    fields.insert("members".to_owned(), Value::Array(members));
    fields.insert("eTag".to_owned(), json!(format!("\"{}\"", conversation.created)));
    json(200, &body)
}

/// `GET v1/threads/<id>/filtered`: refused for a member, as `thread-group-filtered.json` recorded it.
pub(crate) fn filtered(state: &State, call: &Call<'_>, id: &str) -> ResponseTemplate {
    match member_of(state, id, &call.caller) {
        Ok(_) => state.fixtures.filtered.template(),
        Err(code) => status(code),
    }
}

/// `GET v1/users/ME/conversations/<chat>?view=msnp24Equivalent`: the caller's own view of a conversation, here its id
/// and the own properties the caller set (`docs/protocol/channels.md`).
pub(crate) fn own_conversation(state: &State, call: &Call<'_>, chat: &str) -> ResponseTemplate {
    if let Err(code) = member_of(state, chat, &call.caller) {
        return status(code);
    }
    let properties = state
        .own_properties
        .get(&(call.caller.clone(), chat.to_owned()))
        .cloned()
        .unwrap_or_default();
    json(200, &json!({"id": chat, "properties": properties}))
}

/// The conversation `id` if `caller` is a member; otherwise the status of the refusal: 404 when there is none, 403
/// when they aren't one.
pub(crate) fn member_of<'s>(state: &'s State, id: &str, caller: &str) -> Result<&'s Conversation, u16> {
    let conversation = state.conversation(id).ok_or(404_u16)?;
    if conversation.is_member(caller) {
        Ok(conversation)
    } else {
        Err(403)
    }
}

/// The thread read of `conversation` (`thread-group.json`'s shape; a 1:1 chat's wasn't kept, so it takes the same, with
/// the `uniquerosterthread` its conversation list entry gives it: the live 1:1 read carried that key, the value is the
/// list's).
pub(crate) fn thread_body(state: &State, conversation: &Conversation) -> Value {
    let mut thread = state.fixtures.thread.clone();
    let template = thread["members"][0].clone();
    let members: Vec<Value> = conversation
        .members
        .iter()
        .map(|mri| member_entry(&template, mri, Some(conversation.created)))
        .collect();
    let id = &conversation.id;
    let fields = object(&mut thread);
    fields.insert("id".to_owned(), json!(id));
    fields.insert("members".to_owned(), Value::Array(members));
    fields.insert(
        "messages".to_owned(),
        json!(format!(
            "https://{REGION}.example.test/v1/users/ME/conversations/{id}/messages"
        )),
    );
    fields.insert("eTag".to_owned(), json!(format!("\"{}\"", conversation.created)));
    fields.insert("rosterVersion".to_owned(), json!(conversation.roster));
    fields.insert("version".to_owned(), json!(conversation.created));
    let properties = object(fields.entry("properties").or_insert(Value::Null));
    properties.insert("createdat".to_owned(), json!(conversation.created.to_string()));
    properties.insert("creator".to_owned(), json!(conversation.creator));
    let unique = &conversation.entry["threadProperties"]["uniquerosterthread"];
    if conversation.kind == Kind::OneToOne && !unique.is_null() {
        properties.insert("uniquerosterthread".to_owned(), unique.clone());
    }
    // a channel's thread is of its own type (`space` for a team's general channel, `topic` for another, as M2 Task
    // 16's probe read them), not a chat's
    // a named chat's name, as the thread read carries it (`properties.topic`)
    let topic = &conversation.entry["threadProperties"]["topic"];
    if conversation.kind == Kind::Group && topic.is_string() {
        properties.insert("topic".to_owned(), topic.clone());
    }
    // a meeting's chat names its meeting (`properties.meeting`, [`crate::ChatServiceMock::add_meeting_chat`])
    let meeting = &conversation.entry["threadProperties"]["meeting"];
    if meeting.is_string() {
        properties.insert("meeting".to_owned(), meeting.clone());
    }
    let thread_type = &conversation.entry["threadProperties"]["threadType"];
    if conversation.kind == Kind::ReadOnly && thread_type.is_string() {
        properties.insert("threadType".to_owned(), thread_type.clone());
        // what Manage team's settings read of a team's thread
        properties.insert("spaceAdminSettings".to_owned(), crate::team_pages::settings());
    }
    thread
}

/// One member entry, as `template` (a fixture's) with `mri`'s id and link, and `range_start` if the template has one.
fn member_entry(template: &Value, mri: &str, range_start: Option<u64>) -> Value {
    let mut member = template.clone();
    let fields = object(&mut member);
    fields.insert("id".to_owned(), json!(mri));
    fields.insert(
        "userLink".to_owned(),
        json!(format!("https://{REGION}.example.test/v1/users/{mri}")),
    );
    if let (Some(start), true) = (range_start, fields.contains_key("rangeStart")) {
        fields.insert("rangeStart".to_owned(), json!(start));
    }
    member
}

/// `conversation`'s entry in `caller`'s conversation list: its fixture entry with what changes filled in.
fn list_entry(state: &State, conversation: &Conversation, caller: &str) -> Value {
    let mut entry = conversation.entry.clone();
    let last = conversation.messages.last();
    let id = &conversation.id;
    let fields = object(&mut entry);
    fields.insert("id".to_owned(), json!(id));
    fields.insert("version".to_owned(), json!(conversation.version));
    fields.insert("targetLink".to_owned(), json!(format!("{CHAT_BASE}/v1/threads/{id}")));
    fields.insert(
        "messages".to_owned(),
        json!(format!("{CHAT_BASE}/v1/users/ME/conversations/{id}/messages")),
    );
    fields.insert(
        "lastUpdatedMessageId".to_owned(),
        json!(last.map_or(0, |message| number(&message["id"]))),
    );
    fields.insert(
        "lastUpdatedMessageVersion".to_owned(),
        json!(last.map_or(0, |message| number(&message["version"]))),
    );
    fields.insert(
        "lastMessage".to_owned(),
        last.map_or_else(|| json!({}), without_properties),
    );
    let properties = object(fields.entry("properties").or_insert(Value::Null));
    if let Some(own) = state.own_properties.get(&(caller.to_owned(), id.to_owned())) {
        properties.extend(own.clone());
    }
    match conversation.horizons.get(caller) {
        Some(horizon) => properties.insert("consumptionhorizon".to_owned(), json!(horizon)),
        None => properties.remove("consumptionhorizon"),
    };
    let empty = if conversation.messages.is_empty() {
        "True"
    } else {
        "False"
    };
    properties.insert("isemptyconversation".to_owned(), json!(empty));
    let thread = object(fields.entry("threadProperties").or_insert(Value::Null));
    match last.and_then(|message| message["sequenceId"].as_u64()) {
        Some(sequence) => thread.insert("lastSequenceId".to_owned(), json!(sequence.to_string())),
        None => thread.remove("lastSequenceId"),
    };
    if conversation.kind != Kind::ReadOnly {
        thread.insert("isCreator".to_owned(), json!(conversation.creator == caller));
    }
    entry
}

/// A message as the conversation list's `lastMessage` gives it: without `properties`.
fn without_properties(message: &Value) -> Value {
    let mut message = message.clone();
    object(&mut message).remove("properties");
    message
}

/// The first `size` of `messages`, and whether more remain.
fn page_of<'a>(messages: impl Iterator<Item = &'a Value>, size: usize) -> (Vec<Value>, bool) {
    let mut page: Vec<Value> = messages.cloned().collect();
    let more = page.len() > size;
    page.truncate(size);
    (page, more)
}

/// The read's `pageSize`, at most [`MAX_PAGE`] (and that without one).
fn page_size(call: &Call<'_>) -> usize {
    call.query("pageSize")
        .and_then(|size| size.parse().ok())
        .map_or(MAX_PAGE, |size: usize| size.clamp(1, MAX_PAGE))
}

/// A mock token's value: `<prefix><number>`.
fn token_value(token: &str, prefix: &str) -> Option<u64> {
    token.strip_prefix(prefix)?.parse().ok()
}

/// A backward token's values, `<BEFORE><sequenceId>-<time>`: the messages before that `sequenceId`, and the time the
/// first page's sync link started from.
fn backward_token(token: &str) -> Option<(u64, u64)> {
    let (before, watermark) = token.strip_prefix(BEFORE)?.split_once('-')?;
    Some((before.parse().ok()?, watermark.parse().ok()?))
}

/// A number the service writes as a string (a message's `id` and `version`), or 0.
pub(crate) fn number(value: &Value) -> u64 {
    value
        .as_str()
        .and_then(|text| text.parse().ok())
        .or_else(|| value.as_u64())
        .unwrap_or(0)
}
