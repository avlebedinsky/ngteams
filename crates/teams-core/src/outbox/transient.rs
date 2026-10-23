//! The outbox's transient kind: a write that matters only at the moment it is asked for (typing, and the account's own
//! availability: its endpoint's `PUT` and its manual status, M2 Task 19). It goes out through the outbox like every other write, each send under a fresh
//! [`Fence::check`](teams_fence::Fence::check) with its `fence.log` line and the transport's rules, but it is never a
//! row: nothing of it is stored, so nothing is checked for, tried again after an unknown outcome, adopted by a later
//! run or offered to send again. A durable row for `Control/Typing` would never settle (a `Control/*` message never
//! settles a row) and could go out again at a moment the user didn't choose.
//!
//! - A 2xx is sent; a 401 gets one more attempt at once (the transport had the session refresh the refused token),
//!   as a row does; a 429 or 503 (or a throttled token service) waits its `Retry-After` in memory and goes out from
//!   the engine's tick ([`Outbox::send_due_transients`]); a denial, any other answer and any outcome that isn't known
//!   (a network error, a timeout, another 5xx) drop it.
//! - **A transient waits once, and briefly**: it is worth sending only near the moment it was asked for, so one whose
//!   `Retry-After` is longer than [`MAX_TRANSIENT_WAIT`], or that is throttled again after its wait, is dropped; and a
//!   waiting one is dropped when what it announced has gone first ([`Outbox::drop_waiting`]: the engine calls it
//!   before a message post to its conversation), and a waiting availability when the push channel stopped
//!   ([`Outbox::drop_own_availability`]: its endpoint is no longer the account's live one).
//! - **Latest wins while one waits**: a transient asked for while one of the same ([`Transient::replaces`]) waits takes
//!   its place, and waits as long; only the newest goes out.
//! - **An availability names only the outbox's account's endpoint**: one whose endpoint was registered for another
//!   account (or for none: the registering transport's fence owned no account) is dropped before anything is asked
//!   of the fence or sent; `teams_proto` builds no write of it.

use std::sync::PoisonError;
use std::time::Duration;

use teams_proto::{
    DEFAULT_RETRY_WAIT, ForceAvailability, HttpError, ManualAvailability, RegisteredEndpoint, Response,
    SetEndpointAvailability, Transport, Typing,
};

use super::{Outbox, Tried};
use crate::engine::OwnStatus;

/// The longest `Retry-After` a transient waits out: a typing sent later than this would tell the chat of a moment long
/// gone, so one throttled for longer is dropped.
pub const MAX_TRANSIENT_WAIT: Duration = Duration::from_secs(5);

/// Why an availability naming an endpoint registered for another account (or for none) is dropped unsent.
const NOT_THIS_ACCOUNTS: &str = "the endpoint it names wasn't registered for this account, so nothing went out";

/// A write the outbox sends without a row (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transient {
    /// The account is typing in a chat (`Control/Typing`).
    Typing { conversation: String },
    /// The account's registered endpoint is up and available: UPS's presence endpoint `PUT`
    /// ([`SetEndpointAvailability`]), what makes the account show online. Its endpoint is one a registration of this
    /// run registered: no other can be named, and it goes out only when that registration was for the outbox's account.
    EndpointAvailable { endpoint: RegisteredEndpoint },
    /// The account's manual status set (`Some`) or cleared (`None`), from its registered endpoint
    /// ([`ForceAvailability`]).
    ManualStatus {
        endpoint: RegisteredEndpoint,
        status: Option<ManualAvailability>,
    },
}

