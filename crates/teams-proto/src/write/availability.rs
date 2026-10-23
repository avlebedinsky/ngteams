//! The account's own availability (M2 Task 19): UPS's presence endpoint `PUT` and the manual status, each a sealed
//! [`Mutating`] request in the shape the web client sent (M2 Task 18, `docs/protocol/trouter.md`, "Own presence"; the
//! synthetic fixtures `tests/fixtures/ups/endpoint-requests.json` and `manual-status-requests.json`).
//!
//! A lab account's own availability is seen across the tenant, so no participant check can pass for it; the user
//! approved a narrow exception on 2026-10-21 (CLAUDE.md, "A lab account may set its own availability"): in the
//! captured shapes, availability values only, every id a request names the account itself or its own registered
//! endpoint. So each request here:
//! - targets [`WriteKind::OwnAvailability`] on [`Scope::Account`] (the account by its MRI), which the fence permits
//!   only for its own account and without a member read (`teams_fence::Fence::check`);
//! - names the account only as UPS's `me` (the account is the token's) and the endpoint only as a
//!   [`RegisteredEndpoint`], which only a registration this run made for the account gives ([`crate::register`]):
//!   no other id can be put in one;
//! - sets only the values a capture showed: the endpoint `Available`/`Available` ([`SetEndpointAvailability`]; the
//!   captured `Busy`/`InACall` is a call's, and the native client has none), a manual status of `Busy`,
//!   `DoNotDisturb`, `Away` or `Available` ([`ManualAvailability`]), or the manual status's reset
//!   ([`ForceAvailability::reset`]). Never a status note, out-of-office, a work location, "Be right back",
//!   "Appear offline", a duration or anything else of presence.
//!
//! On the wire: `PUT <presence base>/v1/me/endpoints/` or `…/v1/me/forceavailability/` (the trailing slash, no query),
//! with the presence token, the header values every captured presence request carried (the transport's, as for the
//! presence read: `behavioroverride`, `x-ms-client-type`, `x-ms-client-user-agent`, an empty `x-ms-object-id`),
//! `Content-Type: application/json` (the reset too, as captured), and `x-ms-endpoint-id`, the registered endpoint, as
//! every captured one carried it. Nothing that names the client's components (`x-ms-client-caller`) or the session,
//! as for the presence read, and no cookie.

use serde_json::json;
use teams_fence::{Mri, Scope, Target, WriteKind};

use super::sealed::Sealed;
use super::{Mutating, Wire};
use crate::endpoint::{Endpoint, Method, Service};
use crate::own::RegisteredEndpoint;

/// The header naming the endpoint a presence write is for (every captured one carried it).
const ENDPOINT_HEADER: &str = "x-ms-endpoint-id";

/// How the endpoint reports its activity (`activityReporting`) and what it is (`deviceType`), as every capture had them.
const ACTIVITY_REPORTING: &str = "Transport";
const DEVICE_TYPE: &str = "Web";

/// The target of every own-availability write: the account itself.
fn own_target(account: &Mri) -> Target {
    Target::new(
        WriteKind::OwnAvailability,
        Scope::Account {
            mri: account.as_str().to_owned(),
        },
    )
}

/// `PUT v1/me/<name>/` on UPS (the transport adds the trailing slash), naming `endpoint` in its header.
fn wire(name: &str, endpoint: &RegisteredEndpoint, body: Option<serde_json::Value>) -> Wire {
    Wire {
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
    /// `account`'s registered `endpoint`, available.
    #[must_use]
    pub fn available(account: Mri, endpoint: &RegisteredEndpoint) -> Self {
        Self {
            account,
            endpoint: endpoint.clone(),
        }
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

impl Mutating for SetEndpointAvailability {
    fn target(&self) -> Target {
        own_target(&self.account)
    }
}

impl Sealed for SetEndpointAvailability {
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
    /// Sets `account`'s manual status to `status`, from its registered `endpoint`.
    #[must_use]
    pub fn set(account: Mri, endpoint: &RegisteredEndpoint, status: ManualAvailability) -> Self {
        Self {
            account,
            endpoint: endpoint.clone(),
            status: Some(status),
        }
    }

    /// Clears `account`'s manual status, from its registered `endpoint`.
    #[must_use]
    pub fn reset(account: Mri, endpoint: &RegisteredEndpoint) -> Self {
        Self {
            account,
            endpoint: endpoint.clone(),
            status: None,
        }
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

impl Mutating for ForceAvailability {
    fn target(&self) -> Target {
        own_target(&self.account)
    }
}

impl Sealed for ForceAvailability {
    fn wire(&self) -> Option<Wire> {
        let body = self.status.map(|status| json!({"availability": status.as_str()}));
        Some(wire("forceavailability", &self.endpoint, body))
    }
}
