//! Compile the application's real descriptor adoption on both platforms. All endpoints and files are synthetic.
#![cfg(test)]
#[path = "../../ngteams/src/descriptors.rs"]
pub mod descriptors;
#[path = "../../ngteams/src/restart.rs"]
pub mod restart;

#[cfg(windows)]
#[path = "support/app_descriptors_windows.rs"]
mod windows;

#[cfg(windows)]
#[path = "support/app_restarts_windows.rs"]
mod restarts;
