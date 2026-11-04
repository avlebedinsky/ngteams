//! Catalog and search commands remain atomic on SQLite refusal, and can be retried on the same store.
//! Every identifier and message is synthetic; each test uses a private, real database.

use std::path::PathBuf;

use teams_store::{
    Applied, ChannelKind, Conversation, ConversationKind, IndexText, Message, Store, StoreError, Team, TeamChannel,
};

const CHAT: &str = "19:synthetic-catalog@thread.v2";

fn path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("catalog.sqlite")
}

fn open(dir: &tempfile::TempDir) -> Result<Store, StoreError> {
    let mut store = Store::open(&path(dir))?;
    store.upsert_conversation(&Conversation {
        id: CHAT.to_owned(),
        kind: ConversationKind::Group,
        topic: None,
        version: 1,
        last_activity: None,
    })?;
    Ok(store)
}

fn message(id: &str, version: i64, content: &str) -> Message {
    Message {
        conversation_id: CHAT.to_owned(),
        id: id.to_owned(),
        version,
        client_message_id: None,
        sender: None,
        sender_name: None,
        message_type: "Text".to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: 123,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

fn indexed(message: &Message) -> IndexText {
    IndexText {
        conversation_id: message.conversation_id.clone(),
        message_id: message.id.clone(),
        version: message.version,
        text: message.content.clone(),
    }
}

fn team(name: &str) -> Team {
    Team {
        id: "19:synthetic-team@thread.tacv2".to_owned(),
        name: name.to_owned(),
        archived: false,
        group_id: None,
        mailbox: None,
        owner: false,
        can_add_channel: false,
        can_add_private_channel: false,
        about: teams_store::TeamAbout::default(),
        channels: vec![TeamChannel {
            id: "19:synthetic-channel@thread.tacv2".to_owned(),
            name: name.to_owned(),
            general: true,
            kind: ChannelKind::Standard,
            about: teams_store::ChannelAbout::default(),
            shown: true,
        }],
    }
}

fn sqlite_refused<T: std::fmt::Debug>(result: &Result<T, StoreError>) {
    assert!(matches!(result, Err(StoreError::Sqlite(_))), "{result:?}");
}

#[test]
fn replacing_teams_rolls_back_deleted_catalog_and_inserted_team_on_channel_refusal() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir)?;
    let old = vec![team("Original")];
    let new = vec![team("Replacement")];
    store.replace_teams(&old, 10)?;
    let raw = rusqlite::Connection::open(path(&dir)).expect("raw database");
    raw.execute_batch(
        "CREATE TRIGGER refuse_channel BEFORE INSERT ON team_channels
         BEGIN SELECT RAISE(ABORT, 'synthetic refusal'); END;",
    )
    .expect("install refusal");
    sqlite_refused(&store.replace_teams(&new, 20));
    assert_eq!(store.teams()?, old);
    assert_eq!(store.teams_read_at()?, Some(10));
    raw.execute_batch("DROP TRIGGER refuse_channel")
        .expect("remove refusal");
    assert_eq!(store.replace_teams(&new, 20)?, Applied::Updated);
    drop(store);
    let store = Store::open(&path(&dir))?;
    assert_eq!(store.teams()?, new);
    assert_eq!(store.teams_read_at()?, Some(20));
    Ok(())
}

#[test]
fn a_failed_catalog_read_timestamp_rolls_back_the_complete_replacement() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir)?;
    store.replace_teams(&[team("Original")], 10)?;
    let raw = rusqlite::Connection::open(path(&dir)).expect("raw database");
    raw.execute_batch(
        "CREATE TRIGGER refuse_timestamp BEFORE UPDATE ON teams_read
         BEGIN SELECT RAISE(ABORT, 'synthetic refusal'); END;",
    )
    .expect("install refusal");
    sqlite_refused(&store.replace_teams(&[], 20));
    assert_eq!(store.teams()?, [team("Original")]);
    assert_eq!(store.teams_read_at()?, Some(10));
    raw.execute_batch("DROP TRIGGER refuse_timestamp")
        .expect("remove refusal");
    assert_eq!(store.replace_teams(&[], 20)?, Applied::Updated);
    assert_eq!(store.teams()?, []);
    assert_eq!(store.teams_read_at()?, Some(20));
    Ok(())
}

