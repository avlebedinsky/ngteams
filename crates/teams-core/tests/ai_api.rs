//! The AI APIs' requests (`teams_core::ai_api`) against a mock HTTP server on the loopback (synthetic keys, no
//! network): each kind's request and its headers, the streamed answer, each refusal, a stop (also while the server is
//! quiet), a server that cannot be reached or is slow, and the listing of the models. No key is in a failure or a
//! `Debug` output.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_core::ai_api::{ANTHROPIC_VERSION, Api, ApiError, ApiKind, ApiModel, Question, models_in};

/// The synthetic key every test gives.
const KEY: &str = "sk-synthetic-core-key-0000";
/// The time that each request may take.
const TIME: Duration = Duration::from_secs(10);

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

/// An answer of the mock: `(status, content type, body)`.
type Answer = (u16, &'static str, String);

/// A mock HTTP server that answers each request with the next of its answers.
struct Mock {
    base: String,
    taken: Arc<Mutex<Vec<Taken>>>,
}

impl Mock {
    fn new(answers: Vec<Answer>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let taken = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&taken);
        thread::spawn(move || {
            for answer in answers {
                let Ok((stream, _)) = listener.accept() else { return };
                serve(stream, answer, &kept);
            }
        });
        Self { base, taken }
    }

    fn taken(&self) -> Vec<Taken> {
        self.taken.lock().unwrap().clone()
    }
}

/// Takes one request from `stream` into `taken`, and answers it with `answer`.
fn serve(mut stream: TcpStream, answer: Answer, taken: &Mutex<Vec<Taken>>) {
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

fn anthropic_answer(parts: &[&str]) -> Answer {
    let mut events = vec![r#"{"type":"message_start","message":{"id":"msg_synthetic"}}"#.to_owned()];
    for part in parts {
        events.push(format!(
            r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"{part}"}}}}"#
        ));
    }
    events.push(r#"{"type":"message_stop"}"#.to_owned());
    let events: Vec<&str> = events.iter().map(String::as_str).collect();
    (200, "text/event-stream", sse(&events))
}

