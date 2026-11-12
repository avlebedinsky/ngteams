//! Private files: Unix's 0700/0600, or a protected Windows DACL granting the current user and SYSTEM full access.
//! New files and directories are private from creation, even under a shared parent. Existing ancestors are left
//! alone; callers can explicitly restrict the leaf directory. Windows descriptors have delete sharing so log
//! rotation can rename an open file, and SQLite can remove a finished run's lock.

use std::fs::File;
use std::io;
use std::path::Path;

#[cfg(unix)]
#[path = "private/unix.rs"]
pub(crate) mod platform;
#[cfg(windows)]
#[path = "private/windows.rs"]
pub(crate) mod platform;

/// How to open a private file; none of these truncate an existing file.
#[derive(Clone, Copy, Debug)]
pub enum Open {
    Append,
    NewAppend,
    NewReadWrite,
    ReadWrite,
}

/// Creates missing directories privately. Does not change permissions on existing directories.
///
/// # Errors
/// The path cannot be created with private access.
pub fn create_dir_all(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    platform::create_dir_all(path)
}

/// Removes other users' access to an existing directory (including inherited grants on Windows).
///
/// # Errors
/// The path is not a directory or its permissions cannot be changed.
pub fn restrict_dir(path: &Path) -> io::Result<()> {
    if !path.metadata()?.is_dir() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a directory"));
    }
    platform::restrict(path, true)
}

/// Removes other users' access to an existing regular file, without opening or closing the file.
/// SQLite's POSIX locks survive this check even when another connection in the process has the file open.
///
/// # Errors
/// The path is not a regular file or its permissions cannot be changed.
pub fn restrict_file(path: &Path) -> io::Result<()> {
    if !path.metadata()?.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
    }
    platform::restrict(path, false)
}

/// Opens a file, creating it privately if missing. An existing file's permissions are left alone: a caller can
/// restrict it by path without opening a second descriptor on a SQLite database.
///
/// # Errors
/// The file cannot be opened with the requested access or private creation permissions.
pub fn open(path: &Path, access: Open) -> io::Result<File> {
    platform::open(path, access)
}

#[cfg(windows)]
#[path = "private/windows_check.rs"]
mod windows_check;

/// Verifies that an opened file's owner and access grants are private, without changing them.
///
/// # Errors
/// The file is not regular, not owned by the current user, or grants access to anyone but this user and SYSTEM.
#[cfg(windows)]
pub fn check_file(file: &File) -> io::Result<()> {
    windows_check::check_file(file)
}

/// Opens a kernel sharing lock. Shared holders coexist; an exclusive holder excludes all other holders.
/// The returned file owns the lock until it closes, including after a process crash.
///
/// # Errors
/// A conflicting holder exists or the file cannot be privately created/opened.
#[cfg(windows)]
pub fn share_lock(path: &Path, shared: bool) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
    if !path.try_exists()? {
        drop(open(path, Open::Append)?);
    }
    std::fs::OpenOptions::new()
        .read(true)
        .write(!shared)
        .share_mode(if shared { FILE_SHARE_READ } else { 0 })
        .open(path)
}

/// Opens privately, removes other users' access, then truncates the file before any new bytes are written.
///
/// # Errors
/// Opening, restricting or truncating the file fails.
pub fn replace(path: &Path) -> io::Result<File> {
    let file = open(path, Open::ReadWrite)?;
    restrict_file(path)?;
    file.set_len(0)?;
    Ok(file)
}
