//! The media thread: the node's one UDP socket, every room's legs on it, and their ticks. No tokio inside: the
//! internal API sends [`Command`]s and waits for the reply; a datagram goes to the leg whose `Rtc` accepts it. A leg
//! whose media is lost is reported ([`Lost`]), and the node's async side takes its party out of the call; a leg still
//! there [`REPORT_AGAIN`] later (that step failed) is reported again.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use serde::Serialize;
use str0m::Input;
use str0m::net::{Protocol, Receive};
use tokio::sync::{mpsc, oneshot};

use super::leg::{Leg, LegError, Making};
use super::room::{Room, TICK};
use crate::calling::{Sdp, Sources};

/// The longest a datagram the socket reads.
const DATAGRAM: usize = 2000;
/// How often the node judges its legs' health.
const HEALTH_EVERY: Duration = Duration::from_millis(500);
/// How long after its report a lost leg still in its room is reported again.
pub(crate) const REPORT_AGAIN: Duration = Duration::from_secs(5);

/// Why the node didn't do what it was asked.
#[derive(Debug, thiserror::Error)]
pub(crate) enum NodeError {
    /// Over `mixer.max_rooms` or `mixer.max_legs_per_room`.
    #[error("the node is full")]
    Full,
    #[error("no such room or leg")]
    Missing,
    #[error(transparent)]
    Leg(#[from] LegError),
}

type Reply<T> = oneshot::Sender<Result<T, NodeError>>;

/// What the internal API asks the media thread.
#[derive(Debug)]
pub(crate) enum Command {
    Put {
        call: String,
        leg: String,
        sources: Sources,
        offer: Option<Sdp>,
        reply: Reply<Sdp>,
    },
    Answer {
        call: String,
        leg: String,
        answer: Sdp,
        reply: Reply<()>,
    },
    Renegotiate {
        call: String,
        leg: String,
        offer: Sdp,
        reply: Reply<Sdp>,
    },
    DeleteLeg {
        call: String,
        leg: String,
        reply: Reply<()>,
    },
    DeleteRoom {
        call: String,
        reply: Reply<()>,
    },
    Room {
        call: String,
        reply: oneshot::Sender<Option<Vec<LegView>>>,
    },
    Load {
        reply: oneshot::Sender<Load>,
    },
    /// The node stops: every room goes.
    Stop,
}

/// A leg as the internal API shows it.
#[derive(Clone, Debug, Serialize)]
pub struct LegView {
    pub id: String,
    pub sources: Sources,
    /// Whether its DTLS came up.
    pub connected: bool,
    /// Whether its control plane's handshake was answered.
    pub control: bool,
}

/// What the node carries: its rooms (by call) and how many legs.
#[derive(Clone, Debug, Default)]
pub(crate) struct Load {
    pub rooms: Vec<String>,
    pub legs: usize,
}

/// A leg whose media was lost: its call and leg ids.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Lost {
    pub call: String,
    pub leg: String,
}

/// What the node is run with.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub making: Making,
    pub max_rooms: usize,
    pub max_legs_per_room: usize,
    pub media_timeout: Duration,
    pub connect_timeout: Duration,
}

struct Node {
    socket: UdpSocket,
    /// The address a datagram the socket got was sent to, as the legs' candidates name it.
    destination: SocketAddr,
    settings: Settings,
    rooms: BTreeMap<String, Room>,
    lost: mpsc::UnboundedSender<Lost>,
    /// The lost legs reported, and when.
    reported: BTreeMap<Lost, Instant>,
    judged: Instant,
}

/// Runs the media thread until every sender of `commands` is gone.
pub(crate) fn run(
    socket: UdpSocket,
    settings: Settings,
    commands: &Receiver<Command>,
    lost: mpsc::UnboundedSender<Lost>,
) {
    let Some(destination) = settings.making.candidates.first().copied() else {
        return;
    };
    let mut node = Node {
        socket,
        destination,
        settings,
        rooms: BTreeMap::new(),
        lost,
        reported: BTreeMap::new(),
        judged: Instant::now(),
    };
    let mut buffer = vec![0_u8; DATAGRAM];
    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => return,
                Ok(command) => node.command(command, Instant::now()),
                Err(TryRecvError::Empty) => break,
            }
        }
        node.drive(Instant::now());
        node.read(&mut buffer);
    }
}

