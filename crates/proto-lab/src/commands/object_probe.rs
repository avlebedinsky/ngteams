//! `proto-lab probe object <alias> <id> [--save <name>]`: what the async gateway answers about one media service
//! (AMS) object to the account's own `ic3` token. Read-only (M2 Task 21, `docs/backlog/ams-image-sending.md`): does
//! the object's read list who may read it, and completely? It doesn't: the object M2 Task 21 created once for the a–b
//! chat answered its creator and the chat's other member with eight keys of metadata, none naming a reader, and c with
//! a 403 (`docs/protocol/chat.md`, "Images"). The probe stays for a revisit of the backlog item.
//!
//! 1. The id must be one path segment of the form the media service gives ([`is_object_id`]); a bad one fails before
//!    anything is asked. The account's `ic3` access token is refreshed from its stored refresh token
//!    ([`TokenClient::refresh`], audience [`IC3_RESOURCE`]); nothing else is asked of Entra.
//! 2. Only `GET`s go to [`ASYNC_GATEWAY`]: the object (`v1/objects/<id>/`, the bundle's read of it) once with the web
//!    client's headers (the token and `X-MS-Migration: True`, as its object creation sent them) and once with the
//!    token alone ([`OBJECT_FORMS`]).
//! 3. The report ([`ObjectProbe::lines`]) shows per read its status and header names, the answer's `Content-Type` and
//!    size, and a JSON answer's outline ([`outline`]): every key path with its JSON type, where a key or a string that
//!    is a lab account's MRI, a chat between lab accounts or the object's own id is named for what it is (`<a's
//!    MRI>`, `<the a-b chat>`, `<the object>`), a plain word is shown (`read`, `pish/image`), a boolean as it is, and
//!    any other value only by its type and length. Never a token, another value of the answer, or a header value.
//!
//! `--save <name>` hands the answers ([`ObjectProbe::reference_events`]) to
//! [`export::export_events`](crate::commands::export::export_events), sanitized as one capture, as `probe chat` does.
//!
//! A test reaches a local mock instead of the gateway only through [`Origin::Mock`], which a build without
//! `proto-lab`'s `test-support` feature refuses (and the `proto-lab` binary refuses to run in such a build).

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use teams_auth::claims::TokenSummary;
use teams_auth::secrets::SecretStore;
use teams_auth::token::{Scope, TokenClient};

use super::probe::{
    Answer, HeaderForm, IC3_RESOURCE, Origin, ProbeError, ReadFailure, at_origin, body_value, client, outcome_line,
    read, response_headers,
};
use super::profile_probe::{json_type, plain_content_type};
use super::teams_probe::shown_key;
use crate::config::{Alias, LabConfig};
use crate::scenario::ab_chat::one_to_one_id;

/// The async gateway, where the web client created the object (`docs/protocol/chat.md`, "Images").
pub const ASYNC_GATEWAY: &str = "https://us-prod.asyncgw.teams.microsoft.com";

/// The forms of the object read: the web client's own headers first, the bare token last.
pub const OBJECT_FORMS: [HeaderForm; 2] = [
    HeaderForm {
        migration: true,
        behavior_override: false,
    },
    HeaderForm {
        migration: false,
        behavior_override: false,
    },
];

/// The longest object id read.
const MAX_ID: usize = 128;

/// How deep an outline goes, and how many lines it gives at most.
const OUTLINE_DEPTH: usize = 8;
const OUTLINE_LINES: usize = 200;

/// How many entries of one list an outline shows.
const OUTLINE_ITEMS: usize = 20;

/// Whether `id` can be an object's id in one path segment: 1 to [`MAX_ID`] bytes of ASCII letters, digits, `-`, `_`
/// and `.` (the media service's are like `0-wus-d1-<hex>`), not `.` or `..`.
#[must_use]
pub fn is_object_id(id: &str) -> bool {
    (1..=MAX_ID).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && id != "."
        && id != ".."
}

/// The object read's URL.
#[must_use]
pub fn object_url(id: &str) -> String {
    format!("{ASYNC_GATEWAY}/v1/objects/{id}/")
}

