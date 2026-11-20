//! Linux compositor/input harness: "Share this file" on a file card, as a user opens and uses it in the app's window.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_share_file_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_share_file_window: Linux compositor harness; the teams-ui view tests cover the dialog elsewhere");
}
