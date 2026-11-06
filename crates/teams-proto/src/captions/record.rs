//! Recording and transcription (More → Record and transcribe → Start recording, Start transcription, and their stops),
//! as the web client posts them to the recording bot's command link once the bot is in the call
//! (`docs/protocol/calling.md`, "More of a running call"). The bot is invited as for captions
//! ([`CaptionWrite::invite`]); its command link carries the account's recorder token only, as the web client sends it
//! (no skypetoken). A command names what it starts or stops ([`Processing`]): the recording and its transcript
//! together, or either alone.

use serde_json::{Value, json};

use super::{
    CaptionCommandLink, CaptionWrite, CaptionWriteError, CaptionWriteKind, SPOKEN_LANGUAGES, fresh_guid, timestamp,
};
use crate::{CallContext, CallParticipant};

/// The longest call title a recording's start names.
const MAX_TITLE: usize = 256;

/// What a start or a stop of the recording bot covers, as the web client names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Processing {
    /// The recording and its transcript together: Start recording while nothing runs, Stop recording while both run.
    Both,
    /// The recording alone: Start recording while the transcription runs already.
    Recording,
    /// The transcription alone: Start transcription, Stop transcription.
    Transcript,
}

impl Processing {
    /// Its `processingModes`.
    fn modes(self) -> &'static [&'static str] {
        match self {
            Self::Both => &["recording", "realTimeTranscript"],
            Self::Recording => &["recording"],
            Self::Transcript => &["realTimeTranscript"],
        }
    }

    /// Its `mode`, which a command for one of them names (one for both names none).
    fn mode(self) -> Option<&'static str> {
        match self {
            Self::Both => None,
            Self::Recording => Some("recording"),
            Self::Transcript => Some("transcription"),
        }
    }

    /// The command's body as a start and a stop share it: who asks, `action`, and what it covers.
    fn body(self, from: &CallParticipant, action: &str) -> Value {
        let mut body = json!({"timestamp":timestamp(),"participantMri":from.mri.as_str(),
            "participantLegId":from.participant_id,"action":action});
        if let Some(mode) = self.mode() {
            body["mode"] = json!(mode);
        }
        body["processingModes"] = json!(self.modes());
        body
    }
}

/// What the account asks of the recording bot (More → Record and transcribe). Whether a start or a stop of the
/// recording covers the transcription too follows from what the call's roster says runs
/// (`teams_call::Calls::record_command`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordCommand {
    /// Start recording: with its transcript while no transcription runs, alone while one does.
    Record(RecordStart),
    /// Start the transcription alone.
    Transcribe(RecordStart),
    /// Stop recording: with the transcription while it runs.
    StopRecording,
    /// Stop the transcription alone.
    StopTranscription,
}

impl RecordCommand {
    /// It starts something (else it stops something).
    #[must_use]
    pub fn starts(&self) -> bool {
        matches!(self, Self::Record(_) | Self::Transcribe(_))
    }

    /// It is about the transcription alone.
    #[must_use]
    pub fn transcription(&self) -> bool {
        matches!(self, Self::Transcribe(_) | Self::StopTranscription)
    }
}

/// What a recording's or a transcription's start names besides the call: its title, its organizer (a meeting's), the
/// spoken language picked, when it starts, and whether a recording records the audio alone.
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
    /// The recording records the audio alone ("Choose what to record" → Audio only); a transcription ignores it.
    pub audio_only: bool,
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

/// The recorder's features, as every captured start (and the bot's invitation) named them.
fn features() -> Value {
    json!({"enablePPTSharing":true,"intermediateLiveCaptions":false,"actionItemsEnabled":false,
        "enableEmailAndMeetingLanguageModel":true,"ceoSummit":false,"useUnmixedAudio":true,
        "enableTranscriptMeetingChaptering":false})
}

impl CaptionWrite {
    /// Starts recording, transcription or both (`what`) on the recording bot's command link, as More → Record and
    /// transcribe → Start recording's or Start transcription's Confirm sends it: saved to the account's OneDrive
    /// (`Recordings`, named `<title>-<stamp>-Meeting Recording`, or `…-Meeting Transcript` for a transcription alone).
    /// A recording names what it records (`recordingMode`); a transcript names its spoken language.
    ///
    /// # Errors
    /// Rejects an actor without its participant leg, an invalid conversation, an unsupported language, a title with
    /// control characters (or none) and a stamp not `YYYYMMDD_HHMMSS`.
    pub fn record(
        context: &CallContext,
        link: &CaptionCommandLink,
        from: &CallParticipant,
        thread: &str,
        (start, what): (&RecordStart, Processing),
    ) -> Result<Self, CaptionWriteError> {
        if !start.valid() {
            return Err(CaptionWriteError);
        }
        let mut write = Self::start(context, link, from, thread)?;
        let file = if what == Processing::Transcript {
            "Meeting Transcript"
        } else {
            "Meeting Recording"
        };
        let mut parameters = json!({
            "recordingFeatures":features(),
            "recordingStorageSettings":[{"StorageType":"OnedriveForBusiness","StorageLocation":"Recordings",
                "FileName":format!("{}-{}-{file}", start.title, start.stamp)}],
            "correlationId":fresh_guid()?,"meetingTitle":start.title,"exchangeId":null,"type":"start"
        });
        if what != Processing::Transcript {
            parameters["recordingMode"] = json!(if start.audio_only { "AudioOnly" } else { "Normal" });
        }
        if what != Processing::Recording {
            parameters["spokenLanguage"] = json!(start.language);
        }
        if let Some(organizer) = &start.organizer {
            parameters["meetingOrganizer"] = json!(organizer);
        }
        write.kind = CaptionWriteKind::Record;
        write.body = what.body(from, "start");
        write.body["actionParameters"] = parameters;
        Ok(write)
    }

