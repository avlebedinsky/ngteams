//! Message and sync failures preserve durable state on a real SQLite database.

#![cfg(test)]

use teams_store::{Applied, Conversation, ConversationKind, Message, Reaction, ReactionChange, Store};

const CHAT: &str = "19:synthetic-state@thread.v2";
const ME: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const PEER: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const MESSAGE: &str = "synthetic-message";

fn setup() -> (tempfile::TempDir, Store) {
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
    (dir, store)
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

fn write(store: &mut Store, message: &Message, reactions: Option<&[Reaction]>, indexed: bool) -> Applied {
    if indexed {
        store
            .upsert_message_indexed(message, reactions, |message| {
                if message.delete_time.is_some() {
                    String::new()
                } else {
                    message.content.clone()
                }
            })
            .expect("indexed message")
    } else {
        store.upsert_message(message, reactions).expect("message")
    }
}

fn assert_deleted(store: &Store) {
    let stored = store
        .message(CHAT, MESSAGE)
        .expect("read")
        .expect("deleted row remains");
    assert_eq!(stored.version, 20);
    assert_eq!(stored.delete_time, Some(200));
    assert!(stored.content.is_empty());
    assert!(stored.properties.is_none());
    assert!(store.reactions(CHAT, MESSAGE).expect("deleted reactions").is_empty());
}

#[test]
fn a_stale_snapshot_cannot_put_reactions_back_on_a_deleted_message() {
    for indexed in [false, true] {
        let (dir, mut store) = setup();
        let old = message(10, 100);
        assert_eq!(write(&mut store, &old, None, indexed), Applied::Inserted);
        let deleted = Message {
            version: 20,
            delete_time: Some(200),
            ..old.clone()
        };
        assert_eq!(write(&mut store, &deleted, None, indexed), Applied::Updated);
        for version in [10, 20] {
            let stale = Message { version, ..old.clone() };
            assert_eq!(
                write(&mut store, &stale, Some(&[reaction("like", 30)]), indexed),
                Applied::Ignored
            );
            assert_deleted(&store);
        }
        assert_eq!(
            store
                .apply_reaction(&ReactionChange::add(CHAT, MESSAGE, "heart", ME, 40))
                .expect("reaction on deleted message"),
            Applied::Ignored
        );
        if indexed {
            assert!(store.search("Synthetic", 10).expect("search deleted text").is_empty());
        }
        drop(store);
        assert_deleted(&Store::open(&dir.path().join("store.sqlite")).expect("reopen"));
    }
}

#[test]
fn deletion_clears_a_reaction_whose_change_time_is_later_than_the_delete_version() {
    for indexed in [false, true] {
        let (_dir, mut store) = setup();
        let old = message(10, 100);
        write(&mut store, &old, Some(&[reaction("like", 30)]), indexed);
        let deleted = Message {
            version: 20,
            delete_time: Some(200),
            ..old
        };
        assert_eq!(
            write(&mut store, &deleted, Some(&[reaction("heart", 40)]), indexed),
            Applied::Updated
        );
        assert_deleted(&store);
        let restored = message(25, 100);
        assert_eq!(
            write(&mut store, &restored, Some(&[reaction("like", 29)]), indexed),
            Applied::Updated
        );
        assert!(
            store
                .reactions(CHAT, MESSAGE)
                .expect("older fact stays removed")
                .is_empty()
        );
        assert_eq!(
            store
                .apply_reaction(&ReactionChange::add(CHAT, MESSAGE, "like", ME, 30))
                .expect("equal-time fact stays removed"),
            Applied::Ignored
        );
        assert_eq!(
            store
                .apply_reaction(&ReactionChange::add(CHAT, MESSAGE, "like", ME, 31))
                .expect("newer fact on restored body"),
            Applied::Updated
        );
        assert_eq!(
            store.reactions(CHAT, MESSAGE).expect("restored reactions"),
            [reaction("like", 31)]
        );
    }
}

#[test]
fn a_stale_snapshot_still_merges_independent_reactions_on_a_nondeleted_message() {
    for indexed in [false, true] {
        let (_dir, mut store) = setup();
        write(&mut store, &message(20, 100), None, indexed);
        assert_eq!(
            write(&mut store, &message(10, 100), Some(&[reaction("like", 30)]), indexed),
            Applied::Ignored
        );
        assert_eq!(store.message(CHAT, MESSAGE).expect("read").expect("row").version, 20);
        assert_eq!(
            store.reactions(CHAT, MESSAGE).expect("new reaction fact"),
            [reaction("like", 30)]
        );
    }
}

#[test]
fn an_ignored_deleted_body_does_not_delete_a_newer_live_body() {
    for indexed in [false, true] {
        let (_dir, mut store) = setup();
        let live = message(40, 100);
        write(&mut store, &live, Some(&[reaction("like", 50)]), indexed);
        let stale_delete = Message {
            version: 20,
            delete_time: Some(200),
            ..live.clone()
        };
        assert_eq!(write(&mut store, &stale_delete, None, indexed), Applied::Ignored);
        assert_eq!(store.message(CHAT, MESSAGE).expect("read").expect("row"), live);
        assert_eq!(
            store.reactions(CHAT, MESSAGE).expect("newer reaction remains"),
            [reaction("like", 50)]
        );
    }
}

#[test]
fn inserting_a_deleted_body_discards_its_supplied_reactions() {
    let (_dir, mut store) = setup();
    let deleted = Message {
        version: 20,
        delete_time: Some(200),
        ..message(10, 100)
    };
    assert_eq!(
        write(&mut store, &deleted, Some(&[reaction("like", 30)]), false),
        Applied::Inserted
    );
    assert_deleted(&store);
}

#[test]
fn failed_forced_cleanup_rolls_back_the_delete_and_can_be_retried() {
    for indexed in [false, true] {
        let (_dir, mut store) = setup();
        let old = message(10, 100);
        let reactions = [reaction("like", 30)];
        write(&mut store, &old, Some(&reactions), indexed);
        let raw = rusqlite::Connection::open(store.path()).expect("trigger connection");
        raw.execute_batch(
            "CREATE TRIGGER refuse_cleanup BEFORE UPDATE OF present ON reactions
             WHEN OLD.changed = 30 AND NEW.present = 0
             BEGIN SELECT RAISE(ABORT, 'synthetic cleanup failure'); END;",
        )
        .expect("install failure");
        let deleted = Message {
            version: 20,
            delete_time: Some(200),
            ..old.clone()
        };
        let result = if indexed {
            store.upsert_message_indexed(&deleted, None, |_| String::new())
        } else {
            store.upsert_message(&deleted, None)
        };
        assert!(
            matches!(result, Err(teams_store::StoreError::Sqlite(_))),
            "actual result: {result:?}"
        );
        assert_eq!(store.message(CHAT, MESSAGE).expect("read").expect("row"), old);
        assert_eq!(store.reactions(CHAT, MESSAGE).expect("rollback reactions"), reactions);
        if indexed {
            assert_eq!(store.search("Synthetic", 10).expect("rollback search").len(), 1);
        }
        raw.execute_batch("DROP TRIGGER refuse_cleanup;").expect("allow retry");
        assert_eq!(write(&mut store, &deleted, None, indexed), Applied::Updated);
        assert_deleted(&store);
    }
}
