//! Microsoft's built-in caption service, as captured from the Teams call controls: Microsoft's recording bot, which
//! both runs live captions and records and transcribes a call ([`CaptionWrite::record`]).
//! Credentials are attached only by the transport, never stored in a request or its debug representation.
use std::fmt;

mod record;
mod roster;
pub use record::{Processing, RecordCommand, RecordStart};
pub use roster::{CaptionRoster, FollowedService};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use url::Url;

use crate::{CallContext, CallLink, CallParticipant};

/// The commercial Teams built-in combined recording/transcription identity: the one a caption start invites. Caption
/// start selects only closed captions.
pub const CAPTION_SERVICE_MRI: &str = "28:bdd75849-e0a6-4cce-8fc1-d7c0d4da43e5";
/// The same service when it serves the call from another region: invited as [`CAPTION_SERVICE_MRI`], it can join the
/// roster as this one instead. Teams' web client follows both (its commercial `recordingBotMrisFromAlternateRegions`).
pub const CAPTION_SERVICE_ALTERNATE_MRI: &str = "28:4b25d9f8-18f5-4d09-a582-cd0a28f63181";
/// Every MRI the built-in service joins a call as, the invited one first.
pub const CAPTION_SERVICE_MRIS: [&str; 2] = [CAPTION_SERVICE_MRI, CAPTION_SERVICE_ALTERNATE_MRI];
/// The first-party recorder resource, with delegated `access_recorder_service` permission.
pub const RECORDER_AUDIENCE: &str = "4580fd1d-e5a3-4f56-9ad1-aab0e3bf8f76";

/// A command URL handed out by the verified caption service in the call's roster.
#[derive(Clone, PartialEq, Eq)]
pub struct CaptionCommandLink(CallLink);

impl CaptionCommandLink {
    /// Accepts only the captured Microsoft Teams recorder endpoint through the authenticated flight proxy.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let link = CallLink::parse(value)?;
        let url = Url::parse(value).ok()?;
        let parts: Vec<_> = url.path_segments()?.collect();
        if url.query().is_some() || parts.len() != 7 || parts[4..6] != ["v2", "oncommand"] || !guid(parts[6]) {
            return None;
        }
        let backend = Url::parse(&format!("https://{}/", parts[3])).ok()?;
        let host = backend.host_str()?;
        let namespace = host.strip_suffix(".callrecorder.teams.cloud.microsoft")?;
        if namespace.is_empty()
            || !backend.username().is_empty()
            || backend.password().is_some()
            || backend.port().is_none_or(|port| port == 0)
            || backend.path() != "/"
            || backend.query().is_some()
            || backend.fragment().is_some()
        {
            return None;
        }
        Some(Self(link))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    #[must_use]
    pub fn url(&self) -> Url {
        self.0.url().clone()
    }
}

impl fmt::Debug for CaptionCommandLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("CaptionCommandLink").field(&self.0).finish()
    }
}

/// Whether `mri` is the built-in service, joined from the call's region or from another one.
#[must_use]
pub fn is_caption_service(mri: &str) -> bool {
    CAPTION_SERVICE_MRIS
        .iter()
        .any(|service| mri.eq_ignore_ascii_case(service))
}

/// The service endpoint's current state, read only from this call's bot roster entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptionService {
    /// The MRI the service joined as ([`CAPTION_SERVICE_MRIS`]).
    pub mri: &'static str,
    pub command: CaptionCommandLink,
    pub endpoint_id: String,
    /// When the endpoint joined (`joinTime`, as the service wrote it), if the roster said.
    pub joined: Option<String>,
    pub spoken_language: String,
    /// The service's own processing state; unknown states are preserved rather than treated as active.
    pub state: String,
}

