//! Synthetic BGRA rendering and bounded atlas lifetime in the authorized Windows VM console.
#![cfg(test)]
#[cfg(windows)]
#[path = "support/native_video.rs"]
mod native;
#[cfg(windows)]
#[path = "../src/video_surface.rs"]
mod video_surface;

#[cfg(not(windows))]
fn main() {}
#[cfg(windows)]
fn main() -> std::process::ExitCode {
    native::main()
}
