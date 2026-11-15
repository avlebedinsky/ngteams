//! A call's room: its legs and its 20 ms tick, on the node's monotonic clock. Each tick takes a frame from every leg,
//! mixes for each listener every active source but its own, and sends the mix; the dominant speakers are told when
//! they change (at most every 300 ms) and to a leg once its control plane comes up.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::audio::{self, Activity, FRAME, Heard, Speakers};
use super::leg::Leg;

/// The tick.
pub(crate) const TICK: Duration = Duration::from_millis(20);
/// The dominant speakers are told at most this often.
const SPEAKERS_EVERY: Duration = Duration::from_millis(300);

/// One call's room.
#[derive(Debug)]
pub(crate) struct Room {
    pub legs: BTreeMap<String, Leg>,
    activity: BTreeMap<String, Activity>,
    speakers: Speakers,
    /// The history last told, and when.
    told: Vec<u32>,
    told_at: Option<Instant>,
    /// When the next tick is due.
    pub next_tick: Instant,
}

impl Room {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            legs: BTreeMap::new(),
            activity: BTreeMap::new(),
            speakers: Speakers::default(),
            told: Vec::new(),
            told_at: None,
            next_tick: now + TICK,
        }
    }

    /// Takes the leg `id` out.
    pub(crate) fn remove(&mut self, id: &str) {
        if let Some(mut leg) = self.legs.remove(id) {
            leg.close();
            self.speakers.forget(leg.sources.audio);
        }
        self.activity.remove(id);
    }

    /// A leg's control plane came up: it hears who spoke so far.
    pub(crate) fn control_up(&mut self, id: &str) {
        let history = self.speakers.history().to_vec();
        if let Some(leg) = self.legs.get_mut(id)
            && !history.is_empty()
        {
            leg.tell_speakers(&history);
        }
    }

    /// The ticks due by `now`, each a frame of the mix; a room that fell behind more than a few ticks skips ahead.
    pub(crate) fn run_ticks(&mut self, now: Instant) {
        if now.duration_since(self.next_tick) > 5 * TICK {
            self.next_tick = now;
        }
        while self.next_tick <= now {
            self.tick(now);
            self.next_tick += TICK;
        }
    }

    fn tick(&mut self, now: Instant) {
        let mut heard = Vec::with_capacity(self.legs.len());
        for (id, leg) in &mut self.legs {
            let mut frame = [0.0; FRAME];
            leg.take(&mut frame);
            let level = audio::dbfs(&frame);
            let active = self.activity.entry(id.clone()).or_default().hear(level);
            self.speakers.hear(leg.sources.audio, level);
            heard.push(Heard { frame, active });
        }
        for (listener, leg) in self.legs.values_mut().enumerate() {
            leg.send(audio::mix_for(listener, &heard), now);
        }
        self.tell_speakers(now);
    }

    fn tell_speakers(&mut self, now: Instant) {
        let history = self.speakers.history();
        let due = self.told_at.is_none_or(|at| now.duration_since(at) >= SPEAKERS_EVERY);
        if history == self.told.as_slice() || !due {
            return;
        }
        self.told = history.to_vec();
        self.told_at = Some(now);
        for leg in self.legs.values_mut() {
            leg.tell_speakers(&self.told);
        }
    }
}
