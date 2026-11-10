//! Disposable browser stand-ins; never a browser, GUI or tenant connection.
#[cfg(unix)]
pub(crate) fn spawn() -> (crate::browser::Browser, u32) {
    let child = tokio::process::Command::new("/bin/sleep")
        .arg("300")
        .env_clear()
        .process_group(0)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();
    (crate::browser::Browser::synthetic_process(child), pid)
}

#[cfg(windows)]
pub(crate) fn spawn() -> (crate::browser::Browser, u32) {
    let mut command = std::process::Command::new("powershell.exe");
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Seconds 300"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = teams_platform::command::Tree::spawn(&mut command).unwrap();
    let pid = child.id();
    (crate::browser::Browser::synthetic_process(child), pid)
}

#[cfg(unix)]
pub(crate) fn terminate(pid: u32) -> Result<(), String> {
    let pid = rustix::process::Pid::from_raw(pid.cast_signed()).ok_or("missing PID")?;
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).map_err(|error| error.to_string())
}

#[cfg(windows)]
pub(crate) fn terminate(pid: u32) -> Result<(), String> {
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    // SAFETY: an owned disposable child's PID, opened only for termination.
    let raw = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if raw.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // SAFETY: OpenProcess returned a new handle owned by this guard.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: the guard keeps the process handle alive.
    if unsafe { TerminateProcess(handle.as_raw_handle(), 0) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn exited(pid: u32) -> bool {
    !std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(windows)]
pub(crate) fn exited(pid: u32) -> bool {
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: querying a disposable child's PID with no mutation rights.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(87));
        return true;
    }
    // SAFETY: OpenProcess returned a new owned handle.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut code = 259;
    // SAFETY: handle and output remain valid throughout the call.
    assert_ne!(unsafe { GetExitCodeProcess(handle.as_raw_handle(), &raw mut code) }, 0);
    code != 259
}
