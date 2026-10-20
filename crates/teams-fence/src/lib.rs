//! The write fence: a write is allowed only when every participant of its target, fetched fresh, is one of the 3 lab
//! accounts or an approved service participant.
//!
//! [`Fence::check`] takes a [`Target`] (a [`WriteKind`] and a [`Scope`], plus any participants the write adds) and a
//! [`MembershipSource`], fetches the scope's members on every call, and returns a [`WritePermit`] or [`Denied`].
//! Anything unknown is denied: a failed, empty, paginated or truncated membership, an org-wide team or channel, an
//! outside participant, a [`Scope::New`] target for a kind that doesn't create one. Every decision goes to `fence.log`
//! ([`FenceLog`]), and so do the browser requests the browser fence decides without a membership check
//! ([`RequestRecord`]) and the targets it couldn't arm ([`ArmingRecord`]).
//!
//! Nothing outside this crate can bend a decision: a lab binary builds its fence only with [`Fence::open_lab`] (the
//! roster from `lab.toml`, the log at the lab's `fence.log`), and [`MembershipSource`] is sealed, so its sources are
//! the ones this crate defines: [`FailClosed`], which denies every write whose scope has to be fetched, and
//! [`LiveMembershipSource`], which reads a scope's members from Teams' chat service itself, the caller supplying only
//! the lab account's credential. The `test-support` feature lifts both for tests (`Fence::new` over any `Roster::new`, `FenceLog::from_writer`, any type as a
//! membership source); only dev-dependencies enable it, and a lab binary refuses to run when a build turned it on
//! ([`test_support_enabled`]).

mod decision;
pub mod lab;
mod log;
mod membership;
mod target;

pub use decision::{
    Denied, DenyReason, FailClosed, Fence, FetchFailure, Membership, MembershipFuture, MembershipSource, OpenError,
    PartialReason, Roster, WritePermit,
};
pub use log::{ArmingCause, ArmingRecord, Channel, CloseOutcome, Containment, FenceLog, RequestOutcome, RequestRecord};
pub use membership::{
    AFD_HOSTS, ChatCredential, FETCH_TIMEOUT, LiveMembershipSource, LiveSourceError, MAX_BODY, REGIONAL_SUFFIX,
    chat_service_path, is_group_chat_id, one_to_one_pair, parse_thread,
};
pub use target::{Mri, Scope, Target, WriteKind};

/// Whether this build has the `test-support` feature, which lets any code build a fence over any roster and log and
/// implement [`MembershipSource`]. Only dev-dependencies enable it, but cargo unifies features across one build, so a
/// lab binary built together with the tests (`cargo test`, `--all-targets`, `--all-features`) gets it too; such a
/// binary checks this and refuses to run.
#[must_use]
pub const fn test_support_enabled() -> bool {
    cfg!(feature = "test-support")
}
