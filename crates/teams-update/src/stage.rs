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
                && remove(&work).is_ok()
        })
        .count()
}

/// Removes the stage `work` with its owner file last, so a stage a file of which is still held (Windows' installer
/// runs from its stage until it exits) keeps its owner and is removed by a later start.
fn remove(work: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(work)? {
        let entry = entry?;
        if entry.file_name() == OWNER {
            continue;
        }
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    fs::remove_dir_all(work)
}

/// Whether process `pid` runs; one that can't be asked counts as running.
#[cfg(unix)]
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

/// Whether process `pid` runs; one that can't be asked counts as running.
#[cfg(windows)]
pub(crate) fn alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // the idle process, never an owner
    if pid == 0 {
        return true;
    }
    // SAFETY: a query-only handle to `pid`, closed below; nothing else is done with it.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        // SAFETY: reads this thread's last error, set by the failed OpenProcess above.
        return unsafe { GetLastError() } != ERROR_INVALID_PARAMETER;
    }
    let mut code = 0;
    // SAFETY: the handle opened above and a writable exit code.
    let asked = unsafe { GetExitCodeProcess(process, &raw mut code) };
    // SAFETY: the one handle OpenProcess returned.
    unsafe { CloseHandle(process) };
    asked == 0 || code == STILL_ACTIVE.cast_unsigned()
}
