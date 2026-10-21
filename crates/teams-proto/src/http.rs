//! The native HTTP transport (M1 Task 5): the one way the native client sends anything to Teams over HTTP.
//!
//! [`HttpTransport`] takes the three classes of request [`Transport`] defines and nothing else: a [`Read`] (always a
//! `GET`), an [`OwnState`] request (the sealed, reviewed list, logged to `fence.log` before it is sent; one that can't
//! be logged isn't sent), and an [`Authorized`] write (its sealed wire form, under the permit `Fence::check` issued for
//! its target). It has no method that takes a URL, a method or a body of the caller's choosing.
//!
//! - **Hosts**: a fixed allowlist. Every URL is built from a [`Service`]'s base and checked before anything is sent:
//!   the chat service exactly as [`teams_fence::chat_service_path`] accepts it (the base the account's region gives,
//!   `chatServiceAfd`), the registrar on an AFD host (`https://<AFD host>/registrar/…`), and Trouter's HTTP side
//!   (`https://go.trouter.teams.microsoft.com/`). An endpoint with an empty, `.` or `..` segment is refused, and so is
//!   any URL whose decoded path isn't exactly the base's plus the endpoint's segments. Entra and authz aren't on it
//!   (they stay in `teams-auth`'s own clients, the reviewed exception), and neither is AMS.
//! - **The client**: `https` only, no proxy (an environment's `HTTPS_PROXY` could point anywhere), no redirects (a 3xx
//!   is an answer, never followed: a region hop is the caller's to report), no retries of its own (reqwest's default
//!   replays a request after an HTTP/2 GOAWAY or `REFUSED_STREAM`, which would send a write again and an own-state
//!   request unlogged), [`CONNECT_TIMEOUT`] and [`REQUEST_TIMEOUT`], bodies read up to [`MAX_BODY`].
//! - **Credentials** come from the account's [`Session`] ([`Credentials`]), per service: the chat service takes
//!   `Authorization: Bearer <ic3>` with `X-MS-Migration: True` and `BehaviorOverride: redirectAs404` (the web client's
//!   set, which the service took with our own non-CAE token, M1 Task 3); the registrar the same token with
//!   `X-MS-Migration: True` and `X-Ms-Test-User: False` (the messaging stack's registration in the ab-trouter
//!   captures of 2026-10-20; the calling stack's in the echo-call capture had no `X-Ms-Test-User`); Trouter's HTTP
//!   side none (its probe carried none).
//! - **Retries** (the transport's part of the plan's retry table), for reads and own-state requests: a 401 makes the
//!   session refresh the refused token, a 429 or 503 waits its `Retry-After` (capped at the transport's maximum,
//!   [`MAX_RETRY_WAIT`] by default; [`DEFAULT_RETRY_WAIT`] without one), and either is sent once more; that is the
//!   only retry. The retry after a wait asks the session for its token again, never sends the one held before it: a
//!   session signed out during the wait answers its error, and nothing is sent. Any other answer that isn't a success
//!   is [`HttpError::Status`]. A write is never sent again here: its answer comes back whatever the status, for the
//!   outbox to decide, and after a 401 the session refreshes the refused token for the outbox's next attempt.
//! - **Tests** reach a mock only through [`HttpTransport::with_mock_origin`] (`test-support`): plain `http` to a
//!   loopback origin, the URL built and checked as above first.
//!
//! Nothing here logs a token: the `Authorization` header is marked sensitive, errors carry no token or body, and
//! [`Response`]'s `Debug` shows the body's length only.

use std::fmt;
use std::future::Future;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use percent_encoding::percent_decode_str;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, LOCATION};
use serde_json::Value;
use teams_auth::secrets::{Secret, SecretStore};
use teams_auth::session::Session;
use teams_auth::token::{TokenError, retry_after};
use teams_fence::{
    AFD_HOSTS, Channel, ChatCredential, CredentialFuture, CredentialSource, Fence, RequestOutcome, RequestRecord,
    chat_service_path,
};
use url::Url;

use crate::client::{Response, Transport};
use crate::endpoint::{Endpoint, Service};
use crate::own::OwnState;
use crate::read::Read;
use crate::write::{Authorized, Mutating};

/// The Entra audience of the token the chat service and the registrar take.
pub const IC3_AUDIENCE: &str = "https://ic3.teams.office.com";

/// Trouter's HTTP host.
pub const TROUTER_HOST: &str = "go.trouter.teams.microsoft.com";

/// The largest body read, in bytes; a longer one is [`HttpError::BodyTooLarge`].
pub const MAX_BODY: usize = 4 * 1024 * 1024;

/// How long connecting may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long one request may take, end to end.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The longest a throttled read or own-state request waits before it is sent again, by default.
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
}

impl<S: SecretStore> Credentials for Session<S> {
    async fn bearer(&self, audience: &str) -> Result<Secret, TokenError> {
        Ok(self.access_token(audience).await?.token)
    }

