//! Real native descriptor adoption: ownership, refusal, byte I/O and child inheritance.
#![cfg(test)]

#[cfg(unix)]
#[path = "support/descriptors_unix.rs"]
mod unix;
#[cfg(windows)]
#[path = "../../teams-auth/tests/support/app_descriptors_windows.rs"]
mod windows;

// The same native contract also runs in teams-auth's lighter display-free gate.
#[cfg(windows)]
use ngteams::descriptors;
