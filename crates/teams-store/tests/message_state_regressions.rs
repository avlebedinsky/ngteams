//! Message and sync failures preserve durable state on a real SQLite database.

#![cfg(test)]

use rusqlite::Connection;
use teams_store::{
    Applied, Conversation, ConversationKind, Message, Reaction, ReactionChange, ReadHorizon, Store, StoreError,
    SyncState,
};

const CHAT: &str = "19:synthetic-state@thread.v2";
const ME: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const PEER: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const MESSAGE: &str = "synthetic-message";

fn setup() -> (tempfile::TempDir, Store, Connection) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&dir.path().join("store.sqlite")).expect("open store");
    store
        .upsert_conversation(&Conversation {
            id: CHAT.into(),
            kind: ConversationKind::Group,
            topic: Some("Synthetic chat".into()),
            version: 1,
            last_activity: Some(100),
        })
        .expect("conversation");
    let raw = Connection::open(store.path()).expect("second connection to the store's path");
    (dir, store, raw)
}

fn message(version: i64, arrival: i64) -> Message {
    Message {
        conversation_id: CHAT.into(),
        id: MESSAGE.into(),
        version,
        client_message_id: Some("synthetic-client-message".into()),
        sender: Some(PEER.into()),
        sender_name: Some("Synthetic peer".into()),
        message_type: "Text".into(),
        content: format!("Synthetic version {version}"),
        properties: Some("{}".into()),
        arrival_time: arrival,
        compose_time: Some(arrival),
        edit_time: None,
        delete_time: None,
    }
}

fn reaction(key: &str, time: i64) -> Reaction {
    Reaction {
        key: key.into(),
        user: ME.into(),
        time,
    }
}

fn seeded(store: &mut Store) -> Message {
    let old = message(10, 100);
    store
        .upsert_message(&old, Some(&[reaction("like", 10)]))
        .expect("initial message and reaction");
    old
}

fn stored(store: &Store) -> Message {
    store
        .message(CHAT, MESSAGE)
        .expect("read message")
        .expect("stored message")
}

fn assert_refused(error: StoreError) {
    assert!(
        matches!(error, StoreError::Sqlite(rusqlite::Error::SqliteFailure(_, Some(reason))) if reason == "synthetic refusal")
    );
}

fn rename(raw: &Connection, from: &str, to: &str) {
    raw.execute_batch(&format!("ALTER TABLE {from} RENAME TO {to}"))
        .expect("temporarily unavailable storage");
}

#[track_caller]
fn assert_storage_error<T: std::fmt::Debug>(result: Result<T, StoreError>) {
    let error = result.expect_err("unavailable storage must be reported");
    assert!(matches!(&error, StoreError::Sqlite(_)), "{error}");
}

fn defer_commit_failure(raw: &Connection, trigger: &str) {
    raw.execute_batch(
        "CREATE TABLE synthetic_required (id INTEGER PRIMARY KEY) STRICT;
         CREATE TABLE synthetic_deferred_check (
             parent_id INTEGER REFERENCES synthetic_required(id) DEFERRABLE INITIALLY DEFERRED
         ) STRICT;",
    )
    .expect("valid storage whose foreign-key constraint is deferred until commit");
    raw.execute_batch(trigger)
        .expect("inject a failure after successful writes");
}

fn assert_commit_refused(error: &StoreError) {
    assert!(
        matches!(error, StoreError::Sqlite(rusqlite::Error::SqliteFailure(code, _))
            if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY)
    );
}

fn state(updated: i64) -> SyncState {
    SyncState {
        conversation_id: CHAT.into(),
        sync_link: Some(format!("synthetic-sync-{updated}")),
        backward_link: Some("synthetic-history".into()),
        backward_oldest: Some(10),
        sync_until: Some(100),
        gap_link: Some("synthetic-gap".into()),
        gap_until: Some(50),
        history_complete: false,
        updated,
    }
}

