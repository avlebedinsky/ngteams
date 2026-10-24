//! The calling family's rows (`docs/protocol/calling-echo.md`, `docs/protocol/calling.md`): the web client's test call
//! start, a new call of the caller and the echo service; its call start from a 1:1 or group chat's header
//! ([`chat_call_start`]), a call in the chat it carries; and the captured operations on a call that exists ([`follow_up`]: the endpoint's
//! metadata, an `add`, the `leave`, the acceptance's acknowledgement and a media line's channel parameters), each a
//! write on [`Scope::CallOperation`] that the call registry resolves (`crate::calls`). Only their captured shapes map;
//! every other mutating request on a calling host is unreadable until a row maps it.

use serde_json::{Map, Value};
use url::{Position, Url};

use super::decoded_segments;
use super::json::strict_json;
use crate::membership::{ONE_TO_ONE_SUFFIX, is_group_chat_id, is_guid, one_to_one_id};
use crate::{CallOperation, Mri, Scope, Target, WriteKind};

/// The flight proxy every call request goes through: the call start, and every link the call's services hand out.
pub(crate) const PROXY_HOST: &str = "api.flightproxy.teams.microsoft.com";

/// The call controller's conversations endpoint, where a call starts (its URL without a query).
pub const CALL_START_URL: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/epconv";

/// The conversation scenario of the web client's test call. Its start names no callee: the client opens a conversation
/// of its own and then adds the echo participant with the conversation's `add`.
pub const TEST_CALL_SCENARIO: &str = "UserInitiatedTestCall";

