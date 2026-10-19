//! What the browser tests share: finding Chrome (and skipping without one), and watching a process die.
//!
//! Chrome is taken from `$PROTO_LAB_CHROME`, else from the pinned build in the lab's chrome dir, found as
//! `proto-lab chrome install` finds it (lab.toml's `data_dir` when set). Without either a test says so on stderr and
//! passes, unless `PROTO_LAB_REQUIRE_CHROME=1`.

use std::path::PathBuf;
use std::time::Duration;

use proto_lab::commands::chrome::ChromeBuild;
use proto_lab::config::LabLocations;

/// The Chrome to test with, if there is one.
pub fn find_chrome() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PROTO_LAB_CHROME") {
        return Some(PathBuf::from(path));
    }
    // the same dir `proto-lab chrome install` installs into
    let paths = match LabLocations::from_env().and_then(|locations| locations.lab_paths()) {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("can't find the lab's chrome dir: {error}");
            return None;
        }
    };
    let executable = ChromeBuild::pinned().executable(&paths.chrome_dir());
    executable.is_file().then_some(executable)
}

/// The Chrome to test with; `None` (after a note on stderr) means the test is skipped.
///
/// # Panics
/// When there's no Chrome and `PROTO_LAB_REQUIRE_CHROME` is set.
pub fn chrome_or_skip() -> Option<PathBuf> {
    let chrome = find_chrome();
    if chrome.is_none() {
        assert!(
            std::env::var_os("PROTO_LAB_REQUIRE_CHROME").is_none(),
            "PROTO_LAB_REQUIRE_CHROME is set, but there's no Chrome (run `proto-lab chrome install`)"
        );
        eprintln!("skipped: no Chrome (set PROTO_LAB_CHROME or run `proto-lab chrome install`)");
    }
    chrome
}

/// Whether a process is still running (a zombie waiting for its parent counts as gone).
pub fn running(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(')')
            .is_some_and(|(_, rest)| !rest.trim_start().starts_with('Z'))
    })
}

/// Waits up to 5 s for a process to be gone.
pub async fn gone(pid: u32) -> bool {
    for _ in 0..100 {
        if !running(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}
