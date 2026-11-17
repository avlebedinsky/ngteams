//! The AI APIs that the app asks with a key ([`super::providers`]): Anthropic's Messages API, and OpenAI's Chat
//! Completions (also at the base URL of an OpenAI-compatible server). A [`Conversation`] keeps the questions and the
//! answers, as a command-line tool keeps its conversation, and sends them all with each question.
//!
//! - Each answer streams (server-sent events), and a request runs on a thread of its own: the wait that asks it
//!   looks at its deadline and its stop flag while the answer comes, as for a tool's process.
//! - No redirect is followed, so the key goes only to the base URL that the user gave.
//! - **Failures** say what the user can do: the key refused, no credit, a rate limit, a model that the API does not
//!   know, or the API's own words with the key cut out of them. Nothing here logs what is asked or answered, or a key.

use std::io::{BufRead as _, BufReader, Read as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;

use super::catalog::Listed;
use super::providers::{AiKeys, ApiTarget, Provider};
use super::{Backend, Failure, Launch, Model, POLL, Work, cap};

/// The version of the Anthropic API that the requests take.
const ANTHROPIC_VERSION: &str = "2026-10-21";
/// The most tokens an answer may have.
const MAX_TOKENS: u32 = 16_000;
/// How long a connection may take to open.
const CONNECT: Duration = Duration::from_secs(15);
/// How long a listing of the models may take.
const LISTING: Duration = Duration::from_secs(20);
/// The most of an error's body read, in bytes.
const MAX_ERROR: u64 = 64 << 10;

/// One request's event, from its thread.
enum Event {
    /// More text of the answer.
    Delta(String),
    /// The whole answer, or why there is none.
    Done(Result<String, Failure>),
}

/// A conversation with an API (see the module docs).
pub struct Conversation {
    backend: Backend,
    base_url: String,
    key: Option<Secret>,
    model: String,
    /// The effort the work asks for, sent only with the model that this app recommends.
    effort: Option<&'static str>,
    instructions: String,
    /// The questions and answers so far, `(role, text)`.
    history: Vec<(&'static str, String)>,
    client: reqwest::blocking::Client,
}

impl Conversation {
    /// The conversation `launch` asks for with the API of `api`: its key read and its model found.
    ///
    /// # Errors
    /// No key, no model, or no client.
    pub fn start(launch: &Launch, api: &ApiTarget) -> Result<Self, Failure> {
        let backend = launch.backend;
        let key = api.key(backend)?;
        let model = model_for(backend, launch.work, &launch.model, api.model.as_deref())?;
        let recommended = backend.recommended(launch.work) == Some(model.as_str());
        let effort = (recommended && backend != Backend::Compatible).then(|| backend.effort(launch.work));
        Ok(Self {
            backend,
            base_url: api.base_url.trim_end_matches('/').to_owned(),
            key,
            model,
            effort,
            instructions: launch.instructions.clone(),
            history: Vec::new(),
            client: client(backend)?,
        })
    }

    /// Asks `message` in the conversation, telling `partial` the answer so far; by `deadline`, until `stop`.
    ///
    /// # Errors
    /// The API refused, failed, took too long, or `stop` was set.
    pub fn ask(
        &mut self,
        message: &str,
        deadline: Instant,
        stop: &AtomicBool,
        partial: &mut dyn FnMut(&str),
    ) -> Result<String, Failure> {
        self.history.push(("user", message.to_owned()));
        let asked = self
            .send(deadline)
            .and_then(|events| self.wait(&events, deadline, stop, partial));
        match &asked {
            Ok(text) => self.history.push(("assistant", text.clone())),
            Err(_) => {
                self.history.pop();
            }
        }
        asked
    }

    /// Starts the request on a thread of its own: its events.
    fn send(&self, deadline: Instant) -> Result<Receiver<Event>, Failure> {
        let backend = self.backend;
        let left = deadline.saturating_duration_since(Instant::now());
        let request = self.request().timeout(left.max(Duration::from_millis(1)));
        let (sender, events) = mpsc::channel();
        let key = self.key.clone();
        let model = self.model.clone();
        thread::Builder::new()
            .name("ngteams-ai-api".to_owned())
            .spawn(move || stream(backend, request, key.as_ref(), &model, &sender))
            .map_err(|error| Failure::Stopped(backend, Some(error.to_string())))?;
        Ok(events)
    }

    /// The request of the conversation so far.
    fn request(&self) -> reqwest::blocking::RequestBuilder {
        let messages: Vec<Value> = self
            .history
            .iter()
            .map(|(role, text)| json!({"role": role, "content": text}))
            .collect();
        if self.backend == Backend::Anthropic {
            let mut body = json!({
                "model": self.model,
                "max_tokens": MAX_TOKENS,
                "system": self.instructions,
                "messages": messages,
                "stream": true,
            });
            if let (Some(effort), Some(body)) = (self.effort, body.as_object_mut()) {
                body.insert("output_config".to_owned(), json!({"effort": effort}));
            }
            let request = self
                .client
                .post(format!("{}/v1/messages", self.base_url))
                .header("anthropic-version", ANTHROPIC_VERSION)
                .json_body(&body);
            with_key(request, self.backend, self.key.as_ref())
        } else {
            let mut all = vec![json!({"role": "system", "content": self.instructions})];
            all.extend(messages);
            let mut body = json!({"model": self.model, "messages": all, "stream": true});
            if let (Some(effort), Some(body)) = (self.effort, body.as_object_mut()) {
                body.insert("reasoning_effort".to_owned(), json!(effort));
            }
            let request = self
                .client
                .post(format!("{}/chat/completions", self.base_url))
                .json_body(&body);
            with_key(request, self.backend, self.key.as_ref())
        }
    }

    /// The answer the request's `events` bring, telling `partial` its text so far.
    fn wait(
        &self,
        events: &Receiver<Event>,
        deadline: Instant,
        stop: &AtomicBool,
        partial: &mut dyn FnMut(&str),
    ) -> Result<String, Failure> {
        let mut so_far = String::new();
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err(Failure::Stopped(self.backend, None));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Failure::TimedOut(self.backend));
            }
            match events.recv_timeout(left.min(POLL)) {
                Ok(Event::Delta(text)) => {
                    so_far.push_str(&text);
                    partial(&so_far);
                }
                Ok(Event::Done(done)) => return done,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(Failure::Stopped(self.backend, None)),
            }
        }
    }
}

