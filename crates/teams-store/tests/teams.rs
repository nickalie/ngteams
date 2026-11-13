//! The account's teams and channels in the store, and each channel message's post (M2 Task 16), on a real file in a
//! temporary directory: the list replaced whole and told when it changed, kept across a reopen; a post recorded only
//! for a stored message and gone with it; and the migration's restart of the channels' syncs. Every id and name here
//! is made up.

use std::path::PathBuf;

use teams_store::{
    Applied, ChannelKind, Conversation, ConversationKind, Message, SCHEMA_VERSION, Store, StoreError, SyncState, Team,
    TeamChannel,
};

const TEAM: &str = "19:team0001general@thread.tacv2";
const CHANNEL: &str = "19:team0001channel2@thread.tacv2";
const OTHER_TEAM: &str = "19:team0002general@thread.tacv2";
const CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const PEER: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";

fn store_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("store").join("a.sqlite")
}

fn channel(id: &str, name: &str, general: bool) -> TeamChannel {
    TeamChannel {
        id: id.to_owned(),
        name: name.to_owned(),
        general,
        kind: ChannelKind::Standard,
        about: teams_store::ChannelAbout::default(),
        shown: true,
    }
}

fn teams() -> Vec<Team> {
    vec![
        Team {
            id: TEAM.to_owned(),
            name: "A made-up team".to_owned(),
            archived: false,
            group_id: None,
            mailbox: None,
            owner: false,
            can_add_channel: false,
            can_add_private_channel: false,
            about: teams_store::TeamAbout::default(),
            channels: vec![
                channel(TEAM, "General", true),
                channel(CHANNEL, "A made-up channel", false),
            ],
        },
        Team {
            id: OTHER_TEAM.to_owned(),
            name: "A made-up archived team".to_owned(),
            archived: true,
            group_id: None,
            mailbox: None,
            owner: false,
            can_add_channel: false,
            can_add_private_channel: false,
            about: teams_store::TeamAbout::default(),
            channels: vec![channel(OTHER_TEAM, "General", true)],
        },
    ]
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

fn message(conversation: &str, id: &str) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(PEER.to_owned()),
        sender_name: None,
        message_type: "RichText/Html".to_owned(),
        content: "<p>made up</p>".to_owned(),
        properties: None,
        arrival_time: id.parse().unwrap_or_default(),
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

fn sync_state(conversation: &str) -> SyncState {
    SyncState {
        conversation_id: conversation.to_owned(),
        sync_link: Some("https://example.test/sync".to_owned()),
        backward_link: None,
        backward_oldest: None,
        sync_until: None,
        gap_link: None,
        gap_until: None,
        history_complete: true,
        updated: 1,
    }
}

#[test]
fn the_teams_are_replaced_whole_and_told_only_when_they_changed() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&store_path(&dir))?;
    assert_eq!(store.teams()?, []);
    assert_eq!(store.teams_read_at()?, None);

    assert_eq!(store.replace_teams(&teams(), 1_000)?, Applied::Inserted);
    assert_eq!(store.teams()?, teams());
    assert_eq!(store.teams_read_at()?, Some(1_000));

    // the same list again: only the read time moves
    assert_eq!(store.replace_teams(&teams(), 2_000)?, Applied::Ignored);
    assert_eq!(store.teams_read_at()?, Some(2_000));

    // a channel renamed and one gone, a team left: replaced whole
    let mut changed = teams();
    changed.truncate(1);
    changed[0].channels = vec![channel(TEAM, "General (renamed)", true)];
    assert_eq!(store.replace_teams(&changed, 3_000)?, Applied::Updated);
    assert_eq!(store.teams()?, changed);

    // none at all is a list too
    assert_eq!(store.replace_teams(&[], 4_000)?, Applied::Updated);
    assert_eq!(store.teams()?, []);

    // a team or a channel listed twice keeps its first place; kept across a reopen; `Debug` shows no name
    let mut twice = teams();
    twice.push(teams()[0].clone());
    twice[1].channels.push(channel(CHANNEL, "listed again", false));
    store.replace_teams(&twice, 5_000)?;
    drop(store);
    let store = Store::open(&store_path(&dir))?;
    assert_eq!(store.teams()?, teams());
    let shown = format!("{:?}", store.teams()?);
    assert!(!shown.contains("made-up") && !shown.contains("General"), "{shown}");
    Ok(())
}

