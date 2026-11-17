//! QA of task 523 (round 1): a channel post that came by email, as a user meets it in the app's window, for a
//! Microsoft account on the mock. Linux compositor harness (`harness = false`: GPUI runs on the process's main thread).
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_email_post_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_email_post_window: Linux compositor harness");
}
