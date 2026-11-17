//! The AI APIs that the app asks with a key ([`super::providers`]): Anthropic's Messages API, and OpenAI's Chat
//! Completions (also at the base URL of an OpenAI-compatible server). A [`Conversation`] keeps the questions and the
//! answers, as a command-line tool keeps its conversation, and sends them all with each question. The requests are
//! `teams-core`'s ([`teams_core::ai_api`]): this crate holds no HTTP client and never reads a key.
//!
//! - Each answer streams, and a request runs on a thread of its own: the wait that asks it looks at its deadline and
//!   its stop flag while the answer comes, as for a tool's process. When the wait ends without the answer (stopped,
//!   too slow), the request's own flag is set, and its thread drops the request at once (also while the server sends
//!   nothing), so the API stops the answer. No request stays open between two questions.
//! - **Failures** say what the user can do: the key refused, no credit, a rate limit, a model that the API does not
//!   know, or the API's own words with the key cut out of them. Nothing here logs what is asked or answered, or a key.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use teams_core::ai_api::{Api, ApiError, ApiKind, Question};

use super::catalog::Listed;
use super::providers::{AiKeys, ApiTarget, Provider};
use super::{Backend, Failure, Launch, Model, POLL, Work};

/// How long a listing of the models may take.
const LISTING: Duration = Duration::from_secs(20);

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
    api: Api,
    model: String,
    /// The effort the work asks for, sent only with the model that this app recommends.
    effort: Option<&'static str>,
    instructions: String,
    /// The questions and answers so far, `(role, text)`.
    history: Vec<(&'static str, String)>,
}

impl Conversation {
    /// The conversation `launch` asks for with the API of `api`: its key read and its model found.
    ///
    /// # Errors
    /// No key, no model, or no HTTP client.
    pub fn start(launch: &Launch, api: &ApiTarget) -> Result<Self, Failure> {
        let backend = launch.backend;
        let key = api.key(backend)?;
        let model = model_for(backend, launch.work, &launch.model, api.model.as_deref())?;
        let recommended = backend.recommended(launch.work) == Some(model.as_str());
        let effort = (recommended && backend != Backend::Compatible).then(|| backend.effort(launch.work));
        let api = Api::new(kind(backend), &api.base_url, key).map_err(|error| failure(backend, &model, error))?;
        Ok(Self {
            backend,
            api,
            model,
            effort,
            instructions: launch.instructions.clone(),
            history: Vec::new(),
        })
    }

    /// Asks `message` in the conversation, telling `partial` the answer so far; by `deadline`, until `stop`.
    ///
    /// # Errors
    /// The API refused, failed, gave no text, took too long, or `stop` was set.
    pub fn ask(
        &mut self,
        message: &str,
        deadline: Instant,
        stop: &AtomicBool,
        partial: &mut dyn FnMut(&str),
    ) -> Result<String, Failure> {
        if stop.load(Ordering::Relaxed) {
            return Err(Failure::Stopped(self.backend, None));
        }
        self.history.push(("user", message.to_owned()));
        let dropped = Arc::new(AtomicBool::new(false));
        let asked = self
            .send(deadline, &dropped)
            .and_then(|events| self.wait(&events, deadline, stop, partial));
        // the request's thread, if it still reads, drops the response
        dropped.store(true, Ordering::Relaxed);
        match &asked {
            Ok(text) => self.history.push(("assistant", text.clone())),
            Err(_) => {
                self.history.pop();
            }
        }
        asked
    }

    /// Starts the request on a thread of its own, which drops it once `dropped` is set: its events.
    fn send(&self, deadline: Instant, dropped: &Arc<AtomicBool>) -> Result<Receiver<Event>, Failure> {
        let backend = self.backend;
        let left = deadline.saturating_duration_since(Instant::now());
        let question = Question {
            model: self.model.clone(),
            instructions: self.instructions.clone(),
            turns: self.history.clone(),
            effort: self.effort,
        };
        let api = self.api.clone();
        let dropped = Arc::clone(dropped);
        let (sender, events) = mpsc::channel();
        thread::Builder::new()
            .name("ngteams-ai-api".to_owned())
            .spawn(move || {
                let answer = api.ask(&question, left, &dropped, &mut |part| {
                    let _ = sender.send(Event::Delta(part.to_owned()));
                });
                let done = answer.map_err(|error| failure(backend, &question.model, error));
                let _ = sender.send(Event::Done(done));
            })
            .map_err(|error| Failure::Stopped(backend, Some(error.to_string())))?;
        Ok(events)
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

/// The protocol of an API kind: OpenAI's for an OpenAI-compatible server too.
fn kind(backend: Backend) -> ApiKind {
    if backend == Backend::Anthropic {
        ApiKind::Anthropic
    } else {
        ApiKind::OpenAi
    }
}

/// The failure that the user sees for an API's `error` while it was asked `model`.
fn failure(backend: Backend, model: &str, error: ApiError) -> Failure {
    match error {
        ApiError::TimedOut => Failure::TimedOut(backend),
        ApiError::BadKey => Failure::SignedOut(backend),
        ApiError::NoCredit => Failure::NoCredit(backend),
        ApiError::RateLimited => Failure::RateLimited(backend),
        ApiError::UnknownModel => Failure::UnknownModel(backend, model.to_owned()),
        ApiError::Stopped(why) => Failure::Stopped(backend, Some(why)),
        ApiError::Cancelled => Failure::Stopped(backend, None),
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
    let api = Api::new(kind(backend), &target.base_url, key).map_err(|error| failure(backend, "", error))?;
    let models = api.models(LISTING).map_err(|error| failure(backend, "", error))?;
    Ok(models
        .into_iter()
        .map(|model| Listed {
            id: model.id,
            name: model.name,
            latest: false,
        })
        .collect())
}

#[cfg(test)]
#[path = "../../tests/support/local_ai_api_units.rs"]
mod tests;
