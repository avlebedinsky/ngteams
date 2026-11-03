use super::{Call, LabCallBindings, State, add_callbacks, bind_link, bind_links, callbacks, calling_url, text};
use crate::write::CallWire;
use serde_json::Value;
use std::collections::BTreeSet;
use teams_lab_targets::{TargetError, one_to_one_id};

pub(crate) struct CallPlan {
    pub(crate) key: Option<u64>,
    pub(crate) chat: String,
    pub(crate) cleanup: bool,
    pub(crate) added: BTreeSet<String>,
    pub(crate) peers: BTreeSet<String>,
    pub(super) entry: bool,
    pub(super) operation: String,
    pub(super) body: Value,
}

impl LabCallBindings {
    pub(crate) fn plan(&self, wire: &CallWire) -> Result<CallPlan, TargetError> {
        let body = wire.body().cloned().unwrap_or(Value::Null);
        self.actor(&body)?;
        let state = self.state();
        if body.get("conversationRequest").is_some() {
            return self.entry_plan(&state, wire, body);
        }
        let binding = state.links.get(wire.url().as_str()).ok_or(TargetError::Conversation)?;
        let call = state.calls.get(&binding.key).ok_or(TargetError::Conversation)?;
        let cleanup = binding.operation == "leave"
            || (matches!(binding.operation.as_str(), "reject" | "callLeg") && wire.method().as_str() == "DELETE");
        if !call.proven || (!cleanup && (call.unsafe_target || call.ended)) {
            return Err(TargetError::Recipients);
        }
        if !method_matches(&binding.operation, wire.method().as_str()) {
            return Err(TargetError::Conversation);
        }
        let mut plan = CallPlan {
            key: Some(binding.key),
            chat: call.chat.clone(),
            cleanup,
            added: self.added(&body)?,
            peers: call.members.clone(),
            entry: false,
            operation: binding.operation.clone(),
            body,
        };
        self.attach_input(&state, call, &mut plan)?;
        Ok(plan)
    }

    fn entry_plan(&self, state: &State, wire: &CallWire, body: Value) -> Result<CallPlan, TargetError> {
        if wire.method().as_str() != "POST"
            || text(&body, "/participants/from/id")? != self.own
            || body
                .pointer("/conversationRequest/scenario")
                .is_some_and(|value| !value.is_null())
        {
            return Err(TargetError::Identity);
        }
        let added = self.added(&body)?;
        let chat = if let Some(group) = body.pointer("/groupChat/threadId") {
            group.as_str().ok_or(TargetError::Conversation)?.to_owned()
        } else {
            let mut peers = added.iter().filter(|mri| **mri != self.own);
            let peer = peers.next().ok_or(TargetError::Recipients)?;
            if peers.next().is_some() {
                return Err(TargetError::Recipients);
            }
            one_to_one_id(&self.own, peer).ok_or(TargetError::Conversation)?
        };
        if let Some(organizer) = body.pointer("/meetingInfo/organizerId").and_then(Value::as_str) {
            let mri = if organizer.starts_with("8:orgid:") {
                organizer.to_owned()
            } else {
                format!("8:orgid:{organizer}")
            };
            if self.roster.account(&mri).is_none() {
                return Err(TargetError::Recipients);
            }
        }
        let key = if wire.url().as_str() == "https://api.flightproxy.teams.microsoft.com/api/v2/epconv" {
            None
        } else {
            let link = state
                .links
                .get(wire.url().as_str())
                .filter(|link| link.operation == "conversation")
                .ok_or(TargetError::Conversation)?;
            let call = state.calls.get(&link.key).ok_or(TargetError::Conversation)?;
            if call.chat != chat || !call.proven || call.unsafe_target {
                return Err(TargetError::Conversation);
            }
            (!call.ended).then_some(link.key)
        };
        if callbacks(state, &body)?.is_empty() {
            return Err(TargetError::Conversation);
        }
        Ok(CallPlan {
            key,
            chat,
            cleanup: false,
            peers: added.clone(),
            added,
            entry: true,
            operation: "entry".to_owned(),
            body,
        })
    }

    fn actor(&self, body: &Value) -> Result<(), TargetError> {
        for pointer in [
            "/participants/from/id",
            "/from/id",
            "/sender/id",
            "/callProgress/sender/id",
            "/callAcceptance/acceptedBy/id",
            "/mediaNegotiation/sender/id",
            "/mediaAnswer/sender/id",
        ] {
            if let Some(value) = body.pointer(pointer)
                && value.as_str() != Some(self.own.as_str())
            {
                return Err(TargetError::Identity);
            }
        }
        Ok(())
    }

    fn added(&self, body: &Value) -> Result<BTreeSet<String>, TargetError> {
        let Some(value) = body.pointer("/participants/to") else {
            return Ok(BTreeSet::new());
        };
        let entries = value.as_array().ok_or(TargetError::Recipients)?;
        let mut found = BTreeSet::new();
        for entry in entries {
            let id = text(entry, "/id")?;
            if self.roster.account(id).is_none() || !found.insert(id.to_owned()) {
                return Err(TargetError::Recipients);
            }
        }
        Ok(found)
    }

