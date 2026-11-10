//! An update's stage: a hidden folder beside the installation (`.ngteams-update-*`) holding the download, the unpacked
//! release and, after a swap, the previous version. It names the process that made it, so a later start removes what
//! a run left behind without touching another running app's stage.

use std::fs;
use std::path::Path;

use crate::Error;

pub(crate) const PREFIX: &str = ".ngteams-update-";
const OWNER: &str = "owner";
/// Left in a stage whose previous version couldn't be put back: it is the user's to recover, never removed.
pub(crate) const RECOVERY: &str = "recovery-required";

/// A new stage under `parent`, owned by this process.
pub(crate) fn create(parent: &Path) -> Result<tempfile::TempDir, Error> {
    fs::create_dir_all(parent).map_err(|error| crate::prepare::unwritable(error, parent))?;
    let work = tempfile::Builder::new()
        .prefix(PREFIX)
        .tempdir_in(parent)
        .map_err(|error| crate::prepare::unwritable(error, parent))?;
    fs::write(work.path().join(OWNER), std::process::id().to_string())
        .map_err(|error| crate::prepare::unwritable(error, parent))?;
    Ok(work)
}

/// Removes the stages under `parent` whose owner is gone, or is this very process (a stage made before an update's
/// restart, which runs the new version in the same process); a stage that needs recovery stays. How many went.
pub(crate) fn cleanup(parent: &Path) -> usize {
    let Ok(entries) = fs::read_dir(parent) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            entry.file_name().to_string_lossy().starts_with(PREFIX) && entry.file_type().is_ok_and(|kind| kind.is_dir())
        })
        .filter(|entry| {
            let work = entry.path();
            let owner = fs::read_to_string(work.join(OWNER))
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok());
            owner.is_some_and(|pid| pid == std::process::id() || !alive(pid))
                && !work.join(RECOVERY).exists()
                && fs::remove_dir_all(&work).is_ok()
        })
        .count()
}

/// Whether process `pid` runs; one that can't be asked counts as running.
pub(crate) fn alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return true;
    };
    if pid <= 0 {
        return true;
    }
    // SAFETY: signal 0 only checks that the process exists; nothing is sent to it.
    let sent = unsafe { libc::kill(pid, 0) };
    sent == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}
