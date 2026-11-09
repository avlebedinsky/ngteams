//! Windows rejects a UDP socket bound to a real interface sending to 127/8 (WSAEADDRNOTAVAIL). libwebrtc ignores
//! loopback interfaces, so the test TURN client needs a host-local interface address. This adapter forwards only
//! packets whose source is that same host address into the existing loopback server; it never forwards off-host.
//! The TURN allocations themselves remain loopback sockets and production ICE policy is untouched.
use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub(super) struct Proxy {
    pub address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Proxy {
    pub fn start(destination: SocketAddr) -> io::Result<Self> {
        if !destination.ip().is_loopback() {
            return Err(io::Error::other("the test adapter forwards only to loopback"));
        }
        let route = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        // UDP connect selects a local route without sending a datagram. TEST-NET-1 is a documentation address,
        // never a relay or an external test target. Every actual packet below goes to this host or to loopback.
        route.connect((Ipv4Addr::new(192, 0, 2, 1), 9))?;
        let local = route.local_addr()?.ip();
        let socket = UdpSocket::bind((local, 0))?;
        socket.set_nonblocking(true)?;
        let address = socket.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let ended = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("test-turn-local-adapter".into())
            .spawn(move || {
                let mut links = HashMap::new();
                let mut bytes = vec![0; 65_536];
                while !ended.load(Ordering::Relaxed) {
                    let incoming = receive_clients(&socket, destination, &mut links, &mut bytes);
                    let outgoing = receive_replies(&socket, destination, &links, &mut bytes);
                    if !incoming && !outgoing {
                        thread::sleep(Duration::from_micros(200));
                    }
                }
            })?;
        Ok(Self {
            address,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn receive_clients(
    socket: &UdpSocket,
    destination: SocketAddr,
    links: &mut HashMap<SocketAddr, UdpSocket>,
    bytes: &mut [u8],
) -> bool {
    let Ok(local) = socket.local_addr() else { return false };
    let mut received = false;
    while let Ok((length, from)) = socket.recv_from(bytes) {
        if from.ip() != local.ip() {
            continue;
        }
        received = true;
        if let std::collections::hash_map::Entry::Vacant(entry) = links.entry(from) {
            let Ok(upstream) = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)) else {
                continue;
            };
            if upstream.set_nonblocking(true).is_err() {
                continue;
            }
            entry.insert(upstream);
        }
        if let Some(upstream) = links.get(&from) {
            let _ = upstream.send_to(&bytes[..length], destination);
        }
    }
    received
}

fn receive_replies(
    socket: &UdpSocket,
    destination: SocketAddr,
    links: &HashMap<SocketAddr, UdpSocket>,
    bytes: &mut [u8],
) -> bool {
    let mut received = false;
    for (client, upstream) in links {
        while let Ok((length, from)) = upstream.recv_from(bytes) {
            if from == destination {
                received = true;
                let _ = socket.send_to(&bytes[..length], client);
            }
        }
    }
    received
}
