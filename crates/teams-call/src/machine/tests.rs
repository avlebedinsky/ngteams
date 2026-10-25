//! The machine on its own, fed by hand: what each delivery, outcome and action makes it do (the whole flows on the
//! mock are `tests/mock.rs`).

use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};
use teams_fence::{Delivery, Mri, Observed};
use teams_proto::{ClientInfo, MediaContent};

use super::{CallError, CallEvent, CallId, CallState, Calls, DeliveryReply, Effect, End, Identity, Outcome, Placement};
use crate::CallWrite;
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

/// The keep-alive's period: 90 % of the acceptance's `callKeepAliveInterval` (2700 s), as the web client sends it.
const PERIOD: Duration = Duration::from_secs(2_430);

/// A placed call up by an acceptance whose links name the call leg's own (the keep-alive's).
fn up_with_a_call_leg(now: Instant) -> (Calls, CallId) {
    let (mut calls, call) = ringing_call(now);
    assert_eq!(calls.deadline(), None);
    let path = acceptance_path(&calls);
    let mut accepting = acceptance();
    accepting["callAcceptance"]["links"]["callLeg"] = json!(call_leg());
    calls.on_delivery(&delivery(&path, &accepting), Observed::Acceptance, now);
    let _ = effects(&mut calls);
    (calls, call)
}

#[test]
fn the_keep_alive_goes_to_the_call_leg_every_90_percent_of_its_interval() {
    let now = Instant::now();
    let (mut calls, call) = up_with_a_call_leg(now);
    assert_eq!(calls.keep_alive_interval(), Some(Duration::from_mins(45)));
    assert_eq!(calls.deadline(), Some(now + PERIOD));
    calls.tick(now + Duration::from_secs(2_429));
    assert!(effects(&mut calls).is_empty());
    calls.tick(now + PERIOD);
    let due = effects(&mut calls);
    assert_eq!(events(&due), [CallEvent::KeepAliveDue { call, sent: true }]);
    let first = sent(&due);
    assert_eq!(first.iter().map(|(_, name)| *name).collect::<Vec<_>>(), ["keep_alive"]);
    let Some(Effect::Send { write, .. }) = due.iter().find(|effect| matches!(effect, Effect::Send { .. })) else {
        panic!("{due:#?}");
    };
    assert!(matches!(**write, CallWrite::KeepAlive(_)), "{write:?}");
    // its failing ends nothing; the next one falls due a period later, again and again
    calls.on_outcome(first[0].0, &Outcome::Failed, now + PERIOD);
    assert!(events(&effects(&mut calls)).is_empty());
    assert_eq!(calls.current().map(|(id, _)| id), Some(call));
    assert_eq!(calls.deadline(), Some(now + PERIOD * 2));
    calls.tick(now + PERIOD * 2);
    let again = effects(&mut calls);
    assert_eq!(events(&again), [CallEvent::KeepAliveDue { call, sent: true }]);
    calls.on_outcome(sent(&again)[0].0, &ok(&json!({})), now + PERIOD * 2);
    calls.tick(now + PERIOD * 3);
    assert_eq!(sent(&effects(&mut calls)).len(), 1, "a third one");
}

#[test]
fn no_keep_alive_falls_due_once_the_call_ended() {
    let now = Instant::now();
    let (mut calls, _) = up_with_a_call_leg(now);
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
    for period in 1..=3 {
        calls.tick(now + PERIOD * period);
        assert!(effects(&mut calls).is_empty(), "none after the call ended");
    }
}

#[test]
fn a_call_without_a_call_leg_link_tells_its_keep_alive_due_and_sends_nothing() {
    let now = Instant::now();
    let (mut calls, call) = ringing_call(now);
    let path = acceptance_path(&calls);
    calls.on_delivery(&delivery(&path, &acceptance()), Observed::Acceptance, now);
    let _ = effects(&mut calls);
    calls.tick(now + PERIOD);
    let due = effects(&mut calls);
    assert_eq!(events(&due), [CallEvent::KeepAliveDue { call, sent: false }]);
    assert!(sent(&due).is_empty());
    assert_eq!(calls.deadline(), Some(now + PERIOD * 2));
}