    /// Stops recording, transcription or both (`what`), as Stop recording's "Stop recording and transcription?" or
    /// Stop transcription's "Stop transcription?" → Stop sends it. Anyone in the call may stop them, not only who
    /// started them.
    ///
    /// # Errors
    /// Rejects an actor without its participant leg or an invalid conversation.
    pub fn stop_recording(
        context: &CallContext,
        link: &CaptionCommandLink,
        from: &CallParticipant,
        thread: &str,
        what: Processing,
    ) -> Result<Self, CaptionWriteError> {
        let mut write = Self::start(context, link, from, thread)?;
        write.kind = CaptionWriteKind::StopRecord;
        write.body = what.body(from, "stop");
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
            audio_only: false,
        }
    }

    fn link() -> CaptionCommandLink {
        CaptionCommandLink::parse(COMMAND).unwrap()
    }

    fn context() -> CallContext {
        CallContext::new("00000000-0000-4000-8000-0000000000c1")
    }

    #[test]
    fn start_names_the_recording_its_transcript_and_the_language() {
        let write = CaptionWrite::record(&context(), &link(), &actor(), CHAT, (&start(), Processing::Both)).unwrap();
        assert_eq!(write.kind(), CaptionWriteKind::Record);
        let body = write.bare_body();
        assert_eq!(body["action"], "start");
        assert!(body.get("mode").is_none(), "a start of both names no mode");
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
    }

    #[test]
    fn a_meeting_s_start_names_its_organizer_and_the_audio_alone() {
        let organized = RecordStart {
            organizer: Some("Lab A".into()),
            audio_only: true,
            ..start()
        };
        let write = CaptionWrite::record(&context(), &link(), &actor(), CHAT, (&organized, Processing::Both)).unwrap();
        let parameters = &write.bare_body()["actionParameters"];
        assert_eq!(parameters["meetingOrganizer"], "Lab A");
        assert_eq!(parameters["recordingMode"], "AudioOnly");
    }

    #[test]
    fn a_transcription_alone_names_its_language_and_its_transcript_file() {
        let write =
            CaptionWrite::record(&context(), &link(), &actor(), CHAT, (&start(), Processing::Transcript)).unwrap();
        let body = write.bare_body();
        assert_eq!(body["mode"], "transcription");
        assert_eq!(body["processingModes"], json!(["realTimeTranscript"]));
        let parameters = &body["actionParameters"];
        assert!(
            parameters.get("recordingMode").is_none(),
            "a transcript records nothing"
        );
        assert_eq!(parameters["spokenLanguage"], "en-us");
        assert_eq!(
            parameters["recordingStorageSettings"][0]["FileName"],
            "Call with Lab B-20261106_120144-Meeting Transcript"
        );
    }

    #[test]
    fn a_recording_beside_a_running_transcription_names_what_it_records_and_no_language() {
        let audio = RecordStart {
            audio_only: true,
            ..start()
        };
        let write = CaptionWrite::record(&context(), &link(), &actor(), CHAT, (&audio, Processing::Recording)).unwrap();
        let body = write.bare_body();
        assert_eq!(body["mode"], "recording");
        assert_eq!(body["processingModes"], json!(["recording"]));
        let parameters = &body["actionParameters"];
        assert_eq!(parameters["recordingMode"], "AudioOnly");
        assert!(parameters.get("spokenLanguage").is_none());
        assert_eq!(
            parameters["recordingStorageSettings"][0]["FileName"],
            "Call with Lab B-20261106_120144-Meeting Recording"
        );
    }

    #[test]
    fn stop_stops_what_it_names_and_names_nothing_else() {
        for (what, mode, modes) in [
            (Processing::Both, None, json!(["recording", "realTimeTranscript"])),
            (Processing::Recording, Some("recording"), json!(["recording"])),
            (
                Processing::Transcript,
                Some("transcription"),
                json!(["realTimeTranscript"]),
            ),
        ] {
            let write = CaptionWrite::stop_recording(&context(), &link(), &actor(), CHAT, what).unwrap();
            assert_eq!(write.kind(), CaptionWriteKind::StopRecord);
            let body = write.bare_body();
            assert_eq!(body["action"], "stop");
            assert_eq!(body.get("mode").and_then(Value::as_str), mode);
            assert_eq!(body["processingModes"], modes);
            assert!(body.get("actionParameters").is_none());
        }
    }

    #[test]
    fn a_command_says_what_it_starts_or_stops() {
        assert!(RecordCommand::Record(start()).starts());
        assert!(RecordCommand::Transcribe(start()).starts() && RecordCommand::Transcribe(start()).transcription());
        assert!(!RecordCommand::StopRecording.starts() && !RecordCommand::StopRecording.transcription());
        assert!(RecordCommand::StopTranscription.transcription());
    }

    #[test]
    fn start_refuses_what_the_web_client_never_sends() {
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
            assert!(CaptionWrite::record(&context(), &link(), &actor(), CHAT, (&bad, Processing::Both)).is_err());
        }
        let mut legless = actor();
        legless.participant_id = None;
        assert!(CaptionWrite::record(&context(), &link(), &legless, CHAT, (&start(), Processing::Both)).is_err());
        assert!(CaptionWrite::stop_recording(&context(), &link(), &legless, CHAT, Processing::Both).is_err());
    }
}
