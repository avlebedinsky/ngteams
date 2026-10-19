//! Watching a process die, for the tests that check a browser (or its helpers) is gone. Included with
//! `#[path = "common/process.rs"] mod process;` by the test crates that use it.

use std::time::Duration;

/// Whether a process is still running (a zombie waiting for its parent counts as gone).
pub fn running(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(')')
            .is_some_and(|(_, rest)| !rest.trim_start().starts_with('Z'))
    })
}

/// Waits up to 5 s for a process to be gone.
pub async fn gone(pid: u32) -> bool {
    for _ in 0..100 {
        if !running(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}