impl Node {
    fn command(&mut self, command: Command, now: Instant) {
        match command {
            Command::Put {
                call,
                leg,
                sources,
                offer,
                reply,
            } => {
                let _ = reply.send(self.put(call, leg, sources, offer.as_ref(), now));
            }
            Command::Answer {
                call,
                leg,
                answer,
                reply,
            } => {
                let _ = reply.send(self.leg(&call, &leg).and_then(|leg| Ok(leg.accept(&answer, now)?)));
            }
            Command::Renegotiate {
                call,
                leg,
                offer,
                reply,
            } => {
                let _ = reply.send(self.leg(&call, &leg).and_then(|leg| Ok(leg.renegotiate(&offer, now)?)));
            }
            Command::DeleteLeg { call, leg, reply } => {
                if let Some(room) = self.rooms.get_mut(&call) {
                    room.remove(&leg);
                }
                self.reported.retain(|lost, _| lost.call != call || lost.leg != leg);
                let _ = reply.send(Ok(()));
            }
            Command::DeleteRoom { call, reply } => {
                self.drop_room(&call);
                let _ = reply.send(Ok(()));
            }
            Command::Room { call, reply } => {
                let _ = reply.send(self.rooms.get(&call).map(view));
            }
            Command::Stop => {}
            Command::Load { reply } => {
                let _ = reply.send(Load {
                    rooms: self.rooms.keys().cloned().collect(),
                    legs: self.rooms.values().map(|room| room.legs.len()).sum(),
                });
            }
        }
    }

    fn put(
        &mut self,
        call: String,
        id: String,
        sources: Sources,
        offer: Option<&Sdp>,
        now: Instant,
    ) -> Result<Sdp, NodeError> {
        if let Some(leg) = self.rooms.get(&call).and_then(|room| room.legs.get(&id)) {
            return Ok(leg.first.clone());
        }
        if !self.rooms.contains_key(&call) && self.rooms.len() >= self.settings.max_rooms {
            return Err(NodeError::Full);
        }
        let room = self.rooms.entry(call).or_insert_with(|| Room::new(now));
        if room.legs.len() >= self.settings.max_legs_per_room {
            return Err(NodeError::Full);
        }
        let leg = Leg::new(&self.settings.making, sources, offer, now)?;
        let first = leg.first.clone();
        room.legs.insert(id, leg);
        Ok(first)
    }

    fn leg(&mut self, call: &str, leg: &str) -> Result<&mut Leg, NodeError> {
        self.rooms
            .get_mut(call)
            .and_then(|room| room.legs.get_mut(leg))
            .ok_or(NodeError::Missing)
    }

    fn drop_room(&mut self, call: &str) {
        if let Some(mut room) = self.rooms.remove(call) {
            let legs: Vec<String> = room.legs.keys().cloned().collect();
            for leg in legs {
                room.remove(&leg);
            }
        }
        self.reported.retain(|lost, _| lost.call != call);
    }

    /// Every leg's timeouts and output, every room's due ticks, and now and then the legs' health.
    fn drive(&mut self, now: Instant) {
        let (socket, rooms) = (&self.socket, &mut self.rooms);
        let mut send = |to: SocketAddr, datagram: &[u8]| {
            if let Err(error) = socket.send_to(datagram, to) {
                tracing::debug!(%error, "a media datagram wasn't sent");
            }
        };
        for room in rooms.values_mut() {
            let mut up = Vec::new();
            for (id, leg) in &mut room.legs {
                leg.tick(now);
                if leg.drive(now, &mut send).control_up {
                    up.push(id.clone());
                }
            }
            for id in up {
                room.control_up(&id);
            }
            room.run_ticks(now);
            for leg in room.legs.values_mut() {
                leg.drive(now, &mut send);
            }
        }
        if now.duration_since(self.judged) >= HEALTH_EVERY {
            self.judged = now;
            self.judge(now);
        }
    }

