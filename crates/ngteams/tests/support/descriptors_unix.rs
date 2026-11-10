//! Synthetic descriptors only: the broker and trace take sole ownership and never inherit across exec.

#![cfg(test)]

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::os::fd::{AsRawFd as _, IntoRawFd as _};
use std::os::unix::net::{UnixDatagram, UnixStream};

use ngteams::descriptors::{AdoptError, adopt_socket, adopt_trace};
use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};

// Keep close/re-adopt checks in a single test so another harness case cannot reuse the closed fd.
#[test]
fn broker_and_trace_descriptors_are_owned_validated_and_close_on_exec() {
    reserved_descriptors_are_not_taken();
    missing_descriptors_are_reported();
    broker_round_trip();
    wrong_socket_kinds_close_the_passed_descriptor();
    trace_round_trip();
}

fn reserved_descriptors_are_not_taken() {
    for fd in [-1, 0, 1, 2] {
        let socket = adopt_socket(fd).unwrap_err();
        let trace = adopt_trace(fd).unwrap_err();
        assert!(matches!(socket, AdoptError::Reserved(value) if value == fd));
        assert!(matches!(trace, AdoptError::Reserved(value) if value == fd));
        assert!(socket.to_string().contains(&fd.to_string()));
        assert!(trace.to_string().contains(&fd.to_string()));
    }
}

fn missing_descriptors_are_reported() {
    for error in [adopt_socket(i32::MAX).unwrap_err(), adopt_trace(i32::MAX).unwrap_err()] {
        assert!(matches!(error, AdoptError::NotOpen { fd, .. } if fd == i32::MAX));
        assert!(error.to_string().contains("isn't open"));
        assert!(format!("{error:?}").contains("NotOpen"));
    }
}

fn broker_round_trip() {
    let (mut peer, passed) = UnixStream::pair().unwrap();
    fcntl_setfd(&passed, FdFlags::empty()).unwrap();
    let fd = passed.into_raw_fd();
    let mut adopted = adopt_socket(fd).unwrap();
    assert_eq!(adopted.as_raw_fd(), fd);
    assert!(fcntl_getfd(&adopted).unwrap().contains(FdFlags::CLOEXEC));
    peer.write_all(b"synthetic question").unwrap();
    let mut bytes = [0; 18];
    adopted.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"synthetic question");
    adopted.write_all(b"synthetic answer").unwrap();
    let mut bytes = [0; 16];
    peer.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"synthetic answer");
    drop(adopted);
    assert_eq!(peer.read(&mut bytes).unwrap(), 0);
}

fn wrong_socket_kinds_close_the_passed_descriptor() {
    let file = tempfile::tempfile().unwrap().into_raw_fd();
    let datagram = UnixDatagram::unbound().unwrap().into_raw_fd();
    let internet = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .into_raw_fd();
    for fd in [file, datagram, internet] {
        let error = adopt_socket(fd).unwrap_err();
        assert!(matches!(error, AdoptError::NotAUnixStream { fd: value } if value == fd));
        assert!(error.to_string().contains("isn't a Unix stream socket"));
        assert!(matches!(adopt_socket(fd), Err(AdoptError::NotOpen { .. })));
    }
}

fn trace_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("synthetic-trace.jsonl");
    let mut file = File::create(&path).unwrap();
    file.write_all(b"synthetic\n").unwrap();
    drop(file);
    let reader = File::open(&path).unwrap().into_raw_fd();
    let error = adopt_trace(reader).unwrap_err();
    assert!(matches!(error, AdoptError::NotWritable { fd } if fd == reader));
    assert!(error.to_string().contains("isn't open for writing"));
    assert!(matches!(adopt_trace(reader), Err(AdoptError::NotOpen { .. })));
    for read in [false, true] {
        let file = OpenOptions::new()
            .read(read)
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        fcntl_setfd(&file, FdFlags::empty()).unwrap();
        let fd = file.into_raw_fd();
        let mut adopted = adopt_trace(fd).unwrap();
        assert_eq!(adopted.as_raw_fd(), fd);
        assert!(fcntl_getfd(&adopted).unwrap().contains(FdFlags::CLOEXEC));
        adopted.write_all(b"synthetic trace\n").unwrap();
        drop(adopted);
        assert_eq!(std::fs::read(&path).unwrap(), b"synthetic trace\n");
    }
}
