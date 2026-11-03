//! Disposable subprocesses verify lab self-exec without exposing the broker to ordinary child processes.

#![cfg(test)]

use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::Command;

use ngteams::descriptors::{adopt_socket, adopt_trace};
use ngteams::restart::RestartDescriptors;
use rustix::io::{FdFlags, fcntl_getfd};

const STAGE: &str = "NGTEAMS_RESTART_TEST_STAGE";
const SOCKET: &str = "NGTEAMS_RESTART_TEST_SOCKET";
const TRACE: &str = "NGTEAMS_RESTART_TEST_TRACE";
const SOCKET_LINK: &str = "NGTEAMS_RESTART_TEST_SOCKET_LINK";
const TRACE_LINK: &str = "NGTEAMS_RESTART_TEST_TRACE_LINK";

#[test]
fn lab_descriptors_survive_only_the_explicit_self_exec_and_failed_exec_restores_them() {
    for with_trace in [false, true] {
        check_restart(with_trace);
    }
}

fn check_restart(with_trace: bool) {
    let (socket, mut peer) = UnixStream::pair().unwrap();
    let trace = tempfile::NamedTempFile::new().unwrap();
    let (socket_fd, trace_fd) = (socket.as_raw_fd(), if with_trace { trace.as_raw_fd() } else { -1 });
    let mut child = child_command("before", socket_fd, trace_fd);
    // SAFETY: after fork this closure only changes descriptor flags with the async-signal-safe fcntl call.
    // The parent keeps both descriptors open until spawn returns; no Rust-owned values are dropped in the closure.
    unsafe {
        child.pre_exec(move || {
            for fd in [socket_fd, trace_fd] {
                inherit_passed_fd(fd)?;
            }
            Ok(())
        });
    }
    let output = child.output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    drop(socket);
    let mut answered = String::new();
    peer.read_to_string(&mut answered).unwrap();
    assert_eq!(answered, "broker survived self-exec");
    assert_eq!(
        fs::read_to_string(trace.path()).unwrap(),
        if with_trace { "trace survived self-exec" } else { "" }
    );
}

fn inherit_passed_fd(fd: RawFd) -> std::io::Result<()> {
    if fd < 0 {
        return Ok(());
    }
    // SAFETY: this only updates flags on a descriptor the parent keeps open through spawn.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, 0) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn child_command(stage: &str, socket: RawFd, trace: RawFd) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "restart_child", "--nocapture", "--test-threads", "1"])
        .env(STAGE, stage)
        .env(SOCKET, socket.to_string())
        .env(TRACE, trace.to_string());
    command
}

fn fd_link(fd: RawFd) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{fd}"))
}

fn close_on_exec(fd: &impl std::os::fd::AsFd) {
    assert!(fcntl_getfd(fd).unwrap().contains(FdFlags::CLOEXEC));
}

#[test]
fn restart_child() {
    let Ok(stage) = std::env::var(STAGE) else {
        return;
    };
    let socket_fd = std::env::var(SOCKET).unwrap().parse::<RawFd>().unwrap();
    let trace_fd = std::env::var(TRACE).unwrap().parse::<RawFd>().unwrap();
    if stage == "ordinary" {
        for (fd, expected) in [(socket_fd, SOCKET_LINK), (trace_fd, TRACE_LINK)] {
            if fd < 0 {
                continue;
            }
            let original = PathBuf::from(std::env::var_os(expected).unwrap());
            assert_ne!(fs::read_link(fd_link(fd)).ok(), Some(original));
        }
        return;
    }
    let mut socket = adopt_socket(socket_fd).unwrap();
    let mut trace = (trace_fd >= 0).then(|| adopt_trace(trace_fd).unwrap());
    close_on_exec(&socket);
    if let Some(trace) = &trace {
        close_on_exec(trace);
    }
    if stage == "after" {
        socket.write_all(b"broker survived self-exec").unwrap();
        if let Some(trace) = &mut trace {
            trace.write_all(b"trace survived self-exec").unwrap();
        }
        return;
    }
    let originals = RestartDescriptors::new(socket, trace);
    let cloned_socket = originals.socket().unwrap();
    let cloned_trace: Option<File> = originals.trace().unwrap();
    close_on_exec(&cloned_socket);
    if let Some(trace) = &cloned_trace {
        close_on_exec(trace);
        assert_ne!(trace.as_raw_fd(), trace_fd);
    }
    assert_ne!(cloned_socket.as_raw_fd(), socket_fd);
    let error = originals.exec(&mut Command::new("/ngteams-synthetic-missing-restart-executable"));
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    close_on_exec(&originals.socket().unwrap());
    if let Some(trace) = originals.trace().unwrap() {
        close_on_exec(&trace);
    }
    let mut ordinary = child_command("ordinary", socket_fd, trace_fd);
    ordinary.env(SOCKET_LINK, fs::read_link(fd_link(socket_fd)).unwrap());
    if trace_fd >= 0 {
        ordinary.env(TRACE_LINK, fs::read_link(fd_link(trace_fd)).unwrap());
    }
    let ordinary = ordinary.output().unwrap();
    assert!(
        ordinary.status.success(),
        "{}",
        String::from_utf8_lossy(&ordinary.stderr)
    );
    let error = originals.exec(&mut child_command("after", socket_fd, trace_fd));
    panic!("self-exec failed: {error}");
}
