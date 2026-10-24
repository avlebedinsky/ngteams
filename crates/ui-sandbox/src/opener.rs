//! The no-op URI opener: scripts named after the launchers GPUI's `open_url` runs, first on a sandboxed client's
//! `PATH`, that record their arguments and open nothing.
//!
//! GPUI's Linux `open_url` runs the `open` crate's launchers in turn (`xdg-open`, `gio open`, `gnome-open`,
//! `kde-open`) and stops at the first that succeeds; only when all fail does it ask the `OpenURI` portal on the session
//! bus, which the sandbox's private bus doesn't have. Each script here exits 0, so `xdg-open` takes every request and
//! the desktop's opener, browser and portal are never reached.

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
};

use crate::SandboxResult;

/// The log's name in the sandbox dir: one line per call, the launcher's name and its arguments, tab-separated.
pub const LOG: &str = "opened.log";

/// The launchers covered, in the order GPUI tries them.
pub const LAUNCHERS: [&str; 4] = ["xdg-open", "gio", "gnome-open", "kde-open"];

/// One call of a launcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    pub launcher: String,
    pub args: Vec<String>,
}

/// `text` as one single-quoted shell word.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Writes the launchers into `bin` (mode 0755), each appending to `log`.
///
/// # Errors
/// A script can't be written.
pub fn install(bin: &Path, log: &Path) -> SandboxResult<Vec<PathBuf>> {
    let log = shell_quote(&log.to_string_lossy());
    let script = format!(
        "#!/bin/sh\n# ui-sandbox: a no-op URI opener; records what it was asked to open and opens nothing\n\
         line=\"${{0##*/}}\"\nfor arg in \"$@\"; do line=\"$line\t$arg\"; done\n\
         printf '%s\\n' \"$line\" >> {log}\nexit 0\n"
    );
    let mut written = Vec::new();
    for launcher in LAUNCHERS {
        let path = bin.join(launcher);
        fs::write(&path, &script)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        written.push(path);
    }
    Ok(written)
}

/// The calls recorded in `log`, in order; none when it doesn't exist yet.
///
/// # Errors
/// It exists but can't be read.
pub fn read_log(log: &Path) -> SandboxResult<Vec<Opened>> {
    let text = match fs::read_to_string(log) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    Ok(text
        .lines()
        .map(|line| {
            let mut fields = line.split('\t').map(str::to_owned);
            Opened {
                launcher: fields.next().unwrap_or_default(),
                args: fields.collect(),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_survive_the_shell() {
        assert_eq!(shell_quote("/tmp/a b"), "'/tmp/a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
