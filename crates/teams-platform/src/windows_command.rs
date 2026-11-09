//! The standard library owns Windows argument escaping and selective pipe inheritance. Its stable API lacks
//! process attribute lists, so launch suspended and attach to the job before resuming the only initial thread.

use std::io;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::os::windows::process::CommandExt as _;
use std::process::{Child, Command};
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
};

pub(super) fn spawn(command: &mut Command, job: &crate::windows_job::Job) -> io::Result<Child> {
    let mut child = command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW).spawn()?;
    if let Err(error) = attach(&child, job) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok(child)
}

fn attach(child: &Child, job: &crate::windows_job::Job) -> io::Result<()> {
    // SAFETY: both handles are borrowed from their live owners. The child has never run an instruction.
    if unsafe { AssignProcessToJobObject(job.handle().as_raw_handle(), child.as_raw_handle()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let thread = initial_thread(child.id())?;
    // SAFETY: this is the initial thread of the still-suspended owned child. No other code resumes it.
    if unsafe { ResumeThread(thread.as_raw_handle()) } != 1 {
        return Err(io::Error::other("could not resume the suspended command"));
    }
    Ok(())
}

fn initial_thread(pid: u32) -> io::Result<OwnedHandle> {
    // SAFETY: queries a snapshot without modifying any thread or process.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut entry = THREADENTRY32 {
        dwSize: u32::try_from(size_of::<THREADENTRY32>()).map_err(io::Error::other)?,
        ..Default::default()
    };
    let result = (|| {
        // SAFETY: the snapshot is valid and entry has the correct size.
        let mut available = unsafe { Thread32First(snapshot, &raw mut entry) };
        while available != 0 {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: the live child's PID cannot be recycled. It is suspended and cannot create other threads.
                let handle = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: OpenThread returned a fresh uniquely owned handle.
                return Ok(unsafe { OwnedHandle::from_raw_handle(handle) });
            }
            // SAFETY: the same initialized entry and snapshot remain valid.
            available = unsafe { Thread32Next(snapshot, &raw mut entry) };
        }
        Err(io::Error::other("the suspended command has no initial thread"))
    })();
    // SAFETY: the snapshot was created here and is closed exactly once.
    unsafe { CloseHandle(snapshot) };
    result
}
