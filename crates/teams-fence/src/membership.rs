//! The live membership source: who takes part in a scope, read fresh from Teams' chat service on every check.
//!
//! [`LiveMembershipSource`] owns the whole read: it builds the request from the [`Scope`] (the scope id is one
//! encoded path segment), sends it with its own HTTPS client (no proxy, no redirects, a fixed allowlist of chat
//! service hosts, no caller-chosen path or query) and parses the answer. The caller supplies only the lab account's
//! credential ([`ChatCredential`]), taken from a request the armed lab page itself sent to the chat service: the
//! skypetoken and the chat service base that request went to. A made-up credential gets a 401 and a deny; nothing the
//! caller supplies can make up a member list.
//!
//! **What is supported** (M0 Task 11, from the web bundle; see `docs/protocol/fence-targets.md`): only 1:1 chats,
//! `19:<oid>_<oid>@unq.gbl.spaces`. Their id names both members, so an answer is [`Membership::Complete`] only when
//! the thread the service returns is that exact thread and lists exactly those two members (`8:orgid:<oid>`); that
//! cross-check is the evidence that the list is the whole membership and not a view filtered by the caller's
//! permissions. Every other scope (group and meeting chats, channels, teams, calls) is
//! [`PartialReason::Unsupported`] until a capture shows how its full membership can be confirmed.
//!
//! Anything less than a clean answer is [`Membership::Partial`]: no credential yet, a failed or slow request, a
//! non-success status, a body over [`MAX_BODY`] (truncated), a pagination marker, a member count above the listed
//! members, or a body that isn't the expected thread.

use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use percent_encoding::percent_decode_str;
use serde_json::Value;
use url::Url;
use zeroize::Zeroizing;

use crate::decision::{FetchFailure, Membership, MembershipFuture, MembershipSource, PartialReason};
use crate::target::{Mri, Scope};

/// The hosts the chat service is reached on through Azure Front Door, as `https://<host>/api/chatsvc/<region>/`: the
/// commercial cloud's two Teams origins.
pub const AFD_HOSTS: [&str; 2] = ["teams.microsoft.com", "teams.cloud.microsoft"];

/// The host suffix of the chat service's regional hosts (`https://<name>.msg.teams.microsoft.com/`), matched on a
/// dot boundary.
pub const REGIONAL_SUFFIX: &str = ".msg.teams.microsoft.com";

/// The largest member-list body read, in bytes; a longer one is [`PartialReason::Truncated`].
pub const MAX_BODY: usize = 1024 * 1024;

/// How long one membership read may take, end to end.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The suffix of a 1:1 chat's conversation id.
const ONE_TO_ONE_SUFFIX: &str = "@unq.gbl.spaces";

/// Where the chat service is, as a validated base: never a caller-chosen path, port or query.
#[derive(Clone, PartialEq, Eq)]
enum ChatBase {
    /// `https://<AFD host>/api/chatsvc/<region>/`.
    Afd { host: &'static str, region: String },
    /// `https://<name>.msg.teams.microsoft.com/`.
    Regional { host: String },
}

impl ChatBase {
    /// The base a chat service request URL went to, if it is one, and the raw path segments after its `v1`: `https`,
    /// no user info, no port, and either an AFD host with a `/api/chatsvc/<region>/v1/…` path or a regional host (every
    /// DNS label checked) with a `/v1/…` path.
    fn split(url: &Url) -> Option<(Self, Vec<&str>)> {
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
            return None;
        }
        let host = url.host_str()?.to_ascii_lowercase();
        let mut segments = url.path_segments()?;
        let base = if let Some(afd) = AFD_HOSTS.iter().find(|afd| **afd == host) {
            let (api, chatsvc, region) = (segments.next()?, segments.next()?, segments.next()?);
            if api != "api" || chatsvc != "chatsvc" || !is_region(region) {
                return None;
            }
            Self::Afd {
                host: afd,
                region: region.to_owned(),
            }
        } else {
            let name = host.strip_suffix(REGIONAL_SUFFIX)?;
            let valid = !name.is_empty()
                && name.split('.').all(|label| {
                    !label.is_empty() && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                });
            if !valid {
                return None;
            }
            Self::Regional { host }
        };
        (segments.next()? == "v1").then(|| (base, segments.collect()))
    }

