//! Probes the recorded group chat with plain GETs: thread, batch thread, members, filtered thread and own-conversation views. Reports show keys/counts and optional sanitized reference events.

use std::collections::BTreeSet;
use std::fmt::{self, Write as _};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::LabChats;
use crate::protocol::chat_service_path;
use serde_json::{Map, Value, json};
use teams_auth::claims::TokenSummary;
use teams_auth::secrets::SecretStore;
use teams_auth::token::{Scope, TokenClient};
use url::Url;

use super::probe::{
    Answer, HeaderForm, IC3_RESOURCE, Origin, ProbeError, ReadFailure, at_origin, body_value, chat_base, client,
    outcome_line, read, read_region, response_headers,
};
use crate::config::{Alias, LabConfig};

/// The page size of the one-page member read: more than a group chat of the lab can hold.
pub const ONE_PAGE: u32 = 100;

/// The page size of the paged member read: one member a page, so a 3-member chat shows the paging form.
pub const PAGED: u32 = 1;

/// The most pages of the paged member read followed.
pub const MAX_PAGES: usize = 5;

/// Which read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadReadKind {
    /// `v1/threads/<id>`: the browser's read.
    Thread,
    /// `v1/threads?threadIds=<id>`: the web client's batch read.
    Threads,
    /// `v1/threads/<id>/members?pageSize=`[`ONE_PAGE`].
    Members,
    /// `v1/threads/<id>/members?pageSize=`[`PAGED`], then each next page.
    MembersPaged,
    /// `v1/threads/<id>/filtered`.
    Filtered,
    /// `v1/users/ME/conversations/<id>`: the account's own view of the chat.
    MyConversation,
}

impl ThreadReadKind {
    /// Every read but the next pages, in the order made.
    pub const ALL: [Self; 6] = [
        Self::Thread,
        Self::Threads,
        Self::Members,
        Self::MembersPaged,
        Self::Filtered,
        Self::MyConversation,
    ];

    /// The bundle's name for it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Thread => "thread",
            Self::Threads => "threads",
            Self::Members => "thread_members",
            Self::MembersPaged => "thread_members (paged)",
            Self::Filtered => "filtered_thread",
            Self::MyConversation => "myconversation",
        }
    }

    /// The read's URL under `base` for the chat `id` (one encoded path segment).
    fn url(self, base: &str, id: &str) -> Option<Url> {
        let mut url = Url::parse(&format!("{base}/")).ok()?;
        let segments: Vec<&str> = match self {
            Self::Thread => vec!["v1", "threads", id],
            Self::Threads => vec!["v1", "threads"],
            Self::Members | Self::MembersPaged => vec!["v1", "threads", id, "members"],
            Self::Filtered => vec!["v1", "threads", id, "filtered"],
            Self::MyConversation => vec!["v1", "users", "ME", "conversations", id],
        };
        url.path_segments_mut().ok()?.pop_if_empty().extend(segments);
        let mut query = url.query_pairs_mut();
        query.append_pair("view", "msnp24Equivalent");
        match self {
            Self::Threads => {
                query.append_pair("threadIds", id);
            }
            Self::Members => {
                query.append_pair("pageSize", &ONE_PAGE.to_string());
            }
            Self::MembersPaged => {
                query.append_pair("pageSize", &PAGED.to_string());
            }
            _ => {}
        }
        drop(query);
        Some(url)
    }
}

/// One read: which, its page (1 for all but the paged member read's next pages), the URL as sent to the chat service
/// (never a mock's) and what came back. `Debug` leaves the URL out: it names the chat.
#[derive(Clone)]
pub struct ThreadRead {
    pub kind: ThreadReadKind,
    pub page: usize,
    url: Url,
    pub outcome: Result<Answer, ReadFailure>,
}

impl fmt::Debug for ThreadRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadRead")
            .field("kind", &self.kind)
            .field("page", &self.page)
            .field("outcome", &self.outcome)
            .finish_non_exhaustive()
    }
}

/// What a read's answer shows, as far as the report may show it: key names and counts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shape {
    /// The answer object's keys (for the batch read, its thread's), sorted.
    pub keys: Vec<String>,
    /// The keys of its `_metadata`, sorted.
    pub metadata_keys: Vec<String>,
    /// The keys of its `properties`, sorted.
    pub property_keys: Vec<String>,
    /// How many members it lists (`members`, an array), if it lists any.
    pub members: Option<usize>,
    /// The keys, top level or under `_metadata`, that look like paging (`next`, `continuation`, `skiptoken`,
    /// `syncstate`, `backward`).
    pub paging_keys: Vec<String>,
    member_ids: BTreeSet<String>,
    roster_version: Option<Value>,
}

