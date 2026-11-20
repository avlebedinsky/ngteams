//! QA of task 664 (round 1): the meeting notices as a user meets them, in the app's window and on the sandbox's
//! notification bus: the calling service's push of a meeting's start (`evt` 128), the app started after a meeting's
//! start, and the calendar's meeting reminders. Linux compositor harness (`harness = false`: GPUI runs on the
//! process's main thread).
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_meeting_notice_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_meeting_notice_window: Linux compositor harness");
}
