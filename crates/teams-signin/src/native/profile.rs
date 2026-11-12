//! Chromium creates readonly cache metadata, and Windows may release its last file handles slightly after job
//! accounting reaches zero. Clear only the Windows readonly attribute in this private tree and retry deletion.
//! Never traverse reparse points or weaken ACLs. A failed cleanup retains a private path and is reported.
use std::fs;
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::fs::MetadataExt as _;
use std::path::Path;
use std::time::{Duration, Instant};
use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY, SetFileAttributesW};

pub(super) fn remove(path: &Path) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let result = removable(path).and_then(|()| fs::remove_dir_all(path));
        match result {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound && !path.try_exists()? => return Ok(()),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn removable(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    // FILE_ATTRIBUTE_REPARSE_POINT: even a directory junction must not lead outside our profile.
    if metadata.file_attributes() & 0x400 != 0 {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            removable(&entry?.path())?;
        }
    }
    if metadata.permissions().readonly() {
        let name: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let attributes = metadata.file_attributes() & !FILE_ATTRIBUTE_READONLY;
        let attributes = if attributes == 0 {
            FILE_ATTRIBUTE_NORMAL
        } else {
            attributes
        };
        // SAFETY: the filesystem-provided path is NUL-terminated and lives through the call. This changes the
        // Windows readonly flag only; it does not modify the protected DACL or follow a reparse point.
        if unsafe { SetFileAttributesW(name.as_ptr(), attributes) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readonly_browser_metadata_is_removed_and_missing_profiles_are_harmless() {
        let directory = tempfile::tempdir().unwrap();
        let profile = directory.path().join("browser/cache");
        teams_platform::private::create_dir_all(&profile).unwrap();
        let file = profile.join("synthetic-metadata");
        fs::write(&file, b"synthetic browser metadata").unwrap();
        let mut permissions = fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&file, permissions).unwrap();
        remove(directory.path()).unwrap();
        assert!(!directory.path().exists());
        remove(directory.path()).unwrap();
    }
}
