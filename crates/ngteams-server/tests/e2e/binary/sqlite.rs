//! One SQLite file under the real binary's processes at once: `serve` starting while the operator adds accounts and
//! migrates. SQLite takes one writer at a time; the others wait for the database instead of failing.

use std::sync::Arc;
use std::time::Duration;

use teams_core::engine::Action;
use teams_proto::chat::MessageText;
use teams_store::OutboxState;

use super::account;
use super::chats::support::Actor;
use super::support::{Deployment, Node};
use crate::support::{Backend, PASSWORD};

/// How many fresh deployments the race runs on: the commands start with the server, or a little later.
const ROUNDS: u64 = 10;

/// A command run beside the starting server: what it was, whether it succeeded, and its error output.
fn run(node: &Node, args: &[&str], input: &str) -> (String, bool, String) {
    let (done, said) = node.try_cli(args, input);
    (args.join(" "), done, said)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commands_and_a_starting_server_wait_for_each_other_on_sqlite() {
    for round in 0..ROUNDS {
        let deployment = Deployment::new(Backend::Sqlite, false, None).await;
        let mut server = deployment.node("a");
        let cli = Arc::new(deployment.node("a"));
        let log = deployment.dir.path().join("serve.log");
        let upns = [account(), account()];
        let delay = Duration::from_millis(round % 5 * 40);
        let mut commands = Vec::new();
        for upn in &upns {
            let (cli, upn) = (Arc::clone(&cli), upn.clone());
            commands.push(std::thread::spawn(move || {
                std::thread::sleep(delay);
                run(
                    &cli,
                    &["user", "add", &upn, "--name", "QA Operator"],
                    &format!("{PASSWORD}\n"),
                )
            }));
        }
        let migrating = Arc::clone(&cli);
        commands.push(std::thread::spawn(move || {
            std::thread::sleep(delay);
            run(&migrating, &["migrate"], "")
        }));

        server.start_logged(&log).await;
        for command in commands {
            let (what, done, said) = command.join().unwrap();
            assert!(
                done,
                "round {round}: `{what}` failed beside the starting server: {said}\nserver log:\n{}",
                std::fs::read_to_string(&log).unwrap_or_default()
            );
        }
        assert_eq!(server.status("/health/ready").await, Some(204), "round {round}");
        let listed = cli.cli(&["user", "list"], "");
        for upn in &upns {
            assert!(
                listed.lines().any(|line| line.starts_with(&format!("{upn}\t"))),
                "round {round}"
            );
        }
        server.stop().await;
    }
}

// --- QA of task 236 (round 1): the operator beside a busy server, two servers at once, a wait past the limit ---

/// Synthetic accounts the operator changes while the clients chat.
const SEEDED: usize = 30;
/// Messages each chat holds before the load starts.
const HISTORY: usize = 100;
/// The longest a command that migrates waits for the write lock (`docs/server.md`, Scaling).
const MIGRATION_WAIT: Duration = Duration::from_secs(120);
/// The longest a statement waits for the write lock (`docs/server.md`, Scaling).
const STATEMENT_WAIT: Duration = Duration::from_secs(5);

/// A sender's load: messages to `chat` until `stop`; how many it sent and every error it saw.
async fn chat_load(actor: &Actor, chat: &str, stop: &std::sync::atomic::AtomicBool) -> (usize, Vec<String>) {
    let (mut sent, mut errors) = (0, Vec::new());
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        let text = MessageText::plain(&format!("QA synthetic load message {sent}")).unwrap();
        match actor
            .engine()
            .write(Action::Send {
                conversation: chat.to_owned(),
                text,
            })
            .await
        {
            Ok(row) if row.state == OutboxState::Sent => sent += 1,
            Ok(row) => errors.push(format!("{row:?}")),
            Err(error) => errors.push(format!("{error:?}")),
        }
    }
    (sent, errors)
}

