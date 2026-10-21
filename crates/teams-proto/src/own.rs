//! Own-state requests: the reviewed list of requests that aren't reads but touch only the signed-in account's own
//! state (registering its own endpoint, say), sent without a membership check.
//!
//! The list is the set of types implementing [`OwnState`], and the trait is sealed, so only this crate adds to it.
//! It started empty (M1 Task 5): each entry is added by the task that captured and reviewed it, in its exact shape
//! (method, service, path, body schema), the way `proto-lab`'s browser fence adds its own-state kinds. The transport
//! logs every own-state request to `fence.log` before it sends it, and sends none it couldn't log.
//!
//! The entries:
//! - [`EndpointRegistration`] (M1 Task 16): the registrar registration of the account's own Trouter endpoint for the
//!   **messaging** stack, in the shape the web client's messaging stack sent (M1 Task 14, `docs/protocol/trouter.md`),
//!   so the account's chat events reach its push socket. Never the calling stack's (`SkypeSpacesWeb`): that one makes
//!   the account reachable for calls.

use serde_json::{Value, json};
use url::Url;

use crate::endpoint::{Endpoint, Method, Service};

/// The messaging stack's app on the registrar, exactly (`clientDescription.appId`).
pub const MESSAGING_APP_ID: &str = "TeamsCDLWebWorker";

/// The messaging stack's template (`clientDescription.templateKey`): the app and a version, as captured.
pub const MESSAGING_TEMPLATE: &str = "TeamsCDLWebWorker_2.6";

/// The client build the registration and the Trouter connect name (`platformUIVersion`, and the connect's `tc` `v`):
/// the web client's in the captures of 2026-10-20.
pub const CLIENT_BUILD: &str = "1415/26091712226";

/// How long a registration lasts, in seconds (`transports.TROUTER[].ttl`), as every captured one asked.
pub const REGISTRATION_TTL: u32 = 3600;

/// The host suffix of every Trouter host (`go.`, `<name>-t.`, `<name>-f.`).
pub const TROUTER_DOMAIN: &str = ".trouter.teams.microsoft.com";

/// A reviewed own-state request (see the module docs). Sealed: no other crate can implement it.
pub trait OwnState: sealed::Sealed {
    /// The entry's name, logged to `fence.log` with every request of it.
    const KIND: &'static str;

    fn service(&self) -> Service;
    fn method(&self) -> Method;
    fn endpoint(&self) -> Endpoint;
    /// The JSON body, if it has one.
    fn body(&self) -> Option<Value>;
}

mod sealed {
    /// The seal on [`OwnState`](super::OwnState): a public trait no other crate can name. Its implementations are the
    /// reviewed list.
    pub trait Sealed {}

    impl Sealed for super::EndpointRegistration {}

    #[cfg(feature = "test-support")]
    impl Sealed for super::ProbeOwnState {}
}

/// `POST <AFD host>/registrar/prod/V2/registrations`: the registration of the account's own Trouter endpoint for the
/// messaging stack (see the module docs). The registrar takes the account from the request's token; the body names
/// only the endpoint (its `registrationId`, the socket's `epid`) and its push URL (the `surl` the endpoint's
/// `trouter.connected` handed over). Answered `202` with no body. Every other field is the captured value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointRegistration {
    registration_id: String,
    push_url: String,
}

impl EndpointRegistration {
    /// The registration of the endpoint `registration_id` (a GUID in lowercase hex with dashes, the socket's `epid`)
    /// at `push_url`; `None` unless `push_url` is a Trouter push URL as written: `https`, a host `<name>` +
    /// [`TROUTER_DOMAIN`] (letters, digits, `-`), an optional port, the path `/v4/f/<endpoint id>/` (base64url: letters,
    /// digits, `-`, `_`), no query, fragment or user info.
    #[must_use]
    pub fn messaging(registration_id: &str, push_url: &str) -> Option<Self> {
        (is_guid(registration_id) && is_push_url(push_url)).then(|| Self {
            registration_id: registration_id.to_owned(),
            push_url: push_url.to_owned(),
        })
    }

    #[must_use]
    pub fn registration_id(&self) -> &str {
        &self.registration_id
    }

    #[must_use]
    pub fn push_url(&self) -> &str {
        &self.push_url
    }
}

impl OwnState for EndpointRegistration {
    const KIND: &'static str = "endpoint_registration";

    fn service(&self) -> Service {
        Service::Registrar
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["prod", "V2", "registrations"])
    }

    fn body(&self) -> Option<Value> {
        Some(json!({
            "clientDescription": {
                "appId": MESSAGING_APP_ID,
                "aesKey": "",
                "languageId": "en-US",
                "platform": "chrome",
                "templateKey": MESSAGING_TEMPLATE,
                "platformUIVersion": CLIENT_BUILD,
            },
            "registrationId": self.registration_id,
            "nodeId": "",
            "transports": {
                "TROUTER": [{"context": "", "path": self.push_url, "ttl": REGISTRATION_TTL}],
            },
        }))
    }
}

/// A GUID as `8-4-4-4-12` lowercase hex digits.
fn is_guid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups.iter().all(|group| {
            group
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

/// A Trouter push URL (see [`EndpointRegistration::messaging`]).
fn is_push_url(text: &str) -> bool {
    let Ok(url) = Url::parse(text) else {
        return false;
    };
    let name = url
        .host_str()
        .and_then(|host| host.strip_suffix(TROUTER_DOMAIN))
        .unwrap_or_default();
    let endpoint = url
        .path()
        .strip_prefix("/v4/f/")
        .and_then(|rest| rest.strip_suffix('/'))
        .unwrap_or_default();
    url.scheme() == "https"
        && !name.is_empty()
        && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && !endpoint.is_empty()
        // the endpoint id is base64url: letters, digits, `-` and `_` (live ids carry both)
        && endpoint
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        // the URL as it was given, not one the parser normalized
        && url.as_str() == text
}

/// An own-state request of the test's choosing, `POST`ed: the transport's tests need one beyond the reviewed list.
/// Tests only (`test-support`), which a lab binary refuses to run with.
#[cfg(feature = "test-support")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeOwnState {
    service: Service,
    endpoint: Endpoint,
    body: Option<Value>,
}

#[cfg(feature = "test-support")]
impl ProbeOwnState {
    #[must_use]
    pub fn new(service: Service, endpoint: Endpoint, body: Option<Value>) -> Self {
        Self {
            service,
            endpoint,
            body,
        }
    }
}

#[cfg(feature = "test-support")]
impl OwnState for ProbeOwnState {
    const KIND: &'static str = "test_probe";

    fn service(&self) -> Service {
        self.service
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        self.endpoint.clone()
    }

    fn body(&self) -> Option<Value> {
        self.body.clone()
    }
}
