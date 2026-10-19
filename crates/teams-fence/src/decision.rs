//! The decision: [`Fence::check`] fetches the target's members fresh and issues a [`WritePermit`] only when every
//! participant is a lab account or an approved service.

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;

use serde::Serialize;

use crate::lab::{ConfigError, LabConfig, LabLocations};
use crate::log::{FenceLog, Verdict};
use crate::target::{Mri, Scope, Target};

/// Who may take part in a write: the lab accounts and the approved service participants, by exact MRI. A lab account
/// or service whose MRI isn't known yet is simply left out, so it matches nobody.
///
/// A lab binary's roster comes only from `lab.toml`, through [`Fence::open_lab`]. [`Roster::new`], which takes any
/// MRIs, exists only with the `test-support` feature.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roster {
    lab: BTreeSet<Mri>,
    services: BTreeSet<Mri>,
}

impl Roster {
    /// A roster of any MRIs. Tests only (`test-support`).
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn new(lab: impl IntoIterator<Item = Mri>, services: impl IntoIterator<Item = Mri>) -> Self {
        Self::of(lab, services)
    }

    fn of(lab: impl IntoIterator<Item = Mri>, services: impl IntoIterator<Item = Mri>) -> Self {
        Self {
            lab: lab.into_iter().collect(),
            services: services.into_iter().collect(),
        }
    }

    /// The MRIs `lab.toml` gives its accounts and approved service participants; one without an MRI is left out.
    fn from_lab(config: &LabConfig) -> Self {
        Self::of(
            config
                .accounts()
                .iter()
                .filter_map(|account| account.mri.as_deref())
                .map(Mri::new),
            config
                .service_participants()
                .iter()
                .filter_map(|service| service.mri.as_deref())
                .map(Mri::new),
        )
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
    /// Nothing was fetched: the fence runs fail-closed, without a live source ([`FailClosed`]).
    NoSource,
    /// The live source has no credential to read with yet ([`crate::LiveMembershipSource`]).
    NoCredential,
    /// The source can't read this kind of scope in full (yet).
    Unsupported,
}

impl fmt::Display for PartialReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FetchFailed(FetchFailure::Status(status)) => write!(f, "the fetch failed with HTTP {status}"),
            Self::FetchFailed(failure) => write!(f, "the fetch failed ({failure:?})"),
            Self::Paginated => f.write_str("the list is paginated"),
            Self::Truncated => f.write_str("the list is truncated"),
            Self::NoSource => f.write_str("no membership source (the fence runs fail-closed)"),
            Self::NoCredential => f.write_str("the membership source has no credential yet"),
            Self::Unsupported => f.write_str("the membership source can't read this scope in full"),
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
///
/// The trait is sealed: what the fence is told about a scope's members decides the permit, so every source a lab
/// binary can use is defined in this crate ([`FailClosed`], [`crate::LiveMembershipSource`]), and no other crate can hand the fence a member
/// list of its own making. With the `test-support` feature (tests only, never a lab binary) any type may implement it.
pub trait MembershipSource: Sync + sealed::Sealed {
    fn members<'a>(&'a self, scope: &'a Scope) -> MembershipFuture<'a>;
}

/// The source of a fence that runs fail-closed: it fetches nothing and answers every scope with
/// [`PartialReason::NoSource`], so [`Fence::check`] denies every write whose scope has to be fetched. A lab binary
/// arms the browser fence with it when its run writes nothing (`load`, `status`, `login`). A [`Scope::New`] target isn't fetched, so
/// it is still judged by its added participants alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FailClosed;

impl MembershipSource for FailClosed {
    fn members<'a>(&'a self, _scope: &'a Scope) -> MembershipFuture<'a> {
        Box::pin(std::future::ready(Membership::Partial(PartialReason::NoSource)))
    }
}

mod sealed {
    /// The seal on [`MembershipSource`](super::MembershipSource): a public trait no other crate can name.
    pub trait Sealed {}

    /// The sources this crate defines. Under `test-support` the blanket impl below covers them, so these impls exist
    /// only without it and never overlap it; `crates/fence-seal` checks that a build without it can use them.
    #[cfg(not(feature = "test-support"))]
    impl Sealed for super::FailClosed {}
    #[cfg(not(feature = "test-support"))]
    impl Sealed for crate::membership::LiveMembershipSource {}

