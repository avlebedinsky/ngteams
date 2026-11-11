//! Synthetic private-worker conversations exercise parent decisions without opening a page or contacting a tenant.
#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
#[path = "support/native_protocol.rs"]
mod native;
#[cfg(windows)]
fn main() -> std::process::ExitCode {
    native::main()
}
