//! Account-free Windows shell contract, run only by the authorized VM console helper.
#![cfg(test)]
#[cfg(windows)]
#[path = "support/native_desktop.rs"]
mod native;
#[cfg(windows)]
mod notify {
    pub use teams_ui::notify::*;
}
#[cfg(windows)]
mod tray {
    pub use teams_ui::tray::*;
}
#[cfg(windows)]
mod app_icon {
    pub use teams_ui::app_icon::*;
}
#[cfg(windows)]
fn main() -> std::process::ExitCode {
    native::main()
}
#[cfg(not(windows))]
fn main() {}
