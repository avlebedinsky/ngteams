//! The machine on its own, fed by hand: what each delivery, outcome and action makes it do (the whole flows on the
//! mock are `tests/mock.rs`).

use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};
use teams_fence::{Delivery, Mri, Observed};
use teams_proto::{ClientInfo, MediaContent};

use super::{CallError, CallEvent, CallId, CallState, Calls, DeliveryReply, Effect, End, Identity, Outcome, Placement};
use crate::WriteId;
use crate::agent::Kind;

const A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const PUSH: &str = "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000/";
const PROXY: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep";

fn identity(mri: &str) -> Identity {
    Identity {
        mri: Mri::new(mri),
        display_name: Some("Lab".into()),
        endpoint_id: "00000000-0000-4000-8000-0000000000e1".into(),
        language_id: Some("en-us".into()),
        tenant_id: "00000000-0000-4000-8000-0000000000d0".into(),
        client: ClientInfo {
            endpoint_capabilities: Some(1),
            client_endpoint_capabilities: Some(1),
            platform_name: Some("SyntheticNative/1".into()),
        },
        region: Some("amer".into()),
        partition: None,
    }
}

fn media() -> MediaContent {
    MediaContent::new("v=0\r\n", "application/sdp-ngc-1.0")
}

fn conv(rest: &str) -> String {
    format!("{PROXY}/conv-zz-01-prod-aks.conv.skype.com/conv/Conversation1{rest}?i=10-0-0-2&e=1")
}

fn cc(kind: &str, op: &str) -> String {
    format!("{PROXY}/cc-zz-01-prod-aks.cc.skype.com/cc/v1/{kind}/Call1/1/x1/1/{op}?i=10-0-0-1")
}

fn delivery(path: &str, body: &Value) -> Delivery {
    Delivery {
        connection: "calling/1".into(),
        id: 7,
        path: path.into(),
        trouter_request: Some("request-7".into()),
        chain_id: Some("chain".into()),
        message_id: Some("message".into()),
        body: body.to_string().into_bytes(),
    }
}

fn effects(calls: &mut Calls) -> Vec<Effect> {
    std::iter::from_fn(|| calls.poll()).collect()
}

/// The names of the writes among `effects`, with their ids.
fn sent(effects: &[Effect]) -> Vec<(WriteId, &'static str)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Send { id, write } => Some((*id, write.name())),
            _ => None,
        })
        .collect()
}

fn events(effects: &[Effect]) -> Vec<CallEvent> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Event(event) => Some(event.clone()),
            _ => None,
        })
        .collect()
}

fn ok(body: &Value) -> Outcome {
    Outcome::Answered {
        status: 200,
        body: body.to_string().into_bytes(),
    }
}

fn conversation_answer() -> Value {
    json!({"conversationController": conv(""), "links": {
        "leave": conv("/leave"), "updateEndpointMetadata": conv("/updateEndpointMetadata"),
        "addParticipantAndModality": conv("/add")}})
}

/// a's machine with a 1:1 call to b placed and its start answered: ringing.
fn ringing_call(now: Instant) -> (Calls, CallId) {
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let placement = Placement::Chat {
        callees: vec![Mri::new(B)],
        group_chat: None,
    };
    let call = calls.place(&placement, media(), now).unwrap();
    let placed = effects(&mut calls);
    let [(start, "start")] = sent(&placed)[..] else {
        panic!("{placed:#?}");
    };
    calls.on_outcome(start, &ok(&conversation_answer()), now);
    let answered = effects(&mut calls);
    assert_eq!(
        events(&answered),
        [CallEvent::State {
            call,
            state: CallState::Ringing
        }]
    );
    (calls, call)
}

fn acceptance_path(calls: &Calls) -> String {
    calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Call, "acceptance"))
        .unwrap()
        .to_owned()
}

fn acceptance() -> Value {
    json!({"callAcceptance": {
        "links": {"acknowledgement": cc("callAcceptance", "acknowledge")},
        "mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0"},
        "callKeepAliveInterval": 2700,
    }})
}

