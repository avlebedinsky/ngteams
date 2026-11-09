//! Windows ACL handling adapted from ngterm's MIT-licensed `owner_only.rs` (copyright 2026 Nick Gluschenko).
//! See LICENSE.ngterm. Creation passes the descriptor directly to Win32, before any sensitive bytes exist.
use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
    SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, GetTokenInformation,
    OBJECT_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    PSID, SECURITY_ATTRIBUTES, TOKEN_INFORMATION_CLASS, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_APPEND_DATA, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::Open;

pub(super) fn create_dir_all(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        create_dir_all(parent)?;
    }
    let descriptor = Descriptor::private(true)?;
    let attributes = descriptor.attributes();
    let name = wide(path.as_os_str())?;
    // SAFETY: the NUL-terminated name, attributes and descriptor remain alive throughout the call.
    if unsafe { CreateDirectoryW(name.as_ptr(), &raw const attributes) } == 0 {
        let error = io::Error::last_os_error();
        // Another creator may have won; existing directories are deliberately left unchanged.
        if error.kind() != io::ErrorKind::AlreadyExists || !path.is_dir() {
            return Err(error);
        }
    }
    Ok(())
}

pub(super) fn open(path: &Path, access: Open) -> io::Result<File> {
    let descriptor = Descriptor::private(false)?;
    let attributes = descriptor.attributes();
    let name = wide(path.as_os_str())?;
    let (desired, disposition) = match access {
        Open::Append => (FILE_APPEND_DATA, OPEN_ALWAYS),
        Open::NewAppend => (FILE_APPEND_DATA, CREATE_NEW),
        Open::NewReadWrite => (GENERIC_READ | GENERIC_WRITE, CREATE_NEW),
        Open::ReadWrite => (GENERIC_READ | GENERIC_WRITE, OPEN_ALWAYS),
    };
    // SAFETY: all pointers are valid for the call. No template is used, and the handle is non-inheritable.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            desired,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &raw const attributes,
            disposition,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW returned a fresh owned handle, transferred exactly once to File.
    Ok(unsafe { File::from_raw_handle(handle) })
}

/// What is set on a path: its DACL, protected from the parent's inheritable grants (and its owner, when the
/// descriptor names one).
const APPLIED: OBJECT_SECURITY_INFORMATION = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;

/// Gives `path` to the current user alone: owner, full access for that user and SYSTEM, no other grant, none
/// inherited. A `directory` passes the same grants on to what is created in it.
///
/// # Errors
///
/// The current user is unknown, or the path's security cannot be set (another user's path, for one).
pub(super) fn restrict(path: &Path, directory: bool) -> io::Result<()> {
    apply(path, &owner_only_sddl(&current_user_sid()?, directory))
}

/// The owner-only descriptor in SDDL: `user`'s and SYSTEM's full access, inheritable by files and subdirectories for
/// a `directory`.
fn owner_only_sddl(user: &str, directory: bool) -> String {
    let inherit = if directory { "OICI" } else { "" };
    format!("O:{user}D:P(A;{inherit};FA;;;SY)(A;{inherit};FA;;;{user})")
}

/// Sets the protected DACL, and the owner when it names one, of the descriptor `sddl` spells on `path`.
fn apply(path: &Path, sddl: &str) -> io::Result<()> {
    let descriptor = Descriptor::from_sddl(sddl)?;
    let (owner, dacl) = descriptor.owner_and_dacl()?;
    let applied = if owner.is_null() {
        APPLIED
    } else {
        APPLIED | OWNER_SECURITY_INFORMATION
    };
    let name = wide(path.as_os_str())?;
    // SAFETY: `name` is a NUL-terminated UTF-16 path; `owner` (null when not set) and `dacl` point into `descriptor`,
    // which outlives the call; the group and the SACL are not set.
    let status =
        unsafe { SetNamedSecurityInfoW(name.as_ptr(), SE_FILE_OBJECT, applied, owner, null_mut(), dacl, null()) };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status.cast_signed()));
    }
    Ok(())
}