    fn attach_input(&self, state: &State, call: &Call, plan: &mut CallPlan) -> Result<(), TargetError> {
        if plan.operation != "attach" {
            return Ok(());
        }
        let actions = plan
            .body
            .get("additionalActions")
            .and_then(Value::as_array)
            .ok_or(TargetError::Conversation)?;
        if actions.len() != 1 {
            return Err(TargetError::Conversation);
        }
        let action = &actions[0];
        if text(action, "/name")? != "join" || Some(text(action, "/url")?) != call.controller.as_deref() {
            return Err(TargetError::Conversation);
        }
        let input = action.get("input").ok_or(TargetError::Conversation)?;
        self.actor(input)?;
        plan.added.extend(self.added(input)?);
        if text(input, "/participants/from/id")? != self.own {
            return Err(TargetError::Identity);
        }
        if input
            .pointer("/groupChat/threadId")
            .is_some_and(|chat| chat.as_str() != Some(call.chat.as_str()))
        {
            return Err(TargetError::Conversation);
        }
        callbacks(state, input)?;
        Ok(())
    }

    pub(crate) fn begin(&self, plan: &CallPlan, members: Option<Vec<String>>) -> Result<u64, TargetError> {
        let mut state = self.state();
        let key = if let Some(key) = plan.key {
            key
        } else {
            if state.calls.len() >= 32 {
                return Err(TargetError::Conversation);
            }
            state.next = state.next.checked_add(1).ok_or(TargetError::Conversation)?;
            let key = state.next;
            state.calls.insert(
                key,
                Call {
                    chat: plan.chat.clone(),
                    members: BTreeSet::new(),
                    callbacks: BTreeSet::new(),
                    proven: false,
                    unsafe_target: false,
                    ended: false,
                    controller: None,
                    inviter: None,
                    actor: None,
                    caption_version: None,
                },
            );
            key
        };
        let mut found = callbacks(&state, &plan.body)?;
        if plan.operation == "attach" {
            found.extend(callbacks(&state, &plan.body["additionalActions"][0]["input"])?);
        }
        add_callbacks(&mut state, key, found)?;
        let call = state.calls.get_mut(&key).ok_or(TargetError::Conversation)?;
        if let Some(actor) = actor_in(&plan.body) {
            if call.actor.as_ref().is_some_and(|known| known != &actor) {
                return Err(TargetError::Identity);
            }
            call.actor = Some(actor);
        }
        if !plan.cleanup && (call.unsafe_target || call.ended) {
            return Err(TargetError::Recipients);
        }
        if let Some(members) = members {
            self.roster.verify_recipients(&self.own, &members)?;
            if plan.entry && plan.added.iter().any(|mri| !members.contains(mri)) {
                return Err(TargetError::Recipients);
            }
            call.members.extend(members);
            call.members.extend(plan.added.iter().cloned());
            call.proven = true;
        }
        if !call.proven {
            return Err(TargetError::Recipients);
        }
        Ok(key)
    }

    pub(crate) fn answered(&self, key: u64, plan: &CallPlan, status: u16, body: &[u8]) {
        if !(200..300).contains(&status) {
            return;
        }
        let mut state = self.state();
        if plan.cleanup {
            if let Some(call) = state.calls.get_mut(&key) {
                call.ended = true;
            }
            return;
        }
        let Ok(answer) = serde_json::from_slice::<Value>(body) else {
            return;
        };
        if bind_answer(&mut state, key, &answer).is_err()
            && let Some(call) = state.calls.get_mut(&key)
        {
            call.unsafe_target = true;
        }
    }
}

fn actor_in(body: &Value) -> Option<(String, String)> {
    [
        "/participants/from",
        "/from",
        "/callProgress/sender",
        "/callAcceptance/acceptedBy",
        "/additionalActions/0/input/participants/from",
    ]
    .into_iter()
    .find_map(|pointer| {
        let actor = body.pointer(pointer)?;
        Some((
            actor.get("endpointId")?.as_str()?.to_owned(),
            actor.get("participantId")?.as_str()?.to_owned(),
        ))
    })
}

fn method_matches(operation: &str, method: &str) -> bool {
    match operation {
        "updateEndpointMetadata" => method == "PUT",
        "reject" => method == "DELETE",
        "callLeg" => matches!(method, "POST" | "DELETE"),
        _ => method == "POST",
    }
}

pub(super) fn bind_answer(state: &mut State, key: u64, answer: &Value) -> Result<(), TargetError> {
    if let Some(leave) = answer.pointer("/links/leave").and_then(Value::as_str) {
        bind_link(state, key, "leave", leave)?;
    }
    if let Some(controller) = answer.get("conversationController") {
        let controller = calling_url(controller.as_str().ok_or(TargetError::Conversation)?)?;
        let call = state.calls.get_mut(&key).ok_or(TargetError::Conversation)?;
        if call.controller.as_ref().is_some_and(|known| known != &controller) {
            return Err(TargetError::Conversation);
        }
        call.controller = Some(controller.clone());
        bind_link(state, key, "conversation", &controller)?;
    }
    for pointer in [
        "/links",
        "/callInvitation/links",
        "/callAcceptance/links",
        "/callAcceptanceAcknowledgement/links",
        "/mediaNegotiation/links",
        "/mediaAnswer/links",
    ] {
        if let Some(links) = answer.pointer(pointer) {
            bind_links(state, key, links)?;
        }
    }
    if let Some(actions) = answer.get("additionalActionResponses").and_then(Value::as_array) {
        for action in actions {
            if action.get("url").and_then(Value::as_str)
                != state.calls.get(&key).and_then(|call| call.controller.as_deref())
            {
                return Err(TargetError::Conversation);
            }
            if let Some(links) = action.pointer("/output/links") {
                bind_links(state, key, links)?;
            }
        }
    }
    Ok(())
}
