//! Linux compositor/input harness: Teams stickers that people send, as a user sees them in the app's window.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_stickers_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_stickers_window: Linux compositor harness; the teams-ui view tests cover stickers elsewhere");
}
