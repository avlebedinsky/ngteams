//! The client: every mutating call takes a [`WritePermit`] and goes through [`Authorized::new`] before it reaches the
//! [`Transport`], and every read by `POST` takes a [`PostReadPermit`] and goes through [`AuthorizedPostRead::new`]. A
//! transport takes requests of four classes only: a [`Read`], an [`OwnState`] request, an [`AuthorizedPostRead`], or
//! an [`Authorized`] write.

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::time::Duration;

use teams_fence::{PostReadPermit, WritePermit};

use crate::own::OwnState;
use crate::post_read::{AuthorizedPostRead, PostRead, PostReadMismatch};
use crate::read::Read;
use crate::write::{Authorized, Mutating, TargetMismatch};

/// Carries requests to the service, of four classes only: a [`Read`] (always a `GET`), an [`OwnState`] request (the
/// reviewed, sealed list), an [`AuthorizedPostRead`] (an entry of the sealed `PostRead` list, which it can't be handed
/// without the permit for its exact MRIs) and an [`Authorized`] write, which it can't be handed without the permit for
/// its target.
pub trait Transport: Sync {
    type Error: Error + Send + Sync + 'static;

    /// Sends a read as a `GET`.
    fn read<R: Read>(&self, request: &R) -> impl Future<Output = Result<Response, Self::Error>> + Send;

    /// Sends a reviewed own-state request.
    fn own_state<R: OwnState>(&self, request: &R) -> impl Future<Output = Result<Response, Self::Error>> + Send;

    /// Sends a permitted read by `POST`, once, logged to `fence.log` before it goes, and only in its entry's captured
    /// shape.
    fn post_read<R: PostRead + Send>(
        &self,
        read: AuthorizedPostRead<R>,
    ) -> impl Future<Output = Result<Response, Self::Error>> + Send;

    /// Sends an authorized write and returns the service's answer, whatever its status: what to do about a refusal is
    /// the caller's (the outbox's) decision.
    fn deliver<R: Mutating + Send>(
        &self,
        write: Authorized<R>,
    ) -> impl Future<Output = Result<Response, Self::Error>> + Send;
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

    /// Sends a reviewed own-state request.
    ///
    /// # Errors
    /// The transport's.
    pub async fn own_state<R: OwnState>(&self, request: &R) -> Result<Response, T::Error> {
        self.transport.own_state(request).await
    }

    /// Sends one read by `POST` under the permit `Fence::check_post_read` gave for its entry and its exact MRIs. The
    /// permit is consumed.
    ///
    /// # Errors
    /// [`PostReadError::Mismatch`] when the permit was given for another entry or other MRIs (checked before anything
    /// is sent), [`PostReadError::Transport`] when the transport refuses or fails it.
    pub async fn post_read<R: PostRead + Send>(
        &self,
        request: R,
        permit: PostReadPermit,
    ) -> Result<Response, PostReadError<T::Error>> {
        let read = AuthorizedPostRead::new(request, permit)?;
        self.transport.post_read(read).await.map_err(PostReadError::Transport)
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
