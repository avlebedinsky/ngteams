//! Failed native restarts must leave the original synthetic broker and trace private and usable. These checks
//! stay in the parent process; the shared restart fixture separately verifies successful successor transfers.

#![cfg(test)]

use std::fs::File;
use std::io::{ErrorKind, Read as _, Write as _};
use std::path::Path;
use std::process::Command;

use ngteams::restart::{self, RestartDescriptors};
use teams_auth::broker::BrokerStream;
use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT};

#[test]
fn failed_launches_preserve_the_original_broker_and_optional_trace() {
    for with_trace in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let (socket, mut peer) = teams_auth::broker::private_pair().unwrap();
        let path = directory.path().join("trace");
        let trace = with_trace.then(|| File::create(&path).unwrap());
        let descriptors = RestartDescriptors::new(socket, trace);
        for equals_form in [false, true] {
            let args = valid_arguments(with_trace, equals_form);
            let mut command = missing(directory.path());
            command.args(&args);
            assert_eq!(descriptors.exec(&mut command).kind(), ErrorKind::NotFound);
            for change_directory in [false, true] {
                let mut command = missing(directory.path());
                command.args(&args);
                if change_directory {
                    command.current_dir(directory.path());
                } else {
                    command.env("NGTEAMS_SYNTHETIC_RESTART", "changed");
                }
                assert_eq!(descriptors.exec(&mut command).kind(), ErrorKind::InvalidInput);
                still_usable(&descriptors, &mut peer);
            }
        }
        if with_trace {
            assert_eq!(std::fs::read(path).unwrap(), b"tttt");
        }
    }
}

#[test]
fn incomplete_or_unavailable_descriptor_arguments_never_launch_a_successor() {
    let directory = tempfile::tempdir().unwrap();
    let (socket, mut peer) = teams_auth::broker::private_pair().unwrap();
    let descriptors = RestartDescriptors::new(socket, None);
    for args in [
        vec![],
        vec!["--secrets-fd"],
        vec!["--trace-fd"],
        vec!["--trace-fd", "123"],
        vec!["--trace-fd=123"],
        vec!["--secrets-fd", "123", "--trace-fd", "456"],
        vec!["--secrets-fd=123", "--trace-fd=456"],
    ] {
        let mut command = missing(directory.path());
        command.args(args);
        assert_eq!(descriptors.exec(&mut command).kind(), ErrorKind::InvalidInput);
        still_usable(&descriptors, &mut peer);
    }
    let trace = File::create(directory.path().join("trace")).unwrap();
    let descriptors = RestartDescriptors::new(descriptors.socket().unwrap(), Some(trace));
    let mut missing_trace = missing(directory.path());
    missing_trace.args(["--secrets-fd=123", "unchanged-argument"]);
    assert_eq!(descriptors.exec(&mut missing_trace).kind(), ErrorKind::InvalidInput);
    still_usable(&descriptors, &mut peer);
    for change_directory in [false, true] {
        let mut command = missing(directory.path());
        if change_directory {
            command.current_dir(directory.path());
        } else {
            command.env("NGTEAMS_SYNTHETIC_RESTART", "changed");
        }
        assert_eq!(restart::exec(&mut command).kind(), ErrorKind::InvalidInput);
    }
}

fn missing(directory: &Path) -> Command {
    Command::new(directory.join("missing-successor.exe"))
}

fn valid_arguments(with_trace: bool, equals_form: bool) -> Vec<String> {
    let mut arguments = vec!["unchanged-argument".to_owned()];
    // Deliberately obsolete numbers: only fresh duplicates may reach a successor.
    for (flag, value) in [("--secrets-fd", "123"), ("--trace-fd", "456")] {
        if flag == "--trace-fd" && !with_trace {
            continue;
        }
        if equals_form {
            arguments.push(format!("{flag}={value}"));
        } else {
            arguments.extend([flag.to_owned(), value.to_owned()]);
        }
    }
    arguments
}

fn still_usable(descriptors: &RestartDescriptors, peer: &mut BrokerStream) {
    let mut socket = descriptors.socket().unwrap();
    let BrokerStream::NamedPipe(pipe) = &socket;
    private(pipe.inner());
    peer.write_all(b"p").unwrap();
    let mut byte = [0];
    socket.read_exact(&mut byte).unwrap();
    assert_eq!(&byte, b"p");
    socket.write_all(b"q").unwrap();
    peer.read_exact(&mut byte).unwrap();
    assert_eq!(&byte, b"q");
    if let Some(mut trace) = descriptors.trace().unwrap() {
        private(&trace);
        trace.write_all(b"t").unwrap();
    }
}

fn private(handle: &impl std::os::windows::io::AsRawHandle) {
    let mut flags = 0;
    assert_ne!(
        // SAFETY: the owner keeps this handle live; flags is a valid writable output.
        unsafe { GetHandleInformation(handle.as_raw_handle(), &raw mut flags) },
        0
    );
    assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
}
