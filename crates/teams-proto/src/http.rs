//! The native HTTP transport: the one way the native client sends anything to Teams over HTTP.
//!
//! [`HttpTransport`] takes the three classes of request [`Transport`] defines and nothing else: a [`Read`] (always a
//! `GET`), a plain [`Request`] (any method, sent only when [`teams_fence::contacting`] says it reaches nobody, logged to
//! `fence.log` as `allowed` before it goes; one that can't be logged isn't sent; a write or a request the classifier
//! can't read is refused before anything is sent, but through the personal build's fence, which denies nothing: there
//! it goes too, logged with what the classifier made of it) and an [`Authorized`] write (its sealed wire form, under the permit
//! `Fence::check` issued for its target). It has no method that takes a URL or a host of the caller's choosing: a call
//! write's link ([`crate::CallLink`]) is one the call's services handed out, parsed and kept by the sealed write and
//! checked against the calling base again before it goes. Beside them it keeps the personal build's sealed lists: an
//! [`OwnState`] request ([`HttpTransport::own_state`]) and an [`AuthorizedPostRead`] ([`HttpTransport::post_read`],
//! read back against its entry's captured shape and the permit's MRIs, sent once), each also asked of the classifier
//! and logged by its entry's kind before it goes.
//!
//! - **Hosts**: each [`Service`] has one base, and every URL is built from it and checked before anything is sent:
//!   `https`, the base's host, no user info, port or fragment, the decoded path exactly the base's and the endpoint's
//!   segments, the query exactly the endpoint's. A call write is the one exception: its URL isn't built from an
//!   endpoint but is the sealed write's own (the call start's fixed `…/epconv`, or a [`crate::CallLink`] whose routing
//!   query is kept verbatim), and it is checked before anything is sent instead (`https`, the flight proxy's host, no
//!   user info, port or fragment, the path `/api/v2/epconv` with no query or `/api/v2/ep/<host>/…` with no segment
//!   empty, `.` or `..`). The chat service at the account's chat base, exactly as
//!   [`teams_fence::chat_service_path`] accepts it (the region's `chatServiceAfd`, endpoints under `v1`); the registrar
//!   on an AFD host (`https://<AFD host>/registrar/…`); Trouter's HTTP side (`https://go.trouter.teams.microsoft.com/`);
//!   Microsoft Graph (`https://graph.microsoft.com/v1.0/…`); and, once the transport was given them, the middle tier
//!   ([`HttpTransport::with_middle_tier`], the GTM `middleTier`: `https://<AFD host>/api/mt/part/<partition>`), CSA
//!   ([`HttpTransport::with_csa`], the GTM `chatSvcAggAfd`: `https://<AFD host>/api/csa/<region>`) and UPS
//!   ([`HttpTransport::with_presence`], the GTM `presenceUPS`: `https://<AFD host>/ups/<geo>`, every path with the
//!   trailing slash every captured one had); the calling service ([`Service::Calling`], the flight proxy:
//!   `https://api.flightproxy.teams.microsoft.com/api/v2/`, authorized call writes only, a read or a plain request on
//!   it refused); and the TURN relay's credentials ([`Service::Relay`]: `https://teams.microsoft.com/trap`, the
//!   relay read); and Teams' reaction pictures on its CDN ([`Service::Emoticons`]:
//!   `https://statics.teams.cdn.office.net/evergreen-assets/personal-expressions/v2/assets/emoticons`, no token); and,
//!   once the transport was given its base, AMS's async gateway ([`HttpTransport::with_ams`], the GTM `amsV2`:
//!   `https://<name>.asyncgw.teams.microsoft.com`, reads of a pasted picture's view only); and a file thumbnail's
//!   picture ([`Service::FileThumbnail`], no base of its own: only [`HttpTransport::file_thumbnail`] reads there, a
//!   link Graph handed out on SharePoint's media host or a SharePoint host, checked again, with no token); and Giphy's
//!   media CDN ([`Service::Giphy`]: `https://media.giphy.com/`, a GIF a message shows, no token). An endpoint with an
//!   empty,
//!   `.` or `..` segment is refused. Entra and authz aren't a service here (they stay in `teams-auth`'s own clients). A
//!   new service is a code change: a variant, its base and its token's audience.
//! - **The client**: `https` only, no proxy (an environment's `HTTPS_PROXY` could point anywhere), no redirects (a 3xx
//!   is an answer, never followed: a region hop is the caller's to report), no retries of its own (reqwest's default
//!   replays a request after an HTTP/2 GOAWAY or `REFUSED_STREAM`, which would send a write again and a plain request
//!   unlogged), [`CONNECT_TIMEOUT`] and [`REQUEST_TIMEOUT`], bodies read up to [`MAX_BODY`] (a
//!   picture's from AMS or a GIF from Giphy up to [`MAX_PICTURE_BODY`]).
//! - **Credentials** come from the account's [`Session`] ([`Credentials`]), per service, each token only to its own
//!   service's host: the chat service takes `Authorization: Bearer <ic3>` with `X-MS-Migration: True` and
//!   `BehaviorOverride: redirectAs404` (the web client's set, which the service took with our own non-CAE token); the
//!   registrar the same token with `X-MS-Migration: True` and `X-Ms-Test-User: False` (the messaging stack's
//!   registration as captured; the calling stack's had no `X-Ms-Test-User`); Trouter's HTTP side none (its probe carried
//!   none); the middle tier the account's `api.spaces.skype.com` token with the values every captured profile lookup
//!   carried (`x-ms-migration: True`, `x-ms-test-user: False`, `x-ms-client-type: cdlworker`,
//!   `x-ms-client-request-type: 0`, an empty `x-ms-object-id`) and its body as `application/json;charset=UTF-8`; Graph
//!   the account's `https://graph.microsoft.com` token and nothing else (the probe's form); CSA the account's
//!   `https://chatsvcagg.teams.microsoft.com` token and nothing else (the probe's form); UPS the account's
//!   `https://presence.teams.microsoft.com/` token with the values every captured presence read carried
//!   (`behavioroverride: redirectAs404`, `x-ms-client-type: cdlworker`, `x-ms-client-user-agent: Teams-V2-Web`, an empty
//!   `x-ms-object-id`) and its body as `application/json` (an own-availability write adds its `x-ms-endpoint-id`, and
//!   sends `Content-Type` even without a body, as captured); the calling service the account's skypetoken in
//!   `X-Skypetoken` (no bearer token; [`Credentials::skypetoken`], a call write refused without one) and the call's own
//!   headers as its sealed write carries them; the relay read the `ic3` token with `api-version: 2` and
//!   `x-ms-migration: True`; AMS the `ic3` token with `x-ms-migration: True` (the object read's form) and an image's
//!   `Accept`.
//! - **Retries** (the transport's part of the retry table), for reads, plain requests and own-state requests: a 401
//!   makes the session refresh the refused token, a 429 or 503 waits its `Retry-After` (capped at the transport's
//!   maximum, [`MAX_RETRY_WAIT`] by default; [`DEFAULT_RETRY_WAIT`] without one), and either is sent once more; that is
//!   the only retry. The retry after a wait asks the session for its token again, never sends the one held before it: a
//!   session signed out during the wait answers its error, and nothing is sent. Any other answer that isn't a success
//!   is [`HttpError::Status`]. A write is never sent again here: its answer comes back whatever the status, for the
//!   outbox to decide, and after a 401 the session refreshes the refused token for the outbox's next attempt. Nor is a
//!   read by `POST` of the sealed list: one permit, one send; its answer comes back as a read's does (a failure status
//!   as [`HttpError::Status`]), and after a 401 the session refreshes the refused token for a later request under a new
//!   permit.
//! - **Tests** reach a mock only through [`HttpTransport::with_mock_origin`] (`test-support`): plain `http` to a
//!   loopback origin, the URL built, checked and classified as above first.
//!
//! **The log** (`teams-log`): every request at `info` by its method, host and path (never its query), with its status
//! and latency, or how it failed, and the retry decision; at `debug` the request and its answer in full, as
//! `teams_log::wire` records whose formatting redacts the credentials (the `Authorization` and `X-Skypetoken` headers
//! among them, and a call's SDP ICE credentials): redaction is the logger's, never this module's. Otherwise nothing
//! here shows a token: the `Authorization` and `X-Skypetoken` headers are marked sensitive, errors carry no token or
//! body, and [`Response`]'s `Debug` shows the body's length only.

