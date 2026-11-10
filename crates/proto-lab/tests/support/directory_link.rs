//! A real directory alias in a temporary test tree, without Windows symlink privileges.
#![cfg(test)]

use std::path::Path;

#[cfg(unix)]
pub fn create(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
pub fn create(target: &Path, link: &Path) {
    // Junction creation needs no developer-mode or security-policy change. Pass paths through the environment
    // rather than interpolating test directory names into shell syntax.
    let result = std::process::Command::new("powershell.exe")
        .env_remove("PSModulePath")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:NGTEAMS_TEST_LINK -Target $env:NGTEAMS_TEST_TARGET | Out-Null",
        ])
        .env("NGTEAMS_TEST_LINK", link)
        .env("NGTEAMS_TEST_TARGET", target)
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(link.canonicalize().unwrap(), target.canonicalize().unwrap());
}
