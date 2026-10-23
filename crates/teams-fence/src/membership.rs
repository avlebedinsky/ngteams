//! The live membership source: who takes part in a scope, read fresh from Teams' chat service on every check.
//!
//! [`LiveMembershipSource`] owns the whole read: it builds the request from the [`Scope`] (the scope id is one
//! encoded path segment), sends it with its own HTTPS client (no proxy, no redirects, a fixed allowlist of chat
//! service hosts, no caller-chosen path or query) and parses the answer. The caller supplies only the lab account's
//! credential ([`ChatCredential`]), taken from a request the armed lab page itself sent to the chat service: its token
//! (an Entra bearer token, as the web client sends today, or a skypetoken) and the chat service base that request went
//! to; or, for a native client, its own `ic3` Entra token and the chat base from the account's region GTMs
//! ([`ChatCredential::bearer`]), taken fresh for every read from a [`CredentialSource`] (the native transport, over
//! the account's session, which renews the token). A made-up credential gets a 401 and a deny; nothing the
//! caller supplies can make up a member list.
//!
//! **What is supported** (from the web bundle and the live reads; see `docs/protocol/fence-targets.md`): 1:1 chats,
//! `19:<oid>_<oid>@unq.gbl.spaces`. Their id names both members, so an answer is [`Membership::Complete`] only when
//! the thread the service returns is that exact thread and lists exactly those two members (`8:orgid:<oid>`); that
//! cross-check is the evidence that the list is the whole membership and not a view filtered by the caller's
//! permissions.
//!
//! **Group and meeting chats** (`19:…@thread.v2`, [`is_group_chat_id`] and [`is_meeting_chat_id`]), any of them, one
//! the native client created too: one fresh read of the chat's member list, complete only in the shape
//! [`parse_members`] takes (no paging key, the count equal to the members listed, nobody hidden or deleted, nobody
//! twice, every member an MRI in a form the reads showed). Whether those members are all lab accounts or approved
//! services is the fence's to judge. Channels, teams and calls are [`PartialReason::Unsupported`] until evidence shows
//! how their full membership can be confirmed.
//!
//! Anything less than a clean answer is [`Membership::Partial`]: no credential yet, a failed or slow request, a
//! non-success status, a body over [`MAX_BODY`] (truncated), a pagination marker, a member count above the listed
//! members, or a body that isn't the expected thread.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use percent_encoding::percent_decode_str;
use serde_json::Value;
use url::Url;
use zeroize::Zeroizing;

use crate::decision::{Fence, FetchFailure, Membership, MembershipFuture, MembershipSource, PartialReason};
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

/// A read the chat service refused the credential of (HTTP 401).
const REFUSED: PartialReason = PartialReason::FetchFailed(FetchFailure::Status(401));

/// The page size a group chat's member list is read with, as `proto-lab probe thread` read it live.
const MEMBERS_PAGE_SIZE: &str = "100";

/// Which of a thread's reads a request makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThreadRead {
    /// The thread itself (`v1/threads/<id>`), with its members.
    Thread,
    /// The thread's member list (`v1/threads/<id>/members`).
    Members,
}

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
/// The contacting classifier ([`crate::contacting`]) reads its chat targets from it, so the classifier and the live
/// read recognize the chat service by one check.
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

/// How a chat service request carried its token, and so how the live read sends it back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scheme {
    /// `Authentication: skypetoken=<token>` (or `X-Skypetoken: <token>`): the skypetoken from authz.
    Skype,
    /// `Authorization: Bearer <token>`: an Entra access token for the chat service. The web client seen on 2026-10-20
    /// sends one (audience `https://ic3.teams.office.com`) on every chat service request, and no skypetoken there.
    Bearer,
}

