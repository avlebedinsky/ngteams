//! The worker's packets: one `SOCK_SEQPACKET` message each, a fixed header, the body inline when it is small, and
//! file descriptors passed with it (`SCM_RIGHTS`).
//!
//! A body over [`INLINE_MAX`] travels in a sealed `memfd` instead, and so does the packet's blob (raw bytes the body
//! describes: an SHM frame's pixels, a scheme answer's data), so no packet outgrows the socket's buffer. The
//! descriptors come in a fixed order: the body's `memfd`, the blob's, then the body's own (a dmabuf's planes). A
//! receiver checks each `memfd`'s size and seals before it reads, and takes every descriptor into its ownership at once,
//! so an error path closes them. A truncated packet (`MSG_TRUNC`, `MSG_CTRUNC`), a descriptor count other than the
//! header's, or a size out of the receiver's [`Limits`] is a broken transport, never something to read around: the app
//! reads what an untrusted worker sends within bounds of its own ([`Limits::FROM_WORKER`]).

use std::fs::File;
use std::io::{IoSlice, IoSliceMut, Write as _};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::FileExt as _;

use rustix::fs::{MemfdFlags, SealFlags};
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer, SendAncillaryMessage,
    SendFlags,
};

/// The largest body sent inline; a larger one goes in a `memfd`.
pub(crate) const INLINE_MAX: usize = 32 * 1024;
/// The most descriptors a body carries itself (a dmabuf has at most four memory planes).
pub(crate) const MAX_BODY_FDS: usize = 4;
/// The largest body or blob a `memfd` may hold: far above an 8K frame's pixels, far below a size nobody means.
const MEMFD_MAX: u64 = 1 << 30;

/// The largest body and blob a receiver reads, in bytes: anything larger is a broken packet, refused before a byte of
/// it is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    pub body: u64,
    pub blob: u64,
}

impl Limits {
    /// What the worker reads from the app, which it trusts: a scheme answer's bytes may be large.
    pub(crate) const FROM_APP: Self = Self {
        body: MEMFD_MAX,
        blob: MEMFD_MAX,
    };
    /// What the app reads from the worker, which it does not trust: a body as large as a big clipboard or script
    /// message, and a blob (only an SHM frame's pixels) as large as the largest frame
    /// ([`super::message::MAX_FRAME_BYTES`]).
    pub(crate) const FROM_WORKER: Self = Self {
        body: 64 << 20,
        blob: super::message::MAX_FRAME_BYTES,
    };

    /// Whether a receiver within these limits reads `packet`: its body and blob no larger than they allow (a sender
    /// that sends a larger one breaks the transport).
    pub(crate) fn admit(self, packet: &Packet) -> bool {
        let within = |len: usize, max: u64| u64::try_from(len).is_ok_and(|len| len <= max.min(MEMFD_MAX));
        within(packet.body.len(), self.body) && packet.blob.as_ref().is_none_or(|blob| within(blob.len(), self.blob))
    }
}
/// kind (1), body length (8), blob length (8), the body's descriptor count (4).
const HEADER: usize = 21;
/// The body inline.
const INLINE: u8 = 0;
/// The body in a `memfd`.
const IN_MEMFD: u8 = 1;
/// The header's blob length when there is no blob.
const NO_BLOB: u64 = u64::MAX;
/// The seals a received `memfd` must carry: nobody can change it while it is read.
const SEALS: SealFlags = SealFlags::SHRINK.union(SealFlags::GROW).union(SealFlags::WRITE);

/// One message: its body (JSON), its blob and the descriptors the body names.
#[derive(Debug, Default)]
pub(crate) struct Packet {
    pub body: Vec<u8>,
    pub blob: Option<Vec<u8>>,
    pub fds: Vec<OwnedFd>,
}

/// Why the transport broke.
#[derive(Debug, thiserror::Error)]
pub(crate) enum WireError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("a broken packet: {0}")]
    Broken(String),
}

impl From<rustix::io::Errno> for WireError {
    fn from(errno: rustix::io::Errno) -> Self {
        Self::Io(errno.into())
    }
}

fn broken(what: impl Into<String>) -> WireError {
    WireError::Broken(what.into())
}

