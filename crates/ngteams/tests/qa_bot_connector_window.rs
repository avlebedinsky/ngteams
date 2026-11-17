//! Linux compositor/input harness: a user of an own server talks to a bot in the app's window, through the real
//! `ngteams-server` binary and a test bot on its connector routes.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_bot_connector_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_bot_connector_window: Linux compositor harness; the own server runs on Linux only");
}