/// The model to ask: the one chosen, else the provider's default, else the one recommended for the work.
fn model_for(backend: Backend, work: Work, model: &Model, own: Option<&str>) -> Result<String, Failure> {
    let named = match model {
        Model::Named(name) => Some(name.clone()),
        Model::Recommended => backend
            .recommended(work)
            .map(str::to_owned)
            .or_else(|| own.map(str::to_owned)),
        Model::ToolDefault => own
            .map(str::to_owned)
            .or_else(|| backend.recommended(work).map(str::to_owned)),
    };
    named.filter(|name| !name.is_empty()).ok_or(Failure::NoModel(backend))
}

/// A client that follows no redirect.
fn client(backend: Backend) -> Result<reqwest::blocking::Client, Failure> {
    reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT)
        .user_agent(concat!("ngteams/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| Failure::Stopped(backend, Some(error.to_string())))
}

trait JsonBody {
    fn json_body(self, body: &Value) -> Self;
}

impl JsonBody for reqwest::blocking::RequestBuilder {
    fn json_body(self, body: &Value) -> Self {
        self.header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
    }
}

/// `request` with the API's key in the header that its kind takes.
fn with_key(
    request: reqwest::blocking::RequestBuilder,
    backend: Backend,
    key: Option<&Secret>,
) -> reqwest::blocking::RequestBuilder {
    let Some(key) = key else { return request };
    let (name, value) = if backend == Backend::Anthropic {
        ("x-api-key", key.expose().to_owned())
    } else {
        ("authorization", format!("Bearer {}", key.expose()))
    };
    match reqwest::header::HeaderValue::from_str(&value) {
        Ok(mut value) => {
            value.set_sensitive(true);
            request.header(name, value)
        }
        Err(_) => request,
    }
}

/// Sends `request` and hands its answer to `sender` as it streams.
fn stream(
    backend: Backend,
    request: reqwest::blocking::RequestBuilder,
    key: Option<&Secret>,
    model: &str,
    sender: &Sender<Event>,
) {
    let done = match request.send() {
        Err(error) => Err(unreachable(backend, &error)),
        Ok(response) if !response.status().is_success() => Err(refused(backend, response, key, model)),
        Ok(response) => read_stream(backend, response, key, model, sender),
    };
    let _ = sender.send(Event::Done(done));
}

/// The answer in the server-sent events of `response`.
fn read_stream(
    backend: Backend,
    response: reqwest::blocking::Response,
    key: Option<&Secret>,
    model: &str,
    sender: &Sender<Event>,
) -> Result<String, Failure> {
    let mut text = String::new();
    let mut ended = false;
    for line in BufReader::new(response).lines() {
        let line = line.map_err(|error| Failure::Stopped(backend, Some(scrub(&error.to_string(), key))))?;
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data == "[DONE]" {
            ended = true;
            break;
        }
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if let Some(error) = event.get("error") {
            return Err(classify(backend, None, error, key, model));
        }
        let delta = if backend == Backend::Anthropic {
            match event.get("type").and_then(Value::as_str) {
                Some("content_block_delta") if event.pointer("/delta/type") == Some(&json!("text_delta")) => {
                    event.pointer("/delta/text").and_then(Value::as_str)
                }
                Some("message_delta") if event.pointer("/delta/stop_reason") == Some(&json!("refusal")) => {
                    return Err(Failure::Stopped(backend, Some("it declined to answer.".to_owned())));
                }
                Some("message_stop") => {
                    ended = true;
                    break;
                }
                _ => None,
            }
        } else {
            event.pointer("/choices/0/delta/content").and_then(Value::as_str)
        };
        if let Some(delta) = delta.filter(|delta| !delta.is_empty()) {
            text.push_str(delta);
            let _ = sender.send(Event::Delta(delta.to_owned()));
        }
    }
    if !ended && text.is_empty() {
        return Err(Failure::Stopped(backend, Some("it gave no answer.".to_owned())));
    }
    Ok(text)
}

/// Why a request that could not be sent failed.
fn unreachable(backend: Backend, error: &reqwest::Error) -> Failure {
    if error.is_timeout() {
        return Failure::TimedOut(backend);
    }
    let why = if error.is_connect() {
        "its server could not be reached. Check the base URL and the connection.".to_owned()
    } else {
        format!("its server could not be reached ({error}).")
    };
    Failure::Stopped(backend, Some(why))
}

/// Why the API refused the request: from its status and its error's body.
fn refused(backend: Backend, response: reqwest::blocking::Response, key: Option<&Secret>, model: &str) -> Failure {
    let status = response.status().as_u16();
    let mut body = String::new();
    let _ = response.take(MAX_ERROR).read_to_string(&mut body);
    let error = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| value.get("error").cloned())
        .unwrap_or(Value::Null);
    classify(backend, Some(status), &error, key, model)
}

