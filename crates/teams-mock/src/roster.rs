//! A group call's roster (M3 Task 19): who is in the call, delivered at the `rosterUpdate` callbacks whenever that
//! changed, in the shape the captures show (`docs/protocol/calling.md`, "Group video"): `{"participants": {<MRI>:
//! {"version", "state", "details": {"id", "displayName"}, "endpoints": {<id>: {"call": {"mediaStreams": [main audio,
//! main video]}}}}}, "type": "Delta", "sequenceNumber"}`. A delivery lists only the party that changed, as the
//! service's do, so a client knows the others only from what it was told before.
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

use crate::world::State;

/// A party of a group call's roster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Party {
    pub(crate) mri: String,
    /// Its `rosterUpdate` callback.
    pub(crate) link: Option<String>,
    pub(crate) active: bool,
    pub(crate) video_on: bool,
}

/// The `rosterUpdate` callback a start's, a join's or an `attach`'s body hands out (wherever in it).
pub(crate) fn link_of(body: &Value) -> Option<String> {
    match body {
        Value::Object(fields) => fields.iter().find_map(|(key, value)| match value {
            Value::String(link) if key == "rosterUpdate" => Some(link.clone()),
            _ => link_of(value),
        }),
        Value::Array(values) => values.iter().find_map(link_of),
        _ => None,
    }
}

/// `mri` is in group call `index` (again), told at `link`: everyone is told.
pub(crate) fn enter(state: &mut State, index: usize, mri: &str, link: Option<String>) {
    let Some(call) = state.calling.calls.get_mut(index) else {
        return;
    };
    if call.group_chat.is_none() {
        return;
    }
    call.roster.retain(|party| party.mri != mri);
    call.roster.push(Party {
        mri: mri.to_owned(),
        link,
        active: true,
        video_on: false,
    });
    send(state, index, Some(mri));
}

/// A callee's `attach` in group call `index` subscribes `link` to its roster: told everyone now, and every change after.
pub(crate) fn watch(state: &mut State, index: usize, link: &str) {
    let Some(call) = state.calling.calls.get_mut(index) else {
        return;
    };
    if call.group_chat.is_none() || call.roster_watchers.iter().any(|watcher| watcher == link) {
        return;
    }
    call.roster_watchers.push(link.to_owned());
    if let Some(body) = body(state, index, None) {
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
    let Some(call) = state.calling.calls.get_mut(index) else {
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
    let (id, parties) = (call.id.clone(), call.roster.clone());
    let mut participants = Map::new();
    for (number, party) in (1_u64..).zip(&parties) {
        if changed.is_some_and(|mri| mri != party.mri) {
            continue;
        }
        let sources = state.mixer_sources(&id, &party.mri);
        participants.insert(party.mri.clone(), entry(state, party, number, version, sources));
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

/// One party as the roster lists it.
fn entry(state: &State, party: &Party, number: u64, version: u64, sources: Option<(i64, i64)>) -> Value {
    let stream = |kind: &str, label: &str, source: Option<i64>, direction: &str| {
        let mut stream = json!({"type": kind, "label": label, "direction": direction, "serverMuted": false});
        if let Some(source) = source {
            stream["sourceId"] = json!(source);
        }
        stream
    };
    let endpoints = if party.active {
        let direction = if party.video_on { "sendrecv" } else { "recvonly" };
        json!({format!("00000000-0000-4000-8000-{number:012}"): {"call": {"mediaStreams": [
            stream("audio", "main-audio", sources.map(|(audio, _)| audio), "sendrecv"),
            stream("video", "main-video", sources.map(|(_, video)| video), direction),
        ]}}})
    } else {
        json!({})
    };
    json!({
        "version": version,
        "state": if party.active { "active" } else { "inactive" },
        "details": {"id": party.mri, "displayName": state.display_name(&party.mri)},
        "endpoints": endpoints,
    })
}
