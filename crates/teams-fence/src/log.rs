//! `fence.log`: one JSON line per decision.
//!
//! ```json
//! {"ts_ms":1760000000000,"verdict":"deny","kind":"message","scope":{"conversation":{"id":"19:…"}},"added":0,
//!  "reason":{"outsiders":{"count":1}},"message":"1 participant(s) outside the lab"}
//! ```
//!
//! An entry holds the write kind, the scope ids, how many participants the write adds and the reason: never a member's
//! identity, never a token. Allows are logged too (without a reason), so the log is the full audit of a run.

use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::decision::DenyReason;
use crate::target::{Scope, Target, WriteKind};

/// The outcome of one decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Allow,
    Deny(DenyReason),
}

#[derive(Serialize)]
struct Entry<'a> {
    ts_ms: u64,
    verdict: &'static str,
    kind: WriteKind,
    scope: &'a Scope,
    added: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<DenyReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

/// The append-only decision log.
pub struct FenceLog {
    sink: Mutex<Box<dyn Write + Send>>,
}

impl fmt::Debug for FenceLog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FenceLog")
    }
}

impl FenceLog {
    /// Opens `path` for appending, creating it readable by the owner only (0600).
    ///
    /// # Errors
    /// When the file can't be opened or created.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).mode(0o600).open(path)?;
        Ok(Self::from_writer(file))
    }

    /// Logs into any writer.
    #[must_use]
    pub fn from_writer(writer: impl Write + Send + 'static) -> Self {
        Self {
            sink: Mutex::new(Box::new(writer)),
        }
    }

    /// Appends one decision as a single line and flushes it.
    pub(crate) fn record(&self, target: &Target, verdict: Verdict) -> io::Result<()> {
        let reason = match verdict {
            Verdict::Allow => None,
            Verdict::Deny(reason) => Some(reason),
        };
        let entry = Entry {
            ts_ms: now_ms(),
            verdict: if reason.is_some() { "deny" } else { "allow" },
            kind: target.kind(),
            scope: target.scope(),
            added: target.added().len(),
            reason,
            message: reason.map(|reason| reason.to_string()),
        };
        let mut line = serde_json::to_vec(&entry)?;
        line.push(b'\n');
        let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        sink.write_all(&line)?;
        sink.flush()
    }
}

fn now_ms() -> u64 {
    let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX)
}
