//! The calling stack under the fence (M0 Task 18, `docs/protocol/calling-echo.md`).
//!
//! The web client's calling stack comes up only on a working Trouter session: its push channel, over which the call
//! controller calls the endpoint back. A run that calls (`echo-call`) therefore lets through, as reviewed own-state of
//! the signed-in account, what sets up that account's own endpoint, and nothing that reaches another person:
//! - [`trouter_frame`] ([`OwnKind::TrouterSession`]): on the Trouter sockets, the authentication frame (exactly the
//!   `user.authenticate` event, its one argument holding only reviewed headers, an `Authorization` among them, and
//!   the reconnect parameters the service handed over), the `ping` event, the client's own `user.activity`, its
//!   acknowledgement of the service's message-loss report, and the disconnect. Not the responses to what the service
//!   delivers (`3:::`): they go back to whoever sent the delivery, so they stay unknown;
//! - [`own_request`]: registering the endpoint's own Trouter URL with the registrar ([`OwnKind::EndpointRegistration`]),
//!   which makes the account reachable (accepted by the user, 2026-10-19; an incoming call is never answered: no
//!   answer is mapped), and reading the account's own settings and policies by `POST` ([`OwnKind::SettingsRead`]).
//!
//! And it maps one write: [`call_start`], the web client's test call, a new call of the caller and the echo service,
//! judged by the fence like a new chat. Only that captured shape maps; a call to anyone else is unknown. Writes on a
//! call that exists (adding a participant, acknowledgements, leaving) stay unknown: the fence can't read a call's
//! members yet (`docs/backlog/fence-call-follow-ups.md`).
//!
//! A run that chats (M1 Task 14, `docs/protocol/trouter.md`) uses a narrower **messaging set**: the same Trouter
//! session frames ([`trouter_frame`], `user.activity` included: the account shows online while the client runs,
//! accepted by the user) and the registration of the messaging stack's endpoint only ([`messaging_request`],
//! [`MESSAGING_APP`]), never the calling stack's, no settings reads and no call. Deliveries stay unanswered in both
//! sets.
//!
//! Every recognizer reads the whole frame or body strictly ([`crate::classify`]'s reader: one JSON value, no two keys
//! alike in any case) and answers `None` for anything off the reviewed shape, which the fence then denies as unknown.

use serde_json::{Map, Value};
use url::{Position, Url};

use teams_fence::{Mri, Scope, Target, WriteKind};

use crate::arming::TEAMS_ORIGINS;
use crate::classify::{OwnKind, strict_json};

/// The Trouter service's domain: the socket opens on `go.<domain>` and moves to a regional `<name>.<domain>`.
pub const TROUTER_DOMAIN: &str = "trouter.teams.microsoft.com";

/// The Trouter socket's paths, as the web client opens it (with and without the trailing slash); its query, the
/// client's description and endpoint id, plays no part.
const TROUTER_PATHS: [&str; 2] = ["/v4/c/", "/v4/c"];

/// The headers the authentication frame may carry besides the account's own bearer token (`Authorization`): two
/// client flags.
const AUTHENTICATE_FLAGS: [&str; 2] = ["X-Ms-Test-User", "X-MS-Migration"];

/// The reconnect parameters the service hands the client for the regional socket, which it sends back as they were.
const CONNECT_PARAMS: [&str; 7] = ["issuer", "scae", "sig", "sr", "sp", "se", "st"];

/// What the endpoint's registration describes the client with.
const CLIENT_DESCRIPTION: [&str; 6] = [
    "appId",
    "aesKey",
    "languageId",
    "platform",
    "templateKey",
    "platformUIVersion",
];

/// The ports the Trouter service hands out for an endpoint's push URL (`surl` on 3443, `url` on 8443), or none.
const PUSH_PORTS: [Option<u16>; 3] = [None, Some(3443), Some(8443)];

