//! The call writes (M3 Task 11): each request a call makes, a sealed [`Mutating`] type built in the shape the web
//! client's captures show (`docs/protocol/calling-echo.md`, `docs/protocol/calling.md`), sent only as
//! [`Authorized<_>`](super::Authorized) through the calling service ([`Service::Calling`](crate::Service::Calling)).
//!
//! **The target comes from the bytes.** A call write is built whole when it is made: its method, its exact URL (the
//! call start's [`CALL_START_URL`], or a [`CallLink`] the call's services handed out, kept byte for byte), its headers
//! ([`CallContext`]) and its JSON body. The constructor then asks [`teams_fence::contacting`], the lab rule's one
//! classifier, what that request is, and keeps the [`Target`] it gives; a request the classifier doesn't map
//! ([`CallWriteError::Unmapped`]), or maps as another write than the type stands for ([`CallWriteError::OtherWrite`]),
//! isn't made. So a native call write's target is, by construction, what the browser fence decides for the same bytes:
//!
//! - [`StartCall`]: the test call's start (`WriteKind::Call` on `Scope::New`, the caller and the echo service) or a
//!   chat's call start (`WriteKind::Call` on the chat it carries, the caller and every callee);
//! - every other write: `Scope::CallOperation` with its operation, its exact URL, the sender its body names, the
//!   callbacks it hands out, the conversation an `attach` or a join subscribes to, and an `add`'s invitee, which the
//!   fence's call registry resolves.
//!
//! The transport sends exactly what was classified, never resent. A start and a join with media make their call
//! pending in the fence's registry before they go and register it from their answer; an `attach` and an `accept` bind
//! the links their answer hands out (`HttpTransport::deliver`).

mod parts;

use std::fmt;

use serde_json::{Map, Value, json};
use teams_fence::{CallOperation, Contact, Mri, Policy, Request as FenceRequest, Scope, Target, WriteKind, contacting};
use url::Url;

pub(crate) use self::parts::on_calling_base;
pub use self::parts::{
    CALL_START_URL, CALLING_BASE, CALLING_HOST, CallContext, CallLink, CallParticipant, ChannelParameters, ClientInfo,
    Invitee, Links, MediaContent, MediaDescription, MediaDescriptions,
};

use super::sealed::Sealed;
use super::{Mutating, Wire};
use crate::endpoint::Method;

/// Why a call write wasn't made. Nothing was sent.
#[derive(Debug, thiserror::Error)]
pub enum CallWriteError {
    /// The fence's classifier doesn't map the request built from these parts: a value outside its captured shape (a
    /// callback name nobody reviewed, a callee that isn't a work account, a test call naming a chat, a link of another
    /// operation, …).
    #[error("the request isn't in a captured shape the fence maps")]
    Unmapped,
    /// The classifier reads the request as another write than the type stands for.
    #[error("the fence reads the request as another write")]
    OtherWrite,
    /// The request's message id couldn't be made.
    #[error("the system's random source failed: {0}")]
    Random(getrandom::Error),
}

/// What the transport does with the fence's call registry around the send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Registry {
    Nothing,
    /// A call start (or a join with media): pended before it goes, registered from its answer.
    Start,
    /// An `attach` or an `accept`: its answer's links bound.
    Answer,
}

/// A call write as the transport sends it: method, exact URL, headers (by lower-cased name, none of them a
/// credential), body, and its registry step. Only this module makes one.
#[derive(Clone, PartialEq, Eq)]
pub struct CallWire {
    method: Method,
    url: Url,
    headers: Vec<(&'static str, String)>,
    body: Option<Value>,
    registry: Registry,
}

impl CallWire {
    pub(crate) fn method(&self) -> Method {
        self.method
    }

    pub(crate) fn url(&self) -> &Url {
        &self.url
    }

    pub(crate) fn headers(&self) -> &[(&'static str, String)] {
        &self.headers
    }

    /// The body's bytes, exactly as classified and as sent.
    pub(crate) fn body_bytes(&self) -> Option<Vec<u8>> {
        self.body.as_ref().map(body_bytes)
    }

    pub(crate) fn registry(&self) -> Registry {
        self.registry
    }
}

/// `Debug` shows the method, the host and path (never the query) and the body's length.
impl fmt::Debug for CallWire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallWire")
            .field("method", &self.method)
            .field(
                "url",
                &format_args!("{}{}", self.url.host_str().unwrap_or_default(), self.url.path()),
            )
            .field("body_len", &self.body_bytes().map(|body| body.len()))
            .finish_non_exhaustive()
    }
}

/// A JSON body as sent.
fn body_bytes(body: &Value) -> Vec<u8> {
    body.to_string().into_bytes()
}

/// A built call write: its target, as classified, and its wire.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Built {
    target: Target,
    wire: CallWire,
}

/// The write a built request must classify as.
#[derive(Clone, Copy)]
enum Expect {
    /// The test call's start: `Call` on `Scope::New`.
    TestStart,
    /// A chat's call start: `Call` on `Scope::Conversation`.
    ChatStart,
    /// This operation on `Scope::CallOperation`.
    Operation(CallOperation),
}