    async fn bearer_rejected(&self, audience: &str, rejected: &Secret) -> Result<Secret, TokenError> {
        Ok(self.refresh_rejected(audience, rejected).await?.token)
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
    /// An own-state request whose `fence.log` entry couldn't be written; it wasn't sent.
    #[error("fence.log can't be written, so the own-state request wasn't sent: {0}")]
    LogUnavailable(#[source] io::Error),
    /// The session couldn't give a token; nothing (more) was sent.
    #[error("no token for the request: {0}")]
    Credentials(#[source] TokenError),
    /// The request or its answer failed on the way.
    #[error("the request failed: {0}")]
    Failed(Failure),
    /// The answer's body is longer than [`MAX_BODY`].
    #[error("the answer's body is over {MAX_BODY} bytes")]
    BodyTooLarge,
    /// A read or own-state request answered with a status other than a success, after the retry the status allows.
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
    /// The URL built isn't on the host allowlist, or its path isn't exactly the base's and the endpoint's.
    #[error("the URL isn't on the transport's allowlist")]
    NotAllowed,
    /// A token that can't be sent as a header value.
    #[error("a token that can't be sent in a header")]
    Token,
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
    max_retry_wait: Duration,
    /// Tests only: every request goes to this origin instead, with the path and query it was built and checked with.
    /// Only [`HttpTransport::with_mock_origin`] (`test-support`) sets it; without that feature it is always `None`.
    mock_origin: Option<Url>,
}

impl<C> fmt::Debug for HttpTransport<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpTransport")
            .field("chat_base", &self.chat_base.as_str())
            .field("registrar_host", &self.registrar_host)
            .field("max_retry_wait", &self.max_retry_wait)
            .finish_non_exhaustive()
    }
}

/// One request, built and checked.
struct Outgoing<'a> {
    method: reqwest::Method,
    url: &'a Url,
    service: Service,
    body: Option<&'a Value>,
}

