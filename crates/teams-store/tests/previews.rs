//! Synthetic list-page hints: persisted for rows, absent from history, replaced by sync and counted by read horizons.

#![cfg(test)]

use teams_store::{AppReadHorizon, Conversation, ConversationKind, Message, ReadHorizon, Store, SyncState};

const CHAT: &str = "19:synthetic-preview@thread.v2";
const ME: &str = "8:orgid:synthetic-me";
const PEER: &str = "8:orgid:synthetic-peer";

fn open(dir: &tempfile::TempDir) -> Store {
    let mut store = Store::open(&dir.path().join("store.sqlite")).unwrap();
    store
        .upsert_conversation(&Conversation {
            id: CHAT.to_owned(),
            kind: ConversationKind::Group,
            topic: None,
            version: 0,
            last_activity: Some(100),
        })
        .unwrap();
    store
}

fn hint(id: &str, version: i64, at: i64) -> Message {
    Message {
        conversation_id: CHAT.to_owned(),
        id: id.to_owned(),
        version,
        client_message_id: Some("synthetic-preview-client-id".to_owned()),
        sender: Some(PEER.to_owned()),
        sender_name: Some("Synthetic".to_owned()),
        message_type: "RichText/Html".to_owned(),
        content: "<p>Synthetic preview</p>".to_owned(),
        properties: Some("{}".to_owned()),
        arrival_time: at,
        compose_time: Some(at),
        edit_time: None,
        delete_time: None,
    }
}

#[test]
fn a_preview_survives_reopening_without_becoming_history_and_is_forgotten_with_its_chat() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(&dir);
    let message = hint("first", 1, 100);
    assert!(store.upsert_conversation_preview(&message).unwrap());
    drop(store);
    let mut store = open(&dir);
    assert_eq!(store.conversation_preview(CHAT).unwrap(), Some(message));
    assert!(store.messages_before(CHAT, None, 10).unwrap().messages.is_empty());
    assert!(store.message(CHAT, "first").unwrap().is_none());
    assert!(
        store
            .message_by_client_id(CHAT, "synthetic-preview-client-id")
            .unwrap()
            .is_none()
    );
    assert!(store.sync_state(CHAT).unwrap().is_none());
    store.forget_conversation(CHAT).unwrap();
    assert!(store.conversation_preview(CHAT).unwrap().is_none());
}

#[test]
fn previews_take_newer_messages_and_edits_and_strip_deleted_content() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(&dir);
    assert!(store.upsert_conversation_preview(&hint("first", 2, 100)).unwrap());
    assert!(!store.upsert_conversation_preview(&hint("first", 1, 100)).unwrap());
    assert!(!store.upsert_conversation_preview(&hint("older", 99, 90)).unwrap());
    assert!(store.upsert_conversation_preview(&hint("second", 3, 110)).unwrap());
    let mut deleted = hint("second", 4, 110);
    deleted.delete_time = Some(120);
    assert!(store.upsert_conversation_preview(&deleted).unwrap());
    let stored = store.conversation_preview(CHAT).unwrap().unwrap();
    assert!(stored.content.is_empty());
    assert!(stored.properties.is_none());
    assert_eq!(store.unread_count(CHAT, ME).unwrap(), 0);
}

#[test]
fn a_preview_uses_the_same_unread_rules_as_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(&dir);
    store.upsert_conversation_preview(&hint("first", 1, 100)).unwrap();
    assert_eq!(store.unread_count(CHAT, ME).unwrap(), 1);
    store
        .set_read_horizon(&ReadHorizon {
            conversation_id: CHAT.to_owned(),
            user: ME.to_owned(),
            last_read_arrival: 100,
            updated: 101,
            raw: None,
        })
        .unwrap();
    assert_eq!(store.unread_count(CHAT, ME).unwrap(), 0);
    store.upsert_conversation_preview(&hint("second", 2, 110)).unwrap();
    assert_eq!(store.unread_count(CHAT, ME).unwrap(), 1);
    store
        .set_app_read_horizon(AppReadHorizon { chat: 110, channel: 0 })
        .unwrap();
    assert_eq!(store.unread_count(CHAT, ME).unwrap(), 0);
    let mut own = hint("own", 3, 120);
    own.sender = Some(ME.to_owned());
    store.upsert_conversation_preview(&own).unwrap();
    assert_eq!(store.unread_count(CHAT, ME).unwrap(), 0);
    let mut unknown = hint("unknown", 4, 130);
    unknown.sender = None;
    store.upsert_conversation_preview(&unknown).unwrap();
    assert_eq!(store.unread_count(CHAT, ME).unwrap(), 0);
}

#[test]
fn synced_history_replaces_a_preview_even_when_empty_and_never_counts_it_twice() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(&dir);
    let message = hint("first", 1, 100);
    store.upsert_conversation_preview(&message).unwrap();
    store.upsert_message(&message, None).unwrap();
    assert!(store.conversation_preview(CHAT).unwrap().is_none());
    assert_eq!(store.unread_count(CHAT, ME).unwrap(), 1);
    store.forget_conversation(CHAT).unwrap();
    drop(store);
    let mut store = open(&dir);
    store.upsert_conversation_preview(&message).unwrap();
    store
        .set_sync_state(&SyncState {
            conversation_id: CHAT.to_owned(),
            sync_link: None,
            backward_link: None,
            backward_oldest: None,
            sync_until: None,
            gap_link: None,
            gap_until: None,
            history_complete: true,
            updated: 200,
        })
        .unwrap();
    assert!(store.conversation_preview(CHAT).unwrap().is_none());
    assert_eq!(store.unread_count(CHAT, ME).unwrap(), 0);
}

#[test]
fn schema_23_previews_survive_the_activity_bookmark_migration() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(&dir);
    let preview = hint("before-activity", 3, 100);
    store.upsert_conversation_preview(&preview).unwrap();
    drop(store);

    // Main's schema 23 already has list-page previews, but no Activity bookmarks.
    let raw = rusqlite::Connection::open(dir.path().join("store.sqlite")).unwrap();
    raw.execute_batch("DROP TABLE horizon_bookmarks; PRAGMA user_version = 23;")
        .unwrap();
    drop(raw);

    let mut store = Store::open(&dir.path().join("store.sqlite")).unwrap();
    assert_eq!(store.schema_version().unwrap(), teams_store::SCHEMA_VERSION);
    assert_eq!(store.conversation_preview(CHAT).unwrap(), Some(preview.clone()));
    assert_eq!(store.horizon_bookmark(CHAT).unwrap(), None);
    store.set_horizon_bookmark(CHAT, 200).unwrap();
    drop(store);

    let store = Store::open(&dir.path().join("store.sqlite")).unwrap();
    assert_eq!(store.horizon_bookmark(CHAT).unwrap(), Some(200));
    assert_eq!(store.conversation_preview(CHAT).unwrap(), Some(preview));
    assert!(store.messages_before(CHAT, None, 10).unwrap().messages.is_empty());
}
