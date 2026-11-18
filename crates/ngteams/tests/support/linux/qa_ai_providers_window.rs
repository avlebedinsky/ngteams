//! QA of task 516 (round 1), in the UI sandbox: the AI providers (the Anthropic API, the OpenAI API and an
//! OpenAI-compatible API beside the Claude Code and Codex CLIs). The real app runs as a child in the sandbox, as lab
//! account a on `teams-mock`. The parent drives it with the sandbox's virtual keyboard, and asks the child through a
//! file in the sandbox dir to do what a click on a button or a dropdown row does (the same view methods), and what
//! the window holds (`harness = false`: GPUI runs on the process's main thread).
//!
//! - **A fake AI server** ([`FakeAi`]) runs in the parent on the loopback. It speaks the Anthropic Messages API
//!   (`/v1/messages`, `/v1/models`) and OpenAI's Chat Completions (`/chat/completions`, `/models`), with streamed
//!   server-sent events, synthetic keys and made-up models. The first part of a base URL's path selects how it acts:
//!   `ok`, `compat` (no key necessary), `badkey`, `nocredit`, `ratelimit`, `words` (an error in its own words, with the
//!   key in them), `slow` (it never answers). It keeps each request; the parent examines them.
//! - **A made-up `claude`** ([`FAKE_CLAUDE`]) is first on the app's `PATH`, which holds no real `claude` or `codex`.
//! - **Three runs of the app**: an earlier build's settings file; a new settings file (add each API kind, test the
//!   connection, pick per feature, use each feature, reorder and turn off, "Other model…", failures, remove a key,
//!   many providers); a restart over the same file (the choices kept).
//! - The keys of the AI providers go to the app's own secret store, here the in-memory one (`AppSecrets::in_memory`):
//!   the sandbox has no Secret Service. The restart puts the keys that the first run saved back in it, as a keyring
//!   keeps them. The real keyring is not tested here.
//!
//! `QA_AI_PROVIDERS_ONLY` (a comma list of check names) runs only those. `QA_AI_PROVIDERS_EVIDENCE` names a directory
//! that gets the screenshots, the requests of the fake server, the settings files, the app's debug log and the
//! results. Screenshots also go to `target/ui-shots/qa516-*.png`. Every name, key, model and text is made up.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::fmt::Write as _;
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, future, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::accounts::Runner;
use ngteams::accounts_file::PersonalPaths;
use ngteams::app::{self, Launch};
use ngteams::secrets::AppSecrets;
use ngteams::setup::Setup;
#[path = "../window.rs"]
#[expect(
    dead_code,
    reason = "the shared window support serves other harnesses too: its runner is not used here"
)]
mod support;
use serde_json::{Value, json};
use support::{Alias, display_name};
use teams_auth::secrets::{Secret, SecretKey, SecretStore as _};
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{CallFloor, Outbox};
use teams_mock::{
    AB_CHAT, AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE, PRESENCE_BASE,
    PROVISIONING_BASE, TrouterMock, USER_PROFILE_BASE,
};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::local_ai::providers::ApiModels;
use teams_ui::local_ai::{AiKeys, Backend, Model, Providers, ToolChoice, Work};
use teams_ui::settings_view::SettingsView;
use teams_ui::settings_view::ai_picker::AiPicker;
use teams_ui::settings_view::ai_providers::AiProviders;
use teams_ui::shell::{Main, Shell};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::VirtualInput;
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

const CHILD_ENV: &str = "QA_AI_PROVIDERS_CHILD";
const ONLY_ENV: &str = "QA_AI_PROVIDERS_ONLY";
const EVIDENCE_ENV: &str = "QA_AI_PROVIDERS_EVIDENCE";
const SETTINGS_ENV: &str = "QA_AI_PROVIDERS_SETTINGS";
const TOOLS_ENV: &str = "QA_AI_PROVIDERS_TOOLS";
/// The child's data directory: the app's debug log is in its `logs/`.
const DATA_ENV: &str = "QA_AI_PROVIDERS_DATA";
/// The keys that the child puts in its secret store before the app starts: `<provider id>=<key>` lines in a file.
const SEED_ENV: &str = "QA_AI_PROVIDERS_SEED";
/// The file in the sandbox dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "qa516-ask";
/// The file in the sandbox dir that gets the mock's Teams token of a (the fake AI server must never see it).
const TEAMS_TOKEN: &str = "qa516-teams-token";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(80);
const SETTLE: Duration = Duration::from_millis(900);
/// The time between a key and the next, so the app sees each.
const STEP: Duration = Duration::from_millis(150);
/// niri's layout: each window takes the whole screen, and no hot corner.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
const SCREEN: (u32, u32) = (1280, 1000);

/// The synthetic keys: the ones the fake server takes, and one it refuses.
const KEY_ANTHROPIC: &str = "sk-ant-qa516-synthetic-0001";
const KEY_OPENAI: &str = "sk-qa516-synthetic-openai-0002";
const KEY_WRONG: &str = "sk-qa516-synthetic-wrong-9999";
/// A long provider name: 100 bytes, the most that the settings keep.
const LONG_NAME: &str =
    "QA local server with a very long name that goes on and on to fill the row of the box and more orange";

/// A made-up Claude Code (`__STATE__` its state directory): `--help` names two models; it logs how it was started and
/// each question, and answers a draft's translation with `[cli:<model>]` before the draft and a correction with
/// "teh" fixed.
const FAKE_CLAUDE: &str = r#"#!/bin/bash
state="__STATE__"
case "$1" in
  --help)
    printf '%s\n' "Usage: claude [options]" "  --model <model>  Model for the current session: an alias such as 'sonnet' or 'opus'"
    exit 0;;
esac
model=""
args="$*"
while [ $# -gt 0 ]; do
  [ "$1" = "--model" ] && model="$2"
  shift
done
printf 'start model=%s\n' "$model" >> "$state/calls.log"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$state/received.log"
  t=$(printf '%s' "$line" | sed 's/.*<message>\\n\(.*\)\\n<\/message>.*/\1/')
  case "$line" in
    *'<language>'*) a="<translated>$(printf '%s' "$t" | sed "s/^\(<x[0-9]*>\)\{0,1\}/\1[cli:$model] /")</translated>";;
    *) a="<corrected>$(printf '%s' "$t" | sed 's/teh/the/g')</corrected>";;
  esac
  printf '{"type":"result","subtype":"success","is_error":false,"result":"%s"}\n' "$a" | tee -a "$state/answers.log"
done
"#;

/// A summary as the recap asks for it.
const SUMMARY_JSON: &str =
    r#"{"purpose": "QA summary", "takeaways": ["one"], "topics": [], "next_steps": [], "questions": []}"#;

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_ai_providers_window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn ensure(holds: bool, what: impl FnOnce() -> String) -> SandboxResult {
    if holds { Ok(()) } else { Err(what().into()) }
}

fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn evidence_dir() -> Option<PathBuf> {
    std::env::var_os(EVIDENCE_ENV).map(PathBuf::from)
}

fn wanted(name: &str) -> bool {
    std::env::var(ONLY_ENV)
        .ok()
        .is_none_or(|list| list.split(',').map(str::trim).any(|one| one == name))
}

// --- the fake AI server ---

/// One request that the fake server got.
#[derive(Clone, Debug)]
struct Seen {
    path: String,
    /// The head's lines after the request line, names in lower case.
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }

    fn model(&self) -> String {
        self.json()["model"].as_str().unwrap_or_default().to_owned()
    }

    fn as_json(&self) -> Value {
        json!({"path": self.path, "headers": self.headers, "body": self.json()})
    }
}

/// The fake AI server (see the module docs).
struct FakeAi {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
    /// The requests to `slow` that the app dropped, with how long they were open.
    dropped: Arc<Mutex<Vec<(String, Duration)>>>,
}

impl FakeAi {
    fn start() -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let dropped = Arc::new(Mutex::new(Vec::new()));
        let (held, gone) = (Arc::clone(&seen), Arc::clone(&dropped));
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (held, gone) = (Arc::clone(&held), Arc::clone(&gone));
                thread::spawn(move || {
                    let _ = serve(stream, &held, &gone);
                });
            }
        });
        Ok(Self { port, seen, dropped })
    }

    fn base(&self, persona: &str) -> String {
        format!("http://127.0.0.1:{}/{persona}", self.port)
    }

    fn count(&self) -> usize {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    fn since(&self, from: usize) -> Vec<Seen> {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner)[from..].to_vec()
    }

    fn all(&self) -> Vec<Seen> {
        self.since(0)
    }

    fn dropped(&self) -> Vec<(String, Duration)> {
        self.dropped.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// The address a dead endpoint has: a port that nothing listens on.
fn dead_base() -> io::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(format!("http://127.0.0.1:{port}/dead"))
}

fn serve(mut stream: TcpStream, seen: &Mutex<Vec<Seen>>, dropped: &Mutex<Vec<(String, Duration)>>) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let path = request_line.split_whitespace().nth(1).unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim_end().is_empty() {
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
    (&mut reader).take(length).read_to_end(&mut body)?;
    let request = Seen {
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    seen.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(request.clone());
    match reply(&request) {
        Reply::Json(status, body) => write!(
            stream,
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        ),
        Reply::Events(events) => {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n"
            )?;
            for event in events {
                stream.write_all(event.as_bytes())?;
                stream.flush()?;
                thread::sleep(Duration::from_millis(30));
            }
            Ok(())
        }
        Reply::Hold => {
            // never answers: waits until the app drops the request
            let start = Instant::now();
            stream.set_read_timeout(Some(Duration::from_millis(100)))?;
            let mut byte = [0_u8; 1];
            while start.elapsed() < Duration::from_secs(300) {
                match stream.read(&mut byte) {
                    Ok(0) => {
                        dropped
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .push((request.path.clone(), start.elapsed()));
                        return Ok(());
                    }
                    Ok(_) => {}
                    Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                    Err(_) => return Ok(()),
                }
            }
            Ok(())
        }
    }
}

