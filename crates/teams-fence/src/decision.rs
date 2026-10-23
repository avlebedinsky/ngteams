//! The decision: [`Fence::check`] fetches the target's members fresh and issues a [`WritePermit`] only when every
//! participant is a lab account or an approved service. A write that reaches nobody ([`WriteKind::is_contacting`]
//! false) is judged without a member read: typing and read receipts on a chat (1:1, group or meeting), and the
//! account's own availability ([`WriteKind::OwnAvailability`] on [`Scope::Account`]), which the fence permits only
//! for its own account ([`Fence::own_account`]), so that a lab account never sets another's presence.

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;

use serde::Serialize;

use crate::lab::{Alias, ConfigError, LabConfig, LabLocations};
use crate::log::{FenceLog, Verdict};
use crate::membership::is_chat_id;
use crate::target::{Mri, Scope, Target, WriteKind};

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

    /// Whether `mri` is one of the lab accounts, by exact MRI (an approved service isn't).
    #[must_use]
    pub fn is_lab_account(&self, mri: &Mri) -> bool {
        self.lab.contains(mri)
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
    /// The account's own availability ([`WriteKind::OwnAvailability`]) for anyone but the fence's own account: on
    /// another account, on any scope but [`Scope::Account`], with participants added, or from a fence that has no own
    /// account ([`Fence::open_lab`]).
    NotOwnAccount,
    /// A write other than the account's own availability on [`Scope::Account`]: nothing else lands on the account
    /// itself.
    AccountScope,
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
            Self::NotOwnAccount => f.write_str("not the fence's own account's availability"),
            Self::AccountScope => f.write_str("only the account's own availability lands on the account itself"),
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

/// The write fence: the roster it admits, the log every decision goes to, and the account it is a client's of, if any.
///
/// A lab binary builds it only with [`Fence::open_lab`] or [`Fence::open_lab_account`]: the roster from `lab.toml`,
/// the log at the lab's `fence.log`, and for the latter the account's own MRI from the same file. [`Fence::new`] (any
/// roster, any log), [`Fence::open`] and [`Fence::open_account`] (any config) and [`Fence::with_own_account`] (any
/// account) exist only with the `test-support` feature.
#[derive(Debug)]
pub struct Fence {
    roster: Roster,
    log: FenceLog,
    /// The account this fence is a client's of ([`Fence::open_lab_account`]): the one account whose own availability
    /// it permits. `None` for a fence of no account ([`Fence::open_lab`], the browser fence's) or of an account whose
    /// MRI `lab.toml` doesn't know yet: it permits no own availability.
    own: Option<Mri>,
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

    /// The fence of one lab account's native client: [`Fence::open_lab`]'s, whose own account is `alias`'s, by the MRI
    /// `lab.toml` gives it. It permits that account's own availability ([`WriteKind::OwnAvailability`]) and no other
    /// account's; an account whose MRI isn't known yet gets none.
    ///
    /// # Errors
    /// As [`Fence::open_lab`].
    pub fn open_lab_account(alias: Alias) -> Result<Self, OpenError> {
        Self::from_account(&LabConfig::load(&LabLocations::from_env()?)?, alias)
    }

    /// The fence [`Fence::open_lab`] builds, from a given config. Tests only (`test-support`).
    ///
    /// # Errors
    /// When `fence.log` can't be opened.
    #[cfg(feature = "test-support")]
    pub fn open(config: &LabConfig) -> Result<Self, OpenError> {
        Self::from_config(config)
    }

    /// The fence [`Fence::open_lab_account`] builds, from a given config. Tests only (`test-support`).
    ///
    /// # Errors
    /// When `fence.log` can't be opened.
    #[cfg(feature = "test-support")]
    pub fn open_account(config: &LabConfig, alias: Alias) -> Result<Self, OpenError> {
        Self::from_account(config, alias)
    }

    fn from_config(config: &LabConfig) -> Result<Self, OpenError> {
        let path = config.paths().fence_log();
        let log = FenceLog::open(&path).map_err(|source| OpenError::Log { path, source })?;
        Ok(Self {
            roster: Roster::from_lab(config),
            log,
            own: None,
        })
    }

    fn from_account(config: &LabConfig, alias: Alias) -> Result<Self, OpenError> {
        let mut fence = Self::from_config(config)?;
        fence.own = config.account(alias).mri.as_deref().map(Mri::new);
        Ok(fence)
    }

    /// A fence over any roster and any log. Tests only (`test-support`).
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn new(roster: Roster, log: FenceLog) -> Self {
        Self { roster, log, own: None }
    }

    /// The fence with `mri` as its own account (see [`Fence::own_account`]). Tests only (`test-support`).
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn with_own_account(mut self, mri: Mri) -> Self {
        self.own = Some(mri);
        self
    }

    /// The account whose own availability this fence permits: the account a [`Fence::open_lab_account`] was opened
    /// for, when `lab.toml` knows its MRI; `None` otherwise.
    #[must_use]
    pub fn own_account(&self) -> Option<&Mri> {
        self.own.as_ref()
    }

    /// The log every decision goes to; the browser fence records its own request decisions there too.
    #[must_use]
    pub fn log(&self) -> &FenceLog {
        &self.log
    }

    /// Whether `names` names someone and only the lab accounts, each by its exact MRI: no approved service, nobody
    /// else. The browser fence lets a reviewed read by `POST` through only then (a profile lookup of the lab's own
    /// accounts), so that nothing such a read names is outside the lab. It mints no permit and logs nothing.
    #[must_use]
    pub fn lab_accounts_only(&self, names: &[Mri]) -> bool {
        !names.is_empty() && names.iter().all(|mri| self.roster.is_lab_account(mri))
    }

    /// Fetches the target scope's members from `source` (never from a cache) and allows the write only when every
    /// participant, current or added, is in the roster. Every write is fetched so but three:
    /// - a [`Scope::New`] target, judged by its added participants alone, and only for a kind that
    ///   [creates a target](crate::WriteKind::creates_target);
    /// - typing and a read receipt on a chat by its id ([`is_chat_id`]: a 1:1, group or meeting chat), allowed with
    ///   only lab accounts or approved services added (if any); off a chat (a channel, a chat id in another form) they
    ///   are fetched and judged like any write;
    /// - the account's own availability, allowed only on [`Scope::Account`] naming the fence's
    ///   [own account](Fence::own_account), a lab account, with nobody added.
    ///
    /// Nothing else lands on [`Scope::Account`]. Every decision is appended to `fence.log`; an allow that can't be
    /// logged is denied.
    ///
    /// # Errors
    /// [`Denied`] when the membership is partial, empty or org-wide, when any participant is outside the roster, when
    /// a kind that doesn't create a target names [`Scope::New`], when an own availability isn't the fence's own
    /// account's or another write names [`Scope::Account`], or when an allow can't be logged.
    pub async fn check(&self, target: Target, source: &dyn MembershipSource) -> Result<WritePermit, Denied> {
        let membership = match target.scope() {
            Scope::New | Scope::Account { .. } => Membership::Complete(Vec::new()),
            _ if target.kind() == WriteKind::OwnAvailability || is_signal_on_a_chat(&target) => {
                Membership::Complete(Vec::new())
            }
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
        if target.kind() == WriteKind::OwnAvailability || matches!(target.scope(), Scope::Account { .. }) {
            return self.judge_own(target);
        }
        let fetched = !matches!(target.scope(), Scope::New);
        if !fetched && !target.kind().creates_target() {
            return Verdict::Deny(DenyReason::NotCreatable);
        }
        if is_signal_on_a_chat(target) {
            return self.judge_added(target);
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
        self.admits_all(participants.into_iter())
    }

    /// The decision on typing or a read receipt on a chat: it reaches nobody, so the chat's members aren't read and
    /// only the participants it names itself, if any, are judged.
    fn judge_added(&self, target: &Target) -> Verdict {
        self.admits_all(target.added().iter())
    }

    /// Allow when the roster admits every one of `participants`.
    fn admits_all<'a>(&self, participants: impl Iterator<Item = &'a Mri>) -> Verdict {
        let count = participants.filter(|mri| !self.roster.admits(mri)).count();
        if count > 0 {
            Verdict::Deny(DenyReason::Outsiders { count })
        } else {
            Verdict::Allow
        }
    }

    /// The decision on a write that is the account's own availability or lands on [`Scope::Account`]: allowed only
    /// when it is the own availability of the fence's own account, a lab account of the roster, with nobody added.
    fn judge_own(&self, target: &Target) -> Verdict {
        if target.kind() != WriteKind::OwnAvailability {
            return Verdict::Deny(DenyReason::AccountScope);
        }
        let own = match target.scope() {
            Scope::Account { mri } => self
                .own
                .as_ref()
                .is_some_and(|own| own.as_str() == mri && self.roster.is_lab_account(own)),
            _ => false,
        };
        if own && target.added().is_empty() {
            Verdict::Allow
        } else {
            Verdict::Deny(DenyReason::NotOwnAccount)
        }
    }
}

/// Whether `target` is typing or a read receipt on a chat by its id ([`is_chat_id`]: a 1:1, group or meeting chat, the
/// same rule [`crate::contacting`] carves them out by), which the fence permits without a member read. On anything else
/// (a channel, a team, a call, a chat id in another form) they are judged like any write.
fn is_signal_on_a_chat(target: &Target) -> bool {
    let Scope::Conversation { id } = target.scope() else {
        return false;
    };
    !target.kind().is_contacting() && target.kind() != WriteKind::OwnAvailability && is_chat_id(id)
}
