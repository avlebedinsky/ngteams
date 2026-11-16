//! Linux compositor/input harness: a bot's cards and buttons, as a user sees and uses them in the app's window.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_bot_cards_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_bot_cards_window: Linux compositor harness; the teams-ui view tests cover the cards elsewhere");
}
