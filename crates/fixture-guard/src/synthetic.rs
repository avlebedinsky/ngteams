//! The synthetic marker: every committed fixture under the repository's top-level `tests/fixtures/` declares that it
//! is synthetic, written by hand with made-up values (synthetic-from-capture: the shape read from a local reference
//! copy, never the capture's values). Captures never enter the repository, not even sanitized.
//!
//! A fixture is marked when it carries a non-empty text under [`MARKER`]:
//! - a `.json` file: a top-level object with a `"_synthetic"` string field (`{"_synthetic": "made up: …", …}`);
//! - a `.jsonl` file: its first non-blank line is an object with that field, as a header line of its own
//!   (`{"_synthetic": "…"}`) or on every line;
//! - any other file: UTF-8 text whose first line holds `_synthetic:` followed by the text, in that format's comment
//!   (`# _synthetic: …`, `<!-- _synthetic: … -->`).
//!
//! `.gitkeep` is not a fixture. A symlink, a non-UTF-8 file, or anything else is unmarked. Crate-local test pages and
//! inputs (`crates/*/tests/fixtures/`) are test inputs, not captures, and are not checked.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The key, or header tag, a synthetic fixture carries.
pub const MARKER: &str = "_synthetic";

/// The file a directory may hold without being a fixture.
const KEEP: &str = ".gitkeep";

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

/// A first line holding `_synthetic:` and some text after it, a comment's closing `-->` or `*/` aside.
fn has_header(line: &str) -> bool {
    line.split_once(&format!("{MARKER}:")).is_some_and(|(_, note)| {
        let note = note.trim().trim_end_matches("-->").trim_end_matches("*/");
        note.chars().any(char::is_alphanumeric)
    })
}

/// Every file under `root` (a directory walked recursively, or one file) that is not marked synthetic
/// ([`is_marked`]), in path order. `.gitkeep` is skipped; a symlink is reported as unmarked.
///
/// # Errors
/// `root` is missing, or a file or directory under it can't be read; the error message names that path.
pub fn unmarked(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    walk(root, &mut found)?;
    Ok(found)
}

fn walk(path: &Path, found: &mut Vec<PathBuf>) -> io::Result<()> {
    let kind = fs::symlink_metadata(path).map_err(|e| at(path, &e))?.file_type();
    if kind.is_dir() {
        let mut entries = fs::read_dir(path)
            .map_err(|e| at(path, &e))?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<io::Result<Vec<_>>>()
            .map_err(|e| at(path, &e))?;
        entries.sort();
        for entry in entries {
            walk(&entry, found)?;
        }
        return Ok(());
    }
    let fixture = if !kind.is_file() {
        Some(false)
    } else if path.file_name().is_some_and(|name| name == KEEP) {
        None
    } else {
        Some(is_marked(path, &fs::read(path).map_err(|e| at(path, &e))?))
    };
    // `Some(marked)` for a fixture, `None` for the `.gitkeep`
    if fixture == Some(false) {
        found.push(path.to_owned());
    }
    Ok(())
}

/// `error` with `path` in its message.
fn at(path: &Path, error: &io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}
