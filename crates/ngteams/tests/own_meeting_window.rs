//! Linux compositor/input harness: an own server's meetings in the app's windows (QA of task 298). Cross-platform
//! view/model tests run separately.
#[cfg(target_os = "linux")]
#[path = "support/linux/own_meeting_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("own_meeting_window: Linux compositor harness");
}