impl Expect {
    fn fits(self, target: &Target) -> bool {
        match (self, target.scope()) {
            (Self::TestStart, Scope::New) | (Self::ChatStart, Scope::Conversation { .. }) => {
                target.kind() == WriteKind::Call
            }
            (Self::Operation(expected), Scope::CallOperation { operation, .. }) => {
                *operation == expected && target.kind() == expected.kind()
            }
            _ => false,
        }
    }
}

/// One request to build: what is sent, and what it must classify as.
struct Plan<'a> {
    context: &'a CallContext,
    method: Method,
    url: &'a Url,
    body: Option<Value>,
    expect: Expect,
    registry: Registry,
    /// The echo service, for the test call's start.
    echo: Option<&'a Mri>,
}

/// Builds `plan`'s request and classifies it (see the module docs).
fn build(plan: Plan<'_>) -> Result<Built, CallWriteError> {
    let bytes = plan.body.as_ref().map(body_bytes);
    let request = FenceRequest::new(plan.method.as_str(), plan.url.as_str()).with_body(bytes.as_deref());
    let policy = plan
        .echo
        .map_or_else(Policy::new, |echo| Policy::new().with_echo_callee(echo.clone()));
    let Contact::Write(target) = contacting(&request, &policy) else {
        return Err(CallWriteError::Unmapped);
    };
    if !plan.expect.fits(&target) {
        return Err(CallWriteError::OtherWrite);
    }
    let headers = plan.context.headers().map_err(CallWriteError::Random)?;
    Ok(Built {
        target,
        wire: CallWire {
            method: plan.method,
            url: plan.url.clone(),
            headers,
            body: plan.body,
            registry: plan.registry,
        },
    })
}

/// [`build`] of an operation on a link, with no registry step.
fn operation(
    context: &CallContext,
    operation: CallOperation,
    link: &CallLink,
    body: Option<Value>,
) -> Result<Built, CallWriteError> {
    let method = match operation.method() {
        "PUT" => Method::Put,
        "DELETE" => Method::Delete,
        _ => Method::Post,
    };
    build(Plan {
        context,
        method,
        url: link.url(),
        body,
        expect: Expect::Operation(operation),
        registry: Registry::Nothing,
        echo: None,
    })
}

/// The properties of the conversation a start or a join opens, as every capture sent them.
fn conversation_properties() -> Value {
    json!({
        "allowConversationWithoutHost": true,
        "enableGroupCallEventMessages": true,
        "enableGroupCallUpgradeMessage": false,
        "enableGroupCallMeetupGeneration": false,
    })
}

/// The endpoint's state as a start, a join and an answer report it.
fn endpoint_state(sequence: u64) -> Value {
    json!({
        "endpointStateSequenceNumber": sequence,
        "endpointProperties": {"additionalEndpointProperties": {"infoShownInReportMode": "FullInformation"}},
    })
}

/// What a call start sends besides its kind (the test call or a chat's): who calls, the endpoint's callbacks for the
/// conversation (its roster's and its `links`), for the call (`progress`, `mediaAnswer`, `acceptance`, `redirection`,
/// `end`) and for the media controller (`controlVideoStreaming`, `csrcInfo`), the call's modalities and offer, and what
/// the client says of itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartParts {
    pub from: CallParticipant,
    pub roster_update: String,
    pub conversation_links: Links,
    pub call_links: Links,
    pub media_controller_links: Links,
    pub modalities: Vec<String>,
    pub media: MediaContent,
    pub client: ClientInfo,
}

/// The call invitation of a start or a join: its modalities, callbacks, offer and the client's description.
fn call_invitation(
    modalities: &[String],
    call_links: &Links,
    media_controller_links: &Links,
    media: &MediaContent,
    client: &ClientInfo,
) -> Value {
    let mut invitation = Map::new();
    invitation.insert("callModalities".to_owned(), json!(modalities));
    for key in ["replaces", "transferor", "clientTransferContext", "customContext"] {
        invitation.insert(key.to_owned(), Value::Null);
    }
    invitation.insert("links".to_owned(), call_links.json());
    if !media_controller_links.is_empty() {
        invitation.insert(
            "clientContentForMediaController".to_owned(),
            media_controller_links.json(),
        );
    }
    if let Some(pstn) = client.pstn_content() {
        invitation.insert("pstnContent".to_owned(), pstn);
    }
    invitation.insert("emergencyContent".to_owned(), Value::Null);
    invitation.insert("mediaContent".to_owned(), media.json());
    invitation.insert("voicemailSettings".to_owned(), json!({}));
    for key in ["locationContent", "networkContent", "areaContent"] {
        invitation.insert(key.to_owned(), Value::Null);
    }
    Value::Object(invitation)
}