fn openai_answer(parts: &[&str]) -> Answer {
    let mut events: Vec<String> = parts
        .iter()
        .map(|part| format!(r#"{{"choices":[{{"index":0,"delta":{{"content":"{part}"}}}}]}}"#))
        .collect();
    events.push("[DONE]".to_owned());
    let events: Vec<&str> = events.iter().map(String::as_str).collect();
    (200, "text/event-stream", sse(&events))
}

fn question(effort: Option<&'static str>) -> Question {
    Question {
        model: "synthetic-model".to_owned(),
        instructions: "Synthetic instructions.".to_owned(),
        turns: vec![
            ("user", "Hello".to_owned()),
            ("assistant", "Bonjour".to_owned()),
            ("user", "Hi".to_owned()),
        ],
        effort,
    }
}

fn api(kind: ApiKind, base: &str) -> Api {
    Api::new(kind, base, Some(Secret::new(KEY))).unwrap()
}

fn ask(kind: ApiKind, answer: Answer) -> Result<String, ApiError> {
    let mock = Mock::new(vec![answer]);
    api(kind, &mock.base).ask(&question(None), TIME, &AtomicBool::new(false), &mut |_| {})
}

#[test]
fn anthropic_streams_its_answer_with_the_key_the_version_and_the_effort() {
    let mock = Mock::new(vec![anthropic_answer(&["Bon", "jour"])]);
    let mut parts = Vec::new();
    let answer = api(ApiKind::Anthropic, &format!("{}/", mock.base))
        .ask(&question(Some("low")), TIME, &AtomicBool::new(false), &mut |part| {
            parts.push(part.to_owned());
        })
        .unwrap();
    assert_eq!(answer, "Bonjour");
    assert_eq!(parts, ["Bon", "jour"]);
    let taken = &mock.taken()[0];
    assert_eq!(taken.line, "POST /v1/messages HTTP/1.1");
    assert_eq!(taken.header("x-api-key"), Some(KEY));
    assert_eq!(taken.header("anthropic-version"), Some(ANTHROPIC_VERSION));
    assert_eq!(taken.header("content-type"), Some("application/json"));
    assert_eq!(taken.header("authorization"), None);
    let body = taken.json();
    assert_eq!(body["model"], "synthetic-model");
    assert_eq!(body["max_tokens"], 16_000);
    assert_eq!(body["system"], "Synthetic instructions.");
    assert_eq!(body["stream"], true);
    assert_eq!(body["output_config"], json!({"effort": "low"}));
    assert!(body.get("temperature").is_none());
    assert_eq!(
        body["messages"],
        json!([
            {"role": "user", "content": "Hello"},
            {"role": "assistant", "content": "Bonjour"},
            {"role": "user", "content": "Hi"},
        ])
    );
}

#[test]
fn openai_streams_its_answer_with_a_bearer_key_and_the_instructions_first() {
    let mock = Mock::new(vec![openai_answer(&["Hal", "lo"]), openai_answer(&["ok"])]);
    let answer = api(ApiKind::OpenAi, &mock.base)
        .ask(&question(Some("medium")), TIME, &AtomicBool::new(false), &mut |_| {})
        .unwrap();
    assert_eq!(answer, "Hallo");
    // a local server without a key, and no effort
    let keyless = Api::new(ApiKind::OpenAi, &mock.base, None).unwrap();
    assert_eq!(
        keyless
            .ask(&question(None), TIME, &AtomicBool::new(false), &mut |_| {})
            .unwrap(),
        "ok"
    );
    let taken = mock.taken();
    assert_eq!(taken[0].line, "POST /chat/completions HTTP/1.1");
    assert_eq!(taken[0].header("authorization"), Some(format!("Bearer {KEY}").as_str()));
    assert_eq!(taken[0].header("anthropic-version"), None);
    let body = taken[0].json();
    assert_eq!(body["reasoning_effort"], "medium");
    assert_eq!(
        body["messages"][0],
        json!({"role": "system", "content": "Synthetic instructions."})
    );
    assert_eq!(body["messages"][3], json!({"role": "user", "content": "Hi"}));
    assert_eq!(taken[1].header("authorization"), None);
    assert!(taken[1].json().get("reasoning_effort").is_none());
}

#[test]
fn the_apis_refusals_say_what_the_user_can_do() {
    let refusal = |kind, status, body: &str| ask(kind, (status, "application/json", body.to_owned())).unwrap_err();
    let anthropic = ApiKind::Anthropic;
    assert_eq!(
        refusal(
            anthropic,
            401,
            r#"{"error":{"type":"authentication_error","message":"invalid x-api-key"}}"#
        ),
        ApiError::BadKey
    );
    assert_eq!(
        refusal(
            anthropic,
            400,
            r#"{"error":{"type":"invalid_request_error","message":"Your credit balance is too low."}}"#
        ),
        ApiError::NoCredit
    );
    assert_eq!(refusal(anthropic, 402, "{}"), ApiError::NoCredit);
    assert_eq!(
        refusal(
            anthropic,
            429,
            r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#
        ),
        ApiError::RateLimited
    );
    assert_eq!(
        refusal(
            anthropic,
            404,
            r#"{"error":{"type":"not_found_error","message":"model: synthetic-model"}}"#
        ),
        ApiError::UnknownModel
    );
    let openai = ApiKind::OpenAi;
    assert_eq!(
        refusal(
            openai,
            429,
            r#"{"error":{"message":"You exceeded your quota.","code":"insufficient_quota"}}"#
        ),
        ApiError::NoCredit
    );
    assert_eq!(
        refusal(openai, 400, r#"{"error":{"message":"Incorrect API key provided"}}"#),
        ApiError::BadKey
    );
    assert_eq!(
        refusal(
            openai,
            404,
            r#"{"error":{"message":"no such model","code":"model_not_found"}}"#
        ),
        ApiError::UnknownModel
    );
    assert_eq!(
        refusal(openai, 503, "not json"),
        ApiError::Stopped("its server answered HTTP 503.".to_owned())
    );
    assert_eq!(
        refusal(openai, 500, r#"{"error":"the server had an error\nand more"}"#),
        ApiError::Stopped("the server had an error".to_owned())
    );
}

#[test]
fn a_refusal_never_repeats_the_key_and_long_words_are_cut() {
    let body = format!(r#"{{"error":{{"message":"Bad request for key {KEY}"}}}}"#);
    let failure = ask(ApiKind::OpenAi, (400, "application/json", body)).unwrap_err();
    assert_eq!(failure, ApiError::Stopped("Bad request for key …".to_owned()));
    let long = format!(r#"{{"error":{{"message":"{}"}}}}"#, "é".repeat(200));
    let ApiError::Stopped(why) = ask(ApiKind::OpenAi, (500, "application/json", long)).unwrap_err() else {
        panic!("not stopped")
    };
    assert!(why.ends_with('…') && why.len() <= 160 + '…'.len_utf8(), "{why}");
    let api = api(ApiKind::Anthropic, "http://127.0.0.1:9");
    assert!(!format!("{api:?}").contains(KEY));
}

#[test]
fn an_error_in_the_stream_a_refusal_and_no_answer_stop_it() {
    let overloaded = sse(&[r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#]);
    assert_eq!(
        ask(ApiKind::Anthropic, (200, "text/event-stream", overloaded)),
        Err(ApiError::Stopped("Overloaded".to_owned()))
    );
    let declined = sse(&[r#"{"type":"message_delta","delta":{"stop_reason":"refusal"}}"#]);
    assert_eq!(
        ask(ApiKind::Anthropic, (200, "text/event-stream", declined)),
        Err(ApiError::Stopped("it declined to answer.".to_owned()))
    );
    assert_eq!(
        ask(ApiKind::OpenAi, (200, "text/event-stream", String::new())),
        Err(ApiError::Stopped("it gave no answer.".to_owned()))
    );
    // lines that are no data, and data that is no JSON, are passed over
    let noisy = format!(
        ": ping\n\ndata: not json\n\n{}",
        sse(&[r#"{"choices":[{"delta":{"content":"ok"}}]}"#])
    );
    assert_eq!(
        ask(ApiKind::OpenAi, (200, "text/event-stream", noisy)),
        Ok("ok".to_owned())
    );
    // an answer that ends without text, or with only spaces, is no answer
    assert_eq!(
        ask(ApiKind::OpenAi, (200, "text/event-stream", sse(&["[DONE]"]))),
        Err(ApiError::Stopped("it gave no answer.".to_owned()))
    );
    assert_eq!(
        ask(ApiKind::Anthropic, anthropic_answer(&[" ", "\\n"])),
        Err(ApiError::Stopped("it gave no answer.".to_owned()))
    );
}

#[test]
fn a_stop_drops_the_request() {
    // set before the request: nothing is sent
    let mock = Mock::new(vec![openai_answer(&["never"])]);
    let stop = AtomicBool::new(true);
    assert_eq!(
        api(ApiKind::OpenAi, &mock.base).ask(&question(None), TIME, &stop, &mut |_| {}),
        Err(ApiError::Cancelled)
    );
    assert!(mock.taken().is_empty());
    // set while the answer streams: the next line drops the response
    let mock = Mock::new(vec![anthropic_answer(&["Bon", "jour", "!"])]);
    let stop = AtomicBool::new(false);
    let mut parts = Vec::new();
    let stopped = api(ApiKind::Anthropic, &mock.base).ask(&question(None), TIME, &stop, &mut |part| {
        parts.push(part.to_owned());
        stop.store(true, Ordering::Relaxed);
    });
    assert_eq!(stopped, Err(ApiError::Cancelled));
    assert_eq!(parts, ["Bon"]);
}

/// A server that takes one request, answers `head` (nothing when empty) and then stays quiet: whether it saw the
/// connection close within 5 s.
fn quiet_server(head: &'static str) -> (String, mpsc::Receiver<bool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (closed, saw) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|read| read > 0) && line != "\r\n" {
            line.clear();
        }
        stream.write_all(head.as_bytes()).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        // the body of the request, then the end of the connection (0), else a timeout
        let mut rest = [0; 4096];
        let ended = loop {
            match reader.read(&mut rest) {
                Ok(0) => break true,
                Ok(_) => {}
                Err(_) => break false,
            }
        };
        let _ = closed.send(ended);
    });
    (base, saw)
}

#[test]
fn a_stop_drops_a_quiet_request_at_once() {
    // no headers yet (the server thinks), then headers and no line (a model that reasons, a local server that loads)
    let heads = [
        "",
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
    ];
    for head in heads {
        let (base, closed) = quiet_server(head);
        let stop = Arc::new(AtomicBool::new(false));
        let setter = Arc::clone(&stop);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            setter.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        let asked = api(ApiKind::OpenAi, &base).ask(&question(Some("medium")), TIME, &stop, &mut |_| {});
        assert_eq!(asked, Err(ApiError::Cancelled), "head {head:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "head {head:?}: {:?}",
            started.elapsed()
        );
        assert_eq!(
            closed.recv_timeout(Duration::from_secs(6)),
            Ok(true),
            "head {head:?}: the connection stays open"
        );
    }
}

#[test]
fn a_server_that_cannot_be_reached_or_is_slow_says_so() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    assert!(matches!(
        api(ApiKind::OpenAi, &base).ask(&question(None), TIME, &AtomicBool::new(false), &mut |_| {}),
        Err(ApiError::Stopped(why)) if why.contains("could not be reached")
    ));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (release, released) = mpsc::channel::<()>();
    thread::spawn(move || {
        let _connection = listener.accept();
        let _ = released.recv_timeout(Duration::from_secs(5));
    });
    let started = Instant::now();
    assert_eq!(
        api(ApiKind::OpenAi, &base).ask(
            &question(None),
            Duration::from_millis(300),
            &AtomicBool::new(false),
            &mut |_| {}
        ),
        Err(ApiError::TimedOut)
    );
    assert!(started.elapsed() < Duration::from_secs(4));
    drop(release);
}

#[test]
fn each_api_lists_its_models() {
    let mock = Mock::new(vec![(
        200,
        "application/json",
        r#"{"data":[{"id":"claude-sonnet-5-5","display_name":"Claude Sonnet 5.5"},{"id":"claude-haiku-5-5"}]}"#
            .to_owned(),
    )]);
    let listed = api(ApiKind::Anthropic, &mock.base).models(TIME).unwrap();
    assert_eq!(
        listed,
        [
            ApiModel {
                id: "claude-sonnet-5-5".to_owned(),
                name: "Claude Sonnet 5.5".to_owned()
            },
            ApiModel {
                id: "claude-haiku-5-5".to_owned(),
                name: "claude-haiku-5-5".to_owned()
            },
        ]
    );
    let taken = &mock.taken()[0];
    assert_eq!(taken.line, "GET /v1/models?limit=1000 HTTP/1.1");
    assert_eq!(taken.header("x-api-key"), Some(KEY));
    assert_eq!(taken.header("anthropic-version"), Some(ANTHROPIC_VERSION));
    let mock = Mock::new(vec![
        (
            200,
            "application/json",
            r#"{"object":"list","data":[{"id":"gpt-6-luna"},{"object":"model"}]}"#.to_owned(),
        ),
        (
            401,
            "application/json",
            r#"{"error":{"message":"Incorrect API key provided","code":"invalid_api_key"}}"#.to_owned(),
        ),
    ]);
    let openai = api(ApiKind::OpenAi, &format!("{}/v1", mock.base));
    assert_eq!(openai.models(TIME).unwrap()[0].id, "gpt-6-luna");
    assert_eq!(mock.taken()[0].line, "GET /v1/models HTTP/1.1");
    assert_eq!(
        mock.taken()[0].header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    assert_eq!(openai.models(TIME), Err(ApiError::BadKey));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    assert!(matches!(
        api(ApiKind::OpenAi, &base).models(TIME),
        Err(ApiError::Stopped(_))
    ));
    assert!(models_in("not json").is_empty());
    assert!(models_in(r#"{"data":"no list"}"#).is_empty());
}
