//! The AI APIs that the app asks with a key that the user gives: Anthropic's Messages API, and OpenAI's Chat
//! Completions (also at the base URL of an OpenAI-compatible server). The app crates hold no HTTP client and never
//! read a key out of its `Secret`, so these requests are made here; `teams-ui` keeps the conversation and its words.
//!
//! - [`Api::ask`] blocks while the answer streams (server-sent events), and gives each part of the text as it comes.
//! - No redirect is followed, so the key goes only to the base URL that the user gave. This is not the Teams
//!   transport: no Teams token goes here, and a local OpenAI-compatible server can be on plain `http`.
//! - An [`ApiError`] tells what the user can do: the key refused, no credit, a rate limit, a model that the API does
//!   not know, or the API's own words with the key cut out of them. Nothing here logs what is asked or answered, or
//!   a key.

use std::fmt;
use std::io::{BufRead as _, BufReader, Read as _};
use std::time::Duration;

use serde_json::{Value, json};
use teams_auth::secrets::Secret;

/// The version of the Anthropic API that the requests take.
pub const ANTHROPIC_VERSION: &str = "2026-10-21";
/// The most tokens an answer may have.
const MAX_TOKENS: u32 = 16_000;
/// How long a connection may take to open.
const CONNECT: Duration = Duration::from_secs(15);
/// The most of an error's body read, in bytes.
const MAX_ERROR: u64 = 64 << 10;
/// The most of a listing of the models read, in bytes.
const MAX_LISTING: u64 = 4 << 20;
/// The most of the API's own words that a failure keeps, in bytes.
const MAX_SAID: usize = 160;

/// The protocol of an API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiKind {
    /// Anthropic's Messages API (`/v1/messages`, the key in `x-api-key`).
    Anthropic,
    /// OpenAI's Chat Completions (`/chat/completions`, a bearer key), also at an OpenAI-compatible server.
    OpenAi,
}

/// Why an API gave no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiError {
    /// The time given ran out.
    TimedOut,
    /// The API refused the key.
    BadKey,
    /// The account of the key has no credit or quota left.
    NoCredit,
    /// The API limits the rate of the requests.
    RateLimited,
    /// The API does not know the model.
    UnknownModel,
    /// Another failure, in words for the user (never with the key in them).
    Stopped(String),
}

/// A model that an API lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiModel {
    /// The id that a request names.
    pub id: String,
    /// The name to show: the API's display name, else the id.
    pub name: String,
}

/// One question to an API, with the conversation before it.
#[derive(Clone, Debug)]
pub struct Question {
    /// The model to ask.
    pub model: String,
    /// The instructions (the system prompt).
    pub instructions: String,
    /// The conversation, `(role, text)` with the role `user` or `assistant`; the last is the question.
    pub turns: Vec<(&'static str, String)>,
    /// The effort to ask for (Anthropic's `output_config.effort`, OpenAI's `reasoning_effort`), if any.
    pub effort: Option<&'static str>,
}

/// An API at its base URL, with its key (see the module docs).
#[derive(Clone)]
pub struct Api {
    kind: ApiKind,
    base_url: String,
    key: Option<Secret>,
    http: reqwest::blocking::Client,
}

impl fmt::Debug for Api {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Api")
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .field("key", &self.key.as_ref().map(|_| "set"))
            .finish_non_exhaustive()
    }
}

impl Api {
    /// The API of `kind` at `base_url`, asked with `key` (none for a local server that takes no key).
    ///
    /// # Errors
    /// The HTTP client could not be made.
    pub fn new(kind: ApiKind, base_url: &str, key: Option<Secret>) -> Result<Self, ApiError> {
        let http = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT)
            .user_agent(concat!("ngteams/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| ApiError::Stopped(error.to_string()))?;
        Ok(Self {
            kind,
            base_url: base_url.trim_end_matches('/').to_owned(),
            key,
            http,
        })
    }

    /// Asks `question` within `timeout`, giving `part` each part of the answer as it comes (this blocks).
    ///
    /// # Errors
    /// The API refused, failed, or took too long.
    pub fn ask(&self, question: &Question, timeout: Duration, part: &mut dyn FnMut(&str)) -> Result<String, ApiError> {
        let response = self
            .question(question)
            .timeout(timeout.max(Duration::from_millis(1)))
            .send()
            .map_err(|error| unreachable(&error))?;
        if !response.status().is_success() {
            return Err(self.refused(response));
        }
        self.read_stream(response, part)
    }

    /// The models that the API lists (`/v1/models`), in its order, within `timeout` (this blocks).
    ///
    /// # Errors
    /// The API refused, failed, or took too long.
    pub fn models(&self, timeout: Duration) -> Result<Vec<ApiModel>, ApiError> {
        let mut request = if self.kind == ApiKind::Anthropic {
            self.http
                .get(format!("{}/v1/models?limit=1000", self.base_url))
                .header("anthropic-version", ANTHROPIC_VERSION)
        } else {
            self.http.get(format!("{}/models", self.base_url))
        };
        request = self.with_key(request.timeout(timeout));
        let response = request.send().map_err(|error| unreachable(&error))?;
        if !response.status().is_success() {
            return Err(self.refused(response));
        }
        let mut body = String::new();
        response
            .take(MAX_LISTING)
            .read_to_string(&mut body)
            .map_err(|error| ApiError::Stopped(self.scrub(&error.to_string())))?;
        Ok(models_in(&body))
    }

    /// The request that asks `question`.
    fn question(&self, question: &Question) -> reqwest::blocking::RequestBuilder {
        let messages: Vec<Value> = question
            .turns
            .iter()
            .map(|(role, text)| json!({"role": role, "content": text}))
            .collect();
        let (url, mut body) = if self.kind == ApiKind::Anthropic {
            let body = json!({
                "model": question.model,
                "max_tokens": MAX_TOKENS,
                "system": question.instructions,
                "messages": messages,
                "stream": true,
            });
            (format!("{}/v1/messages", self.base_url), body)
        } else {
            let mut all = vec![json!({"role": "system", "content": question.instructions})];
            all.extend(messages);
            let body = json!({"model": question.model, "messages": all, "stream": true});
            (format!("{}/chat/completions", self.base_url), body)
        };
        if let (Some(effort), Some(fields)) = (question.effort, body.as_object_mut()) {
            if self.kind == ApiKind::Anthropic {
                fields.insert("output_config".to_owned(), json!({"effort": effort}));
            } else {
                fields.insert("reasoning_effort".to_owned(), json!(effort));
            }
        }
        let mut request = self
            .http
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string());
        if self.kind == ApiKind::Anthropic {
            request = request.header("anthropic-version", ANTHROPIC_VERSION);
        }
        self.with_key(request)
    }

