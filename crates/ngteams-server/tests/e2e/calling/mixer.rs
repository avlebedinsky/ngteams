//! A media node for signaling tests: it keeps the rooms and legs signaling asks for and answers fixed SDP that names
//! the leg, so a test tells whose offer or answer reached whom. No media flows.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

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
pub struct FixedMixer(Arc<Mutex<Rooms>>);

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
        let rooms = self.0.lock().unwrap();
        rooms.rooms.get(call).cloned().unwrap_or_default()
    }

    /// Whether the call `call`'s room was dropped.
    pub fn dropped(&self, call: &str) -> bool {
        self.0.lock().unwrap().dropped.iter().any(|dropped| dropped == call)
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
        let mut rooms = self.0.lock().unwrap();
        let legs = rooms.rooms.entry(call.to_owned()).or_default();
        let kept = legs.entry(leg.to_owned()).or_insert(Leg {
            sources,
            answered: false,
            renegotiations: 0,
        });
        Ok(match offer {
            Some(_) => sdp(answer_on(leg, kept.renegotiations)),
            None => sdp(offer_to(leg)),
        })
    }

    async fn answer_leg(&self, _home: &MediaHome, call: &str, leg: &str, _answer: &Sdp) -> Result<(), MediaError> {
        let mut rooms = self.0.lock().unwrap();
        let kept = rooms
            .rooms
            .get_mut(call)
            .and_then(|legs| legs.get_mut(leg))
            .ok_or(MediaError::Gone)?;
        kept.answered = true;
        Ok(())
    }

    async fn renegotiate_leg(&self, _home: &MediaHome, call: &str, leg: &str, _offer: &Sdp) -> Result<Sdp, MediaError> {
        let mut rooms = self.0.lock().unwrap();
        let kept = rooms
            .rooms
            .get_mut(call)
            .and_then(|legs| legs.get_mut(leg))
            .ok_or(MediaError::Gone)?;
        kept.renegotiations += 1;
        Ok(sdp(answer_on(leg, kept.renegotiations)))
    }

    async fn delete_leg(&self, _home: &MediaHome, call: &str, leg: &str) -> Result<(), MediaError> {
        let mut rooms = self.0.lock().unwrap();
        if let Some(legs) = rooms.rooms.get_mut(call) {
            legs.remove(leg);
        }
        Ok(())
    }

    async fn delete_room(&self, _home: &MediaHome, call: &str) -> Result<(), MediaError> {
        let mut rooms = self.0.lock().unwrap();
        rooms.rooms.remove(call);
        rooms.dropped.push(call.to_owned());
        Ok(())
    }
}
