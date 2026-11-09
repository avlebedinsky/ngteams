//! A command owns its whole process tree. Unix children start in a new process group; Windows children start
//! suspended, enter a private kill-on-close job, then resume. No descendant can escape between launch and attach.
//! `std::process::Command` retains its argument escaping (including Windows npm .cmd shims) and pipe handling.

use std::io;
use std::ops::{Deref, DerefMut};
use std::process::{Child, Command};

#[derive(Debug)]
pub struct Tree {
    child: Child,
    stopped: bool,
    #[cfg(windows)]
    job: crate::windows_job::Job,
}

impl Tree {
    /// Spawn `command` with a private process tree. Dropping it kills and reaps it, including surviving descendants.
    ///
    /// # Errors
    /// Launching, attaching to the native job or resuming its initial thread fails.
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command
                .process_group(0)
                .spawn()
                .map(|child| Self { child, stopped: false })
        }
        #[cfg(windows)]
        {
            let job = crate::windows_job::Job::new()?;
            let child = super::windows_command::spawn(command, &job)?;
            Ok(Self {
                child,
                stopped: false,
                job,
            })
        }
    }

    /// Terminate and reap this command and all its descendants, even after the initial child exits.
    pub fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        #[cfg(unix)]
        {
            let group = rustix::process::Pid::from_child(&self.child);
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        #[cfg(windows)]
        let _ = self.job.terminate(1);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Deref for Tree {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}

impl DerefMut for Tree {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        self.stop();
    }
}
