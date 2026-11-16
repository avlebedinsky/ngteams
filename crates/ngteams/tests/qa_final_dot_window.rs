//! QA of task 406 in the UI sandbox: the AI correction keeps a missing final dot missing.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_final_dot_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_final_dot_window: Linux compositor harness");
}
