//! The write fence: a write is allowed only when every participant of its target, fetched fresh, is one of the 3 lab
//! accounts or an approved service participant.
//!
//! [`Fence::check`] takes a [`Target`] (a [`WriteKind`] and a [`Scope`], plus any participants the write adds) and a
//! [`MembershipSource`], fetches the scope's members on every call, and returns a [`WritePermit`] or [`Denied`].
//! Anything unknown is denied: a failed, empty, paginated or truncated membership, an org-wide team or channel, an
//! outside participant, a [`Scope::New`] target for a kind that doesn't create one. Every decision goes to `fence.log`
//! ([`FenceLog`]), and so do the browser requests the browser fence decides without a membership check
//! ([`RequestRecord`]).

mod decision;
mod log;
mod target;

pub use decision::{
    Denied, DenyReason, Fence, FetchFailure, Membership, MembershipFuture, MembershipSource, PartialReason, Roster,
    WritePermit,
};
pub use log::{Channel, FenceLog, RequestOutcome, RequestRecord};
pub use target::{Mri, Scope, Target, WriteKind};