#[test]
fn a_post_is_recorded_only_for_a_stored_message_and_goes_with_it() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&store_path(&dir))?;
    store.upsert_conversation(&conversation(CHANNEL, ConversationKind::Channel))?;
    store.upsert_message(&message(CHANNEL, "1001"), None)?;
    store.upsert_message(&message(CHANNEL, "1002"), None)?;

    store.set_reply_root(CHANNEL, "1001", "1001")?;
    store.set_reply_root(CHANNEL, "1002", "999")?;
    // a message the store doesn't hold gets none
    store.set_reply_root(CHANNEL, "1003", "1001")?;
    // the newest one wins
    store.set_reply_root(CHANNEL, "1002", "1001")?;
    let mut roots = store.reply_roots(CHANNEL)?;
    roots.sort();
    assert_eq!(
        roots,
        [
            ("1001".to_owned(), "1001".to_owned()),
            ("1002".to_owned(), "1001".to_owned())
        ]
    );
    assert_eq!(store.reply_roots(TEAM)?, []);

    // a conversation deleted takes its messages and their posts with it
    let raw = rusqlite::Connection::open(store_path(&dir)).expect("open directly");
    raw.execute_batch("PRAGMA foreign_keys = ON;").expect("foreign keys");
    raw.execute("DELETE FROM conversations WHERE id = ?1", [CHANNEL])
        .expect("delete");
    drop(raw);
    assert_eq!(store.reply_roots(CHANNEL)?, []);
    Ok(())
}

#[test]
fn the_migration_starts_the_channels_syncs_again_and_keeps_the_chats() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&store_path(&dir))?;
    store.upsert_conversation(&conversation(CHAT, ConversationKind::OneToOne))?;
    store.upsert_conversation(&conversation(TEAM, ConversationKind::Channel))?;
    store.set_sync_state(&sync_state(CHAT))?;
    store.set_sync_state(&sync_state(TEAM))?;
    drop(store);
    // the store as schema 8 left it
    let raw = rusqlite::Connection::open(store_path(&dir)).expect("open directly");
    raw.execute_batch(
        "DROP TABLE unsynced_conversations; ALTER TABLE conversation_sync DROP COLUMN sync_until;
         ALTER TABLE conversation_sync DROP COLUMN gap_until;
         DROP TABLE reply_roots; DROP TABLE team_channels; DROP TABLE teams; DROP TABLE teams_read;
         DROP TABLE favorites; DROP TABLE favorites_read; ALTER TABLE conversations DROP COLUMN empty;
         DROP TABLE app_read_horizon; DROP TABLE calendar_events; DROP TABLE forgotten_conversations; DROP TABLE message_translations; DROP TABLE sent_originals; DROP TABLE call_transcript_lines; DROP TABLE call_transcripts; DROP TABLE translation_glossary; DROP TABLE blocked_copies; DROP TABLE horizon_bookmarks; DROP VIEW pending_conversation_previews; DROP TABLE conversation_previews; PRAGMA user_version = 8;",
    )
    .expect("back to schema 8");
    drop(raw);

    let store = Store::open(&store_path(&dir))?;
    assert_eq!(store.schema_version()?, SCHEMA_VERSION);
    assert_eq!(store.sync_state(CHAT)?, Some(sync_state(CHAT)));
    assert_eq!(
        store.sync_state(TEAM)?,
        None,
        "a channel's sync starts again from the newest page"
    );
    assert_eq!(store.teams()?, []);
    assert_eq!(store.favorites()?, None);
    assert_eq!(store.empty_conversations()?, Vec::<String>::new());
    Ok(())
}

