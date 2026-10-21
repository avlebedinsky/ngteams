//! The outbox's transient kind: a write that matters only at the moment it is asked for (typing; later the account's
//! own availability). It goes out through the outbox like every other write, each send under a fresh
//! [`Fence::check`](teams_fence::Fence::check) with its `fence.log` line and the transport's rules, but it is never a
//! row: nothing of it is stored, so nothing is checked for, tried again after an unknown outcome, adopted by a later
//! run or offered to send again. A durable row for `Control/Typing` would never settle (a `Control/*` message never
//! settles a row) and could go out again at a moment the user didn't choose.
//!
//! - A 2xx is sent; a 401 gets one more attempt at once (the transport had the session refresh the refused token),
//!   as a row does; a 429 or 503 (or a throttled token service) waits its `Retry-After` in memory and goes out from
//!   the engine's tick ([`Outbox::send_due_transients`]); a denial, any other answer and any outcome that isn't known
//!   (a network error, a timeout, another 5xx) drop it.
//! - **Latest wins while one waits**: a transient asked for while one of the same ([`Transient::replaces`]) waits takes
//!   its place, and waits as long; only the newest goes out.

use std::sync::PoisonError;
use std::time::Duration;

use teams_proto::{DEFAULT_RETRY_WAIT, HttpError, Response, Transport, Typing};

use super::{Outbox, Tried};

/// A write the outbox sends without a row (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transient {
    /// The account is typing in a chat (`Control/Typing`).
    Typing { conversation: String },
}

impl Transient {
    /// Whether this one takes `other`'s place while `other` waits: the same kind on the same target.
    #[must_use]
    pub fn replaces(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Typing { conversation }, Self::Typing { conversation: other }) => conversation == other,
        }
    }

    /// The conversation it writes to.
    #[must_use]
    pub fn conversation(&self) -> Option<&str> {
        match self {
            Self::Typing { conversation } => Some(conversation),
        }
    }
}

/// How a transient's send went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransientSent {
    /// The service took it.
    Sent,
    /// Throttled, or another of the same waits: it goes out from the tick after `not_before` (milliseconds since the
    /// Unix epoch), unless a newer one takes its place first.
    Waiting { not_before: i64 },
    /// The fence denied it: nothing went out.
    Denied { reason: String },
    /// It didn't go out, or whether it arrived is unknown: dropped, never sent again.
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
        self.try_transient(transient).await
    }

    /// Sends every waiting transient whose `Retry-After` has passed, each as [`Outbox::send_transient`] would; returns
    /// each with how it went (one throttled again waits again).
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
            let outcome = self.try_transient(transient.clone()).await;
            sent.push((transient, outcome));
        }
        sent
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

    /// Attempts until it is sent, denied, dropped or throttled (one more attempt after a first 401).
    async fn try_transient(&self, transient: Transient) -> TransientSent {
        let mut refreshed = false;
        loop {
            let tried = match &transient {
                Transient::Typing { conversation } => self.deliver(Typing::new(conversation.clone())).await,
            };
            match tried {
                Tried::Answered(answer) if answer.status() == 401 && !refreshed => refreshed = true,
                Tried::Answered(answer) => return self.answered(transient, &answer),
                Tried::Denied(denied) => {
                    return TransientSent::Denied {
                        reason: denied.to_string(),
                    };
                }
                Tried::NotSent(reason) => return TransientSent::Dropped { reason },
                Tried::Throttled { wait, .. } => return self.wait_transient(transient, wait),
                Tried::Uncertain(error) => {
                    return TransientSent::Dropped {
                        reason: format!("{error}; whether it arrived is unknown, so it isn't sent again"),
                    };
                }
            }
        }
    }

    fn answered(&self, transient: Transient, answer: &Response) -> TransientSent {
        let status = answer.status();
        match status {
            200..=299 => TransientSent::Sent,
            429 | 503 => self.wait_transient(transient, answer.retry_after().unwrap_or(DEFAULT_RETRY_WAIT)),
            500..=599 => TransientSent::Dropped {
                reason: format!("HTTP {status}; whether it arrived is unknown, so it isn't sent again"),
            },
            _ => TransientSent::Dropped {
                reason: format!("HTTP {status}"),
            },
        }
    }

    /// Keeps a throttled transient until `wait` has passed, in the place of any of the same that waits (a newer one
    /// asked for meanwhile can't be: the engine sends one thing at a time).
    fn wait_transient(&self, transient: Transient, wait: Duration) -> TransientSent {
        let not_before = (self.clock)().saturating_add(i64::try_from(wait.as_millis()).unwrap_or(i64::MAX));
        let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
        waiting.retain(|waiting| !transient.replaces(&waiting.transient));
        waiting.push(Waiting { transient, not_before });
        TransientSent::Waiting { not_before }
    }
}