impl Shape {
    /// The shape of an answer for the chat `id`: the object itself, or for an array (the batch read) the element
    /// whose `id` is `id`.
    #[must_use]
    pub fn of(body: &Value, id: &str) -> Self {
        let object = match body {
            Value::Object(object) => Some(object),
            Value::Array(items) => items
                .iter()
                .filter_map(Value::as_object)
                .find(|item| item.get("id").and_then(Value::as_str) == Some(id)),
            _ => None,
        };
        object.map(Self::of_object).unwrap_or_default()
    }

    fn of_object(object: &Map<String, Value>) -> Self {
        let keys_of = |value: Option<&Value>| -> Vec<String> {
            let mut keys: Vec<String> = value
                .and_then(Value::as_object)
                .map(|map| map.keys().cloned().collect())
                .unwrap_or_default();
            keys.sort();
            keys
        };
        let metadata_keys = keys_of(object.get("_metadata"));
        let mut keys: Vec<String> = object.keys().cloned().collect();
        keys.sort();
        let paging_keys = keys
            .iter()
            .chain(&metadata_keys)
            .filter(|key| is_paging_key(key))
            .cloned()
            .collect();
        let listed = object.get("members").and_then(Value::as_array);
        Self {
            property_keys: keys_of(object.get("properties")),
            members: listed.map(Vec::len),
            member_ids: listed
                .into_iter()
                .flatten()
                .filter_map(|member| member.get("id").and_then(Value::as_str))
                .map(str::to_ascii_lowercase)
                .collect(),
            roster_version: object.get("rosterVersion").cloned(),
            keys,
            metadata_keys,
            paging_keys,
        }
    }
}

fn is_paging_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    ["next", "continuation", "skiptoken", "syncstate", "backward"]
        .iter()
        .any(|marker| key.contains(marker))
}

/// What the probe found. Holds no token; the answers only for [`ThreadProbe::reference_events`]. `Debug` shows neither
/// the chat's id nor a member's.
pub struct ThreadProbe {
    pub alias: Alias,
    pub token: Option<TokenSummary>,
    pub base: String,
    chat_id: String,
    pub reads: Vec<ThreadRead>,
}

impl fmt::Debug for ThreadProbe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadProbe")
            .field("alias", &self.alias)
            .field("token", &self.token)
            .field("base", &self.base)
            .field("reads", &self.reads)
            .finish_non_exhaustive()
    }
}

/// Probes the a–b–c chat's member reads as `alias` (see the module docs), the browser's own read through `source`.
///
/// # Errors
/// [`ProbeError`] when nothing could be read: no a–b–c chat in `chats.toml` ([`ProbeError::NoGroupChat`]), no usable
/// chat base, a refused or failed refresh, a mock origin in a normal build.
pub async fn probe_thread<S: SecretStore>(
    store: &S,
    tokens: &TokenClient,
    config: &LabConfig,
    alias: Alias,
    origin: &Origin,
) -> Result<ThreadProbe, ProbeError> {
    if matches!(origin, Origin::Mock(_)) && !crate::test_support_enabled() {
        return Err(ProbeError::MockOrigin);
    }
    let chat_id = LabChats::load(config.paths())
        .map_err(|error| ProbeError::NoGroupChat(error.to_string()))?
        .abc
        .ok_or_else(|| ProbeError::NoGroupChat("chats.toml records none: run `proto-lab run abc-chat`".to_owned()))?;
    let base = chat_base(&read_region(config, alias)?)?;
    let account = &config.account(alias).upn;
    let ic3 = tokens
        .refresh(store, account, &Scope::for_resource(IC3_RESOURCE))
        .await?;
    let token = ic3.access_token.expose();
    let reader = Reader {
        client: client(origin)?,
        origin,
        token,
    };
    let mut reads = Vec::new();
    for kind in ThreadReadKind::ALL {
        let url = kind
            .url(&base, &chat_id)
            .ok_or_else(|| ProbeError::NotAChatBase(base.clone()))?;
        let pages = if kind == ThreadReadKind::MembersPaged {
            MAX_PAGES
        } else {
            1
        };
        reads.extend(reader.pages(kind, url, pages).await);
    }
    Ok(ThreadProbe {
        alias,
        token: TokenSummary::read(&ic3.access_token),
        base,
        chat_id,
        reads,
    })
}

/// How the reads go out: the client, where to, with which token.
struct Reader<'a> {
    client: reqwest::Client,
    origin: &'a Origin,
    token: &'a str,
}

impl Reader<'_> {
    /// Reads `first`, then while the answer names a next page of the same read ([`next_page`]) and fewer than `pages`
    /// were read, that page.
    async fn pages(&self, kind: ThreadReadKind, first: Url, pages: usize) -> Vec<ThreadRead> {
        let mut reads = Vec::new();
        let mut url = Some(first);
        for page in 1..=pages {
            let Some(this) = url.take() else {
                break;
            };
            let Some(sent) = at_origin(this.as_str(), self.origin) else {
                break;
            };
            let outcome = read(&self.client, sent, self.token, HeaderForm::ALL[0]).await;
            url = outcome
                .as_ref()
                .ok()
                .and_then(|answer| serde_json::from_slice::<Value>(answer.body()).ok())
                .and_then(|body| next_page(&body, &this));
            reads.push(ThreadRead {
                kind,
                page,
                url: this,
                outcome,
            });
        }
        reads
    }
}

