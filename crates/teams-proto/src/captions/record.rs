//! Recording and transcription (More → Record and transcribe → Start recording, and Stop recording), as the web client
//! posts them to the recording bot's command link once the bot is in the call (`docs/protocol/calling.md`, "More of a
//! running call"). The bot is invited as for captions ([`CaptionWrite::invite`]); its command link carries the
//! account's recorder token only, as the web client sends it (no skypetoken).

use serde_json::{Value, json};

use super::{
    CaptionCommandLink, CaptionWrite, CaptionWriteError, CaptionWriteKind, SPOKEN_LANGUAGES, fresh_guid, timestamp,
};
use crate::{CallContext, CallParticipant};

/// What both captured recordings record: the recording and its transcript.
const PROCESSING_MODES: [&str; 2] = ["recording", "realTimeTranscript"];

/// The longest call title a recording's start names.
const MAX_TITLE: usize = 256;

/// What a recording's start names besides the call: its title, its organizer (a meeting's), the spoken language picked
/// and when it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordStart {
    /// The call's title as Teams names its recording: "Call with <name>" for a call, the meeting's title for a meeting.
    pub title: String,
    /// The meeting organizer's name, which the web client names in a meeting's start (not in a call's).
    pub organizer: Option<String>,
    /// The spoken language picked, one of [`SPOKEN_LANGUAGES`]' codes.
    pub language: String,
    /// When it starts, in local time as `YYYYMMDD_HHMMSS`, as the recording's file name has it.
    pub stamp: String,
}

impl RecordStart {
    fn valid(&self) -> bool {
        let named =
            |text: &str| !text.trim().is_empty() && text.len() <= MAX_TITLE && !text.chars().any(char::is_control);
        named(&self.title)
            && self.organizer.as_deref().is_none_or(named)
            && SPOKEN_LANGUAGES.iter().any(|(code, _)| *code == self.language)
            && stamp(&self.stamp)
    }
}

/// `YYYYMMDD_HHMMSS`.
fn stamp(value: &str) -> bool {
    value.len() == 15
        && value.bytes().enumerate().all(|(index, byte)| {
            if index == 8 {
                byte == b'_'
            } else {
                byte.is_ascii_digit()
            }
        })
}

/// The recorder's features, as both captured starts (and the bot's invitation) named them.
fn features() -> Value {
    json!({"enablePPTSharing":true,"intermediateLiveCaptions":false,"actionItemsEnabled":false,
        "enableEmailAndMeetingLanguageModel":true,"ceoSummit":false,"useUnmixedAudio":true,
        "enableTranscriptMeetingChaptering":false})
}

impl CaptionWrite {
    /// Starts recording and transcription on the recording bot's command link, as More → Record and transcribe →
    /// Start recording's Confirm sends it: the recording saved to the account's OneDrive (`Recordings`, named
    /// `<title>-<stamp>-Meeting Recording`), its transcript in `start.language`.
    ///
    /// # Errors
    /// Rejects an actor without its participant leg, an invalid conversation, an unsupported language, a title with
    /// control characters (or none) and a stamp not `YYYYMMDD_HHMMSS`.
    pub fn record(
        context: &CallContext,
        link: &CaptionCommandLink,
        from: &CallParticipant,
        thread: &str,
        start: &RecordStart,
    ) -> Result<Self, CaptionWriteError> {
        if !start.valid() {
            return Err(CaptionWriteError);
        }
        let mut write = Self::start(context, link, from, thread)?;
        let mut parameters = json!({
            "recordingFeatures":features(),
            "recordingStorageSettings":[{"StorageType":"OnedriveForBusiness","StorageLocation":"Recordings",
                "FileName":format!("{}-{}-Meeting Recording", start.title, start.stamp)}],
            "correlationId":fresh_guid()?,"meetingTitle":start.title,"exchangeId":null,
            "recordingMode":"Normal","spokenLanguage":start.language,"type":"start"
        });
        if let Some(organizer) = &start.organizer {
            parameters["meetingOrganizer"] = json!(organizer);
        }
        write.kind = CaptionWriteKind::Record;
        write.body = json!({"timestamp":timestamp(),"participantMri":from.mri.as_str(),
            "participantLegId":from.participant_id,"action":"start","processingModes":PROCESSING_MODES,
            "actionParameters":parameters});
        Ok(write)
    }

