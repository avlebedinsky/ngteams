//! Account-free real GPUI desktop lifecycle in the authorized Windows VM console.
#![cfg(test)]
#![cfg_attr(windows, windows_subsystem = "windows")]
#[cfg(windows)]
#[expect(
    dead_code,
    reason = "the shared fake engine supplies more operations than this native fixture uses"
)]
mod common;
#[cfg(windows)]
#[path = "support/native_shell_driver.rs"]
mod driver;
#[cfg(windows)]
#[path = "support/native_shell.rs"]
mod native;
#[cfg(windows)]
fn main() -> std::process::ExitCode {
    if std::env::args().any(|arg| arg == "--drive-tray") {
        return driver::main();
    }
    native::main()
}
#[cfg(not(windows))]
fn main() {}
