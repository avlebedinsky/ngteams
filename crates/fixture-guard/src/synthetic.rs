//! The synthetic marker: every committed protocol fixture declares that it is synthetic, written by hand with made-up
//! values (synthetic-from-capture: the shape read from a local reference copy, never the capture's values). Captures
//! never enter the repository, not even sanitized.
//!
//! A fixture is marked when it carries a non-empty text under [`MARKER`]:
//! - a `.json` file: a top-level object with a `"_synthetic"` string field (`{"_synthetic": "made up: …", …}`);
//! - a `.jsonl` file: its first non-blank line is an object with that field, as a header line of its own
//!   (`{"_synthetic": "…"}`) or on every line;
//! - any other file: UTF-8 text whose first line starts with `_synthetic:` followed by the text, bare or after that
//!   format's comment opener (`# _synthetic: …`, `<!-- _synthetic: … -->`, `// _synthetic: …`).
//!
//! An empty `.gitkeep` is not a fixture; a `.gitkeep` with content is checked like any other file. A symlink, a
//! non-UTF-8 file, or anything else is unmarked.
//!
//! Where fixtures live ([`fixture_dirs`]): every `tests/fixtures/` directory of the workspace, the top-level one, a
//! crate's and a spike's (`spikes/*/tests/fixtures/`) alike. The only exceptions are [`TEST_INPUTS`], directories of
//! test inputs that are not Teams payloads and so are not shaped like a capture.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The key, or header tag, a synthetic fixture carries.
pub const MARKER: &str = "_synthetic";

/// The file a directory may hold, empty, without being a fixture.
const KEEP: &str = ".gitkeep";

/// Directories [`fixture_dirs`] doesn't walk into: build output and package caches (dot-directories aside).
const NOT_WALKED: [&str; 2] = ["target", "node_modules"];

/// Directories under a `tests/fixtures/`, relative to the workspace root, that hold test inputs rather than protocol
/// fixtures: they are not shaped like anything Teams sends, so they carry no marker and [`unmarked_in_workspace`] skips
/// them. The fixture guard's scan still covers them. A directory is listed here only when nothing in it comes from, or
/// imitates, a capture.
pub const TEST_INPUTS: &[&str] = &[
    // the pages and workers proto-lab's capture tests load
    "crates/proto-lab/tests/fixtures/pages",
    // the miniature desktop bundle proto-lab's bundle import tests unpack
    "crates/proto-lab/tests/fixtures/mini-bundle",
    // the made-up MP4 (FFmpeg's test pattern and a tone) the inline video player plays in teams-media's tests and the
    // UI tour
    "crates/teams-media/tests/fixtures/media",
];

/// Comment openers a text fixture's `_synthetic:` header may follow.
const COMMENT_OPENERS: [&str; 5] = ["<!--", "/*", "//", "#", "--"];

/// Whether `bytes`, the content of the fixture at `path` (its extension picks the format), declares itself synthetic.
#[must_use]
pub fn is_marked(path: &Path, bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => serde_json::from_str::<Value>(text).is_ok_and(|value| has_marker(&value)),
        Some("jsonl") => text
            .lines()
            .find(|line| !line.trim().is_empty())
            .and_then(|line| serde_json::from_str::<Value>(line).ok())
            .is_some_and(|value| has_marker(&value)),
        _ => text.lines().next().is_some_and(has_header),
    }
}

/// A top-level object with non-empty text under [`MARKER`].
fn has_marker(value: &Value) -> bool {
    value
        .get(MARKER)
        .and_then(Value::as_str)
        .is_some_and(|note| note.chars().any(char::is_alphanumeric))
}