impl<C: Credentials> HttpTransport<C> {
    /// A transport for the account whose tokens `credentials` gives, logging own-state requests to `fence`'s log, with
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
            max_retry_wait: MAX_RETRY_WAIT,
            mock_origin: None,
        })
    }

    /// Another cap on the wait before a throttled read or own-state request is sent again.
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

    /// The URL of `endpoint` on `service`, checked against the allowlist.
    fn url_for(&self, service: Service, endpoint: &Endpoint) -> Result<Url, HttpError> {
        let refused = |refusal| HttpError::Refused(refusal);
        if endpoint
            .segments()
            .iter()
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        {
            return Err(refused(Refusal::Segment));
        }
        let mut url = match service {
            Service::ChatService if endpoint.segments().first().is_none_or(|first| first != "v1") => {
                return Err(refused(Refusal::ChatPath));
            }
            Service::ChatService => self.chat_base.clone(),
            Service::Registrar => Url::parse(&format!("https://{}/registrar", self.registrar_host))
                .map_err(|_| refused(Refusal::NotAllowed))?,
            Service::Trouter => {
                Url::parse(&format!("https://{TROUTER_HOST}/")).map_err(|_| refused(Refusal::NotAllowed))?
            }
        };
        url.path_segments_mut()
            .map_err(|()| refused(Refusal::NotAllowed))?
            .pop_if_empty()
            .extend(endpoint.segments());
        if !endpoint.query().is_empty() {
            url.query_pairs_mut().extend_pairs(endpoint.query());
        }
        if allowed(service, &url, endpoint) {
            Ok(url)
        } else {
            Err(refused(Refusal::NotAllowed))
        }
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

    /// Sends a read or own-state request (`own`: its kind, logged before every send), with the one retry a 401, 429 or
    /// 503 allows; any answer but a success is [`HttpError::Status`].
    async fn exchange(&self, outgoing: Outgoing<'_>, own: Option<&'static str>) -> Result<Response, HttpError> {
        let mut token = self.token(outgoing.service).await?;
        let first = self.attempt(&outgoing, own, token.as_ref()).await?;
        let retry = match (first.status(), audience(outgoing.service), &token) {
            (401, Some(audience), Some(refused)) => {
                let renewed = self.credentials.bearer_rejected(audience, refused).await;
                token = Some(renewed.map_err(HttpError::Credentials)?);
                true
            }
            (429 | 503, ..) => {
                let wait = first.retry_after().unwrap_or(DEFAULT_RETRY_WAIT);
                tokio::time::sleep(wait.min(self.max_retry_wait)).await;
                // the session may have signed out (or renewed) during the wait: the retry takes what it holds now
                token = self.token(outgoing.service).await?;
                true
            }
            _ => false,
        };
        let answer = if retry {
            self.attempt(&outgoing, own, token.as_ref()).await?
        } else {
            first
        };
        if answer.is_success() {
            Ok(answer)
        } else {
            Err(HttpError::Status(Box::new(answer)))
        }
    }

    /// One send; an own-state request is logged first, and not sent when it can't be.
    async fn attempt(
        &self,
        outgoing: &Outgoing<'_>,
        own: Option<&'static str>,
        token: Option<&Secret>,
    ) -> Result<Response, HttpError> {
        if let Some(kind) = own {
            let record = RequestRecord {
                channel: Channel::Http,
                method: outgoing.method.as_str(),
                url: outgoing.url.as_str(),
                outcome: RequestOutcome::Own(kind),
            };
            self.fence
                .log()
                .record_request(&record)
                .map_err(HttpError::LogUnavailable)?;
        }
        self.send(outgoing, token).await
    }

    /// One request with the service's headers, and its answer read.
    async fn send(&self, outgoing: &Outgoing<'_>, token: Option<&Secret>) -> Result<Response, HttpError> {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        if let Some(token) = token {
            let mut value = HeaderValue::from_str(&format!("Bearer {}", token.expose()))
                .map_err(|_| HttpError::Refused(Refusal::Token))?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
        for (name, value) in service_headers(outgoing.service) {
            headers.insert(HeaderName::from_static(name), HeaderValue::from_static(value));
        }
        let mut request = self
            .client
            .request(outgoing.method.clone(), self.destination(outgoing.url))
            .headers(headers);
        if let Some(body) = outgoing.body {
            request = request.header(CONTENT_TYPE, "application/json").body(body.to_string());
        }
        let answer = request.send().await.map_err(|error| failure(&error))?;
        read_answer(answer).await
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
            self.exchange(outgoing, None).await
        }
    }

    fn own_state<R: OwnState>(&self, request: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        let service = request.service();
        let method = request.method();
        let endpoint = request.endpoint();
        let body = request.body();
        async move {
            let url = self.url_for(service, &endpoint)?;
            let outgoing = Outgoing {
                method: method.to_reqwest(),
                url: &url,
                service,
                body: body.as_ref(),
            };
            self.exchange(outgoing, Some(R::KIND)).await
        }
    }

    async fn deliver<R: Mutating + Send>(&self, write: Authorized<R>) -> Result<Response, HttpError> {
        let wire = write.wire().ok_or(HttpError::NoRoute)?;
        let url = self.url_for(Service::ChatService, wire.endpoint())?;
        let token = self
            .credentials
            .bearer(IC3_AUDIENCE)
            .await
            .map_err(HttpError::Credentials)?;
        let outgoing = Outgoing {
            method: wire.method().to_reqwest(),
            url: &url,
            service: Service::ChatService,
            body: wire.body(),
        };
        let answer = self.send(&outgoing, Some(&token)).await?;
        if answer.status() == 401 {
            // the session refreshes the refused token for the outbox's next attempt, which also meets any failure of it
            let _ = self.credentials.bearer_rejected(IC3_AUDIENCE, &token).await;
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

/// The Entra audience of the bearer token `service` takes, if it takes one.
fn audience(service: Service) -> Option<&'static str> {
    match service {
        Service::ChatService | Service::Registrar => Some(IC3_AUDIENCE),
        Service::Trouter => None,
    }
}

/// The headers `service` takes besides the token, as the web client sends them.
fn service_headers(service: Service) -> &'static [(&'static str, &'static str)] {
    match service {
        Service::ChatService => &[("x-ms-migration", "True"), ("behavioroverride", "redirectAs404")],
        Service::Registrar => &[("x-ms-migration", "True"), ("x-ms-test-user", "False")],
        Service::Trouter => &[],
    }
}

/// Whether `url`, built for `endpoint` on `service`, is on the allowlist: `https`, no user info, port or fragment, an
/// allowed host, and a decoded path that is exactly the service's base path and the endpoint's segments.
fn allowed(service: Service, url: &Url, endpoint: &Endpoint) -> bool {
    let plain = url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none();
    let segments = endpoint.segments();
    plain
        && match service {
            Service::ChatService => chat_service_path(url).is_some_and(|rest| segments.get(1..) == Some(&rest[..])),
            Service::Registrar => {
                url.host_str().is_some_and(|host| AFD_HOSTS.contains(&host))
                    && decoded_path(url).is_some_and(|path| {
                        path.split_first()
                            .is_some_and(|(first, rest)| first == "registrar" && rest == segments)
                    })
            }
            Service::Trouter => {
                url.host_str() == Some(TROUTER_HOST) && decoded_path(url).is_some_and(|path| path == segments)
            }
        }
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

/// Reads an answer: its status, `Location`, `Retry-After`, `Content-Type`, and its body up to [`MAX_BODY`].
async fn read_answer(mut answer: reqwest::Response) -> Result<Response, HttpError> {
    let headers = answer.headers();
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
        if body.len() + chunk.len() > MAX_BODY {
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
    Ok(response)
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