    /// Stops the recording and its transcription, as Stop recording's "Stop recording and transcription?" → Stop
    /// sends it. Anyone in the call may stop it, not only who started it.
    ///
    /// # Errors
    /// Rejects an actor without its participant leg or an invalid conversation.
    pub fn stop_recording(
        context: &CallContext,
        link: &CaptionCommandLink,
        from: &CallParticipant,
        thread: &str,
    ) -> Result<Self, CaptionWriteError> {
        let mut write = Self::start(context, link, from, thread)?;
        write.kind = CaptionWriteKind::StopRecord;
        write.body = json!({"timestamp":timestamp(),"participantMri":from.mri.as_str(),
            "participantLegId":from.participant_id,"action":"stop","processingModes":PROCESSING_MODES});
        Ok(write)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mri;

    const COMMAND: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/aks-prod-zz-api.callrecorder.teams.cloud.microsoft:21606/v2/oncommand/00000000-0000-4000-8000-000000000001";
    const CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

    fn actor() -> CallParticipant {
        CallParticipant {
            mri: Mri::new("8:orgid:00000000-0000-4000-8000-00000000000a"),
            display_name: Some("Lab A".into()),
            endpoint_id: "00000000-0000-4000-8000-0000000000e1".into(),
            participant_id: Some("00000000-0000-4000-8000-0000000000e2".into()),
            language_id: Some("en-us".into()),
        }
    }

    fn start() -> RecordStart {
        RecordStart {
            title: "Call with Lab B".into(),
            organizer: None,
            language: "en-us".into(),
            stamp: "20261106_120144".into(),
        }
    }

    fn link() -> CaptionCommandLink {
        CaptionCommandLink::parse(COMMAND).unwrap()
    }

    #[test]
    fn start_names_the_recording_its_transcript_and_the_language() {
        let context = CallContext::new("00000000-0000-4000-8000-0000000000c1");
        let write = CaptionWrite::record(&context, &link(), &actor(), CHAT, &start()).unwrap();
        assert_eq!(write.kind(), CaptionWriteKind::Record);
        let body = write.bare_body();
        assert_eq!(body["action"], "start");
        assert_eq!(body["processingModes"], json!(["recording", "realTimeTranscript"]));
        assert_eq!(body["participantLegId"], "00000000-0000-4000-8000-0000000000e2");
        let parameters = &body["actionParameters"];
        assert_eq!(parameters["type"], "start");
        assert_eq!(parameters["recordingMode"], "Normal");
        assert_eq!(parameters["spokenLanguage"], "en-us");
        assert_eq!(parameters["meetingTitle"], "Call with Lab B");
        assert!(parameters.get("meetingOrganizer").is_none());
        assert_eq!(
            parameters["recordingStorageSettings"][0]["FileName"],
            "Call with Lab B-20261106_120144-Meeting Recording"
        );
        assert_eq!(write.url().as_str(), COMMAND);
        assert!(!format!("{write:?}").contains("Lab B"));

        let organized = RecordStart {
            organizer: Some("Lab A".into()),
            ..start()
        };
        let write = CaptionWrite::record(&context, &link(), &actor(), CHAT, &organized).unwrap();
        assert_eq!(write.bare_body()["actionParameters"]["meetingOrganizer"], "Lab A");
    }

    #[test]
    fn stop_stops_both_and_names_nothing_else() {
        let context = CallContext::new("00000000-0000-4000-8000-0000000000c1");
        let write = CaptionWrite::stop_recording(&context, &link(), &actor(), CHAT).unwrap();
        assert_eq!(write.kind(), CaptionWriteKind::StopRecord);
        let body = write.bare_body();
        assert_eq!(body["action"], "stop");
        assert_eq!(body["processingModes"], json!(["recording", "realTimeTranscript"]));
        assert!(body.get("actionParameters").is_none());
    }

    #[test]
    fn start_refuses_what_the_web_client_never_sends() {
        let context = CallContext::new("00000000-0000-4000-8000-0000000000c1");
        for bad in [
            RecordStart {
                language: "xx-xx".into(),
                ..start()
            },
            RecordStart {
                title: " ".into(),
                ..start()
            },
            RecordStart {
                title: "Call\nwith".into(),
                ..start()
            },
            RecordStart {
                stamp: "2026-11-06 12:01".into(),
                ..start()
            },
            RecordStart {
                organizer: Some(String::new()),
                ..start()
            },
        ] {
            assert!(CaptionWrite::record(&context, &link(), &actor(), CHAT, &bad).is_err());
        }
        let mut legless = actor();
        legless.participant_id = None;
        assert!(CaptionWrite::record(&context, &link(), &legless, CHAT, &start()).is_err());
        assert!(CaptionWrite::stop_recording(&context, &link(), &legless, CHAT).is_err());
    }
}
