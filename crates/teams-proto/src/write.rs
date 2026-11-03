//! Mutating requests ([`Mutating`]) and the pairing of one with the permit issued for its target ([`Authorized`]).

use serde_json::Value;
use teams_fence::{Target, WritePermit};

use crate::endpoint::{Endpoint, Method, Service};

mod availability;
mod calendar;
mod call;
mod channel;
mod chat;
mod team;

pub use availability::{ForceAvailability, ManualAvailability, SetEndpointAvailability};
pub use calendar::{
    CancelChannelEvent, CancelEvent, CreateChannelEvent, CreateEvent, CreateOnlineMeeting, DeleteEvent, EventAttendee,
    EventDraft, EventShowAs, MeetingChannel, Organizer, UpdateEvent, ZonedTime,
};
pub use call::{
    AcceptCall, AcceptParts, AcknowledgeAcceptance, AcknowledgeMediaAnswer, AddParts, AddToCall, AnswerMedia,
    AnswerMediaParts, ApplyChannelParameters, AttachCall, AttachParts, CALL_START_URL, CALLING_BASE, CALLING_HOST,
    CallContext, CallLink, CallParticipant, CallWriteError, ChannelParameters, ClientInfo, DecideLobby, DeclineCall,
    Invitee, JoinCall, JoinMedia, JoinParts, KeepAliveCall, LeaveCall, LeaveReason, Links, MediaContent,
    MediaDescription, MediaDescriptions, MeetingInfo, RenegotiateMedia, ReportRinging, StartCall, StartParts,
    TEAMS_ECHO_BOT_MRI, TEST_CALL_SCENARIO, UpdateEndpointMetadata, UpdateEndpointState, UpdateMediaDescriptions,
    UpdateParticipantProperties, VoiceConsent,
};
pub(crate) use call::{CallWire, Registry, on_calling_base};
pub use channel::{
    AddChannelMembers, ChannelCreated, ChannelLayout, CreateChannel, NewChannel, NewChannelKind, ShowChannelByDefault,
    channel_error_code,
};
pub use chat::{
    Author, CreateChat, DeleteMessage, EditMessage, Original, ReadReceipt, RemoveReaction, SendMessage, SetReaction,
    Typing,
};
pub use team::{
    AppDefinition, MemberUpdate, NewTeam, TeamCopy, TeamRole, TeamWrite, TemplateTeam, team_created, team_error_code,
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
    use super::call::CallWire;

    /// The seal on [`Mutating`](super::Mutating): a public trait no other crate can name, implemented for this crate's
    /// requests only. It also carries the request's wire form, built from the same fields as its target.
    pub trait Sealed {
        /// What the transport sends for this request on a service base; `None` for a call write or a request the
        /// native transport has no route for.
        fn wire(&self) -> Option<Wire>;

        /// A call write's wire form, its exact URL and body as the fence classified them (`super::call`); `None` for
        /// any other request.
        fn call(&self) -> Option<&CallWire> {
            None
        }
    }
}

/// A mutating request as the transport sends it: the service it goes to (the chat service, or UPS for the account's
/// own availability), its method, its endpoint under that service's base, the headers it adds to the service's own
/// (the presence writes' `x-ms-endpoint-id`) and its JSON body, or its raw bytes (a team's picture). Sealed: only this
/// crate's requests make one, from the same fields their [`Mutating::target`] comes from, and only this crate reads one,
/// so a permit for one target can't carry another request's wire form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wire {
    service: Service,
    method: Method,
    endpoint: Endpoint,
    /// By lower-cased name, none of them a credential.
    headers: Vec<(&'static str, String)>,
    body: Option<Value>,
    /// A body of bytes sent as they are, in place of `body`: an image. The target never comes from it.
    raw: Option<RawBody>,
}

/// A write's body of bytes (a picture). `Debug` shows only its length.
#[derive(Clone, PartialEq, Eq)]
pub struct RawBody(std::sync::Arc<[u8]>);

impl RawBody {
    /// The bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for RawBody {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes.into())
    }
}

impl std::fmt::Debug for RawBody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "RawBody({} bytes)", self.0.len())
    }
}

impl Wire {
    /// The bytes it sends: its raw body, or its JSON body as text.
    pub(crate) fn body_bytes(&self) -> Option<Vec<u8>> {
        self.raw
            .as_ref()
            .map(|raw| raw.bytes().to_vec())
            .or_else(|| self.body.as_ref().map(|body| body.to_string().into_bytes()))
    }

    pub(crate) fn service(&self) -> Service {
        self.service
    }

    pub(crate) fn headers(&self) -> &[(&'static str, String)] {
        &self.headers
    }

    pub(crate) fn method(&self) -> Method {
        self.method
    }

    pub(crate) fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    #[cfg(test)]
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

    /// A call write's wire form, for the transport.
    pub(crate) fn call(&self) -> Option<&call::CallWire> {
        sealed::Sealed::call(&self.request)
    }
}

/// The method and the endpoint or URL a mutating request would go to, for the transport's read-only refusal
/// (`HttpTransport::read_only`): a call write's exact URL, else its wire form; `None` for neither.
pub(crate) fn wire_or_call<R: Mutating>(request: &R) -> Option<Result<Wire, (Method, url::Url)>> {
    if let Some(call) = sealed::Sealed::call(request) {
        return Some(Err((call.method(), call.url().clone())));
    }
    sealed::Sealed::wire(request).map(Ok)
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
