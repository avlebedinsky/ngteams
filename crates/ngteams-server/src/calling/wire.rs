//! The call controller's wire (`docs/protocol/calling.md`): the links it hands out, all under
//! `<origin>/flightproxy/api/v2/ep/`, the answers of its requests and the bodies it delivers to the parties'
//! callbacks, in the shapes the client reads (`tests/fixtures/calling/`).

use std::collections::BTreeMap;

use base64::Engine as _;
use serde_json::{Map, Value, json};

use super::state::{Call, KEEP_ALIVE, Kind, Party, Seat, Status};

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

/// The `endpointId` of an endpoint nobody named.
const ZERO_ENDPOINT: &str = "00000000-0000-0000-0000-000000000000";

/// The leg a call controller link names: a 1:1 call's are all `1`, a group call's the party's seat.
pub(crate) fn leg_of(call: &Call, seat: Seat) -> usize {
    match call.kind {
        Kind::OneToOne => 1,
        Kind::Group => seat.0,
    }
}

/// A call controller link of the party at `seat`: `…/cc/v1/<kind>/<call>/<leg>/<at>/<number>[/<op>]`.
fn controller_url(
    origin: &str,
    (call, seat): (&Call, Seat),
    (kind, at, number): (&str, &str, u64),
    op: Option<&str>,
) -> String {
    let op = op.map(|op| format!("/{op}")).unwrap_or_default();
    let (id, leg) = (&call.id, leg_of(call, seat));
    format!("{origin}/flightproxy/api/v2/ep/{CONTROLLER_HOST}/cc/v1/{kind}/{id}/{leg}/{at}/{number}{op}")
}

/// The party as it names itself in `named`, with the id and name the server knows it by.
pub(crate) fn named_by(named: &Value, party: &Party) -> Value {
    let text = |key: &str| named.get(key).and_then(Value::as_str);
    json!({
        "id": party.mri(),
        "displayName": party.name,
        "endpointId": text("endpointId").unwrap_or(ZERO_ENDPOINT),
        "participantId": text("participantId"),
        "languageId": text("languageId"),
    })
}

/// The conversation's links a party operates on; a group call's also where someone is added to it.
fn conversation_links(origin: &str, call: &Call) -> Value {
    let link = |op: &str| conversation_url(origin, &call.conversation, Some(op));
    let mut links = json!({
        "leave": link("leave"),
        "updateEndpointMetadata": link("updateEndpointMetadata"),
        "updateEndpointState": link("updateEndpointState"),
        "updateParticipantProperties": link("updateParticipantProperties"),
        "notificationLinks": link("notificationLinks"),
    });
    if call.kind == Kind::Group {
        links["addParticipant"] = json!(link("add"));
        links["addParticipantAndModality"] = json!(link("add"));
    }
    links
}

/// The conversation's state: a group call's is multi-party, started by its caller.
fn conversation_state(call: &Call) -> Value {
    match call.kind {
        Kind::OneToOne => json!({"isMultiParty": false, "groupCallInitiator": null, "isBroadcast": false}),
        Kind::Group => json!({
            "isMultiParty": true, "groupCallInitiator": call.party(Seat::CALLER).mri(), "isBroadcast": false,
        }),
    }
}

/// The roster a conversation's answer gives: a 1:1 call's none, a group call's whole.
fn answered_roster(call: &Call) -> Value {
    match call.kind {
        Kind::OneToOne => json!({"type": "Delta", "sequenceNumber": call.roster_sequence, "participants": {}}),
        Kind::Group => roster(call, &call.rostered()),
    }
}

/// The conversation as a start or a subscription answers it.
pub(crate) fn conversation(origin: &str, region: &str, call: &Call) -> Value {
    json!({
        "conversationController": conversation_url(origin, &call.conversation, None),
        "sequenceNumber": 1,
        "subject": "",
        "region": region,
        "conversationStartTime": iso(call.started_at),
        "state": conversation_state(call),
        "roster": answered_roster(call),
        "links": conversation_links(origin, call),
    })
}