#[test]
fn failing_to_update_activity_rolls_back_a_message_insert_or_edit() {
    for insert in [true, false] {
        let (_dir, mut store, raw) = setup();
        let old = seeded(&mut store);
        let chat = store.conversation(CHAT).expect("conversation");
        raw.execute_batch(
            "CREATE TRIGGER refuse_activity BEFORE UPDATE OF last_activity ON conversations
             BEGIN SELECT RAISE(ABORT, 'synthetic refusal'); END",
        )
        .expect("refuse the second write after writing the message");
        let next = Message {
            id: if insert {
                "synthetic-second-message".into()
            } else {
                MESSAGE.into()
            },
            ..message(20, 200)
        };

        assert_refused(store.upsert_message(&next, None).expect_err("activity write fails"));

        assert_eq!(stored(&store), old);
        assert_eq!(store.conversation(CHAT).expect("unchanged activity"), chat);
        if insert {
            assert_eq!(store.message(CHAT, &next.id).expect("rolled back insert"), None);
        }
        raw.execute_batch("DROP TRIGGER refuse_activity")
            .expect("restore writes");
        assert_eq!(
            store.upsert_message(&next, None).expect("retry"),
            if insert { Applied::Inserted } else { Applied::Updated }
        );
        assert_eq!(store.message(CHAT, &next.id).expect("retried message"), Some(next));
        assert_eq!(
            store
                .conversation(CHAT)
                .expect("conversation")
                .expect("stored")
                .last_activity,
            Some(200)
        );
    }
}

#[test]
fn a_reaction_insert_failure_rolls_back_earlier_removals_and_the_message_edit() {
    let (_dir, mut store, raw) = setup();
    let old = seeded(&mut store);
    let next = message(20, 200);
    let replacement = [reaction("heart", 20), reaction("laugh", 20)];
    raw.execute_batch(
        "CREATE TRIGGER refuse_reaction BEFORE INSERT ON reactions WHEN NEW.reaction = 'laugh'
         BEGIN SELECT RAISE(ABORT, 'synthetic refusal'); END",
    )
    .expect("fail after removing like and inserting heart");

    assert_refused(
        store
            .upsert_message(&next, Some(&replacement))
            .expect_err("reaction insert fails"),
    );

    assert_eq!(stored(&store), old);
    assert_eq!(
        store.reactions(CHAT, MESSAGE).expect("prior reaction"),
        [reaction("like", 10)]
    );
    // A failed merge must not advance the whole-set watermark and suppress a later per-user delivery.
    assert_eq!(
        store
            .apply_reaction(&ReactionChange::add(CHAT, MESSAGE, "wave", PEER, 15))
            .expect("change after the old set"),
        Applied::Inserted
    );
    raw.execute_batch("DROP TRIGGER refuse_reaction")
        .expect("restore reactions");
    assert_eq!(
        store.upsert_message(&next, Some(&replacement)).expect("retry"),
        Applied::Updated
    );
    assert_eq!(store.reactions(CHAT, MESSAGE).expect("replacement"), replacement);
}

#[test]
fn a_failed_reaction_watermark_rolls_back_the_whole_merge() {
    let (_dir, mut store, raw) = setup();
    let old = seeded(&mut store);
    raw.execute_batch(
        "CREATE TRIGGER refuse_watermark BEFORE UPDATE OF reactions_as_of ON messages
         BEGIN SELECT RAISE(ABORT, 'synthetic refusal'); END",
    )
    .expect("fail after the reaction facts were written");
    let next = message(20, 200);

    assert_refused(
        store
            .upsert_message(&next, Some(&[reaction("heart", 20)]))
            .expect_err("watermark fails"),
    );

    assert_eq!(stored(&store), old);
    assert_eq!(
        store.reactions(CHAT, MESSAGE).expect("prior facts"),
        [reaction("like", 10)]
    );
    assert_eq!(
        store.conversation(CHAT).expect("activity").expect("chat").last_activity,
        Some(100)
    );
    raw.execute_batch("DROP TRIGGER refuse_watermark")
        .expect("restore watermark");
    store
        .upsert_message(&next, Some(&[reaction("heart", 20)]))
        .expect("retry");
    assert_eq!(
        store.reactions(CHAT, MESSAGE).expect("new facts"),
        [reaction("heart", 20)]
    );
}

