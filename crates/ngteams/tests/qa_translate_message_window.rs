//! QA of task 575 (round 1), Linux compositor/input harness: one message translated from its menu, in the app's
//! window over the mock and on an own server.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_translate_message_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_translate_message_window: Linux compositor harness");
}
