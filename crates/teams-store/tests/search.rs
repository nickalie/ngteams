//! Local search on a real store file (M2 Task 15): the migration creates the index and fills nothing; a message stored
//! with its text is indexed in the same write (an edit replaces it, a delete removes it); the backfill's batches; the
//! query across chats, the newest first, the user's input quoted so FTS5's syntax is only text. Every id, MRI and text
//! here is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::path::PathBuf;

use teams_store::search::{ELLIPSIS, MATCH_END, MATCH_START, match_expression};
use teams_store::{
    Applied, Conversation, ConversationKind, IndexCursor, IndexText, Message, SCHEMA_VERSION, Store, StoreError,
};

const PEER: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const CHAT: &str = "19:synthetic-ab@unq.gbl.spaces";
const GROUP: &str = "19:synthetic-abc@thread.v2";

fn store_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("store").join("a.sqlite")
}

fn conversation(id: &str, kind: ConversationKind) -> Conversation {
    Conversation {
        id: id.to_owned(),
        kind,
        topic: None,
        version: 1,
        last_activity: None,
    }
}

/// A new store holding the 1:1 chat and the group chat.
fn with_chats(dir: &tempfile::TempDir) -> Result<Store, StoreError> {
    let mut store = Store::open(&store_path(dir))?;
    store.upsert_conversation(&conversation(CHAT, ConversationKind::OneToOne))?;
    store.upsert_conversation(&conversation(GROUP, ConversationKind::Group))?;
    Ok(store)
}

/// A message whose content is its own plain text (the tests' stand-in for the core's reading of it).
fn message(conversation: &str, id: &str, version: i64, text: &str, arrival: i64) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: id.to_owned(),
        version,
        client_message_id: None,
        sender: Some(PEER.to_owned()),
        sender_name: Some("Synthetic Peer".to_owned()),
        message_type: "Text".to_owned(),
        content: text.to_owned(),
        properties: None,
        arrival_time: arrival,
        compose_time: Some(arrival),
        edit_time: None,
        delete_time: None,
    }
}

/// The text the tests index: the content, none once deleted.
fn text_of(message: &Message) -> String {
    if message.delete_time.is_some() {
        String::new()
    } else {
        message.content.clone()
    }
}

fn store_indexed(store: &mut Store, message: &Message) -> Result<Applied, StoreError> {
    store.upsert_message_indexed(message, None, text_of)
}

/// The hits' message ids, in order.
fn found(store: &Store, query: &str) -> Vec<String> {
    store
        .search(query, 50)
        .expect("search")
        .into_iter()
        .map(|hit| hit.message_id)
        .collect()
}

#[test]
fn the_migration_creates_the_index_and_fills_nothing() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chats(&dir)?;
    // a message stored without its text, as every message was before the index existed
    store.upsert_message(&message(CHAT, "1001", 1, "lunch at noon", 1_000), None)?;
    drop(store);
    // the store as schema 7 left it: no index, the message there
    let raw = rusqlite::Connection::open(store_path(&dir)).expect("open directly");
    raw.execute_batch(
        "DROP TABLE unsynced_conversations; ALTER TABLE conversation_sync DROP COLUMN sync_until;
         ALTER TABLE conversation_sync DROP COLUMN gap_until;
         DROP TABLE message_search; DROP TABLE search_entries; DROP TABLE reply_roots; DROP TABLE team_channels;
         DROP TABLE teams; DROP TABLE teams_read; DROP TABLE favorites; DROP TABLE favorites_read;
         ALTER TABLE conversations DROP COLUMN empty; DROP TABLE app_read_horizon; DROP TABLE calendar_events; DROP TABLE forgotten_conversations; DROP TABLE message_translations;
         DROP TABLE sent_originals; DROP TABLE call_transcript_lines; DROP TABLE call_transcripts; DROP TABLE translation_glossary; DROP TABLE horizon_bookmarks; DROP VIEW pending_conversation_previews; DROP TABLE conversation_previews; PRAGMA user_version = 7;",
    )
    .expect("back to schema 7");
    drop(raw);

    let store = Store::open(&store_path(&dir))?;
    assert_eq!(store.schema_version()?, SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 24);
    let raw = rusqlite::Connection::open(store_path(&dir)).expect("open directly");
    let tables: Vec<String> = raw
        .prepare("SELECT name FROM sqlite_master WHERE name IN ('message_search', 'search_entries') ORDER BY name")
        .and_then(|mut statement| {
            statement
                .query_map([], |row| row.get(0))?
                .collect::<Result<Vec<String>, _>>()
        })
        .expect("tables");
    assert_eq!(tables, ["message_search", "search_entries"]);
    let rows: i64 = raw
        .query_row(
            "SELECT (SELECT COUNT(*) FROM search_entries) + (SELECT COUNT(*) FROM message_search)",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(rows, 0, "the migration fills nothing");
    assert!(found(&store, "lunch").is_empty());
    let pending = store.messages_to_index(None, 10)?;
    assert_eq!(
        pending.iter().map(|message| message.id.as_str()).collect::<Vec<_>>(),
        ["1001"]
    );
    Ok(())
}

