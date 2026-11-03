//! The registrar registration of the endpoint's push URL and its renewal: the messaging stack's
//! ([`EndpointRegistration`], `TeamsCDLWebWorker`) or the calling stack's ([`CallingRegistration`], `SkypeSpacesWeb`),
//! each sent through the account's transport ([`teams_proto::Transport::send_own`]) with the session's `ic3` token.
//! The transport's diagnostic wire logger records each request and response at `debug`, with credentials redacted.
//!
//! As the web client's stacks do it (`docs/protocol/trouter.md`): a registration on every `trouter.connected` (the
//! calling stack's after a dropped socket too, `docs/protocol/calling.md`), again [`RegistrationTimes::renew_before`]
//! before its TTL ends, and after a failure again once [`RegistrationTimes::retry_after`] has passed. The calling
//! stack's registration makes the account reachable for calls; the call controller decides each typed response.
//!
//! The messaging registration goes out through `teams_proto::register`, which gives the endpoint it registered once the
//! registrar took it (a `teams_proto::RegisteredEndpoint`): the one id the account's own availability may name (M2 Task
//! 19). The loop hands it on with the registration's report. The calling registration registers no such endpoint
//! (`teams_proto::register_calling`).

use std::sync::Arc;
use std::time::Duration;

use teams_proto::{
    CallingRegistration, EndpointRegistrar, EndpointRegistration, REGISTRATION_TTL, RegisteredEndpoint, register,
    register_calling,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// When the registration is renewed or retried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegistrationTimes {
    /// How long one lasts ([`REGISTRATION_TTL`], what it asks for).
    pub ttl: Duration,
    /// How long before the TTL ends it is renewed (30 s, the web client's).
    pub renew_before: Duration,
    /// How long after a failure it is tried again (123 s, the web client's).
    pub retry_after: Duration,
}

impl Default for RegistrationTimes {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(u64::from(REGISTRATION_TTL)),
            renew_before: Duration::from_secs(30),
            retry_after: Duration::from_secs(123),
        }
    }
}

/// How a registration went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistrationReport {
    /// Answered with a success.
    Registered,
    /// Refused or failed (the transport's error, which carries no token or body); tried again later.
    Failed(String),
}

/// What one registration came to: its report, and the endpoint it registered when it went through (the messaging
/// stack's only).
pub(crate) type Outcome = (RegistrationReport, Option<RegisteredEndpoint>);

/// Which stack's registration a loop keeps.
pub(crate) enum Registration {
    Messaging(EndpointRegistration),
    Calling(CallingRegistration),
}

/// The registration loop of one connection; dropping it stops the loop.
pub(crate) struct Registrar {
    task: JoinHandle<()>,
}

impl Drop for Registrar {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Registrar {
    /// Registers `registration` through `transport` now and keeps it registered, reporting each attempt.
    pub(crate) fn start<T>(
        transport: Arc<T>,
        registration: Registration,
        times: RegistrationTimes,
        reports: mpsc::UnboundedSender<Outcome>,
    ) -> Self
    where
        T: EndpointRegistrar + Send + 'static,
    {
        let task = tokio::spawn(async move {
            loop {
                let outcome = once(&*transport, &registration).await;
                let wait = if outcome.0 == RegistrationReport::Registered {
                    times.ttl.saturating_sub(times.renew_before)
                } else {
                    times.retry_after
                };
                if reports.send(outcome).is_err() {
                    return;
                }
                tokio::time::sleep(wait).await;
            }
        });
        Self { task }
    }
}

/// One registration and what it came to.
async fn once<T: EndpointRegistrar>(transport: &T, registration: &Registration) -> Outcome {
    let refused = |status: u16| RegistrationReport::Failed(format!("the registrar answered HTTP {status}"));
    match registration {
        Registration::Messaging(registration) => match register(transport, registration).await {
            Ok((_, Some(endpoint))) => (RegistrationReport::Registered, Some(endpoint)),
            Ok((answer, None)) => (refused(answer.status()), None),
            Err(error) => (RegistrationReport::Failed(error.to_string()), None),
        },
        Registration::Calling(registration) => match register_calling(transport, registration).await {
            Ok(answer) if answer.is_success() => (RegistrationReport::Registered, None),
            Ok(answer) => (refused(answer.status()), None),
            Err(error) => (RegistrationReport::Failed(error.to_string()), None),
        },
    }
}
