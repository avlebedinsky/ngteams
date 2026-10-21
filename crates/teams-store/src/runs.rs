//! Which runs are still open. Each run holds an exclusive `flock` on its own file, `<store>-runs/<run_id>.lock`, for as
//! long as its [`Store`](crate::Store) is open; the kernel drops the lock when the process dies, however it dies. A run
//! whose lock can be taken has ended: only its rows can be adopted by another run ([`Store::resend`](crate::Store::resend)).
//!
//! `flock` locks belong to an open file, not a process, so two stores open in one process see each other as open too.
//! A run takes its lock, and lock files of ended runs are removed, only inside the `BEGIN IMMEDIATE` transaction that
//! adds the run: the store's write lock serializes them with each other and with a resend's check, so no check finds a
//! file removed under a run that is taking it.
//!
//! The same directory holds the open lock, `open.lock` (never swept: it names no run). An open holds it, blocking, from
//! before the store file is made until the file is in WAL mode. SQLite gives that step no way to wait: turning a file
//! into WAL rewrites its header in a transaction that begins as a read and then upgrades to a write, and SQLite never
//! calls the busy handler for an upgrade from a read (it could deadlock), so of two connections making the same new
//! file WAL at once, one gets `SQLITE_BUSY` at once, busy timeout or not. A `BEGIN IMMEDIATE` can't stand in, as the
//! journal mode can't change inside a transaction. The lock is held only while the open checks the file's modes and
//! SQLite opens it and switches it to WAL, each step bounded (SQLite's own waits by the busy timeout); everything
//! after, the migrations included, waits on SQLite's write lock instead.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::ErrorKind;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::StoreError;

const FILE_MODE: u32 = 0o600;
const SUFFIX: &str = ".lock";
/// The open lock's file name: no run id, so [`sweep`] leaves it.
const OPEN_LOCK: &str = "open.lock";

/// The directory of a store's run locks, next to it: `<store>-runs`.
pub(crate) fn dir(store: &Path) -> PathBuf {
    let mut name = store.as_os_str().to_owned();
    name.push("-runs");
    PathBuf::from(name)
}

fn failed(path: &Path) -> impl FnOnce(std::io::Error) -> StoreError {
    let path = path.to_owned();
    move |source| StoreError::File { path, source }
}

/// The name of a run's lock file in the directory.
fn lock_name(run_id: i64) -> String {
    format!("{run_id}{SUFFIX}")
}

fn lock_file(dir: &Path, run_id: i64) -> PathBuf {
    dir.join(lock_name(run_id))
}

/// Creates the directory (`0700`) when missing and opens (creating, `0600`) the lock file `name` in it, unlocked.
fn lock_file_in(dir: &Path, name: &str) -> Result<(File, PathBuf), StoreError> {
    crate::file::private_dir(dir)?;
    let path = dir.join(name);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(FILE_MODE)
        .open(&path)
        .map_err(failed(&path))?;
    Ok((file, path))
}

/// Takes the open lock (see the module docs), waiting for another open to finish its first steps; held until the
/// returned file is dropped.
pub(crate) fn open_lock(dir: &Path) -> Result<File, StoreError> {
    let (file, path) = lock_file_in(dir, OPEN_LOCK)?;
    file.lock().map_err(failed(&path))?;
    Ok(file)
}

/// Creates the directory (`0700`) and takes the run's lock, held until the returned file is dropped. Called inside
/// the transaction that adds the run.
pub(crate) fn hold(dir: &Path, run_id: i64) -> Result<File, StoreError> {
    let (file, path) = lock_file_in(dir, &lock_name(run_id))?;
    // only a check holds it, and only for a moment: no open run has this id
    file.lock().map_err(failed(&path))?;
    Ok(file)
}

/// Whether the run's store is still open somewhere. No lock file means the run has ended (its file was removed by a
/// later open) or never took one (it failed to open, and has no rows).
pub(crate) fn is_open(dir: &Path, run_id: i64) -> Result<bool, StoreError> {
    let path = lock_file(dir, run_id);
    match OpenOptions::new().read(true).write(true).open(&path) {
        Ok(file) => held(&file, &path),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(failed(&path)(error)),
    }
}

/// Whether another open file holds the lock; a lock taken here to find out is dropped with `file`.
fn held(file: &File, path: &Path) -> Result<bool, StoreError> {
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(failed(path)(error)),
    }
}

/// Removes the lock files of runs that have ended. Called inside the transaction that adds a run.
pub(crate) fn sweep(dir: &Path) -> Result<(), StoreError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(failed(dir)(error)),
    };
    for entry in entries {
        let path = entry.map_err(failed(dir))?.path();
        let run_id = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(SUFFIX))
            .and_then(|id| id.parse::<i64>().ok());
        let Some(run_id) = run_id else { continue };
        if !is_open(dir, run_id)? {
            match std::fs::remove_file(&path) {
                Err(error) if error.kind() != ErrorKind::NotFound => return Err(failed(&path)(error)),
                _ => {}
            }
        }
    }
    Ok(())
}