impl Transient {
    /// Whether this one takes `other`'s place while `other` waits: the same kind on the same target (a typing in the
    /// same chat; the account's endpoint; its manual status, whichever the newer one sets).
    #[must_use]
    pub fn replaces(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Typing { conversation }, Self::Typing { conversation: other }) => conversation == other,
            (Self::EndpointAvailable { .. }, Self::EndpointAvailable { .. })
            | (Self::ManualStatus { .. }, Self::ManualStatus { .. }) => true,
            _ => false,
        }
    }

    /// Its kind, as a log or a printed event names it: `typing`, `endpoint_available` or `manual_status`.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Typing { .. } => "typing",
            Self::EndpointAvailable { .. } => "endpoint_available",
            Self::ManualStatus { .. } => "manual_status",
        }
    }

    /// The conversation it writes to; `None` for the account's own availability, which writes to no conversation.
    #[must_use]
    pub fn conversation(&self) -> Option<&str> {
        match self {
            Self::Typing { conversation } => Some(conversation),
            Self::EndpointAvailable { .. } | Self::ManualStatus { .. } => None,
        }
    }

    /// The account's own availability it sets, as [`EngineRequests::own_availability`](crate::EngineRequests::own_availability)
    /// takes it; `None` for a typing.
    #[must_use]
    pub fn own_status(&self) -> Option<OwnStatus> {
        match self {
            Self::Typing { .. } => None,
            Self::EndpointAvailable { .. } => Some(OwnStatus::Available),
            Self::ManualStatus {
                status: Some(status), ..
            } => Some(OwnStatus::Manual(*status)),
            Self::ManualStatus { status: None, .. } => Some(OwnStatus::ClearManual),
        }
    }

    /// The endpoint it names: the account's own availability's; `None` for a typing.
    #[must_use]
    pub fn endpoint(&self) -> Option<&RegisteredEndpoint> {
        match self {
            Self::Typing { .. } => None,
            Self::EndpointAvailable { endpoint } | Self::ManualStatus { endpoint, .. } => Some(endpoint),
        }
    }
}

/// How a transient's send went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransientSent {
    /// The service took it.
    Sent,
    /// Throttled, or another of the same waits: it goes out from the tick after `not_before` (milliseconds since the
    /// Unix epoch), unless a newer one takes its place first or it is dropped ([`Outbox::drop_waiting`]).
    Waiting { not_before: i64 },
    /// The fence denied it: nothing went out.
    Denied { reason: String },
    /// It didn't go out (refused, throttled too long or again, or what it announced went first), or whether it
    /// arrived is unknown: dropped, never sent again.
    Dropped { reason: String },
}

/// A transient waiting for its `Retry-After`.
#[derive(Debug)]
pub(super) struct Waiting {
    transient: Transient,
    not_before: i64,
}

impl<T: Transport<Error = HttpError>> Outbox<T> {
    /// Sends a transient now, unless one of the same waits: then it takes that one's place and waits as long.
    pub async fn send_transient(&self, transient: Transient) -> TransientSent {
        if let Some(not_before) = self.replace_waiting(&transient) {
            return TransientSent::Waiting { not_before };
        }
        self.try_transient(transient, false).await
    }

    /// Sends every waiting transient whose `Retry-After` has passed, each as [`Outbox::send_transient`] would; returns
    /// each with how it went (one throttled again is dropped: it has waited once).
    pub async fn send_due_transients(&self) -> Vec<(Transient, TransientSent)> {
        let now = (self.clock)();
        let due: Vec<Transient> = {
            let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
            let (due, later) = std::mem::take(&mut *waiting)
                .into_iter()
                .partition(|waiting| waiting.not_before <= now);
            *waiting = later;
            due.into_iter().map(|waiting: Waiting| waiting.transient).collect()
        };
        let mut sent = Vec::with_capacity(due.len());
        for transient in due {
            let outcome = self.try_transient(transient.clone(), true).await;
            sent.push((transient, outcome));
        }
        sent
    }

    /// Drops every transient waiting in `conversation`, each returned with its drop: what it announced has gone first
    /// (the engine calls this before a message post to the conversation, so no typing follows the message).
    pub fn drop_waiting(&self, conversation: &str) -> Vec<(Transient, TransientSent)> {
        self.drop_where(
            |transient| transient.conversation() == Some(conversation),
            "a message to its conversation went first",
        )
    }

    /// Drops every waiting write of the account's own availability, each returned with its drop: the endpoint it
    /// names is no longer the account's live one (the engine calls this when the push channel stops, so no
    /// availability goes out from the tick after it).
    pub fn drop_own_availability(&self) -> Vec<(Transient, TransientSent)> {
        self.drop_where(
            |transient| transient.endpoint().is_some(),
            "the push channel stopped: the endpoint it names is no longer the account's live one",
        )
    }

    /// Drops every waiting transient `drop` picks, each returned with its drop for `reason`.
    fn drop_where(&self, drop: impl Fn(&Transient) -> bool, reason: &str) -> Vec<(Transient, TransientSent)> {
        let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
        let (dropped, kept): (Vec<Waiting>, Vec<Waiting>) = std::mem::take(&mut *waiting)
            .into_iter()
            .partition(|waiting| drop(&waiting.transient));
        *waiting = kept;
        dropped
            .into_iter()
            .map(|waiting| {
                let reason = reason.to_owned();
                (waiting.transient, TransientSent::Dropped { reason })
            })
            .collect()
    }

