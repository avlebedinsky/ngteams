//! Linux compositor/input harness: avatars of odd names in each view, and the ringing banner over an open chat, on the
//! mock and on the own server. Cross-platform view/model tests run separately.
#[cfg(target_os = "linux")]
#[path = "support/linux/initials_ring_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("initials_ring_window: Linux compositor harness");
}
