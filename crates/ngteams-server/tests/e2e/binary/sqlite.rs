//! One SQLite file under the real binary's processes at once: `serve` starting while the operator adds accounts and
//! migrates. SQLite takes one writer at a time; the others wait for the database instead of failing.

use std::sync::Arc;
use std::time::Duration;

use super::account;
use super::support::{Deployment, Node};
use crate::support::{Backend, PASSWORD};

/// How many fresh deployments the race runs on: the commands start with the server, or a little later.
const ROUNDS: u64 = 10;

/// A command run beside the starting server: what it was, whether it succeeded, and its error output.
fn run(node: &Node, args: &[&str], input: &str) -> (String, bool, String) {
    let output = node.cli_output(args, input);
    (
        args.join(" "),
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    )
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