#[test]
fn replacing_favorites_rolls_back_earlier_rows_when_a_later_insert_is_refused() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir)?;
    let old = vec!["48:original".to_owned()];
    let new = vec!["48:first".to_owned(), "48:second".to_owned()];
    store.replace_favorites(&old, 10)?;
    let raw = rusqlite::Connection::open(path(&dir)).expect("raw database");
    raw.execute_batch(
        "CREATE TRIGGER refuse_second BEFORE INSERT ON favorites WHEN NEW.position = 1
         BEGIN SELECT RAISE(ABORT, 'synthetic refusal'); END;",
    )
    .expect("install refusal");
    sqlite_refused(&store.replace_favorites(&new, 20));
    assert_eq!(store.favorites()?, Some(old));
    let read_at: i64 = raw
        .query_row("SELECT read_at FROM favorites_read", [], |row| row.get(0))
        .expect("read time");
    assert_eq!(read_at, 10);
    raw.execute_batch("DROP TRIGGER refuse_second").expect("remove refusal");
    assert_eq!(store.replace_favorites(&new, 20)?, Applied::Updated);
    drop(store);
    assert_eq!(Store::open(&path(&dir))?.favorites()?, Some(new));
    Ok(())
}

#[test]
fn a_failed_first_favorites_read_stays_unknown_and_can_be_retried() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir)?;
    let listed = vec![CHAT.to_owned()];
    let raw = rusqlite::Connection::open(path(&dir)).expect("raw database");
    raw.execute_batch(
        "CREATE TRIGGER refuse_timestamp BEFORE INSERT ON favorites_read
         BEGIN SELECT RAISE(ABORT, 'synthetic refusal'); END;",
    )
    .expect("install refusal");
    sqlite_refused(&store.replace_favorites(&listed, 10));
    assert_eq!(store.favorites()?, None);
    let rows: i64 = raw
        .query_row("SELECT count(*) FROM favorites", [], |row| row.get(0))
        .expect("favorite rows");
    assert_eq!(rows, 0, "the successful row insertion was rolled back");
    raw.execute_batch("DROP TRIGGER refuse_timestamp")
        .expect("remove refusal");
    assert_eq!(store.replace_favorites(&listed, 20)?, Applied::Inserted);
    assert_eq!(store.favorites()?, Some(listed));
    Ok(())
}

#[test]
fn a_backfill_batch_restores_prior_search_text_when_a_later_entry_is_refused() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir)?;
    let first = message("1", 1, "original");
    let second = message("2", 1, "second");
    store.upsert_message_indexed(&first, None, |row| row.content.clone())?;
    store.upsert_message(&second, None)?;
    let fresh = message("1", 2, "replacement");
    store.upsert_message(&fresh, None)?;
    let batch = [indexed(&fresh), indexed(&second)];
    let raw = rusqlite::Connection::open(path(&dir)).expect("raw database");
    raw.execute_batch(
        "CREATE TRIGGER refuse_second BEFORE INSERT ON search_entries WHEN NEW.message_id = '2'
         BEGIN SELECT RAISE(ABORT, 'synthetic refusal'); END;",
    )
    .expect("install refusal");
    sqlite_refused(&store.index_messages(&batch));
    assert_eq!(store.search("original", 10)?.len(), 1);
    assert!(store.search("replacement", 10)?.is_empty());
    assert_eq!(store.messages_to_index(None, 10)?.len(), 2);
    raw.execute_batch("DROP TRIGGER refuse_second").expect("remove refusal");
    assert_eq!(store.index_messages(&batch)?, 2);
    drop(store);
    let store = Store::open(&path(&dir))?;
    assert!(store.search("original", 10)?.is_empty());
    assert_eq!(store.search("replacement", 10)?.len(), 1);
    assert_eq!(store.search("second", 10)?.len(), 1);
    assert!(store.messages_to_index(None, 10)?.is_empty());
    Ok(())
}

#[test]
fn an_index_refusal_rolls_back_the_message_edit_and_allows_a_retry() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir)?;
    let old = message("1", 1, "original");
    store.upsert_message_indexed(&old, None, |row| row.content.clone())?;
    let raw = rusqlite::Connection::open(path(&dir)).expect("raw database");
    raw.execute_batch(
        "CREATE TRIGGER refuse_version BEFORE UPDATE ON search_entries
         BEGIN SELECT RAISE(ABORT, 'synthetic refusal'); END;",
    )
    .expect("install refusal");
    let edit = message("1", 2, "replacement");
    sqlite_refused(&store.upsert_message_indexed(&edit, None, |row| row.content.clone()));
    assert_eq!(store.message(CHAT, "1")?, Some(old));
    assert_eq!(store.search("original", 10)?.len(), 1);
    assert!(store.search("replacement", 10)?.is_empty());
    raw.execute_batch("DROP TRIGGER refuse_version")
        .expect("remove refusal");
    assert_eq!(
        store.upsert_message_indexed(&edit, None, |row| row.content.clone())?,
        Applied::Updated
    );
    assert_eq!(store.message(CHAT, "1")?, Some(edit));
    assert!(store.search("original", 10)?.is_empty());
    assert_eq!(store.search("replacement", 10)?.len(), 1);
    Ok(())
}