    /// Puts `transient` in the place of a waiting one of the same, if one waits; its time, when it did.
    fn replace_waiting(&self, transient: &Transient) -> Option<i64> {
        let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
        let found = waiting
            .iter_mut()
            .find(|waiting| transient.replaces(&waiting.transient))?;
        found.transient = transient.clone();
        Some(found.not_before)
    }

    /// Attempts until it is sent, denied, dropped or throttled (one more attempt after a first 401); `waited`: it has
    /// waited out a `Retry-After` already.
    async fn try_transient(&self, transient: Transient, waited: bool) -> TransientSent {
        let mut refreshed = false;
        loop {
            let Some(tried) = self.attempt_transient(&transient).await else {
                return TransientSent::Dropped {
                    reason: NOT_THIS_ACCOUNTS.to_owned(),
                };
            };
            log::info!(
                "transient {transient:?}{}: {}",
                if waited { " after its wait" } else { "" },
                tried.summary()
            );
            match tried {
                Tried::Answered(answer) if answer.status() == 401 && !refreshed => refreshed = true,
                Tried::Answered(answer) => return self.answered(transient, &answer, waited),
                Tried::Denied(denied) => {
                    return TransientSent::Denied {
                        reason: denied.to_string(),
                    };
                }
                Tried::NotSent(reason) => return TransientSent::Dropped { reason },
                Tried::Throttled { wait, .. } => return self.wait_transient(transient, wait, waited),
                Tried::Uncertain(error) => {
                    return TransientSent::Dropped {
                        reason: format!("{error}; whether it arrived is unknown, so it isn't sent again"),
                    };
                }
            }
        }
    }

    /// One attempt at `transient`; `None` when it names an endpoint registered for another account than the outbox's
    /// (or for none): nothing of it was built, asked of the fence or sent.
    async fn attempt_transient(&self, transient: &Transient) -> Option<Tried> {
        let me = self.me.mri().clone();
        Some(match transient {
            Transient::Typing { conversation } => self.deliver(Typing::new(conversation.clone())).await,
            Transient::EndpointAvailable { endpoint } => {
                self.deliver(SetEndpointAvailability::available(me, endpoint)?).await
            }
            Transient::ManualStatus { endpoint, status } => {
                let write = match status {
                    Some(status) => ForceAvailability::set(me, endpoint, *status),
                    None => ForceAvailability::reset(me, endpoint),
                };
                self.deliver(write?).await
            }
        })
    }

    fn answered(&self, transient: Transient, answer: &Response, waited: bool) -> TransientSent {
        let status = answer.status();
        match status {
            200..=299 => TransientSent::Sent,
            429 | 503 => {
                let wait = answer.retry_after().unwrap_or(DEFAULT_RETRY_WAIT);
                self.wait_transient(transient, wait, waited)
            }
            500..=599 => TransientSent::Dropped {
                reason: format!("HTTP {status}; whether it arrived is unknown, so it isn't sent again"),
            },
            _ => TransientSent::Dropped {
                reason: format!("HTTP {status}"),
            },
        }
    }

    /// Keeps a throttled transient until `wait` has passed, in the place of any of the same that waits (a newer one
    /// asked for meanwhile can't be: the engine sends one thing at a time); drops it instead when it has `waited`
    /// once already or `wait` is longer than [`MAX_TRANSIENT_WAIT`].
    fn wait_transient(&self, transient: Transient, wait: Duration, waited: bool) -> TransientSent {
        if waited {
            return TransientSent::Dropped {
                reason: "throttled again after waiting once".to_owned(),
            };
        }
        if wait > MAX_TRANSIENT_WAIT {
            return TransientSent::Dropped {
                reason: format!(
                    "throttled for {} ms, longer than the {} ms a transient is worth waiting",
                    wait.as_millis(),
                    MAX_TRANSIENT_WAIT.as_millis()
                ),
            };
        }
        let not_before = (self.clock)().saturating_add(i64::try_from(wait.as_millis()).unwrap_or(i64::MAX));
        let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
        waiting.retain(|waiting| !transient.replaces(&waiting.transient));
        waiting.push(Waiting { transient, not_before });
        TransientSent::Waiting { not_before }
    }
}