use std::fmt;
use std::future::Future;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use percent_encoding::percent_decode_str;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, LOCATION};
use serde_json::Value;
use teams_auth::claims::TokenSummary;
use teams_auth::secrets::{Secret, SecretStore};
use teams_auth::session::Session;
use teams_auth::token::{SPACES_RESOURCE, TokenError, retry_after};
use teams_fence::{
    AFD_HOSTS, AllowedRecord, Channel, ChatCredential, Contact, CredentialFuture, CredentialSource, Fence, Mri,
    PendingAnswer, PendingStart, Policy, Request as FenceRequest, RequestOutcome, RequestRecord, WritePermit,
    chat_service_path, contacting,
};
use url::Url;

use crate::client::{Response, Transport};
use crate::endpoint::{Endpoint, Service};
use crate::own::OwnState;
use crate::post_read::{AuthorizedPostRead, PostRead};
use crate::read::Read;
use crate::request::Request;
use crate::thumbnail::{ThumbnailLink, is_thumbnail_link};
use crate::write::{Authorized, CallWire, Mutating, Registry, on_calling_base};

/// The Entra audience of the token the chat service and the registrar take.
pub const IC3_AUDIENCE: &str = "https://ic3.teams.office.com";

/// The Entra audience of the token the middle tier takes.
pub const MIDDLE_TIER_AUDIENCE: &str = SPACES_RESOURCE;

/// The Entra audience of the token Microsoft Graph takes.
pub const GRAPH_AUDIENCE: &str = "https://graph.microsoft.com";

/// The Entra audience of the token CSA takes.
pub const CSA_AUDIENCE: &str = "https://chatsvcagg.teams.microsoft.com";

/// The Entra audience of the token UPS (presence) takes: the presence origin with its slash, as the web client asks for
/// it (`docs/protocol/auth.md`, question 4).
pub const PRESENCE_AUDIENCE: &str = "https://presence.teams.microsoft.com/";

/// Microsoft Graph's host.
pub const GRAPH_HOST: &str = "graph.microsoft.com";

/// Trouter's HTTP host.
pub const TROUTER_HOST: &str = "go.trouter.teams.microsoft.com";

/// The largest body read, in bytes; a longer one is [`HttpError::BodyTooLarge`].
pub const MAX_BODY: usize = 4 * 1024 * 1024;

/// The largest body read from AMS ([`Service::Ams`]) or Giphy ([`Service::Giphy`]), in bytes: a picture pasted into a
/// message, a photo's original included, or a GIF at its full size; a longer one is [`HttpError::BodyTooLarge`].
pub const MAX_PICTURE_BODY: usize = 24 * 1024 * 1024;

/// How long connecting may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long one request may take, end to end.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The longest a throttled read, plain or own-state request waits before it is sent again, by default.
pub const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);

/// The wait before a throttled request is sent again when the answer gives no `Retry-After` (capped like any other).
pub const DEFAULT_RETRY_WAIT: Duration = Duration::from_secs(1);

/// The account's tokens, as the transport asks for them: a bearer token for an audience, and a new one after a service
/// refused one. The account's [`Session`] is the one a client uses; any type may implement it (a fake in tests), since
/// a token decides nothing about where a request goes or what it writes.
pub trait Credentials: Send + Sync {
    /// A bearer token for `audience`.
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send;

    /// A new bearer token for `audience` after a service refused `rejected` (a 401).
    fn bearer_rejected(
        &self,
        audience: &str,
        rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send;

    /// The account's skypetoken, which the calling service takes (`X-Skypetoken`). `Ok(None)` (the default) for a
    /// source that has none: the transport then refuses a call write ([`Refusal::NoSkypetoken`]) and sends nothing.
    fn skypetoken(&self) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        async { Ok(None) }
    }

    /// A new skypetoken after the calling service refused `rejected` (a 401), for a later request; `Ok(None)` (the
    /// default) for a source that has none.
    fn skypetoken_rejected(
        &self,
        rejected: &Secret,
    ) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        let _ = rejected;
        async { Ok(None) }
    }
}

impl<S: SecretStore> Credentials for Session<S> {
    async fn bearer(&self, audience: &str) -> Result<Secret, TokenError> {
        Ok(self.access_token(audience).await?.token)
    }

    async fn bearer_rejected(&self, audience: &str, rejected: &Secret) -> Result<Secret, TokenError> {
        Ok(self.refresh_rejected(audience, rejected).await?.token)
    }

    async fn skypetoken(&self) -> Result<Option<Secret>, TokenError> {
        Ok(Some(Session::skypetoken(self).await?.token))
    }

    async fn skypetoken_rejected(&self, rejected: &Secret) -> Result<Option<Secret>, TokenError> {
        Ok(Some(Session::skypetoken_rejected(self, rejected).await?.token))
    }
}

/// Why a request didn't produce an answer. No variant carries a token or a body.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// Refused before anything was sent.
    #[error("refused before sending: {0}")]
    Refused(Refusal),
    /// The write has no wire form the native transport has a route for (a call); nothing was sent.
    #[error("the native transport has no route for this write")]
    NoRoute,
    /// A plain request, an own-state request or a read by `POST` whose `fence.log` entry couldn't be written; it wasn't
    /// sent.
    #[error("fence.log can't be written, so the request wasn't sent: {0}")]
    LogUnavailable(#[source] io::Error),
    /// A read by `POST` whose request isn't exactly its entry's captured shape, or names other MRIs than its permit;
    /// nothing was sent, logged or asked of the session.
    #[error("the read by POST isn't its entry's captured shape; nothing was sent")]
    Unknown,
    /// The session couldn't give a token; nothing (more) was sent.
    #[error("no token for the request: {0}")]
    Credentials(#[source] TokenError),
    /// The request or its answer failed on the way.
    #[error("the request failed: {0}")]
    Failed(Failure),
    /// The answer's body is longer than [`MAX_BODY`] (AMS's: [`MAX_PICTURE_BODY`]).
    #[error("the answer's body is over the largest read")]
    BodyTooLarge,
    /// A read, plain or own-state request answered with a status other than a success, after the retry the status
    /// allows.
    #[error("the service answered HTTP {}", .0.status())]
    Status(Box<Response>),
}

/// Why a request was refused before sending.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// An endpoint with an empty, `.` or `..` segment.
    #[error("an empty, `.` or `..` path segment")]
    Segment,
    /// A chat service endpoint that doesn't start with `v1`.
    #[error("a chat service endpoint that doesn't start with `v1`")]
    ChatPath,
    /// A middle-tier request on a transport that wasn't given the account's middle-tier base.
    #[error("no middle-tier base")]
    NoMiddleTier,
    /// A CSA request on a transport that wasn't given the account's CSA base.
    #[error("no CSA base")]
    NoCsa,
    /// A presence request on a transport that wasn't given the account's presence base.
    #[error("no presence base")]
    NoPresence,
    /// An AMS read on a transport that wasn't given the account's AMS base.
    #[error("no AMS base")]
    NoAms,
    /// A plain request [`teams_fence::contacting`] calls a write, through a lab fence: one goes out only as an
    /// [`Authorized`] write, under the permit for its target.
    #[error("a write sent as a plain request: a write goes out only under its permit")]
    Write,
    /// A plain request [`teams_fence::contacting`] can't read, through a lab fence: a mutating request on a contacting
    /// service that no row maps or carves out.
    #[error("a mutating request on a contacting service the fence can't read")]
    Unreadable,
    /// The URL built isn't on its service's base: another host, scheme or port, or a path or query other than the
    /// base's and the endpoint's exactly.
    #[error("the URL isn't on its service's base")]
    NotAllowed,
    /// A token that can't be sent as a header value.
    #[error("a token that can't be sent in a header")]
    Token,
    /// A read by `POST` the fence denied (a personal fence only when it can't log it).
    #[error("the fence denied the read by POST")]
    PostReadDenied,
    /// A read or a plain request to the calling service: only authorized call writes go there.
    #[error("only authorized call writes go to the calling service")]
    Calling,
    /// A read or a plain request to a file thumbnail's host: only a link Graph handed out is read there
    /// ([`HttpTransport::file_thumbnail`]).
    #[error("only a thumbnail link Graph handed out is read from a thumbnail's host")]
    FileThumbnail,
    /// A call write while the credentials give no skypetoken.
    #[error("no skypetoken for the calling service")]
    NoSkypetoken,
    /// A call start, a join with media, an `attach` or an `accept` the fence's call registry wouldn't take (its permit
    /// taken already, or another fence's): sent, it couldn't register its call or bind its answer's links.
    #[error("the fence's call registry wouldn't take the call write's permit")]
    NotPended,
    /// A write, an own-state request or a mutating plain request on a read-only transport
    /// ([`HttpTransport::read_only`], the personal build's read-only mode): logged as `refused_read_only`, never sent.
    #[error("read-only mode: nothing that writes is sent")]
    ReadOnly,
}

/// How a request failed on the way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    #[error("network")]
    Network,
    #[error("timed out")]
    Timeout,
    /// The answer's body couldn't be read.
    #[error("the body couldn't be read")]
    Body,
}

