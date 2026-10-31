//! Refused outbox operations preserve durable commands and their retry budgets on a real SQLite store.

#![cfg(test)]

use std::fs;
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use teams_store::{
    AttemptOutcome, CheckFailed, EngineLock, NewOutboxEntry, OutboxEntry, OutboxKind, OutboxState, Store, StoreError,
};

const CMID: &str = "synthetic-command";
type Mutation = fn(&mut Store, i64) -> Result<OutboxEntry, StoreError>;
type Preparation = fn(&mut Store, i64);
type Case = (Preparation, Mutation);

fn command() -> NewOutboxEntry {
    NewOutboxEntry {
        conversation_id: Some("19:synthetic-outbox@thread.v2".into()),
        kind: OutboxKind::Send,
        client_message_id: CMID.into(),
        payload: r#"{"text":"synthetic"}"#.into(),
    }
}

fn run_directory(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push("-runs");
    PathBuf::from(name)
}

fn row(store: &Store, id: i64) -> OutboxEntry {
    store.outbox_entry(id).expect("read outbox").expect("stored command")
}

fn failure() -> AttemptOutcome {
    AttemptOutcome::Failed {
        error: "synthetic failure".into(),
    }
}

fn give_up() -> CheckFailed {
    CheckFailed::GiveUp {
        error: "synthetic check failure".into(),
    }
}

#[test]
fn another_run_cannot_finish_or_fail_a_check_on_a_command_it_does_not_own() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.sqlite");
    let mut owner = Store::open(&path).expect("owner");
    let pending = owner.enqueue(&command()).expect("enqueue");
    let mut observer = Store::open(&path).expect("second run");

    let error = observer
        .check_failed(pending.id, &give_up())
        .expect_err("not this run's command");
    assert!(matches!(error, StoreError::OutboxNotSendable { id, state: OutboxState::Pending } if id == pending.id));
    assert_eq!(row(&owner, pending.id), pending);

    let sending = owner.begin_attempt(pending.id).expect("owner begins");
    let error = observer
        .finish_attempt(sending.id, &failure())
        .expect_err("not this run's attempt");
    assert!(matches!(error, StoreError::OutboxNotSendable { id, state: OutboxState::Sending } if id == sending.id));
    assert_eq!(row(&owner, sending.id), sending);
    assert!(observer.outbox_due(i64::MAX).expect("nothing due").is_empty());
    assert_eq!(
        owner
            .finish_attempt(sending.id, &failure())
            .expect("owner finishes")
            .attempts,
        1
    );
}

#[test]
fn a_missing_command_is_absent_from_reads_and_every_mutation_refuses_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&dir.path().join("store.sqlite")).expect("open");
    let id = 9_999;
    assert_eq!(store.outbox_entry(id).expect("lookup"), None);
    assert_eq!(store.outbox_by_client_message_id(CMID).expect("lookup"), None);
    for error in [
        store.begin_attempt(id).expect_err("missing"),
        store.finish_attempt(id, &failure()).expect_err("missing"),
        store.check_failed(id, &give_up()).expect_err("missing"),
        store.resend(id).expect_err("missing"),
    ] {
        assert!(matches!(error, StoreError::UnknownOutboxEntry(found) if found == id));
    }
    assert!(store.outbox_due(i64::MAX).expect("nothing enqueued").is_empty());
    assert_eq!(store.enqueue(&command()).expect("still usable").client_message_id, CMID);
}

#[test]
fn unknown_stored_kind_and_state_are_reported_instead_of_becoming_sendable_commands() {
    for column in ["kind", "state"] {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("store.sqlite");
        let mut store = Store::open(&path).expect("open");
        let pending = store.enqueue(&command()).expect("enqueue");
        let raw = Connection::open(&path).expect("second connection");
        raw.execute(
            &format!("UPDATE outbox SET {column} = 'future-value' WHERE id = ?1"),
            [pending.id],
        )
        .expect("simulate a value from another build");

        let error = store.outbox_entry(pending.id).expect_err("unknown stored enum");
        let StoreError::Sqlite(rusqlite::Error::FromSqlConversionFailure(_, _, cause)) = error else {
            panic!("expected a stored-value conversion failure, got {error}");
        };
        let detail = cause.to_string();
        assert!(detail.contains("outbox"), "{detail}");
        assert!(detail.contains("future-value"), "{detail}");
        assert!(store.outbox_by_client_message_id(CMID).is_err());
        assert!(store.begin_attempt(pending.id).is_err());
        let kept: String = raw
            .query_row(
                &format!("SELECT {column} FROM outbox WHERE id = ?1"),
                [pending.id],
                |r| r.get(0),
            )
            .expect("read unchanged value");
        assert_eq!(kept, "future-value");
        let attempts: u32 = raw
            .query_row("SELECT attempts FROM outbox WHERE id = ?1", [pending.id], |r| r.get(0))
            .expect("read unchanged budget");
        assert_eq!(attempts, 0);
    }
}

