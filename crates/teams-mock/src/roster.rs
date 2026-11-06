//! A call's roster (M3 Task 19): who is in the call, delivered at the `rosterUpdate` callbacks whenever that
//! changed, in the shape the captures show (`docs/protocol/calling.md`, "Group video"): `{"participants": {<MRI>:
//! {"version", "state", "details": {"id", "displayName"}, "endpoints": {<id>: {"call": {"mediaStreams": [main audio,
//! main video]}}}}}, "type": "Delta", "sequenceNumber"}`. A delivery lists only the party that changed, as the
//! service's do, so a client knows the others only from what it was told before.
//!
//! A 1:1 call's roster lists its endpoints' `call` with no media streams, as captured, and tells nobody anything before
//! the callee answered; then both parties and every watcher are told both, and only the changes after. The test call's
//! tells each change at once, as captured: its caller from its start, the echo once its `add` went (no display name).
//!
//! - **Parties**: the caller from its start, a callee once it answered, a joiner once it joined with media; each with
//!   the callback its start, `attach` or join handed out.
//! - **Watchers**: a callee's `attach` subscribes to the roster: its callback is told everyone at once, then every
//!   change while the call runs, whether its callee answered or let the ring end (a join later gets only the changes
//!   from then on at its own callback, as the captures show).
//! - **Leaving** (a group call's `leave`): the party turns `inactive` (its endpoints empty) and the others are told;
//!   the call ends only when nobody is left.
//! - **Sources**: each party's audio and main video source ids, its mixer's ([`crate::mixer::MixerPeer`]) when the
//!   group video is on, none otherwise; its main video `sendrecv` once its media descriptions said so (its camera on),
//!   `recvonly` before.
//!
//! **The mock's own, not captured**: the endpoint ids, and that an `attach` is told everyone in one delivery.

use serde_json::{Map, Value, json};

use crate::calling::CallPhase;
use crate::world::State;

/// A party of a call's roster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Party {
    pub(crate) mri: String,
    /// Its `rosterUpdate` callback.
    pub(crate) link: Option<String>,
    /// Its `receiveMessage` callback: where the call's live reactions reach it.
    pub(crate) messages: Option<String>,
    pub(crate) active: bool,
    pub(crate) video_on: bool,
    /// Its endpoint's state as it last reported it (`updateEndpointState`).
    pub(crate) muted: bool,
    /// The id of its raised hand while it is up (`publishState`, taken back by `removeState`).
    pub(crate) hand: Option<String>,
}

/// The `rosterUpdate` callback a start's, a join's or an `attach`'s body hands out (wherever in it).
pub(crate) fn link_of(body: &Value) -> Option<String> {
    named(body, "rosterUpdate")
}

/// The callback named `name` a body hands out (wherever in it).
pub(crate) fn named(body: &Value, name: &str) -> Option<String> {
    match body {
        Value::Object(fields) => fields.iter().find_map(|(key, value)| match value {
            Value::String(link) if key == name => Some(link.clone()),
            _ => named(value, name),
        }),
        Value::Array(values) => values.iter().find_map(|value| named(value, name)),
        _ => None,
    }
}

/// `mri`'s live reactions in call `index` reach it at `link` from now on.
pub(crate) fn listen(state: &mut State, index: usize, mri: &str, link: Option<&str>) {
    let Some(call) = state.calling.calls.get_mut(index) else {
        return;
    };
    for party in call.roster.iter_mut().filter(|party| party.mri == mri) {
        party.messages = link.map(str::to_owned);
    }
}

/// `mri`'s endpoint in call `index` reports itself muted (`muted`) or not: everyone is told when that changed.
pub(crate) fn muted(state: &mut State, index: usize, mri: &str, muted: bool) {
    let Some(call) = state.calling.calls.get_mut(index) else {
        return;
    };
    let mut changed = false;
    for party in call
        .roster
        .iter_mut()
        .filter(|party| party.mri == mri && party.muted != muted)
    {
        party.muted = muted;
        changed = true;
    }
    if changed {
        send(state, index, Some(mri));
    }
}

