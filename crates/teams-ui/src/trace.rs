//! The view trace: one JSON line per view change, for the live test to read what each view shows without reading the
//! screen. A line holds the view, the change, the conversation and message ids it concerns (a list's conversations in
//! its order, with their unread counts), the people it concerns (a call's caller and participants, by their ids), a
//! call's number, kinds and the time:
//! `{"at_ms":1767225600000,"view":"window","change":"opened"}`.
//!
//! It never holds text: the view and the change are names fixed in the code (`&'static str`), kinds too, and an id
//! goes in only as a [`TraceId`], which takes only the service id shapes the captures showed (a message or row id's
//! digits, a `19:…@thread.v2`-style conversation, an own `48:` stream, an `8:orgid:<guid>` or bot `28:<guid>`
//! account), so a message's text, a link or a person's name can't pass for one. Whatever isn't such an id is left
//! out and counted (`"refused"`).
//!
//! The caller hands the writer in (the app: a descriptor it was given, `--trace-fd`); a line that can't be written is
//! lost, and the view goes on.

use std::io::Write;
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

/// The longest id the trace takes.
const MAX_ID: usize = 512;

/// The most digits a numeric id has: a message's id (milliseconds), a `clientmessageid` (18 and 19 digits seen), an
/// outbox row's id (an `i64`).
const MAX_DIGITS: usize = 20;

/// What follows `19:<thread>@` in a conversation id (`docs/protocol/chat-service.md`, "Ids"): a 1:1 chat, a group or
/// meeting chat, a team's channel.
const THREAD_DOMAINS: [&str; 4] = ["unq.gbl.spaces", "thread.v2", "thread.tacv2", "thread.skype"];

/// The domains of a channel, whose reply chain is `<channel id>;messageid=<root id>`.
const CHANNEL_DOMAINS: [&str; 2] = ["thread.tacv2", "thread.skype"];

/// The account's own streams the captures showed (`48:<name>`).
const OWN_STREAMS: [&str; 4] = ["notes", "notifications", "mentions", "calllogs"];

/// A service id the trace may hold (see the module docs), in one of the shapes the captures showed
/// (`docs/protocol/chat-service.md`, "Ids"), at most 512 bytes:
///
/// - a number of 1 to 20 digits: a message's, a `clientmessageid`, an outbox row's;
/// - a conversation: `19:<thread>@<domain>`, the thread letters, digits, `_` and `-`, the domain one of
///   [`THREAD_DOMAINS`], or a channel's reply chain `19:<thread>@<channel domain>;messageid=<number>`; an own stream
///   `48:<name>` ([`OWN_STREAMS`]);
/// - an account: `8:orgid:<guid>`, a bot `28:<guid>`.
///
/// Anything else (a word, a time like `10:30`, `re:done`, a link, a name) is not an id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceId<'a>(&'a str);

impl<'a> TraceId<'a> {
    /// `id` if it has an id's shape.
    #[must_use]
    pub fn new(id: &'a str) -> Option<Self> {
        let shaped = is_number(id) || is_conversation(id) || is_account(id);
        (id.len() <= MAX_ID && shaped).then_some(Self(id))
    }

    #[must_use]
    pub fn as_str(&self) -> &'a str {
        self.0
    }
}

