//! The AI APIs' clients against a mock HTTP server on the loopback (synthetic keys, no network): each kind's request,
//! its streamed answer, the conversation kept, its failures, and the listing of its models.

use std::io::{BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use teams_auth::secrets::Secret;

use super::*;
use crate::local_ai::providers::{AiKeys, ApiTarget, MemoryKeys, Provider};
use crate::local_ai::{Launch, Target};

/// The synthetic key every test gives.
const KEY: &str = "sk-synthetic-test-key-0000";

/// A request the mock took.
#[derive(Clone, Debug)]
struct Taken {
    line: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Taken {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap()
    }
}

/// A mock HTTP server that answers each request with the next of `answers` (`status`, `content type`, `body`).
struct Mock {
    base: String,
    taken: Arc<Mutex<Vec<Taken>>>,
}

impl Mock {
    fn new(answers: Vec<(u16, &'static str, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let taken = Arc::new(Mutex::new(Vec::new()));
        let held = Arc::clone(&taken);
        thread::spawn(move || {
            for answer in answers {
                let Ok((stream, _)) = listener.accept() else { return };
                serve(stream, answer, &held);
            }
        });
        Self { base, taken }
    }

    fn taken(&self) -> Vec<Taken> {
        self.taken.lock().unwrap().clone()
    }
}

/// Takes one request from `stream` into `taken`, and answers it with `answer`.
fn serve(mut stream: TcpStream, answer: (u16, &'static str, String), taken: &Mutex<Vec<Taken>>) {
    let (status, kind, body) = answer;
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (key, value) = header.split_once(':').unwrap();
        headers.push((key.trim().to_owned(), value.trim().to_owned()));
    }
    let length = headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .map_or(0, |(_, value)| value.parse::<usize>().unwrap());
    let mut content = vec![0; length];
    reader.read_exact(&mut content).unwrap();
    taken.lock().unwrap().push(Taken {
        line: line.trim_end().to_owned(),
        headers,
        body: String::from_utf8(content).unwrap(),
    });
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

fn sse(events: &[&str]) -> String {
    events.iter().fold(String::new(), |mut text, event| {
        text.push_str("data: ");
        text.push_str(event);
        text.push_str("\n\n");
        text
    })
}

fn anthropic_answer(parts: &[&str]) -> (u16, &'static str, String) {
    let mut events = vec![r#"{"type":"message_start","message":{"id":"msg_synthetic"}}"#.to_owned()];
    for part in parts {
        events.push(format!(
            r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"{part}"}}}}"#
        ));
    }
    events.push(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#.to_owned());
    events.push(r#"{"type":"message_stop"}"#.to_owned());
    let events: Vec<&str> = events.iter().map(String::as_str).collect();
    (200, "text/event-stream", sse(&events))
}

fn openai_answer(parts: &[&str]) -> (u16, &'static str, String) {
    let mut events: Vec<String> = parts
        .iter()
        .map(|part| format!(r#"{{"choices":[{{"index":0,"delta":{{"content":"{part}"}}}}]}}"#))
        .collect();
    events.push("[DONE]".to_owned());
    let events: Vec<&str> = events.iter().map(String::as_str).collect();
    (200, "text/event-stream", sse(&events))
}

fn keys_with_key() -> AiKeys {
    let keys = AiKeys::new(Arc::new(MemoryKeys::default()));
    keys.put("api-1", &Secret::new(KEY)).unwrap();
    keys
}

fn target(base: &str, has_key: bool) -> ApiTarget {
    ApiTarget {
        provider: "api-1".to_owned(),
        base_url: base.to_owned(),
        model: None,
        has_key,
        keys: keys_with_key(),
    }
}

fn launch(backend: Backend, work: Work, model: Model, api: ApiTarget) -> Launch {
    Launch {
        backend,
        target: Target::Api(api),
        work,
        model,
        instructions: "Synthetic instructions.".to_owned(),
        first: Duration::from_secs(5),
    }
}

fn ask(launch: &Launch, message: &str) -> Result<String, Failure> {
    let Target::Api(api) = &launch.target else {
        unreachable!()
    };
    let mut conversation = Conversation::start(launch, api)?;
    let stop = AtomicBool::new(false);
    conversation.ask(message, Instant::now() + Duration::from_secs(10), &stop, &mut |_| {})
}

#[test]
fn anthropic_streams_its_answer_with_the_key_the_version_and_the_effort() {
    let mock = Mock::new(vec![anthropic_answer(&["Bon", "jour"]), anthropic_answer(&["Salut"])]);
    let base = mock.base.clone();
    let launch = launch(
        Backend::Anthropic,
        Work::Captions,
        Model::Recommended,
        target(&base, true),
    );
    let Target::Api(api) = &launch.target else {
        unreachable!()
    };
    let mut conversation = Conversation::start(&launch, api).unwrap();
    let stop = AtomicBool::new(false);
    let mut seen = Vec::new();
    let answer = conversation
        .ask(
            "Hello",
            Instant::now() + Duration::from_secs(10),
            &stop,
            &mut |so_far| {
                seen.push(so_far.to_owned());
            },
        )
        .unwrap();
    assert_eq!(answer, "Bonjour");
    assert_eq!(seen, ["Bon", "Bonjour"]);
    let second = conversation
        .ask("Hi", Instant::now() + Duration::from_secs(10), &stop, &mut |_| {})
        .unwrap();
    assert_eq!(second, "Salut");
    let taken = mock.taken();
    assert_eq!(taken[0].line, "POST /v1/messages HTTP/1.1");
    assert_eq!(taken[0].header("x-api-key"), Some(KEY));
    assert_eq!(taken[0].header("anthropic-version"), Some("2026-10-21"));
    assert_eq!(taken[0].header("authorization"), None);
    let body = taken[0].json();
    assert_eq!(body["model"], "claude-haiku-5-5");
    assert_eq!(body["system"], "Synthetic instructions.");
    assert_eq!(body["stream"], true);
    assert_eq!(body["output_config"]["effort"], "low");
    assert!(body.get("temperature").is_none());
    // the second question carries the first and its answer
    let messages = taken[1].json()["messages"].clone();
    assert_eq!(
        messages,
        serde_json::json!([
            {"role": "user", "content": "Hello"},
            {"role": "assistant", "content": "Bonjour"},
            {"role": "user", "content": "Hi"},
        ])
    );
}

#[test]
fn openai_streams_its_answer_with_a_bearer_key_and_the_instructions_first() {
    let mock = Mock::new(vec![openai_answer(&["Hal", "lo"])]);
    let launch = launch(
        Backend::OpenAi,
        Work::Summary,
        Model::Recommended,
        target(&mock.base, true),
    );
    assert_eq!(ask(&launch, "Hello").unwrap(), "Hallo");
    let taken = mock.taken();
    assert_eq!(taken[0].line, "POST /chat/completions HTTP/1.1");
    assert_eq!(taken[0].header("authorization"), Some(format!("Bearer {KEY}").as_str()));
    let body = taken[0].json();
    assert_eq!(body["model"], "gpt-6.1-terra");
    assert_eq!(body["reasoning_effort"], "medium");
    assert_eq!(
        body["messages"][0],
        serde_json::json!({"role": "system", "content": "Synthetic instructions."})
    );
    assert_eq!(
        body["messages"][1],
        serde_json::json!({"role": "user", "content": "Hello"})
    );
}

#[test]
fn a_compatible_server_takes_its_own_model_and_no_key() {
    let mock = Mock::new(vec![openai_answer(&["ok"])]);
    let mut api = target(&format!("{}/v1/", mock.base), false);
    api.model = Some("llama-synthetic".to_owned());
    let launch = launch(Backend::Compatible, Work::Chat, Model::Recommended, api);
    assert_eq!(ask(&launch, "Hello").unwrap(), "ok");
    let taken = mock.taken();
    assert_eq!(taken[0].line, "POST /v1/chat/completions HTTP/1.1");
    assert_eq!(taken[0].header("authorization"), None);
    let body = taken[0].json();
    assert_eq!(body["model"], "llama-synthetic");
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn a_model_named_is_asked_without_an_effort() {
    let mock = Mock::new(vec![anthropic_answer(&["ok"])]);
    let launch = launch(
        Backend::Anthropic,
        Work::Captions,
        Model::Named("claude-haiku-4-5".to_owned()),
        target(&mock.base, true),
    );
    assert_eq!(ask(&launch, "Hello").unwrap(), "ok");
    let body = mock.taken()[0].json();
    assert_eq!(body["model"], "claude-haiku-4-5");
    assert!(body.get("output_config").is_none());
}

#[test]
fn the_models_are_the_ones_chosen_the_providers_or_the_recommended() {
    assert_eq!(
        model_for(Backend::Compatible, Work::Chat, &Model::ToolDefault, Some("own")).unwrap(),
        "own"
    );
    assert_eq!(
        model_for(Backend::Anthropic, Work::Summary, &Model::ToolDefault, None).unwrap(),
        "claude-sonnet-5-5"
    );
    assert_eq!(
        model_for(Backend::Compatible, Work::Chat, &Model::Recommended, None),
        Err(Failure::NoModel(Backend::Compatible))
    );
}

#[test]
fn no_key_stops_before_any_request() {
    let launch = launch(
        Backend::OpenAi,
        Work::Chat,
        Model::Recommended,
        target("http://127.0.0.1:9", false),
    );
    assert_eq!(ask(&launch, "Hello").err(), Some(Failure::NoKey(Backend::OpenAi)));
    let mut api = target("http://127.0.0.1:9", true);
    api.keys = AiKeys::default();
    let launch = self::launch(Backend::OpenAi, Work::Chat, Model::Recommended, api);
    assert_eq!(ask(&launch, "Hello").err(), Some(Failure::NoKey(Backend::OpenAi)));
}

fn failure_of(backend: Backend, status: u16, body: &str) -> Failure {
    let mock = Mock::new(vec![(status, "application/json", body.to_owned())]);
    let launch = launch(
        backend,
        Work::Chat,
        Model::Named("synthetic-model".to_owned()),
        target(&mock.base, true),
    );
    ask(&launch, "Hello").unwrap_err()
}

#[test]
fn the_apis_refusals_say_what_the_user_can_do() {
    let anthropic = Backend::Anthropic;
    assert_eq!(
        failure_of(
            anthropic,
            401,
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#
        ),
        Failure::SignedOut(anthropic)
    );
    assert_eq!(
        failure_of(
            anthropic,
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API."}}"#
        ),
        Failure::NoCredit(anthropic)
    );
    assert_eq!(
        failure_of(
            anthropic,
            429,
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#
        ),
        Failure::RateLimited(anthropic)
    );
    assert_eq!(
        failure_of(
            anthropic,
            404,
            r#"{"type":"error","error":{"type":"not_found_error","message":"model: synthetic-model"}}"#
        ),
        Failure::UnknownModel(anthropic, "synthetic-model".to_owned())
    );
    let openai = Backend::OpenAi;
    assert_eq!(
        failure_of(
            openai,
            429,
            r#"{"error":{"message":"You exceeded your current quota.","type":"insufficient_quota","code":"insufficient_quota"}}"#
        ),
        Failure::NoCredit(openai)
    );
    assert_eq!(
        failure_of(
            openai,
            404,
            r#"{"error":{"message":"The model `synthetic-model` does not exist","code":"model_not_found"}}"#
        ),
        Failure::UnknownModel(openai, "synthetic-model".to_owned())
    );
    assert_eq!(
        failure_of(openai, 503, "not json"),
        Failure::Stopped(openai, Some("its server answered HTTP 503.".to_owned()))
    );
    assert_eq!(
        failure_of(openai, 500, r#"{"error":{"message":"the server had an error"}}"#),
        Failure::Stopped(openai, Some("the server had an error".to_owned()))
    );
}

#[test]
fn a_refusal_never_repeats_the_key() {
    let body = format!(r#"{{"error":{{"message":"Bad request for key {KEY}"}}}}"#);
    let mock = Mock::new(vec![(400, "application/json", body)]);
    let launch = launch(
        Backend::OpenAi,
        Work::Chat,
        Model::Recommended,
        target(&mock.base, true),
    );
    let failure = ask(&launch, "Hello").unwrap_err();
    assert!(!failure.message().contains(KEY), "{}", failure.message());
    assert!(!format!("{failure:?}").contains(KEY));
    assert!(!format!("{launch:?}").contains(KEY));
}

#[test]
fn an_error_in_the_stream_and_a_refusal_stop_the_answer() {
    let mock = Mock::new(vec![(
        200,
        "text/event-stream",
        sse(&[r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#]),
    )]);
    let launch = launch(
        Backend::Anthropic,
        Work::Chat,
        Model::Recommended,
        target(&mock.base, true),
    );
    assert_eq!(
        ask(&launch, "Hello"),
        Err(Failure::Stopped(Backend::Anthropic, Some("Overloaded".to_owned())))
    );
    let mock = Mock::new(vec![(
        200,
        "text/event-stream",
        sse(&[r#"{"type":"message_delta","delta":{"stop_reason":"refusal"}}"#]),
    )]);
    let launch = self::launch(
        Backend::Anthropic,
        Work::Chat,
        Model::Recommended,
        target(&mock.base, true),
    );
    assert_eq!(
        ask(&launch, "Hello"),
        Err(Failure::Stopped(
            Backend::Anthropic,
            Some("it declined to answer.".to_owned())
        ))
    );
    let mock = Mock::new(vec![(200, "text/event-stream", String::new())]);
    let launch = self::launch(
        Backend::OpenAi,
        Work::Chat,
        Model::Recommended,
        target(&mock.base, true),
    );
    assert_eq!(
        ask(&launch, "Hello"),
        Err(Failure::Stopped(Backend::OpenAi, Some("it gave no answer.".to_owned())))
    );
}

#[test]
fn a_server_that_cannot_be_reached_says_so() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let launch = launch(Backend::OpenAi, Work::Chat, Model::Recommended, target(&base, true));
    let failure = ask(&launch, "Hello").unwrap_err();
    assert!(
        matches!(failure, Failure::Stopped(Backend::OpenAi, Some(_))),
        "{failure:?}"
    );
}

#[test]
fn a_slow_answer_times_out_and_a_stop_ends_the_wait() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (release, released) = mpsc::channel::<()>();
    thread::spawn(move || {
        let _connection = listener.accept();
        let _ = released.recv_timeout(Duration::from_secs(5));
    });
    let launch = launch(Backend::OpenAi, Work::Chat, Model::Recommended, target(&base, true));
    let Target::Api(api) = &launch.target else {
        unreachable!()
    };
    let mut conversation = Conversation::start(&launch, api).unwrap();
    let stop = AtomicBool::new(false);
    let started = Instant::now();
    let timed_out = conversation.ask("Hello", Instant::now() + Duration::from_millis(300), &stop, &mut |_| {});
    assert_eq!(timed_out, Err(Failure::TimedOut(Backend::OpenAi)));
    assert!(started.elapsed() < Duration::from_secs(4));
    let stop = AtomicBool::new(true);
    let stopped = conversation.ask("Hello", Instant::now() + Duration::from_secs(5), &stop, &mut |_| {});
    assert_eq!(stopped, Err(Failure::Stopped(Backend::OpenAi, None)));
    drop(release);
}

fn provider(kind: Backend, base: &str) -> Provider {
    Provider {
        id: "api-1".to_owned(),
        kind,
        name: "Synthetic".to_owned(),
        enabled: true,
        base_url: Some(base.to_owned()),
        model: None,
        has_key: true,
    }
}

#[test]
fn each_api_lists_its_models() {
    let mock = Mock::new(vec![(
        200,
        "application/json",
        r#"{"data":[{"id":"claude-sonnet-5-5","display_name":"Claude Sonnet 5.5"},{"id":"claude-haiku-5-5"}],"has_more":false}"#
            .to_owned(),
    )]);
    let listed = list_models(&provider(Backend::Anthropic, &mock.base), &keys_with_key()).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(
        (listed[0].id.as_str(), listed[0].name.as_str()),
        ("claude-sonnet-5-5", "Claude Sonnet 5.5")
    );
    assert_eq!(listed[1].name, "claude-haiku-5-5");
    let taken = mock.taken();
    assert_eq!(taken[0].line, "GET /v1/models?limit=1000 HTTP/1.1");
    assert_eq!(taken[0].header("x-api-key"), Some(KEY));
    let mock = Mock::new(vec![(
        200,
        "application/json",
        r#"{"object":"list","data":[{"id":"gpt-6-luna","object":"model"}]}"#.to_owned(),
    )]);
    let listed = list_models(
        &provider(Backend::OpenAi, &format!("{}/v1", mock.base)),
        &keys_with_key(),
    )
    .unwrap();
    assert_eq!(listed[0].id, "gpt-6-luna");
    assert_eq!(mock.taken()[0].line, "GET /v1/models HTTP/1.1");
    let mock = Mock::new(vec![(
        401,
        "application/json",
        r#"{"error":{"message":"Incorrect API key provided","code":"invalid_api_key"}}"#.to_owned(),
    )]);
    assert_eq!(
        list_models(&provider(Backend::OpenAi, &mock.base), &keys_with_key()),
        Err(Failure::SignedOut(Backend::OpenAi))
    );
    assert!(models_in("not json").is_empty());
}