/// `mri` raises its hand in call `index` (`Some` its state's id) or lowers it: everyone is told.
pub(crate) fn hand(state: &mut State, index: usize, mri: &str, hand: Option<&str>) {
    let Some(call) = state.calling.calls.get_mut(index) else {
        return;
    };
    for party in call.roster.iter_mut().filter(|party| party.mri == mri) {
        party.hand = hand.map(str::to_owned);
    }
    send(state, index, Some(mri));
}

/// The live reaction `body` (a `sendMessage`'s) from `mri` in call `index`, delivered to everyone in it at their
/// `receiveMessage` callbacks (its sender too, as the service's are) in the captured shape.
pub(crate) fn react(state: &mut State, index: usize, body: &Value) {
    let Some(call) = state.calling.calls.get(index) else {
        return;
    };
    let Some(content) = body.pointer("/messageContent/0") else {
        return;
    };
    let message = json!({
        "from": body.get("from").cloned().unwrap_or(Value::Null),
        "type": content.get("type").cloned().unwrap_or(Value::Null),
        "payload": content.get("payload").cloned().unwrap_or(Value::Null),
        "operationId": content.get("operationId").cloned().unwrap_or(Value::Null),
    });
    let links: Vec<String> = call
        .roster
        .iter()
        .filter(|party| party.active)
        .filter_map(|party| party.messages.clone())
        .collect();
    for link in links {
        state.call_back(Some(&link), message.clone());
    }
}

/// `mri` is in call `index` (again), told at `link`: in a group call and the test call everyone is told the change; in
/// a 1:1 call nobody is told before the callee answered, then everyone everything.
pub(crate) fn enter(state: &mut State, index: usize, mri: &str, link: Option<String>) {
    let Some(call) = state.calling.calls.get_mut(index) else {
        return;
    };
    call.roster.retain(|party| party.mri != mri);
    call.roster.push(Party {
        mri: mri.to_owned(),
        link,
        messages: None,
        active: true,
        video_on: false,
        muted: false,
        hand: None,
    });
    if call.group_chat.is_some() || call.test_call {
        send(state, index, Some(mri));
    } else if call.phase == CallPhase::Connected {
        send(state, index, None);
    }
}

/// A callee's `attach` in call `index` subscribes `link` to its roster: in a group call told everyone now, and every
/// change after (in a 1:1 call, from the answer on).
pub(crate) fn watch(state: &mut State, index: usize, link: &str) {
    let Some(call) = state.calling.calls.get_mut(index) else {
        return;
    };
    if call.test_call || call.roster_watchers.iter().any(|watcher| watcher == link) {
        return;
    }
    call.roster_watchers.push(link.to_owned());
    if call.group_chat.is_some()
        && let Some(body) = body(state, index, None)
    {
        state.call_back(Some(link), body);
    }
}

/// `mri` left group call `index`: everyone left is told; whether anyone is left.
pub(crate) fn leave(state: &mut State, index: usize, mri: &str) -> bool {
    let Some(call) = state.calling.calls.get_mut(index) else {
        return false;
    };
    for party in call.roster.iter_mut().filter(|party| party.mri == mri) {
        party.active = false;
        party.video_on = false;
    }
    // a party that leaves isn't told any more, at its `attach`'s callback either
    let gone: Vec<String> = call
        .roster
        .iter()
        .filter(|party| party.mri == mri)
        .filter_map(|party| party.link.clone())
        .collect();
    call.roster_watchers.retain(|link| !gone.contains(link));
    let left = call.roster.iter().any(|party| party.active);
    send(state, index, Some(mri));
    left
}

