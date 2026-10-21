//! The view trace: one JSON line per view change, for the live test to read what each view shows without reading the
//! screen. A line holds the view, the change, the conversation and message ids it concerns, kinds and the time:
//! `{"at_ms":1767225600000,"view":"window","change":"opened"}`.
//!
//! It never holds text: the view and the change are names fixed in the code (`&'static str`), kinds too, and an id
//! goes in only as a [`TraceId`], which takes a service id's shape only (a message or row id's digits, or a
//! conversation or account id with its `:`), so a message's text or a person's name can't pass for one. Whatever
//! isn't such an id is left out and counted (`"refused"`).
//!
//! The caller hands the writer in (the app: a descriptor it was given, `--trace-fd`); a line that can't be written is
//! lost, and the view goes on.

use std::io::Write;
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

/// The longest id the trace takes.
const MAX_ID: usize = 512;

/// A service id the trace may hold (see the module docs): 1 to 512 visible ASCII characters, and either all digits
/// (a message's or an outbox row's id) or with a `:` in it (a conversation's or an account's id).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceId<'a>(&'a str);

impl<'a> TraceId<'a> {
    /// `id` if it has an id's shape.
    #[must_use]
    pub fn new(id: &'a str) -> Option<Self> {
        let visible = !id.is_empty() && id.len() <= MAX_ID && id.bytes().all(|byte| byte.is_ascii_graphic());
        let shaped = id.bytes().all(|byte| byte.is_ascii_digit()) || id.contains(':');
        (visible && shaped).then_some(Self(id))
    }

    #[must_use]
    pub fn as_str(&self) -> &'a str {
        self.0
    }
}

/// One line of the trace, built up and then [recorded](ViewTrace::record).
#[derive(Clone, Debug)]
pub struct TraceLine<'a> {
    view: &'static str,
    change: &'static str,
    conversation: Option<TraceId<'a>>,
    messages: Vec<TraceId<'a>>,
    kinds: Vec<&'static str>,
    refused: usize,
}

impl<'a> TraceLine<'a> {
    /// A change of `view` (`window`, `status`, …): `change` names what happened (`opened`, `signed-out`, …).
    #[must_use]
    pub fn new(view: &'static str, change: &'static str) -> Self {
        Self {
            view,
            change,
            conversation: None,
            messages: Vec::new(),
            kinds: Vec::new(),
            refused: 0,
        }
    }

    /// The conversation it concerns, if `id` has an id's shape.
    #[must_use]
    pub fn conversation(mut self, id: &'a str) -> Self {
        match TraceId::new(id) {
            Some(id) => self.conversation = Some(id),
            None => self.refused += 1,
        }
        self
    }

    /// The messages it concerns, those with an id's shape.
    #[must_use]
    pub fn messages<I: IntoIterator<Item = &'a str>>(mut self, ids: I) -> Self {
        for id in ids {
            match TraceId::new(id) {
                Some(id) => self.messages.push(id),
                None => self.refused += 1,
            }
        }
        self
    }

    /// A kind (a message's, a row's state, …), named in the code.
    #[must_use]
    pub fn kind(mut self, kind: &'static str) -> Self {
        self.kinds.push(kind);
        self
    }

    fn to_json(&self, at_ms: u128) -> Value {
        let mut line = Map::new();
        line.insert("at_ms".to_owned(), json!(at_ms));
        line.insert("view".to_owned(), json!(self.view));
        line.insert("change".to_owned(), json!(self.change));
        if let Some(conversation) = self.conversation {
            line.insert("conversation".to_owned(), json!(conversation.as_str()));
        }
        if !self.messages.is_empty() {
            let ids: Vec<&str> = self.messages.iter().map(TraceId::as_str).collect();
            line.insert("messages".to_owned(), json!(ids));
        }
        if !self.kinds.is_empty() {
            line.insert("kinds".to_owned(), json!(self.kinds));
        }
        if self.refused > 0 {
            line.insert("refused".to_owned(), json!(self.refused));
        }
        Value::Object(line)
    }
}

/// Where the trace goes (see the module docs). Shared by the views; each line is written whole, under a lock.
pub struct ViewTrace {
    out: Option<Mutex<Box<dyn Write + Send>>>,
}

impl std::fmt::Debug for ViewTrace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewTrace").field("on", &self.out.is_some()).finish()
    }
}

impl ViewTrace {
    /// A trace written to `out`.
    #[must_use]
    pub fn new(out: impl Write + Send + 'static) -> Self {
        Self {
            out: Some(Mutex::new(Box::new(out))),
        }
    }

    /// No trace: every line is dropped.
    #[must_use]
    pub fn off() -> Self {
        Self { out: None }
    }

    /// Writes `line`, stamped with the time now, and flushes it.
    pub fn record(&self, line: &TraceLine<'_>) {
        let Some(out) = &self.out else { return };
        let at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        let mut text = line.to_json(at_ms).to_string();
        text.push('\n');
        let mut out = out.lock().unwrap_or_else(PoisonError::into_inner);
        // a line that can't be written is lost; the view goes on
        let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
    }
}