#[test]
fn the_favourites_are_replaced_whole_in_their_order_and_kept_across_a_reopen() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&store_path(&dir))?;
    assert_eq!(store.favorites()?, None, "none before a read");
    let listed = ["48:notes".to_owned(), CHAT.to_owned(), "48:notes".to_owned()];
    assert_eq!(store.replace_favorites(&listed, 1_000)?, Applied::Inserted);
    assert_eq!(store.replace_favorites(&listed[..2], 2_000)?, Applied::Ignored);
    drop(store);
    let mut store = Store::open(&store_path(&dir))?;
    assert_eq!(store.favorites()?, Some(vec!["48:notes".to_owned(), CHAT.to_owned()]));
    let reordered = [CHAT.to_owned(), "48:notes".to_owned()];
    assert_eq!(store.replace_favorites(&reordered, 3_000)?, Applied::Updated);
    assert_eq!(store.favorites()?, Some(reordered.to_vec()));
    assert_eq!(store.replace_favorites(&[], 4_000)?, Applied::Updated);
    assert_eq!(store.favorites()?, Some(Vec::new()), "none, known");
    Ok(())
}

#[test]
fn a_conversation_is_empty_only_as_the_service_last_said() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&store_path(&dir))?;
    store.upsert_conversation(&conversation(CHAT, ConversationKind::OneToOne))?;
    assert_eq!(store.empty_conversations()?, Vec::<String>::new());
    assert!(store.set_conversation_empty(CHAT, Some(true))?);
    assert!(!store.set_conversation_empty(CHAT, Some(true))?, "no change");
    assert!(!store.set_conversation_empty("19:unknown@thread.v2", Some(true))?);
    assert_eq!(store.empty_conversations()?, [CHAT]);
    assert!(store.set_conversation_empty(CHAT, None)?);
    assert_eq!(
        store.empty_conversations()?,
        Vec::<String>::new(),
        "not known is not empty"
    );
    assert!(store.set_conversation_empty(CHAT, Some(false))?);
    assert_eq!(store.empty_conversations()?, Vec::<String>::new());
    Ok(())
}

#[test]
fn the_favourites_migration_keeps_the_teams_but_has_them_read_again() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&store_path(&dir))?;
    store.replace_teams(&teams(), 5_000)?;
    store.upsert_conversation(&conversation(CHAT, ConversationKind::OneToOne))?;
    drop(store);
    // the store as schema 10 left it
    let raw = rusqlite::Connection::open(store_path(&dir)).expect("open directly");
    raw.execute_batch(
        "DROP TABLE unsynced_conversations; ALTER TABLE conversation_sync DROP COLUMN sync_until;
         ALTER TABLE conversation_sync DROP COLUMN gap_until;
         DROP TABLE favorites; DROP TABLE favorites_read; ALTER TABLE conversations DROP COLUMN empty;
         ALTER TABLE teams DROP COLUMN group_id; ALTER TABLE teams DROP COLUMN mailbox;
         ALTER TABLE teams DROP COLUMN owner; ALTER TABLE teams DROP COLUMN can_add_channel;
         ALTER TABLE teams DROP COLUMN can_add_private_channel; ALTER TABLE teams DROP COLUMN tenant; ALTER TABLE teams DROP COLUMN description; ALTER TABLE teams DROP COLUMN public;
         ALTER TABLE teams DROP COLUMN people; ALTER TABLE teams DROP COLUMN guest; ALTER TABLE teams DROP COLUMN dynamic_membership;
         ALTER TABLE teams DROP COLUMN tenant_wide; ALTER TABLE teams DROP COLUMN channel_only; ALTER TABLE teams DROP COLUMN hidden;
         ALTER TABLE teams DROP COLUMN picture_etag; ALTER TABLE teams DROP COLUMN site; ALTER TABLE team_channels DROP COLUMN shown; ALTER TABLE team_channels DROP COLUMN kind; ALTER TABLE team_channels DROP COLUMN description; ALTER TABLE team_channels DROP COLUMN archived; ALTER TABLE team_channels DROP COLUMN owner; ALTER TABLE team_channels DROP COLUMN can_edit; ALTER TABLE team_channels DROP COLUMN can_delete; ALTER TABLE team_channels DROP COLUMN channel_group;
         DROP TABLE app_read_horizon; DROP TABLE calendar_events; DROP TABLE forgotten_conversations;
         DROP TABLE message_translations; DROP TABLE sent_originals; DROP TABLE call_transcript_lines; DROP TABLE call_transcripts; DROP TABLE translation_glossary; DROP TABLE blocked_copies; DROP TABLE horizon_bookmarks; DROP VIEW pending_conversation_previews; DROP TABLE conversation_previews;
         PRAGMA user_version = 10;",
    )
    .expect("back to schema 10");
    drop(raw);

    let store = Store::open(&store_path(&dir))?;
    assert_eq!(store.schema_version()?, SCHEMA_VERSION);
    assert_eq!(store.teams()?, teams());
    assert_eq!(store.teams_read_at()?, Some(0), "old: read again at the next ask");
    assert_eq!(store.favorites()?, None);
    assert_eq!(store.empty_conversations()?, Vec::<String>::new());
    Ok(())
}

