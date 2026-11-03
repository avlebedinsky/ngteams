//! The decision: [`Fence::check`] fetches the target's members fresh and issues a [`WritePermit`] unless it reaches a
//! person the lab may not contact: every participant must be a lab account, an approved service, a contact (the
//! user's own account) or an app or a bot, which is no person ([`Roster`]). A write that reaches nobody
//! ([`WriteKind::is_contacting`] false) is judged without a member read: typing and read receipts on a chat (1:1, group
//! or meeting), and the account's own availability ([`WriteKind::OwnAvailability`]).
//!
//! A personal fence (`Fence::open_personal`, only with the `personal` feature) decides nothing: it logs every write and
//! permits it (`decision/audit.rs`), keeping only the call floor's one call at a time across the app's accounts.

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::calls::{
    CallRegistry, Delivery, DeliveryDecision, DeliveryPermit, Observed, PendingAnswer, PendingStart, RegisterError,
    Resolved, Response, may_be_invitation, read_invitation,
};
use crate::contacting::call::{start_evidence, start_target};
use crate::contacting::json::strict_json;
use crate::floor::CallFloor;
use crate::lab::{Alias, ConfigError, LabConfig, LabLocations};
use crate::log::{FenceLog, Verdict};
use crate::membership::{is_chat_id, is_team_thread_id};
use crate::target::{CallOperation, Mri, Scope, Target, WriteKind};

#[cfg(feature = "personal")]
mod audit;

/// Who a write may reach: the lab accounts, the approved service participants and the contacts (`lab.toml`'s
/// `contacts`, the user's own accounts), by exact MRI, or by mail address ([`Mri::mail`]) for a lab account or a
/// contact whose UPN is known. A lab account, service or contact whose MRI isn't known yet is simply left out, so it
/// matches nobody. Apps and bots aren't people: any `28:` MRI is admitted wherever it appears ([`Roster::admits`]).
///
/// A lab binary's roster comes only from `lab.toml`, through [`Fence::open_lab`]. [`Roster::new`], which takes any
/// MRIs, exists only with the `test-support` feature.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roster {
    lab: BTreeSet<Mri>,
    services: BTreeSet<Mri>,
    contacts: BTreeSet<Mri>,
    /// The lab accounts' and the contacts' mail addresses (their UPNs), in lower case: a calendar event's attendees are
    /// judged by them ([`Mri::mail`]).
    mail: BTreeSet<String>,
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
            contacts: BTreeSet::new(),
            mail: BTreeSet::new(),
        }
    }

    /// This roster with `addresses` as the lab accounts' and contacts' mail addresses. Tests only (`test-support`).
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn with_lab_mail<'a>(mut self, addresses: impl IntoIterator<Item = &'a str>) -> Self {
        self.mail = addresses
            .into_iter()
            .map(|address| address.trim().to_lowercase())
            .collect();
        self
    }

    /// This roster with `contacts` as its contacts. Tests only (`test-support`).
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn with_contacts(mut self, contacts: impl IntoIterator<Item = Mri>) -> Self {
        self.contacts = contacts.into_iter().collect();
        self
    }

    /// The MRIs `lab.toml` gives its accounts, approved service participants and contacts; one without an MRI is left
    /// out.
    fn from_lab(config: &LabConfig) -> Self {
        let roster = Self::of(
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
        );
        let lab_mail = config.accounts().iter().map(|account| account.upn.as_str());
        let contact_mail = config.contacts().iter().filter_map(|contact| contact.upn.as_deref());
        Self {
            contacts: config.contacts().iter().map(|contact| Mri::new(&contact.mri)).collect(),
            mail: lab_mail
                .chain(contact_mail)
                .map(|upn| upn.trim().to_lowercase())
                .collect(),
            ..roster
        }
    }

    /// Whether a write may reach `mri`: a lab account, an approved service or a contact, by exact MRI; a participant
    /// known by address ([`Mri::mail`]) whose address is a lab account's or a contact's; or an app or a bot (a `28:`
    /// MRI), which is no person.
    #[must_use]
    pub fn admits(&self, mri: &Mri) -> bool {
        self.lab.contains(mri)
            || self.services.contains(mri)
            || self.contacts.contains(mri)
            || mri.is_bot()
            || mri.mail_address().is_some_and(|address| self.mail.contains(address))
    }

    /// Whether `mri` is one of the lab accounts, by exact MRI (an approved service or a contact isn't).
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
    /// A call operation whose link no call or offered invitation of this run's fence holds: a call this fence didn't
    /// permit, whose answer or acceptance wasn't read, or of another run; or, under the lab's call rule, an answer or a
    /// decline of a call leg no offered invitation names.
    UnregisteredCall,
    /// A call operation on a call that ended (or is ending, for anything but its `leave`).
    CallEnded,
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
            Self::UnregisteredCall => f.write_str("no call or offered invitation of this run's fence holds this link"),
            Self::CallEnded => f.write_str("the call ended"),
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
/// `Clone`, and is bound to the target it was issued for and to the fence that issued it (its run-unique id and the
/// permit's serial, so the call registry takes a call start's permit only from its own fence, and only once).
#[derive(Debug)]
pub struct WritePermit {
    target: Target,
    issuer: u64,
    serial: u64,
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
    /// An org-wide team or channel.
    OrgWide,
    /// Participants that are people outside the lab: neither lab accounts, approved services nor contacts.
    Outsiders { count: usize },
    /// A [`Scope::New`] target for a kind that doesn't create one: the write lands on something that already exists,
    /// whose members have to be fetched.
    NotCreatable,
    /// A write other than the account's own availability on [`Scope::Account`]: nothing else lands on the account
    /// itself.
    AccountScope,
    /// The write would be allowed, but its decision couldn't be written to `fence.log`.
    LogUnavailable,
    /// A call operation that doesn't fit the registered call holding its link: another kind of write, a sender other
    /// than the call's caller, or participants added by anything but an `add`.
    CallMismatch,
    /// A start, a join or an answer while another account of the app is in a call or one rings for it: one call at a
    /// time across the accounts sharing a call floor ([`crate::CallFloor`]).
    AnotherAccountInCall,
}

