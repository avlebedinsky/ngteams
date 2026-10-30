//! The worker's side (module docs of `worker`): the hello, the in-process engine started with the app's
//! configuration, the app's commands handed to it, its events written back by a thread of their own, and the end.

use std::os::fd::{AsFd as _, OwnedFd};
use std::sync::Arc;
use std::time::Duration;

use super::message::{self, FromWorker, PROTOCOL, StartFailure, ToWorker};
use super::wire::{self, Limits, Packet};
use crate::event::SchemeResponse;
use crate::host::{Command, Local};

/// How long the worker waits for its last answer to leave before it ends anyway (an app that stopped reading).
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

/// What the writer thread sends.
enum Outgoing {
    Packet(Packet),
    /// Told once everything queued before it was sent.
    Flush(std::sync::mpsc::SyncSender<()>),
}

/// Serves the engine on `socket` until the app quits it or goes away; the worker's exit code.
pub(super) fn serve(socket: OwnedFd) -> i32 {
    // inherited across the exec alone: nothing WebKit starts may keep the app's peer alive
    if let Err(err) = rustix::io::fcntl_setfd(&socket, rustix::io::FdFlags::CLOEXEC) {
        eprintln!("teams-wpe: the WPE worker cannot make its socket close-on-exec: {err}");
        return 2;
    }
    let socket = Arc::new(socket);
    let hello = FromWorker::Hello {
        protocol: PROTOCOL,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        pid: std::process::id(),
    };
    if send(&socket, &hello).is_err() {
        return 0;
    }
    let first = match wire::recv(socket.as_fd(), Limits::FROM_APP) {
        Ok(Some(packet)) => message::body::<ToWorker>(&packet),
        Ok(None) => return 0,
        Err(err) => {
            eprintln!("teams-wpe: the WPE worker's socket: {err}");
            return 2;
        }
    };
    let Ok(ToWorker::Start(config)) = first else {
        eprintln!("teams-wpe: the WPE worker was not given its configuration first");
        return 2;
    };
    let (engine, events) = match Local::start(config) {
        Ok(started) => started,
        Err(error) => {
            let _ = send(&socket, &FromWorker::Started(Err(StartFailure::from(&error))));
            return 1;
        }
    };
    if send(&socket, &FromWorker::Started(Ok(()))).is_err() {
        return 0;
    }
    let (outgoing, queue) = async_channel::unbounded::<Outgoing>();
    let writer_socket = Arc::clone(&socket);
    std::thread::spawn(move || {
        while let Ok(item) = queue.recv_blocking() {
            match item {
                Outgoing::Packet(packet) => {
                    if wire::send(writer_socket.as_fd(), &packet).is_err() {
                        // the app is gone: the reader sees the end of the socket and stops the engine before the
                        // worker ends
                        let _ = rustix::net::shutdown(&*writer_socket, rustix::net::Shutdown::Both);
                        return;
                    }
                }
                Outgoing::Flush(done) => {
                    let _ = done.send(());
                }
            }
        }
    });
    let forward = outgoing.clone();
    std::thread::spawn(move || {
        while let Ok(event) = events.recv_blocking() {
            match FromWorker::event_packet(event) {
                Ok(packet) => {
                    let _ = forward.try_send(Outgoing::Packet(packet));
                }
                Err(err) => log::error!("a WPE event could not be written for the app: {err}"),
            }
        }
    });
    serve_commands(&socket, &engine, &outgoing)
}

/// Hands the app's commands to `engine` until a quit (after which the engine's stop is told and flushed) or the end of
/// the socket; the engine stops either way, so its web processes end with the worker (`engine::stop`) rather than
/// outlive it.
fn serve_commands(socket: &OwnedFd, engine: &Local, outgoing: &async_channel::Sender<Outgoing>) -> i32 {
    let code = serve_until_end(socket, engine, outgoing);
    engine.stop();
    code
}

/// [`serve_commands`]'s loop: the worker's exit code once the app quit the engine or went away.
fn serve_until_end(socket: &OwnedFd, engine: &Local, outgoing: &async_channel::Sender<Outgoing>) -> i32 {
    loop {
        let packet = match wire::recv(socket.as_fd(), Limits::FROM_APP) {
            Ok(Some(packet)) => packet,
            Ok(None) => return 0,
            Err(err) => {
                eprintln!("teams-wpe: the WPE worker's socket broke: {err}");
                return 2;
            }
        };
        let blob = packet.blob.clone();
        match message::body::<ToWorker>(&packet) {
            Ok(ToWorker::Command(Command::Quit)) => {
                engine.stop();
                if let Ok(stopped) = FromWorker::Stopped.packet() {
                    let _ = outgoing.try_send(Outgoing::Packet(stopped));
                }
                let (done, flushed) = std::sync::mpsc::sync_channel(1);
                let _ = outgoing.try_send(Outgoing::Flush(done));
                let _ = flushed.recv_timeout(FLUSH_TIMEOUT);
                return 0;
            }
            Ok(ToWorker::Command(command)) => engine.post(command),
            Ok(ToWorker::SchemeBytes { token, mime }) => engine.post(Command::Scheme(
                token,
                SchemeResponse::Bytes {
                    data: blob.unwrap_or_default(),
                    mime,
                },
            )),
            Ok(ToWorker::Evaluate {
                id,
                view,
                script,
                world,
            }) => {
                let result = engine.evaluate(view, script, world);
                let answer = outgoing.clone();
                std::thread::spawn(move || {
                    let result = result.recv_blocking().unwrap_or_else(|_| Err("page closed".to_owned()));
                    if let Ok(packet) = (FromWorker::Evaluation { id, result }).packet() {
                        let _ = answer.try_send(Outgoing::Packet(packet));
                    }
                });
            }
            Ok(ToWorker::Start(_)) => log::warn!("the WPE worker was started twice: ignored"),
            Err(err) => {
                eprintln!("teams-wpe: the WPE worker got a broken message: {err}");
                return 2;
            }
        }
    }
}

fn send(socket: &OwnedFd, message: &FromWorker) -> Result<(), wire::WireError> {
    wire::send(socket.as_fd(), &message.packet()?)
}
