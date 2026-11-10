//! Real calendar layout in the private Linux compositor; cross-platform bounds checks live in `calendar.rs`.
#[cfg(target_os = "linux")]
#[path = "support/linux/calendar_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("calendar_window: Linux compositor harness; use the calendar bounds tests on Windows");
}