impl fmt::Display for DenyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Partial(reason) => write!(f, "membership not known in full: {reason}"),
            Self::OrgWide => f.write_str("an org-wide team or channel"),
            Self::Outsiders { count } => write!(f, "{count} participant(s) outside the lab"),
            Self::NotCreatable => f.write_str("this kind of write doesn't create a new target"),
            Self::AccountScope => f.write_str("only the account's own availability lands on the account itself"),
            Self::LogUnavailable => f.write_str("fence.log can't be written"),
            Self::CallMismatch => f.write_str("the write doesn't fit the registered call"),
            Self::AnotherAccountInCall => f.write_str("another account of the app is in a call"),
        }
    }
}

/// A denied write: the target and the reason. Carries no credentials. Its `Display` and `Debug` show the added
/// participants as a count only, but [`Denied::target`] gives the whole [`Target`], whose [`Target::added`] holds
/// their MRIs, and the scope ids are shown as they are (a 1:1 chat id holds both members' object ids).
#[derive(Debug, thiserror::Error)]
#[error("fence denied {target}: {reason}")]
pub struct Denied {
    // boxed: a call operation's target is large, and every write's result carries this
    target: Box<Target>,
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
    rules: Rules,
    roster: Roster,
    log: FenceLog,
    /// The account this fence is a client's of ([`Fence::open_lab_account`], or the personal build's signed-in account
    /// through `Fence::open_personal`): the one account whose incoming calls it offers. `None` for a fence of no account
    /// ([`Fence::open_lab`], the browser fence's) or of an account whose MRI `lab.toml` doesn't know yet: it offers no
    /// call.
    own: Option<Mri>,
    /// This fence's run-unique id, which its permits carry.
    id: u64,
    /// The serial of the next permit.
    serial: AtomicU64,
    /// The calls this fence permitted, and the links their services handed out ([`crate::calls`]).
    calls: Arc<CallRegistry>,
    /// The call floor this fence shares with the other accounts of its app ([`Fence::sharing_calls`]); `None` for a
    /// fence of its own.
    floor: Option<CallFloor>,
    /// How it decides an operation on a call and a call delivery response.
    call_rule: CallRule,
}

/// How a fence decides an operation on a call ([`Scope::CallOperation`]) and a call delivery response
/// ([`Fence::check_delivery_response`]). A personal fence keeps [`CallRule::Entry`]'s bookkeeping and judges nothing
/// (`decision/audit.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallRule {
    /// The lab's: only a call's entry is judged, by who takes part ([`Roster::admits`]). A join by the chat it joins
    /// (read fresh and complete), an answer or a decline only of an offered invitation (to the fence's own account, its
    /// group chat read fresh), an `add` by its invitees; everything else a call sends once it was let in (its follow-ups, its
    /// ring-time sends, its `leave`, its delivery responses) is permitted and logged with no registry lookup. The
    /// registry still reads what passes (its deliveries, its answers), so the native client's call machine sees the
    /// same deliveries.
    Entry,
    /// The call registry's ([`crate::calls`]): a follow-up only to a link a call or an offered invitation this fence
    /// registered handed out, a delivery response only to a delivery it waits for. No build's fence decides by it: tests
    /// turn it on ([`Fence::tracking_calls`]) to check the registry's resolution, which [`CallRule::Entry`]'s
    /// bookkeeping binds by, as verdicts.
    #[cfg(feature = "test-support")]
    Registry,
}

/// The id of the next fence made in this process.
static NEXT_FENCE: AtomicU64 = AtomicU64::new(1);

/// Which rules a fence decides by: the lab's, or (only with the `personal` feature, through
/// `Fence::open_personal`) the personal build's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rules {
    Lab,
    #[cfg(feature = "personal")]
    Personal,
}