/// The setting groups the web client reads with `useraggregatesettings` (seen 2026-10-20). Each names a group of the
/// account's or its tenant's settings; the request returns them and changes nothing.
const AGGREGATE_SETTINGS: [&str; 18] = [
    "adminSettings",
    "clientSettings",
    "deviceStoreMetadata",
    "educationSettings",
    "smbTenantSettings",
    "teamsAcsFederationConfiguration",
    "teamsLinkPolicy",
    "teamsMessagingConfiguration",
    "teamsOfficeCloudPolicy",
    "teamsUserPhoneNumbers",
    "tenantSettingsV2",
    "tenantSiteUrlAndDomains",
    "tenantVoiceSettings",
    "userCopilotSettings",
    "userLanguageAccountSettings",
    "userPropertiesSettings",
    "userResourcesSettings",
    "voiceAdminSettings",
];

/// The policies the web client reads with `effectivePolicies` (seen 2026-10-20): the account's effective policy of
/// each name.
const EFFECTIVE_POLICIES: [&str; 37] = [
    "byot",
    "callingLineIdentity",
    "clientPolicy",
    "externalAccessPolicy",
    "locationPolicy",
    "onlineDialinConferencingPolicy",
    "onlineVoicemailPolicy",
    "teamsAIPolicy",
    "teamsAppPermissionPolicy",
    "teamsAppSetupPolicy",
    "teamsBYODAndDesksPolicy",
    "teamsBranchSurvivabilityPolicy",
    "teamsCallParkPolicy",
    "teamsCallingPolicy",
    "teamsChannelsPolicy",
    "teamsEducationAssignmentsAppPolicy",
    "teamsEmergencyCallRoutingPolicy",
    "teamsEmergencyCallingPolicy",
    "teamsEnhancedEncryptionPolicy",
    "teamsEventsPolicy",
    "teamsFeedbackPolicy",
    "teamsFilesPolicy",
    "teamsInteropPolicy",
    "teamsMediaConnectivityPolicy",
    "teamsMediaLoggingPolicy",
    "teamsMeetingBrandingPolicy",
    "teamsMeetingBroadcastPolicy",
    "teamsMeetingPolicy",
    "teamsMessagingPolicy",
    "teamsPhoneNumberPolicies",
    "teamsSyntheticAutomatedCallPolicy",
    "teamsTargetingPolicy",
    "teamsUpdateManagementPolicy",
    "teamsVdiPolicy",
    "teamsVirtualAppointmentsPolicy",
    "teamsVoiceApplicationsPolicy",
    "tenantDialPlan",
];

/// The registrar's path for an endpoint's registration.
const REGISTRATIONS: &str = "/registrar/prod/V2/registrations";

/// The app the messaging stack registers its endpoint as (`clientDescription.appId`), and the start of its template
/// (`templateKey`, `TeamsCDLWebWorker_<version>`). The calling stack registers as `SkypeSpacesWeb`.
pub const MESSAGING_APP: &str = "TeamsCDLWebWorker";

/// What a text frame on a socket at `url` is to a run that calls: [`OwnKind::TrouterSession`] for the Trouter
/// session's own frames (see the module docs), `None` for anything else.
#[must_use]
pub fn trouter_frame(url: &str, text: &str) -> Option<OwnKind> {
    let url = Url::parse(url).ok()?;
    if url.scheme() != "wss"
        || url.port().is_some()
        || !url.host_str().is_some_and(is_trouter_host)
        || !TROUTER_PATHS.contains(&url.path())
    {
        return None;
    }
    let session = text == "0::"
        || acknowledged_event(text).is_some_and(|event| is_ping(&event) || is_loss_ack(&event) || is_activity(&event))
        || text.strip_prefix("5:::").is_some_and(is_authenticate);
    session.then_some(OwnKind::TrouterSession)
}

