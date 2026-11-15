//! The call controller's wire (`docs/protocol/calling.md`): the links it hands out, all under
//! `<origin>/flightproxy/api/v2/ep/`, the answers of its requests and the bodies it delivers to the parties'
//! callbacks, in the shapes the client reads (`tests/fixtures/calling/`).

use std::collections::BTreeMap;

use base64::Engine as _;
use serde_json::{Map, Value, json};

use super::state::{Call, KEEP_ALIVE, Party, Side};

/// A party's callbacks, by the name its request gave each.
pub(crate) type Links = BTreeMap<String, String>;

/// The names the conversation service and the call controller have behind the flight proxy.
pub(crate) const CONVERSATION_HOST: &str = "conv.own";
pub(crate) const CONTROLLER_HOST: &str = "cc.own";

/// A time as Teams writes it: seven fractional digits.
pub(crate) fn iso(ms: i64) -> String {
    let time = chrono::DateTime::from_timestamp_millis(ms).unwrap_or_default();
    format!("{}0Z", time.format("%Y-%m-%dT%H:%M:%S%.6f"))
}

/// A callback's endpoint and its path under the endpoint's push URL, when `link` is a call agent's callback under
/// this server's Trouter.
pub(crate) fn callback<'a>(origin: &str, link: &'a str) -> Option<(&'a str, &'a str)> {
    let rest = link.strip_prefix(origin)?.strip_prefix("/trouter/v4/f/")?;
    let (endpoint, path) = rest.split_once('/')?;
    (!endpoint.is_empty() && path.starts_with("callAgent/") && !path.contains(['?', '#'])).then_some((endpoint, path))
}

/// Every callback `body` hands out, wherever in it, by its name (the first of a name).
pub(crate) fn links_in(origin: &str, body: &Value) -> Links {
    let mut links = Links::new();
    collect(origin, body, &mut links);
    links
}

fn collect(origin: &str, value: &Value, links: &mut Links) {
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                match value {
                    Value::String(link) if callback(origin, link).is_some() => {
                        links.entry(name.clone()).or_insert_with(|| link.clone());
                    }
                    other => collect(origin, other, links),
                }
            }
        }
        Value::Array(values) => values.iter().for_each(|value| collect(origin, value, links)),
        _ => {}
    }
}

/// Whether `body` names a URL that is no callback of this server's Trouter where a callback belongs: under `links`,
/// or as a roster's `rosterUpdate`.
pub(crate) fn foreign_callback(origin: &str, body: &Value) -> bool {
    fn links_foreign(origin: &str, links: &Value) -> bool {
        links.as_object().is_some_and(|links| {
            links
                .values()
                .filter_map(Value::as_str)
                .any(|link| callback(origin, link).is_none())
        })
    }
    match body {
        Value::Object(fields) => fields.iter().any(|(name, value)| match name.as_str() {
            "links" => links_foreign(origin, value),
            "rosterUpdate" => value.as_str().is_some_and(|link| callback(origin, link).is_none()),
            _ => foreign_callback(origin, value),
        }),
        Value::Array(values) => values.iter().any(|value| foreign_callback(origin, value)),
        _ => false,
    }
}

pub(crate) fn conversation_url(origin: &str, conversation: &str, op: Option<&str>) -> String {
    let op = op.map(|op| format!("/{op}")).unwrap_or_default();
    format!("{origin}/flightproxy/api/v2/ep/{CONVERSATION_HOST}/conv/{conversation}{op}")
}

/// A call controller link: `…/cc/v1/<kind>/<call>/1/<at>/<number>[/<op>]`.
fn controller_url(origin: &str, call: &str, (kind, at, number): (&str, &str, u64), op: Option<&str>) -> String {
    let op = op.map(|op| format!("/{op}")).unwrap_or_default();
    format!("{origin}/flightproxy/api/v2/ep/{CONTROLLER_HOST}/cc/v1/{kind}/{call}/1/{at}/{number}{op}")
}

/// The party as it names itself in `named`, with the id and name the server knows it by.
pub(crate) fn named_by(named: &Value, party: &Party) -> Value {
    let text = |key: &str| named.get(key).and_then(Value::as_str);
    json!({
        "id": party.mri(),
        "displayName": party.name,
        "endpointId": text("endpointId").unwrap_or("00000000-0000-0000-0000-000000000000"),
        "participantId": text("participantId"),
        "languageId": text("languageId"),
    })
}