/// A first line that starts with `_synthetic:`, bare or after one comment opener, and has some text after it, a
/// comment's closing `-->` or `*/` aside. `_synthetic:` anywhere else in the line (inside a value, in a longer word)
/// is not a header.
fn has_header(line: &str) -> bool {
    let line = line.trim_start();
    let line = COMMENT_OPENERS
        .iter()
        .find_map(|opener| line.strip_prefix(opener))
        .unwrap_or(line)
        .trim_start();
    line.strip_prefix(MARKER)
        .and_then(|rest| rest.strip_prefix(':'))
        .is_some_and(|note| {
            let note = note.trim().trim_end_matches("-->").trim_end_matches("*/");
            note.chars().any(char::is_alphanumeric)
        })
}

/// Every file under `root` (a directory walked recursively, or one file) that is not marked synthetic
/// ([`is_marked`]), in path order. An empty `.gitkeep` is skipped; a symlink is reported as unmarked.
///
/// # Errors
/// `root` is missing, or a file or directory under it can't be read; the error message names that path.
pub fn unmarked(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    walk(root, &[], &mut found)?;
    Ok(found)
}

/// Every `tests/fixtures` directory under `workspace`, in path order: the top-level one, a crate's, a spike's, at any
/// depth. A fixture directory is not searched for more of them; dot-directories, `target` and `node_modules` are not
/// walked, and symlinks are not followed.
///
/// # Errors
/// `workspace` or a directory under it can't be read; the error message names that path.
pub fn fixture_dirs(workspace: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    find_fixture_dirs(workspace, &mut found)?;
    Ok(found)
}

/// Every file in `workspace`'s fixture directories ([`fixture_dirs`]) that is not marked synthetic ([`unmarked`]),
/// apart from those under [`TEST_INPUTS`], in path order.
///
/// # Errors
/// As [`fixture_dirs`] and [`unmarked`].
pub fn unmarked_in_workspace(workspace: &Path) -> io::Result<Vec<PathBuf>> {
    let skip: Vec<PathBuf> = TEST_INPUTS.iter().map(|dir| workspace.join(dir)).collect();
    let mut found = Vec::new();
    for dir in fixture_dirs(workspace)? {
        walk(&dir, &skip, &mut found)?;
    }
    Ok(found)
}

fn find_fixture_dirs(dir: &Path, found: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in sorted_entries(dir)? {
        if !walked(&entry)? {
            continue;
        }
        if entry.ends_with("tests/fixtures") {
            found.push(entry);
        } else {
            find_fixture_dirs(&entry, found)?;
        }
    }
    Ok(())
}

/// Whether [`fixture_dirs`] walks into `entry`: a directory (not a symlink to one) that isn't hidden or
/// [`NOT_WALKED`].
fn walked(entry: &Path) -> io::Result<bool> {
    let named = entry
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| !name.starts_with('.') && !NOT_WALKED.contains(&name));
    Ok(named && fs::symlink_metadata(entry).map_err(|e| at(entry, &e))?.is_dir())
}

/// The entries of the directory `dir`, in path order.
fn sorted_entries(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut entries = fs::read_dir(dir)
        .map_err(|e| at(dir, &e))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<io::Result<Vec<_>>>()
        .map_err(|e| at(dir, &e))?;
    entries.sort();
    Ok(entries)
}

fn walk(path: &Path, skip: &[PathBuf], found: &mut Vec<PathBuf>) -> io::Result<()> {
    if skip.iter().any(|dir| dir == path) {
        return Ok(());
    }
    let kind = fs::symlink_metadata(path).map_err(|e| at(path, &e))?.file_type();
    if kind.is_dir() {
        for entry in sorted_entries(path)? {
            walk(&entry, skip, found)?;
        }
        return Ok(());
    }
    let marked = kind.is_file() && {
        let bytes = fs::read(path).map_err(|e| at(path, &e))?;
        is_keep(path, &bytes) || is_marked(path, &bytes)
    };
    if !marked {
        found.push(path.to_owned());
    }
    Ok(())
}

/// An empty `.gitkeep`, the one file that isn't a fixture.
fn is_keep(path: &Path, bytes: &[u8]) -> bool {
    bytes.is_empty() && path.file_name().is_some_and(|name| name == KEEP)
}

/// `error` with `path` in its message.
fn at(path: &Path, error: &io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}
