//! `proto-lab probe profile <alias>`: does Microsoft Graph give the lab accounts' names and pictures to the account's
//! own Graph token? Read-only (M2 Task 12).
//!
//! Graph is our own reach, not the web client's: the web client names people through the middle tier's profile
//! lookups (reads by `POST`) and draws their pictures from the middle tier's `profilepicturev2` with a cookie it gets
//! from `cookiev2` (`docs/protocol/endpoints.md`). So nothing captured shows these reads, and this probe's answers are
//! the shape the native client's Graph reads are reviewed against (`docs/protocol/graph.md`).
//!
//! 1. The account's Graph access token is refreshed from its stored refresh token ([`TokenClient::refresh`], audience
//!    [`GRAPH_RESOURCE`]); nothing else is asked of Entra.
//! 2. Only `GET`s go to [`GRAPH_BASE`], each with the token and `Accept: application/json` (the native transport's
//!    form), for every lab account with an MRI of the form `8:orgid:<object id>` ([`ProfileReadKind`]): the user
//!    record whole (`users/<id>`), the user record as the native client asks for it (`users/<id>?$select=`
//!    [`USER_SELECT`]), the picture's bytes (`users/<id>/photo/$value`, the largest size Graph has) and its 96×96
//!    form (`users/<id>/photos/96x96/$value`, the size an avatar needs).
//! 3. The report ([`ProfileProbe::lines`]) shows per read its status, its header names, the answer's
//!    `Content-Type`, its size, a JSON answer's top-level key names with their JSON types, and a Graph error's `code`
//!    when it is plain (`ImageNotFound`); the Graph token's claims (aud, exp, scopes, tenant). Never a token, a value
//!    of the answer, an object id or a picture.
//!
//! A test reaches a local mock instead of Graph only through [`Origin::Mock`], which a build without `teams-fence`'s
//! `test-support` feature refuses (and the `proto-lab` binary refuses to run in such a build).

use std::fmt::{self, Write as _};
use std::time::SystemTime;

use serde_json::Value;
use teams_auth::claims::TokenSummary;
use teams_auth::secrets::SecretStore;
use teams_auth::token::{Scope, TokenClient};
use url::Url;

use super::probe::{Answer, HeaderForm, Origin, ProbeError, ReadFailure, at_origin, client, outcome_line, read};
use crate::config::{Alias, LabConfig};

/// The audience of the Graph token.
pub const GRAPH_RESOURCE: &str = "https://graph.microsoft.com";

/// Graph's v1.0 base: the only one read.
pub const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// The fields the native client's user read selects: the record's id and the name it shows.
pub const USER_SELECT: &str = "id,displayName";

/// The headers every read carries besides the token and `Accept`: none (the native transport's form for Graph).
const PLAIN: HeaderForm = HeaderForm {
    migration: false,
    behavior_override: false,
};

/// Which read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileReadKind {
    /// `users/<id>`: the user record as Graph gives it by default.
    User,
    /// `users/<id>?$select=`[`USER_SELECT`]: the native client's read.
    UserSelect,
    /// `users/<id>/photo/$value`: the picture's bytes, at the largest size Graph has.
    Photo,
    /// `users/<id>/photos/96x96/$value`: the picture at 96×96.
    Photo96,
}

impl ProfileReadKind {
    /// Every read, in the order made for each account.
    pub const ALL: [Self; 4] = [Self::User, Self::UserSelect, Self::Photo, Self::Photo96];

    /// How the report names it (the object id left out).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "users/<id>",
            Self::UserSelect => "users/<id>?$select=id,displayName",
            Self::Photo => "users/<id>/photo/$value",
            Self::Photo96 => "users/<id>/photos/96x96/$value",
        }
    }

    /// The read's URL for the object id `id` (one encoded path segment).
    fn url(self, id: &str) -> Option<Url> {
        let mut url = Url::parse(&format!("{GRAPH_BASE}/")).ok()?;
        let segments: &[&str] = match self {
            Self::User | Self::UserSelect => &["users", id],
            Self::Photo => &["users", id, "photo", "$value"],
            Self::Photo96 => &["users", id, "photos", "96x96", "$value"],
        };
        url.path_segments_mut().ok()?.pop_if_empty().extend(segments);
        if self == Self::UserSelect {
            url.query_pairs_mut().append_pair("$select", USER_SELECT);
        }
        Some(url)
    }
}

/// One read: which, of whose record, and what came back. `Debug` leaves the answer's body out.
#[derive(Clone, Debug)]
pub struct ProfileRead {
    pub kind: ProfileReadKind,
    /// The lab account whose record was read.
    pub subject: Alias,
    pub outcome: Result<Answer, ReadFailure>,
}

/// What the probe found. Holds no token and shows no answer's value.
pub struct ProfileProbe {
    pub alias: Alias,
    /// The refreshed Graph token's claims; `None` for an opaque token.
    pub token: Option<TokenSummary>,
    pub reads: Vec<ProfileRead>,
    /// The lab accounts not read: no MRI in lab.toml, or one that isn't `8:orgid:<object id>`.
    pub skipped: Vec<Alias>,
}