    /// Tests only: any type may be a membership source.
    #[cfg(feature = "test-support")]
    impl<T: ?Sized> Sealed for T {}
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
    /// A [`Scope::New`] target for a kind that doesn't create one: the write lands on something that already exists,
    /// whose members have to be fetched.
    NotCreatable,
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
            Self::NotCreatable => f.write_str("this kind of write doesn't create a new target"),
            Self::LogUnavailable => f.write_str("fence.log can't be written"),
        }
    }
}

/// A denied write: the target and the reason. Carries no credentials. Its `Display` and `Debug` show the added
/// participants as a count only, but [`Denied::target`] gives the whole [`Target`], whose [`Target::added`] holds
/// their MRIs, and the scope ids are shown as they are (a 1:1 chat id holds both members' object ids).
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

/// Why the lab's fence couldn't be built.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// `lab.toml` can't be found, read or validated.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// `fence.log` can't be opened.
    #[error("can't open {}: {source}", path.display())]
    Log { path: PathBuf, source: io::Error },
}

/// The write fence: the roster it admits and the log every decision goes to.
///
/// A lab binary builds it only with [`Fence::open_lab`]: the roster from `lab.toml`, the log at the lab's
/// `fence.log`. [`Fence::new`] (any roster, any log) and [`Fence::open`] (any config) exist only with the
/// `test-support` feature.
#[derive(Debug)]
pub struct Fence {
    roster: Roster,
    log: FenceLog,
}

impl Fence {
    /// The lab's fence. Its roster is the MRIs `lab.toml` gives the lab accounts and the approved service participants,
    /// and its decisions go to `<data_dir>/fence.log`; both files are found from the environment, as
    /// [`LabLocations::from_env`] finds them.
    ///
    /// # Errors
    /// When `lab.toml` can't be found, read or validated, or `fence.log` can't be opened.
    pub fn open_lab() -> Result<Self, OpenError> {
        Self::from_config(&LabConfig::load(&LabLocations::from_env()?)?)
    }

    /// The fence [`Fence::open_lab`] builds, from a given config. Tests only (`test-support`).
    ///
    /// # Errors
    /// When `fence.log` can't be opened.
    #[cfg(feature = "test-support")]
    pub fn open(config: &LabConfig) -> Result<Self, OpenError> {
        Self::from_config(config)
    }

    fn from_config(config: &LabConfig) -> Result<Self, OpenError> {
        let path = config.paths().fence_log();
        let log = FenceLog::open(&path).map_err(|source| OpenError::Log { path, source })?;
        Ok(Self {
            roster: Roster::from_lab(config),
            log,
        })
    }

    /// A fence over any roster and any log. Tests only (`test-support`).
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn new(roster: Roster, log: FenceLog) -> Self {
        Self { roster, log }
    }

    /// The log every decision goes to; the browser fence records its own request decisions there too.
    #[must_use]
    pub fn log(&self) -> &FenceLog {
        &self.log
    }

    /// Fetches the target scope's members from `source` (never from a cache) and allows the write only when every
    /// participant, current or added, is in the roster. A [`Scope::New`] target isn't fetched: it is judged by its
    /// added participants alone, and only for a kind that [creates a target](crate::WriteKind::creates_target).
    /// Every decision is appended to `fence.log`; an allow that can't be logged is denied.
    ///
    /// # Errors
    /// [`Denied`] when the membership is partial, empty or org-wide, when any participant is outside the roster, when
    /// a kind that doesn't create a target names [`Scope::New`], or when an allow can't be logged.
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
        let fetched = !matches!(target.scope(), Scope::New);
        if !fetched && !target.kind().creates_target() {
            return Verdict::Deny(DenyReason::NotCreatable);
        }
        let current = match membership {
            Membership::Complete(members) => members,
            Membership::Partial(reason) => return Verdict::Deny(DenyReason::Partial(reason)),
            Membership::OrgWide => return Verdict::Deny(DenyReason::OrgWide),
        };
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
