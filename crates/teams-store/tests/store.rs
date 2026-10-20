//! The store on a real file in a temporary directory: migrations, versioned upserts, reactions, read horizons, sync
//! state, the outbox and the queries the core reads with. Every id and MRI here is made up.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use teams_store::{
    Applied, AttemptOutcome, CheckFailed, Conversation, ConversationKind, Member, Message, NewOutboxEntry, OutboxKind,
    OutboxState, ReactionChange, ReadHorizon, SCHEMA_VERSION, Store, StoreError, SyncState,
};

const ME: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const PEER: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const THIRD: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const CHAT: &str = "19:synthetic-ab@unq.gbl.spaces";
const GROUP: &str = "19:synthetic-abc@thread.v2";

fn store_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("store").join("a.sqlite")
}

fn open(dir: &tempfile::TempDir) -> Result<Store, StoreError> {
    Store::open(&store_path(dir))
}

fn conversation(id: &str, kind: ConversationKind, version: i64) -> Conversation {
    Conversation {
        id: id.to_owned(),
        kind,
        topic: None,
        version,
        last_activity: None,
    }
}

fn message(id: &str, version: i64, content: &str, arrival: i64, sender: &str) -> Message {
    Message {
        conversation_id: CHAT.to_owned(),
        id: id.to_owned(),
        version,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: Some("Synthetic".to_owned()),
        message_type: "RichText/Html".to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: arrival,
        compose_time: Some(arrival),
        edit_time: None,
        delete_time: None,
    }
}

/// A new store holding the 1:1 chat.
fn with_chat(dir: &tempfile::TempDir) -> Result<Store, StoreError> {
    let mut store = open(dir)?;
    store.upsert_conversation(&conversation(CHAT, ConversationKind::OneToOne, 1))?;
    Ok(store)
}

fn mode(path: &Path) -> std::io::Result<u32> {
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
}

// --- opening and migrations ---

#[test]
fn a_new_store_is_migrated_to_the_latest_schema_in_wal_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).expect("open the store");
    assert_eq!(store.schema_version().expect("schema version"), SCHEMA_VERSION);
    // WAL is kept in the file: a second connection sees it
    let raw = rusqlite::Connection::open(store_path(&dir)).expect("open directly");
    let mode: String = raw
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .expect("journal mode");
    assert_eq!(mode, "wal");
    drop(store);
}

/// Opens the store at `path` from `count` threads at once (each a store of its own, as a second process would open).
fn open_at_once(path: &Path, count: usize) -> Vec<Result<Store, StoreError>> {
    let barrier = std::sync::Barrier::new(count);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..count)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    Store::open(path)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)))
            .collect()
    })
}

/// Opens the store at `path` from 4 threads at once: every one must open it, at the latest schema, as a run of its own.
fn four_open_at_once(path: &Path) -> Result<(), StoreError> {
    let mut runs = Vec::new();
    for store in open_at_once(path, 4) {
        let store = store?;
        assert_eq!(store.schema_version()?, SCHEMA_VERSION);
        runs.push(store.run_id());
    }
    runs.sort_unstable();
    runs.dedup();
    assert_eq!(runs.len(), 4, "a run of its own each");
    Ok(())
}

#[test]
fn two_processes_opening_a_new_store_at_once_both_open_it() {
    // SQLite can't wait while it turns a new file into WAL: without the open lock one of them gets SQLITE_BUSY
    let dir = tempfile::tempdir().expect("tempdir");
    let path = store_path(&dir);
    for _ in 0..8 {
        if let Some(parent) = path.parent() {
            drop(std::fs::remove_dir_all(parent));
        }
        four_open_at_once(&path).expect("every one opens the new store");
    }
}

#[test]
fn opening_an_existing_file_not_in_wal_mode_at_once_all_open_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = store_path(&dir);
    for _ in 0..8 {
        if let Some(parent) = path.parent() {
            drop(std::fs::remove_dir_all(parent));
            std::fs::create_dir_all(parent).expect("dir");
        }
        // an empty file, as a rollback journal store starts
        std::fs::File::create(&path).expect("an empty file");
        four_open_at_once(&path).expect("every one opens the file");
    }
}

#[test]
fn opening_an_existing_store_at_once_all_open_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = store_path(&dir);
    drop(Store::open(&path).expect("open the store"));
    for _ in 0..8 {
        four_open_at_once(&path).expect("every one opens the store");
    }
}

/// The POSIX locks this process holds on the file at `path`, from `/proc/locks` (SQLite locks with POSIX locks).
fn posix_locks_held_on(path: &Path) -> std::io::Result<usize> {
    use std::os::unix::fs::MetadataExt;
    let inode = std::fs::metadata(path)?.ino().to_string();
    let pid = std::process::id().to_string();
    let locks = std::fs::read_to_string("/proc/locks")?;
    Ok(locks
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .filter(|fields| !fields.contains(&"->") && fields.contains(&"POSIX") && fields.contains(&pid.as_str()))
        .filter(|fields| {
            fields
                .iter()
                .any(|field| field.matches(':').count() == 2 && field.rsplit(':').next() == Some(inode.as_str()))
        })
        .count())
}

#[test]
fn opening_a_second_store_in_the_process_leaves_the_first_ones_sqlite_locks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = store_path(&dir);
    let shm = PathBuf::from(format!("{}-shm", path.display()));
    let held = || -> std::io::Result<(usize, usize)> { Ok((posix_locks_held_on(&path)?, posix_locks_held_on(&shm)?)) };
    let first = Store::open(&path).expect("open the store");
    // a WAL connection keeps a read lock on the file and its hold on the -shm while it is open
    let before = held().expect("locks");
    assert!(before.0 > 0 && before.1 > 0, "{before:?}");
    let second = Store::open(&path).expect("open the store again");
    assert_eq!(
        held().expect("locks"),
        before,
        "a second open closes no descriptor of the store's files"
    );
    drop(second);
    assert_eq!(held().expect("locks"), before);
    drop(first);
}