#[test]
fn negative_stored_budgets_are_rejected_without_starting_an_attempt() {
    for column in ["attempts", "attempts_at_resend", "failed_checks"] {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("store.sqlite");
        let mut store = Store::open(&path).expect("open");
        let pending = store.enqueue(&command()).expect("enqueue");
        let raw = Connection::open(&path).expect("second connection");
        raw.execute(&format!("UPDATE outbox SET {column} = -1 WHERE id = ?1"), [pending.id])
            .expect("simulate corrupt budget");
        assert!(matches!(store.outbox_entry(pending.id), Err(StoreError::Sqlite(_))));
        assert!(matches!(store.begin_attempt(pending.id), Err(StoreError::Sqlite(_))));
        let stored: (String, i64) = raw
            .query_row(
                &format!("SELECT state, {column} FROM outbox WHERE id = ?1"),
                [pending.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("read corrupt row");
        assert_eq!(stored, ("pending".into(), -1));
    }
}

fn begin(store: &mut Store, id: i64) {
    store.begin_attempt(id).expect("begin setup attempt");
}

fn fail(store: &mut Store, id: i64) {
    begin(store, id);
    store.finish_attempt(id, &failure()).expect("fail setup attempt");
}

fn mutations() -> [Case; 6] {
    [
        (|_, _| {}, Store::begin_attempt),
        (begin, |store, id| store.finish_attempt(id, &failure())),
        (|_, _| {}, |store, id| store.check_failed(id, &give_up())),
        (fail, Store::resend),
        (|_, _| {}, |store, id| store.outbox_unreadable_seen(id, 5_000)),
        (
            |_, _| {},
            |store, id| {
                store
                    .outbox_arrived(CMID, "synthetic-message")?
                    .ok_or(StoreError::UnknownOutboxEntry(id))
            },
        ),
    ]
}

#[test]
fn rejected_sql_updates_roll_back_the_entire_command_and_allow_a_later_retry() {
    for (prepare, mutate) in mutations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("store.sqlite");
        let mut store = Store::open(&path).expect("open");
        let id = store.enqueue(&command()).expect("enqueue").id;
        prepare(&mut store, id);
        let before = row(&store, id);
        let raw = Connection::open(&path).expect("second connection");
        // An AFTER trigger fails after SQLite has changed the row: the statement must roll everything back.
        raw.execute_batch(
            "CREATE TRIGGER refuse_update AFTER UPDATE ON outbox BEGIN
            SELECT RAISE(ABORT, 'synthetic update refused'); END;",
        )
        .expect("install refusal");

        let error = mutate(&mut store, id).expect_err("database rejected the write");

        assert!(matches!(error, StoreError::Sqlite(_)), "{error}");
        assert_eq!(row(&store, id), before, "a failed mutation preserves every field");
        raw.execute_batch("DROP TRIGGER refuse_update").expect("remove refusal");
        mutate(&mut store, id).expect("a later retry can apply the same operation");
    }
}

#[test]
fn a_failed_insert_does_not_reserve_the_client_message_id_or_leave_a_partial_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.sqlite");
    let mut store = Store::open(&path).expect("open");
    let raw = Connection::open(&path).expect("second connection");
    raw.execute_batch(
        "CREATE TRIGGER refuse_insert AFTER INSERT ON outbox BEGIN
        SELECT RAISE(ABORT, 'synthetic insert refused'); END;",
    )
    .expect("install refusal");
    assert!(matches!(store.enqueue(&command()), Err(StoreError::Sqlite(_))));
    assert_eq!(store.outbox_by_client_message_id(CMID).expect("lookup"), None);
    assert!(store.outbox_due(i64::MAX).expect("no partial command").is_empty());
    raw.execute_batch("DROP TRIGGER refuse_insert").expect("remove refusal");
    let pending = store.enqueue(&command()).expect("retry the same command");
    drop(store);
    let reopened = Store::open(&path).expect("reopen");
    assert_eq!(row(&reopened, pending.id), pending, "the successful retry is durable");
}

