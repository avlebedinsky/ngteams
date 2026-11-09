//! Every supervised process starts inside its job. Cleanup waits for the job to empty before the caller's locks drop.
use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};
use teams_platform::windows_job::Job;
use teams_platform::windows_process::{Child, Transfer};

pub struct Tree(pub Job);
impl Tree {
    pub fn new() -> io::Result<Self> {
        Job::new().map(Self)
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        // Retain the owning runner (and its locks) if the OS cannot complete cleanup. The outer scheduled-task
        // deadline still kills the runner, closing the last job handle. Never announce a successful early unlock.
        let _ = self.0.terminate(1);
        while self.0.active_processes().ok() != Some(0) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

pub fn completed(child: &Child, timeout: Duration) -> io::Result<u32> {
    child
        .wait(timeout)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "process deadline expired"))
}

pub fn command(program: &Path, args: &[OsString], timeout: Duration) -> io::Result<u32> {
    let tree = Tree::new()?;
    let child = Transfer::new(&[])?.spawn_in_job(program, args, &tree.0)?;
    completed(&child, timeout)
}

pub fn observe(app: &Tree, broker: &Child, stop: &Path, timeout: Duration) -> io::Result<u8> {
    let deadline = Instant::now() + timeout;
    loop {
        if app.0.active_processes()? == 0 {
            return Ok(0);
        }
        if broker.wait(Duration::ZERO)?.is_some() {
            return Err(io::Error::other("credential broker exited while the app was running"));
        }
        if stop.try_exists()? {
            return Ok(0);
        }
        if Instant::now() >= deadline {
            return Ok(124);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