#[test]
fn reopening_keeps_the_data_and_runs_no_migration_twice() {
    let dir = tempfile::tempdir().expect("tempdir");
    with_chat(&dir).expect("a store with the chat");
    let store = open(&dir).expect("open the store");
    assert_eq!(store.schema_version().expect("schema version"), SCHEMA_VERSION);
    assert!(store.conversation(CHAT).expect("read").is_some());
}

#[test]
fn a_store_from_a_newer_build_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    drop(open(&dir).expect("open the store"));
    let raw = rusqlite::Connection::open(store_path(&dir)).expect("open directly");
    raw.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .expect("bump the schema version");
    drop(raw);
    let error = Store::open(&store_path(&dir)).expect_err("refused");
    assert!(matches!(
        error,
        StoreError::SchemaTooNew { found, supported } if found == SCHEMA_VERSION + 1 && supported == SCHEMA_VERSION
    ));
}

#[test]
fn the_store_file_and_its_wal_are_private_to_the_user() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chat(&dir).expect("a store with the chat");
    store
        .upsert_message(&message("1000", 1000, "hi", 1000, PEER), None)
        .expect("write");
    let path = store_path(&dir);
    assert_eq!(mode(&path).expect("mode"), 0o600);
    assert_eq!(mode(path.parent().expect("parent")).expect("mode"), 0o700);
    // the store is still open, so SQLite keeps both side files
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", path.display()));
        assert_eq!(mode(&side).expect("mode"), 0o600, "{suffix}");
    }
    drop(store);
}

#[test]
fn an_existing_file_readable_by_others_is_tightened() {
    let dir = tempfile::tempdir().expect("tempdir");
    drop(open(&dir).expect("open the store"));
    let path = store_path(&dir);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("loosen");
    drop(open(&dir).expect("open the store"));
    assert_eq!(mode(&path).expect("mode"), 0o600);
}

#[test]
fn a_directory_at_the_store_path_is_refused_and_keeps_its_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = store_path(&dir);
    std::fs::create_dir_all(&path).expect("a directory at the store path");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("mode");
    let refused = open(&dir).expect_err("a directory is not a store");
    assert!(matches!(refused, StoreError::File { .. }), "{refused:?}");
    assert_eq!(mode(&path).expect("mode"), 0o755);
}

#[test]
fn a_link_to_a_directory_at_the_store_path_is_refused_and_its_target_keeps_its_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = store_path(&dir);
    let target = dir.path().join("elsewhere");
    std::fs::create_dir(&target).expect("the link's target");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).expect("mode");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("the store's directory");
    std::os::unix::fs::symlink(&target, &path).expect("a link at the store path");
    let refused = open(&dir).expect_err("a directory is not a store");
    assert!(matches!(refused, StoreError::File { .. }), "{refused:?}");
    assert_eq!(mode(&target).expect("mode"), 0o755);
}

// --- conversations and members ---

#[test]
fn a_conversation_upsert_keeps_the_newest_version() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir).expect("open the store");
    let mut chat = conversation(GROUP, ConversationKind::Group, 10);
    chat.topic = Some("new".to_owned());
    assert_eq!(store.upsert_conversation(&chat).expect("insert"), Applied::Inserted);
    assert_eq!(store.upsert_conversation(&chat).expect("same again"), Applied::Ignored);
    let mut older = chat.clone();
    older.version = 9;
    older.topic = Some("old".to_owned());
    assert_eq!(store.upsert_conversation(&older).expect("older"), Applied::Ignored);
    assert_eq!(store.conversation(GROUP).expect("read"), Some(chat.clone()));
    let mut newer = chat;
    newer.version = 11;
    newer.topic = Some("newer".to_owned());
    assert_eq!(store.upsert_conversation(&newer).expect("newer"), Applied::Updated);
    assert_eq!(store.conversation(GROUP).expect("read"), Some(newer));
}

#[test]
fn a_later_last_activity_is_taken_whatever_the_version() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir).expect("open the store");
    let mut chat = conversation(GROUP, ConversationKind::Group, 10);
    chat.topic = Some("kept".to_owned());
    chat.last_activity = Some(1_000);
    store.upsert_conversation(&chat).expect("insert");
    let mut same = chat.clone();
    same.topic = Some("not taken".to_owned());
    same.last_activity = Some(2_000);
    assert_eq!(
        store.upsert_conversation(&same).expect("same version"),
        Applied::Ignored
    );
    let mut older = chat.clone();
    older.version = 9;
    older.last_activity = Some(1_500);
    assert_eq!(
        store.upsert_conversation(&older).expect("older version"),
        Applied::Ignored
    );
    older.last_activity = None;
    store.upsert_conversation(&older).expect("no activity");
    let stored = store.conversation(GROUP).expect("read").expect("group");
    assert_eq!(
        (stored.version, stored.topic.as_deref(), stored.last_activity),
        (10, Some("kept"), Some(2_000))
    );
}

#[test]
fn an_unknown_conversation_kind_is_kept_as_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir).expect("open the store");
    let chat = conversation(GROUP, ConversationKind::Other("sfbinteropchat".to_owned()), 1);
    store.upsert_conversation(&chat).expect("insert");
    assert_eq!(store.conversation(GROUP).expect("read"), Some(chat));
    for kind in [
        ConversationKind::OneToOne,
        ConversationKind::Group,
        ConversationKind::Meeting,
        ConversationKind::Channel,
    ] {
        assert_eq!(ConversationKind::from_text(kind.as_str()), kind);
    }
}

