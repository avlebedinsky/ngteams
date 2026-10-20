//! `proto-lab export <capture> <name>`: a raw capture ([`crate::capture`]) made into a sanitized **reference copy**
//! under the lab's data dir (`reference/<name>.jsonl`), outside any repository.
//!
//! Captures never enter the repository, not even sanitized: the sanitizer is defence in depth, not a proof. A committed
//! fixture is written by hand, synthetic-from-capture: an agent reads the reference copy locally and writes a fixture
//! of the same shape with made-up values, which declares itself synthetic (`"_synthetic"`, checked by the fixture
//! guard). The reference copy is what it reads, so it still goes through every check, and a refusal still writes
//! nothing.
//!
//! The capture's events are sanitized as one capture ([`crate::sanitize`]), and the result goes to
//! `<reference dir>/<name>`: JSONL by default (`.jsonl` is added to a name without an extension), or one
//! pretty-printed JSON array for a name ending in `.json`. A destination inside a git work tree (a directory at or above
//! it holds `.git`, as written or with symlinks resolved) is refused before anything is read. If sanitizing had to
//! replace a name inside a host (a name word that is also a label of a host other than a public protocol host,
//! `https://<name:1>.example.test/`), the export fails naming the events, since that host would be destroyed. Before
//! anything is written, the text is checked for every name the sanitizer found in the capture ([`check_names`], at
//! every layer of encoding, apart from the sanitizer's own replacing): if a word of one is still there, nothing is
//! written and the export fails, naming the lines and the names' placeholders, never the names. Then the fixture guard
//! scans the text: if it finds anything, nothing is written (an existing copy of that name stays as it was) and the
//! export fails, naming the lines and what was found, never the values. The file is written next to its final name
//! (`0600`, in directories created `0700`), scanned once more as it lies on disk, and only then renamed into place, so
//! a refusal at either scan leaves an existing copy as it was.

use std::fs::{self, DirBuilder, OpenOptions, Permissions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use fixture_guard::{Guard, Kind};
use serde_json::Value;

use crate::capture::EVENTS_FILE;
use crate::sanitize::{Identities, NameFinding, Sanitized, check_names, sanitize_capture};

/// Where reference copies live, relative to the lab's data dir.
pub const REFERENCE_DIR: &str = "reference";

/// The reference dir under the lab's data dir `data_dir`.
#[must_use]
pub fn reference_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(REFERENCE_DIR)
}

/// Why an export failed. None of these carries a value from the capture.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error(
        "not a reference name (a relative path of letters, digits, '.', '-' and '_', no segment starting with '.', \
         ending in .jsonl, .json or no extension): {0:?}"
    )]
    BadName(String),
    #[error(
        "{} is inside a git work tree; captures never enter a repository, not even sanitized, so the reference copy \
         goes under the lab's data dir",
        .0.display()
    )]
    InsideRepository(PathBuf),
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

/// A reference copy written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exported {
    pub path: PathBuf,
    pub events: usize,
}

/// How a reference copy is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// One event per line.
    Jsonl,
    /// One pretty-printed array.
    Json,
}

/// Where a reference copy named `name` goes under `reference_dir`, and its layout.
///
/// # Errors
/// [`ExportError::BadName`] for an absolute path, an empty segment, a segment starting with `.` (`..` included), a
/// character outside letters, digits, `.`, `-` and `_`, or an extension other than `.jsonl` and `.json`.
pub fn reference_path(reference_dir: &Path, name: &str) -> Result<(PathBuf, Layout), ExportError> {
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
    Ok((reference_dir.join(relative), layout))
}

/// Refuses `path` when it is inside a git work tree: a directory at or above it (as written, made absolute, and with
/// symlinks resolved on the part that exists) holds a `.git` entry, a directory or a worktree's `.git` file.
///
/// # Errors
/// [`ExportError::InsideRepository`]; [`ExportError::Io`] when the current directory can't be read for a relative
/// path.
pub fn refuse_inside_repository(path: &Path) -> Result<(), ExportError> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir().map_err(io_at(path))?.join(path)
    };
    let resolved = absolute
        .ancestors()
        .find_map(|existing| fs::canonicalize(existing).ok());
    let inside = [Some(absolute.as_path()), resolved.as_deref()]
        .into_iter()
        .flatten()
        .any(|candidate| {
            candidate
                .ancestors()
                .any(|dir| fs::symlink_metadata(dir.join(".git")).is_ok())
        });
    if inside {
        Err(ExportError::InsideRepository(path.to_owned()))
    } else {
        Ok(())
    }
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

