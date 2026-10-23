//! The calling family's one mapped write: the web client's test call start, a new call of the caller and the echo
//! service (`docs/protocol/calling-echo.md`). Only its captured shape maps; every other mutating request on a calling
//! host is unreadable until a row maps it.

use serde_json::{Map, Value};
use url::{Position, Url};

use super::json::strict_json;
use crate::{Mri, Scope, Target, WriteKind};

/// The call controller's conversations endpoint, where a call starts (its URL without a query).
pub const CALL_START_URL: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/epconv";

/// The conversation scenario of the web client's test call. Its start names no callee: the client opens a conversation
/// of its own and then adds the echo participant with the conversation's `add`.
pub const TEST_CALL_SCENARIO: &str = "UserInitiatedTestCall";

/// What a value of a call start may be: the type the capture had there (every call start of 2026-10-20 alike).
#[derive(Clone, Copy)]
enum Shape {
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
}

/// A reviewed key of an object, the shape of its value, and whether it must be there.
struct Field {
    key: &'static str,
    shape: Shape,
    required: bool,
}

/// A key that may be missing.
const fn field(key: &'static str, shape: Shape) -> Field {
    Field {
        key,
        shape,
        required: false,
    }
}

/// A key that must be there.
const fn required(key: &'static str, shape: Shape) -> Field {
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
    field(
        "endpointMetadata",
        Shape::Object(&[field("holographicCapabilities", Shape::Count)]),
    ),
    field("groupContext", Shape::Null),
    field("groupChat", Shape::Null),
    field("meetingInfo", Shape::Null),
    field("meetingData", Shape::Null),
    field("endpointState", Shape::Object(ENDPOINT_STATE)),
    required("callInvitation", Shape::Object(CALL_INVITATION)),
    field("debugContent", Shape::Object(&[field("ecsEtag", Shape::Text)])),
    field("participantPropertyBag", Shape::Object(PROPERTY_BAG)),
];

/// The conversation the start opens: its scenario, and the endpoint's callbacks for it.
const CONVERSATION_REQUEST: &[Field] = &[
    field("conversationType", Shape::Null),
    field("subject", Shape::Null),
    field("suppressDialout", Shape::Bool),
    field(
        "roster",
        Shape::Object(&[field("type", Shape::Text), field("rosterUpdate", Shape::Text)]),
    ),
    field(
        "properties",
        Shape::Object(&[
            field("allowConversationWithoutHost", Shape::Bool),
            field("enableGroupCallEventMessages", Shape::Bool),
            field("enableGroupCallUpgradeMessage", Shape::Bool),
            field("enableGroupCallMeetupGeneration", Shape::Bool),
        ]),
    ),
    field(
        "links",
        Shape::Object(&[
            field("conversationEnd", Shape::Text),
            field("conversationUpdate", Shape::Text),
            field("localParticipantUpdate", Shape::Text),
            field("addParticipantSuccess", Shape::Text),
            field("addParticipantFailure", Shape::Text),
            field("addModalitySuccess", Shape::Text),
            field("addModalityFailure", Shape::Text),
            field("confirmUnmute", Shape::Text),
            field("receiveMessage", Shape::Text),
        ]),
    ),
    required("scenario", Shape::Text),
];

/// The caller and the callees; nobody else.
const PARTICIPANTS: &[Field] = &[
    required(
        "from",
        Shape::Object(&[
            required("id", Shape::Text),
            field("displayName", Shape::Text),
            field("endpointId", Shape::Text),
            field("participantId", Shape::Text),
            field("languageId", Shape::Text),
        ]),
    ),
    // a callee as the client names the echo bot when it adds it (`docs/protocol/calling-echo.md`)
    field(
        "to",
        Shape::Objects(&[required("id", Shape::Text), field("participantId", Shape::Text)]),
    ),
];

/// The call the start places: its modalities, the endpoint's callbacks, the client and its offer.
const CALL_INVITATION: &[Field] = &[
    field("callModalities", Shape::Texts),
    field("replaces", Shape::Null),
    field("transferor", Shape::Null),
    field("clientTransferContext", Shape::Null),
    field("customContext", Shape::Null),
    field(
        "links",
        Shape::Object(&[
            field("progress", Shape::Text),
            field("mediaAnswer", Shape::Text),
            field("acceptance", Shape::Text),
            field("redirection", Shape::Text),
            field("end", Shape::Text),
        ]),
    ),
    field(
        "clientContentForMediaController",
        Shape::Object(&[
            field("controlVideoStreaming", Shape::Text),
            field("csrcInfo", Shape::Text),
        ]),
    ),
    field(
        "pstnContent",
        Shape::Object(&[
            field("emergencyCallCountry", Shape::Text),
            field("platformName", Shape::Text),
            field("publicApiCall", Shape::Bool),
        ]),
    ),
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
    field(
        "mediaDescriptions",
        Shape::Object(&[
            field(
                "descriptions",
                Shape::Objects(&[
                    field("mid", Shape::Text),
                    field("direction", Shape::Text),
                    field("label", Shape::Text),
                ]),
            ),
            field("requestId", Shape::Count),
        ]),
    ),
    field(
        "applyChannelParameters",
        Shape::Object(&[field(
            "multiChannelParameter",
            Shape::Object(&[field("mediaParameter", Shape::Text), field("mids", Shape::Texts)]),
        )]),
    ),
    field("mediaLegId", Shape::Text),
];

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

/// Whether `value` has `shape`, all the way down.
fn fits(value: &Value, shape: Shape) -> bool {
    match shape {
        Shape::Null => value.is_null(),
        Shape::Bool => value.is_boolean(),
        Shape::Count => value.is_u64(),
        Shape::Text => value.is_string(),
        Shape::Texts => value.as_array().is_some_and(|items| items.iter().all(Value::is_string)),
        Shape::Object(fields) => value.as_object().is_some_and(|object| fits_fields(object, fields)),
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
    if method != "POST" || &url[..Position::AfterPath] != CALL_START_URL || url.query().is_some() {
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