#[test]
fn members_are_replaced_only_by_a_newer_roster() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir).expect("open the store");
    store
        .upsert_conversation(&conversation(GROUP, ConversationKind::Group, 1))
        .expect("chat");
    let two = [Member::new(ME, Some("Admin")), Member::new(PEER, Some("User"))];
    assert_eq!(
        store.replace_members(GROUP, 5, &two).expect("first roster"),
        Applied::Inserted
    );
    assert_eq!(
        store.replace_members(GROUP, 5, &two).expect("same roster"),
        Applied::Ignored
    );
    let three = [
        Member::new(ME, Some("Admin")),
        Member::new(PEER, Some("User")),
        Member::new(THIRD, None),
    ];
    assert_eq!(
        store.replace_members(GROUP, 4, &three).expect("older roster"),
        Applied::Ignored
    );
    assert_eq!(store.members(GROUP).expect("members").len(), 2);
    assert_eq!(
        store.replace_members(GROUP, 6, &three).expect("newer roster"),
        Applied::Updated
    );
    assert_eq!(store.members(GROUP).expect("members"), three.to_vec());
    assert_eq!(store.roster_version(GROUP).expect("roster version"), Some(6));
    let one = [Member::new(ME, Some("Admin"))];
    store.replace_members(GROUP, 7, &one).expect("someone left");
    assert_eq!(store.members(GROUP).expect("members"), one.to_vec());
}

#[test]
fn members_of_an_unknown_conversation_are_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir).expect("open the store");
    let error = store
        .replace_members(GROUP, 1, &[Member::new(ME, None)])
        .expect_err("unknown");
    assert!(matches!(error, StoreError::UnknownConversation(id) if id == GROUP));
}

#[test]
fn the_conversation_list_is_ordered_by_last_activity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chat(&dir).expect("a store with the chat");
    let mut group = conversation(GROUP, ConversationKind::Group, 1);
    group.last_activity = Some(5_000);
    store.upsert_conversation(&group).expect("group");
    store
        .upsert_conversation(&conversation(
            "19:synthetic-quiet@thread.v2",
            ConversationKind::Group,
            1,
        ))
        .expect("quiet");
    let ids = |store: &Store| -> Vec<String> {
        store
            .conversations_by_activity(10)
            .expect("list")
            .into_iter()
            .map(|c| c.id)
            .collect()
    };
    assert_eq!(ids(&store), [GROUP, CHAT, "19:synthetic-quiet@thread.v2"]);
    store
        .upsert_message(&message("6000", 6000, "later", 6000, PEER), None)
        .expect("message");
    assert_eq!(ids(&store), [CHAT, GROUP, "19:synthetic-quiet@thread.v2"]);
    assert_eq!(
        store.conversation(CHAT).expect("read").expect("chat").last_activity,
        Some(6000)
    );
    // an older conversation version can't move the activity back
    let mut stale = conversation(CHAT, ConversationKind::OneToOne, 2);
    stale.last_activity = Some(1);
    store
        .upsert_conversation(&stale)
        .expect("newer version, older activity");
    assert_eq!(
        store.conversation(CHAT).expect("read").expect("chat").last_activity,
        Some(6000)
    );
    assert_eq!(store.conversations_by_activity(1).expect("limited").len(), 1);
}

// --- messages: versions, edits, soft deletes ---

#[test]
fn a_message_upsert_keeps_the_newest_version() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chat(&dir).expect("a store with the chat");
    let first = message("1000", 1000, "first", 1000, PEER);
    assert_eq!(store.upsert_message(&first, None).expect("insert"), Applied::Inserted);
    assert_eq!(store.upsert_message(&first, None).expect("again"), Applied::Ignored);
    let mut edited = first.clone();
    edited.version = 2000;
    edited.content = "edited".to_owned();
    edited.edit_time = Some(2000);
    assert_eq!(store.upsert_message(&edited, None).expect("edit"), Applied::Updated);
    assert_eq!(
        store.upsert_message(&first, None).expect("the older version late"),
        Applied::Ignored
    );
    assert_eq!(store.message(CHAT, "1000").expect("read"), Some(edited));
}

#[test]
fn a_soft_deleted_message_keeps_its_row_but_no_content() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chat(&dir).expect("a store with the chat");
    let mut original = message("1000", 1000, "secret", 1000, PEER);
    original.properties = Some(r#"{"mentions":"[]"}"#.to_owned());
    store.upsert_message(&original, None).expect("insert");
    store
        .apply_reaction(&ReactionChange::add(CHAT, "1000", "like", ME, 1500))
        .expect("reaction");
    let mut deleted = original.clone();
    deleted.version = 3000;
    deleted.delete_time = Some(3000);
    assert_eq!(store.upsert_message(&deleted, None).expect("delete"), Applied::Updated);
    let stored = store.message(CHAT, "1000").expect("read").expect("row kept");
    assert_eq!(stored.delete_time, Some(3000));
    assert_eq!(stored.content, "");
    assert_eq!(stored.properties, None);
    assert!(store.reactions(CHAT, "1000").expect("reactions").is_empty());
    assert_eq!(
        store.upsert_message(&original, None).expect("the older version late"),
        Applied::Ignored
    );
    assert_eq!(store.message(CHAT, "1000").expect("read").expect("row").content, "");
}

#[test]
fn a_message_in_an_unknown_conversation_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir).expect("open the store");
    let error = store
        .upsert_message(&message("1", 1, "x", 1, PEER), None)
        .expect_err("unknown");
    assert!(matches!(error, StoreError::UnknownConversation(id) if id == CHAT));
}

#[test]
fn our_own_message_is_found_by_its_client_message_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chat(&dir).expect("a store with the chat");
    let mut ours = message("1000", 1000, "mine", 1000, ME);
    ours.client_message_id = Some("4711".to_owned());
    store.upsert_message(&ours, None).expect("insert");
    // the same message back from the other path (sync after Trouter): one row
    store.upsert_message(&ours, None).expect("again");
    let found = store
        .message_by_client_id(CHAT, "4711")
        .expect("lookup")
        .expect("found");
    assert_eq!(found.id, "1000");
    assert_eq!(store.message_by_client_id(CHAT, "4712").expect("lookup"), None);
    assert_eq!(store.messages_before(CHAT, None, 10).expect("page").messages.len(), 1);
}

// --- reactions ---

