#[cfg(windows)]
fn main() {}

#[cfg(unix)]
#[path = "support/engine_wait.rs"]
mod linux;
#[cfg(unix)]
fn main() -> std::process::ExitCode {
    linux::main()
}
