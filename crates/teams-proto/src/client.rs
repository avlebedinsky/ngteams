//! The client: every mutating call takes a [`WritePermit`] and goes through [`Authorized::new`] before it reaches the
//! [`Transport`].

use std::error::Error;
use std::future::Future;

use teams_fence::WritePermit;

use crate::write::{Authorized, Mutating, TargetMismatch};

/// Carries authorized writes to the service. It only ever receives an [`Authorized`] write, so it can't be handed a
/// request without the permit for its target.
pub trait Transport: Sync {
    type Error: Error + Send + Sync + 'static;

    fn deliver<R: Mutating + Send>(&self, write: Authorized<R>)
    -> impl Future<Output = Result<(), Self::Error>> + Send;
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

    /// Sends one mutating request under the permit `teams-fence` issued for its target. The permit is consumed.
    ///
    /// # Errors
    /// [`WriteError::TargetMismatch`] when the permit was issued for another target (checked before anything is
    /// sent), [`WriteError::Transport`] when delivery fails.
    pub async fn write<R: Mutating + Send>(&self, request: R, permit: WritePermit) -> Result<(), WriteError<T::Error>> {
        let write = Authorized::new(request, permit)?;
        self.transport.deliver(write).await.map_err(WriteError::Transport)
    }
}
