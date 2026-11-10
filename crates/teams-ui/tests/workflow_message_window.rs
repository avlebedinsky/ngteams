//! Shared synthetic scene on a private Linux compositor or the authorized Windows VM console.
#[cfg(any(target_os = "linux", windows))]
#[path = "support/scenes/workflow_message_window.rs"]
mod scene;

#[cfg(any(target_os = "linux", windows))]
fn main() -> std::process::ExitCode {
    scene::main()
}

#[cfg(not(any(target_os = "linux", windows)))]
fn main() {
    eprintln!("workflow_message_window: Linux compositor harness; use the native Windows console fixtures");
}