/// A fixed caption operation. Its body contains no token until the native HTTP transport sends it.
#[derive(Clone)]
pub struct CaptionWrite {
    kind: CaptionWriteKind,
    url: Url,
    from: CallParticipant,
    thread: String,
    headers: Vec<(String, String)>,
    body: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptionWriteKind {
    Invite,
    Start,
    Language,
    /// Start recording, transcription or both ([`CaptionWrite::record`]).
    Record,
    /// Stop recording, transcription or both ([`CaptionWrite::stop_recording`]).
    StopRecord,
}

/// Invalid caption protocol parts. The error never contains a URL, identity, token or caption text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the caption request does not match the verified Teams service protocol")]
pub struct CaptionWriteError;

impl CaptionWrite {
    /// Invites only Microsoft's built-in service, using this conversation's service-issued `addParticipant` link.
    ///
    /// # Errors
    /// Rejects an unknown actor, conversation, callback, operation link or missing participant leg.
    pub fn invite(
        context: &CallContext,
        link: &CallLink,
        from: &CallParticipant,
        thread: &str,
        callbacks: (&str, &str),
    ) -> Result<Self, CaptionWriteError> {
        actor(from)?;
        if !conversation(thread)
            || Url::parse(link.as_str()).ok().and_then(|url| {
                url.path_segments()
                    .map(|mut parts| parts.next_back() == Some("addParticipant"))
            }) != Some(true)
            || !caption_callback(callbacks.0, "addParticipantSuccess")
            || !caption_callback(callbacks.1, "addParticipantFailure")
        {
            return Err(CaptionWriteError);
        }
        let service_participant = fresh_guid()?;
        let body = json!({
            "disableUnmute":false,
            "participants":{"from":participant(from),"to":[{"id":CAPTION_SERVICE_MRI,"participantId":service_participant}]},
            "participantInvitationData":{"botData":{
                "meetingTitle":"","clientInfo":"Teams-R4","callId":context.chain_id(),"threadId":thread,
                "recorderFeatures":{"enablePPTSharing":true,"intermediateLiveCaptions":false,"actionItemsEnabled":false,
                    "enableEmailAndMeetingLanguageModel":true,"ceoSummit":false,"useUnmixedAudio":true,"enableTranscriptMeetingChaptering":false},
                "mode":"RecordingAndTranscription","iCalUid":null,"consumerType":"Teams","spokenLanguage":""
            }},
            "replacementDetails":null,"links":{"addParticipantSuccess":callbacks.0,"addParticipantFailure":callbacks.1},"debugContent":{}
        });
        Ok(Self {
            kind: CaptionWriteKind::Invite,
            url: Url::parse(link.as_str()).map_err(|_| CaptionWriteError)?,
            from: from.clone(),
            thread: thread.to_owned(),
            headers: context_headers(context)?,
            body,
        })
    }

    /// Starts ephemeral closed captions on this service endpoint. This does not start recording or a saved transcript.
    ///
    /// # Errors
    /// Rejects an actor without its participant leg or an invalid conversation.
    pub fn start(
        context: &CallContext,
        link: &CaptionCommandLink,
        from: &CallParticipant,
        thread: &str,
    ) -> Result<Self, CaptionWriteError> {
        actor(from)?;
        if !conversation(thread) {
            return Err(CaptionWriteError);
        }
        let body = json!({"timestamp":timestamp(),"participantMri":from.mri.as_str(),"participantLegId":from.participant_id,
            "action":"start","mode":"transcription","processingModes":["closedCaptions"]});
        let headers = vec![
            ("x-microsoft-skype-chain-id".into(), context.chain_id().to_owned()),
            ("requestid".into(), fresh_guid()?),
        ];
        Ok(Self {
            kind: CaptionWriteKind::Start,
            url: link.url(),
            from: from.clone(),
            thread: thread.to_owned(),
            headers,
            body,
        })
    }