#[test]
fn a_call_s_media_message_an_earlier_build_indexed_is_never_a_hit() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chats(&dir)?;
    // indexed with its text, as a build before this rule did
    let mut record = message(CHAT, "1001", 1, "Weekly sync Play transcriptid", 1_000);
    record.message_type = "RichText/Media_CallRecording".to_owned();
    store_indexed(&mut store, &record)?;
    store_indexed(&mut store, &message(CHAT, "1002", 1, "Weekly sync notes", 2_000))?;
    assert_eq!(found(&store, "weekly"), ["1002"]);
    assert!(found(&store, "transcriptid").is_empty());
    Ok(())
}

#[test]
fn a_message_stored_with_its_text_is_found_an_edit_replaces_it_and_a_delete_removes_it() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chats(&dir)?;
    assert_eq!(
        store_indexed(&mut store, &message(CHAT, "1001", 1, "Lunch at the canteen?", 1_000))?,
        Applied::Inserted
    );
    assert_eq!(found(&store, "canteen"), ["1001"]);
    assert!(
        store.messages_to_index(None, 10)?.is_empty(),
        "indexed as it was stored"
    );

    // an older version coming late changes nothing
    assert_eq!(
        store_indexed(&mut store, &message(CHAT, "1001", 0, "something else", 1_000))?,
        Applied::Ignored
    );
    assert_eq!(found(&store, "canteen"), ["1001"]);
    assert!(found(&store, "something").is_empty());

    // an edit is a newer version: its text replaces the old one
    assert_eq!(
        store_indexed(&mut store, &message(CHAT, "1001", 2, "Lunch at the park?", 1_000))?,
        Applied::Updated
    );
    assert!(found(&store, "canteen").is_empty(), "the old text is gone");
    assert_eq!(found(&store, "park"), ["1001"]);

    // a delete removes it
    let mut deleted = message(CHAT, "1001", 3, "", 1_000);
    deleted.delete_time = Some(2_000);
    store_indexed(&mut store, &deleted)?;
    assert!(found(&store, "park").is_empty());
    assert!(found(&store, "lunch").is_empty());
    assert!(
        store.messages_to_index(None, 10)?.is_empty(),
        "a deleted message is indexed as having no text"
    );
    Ok(())
}