impl fmt::Debug for ProfileProbe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProfileProbe")
            .field("alias", &self.alias)
            .field("token", &self.token)
            .field("reads", &self.reads.len())
            .field("skipped", &self.skipped)
            .finish()
    }
}

/// Probes Graph's user and picture reads of every lab account as `alias` (see the module docs).
///
/// # Errors
/// [`ProbeError`] when nothing could be read: a refused or failed refresh, a mock origin in a normal build. A read
/// Graph refuses is in the report instead.
pub async fn probe_profile<S: SecretStore>(
    store: &S,
    tokens: &TokenClient,
    config: &LabConfig,
    alias: Alias,
    origin: &Origin,
) -> Result<ProfileProbe, ProbeError> {
    if matches!(origin, Origin::Mock(_)) && !teams_fence::test_support_enabled() {
        return Err(ProbeError::MockOrigin);
    }
    let account = &config.account(alias).upn;
    let graph = tokens
        .refresh(store, account, &Scope::for_resource(GRAPH_RESOURCE))
        .await?;
    let token = graph.access_token.expose();
    let client = client(origin)?;
    let mut reads = Vec::new();
    let mut skipped = Vec::new();
    for subject in Alias::ALL {
        let Some(id) = config.account(subject).mri.as_deref().and_then(object_id) else {
            skipped.push(subject);
            continue;
        };
        for kind in ProfileReadKind::ALL {
            let Some(target) = kind.url(id).and_then(|url| at_origin(url.as_str(), origin)) else {
                continue;
            };
            let outcome = read(&client, target, token, PLAIN).await;
            reads.push(ProfileRead { kind, subject, outcome });
        }
    }
    Ok(ProfileProbe {
        alias,
        token: TokenSummary::read(&graph.access_token),
        reads,
        skipped,
    })
}

/// The object id of an MRI `8:orgid:<object id>`, when it is a GUID.
#[must_use]
pub fn object_id(mri: &str) -> Option<&str> {
    mri.strip_prefix("8:orgid:").filter(|id| is_guid(id))
}

/// A GUID in its hyphenated form, any case.
fn is_guid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|group| group.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

impl ProfileProbe {
    /// The report as lines for the operator, as of `now` (see the module docs for what they show and never show).
    #[must_use]
    pub fn lines(&self, now: SystemTime) -> Vec<String> {
        let alias = self.alias;
        let mut lines = vec![format!(
            "{alias}: Graph token: {}",
            super::auth::summary_line(self.token.as_ref(), now)
        )];
        for read in &self.reads {
            lines.push(format!(
                "{alias}: GET {} of {}: {}",
                read.kind.as_str(),
                read.subject,
                outcome_line(&read.outcome)
            ));
            if let Ok(answer) = &read.outcome {
                lines.push(format!("{alias}:   {}", body_line(answer)));
            }
        }
        for subject in &self.skipped {
            lines.push(format!(
                "{alias}: {subject} not read: no 8:orgid:<object id> MRI in lab.toml"
            ));
        }
        lines
    }
}

/// What an answer's body shows: its `Content-Type`, its size, and for JSON its keys with their types and a Graph
/// error's plain `code`.
fn body_line(answer: &Answer) -> String {
    let content_type = answer.header("content-type").map_or("none", plain_content_type);
    let mut line = format!("content-type {content_type}; {} bytes", answer.body().len());
    if let Ok(Value::Object(object)) = serde_json::from_slice::<Value>(answer.body()) {
        let mut keys: Vec<String> = object
            .iter()
            .map(|(key, value)| format!("{}:{}", shown_key(key), json_type(value)))
            .collect();
        keys.sort();
        let _ = write!(line, "; keys [{}]", keys.join(", "));
        let code = object
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str);
        if let Some(code) = code {
            let _ = write!(line, "; error code {}", plain_code(code));
        }
    }
    line
}

/// A media type without its parameters, when it is `type/subtype` of plain characters; `?` otherwise.
pub(crate) fn plain_content_type(value: &str) -> &str {
    let media = value.split(';').next().unwrap_or_default().trim();
    let plain = !media.is_empty()
        && media.len() <= 64
        && media.split('/').count() == 2
        && media
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'+'));
    if plain { media } else { "?" }
}

/// A key that may be shown: letters, digits, `@`, `.`, `_` and `-`, short; `?` otherwise (a key that might be a value).
fn shown_key(key: &str) -> &str {
    let plain = (1..=64).contains(&key.len())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'@' | b'.' | b'_' | b'-'));
    if plain { key } else { "?" }
}

/// An error code that may be shown: letters, digits and `_`, short; `?` otherwise.
fn plain_code(code: &str) -> &str {
    let plain = (1..=64).contains(&code.len()) && code.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if plain { code } else { "?" }
}

pub(crate) fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
