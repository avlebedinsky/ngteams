//! Unix private creation and permission changes.

use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;

use super::Open;

pub(super) fn create_dir_all(path: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)
}

pub(super) fn restrict(path: &Path, directory: bool) -> io::Result<()> {
    let mode = if directory { 0o700 } else { 0o600 };
    if fs::metadata(path)?.permissions().mode() & 0o777 != mode {
        fs::set_permissions(path, Permissions::from_mode(mode))?;
    }
    Ok(())
}

pub(super) fn open(path: &Path, access: Open) -> io::Result<File> {
    let mut options = OpenOptions::new();
    match access {
        Open::Append => options.append(true).create(true),
        Open::NewAppend => options.append(true).create_new(true),
        Open::NewReadWrite => options.read(true).write(true).create_new(true),
        Open::ReadWrite => options.read(true).write(true).create(true).truncate(false),
    };
    options.mode(0o600).open(path)
}
