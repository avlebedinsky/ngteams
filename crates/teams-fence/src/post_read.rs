//! The fence's decision on a native read by `POST` of `teams-proto`'s sealed `PostRead` list: [`Fence::check_post_read`]
//! gives a [`PostReadPermit`] for one entry and its exact list of MRIs, or [`PostReadDenied`]. The lab's native client
//! sends its reads by `POST` as plain requests (they reach nobody); the personal build sends them under a permit, so
//! each is logged by its entry's kind (`docs/personal-build.md`).
//!
//! A read reaches nobody, so every fence, a lab one too, permits every one, whoever it names. It fetches nothing.
//!
//! Every decision is appended to `fence.log` (the entry's kind, how many MRIs it names, the verdict and the reason;
//! never the MRIs), and an allow that can't be logged is denied. What the request looks like on the wire (its
//! service, path, query, headers and body) is `teams-proto`'s to check against the entry's captured shape: this
//! crate knows MRIs, not HTTP.

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
    /// It would be allowed, but its decision couldn't be written to `fence.log`.
    LogUnavailable,
}

impl fmt::Display for PostReadDenyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
    /// Decides one read by `POST` of the entry `kind` naming `names`, in that order: a read reaches nobody, so it is a
    /// permit for exactly them, from a lab fence and a personal one alike. The decision is appended to `fence.log`
    /// before the permit is made; an allow that can't be logged is denied.
    ///
    /// # Errors
    /// [`PostReadDenied`] for an allow that can't be logged.
    pub fn check_post_read(&self, kind: &'static str, names: &[Mri]) -> Result<PostReadPermit, PostReadDenied> {
        match self.log().record_post_read(kind, names.len(), None) {
            Ok(()) => Ok(PostReadPermit {
                kind,
                names: names.to_vec(),
            }),
            Err(_) => Err(PostReadDenied {
                kind,
                names: names.len(),
                reason: PostReadDenyReason::LogUnavailable,
            }),
        }
    }
}