/// A call leg's own link, the keep-alive's.
fn call_leg() -> String {
    format!("{PROXY}/cc-zz-01-prod-aks.cc.skype.com/cc/v1/active/Call1/1/a1/3?i=10-0-0-1&e=1")
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

/// a's machine with a 1:1 call to b placed, its start handed out and unanswered.
fn placing_call(now: Instant) -> (Calls, CallId, WriteId) {
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
    (calls, call, start)
}

#[test]
fn a_media_failure_before_the_start_was_answered_leaves_once_it_is() {
    let now = Instant::now();
    let (mut calls, call, start) = placing_call(now);
    calls.fail_media(call, now);
    let failed = effects(&mut calls);
    assert!(sent(&failed).is_empty(), "no leave link yet");
    assert_eq!(
        events(&failed),
        [CallEvent::State {
            call,
            state: CallState::Leaving
        }]
    );
    assert_eq!(
        calls.current(),
        Some((call, Some(CallState::Leaving))),
        "kept for the start's answer"
    );
    assert!(!calls.may_send_again(start), "the call leaves");
    calls.fail_media(call, now);
    assert!(effects(&mut calls).is_empty(), "leaving already");

    // the service took the start: the call it opened is left
    calls.on_outcome(start, &ok(&conversation_answer()), now);
    let answered = effects(&mut calls);
    let [(leave, "leave")] = sent(&answered)[..] else {
        panic!("{answered:#?}");
    };
    assert!(
        !events(&answered).iter().any(|event| matches!(
            event,
            CallEvent::State {
                state: CallState::Ringing,
                ..
            }
        )),
        "never told ringing: its devices never open"
    );
    calls.on_outcome(leave, &ok(&json!({})), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended {
            call,
            end: End::Failed("media")
        }]
    );
    assert_eq!(calls.current(), None);
}

#[test]
fn a_start_that_fails_after_the_call_began_leaving_ends_it_for_what_it_left_for() {
    let now = Instant::now();
    let refused = Outcome::Failed;
    let (mut calls, call, start) = placing_call(now);
    calls.fail_media(call, now);
    let _ = effects(&mut calls);
    calls.on_outcome(start, &refused, now);
    let ended = effects(&mut calls);
    assert!(sent(&ended).is_empty(), "nothing to leave");
    assert_eq!(
        events(&ended),
        [CallEvent::Ended {
            call,
            end: End::Failed("media")
        }]
    );

    let (mut calls, call, start) = placing_call(now);
    calls.hang_up(call, now).unwrap();
    let _ = effects(&mut calls);
    calls.on_outcome(start, &refused, now);
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
fn the_call_s_end_delivered_before_its_leave_s_answer_ends_it_as_hung_up() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    let _ = effects(&mut calls);
    calls.hang_up(call, now).unwrap();
    let leaving = effects(&mut calls);
    assert_eq!(names(&leaving), ["leave"]);
    let end = callback(&calls, "end");
    let body = json!({"callEnd": {"code": 0, "phrase": "CallEndReasonLocalUserInitiated", "reason": "noError"}});
    calls.on_delivery(&delivery(&end, &body), Observed::Ended, now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended { call, end: End::HungUp }]
    );

    // a call nobody is leaving that ends that way ended at the far end
    let (mut calls, call) = connected_call(now);
    let end = callback(&calls, "end");
    calls.on_delivery(&delivery(&end, &body), Observed::Ended, now);
    assert!(matches!(
        &events(&effects(&mut calls))[..],
        [.., CallEvent::Ended { call: of, end: End::Remote(_) }] if *of == call
    ));
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

/// a's 1:1 call to b, placed and accepted (its acceptance naming the call controller's `mediaRenegotiation` link):
/// connected, its effects so far taken.
fn connected_call(now: Instant) -> (Calls, CallId) {
    let (mut calls, call) = ringing_call(now);
    let path = acceptance_path(&calls);
    let mut accepted = acceptance();
    accepted["callAcceptance"]["links"]["mediaRenegotiation"] = json!(cc("active", "renegotiate"));
    calls.on_delivery(&delivery(&path, &accepted), Observed::Acceptance, now);
    let connected = effects(&mut calls);
    assert!(events(&connected).contains(&CallEvent::State {
        call,
        state: CallState::Connected
    }));
    (calls, call)
}