/// A call start's body (`docs/protocol/calling.md`, "The start"): the test call's with its `scenario`, a chat's with
/// none.
fn start_body(parts: &StartParts, scenario: Option<&str>, to: &[Value], group_chat: Value) -> Value {
    let mut request = Map::new();
    request.insert("conversationType".to_owned(), Value::Null);
    request.insert("subject".to_owned(), Value::Null);
    request.insert("suppressDialout".to_owned(), json!(scenario.is_some()));
    request.insert(
        "roster".to_owned(),
        json!({"type": "Delta", "rosterUpdate": parts.roster_update}),
    );
    request.insert("properties".to_owned(), conversation_properties());
    request.insert("links".to_owned(), parts.conversation_links.json());
    if let Some(scenario) = scenario {
        request.insert("scenario".to_owned(), json!(scenario));
    }
    let mut body = Map::new();
    body.insert("conversationRequest".to_owned(), Value::Object(request));
    body.insert("contentSharing".to_owned(), Value::Null);
    body.insert("participants".to_owned(), json!({"from": parts.from.json(), "to": to}));
    parts.client.add_capabilities(&mut body, true);
    body.insert("endpointMetadata".to_owned(), json!({}));
    for key in ["groupContext", "meetingInfo", "meetingData"] {
        body.insert(key.to_owned(), Value::Null);
    }
    body.insert("groupChat".to_owned(), group_chat);
    body.insert("endpointState".to_owned(), endpoint_state(1));
    let invitation = call_invitation(
        &parts.modalities,
        &parts.call_links,
        &parts.media_controller_links,
        &parts.media,
        &parts.client,
    );
    body.insert("callInvitation".to_owned(), invitation);
    Value::Object(body)
}

/// Starts a call: `POST` [`CALL_START_URL`].
///
/// - [`StartCall::echo_test`]: the test call, as the web client places it: a conversation of the caller alone (no
///   callee, the scenario [`TEST_CALL_SCENARIO`]), into which the caller then adds the echo service ([`AddToCall`]);
///   its target counts the echo in (`WriteKind::Call` on `Scope::New`, the caller and the echo), so the fence lets it
///   through only for a call it would let the echo join.
/// - [`StartCall::in_chat`]: a call from a 1:1 or group chat: its callees in `participants.to`, the group chat in
///   `groupChat` (`null` for a 1:1 call); its target is `WriteKind::Call` on the chat it carries (the group chat, or the
///   1:1 chat of the caller and its one callee), the caller and every callee added, so the fence reads that chat fresh.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartCall {
    caller: Mri,
    echo: Option<Mri>,
    target: Target,
    built: Built,
}

impl StartCall {
    /// The test call of `parts.from` for `echo` (see the type's docs).
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't the test call's start in its captured shape.
    pub fn echo_test(context: &CallContext, parts: &StartParts, echo: &Mri) -> Result<Self, CallWriteError> {
        let body = start_body(parts, Some(TEST_CALL_SCENARIO), &[], Value::Null);
        Self::start(context, parts, body, Expect::TestStart, Some(echo))
    }

    /// A call from a chat to `callees`: a group chat's (`group_chat`, its id) or, with none, the 1:1 chat of the caller
    /// and its one callee (see the type's docs).
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a chat's call start in its captured shape (no callee, a callee twice
    /// or the caller, an MRI that isn't a work account, a group chat id that isn't one, two callees and no group chat).
    pub fn in_chat(
        context: &CallContext,
        parts: &StartParts,
        callees: &[Invitee],
        group_chat: Option<&str>,
    ) -> Result<Self, CallWriteError> {
        let to: Vec<Value> = callees.iter().map(Invitee::json).collect();
        let chat = group_chat.map_or(Value::Null, |id| json!({"threadId": id, "messageId": null}));
        let body = start_body(parts, None, &to, chat);
        Self::start(context, parts, body, Expect::ChatStart, None)
    }

    fn start(
        context: &CallContext,
        parts: &StartParts,
        body: Value,
        expect: Expect,
        echo: Option<&Mri>,
    ) -> Result<Self, CallWriteError> {
        let url = Url::parse(CALL_START_URL).map_err(|_| CallWriteError::Unmapped)?;
        let built = build(Plan {
            context,
            method: Method::Post,
            url: &url,
            body: Some(body),
            expect,
            registry: Registry::Start,
            echo,
        })?;
        Ok(Self {
            caller: parts.from.mri.clone(),
            echo: echo.cloned(),
            target: built.target.clone(),
            built,
        })
    }

    /// The account placing the call.
    #[must_use]
    pub fn caller(&self) -> &Mri {
        &self.caller
    }

    /// The echo service a test call is for (the start doesn't name it: an [`AddToCall`] adds it); `None` for a chat's
    /// call.
    #[must_use]
    pub fn echo(&self) -> Option<&Mri> {
        self.echo.as_ref()
    }
}

impl Mutating for StartCall {
    fn target(&self) -> Target {
        self.target.clone()
    }
}

impl Sealed for StartCall {
    fn wire(&self) -> Option<Wire> {
        None
    }

