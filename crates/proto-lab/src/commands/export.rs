//! `proto-lab export <capture> <fixture-name>`: a raw capture ([`crate::capture`]) made into a committed fixture.
//!
//! The capture's events are sanitized as one capture ([`crate::sanitize`]), and the result goes to
//! `tests/fixtures/<fixture-name>`: JSONL by default (`.jsonl` is added to a name without an extension), or one
//! pretty-printed JSON array for a name ending in `.json`. If sanitizing had to replace a name inside a host (a name
//! word that is also a label of a host other than a public protocol host, `https://<name:1>.example.test/`), the export
//! fails naming the events, since that host would be destroyed in the fixture. Before anything is written, the text is
//! checked for every
//! name the sanitizer found in the capture ([`check_names`], at every layer of encoding, apart from the sanitizer's own
//! replacing): if a word of one is still there, nothing is written and the export fails, naming the lines and the
//! names' placeholders, never the names. Then the fixture guard scans the text:
//! if it finds anything, nothing is written (an existing fixture of that name stays as it was) and the export fails,
//! naming the lines and what was found, never the values. The file is written next to its final name, scanned once more
//! as it lies on disk, and only then renamed into place, so a refusal at either scan leaves an existing fixture as it
//! was.
//!
//! By default fixtures go to the `tests/fixtures` of the cargo workspace around the current directory (the nearest
//! `Cargo.toml` above it with a `[workspace]` table), never to a member crate's own `tests/fixtures`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use fixture_guard::{Guard, Kind};
use serde_json::Value;

use crate::capture::EVENTS_FILE;
use crate::sanitize::{Identities, NameFinding, Sanitized, check_names, sanitize_capture};

/// Where fixtures live, relative to the repository root.
pub const FIXTURES_DIR: &str = "tests/fixtures";

/// Why an export failed. None of these carries a value from the capture.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error(
        "not a fixture name (a relative path of letters, digits, '.', '-' and '_', no segment starting with '.', \
         ending in .jsonl, .json or no extension): {0:?}"
    )]
    BadName(String),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}:{line}: not a JSON event", path.display())]
    Malformed { path: PathBuf, line: usize },
    #[error("the sanitizer's patterns don't compile: {0}")]
    Pattern(#[source] regex::Error),
    #[error("the fixture guard's patterns don't compile: {0}")]
    GuardPattern(#[source] regex::Error),
    #[error("the fixture guard refused the export, nothing was written: {}", findings(.0))]
    Refused(Vec<(usize, Kind)>),
    #[error("a name found in the capture is still in the sanitized text, nothing was written: {}", name_findings(.0))]
    NameLeft(Vec<NameFinding>),
    #[error(
        "a name or passcode found in the capture is also a label of a host, which replacing it would destroy, nothing was written: {}",
        events(.0)
    )]
    NameInHost(Vec<usize>),
    #[error("no cargo workspace with a {FIXTURES_DIR} in {} or above it; pass --fixtures-dir", .0.display())]
    NoFixturesDir(PathBuf),
}

fn findings(found: &[(usize, Kind)]) -> String {
    let lines: Vec<String> = found
        .iter()
        .map(|(line, kind)| format!("line {line}: {kind:?}"))
        .collect();
    lines.join(", ")
}

fn events(found: &[usize]) -> String {
    let events: Vec<String> = found.iter().map(|event| format!("event {event}")).collect();
    events.join(", ")
}

fn name_findings(found: &[NameFinding]) -> String {
    let found: Vec<String> = found.iter().map(ToString::to_string).collect();
    found.join(", ")
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> ExportError + '_ {
    move |source| ExportError::Io {
        path: path.to_owned(),
        source,
    }
}

/// A fixture written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exported {
    pub path: PathBuf,
    pub events: usize,
}

/// How a fixture is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// One event per line.
    Jsonl,
    /// One pretty-printed array.
    Json,
}

/// Where a fixture named `name` goes under `fixtures_dir`, and its layout.
///
/// # Errors
/// [`ExportError::BadName`] for an absolute path, an empty segment, a segment starting with `.` (`..` included), a
/// character outside letters, digits, `.`, `-` and `_`, or an extension other than `.jsonl` and `.json`.
pub fn fixture_path(fixtures_dir: &Path, name: &str) -> Result<(PathBuf, Layout), ExportError> {
    let bad = || ExportError::BadName(name.to_owned());
    let segments: Vec<&str> = name.split('/').collect();
    let valid = segments.iter().all(|segment| {
        !segment.is_empty()
            && !segment.starts_with('.')
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    });
    let file = segments.last().filter(|_| valid).ok_or_else(bad)?;
    let (relative, layout) = match file.rsplit_once('.') {
        None => (format!("{name}.jsonl"), Layout::Jsonl),
        Some((_, "jsonl")) => (name.to_owned(), Layout::Jsonl),
        Some((_, "json")) => (name.to_owned(), Layout::Json),
        Some(_) => return Err(bad()),
    };
    Ok((fixtures_dir.join(relative), layout))
}

/// The events of a capture: its directory (holding `events.jsonl`) or the file itself. Blank lines are skipped.
///
/// # Errors
/// When the file can't be read, or a line isn't JSON.
pub fn read_capture(capture: &Path) -> Result<Vec<Value>, ExportError> {
    let file = if capture.is_dir() {
        capture.join(EVENTS_FILE)
    } else {
        capture.to_owned()
    };
    let text = fs::read_to_string(&file).map_err(io_at(&file))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|_| ExportError::Malformed {
                path: file.clone(),
                line: index + 1,
            })
        })
        .collect()
}

