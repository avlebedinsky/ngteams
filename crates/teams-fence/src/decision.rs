//! The decision: [`Fence::check`] fetches the target's members fresh and issues a [`WritePermit`] only when every
//! participant is a lab account or an approved service.

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use serde::Serialize;

use crate::log::{FenceLog, Verdict};
use crate::target::{Mri, Scope, Target};

/// Who may take part in a write: the lab accounts and the approved service participants, by exact MRI. A lab account
/// or service whose MRI isn't known yet is simply left out, so it matches nobody.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roster {
    lab: BTreeSet<Mri>,
    services: BTreeSet<Mri>,
}

impl Roster {
    #[must_use]
    pub fn new(lab: impl IntoIterator<Item = Mri>, services: impl IntoIterator<Item = Mri>) -> Self {
        Self {
            lab: lab.into_iter().collect(),
            services: services.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn admits(&self, mri: &Mri) -> bool {
        self.lab.contains(mri) || self.services.contains(mri)
    }
}

/// Why a membership fetch didn't produce a list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchFailure {
    Network,
    Timeout,
    /// The server answered with a non-success HTTP status.
    Status(u16),
    /// The response couldn't be read as a member list.
    Malformed,
}

/// Why a membership is not known in full.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PartialReason {
    FetchFailed(FetchFailure),
    /// The list has more pages.
    Paginated,
    /// The list was cut short (a member count above the listed members, a size cap).
    Truncated,
}

impl fmt::Display for PartialReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FetchFailed(FetchFailure::Status(status)) => write!(f, "the fetch failed with HTTP {status}"),
            Self::FetchFailed(failure) => write!(f, "the fetch failed ({failure:?})"),
            Self::Paginated => f.write_str("the list is paginated"),
            Self::Truncated => f.write_str("the list is truncated"),
        }
    }
}

/// A fresh answer to "who takes part in this scope?".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Membership {
    /// The full list of current participants.
    Complete(Vec<Mri>),
    /// Anything less than the full list.
    Partial(PartialReason),
    /// An org-wide team or one of its channels: everyone in the tenant takes part.
    OrgWide,
}

/// The future a [`MembershipSource`] returns.
pub type MembershipFuture<'a> = Pin<Box<dyn Future<Output = Membership> + Send + 'a>>;

/// Fetches a scope's participants. Implementations fetch on every call and never answer from a cache; anything they
/// can't confirm in full is [`Membership::Partial`].
pub trait MembershipSource: Sync {
    fn members<'a>(&'a self, scope: &'a Scope) -> MembershipFuture<'a>;
}

/// The proof that one write passed the fence. Only [`Fence::check`] makes one: it has no public constructor, is not
/// `Clone`, and is bound to the target it was issued for.
#[derive(Debug)]
pub struct WritePermit {
    target: Target,
}

impl WritePermit {
    #[must_use]
    pub fn target(&self) -> &Target {
        &self.target
    }
}

/// Why a write was denied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// The membership isn't known in full.
    Partial(PartialReason),
    /// The fetched membership, or a new target's proposed one, is empty.
    NoMembers,
    /// An org-wide team or channel.
    OrgWide,
    /// Participants that are neither lab accounts nor approved services.
    Outsiders { count: usize },
    /// The write would be allowed, but its decision couldn't be written to `fence.log`.
    LogUnavailable,
}

impl fmt::Display for DenyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Partial(reason) => write!(f, "membership not known in full: {reason}"),
            Self::NoMembers => f.write_str("no members"),
            Self::OrgWide => f.write_str("an org-wide team or channel"),
            Self::Outsiders { count } => write!(f, "{count} participant(s) outside the lab"),
            Self::LogUnavailable => f.write_str("fence.log can't be written"),
        }
    }
}

/// A denied write: the target and the reason. Carries no member identities and no credentials.
#[derive(Debug, thiserror::Error)]
#[error("fence denied {target}: {reason}")]
pub struct Denied {
    target: Target,
    reason: DenyReason,
}

impl Denied {
    #[must_use]
    pub fn target(&self) -> &Target {
        &self.target
    }

    #[must_use]
    pub fn reason(&self) -> &DenyReason {
        &self.reason
    }
}

/// The write fence: the roster it admits and the log every decision goes to.
#[derive(Debug)]
pub struct Fence {
    roster: Roster,
    log: FenceLog,
}

impl Fence {
    #[must_use]
    pub fn new(roster: Roster, log: FenceLog) -> Self {
        Self { roster, log }
    }

    /// Fetches the target scope's members from `source` (never from a cache) and allows the write only when every
    /// participant, current or added, is in the roster. Every decision is appended to `fence.log`; an allow that
    /// can't be logged is denied.
    ///
    /// # Errors
    /// [`Denied`] when the membership is partial, empty or org-wide, when any participant is outside the roster, or
    /// when an allow can't be logged.
    pub async fn check(&self, target: Target, source: &dyn MembershipSource) -> Result<WritePermit, Denied> {
        let membership = match target.scope() {
            Scope::New => Membership::Complete(Vec::new()),
            scope => source.members(scope).await,
        };
        let verdict = self.judge(&target, membership);
        let logged = self.log.record(&target, verdict);
        match (verdict, logged) {
            (Verdict::Allow, Ok(())) => Ok(WritePermit { target }),
            (Verdict::Allow, Err(_)) => Err(Denied {
                target,
                reason: DenyReason::LogUnavailable,
            }),
            (Verdict::Deny(reason), _) => Err(Denied { target, reason }),
        }
    }

    fn judge(&self, target: &Target, membership: Membership) -> Verdict {
        let current = match membership {
            Membership::Complete(members) => members,
            Membership::Partial(reason) => return Verdict::Deny(DenyReason::Partial(reason)),
            Membership::OrgWide => return Verdict::Deny(DenyReason::OrgWide),
        };
        let fetched = !matches!(target.scope(), Scope::New);
        if fetched && current.is_empty() {
            return Verdict::Deny(DenyReason::NoMembers);
        }
        let participants: BTreeSet<&Mri> = current.iter().chain(target.added()).collect();
        if participants.is_empty() {
            return Verdict::Deny(DenyReason::NoMembers);
        }
        let count = participants.iter().filter(|mri| !self.roster.admits(mri)).count();
        if count > 0 {
            return Verdict::Deny(DenyReason::Outsiders { count });
        }
        Verdict::Allow
    }
}
