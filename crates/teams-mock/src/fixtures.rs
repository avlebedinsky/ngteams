//! The synthetic chat fixtures the mock is built from, embedded at compile time, with their placeholders filled in
//! with the mock's made-up ids. Only `tests/fixtures/chatsvc/` is read: no capture, no reference copy.

use std::fmt;

use serde_json::{Map, Value};
use wiremock::ResponseTemplate;

use crate::{ABC_CHAT, LAB_A, LAB_B, LAB_C, REGION, TENANT};

/// Every fixture the mock reads, by its file name under `tests/fixtures/chatsvc/`.
pub(crate) const SOURCES: [(&str, &str); 9] = [
    (
        "conversations.json",
        include_str!("../../../tests/fixtures/chatsvc/conversations.json"),
    ),
    (
        "messages.json",
        include_str!("../../../tests/fixtures/chatsvc/messages.json"),
    ),
    ("edit.json", include_str!("../../../tests/fixtures/chatsvc/edit.json")),
    (
        "delete.json",
        include_str!("../../../tests/fixtures/chatsvc/delete.json"),
    ),
    (
        "reaction.json",
        include_str!("../../../tests/fixtures/chatsvc/reaction.json"),
    ),
    ("reply.json", include_str!("../../../tests/fixtures/chatsvc/reply.json")),
    (
        "thread-group.json",
        include_str!("../../../tests/fixtures/chatsvc/thread-group.json"),
    ),
    (
        "thread-group-members.json",
        include_str!("../../../tests/fixtures/chatsvc/thread-group-members.json"),
    ),
    (
        "thread-group-filtered.json",
        include_str!("../../../tests/fixtures/chatsvc/thread-group-filtered.json"),
    ),
];

/// An embedded fixture without the shape the mock reads from it.
#[derive(Debug)]
pub struct FixtureError(String);

impl fmt::Display for FixtureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "a chat fixture isn't as the mock expects: {}", self.0)
    }
}

impl std::error::Error for FixtureError {}

impl FixtureError {
    pub(crate) fn from_message(message: &str) -> Self {
        Self(message.to_owned())
    }
}

/// An answer as a fixture recorded it: its status and its JSON body or its bare text.
#[derive(Clone, Debug)]
pub(crate) struct Answer {
    status: u16,
    body: Body,
}

#[derive(Clone, Debug)]
enum Body {
    Json(Value),
    Text(String),
}

impl Answer {
    /// The fixture's `response`: `status` with `body` (JSON) or `body_text`.
    fn of(fixture: &Value, name: &str) -> Result<Self, FixtureError> {
        let response = &fixture["response"];
        let status = response["status"]
            .as_u64()
            .and_then(|status| u16::try_from(status).ok())
            .ok_or_else(|| FixtureError(format!("{name}: no response status")))?;
        let body = match (response.get("body"), response["body_text"].as_str()) {
            (Some(body), _) => Body::Json(body.clone()),
            (None, Some(text)) => Body::Text(text.to_owned()),
            (None, None) => return Err(FixtureError(format!("{name}: no response body"))),
        };
        Ok(Self { status, body })
    }

    pub(crate) fn json(&self) -> Option<&Value> {
        match &self.body {
            Body::Json(body) => Some(body),
            Body::Text(_) => None,
        }
    }

    pub(crate) fn template(&self) -> ResponseTemplate {
        let answer = ResponseTemplate::new(self.status);
        match &self.body {
            Body::Json(body) => answer.set_body_json(body),
            Body::Text(text) => answer.set_body_string(text.clone()),
        }
    }

    pub(crate) fn status(&self) -> u16 {
        self.status
    }
}

/// What the mock takes from the fixtures.
#[derive(Clone, Debug)]
pub(crate) struct Fixtures {
    /// The conversation list's entries (`conversations.json`): a 1:1 chat with messages, an empty 1:1 chat, a team
    /// channel, the notifications stream.
    pub(crate) entries: Vec<Value>,
    /// The a–b chat's messages (`messages.json`: the sync read's and the read around a time's), oldest first.
    pub(crate) ab_messages: Vec<Value>,
    /// A thread read (`thread-group.json`).
    pub(crate) thread: Value,
    /// A member list (`thread-group-members.json`).
    pub(crate) members: Value,
    /// The filtered view, refused (`thread-group-filtered.json`).
    pub(crate) filtered: Answer,
    /// A message post's answer (`reply.json`: 201 with `OriginalArrivalTime`).
    pub(crate) posted: Answer,
    /// An edit's answer (`edit.json`).
    pub(crate) edited: Answer,
    /// A soft delete's answer (`delete.json`).
    pub(crate) deleted: Answer,
    /// A reaction's answer and its taking back's (`reaction.json`).
    pub(crate) reacted: Answer,
    pub(crate) unreacted: Answer,
}

