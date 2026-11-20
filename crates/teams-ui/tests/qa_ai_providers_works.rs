//! QA of task 516 (round 1): the two AI works that the app's window cannot reach on the mock (it has no captions and
//! no recaps), through an AI provider's API: a call's captions translated (`translate::session`) and a meeting's
//! summary (`recap_summary::run`), each with the provider's target as the settings pick it. A fake server on the
//! loopback speaks the Anthropic Messages API and OpenAI's Chat Completions (streamed server-sent events). Every key,
//! model and text is synthetic; nothing leaves the loopback.

#![cfg(test)]

use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Read as _, Write as IoWrite};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_ui::local_ai::providers::{ApiTarget, MemoryKeys};
use teams_ui::local_ai::{AiKeys, Backend, Failure, Model, Target};
use teams_ui::recap_summary::{Input, Line, Request, Summarizing, Update};
use teams_ui::translate::LineKey;
use teams_ui::translate::session::{Job, Limits, Outcome, Session, Spec};

const KEY: &str = "sk-qa516-synthetic-works-0001";
const SUMMARY: &str = r#"{"purpose": "QA summary of the budget", "takeaways": ["The budget is ready"], "topics": [{"title": "Budget", "points": [{"text": "Ana has the budget", "at": "0:05"}]}], "next_steps": [{"owner": "Ana", "text": "Send the budget", "at": "0:05"}], "questions": []}"#;

/// One request the fake server got: its path, its head's lines (names in lower case) and its body.
#[derive(Clone, Debug)]
struct Seen {
    path: String,
    headers: Vec<(String, String)>,
    body: Value,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.as_str())
    }
}

/// The fake server: `/ok` answers, `/nocredit` says that the account has no credit; a model `no-such-model` is not
/// known. It keeps each request.
struct Fake {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Fake {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let held = Arc::clone(&seen);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let held = Arc::clone(&held);
                thread::spawn(move || serve(stream, &held));
            }
        });
        Self { base, seen }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

fn serve(mut stream: TcpStream, seen: &Mutex<Vec<Seen>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    let path = first.split_whitespace().nth(1).unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim_end().is_empty() {
            break;
        }
        if let Some((name, value)) = line.trim_end().split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<u64>().ok())
        .unwrap_or(0);
    let mut body = Vec::new();
    (&mut reader).take(length).read_to_end(&mut body).unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let request = Seen { path, headers, body };
    seen.lock().unwrap().push(request.clone());
    let anthropic = request.path.ends_with("/v1/messages");
    let refusal = |status: u16, error: Value| {
        format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{error}",
            error.to_string().len()
        )
    };
    let reply = if request.path.starts_with("/nocredit") {
        refusal(
            429,
            json!({"error": {"message": "You exceeded your current quota.", "type": "insufficient_quota",
                   "code": "insufficient_quota"}}),
        )
    } else if request.body["model"] == "no-such-model" {
        refusal(
            404,
            json!({"type": "error", "error": {"type": "not_found_error", "message": "model: no-such-model"}}),
        )
    } else {
        let text = answer(&request.body, anthropic);
        let mut events = String::new();
        if anthropic {
            for (name, data) in [
                (
                    "message_start",
                    json!({"type": "message_start", "message": {"id": "qa", "content": []}}),
                ),
                (
                    "content_block_delta",
                    json!({"type": "content_block_delta", "index": 0,
                                               "delta": {"type": "text_delta", "text": text}}),
                ),
                ("message_stop", json!({"type": "message_stop"})),
            ] {
                let _ = write!(events, "event: {name}\ndata: {data}\n\n");
            }
        } else {
            let chunk = json!({"choices": [{"index": 0, "delta": {"content": text}}]});
            let _ = write!(events, "data: {chunk}\n\ndata: [DONE]\n\n");
        }
        format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{events}")
    };
    let _ = IoWrite::write_all(&mut stream, reply.as_bytes());
}

/// What the fake model answers: a caption in "German", a summary as the recap asks for it.
fn answer(body: &Value, anthropic: bool) -> String {
    let turns = body["messages"].as_array().cloned().unwrap_or_default();
    let question = turns
        .last()
        .and_then(|turn| turn["content"].as_str())
        .unwrap_or_default()
        .to_owned();
    let instructions = if anthropic {
        body["system"].as_str().unwrap_or_default().to_owned()
    } else {
        turns
            .first()
            .and_then(|turn| turn["content"].as_str())
            .unwrap_or_default()
            .to_owned()
    };
    if question.contains("<glossary-request/>") {
        return "none".to_owned();
    }
    if instructions.contains("Fathom") {
        return SUMMARY.to_owned();
    }
    if let Some(start) = question.find("<caption") {
        let rest = &question[start..];
        let text = rest.split_once('>').map_or("", |(_, text)| text);
        let text = text.split("</caption>").next().unwrap_or_default();
        return format!("DE {text}");
    }
    "?".to_owned()
}

fn api(base: &str, model: Option<&str>, has_key: bool) -> Target {
    let keys = AiKeys::new(Arc::new(MemoryKeys::default()));
    keys.put("api-1", &Secret::new(KEY)).unwrap();
    Target::Api(ApiTarget {
        provider: "api-1".to_owned(),
        name: "QA API".to_owned(),
        base_url: base.to_owned(),
        model: model.map(str::to_owned),
        has_key,
        keys,
    })
}

fn caption(text: &str, stamp: u64) -> Job {
    let line = teams_core::Caption {
        id: format!("synthetic/{stamp}"),
        speaker: Some("8:orgid:00000000-0000-4000-8000-0000000000aa".to_owned()),
        display_name: Some("Ana".to_owned()),
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
        speaker: Some("Ana".to_owned()),
        said: Vec::new(),
        brief: None,
    }
}

