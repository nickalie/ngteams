//! Chats translated on this computer in the store (schema 20), on a real file in a temporary directory: a message's
//! translation kept by its id, version and language across a reopen, an older version's dropped when a newer one is
//! kept, the account's own messages as written by their key, a chat's glossary by its terms, newest first and trimmed,
//! and all of them gone with their conversation. Every id and text here is made up.

use std::path::PathBuf;

use teams_store::{
    Conversation, ConversationKind, GLOSSARY_TERMS, GlossaryTerm, Message, SCHEMA_VERSION, SentOriginal, Store,
    StoreError, StoredTranslation, TranslationKind,
};

const CHAT: &str = "19:synthetic-translated@unq.gbl.spaces";
const OTHER: &str = "19:synthetic-other@unq.gbl.spaces";

fn store_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("store").join("a.sqlite")
}

fn with_chats(dir: &tempfile::TempDir) -> Result<Store, StoreError> {
    let mut store = Store::open(&store_path(dir))?;
    for id in [CHAT, OTHER] {
        store.upsert_conversation(&Conversation {
            id: id.to_owned(),
            kind: ConversationKind::OneToOne,
            topic: None,
            version: 1,
            last_activity: None,
        })?;
    }
    for (conversation, id) in [(CHAT, "1"), (CHAT, "2"), (OTHER, "1")] {
        store.upsert_message(
            &Message {
                conversation_id: conversation.to_owned(),
                id: id.to_owned(),
                version: 10,
                client_message_id: None,
                sender: Some("8:orgid:synthetic-b".to_owned()),
                sender_name: None,
                message_type: "RichText/Html".to_owned(),
                content: "<p>Synthetic text</p>".to_owned(),
                properties: None,
                arrival_time: 1000,
                compose_time: None,
                edit_time: None,
                delete_time: None,
            },
            None,
        )?;
    }
    Ok(store)
}

fn translation(conversation: &str, id: &str, version: i64, language: &str, kind: TranslationKind) -> StoredTranslation {
    StoredTranslation {
        conversation_id: conversation.to_owned(),
        message_id: id.to_owned(),
        version,
        language: language.to_owned(),
        kind,
        content: format!("synthetic {id} v{version} in {language}"),
        source_language: Some("en".to_owned()),
    }
}

fn original(conversation: &str, key: &str, content: &str) -> SentOriginal {
    SentOriginal {
        conversation_id: conversation.to_owned(),
        key: key.to_owned(),
        language: "de".to_owned(),
        content: content.to_owned(),
        sent_text: "Synthetic sent".to_owned(),
        created_at: 5,
    }
}

fn ids(found: &[StoredTranslation]) -> Vec<(String, i64)> {
    let mut ids: Vec<_> = found
        .iter()
        .map(|kept| (kept.message_id.clone(), kept.version))
        .collect();
    ids.sort();
    ids
}

#[test]
fn translations_are_kept_by_message_version_and_language() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let mut store = with_chats(&dir)?;
        assert_eq!(store.schema_version()?, SCHEMA_VERSION);
        store.put_translations(&[
            translation(CHAT, "1", 10, "de", TranslationKind::Translated),
            translation(CHAT, "2", 10, "de", TranslationKind::Same),
            translation(CHAT, "1", 10, "fr", TranslationKind::Translated),
            translation(OTHER, "1", 10, "de", TranslationKind::Translated),
            // a message the store doesn't hold is left out
            translation(CHAT, "missing", 10, "de", TranslationKind::Translated),
        ])?;
    }
    // a later run reads them back
    let mut store = Store::open(&store_path(&dir))?;
    let wanted = ["1", "2", "missing"].map(str::to_owned);
    let german = store.translations(CHAT, "de", &wanted)?;
    assert_eq!(ids(&german), [("1".to_owned(), 10), ("2".to_owned(), 10)]);
    let first = german.iter().find(|kept| kept.message_id == "1").expect("kept");
    assert_eq!(first.kind, TranslationKind::Translated);
    assert_eq!(first.content, "synthetic 1 v10 in de");
    assert_eq!(first.source_language.as_deref(), Some("en"));
    assert_eq!(
        german.iter().find(|kept| kept.message_id == "2").map(|kept| kept.kind),
        Some(TranslationKind::Same)
    );
    assert_eq!(ids(&store.translations(CHAT, "fr", &wanted)?), [("1".to_owned(), 10)]);
    assert!(store.translations(CHAT, "es", &wanted)?.is_empty());
    assert!(
        format!("{first:?}").contains("message_id") && !format!("{first:?}").contains("synthetic 1"),
        "Debug leaves the text out"
    );

    // a newer version's translation replaces the older one in that language only
    store.put_translations(&[translation(CHAT, "1", 11, "de", TranslationKind::Original)])?;
    let german = store.translations(CHAT, "de", &wanted)?;
    assert_eq!(ids(&german), [("1".to_owned(), 11), ("2".to_owned(), 10)]);
    assert_eq!(ids(&store.translations(CHAT, "fr", &wanted)?), [("1".to_owned(), 10)]);
    // the same key again replaces it
    let mut again = translation(CHAT, "2", 10, "de", TranslationKind::Translated);
    again.content = "synthetic again".to_owned();
    store.put_translations(&[again])?;
    let german = store.translations(CHAT, "de", &["2".to_owned()])?;
    assert_eq!(
        german.first().map(|kept| kept.content.as_str()),
        Some("synthetic again")
    );
    assert_eq!(TranslationKind::Same.key(), "same");
    Ok(())
}

