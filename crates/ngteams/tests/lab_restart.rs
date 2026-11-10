//! Real native self-restart preserves only explicitly transferred descriptors.
#![cfg(test)]
#[cfg(unix)]
#[path = "support/restart_unix.rs"]
mod unix;
#[cfg(windows)]
use ngteams::{descriptors, restart};
#[cfg(windows)]
#[path = "../../teams-auth/tests/support/app_restarts_windows.rs"]
mod restarts;
