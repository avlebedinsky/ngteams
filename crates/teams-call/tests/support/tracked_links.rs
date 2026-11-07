//! The links a live call's services hand out after its acceptance, bound by the tracker only as they were handed out:
//! a hold's `resume` (its answer's), a far end's transfer's `transferAcceptance` (delivered at the call's `transfer`
//! callback); and the call to the transfer's target then placed, on synthetic deliveries.

use super::*;

/// `link` (one of [`cc`]'s) on the call controller's call `Call2`, not the account's.
fn of_another_call(link: &str) -> String {
    link.replace("/Call1/", "/Call2/")
}

/// The writes `calls` hands out, each by name with its link and body, none checked against the tracker.
fn unchecked(calls: &mut Calls) -> Vec<(&'static str, url::Url, Value)> {
    struct Plain(Vec<(url::Url, Value)>);
    impl crate::WriteSink for Plain {
        fn submit<R: Request + Clone + Send + 'static>(&mut self, _: WriteId, write: R) {
            self.0
                .push((write.call_url().unwrap().clone(), write.body().unwrap_or(Value::Null)));
        }
    }
    let mut writes = Vec::new();
    for effect in effects(calls) {
        if let Effect::Send { id, write } = effect {
            let name = write.name();
            let mut plain = Plain(Vec::new());
            write.submit_to(id, &mut plain);
            writes.extend(plain.0.into_iter().map(|(url, body)| (name, url, body)));
        }
    }
    writes
}

fn hold_answer(resume: &str) -> Value {
    json!({"holdResponse": {"links": {"resume": resume}}})
}

#[tokio::test]
async fn a_hold_s_answer_lets_its_resume_link_through_and_one_naming_another_call_isn_t() {
    let mut setup = Connected::with_links(&[("hold", cc("active", "hold"))]).await;
    setup.calls.hold(setup.call, true).unwrap();
    let first = drain(&mut setup.calls, &setup.tracker);
    let [hold] = first.sent.as_slice() else {
        panic!("one hold")
    };
    assert_eq!(hold.name, "hold");
    complete(
        &mut setup.calls,
        &setup.tracker,
        hold,
        &hold_answer(&cc("holdSession", "resume")),
        setup.now,
    );
    assert!(
        drain(&mut setup.calls, &setup.tracker)
            .events
            .contains(&CallEvent::Held {
                call: setup.call,
                held: true
            })
    );
    // the resume goes through the capture's check: the tracker let its link through
    setup.calls.hold(setup.call, false).unwrap();
    let resumed = drain(&mut setup.calls, &setup.tracker);
    let [resume] = resumed.sent.as_slice() else {
        panic!("one resume")
    };
    assert_eq!(resume.name, "resume");
    complete(&mut setup.calls, &setup.tracker, resume, &json!({}), setup.now);
    drop(drain(&mut setup.calls, &setup.tracker));
    // held again, its answer naming a resume link on another call: the machine takes it, the tracker doesn't
    setup.calls.hold(setup.call, true).unwrap();
    let again = drain(&mut setup.calls, &setup.tracker);
    let [hold] = again.sent.as_slice() else {
        panic!("one hold")
    };
    let elsewhere = of_another_call(&cc("holdSession", "resume"));
    complete(
        &mut setup.calls,
        &setup.tracker,
        hold,
        &hold_answer(&elsewhere),
        setup.now,
    );
    drop(drain(&mut setup.calls, &setup.tracker));
    setup.calls.hold(setup.call, false).unwrap();
    let [("resume", url, body)] = &unchecked(&mut setup.calls)[..] else {
        panic!("one resume")
    };
    assert_eq!(url.as_str(), elsewhere);
    assert!(!setup.tracker.call_allowed(url, Some(body)));
}