    fn call(&self) -> Option<&CallWire> {
        Some(&self.built.wire)
    }
}

/// The conversation scenario of the web client's test call (`docs/protocol/calling-echo.md`): the call start that
/// opens it names no callee. The fence's own, which its classifier reads a test call by.
pub use teams_fence::TEST_CALL_SCENARIO;

/// The echo service's MRI from the web bundle's `teamsEchoBotMri` (its commercial config): the bot the personal build's
/// "Test call" calls ([`StartCall::echo_test`], then [`AddToCall`] of exactly it). The fence's own, which the personal
/// fence judges the test call by; never taken from the network or the UI.
pub use teams_fence::TEAMS_ECHO_BOT_MRI;

/// Invites someone into a call that exists: the conversation's `add` (how the test call brings the echo in).
///
/// [`AddToCall::invite`] posts to the `addParticipantAndModality` link the start's answer handed out (`…/conv/<id>/add`
/// with the conversation's query), from the call's caller, naming the 1:1 chat of the caller and the invitee and the
/// caller as the meeting's organizer, as the web client does; its target is `WriteKind::Invite` on
/// `Scope::CallOperation` with the invitee added, which the call registry resolves and judges per invitee.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddToCall {
    invitee: Mri,
    url: Url,
    target: Target,
    built: Built,
}

/// What an `add` sends besides its link: who invites, whom (with a fresh participant id), the 1:1 chat of the two (its
/// thread id), the tenant the caller organizes the call in, and the endpoint's callbacks for the add's outcome
/// (`addParticipantSuccess`, `addParticipantFailure`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddParts {
    pub from: CallParticipant,
    pub invitee: Invitee,
    pub chat_thread: String,
    pub tenant_id: String,
    pub links: Links,
}

impl AddToCall {
    /// The `add` at `link` (see the type's docs).
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't an `add` in its captured shape: the caller's MRI not
    /// `8:orgid:<object id>`, a chat other than the 1:1 chat of the caller and the invitee, a link of another operation.
    pub fn invite(context: &CallContext, link: &CallLink, parts: &AddParts) -> Result<Self, CallWriteError> {
        let organizer = parts
            .from
            .mri
            .as_str()
            .strip_prefix("8:orgid:")
            .ok_or(CallWriteError::Unmapped)?;
        let body = json!({
            "disableUnmute": false,
            "participants": {"from": parts.from.json(), "to": [parts.invitee.json()]},
            "participantInvitationData": {
                "invitationData": {"meetingInfo": {"organizerId": organizer, "tenantId": parts.tenant_id}},
            },
            "replacementDetails": null,
            "groupContext": null,
            "groupChat": {"threadId": parts.chat_thread, "messageId": null},
            "links": parts.links.json(),
        });
        let built = operation(context, CallOperation::Add, link, Some(body))?;
        Ok(Self {
            invitee: parts.invitee.mri.clone(),
            url: link.url().clone(),
            target: built.target.clone(),
            built,
        })
    }

    /// Who is invited.
    #[must_use]
    pub fn invitee(&self) -> &Mri {
        &self.invitee
    }

    /// Where the `add` is posted: the link it was built with.
    #[must_use]
    pub fn url(&self) -> Url {
        self.url.clone()
    }
}

impl Mutating for AddToCall {
    fn target(&self) -> Target {
        self.target.clone()
    }
}

impl Sealed for AddToCall {
    fn wire(&self) -> Option<Wire> {
        None
    }

    fn call(&self) -> Option<&CallWire> {
        Some(&self.built.wire)
    }
}

/// The call writes that are only ever built whole: a newtype over [`Built`], its target and wire from it.
macro_rules! call_write {
    ($($name:ident),* $(,)?) => {$(
        impl Mutating for $name {
            fn target(&self) -> Target {
                self.0.target.clone()
            }
        }

        impl Sealed for $name {
            fn wire(&self) -> Option<Wire> {
                None
            }

            fn call(&self) -> Option<&CallWire> {
                Some(&self.0.wire)
            }
        }
    )*};
}

call_write!(
    JoinCall,
    AttachCall,
    ReportRinging,
    UpdateEndpointState,
    AcceptCall,
    DeclineCall,
    AcknowledgeAcceptance,
    ApplyChannelParameters,
    UpdateEndpointMetadata,
    UpdateParticipantProperties,
    LeaveCall,
    RenegotiateMedia,
    AnswerMedia,
    AcknowledgeMediaAnswer,
    UpdateMediaDescriptions,
    KeepAliveCall,
);

/// The meeting a group chat's call is, as a join names it (`meetingInfo`): its organizer's object id and tenant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingInfo {
    pub organizer_id: String,
    pub tenant_id: String,
}

/// What a join of a running call sends: who joins, the endpoint's callbacks for the conversation, the group chat the
/// call belongs to, the meeting, what the client says of itself, and to enter the call ([`JoinCall::enter`]) its
/// media ([`JoinMedia`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinParts {
    pub from: CallParticipant,
    pub roster_update: String,
    pub conversation_links: Links,
    pub group_chat: String,
    pub meeting: Option<MeetingInfo>,
    pub client: ClientInfo,
}