    /// `https://<host>` and the path segments that lead to `v1`.
    fn origin_and_prefix(&self) -> (String, Vec<&str>) {
        match self {
            Self::Afd { host, region } => (format!("https://{host}"), vec!["api", "chatsvc", region]),
            Self::Regional { host } => (format!("https://{host}"), Vec::new()),
        }
    }
}

/// The chat service path of a request URL after its base, from `v1` on (`v1` itself left out), each segment
/// percent-decoded: `Some` only for a URL whose base the live read accepts too (see [`ChatCredential::from_request`]).
/// The browser fence's write mapping reads its targets from it, so the mapping and the live read recognize the chat
/// service by one check.
#[must_use]
pub fn chat_service_path(url: &Url) -> Option<Vec<String>> {
    let (_, rest) = ChatBase::split(url)?;
    rest.into_iter()
        .map(|segment| percent_decode_str(segment).decode_utf8().ok().map(String::from))
        .collect()
}

/// A chat service region in a base path: a short lowercase name (`emea`, `amer`, `apac`, …).
fn is_region(region: &str) -> bool {
    (1..=32).contains(&region.len())
        && region
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The lab account's chat service credential: the skypetoken a request of the armed lab page carried, and the chat
/// service base that request went to. `Debug` never shows the token, and it is wiped when dropped.
#[derive(Clone)]
pub struct ChatCredential {
    base: ChatBase,
    token: Zeroizing<String>,
}

impl fmt::Debug for ChatCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let base = match &self.base {
            ChatBase::Afd { host, region } => format!("{host} ({region})"),
            ChatBase::Regional { host } => host.clone(),
        };
        f.debug_struct("ChatCredential")
            .field("base", &base)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl ChatCredential {
    /// The credential of one request the page sent, given its URL and headers: `Some` only for a request to the chat
    /// service (an allowed host and path, see [`AFD_HOSTS`] and [`REGIONAL_SUFFIX`]) that carries a skypetoken, as
    /// `Authentication: skypetoken=<token>` or `X-Skypetoken: <token>` (header names in any case). Every other header
    /// is ignored: this crate decides how the token is sent.
    pub fn from_request<'a>(url: &str, headers: impl IntoIterator<Item = (&'a str, &'a str)>) -> Option<Self> {
        let (base, _) = ChatBase::split(&Url::parse(url).ok()?)?;
        let token = headers.into_iter().find_map(|(name, value)| {
            if name.eq_ignore_ascii_case("authentication") {
                value.trim().strip_prefix("skypetoken=")
            } else if name.eq_ignore_ascii_case("x-skypetoken") {
                Some(value.trim())
            } else {
                None
            }
        })?;
        is_token(token).then(|| Self {
            base,
            token: Zeroizing::new(token.to_owned()),
        })
    }
}

/// A skypetoken: a JWT, so URL-safe base64 and dots, and nothing that could break out of a header.
fn is_token(token: &str) -> bool {
    (20..=16 * 1024).contains(&token.len())
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// Why the live source couldn't be built.
#[derive(Debug, thiserror::Error)]
#[error("can't build the membership source's HTTPS client: {0}")]
pub struct LiveSourceError(#[source] reqwest::Error);

/// The membership source of a lab run that writes: it reads a scope's members from the chat service on every check,
/// with the lab account's [`ChatCredential`] (see the module docs). Until a credential is set every scope is
/// [`PartialReason::NoCredential`].
pub struct LiveMembershipSource {
    client: reqwest::Client,
    credential: Mutex<Option<ChatCredential>>,
    /// Tests only: every read goes to this origin instead of the credential's host.
    #[cfg(feature = "test-support")]
    mock_origin: Option<String>,
}

impl fmt::Debug for LiveMembershipSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveMembershipSource")
            .field("credential", &self.credential())
            .finish_non_exhaustive()
    }
}