/// The conversation's links a party operates on.
fn conversation_links(origin: &str, conversation: &str) -> Value {
    let link = |op: &str| conversation_url(origin, conversation, Some(op));
    json!({
        "leave": link("leave"),
        "updateEndpointMetadata": link("updateEndpointMetadata"),
        "updateEndpointState": link("updateEndpointState"),
        "updateParticipantProperties": link("updateParticipantProperties"),
        "notificationLinks": link("notificationLinks"),
    })
}

/// The conversation as a start or a subscription answers it.
pub(crate) fn conversation(origin: &str, region: &str, call: &Call) -> Value {
    json!({
        "conversationController": conversation_url(origin, &call.conversation, None),
        "sequenceNumber": 1,
        "subject": "",
        "region": region,
        "conversationStartTime": iso(call.started_at),
        "state": {"isMultiParty": false, "groupCallInitiator": null, "isBroadcast": false},
        "roster": {"type": "Delta", "sequenceNumber": call.roster_sequence, "participants": {}},
        "links": conversation_links(origin, &call.conversation),
    })
}

/// The callee's invitation: `{"evt": 107, "gp": <base64 JSON>}`, with the caller's offer.
pub(crate) fn invitation(origin: &str, call: &Call, tenant: &str, region: &str) -> Value {
    let forked = |op: &str| controller_url(origin, &call.id, ("forked", "i1", 1), Some(op));
    let party = |party: &Party| {
        let mut named = party.participant.clone();
        named["displayNameSource"] = json!("substrate");
        named["hidden"] = json!(false);
        named["tenantId"] = json!(tenant);
        named["propertyBag"] = Value::Null;
        named
    };
    let mut from = party(&call.caller);
    from["region"] = json!(region);
    let mut to = party(&call.callee);
    to["endpointId"] = json!("00000000-0000-0000-0000-000000000000");
    let gp = json!({
        "callNotification": {
            "from": from,
            "to": to,
            "links": {
                "attach": forked("attach"),
                "mediaAnswer": "cc://ma",
                "progress": forked("progress"),
                "reject": forked("reject"),
            },
            "mediaContent": call.offer,
            "replaces": null,
            "voicemailSettings": {},
        },
        "conversationInvitation": {
            "conversationController": conversation_url(origin, &call.conversation, None),
            "isMultiParty": false,
            "isBroadcast": false,
        },
        "debugContent": {
            "callId": call.id,
            "callStartTime": iso(call.started_at),
            "participantId": call.callee.participant["participantId"],
        },
        "groupContext": null,
    });
    json!({"evt": 107, "gp": base64::engine::general_purpose::STANDARD.encode(gp.to_string())})
}

/// The answer of an `attach`: the ringing leg's links and the conversation its subscription joined.
pub(crate) fn attach_answer(origin: &str, call: &Call) -> Value {
    let incoming = |op: &str| controller_url(origin, &call.id, ("incoming", "t", 1), Some(op));
    let controller = conversation_url(origin, &call.conversation, None);
    json!({
        "participants": {"from": call.caller.participant, "to": call.callee.participant},
        "callInvitation": {
            "callModalities": ["audio"],
            "links": {
                "progress": incoming("progress"),
                "acceptance": incoming("accept"),
                "callLeg": incoming("reject"),
            },
            "mediaContent": null,
            "replaces": null,
            "voicemailSettings": {},
        },
        "additionalActionResponses": [{
            "url": controller,
            "output": {
                "conversationController": controller,
                "sequenceNumber": 1,
                "roster": {"type": "Delta", "sequenceNumber": call.roster_sequence, "participants": {}},
                "state": {"isMultiParty": false, "groupCallInitiator": null, "isBroadcast": false},
                "links": conversation_links(origin, &call.conversation),
            },
        }],
    })
}

/// A party's links on the call that is up: its call leg (the keep-alive's), renegotiation, hold and the media
/// lines' update.
fn leg_links(origin: &str, call: &Call, at: &str) -> Map<String, Value> {
    let active = |op: Option<&str>| controller_url(origin, &call.id, ("active", at, 1), op);
    let mut links = Map::new();
    links.insert("callLeg".into(), json!(active(None)));
    links.insert("mediaRenegotiation".into(), json!(active(Some("renegotiate"))));
    links.insert(
        "updateMediaDescriptions".into(),
        json!(active(Some("updateMediaDescriptions"))),
    );
    links.insert("hold".into(), json!(active(Some("hold"))));
    links.insert(
        "applyChannelParameters".into(),
        json!(controller_url(
            origin,
            &call.id,
            ("mcProxy", at, 1),
            Some("applyChannelParameters")
        )),
    );
    links
}

