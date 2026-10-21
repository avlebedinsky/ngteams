//! The fence's decision on a native read by `POST` (M2 Task 11): [`Fence::check_post_read`] gives a
//! [`PostReadPermit`] for one entry of `teams-proto`'s sealed `PostRead` list and its exact list of MRIs, or
//! [`PostReadDenied`].
//!
//! A read by `POST` changes nothing, but it names people, and its answer tells about them: the user decided on
//! 2026-10-21 that the native client may send one only when every MRI it names is a lab account at the moment the
//! fence decides (CLAUDE.md, "The native client's fourth list"). So the lab fence denies a list that is empty, that
//! names an MRI twice, or that names anyone but the lab accounts of its roster, an approved service participant
//! included ([`Fence::lab_accounts_only`]). It fetches nothing.
//!
//! Every decision is appended to `fence.log` (the entry's kind, how many MRIs it names, the verdict and the reason;
//! never the MRIs), and an allow that can't be logged is denied. What the request looks like on the wire (its
//! service, path, query, headers and body) is `teams-proto`'s to check against the entry's captured shape: this
//! crate knows MRIs, not HTTP.

use std::collections::BTreeSet;
use std::fmt;

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
    /// It would be allowed, but its decision couldn't be written to `fence.log`.
    LogUnavailable,
}

impl fmt::Display for PostReadDenyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoNames => f.write_str("it names nobody"),
            Self::Duplicate => f.write_str("it names an MRI more than once"),
            Self::Outsiders { count } => write!(f, "{count} MRI(s) that aren't lab accounts"),
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

impl Fence {
    /// Decides one read by `POST` of the entry `kind` naming `names`, in that order: a permit for exactly them when
    /// they are at least one MRI, none twice, and every one a lab account of the roster
    /// ([`Fence::lab_accounts_only`]); otherwise denied. The decision is appended to `fence.log` before the permit is
    /// made; an allow that can't be logged is denied.
    ///
    /// # Errors
    /// [`PostReadDenied`] for an empty list, a duplicate MRI, any MRI that isn't a lab account, or an allow that
    /// can't be logged.
    pub fn check_post_read(&self, kind: &'static str, names: &[Mri]) -> Result<PostReadPermit, PostReadDenied> {
        let verdict = self.judge_post_read(names);
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
