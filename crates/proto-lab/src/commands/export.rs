//! `proto-lab export <capture> <name> [--strict]`: a raw capture ([`crate::capture`]) made into a sanitized
//! **reference copy** under the lab's data dir (`reference/<name>.jsonl`), outside any repository.
//!
//! Captures never enter the repository, not even sanitized: the sanitizer is defence in depth, not a proof. A committed
//! fixture is written by hand, synthetic-from-capture: an agent reads the reference copy locally and writes a fixture
//! of the same shape with made-up values, which declares itself synthetic (`"_synthetic"`, checked by the fixture
//! guard on everything committed). The reference copy is only that local file, so what the checks below find in it is
//! reported, not refused ([`Checks::Warn`]): the copy is written whole and the export prints what they found (kinds,
//! placeholders, event and line numbers, never a value). With `--strict` ([`Checks::Strict`]) any finding refuses the
//! export instead, and a refusal writes nothing.
//!
//! The capture's events are sanitized as one capture ([`crate::sanitize`]), and the result goes to
//! `<reference dir>/<name>`: JSONL by default (`.jsonl` is added to a name without an extension), or one
//! pretty-printed JSON array for a name ending in `.json`. A destination inside a git work tree (a directory at or above
//! it holds `.git`, as written or with symlinks resolved, `..` resolved too) is always refused before anything is read,
//! as is one that is a symlink or has a `..` after a directory that doesn't exist yet. The checks:
//! - where the sanitizer left a word of a name inside a host (a name word that is also a label of a real host other
//!   than a public protocol host, such as a URL's authority or a `Host` header's value: replacing it would destroy the
//!   host, so it stays; a dotted name or a file name is no host) or replaced a meeting passcode inside one
//!   ([`Warnings::name_in_host`], [`Warnings::passcode_in_host`]);
//! - every name the sanitizer found in the capture, looked for in the text ([`check_names`], at every layer of
//!   encoding): a word of one left outside any host, one inside a host, a line too deeply encoded to check
//!   ([`Warnings::names`]);
//! - the fixture guard's scan of the text ([`Warnings::guard`]).
//!
//! The file is written next to its final name (`0600`, in directories created `0700`) and only then renamed into
//! place; under `--strict` it is scanned once more as it lies on disk first, so a refusal at either scan leaves an
//! existing copy as it was.

use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, OpenOptions, Permissions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};

use fixture_guard::{Guard, Kind};
use serde_json::Value;

use crate::capture::EVENTS_FILE;
use crate::sanitize::{CHECK_ROUNDS, Identities, NameFinding, Sanitized, check_names, sanitize_capture};

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
    #[error("{} is a symlink; a reference copy is written as a file of its own, never through a link", .0.display())]
    SymlinkDestination(PathBuf),
    #[error(
        "{} has a '..' after a directory that doesn't exist, so where it leads can't be checked for a git work tree",
        .0.display()
    )]
    Unresolvable(PathBuf),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}:{line}: not a JSON event", path.display())]
    Malformed { path: PathBuf, line: usize },
    #[error("the sanitizer's patterns don't compile: {0}")]
    Pattern(#[source] regex::Error),
    #[error("the fixture guard's patterns don't compile: {0}")]
    GuardPattern(#[source] regex::Error),
    #[error("the fixture guard refused the export (--strict), nothing was written: {}", findings(.0))]
    Refused(Vec<(usize, Kind)>),
    #[error(
        "a name found in the capture is still in the sanitized text (--strict), nothing was written: {}",
        name_findings(.0)
    )]
    NameLeft(Vec<NameFinding>),
    #[error(
        "a name or passcode found in the capture is also a label of a host, where a name was left or a passcode \
         destroyed the host (--strict), nothing was written: {}",
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
    /// What the checks found (always empty under [`Checks::Strict`], which refuses instead).
    pub warnings: Warnings,
}

