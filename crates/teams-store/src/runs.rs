//! Which runs are still open. Each run holds an exclusive `flock` on its own file, `<store>-runs/<run_id>.lock`, for as
//! long as its [`Store`](crate::Store) is open; the kernel drops the lock when the process dies, however it dies. A run
//! whose lock can be taken has ended: only its rows can be adopted by another run ([`Store::resend`](crate::Store::resend)).
//!
//! `flock` locks belong to an open file, not a process, so two stores open in one process see each other as open too.
//! A run takes its lock, and lock files of ended runs are removed, only inside the `BEGIN IMMEDIATE` transaction that
//! adds the run: the store's write lock serializes them with each other and with a resend's check, so no check finds a
//! file removed under a run that is taking it.

use std::fs::{DirBuilder, File, OpenOptions, TryLockError};
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::StoreError;

const FILE_MODE: u32 = 0o600;
const DIR_MODE: u32 = 0o700;
const SUFFIX: &str = ".lock";

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

fn lock_file(dir: &Path, run_id: i64) -> PathBuf {
    dir.join(format!("{run_id}{SUFFIX}"))
}

/// Creates the directory (`0700`) and takes the run's lock, held until the returned file is dropped. Called inside
/// the transaction that adds the run.
pub(crate) fn hold(dir: &Path, run_id: i64) -> Result<File, StoreError> {
    DirBuilder::new()
        .recursive(true)
        .mode(DIR_MODE)
        .create(dir)
        .map_err(failed(dir))?;
    let path = lock_file(dir, run_id);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(FILE_MODE)
        .open(&path)
        .map_err(failed(&path))?;
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
