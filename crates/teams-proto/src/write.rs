//! Mutating requests ([`Mutating`]) and the pairing of one with the permit issued for its target ([`Authorized`]).

use serde_json::Value;
use teams_fence::{Mri, Scope, Target, WriteKind, WritePermit};
use url::Url;

use crate::endpoint::{Endpoint, Method};

mod chat;

pub use chat::{
    Author, CreateChat, DeleteMessage, EditMessage, Original, ReadReceipt, RemoveReaction, SendMessage, SetReaction,
    Typing,
};

/// A request others could notice. It names the one [`Target`] it writes to, and it reaches a transport only inside an
/// [`Authorized`], next to a [`WritePermit`] issued for exactly that target.
///
/// The trait is sealed: only this crate's own requests implement it. The permit check in [`Authorized::new`] holds
/// only as far as `target()` describes what the request really writes, so `target()` is computed from the request's
/// own immutable fields, and the wire request a transport sends is built from those same fields: its sealed [`Wire`]
/// form, which only this crate can make or read.
pub trait Mutating: sealed::Sealed {
    /// The target this request writes to: what the fence must have judged for the permit to cover it.
    fn target(&self) -> Target;
}

mod sealed {
    use super::Wire;

    /// The seal on [`Mutating`](super::Mutating): a public trait no other crate can name, implemented for this crate's
    /// requests only. It also carries the request's wire form, built from the same fields as its target.
    pub trait Sealed {
        /// What the transport sends for this request on the chat service; `None` for a request the native transport
        /// has no route for.
        fn wire(&self) -> Option<Wire>;
    }

    /// Calls go to the call controller and the conversation service, not the chat service: the native transport has no
    /// route for them before M3 (`docs/backlog/fence-call-follow-ups.md`).
    impl Sealed for super::StartCall {
        fn wire(&self) -> Option<Wire> {
            None
        }
    }

    /// As [`StartCall`](super::StartCall): no native route yet.
    impl Sealed for super::AddToCall {
        fn wire(&self) -> Option<Wire> {
            None
        }
    }
}

/// A mutating request as the transport sends it: its method, its endpoint under the chat base and its JSON body.
/// Sealed: only this crate's requests make one, from the same fields their [`Mutating::target`] comes from, and only
/// this crate reads one, so a permit for one target can't carry another request's wire form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wire {
    method: Method,
    endpoint: Endpoint,
    body: Option<Value>,
}

impl Wire {
    pub(crate) fn method(&self) -> Method {
        self.method
    }

    pub(crate) fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub(crate) fn body(&self) -> Option<&Value> {
        self.body.as_ref()
    }
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

    /// The request's wire form, for the transport.
    pub(crate) fn wire(&self) -> Option<Wire> {
        sealed::Sealed::wire(&self.request)
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

/// A call's conversation on the conversation service: the URL the call start's answer gives as its
/// `conversationController` (`https://<proxy>/…/conv/<conversation id>?<routing query>`). Writes on the call go to
/// this URL plus an operation, so it is also what the fence knows the call by: [`Scope::Call`]'s id is this URL as is,
/// host, path and query, and two calls on different hosts or routes never share an id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationUrl(Url);

impl ConversationUrl {
    /// `None` unless `url` is an `https` URL with a host, no user info, no fragment and a non-empty last path segment
    /// (the conversation id).
    #[must_use]
    pub fn parse(url: &str) -> Option<Self> {
        let url = Url::parse(url).ok()?;
        let has_id = url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .is_some_and(|id| !id.is_empty());
        let plain = url.username().is_empty() && url.password().is_none() && url.fragment().is_none();
        (url.scheme() == "https" && url.host_str().is_some() && plain && has_id).then_some(Self(url))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// The conversation id: the last path segment.
    #[must_use]
    pub fn id(&self) -> &str {
        self.0
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .unwrap_or_default()
    }

    /// The URL of the operation `name` on this conversation: its path plus `/<name>`, its query kept.
    fn operation(&self, name: &str) -> Url {
        let mut url = self.0.clone();
        url.set_path(&format!("{}/{name}", self.0.path()));
        url
    }
}

/// Invites someone into a call that exists (the conversation's `add`; how the test call brings the echo in). Its
/// target is [`WriteKind::Invite`] on [`Scope::Call`] (the call's [`ConversationUrl`] as is) with the invitee added,
/// so the fence judges the call's current members, read fresh and in full, together with the invitee. The request is
/// posted to [`AddToCall::url`], which comes from the same conversation URL, so a permit for one call never carries an
/// `add` to another. No membership source can read a call yet (`docs/backlog/fence-call-follow-ups.md`): today the
/// fence denies this request as partial, whoever the invitee is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddToCall {
    conversation: ConversationUrl,
    invitee: Mri,
}

impl AddToCall {
    #[must_use]
    pub fn new(conversation: ConversationUrl, invitee: Mri) -> Self {
        Self { conversation, invitee }
    }

    /// The call's conversation.
    #[must_use]
    pub fn conversation(&self) -> &ConversationUrl {
        &self.conversation
    }

    /// Who is invited.
    #[must_use]
    pub fn invitee(&self) -> &Mri {
        &self.invitee
    }

    /// Where the `add` is posted, as the web client posts it: the conversation's path plus `/add`, its query kept (not
    /// the answer's `addParticipant` link). A transport posts the request here and nowhere else.
    #[must_use]
    pub fn url(&self) -> Url {
        self.conversation.operation("add")
    }
}

impl Mutating for AddToCall {
    fn target(&self) -> Target {
        Target::new(
            WriteKind::Invite,
            Scope::Call {
                id: self.conversation.as_str().to_owned(),
            },
        )
        .with_added([self.invitee.clone()])
    }
}