/// What a join that enters the call adds: the endpoint's callbacks for the call and the media controller, the
/// modalities and the offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinMedia {
    pub call_links: Links,
    pub media_controller_links: Links,
    pub modalities: Vec<String>,
    pub media: MediaContent,
}

/// A join of a running call's conversation: `POST` to the conversation's own link (the invitation's
/// `conversationController`), from the chat it belongs to. With media ([`JoinCall::enter`]) the sender enters the call
/// (the call registry takes it as a start in that chat, pended before it goes and registered from its answer); without
/// ([`JoinCall::subscribe`]) it subscribes to the conversation's roster only, as the web client does once its ring
/// ended. Either is `WriteKind::Call` on `Scope::CallOperation` (`join`), resolved by the invitation that bound the
/// conversation to its group chat, the chat read fresh.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinCall(Built);

impl JoinCall {
    /// The join that enters the call at `conversation` with `media`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a join in its captured shape (the group chat not a group chat's id,
    /// a link that isn't a conversation's).
    pub fn enter(
        context: &CallContext,
        conversation: &CallLink,
        parts: &JoinParts,
        media: &JoinMedia,
    ) -> Result<Self, CallWriteError> {
        let mut body = join_body(parts, &json!(""));
        let invitation = call_invitation(
            &media.modalities,
            &media.call_links,
            &media.media_controller_links,
            &media.media,
            &parts.client,
        );
        body.insert("callInvitation".to_owned(), invitation);
        Self::join(context, conversation, body, Registry::Start)
    }

    /// The roster-only join of the conversation at `conversation`.
    ///
    /// # Errors
    /// As [`Self::enter`].
    pub fn subscribe(
        context: &CallContext,
        conversation: &CallLink,
        parts: &JoinParts,
    ) -> Result<Self, CallWriteError> {
        Self::join(context, conversation, join_body(parts, &Value::Null), Registry::Nothing)
    }

    fn join(
        context: &CallContext,
        conversation: &CallLink,
        body: Map<String, Value>,
        registry: Registry,
    ) -> Result<Self, CallWriteError> {
        let built = build(Plan {
            context,
            method: Method::Post,
            url: conversation.url(),
            body: Some(Value::Object(body)),
            expect: Expect::Operation(CallOperation::Join),
            registry,
            echo: None,
        })?;
        Ok(Self(built))
    }
}

/// A join's body without its call invitation (`docs/protocol/calling.md`, "Joining a running call").
fn join_body(parts: &JoinParts, subject: &Value) -> Map<String, Value> {
    let request = json!({
        "conversationType": null,
        "subject": subject,
        "roster": {"type": "Delta", "rosterUpdate": parts.roster_update},
        "properties": conversation_properties(),
        "links": parts.conversation_links.json(),
    });
    let mut body = Map::new();
    body.insert("conversationRequest".to_owned(), request);
    body.insert("groupContext".to_owned(), Value::Null);
    body.insert(
        "groupChat".to_owned(),
        json!({"threadId": parts.group_chat, "messageId": "0"}),
    );
    body.insert("participants".to_owned(), json!({"from": parts.from.json()}));
    parts.client.add_capabilities(&mut body, true);
    body.insert("endpointMetadata".to_owned(), json!({}));
    if let Some(meeting) = &parts.meeting {
        body.insert(
            "meetingInfo".to_owned(),
            json!({"organizerId": meeting.organizer_id, "tenantId": meeting.tenant_id, "isBroadcast": false}),
        );
    }
    body.insert("meetingData".to_owned(), Value::Null);
    body.insert("endpointState".to_owned(), endpoint_state(0));
    body
}

/// What an `attach` sends: the callee, the call leg's `end` callback, the conversation subscription's callbacks (its
/// roster's and its `links`), the conversation it subscribes to (the invitation's `conversationController`), and the
/// client's endpoint capabilities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachParts {
    pub from: CallParticipant,
    pub end: String,
    pub roster_update: String,
    pub conversation_links: Links,
    pub conversation: CallLink,
    pub client: ClientInfo,
}

/// The callee's endpoint attached to its ringing call leg: `POST` to the invitation's `attach` link, with the
/// roster-only subscription to the call's conversation (it joins no call, answers nothing). Sent by the client by
/// itself while an offered invitation rings; its answer hands out the leg's `progress`, `acceptance` and `callLeg`
/// links, which the transport binds in the fence's registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachCall(Built);