/// The answer of the callee's `accept`: its acknowledgement, with the call leg's links.
pub(crate) fn accept_answer(origin: &str, call: &Call) -> Value {
    json!({
        "callAcceptanceAcknowledgement": {
            "links": leg_links(origin, call, "a4"),
            "callKeepAliveInterval": KEEP_ALIVE,
        },
        "debugContent": null,
    })
}

/// The caller's acceptance: who answered, its answer (`media`) and the call leg's links.
pub(crate) fn acceptance(origin: &str, call: &Call, media: &Value) -> Value {
    let mut links = leg_links(origin, call, "k1");
    links.insert(
        "acknowledgement".into(),
        json!(controller_url(
            origin,
            &call.id,
            ("callAcceptance", "a1", 1),
            Some("acknowledge")
        )),
    );
    json!({
        "callAcceptance": {
            "acceptedBy": call.callee.participant,
            "acceptedCallModalities": ["Audio"],
            "links": links,
            "mediaContent": media,
            "callKeepAliveInterval": KEEP_ALIVE,
        },
        "debugContent": null,
    })
}

/// A call's end as delivered: `sender` whose action ended it, none when the service did.
pub(crate) fn end(code: i64, phrase: &str, sender: Option<&Value>) -> Value {
    json!({
        "callEnd": {
            "code": code, "subCode": 0, "phrase": phrase, "reason": "noError", "resultCategories": ["Success"],
            "sender": sender,
        },
        "debugContent": null,
    })
}

/// The end of a call the service gave up on.
pub(crate) fn failure(code: i64, phrase: &str) -> Value {
    json!({
        "callEnd": {
            "code": code, "subCode": 0, "phrase": phrase, "reason": "clientError",
            "resultCategories": ["UnexpectedServerError"], "sender": null,
        },
        "debugContent": null,
    })
}

/// A renegotiation as the far end is told of it: the offer, and where its answer or its rejection goes.
pub(crate) fn negotiation(origin: &str, call: &Call, sender: &Value, offered: &Value, media: &Value) -> Value {
    let at = |op: &str| controller_url(origin, &call.id, ("negotiations", "t", call.negotiations), Some(op));
    json!({
        "mediaNegotiation": {
            "sender": sender,
            "mediaContent": media,
            "callModalities": offered.get("callModalities").cloned().unwrap_or_else(|| json!([])),
            "links": {"mediaAnswer": at("answer"), "rejection": at("reject")},
        },
    })
}

/// A renegotiation's answer as the party that offered is told of it, with where it acknowledges it.
pub(crate) fn media_answer(origin: &str, call: &Call, sender: &str, answer: &Value, media: &Value) -> Value {
    let acknowledge = controller_url(
        origin,
        &call.id,
        ("mediaAnswers", "t", call.negotiations),
        Some("acknowledge"),
    );
    json!({
        "mediaAnswer": {
            "sender": {"id": sender},
            "mediaContent": media,
            "callModalities": answer.get("callModalities").cloned().unwrap_or_else(|| json!([])),
            "links": {"mediaAcknowledgement": acknowledge},
        },
    })
}

pub(crate) fn media_acknowledgement() -> Value {
    json!({"mediaAcknowledgement": {"code": 0, "subCode": 0, "phrase": "", "resultCategories": ["Success"]}})
}

pub(crate) fn negotiation_failure(code: i64, phrase: &str) -> Value {
    json!({"mediaNegotiationFailure": {"code": code, "phrase": phrase, "resultCategories": ["ExpectedError"]}})
}

/// The answer of a hold: where it is taken back.
pub(crate) fn hold_answer(origin: &str, call: &Call) -> Value {
    let resume = controller_url(origin, &call.id, ("holdSession", "a6", 1), Some("resume"));
    json!({"holdResponse": {"links": {"resume": resume}}, "debugContent": null})
}

pub(crate) fn hold_completion() -> Value {
    json!({"holdCompletion": {"reason": "noError", "code": 0, "phrase": "Success", "audioBotType": "audioPlayback"}})
}

