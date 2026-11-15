//! The mixer's end of a leg's control plane: MCP messages on the client's `main-channel` (`teams_sdp::mcp`). A `syn`
//! is answered with an `ack` naming what the mixer sends and takes; the mixer tells the dominant speakers (`dsh`).
//! Any other message is ignored: the native client runs without `heartbeat`, `bwe` and the rest.

use serde_json::{Value, json};
use teams_sdp::mcp::{Depacketizer, MCP_DATA_ID, MIXER_RECIPIENT, Packetizer};

/// What the mixer's `ack` says it sends and takes: the dominant speakers only, so the client asks for no video source.
const SEND_CAPABILITIES: [&str; 1] = ["dsh"];
const RECEIVE_CAPABILITIES: [&str; 0] = [];
/// The version byte in front of every message array.
const VERSION: u8 = 1;

/// One leg's control plane.
#[derive(Debug)]
pub(crate) struct Control {
    depacketizer: Depacketizer,
    packetizer: Packetizer,
    acked: bool,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            depacketizer: Depacketizer::new(),
            packetizer: Packetizer::new(MCP_DATA_ID),
            acked: false,
        }
    }
}

impl Control {
    /// Whether the client's handshake was answered.
    pub(crate) fn acked(&self) -> bool {
        self.acked
    }

    /// A packet the client sent on its channel: the packets that answer it.
    pub(crate) fn receive(&mut self, packet: &[u8]) -> Vec<Vec<u8>> {
        let Ok(Some(message)) = self.depacketizer.depacketize(packet) else {
            return Vec::new();
        };
        let Some((&VERSION, body)) = message.data.split_first() else {
            return Vec::new();
        };
        let Ok(Value::Array(messages)) = serde_json::from_slice::<Value>(body) else {
            return Vec::new();
        };
        let mut answers = Vec::new();
        for message in messages {
            if message.get("type").and_then(Value::as_str) == Some("syn") {
                self.acked = true;
                let ack = json!([{"type": "ack", "send_capabilities": SEND_CAPABILITIES,
                    "receive_capabilities": RECEIVE_CAPABILITIES}]);
                answers.push(self.packet(&ack));
            }
        }
        answers
    }

    /// The packet that tells the dominant speakers, `history` the newest first.
    pub(crate) fn dominant(&mut self, history: &[u32]) -> Vec<u8> {
        self.packet(&json!([{"type": "dsh", "history": history}]))
    }

    fn packet(&mut self, messages: &Value) -> Vec<u8> {
        let mut payload = vec![VERSION];
        payload.extend_from_slice(messages.to_string().as_bytes());
        self.packetizer.packetize(MIXER_RECIPIENT, &[], &payload)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use teams_sdp::mcp::{CLIENT_CAPABILITIES, ControlPlane, McpEvent, McpState};

    use super::*;

    #[test]
    fn the_clients_handshake_is_acked_and_it_hears_the_dominant_speakers() {
        let mut client = ControlPlane::new(Some(1003), &CLIENT_CAPABILITIES);
        let mut mixer = Control::default();
        let syn = client.start(Instant::now());
        assert!(!mixer.acked());
        let answers = mixer.receive(&syn);
        assert!(mixer.acked());
        let [ack] = answers.as_slice() else {
            panic!("one answer: {answers:?}");
        };
        client.receive(ack);
        assert_eq!(client.state(), McpState::Active);
        // the mixer takes no source request: the client asks for none
        assert!(!client.takes_source_requests());
        let told = client.receive(&mixer.dominant(&[1010, 1000]));
        assert!(told.contains(&McpEvent::DominantSpeakers(vec![1010, 1000])), "{told:?}");
    }

    #[test]
    fn what_isnt_a_handshake_is_ignored() {
        let mut mixer = Control::default();
        let mut packetizer = Packetizer::new(MCP_DATA_ID);
        let mut message = |payload: &[u8]| mixer.receive(&packetizer.packetize(1003, &[MIXER_RECIPIENT], payload));
        assert!(message(b"\x01[{\"type\":\"heartbeat\"}]").is_empty());
        assert!(message(b"\x02[{\"type\":\"syn\"}]").is_empty());
        assert!(message(b"\x01{\"type\":\"syn\"}").is_empty());
        assert!(message(b"").is_empty());
        assert!(mixer.receive(b"not a packet").is_empty());
        assert!(!mixer.acked());
    }
}
