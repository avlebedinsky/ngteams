//! QA of task 660 (round 1), Linux compositor/input harness: "Mark as unread" from a message's menu and from a chat
//! row's menu, in the app's window over the mock.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_mark_unread_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_mark_unread_window: Linux compositor harness");
}
