//! Shared synthetic scene on a private Linux compositor.
#[cfg(target_os = "linux")]
#[path = "support/scenes/app_card_window.rs"]
mod scene;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    scene::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("app_card_window: Linux compositor harness");
}
