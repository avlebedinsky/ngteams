//! Linux compositor/input harness: an own-server group call in the app's call window. Cross-platform view/model
//! tests run separately.
#[cfg(target_os = "linux")]
#[path = "support/linux/own_group_call_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("own_group_call_window: Linux compositor harness");
}
