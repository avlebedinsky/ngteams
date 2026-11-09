//! The store file is the account's chat history: private to the user from the moment it exists. SQLite gives its
//! `-wal` and `-shm` files the main file's mode on Unix. On Windows they inherit the directory's private ACL; both
//! the directory and the database are restricted before SQLite opens them. Existing side files are tightened too.
//!
//! None of these files is ever opened here once it exists: SQLite's locks are POSIX locks, which belong to the process,
//! and closing any descriptor of a file drops all of them, so a check that opened and closed the file would take the
//! locks of another store open on it in this process (its read lock and its hold on the `-shm`). Modes are read and
//! set by path, and the file is created only when missing, under the open lock (`runs::open_lock`), before SQLite has
//! it open anywhere.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::StoreError;
use teams_platform::private::{self, Open};

/// The files SQLite keeps next to the database.
pub(crate) const SIDE_FILES: [&str; 3] = ["-wal", "-shm", "-journal"];

fn failed(path: &Path) -> impl FnOnce(std::io::Error) -> StoreError {
    let path = path.to_owned();
    move |source| StoreError::File { path, source }
}

/// Creates `dir` (and the directories above it) with mode `0700` when missing.
pub(crate) fn private_dir(dir: &Path) -> Result<(), StoreError> {
    if dir.as_os_str().is_empty() {
        return Ok(());
    }
    private::create_dir_all(dir).map_err(failed(dir))?;
    private::restrict_dir(dir).map_err(failed(dir))
}

/// Creates the store's directory (`0700`) and file (`0600`) when missing, and tightens an existing file (or side file)
/// that others can read or write. Called under the open lock.
pub(crate) fn prepare(path: &Path) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        private_dir(parent)?;
    }
    let created = private::open(path, Open::NewReadWrite);
    match created {
        // a new file: no connection has it open yet, so closing it here drops nobody's locks
        Ok(file) => drop(file),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => tighten(path).map_err(failed(path))?,
        Err(error) => return Err(failed(path)(error)),
    }
    for suffix in SIDE_FILES {
        let side = side_file(path, suffix);
        match tighten(&side) {
            Err(error) if error.kind() != ErrorKind::NotFound => return Err(failed(&side)(error)),
            _ => {}
        }
    }
    Ok(())
}

/// Takes away the group's and others' access to the file at `path`, by path (see the module docs). Anything but a
/// regular file there (a directory, or a link to one) is refused with its mode left as it was: `0600` on a directory
/// would take away its owner's traversal, and SQLite can't use it as a store anyway.
fn tighten(path: &Path) -> std::io::Result<()> {
    private::restrict_file(path)
}

pub(crate) fn side_file(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invalid_file_name_is_refused_before_sqlite_opens_it() {
        let path = Path::new("invalid\0store.sqlite");
        let error = prepare(path).unwrap_err();
        assert!(matches!(error, StoreError::File { path: failed, .. } if failed == path));
    }
}