    /// Changes the meeting's shared spoken language after the user confirms that everyone is speaking it.
    ///
    /// # Errors
    /// Rejects an unsupported language or invalid call context.
    pub fn language(
        context: &CallContext,
        link: &CaptionCommandLink,
        from: &CallParticipant,
        thread: &str,
        language: &str,
    ) -> Result<Self, CaptionWriteError> {
        if !SPOKEN_LANGUAGES.iter().any(|(code, _)| *code == language) {
            return Err(CaptionWriteError);
        }
        let mut write = Self::start(context, link, from, thread)?;
        write.kind = CaptionWriteKind::Language;
        write.body["action"] = json!("setLanguage");
        write.body["actionParameters"] = json!({"spokenLanguage":language,"type":"setLanguage"});
        Ok(write)
    }

    #[must_use]
    pub fn kind(&self) -> CaptionWriteKind {
        self.kind
    }
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }
    #[must_use]
    pub fn from(&self) -> &CallParticipant {
        &self.from
    }
    #[must_use]
    pub fn thread(&self) -> &str {
        &self.thread
    }
    pub(crate) fn headers(&self) -> &[(String, String)] {
        &self.headers
    }
    #[must_use]
    pub fn bare_body(&self) -> &Value {
        &self.body
    }

    pub(crate) fn body(&self, recorder: &Secret, _skype: &Secret) -> Vec<u8> {
        let mut body = self.body.clone();
        match self.kind {
            CaptionWriteKind::Invite => {
                body["participantInvitationData"]["botData"]["initiatorUserToken"] = json!(recorder.expose());
            }
            // Teams sends the Skype credential in the header; the captured body field is empty.
            CaptionWriteKind::Start
            | CaptionWriteKind::Language
            | CaptionWriteKind::Record
            | CaptionWriteKind::StopRecord => body["participantSkypeToken"] = json!(""),
        }
        body.to_string().into_bytes()
    }
}

impl fmt::Debug for CaptionWrite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CaptionWrite")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

fn actor(from: &CallParticipant) -> Result<(), CaptionWriteError> {
    if !from.mri.as_str().strip_prefix("8:orgid:").is_some_and(guid)
        || !guid(&from.endpoint_id)
        || !from.participant_id.as_deref().is_some_and(guid)
    {
        return Err(CaptionWriteError);
    }
    Ok(())
}

fn guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

fn conversation(thread: &str) -> bool {
    thread.starts_with("19:")
        && !thread.chars().any(char::is_whitespace)
        && ["@unq.gbl.spaces", "@thread.v2", "@thread.tacv2"]
            .iter()
            .any(|end| thread.ends_with(end))
}

fn participant(from: &CallParticipant) -> Value {
    let mut value = json!({"id":from.mri.as_str(),"endpointId":from.endpoint_id,"participantId":from.participant_id});
    if let Some(name) = &from.display_name {
        value["displayName"] = json!(name);
    }
    if let Some(language) = &from.language_id {
        value["languageId"] = json!(language);
    }
    value
}

fn caption_callback(value: &str, name: &str) -> bool {
    let Ok(url) = Url::parse(value) else { return false };
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| host.ends_with(".trouter.teams.microsoft.com"))
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path().starts_with("/v4/f/")
        && url.path().contains("/callAgent/")
        && url.path().ends_with(&format!("/conversation/{name}/"))
}

fn context_headers(context: &CallContext) -> Result<Vec<(String, String)>, CaptionWriteError> {
    // These are ordinary call headers; the credential itself is attached only at the send.
    Ok(vec![
        ("x-microsoft-skype-chain-id".into(), context.chain_id().to_owned()),
        ("x-microsoft-skype-message-id".into(), fresh_guid()?),
    ])
}

fn fresh_guid() -> Result<String, CaptionWriteError> {
    CallContext::fresh()
        .map(|context| context.chain_id().to_owned())
        .map_err(|_| CaptionWriteError)
}

fn timestamp() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.millisecond()
    )
}