/// Why the object probe couldn't run.
#[derive(Debug, thiserror::Error)]
pub enum ObjectProbeError {
    #[error("not an object id (one path segment of letters, digits, '-', '_' and '.')")]
    BadId,
    #[error(transparent)]
    Probe(#[from] ProbeError),
}

impl From<teams_auth::token::TokenError> for ObjectProbeError {
    fn from(error: teams_auth::token::TokenError) -> Self {
        Self::Probe(ProbeError::Token(error))
    }
}

/// One object read.
#[derive(Clone, Debug)]
pub struct ObjectRead {
    pub form: HeaderForm,
    pub outcome: Result<Answer, ReadFailure>,
}

/// The values an outline names for what they are, each with its name.
#[derive(Clone, Default)]
pub struct Known(Vec<(String, String)>);

impl Known {
    /// The lab accounts' MRIs, the 1:1 chats between them, and the object's own id.
    #[must_use]
    pub fn of(config: &LabConfig, object: &str) -> Self {
        let mut known = Vec::new();
        let mris: Vec<(Alias, String)> = Alias::ALL
            .iter()
            .filter_map(|&alias| Some((alias, config.account(alias).mri.clone()?)))
            .collect();
        for (alias, mri) in &mris {
            known.push((mri.clone(), format!("<{alias}'s MRI>")));
        }
        for (at, (one, mri)) in mris.iter().enumerate() {
            for (other, other_mri) in &mris[at + 1..] {
                if let Some(chat) = one_to_one_id(mri, other_mri) {
                    known.push((chat, format!("<the {one}-{other} chat>")));
                }
            }
        }
        known.push((object.to_owned(), "<the object>".to_owned()));
        Self(known)
    }

    fn name(&self, value: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(known, _)| known == value)
            .map(|(_, name)| name.as_str())
    }
}

impl fmt::Debug for Known {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Known({} value(s))", self.0.len())
    }
}

/// What the probe found. Holds no token; the answers' header values and bodies only for [`Self::reference_events`].
/// `Debug` shows neither.
#[derive(Debug)]
pub struct ObjectProbe {
    pub alias: Alias,
    /// The refreshed `ic3` token's claims; `None` for an opaque token.
    pub token: Option<TokenSummary>,
    pub object: String,
    /// One read per [`OBJECT_FORMS`], in that order.
    pub reads: Vec<ObjectRead>,
    known: Known,
}

/// Reads the object `id` as `alias` (see the module docs).
///
/// # Errors
/// [`ObjectProbeError::BadId`] before anything is sent; [`ProbeError`] when nothing could be read (a refused or failed
/// refresh, a mock origin in a normal build). A read the gateway refuses is in the report instead.
pub async fn probe_object<S: SecretStore>(
    store: &S,
    tokens: &TokenClient,
    config: &LabConfig,
    alias: Alias,
    id: &str,
    origin: &Origin,
) -> Result<ObjectProbe, ObjectProbeError> {
    if !is_object_id(id) {
        return Err(ObjectProbeError::BadId);
    }
    if matches!(origin, Origin::Mock(_)) && !crate::test_support_enabled() {
        return Err(ProbeError::MockOrigin.into());
    }
    let account = &config.account(alias).upn;
    let ic3 = tokens
        .refresh(store, account, &Scope::for_resource(IC3_RESOURCE))
        .await?;
    let token = ic3.access_token.expose();
    let client = client(origin)?;
    let target = at_origin(&object_url(id), origin).ok_or(ObjectProbeError::BadId)?;
    let mut reads = Vec::with_capacity(OBJECT_FORMS.len());
    for form in OBJECT_FORMS {
        let outcome = read(&client, target.clone(), token, form).await;
        reads.push(ObjectRead { form, outcome });
    }
    Ok(ObjectProbe {
        alias,
        token: TokenSummary::read(&ic3.access_token),
        object: id.to_owned(),
        reads,
        known: Known::of(config, id),
    })
}