#[test]
fn a_query_finds_across_chats_the_newest_first_with_prefixes_and_every_term() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chats(&dir)?;
    store_indexed(
        &mut store,
        &message(CHAT, "1001", 1, "The budget review is on Monday", 1_000),
    )?;
    store_indexed(
        &mut store,
        &message(GROUP, "2001", 1, "Budgeting tool works now", 3_000),
    )?;
    store_indexed(
        &mut store,
        &message(CHAT, "1002", 1, "Résumé attached for the review", 2_000),
    )?;
    store_indexed(&mut store, &message(GROUP, "2002", 1, "Nothing to see here", 4_000))?;

    assert_eq!(
        found(&store, "budget"),
        ["2001", "1001"],
        "the newest first, a prefix matches"
    );
    assert_eq!(
        found(&store, "BUDGET review"),
        ["1001"],
        "any case; every term in the message"
    );
    assert_eq!(found(&store, "review"), ["1002", "1001"]);
    assert_eq!(found(&store, "resume"), ["1002"], "diacritics left out");
    assert!(found(&store, "budgetary").is_empty());
    assert!(found(&store, "").is_empty());
    assert!(found(&store, "   ").is_empty());

    let hits = store.search("budget", 1)?;
    assert_eq!(hits.len(), 1, "at most the limit");
    let hit = &hits[0];
    assert_eq!(hit.conversation_id, GROUP);
    assert_eq!(hit.arrival_time, 3_000);
    assert_eq!(hit.sender.as_deref(), Some(PEER));
    assert_eq!(hit.sender_name.as_deref(), Some("Synthetic Peer"));
    assert_eq!(hit.snippet, format!("{MATCH_START}Budgeting{MATCH_END} tool works now"));
    assert!(!format!("{hit:?}").contains("tool"), "Debug leaves the text out");
    Ok(())
}

#[test]
fn a_long_text_s_snippet_is_the_text_around_the_match() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chats(&dir)?;
    let words: Vec<String> = (0..60).map(|at| format!("word{at}")).collect();
    let mut text = words.join(" ");
    text.push_str(" needle and more words after it");
    store_indexed(&mut store, &message(CHAT, "1001", 1, &text, 1_000))?;
    let hits = store.search("needle", 10)?;
    let snippet = &hits[0].snippet;
    assert!(snippet.starts_with(ELLIPSIS), "{snippet}");
    assert!(
        snippet.contains(&format!("{MATCH_START}needle{MATCH_END}")),
        "{snippet}"
    );
    assert!(!snippet.contains("word0 "), "{snippet}");
    Ok(())
}