fn is_number(id: &str) -> bool {
    (1..=MAX_DIGITS).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_conversation(id: &str) -> bool {
    if let Some(stream) = id.strip_prefix("48:") {
        return OWN_STREAMS.contains(&stream);
    }
    let Some(rest) = id.strip_prefix("19:") else {
        return false;
    };
    let (thread, root) = match rest.split_once(";messageid=") {
        Some((thread, root)) => (thread, Some(root)),
        None => (rest, None),
    };
    let Some((local, domain)) = thread.split_once('@') else {
        return false;
    };
    let domain = match root {
        Some(root) => is_number(root) && CHANNEL_DOMAINS.contains(&domain),
        None => THREAD_DOMAINS.contains(&domain),
    };
    domain
        && !local.is_empty()
        && local
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn is_account(id: &str) -> bool {
    id.strip_prefix("8:orgid:")
        .or_else(|| id.strip_prefix("28:"))
        .is_some_and(is_guid)
}

/// `8-4-4-4-12` hex digits.
fn is_guid(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(at, byte)| match at {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

/// One line of the trace, built up and then [recorded](ViewTrace::record).
#[derive(Clone, Debug)]
pub struct TraceLine<'a> {
    view: &'static str,
    change: &'static str,
    conversation: Option<TraceId<'a>>,
    conversations: Vec<TraceId<'a>>,
    messages: Vec<TraceId<'a>>,
    kinds: Vec<&'static str>,
    unread: Vec<u64>,
    /// Of the messages, those that carry reactions, and those that quote another (a reply).
    reacted: Vec<TraceId<'a>>,
    replies: Vec<TraceId<'a>>,
    /// The people it concerns (a call's caller, its participants), those with an id's shape.
    people: Vec<TraceId<'a>>,
    /// The call it concerns: the engine's number for it.
    call: Option<u64>,
    /// A count of frames (a video tile's presented so far).
    frames: Option<u64>,
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
            conversations: Vec::new(),
            messages: Vec::new(),
            kinds: Vec::new(),
            unread: Vec::new(),
            reacted: Vec::new(),
            replies: Vec::new(),
            people: Vec::new(),
            call: None,
            frames: None,
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

    /// The conversations a list shows, in its order, those with an id's shape.
    #[must_use]
    pub fn conversations<I: IntoIterator<Item = &'a str>>(mut self, ids: I) -> Self {
        for id in ids {
            match TraceId::new(id) {
                Some(id) => self.conversations.push(id),
                None => self.refused += 1,
            }
        }
        self
    }

    /// A list's rows, in its order: each one's conversation, kind and unread count, kept together. A row whose id hasn't
    /// an id's shape is left out whole (and counted), so the conversations, kinds and unread counts stay in step.
    #[must_use]
    pub fn rows<I: IntoIterator<Item = (&'a str, &'static str, u64)>>(mut self, rows: I) -> Self {
        for (id, kind, unread) in rows {
            match TraceId::new(id) {
                Some(id) => {
                    self.conversations.push(id);
                    self.kinds.push(kind);
                    self.unread.push(unread);
                }
                None => self.refused += 1,
            }
        }
        self
    }

    /// Unread counts, in the order of the conversations: numbers, never text.
    #[must_use]
    pub fn unread<I: IntoIterator<Item = u64>>(mut self, counts: I) -> Self {
        self.unread.extend(counts);
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

    /// The messages that carry reactions, those with an id's shape.
    #[must_use]
    pub fn reacted<I: IntoIterator<Item = &'a str>>(mut self, ids: I) -> Self {
        for id in ids {
            match TraceId::new(id) {
                Some(id) => self.reacted.push(id),
                None => self.refused += 1,
            }
        }
        self
    }

    /// The messages that quote another (a reply), those with an id's shape.
    #[must_use]
    pub fn replies<I: IntoIterator<Item = &'a str>>(mut self, ids: I) -> Self {
        for id in ids {
            match TraceId::new(id) {
                Some(id) => self.replies.push(id),
                None => self.refused += 1,
            }
        }
        self
    }

    /// The people it concerns (a call's caller, its participants), those with an id's shape.
    #[must_use]
    pub fn people<I: IntoIterator<Item = &'a str>>(mut self, ids: I) -> Self {
        for id in ids {
            match TraceId::new(id) {
                Some(id) => self.people.push(id),
                None => self.refused += 1,
            }
        }
        self
    }

    /// The call it concerns, by the engine's number for it (a number, never text).
    #[must_use]
    pub fn call(mut self, call: u64) -> Self {
        self.call = Some(call);
        self
    }

    /// A count of frames (a video tile's presented so far: a number, never a picture).
    #[must_use]
    pub fn frames(mut self, frames: u64) -> Self {
        self.frames = Some(frames);
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
        if !self.conversations.is_empty() {
            let ids: Vec<&str> = self.conversations.iter().map(TraceId::as_str).collect();
            line.insert("conversations".to_owned(), json!(ids));
        }
        if !self.messages.is_empty() {
            let ids: Vec<&str> = self.messages.iter().map(TraceId::as_str).collect();
            line.insert("messages".to_owned(), json!(ids));
        }
        if !self.kinds.is_empty() {
            line.insert("kinds".to_owned(), json!(self.kinds));
        }
        if !self.unread.is_empty() {
            line.insert("unread".to_owned(), json!(self.unread));
        }
        if let Some(call) = self.call {
            line.insert("call".to_owned(), json!(call));
        }
        if let Some(frames) = self.frames {
            line.insert("frames".to_owned(), json!(frames));
        }
        for (key, ids) in [
            ("reacted", &self.reacted),
            ("replies", &self.replies),
            ("people", &self.people),
        ] {
            if !ids.is_empty() {
                let ids: Vec<&str> = ids.iter().map(TraceId::as_str).collect();
                line.insert(key.to_owned(), json!(ids));
            }
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