/// The lab account's chat service credential, a token and the chat service base it is sent to, from one of two
/// sources: the token a request of the armed lab page carried (a skypetoken or an Entra bearer token) and the base
/// that request went to ([`ChatCredential::from_request`]), or a native client's own `ic3` Entra token and the chat
/// base its region GTMs give ([`ChatCredential::bearer`]). `Debug` never shows the token, and it is wiped when
/// dropped.
#[derive(Clone)]
pub struct ChatCredential {
    base: ChatBase,
    scheme: Scheme,
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
            .field("scheme", &self.scheme)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl ChatCredential {
    /// The credential of one request the page sent, given its URL and headers: `Some` only for a request to the chat
    /// service (an allowed host and path, see [`AFD_HOSTS`] and [`REGIONAL_SUFFIX`]) that carries a token: a
    /// skypetoken as `Authentication: skypetoken=<token>` or `X-Skypetoken: <token>`, or an Entra access token as
    /// `Authorization: Bearer <token>` (the scheme word in any case). Header names in any case; the first header that
    /// carries a token wins. Every other header is ignored: this crate decides how the token is sent back, in the form
    /// the page used.
    pub fn from_request<'a>(url: &str, headers: impl IntoIterator<Item = (&'a str, &'a str)>) -> Option<Self> {
        let (base, _) = ChatBase::split(&Url::parse(url).ok()?)?;
        let (scheme, token) = headers.into_iter().find_map(|(name, value)| {
            let value = value.trim();
            if name.eq_ignore_ascii_case("authentication") {
                value.strip_prefix("skypetoken=").map(|token| (Scheme::Skype, token))
            } else if name.eq_ignore_ascii_case("x-skypetoken") {
                Some((Scheme::Skype, value))
            } else if name.eq_ignore_ascii_case("authorization") {
                let (word, token) = value.split_once(' ')?;
                word.eq_ignore_ascii_case("bearer").then_some((Scheme::Bearer, token))
            } else {
                None
            }
        })?;
        is_token(token).then(|| Self {
            base,
            scheme,
            token: Zeroizing::new(token.to_owned()),
        })
    }

    /// The credential of a native client: its own Entra access token for the chat service (`ic3`), sent back as
    /// `Authorization: Bearer <token>`, and the chat service base it reads from, as the account's region GTMs give it
    /// (`chatServiceAfd`, `https://<AFD host>/api/chatsvc/<region>`, or a regional `chatService` host), one trailing
    /// slash allowed. `Some` only for a base that is exactly a base [`chat_service_path`] accepts a request under:
    /// nothing before or after it (no whitespace: a caller trims a GTM value first), no path beyond the base, no
    /// query or fragment; and a token-shaped token.
    #[must_use]
    pub fn bearer(base: &str, token: &str) -> Option<Self> {
        if base.contains(['?', '#']) || base.trim() != base {
            return None;
        }
        let probe = Url::parse(&format!("{}/v1", base.strip_suffix('/').unwrap_or(base))).ok()?;
        let (base, rest) = ChatBase::split(&probe)?;
        (rest.is_empty() && is_token(token)).then(|| Self {
            base,
            scheme: Scheme::Bearer,
            token: Zeroizing::new(token.to_owned()),
        })
    }

    /// The token, for the [`CredentialSource`] that gave it to tell its session which token the chat service refused
    /// ([`CredentialSource::credential_rejected`]). Never logged or shown: `Debug` redacts it.
    #[must_use]
    pub fn expose_token(&self) -> &str {
        &self.token
    }

    /// The header the live read sends the token in, in the page's form: its name and value.
    fn header(&self) -> (&'static str, Zeroizing<String>) {
        match self.scheme {
            Scheme::Skype => (
                "Authentication",
                Zeroizing::new(format!("skypetoken={}", self.token.as_str())),
            ),
            Scheme::Bearer => (
                "Authorization",
                Zeroizing::new(format!("Bearer {}", self.token.as_str())),
            ),
        }
    }
}

/// A token: a JWT (a skypetoken or an Entra access token), so URL-safe base64 and dots, and nothing that could break
/// out of a header.
fn is_token(token: &str) -> bool {
    (20..=16 * 1024).contains(&token.len())
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// The future of a [`CredentialSource`]'s credential.
pub type CredentialFuture<'a> = Pin<Box<dyn Future<Output = Option<ChatCredential>> + Send + 'a>>;

/// Where a native client's live read takes its credential from, fresh for every read: the account's own `ic3` token as
/// its session has it now, and the chat base its region gives ([`ChatCredential::bearer`]). `None` when there is no
/// token (signed out, a failed renewal): the read is then [`PartialReason::NoCredential`] and the write denied.
///
/// When the chat service refuses a credential it gave (a 401: a token revoked before it expires), the read asks it
/// once for a renewed one ([`Self::credential_rejected`]) and reads once more with it; a second refusal, or no renewed
/// credential, leaves the read [`PartialReason::FetchFailed`] and the write denied. So one check reads at most twice.
///
/// Any type may implement it: a credential decides only which token the read is sent with, never where it goes (the
/// base is checked as [`ChatCredential::bearer`] checks it) or what the answer must hold.
pub trait CredentialSource: Send + Sync {
    /// The credential to read with now.
    fn credential(&self) -> CredentialFuture<'_>;

    /// A renewed credential after the chat service refused `rejected` (one this source gave), or `None` when there is
    /// none to give. By default there is none, and the read stays refused.
    fn credential_rejected<'a>(&'a self, rejected: &'a ChatCredential) -> CredentialFuture<'a> {
        let _ = rejected;
        Box::pin(async { None })
    }
}

/// What a live source reads with.
#[derive(Clone)]
enum Credential {
    None,
    /// One credential, set by the caller ([`LiveMembershipSource::set_credential`]).
    Fixed(ChatCredential),
    /// A source asked on every read ([`LiveMembershipSource::set_credential_source`]).
    Source(Arc<dyn CredentialSource>),
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("None"),
            Self::Fixed(credential) => credential.fmt(f),
            Self::Source(_) => f.write_str("Source"),
        }
    }
}