pub(crate) fn resume_completion() -> Value {
    json!({"resumeCompletion": {"reason": "noError", "code": 0, "phrase": "Success"}})
}

/// The music-on-hold app's roster entry while it plays, or once it left.
pub(crate) fn music_entry(version: u64, playing: bool) -> Value {
    let endpoints = if playing {
        json!({"00000000-0000-4000-8000-0000000000f0": {"call": {
            "mediaStreams": [{"type": "audio", "label": "main-audio", "direction": "sendrecv", "serverMuted": false}],
            "serverMuteVersion": 0,
        }}})
    } else {
        json!({})
    };
    json!({
        "version": version,
        "state": if playing { "active" } else { "inactive" },
        "details": {"id": super::state::MUSIC_ON_HOLD, "displayName": null},
        "endpoints": endpoints,
    })
}

/// One party as the roster lists it: a 1:1 call's endpoint lists no media streams.
fn roster_entry(party: &Party, version: u64) -> Value {
    let endpoint = party.participant["endpointId"].as_str().unwrap_or_default();
    let endpoints = if party.active.is_some() {
        json!({endpoint: {
            "call": {"serverMuteVersion": 0},
            "modalityJoined": "Call",
            "participantId": party.participant["participantId"],
            "endpointJoinTime": party.joined,
            "endpointState": {
                "endpointStateSequenceNumber": party.state_sequence.max(1),
                "state": {"isMuted": party.muted},
            },
        }})
    } else {
        json!({})
    };
    json!({
        "version": version,
        "state": if party.active.is_some() { "active" } else { "inactive" },
        "details": {"id": party.mri(), "displayName": party.name},
        "endpoints": endpoints,
    })
}

/// The roster update of `sides`' entries, at the call's current sequence.
pub(crate) fn roster(call: &Call, sides: &[Side]) -> Value {
    let participants: Map<String, Value> = sides
        .iter()
        .map(|side| call.party(*side))
        .map(|party| (party.mri(), roster_entry(party, call.roster_sequence)))
        .collect();
    json!({"participants": participants, "type": "Delta", "sequenceNumber": call.roster_sequence})
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str = "https://own.example.test";

    #[test]
    fn only_a_call_agents_callback_under_this_origins_trouter_is_one() {
        let link = "https://own.example.test/trouter/v4/f/Endpoint/callAgent/a/1/call/end/";
        assert_eq!(callback(ORIGIN, link), Some(("Endpoint", "callAgent/a/1/call/end/")));
        for other in [
            "https://other.example.test/trouter/v4/f/Endpoint/callAgent/a/",
            "https://own.example.test/trouter/v4/f/Endpoint/messaging",
            "https://own.example.test/trouter/v4/f//callAgent/a/",
            "https://own.example.test/trouter/v4/f/Endpoint/callAgent/a/?x=1",
            "https://own.example.test.evil/trouter/v4/f/Endpoint/callAgent/a/",
            "cc://ma",
        ] {
            assert_eq!(callback(ORIGIN, other), None, "{other}");
        }
    }

    #[test]
    fn callbacks_are_found_by_name_wherever_a_body_hands_them_out() {
        let end = "https://own.example.test/trouter/v4/f/E/callAgent/a/1/call/end/";
        let roster = "https://own.example.test/trouter/v4/f/E/callAgent/a/2/conversation/rosterUpdate/";
        let body = json!({
            "attach": {"links": {"end": end}},
            "additionalActions": [{"input": {"conversationRequest": {"roster": {"rosterUpdate": roster}}}}],
            "other": "https://elsewhere.example/x",
        });
        let links = links_in(ORIGIN, &body);
        assert_eq!(links.len(), 2);
        assert_eq!(links["end"], end);
        assert_eq!(links["rosterUpdate"], roster);
        assert!(!foreign_callback(ORIGIN, &body));
        let foreign = json!({"callInvitation": {"links": {"end": "https://elsewhere.example/x"}}});
        assert!(foreign_callback(ORIGIN, &foreign));
        let roster = json!([{"roster": {"rosterUpdate": "https://elsewhere.example/x"}}]);
        assert!(foreign_callback(ORIGIN, &roster));
    }

    #[test]
    fn times_are_written_with_seven_fractional_digits() {
        assert_eq!(iso(1_700_000_000_123), "2023-11-14T22:13:20.1230000Z");
    }
}