/// What an export does with what its checks find.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Checks {
    /// Report it ([`Exported::warnings`]) and write the reference copy all the same.
    #[default]
    Warn,
    /// Refuse the export on any of it, writing nothing (`--strict`).
    Strict,
}

/// What the checks found in a reference copy written under [`Checks::Warn`]: kinds, placeholders and positions, never
/// a value from the capture.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Warnings {
    /// The events (from 1) where a word of a name was left inside a host other than a public protocol host.
    pub name_in_host: Vec<usize>,
    /// The events (from 1) where a meeting passcode was replaced inside a host other than a public protocol host.
    pub passcode_in_host: Vec<usize>,
    /// What [`check_names`] found in the written text.
    pub names: Vec<NameFinding>,
    /// What the fixture guard found in the written text: line (from 1) and kind.
    pub guard: Vec<(usize, Kind)>,
}

/// How many positions a summary line lists before it only counts the rest.
const LISTED: usize = 10;

impl Warnings {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.name_in_host.is_empty()
            && self.passcode_in_host.is_empty()
            && self.names.is_empty()
            && self.guard.is_empty()
    }

    /// One line per kind of finding: counts, kinds, placeholders and positions, never a value.
    #[must_use]
    pub fn summary(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if !self.name_in_host.is_empty() {
            lines.push(format!(
                "a word of a name was left inside a host (not replaced, so the host stays intact) in {} event(s): {}",
                self.name_in_host.len(),
                positions(&self.name_in_host)
            ));
        }
        if !self.passcode_in_host.is_empty() {
            lines.push(format!(
                "a meeting passcode was replaced inside a host (that host is no longer intact) in {} event(s): {}",
                self.passcode_in_host.len(),
                positions(&self.passcode_in_host)
            ));
        }
        lines.extend(self.name_summary());
        if !self.guard.is_empty() {
            let kinds = counted(self.guard.iter().map(|(_, kind)| format!("{kind:?}")));
            let found: Vec<usize> = self.guard.iter().map(|(line, _)| *line).collect();
            lines.push(format!(
                "the fixture guard found {} thing(s) ({kinds}) on line(s) {}",
                self.guard.len(),
                positions(&found)
            ));
        }
        lines
    }

    fn name_summary(&self) -> Vec<String> {
        let mut outside: (Vec<usize>, Vec<String>) = (Vec::new(), Vec::new());
        let mut inside: (Vec<usize>, Vec<String>) = (Vec::new(), Vec::new());
        let mut too_deep = Vec::new();
        for finding in &self.names {
            let (list, line, label) = match finding {
                NameFinding::Name { line, label, .. } => (&mut outside, line, label),
                NameFinding::InHost { line, label, .. } => (&mut inside, line, label),
                NameFinding::TooDeep { line } => {
                    too_deep.push(*line);
                    continue;
                }
            };
            list.0.push(*line);
            list.1.push(label.clone());
        }
        let mut lines = Vec::new();
        for ((found, labels), place) in [(outside, "outside any host"), (inside, "inside a host")] {
            if !found.is_empty() {
                lines.push(format!(
                    "the name check found a word of a name {place} {} time(s) ({}) on line(s) {}",
                    found.len(),
                    counted(labels.into_iter()),
                    positions(&found)
                ));
            }
        }
        if !too_deep.is_empty() {
            lines.push(format!(
                "{} line(s) are still encoded after {CHECK_ROUNDS} layers and weren't checked for names: {}",
                too_deep.len(),
                positions(&too_deep)
            ));
        }
        lines
    }
}

/// `items` sorted and without repeats, the first [`LISTED`] listed and the rest counted.
fn positions(items: &[usize]) -> String {
    let mut items = items.to_vec();
    items.sort_unstable();
    items.dedup();
    let listed: Vec<String> = items.iter().take(LISTED).map(ToString::to_string).collect();
    let rest = items.len().saturating_sub(LISTED);
    if rest == 0 {
        listed.join(", ")
    } else {
        format!("{}, and {rest} more", listed.join(", "))
    }
}

