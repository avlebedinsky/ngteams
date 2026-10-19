//! The fixture guard: scans files meant for the repository (sanitized fixtures, docs) for what must never be committed,
//! such as JWTs, `Bearer` and skypetoken values, OAuth tokens, email addresses and real MRIs.
//!
//! Sanitized placeholders (`<user:a>`, `<mri:b>`, `<guid:1>`, `<token:1>`) and prose that only names these things pass.
//! A finding reports where and what, never the matched value, so the report itself cannot leak it.
//!
//! The repo-wide check is `tests/fixture_guard.rs`; `proto-lab export` (M0 Task 9) runs the same scan before it
//! writes a fixture.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use regex::Regex;

/// What a finding looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A JSON Web Token (`eyJ…`.`…`.`…`); Entra access and id tokens and skypetokens are JWTs.
    Jwt,
    /// An `Authorization: Bearer` value.
    Bearer,
    /// A skypetoken value, in a header (`X-Skypetoken`, `skypetoken=`) or a JSON field.
    Skypetoken,
    /// An `access_token`, `refresh_token` or `id_token` value.
    OauthToken,
    /// An email address outside the reserved `example.com`/`.org`/`.net` domains.
    Email,
    /// A real user or bot MRI (`8:orgid:<guid>`, `8:live:<name>`, `28:<guid>`, …).
    Mri,
}

/// One hit: a file, a 1-based line, and what was found there. It never carries the matched text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub path: PathBuf,
    pub line: usize,
    pub kind: Kind,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {:?}", self.path.display(), self.line, self.kind)
    }
}

const GUID: &str = "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}";
/// A token-shaped value: 20 or more URL-safe base64/JWT characters. Placeholders start with `<`, so they never match.
const VALUE: &str = r"[A-Za-z0-9._\-]{20,}";
/// Reserved documentation domains (RFC 2606), which a fixture or doc may use freely.
const RESERVED_DOMAINS: [&str; 3] = ["example.com", "example.org", "example.net"];

/// The compiled patterns. Build one with [`Guard::new`] and reuse it.
#[derive(Debug)]
pub struct Guard {
    rules: Vec<(Kind, Regex)>,
}

impl Guard {
    /// Compiles the patterns.
    ///
    /// # Errors
    /// A pattern fails to compile; that is a bug in this crate, caught by its tests.
    pub fn new() -> Result<Self, regex::Error> {
        let patterns = [
            (
                Kind::Jwt,
                r"eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]*".to_owned(),
            ),
            (Kind::Bearer, r"(?i)\bbearer\s+[A-Za-z0-9\-._~+/]{20,}".to_owned()),
            (
                Kind::Skypetoken,
                format!(r#"(?i)skypetoken["']?\s*[:=]\s*["']?{VALUE}"#),
            ),
            (
                Kind::OauthToken,
                format!(r#"(?i)\b(?:access|refresh|id)_token["']?\s*[:=]\s*["']?{VALUE}"#),
            ),
            (
                Kind::Email,
                r"[A-Za-z0-9._%+\-]+@([A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)*\.[A-Za-z]{2,})\b".to_owned(),
            ),
            (
                Kind::Mri,
                format!(r"\b(?:8:(?:orgid|teamsvisitor|acs|gcch|dod):{GUID}|8:live:[A-Za-z0-9._\-]+|28:{GUID})"),
            ),
        ];
        let rules = patterns
            .into_iter()
            .map(|(kind, pattern)| Regex::new(&pattern).map(|regex| (kind, regex)))
            .collect::<Result<_, _>>()?;
        Ok(Self { rules })
    }

    /// Every hit in `text`, as (1-based line, kind), at most one per kind and line, in line order.
    #[must_use]
    pub fn scan_text(&self, text: &str) -> Vec<(usize, Kind)> {
        let mut found = Vec::new();
        for (index, line) in text.lines().enumerate() {
            for (kind, regex) in &self.rules {
                if matches_line(*kind, regex, line) {
                    found.push((index + 1, *kind));
                }
            }
        }
        found
    }

    /// Scans each root, a file or a directory walked recursively, and returns every hit in path order. Symlinks are
    /// skipped; non-UTF-8 bytes are read lossily, so binary files are scanned too.
    ///
    /// # Errors
    /// A root is missing, or a file or directory under it can't be read.
    pub fn scan_paths(&self, roots: &[&Path]) -> io::Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for root in roots {
            self.scan_entry(root, &mut findings)?;
        }
        Ok(findings)
    }

    fn scan_entry(&self, path: &Path, findings: &mut Vec<Finding>) -> io::Result<()> {
        let kind = fs::symlink_metadata(path)?.file_type();
        if kind.is_dir() {
            let mut entries = fs::read_dir(path)?
                .map(|entry| entry.map(|e| e.path()))
                .collect::<io::Result<Vec<_>>>()?;
            entries.sort();
            for entry in entries {
                self.scan_entry(&entry, findings)?;
            }
        } else if kind.is_file() {
            let text = String::from_utf8_lossy(&fs::read(path)?).into_owned();
            findings.extend(self.scan_text(&text).into_iter().map(|(line, kind)| Finding {
                path: path.to_owned(),
                line,
                kind,
            }));
        }
        Ok(())
    }
}

fn matches_line(kind: Kind, regex: &Regex, line: &str) -> bool {
    if kind != Kind::Email {
        return regex.is_match(line);
    }
    regex
        .captures_iter(line)
        .any(|captures| captures.get(1).is_some_and(|domain| !is_reserved(domain.as_str())))
}

fn is_reserved(domain: &str) -> bool {
    let domain = domain.to_ascii_lowercase();
    RESERVED_DOMAINS
        .iter()
        .any(|reserved| domain == *reserved || domain.ends_with(&format!(".{reserved}")))
}
