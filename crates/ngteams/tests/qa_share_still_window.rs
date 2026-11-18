//! A still screen shared in a 1:1 call, shared again, a window, and the call's media set up again while it is shared:
//! the far end's window shows each share. Linux compositor/input harness (see the support module's docs).
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_share_still_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_share_still_window: Linux compositor harness");
}
