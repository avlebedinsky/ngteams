//! Real filesystem and kernel-lock checks, without accounts or tenant traffic.
#![cfg(test)]

#[path = "support/privacy.rs"]
mod privacy;

#[cfg(unix)]
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::Path;

use teams_platform::private::{self, Open};

#[test]
fn new_directories_and_files_are_private_without_a_followup_restriction() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("first/second");
    private::create_dir_all(&dir).unwrap();
    privacy::assert_private(&dir, false);
    privacy::assert_private(dir.parent().unwrap(), false);
    private::create_dir_all(&dir).unwrap();
    private::create_dir_all(Path::new("")).unwrap();
    for (name, access) in [
        ("append", Open::Append),
        ("new-append", Open::NewAppend),
        ("read-write", Open::ReadWrite),
        ("new-read-write", Open::NewReadWrite),
    ] {
        let path = dir.join(name);
        let mut file = private::open(&path, access).unwrap();
        file.write_all(b"synthetic private bytes").unwrap();
        privacy::assert_private(&path, false);
    }
}

#[test]
fn opening_preserves_contents_and_create_new_refuses_existing_files() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file");
    let mut file = private::open(&path, Open::NewReadWrite).unwrap();
    file.write_all(b"first").unwrap();
    drop(file);
    for access in [Open::NewAppend, Open::NewReadWrite] {
        assert_eq!(
            private::open(&path, access).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
    }
    private::open(&path, Open::Append)
        .unwrap()
        .write_all(b"second")
        .unwrap();
    let mut contents = String::new();
    private::open(&path, Open::ReadWrite)
        .unwrap()
        .read_to_string(&mut contents)
        .unwrap();
    assert_eq!(contents, "firstsecond");
    private::restrict_file(&path).unwrap();
    privacy::assert_private(&path, false);
}

#[test]
fn restriction_does_not_close_a_descriptor_or_drop_its_lock() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("locked");
    let held = private::open(&path, Open::ReadWrite).unwrap();
    held.lock().unwrap();
    private::restrict_file(&path).unwrap();
    let contender = private::open(&path, Open::ReadWrite).unwrap();
    assert!(matches!(contender.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
    drop(held);
    contender.try_lock().unwrap();
}

#[test]
fn invalid_paths_and_wrong_object_kinds_fail() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file");
    private::open(&path, Open::NewAppend).unwrap();
    assert!(private::restrict_dir(&path).is_err());
    assert!(private::restrict_file(root.path()).is_err());
    assert!(private::restrict_file(&root.path().join("missing")).is_err());
    assert!(private::restrict_dir(&root.path().join("missing")).is_err());
    assert!(private::create_dir_all(&path.join("child")).is_err());
    assert!(private::open(&path.join("child"), Open::ReadWrite).is_err());
    assert!(private::open(Path::new("invalid\0name"), Open::NewAppend).is_err());
}

#[cfg(windows)]
#[test]
fn windows_restriction_removes_an_explicit_everyone_grant() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("earlier.log");
    std::fs::write(&path, b"earlier bytes").unwrap();
    let loosened = std::process::Command::new("powershell.exe")
        .env_remove("PSModulePath")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            r"
            $ErrorActionPreference = 'Stop'
            foreach ($path in @($env:NGTEAMS_TEST_DIR, $env:NGTEAMS_TEST_FILE)) {
                $acl = Get-Acl -LiteralPath $path
                $everyone = [System.Security.Principal.SecurityIdentifier]::new('S-1-1-0')
                $rule = [System.Security.AccessControl.FileSystemAccessRule]::new($everyone, 'FullControl', 'Allow')
                $acl.AddAccessRule($rule)
                Set-Acl -LiteralPath $path -AclObject $acl
            }
        ",
        ])
        .env("NGTEAMS_TEST_DIR", root.path())
        .env("NGTEAMS_TEST_FILE", &path)
        .output()
        .unwrap();
    assert!(
        loosened.status.success(),
        "{}",
        String::from_utf8_lossy(&loosened.stderr)
    );
    private::restrict_dir(root.path()).unwrap();
    private::restrict_file(&path).unwrap();
    privacy::assert_private(root.path(), false);
    privacy::assert_private(&path, false);
    assert_eq!(std::fs::read(&path).unwrap(), b"earlier bytes");
}

#[cfg(unix)]
#[test]
fn permissive_existing_files_and_directories_are_tightened() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("shared");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    let path = dir.join("file");
    fs::write(&path, b"earlier bytes").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    private::restrict_dir(&dir).unwrap();
    private::restrict_file(&path).unwrap();
    private::restrict_file(&path).unwrap();
    privacy::assert_private(&dir, false);
    privacy::assert_private(&path, false);
    assert_eq!(fs::read(&path).unwrap(), b"earlier bytes");
}

#[cfg(windows)]
#[test]
fn check_open_file_rejects_unsafe_acl_without_repairing_it() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("input.json");
    let file = private::open(&path, Open::NewReadWrite).unwrap();
    private::check_file(&file).unwrap();
    let changed = std::process::Command::new("powershell.exe")
        .env_remove("PSModulePath")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            r"
            $ErrorActionPreference = 'Stop'
            $acl = Get-Acl -LiteralPath $env:NGTEAMS_TEST_FILE
            $everyone = [Security.Principal.SecurityIdentifier]::new('S-1-1-0')
            $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($everyone, 'Read', 'Allow'))
            Set-Acl -LiteralPath $env:NGTEAMS_TEST_FILE -AclObject $acl
        ",
        ])
        .env("NGTEAMS_TEST_FILE", &path)
        .output()
        .unwrap();
    assert!(changed.status.success(), "{}", String::from_utf8_lossy(&changed.stderr));
    for _ in 0..2 {
        assert_eq!(
            private::check_file(&file).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }
    private::restrict_file(&path).unwrap();
    private::check_file(&file).unwrap();
}

#[test]
fn replacing_a_private_output_truncates_and_restricts_existing_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("output");
    private::replace(&path)
        .unwrap()
        .write_all(b"synthetic long output")
        .unwrap();
    std::fs::write(&path, b"earlier output").unwrap();
    let mut file = private::replace(&path).unwrap();
    privacy::assert_private(&path, false);
    file.write_all(b"new").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
    assert!(private::replace(dir.path()).is_err());
}

#[cfg(windows)]
#[test]
fn shared_holders_exclude_exclusive_and_exclusive_excludes_every_holder() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lock");
    let first = private::share_lock(&path, true).unwrap();
    let second = private::share_lock(&path, true).unwrap();
    assert!(private::share_lock(&path, false).is_err());
    drop((first, second));
    let exclusive = private::share_lock(&path, false).unwrap();
    assert!(private::share_lock(&path, false).is_err());
    assert!(private::share_lock(&path, true).is_err());
    drop(exclusive);
    private::share_lock(&path, true).unwrap();
    assert!(private::share_lock(&dir.path().join("absent/lock"), false).is_err());
}
