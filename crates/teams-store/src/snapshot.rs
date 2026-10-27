//! A consistent copy of a store that another process may be writing (the personal build's read-only mode works on a
//! snapshot of the user's store, never on the store itself): [`snapshot`].
//!
//! The source is opened read-only and copied with `VACUUM INTO`, one read transaction, so the copy is a state the
//! store was in. Nothing is written to the source's file or its WAL: with a WAL beside it (an engine has it open) it is
//! read through its WAL as a reader (SQLite's reader marks in the shared-memory file are all it touches); without one
//! (no engine has it open, so the file alone holds everything) it is opened `immutable`, which takes no lock and makes
//! no `-wal` or `-shm` file. The copy is checked (`quick_check`) and, when that fails (an engine opened the store in
//! between), made once more as a reader.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::StoreError;

/// Copies the store at `source` to `dest` (a new file: one there already is an error), `0600`. See the module docs.
///
/// # Errors
/// [`StoreError::File`] when `dest` exists or its permissions can't be set; [`StoreError::Sqlite`] when the source
/// can't be read or the copy can't be made or checked.
pub fn snapshot(source: &Path, dest: &Path) -> Result<(), StoreError> {
    let file_error = |source| StoreError::File {
        path: dest.to_path_buf(),
        source,
    };
    if dest.exists() {
        return Err(file_error(std::io::Error::from(std::io::ErrorKind::AlreadyExists)));
    }
    let has_wal = wal_of(source).exists();
    copy(source, dest, !has_wal)?;
    if !has_wal && !intact(dest)? {
        // an engine opened the store while it was read as immutable: read it again as a reader, through its WAL
        fs::remove_file(dest).map_err(file_error)?;
        copy(source, dest, false)?;
    }
    if !intact(dest)? {
        return Err(StoreError::Snapshot {
            path: dest.to_path_buf(),
        });
    }
    fs::set_permissions(dest, fs::Permissions::from_mode(0o600)).map_err(file_error)
}

/// `source`'s WAL file.
fn wal_of(source: &Path) -> PathBuf {
    let mut wal = source.as_os_str().to_owned();
    wal.push("-wal");
    PathBuf::from(wal)
}

/// One `VACUUM INTO` from `source`, opened read-only (and `immutable` when asked) to `dest`.
fn copy(source: &Path, dest: &Path, immutable: bool) -> Result<(), StoreError> {
    let mut uri = format!("file:{}?mode=ro", uri_path(source));
    if immutable {
        uri.push_str("&immutable=1");
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(&uri, flags)?;
    conn.busy_timeout(crate::BUSY_TIMEOUT)?;
    let dest = dest.to_str().ok_or(rusqlite::Error::InvalidPath(dest.to_path_buf()))?;
    conn.execute("VACUUM INTO ?1", [dest])?;
    Ok(())
}

/// Whether the copy at `path` passes SQLite's `quick_check`.
fn intact(path: &Path) -> Result<bool, StoreError> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    let answer: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    Ok(answer == "ok")
}

/// `path` as a URI's path: the characters a URI gives a meaning (`%`, `?`, `#`) escaped.
fn uri_path(path: &Path) -> String {
    let mut text = String::new();
    for character in path.to_string_lossy().chars() {
        match character {
            '%' => text.push_str("%25"),
            '?' => text.push_str("%3f"),
            '#' => text.push_str("%23"),
            other => text.push(other),
        }
    }
    text
}
