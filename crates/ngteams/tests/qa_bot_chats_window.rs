//! Linux compositor/input harness: bot chats and bot mentions, as a user sees and uses them in the app's window.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_bot_chats_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_bot_chats_window: Linux compositor harness; the teams-ui view tests cover bot chats elsewhere");
}