#[test]
fn an_action_needs_the_endpoint_and_no_call_up() {
    let now = Instant::now();
    let mut calls = Calls::new(identity(A));
    let echo = Placement::TestCall {
        echo: Mri::new("28:00000000-0000-4000-8000-0000000000ec"),
    };
    assert!(matches!(calls.place(&echo, media(), now), Err(CallError::NoEndpoint)));
    assert!(matches!(
        calls.join("19:x@thread.v2", media(), now),
        Err(CallError::NotJoinable)
    ));
    let (mut calls, call) = ringing_call(now);
    assert!(matches!(calls.place(&echo, media(), now), Err(CallError::Busy)));
    assert!(
        matches!(calls.answer(call, media()), Err(CallError::NotNow)),
        "not an invitation"
    );
    assert!(matches!(calls.decline(CallId(99)), Err(CallError::NoSuchCall)));
}

#[test]
fn an_acceptance_is_acknowledged_by_response_and_by_request_only_when_that_failed() {
    let now = Instant::now();
    let (mut calls, call) = ringing_call(now);
    let path = acceptance_path(&calls);
    calls.on_delivery(&delivery(&path, &acceptance()), Observed::Acceptance, now);
    let accepted = effects(&mut calls);
    let Some(Effect::Respond {
        delivery,
        reply: DeliveryReply::Acknowledgement(_),
    }) = accepted.first()
    else {
        panic!("{accepted:#?}");
    };
    assert_eq!((delivery.connection.as_str(), delivery.id), ("calling/1", 7));
    assert!(events(&accepted).contains(&CallEvent::State {
        call,
        state: CallState::Connected
    }));
    assert_eq!(
        sent(&accepted).iter().map(|(_, name)| *name).collect::<Vec<_>>(),
        ["update_endpoint_metadata"]
    );
    let key = delivery.clone();
    calls.on_response_failed(&key);
    let by_request = effects(&mut calls);
    assert_eq!(
        sent(&by_request).iter().map(|(_, name)| *name).collect::<Vec<_>>(),
        ["acknowledge"]
    );
    calls.on_response_failed(&key);
    assert!(effects(&mut calls).is_empty(), "once");
}

#[test]
fn the_keep_alive_falls_due_on_its_interval_and_ends_with_the_call() {
    let now = Instant::now();
    let (mut calls, call) = ringing_call(now);
    assert_eq!(calls.deadline(), None);
    let path = acceptance_path(&calls);
    calls.on_delivery(&delivery(&path, &acceptance()), Observed::Acceptance, now);
    let _ = effects(&mut calls);
    assert_eq!(calls.keep_alive_interval(), Some(Duration::from_mins(45)));
    assert_eq!(calls.deadline(), Some(now + Duration::from_mins(45)));
    calls.tick(now + Duration::from_secs(2_699));
    assert!(effects(&mut calls).is_empty());
    calls.tick(now + Duration::from_mins(45));
    let due = effects(&mut calls);
    assert_eq!(events(&due), [CallEvent::KeepAliveDue { call }]);
    assert!(sent(&due).is_empty(), "no request: none was captured");
    assert_eq!(calls.deadline(), Some(now + Duration::from_mins(90)));

    let end = calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Call, "end"))
        .unwrap()
        .to_owned();
    calls.on_delivery(
        &delivery(&end, &json!({"callEnd": {"code": 0, "phrase": "Done"}})),
        Observed::Ended,
        now,
    );
    let ended = effects(&mut calls);
    let ended_events = events(&ended);
    let [
        CallEvent::Ended {
            end: End::Remote(remote),
            ..
        },
    ] = ended_events.as_slice()
    else {
        panic!("{ended:#?}");
    };
    assert_eq!((remote.code, remote.phrase.as_deref()), (Some(0), Some("Done")));
    assert_eq!(calls.deadline(), None);
    assert_eq!(calls.current(), None);
}

#[test]
fn an_acceptance_the_fence_bound_nothing_of_ends_the_call_with_a_cancel() {
    let now = Instant::now();
    let (mut calls, call) = ringing_call(now);
    let path = acceptance_path(&calls);
    calls.on_delivery(&delivery(&path, &acceptance()), Observed::AcceptanceUnbound, now);
    let failed = effects(&mut calls);
    assert_eq!(
        sent(&failed).iter().map(|(_, name)| *name).collect::<Vec<_>>(),
        ["leave"]
    );
    assert!(events(&failed).contains(&CallEvent::Ended {
        call,
        end: End::Failed("acknowledge")
    }));
    assert!(!failed.iter().any(|effect| matches!(effect, Effect::Respond { .. })));
}

