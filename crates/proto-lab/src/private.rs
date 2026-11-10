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
    private::create_dir_all(parent)?;
    private::restrict_dir(parent)?;
    // tempfile's randomized name is created within an already protected directory.
    let mut stage = tempfile::Builder::new().prefix(".lab-").tempfile_in(parent)?;
    // An elevated SSH token gives ordinary tempfile creation to Administrators. Set the exact user owner and
    // protected DACL while it is still empty, then verify the opened handle before writing any bytes.
    private::restrict_file(stage.path())?;
    private::check_file(stage.as_file())?;
    stage.write_all(bytes)?;
    stage.as_file().sync_all()?;
    stage.persist(path).map_err(|error| error.error)?;
    Ok(())
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
