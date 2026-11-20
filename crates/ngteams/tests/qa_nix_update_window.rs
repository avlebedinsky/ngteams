//! QA of task 580 in the UI sandbox: a copy of each Nix install kind sees a newer release in the app's window.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_nix_update_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_nix_update_window: Linux compositor harness");
}
