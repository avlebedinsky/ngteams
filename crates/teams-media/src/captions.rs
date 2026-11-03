//! The built-in Teams caption service's data-channel messages (data ID 3, JSON version 1.1).
//! Text is ephemeral and deliberately omitted from debug output.
use std::fmt;

use serde_json::Value;

use crate::mcp::Depacketizer;

/// One service recognition result. The speaker, audio timestamp and track identify its continuing utterance;
/// result IDs advance on each interim/final update.
#[derive(Clone, PartialEq, Eq)]
pub struct Caption {
    pub id: String,
    pub speaker: Option<String>,
    pub display_name: Option<String>,
    pub text: String,
    pub final_result: bool,
    pub spoken_language: String,
    pub track_language: String,
    pub audio_timestamp: u64,
    pub duration: u64,
}

impl fmt::Debug for Caption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Caption")
            .field("final_result", &self.final_result)
            .field("text_bytes", &self.text.len())
            .finish_non_exhaustive()
    }
}

/// Reassembles caption packets; ignores other data IDs and unsupported/malformed messages.
#[derive(Default)]
pub(crate) struct Receiver {
    framing: Depacketizer,
    pending_bytes: usize,
}

impl Receiver {
    pub(crate) fn receive(&mut self, packet: &[u8]) -> Vec<Caption> {
        if packet.get(2).is_none_or(|id| id & 63 != 3) {
            return Vec::new();
        }
        if packet.get(2).is_some_and(|flags| flags & 0x80 != 0) {
            self.pending_bytes = 0;
        }
        self.pending_bytes = self.pending_bytes.saturating_add(packet.len());
        if self.pending_bytes > 128 * 1024 {
            self.framing = Depacketizer::default();
            self.pending_bytes = 0;
            return Vec::new();
        }
        let Ok(Some(message)) = self.framing.depacketize(packet) else {
            return Vec::new();
        };
        self.pending_bytes = 0;
        read(&message.data)
    }
}

fn read(bytes: &[u8]) -> Vec<Caption> {
    if bytes.len() > 128 * 1024 {
        return Vec::new();
    }
    let Ok(body) = serde_json::from_slice::<Value>(bytes) else {
        return Vec::new();
    };
    if body.get("version").and_then(Value::as_str) != Some("1.1") {
        return Vec::new();
    }
    body.get("recognitionResults")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(32)
        .filter_map(|result| {
            let bounded = |key: &str, max: usize| {
                result
                    .get(key)?
                    .as_str()
                    .filter(|value| value.len() <= max)
                    .map(str::to_owned)
            };
            let id = bounded("id", 256).filter(|id| !id.is_empty())?;
            let text = bounded("text", 8192)?;
            Some(Caption {
                id,
                speaker: bounded("userId", 256),
                display_name: bounded("displayName", 256).filter(|name| !name.trim().is_empty()),
                text,
                final_result: result.get("isFinal")?.as_bool()?,
                spoken_language: bounded("spokenLanguage", 64).unwrap_or_default(),
                track_language: bounded("trackLanguage", 64).unwrap_or_default(),
                audio_timestamp: result
                    .get("timestampAudioSent")
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
                duration: result.get("duration").and_then(Value::as_u64).unwrap_or_default(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::{MIXER_RECIPIENT, Packetizer};
    use serde_json::json;

    #[test]
    fn actual_framing_carries_interim_and_final_without_mcp_version_prefix() {
        let mut receiver = Receiver::default();
        let mut packets = Packetizer::new(3);
        for final_result in [false, true] {
            let body = json!({"version":"1.1","recognitionResults":[{
                "id":"synthetic/1","userId":"8:orgid:00000000-0000-4000-8000-00000000000a",
                "displayName":"Lab Speaker","text":"Synthetic caption.","isFinal":final_result,
                "spokenLanguage":"en-us","trackLanguage":"en-us","timestampAudioSent":123_456_789,"duration":3_000_000
            }]});
            let packet = packets.packetize(127, &[MIXER_RECIPIENT], body.to_string().as_bytes());
            let captions = receiver.receive(&packet);
            assert_eq!(captions.len(), 1);
            assert_eq!(captions[0].final_result, final_result);
            assert_eq!(captions[0].text, "Synthetic caption.");
            assert!(!format!("{:?}", captions[0]).contains("Synthetic caption"));
        }
    }

    #[test]
    fn unsupported_and_malformed_payloads_are_ignored() {
        for body in [
            b"unreadable".as_slice(),
            br#"{"version":"2","recognitionResults":[]}"#,
            br#"{"version":"1.1","recognitionResults":[{"id":"x","text":"x","isFinal":"false"}]}"#,
        ] {
            assert!(read(body).is_empty());
        }
        assert!(read(&vec![b' '; 128 * 1024 + 1]).is_empty());
    }
}