impl Fence {
    /// The lab's fence. Its roster is the MRIs `lab.toml` gives the lab accounts, the approved service participants and
    /// the contacts (with the accounts' and contacts' mail addresses), and its decisions go to `<data_dir>/fence.log`; both files are found from the environment, as
    /// [`LabLocations::from_env`] finds them.
    ///
    /// # Errors
    /// When `lab.toml` can't be found, read or validated, or `fence.log` can't be opened.
    pub fn open_lab() -> Result<Self, OpenError> {
        Self::from_config(&LabConfig::load(&LabLocations::from_env()?)?)
    }

    /// The fence of one lab account's native client: [`Fence::open_lab`]'s, whose own account is `alias`'s, by the MRI
    /// `lab.toml` gives it. It offers the incoming calls addressed to that account ([`Fence::observe_delivery`]); an
    /// account whose MRI isn't known yet is offered none.
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
        Ok(Self::of(Rules::Lab, Roster::from_lab(config), log, None))
    }

    /// A fence of `rules`, with its own id, no permit issued yet and no call registered. Its call registry keeps the
    /// bookkeeping of the lab's call rule ([`CallRule::Entry`]); a personal fence judges no call by it.
    fn of(rules: Rules, roster: Roster, log: FenceLog, own: Option<Mri>) -> Self {
        let call_rule = CallRule::Entry;
        Self {
            rules,
            roster,
            log,
            own,
            id: NEXT_FENCE.fetch_add(1, Ordering::Relaxed),
            serial: AtomicU64::new(0),
            calls: Arc::new(CallRegistry::default()),
            floor: None,
            call_rule,
        }
    }

    fn from_account(config: &LabConfig, alias: Alias) -> Result<Self, OpenError> {
        let mut fence = Self::from_config(config)?;
        fence.own = config.account(alias).mri.as_deref().map(Mri::new);
        Ok(fence)
    }

    /// This fence sharing `floor` with the other accounts of its app: one call or ringing invitation at a time across
    /// them all ([`crate::floor`]). A fence shares one floor at most; a second call replaces the first.
    #[must_use]
    pub fn sharing_calls(mut self, floor: &CallFloor) -> Self {
        floor.join(self.id, &self.calls);
        self.floor = Some(floor.clone());
        self
    }

    /// A fence over any roster and any log. Tests only (`test-support`).
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn new(roster: Roster, log: FenceLog) -> Self {
        Self::of(Rules::Lab, roster, log, None)
    }

    /// The personal fence of the account `own` over any log. Tests only (`test-support` and `personal`).
    #[cfg(all(feature = "test-support", feature = "personal"))]
    #[must_use]
    pub fn personal(own: Mri, log: FenceLog) -> Self {
        Self::of_personal(own, log)
    }

    /// The personal fence of the account `own`, logging to `log`.
    #[cfg(feature = "personal")]
    pub(crate) fn of_personal(own: Mri, log: FenceLog) -> Self {
        Self::of(Rules::Personal, Roster::default(), log, Some(own))
    }

    /// Whether this is the personal build's fence (`Fence::open_personal`, only with the `personal` feature); `false`
    /// for every lab fence. A personal fence denies nothing and logs every send: the native transport sends every plain
    /// request through it, logged, and its reads by `POST` go under its permits ([`Fence::check_post_read`]).
    #[must_use]
    pub fn is_personal(&self) -> bool {
        self.rules != Rules::Lab
    }

    /// The fence deciding calls by the call registry's own rule (see [`Fence::tracks_calls`]), over its lab roster.
    /// Tests only (`test-support`): no build's fence decides by it; it shows the registry's resolution as verdicts.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn tracking_calls(mut self) -> Self {
        self.call_rule = CallRule::Registry;
        self
    }

    /// Whether this fence decides an operation on a call and a call delivery response by its call registry's own rule
    /// ([`Fence::tracking_calls`], tests only): only to a link a call or an offered invitation it registered handed
    /// out, a response only to a delivery it waits for. No build's fence does (`false`): a lab fence judges a call's
    /// entry (its start, a join, an answer or a decline, an `add`) and permits what the call sends once it was let in,
    /// a personal fence judges nothing; either registry only keeps what it reads. The native transport refuses a call
    /// write the registry won't pend only on a fence that tracks calls.
    #[must_use]
    pub fn tracks_calls(&self) -> bool {
        self.call_rule != CallRule::Entry
    }

    /// The fence with `mri` as its own account (see [`Fence::own_account`]). Tests only (`test-support`).
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn with_own_account(mut self, mri: Mri) -> Self {
        self.own = Some(mri);
        self
    }

    /// The account whose incoming calls this fence offers: the account a [`Fence::open_lab_account`] was opened
    /// for, when `lab.toml` knows its MRI, or the signed-in account of a personal fence (`Fence::open_personal`, which
    /// opens only once its `accounts.toml` entry holds its object id); `None` otherwise.
    #[must_use]
    pub fn own_account(&self) -> Option<&Mri> {
        self.own.as_ref()
    }

    /// The log every decision goes to; the browser fence records its own request decisions there too.
    #[must_use]
    pub fn log(&self) -> &FenceLog {
        &self.log
    }

    /// Fetches the target scope's members from `source` (never from a cache) and allows the write only when every
    /// participant, current or added, is one the lab may reach ([`Roster::admits`]). Every write is fetched so but four:
    /// - a [`Scope::New`] target, judged by its added participants alone, and only for a kind that
    ///   [creates a target](crate::WriteKind::creates_target);
    /// - typing and a read receipt on a chat by its id ([`is_chat_id`]: a 1:1, group or meeting chat), allowed with
    ///   only participants the roster admits added (if any); off a chat (a channel, a chat id in another form) they
    ///   are fetched and judged like any write;
    /// - the account's own availability, allowed with nobody added but participants the roster admits;
    /// - an operation on a call ([`Scope::CallOperation`]), by the fence's call rule. A lab fence judges a call's entry
    ///   only: a join by the group chat it names, read fresh, complete and listing the joiner; an answer or a decline
    ///   only of an invitation it offered (by the call leg its link names), the invitation's group chat read fresh; an
    ///   `add` by its sender and invitees; and it permits every other operation (what a call sends once it was let in)
    ///   with no member read and no registry lookup, logged. Under the registry's rule ([`Fence::tracks_calls`]) the
    ///   call registry ([`crate::calls`]) resolves it to a live call this fence permitted and registered, with no
    ///   member read. Either way the participants (with any invitee and the chat's members) and the sender of a start,
    ///   a join or an `add` ([`Target::sender`] for a start) must all be ones the roster admits.
    ///
    /// Nothing else lands on [`Scope::Account`]. Every decision is appended to `fence.log`; an allow that can't be
    /// logged is denied.
    ///
    /// A personal fence (`Fence::open_personal`, only with the `personal` feature) decides nothing
    /// (`docs/personal-build.md`, `decision/audit.rs`): it reads no members and permits every write, logged, an
    /// operation on a call moving the call registry as the lab's call rule moves it. The one thing it holds back is the
    /// call floor's: a start, a join or an answer while another account of the app is in a call.
    ///
    /// # Errors
    /// [`Denied`] when the membership is partial or org-wide, when any participant is a person outside the roster, when
    /// a kind that doesn't create a target names [`Scope::New`], when a write other than an own availability names
    /// [`Scope::Account`], or when an allow can't be logged. A lab fence denies an
    /// answer or a decline of no offered invitation as [`PartialReason::UnregisteredCall`], one of an invitation no
    /// longer open for it as [`PartialReason::CallEnded`], and a join naming no chat or an `add` naming nobody as
    /// [`DenyReason::CallMismatch`]. Under the registry's rule a call operation is denied as
    /// [`PartialReason::UnregisteredCall`] when no registered call holds its link,
    /// [`PartialReason::CallEnded`] when its call is ending (other than its `leave`) or ended, and
    /// [`DenyReason::CallMismatch`] when it isn't its operation's kind, from the call's caller, adding exactly its
    /// invitees and handing out only the call's own callbacks. A personal fence denies only an allow it can't log. A
    /// fence sharing a call floor
    /// ([`Fence::sharing_calls`]) denies a start, a join or an answer while another account on it is busy with a call
    /// as [`DenyReason::AnotherAccountInCall`], before anything else is read.
    pub async fn check(&self, target: Target, source: &dyn MembershipSource) -> Result<WritePermit, Denied> {
        if self.another_account_in_call(&target) {
            return self.decided(target, Verdict::Deny(DenyReason::AnotherAccountInCall));
        }
        #[cfg(feature = "personal")]
        if self.rules == Rules::Personal {
            return self.audit(target);
        }
        let membership = match target.scope() {
            Scope::CallOperation { .. } if self.call_rule == CallRule::Entry => {
                return self.check_call_entry(target, source).await;
            }
            Scope::CallOperation { .. } => return self.check_call_operation(target, source).await,
            Scope::New | Scope::Account { .. } => Membership::Complete(Vec::new()),
            _ if target.kind() == WriteKind::OwnAvailability || is_signal_on_a_chat(&target) => {
                Membership::Complete(Vec::new())
            }
            scope => source.members(scope).await,
        };
        let verdict = self.judge(&target, membership);
        self.decided(target, verdict)
    }

    /// Whether `target` enters a call (a start, a join, an answer) while another account on this fence's call floor is
    /// busy with one ([`crate::floor`]).
    fn another_account_in_call(&self, target: &Target) -> bool {
        matches!(target.kind(), WriteKind::Call | WriteKind::CallAnswer)
            && self.floor.as_ref().is_some_and(|floor| floor.others_busy(self.id))
    }

    /// Logs `verdict` on `target` and gives the permit when it allows and was logged.
    fn decided(&self, target: Target, verdict: Verdict) -> Result<WritePermit, Denied> {
        let logged = self.log.record(&target, verdict);
        match (verdict, logged) {
            (Verdict::Allow, Ok(())) => Ok(WritePermit {
                target,
                issuer: self.id,
                serial: self.serial.fetch_add(1, Ordering::Relaxed),
            }),
            (Verdict::Allow, Err(_)) => Err(Denied {
                target: Box::new(target),
                reason: DenyReason::LogUnavailable,
            }),
            (Verdict::Deny(reason), _) => Err(Denied {
                target: Box::new(target),
                reason,
            }),
        }
    }

    /// The decision on an operation on a registered call, an offered invitation or a join ([`Scope::CallOperation`]):
    /// the call registry resolves its link to a live call this fence registered, an invitation it offered in a state
    /// open for the operation (ringing, answered, leaving or declined), or, for a join, the group chat an offered
    /// invitation bound the conversation to ([`crate::calls`]); the participants (the call's; the invitation's caller and
    /// callee, or the joiner, with every member of the group chat, read fresh and complete) together with any invitee
    /// must all be ones the roster admits, and an invitation must still be open for the operation once the chat was
    /// read. A permitted `leave` moves its call (or its answered invitation) to leaving, a permitted `attach`
    /// binds its invitation's call agent and the callbacks its ring ends at, a permitted `accept` answers its invitation
    /// and a permitted `reject` declines it.
    async fn check_call_operation(&self, target: Target, source: &dyn MembershipSource) -> Result<WritePermit, Denied> {
        let resolved = match self.calls.resolve(&target) {
            Ok(resolved) => resolved,
            Err(reason) => return self.decided(target, Verdict::Deny(reason)),
        };
        self.judge_resolved(target, resolved, source).await
    }

    /// The lab's decision on an operation on a call ([`CallRule::Entry`]): only a call's entry is judged, by who takes
    /// part in it.
    /// - a join (a `POST` to a conversation): the group chat it names (`joins`), read fresh, complete and listing the
    ///   joiner, every member and the joiner admitted ([`Roster::admits`]);
    /// - an answer (`accept`) or a decline (`reject`): only of an invitation this fence offered (to its own account), by
    ///   the call leg its link names, still open for it; the invitation's parties and its group chat, read fresh, all
    ///   admitted;
    /// - an `add`, a meeting lobby's admit or a removal (its Deny): its sender and every person it names admitted;
    /// - anything else (a follow-up of a call let in, a ring-time send, a `leave`): permitted and logged, with no member
    ///   read and no registry lookup. The registry still reads it (a `leave` ends its call there, an `attach` binds the
    ///   callbacks its ring ends at), but decides nothing.
    async fn check_call_entry(&self, target: Target, source: &dyn MembershipSource) -> Result<WritePermit, Denied> {
        let Scope::CallOperation {
            operation, from, joins, ..
        } = target.scope()
        else {
            return self.decided(target, Verdict::Deny(DenyReason::CallMismatch));
        };
        let sender: BTreeSet<Mri> = from.iter().cloned().collect();
        if matches!(
            operation,
            CallOperation::Join | CallOperation::Add | CallOperation::Admit | CallOperation::RemoveParticipant
        ) && !sender.iter().all(|mri| self.roster.admits(mri))
        {
            let count = sender.iter().filter(|mri| !self.roster.admits(mri)).count();
            return self.decided(target, Verdict::Deny(DenyReason::Outsiders { count }));
        }
        let resolved = match operation {
            CallOperation::Join if joins.is_some() && !sender.is_empty() && target.added().is_empty() => {
                Ok(Resolved::unbound(sender, joins.clone()))
            }
            CallOperation::Add | CallOperation::Admit | CallOperation::RemoveParticipant
                if !target.added().is_empty() =>
            {
                Ok(Resolved::unbound(sender, None))
            }
            CallOperation::Join | CallOperation::Add | CallOperation::Admit | CallOperation::RemoveParticipant => {
                Err(DenyReason::CallMismatch)
            }
            CallOperation::Accept | CallOperation::Reject => self.calls.resolve_answer(&target),
            _ => {
                // logged first: a permit the log can't take is refused, and the registry never hears of it
                let permit = self.decided(target, Verdict::Allow)?;
                self.calls.note(permit.target());
                return Ok(permit);
            }
        };
        match resolved {
            Ok(resolved) => self.judge_resolved(target, resolved, source).await,
            Err(reason) => self.decided(target, Verdict::Deny(reason)),
        }
    }

    /// The decision on a call operation resolved to what it acts on: its group chat, if any, read fresh; still open
    /// once it was read; every participant (the resolved ones, the chat's members, any invitee) admitted. A permitted
    /// one moves what it acts on (`CallRegistry::permitted`).
    async fn judge_resolved(
        &self,
        target: Target,
        resolved: Resolved,
        source: &dyn MembershipSource,
    ) -> Result<WritePermit, Denied> {
        let chat = match self
            .chat_members(resolved.chat.as_deref(), &resolved.participants, source)
            .await
        {
            Ok(members) => members,
            Err(reason) => return self.decided(target, Verdict::Deny(reason)),
        };
        if !self.calls.still_open(&resolved) {
            return self.decided(target, Verdict::Deny(DenyReason::Partial(PartialReason::CallEnded)));
        }
        let participants: BTreeSet<&Mri> = resolved
            .participants
            .iter()
            .chain(&chat)
            .chain(target.added())
            .collect();
        let verdict = self.admits_all(participants.into_iter());
        let permit = self.decided(target, verdict)?;
        self.calls.permitted(&resolved);
        Ok(permit)
    }

    /// The members of the group chat an offered invitation names (`chat`), read fresh from `source`: complete, and
    /// listing every one of `parties` (its caller and callee). No chat, no read.
    async fn chat_members(
        &self,
        chat: Option<&str>,
        parties: &BTreeSet<Mri>,
        source: &dyn MembershipSource,
    ) -> Result<Vec<Mri>, DenyReason> {
        let Some(chat) = chat else {
            return Ok(Vec::new());
        };
        // a channel's call is joined by its team: read from the team's thread
        let scope = if is_team_thread_id(chat) {
            Scope::Team { id: chat.to_owned() }
        } else {
            Scope::Conversation { id: chat.to_owned() }
        };
        match source.members(&scope).await {
            Membership::Complete(members) if parties.iter().all(|party| members.contains(party)) => Ok(members),
            // a group call's invitation whose chat doesn't list its own caller and callee isn't bound to that chat
            Membership::Complete(_) => Err(DenyReason::CallMismatch),
            Membership::Partial(reason) => Err(DenyReason::Partial(reason)),
            Membership::OrgWide => Err(DenyReason::OrgWide),
        }
    }

    /// Takes a permit this fence issued for a call start ([`WriteKind::Call`]: the test call's on [`Scope::New`], a
    /// chat's call start on the chat it carries, or a join with media of a running call on the group chat an offered
    /// invitation bound its conversation to), once, with the start's body, which must read, by the same row, as the
    /// start the permit was issued for: the call is then pending, waiting for its answer ([`Fence::register_start`]),
    /// and grants nothing yet ([`crate::calls`]). `None` for another fence's permit, one taken already, another kind of
    /// write, a join without media (it starts no call), or a body that doesn't give the permit's target.
    #[must_use]
    pub fn pend_start(&self, permit: &WritePermit, start_body: &[u8]) -> Option<PendingStart> {
        let target = permit.target();
        if permit.issuer != self.id || target.kind() != WriteKind::Call {
            return None;
        }
        let evidence = start_evidence(start_body)?;
        if start_target(target, start_body, &evidence.caller).as_ref() != Some(target) {
            return None;
        }
        self.calls.pend(self.id, permit.serial, target, &evidence)
    }

    /// Registers a pending call with its start's answer, read in full ([`crate::calls`]): its conversation's links
    /// become the links its operations may go to.
    ///
    /// # Errors
    /// [`RegisterError`] when the pending start is another fence's, isn't pending any more, or the answer isn't a
    /// conversation in the captured shape (the call then never registers).
    pub fn register_start(&self, pending: PendingStart, answer: &[u8]) -> Result<(), RegisterError> {
        if pending.fence() != self.id {
            return Err(RegisterError::OtherFence);
        }
        self.calls.register(pending, answer)
    }

    /// Ends a pending call unregistered: its start was answered without success (the service didn't take it), so
    /// nothing it named binds and a start sent again, a pending call of its own with the same callbacks, is the one an
    /// acceptance binds to ([`crate::calls`]). Nothing for another fence's pending start.
    pub fn abandon_start(&self, pending: PendingStart) {
        if pending.fence() == self.id {
            self.calls.abandon(pending);
        }
    }

    /// Takes a permit this fence issued for an `attach` or an `accept` on an offered invitation
    /// ([`crate::CallOperation::Attach`], [`crate::CallOperation::Accept`]), once: its answer is then pending
    /// ([`Fence::bind_answer`]). `None` for another fence's permit, one taken already, another write, or an invitation
    /// no longer in the state the permit left it in.
    #[must_use]
    pub fn pend_answer(&self, permit: &WritePermit) -> Option<PendingAnswer> {
        let Scope::CallOperation { operation, url, .. } = permit.target().scope() else {
            return None;
        };
        if permit.issuer != self.id {
            return None;
        }
        self.calls.pend_answer(self.id, permit.serial, *operation, url)
    }

    /// Binds the links a pending answer hands out (read in full) to its invitation ([`crate::calls`]): an `attach`'s
    /// answer the call leg's links (its `progress`, its `acceptance`, its rejection) and the subscribed conversation's;
    /// an `accept`'s answer the answered call's `mediaRenegotiation`, `updateMediaDescriptions` and
    /// `applyChannelParameters` links.
    ///
    /// # Errors
    /// [`RegisterError`] when the pending answer is another fence's, its invitation moved on, or the answer binds
    /// nothing.
    pub fn bind_answer(&self, pending: PendingAnswer, answer: &[u8]) -> Result<(), RegisterError> {
        if pending.fence() != self.id {
            return Err(RegisterError::OtherFence);
        }
        self.calls.bind_answer(pending, answer)
    }

    /// Retires the ringing invitation whose `attach` link is exactly `attach`, once the account's call machine let it
    /// go before it rang (its attach failed for good: no further attempt goes; its answer didn't read; its media
    /// failed): its ring ends here for good, so it holds no call open (a personal fence offers the next invitation) and
    /// nothing passes on it any more ([`crate::calls`]). An attempt that a further one may follow (a 401, a short
    /// throttle) retires nothing. It only ends an invitation still ringing; it never permits anything.
    pub fn let_go(&self, attach: &str) {
        self.calls.let_go(attach);
    }

    /// Reads a Trouter delivery the account's socket received ([`crate::calls`]): a pending or registered call's
    /// acceptance binds its call controller's links, and the end of a call ends it; an incoming call's invitation is
    /// offered when it is addressed to this fence's [own account](Fence::own_account), a lab account, and its caller
    /// is one the roster admits ([`Roster::admits`]; a fence of no account offers none; a personal fence offers any one
    /// addressed to its account whose caller isn't the account, one at a time, as a Teams client rings for one call at
    /// a time), and a delivery at an offered invitation's end callback ends its ring. A delivery at an endpoint's root that may be an invitation but doesn't
    /// read is [`Observed::Ignored`] too. It sends nothing and permits nothing by itself.
    pub fn observe_delivery(&self, delivery: &Delivery) -> Observed {
        let Some(invitation) = read_invitation(delivery) else {
            if may_be_invitation(delivery) {
                return Observed::Ignored;
            }
            return self.calls.observe(delivery);
        };
        // the personal build's: any readable invitation addressed to the account, whoever it is from, one at a time
        let admitted = self.is_personal()
            || (self.roster.is_lab_account(&invitation.callee) && self.roster.admits(&invitation.caller));
        let offered =
            self.own.as_ref() == Some(&invitation.callee) && admitted && invitation.caller != invitation.callee;
        if !offered {
            return Observed::Ignored;
        }
        let Some(floor) = &self.floor else {
            return self.calls.offer(invitation, delivery, self.is_personal());
        };
        // one at a time across the accounts on the floor: held, so two accounts can't both be offered one at once; its
        // own account's invitations by its own rule
        let _held = floor.hold();
        if floor.others_busy(self.id) && !self.calls.offers(&invitation) {
            return Observed::Ignored;
        }
        self.calls.offer(invitation, delivery, self.is_personal())
    }

    /// Forgets the pending deliveries of a connection that closed.
    pub fn close_connection(&self, connection: &str) {
        self.calls.close_connection(connection);
    }

    /// Decides a Trouter delivery response sent on `connection` (`response`: the socket.io message's JSON, after
    /// `3:::`). A lab or personal fence permits every one that names a delivery (a numeric `id`), logged
    /// ([`Self::permit_delivery_response`]). Under the registry's rule ([`Fence::tracks_calls`]) it is decided once per
    /// delivery ([`crate::calls`]): permitted only as the captured acknowledgement of a pending
    /// acceptance delivery of a live call on that connection, as the captured receipt (an empty body) of a renegotiation
    /// or its answer delivered on that connection at a live call's or an answered invitation's own callback, or as the
    /// captured response to an offered invitation's delivery on that connection while it rings, its caller and callee
    /// admitted ([`Roster::admits`]) and the group chat it names, if any, of them only by a fresh, complete read from
    /// `source` (listing both). A decision is logged; a response to no pending delivery is [`DeliveryDecision::NotPending`], neither decided nor logged.
    pub async fn check_delivery_response(
        &self,
        connection: &str,
        response: &[u8],
        source: &dyn MembershipSource,
    ) -> DeliveryDecision {
        if self.call_rule == CallRule::Entry {
            return self.permit_delivery_response(connection, response);
        }
        let (kind, decided) = match self.calls.respond(connection, response) {
            None => return DeliveryDecision::NotPending,
            Some(Response::Acknowledgement(decided)) => (ACKNOWLEDGEMENT, decided),
            Some(Response::NegotiationReceipt(decided)) => (NEGOTIATION_RECEIPT, decided),
            Some(Response::ReceiptDenied(reason)) => (RECEIPT, Err(reason)),
            Some(Response::Receipt {
                delivery,
                offer,
                participants,
                chat,
            }) => {
                let judged = match self.chat_members(chat.as_deref(), &participants, source).await {
                    Ok(members) if !self.calls.still_ringing(offer) => {
                        drop(members);
                        Err(crate::calls::DeliveryDenyReason::NotLive)
                    }
                    Ok(members) => {
                        let all = participants.iter().chain(&members);
                        match self.admits_all(all) {
                            Verdict::Allow => Ok(delivery),
                            Verdict::Deny(_) => Err(crate::calls::DeliveryDenyReason::Parties),
                        }
                    }
                    Err(_) => Err(crate::calls::DeliveryDenyReason::Parties),
                };
                (RECEIPT, judged)
            }
        };
        let reason = decided.as_ref().err().copied();
        let logged = self.log.record_delivery_response(kind, reason);
        match (decided, logged) {
            (Ok(delivery), Ok(())) => DeliveryDecision::Permitted(DeliveryPermit::new(delivery)),
            (Ok(_), Err(_)) => DeliveryDecision::Denied(crate::calls::DeliveryDenyReason::LogUnavailable),
            (Err(reason), _) => DeliveryDecision::Denied(reason),
        }
    }

    /// The lab's and the personal fence's decision on a call delivery response ([`CallRule::Entry`]): a response to a delivery the account's
    /// calling socket received goes back to that call's services, so it is permitted and logged, whatever it answers;
    /// the registry still takes the delivery it answers, if one waits, which names its kind in the log. A response
    /// without a numeric `id` answers no delivery: [`DeliveryDecision::NotPending`].
    fn permit_delivery_response(&self, connection: &str, response: &[u8]) -> DeliveryDecision {
        let Some(id) = strict_json(response).and_then(|envelope| envelope.get("id")?.as_i64()) else {
            return DeliveryDecision::NotPending;
        };
        let kind = match self.calls.respond(connection, response) {
            Some(Response::Acknowledgement(_)) => ACKNOWLEDGEMENT,
            Some(Response::NegotiationReceipt(_)) => NEGOTIATION_RECEIPT,
            Some(Response::Receipt { .. } | Response::ReceiptDenied(_)) => RECEIPT,
            None => DELIVERY_RESPONSE,
        };
        match self.log.record_delivery_response(kind, None) {
            Ok(()) => DeliveryDecision::Permitted(DeliveryPermit::new(id)),
            Err(_) => DeliveryDecision::Denied(crate::calls::DeliveryDenyReason::LogUnavailable),
        }
    }

    fn judge(&self, target: &Target, membership: Membership) -> Verdict {
        if target.kind() == WriteKind::OwnAvailability || matches!(target.scope(), Scope::Account { .. }) {
            return self.judge_own(target);
        }
        if let Some(sender) = target.sender()
            && !self.roster.admits(sender)
        {
            return Verdict::Deny(DenyReason::Outsiders { count: 1 });
        }
        if matches!(target.scope(), Scope::New) && !target.kind().creates_target() {
            return Verdict::Deny(DenyReason::NotCreatable);
        }
        if is_signal_on_a_chat(target) {
            return self.judge_added(target);
        }
        // apps and bots on a team or a chat aren't people (`Roster::admits`), and a target nobody but the account is
        // in reaches nobody: only a person outside the lab, current or added, denies it
        let current = match membership {
            Membership::Complete(members) => members,
            Membership::Partial(reason) => return Verdict::Deny(DenyReason::Partial(reason)),
            Membership::OrgWide => return Verdict::Deny(DenyReason::OrgWide),
        };
        self.admits_all(current.iter().chain(target.added()))
    }

    /// The decision on typing or a read receipt on a chat: it reaches nobody, so the chat's members aren't read and
    /// only the participants it names itself, if any, are judged.
    fn judge_added(&self, target: &Target) -> Verdict {
        self.admits_all(target.added().iter())
    }

    /// Allow when the roster admits every one of `participants` ([`Roster::admits`]).
    fn admits_all<'a>(&self, participants: impl Iterator<Item = &'a Mri>) -> Verdict {
        let outsiders: BTreeSet<&Mri> = participants.filter(|mri| !self.roster.admits(mri)).collect();
        if outsiders.is_empty() {
            Verdict::Allow
        } else {
            Verdict::Deny(DenyReason::Outsiders { count: outsiders.len() })
        }
    }

    /// The decision on a write that is the account's own availability or lands on [`Scope::Account`]: the account's
    /// own availability reaches nobody, so it is allowed unless it names a person outside the lab; nothing else lands
    /// on the account itself.
    fn judge_own(&self, target: &Target) -> Verdict {
        if target.kind() == WriteKind::OwnAvailability {
            self.admits_all(target.added().iter())
        } else {
            Verdict::Deny(DenyReason::AccountScope)
        }
    }
}

/// The kinds of call delivery response `fence.log` names: the acceptance's acknowledgement, an invitation's receipt, a
/// renegotiation's receipt, and (under the lab's call rule) any other.
const ACKNOWLEDGEMENT: &str = "call_acceptance_acknowledgement";
const RECEIPT: &str = "call_invitation_receipt";
const NEGOTIATION_RECEIPT: &str = "call_negotiation_receipt";
const DELIVERY_RESPONSE: &str = "call_delivery_response";

/// Whether `target` is typing or a read receipt on a chat by its id ([`is_chat_id`]: a 1:1, group or meeting chat, the
/// same rule [`crate::contacting`] carves them out by), which the fence permits without a member read. On anything else
/// (a channel, a team, a call, a chat id in another form) they are judged like any write.
fn is_signal_on_a_chat(target: &Target) -> bool {
    let Scope::Conversation { id } = target.scope() else {
        return false;
    };
    !target.kind().is_contacting() && target.kind() != WriteKind::OwnAvailability && is_chat_id(id)
}
