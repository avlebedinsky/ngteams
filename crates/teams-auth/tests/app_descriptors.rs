//! Compile the application's real descriptor adoption on both platforms. All endpoints and files are synthetic.
#![cfg(test)]
#[path = "../../ngteams/src/descriptors.rs"]
pub mod descriptors;
#[path = "../../ngteams/src/restart.rs"]
pub mod restart;

#[cfg(windows)]
mod windows {
    use super::descriptors::*;
    use std::fs::{File, OpenOptions};
    use std::io::{Read as _, Write as _};
    use std::os::windows::io::{AsRawHandle as _, IntoRawHandle as _, OwnedHandle};
    use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT, SetHandleInformation};

    fn raw(stream: teams_auth::broker::BrokerStream) -> usize {
        let teams_auth::broker::BrokerStream::NamedPipe(pipe) = stream;
        let handle: OwnedHandle = pipe.into();
        handle.into_raw_handle() as usize
    }
    fn private(handle: &impl std::os::windows::io::AsRawHandle) {
        let mut flags = 0;
        // SAFETY: handle owns the queried object and flags is a live output.
        let valid = unsafe { GetHandleInformation(handle.as_raw_handle(), &raw mut flags) };
        assert_ne!(valid, 0);
        assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
    }
    #[test]
    fn reserved_and_closed_handles_are_not_adopted() {
        for value in [0, 1, 2, usize::MAX] {
            assert!(matches!(adopt_socket(value), Err(AdoptError::Reserved(_))));
            assert!(matches!(adopt_trace(value), Err(AdoptError::Reserved(_))));
        }
        assert!(matches!(adopt_socket(usize::MAX - 15), Err(AdoptError::NotOpen { .. })));
        assert!(matches!(adopt_trace(usize::MAX - 15), Err(AdoptError::NotOpen { .. })));
    }
    #[test]
    fn file_access_is_checked_and_inheritance_is_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace");
        let file = File::create(&path).unwrap();
        // SAFETY: the file stays open during this flag change.
        let updated = unsafe { SetHandleInformation(file.as_raw_handle(), HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
        assert_ne!(updated, 0);
        let mut file = adopt_trace(file.into_raw_handle() as usize).unwrap();
        private(&file);
        file.write_all(b"synthetic trace").unwrap();
        drop(file);
        assert_eq!(std::fs::read(&path).unwrap(), b"synthetic trace");
        let reader = File::open(&path).unwrap();
        assert!(matches!(
            adopt_trace(reader.into_raw_handle() as usize),
            Err(AdoptError::NotWritable { .. })
        ));
        let file = OpenOptions::new().append(true).open(&path).unwrap();
        let mut file = adopt_trace(file.into_raw_handle() as usize).unwrap();
        file.write_all(b" append").unwrap();
        drop(file);
        assert_eq!(std::fs::read(&path).unwrap(), b"synthetic trace append");
        let file = File::open(&path).unwrap();
        assert!(matches!(
            adopt_socket(file.into_raw_handle() as usize),
            Err(AdoptError::NotAPipe { .. })
        ));
    }
    #[test]
    fn broker_pipes_keep_byte_io_and_do_not_become_trace_files() {
        let (peer, stream) = teams_auth::broker::private_pair().unwrap();
        let mut adopted = adopt_socket(raw(stream)).unwrap();
        let teams_auth::broker::BrokerStream::NamedPipe(pipe) = &adopted;
        private(pipe.inner());
        (&peer).write_all(b"synthetic").unwrap();
        let mut bytes = [0; 9];
        adopted.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"synthetic");
        adopted.write_all(&bytes).unwrap();
        (&peer).read_exact(&mut bytes).unwrap();
        assert!(matches!(adopt_trace(raw(adopted)), Err(AdoptError::NotWritable { .. })));
    }
}

