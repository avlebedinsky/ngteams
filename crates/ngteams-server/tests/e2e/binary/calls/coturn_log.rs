//! What a coturn wrote in its own log (`-v`, one file: `NGTS_QA_COTURN_LOG`) since a test
//! began: each request it processed for a user, and how much it relayed for each. Coturn's log is text, its only
//! record of who it relayed for.

use std::path::PathBuf;

/// The log of the run's coturn.
pub const COTURN_LOG: &str = "NGTS_QA_COTURN_LOG";

/// One line of the log about a user: a request processed, or a session's traffic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// `incoming packet <METHOD> processed, success` or `…, error <code>`.
    Request {
        user: String,
        method: String,
        error: Option<u16>,
    },
    /// `usage:` or `peer usage:`: packets and bytes received and sent, on the client's side or the peer's.
    Usage {
        user: String,
        peer: bool,
        received_bytes: u64,
        sent_bytes: u64,
    },
}

impl Entry {
    pub fn user(&self) -> &str {
        match self {
            Self::Request { user, .. } | Self::Usage { user, .. } => user,
        }
    }
}

/// A coturn's log from one moment on.
pub struct CoturnLog {
    path: PathBuf,
    from: usize,
}

/// The text between `open` and the next `>` after it.
fn between<'a>(line: &'a str, open: &str) -> Option<&'a str> {
    let start = line.find(open)? + open.len();
    let end = line[start..].find('>')?;
    Some(&line[start..start + end])
}

/// The number right after `key=`.
fn count(line: &str, key: &str) -> u64 {
    line.split(", ")
        .find_map(|part| {
            let rest = part.trim().strip_prefix(key)?;
            let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            rest[..digits].parse().ok()
        })
        .unwrap_or(0)
}

fn parse(line: &str) -> Option<Entry> {
    if let Some(at) = line.find("incoming packet ") {
        let user = between(line, "user <")?.to_owned();
        let rest = &line[at + "incoming packet ".len()..];
        let method = rest.split_whitespace().next()?.to_owned();
        let error = rest
            .split("processed, error ")
            .nth(1)
            .and_then(|code| code.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok());
        return Some(Entry::Request { user, method, error });
    }
    if line.contains("usage: realm=") {
        let user = between(line, "username=<")
            .or_else(|| between(line, "user=<"))?
            .to_owned();
        return Some(Entry::Usage {
            user,
            peer: line.contains("peer usage:"),
            received_bytes: count(line, "rb="),
            sent_bytes: count(line, "sb="),
        });
    }
    None
}

impl CoturnLog {
    /// The log named by `variable`, from its present end on; `None` when the run names none.
    pub fn from_now(variable: &str) -> Option<Self> {
        let path = PathBuf::from(std::env::var_os(variable)?);
        let from = std::fs::read(&path).map_or(0, |text| text.len());
        Some(Self { path, from })
    }

    /// Everything about a user since the start.
    pub fn entries(&self) -> Vec<Entry> {
        let text = std::fs::read(&self.path).unwrap();
        String::from_utf8_lossy(&text[self.from.min(text.len())..])
            .lines()
            .filter_map(parse)
            .collect()
    }

    /// The entries whose user is a REST credential of account `oid` (`<expiry>:<oid>`).
    pub fn of(&self, oid: &str) -> Vec<Entry> {
        let suffix = format!(":{oid}");
        self.entries()
            .into_iter()
            .filter(|entry| entry.user().ends_with(&suffix))
            .collect()
    }

    /// Allocations coturn made for `oid`.
    pub fn allocations(&self, oid: &str) -> usize {
        self.of(oid)
            .iter()
            .filter(|entry| matches!(entry, Entry::Request { method, error: None, .. } if method == "ALLOCATE"))
            .count()
    }

    /// The most coturn said it relayed for `oid` in one usage line, from the client or toward it.
    pub fn relayed(&self, oid: &str) -> u64 {
        self.of(oid)
            .iter()
            .filter_map(|entry| match entry {
                Entry::Usage {
                    received_bytes,
                    sent_bytes,
                    ..
                } => Some((*received_bytes).min(*sent_bytes)),
                Entry::Request { .. } => None,
            })
            .max()
            .unwrap_or(0)
    }

    /// Every user coturn processed a request of: the running test's own (the rigs take turns while the log is named,
    /// `support::take_turn`).
    pub fn users(&self) -> Vec<String> {
        let mut users: Vec<String> = self.entries().iter().map(|entry| entry.user().to_owned()).collect();
        users.sort();
        users.dedup();
        users.retain(|user| !user.is_empty());
        users
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coturns_lines_read_as_entries() {
        let request = "2026-11-15T14:25:01.844+0000 INFO session 001000000000000001: realm <rtcmedia> user \
                       <1794752000:abc>: incoming packet ALLOCATE processed, success";
        assert_eq!(
            parse(request),
            Some(Entry::Request {
                user: "1794752000:abc".into(),
                method: "ALLOCATE".into(),
                error: None
            })
        );
        let refused = "session 001: realm <rtcmedia> user <1:abc>: incoming packet REFRESH processed, error 401: \
                       Unauthorized";
        assert!(matches!(parse(refused), Some(Entry::Request { error: Some(401), .. })));
        let usage = "INFO usage: realm=<rtcmedia>, username=<1:abc>, rp=10, rb=1200, sp=9, sb=1100 (session 0090)";
        assert_eq!(
            parse(usage),
            Some(Entry::Usage {
                user: "1:abc".into(),
                peer: false,
                received_bytes: 1200,
                sent_bytes: 1100
            })
        );
    }
}