/// A self-relative security descriptor the system allocated, freed on drop.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    fn private(directory: bool) -> io::Result<Self> {
        Self::from_sddl(&owner_only_sddl(&current_user_sid()?, directory))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }

    fn from_sddl(sddl: &str) -> io::Result<Self> {
        let text = wide(std::ffi::OsStr::new(sddl))?;
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: `text` is a NUL-terminated UTF-16 string and `descriptor` outlives the call; the size is not asked.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                null_mut(),
            )
        };
        if converted == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    /// The owner SID (null when the descriptor names none) and the DACL, both pointing into the descriptor.
    fn owner_and_dacl(&self) -> io::Result<(PSID, *mut ACL)> {
        let mut owner: PSID = null_mut();
        let mut defaulted = 0;
        // SAFETY: the descriptor is a valid one the system built; `owner` and `defaulted` outlive the call.
        if unsafe { GetSecurityDescriptorOwner(self.0, &raw mut owner, &raw mut defaulted) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut present = 0;
        let mut dacl: *mut ACL = null_mut();
        // SAFETY: as above; `present`, `dacl` and `defaulted` outlive the call.
        if unsafe { GetSecurityDescriptorDacl(self.0, &raw mut present, &raw mut dacl, &raw mut defaulted) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if present == 0 || dacl.is_null() {
            return Err(io::Error::other("the descriptor has no DACL"));
        }
        Ok((owner, dacl))
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the system allocated the descriptor with LocalAlloc, and nothing else frees it.
        unsafe { LocalFree(self.0) };
    }
}

/// The current process's user SID in its string form (`S-1-5-21-…`).
fn current_user_sid() -> io::Result<String> {
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing and is always valid.
    let process = unsafe { GetCurrentProcess() };
    let token = open_token(process)?;
    let user = token_information(&token, TokenUser)?;
    // SAFETY: GetTokenInformation(TokenUser) filled the aligned buffer with a TOKEN_USER at its start.
    let sid = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() }.User.Sid;
    let mut text: *mut u16 = null_mut();
    // SAFETY: `sid` points into `user`, alive across the call, and `text` outlives it.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: ConvertSidToStringSidW returned a NUL-terminated string allocated with LocalAlloc, now ours.
    Ok(unsafe { take_local_string(text) })
}

/// The access token of `process`, opened for queries.
pub(crate) fn open_token(process: HANDLE) -> io::Result<OwnedHandle> {
    let mut raw: HANDLE = null_mut();
    // SAFETY: `process` is a valid process handle for the duration of the call and `raw` outlives it.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcessToken returned a fresh handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// The token's information of `class`, in `usize` storage so the structure at its start is aligned and the SIDs it
/// points to live with it.
pub(crate) fn token_information(token: &OwnedHandle, class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<usize>> {
    let mut needed = 0_u32;
    // SAFETY: a null buffer of length zero only asks for the size, written to `needed`, which outlives the call.
    unsafe { GetTokenInformation(token.as_raw_handle(), class, null_mut(), 0, &raw mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let words = usize::try_from(needed)
        .map_err(io::Error::other)?
        .div_ceil(size_of::<usize>());
    let mut buffer = vec![0_usize; words];
    // SAFETY: the buffer holds at least `needed` bytes, and `needed` outlives the call.
    let filled = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            class,
            buffer.as_mut_ptr().cast::<c_void>(),
            needed,
            &raw mut needed,
        )
    };
    if filled == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buffer)
}

/// Copies out and frees a string the system allocated.
///
/// # Safety
///
/// `text` is a NUL-terminated UTF-16 string allocated with `LocalAlloc` that the caller owns.
unsafe fn take_local_string(text: *mut u16) -> String {
    let mut len = 0;
    // SAFETY: the caller guarantees a NUL terminator, so every unit up to it is in bounds.
    while unsafe { *text.wrapping_add(len) } != 0 {
        len += 1;
    }
    // SAFETY: the `len` units before the terminator were just read.
    let units = unsafe { std::slice::from_raw_parts(text, len) };
    let string = String::from_utf16_lossy(units);
    // SAFETY: the caller hands the allocation over, and nothing reads it after this.
    unsafe { LocalFree(text.cast::<c_void>()) };
    string
}

/// `text` as a NUL-terminated UTF-16 string; one with a NUL of its own is refused.
fn wide(text: &std::ffi::OsStr) -> io::Result<Vec<u16>> {
    let mut units: Vec<u16> = text.encode_wide().collect();
    if units.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "a path with a NUL in it"));
    }
    units.push(0);
    Ok(units)
}
