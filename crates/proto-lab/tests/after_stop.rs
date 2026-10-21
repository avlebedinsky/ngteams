//! After a stop, `proto-lab` ends without waiting for a blocking task its dropped command left running (an unzip, a
//! checksum): dropping the runtime would wait for it, with every later signal caught. A test binary of its own, since
//! it signals its own process and another test listening for signals would hear it.

use std::time::{Duration, Instant};

use proto_lab::stop::{Stopped, after_stop, until_stopped};
use rustix::process::{Signal, getpid, kill_process};

#[test]
fn a_stop_ends_the_runtime_without_waiting_for_a_blocking_task() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let command = async { tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_secs(60))).await };
    let signaller = std::thread::spawn(|| {
        // the listeners are in place once the command is polled
        std::thread::sleep(Duration::from_millis(300));
        kill_process(getpid(), Signal::TERM)
    });
    let outcome = runtime.block_on(until_stopped(command));
    signaller.join().map_err(|_| "the signalling thread panicked")??;
    let Err(stopped) = outcome else {
        return Err("the blocking task ended before the signal".into());
    };
    assert_eq!(stopped, Stopped::Terminate);

    let started = Instant::now();
    let code = after_stop(runtime, stopped, Duration::from_secs(1));

    assert_eq!(code, 143);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the stop waited for the blocking task: {:?}",
        started.elapsed()
    );
    Ok(())
}