/// The fixture's text.
#[must_use]
pub fn render(events: &[Value], layout: Layout) -> String {
    match layout {
        Layout::Jsonl => {
            let mut text = String::new();
            for event in events {
                text.push_str(&event.to_string());
                text.push('\n');
            }
            text
        }
        Layout::Json => {
            let array = Value::Array(events.to_vec());
            // a `Value` always serializes
            format!("{}\n", serde_json::to_string_pretty(&array).unwrap_or_default())
        }
    }
}

/// Writes `text` to `path` only if the fixture guard finds nothing in it: scanned before anything touches the disk,
/// written to a staging file beside `path`, scanned again as it lies there, and only then renamed into place. Whatever
/// fails, the staging file is removed and an existing fixture at `path` stays as it was.
///
/// # Errors
/// [`ExportError::Refused`] listing what the guard found, with nothing written; [`ExportError::Io`] when the file
/// can't be written; [`ExportError::GuardPattern`] when the guard can't be built.
pub fn write_fixture(path: &Path, text: &str) -> Result<(), ExportError> {
    let guard = Guard::new().map_err(ExportError::GuardPattern)?;
    let found = guard.scan_text(text);
    if !found.is_empty() {
        return Err(ExportError::Refused(found));
    }
    let parent = path
        .parent()
        .ok_or_else(|| ExportError::BadName(path.display().to_string()))?;
    fs::create_dir_all(parent).map_err(io_at(parent))?;
    let file_name = path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
    let staging = parent.join(format!(".{file_name}.export-{}", std::process::id()));
    let placed = stage(&guard, &staging, text).and_then(|()| fs::rename(&staging, path).map_err(io_at(path)));
    if placed.is_err() {
        let _ = fs::remove_file(&staging);
    }
    placed
}

/// Writes the staging file and scans it as it lies on disk.
fn stage(guard: &Guard, staging: &Path, text: &str) -> Result<(), ExportError> {
    fs::write(staging, text).map_err(io_at(staging))?;
    let found = guard.scan_paths(&[staging]).map_err(io_at(staging))?;
    if found.is_empty() {
        Ok(())
    } else {
        Err(ExportError::Refused(
            found.into_iter().map(|finding| (finding.line, finding.kind)).collect(),
        ))
    }
}

/// Exports a capture: reads it, sanitizes it as one capture, checks the names found in it ([`check_names`]), and
/// writes it as fixture `name` under `fixtures_dir` through [`write_fixture`].
///
/// # Errors
/// As [`fixture_path`], [`read_capture`] and [`write_fixture`]; [`ExportError::NameInHost`] when a name took a host's
/// place, and [`ExportError::NameLeft`] when a name is left, with nothing written.
pub fn export(
    capture: &Path,
    name: &str,
    fixtures_dir: &Path,
    identities: &Identities,
) -> Result<Exported, ExportError> {
    export_with(capture, name, fixtures_dir, |events| {
        sanitize_capture(events, identities)
    })
}

/// [`export`] with the sanitizing step given: the seam its tests use to hand the checks a sanitizer that misses
/// something. Every check after that step runs as in [`export`].
///
/// # Errors
/// As [`export`].
pub fn export_with(
    capture: &Path,
    name: &str,
    fixtures_dir: &Path,
    sanitize: impl FnOnce(&[Value]) -> Result<Sanitized, regex::Error>,
) -> Result<Exported, ExportError> {
    let (path, layout) = fixture_path(fixtures_dir, name)?;
    let events = read_capture(capture)?;
    let sanitized = sanitize(&events).map_err(ExportError::Pattern)?;
    if !sanitized.name_in_host.is_empty() {
        return Err(ExportError::NameInHost(sanitized.name_in_host));
    }
    let text = render(&sanitized.events, layout);
    let left = check_names(&sanitized.names, &text);
    if !left.is_empty() {
        return Err(ExportError::NameLeft(left));
    }
    write_fixture(&path, &text)?;
    Ok(Exported {
        path,
        events: sanitized.events.len(),
    })
}

/// The repository's `tests/fixtures`: that of the cargo workspace around `start`, the nearest directory at or above it
/// whose `Cargo.toml` has a `[workspace]` table. A member crate's own `tests/fixtures` is never picked.
///
/// # Errors
/// [`ExportError::NoFixturesDir`] when there is no workspace above `start`, or it has no `tests/fixtures`.
pub fn find_fixtures_dir(start: &Path) -> Result<PathBuf, ExportError> {
    start
        .ancestors()
        .find(|dir| is_workspace_root(dir))
        .map(|dir| dir.join(FIXTURES_DIR))
        .filter(|fixtures| fixtures.is_dir())
        .ok_or_else(|| ExportError::NoFixturesDir(start.to_owned()))
}

/// Whether `dir` has a `Cargo.toml` with a `[workspace]` table (or a `[workspace.*]` one, which implies it).
fn is_workspace_root(dir: &Path) -> bool {
    fs::read_to_string(dir.join("Cargo.toml")).is_ok_and(|manifest| {
        manifest.lines().any(|line| {
            let line = line.trim();
            line == "[workspace]" || line.starts_with("[workspace.")
        })
    })
}
