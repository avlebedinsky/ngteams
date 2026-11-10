//! Linux compositor/input harness. Cross-platform view/model tests run separately; Windows console integration
//! is exercised by the native desktop, shell and video fixtures.
#[cfg(target_os = "linux")]
#[path = "support/linux/ui_tour.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("ui_tour: Linux compositor harness; use the native Windows console fixtures");
}
