//! The client: every contacting write takes a [`WritePermit`] and goes through [`Authorized::new`] before it reaches
//! the [`Transport`]. A transport takes requests of three classes only: a [`Read`] (always a `GET`), a plain
//! [`Request`] (sent only when [`teams_fence::contacting`] says it reaches nobody), or an [`Authorized`] write.

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use teams_fence::{Fence, Mri, PostReadPermit, WritePermit};

use crate::http::{Credentials, HttpError, HttpTransport, Refusal};
use crate::own::OwnState;
use crate::post_read::{AuthorizedPostRead, PostRead, PostReadMismatch};
use crate::read::Read;
use crate::request::Request;
use crate::write::{Authorized, Mutating, TargetMismatch};

/// Carries requests to the service, of three classes only: a [`Read`] (always a `GET`), a plain [`Request`] (any
/// method, which the transport sends only when [`teams_fence::contacting`] calls it [`teams_fence::Contact::None`],
/// and refuses otherwise before anything is sent) and an [`Authorized`] write, which it can't be handed without the
/// permit for its target.
pub trait Transport: Sync {
    type Error: Error + Send + Sync + 'static;

    /// Sends a read as a `GET`.
    fn read<R: Read>(&self, request: &R) -> impl Future<Output = Result<Response, Self::Error>> + Send;

    /// Sends a plain request when it reaches nobody ([`teams_fence::contacting`] calls it
    /// [`teams_fence::Contact::None`]), logged to `fence.log` as `allowed` before it goes when it is mutating; refuses
    /// a write or a request the classifier can't read, before anything is sent.
    fn send<R: Request>(&self, request: &R) -> impl Future<Output = Result<Response, Self::Error>> + Send;

    /// Sends an entry of the sealed own-state list that is also a plain request (the registration): as a plain request
    /// ([`Transport::send`]) by default and through a lab fence; through the personal build's fence, as its own-state
    /// entry, logged by its kind ([`HttpTransport::own_state`]).
    fn send_own<R: OwnState + Request + Sync>(
        &self,
        request: &R,
    ) -> impl Future<Output = Result<Response, Self::Error>> + Send {
        self.send(request)
    }

    /// Sends an authorized write and returns the service's answer, whatever its status: what to do about a refusal is
    /// the caller's (the outbox's) decision.
    fn deliver<R: Mutating + Send>(
        &self,
        write: Authorized<R>,
    ) -> impl Future<Output = Result<Response, Self::Error>> + Send;

    /// The account this transport sends for, as its fence owns it ([`teams_fence::Fence::own_account`]): the account an
    /// endpoint registered through it is recorded for ([`crate::register`], [`crate::RegisteredEndpoint::account`]).
    /// `None` (the default) when it owns none. Only an [`EndpointRegistrar`](crate::EndpointRegistrar)'s is ever read,
    /// and outside `test-support` that is the native transport's alone, so another transport's answer here can't make
    /// an endpoint any account's.
    fn own_account(&self) -> Option<&Mri> {
        None
    }

    /// The fence whose log the push channel's frames go to before they are sent ([`crate::FrameSink::logging`]): the
    /// native transport's when its fence is the personal build's; `None` (the default, and every lab transport's) for a
    /// sink that logs none.
    fn frame_log(&self) -> Option<Arc<Fence>> {
        None
    }
}

/// A service's answer: its status, the few headers a caller acts on, and the body (capped by the transport that read
/// it). `Debug` shows the body's length only: a body may hold message text.
#[derive(Clone, PartialEq, Eq)]
pub struct Response {
    status: u16,
    location: Option<String>,
    retry_after: Option<Duration>,
    content_type: Option<String>,
    body: Vec<u8>,
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("location", &self.location)
            .field("retry_after", &self.retry_after)
            .field("content_type", &self.content_type)
            .field("body_len", &self.body.len())
            .finish()
    }
}

impl Response {
    /// An answer with `status` and `body` and none of the headers.
    #[must_use]
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            location: None,
            retry_after: None,
            content_type: None,
            body: body.into(),
        }
    }

    #[must_use]
    pub fn with_location(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    #[must_use]
    pub fn with_retry_after(mut self, wait: Duration) -> Self {
        self.retry_after = Some(wait);
        self
    }

    #[must_use]
    pub fn with_content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }

    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }

    /// A 2xx status.
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// `Location`: where a message post's new message is, or where a region hop points (never followed).
    #[must_use]
    pub fn location(&self) -> Option<&str> {
        self.location.as_deref()
    }

    /// `Retry-After`, as a wait from when the answer was read.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

