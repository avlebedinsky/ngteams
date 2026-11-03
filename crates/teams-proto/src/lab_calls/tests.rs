//! Synthetic identities and service replies, not tenant captures.
use super::*;
use crate::endpoint::Method;
use crate::write::CallWire;
use serde_json::json;
use teams_lab_targets::{LabAccount, one_to_one_id};

const A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const X: &str = "8:orgid:00000000-0000-4000-8000-00000000000d";
const ROOT: &str = "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint/";
const CONV: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01.conv.skype.com/conv/Synthetic";
const START: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/epconv";

fn bindings(own: &str) -> LabCallBindings {
    let roster = Arc::new(
        LabRoster::new([A, B, C].map(|mri| LabAccount {
            mri: mri.into(),
            upn: format!("{}@lab.example.test", mri.chars().last().unwrap()),
        }))
        .unwrap(),
    );
    let calls = LabCallBindings::new(own.into(), roster);
    calls.connected("socket1", ROOT).unwrap();
    calls
}
fn callback(name: &str) -> String {
    format!("{ROOT}callAgent/synthetic/{name}/")
}
fn start(actor: &str, peer: &str) -> CallWire {
    CallWire::synthetic(
        Method::Post,
        START,
        Some(json!({
            "participants":{"from":{"id":actor},"to":[{"id":peer}]},
            "conversationRequest":{"roster":{"rosterUpdate":callback("rosterUpdate")},"links":{"conversationEnd":callback("conversationEnd")}},
            "callInvitation":{"links":{"acceptance":callback("acceptance"),"end":callback("end")}}
        })),
    )
}
fn begin(calls: &LabCallBindings) -> (u64, CallPlan) {
    let plan = calls.plan(&start(A, B)).unwrap();
    let key = calls.begin(&plan, Some(vec![A.into(), B.into()])).unwrap();
    (key, plan)
}
fn response(calls: &LabCallBindings, key: u64, plan: &CallPlan) {
    calls.answered(
        key,
        plan,
        200,
        &serde_json::to_vec(&json!({"conversationController":CONV,"links":{
            "leave":format!("{CONV}/leave"),"updateEndpointMetadata":format!("{CONV}/updateEndpointMetadata")
        }}))
        .unwrap(),
    );
}
fn packet(path: &str, id: i64, body: &Value) -> CallDeliveryData {
    CallDeliveryData {
        connection: "socket1".into(),
        id,
        path: url::Url::parse(&callback(path)).unwrap().path().into(),
        body: serde_json::to_vec(body).unwrap(),
        headers: vec![
            ("trouter-request", Some(format!("request{id}"))),
            ("X-Microsoft-Skype-Chain-ID", Some("chain".into())),
            ("X-Microsoft-Skype-Message-ID", Some("message".into())),
        ],
    }
}

#[test]
fn entry_requires_exact_actor_peer_and_owned_callbacks_without_a_fence() {
    let calls = bindings(A);
    assert!(calls.plan(&start(B, A)).is_err());
    assert!(calls.plan(&start(A, X)).is_err());
    let mut wire = start(A, B);
    let mut body = wire.body().unwrap().clone();
    body["conversationRequest"]["roster"]["rosterUpdate"] = json!("https://outside.invalid/callback");
    wire = CallWire::synthetic(Method::Post, START, Some(body));
    assert!(calls.plan(&wire).is_err());
    let (key, _) = begin(&calls);
    assert!(calls.state().calls[&key].proven);
}

#[test]
fn only_service_issued_links_of_this_call_work_and_cleanup_survives_unsafe_roster() {
    let calls = bindings(A);
    let (key, plan) = begin(&calls);
    response(&calls, key, &plan);
    let foreign = CallWire::synthetic(Method::Post, &format!("{CONV}Other/leave"), None);
    assert!(calls.plan(&foreign).is_err());
    let bad = packet("rosterUpdate", 2, &json!({"participants":{X:{"state":"active"}}}));
    assert!(matches!(
        calls.record_delivery(&bad).unwrap(),
        DeliveryPlan::Known(CallObservation::TargetChanged)
    ));
    let metadata = CallWire::synthetic(Method::Put, &format!("{CONV}/updateEndpointMetadata"), None);
    assert!(calls.plan(&metadata).is_err());
    calls.disconnected("socket1");
    let leave = CallWire::synthetic(Method::Post, &format!("{CONV}/leave"), Some(json!({"from":{"id":A}})));
    let cleanup = calls.plan(&leave).unwrap();
    assert!(cleanup.cleanup);
    assert_eq!(calls.begin(&cleanup, None).unwrap(), key);
}

