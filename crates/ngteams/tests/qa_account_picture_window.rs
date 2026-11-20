//! QA of task 670 in the UI sandbox: a picture of the user's own for each account.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_account_picture_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_account_picture_window: Linux compositor harness");
}