/// Why a transport couldn't be built.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// Not a chat service base the transport accepts (see [`HttpTransport::new`]).
    #[error("not a chat service base the transport accepts")]
    ChatBase,
    /// Not a middle-tier base the transport accepts (see [`HttpTransport::with_middle_tier`]).
    #[error("not a middle-tier base the transport accepts")]
    MiddleTierBase,
    /// Not a CSA base the transport accepts (see [`HttpTransport::with_csa`]).
    #[error("not a CSA base the transport accepts")]
    CsaBase,
    /// Not a presence base the transport accepts (see [`HttpTransport::with_presence`]).
    #[error("not a presence base the transport accepts")]
    PresenceBase,
    /// Not an AMS base the transport accepts (see [`HttpTransport::with_ams`]).
    #[error("not an AMS base the transport accepts")]
    AmsBase,
    /// Not a loopback `http` origin with a port.
    #[error("not a loopback http origin")]
    MockOrigin,
    #[error("can't build the HTTPS client: {0}")]
    Client(#[source] reqwest::Error),
}

/// The native HTTP transport (see the module docs).
pub struct HttpTransport<C> {
    client: reqwest::Client,
    credentials: Arc<C>,
    fence: Arc<Fence>,
    /// The chat base: an AFD base (`https://<AFD host>/api/chatsvc/<region>`) without a trailing slash, a regional
    /// base as its origin (`https://<host>/`, the slash `Url` keeps for an empty path).
    chat_base: Url,
    /// The AFD host the registrar is reached on.
    registrar_host: &'static str,
    /// The middle-tier base (`https://<AFD host>/api/mt/part/<partition>`, no trailing slash), once given.
    middle_tier: Option<Url>,
    /// The CSA base (`https://<AFD host>/api/csa/<region>`, no trailing slash), once given.
    csa: Option<Url>,
    /// The presence base (`https://<AFD host>/ups/<geo>`, no trailing slash), once given.
    presence: Option<Url>,
    /// The AMS base (`https://<name>.asyncgw.teams.microsoft.com`, its path empty), once given.
    ams: Option<Url>,
    max_retry_wait: Duration,
    /// What the classifier needs beyond a request: the fence's own account, which a chat service path may name in
    /// place of `ME`.
    policy: Policy,
    /// Tests only: every request goes to this origin instead, with the path and query it was built and checked with.
    /// Only [`HttpTransport::with_mock_origin`] (`test-support`) sets it; without that feature it is always `None`.
    mock_origin: Option<Url>,
    /// Read-only ([`HttpTransport::read_only`]): only reads (a `GET`, a read by `POST` of the sealed list) go out.
    read_only: bool,
}

impl<C> fmt::Debug for HttpTransport<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpTransport")
            .field("chat_base", &self.chat_base.as_str())
            .field("registrar_host", &self.registrar_host)
            .field("middle_tier", &self.middle_tier.as_ref().map(Url::as_str))
            .field("csa", &self.csa.as_ref().map(Url::as_str))
            .field("presence", &self.presence.as_ref().map(Url::as_str))
            .field("max_retry_wait", &self.max_retry_wait)
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

/// One request, built and checked.
struct Outgoing<'a> {
    method: reqwest::Method,
    url: &'a Url,
    service: Service,
    body: Option<&'a [u8]>,
}

