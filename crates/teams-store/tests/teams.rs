//! The account's teams and channels in the store, and each channel message's post (M2 Task 16), on a real file in a
//! temporary directory: the list replaced whole and told when it changed, kept across a reopen; a post recorded only
//! for a stored message and gone with it; and the migration's restart of the channels' syncs. Every id and name here
//! is made up.

use std::path::PathBuf;

use teams_store::{
    Applied, Conversation, ConversationKind, Message, SCHEMA_VERSION, Store, StoreError, SyncState, Team, TeamChannel,
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
    }
}

fn teams() -> Vec<Team> {
    vec![
        Team {
            id: TEAM.to_owned(),
            name: "A made-up team".to_owned(),
            archived: false,
            channels: vec![
                channel(TEAM, "General", true),
                channel(CHANNEL, "A made-up channel", false),
            ],
        },
        Team {
            id: OTHER_TEAM.to_owned(),
            name: "A made-up archived team".to_owned(),
            archived: true,
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
        gap_link: None,
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
        "DROP TABLE reply_roots; DROP TABLE team_channels; DROP TABLE teams; DROP TABLE teams_read;
         DROP TABLE favorites; DROP TABLE favorites_read; ALTER TABLE conversations DROP COLUMN empty;
         PRAGMA user_version = 8;",
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
        "DROP TABLE favorites; DROP TABLE favorites_read; ALTER TABLE conversations DROP COLUMN empty;
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
