//! Synthetic signaling orderings: the reject response and its end callback travel independently.

use super::*;

fn decline_end() -> Value {
    json!({"callEnd": {
        "code": 603,
        "subCode": 0,
        "phrase": "CallEndReasonLocalUserInitiated",
        "resultCategories": ["Success"],
        "sender": {"id": B, "displayName": "Synthetic callee"}
    }})
}

fn begin_decline(now: Instant) -> (Calls, CallId, WriteId, String) {
    let (mut calls, call) = offered_call(now);
    let end = callback(&calls, "end");
    calls.decline(call).unwrap();
    let declining = effects(&mut calls);
    let [(reject, "reject")] = sent(&declining)[..] else {
        panic!("{declining:#?}");
    };
    assert_eq!(
        events(&declining),
        [CallEvent::State {
            call,
            state: CallState::Declining
        }]
    );
    (calls, call, reject, end)
}

#[test]
fn a_decline_has_one_successful_end_in_either_callback_response_order() {
    for callback_first in [true, false] {
        let now = Instant::now();
        let (mut calls, call, reject, end) = begin_decline(now);
        let callback = delivery(&end, &decline_end());
        if callback_first {
            calls.on_delivery(&callback, Observed::Ended, now);
            let early = effects(&mut calls);
            assert!(events(&early).is_empty(), "the reject is still unanswered: {early:#?}");
            assert_eq!(calls.current(), Some((call, Some(CallState::Declining))));
        }
        calls.on_outcome(reject, &ok(&json!({})), now);
        let ended = effects(&mut calls);
        assert_eq!(
            events(&ended),
            [CallEvent::Ended {
                call,
                end: End::Declined
            }]
        );
        assert!(sent(&ended).is_empty(), "no additional write or retry");
        assert_eq!(calls.current(), None);
        assert!(!calls.may_send_again(reject));

        // A late callback, redelivery, or repeated outcome cannot end the call twice.
        calls.on_delivery(&callback, Observed::Ended, now);
        calls.on_delivery(&callback, Observed::Ended, now);
        calls.on_outcome(reject, &ok(&json!({})), now);
        assert!(effects(&mut calls).is_empty());
    }
}

fn assert_failed_decline(body: &Value, outcome: &Outcome, callback_first: bool) {
    let now = Instant::now();
    let (mut calls, call, reject, end) = begin_decline(now);
    let callback = delivery(&end, body);
    if callback_first {
        calls.on_delivery(&callback, Observed::Ended, now);
        let early = effects(&mut calls);
        assert!(events(&early).is_empty(), "the reject is still unanswered: {early:#?}");
    }
    calls.on_outcome(reject, outcome, now);
    let ended = effects(&mut calls);
    assert_eq!(
        events(&ended),
        [CallEvent::Ended {
            call,
            end: End::Failed("reject")
        }]
    );
    assert!(sent(&ended).is_empty(), "a failed reject is never resent");
    assert_eq!(calls.current(), None);
    assert!(!calls.may_send_again(reject));
    calls.on_delivery(&callback, Observed::Ended, now);
    assert!(effects(&mut calls).is_empty());
}

#[test]
fn a_terminal_callback_cannot_make_a_failed_decline_succeed() {
    let unrelated = json!({"callEnd": {
        "code": 500,
        "phrase": "Synthetic controller failure",
        "resultCategories": ["MediaNegotiationFailure"],
        "sender": null
    }});
    for body in [decline_end(), unrelated] {
        for (outcome, callback_first) in [
            (failed(), true),
            (failed(), false),
            (Outcome::Failed, true),
            (Outcome::Failed, false),
        ] {
            assert_failed_decline(&body, &outcome, callback_first);
        }
    }
}

#[test]
fn the_same_end_without_a_local_decline_remains_a_remote_ring_end() {
    let now = Instant::now();
    let (mut calls, call) = offered_call(now);
    let end = callback(&calls, "end");
    let body = decline_end();
    calls.on_delivery(&delivery(&end, &body), Observed::Ended, now);
    let ended = effects(&mut calls);
    assert_eq!(
        events(&ended),
        [CallEvent::RingEnded {
            call,
            end: crate::read::remote_end(&body.to_string().into_bytes())
        }]
    );
    assert!(sent(&ended).is_empty());
}

#[test]
fn a_decline_waiting_for_its_response_still_receipts_other_typed_deliveries() {
    let now = Instant::now();
    let (mut calls, call) = offered_call(now);
    calls.answer(call, media()).unwrap();
    let _ = effects(&mut calls);
    let renegotiation = callback(&calls, "mediaRenegotiation");
    // The accept is held for the endpoint-state response: hanging up must decline instead.
    calls.hang_up(call, now).unwrap();
    let [(reject, "reject")] = sent(&effects(&mut calls))[..] else {
        panic!();
    };
    let end = callback(&calls, "end");
    calls.on_delivery(&delivery(&end, &decline_end()), Observed::Ended, now);
    assert!(effects(&mut calls).is_empty());

    let incoming = delivery(&renegotiation, &negotiation());
    calls.on_delivery(&incoming, Observed::Renegotiation, now);
    let acknowledged = effects(&mut calls);
    assert_eq!(receipts(&acknowledged), [DeliveryKey::of(&incoming)]);
    assert!(events(&acknowledged).is_empty());
    assert!(sent(&acknowledged).is_empty());
    assert_eq!(calls.current(), Some((call, Some(CallState::Declining))));
    calls.on_outcome(reject, &ok(&json!({})), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended {
            call,
            end: End::Declined
        }]
    );
}
