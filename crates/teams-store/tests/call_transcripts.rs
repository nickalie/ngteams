//! Calls' live captions saved on this computer in the store (schema 21), on a real file in a temporary directory: a
//! transcript opened for a call, its lines kept by utterance (a later final result replacing the text) across a
//! reopen, a call rejoined soon after going on in the same transcript and a later one starting another, the chat's
//! transcripts listed (one without a line left out), and one deleted with its lines. Every id and word here is made up.

use std::path::PathBuf;

use teams_store::{CallTranscript, SCHEMA_VERSION, Store, StoreError, TranscriptLine};

const CHAT: &str = "19:synthetic-call@unq.gbl.spaces";
const OTHER: &str = "19:meeting_synthetic@thread.v2";
const REJOIN: i64 = 5 * 60 * 1000;

fn store_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("store").join("a.sqlite")
}

fn line(utterance: &str, at: i64, text: &str) -> TranscriptLine {
    TranscriptLine {
        utterance: utterance.to_owned(),
        speaker: Some("8:orgid:synthetic-b".to_owned()),
        speaker_name: Some("Lab B".to_owned()),
        started_at: at,
        ended_at: at + 2_000,
        text: text.to_owned(),
    }
}

#[test]
fn a_calls_lines_are_kept_by_utterance_across_a_reopen() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&store_path(&dir))?;
    let id = store.open_call_transcript(CHAT, None, 1_000_000, REJOIN)?;
    // a transcript without a line isn't listed
    assert!(store.call_transcripts(CHAT)?.is_empty());
    store.put_transcript_lines(id, &[line("u1", 1_010_000, "First."), line("u2", 1_020_000, "Second.")])?;
    store.put_transcript_lines(id, &[line("u1", 1_010_000, "First, corrected.")])?;
    drop(store);

    let store = Store::open(&store_path(&dir))?;
    let lines = store.transcript_lines(id)?;
    assert_eq!(
        lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>(),
        ["First, corrected.", "Second."]
    );
    assert_eq!(lines[0].speaker_name.as_deref(), Some("Lab B"));
    assert_eq!(
        store.call_transcripts(CHAT)?,
        [CallTranscript {
            id,
            conversation_id: CHAT.to_owned(),
            title: None,
            started_at: 1_000_000,
            ended_at: 1_022_000,
            lines: 2,
        }]
    );
    // the words stay out of its debug form
    assert!(!format!("{:?}", lines[0]).contains("corrected"));
    Ok(())
}

#[test]
fn a_rejoin_goes_on_in_the_same_transcript_and_a_later_call_starts_another() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&store_path(&dir))?;
    let first = store.open_call_transcript(OTHER, Some("Synthetic sync"), 1_000_000, REJOIN)?;
    store.put_transcript_lines(first, &[line("u1", 1_010_000, "Hello.")])?;
    store.end_call_transcript(first, 1_060_000)?;
    // back four minutes after it ended: the same call
    let again = store.open_call_transcript(OTHER, None, 1_300_000, REJOIN)?;
    assert_eq!(again, first);
    store.put_transcript_lines(again, &[line("u2", 1_310_000, "Back again.")])?;
    store.end_call_transcript(again, 1_400_000)?;
    // an hour later: another call
    let later = store.open_call_transcript(OTHER, None, 5_000_000, REJOIN)?;
    assert_ne!(later, first);
    store.put_transcript_lines(later, &[line("u1", 5_010_000, "Next week.")])?;
    // another chat's call is another transcript
    let elsewhere = store.open_call_transcript(CHAT, None, 1_300_000, REJOIN)?;
    assert_ne!(elsewhere, first);

    let listed = store.call_transcripts(OTHER)?;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].title.as_deref(), Some("Synthetic sync"));
    assert_eq!(
        (listed[0].started_at, listed[0].ended_at, listed[0].lines),
        (1_000_000, 1_400_000, 2)
    );
    assert_eq!(listed[1].id, later);
    Ok(())
}

#[test]
fn a_deleted_transcript_goes_with_its_lines_and_takes_none_after() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&store_path(&dir))?;
    let id = store.open_call_transcript(CHAT, None, 1_000_000, REJOIN)?;
    store.put_transcript_lines(id, &[line("u1", 1_010_000, "Gone soon.")])?;
    assert!(store.delete_call_transcript(id)?);
    assert!(!store.delete_call_transcript(id)?);
    assert!(store.call_transcripts(CHAT)?.is_empty());
    assert!(store.transcript_lines(id)?.is_empty());
    // a line of the call still running isn't kept for the transcript deleted under it
    store.put_transcript_lines(id, &[line("u2", 1_020_000, "Too late.")])?;
    assert!(store.transcript_lines(id)?.is_empty());
    store.end_call_transcript(id, 1_100_000)?;
    assert!(store.call_transcripts(CHAT)?.is_empty());
    Ok(())
}

#[test]
fn the_call_transcripts_migration_starts_with_none() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    drop(Store::open(&store_path(&dir))?);
    // the store as schema 20 left it
    let raw = rusqlite::Connection::open(store_path(&dir)).expect("open directly");
    raw.execute_batch("DROP TABLE call_transcript_lines; DROP TABLE call_transcripts; DROP TABLE translation_glossary; DROP TABLE horizon_bookmarks; DROP VIEW pending_conversation_previews; DROP TABLE conversation_previews; PRAGMA user_version = 20;")
        .expect("back to schema 20");
    drop(raw);
    let store = Store::open(&store_path(&dir))?;
    assert_eq!(store.schema_version()?, SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 24);
    assert!(store.call_transcripts(CHAT)?.is_empty());
    Ok(())
}

fn refused<T>(result: &Result<T, StoreError>) -> bool {
    matches!(result, Err(StoreError::Sqlite(_)))
}

#[test]
fn the_transcripts_gone_answer_an_error_never_a_panic() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&store_path(&dir))?;
    drop(store);
    let raw = rusqlite::Connection::open(store_path(&dir)).expect("open directly");
    raw.execute_batch("DROP TABLE call_transcript_lines; DROP TABLE call_transcripts;")
        .expect("drop the tables");
    drop(raw);

    // opened afresh, so nothing is read from a statement prepared before
    let mut store = Store::open(&store_path(&dir))?;
    assert!(refused(&store.open_call_transcript(CHAT, None, 1_000, REJOIN)));
    assert!(refused(&store.put_transcript_lines(1, &[line("u1", 1_010, "First.")])));
    assert!(refused(&store.end_call_transcript(1, 2_000)));
    assert!(refused(&store.call_transcripts(CHAT)));
    assert!(refused(&store.transcript_lines(1)));
    assert!(refused(&store.delete_call_transcript(1)));
    Ok(())
}