impl AttachCall {
    /// The `attach` at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't an `attach` in its captured shape.
    pub fn new(context: &CallContext, link: &CallLink, parts: &AttachParts) -> Result<Self, CallWriteError> {
        let mut input = Map::new();
        parts.client.add_capabilities(&mut input, false);
        input.insert(
            "conversationRequest".to_owned(),
            json!({
                "roster": {"type": "Delta", "rosterUpdate": parts.roster_update},
                "links": parts.conversation_links.json(),
            }),
        );
        input.insert("endpointMetadata".to_owned(), json!({}));
        input.insert("participants".to_owned(), json!({"from": parts.from.json()}));
        let mut body = Map::new();
        body.insert(
            "attach".to_owned(),
            json!({
                "requireMediaContent": false,
                "links": {"end": parts.end},
                "locationContent": null,
                "networkContent": null,
                "areaContent": null,
            }),
        );
        parts.client.add_capabilities(&mut body, false);
        body.insert(
            "additionalActions".to_owned(),
            json!([{
                "name": "join",
                "url": parts.conversation.as_str(),
                "waitForResponse": true,
                "input": Value::Object(input),
            }]),
        );
        let mut built = operation(context, CallOperation::Attach, link, Some(Value::Object(body)))?;
        built.wire.registry = Registry::Answer;
        Ok(Self(built))
    }
}

/// The callee's endpoint tells the call controller it rings: `POST` to the `progress` link the `attach`'s answer handed
/// out, `ringing` only. Sent by the client by itself while an offered invitation rings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportRinging(Built);

impl ReportRinging {
    /// `sender` rings, at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a `progress` in its captured shape.
    pub fn new(context: &CallContext, link: &CallLink, sender: &CallParticipant) -> Result<Self, CallWriteError> {
        let body = json!({"callProgress": {"sender": sender.json(), "status": "ringing", "phrase": "ringing"}});
        Ok(Self(operation(context, CallOperation::Progress, link, Some(body))?))
    }
}

/// The callee's endpoint state in the call's conversation (muted or not): `POST` to the conversation subscription's
/// `updateEndpointState` link, as the callee sends it when it answers and on its call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateEndpointState(Built);

impl UpdateEndpointState {
    /// `from`'s state at `link`: `muted`, the `sequence`th report of the call.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't an endpoint state in its captured shape.
    pub fn new(
        context: &CallContext,
        link: &CallLink,
        from: &CallParticipant,
        muted: bool,
        sequence: u64,
    ) -> Result<Self, CallWriteError> {
        let mut state = endpoint_state(sequence);
        if let Some(state) = state.as_object_mut() {
            state.insert("state".to_owned(), json!({"isMuted": muted}));
        }
        let body = json!({"from": from.json(), "endpointState": state});
        Ok(Self(operation(
            context,
            CallOperation::UpdateEndpointState,
            link,
            Some(body),
        )?))
    }
}

/// What an answer sends: who answers, the modalities it accepts, its endpoint's callbacks for the call (the names of an
/// acknowledgement's: `mediaRenegotiation`, `transfer`, …) and for the media controller, its media answer, and what the
/// client says of itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptParts {
    pub accepted_by: CallParticipant,
    pub modalities: Vec<String>,
    pub call_links: Links,
    pub media_controller_links: Links,
    pub media: MediaContent,
    pub client: ClientInfo,
}

/// The callee answers an offered invitation: `POST` to the `acceptance` link its `attach`'s answer handed out. Its
/// answer (an inline acknowledgement) hands out the answered call's links, which the transport binds in the fence's
/// registry. Only on an explicit answer of an invitation the engine offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptCall(Built);

impl AcceptCall {
    /// The answer at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't an `accept` in its captured shape.
    pub fn new(context: &CallContext, link: &CallLink, parts: &AcceptParts) -> Result<Self, CallWriteError> {
        let mut acceptance = Map::new();
        acceptance.insert("acceptedBy".to_owned(), parts.accepted_by.json());
        acceptance.insert("acceptedCallModalities".to_owned(), json!(parts.modalities));
        parts.client.add_capabilities(&mut acceptance, true);
        acceptance.insert("links".to_owned(), parts.call_links.json());
        if !parts.media_controller_links.is_empty() {
            acceptance.insert(
                "clientContentForMediaController".to_owned(),
                parts.media_controller_links.json(),
            );
        }
        acceptance.insert("mediaContent".to_owned(), parts.media.json());
        if let Some(pstn) = parts.client.pstn_content() {
            acceptance.insert("pstnContent".to_owned(), pstn);
        }
        acceptance.insert("callKeepAliveInterval".to_owned(), Value::Null);
        let body = json!({"callAcceptance": Value::Object(acceptance)});
        let mut built = operation(context, CallOperation::Accept, link, Some(body))?;
        built.wire.registry = Registry::Answer;
        Ok(Self(built))
    }
}

/// The callee declines an offered invitation, as the web client does: `DELETE` to the `callLeg` link its `attach`'s
/// answer handed out, the call's end `603` `CallEndReasonLocalUserInitiated`. The ringing stops everywhere and the
/// caller sees it declined. Only on an explicit decline of an invitation the engine offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclineCall(Built);