/// Each distinct item with how often it shows: `Jwt x2, Email x1`.
fn counted(items: impl Iterator<Item = String>) -> String {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for item in items {
        *counts.entry(item).or_default() += 1;
    }
    let counts: Vec<String> = counts
        .into_iter()
        .map(|(item, count)| format!("{item} x{count}"))
        .collect();
    counts.join(", ")
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

/// Refuses `path` when it is inside a git work tree, and returns the path to write to otherwise: `path` made absolute
/// with every `..` resolved ([`normalize`]). Refused: a destination that is a symlink, and one where a directory at or
/// above it holds a `.git` entry (a directory or a worktree's `.git` file), checked on the normalized path as it reads
/// and on the deepest existing directory that will hold the file, with its symlinks resolved.
///
/// # Errors
/// [`ExportError::InsideRepository`], [`ExportError::SymlinkDestination`], [`ExportError::Unresolvable`];
/// [`ExportError::Io`] when the current directory can't be read for a relative path.
pub fn refuse_inside_repository(path: &Path) -> Result<PathBuf, ExportError> {
    let normal = normalize(path)?;
    if fs::symlink_metadata(&normal).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(ExportError::SymlinkDestination(path.to_owned()));
    }
    // the directory the file goes in, resolved as far as it exists: the rest is created under it
    let holder = normal.parent().unwrap_or(&normal);
    let resolved = holder.ancestors().find_map(|existing| fs::canonicalize(existing).ok());
    let inside = [Some(normal.as_path()), resolved.as_deref()]
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
        Ok(normal)
    }
}

/// `path` made absolute, with every `..` resolved the way the kernel does: against the real directory it follows,
/// symlinks resolved. A `..` after a directory that doesn't exist (yet) is refused, since where it leads would only be
/// known once that directory is created.
///
/// # Errors
/// [`ExportError::Unresolvable`]; [`ExportError::Io`] when the current directory can't be read for a relative path.
fn normalize(path: &Path) -> Result<PathBuf, ExportError> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir().map_err(io_at(path))?.join(path)
    };
    let mut normal = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let real = fs::canonicalize(&normal).map_err(|_| ExportError::Unresolvable(path.to_owned()))?;
                normal = real.parent().map_or_else(|| real.clone(), Path::to_owned);
            }
            other => normal.push(other),
        }
    }
    Ok(normal)
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

/// Writes `text` to `path` only if it is outside every git work tree ([`refuse_inside_repository`], whose normalized
/// path is the one written): to a staging file beside `path` (`0600`, missing directories created `0700`), then renamed
/// into place, and hands back what the fixture guard found in `text`. Under [`Checks::Warn`] the file is written
/// whatever the guard finds; under [`Checks::Strict`] a finding refuses the write before anything touches the disk,
/// and the staging file is scanned again as it lies there before the rename. Whatever fails, the staging file is
/// removed and an existing file at `path` stays as it was. The staging file is always created new: anything already
/// at its path (a leftover, a symlink) refuses the write and is left alone.
///
/// # Errors
/// As [`refuse_inside_repository`], and under [`Checks::Strict`] [`ExportError::Refused`] (listing what the guard
/// found), with nothing written; [`ExportError::Io`] when the file can't be written, its staging path taken included;
/// [`ExportError::GuardPattern`] when the guard can't be built.
pub fn write_reference(path: &Path, text: &str, checks: Checks) -> Result<Vec<(usize, Kind)>, ExportError> {
    let path = &refuse_inside_repository(path)?;
    let guard = Guard::new().map_err(ExportError::GuardPattern)?;
    let found = guard.scan_text(text);
    let rescan = match checks {
        Checks::Strict if !found.is_empty() => return Err(ExportError::Refused(found)),
        Checks::Strict => Some(&guard),
        Checks::Warn => None,
    };
    let parent = path
        .parent()
        .ok_or_else(|| ExportError::BadName(path.display().to_string()))?;
    private_dir(parent)?;
    let file_name = path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
    let staging = parent.join(format!(".{file_name}.export-{}", std::process::id()));
    // what was at the staging path already isn't ours: refused, and left as it was
    let file = create_staging(&staging)?;
    let placed = stage(file, rescan, &staging, text).and_then(|()| fs::rename(&staging, path).map_err(io_at(path)));
    if placed.is_err() {
        let _ = fs::remove_file(&staging);
    }
    placed.map(|()| found)
}

