//! Shared synthetic native adoption contracts, linked against each harness's production descriptors.
use super::descriptors::*;
use interprocess::os::windows::named_pipe::{DuplexPipeStream, PipeListenerOptions, PipeMode, pipe_mode::Messages};
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::{AsRawHandle as _, IntoRawHandle as _, OwnedHandle};
use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT, SetHandleInformation};
use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};

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
fn inheritable(handle: &impl std::os::windows::io::AsRawHandle) {
    // SAFETY: the borrowed owner keeps this handle live while its flags change.
    let updated = unsafe { SetHandleInformation(handle.as_raw_handle(), HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    assert_ne!(updated, 0);
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
fn standard_stream_handles_are_reserved_without_taking_ownership() {
    for kind in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: this queries a process standard handle without taking ownership.
        let handle = unsafe { GetStdHandle(kind) };
        if !handle.is_null() && handle as usize != usize::MAX {
            assert!(matches!(adopt_socket(handle as usize), Err(AdoptError::Reserved(_))));
            assert!(matches!(adopt_trace(handle as usize), Err(AdoptError::Reserved(_))));
            let mut flags = 0;
            // SAFETY: the standard stream is still owned by the process.
            assert_ne!(unsafe { GetHandleInformation(handle, &raw mut flags) }, 0);
        }
    }
}
#[test]
fn file_access_is_checked_and_inheritance_is_cleared() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    inheritable(&file);
    let value = file.into_raw_handle() as usize;
    let mut file = adopt_trace(value).unwrap();
    assert_eq!(file.as_raw_handle() as usize, value);
    private(&file);
    file.write_all(b"synthetic trace").unwrap();
    drop(file);
    assert_eq!(std::fs::read(&path).unwrap(), b"synthetic trace");
    let reader = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap()
        .into_raw_handle() as usize;
    assert!(matches!(adopt_trace(reader), Err(AdoptError::NotWritable { .. })));
    File::open(&path).unwrap();
    let file = OpenOptions::new().append(true).open(&path).unwrap();
    let mut file = adopt_trace(file.into_raw_handle() as usize).unwrap();
    file.write_all(b" append").unwrap();
    drop(file);
    assert_eq!(std::fs::read(&path).unwrap(), b"synthetic trace append");
    let file = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap()
        .into_raw_handle() as usize;
    assert!(matches!(adopt_socket(file), Err(AdoptError::NotAPipe { .. })));
    File::open(&path).unwrap();
}
#[test]
fn broker_pipes_keep_byte_io_and_do_not_become_trace_files() {
    let (peer, stream) = teams_auth::broker::private_pair().unwrap();
    let teams_auth::broker::BrokerStream::NamedPipe(pipe) = &stream;
    inheritable(pipe.inner());
    let value = raw(stream);
    let mut adopted = adopt_socket(value).unwrap();
    let teams_auth::broker::BrokerStream::NamedPipe(pipe) = &adopted;
    private(pipe.inner());
    assert_eq!(pipe.inner().as_raw_handle() as usize, value);
    (&peer).write_all(b"synthetic").unwrap();
    let mut bytes = [0; 9];
    adopted.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"synthetic");
    adopted.write_all(&bytes).unwrap();
    (&peer).read_exact(&mut bytes).unwrap();
    // local_socket's stream drop flushes its byte pipe. Drain the response before closing it.
    drop(adopted);
    assert_eq!((&peer).read(&mut bytes).unwrap(), 0);
    let (peer, stream) = teams_auth::broker::private_pair().unwrap();
    let value = raw(stream);
    assert!(matches!(adopt_trace(value), Err(AdoptError::NotWritable { .. })));
    assert_eq!((&peer).read(&mut bytes).unwrap(), 0);
}

#[test]
fn a_message_mode_pipe_cannot_be_adopted_as_the_brokers_byte_stream() {
    let dir = tempfile::tempdir().unwrap();
    let name = format!(
        r"\\.\pipe\ngteams-descriptor-{}-{}",
        std::process::id(),
        dir.path().file_name().unwrap().to_string_lossy()
    );
    let listener = PipeListenerOptions::new()
        .path(name.as_str())
        .mode(PipeMode::Messages)
        .nonblocking(true)
        .accept_remote(false)
        .create_duplex::<Messages>()
        .unwrap();
    let client = DuplexPipeStream::<Messages>::connect_by_path_with_wait_mode(
        name.as_str(),
        interprocess::ConnectWaitMode::Timeout(std::time::Duration::from_secs(1)),
    )
    .unwrap();
    let _peer = listener.accept().unwrap();
    let handle: OwnedHandle = client.try_into().unwrap();
    assert!(matches!(
        adopt_socket(handle.into_raw_handle() as usize),
        Err(AdoptError::NotAPipe { .. })
    ));
}