/// The path of the call's callback `name` (`call/<name>/`), the first made.
fn callback(calls: &Calls, name: &str) -> String {
    calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Call, name))
        .unwrap()
        .to_owned()
}

#[test]
fn the_account_s_renegotiation_goes_to_the_controller_s_link_and_its_answer_is_told_and_acknowledged() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    calls.renegotiate(call, &media()).unwrap();
    let asked = effects(&mut calls);
    let [(renegotiation, "renegotiate")] = sent(&asked)[..] else {
        panic!("{asked:#?}");
    };
    assert!(
        matches!(calls.renegotiate(call, &media()), Err(CallError::NotNow)),
        "one at a time"
    );
    calls.on_outcome(renegotiation, &ok(&json!({})), now);
    assert!(effects(&mut calls).is_empty());

    let answer = json!({"mediaAnswer": {
        "mediaContent": {"blob": "v=1\r\n", "contentType": "application/sdp-ngc-1.0"},
        "links": {"mediaAcknowledgement": cc("mediaAnswers", "acknowledge")},
    }});
    let at = latest_callback(&calls, "mediaAnswer");
    calls.on_delivery(&delivery(&at, &answer), Observed::Renegotiation, now);
    let answered = effects(&mut calls);
    assert_eq!(names(&answered), ["media_acknowledge"]);
    let told = events(&answered);
    let [
        CallEvent::RemoteMedia {
            call: of,
            media: told_media,
        },
    ] = told.as_slice()
    else {
        panic!("{answered:#?}");
    };
    assert_eq!((*of, told_media.blob.as_str()), (call, "v=1\r\n"));
    calls.on_delivery(&delivery(&at, &answer), Observed::Renegotiation, now);
    assert!(effects(&mut calls).is_empty(), "no renegotiation waits for it");
    calls.renegotiate(call, &media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["renegotiate"], "and another may go");
}

#[test]
fn a_renegotiation_begins_before_its_offer_and_takes_no_other_meanwhile() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    calls.begin_renegotiation(call).unwrap();
    assert!(
        matches!(calls.begin_renegotiation(call), Err(CallError::NotNow)),
        "one at a time, before its offer is made"
    );
    calls.give_up_renegotiation(call);
    calls.begin_renegotiation(call).unwrap();
    calls.renegotiate(call, &media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["renegotiate"]);
    assert!(
        matches!(calls.begin_renegotiation(call), Err(CallError::NotNow)),
        "nor while its answer waits"
    );

    // the far end's renegotiation while the account's is being made fails the call
    let (mut calls, call) = connected_call(now);
    calls.begin_renegotiation(call).unwrap();
    let at = callback(&calls, "mediaRenegotiation");
    calls.on_delivery(&delivery(&at, &negotiation()), Observed::Renegotiation, now);
    let failed = effects(&mut calls);
    assert_eq!(names(&failed), ["leave"]);
    assert!(events(&failed).contains(&CallEvent::Ended {
        call,
        end: End::Failed("media_answer")
    }));
}

#[test]
fn the_account_s_renegotiation_waits_while_the_far_end_s_waits_for_its_answer() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    let at = callback(&calls, "mediaRenegotiation");
    calls.on_delivery(&delivery(&at, &negotiation()), Observed::Renegotiation, now);
    let _ = effects(&mut calls);
    assert!(matches!(calls.begin_renegotiation(call), Err(CallError::NotNow)));
    assert!(matches!(calls.renegotiate(call, &media()), Err(CallError::NotNow)));
    // the far end's is answered: the call goes on, and the account's may go now
    calls.answer_media(call, media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["media_answer"]);
    calls.begin_renegotiation(call).unwrap();
    calls.renegotiate(call, &media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["renegotiate"]);
}

