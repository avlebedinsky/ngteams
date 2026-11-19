//! QA of task 584 in the UI sandbox: the message box's toolbar with one AI button, and the buttons that the user can
//! hide, in every box, in Settings and after a restart.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_compose_toolbar_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_compose_toolbar_window: Linux compositor harness");
}