impl LiveMembershipSource {
    /// The source, with no credential yet.
    ///
    /// # Errors
    /// When the HTTPS client can't be built.
    pub fn new() -> Result<Self, LiveSourceError> {
        Ok(Self {
            client: client(true)?,
            credential: Mutex::new(None),
            #[cfg(feature = "test-support")]
            mock_origin: None,
        })
    }

    /// A source whose reads go to `origin` (`http://127.0.0.1:<port>`, a local mock) instead of the chat service,
    /// with the credential's path. Tests only (`test-support`).
    ///
    /// # Errors
    /// When the client can't be built.
    #[cfg(feature = "test-support")]
    pub fn with_mock_origin(origin: &str) -> Result<Self, LiveSourceError> {
        Ok(Self {
            client: client(false)?,
            credential: Mutex::new(None),
            mock_origin: Some(origin.trim_end_matches('/').to_owned()),
        })
    }

    /// Uses `credential` for every read from now on (the newest one the page sent wins).
    pub fn set_credential(&self, credential: ChatCredential) {
        *self.credential.lock().unwrap_or_else(PoisonError::into_inner) = Some(credential);
    }

    /// Whether a credential has been set.
    #[must_use]
    pub fn has_credential(&self) -> bool {
        self.credential().is_some()
    }

