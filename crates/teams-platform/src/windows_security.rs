//! Current-user identity and pipe protection. Private pipes reject lower-integrity processes and their clients
//! verify the serving process before sending data: a derived pipe name alone never authenticates its owner.
//! Identity checks follow ngterm's MIT-licensed pipe_security.rs (copyright 2026 Nick Gluschenko); see LICENSE.ngterm.
use std::io;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

use windows_sys::Win32::Security::{EqualSid, TOKEN_ELEVATION, TOKEN_USER, TokenElevation, TokenUser};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

use crate::private::platform::{open_token, token_information};

/// The current process's user SID in its string form.
///
/// # Errors
/// The process token cannot be queried or its SID cannot be converted.
pub fn current_user_sid() -> io::Result<String> {
    crate::private::platform::current_user_sid()
}

/// A protected pipe descriptor granting only SYSTEM and the exact current user access, at the server's integrity.
/// Uses the user SID explicitly: an elevated token's default owner may be the Administrators group.
///
/// # Errors
/// The current user's SID or process elevation cannot be queried.
pub fn pipe_sddl() -> io::Result<String> {
    // SAFETY: the pseudo-handle is always valid and must not be closed.
    let token = open_token(unsafe { GetCurrentProcess() })?;
    let elevation = token_information(&token, TokenElevation)?;
    // SAFETY: token_information returned an aligned buffer containing TOKEN_ELEVATION.
    let elevated = unsafe { &*elevation.as_ptr().cast::<TOKEN_ELEVATION>() }.TokenIsElevated != 0;
    let level = if elevated { "HI" } else { "ME" };
    let user = current_user_sid()?;
    Ok(format!("O:{user}D:P(A;;GA;;;SY)(A;;GA;;;{user})S:(ML;;NRNW;;;{level})"))
}

/// Refuse a peer process running as another user. A pipe client must do this before sending any request.
///
/// # Errors
/// `PermissionDenied` for another user, or the process/token query error (including an exited peer).
pub fn check_user(pid: u32) -> io::Result<()> {
    // SAFETY: no pointers; a null result is checked before ownership is assumed.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcess returned a fresh handle, now owned exactly once.
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let peer = token_information(&open_token(process.as_raw_handle())?, TokenUser)?;
    // SAFETY: GetCurrentProcess's pseudo-handle remains valid; no ownership is assumed.
    let ours = token_information(&open_token(unsafe { GetCurrentProcess() })?, TokenUser)?;
    // SAFETY: each aligned buffer contains TOKEN_USER and keeps its SID alive through EqualSid.
    let peer_sid = unsafe { &*peer.as_ptr().cast::<TOKEN_USER>() }.User.Sid;
    // SAFETY: as above, for the current user's buffer.
    let own_sid = unsafe { &*ours.as_ptr().cast::<TOKEN_USER>() }.User.Sid;
    // SAFETY: both SIDs point into the live buffers above.
    let same = unsafe { EqualSid(peer_sid, own_sid) } != 0;
    if same {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "the local pipe peer runs as another user",
    ))
}