#[test]
fn an_unreadable_run_lock_prevents_adoption_without_changing_the_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.sqlite");
    let mut owner = Store::open(&path).expect("owner");
    let pending = owner.enqueue(&command()).expect("enqueue");
    let mut observer = Store::open(&path).expect("observer");
    drop(owner);
    let lock = run_directory(&path).join(format!("{}.lock", pending.run_id));
    fs::remove_file(&lock).expect("remove ended run's lock");
    fs::create_dir(&lock).expect("simulate an unusable lock path");
    for error in [
        observer.outbox_left_by_earlier_runs().expect_err("cannot check run"),
        observer.resend(pending.id).expect_err("cannot adopt unchecked run"),
    ] {
        assert!(matches!(error, StoreError::File { path, .. } if path == lock));
    }
    assert_eq!(row(&observer, pending.id), pending);
    fs::remove_dir(&lock).expect("restore missing ended lock");
    assert_eq!(
        observer.outbox_left_by_earlier_runs().expect("ended run"),
        std::slice::from_ref(&pending)
    );
    assert_eq!(
        observer.resend(pending.id).expect("explicit adoption").run_id,
        observer.run_id()
    );
}

#[test]
fn a_failed_lock_sweep_refuses_open_without_disturbing_an_existing_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.sqlite");
    let mut owner = Store::open(&path).expect("owner");
    let pending = owner.enqueue(&command()).expect("enqueue");
    let bad_lock = run_directory(&path).join("999.lock");
    fs::create_dir(&bad_lock).expect("unusable lock path");
    assert!(matches!(Store::open(&path), Err(StoreError::File { path, .. }) if path == bad_lock));
    assert_eq!(row(&owner, pending.id), pending);
    assert_eq!(owner.begin_attempt(pending.id).expect("owner still usable").attempts, 1);
    fs::remove_dir(&bad_lock).expect("remove unusable path");
    let observer = Store::open(&path).expect("open after repair");
    assert!(
        observer
            .outbox_left_by_earlier_runs()
            .expect("owner remains open")
            .is_empty()
    );
}

#[test]
fn lock_cleanup_preserves_files_that_do_not_name_a_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.sqlite");
    let owner = Store::open(&path).expect("owner");
    let runs = run_directory(&path);
    let names = [
        "not-a-run.lock".into(),
        ".lock".into(),
        "123".into(),
        std::ffi::OsString::from_vec(b"invalid-\xff.lock".to_vec()),
    ];
    for name in &names {
        fs::write(runs.join(name), b"synthetic unrelated data").expect("unrelated file");
    }
    drop(owner);
    let _next = Store::open(&path).expect("sweep ended locks");
    for name in names {
        assert_eq!(
            fs::read(runs.join(name)).expect("unrelated file survives"),
            b"synthetic unrelated data"
        );
    }
}

#[test]
fn an_unusable_engine_lock_path_is_reported_without_creating_a_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.sqlite");
    let lock = run_directory(&path).join("engine.lock");
    fs::create_dir_all(&lock).expect("unusable lock path");
    assert!(matches!(EngineLock::hold(&path), Err(StoreError::File { path, .. }) if path == lock));
    assert!(!path.exists());
    fs::remove_dir(&lock).expect("remove unusable lock path");
    let held = EngineLock::hold(&path).expect("take engine lock without opening store");
    assert_eq!(held.path(), lock);
    assert!(!path.exists());
}

#[test]
fn a_file_in_place_of_the_run_directory_is_preserved_and_no_store_is_created() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.sqlite");
    let runs = run_directory(&path);
    fs::write(&runs, b"synthetic unrelated file").expect("block the run directory");
    for error in [
        EngineLock::hold(&path).expect_err("no directory for engine lock"),
        Store::open(&path).expect_err("no directory for open lock"),
    ] {
        assert!(matches!(error, StoreError::File { path, .. } if path == runs));
    }
    assert_eq!(fs::read(&runs).expect("unchanged file"), b"synthetic unrelated file");
    assert!(!path.exists());
}