#[test]
fn the_user_s_input_is_quoted_so_fts_syntax_is_only_text_and_never_fails() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chats(&dir)?;
    let texts = [
        ("1001", r#"she said "ship it" today"#),
        ("1002", "glob a*b star"),
        ("1003", "the follow-up item"),
        ("1004", "time 10:30 sharp"),
        ("1005", "NEAR the station"),
        ("1006", "call (maybe) later"),
        ("1007", "this AND that OR not NOT other"),
        ("1008", "plain filler text"),
    ];
    for (at, (id, text)) in (1_000..).zip(texts) {
        store_indexed(&mut store, &message(CHAT, id, 1, text, at))?;
    }
    assert_eq!(found(&store, r#""ship it""#), ["1001"]);
    assert_eq!(found(&store, r#"said "ship"#), ["1001"], "an unbalanced quote");
    assert_eq!(found(&store, "a*b"), ["1002"]);
    assert_eq!(found(&store, "follow-up"), ["1003"]);
    assert_eq!(found(&store, "-up"), ["1003"], "a leading minus is no exclusion");
    assert_eq!(found(&store, "10:30"), ["1004"]);
    assert_eq!(found(&store, "time:"), ["1004"], "no column filter");
    assert_eq!(found(&store, "NEAR station"), ["1005"]);
    assert_eq!(found(&store, "NEAR(the station)"), ["1005"]);
    assert_eq!(found(&store, "(maybe)"), ["1006"]);
    assert_eq!(found(&store, "AND that"), ["1007"]);
    assert_eq!(found(&store, "NOT other"), ["1007"], "NOT is a word, not an operator");
    // input FTS5 would refuse as syntax finds nothing, and never fails
    for input in [
        "\"", "*", "-", ":", "(", ")", "()", "NEAR", "NEAR(", "\" OR \"", "a OR", "^", "+", "{x}", "x:y:z", "**a**",
        "\0", "x\0", "\0\"",
    ] {
        store
            .search(input, 10)
            .unwrap_or_else(|error| panic!("{input:?} failed: {error}"));
    }
    Ok(())
}

#[test]
fn a_control_character_in_the_input_parts_terms_like_a_space_and_never_fails() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chats(&dir)?;
    store_indexed(&mut store, &message(CHAT, "1001", 1, "glob a*b star", 1_000))?;
    store_indexed(&mut store, &message(CHAT, "1008", 1, "plain filler text", 1_001))?;
    // FTS5 reads a query only up to a NUL: inside a quoted term it was "unterminated string"; every control
    // character is checked the same way
    for ch in ('\0'..='\u{1f}').chain('\u{7f}'..='\u{9f}') {
        for input in [
            format!("plain{ch}filler"),
            format!("{ch}plain filler{ch}"),
            format!("pla{ch}"),
        ] {
            let hits = store
                .search(&input, 10)
                .unwrap_or_else(|error| panic!("{input:?} failed: {error}"));
            let ids: Vec<&str> = hits.iter().map(|hit| hit.message_id.as_str()).collect();
            assert_eq!(ids, ["1008"], "{input:?}");
        }
    }
    Ok(())
}

#[test]
fn a_search_in_some_chats_counts_only_their_hits_however_many_others_match() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chats(&dir)?;
    let channel = "19:synthetic-channel@thread.tacv2";
    store.upsert_conversation(&conversation(channel, ConversationKind::Channel))?;
    store_indexed(&mut store, &message(CHAT, "1001", 1, "the zeppelin plan", 1_000))?;
    store_indexed(&mut store, &message(GROUP, "2001", 1, "a zeppelin in the group", 2_000))?;
    // newer than both, and more of them than the limit
    for at in 0..20 {
        let id = (3_000 + at).to_string();
        store_indexed(
            &mut store,
            &message(channel, &id, 1, "zeppelin channel post", 3_000 + at),
        )?;
    }
    let ids =
        |hits: Vec<teams_store::SearchHit>| -> Vec<String> { hits.into_iter().map(|hit| hit.message_id).collect() };
    let chats = [CHAT.to_owned(), GROUP.to_owned()];
    assert_eq!(ids(store.search_in("zeppelin", &chats, 5)?), ["2001", "1001"]);
    assert_eq!(
        ids(store.search_in("zeppelin", &chats, 1)?),
        ["2001"],
        "at most the limit"
    );
    assert_eq!(ids(store.search_in("zeppelin", &[CHAT.to_owned()], 5)?), ["1001"]);
    assert!(store.search_in("zeppelin", &[], 5)?.is_empty(), "no chat, no hit");
    assert!(
        store
            .search("zeppelin", 5)?
            .iter()
            .all(|hit| hit.conversation_id == channel),
        "a search in every chat counts the channel's"
    );
    Ok(())
}

#[test]
fn the_match_expression_quotes_each_term_as_a_prefix() {
    assert_eq!(match_expression("hello"), Some(r#""hello"*"#.to_owned()));
    assert_eq!(match_expression("  two  words "), Some(r#""two"* "words"*"#.to_owned()));
    assert_eq!(match_expression(r#"say "hi""#), Some(r#""say"* """hi"""*"#.to_owned()));
    assert_eq!(match_expression("NEAR(a b)"), Some(r#""NEAR(a"* "b)"*"#.to_owned()));
    assert_eq!(match_expression("- * : ( )"), None, "no term has a letter or a digit");
    assert_eq!(
        match_expression("foo\0bar\u{1}baz\u{85}qux"),
        Some(r#""foo"* "bar"* "baz"* "qux"*"#.to_owned()),
        "control characters part terms"
    );
    assert_eq!(match_expression("\0\u{7f}"), None);
    assert_eq!(match_expression(""), None);
    let many = (0..40).map(|at| format!("t{at}")).collect::<Vec<_>>().join(" ");
    let expression = match_expression(&many).expect("terms");
    assert_eq!(expression.matches('*').count(), teams_store::search::MAX_TERMS);
}

#[test]
fn the_backfill_lists_a_batch_at_a_time_and_goes_on_after_a_stop() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chats(&dir)?;
    for at in 0..7 {
        let id = format!("{}", 1_000 + at);
        store.upsert_message(&message(CHAT, &id, 1, &format!("backlog item {at}"), 1_000 + at), None)?;
    }
    store.upsert_message(&message(GROUP, "2001", 1, "backlog in the group", 5_000), None)?;
    // one already indexed (stored with its text) is never listed
    store_indexed(&mut store, &message(GROUP, "2002", 1, "indexed already", 6_000))?;

    let first = store.messages_to_index(None, 3)?;
    let ids: Vec<&str> = first.iter().map(|message| message.id.as_str()).collect();
    assert_eq!(
        ids,
        ["1000", "1001", "1002"],
        "a batch, in the order of conversation and id"
    );
    let texts: Vec<IndexText> = first
        .iter()
        .map(|message| IndexText {
            conversation_id: message.conversation_id.clone(),
            message_id: message.id.clone(),
            version: message.version,
            text: text_of(message),
        })
        .collect();
    assert_eq!(store.index_messages(&texts)?, 3);
    let cursor = IndexCursor {
        conversation_id: CHAT.to_owned(),
        message_id: "1002".to_owned(),
    };
    let next: Vec<String> = store
        .messages_to_index(Some(&cursor), 3)?
        .into_iter()
        .map(|message| message.id)
        .collect();
    assert_eq!(next, ["1003", "1004", "1005"]);
    drop(store);

    // a stop: the next start lists only what wasn't indexed, from the first
    let store = Store::open(&store_path(&dir))?;
    let rest: Vec<(String, String)> = store
        .messages_to_index(None, 100)?
        .into_iter()
        .map(|message| (message.conversation_id, message.id))
        .collect();
    let expected: Vec<(String, String)> = [
        (CHAT, "1003"),
        (CHAT, "1004"),
        (CHAT, "1005"),
        (CHAT, "1006"),
        (GROUP, "2001"),
    ]
    .iter()
    .map(|(chat, id)| ((*chat).to_owned(), (*id).to_owned()))
    .collect();
    // the conversation ids order the batches: the 1:1 chat's before the group's
    assert!(CHAT < GROUP);
    assert_eq!(rest, expected);
    assert_eq!(found(&store, "backlog"), ["1002", "1001", "1000"]);

    assert!(
        !format!("{:?}", texts[0]).contains("backlog"),
        "Debug leaves the text out"
    );
    Ok(())
}

#[test]
fn a_newer_version_is_listed_again_and_a_stale_or_unstored_text_is_left_out() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chats(&dir)?;
    store_indexed(&mut store, &message(CHAT, "1000", 1, "backlog item zero", 1_000))?;
    assert!(store.messages_to_index(None, 100)?.is_empty());
    // a newer version than the indexed one is listed again; a stale text never replaces a newer one
    store.upsert_message(&message(CHAT, "1000", 2, "backlog item zero, edited", 1_000), None)?;
    assert!(
        store
            .messages_to_index(None, 100)?
            .iter()
            .any(|message| message.id == "1000" && message.version == 2)
    );
    let stale = IndexText {
        conversation_id: CHAT.to_owned(),
        message_id: "1000".to_owned(),
        version: 1,
        text: "stale".to_owned(),
    };
    let fresh = IndexText {
        version: 2,
        text: "backlog item zero, edited".to_owned(),
        ..stale.clone()
    };
    assert_eq!(store.index_messages(&[fresh])?, 1);
    assert_eq!(
        store.index_messages(&[stale])?,
        0,
        "an older version's text is left out"
    );
    assert!(found(&store, "stale").is_empty());
    assert_eq!(found(&store, "edited"), ["1000"]);
    // a message no longer stored is left out
    let gone = IndexText {
        conversation_id: CHAT.to_owned(),
        message_id: "9999".to_owned(),
        version: 1,
        text: "gone".to_owned(),
    };
    assert_eq!(store.index_messages(&[gone])?, 0);
    Ok(())
}

#[test]
fn a_snippet_marker_in_a_text_is_never_taken_for_a_match() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chats(&dir)?;
    let text = format!("odd {MATCH_START}marker{MATCH_END} text");
    store_indexed(&mut store, &message(CHAT, "1001", 1, &text, 1_000))?;
    let hits = store.search("odd", 10)?;
    assert_eq!(hits[0].snippet, format!("{MATCH_START}odd{MATCH_END} marker text"));
    Ok(())
}