#[test]
fn sent_originals_are_kept_by_key_and_go_with_their_conversation() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chats(&dir)?;
    store.put_sent_original(&original(CHAT, "client-1", "as written"))?;
    store.put_sent_original(&original(CHAT, &SentOriginal::edit_key("1"), "edited as written"))?;
    store.put_sent_original(&original("19:unknown@unq.gbl.spaces", "client-2", "left out"))?;
    let keys = ["client-1".to_owned(), "edit:1".to_owned(), "client-2".to_owned()];
    let mut found = store.sent_originals(CHAT, &keys)?;
    found.sort_by(|a, b| a.key.cmp(&b.key));
    assert_eq!(
        found.iter().map(|kept| kept.key.as_str()).collect::<Vec<_>>(),
        ["client-1", "edit:1"]
    );
    assert_eq!(found[0].content, "as written");
    assert_eq!(found[0].sent_text, "Synthetic sent");
    assert_eq!(found[0].language, "de");
    assert_eq!(found[0].created_at, 5);
    assert!(
        !format!("{:?}", found[0]).contains("as written"),
        "Debug leaves the text out"
    );
    assert!(store.sent_originals("19:unknown@unq.gbl.spaces", &keys)?.is_empty());

    // a later edit of the same message replaces its original
    store.put_sent_original(&original(CHAT, "edit:1", "edited again"))?;
    let edit = store.sent_originals(CHAT, &["edit:1".to_owned()])?;
    assert_eq!(edit.first().map(|kept| kept.content.as_str()), Some("edited again"));

    // forgetting the chat forgets them, and its messages' translations
    store.put_translations(&[translation(CHAT, "1", 10, "de", TranslationKind::Translated)])?;
    store.forget_conversation(CHAT)?;
    assert!(store.sent_originals(CHAT, &keys)?.is_empty());
    assert!(store.translations(CHAT, "de", &["1".to_owned()])?.is_empty());
    Ok(())
}

#[test]
fn a_long_list_of_ids_is_read_in_parts() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chats(&dir)?;
    store.put_translations(&[translation(CHAT, "2", 10, "de", TranslationKind::Translated)])?;
    let mut wanted: Vec<String> = (0..1000).map(|n| format!("unknown-{n}")).collect();
    wanted.push("2".to_owned());
    assert_eq!(ids(&store.translations(CHAT, "de", &wanted)?), [("2".to_owned(), 10)]);
    let mut keys: Vec<String> = (0..1000).map(|n| format!("unknown-{n}")).collect();
    store.put_sent_original(&original(CHAT, "client-9", "as written"))?;
    keys.push("client-9".to_owned());
    assert_eq!(store.sent_originals(CHAT, &keys)?.len(), 1);
    Ok(())
}

#[test]
fn the_translations_migration_starts_with_none() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    drop(with_chats(&dir)?);
    // the store as schema 19 left it
    let raw = rusqlite::Connection::open(store_path(&dir)).expect("open directly");
    raw.execute_batch("DROP TABLE message_translations; DROP TABLE sent_originals; DROP TABLE call_transcript_lines; DROP TABLE call_transcripts; DROP TABLE translation_glossary; DROP TABLE blocked_copies; DROP TABLE horizon_bookmarks; DROP VIEW pending_conversation_previews; DROP TABLE conversation_previews; PRAGMA user_version = 19;")
        .expect("back to schema 19");
    drop(raw);
    let store = Store::open(&store_path(&dir))?;
    assert_eq!(store.schema_version()?, SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 25);
    assert!(store.translations(CHAT, "de", &["1".to_owned()])?.is_empty());
    assert!(store.sent_originals(CHAT, &["client-1".to_owned()])?.is_empty());
    Ok(())
}