#[test]
fn a_delete_whose_reaction_cleanup_fails_leaves_the_message_usable_for_retry() {
    let (_dir, mut store, raw) = setup();
    let old = seeded(&mut store);
    let deleted = Message {
        version: 20,
        delete_time: Some(200),
        ..old.clone()
    };
    raw.execute_batch(
        "CREATE TRIGGER refuse_removal BEFORE UPDATE ON reactions
         BEGIN SELECT RAISE(ABORT, 'synthetic refusal'); END",
    )
    .expect("refuse clearing the reaction after the body was deleted");

    assert_refused(store.upsert_message(&deleted, None).expect_err("cleanup fails"));

    assert_eq!(stored(&store), old);
    assert_eq!(
        store.reactions(CHAT, MESSAGE).expect("prior reaction"),
        [reaction("like", 10)]
    );
    raw.execute_batch("DROP TRIGGER refuse_removal")
        .expect("restore cleanup");
    store.upsert_message(&deleted, None).expect("retry delete");
    let path = store.path().to_owned();
    drop(store);
    let mut reopened = Store::open(&path).expect("reopen");
    let row = stored(&reopened);
    assert_eq!(row.delete_time, Some(200));
    assert!(row.content.is_empty());
    assert_eq!(row.properties, None);
    assert!(reopened.reactions(CHAT, MESSAGE).expect("cleared reactions").is_empty());
    assert_eq!(
        reopened
            .apply_reaction(&ReactionChange::add(CHAT, MESSAGE, "heart", ME, 300))
            .expect("late delivery on deleted message"),
        Applied::Ignored
    );
}

#[test]
fn a_refused_per_user_reaction_preserves_the_previous_choice_until_retry() {
    let (_dir, mut store, raw) = setup();
    seeded(&mut store);
    raw.execute_batch(
        "CREATE TRIGGER refuse_reaction BEFORE INSERT ON reactions
         BEGIN SELECT RAISE(ABORT, 'synthetic refusal'); END",
    )
    .expect("refuse reaction updates");
    let removal = ReactionChange::remove(CHAT, MESSAGE, "like", ME, 20);

    assert_refused(store.apply_reaction(&removal).expect_err("write refused"));

    assert_eq!(
        store.reactions(CHAT, MESSAGE).expect("still present"),
        [reaction("like", 10)]
    );
    raw.execute_batch("DROP TRIGGER refuse_reaction")
        .expect("restore reactions");
    assert_eq!(store.apply_reaction(&removal).expect("retry"), Applied::Updated);
    assert!(store.reactions(CHAT, MESSAGE).expect("removed").is_empty());
}

#[test]
fn unavailable_message_storage_is_an_error_instead_of_a_missing_message_or_empty_page() {
    let (_dir, mut store, raw) = setup();
    let old = seeded(&mut store);
    rename(&raw, "messages", "unavailable_messages");

    assert_storage_error(store.message(CHAT, MESSAGE));
    assert_storage_error(store.message_by_client_id(CHAT, "synthetic-client-message"));
    assert_storage_error(store.messages_before(CHAT, None, 10));
    assert_storage_error(store.unread_count(CHAT, ME));
    assert_storage_error(store.display_name(PEER));
    assert_storage_error(store.upsert_message(&message(20, 200), None));
    assert_storage_error(store.apply_reaction(&ReactionChange::add(CHAT, MESSAGE, "heart", ME, 20)));

    rename(&raw, "unavailable_messages", "messages");
    assert_eq!(stored(&store), old);
    assert_eq!(
        store
            .messages_before(CHAT, None, 10)
            .expect("page after recovery")
            .messages,
        [old]
    );
    assert_eq!(store.unread_count(CHAT, ME).expect("unread count"), 1);
    assert_eq!(store.display_name(PEER).expect("sender"), Some("Synthetic peer".into()));
}