/// `mri`'s main video in group call `index` goes out (`on`) or not: everyone is told.
pub(crate) fn video(state: &mut State, index: usize, mri: &str, on: bool) {
    let Some(call) = state
        .calling
        .calls
        .get_mut(index)
        .filter(|call| call.group_chat.is_some())
    else {
        return;
    };
    let mut changed = false;
    for party in call
        .roster
        .iter_mut()
        .filter(|party| party.mri == mri && party.video_on != on)
    {
        party.video_on = on;
        changed = true;
    }
    if changed {
        send(state, index, Some(mri));
    }
}

/// The change of `changed` in group call `index` (everyone, for `None`) as a roster update, its version counted up.
fn body(state: &mut State, index: usize, changed: Option<&str>) -> Option<Value> {
    let call = state.calling.calls.get_mut(index)?;
    call.roster_version += 1;
    let version = call.roster_version;
    let (id, parties, group) = (call.id.clone(), call.roster.clone(), call.group_chat.is_some());
    let mut participants = Map::new();
    for (number, party) in (1_u64..).zip(&parties) {
        if changed.is_some_and(|mri| mri != party.mri) {
            continue;
        }
        let sources = group.then(|| state.mixer_sources(&id, &party.mri)).flatten();
        participants.insert(
            party.mri.clone(),
            entry(state, party, (number, version), group, sources),
        );
    }
    Some(json!({"participants": participants, "type": "Delta", "sequenceNumber": version}))
}

/// The change of `changed` in group call `index` delivered to every party still in it and every watcher, once each.
fn send(state: &mut State, index: usize, changed: Option<&str>) {
    let Some(body) = body(state, index, changed) else {
        return;
    };
    let call = &state.calling.calls[index];
    let mut links: Vec<String> = call
        .roster
        .iter()
        .filter(|party| party.active)
        .filter_map(|party| party.link.clone())
        .collect();
    for watcher in &call.roster_watchers {
        if !links.contains(watcher) {
            links.push(watcher.clone());
        }
    }
    for link in links {
        state.call_back(Some(&link), body.clone());
    }
}

/// One party as the roster lists it: a group call's (`group`) with its `sources` if any; a 1:1 call's endpoint lists
/// no media streams, as captured.
fn entry(
    state: &State,
    party: &Party,
    (number, version): (u64, u64),
    group: bool,
    sources: Option<(i64, i64)>,
) -> Value {
    let stream = |kind: &str, label: &str, source: Option<i64>, direction: &str| {
        let mut stream = json!({"type": kind, "label": label, "direction": direction, "serverMuted": false});
        if let Some(source) = source {
            stream["sourceId"] = json!(source);
        }
        stream
    };
    let endpoint = format!("00000000-0000-4000-8000-{number:012}");
    let endpoints = match group {
        _ if !party.active => json!({}),
        true => {
            let direction = if party.video_on { "sendrecv" } else { "recvonly" };
            json!({endpoint: {"call": {"mediaStreams": [
                stream("audio", "main-audio", sources.map(|(audio, _)| audio), "sendrecv"),
                stream("video", "main-video", sources.map(|(_, video)| video), direction),
            ]}}})
        }
        false => json!({endpoint: {"call": {"serverMuteVersion": 0}, "modalityJoined": "Call"}}),
    };
    // each endpoint in the call reports its microphone (the mock's endpoints report once)
    let mut endpoints = endpoints;
    if let Some(fields) = endpoints.as_object_mut() {
        for endpoint in fields.values_mut() {
            endpoint["endpointState"] = json!({"endpointStateSequenceNumber": 1, "state": {"isMuted": party.muted}});
        }
    }
    let mut entry = json!({
        "version": version,
        "state": if party.active { "active" } else { "inactive" },
        "details": {"id": party.mri, "displayName": state.display_name(&party.mri)},
        "endpoints": endpoints,
    });
    if let Some(hand) = &party.hand {
        entry["publishedStates"] =
            json!([{"typeRank": 1, "stateType": "raiseHands", "content": {"skinTone": 0}, "stateId": hand}]);
    }
    entry
}
