//! The store file is the account's chat history: private to the user from the moment it exists. SQLite gives its
//! `-wal` and `-shm` files the main file's mode when it creates them, so making the main file `0600` before SQLite
//! opens it covers them; ones left over from before are tightened too.

use std::ffi::OsString;
use std::fs::{DirBuilder, File, OpenOptions, Permissions};
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::StoreError;

const FILE_MODE: u32 = 0o600;
const DIR_MODE: u32 = 0o700;
/// The files SQLite keeps next to the database.
const SIDE_FILES: [&str; 3] = ["-wal", "-shm", "-journal"];

/// Creates the store's directory (`0700`) and file (`0600`) when missing, and tightens an existing file (or side file)
/// that others can read or write.
pub(crate) fn prepare(path: &Path) -> Result<(), StoreError> {
    let failed = |path: &Path| {
        let path = path.to_owned();
        move |source| StoreError::File { path, source }
    };
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        DirBuilder::new()
            .recursive(true)
            .mode(DIR_MODE)
            .create(parent)
            .map_err(failed(parent))?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(FILE_MODE)
        .open(path);
    tighten(&file.map_err(failed(path))?).map_err(failed(path))?;
    for suffix in SIDE_FILES {
        let side = side_file(path, suffix);
        match File::open(&side) {
            Ok(file) => tighten(&file).map_err(failed(&side))?,
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(failed(&side)(error)),
        }
    }
    Ok(())
}

fn tighten(file: &File) -> std::io::Result<()> {
    if file.metadata()?.permissions().mode() & 0o077 != 0 {
        file.set_permissions(Permissions::from_mode(FILE_MODE))?;
    }
    Ok(())
}

fn side_file(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}
