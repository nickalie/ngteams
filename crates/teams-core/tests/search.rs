//! Local search's core side on a real store (M2 Task 15): the text a message is found by, and the backfill of the
//! messages stored before the index, in bounded batches, going on after a stop. The engine's part (each message
//! indexed as it is stored, edits and deletes, the backfill from its start, the query) is in `tests/engine.rs`.
//! Every id, MRI and text here is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::path::PathBuf;

use teams_core::search::{Backfill, Step, search_text};
use teams_store::{Conversation, ConversationKind, Message, Store, StoreError};

const PEER: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const CHAT: &str = "19:synthetic-ab@unq.gbl.spaces";
const GROUP: &str = "19:synthetic-abc@thread.v2";

fn message(conversation: &str, id: &str, message_type: &str, content: &str) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(PEER.to_owned()),
        sender_name: None,
        message_type: message_type.to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: id.parse().unwrap_or_default(),
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

#[test]
fn a_mention_of_everyone_is_found_by_the_reader_s_label() {
    let mut rich = message(
        GROUP,
        "1",
        "RichText/Html",
        r#"<p><span itemtype="http://schema.skype.com/Mention" itemid="0">Все</span></p><p>a made-up build</p>"#,
    );
    rich.properties =
        Some(r#"{"mentions":[{"itemid":"0","mri":"19:synthetic-abc@thread.v2","mentionType":"everyone"}]}"#.to_owned());
    let text = search_text(&rich);
    assert!(
        text.contains("Everyone") && text.contains("a made-up build"),
        "{text:?}"
    );
    assert!(!text.contains("Все"), "{text:?}");
}

#[test]
fn a_message_is_found_by_the_text_its_reader_sees() {
    let rich = message(
        CHAT,
        "1",
        "RichText/Html",
        r#"<p>Hello <b>there</b>, <span itemtype="http://schema.skype.com/Mention" itemid="0">Lab Bee</span></p><p>see <a href="https://example.invalid/x">the plan</a> &amp; <img src="https://example.invalid/i.png" alt="a chart"></p><script>hidden()</script>"#,
    );
    let text = search_text(&rich);
    for shown in ["Hello", "there", "Lab Bee", "the plan", "&", "a chart"] {
        assert!(text.contains(shown), "{shown:?} in {text:?}");
    }
    for hidden in ["<b>", "itemtype", "example.invalid", "hidden()", "&amp;"] {
        assert!(!text.contains(hidden), "{hidden:?} in {text:?}");
    }
    // a plain text message's `<` and `&` are its own
    assert_eq!(search_text(&message(CHAT, "2", "Text", "a <b> & c")), "a <b> & c");
    // nothing for a deleted message, nor one that isn't text
    let mut deleted = message(CHAT, "3", "RichText/Html", "<p>gone</p>");
    deleted.delete_time = Some(3);
    assert_eq!(search_text(&deleted), "");
    let event = message(
        CHAT,
        "4",
        "ThreadActivity/AddMember",
        "<addmember><target>x</target></addmember>",
    );
    assert_eq!(search_text(&event), "");
    assert_eq!(search_text(&message(CHAT, "5", "Control/Typing", "")), "");
    // a call's media message is a record, ids in it: never indexed as text
    for kind in [
        "RichText/Media_CallRecording",
        "RichText/Media_CallTranscript",
        "RichText/Media_CallLogRecording",
    ] {
        let record = message(
            CHAT,
            "6",
            kind,
            r#"<URIObject><Title>Weekly</Title><a href="">Play</a></URIObject>"#,
        );
        assert_eq!(search_text(&record), "", "{kind}");
    }
}

fn store_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("store").join("a.sqlite")
}

/// A store with `count` text messages in the 1:1 chat and one in the group chat, none of them indexed (stored as before
/// the index existed).
fn unindexed(dir: &tempfile::TempDir, count: u32) -> Result<Store, StoreError> {
    let mut store = Store::open(&store_path(dir))?;
    for (id, kind) in [(CHAT, ConversationKind::OneToOne), (GROUP, ConversationKind::Group)] {
        store.upsert_conversation(&Conversation {
            id: id.to_owned(),
            kind,
            topic: None,
            version: 1,
            last_activity: None,
        })?;
    }
    for at in 0..count {
        let id = (1_000 + at).to_string();
        store.upsert_message(
            &message(CHAT, &id, "RichText/Html", &format!("<p>old note {at}</p>")),
            None,
        )?;
    }
    store.upsert_message(&message(GROUP, "5000", "Text", "old note in the group"), None)?;
    Ok(store)
}

fn found(store: &Store, query: &str) -> usize {
    store.search(query, 100).expect("search").len()
}

#[test]
fn the_backfill_indexes_a_bounded_batch_per_step_until_nothing_is_left() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = unindexed(&dir, 4)?;
    assert_eq!(found(&store, "note"), 0);
    let mut backfill = Backfill::new(2);
    assert_eq!(backfill.step(&mut store)?, Step::Indexed(2));
    assert_eq!(found(&store, "note"), 2, "a batch at a time");
    assert!(!backfill.done());
    assert_eq!(backfill.step(&mut store)?, Step::Indexed(2));
    assert_eq!(
        backfill.step(&mut store)?,
        Step::Done(1),
        "a batch that comes short is the last"
    );
    assert!(backfill.done());
    assert_eq!(backfill.step(&mut store)?, Step::Done(0));
    assert_eq!(backfill.indexed(), 5);
    assert_eq!(found(&store, "note"), 5);
    assert_eq!(found(&store, "group"), 1);
    // a later backfill finds nothing to do
    let mut again = Backfill::new(2);
    assert_eq!(again.step(&mut store)?, Step::Done(0));
    assert_eq!(again.indexed(), 0);
    Ok(())
}

#[test]
fn a_backfill_that_stopped_goes_on_at_the_next_start_without_indexing_anything_twice() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = unindexed(&dir, 5)?;
    let mut first = Backfill::new(3);
    assert_eq!(first.step(&mut store)?, Step::Indexed(3));
    // the engine stops: its store and backfill are gone
    drop(first);
    drop(store);

    let mut store = Store::open(&store_path(&dir))?;
    let mut next = Backfill::new(3);
    assert_eq!(
        next.step(&mut store)?,
        Step::Indexed(3),
        "the three left: two of the chat and the group's"
    );
    // a full batch may have more after it: the next step finds none
    assert_eq!(next.step(&mut store)?, Step::Done(0));
    assert!(next.done());
    assert_eq!(next.indexed(), 3);
    assert_eq!(found(&store, "note"), 6);
    Ok(())
}

#[test]
fn a_batch_of_messages_without_text_is_indexed_as_having_none() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = unindexed(&dir, 0)?;
    store.upsert_message(
        &message(CHAT, "2000", "ThreadActivity/TopicUpdate", "<x>topic</x>"),
        None,
    )?;
    let mut backfill = Backfill::new(10);
    assert_eq!(backfill.step(&mut store)?, Step::Done(2));
    assert!(backfill.done());
    assert_eq!(found(&store, "topic"), 0, "an event's markup isn't text");
    // and is never listed again
    assert!(store.messages_to_index(None, 10)?.is_empty());
    Ok(())
}
