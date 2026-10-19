//! Mutating requests ([`Mutating`]) and the pairing of one with the permit issued for its target ([`Authorized`]).

use teams_fence::{Scope, Target, WriteKind, WritePermit};

/// A request others could notice. It names the one [`Target`] it writes to, and it reaches a transport only inside an
/// [`Authorized`], next to a [`WritePermit`] issued for exactly that target.
///
/// The trait is sealed: only this crate's own requests implement it. The permit check in [`Authorized::new`] holds
/// only as far as `target()` describes what the request really writes, so `target()` is computed from the request's
/// own immutable fields, and the wire request a transport sends must be built from those same fields.
pub trait Mutating: sealed::Sealed {
    /// The target this request writes to: what the fence must have judged for the permit to cover it.
    fn target(&self) -> Target;
}

mod sealed {
    /// The seal on [`Mutating`](super::Mutating): a public trait no other crate can name, implemented for this crate's
    /// requests only.
    pub trait Sealed {}

    impl Sealed for super::SendMessage {}
}

/// A mutating request together with the permit for its target. The only way to make one is [`Authorized::new`],
/// which checks at runtime that the permit was issued for the request's own target, and it consumes the permit, so
/// one permit covers one write.
#[derive(Debug)]
pub struct Authorized<R> {
    request: R,
    permit: WritePermit,
}

impl<R: Mutating> Authorized<R> {
    /// Pairs `request` with `permit`.
    ///
    /// # Errors
    /// [`TargetMismatch`] when the permit was issued for any other target: another conversation, another kind of
    /// write, other added participants. The permit is dropped with it.
    pub fn new(request: R, permit: WritePermit) -> Result<Self, TargetMismatch> {
        let requested = request.target();
        if *permit.target() == requested {
            Ok(Self { request, permit })
        } else {
            Err(TargetMismatch {
                permitted: Box::new(permit.target().clone()),
                requested: Box::new(requested),
            })
        }
    }

    #[must_use]
    pub fn request(&self) -> &R {
        &self.request
    }

    #[must_use]
    pub fn permit(&self) -> &WritePermit {
        &self.permit
    }
}

/// A permit used for a request it wasn't issued for. Its `Display` and `Debug` show both targets the way [`Target`]
/// does: scope ids as they are, added participants as a count. Both targets are boxed to keep the error small.
#[derive(Debug, thiserror::Error)]
#[error("the permit for {permitted} doesn't cover {requested}")]
pub struct TargetMismatch {
    permitted: Box<Target>,
    requested: Box<Target>,
}

impl TargetMismatch {
    /// The target the permit was issued for.
    #[must_use]
    pub fn permitted(&self) -> &Target {
        &self.permitted
    }

    /// The target the request writes to.
    #[must_use]
    pub fn requested(&self) -> &Target {
        &self.requested
    }
}

/// Posts a message to an existing chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendMessage {
    conversation: String,
    content: String,
}

impl SendMessage {
    #[must_use]
    pub fn new(conversation: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            conversation: conversation.into(),
            content: content.into(),
        }
    }

    /// The conversation id the message goes to.
    #[must_use]
    pub fn conversation(&self) -> &str {
        &self.conversation
    }

    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }
}

impl Mutating for SendMessage {
    fn target(&self) -> Target {
        Target::new(
            WriteKind::Message,
            Scope::Conversation {
                id: self.conversation.clone(),
            },
        )
    }
}