    /// Reports each leg whose connection stayed down, or never came up after its exchange, too long, unless it was
    /// reported less than [`REPORT_AGAIN`] ago.
    fn judge(&mut self, now: Instant) {
        let (media_timeout, connect_timeout) = (self.settings.media_timeout, self.settings.connect_timeout);
        for (call, room) in &self.rooms {
            for (id, leg) in &room.legs {
                let health = leg.health;
                let down = health
                    .down_since
                    .is_some_and(|since| now.duration_since(since) >= media_timeout);
                let unconnected = !health.connected
                    && health
                        .exchanged
                        .is_some_and(|since| now.duration_since(since) >= connect_timeout);
                let lost = Lost {
                    call: call.clone(),
                    leg: id.clone(),
                };
                let due = self
                    .reported
                    .get(&lost)
                    .is_none_or(|at| now.duration_since(*at) >= REPORT_AGAIN);
                if (down || unconnected) && due {
                    let _ = self.lost.send(lost.clone());
                    self.reported.insert(lost, now);
                }
            }
        }
    }

    /// Waits for a datagram until the next leg or room is due, and hands it to its leg.
    fn read(&mut self, buffer: &mut [u8]) {
        let now = Instant::now();
        let due = self
            .rooms
            .values()
            .flat_map(|room| room.legs.values().map(|leg| leg.due).chain([room.next_tick]))
            .min()
            .unwrap_or(now + TICK);
        let wait = due.saturating_duration_since(now).clamp(Duration::from_millis(1), TICK);
        if self.socket.set_read_timeout(Some(wait)).is_err() {
            return;
        }
        let (length, source) = match self.socket.recv_from(buffer) {
            Ok(read) => read,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return,
            Err(error) => {
                tracing::debug!(%error, "the media socket failed a read");
                return;
            }
        };
        let Some(datagram) = buffer.get(..length) else { return };
        let Ok(contents) = datagram.try_into() else { return };
        let receive = Receive {
            proto: Protocol::Udp,
            source,
            destination: self.destination,
            contents,
        };
        let now = Instant::now();
        let input = Input::Receive(now, receive);
        let leg = self
            .rooms
            .values_mut()
            .flat_map(|room| room.legs.values_mut())
            .find(|leg| leg.accepts(&input));
        if let (Some(leg), Input::Receive(_, receive)) = (leg, input) {
            leg.receive(now, receive);
        }
    }
}

fn view(room: &Room) -> Vec<LegView> {
    room.legs
        .iter()
        .map(|(id, leg)| LegView {
            id: id.clone(),
            sources: leg.sources,
            connected: leg.health.connected,
            control: leg.control_up(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node() -> (Node, mpsc::UnboundedReceiver<Lost>) {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let destination = socket.local_addr().unwrap();
        let (lost, losses) = mpsc::unbounded_channel();
        let settings = Settings {
            making: Making {
                candidates: vec![destination],
                ice_lite: true,
            },
            max_rooms: 4,
            max_legs_per_room: 4,
            media_timeout: Duration::from_secs(20),
            connect_timeout: Duration::from_secs(30),
        };
        let node = Node {
            socket,
            destination,
            settings,
            rooms: BTreeMap::new(),
            lost,
            reported: BTreeMap::new(),
            judged: Instant::now(),
        };
        (node, losses)
    }

    #[test]
    fn a_lost_leg_still_in_its_room_is_reported_again_now_and_then() {
        let now = Instant::now();
        let (mut node, mut losses) = node();
        node.put("call".into(), "leg".into(), Sources::default(), None, now)
            .unwrap();
        // the callee's answer was taken, and its leg never connected
        node.leg("call", "leg").unwrap().health.exchanged = Some(now);
        let lost = Lost {
            call: "call".into(),
            leg: "leg".into(),
        };
        let late = now + node.settings.connect_timeout;
        node.judge(late);
        assert_eq!(losses.try_recv().unwrap(), lost);
        node.judge(late + HEALTH_EVERY);
        assert!(losses.try_recv().is_err(), "reported once, not every look");
        node.judge(late + REPORT_AGAIN);
        assert_eq!(losses.try_recv().unwrap(), lost, "its step failed: reported again");
        let (reply, _) = oneshot::channel();
        node.command(
            Command::DeleteLeg {
                call: "call".into(),
                leg: "leg".into(),
                reply,
            },
            late,
        );
        node.judge(late + 3 * REPORT_AGAIN);
        assert!(losses.try_recv().is_err(), "a leg that went isn't reported");
    }
}
