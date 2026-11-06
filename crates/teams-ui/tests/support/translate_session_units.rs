//! The translating tool's session against fake `claude` and `codex` scripts (synthetic captions only): what reaches
//! the tool, the call's context included, and how each failure ends it.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::Value;

use super::*;
use crate::translate::LineKey;

/// A fake Claude Code: logs each message it gets (beside itself, in the test's own directory), answers with the
/// caption's text after `DE `, and the glossary request with a synthetic glossary.
const FAKE_CLAUDE: &str = r#"
log="$(dirname "$0")/received.log"
echo "start $$" >> "$log"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  echo '{"type":"system","subtype":"init"}'
  echo 'not json'
  text=$(printf '%s' "$line" | sed -n 's/.*<caption[^>]*>\(.*\)<\/caption>.*/\1/p')
  case "$line" in *glossary-request*) text="Acme = Acme";; esac
  printf '{"type":"result","subtype":"success","is_error":false,"result":"DE %s"}\n' "$text"
done
"#;

/// A fake Codex app server: answers `initialize` and `thread/start`, asks one approval (which must be refused) and
/// answers the first turn; refuses every later turn with Codex's words for a model it doesn't have.
const FAKE_CODEX: &str = r#"
log="$(dirname "$0")/received.log"
turns=0
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) echo "{\"id\":$id,\"result\":{}}";;
    *'"method":"thread/start"'*) echo "{\"id\":$id,\"result\":{\"thread\":{\"id\":\"synthetic-thread\"}}}";;
    *'"method":"turn/start"'*)
      turns=$((turns+1))
      echo "{\"id\":$id,\"result\":{\"turn\":{}}}"
      if [ $turns -eq 1 ]; then
        echo '{"id":99,"method":"item/commandExecution/requestApproval","params":{}}'
        text=$(printf '%s' "$line" | sed -n 's/.*<caption[^>]*>\(.*\)<\/caption>.*/\1/p')
        echo '{"method":"item/completed","params":{"item":{"type":"agentMessage","text":"DE '"$text"'"}}}'
        echo '{"method":"turn/completed","params":{"turn":{"status":"completed"}}}'
      else
        echo '{"method":"error","params":{"error":{"message":"The '"'"'luna'"'"' model is not supported when using Codex with a ChatGPT account."},"willRetry":false}}'
        echo '{"method":"turn/completed","params":{"turn":{"status":"failed"}}}'
      fi;;
  esac
done
"#;

/// A script `name` in `dir`, put there by a child: written by this process, it couldn't be run (`ETXTBSY`) while
/// another test's child, forked before the write's descriptor closed, hadn't reached its own `exec`.
fn fake(dir: &Path, name: &str, body: &str) -> PathBuf {
    let source = dir.join(format!("{name}.source"));
    fs::write(&source, format!("#!/bin/bash\n{body}")).unwrap();
    let path = dir.join(name);
    let installed = std::process::Command::new("install")
        .args(["-m", "755"])
        .arg(&source)
        .arg(&path)
        .status()
        .unwrap();
    assert!(installed.success(), "{name} installed");
    path
}

fn spec(backend: Backend, program: PathBuf, limits: Limits) -> Spec {
    Spec {
        backend,
        program,
        model: Model::Recommended,
        language: "German",
        limits,
    }
}

fn quick() -> Limits {
    Limits {
        turns: 100,
        chars: 100_000,
        first: Duration::from_secs(10),
        turn: Duration::from_secs(10),
    }
}

fn caption(speaker: &str, text: &str, stamp: u64) -> Job {
    let line = teams_core::Caption {
        id: format!("synthetic/{stamp}"),
        speaker: Some(format!("8:orgid:{speaker}")),
        display_name: Some(speaker.to_owned()),
        text: text.to_owned(),
        final_result: true,
        spoken_language: "en-us".into(),
        track_language: "en-us".into(),
        audio_timestamp: stamp,
        duration: 10,
    };
    Job {
        key: LineKey::of(&line),
        source: text.to_owned(),
        final_result: true,
        speaker: Some(speaker.to_owned()),
        said: Vec::new(),
        brief: None,
    }
}