/// Sends `packet` on `socket`, blocking until the socket takes it.
pub(crate) fn send(socket: BorrowedFd<'_>, packet: &Packet) -> Result<(), WireError> {
    if packet.fds.len() > MAX_BODY_FDS {
        return Err(broken(format!("{} descriptors in one body", packet.fds.len())));
    }
    let inline = packet.body.len() <= INLINE_MAX;
    let mut carried: Vec<OwnedFd> = Vec::new();
    if !inline {
        carried.push(sealed_memfd(&packet.body)?);
    }
    if let Some(blob) = &packet.blob {
        carried.push(sealed_memfd(blob)?);
    }
    let mut header = [0u8; HEADER];
    header[0] = if inline { INLINE } else { IN_MEMFD };
    header[1..9].copy_from_slice(&(packet.body.len() as u64).to_le_bytes());
    let blob_len = packet.blob.as_ref().map_or(NO_BLOB, |blob| blob.len() as u64);
    header[9..17].copy_from_slice(&blob_len.to_le_bytes());
    let count = u32::try_from(packet.fds.len()).unwrap_or(u32::MAX);
    header[17..21].copy_from_slice(&count.to_le_bytes());
    let fds: Vec<BorrowedFd<'_>> = carried.iter().chain(&packet.fds).map(AsFd::as_fd).collect();
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(2 + MAX_BODY_FDS))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if !fds.is_empty() && !control.push(SendAncillaryMessage::ScmRights(&fds)) {
        return Err(broken("the descriptors do not fit the control buffer"));
    }
    let body: &[u8] = if inline { &packet.body } else { &[] };
    let iov = [IoSlice::new(&header), IoSlice::new(body)];
    loop {
        match rustix::net::sendmsg(socket, &iov, &mut control, SendFlags::NOSIGNAL) {
            Ok(sent) if sent == HEADER + body.len() => return Ok(()),
            Ok(sent) => return Err(broken(format!("{sent} of {} bytes sent", HEADER + body.len()))),
            Err(rustix::io::Errno::INTR) => {}
            Err(errno) => return Err(errno.into()),
        }
    }
}

/// The next packet on `socket` within `limits`, blocking until one arrives; `None` once the peer is gone.
pub(crate) fn recv(socket: BorrowedFd<'_>, limits: Limits) -> Result<Option<Packet>, WireError> {
    let mut buffer = vec![0u8; HEADER + INLINE_MAX];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(2 + MAX_BODY_FDS))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    let received = loop {
        let mut iov = [IoSliceMut::new(&mut buffer)];
        match rustix::net::recvmsg(socket, &mut iov, &mut control, RecvFlags::CMSG_CLOEXEC) {
            Ok(received) => break received,
            Err(rustix::io::Errno::INTR) => {}
            Err(errno) => return Err(errno.into()),
        }
    };
    // owned at once: every path from here closes what arrived
    let mut fds: Vec<OwnedFd> = Vec::new();
    for message in control.drain() {
        if let RecvAncillaryMessage::ScmRights(rights) = message {
            fds.extend(rights);
        }
    }
    if received.flags.contains(ReturnFlags::CTRUNC) {
        return Err(broken("its descriptors were cut off"));
    }
    if received.flags.contains(ReturnFlags::TRUNC) {
        return Err(broken("longer than any packet"));
    }
    if received.bytes == 0 && fds.is_empty() {
        return Ok(None);
    }
    decode(&buffer[..received.bytes], fds, limits).map(Some)
}

/// A received packet's bytes and descriptors as a [`Packet`], its body and blob within `limits`.
fn decode(bytes: &[u8], fds: Vec<OwnedFd>, limits: Limits) -> Result<Packet, WireError> {
    let Some((header, inline)) = bytes.split_first_chunk::<HEADER>() else {
        return Err(broken(format!("{} bytes, shorter than its header", bytes.len())));
    };
    let field = |range: std::ops::Range<usize>| {
        let mut word = [0u8; 8];
        word[..range.len()].copy_from_slice(&header[range]);
        u64::from_le_bytes(word)
    };
    let (kind, body_len, blob_len, body_fds) = (header[0], field(1..9), field(9..17), field(17..21));
    let in_memfd = match kind {
        INLINE => false,
        IN_MEMFD => true,
        other => return Err(broken(format!("an unknown body kind {other}"))),
    };
    let expected = usize::from(in_memfd) + usize::from(blob_len != NO_BLOB) + usize::try_from(body_fds).unwrap_or(0);
    if body_fds > MAX_BODY_FDS as u64 || fds.len() != expected {
        return Err(broken(format!(
            "{} descriptors where the header names {expected}",
            fds.len()
        )));
    }
    let mut fds = fds.into_iter();
    let body = if in_memfd {
        if !inline.is_empty() {
            return Err(broken("a body both inline and in a memfd"));
        }
        let fd = fds.next().ok_or_else(|| broken("no body memfd"))?;
        read_memfd(fd, body_len, limits.body)?
    } else {
        if inline.len() as u64 != body_len {
            return Err(broken(format!(
                "{} body bytes where the header names {body_len}",
                inline.len()
            )));
        }
        inline.to_vec()
    };
    let blob = if blob_len == NO_BLOB {
        None
    } else {
        let fd = fds.next().ok_or_else(|| broken("no blob memfd"))?;
        Some(read_memfd(fd, blob_len, limits.blob)?)
    };
    Ok(Packet {
        body,
        blob,
        fds: fds.collect(),
    })
}