    fn credential(&self) -> Option<ChatCredential> {
        self.credential.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The URL of a thread's read: the base's origin (or the mock's), its path to `v1`, then `threads/<id>`, the id one
    /// encoded segment, and the fixed view.
    fn thread_url(&self, base: &ChatBase, id: &str) -> Option<Url> {
        let (origin, prefix) = base.origin_and_prefix();
        #[cfg(feature = "test-support")]
        let origin = self.mock_origin.clone().unwrap_or(origin);
        let mut url = Url::parse(&origin).ok()?;
        url.path_segments_mut()
            .ok()?
            .clear()
            .extend(prefix)
            .extend(["v1", "threads", id]);
        url.set_query(Some("view=msnp24Equivalent"));
        Some(url)
    }

    async fn one_to_one(&self, id: &str, pair: [Mri; 2]) -> Membership {
        let Some(credential) = self.credential() else {
            return Membership::Partial(PartialReason::NoCredential);
        };
        let Some(url) = self.thread_url(&credential.base, id) else {
            return Membership::Partial(PartialReason::FetchFailed(FetchFailure::Malformed));
        };
        let body = match self.fetch(url, &credential).await {
            Ok(body) => body,
            Err(reason) => return Membership::Partial(reason),
        };
        let Ok(thread) = serde_json::from_slice::<Value>(&body) else {
            return Membership::Partial(PartialReason::FetchFailed(FetchFailure::Malformed));
        };
        match parse_thread(id, &thread) {
            Membership::Complete(members) if !same_members(&members, &pair) => {
                Membership::Partial(PartialReason::FetchFailed(FetchFailure::Malformed))
            }
            membership => membership,
        }
    }

    /// One GET with the skypetoken, its body read up to [`MAX_BODY`]; the client's timeout covers the whole read.
    async fn fetch(&self, url: Url, credential: &ChatCredential) -> Result<Vec<u8>, PartialReason> {
        let header = Zeroizing::new(format!("skypetoken={}", credential.token.as_str()));
        let mut response = self
            .client
            .get(url)
            .header("Authentication", header.as_str())
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|error| failure(&error))?;
        let status = response.status();
        if !status.is_success() {
            return Err(PartialReason::FetchFailed(FetchFailure::Status(status.as_u16())));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| failure(&error))? {
            if body.len() + chunk.len() > MAX_BODY {
                return Err(PartialReason::Truncated);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

impl MembershipSource for LiveMembershipSource {
    fn members<'a>(&'a self, scope: &'a Scope) -> MembershipFuture<'a> {
        Box::pin(async move {
            match scope {
                Scope::Conversation { id } => match one_to_one_pair(id) {
                    Some(pair) => self.one_to_one(id, pair).await,
                    None => Membership::Partial(PartialReason::Unsupported),
                },
                _ => Membership::Partial(PartialReason::Unsupported),
            }
        })
    }
}

/// The HTTPS client: no proxy (an environment's `HTTPS_PROXY` could point at anything), no redirects, `https` only
/// unless it serves a local mock.
fn client(https_only: bool) -> Result<reqwest::Client, LiveSourceError> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .https_only(https_only)
        .connect_timeout(FETCH_TIMEOUT)
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(LiveSourceError)
}

fn failure(error: &reqwest::Error) -> PartialReason {
    if error.is_timeout() {
        PartialReason::FetchFailed(FetchFailure::Timeout)
    } else if error.is_decode() || error.is_body() {
        PartialReason::FetchFailed(FetchFailure::Malformed)
    } else {
        PartialReason::FetchFailed(FetchFailure::Network)
    }
}

/// The two members a 1:1 chat id names: `19:<oid>_<oid>@unq.gbl.spaces` → `8:orgid:<oid>` each, lowercased. `None`
/// for any other id.
#[must_use]
pub fn one_to_one_pair(id: &str) -> Option<[Mri; 2]> {
    let pair = id.strip_prefix("19:")?.strip_suffix(ONE_TO_ONE_SUFFIX)?;
    let (first, second) = pair.split_once('_')?;
    if !is_guid(first) || !is_guid(second) || first.eq_ignore_ascii_case(second) {
        return None;
    }
    let mri = |oid: &str| Mri::new(format!("8:orgid:{}", oid.to_ascii_lowercase()));
    Some([mri(first), mri(second)])
}

fn is_guid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn same_members(members: &[Mri], pair: &[Mri; 2]) -> bool {
    let lower = |mri: &Mri| mri.as_str().to_ascii_lowercase();
    let mut got: Vec<String> = members.iter().map(lower).collect();
    got.sort();
    got.dedup();
    let mut wanted: Vec<String> = pair.iter().map(lower).collect();
    wanted.sort();
    got.len() == members.len() && got == wanted
}

/// Reads a chat service thread (`GET v1/threads/<id>?view=msnp24Equivalent`) as the membership of the thread `id`:
/// - a pagination marker anywhere at the top level or under `_metadata` (a key holding `next`, `continuation`,
///   `skiptoken` or `syncstate`, in any case) → [`PartialReason::Paginated`];
/// - a member count (`memberCount`, `membercount`, `membersCount`, at the top level or under `properties`, a number
///   or a numeric string) other than the number of listed members → [`PartialReason::Truncated`];
/// - no object, an `id` other than `id`, no `members` array, or a member without a string `id` → malformed;
/// - otherwise the listed members' ids, [`Membership::Complete`].
#[must_use]
pub fn parse_thread(id: &str, thread: &Value) -> Membership {
    let malformed = Membership::Partial(PartialReason::FetchFailed(FetchFailure::Malformed));
    let Some(object) = thread.as_object() else {
        return malformed;
    };
    let metadata = object.get("_metadata").and_then(Value::as_object);
    if object
        .keys()
        .chain(metadata.into_iter().flat_map(|meta| meta.keys()))
        .any(|key| is_page_marker(key))
    {
        return Membership::Partial(PartialReason::Paginated);
    }
    if object.get("id").and_then(Value::as_str) != Some(id) {
        return malformed;
    }
    let Some(listed) = object.get("members").and_then(Value::as_array) else {
        return malformed;
    };
    let Some(members) = listed
        .iter()
        .map(|member| member.get("id").and_then(Value::as_str).map(Mri::new))
        .collect::<Option<Vec<Mri>>>()
    else {
        return malformed;
    };
    let properties = object.get("properties").and_then(Value::as_object);
    let counts = object
        .iter()
        .chain(properties.into_iter().flat_map(|map| map.iter()))
        .filter(|(key, _)| matches!(key.to_ascii_lowercase().as_str(), "membercount" | "memberscount"));
    for (_, count) in counts {
        let count = count
            .as_u64()
            .or_else(|| count.as_str().and_then(|text| text.trim().parse().ok()));
        if count != u64::try_from(members.len()).ok() {
            return Membership::Partial(PartialReason::Truncated);
        }
    }
    Membership::Complete(members)
}

fn is_page_marker(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    ["next", "continuation", "skiptoken", "syncstate"]
        .iter()
        .any(|marker| key.contains(marker))
}
