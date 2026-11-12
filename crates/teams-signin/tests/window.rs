#[cfg(windows)]
fn main() {}

#[cfg(unix)]
#[path = "support/window.rs"]
mod linux;
#[cfg(unix)]
fn main() -> std::process::ExitCode {
    linux::main()
}