enum Reply {
    Json(u16, String),
    Events(Vec<String>),
    Hold,
}

/// What the fake server answers `request` (see the module docs).
fn reply(request: &Seen) -> Reply {
    let persona = request
        .path
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or_default();
    let anthropic = request.path.contains("/v1/messages") || request.header("anthropic-version").is_some();
    let key = if anthropic {
        request.header("x-api-key").map(str::to_owned)
    } else {
        request
            .header("authorization")
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::to_owned)
    };
    let good = match (persona, anthropic) {
        ("compat", _) => key.is_none(),
        (_, true) => key.as_deref() == Some(KEY_ANTHROPIC),
        (_, false) => key.as_deref() == Some(KEY_OPENAI),
    };
    if let Some(refused) = refusal(persona, anthropic, good, &key.unwrap_or_default()) {
        return refused;
    }
    answer_of(request, persona, anthropic)
}

/// How a `persona` refuses a request, if it does: the key refused, no credit, a rate limit, its own words, no answer.
fn refusal(persona: &str, anthropic: bool, good: bool, typed: &str) -> Option<Reply> {
    if !good || persona == "badkey" {
        return Some(if anthropic {
            Reply::Json(
                401,
                json!({"type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"}})
                    .to_string(),
            )
        } else {
            Reply::Json(
                401,
                json!({"error": {"message": format!("Incorrect API key provided: {typed}. You can find your API key \
                       at the fake page."), "type": "invalid_request_error", "code": "invalid_api_key"}})
                .to_string(),
            )
        });
    }
    match persona {
        "nocredit" if anthropic => {
            return Some(Reply::Json(
                400,
                json!({"type": "error", "error": {"type": "invalid_request_error", "message": "Your credit balance \
                       is too low to access the Anthropic API. Please go to Plans & Billing to upgrade or purchase \
                       credits."}})
                .to_string(),
            ));
        }
        "nocredit" => {
            return Some(Reply::Json(
                429,
                json!({"error": {"message": "You exceeded your current quota, please check your plan and billing \
                       details.", "type": "insufficient_quota", "code": "insufficient_quota"}})
                .to_string(),
            ));
        }
        "ratelimit" => {
            let error = if anthropic {
                json!({"type": "error", "error": {"type": "rate_limit_error", "message": "Number of request tokens \
                       has exceeded your per-minute rate limit"}})
            } else {
                json!({"error": {"message": "Rate limit reached for requests", "type": "requests",
                       "code": "rate_limit_exceeded"}})
            };
            return Some(Reply::Json(429, error.to_string()));
        }
        "words" => {
            return Some(Reply::Json(
                503,
                json!({"error": {"type": "api_error", "message": format!("The QA server is down for upkeep (key \
                       {typed}).\nA second line that the app does not show.")}})
                .to_string(),
            ));
        }
        "slow" => return Some(Reply::Hold),
        _ => {}
    }
    None
}

/// The answer of a `persona` that answers: the models listed, a model it does not know, or the answer streamed.
fn answer_of(request: &Seen, persona: &str, anthropic: bool) -> Reply {
    if request.path.contains("/models") {
        let listed = match (persona, anthropic) {
            ("compat", _) => json!({"object": "list", "data": [{"id": "local-qa-1"}, {"id": "local-qa-2"}]}),
            (_, true) => json!({"data": [
                {"id": "claude-sonnet-5-5", "display_name": "Claude Sonnet 5.5"},
                {"id": "claude-haiku-5-5", "display_name": "Claude Haiku 5.5"},
                {"id": "claude-qa-listed-1", "display_name": "QA Listed Model"}]}),
            (_, false) => json!({"object": "list", "data": [
                {"id": "gpt-6.1-terra"}, {"id": "gpt-6-luna"}, {"id": "gpt-qa-listed-1"}]}),
        };
        return Reply::Json(200, listed.to_string());
    }
    let body = request.json();
    let model = body["model"].as_str().unwrap_or_default().to_owned();
    if model == "no-such-model" {
        let error = if anthropic {
            json!({"type": "error", "error": {"type": "not_found_error", "message": "model: no-such-model"}})
        } else {
            json!({"error": {"message": "The model `no-such-model` does not exist or you do not have access to it.",
                   "type": "invalid_request_error", "code": "model_not_found"}})
        };
        return Reply::Json(404, error.to_string());
    }
    let (instructions, question) = if anthropic {
        let last = body["messages"]
            .as_array()
            .and_then(|turns| turns.last())
            .cloned()
            .unwrap_or(Value::Null);
        (
            body["system"].as_str().unwrap_or_default().to_owned(),
            last["content"].as_str().unwrap_or_default().to_owned(),
        )
    } else {
        let turns = body["messages"].as_array().cloned().unwrap_or_default();
        (
            turns
                .first()
                .and_then(|turn| turn["content"].as_str())
                .unwrap_or_default()
                .to_owned(),
            turns
                .last()
                .and_then(|turn| turn["content"].as_str())
                .unwrap_or_default()
                .to_owned(),
        )
    };
    let flavor = match (persona, anthropic) {
        ("compat", _) => "compat",
        (_, true) => "anthropic",
        (_, false) => "openai",
    };
    let text = answer_text(&instructions, &question, &format!("{flavor}:{model}"));
    Reply::Events(if anthropic {
        anthropic_events(&text)
    } else {
        openai_events(&text)
    })
}

/// The text between `open` and `close` after `from` in `text`, and where it ends.
fn between<'a>(text: &'a str, open: &str, close: &str, from: usize) -> Option<(&'a str, usize)> {
    let start = text[from..].find(open)? + from + open.len();
    let end = text[start..].find(close)? + start;
    Some((&text[start..end], end + close.len()))
}

/// What the fake model answers `question` with `instructions`, its answers marked with `tag`.
fn answer_text(instructions: &str, question: &str, tag: &str) -> String {
    if question.contains("<glossary-request/>") {
        return "none".to_owned();
    }
    if instructions.contains("Fathom") {
        return SUMMARY_JSON.to_owned();
    }
    if let Some((caption, _)) = between(question, "<caption", "</caption>", 0) {
        let text = caption.split_once('>').map_or(caption, |(_, text)| text);
        return format!("[{tag}] DE {text}");
    }
    if let Some((batch, _)) = between(question, "<translate>", "</translate>", 0) {
        let mut answer = String::new();
        let mut at = 0;
        while let Some((head, end)) = between(batch, "<m id=\"", "</m>", at) {
            at = end;
            let Some((id, rest)) = head.split_once('"') else {
                continue;
            };
            let text = rest.split_once('>').map_or("", |(_, text)| text);
            let _ = write!(answer, "<m id=\"{id}\" lang=\"en\">[{tag}] DE {text}</m>");
        }
        return answer;
    }
    if let Some((outgoing, _)) = between(question, "<outgoing", "</outgoing>", 0) {
        let text = outgoing.split_once('>').map_or(outgoing, |(_, text)| text);
        return format!("<out lang=\"en\">{text}</out>");
    }
    if let Some((draft, _)) = between(question, "<message>\n", "\n</message>", 0) {
        if question.contains("<language>") {
            // the tag inside the draft's first marker, as a model keeps every marker
            let at = if draft.starts_with("<x") {
                draft.find('>').map_or(0, |end| end + 1)
            } else {
                0
            };
            return format!("<translated>{}[{tag}] {}</translated>", &draft[..at], &draft[at..]);
        }
        return format!("<corrected>{}</corrected>", draft.replace("teh", "the"));
    }
    format!("[{tag}] ok")
}

/// `text` in two parts, as the Anthropic API streams it.
fn anthropic_events(text: &str) -> Vec<String> {
    let event = |name: &str, data: Value| format!("event: {name}\ndata: {data}\n\n");
    let half = text
        .char_indices()
        .nth(text.chars().count() / 2)
        .map_or(0, |(at, _)| at);
    let mut events = vec![
        event(
            "message_start",
            json!({"type": "message_start", "message": {"id": "msg_qa", "type": "message", "role": "assistant",
                   "content": [], "model": "qa", "usage": {"input_tokens": 1, "output_tokens": 1}}}),
        ),
        event(
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
    ];
    for part in [&text[..half], &text[half..]] {
        events.push(event(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": part}}),
        ));
    }
    events.push(event(
        "content_block_stop",
        json!({"type": "content_block_stop", "index": 0}),
    ));
    events.push(event(
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 2}}),
    ));
    events.push(event("message_stop", json!({"type": "message_stop"})));
    events
}

/// `text` in two parts, as OpenAI's Chat Completions stream it.
fn openai_events(text: &str) -> Vec<String> {
    let half = text
        .char_indices()
        .nth(text.chars().count() / 2)
        .map_or(0, |(at, _)| at);
    let mut events: Vec<String> = [&text[..half], &text[half..]]
        .iter()
        .map(|part| {
            let chunk = json!({"id": "qa", "object": "chat.completion.chunk",
                               "choices": [{"index": 0, "delta": {"content": part}, "finish_reason": null}]});
            format!("data: {chunk}\n\n")
        })
        .collect();
    events.push("data: [DONE]\n\n".to_owned());
    events
}

// --- the parent ---

type Check = (&'static str, fn(&mut User<'_>) -> SandboxResult);