#[cfg(windows)]
mod restarts {
    use crate::descriptors::{adopt_socket, adopt_trace};
    use crate::restart::RestartDescriptors;
    use std::ffi::OsString;
    use std::fs::File;
    use std::io::{Read as _, Write as _};
    use std::os::windows::io::AsHandle as _;
    use std::process::Command;
    use std::time::Duration;
    use teams_auth::broker::BrokerStream;
    use teams_platform::windows_process::Transfer;
    use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT};

    const MARKER: &str = "NGTEAMS_RESTART_SYNTHETIC";
    fn arguments(stage: &str) -> Vec<OsString> {
        [
            "--exact",
            "restarts::restart_child",
            "--nocapture",
            "--test-threads=1",
            "--",
            MARKER,
            stage,
        ]
        .map(OsString::from)
        .to_vec()
    }
    fn private(handle: &impl std::os::windows::io::AsRawHandle) {
        let mut flags = 0;
        // SAFETY: the owner keeps the handle open; flags is a live output.
        let valid = unsafe { GetHandleInformation(handle.as_raw_handle(), &raw mut flags) };
        assert_ne!(valid, 0);
        assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
    }
    #[test]
    fn actual_restart_transfers_the_broker_and_optional_trace_and_failures_leave_them_private() {
        for trace_present in [false, true] {
            for equals_form in [false, true] {
                check_restart(trace_present, equals_form);
            }
        }
    }
    fn check_restart(trace_present: bool, equals_form: bool) {
        let (socket, mut peer) = teams_auth::broker::private_pair().unwrap();
        let BrokerStream::NamedPipe(pipe) = &socket;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace");
        let trace = File::create(&path).unwrap();
        let mut handles = vec![pipe.inner().as_handle()];
        if trace_present {
            handles.push(trace.as_handle());
        }
        let transfer = Transfer::new(&handles).unwrap();
        let mut args = arguments("before");
        if equals_form {
            args.push(format!("--secrets-fd={}", transfer.value(0).unwrap()).into());
        } else {
            args.extend(["--secrets-fd".into(), transfer.value(0).unwrap().to_string().into()]);
        }
        if trace_present {
            if equals_form {
                args.push(format!("--trace-fd={}", transfer.value(1).unwrap()).into());
            } else {
                args.extend(["--trace-fd".into(), transfer.value(1).unwrap().to_string().into()]);
            }
        }
        let child = transfer.spawn(&std::env::current_exe().unwrap(), &args).unwrap();
        drop(transfer);
        drop(socket);
        assert_eq!(child.wait(Duration::from_secs(20)).unwrap(), Some(0));
        let mut bytes = [0; 9];
        peer.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"restarted");
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            if trace_present { b"trace".as_slice() } else { &[] }
        );
    }
    fn value(args: &[OsString], flag: &str) -> Option<usize> {
        for (index, arg) in args.iter().enumerate() {
            if arg == flag {
                return Some(args[index + 1].to_str().unwrap().parse().unwrap());
            }
            if let Some(value) = arg.to_str().and_then(|arg| arg.strip_prefix(&format!("{flag}="))) {
                return Some(value.parse().unwrap());
            }
        }
        None
    }
    #[test]
    fn restart_child() {
        let args: Vec<OsString> = std::env::args_os().skip(1).collect();
        let Some(marker) = args.iter().position(|value| value == MARKER) else {
            return;
        };
        let mut socket = adopt_socket(value(&args, "--secrets-fd").unwrap()).unwrap();
        let trace = value(&args, "--trace-fd").map(|handle| adopt_trace(handle).unwrap());
        let BrokerStream::NamedPipe(pipe) = &socket;
        private(pipe.inner());
        if let Some(trace) = &trace {
            private(trace);
        }
        if args[marker + 1] == "after" {
            socket.write_all(b"restarted").unwrap();
            if let Some(mut trace) = trace {
                trace.write_all(b"trace").unwrap();
            }
            return;
        }
        let descriptors = RestartDescriptors::new(socket, trace);
        let cloned = descriptors.socket().unwrap();
        let BrokerStream::NamedPipe(pipe) = &cloned;
        private(pipe.inner());
        if let Some(trace) = descriptors.trace().unwrap() {
            private(&trace);
        }
        let mut missing = Command::new(r"C:\ngteams-synthetic-missing.exe");
        missing.args(&args);
        assert_eq!(descriptors.exec(&mut missing).kind(), std::io::ErrorKind::NotFound);
        private(pipe.inner());
        assert_eq!(
            descriptors
                .exec(&mut Command::new(std::env::current_exe().unwrap()))
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        let mut changed_env = Command::new(std::env::current_exe().unwrap());
        changed_env.args(&args).env("SYNTHETIC", "value");
        assert_eq!(
            descriptors.exec(&mut changed_env).kind(),
            std::io::ErrorKind::InvalidInput
        );
        let mut next_args = args;
        next_args[marker + 1] = "after".into();
        let error = descriptors.exec(Command::new(std::env::current_exe().unwrap()).args(next_args));
        panic!("restart failed: {error}");
    }
}