/// Why the live source couldn't be built.
#[derive(Debug, thiserror::Error)]
#[error("can't build the membership source's HTTPS client: {0}")]
pub struct LiveSourceError(#[source] reqwest::Error);

/// The membership source of a lab run that writes: it reads a scope's members from the chat service on every check,
/// with the lab account's [`ChatCredential`] (see the module docs). Until a credential or a [`CredentialSource`] is
/// set every scope is [`PartialReason::NoCredential`]. A lab run's source is [`Self::for_fence`]; it and
/// [`Self::new`] read the same scopes: 1:1, group and meeting chats.
pub struct LiveMembershipSource {
    client: reqwest::Client,
    credential: Mutex<Credential>,
    /// How long a group or meeting chat's whole read may take, its credential and renewal included ([`FETCH_TIMEOUT`];
    /// shorter in tests).
    deadline: Duration,
    /// Tests only: every read goes to this origin instead of the credential's host.
    #[cfg(feature = "test-support")]
    mock_origin: Option<String>,
}

impl fmt::Debug for LiveMembershipSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveMembershipSource")
            .field("credential", &self.current())
            .finish_non_exhaustive()
    }
}

impl LiveMembershipSource {
    /// The source, with no credential yet. A lab run that writes uses [`Self::for_fence`].
    ///
    /// # Errors
    /// When the HTTPS client can't be built.
    pub fn new() -> Result<Self, LiveSourceError> {
        Self::build(true)
    }

    /// The source of a lab run that writes through `fence` ([`Fence::open_lab`]), with no credential yet. It reads
    /// what [`Self::new`]'s does: which members the fence admits is the fence's roster's to say, not the source's.
    ///
    /// # Errors
    /// When the HTTPS client can't be built.
    pub fn for_fence(_fence: &Fence) -> Result<Self, LiveSourceError> {
        Self::build(true)
    }

    fn build(https_only: bool) -> Result<Self, LiveSourceError> {
        Ok(Self {
            client: client(https_only)?,
            credential: Mutex::new(Credential::None),
            deadline: FETCH_TIMEOUT,
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
            mock_origin: Some(origin.trim_end_matches('/').to_owned()),
            ..Self::build(false)?
        })
    }

    /// This source with a group or meeting chat's whole read bounded by `deadline` instead of [`FETCH_TIMEOUT`]. Tests only
    /// (`test-support`).
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn with_deadline(self, deadline: Duration) -> Self {
        Self { deadline, ..self }
    }

    /// Uses `credential` for every read from now on, in place of any credential or source set before (the newest one
    /// wins).
    pub fn set_credential(&self, credential: ChatCredential) {
        *self.credential.lock().unwrap_or_else(PoisonError::into_inner) = Credential::Fixed(credential);
    }

    /// Asks `source` for the credential on every read from now on, in place of any credential or source set before:
    /// a native client's own token, renewed as its session renews it.
    pub fn set_credential_source(&self, source: Arc<dyn CredentialSource>) {
        *self.credential.lock().unwrap_or_else(PoisonError::into_inner) = Credential::Source(source);
    }

    /// Whether a credential or a credential source has been set.
    #[must_use]
    pub fn has_credential(&self) -> bool {
        !matches!(self.current(), Credential::None)
    }

