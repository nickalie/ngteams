//! The account's calendar events in the store (schema 16), on a real file in a temporary directory: a window's read
//! replaces the events it overlaps and nothing else, kept across a reopen; and the migration from schema 15, which
//! starts with none. Every id and entry here is made up.

use std::path::PathBuf;

use teams_store::{SCHEMA_VERSION, Store, StoreError, StoredEvent};

const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;
/// 2026-11-02T00:00:00Z, a made-up Monday's start.
const MONDAY: i64 = 1_794_009_600_000;

fn store_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("store").join("a.sqlite")
}

fn event(id: &str, start_ms: i64, end_ms: i64, subject: &str) -> StoredEvent {
    StoredEvent {
        id: id.to_owned(),
        start_ms,
        end_ms,
        entry: format!(r#"{{"objectId": "{id}", "subject": "{subject}"}}"#),
    }
}

fn ids(events: &[StoredEvent]) -> Vec<&str> {
    events.iter().map(|event| event.id.as_str()).collect()
}

#[test]
fn a_window_read_replaces_only_the_events_it_overlaps() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&store_path(&dir))?;
    let week = [
        event("mon", MONDAY + 9 * HOUR, MONDAY + 10 * HOUR, "Standup"),
        event("tue", MONDAY + DAY + 9 * HOUR, MONDAY + DAY + 10 * HOUR, "Standup"),
        event("over", MONDAY - HOUR, MONDAY + HOUR, "Night shift"),
        event(
            "next",
            MONDAY + 7 * DAY + 9 * HOUR,
            MONDAY + 7 * DAY + 10 * HOUR,
            "Planning",
        ),
        event("instant", MONDAY + 2 * DAY, MONDAY + 2 * DAY, "A reminder"),
    ];
    store.replace_calendar_window(MONDAY - DAY, MONDAY + 14 * DAY, &week)?;
    assert_eq!(
        ids(&store.calendar_events(MONDAY, MONDAY + 7 * DAY)?),
        ["over", "mon", "tue", "instant"],
        "by start, the one reaching in from the day before too"
    );
    assert_eq!(
        ids(&store.calendar_events(MONDAY + 2 * DAY, MONDAY + 3 * DAY)?),
        ["instant"]
    );
    assert_eq!(
        ids(&store.calendar_events(MONDAY + 3 * DAY, MONDAY + 4 * DAY)?),
        Vec::<&str>::new()
    );

    // Tuesday read again: its standup deleted, Monday's moved there and renamed, a new one added; "over" ends
    // before the window, so it stays
    let tuesday = [
        event(
            "mon",
            MONDAY + DAY + 11 * HOUR,
            MONDAY + DAY + 12 * HOUR,
            "Standup, moved",
        ),
        event("new", MONDAY + DAY + 14 * HOUR, MONDAY + DAY + 15 * HOUR, "Review"),
    ];
    store.replace_calendar_window(MONDAY + DAY, MONDAY + 2 * DAY, &tuesday)?;
    let stored = store.calendar_events(MONDAY - DAY, MONDAY + 14 * DAY)?;
    assert_eq!(ids(&stored), ["over", "mon", "new", "instant", "next"]);
    assert_eq!(stored[1], tuesday[0], "the moved one as now read");

    // an empty read empties its window
    store.replace_calendar_window(MONDAY + 7 * DAY, MONDAY + 8 * DAY, &[])?;
    assert_eq!(
        ids(&store.calendar_events(MONDAY + 7 * DAY, MONDAY + 14 * DAY)?),
        Vec::<&str>::new()
    );
    drop(store);

    let store = Store::open(&store_path(&dir))?;
    assert_eq!(
        ids(&store.calendar_events(MONDAY - DAY, MONDAY + 14 * DAY)?),
        ["over", "mon", "new", "instant"],
        "kept across a reopen"
    );
    Ok(())
}

#[test]
fn an_id_given_twice_keeps_the_last() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&store_path(&dir))?;
    let twice = [
        event("one", MONDAY, MONDAY + HOUR, "First"),
        event("one", MONDAY + 2 * HOUR, MONDAY + 3 * HOUR, "Second"),
    ];
    store.replace_calendar_window(MONDAY, MONDAY + DAY, &twice)?;
    assert_eq!(store.calendar_events(MONDAY, MONDAY + DAY)?, [twice[1].clone()]);
    Ok(())
}

