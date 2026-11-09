//! A runner owns the last handle to a non-breakaway, kill-on-close job. All app restart images and workers remain
//! in that job; no PID lookup or recursive process enumeration is needed to clean them up. Creation is atomic with
//! process launch through `Transfer::spawn_in_job` (Windows 10's `PROC_THREAD_ATTRIBUTE_JOB_LIST`).
use std::io;
use std::os::windows::io::{AsHandle as _, AsRawHandle as _, BorrowedHandle, FromRawHandle as _, OwnedHandle};
use std::ptr::null;
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};

/// An unnamed job. Its handle is private and must never be duplicated into one of its children.
#[derive(Debug)]
pub struct Job(OwnedHandle);

impl Job {
    /// Create a job that terminates every member when its last handle closes, including if the runner crashes.
    ///
    /// # Errors
    /// Job creation or configuring its limits fails.
    pub fn new() -> io::Result<Self> {
        // SAFETY: no name or security attributes: a fresh unnamed, noninheritable kernel object.
        let handle = unsafe { CreateJobObjectW(null(), null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a fresh uniquely owned handle.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: job and the initialized limit structure outlive the call. Breakaway flags remain clear.
        if unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                u32::try_from(size_of_val(&limits)).map_err(io::Error::other)?,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    pub(super) fn handle(&self) -> BorrowedHandle<'_> {
        self.0.as_handle()
    }

    /// Count every live member, including descendants whose original app process has exited after a restart.
    ///
    /// # Errors
    /// The native query fails.
    pub fn active_processes(&self) -> io::Result<u32> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: job is owned, accounting is a writable output of the correct size and type.
        if unsafe {
            QueryInformationJobObject(
                self.0.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&raw mut accounting).cast(),
                u32::try_from(size_of_val(&accounting)).map_err(io::Error::other)?,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(accounting.ActiveProcesses)
    }

    /// Stop the complete tree. Callers retain locks until `active_processes` reaches zero.
    ///
    /// # Errors
    /// The native termination fails.
    pub fn terminate(&self, exit_code: u32) -> io::Result<()> {
        // SAFETY: job is the owned unnamed job, with no PID-based lookup.
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), exit_code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