/// The session's next outcome, failing the test after a while.
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

fn translated(session: &Session) -> String {
    match next(session) {
        Outcome::Translated { text, .. } => text,
        Outcome::Failed(failure) => panic!("failed: {failure:?}"),
    }
}

fn failed(session: &Session) -> Failure {
    match next(session) {
        Outcome::Failed(failure) => failure,
        Outcome::Translated { .. } => panic!("translated"),
    }
}

fn received(dir: &Path) -> String {
    fs::read_to_string(dir.join("received.log")).unwrap_or_default()
}

#[test]
fn claude_gets_the_whole_call_as_context_and_a_renewed_process_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake(dir.path(), "claude", FAKE_CLAUDE);
    let limits = Limits { turns: 3, ..quick() };
    let session = Session::start(spec(Backend::Claude, program, limits));

    let mut first = caption("Lab Speaker", "Acme ships on Monday.", 1);
    first.brief = Some("Title: Synthetic standup\nIn the call: Lab Speaker, Lab Other".into());
    first.said = vec![Said {
        speaker: Some("Lab Other".into()),
        text: "Earlier synthetic words.".into(),
    }];
    session.send(first);
    assert_eq!(translated(&session), "DE Acme ships on Monday.");
    for (stamp, text) in [(2, "Second synthetic line."), (3, "Third synthetic line.")] {
        session.send(caption("Lab Speaker", text, stamp));
        assert_eq!(translated(&session), format!("DE {text}"));
    }
    // the fourth caption goes to a new process: the old one is asked for its glossary first
    session.send(caption("Lab Other", "Fourth synthetic line.", 4));
    assert_eq!(translated(&session), "DE Fourth synthetic line.");
    the_call_reached_both_processes(&received(dir.path()));
}

/// What the fake Claude got: the call's brief, what was said and each caption; then the glossary request; then, in
/// the new process, the brief, the glossary and the earlier captions with their translations.
fn the_call_reached_both_processes(log: &str) {
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.iter().filter(|line| line.starts_with("start ")).count(), 2);
    let messages: Vec<Value> = lines
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect();
    let content = |index: usize| {
        messages[index]
            .pointer("/message/content")
            .and_then(Value::as_str)
            .unwrap()
            .to_owned()
    };
    assert_eq!(messages[0]["type"], "user");
    let opening = content(0);
    assert!(opening.contains("<call>\nTitle: Synthetic standup\nIn the call: Lab Speaker, Lab Other\n</call>"));
    assert!(opening.contains("<said>\nLab Other: Earlier synthetic words.\n</said>"));
    assert!(opening.ends_with("<caption from=\"Lab Speaker\">Acme ships on Monday.</caption>"));
    // the brief isn't sent again while it holds: the process's conversation has it
    assert!(!content(1).contains("<call>"));
    assert_eq!(content(3), GLOSSARY_REQUEST);
    let renewed = content(4);
    assert!(renewed.contains("<call>\nTitle: Synthetic standup"));
    assert!(renewed.contains("<glossary>\nDE Acme = Acme\n</glossary>"));
    assert!(renewed.contains("<earlier>\nLab Other: Earlier synthetic words.\nLab Speaker: Acme ships on Monday.\n=> DE Acme ships on Monday.\n"));
    assert!(renewed.contains("Lab Speaker: Third synthetic line.\n=> DE Third synthetic line.\n</earlier>"));
    assert!(renewed.ends_with("<caption from=\"Lab Other\">Fourth synthetic line.</caption>"));
}

