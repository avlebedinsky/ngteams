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
//! And it maps one write: [`call_start`], a new call of the caller and its callees, judged by the fence like a new chat.
//! Writes on a call that exists (adding a participant, acknowledgements, leaving) stay unknown: the fence can't read a
//! call's members yet (`docs/backlog/fence-call-follow-ups.md`).
//!
//! Every recognizer reads the whole frame or body strictly ([`crate::classify`]'s reader: one JSON value, no two keys
//! alike in any case) and answers `None` for anything off the reviewed shape, which the fence then denies as unknown.

use serde_json::{Map, Value};
use url::Url;

use teams_fence::{Mri, Scope, Target, WriteKind};

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

/// The origins of the web client, where the registrar is.
const TEAMS_ORIGINS: [&str; 2] = ["https://teams.microsoft.com", "https://teams.cloud.microsoft"];

/// The registrar's path for an endpoint's registration.
const REGISTRATIONS: &str = "/registrar/prod/V2/registrations";

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

/// What a mutating request is to a run that calls: [`OwnKind::EndpointRegistration`] for registering the endpoint's
/// own Trouter URL (see the module docs), `None` for anything else.
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

/// The call controller's conversations endpoint, where a call starts.
const CONVERSATIONS_ORIGIN: &str = "https://api.flightproxy.teams.microsoft.com";
const CONVERSATIONS_PATH: &str = "/api/v2/epconv";

/// The conversation scenario of the web client's test call. Its start names no callee: the client opens a conversation
/// of its own and then adds the echo participant with the conversation's `add` (seen 2026-10-20).
pub const TEST_CALL_SCENARIO: &str = "UserInitiatedTestCall";

/// The top-level keys of a call start (seen 2026-10-20); a key outside them makes the request unknown.
const CALL_START_KEYS: [&str; 15] = [
    "conversationRequest",
    "contentSharing",
    "participants",
    "capabilities",
    "endpointCapabilities",
    "clientEndpointCapabilities",
    "endpointMetadata",
    "groupContext",
    "groupChat",
    "meetingInfo",
    "meetingData",
    "endpointState",
    "callInvitation",
    "debugContent",
    "participantPropertyBag",
];

/// The keys that would put the call into something that exists already (a group, a group chat, a meeting) or share
/// content into one: each must be `null` or missing for a call that creates its own conversation.
const EXISTING_TARGET_KEYS: [&str; 5] = [
    "groupContext",
    "groupChat",
    "meetingInfo",
    "meetingData",
    "contentSharing",
];

/// The write a call start makes: [`WriteKind::Call`] on [`Scope::New`], its participants the caller
/// (`participants.from.id`) and every callee (`participants.to[].id`). The test call names no callee; the participant
/// the call is for, the echo service the client adds right after, is counted in: `test_call_callee` (the echo service
/// `lab.toml` approves), so the fence lets a test call start only when it would admit the echo too, and without one it is
/// `None`. (The target is what the fence authorizes, not the conversation's members at that moment, which are the caller
/// alone.) Any other scenario, a body off the reviewed shape, or a call into something that exists already is `None`
/// (unknown).
#[must_use]
pub fn call_start(method: &str, url: &Url, body: Option<&[u8]>, test_call_callee: Option<&Mri>) -> Option<Target> {
    if method != "POST"
        || url.origin().ascii_serialization() != CONVERSATIONS_ORIGIN
        || url.path() != CONVERSATIONS_PATH
    {
        return None;
    }
    let Value::Object(start) = strict_json(body?)? else {
        return None;
    };
    let reviewed = start.keys().all(|key| CALL_START_KEYS.contains(&key.as_str()))
        && EXISTING_TARGET_KEYS
            .iter()
            .all(|key| start.get(*key).is_none_or(Value::is_null))
        && start.get("callInvitation").is_some_and(Value::is_object);
    let request = start.get("conversationRequest")?.as_object()?;
    if !reviewed || !request.get("conversationType").is_none_or(Value::is_null) {
        return None;
    }
    let (caller, callees) = participants(start.get("participants")?)?;
    let callees = match request.get("scenario") {
        None if !callees.is_empty() => callees,
        Some(Value::String(scenario)) if scenario == TEST_CALL_SCENARIO && callees.is_empty() => {
            vec![test_call_callee?.clone()]
        }
        _ => return None,
    };
    Some(Target::new(WriteKind::Call, Scope::New).with_added(std::iter::once(caller).chain(callees)))
}

/// `{"from":{"id":…,…},"to":[{"id":…,…},…]}`: the caller and the callees, each by its MRI.
fn participants(participants: &Value) -> Option<(Mri, Vec<Mri>)> {
    let participants = participants.as_object()?;
    if !participants.keys().all(|key| ["from", "to"].contains(&key.as_str())) {
        return None;
    }
    let id = |participant: &Value| participant.as_object()?.get("id")?.as_str().map(Mri::new);
    let caller = id(participants.get("from")?)?;
    let callees = participants
        .get("to")?
        .as_array()?
        .iter()
        .map(id)
        .collect::<Option<Vec<_>>>()?;
    Some((caller, callees))
}
