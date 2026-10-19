//! `proto-lab export <capture> <fixture-name>`: a raw capture ([`crate::capture`]) made into a committed fixture.
//!
//! The capture's events are sanitized as one capture ([`crate::sanitize`]), and the result goes to
//! `tests/fixtures/<fixture-name>`: JSONL by default (`.jsonl` is added to a name without an extension), or one
//! pretty-printed JSON array for a name ending in `.json`. The fixture guard scans the text before anything is written:
//! if it finds anything, nothing is written (an existing fixture of that name stays as it was) and the export fails,
//! naming the lines and what was found, never the values. The file is written next to its final name and renamed into
//! place, then scanned once more where it lies.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use fixture_guard::{Guard, Kind};
use serde_json::Value;

use crate::capture::EVENTS_FILE;
use crate::sanitize::{Identities, sanitize_events};

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
    Pattern(#[from] regex::Error),
    #[error("the fixture guard refused the export, nothing was written: {}", findings(.0))]
    Refused(Vec<(usize, Kind)>),
    #[error("no {FIXTURES_DIR} in {} or above it; pass --fixtures-dir", .0.display())]
    NoFixturesDir(PathBuf),
}

fn findings(found: &[(usize, Kind)]) -> String {
    let lines: Vec<String> = found
        .iter()
        .map(|(line, kind)| format!("line {line}: {kind:?}"))
        .collect();
    lines.join(", ")
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
/// written beside `path` and renamed into place, then scanned again where it lies (and removed if that finds
/// anything).
///
/// # Errors
/// [`ExportError::Refused`] listing what the guard found, with nothing written; [`ExportError::Io`] when the file
/// can't be written.
pub fn write_fixture(path: &Path, text: &str) -> Result<(), ExportError> {
    let guard = Guard::new()?;
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
    fs::write(&staging, text).map_err(io_at(&staging))?;
    if let Err(error) = fs::rename(&staging, path) {
        let _ = fs::remove_file(&staging);
        return Err(io_at(path)(error));
    }
    let found = guard.scan_paths(&[path]).map_err(io_at(path))?;
    if !found.is_empty() {
        let _ = fs::remove_file(path);
        return Err(ExportError::Refused(
            found.into_iter().map(|finding| (finding.line, finding.kind)).collect(),
        ));
    }
    Ok(())
}

/// Exports a capture: reads it, sanitizes it as one capture, and writes it as fixture `name` under `fixtures_dir`
/// through [`write_fixture`].
///
/// # Errors
/// As [`fixture_path`], [`read_capture`] and [`write_fixture`].
pub fn export(
    capture: &Path,
    name: &str,
    fixtures_dir: &Path,
    identities: &Identities,
) -> Result<Exported, ExportError> {
    let (path, layout) = fixture_path(fixtures_dir, name)?;
    let events = read_capture(capture)?;
    let sanitized = sanitize_events(&events, identities)?;
    write_fixture(&path, &render(&sanitized, layout))?;
    Ok(Exported {
        path,
        events: sanitized.len(),
    })
}

/// The repository's `tests/fixtures`: in `start` or the nearest directory above it that has a `Cargo.toml` and
/// `tests/fixtures`.
///
/// # Errors
/// [`ExportError::NoFixturesDir`] when there is none.
pub fn find_fixtures_dir(start: &Path) -> Result<PathBuf, ExportError> {
    start
        .ancestors()
        .find(|dir| dir.join("Cargo.toml").is_file() && dir.join(FIXTURES_DIR).is_dir())
        .map(|dir| dir.join(FIXTURES_DIR))
        .ok_or_else(|| ExportError::NoFixturesDir(start.to_owned()))
}
