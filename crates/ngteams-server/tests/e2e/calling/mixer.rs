//! A media node for signaling tests: it keeps the rooms and legs signaling asks for and answers fixed SDP that names
//! the leg, so a test tells whose offer or answer reached whom. A test can make it fail the legs it is asked for, hold
//! new rooms until several are asked for at once, or hold the next answer or new leg (before or once it is made) until
//! it lets it go. No media flows.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{Barrier, Notify};

use async_trait::async_trait;
use ngteams_server::calling::{MediaError, MediaHome, MediaNodes, Sdp, Sources};

const CONTENT_TYPE: &str = "application/sdp-ngc-1.0";

/// What the node holds of one leg.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leg {
    pub sources: Sources,
    /// The callee's answer to the node's offer, once it came.
    pub answered: bool,
    pub renegotiations: u32,
}

#[derive(Debug, Default)]
struct Rooms {
    /// By call, then leg.
    rooms: BTreeMap<String, BTreeMap<String, Leg>>,
    /// The rooms dropped, by call.
    dropped: Vec<String>,
}

/// The node, shared by every server of a test.
#[derive(Clone, Debug, Default)]
pub struct FixedMixer {
    rooms: Arc<Mutex<Rooms>>,
    /// Every leg asked for fails.
    failing: Arc<AtomicBool>,
    /// New rooms wait here for each other.
    gate: Arc<Mutex<Option<Arc<Barrier>>>>,
    /// The next answer waits here.
    answers: Arc<Mutex<Option<Held>>>,
    /// The next new leg waits here.
    new_legs: Arc<Mutex<Option<Held>>>,
    /// The next new leg waits here once it is made.
    made_legs: Arc<Mutex<Option<Held>>>,
}

/// A request the node holds: told when it is reached, answered once let go.
#[derive(Clone, Debug, Default)]
pub struct Held {
    reached: Arc<Notify>,
    go: Arc<Notify>,
}

impl Held {
    /// Until the held request reaches the node.
    pub async fn reached(&self) {
        self.reached.notified().await;
    }

    /// The held request is answered.
    pub fn go(&self) {
        self.go.notify_one();
    }

    async fn hold(slot: &Mutex<Option<Self>>) {
        let held = slot.lock().unwrap().take();
        if let Some(held) = held {
            held.reached.notify_one();
            held.go.notified().await;
        }
    }
}

/// The node's offer to the leg `leg`.
pub fn offer_to(leg: &str) -> String {
    format!("v=0\r\no=mixer 1 1 IN IP4 192.0.2.1\r\ns=offer to {leg}\r\n")
}

/// The node's answer on the leg `leg`, after `renegotiations` renegotiations.
pub fn answer_on(leg: &str, renegotiations: u32) -> String {
    format!(
        "v=0\r\no=mixer 1 {} IN IP4 192.0.2.1\r\ns=answer on {leg}\r\n",
        renegotiations + 1
    )
}

fn sdp(blob: String) -> Sdp {
    Sdp {
        blob,
        content_type: CONTENT_TYPE.to_owned(),
    }
}

impl FixedMixer {
    /// The legs of the call `call`'s room, by leg id; empty when it has none or is dropped.
    pub fn legs(&self, call: &str) -> BTreeMap<String, Leg> {
        let rooms = self.rooms.lock().unwrap();
        rooms.rooms.get(call).cloned().unwrap_or_default()
    }

    /// The calls whose rooms the node holds.
    pub fn rooms(&self) -> Vec<String> {
        self.rooms.lock().unwrap().rooms.keys().cloned().collect()
    }

    /// Whether the call `call`'s room was dropped.
    pub fn dropped(&self, call: &str) -> bool {
        self.rooms.lock().unwrap().dropped.iter().any(|dropped| dropped == call)
    }

    /// How many rooms were dropped.
    pub fn dropped_rooms(&self) -> usize {
        self.rooms.lock().unwrap().dropped.len()
    }

    /// From now on every leg asked for fails (`failing`), or is made again.
    pub fn fail_legs(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
    }