#[test]
fn the_translations_gone_answer_an_error_never_a_panic() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    drop(with_chats(&dir)?);
    let raw = rusqlite::Connection::open(store_path(&dir)).expect("open directly");
    raw.execute_batch("DROP TABLE message_translations; DROP TABLE sent_originals;")
        .expect("drop the tables");
    drop(raw);

    // opened afresh, so nothing is read from a statement prepared before
    let mut store = Store::open(&store_path(&dir))?;
    let refused = |result: Result<(), StoreError>| matches!(result, Err(StoreError::Sqlite(_)));
    assert!(matches!(
        store.translations(CHAT, "de", &["1".to_owned()]),
        Err(StoreError::Sqlite(_))
    ));
    let translation = StoredTranslation {
        conversation_id: CHAT.to_owned(),
        message_id: "1".to_owned(),
        version: 1,
        language: "de".to_owned(),
        kind: TranslationKind::Same,
        content: String::new(),
        source_language: None,
    };
    assert!(refused(store.put_translations(&[translation])));
    assert!(matches!(
        store.sent_originals(CHAT, &["key".to_owned()]),
        Err(StoreError::Sqlite(_))
    ));
    let original = SentOriginal {
        conversation_id: CHAT.to_owned(),
        key: "key".to_owned(),
        language: "de".to_owned(),
        content: "Hallo".to_owned(),
        sent_text: "Hello".to_owned(),
        created_at: 1,
    };
    assert!(refused(store.put_sent_original(&original)));
    Ok(())
}

fn term(conversation: &str, language: &str, term: &str, rendering: &str, used_at: i64) -> GlossaryTerm {
    GlossaryTerm {
        conversation_id: conversation.to_owned(),
        language: language.to_owned(),
        term: term.to_owned(),
        rendering: rendering.to_owned(),
        used_at,
    }
}

fn renderings(found: &[GlossaryTerm]) -> Vec<(&str, &str)> {
    found
        .iter()
        .map(|kept| (kept.term.as_str(), kept.rendering.as_str()))
        .collect()
}

#[test]
fn a_chat_s_glossary_is_kept_by_term_newest_first_and_trimmed() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let mut store = with_chats(&dir)?;
        store.put_glossary(&[
            term(CHAT, "en", "Abnahme", "acceptance", 1),
            term(CHAT, "en", "Lastenheft", "requirements spec", 2),
            term(CHAT, "fr", "Abnahme", "recette", 3),
            term(OTHER, "en", "Abnahme", "decrease", 4),
            term("19:synthetic-unknown@unq.gbl.spaces", "en", "Ignored", "ignored", 5),
        ])?;
        // a term named again replaces its rendering and comes first
        store.put_glossary(&[term(CHAT, "en", "Abnahme", "sign-off", 6)])?;
        let debug = format!("{:?}", store.glossary(CHAT, "en")?);
        assert!(!debug.contains("sign-off"), "Debug leaves the words out: {debug}");
    }
    let mut store = Store::open(&store_path(&dir))?;
    assert_eq!(
        renderings(&store.glossary(CHAT, "en")?),
        [("Abnahme", "sign-off"), ("Lastenheft", "requirements spec")]
    );
    assert_eq!(renderings(&store.glossary(CHAT, "fr")?), [("Abnahme", "recette")]);
    assert_eq!(renderings(&store.glossary(OTHER, "en")?), [("Abnahme", "decrease")]);
    assert!(store.glossary("19:synthetic-unknown@unq.gbl.spaces", "en")?.is_empty());

    // only the newest are kept
    let many: Vec<GlossaryTerm> = (0..GLOSSARY_TERMS + 5)
        .map(|n| {
            term(
                CHAT,
                "en",
                &format!("term {n}"),
                "rendering",
                100 + i64::try_from(n).unwrap_or(0),
            )
        })
        .collect();
    store.put_glossary(&many)?;
    let kept = store.glossary(CHAT, "en")?;
    assert_eq!(kept.len(), GLOSSARY_TERMS);
    assert_eq!(kept[0].term, format!("term {}", GLOSSARY_TERMS + 4));
    assert!(!kept.iter().any(|kept| kept.term == "Abnahme"));
    assert_eq!(renderings(&store.glossary(CHAT, "fr")?), [("Abnahme", "recette")]);

    // forgetting the chat forgets its glossary
    store.forget_conversation(CHAT)?;
    assert!(store.glossary(CHAT, "fr")?.is_empty());
    assert_eq!(store.glossary(OTHER, "en")?.len(), 1);
    Ok(())
}

#[test]
fn the_glossary_gone_answers_an_error_never_a_panic() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    drop(with_chats(&dir)?);
    let raw = rusqlite::Connection::open(store_path(&dir)).expect("open directly");
    raw.execute_batch("DROP TABLE translation_glossary;")
        .expect("drop the table");
    drop(raw);
    let mut store = Store::open(&store_path(&dir))?;
    assert!(matches!(store.glossary(CHAT, "en"), Err(StoreError::Sqlite(_))));
    assert!(matches!(
        store.put_glossary(&[term(CHAT, "en", "a", "b", 1)]),
        Err(StoreError::Sqlite(_))
    ));
    Ok(())
}
