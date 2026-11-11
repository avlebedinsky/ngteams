//! Private Windows lab output, using protected DACLs before any bytes are written.
use std::fs::File;
use std::io::{self, Write as _};
use std::path::Path;

use teams_platform::private::{self, Open};

/// Truncates a verified private output file.
///
/// # Errors
/// The file cannot be privately opened, checked or truncated.
pub fn replace(path: &Path) -> io::Result<File> {
    let file = private::open(path, Open::ReadWrite)?;
    private::check_file(&file)?;
    file.set_len(0)?;
    Ok(file)
}

/// Atomically replaces a private lab output.
///
/// # Errors
/// Private staging, writing or persistence fails.
pub fn atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| io::Error::other("missing output parent"))?;
    private::create_dir_all(parent).map_err(|error| stage_error("create output directory", &error))?;
    private::restrict_dir(parent).map_err(|error| stage_error("protect output directory", &error))?;
    // Use tempfile's unique name and cleanup guard with our native protected creation descriptor. Ordinary
    // tempfile creation under an elevated SSH token would initially give the file to Administrators.
    let mut stage = tempfile::Builder::new()
        .prefix(".lab-")
        .make_in(parent, |path| private::open(path, Open::NewReadWrite))
        .map_err(|error| stage_error("create output stage", &error))?;
    private::check_file(stage.as_file()).map_err(|error| stage_error("verify output stage", &error))?;
    stage.write_all(bytes)?;
    stage.as_file().sync_all()?;
    // std's Windows rename supports FileRenameInfoEx when simultaneous replacements make MoveFileEx deny the
    // destination. tempfile::persist uses MoveFileEx alone. Keep the cleanup guard until publication succeeds.
    let stage = stage.into_temp_path();
    std::fs::rename(&stage, path).map_err(|error| stage_error("publish output stage", &error))?;
    Ok(())
}

fn stage_error(operation: &str, error: &io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{operation}: {error}"))
}

/// Matches the Windows native runner's sharing-based lifetime lock.
///
/// # Errors
/// A holder already has this lock, or it cannot be privately created/opened.
pub fn exclusive(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    if !path.try_exists()? {
        drop(private::open(path, Open::Append)?);
    }
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::AccountsFile;
    use crate::config::Alias;

    #[test]
    fn output_replacement_and_exclusive_lock_release_use_native_file_handles() {
        let root = tempfile::tempdir().unwrap();
        private::restrict_dir(root.path()).unwrap();
        let output = root.path().join("output.json");
        atomic(&output, b"synthetic-long-output").unwrap();
        let mut file = replace(&output).unwrap();
        file.write_all(b"new").unwrap();
        drop(file);
        assert_eq!(std::fs::read(&output).unwrap(), b"new");
        private::check_file(&File::open(&output).unwrap()).unwrap();
        let lock = root.path().join("live.lock");
        let held = exclusive(&lock).unwrap();
        assert!(exclusive(&lock).is_err());
        drop(held);
        drop(exclusive(&lock).unwrap());
        assert!(
            std::fs::read_dir(root.path()).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".lab-"))
        );
    }

    #[test]
    fn failed_output_publication_cleans_its_stage_and_keeps_the_destination() {
        let root = tempfile::tempdir().unwrap();
        private::restrict_dir(root.path()).unwrap();
        let destination = root.path().join("directory");
        private::create_dir_all(&destination).unwrap();
        assert!(atomic(&destination, b"must not publish").is_err());
        assert!(destination.is_dir());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        let blocked = destination.join("file");
        atomic(&blocked, b"blocking file").unwrap();
        assert!(atomic(&blocked.join("child"), b"must not publish").is_err());
        assert!(replace(&destination).is_err());
        assert!(exclusive(&blocked.join("missing")).is_err());
    }

    #[test]
    fn account_input_requires_safe_acl_and_matches_the_requested_roster_account() {
        let root = tempfile::tempdir().unwrap();
        private::restrict_dir(root.path()).unwrap();
        let path = root.path().join("accounts.json");
        let mut file = private::open(&path, Open::NewReadWrite).unwrap();
        file.write_all(br#"[{"profile":"a","email":"synthetic-a@example.test","password":"synthetic-only"}]"#)
            .unwrap();
        let accounts = AccountsFile::load(&path, |_| None).unwrap();
        assert!(accounts.for_alias(Alias::A, "someone-else@example.test").is_err());
        assert!(accounts.for_alias(Alias::B, "synthetic-a@example.test").is_err());
        let account = accounts.for_alias(Alias::A, "synthetic-a@example.test").unwrap();
        assert_eq!(account.password().expose(), "synthetic-only");
        assert!(!format!("{account:?}").contains("synthetic-only"));
        atomic(&root.path().join("region.json"), b"first").unwrap();
        atomic(&root.path().join("region.json"), b"second").unwrap();
        assert_eq!(std::fs::read(root.path().join("region.json")).unwrap(), b"second");
        for alias in ["", "..", "../ngteams", "a/b", "a\\b", "C:", "a."] {
            assert!(crate::commands::auth::NativeTarget::windows_alias(alias).is_err());
        }
    }
}
