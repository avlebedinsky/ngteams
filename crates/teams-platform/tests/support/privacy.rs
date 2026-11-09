//! Assert native privacy, also used by the log and store integration tests.
#![cfg(test)]

use std::path::Path;

#[cfg(unix)]
pub fn assert_private(path: &Path, _allow_default_owner: bool) {
    use std::os::unix::fs::PermissionsExt as _;
    let metadata = path.metadata().unwrap();
    assert_eq!(
        metadata.permissions().mode() & 0o777,
        if metadata.is_dir() { 0o700 } else { 0o600 }
    );
}

#[cfg(windows)]
pub fn assert_private(path: &Path, allow_default_owner: bool) {
    // Inspect the OS's effective descriptor, including inherited ACEs on SQLite's side files. Only the current
    // user and SYSTEM may be granted access; chmod's read-only bit cannot establish this on Windows.
    let result = std::process::Command::new("powershell.exe")
        // A PowerShell 7 parent (Actions) exports its incompatible Security module path to Windows PowerShell.
        .env_remove("PSModulePath")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            r"
            $ErrorActionPreference = 'Stop'
            $acl = Get-Acl -LiteralPath $env:NGTEAMS_PRIVACY_TEST_PATH
            $identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
            $sid = $identity.User.Value
            $owners = @($sid)
            if ($env:NGTEAMS_PRIVACY_DEFAULT_OWNER -eq '1') { $owners += $identity.Owner.Value }
            if ($acl.GetOwner([System.Security.Principal.SecurityIdentifier]).Value -notin $owners) { exit 1 }
            $rules = @($acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
            if ($rules.Count -ne 2) { exit 2 }
            foreach ($rule in $rules) {
                if ($rule.IdentityReference.Value -notin @($sid, 'S-1-5-18')) { exit 3 }
                if ($rule.AccessControlType -ne 'Allow') { exit 4 }
                if ($rule.FileSystemRights -ne 'FullControl') { exit 5 }
            }
        ",
        ])
        .env("NGTEAMS_PRIVACY_TEST_PATH", path)
        .env(
            "NGTEAMS_PRIVACY_DEFAULT_OWNER",
            if allow_default_owner { "1" } else { "0" },
        )
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}: {:?}: {}",
        path.display(),
        result.status,
        String::from_utf8_lossy(&result.stderr)
    );
}