#[test]
fn a_confirmed_write_forgets_the_events_it_touched_in_every_week_and_keeps_a_new_one() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&store_path(&dir))?;
    // two weeks read: a weekly series' occurrence in each, and an event of their own
    let first = [
        event("occ-1", MONDAY + 9 * HOUR, MONDAY + 10 * HOUR, "Weekly"),
        event("own-1", MONDAY + 11 * HOUR, MONDAY + 12 * HOUR, "Kept"),
    ];
    let second = [
        event(
            "occ-2",
            MONDAY + 7 * DAY + 9 * HOUR,
            MONDAY + 7 * DAY + 10 * HOUR,
            "Weekly",
        ),
        event(
            "own-2",
            MONDAY + 7 * DAY + 11 * HOUR,
            MONDAY + 7 * DAY + 12 * HOUR,
            "Kept",
        ),
    ];
    store.replace_calendar_window(MONDAY, MONDAY + 7 * DAY, &first)?;
    store.replace_calendar_window(MONDAY + 7 * DAY, MONDAY + 14 * DAY, &second)?;
    store.forget_calendar_events(&["occ-1".to_owned(), "occ-2".to_owned(), "never-stored".to_owned()])?;
    assert_eq!(
        ids(&store.calendar_events(MONDAY, MONDAY + 14 * DAY)?),
        ["own-1", "own-2"]
    );

    let created = event("new", MONDAY + 7 * DAY + 14 * HOUR, MONDAY + 7 * DAY + 15 * HOUR, "New");
    store.keep_calendar_events(std::slice::from_ref(&created))?;
    assert_eq!(
        ids(&store.calendar_events(MONDAY + 7 * DAY, MONDAY + 14 * DAY)?),
        ["own-2", "new"],
        "kept beside the week's other events"
    );
    let renamed = event(
        "new",
        MONDAY + 7 * DAY + 14 * HOUR,
        MONDAY + 7 * DAY + 15 * HOUR,
        "Renamed",
    );
    store.keep_calendar_events(std::slice::from_ref(&renamed))?;
    assert_eq!(
        store.calendar_events(MONDAY + 7 * DAY + 14 * HOUR, MONDAY + 7 * DAY + 15 * HOUR)?,
        [renamed],
        "one kept again replaces it"
    );
    Ok(())
}

#[test]
fn debug_leaves_the_entry_out() {
    let shown = format!("{:?}", event("one", MONDAY, MONDAY + HOUR, "A private subject"));
    assert!(shown.contains("one"));
    assert!(!shown.contains("private"), "{shown}");
}

#[test]
fn the_calendar_migration_starts_with_no_events() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&store_path(&dir))?;
    store.replace_favorites(&["48:notes".to_owned()], 1_000)?;
    drop(store);
    // the store as schema 15 left it
    let raw = rusqlite::Connection::open(store_path(&dir)).expect("open directly");
    raw.execute_batch("ALTER TABLE teams DROP COLUMN site; ALTER TABLE team_channels DROP COLUMN description; ALTER TABLE team_channels DROP COLUMN archived; ALTER TABLE team_channels DROP COLUMN owner; ALTER TABLE team_channels DROP COLUMN can_edit; ALTER TABLE team_channels DROP COLUMN can_delete; ALTER TABLE team_channels DROP COLUMN channel_group; DROP TABLE calendar_events; DROP TABLE forgotten_conversations; DROP TABLE message_translations; DROP TABLE sent_originals; DROP TABLE call_transcript_lines; DROP TABLE call_transcripts; DROP TABLE translation_glossary; PRAGMA user_version = 15;")
        .expect("back to schema 15");
    drop(raw);

    let mut store = Store::open(&store_path(&dir))?;
    assert_eq!(store.schema_version()?, SCHEMA_VERSION);
    assert_eq!(store.favorites()?, Some(vec!["48:notes".to_owned()]), "the rest kept");
    assert_eq!(store.calendar_events(i64::MIN, i64::MAX)?, []);
    let one = event("one", MONDAY, MONDAY + HOUR, "Standup");
    store.replace_calendar_window(MONDAY, MONDAY + DAY, std::slice::from_ref(&one))?;
    assert_eq!(store.calendar_events(MONDAY, MONDAY + DAY)?, [one]);
    Ok(())
}

#[test]
fn the_calendar_gone_answers_an_error_never_a_panic() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    drop(Store::open(&store_path(&dir))?);
    let raw = rusqlite::Connection::open(store_path(&dir)).expect("open directly");
    raw.execute_batch("DROP TABLE calendar_events;")
        .expect("drop the table");
    drop(raw);

    // opened afresh, so nothing is read from a statement prepared before
    let mut store = Store::open(&store_path(&dir))?;
    let refused = |result: Result<(), StoreError>| matches!(result, Err(StoreError::Sqlite(_)));
    assert!(matches!(
        store.calendar_events(MONDAY, MONDAY + DAY),
        Err(StoreError::Sqlite(_))
    ));
    let events = [event("e1", MONDAY, MONDAY + HOUR, "Synthetic")];
    assert!(refused(store.replace_calendar_window(MONDAY, MONDAY + DAY, &events)));
    assert!(refused(store.keep_calendar_events(&events)));
    assert!(refused(store.forget_calendar_events(&["e1".to_owned()])));
    Ok(())
}
