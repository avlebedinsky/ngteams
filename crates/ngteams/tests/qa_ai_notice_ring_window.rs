//! QA of task 579 (round 2): a call rings while the AI notice waits, in the app's window over the mock. Linux
//! compositor harness (`harness = false`: GPUI runs on the process's main thread).
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_ai_notice_ring_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_ai_notice_ring_window: Linux compositor harness");
}
