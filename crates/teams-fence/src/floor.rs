//! The call floor: one call or ringing invitation at a time across the accounts of one app.
//!
//! An app that runs several accounts (the personal build with the user's accounts, the lab app with several lab
//! accounts) opens one fence per account and lets them share one [`CallFloor`] ([`Fence::sharing_calls`]). Each fence's
//! call registry still tracks its own account's calls; the floor only lets each one see whether another account holds
//! it (a registered call that is live, an offered invitation answered and not left, or one that still rings, attached
//! or not):
//!
//! - **An invitation** to an account isn't offered while another account on the floor holds it
//!   ([`Fence::observe_delivery`], decided under the floor's lock, so two invitations arriving at once are never both
//!   offered): it is ignored, nothing is sent for it, and it rings on the user's other clients, as one to a busy
//!   account is under the personal build's rule. Its own account's invitations are judged by its own rule, as without
//!   a floor (a lab fence offers more than one, a personal one one at a time).
//! - **A start, a join or an answer** (the writes that enter a call) is denied while another account on the floor
//!   holds it ([`DenyReason::AnotherAccountInCall`](crate::DenyReason::AnotherAccountInCall)), logged like every
//!   decision. That check reads the floor without its lock, and a start still waiting for its answer holds nothing:
//!   two starts asked of two accounts at the same moment are kept apart by the app's views
//!   (`teams_ui::Accounts::busy_elsewhere`), which offer no call while another account has one asked for.
//!
//! An app of one account shares a floor with nobody: only its own registry counts, as for a fence of its own. The floor holds the
//! fences' registries weakly, so a fence that is dropped (an account removed, an engine restarted) leaves it.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use crate::calls::CallRegistry;
#[cfg(doc)]
use crate::decision::Fence;

/// The calls of several accounts, one at a time (see the module docs). Cloned, it is the same floor.
#[derive(Clone, Default)]
pub struct CallFloor {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Held around an offer's decision, so two accounts can't both be offered an invitation at once. Only a registry's
    /// own lock is taken under it, never the other way round.
    taking: Mutex<()>,
    /// Each fence's id and its registry.
    fences: Mutex<Vec<(u64, Weak<CallRegistry>)>>,
}

impl fmt::Debug for CallFloor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallFloor")
            .field("fences", &lock(&self.inner.fences).len())
            .finish()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl CallFloor {
    /// A floor no fence shares yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds fence `id`'s registry; fences already dropped leave the floor.
    pub(crate) fn join(&self, id: u64, registry: &Arc<CallRegistry>) {
        let mut fences = lock(&self.inner.fences);
        fences.retain(|(_, registry)| registry.strong_count() > 0);
        fences.push((id, Arc::downgrade(registry)));
    }

    /// Holds the floor while an offer is decided (see [`Inner::taking`]).
    pub(crate) fn hold(&self) -> MutexGuard<'_, ()> {
        lock(&self.inner.taking)
    }

    /// Whether an account on the floor other than fence `id`'s holds it (`CallRegistry::holds_floor`).
    pub(crate) fn others_busy(&self, id: u64) -> bool {
        let others: Vec<Arc<CallRegistry>> = lock(&self.inner.fences)
            .iter()
            .filter(|(fence, _)| *fence != id)
            .filter_map(|(_, registry)| registry.upgrade())
            .collect();
        others.iter().any(|registry| registry.holds_floor())
    }
}