impl DeclineCall {
    /// The decline at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the link isn't a `reject`'s.
    pub fn new(context: &CallContext, link: &CallLink) -> Result<Self, CallWriteError> {
        let body = json!({"callEnd": {
            "code": 603,
            "subCode": 0,
            "phrase": "CallEndReasonLocalUserInitiated",
            "resultCategories": ["Success"],
        }});
        Ok(Self(operation(context, CallOperation::Reject, link, Some(body))?))
    }
}

/// The caller acknowledges its call's acceptance by request: `POST` to the acceptance's `acknowledgement` link, handing
/// the call controller the endpoint's callbacks for the call (`mediaRenegotiation`, `transfer`, …). The web client sends
/// it only when the acceptance's delivery response failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcknowledgeAcceptance(Built);

impl AcknowledgeAcceptance {
    /// The acknowledgement at `link`, handing out `links`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't an acknowledgement in its captured shape.
    pub fn new(context: &CallContext, link: &CallLink, links: &Links) -> Result<Self, CallWriteError> {
        let body = json!({"callAcceptanceAcknowledgement": {"links": links.json()}});
        Ok(Self(operation(context, CallOperation::Acknowledge, link, Some(body))?))
    }
}

/// A media line's send capabilities: `POST` to the call controller's `applyChannelParameters` link (`mcProxy`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyChannelParameters(Built);

impl ApplyChannelParameters {
    /// `parameters` at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't channel parameters in their captured shape.
    pub fn new(context: &CallContext, link: &CallLink, parameters: &ChannelParameters) -> Result<Self, CallWriteError> {
        let body = json!({"applyChannelParameters": parameters.json()});
        Ok(Self(operation(
            context,
            CallOperation::ApplyChannelParameters,
            link,
            Some(body),
        )?))
    }
}

/// The sender's endpoint metadata in the call's conversation: `PUT` to the `updateEndpointMetadata` link (the start's
/// answer's for the caller, the conversation subscription's for the callee).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateEndpointMetadata(Built);

impl UpdateEndpointMetadata {
    /// `from`'s metadata at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't endpoint metadata in its captured shape.
    pub fn new(context: &CallContext, link: &CallLink, from: &CallParticipant) -> Result<Self, CallWriteError> {
        let body = json!({"participants": {"from": from.json()}, "endpointMetadata": {}});
        Ok(Self(operation(
            context,
            CallOperation::UpdateEndpointMetadata,
            link,
            Some(body),
        )?))
    }
}

/// The sender's own participant properties in the call's conversation (its AI voice consent): `POST` to the
/// conversation subscription's `updateParticipantProperties` link, with the callback for the update's status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateParticipantProperties(Built);

/// The sender's AI voice consent as the call's conversation takes it: the value (`"0"` in every capture) and the
/// sender's own sequence number for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceConsent {
    pub value: String,
    pub sequence: u64,
}

impl UpdateParticipantProperties {
    /// `from`'s `consent` at `link`, its status called back at `status`, the update numbered `operation_id`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't participant properties in their captured shape.
    pub fn new(
        context: &CallContext,
        link: &CallLink,
        from: &CallParticipant,
        consent: &VoiceConsent,
        status: &str,
        operation_id: &str,
    ) -> Result<Self, CallWriteError> {
        let body = json!({
            "from": from.short_json(),
            "to": [{
                "id": from.mri.as_str(),
                "propertyBag": {"aiVoiceConsent": {
                    "sequenceNumber": consent.sequence,
                    "value": {"aiVoiceConsentValue": consent.value},
                }},
            }],
            "links": {"updateParticipantPropertiesStatus": status},
            "operationId": operation_id,
        });
        Ok(Self(operation(
            context,
            CallOperation::UpdateParticipantProperties,
            link,
            Some(body),
        )?))
    }
}

/// Why the sender leaves its call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaveReason {
    /// It hangs up a call that connected (the call's end `0`).
    HungUp,
    /// The caller cancels its call while it still rings, after `rang_seconds` (the call's end `487`, with how long it
    /// rang in its call quality diagnostics).
    Cancelled { rang_seconds: u64 },
}

/// The sender leaves its call: `POST` to the conversation's `leave` link (the start's answer's for the caller, the
/// conversation subscription's for the callee), the conversation's and the call's end as the web client sends them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaveCall(Built);

impl LeaveCall {
    /// `from` leaves at `link`, for `reason`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a `leave` in its captured shape.
    pub fn new(
        context: &CallContext,
        link: &CallLink,
        from: &CallParticipant,
        reason: LeaveReason,
    ) -> Result<Self, CallWriteError> {
        let mut call_end = json!({
            "code": 0,
            "subCode": 0,
            "phrase": "CallEndReasonLocalUserInitiated",
            "resultCategories": ["Success"],
        });
        if let (LeaveReason::Cancelled { rang_seconds }, Some(end)) = (reason, call_end.as_object_mut()) {
            end.insert("code".to_owned(), json!(487));
            end.insert(
                "callQualityDiagnosticsInformation".to_owned(),
                json!({"cancelationDuration": rang_seconds}),
            );
        }
        let body = json!({
            "participants": {"from": from.json()},
            "conversationTransactionEnd": {"reason": "noError", "code": 0, "phrase": "ConversationEndNoModalityConnected"},
            "callTransactionEnd": call_end,
        });
        Ok(Self(operation(context, CallOperation::Leave, link, Some(body))?))
    }
}