/// The next page an answer to `current` names, re-based on `current`'s origin: a string under a paging key
/// ([`Shape::paging_keys`] holding `next`) at the top level or under `_metadata`, an absolute or relative URL whose
/// chat service path is `current`'s (the same member list of the same chat, on any chat service host); only its query
/// is kept, on `current`'s origin. `None` for anything else, which
/// isn't followed.
#[must_use]
pub fn next_page(body: &Value, current: &Url) -> Option<Url> {
    let object = body.as_object()?;
    let metadata = object.get("_metadata").and_then(Value::as_object);
    let link = object
        .iter()
        .chain(metadata.into_iter().flatten())
        .filter(|(key, _)| key.to_ascii_lowercase().contains("next"))
        .find_map(|(_, value)| value.as_str())?;
    let next = current.join(link).ok()?;
    let path = chat_service_path(current)?;
    if chat_service_path(&next)? != path {
        return None;
    }
    let mut rebased = current.clone();
    rebased.set_query(next.query());
    Some(rebased)
}

impl ThreadProbe {
    /// The report as lines for the operator, as of `now` (see the module docs for what they show and never show).
    #[must_use]
    pub fn lines(&self, now: SystemTime) -> Vec<String> {
        let alias = self.alias;
        let mut lines = vec![
            format!(
                "{alias}: ic3 token: {}",
                super::auth::summary_line(self.token.as_ref(), now)
            ),
            format!("{alias}: chat base {}", self.base),
        ];
        let reference = self.shape_of(ThreadReadKind::Thread);
        for read in &self.reads {
            lines.push(format!(
                "{alias}: GET {} page {}: {}",
                read.kind.as_str(),
                read.page,
                outcome_line(&read.outcome)
            ));
            if let Ok(answer) = &read.outcome {
                let shape = serde_json::from_slice::<Value>(answer.body())
                    .map(|body| Shape::of(&body, &self.chat_id))
                    .unwrap_or_default();
                lines.push(format!("{alias}:   {}", shape_line(&shape, reference.as_ref())));
            }
        }
        lines
    }

    /// The first page's shape of the read `kind`, when it answered with JSON.
    fn shape_of(&self, kind: ThreadReadKind) -> Option<Shape> {
        let read = self.reads.iter().find(|read| read.kind == kind && read.page == 1)?;
        let body = serde_json::from_slice::<Value>(read.outcome.as_ref().ok()?.body()).ok()?;
        Some(Shape::of(&body, &self.chat_id))
    }

    /// The reads as capture-like events for a reference copy: per read its kind and page, the URL as sent to the chat
    /// service, the request's header names, the status, the answer's headers and its body. The token is in none of
    /// them; the sanitizer still runs over everything.
    #[must_use]
    pub fn reference_events(&self) -> Vec<Value> {
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        let request_headers = ["Authorization", "Accept", "X-MS-Migration", "BehaviorOverride"];
        self.reads
            .iter()
            .zip(1_u64..)
            .map(|(read, seq)| {
                let mut event = json!({
                    "seq": seq,
                    "ts_ms": ts_ms,
                    "type": "probe_read",
                    "read": read.kind.as_str(),
                    "page": read.page,
                    "method": "GET",
                    "url": read.url.as_str(),
                    "request_headers": request_headers,
                });
                match &read.outcome {
                    Ok(answer) => {
                        event["status"] = json!(answer.status);
                        event["response_headers"] = Value::Object(response_headers(answer));
                        event["body"] = body_value(answer.body());
                    }
                    Err(failure) => event["error"] = json!(format!("{failure:?}")),
                }
                event
            })
            .collect()
    }
}

/// A shape as one line: keys, member count, paging keys, and how it compares with the thread read's `reference`.
fn shape_line(shape: &Shape, reference: Option<&Shape>) -> String {
    let members = shape
        .members
        .map_or_else(|| "no members list".to_owned(), |count| format!("{count} member(s)"));
    let mut line = format!(
        "keys [{}]; _metadata [{}]; properties [{}]; {members}; paging keys [{}]",
        shape.keys.join(", "),
        shape.metadata_keys.join(", "),
        shape.property_keys.join(", "),
        shape.paging_keys.join(", ")
    );
    if let (Some(reference), Some(_)) = (reference, shape.members) {
        let same = |yes: bool| if yes { "same" } else { "different" };
        let _ = write!(
            line,
            "; members {} as the thread read's",
            same(reference.member_ids == shape.member_ids)
        );
        if shape.roster_version.is_some() || reference.roster_version.is_some() {
            let _ = write!(
                line,
                "; rosterVersion {}",
                same(reference.roster_version == shape.roster_version)
            );
        }
    }
    line
}
