//! Verify the opened file, rather than trusting path attributes or repairing unsafe input.
use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle as _;
use std::ptr::{addr_of_mut, null_mut};

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, GetSecurityInfo, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce, INHERIT_ONLY_ACE,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
};

use super::platform::{current_user_sid, take_local_string};

struct Descriptor(PSECURITY_DESCRIPTOR);
impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: GetSecurityInfo allocated this descriptor, owned by this guard only.
        unsafe { LocalFree(self.0) };
    }
}

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "file must be owned by this user and grant access only to this user and SYSTEM",
    )
}

fn sid_text(sid: PSID) -> io::Result<String> {
    if sid.is_null() {
        return Err(denied());
    }
    let mut text = null_mut();
    // SAFETY: sid points into the live security descriptor. The output owns a new LocalAlloc buffer.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful call returns a NUL-terminated LocalAlloc string transferred to this helper.
    Ok(unsafe { take_local_string(text) })
}

pub(super) fn check_file(file: &File) -> io::Result<()> {
    if !file.metadata()?.is_file() {
        return Err(denied());
    }
    let mut descriptor = null_mut();
    let mut owner = null_mut();
    let mut dacl: *mut ACL = null_mut();
    // SAFETY: the file owns a valid handle; all output pointers outlive the call.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &raw mut owner,
            null_mut(),
            &raw mut dacl,
            null_mut(),
            &raw mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status.cast_signed()));
    }
    let _descriptor = Descriptor(descriptor);
    let user = current_user_sid()?;
    if sid_text(owner)? != user || dacl.is_null() {
        return Err(denied());
    }
    // SAFETY: GetSecurityInfo returned a valid ACL in the live descriptor.
    for index in 0..u32::from(unsafe { (*dacl).AceCount }) {
        let mut ace: *mut c_void = null_mut();
        // SAFETY: index is below AceCount; the output pointer outlives this call.
        if unsafe { GetAce(dacl, index, &raw mut ace) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: each ACL entry begins with ACE_HEADER.
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0 {
            continue;
        }
        // Only ordinary allow entries are understood. Reject unfamiliar/object/callback ACEs, even if benign.
        if header.AceType != 0 {
            return Err(denied());
        }
        // SAFETY: AceType 0 is ACCESS_ALLOWED_ACE, with its SID beginning at SidStart.
        let sid = unsafe { addr_of_mut!((*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart).cast() };
        let principal = sid_text(sid)?;
        if principal != user && principal != "S-1-5-18" {
            return Err(denied());
        }
    }
    Ok(())
}
