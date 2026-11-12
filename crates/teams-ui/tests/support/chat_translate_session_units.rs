//! A chat's translating session against a fake `claude` (synthetic messages only): a batch and an outgoing message
//! answered in order, the process renewed once worn, and a failure told with its job, the next job starting the tool
//! anew.

use crate::fake_tool;

use std::collections::HashSet;
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

/// A fake Claude Code that streams a batch's answer (`--include-partial-messages`), logging its arguments: the first
/// message's answer, the second's cut mid-way, then the rest; a new message starting over what came before.
const STREAMING_CLAUDE: &str = r#"#!/bin/bash
printf 'args %s\n' "$*" >> "$(dirname "$0")/received.log"
delta() { printf '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"%s"}}}\n' "$1"; }
while IFS= read -r line; do
  echo '{"type":"stream_event","event":{"type":"message_start"}}'
  delta '<m id=\"1\" lang=\"en\">DE one</m>'
  delta '\n<m id=\"2\" lang=\"e'
  echo '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{}"}}}'
  delta 'n\">DE two</m><m id=\"3\" lang=\"de\" same/>'
  printf '{"type":"result","is_error":false,"result":"%s"}\n' '<m id=\"1\" lang=\"en\">DE one</m>\n<m id=\"2\" lang=\"en\">DE two</m><m id=\"3\" lang=\"de\" same/>'
done
"#;

/// A fake Codex app server that streams a turn's agent message as deltas.
const STREAMING_CODEX: &str = r#"#!/bin/bash
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  delta() { printf '{"method":"item/agentMessage/delta","params":{"itemId":"%s","delta":"%s"}}\n' "$1" "$2"; }
  case "$line" in
    *'"method":"initialize"'*) echo "{\"id\":$id,\"result\":{}}";;
    *'"method":"thread/start"'*) echo "{\"id\":$id,\"result\":{\"thread\":{\"id\":\"synthetic-thread\"}}}";;
    *'"method":"turn/start"'*)
      echo "{\"id\":$id,\"result\":{\"turn\":{}}}"
      delta b '<m id=\"1\" lang=\"en\">DE one</m>'
      delta b '<m id=\"2\" lang=\"en\">DE two</m>'
      echo '{"method":"item/completed","params":{"item":{"type":"agentMessage","text":"<m id=\"1\" lang=\"en\">DE one</m><m id=\"2\" lang=\"en\">DE two</m>"}}}'
      echo '{"method":"turn/completed","params":{"turn":{"status":"completed"}}}';;
  esac
done
"#;

fn three_items() -> Job {
    Job::Batch {
        id: 7,
        context: Vec::new(),
        items: (1..=3)
            .map(|n| Item {
                n,
                from: "Lab B".to_owned(),
                text: format!("message {n}"),
            })
            .collect(),
    }
}

/// The session's outcomes until the batch's whole answer, its partial answers' numbers in the order they came.
fn partials_then_batch(session: &Session) -> (Vec<Vec<usize>>, HashMap<usize, Answer>) {
    let mut partials = Vec::new();
    loop {
        match next(session) {
            Outcome::Partial { id: 7, answers } => {
                let mut numbers: Vec<usize> = answers.keys().copied().collect();
                numbers.sort_unstable();
                partials.push(numbers);
            }
            Outcome::Batch { id: 7, answers } => return (partials, answers.unwrap()),
            other => panic!("{other:?}"),
        }
    }
}

fn quick() -> Limits {
    Limits {
        turns: 100,
        chars: 1_000_000,
        first: Duration::from_secs(10),
        turn: Duration::from_secs(10),
    }
}

#[test]
fn claude_s_batch_answers_come_a_message_at_a_time_as_it_writes_them() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(Spec {
        backend: Backend::Claude,
        program: fake_tool::write(dir.path(), "claude", STREAMING_CLAUDE),
        model: Model::Recommended,
        language: "German",
        limits: quick(),
    });
    session.send(three_items());
    let (partials, whole) = partials_then_batch(&session);
    // the second came only once it was closed, the third with it
    assert_eq!(partials, [vec![1], vec![2, 3]]);
    assert_eq!(whole.len(), 3);
    assert_eq!(
        whole.get(&3),
        Some(&Answer::Same {
            source: Some("de".to_owned())
        })
    );
    let log = fs::read_to_string(dir.path().join("received.log")).unwrap();
    assert!(log.contains("--include-partial-messages"), "{log}");
}

#[test]
fn codex_s_batch_answers_come_from_its_message_deltas() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(Spec {
        backend: Backend::Codex,
        program: fake_tool::write(dir.path(), "codex", STREAMING_CODEX),
        model: Model::Recommended,
        language: "German",
        limits: quick(),
    });
    session.send(three_items());
    let (partials, whole) = partials_then_batch(&session);
    assert_eq!(partials, [vec![1], vec![2]]);
    assert_eq!(whole.len(), 2);
}

#[test]
fn only_closed_answers_are_fresh_and_each_once() {
    let mut told = HashSet::new();
    assert!(fresh_answers("<m id=\"1\" lang=\"en\">DE o", &mut told).is_empty());
    assert!(fresh_answers("<m id=\"1\" lang=\"en", &mut told).is_empty());
    let first = fresh_answers("<m id=\"1\" lang=\"en\">DE one</m>", &mut told);
    assert_eq!(first.keys().copied().collect::<Vec<_>>(), [1]);
    // still open after the first: nothing new
    assert!(fresh_answers("<m id=\"1\" lang=\"en\">DE one</m>\n<m id=\"2\">D", &mut told).is_empty());
    let second = fresh_answers(
        "<m id=\"1\" lang=\"en\">DE one</m>\n<m id=\"2\" lang=\"en\" same/>\n",
        &mut told,
    );
    assert_eq!(second.keys().copied().collect::<Vec<_>>(), [2]);
}
