//! `ngteams snapshot --to <dir>`: a copy of the user's personal files for the read-only mode to run on
//! (`crate::read_only`), so it never opens, locks or writes the user's own store, settings or `fence.log`.
//!
//! Copied, under `<dir>` laid out as [`layout`] names it: `accounts.toml` (and an earlier build's `account.toml`), the
//! settings, every account's region and every account's store, each store as a consistent copy read without writing
//! it ([`teams_store::snapshot::snapshot`]: `VACUUM INTO` from a read-only connection, the user's running `ngteams`
//! included). Left out: `fence.log`, the diagnostic logs, a store's run and engine locks and its picture cache
//! (`<store>-cache/`, `teams_store::PictureCache`): the read-only mode makes its own. Nothing of the source is
//! written; `<dir>` must be new (or empty) and lie apart from the user's own directories, and is made `0700`, its
//! files `0600`.

use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use teams_fence::apart::overlapping;
use teams_fence::personal::PersonalPaths;
use teams_store::StoreError;

/// Why the snapshot wasn't made.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("{} is, contains or lies inside your own Teams files ({}): a snapshot goes elsewhere", dest.display(), mine.display())]
    Overlaps { dest: PathBuf, mine: PathBuf },
    #[error("{} isn't empty: a snapshot goes into a new directory", .0.display())]
    NotEmpty(PathBuf),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The personal files laid out under a snapshot's root `dir`: `<dir>/config` and `<dir>/data`, as
/// `~/.config/ngteams/` and `~/.local/share/ngteams/` hold them.
#[must_use]
pub fn layout(dir: &Path) -> PersonalPaths {
    PersonalPaths::new(dir.join("config"), dir.join("data"))
}

/// Copies `source`'s files into a snapshot at `dir` (see the module docs) and gives the snapshot's paths.
///
/// # Errors
/// [`SnapshotError`]: `dir` overlaps `source`'s directories or isn't empty, a file can't be read or written, a store
/// can't be copied.
pub fn snapshot(source: &PersonalPaths, dir: &Path) -> Result<PersonalPaths, SnapshotError> {
    for mine in [source.config_dir(), source.data_dir()] {
        if overlapping(dir, mine) {
            return Err(SnapshotError::Overlaps {
                dest: dir.to_path_buf(),
                mine: mine.to_path_buf(),
            });
        }
    }
    if fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some()) {
        return Err(SnapshotError::NotEmpty(dir.to_path_buf()));
    }
    let dest = layout(dir);
    for made in [dir, dest.config_dir(), dest.data_dir()] {
        private_dir(made)?;
    }
    copy_file(&source.accounts_file(), &dest.accounts_file())?;
    copy_file(&source.legacy_account_file(), &dest.legacy_account_file())?;
    copy_file(&source.settings_file(), &dest.settings_file())?;
    copy_file(&source.legacy_region_file(), &dest.legacy_region_file())?;
    for (name, from) in files_in(&source.data_dir().join("regions"), "json")? {
        let to = dest.data_dir().join("regions");
        private_dir(&to)?;
        copy_file(&from, &to.join(name))?;
    }
    for (name, from) in files_in(&source.data_dir().join("store"), "sqlite")? {
        let to = dest.data_dir().join("store");
        private_dir(&to)?;
        teams_store::snapshot::snapshot(&from, &to.join(name))?;
    }
    Ok(dest)
}

/// The files of `dir` ending in `.extension` (none when `dir` doesn't exist), by name.
fn files_in(dir: &Path, extension: &str) -> Result<Vec<(String, PathBuf)>, SnapshotError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(SnapshotError::Io {
                path: dir.to_path_buf(),
                source,
            });
        }
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| SnapshotError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_file() && path.extension().is_some_and(|found| found == extension) {
            files.push((name, path));
        }
    }
    files.sort();
    Ok(files)
}

/// Makes `dir` (and its parents), `0700`.
fn private_dir(dir: &Path) -> Result<(), SnapshotError> {
    let failed = |source| SnapshotError::Io {
        path: dir.to_path_buf(),
        source,
    };
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(failed)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(failed)
}

/// Copies `from` to `to` (`0600`, a new file) when `from` exists; nothing when it doesn't.
fn copy_file(from: &Path, to: &Path) -> Result<(), SnapshotError> {
    let bytes = match fs::read(from) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(SnapshotError::Io {
                path: from.to_path_buf(),
                source,
            });
        }
    };
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(to)
        .and_then(|mut file| io::Write::write_all(&mut file, &bytes));
    written.map_err(|source| SnapshotError::Io {
        path: to.to_path_buf(),
        source,
    })
}