#[test]
fn unavailable_reaction_storage_does_not_look_like_no_reactions() {
    let (_dir, mut store, raw) = setup();
    seeded(&mut store);
    rename(&raw, "reactions", "unavailable_reactions");

    assert!(matches!(store.reactions(CHAT, MESSAGE), Err(StoreError::Sqlite(_))));
    assert!(matches!(store.recent_reactions(ME, 10), Err(StoreError::Sqlite(_))));
    assert!(matches!(store.frequent_reactions(ME, 10), Err(StoreError::Sqlite(_))));
    assert!(matches!(
        store.apply_reaction(&ReactionChange::remove(CHAT, MESSAGE, "like", ME, 20)),
        Err(StoreError::Sqlite(_))
    ));

    rename(&raw, "unavailable_reactions", "reactions");
    assert_eq!(
        store.reactions(CHAT, MESSAGE).expect("recover reactions"),
        [reaction("like", 10)]
    );
    assert_eq!(store.recent_reactions(ME, 10).expect("recent"), ["like"]);
    assert_eq!(store.frequent_reactions(ME, 10).expect("frequent"), ["like"]);
}

#[test]
fn a_refused_read_horizon_keeps_unread_messages_unread_until_the_retry_commits() {
    let (_dir, mut store, raw) = setup();
    seeded(&mut store);
    let before = ReadHorizon {
        conversation_id: CHAT.into(),
        user: ME.into(),
        last_read_arrival: 50,
        updated: 10,
        raw: Some("synthetic-old-horizon".into()),
    };
    store.set_read_horizon(&before).expect("prior horizon");
    let after = ReadHorizon {
        last_read_arrival: 100,
        updated: 20,
        raw: Some("synthetic-new-horizon".into()),
        ..before.clone()
    };
    raw.execute_batch(
        "CREATE TRIGGER refuse_horizon BEFORE UPDATE ON read_horizons
         BEGIN SELECT RAISE(ABORT, 'synthetic refusal'); END",
    )
    .expect("refuse horizon checkpoint");

    assert_refused(store.set_read_horizon(&after).expect_err("write refused"));

    assert_eq!(store.read_horizon(CHAT, ME).expect("prior horizon"), Some(before));
    assert_eq!(store.unread_count(CHAT, ME).expect("still unread"), 1);
    raw.execute_batch("DROP TRIGGER refuse_horizon")
        .expect("restore checkpoint");
    assert_eq!(store.set_read_horizon(&after).expect("retry"), Applied::Updated);
    let path = store.path().to_owned();
    drop(store);
    let reopened = Store::open(&path).expect("reopen");
    assert_eq!(reopened.read_horizon(CHAT, ME).expect("durable horizon"), Some(after));
    assert_eq!(reopened.unread_count(CHAT, ME).expect("now read"), 0);
}

#[test]
fn a_refused_sync_checkpoint_keeps_both_history_and_gap_boundaries_for_retry() {
    let (_dir, mut store, raw) = setup();
    let before = state(10);
    store.set_sync_state(&before).expect("prior checkpoint");
    let after = SyncState {
        sync_link: Some("synthetic-new-sync".into()),
        backward_link: None,
        backward_oldest: Some(1),
        sync_until: Some(200),
        gap_link: None,
        gap_until: None,
        history_complete: true,
        ..state(20)
    };
    raw.execute_batch(
        "CREATE TRIGGER refuse_sync BEFORE UPDATE ON conversation_sync
         BEGIN SELECT RAISE(ABORT, 'synthetic refusal'); END",
    )
    .expect("refuse replacing the checkpoint");

    assert_refused(store.set_sync_state(&after).expect_err("write refused"));

    assert_eq!(store.sync_state(CHAT).expect("old checkpoint"), Some(before));
    raw.execute_batch("DROP TRIGGER refuse_sync")
        .expect("restore checkpoint");
    store.set_sync_state(&after).expect("retry");
    let path = store.path().to_owned();
    drop(store);
    assert_eq!(
        Store::open(&path).expect("reopen").sync_state(CHAT).expect("durable"),
        Some(after)
    );
}

