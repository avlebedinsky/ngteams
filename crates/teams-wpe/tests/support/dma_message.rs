//! CPU-only worker protocol tests. Ordinary temporary files exercise descriptor transport, never GPU allocation.

use std::os::fd::AsRawFd as _;

use super::*;

#[test]
fn cpu_dma_packet_duplicates_each_plane_and_preserves_metadata_without_accepting_plain_files() {
    let planes: Vec<Plane> = [(4, 32), (128, 16)]
        .into_iter()
        .map(|(offset, stride)| Plane {
            fd: tempfile::tempfile().unwrap().into(),
            offset,
            stride,
        })
        .collect();
    let original: Vec<_> = planes
        .iter()
        .map(|plane| rustix::fs::fstat(&plane.fd).unwrap().st_ino)
        .collect();
    let dmabuf = Arc::new(DmaBuf {
        width: 8,
        height: 4,
        fourcc: crate::fourcc(b"XR24"),
        modifier: crate::MODIFIER_LINEAR,
        planes,
    });
    let packet = FromWorker::event_packet(Event::Frame(Frame {
        view: ViewId(17),
        buffer: BufferId(29),
        width: 8,
        height: 4,
        damage: vec![Rect {
            x: 1,
            y: 2,
            width: 3,
            height: 1,
        }],
        content: FrameContent::DmaBuf(Arc::clone(&dmabuf)),
    }))
    .unwrap();
    assert!(packet.blob.is_none());
    assert_eq!(packet.fds.len(), 2);
    for (copied, source) in packet.fds.iter().zip(&dmabuf.planes) {
        assert_ne!(copied.as_raw_fd(), source.fd.as_raw_fd());
    }
    drop(dmabuf);
    for (fd, inode) in packet.fds.iter().zip(original) {
        assert_eq!(
            rustix::fs::fstat(fd).unwrap().st_ino,
            inode,
            "the packet owns a live duplicate"
        );
    }
    let FromWorker::DmaBufFrame { frame, dmabuf } = body::<FromWorker>(&packet).unwrap() else {
        panic!("a DMA frame must use the descriptor-carrying message");
    };
    assert_eq!((frame.view, frame.buffer), (ViewId(17), BufferId(29)));
    assert_eq!(
        (dmabuf.width, dmabuf.height, dmabuf.fourcc, dmabuf.modifier),
        (8, 4, crate::fourcc(b"XR24"), crate::MODIFIER_LINEAR)
    );
    assert_eq!(dmabuf.planes, [(4, 32), (128, 16)]);
    assert_eq!(
        frame.damage,
        [Rect {
            x: 1,
            y: 2,
            width: 3,
            height: 1
        }]
    );
    let error = dmabuf_frame(frame, dmabuf, packet.fds, None).unwrap_err();
    assert!(error.to_string().contains("plane 0 is not a dmabuf"), "{error}");
}

#[test]
fn cpu_dma_worker_start_failures_retain_display_and_ended_kinds_and_other_failure_context() {
    for error in [
        StartError::Display("synthetic display failure".into()),
        StartError::Ended,
        StartError::Config(crate::ConfigError::NoFormats),
        StartError::AlreadyStarted,
        StartError::Thread(std::io::Error::other("synthetic thread creation failure")),
    ] {
        let description = error.to_string();
        let encoded = serde_json::to_vec(&StartFailure::from(&error)).unwrap();
        let restored: StartError = serde_json::from_slice::<StartFailure>(&encoded).unwrap().into();
        match (&error, restored) {
            (StartError::Display(expected), StartError::Display(actual)) => assert_eq!(&actual, expected),
            (StartError::Ended, StartError::Ended) => {}
            (_, StartError::Worker(actual)) => assert_eq!(actual, description),
            (_, other) => panic!("wrong startup failure kind: {other}"),
        }
    }
}