#[test]
fn reactions_are_kept_per_user_and_the_newest_change_wins() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chat(&dir).expect("a store with the chat");
    store
        .upsert_message(&message("1000", 1000, "hi", 1000, PEER), None)
        .expect("message");
    let like_me = ReactionChange::add(CHAT, "1000", "like", ME, 2000);
    assert_eq!(store.apply_reaction(&like_me).expect("add"), Applied::Inserted);
    assert_eq!(store.apply_reaction(&like_me).expect("again"), Applied::Ignored);
    store
        .apply_reaction(&ReactionChange::add(CHAT, "1000", "like", PEER, 2100))
        .expect("peer");
    store
        .apply_reaction(&ReactionChange::add(CHAT, "1000", "heart", PEER, 2200))
        .expect("heart");
    assert_eq!(store.reactions(CHAT, "1000").expect("read").len(), 3);

    let removed = ReactionChange::remove(CHAT, "1000", "like", ME, 3000);
    assert_eq!(store.apply_reaction(&removed).expect("remove"), Applied::Updated);
    // the add arriving late can't bring it back
    assert_eq!(store.apply_reaction(&like_me).expect("late add"), Applied::Ignored);
    let left: Vec<(String, String)> = store
        .reactions(CHAT, "1000")
        .expect("read")
        .into_iter()
        .map(|r| (r.key, r.user))
        .collect();
    assert_eq!(
        left,
        [
            ("heart".to_owned(), PEER.to_owned()),
            ("like".to_owned(), PEER.to_owned())
        ]
    );
}

#[test]
fn a_newer_message_version_with_its_reactions_replaces_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chat(&dir).expect("a store with the chat");
    let first = message("1000", 1000, "hi", 1000, PEER);
    let set = [teams_store::Reaction {
        key: "like".to_owned(),
        user: ME.to_owned(),
        time: 1100,
    }];
    store.upsert_message(&first, Some(&set)).expect("with reactions");
    assert_eq!(store.reactions(CHAT, "1000").expect("read"), set.to_vec());
    let mut newer = first.clone();
    newer.version = 1200;
    let replaced = [teams_store::Reaction {
        key: "laugh".to_owned(),
        user: PEER.to_owned(),
        time: 1150,
    }];
    store.upsert_message(&newer, Some(&replaced)).expect("newer");
    assert_eq!(store.reactions(CHAT, "1000").expect("read"), replaced.to_vec());
    // a stale version's set is ignored with it
    store.upsert_message(&first, Some(&set)).expect("stale");
    assert_eq!(store.reactions(CHAT, "1000").expect("read"), replaced.to_vec());
    // a newer version without a set leaves the reactions alone
    let mut text_only = newer;
    text_only.version = 1300;
    store.upsert_message(&text_only, None).expect("no set");
    assert_eq!(store.reactions(CHAT, "1000").expect("read"), replaced.to_vec());
}

fn reaction(key: &str, user: &str, time: i64) -> teams_store::Reaction {
    teams_store::Reaction {
        key: key.to_owned(),
        user: user.to_owned(),
        time,
    }
}

fn keys_and_users(store: &Store, message_id: &str) -> Result<Vec<(String, String)>, StoreError> {
    Ok(store
        .reactions(CHAT, message_id)?
        .into_iter()
        .map(|r| (r.key, r.user))
        .collect())
}

#[test]
fn a_removed_reaction_stays_removed_after_a_set_leaves_it_out_and_its_add_arrives_late() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chat(&dir).expect("a store with the chat");
    let first = message("1000", 1000, "hi", 1000, PEER);
    store.upsert_message(&first, None).expect("message");
    let late_add = ReactionChange::add(CHAT, "1000", "like", ME, 1100);
    store.apply_reaction(&late_add).expect("add");
    store
        .apply_reaction(&ReactionChange::remove(CHAT, "1000", "like", ME, 1200))
        .expect("remove");
    let mut newer = first;
    newer.version = 1300;
    store
        .upsert_message(&newer, Some(&[reaction("heart", PEER, 1250)]))
        .expect("a newer version without our like");
    assert_eq!(store.apply_reaction(&late_add).expect("late add"), Applied::Ignored);
    assert_eq!(
        keys_and_users(&store, "1000").expect("read"),
        [("heart".to_owned(), PEER.to_owned())]
    );
}

#[test]
fn a_reaction_a_newer_set_dropped_isnt_brought_back_by_its_late_add() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chat(&dir).expect("a store with the chat");
    let first = message("1000", 1000, "hi", 1000, PEER);
    store
        .upsert_message(&first, Some(&[reaction("like", ME, 1100)]))
        .expect("with our like");
    let mut newer = first;
    newer.version = 1200;
    store.upsert_message(&newer, Some(&[])).expect("like dropped");
    assert!(store.reactions(CHAT, "1000").expect("read").is_empty());
    let late = ReactionChange::add(CHAT, "1000", "like", ME, 1100);
    assert_eq!(store.apply_reaction(&late).expect("late add"), Applied::Ignored);
    // a user the set never named, whose add predates it, stays out too
    let unseen = ReactionChange::add(CHAT, "1000", "laugh", THIRD, 1150);
    assert_eq!(store.apply_reaction(&unseen).expect("unseen add"), Applied::Ignored);
    assert!(store.reactions(CHAT, "1000").expect("read").is_empty());
    // a change after the set does count
    let after = ReactionChange::add(CHAT, "1000", "laugh", THIRD, 1250);
    assert_eq!(store.apply_reaction(&after).expect("add after"), Applied::Inserted);
    assert_eq!(
        keys_and_users(&store, "1000").expect("read"),
        [("laugh".to_owned(), THIRD.to_owned())]
    );
}

