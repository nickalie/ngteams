//! Store snapshots use real SQLite files, including a writer whose latest commit is still in its WAL.

#![cfg(test)]

use std::fs;
use std::io::ErrorKind;
#[path = "support/invalid_path.rs"]
mod invalid_path;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use teams_store::snapshot::snapshot;
use teams_store::{Conversation, ConversationKind, Store, StoreError};

const CHAT: &str = "19:synthetic-snapshot@thread.v2";

fn conversation(topic: &str, version: i64) -> Conversation {
    Conversation {
        id: CHAT.to_owned(),
        kind: ConversationKind::Group,
        topic: Some(topic.to_owned()),
        version,
        last_activity: Some(123),
    }
}

fn store(path: &Path) -> Store {
    let mut store = Store::open(path).expect("create the source store");
    store
        .upsert_conversation(&conversation("original", 1))
        .expect("store the conversation");
    store
}

fn side_file(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

fn topic(path: &Path) -> String {
    let reader = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("read the copy");
    reader
        .query_row("SELECT topic FROM conversations WHERE id = ?1", [CHAT], |row| {
            row.get(0)
        })
        .expect("copied conversation")
}

fn assert_no_side_files(path: &Path) {
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(
            !side_file(path, suffix).exists(),
            "no {suffix} beside {}",
            path.display()
        );
    }
}

#[test]
fn a_closed_store_is_copied_without_touching_its_file_or_creating_side_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source.sqlite");
    let dest = dir.path().join("copy.sqlite");
    drop(store(&source));
    assert_no_side_files(&source);
    // A read-only source must retain its mode; only the new destination is tightened.
    #[cfg(unix)]
    fs::set_permissions(&source, fs::Permissions::from_mode(0o400)).expect("make source read-only");
    let before = fs::read(&source).expect("source bytes");

    snapshot(&source, &dest).expect("snapshot a closed store");

    #[cfg(unix)]
    assert_eq!(mode(&dest), 0o600);
    #[cfg(unix)]
    assert_eq!(mode(&source), 0o400);
    assert_eq!(fs::read(&source).expect("source after snapshot"), before);
    assert_no_side_files(&source);
    assert_eq!(topic(&dest), "original");
    let copied = Store::open(&dest).expect("the copy is a complete store");
    assert_eq!(
        copied.conversation(CHAT).expect("conversation"),
        Some(conversation("original", 1))
    );
}

#[test]
fn a_running_stores_wal_commit_is_copied_and_later_writes_do_not_change_the_copy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source.sqlite");
    let dest = dir.path().join("copy.sqlite");
    let mut writer = store(&source);
    writer
        .upsert_conversation(&conversation("committed in WAL", 2))
        .expect("commit a newer value");
    let wal = side_file(&source, "-wal");
    let database_before = fs::read(&source).expect("database bytes");
    let wal_before = fs::read(&wal).expect("the writer has a WAL");
    assert!(!wal_before.is_empty());

    snapshot(&source, &dest).expect("snapshot the open store");

    assert_eq!(topic(&dest), "committed in WAL");
    #[cfg(unix)]
    assert_eq!(mode(&dest), 0o600);
    #[cfg(unix)]
    assert_eq!(mode(&source), 0o600);
    assert_eq!(fs::read(&source).expect("source after snapshot"), database_before);
    assert_eq!(fs::read(&wal).expect("WAL after snapshot"), wal_before);
    writer
        .upsert_conversation(&conversation("after snapshot", 3))
        .expect("source still writable");
    drop(writer);
    let reopened = Store::open(&source).expect("reopen source");
    assert_eq!(
        reopened.conversation(CHAT).expect("source row"),
        Some(conversation("after snapshot", 3))
    );
    let mut copied = Store::open(&dest).expect("reopen copy");
    assert_eq!(
        copied.conversation(CHAT).expect("copied row"),
        Some(conversation("committed in WAL", 2))
    );
    copied
        .upsert_conversation(&conversation("changed independently", 4))
        .expect("write to the copy");
    assert_eq!(
        reopened.conversation(CHAT).expect("source row"),
        Some(conversation("after snapshot", 3))
    );
}

#[test]
fn an_uncommitted_write_is_not_included_and_the_writer_can_commit_after_the_snapshot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source.sqlite");
    let dest = dir.path().join("copy.sqlite");
    let _store = store(&source);
    let mut writer = Connection::open(&source).expect("second writer");
    let transaction = writer.transaction().expect("begin write");
    transaction
        .execute("UPDATE conversations SET topic = 'not committed' WHERE id = ?1", [CHAT])
        .expect("write inside the transaction");

    snapshot(&source, &dest).expect("read while a write is pending");

    assert_eq!(topic(&dest), "original");
    transaction.commit().expect("the writer can still commit");
    let busy: i64 = writer
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
        .expect("checkpoint the source after the snapshot");
    assert_eq!(busy, 0, "the snapshot did not keep a reader that blocks a checkpoint");
    assert_eq!(topic(&source), "not committed");
    assert_eq!(topic(&dest), "original");
}