impl ObjectProbe {
    /// The report as lines for the operator, as of `now` (see the module docs for what they show and never show).
    #[must_use]
    pub fn lines(&self, now: SystemTime) -> Vec<String> {
        let alias = self.alias;
        let mut lines = vec![format!(
            "{alias}: ic3 token: {}",
            super::auth::summary_line(self.token.as_ref(), now)
        )];
        for read in &self.reads {
            lines.push(format!(
                "{alias}: GET v1/objects/<the object>/ ({}): {}",
                read.form,
                outcome_line(&read.outcome)
            ));
            let Ok(answer) = &read.outcome else { continue };
            let content_type = answer.header("content-type").map_or("none", plain_content_type);
            lines.push(format!(
                "{alias}:   content-type {content_type}; {} bytes",
                answer.body().len()
            ));
            if let Ok(value) = serde_json::from_slice::<Value>(answer.body()) {
                lines.extend(
                    outline(&value, &self.known)
                        .into_iter()
                        .map(|line| format!("{alias}:   {line}")),
                );
            }
        }
        lines
    }

    /// The reads as capture-like events for a reference copy, as `probe chat` makes them: the URL, the request's
    /// header names, the status, the answer's headers and body. The token is in none of them; the sanitizer still runs.
    #[must_use]
    pub fn reference_events(&self) -> Vec<Value> {
        let url = object_url(&self.object);
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        self.reads
            .iter()
            .zip(1_u64..)
            .map(|(read, seq)| {
                let mut event = json!({
                    "seq": seq,
                    "ts_ms": ts_ms,
                    "type": "probe_read",
                    "form": read.form.to_string(),
                    "method": "GET",
                    "url": url,
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

/// The outline of a JSON answer: one line per key path, `path: type`, a scalar's value as far as it may be shown
/// ([`shown`]), a list's length; a key named for what it is when `known` knows it, as it is when `probe teams` would
/// show it (no run of four digits or of hex with a digit, short), `?` otherwise. At most [`OUTLINE_DEPTH`] levels, [`OUTLINE_ITEMS`] entries of a list and [`OUTLINE_LINES`] lines.
#[must_use]
pub fn outline(value: &Value, known: &Known) -> Vec<String> {
    let mut lines = Vec::new();
    walk(value, "$", 0, known, &mut lines);
    if lines.len() > OUTLINE_LINES {
        let more = lines.len() - OUTLINE_LINES;
        lines.truncate(OUTLINE_LINES);
        lines.push(format!("… {more} more line(s)"));
    }
    lines
}

fn walk(value: &Value, path: &str, depth: usize, known: &Known, lines: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            lines.push(format!("{path}: object, {} key(s)", object.len()));
            if depth >= OUTLINE_DEPTH {
                return;
            }
            for (key, value) in object {
                let key = known.name(key).map_or_else(|| shown_key(key).to_owned(), str::to_owned);
                walk(value, &format!("{path}.{key}"), depth + 1, known, lines);
            }
        }
        Value::Array(items) => {
            lines.push(format!("{path}: array, {} item(s)", items.len()));
            if depth >= OUTLINE_DEPTH {
                return;
            }
            for (at, item) in items.iter().take(OUTLINE_ITEMS).enumerate() {
                walk(item, &format!("{path}[{at}]"), depth + 1, known, lines);
            }
        }
        scalar => lines.push(format!("{path}: {}", shown(scalar, known))),
    }
}

/// A scalar as an outline shows it: `null`, a boolean as it is, a number's type, a string `known` names by its name,
/// a plain word ([`is_plain_word`]) quoted, any other string by its length.
fn shown(value: &Value, known: &Known) -> String {
    match value {
        Value::Bool(flag) => format!("bool {flag}"),
        Value::String(text) => match known.name(text) {
            Some(name) => format!("string {name}"),
            None if is_plain_word(text) => format!("string {text:?}"),
            None => format!("string of {} char(s)", text.chars().count()),
        },
        other => json_type(other).to_owned(),
    }
}

/// A string value that may be shown: 1 to 32 bytes of ASCII letters, `/`, `-`, `_`, `.` and `:` with no digit, so
/// it can't carry an id, a time or a token (`read`, `pish/image`, `Inline`).
#[must_use]
pub fn is_plain_word(text: &str) -> bool {
    (1..=32).contains(&text.len())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b':'))
}