#[test]
fn unicode_search_preserves_hit_metadata_and_empty_reindex_removes_only_searchability() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir)?;
    let entry = message("1", 1, "Café 東京");
    store.upsert_message_indexed(&entry, None, |row| row.content.clone())?;
    let hits = store.search_in("\"café\" 東京", &[CHAT.to_owned(), CHAT.to_owned()], usize::MAX)?;
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(
        (&hit.conversation_id, &hit.message_id, hit.arrival_time),
        (&CHAT.to_owned(), &entry.id, 123)
    );
    assert_eq!((hit.sender.as_ref(), hit.sender_name.as_ref()), (None, None));
    assert!(hit.snippet.contains('東'));
    assert!(!format!("{hit:?}").contains("Café"));
    assert!(store.search("café", 0)?.is_empty());
    let empty = IndexText {
        text: " \n\t".to_owned(),
        ..indexed(&entry)
    };
    assert_eq!(store.index_messages(&[empty])?, 1);
    assert!(store.search("café", 10)?.is_empty());
    assert!(store.messages_to_index(None, usize::MAX)?.is_empty());
    assert_eq!(store.message(CHAT, "1")?, Some(entry));
    Ok(())
}

#[test]
fn unavailable_catalog_storage_is_an_error_and_never_an_empty_replacement() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir)?;
    let original = vec![team("Original")];
    store.replace_teams(&original, 10)?;
    let raw = rusqlite::Connection::open(path(&dir)).expect("raw database");
    for table in ["teams_read", "teams", "team_channels"] {
        raw.execute_batch(&format!("ALTER TABLE {table} RENAME TO unavailable"))
            .expect("make storage unavailable");
        if table == "teams_read" {
            sqlite_refused(&store.teams_read_at());
        } else {
            sqlite_refused(&store.teams());
        }
        sqlite_refused(&store.replace_teams(&[], 20));
        raw.execute_batch(&format!("ALTER TABLE unavailable RENAME TO {table}"))
            .expect("restore storage");
        assert_eq!(store.teams()?, original);
        assert_eq!(store.teams_read_at()?, Some(10));
    }
    assert_eq!(store.replace_teams(&original, 30)?, Applied::Ignored);
    Ok(())
}

#[test]
fn unavailable_favorites_storage_never_looks_like_an_unknown_or_empty_list() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir)?;
    let original = vec![CHAT.to_owned()];
    store.replace_favorites(&original, 10)?;
    let raw = rusqlite::Connection::open(path(&dir)).expect("raw database");
    for table in ["favorites_read", "favorites"] {
        raw.execute_batch(&format!("ALTER TABLE {table} RENAME TO unavailable"))
            .expect("make storage unavailable");
        sqlite_refused(&store.favorites());
        sqlite_refused(&store.replace_favorites(&[], 20));
        raw.execute_batch(&format!("ALTER TABLE unavailable RENAME TO {table}"))
            .expect("restore storage");
        assert_eq!(store.favorites()?, Some(original.clone()));
    }
    assert_eq!(store.replace_favorites(&original, 30)?, Applied::Ignored);
    Ok(())
}

#[test]
fn unavailable_search_storage_reports_failure_and_recovers_without_losing_the_index() -> Result<(), StoreError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir)?;
    let entry = message("1", 1, "searchable");
    store.upsert_message_indexed(&entry, None, |row| row.content.clone())?;
    let raw = rusqlite::Connection::open(path(&dir)).expect("raw database");
    raw.execute_batch("ALTER TABLE search_entries RENAME TO unavailable")
        .expect("make storage unavailable");
    sqlite_refused(&store.search("searchable", 10));
    sqlite_refused(&store.search_in("searchable", &[CHAT.to_owned()], 10));
    sqlite_refused(&store.messages_to_index(None, 10));
    sqlite_refused(&store.index_messages(&[indexed(&entry)]));
    raw.execute_batch("ALTER TABLE unavailable RENAME TO search_entries")
        .expect("restore storage");
    assert_eq!(store.search("searchable", 10)?.len(), 1);
    assert!(store.messages_to_index(None, 10)?.is_empty());
    Ok(())
}
