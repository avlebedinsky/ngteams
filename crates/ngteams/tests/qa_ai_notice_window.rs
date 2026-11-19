//! QA of task 579 (round 1), Linux compositor/input harness: the AI notice and the "Use AI features" switch in the
//! app's window over the mock.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_ai_notice_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_ai_notice_window: Linux compositor harness");
}