/// The reference copy's text.
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

/// Writes `text` to `path` only if it is outside every git work tree ([`refuse_inside_repository`]) and the fixture
/// guard finds nothing in it: scanned before anything touches the disk, written (`0600`, missing directories created
/// `0700`) to a staging file beside `path`, scanned again as it lies there, and only then renamed into place. Whatever
/// fails, the staging file is removed and an existing file at `path` stays as it was.
///
/// # Errors
/// [`ExportError::InsideRepository`] and [`ExportError::Refused`] (listing what the guard found), with nothing
/// written; [`ExportError::Io`] when the file can't be written; [`ExportError::GuardPattern`] when the guard can't be
/// built.
pub fn write_reference(path: &Path, text: &str) -> Result<(), ExportError> {
    refuse_inside_repository(path)?;
    let guard = Guard::new().map_err(ExportError::GuardPattern)?;
    let found = guard.scan_text(text);
    if !found.is_empty() {
        return Err(ExportError::Refused(found));
    }
    let parent = path
        .parent()
        .ok_or_else(|| ExportError::BadName(path.display().to_string()))?;
    private_dir(parent)?;
    let file_name = path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
    let staging = parent.join(format!(".{file_name}.export-{}", std::process::id()));
    let placed = stage(&guard, &staging, text).and_then(|()| fs::rename(&staging, path).map_err(io_at(path)));
    if placed.is_err() {
        let _ = fs::remove_file(&staging);
    }
    placed
}

/// Creates `dir` and its missing parents `0700`, and makes `dir` itself `0700` when it already existed.
fn private_dir(dir: &Path) -> Result<(), ExportError> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(io_at(dir))?;
    fs::set_permissions(dir, Permissions::from_mode(0o700)).map_err(io_at(dir))
}

/// Writes the staging file (`0600`) and scans it as it lies on disk.
fn stage(guard: &Guard, staging: &Path, text: &str) -> Result<(), ExportError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(staging)
        .map_err(io_at(staging))?;
    // a leftover staging file keeps its old mode through `open`
    file.set_permissions(Permissions::from_mode(0o600))
        .map_err(io_at(staging))?;
    file.write_all(text.as_bytes()).map_err(io_at(staging))?;
    drop(file);
    let found = guard.scan_paths(&[staging]).map_err(io_at(staging))?;
    if found.is_empty() {
        Ok(())
    } else {
        Err(ExportError::Refused(
            found.into_iter().map(|finding| (finding.line, finding.kind)).collect(),
        ))
    }
}

/// Exports a capture: refuses a destination inside a git work tree, reads the capture, sanitizes it as one capture,
/// checks the names found in it ([`check_names`]), writes it as reference copy `name` under `reference_dir` through
/// [`write_reference`], and makes `reference_dir` `0700`.
///
/// # Errors
/// As [`reference_path`], [`refuse_inside_repository`], [`read_capture`] and [`write_reference`];
/// [`ExportError::NameInHost`] when a name took a host's place, and [`ExportError::NameLeft`] when a name is left, with
/// nothing written.
pub fn export(
    capture: &Path,
    name: &str,
    reference_dir: &Path,
    identities: &Identities,
) -> Result<Exported, ExportError> {
    export_with(capture, name, reference_dir, |events| {
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
    reference_dir: &Path,
    sanitize: impl FnOnce(&[Value]) -> Result<Sanitized, regex::Error>,
) -> Result<Exported, ExportError> {
    let (path, layout) = reference_path(reference_dir, name)?;
    refuse_inside_repository(&path)?;
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
    write_reference(&path, &text)?;
    // `write_reference` made what was missing `0700`; the reference dir is too, even when it was there before
    fs::set_permissions(reference_dir, Permissions::from_mode(0o700)).map_err(io_at(reference_dir))?;
    Ok(Exported {
        path,
        events: sanitized.events.len(),
    })
}