/// The run of an earlier build's settings file.
const EARLIER_CHECKS: [Check; 1] = [(
    "earlier_build_file_keeps_its_choices",
    earlier_build_file_keeps_its_choices,
)];

/// The first run over a new settings file, in this order (each check starts where the last one ended).
const FIRST_CHECKS: [Check; 15] = [
    ("cli_found_is_added_and_on", cli_found_is_added_and_on),
    ("add_anthropic_and_test", add_anthropic_and_test),
    ("add_openai_bad_key_then_good", add_openai_bad_key_then_good),
    ("add_compatible_long_name", add_compatible_long_name),
    ("pick_per_feature", pick_per_feature),
    ("draft_through_anthropic", draft_through_anthropic),
    ("correction_through_cli", correction_through_cli),
    ("correction_other_model_compatible", correction_other_model_compatible),
    ("chat_translation_through_openai", chat_translation_through_openai),
    (
        "automatic_follows_order_and_switches",
        automatic_follows_order_and_switches,
    ),
    ("failures_say_why", failures_say_why),
    ("slow_endpoint_stops", slow_endpoint_stops),
    ("remove_key", remove_key),
    ("many_providers", many_providers),
    (
        "no_teams_token_reaches_the_ai_server",
        no_teams_token_reaches_the_ai_server,
    ),
];

/// The restart over the first run's settings file.
const RESTART_CHECKS: [Check; 1] = [("restart_keeps_choices", restart_keeps_choices)];

/// What an earlier build (before the AI providers) kept of the AI choices.
const EARLIER_FILE: &str = "message_translate_to = \"German\"\n\
                            message_translator = { tool = \"claude\", model = \"opus\" }\n\
                            message_corrector = \"codex\"\n\
                            chat_translator = { tool = \"automatic\", model = \"recommended\" }\n\
                            caption_translator = { tool = \"claude\", model = \"haiku\" }\n\
                            recap_summarizer = { tool = \"codex\", model = \"gpt-6.1-terra\" }\n";

/// What the parent remembers across the runs.
#[derive(Default)]
struct Shared {
    passed: Vec<String>,
    failed: Vec<(String, String)>,
    /// The first run's last state, for the restart to compare.
    before_restart: Option<Value>,
}

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_ai_providers_window", &options)? else {
        return Ok(());
    };
    let tool = FakeTool::install(&sandbox)?;
    let ai = FakeAi::start()?;
    let mut shared = Shared::default();
    let earlier = sandbox.path("earlier-settings.toml");
    fs::write(&earlier, EARLIER_FILE)?;
    let settings = sandbox.path("settings.toml");
    let runs: [(&'static str, &Path, &[Check]); 3] = [
        ("earlier", &earlier, &EARLIER_CHECKS),
        ("first", &settings, &FIRST_CHECKS),
        ("restart", &settings, &RESTART_CHECKS),
    ];
    let mut quits = Vec::new();
    for (name, file, checks) in runs {
        if !checks.iter().any(|(check, _)| wanted(check)) {
            continue;
        }
        quits.push(run(&sandbox, &tool, &ai, (name, file, checks), &mut shared)?);
    }
    summarize(&tool, &ai, &shared);
    for quit in quits {
        quit?;
    }
    ensure(shared.failed.is_empty(), || {
        format!("{} checks failed", shared.failed.len())
    })
}