#[test]
fn a_set_and_per_user_changes_leave_the_same_reactions_in_either_order() {
    let first = message("1000", 1000, "hi", 1000, PEER);
    let mut snapshot = first.clone();
    snapshot.version = 1250;
    // the set was read at 1250: our like (1100) is in it, the peer's heart (1300) came after it
    let set = [reaction("like", ME, 1100)];
    let changes = [
        ReactionChange::add(CHAT, "1000", "like", ME, 1100),
        ReactionChange::add(CHAT, "1000", "heart", PEER, 1300),
        ReactionChange::add(CHAT, "1000", "laugh", THIRD, 1150),
        ReactionChange::remove(CHAT, "1000", "laugh", THIRD, 1200),
    ];
    let expected = [
        ("heart".to_owned(), PEER.to_owned()),
        ("like".to_owned(), ME.to_owned()),
    ];
    for set_first in [true, false] {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = with_chat(&dir).expect("a store with the chat");
        store.upsert_message(&first, None).expect("message");
        if set_first {
            store.upsert_message(&snapshot, Some(&set)).expect("set");
        }
        for change in &changes {
            store.apply_reaction(change).expect("change");
        }
        if !set_first {
            store.upsert_message(&snapshot, Some(&set)).expect("set");
        }
        assert_eq!(
            keys_and_users(&store, "1000").expect("read"),
            expected,
            "set first: {set_first}"
        );
    }
}

#[test]
fn a_reaction_on_an_unknown_message_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chat(&dir).expect("a store with the chat");
    let error = store
        .apply_reaction(&ReactionChange::add(CHAT, "404", "like", ME, 1))
        .expect_err("unknown");
    assert!(matches!(error, StoreError::UnknownMessage { message, .. } if message == "404"));
}

// --- pages and unread counts ---

#[test]
fn messages_come_in_pages_newest_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chat(&dir).expect("a store with the chat");
    for n in 1..=5 {
        let id = (n * 1000).to_string();
        store
            .upsert_message(&message(&id, n * 1000, &id, n * 1000, PEER), None)
            .expect("insert");
    }
    let first = store.messages_before(CHAT, None, 2).expect("first page");
    let ids = |page: &teams_store::MessagePage| page.messages.iter().map(|m| m.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&first), ["5000", "4000"]);
    let second = store
        .messages_before(CHAT, first.next.as_ref(), 2)
        .expect("second page");
    assert_eq!(ids(&second), ["3000", "2000"]);
    let last = store.messages_before(CHAT, second.next.as_ref(), 2).expect("last page");
    assert_eq!(ids(&last), ["1000"]);
    assert_eq!(last.next, None);
    let exact = store.messages_before(CHAT, None, 5).expect("all at once");
    assert_eq!(exact.messages.len(), 5);
    assert_eq!(exact.next, None);
}

#[test]
fn messages_with_the_same_arrival_time_are_paged_without_loss() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chat(&dir).expect("a store with the chat");
    for id in ["1000", "1001", "1002"] {
        store
            .upsert_message(&message(id, 1, id, 500, PEER), None)
            .expect("insert");
    }
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = store.messages_before(CHAT, cursor.as_ref(), 1).expect("page");
        seen.extend(page.messages.into_iter().map(|m| m.id));
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen, ["1002", "1001", "1000"]);
}

#[test]
fn the_unread_count_follows_our_read_horizon() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chat(&dir).expect("a store with the chat");
    store
        .upsert_message(&message("1000", 1000, "a", 1000, PEER), None)
        .expect("1");
    store
        .upsert_message(&message("2000", 2000, "b", 2000, PEER), None)
        .expect("2");
    store
        .upsert_message(&message("2500", 2500, "mine", 2500, ME), None)
        .expect("ours");
    let mut control = message("2600", 2600, "", 2600, PEER);
    control.message_type = "ThreadActivity/AddMember".to_owned();
    store.upsert_message(&control, None).expect("event");
    let mut gone = message("2700", 2700, "x", 2700, PEER);
    gone.delete_time = Some(2800);
    store.upsert_message(&gone, None).expect("deleted");
    store
        .upsert_message(&message("3000", 3000, "c", 3000, PEER), None)
        .expect("3");
    assert_eq!(store.unread_count(CHAT, ME).expect("no horizon"), 3);

    let horizon = ReadHorizon {
        conversation_id: CHAT.to_owned(),
        user: ME.to_owned(),
        last_read_arrival: 2000,
        updated: 10_000,
        raw: Some("2000;10000;0".to_owned()),
    };
    assert_eq!(store.set_read_horizon(&horizon).expect("horizon"), Applied::Inserted);
    assert_eq!(store.unread_count(CHAT, ME).expect("after horizon"), 1);
    // the peer's own horizon doesn't change our count
    let peer = ReadHorizon {
        user: PEER.to_owned(),
        last_read_arrival: 3000,
        ..horizon.clone()
    };
    store.set_read_horizon(&peer).expect("peer horizon");
    assert_eq!(store.unread_count(CHAT, ME).expect("unchanged"), 1);
    assert_eq!(store.read_horizon(CHAT, PEER).expect("read"), Some(peer));
}

#[test]
fn a_read_horizon_set_later_wins_even_when_it_moves_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chat(&dir).expect("a store with the chat");
    let read = ReadHorizon {
        conversation_id: CHAT.to_owned(),
        user: ME.to_owned(),
        last_read_arrival: 3000,
        updated: 10_000,
        raw: None,
    };
    store.set_read_horizon(&read).expect("read up to 3000");
    let unread = ReadHorizon {
        last_read_arrival: 1000,
        updated: 11_000,
        ..read.clone()
    };
    assert_eq!(
        store.set_read_horizon(&unread).expect("marked unread"),
        Applied::Updated
    );
    assert_eq!(
        store.set_read_horizon(&read).expect("the older set late"),
        Applied::Ignored
    );
    assert_eq!(store.read_horizon(CHAT, ME).expect("read"), Some(unread));
}

// --- sync state ---

