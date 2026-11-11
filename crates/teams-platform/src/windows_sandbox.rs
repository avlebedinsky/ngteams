//! The Windows lab/test display is the explicitly authorized win11 VM's interactive console, not the host display.
//! This is an accidental-launch guard, like the Linux sandbox's environment opt-in; the runner owns isolation and
//! agents still verify which VM they drive. A regular desktop launch never opts in through this boundary.
use std::ffi::OsStr;
use std::io;
use std::path::Path;
use windows_sys::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId};

pub const VM_ENV: &str = "NGTEAMS_WINDOWS_VM_SANDBOX";
pub const DIRECTORY_ENV: &str = "UI_SANDBOX_DIR";

/// Validate the runner's explicit VM opt-in, existing private run directory and interactive console session.
///
/// # Errors
/// Missing opt-in/directory or a process outside the active VM console (including an SSH/service process).
fn console_allowed(
    vm: Option<&OsStr>,
    directory: Option<&OsStr>,
    process_session: u32,
    console_session: u32,
) -> io::Result<()> {
    if vm != Some(OsStr::new("win11")) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the Windows UI sandbox requires an explicit win11 VM console opt-in",
        ));
    }
    if !directory.is_some_and(|directory| {
        let path = Path::new(directory);
        path.is_absolute() && path.is_dir()
    }) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the Windows UI sandbox requires its existing absolute run directory",
        ));
    }
    if process_session == 0 || console_session == u32::MAX || process_session != console_session {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the Windows UI sandbox must run in the VM's active console session",
        ));
    }
    Ok(())
}

/// Validate this process before opening a lab/read-only/test window. No display or media is opened by this check.
///
/// # Errors
/// Native session lookup failed, or [`console_allowed`] refused the environment/session.
pub fn window_allowed_here() -> io::Result<()> {
    let mut process_session = 0;
    // SAFETY: the current PID is valid, and process_session is a live output buffer.
    if unsafe { ProcessIdToSessionId(std::process::id(), &raw mut process_session) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: this returns a session number, not an owned handle or pointer.
    let console_session = unsafe { WTSGetActiveConsoleSessionId() };
    console_allowed(
        std::env::var_os(VM_ENV).as_deref(),
        std::env::var_os(DIRECTORY_ENV).as_deref(),
        process_session,
        console_session,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_an_explicit_vm_console_run_can_open_a_window() {
        let directory = tempfile::tempdir().unwrap();
        let path = Some(directory.path().as_os_str());
        let vm = Some(OsStr::new("win11"));
        console_allowed(vm, path, 1, 1).unwrap();
        for value in [
            None,
            Some(OsStr::new("")),
            Some(OsStr::new("1")),
            Some(OsStr::new("desktop")),
        ] {
            assert!(console_allowed(value, path, 1, 1).is_err());
        }
        for value in [
            None,
            Some(OsStr::new("relative")),
            Some(directory.path().join("absent").as_os_str()),
        ] {
            assert!(console_allowed(vm, value, 1, 1).is_err());
        }
        let file = tempfile::NamedTempFile::new().unwrap();
        assert!(console_allowed(vm, Some(file.path().as_os_str()), 1, 1).is_err());
        for (process, console) in [(0, 0), (0, 1), (1, 2), (1, u32::MAX)] {
            assert!(console_allowed(vm, path, process, console).is_err());
        }
        assert!(window_allowed_here().is_err()); // Native offline harness has no VM runner opt-in.
    }
}
