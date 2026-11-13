//! Synthetic copies of the account's own blocked messages: kept across a reopen, found by the messages that name them
//! (the verdict as an object or as its JSON text), and put back only into the version stored.

#![cfg(test)]

use teams_store::{BlockedCopy, Conversation, ConversationKind, Message, Store};

const CHAT: &str = "19:synthetic-blocked@thread.v2";
const ME: &str = "8:orgid:synthetic-me";
const PEER: &str = "8:orgid:synthetic-peer";

fn open(dir: &tempfile::TempDir) -> Store {
    let mut store = Store::open(&dir.path().join("store.sqlite")).unwrap();
    store
        .upsert_conversation(&Conversation {
            id: CHAT.to_owned(),
            kind: ConversationKind::OneToOne,
            topic: None,
            version: 0,
            last_activity: None,
        })
        .unwrap();
    store
}

fn blocked(id: &str, sender: &str, properties: &str) -> Message {
    Message {
        conversation_id: CHAT.to_owned(),
        id: id.to_owned(),
        version: 2,
        client_message_id: Some(format!("synthetic-client-{id}")),
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: "RichText/Html".to_owned(),
        content: String::new(),
        properties: Some(properties.to_owned()),
        arrival_time: 100,
        compose_time: Some(100),
        edit_time: None,
        delete_time: None,
    }
}

fn copy() -> BlockedCopy {
    BlockedCopy {
        id: "900".to_owned(),
        content: "<p>synthetic text</p>".to_owned(),
        properties: Some(r#"{"files":"[]"}"#.to_owned()),
        arrival_time: 101,
    }
}

#[test]
fn a_copy_is_kept_across_a_reopen_and_replaced_when_sent_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(&dir);
    assert_eq!(store.blocked_copy("900").unwrap(), None);
    store.keep_blocked_copy(&copy()).unwrap();
    drop(store);
    let mut store = open(&dir);
    assert_eq!(store.blocked_copy("900").unwrap(), Some(copy()));
    let again = BlockedCopy {
        content: "<p>synthetic text again</p>".to_owned(),
        ..copy()
    };
    store.keep_blocked_copy(&again).unwrap();
    assert_eq!(store.blocked_copy("900").unwrap(), Some(again));
}

#[test]
fn the_messages_naming_a_copy_are_the_senders_own_and_still_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(&dir);
    let object = r#"{"policyViolation":{"state":1,"genericStreamItemId":900}}"#;
    let text = r#"{"policyViolation":"{\"state\":1,\"genericStreamItemId\":\"900\"}"}"#;
    for message in [
        blocked("1", ME, object),
        blocked("2", ME, text),
        blocked("3", PEER, object),
        blocked("4", ME, r#"{"policyViolation":{"state":1,"genericStreamItemId":901}}"#),
        blocked("5", ME, r#"{"policyViolation":"not json"}"#),
        blocked("6", ME, "not json"),
        Message {
            content: "<p>kept</p>".to_owned(),
            ..blocked("7", ME, object)
        },
        Message {
            delete_time: Some(3),
            ..blocked("8", ME, object)
        },
        blocked("9", ME, r#"{"policyViolation":{"state":0,"genericStreamItemId":900}}"#),
        blocked("10", ME, r#"{"policyViolation":{"state":2,"genericStreamItemId":900}}"#),
        blocked("11", ME, r#"{"policyViolation":{"genericStreamItemId":900}}"#),
    ] {
        store.upsert_message(&message, None).unwrap();
    }
    let ids: Vec<String> = store
        .messages_naming_copy(ME, "900")
        .unwrap()
        .into_iter()
        .map(|message| message.id)
        .collect();
    assert_eq!(ids, ["1", "2", "7"]);
    let ids: Vec<_> = store
        .messages_missing_blocked_copies(ME, None)
        .unwrap()
        .into_iter()
        .map(|message| message.id)
        .collect();
    assert_eq!(ids, ["1", "2", "4", "7"]);
    assert!(
        store
            .messages_missing_blocked_copies(ME, Some("other-chat"))
            .unwrap()
            .is_empty()
    );
    store.keep_blocked_copy(&copy()).unwrap();
    drop(store);
    let store = open(&dir);
    let ids: Vec<_> = store
        .messages_missing_blocked_copies(ME, Some(CHAT))
        .unwrap()
        .into_iter()
        .map(|message| message.id)
        .collect();
    assert_eq!(ids, ["4"]);
}

#[test]
fn text_goes_back_only_into_the_version_stored_and_is_indexed_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(&dir);
    let message = blocked("1", ME, r#"{"policyViolation":{"state":1,"genericStreamItemId":900}}"#);
    store.upsert_message_indexed(&message, None, |_| String::new()).unwrap();
    assert!(store.messages_to_index(None, 10).unwrap().is_empty());
    let restored = Message {
        content: "<p>synthetic text</p>".to_owned(),
        ..message.clone()
    };
    assert!(
        store
            .restore_blocked(&restored, |message| message.content.clone())
            .unwrap()
    );
    assert_eq!(store.message(CHAT, "1").unwrap(), Some(restored.clone()));
    assert!(store.messages_to_index(None, 10).unwrap().is_empty());
    assert_eq!(store.search("synthetic", 10).unwrap()[0].message_id, restored.id);
    // a newer version (or a delete) that came meanwhile stays
    let stale = Message {
        version: 1,
        ..restored.clone()
    };
    assert!(
        !store
            .restore_blocked(&stale, |message| message.content.clone())
            .unwrap()
    );
    assert!(
        !store
            .restore_blocked(&blocked("9", ME, "{}"), |message| message.content.clone())
            .unwrap()
    );
}