/// A `memfd` holding `bytes`, sealed against every change.
fn sealed_memfd(bytes: &[u8]) -> Result<OwnedFd, WireError> {
    let fd = rustix::fs::memfd_create("teams-wpe", MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)?;
    let mut file = File::from(fd);
    file.write_all(bytes)?;
    rustix::fs::fcntl_add_seals(&file, SEALS | SealFlags::SEAL)?;
    Ok(file.into())
}

/// The `len` bytes of a received `memfd`, once `len` is at most `max` and its seals and size are what they must be.
fn read_memfd(fd: OwnedFd, len: u64, max: u64) -> Result<Vec<u8>, WireError> {
    if len > max.min(MEMFD_MAX) {
        return Err(broken(format!(
            "a memfd of {len} bytes, more than the {max} this side reads"
        )));
    }
    let seals = rustix::fs::fcntl_get_seals(&fd)?;
    if !seals.contains(SEALS) {
        return Err(broken("an unsealed memfd"));
    }
    let size = rustix::fs::fstat(&fd)?.st_size;
    if u64::try_from(size).ok() != Some(len) {
        return Err(broken(format!("a memfd of {size} bytes where the header names {len}")));
    }
    let mut bytes = vec![0u8; usize::try_from(len).map_err(|_| broken("a memfd too large to read"))?];
    // at an offset: the descriptor shares its file offset with the sender's, which wrote it to the end
    File::from(fd).read_exact_at(&mut bytes, 0)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd as _;

    use rustix::net::{AddressFamily, SocketFlags, SocketType};

    use super::*;

    fn pair() -> (OwnedFd, OwnedFd) {
        rustix::net::socketpair(AddressFamily::UNIX, SocketType::SEQPACKET, SocketFlags::CLOEXEC, None).unwrap()
    }

    fn inode(fd: &impl AsFd) -> (u64, u64) {
        let stat = rustix::fs::fstat(fd).unwrap();
        (stat.st_dev, stat.st_ino)
    }

    #[test]
    fn a_small_body_goes_inline_and_a_large_one_and_a_blob_in_memfds() {
        let (ours, theirs) = pair();
        let small = Packet {
            body: b"{\"small\":1}".to_vec(),
            ..Packet::default()
        };
        send(ours.as_fd(), &small).unwrap();
        let got = recv(theirs.as_fd(), Limits::FROM_APP).unwrap().unwrap();
        assert_eq!((got.body, got.blob, got.fds.len()), (small.body, None, 0));

        let large = Packet {
            body: vec![b'x'; INLINE_MAX * 4 + 3],
            blob: Some((0..=255u8).cycle().take(1 << 20).collect()),
            fds: Vec::new(),
        };
        send(ours.as_fd(), &large).unwrap();
        let got = recv(theirs.as_fd(), Limits::FROM_APP).unwrap().unwrap();
        assert_eq!(got.body, large.body);
        assert_eq!(got.blob, large.blob);
        assert!(got.fds.is_empty(), "the memfds are the transport's, not the body's");
    }

    #[test]
    fn a_full_hd_shm_blob_round_trips_and_its_time_is_reported() {
        let (ours, theirs) = pair();
        let pixels: Vec<u8> = (0..1920 * 1080 * 4)
            .map(|at: usize| u8::try_from(at % 251).unwrap())
            .collect();
        let packet = Packet {
            body: b"{}".to_vec(),
            blob: Some(pixels),
            fds: Vec::new(),
        };
        let receiver = std::thread::spawn(move || recv(theirs.as_fd(), Limits::FROM_APP).unwrap().unwrap());
        let started = std::time::Instant::now();
        send(ours.as_fd(), &packet).unwrap();
        let got = receiver.join().unwrap();
        let elapsed = started.elapsed();
        assert_eq!(got.blob, packet.blob);
        println!("a 1920x1080 SHM frame (8.3 MB) through the worker's transport: {elapsed:?}");
    }

    #[test]
    fn a_body_s_descriptors_arrive_after_the_memfds_in_their_order() {
        let (ours, theirs) = pair();
        let files: Vec<File> = (0..3).map(|_| tempfile::tempfile().unwrap()).collect();
        let expected: Vec<(u64, u64)> = files.iter().map(inode).collect();
        let packet = Packet {
            body: vec![b'y'; INLINE_MAX + 1],
            blob: Some(vec![7; 16]),
            fds: files.into_iter().map(OwnedFd::from).collect(),
        };
        send(ours.as_fd(), &packet).unwrap();
        let got = recv(theirs.as_fd(), Limits::FROM_APP).unwrap().unwrap();
        assert_eq!(got.blob.as_deref(), Some(&[7u8; 16][..]));
        assert_eq!(
            got.fds.iter().map(inode).collect::<Vec<_>>(),
            expected,
            "every plane in its place"
        );
    }

    /// Whether descriptor number `fd` is open in this process.
    fn open(fd: i32) -> bool {
        // SAFETY: `fcntl(F_GETFD)` only reads the descriptor table.
        unsafe { libc::fcntl(fd, libc::F_GETFD) != -1 }
    }

    #[test]
    fn a_packet_whose_descriptors_disagree_with_its_header_is_refused_and_its_descriptors_closed() {
        let (ours, theirs) = pair();
        // a header naming no descriptor, sent with one
        let mut header = [0u8; HEADER];
        header[9..17].copy_from_slice(&NO_BLOB.to_le_bytes());
        let stray = tempfile::tempfile().unwrap();
        let stray_inode = inode(&stray);
        let fds = [stray.as_fd()];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut control = SendAncillaryBuffer::new(&mut space);
        assert!(control.push(SendAncillaryMessage::ScmRights(&fds)));
        rustix::net::sendmsg(&ours, &[IoSlice::new(&header)], &mut control, SendFlags::NOSIGNAL).unwrap();
        drop(stray);
        let error = recv(theirs.as_fd(), Limits::FROM_APP).unwrap_err();
        assert!(matches!(error, WireError::Broken(_)), "{error}");
        // no descriptor of this process names the stray file any more (other tests open files meanwhile)
        let open_inodes: Vec<(u64, u64)> = std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .flatten()
            .filter_map(|entry| std::fs::metadata(entry.path()).ok())
            .map(|metadata| {
                use std::os::unix::fs::MetadataExt as _;
                (metadata.dev(), metadata.ino())
            })
            .collect();
        assert!(!open_inodes.contains(&stray_inode), "the stray descriptor was closed");

        // a packet longer than any the wire makes
        rustix::net::send(&ours, &vec![0u8; HEADER + INLINE_MAX + 1], SendFlags::NOSIGNAL).unwrap();
        assert!(matches!(
            recv(theirs.as_fd(), Limits::FROM_APP),
            Err(WireError::Broken(_))
        ));
        // the peer gone: the end, not an error
        drop(ours);
        assert!(recv(theirs.as_fd(), Limits::FROM_APP).unwrap().is_none());
        assert!(open(theirs.as_raw_fd()));
    }

    #[test]
    fn a_receiver_s_limits_say_which_packets_it_reads() {
        // zeroed and never touched: no memory for the largest ones
        let packet = |body: usize, blob: Option<usize>| Packet {
            body: vec![0; body],
            blob: blob.map(|len| vec![0; len]),
            fds: Vec::new(),
        };
        let max = usize::try_from(MEMFD_MAX).unwrap();
        assert!(Limits::FROM_APP.admit(&packet(max, Some(max))));
        assert!(!Limits::FROM_APP.admit(&packet(max + 1, None)));
        assert!(!Limits::FROM_APP.admit(&packet(2, Some(max + 1))));
        assert!(Limits::FROM_WORKER.admit(&packet(64 << 20, None)));
        assert!(!Limits::FROM_WORKER.admit(&packet((64 << 20) + 1, None)));
    }

    #[test]
    fn an_unsealed_or_resized_memfd_is_refused() {
        let unsealed = rustix::fs::memfd_create("t", MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING).unwrap();
        File::from(unsealed.try_clone().unwrap()).write_all(b"abc").unwrap();
        assert!(matches!(read_memfd(unsealed, 3, MEMFD_MAX), Err(WireError::Broken(_))));
        let sealed = sealed_memfd(b"abc").unwrap();
        assert!(matches!(
            read_memfd(sealed.try_clone().unwrap(), 4, MEMFD_MAX),
            Err(WireError::Broken(_))
        ));
        assert!(
            matches!(read_memfd(sealed.try_clone().unwrap(), 3, 2), Err(WireError::Broken(_))),
            "a memfd larger than the receiver reads"
        );
        assert_eq!(read_memfd(sealed, 3, MEMFD_MAX).unwrap(), b"abc");
    }
}
