//! QA of task 232: Manage apps as a real administrator uses it, against the real `ngteams-server` binary, in the UI
//! sandbox. Linux compositor/input harness; the view tests run in `teams-ui`.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_apps_admin_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_apps_admin_window: Linux compositor harness");
}