#[test]
fn sync_state_is_kept_per_conversation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = with_chat(&dir).expect("a store with the chat");
    store
        .upsert_conversation(&conversation(GROUP, ConversationKind::Group, 1))
        .expect("group");
    assert_eq!(store.sync_state(CHAT).expect("none yet"), None);
    let state = SyncState {
        conversation_id: CHAT.to_owned(),
        sync_link: Some("https://chat.example.invalid/v1/sync?syncState=one".to_owned()),
        backward_link: Some("https://chat.example.invalid/v1/back?page=2".to_owned()),
        history_complete: false,
        updated: 100,
    };
    store.set_sync_state(&state).expect("set");
    let done = SyncState {
        backward_link: None,
        history_complete: true,
        updated: 200,
        ..state.clone()
    };
    store.set_sync_state(&done).expect("replace");
    assert_eq!(store.sync_state(CHAT).expect("read"), Some(done));
    assert_eq!(store.sync_state(GROUP).expect("other chat"), None);
    let error = store
        .set_sync_state(&SyncState {
            conversation_id: "19:unknown@thread.v2".to_owned(),
            ..state
        })
        .expect_err("unknown");
    assert!(matches!(error, StoreError::UnknownConversation(_)));
}

#[test]
fn the_conversation_list_sync_link_is_kept() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir).expect("open the store");
    assert_eq!(store.conversation_list_sync_link().expect("none yet"), None);
    store
        .set_conversation_list_sync_link(Some("https://chat.example.invalid/v1/list?syncState=x"), 1)
        .expect("set");
    store
        .set_conversation_list_sync_link(Some("https://chat.example.invalid/v1/list?syncState=y"), 2)
        .expect("again");
    assert_eq!(
        store.conversation_list_sync_link().expect("read").as_deref(),
        Some("https://chat.example.invalid/v1/list?syncState=y")
    );
    store.set_conversation_list_sync_link(None, 3).expect("cleared");
    assert_eq!(store.conversation_list_sync_link().expect("read"), None);
}

// --- the outbox ---

fn send(cmid: &str) -> NewOutboxEntry {
    NewOutboxEntry {
        conversation_id: Some(CHAT.to_owned()),
        kind: OutboxKind::Send,
        client_message_id: cmid.to_owned(),
        payload: r#"{"text":"synthetic"}"#.to_owned(),
    }
}

#[test]
fn a_command_becomes_a_pending_row_of_this_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir).expect("open the store");
    let entry = store.enqueue(&send("1001")).expect("enqueue");
    assert_eq!(entry.state, OutboxState::Pending);
    assert_eq!(entry.attempts, 0);
    assert_eq!(entry.run_id, store.run_id());
    assert_eq!(store.outbox_due(0).expect("due"), std::slice::from_ref(&entry));
    assert_eq!(store.outbox_by_client_message_id("1001").expect("lookup"), Some(entry));
    let error = store.enqueue(&send("1001")).expect_err("duplicate");
    assert!(matches!(error, StoreError::DuplicateClientMessageId(id) if id == "1001"));
}

#[test]
fn every_outbox_kind_round_trips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir).expect("open the store");
    let kinds = [
        OutboxKind::Send,
        OutboxKind::Edit,
        OutboxKind::Delete,
        OutboxKind::AddReaction,
        OutboxKind::RemoveReaction,
        OutboxKind::ReadReceipt,
        OutboxKind::CreateChat,
    ];
    for (n, kind) in kinds.into_iter().enumerate() {
        let entry = NewOutboxEntry {
            kind,
            conversation_id: None,
            ..send(&n.to_string())
        };
        assert_eq!(store.enqueue(&entry).expect("enqueue").kind, kind);
    }
}

#[test]
fn an_attempt_goes_pending_sending_sent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir).expect("open the store");
    let id = store.enqueue(&send("1001")).expect("enqueue").id;
    let sending = store.begin_attempt(id).expect("begin");
    assert_eq!((sending.state, sending.attempts), (OutboxState::Sending, 1));
    assert!(store.outbox_due(0).expect("due").is_empty());
    let sent = store
        .finish_attempt(
            id,
            &AttemptOutcome::Sent {
                message_id: Some("1000".to_owned()),
            },
        )
        .expect("sent");
    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(sent.message_id.as_deref(), Some("1000"));
    let error = store.begin_attempt(id).expect_err("a sent row is never sent again");
    assert!(matches!(
        error,
        StoreError::OutboxNotSendable {
            state: OutboxState::Sent,
            ..
        }
    ));
}

#[test]
fn a_retry_waits_for_its_time_and_counts_attempts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir).expect("open the store");
    let id = store.enqueue(&send("1001")).expect("enqueue").id;
    store.begin_attempt(id).expect("begin");
    let retry = AttemptOutcome::Retry {
        not_before: 5_000,
        error: "429".to_owned(),
    };
    let waiting = store.finish_attempt(id, &retry).expect("retry");
    assert_eq!(waiting.state, OutboxState::Pending);
    assert_eq!(waiting.last_error.as_deref(), Some("429"));
    assert!(store.outbox_due(4_999).expect("not yet").is_empty());
    assert_eq!(store.outbox_due(5_000).expect("now").len(), 1);
    assert_eq!(store.begin_attempt(id).expect("second").attempts, 2);
}

#[test]
fn denied_and_failed_rows_are_final() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir).expect("open the store");
    for (cmid, outcome, state) in [
        (
            "1",
            AttemptOutcome::Denied {
                reason: "outsider".to_owned(),
            },
            OutboxState::Denied,
        ),
        (
            "2",
            AttemptOutcome::Failed {
                error: "400".to_owned(),
            },
            OutboxState::Failed,
        ),
    ] {
        let id = store.enqueue(&send(cmid)).expect("enqueue").id;
        store.begin_attempt(id).expect("begin");
        let done = store.finish_attempt(id, &outcome).expect("finish");
        assert_eq!(done.state, state);
        assert!(store.outbox_due(i64::MAX).expect("due").is_empty());
        assert!(store.begin_attempt(id).is_err());
    }
}