    /// `request` with the key in the header that the kind takes.
    fn with_key(&self, request: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
        let Some(key) = &self.key else { return request };
        let (name, value) = if self.kind == ApiKind::Anthropic {
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

    /// The answer in the server-sent events of `response`, given to `part` as it comes.
    fn read_stream(
        &self,
        response: reqwest::blocking::Response,
        part: &mut dyn FnMut(&str),
    ) -> Result<String, ApiError> {
        let mut text = String::new();
        let mut ended = false;
        for line in BufReader::new(response).lines() {
            let line = line.map_err(|error| ApiError::Stopped(self.scrub(&error.to_string())))?;
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
                return Err(self.classify(None, error));
            }
            let delta = if self.kind == ApiKind::Anthropic {
                match event.get("type").and_then(Value::as_str) {
                    Some("content_block_delta") if event.pointer("/delta/type") == Some(&json!("text_delta")) => {
                        event.pointer("/delta/text").and_then(Value::as_str)
                    }
                    Some("message_delta") if event.pointer("/delta/stop_reason") == Some(&json!("refusal")) => {
                        return Err(ApiError::Stopped("it declined to answer.".to_owned()));
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
                part(delta);
            }
        }
        if !ended && text.is_empty() {
            return Err(ApiError::Stopped("it gave no answer.".to_owned()));
        }
        Ok(text)
    }

    /// Why the API refused the request: from its status and its error's body.
    fn refused(&self, response: reqwest::blocking::Response) -> ApiError {
        let status = response.status().as_u16();
        let mut body = String::new();
        let _ = response.take(MAX_ERROR).read_to_string(&mut body);
        let error = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|value| value.get("error").cloned())
            .unwrap_or(Value::Null);
        self.classify(Some(status), &error)
    }

    /// The failure that an API's `error` (its body's `error`) and `status` tell.
    fn classify(&self, status: Option<u16>, error: &Value) -> ApiError {
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
            return ApiError::NoCredit;
        }
        if status == Some(401)
            || kind == "authentication_error"
            || code == "invalid_api_key"
            || lower.contains("api key")
        {
            return ApiError::BadKey;
        }
        if status == Some(429) || kind == "rate_limit_error" || code == "rate_limit_exceeded" {
            return ApiError::RateLimited;
        }
        if code == "model_not_found" || (status == Some(404) || kind == "not_found_error") && lower.contains("model") {
            return ApiError::UnknownModel;
        }
        let said = self.scrub(message.lines().next().unwrap_or_default().trim());
        if said.is_empty() {
            return ApiError::Stopped(status.map_or_else(
                || "its server failed.".to_owned(),
                |status| format!("its server answered HTTP {status}."),
            ));
        }
        let mut end = said.len().min(MAX_SAID);
        while !said.is_char_boundary(end) {
            end -= 1;
        }
        let mut why = said[..end].to_owned();
        if end < said.len() {
            why.push('…');
        }
        ApiError::Stopped(why)
    }

    /// `text` without the key in it (an API can repeat a part of the key that it refused).
    fn scrub(&self, text: &str) -> String {
        match self.key.as_ref().map(Secret::expose).filter(|key| key.len() >= 4) {
            Some(key) => text.replace(key, "…"),
            None => text.to_owned(),
        }
    }
}

/// Why a request that could not be sent failed.
fn unreachable(error: &reqwest::Error) -> ApiError {
    if error.is_timeout() {
        return ApiError::TimedOut;
    }
    if error.is_connect() {
        return ApiError::Stopped("its server could not be reached. Check the base URL and the connection.".to_owned());
    }
    ApiError::Stopped(format!("its server could not be reached ({error})."))
}

/// The models in a `/v1/models` answer's `body`: each by its id, with its display name when the API gives one.
#[must_use]
pub fn models_in(body: &str) -> Vec<ApiModel> {
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
            Some(ApiModel { id, name })
        })
        .collect()
}
