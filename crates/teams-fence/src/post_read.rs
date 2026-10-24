//! The fence's decision on a native read by `POST` of the personal build's sealed list: [`Fence::check_post_read`]
//! gives a [`PostReadPermit`] for one entry of `teams-proto`'s sealed `PostRead` list and its exact list of MRIs, or
//! [`PostReadDenied`]. The lab's native client sends its reads by `POST` as plain requests (they reach nobody); this
//! decision stays for the personal build (`docs/personal-build.md`), which keeps the sealed list and, in a lab build,
//! its limit to the lab's own accounts.
//!
//! The limit: the fence denies a list that is empty, that names an MRI twice, or that names anyone but the lab
//! accounts of its roster, an approved service participant included ([`Fence::lab_accounts_only`]). It fetches
//! nothing.
//!
//! Every decision is appended to `fence.log` (the entry's kind, how many MRIs it names, the verdict and the reason;
//! never the MRIs), and an allow that can't be logged is denied. What the request looks like on the wire (its
//! service, path, query, headers and body) is `teams-proto`'s to check against the entry's captured shape: this
//! crate knows MRIs, not HTTP.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::{Mutex, PoisonError};

use serde::Serialize;

use crate::decision::Fence;
use crate::target::Mri;

/// The proof that one read by `POST` passed the fence: the entry's kind and the exact, ordered list of MRIs it was
/// given for. Only [`Fence::check_post_read`] makes one: it has no public constructor and is not `Clone`, so one
/// permit covers one request (`teams-proto`'s `AuthorizedPostRead` consumes it).
#[derive(Debug)]
pub struct PostReadPermit {
    kind: &'static str,
    names: Vec<Mri>,
}

impl PostReadPermit {
    /// The entry the permit was given for (its `PostRead::KIND`).
    #[must_use]
    pub fn kind(&self) -> &'static str {
        self.kind
    }

    /// The MRIs the permit was given for, in the order they were asked.
    #[must_use]
    pub fn names(&self) -> &[Mri] {
        &self.names
    }
}

/// Why a read by `POST` was denied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PostReadDenyReason {
    /// It names nobody.
    NoNames,
    /// It names an MRI more than once.
    Duplicate,
    /// It names MRIs that aren't lab accounts (an approved service participant isn't one).
    Outsiders { count: usize },
    /// The personal fence: it names MRIs that are neither the account nor a member of one of the account's chats as
    /// the store holds them ([`ChatPeople`]), or a service's `28:` id.
    NotInChats { count: usize },
    /// It would be allowed, but its decision couldn't be written to `fence.log`.
    LogUnavailable,
}

impl fmt::Display for PostReadDenyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoNames => f.write_str("it names nobody"),
            Self::Duplicate => f.write_str("it names an MRI more than once"),
            Self::Outsiders { count } => write!(f, "{count} MRI(s) that aren't lab accounts"),
            Self::NotInChats { count } => write!(f, "{count} MRI(s) not in the account's chats"),
            Self::LogUnavailable => f.write_str("fence.log can't be written"),
        }
    }
}

/// A denied read by `POST`: the entry's kind, how many MRIs it named and the reason. It never holds the MRIs.
#[derive(Debug, thiserror::Error)]
#[error("fence denied the read by POST {kind} naming {names} MRI(s): {reason}")]
pub struct PostReadDenied {
    kind: &'static str,
    names: usize,
    reason: PostReadDenyReason,
}

impl PostReadDenied {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        self.kind
    }

    #[must_use]
    pub fn reason(&self) -> PostReadDenyReason {
        self.reason
    }
}

/// The people of the account's chats as its store holds them (the members its own thread reads gave): who a read by
/// `POST` of the personal build may name besides the account itself ([`Fence::check_post_read_among`]). The engine
/// replaces it from its store; a lab fence never reads it. Shared between the engine and the transport.
#[derive(Debug, Default)]
pub struct ChatPeople(Mutex<BTreeSet<Mri>>);

impl ChatPeople {
    /// No one.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes `people` the whole set.
    pub fn replace(&self, people: impl IntoIterator<Item = Mri>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = people.into_iter().collect();
    }