#[test]
fn unavailable_sync_storage_is_reported_instead_of_starting_a_new_sync() {
    let (_dir, mut store, raw) = setup();
    let before = state(10);
    store.set_sync_state(&before).expect("prior checkpoint");
    rename(&raw, "conversation_sync", "unavailable_sync");

    assert!(matches!(store.sync_state(CHAT), Err(StoreError::Sqlite(_))));
    assert!(matches!(store.set_sync_state(&state(20)), Err(StoreError::Sqlite(_))));

    rename(&raw, "unavailable_sync", "conversation_sync");
    assert_eq!(store.sync_state(CHAT).expect("recover prior checkpoint"), Some(before));
}

#[test]
fn refused_list_cursor_and_dirty_marker_writes_leave_the_retry_work_durable() {
    let (_dir, mut store, raw) = setup();
    store
        .set_conversation_list_sync_link(Some("synthetic-old-list"), 10)
        .expect("old cursor");
    store.set_unsynced(CHAT).expect("work to retry");
    raw.execute_batch(
        "CREATE TRIGGER refuse_list BEFORE UPDATE ON conversation_list_sync
         BEGIN SELECT RAISE(ABORT, 'synthetic refusal'); END;
         CREATE TRIGGER refuse_synced BEFORE DELETE ON unsynced_conversations
         BEGIN SELECT RAISE(ABORT, 'synthetic refusal'); END",
    )
    .expect("refuse committing progress");

    assert_refused(
        store
            .set_conversation_list_sync_link(None, 20)
            .expect_err("cursor refused"),
    );
    assert_refused(store.set_synced(CHAT).expect_err("marker refused"));

    assert_eq!(
        store.conversation_list_sync_link().expect("prior cursor"),
        Some("synthetic-old-list".into())
    );
    assert_eq!(store.unsynced_conversations().expect("pending work"), [CHAT]);
    raw.execute_batch("DROP TRIGGER refuse_list; DROP TRIGGER refuse_synced")
        .expect("restore progress writes");
    store.set_conversation_list_sync_link(None, 20).expect("retry cursor");
    assert!(store.set_synced(CHAT).expect("retry marker"));
    let path = store.path().to_owned();
    drop(store);
    let reopened = Store::open(&path).expect("reopen");
    assert_eq!(reopened.conversation_list_sync_link().expect("reset cursor"), None);
    assert!(reopened.unsynced_conversations().expect("work completed").is_empty());
}

#[test]
fn failed_dirty_marker_creation_and_unknown_targets_leave_existing_work_intact() {
    let (_dir, mut store, raw) = setup();
    raw.execute_batch(
        "CREATE TRIGGER refuse_dirty BEFORE INSERT ON unsynced_conversations
         BEGIN SELECT RAISE(ABORT, 'synthetic refusal'); END",
    )
    .expect("refuse saving work");
    assert_refused(store.set_unsynced(CHAT).expect_err("write refused"));
    assert!(store.unsynced_conversations().expect("no phantom marker").is_empty());
    raw.execute_batch("DROP TRIGGER refuse_dirty")
        .expect("restore saving work");
    store.set_unsynced(CHAT).expect("retry");

    let unknown = "19:synthetic-unknown@thread.v2";
    let horizon = ReadHorizon {
        conversation_id: unknown.into(),
        user: ME.into(),
        last_read_arrival: 100,
        updated: 20,
        raw: None,
    };
    for error in [
        store.set_read_horizon(&horizon).expect_err("unknown horizon target"),
        store
            .set_sync_state(&SyncState {
                conversation_id: unknown.into(),
                ..state(20)
            })
            .expect_err("unknown sync target"),
        store.set_unsynced(unknown).expect_err("unknown retry target"),
    ] {
        assert!(matches!(error, StoreError::UnknownConversation(id) if id == unknown));
    }
    assert_eq!(store.unsynced_conversations().expect("prior work"), [CHAT]);
    assert_eq!(store.sync_state(unknown).expect("no phantom checkpoint"), None);
    assert_eq!(store.read_horizon(unknown, ME).expect("no phantom horizon"), None);
}

