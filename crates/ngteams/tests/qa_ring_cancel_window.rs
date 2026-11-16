//! QA of task 371 (round 1): the callee's window when the caller cancels a ring before, at and just after the moment
//! it starts to ring, for a Microsoft account on the mock and for an own server's account on the real binary. Linux
//! compositor harness (`harness = false`: GPUI runs on the process's main thread).
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_ring_cancel_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_ring_cancel_window: Linux compositor harness");
}
