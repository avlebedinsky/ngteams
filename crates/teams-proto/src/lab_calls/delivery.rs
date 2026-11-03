use base64::Engine as _;
use serde_json::Value;
use teams_lab_targets::{TargetError, one_to_one_id};

use super::{
    Call, CallDeliveryData, CallObservation, LabCallBindings, PendingResponse, State, add_callbacks, bind_link,
    bind_links, callbacks, calling_url, text,
};
use std::collections::BTreeSet;

pub(crate) enum DeliveryPlan {
    Known(CallObservation),
    Incoming { key: u64, chat: String },
}

impl LabCallBindings {
    /// This part runs synchronously, before a recipient lookup is awaited: cancellations revoke pending receipts.
    pub(crate) fn record_delivery(&self, delivery: &CallDeliveryData) -> Result<DeliveryPlan, TargetError> {
        let body: Value = serde_json::from_slice(&delivery.body).map_err(|_| TargetError::Conversation)?;
        let mut state = self.state();
        let root = state.roots.get(&delivery.connection).ok_or(TargetError::Conversation)?;
        let root = url::Url::parse(root).map_err(|_| TargetError::Conversation)?;
        if delivery.path == root.path() {
            return self.invitation(&mut state, delivery, &body);
        }
        if !delivery
            .path
            .strip_prefix(root.path())
            .is_some_and(|tail| tail.starts_with("callAgent/"))
        {
            return Err(TargetError::Conversation);
        }
        let key = *state.callbacks.get(&delivery.path).ok_or(TargetError::Conversation)?;
        let call = state.calls.get(&key).ok_or(TargetError::Conversation)?;
        if !call.proven || call.ended {
            return Err(TargetError::Conversation);
        }
        if delivery.path.ends_with("/end/") || delivery.path.ends_with("/conversationEnd/") {
            if let Some(call) = state.calls.get_mut(&key) {
                call.ended = true;
            }
            state.responses.retain(|_, response| response.key != key);
            return Ok(DeliveryPlan::Known(CallObservation::Allowed));
        }
        let roster_update = delivery.path.ends_with("/rosterUpdate/");
        if !self.delivered_people(&body, roster_update) {
            if let Some(call) = state.calls.get_mut(&key) {
                call.unsafe_target = true;
            }
            return Ok(DeliveryPlan::Known(CallObservation::TargetChanged));
        }
        if call.unsafe_target {
            return Err(TargetError::Recipients);
        }
        let roster = roster_update
            .then(|| {
                body.get("participants")
                    .or_else(|| body.pointer("/roster/participants"))
            })
            .flatten();
        if let Some(roster) = roster.and_then(Value::as_object)
            && let Some(call) = state.calls.get_mut(&key)
        {
            call.members
                .extend(roster.keys().filter(|mri| self.roster.account(mri).is_some()).cloned());
        }
        if roster_update {
            super::captions::bind_service(&mut state, key, &delivery.body)?;
        }
        super::wire::bind_answer(&mut state, key, &body)?;
        if [
            "/acceptance/",
            "/mediaRenegotiation/",
            "/mediaAnswer/",
            "/mediaAcknowledgement/",
            "/rejection/",
        ]
        .iter()
        .any(|suffix| delivery.path.ends_with(suffix))
        {
            pending(&mut state, key, delivery, body.get("callAcceptance").is_some())?;
        }
        Ok(DeliveryPlan::Known(CallObservation::Allowed))
    }

