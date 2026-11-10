//! Platform-specific lab command sets. Keep the Unix entry point at the binary root, as before the Windows port.
#[cfg(unix)]
include!("main_unix.rs");

#[cfg(windows)]
mod main_windows;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    main_windows::main()
}
