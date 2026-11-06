//! Caption operations use their own outcome path: a service failure never fails the voice call.
use teams_proto::captions::{CAPTION_SERVICE_MRI, CaptionService, CaptionWrite, Processing, RecordCommand};

use super::{CallError, CallEvent, CallId, Calls};
use crate::agent::Kind;
use crate::call::Phase;

impl Calls {
    /// Invites the built-in caption service into the current connected call.
    ///
    /// # Errors
    /// The call must be connected and have its own service-issued conversation link and thread.
    pub fn caption_invite(&mut self, id: CallId) -> Result<CaptionWrite, CallError> {
        let call = self
            .call
            .as_mut()
            .filter(|call| call.id == id && call.phase == Phase::Connected)
            .ok_or(CallError::NotNow)?;
        let link = call.conversation.caption_add.clone().ok_or(CallError::NotNow)?;
        let thread = call.caption_thread.clone().ok_or(CallError::NotNow)?;
        let success = call.callback(Kind::Conversation, "addParticipantSuccess")?;
        let failure = call.callback(Kind::Conversation, "addParticipantFailure")?;
        let write = CaptionWrite::invite(&call.context, &link, &call.me, &thread, (&success, &failure))
            .map_err(|_| CallError::NotNow)?;
        call.caption_inviting = true;
        Ok(write)
    }

    /// Starts closed captions, or changes their shared spoken language after confirmation.
    ///
    /// # Errors
    /// The call must be connected and its caption service metadata must be known.
    pub fn caption_command(&self, id: CallId, language: Option<&str>) -> Result<CaptionWrite, CallError> {
        let call = self
            .call
            .as_ref()
            .filter(|call| call.id == id && call.phase == Phase::Connected)
            .ok_or(CallError::NotNow)?;
        let service = call.caption_service.as_ref().ok_or(CallError::NotNow)?;
        let thread = call.caption_thread.as_deref().ok_or(CallError::NotNow)?;
        match language {
            Some(language) => CaptionWrite::language(&call.context, &service.command, &call.me, thread, language),
            None => CaptionWrite::start(&call.context, &service.command, &call.me, thread),
        }
        .map_err(|_| CallError::NotNow)
    }

    /// Sends `command` to the recording bot's command link: the bot must be in the call already
    /// ([`Calls::caption_invite`] invites it). A recording starts alone while a transcription runs, and with its
    /// transcript otherwise; Stop recording stops the transcription too while it runs, as the web client does.
    ///
    /// # Errors
    /// The call must be connected and its recording bot's command link known; a start must be one the web client
    /// would send.
    pub fn record_command(&self, id: CallId, command: &RecordCommand) -> Result<CaptionWrite, CallError> {
        let call = self
            .call
            .as_ref()
            .filter(|call| call.id == id && call.phase == Phase::Connected)
            .ok_or(CallError::NotNow)?;
        let service = call.caption_service.as_ref().ok_or(CallError::NotNow)?;
        let thread = call.caption_thread.as_deref().ok_or(CallError::NotNow)?;
        let (link, me, context) = (&service.command, &call.me, &call.context);
        let recording = if call.meeting.state.transcribing.is_some() {
            Processing::Recording
        } else {
            Processing::Both
        };
        match command {
            RecordCommand::Record(start) => CaptionWrite::record(context, link, me, thread, (start, recording)),
            RecordCommand::Transcribe(start) => {
                CaptionWrite::record(context, link, me, thread, (start, Processing::Transcript))
            }
            RecordCommand::StopRecording => {
                let what = match recording {
                    Processing::Recording => Processing::Both,
                    _ => Processing::Recording,
                };
                CaptionWrite::stop_recording(context, link, me, thread, what)
            }
            RecordCommand::StopTranscription => {
                CaptionWrite::stop_recording(context, link, me, thread, Processing::Transcript)
            }
        }
        .map_err(|_| CallError::NotNow)
    }

    /// This call's built-in service, whether or not this viewer has captions visible.
    #[must_use]
    pub fn caption_service(&self, id: CallId) -> Option<&CaptionService> {
        self.call
            .as_ref()
            .filter(|call| call.id == id)?
            .caption_service
            .as_ref()
    }

    pub(super) fn caption_roster(&mut self, body: &[u8]) {
        let Some(call) = self.call.as_mut() else { return };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
            return;
        };
        if value
            .get("participants")
            .and_then(|p| p.get(CAPTION_SERVICE_MRI))
            .is_none()
        {
            return;
        }
        let entry = &value["participants"][CAPTION_SERVICE_MRI];
        if let Some(version) = entry.get("version").and_then(serde_json::Value::as_u64) {
            if call.caption_version.is_some_and(|old| version <= old) {
                return;
            }
            call.caption_version = Some(version);
        }

        let service = updated_service(call.caption_service.as_ref(), entry, body);
        if service == call.caption_service {
            return;
        }
        call.caption_service.clone_from(&service);
        call.caption_inviting = false;
        self.out.event(CallEvent::CaptionService { call: call.id, service });
    }
}

fn updated_service(
    previous: Option<&CaptionService>,
    entry: &serde_json::Value,
    body: &[u8],
) -> Option<CaptionService> {
    if entry
        .get("state")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|state| state != "active")
    {
        return None;
    }
    if let Some(service) = CaptionService::from_roster(body) {
        return Some(service);
    }
    let mut service = previous?.clone();
    let Some(metadata) = entry
        .get("endpoints")
        .and_then(|endpoints| endpoints.get(&service.endpoint_id))
        .and_then(|endpoint| endpoint.get("endpointMetadata"))
    else {
        return Some(service);
    };
    if let Some(language) = metadata.get("spokenLanguage").and_then(serde_json::Value::as_str) {
        service.spoken_language = language.into();
    }
    if let Some(state) = metadata
        .pointer("/processingModes/closedCaptions/state")
        .and_then(serde_json::Value::as_str)
    {
        service.state = state.into();
    }
    if let Some(url) = metadata.get("commandUrl") {
        service.command = url
            .as_str()
            .and_then(teams_proto::captions::CaptionCommandLink::parse)?;
    }
    Some(service)
}