/// A host of the Trouter service: one label under [`TROUTER_DOMAIN`], of lowercase letters, digits and dashes.
fn is_trouter_host(host: &str) -> bool {
    host.strip_suffix(TROUTER_DOMAIN)
        .and_then(|label| label.strip_suffix('.'))
        .is_some_and(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

/// The event of `5:<id>+::<json>`: an event with a numeric id that asks for an acknowledgement, no endpoint. The
/// client sends its session's events this way; it closes and opens the socket again when one goes unacknowledged.
fn acknowledged_event(text: &str) -> Option<Map<String, Value>> {
    let (id, event) = text.strip_prefix("5:")?.split_once("+::")?;
    let numeric = !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit());
    numeric.then(|| json_object(event)).flatten()
}

/// `{"name":"ping"}`: the keep-alive.
fn is_ping(event: &Map<String, Value>) -> bool {
    event.len() == 1 && event.get("name").and_then(Value::as_str) == Some("ping")
}

/// The one argument of an event named `name` with nothing beside its name and arguments.
fn only_argument<'a>(event: &'a Map<String, Value>, name: &str) -> Option<&'a Map<String, Value>> {
    if event.len() != 2 || event.get("name").and_then(Value::as_str) != Some(name) {
        return None;
    }
    match event.get("args")?.as_array()?.as_slice() {
        [argument] => argument.as_object(),
        _ => None,
    }
}

/// `{"name":"trouter.processed_message_loss","args":[{"droppedIndicators":[{"tag":…,"etag":…},…]}]}`: the client
/// acknowledging the service's report of what it missed while away, the indicators handed back as they came.
fn is_loss_ack(event: &Map<String, Value>) -> bool {
    only_argument(event, "trouter.processed_message_loss").is_some_and(|argument| {
        argument.len() == 1
            && argument
                .get("droppedIndicators")
                .and_then(Value::as_array)
                .is_some_and(|indicators| {
                    indicators.iter().all(|indicator| {
                        indicator
                            .as_object()
                            .is_some_and(|indicator| exactly_strings(indicator, &["tag", "etag"], &[]))
                    })
                })
    })
}

/// `{"name":"user.activity","args":[{"state":"active"|"inactive","cv":…}]}`: the client reporting its own activity,
/// which signed-in clients do by themselves (the account shows "Available"; accepted by the user, 2026-10-19). No
/// other state: the lab never sets a presence.
fn is_activity(event: &Map<String, Value>) -> bool {
    only_argument(event, "user.activity").is_some_and(|argument| {
        exactly_strings(argument, &["state", "cv"], &[])
            && argument
                .get("state")
                .and_then(Value::as_str)
                .is_some_and(|state| ["active", "inactive"].contains(&state))
    })
}

/// `{"name":"user.authenticate","args":[{"headers":{…}, "connectparams":{…}}]}`, nothing more: the headers all
/// reviewed strings, with an `Authorization`; the reconnect parameters, if there, all reviewed strings.
fn is_authenticate(json: &str) -> bool {
    let Some(event) = json_object(json) else {
        return false;
    };
    if event.len() != 2 || event.get("name").and_then(Value::as_str) != Some("user.authenticate") {
        return false;
    }
    let Some([argument]) = event.get("args").and_then(Value::as_array).map(Vec::as_slice) else {
        return false;
    };
    let Some(argument) = argument.as_object() else {
        return false;
    };
    let headers = argument.get("headers").and_then(Value::as_object);
    let params = argument.get("connectparams");
    let expected_len = 1 + usize::from(params.is_some());
    argument.len() == expected_len
        && headers.is_some_and(|headers| exactly_strings(headers, &["Authorization"], &AUTHENTICATE_FLAGS))
        && params.is_none_or(|params| {
            params
                .as_object()
                .is_some_and(|params| exactly_strings(params, &CONNECT_PARAMS, &[]))
        })
}

/// Whether a text frame is an acknowledged event named `user.activity`, whatever its argument: what
/// [`crate::fence_ws::FrameClassifier::without_activity`] holds back, to see how the service treats an endpoint that
/// never reports its activity.
#[must_use]
pub fn is_activity_event(text: &str) -> bool {
    acknowledged_event(text).is_some_and(|event| event.get("name").and_then(Value::as_str) == Some("user.activity"))
}

/// Whether `object` has every key of `required`, no key outside `required` and `optional`, and only strings.
fn exactly_strings(object: &Map<String, Value>, required: &[&str], optional: &[&str]) -> bool {
    required.iter().all(|key| object.contains_key(*key))
        && object.iter().all(|(key, value)| {
            (required.contains(&key.as_str()) || optional.contains(&key.as_str())) && value.is_string()
        })
}