/// A transfer the far end asked for, to c, taken at `acceptance`.
fn transfer_to_c(acceptance: &str) -> Value {
    json!({"callTransfer": {"target": {"id": C}, "isConsultative": false,
        "transferor": {"details": {"id": B, "displayName": "Lab B"}, "authorizationToken": "synthetic-authorization"},
        "links": {"transferAcceptance": acceptance, "transferCompletion": cc("transfer", "completion")}}})
}

/// `body` delivered as `id` at the call's `transfer` callback, which the acceptance's acknowledgement handed out.
fn at_transfer(setup: &Connected, id: i64, body: &Value) -> Delivery {
    let path = setup
        .calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Call, "transfer"))
        .unwrap();
    let mut asked = delivery(path, body);
    asked.id = id;
    asked.trouter_request = Some(format!("request-{id}"));
    asked
}

const C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";

#[tokio::test]
async fn a_transfer_delivered_binds_its_acceptance_link_the_transfer_is_taken_there_and_its_target_called() {
    let mut setup = Connected::new().await;
    let asked = at_transfer(&setup, 20, &transfer_to_c(&cc("transfer", "acceptance")));
    // the capture's check passes only for a link the tracker bound
    let accepting = setup.deliver(&asked, Observed::Nothing);
    let [taken] = accepting.sent.as_slice() else {
        panic!("one transfer acceptance")
    };
    assert_eq!(taken.name, "transfer_acceptance");
    complete(&mut setup.calls, &setup.tracker, taken, &json!({}), setup.now);
    let told = drain(&mut setup.calls, &setup.tracker).events;
    let transferor = told
        .iter()
        .find_map(|event| match event {
            CallEvent::Transferred { target, transferor, .. } if *target == Mri::new(C) => Some(transferor.clone()),
            _ => None,
        })
        .unwrap();
    assert!(setup.calls.current().is_none(), "the call is over here");
    // the call to the target, naming who transferred it, in their 1:1 chat
    let target = Placement::Transferred {
        target: Mri::new(C),
        transferor,
    };
    let call = setup.calls.place(&target, media(), setup.now).unwrap();
    let placed = drain(&mut setup.calls, &setup.tracker);
    let [start] = placed.sent.as_slice() else {
        panic!("one start")
    };
    assert_eq!(start.name, "start");
    let invitation = &start.body["callInvitation"];
    assert_eq!(invitation["transferor"]["details"]["id"], json!(B));
    assert_eq!(start.body["participants"]["to"][0]["id"], json!(C));
    assert_eq!(
        setup
            .calls
            .call
            .as_ref()
            .and_then(|current| current.caption_thread.clone()),
        teams_proto::one_to_one_id(A, C)
    );
    assert_eq!(setup.calls.current().map(|(id, _)| id), Some(call));
}

#[tokio::test]
async fn a_transfer_whose_acceptance_names_another_call_binds_nothing() {
    let mut setup = Connected::new().await;
    let elsewhere = of_another_call(&cc("transfer", "acceptance"));
    let asked = at_transfer(&setup, 21, &transfer_to_c(&elsewhere));
    assert_eq!(setup.tracker.observe_delivery(&asked), Observed::Nothing);
    setup.calls.on_delivery(&asked, Observed::Nothing, setup.now);
    let [("transfer_acceptance", url, body)] = &unchecked(&mut setup.calls)[..] else {
        panic!("one transfer acceptance")
    };
    assert_eq!(url.as_str(), elsewhere);
    assert!(!setup.tracker.call_allowed(url, Some(body)));
}

#[test]
fn a_delivery_s_debug_leaves_its_body_out() {
    let told = delivery(
        "/v4/f/x/callAgent/y/call/end/",
        &json!({"secret": "synthetic-sdp-ice-credentials"}),
    );
    let debug = format!("{told:?}");
    assert!(debug.contains("calling/1") && debug.contains("body_len"), "{debug}");
    assert!(!debug.contains("synthetic-sdp-ice-credentials"), "{debug}");
    let tracker = crate::Tracker::new(Mri::new(A));
    assert_eq!(tracker.account(), &Mri::new(A));
}
