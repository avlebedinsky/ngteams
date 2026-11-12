//! The explicit Windows process identity shared by the main app and its sign-in worker, before either opens a HWND.
use std::io;
use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

/// Set the taskbar grouping/notification identity before the first window opens.
///
/// # Errors
/// The identity contains NUL, is empty/too long, or the native call fails.
pub fn set_app_id(app_id: &str) -> io::Result<()> {
    let mut wide: Vec<u16> = app_id.encode_utf16().collect();
    if wide.is_empty() || wide.len() > 128 || wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Windows app identity",
        ));
    }
    wide.push(0);
    // SAFETY: wide is a NUL-terminated application identity that outlives the call.
    let result = unsafe { SetCurrentProcessExplicitAppUserModelID(wide.as_ptr()) };
    if result < 0 {
        return Err(io::Error::other(format!("Windows app identity failed ({result:#x})")));
    }
    Ok(())
}