/// `text` as one JSON object read strictly, with nothing around it (not even whitespace).
fn json_object(text: &str) -> Option<Map<String, Value>> {
    if text.trim() != text {
        return None;
    }
    match strict_json(text.as_bytes())? {
        Value::Object(object) => Some(object),
        _ => None,
    }
}

/// What a mutating request is to a run that calls, on the web client's origins ([`TEAMS_ORIGINS`]):
/// [`OwnKind::EndpointRegistration`] for registering the endpoint's own Trouter URL, [`OwnKind::SettingsRead`] for
/// reading the account's own settings (`useraggregatesettings`) or effective policies (`effectivePolicies`) by `POST`
/// (see the module docs), `None` for anything else.
#[must_use]
pub fn own_request(method: &str, url: &Url, body: Option<&[u8]>) -> Option<OwnKind> {
    let teams = TEAMS_ORIGINS.contains(&url.origin().ascii_serialization().as_str());
    if method != "POST" || !teams {
        return None;
    }
    let body = body?;
    if url.path() == REGISTRATIONS && is_registration(body) {
        return Some(OwnKind::EndpointRegistration);
    }
    let read = match own_settings(url.path())? {
        Settings::Aggregate => is_flag_set(body),
        Settings::Policies => is_name_list(body),
    };
    read.then_some(OwnKind::SettingsRead)
}

/// What a mutating request is to a run that chats ([`crate::classify::Classifier::messaging`]), on the web client's
/// origins: [`OwnKind::EndpointRegistration`] for registering the **messaging** stack's endpoint only (the reviewed
/// registration whose app is [`MESSAGING_APP`] and whose template is `TeamsCDLWebWorker_<version>`), so the account's
/// chat events reach its Trouter socket. Never the calling stack's (`SkypeSpacesWeb`), which makes the account
/// reachable for calls; nothing else (`None`).
#[must_use]
pub fn messaging_request(method: &str, url: &Url, body: Option<&[u8]>) -> Option<OwnKind> {
    let teams = TEAMS_ORIGINS.contains(&url.origin().ascii_serialization().as_str());
    if method != "POST" || !teams || url.path() != REGISTRATIONS {
        return None;
    }
    let body = body?;
    (is_registration(body) && registers_messaging(body)).then_some(OwnKind::EndpointRegistration)
}

/// Whether a registration ([`is_registration`]) describes the messaging stack: its app exactly [`MESSAGING_APP`], its
/// template [`MESSAGING_APP`], `_` and a version of digits and dots.
fn registers_messaging(body: &[u8]) -> bool {
    let Some(registration) = strict_json(body) else {
        return false;
    };
    let field = |key| {
        registration
            .pointer(&format!("/clientDescription/{key}"))
            .and_then(Value::as_str)
    };
    let version = field("templateKey")
        .and_then(|template| template.strip_prefix(MESSAGING_APP))
        .and_then(|rest| rest.strip_prefix('_'));
    field("appId") == Some(MESSAGING_APP)
        && version.is_some_and(|version| {
            !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit() || byte == b'.')
        })
}

/// The account's own settings, which the middle tier answers to a `POST` naming what to read.
enum Settings {
    /// `useraggregatesettings`: an object of `"<setting group>": true`.
    Aggregate,
    /// `effectivePolicies`: a list of policy names.
    Policies,
}

/// `/api/mt/part/<partition>/beta/users/<useraggregatesettings|effectivePolicies>`, the partition a region and a
/// number (`amer-03`).
fn own_settings(path: &str) -> Option<Settings> {
    let rest = path.strip_prefix("/api/mt/part/")?;
    let (partition, rest) = rest.split_once('/')?;
    let (region, number) = partition.split_once('-')?;
    let partition = !region.is_empty()
        && region.bytes().all(|byte| byte.is_ascii_lowercase())
        && !number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit());
    if !partition {
        return None;
    }
    match rest {
        "beta/users/useraggregatesettings" => Some(Settings::Aggregate),
        "beta/users/effectivePolicies" => Some(Settings::Policies),
        _ => None,
    }
}

