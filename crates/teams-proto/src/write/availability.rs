//! Typed requests for the account's availability and manual status, tied to an endpoint registered for that account.

use serde_json::json;
use teams_fence::Mri;

use super::Prepared;
use super::Wire;
use crate::endpoint::{Endpoint, Method, Service};
use crate::own::RegisteredEndpoint;

/// The header naming the endpoint a presence write is for (every captured one carried it).
const ENDPOINT_HEADER: &str = "x-ms-endpoint-id";

/// How the endpoint reports its activity (`activityReporting`) and what it is (`deviceType`), as every capture had them.
const ACTIVITY_REPORTING: &str = "Transport";
const DEVICE_TYPE: &str = "Web";

/// `PUT v1/me/<name>/` on UPS (the transport adds the trailing slash), naming `endpoint` in its header.
fn wire(name: &str, endpoint: &RegisteredEndpoint, body: Option<serde_json::Value>) -> Wire {
    Wire {
        raw: None,
        service: Service::Presence,
        method: Method::Put,
        endpoint: Endpoint::new(["v1", "me", name]),
        headers: vec![(ENDPOINT_HEADER, endpoint.id().to_owned())],
        body,
    }
}

/// UPS's presence endpoint `PUT` (`v1/me/endpoints/`): the account's endpoint is up and the account is available, what
/// makes it show online to others (M2 Task 18: needed and enough). Its body is exactly the captured one, `{"id":
/// <endpoint>, "availability": "Available", "activity": "Available", "activityReporting": "Transport", "deviceType":
/// "Web"}`; UPS answered `201` with no body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetEndpointAvailability {
    account: Mri,
    endpoint: RegisteredEndpoint,
}

impl SetEndpointAvailability {
    /// `account`'s registered `endpoint`, available; `None` unless `endpoint` was registered for `account`
    /// ([`RegisteredEndpoint::is_for`]).
    #[must_use]
    pub fn available(account: Mri, endpoint: &RegisteredEndpoint) -> Option<Self> {
        endpoint.is_for(&account).then(|| Self {
            account,
            endpoint: endpoint.clone(),
        })
    }

    #[must_use]
    pub fn account(&self) -> &Mri {
        &self.account
    }

    #[must_use]
    pub fn endpoint(&self) -> &RegisteredEndpoint {
        &self.endpoint
    }
}

impl Prepared for SetEndpointAvailability {
    fn wire(&self) -> Option<Wire> {
        let body = json!({
            "id": self.endpoint.id(),
            "availability": "Available",
            "activity": "Available",
            "activityReporting": ACTIVITY_REPORTING,
            "deviceType": DEVICE_TYPE,
        });
        Some(wire("endpoints", &self.endpoint, Some(body)))
    }
}

/// A manual status the account menu offered and a capture showed set (M2 Task 18, `own-presence-manual-held`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ManualAvailability {
    /// "Busy".
    Busy,
    /// "Do not disturb".
    DoNotDisturb,
    /// "Appear away".
    Away,
    /// "Available" chosen by hand: it forces available (only the reset clears a manual status).
    Available,
}

impl ManualAvailability {
    pub const ALL: [Self; 4] = [Self::Busy, Self::DoNotDisturb, Self::Away, Self::Available];

    /// The value the request carries (`availability`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Busy => "Busy",
            Self::DoNotDisturb => "DoNotDisturb",
            Self::Away => "Away",
            Self::Available => "Available",
        }
    }
}

/// The manual status (`v1/me/forceavailability/`): set ([`Self::set`], a body of exactly `{"availability": …}`) or
/// cleared ([`Self::reset`], the same `PUT` with no body at all). UPS answered `200` with no body to each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForceAvailability {
    account: Mri,
    endpoint: RegisteredEndpoint,
    /// `None`: the reset.
    status: Option<ManualAvailability>,
}

impl ForceAvailability {
    /// Sets `account`'s manual status to `status`, from its registered `endpoint`; `None` unless `endpoint` was
    /// registered for `account` ([`RegisteredEndpoint::is_for`]).
    #[must_use]
    pub fn set(account: Mri, endpoint: &RegisteredEndpoint, status: ManualAvailability) -> Option<Self> {
        Self::from_endpoint(account, endpoint, Some(status))
    }

    /// Clears `account`'s manual status, from its registered `endpoint`; `None` unless `endpoint` was registered for
    /// `account` ([`RegisteredEndpoint::is_for`]).
    #[must_use]
    pub fn reset(account: Mri, endpoint: &RegisteredEndpoint) -> Option<Self> {
        Self::from_endpoint(account, endpoint, None)
    }

    fn from_endpoint(account: Mri, endpoint: &RegisteredEndpoint, status: Option<ManualAvailability>) -> Option<Self> {
        endpoint.is_for(&account).then(|| Self {
            account,
            endpoint: endpoint.clone(),
            status,
        })
    }

    #[must_use]
    pub fn account(&self) -> &Mri {
        &self.account
    }

    #[must_use]
    pub fn endpoint(&self) -> &RegisteredEndpoint {
        &self.endpoint
    }

    /// The status it sets; `None` for the reset.
    #[must_use]
    pub fn status(&self) -> Option<ManualAvailability> {
        self.status
    }
}

impl Prepared for ForceAvailability {
    fn wire(&self) -> Option<Wire> {
        let body = self.status.map(|status| json!({"availability": status.as_str()}));
        Some(wire("forceavailability", &self.endpoint, body))
    }
}