#[test]
fn an_attempt_is_finished_only_while_it_is_sending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir).expect("open the store");
    let id = store.enqueue(&send("1001")).expect("enqueue").id;
    let error = store
        .finish_attempt(id, &AttemptOutcome::Sent { message_id: None })
        .expect_err("not begun");
    assert!(matches!(
        error,
        StoreError::OutboxNotSendable {
            state: OutboxState::Pending,
            ..
        }
    ));
    let error = store.begin_attempt(999).expect_err("no such row");
    assert!(matches!(error, StoreError::UnknownOutboxEntry(999)));
}

#[test]
fn rows_left_by_an_earlier_run_are_not_sent_until_resent_explicitly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (pending, in_flight, sent) = {
        let mut earlier = open(&dir).expect("open the store");
        let pending = earlier.enqueue(&send("1")).expect("pending").id;
        let in_flight = earlier.enqueue(&send("2")).expect("in flight").id;
        earlier.begin_attempt(in_flight).expect("begin");
        let sent = earlier.enqueue(&send("3")).expect("sent").id;
        earlier.begin_attempt(sent).expect("begin");
        earlier
            .finish_attempt(sent, &AttemptOutcome::Sent { message_id: None })
            .expect("sent");
        (pending, in_flight, sent)
    };
    let mut store = open(&dir).expect("open the store");
    assert!(
        store.outbox_due(i64::MAX).expect("due").is_empty(),
        "nothing of the earlier run is due"
    );
    let left: Vec<i64> = store
        .outbox_left_by_earlier_runs()
        .expect("left")
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(left, [pending, in_flight]);
    assert!(matches!(
        store.begin_attempt(pending).expect_err("not ours"),
        StoreError::OutboxNotSendable { .. }
    ));

    let adopted = store.resend(in_flight).expect("explicit resend");
    assert_eq!((adopted.state, adopted.run_id), (OutboxState::Pending, store.run_id()));
    assert_eq!(adopted.client_message_id, "2", "the resend keeps its clientmessageid");
    assert_eq!(store.outbox_due(0).expect("due").len(), 1);
    assert!(store.resend(sent).is_err(), "a sent row can't be resent");
}

#[test]
fn a_row_of_another_open_store_is_not_adopted_until_that_store_is_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut first = open(&dir).expect("open the store");
    let pending = first.enqueue(&send("1")).expect("pending").id;
    let in_flight = first.enqueue(&send("2")).expect("in flight").id;
    first.begin_attempt(in_flight).expect("begin");
    let failed = first.enqueue(&send("3")).expect("failed").id;
    first.begin_attempt(failed).expect("begin");
    let refused = AttemptOutcome::Failed {
        error: "400".to_owned(),
    };
    first.finish_attempt(failed, &refused).expect("failed");

    // a second process on the account, while the first one's attempt hasn't reached the service yet
    let mut second = open(&dir).expect("open the store again");
    assert!(
        second.outbox_left_by_earlier_runs().expect("left").is_empty(),
        "nothing is left by a run whose store is open"
    );
    for (id, state) in [(pending, OutboxState::Pending), (in_flight, OutboxState::Sending)] {
        let error = second.resend(id).expect_err("its run is open");
        assert!(
            matches!(error, StoreError::OutboxRunOpen { id: found, run_id, state: s }
                if found == id && run_id == first.run_id() && s == state),
            "{error}"
        );
    }
    assert!(
        second.outbox_due(i64::MAX).expect("due").is_empty(),
        "no second attempt"
    );
    // a failed row has no attempt in flight: it can be resent
    let adopted = second.resend(failed).expect("resend the failed row");
    assert_eq!(adopted.run_id, second.run_id());

    // the first run finishes its own attempt: the row is still its own
    let done = first
        .finish_attempt(in_flight, &AttemptOutcome::Sent { message_id: None })
        .expect("finish the first run's attempt");
    assert_eq!((done.state, done.run_id), (OutboxState::Sent, first.run_id()));

    drop(first);
    let left: Vec<i64> = second
        .outbox_left_by_earlier_runs()
        .expect("left")
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(left, [pending], "left once its store is closed");
    let adopted = second.resend(pending).expect("explicit resend");
    assert_eq!((adopted.state, adopted.run_id), (OutboxState::Pending, second.run_id()));
}

#[test]
fn each_open_store_holds_a_private_run_lock_and_ended_runs_locks_are_removed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runs = PathBuf::from(format!("{}-runs", store_path(&dir).display()));
    let lock = |store: &Store| runs.join(format!("{}.lock", store.run_id()));
    let first = open(&dir).expect("open the store");
    let first_lock = lock(&first);
    assert_eq!(mode(&runs).expect("mode"), 0o700);
    assert_eq!(mode(&first_lock).expect("mode"), 0o600);
    let second = open(&dir).expect("open the store again");
    assert!(first_lock.exists(), "an open run's lock is kept");
    drop(first);
    let third = open(&dir).expect("open the store a third time");
    assert!(!first_lock.exists(), "an ended run's lock is removed");
    assert!(lock(&second).exists() && lock(&third).exists());
}

#[test]
fn this_runs_own_pending_or_in_flight_row_cant_be_resent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir).expect("open the store");
    let id = store.enqueue(&send("1001")).expect("enqueue").id;
    let refused = |store: &mut Store, state| {
        let error = store.resend(id).expect_err("ours, not finished");
        assert!(matches!(error, StoreError::OutboxNotSendable { state: found, .. } if found == state));
    };
    refused(&mut store, OutboxState::Pending);
    store.begin_attempt(id).expect("begin");
    refused(&mut store, OutboxState::Sending);
    assert!(store.outbox_due(i64::MAX).expect("due").is_empty(), "no second attempt");
    let retry = AttemptOutcome::Retry {
        not_before: 5_000,
        error: "429".to_owned(),
    };
    store.finish_attempt(id, &retry).expect("retry");
    refused(&mut store, OutboxState::Pending);
    assert!(store.outbox_due(4_999).expect("due").is_empty(), "Retry-After kept");
    // a failed row of this run can be resent
    store.begin_attempt(id).expect("begin");
    store
        .finish_attempt(
            id,
            &AttemptOutcome::Failed {
                error: "400".to_owned(),
            },
        )
        .expect("failed");
    assert_eq!(store.resend(id).expect("resend").state, OutboxState::Pending);
}