/// What goes to `fence.log` before each send of a request.
#[derive(Clone, Copy)]
enum Record {
    /// Nothing: a read.
    Nothing,
    /// A plain request, let through because it reaches nobody.
    Allowed,
    /// An entry of the sealed own-state list, by its kind.
    Own(&'static str),
    /// A plain request the personal build sends although the classifier calls it a write (`write`) or can't read it
    /// (`unreadable`).
    Unjudged(&'static str),
}

impl<C: Credentials> HttpTransport<C> {
    /// A transport for the account whose tokens `credentials` gives, logging the requests it lets through to `fence`'s
    /// log and reading a chat service path naming `fence`'s own account as `ME`, with
    /// `chat_base` as the chat service's base: the region's GTM `chatServiceAfd` (trimmed), one trailing slash
    /// allowed, and only a base [`teams_fence::chat_service_path`] accepts a request under, exactly (nothing before or
    /// after it, no query or fragment).
    ///
    /// # Errors
    /// [`BuildError::ChatBase`] for any other base; [`BuildError::Client`] when the HTTPS client can't be built.
    pub fn new(credentials: Arc<C>, fence: Arc<Fence>, chat_base: &str) -> Result<Self, BuildError> {
        Self::build(credentials, fence, chat_base, true)
    }

    /// A transport whose requests go to `origin` (`http://127.0.0.1:<port>`, a local mock) instead, each with the path
    /// and query it was built and checked with as [`Self::new`]'s. Tests only (`test-support`).
    ///
    /// # Errors
    /// [`BuildError::MockOrigin`] unless `origin` is `http` on a loopback host (`127.0.0.1`, `[::1]`, `localhost`) with
    /// a port and nothing after it; otherwise as [`Self::new`].
    #[cfg(feature = "test-support")]
    pub fn with_mock_origin(
        credentials: Arc<C>,
        fence: Arc<Fence>,
        chat_base: &str,
        origin: &str,
    ) -> Result<Self, BuildError> {
        let origin = parse_mock_origin(origin).ok_or(BuildError::MockOrigin)?;
        let mut transport = Self::build(credentials, fence, chat_base, false)?;
        transport.mock_origin = Some(origin);
        Ok(transport)
    }

    fn build(credentials: Arc<C>, fence: Arc<Fence>, chat_base: &str, https_only: bool) -> Result<Self, BuildError> {
        let chat_base = parse_chat_base(chat_base).ok_or(BuildError::ChatBase)?;
        let registrar_host = chat_base
            .host_str()
            .and_then(|host| AFD_HOSTS.iter().find(|afd| **afd == host))
            .copied()
            .unwrap_or(AFD_HOSTS[0]);
        let policy = fence
            .own_account()
            .cloned()
            .map_or_else(Policy::new, |account| Policy::new().with_own_account(account));
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            // the only retries are `exchange`'s and the outbox's (see the module docs)
            .retry(reqwest::retry::never())
            .https_only(https_only)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(BuildError::Client)?;
        Ok(Self {
            client,
            credentials,
            fence,
            chat_base,
            registrar_host,
            middle_tier: None,
            csa: None,
            presence: None,
            ams: None,
            max_retry_wait: MAX_RETRY_WAIT,
            policy,
            mock_origin: None,
            read_only: false,
        })
    }

    /// This transport, read-only for the rest of its life (the personal build's read-only mode, `teams --read-only`):
    /// it sends reads (a [`Read`], a read by `POST` of the sealed list) and nothing else. A write ([`Transport::deliver`],
    /// a call write included), an own-state request (a registration) and a plain request with any method but `GET` or
    /// `HEAD` are refused before anything is sent ([`Refusal::ReadOnly`]), each logged to the fence's log as
    /// `refused_read_only` (its method, host and path shape). Nothing turns it off again.
    #[must_use]
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    /// Whether this transport is read-only ([`Self::read_only`]).
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// The refusal of `method` to `url` in read-only mode, logged first (a line that can't be written is said in the
    /// app's log; the request is refused either way).
    fn refused_read_only(&self, method: &str, url: &str) -> HttpError {
        if let Err(error) = self
            .fence
            .log()
            .record_refused_read_only(&AllowedRecord { method, url })
        {
            log::warn!("a request refused in read-only mode couldn't be logged to fence.log: {error}");
        }
        log::info!("read-only: a {method} refused, nothing sent (fence.log has where it would have gone)");
        HttpError::Refused(Refusal::ReadOnly)
    }

    /// The refusal of the mutating request `write` in read-only mode, logged with where it would have gone.
    fn refused_write<R: Mutating>(&self, write: &R) -> HttpError {
        let (method, url) = match crate::write::wire_or_call(write) {
            Some(Err((method, url))) => (method.as_str(), url.to_string()),
            Some(Ok(wire)) => (
                wire.method().as_str(),
                self.url_for(wire.service(), wire.endpoint())
                    .map_or_else(|_| String::new(), |url| url.to_string()),
            ),
            None => ("", String::new()),
        };
        self.refused_read_only(method, &url)
    }

    /// Whether this transport sends through the personal build's fence ([`Fence::is_personal`]).
    #[must_use]
    pub fn is_personal(&self) -> bool {
        self.fence.is_personal()
    }

    /// The fence this transport logs to and asks.
    #[must_use]
    pub fn fence(&self) -> &Fence {
        &self.fence
    }

    /// The transport with the account's middle-tier base, the region's GTM `middleTier` (trimmed): `https`, an AFD host
    /// ([`teams_fence::AFD_HOSTS`]) with no port or user info, the path `/api/mt/part/<partition>` exactly (the
    /// partition a lowercase region, `-` and a number: `amer-03`), one trailing slash allowed, no query or fragment.
    /// Without it, every middle-tier request is refused ([`Refusal::NoMiddleTier`]).
    ///
    /// # Errors
    /// [`BuildError::MiddleTierBase`] for any other base.
    pub fn with_middle_tier(mut self, base: &str) -> Result<Self, BuildError> {
        self.middle_tier = Some(parse_middle_tier_base(base).ok_or(BuildError::MiddleTierBase)?);
        Ok(self)
    }

    /// The transport with the account's CSA base, the region's GTM `chatSvcAggAfd` (trimmed): `https`, an AFD host
    /// ([`teams_fence::AFD_HOSTS`]) with no port or user info, the path `/api/csa/<region>` exactly (the region
    /// lowercase letters: `amer`), one trailing slash allowed, no query or fragment. Without it, every CSA request is
    /// refused ([`Refusal::NoCsa`]).
    ///
    /// # Errors
    /// [`BuildError::CsaBase`] for any other base ([`Self::accepts_csa_base`]).
    pub fn with_csa(mut self, base: &str) -> Result<Self, BuildError> {
        self.csa = Some(parse_csa_base(base).ok_or(BuildError::CsaBase)?);
        Ok(self)
    }

    /// Whether [`Self::with_csa`] takes `base`.
    #[must_use]
    pub fn accepts_csa_base(base: &str) -> bool {
        parse_csa_base(base).is_some()
    }

    /// Whether the transport was given the account's CSA base ([`Self::with_csa`]): without one, every CSA request is
    /// refused.
    #[must_use]
    pub fn has_csa(&self) -> bool {
        self.csa.is_some()
    }

    /// The transport with the account's presence base, the region's GTM `presenceUPS` (trimmed): `https`, an AFD host
    /// ([`teams_fence::AFD_HOSTS`]) with no port or user info, the path `/ups/<geo>` exactly (the geo lowercase
    /// letters: `noam`), one trailing slash allowed, no query or fragment. Without it, every presence request is
    /// refused ([`Refusal::NoPresence`]).
    ///
    /// # Errors
    /// [`BuildError::PresenceBase`] for any other base ([`Self::accepts_presence_base`]).
    pub fn with_presence(mut self, base: &str) -> Result<Self, BuildError> {
        self.presence = Some(parse_presence_base(base).ok_or(BuildError::PresenceBase)?);
        Ok(self)
    }

    /// Whether [`Self::with_presence`] takes `base`.
    #[must_use]
    pub fn accepts_presence_base(base: &str) -> bool {
        parse_presence_base(base).is_some()
    }

    /// Whether the transport was given the account's presence base ([`Self::with_presence`]): without one, every
    /// presence request is refused.
    #[must_use]
    pub fn has_presence(&self) -> bool {
        self.presence.is_some()
    }

    /// The transport with the account's AMS base, the region's GTM `amsV2` (trimmed): `https`, a host
    /// `<name>.asyncgw.teams.microsoft.com` (`<name>` lowercase ASCII letters, digits and `-`: `us-prod`) with no port or
    /// user info, no path but `/`, no query or fragment. Without it, every AMS read is refused ([`Refusal::NoAms`]).
    ///
    /// # Errors
    /// [`BuildError::AmsBase`] for any other base ([`Self::accepts_ams_base`]).
    pub fn with_ams(mut self, base: &str) -> Result<Self, BuildError> {
        self.ams = Some(parse_ams_base(base).ok_or(BuildError::AmsBase)?);
        Ok(self)
    }

    /// Whether [`Self::with_ams`] takes `base`.
    #[must_use]
    pub fn accepts_ams_base(base: &str) -> bool {
        parse_ams_base(base).is_some()
    }

    /// Another cap on the wait before a throttled read, plain or own-state request is sent again.
    #[must_use]
    pub fn with_max_retry_wait(mut self, wait: Duration) -> Self {
        self.max_retry_wait = wait;
        self
    }

    /// The chat base requests are built on.
    #[must_use]
    pub fn chat_base(&self) -> &str {
        self.chat_base.as_str()
    }

    /// The account's tenant id, as its `ic3` token's `tid` claim names it (the test call's `add` names it as the
    /// organizer's); `None` without a token, or one that isn't a JWT with that claim. Nothing of the token leaves here.
    pub async fn account_tenant(&self) -> Option<String> {
        let token = self.credentials.bearer(IC3_AUDIENCE).await.ok()?;
        TokenSummary::read(&token)?.tenant
    }

    /// Whether the transport was given the account's middle-tier base ([`Self::with_middle_tier`]): without one, every
    /// middle-tier request is refused.
    #[must_use]
    pub fn has_middle_tier(&self) -> bool {
        self.middle_tier.is_some()
    }

    /// The URL of `endpoint` on `service`, built on the service's base and checked against it.
    fn url_for(&self, service: Service, endpoint: &Endpoint) -> Result<Url, HttpError> {
        let refused = |refusal| HttpError::Refused(refusal);
        if endpoint
            .segments()
            .iter()
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        {
            return Err(refused(Refusal::Segment));
        }
        let base = self.base(service, endpoint)?;
        let mut url = base.clone();
        {
            let mut segments = url.path_segments_mut().map_err(|()| refused(Refusal::NotAllowed))?;
            segments.pop_if_empty().extend(endpoint.segments());
            if service == Service::Presence && ups_slash(endpoint) {
                // UPS's paths end in a slash, as every captured one did (but a subscription's)
                segments.push("");
            }
        }
        if !endpoint.query().is_empty() {
            url.query_pairs_mut().extend_pairs(endpoint.query());
        }
        if on_base(service, &base, &url, endpoint) {
            Ok(url)
        } else {
            Err(refused(Refusal::NotAllowed))
        }
    }

    /// `service`'s base, the one every URL of it is built on.
    fn base(&self, service: Service, endpoint: &Endpoint) -> Result<Url, HttpError> {
        let refused = |refusal| HttpError::Refused(refusal);
        let fixed = |base: String| Url::parse(&base).map_err(|_| refused(Refusal::NotAllowed));
        match service {
            Service::ChatService if endpoint.segments().first().is_none_or(|first| first != "v1") => {
                Err(refused(Refusal::ChatPath))
            }
            Service::ChatService => Ok(self.chat_base.clone()),
            Service::Registrar => fixed(format!("https://{}/registrar", self.registrar_host)),
            Service::Trouter => fixed(format!("https://{TROUTER_HOST}/")),
            Service::Graph => fixed(format!("https://{GRAPH_HOST}/v1.0")),
            Service::MiddleTier => self.middle_tier.clone().ok_or(refused(Refusal::NoMiddleTier)),
            Service::Csa => self.csa.clone().ok_or(refused(Refusal::NoCsa)),
            Service::Presence => self.presence.clone().ok_or(refused(Refusal::NoPresence)),
            Service::Calling => Err(refused(Refusal::Calling)),
            Service::FileThumbnail => Err(refused(Refusal::FileThumbnail)),
            Service::Relay => fixed(format!("https://{RELAY_HOST}/{RELAY_PATH}")),
            Service::Emoticons => fixed(format!("https://{EMOTICONS_HOST}/{EMOTICONS_PATH}")),
            Service::Giphy => fixed(format!("https://{GIPHY_HOST}/")),
            Service::Ams => self.ams.clone().ok_or(refused(Refusal::NoAms)),
        }
    }

    /// Whether the classifier lets a request through (see [`Transport::send`]): `Ok(None)` when it reaches nobody, the
    /// refusal of a write or of a request it can't read otherwise; through the personal build's fence, which denies
    /// nothing, those go too, as `Ok(Some("write"))` and `Ok(Some("unreadable"))`, the classification its log line names.
    /// It sees the request as it is sent: its method, URL, headers (the token aside, which changes nothing about what it
    /// does) and body.
    fn judge(
        &self,
        method: &reqwest::Method,
        url: &Url,
        headers: &[(String, String)],
        body: Option<&[u8]>,
    ) -> Result<Option<&'static str>, HttpError> {
        let request = headers
            .iter()
            .fold(
                FenceRequest::new(method.as_str(), url.as_str()),
                |request, (name, value)| request.with_header(name, value),
            )
            .with_body(body);
        match (contacting(&request, &self.policy), self.is_personal()) {
            (Contact::None, _) => Ok(None),
            (Contact::Write(_), true) => Ok(Some("write")),
            (Contact::Unreadable, true) => Ok(Some("unreadable")),
            (Contact::Write(_), false) => Err(HttpError::Refused(Refusal::Write)),
            (Contact::Unreadable, false) => Err(HttpError::Refused(Refusal::Unreadable)),
        }
    }

    /// Sends a plain or own-state request (`record`: what is logged before each send) once the classifier let it
    /// through, with the one retry [`Self::exchange`] allows.
    async fn send_plain(
        &self,
        service: Service,
        method: reqwest::Method,
        endpoint: &Endpoint,
        body: Option<&Value>,
        record: Record,
    ) -> Result<Response, HttpError> {
        let url = self.url_for(service, endpoint)?;
        if self.read_only && !matches!(method, reqwest::Method::GET | reqwest::Method::HEAD) {
            return Err(self.refused_read_only(method.as_str(), url.as_str()));
        }
        let body = body.map(|body| body.to_string().into_bytes());
        let record = match self.judge(&method, &url, &plain_headers(service, body.is_some()), body.as_deref())? {
            Some(contact) => Record::Unjudged(contact),
            None => record,
        };
        let outgoing = Outgoing {
            method,
            url: &url,
            service,
            body: body.as_deref(),
        };
        self.exchange(outgoing, record).await
    }

    /// Where a checked URL is sent: itself, or (`test-support` only) the mock origin with its path and query.
    fn destination(&self, url: &Url) -> Url {
        match &self.mock_origin {
            Some(origin) => {
                let mut mocked = origin.clone();
                mocked.set_path(url.path());
                mocked.set_query(url.query());
                mocked
            }
            None => url.clone(),
        }
    }

    async fn token(&self, service: Service) -> Result<Option<Secret>, HttpError> {
        match audience(service) {
            Some(audience) => Ok(Some(
                self.credentials
                    .bearer(audience)
                    .await
                    .map_err(HttpError::Credentials)?,
            )),
            None => Ok(None),
        }
    }

    /// Sends a read, plain or own-state request (`record`: what is logged before every send), with the one retry a 401,
    /// 429 or 503 allows; any answer but a success is [`HttpError::Status`].
    async fn exchange(&self, outgoing: Outgoing<'_>, record: Record) -> Result<Response, HttpError> {
        let mut token = self.token(outgoing.service).await?;
        let first = self.attempt(&outgoing, record, token.as_ref()).await?;
        let retry = match (first.status(), audience(outgoing.service), &token) {
            (401, Some(audience), Some(refused)) => {
                log::info!("{}: HTTP 401, sent once more with a renewed token", shown(&outgoing));
                let renewed = self.credentials.bearer_rejected(audience, refused).await;
                token = Some(renewed.map_err(HttpError::Credentials)?);
                true
            }
            (status @ (429 | 503), ..) => {
                let wait = first.retry_after().unwrap_or(DEFAULT_RETRY_WAIT);
                let wait = wait.min(self.max_retry_wait);
                log::info!(
                    "{}: HTTP {status}, sent once more after {} ms",
                    shown(&outgoing),
                    wait.as_millis()
                );
                tokio::time::sleep(wait).await;
                // the session may have signed out (or renewed) during the wait: the retry takes what it holds now
                token = self.token(outgoing.service).await?;
                true
            }
            _ => false,
        };
        let answer = if retry {
            self.attempt(&outgoing, record, token.as_ref()).await?
        } else {
            first
        };
        if answer.is_success() {
            Ok(answer)
        } else {
            Err(HttpError::Status(Box::new(answer)))
        }
    }

    /// One send; a plain or own-state request is logged first, and not sent when it can't be.
    async fn attempt(
        &self,
        outgoing: &Outgoing<'_>,
        record: Record,
        token: Option<&Secret>,
    ) -> Result<Response, HttpError> {
        let (method, url) = (outgoing.method.as_str(), outgoing.url.as_str());
        let logged = match record {
            Record::Nothing => Ok(()),
            Record::Allowed => self.fence.log().record_allowed(&AllowedRecord { method, url }),
            Record::Unjudged(contact) => self
                .fence
                .log()
                .record_unjudged(&AllowedRecord { method, url }, contact),
            Record::Own(kind) => self.fence.log().record_request(&RequestRecord {
                channel: Channel::Http,
                method,
                url,
                outcome: RequestOutcome::Own(kind),
            }),
        };
        logged.map_err(HttpError::LogUnavailable)?;
        let headers = plain_headers(outgoing.service, outgoing.body.is_some());
        let body = outgoing.body.map(<[u8]>::to_vec);
        self.send_as(
            outgoing.method.clone(),
            outgoing.url,
            &headers,
            Auth::bearer(token),
            body,
        )
        .await
    }

    /// One request with exactly `headers` (lower-cased names, none of them a credential) and the credential `auth`
    /// (none, a bearer token, or the skypetoken), and its answer read.
    async fn send_as(
        &self,
        method: reqwest::Method,
        url: &Url,
        headers: &[(String, String)],
        auth: Auth<'_>,
        body: Option<Vec<u8>>,
    ) -> Result<Response, HttpError> {
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| HttpError::Unknown)?;
            let value = HeaderValue::from_str(value).map_err(|_| HttpError::Unknown)?;
            map.append(name, value);
        }
        let credential = match auth {
            Auth::None => None,
            Auth::Bearer(token) => Some((AUTHORIZATION, format!("Bearer {}", token.expose()))),
            Auth::Skypetoken(token) => Some((HeaderName::from_static(SKYPETOKEN_HEADER), token.expose().to_owned())),
        };
        if let Some((name, value)) = credential {
            let mut value = HeaderValue::from_str(&value).map_err(|_| HttpError::Refused(Refusal::Token))?;
            value.set_sensitive(true);
            map.insert(name, value);
        }
        let full = log::log_enabled!(log::Level::Debug);
        if full {
            let headers = header_pairs(&map);
            let request = teams_log::wire::Request {
                method: method.as_str(),
                url: url.as_str(),
                headers: &headers,
                body: body.as_deref(),
            };
            log::debug!(target: teams_log::wire::HTTP_TARGET, "{request}");
        }
        let shown = format!("{method} {}{}", url.host_str().unwrap_or_default(), url.path());
        let started = Instant::now();
        let mut request = self.client.request(method.clone(), self.destination(url)).headers(map);
        if let Some(body) = body {
            request = request.body(body);
        }
        let answered = match request.send().await {
            Ok(answer) => read_answer(answer, full, max_body(url)).await,
            Err(error) => Err(failure(&error)),
        };
        let elapsed = started.elapsed();
        match &answered {
            Ok((answer, headers)) => {
                log::info!("{shown} -> HTTP {} in {} ms", answer.status(), elapsed.as_millis());
                if full {
                    let response = teams_log::wire::Response {
                        method: method.as_str(),
                        url: url.as_str(),
                        status: answer.status(),
                        headers,
                        body: answer.body(),
                        elapsed,
                    };
                    log::debug!(target: teams_log::wire::HTTP_TARGET, "{response}");
                }
            }
            Err(error) => log::info!("{shown} failed after {} ms: {error}", elapsed.as_millis()),
        }
        answered.map(|(answer, _)| answer)
    }

    /// Sends a read by `POST` (see [`HttpTransport::post_read`]): the request built from the entry, read back against
    /// the entry's captured shape and the permit's MRIs, its token, its `fence.log` record, then one send.
    async fn send_post_read<R: PostRead + Send>(&self, read: AuthorizedPostRead<R>) -> Result<Response, HttpError> {
        let shape = read.shape();
        let wire = read.wire();
        let service = shape.service();
        let url = self.url_for(service, wire.endpoint())?;
        let mut headers = plain_headers(service, true);
        headers.extend(wire.headers().iter().cloned());
        let body = serde_json::to_vec(wire.body()).map_err(|_| HttpError::Unknown)?;
        let query: Vec<(String, String)> = url
            .query_pairs()
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect();
        let names = shape
            .read_back(wire.endpoint().segments(), &query, &headers, &body)
            .ok_or(HttpError::Unknown)?;
        if names != read.permit().names() {
            return Err(HttpError::Unknown);
        }
        self.judge(&reqwest::Method::POST, &url, &headers, Some(&body))?;
        let token = self.token(service).await?;
        let record = RequestRecord {
            channel: Channel::Http,
            method: "POST",
            url: url.as_str(),
            outcome: RequestOutcome::PostRead(R::KIND),
        };
        self.fence
            .log()
            .record_request(&record)
            .map_err(HttpError::LogUnavailable)?;
        let answer = self
            .send_as(
                reqwest::Method::POST,
                &url,
                &headers,
                Auth::bearer(token.as_ref()),
                Some(body),
            )
            .await?;
        if let (401, Some(audience), Some(refused)) = (answer.status(), audience(service), &token) {
            // the session refreshes the refused token for a later request, under a new permit; this one isn't resent
            let _ = self.credentials.bearer_rejected(audience, refused).await;
        }
        if answer.is_success() {
            Ok(answer)
        } else {
            Err(HttpError::Status(Box::new(answer)))
        }
    }
}