#[test]
fn a_failed_start_ends_the_call_and_nothing_is_sent_again() {
    let now = Instant::now();
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let placement = Placement::Chat {
        callees: vec![Mri::new(B)],
        group_chat: None,
    };
    let call = calls.place(&placement, media(), now).unwrap();
    let [(start, _)] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(
        start,
        &Outcome::Answered {
            status: 500,
            body: Vec::new(),
        },
        now,
    );
    let failed = effects(&mut calls);
    assert!(sent(&failed).is_empty());
    assert_eq!(
        events(&failed),
        [CallEvent::Ended {
            call,
            end: End::Failed("start")
        }]
    );
    calls.on_outcome(start, &ok(&conversation_answer()), now);
    assert!(effects(&mut calls).is_empty(), "an outcome of an ended call is dropped");
}

#[test]
fn a_hang_up_before_the_start_was_answered_cancels_once_it_is() {
    let now = Instant::now();
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let placement = Placement::Chat {
        callees: vec![Mri::new(B)],
        group_chat: None,
    };
    let call = calls.place(&placement, media(), now).unwrap();
    let [(start, _)] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.hang_up(call, now).unwrap();
    let asked = effects(&mut calls);
    assert!(sent(&asked).is_empty(), "no leave link yet");
    calls.on_outcome(start, &ok(&conversation_answer()), now + Duration::from_secs(3));
    let answered = effects(&mut calls);
    let [(leave, "leave")] = sent(&answered)[..] else {
        panic!("{answered:#?}");
    };
    calls.on_outcome(leave, &ok(&json!({})), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended { call, end: End::HungUp }]
    );
}

/// An invitation to `callee` from a, its attach link on `Call1`.
fn invitation(callee: &str) -> Value {
    let gp = json!({
        "callNotification": {
            "from": {"id": A, "displayName": "Lab A"},
            "to": {"id": callee},
            "links": {"attach": cc("forked", "attach")},
            "mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0"},
        },
        "conversationInvitation": {"conversationController": conv("")},
    });
    json!({"evt": 107, "gp": base64::engine::general_purpose::STANDARD.encode(gp.to_string())})
}

#[test]
fn only_an_offered_invitation_to_the_account_gets_its_receipt_and_attach_and_rings_once_attached() {
    let now = Instant::now();
    let mut calls = Calls::new(identity(B));
    calls.set_push_url(PUSH).unwrap();
    let root = "/v4/f/SyntheticEndpoint00000/";
    calls.on_delivery(&delivery(root, &invitation(B)), Observed::Ignored, now);
    calls.on_delivery(&delivery(root, &invitation(A)), Observed::Offered, now);
    assert!(
        effects(&mut calls).is_empty(),
        "nothing for one not offered or not the account's"
    );

    calls.on_delivery(&delivery(root, &invitation(B)), Observed::Offered, now);
    let offered = effects(&mut calls);
    assert!(matches!(
        offered.first(),
        Some(Effect::Respond {
            reply: DeliveryReply::Receipt(_),
            ..
        })
    ));
    let [(attach, "attach")] = sent(&offered)[..] else {
        panic!("{offered:#?}");
    };
    assert!(events(&offered).is_empty(), "shown only once attached");
    let (call, _) = calls.current().unwrap();
    assert!(matches!(calls.answer(call, media()), Err(CallError::NotNow)));

    let attached = json!({
        "callInvitation": {"links": {"progress": cc("incoming", "progress"),
            "acceptance": cc("incoming", "accept"), "callLeg": cc("incoming", "reject")}},
        "additionalActionResponses": [{"url": conv(""), "output": {"links": {
            "leave": conv("/leave"), "updateEndpointState": conv("/updateEndpointState")}}}],
    });
    calls.on_outcome(attach, &ok(&attached), now);
    let ringing = effects(&mut calls);
    assert_eq!(
        sent(&ringing).iter().map(|(_, name)| *name).collect::<Vec<_>>(),
        ["progress"]
    );
    let rung = events(&ringing);
    let [CallEvent::Incoming(incoming)] = rung.as_slice() else {
        panic!("{ringing:#?}");
    };
    assert_eq!((incoming.caller.as_str(), incoming.call), (A, call));

    // its redelivery gets a receipt again, nothing more
    calls.on_delivery(&delivery(root, &invitation(B)), Observed::Offered, now);
    let again = effects(&mut calls);
    assert!(sent(&again).is_empty());
    assert_eq!(again.len(), 1);

    calls.answer(call, media()).unwrap();
    let answering = effects(&mut calls);
    let [(state, "update_endpoint_state")] = sent(&answering)[..] else {
        panic!("{answering:#?}");
    };
    calls.on_outcome(state, &ok(&json!({})), now);
    assert_eq!(
        names(&effects(&mut calls)),
        ["accept"],
        "the accept once the state was taken"
    );
    assert!(matches!(calls.hang_up(call, now), Ok(())));
}