impl Fixtures {
    pub(crate) fn load() -> Result<Self, FixtureError> {
        let conversations = parsed("conversations.json")?;
        let messages = parsed("messages.json")?;
        let reaction = parsed("reaction.json")?;
        let entries = conversations["response"]["body"]["conversations"]
            .as_array()
            .filter(|entries| entries.len() == 4)
            .cloned()
            .ok_or_else(|| FixtureError("conversations.json: not four entries".to_owned()))?;
        let mut ab_messages = Vec::new();
        for read in ["sync", "around"] {
            let page = messages[read]["response"]["body"]["messages"]
                .as_array()
                .ok_or_else(|| FixtureError(format!("messages.json: no {read} page")))?;
            ab_messages.extend(page.iter().cloned());
        }
        ab_messages.sort_by_key(|message| message["sequenceId"].as_u64());
        Ok(Self {
            entries,
            ab_messages,
            thread: body("thread-group.json")?,
            members: body("thread-group-members.json")?,
            filtered: Answer::of(&parsed("thread-group-filtered.json")?, "thread-group-filtered.json")?,
            posted: Answer::of(&parsed("reply.json")?, "reply.json")?,
            edited: Answer::of(&parsed("edit.json")?, "edit.json")?,
            deleted: Answer::of(&parsed("delete.json")?, "delete.json")?,
            reacted: Answer::of(&reaction["add"], "reaction.json add")?,
            unreacted: Answer::of(&reaction["remove"], "reaction.json remove")?,
        })
    }
}

/// The text of the embedded fixture `name`, its placeholders filled in.
pub(crate) fn filled(name: &str) -> Result<String, FixtureError> {
    SOURCES
        .iter()
        .find(|(source, _)| *source == name)
        .map(|(_, text)| fill(text))
        .ok_or_else(|| FixtureError(format!("{name}: not embedded")))
}

/// The fixtures' placeholders, as the mock's made-up ids: the accounts' object ids, the tenant, the group chat's id,
/// a team's group, a context id, and sync states (which the mock never reads back: it hands out its own).
fn fill(text: &str) -> String {
    let oid = |mri: &str| mri.trim_start_matches("8:orgid:").to_owned();
    let group = ABC_CHAT.trim_start_matches("19:").trim_end_matches("@thread.v2");
    [
        ("<region>", REGION.to_owned()),
        ("<guid:a>", oid(LAB_A)),
        ("<guid:b>", oid(LAB_B)),
        ("<guid:c>", oid(LAB_C)),
        ("<guid:tenant>", TENANT.to_owned()),
        ("<guid:group>", "00000000-0000-4000-8000-000000000002".to_owned()),
        ("<guid:context>", "00000000-0000-4000-8000-000000000003".to_owned()),
        ("<group>", group.to_owned()),
        ("<token:1>", "mock-fixture-1".to_owned()),
        ("<token:2>", "mock-fixture-2".to_owned()),
    ]
    .iter()
    .fold(text.to_owned(), |text, (placeholder, value)| {
        text.replace(placeholder, value)
    })
}

/// The embedded fixture `name`, filled and parsed.
pub(crate) fn parsed(name: &str) -> Result<Value, FixtureError> {
    serde_json::from_str(&filled(name)?).map_err(|error| FixtureError(format!("{name}: {error}")))
}

/// The response body of the embedded fixture `name`, which must be an object.
fn body(name: &str) -> Result<Value, FixtureError> {
    let body = parsed(name)?["response"]["body"].clone();
    if body.is_object() {
        Ok(body)
    } else {
        Err(FixtureError(format!("{name}: no response body object")))
    }
}

/// `value` as an object, made one first if it isn't.
pub(crate) fn object(value: &mut Value) -> &mut Map<String, Value> {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    match value {
        Value::Object(map) => map,
        _ => unreachable!("made an object above"),
    }
}