/// The calling service's writes (see [`Self::deliver_call`]).
impl<C: Credentials> HttpTransport<C> {
    /// Sends an authorized call write (`crate::StartCall` and the others) to [`Service::Calling`], once: its exact URL,
    /// checked against the calling base before anything else ([`Refusal::NotAllowed`]), with the account's skypetoken
    /// ([`Refusal::NoSkypetoken`] without one), the call's headers and its body as the fence classified them. A call
    /// start or a join with media is pended in the fence's call registry first ([`Fence::pend_start`], with exactly the
    /// body sent) and registered from a successful answer ([`Fence::register_start`]), ended unregistered by any other
    /// ([`Fence::abandon_start`], so a start sent again is the call its acceptance binds to); an `attach` or an
    /// `accept` pends its answer ([`Fence::pend_answer`]) and binds the links a successful answer hands out
    /// ([`Fence::bind_answer`]); one answered without success binds nothing (whether its invitation is let go is the
    /// call machine's to say once no further attempt goes, [`Fence::let_go`]). A start's permit the registry won't take
    /// is [`Refusal::NotPended`], nothing sent, and so is an `attach`'s or an `accept`'s on a fence that tracks calls
    /// ([`Fence::tracks_calls`]); a lab fence's goes with its answer unread (its fence judged it by its entry, and
    /// nothing it sends later needs a binding). The answer comes back whatever its status, as any write's; after a 401
    /// the session renews the skypetoken for a later request. A registration that fails is logged (on a fence that
    /// tracks calls the call's follow-ups then stay denied), never retried.
    async fn deliver_call(&self, permit: &WritePermit, call: &CallWire) -> Result<Response, HttpError> {
        let url = call.url();
        if !on_calling_base(url) {
            return Err(HttpError::Refused(Refusal::NotAllowed));
        }
        let token = self
            .credentials
            .skypetoken()
            .await
            .map_err(HttpError::Credentials)?
            .ok_or(HttpError::Refused(Refusal::NoSkypetoken))?;
        let body = call.body_bytes();
        let pending = match call.registry() {
            Registry::Nothing => Pending::Nothing,
            Registry::Start => self
                .fence
                .pend_start(permit, body.as_deref().unwrap_or_default())
                .map(Pending::Start)
                .ok_or(HttpError::Refused(Refusal::NotPended))?,
            Registry::Answer => match self.fence.pend_answer(permit) {
                Some(pending) => Pending::Answer(pending),
                // a lab fence permitted it by its entry rule and binds nothing on it: it goes, its answer unread
                None if !self.fence.tracks_calls() => Pending::Nothing,
                None => return Err(HttpError::Refused(Refusal::NotPended)),
            },
        };
        // a `POST` with nothing to say (an acknowledgement) goes as the web client sends one: an empty JSON body,
        // `Content-Length: 0` with its `Content-Type` (the `mediaAnswers` acknowledgement, `ab-call --call video`). Sent
        // with no body at all, the call controller took 22 s to answer it (M3 Task 18, live), and a renegotiation of the
        // other side's waiting on it was rejected meanwhile
        let body = body.or_else(|| (call.method() == crate::endpoint::Method::Post).then(Vec::new));
        let mut headers = plain_headers(Service::Calling, body.is_some());
        if body.as_ref().is_some_and(Vec::is_empty) {
            // the HTTP client says no length of an empty body by itself
            headers.push(("content-length".to_owned(), "0".to_owned()));
        }
        headers.extend(
            call.headers()
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone())),
        );
        let answer = self
            .send_as(
                call.method().to_reqwest(),
                url,
                &headers,
                Auth::Skypetoken(&token),
                body,
            )
            .await?;
        if answer.status() == 401 {
            // renewed for a later request; this write isn't resent
            let _ = self.credentials.skypetoken_rejected(&token).await;
        }
        if answer.is_success() {
            self.register(pending, answer.body(), url);
        } else {
            self.unregistered(pending);
        }
        Ok(answer)
    }

    /// A pended call write answered without success: the service didn't take it. A start ends unregistered (the start
    /// sent again pends a call of its own, and this one binds nothing); an attach or an accept binds nothing, and its
    /// invitation stays as it was: an attach refused for a reason another attempt may clear (a 401, a short throttle)
    /// goes again on it, and one that failed for good is let go by the call machine ([`Fence::let_go`]).
    fn unregistered(&self, pending: Pending) {
        if let Pending::Start(pending) = pending {
            self.fence.abandon_start(pending);
        }
    }

    /// Registers a pended call write's answer in the fence's call registry (see [`Self::deliver_call`]).
    fn register(&self, pending: Pending, answer: &[u8], url: &Url) {
        let registered = match pending {
            Pending::Nothing => return,
            Pending::Start(pending) => self.fence.register_start(pending, answer),
            Pending::Answer(pending) => self.fence.bind_answer(pending, answer),
        };
        if let Err(error) = registered {
            log::info!(
                "{}{}: the answer registered nothing in the call registry: {error}",
                url.host_str().unwrap_or_default(),
                url.path()
            );
        }
    }
}

