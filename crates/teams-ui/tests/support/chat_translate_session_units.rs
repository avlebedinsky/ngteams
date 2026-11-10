//! A chat's translating session against a fake `claude` (synthetic messages only): a batch and an outgoing message
//! answered in order, the process renewed once worn, and a failure told with its job, the next job starting the tool
//! anew.

use crate::fake_tool;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::*;

/// A fake Claude Code: logs its start and each question beside itself; answers a batch's first message, an outgoing
/// message, and anything holding "die" by exiting.
const FAKE: &str = r#"#!/bin/bash
log="$(dirname "$0")/received.log"
echo "start $$" >> "$log"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  case "$line" in
    *die*) echo 'Synthetic crash' >&2; exit 3;;
    *'<translate>'*) printf '{"type":"result","is_error":false,"result":"<m id=\\"1\\" lang=\\"en\\">DE one</m>"}\n';;
    *'<outgoing'*) printf '{"type":"result","is_error":false,"result":"<out lang=\\"en\\">EN two</out>"}\n';;
    *) printf '{"type":"result","is_error":false,"result":"nonsense"}\n';;
  esac
done
"#;

fn fake(dir: &Path) -> PathBuf {
    fake_tool::write(dir, "claude", FAKE)
}

fn next(session: &Session) -> Outcome {
    let outcomes = session.outcomes();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(outcome) = outcomes.try_recv() {
            return outcome;
        }
        assert!(Instant::now() < deadline, "the session said nothing");
        thread::sleep(Duration::from_millis(10));
    }
}

fn batch(id: u64, text: &str) -> Job {
    Job::Batch {
        id,
        context: vec![Said {
            from: "Lab B".to_owned(),
            text: "Earlier".to_owned(),
        }],
        items: vec![Item {
            n: 1,
            from: "Lab B".to_owned(),
            text: text.to_owned(),
        }],
    }
}

#[test]
fn jobs_are_answered_in_order_and_a_worn_process_is_renewed() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(Spec {
        backend: Backend::Claude,
        program: fake(dir.path()),
        model: Model::Recommended,
        language: "German",
        limits: Limits {
            turns: 2,
            chars: 1_000_000,
            first: Duration::from_secs(10),
            turn: Duration::from_secs(10),
        },
    });
    session.send(batch(1, "one"));
    match next(&session) {
        Outcome::Batch {
            id: 1,
            answers: Ok(answers),
        } => {
            assert_eq!(
                answers.get(&1),
                Some(&Answer::Translated {
                    text: "DE one".to_owned(),
                    source: Some("en".to_owned()),
                })
            );
        }
        other => panic!("{other:?}"),
    }
    session.send(Job::Outgoing {
        id: 2,
        context: Vec::new(),
        to: Some("en".to_owned()),
        text: "zwei".to_owned(),
    });
    match next(&session) {
        Outcome::Outgoing { id: 2, answer } => {
            assert_eq!(answer, Ok((Some("EN two".to_owned()), Some("en".to_owned()))));
        }
        other => panic!("{other:?}"),
    }
    // the third question goes to a new process
    session.send(Job::Outgoing {
        id: 3,
        context: Vec::new(),
        to: None,
        text: "x".to_owned(),
    });
    match next(&session) {
        Outcome::Outgoing { id: 3, answer: Ok(_) } => {}
        other => panic!("{other:?}"),
    }
    let log = fs::read_to_string(dir.path().join("received.log")).unwrap();
    assert_eq!(log.matches("start ").count(), 2, "{log}");
    assert!(log.contains("<context>") && log.contains("Earlier"));
    drop(session);
}

#[test]
fn a_failure_is_told_with_its_job_and_the_next_job_starts_the_tool_anew() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(Spec {
        backend: Backend::Claude,
        program: fake(dir.path()),
        model: Model::Recommended,
        language: "German",
        limits: Limits {
            turns: 100,
            chars: 1_000_000,
            first: Duration::from_secs(10),
            turn: Duration::from_secs(10),
        },
    });
    session.send(batch(1, "please die"));
    match next(&session) {
        Outcome::Batch {
            id: 1,
            answers: Err(Failure::Stopped(Backend::Claude, Some(why))),
        } => {
            assert!(why.contains("Synthetic crash"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    session.send(Job::Outgoing {
        id: 2,
        context: Vec::new(),
        to: Some("en".to_owned()),
        text: "nonsense please".to_owned(),
    });
    match next(&session) {
        Outcome::Outgoing { id: 2, answer } => assert_eq!(answer.map(|(text, _)| text), Ok(Some("EN two".to_owned()))),
        other => panic!("{other:?}"),
    }
    // a tool that isn't there fails its job
    let missing = Session::start(Spec {
        backend: Backend::Claude,
        program: dir.path().join("no-such-claude"),
        model: Model::Recommended,
        language: "German",
        limits: Limits::default(),
    });
    missing.send(batch(5, "one"));
    match next(&missing) {
        Outcome::Batch {
            id: 5,
            answers: Err(Failure::NotInstalled(Some(Backend::Claude))),
        } => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(
        not_a_translation(Backend::Codex).message(),
        "The Codex CLI stopped: its answer wasn't a translation."
    );
}