/// A reader's load: syncs until `stop`; how many syncs and every failure.
async fn sync_load(actor: &Actor, stop: &std::sync::atomic::AtomicBool) -> (usize, Vec<String>) {
    let (mut synced, mut errors) = (0, Vec::new());
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        match actor.engine().sync().await {
            Ok(report) if report.failed.is_empty() => synced += 1,
            Ok(report) => errors.push(format!("{report:?}")),
            Err(error) => errors.push(format!("{error:?}")),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (synced, errors)
}

/// A command against the file, timed: what it was, whether it succeeded, its error output, how long it took.
async fn timed(node: &Arc<Node>, args: Vec<String>, input: &'static str) -> (String, bool, String, Duration) {
    let node = Arc::clone(node);
    tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let started = std::time::Instant::now();
        let (done, said) = node.try_cli(&args, input);
        (args.join(" "), done, said, started.elapsed())
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_operators_commands_succeed_beside_a_busy_sqlite_server_and_its_clients_see_no_error() {
    let mut deployment = Deployment::new(Backend::Sqlite, false, None).await;
    deployment.origin = deployment.origin.replacen("http:", "https:", 1);
    let mut server = deployment.node("a");
    let cli = Arc::new(deployment.node("a"));
    server.cli(&["migrate"], "");
    for upn in ["a@own.test", "b@own.test", "c@own.test"] {
        server.add_user(upn, PASSWORD);
    }
    let seeded: Vec<String> = (0..SEEDED).map(|_| account()).collect();
    for upn in &seeded {
        server.add_user(upn, PASSWORD);
    }
    let log = deployment.dir.path().join("serve.log");
    server.start_logged(&log).await;
    let a = Actor::sign_in(&server, &deployment.origin, "a@own.test").await;
    let b = Actor::sign_in(&server, &deployment.origin, "b@own.test").await;
    let c = Actor::sign_in(&server, &deployment.origin, "c@own.test").await;
    let pair = a.create(&[&b], None).await;
    let group = b.create(&[&a, &c], Some("QA synthetic busy room")).await;
    for n in 0..HISTORY {
        a.send(&pair, &format!("QA synthetic history {n}")).await;
        b.send(&group, &format!("QA synthetic group history {n}")).await;
    }

    let stop = std::sync::atomic::AtomicBool::new(false);
    let mut added = Vec::new();
    let operating = async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let ran = operator(&cli, &seeded, &mut added).await;
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        ran
    };
    let (ran, first, second, (synced, read_errors)) = tokio::join!(
        operating,
        chat_load(&a, &pair, &stop),
        chat_load(&b, &group, &stop),
        sync_load(&c, &stop)
    );
    let load = [first, second];
    check_the_busy_run(&ran, &load, synced, &read_errors, &log);

    let listed = cli.cli(&["user", "list"], "");
    for upn in &added {
        assert!(
            listed.lines().any(|line| line.starts_with(&format!("{upn}\t"))),
            "{upn} not listed"
        );
    }
    for pair_of in seeded.chunks(2) {
        let [disabled, reset] = pair_of else { unreachable!() };
        assert!(
            listed
                .lines()
                .any(|line| line.starts_with(&format!("{disabled}\t")) && line.contains("\tdisabled\t")),
            "{disabled} not disabled"
        );
        assert_eq!(
            server.password(reset, "another horse battery").await.status().as_u16(),
            200
        );
    }
    assert_eq!(server.status("/health/ready").await, Some(204));
    drop((a, b, c));
    server.stop().await;
}

/// Every command succeeded and met no lock; each client sent or synced and saw no error.
fn check_the_busy_run(
    ran: &[(String, bool, String, Duration)],
    load: &[(usize, Vec<String>)],
    synced: usize,
    read_errors: &[String],
    log: &std::path::Path,
) {
    let report: Vec<String> = ran
        .iter()
        .map(|(what, done, said, took)| format!("{took:>10.3?} {done} `{what}` {said}"))
        .collect();
    eprintln!("commands beside the busy server:\n{}", report.join("\n"));
    eprintln!(
        "load: sent {:?}, synced {synced}",
        load.iter().map(|(sent, _)| sent).collect::<Vec<_>>()
    );
    for (what, done, said, took) in ran {
        assert!(
            *done,
            "`{what}` failed beside the busy server after {took:?}: {said}\nserver log:\n{}",
            std::fs::read_to_string(log).unwrap_or_default()
        );
        assert!(!said.contains("locked"), "`{what}` met a lock: {said}");
    }
    for (sent, errors) in load {
        assert!(errors.is_empty(), "a client's sends failed: {errors:?}");
        assert!(*sent > 0, "no load ran beside the commands");
    }
    assert!(read_errors.is_empty(), "a client's syncs failed: {read_errors:?}");
    assert!(synced > 0);
}

/// For each two seeded accounts: `user add` of a new one, `user disable` of the first, `user reset-password` of the
/// second; the new accounts in `added`.
async fn operator(
    cli: &Arc<Node>,
    seeded: &[String],
    added: &mut Vec<String>,
) -> Vec<(String, bool, String, Duration)> {
    let mut ran = Vec::new();
    for pair_of in seeded.chunks(2) {
        let [disabled, reset] = pair_of else { unreachable!() };
        let upn = account();
        ran.push(
            timed(
                cli,
                vec![
                    "user".into(),
                    "add".into(),
                    upn.clone(),
                    "--name".into(),
                    "QA Operator".into(),
                ],
                "correct horse battery\n",
            )
            .await,
        );
        added.push(upn);
        ran.push(timed(cli, vec!["user".into(), "disable".into(), disabled.clone()], "").await);
        ran.push(
            timed(
                cli,
                vec!["user".into(), "reset-password".into(), reset.clone()],
                "another horse battery\n",
            )
            .await,
        );
    }
    ran
}

/// `serve` on the file, as an operator starts it, its log in `log`.
fn serve(node: &Node, log: &std::path::Path) -> std::process::Child {
    let mut command = std::process::Command::new(&node.binary);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("NGTEAMS_SERVER_") {
            command.env_remove(key);
        }
    }
    command
        .args(["--config", node.config.to_str().unwrap(), "serve"])
        .env("NGTEAMS_SERVER_LOG", "info")
        .env("TOKIO_WORKER_THREADS", "2")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(log).unwrap())
        .spawn()
        .unwrap()
}