/// A call write's registry step, waiting for its answer.
enum Pending {
    Nothing,
    Start(PendingStart),
    Answer(PendingAnswer),
}

/// The personal build's sealed lists (see the module docs): a lab binary's requests of their entries go through
/// [`Transport::send`] instead.
impl<C: Credentials> HttpTransport<C> {
    /// Sends an entry of the sealed own-state list once the classifier let it through, logged to `fence.log` by its
    /// kind before every send, with the one retry a read has.
    ///
    /// # Errors
    /// As [`Transport::send`]'s.
    pub async fn own_state<R: OwnState>(&self, request: &R) -> Result<Response, HttpError> {
        let (service, method, endpoint, body) =
            (request.service(), request.method(), request.endpoint(), request.body());
        self.send_plain(
            service,
            method.to_reqwest(),
            &endpoint,
            body.as_ref(),
            Record::Own(R::KIND),
        )
        .await
    }

    /// Sends a permitted read by `POST` of the sealed list, once, logged to `fence.log` before it goes, and only in its
    /// entry's captured shape ([`HttpError::Unknown`] otherwise, with nothing sent).
    ///
    /// # Errors
    /// [`HttpError::Unknown`] for a request off its entry's shape or naming other MRIs than its permit; otherwise as a
    /// read's.
    pub async fn post_read<R: PostRead + Send>(&self, read: AuthorizedPostRead<R>) -> Result<Response, HttpError> {
        self.send_post_read(read).await
    }

    /// Reads the picture of a file's thumbnail at `link`, the link Graph handed out for it
    /// ([`crate::FileThumbnails::read_answer`]): a `GET` on [`Service::FileThumbnail`] with no token of the account's
    /// (the link carries its own credential), the link checked again before it goes ([`is_thumbnail_link`]), with the
    /// one retry a 429 or 503 allows, as any read.
    ///
    /// # Errors
    /// As a read's: [`Refusal::NotAllowed`] for a link off the thumbnail hosts, [`HttpError::Status`] for an answer that
    /// isn't a success.
    pub async fn file_thumbnail(&self, link: &ThumbnailLink) -> Result<Response, HttpError> {
        let url = link.url();
        if !is_thumbnail_link(url) {
            return Err(HttpError::Refused(Refusal::NotAllowed));
        }
        let outgoing = Outgoing {
            method: reqwest::Method::GET,
            url,
            service: Service::FileThumbnail,
            body: None,
        };
        self.exchange(outgoing, Record::Nothing).await
    }
}

impl<C: Credentials> Transport for HttpTransport<C> {
    type Error = HttpError;

    fn read<R: Read>(&self, request: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        let service = request.service();
        let endpoint = request.endpoint();
        async move {
            let url = self.url_for(service, &endpoint)?;
            let outgoing = Outgoing {
                method: reqwest::Method::GET,
                url: &url,
                service,
                body: None,
            };
            self.exchange(outgoing, Record::Nothing).await
        }
    }

    fn send<R: Request>(&self, request: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        let (service, method, endpoint, body) =
            (request.service(), request.method(), request.endpoint(), request.body());
        async move {
            self.send_plain(service, method.to_reqwest(), &endpoint, body.as_ref(), Record::Allowed)
                .await
        }
    }

    async fn send_own<R: OwnState + Request + Sync>(&self, request: &R) -> Result<Response, HttpError> {
        if self.is_personal() {
            self.own_state(request).await
        } else {
            self.send(request).await
        }
    }

    fn own_account(&self) -> Option<&Mri> {
        self.fence.own_account()
    }

