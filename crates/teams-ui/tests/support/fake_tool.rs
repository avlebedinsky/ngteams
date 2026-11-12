//! Synthetic CLI programs. Unix runs an executable script; Windows runs a native .cmd shim, like npm-installed
//! tools, with Git for Windows' Bash interpreting the same fake protocol. No installed AI tool is ever invoked.
use std::fs;
use std::path::{Path, PathBuf};

pub fn program(dir: &Path, name: &str) -> PathBuf {
    #[cfg(unix)]
    {
        dir.join(name)
    }
    #[cfg(windows)]
    {
        dir.join(format!("{name}.cmd"))
    }
}

#[cfg(windows)]
fn bash() -> std::io::Result<PathBuf> {
    let root =
        std::env::var_os("ProgramFiles").ok_or_else(|| std::io::Error::other("Windows ProgramFiles is unset"))?;
    let path = Path::new(&root).join("Git/bin/bash.exe");
    if !path.is_file() {
        return Err(std::io::Error::other(format!(
            "fake CLI tests require Git for Windows: {}",
            path.display()
        )));
    }
    Ok(path)
}

pub fn try_write(dir: &Path, name: &str, body: &str) -> std::io::Result<PathBuf> {
    let source = dir.join(format!("{name}.source"));
    fs::write(&source, format!("#!/bin/bash\n{body}"))?;
    let path = program(dir, name);
    #[cfg(unix)]
    {
        // Publish through a child: another test's fork cannot retain our writable script descriptor (ETXTBSY).
        let installed = std::process::Command::new("install")
            .args(["-m", "755"])
            .arg(&source)
            .arg(&path)
            .status()?;
        if !installed.success() {
            return Err(std::io::Error::other("cannot install synthetic script"));
        }
    }
    #[cfg(windows)]
    {
        let script = format!(
            "@echo off\r\nset MSYS_NO_PATHCONV=1\r\n\"{}\" \"%~dp0{name}.source\" %*\r\n",
            bash()?.display()
        );
        fs::write(&path, script)?;
    }
    Ok(path)
}

#[cfg(test)]
pub fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    try_write(dir, name, body).unwrap()
}

#[cfg(test)]
pub fn running(pid: &str) -> bool {
    #[cfg(unix)]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(windows)]
    {
        // Bash's $$ is its MSYS PID. Ask the same runtime, not Win32's unrelated numeric PID namespace.
        assert!(pid.bytes().all(|byte| byte.is_ascii_digit()) && !pid.is_empty());
        std::process::Command::new(bash().unwrap())
            .args(["-c", &format!("kill -0 {pid}")])
            .output()
            .unwrap()
            .status
            .success()
    }
}