#[test]
fn claude_runs_without_tools_settings_or_saved_sessions() {
    let args = Backend::Claude.args(Work::Captions, &Model::Recommended, "Synthetic instructions.");
    for flag in [
        "--tools",
        "--setting-sources",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--no-session-persistence",
    ] {
        assert!(args.iter().any(|arg| arg == flag), "{flag}");
    }
    assert_eq!(args.last().map(String::as_str), Some("Synthetic instructions."));
    let codex = Backend::Codex.args(Work::Captions, &Model::Recommended, "unused");
    assert_eq!(codex[0], "app-server");
    assert!(codex.iter().any(|arg| arg == "features.hooks=false"));
    assert!(!codex.iter().any(|arg| arg == "unused"));
}

#[test]
fn claude_is_asked_for_the_model_picked_or_none_for_its_own_default() {
    let model_of = |model: &Model| {
        let args = Backend::Claude.args(Work::Captions, model, "Synthetic instructions.");
        let at = args.iter().position(|arg| arg == "--model");
        (at.map(|at| args[at + 1].clone()), args.last().cloned())
    };
    let instructions = Some("Synthetic instructions.".to_owned());
    assert_eq!(
        model_of(&Model::Recommended),
        (Some("haiku".into()), instructions.clone())
    );
    assert_eq!(
        model_of(&Model::Named("claude-opus-5-5".into())),
        (Some("claude-opus-5-5".into()), instructions.clone())
    );
    assert_eq!(model_of(&Model::ToolDefault), (None, instructions));
    let summary = Backend::Claude.args(Work::Summary, &Model::Recommended, "unused");
    assert!(summary.windows(2).any(|pair| pair == ["--model", "sonnet"]));
    // Codex takes its model in its thread
    let codex = Backend::Codex.args(Work::Captions, &Model::Named("gpt-6.1-sol".into()), "unused");
    assert!(!codex.iter().any(|arg| arg.contains("gpt-6.1-sol")));
}

#[test]
fn a_fake_claude_runs_with_the_model_picked() {
    let dir = tempfile::tempdir().unwrap();
    let body = format!("echo \"args $*\" >> \"$(dirname \"$0\")/received.log\"\n{FAKE_CLAUDE}");
    let program = fake(dir.path(), "claude", &body);
    let mut spec = spec(Backend::Claude, program, quick());
    spec.model = Model::Named("opus".into());
    let session = Session::start(spec);
    session.send(caption("Lab Speaker", "Synthetic hello.", 1));
    assert_eq!(translated(&session), "DE Synthetic hello.");
    let log = received(dir.path());
    let args = log.lines().find(|line| line.starts_with("args ")).unwrap();
    assert!(args.starts_with("args -p --model opus --input-format"), "{args}");
}

#[test]
fn codex_starts_its_thread_with_the_model_picked_or_its_own_default() {
    for (model, asked) in [
        (Model::Named("gpt-6.1-sol".into()), Some("gpt-6.1-sol")),
        (Model::ToolDefault, None),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let program = fake(dir.path(), "codex", FAKE_CODEX);
        let mut spec = spec(Backend::Codex, program, quick());
        spec.model = model;
        let session = Session::start(spec);
        session.send(caption("Lab Speaker", "Synthetic hello.", 1));
        assert_eq!(translated(&session), "DE Synthetic hello.");
        let log = received(dir.path());
        let start = log
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|message| message["method"] == "thread/start")
            .unwrap();
        assert_eq!(start["params"].get("model").and_then(Value::as_str), asked);
    }
}

