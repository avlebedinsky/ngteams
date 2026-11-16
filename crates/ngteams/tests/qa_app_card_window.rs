//! Linux compositor/input harness: a YouTube link's app card, as a user sees and clicks it in the app's window.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_app_card_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_app_card_window: Linux compositor harness; the teams-ui view tests cover the card elsewhere");
}
