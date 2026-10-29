//! The personal build's fence decides nothing (`docs/personal-build.md`; CLAUDE.md, "The personal build (`ngteams`) is
//! the user's Teams"): it is the user's own account, used in the user's own window, so whatever the Teams client may
//! do, it may do. [`Fence::check`] logs every write to `fence.log` and permits it: any kind, on any target (a chat of
//! any kind, a channel, a new chat, a meeting, a call), with no member read. What it keeps:
//!
//! - **The log**: every decision goes to `fence.log` before the permit is made, and an allow that can't be logged is
//!   denied ([`DenyReason::LogUnavailable`]), so nothing goes unlogged.
//! - **The permit per write**: a [`WritePermit`] for the exact target, which the outbox's `Authorized<_>` needs, as in
//!   the lab.
//! - **The call registry's bookkeeping**: an operation on a call moves what the registry resolves it to (a `leave` its
//!   call, an `attach`, an answer or a decline its offered invitation), as the lab's call rule does after a permit, so
//!   the call machine and the call floor see the same calls.
//! - **One call at a time** across the app's accounts (the call floor, checked by [`Fence::check`] before this), as a
//!   Teams client holds one active call.

use super::{Denied, Fence, WritePermit};
use crate::calls::Resolved;
use crate::log::Verdict;
use crate::target::{CallOperation, Scope, Target};

impl Fence {
    /// The personal fence's decision on `target`: permitted, logged (see the module docs).
    pub(crate) fn audit(&self, target: Target) -> Result<WritePermit, Denied> {
        let resolved = self.bookkeeping(&target);
        let permit = self.decided(target, Verdict::Allow)?;
        if let Some(resolved) = resolved.filter(|resolved| self.calls.still_open(resolved)) {
            self.calls.permitted(&resolved);
        }
        Ok(permit)
    }

    /// What an operation on a call acts on in the call registry, if anything: an answer or a decline its offered
    /// invitation (by the links the invitation's `attach` bound, else by the call leg its link names), anything else
    /// the call or invitation holding its link.
    fn bookkeeping(&self, target: &Target) -> Option<Resolved> {
        let Scope::CallOperation { operation, .. } = target.scope() else {
            return None;
        };
        let resolved = self.calls.resolve(target);
        match operation {
            CallOperation::Accept | CallOperation::Reject => resolved.or_else(|_| self.calls.resolve_answer(target)),
            _ => resolved,
        }
        .ok()
    }
}
