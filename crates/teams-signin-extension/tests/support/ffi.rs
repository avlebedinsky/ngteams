//! Real `GLib` values exercise the initialization-data and C-string boundaries.

use std::ptr;

use glib_sys::GVariant;

use super::{socket_path, text};

struct Variant(*mut GVariant);

impl Drop for Variant {
    fn drop(&mut self) {
        // SAFETY: each test constructor returns one owned, non-null GVariant reference.
        unsafe { glib_sys::g_variant_unref(self.0) };
    }
}

#[test]
fn initialization_data_must_be_a_string_and_preserves_the_socket_path() {
    // SAFETY: null is explicitly accepted by socket_path.
    assert_eq!(unsafe { socket_path(ptr::null_mut()) }, None);
    // SAFETY: the constructor takes an integer and returns an owned variant.
    let number = Variant(unsafe { glib_sys::g_variant_new_uint32(7) });
    // SAFETY: the variant is live for this call.
    assert_eq!(unsafe { socket_path(number.0) }, None);
    // SAFETY: the constructor copies the static NUL-terminated string.
    let path = Variant(unsafe { glib_sys::g_variant_new_string(c"/tmp/synthetic-guard.sock".as_ptr()) });
    // SAFETY: the variant remains alive while socket_path copies its string.
    let copied = unsafe { socket_path(path.0) };
    drop(path);
    assert_eq!(copied.as_deref(), Some("/tmp/synthetic-guard.sock"));
    // SAFETY: the constructor copies this static, empty C string.
    let empty = Variant(unsafe { glib_sys::g_variant_new_string(c"".as_ptr()) });
    // SAFETY: the variant is live for this call.
    assert_eq!(unsafe { socket_path(empty.0) }.as_deref(), Some(""));
}

#[test]
fn c_strings_are_copied_and_null_or_invalid_utf8_stays_absent() {
    // SAFETY: null is explicitly accepted by text.
    assert_eq!(unsafe { text(ptr::null()) }, None);
    // SAFETY: these byte arrays are NUL-terminated and live for each call.
    assert_eq!(unsafe { text(c"\xff".as_ptr()) }, None);
    // SAFETY: this static C string is valid and NUL-terminated.
    assert_eq!(unsafe { text(c"".as_ptr()) }.as_deref(), Some(""));
    // SAFETY: this static C string is valid and NUL-terminated.
    let unicode = unsafe { text(c"синтетический".as_ptr()) };
    assert_eq!(unicode.as_deref(), Some("синтетический"));
}