/// The failure an API's `error` (its body's `error`) and `status` tell.
fn classify(backend: Backend, status: Option<u16>, error: &Value, key: Option<&Secret>, model: &str) -> Failure {
    let field = |name: &str| error.get(name).and_then(Value::as_str).unwrap_or_default().to_owned();
    let (kind, code) = (field("type"), field("code"));
    let message = match error {
        Value::String(text) => text.clone(),
        _ => field("message"),
    };
    let lower = message.to_lowercase();
    let credit = kind == "billing_error"
        || code == "insufficient_quota"
        || kind == "insufficient_quota"
        || lower.contains("credit balance");
    if credit || status == Some(402) {
        return Failure::NoCredit(backend);
    }
    if status == Some(401) || kind == "authentication_error" || code == "invalid_api_key" || lower.contains("api key") {
        return Failure::SignedOut(backend);
    }
    if status == Some(429) || kind == "rate_limit_error" || code == "rate_limit_exceeded" {
        return Failure::RateLimited(backend);
    }
    if code == "model_not_found" || (status == Some(404) || kind == "not_found_error") && lower.contains("model") {
        return Failure::UnknownModel(backend, model.to_owned());
    }
    let said = scrub(message.lines().next().unwrap_or_default().trim(), key);
    let why = if said.is_empty() {
        status.map_or_else(
            || "its server failed.".to_owned(),
            |status| format!("its server answered HTTP {status}."),
        )
    } else {
        let mut why = cap(&said, 160);
        if why.len() < said.len() {
            why.push('…');
        }
        why
    };
    Failure::Stopped(backend, Some(why))
}

/// `text` without `key` in it (an API can repeat a part of the key that it refused).
fn scrub(text: &str, key: Option<&Secret>) -> String {
    match key.map(Secret::expose).filter(|key| key.len() >= 4) {
        Some(key) => text.replace(key, "…"),
        None => text.to_owned(),
    }
}

/// The models `provider`'s API lists (`/v1/models`), in the order it gives them (this blocks: a request runs).
///
/// # Errors
/// No key, the API refused, or it could not be reached.
pub fn list_models(provider: &Provider, keys: &AiKeys) -> Result<Vec<Listed>, Failure> {
    let backend = provider.kind;
    let target = ApiTarget {
        provider: provider.id.clone(),
        base_url: provider.base_url(),
        model: provider.model.clone(),
        has_key: provider.has_key,
        keys: keys.clone(),
    };
    let key = target.key(backend)?;
    let base = target.base_url.trim_end_matches('/');
    let url = if backend == Backend::Anthropic {
        format!("{base}/v1/models?limit=1000")
    } else {
        format!("{base}/models")
    };
    let mut request = client(backend)?.get(url).timeout(LISTING);
    if backend == Backend::Anthropic {
        request = request.header("anthropic-version", ANTHROPIC_VERSION);
    }
    let response = with_key(request, backend, key.as_ref())
        .send()
        .map_err(|error| unreachable(backend, &error))?;
    if !response.status().is_success() {
        return Err(refused(backend, response, key.as_ref(), ""));
    }
    let mut body = String::new();
    response
        .take(super::catalog::MAX_LISTING)
        .read_to_string(&mut body)
        .map_err(|error| Failure::Stopped(backend, Some(error.to_string())))?;
    Ok(models_in(&body))
}

/// The models in a `/v1/models` answer's `body`: each by its id, with its display name when the API gives one.
#[must_use]
pub fn models_in(body: &str) -> Vec<Listed> {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let id = model.get("id").and_then(Value::as_str)?.to_owned();
            let name = model
                .get("display_name")
                .and_then(Value::as_str)
                .map_or_else(|| id.clone(), str::to_owned);
            Some(Listed {
                id,
                name,
                latest: false,
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "../../tests/support/local_ai_api_units.rs"]
mod tests;