#[test]
fn codex_starts_its_thread_refuses_approvals_and_reports_a_model_it_lacks() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake(dir.path(), "codex", FAKE_CODEX);
    let session = Session::start(spec(Backend::Codex, program, quick()));
    session.send(caption("Lab Speaker", "Synthetic hello.", 1));
    assert_eq!(translated(&session), "DE Synthetic hello.");
    session.send(caption("Lab Speaker", "Synthetic again.", 2));
    assert_eq!(
        failed(&session),
        Failure::Stopped(
            Backend::Codex,
            Some("The 'luna' model is not supported when using Codex with a ChatGPT account.".into())
        )
    );
    let log = received(dir.path());
    let messages: Vec<Value> = log.lines().filter_map(|line| serde_json::from_str(line).ok()).collect();
    assert_eq!(messages[0]["method"], "initialize");
    assert_eq!(messages[1]["method"], "initialized");
    let start = &messages[2];
    assert_eq!(start["method"], "thread/start");
    assert_eq!(start["params"]["model"], "gpt-6-luna");
    assert_eq!(start["params"]["ephemeral"], true);
    assert_eq!(start["params"]["sandbox"], "read-only");
    assert!(
        start["params"]["baseInstructions"]
            .as_str()
            .unwrap()
            .contains("into German")
    );
    let turn = &messages[3];
    assert_eq!(turn["params"]["threadId"], "synthetic-thread");
    assert!(
        turn["params"]["input"][0]["text"]
            .as_str()
            .unwrap()
            .ends_with("<caption from=\"Lab Speaker\">Synthetic hello.</caption>")
    );
    assert_eq!(messages[4]["id"], 99);
    assert_eq!(messages[4]["error"]["code"], -32601);
}

#[test]
fn a_refusal_naming_a_sign_in_says_how_to_sign_in() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake(
        dir.path(),
        "claude",
        r#"read -r line; echo '{"type":"result","is_error":true,"result":"Invalid API key · Please run /login"}'"#,
    );
    let session = Session::start(spec(Backend::Claude, program, quick()));
    session.send(caption("Lab Speaker", "Synthetic.", 1));
    let failure = failed(&session);
    assert_eq!(failure, Failure::SignedOut(Backend::Claude));
    assert_eq!(
        failure.message(),
        "Claude Code isn't signed in. Run claude in a terminal, then try again."
    );
}

#[test]
fn a_tool_that_says_nothing_times_out() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake(dir.path(), "claude", "while read -r line; do :; done");
    let limits = Limits {
        first: Duration::from_millis(300),
        ..quick()
    };
    let session = Session::start(spec(Backend::Claude, program, limits));
    session.send(caption("Lab Speaker", "Synthetic.", 1));
    assert_eq!(failed(&session), Failure::TimedOut(Backend::Claude));
}

#[test]
fn a_tool_that_exits_is_told_by_its_last_words() {
    let dir = tempfile::tempdir().unwrap();
    let signed_out = fake(dir.path(), "signed-out", "echo 'Error: not logged in' >&2; exit 1");
    let session = Session::start(spec(Backend::Codex, signed_out, quick()));
    assert_eq!(failed(&session), Failure::SignedOut(Backend::Codex));
    let broken = fake(dir.path(), "broken", "read -r line; echo 'synthetic crash' >&2; exit 3");
    let session = Session::start(spec(Backend::Claude, broken, quick()));
    session.send(caption("Lab Speaker", "Synthetic.", 1));
    let failure = failed(&session);
    assert_eq!(
        failure,
        Failure::Stopped(Backend::Claude, Some("synthetic crash".into()))
    );
    assert_eq!(failure.message(), "Claude Code stopped: synthetic crash");
    let silent = fake(dir.path(), "silent", "read -r line; exit 3");
    let session = Session::start(spec(Backend::Claude, silent, quick()));
    session.send(caption("Lab Speaker", "Synthetic.", 1));
    assert_eq!(failed(&session), Failure::Stopped(Backend::Claude, None));
}

#[test]
fn a_missing_tool_is_not_installed() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(spec(Backend::Claude, dir.path().join("claude"), quick()));
    assert_eq!(failed(&session), Failure::NotInstalled(Some(Backend::Claude)));
    assert_eq!(
        Failure::NotInstalled(Some(Backend::Codex)).message(),
        "The Codex CLI (codex) isn't installed."
    );
    assert!(Failure::NotInstalled(None).message().starts_with("Neither Claude Code"));
    assert_eq!(
        Failure::TimedOut(Backend::Codex).message(),
        "The Codex CLI took too long to answer."
    );
    assert_eq!(
        Failure::Stopped(Backend::Codex, None).message(),
        "The Codex CLI stopped unexpectedly."
    );
}

