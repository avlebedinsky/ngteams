//! The registrar registration of the endpoint's push URL, for the messaging stack only, and its renewal: the reviewed
//! own-state request [`EndpointRegistration`] (`teams_proto`), sent through the account's transport, which logs it to
//! `fence.log` first and takes the `ic3` token from the session.
//!
//! As the web client's messaging stack does it (`docs/protocol/trouter.md`): a registration on every
//! `trouter.connected`, again [`RegistrationTimes::renew_before`] before its TTL ends, and after a failure again once
//! [`RegistrationTimes::retry_after`] has passed. The calling stack's registration (`SkypeSpacesWeb`) is never made:
//! it would make the account reachable for calls.
//!
//! Each registration goes out through `teams_proto::register`, which gives the endpoint it registered once the
//! registrar took it (a `teams_proto::RegisteredEndpoint`): the one id the account's own availability may name (M2 Task
//! 19). The loop hands it on with the registration's report.

use std::sync::Arc;
use std::time::Duration;

use teams_proto::{EndpointRegistrar, EndpointRegistration, REGISTRATION_TTL, RegisteredEndpoint, register};
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

/// What one registration came to: its report, and the endpoint it registered when it went through.
pub(crate) type Outcome = (RegistrationReport, Option<RegisteredEndpoint>);

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
        registration: EndpointRegistration,
        times: RegistrationTimes,
        reports: mpsc::UnboundedSender<Outcome>,
    ) -> Self
    where
        T: EndpointRegistrar + Send + 'static,
    {
        let task = tokio::spawn(async move {
            loop {
                let (outcome, wait) = match register(&*transport, &registration).await {
                    Ok((_, Some(endpoint))) => (
                        (RegistrationReport::Registered, Some(endpoint)),
                        times.ttl.saturating_sub(times.renew_before),
                    ),
                    Ok((answer, None)) => (
                        (
                            RegistrationReport::Failed(format!("the registrar answered HTTP {}", answer.status())),
                            None,
                        ),
                        times.retry_after,
                    ),
                    Err(error) => ((RegistrationReport::Failed(error.to_string()), None), times.retry_after),
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
