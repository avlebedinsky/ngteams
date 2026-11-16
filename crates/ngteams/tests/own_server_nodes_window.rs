//! Linux compositor/input harness: ngteams apps on their own server's two API nodes and media node, each app in a
//! UI sandbox of its own.
#[cfg(target_os = "linux")]
#[path = "support/linux/own_server_nodes_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("own_server_nodes_window: Linux compositor harness");
}