#[test]
fn a_team_keeps_what_its_new_channel_needs_and_a_channel_its_kind() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("a.sqlite")).unwrap();
    let team = Team {
        id: "19:team@thread.tacv2".to_owned(),
        name: "A team".to_owned(),
        archived: false,
        group_id: Some("group".to_owned()),
        mailbox: Some("team@example.invalid".to_owned()),
        owner: true,
        can_add_channel: true,
        can_add_private_channel: true,
        about: teams_store::TeamAbout::default(),
        channels: vec![TeamChannel {
            id: "19:hush@thread.tacv2".to_owned(),
            name: "Hush".to_owned(),
            general: false,
            kind: ChannelKind::Private,
            about: teams_store::ChannelAbout::default(),
            shown: true,
        }],
    };
    store.replace_teams(std::slice::from_ref(&team), 5).unwrap();
    assert_eq!(store.teams().unwrap(), vec![team]);
}

#[test]
fn a_count_out_of_range_reads_as_an_error_never_a_panic() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&store_path(&dir))?;
    store.replace_teams(&teams(), 1)?;
    let raw = rusqlite::Connection::open(store_path(&dir)).expect("open directly");
    raw.execute("UPDATE teams SET people = -1 WHERE id = ?1", [TEAM])
        .expect("damage a team");
    assert!(matches!(store.teams(), Err(StoreError::Sqlite(_))));
    Ok(())
}

#[test]
fn corrupt_channel_management_metadata_is_not_read_as_permission_or_silently_defaulted() -> Result<(), StoreError> {
    use rusqlite::types::Value;
    for (column, restored) in [
        ("description", Value::Text(String::new())),
        ("archived", Value::Integer(0)),
        ("owner", Value::Integer(0)),
        ("can_edit", Value::Integer(0)),
        ("can_delete", Value::Integer(0)),
        ("channel_group", Value::Null),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = store_path(&dir);
        let mut store = Store::open(&path)?;
        let expected = teams();
        store.replace_teams(&expected, 1)?;
        let raw = rusqlite::Connection::open(path).expect("open directly");
        let update = format!("UPDATE team_channels SET {column} = ?1 WHERE id = ?2");
        assert!(
            raw.execute(&update, rusqlite::params![Value::Blob(vec![0, 255]), CHANNEL])
                .is_err()
        );
        // The released STRICT schema rejects such a write. Model a separately damaged local table to exercise
        // the reader's fallible decoding too; neither the released migration nor Store's connection is relaxed.
        raw.execute_batch(
            "CREATE TABLE damaged_channels AS SELECT * FROM team_channels;
             DROP TABLE team_channels;
             ALTER TABLE damaged_channels RENAME TO team_channels;",
        )
        .expect("rebuild only this disposable fixture's table without its constraints");
        raw.execute(&update, rusqlite::params![Value::Blob(vec![0, 255]), CHANNEL])
            .expect("damage only synthetic channel metadata");
        assert!(matches!(store.teams(), Err(StoreError::Sqlite(_))), "{column}");
        raw.execute(&update, rusqlite::params![restored, CHANNEL])
            .expect("repair the stored value");
        assert_eq!(store.teams()?, expected, "the next complete read recovers {column}");
    }
    Ok(())
}

