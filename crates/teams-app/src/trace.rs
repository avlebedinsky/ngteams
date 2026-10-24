//! The view trace's descriptor (`--trace-fd <n>`): handed to the app at its start by whoever reads the trace (the live
//! test), open for writing, taken over before anything else in the process opens a descriptor.

use std::fs::File;
use std::io;
use std::os::fd::{FromRawFd as _, OwnedFd, RawFd};

use rustix::fs::{OFlags, fcntl_getfl};
use rustix::io::{FdFlags, fcntl_setfd};

use crate::secrets::FIRST_PASSED_FD;

/// Why the trace's descriptor wasn't taken.
#[derive(Debug, thiserror::Error)]
pub enum TraceFdError {
    #[error("trace descriptor {0} is stdin, stdout or stderr, or not a descriptor (it is 3 or above)")]
    Reserved(RawFd),
    #[error("trace descriptor {fd} isn't open: {source}")]
    NotOpen { fd: RawFd, source: io::Error },
    #[error("trace descriptor {fd} isn't open for writing")]
    NotWritable { fd: RawFd },
    #[error("trace descriptor {fd}: {source}")]
    Io { fd: RawFd, source: io::Error },
}

/// Takes over descriptor `fd` as the view trace's file: 3 or above, open, open for writing; made close-on-exec. One
/// that is open but not writable is closed; one below 3 or not open is left alone.
///
/// # Errors
/// See [`TraceFdError`].
pub fn adopt_trace(fd: RawFd) -> Result<File, TraceFdError> {
    if fd < FIRST_PASSED_FD {
        return Err(TraceFdError::Reserved(fd));
    }
    // SAFETY: `F_GETFD` only reads the descriptor's flags; a number that isn't an open descriptor fails with `EBADF`.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
        return Err(TraceFdError::NotOpen {
            fd,
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: the descriptor is open (checked above), and the command line hands it to this process to own: nothing
    // else in the process uses it, since the app takes it over before it opens anything of its own.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    let flags = fcntl_getfl(&owned).map_err(|error| TraceFdError::Io {
        fd,
        source: error.into(),
    })?;
    if !(flags.contains(OFlags::WRONLY) || flags.contains(OFlags::RDWR)) {
        return Err(TraceFdError::NotWritable { fd });
    }
    fcntl_setfd(&owned, FdFlags::CLOEXEC).map_err(|error| TraceFdError::Io {
        fd,
        source: error.into(),
    })?;
    Ok(File::from(owned))
}