fn names(effects: &[Effect]) -> Vec<&'static str> {
    sent(effects).iter().map(|(_, name)| *name).collect()
}

fn failed() -> Outcome {
    Outcome::Answered {
        status: 500,
        body: Vec::new(),
    }
}

/// An invitation to `callee` from a in the group chat `chat`, its conversation `Conversation<n>`.
fn group_invitation(callee: &str, chat: &str, n: u8) -> Value {
    let gp = json!({
        "callNotification": {
            "from": {"id": A, "displayName": "Lab A"},
            "to": {"id": callee},
            "links": {"attach": cc("forked", "attach")},
        },
        "conversationInvitation": {"conversationController": conv("").replace("Conversation1", &format!("Conversation{n}"))},
        "groupChat": {"threadId": chat},
    });
    json!({"evt": 109, "gp": base64::engine::general_purpose::STANDARD.encode(gp.to_string())})
}

/// b's machine with a's invitation offered (its `attach` answered): it rings here.
fn offered_call(now: Instant) -> (Calls, CallId) {
    let mut calls = Calls::new(identity(B));
    calls.set_push_url(PUSH).unwrap();
    calls.on_delivery(
        &delivery("/v4/f/SyntheticEndpoint00000/", &invitation(B)),
        Observed::Offered,
        now,
    );
    let [(attach, "attach")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    let attached = json!({
        "callInvitation": {"links": {"progress": cc("incoming", "progress"),
            "acceptance": cc("incoming", "accept"), "callLeg": cc("incoming", "reject")}},
        "additionalActionResponses": [{"url": conv(""), "output": {"links": {
            "leave": conv("/leave"), "updateEndpointState": conv("/updateEndpointState")}}}],
    });
    calls.on_outcome(attach, &ok(&attached), now);
    let ringing = effects(&mut calls);
    let [CallEvent::Incoming(incoming)] = &events(&ringing)[..] else {
        panic!("{ringing:#?}");
    };
    (calls, incoming.call)
}

#[test]
fn a_failed_endpoint_state_ends_the_answer_and_its_accept_never_goes() {
    let now = Instant::now();
    let (mut calls, call) = offered_call(now);
    calls.answer(call, media()).unwrap();
    let answering = effects(&mut calls);
    let [(state, "update_endpoint_state")] = sent(&answering)[..] else {
        panic!("{answering:#?}");
    };
    calls.on_outcome(state, &failed(), now);
    let ended = effects(&mut calls);
    assert_eq!(names(&ended), ["leave"], "no accept after the call was over");
    assert_eq!(
        events(&ended).last(),
        Some(&CallEvent::Ended {
            call,
            end: End::Failed("update_endpoint_state")
        })
    );
}

#[test]
fn a_hang_up_before_the_accept_went_declines_and_never_leaves() {
    let now = Instant::now();
    // the endpoint state handed out, the accept held for its answer
    let (mut calls, call) = offered_call(now);
    calls.answer(call, media()).unwrap();
    let [(state, "update_endpoint_state")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.hang_up(call, now).unwrap();
    let hung_up = effects(&mut calls);
    let [(reject, "reject")] = sent(&hung_up)[..] else {
        panic!("{hung_up:#?}");
    };
    assert_eq!(
        events(&hung_up),
        [CallEvent::State {
            call,
            state: CallState::Declining
        }]
    );
    calls.on_outcome(state, &ok(&json!({})), now);
    assert!(effects(&mut calls).is_empty(), "the held accept never goes");
    calls.on_outcome(reject, &ok(&json!({})), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended {
            call,
            end: End::Declined
        }]
    );

    // the endpoint state not taken by the engine yet: it never goes, only the decline
    let (mut calls, call) = offered_call(now);
    calls.answer(call, media()).unwrap();
    calls.hang_up(call, now).unwrap();
    let hung_up = effects(&mut calls);
    let [(reject, "reject")] = sent(&hung_up)[..] else {
        panic!("{hung_up:#?}");
    };
    calls.on_outcome(reject, &ok(&json!({})), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended {
            call,
            end: End::Declined
        }]
    );

    // the endpoint state failing after the hang-up: the decline goes on and ends the call
    let (mut calls, call) = offered_call(now);
    calls.answer(call, media()).unwrap();
    let [(state, _)] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.hang_up(call, now).unwrap();
    let [(reject, "reject")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(state, &failed(), now);
    assert!(effects(&mut calls).is_empty(), "the decline goes on");
    calls.on_outcome(reject, &ok(&json!({})), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended {
            call,
            end: End::Declined
        }]
    );
}

