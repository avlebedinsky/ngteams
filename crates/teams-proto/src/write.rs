//! Mutating requests ([`Mutating`]) and the pairing of one with the permit issued for its target ([`Authorized`]).

use teams_fence::{Mri, Scope, Target, WriteKind, WritePermit};

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
    impl Sealed for super::StartCall {}
    impl Sealed for super::AddToCall {}
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

/// The conversation scenario of the web client's test call (Task 18, `docs/protocol/calling-echo.md`): the call start
/// that opens it names no callee.
pub const TEST_CALL_SCENARIO: &str = "UserInitiatedTestCall";

/// Places the test call to the echo service, as the web client does (Task 18): the call start opens a conversation of
/// the caller alone (no callee, scenario [`TEST_CALL_SCENARIO`]), and the caller then invites the echo service into
/// it with an [`AddToCall`]. The echo is counted in here anyway, so the fence lets the start through only for a call
/// it would let the echo join: the target is [`WriteKind::Call`] on [`Scope::New`] with the caller and the echo, the
/// same target the browser fence gives the web client's start (`proto_lab::calling::call_start`).
///
/// There is no request here that answers an incoming call ([`WriteKind::CallAnswer`]): a lab client never answers one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartCall {
    caller: Mri,
    echo: Mri,
}

impl StartCall {
    /// `caller`'s test call, which `echo` (the echo service) joins once the caller adds it.
    #[must_use]
    pub fn test_call(caller: Mri, echo: Mri) -> Self {
        Self { caller, echo }
    }

    /// The account placing the call.
    #[must_use]
    pub fn caller(&self) -> &Mri {
        &self.caller
    }

    /// The echo service the call is for. The call start doesn't name it: an [`AddToCall`] adds it.
    #[must_use]
    pub fn echo(&self) -> &Mri {
        &self.echo
    }
}

impl Mutating for StartCall {
    fn target(&self) -> Target {
        Target::new(WriteKind::Call, Scope::New).with_added([self.caller.clone(), self.echo.clone()])
    }
}

/// Invites someone into a call that exists (the conversation's `add`; how the test call brings the echo in). Its
/// target is [`WriteKind::Invite`] on [`Scope::Call`] (the conversation id from the call start's answer) with the
/// invitee added, so the fence judges the call's current members, read fresh and in full, together with the invitee.
/// No membership source can read a call yet (`docs/backlog/fence-call-follow-ups.md`): today the fence denies this
/// request as partial, whoever the invitee is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddToCall {
    call: String,
    invitee: Mri,
}

impl AddToCall {
    #[must_use]
    pub fn new(call: impl Into<String>, invitee: Mri) -> Self {
        Self {
            call: call.into(),
            invitee,
        }
    }

    /// The conversation id of the call.
    #[must_use]
    pub fn call(&self) -> &str {
        &self.call
    }

    /// Who is invited.
    #[must_use]
    pub fn invitee(&self) -> &Mri {
        &self.invitee
    }
}

impl Mutating for AddToCall {
    fn target(&self) -> Target {
        Target::new(WriteKind::Invite, Scope::Call { id: self.call.clone() }).with_added([self.invitee.clone()])
    }
}