#[test]
fn a_rejected_renegotiation_or_one_whose_answer_doesn_t_read_fails_the_call() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    calls.renegotiate(call, &media()).unwrap();
    let _ = effects(&mut calls);
    let rejection = latest_callback(&calls, "rejection");
    calls.on_delivery(&delivery(&rejection, &json!({})), Observed::Nothing, now);
    let failed = effects(&mut calls);
    assert_eq!(names(&failed), ["leave"]);
    assert!(events(&failed).contains(&CallEvent::Ended {
        call,
        end: End::Failed("renegotiate")
    }));

    let (mut calls, call) = connected_call(now);
    calls.renegotiate(call, &media()).unwrap();
    let _ = effects(&mut calls);
    let at = latest_callback(&calls, "mediaAnswer");
    calls.on_delivery(
        &delivery(&at, &json!({"mediaAnswer": {}})),
        Observed::Renegotiation,
        now,
    );
    let failed = effects(&mut calls);
    assert_eq!(names(&failed), ["leave"]);
    assert!(events(&failed).contains(&CallEvent::Ended {
        call,
        end: End::Failed("renegotiate")
    }));
}

/// A renegotiation of the call delivered to the account, its answer going to `Call1`'s negotiation link.
fn negotiation() -> Value {
    json!({"mediaNegotiation": {
        "mediaContent": {"blob": "v=2\r\n", "contentType": "application/sdp-ngc-1.0"},
        "links": {"mediaAnswer": cc("negotiations", "answer"), "rejection": cc("negotiations", "reject")},
    }})
}

#[test]
fn a_renegotiation_delivered_is_told_and_answered_only_by_the_account() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    assert!(matches!(calls.answer_media(call, media()), Err(CallError::NotNow)));
    let at = callback(&calls, "mediaRenegotiation");
    calls.on_delivery(&delivery(&at, &negotiation()), Observed::Renegotiation, now);
    let offered = effects(&mut calls);
    assert!(sent(&offered).is_empty(), "nothing goes until the account answers");
    let told = events(&offered);
    let [CallEvent::MediaOffer { call: of, offer }] = told.as_slice() else {
        panic!("{offered:#?}");
    };
    assert_eq!((*of, offer.blob.as_str()), (call, "v=2\r\n"));
    calls.answer_media(call, media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["media_answer"]);
    assert!(
        matches!(calls.answer_media(call, media()), Err(CallError::NotNow)),
        "answered once"
    );

    // one the fence bound nothing of (no link its answer may go to) fails the call
    calls.on_delivery(&delivery(&at, &negotiation()), Observed::Nothing, now);
    let failed = effects(&mut calls);
    assert_eq!(names(&failed), ["leave"]);
    assert!(events(&failed).contains(&CallEvent::Ended {
        call,
        end: End::Failed("media_answer")
    }));
}

/// The path of the call's callback `name` made last: the one the request just built handed out.
fn latest_callback(calls: &Calls, name: &str) -> String {
    calls
        .call
        .as_ref()
        .and_then(|call| call.latest_path_of(Kind::Call, name))
        .unwrap()
        .to_owned()
}

/// The answer to the account's renegotiation, `blob` its SDP.
fn media_answer(blob: &str) -> Value {
    json!({"mediaAnswer": {
        "mediaContent": {"blob": blob, "contentType": "application/sdp-ngc-1.0"},
        "links": {"mediaAcknowledgement": cc("mediaAnswers", "acknowledge")},
    }})
}

/// The account renegotiates `call` and its renegotiation is taken: the paths of the `mediaAnswer` and `rejection`
/// callbacks this renegotiation's request handed out.
fn renegotiated(calls: &mut Calls, call: CallId, now: Instant) -> (String, String) {
    calls.renegotiate(call, &media()).unwrap();
    let asked = effects(calls);
    let [(renegotiation, "renegotiate")] = sent(&asked)[..] else {
        panic!("{asked:#?}");
    };
    let paths = (
        latest_callback(calls, "mediaAnswer"),
        latest_callback(calls, "rejection"),
    );
    calls.on_outcome(renegotiation, &ok(&json!({})), now);
    assert!(effects(calls).is_empty());
    paths
}