#[test]
fn a_check_that_couldnt_be_made_defers_or_fails_a_pending_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir).expect("open the store");
    let id = store.enqueue(&send("1001")).expect("enqueue").id;
    let first = CheckFailed::Wait {
        not_before: 0,
        error: "no sync".to_owned(),
    };
    assert_eq!(store.check_failed(id, &first).expect("a pending row").failed_checks, 1);
    store.begin_attempt(id).expect("begin");
    let in_flight = store.check_failed(
        id,
        &CheckFailed::GiveUp {
            error: "no sync".to_owned(),
        },
    );
    assert!(
        matches!(
            in_flight,
            Err(StoreError::OutboxNotSendable {
                state: OutboxState::Sending,
                ..
            })
        ),
        "only a pending row is checked: {in_flight:?}"
    );
    let retry = AttemptOutcome::Retry {
        not_before: 0,
        error: "timed out".to_owned(),
    };
    store.finish_attempt(id, &retry).expect("retry");

    let wait = CheckFailed::Wait {
        not_before: 5_000,
        error: "no sync".to_owned(),
    };
    let waiting = store.check_failed(id, &wait).expect("wait");
    assert_eq!((waiting.state, waiting.not_before), (OutboxState::Pending, Some(5_000)));
    assert_eq!((waiting.failed_checks, waiting.attempts), (2, 1));
    assert_eq!(waiting.last_error.as_deref(), Some("no sync"));
    assert!(store.outbox_due(4_999).expect("due").is_empty());

    let give_up = CheckFailed::GiveUp {
        error: "given up".to_owned(),
    };
    let failed = store.check_failed(id, &give_up).expect("give up");
    assert_eq!((failed.state, failed.failed_checks), (OutboxState::Failed, 3));
    assert!(store.outbox_due(i64::MAX).expect("due").is_empty());
    assert!(store.check_failed(id, &give_up).is_err(), "a failed row isn't checked");

    // a row our message came back for stays sent
    let sent = store.enqueue(&send("1002")).expect("enqueue").id;
    store.outbox_arrived("1002", "2000").expect("arrived");
    let after = store.check_failed(sent, &wait).expect("sent");
    assert_eq!((after.state, after.failed_checks), (OutboxState::Sent, 0));
}

#[test]
fn a_resend_starts_the_rows_budget_over_and_keeps_its_attempt_count() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir).expect("open the store");
    let id = store.enqueue(&send("1001")).expect("enqueue").id;
    for _ in 0..2 {
        store.begin_attempt(id).expect("begin");
        let retry = AttemptOutcome::Retry {
            not_before: 0,
            error: "timed out".to_owned(),
        };
        store.finish_attempt(id, &retry).expect("retry");
    }
    let give_up = CheckFailed::GiveUp {
        error: "no sync".to_owned(),
    };
    let failed = store.check_failed(id, &give_up).expect("give up");
    assert_eq!(
        (failed.attempts, failed.attempts_since_resend(), failed.failed_checks),
        (2, 2, 1)
    );

    let resent = store.resend(id).expect("resend");

    assert_eq!(resent.state, OutboxState::Pending);
    assert_eq!(
        (resent.attempts, resent.attempts_at_resend),
        (2, 2),
        "attempted before: still known"
    );
    assert_eq!((resent.attempts_since_resend(), resent.failed_checks), (0, 0));
    assert_eq!(store.begin_attempt(id).expect("begin").attempts_since_resend(), 1);
}

#[test]
fn a_store_of_the_first_schema_is_migrated_with_its_outbox_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = store_path(&dir);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("store dir");
    let raw = rusqlite::Connection::open(&path).expect("create directly");
    raw.execute_batch(include_str!("../src/migrations/0001_initial.sql"))
        .expect("the first migration");
    raw.pragma_update(None, "user_version", 1).expect("version 1");
    raw.execute_batch(
        "INSERT INTO runs (id, started_at) VALUES (1, 0);
         INSERT INTO outbox (run_id, client_message_id, conversation_id, kind, payload, state, attempts, created_at,
             updated_at)
         VALUES (1, '1001', NULL, 'send', '{}', 'failed', 3, 0, 0);",
    )
    .expect("a row of the first schema");
    drop(raw);

    let store = Store::open(&path).expect("open and migrate");

    assert_eq!(store.schema_version().expect("schema version"), SCHEMA_VERSION);
    let row = store
        .outbox_by_client_message_id("1001")
        .expect("read")
        .expect("the row");
    assert_eq!((row.attempts, row.attempts_at_resend, row.failed_checks), (3, 0, 0));
    assert_eq!(row.attempts_since_resend(), 3);
}

#[test]
fn our_message_coming_back_marks_its_row_sent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = open(&dir).expect("open the store");
    let id = store.enqueue(&send("4711")).expect("enqueue").id;
    store.begin_attempt(id).expect("begin");
    let arrived = store.outbox_arrived("4711", "1000").expect("arrived").expect("row");
    assert_eq!((arrived.id, arrived.state), (id, OutboxState::Sent));
    assert_eq!(arrived.message_id.as_deref(), Some("1000"));
    assert_eq!(store.outbox_arrived("9999", "1000").expect("not ours"), None);
    // the attempt's own answer (or its timeout) after the echo leaves the row sent
    let late = AttemptOutcome::Retry {
        not_before: 1,
        error: "timeout".to_owned(),
    };
    let row = store.finish_attempt(id, &late).expect("finished after the echo");
    assert_eq!(
        (row.state, row.message_id.as_deref()),
        (OutboxState::Sent, Some("1000"))
    );
}