#[test]
fn a_hang_up_once_the_accept_went_leaves() {
    let now = Instant::now();
    let (mut calls, call) = offered_call(now);
    calls.answer(call, media()).unwrap();
    let [(state, _)] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(state, &ok(&json!({})), now);
    let [(_, "accept")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.hang_up(call, now).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["leave"]);
}

#[test]
fn writes_still_queued_when_the_call_ends_never_go() {
    let now = Instant::now();
    let (mut calls, call) = ringing_call(now);
    let path = acceptance_path(&calls);
    calls.on_delivery(&delivery(&path, &acceptance()), Observed::Acceptance, now);
    // the engine hasn't taken the endpoint metadata yet when the call's end comes
    let end = calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Call, "end"))
        .unwrap()
        .to_owned();
    calls.on_delivery(&delivery(&end, &json!({"callEnd": {"code": 0}})), Observed::Ended, now);
    let all = effects(&mut calls);
    assert!(sent(&all).is_empty(), "{all:#?}");
    assert!(matches!(
        events(&all).last(),
        Some(CallEvent::Ended {
            end: End::Remote(_),
            ..
        })
    ));
    assert!(events(&all).contains(&CallEvent::State {
        call,
        state: CallState::Connected
    }));
}

/// a's machine with the test call placed, its start answered and its acceptance delivered: connected.
fn connected_test_call(now: Instant) -> (Calls, CallId, Vec<Effect>) {
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let echo = Placement::TestCall {
        echo: Mri::new("28:00000000-0000-4000-8000-0000000000ec"),
    };
    let call = calls.place(&echo, media(), now).unwrap();
    let [(start, "start")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(start, &ok(&conversation_answer()), now);
    let _ = effects(&mut calls);
    let path = acceptance_path(&calls);
    calls.on_delivery(&delivery(&path, &acceptance()), Observed::Acceptance, now);
    let accepted = effects(&mut calls);
    (calls, call, accepted)
}

#[test]
fn the_echo_is_added_only_once_the_endpoint_metadata_was_taken() {
    let now = Instant::now();
    let (mut calls, _, accepted) = connected_test_call(now);
    let [(metadata, "update_endpoint_metadata")] = sent(&accepted)[..] else {
        panic!("{accepted:#?}");
    };
    calls.on_outcome(metadata, &ok(&json!({})), now);
    assert_eq!(names(&effects(&mut calls)), ["add"]);

    let (mut calls, call, accepted) = connected_test_call(now);
    let [(metadata, _)] = sent(&accepted)[..] else { panic!() };
    calls.on_outcome(metadata, &Outcome::Failed, now);
    let ended = effects(&mut calls);
    assert_eq!(names(&ended), ["leave"], "no add to a call that is over");
    assert_eq!(
        events(&ended).last(),
        Some(&CallEvent::Ended {
            call,
            end: End::Failed("update_endpoint_metadata")
        })
    );
}

#[test]
fn an_acceptance_before_the_start_s_answer_is_applied_once_it_came() {
    let now = Instant::now();
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let placement = Placement::Chat {
        callees: vec![Mri::new(B)],
        group_chat: None,
    };
    let call = calls.place(&placement, media(), now).unwrap();
    let [(start, "start")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    let path = acceptance_path(&calls);
    calls.on_delivery(&delivery(&path, &acceptance()), Observed::Acceptance, now);
    assert!(effects(&mut calls).is_empty(), "held until the start's answer");

    calls.on_outcome(start, &ok(&conversation_answer()), now);
    let answered = effects(&mut calls);
    assert!(
        answered.iter().any(|effect| matches!(
            effect,
            Effect::Respond {
                reply: DeliveryReply::Acknowledgement(_),
                ..
            }
        )),
        "{answered:#?}"
    );
    let told = events(&answered);
    assert_eq!(
        told.first(),
        Some(&CallEvent::State {
            call,
            state: CallState::Ringing
        })
    );
    assert!(told.iter().any(|event| matches!(event, CallEvent::RemoteMedia { .. })));
    assert_eq!(
        told.last(),
        Some(&CallEvent::State {
            call,
            state: CallState::Connected
        })
    );
    assert_eq!(names(&answered), ["update_endpoint_metadata"]);
}

#[test]
fn a_decline_or_a_leave_that_wasn_t_taken_ends_the_call_failed() {
    let now = Instant::now();
    let (mut calls, call) = offered_call(now);
    calls.decline(call).unwrap();
    let [(reject, "reject")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(reject, &Outcome::Failed, now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended {
            call,
            end: End::Failed("reject")
        }]
    );

    let (mut calls, call) = offered_call(now);
    calls.decline(call).unwrap();
    let [(reject, _)] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(reject, &ok(&json!({})), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended {
            call,
            end: End::Declined
        }]
    );

    let (mut calls, call) = ringing_call(now);
    calls.hang_up(call, now).unwrap();
    let [(leave, "leave")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(leave, &failed(), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended {
            call,
            end: End::Failed("leave")
        }]
    );
}

const CHAT: &str = "19:00000000000000000000000000000001@thread.v2";

/// b's machine told of a's group call in [`CHAT`] (its ring ended): joinable.
fn told_of_a_group_call(now: Instant) -> Calls {
    let mut calls = Calls::new(identity(B));
    calls.set_push_url(PUSH).unwrap();
    calls.on_delivery(
        &delivery("/v4/f/SyntheticEndpoint00000/", &group_invitation(B, CHAT, 1)),
        Observed::Offered,
        now,
    );
    let [(attach, "attach")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(attach, &failed(), now);
    let _ = effects(&mut calls);
    assert_eq!(calls.current(), None);
    assert!(calls.joinable(CHAT));
    calls
}

#[test]
fn a_running_call_is_forgotten_once_a_join_to_it_fails_or_its_conversation_ended() {
    let now = Instant::now();
    let mut calls = told_of_a_group_call(now);
    let call = calls.join(CHAT, media(), now).unwrap();
    let [(join, "join")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(join, &failed(), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended {
            call,
            end: End::Failed("join")
        }]
    );
    assert!(!calls.joinable(CHAT), "a failed join forgets it");
    assert!(matches!(calls.join(CHAT, media(), now), Err(CallError::NotJoinable)));

    let mut calls = told_of_a_group_call(now);
    calls.join(CHAT, media(), now).unwrap();
    let [(join, _)] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(join, &ok(&conversation_answer()), now);
    let _ = effects(&mut calls);
    let conversation_end = calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Conversation, "conversationEnd"))
        .unwrap()
        .to_owned();
    calls.on_delivery(&delivery(&conversation_end, &json!({})), Observed::Ended, now);
    let _ = effects(&mut calls);
    assert_eq!(calls.current(), None);
    assert!(!calls.joinable(CHAT), "its conversation's end forgets it");

    // a newer invitation in the chat isn't forgotten by the older call's end
    let mut calls = told_of_a_group_call(now);
    calls.join(CHAT, media(), now).unwrap();
    let [(join, _)] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(join, &ok(&conversation_answer()), now);
    calls.on_delivery(
        &delivery("/v4/f/SyntheticEndpoint00000/", &group_invitation(B, CHAT, 2)),
        Observed::Offered,
        now,
    );
    let _ = effects(&mut calls);
    let conversation_end = calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Conversation, "conversationEnd"))
        .unwrap()
        .to_owned();
    calls.on_delivery(&delivery(&conversation_end, &json!({})), Observed::Ended, now);
    let _ = effects(&mut calls);
    assert!(calls.joinable(CHAT), "the newer call stays joinable");
}