#[test]
fn acceptance_is_bound_before_start_answer_and_response_is_one_shot_on_its_socket() {
    let calls = bindings(A);
    let (key, _) = begin(&calls);
    let accepted = packet(
        "acceptance",
        8,
        &json!({"callAcceptance":{"acceptedBy":{"id":B},"links":{
        "mediaRenegotiation":format!("{CONV}/renegotiate")}}}),
    );
    assert!(matches!(
        calls.record_delivery(&accepted).unwrap(),
        DeliveryPlan::Known(CallObservation::Allowed)
    ));
    let ack = json!({"id":8,"status":200,"headers":{"trouter-request":"request8","X-Microsoft-Skype-Chain-ID":"chain","X-Microsoft-Skype-Message-ID":"message"},
        "body":json!({"callAcceptanceAcknowledgement":{"links":{"mediaRenegotiation":callback("mediaRenegotiation")}}}).to_string()}).to_string();
    assert!(calls.response("socket2", 8, &ack).is_err());
    assert_eq!(calls.response("socket1", 8, &ack).unwrap(), key);
    assert!(calls.response("socket1", 8, &ack).is_err());
    assert!(calls.response_still_valid("socket1", key));
    calls
        .record_delivery(&packet("end", 9, &json!({"callEnd":{"code":200}})))
        .unwrap();
    assert!(!calls.response_still_valid("socket1", key));
}

#[test]
fn incoming_invitation_needs_fresh_members_and_cannot_change_its_caller_or_callee() {
    use base64::Engine as _;
    let calls = bindings(B);
    let invite = |caller, callee| {
        let body = json!({"callNotification":{"from":{"id":caller},"to":{"id":callee},"links":{"attach":format!("{CONV}/attach")}},
            "conversationInvitation":{"conversationController":CONV}});
        let mut delivery = packet(
            "unused",
            1,
            &json!({"evt":107,"gp":base64::engine::general_purpose::STANDARD.encode(body.to_string())}),
        );
        delivery.path = "/v4/f/SyntheticEndpoint/".into();
        delivery
    };
    assert!(calls.record_delivery(&invite(X, B)).is_err());
    assert!(calls.record_delivery(&invite(A, C)).is_err());
    let DeliveryPlan::Incoming { key, chat } = calls.record_delivery(&invite(A, B)).unwrap() else {
        panic!()
    };
    assert_eq!(chat, one_to_one_id(A, B).unwrap());
    assert!(calls.confirm_incoming(key, "socket1", &[A.into(), X.into()]).is_err());
    calls.confirm_incoming(key, "socket1", &[A.into(), B.into()]).unwrap();
    assert!(calls.record_delivery(&invite(C, B)).is_err());
    let receipt = json!({"id":1,"status":200,"headers":{"trouter-request":"request1"},"body":""}).to_string();
    assert!(calls.response("socket1", 1, &receipt).is_ok());
    calls.disconnected("socket1");
    assert!(calls.confirm_incoming(key, "socket1", &[A.into(), B.into()]).is_err());
}

#[test]
fn wrong_actor_followups_unbound_joins_and_arbitrary_service_links_are_refused() {
    let calls = bindings(A);
    let (key, plan) = begin(&calls);
    response(&calls, key, &plan);
    let leave = CallWire::synthetic(Method::Post, &format!("{CONV}/leave"), Some(json!({"from":{"id":X}})));
    assert!(calls.plan(&leave).is_err());
    let unknown_join = CallWire::synthetic(Method::Post, &format!("{CONV}Unknown"), start(A, B).body().cloned());
    assert!(calls.plan(&unknown_join).is_err());
    calls.answered(
        key,
        &plan,
        200,
        br#"{"links":{"mediaRenegotiation":"https://outside.invalid/renegotiate"}}"#,
    );
    assert!(calls.state().calls[&key].unsafe_target);
    let leave = CallWire::synthetic(Method::Post, &format!("{CONV}/leave"), None);
    assert!(calls.plan(&leave).unwrap().cleanup);
}