/// A media renegotiation of the sender's call (its camera turned on, say): `POST` to the call controller's
/// `mediaRenegotiation` link, the sender's new offer and its callbacks for the answer and the rejection
/// (`mediaAnswer`, `rejection`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenegotiateMedia(Built);

impl RenegotiateMedia {
    /// `sender`'s renegotiation at `link`: the call's `modalities`, the callbacks `links`, the offer `media`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a renegotiation in its captured shape.
    pub fn new(
        context: &CallContext,
        link: &CallLink,
        sender: &CallParticipant,
        modalities: &[String],
        links: &Links,
        media: &MediaContent,
    ) -> Result<Self, CallWriteError> {
        let body = json!({"mediaNegotiation": {
            "callModalities": modalities,
            "sender": sender.json(),
            "links": links.json(),
            "mediaContent": media.json(),
        }});
        Ok(Self(operation(context, CallOperation::Renegotiate, link, Some(body))?))
    }
}

/// The sender's answer to a renegotiation of its call: `POST` to the `mediaAnswer` link the renegotiation delivered,
/// its callbacks for the media acknowledgement (`links`) and the media controller, and its answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnswerMedia(Built);

/// What an answer to a renegotiation sends beside its link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnswerMediaParts {
    pub sender: CallParticipant,
    pub modalities: Vec<String>,
    pub links: Links,
    pub media_controller_links: Links,
    pub media: MediaContent,
}

impl AnswerMedia {
    /// The answer at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't a renegotiation's answer in its captured shape.
    pub fn new(context: &CallContext, link: &CallLink, parts: &AnswerMediaParts) -> Result<Self, CallWriteError> {
        let mut answer = Map::new();
        answer.insert("callModalities".to_owned(), json!(parts.modalities));
        answer.insert("sender".to_owned(), parts.sender.json());
        answer.insert("links".to_owned(), parts.links.json());
        if !parts.media_controller_links.is_empty() {
            answer.insert(
                "clientContentForMediaController".to_owned(),
                parts.media_controller_links.json(),
            );
        }
        answer.insert("mediaContent".to_owned(), parts.media.json());
        let body = json!({"mediaAnswer": Value::Object(answer)});
        Ok(Self(operation(context, CallOperation::MediaAnswer, link, Some(body))?))
    }
}

/// The answer to the sender's renegotiation acknowledged: `POST` with no body to the `mediaAcknowledgement` link that
/// answer handed out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcknowledgeMediaAnswer(Built);

impl AcknowledgeMediaAnswer {
    /// The acknowledgement at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the link isn't a media answer's acknowledgement.
    pub fn new(context: &CallContext, link: &CallLink) -> Result<Self, CallWriteError> {
        Ok(Self(operation(context, CallOperation::MediaAcknowledge, link, None)?))
    }
}

/// Which media lines the sender sends and receives (its camera on: its main video line `sendrecv`): `POST` to the call
/// controller's `updateMediaDescriptions` link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateMediaDescriptions(Built);

impl UpdateMediaDescriptions {
    /// `descriptions` at `link`, tagged `negotiation_tag` when given.
    ///
    /// # Errors
    /// [`CallWriteError`] when the request isn't media descriptions in their captured shape.
    pub fn new(
        context: &CallContext,
        link: &CallLink,
        descriptions: &MediaDescriptions,
        negotiation_tag: Option<&str>,
    ) -> Result<Self, CallWriteError> {
        let mut described = descriptions.json();
        if let (Some(tag), Some(object)) = (negotiation_tag, described.as_object_mut()) {
            object.insert("negotiationTag".to_owned(), json!(tag));
        }
        let body = json!({"UpdateMediaDescriptions": {"mediaDescriptions": described}});
        Ok(Self(operation(
            context,
            CallOperation::UpdateMediaDescriptions,
            link,
            Some(body),
        )?))
    }
}

/// The call's keep-alive: `POST` of an empty participant update (`{"callParticipantUpdate": {}}`) to the call leg's
/// own `callLeg` link (the caller's acceptance's, the callee's acceptance's acknowledgement's), sent every 90 % of the
/// call's `callKeepAliveInterval` as the web client does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeepAliveCall(Built);

impl KeepAliveCall {
    /// The keep-alive at `link`.
    ///
    /// # Errors
    /// [`CallWriteError`] when the link isn't a call leg's own.
    pub fn new(context: &CallContext, link: &CallLink) -> Result<Self, CallWriteError> {
        let body = json!({"callParticipantUpdate": {}});
        Ok(Self(operation(context, CallOperation::KeepAlive, link, Some(body))?))
    }
}
