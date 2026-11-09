//! Adopt broker and trace descriptors before opening any other resources. The runner transfers sole ownership;
//! adoption validates the native object and prevents ordinary children (including sign-in workers) inheriting it.
#[cfg(unix)]
#[path = "descriptors/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "descriptors/windows.rs"]
mod platform;

pub use platform::*;
