//! The receiving side's jitter buffer: packets go in as they arrive (in any order, some missing, some twice), and
//! come out in media-time order at the pace the speaker plays them.
//!
//! Playout starts once [`JitterConfig::depth`] packets are held, which is the delay a late or reordered packet may
//! make up. At each turn the buffer hands out the packet that comes next in media time; when that one is missing it
//! hands out the packet after it for its in-band FEC ([`Playout::Recover`]) or, with nothing to recover from, asks for
//! concealment ([`Playout::Conceal`]). After [`JitterConfig::max_concealed`] concealed packets in a row (the sender
//! went quiet, or jumped ahead in media time) it stops and fills up again. A packet whose media time has already been
//! played or concealed is late and dropped; a full buffer drops its oldest packet. Media times are 48 kHz ticks.
//!
//! The delay stays near the depth for the whole call: whenever more than twice the depth is held (a speaker that
//! stalled, a burst after an outage, a far clock running fast) the oldest packets are shed down to the depth, and a
//! restart drops what sits before a gap too long to conceal (a stale packet before a pause), so playout picks up with
//! the newest. A packet more than [`EPOCH_TICKS`] behind what the buffer has seen is a new source's (a far end that
//! changed its SSRC starts its media time anew): the buffer drops what it holds and fills up again from it.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::codec::PACKET_TICKS;

/// How far behind the playout (or the newest packet held) a packet's media time must be to start a new epoch rather
/// than count as late: one second at 48 kHz. Reordering never reaches that far; a new source's media time starts at
/// random.
pub const EPOCH_TICKS: u64 = 48_000;

/// How the buffer plays out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JitterConfig {
    /// Packets held before playout starts (and starts again after a pause).
    pub depth: usize,
    /// Most packets held; more drops the oldest.
    pub capacity: usize,
    /// Concealed packets in a row after which playout stops and the buffer fills up again.
    pub max_concealed: usize,
}

impl Default for JitterConfig {
    /// 60 ms of depth (three 20 ms packets), a second of capacity, 100 ms of concealment.
    fn default() -> Self {
        Self {
            depth: 3,
            capacity: 50,
            max_concealed: 5,
        }
    }
}

/// What the speaker gets next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Playout {
    /// The next packet: decode it.
    Packet(Arc<[u8]>),
    /// The next packet is missing but the one after it is here: decode `samples` samples from its FEC. It is still
    /// held, and comes as [`Playout::Packet`] at the next turn.
    Recover { next: Arc<[u8]>, samples: usize },
    /// The next packet is missing and can't be recovered: conceal `samples` samples.
    Conceal { samples: usize },
    /// Nothing is playing (filling up): play silence.
    Silence,
}

/// What a [`JitterBuffer::push`] did with a packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pushed {
    Held,
    /// Its media time was already played or concealed: dropped.
    Late,
    /// A packet of the same media time is held: dropped.
    Duplicate,
    /// Held, and the oldest held packet was dropped to make room.
    Overflowed,
    /// Far behind what the buffer had seen: a new epoch. Everything held before was dropped; this one is held.
    NewEpoch,
}

/// The buffer's counts since it was made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JitterStats {
    pub held: u64,
    pub late: u64,
    pub duplicate: u64,
    pub overflowed: u64,
    /// Held packets dropped unplayed to keep the delay down (shed from a backlog, stranded before a long gap) or
    /// left from an old epoch.
    pub shed: u64,
    /// New epochs started (see [`Pushed::NewEpoch`]).
    pub epochs: u64,
    pub played: u64,
    pub recovered: u64,
    pub concealed: u64,
}

struct Held {
    payload: Arc<[u8]>,
    ticks: u64,
}

/// Packets by media time, played out in order.
pub struct JitterBuffer {
    config: JitterConfig,
    packets: BTreeMap<u64, Held>,
    /// The media time of the next packet to play: everything before it was played or concealed. `None` until the
    /// first playout.
    cursor: Option<u64>,
    playing: bool,
    /// The length of the last packet played, assumed for a missing one.
    step: u64,
    concealed_run: usize,
    stats: JitterStats,
}

impl JitterBuffer {
    /// An empty buffer, filling up. A `depth` or `capacity` of 0 counts as 1, and `depth` is capped at `capacity`.
    #[must_use]
    pub fn new(config: JitterConfig) -> Self {
        let capacity = config.capacity.max(1);
        Self {
            config: JitterConfig {
                depth: config.depth.clamp(1, capacity),
                capacity,
                max_concealed: config.max_concealed,
            },
            packets: BTreeMap::new(),
            cursor: None,
            playing: false,
            step: PACKET_TICKS,
            concealed_run: 0,
            stats: JitterStats::default(),
        }
    }

