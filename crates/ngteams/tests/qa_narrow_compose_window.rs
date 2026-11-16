//! QA of task 429 in the UI sandbox: the message box stays usable in a narrow call chat, a narrow window, a thread's
//! pane and a channel's box.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_narrow_compose_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_narrow_compose_window: Linux compositor harness");
}
