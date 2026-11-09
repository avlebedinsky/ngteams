//! The native identity round trip changes only this synthetic test process, without a window or shell registration.
#![cfg(all(test, windows))]
use teams_platform::windows_desktop::set_app_id;
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::GetCurrentProcessExplicitAppUserModelID;

#[test]
fn the_process_receives_the_exact_identity_and_invalid_ids_are_refused() {
    let identity = "com.nickalie.ngteams.synthetic";
    set_app_id(identity).unwrap();
    let mut value = std::ptr::null_mut();
    // SAFETY: value is a writable output; the API allocates a NUL-terminated string on success.
    assert_eq!(unsafe { GetCurrentProcessExplicitAppUserModelID(&raw mut value) }, 0);
    assert!(!value.is_null());
    let mut length = 0;
    // SAFETY: the API returned a NUL-terminated string; the scan ends at its terminator.
    while unsafe { *value.wrapping_add(length) } != 0 {
        length += 1;
    }
    // SAFETY: the preceding scan established the allocation's live UTF-16 string range.
    let actual = String::from_utf16(unsafe { std::slice::from_raw_parts(value, length) }).unwrap();
    // SAFETY: this API's allocation is released exactly once using its matching allocator.
    unsafe {
        CoTaskMemFree(value.cast());
    }
    assert_eq!(actual, identity);
    for invalid in [String::new(), "nul\0id".to_owned(), "x".repeat(129)] {
        assert_eq!(
            set_app_id(&invalid).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
}