    fn frame_log(&self) -> Option<Arc<Fence>> {
        self.is_personal().then(|| Arc::clone(&self.fence))
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn read_only_refusal<R: Mutating>(&self, write: &R) -> Option<HttpError> {
        self.read_only.then(|| self.refused_write(write))
    }

    async fn deliver<R: Mutating + Send>(&self, write: Authorized<R>) -> Result<Response, HttpError> {
        if self.read_only {
            return Err(self.refused_write(write.request()));
        }
        if let Some(call) = write.call() {
            return self.deliver_call(write.permit(), call).await;
        }
        let wire = write.wire().ok_or(HttpError::NoRoute)?;
        let service = wire.service();
        match service {
            // the chat service's writes, and UPS's: the account's own availability
            Service::ChatService | Service::Presence => {}
            _ => return Err(HttpError::NoRoute),
        }
        let url = self.url_for(service, wire.endpoint())?;
        let audience = audience(service).ok_or(HttpError::NoRoute)?;
        let token = self
            .credentials
            .bearer(audience)
            .await
            .map_err(HttpError::Credentials)?;
        // an own-availability write sends its `Content-Type` even without a body (the reset), as captured
        let mut headers = plain_headers(service, wire.body().is_some() || service == Service::Presence);
        headers.extend(
            wire.headers()
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone())),
        );
        let body = wire.body().map(|body| body.to_string().into_bytes());
        let answer = self
            .send_as(wire.method().to_reqwest(), &url, &headers, Auth::Bearer(&token), body)
            .await?;
        if answer.status() == 401 {
            // the session refreshes the refused token for the outbox's next attempt, which also meets any failure of it
            let _ = self.credentials.bearer_rejected(audience, &token).await;
        }
        Ok(answer)
    }
}

/// The fence's live membership read, for a native client, reads with what the transport writes with: the account's
/// `ic3` token as its session has it now (renewed before it expires) and the transport's chat base, as
/// [`ChatCredential::bearer`]. No token, or none in a token's shape, is no credential, and the read is
/// `Partial(NoCredential)`. A token the chat service refuses is renewed as a refused token of the transport's own is
/// ([`Credentials::bearer_rejected`]), so the read is sent once more with the new one; a failed renewal is no renewed
/// credential, and the read stays refused.
impl<C: Credentials> CredentialSource for HttpTransport<C> {
    fn credential(&self) -> CredentialFuture<'_> {
        Box::pin(async move {
            let token = self.credentials.bearer(IC3_AUDIENCE).await.ok()?;
            ChatCredential::bearer(self.chat_base.as_str(), token.expose())
        })
    }

    fn credential_rejected<'a>(&'a self, rejected: &'a ChatCredential) -> CredentialFuture<'a> {
        Box::pin(async move {
            let refused = Secret::new(rejected.expose_token());
            let token = self.credentials.bearer_rejected(IC3_AUDIENCE, &refused).await.ok()?;
            ChatCredential::bearer(self.chat_base.as_str(), token.expose())
        })
    }
}

/// The credential a request carries: none, a bearer token, or the account's skypetoken (the calling service's).
#[derive(Clone, Copy)]
enum Auth<'a> {
    None,
    Bearer(&'a Secret),
    Skypetoken(&'a Secret),
}

impl<'a> Auth<'a> {
    /// A bearer token when there is one.
    fn bearer(token: Option<&'a Secret>) -> Self {
        token.map_or(Self::None, Self::Bearer)
    }
}

/// The header the calling service takes the skypetoken in.
const SKYPETOKEN_HEADER: &str = "x-skypetoken";

/// The TURN credentials' host and path ([`Service::Relay`]).
pub const RELAY_HOST: &str = "teams.microsoft.com";
const RELAY_PATH: &str = "trap";

/// The async gateway's hosts end in this ([`Service::Ams`]: `us-prod.asyncgw.teams.microsoft.com`).
pub const AMS_HOST_SUFFIX: &str = ".asyncgw.teams.microsoft.com";

/// The `Accept` a picture's view is read with, as a browser's `<img>` sends it.
const PICTURE_ACCEPT: &str = "image/avif,image/webp,image/apng,image/*,*/*;q=0.8";

/// Teams' reaction pictures' host and path ([`Service::Emoticons`]).
pub const EMOTICONS_HOST: &str = "statics.teams.cdn.office.net";
const EMOTICONS_PATH: &str = "evergreen-assets/personal-expressions/v2/assets/emoticons";

/// Giphy's media host ([`Service::Giphy`]).
pub const GIPHY_HOST: &str = "media.giphy.com";

/// The `Accept` a GIF is read with: a GIF, whose frames the views play (a browser's `Accept` would take a WebP, of
/// which only the first frame is drawn).
const GIF_ACCEPT: &str = "image/gif";

/// The Entra audience of the bearer token `service` takes, if it takes one.
fn audience(service: Service) -> Option<&'static str> {
    match service {
        Service::ChatService | Service::Registrar | Service::Relay | Service::Ams => Some(IC3_AUDIENCE),
        Service::MiddleTier => Some(MIDDLE_TIER_AUDIENCE),
        Service::Graph => Some(GRAPH_AUDIENCE),
        Service::Csa => Some(CSA_AUDIENCE),
        Service::Presence => Some(PRESENCE_AUDIENCE),
        Service::Trouter | Service::Calling | Service::Emoticons | Service::FileThumbnail | Service::Giphy => None,
    }
}

/// The headers a request to `service` goes out with besides its token, by lower-cased name: `Accept`, the body's
/// `Content-Type` when it has one, and the service's own.
fn plain_headers(service: Service, body: bool) -> Vec<(String, String)> {
    let content_type = match service {
        Service::MiddleTier => "application/json;charset=UTF-8",
        Service::ChatService
        | Service::Registrar
        | Service::Trouter
        | Service::Graph
        | Service::Csa
        | Service::Presence
        | Service::Calling
        | Service::Relay
        | Service::Emoticons
        | Service::Ams
        | Service::FileThumbnail
        | Service::Giphy => "application/json",
    };
    // a picture's view and a thumbnail answer an image, as a browser's `<img>` asks for one
    let accept = match service {
        Service::Ams | Service::FileThumbnail => PICTURE_ACCEPT,
        Service::Giphy => GIF_ACCEPT,
        Service::ChatService
        | Service::Registrar
        | Service::Trouter
        | Service::MiddleTier
        | Service::Graph
        | Service::Csa
        | Service::Presence
        | Service::Calling
        | Service::Relay
        | Service::Emoticons => "application/json",
    };
    let mut headers = vec![(ACCEPT.as_str().to_owned(), accept.to_owned())];
    if body {
        headers.push((CONTENT_TYPE.as_str().to_owned(), content_type.to_owned()));
    }
    headers.extend(
        service_headers(service)
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
    );
    headers
}

/// The headers `service` takes besides the token, as the web client sends them.
fn service_headers(service: Service) -> &'static [(&'static str, &'static str)] {
    match service {
        Service::ChatService => &[("x-ms-migration", "True"), ("behavioroverride", "redirectAs404")],
        Service::Registrar => &[("x-ms-migration", "True"), ("x-ms-test-user", "False")],
        Service::MiddleTier => &[
            ("x-ms-client-request-type", "0"),
            ("x-ms-client-type", "cdlworker"),
            ("x-ms-migration", "True"),
            ("x-ms-object-id", ""),
            ("x-ms-test-user", "False"),
        ],
        // the values every captured presence read carried
        Service::Presence => &[
            ("behavioroverride", "redirectAs404"),
            ("x-ms-client-type", "cdlworker"),
            ("x-ms-client-user-agent", "Teams-V2-Web"),
            ("x-ms-object-id", ""),
        ],
        // the Entra form of the web client's read (`docs/protocol/auth.md`)
        Service::Relay => &[("api-version", "2"), ("x-ms-migration", "True")],
        // the object read's form (`proto-lab probe object`), as the web client's object creation sent it
        Service::Ams => &[("x-ms-migration", "True")],
        Service::Trouter
        | Service::Graph
        | Service::Csa
        | Service::Calling
        | Service::Emoticons
        | Service::FileThumbnail
        | Service::Giphy => &[],
    }
}

