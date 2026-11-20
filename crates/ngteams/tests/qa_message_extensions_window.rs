//! Linux compositor/input harness: message extensions and apps' dialogs, as a user sees and uses them in the app's
//! window.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_message_extensions_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_message_extensions_window: Linux compositor harness; the teams-ui view tests cover it elsewhere");
}