/// Creates the staging file `0600`, only if nothing is at `staging`: `create_new` (`O_CREAT | O_EXCL`) refuses an
/// existing file and a symlink, a dangling one too, rather than write through it.
fn create_staging(staging: &Path) -> Result<fs::File, ExportError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(staging)
        .map_err(io_at(staging))
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

/// Writes the staging file `file` (at `staging`, made `0600`) and, given a guard, scans it as it lies on disk.
fn stage(mut file: fs::File, guard: Option<&Guard>, staging: &Path, text: &str) -> Result<(), ExportError> {
    // the umask may have taken bits off the mode it was created with
    file.set_permissions(Permissions::from_mode(0o600))
        .map_err(io_at(staging))?;
    file.write_all(text.as_bytes()).map_err(io_at(staging))?;
    drop(file);
    let Some(guard) = guard else {
        return Ok(());
    };
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
/// [`write_reference`], and makes `reference_dir` `0700`. Under [`Checks::Warn`] what the checks find comes back in
/// [`Exported::warnings`]; under [`Checks::Strict`] any of it refuses the export.
///
/// # Errors
/// As [`reference_path`], [`refuse_inside_repository`], [`read_capture`] and [`write_reference`]; under
/// [`Checks::Strict`], [`ExportError::NameInHost`] when a name was left in a host or a passcode took a host's place,
/// and [`ExportError::NameLeft`] when the name check finds anything, with nothing written.
pub fn export(
    capture: &Path,
    name: &str,
    reference_dir: &Path,
    identities: &Identities,
    checks: Checks,
) -> Result<Exported, ExportError> {
    export_with(capture, name, reference_dir, checks, |events| {
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
    checks: Checks,
    sanitize: impl FnOnce(&[Value]) -> Result<Sanitized, regex::Error>,
) -> Result<Exported, ExportError> {
    let (path, layout) = reference_path(reference_dir, name)?;
    let path = refuse_inside_repository(&path)?;
    let events = read_capture(capture)?;
    let sanitized = sanitize(&events).map_err(ExportError::Pattern)?;
    let text = render(&sanitized.events, layout);
    let mut warnings = Warnings {
        name_in_host: sanitized.name_in_host,
        passcode_in_host: sanitized.passcode_in_host,
        names: check_names(&sanitized.names, &text),
        guard: Vec::new(),
    };
    if checks == Checks::Strict {
        refuse_on(&warnings)?;
    }
    warnings.guard = write_reference(&path, &text, checks)?;
    // `write_reference` made what was missing `0700`; the reference dir is too, even when it was there before
    fs::set_permissions(reference_dir, Permissions::from_mode(0o700)).map_err(io_at(reference_dir))?;
    Ok(Exported {
        path,
        events: sanitized.events.len(),
        warnings,
    })
}

/// The refusal `--strict` makes of what the sanitizer and the name check found, if anything.
fn refuse_on(warnings: &Warnings) -> Result<(), ExportError> {
    let mut in_host = [warnings.name_in_host.as_slice(), warnings.passcode_in_host.as_slice()].concat();
    in_host.sort_unstable();
    in_host.dedup();
    if !in_host.is_empty() {
        return Err(ExportError::NameInHost(in_host));
    }
    if !warnings.names.is_empty() {
        return Err(ExportError::NameLeft(warnings.names.clone()));
    }
    Ok(())
}