fn next(session: &Session) -> Outcome {
    let outcomes = session.outcomes();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(outcome) = outcomes.try_recv() {
            return outcome;
        }
        assert!(Instant::now() < deadline, "no outcome");
        thread::sleep(Duration::from_millis(10));
    }
}

fn captions(backend: Backend, target: Target) -> Session {
    Session::start(Spec {
        backend,
        target,
        model: Model::Recommended,
        language: "German",
        limits: Limits::default(),
    })
}

#[test]
fn captions_go_to_the_anthropic_api_with_its_fast_model() {
    let fake = Fake::start();
    let session = captions(Backend::Anthropic, api(&format!("{}/ok", fake.base), None, true));
    session.send(caption("the budget is ready", 1));
    match next(&session) {
        Outcome::Translated { text, .. } => assert_eq!(text, "DE the budget is ready"),
        Outcome::Failed(failure) => panic!("a caption failed: {}", failure.message()),
    }
    let seen = fake.seen();
    let asked = seen
        .iter()
        .find(|seen| seen.path == "/ok/v1/messages")
        .expect("a messages request");
    assert_eq!(asked.header("x-api-key"), Some(KEY));
    assert_eq!(asked.body["model"], "claude-haiku-5-5");
    assert_eq!(asked.body["output_config"]["effort"], "low");
    assert!(asked.body["system"].as_str().unwrap().contains("live captions"));
}

#[test]
fn captions_through_an_openai_account_without_credit_say_so() {
    let fake = Fake::start();
    let session = captions(Backend::OpenAi, api(&format!("{}/nocredit/v1", fake.base), None, true));
    session.send(caption("the budget is ready", 1));
    match next(&session) {
        Outcome::Failed(failure) => {
            assert_eq!(failure, Failure::NoCredit(Backend::OpenAi).of("QA API"));
            assert!(
                failure
                    .message()
                    .starts_with("QA API says that the account has no credit left"),
                "{}",
                failure.message()
            );
        }
        Outcome::Translated { text, .. } => panic!("translated without credit: {text}"),
    }
}

fn input() -> Input {
    Input {
        brief: "Budget review, 2026-11-17, Ana and Ben".to_owned(),
        lines: vec![
            Line {
                at: 5,
                speaker: Some("Ana".to_owned()),
                text: "The budget is ready, I send it today.".to_owned(),
            },
            Line {
                at: 12,
                speaker: Some("Ben".to_owned()),
                text: "Thanks, Ana.".to_owned(),
            },
        ],
    }
}

fn summarize(backend: Backend, target: Target, model: Model) -> Update {
    let job = Summarizing::start(Request {
        backend,
        target,
        model,
        input: input(),
        limits: teams_ui::recap_summary::Limits::default(),
    });
    let updates = job.updates();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(update) = updates.try_recv()
            && matches!(update, Update::Done(_) | Update::Failed(_))
        {
            return update;
        }
        assert!(Instant::now() < deadline, "the summary never ended");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_recap_summary_comes_from_the_openai_api_with_its_strong_model() {
    let fake = Fake::start();
    let update = summarize(
        Backend::OpenAi,
        api(&format!("{}/ok/v1", fake.base), None, true),
        Model::Recommended,
    );
    let Update::Done(made) = update else {
        panic!("no summary: {update:?}")
    };
    assert_eq!(made.summary.purpose, "QA summary of the budget");
    assert_eq!(made.summary.next_steps.len(), 1);
    assert_eq!(made.tool, "openai");
    let seen = fake.seen();
    let asked = seen
        .iter()
        .find(|seen| seen.path == "/ok/v1/chat/completions")
        .expect("a completions request");
    assert_eq!(asked.header("authorization"), Some(format!("Bearer {KEY}").as_str()));
    assert_eq!(asked.body["model"], "gpt-6.1-terra");
    assert_eq!(asked.body["reasoning_effort"], "medium");
    let transcript = asked.body["messages"][1]["content"].as_str().unwrap();
    assert!(transcript.contains("Ana: The budget is ready"), "{transcript}");
}

#[test]
fn a_recap_summary_from_a_compatible_server_takes_its_default_model_and_no_key() {
    let fake = Fake::start();
    let target = api(&format!("{}/ok/v1", fake.base), Some("local-qa-1"), false);
    let update = summarize(Backend::Compatible, target, Model::Recommended);
    let Update::Done(made) = update else {
        panic!("no summary: {update:?}")
    };
    assert_eq!(made.summary.purpose, "QA summary of the budget");
    let seen = fake.seen();
    let asked = seen
        .iter()
        .find(|seen| seen.path == "/ok/v1/chat/completions")
        .expect("a completions request");
    assert_eq!(asked.body["model"], "local-qa-1");
    assert!(asked.header("authorization").is_none());
    assert!(asked.body.get("reasoning_effort").is_none());
}

#[test]
fn a_recap_summary_with_a_model_the_api_does_not_know_says_so() {
    let fake = Fake::start();
    let target = api(&format!("{}/ok", fake.base), None, true);
    let update = summarize(Backend::Anthropic, target, Model::Named("no-such-model".to_owned()));
    let Update::Failed(failure) = update else {
        panic!("a summary with an unknown model: {update:?}")
    };
    assert_eq!(
        failure,
        Failure::UnknownModel(Backend::Anthropic, "no-such-model".to_owned()).of("QA API")
    );
    assert!(
        failure
            .message()
            .starts_with("QA API does not know the model no-such-model")
    );
    assert!(!failure.message().contains(KEY));
}