#[test]
fn channel_metadata_diagnostics_leave_private_description_and_group_out() {
    let about = teams_store::ChannelAbout {
        description: "Synthetic confidential channel description".to_owned(),
        group: Some("synthetic-private-channel-group".to_owned()),
        archived: true,
        ..teams_store::ChannelAbout::default()
    };
    let debug = format!("{about:?}");
    assert!(debug.contains("ChannelAbout") && debug.contains("true"));
    assert!(!debug.contains(&about.description));
    assert!(!debug.contains(about.group.as_deref().unwrap()));
}

/// Whether `result` is SQLite's refusal.
fn refused<T>(result: &Result<T, StoreError>) -> bool {
    matches!(result, Err(StoreError::Sqlite(_)))
}

#[test]
fn the_tables_gone_answer_an_error_never_a_panic() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&store_path(&dir))?;
    store.replace_teams(&teams(), 1)?;
    store.replace_favorites(&[CHAT.to_owned()], 1)?;
    drop(store);
    let raw = rusqlite::Connection::open(store_path(&dir)).expect("open directly");
    raw.execute_batch(
        "DROP TABLE team_channels; DROP TABLE teams_read; DROP TABLE favorites_read; DROP TABLE reply_roots;",
    )
    .expect("drop the tables");
    drop(raw);

    // opened afresh, so nothing is read from a statement prepared before
    let mut store = Store::open(&store_path(&dir))?;
    assert!(refused(&store.teams()));
    assert!(refused(&store.teams_read_at()));
    assert!(refused(&store.replace_teams(&teams(), 2)));
    assert!(refused(&store.favorites()));
    assert!(refused(&store.replace_favorites(&[CHAT.to_owned()], 2)));
    assert!(refused(&store.set_reply_root(CHANNEL, "1001", "1001")));
    assert!(refused(&store.reply_roots(CHANNEL)));
    Ok(())
}

fn about(edit: impl FnOnce(&mut teams_store::TeamAbout)) -> teams_store::TeamAbout {
    let mut about = teams_store::TeamAbout::default();
    edit(&mut about);
    about
}

fn team_with(owner: bool, archived: bool, about: teams_store::TeamAbout) -> Team {
    Team {
        id: TEAM.to_owned(),
        name: "A made-up team".to_owned(),
        archived,
        owner,
        about,
        ..Team::default()
    }
}