    fn current(&self) -> Credential {
        self.credential.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The credential to read with now: the one set, or the source's answer.
    async fn credential(current: &Credential) -> Option<ChatCredential> {
        match current {
            Credential::None => None,
            Credential::Fixed(credential) => Some(credential.clone()),
            Credential::Source(source) => source.credential().await,
        }
    }

    /// The URL of a thread's `read`: `origin` (the base's, or the mock's), the base's path to `v1` (`prefix`), then
    /// `threads/<id>` (and `members` for [`ThreadRead::Members`]), the id one encoded segment, and the fixed query.
    fn thread_url(origin: &str, prefix: Vec<&str>, id: &str, read: ThreadRead) -> Option<Url> {
        let mut url = Url::parse(origin).ok()?;
        {
            let mut segments = url.path_segments_mut().ok()?;
            segments.clear().extend(prefix).extend(["v1", "threads", id]);
            if read == ThreadRead::Members {
                segments.push("members");
            }
        }
        if read == ThreadRead::Members {
            url.query_pairs_mut()
                .append_pair("view", "msnp24Equivalent")
                .append_pair("pageSize", MEMBERS_PAGE_SIZE);
        } else {
            url.set_query(Some("view=msnp24Equivalent"));
        }
        Some(url)
    }

    async fn one_to_one(&self, id: &str, pair: [Mri; 2]) -> Membership {
        let body = match self.read_renewing(id, ThreadRead::Thread).await {
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

    /// A group or meeting chat's membership: its member list, read once within this source's deadline
    /// ([`FETCH_TIMEOUT`]: the credential, a renewal, the read and its body), judged by [`parse_members`].
    async fn group(&self, id: &str) -> Membership {
        let read = async {
            match self.read_renewing(id, ThreadRead::Members).await {
                Ok(body) => serde_json::from_slice::<Value>(&body)
                    .map_or(Membership::Partial(MALFORMED), |members| parse_members(&members)),
                Err(reason) => Membership::Partial(reason),
            }
        };
        tokio::time::timeout(self.deadline, read)
            .await
            .unwrap_or(Membership::Partial(PartialReason::FetchFailed(FetchFailure::Timeout)))
    }

    /// The thread `id`'s `read` with the credential this source has now; when the service refuses it, once more with
    /// the renewed credential its source gives, if any. No credential is [`PartialReason::NoCredential`], nothing sent.
    async fn read_renewing(&self, id: &str, read: ThreadRead) -> Result<Vec<u8>, PartialReason> {
        let current = self.current();
        let credential = Self::credential(&current).await.ok_or(PartialReason::NoCredential)?;
        match self.read_thread(id, &credential, read).await {
            Err(REFUSED) => match Self::renewed(&current, &credential).await {
                Some(renewed) => self.read_thread(id, &renewed, read).await,
                None => Err(REFUSED),
            },
            read => read,
        }
    }

    /// The thread `id`'s `read`, with `credential`.
    async fn read_thread(
        &self,
        id: &str,
        credential: &ChatCredential,
        read: ThreadRead,
    ) -> Result<Vec<u8>, PartialReason> {
        let (origin, prefix) = credential.base.origin_and_prefix();
        #[cfg(feature = "test-support")]
        let origin = self.mock_origin.clone().unwrap_or(origin);
        let url =
            Self::thread_url(&origin, prefix, id, read).ok_or(PartialReason::FetchFailed(FetchFailure::Malformed))?;
        self.fetch(url, credential, read).await
    }

    /// The renewed credential `current`'s source gives after the service refused `rejected`; `None` when `current` is
    /// a fixed credential (a page's, which can't be renewed here) or the source has no renewed one.
    async fn renewed(current: &Credential, rejected: &ChatCredential) -> Option<ChatCredential> {
        match current {
            Credential::Source(source) => source.credential_rejected(rejected).await,
            Credential::None | Credential::Fixed(_) => None,
        }
    }

    /// One GET with the credential's token, its body read up to [`MAX_BODY`]; the client's timeout covers the whole
    /// read. The thread read goes with the token alone, the form the fence's read was answered in live; the member
    /// list ([`ThreadRead::Members`]) with the web client's `X-MS-Migration: True` and `BehaviorOverride: redirectAs404`
    /// beside it, the only form it was read in live (`proto-lab probe thread`).
    async fn fetch(&self, url: Url, credential: &ChatCredential, read: ThreadRead) -> Result<Vec<u8>, PartialReason> {
        let (name, value) = credential.header();
        let mut request = self
            .client
            .get(url)
            .header(name, value.as_str())
            .header("Accept", "application/json");
        if read == ThreadRead::Members {
            request = request
                .header("X-MS-Migration", "True")
                .header("BehaviorOverride", "redirectAs404");
        }
        let mut response = request.send().await.map_err(|error| failure(&error))?;
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

impl LiveMembershipSource {
    /// Whether this source reads `scope`'s membership at all: a 1:1 chat's ([`one_to_one_pair`]), a group chat's
    /// ([`is_group_chat_id`]) or a meeting chat's ([`is_meeting_chat_id`]). Every other scope is
    /// [`PartialReason::Unsupported`], answered without a read. It is eligibility, not a verdict: whether the read is
    /// complete and its members all in the lab is decided on every write. A caller can tell before a write that it
    /// will be denied.
    #[must_use]
    pub fn reads_in_full(&self, scope: &Scope) -> bool {
        let Scope::Conversation { id } = scope else {
            return false;
        };
        is_chat_id(id)
    }
}

impl MembershipSource for LiveMembershipSource {
    fn members<'a>(&'a self, scope: &'a Scope) -> MembershipFuture<'a> {
        Box::pin(async move {
            match scope {
                Scope::Conversation { id } => match one_to_one_pair(id) {
                    Some(pair) => self.one_to_one(id, pair).await,
                    None if is_group_chat_id(id) || is_meeting_chat_id(id) => self.group(id).await,
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

/// Whether `id` is a chat's conversation id in a form the fence knows: a 1:1 chat's ([`one_to_one_pair`]), a group
/// chat's ([`is_group_chat_id`]) or a meeting chat's ([`is_meeting_chat_id`]). It is the one chat-id rule for typing
/// and read receipts: [`crate::contacting`] carves them out only on such an id, and [`crate::Fence::check`] permits
/// them without a member read only there. Any other id, a looser `19:…@unq.gbl.spaces` or `19:…@thread.v2` included,
/// is none of these.
#[must_use]
pub fn is_chat_id(id: &str) -> bool {
    one_to_one_pair(id).is_some() || is_group_chat_id(id) || is_meeting_chat_id(id)
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

/// The 1:1 chat id of two work accounts, as the web client builds it: `19:<oid>_<oid>@unq.gbl.spaces`, the two object
/// ids (lowercase) sorted; [`one_to_one_pair`] reads it back. `None` unless both MRIs are `8:orgid:<oid>` (a GUID) of
/// two different accounts.
#[must_use]
pub fn one_to_one_id(mri_a: &str, mri_b: &str) -> Option<String> {
    let oid = |mri: &str| {
        let oid = mri.strip_prefix("8:orgid:")?;
        is_guid(oid).then(|| oid.to_ascii_lowercase())
    };
    let mut oids = [oid(mri_a)?, oid(mri_b)?];
    oids.sort();
    let [first, second] = oids;
    (first != second).then(|| format!("19:{first}_{second}{ONE_TO_ONE_SUFFIX}"))
}

/// The suffix of a group (or meeting) chat's conversation id.
const GROUP_SUFFIX: &str = "@thread.v2";

/// Whether `id` is a group chat's conversation id: `19:<id>@thread.v2`, the id 1–128 ASCII letters, digits, `_` or
/// `-`, and not a meeting's chat (`19:meeting_…`, [`is_meeting_chat_id`]).
#[must_use]
pub fn is_group_chat_id(id: &str) -> bool {
    thread_v2(id).is_some_and(|inner| !has_meeting_prefix(inner))
}

/// Whether `id` is a meeting chat's conversation id: `19:meeting_<id>@thread.v2` (`meeting_` in any case), the whole
/// id 1–128 ASCII letters, digits, `_` or `-`, with something after `meeting_`. No id is both this and a group chat's
/// ([`is_group_chat_id`]).
#[must_use]
pub fn is_meeting_chat_id(id: &str) -> bool {
    thread_v2(id).is_some_and(|inner| has_meeting_prefix(inner) && inner.len() > MEETING_PREFIX.len())
}

/// The prefix of a meeting chat's inner id.
const MEETING_PREFIX: &str = "meeting_";

/// The id inside `19:<id>@thread.v2`, when it is 1–128 ASCII letters, digits, `_` or `-`.
fn thread_v2(id: &str) -> Option<&str> {
    let inner = id.strip_prefix("19:")?.strip_suffix(GROUP_SUFFIX)?;
    ((1..=128).contains(&inner.len())
        && inner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
    .then_some(inner)
}

/// Whether a `@thread.v2` chat's inner id begins with `meeting_`, in any case.
fn has_meeting_prefix(inner: &str) -> bool {
    inner
        .get(..MEETING_PREFIX.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(MEETING_PREFIX))
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
    if !counts_match(object, members.len()) {
        return Membership::Partial(PartialReason::Truncated);
    }
    Membership::Complete(members)
}

/// Whether every member count of `object` (`memberCount`, `membercount`, `membersCount`, at the top level or under
/// `properties`, a number or a numeric string) is `listed`; none at all is a match.
fn counts_match(object: &serde_json::Map<String, Value>, listed: usize) -> bool {
    let properties = object.get("properties").and_then(Value::as_object);
    object
        .iter()
        .chain(properties.into_iter().flat_map(|map| map.iter()))
        .filter(|(key, _)| matches!(key.to_ascii_lowercase().as_str(), "membercount" | "memberscount"))
        .all(|(_, count)| {
            let count = count
                .as_u64()
                .or_else(|| count.as_str().and_then(|text| text.trim().parse().ok()));
            count == u64::try_from(listed).ok()
        })
}

const MALFORMED: PartialReason = PartialReason::FetchFailed(FetchFailure::Malformed);

/// Reads a group or meeting chat's member list (`GET v1/threads/<id>/members?view=msnp24Equivalent&pageSize=100`) as
/// its membership. [`Membership::Complete`] with the listed members only when all of this holds, in the shape the
/// service gave live (`tests/fixtures/chatsvc/thread-group-members.json`):
/// - no pagination key (as [`parse_thread`] reads one) at any depth → else [`PartialReason::Paginated`];
/// - an object with `isDeleted` `false` and a `members` array;
/// - its `totalMemberCount` an integer equal to the members it lists, and any other member count agreeing → else
///   [`PartialReason::Truncated`];
/// - every member says `hidden` `false`, says `isDeleted` `false` or nothing about it, and has an `id` that is an MRI
///   in a form the reads showed: a work account's `8:orgid:<guid>` or a bot's `28:<guid>`;
/// - nobody is listed twice, case folded.
///
/// Any other shape is malformed. Whether the members are lab accounts or approved services is the fence's to judge.
#[must_use]
pub fn parse_members(members: &Value) -> Membership {
    match judge_members(members) {
        Ok(listed) => Membership::Complete(listed),
        Err(reason) => Membership::Partial(reason),
    }
}

fn judge_members(answer: &Value) -> Result<Vec<Mri>, PartialReason> {
    if has_page_marker(answer) {
        return Err(PartialReason::Paginated);
    }
    let object = answer.as_object().ok_or(MALFORMED)?;
    let listed = object.get("members").and_then(Value::as_array).ok_or(MALFORMED)?;
    if object.get("isDeleted") != Some(&Value::Bool(false)) {
        return Err(MALFORMED);
    }
    let total = object
        .get("totalMemberCount")
        .and_then(Value::as_u64)
        .ok_or(MALFORMED)?;
    if Some(total) != u64::try_from(listed.len()).ok() || !counts_match(object, listed.len()) {
        return Err(PartialReason::Truncated);
    }
    let members = listed
        .iter()
        .map(member)
        .collect::<Option<Vec<Mri>>>()
        .ok_or(MALFORMED)?;
    let mut folded: Vec<String> = members.iter().map(|mri| mri.as_str().to_ascii_lowercase()).collect();
    folded.sort_unstable();
    folded.dedup();
    (folded.len() == members.len()).then_some(members).ok_or(MALFORMED)
}

/// A listed member's MRI, when the entry says `hidden` `false`, isn't deleted, and has an MRI of a form the reads
/// showed.
fn member(entry: &Value) -> Option<Mri> {
    let shown = entry.get("hidden") == Some(&Value::Bool(false))
        && entry
            .get("isDeleted")
            .is_none_or(|deleted| *deleted == Value::Bool(false));
    let id = entry.get("id")?.as_str()?;
    let known = id
        .strip_prefix("8:orgid:")
        .or_else(|| id.strip_prefix("28:"))
        .is_some_and(is_guid);
    (shown && known).then(|| Mri::new(id))
}

/// Whether any object key at any depth of `value` looks like pagination ([`is_page_marker`]).
fn has_page_marker(value: &Value) -> bool {
    match value {
        Value::Object(map) => map
            .iter()
            .any(|(key, inner)| is_page_marker(key) || has_page_marker(inner)),
        Value::Array(items) => items.iter().any(has_page_marker),
        _ => false,
    }
}

fn is_page_marker(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    ["next", "continuation", "skiptoken", "syncstate"]
        .iter()
        .any(|marker| key.contains(marker))
}
