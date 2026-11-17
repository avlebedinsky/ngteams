//! Linux compositor/input harness: a message with several files, its grid of file cards and their actions, as a user
//! sees and uses them in the app's window.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_file_grid_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_file_grid_window: Linux compositor harness; the teams-ui view tests cover the file cards elsewhere");
}