/// One run of the app over `file`, its `checks` run in order; how its app ended.
fn run(
    sandbox: &Sandbox,
    tool: &FakeTool,
    ai: &FakeAi,
    (name, file, checks): (&'static str, &Path, &[Check]),
    shared: &mut Shared,
) -> SandboxResult<SandboxResult> {
    let seed = sandbox.path(&format!("seed-{name}"));
    // the keyring keeps the keys of the first run: the restart puts them back (the OpenAI key left out)
    let seeded = if name == "restart" {
        format!("api-1={KEY_ANTHROPIC}\n")
    } else {
        String::new()
    };
    fs::write(&seed, seeded)?;
    let data = sandbox.path(&format!("data-{name}"));
    fs::create_dir_all(&data)?;
    let child = sandbox.spawn(
        &std::env::current_exe()?,
        &[],
        &[
            (CHILD_ENV, "app"),
            (SETTINGS_ENV, &file.display().to_string()),
            (TOOLS_ENV, &tool.bin.display().to_string()),
            (DATA_ENV, &data.display().to_string()),
            (SEED_ENV, &seed.display().to_string()),
            ("TEAMS_APP_LOG", "debug"),
            ("PATH", &tool.path),
            ("CLAUDE_CONFIG_DIR", &tool.home.display().to_string()),
            ("CODEX_HOME", &tool.home.display().to_string()),
        ],
    )?;
    let running = Running::new(child)?;
    let mut user = User::start(sandbox, running, tool, ai, name, file.to_owned())?;
    user.before_restart = shared.before_restart.take();
    for (check, run) in checks {
        if wanted(check) {
            user.run_check(check, *run, shared);
        }
    }
    if name == "first" {
        shared.before_restart = user.state().ok();
    }
    let quit = user.ask("quit").and_then(|_| {
        let (status, stderr) = user.running.finish(WAIT)?;
        ensure(status.success(), || {
            format!("the {name} run's app exited {status}: {stderr}")
        })
    });
    after_run(sandbox, ai, name, file, &data, shared);
    Ok(quit)
}

/// The fake server's requests, the made-up tool's logs and the results, as evidence; the results said.
fn summarize(tool: &FakeTool, ai: &FakeAi, shared: &Shared) {
    if let Some(dir) = evidence_dir() {
        let requests: Vec<Value> = ai.all().iter().map(Seen::as_json).collect();
        let _ = fs::write(
            dir.join("fake-ai-requests.json"),
            serde_json::to_string_pretty(&requests).unwrap_or_default(),
        );
        for log in ["received.log", "answers.log", "calls.log"] {
            let _ = fs::copy(tool.state.join(log), dir.join(format!("cli-{log}")));
        }
    }
    let mut summary = String::new();
    for name in &shared.passed {
        let _ = writeln!(summary, "pass {name}");
    }
    for (name, why) in &shared.failed {
        let _ = writeln!(summary, "FAIL {name}: {why}");
    }
    say(&summary);
    if let Some(dir) = evidence_dir() {
        let _ = fs::write(dir.join("results.txt"), &summary);
    }
    say(&format!(
        "qa_ai_providers_window: {} passed, {} failed",
        shared.passed.len(),
        shared.failed.len()
    ));
}

/// After a run: its settings file and its debug log hold no key; both kept as evidence.
fn after_run(sandbox: &Sandbox, ai: &FakeAi, name: &str, file: &Path, data: &Path, shared: &mut Shared) {
    let check = format!("{name}_files_hold_no_key");
    if !wanted(&check) && !wanted(name) && std::env::var(ONLY_ENV).is_ok() {
        return;
    }
    let settings = fs::read_to_string(file).unwrap_or_default();
    let mut logs = String::new();
    for entry in walk(data) {
        if entry.extension().is_some_and(|extension| extension == "log")
            || entry.to_string_lossy().contains("teams-app.log")
        {
            logs.push_str(&fs::read_to_string(&entry).unwrap_or_default());
        }
    }
    if let Some(dir) = evidence_dir() {
        let _ = fs::write(dir.join(format!("settings-after-{name}.toml")), &settings);
        let _ = fs::write(dir.join(format!("app-debug-log-{name}.txt")), &logs);
    }
    let token = fs::read_to_string(sandbox.path(TEAMS_TOKEN)).unwrap_or_default();
    let mut found = Vec::new();
    for key in [KEY_ANTHROPIC, KEY_OPENAI, KEY_WRONG] {
        if settings.contains(key) {
            found.push(format!("{key} in the settings file"));
        }
        if logs.contains(key) {
            found.push(format!("{key} in the app's log"));
        }
    }
    if !token.is_empty() && logs.contains(&token) {
        found.push("the Teams token in the app's log".to_owned());
    }
    let _ = ai;
    say(&format!(
        "check {check}: settings {} bytes, log {} bytes",
        settings.len(),
        logs.len()
    ));
    if logs.is_empty() {
        found.push("no app log was written".to_owned());
    }
    if found.is_empty() {
        shared.passed.push(check);
    } else {
        shared.failed.push((check, found.join("; ")));
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}

/// The made-up tool as the parent sees it.
struct FakeTool {
    bin: PathBuf,
    state: PathBuf,
    home: PathBuf,
    /// The child's `PATH`: `bin`, then the sandbox's own, without any directory holding a real `claude` or `codex`.
    path: String,
}

impl FakeTool {
    fn install(sandbox: &Sandbox) -> SandboxResult<Self> {
        use std::os::unix::fs::PermissionsExt as _;
        let (bin, state, home, commands) = (
            sandbox.path("ai-bin"),
            sandbox.path("ai-state"),
            sandbox.path("ai-home"),
            sandbox.path("ai-commands"),
        );
        for dir in [&bin, &state, &home, &commands] {
            fs::create_dir_all(dir)?;
        }
        let inherited = sandbox
            .client_env()
            .into_iter()
            .find(|(name, _)| *name == "PATH")
            .map(|(_, path)| path)
            .unwrap_or_default();
        let kept: Vec<PathBuf> = std::env::split_paths(&inherited)
            .filter(|dir| !["claude", "codex"].iter().any(|tool| dir.join(tool).exists()))
            .collect();
        for command in ["sed", "tee"] {
            let found = std::env::split_paths(&inherited)
                .map(|dir| dir.join(command))
                .find(|candidate| candidate.is_file())
                .ok_or_else(|| format!("no {command} on the PATH"))?;
            std::os::unix::fs::symlink(found, commands.join(command))?;
        }
        let program = bin.join("claude");
        fs::write(&program, FAKE_CLAUDE.replace("__STATE__", &state.display().to_string()))?;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755))?;
        let path = std::env::join_paths([bin.clone(), commands].into_iter().chain(kept))?
            .into_string()
            .map_err(|_| "a PATH that isn't UTF-8")?;
        Ok(Self { bin, state, home, path })
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.state.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// The user at the sandbox: its keyboard, its screenshots, the child it asks, the fake AI server and the made-up tool.
struct User<'a> {
    sandbox: &'a Sandbox,
    running: Running,
    input: VirtualInput,
    tool: &'a FakeTool,
    ai: &'a FakeAi,
    run: &'static str,
    settings_file: PathBuf,
    asked: u32,
    before_restart: Option<Value>,
}

impl<'a> User<'a> {
    fn start(
        sandbox: &'a Sandbox,
        running: Running,
        tool: &'a FakeTool,
        ai: &'a FakeAi,
        run: &'static str,
        settings_file: PathBuf,
    ) -> SandboxResult<Self> {
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| format!("no ready line: {error:?}"))?;
        let user = Self {
            sandbox,
            running,
            input: sandbox.input()?,
            tool,
            ai,
            run,
            settings_file,
            asked: 0,
            before_restart: None,
        };
        thread::sleep(SETTLE);
        Ok(user)
    }

    fn run_check(&mut self, name: &str, check: fn(&mut User<'_>) -> SandboxResult, shared: &mut Shared) {
        say(&format!("check {name} ({} run)", self.run));
        match check(self) {
            Ok(()) => {
                say(&format!("  pass {name}"));
                shared.passed.push(name.to_owned());
            }
            Err(error) => {
                say(&format!("  FAIL {name}: {error}"));
                let _ = self.shot(&format!("failed-{name}"));
                if let Ok(state) = self.state() {
                    say(&format!("  state: {state}"));
                }
                shared.failed.push((name.to_owned(), error.to_string()));
                let _ = self.ask("cancel");
                let _ = self.ask("theme-light");
            }
        }
    }

    fn ask(&mut self, verb: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = self.sandbox.path(&format!("{ASK}.staged"));
        fs::write(&staged, format!("{} {verb}", self.asked))?;
        fs::rename(&staged, self.sandbox.path(ASK))?;
        let prefix = format!("re {} ", self.asked);
        let line = self
            .running
            .line_where(WAIT, |line| line.starts_with(&prefix))
            .map_err(|error| format!("no answer to {verb}: {error:?}"))?;
        Ok(line[prefix.len()..].to_owned())
    }

    fn ok(&mut self, verb: &str) -> SandboxResult {
        let answer = self.ask(verb)?;
        ensure(answer == "ok", || format!("{verb}: {answer}"))
    }

    fn state(&mut self) -> SandboxResult<Value> {
        Ok(serde_json::from_str(&self.ask("state")?)?)
    }

    fn until(&mut self, what: &str, wait: Duration, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let start = Instant::now();
        loop {
            let state = self.state()?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > wait {
                return Err(format!("{what}: never came; last {state}").into());
            }
            thread::sleep(POLL);
        }
    }

    fn press(&mut self, spec: &str) -> SandboxResult {
        let (modifiers, key) =
            ui_sandbox::input::parse_key(spec, self.input.keymap()).ok_or_else(|| format!("no key {spec}"))?;
        self.input.key_with(&modifiers, key)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn type_text(&mut self, text: &str) -> SandboxResult {
        self.input.type_text(text)?;
        // the app takes the keys in its own time: the next step waits until all of them arrived
        let typed = u64::try_from(text.chars().count()).unwrap_or(u64::MAX);
        thread::sleep(STEP + Duration::from_millis(400 + 70_u64.saturating_mul(typed)));
        Ok(())
    }

    /// The wheel over the middle of the settings page, `steps` down (up when negative).
    fn scroll(&mut self, steps: i32) -> SandboxResult {
        self.input.move_to(860, 520)?;
        self.input.scroll(860, 520, steps)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// Empties the focused field and types `text` in it.
    fn replace(&mut self, text: &str) -> SandboxResult {
        self.press("ctrl+a")?;
        self.press("BackSpace")?;
        if !text.is_empty() {
            self.type_text(text)?;
        }
        Ok(())
    }

    fn shot(&self, name: &str) -> SandboxResult<PathBuf> {
        let file = format!("qa516-{name}.png");
        let png = self.sandbox.screenshot(&file, false)?;
        fs::copy(&png, shots_dir()?.join(&file))?;
        if let Some(dir) = evidence_dir() {
            fs::copy(&png, dir.join(&file))?;
        }
        Ok(png)
    }

    /// The same screen in Teams Light and Teams Dark; back in Light after.
    fn shots(&mut self, name: &str) -> SandboxResult {
        self.input.move_to(300, 12)?;
        thread::sleep(SETTLE);
        self.shot(&format!("{name}-light"))?;
        self.ok("theme-dark")?;
        thread::sleep(SETTLE);
        self.shot(&format!("{name}-dark"))?;
        self.ok("theme-light")?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn settings_text(&self) -> String {
        fs::read_to_string(&self.settings_file).unwrap_or_default()
    }

    /// The providers as the window lists them.
    fn providers(&mut self) -> SandboxResult<Vec<Value>> {
        Ok(self.state()?["providers"].as_array().cloned().unwrap_or_default())
    }

    /// Opens the editor of a new `kind`, types its fields (Tab between them; an empty one is left as it is) and
    /// tests the connection when `test`; what the test said.
    fn new_provider(&mut self, kind: &str, fields: [&str; 4], test: bool) -> SandboxResult<Option<String>> {
        self.ok("open-settings")?;
        self.ok(&format!("add {kind}"))?;
        thread::sleep(SETTLE);
        self.fill(fields)?;
        if !test {
            return Ok(None);
        }
        self.tested()
    }

    /// Types `fields` (name, base URL, model, key) into the open editor, from its name field (focused when it opens).
    fn fill(&mut self, fields: [&str; 4]) -> SandboxResult {
        for (at, field) in fields.iter().enumerate() {
            if at > 0 {
                self.press("Tab")?;
            }
            if !field.is_empty() {
                self.replace(field)?;
            }
        }
        Ok(())
    }

    /// "Test connection", and what it says once it ends.
    fn tested(&mut self) -> SandboxResult<Option<String>> {
        self.ok("test")?;
        let state = self.until("the test's end", WAIT, |state| {
            state["tested"]
                .as_str()
                .is_some_and(|said| !said.starts_with("Testing"))
        })?;
        Ok(state["tested"].as_str().map(str::to_owned))
    }

    /// Empties the chat's box, types `draft` and translates it (Ctrl+Shift+Y); the box's state once it ends.
    fn translate(&mut self, draft: &str, wait: Duration) -> SandboxResult<Value> {
        self.ok("open-ab")?;
        thread::sleep(SETTLE);
        self.ok("focus-box")?;
        self.replace(draft)?;
        self.press("ctrl+shift+y")?;
        let state = self.until("the translation's end", wait, |state| {
            let status = state["box"]["status"].as_str().unwrap_or_default();
            status != "running" && state["box"]["text"].as_str() != Some(draft) || status.starts_with("failed")
        })?;
        Ok(state["box"].clone())
    }

    /// Chooses `tool` (a provider id or `automatic`) for `work`, with `model` (`recommended`, `default` or a name).
    fn choose(&mut self, work: &str, tool: &str, model: &str) -> SandboxResult {
        self.ok(&format!("choose {work} {tool} {model}"))
    }

    /// Edits provider `id`: its field at `field` (0 name, 1 base URL, 2 model, 3 key) replaced by `text`, then Enter
    /// (which saves).
    fn edit_field(&mut self, id: &str, field: usize, text: &str) -> SandboxResult {
        self.ok("open-settings")?;
        self.ok(&format!("edit {id}"))?;
        thread::sleep(SETTLE);
        for _ in 0..field {
            self.press("Tab")?;
        }
        self.replace(text)?;
        self.press("Return")?;
        self.until("the editor closed", WAIT, |state| state["editing"].is_null())?;
        Ok(())
    }
}

/// The provider `id` in `list`.
fn provider<'v>(list: &'v [Value], id: &str) -> Option<&'v Value> {
    list.iter().find(|provider| provider["id"] == id)
}

/// The ids of `list`, in its order.
fn ids(list: &[Value]) -> Vec<String> {
    list.iter()
        .filter_map(|provider| provider["id"].as_str().map(str::to_owned))
        .collect()
}

// --- the checks ---

fn earlier_build_file_keeps_its_choices(user: &mut User<'_>) -> SandboxResult {
    let state = user.state()?;
    let choices = &state["choices"];
    let expected = json!({
        "draft": "claude/opus", "correction": "codex/recommended", "chat": "automatic/recommended",
        "captions": "claude/haiku", "summary": "codex/gpt-6.1-terra",
    });
    ensure(*choices == expected, || {
        format!("the earlier choices: {choices}, expected {expected}")
    })?;
    let list = state["providers"].as_array().cloned().unwrap_or_default();
    ensure(ids(&list) == ["claude"], || {
        format!("the providers of an earlier file: {list:?}")
    })?;
    user.ok("open-settings")?;
    user.shots("earlier-settings")?;
    // the draft goes to the made-up claude with the model the earlier file chose
    let calls = user.tool.calls().len();
    let done = user.translate("hello earlier build", WAIT)?;
    let text = done["text"].as_str().unwrap_or_default();
    ensure(text.starts_with("[cli:opus] hello earlier build"), || {
        format!("the draft: {done}")
    })?;
    ensure(user.tool.calls().len() > calls, || {
        "the made-up claude did not start".to_owned()
    })?;
    // Codex is not installed: the correction says so
    user.ok("focus-box")?;
    user.replace("see teh notes")?;
    user.press("ctrl+shift+g")?;
    let state = user.until("the correction's end", WAIT, |state| {
        state["box"]["correction"]
            .as_str()
            .is_some_and(|said| said.starts_with("Failed"))
    })?;
    let said = state["box"]["correction"].as_str().unwrap_or_default().to_owned();
    ensure(said.contains("Codex"), || {
        format!("the correction with Codex missing: {said}")
    })?;
    user.shots("earlier-codex-missing")?;
    user.press("Escape")?;
    let file = user.settings_text();
    ensure(!file.contains("ai_providers"), || {
        format!("the file got a provider list: {file}")
    })?;
    Ok(())
}

fn cli_found_is_added_and_on(user: &mut User<'_>) -> SandboxResult {
    user.ok("open-settings")?;
    let list = user.providers()?;
    let claude = provider(&list, "claude").ok_or("no Claude Code provider")?;
    ensure(claude["enabled"] == true && claude["kind"] == "claude", || {
        format!("claude: {claude}")
    })?;
    ensure(provider(&list, "codex").is_none(), || {
        "a Codex provider without the CLI".to_owned()
    })?;
    ensure(list.len() == 1, || format!("the list: {list:?}"))?;
    let state = user.state()?;
    ensure(state["choices"]["draft"] == "automatic/recommended", || {
        format!("{}", state["choices"])
    })?;
    user.shots("list-cli-found")?;
    Ok(())
}

fn add_anthropic_and_test(user: &mut User<'_>) -> SandboxResult {
    let base = user.ai.base("ok");
    let tested = user.new_provider("anthropic", ["QA Anthropic", &base, "", KEY_ANTHROPIC], true)?;
    ensure(tested.as_deref() == Some("Connected: 3 models"), || {
        format!("the test: {tested:?}")
    })?;
    let probe = user.ai.all().pop().ok_or("no request")?;
    ensure(probe.path == "/ok/v1/models?limit=1000", || {
        format!("the test asked {}", probe.path)
    })?;
    ensure(probe.header("x-api-key") == Some(KEY_ANTHROPIC), || {
        "the test's key".to_owned()
    })?;
    ensure(probe.header("anthropic-version").is_some(), || {
        "no anthropic-version".to_owned()
    })?;
    user.shots("editor-anthropic-connected")?;
    user.ok("save")?;
    let state = user.until("api-1 saved", WAIT, |state| state["editing"].is_null())?;
    let list = state["providers"].as_array().cloned().unwrap_or_default();
    let api = provider(&list, "api-1").ok_or_else(|| format!("no api-1: {list:?}"))?;
    ensure(api["name"] == "QA Anthropic" && api["kind"] == "anthropic", || {
        format!("api-1: {api}")
    })?;
    ensure(api["has_key"] == true && api["base_url"] == base.as_str(), || {
        format!("api-1: {api}")
    })?;
    ensure(state["vault"]["api-1"] == true, || {
        format!("the vault: {}", state["vault"])
    })?;
    let file = user.settings_text();
    ensure(file.contains("key = \"keyring\""), || format!("the file: {file}"))?;
    ensure(!file.contains(KEY_ANTHROPIC), || {
        "the key is in the settings file".to_owned()
    })?;
    // the CLI found joined the list in the file, before the new provider
    ensure(ids(&list) == ["claude", "api-1"], || {
        format!("the order: {:?}", ids(&list))
    })?;
    Ok(())
}

fn add_openai_bad_key_then_good(user: &mut User<'_>) -> SandboxResult {
    let base = format!("{}/v1", user.ai.base("ok"));
    let tested = user.new_provider("openai", ["QA OpenAI", &base, "", KEY_WRONG], true)?;
    let said = tested.unwrap_or_default();
    ensure(said.contains("refused the API key"), || {
        format!("a wrong key's test: {said}")
    })?;
    ensure(!said.contains(KEY_WRONG), || {
        format!("the test repeats the key: {said}")
    })?;
    user.shots("editor-openai-bad-key")?;
    // the key field still has the focus: the right key typed over the wrong one
    user.replace(KEY_OPENAI)?;
    let tested = user.tested()?;
    ensure(tested.as_deref() == Some("Connected: 3 models"), || {
        format!("the good key's test: {tested:?}")
    })?;
    let probe = user.ai.all().pop().ok_or("no request")?;
    ensure(probe.path == "/ok/v1/models", || {
        format!("the test asked {}", probe.path)
    })?;
    ensure(
        probe.header("authorization") == Some(&format!("Bearer {KEY_OPENAI}")),
        || "the bearer key".to_owned(),
    )?;
    user.ok("save")?;
    let state = user.until("api-2 saved", WAIT, |state| state["editing"].is_null())?;
    let list = state["providers"].as_array().cloned().unwrap_or_default();
    let api = provider(&list, "api-2").ok_or_else(|| format!("no api-2: {list:?}"))?;
    ensure(api["has_key"] == true && api["kind"] == "openai", || {
        format!("api-2: {api}")
    })?;
    let file = user.settings_text();
    ensure(!file.contains(KEY_OPENAI) && !file.contains(KEY_WRONG), || {
        "a key in the file".to_owned()
    })?;
    Ok(())
}

fn add_compatible_long_name(user: &mut User<'_>) -> SandboxResult {
    let base = format!("{}/v1", user.ai.base("compat"));
    let too_long = format!("{LONG_NAME}x");
    user.new_provider("openai-compatible", [&too_long, &base, "local-qa-1", ""], false)?;
    user.ok("save")?;
    let state = user.until("the name refused", WAIT, |state| state["problem"].is_string())?;
    let problem = state["problem"].as_str().unwrap_or_default().to_owned();
    ensure(problem.contains("shorter name"), || format!("a long name: {problem}"))?;
    user.ok("cancel")?;
    let from = user.ai.count();
    let tested = user.new_provider("openai-compatible", [LONG_NAME, &base, "local-qa-1", ""], true)?;
    let asked = user.ai.since(from);
    // saved also when the test failed, so the next checks have the provider
    user.ok("save")?;
    ensure(tested.as_deref() == Some("Connected: 2 models"), || {
        format!(
            "the server's test: {tested:?}; requests that reached the server: {}",
            asked.len()
        )
    })?;
    let probe = asked.last().ok_or("no request")?;
    ensure(probe.header("authorization").is_none(), || {
        "a key went to the server".to_owned()
    })?;
    let state = user.until("api-3 saved", WAIT, |state| state["editing"].is_null())?;
    let list = state["providers"].as_array().cloned().unwrap_or_default();
    let api = provider(&list, "api-3").ok_or_else(|| format!("no api-3: {list:?}"))?;
    ensure(api["name"] == LONG_NAME, || format!("api-3's name: {}", api["name"]))?;
    ensure(api["has_key"] == false && api["model"] == "local-qa-1", || {
        format!("api-3: {api}")
    })?;
    user.shots("list-three-apis")?;
    Ok(())
}

fn pick_per_feature(user: &mut User<'_>) -> SandboxResult {
    user.choose("draft", "api-1", "recommended")?;
    user.choose("correction", "claude", "recommended")?;
    user.choose("chat", "api-2", "recommended")?;
    let state = user.until("the models listed", WAIT, |state| {
        state["listed"]["api-1"]
            .as_array()
            .is_some_and(|models| !models.is_empty())
            && state["listed"]["api-2"]
                .as_array()
                .is_some_and(|models| !models.is_empty())
    })?;
    let choices = &state["choices"];
    ensure(choices["draft"] == "api-1/recommended", || format!("{choices}"))?;
    ensure(choices["correction"] == "claude/recommended", || format!("{choices}"))?;
    ensure(choices["chat"] == "api-2/recommended", || format!("{choices}"))?;
    let listed = state["listed"]["api-1"].to_string();
    ensure(listed.contains("claude-qa-listed-1"), || {
        format!("api-1 lists {listed}")
    })?;
    let file = user.settings_text();
    ensure(
        file.contains("[message_translator]") && file.contains("tool = \"api-1\""),
        || format!("the file: {file}"),
    )?;
    user.ok("open-settings")?;
    user.scroll(12)?;
    user.shots("pickers-api")?;
    user.scroll(-30)?;
    Ok(())
}

fn draft_through_anthropic(user: &mut User<'_>) -> SandboxResult {
    let from = user.ai.count();
    let done = user.translate("hello team", WAIT)?;
    let text = done["text"].as_str().unwrap_or_default();
    ensure(text == "[anthropic:claude-sonnet-5-5] hello team", || {
        format!("the draft: {done}")
    })?;
    let asked = user.ai.since(from);
    let request = asked
        .iter()
        .find(|seen| seen.path.ends_with("/v1/messages"))
        .ok_or("no messages request")?;
    ensure(request.path == "/ok/v1/messages", || request.path.clone())?;
    ensure(request.header("x-api-key") == Some(KEY_ANTHROPIC), || {
        "the key".to_owned()
    })?;
    ensure(request.header("authorization").is_none(), || {
        "an authorization header".to_owned()
    })?;
    let body = request.json();
    ensure(
        body["stream"] == true && body["output_config"]["effort"] == "low",
        || format!("{body}"),
    )?;
    user.shots("draft-translated-anthropic")?;
    Ok(())
}

fn correction_through_cli(user: &mut User<'_>) -> SandboxResult {
    let (from, calls) = (user.ai.count(), user.tool.calls().len());
    user.ok("open-ab")?;
    user.ok("focus-box")?;
    user.replace("see teh notes")?;
    user.press("ctrl+shift+g")?;
    let state = user.until("the correction", WAIT, |state| state["box"]["correction"] == "Proposed")?;
    ensure(user.tool.calls().len() > calls, || {
        "the made-up claude did not start".to_owned()
    })?;
    let to_api = user.ai.since(from);
    ensure(to_api.is_empty(), || {
        format!("the CLI's work went to the API: {to_api:?}")
    })?;
    let _ = state;
    user.shots("correction-cli")?;
    user.press("Escape")?;
    Ok(())
}

fn correction_other_model_compatible(user: &mut User<'_>) -> SandboxResult {
    user.choose("correction", "api-3", "recommended")?;
    user.ok("other-model correction")?;
    thread::sleep(SETTLE);
    user.type_text("qa-other-model-7")?;
    user.press("Return")?;
    let state = user.until("the model typed", WAIT, |state| {
        state["choices"]["correction"] == "api-3/qa-other-model-7"
    })?;
    let _ = state;
    let from = user.ai.count();
    user.ok("open-ab")?;
    user.ok("focus-box")?;
    user.replace("see teh notes")?;
    user.press("ctrl+shift+g")?;
    user.until("the correction", WAIT, |state| state["box"]["correction"] == "Proposed")?;
    let asked = user.ai.since(from);
    let request = asked
        .iter()
        .find(|seen| seen.path.ends_with("/chat/completions"))
        .ok_or_else(|| format!("no completions request: {asked:?}"))?;
    ensure(request.path == "/compat/v1/chat/completions", || request.path.clone())?;
    ensure(request.model() == "qa-other-model-7", || request.body.clone())?;
    ensure(request.header("authorization").is_none(), || {
        "a key went to the server".to_owned()
    })?;
    ensure(request.json().get("reasoning_effort").is_none(), || {
        "an effort to a compatible server".to_owned()
    })?;
    user.shots("correction-compatible-other-model")?;
    user.press("Escape")?;
    Ok(())
}

fn chat_translation_through_openai(user: &mut User<'_>) -> SandboxResult {
    let from = user.ai.count();
    user.ok("open-ab")?;
    user.ok("chat-lang German")?;
    let state = user.until("the chat translated", Duration::from_secs(60), |state| {
        let progress = &state["chat_translation"]["progress"];
        progress["total"].as_u64().is_some_and(|total| total > 0) && progress["done"] == progress["total"]
            || state["chat_translation"]["status"]
                .as_str()
                .is_some_and(|status| status.starts_with("Failed"))
    })?;
    let status = state["chat_translation"]["status"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    ensure(status == "On", || format!("the chat's translation: {state}"))?;
    let asked = user.ai.since(from);
    let request = asked
        .iter()
        .find(|seen| seen.path.ends_with("/chat/completions"))
        .ok_or("no completions request")?;
    ensure(request.path == "/ok/v1/chat/completions", || request.path.clone())?;
    ensure(request.model() == "gpt-6-luna", || request.body.clone())?;
    ensure(
        request.header("authorization") == Some(&format!("Bearer {KEY_OPENAI}")),
        || "the bearer key".to_owned(),
    )?;
    ensure(request.json()["reasoning_effort"] == "low", || request.body.clone())?;
    user.shots("chat-translated-openai")?;
    user.ok("chat-lang off")?;
    Ok(())
}

fn automatic_follows_order_and_switches(user: &mut User<'_>) -> SandboxResult {
    user.choose("draft", "automatic", "recommended")?;
    let list = user.providers()?;
    ensure(ids(&list) == ["claude", "api-1", "api-2", "api-3"], || {
        format!("the order: {:?}", ids(&list))
    })?;
    let done = user.translate("one", WAIT)?;
    ensure(done["text"] == "[cli:sonnet] one", || {
        format!("Automatic with Claude Code first: {done}")
    })?;
    user.ok("up api-1")?;
    let done = user.translate("two", WAIT)?;
    ensure(done["text"] == "[anthropic:claude-sonnet-5-5] two", || {
        format!("Automatic with the Anthropic API first: {done}")
    })?;
    user.ok("enable api-1 off")?;
    let done = user.translate("three", WAIT)?;
    ensure(done["text"] == "[cli:sonnet] three", || {
        format!("Automatic with api-1 off: {done}")
    })?;
    user.ok("enable claude off")?;
    let done = user.translate("four", WAIT)?;
    ensure(done["text"] == "[openai:gpt-6.1-terra] four", || {
        format!("Automatic with Claude Code off: {done}")
    })?;
    user.ok("open-settings")?;
    user.shots("list-reordered-two-off")?;
    let list = user.providers()?;
    ensure(ids(&list) == ["api-1", "claude", "api-2", "api-3"], || {
        format!("the order: {:?}", ids(&list))
    })?;
    // all off: Automatic has none
    user.ok("enable api-2 off")?;
    user.ok("enable api-3 off")?;
    let done = user.translate("five", WAIT)?;
    let status = done["status"].as_str().unwrap_or_default();
    ensure(status.contains("No AI provider"), || format!("all off: {done}"))?;
    user.shots("draft-no-provider-on")?;
    for id in ["api-1", "claude", "api-2", "api-3"] {
        user.ok(&format!("enable {id} on"))?;
    }
    Ok(())
}

fn failures_say_why(user: &mut User<'_>) -> SandboxResult {
    user.choose("draft", "api-2", "recommended")?;
    let dead = dead_base()?;
    let cases = [
        (
            "badkey",
            format!("{}/v1", user.ai.base("badkey")),
            "refused the API key",
        ),
        ("nocredit", format!("{}/v1", user.ai.base("nocredit")), "no credit left"),
        ("ratelimit", format!("{}/v1", user.ai.base("ratelimit")), "rate limit"),
        (
            "words",
            format!("{}/v1", user.ai.base("words")),
            "The QA server is down for upkeep",
        ),
        ("dead", format!("{dead}/v1"), "could not be reached"),
    ];
    let mut wrong = Vec::new();
    for (name, base, words) in cases {
        user.edit_field("api-2", 1, &base)?;
        let done = user.translate(&format!("failure {name}"), WAIT)?;
        let status = done["status"].as_str().unwrap_or_default().to_owned();
        say(&format!("  {name}: {status}"));
        if !status.starts_with("failed") || !status.contains(words) {
            wrong.push(format!("{name}: {status}"));
        }
        if [KEY_OPENAI, KEY_ANTHROPIC].iter().any(|key| status.contains(key)) {
            wrong.push(format!("{name} shows the key: {status}"));
        }
        if done["text"] != format!("failure {name}") {
            wrong.push(format!("{name} changed the draft: {}", done["text"]));
        }
        if name == "nocredit" {
            user.shots("draft-failed-no-credit")?;
        }
        if name == "words" {
            user.shots("draft-failed-api-words")?;
        }
    }
    user.edit_field("api-2", 1, &format!("{}/v1", user.ai.base("ok")))?;
    // a model that the API does not know, typed under "Other model…"
    user.choose("draft", "api-2", "no-such-model")?;
    let done = user.translate("failure model", WAIT)?;
    let status = done["status"].as_str().unwrap_or_default().to_owned();
    say(&format!("  unknown model: {status}"));
    if !status.contains("does not know the model no-such-model") {
        wrong.push(format!("unknown model: {status}"));
    }
    user.choose("draft", "api-2", "recommended")?;
    ensure(wrong.is_empty(), || wrong.join("; "))
}

fn slow_endpoint_stops(user: &mut User<'_>) -> SandboxResult {
    user.choose("draft", "api-2", "recommended")?;
    user.edit_field("api-2", 1, &format!("{}/v1", user.ai.base("slow")))?;
    let dropped = user.ai.dropped().len();
    user.ok("open-ab")?;
    user.ok("focus-box")?;
    user.replace("slow one")?;
    user.press("ctrl+shift+y")?;
    user.until("running", WAIT, |state| state["box"]["status"] == "running")?;
    thread::sleep(Duration::from_secs(3));
    user.shots("draft-running-slow")?;
    user.press("Escape")?;
    let state = user.until("stopped", WAIT, |state| state["box"]["status"] != "running")?;
    ensure(state["box"]["text"] == "slow one", || {
        format!("the draft after a stop: {}", state["box"])
    })?;
    let start = Instant::now();
    while user.ai.dropped().len() == dropped && start.elapsed() < Duration::from_secs(5) {
        thread::sleep(POLL);
    }
    ensure(user.ai.dropped().len() > dropped, || {
        "the request stayed open after the stop".to_owned()
    })?;
    // no stop: the app gives up at its deadline (90 s for a draft)
    let done = user.translate("slow two", Duration::from_secs(150))?;
    let status = done["status"].as_str().unwrap_or_default().to_owned();
    say(&format!("  slow: {status}"));
    ensure(status.contains("took too long"), || {
        format!("a server that never answers: {status}")
    })?;
    user.edit_field("api-2", 1, &format!("{}/v1", user.ai.base("ok")))?;
    Ok(())
}

fn remove_key(user: &mut User<'_>) -> SandboxResult {
    user.ok("open-settings")?;
    user.ok("edit api-1")?;
    user.ok("remove-key")?;
    let state = user.until("the key removed", WAIT, |state| {
        provider(state["providers"].as_array().map_or(&[][..], Vec::as_slice), "api-1")
            .is_some_and(|api| api["has_key"] == false)
    })?;
    ensure(state["vault"]["api-1"] == false, || {
        format!("the vault: {}", state["vault"])
    })?;
    user.shots("editor-key-removed")?;
    user.ok("cancel")?;
    let file = user.settings_text();
    let api_1 = file
        .split("[[ai_providers]]")
        .find(|entry| entry.contains("\"api-1\""))
        .unwrap_or_default();
    ensure(!api_1.contains("key ="), || format!("api-1 in the file: {api_1}"))?;
    user.choose("draft", "api-1", "recommended")?;
    let done = user.translate("no key", WAIT)?;
    let status = done["status"].as_str().unwrap_or_default().to_owned();
    ensure(status.contains("has no API key"), || {
        format!("a provider without a key: {status}")
    })?;
    // the key typed again, for the restart
    user.edit_field("api-1", 3, KEY_ANTHROPIC)?;
    let state = user.state()?;
    ensure(state["vault"]["api-1"] == true, || {
        format!("the vault: {}", state["vault"])
    })?;
    Ok(())
}

fn many_providers(user: &mut User<'_>) -> SandboxResult {
    let base = format!("{}/v1", user.ai.base("compat"));
    for n in 1..=6 {
        user.new_provider(
            "openai-compatible",
            [&format!("QA server {n}"), &base, "local-qa-2", ""],
            false,
        )?;
        user.ok("save")?;
        user.until("saved", WAIT, |state| state["editing"].is_null())?;
    }
    let list = user.providers()?;
    ensure(list.len() == 10, || format!("ten providers: {:?}", ids(&list)))?;
    user.ok("open-settings")?;
    user.shots("list-ten-providers")?;
    for id in ids(&list)
        .iter()
        .filter(|id| !["claude", "api-1", "api-2", "api-3"].contains(&id.as_str()))
    {
        user.ok(&format!("remove {id}"))?;
    }
    let list = user.providers()?;
    ensure(list.len() == 4, || format!("after the removals: {:?}", ids(&list)))?;
    Ok(())
}

fn no_teams_token_reaches_the_ai_server(user: &mut User<'_>) -> SandboxResult {
    let token = fs::read_to_string(user.sandbox.path(TEAMS_TOKEN))?;
    ensure(token.len() > 8, || "no Teams token was noted".to_owned())?;
    let mut wrong = Vec::new();
    let all = user.ai.all();
    for seen in &all {
        for (name, value) in &seen.headers {
            if value.contains(&token) || seen.body.contains(&token) {
                wrong.push(format!("the Teams token in {}", seen.path));
            }
            if ["authentication", "x-skypetoken", "cookie", "x-ms-client-request-id"].contains(&name.as_str()) {
                wrong.push(format!("header {name} to {}", seen.path));
            }
        }
    }
    say(&format!("  {} requests to the fake AI server examined", all.len()));
    ensure(wrong.is_empty(), || wrong.join("; "))
}

fn restart_keeps_choices(user: &mut User<'_>) -> SandboxResult {
    let before = user.before_restart.clone().ok_or("no state of the first run")?;
    let state = user.state()?;
    ensure(state["choices"] == before["choices"], || {
        format!("the choices: {} before, {} after", before["choices"], state["choices"])
    })?;
    let strip = |state: &Value| -> Vec<Value> {
        state["providers"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|mut provider| {
                provider.as_object_mut().map(|fields| fields.remove("ready"));
                provider
            })
            .collect()
    };
    ensure(strip(&state) == strip(&before), || {
        format!("the providers: {:?} before, {:?} after", strip(&before), strip(&state))
    })?;
    user.ok("open-settings")?;
    user.shots("list-after-restart")?;
    // the keyring kept the Anthropic key: the draft works
    user.choose("draft", "api-1", "recommended")?;
    let done = user.translate("after restart", WAIT)?;
    ensure(done["text"] == "[anthropic:claude-sonnet-5-5] after restart", || {
        format!("the draft after the restart: {done}")
    })?;
    // the file says that the OpenAI key is in the keyring, but the keyring lost it
    user.choose("draft", "api-2", "recommended")?;
    let done = user.translate("lost key", WAIT)?;
    let status = done["status"].as_str().unwrap_or_default().to_owned();
    say(&format!("  lost key: {status}"));
    ensure(status.contains("has no API key"), || {
        format!("a key that the keyring lost: {status}")
    })?;
    user.shots("draft-key-lost")?;
    Ok(())
}

// --- the child: the real app on the mock ---

struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

impl Tokens {
    fn access(&self) -> AccessToken {
        AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: std::time::SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }
    }
}

/// The mock and a's engine on it.
struct MockLab {
    _runtime: tokio::runtime::Runtime,
    _mock: ChatServiceMock,
    _trouter: TrouterMock,
    dir: tempfile::TempDir,
}

impl MockLab {
    fn start() -> SandboxResult<(Self, EngineHandle, String)> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        mock.post_as(LAB_B, AB_CHAT, "<p>good morning, the QA notes are ready</p>")
            .ok_or("b's post refused")?;
        let dir = tempfile::tempdir()?;
        let token = mock.token(LAB_A).ok_or("the mock's token for a")?;
        let tokens = Arc::new(Tokens {
            token: token.clone(),
            renewals: broadcast::channel(4).0,
        });
        let origin = mock.origin();
        let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
        let transport = Arc::new(http()?);
        let trouter_client = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), trouter.origin())?
            .with_options(Options {
                backoff_base: Duration::from_millis(50),
                backoff_cap: Duration::from_millis(50),
                backoff_jitter_percent: 0,
                ..Options::default()
            });
        let author = Author::new(Mri::new(LAB_A), display_name("bot-a@lab.example.test"));
        let engine = engine::start(Parts {
            calls: None,
            store: Store::open(&support::store_file(dir.path(), Alias::A))?,
            client: Arc::new(Client::new(
                http()?
                    .with_ams(AMS_BASE)?
                    .with_presence(PRESENCE_BASE)?
                    .with_csa(CSA_BASE)?
                    .with_user_profile(USER_PROFILE_BASE)?
                    .with_provisioning(PROVISIONING_BASE)?
                    .with_middle_tier(MIDDLE_TIER_BASE)?,
            )),
            outbox: Outbox::new(transport, author),
            push: Some(Push::new(trouter_client)),
            options: EngineOptions::default(),
            known_names: vec![
                (LAB_B.to_owned(), "Lab B".to_owned()),
                (LAB_C.to_owned(), "Lab C".to_owned()),
            ],
        })?;
        let lab = Self {
            _runtime: runtime,
            _mock: mock,
            _trouter: trouter,
            dir,
        };
        Ok((lab, engine, token))
    }
}