#[test]
fn an_answer_or_a_rejection_counts_only_at_the_waiting_renegotiation_s_own_callbacks() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    let (first_answer, first_rejection) = renegotiated(&mut calls, call, now);
    calls.on_delivery(
        &delivery(&first_answer, &media_answer("v=1\r\n")),
        Observed::Renegotiation,
        now,
    );
    assert_eq!(names(&effects(&mut calls)), ["media_acknowledge"]);

    let (second_answer, second_rejection) = renegotiated(&mut calls, call, now);
    assert_ne!(
        (&first_answer, &first_rejection),
        (&second_answer, &second_rejection),
        "each renegotiation hands out callbacks of its own"
    );
    // the first one's answer again, and its rejection: the second still waits, nothing is told, sent or failed
    calls.on_delivery(
        &delivery(&first_answer, &media_answer("v=1\r\n")),
        Observed::Renegotiation,
        now,
    );
    assert!(effects(&mut calls).is_empty(), "an earlier renegotiation's answer");
    calls.on_delivery(&delivery(&first_rejection, &json!({})), Observed::Nothing, now);
    assert!(effects(&mut calls).is_empty(), "an earlier renegotiation's rejection");
    assert_eq!(calls.current(), Some((call, Some(CallState::Connected))));
    assert!(
        matches!(calls.renegotiate(call, &media()), Err(CallError::NotNow)),
        "the second still waits for its answer"
    );

    // the second's own answer: told and acknowledged, once
    calls.on_delivery(
        &delivery(&second_answer, &media_answer("v=3\r\n")),
        Observed::Renegotiation,
        now,
    );
    let answered = effects(&mut calls);
    assert_eq!(names(&answered), ["media_acknowledge"]);
    let told = events(&answered);
    let [CallEvent::RemoteMedia { call: of, media: told }] = told.as_slice() else {
        panic!("{answered:#?}");
    };
    assert_eq!((*of, told.blob.as_str()), (call, "v=3\r\n"));
    calls.on_delivery(
        &delivery(&second_answer, &media_answer("v=3\r\n")),
        Observed::Renegotiation,
        now,
    );
    assert!(effects(&mut calls).is_empty(), "its answer again");
    calls.on_delivery(&delivery(&second_rejection, &json!({})), Observed::Nothing, now);
    assert!(effects(&mut calls).is_empty(), "its rejection once it was answered");
    calls.renegotiate(call, &media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["renegotiate"], "and another may go");
}

/// A renegotiation of the call delivered to the account: the offer `blob`, its answer going to the link `answer`.
fn negotiation_of(blob: &str, answer: &str) -> Value {
    json!({"mediaNegotiation": {
        "mediaContent": {"blob": blob, "contentType": "application/sdp-ngc-1.0"},
        "links": {"mediaAnswer": answer, "rejection": cc("negotiations", "reject")},
    }})
}

/// The offers `effects` tell, by their SDP.
fn offers(effects: &[Effect]) -> Vec<String> {
    events(effects)
        .into_iter()
        .filter_map(|event| match event {
            CallEvent::MediaOffer { offer, .. } => Some(offer.blob),
            _ => None,
        })
        .collect()
}

#[test]
fn a_renegotiation_delivered_again_as_it_was_is_a_replay_and_changes_nothing() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    let at = latest_callback(&calls, "mediaRenegotiation");
    let first = negotiation_of("v=2\r\n", &cc("negotiations", "answer"));
    calls.on_delivery(&delivery(&at, &first), Observed::Renegotiation, now);
    assert_eq!(offers(&effects(&mut calls)), ["v=2\r\n"]);

    calls.on_delivery(&delivery(&at, &first), Observed::Renegotiation, now);
    assert!(effects(&mut calls).is_empty(), "again while it waits for the answer");
    calls.answer_media(call, media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["media_answer"], "still answered once");
    calls.on_delivery(&delivery(&at, &first), Observed::Renegotiation, now);
    assert!(effects(&mut calls).is_empty(), "again once it was answered");
    assert!(
        matches!(calls.answer_media(call, media()), Err(CallError::NotNow)),
        "nothing waits for an answer"
    );

    let other = format!("{PROXY}/cc-zz-01-prod-aks.cc.skype.com/cc/v1/negotiations/Call1/1/x1/2/answer?i=10-0-0-1");
    let second = negotiation_of("v=4\r\n", &other);
    calls.on_delivery(&delivery(&at, &second), Observed::Renegotiation, now);
    assert_eq!(offers(&effects(&mut calls)), ["v=4\r\n"], "a new one is told");
    calls.on_delivery(&delivery(&at, &first), Observed::Renegotiation, now);
    assert!(
        effects(&mut calls).is_empty(),
        "the answered one again while another waits"
    );
    calls.on_delivery(&delivery(&at, &second), Observed::Renegotiation, now);
    assert!(effects(&mut calls).is_empty(), "the waiting one again");
    assert_eq!(calls.current(), Some((call, Some(CallState::Connected))));
    calls.answer_media(call, media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["media_answer"]);

    // a different offer on a link seen before is a new renegotiation
    let third = negotiation_of("v=5\r\n", &cc("negotiations", "answer"));
    calls.on_delivery(&delivery(&at, &third), Observed::Renegotiation, now);
    assert_eq!(offers(&effects(&mut calls)), ["v=5\r\n"]);
    calls.answer_media(call, media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["media_answer"]);
}