    fn invitation(
        &self,
        state: &mut State,
        delivery: &CallDeliveryData,
        envelope: &Value,
    ) -> Result<DeliveryPlan, TargetError> {
        if !matches!(envelope.get("evt").and_then(Value::as_u64), Some(107 | 109)) {
            return Err(TargetError::Conversation);
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text(envelope, "/gp")?)
            .map_err(|_| TargetError::Conversation)?;
        let body: Value = serde_json::from_slice(&bytes).map_err(|_| TargetError::Conversation)?;
        let caller = text(&body, "/callNotification/from/id")?;
        if text(&body, "/callNotification/to/id")? != self.own
            || caller == self.own
            || self.roster.account(caller).is_none()
        {
            return Err(TargetError::Identity);
        }
        let chat = body
            .pointer("/groupChat/threadId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| one_to_one_id(&self.own, caller))
            .ok_or(TargetError::Conversation)?;
        let attach = calling_url(text(&body, "/callNotification/links/attach")?)?;
        let controller = calling_url(text(&body, "/conversationInvitation/conversationController")?)?;
        let key = if let Some(binding) = state.links.get(&attach) {
            let call = state.calls.get(&binding.key).ok_or(TargetError::Conversation)?;
            if call.chat != chat
                || call.controller.as_ref() != Some(&controller)
                || call.inviter.as_deref() != Some(caller)
                || call.ended
                || call.unsafe_target
            {
                return Err(TargetError::Conversation);
            }
            binding.key
        } else {
            if state.calls.len() >= 32 {
                return Err(TargetError::Conversation);
            }
            state.next = state.next.checked_add(1).ok_or(TargetError::Conversation)?;
            let key = state.next;
            state.calls.insert(
                key,
                Call {
                    chat: chat.clone(),
                    members: BTreeSet::from([self.own.clone(), caller.to_owned()]),
                    callbacks: BTreeSet::new(),
                    proven: false,
                    unsafe_target: false,
                    ended: false,
                    controller: Some(controller.clone()),
                    inviter: Some(caller.to_owned()),
                    actor: None,
                    caption_version: None,
                },
            );
            key
        };
        bind_link(state, key, "conversation", &controller)?;
        bind_links(
            state,
            key,
            body.pointer("/callNotification/links")
                .ok_or(TargetError::Conversation)?,
        )?;
        pending(state, key, delivery, false)?;
        Ok(DeliveryPlan::Incoming { key, chat })
    }

    fn delivered_people(&self, body: &Value, roster_update: bool) -> bool {
        for at in [
            "/callAcceptance/acceptedBy/id",
            "/callProgress/sender/id",
            "/mediaNegotiation/sender/id",
            "/mediaAnswer/sender/id",
        ] {
            if let Some(value) = body.pointer(at)
                && value.as_str().is_none_or(|mri| self.roster.account(mri).is_none())
            {
                return false;
            }
        }
        if !roster_update {
            return true;
        }
        for at in ["/participants", "/roster/participants"] {
            if let Some(value) = body.pointer(at) {
                let Some(participants) = value.as_object() else {
                    return false;
                };
                if participants.iter().any(|(mri, entry)| {
                    (self.roster.account(mri).is_none() && mri != crate::captions::CAPTION_SERVICE_MRI)
                        || !entry.is_object()
                }) {
                    return false;
                }
            }
        }
        true
    }

    pub(crate) fn confirm_incoming(&self, key: u64, connection: &str, members: &[String]) -> Result<(), TargetError> {
        self.roster.verify_recipients(&self.own, members)?;
        let mut state = self.state();
        if !state.roots.contains_key(connection) {
            return Err(TargetError::Conversation);
        }
        let call = state.calls.get_mut(&key).ok_or(TargetError::Conversation)?;
        if call.ended || call.unsafe_target || call.members.iter().any(|mri| !members.contains(mri)) {
            return Err(TargetError::Recipients);
        }
        call.members.extend(members.iter().cloned());
        call.proven = true;
        Ok(())
    }

    /// Correlates a typed response with a proved delivery on this exact socket, once.
    ///
    /// # Errors
    /// Rejects an unproved, ended, changed, disconnected or already answered call delivery.
    pub fn response(&self, connection: &str, id: i64, json: &str) -> Result<u64, TargetError> {
        let body: Value = serde_json::from_str(json).map_err(|_| TargetError::Conversation)?;
        if body.get("id").and_then(Value::as_i64) != Some(id) || body.get("status").and_then(Value::as_u64) != Some(200)
        {
            return Err(TargetError::Conversation);
        }
        let mut state = self.state();
        if !state.roots.contains_key(connection) {
            return Err(TargetError::Conversation);
        }
        let pending = state
            .responses
            .get(&(connection.to_owned(), id))
            .ok_or(TargetError::Conversation)?;
        let key = pending.key;
        let call = state.calls.get(&key).ok_or(TargetError::Conversation)?;
        if !call.proven || call.unsafe_target || call.ended {
            return Err(TargetError::Recipients);
        }
        for (name, expected) in &pending.headers {
            if let Some(expected) = expected
                && body
                    .get("headers")
                    .and_then(|headers| headers.get(*name))
                    .and_then(Value::as_str)
                    != Some(expected.as_str())
            {
                return Err(TargetError::Conversation);
            }
        }
        let content = text(&body, "/body")?;
        let found = if pending.acceptance {
            let ack: Value = serde_json::from_str(content).map_err(|_| TargetError::Conversation)?;
            if !ack.get("callAcceptanceAcknowledgement").is_some_and(Value::is_object) {
                return Err(TargetError::Conversation);
            }
            callbacks(&state, &ack)?
        } else {
            if !content.is_empty() {
                return Err(TargetError::Conversation);
            }
            BTreeSet::new()
        };
        add_callbacks(&mut state, key, found)?;
        state.responses.remove(&(connection.to_owned(), id));
        Ok(key)
    }
}

fn pending(state: &mut State, key: u64, delivery: &CallDeliveryData, acceptance: bool) -> Result<(), TargetError> {
    if !delivery
        .headers
        .iter()
        .any(|(name, value)| *name == "trouter-request" && value.is_some())
    {
        return Ok(());
    }
    if state.responses.len() >= 64 {
        return Err(TargetError::Conversation);
    }
    let headers = delivery
        .headers
        .iter()
        .filter(|(name, _)| acceptance || *name == "trouter-request")
        .cloned()
        .collect();
    state.responses.insert(
        (delivery.connection.clone(), delivery.id),
        PendingResponse {
            key,
            acceptance,
            headers,
        },
    );
    Ok(())
}