/// For each node, within 60 s: whether it serves, and how it exited if it did.
async fn served_or_exited(
    nodes: &[Node; 2],
    children: &mut [std::process::Child; 2],
) -> ([bool; 2], [Option<std::process::ExitStatus>; 2]) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut ready = [false, false];
    let mut exited = [None, None];
    while tokio::time::Instant::now() < deadline && (0..2).any(|i| !ready[i] && exited[i].is_none()) {
        let waiting: Vec<usize> = (0..2).filter(|i| !ready[*i] && exited[*i].is_none()).collect();
        for i in waiting {
            exited[i] = children[i].try_wait().unwrap();
            ready[i] = exited[i].is_none() && nodes[i].status("/health/ready").await == Some(204);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (ready, exited)
}

/// Two `serve` processes on one fresh SQLite file at once (two nodes, two ports): both migrate it safely and serve, or
/// one refuses with a clear message; never a raw "database is locked".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_servers_started_at_once_on_one_fresh_sqlite_file_both_migrate_it_safely() {
    for round in 0..ROUNDS {
        let deployment = Deployment::new(Backend::Sqlite, false, None).await;
        let nodes = [deployment.node("a"), deployment.node("b")];
        let logs = [
            deployment.dir.path().join("serve-a.log"),
            deployment.dir.path().join("serve-b.log"),
        ];
        let mut children = [serve(&nodes[0], &logs[0]), serve(&nodes[1], &logs[1])];
        let (ready, exited) = served_or_exited(&nodes, &mut children).await;
        let said = |i: usize| std::fs::read_to_string(&logs[i]).unwrap_or_default();
        for i in 0..2 {
            assert!(
                ready[i] || exited[i].is_some(),
                "round {round}: node {i} neither served nor exited in 60 s:\n{}",
                said(i)
            );
            assert!(
                !said(i).contains("database is locked"),
                "round {round}: node {i}: a raw lock error:\n{}",
                said(i)
            );
        }
        assert!(
            ready.iter().any(|ready| *ready),
            "round {round}: no node serves:\n{}\n{}",
            said(0),
            said(1)
        );
        let outcome = match ready {
            [true, true] => "both serve".to_owned(),
            _ => format!(
                "one serves; the other exited {:?}: {}",
                exited.iter().flatten().next(),
                said(usize::from(ready[0])).lines().last().unwrap_or_default()
            ),
        };
        eprintln!("round {round}: {outcome}");
        let upn = account();
        nodes[0].add_user(&upn, PASSWORD);
        for i in 0..2 {
            if ready[i] {
                assert_eq!(
                    nodes[i].password(&upn, PASSWORD).await.status().as_u16(),
                    200,
                    "round {round}: node {i} signs in the new account"
                );
            }
        }
        for child in &mut children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A command on a file with migrations to apply, while another process holds its write lock past the wait: it fails
/// after about the wait, and says it was the write lock, the file and how long it waited, not SQLite's raw error. It
/// takes over 2 minutes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_command_held_past_the_lock_wait_names_the_lock_and_the_time() {
    use diesel::connection::SimpleConnection as _;
    use diesel::{Connection as _, SqliteConnection};

    let deployment = Deployment::new(Backend::Sqlite, false, None).await;
    let node = deployment.node("a");
    let path = deployment.database.strip_prefix("sqlite://").unwrap().to_owned();
    let mut holder = SqliteConnection::establish(&path).unwrap();
    holder
        .batch_execute("PRAGMA journal_mode = WAL; BEGIN IMMEDIATE")
        .unwrap();
    let started = std::time::Instant::now();
    let (done, said) = node.try_cli(
        &["user", "add", &account(), "--name", "QA Operator"],
        &format!("{PASSWORD}\n"),
    );
    let took = started.elapsed();
    holder.batch_execute("ROLLBACK").unwrap();
    eprintln!("after {took:?}: {said}");
    assert!(!done, "the command succeeded under a held write lock: {said}");
    assert!(took >= MIGRATION_WAIT, "it gave up after {took:?}, before the wait");
    assert!(took < MIGRATION_WAIT + Duration::from_secs(60), "it waited {took:?}");
    assert_names_the_lock(&said, &path, "120 s");
}

/// A held write lock on a migrated file: a read command does not wait for it, and a write command waits only the
/// statement wait, then names the lock, the file and the time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_command_on_a_migrated_file_waits_only_the_statement_wait_for_a_held_lock() {
    use diesel::connection::SimpleConnection as _;
    use diesel::{Connection as _, SqliteConnection};

    let deployment = Deployment::new(Backend::Sqlite, false, None).await;
    let node = deployment.node("a");
    node.cli(&["migrate"], "");
    let upn = account();
    node.add_user(&upn, PASSWORD);
    let path = deployment.database.strip_prefix("sqlite://").unwrap().to_owned();
    let mut holder = SqliteConnection::establish(&path).unwrap();
    holder.batch_execute("BEGIN IMMEDIATE").unwrap();

    let started = std::time::Instant::now();
    let (listed, said) = node.try_cli(&["user", "list"], "");
    let list_took = started.elapsed();
    let started = std::time::Instant::now();
    let (added, add_said) = node.try_cli(
        &["user", "add", &account(), "--name", "QA Operator"],
        &format!("{PASSWORD}\n"),
    );
    let add_took = started.elapsed();
    holder.batch_execute("ROLLBACK").unwrap();
    eprintln!("user list after {list_took:?}: {said}\nuser add after {add_took:?}: {add_said}");
    assert!(listed, "`user list` failed beside a held write lock: {said}");
    assert!(
        list_took < STATEMENT_WAIT,
        "`user list` waited {list_took:?} for the write lock"
    );
    assert!(!added, "`user add` succeeded under a held write lock: {add_said}");
    assert!(
        add_took >= STATEMENT_WAIT,
        "`user add` gave up after {add_took:?}, before the wait"
    );
    assert!(
        add_took < STATEMENT_WAIT + Duration::from_secs(10),
        "`user add` waited {add_took:?}, past the statement wait"
    );
    assert_names_the_lock(&add_said, &path, "5 s");
}

/// `said` names the SQLite write lock, another process, the file and the time waited, and not SQLite's raw error.
fn assert_names_the_lock(said: &str, path: &str, waited: &str) {
    assert!(
        !said.contains("database is locked"),
        "the raw SQLite error, not the lock and the time: {said}"
    );
    for part in ["SQLite write lock", "another process", path, waited] {
        assert!(said.contains(part), "`{part}` not named: {said}");
    }
}
