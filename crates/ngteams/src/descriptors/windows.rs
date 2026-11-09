//! Inherited Windows handles are kernel handles, never CRT file descriptor indexes. They are private byte-pipe
//! endpoints or writable disk files. Only the explicit restart/runner launch may transfer these handles again.
use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

use teams_auth::broker::BrokerStream;
use windows_sys::Win32::Foundation::{
    GetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_APPEND_DATA, FILE_TYPE_DISK, FILE_WRITE_DATA, GetFileType};
use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};

/// The command-line representation of a Windows kernel handle, including all bits on x64.
pub type RawDescriptor = usize;
/// Numeric values below this cannot name passed kernel handles.
pub const FIRST_PASSED_FD: RawDescriptor = 3;

#[derive(Debug, thiserror::Error)]
pub enum AdoptError {
    #[error("handle {0} is reserved or a standard stream")]
    Reserved(RawDescriptor),
    #[error("handle {fd} isn't open: {source}")]
    NotOpen { fd: RawDescriptor, source: io::Error },
    #[error("handle {fd} isn't a connected local byte pipe: {source}")]
    NotAPipe { fd: RawDescriptor, source: io::Error },
    #[error("handle {fd} isn't a writable disk file")]
    NotWritable { fd: RawDescriptor },
    #[error("handle {fd}: {source}")]
    Io { fd: RawDescriptor, source: io::Error },
}

fn adopt(fd: RawDescriptor) -> Result<OwnedHandle, AdoptError> {
    let raw = fd as *mut std::ffi::c_void;
    if fd < FIRST_PASSED_FD
        || raw == INVALID_HANDLE_VALUE
        || [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
            .iter()
            .any(|kind| {
                // SAFETY: queries a standard handle without assuming ownership or changing it.
                raw == unsafe { GetStdHandle(*kind) }
            })
    {
        return Err(AdoptError::Reserved(fd));
    }
    let mut flags = 0;
    // SAFETY: this only queries whether the given numeric handle is valid; no pointed-to memory is accessed.
    if unsafe { GetHandleInformation(raw, &raw mut flags) } == 0 {
        return Err(AdoptError::NotOpen {
            fd,
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: the runner handed this handle to the app's startup for sole ownership, and it was checked open above.
    let owned = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: owned keeps this valid handle alive while inheritance is cleared for all ordinary child launches.
    if unsafe { SetHandleInformation(owned.as_raw_handle(), HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(AdoptError::Io {
            fd,
            source: io::Error::last_os_error(),
        });
    }
    Ok(owned)
}

/// Take sole ownership of the inherited broker endpoint, refuse the wrong pipe mode/kind and verify its peer user.
///
/// # Errors
/// A reserved/closed handle, invalid native pipe, peer identity mismatch, or inability to clear inheritance.
pub fn adopt_socket(fd: RawDescriptor) -> Result<BrokerStream, AdoptError> {
    let pipe = interprocess::os::windows::named_pipe::local_socket::Stream::try_from(adopt(fd)?).map_err(|error| {
        AdoptError::NotAPipe {
            fd,
            source: error.into(),
        }
    })?;
    let pid = pipe
        .inner()
        .peer_process_id()
        .map_err(|source| AdoptError::Io { fd, source })?;
    teams_platform::windows_security::check_user(pid).map_err(|source| AdoptError::Io { fd, source })?;
    Ok(BrokerStream::NamedPipe(pipe))
}

/// Take sole ownership of a writable disk file used for the view trace; refuse read-only files, pipes and devices.
///
/// # Errors
/// A reserved/closed handle, nonwritable file or inability to clear inheritance.
pub fn adopt_trace(fd: RawDescriptor) -> Result<File, AdoptError> {
    let owned = adopt(fd)?;
    // SAFETY: owned keeps the queried handle alive.
    if unsafe { GetFileType(owned.as_raw_handle()) } != FILE_TYPE_DISK {
        return Err(AdoptError::NotWritable { fd });
    }
    let mut access = windows_sys::Win32::System::WindowsProgramming::PUBLIC_OBJECT_BASIC_INFORMATION::default();
    // SAFETY: access is an aligned buffer for ObjectBasicInformation; the owned handle stays alive throughout.
    let status = unsafe {
        windows_sys::Wdk::Foundation::NtQueryObject(
            owned.as_raw_handle(),
            windows_sys::Wdk::Foundation::ObjectBasicInformation,
            (&raw mut access).cast(),
            u32::try_from(size_of_val(&access)).map_err(|source| AdoptError::Io {
                fd,
                source: io::Error::other(source),
            })?,
            std::ptr::null_mut(),
        )
    };
    if status < 0 {
        return Err(AdoptError::Io {
            fd,
            source: io::Error::other(format!("cannot query handle access (NTSTATUS {status:#x})")),
        });
    }
    if access.GrantedAccess & (FILE_WRITE_DATA | FILE_APPEND_DATA) == 0 {
        return Err(AdoptError::NotWritable { fd });
    }
    Ok(File::from(owned))
}