#[test]
fn dropping_the_session_ends_its_process() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake(dir.path(), "claude", FAKE_CLAUDE);
    let session = Session::start(spec(Backend::Claude, program, quick()));
    session.send(caption("Lab Speaker", "Synthetic.", 1));
    assert_eq!(translated(&session), "DE Synthetic.");
    let log = received(dir.path());
    let pid = log
        .lines()
        .find_map(|line| line.strip_prefix("start "))
        .unwrap()
        .to_owned();
    drop(session);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Path::new(&format!("/proc/{pid}")).exists() {
        assert!(Instant::now() < deadline, "the tool still runs");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn messages_mark_unfinished_captions_and_keep_their_tags_their_own() {
    let mut memory = Memory::default();
    let mut job = caption("Lab \"Quoted\" Speaker", "a <b> c", 1);
    job.final_result = false;
    assert_eq!(
        memory.message(&job, false),
        "<caption from=\"Lab 'Quoted' Speaker\" unfinished>a ‹b› c</caption>"
    );
    job.speaker = None;
    assert!(memory.message(&job, false).starts_with("<caption from=\"Speaker\""));
}

#[test]
fn the_recent_captions_kept_are_bounded() {
    let mut memory = Memory::default();
    for stamp in 0..100 {
        memory.remember(&caption("Lab Speaker", "Synthetic line.", stamp), "DE Synthetic line.");
    }
    assert_eq!(memory.recent.len(), RECENT_LINES);
    memory.remember(&caption("Lab Speaker", &"x".repeat(RECENT_CHARS - 10), 200), "y");
    assert_eq!(memory.recent.len(), 1);
    // an unfinished caption isn't kept, what was said with it is
    let mut job = caption("Lab Speaker", "Unfinished", 300);
    job.final_result = false;
    job.said = vec![Said {
        speaker: None,
        text: "Said.".into(),
    }];
    memory.remember(&job, "DE");
    assert_eq!(
        memory
            .recent
            .back()
            .map(|(_, text, shown)| (text.as_str(), shown.is_none())),
        Some(("Said.", true))
    );
}

#[test]
fn answers_lose_a_repeated_caption_tag_and_are_capped() {
    assert_eq!(answer("  <caption from=\"x\">Hallo.</caption> "), "Hallo.");
    assert_eq!(answer("Hallo."), "Hallo.");
    assert_eq!(answer(&"ä".repeat(MAX_ANSWER)).len(), MAX_ANSWER);
}

/// The real tools, when installed: `cargo test -p teams-ui --lib translate::session -- --ignored --nocapture`. They
/// send a synthetic sentence to the tool's provider.
#[test]
#[ignore = "runs the installed claude and codex, which reach their providers"]
fn the_installed_tools_translate() {
    let translators = crate::translate::Translators::detect();
    for backend in translators.installed() {
        let (_, program) = translators
            .pick(crate::translate::TranslatorChoice::Only(backend))
            .unwrap();
        let session = Session::start(spec(backend, program, Limits::default()));
        let mut first = caption(
            "Lab Speaker",
            "Good morning, everyone. Let's look at the Acme numbers.",
            1,
        );
        first.brief = Some("Title: Synthetic review\nIn the call: Lab Speaker, Lab Other".into());
        let started = Instant::now();
        session.send(first);
        println!(
            "{backend:?} first: {:?} in {:?}",
            translated(&session),
            started.elapsed()
        );
        let started = Instant::now();
        session.send(caption("Lab Other", "The Acme numbers look good.", 2));
        println!(
            "{backend:?} second: {:?} in {:?}",
            translated(&session),
            started.elapsed()
        );
    }
}