/// Why a write didn't go out.
#[derive(Debug, thiserror::Error)]
pub enum WriteError<E> {
    /// The permit was issued for another target; nothing reached the transport.
    #[error(transparent)]
    TargetMismatch(#[from] TargetMismatch),
    /// The transport failed to deliver the write.
    #[error("the write could not be delivered: {0}")]
    Transport(#[source] E),
}

/// Teams' internal API over a [`Transport`].
#[derive(Debug)]
pub struct Client<T> {
    transport: T,
}

impl<T: Transport> Client<T> {
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Sends a read.
    ///
    /// # Errors
    /// The transport's.
    pub async fn read<R: Read>(&self, request: &R) -> Result<Response, T::Error> {
        self.transport.read(request).await
    }

    /// Sends a plain request, when it reaches nobody (see [`Transport::send`]).
    ///
    /// # Errors
    /// The transport's, a refusal of a write or of a request the fence's classifier can't read included.
    pub async fn send<R: Request>(&self, request: &R) -> Result<Response, T::Error> {
        self.transport.send(request).await
    }

    /// Sends one mutating request under the permit `teams-fence` issued for its target, and returns the service's
    /// answer. The permit is consumed.
    ///
    /// # Errors
    /// [`WriteError::TargetMismatch`] when the permit was issued for another target (checked before anything is
    /// sent), [`WriteError::Transport`] when delivery fails.
    pub async fn write<R: Mutating + Send>(
        &self,
        request: R,
        permit: WritePermit,
    ) -> Result<Response, WriteError<T::Error>> {
        let write = Authorized::new(request, permit)?;
        self.transport.deliver(write).await.map_err(WriteError::Transport)
    }
}

/// The sealed lists the personal build sends its own-state requests and reads by `POST` from (`docs/personal-build.md`):
/// over the native transport only, each logged to `fence.log` by its entry's kind before it goes.
impl<C: Credentials> Client<HttpTransport<C>> {
    /// Sends an entry of the sealed own-state list ([`HttpTransport::own_state`]).
    ///
    /// # Errors
    /// The transport's.
    pub async fn own_state<R: OwnState>(&self, request: &R) -> Result<Response, HttpError> {
        self.transport.own_state(request).await
    }

    /// Sends a read by `POST` of the sealed list: through a lab fence as a plain request ([`Transport::send`], whoever it
    /// names); through the personal build's fence only under the permit [`teams_fence::Fence::check_post_read_among`]
    /// gives for its exact MRIs against the people of the account's chats ([`HttpTransport::chat_people`]), once, in its
    /// entry's captured shape ([`HttpTransport::post_read`]).
    ///
    /// # Errors
    /// The transport's; [`HttpError::Refused`] with [`Refusal::PostReadDenied`] when the personal fence denied it
    /// (nothing sent), [`HttpError::Unknown`] when the permit doesn't match it.
    pub async fn lookup<R: PostRead + Request + Send + Sync>(&self, request: R) -> Result<Response, HttpError> {
        if !self.transport.is_personal() {
            return self.transport.send(&request).await;
        }
        let permit = self
            .transport
            .fence()
            .check_post_read_among(R::KIND, request.names(), self.transport.chat_people())
            .map_err(|_| HttpError::Refused(Refusal::PostReadDenied))?;
        self.post_read(request, permit).await.map_err(|error| match error {
            PostReadError::Mismatch(_) => HttpError::Unknown,
            PostReadError::Transport(error) => error,
        })
    }

    /// Sends one read by `POST` under the permit `Fence::check_post_read` gave for its entry and its exact MRIs
    /// ([`HttpTransport::post_read`]). The permit is consumed.
    ///
    /// # Errors
    /// [`PostReadError::Mismatch`] when the permit was given for another entry or other MRIs (checked before anything
    /// is sent), [`PostReadError::Transport`] when the transport refuses or fails it.
    pub async fn post_read<R: PostRead + Send>(
        &self,
        request: R,
        permit: PostReadPermit,
    ) -> Result<Response, PostReadError<HttpError>> {
        let read = AuthorizedPostRead::new(request, permit)?;
        self.transport.post_read(read).await.map_err(PostReadError::Transport)
    }
}

/// Why a read by `POST` didn't go out or come back.
#[derive(Debug, thiserror::Error)]
pub enum PostReadError<E> {
    /// The permit was given for another entry or other MRIs; nothing reached the transport.
    #[error(transparent)]
    Mismatch(#[from] PostReadMismatch),
    /// The transport refused or failed it.
    #[error("the read by POST failed: {0}")]
    Transport(#[source] E),
}
