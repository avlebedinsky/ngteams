//! A name the OS accepts but SQLite cannot express as UTF-8.
use std::ffi::OsString;

pub fn name(prefix: &str, suffix: &str) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        let mut bytes = prefix.as_bytes().to_vec();
        bytes.push(0xff);
        bytes.extend_from_slice(suffix.as_bytes());
        OsString::from_vec(bytes)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt as _;
        let mut units: Vec<u16> = prefix.encode_utf16().collect();
        units.push(0xd800);
        units.extend(suffix.encode_utf16());
        OsString::from_wide(&units)
    }
}