/// A non-empty JSON object of reviewed setting groups ([`AGGREGATE_SETTINGS`]), each `true`.
fn is_flag_set(body: &[u8]) -> bool {
    matches!(strict_json(body), Some(Value::Object(flags)) if !flags.is_empty()
        && flags.iter().all(|(name, flag)| AGGREGATE_SETTINGS.contains(&name.as_str()) && *flag == Value::Bool(true)))
}

/// A non-empty JSON array of reviewed policy names ([`EFFECTIVE_POLICIES`]).
fn is_name_list(body: &[u8]) -> bool {
    matches!(strict_json(body), Some(Value::Array(names)) if !names.is_empty()
        && names.iter().all(|name| name.as_str().is_some_and(|name| EFFECTIVE_POLICIES.contains(&name))))
}

/// The registration of the account's own endpoint (the registrar takes the account from the request's token; the body
/// names no other user): exactly `clientDescription` (the reviewed strings), `registrationId` and `nodeId` (strings),
/// and `transports` with Trouter push URLs only. Whichever Trouter URL it names, it routes only this account's own
/// notifications.
fn is_registration(body: &[u8]) -> bool {
    let Some(Value::Object(registration)) = strict_json(body) else {
        return false;
    };
    let Some(transports) = registration.get("transports").and_then(Value::as_object) else {
        return false;
    };
    let Some(trouter) = transports.get("TROUTER").and_then(Value::as_array) else {
        return false;
    };
    registration.len() == 4
        && registration
            .get("clientDescription")
            .and_then(Value::as_object)
            .is_some_and(|description| exactly_strings(description, &CLIENT_DESCRIPTION, &[]))
        && registration.get("registrationId").is_some_and(Value::is_string)
        && registration.get("nodeId").is_some_and(Value::is_string)
        && transports.len() == 1
        && !trouter.is_empty()
        && trouter.iter().all(is_trouter_transport)
}

/// `{"context": "<string>", "path": "https://<trouter host>[:3443|:8443]/v4/f/<endpoint>/", "ttl": <integer>}`.
fn is_trouter_transport(transport: &Value) -> bool {
    let Some(transport) = transport.as_object() else {
        return false;
    };
    let path = transport
        .get("path")
        .and_then(Value::as_str)
        .and_then(|path| Url::parse(path).ok());
    transport.len() == 3
        && transport.get("context").is_some_and(Value::is_string)
        && transport.get("ttl").is_some_and(Value::is_u64)
        && path.is_some_and(|path| {
            path.scheme() == "https"
                && path.username().is_empty()
                && path.password().is_none()
                && path.query().is_none()
                && path.fragment().is_none()
                && PUSH_PORTS.contains(&path.port())
                && path.host_str().is_some_and(is_trouter_host)
                && is_push_path(path.path())
        })
}

/// `/v4/f/<endpoint>/`: one segment of letters, digits, `-` or `_`.
fn is_push_path(path: &str) -> bool {
    path.strip_prefix("/v4/f/")
        .and_then(|rest| rest.strip_suffix('/'))
        .is_some_and(|endpoint| {
            !endpoint.is_empty()
                && endpoint
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        })
}

/// The call controller's conversations endpoint, where a call starts (its URL without a query, as the fence's verdicts
/// name it).
pub const CALL_START_URL: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/epconv";

/// The conversation scenario of the web client's test call. Its start names no callee: the client opens a conversation
/// of its own and then adds the echo participant with the conversation's `add` (seen 2026-10-20).
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
/// Only the captured shape maps: the scenario [`TEST_CALL_SCENARIO`], and every key, at every level, one the capture
/// had, its value of the type it had there (`null` where it was `null`). Another scenario, a call to anyone else, a key
/// nobody reviewed or a call into something that exists already is `None` (unknown).
#[must_use]
pub fn call_start(method: &str, url: &Url, body: Option<&[u8]>, test_call_callee: Option<&Mri>) -> Option<Target> {
    if method != "POST" || &url[..Position::AfterPath] != CALL_START_URL {
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
