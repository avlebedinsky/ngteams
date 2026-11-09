//! Kernel-enforced sharing locks. No PID files identify a holder. A crashed holder releases them automatically.
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::Path;
use teams_platform::private::{self, Open};
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

pub fn lock(path: &Path, shared: bool) -> io::Result<File> {
    // Create privately, then reopen with the sharing contract. Simultaneous creation is harmless; an active
    // exclusive holder can also refuse this open, which is the desired lock failure.
    if !path.try_exists()? {
        drop(private::open(path, Open::Append)?);
    }
    OpenOptions::new()
        .read(true)
        .write(!shared)
        .share_mode(if shared { FILE_SHARE_READ } else { 0 })
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_holders_exclude_exclusive_and_exclusive_excludes_every_holder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let first = lock(&path, true).unwrap();
        let second = lock(&path, true).unwrap();
        assert!(lock(&path, false).is_err());
        drop((first, second));
        let exclusive = lock(&path, false).unwrap();
        assert!(lock(&path, false).is_err());
        assert!(lock(&path, true).is_err());
        drop(exclusive);
        lock(&path, true).unwrap();
    }
}
