//! Offline subprocess contracts. A reexecuted synthetic helper starts one descendant holding its stdout open.
//! EOF proves cleanup reached that descendant even if its initial parent already exited.
#![cfg(test)]
use std::io::Read as _;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use teams_platform::command::Tree;

const ROLE: &str = "NGTEAMS_SYNTHETIC_TREE_ROLE";
const READY: &str = "NGTEAMS_SYNTHETIC_TREE_READY";

fn helper(role: &str, ready: &std::path::Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            "synthetic_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ROLE, role)
        .env(READY, ready);
    command
}

#[test]
#[ignore = "reexecuted by the process tree tests"]
#[expect(
    clippy::zombie_processes,
    reason = "the parent-exits case deliberately leaves a descendant for the tree owner to terminate"
)]
fn synthetic_child() {
    let role = std::env::var(ROLE).unwrap();
    let ready = std::env::var_os(READY).unwrap();
    if role == "descendant" {
        std::fs::write(ready, b"ready").unwrap();
        loop {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let mut child = helper("descendant", std::path::Path::new(&ready)).spawn().unwrap();
    if role == "parent-exits" {
        return;
    }
    let _ = child.wait();
}

fn exercise(role: &str, explicit_stop: bool) {
    let directory = tempfile::tempdir().unwrap();
    let ready = directory.path().join("ready");
    let mut command = helper(role, &ready);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = Tree::spawn(&mut command).unwrap();
    assert!(child.id() > 0);
    let mut stdout = child.stdout.take().unwrap();
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        let _ = send.send(text);
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.is_file() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.is_file(), "descendant started");
    if role == "parent-exits" {
        while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(child.try_wait().unwrap().is_some());
    }
    assert!(receive.try_recv().is_err(), "descendant retains stdout");
    if explicit_stop {
        child.stop();
    }
    drop(child);
    receive
        .recv_timeout(Duration::from_secs(5))
        .expect("complete tree closed stdout");
}

#[test]
fn dropping_a_command_stops_its_descendants() {
    exercise("parent-waits", false);
}

#[test]
fn stopping_a_finished_command_stops_its_surviving_descendants() {
    exercise("parent-exits", true);
}

#[cfg(windows)]
#[test]
fn npm_style_cmd_shims_keep_arguments_literal() {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("synthetic shim.cmd");
    std::fs::write(&script, "@echo off\r\necho %1\r\n").unwrap();
    let mut command = Command::new(script);
    command
        .arg("words & whoami")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = Tree::spawn(&mut command).unwrap();
    let mut text = String::new();
    child.stdout.take().unwrap().read_to_string(&mut text).unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(text.trim(), "\"words & whoami\"");
}

#[test]
fn a_missing_command_returns_its_launch_failure() {
    let directory = tempfile::tempdir().unwrap();
    let mut command = Command::new(directory.path().join("missing-synthetic-program.exe"));
    assert_eq!(
        Tree::spawn(&mut command).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
}