/// Whether `url`, built for `endpoint` on `service`, is on `base` and nothing else: `https`, no user info, port or
/// fragment, the base's host, a decoded path that is exactly the base's and the endpoint's segments (UPS's with its
/// trailing slash, but a subscription's), and the endpoint's query exactly. The chat service's also as
/// [`chat_service_path`] reads it, and the registrar's on an AFD host.
fn on_base(service: Service, base: &Url, url: &Url, endpoint: &Endpoint) -> bool {
    let plain = url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none();
    let segments = endpoint.segments();
    let mut wanted = decoded_path(base).unwrap_or_default();
    wanted.extend(segments.iter().cloned());
    if service == Service::Presence && ups_slash(endpoint) {
        wanted.push(String::new());
    }
    let service_rule = match service {
        Service::ChatService => chat_service_path(url).is_some_and(|rest| segments.get(1..) == Some(&rest[..])),
        Service::Registrar => url.host_str().is_some_and(|host| AFD_HOSTS.contains(&host)),
        Service::Trouter
        | Service::Graph
        | Service::MiddleTier
        | Service::Csa
        | Service::Presence
        | Service::Relay
        | Service::Calling
        | Service::Emoticons
        | Service::Ams
        | Service::FileThumbnail
        | Service::Giphy => true,
    };
    plain
        && url.host_str() == base.host_str()
        && decoded_path(url).is_some_and(|path| path == wanted)
        && query_is(url, endpoint.query())
        && service_rule
}

/// Whether a UPS path ends in a slash: every captured one did but the presence subscription's
/// (`v1/pubsub/subscriptions/<endpoint>`, [`crate::PresenceSubscription`]), which never did.
fn ups_slash(endpoint: &Endpoint) -> bool {
    !matches!(
        endpoint.segments(),
        [v1, pubsub, subscriptions, _] if v1 == "v1" && pubsub == "pubsub" && subscriptions == "subscriptions"
    )
}

/// Whether `url`'s query is exactly `query`, pair by pair in order.
fn query_is(url: &Url, query: &[(String, String)]) -> bool {
    url.query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .eq(query.iter().cloned())
}

/// The AMS base, if `base` is one [`HttpTransport::with_ams`] accepts.
fn parse_ams_base(base: &str) -> Option<Url> {
    if base.contains(['?', '#']) || base.trim() != base {
        return None;
    }
    let base = base.strip_suffix('/').unwrap_or(base);
    let url = Url::parse(base).ok()?;
    let host = url.host_str()?;
    let named = host.strip_suffix(AMS_HOST_SUFFIX).is_some_and(|name| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    });
    let plain = url.scheme() == "https"
        && named
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        // the base as it was given (with the slash `Url` keeps for an empty path), not one the parser normalized
        && url.as_str() == format!("{base}/");
    plain.then_some(url)
}

/// The presence base, if `base` is one [`HttpTransport::with_presence`] accepts.
fn parse_presence_base(base: &str) -> Option<Url> {
    if base.contains(['?', '#']) || base.trim() != base {
        return None;
    }
    let base = base.strip_suffix('/').unwrap_or(base);
    let url = Url::parse(base).ok()?;
    let plain = url.scheme() == "https"
        && url.host_str().is_some_and(|host| AFD_HOSTS.contains(&host))
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        // the base as it was given, not one the parser normalized
        && url.as_str() == base;
    let path = decoded_path(&url)?;
    let geo = matches!(
        path.as_slice(),
        [ups, geo] if ups == "ups" && !geo.is_empty() && geo.bytes().all(|byte| byte.is_ascii_lowercase())
    );
    (plain && geo).then_some(url)
}

/// The CSA base, if `base` is one [`HttpTransport::with_csa`] accepts.
fn parse_csa_base(base: &str) -> Option<Url> {
    if base.contains(['?', '#']) || base.trim() != base {
        return None;
    }
    let base = base.strip_suffix('/').unwrap_or(base);
    let url = Url::parse(base).ok()?;
    let plain = url.scheme() == "https"
        && url.host_str().is_some_and(|host| AFD_HOSTS.contains(&host))
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        // the base as it was given, not one the parser normalized
        && url.as_str() == base;
    let path = decoded_path(&url)?;
    let region = matches!(
        path.as_slice(),
        [api, csa, region] if api == "api"
            && csa == "csa"
            && !region.is_empty()
            && region.bytes().all(|byte| byte.is_ascii_lowercase())
    );
    (plain && region).then_some(url)
}

/// The middle-tier base, if `base` is one [`HttpTransport::with_middle_tier`] accepts.
fn parse_middle_tier_base(base: &str) -> Option<Url> {
    if base.contains(['?', '#']) || base.trim() != base {
        return None;
    }
    let base = base.strip_suffix('/').unwrap_or(base);
    let url = Url::parse(base).ok()?;
    let plain = url.scheme() == "https"
        && url.host_str().is_some_and(|host| AFD_HOSTS.contains(&host))
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        // the base as it was given, not one the parser normalized
        && url.as_str() == base;
    let path = decoded_path(&url)?;
    let partition = matches!(
        path.as_slice(),
        [api, mt, part, partition] if api == "api" && mt == "mt" && part == "part" && is_partition(partition)
    );
    (plain && partition).then_some(url)
}

/// A middle-tier partition: a lowercase region, `-`, a number (`amer-03`).
fn is_partition(partition: &str) -> bool {
    partition.split_once('-').is_some_and(|(region, number)| {
        !region.is_empty()
            && region.bytes().all(|byte| byte.is_ascii_lowercase())
            && !number.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// The URL's path segments, percent-decoded; a bare `/` is none.
fn decoded_path(url: &Url) -> Option<Vec<String>> {
    let segments: Vec<String> = url
        .path_segments()?
        .map(|segment| percent_decode_str(segment).decode_utf8().ok().map(String::from))
        .collect::<Option<_>>()?;
    Some(if segments == [""] { Vec::new() } else { segments })
}

/// The chat base, if `base` is exactly one [`chat_service_path`] accepts a request under (see [`HttpTransport::new`]).
fn parse_chat_base(base: &str) -> Option<Url> {
    if base.contains(['?', '#']) || base.trim() != base {
        return None;
    }
    let base = base.strip_suffix('/').unwrap_or(base);
    let probe = Url::parse(&format!("{base}/v1")).ok()?;
    chat_service_path(&probe).filter(Vec::is_empty)?;
    Url::parse(base).ok()
}

/// A loopback `http` origin with a port and nothing after it.
#[cfg(feature = "test-support")]
fn parse_mock_origin(origin: &str) -> Option<Url> {
    let url = Url::parse(origin).ok()?;
    let loopback = matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"));
    let bare = url.path() == "/" && url.query().is_none() && url.fragment().is_none() && url.username().is_empty();
    (url.scheme() == "http" && loopback && url.port().is_some() && bare && url.password().is_none()).then_some(url)
}

/// The largest body read for a request to `url`: [`MAX_PICTURE_BODY`] from AMS's async gateway and Giphy's media host,
/// [`MAX_BODY`] from anywhere else.
fn max_body(url: &Url) -> usize {
    if url
        .host_str()
        .is_some_and(|host| host.ends_with(AMS_HOST_SUFFIX) || host == GIPHY_HOST)
    {
        MAX_PICTURE_BODY
    } else {
        MAX_BODY
    }
}

/// A request's description in the log at `info`: its method, host and path, never its query.
fn shown(outgoing: &Outgoing<'_>) -> String {
    format!(
        "{} {}{}",
        outgoing.method,
        outgoing.url.host_str().unwrap_or_default(),
        outgoing.url.path()
    )
}

/// Every header of `map` as text, for the log's `debug` records (which redact the credentials'): a value that isn't
/// text is taken lossily.
fn header_pairs(map: &HeaderMap) -> Vec<(String, String)> {
    map.iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect()
}

/// Reads an answer: its status, `Location`, `Retry-After`, `Content-Type`, and its body up to [`MAX_BODY`]; with
/// `all_headers`, every header too, for the log's `debug` record (none otherwise).
async fn read_answer(
    mut answer: reqwest::Response,
    all_headers: bool,
    max_body: usize,
) -> Result<(Response, Vec<(String, String)>), HttpError> {
    let headers = answer.headers();
    let kept = if all_headers { header_pairs(headers) } else { Vec::new() };
    let text = |name| {
        headers
            .get(name)
            .and_then(|value: &HeaderValue| value.to_str().ok())
            .map(str::to_owned)
    };
    let (location, content_type) = (text(LOCATION), text(CONTENT_TYPE));
    let wait = retry_after(headers);
    let status = answer.status().as_u16();
    let mut body = Vec::new();
    while let Some(chunk) = answer.chunk().await.map_err(|error| failure(&error))? {
        if body.len() + chunk.len() > max_body {
            return Err(HttpError::BodyTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    let mut response = Response::new(status, body);
    if let Some(location) = location {
        response = response.with_location(location);
    }
    if let Some(content_type) = content_type {
        response = response.with_content_type(content_type);
    }
    if let Some(wait) = wait {
        response = response.with_retry_after(wait);
    }
    Ok((response, kept))
}

fn failure(error: &reqwest::Error) -> HttpError {
    HttpError::Failed(if error.is_timeout() {
        Failure::Timeout
    } else if error.is_body() || error.is_decode() {
        Failure::Body
    } else {
        Failure::Network
    })
}