#[test]
fn captions_are_bound_to_own_leg_and_exact_service_and_never_accept_other_bots() {
    use crate::captions::{CAPTION_SERVICE_MRI, CaptionCommandLink, CaptionWrite};
    let calls = bindings(A);
    let mut wire = start(A, B);
    let mut body = wire.body().unwrap().clone();
    body["participants"]["from"]["endpointId"] = json!("00000000-0000-4000-8000-0000000000e1");
    body["participants"]["from"]["participantId"] = json!("00000000-0000-4000-8000-0000000000e2");
    wire = CallWire::synthetic(Method::Post, START, Some(body));
    let plan = calls.plan(&wire).unwrap();
    let key = calls.begin(&plan, Some(vec![A.into(), B.into()])).unwrap();
    calls.answered(key, &plan, 201, &serde_json::to_vec(&json!({"conversationController":CONV,"links":{"addParticipant":format!("{CONV}/addParticipant"),"leave":format!("{CONV}/leave")}})).unwrap());
    let actor = crate::CallParticipant {
        mri: teams_fence::Mri::new(A),
        display_name: None,
        endpoint_id: "00000000-0000-4000-8000-0000000000e1".into(),
        participant_id: Some("00000000-0000-4000-8000-0000000000e2".into()),
        language_id: None,
    };
    let context = crate::CallContext::new("00000000-0000-4000-8000-0000000000c1");
    let chat = one_to_one_id(A, B).unwrap();
    let invite = CaptionWrite::invite(
        &context,
        &crate::CallLink::parse(&format!("{CONV}/addParticipant")).unwrap(),
        &actor,
        &chat,
        (
            &callback("segment/conversation/addParticipantSuccess"),
            &callback("segment/conversation/addParticipantFailure"),
        ),
    )
    .unwrap();
    let invite_plan = calls.plan_caption(&invite).unwrap();
    calls.begin(&invite_plan, Some(vec![A.into(), B.into()])).unwrap();
    let command = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/aks-prod-zz-api.callrecorder.teams.cloud.microsoft:21606/v2/oncommand/00000000-0000-4000-8000-000000000001";
    let command_link = CaptionCommandLink::parse(command).unwrap();
    let write = CaptionWrite::start(&context, &command_link, &actor, &chat).unwrap();
    assert!(calls.plan_caption(&write).is_err());
    let service = json!({"participants":{CAPTION_SERVICE_MRI:{"state":"active","endpoints":{"synthetic":{"endpointMetadata":{"commandUrl":command,"spokenLanguage":"en-us","processingModes":{"closedCaptions":{"state":"Inactive"}}}}}}}});
    assert!(matches!(
        calls.record_delivery(&packet("rosterUpdate", 20, &service)).unwrap(),
        delivery::DeliveryPlan::Known(CallObservation::Allowed)
    ));
    assert!(calls.plan_caption(&write).is_ok());
    assert_eq!(calls.state().calls[&key].members, BTreeSet::from([A.into(), B.into()]));
    let mut wrong_actor = actor.clone();
    wrong_actor.participant_id = Some("00000000-0000-4000-8000-000000000099".into());
    let wrong = CaptionWrite::start(&context, &command_link, &wrong_actor, &chat).unwrap();
    assert!(calls.plan_caption(&wrong).is_err());
    // The service's AddSuccess array is an operation result, never an outsider roster.
    let success_path = url::Url::parse(&callback("segment/conversation/addParticipantSuccess"))
        .unwrap()
        .path()
        .to_owned();
    let mut success = packet(
        "rosterUpdate",
        21,
        &json!({"participants":[{"id":CAPTION_SERVICE_MRI}]}),
    );
    success.path = success_path;
    assert!(matches!(
        calls.record_delivery(&success).unwrap(),
        delivery::DeliveryPlan::Known(CallObservation::Allowed)
    ));
    let outsider = packet(
        "rosterUpdate",
        22,
        &json!({"participants":{"28:00000000-0000-4000-8000-000000000099":{"state":"active"}}}),
    );
    assert!(matches!(
        calls.record_delivery(&outsider).unwrap(),
        delivery::DeliveryPlan::Known(CallObservation::TargetChanged)
    ));
    assert!(calls.plan_caption(&write).is_err());
}