#[test]
fn a_reaction_removal_wins_an_equal_time_add_or_whole_set_in_either_delivery_order() {
    for whole_set in [true, false] {
        for removal_first in [true, false] {
            let (_dir, mut store, _raw) = setup();
            seeded(&mut store);
            let remove = ReactionChange::remove(CHAT, MESSAGE, "like", ME, 20);
            if removal_first {
                assert_eq!(store.apply_reaction(&remove).expect("remove first"), Applied::Updated);
            }
            if whole_set {
                store
                    .upsert_message(&message(20, 200), Some(&[reaction("like", 20)]))
                    .expect("whole set at the same time");
            } else {
                store
                    .apply_reaction(&ReactionChange::add(CHAT, MESSAGE, "like", ME, 20))
                    .expect("per-user add at the same time");
            }
            if !removal_first {
                assert_eq!(store.apply_reaction(&remove).expect("remove last"), Applied::Updated);
            }
            assert!(store.reactions(CHAT, MESSAGE).expect("removal wins").is_empty());
            assert_eq!(
                store.apply_reaction(&remove).expect("duplicate removal"),
                Applied::Ignored
            );
        }
    }
}

#[test]
fn maximum_usize_limits_return_all_results_without_truncation_or_wrapping() {
    let (_dir, mut store, _raw) = setup();
    let first = seeded(&mut store);
    let second = Message {
        id: "synthetic-second".into(),
        client_message_id: None,
        ..message(20, 200)
    };
    store
        .upsert_message(&second, Some(&[reaction("heart", 20)]))
        .expect("second message");

    let page = store.messages_before(CHAT, None, usize::MAX).expect("large page limit");
    assert_eq!(page.messages, [second, first]);
    assert_eq!(page.next, None);
    assert_eq!(
        store.recent_reactions(ME, usize::MAX).expect("large recent limit"),
        ["heart", "like"]
    );
    assert_eq!(
        store.frequent_reactions(ME, usize::MAX).expect("large frequent limit"),
        ["heart", "like"]
    );
}

#[test]
fn a_refused_message_statement_preserves_the_prior_message_and_conversation() {
    let (_dir, mut store, raw) = setup();
    let before = seeded(&mut store);
    let chat = store.conversation(CHAT).expect("chat");
    raw.execute_batch(
        "CREATE TRIGGER refuse_message BEFORE INSERT ON messages
         BEGIN SELECT RAISE(ABORT, 'synthetic refusal'); END",
    )
    .expect("refuse the message statement itself");

    assert_refused(
        store
            .upsert_message(&message(20, 200), None)
            .expect_err("message refused"),
    );

    assert_eq!(stored(&store), before);
    assert_eq!(store.conversation(CHAT).expect("unchanged chat"), chat);
    assert_eq!(
        store.reactions(CHAT, MESSAGE).expect("unchanged reactions"),
        [reaction("like", 10)]
    );
    raw.execute_batch("DROP TRIGGER refuse_message")
        .expect("restore message writes");
    assert_eq!(
        store.upsert_message(&message(20, 200), None).expect("retry"),
        Applied::Updated
    );
}

#[test]
fn unavailable_progress_storage_never_looks_like_a_fresh_cursor_or_an_empty_retry_queue() {
    let (_dir, mut store, raw) = setup();
    let horizon = ReadHorizon {
        conversation_id: CHAT.into(),
        user: ME.into(),
        last_read_arrival: 100,
        updated: 20,
        raw: Some("synthetic-horizon".into()),
    };
    store.set_read_horizon(&horizon).expect("prior horizon");
    store
        .set_conversation_list_sync_link(Some("synthetic-list"), 20)
        .expect("prior cursor");
    store.set_unsynced(CHAT).expect("work to retry");

    rename(&raw, "read_horizons", "unavailable_horizons");
    assert_storage_error(store.read_horizon(CHAT, ME));
    assert_storage_error(store.set_read_horizon(&horizon));
    rename(&raw, "unavailable_horizons", "read_horizons");
    assert_eq!(
        store.read_horizon(CHAT, ME).expect("prior horizon survives"),
        Some(horizon)
    );

    rename(&raw, "conversation_list_sync", "unavailable_list_sync");
    assert_storage_error(store.conversation_list_sync_link());
    assert_storage_error(store.set_conversation_list_sync_link(None, 30));
    rename(&raw, "unavailable_list_sync", "conversation_list_sync");
    assert_eq!(
        store.conversation_list_sync_link().expect("prior cursor survives"),
        Some("synthetic-list".into())
    );

    rename(&raw, "unsynced_conversations", "unavailable_dirty_markers");
    assert_storage_error(store.unsynced_conversations());
    assert_storage_error(store.set_unsynced(CHAT));
    assert_storage_error(store.set_synced(CHAT));
    rename(&raw, "unavailable_dirty_markers", "unsynced_conversations");
    assert_eq!(store.unsynced_conversations().expect("prior work survives"), [CHAT]);
}