    /// Takes a packet of media time `rtp_time` lasting `ticks` (48 kHz).
    pub fn push(&mut self, rtp_time: u64, ticks: u64, payload: Arc<[u8]>) -> Pushed {
        let mut pushed = Pushed::Held;
        if self
            .seen()
            .is_some_and(|seen| rtp_time.saturating_add(EPOCH_TICKS) < seen)
        {
            self.new_epoch();
            pushed = Pushed::NewEpoch;
        } else if self.cursor.is_some_and(|cursor| rtp_time < cursor) {
            self.stats.late += 1;
            return Pushed::Late;
        }
        if self.packets.contains_key(&rtp_time) {
            self.stats.duplicate += 1;
            return Pushed::Duplicate;
        }
        self.packets.insert(
            rtp_time,
            Held {
                payload,
                ticks: ticks.max(1),
            },
        );
        self.stats.held += 1;
        if self.packets.len() > self.config.capacity {
            self.packets.pop_first();
            self.stats.overflowed += 1;
            return Pushed::Overflowed;
        }
        pushed
    }

    /// What to play next, advancing the playout by one packet (or by nothing while filling up).
    pub fn pop(&mut self) -> Playout {
        if !self.playing {
            self.drop_stranded();
            if self.packets.len() < self.config.depth {
                return Playout::Silence;
            }
            self.playing = true;
            self.cursor = self.packets.first_key_value().map(|(at, _)| *at);
        }
        if self.packets.len() > self.config.depth.saturating_mul(2) {
            self.shed_to_depth();
        }
        let Some(at) = self.cursor else {
            return Playout::Silence;
        };
        if let Some(held) = self.packets.remove(&at) {
            self.step = held.ticks;
            self.cursor = Some(at + held.ticks);
            self.concealed_run = 0;
            self.stats.played += 1;
            return Playout::Packet(held.payload);
        }
        let samples = usize::try_from(self.step).unwrap_or(usize::MAX);
        if let Some(next) = self.packets.get(&(at + self.step)) {
            self.cursor = Some(at + self.step);
            self.concealed_run = 0;
            self.stats.recovered += 1;
            return Playout::Recover {
                next: Arc::clone(&next.payload),
                samples,
            };
        }
        if self.concealed_run >= self.config.max_concealed {
            self.playing = false;
            self.concealed_run = 0;
            return Playout::Silence;
        }
        self.cursor = Some(at + self.step);
        self.concealed_run += 1;
        self.stats.concealed += 1;
        Playout::Conceal { samples }
    }

    /// The furthest media time the buffer has seen: the playout's cursor or the end of the newest packet held.
    fn seen(&self) -> Option<u64> {
        let newest = self
            .packets
            .last_key_value()
            .map(|(at, held)| at.saturating_add(held.ticks));
        self.cursor.max(newest)
    }

    /// Drops everything held and the cursor, and fills up again.
    fn new_epoch(&mut self) {
        self.shed(self.packets.len());
        self.cursor = None;
        self.playing = false;
        self.concealed_run = 0;
        self.stats.epochs += 1;
    }

    /// Before a restart: drops the packets before the last gap longer than playout would conceal, which would only
    /// play, conceal and stop again ahead of what comes after it.
    fn drop_stranded(&mut self) {
        let bridged = u64::try_from(self.config.max_concealed)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let mut before = 0;
        // the end and length of the packet before
        let mut previous: Option<(u64, u64)> = None;
        for (index, (&at, held)) in self.packets.iter().enumerate() {
            if let Some((end, ticks)) = previous
                && at > end.saturating_add(bridged.saturating_mul(ticks))
            {
                before = index;
            }
            previous = Some((at.saturating_add(held.ticks), held.ticks));
        }
        self.shed(before);
    }

    /// Sheds the oldest packets down to the depth and moves the cursor to the first one left.
    fn shed_to_depth(&mut self) {
        self.shed(self.packets.len().saturating_sub(self.config.depth));
        if let Some((&at, _)) = self.packets.first_key_value() {
            self.cursor = Some(at);
            self.concealed_run = 0;
        }
    }

    /// Drops the `count` oldest packets held.
    fn shed(&mut self, count: usize) {
        for _ in 0..count {
            if self.packets.pop_first().is_some() {
                self.stats.shed += 1;
            }
        }
    }

    /// Packets held now.
    #[must_use]
    pub fn len(&self) -> usize {
        self.packets.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    #[must_use]
    pub fn stats(&self) -> JitterStats {
        self.stats
    }
}
