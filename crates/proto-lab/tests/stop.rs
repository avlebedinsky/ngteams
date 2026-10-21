//! A command stopped by SIGINT or SIGTERM is dropped, and with it what it owns (the lab browsers), before `proto-lab`
//! exits. This test binary signals itself: its own process, with the signals taken over by `until_stopped`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use proto_lab::stop::{Stopped, until_stopped};
use rustix::process::{Signal, getpid, kill_process};

/// Sets its flag when dropped: what a command owns.
struct Owned(Arc<AtomicBool>);

impl Drop for Owned {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn a_signal_drops_the_command_and_what_it_owns() -> Result<(), Box<dyn std::error::Error>> {
    for (signal, name) in [(Signal::TERM, "SIGTERM"), (Signal::INT, "SIGINT")] {
        let dropped = Arc::new(AtomicBool::new(false));
        let owned = Owned(Arc::clone(&dropped));
        let command = async move {
            let _owned = owned;
            std::future::pending::<()>().await;
        };
        let stopping = tokio::spawn(until_stopped(command));
        // the listeners are in place once the command is polled
        tokio::time::sleep(Duration::from_millis(300)).await;
        kill_process(getpid(), signal)?;
        let outcome = tokio::time::timeout(Duration::from_secs(10), stopping).await??;
        assert_eq!(outcome, Err(Stopped(name)));
        assert!(
            dropped.load(Ordering::SeqCst),
            "{name}: the command's own value was dropped"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_command_that_ends_gives_its_outcome() {
    assert_eq!(until_stopped(async { 7 }).await, Ok(7));
}