/// The child's `PATH` holds no tool but the made-up one.
fn only_the_made_up_tool() -> SandboxResult {
    let made_up = PathBuf::from(std::env::var_os(TOOLS_ENV).ok_or("no made-up tool's directory")?);
    let found = teams_ui::local_ai::Tools::detect();
    for backend in [Backend::Claude, Backend::Codex] {
        if let Some(program) = found.program(backend)
            && !program.starts_with(&made_up)
        {
            return Err(format!("a real tool is on the PATH: {}", program.display()).into());
        }
    }
    Ok(())
}

/// The app's secret store, in memory, with the keys of the seed file in it (as a keyring keeps them).
fn secrets() -> SandboxResult<AppSecrets> {
    let secrets = AppSecrets::in_memory();
    let seed = fs::read_to_string(std::env::var_os(SEED_ENV).ok_or("no seed file")?)?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    for line in seed.lines() {
        if let Some((id, key)) = line.split_once('=') {
            let name = SecretKey::new(ngteams::ai_keys::AI_ACCOUNT, id);
            runtime.block_on(secrets.put(&name, &Secret::new(key.to_owned())))?;
        }
    }
    Ok(secrets)
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    only_the_made_up_tool()?;
    let settings_path = PathBuf::from(std::env::var_os(SETTINGS_ENV).ok_or("no settings path")?);
    let data = PathBuf::from(std::env::var_os(DATA_ENV).ok_or("no data dir")?);
    let diagnostics = app::install_log(data.join("logs"), teams_ui::settings::LogLevel::Debug);
    let (lab, engine, token) = MockLab::start()?;
    if let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) {
        fs::write(PathBuf::from(dir).join(TEAMS_TOKEN), &token)?;
    }
    let first = support::started(Alias::A, engine)?;
    let settings = teams_ui::SettingsFile::new(settings_path);
    let root = lab.dir.path();
    let paths = PersonalPaths::new(root.join("ngteams-config"), root.join("ngteams-data"));
    let runner = Runner {
        setup: Setup::new(paths)?.with_secrets(secrets()?),
        floor: CallFloor::new(),
        devices: None,
    };
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: Vec::new(),
                trace: Arc::new(teams_ui::ViewTrace::off()),
                ctl: None,
                settings: teams_ui::SettingsModel::load(settings).with_diagnostics(diagnostics),
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(launched) => {
                let window = launched.window;
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, 3, |_, _| say("ready"));
                });
                answer(window, cx);
            }
            Err(failed) => {
                *failed_in_app.lock().unwrap_or_else(PoisonError::into_inner) = Some(failed.error);
                app::quit_after(failed.stopped, cx);
            }
        }
    });
    if let Some(error) = failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
        return Err(error.into());
    }
    drop(lab);
    Ok(())
}

fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

/// Answers the parent's questions as they come, until it asks to quit.
fn answer(window: WindowHandle<KitRoot>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let ask = PathBuf::from(dir).join(ASK);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(10)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let (seq, verb) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            if verb == "quit" {
                say(&format!("re {seq} ok"));
                cx.update(|cx| cx.quit());
                return;
            }
            let reply = window
                .update(cx, |kit, window, cx| {
                    let shell = kit
                        .view()
                        .clone()
                        .downcast::<Root>()
                        .ok()?
                        .read(cx)
                        .shell_in(cx)
                        .cloned()?;
                    Some(on_shell(&shell, verb, window, cx))
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| "no shell".to_owned());
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn work_of(name: &str) -> Option<Work> {
    match name {
        "draft" => Some(Work::Draft),
        "correction" => Some(Work::Correction),
        "chat" => Some(Work::Chat),
        "captions" => Some(Work::Captions),
        "summary" => Some(Work::Summary),
        _ => None,
    }
}

const WORKS: [(&str, Work); 5] = [
    ("draft", Work::Draft),
    ("correction", Work::Correction),
    ("chat", Work::Chat),
    ("captions", Work::Captions),
    ("summary", Work::Summary),
];

fn settings_view(shell: &Entity<Shell>, cx: &App) -> Entity<SettingsView> {
    shell.read(cx).settings_view().clone()
}

fn providers_box(shell: &Entity<Shell>, cx: &mut App) -> Entity<AiProviders> {
    settings_view(shell, cx).update(cx, SettingsView::ai_providers)
}

fn picker(shell: &Entity<Shell>, work: Work, cx: &mut App) -> Entity<AiPicker> {
    let view = settings_view(shell, cx);
    view.update(cx, |view, cx| match work {
        Work::Draft => view.message_translation_ai_picker(cx),
        Work::Correction => view.correction_ai_picker(cx),
        Work::Chat => view.chat_ai_picker(cx),
        Work::Captions => view.ai_pickers(cx)[0].clone(),
        Work::Summary => view.ai_pickers(cx)[1].clone(),
    })
}

fn on_shell(shell: &Entity<Shell>, verb: &str, window: &mut Window, cx: &mut App) -> String {
    let words: Vec<&str> = verb.split_whitespace().collect();
    match words.as_slice() {
        ["open-settings"] => {
            if !matches!(shell.read(cx).main(), Main::Settings) {
                shell.update(cx, |shell, cx| {
                    shell.open_settings_page(teams_ui::settings_view::Page::General, window, cx);
                });
            }
        }
        ["open-ab"] => shell.update(cx, |shell, cx| shell.open_chat(AB_CHAT.to_owned(), window, cx)),
        ["focus-box"] => {
            let Some(chat) = shell.read(cx).chat().cloned() else {
                return "no chat".to_owned();
            };
            let compose = chat.read(cx).compose().clone();
            compose.update(cx, |compose, cx| compose.focus(window, cx));
        }
        [theme @ ("theme-dark" | "theme-light")] => {
            let key = if *theme == "theme-dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            let model = shell.read(cx).settings().clone();
            model.update(cx, |model, cx| {
                model.change(
                    |settings| {
                        settings.appearance.follow_system = false;
                        settings.appearance.theme = key;
                    },
                    cx,
                );
            });
        }
        ["choose", work, tool, model] => {
            let (Some(work), Some(tool), Some(model)) =
                (work_of(work), ToolChoice::from_key(tool), Model::from_key(model))
            else {
                return format!("bad choice {verb}");
            };
            let picker = picker(shell, work, cx);
            picker.update(cx, |picker, cx| {
                picker.choose_tool(tool, cx);
                picker.choose_model(model, cx);
            });
        }
        ["other-model", work] => {
            let Some(work) = work_of(work) else {
                return format!("no work {work}");
            };
            if !matches!(shell.read(cx).main(), Main::Settings) {
                shell.update(cx, |shell, cx| {
                    shell.open_settings_page(teams_ui::settings_view::Page::General, window, cx);
                });
            }
            picker(shell, work, cx).update(cx, AiPicker::type_model);
        }
        ["chat-lang", language] => {
            let Some(chat) = shell.read(cx).chat().cloned() else {
                return "no chat".to_owned();
            };
            let translation = chat.read(cx).translation().clone();
            let language: Option<&'static str> = match *language {
                "off" => None,
                "German" => Some("German"),
                other => return format!("no language {other}"),
            };
            translation.update(cx, |translation, cx| translation.set_language(language, cx));
        }
        ["state"] => return state_of(shell, window, cx).to_string(),
        _ => return on_providers(shell, &words, window, cx).unwrap_or_else(|| format!("unknown {verb}")),
    }
    "ok".to_owned()
}

/// A verb of the AI providers box (what its buttons do); `None`: not one of them.
fn on_providers(shell: &Entity<Shell>, words: &[&str], window: &mut Window, cx: &mut App) -> Option<String> {
    match words {
        ["add", kind] => {
            let Some(kind) = Backend::from_key(kind) else {
                return Some(format!("no kind {kind}"));
            };
            let providers = providers_box(shell, cx);
            providers.update(cx, |providers, cx| providers.add(kind, window, cx));
        }
        ["edit", id] => {
            let providers = providers_box(shell, cx);
            providers.update(cx, |providers, cx| providers.edit(id, window, cx));
        }
        ["save"] => providers_box(shell, cx).update(cx, AiProviders::save),
        ["cancel"] => providers_box(shell, cx).update(cx, AiProviders::cancel),
        ["test"] => providers_box(shell, cx).update(cx, AiProviders::test_connection),
        ["remove-key"] => providers_box(shell, cx).update(cx, AiProviders::remove_key),
        ["remove", id] => providers_box(shell, cx).update(cx, |providers, cx| providers.remove(id, cx)),
        ["enable", id, on] => {
            providers_box(shell, cx).update(cx, |providers, cx| providers.set_enabled(id, *on == "on", cx));
        }
        ["up", id] => providers_box(shell, cx).update(cx, |providers, cx| providers.move_by(id, true, cx)),
        ["down", id] => providers_box(shell, cx).update(cx, |providers, cx| providers.move_by(id, false, cx)),
        _ => return None,
    }
    Some("ok".to_owned())
}

/// The chat's box (its text, its translation's and its correction's state) and the chat's translation.
fn chat_state(shell: &Entity<Shell>, window: &Window, cx: &App) -> (Value, Value) {
    let Some(chat) = shell.read(cx).chat() else {
        return (Value::Null, Value::Null);
    };
    {
        let chat = chat.read(cx);
        let held = chat.compose().read(cx);
        let status = match held.draft_translation_status() {
            teams_ui::compose::DraftTranslationStatus::Idle => "idle".to_owned(),
            teams_ui::compose::DraftTranslationStatus::Running => "running".to_owned(),
            teams_ui::compose::DraftTranslationStatus::NothingToTranslate => "nothing".to_owned(),
            teams_ui::compose::DraftTranslationStatus::DraftChanged => "changed".to_owned(),
            teams_ui::compose::DraftTranslationStatus::Failed(why) => format!("failed: {why}"),
        };
        let correction = match held.correction_status() {
            teams_ui::compose::CorrectionStatus::Failed(why) => format!("Failed: {why}"),
            other => format!("{other:?}"),
        };
        let compose = json!({
            "text": held.editor().read(cx).doc().plain_text(),
            "status": status,
            "correction": correction,
            "focused": held.focused(window, cx),
        });
        let chat_translation = chat.translation().read(cx);
        let status = match chat_translation.status() {
            teams_ui::chat_translate::Status::Failed(failure) => format!("Failed: {}", failure.message()),
            other => format!("{other:?}"),
        };
        let progress = chat_translation.progress().map_or(
            Value::Null,
            |progress| json!({"done": progress.done, "total": progress.total}),
        );
        (compose, json!({"status": status, "progress": progress}))
    }
}

/// What the parent reads: the providers, the editor, the choices, the vault, the chat's box and its translation.
fn state_of(shell: &Entity<Shell>, window: &Window, cx: &mut App) -> Value {
    let providers = providers_box(shell, cx);
    let list = providers.update(cx, |providers, cx| providers.list(cx));
    let keys = AiKeys::of(cx);
    let mut vault = serde_json::Map::new();
    let mut listed = serde_json::Map::new();
    let shown: Vec<Value> = list
        .iter()
        .map(|provider| {
            if !provider.kind.is_cli() {
                let held = keys.get(&provider.id).ok().flatten().is_some();
                vault.insert(provider.id.clone(), json!(held));
                if let Some(Ok(models)) = ApiModels::of(provider, cx) {
                    let ids: Vec<String> = models.iter().map(|model| model.id.clone()).collect();
                    listed.insert(provider.id.clone(), json!(ids));
                }
            }
            json!({
                "id": provider.id, "kind": provider.kind.key(), "name": provider.name, "enabled": provider.enabled,
                "has_key": provider.has_key, "base_url": provider.base_url, "model": provider.model,
            })
        })
        .collect();
    let held = providers.read(cx);
    let (editing, tested, problem) = (
        held.editing().map(str::to_owned),
        held.tested().map(|said| said.to_string()),
        held.problem().map(|said| said.to_string()),
    );
    let settings = shell.read(cx).settings().read(cx).settings().clone();
    let choices: serde_json::Map<String, Value> = WORKS
        .iter()
        .map(|(name, work)| {
            let choice = settings.ai(*work);
            (
                (*name).to_owned(),
                json!(format!("{}/{}", choice.tool.key(), choice.model.key())),
            )
        })
        .collect();
    let (compose, translation) = chat_state(shell, window, cx);
    let main = match shell.read(cx).main() {
        Main::Chat(_) => "chat".to_owned(),
        Main::Settings => "settings".to_owned(),
        other => format!("{other:?}"),
    };
    json!({
        "main": main,
        "providers": shown,
        "editing": editing,
        "tested": tested,
        "problem": problem,
        "choices": choices,
        "vault": vault,
        "listed": listed,
        "box": compose,
        "chat_translation": translation,
        "known": Providers::of(cx).len(),
    })
}