/// Spoken languages offered by the Teams caption settings (commercial service).
pub const SPOKEN_LANGUAGES: &[(&str, &str)] = &[
    ("sq-al", "Albanian (Albania)"),
    ("ar-sa", "Arabic (Saudi Arabia)"),
    ("ar-ae", "Arabic (United Arab Emirates)"),
    ("bg-bg", "Bulgarian (Bulgaria)"),
    ("ca-es", "Catalan (Catalan)"),
    ("zh-cn", "Chinese (Simplified, China)"),
    ("zh-hk", "Chinese (Traditional, Hong Kong SAR)"),
    ("zh-tw", "Chinese (Traditional, Taiwan)"),
    ("hr-hr", "Croatian (Croatia)"),
    ("cs-cz", "Czech (Czechia)"),
    ("da-dk", "Danish (Denmark)"),
    ("nl-be", "Dutch (Belgium)"),
    ("nl-nl", "Dutch (Netherlands)"),
    ("en-au", "English (Australia)"),
    ("en-ca", "English (Canada)"),
    ("en-in", "English (India)"),
    ("en-nz", "English (New Zealand)"),
    ("en-gb", "English (UK)"),
    ("en-us", "English (US)"),
    ("et-ee", "Estonian (Estonia)"),
    ("fil-ph", "Filipino (Philippines)"),
    ("fi-fi", "Finnish (Finland)"),
    ("fr-ca", "French (Canada)"),
    ("fr-fr", "French (France)"),
    ("de-de", "German (Germany)"),
    ("de-ch", "German (Switzerland)"),
    ("el-gr", "Greek (Greece)"),
    ("he-il", "Hebrew (Israel)"),
    ("hi-in", "Hindi (India)"),
    ("hu-hu", "Hungarian (Hungary)"),
    ("id-id", "Indonesian (Indonesia)"),
    ("it-it", "Italian (Italy)"),
    ("ja-jp", "Japanese (Japan)"),
    ("kk-kz", "Kazakh (Kazakhstan)"),
    ("ko-kr", "Korean (Korea)"),
    ("lv-lv", "Latvian (Latvia)"),
    ("lt-lt", "Lithuanian (Lithuania)"),
    ("ms-my", "Malay (Malaysia)"),
    ("mt-mt", "Maltese (Malta)"),
    ("nb-no", "Norwegian (Norway)"),
    ("pl-pl", "Polish (Poland)"),
    ("pt-br", "Portuguese (Brazil)"),
    ("pt-pt", "Portuguese (Portugal)"),
    ("ro-ro", "Romanian (Romania)"),
    ("ru-ru", "Russian (Russia)"),
    ("sr-rs", "Serbian (Cyrillic, Serbia)"),
    ("sk-sk", "Slovak (Slovakia)"),
    ("sl-si", "Slovenian (Slovenia)"),
    ("es-mx", "Spanish (Mexico)"),
    ("es-es", "Spanish (Spain)"),
    ("sv-se", "Swedish (Sweden)"),
    ("th-th", "Thai (Thailand)"),
    ("tr-tr", "Turkish (Turkey)"),
    ("uk-ua", "Ukrainian (Ukraine)"),
    ("vi-vn", "Vietnamese (Vietnam)"),
    ("cy-gb", "Welsh (United Kingdom)"),
];

#[cfg(test)]
mod tests {
    use super::*;
    const COMMAND: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/aks-prod-zz-api.callrecorder.teams.cloud.microsoft:21606/v2/oncommand/00000000-0000-4000-8000-000000000001";

    #[test]
    fn command_only_targets_allocated_first_party_recorder() {
        assert!(CaptionCommandLink::parse(COMMAND).is_some());
        for invalid in [
            COMMAND.replace(".callrecorder.teams.cloud.microsoft", ".outside.example.test"),
            COMMAND.replace(":21606", ""),
            COMMAND.replace("oncommand", "arbitrary"),
            format!("{COMMAND}?target=outside"),
            COMMAND.replace("api.flightproxy.teams.microsoft.com", "outside.example.test"),
            COMMAND.replace("https://", "http://"),
        ] {
            assert!(CaptionCommandLink::parse(&invalid).is_none());
        }
    }
}
