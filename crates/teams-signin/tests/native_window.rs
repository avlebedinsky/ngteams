//! Offline native Windows sign-in contract. Only numerical loopback and synthetic page data are used.
#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
#[path = "support/native_window.rs"]
mod native;
#[cfg(windows)]
fn main() -> std::process::ExitCode {
    native::main()
}