#[test]
fn who_may_add_leave_and_administer_follows_the_account_and_the_team() {
    // a member of an ordinary team may add and leave, not administer
    let member = team_with(false, false, teams_store::TeamAbout::default());
    assert!(member.can_add_member() && member.can_leave() && !member.can_administer());
    // an owner may do all three
    let owner = team_with(true, false, teams_store::TeamAbout::default());
    assert!(owner.can_add_member() && owner.can_leave() && owner.can_administer());

    // an archived team takes no one, but may still be left
    let archived = team_with(true, true, teams_store::TeamAbout::default());
    assert!(!archived.can_add_member() && archived.can_leave());

    // a rule's people: nobody adds by hand or leaves
    let dynamic = team_with(true, false, about(|about| about.dynamic_membership = true));
    assert!(!dynamic.can_add_member() && !dynamic.can_leave() && dynamic.can_administer());

    // a guest adds nobody, but may leave
    let guest = team_with(false, false, about(|about| about.guest = true));
    assert!(!guest.can_add_member() && guest.can_leave());

    // on it only through channels: nothing of the team's own
    let channel_only = team_with(true, false, about(|about| about.channel_only = true));
    assert!(!channel_only.can_add_member() && !channel_only.can_leave() && !channel_only.can_administer());

    // an org-wide team: only an owner adds or leaves
    let org_member = team_with(false, false, about(|about| about.tenant_wide = true));
    assert!(!org_member.can_add_member() && !org_member.can_leave());
    let org_owner = team_with(true, false, about(|about| about.tenant_wide = true));
    assert!(org_owner.can_add_member() && org_owner.can_leave());
}

#[test]
fn every_channel_kind_and_what_a_team_shows_survive_the_store() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&store_path(&dir))?;
    let mut hidden = channel(CHANNEL, "A made-up hidden channel", false);
    hidden.shown = false;
    hidden.kind = ChannelKind::Shared;
    let mut private = channel("19:team0001private@thread.tacv2", "A made-up private channel", false);
    private.kind = ChannelKind::Private;
    let team = Team {
        channels: vec![channel(TEAM, "General", true), hidden, private],
        ..team_with(
            true,
            false,
            teams_store::TeamAbout {
                tenant: "00000000-0000-4000-8000-0000000000ff".to_owned(),
                description: "A made-up description".to_owned(),
                public: true,
                people: 7,
                guest: false,
                dynamic_membership: false,
                tenant_wide: false,
                channel_only: false,
                hidden: true,
                picture_etag: Some("made-up-etag".to_owned()),
                site: Some("https://made-up.example.invalid/sites/made-up".to_owned()),
            },
        )
    };
    assert_eq!(team.hidden_channels(), 1);
    store.replace_teams(std::slice::from_ref(&team), 1)?;
    let stored = store.teams()?;
    assert_eq!(stored, vec![team]);
    let kinds: Vec<_> = stored[0].channels.iter().map(|channel| channel.kind).collect();
    assert_eq!(
        kinds,
        [ChannelKind::Standard, ChannelKind::Shared, ChannelKind::Private]
    );

    // the keys round-trip; one nothing wrote reads as standard
    for kind in [ChannelKind::Standard, ChannelKind::Private, ChannelKind::Shared] {
        assert_eq!(ChannelKind::of_key(kind.key()), kind);
    }
    assert_eq!(ChannelKind::of_key("unknown"), ChannelKind::Standard);

    // `Debug` leaves the words out
    let shown = format!("{:?}", stored[0].about);
    assert!(shown.contains("people: 7") && shown.contains("hidden: true"), "{shown}");
    assert!(!shown.contains("made-up") && !shown.contains("0000000000ff"), "{shown}");
    Ok(())
}

#[test]
fn channel_management_metadata_survives_refresh_and_reopen() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = store_path(&dir);
    let mut store = Store::open(&path)?;
    let mut listed = teams();
    let channel = &mut listed[0].channels[1];
    channel.kind = ChannelKind::Private;
    channel.about = teams_store::ChannelAbout {
        description: "Synthetic private channel".to_owned(),
        archived: true,
        owner: true,
        can_edit: true,
        can_delete: false,
        group: Some("synthetic-group".to_owned()),
    };
    store.replace_teams(&listed, 10)?;
    drop(store);
    let mut store = Store::open(&path)?;
    assert_eq!(store.teams()?, listed);
    listed[0].channels[1].about.archived = false;
    store.replace_teams(&listed, 11)?;
    assert_eq!(store.teams()?, listed);
    Ok(())
}
