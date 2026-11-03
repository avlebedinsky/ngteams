use std::collections::BTreeSet;

use teams_lab_targets::TargetError;

use super::{CallPlan, LabCallBindings, State, bind_link, callbacks};
use crate::captions::{CaptionService, CaptionWrite, CaptionWriteKind};

impl LabCallBindings {
    pub(crate) fn plan_caption(&self, write: &CaptionWrite) -> Result<CallPlan, TargetError> {
        if write.from().mri.as_str() != self.own {
            return Err(TargetError::Identity);
        }
        let state = self.state();
        let binding = state.links.get(write.url().as_str()).ok_or(TargetError::Conversation)?;
        let call = state.calls.get(&binding.key).ok_or(TargetError::Conversation)?;
        let expected = match write.kind() {
            CaptionWriteKind::Invite => "addParticipant",
            CaptionWriteKind::Start | CaptionWriteKind::Language => "captionCommand",
        };
        let actor = write
            .from()
            .participant_id
            .as_ref()
            .map(|id| (write.from().endpoint_id.clone(), id.clone()));
        if binding.operation != expected
            || !call.proven
            || call.ended
            || call.unsafe_target
            || call.chat != write.thread()
        {
            return Err(TargetError::Conversation);
        }
        if actor.is_none() || actor != call.actor {
            return Err(TargetError::Identity);
        }
        callbacks(&state, write.bare_body())?;
        Ok(CallPlan {
            key: Some(binding.key),
            chat: call.chat.clone(),
            cleanup: false,
            added: BTreeSet::new(),
            peers: call.members.clone(),
            entry: false,
            operation: expected.to_owned(),
            body: write.bare_body().clone(),
        })
    }
}

pub(super) fn bind_service(state: &mut State, key: u64, body: &[u8]) -> Result<(), TargetError> {
    let value: serde_json::Value = serde_json::from_slice(body).map_err(|_| TargetError::Conversation)?;
    let Some(entry) = value
        .get("participants")
        .and_then(|participants| participants.get(crate::captions::CAPTION_SERVICE_MRI))
    else {
        return Ok(());
    };
    if let Some(version) = entry.get("version").and_then(serde_json::Value::as_u64) {
        let call = state.calls.get_mut(&key).ok_or(TargetError::Conversation)?;
        if call.caption_version.is_some_and(|old| version <= old) {
            return Ok(());
        }
        call.caption_version = Some(version);
    }
    let service = CaptionService::from_roster(body);
    let has_command = entry
        .get("endpoints")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|endpoints| {
            endpoints
                .values()
                .any(|endpoint| endpoint.pointer("/endpointMetadata/commandUrl").is_some())
        });
    let ended = entry
        .get("state")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|status| status != "active");
    if ended || has_command {
        state
            .links
            .retain(|_, link| link.key != key || link.operation != "captionCommand");
    }
    if !ended && let Some(service) = service {
        bind_link(state, key, "captionCommand", service.command.as_str())?;
    }
    Ok(())
}
