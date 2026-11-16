//! QA of task 407 in the UI sandbox: a new chat not made yet, and its draft, stay after the user opens another chat.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_new_chat_kept_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_new_chat_kept_window: Linux compositor harness");
}