/// A conversation that never runs: the answer of a start refused at once at its `conversationEnd` callback.
pub(crate) fn unrun_conversation(origin: &str, region: &str, conversation: &str) -> Value {
    json!({
        "conversationController": conversation_url(origin, conversation, None),
        "sequenceNumber": 1,
        "subject": "",
        "region": region,
        "state": {"isMultiParty": true, "groupCallInitiator": null, "isBroadcast": false},
        "roster": {"type": "Delta", "sequenceNumber": 0, "participants": {}},
        "links": {"leave": conversation_url(origin, conversation, Some("leave"))},
    })
}

/// What a start in a chat whose call still runs is told at its `conversationEnd` callback: `409` / `5704`, naming the
/// running call's conversation and the chat (`docs/protocol/calling.md`, "Joining a running call").
pub(crate) fn running_elsewhere(origin: &str, running: &Call) -> Value {
    json!({
        "conversationEnd": {
            "code": 409,
            "subCode": 5704,
            "phrase": "Conversation cannot be created due to a failure to resolve ownership",
            "resultCategories": ["ExpectedError"],
            "conversationUrl": {"Location": conversation_url(origin, &running.conversation, None)},
            "threadId": running.chat,
        },
    })
}

/// The callee's invitation: `{"evt": 107, "gp": <base64 JSON>}`, with the caller's offer.
pub(crate) fn invitation(origin: &str, call: &Call, tenant: &str, region: &str) -> Value {
    let forked = |op: &str| controller_url(origin, (call, Seat::CALLEE), ("forked", "i1", 1), Some(op));
    let party = |party: &Party| {
        let mut named = party.participant.clone();
        named["displayNameSource"] = json!("substrate");
        named["hidden"] = json!(false);
        named["tenantId"] = json!(tenant);
        named["propertyBag"] = Value::Null;
        named
    };
    let mut from = party(call.party(Seat::CALLER));
    from["region"] = json!(region);
    let mut to = party(call.party(Seat::CALLEE));
    to["endpointId"] = json!(ZERO_ENDPOINT);
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
            "participantId": call.party(Seat::CALLEE).participant["participantId"],
        },
        "groupContext": null,
    });
    json!({"evt": 107, "gp": base64::engine::general_purpose::STANDARD.encode(gp.to_string())})
}

/// The answer of an `attach` of the party at `seat`: the ringing leg's links and the conversation its subscription
/// joined.
pub(crate) fn attach_answer(origin: &str, call: &Call, seat: Seat) -> Value {
    let incoming = |op: &str| controller_url(origin, (call, seat), ("incoming", "t", 1), Some(op));
    let controller = conversation_url(origin, &call.conversation, None);
    json!({
        "participants": {"from": call.party(Seat::CALLER).participant, "to": call.party(seat).participant},
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
                "roster": answered_roster(call),
                "state": conversation_state(call),
                "links": conversation_links(origin, call),
            },
        }],
    })
}

/// The links of the party at `seat` on the call that is up: its call leg (the keep-alive's), renegotiation, hold
/// (a 1:1 call's only) and the media lines' update.
fn leg_links(origin: &str, (call, seat): (&Call, Seat), at: &str) -> Map<String, Value> {
    let active = |op: Option<&str>| controller_url(origin, (call, seat), ("active", at, 1), op);
    let mut links = Map::new();
    links.insert("callLeg".into(), json!(active(None)));
    links.insert("mediaRenegotiation".into(), json!(active(Some("renegotiate"))));
    links.insert(
        "updateMediaDescriptions".into(),
        json!(active(Some("updateMediaDescriptions"))),
    );
    if call.kind == Kind::OneToOne {
        links.insert("hold".into(), json!(active(Some("hold"))));
    }
    links.insert(
        "applyChannelParameters".into(),
        json!(controller_url(
            origin,
            (call, seat),
            ("mcProxy", at, 1),
            Some("applyChannelParameters")
        )),
    );
    links
}

/// The answer of the `accept` of the party at `seat`: its acknowledgement, with the call leg's links.
pub(crate) fn accept_answer(origin: &str, call: &Call, seat: Seat) -> Value {
    json!({
        "callAcceptanceAcknowledgement": {
            "links": leg_links(origin, (call, seat), "a4"),
            "callKeepAliveInterval": KEEP_ALIVE,
        },
        "debugContent": null,
    })
}