#[test]
fn a_failed_commit_keeps_the_prior_sync_checkpoint_and_pending_retry_work() {
    let (_dir, mut store, raw) = setup();
    let before = state(10);
    let after = state(20);
    store.set_sync_state(&before).expect("prior checkpoint");
    store.set_unsynced(CHAT).expect("pending work");
    defer_commit_failure(
        &raw,
        "CREATE TRIGGER refuse_commit AFTER UPDATE ON conversation_sync
         BEGIN INSERT INTO synthetic_deferred_check VALUES (999); END",
    );

    let error = store.set_sync_state(&after).expect_err("the commit fails");

    assert_commit_refused(&error);
    assert_eq!(store.sync_state(CHAT).expect("old checkpoint remains"), Some(before));
    assert_eq!(store.unsynced_conversations().expect("retry still needed"), [CHAT]);
    let count: i64 = raw
        .query_row("SELECT COUNT(*) FROM synthetic_deferred_check", [], |row| row.get(0))
        .expect("failed transaction was rolled back");
    assert_eq!(count, 0);
    raw.execute_batch("DROP TRIGGER refuse_commit")
        .expect("restore commits");
    store.set_sync_state(&after).expect("retry succeeds on the same store");
    assert_eq!(store.sync_state(CHAT).expect("new checkpoint"), Some(after));
    assert!(store.set_synced(CHAT).expect("the caller can now finish the retry"));
}

#[test]
fn a_failed_commit_rolls_back_the_message_activity_and_every_reaction_change() {
    let (_dir, mut store, raw) = setup();
    let before = seeded(&mut store);
    let chat = store.conversation(CHAT).expect("prior activity");
    defer_commit_failure(
        &raw,
        "CREATE TRIGGER refuse_commit AFTER UPDATE OF reactions_as_of ON messages
         BEGIN INSERT INTO synthetic_deferred_check VALUES (999); END",
    );
    let after = message(20, 200);
    let replacement = [reaction("heart", 20)];

    let error = store
        .upsert_message(&after, Some(&replacement))
        .expect_err("all statements succeeded but the commit failed");

    assert_commit_refused(&error);
    assert_eq!(stored(&store), before);
    assert_eq!(store.conversation(CHAT).expect("activity rolled back"), chat);
    assert_eq!(
        store.reactions(CHAT, MESSAGE).expect("reaction changes rolled back"),
        [reaction("like", 10)]
    );
    let count: i64 = raw
        .query_row("SELECT COUNT(*) FROM synthetic_deferred_check", [], |row| row.get(0))
        .expect("the deferred side effect was rolled back too");
    assert_eq!(count, 0);
    raw.execute_batch("DROP TRIGGER refuse_commit")
        .expect("restore commits");
    assert_eq!(
        store.upsert_message(&after, Some(&replacement)).expect("retry"),
        Applied::Updated
    );
    assert_eq!(stored(&store), after);
    assert_eq!(
        store.reactions(CHAT, MESSAGE).expect("replacement after retry"),
        replacement
    );
    assert_eq!(
        store
            .conversation(CHAT)
            .expect("new activity")
            .expect("chat")
            .last_activity,
        Some(200)
    );
}
