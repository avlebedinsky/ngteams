//! QA of task 236 in the UI sandbox: chat in the app's window on a SQLite own server while the operator runs commands.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_sqlite_busy_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_sqlite_busy_window: Linux compositor harness");
}