/// The caller's acceptance: who answered, its answer (`media`) and the call leg's links.
pub(crate) fn acceptance(origin: &str, call: &Call, media: &Value) -> Value {
    let mut links = leg_links(origin, (call, Seat::CALLER), "k1");
    links.insert(
        "acknowledgement".into(),
        json!(controller_url(
            origin,
            (call, Seat::CALLER),
            ("callAcceptance", "a1", 1),
            Some("acknowledge")
        )),
    );
    json!({
        "callAcceptance": {
            "acceptedBy": call.party(Seat::CALLEE).participant,
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
    let at = |op: &str| {
        let number = call.negotiations;
        controller_url(origin, (call, Seat::CALLER), ("negotiations", "t", number), Some(op))
    };
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
        (call, Seat::CALLER),
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
    let resume = controller_url(origin, (call, Seat::CALLER), ("holdSession", "a6", 1), Some("resume"));
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

/// What an endpoint in the call sends and takes: a 1:1 call's lists no media streams (its media goes through no
/// mixer); a group call's its main audio and its main video (only taken, until the media node forwards video), by
/// the party's sources.
fn endpoint_call(kind: Kind, party: &Party) -> Value {
    let Some(leg) = party.leg.as_ref().filter(|_| kind == Kind::Group) else {
        return json!({"serverMuteVersion": 0});
    };
    json!({
        "mediaStreams": [
            {"type": "audio", "label": "main-audio", "sourceId": leg.sources.audio, "direction": "sendrecv",
                "serverMuted": false},
            {"type": "video", "label": "main-video", "sourceId": leg.sources.video, "direction": "recvonly"},
        ],
        "serverMuteVersion": 0,
    })
}

/// One party as the roster lists it.
fn roster_entry(kind: Kind, party: &Party, version: u64) -> Value {
    let endpoint = party.participant["endpointId"].as_str().unwrap_or_default();
    let endpoints = if party.active.is_some() {
        json!({endpoint: {
            "call": endpoint_call(kind, party),
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

/// The roster update of `seats`' entries, at the call's current sequence.
pub(crate) fn roster(call: &Call, seats: &[Seat]) -> Value {
    let participants: Map<String, Value> = seats
        .iter()
        .map(|seat| call.party(*seat))
        .map(|party| (party.mri(), roster_entry(call.kind, party, call.roster_sequence)))
        .collect();
    json!({"participants": participants, "type": "Delta", "sequenceNumber": call.roster_sequence})
}

/// A group call's invitation of the party at `seat`: `{"evt": 109, "gp": <base64 JSON>}`, with the media node's offer
/// to its leg (`deliveries.json`'s group invitation).
pub(crate) fn group_invitation(
    origin: &str,
    call: &Call,
    seat: Seat,
    (tenant, region): (&str, &str),
    offer: &Value,
) -> Value {
    let forked = |op: &str| controller_url(origin, (call, seat), ("forked", "i1", 1), Some(op));
    let named = |party: &Party| {
        let mut named = party.participant.clone();
        named["displayNameSource"] = json!("substrate");
        named["hidden"] = json!(false);
        named["tenantId"] = json!(tenant);
        named["propertyBag"] = Value::Null;
        named
    };
    let caller = call.party(Seat::CALLER);
    let mut from = named(caller);
    from["region"] = json!(region);
    let mut to = named(call.party(seat));
    to["endpointId"] = json!(ZERO_ENDPOINT);
    let mut media = offer.clone();
    media["fromMixer"] = json!(true);
    media["mediaLegId"] = json!(call.party(seat).leg.as_ref().map(|leg| leg.id.as_str()));
    media["originator"] = json!("mcGvc");
    media["skipIceReinvite"] = json!(true);
    media["newOffer"] = json!(false);
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
            "mediaContent": media,
            "fromMixer": true,
            "conversationType": "groupCall",
            "isDialOut": true,
            "isDialIn": false,
        },
        "groupChat": {"threadId": call.chat, "messageId": "0"},
        "meetingProperties": {"meetingLiveState": {"phase": "staging", "sequenceNumber": 0, "seqNum": 0}},
        "conversationInvitation": {
            "conversationController": conversation_url(origin, &call.conversation, None),
            "isMultiParty": true,
            "isBroadcast": false,
            "groupCallInitiator": caller.mri(),
            "isHostless": true,
        },
        "debugContent": {
            "callId": call.id,
            "callStartTime": iso(call.started_at),
            "participantId": call.party(seat).participant["participantId"],
        },
        "groupContext": null,
    });
    json!({"evt": 109, "gp": base64::engine::general_purpose::STANDARD.encode(gp.to_string())})
}

/// The acceptance the media node gives the party at `seat` on its leg (a group call's caller, a joiner): its `answer`,
/// the call leg's links (`answers.json`'s acceptance callback).
pub(crate) fn mixer_acceptance(origin: &str, call: &Call, seat: Seat, answer: &Value) -> Value {
    let mut links = leg_links(origin, (call, seat), "k1");
    links.insert(
        "acknowledgement".into(),
        json!(controller_url(
            origin,
            (call, seat),
            ("callAcceptance", "a1", 1),
            Some("acknowledge")
        )),
    );
    json!({
        "callAcceptance": {
            "acceptedCallModalities": [],
            "links": links,
            "mediaContent": mixer_media(call, seat, answer),
            "callKeepAliveInterval": KEEP_ALIVE,
        },
        "debugContent": null,
    })
}

/// The media node's offer or answer on the leg of the party at `seat`, as the call's deliveries carry it.
fn mixer_media(call: &Call, seat: Seat, sdp: &Value) -> Value {
    let mut media = sdp.clone();
    media["mediaLegId"] = json!(call.party(seat).leg.as_ref().map(|leg| leg.id.as_str()));
    media["escalationOccurring"] = json!(false);
    media["newOffer"] = json!(false);
    media["fromMixer"] = json!(true);
    media["skipIceReinvite"] = json!(false);
    media
}

/// The media node's answer to the renegotiation of the party at `seat`, with where it acknowledges it.
pub(crate) fn mixer_media_answer(origin: &str, call: &Call, seat: Seat, answer: &Value) -> Value {
    let acknowledge = controller_url(
        origin,
        (call, seat),
        ("mediaAnswers", "t", call.negotiations),
        Some("acknowledge"),
    );
    json!({
        "mediaAnswer": {
            "sender": null,
            "mediaContent": mixer_media(call, seat, answer),
            "callModalities": ["Audio"],
            "links": {"mediaAcknowledgement": acknowledge},
        },
    })
}

/// What an add of `invitee` to a group call is told when it rings no endpoint (as captured for someone with no
/// calling endpoint).
pub(crate) fn add_failure(invitee: &str) -> Value {
    let phrase = "Audio-video modality controller could not invite participant to this conversation since no callee \
        endpoints were found.";
    json!({
        "participants": {invitee: {"code": 580, "subCode": 5205, "phrase": phrase,
            "resultCategories": ["UnexpectedServerError"]}},
        "participantInfos": [{"participant": {"id": invitee}, "transactionEnd": {"code": 580}}],
    })
}

/// `evt` of a group call's delivery at a calling endpoint's root telling that someone came into the call.
pub(crate) const JOINED: u64 = 115;
/// … that someone left it.
pub(crate) const LEFT: u64 = 116;

/// A group call's `evt` 115 or 116 (someone came in, someone left): `{"evt", "cp": base64(gzip(JSON))}` with the
/// conversation, when each party came in and who is in it. The client reads neither; the shape is the minimum the
/// protocol notes name.
pub(crate) fn presence(origin: &str, call: &Call, evt: u64) -> Value {
    let participants: Vec<Value> = call
        .parties
        .iter()
        .filter(|party| party.status == Status::InCall)
        .map(|party| json!({"id": party.mri(), "displayName": party.name, "userJoinTime": party.joined}))
        .collect();
    let content = json!({
        "conversationController": conversation_url(origin, &call.conversation, None),
        "participants": participants,
    });
    let mut zipped = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let packed = std::io::Write::write_all(&mut zipped, content.to_string().as_bytes())
        .and_then(|()| zipped.finish())
        .unwrap_or_default();
    json!({"evt": evt, "cp": base64::engine::general_purpose::STANDARD.encode(packed)})
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