#[test]
fn a_media_failure_fails_the_call_with_a_leave_once() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    calls.fail_media(CallId(99), now);
    assert!(effects(&mut calls).is_empty(), "another call's");
    calls.fail_media(call, now);
    let failed = effects(&mut calls);
    assert_eq!(names(&failed), ["leave"]);
    assert_eq!(
        events(&failed).last(),
        Some(&CallEvent::Ended {
            call,
            end: End::Failed("media")
        })
    );
    calls.fail_media(call, now);
    assert!(effects(&mut calls).is_empty(), "it ended already");

    // an invitation that rings here goes away unanswered
    let (mut calls, call) = offered_call(now);
    calls.fail_media(call, now);
    let gone = effects(&mut calls);
    assert!(sent(&gone).is_empty());
    assert!(matches!(&events(&gone)[..], [CallEvent::RingEnded { call: of, .. }] if *of == call));
}

#[test]
fn beside_a_write_in_flight_the_engine_takes_everything_but_the_next_write() {
    let now = Instant::now();
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let placement = Placement::Chat {
        callees: vec![Mri::new(B)],
        group_chat: None,
    };
    let call = calls.place(&placement, media(), now).unwrap();
    // the start, then its state: the state comes out first, the start stays queued
    assert!(matches!(
        calls.poll_beside_write(),
        Some(Effect::Event(CallEvent::State {
            state: CallState::Placing,
            ..
        }))
    ));
    assert!(calls.poll_beside_write().is_none());
    let Some(Effect::Send { id: start, .. }) = calls.poll() else {
        panic!("the start");
    };
    // a hang-up before the start's answer: the start isn't sent again, the cancel that follows may be
    calls.hang_up(call, now).unwrap();
    let _ = effects(&mut calls);
    assert!(!calls.may_send_again(start), "the call leaves");
    calls.on_outcome(start, &ok(&conversation_answer()), now);
    let [(leave, "leave")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    assert!(calls.may_send_again(leave));
    assert!(!calls.may_send_again(WriteId(99)), "not one of the machine's");
}

#[test]
fn a_cleanup_write_of_a_call_that_ended_may_go_again_until_its_outcome_came() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    let metadata = calls
        .call
        .as_ref()
        .and_then(|current| current.writes.keys().next().copied())
        .unwrap();
    assert!(calls.may_send_again(metadata), "the call wants it");
    calls.fail_media(call, now);
    let [(leave, "leave")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    assert_eq!(calls.current(), None, "the call ended here at once");
    assert!(!calls.may_send_again(metadata));
    assert!(calls.may_send_again(leave), "its leave is still the engine's to finish");
    calls.on_outcome(leave, &ok(&json!({})), now);
    assert!(!calls.may_send_again(leave));
}

#[test]
fn a_shut_down_lets_an_invitation_go_unanswered_and_hangs_up_a_call() {
    let now = Instant::now();
    let (mut calls, call) = offered_call(now);
    calls.shut_down(now);
    let gone = effects(&mut calls);
    assert!(sent(&gone).is_empty(), "no decline: nothing is sent for it");
    assert!(matches!(&events(&gone)[..], [CallEvent::RingEnded { call: of, .. }] if *of == call));
    assert_eq!(calls.current(), None);

    let (mut calls, _) = connected_call(now);
    calls.shut_down(now);
    assert_eq!(names(&effects(&mut calls)), ["leave"]);
    calls.shut_down(now);
    assert!(effects(&mut calls).is_empty(), "leaving already");
}
