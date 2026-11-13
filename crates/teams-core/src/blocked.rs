//! The account's own messages a tenant's policy blocked (`docs/protocol/chat.md`, "Messages a policy blocked").
//!
//! The chat service takes a blocked message's text and files out of the message itself (its update comes with empty
//! `content`, no `files` and `properties.policyViolation`) and keeps the sender a copy of them in its own stream
//! `48:generic`, under the id the verdict names. Teams shows its sender the message as written, with the verdict over
//! it, so the copy's text and files are put back into the message as it is stored, whichever comes first: the copy is
//! kept ([`teams_store::BlockedCopy`]) for an update that comes after it, and an update that comes first keeps the text
//! the store had (the account's own message as sent). Everyone else keeps the message without its text.
//!
//! The verdict's update sets `edittime`; Teams doesn't call the message edited for it, so the stored edit time stays
//! what it was.

use serde_json::{Map, Value};
use teams_proto::chat::{self, Field};
use teams_store::{BlockedCopy, Message, Store, StoreError};

/// The properties a blocked message's update comes without or empty, put back from where its text comes from.
const PUT_BACK: [&str; 4] = ["files", "mentions", "links", "qtdMsgs"];

/// The copy a message of [`chat::GENERIC_STREAM`] is, when it is the account's own (`me`) and its arrival can be read.
#[must_use]
pub fn copy_of(message: &chat::Message, me: &str) -> Option<BlockedCopy> {
    if message.message_type.is_control() || message.sender() != Some(me) {
        return None;
    }
    let arrival_time = message.arrival_time_ms().or_else(|| message.id.parse().ok())?;
    Some(BlockedCopy {
        id: message.id.clone(),
        content: message.content.clone(),
        properties: (!message.properties.is_empty()).then(|| Value::Object(message.properties.clone()).to_string()),
        arrival_time,
    })
}

/// `row`, the store's form of `message`, with what a policy's verdict took out of the account's (`me`) own message put
/// back from its copy, else from the stored row, and the stored edit time kept; as it came for any other message.
///
/// # Errors
/// [`StoreError`] when the store can't be read.
pub fn with_text_put_back(
    store: &Store,
    mut row: Message,
    message: &chat::Message,
    me: &str,
) -> Result<Message, StoreError> {
    let Field::Known(verdict) = message.policy_violation() else {
        return Ok(row);
    };
    if verdict.state == chat::PolicyState::None {
        return Ok(row);
    }
    let stored = store.message(&row.conversation_id, &row.id)?;
    row.edit_time = stored.as_ref().and_then(|stored| stored.edit_time);
    let own = row.sender.as_deref() == Some(me);
    if !own || !verdict.blocks() || !row.content.is_empty() || row.delete_time.is_some() {
        return Ok(row);
    }
    let copy = match verdict.generic_stream_item.as_deref() {
        Some(id) => store.blocked_copy(id)?,
        None => None,
    };
    match copy {
        Some(copy) => put_back(&mut row, &copy.content, copy.properties.as_deref()),
        None => {
            if let Some(stored) = stored.filter(|stored| !stored.content.is_empty()) {
                put_back(&mut row, &stored.content, stored.properties.as_deref());
            }
        }
    }
    Ok(row)
}

/// Keeps `copy` and puts its text back into the account's (`me`) stored messages that name it; those it changed.
///
/// # Errors
/// [`StoreError`] when the store can't be read or written.
pub fn keep_copy(store: &mut Store, copy: &BlockedCopy, me: &str) -> Result<Vec<Message>, StoreError> {
    store.keep_blocked_copy(copy)?;
    let mut restored = Vec::new();
    for mut message in store.messages_naming_copy(me, &copy.id)? {
        put_back(&mut message, &copy.content, copy.properties.as_deref());
        if store.restore_blocked(&message)? {
            restored.push(message);
        }
    }
    Ok(restored)
}

/// Puts `content` into `row`, and each of [`PUT_BACK`] that `properties` (JSON text) has and the row lacks or holds
/// empty.
fn put_back(row: &mut Message, content: &str, properties: Option<&str>) {
    content.clone_into(&mut row.content);
    let Some(source) = properties.and_then(|text| serde_json::from_str::<Map<String, Value>>(text).ok()) else {
        return;
    };
    let mut target = row
        .properties
        .as_deref()
        .and_then(|text| serde_json::from_str::<Map<String, Value>>(text).ok())
        .unwrap_or_default();
    for key in PUT_BACK {
        if let Some(value) = source.get(key)
            && target.get(key).is_none_or(is_empty_list)
        {
            target.insert(key.to_owned(), value.clone());
        }
    }
    row.properties = Some(Value::Object(target).to_string());
}

/// An empty list, as a list or as its JSON text.
fn is_empty_list(value: &Value) -> bool {
    match value {
        Value::Array(list) => list.is_empty(),
        Value::String(text) => text.trim() == "[]" || text.is_empty(),
        _ => false,
    }
}
