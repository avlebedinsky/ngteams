//! Embed the native Windows executable's icon; no resource tool runs for other targets.
#[path = "../../build/windows-icon.rs"]
mod windows_icon;

fn main() -> std::process::ExitCode {
    windows_icon::build("ngteams-webview")
}