    /// The next `rooms` new rooms are picked only once all of them are asked for.
    pub fn hold_rooms(&self, rooms: usize) {
        *self.gate.lock().unwrap() = Some(Arc::new(Barrier::new(rooms)));
    }

    /// The next answer to an offer of the node's waits until let go.
    pub fn hold_answer(&self) -> Held {
        let held = Held::default();
        *self.answers.lock().unwrap() = Some(held.clone());
        held
    }

    /// The next leg asked for waits until let go.
    pub fn hold_new_leg(&self) -> Held {
        let held = Held::default();
        *self.new_legs.lock().unwrap() = Some(held.clone());
        held
    }

    /// The next leg asked for is made, then its answer or offer waits until let go.
    pub fn hold_made_leg(&self) -> Held {
        let held = Held::default();
        *self.made_legs.lock().unwrap() = Some(held.clone());
        held
    }

    /// The one leg with the source ids `audio`.
    pub fn leg_of(&self, call: &str, audio: i64) -> Option<String> {
        self.legs(call)
            .into_iter()
            .find(|(_, leg)| i64::from(leg.sources.audio) == audio)
            .map(|(id, _)| id)
    }
}

#[async_trait]
impl MediaNodes for FixedMixer {
    async fn pick(&self) -> Result<MediaHome, MediaError> {
        let gate = self.gate.lock().unwrap().clone();
        if let Some(gate) = gate
            && gate.wait().await.is_leader()
        {
            *self.gate.lock().unwrap() = None;
        }
        Ok(MediaHome {
            node: "fixed".into(),
            url: "http://fixed.invalid".into(),
            epoch: 1,
        })
    }

    async fn put_leg(
        &self,
        _home: &MediaHome,
        call: &str,
        leg: &str,
        sources: Sources,
        offer: Option<&Sdp>,
    ) -> Result<Sdp, MediaError> {
        Held::hold(&self.new_legs).await;
        if self.failing.load(Ordering::SeqCst) {
            return Err(MediaError::Failed("the test fails it".into()));
        }
        let renegotiations = {
            let mut rooms = self.rooms.lock().unwrap();
            let legs = rooms.rooms.entry(call.to_owned()).or_default();
            let kept = legs.entry(leg.to_owned()).or_insert(Leg {
                sources,
                answered: false,
                renegotiations: 0,
            });
            kept.renegotiations
        };
        Held::hold(&self.made_legs).await;
        Ok(match offer {
            Some(_) => sdp(answer_on(leg, renegotiations)),
            None => sdp(offer_to(leg)),
        })
    }

    async fn answer_leg(&self, _home: &MediaHome, call: &str, leg: &str, _answer: &Sdp) -> Result<(), MediaError> {
        Held::hold(&self.answers).await;
        let mut rooms = self.rooms.lock().unwrap();
        let kept = rooms
            .rooms
            .get_mut(call)
            .and_then(|legs| legs.get_mut(leg))
            .ok_or(MediaError::Gone)?;
        kept.answered = true;
        Ok(())
    }

    async fn renegotiate_leg(&self, _home: &MediaHome, call: &str, leg: &str, _offer: &Sdp) -> Result<Sdp, MediaError> {
        let mut rooms = self.rooms.lock().unwrap();
        let kept = rooms
            .rooms
            .get_mut(call)
            .and_then(|legs| legs.get_mut(leg))
            .ok_or(MediaError::Gone)?;
        kept.renegotiations += 1;
        Ok(sdp(answer_on(leg, kept.renegotiations)))
    }

    async fn delete_leg(&self, _home: &MediaHome, call: &str, leg: &str) -> Result<(), MediaError> {
        let mut rooms = self.rooms.lock().unwrap();
        if let Some(legs) = rooms.rooms.get_mut(call) {
            legs.remove(leg);
        }
        Ok(())
    }

    async fn delete_room(&self, _home: &MediaHome, call: &str) -> Result<(), MediaError> {
        let mut rooms = self.rooms.lock().unwrap();
        rooms.rooms.remove(call);
        rooms.dropped.push(call.to_owned());
        Ok(())
    }
}
