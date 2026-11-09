//! Windows sign-in worker. Its only protocol is the private standard streams supplied by the parent.
#![cfg_attr(windows, windows_subsystem = "windows")]
#[cfg(windows)]
fn main() {
    teams_signin::window::worker();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("ngteams-webview is the Windows sign-in worker");
    std::process::exit(2);
}