/// What a value of a call start may be: the type the capture had there (every call start of 2026-10-20 alike).
#[derive(Clone, Copy)]
pub(crate) enum Shape {
    /// `null` only: what the capture left empty, such as a call to replace or transfer, or a group, chat or meeting to
    /// call into.
    Null,
    Bool,
    /// A non-negative integer.
    Count,
    Text,
    /// An array of strings.
    Texts,
    /// An object of reviewed fields: no key outside them, each value of its shape, the required ones there.
    Object(&'static [Field]),
    /// An array of such objects.
    Objects(&'static [Field]),
    /// `null`, or an object of reviewed fields: what a chat's call start says of a group chat it calls into.
    NullOr(&'static [Field]),
}

/// A reviewed key of an object, the shape of its value, and whether it must be there.
pub(crate) struct Field {
    key: &'static str,
    shape: Shape,
    required: bool,
}

/// A key that may be missing.
pub(crate) const fn field(key: &'static str, shape: Shape) -> Field {
    Field {
        key,
        shape,
        required: false,
    }
}

/// A key that must be there.
pub(crate) const fn required(key: &'static str, shape: Shape) -> Field {
    Field {
        key,
        shape,
        required: true,
    }
}

/// A call start as the web client's test call posts it (seen 2026-10-20). A group, chat or meeting to call into, content
/// to share and a call to replace or transfer are all `null`: the call creates its own conversation.
const CALL_START: &[Field] = &[
    required("conversationRequest", Shape::Object(CONVERSATION_REQUEST)),
    field("contentSharing", Shape::Null),
    required("participants", Shape::Object(PARTICIPANTS)),
    field("capabilities", Shape::Null),
    field("endpointCapabilities", Shape::Count),
    field("clientEndpointCapabilities", Shape::Count),
    field("endpointMetadata", Shape::Object(ENDPOINT_METADATA)),
    field("groupContext", Shape::Null),
    field("groupChat", Shape::Null),
    field("meetingInfo", Shape::Null),
    field("meetingData", Shape::Null),
    field("endpointState", Shape::Object(ENDPOINT_STATE)),
    required("callInvitation", Shape::Object(CALL_INVITATION)),
    field("debugContent", Shape::Object(DEBUG_CONTENT)),
    field("participantPropertyBag", Shape::Object(PROPERTY_BAG)),
];

/// The conversation the start opens: its scenario, and the endpoint's callbacks for it.
const CONVERSATION_REQUEST: &[Field] = &[
    field("conversationType", Shape::Null),
    field("subject", Shape::Null),
    field("suppressDialout", Shape::Bool),
    field("roster", Shape::Object(ROSTER)),
    field("properties", Shape::Object(CONVERSATION_PROPERTIES)),
    field("links", Shape::Object(CONVERSATION_LINKS)),
    required("scenario", Shape::Text),
];

/// The caller and the callees; nobody else.
const PARTICIPANTS: &[Field] = &[
    required("from", Shape::Object(FROM)),
    // a callee as the client names the echo bot when it adds it (`docs/protocol/calling-echo.md`)
    field("to", Shape::Objects(INVITEE)),
];

/// The call the start places: its modalities, the endpoint's callbacks, the client and its offer.
const CALL_INVITATION: &[Field] = &[
    field("callModalities", Shape::Texts),
    field("replaces", Shape::Null),
    field("transferor", Shape::Null),
    field("clientTransferContext", Shape::Null),
    field("customContext", Shape::Null),
    field("links", Shape::Object(CALL_LINKS)),
    field(
        "clientContentForMediaController",
        Shape::Object(MEDIA_CONTROLLER_CONTENT),
    ),
    field("pstnContent", Shape::Object(PSTN_CONTENT)),
    field("emergencyContent", Shape::Null),
    field("mediaContent", Shape::Object(MEDIA_CONTENT)),
    field("voicemailSettings", Shape::Object(&[])),
    field("locationContent", Shape::Null),
    field("networkContent", Shape::Null),
    field("areaContent", Shape::Null),
];

/// The offer and how the client describes its media lines.
const MEDIA_CONTENT: &[Field] = &[
    field("blob", Shape::Text),
    field("contentType", Shape::Text),
    field("clientLocation", Shape::Text),
    field("mediaDescriptions", Shape::Object(MEDIA_DESCRIPTIONS)),
    field("applyChannelParameters", Shape::Object(CHANNEL_PARAMETERS)),
    field("mediaLegId", Shape::Text),
];

/// The endpoint's metadata, in a call start and in its update.
const ENDPOINT_METADATA: &[Field] = &[field("holographicCapabilities", Shape::Count)];

/// A call start's debug content.
const DEBUG_CONTENT: &[Field] = &[field("ecsEtag", Shape::Text)];

/// How the conversation a start opens sends its roster.
const ROSTER: &[Field] = &[field("type", Shape::Text), field("rosterUpdate", Shape::Text)];

/// The properties of the conversation a start opens.
const CONVERSATION_PROPERTIES: &[Field] = &[
    field("allowConversationWithoutHost", Shape::Bool),
    field("enableGroupCallEventMessages", Shape::Bool),
    field("enableGroupCallUpgradeMessage", Shape::Bool),
    field("enableGroupCallMeetupGeneration", Shape::Bool),
];

/// The endpoint's callbacks for the conversation a start opens.
const CONVERSATION_LINKS: &[Field] = &[
    field("conversationEnd", Shape::Text),
    field("conversationUpdate", Shape::Text),
    field("localParticipantUpdate", Shape::Text),
    field("addParticipantSuccess", Shape::Text),
    field("addParticipantFailure", Shape::Text),
    field("addModalitySuccess", Shape::Text),
    field("addModalityFailure", Shape::Text),
    field("confirmUnmute", Shape::Text),
    field("receiveMessage", Shape::Text),
];

/// A participant a start calls or an `add` invites.
const INVITEE: &[Field] = &[required("id", Shape::Text), field("participantId", Shape::Text)];

/// The endpoint's callbacks for the call a start places.
const CALL_LINKS: &[Field] = &[
    field("progress", Shape::Text),
    field("mediaAnswer", Shape::Text),
    field("acceptance", Shape::Text),
    field("redirection", Shape::Text),
    field("end", Shape::Text),
];

/// The client's callbacks for the media controller.
const MEDIA_CONTROLLER_CONTENT: &[Field] = &[
    field("controlVideoStreaming", Shape::Text),
    field("csrcInfo", Shape::Text),
];

/// What a start says of the phone network.
const PSTN_CONTENT: &[Field] = &[
    field("emergencyCallCountry", Shape::Text),
    field("platformName", Shape::Text),
    field("publicApiCall", Shape::Bool),
];

/// How the client describes an offer's media lines.
const MEDIA_DESCRIPTIONS: &[Field] = &[
    field(
        "descriptions",
        Shape::Objects(&[
            field("mid", Shape::Text),
            field("direction", Shape::Text),
            field("label", Shape::Text),
        ]),
    ),
    field("requestId", Shape::Count),
];

/// A media line's channel parameters, in an offer.
const CHANNEL_PARAMETERS: &[Field] = &[field("multiChannelParameter", Shape::Object(MULTI_CHANNEL_PARAMETER))];

/// The channel parameters of a media line.
const MULTI_CHANNEL_PARAMETER: &[Field] = &[field("mediaParameter", Shape::Text), field("mids", Shape::Texts)];

/// The endpoint's state as the client reports it.
const ENDPOINT_STATE: &[Field] = &[
    field("endpointStateSequenceNumber", Shape::Count),
    field(
        "endpointProperties",
        Shape::Object(&[field(
            "additionalEndpointProperties",
            Shape::Object(&[field("infoShownInReportMode", Shape::Text)]),
        )]),
    ),
];

/// The caller's own consent setting.
const PROPERTY_BAG: &[Field] = &[field(
    "aiVoiceConsent",
    Shape::Object(&[
        field("value", Shape::Object(&[field("aiVoiceConsentValue", Shape::Text)])),
        field("sequenceNumber", Shape::Count),
    ]),
)];

/// A call start from a chat's header as the web client posts it (seen 2026-10-24, `ab-call` and `abc-call`): the test
/// call's keys, with no conversation scenario, its callees named, and the group chat it calls into (`null` for a 1:1
/// call, which names only its callee). A group context, a meeting, content to share and a call to replace or transfer
/// are all `null`.
const CHAT_CALL_START: &[Field] = &[
    required("conversationRequest", Shape::Object(CHAT_CONVERSATION_REQUEST)),
    field("contentSharing", Shape::Null),
    required("participants", Shape::Object(CHAT_PARTICIPANTS)),
    field("capabilities", Shape::Null),
    field("endpointCapabilities", Shape::Count),
    field("clientEndpointCapabilities", Shape::Count),
    field("endpointMetadata", Shape::Object(ENDPOINT_METADATA)),
    field("groupContext", Shape::Null),
    required(
        "groupChat",
        Shape::NullOr(&[required("threadId", Shape::Text), field("messageId", Shape::Null)]),
    ),
    field("meetingInfo", Shape::Null),
    field("meetingData", Shape::Null),
    field("endpointState", Shape::Object(ENDPOINT_STATE)),
    required("callInvitation", Shape::Object(CHAT_CALL_INVITATION)),
    field("debugContent", Shape::Object(DEBUG_CONTENT)),
    field("participantPropertyBag", Shape::Object(PROPERTY_BAG)),
];

/// The conversation a chat's call start opens: as the test call's, with no scenario.
const CHAT_CONVERSATION_REQUEST: &[Field] = &[
    field("conversationType", Shape::Null),
    field("subject", Shape::Null),
    field("suppressDialout", Shape::Bool),
    field("roster", Shape::Object(ROSTER)),
    field("properties", Shape::Object(CONVERSATION_PROPERTIES)),
    field("links", Shape::Object(CONVERSATION_LINKS)),
];

/// The caller and the callees it names, at least one.
const CHAT_PARTICIPANTS: &[Field] = &[
    required("from", Shape::Object(FROM)),
    required("to", Shape::Objects(INVITEE)),
];

/// The call a chat's call start places: as the test call's, its media content as [`CHAT_MEDIA_CONTENT`].
const CHAT_CALL_INVITATION: &[Field] = &[
    field("callModalities", Shape::Texts),
    field("replaces", Shape::Null),
    field("transferor", Shape::Null),
    field("clientTransferContext", Shape::Null),
    field("customContext", Shape::Null),
    field("links", Shape::Object(CALL_LINKS)),
    field(
        "clientContentForMediaController",
        Shape::Object(MEDIA_CONTROLLER_CONTENT),
    ),
    field("pstnContent", Shape::Object(PSTN_CONTENT)),
    field("emergencyContent", Shape::Null),
    field("mediaContent", Shape::Object(CHAT_MEDIA_CONTENT)),
    field("voicemailSettings", Shape::Object(&[])),
    field("locationContent", Shape::Null),
    field("networkContent", Shape::Null),
    field("areaContent", Shape::Null),
];

/// A chat call's offer: the test call's media content, and the features it requires (a 1:1 call's).
const CHAT_MEDIA_CONTENT: &[Field] = &[
    field("blob", Shape::Text),
    field("contentType", Shape::Text),
    field("requiredFeatures", Shape::Text),
    field("clientLocation", Shape::Text),
    field("mediaDescriptions", Shape::Object(MEDIA_DESCRIPTIONS)),
    field("applyChannelParameters", Shape::Object(CHANNEL_PARAMETERS)),
    field("mediaLegId", Shape::Text),
];

/// Whether `value` has `shape`, all the way down.
pub(crate) fn fits(value: &Value, shape: Shape) -> bool {
    match shape {
        Shape::Null => value.is_null(),
        Shape::Bool => value.is_boolean(),
        Shape::Count => value.is_u64(),
        Shape::Text => value.is_string(),
        Shape::Texts => value.as_array().is_some_and(|items| items.iter().all(Value::is_string)),
        Shape::Object(fields) => value.as_object().is_some_and(|object| fits_fields(object, fields)),
        Shape::NullOr(fields) => value.is_null() || fits(value, Shape::Object(fields)),
        Shape::Objects(fields) => value
            .as_array()
            .is_some_and(|items| items.iter().all(|item| fits(item, Shape::Object(fields)))),
    }
}

/// Whether `object` has every required field, no key outside `fields`, and each value of its field's shape.
fn fits_fields(object: &Map<String, Value>, fields: &[Field]) -> bool {
    fields
        .iter()
        .all(|field| !field.required || object.contains_key(field.key))
        && object.iter().all(|(key, value)| {
            fields
                .iter()
                .find(|field| field.key == key)
                .is_some_and(|field| fits(value, field.shape))
        })
}

/// The write the web client's test call start makes: [`WriteKind::Call`] on [`Scope::New`], its participants the
/// caller (`participants.from.id`) and `test_call_callee`, the echo service `lab.toml` approves. The start names no
/// callee, or only that echo service; the client adds the echo right after, so it is counted in, and the fence lets a
/// test call start only when it would admit the echo too. Without one it is `None`. (The target is what the fence
/// authorizes, not the conversation's members at that moment, which are the caller alone.)
///
/// Only the captured shape maps: [`CALL_START_URL`] with no query, the scenario [`TEST_CALL_SCENARIO`], and every key,
/// at every level, one the capture had, its value of the type it had there (`null` where it was `null`). Another
/// scenario, a call to anyone else, a key nobody reviewed or a call into something that exists already is `None`
/// (unreadable).
#[must_use]
pub(crate) fn call_start(
    method: &str,
    url: &Url,
    body: Option<&[u8]>,
    test_call_callee: Option<&Mri>,
) -> Option<Target> {
    if !is_start_request(method, url) {
        return None;
    }
    let start = strict_json(body?)?;
    if !fits(&start, Shape::Object(CALL_START))
        || start.pointer("/conversationRequest/scenario")?.as_str()? != TEST_CALL_SCENARIO
    {
        return None;
    }
    let echo = test_call_callee?;
    let caller = Mri::new(start.pointer("/participants/from/id")?.as_str()?);
    let named = start
        .pointer("/participants/to")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let echo_only = match named {
        [] => true,
        [only] => only.get("id").and_then(Value::as_str) == Some(echo.as_str()),
        _ => false,
    };
    echo_only.then(|| Target::new(WriteKind::Call, Scope::New).with_added([caller, echo.clone()]))
}

/// Whether `method url` is where a call starts: `POST` [`CALL_START_URL`] with no query.
fn is_start_request(method: &str, url: &Url) -> bool {
    method == "POST" && &url[..Position::AfterPath] == CALL_START_URL && url.query().is_none()
}

/// The write the web client's call start from a 1:1 or group chat's header makes: [`WriteKind::Call`] on the chat it is
/// tied to by what it carries ([`Scope::Conversation`], whose members the fence reads fresh), its added participants
/// the caller (`participants.from.id`) and every callee it names (`participants.to`), so the fence permits it only
/// when the chat is lab-only and every callee a lab account (`docs/protocol/calling.md`).
///
/// The chat: the group chat `groupChat.threadId` names (a group chat's id, [`is_group_chat_id`]); for a start whose
/// `groupChat` is `null`, which names exactly one callee, the 1:1 chat of the caller and that callee as the chat
/// service names it ([`one_to_one_id`]). Only the captured shape maps ([`CHAT_CALL_START`]: no conversation scenario,
/// `groupChat` always there, every key one the captures had, its value of the type it had there): the caller and every
/// callee a work account (`8:orgid:<guid>`), at least one callee, none twice and none the caller. Anything else is
/// `None` (unreadable).
#[must_use]
pub(crate) fn chat_call_start(method: &str, url: &Url, body: Option<&[u8]>) -> Option<Target> {
    if !is_start_request(method, url) {
        return None;
    }
    chat_start_target(&strict_json(body?)?)
}

/// The target of a chat's call start already read ([`chat_call_start`]).
fn chat_start_target(start: &Value) -> Option<Target> {
    if !fits(start, Shape::Object(CHAT_CALL_START)) {
        return None;
    }
    let work_account = |id: &Value| {
        let mri = id.as_str()?;
        is_guid(mri.strip_prefix("8:orgid:")?).then(|| Mri::new(mri))
    };
    let caller = work_account(start.pointer("/participants/from/id")?)?;
    let callees: Vec<Mri> = start
        .pointer("/participants/to")?
        .as_array()?
        .iter()
        .map(|callee| work_account(callee.get("id")?))
        .collect::<Option<_>>()?;
    let mut seen = std::collections::BTreeSet::new();
    let distinct = callees
        .iter()
        .chain([&caller])
        .all(|mri| seen.insert(mri.as_str().to_ascii_lowercase()));
    if callees.is_empty() || !distinct {
        return None;
    }
    let chat = match start.get("groupChat")? {
        Value::Null => match callees.as_slice() {
            [only] => one_to_one_id(caller.as_str(), only.as_str())?,
            _ => return None,
        },
        group => {
            let id = group.get("threadId")?.as_str()?;
            is_group_chat_id(id).then(|| id.to_owned())?
        }
    };
    let scope = Scope::Conversation { id: chat };
    Some(Target::new(WriteKind::Call, scope).with_added(std::iter::once(caller).chain(callees)))
}

/// The target a call start's body gives, as the permit for it was given: the test call's row with the echo service
/// for a new call ([`Scope::New`]: the start's one participant besides its caller), the chat's row for a call in a
/// chat. `None` for a body that gives neither.
pub(crate) fn start_target(target: &Target, body: &[u8], caller: &Mri) -> Option<Target> {
    let url = Url::parse(CALL_START_URL).ok()?;
    match target.scope() {
        Scope::New => {
            let mut others = target.added().iter().filter(|mri| *mri != caller);
            let (Some(echo), None) = (others.next(), others.next()) else {
                return None;
            };
            call_start("POST", &url, Some(body), Some(echo))
        }
        Scope::Conversation { .. } => chat_call_start("POST", &url, Some(body)),
        _ => None,
    }
}

/// What a permitted call start tells the call registry beyond its target: who places it (`participants.from.id`) and
/// the endpoint's own callback links for the call, by name, as the start names them.
pub(crate) struct StartEvidence {
    pub(crate) caller: Mri,
    /// `callInvitation.links` (`acceptance`, `end`, …).
    pub(crate) call_links: Map<String, Value>,
    /// `conversationRequest.links` (`conversationEnd`, `conversationUpdate`, …).
    pub(crate) conversation_links: Map<String, Value>,
}

/// The evidence of a call start in a captured shape (the test call's, as [`call_start`] reads it, or a chat's, as
/// [`chat_call_start`] does); `None` for anything else.
pub(crate) fn start_evidence(body: &[u8]) -> Option<StartEvidence> {
    let start = strict_json(body)?;
    if !fits(&start, Shape::Object(CALL_START)) && !fits(&start, Shape::Object(CHAT_CALL_START)) {
        return None;
    }
    let links = |at: &str| {
        start
            .pointer(at)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    };
    Some(StartEvidence {
        caller: Mri::new(start.pointer("/participants/from/id")?.as_str()?),
        call_links: links("/callInvitation/links"),
        conversation_links: links("/conversationRequest/links"),
    })
}

/// The sender of a conversation service operation, as the capture names it: the caller's participant.
const FROM: &[Field] = &[
    required("id", Shape::Text),
    field("displayName", Shape::Text),
    field("endpointId", Shape::Text),
    field("participantId", Shape::Text),
    field("languageId", Shape::Text),
];

/// `participants` with the sender alone.
const FROM_ONLY: &[Field] = &[required("from", Shape::Object(FROM))];

/// `PUT …/conv/<id>/updateEndpointMetadata` (seen 2026-10-20).
const UPDATE_ENDPOINT_METADATA: &[Field] = &[
    required("participants", Shape::Object(FROM_ONLY)),
    field("endpointMetadata", Shape::Object(ENDPOINT_METADATA)),
];

/// `POST …/conv/<id>/leave` (seen 2026-10-20), and the caller's cancel of a call still ringing, which carries how
/// long it rang in its call quality diagnostics (seen 2026-10-24, `ab-call --call cancel`).
const LEAVE: &[Field] = &[
    required("participants", Shape::Object(FROM_ONLY)),
    field(
        "conversationTransactionEnd",
        Shape::Object(&[
            field("reason", Shape::Text),
            field("code", Shape::Count),
            field("phrase", Shape::Text),
        ]),
    ),
    field(
        "callTransactionEnd",
        Shape::Object(&[
            field("code", Shape::Count),
            field("subCode", Shape::Count),
            field("phrase", Shape::Text),
            field("resultCategories", Shape::Texts),
            field(
                "callQualityDiagnosticsInformation",
                Shape::Object(&[field("cancelationDuration", Shape::Count)]),
            ),
        ]),
    ),
];

/// `POST …/conv/<id>/add` (seen 2026-10-20): the caller invites the participants of `to` into its call, naming the chat
/// the call belongs to (the 1:1 chat of the caller and its one invitee, [`names_own_chat`]) and the caller as the
/// meeting's organizer. Nothing to replace, no group context.
const ADD: &[Field] = &[
    required(
        "participants",
        Shape::Object(&[
            required("from", Shape::Object(FROM)),
            required("to", Shape::Objects(INVITEE)),
        ]),
    ),
    field("disableUnmute", Shape::Bool),
    field(
        "participantInvitationData",
        Shape::Object(&[field(
            "invitationData",
            Shape::Object(&[field(
                "meetingInfo",
                Shape::Object(&[field("organizerId", Shape::Text), field("tenantId", Shape::Text)]),
            )]),
        )]),
    ),
    field("replacementDetails", Shape::Null),
    field("groupContext", Shape::Null),
    field(
        "groupChat",
        Shape::Object(&[field("threadId", Shape::Text), field("messageId", Shape::Null)]),
    ),
    field(
        "links",
        Shape::Object(&[
            field("addParticipantSuccess", Shape::Text),
            field("addParticipantFailure", Shape::Text),
        ]),
    ),
];

/// The endpoint's callbacks an acknowledgement hands the call controller (seen 2026-10-20), in the request and in the
/// acknowledgement's delivery response (`crate::calls`).
const ACKNOWLEDGEMENT_LINKS: &[Field] = &[
    field("balanceUpdate", Shape::Text),
    field("controlVideoStreaming", Shape::Text),
    field("mediaRenegotiation", Shape::Text),
    field("replacement", Shape::Text),
    field("retargetCompletion", Shape::Text),
    field("transfer", Shape::Text),
    field("updateMediaDescriptions", Shape::Text),
];

/// `POST …/cc/v1/callAcceptance/<call>/…/acknowledge`, and the body of the acceptance's delivery response (seen
/// 2026-10-20).
pub(crate) const ACKNOWLEDGE: &[Field] = &[required(
    "callAcceptanceAcknowledgement",
    Shape::Object(&[required("links", Shape::Object(ACKNOWLEDGEMENT_LINKS))]),
)];

/// `POST …/cc/v1/mcProxy/<call>/…/applyChannelParameters` (seen 2026-10-20).
const APPLY_CHANNEL_PARAMETERS: &[Field] = &[required(
    "applyChannelParameters",
    Shape::Object(&[required(
        "multiChannelParameter",
        Shape::Object(MULTI_CHANNEL_PARAMETER),
    )]),
)];

/// The captured operation `url`'s path names: `/api/v2/ep/<host>/conv/<id>/<op>` for the conversation service's,
/// `/api/v2/ep/<host>/cc/v1/<callAcceptance|mcProxy>/<call>/…/<op>` for the call controller's. Its link in the call
/// registry is what ties it to a call; this only tells which operation it is.
fn operation_of(url: &Url) -> Option<CallOperation> {
    let segments = decoded_segments(url)?;
    let path: Vec<&str> = segments.iter().map(String::as_str).collect();
    match path.as_slice() {
        ["api", "v2", "ep", host, "conv", id, op] if !host.is_empty() && !id.is_empty() => match *op {
            "updateEndpointMetadata" => Some(CallOperation::UpdateEndpointMetadata),
            "add" => Some(CallOperation::Add),
            "leave" => Some(CallOperation::Leave),
            _ => None,
        },
        ["api", "v2", "ep", host, "cc", "v1", kind, call, .., op] if !host.is_empty() && !call.is_empty() => {
            match (*kind, *op) {
                ("callAcceptance", "acknowledge") => Some(CallOperation::Acknowledge),
                ("mcProxy", "applyChannelParameters") => Some(CallOperation::ApplyChannelParameters),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The body shape of `operation`, as captured.
fn shape_of(operation: CallOperation) -> &'static [Field] {
    match operation {
        CallOperation::UpdateEndpointMetadata => UPDATE_ENDPOINT_METADATA,
        CallOperation::Add => ADD,
        CallOperation::Leave => LEAVE,
        CallOperation::Acknowledge => ACKNOWLEDGE,
        CallOperation::ApplyChannelParameters => APPLY_CHANNEL_PARAMETERS,
    }
}

/// The write a captured operation on an existing call makes: [`Scope::CallOperation`] with the operation, the exact URL,
/// the sender its body names and the callback links it hands out, of the operation's kind ([`CallOperation::kind`]); an
/// `add` adds the participants of its `to`. Only `https` to the flight proxy ([`PROXY_HOST`]), the operation's captured
/// method and path, and its body in the captured shape map (every key one the capture had, its value of the captured
/// type), an `add` only naming its sender's own chat and meeting ([`names_own_chat`]); anything else is `None`
/// (unreadable). Which call the URL belongs to, whether the sender is that call's caller and whether the callbacks are
/// its endpoint's, is the call registry's to judge (`Fence::check`): this row reads only the request.
#[must_use]
pub(crate) fn follow_up(method: &str, url: &Url, body: Option<&[u8]>) -> Option<Target> {
    if url.scheme() != "https" || url.host_str() != Some(PROXY_HOST) || url.port().is_some() {
        return None;
    }
    let operation = operation_of(url)?;
    if method != operation.method() {
        return None;
    }
    let request = strict_json(body?)?;
    if !fits(&request, Shape::Object(shape_of(operation))) {
        return None;
    }
    let from = match request.pointer("/participants/from/id") {
        Some(id) => Some(Mri::new(id.as_str()?)),
        None => None,
    };
    let invitees: Vec<Mri> = match operation {
        CallOperation::Add => request
            .pointer("/participants/to")?
            .as_array()?
            .iter()
            .map(|invitee| invitee.get("id").and_then(Value::as_str).map(Mri::new))
            .collect::<Option<_>>()?,
        _ => Vec::new(),
    };
    if operation == CallOperation::Add && (invitees.is_empty() || !names_own_chat(&request, from.as_ref()?, &invitees))
    {
        return None;
    }
    let callbacks = match operation {
        CallOperation::Acknowledge => links_of(request.pointer("/callAcceptanceAcknowledgement/links"))?,
        CallOperation::Add => links_of(request.get("links"))?,
        _ => Vec::new(),
    };
    let scope = Scope::CallOperation {
        operation,
        url: url.as_str().to_owned(),
        from,
        callbacks: callbacks.into(),
    };
    Some(Target::new(operation.kind(), scope).with_added(invitees))
}

/// The links of a body's `links` object, each a string; none when there is no such object.
fn links_of(links: Option<&Value>) -> Option<Vec<String>> {
    let Some(links) = links else {
        return Some(Vec::new());
    };
    links
        .as_object()?
        .values()
        .map(|link| link.as_str().map(str::to_owned))
        .collect()
}

/// The id a 1:1 chat's id carries for a participant: a work account's object id (`8:orgid:<guid>`) or a bot's
/// (`28:<guid>`), lowercase. `None` for any other form.
fn chat_member_id(mri: &Mri) -> Option<String> {
    let mri = mri.as_str();
    let id = mri.strip_prefix("8:orgid:").or_else(|| mri.strip_prefix("28:"))?;
    is_guid(id).then(|| id.to_ascii_lowercase())
}

/// Whether an `add`'s chat and meeting are its sender's own (`from`, whom the call registry holds to the call's caller):
/// a `groupChat` must name exactly the 1:1 chat of `from` and the add's one invitee (`19:<id>_<id>@unq.gbl.spaces`, the
/// two ids in either order: the capture doesn't show which order a bot's chat takes), and a `meetingInfo` must name
/// `from`'s object id as its organizer. An add naming a chat with two or more invitees, any other chat, an `add` whose
/// ids aren't the forms a 1:1 chat id carries, or another organizer names something the fence never reads: unreadable.
fn names_own_chat(request: &Value, from: &Mri, invitees: &[Mri]) -> bool {
    let caller = chat_member_id(from);
    let chat_fits = match request.get("groupChat") {
        None => true,
        Some(chat) => {
            let ([invitee], Some(caller)) = (invitees, caller.as_deref()) else {
                return false;
            };
            let Some(invitee) = chat_member_id(invitee).filter(|invitee| invitee != caller) else {
                return false;
            };
            let own = [
                format!("19:{caller}_{invitee}{ONE_TO_ONE_SUFFIX}"),
                format!("19:{invitee}_{caller}{ONE_TO_ONE_SUFFIX}"),
            ];
            chat.get("threadId")
                .and_then(Value::as_str)
                .is_some_and(|thread| own.iter().any(|id| id == thread))
        }
    };
    let meeting_fits = match request.pointer("/participantInvitationData/invitationData/meetingInfo") {
        None => true,
        Some(meeting) => {
            caller.is_some_and(|caller| meeting.get("organizerId").and_then(Value::as_str) == Some(&caller))
        }
    };
    chat_fits && meeting_fits
}
