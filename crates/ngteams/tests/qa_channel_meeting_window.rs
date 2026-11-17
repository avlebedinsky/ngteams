//! QA of task 420 (round 1): a channel meeting's title, its post's Join and the post once it ended, for a Microsoft
//! account on the mock. Linux compositor harness (`harness = false`: GPUI runs on the process's main thread).
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_channel_meeting_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_channel_meeting_window: Linux compositor harness");
}
