//! Shared synthetic scene on a private Linux compositor.
#[cfg(target_os = "linux")]
#[path = "support/scenes/ai_providers_window.rs"]
mod scene;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    scene::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("ai_providers_window: Linux compositor harness");
}