#[test]
fn uri_characters_in_the_source_name_are_literal_and_the_destination_is_bound_as_data() {
    let dir = tempfile::tempdir().expect("tempdir");
    #[cfg(unix)]
    let source = dir.path().join("source %25?# ' ü.sqlite");
    #[cfg(windows)]
    let source = dir.path().join("source %25# ' ü.sqlite");
    #[cfg(unix)]
    let dest = dir.path().join("copy ' ; -- %?#.sqlite");
    #[cfg(windows)]
    let dest = dir.path().join("copy ' ; -- %#.sqlite");
    drop(store(&source));
    let before = fs::read(&source).expect("source bytes");

    snapshot(&source, &dest).expect("snapshot paths containing URI and SQL characters");

    assert_eq!(topic(&dest), "original");
    assert_eq!(fs::read(&source).expect("unchanged source"), before);
    assert_no_side_files(&source);
    #[cfg(unix)]
    assert_eq!(mode(&dest), 0o600);
}

#[test]
fn an_existing_destination_is_refused_and_never_overwritten() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("missing-source.sqlite");
    let dest = dir.path().join("existing.sqlite");
    for bytes in [b"existing data".as_slice(), b"".as_slice()] {
        fs::write(&dest, bytes).expect("create existing destination");
        #[cfg(unix)]
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o640)).expect("set existing mode");

        let error = snapshot(&source, &dest).expect_err("even an empty destination already exists");

        assert!(
            matches!(error, StoreError::File { path, source } if path == dest && source.kind() == ErrorKind::AlreadyExists)
        );
        assert_eq!(fs::read(&dest).expect("existing contents"), bytes);
        #[cfg(unix)]
        assert_eq!(mode(&dest), 0o640);
        assert!(!source.exists(), "the missing source was not created");
    }
}

#[test]
fn a_missing_or_corrupt_source_returns_a_sqlite_error_without_creating_a_copy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source.sqlite");
    let dest = dir.path().join("copy.sqlite");
    assert!(matches!(snapshot(&source, &dest), Err(StoreError::Sqlite(_))));
    assert!(!source.exists(), "read-only open does not create the missing source");
    assert!(!dest.exists());

    fs::write(&source, b"this is not a SQLite database").expect("corrupt source");
    let before = fs::read(&source).expect("corrupt bytes");
    assert!(matches!(snapshot(&source, &dest), Err(StoreError::Sqlite(_))));
    assert_eq!(fs::read(&source).expect("unchanged corrupt file"), before);
    assert!(!dest.exists());
    assert_no_side_files(&source);
}

#[test]
fn an_invalid_destination_returns_an_error_and_leaves_the_source_readable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source.sqlite");
    drop(store(&source));
    let missing_parent = dir.path().join("missing").join("copy.sqlite");
    assert!(matches!(
        snapshot(&source, &missing_parent),
        Err(StoreError::File { path, .. }) if path == missing_parent
    ));
    assert!(!missing_parent.parent().expect("parent").exists());
    assert_eq!(topic(&source), "original");

    let dest = dir.path().join(invalid_path::name("invalid-", ".sqlite"));
    let error = snapshot(&source, &dest).expect_err("SQLite requires a UTF-8 destination");
    assert!(matches!(error, StoreError::Sqlite(rusqlite::Error::InvalidPath(path)) if path == dest));
    assert!(!dest.exists());
    assert_eq!(topic(&source), "original");
}

#[test]
#[cfg_attr(
    target_vendor = "apple",
    ignore = "APFS stores only UTF-8 names: no source can have a non-UTF-8 name there"
)]
fn a_non_utf8_source_never_reads_a_different_file_with_the_lossy_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let original = dir.path().join("original.sqlite");
    drop(store(&original));
    let source = dir.path().join(invalid_path::name("source-", ".sqlite"));
    fs::rename(&original, &source).expect("move the source to a non-UTF-8 name");
    let lossy = PathBuf::from(source.to_string_lossy().into_owned());
    let mut decoy = store(&lossy);
    decoy
        .upsert_conversation(&conversation("a different database", 2))
        .expect("decoy data");
    drop(decoy);
    let source_before = fs::read(&source).expect("source bytes");
    let decoy_before = fs::read(&lossy).expect("decoy bytes");
    let dest = dir.path().join("copy.sqlite");

    let error = snapshot(&source, &dest).expect_err("an unsupported source path must be rejected");

    assert!(matches!(error, StoreError::Sqlite(rusqlite::Error::InvalidPath(path)) if path == source));
    assert!(!dest.exists());
    assert_eq!(fs::read(&source).expect("unchanged source"), source_before);
    assert_eq!(fs::read(&lossy).expect("unchanged decoy"), decoy_before);
    assert_no_side_files(&source);
    assert_no_side_files(&lossy);
}
