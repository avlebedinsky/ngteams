//! The Windows lab uses the system's Microsoft-signed Edge, in its own headless profile. No production worker
//! debugging surface is involved. Its signature and product identity are checked before each browser launch.
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use teams_platform::command::Tree;

/// Resolves only the system Edge installation, never an executable override.
///
/// # Errors
/// ProgramFiles(x86) is missing/relative or Edge is absent.
pub fn installed_edge() -> io::Result<PathBuf> {
    let base = std::env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::other("ProgramFiles(x86) must be absolute"))?;
    let path = base.join("Microsoft/Edge/Application/msedge.exe");
    if !path.is_file() {
        return Err(io::Error::other("Microsoft Edge is not installed"));
    }
    Ok(path)
}

pub(super) async fn verify_edge(executable: &Path) -> io::Result<()> {
    if executable != installed_edge()? {
        return Err(io::Error::other("only the system Edge browser is permitted"));
    }
    let system = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::other("SystemRoot must be absolute"))?;
    let mut command = Command::new(system.join("System32/WindowsPowerShell/v1.0/powershell.exe"));
    // Fixed code; the path travels as an environment value, never interpolated into source. No credential is read.
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            r"
        $ErrorActionPreference = 'Stop'
        $p = $env:NGTEAMS_LAB_EDGE_CHECK
        $signature = Get-AuthenticodeSignature -LiteralPath $p
        $version = (Get-Item -LiteralPath $p).VersionInfo
        if ($signature.Status -ne 'Valid') { exit 10 }
        if ($signature.SignerCertificate.Subject -notmatch '(^|, )CN=Microsoft Corporation(,|$)') { exit 20 }
        if ($version.OriginalFilename -ne 'msedge.exe' -or $version.ProductName -ne 'Microsoft Edge') { exit 30 }
    ",
        ])
        .env_remove("PSModulePath")
        .env("NGTEAMS_LAB_EDGE_CHECK", executable)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut check = Tree::spawn(&mut command)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = check.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "Edge signature or product verification failed ({status})"
                )))
            };
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(io::Error::other("Edge signature check timed out"));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Checks the installed browser's Microsoft signature without launching it.
///
/// # Errors
/// The system browser is missing or signature verification fails/times out.
pub async fn verify_installed_edge() -> io::Result<PathBuf> {
    let path = installed_edge()?;
    verify_edge(&path).await?;
    Ok(path)
}