    /// Whether `mri` is one of them, exactly.
    #[must_use]
    pub fn contains(&self, mri: &Mri) -> bool {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).contains(mri)
    }

    /// How many there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// Whether there is no one.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Fence {
    /// Decides one read by `POST` as [`Fence::check_post_read`] does in a lab build (`people` unread). The personal
    /// fence permits it when it names at least one MRI, none twice, and each the account itself or one of `people`
    /// (the members of the account's chats as its store holds them), never a service's `28:` id
    /// (`docs/personal-build.md`, "Its reads"); logged as every decision.
    ///
    /// # Errors
    /// As [`Fence::check_post_read`]; for the personal fence [`PostReadDenyReason::NotInChats`] in place of
    /// [`PostReadDenyReason::Outsiders`].
    pub fn check_post_read_among(
        &self,
        kind: &'static str,
        names: &[Mri],
        people: &ChatPeople,
    ) -> Result<PostReadPermit, PostReadDenied> {
        let verdict = if self.is_personal() {
            self.judge_post_read_personal(names, people)
        } else {
            self.judge_post_read(names)
        };
        self.post_read_decided(kind, names, verdict)
    }

    /// The personal fence's verdict on a read by `POST` naming `names` (see [`Fence::check_post_read_among`]).
    fn judge_post_read_personal(&self, names: &[Mri], people: &ChatPeople) -> Option<PostReadDenyReason> {
        if names.is_empty() {
            return Some(PostReadDenyReason::NoNames);
        }
        if names.iter().collect::<BTreeSet<_>>().len() != names.len() {
            return Some(PostReadDenyReason::Duplicate);
        }
        let count = names
            .iter()
            .filter(|mri| {
                let known = self.own_account() == Some(*mri) || people.contains(mri);
                mri.as_str().starts_with("28:") || !known
            })
            .count();
        (count > 0).then_some(PostReadDenyReason::NotInChats { count })
    }

    /// Decides one read by `POST` of the entry `kind` naming `names`, in that order: a permit for exactly them when
    /// they are at least one MRI, none twice, and every one a lab account of the roster
    /// ([`Fence::lab_accounts_only`]); otherwise denied. The decision is appended to `fence.log` before the permit is
    /// made; an allow that can't be logged is denied.
    ///
    /// # Errors
    /// [`PostReadDenied`] for an empty list, a duplicate MRI, any MRI that isn't a lab account, or an allow that
    /// can't be logged.
    pub fn check_post_read(&self, kind: &'static str, names: &[Mri]) -> Result<PostReadPermit, PostReadDenied> {
        self.check_post_read_among(kind, names, &ChatPeople::new())
    }

    /// Logs `verdict` (`None` allows) on the read by `POST` `kind` naming `names`, and gives the permit when it allows
    /// and was logged.
    fn post_read_decided(
        &self,
        kind: &'static str,
        names: &[Mri],
        verdict: Option<PostReadDenyReason>,
    ) -> Result<PostReadPermit, PostReadDenied> {
        let logged = self.log().record_post_read(kind, names.len(), verdict);
        let deny = |reason| PostReadDenied {
            kind,
            names: names.len(),
            reason,
        };
        match (verdict, logged) {
            (None, Ok(())) => Ok(PostReadPermit {
                kind,
                names: names.to_vec(),
            }),
            (None, Err(_)) => Err(deny(PostReadDenyReason::LogUnavailable)),
            (Some(reason), _) => Err(deny(reason)),
        }
    }

    /// `None` to allow, or why not.
    fn judge_post_read(&self, names: &[Mri]) -> Option<PostReadDenyReason> {
        if names.is_empty() {
            return Some(PostReadDenyReason::NoNames);
        }
        if names.iter().collect::<BTreeSet<_>>().len() != names.len() {
            return Some(PostReadDenyReason::Duplicate);
        }
        if self.lab_accounts_only(names) {
            return None;
        }
        let count = names
            .iter()
            .filter(|mri| !self.lab_accounts_only(std::slice::from_ref(mri)))
            .count();
        Some(PostReadDenyReason::Outsiders { count })
    }
}
